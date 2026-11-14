use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sw_domain::Observation;
use sw_inference::{
    CudaProvider, Image, ModelId, ModelPack, Preprocessor, Runtime, TensorSize, decode,
};

const OBSERVATION_SPEC: &str = include_str!("../../../qualification/observation-parity-v1.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationTolerance {
    version: String,
    minimum_iou: f64,
    max_score_abs: f64,
    max_box_pixels: f64,
    max_box_fraction: f64,
    max_keypoint_pixels: f64,
    max_keypoint_fraction: f64,
    max_joint_confidence_abs: f64,
    max_histogram_l1: f64,
}

#[derive(Deserialize)]
struct Report {
    cases: BTreeMap<String, TensorFixture>,
    models: BTreeMap<ModelId, BTreeMap<String, ModelFixture>>,
}

#[derive(Deserialize)]
struct TensorFixture {
    tensor_sha256: String,
    tensor_hw: Option<[usize; 2]>,
    image: Option<ImageFixture>,
}

#[derive(Deserialize)]
struct ImageFixture {
    rgb_shape: [u32; 3],
    rgb_sha256: Option<String>,
}

#[derive(Deserialize)]
struct ModelFixture {
    reference_sha256: String,
    observations: Option<Vec<Observation>>,
}

pub fn verify(
    library: &Path,
    library_sha: &str,
    root: &Path,
    manifest_sha: &str,
    cuda: Option<CudaProvider>,
) -> Result<()> {
    verify_with_profile(library, library_sha, root, manifest_sha, cuda, None)
}

pub fn verify_with_profile(
    library: &Path,
    library_sha: &str,
    root: &Path,
    manifest_sha: &str,
    cuda: Option<CudaProvider>,
    profile_dir: Option<&Path>,
) -> Result<()> {
    let pack = ModelPack::open(root, manifest_sha)?;
    let report: Report = serde_json::from_slice(&pack.parity_report()?)?;
    let tolerance: ObservationTolerance = serde_json::from_str(OBSERVATION_SPEC)?;
    ensure!(
        (1..=64).contains(&report.cases.len()),
        "invalid tensor fixture count"
    );
    let provider = if cuda.is_some() { "cuda" } else { "cpu" };
    let runtime = Runtime::load(library, library_sha, 4, cuda)?;
    let fixtures = root.join("fixtures");
    let mut models = BTreeMap::new();
    let mut prepare = Preprocessor::new(pack.profile());
    let mut failed = Vec::new();
    let mut numeric_failures = Vec::new();
    let mut observation_failures = Vec::new();
    for id in ModelId::ALL {
        let profile_prefix = profile_dir.map(|dir| dir.join(id.name()));
        let mut model = pack.load_profiled(&runtime, id, profile_prefix.as_deref())?;
        let mut cases = BTreeMap::new();
        for (name, case) in &report.cases {
            ensure!(
                !name.is_empty()
                    && name.len() <= 40
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "invalid tensor fixture name"
            );
            let reference = report
                .models
                .get(&id)
                .and_then(|model| model.get(name))
                .context("missing model fixture")?;
            let [h, w] = case.tensor_hw.unwrap_or(TensorSize::SQUARE.dimensions());
            let shape = TensorSize::new(h, w)?;
            let anchors = shape.anchors();
            let expected = read_floats(
                &fixtures.join(format!("{}_{name}.f32", id.name())),
                &reference.reference_sha256,
                id.channels() * anchors,
            )?;
            let input = read_floats(
                &fixtures.join(format!("{name}.f32")),
                &case.tensor_sha256,
                shape.input_values(),
            )?;
            let mut metrics = model.run(shape, &input, |actual| {
                compare(id, anchors, &expected, actual)
            })??;
            let case_id = format!("{}/{name}", id.name());
            if metrics["qualified"] != true {
                numeric_failures.push(case_id.clone());
            }
            if let (Some(image), Some(observations)) = (&case.image, &reference.observations) {
                let [height, width, channels] = image.rgb_shape;
                ensure!(
                    channels == 3 && (1..=3840).contains(&width) && (1..=2160).contains(&height),
                    "invalid RGB fixture shape"
                );
                let sha = image
                    .rgb_sha256
                    .as_deref()
                    .context("missing RGB fixture digest")?;
                let rgb = read_bytes(
                    &fixtures.join(format!("{name}.rgb")),
                    sha,
                    (width * height * 3) as usize,
                )?;
                let view = Image::rgb(width, height, &rgb)?;
                let (prepared, geometry) = prepare.prepare(view);
                ensure!(
                    geometry.tensor_size() == shape,
                    "image tensor shape differs from reference"
                );
                let pixel_error = prepared
                    .iter()
                    .zip(&input)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);
                let postprocess = decode(id, &expected, view, geometry, 0.0)?;
                let full = model.run(shape, prepared, |output| {
                    decode(id, output, view, geometry, 0.0)
                })??;
                let same_pixels = prepared
                    .iter()
                    .zip(&input)
                    .all(|(a, b)| a.to_bits() == b.to_bits());
                let postprocess =
                    compare_observations(observations, &postprocess, width, height, &tolerance);
                let full = compare_observations(observations, &full, width, height, &tolerance);
                if !same_pixels || postprocess["qualified"] != true || full["qualified"] != true {
                    metrics["qualified"] = json!(false);
                    observation_failures.push(case_id.clone());
                }
                metrics["image"] = json!({
                    "tensor_max_abs": pixel_error,
                    "tensor_exact": same_pixels,
                    "postprocess": postprocess,
                    "full_pipeline": full,
                });
            }
            if metrics["qualified"] != true {
                failed.push(case_id);
            }
            cases.insert(name, metrics);
        }
        models.insert(id.name(), cases);
        if profile_prefix.is_some() {
            model.finish_profile()?;
        }
    }
    let active = |failures: &[String]| failures.iter().all(|id| id.starts_with("pose/"));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ort": runtime.build_info(), "provider": provider,
            "observation_tolerances": tolerance.version,
            "observation_spec_sha256": format!("{:x}", Sha256::digest(OBSERVATION_SPEC)),
            "artifact_validation": "pass",
            "full_pack_numeric": if numeric_failures.is_empty() { "pass" } else { "fail" },
            "active_profile_numeric": if active(&numeric_failures) { "pass" } else { "fail" },
            "active_profile_observations": if active(&observation_failures) { "pass" } else { "fail" },
            "active_profile_events": "not_run",
            "capacity": "not_run",
            "numeric_failures": numeric_failures, "observation_failures": observation_failures,
            "qualified": failed.is_empty(), "failed": failed, "models": models
        }))?
    );
    ensure!(
        failed.is_empty(),
        "{} parity cases outside the locked bounds",
        failed.len()
    );
    Ok(())
}

fn read_floats(path: &Path, expected_sha: &str, length: usize) -> Result<Vec<f32>> {
    let bytes = read_bytes(path, expected_sha, length * 4)?;
    let values: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    ensure!(
        values.iter().all(|value| value.is_finite()),
        "non-finite tensor fixture"
    );
    Ok(values)
}

fn read_bytes(path: &Path, expected_sha: &str, expected_bytes: usize) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    ensure!(file.metadata()?.is_file(), "invalid tensor fixture file");
    let mut bytes = Vec::with_capacity(expected_bytes);
    file.take(expected_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() == expected_bytes, "invalid tensor fixture size");
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(expected_sha),
        "tensor fixture SHA-256 mismatch"
    );
    Ok(bytes)
}

fn compare_observations(
    reference: &[Observation],
    actual: &[Observation],
    width: u32,
    height: u32,
    spec: &ObservationTolerance,
) -> serde_json::Value {
    let mut candidates = Vec::new();
    for (a, expected) in reference.iter().enumerate() {
        for (b, observed) in actual.iter().enumerate() {
            let iou = expected.region.iou(observed.region);
            if expected.label == observed.label && iou >= spec.minimum_iou {
                candidates.push((iou, a, b));
            }
        }
    }
    candidates.sort_by(|(iou_a, ref_a, obs_a), (iou_b, ref_b, obs_b)| {
        iou_b
            .total_cmp(iou_a)
            .then_with(|| ref_a.cmp(ref_b))
            .then_with(|| obs_a.cmp(obs_b))
    });
    let mut consumed_reference = vec![false; reference.len()];
    let mut consumed_actual = vec![false; actual.len()];
    let mut matched = 0;
    let mut score_max = 0.0f64;
    let mut box_max = 0.0f64;
    let mut box_fraction = 0.0f64;
    let mut keypoint_max = 0.0f64;
    let mut keypoint_fraction = 0.0f64;
    let mut joint_confidence_max = 0.0f64;
    let mut histogram_l1_max = 0.0f64;
    let mut flag_differences = 0;
    for (_, a, b) in candidates {
        if consumed_reference[a] || consumed_actual[b] {
            continue;
        }
        consumed_reference[a] = true;
        consumed_actual[b] = true;
        matched += 1;
        let expected = &reference[a];
        let actual = &actual[b];
        score_max = score_max.max((actual.score - expected.score).abs());
        let r = expected.region;
        let a = actual.region;
        let dx = (a.x1 - r.x1).abs().max((a.x2 - r.x2).abs());
        let dy = (a.y1 - r.y1).abs().max((a.y2 - r.y2).abs());
        box_max = box_max.max(dx).max(dy);
        box_fraction = box_fraction
            .max(dx / f64::from(width))
            .max(dy / f64::from(height));
        flag_differences += usize::from(expected.limited != actual.limited)
            + usize::from(expected.truncated != actual.truncated);
        flag_differences += usize::from(expected.keypoints.len() != actual.keypoints.len());
        flag_differences +=
            usize::from(expected.torso_histogram.len() != actual.torso_histogram.len());
        for (a, r) in actual.keypoints.iter().zip(&expected.keypoints) {
            let dx = (a.0 - r.0).abs();
            let dy = (a.1 - r.1).abs();
            keypoint_max = keypoint_max.max(dx).max(dy);
            keypoint_fraction = keypoint_fraction
                .max(dx / f64::from(width))
                .max(dy / f64::from(height));
            joint_confidence_max = joint_confidence_max.max((a.2 - r.2).abs());
        }
        histogram_l1_max = histogram_l1_max.max(
            actual
                .torso_histogram
                .iter()
                .zip(&expected.torso_histogram)
                .map(|(a, r)| (a - r).abs())
                .sum::<f64>(),
        );
    }
    let missing = reference.len() - matched;
    let extra = actual.len() - matched;
    let qualified = missing == 0
        && extra == 0
        && flag_differences == 0
        && score_max <= spec.max_score_abs
        && box_max <= spec.max_box_pixels
        && box_fraction <= spec.max_box_fraction
        && keypoint_max <= spec.max_keypoint_pixels
        && keypoint_fraction <= spec.max_keypoint_fraction
        && joint_confidence_max <= spec.max_joint_confidence_abs
        && histogram_l1_max <= spec.max_histogram_l1;
    json!({
        "reference": reference.len(), "actual": actual.len(), "missing": missing, "extra": extra,
        "score_max_abs": score_max, "box_max_abs_px": box_max, "keypoint_max_abs_px": keypoint_max,
        "box_max_abs_fraction": box_fraction, "keypoint_max_abs_fraction": keypoint_fraction,
        "joint_confidence_max_abs": joint_confidence_max, "histogram_max_l1": histogram_l1_max,
        "flag_differences": flag_differences, "qualified": qualified,
    })
}

fn compare(
    id: ModelId,
    anchors: usize,
    expected: &[f32],
    actual: &[f32],
) -> Result<serde_json::Value> {
    ensure!(expected.len() == actual.len(), "output length changed");
    let (max_error, mean_limit) = match id {
        ModelId::Pose => (0.016, 0.00015),
        ModelId::PoseBalanced => (0.0055, 0.000040),
        ModelId::Fire => (0.0032, 0.000045),
    };
    let mut max_abs: f64 = 0.0;
    let mut sum = 0.0;
    for (reference, observed) in expected.iter().zip(actual) {
        let delta = (f64::from(*reference) - f64::from(*observed)).abs();
        max_abs = max_abs.max(delta);
        sum += delta;
    }
    let mean_abs = sum / expected.len() as f64;
    let fire = id == ModelId::Fire;
    let class = |tensor: &[f32], anchor: usize| {
        let first = tensor[4 * anchors + anchor];
        let (label, confidence) = if fire && tensor[5 * anchors + anchor] > first {
            (1, tensor[5 * anchors + anchor])
        } else {
            (0, first)
        };
        (confidence > id.legacy_confidence()).then_some(label)
    };
    let mut above_threshold = 0;
    let mut changed_anchors = 0;
    for anchor in 0..anchors {
        let reference = class(expected, anchor);
        changed_anchors += usize::from(class(actual, anchor) != reference);
        above_threshold += usize::from(reference.is_some());
    }
    Ok(json!({
        "max_abs": max_abs, "mean_abs": mean_abs,
        "max_abs_limit": max_error, "mean_abs_limit": mean_limit,
        "above_threshold": above_threshold, "changed_anchors": changed_anchors,
        "qualified": max_abs <= max_error && mean_abs <= mean_limit && changed_anchors == 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_domain::{Box2d, ObservationLabel};

    fn smoke(x: f64) -> Observation {
        Observation {
            capture_ts: 0.0,
            region: Box2d {
                x1: x,
                y1: 20.0,
                x2: x + 20.0,
                y2: 40.0,
            },
            score: 0.201,
            label: ObservationLabel::Smoke,
            track_id: None,
            keypoints: Vec::new(),
            limited: false,
            truncated: false,
            association_uncertain: false,
            torso_histogram: Vec::new(),
        }
    }

    #[test]
    fn matches_reordered_detections_and_rejects_shift_and_score_error() {
        let spec: ObservationTolerance = serde_json::from_str(OBSERVATION_SPEC).unwrap();
        let compare = |expected: &[Observation], actual: &[Observation]| {
            compare_observations(expected, actual, 640, 360, &spec)
        };
        let reference = [smoke(100.0), smoke(130.0)];
        assert_eq!(
            compare(&reference, &[smoke(130.0), smoke(100.0)])["qualified"],
            true
        );
        let moved = compare(&reference, &[smoke(130.0), smoke(200.0)]);
        assert_eq!(
            (moved["missing"].as_u64(), moved["extra"].as_u64()),
            (Some(1), Some(1))
        );
        let excessive_displacement = compare(&reference, &[smoke(130.0), smoke(100.3)]);
        assert_eq!(excessive_displacement["missing"], 0);
        assert_eq!(excessive_displacement["qualified"], false);
        assert_eq!(
            compare(
                &reference,
                &[
                    smoke(130.0),
                    Observation {
                        score: 0.204,
                        ..smoke(100.0)
                    }
                ]
            )["qualified"],
            false
        );
    }
}

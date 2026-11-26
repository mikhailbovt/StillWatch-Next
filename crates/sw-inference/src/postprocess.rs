use sw_domain::{Box2d, Keypoint, Observation, ObservationLabel};

use crate::{Image, InferenceError, Letterbox, ModelId, appearance::torso_histogram};

const MAX_DETECTIONS: usize = 300;
const MAX_FIRE_PROPOSALS: usize = 8;
const FIRE_PROPOSAL_IOU: f64 = 0.45;
const FIRE_DIAGNOSTIC_TOP_K: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FireDisposition {
    Kept,
    LowScore,
    OtherClass,
    InvalidGeometry,
    Padding,
    Suppressed,
    OutputLimit,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct FireAnchor {
    anchor: usize,
    score: f32,
    tensor_box: [f32; 4],
    source_box: Option<Box2d>,
    disposition: FireDisposition,
}

#[derive(Default, serde::Serialize)]
struct FireAnchorCounts {
    kept: usize,
    low_score: usize,
    other_class: usize,
    invalid_geometry: usize,
    padding: usize,
    suppressed: usize,
    output_limit: usize,
}

impl FireAnchorCounts {
    fn record(&mut self, reason: FireDisposition) {
        let count = match reason {
            FireDisposition::Kept => &mut self.kept,
            FireDisposition::LowScore => &mut self.low_score,
            FireDisposition::OtherClass => &mut self.other_class,
            FireDisposition::InvalidGeometry => &mut self.invalid_geometry,
            FireDisposition::Padding => &mut self.padding,
            FireDisposition::Suppressed => &mut self.suppressed,
            FireDisposition::OutputLimit => &mut self.output_limit,
        };
        *count += 1;
    }
}

#[derive(Default, serde::Serialize)]
struct FireClassTrace {
    counts: FireAnchorCounts,
    top: Vec<FireAnchor>,
}

impl FireClassTrace {
    fn record(&mut self, anchor: FireAnchor) {
        self.counts.record(anchor.disposition);
        let at = self
            .top
            .partition_point(|other| other.score >= anchor.score);
        if at < FIRE_DIAGNOSTIC_TOP_K {
            if self.top.len() == FIRE_DIAGNOSTIC_TOP_K {
                self.top.pop();
            }
            self.top.insert(at, anchor);
        }
    }
}

#[derive(serde::Serialize)]
pub struct FireTrace {
    tensor_size: [usize; 2],
    resized_size: [usize; 2],
    padding_left_top: [usize; 2],
    anchor_count: usize,
    legacy_threshold: f32,
    legacy_nms_iou: f32,
    smoke: FireClassTrace,
    flame: FireClassTrace,
}

struct Detection {
    score: f32,
    label: ObservationLabel,
    anchor: usize,
    coordinates: [f32; 4],
}

impl Detection {
    fn score_order(&self, other: &Self) -> std::cmp::Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.anchor.cmp(&other.anchor))
    }

    fn observation(&self, geometry: Letterbox, capture_ts: f64) -> Observation {
        let [x1, y1, x2, y2] = self.coordinates;
        Observation {
            capture_ts,
            region: Box2d {
                x1: geometry.x(x1),
                y1: geometry.y(y1),
                x2: geometry.x(x2),
                y2: geometry.y(y2),
            },
            score: f64::from(self.score),
            label: self.label,
            track_id: None,
            keypoints: Vec::new(),
            limited: false,
            truncated: false,
            association_uncertain: false,
            torso_histogram: Vec::new(),
        }
    }
}

pub fn decode(
    id: ModelId,
    values: &[f32],
    image: Image<'_>,
    geometry: Letterbox,
    capture_ts: f64,
) -> Result<Vec<Observation>, InferenceError> {
    postprocess(id, values, image, geometry, capture_ts, None, None)
}

pub fn decode_fire_with_proposals(
    values: &[f32],
    image: Image<'_>,
    geometry: Letterbox,
    capture_ts: f64,
) -> Result<(Vec<Observation>, Vec<Observation>), InferenceError> {
    let mut weak = Vec::new();
    let detections = postprocess(
        ModelId::Fire,
        values,
        image,
        geometry,
        capture_ts,
        None,
        Some(&mut weak),
    )?;
    // The tensor size bounds the candidates. Limit the reply after removing
    // padding and redundant anchors so one region cannot crowd out another.
    weak.sort_unstable_by(Detection::score_order);
    let mut proposals = Vec::<Observation>::with_capacity(weak.len().min(MAX_FIRE_PROPOSALS));
    for candidate in weak {
        let proposal = candidate.observation(geometry, capture_ts);
        if proposal.region.area() < 4.0
            || detections.iter().chain(&proposals).any(|other| {
                other.label == proposal.label
                    && other.region.iou(proposal.region) > FIRE_PROPOSAL_IOU
            })
        {
            continue;
        }
        proposals.push(proposal);
        if proposals.len() == MAX_FIRE_PROPOSALS {
            break;
        }
    }
    Ok((detections, proposals))
}

pub fn trace_fire(
    values: &[f32],
    image: Image<'_>,
    geometry: Letterbox,
    capture_ts: f64,
) -> Result<(Vec<Observation>, FireTrace), InferenceError> {
    let mut decisions = vec![FireDisposition::LowScore; geometry.tensor_size().anchors()];
    let observations = postprocess(
        ModelId::Fire,
        values,
        image,
        geometry,
        capture_ts,
        Some(&mut decisions),
        None,
    )?;
    let anchors = decisions.len();
    let mut trace = FireTrace {
        tensor_size: geometry.tensor_size().dimensions(),
        resized_size: [geometry.resized_height, geometry.resized_width],
        padding_left_top: [geometry.left, geometry.top],
        anchor_count: anchors,
        legacy_threshold: ModelId::Fire.legacy_confidence(),
        legacy_nms_iou: 0.70,
        smoke: FireClassTrace::default(),
        flame: FireClassTrace::default(),
    };
    for (anchor, winner) in decisions.into_iter().enumerate() {
        let value = |column: usize| values[column * anchors + anchor];
        let tensor_box = corners(value(0), value(1), value(2), value(3));
        let [x1, y1, x2, y2] = tensor_box;
        let raw_box = Box2d {
            x1: geometry.x(x1),
            y1: geometry.y(y1),
            x2: geometry.x(x2),
            y2: geometry.y(y2),
        };
        let valid = tensor_box.iter().all(|v| v.is_finite()) && x1 < x2 && y1 < y2;
        let source_box = (valid && raw_box.area() > 0.0).then_some(raw_box);
        let smoke = value(4);
        let flame = value(5);
        for (score, won, class) in [
            (smoke, smoke >= flame, &mut trace.smoke),
            (flame, flame > smoke, &mut trace.flame),
        ] {
            let disposition = if !valid {
                FireDisposition::InvalidGeometry
            } else if source_box.is_none() {
                FireDisposition::Padding
            } else if score <= trace.legacy_threshold {
                FireDisposition::LowScore
            } else if won {
                winner
            } else {
                FireDisposition::OtherClass
            };
            class.record(FireAnchor {
                anchor,
                score,
                tensor_box,
                source_box,
                disposition,
            });
        }
    }
    Ok((observations, trace))
}

fn postprocess(
    id: ModelId,
    values: &[f32],
    image: Image<'_>,
    geometry: Letterbox,
    capture_ts: f64,
    mut decisions: Option<&mut [FireDisposition]>,
    mut weak: Option<&mut Vec<Detection>>,
) -> Result<Vec<Observation>, InferenceError> {
    let fire = id == ModelId::Fire;
    let anchors = geometry.tensor_size().anchors();
    if values.len() != id.channels() * anchors
        || !capture_ts.is_finite()
        || image.width != geometry.width
        || image.height != geometry.height
    {
        return Err(InferenceError::Tensor);
    }
    let value = |column, anchor| values[column * anchors + anchor];
    let mut candidates = Vec::new();
    for anchor in 0..anchors {
        let first = value(4, anchor);
        let (label, score) = if !fire {
            (ObservationLabel::Person, first)
        } else {
            let second = value(5, anchor);
            if second > first {
                (ObservationLabel::Fire, second)
            } else {
                (ObservationLabel::Smoke, first)
            }
        };
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(InferenceError::Tensor);
        }
        if score <= id.legacy_confidence() {
            if score >= 0.12
                && let Some(weak) = &mut weak
            {
                let coordinates = corners(
                    value(0, anchor),
                    value(1, anchor),
                    value(2, anchor),
                    value(3, anchor),
                );
                if coordinates.iter().all(|value| value.is_finite())
                    && coordinates[2] > coordinates[0]
                    && coordinates[3] > coordinates[1]
                {
                    weak.push(Detection {
                        score,
                        label,
                        anchor,
                        coordinates,
                    });
                }
            }
            continue;
        }
        let width = value(2, anchor);
        let height = value(3, anchor);
        let coordinates = corners(value(0, anchor), value(1, anchor), width, height);
        if coordinates.iter().any(|v| !v.is_finite()) || width * 0.5 < 0.0 || height * 0.5 < 0.0 {
            return Err(InferenceError::Tensor);
        }
        candidates.push(Detection {
            score,
            label,
            anchor,
            coordinates,
        });
        if let Some(entries) = &mut decisions {
            entries[anchor] = FireDisposition::OutputLimit;
        }
    }
    candidates.sort_unstable_by(Detection::score_order);
    let threshold = if fire { 0.70 } else { 0.50 };
    let mut kept = Vec::<Detection>::new();
    for candidate in candidates {
        if kept.iter().any(|other| {
            other.label == candidate.label
                && iou(other.coordinates, candidate.coordinates) > threshold
        }) {
            if let Some(entries) = &mut decisions {
                entries[candidate.anchor] = FireDisposition::Suppressed;
            }
            continue;
        }
        if let Some(entries) = &mut decisions {
            entries[candidate.anchor] = FireDisposition::Kept;
        }
        kept.push(candidate);
        if kept.len() == MAX_DETECTIONS {
            break;
        }
    }
    kept.into_iter()
        .map(|candidate| {
            let mut observation = candidate.observation(geometry, capture_ts);
            let region = observation.region;
            if !fire {
                observation.keypoints = (0..17)
                    .map(|joint| {
                        let column = 5 + joint * 3;
                        let x = geometry.x(value(column, candidate.anchor));
                        let y = geometry.y(value(column + 1, candidate.anchor));
                        let confidence = if x > 0.0
                            && x < image.width as f64
                            && y > 0.0
                            && y < image.height as f64
                        {
                            f64::from(value(column + 2, candidate.anchor))
                        } else {
                            0.0
                        };
                        Keypoint(x, y, confidence)
                    })
                    .collect();
                observation.limited = region.height() < 40.0;
                observation.truncated = region.x1 <= 3.0
                    || region.y1 <= 3.0
                    || region.x2 >= image.width as f64 - 3.0
                    || region.y2 >= image.height as f64 - 3.0;
                observation.torso_histogram = torso_histogram(image, &observation.keypoints);
            }
            if !observation.valid() {
                return Err(InferenceError::Tensor);
            }
            Ok(observation)
        })
        .collect()
}

fn corners(x: f32, y: f32, width: f32, height: f32) -> [f32; 4] {
    let (half_width, half_height) = (width * 0.5, height * 0.5);
    [
        x - half_width,
        y - half_height,
        x + half_width,
        y + half_height,
    ]
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let intersection =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let area = |p: [f32; 4]| (p[2] - p[0]) * (p[3] - p[1]);
    let union = area(a) + area(b) - intersection;
    if union > 0.0 {
        intersection / union
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use crate::{InputProfile, Preprocessor};

    use super::*;

    #[test]
    fn fire_nms_keeps_overlapping_different_classes_and_uses_strict_threshold() {
        let pixels = vec![0; 32 * 32 * 3];
        let image = Image::rgb(32, 32, &pixels).unwrap();
        const ANCHORS: usize = 8400;
        let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
        let mut tensor = vec![0.0; 6 * ANCHORS];
        for anchor in 0..4 {
            tensor[anchor] = 320.0;
            tensor[ANCHORS + anchor] = 320.0;
            tensor[2 * ANCHORS + anchor] = 160.0;
            tensor[3 * ANCHORS + anchor] = 160.0;
        }
        tensor[4 * ANCHORS] = 0.95;
        tensor[4 * ANCHORS + 1] = 0.75;
        tensor[5 * ANCHORS + 2] = 0.85;
        tensor[5 * ANCHORS + 3] = 0.20;
        let observations = decode(ModelId::Fire, &tensor, image, geometry, 0.0).unwrap();
        assert_eq!(
            observations.iter().map(|o| o.label).collect::<Vec<_>>(),
            vec![ObservationLabel::Smoke, ObservationLabel::Fire]
        );
        assert_eq!(observations[0].region.width(), 8.0);
    }

    #[test]
    fn low_score_regions_travel_separately_from_the_legacy_detector_result() {
        let pixels = vec![0; 64 * 64 * 3];
        let image = Image::rgb(64, 64, &pixels).unwrap();
        let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
        let anchors = geometry.tensor_size().anchors();
        let mut tensor = vec![0.0; 6 * anchors];
        for (index, row) in [
            [100.0, 130.0, 50.0, 50.0, 0.9, 0.0],
            [420.0, 380.0, 50.0, 50.0, 0.04, 0.19],
            [575.0, 420.0, 50.0, 50.0, 0.0, 0.08],
        ]
        .into_iter()
        .enumerate()
        {
            for (column, value) in row.into_iter().enumerate() {
                tensor[column * anchors + index] = value;
            }
        }
        let legacy = decode(ModelId::Fire, &tensor, image, geometry, 0.2).unwrap();
        let (detected, weak) = decode_fire_with_proposals(&tensor, image, geometry, 0.2).unwrap();
        assert_eq!(detected, legacy);
        assert_eq!(weak.len(), 1);
        assert_eq!(weak[0].label, ObservationLabel::Fire);
        assert_eq!(weak[0].capture_ts, 0.2);
    }

    #[test]
    fn diagnostic_retains_subthreshold_and_per_class_rejections_without_changing_output() {
        let pixels = vec![0; 32 * 16 * 3];
        let image = Image::rgb(32, 16, &pixels).unwrap();
        let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
        let anchors = geometry.tensor_size().anchors();
        let mut tensor = vec![0.0; 6 * anchors];
        for (n, values) in [
            [320.0, 300.0, 80.0, 80.0, 0.90, 0.40], // smoke wins both scores above threshold
            [320.0, 300.0, 80.0, 80.0, 0.80, 0.05], // suppressed by first box
            [320.0, 300.0, 80.0, 80.0, 0.20, 0.19], // strict score floor
            [320.0, 300.0, -80.0, 80.0, 0.19, 0.18],
            [320.0, 30.0, 20.0, 20.0, 0.19, 0.19], // entirely inside letterbox padding
            [320.0, 300.0, 80.0, 80.0, 0.18, 0.91], // same coordinates, different class
        ]
        .into_iter()
        .enumerate()
        {
            for (column, value) in values.into_iter().enumerate() {
                tensor[column * anchors + n] = value;
            }
        }
        let expected = decode(ModelId::Fire, &tensor, image, geometry, 1.0).unwrap();
        let (actual, trace) = trace_fire(&tensor, image, geometry, 1.0).unwrap();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let reasons = |items: &[FireAnchor], anchor| {
            items
                .iter()
                .find(|item| item.anchor == anchor)
                .unwrap()
                .disposition
        };
        assert_eq!(reasons(&trace.smoke.top, 0), FireDisposition::Kept);
        assert_eq!(reasons(&trace.flame.top, 0), FireDisposition::OtherClass);
        assert_eq!(reasons(&trace.smoke.top, 1), FireDisposition::Suppressed);
        assert_eq!(reasons(&trace.smoke.top, 2), FireDisposition::LowScore);
        assert_eq!(
            reasons(&trace.smoke.top, 3),
            FireDisposition::InvalidGeometry
        );
        assert_eq!(reasons(&trace.smoke.top, 4), FireDisposition::Padding);
        assert_eq!(reasons(&trace.flame.top, 5), FireDisposition::Kept);
        assert_eq!(trace.smoke.top.len(), FIRE_DIAGNOSTIC_TOP_K);
    }
}

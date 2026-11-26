use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use sw_engine::{ANALYSIS_POLICY_VERSION, WorkerBinary, execution_host};
use sw_protocol::{
    AnalysisWorkload, ExecutionHost, SemanticRoute,
    worker::{InferenceConfig, LiveModelProfile, rectangular_tensor_shape},
};
use sw_storage::AnalysisAdmission;

use crate::{Config, MAX_CUDA_INFERENCE_WORKERS, valid_digest};

const MAX_PROFILE_BYTES: u64 = 64 * 1024;
const MAX_REPORT_BYTES: u64 = 1_000_000;
const MAX_MODEL_MANIFEST_BYTES: u64 = 128 * 1024;
const NATIVE_CONTRACT_V2: &str = include_str!("../../../qualification/fast-rc-v2.json");
const NATIVE_CONTRACT_SHORT: &str = include_str!("../../../qualification/fast-rc-v3-short.json");

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub profile_file: std::path::PathBuf,
    pub report_file: std::path::PathBuf,
    pub profile_sha256: String,
    pub report_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionProfile {
    protocol: String,
    id: String,
    candidate_sha256: String,
    native_contract_sha256: String,
    host: ExecutionHost,
    artifacts: Artifacts,
    inference_workers: u8,
    inference_threads: usize,
    work: AnalysisWorkload,
    tensor_shapes: Vec<[u16; 4]>,
    semantic: Option<SemanticRoute>,
    raw_frame_bytes: u32,
    detail_frame_bytes: u32,
    evidence_quota_bytes: u64,
    database_quota_bytes: u64,
    limits: ResourceLimits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifacts {
    core: String,
    worker: String,
    mediamtx: String,
    encoder: String,
    model_manifest: String,
    pose_balanced: String,
    fire: String,
    ort: String,
    cuda: Option<CudaArtifacts>,
    decoder_identity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CudaArtifacts {
    device: u16,
    provider: String,
    shared: String,
    graph_shape: Option<[u16; 2]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceLimits {
    peak_rss_bytes: u64,
    peak_vram_mib: u64,
    peak_disk_bytes: u64,
    maximum_children: u32,
    maximum_open_sockets: u32,
}

#[derive(Deserialize)]
struct QualificationReport {
    protocol: String,
    scope: Option<String>,
    contract_digests: Option<ReportContracts>,
    verdict: String,
    execution_profile_digest: String,
    candidate_digest: String,
    native_wall_seconds: f64,
    active_model_results: ActiveModelResults,
    criteria: Vec<Criterion>,
}

#[derive(Deserialize)]
struct ReportContracts {
    native: String,
}

#[derive(Deserialize)]
struct ActiveModelResults {
    observation_spec_sha256: String,
    artifact_validation: String,
    active_profile_numeric: String,
    active_profile_observations: String,
    active_profile_events: String,
    full_pack_numeric: String,
}

#[derive(Deserialize)]
struct NativeContract {
    protocol: String,
    budget_seconds: f64,
    quiet_load: NativeQuietLoad,
    cases: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct NativeQuietLoad {
    minimum_seconds: f64,
}

#[derive(Deserialize)]
struct Criterion {
    id: String,
    result: String,
    evidence: Vec<serde_json::Value>,
}

pub(super) async fn verify(
    selection: &Selection,
    config: &Config,
    worker: &WorkerBinary,
    inference: &InferenceConfig,
) -> Result<(Arc<AnalysisAdmission>, LiveModelProfile)> {
    ensure!(
        selection.profile_file.is_absolute() && selection.report_file.is_absolute(),
        "qualification artifacts must use absolute paths"
    );
    let profile: ExecutionProfile = read_pinned(
        &selection.profile_file,
        &selection.profile_sha256,
        MAX_PROFILE_BYTES,
    )
    .context("read pinned execution profile")?;
    let report: QualificationReport = read_pinned(
        &selection.report_file,
        &selection.report_sha256,
        MAX_REPORT_BYTES,
    )
    .context("read pinned qualification report")?;
    profile.check_report(&selection.profile_sha256, &report)?;

    let host = execution_host(inference.cuda.as_ref().map(|cuda| cuda.device)).await?;
    ensure!(
        host == profile.host,
        "hardware, driver or Core runtime thread count differs from the qualification"
    );
    ensure!(
        worker.decoder_identity().await? == Some(profile.artifacts.decoder_identity.as_str()),
        "decoder differs from the qualification"
    );
    ensure!(
        profile.inference_workers == config.inference_workers()
            && profile.inference_threads == inference.threads
            && profile.work.cameras == config.active_cameras
            && profile.raw_frame_bytes == config.raw_frame_bytes
            && profile.detail_frame_bytes
                == config
                    .media
                    .media_detail_frame_bytes
                    .unwrap_or(128 * 1024 * 1024)
            && profile.evidence_quota_bytes == config.evidence_quota_bytes
            && profile.database_quota_bytes
                == config
                    .database_quota_bytes
                    .unwrap_or(sw_storage::DEFAULT_DATABASE_QUOTA_BYTES)
            && profile.semantic == config.semantic_worker.map(|_| config.semantic_route.into()),
        "Core settings differ from the qualification"
    );
    ensure!(
        profile.work.media.viewer_sessions
            >= profile.work.media.preview_publishers + profile.work.media.detail_publishers,
        "qualified publisher allowances require viewer slots"
    );
    ensure!(
        inference
            .manifest_sha256
            .eq_ignore_ascii_case(&profile.artifacts.model_manifest)
            && inference
                .library_sha256
                .eq_ignore_ascii_case(&profile.artifacts.ort),
        "model or runtime differs from the qualification"
    );
    match (&profile.artifacts.cuda, &inference.cuda) {
        (None, None) => {}
        (Some(approved), Some(cuda)) => ensure!(
            approved.device == cuda.device
                && approved
                    .provider
                    .eq_ignore_ascii_case(&cuda.provider_sha256)
                && approved.shared.eq_ignore_ascii_case(&cuda.shared_sha256)
                && approved.graph_shape == cuda.graph_shape,
            "CUDA configuration differs from the qualification"
        ),
        _ => anyhow::bail!("execution backend differs from the qualification"),
    }

    let require_active_policy = report.protocol == "fast-rc-v3-short";
    let config = config.clone();
    let worker_path = worker.path().to_owned();
    let profile = tokio::task::spawn_blocking(move || -> Result<_> {
        profile.check_artifacts(&config, &worker_path, require_active_policy)?;
        Ok(profile)
    })
    .await??;

    let live_models = LiveModelProfile {
        pose_balanced_sha256: profile.artifacts.pose_balanced,
        fire_sha256: profile.artifacts.fire,
        warm_shapes: profile
            .tensor_shapes
            .into_iter()
            .map(|[_, _, h, w]| [h, w])
            .collect(),
    };
    Ok((
        Arc::new(AnalysisAdmission {
            id: profile.id,
            sha256: selection.profile_sha256.to_ascii_lowercase(),
            workload: profile.work,
        }),
        live_models,
    ))
}

impl ExecutionProfile {
    fn check_report(&self, digest: &str, report: &QualificationReport) -> Result<()> {
        let (contract, short) = match report.protocol.as_str() {
            "fast-rc-v2" => (NATIVE_CONTRACT_V2, false),
            "fast-rc-v3-short" => (NATIVE_CONTRACT_SHORT, true),
            _ => anyhow::bail!("unsupported qualification report protocol"),
        };
        let native: NativeContract = serde_json::from_str(contract)?;
        let contract_digest = format!("{:x}", Sha256::digest(contract));
        let required = native.cases.values().flatten().collect::<Vec<_>>();
        ensure!(
            self.protocol == "stillwatch-execution-v1"
                && self.id.len() <= 64
                && self
                    .id
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
                && self
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && valid_digest(&self.candidate_sha256)
                && self.native_contract_sha256 == contract_digest
                && self.work.valid()
                && sw_storage::CameraLimits::required_raw_bytes(self.work.source.dimensions)
                    .is_some_and(|bytes| u64::from(bytes) * u64::from(self.work.cameras)
                        <= u64::from(self.raw_frame_bytes))
                && self.work.detail_source.as_ref().is_none_or(|detail| {
                    sw_storage::CameraLimits::required_raw_bytes(detail.dimensions).is_some_and(
                        |bytes| {
                            u64::from(bytes) * u64::from(self.work.media.detail_decoders)
                                <= u64::from(self.detail_frame_bytes)
                        },
                    )
                }),
            "execution profile has an invalid identity, contract or workload"
        );
        ensure!(
            self.limits.peak_rss_bytes > 0
                && self.limits.peak_rss_bytes <= self.host.ram_bytes
                && self.limits.peak_vram_mib
                    <= self
                        .host
                        .accelerator
                        .as_ref()
                        .map_or(0, |gpu| gpu.memory_mib)
                && self.limits.peak_disk_bytes > 0
                && self.limits.maximum_children > 0
                && self.limits.maximum_open_sockets > 0,
            "execution profile has invalid resource high-waters"
        );
        ensure!(
            report.protocol == native.protocol
                && report.verdict == "PASS"
                && report.execution_profile_digest
                    == format!("sha256:{}", digest.to_ascii_lowercase())
                && report.candidate_digest
                    == format!("sha256:{}", self.candidate_sha256.to_ascii_lowercase())
                && (native.quiet_load.minimum_seconds..=native.budget_seconds)
                    .contains(&report.native_wall_seconds)
                && (required.len()..=128).contains(&report.criteria.len()),
            "report does not qualify this execution profile"
        );
        if short {
            ensure!(
                report.scope.as_deref() == Some("finite-native-engineering")
                    && report.contract_digests.as_ref().is_some_and(
                        |digests| digests.native == format!("sha256:{contract_digest}")
                    )
                    && report.criteria.len() == required.len(),
                "short report does not match its native contract or required case inventory"
            );
        }
        let models = &report.active_model_results;
        let tolerance = include_bytes!("../../../qualification/observation-parity-v1.json");
        ensure!(
            format!("{:x}", Sha256::digest(tolerance)) == models.observation_spec_sha256
                && models.artifact_validation == "pass"
                && models.active_profile_numeric == "pass"
                && models.active_profile_observations == "pass"
                && models.active_profile_events == "pass"
                && (models.full_pack_numeric == "pass"
                    || self.artifacts.cuda.is_some() && models.full_pack_numeric == "fail"),
            "report lacks numerical, observation or event evidence for the live roles"
        );
        let mut passed = BTreeSet::new();
        for criterion in &report.criteria {
            ensure!(
                criterion.result == "PASS"
                    && !criterion.evidence.is_empty()
                    && passed.insert(criterion.id.as_str()),
                "report contains invalid acceptance criteria"
            );
        }
        ensure!(
            required.iter().all(|id| passed.contains(id.as_str())),
            "report lacks required native qualification evidence"
        );
        Ok(())
    }

    fn check_artifacts(
        &self,
        config: &Config,
        worker: &Path,
        require_active_policy: bool,
    ) -> Result<()> {
        let artifacts = &self.artifacts;
        check_file(
            &std::env::current_exe()?,
            &artifacts.core,
            256 * 1024 * 1024,
        )?;
        check_file(worker, &artifacts.worker, 256 * 1024 * 1024)?;
        ensure!(
            config
                .media
                .mediamtx_sha256
                .as_ref()
                .is_some_and(|sha| sha.eq_ignore_ascii_case(&artifacts.mediamtx))
                && config
                    .media
                    .media_encoder_sha256
                    .as_ref()
                    .is_some_and(|sha| sha.eq_ignore_ascii_case(&artifacts.encoder))
                && config.media.mediamtx.is_some()
                && config.media.media_encoder.is_some(),
            "media artifacts differ from the qualification"
        );
        // MediaService and the isolated worker independently verify the pinned runtime files.
        let models = config
            .model_pack
            .as_ref()
            .context("qualified model pack required")?;
        let ort = config
            .ort_library
            .as_ref()
            .context("qualified ONNX Runtime required")?;
        check_file(ort, &artifacts.ort, 256 * 1024 * 1024)?;
        if let Some(cuda) = &artifacts.cuda {
            let ort = ort.canonicalize()?;
            let root = ort
                .parent()
                .context("ONNX Runtime library directory required")?;
            let (provider, shared) = if cfg!(windows) {
                (
                    "onnxruntime_providers_cuda.dll",
                    "onnxruntime_providers_shared.dll",
                )
            } else {
                (
                    "libonnxruntime_providers_cuda.so",
                    "libonnxruntime_providers_shared.so",
                )
            };
            check_file(&root.join(provider), &cuda.provider, 1024 * 1024 * 1024)?;
            check_file(&root.join(shared), &cuda.shared, 16 * 1024 * 1024)?;
        }
        let manifest: ModelManifest = read_pinned(
            &models.join("models.lock.json"),
            &artifacts.model_manifest,
            MAX_MODEL_MANIFEST_BYTES,
        )?;
        if require_active_policy {
            ensure!(
                manifest.contract.as_ref().is_some_and(|contract| {
                    contract.compatible_policy.as_deref() == Some(ANALYSIS_POLICY_VERSION)
                }),
                "model pack policy differs from the Core policy"
            );
        }
        ensure!(
            manifest.models.pose_balanced.sha256 == artifacts.pose_balanced
                && manifest.models.fire.sha256 == artifacts.fire,
            "model role hashes differ from the qualification"
        );
        check_file(
            &models.join("pose_balanced.onnx"),
            &artifacts.pose_balanced,
            64 * 1024 * 1024,
        )?;
        check_file(&models.join("fire.onnx"), &artifacts.fire, 64 * 1024 * 1024)?;
        let shape = match manifest.profile.as_str() {
            "cpu_fp32_square_640" => [1, 3, 640, 640],
            "cpu_fp32_rect_640" => {
                let dims = self.work.source.dimensions;
                let [h, w] = rectangular_tensor_shape(dims.width, dims.height);
                [1, 3, h, w]
            }
            _ => anyhow::bail!("unsupported model input profile"),
        };
        if let Some([h, w]) = artifacts.cuda.as_ref().and_then(|cuda| cuda.graph_shape) {
            ensure!(
                self.inference_workers <= MAX_CUDA_INFERENCE_WORKERS
                    && manifest.profile == "cpu_fp32_rect_640"
                    && manifest.contract.is_some()
                    && self.tensor_shapes == [[1, 3, h, w]]
                    && self.tensor_shapes == [shape],
                "CUDA graph capture must use the workload's one pinned tensor shape"
            );
        }
        if let Some(contract) = manifest.contract {
            ensure!(
                (1..=16).contains(&self.tensor_shapes.len())
                    && self.tensor_shapes.contains(&shape)
                    && self
                        .tensor_shapes
                        .iter()
                        .enumerate()
                        .all(|(index, &entry)| {
                            let [batch, channels, height, width] = entry;
                            batch == 1
                                && channels == 3
                                && contract.tensor_shapes.contains(&[height, width])
                                && !self.tensor_shapes[..index].contains(&entry)
                        }),
                "tensor inventory falls outside the model contract"
            );
        } else {
            ensure!(
                self.tensor_shapes == [shape],
                "tensor input differs from the qualification"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct ModelManifest {
    profile: String,
    models: ModelRoles,
    contract: Option<ModelShapeContract>,
}

#[derive(Deserialize)]
struct ModelShapeContract {
    compatible_policy: Option<String>,
    tensor_shapes: Vec<[u16; 2]>,
}

#[derive(Deserialize)]
struct ModelRoles {
    pose_balanced: ModelArtifact,
    fire: ModelArtifact,
}

#[derive(Deserialize)]
struct ModelArtifact {
    sha256: String,
}

pub(super) fn rectangular_model_shapes(config: &InferenceConfig) -> Option<Vec<[u16; 2]>> {
    if let Some(shape) = config.cuda.as_ref().and_then(|cuda| cuda.graph_shape) {
        return Some(vec![shape]);
    }
    let manifest: ModelManifest = read_pinned(
        &config.models.join("models.lock.json"),
        &config.manifest_sha256,
        MAX_MODEL_MANIFEST_BYTES,
    )
    .ok()?;
    if manifest.profile != "cpu_fp32_rect_640" {
        return None;
    }
    match &config.live_profile {
        Some(profile) => Some(profile.warm_shapes.clone()),
        None => manifest.contract.map(|contract| contract.tensor_shapes),
    }
}

pub(super) fn pose_provenance(config: &InferenceConfig) -> Result<(String, String)> {
    let manifest: ModelManifest = read_pinned(
        &config.models.join("models.lock.json"),
        &config.manifest_sha256,
        MAX_MODEL_MANIFEST_BYTES,
    )?;
    Ok((manifest.profile, manifest.models.pose_balanced.sha256))
}

fn read_pinned<T: DeserializeOwned>(path: &Path, digest: &str, max: u64) -> Result<T> {
    let file = limited_file(path, max)?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= max
            && valid_digest(digest)
            && format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(digest),
        "artifact does not match its pinned digest"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn check_file(path: &Path, digest: &str, max: u64) -> Result<()> {
    let file = limited_file(path, max)?;
    let mut hash = Sha256::new();
    let len = io::copy(&mut file.take(max + 1), &mut hash)?;
    ensure!(
        len <= max
            && valid_digest(digest)
            && format!("{:x}", hash.finalize()).eq_ignore_ascii_case(digest),
        "runtime binary differs from the qualification"
    );
    Ok(())
}

fn limited_file(path: &Path, max: u64) -> Result<File> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && (1..=max).contains(&metadata.len()),
        "invalid artifact size"
    );
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sw_protocol::MediaWorkloadLimits;

    fn profile(contract: &str, candidate: &str) -> ExecutionProfile {
        serde_json::from_value(json!({
            "protocol": "stillwatch-execution-v1",
            "id": "verified-local",
            "candidate_sha256": candidate,
            "native_contract_sha256": format!("{:x}", Sha256::digest(contract)),
            "host": {
                "platform": "windows", "architecture": "x86_64", "os_release": "test",
                "cpu": "test", "logical_cores": 4, "core_runtime_threads": 4, "ram_bytes": 2_000_000_000,
                "accelerator": null
            },
            "artifacts": {
                "core": candidate, "worker": candidate, "mediamtx": candidate, "encoder": candidate,
                "model_manifest": candidate, "pose_balanced": candidate, "fire": candidate, "ort": candidate,
                "cuda": null, "decoder_identity": "test"
            },
            "inference_workers": 1, "inference_threads": 4,
            "work": {
                "source": {"kind": "rtsp_tcp", "codec": "h264", "dimensions": {"width": 640, "height": 480},
                           "min_millifps": 15000, "max_millifps": 15000},
                "sampling": {"pose_fps": 5, "fire_fps": 2, "evidence_fps": 2},
                "cameras": 4, "min_pose_per_wall_minute": 285, "min_fire_per_wall_minute": 114,
                "detail_source": null,
                "media": MediaWorkloadLimits {
                    detail_decoders: 0,
                    snapshot_requests_per_minute: 6,
                    ..MediaWorkloadLimits::DEFAULT_LIMITS
                }
            },
            "tensor_shapes": [[1, 3, 480, 640]], "semantic": null,
            "raw_frame_bytes": 32_000_000, "detail_frame_bytes": 0,
            "evidence_quota_bytes": 32_000_000, "database_quota_bytes": 32_000_000,
            "limits": {"peak_rss_bytes": 900_000_000, "peak_vram_mib": 0, "peak_disk_bytes": 320_000_000,
                       "maximum_children": 16, "maximum_open_sockets": 64}
        }))
        .unwrap()
    }

    fn report(
        protocol: &str,
        contract: &str,
        candidate: &str,
        profile: &str,
        duration: u32,
    ) -> Value {
        let native: NativeContract = serde_json::from_str(contract).unwrap();
        let criteria: Vec<_> = native
            .cases
            .values()
            .flatten()
            .map(|id| {
                json!({"id": id, "result": "PASS", "evidence": [{"path": "case.json", "digest": format!("sha256:{candidate}")}]})
            })
            .collect();
        json!({
            "protocol": protocol, "verdict": "PASS", "candidate_digest": format!("sha256:{candidate}"),
            "execution_profile_digest": format!("sha256:{profile}"), "native_wall_seconds": duration,
            "active_model_results": {
                "observation_spec_sha256": format!("{:x}", Sha256::digest(include_bytes!("../../../qualification/observation-parity-v1.json"))),
                "artifact_validation": "pass", "active_profile_numeric": "pass",
                "active_profile_observations": "pass", "active_profile_events": "pass", "full_pack_numeric": "pass"
            },
            "criteria": criteria
        })
    }

    #[test]
    fn activation_binds_the_version_duration_and_complete_proofs() {
        let candidate = "a".repeat(64);
        let profile_digest = "b".repeat(64);
        let full = profile(NATIVE_CONTRACT_V2, &candidate);
        let short = profile(NATIVE_CONTRACT_SHORT, &candidate);
        let v2 = report(
            "fast-rc-v2",
            NATIVE_CONTRACT_V2,
            &candidate,
            &profile_digest,
            3600,
        );
        let mut v3 = report(
            "fast-rc-v3-short",
            NATIVE_CONTRACT_SHORT,
            &candidate,
            &profile_digest,
            720,
        );
        v3["scope"] = json!("finite-native-engineering");
        v3["contract_digests"] =
            json!({"native": format!("sha256:{:x}", Sha256::digest(NATIVE_CONTRACT_SHORT))});
        let check = |frozen: &ExecutionProfile, value: Value| {
            frozen.check_report(&profile_digest, &serde_json::from_value(value).unwrap())
        };
        check(&full, v2.clone()).unwrap();
        check(&short, v3.clone()).unwrap();
        assert!(check(&short, v2).is_err());
        assert!(check(&full, v3.clone()).is_err());
        for (field, value) in [
            ("protocol", json!("stillwatch-short-resource-component-v1")),
            ("native_wall_seconds", json!(719.9)),
            ("scope", Value::Null),
            (
                "contract_digests",
                json!({"native": format!("sha256:{:x}", Sha256::digest(NATIVE_CONTRACT_V2))}),
            ),
        ] {
            let mut invalid = v3.clone();
            invalid[field] = value;
            assert!(check(&short, invalid).is_err(), "{field}");
        }
        let mut duplicate = v3.clone();
        duplicate["criteria"][1] = duplicate["criteria"][0].clone();
        assert!(check(&short, duplicate).is_err());
        v3["criteria"].as_array_mut().unwrap().pop();
        assert!(check(&short, v3).is_err());
    }
}

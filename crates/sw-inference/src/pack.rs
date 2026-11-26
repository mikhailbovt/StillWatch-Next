use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{InferenceError, Model, RUNTIME_VERSION, Runtime, SessionMode};

const BASELINE: &str = "b0d20a20d38a5bbc22178d150999e57cc9e7bfb3";
pub(crate) const SIDE: usize = 640;
#[cfg(feature = "diagnostics")]
pub(crate) const DIAGNOSTIC_FIRE_SIDE: usize = 960;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub enum InputProfile {
    #[serde(rename = "cpu_fp32_square_640")]
    Square,
    #[serde(rename = "cpu_fp32_rect_640")]
    Rectangular,
}

impl InputProfile {
    fn input_shape(self) -> [i64; 4] {
        match self {
            Self::Square => [1, 3, 640, 640],
            Self::Rectangular => [1, 3, -1, -1],
        }
    }

    fn output_shape(self, id: ModelId) -> [i64; 3] {
        [
            1,
            id.channels() as i64,
            if self == Self::Square { 8400 } else { -1 },
        ]
    }

    pub fn accepts(self, shape: TensorSize) -> bool {
        match self {
            Self::Square => shape == TensorSize::SQUARE,
            Self::Rectangular => shape.height <= SIDE && shape.width <= SIDE,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TensorSize {
    height: usize,
    width: usize,
}

impl TensorSize {
    pub const SQUARE: Self = Self {
        height: SIDE,
        width: SIDE,
    };

    pub fn new(height: usize, width: usize) -> Result<Self, InferenceError> {
        Self::bounded(height, width, SIDE)
    }

    pub(crate) fn bounded(
        height: usize,
        width: usize,
        side: usize,
    ) -> Result<Self, InferenceError> {
        if [height, width]
            .into_iter()
            .all(|v| (32..=side).contains(&v) && v.is_multiple_of(32))
        {
            Ok(Self { height, width })
        } else {
            Err(InferenceError::Tensor)
        }
    }

    pub fn dimensions(self) -> [usize; 2] {
        [self.height, self.width]
    }

    pub fn input_values(self) -> usize {
        3 * self.height * self.width
    }

    pub fn anchors(self) -> usize {
        [8, 16, 32]
            .into_iter()
            .map(|stride| (self.height / stride) * (self.width / stride))
            .sum()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ModelId {
    Pose,
    PoseBalanced,
    Fire,
}

impl ModelId {
    pub const ALL: [Self; 3] = [Self::Pose, Self::PoseBalanced, Self::Fire];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Pose => "pose",
            Self::PoseBalanced => "pose_balanced",
            Self::Fire => "fire",
        }
    }

    pub const fn channels(self) -> usize {
        if matches!(self, Self::Fire) { 6 } else { 56 }
    }

    pub const fn legacy_confidence(self) -> f32 {
        match self {
            Self::Fire => 0.20,
            _ => 0.15,
        }
    }

    const fn layout(self) -> &'static str {
        match self {
            Self::Fire => "fire_xywh_class_scores",
            _ => "pose_xywh_keypoints",
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    baseline: String,
    profile: InputProfile,
    execution_provider: String,
    onnxruntime: String,
    parity_report_sha256: String,
    models: BTreeMap<ModelId, Entry>,
    contract: Option<ModelContract>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelContract {
    source_revision: String,
    observation_spec_sha256: String,
    compatible_policy: String,
    tensor_shapes: Vec<[usize; 2]>,
    roles: LiveRoles,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRoles {
    person: ModelId,
    hazard: ModelId,
}

#[derive(Deserialize)]
struct Entry {
    artifact: String,
    sha256: String,
    byte_length: u64,
    opset: u32,
    external_data: Vec<serde_json::Value>,
    source: Source,
    input: Input,
    output: Output,
}

#[derive(Deserialize)]
struct Source {
    classes: BTreeMap<String, String>,
    sha256: Option<String>,
    tensor_sha256: Option<String>,
}

#[derive(Deserialize)]
struct Input {
    name: String,
    dtype: String,
    shape: [i64; 4],
    layout: String,
    colour: String,
    divisor: u32,
    resize: String,
    padding: u8,
    centered_letterbox: bool,
}

#[derive(Deserialize)]
struct Output {
    name: String,
    dtype: String,
    shape: [i64; 3],
    layout: String,
    nms: String,
    keypoint_map: Option<String>,
    legacy_confidence: f32,
    legacy_iou: f32,
}

impl Entry {
    fn validate(&self, id: ModelId, profile: InputProfile) -> bool {
        let fire = matches!(id, ModelId::Fire);
        let classes: &[(&str, &str)] = if fire {
            &[("0", "smoke"), ("1", "fire")]
        } else {
            &[("0", "person")]
        };
        self.artifact == format!("{}.onnx", id.name())
            && (1..=64 * 1024 * 1024).contains(&self.byte_length)
            && self.opset == crate::onnx::OPSET
            && self.external_data.is_empty()
            && self
                .source
                .classes
                .iter()
                .map(|(index, name)| (index.as_str(), name.as_str()))
                .eq(classes.iter().copied())
            && self.input.name == "images"
            && self.input.dtype == "float32"
            && self.input.shape == profile.input_shape()
            && self.input.layout == "nchw"
            && self.input.colour == "rgb"
            && self.input.divisor == 255
            && self.input.resize == "opencv_inter_linear"
            && self.input.padding == 114
            && self.input.centered_letterbox
            && self.output.name == "output0"
            && self.output.dtype == "float32"
            && self.output.shape == profile.output_shape(id)
            && self.output.layout == id.layout()
            && self.output.nms == "class_aware"
            && self.output.keypoint_map.as_deref() == (!fire).then_some("coco17_xy_confidence")
            && self.output.legacy_confidence == id.legacy_confidence()
            && self.output.legacy_iou == if fire { 0.70 } else { 0.50 }
    }
}

pub struct ModelPack {
    root: PathBuf,
    manifest: Manifest,
    graphs: BTreeMap<ModelId, Vec<u8>>,
}

impl ModelPack {
    pub fn artifact_digest(&self, id: ModelId) -> Option<&str> {
        self.manifest
            .models
            .get(&id)
            .map(|entry| entry.sha256.as_str())
    }

    pub fn open(root: &Path, trusted_manifest_sha256: &str) -> Result<Self, InferenceError> {
        let root = root.canonicalize()?;
        let bytes = read_verified(
            &root.join("models.lock.json"),
            128 * 1024,
            trusted_manifest_sha256,
        )?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| InferenceError::Manifest)?;
        if !manifest.contract_valid()
            || manifest.execution_provider != "cpu"
            || manifest.onnxruntime != RUNTIME_VERSION
            || ModelId::ALL.iter().any(|id| {
                !manifest
                    .models
                    .get(id)
                    .is_some_and(|entry| entry.validate(*id, manifest.profile))
            })
        {
            return Err(InferenceError::Manifest);
        }
        let mut graphs = BTreeMap::new();
        for (&id, entry) in &manifest.models {
            let path = root.join(&entry.artifact).canonicalize()?;
            if path.parent() != Some(root.as_path()) {
                return Err(InferenceError::Manifest);
            }
            let bytes = read_verified(&path, entry.byte_length, &entry.sha256)?;
            if bytes.len() as u64 != entry.byte_length {
                return Err(InferenceError::ArtifactSize);
            }
            crate::onnx::validate(&bytes, &entry.input.shape, &entry.output.shape)?;
            graphs.insert(id, bytes);
        }
        Ok(Self {
            root,
            manifest,
            graphs,
        })
    }

    pub fn supports_shape(&self, shape: TensorSize) -> bool {
        self.profile().accepts(shape)
            && self
                .manifest
                .contract
                .as_ref()
                .is_none_or(|contract| contract.tensor_shapes.contains(&shape.dimensions()))
    }

    pub fn declared_shapes(&self) -> Option<&[[usize; 2]]> {
        self.manifest
            .contract
            .as_ref()
            .map(|contract| contract.tensor_shapes.as_slice())
    }

    pub fn profile(&self) -> InputProfile {
        self.manifest.profile
    }

    pub fn load(&self, runtime: &Runtime, id: ModelId) -> Result<Model, InferenceError> {
        self.load_profiled(runtime, id, None)
    }

    /// Construct the two sessions used by an inference worker. The pack has
    /// verified every graph. Each serialized graph can be released as soon as
    /// its session owns the model, including while the next session starts.
    pub fn into_live_models(
        mut self,
        runtime: &Runtime,
        capture_shape: Option<TensorSize>,
    ) -> Result<(Model, Model), InferenceError> {
        self.graphs.remove(&ModelId::Pose);
        let mut load = |id| {
            let model = match capture_shape {
                Some(shape) => self.load_captured(runtime, id, shape),
                None => self.load(runtime, id),
            }?;
            self.graphs.remove(&id);
            Ok::<_, InferenceError>(model)
        };
        Ok((load(ModelId::PoseBalanced)?, load(ModelId::Fire)?))
    }

    pub fn load_profiled(
        &self,
        runtime: &Runtime,
        id: ModelId,
        profile_prefix: Option<&Path>,
    ) -> Result<Model, InferenceError> {
        self.load_mode(runtime, id, SessionMode::Dynamic { profile_prefix })
    }

    pub fn load_captured(
        &self,
        runtime: &Runtime,
        id: ModelId,
        size: TensorSize,
    ) -> Result<Model, InferenceError> {
        if runtime.cuda.is_none() {
            return Err(InferenceError::CudaUnavailable);
        }
        if !matches!(id, ModelId::PoseBalanced | ModelId::Fire)
            || self.profile() != InputProfile::Rectangular
            || !self
                .declared_shapes()
                .is_some_and(|shapes| shapes.contains(&size.dimensions()))
        {
            return Err(InferenceError::Manifest);
        }
        self.load_mode(runtime, id, SessionMode::Captured(size))
    }

    fn load_mode(
        &self,
        runtime: &Runtime,
        id: ModelId,
        mode: SessionMode<'_>,
    ) -> Result<Model, InferenceError> {
        let entry = self
            .manifest
            .models
            .get(&id)
            .ok_or(InferenceError::Manifest)?;
        let bytes = self.graphs.get(&id).ok_or(InferenceError::Manifest)?;
        Model::open(
            runtime,
            id,
            self.profile(),
            &entry.input.shape,
            &entry.output.shape,
            bytes,
            mode,
        )
    }

    pub fn parity_report(&self) -> Result<Vec<u8>, InferenceError> {
        read_verified(
            &self.root.join("parity-report.json"),
            512 * 1024,
            &self.manifest.parity_report_sha256,
        )
    }
}

impl Manifest {
    fn contract_valid(&self) -> bool {
        match (self.schema_version, &self.contract) {
            (2, None) => self.baseline == BASELINE,
            (3, Some(contract)) => {
                let spec = include_bytes!("../../../qualification/observation-parity-v1.json");
                let shapes = &contract.tensor_shapes;
                let shapes_valid = (1..=16).contains(&shapes.len())
                    && shapes.iter().enumerate().all(|(i, &[h, w])| {
                        TensorSize::new(h, w).is_ok_and(|size| self.profile.accepts(size))
                            && !shapes[..i].contains(&[h, w])
                    });
                canonical_hex(&self.baseline, 40)
                    && contract.source_revision == self.baseline
                    && contract.observation_spec_sha256 == format!("{:x}", Sha256::digest(spec))
                    && matches!(
                        contract.compatible_policy.as_str(),
                        "local-policy-4"
                            | "local-policy-5"
                            | "local-policy-6"
                            | "local-policy-7"
                            | "local-policy-8"
                            | "local-policy-9"
                            | "local-policy-10"
                            | "local-policy-11"
                    )
                    && contract.roles.person == ModelId::PoseBalanced
                    && contract.roles.hazard == ModelId::Fire
                    && shapes_valid
                    && self.models.values().all(|entry| {
                        entry
                            .source
                            .sha256
                            .as_deref()
                            .is_some_and(|hash| canonical_hex(hash, 64))
                            && entry
                                .source
                                .tensor_sha256
                                .as_deref()
                                .is_some_and(|hash| canonical_hex(hash, 64))
                    })
            }
            _ => false,
        }
    }
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub(crate) fn read_verified(
    path: &Path,
    limit: u64,
    digest: &str,
) -> Result<Vec<u8>, InferenceError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(InferenceError::ArtifactSize);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(InferenceError::ArtifactSize);
    }
    if !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(digest) {
        return Err(InferenceError::DigestMismatch);
    }
    Ok(bytes)
}

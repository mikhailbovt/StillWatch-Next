use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{InferenceError, Model, RUNTIME_VERSION, Runtime};

const BASELINE: &str = "b0d20a20d38a5bbc22178d150999e57cc9e7bfb3";
pub(crate) const SIDE: usize = 640;

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
        self == Self::Rectangular || shape == TensorSize::SQUARE
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
        if [height, width]
            .into_iter()
            .all(|v| (32..=SIDE).contains(&v) && v.is_multiple_of(32))
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
}

#[derive(Deserialize)]
struct Entry {
    artifact: String,
    sha256: String,
    byte_length: u64,
    opset: u32,
    external_data: Vec<serde_json::Value>,
    input: Input,
    output: Output,
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
        self.artifact == format!("{}.onnx", id.name())
            && (1..=64 * 1024 * 1024).contains(&self.byte_length)
            && self.opset == 17
            && self.external_data.is_empty()
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
}

impl ModelPack {
    pub fn open(root: &Path, trusted_manifest_sha256: &str) -> Result<Self, InferenceError> {
        let root = root.canonicalize()?;
        let bytes = read_verified(
            &root.join("models.lock.json"),
            128 * 1024,
            trusted_manifest_sha256,
        )?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| InferenceError::Manifest)?;
        if manifest.schema_version != 2
            || manifest.baseline != BASELINE
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
        Ok(Self { root, manifest })
    }

    pub fn profile(&self) -> InputProfile {
        self.manifest.profile
    }

    pub fn load(&self, runtime: &Runtime, id: ModelId) -> Result<Model, InferenceError> {
        let entry = self
            .manifest
            .models
            .get(&id)
            .ok_or(InferenceError::Manifest)?;
        let bytes = read_verified(
            &self.root.join(&entry.artifact),
            entry.byte_length,
            &entry.sha256,
        )?;
        if bytes.len() as u64 != entry.byte_length {
            return Err(InferenceError::ArtifactSize);
        }
        Model::open(
            runtime,
            id,
            self.profile(),
            &entry.input.shape,
            &entry.output.shape,
            &bytes,
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

fn read_verified(path: &Path, limit: u64, digest: &str) -> Result<Vec<u8>, InferenceError> {
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

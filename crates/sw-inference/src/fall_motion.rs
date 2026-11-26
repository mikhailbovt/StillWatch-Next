//! Temporal scoring for reviewed pose trajectories. A diagnostic session cannot publish incidents.

use std::path::Path;

use ort::{session::Session, value::TensorRef};
use sw_domain::{FALL_FEATURE_CHANNELS, FALL_FEATURE_CONTRACT, FALL_FEATURE_STEPS};

use crate::{InferenceError, Runtime, is_tensor, onnx, ort_error, pack::read_verified};

pub const FALL_ACTION_CLASSES: [&str; 7] = [
    "upright_or_walk",
    "bend_or_pickup",
    "sit_or_kneel",
    "lie_down_transition",
    "fall_transition",
    "low_after_observed_fall",
    "uncertain_or_occluded",
];

pub(crate) const INPUT_NAME: &str = "pose_features";
pub(crate) const OUTPUT_NAME: &str = "action_logits";
const INPUT_SHAPE: [i64; 3] = [-1, FALL_FEATURE_CHANNELS as i64, FALL_FEATURE_STEPS as i64];
const OUTPUT_SHAPE: [i64; 2] = [-1, FALL_ACTION_CLASSES.len() as i64];
const MODEL_LIMIT: u64 = 2 * 1024 * 1024;

/// The PoseBalanced artifact, input profile and execution provider used to produce observations.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallPoseProvenance {
    pub sha256: String,
    pub profile: String,
    pub execution_provider: String,
}

pub struct FallMotion {
    session: Session,
}

impl FallMotion {
    pub fn open(
        runtime: &Runtime,
        path: &Path,
        expected_sha256: &str,
        pose: &FallPoseProvenance,
        profile_prefix: Option<&Path>,
    ) -> Result<Self, InferenceError> {
        if pose.sha256.len() != 64
            || !pose.sha256.bytes().all(|c| c.is_ascii_hexdigit())
            || pose.profile.is_empty()
            || pose.execution_provider.is_empty()
        {
            return Err(InferenceError::Manifest);
        }
        let bytes = read_verified(path, MODEL_LIMIT, expected_sha256)?;
        onnx::validate_fall_motion(&bytes, &INPUT_SHAPE, &OUTPUT_SHAPE)?;
        let mut builder = runtime.session_builder()?;
        if let Some(prefix) = profile_prefix {
            builder = builder.with_profiling(prefix).map_err(ort_error)?;
        }
        if let Some(config) = &runtime.cuda {
            builder = builder
                .with_execution_providers([config.registration(false)])
                .map_err(ort_error)?;
        }
        let session = builder.commit_from_memory(&bytes).map_err(ort_error)?;
        if !is_tensor(session.inputs(), INPUT_NAME, &INPUT_SHAPE)
            || !is_tensor(session.outputs(), OUTPUT_NAME, &OUTPUT_SHAPE)
        {
            return Err(InferenceError::Tensor);
        }
        {
            let meta = session.metadata().map_err(ort_error)?;
            let matches = |key, value| meta.custom(key).as_deref() == Some(value);
            let classes = meta.custom("classes").and_then(|value| {
                serde_json::from_str::<[String; FALL_ACTION_CLASSES.len()]>(&value).ok()
            });
            if !matches("feature_contract", FALL_FEATURE_CONTRACT)
                || !matches("pose_model_sha256", pose.sha256.as_str())
                || !matches("pose_profile", pose.profile.as_str())
                || !matches("pose_execution_provider", pose.execution_provider.as_str())
                || classes
                    .as_ref()
                    .map(|entries| entries.each_ref().map(String::as_str))
                    != Some(FALL_ACTION_CLASSES)
            {
                return Err(InferenceError::Manifest);
            }
        }
        Ok(Self { session })
    }

    /// Evaluate one causal feature window and return raw logits in the declared class order.
    pub fn score(
        &mut self,
        channels: &[[f32; FALL_FEATURE_STEPS]; FALL_FEATURE_CHANNELS],
    ) -> Result<[f32; FALL_ACTION_CLASSES.len()], InferenceError> {
        let values = channels.as_flattened();
        if values.iter().any(|value| !value.is_finite()) {
            return Err(InferenceError::Tensor);
        }
        let input =
            TensorRef::from_array_view(([1, FALL_FEATURE_CHANNELS, FALL_FEATURE_STEPS], values))
                .map_err(ort_error)?;
        let outputs = self.session.run(ort::inputs![input]).map_err(ort_error)?;
        let (shape, output) = outputs
            .get(OUTPUT_NAME)
            .ok_or(InferenceError::Tensor)?
            .try_extract_tensor::<f32>()
            .map_err(ort_error)?;
        if **shape != [1, OUTPUT_SHAPE[1]] || output.iter().any(|value| !value.is_finite()) {
            return Err(InferenceError::Tensor);
        }
        output.try_into().map_err(|_| InferenceError::Tensor)
    }

    pub fn finish_profile(&mut self) -> Result<String, InferenceError> {
        self.session.end_profiling().map_err(ort_error)
    }
}

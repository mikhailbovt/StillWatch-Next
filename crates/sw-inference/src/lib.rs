#![forbid(unsafe_code)]

mod appearance;
mod onnx;
mod pack;
mod postprocess;
mod preprocess;

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ort::{
    environment::Environment,
    session::{Session, builder::GraphOptimizationLevel},
    value::{Outlet, TensorElementType, TensorRef, ValueType},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use pack::{InputProfile, ModelId, ModelPack, TensorSize};
pub use postprocess::decode;
pub use preprocess::{Image, Letterbox, Preprocessor};

const RUNTIME_VERSION: &str = "1.31.0";

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model or runtime artifact could not be read")]
    Io(#[from] io::Error),
    #[error("artifact SHA-256 does not match its trusted manifest")]
    DigestMismatch,
    #[error("model manifest is invalid or has an unsupported profile")]
    Manifest,
    #[error("ONNX graph is invalid or outside the supported model profile")]
    ModelGraph,
    #[error("model or runtime artifact exceeds its declared size limit")]
    ArtifactSize,
    #[error("the process already loaded a different ONNX Runtime library")]
    RuntimeAlreadyInitialized,
    #[error("ONNX Runtime initialization lock was poisoned")]
    RuntimePoisoned,
    #[error("ONNX Runtime rejected the operation: {0}")]
    Ort(String),
    #[error("model input or output tensor is invalid")]
    Tensor,
    #[error("inference threads must be in 1..=64")]
    ThreadCount,
    #[error("the configured CUDA execution provider is unavailable")]
    CudaUnavailable,
}

fn ort_error(error: impl std::fmt::Display) -> InferenceError {
    InferenceError::Ort(error.to_string())
}

/// ONNX Runtime loads one shared library per worker process. The library is
/// selected from a trusted directory and checked before any ORT API is used.
pub struct Runtime {
    environment: Environment,
    threads: usize,
    cuda: Option<CudaProvider>,
}

pub struct CudaProvider {
    pub device: u16,
    pub provider_sha256: String,
    pub shared_sha256: String,
}

impl Runtime {
    pub fn load(
        path: &Path,
        expected_sha256: &str,
        threads: usize,
        cuda: Option<CudaProvider>,
    ) -> Result<Self, InferenceError> {
        static LIBRARY: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);
        if !(1..=64).contains(&threads) {
            return Err(InferenceError::ThreadCount);
        }
        let path = path.canonicalize()?;
        if let Some(config) = &cuda {
            let directory = path.parent().ok_or(InferenceError::CudaUnavailable)?;
            #[cfg(windows)]
            let (provider, shared) = (
                "onnxruntime_providers_cuda.dll",
                "onnxruntime_providers_shared.dll",
            );
            #[cfg(not(windows))]
            let (provider, shared) = (
                "libonnxruntime_providers_cuda.so",
                "libonnxruntime_providers_shared.so",
            );
            verify_library(
                &directory.join(provider),
                &config.provider_sha256,
                1024 * 1024 * 1024,
            )?;
            verify_library(
                &directory.join(shared),
                &config.shared_sha256,
                16 * 1024 * 1024,
            )?;
        }
        let mut loaded = LIBRARY
            .lock()
            .map_err(|_| InferenceError::RuntimePoisoned)?;
        if let Some((loaded_path, digest)) = loaded.as_ref() {
            if loaded_path != &path || !digest.eq_ignore_ascii_case(expected_sha256) {
                return Err(InferenceError::RuntimeAlreadyInitialized);
            }
        } else {
            verify_library(&path, expected_sha256, 256 * 1024 * 1024)?;
        }
        let builder = ort::init_from(&path).map_err(ort_error)?;
        *loaded = Some((path, expected_sha256.to_owned()));
        let environment = builder
            .with_name("StillWatch worker")
            .build()
            .map_err(ort_error)?;
        if cuda.is_some() {
            use ort::ep::ExecutionProvider;
            if !ort::ep::CUDA::is_available().map_err(ort_error)? {
                return Err(InferenceError::CudaUnavailable);
            }
        }
        Ok(Self {
            environment,
            threads,
            cuda,
        })
    }

    pub fn build_info(&self) -> &'static str {
        ort::info()
    }
}

fn verify_library(path: &Path, expected: &str, maximum: u64) -> Result<(), InferenceError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(InferenceError::ArtifactSize);
    }
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err(InferenceError::DigestMismatch);
    }
    Ok(())
}

/// Session for one locked model. Inference is synchronous by design.
pub struct Model {
    session: Session,
    id: ModelId,
    profile: InputProfile,
}

/// Synchronous tensor evaluation inside an isolated inference worker.
/// Output is borrowed for the duration of `consume` and must never escape it.
pub trait PerceptionBackend {
    fn evaluate<T>(
        &mut self,
        size: TensorSize,
        input: &[f32],
        consume: impl FnOnce(&[f32]) -> T,
    ) -> Result<T, InferenceError>;
}

impl PerceptionBackend for Model {
    fn evaluate<T>(
        &mut self,
        size: TensorSize,
        input: &[f32],
        consume: impl FnOnce(&[f32]) -> T,
    ) -> Result<T, InferenceError> {
        self.run(size, input, consume)
    }
}

impl Model {
    fn open(
        runtime: &Runtime,
        id: ModelId,
        profile: InputProfile,
        input: &[i64; 4],
        output: &[i64; 3],
        bytes: &[u8],
    ) -> Result<Self, InferenceError> {
        let mut builder = Session::builder(&runtime.environment)
            .map_err(ort_error)?
            .with_intra_threads(runtime.threads)
            .map_err(ort_error)?
            .with_inter_threads(1)
            .map_err(ort_error)?
            .with_intra_op_spinning(false)
            .map_err(ort_error)?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(ort_error)?;
        if let Some(config) = &runtime.cuda {
            let cuda = ort::ep::CUDA::default()
                .with_device_id(i32::from(config.device))
                .with_tf32(false)
                .with_conv_algorithm_search(ort::ep::cuda::ConvAlgorithmSearch::Heuristic)
                .with_conv_max_workspace(false)
                .build()
                .error_on_failure();
            builder = builder
                .with_execution_providers([cuda])
                .map_err(ort_error)?;
        }
        let session = builder.commit_from_memory(bytes).map_err(ort_error)?;
        if !is_tensor(session.inputs(), "images", input)
            || !is_tensor(session.outputs(), "output0", output)
        {
            return Err(InferenceError::Tensor);
        }
        Ok(Self {
            session,
            id,
            profile,
        })
    }

    /// The callback borrows the ORT output while its arena remains live.
    pub fn run<T>(
        &mut self,
        size: TensorSize,
        input: &[f32],
        consume: impl FnOnce(&[f32]) -> T,
    ) -> Result<T, InferenceError> {
        if !self.profile.accepts(size)
            || input.len() != size.input_values()
            || input.iter().any(|value| !value.is_finite())
        {
            return Err(InferenceError::Tensor);
        }
        let [height, width] = size.dimensions();
        let view = TensorRef::from_array_view(([1, 3, height, width], input)).map_err(ort_error)?;
        let outputs = self.session.run(ort::inputs![view]).map_err(ort_error)?;
        let (shape, output) = outputs
            .get("output0")
            .ok_or(InferenceError::Tensor)?
            .try_extract_tensor::<f32>()
            .map_err(ort_error)?;
        if **shape != [1, self.id.channels() as i64, size.anchors() as i64]
            || output.iter().any(|value| !value.is_finite())
        {
            return Err(InferenceError::Tensor);
        }
        Ok(consume(output))
    }
}

fn is_tensor(outlets: &[Outlet], name: &str, dimensions: &[i64]) -> bool {
    let [outlet] = outlets else { return false };
    outlet.name() == name
        && matches!(
            outlet.dtype(),
            ValueType::Tensor { ty: TensorElementType::Float32, shape, .. } if &**shape == dimensions
        )
}

#![forbid(unsafe_code)]

mod appearance;
mod cuda_graph;
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
    environment::{Environment, GlobalThreadPoolOptions},
    session::{Session, builder::GraphOptimizationLevel},
    value::{Outlet, TensorElementType, TensorRef, ValueType},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::cuda_graph::CudaGraph;

pub use pack::{InputProfile, ModelId, ModelPack, TensorSize};
pub use postprocess::{FireTrace, decode, decode_fire_with_proposals, trace_fire};
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
        // A worker runs its model sessions serially. They can use the same CPU threads.
        let pool = GlobalThreadPoolOptions::default()
            .with_intra_threads(threads)
            .and_then(|pool| pool.with_inter_threads(1))
            .and_then(|pool| pool.with_spin_control(false))
            .map_err(ort_error)?;
        let environment = builder
            .with_name("StillWatch worker")
            .with_telemetry(false)
            .with_global_thread_pool(pool)
            .build()
            .map_err(ort_error)?;
        if cuda.is_some() {
            use ort::ep::ExecutionProvider;
            if !ort::ep::CUDA::is_available().map_err(ort_error)? {
                return Err(InferenceError::CudaUnavailable);
            }
        }
        Ok(Self { environment, cuda })
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
    graph: Option<CudaGraph>,
    session: Session,
    id: ModelId,
    profile: InputProfile,
}

enum SessionMode<'a> {
    Dynamic { profile_prefix: Option<&'a Path> },
    Captured(TensorSize),
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
        mode: SessionMode<'_>,
    ) -> Result<Self, InferenceError> {
        let mut builder = Session::builder(&runtime.environment)
            .map_err(ort_error)?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(ort_error)?;
        // NCHWc exceeds the locked keypoint tensor error on a square RGB fixture.
        if runtime.cuda.is_none() && id == ModelId::PoseBalanced {
            builder = builder
                .with_disabled_optimizers("NchwcTransformer")
                .map_err(ort_error)?;
        }
        if let SessionMode::Dynamic {
            profile_prefix: Some(prefix),
        } = mode
        {
            builder = builder.with_profiling(prefix).map_err(ort_error)?;
        }
        let capture_shape = match mode {
            SessionMode::Captured(size) => Some(size),
            SessionMode::Dynamic { .. } => None,
        };
        if let Some(config) = &runtime.cuda {
            let cuda = ort::ep::CUDA::default()
                .with_device_id(i32::from(config.device))
                .with_tf32(false)
                .with_conv_algorithm_search(ort::ep::cuda::ConvAlgorithmSearch::Heuristic)
                .with_conv_max_workspace(false)
                .with_cuda_graph(capture_shape.is_some())
                .build()
                .error_on_failure();
            builder = builder
                .with_execution_providers([cuda])
                .map_err(ort_error)?;
        }
        if let Some(size) = capture_shape {
            let [height, width] = size.dimensions();
            builder = builder
                .with_disable_cpu_fallback()
                .map_err(ort_error)?
                .with_dimension_override("height", height as i64)
                .map_err(ort_error)?
                .with_dimension_override("width", width as i64)
                .map_err(ort_error)?
                .with_dimension_override("anchors", size.anchors() as i64)
                .map_err(ort_error)?;
        }
        let session = builder.commit_from_memory(bytes).map_err(ort_error)?;
        let graph = if let Some(size) = capture_shape {
            let [height, width] = size.dimensions();
            let input = [1, 3, height as i64, width as i64];
            let output = [1, id.channels() as i64, size.anchors() as i64];
            if !is_tensor(session.inputs(), "images", &input)
                || !is_tensor(session.outputs(), "output0", &output)
            {
                return Err(InferenceError::Tensor);
            }
            let cuda = runtime
                .cuda
                .as_ref()
                .ok_or(InferenceError::CudaUnavailable)?;
            Some(CudaGraph::new(&session, cuda.device, size, id.channels())?)
        } else {
            if !is_tensor(session.inputs(), "images", input)
                || !is_tensor(session.outputs(), "output0", output)
            {
                return Err(InferenceError::Tensor);
            }
            None
        };
        Ok(Self {
            graph,
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
        if !self.profile.accepts(size) {
            return Err(InferenceError::Tensor);
        }
        self.run_tensor(size, input, consume)
    }

    #[cfg(feature = "diagnostics")]
    pub fn diagnose_fire_scale<T>(
        &mut self,
        size: TensorSize,
        input: &[f32],
        consume: impl FnOnce(&[f32]) -> T,
    ) -> Result<T, InferenceError> {
        if self.id != ModelId::Fire
            || self.profile != InputProfile::Rectangular
            || size.dimensions().into_iter().max() != Some(pack::DIAGNOSTIC_FIRE_SIDE)
        {
            return Err(InferenceError::Tensor);
        }
        self.run_tensor(size, input, consume)
    }

    fn run_tensor<T>(
        &mut self,
        size: TensorSize,
        input: &[f32],
        consume: impl FnOnce(&[f32]) -> T,
    ) -> Result<T, InferenceError> {
        if input.len() != size.input_values() || input.iter().any(|value| !value.is_finite()) {
            return Err(InferenceError::Tensor);
        }
        let [height, width] = size.dimensions();
        let view = TensorRef::from_array_view(([1, 3, height, width], input)).map_err(ort_error)?;
        let outputs;
        let (shape, output) = if let Some(graph) = &mut self.graph {
            graph.run(&mut self.session, size, view)?
        } else {
            outputs = self.session.run(ort::inputs![view]).map_err(ort_error)?;
            outputs
                .get("output0")
                .ok_or(InferenceError::Tensor)?
                .try_extract_tensor::<f32>()
                .map_err(ort_error)?
        };
        if **shape != [1, self.id.channels() as i64, size.anchors() as i64]
            || output.iter().any(|value| !value.is_finite())
        {
            return Err(InferenceError::Tensor);
        }
        Ok(consume(output))
    }

    pub fn finish_profile(&mut self) -> Result<String, InferenceError> {
        self.session.end_profiling().map_err(ort_error)
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

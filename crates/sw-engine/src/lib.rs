#![deny(unsafe_code)]

mod camera_credentials;
mod capture_process;
mod child_process;
mod evidence;
mod hazards;
mod inference_process;
mod media;
mod onvif;
mod registry;
mod replay_upload;
mod scene;
mod semantic;
mod vision;

const MICROS_PER_SECOND: i64 = 1_000_000;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_credentials;

pub use capture_process::{
    CaptureProcess, CaptureRead, Frame, FrameBudget, WorkerBinary, WorkerError,
};
pub use inference_process::verify_inference_startup;
pub use media::{MediaAnswer, MediaConfig, MediaError, MediaService, TurnConfig};
pub use onvif::{OnvifConfig, OnvifError};
pub use registry::{CameraFrame, CameraHealth, CameraRegistry, RegistryError, SourceConfig};
pub use semantic::{
    FirebaseProvider, SemanticConfig, SemanticProvider, SemanticProviderError, SemanticSupervisor,
};
pub use vision::VisionScheduler;

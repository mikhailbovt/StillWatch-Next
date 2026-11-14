#![deny(unsafe_code)]

mod camera_credentials;
mod capture_process;
mod child_process;
mod evidence;
mod execution_host;
mod hazards;
mod inference_process;
mod media;
mod onvif;
#[cfg(feature = "diagnostics")]
mod policy_trace;
mod push;
mod registry;
mod replay_upload;
mod scene;
mod semantic;
mod task_clock;
mod telemetry;
mod vision;

const MICROS_PER_SECOND: i64 = 1_000_000;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_credentials;

pub use capture_process::{
    CaptureProcess, CaptureRead, Frame, FrameBudget, WorkerBinary, WorkerError,
};
pub use execution_host::execution_host;
pub use inference_process::verify_inference_startup;
pub use media::{MediaAnswer, MediaConfig, MediaError, MediaService, TurnConfig};
pub use onvif::{OnvifConfig, OnvifError};
#[cfg(feature = "diagnostics")]
pub use policy_trace::{trace_fire_policy, trace_pose_association};
pub use push::ApnsSender;
pub use registry::{CameraFrame, CameraHealth, CameraRegistry, RegistryError, SourceConfig};
pub use semantic::{
    FirebaseProvider, SemanticConfig, SemanticProvider, SemanticProviderError, SemanticSupervisor,
};
pub use vision::VisionScheduler;

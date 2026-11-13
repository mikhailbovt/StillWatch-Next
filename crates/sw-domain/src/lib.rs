#![forbid(unsafe_code)]

mod fire;
mod incident;
mod medical;
mod motion;
mod observation;
mod pose;
mod tracking;

pub use fire::{
    FireDecision, FireEpisode, FireFacts, FirePolicy, FireSample, FireSettings, Visibility,
};
pub use incident::*;
pub use medical::{MedicalRules, MedicalSettings};
pub use motion::{
    MedicalCandidate, MedicalReason, MotionEvidence, MotionPhase, Posture, RecoveryBasis,
    TransitionKind,
};
pub use observation::*;
pub use tracking::{PoseTracker, TrackingError};

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
pub use medical::{MedicalRules, MedicalSample, MedicalSettings};
pub use motion::{
    MedicalCandidate, MedicalReason, MotionEvidence, MotionPhase, Posture, RecoveryBasis,
    TransitionKind,
};
pub use observation::*;
pub use tracking::{PoseTracker, TrackingError};

/// A deterministic temporal policy for one camera stream. Inputs own or borrow
/// observations and scene context. State transitions stay within the policy.
pub trait HazardPolicy<Sample> {
    type Outcome;

    fn assess(&mut self, sample: Sample) -> Self::Outcome;
}

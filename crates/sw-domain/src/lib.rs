#![forbid(unsafe_code)]

mod fire;
mod fire_episode;
mod incident;
mod medical;
mod motion;
mod observation;
mod pose;
mod scene;
mod tracking;

pub use fire::{
    FireAssociation, FireDecision, FireEpisode, FireFacts, FirePolicy, FireSample, FireSettings,
    Visibility,
};
pub use incident::*;
pub use medical::{MedicalRules, MedicalSample, MedicalSettings};
pub use motion::{
    MedicalCandidate, MedicalReason, MotionEvidence, MotionPhase, Posture, RecoveryBasis,
    TransitionKind,
};
pub use observation::*;
pub use scene::{SCENE_SIGNATURE_BYTES, SceneSignature};
pub use tracking::{AssociationDiagnostic, AssociationOutcome, PoseTracker, TrackingError};

/// A deterministic temporal policy for one camera stream. Inputs own or borrow
/// observations and scene context. State transitions stay within the policy.
pub trait HazardPolicy<Sample> {
    type Outcome;

    fn assess(&mut self, sample: Sample) -> Self::Outcome;
}

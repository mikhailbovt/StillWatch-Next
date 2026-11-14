use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRoute {
    CloudOnly,
    LocalOnly,
    PreferLocal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceSource {
    InCloud,
    OnDevice,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderMode {
    CloudNode,
    HybridWeb,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Disconnected,
    Initializing,
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SemanticState {
    Disabled,
    Connecting,
    Available,
    Degraded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SemanticStatus {
    pub state: SemanticState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_ready: Option<bool>,
    pub last_inference_source: Option<String>,
    pub sdk_version: Option<String>,
    pub cloud_model: Option<String>,
    pub local_model_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_route_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_check_state: Option<AppCheckState>,
    pub used_today: u32,
    pub daily_request_limit: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCapabilities {
    pub mode: ProviderMode,
    pub sdk_version: String,
    pub model: String,
    pub semantic_evaluation: bool,
    pub cloud_transport: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_check_state: Option<AppCheckState>,
    pub local_state: LocalState,
    pub max_images: u8,
    pub max_bytes: usize,
    pub busy: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AppCheckState {
    Ready,
    RenewalDue,
    DevelopmentDebug,
    Unavailable,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observable {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    NoVisibleFire,
    LikelyControlledFlame,
    PossibleUncontrolledFire,
    LikelyUncontrolledFire,
    NoVisibleMedicalConcern,
    PossibleVisibleMedicalConcern,
    LikelyVisibleMedicalConcern,
    InsufficientEvidence,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Contradiction {
    RegionMismatch,
    TemporalMismatch,
    IdentityMismatch,
    InconsistentVisualCues,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticLimitation {
    Glare,
    Blur,
    Darkness,
    Occlusion,
    CroppedView,
    SparseCoverage,
    AmbiguousIdentity,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FireObservation {
    pub flame_visible: Observable,
    pub smoke_visible: Observable,
    pub apparent_spread: Observable,
    pub contained_in_designated_region: Observable,
    pub controlled_source_supported: Observable,
    pub visibility: crate::EvidenceVisibility,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MedicalObservation {
    pub person_visible: Observable,
    pub transition_visible: Observable,
    pub persistent_low_posture: Observable,
    pub same_subject_recovery_visible: Observable,
    pub visibility: crate::EvidenceVisibility,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SemanticObservation {
    Fire(FireObservation),
    Medical(MedicalObservation),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationReport {
    pub classification: Classification,
    pub observations: SemanticObservation,
    pub supporting_image_ids: Vec<String>,
    pub contradictions: Vec<Contradiction>,
    pub limitations: Vec<SemanticLimitation>,
    pub brief_description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticReport {
    pub sdk_version: String,
    pub inference_source: InferenceSource,
    pub model: String,
    pub usage_total_tokens: Option<u32>,
    pub observation: ObservationReport,
}

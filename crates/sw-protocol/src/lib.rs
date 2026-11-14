#![forbid(unsafe_code)]

mod alerts;
mod cameras;
mod evidence;
mod health;
mod media;
mod onvif;
mod primitives;
mod semantic;
mod trust;
#[cfg(feature = "worker")]
pub mod worker;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use alerts::*;
pub use cameras::*;
pub use evidence::*;
pub use health::*;
pub use media::*;
pub use onvif::*;
pub use primitives::{Counter, Cursor, IdempotencyKey, InvalidValue, Reason, UtcTimestamp};
pub use semantic::*;
pub use trust::*;

pub const API_PREFIX: &str = "/api/v2";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl ApiVersion {
    pub const CURRENT: Self = Self { major: 2, minor: 0 };

    pub const fn supports(self, client: Self) -> bool {
        client.major == self.major && client.minor <= self.minor
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Medical,
    Fire,
    Overview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    Unknown,
    NoVisibleConcern,
    Concern,
    ImmediateVisibleConcern,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    Info,
    Review,
    Urgent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Open,
    Resolved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Pending,
    Persisted,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Acknowledgement {
    pub actor_id: Uuid,
    pub acknowledged_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Resolution {
    pub actor_id: Option<Uuid>,
    pub resolved_at: UtcTimestamp,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalysisOrigin {
    pub model_manifest_sha256: String,
    pub policy_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Incident {
    pub id: Uuid,
    pub camera_id: Uuid,
    pub source_epoch: Counter,
    pub scene_revision: Counter,
    pub revision: Counter,
    pub evidence_version: Counter,
    pub evidence_packet_id: Option<Uuid>,
    pub evidence_state: EvidenceState,
    pub category: Category,
    pub detection_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_origin: Option<AnalysisOrigin>,
    pub assessment: Assessment,
    pub attention: Attention,
    pub attention_cycle: Counter,
    pub lifecycle: Lifecycle,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_reassessment: bool,
    pub acknowledgement: Option<Acknowledgement>,
    pub resolution: Option<Resolution>,
    pub live: bool,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EventSchema {
    #[serde(rename = "1.0")]
    V1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "payload")]
pub enum ChangeKind {
    #[serde(rename = "incident.opened")]
    IncidentOpened(Incident),
    #[serde(rename = "incident.updated")]
    IncidentUpdated(Incident),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Change {
    pub schema_version: EventSchema,
    pub stream_generation: Uuid,
    pub sequence: Counter,
    pub entity_id: Uuid,
    pub entity_revision: Counter,
    pub occurred_at: UtcTimestamp,
    #[serde(flatten)]
    pub kind: ChangeKind,
}

impl Change {
    pub fn cursor(&self) -> Cursor {
        Cursor {
            generation: self.stream_generation,
            sequence: self.sequence,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Snapshot {
    pub cursor: Cursor,
    pub incidents: Vec<Incident>,
    /// Older open incidents are available through the incident history endpoint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_older_open_incidents: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct IncidentPage {
    pub incidents: Vec<Incident>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeRequest {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FeedbackClassification {
    Confirmed,
    FalseAlarm,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    pub classification: FeedbackClassification,
    pub evidence_version: Counter,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SilenceRequest {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 3600)))]
    pub duration_seconds: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub code: ProblemCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProblemCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    InvalidTransition,
    CapacityExceeded,
    CursorExpired,
    RateLimited,
    ProviderUnavailable,
    StorageUnavailable,
    InternalError,
    #[serde(other)]
    Unrecognized,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Bootstrap {
    pub instance_id: Uuid,
    pub version: ApiVersion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Health {
    pub state: HealthState,
    pub api: ApiVersion,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Live,
    Ready,
    Degraded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InferenceBackend {
    Cpu,
    Cuda,
    #[serde(other)]
    Unrecognized,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InferenceFailure {
    ModelPackUnavailable,
    ModelInitializationFailed,
    RuntimeUnavailable,
    WorkerUnavailable,
    WorkerTimeout,
    WorkerFailed,
    EvidenceBackpressure,
    #[serde(other)]
    Unrecognized,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InferenceStatus {
    pub state: AnalysisState,
    pub backend: Option<InferenceBackend>,
    pub ready_workers: usize,
    pub configured_workers: usize,
    pub failure: Option<InferenceFailure>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MediaStatus {
    pub router_ready: bool,
    pub video_codec: String,
    pub redacted_transcode: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoreStatus {
    pub version: String,
    pub inference: InferenceStatus,
    pub media: Option<MediaStatus>,
}

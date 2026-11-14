#![forbid(unsafe_code)]

mod alerts;
mod analysis_metrics;
mod cameras;
mod capacity;
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
pub use analysis_metrics::*;
pub use cameras::*;
pub use capacity::*;
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UncertainIncidentLink {
    pub incident_id: Uuid,
    pub source_epoch: Counter,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub possible_continuation_of: Option<UncertainIncidentLink>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_observed_host_utc: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opening_delayed: bool,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub camera_sampling: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub camera_probe_capacity: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub analysis_capacity_probe: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub camera_priority: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub camera_main_stream: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_replay: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ios_push: bool,
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
    /// Task totals since Core started. Visible to the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<InferenceCounts>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MediaStatus {
    pub router_ready: bool,
    pub video_codec: String,
    pub redacted_transcode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_limits: Option<MediaWorkloadLimits>,
}

/// Simultaneous allowances on the host, including both encoder and decoder work.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MediaWorkloadLimits {
    pub preview_publishers: u16,
    pub detail_publishers: u16,
    pub detail_decoders: u16,
    pub viewer_sessions: u16,
    pub publisher_units: u16,
    pub preview_units: u16,
    pub detail_units: u16,
    pub detail_decoder_units: u16,
    pub snapshot_encoders: u16,
    pub snapshot_requests_per_minute: u32,
}

impl MediaWorkloadLimits {
    pub const DEFAULT_LIMITS: Self = Self {
        preview_publishers: 8,
        detail_publishers: 8,
        detail_decoders: 8,
        viewer_sessions: 32,
        publisher_units: 8,
        preview_units: 1,
        detail_units: 1,
        detail_decoder_units: 0,
        snapshot_encoders: 4,
        snapshot_requests_per_minute: 0,
    };

    pub fn valid(self) -> bool {
        self.viewer_sessions <= 128
            && self.preview_publishers <= 32
            && self.detail_publishers <= 32
            && self.detail_decoders <= self.detail_publishers
            && self.snapshot_encoders <= 16
            && self.snapshot_requests_per_minute <= 3_600
            && self.preview_units > 0
            && self.detail_units > 0
            && (self.preview_publishers == 0 || self.preview_units <= self.publisher_units)
            && (self.detail_publishers == 0 || self.detail_units <= self.publisher_units)
            && (self.detail_decoders == 0
                || u32::from(self.detail_units) + u32::from(self.detail_decoder_units)
                    <= u32::from(self.publisher_units))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoreStatus {
    pub version: String,
    pub inference: InferenceStatus,
    /// Highest attention level available from currently loaded local models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_attention_ceiling: Option<Attention>,
    pub media: Option<MediaStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer: Option<WriterStatus>,
    /// Bounded local snapshot sized by the camera registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_coverage: Option<Vec<CameraCoverage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_capacity: Option<AnalysisCapacity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WriterStatus {
    pub queue_capacity: usize,
    pub queued: usize,
    pub queue_high_water: usize,
    pub running_ms: Option<Counter>,
    pub oldest_queued_ms: Option<Counter>,
    pub completed: Counter,
    pub rejected_queue_full: Counter,
    pub cancelled_queued: Counter,
    pub maximum_queue_wait_ms: Counter,
    pub maximum_operation_ms: Counter,
}

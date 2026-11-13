use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Category, Counter, UtcTimestamp};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EvidenceInterval {
    pub start_source_s: f64,
    pub end_source_s: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EvidenceCoverage {
    pub expected_samples: u16,
    pub received_samples: u16,
    pub max_gap_ms: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceVisibility {
    Good,
    Limited,
    Poor,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalFacts {
    Fire {
        flame_visible: bool,
        smoke_visible: bool,
        apparent_growth: bool,
        smoke_growth: bool,
        escaped_controlled_region: bool,
        controlled_region_overlap: Option<f64>,
        continuity_samples: u16,
        detector_confidence: f64,
        visibility: EvidenceVisibility,
    },
    Medical {
        track_id: Counter,
        episode_id: Uuid,
        previous_episode_id: Option<Uuid>,
        onset_observed: bool,
        identity_uncertain: bool,
        recovery_seconds: f64,
        clear_eligible: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLimitation {
    PartialVisibility,
    SparseCoverage,
    SourceChanged,
    PreEventUnavailable,
    NonLive,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EvidenceContext {
    pub observed_interval: EvidenceInterval,
    pub coverage: EvidenceCoverage,
    pub local_facts: LocalFacts,
    pub limitations: Vec<EvidenceLimitation>,
    pub policy_version: String,
    pub prompt_version: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceImageKind {
    Overview,
    Crop,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EvidenceImageAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EvidenceImageMetadata {
    pub image_id: String,
    pub ordinal: u8,
    pub source_s: f64,
    pub kind: EvidenceImageKind,
    pub sha256: Option<String>,
    pub mime: String,
    pub availability: EvidenceImageAvailability,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EvidencePacket {
    pub schema_version: String,
    pub packet_id: Uuid,
    pub incident_id: Uuid,
    pub camera_id: Uuid,
    pub source_epoch: Counter,
    pub evidence_version: Counter,
    pub scene_revision: Counter,
    pub category: Category,
    pub context: Option<EvidenceContext>,
    pub images: Vec<EvidenceImageMetadata>,
    pub expires_at: UtcTimestamp,
}

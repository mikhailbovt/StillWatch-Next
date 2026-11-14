use sw_domain as domain;
use sw_protocol as wire;

use crate::errors::ApiError;

pub fn incident(item: domain::Incident) -> Result<wire::Incident, ApiError> {
    let lifecycle = if item.resolution.is_some() {
        wire::Lifecycle::Resolved
    } else {
        wire::Lifecycle::Open
    };
    let evidence_version = wire::Counter(item.evidence_version());
    let acknowledgement = item
        .acknowledgement
        .map(|value| -> Result<_, ApiError> {
            Ok(wire::Acknowledgement {
                actor_id: value.actor_id,
                acknowledged_at: timestamp(value.at)?,
            })
        })
        .transpose()?;
    let resolution = item
        .resolution
        .map(|value| -> Result<_, ApiError> {
            Ok(wire::Resolution {
                actor_id: value.actor_id,
                resolved_at: timestamp(value.at)?,
                reason: value.reason,
            })
        })
        .transpose()?;
    Ok(wire::Incident {
        id: item.id,
        camera_id: item.scope.camera_id,
        source_epoch: wire::Counter(item.scope.epoch),
        scene_revision: wire::Counter(item.scope.scene_revision),
        revision: wire::Counter(item.revision),
        evidence_version,
        evidence_packet_id: item.evidence.map(|stamp| stamp.packet_id),
        evidence_state: match item.evidence_state {
            domain::EvidenceState::Pending => wire::EvidenceState::Pending,
            domain::EvidenceState::Persisted => wire::EvidenceState::Persisted,
            domain::EvidenceState::Partial => wire::EvidenceState::Partial,
            domain::EvidenceState::Unavailable => wire::EvidenceState::Unavailable,
        },
        category: match item.category {
            domain::Category::Medical => wire::Category::Medical,
            domain::Category::Fire => wire::Category::Fire,
            domain::Category::Overview => wire::Category::Overview,
        },
        detection_reason: item.detection_reason,
        analysis_origin: item.analysis_origin.map(|origin| wire::AnalysisOrigin {
            model_manifest_sha256: origin.model_manifest_sha256,
            policy_version: origin.policy_version,
        }),
        possible_continuation_of: item.possible_continuation_of.map(|link| {
            wire::UncertainIncidentLink {
                incident_id: link.incident_id,
                source_epoch: wire::Counter(link.source_epoch),
            }
        }),
        assessment: match item.assessment {
            domain::Assessment::Unknown => wire::Assessment::Unknown,
            domain::Assessment::NoVisibleConcern => wire::Assessment::NoVisibleConcern,
            domain::Assessment::Concern => wire::Assessment::Concern,
            domain::Assessment::ImmediateVisibleConcern => {
                wire::Assessment::ImmediateVisibleConcern
            }
        },
        attention: match item.attention {
            domain::Attention::Info => wire::Attention::Info,
            domain::Attention::Review => wire::Attention::Review,
            domain::Attention::Urgent => wire::Attention::Urgent,
        },
        attention_cycle: wire::Counter(item.attention_cycle),
        lifecycle,
        needs_reassessment: item.needs_reassessment,
        acknowledgement,
        resolution,
        live: item.live,
        created_at: timestamp(item.created_at)?,
        updated_at: timestamp(item.updated_at)?,
        opening_observed_host_utc: item.opening_observed_host_utc.map(timestamp).transpose()?,
        opening_delayed: item.opening_delayed,
    })
}

pub fn timestamp(value: i64) -> Result<wire::UtcTimestamp, ApiError> {
    wire::UtcTimestamp::from_unix_millis(value).map_err(|_| ApiError::internal())
}

pub fn cursor(value: sw_storage::ChangeCursor) -> wire::Cursor {
    wire::Cursor {
        generation: value.generation,
        sequence: wire::Counter(value.sequence),
    }
}

pub fn store_cursor(value: wire::Cursor) -> sw_storage::ChangeCursor {
    sw_storage::ChangeCursor {
        generation: value.generation,
        sequence: value.sequence.0,
    }
}

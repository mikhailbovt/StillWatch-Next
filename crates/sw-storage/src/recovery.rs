use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sw_domain::{Attention, Category, EvidenceState, RecoveryCourse, SourceScope};
use sw_protocol::{EvidenceContext, EvidenceRecovery, LocalFacts};
use uuid::Uuid;

use crate::{
    ObservationTime, Store, StoreError,
    db::{get_incident, id, update_incident, verify_camera_scope},
    utc_ms,
};

const RECOVERY_MAX_AGE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy)]
pub struct RecoveryIntent {
    pub incident_id: Uuid,
    pub scope: SourceScope,
    pub episode_id: Uuid,
    pub course: RecoveryCourse,
    pub source_us: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome {
    Applied,
    NeedsEvidence,
    Superseded,
    Inapplicable,
}

impl RecoveryIntent {
    pub fn observation(self) -> EvidenceRecovery {
        match self.course {
            RecoveryCourse::UprightActivity => EvidenceRecovery::UprightActivity,
            RecoveryCourse::PriorSupportedPosture => EvidenceRecovery::PriorSupportedPosture,
            RecoveryCourse::ControlledActivity => EvidenceRecovery::ControlledActivity,
        }
    }
}

impl Store {
    pub async fn apply_recovery(
        &self,
        intent: RecoveryIntent,
        observed: ObservationTime,
    ) -> Result<RecoveryOutcome, StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let incident = get_incident(&tx, intent.incident_id)?;
            if let Some(resolution) = incident.resolution {
                return Ok(if resolution.actor_id.is_none()
                    && resolution.reason == intent.course.resolution_reason()
                    && incident.last_source_us == intent.source_us
                    && incident.scope == intent.scope
                {
                    RecoveryOutcome::Applied
                } else {
                    RecoveryOutcome::Inapplicable
                });
            }
            if incident.category != Category::Medical
                || incident.attention != Attention::Review
                || incident.acknowledgement.is_some()
            {
                return Ok(RecoveryOutcome::Inapplicable);
            }
            if let Err(error) = verify_camera_scope(&tx, intent.scope) {
                return match error {
                    StoreError::Domain(sw_domain::DomainError::Stale) => Ok(RecoveryOutcome::Superseded),
                    error => Err(error),
                };
            }
            let now = utc_ms();
            if incident.scope != intent.scope
                || incident.last_source_us != intent.source_us
                || !observed.fresh(now, Instant::now(), RECOVERY_MAX_AGE)
            {
                return Ok(RecoveryOutcome::Superseded);
            }
            let Some(stamp) = incident.evidence.filter(|stamp| {
                stamp.scope == intent.scope && stamp.expires_at == observed.expires_at()
            }) else {
                return Ok(RecoveryOutcome::NeedsEvidence);
            };
            if incident.evidence_state != EvidenceState::Persisted {
                return Ok(RecoveryOutcome::NeedsEvidence);
            }
            let context: Option<String> = tx
                .query_row(
                    "SELECT p.context FROM evidence_packets p
                     WHERE p.id = ? AND p.observed_host_utc = ? AND p.semantic_eligible = 1
                     AND EXISTS (
                        SELECT 1 FROM evidence_images e JOIN artifacts a ON a.digest = e.digest
                        WHERE e.packet_id = p.id AND e.kind = 'overview' AND e.source_us = ?
                          AND e.retired = 0 AND a.available = 1
                     )",
                    params![id(&stamp.packet_id), observed.observed_host_utc, intent.source_us],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let Some(context) = context else {
                return Ok(RecoveryOutcome::NeedsEvidence);
            };
            let context: EvidenceContext = serde_json::from_str(&context)?;
            if context.recovery != Some(intent.observation())
                || !matches!(context.local_facts, LocalFacts::Medical {
                episode_id, onset_observed: true, identity_uncertain: false, clear_eligible: true, ..
            } if episode_id == intent.episode_id)
                || context.limitations.iter().any(|limitation| {
                    matches!(
                        limitation,
                        sw_protocol::EvidenceLimitation::ObservationGap |
                        sw_protocol::EvidenceLimitation::PartialVisibility |
                        sw_protocol::EvidenceLimitation::SourceChanged
                    )
                })
            {
                return Ok(RecoveryOutcome::Superseded);
            }
            if !observed.fresh(utc_ms(), Instant::now(), RECOVERY_MAX_AGE) {
                return Ok(RecoveryOutcome::Superseded);
            }
            let transition = incident.record_recovery_course(intent.scope, intent.source_us, intent.course, now)?;
            update_incident(&tx, &transition)?;
            tx.commit()?;
            db.notify.send_replace(());
            Ok(RecoveryOutcome::Applied)
        }).await
    }
}

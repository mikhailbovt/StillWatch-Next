use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// UTC milliseconds. Source-clock time is always carried separately.
pub type UnixMillis = i64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SourceScope {
    pub camera_id: Uuid,
    pub epoch: u64,
    pub scene_revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Medical,
    Fire,
    Overview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    Unknown,
    NoVisibleConcern,
    Concern,
    ImmediateVisibleConcern,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    Info,
    Review,
    Urgent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Clearance {
    Unsubstantiated,
    SameEpisodeRecovery,
    CorroboratedFireCourse,
}

impl Clearance {
    fn supports(self, category: Category) -> bool {
        matches!(
            (self, category),
            (Self::SameEpisodeRecovery, Category::Medical)
                | (Self::CorroboratedFireCourse, Category::Fire)
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceStamp {
    pub packet_id: Uuid,
    pub version: u64,
    pub scope: SourceScope,
    pub expires_at: UnixMillis,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    Pending,
    Persisted,
    Partial,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Acknowledgement {
    pub actor_id: Uuid,
    pub at: UnixMillis,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Resolution {
    pub actor_id: Option<Uuid>,
    pub at: UnixMillis,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalysisOrigin {
    pub model_manifest_sha256: String,
    pub policy_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Incident {
    pub id: Uuid,
    pub scope: SourceScope,
    pub revision: u64,
    pub category: Category,
    pub detection_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_origin: Option<AnalysisOrigin>,
    pub first_source_us: i64,
    pub last_source_us: i64,
    pub live: bool,
    pub created_at: UnixMillis,
    pub updated_at: UnixMillis,
    pub assessment: Assessment,
    pub attention: Attention,
    pub attention_cycle: u64,
    pub acknowledgement: Option<Acknowledgement>,
    pub resolution: Option<Resolution>,
    pub evidence: Option<EvidenceStamp>,
    pub evidence_state: EvidenceState,
    pub assessed_evidence_version: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_reassessment: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewIncident {
    pub id: Uuid,
    pub scope: SourceScope,
    pub category: Category,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_origin: Option<AnalysisOrigin>,
    pub first_source_us: i64,
    pub last_source_us: i64,
    pub live: bool,
    pub assessment: Assessment,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackClassification {
    Confirmed,
    FalseAlarm,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OperatorAction {
    Acknowledge,
    Resolve {
        reason: String,
    },
    Feedback {
        classification: FeedbackClassification,
        evidence_version: u64,
        reason: String,
    },
    Silence {
        duration_seconds: u32,
    },
}

impl OperatorAction {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Acknowledge => "acknowledge",
            Self::Resolve { .. } => "resolve",
            Self::Feedback { .. } => "feedback",
            Self::Silence { .. } => "silence",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceSilence {
    pub device_id: Uuid,
    pub attention_cycle: u64,
    pub until: UnixMillis,
}

#[derive(Debug)]
pub struct Transition {
    pub incident: Incident,
    pub notification: Option<Attention>,
    pub silence: Option<DeviceSilence>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DomainError {
    #[error("incident is already resolved")]
    Resolved,
    #[error("incident has already been acknowledged for this attention cycle")]
    Acknowledged,
    #[error("source context, packet, or evidence version is stale")]
    Stale,
    #[error("incident revision is exhausted")]
    RevisionExhausted,
    #[error("reason must contain 1 to 512 non-control characters")]
    Reason,
    #[error("incident observation interval is invalid")]
    SourceTime,
    #[error("silence is available for live review or urgent attention for 1 to 3600 seconds")]
    Silence,
    #[error("evidence persistence state is inconsistent")]
    EvidenceState,
}

impl Incident {
    pub fn create(new: NewIncident, now: UnixMillis) -> Result<Transition, DomainError> {
        validate_reason(&new.reason)?;
        if new.first_source_us > new.last_source_us {
            return Err(DomainError::SourceTime);
        }
        let mut incident = Self {
            id: new.id,
            scope: new.scope,
            revision: 1,
            category: new.category,
            detection_reason: new.reason,
            analysis_origin: new.analysis_origin,
            first_source_us: new.first_source_us,
            last_source_us: new.last_source_us,
            live: new.live,
            created_at: now,
            updated_at: now,
            assessment: Assessment::Unknown,
            attention: Attention::Info,
            attention_cycle: 0,
            acknowledgement: None,
            resolution: None,
            evidence: None,
            evidence_state: EvidenceState::Pending,
            assessed_evidence_version: None,
            needs_reassessment: false,
        };
        let notification = incident.assess(new.assessment, Clearance::Unsubstantiated, now)?;
        Ok(Transition {
            incident,
            notification,
            silence: None,
        })
    }

    pub fn evidence_version(&self) -> u64 {
        self.evidence.map_or(0, |stamp| stamp.version)
    }

    pub fn attach_evidence(
        &self,
        stamp: EvidenceStamp,
        state: EvidenceState,
        now: UnixMillis,
    ) -> Result<Transition, DomainError> {
        if self.resolution.is_some() {
            return Err(DomainError::Resolved);
        }
        if stamp.scope != self.scope
            || stamp.version != increment(self.evidence_version())?
            || stamp.expires_at <= now
        {
            return Err(DomainError::Stale);
        }
        if state == EvidenceState::Pending {
            return Err(DomainError::EvidenceState);
        }
        let mut incident = self.next(now)?;
        incident.evidence = Some(stamp);
        incident.evidence_state = state;
        incident.assessed_evidence_version = None;
        Ok(Transition {
            incident,
            notification: None,
            silence: None,
        })
    }

    pub fn record_observation(
        &self,
        scope: SourceScope,
        source_us: i64,
        assessment: Assessment,
        now: UnixMillis,
    ) -> Result<Transition, DomainError> {
        if self.resolution.is_some() {
            return Err(DomainError::Resolved);
        }
        if scope != self.scope || source_us <= self.last_source_us {
            return Err(DomainError::Stale);
        }
        let mut incident = self.next(now)?;
        incident.last_source_us = source_us;
        let notification = incident.assess(assessment, Clearance::Unsubstantiated, now)?;
        Ok(Transition {
            incident,
            notification,
            silence: None,
        })
    }

    pub fn record_semantic(
        &self,
        stamp: EvidenceStamp,
        assessment: Assessment,
        clearance: Clearance,
        now: UnixMillis,
    ) -> Result<Transition, DomainError> {
        if self.resolution.is_some() {
            return Err(DomainError::Resolved);
        }
        if self.evidence != Some(stamp)
            || stamp.expires_at <= now
            || self.assessed_evidence_version == Some(stamp.version)
            || !matches!(
                self.evidence_state,
                EvidenceState::Persisted | EvidenceState::Partial
            )
        {
            return Err(DomainError::Stale);
        }
        let mut incident = self.next(now)?;
        incident.assessed_evidence_version = Some(stamp.version);
        let notification = incident.assess(assessment, clearance, now)?;
        Ok(Transition {
            incident,
            notification,
            silence: None,
        })
    }

    pub fn record_recovery(
        &self,
        scope: SourceScope,
        source_us: i64,
        now: UnixMillis,
    ) -> Result<Transition, DomainError> {
        if self.resolution.is_some() {
            return Err(DomainError::Resolved);
        }
        if scope != self.scope
            || source_us <= self.last_source_us
            || self.category != Category::Medical
            || self.evidence_state != EvidenceState::Persisted
        {
            return Err(DomainError::Stale);
        }
        let mut incident = self.next(now)?;
        incident.last_source_us = source_us;
        incident.assessed_evidence_version = incident.evidence.map(|stamp| stamp.version);
        let notification = incident.assess(
            Assessment::NoVisibleConcern,
            Clearance::SameEpisodeRecovery,
            now,
        )?;
        Ok(Transition {
            incident,
            notification,
            silence: None,
        })
    }

    pub fn evidence_unavailable(&self, now: UnixMillis) -> Result<Transition, DomainError> {
        if self.evidence_state != EvidenceState::Pending || self.evidence.is_some() {
            return Err(DomainError::EvidenceState);
        }
        let mut incident = self.next(now)?;
        incident.evidence_state = EvidenceState::Unavailable;
        Ok(Transition {
            incident,
            notification: None,
            silence: None,
        })
    }

    pub fn apply(
        &self,
        actor_id: Uuid,
        action: &OperatorAction,
        now: UnixMillis,
    ) -> Result<Transition, DomainError> {
        if self.resolution.is_some() && !matches!(action, OperatorAction::Feedback { .. }) {
            return Err(DomainError::Resolved);
        }
        let mut incident = self.next(now)?;
        let mut silence = None;
        match action {
            OperatorAction::Acknowledge => {
                if self.acknowledgement.is_some() {
                    return Err(DomainError::Acknowledged);
                }
                incident.acknowledgement = Some(Acknowledgement {
                    actor_id,
                    at: incident.updated_at,
                });
            }
            OperatorAction::Resolve { reason } => {
                validate_reason(reason)?;
                incident.resolution = Some(Resolution {
                    actor_id: Some(actor_id),
                    at: incident.updated_at,
                    reason: reason.clone(),
                });
            }
            OperatorAction::Feedback {
                evidence_version,
                reason,
                ..
            } => {
                validate_reason(reason)?;
                if *evidence_version != self.evidence_version() {
                    return Err(DomainError::Stale);
                }
            }
            OperatorAction::Silence { duration_seconds } => {
                if !self.live
                    || self.attention == Attention::Info
                    || !(1..=3_600).contains(duration_seconds)
                {
                    return Err(DomainError::Silence);
                }
                let until = incident
                    .updated_at
                    .checked_add(i64::from(*duration_seconds) * 1000)
                    .ok_or(DomainError::Silence)?;
                silence = Some(DeviceSilence {
                    device_id: actor_id,
                    attention_cycle: self.attention_cycle,
                    until,
                });
            }
        }
        Ok(Transition {
            incident,
            notification: None,
            silence,
        })
    }

    fn next(&self, now: UnixMillis) -> Result<Self, DomainError> {
        let mut next = self.clone();
        next.revision = increment(self.revision)?;
        next.updated_at = now.max(self.updated_at);
        Ok(next)
    }

    fn assess(
        &mut self,
        assessment: Assessment,
        clearance: Clearance,
        now: UnixMillis,
    ) -> Result<Option<Attention>, DomainError> {
        self.assessment = assessment;
        let requested = match assessment {
            Assessment::Unknown => return Ok(None),
            Assessment::NoVisibleConcern => {
                if clearance.supports(self.category)
                    && self.evidence_state == EvidenceState::Persisted
                {
                    if self.attention != Attention::Urgent && self.acknowledgement.is_none() {
                        self.resolution = Some(Resolution {
                            actor_id: None,
                            at: now.max(self.updated_at),
                            reason: "Corroborated current observations".into(),
                        });
                    }
                } else {
                    self.assessment = Assessment::Unknown;
                }
                return Ok(None);
            }
            Assessment::Concern => Attention::Review,
            Assessment::ImmediateVisibleConcern => Attention::Urgent,
        };
        if requested <= self.attention {
            return Ok(None);
        }
        self.attention = requested;
        self.attention_cycle = increment(self.attention_cycle)?;
        self.acknowledgement = None;
        Ok(self.live.then_some(requested))
    }
}

fn increment(number: u64) -> Result<u64, DomainError> {
    number.checked_add(1).ok_or(DomainError::RevisionExhausted)
}

fn validate_reason(reason: &str) -> Result<(), DomainError> {
    if reason.trim().is_empty()
        || reason.chars().count() > 512
        || reason.chars().any(char::is_control)
    {
        return Err(DomainError::Reason);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(live: bool) -> Transition {
        Incident::create(
            NewIncident {
                id: Uuid::new_v4(),
                scope: SourceScope {
                    camera_id: Uuid::new_v4(),
                    epoch: 1,
                    scene_revision: 1,
                },
                category: Category::Medical,
                reason: "observed posture change".into(),
                analysis_origin: None,
                first_source_us: 0,
                last_source_us: 50_000,
                live,
                assessment: Assessment::Concern,
            },
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn acknowledgement_and_silence_preserve_attention_and_escalation_starts_a_new_cycle() {
        let transition = review(true);
        assert_eq!(transition.notification, Some(Attention::Review));
        let operator = Uuid::new_v4();
        let acknowledged = transition
            .incident
            .apply(operator, &OperatorAction::Acknowledge, 2_000)
            .unwrap()
            .incident;
        let silenced = acknowledged
            .apply(
                operator,
                &OperatorAction::Silence {
                    duration_seconds: 30,
                },
                3_000,
            )
            .unwrap();
        assert!(silenced.incident.resolution.is_none());
        assert_eq!(silenced.incident.attention, Attention::Review);
        assert_eq!(silenced.silence.unwrap().until, 33_000);
        let escalated = silenced
            .incident
            .record_observation(
                acknowledged.scope,
                100_000,
                Assessment::ImmediateVisibleConcern,
                4_000,
            )
            .unwrap();
        assert_eq!(escalated.notification, Some(Attention::Urgent));
        assert_eq!(
            escalated.incident.attention_cycle,
            acknowledged.attention_cycle + 1
        );
        assert!(escalated.incident.acknowledgement.is_none());
        assert!(review(false).notification.is_none());
    }

    #[test]
    fn stale_evidence_and_unknown_assessments_do_not_clear_an_urgent_incident() {
        let initial = review(true).incident;
        let urgent = initial
            .record_observation(
                initial.scope,
                100_000,
                Assessment::ImmediateVisibleConcern,
                2_000,
            )
            .unwrap()
            .incident;
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 1,
            scope: urgent.scope,
            expires_at: 5_000,
        };
        let incident = urgent
            .attach_evidence(stamp, EvidenceState::Persisted, 3_000)
            .unwrap()
            .incident;
        assert_eq!(
            incident
                .record_semantic(
                    stamp,
                    Assessment::Unknown,
                    Clearance::Unsubstantiated,
                    5_000
                )
                .unwrap_err(),
            DomainError::Stale
        );
        let unknown = incident
            .record_semantic(
                stamp,
                Assessment::Unknown,
                Clearance::Unsubstantiated,
                3_001,
            )
            .unwrap()
            .incident;
        assert_eq!(
            unknown
                .record_semantic(
                    stamp,
                    Assessment::NoVisibleConcern,
                    Clearance::SameEpisodeRecovery,
                    3_002
                )
                .unwrap_err(),
            DomainError::Stale
        );
        let next = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 2,
            ..stamp
        };
        let fresh = unknown
            .attach_evidence(next, EvidenceState::Persisted, 3_003)
            .unwrap()
            .incident;
        let negative = fresh
            .record_semantic(
                next,
                Assessment::NoVisibleConcern,
                Clearance::SameEpisodeRecovery,
                3_004,
            )
            .unwrap()
            .incident;
        assert_eq!(negative.attention, Attention::Urgent);
        assert!(negative.resolution.is_none());
        let resolved = negative
            .apply(
                Uuid::new_v4(),
                &OperatorAction::Resolve {
                    reason: "Reviewed on site".into(),
                },
                4_000,
            )
            .unwrap();
        assert!(resolved.incident.resolution.is_some());
    }
    #[test]
    fn recovery_requires_current_medical_scope_and_complete_evidence() {
        let mut medical = review(true).incident;
        medical.category = Category::Medical;
        let now = medical.updated_at + 3_000;
        let position = medical.last_source_us + 3_000_000;
        assert!(
            medical
                .record_recovery(medical.scope, position, now)
                .is_err()
        );
        let stamp = EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: 1,
            scope: medical.scope,
            expires_at: now + 1_000,
        };
        let mut medical = medical
            .attach_evidence(stamp, EvidenceState::Persisted, now - 50)
            .unwrap()
            .incident;
        let moved = SourceScope {
            epoch: medical.scope.epoch + 1,
            ..medical.scope
        };
        assert!(medical.record_recovery(moved, position, now).is_err());
        let recovered = medical
            .record_recovery(medical.scope, position, now)
            .unwrap()
            .incident;
        assert!(recovered.resolution.is_some());
        assert_eq!(recovered.assessment, Assessment::NoVisibleConcern);
        medical.attention = Attention::Urgent;
        let recovered = medical
            .record_recovery(medical.scope, position, now)
            .unwrap()
            .incident;
        assert!(recovered.resolution.is_none());
        assert_eq!(recovered.attention, Attention::Urgent);
    }
}

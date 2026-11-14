use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use sw_domain::{
    AnalysisOrigin, Assessment, Box2d, Category, DomainError, EvidenceStamp, FireDecision,
    FireEpisode, FirePolicy, FireSample, FireSettings, HazardPolicy, MedicalCandidate,
    MedicalReason, MedicalRules, MedicalSample, MotionPhase, NewIncident, SourceScope, Visibility,
};
use sw_protocol::{
    Counter, Dimensions, EvidenceContext, EvidenceInterval, EvidenceLimitation, EvidenceVisibility,
    LocalFacts, worker::InferenceResult,
};
use sw_storage::{EvidenceWrite, Store, StoreError};
use uuid::Uuid;

use crate::{CameraFrame, evidence::EvidenceSampler, scene};

const MAX_PENDING: usize = 192;
const POLICY_VERSION: &str = "local-policy-2";
const PACKET_TTL_MS: i64 = 15_000;

pub(crate) struct HazardPolicies {
    scope: SourceScope,
    medical: MedicalRules,
    fire: FirePolicy,
    published: HashMap<Uuid, Published>,
    last_pose: Option<i64>,
}

pub(crate) struct Hazard {
    pub id: Uuid,
    pub assessment: Assessment,
    pub reason: &'static str,
    pub first_source_us: i64,
    pub source_us: i64,
    pub region: Box2d,
    pub facts: LocalFacts,
    pub limited: bool,
    pub update: bool,
}

struct Published {
    at: i64,
    facts: LocalFacts,
    updates: u8,
    recovery_queued: bool,
}

pub(crate) struct HazardUpdates {
    hazards: Vec<Hazard>,
    recoveries: Vec<Uuid>,
    pub fire_candidate: bool,
}

impl HazardPolicies {
    pub fn new(scope: SourceScope) -> Self {
        Self {
            scope,
            medical: MedicalRules::default(),
            fire: FirePolicy::new(FireSettings::default()).expect("validated default fire policy"),
            published: HashMap::new(),
            last_pose: None,
        }
    }

    pub fn update(
        &mut self,
        frame: &CameraFrame,
        at: i64,
        result: InferenceResult,
    ) -> HazardUpdates {
        if self.scope != frame.scope {
            *self = Self::new(frame.scope);
        }
        let mut hazards = Vec::new();
        let mut recoveries = Vec::new();
        let mut fire_candidate = false;
        if let Some(people) = result.pose {
            if self
                .last_pose
                .is_some_and(|last| at.saturating_sub(last) > 1_500_000)
            {
                // A long loss of coverage cannot bridge a fresh motion reference.
                self.medical.reset();
            }
            self.last_pose = Some(at);
            let people = if frame.visibility == Visibility::Poor {
                Vec::new()
            } else {
                people
            };
            if let Ok(candidates) = self.medical.assess(MedicalSample {
                observations: people,
                source_seconds: at as f64 / 1_000_000.0,
                source_epoch: frame.scope.epoch,
            }) {
                for candidate in candidates {
                    if let Some(hazard) = Hazard::medical(candidate, at) {
                        hazards.push(hazard);
                    }
                }
                if frame.visibility == Visibility::Good {
                    for motion in self.medical.snapshot() {
                        if motion.phase == MotionPhase::RecoveredActivity
                            && motion.clear_eligible
                            && motion.onset_observed
                            && !motion.identity_uncertain
                            && motion.recovery_basis.is_some()
                            && motion.last_seen_ts == at as f64 / 1_000_000.0
                            && let Some(published) = self.published.get_mut(&motion.episode_id)
                            && !published.recovery_queued
                        {
                            published.recovery_queued = true;
                            recoveries.push(motion.episode_id);
                        }
                    }
                }
            }
        }
        if let Some(regions) = result.fire {
            let header = frame.frame.header;
            let dimensions = Dimensions {
                width: header.width,
                height: header.height,
            };
            let reports = self.fire.assess(FireSample {
                scope: frame.scope,
                source_us: at,
                dimensions: (header.width, header.height),
                regions: &regions,
                visibility: frame.visibility,
                controlled_overlap: &|region| scene::overlap(region, dimensions, &frame.controlled),
            });
            for report in reports {
                fire_candidate |= report.decision == FireDecision::Observing
                    && report.facts.visibility != Visibility::Poor
                    && (report.facts.smoke_visible
                        || report
                            .facts
                            .controlled_region_overlap
                            .is_none_or(|overlap| overlap < 0.98));
                if report.decision == FireDecision::Review {
                    hazards.push(Hazard::fire(report));
                }
            }
        }
        // Bounded policies reuse an episode identity throughout its current evidence window.
        self.published.retain(|id, _| {
            self.fire.contains_episode(*id)
                || self
                    .medical
                    .snapshot()
                    .any(|episode| episode.episode_id == *id)
        });
        hazards.retain_mut(|hazard| match self.published.entry(hazard.id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Published {
                    at,
                    facts: hazard.facts,
                    updates: 0,
                    recovery_queued: false,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let previous = entry.get_mut();
                if previous.updates >= 2
                    || at.saturating_sub(previous.at) < 500_000
                    || !material_change(previous.facts, hazard.facts)
                {
                    false
                } else {
                    previous.at = at;
                    previous.facts = hazard.facts;
                    previous.updates += 1;
                    hazard.update = true;
                    true
                }
            }
        });
        HazardUpdates {
            hazards,
            recoveries,
            fire_candidate,
        }
    }
}

impl Hazard {
    fn medical(candidate: MedicalCandidate, at: i64) -> Option<Self> {
        let motion = candidate.motion?;
        if motion.phase == MotionPhase::OrdinaryActivity && motion.clear_eligible {
            return None;
        }
        let reason = match candidate.reason {
            MedicalReason::RapidPostureChange => "suspected_fall",
            MedicalReason::PartialBodyDescent => "partial_body_descent",
            MedicalReason::PersistentLowPosture if motion.onset_observed => {
                "persistent_low_posture_after_transition"
            }
            MedicalReason::PersistentLowPosture => "persistent_low_posture",
            MedicalReason::ViewLostAfterSuspicion => "visibility_lost_after_suspicion",
            MedicalReason::SlowPostureDescent => "slow_posture_descent",
            MedicalReason::SupportPatternLost => "support_pattern_lost",
            MedicalReason::PostureTransition => "posture_transition",
        };
        Some(Self {
            id: motion.episode_id,
            assessment: Assessment::Concern,
            reason,
            first_source_us: (candidate.first_ts * 1_000_000.0).round() as i64,
            source_us: at,
            region: candidate.region,
            limited: candidate.limited
                || motion.identity_uncertain
                || motion.phase == MotionPhase::VisibilityLost,
            update: false,
            facts: LocalFacts::Medical {
                track_id: Counter(candidate.track_id),
                episode_id: motion.episode_id,
                previous_episode_id: motion.previous_episode_id,
                onset_observed: motion.onset_observed,
                identity_uncertain: motion.identity_uncertain,
                recovery_seconds: motion.recovery_seconds,
                clear_eligible: motion.clear_eligible,
            },
        })
    }

    fn fire(episode: FireEpisode) -> Self {
        let facts = episode.facts;
        let reason = if facts.escaped_controlled_region {
            "flame_outside_reviewed_region"
        } else if facts.apparent_growth || facts.smoke_growth {
            "visible_fire_or_smoke_growth"
        } else if facts.smoke_visible && facts.flame_visible {
            "visible_flame_with_smoke"
        } else if facts.flame_visible {
            "visible_flame"
        } else {
            "visible_smoke"
        };
        Self {
            id: episode.id,
            assessment: Assessment::Concern,
            reason,
            first_source_us: episode.first_source_us,
            source_us: episode.source_us,
            region: episode.region,
            limited: facts.visibility != Visibility::Good,
            update: false,
            facts: LocalFacts::Fire {
                flame_visible: facts.flame_visible,
                smoke_visible: facts.smoke_visible,
                apparent_growth: facts.apparent_growth,
                smoke_growth: facts.smoke_growth,
                escaped_controlled_region: facts.escaped_controlled_region,
                controlled_region_overlap: facts.controlled_region_overlap,
                continuity_samples: facts.continuity_samples,
                detector_confidence: facts.detector_confidence,
                visibility: match facts.visibility {
                    Visibility::Good => EvidenceVisibility::Good,
                    Visibility::Limited => EvidenceVisibility::Limited,
                    Visibility::Poor => EvidenceVisibility::Poor,
                },
            },
        }
    }

    fn category(&self) -> Category {
        match self.facts {
            LocalFacts::Medical { .. } => Category::Medical,
            LocalFacts::Fire { .. } => Category::Fire,
        }
    }
}

fn material_change(previous: LocalFacts, current: LocalFacts) -> bool {
    match (previous, current) {
        (
            LocalFacts::Fire {
                apparent_growth: a,
                smoke_growth: b,
                escaped_controlled_region: c,
                smoke_visible: d,
                ..
            },
            LocalFacts::Fire {
                apparent_growth: e,
                smoke_growth: f,
                escaped_controlled_region: g,
                smoke_visible: h,
                ..
            },
        ) => (!a && e) || (!b && f) || (!c && g) || (!d && h),
        (
            LocalFacts::Medical {
                identity_uncertain: a,
                onset_observed: b,
                ..
            },
            LocalFacts::Medical {
                identity_uncertain: c,
                onset_observed: d,
                ..
            },
        ) => (!a && c) || (!b && d),
        _ => false,
    }
}

pub(crate) struct PendingHazards {
    queue: VecDeque<Batch>,
    store: Store,
    evidence: Arc<EvidenceSampler>,
    origin: AnalysisOrigin,
    retry_at: Option<tokio::time::Instant>,
}

struct Batch {
    frame: Arc<CameraFrame>,
    pending: VecDeque<Pending>,
}

enum Pending {
    Evidence(Hazard),
    Recovery(Uuid),
}

impl PendingHazards {
    pub fn new(store: Store, evidence: Arc<EvidenceSampler>, model_manifest_sha256: &str) -> Self {
        Self {
            queue: VecDeque::new(),
            store,
            evidence,
            origin: AnalysisOrigin {
                model_manifest_sha256: model_manifest_sha256.to_ascii_lowercase(),
                policy_version: POLICY_VERSION.into(),
            },
            retry_at: None,
        }
    }

    pub fn blocked(&self) -> bool {
        self.queue.len() >= 2
            || self
                .queue
                .iter()
                .map(|batch| batch.pending.len())
                .sum::<usize>()
                >= MAX_PENDING
    }

    pub fn push(&mut self, frame: Arc<CameraFrame>, updates: HazardUpdates) {
        let pending: VecDeque<_> = updates
            .hazards
            .into_iter()
            .map(Pending::Evidence)
            .chain(updates.recoveries.into_iter().map(Pending::Recovery))
            .collect();
        if !pending.is_empty() {
            self.queue.push_back(Batch { frame, pending });
        }
    }

    pub async fn flush(&mut self) {
        if self
            .retry_at
            .is_some_and(|time| time > tokio::time::Instant::now())
        {
            return;
        }
        // One commit per iteration keeps camera scheduling responsive.
        if let Some(batch) = self.queue.front_mut()
            && let Some(pending) = batch.pending.front()
        {
            let result = match pending {
                Pending::Evidence(hazard) => {
                    persist(
                        &self.store,
                        &self.evidence,
                        &batch.frame,
                        hazard,
                        &self.origin,
                    )
                    .await
                }
                Pending::Recovery(incident) => match batch.frame.frame.header.source_micros {
                    Some(source_us)
                        if batch.frame.captured_at.elapsed() <= Duration::from_secs(3) =>
                    {
                        self.store
                            .record_recovery(*incident, batch.frame.scope, source_us, utc_ms())
                            .await
                    }
                    _ => Ok(()),
                },
            };
            match result {
                Ok(())
                | Err(
                    StoreError::NotFound
                    | StoreError::Domain(DomainError::Resolved | DomainError::Stale),
                ) => {
                    self.retry_at = None;
                    batch.pending.pop_front();
                    if batch.pending.is_empty() {
                        self.queue.pop_front();
                    }
                }
                Err(_) => {
                    if self.retry_at.is_none() {
                        eprintln!("StillWatch could not commit a pending incident or its evidence");
                    }
                    self.retry_at = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                }
            }
        }
    }
}

async fn persist(
    store: &Store,
    evidence: &Arc<EvidenceSampler>,
    frame: &Arc<CameraFrame>,
    hazard: &Hazard,
    origin: &AnalysisOrigin,
) -> Result<(), StoreError> {
    let now = utc_ms();
    let new = NewIncident {
        id: hazard.id,
        scope: frame.scope,
        category: hazard.category(),
        reason: hazard.reason.into(),
        analysis_origin: Some(origin.clone()),
        first_source_us: hazard.first_source_us.min(hazard.source_us),
        last_source_us: hazard.source_us,
        live: frame.live,
        assessment: hazard.assessment,
    };
    let incident = if hazard.update {
        store.observe_hazard(new, now).await?
    } else {
        store.create_hazard(new, now).await?
    };
    if incident.resolution.is_some()
        || (hazard.update && incident.last_source_us != hazard.source_us)
        || (!hazard.update
            && (incident.evidence.is_some()
                || incident.evidence_state == sw_domain::EvidenceState::Unavailable))
    {
        return Ok(());
    }
    let evidence = evidence.clone();
    let onset = hazard.first_source_us;
    let region = hazard.region;
    let frame_copy = frame.clone();
    let selected = tokio::task::spawn_blocking(move || evidence.select(&frame_copy, onset, region))
        .await
        .map_err(|_| StoreError::Unavailable)?;
    if selected.images.is_empty() {
        return store.evidence_unavailable(incident.id, utc_ms()).await;
    }
    let coverage = selected.coverage;
    let mut limitations = Vec::new();
    if !frame.live {
        limitations.push(EvidenceLimitation::NonLive);
    }
    if hazard.limited {
        limitations.push(EvidenceLimitation::PartialVisibility);
    }
    if !selected.has_pre_event {
        limitations.push(EvidenceLimitation::PreEventUnavailable);
    }
    if coverage.max_gap_ms > 600
        || u32::from(coverage.received_samples) * 4 < u32::from(coverage.expected_samples) * 3
    {
        limitations.push(EvidenceLimitation::SparseCoverage);
    }
    let context = EvidenceContext {
        observed_interval: EvidenceInterval {
            start_source_s: hazard.first_source_us.min(hazard.source_us) as f64 / 1_000_000.0,
            end_source_s: hazard.source_us as f64 / 1_000_000.0,
        },
        coverage,
        local_facts: hazard.facts,
        limitations,
        policy_version: origin.policy_version.clone(),
        prompt_version: match hazard.category() {
            Category::Fire => "fire-evidence-1",
            Category::Medical => "medical-evidence-1",
            Category::Overview => return Err(StoreError::Invalid),
        }
        .into(),
    };
    let time = utc_ms();
    let packet = EvidenceWrite {
        incident_id: incident.id,
        expected_revision: incident.revision,
        stamp: EvidenceStamp {
            packet_id: Uuid::new_v4(),
            version: incident
                .evidence_version()
                .checked_add(1)
                .ok_or(StoreError::Invalid)?,
            scope: frame.scope,
            expires_at: time.saturating_add(PACKET_TTL_MS),
        },
        context: Some(context),
        images: selected.images,
    };
    store.persist_evidence(packet, time).await?;
    Ok(())
}

fn utc_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use sw_domain::{
    AnalysisOrigin, Assessment, Box2d, Category, DomainError, EvidenceStamp, EvidenceState,
    FireDecision, FireEpisode, FirePolicy, FireSample, FireSettings, HazardPolicy,
    MedicalCandidate, MedicalReason, MedicalRules, MedicalSample, MotionPhase, NewIncident,
    SourceScope, Visibility,
};
use sw_protocol::{
    Counter, Dimensions, EvidenceContext, EvidenceInterval, EvidenceLimitation, EvidenceVisibility,
    LocalFacts, worker::InferenceResult,
};
use sw_storage::{EvidenceOutcome, EvidenceWrite, Store, StoreError, utc_ms};
use uuid::Uuid;

use crate::{CameraFrame, evidence::EvidenceSampler, scene};

const MAX_PENDING: usize = 192;
const POLICY_VERSION: &str = "local-policy-3";
const PACKET_PERIOD: Duration = Duration::from_secs(4);
const PACKET_REFRESH: Duration = Duration::from_secs(10);
const PACKET_RETRY: Duration = Duration::from_secs(2);
const PACKET_BURST: u8 = 3;

pub(crate) struct HazardPolicies {
    scope: SourceScope,
    medical: MedicalRules,
    fire: FirePolicy,
    published: HashMap<Uuid, Published>,
    packets: PacketBudget,
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
    write: HazardWrite,
}

struct Published {
    observed_at: i64,
    facts: LocalFacts,
    packet: Option<(Instant, LocalFacts, Box2d)>,
    retry_packet: Option<Instant>,
    recovery_queued: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HazardWrite {
    Open { packet: bool },
    Observe,
    Packet,
}

#[derive(Default)]
struct PacketBudget {
    refill_at: Option<Instant>,
    tokens: u8,
}

impl PacketBudget {
    fn take(&mut self, now: Instant, urgent: bool) -> bool {
        let last = *self.refill_at.get_or_insert_with(|| {
            self.tokens = PACKET_BURST;
            now
        });
        let refills = now.saturating_duration_since(last).as_secs() / PACKET_PERIOD.as_secs();
        if refills > 0 {
            self.tokens = self
                .tokens
                .saturating_add(refills.min(u64::from(PACKET_BURST)) as u8)
                .min(PACKET_BURST);
            self.refill_at = Some(last + PACKET_PERIOD * refills.min(u64::from(u32::MAX)) as u32);
        }
        // Leave one token available for new incidents, flames, escape or recovery.
        if self.tokens <= u8::from(!urgent) {
            return false;
        }
        self.tokens -= 1;
        true
    }
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
            packets: PacketBudget::default(),
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
            let mut new = Self::new(frame.scope);
            new.packets = std::mem::take(&mut self.packets);
            *self = new;
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
        let now = Instant::now();
        let capture = frame.captured_at;
        hazards.retain_mut(|hazard| match self.published.entry(hazard.id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let packet = self.packets.take(now, true);
                entry.insert(Published {
                    observed_at: at,
                    facts: hazard.facts,
                    packet: packet.then_some((capture, hazard.facts, hazard.region)),
                    retry_packet: None,
                    recovery_queued: false,
                });
                hazard.write = HazardWrite::Open { packet };
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .update(hazard, &mut self.packets, now, capture)
            }
        });
        HazardUpdates {
            hazards,
            recoveries,
            fire_candidate,
        }
    }

    pub fn packet_committed(&mut self, scope: SourceScope, id: Uuid, result: PacketResult) {
        if self.scope != scope {
            return;
        }
        if let Some(state) = self.published.get_mut(&id) {
            let at = match result {
                PacketResult::Written { at, .. } | PacketResult::Unavailable { at } => at,
            };
            if state.packet.is_some_and(|(queued, _, _)| queued > at) {
                return;
            }
            match result {
                PacketResult::Written { at, facts, region } => {
                    state.packet = Some((at, facts, region));
                    state.retry_packet = None;
                }
                PacketResult::Unavailable { .. } => {
                    state.packet = None;
                    state.retry_packet = Some(Instant::now() + PACKET_RETRY);
                }
            }
        }
    }
}

impl Published {
    fn update(
        &mut self,
        hazard: &mut Hazard,
        budget: &mut PacketBudget,
        now: Instant,
        capture: Instant,
    ) -> bool {
        let at = hazard.source_us;
        let material = material_change(self.facts, hazard.facts);
        let (important, refresh) = match self.packet {
            Some((previous, facts, region)) => (
                material_change(facts, hazard.facts) || region.iou(hazard.region) < 0.35,
                now.saturating_duration_since(previous) >= PACKET_REFRESH,
            ),
            None => (material, true),
        };
        if (important || self.retry_packet.is_none_or(|ready| now >= ready))
            && (important || refresh)
            && budget.take(now, important)
        {
            self.packet = Some((capture, hazard.facts, hazard.region));
            self.observed_at = at;
            self.facts = hazard.facts;
            hazard.write = HazardWrite::Packet;
            return true;
        }
        if material && at.saturating_sub(self.observed_at) >= 500_000 {
            self.observed_at = at;
            self.facts = hazard.facts;
            hazard.write = HazardWrite::Observe;
            return true;
        }
        false
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
            write: HazardWrite::Open { packet: false },
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
            write: HazardWrite::Open { packet: false },
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
                flame_visible: i,
                visibility: vis_old,
                ..
            },
            LocalFacts::Fire {
                apparent_growth: e,
                smoke_growth: f,
                escaped_controlled_region: g,
                smoke_visible: h,
                flame_visible: j,
                visibility: vis_new,
                ..
            },
        ) => {
            (!a && e)
                || (!b && f)
                || (!c && g)
                || (!d && h)
                || (!i && j)
                || (vis_old != EvidenceVisibility::Good && vis_new == EvidenceVisibility::Good)
        }
        (
            LocalFacts::Medical {
                identity_uncertain: a,
                onset_observed: b,
                clear_eligible: e,
                ..
            },
            LocalFacts::Medical {
                identity_uncertain: c,
                onset_observed: d,
                clear_eligible: f,
                ..
            },
        ) => (a != c) || (!b && d) || (e != f),
        _ => false,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PacketResult {
    Written {
        at: Instant,
        facts: LocalFacts,
        region: Box2d,
    },
    Unavailable {
        at: Instant,
    },
}

pub(crate) struct PacketCommit {
    pub scope: SourceScope,
    pub incident: Uuid,
    pub result: PacketResult,
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
        // Only a more recent packet supersedes queued image work. Keep the
        // earlier opening and material observations in the durable history.
        for update in updates
            .hazards
            .iter()
            .filter(|hazard| hazard.write == HazardWrite::Packet)
        {
            for batch in self
                .queue
                .iter_mut()
                .filter(|batch| batch.frame.scope == frame.scope)
            {
                for pending in &mut batch.pending {
                    if let Pending::Evidence(hazard) = pending
                        && hazard.id == update.id
                    {
                        hazard.write = match hazard.write {
                            HazardWrite::Open { .. } => HazardWrite::Open { packet: false },
                            HazardWrite::Packet | HazardWrite::Observe => HazardWrite::Observe,
                        };
                    }
                }
            }
        }
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

    pub async fn flush(&mut self) -> Option<PacketCommit> {
        if self
            .retry_at
            .is_some_and(|time| time > tokio::time::Instant::now())
        {
            return None;
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
                            .record_fresh_recovery(
                                *incident,
                                batch.frame.scope,
                                source_us,
                                batch.frame.observation_time,
                            )
                            .await
                            .map(|_| None)
                    }
                    _ => Ok(None),
                },
            };
            match result {
                outcome @ (Ok(_)
                | Err(
                    StoreError::NotFound
                    | StoreError::Domain(DomainError::Resolved | DomainError::Stale),
                )) => {
                    self.retry_at = None;
                    batch.pending.pop_front();
                    if batch.pending.is_empty() {
                        self.queue.pop_front();
                    }
                    return outcome.ok().flatten();
                }
                Err(_) => {
                    if self.retry_at.is_none() {
                        eprintln!("StillWatch could not commit a pending incident or its evidence");
                    }
                    self.retry_at = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                }
            }
        }
        None
    }
}

async fn persist(
    store: &Store,
    evidence: &Arc<EvidenceSampler>,
    frame: &Arc<CameraFrame>,
    hazard: &Hazard,
    origin: &AnalysisOrigin,
) -> Result<Option<PacketCommit>, StoreError> {
    let new = NewIncident {
        id: hazard.id,
        scope: frame.scope,
        category: hazard.category(),
        reason: hazard.reason.into(),
        analysis_origin: Some(origin.clone()),
        first_source_us: hazard.first_source_us.min(hazard.source_us),
        last_source_us: hazard.source_us,
        live: frame.live,
        observed_host_utc: Some(frame.observation_time.observed_host_utc),
        assessment: hazard.assessment,
    };
    let opening = matches!(hazard.write, HazardWrite::Open { .. });
    let incident = store
        .record_hazard_observation(
            new,
            frame.observation_time,
            hazard.facts,
            hazard.region,
            opening,
        )
        .await?;
    if incident.resolution.is_some()
        || incident.last_source_us != hazard.source_us
        || (opening
            && (incident.evidence.is_some()
                || incident.evidence_state == EvidenceState::Unavailable))
    {
        return Ok(None);
    }
    if matches!(
        hazard.write,
        HazardWrite::Observe | HazardWrite::Open { packet: false }
    ) {
        return Ok(None);
    }
    let completed = |result| {
        Some(PacketCommit {
            scope: frame.scope,
            incident: incident.id,
            result,
        })
    };
    let evidence = evidence.clone();
    let onset = hazard.first_source_us;
    let region = hazard.region;
    let frame_copy = frame.clone();
    let selected = tokio::task::spawn_blocking(move || evidence.select(&frame_copy, onset, region))
        .await
        .map_err(|_| StoreError::Unavailable)?;
    if selected.images.is_empty() {
        if incident.evidence_state == EvidenceState::Pending {
            store.evidence_unavailable(incident.id, utc_ms()).await?;
        }
        return Ok(completed(PacketResult::Unavailable {
            at: frame.captured_at,
        }));
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
            expires_at: frame.observation_time.expires_at(),
        },
        observation_time: Some(frame.observation_time),
        context: Some(context),
        images: selected.images,
    };
    let outcome = store.persist_evidence(packet, time).await?;
    let result = if matches!(outcome, EvidenceOutcome::Persisted(_))
        && frame
            .observation_time
            .evidence_fresh(utc_ms(), Instant::now())
    {
        PacketResult::Written {
            at: frame.captured_at,
            facts: hazard.facts,
            region: hazard.region,
        }
    } else {
        PacketResult::Unavailable {
            at: frame.captured_at,
        }
    };
    Ok(completed(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fire(flags: u8, visibility: EvidenceVisibility) -> Hazard {
        Hazard {
            id: Uuid::nil(),
            assessment: Assessment::Concern,
            reason: "visible_smoke",
            first_source_us: 0,
            source_us: 0,
            region: Box2d {
                x1: 0.0,
                y1: 0.0,
                x2: 100.0,
                y2: 100.0,
            },
            facts: LocalFacts::Fire {
                flame_visible: flags & 1 != 0,
                smoke_visible: true,
                apparent_growth: flags & 2 != 0,
                smoke_growth: flags & 4 != 0,
                escaped_controlled_region: flags & 8 != 0,
                controlled_region_overlap: None,
                continuity_samples: 2,
                detector_confidence: 0.6,
                visibility,
            },
            limited: visibility != EvidenceVisibility::Good,
            write: HazardWrite::Open { packet: false },
        }
    }

    #[test]
    fn material_changes_continue_after_two_updates_and_replenish_packet_budget() {
        let initial = fire(0, EvidenceVisibility::Limited);
        let start = Instant::now();
        let mut budget = PacketBudget::default();
        assert!(budget.take(start, true));
        let mut published = Published {
            observed_at: 0,
            facts: initial.facts,
            packet: Some((start, initial.facts, initial.region)),
            retry_packet: None,
            recovery_queued: false,
        };
        for (index, (flags, visibility)) in [
            (4, EvidenceVisibility::Limited),
            (5, EvidenceVisibility::Limited),
            (7, EvidenceVisibility::Limited),
            (15, EvidenceVisibility::Limited),
            (15, EvidenceVisibility::Good),
        ]
        .into_iter()
        .enumerate()
        {
            let at = start + PACKET_PERIOD * (index as u32 + 1);
            let mut hazard = fire(flags, visibility);
            hazard.source_us = 4_000_000 * (index as i64 + 1);
            assert!(published.update(&mut hazard, &mut budget, at, at));
            assert_eq!(hazard.write, HazardWrite::Packet);
        }
    }

    #[test]
    fn unavailable_packet_retries_and_unresolved_episode_refreshes() {
        let start = Instant::now();
        let mut hazard = fire(0, EvidenceVisibility::Good);
        let mut budget = PacketBudget::default();
        let mut published = Published {
            observed_at: 0,
            facts: hazard.facts,
            packet: None,
            retry_packet: Some(start + PACKET_RETRY),
            recovery_queued: false,
        };
        assert!(!published.update(&mut hazard, &mut budget, start, start));
        let retry = start + PACKET_RETRY;
        hazard.source_us = 2_000_000;
        assert!(published.update(&mut hazard, &mut budget, retry, retry));
        assert_eq!(hazard.write, HazardWrite::Packet);
        let later = retry + PACKET_REFRESH;
        hazard.source_us = 12_000_000;
        assert!(published.update(&mut hazard, &mut budget, later, later));
        assert_eq!(hazard.write, HazardWrite::Packet);
    }

    #[test]
    fn flickering_updates_use_observation_path_when_image_tokens_are_spent() {
        let start = Instant::now();
        let initial = fire(0, EvidenceVisibility::Good);
        let mut budget = PacketBudget::default();
        for _ in 0..PACKET_BURST {
            assert!(budget.take(start, true));
        }
        let mut published = Published {
            observed_at: 0,
            facts: initial.facts,
            packet: Some((start, initial.facts, initial.region)),
            retry_packet: None,
            recovery_queued: false,
        };
        let flicker = start + Duration::from_secs(1);
        let mut hazard = fire(1, EvidenceVisibility::Good);
        hazard.source_us = 1_000_000;
        assert!(published.update(&mut hazard, &mut budget, flicker, flicker));
        assert_eq!(hazard.write, HazardWrite::Observe);
        let again = start + PACKET_PERIOD;
        hazard.source_us = 4_000_000;
        assert!(published.update(&mut hazard, &mut budget, again, again));
        assert_eq!(hazard.write, HazardWrite::Packet);
        assert!(!budget.take(again, true));
    }
}

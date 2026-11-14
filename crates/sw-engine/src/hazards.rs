use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use sw_domain::{
    AnalysisOrigin, Assessment, Box2d, Category, DomainError, EvidenceStamp, EvidenceState,
    FireAssociation, FireDecision, FireEpisode, FirePolicy, FireSample, FireSettings, HazardPolicy,
    MedicalCandidate, MedicalReason, MedicalRules, MedicalSample, MotionPhase, NewIncident,
    ObservationLabel, RecoveryBasis, RecoveryCourse, SourceScope, TransitionKind, Visibility,
};
use sw_protocol::{
    Counter, Dimensions, EpisodeAssociation, EvidenceContext, EvidenceInterval, EvidenceLimitation,
    EvidenceLineage, EvidenceVisibility, LocalFacts, worker::InferenceResult,
};
use sw_storage::{
    EvidenceOutcome, EvidenceWrite, RecoveryIntent, RecoveryOutcome, Store, StoreError, utc_ms,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{CameraFrame, evidence::EvidenceSampler, scene};

const MAX_PENDING: usize = 192;
const POLICY_VERSION: &str = "local-policy-4";
const PACKET_PERIOD: Duration = Duration::from_secs(4);
const PACKET_REFRESH: Duration = Duration::from_secs(10);
const PACKET_RETRY: Duration = Duration::from_secs(2);
const PACKET_BURST: u8 = 3;
const MEDICAL_REOBSERVATION_US: i64 = 30_000_000;
const MAX_PUBLISHED: usize = 256;

pub(crate) struct HazardPolicies {
    scope: SourceScope,
    medical: MedicalRules,
    fire: FirePolicy,
    published: HashMap<Uuid, Published>,
    packets: PacketBudget,
    last_pose: Option<i64>,
    resolution_check_at: Option<Instant>,
    resolutions: Option<oneshot::Receiver<Vec<Uuid>>>,
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
    segment_start_us: i64,
    lineage: EvidenceLineage,
    recovery: Option<RecoveryIntent>,
    directly_visible: bool,
    write: HazardWrite,
}

struct Published {
    observed_at: i64,
    first_source_us: i64,
    segment_start_us: i64,
    facts: LocalFacts,
    packet: Option<(Instant, LocalFacts, Box2d)>,
    retry_packet: Option<Instant>,
    recovery: RecoveryProgress,
    resolved: bool,
    episode_id: Uuid,
    region: Box2d,
    last_visible_us: i64,
    association: EpisodeAssociation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RecoveryProgress {
    #[default]
    Available,
    Pending {
        source_us: i64,
        course: RecoveryCourse,
    },
    Completed,
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
    recovery_courses: Option<Vec<(Uuid, RecoveryCourse)>>,
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
            resolution_check_at: None,
            resolutions: None,
        }
    }

    pub fn update(
        &mut self,
        frame: &CameraFrame,
        at: i64,
        result: InferenceResult,
        fire_proposals: &[sw_domain::Observation],
    ) -> HazardUpdates {
        if self.scope != frame.scope {
            let mut new = Self::new(frame.scope);
            new.packets = std::mem::take(&mut self.packets);
            *self = new;
        }
        let mut hazards = Vec::new();
        let mut fire_candidate = false;
        let view = scene::VisibleFrame::from(frame);
        let pose_executed = result.pose.is_some();
        if let Some(mut people) = result.pose {
            if self
                .last_pose
                .is_some_and(|last| at.saturating_sub(last) > 1_500_000)
            {
                // A long loss of coverage cannot bridge a fresh motion reference.
                self.medical.reset();
            }
            self.last_pose = Some(at);
            people.retain(|person| view.region(person.region, false) != Visibility::Poor);
            if let Ok(candidates) = self.medical.assess(MedicalSample {
                observations: people,
                source_seconds: at as f64 / 1_000_000.0,
                source_epoch: frame.scope.epoch,
            }) {
                for candidate in candidates {
                    if let Some(mut hazard) = Hazard::medical(candidate, at) {
                        hazard.limited |= view.region(hazard.region, false) != Visibility::Good;
                        hazards.push(hazard);
                    }
                }
                let ts = at as f64 / 1_000_000.0;
                for (motion, region) in self.medical.visible_motion(ts) {
                    if let Some(published) = self
                        .published
                        .values_mut()
                        .find(|published| published.episode_id == motion.episode_id)
                    {
                        published.last_visible_us = at;
                        published.region = region;
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
            let overlap = |region| scene::overlap(region, dimensions, &frame.controlled);
            fire_candidate = fire_proposals.iter().any(|weak| {
                view.region(weak.region, weak.label == ObservationLabel::Fire) != Visibility::Poor
                    && (weak.label == ObservationLabel::Smoke
                        || overlap(weak.region).is_none_or(|value| value < 0.98))
            });
            let reports = self.fire.assess(FireSample {
                scope: frame.scope,
                source_us: at,
                dimensions: (header.width, header.height),
                regions: &regions,
                weak_proposals: fire_proposals,
                visibility: frame.visibility,
                region_visibility: &|observation| {
                    view.region(
                        observation.region,
                        observation.label == ObservationLabel::Fire,
                    )
                },
                controlled_overlap: &overlap,
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
        self.published.retain(|_, state| {
            self.fire.contains_episode(state.episode_id)
                || matches!(state.facts, LocalFacts::Medical { .. })
                    && at.saturating_sub(state.last_visible_us) <= MEDICAL_REOBSERVATION_US
        });
        if !hazards.is_empty() {
            self.associate_incidents(&mut hazards, at);
            // A split contour can yield two reports for one episode at the same PTS.
            // Keep one commit and cover both regions with its overview and crop.
            coalesce_observations(&mut hazards);
        }
        let now = Instant::now();
        let capture = frame.captured_at;
        hazards.retain_mut(|hazard| match self.published.entry(hazard.id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let packet = self.packets.take(now, true);
                entry.insert(Published::new(hazard, capture, packet));
                hazard.write = HazardWrite::Open { packet };
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .update(hazard, &mut self.packets, now, capture)
            }
        });
        if self.published.len() > MAX_PUBLISHED {
            let mut oldest = self
                .published
                .iter()
                .map(|(&id, state)| (state.last_visible_us, id))
                .collect::<Vec<_>>();
            oldest.sort_unstable();
            let remove = self.published.len() - MAX_PUBLISHED;
            for (_, id) in oldest.into_iter().take(remove) {
                self.published.remove(&id);
            }
        }
        let mut recovery_courses = pose_executed.then(Vec::new);
        if frame.visibility == Visibility::Good
            && let Some(courses) = recovery_courses.as_mut()
        {
            let ts = at as f64 / 1_000_000.0;
            for (motion, region) in self.medical.visible_motion(ts) {
                if view.region(region, false) != Visibility::Good {
                    continue;
                }
                let Some(course) = recovery_course(motion) else {
                    continue;
                };
                courses.push((motion.episode_id, course));
                let Some((&incident_id, published)) = self
                    .published
                    .iter_mut()
                    .find(|(_, state)| state.episode_id == motion.episode_id)
                else {
                    continue;
                };
                if published.resolved
                    || hazards.iter().any(|hazard| {
                        hazard.id == incident_id && matches!(hazard.write, HazardWrite::Open { .. })
                    })
                    || published.association == EpisodeAssociation::Uncertain
                    || published.recovery != RecoveryProgress::Available
                    || published.retry_packet.is_some_and(|retry| now < retry)
                    || !self.packets.take(now, true)
                {
                    continue;
                }
                published.recovery = RecoveryProgress::Pending {
                    source_us: at,
                    course,
                };
                published.packet = Some((capture, medical_facts(motion, true), region));
                hazards.retain(|hazard| hazard.id != incident_id);
                let intent = RecoveryIntent {
                    incident_id,
                    episode_id: motion.episode_id,
                    scope: frame.scope,
                    course,
                    source_us: at,
                };
                hazards.push(Hazard::recovery(motion, region, intent));
            }
        }
        if let Some(courses) = &recovery_courses {
            for published in self.published.values_mut() {
                if let RecoveryProgress::Pending { course, .. } = published.recovery
                    && !courses.contains(&(published.episode_id, course))
                {
                    published.recovery = RecoveryProgress::Available;
                }
            }
        }
        HazardUpdates {
            hazards,
            recovery_courses,
            fire_candidate,
        }
    }

    fn associate_incidents(&self, hazards: &mut [Hazard], at: i64) {
        let matches = hazards
            .iter()
            .map(|hazard| {
                self.published
                    .iter()
                    .find(|(_, state)| state.episode_id == hazard.lineage.episode_id)
                    .map(|(&id, _)| (id, true))
                    .or_else(|| self.medical_continuation(hazard, at).map(|id| (id, false)))
            })
            .collect::<Vec<_>>();
        let mut starting = HashMap::new();
        for (index, hazard) in hazards.iter_mut().enumerate() {
            if let Some((id, tracked)) = matches[index]
                && (tracked
                    || matches
                        .iter()
                        .filter(|matched| matched.is_some_and(|(other, _)| other == id))
                        .count()
                        == 1)
            {
                let state = &self.published[&id];
                hazard.id = id;
                hazard.first_source_us = state.first_source_us;
                if matches!(hazard.facts, LocalFacts::Medical { .. })
                    && (!tracked || state.association == EpisodeAssociation::Uncertain)
                {
                    if tracked {
                        hazard.segment_start_us = state.segment_start_us;
                    }
                    hazard.lineage.association = EpisodeAssociation::Uncertain;
                    if let LocalFacts::Medical {
                        ref mut identity_uncertain,
                        ..
                    } = hazard.facts
                    {
                        *identity_uncertain = true;
                    }
                    hazard.limited = true;
                }
            } else {
                hazard.id = *starting
                    .entry(hazard.lineage.episode_id)
                    .or_insert_with(Uuid::new_v4);
            }
        }
    }

    fn medical_continuation(&self, hazard: &Hazard, at: i64) -> Option<Uuid> {
        let LocalFacts::Medical {
            onset_observed,
            identity_uncertain,
            ..
        } = hazard.facts
        else {
            return None;
        };
        // An independently observed onset keeps its own incident. Geometry
        // alone can link an already-low observation only with uncertainty.
        if onset_observed || identity_uncertain || hazard.reason != "persistent_low_posture" {
            return None;
        }
        let mut candidates = self.published.iter().filter(|(_, state)| {
            matches!(state.facts, LocalFacts::Medical { .. })
                && !state.resolved
                && at.saturating_sub(state.last_visible_us) > 1_500_000
                && state.region.iou(hazard.region) >= 0.35
        });
        let (&id, _) = candidates.next()?;
        candidates.next().is_none().then_some(id)
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

    pub fn record_resolution(&mut self, scope: SourceScope, id: Uuid) {
        if self.scope == scope
            && let Some(published) = self.published.get_mut(&id)
        {
            self.fire.record_resolution(published.episode_id);
            published.resolved = true;
        }
    }

    pub fn refresh_resolutions(&mut self, store: &Store, scope: SourceScope, now: Instant) {
        if let Some(pending) = self.resolutions.as_mut() {
            match pending.try_recv() {
                Ok(resolved) => {
                    self.resolutions = None;
                    for id in resolved {
                        self.record_resolution(scope, id);
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => self.resolutions = None,
            }
        }
        if self.scope != scope || self.resolution_check_at.is_some_and(|next| now < next) {
            return;
        }
        self.resolution_check_at = Some(now + PACKET_RETRY);
        let unresolved = self
            .published
            .iter()
            .filter_map(|(&id, state)| {
                (!state.resolved && matches!(state.facts, LocalFacts::Fire { .. })).then_some(id)
            })
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            let (mut send, receive) = oneshot::channel();
            self.resolutions = Some(receive);
            let store = store.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = send.closed() => {},
                    result = store.resolved_hazards(unresolved) => {
                        if let Ok(resolved) = result {
                            let _ = send.send(resolved);
                        }
                    }
                }
            });
        }
    }

    pub fn recovery_committed(&mut self, intent: RecoveryIntent, outcome: RecoveryOutcome) {
        if self.scope != intent.scope {
            return;
        }
        if let Some(published) = self.published.get_mut(&intent.incident_id)
            && published.recovery
                == (RecoveryProgress::Pending {
                    source_us: intent.source_us,
                    course: intent.course,
                })
        {
            published.recovery = match outcome {
                RecoveryOutcome::Applied | RecoveryOutcome::Inapplicable => {
                    RecoveryProgress::Completed
                }
                RecoveryOutcome::NeedsEvidence | RecoveryOutcome::Superseded => {
                    published.retry_packet = Some(Instant::now() + PACKET_RETRY);
                    RecoveryProgress::Available
                }
            };
            published.resolved |= outcome == RecoveryOutcome::Applied;
        }
    }
}

fn coalesce_observations(hazards: &mut Vec<Hazard>) {
    let mut seen = HashMap::<Uuid, usize>::new();
    let mut unique = Vec::<Hazard>::with_capacity(hazards.len());
    for hazard in hazards.drain(..) {
        if let Some(&index) = seen.get(&hazard.id) {
            let previous = &mut unique[index];
            previous.region = previous.region.union(hazard.region);
            if hazard.lineage.association == EpisodeAssociation::Split {
                previous.lineage.association = EpisodeAssociation::Split;
            }
        } else {
            seen.insert(hazard.id, unique.len());
            unique.push(hazard);
        }
    }
    *hazards = unique;
}

fn recovery_course(motion: &sw_domain::MotionEvidence) -> Option<RecoveryCourse> {
    if !motion.clear_eligible || !motion.onset_observed || motion.identity_uncertain {
        return None;
    }
    match (motion.phase, motion.recovery_basis) {
        (MotionPhase::RecoveredActivity, Some(RecoveryBasis::UprightActivity)) => {
            Some(RecoveryCourse::UprightActivity)
        }
        (MotionPhase::RecoveredActivity, Some(RecoveryBasis::PriorSupportedPosture)) => {
            Some(RecoveryCourse::PriorSupportedPosture)
        }
        (MotionPhase::OrdinaryActivity, _)
            if motion.transition == TransitionKind::ControlledPattern =>
        {
            Some(RecoveryCourse::ControlledActivity)
        }
        _ => None,
    }
}

impl Published {
    fn new(hazard: &Hazard, capture: Instant, packet: bool) -> Self {
        Self {
            observed_at: hazard.source_us,
            first_source_us: hazard.first_source_us,
            segment_start_us: hazard.segment_start_us,
            facts: hazard.facts,
            packet: packet.then_some((capture, hazard.facts, hazard.region)),
            retry_packet: None,
            recovery: RecoveryProgress::Available,
            resolved: false,
            episode_id: hazard.lineage.episode_id,
            region: hazard.region,
            last_visible_us: hazard.source_us,
            association: hazard.lineage.association,
        }
    }

    fn update(
        &mut self,
        hazard: &mut Hazard,
        budget: &mut PacketBudget,
        now: Instant,
        capture: Instant,
    ) -> bool {
        if self.resolved {
            return false;
        }
        let at = hazard.source_us;
        if hazard.directly_visible {
            self.region = hazard.region;
            self.last_visible_us = at;
        }
        let new_segment = hazard.segment_start_us > self.segment_start_us
            && match hazard.facts {
                LocalFacts::Fire { .. } => true,
                LocalFacts::Medical { .. } => self.episode_id != hazard.lineage.episode_id,
            };
        self.segment_start_us = hazard.segment_start_us;
        self.episode_id = hazard.lineage.episode_id;
        self.association = hazard.lineage.association;
        let material = new_segment || material_change(self.facts, hazard.facts);
        let (important, refresh) = match self.packet {
            Some((previous, facts, region)) => (
                new_segment
                    || material_change(facts, hazard.facts)
                    || region.iou(hazard.region) < 0.35,
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
            id: Uuid::nil(),
            assessment: Assessment::Concern,
            reason,
            first_source_us: (candidate.first_ts * 1_000_000.0).round() as i64,
            source_us: at,
            segment_start_us: (candidate.first_ts * 1_000_000.0).round() as i64,
            region: candidate.region,
            limited: candidate.limited
                || motion.identity_uncertain
                || motion.phase == MotionPhase::VisibilityLost,
            directly_visible: motion.phase != MotionPhase::VisibilityLost,
            write: HazardWrite::Open { packet: false },
            recovery: None,
            facts: medical_facts(&motion, false),
            lineage: EvidenceLineage {
                episode_id: motion.episode_id,
                fire_track_id: None,
                association: if motion.identity_uncertain {
                    EpisodeAssociation::Uncertain
                } else {
                    EpisodeAssociation::Continuous
                },
            },
        })
    }

    fn recovery(motion: &sw_domain::MotionEvidence, region: Box2d, intent: RecoveryIntent) -> Self {
        Self {
            id: intent.incident_id,
            assessment: Assessment::Concern,
            reason: intent.course.resolution_reason(),
            first_source_us: (motion.started_ts * 1_000_000.0).round() as i64,
            segment_start_us: (motion.started_ts * 1_000_000.0).round() as i64,
            source_us: intent.source_us,
            region,
            facts: medical_facts(motion, true),
            limited: false,
            directly_visible: true,
            lineage: EvidenceLineage {
                episode_id: motion.episode_id,
                fire_track_id: None,
                association: EpisodeAssociation::Continuous,
            },
            recovery: Some(intent),
            write: HazardWrite::Packet,
        }
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
            id: Uuid::nil(),
            assessment: Assessment::Concern,
            reason,
            first_source_us: episode.first_source_us,
            segment_start_us: episode.segment_start_source_us,
            source_us: episode.source_us,
            region: episode.region,
            limited: facts.visibility != Visibility::Good,
            directly_visible: true,
            write: HazardWrite::Open { packet: false },
            recovery: None,
            lineage: EvidenceLineage {
                episode_id: episode.id,
                fire_track_id: Some(episode.track_id),
                association: match episode.association {
                    FireAssociation::New => EpisodeAssociation::New,
                    FireAssociation::Continuous => EpisodeAssociation::Continuous,
                    FireAssociation::Reobserved => EpisodeAssociation::Reobserved,
                    FireAssociation::Split => EpisodeAssociation::Split,
                    FireAssociation::Ambiguous => EpisodeAssociation::Uncertain,
                },
            },
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

fn medical_facts(motion: &sw_domain::MotionEvidence, clear_eligible: bool) -> LocalFacts {
    LocalFacts::Medical {
        track_id: Counter(motion.track_id),
        episode_id: motion.episode_id,
        previous_episode_id: motion.previous_episode_id,
        onset_observed: motion.onset_observed,
        identity_uncertain: motion.identity_uncertain,
        recovery_seconds: motion.recovery_seconds,
        clear_eligible,
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

pub(crate) struct HazardCommit {
    pub scope: SourceScope,
    pub incident: Uuid,
    pub packet: Option<PacketResult>,
    pub recovery: Option<(RecoveryIntent, RecoveryOutcome)>,
    pub resolved: bool,
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
    pending: VecDeque<Hazard>,
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
        self.queue.retain_mut(|batch| {
            if batch.frame.scope.camera_id != frame.scope.camera_id {
                return true;
            }
            if batch.frame.scope != frame.scope {
                batch.pending.retain(|hazard| hazard.recovery.is_none());
                return !batch.pending.is_empty();
            }
            if let Some(courses) = &updates.recovery_courses {
                batch.pending.retain(|hazard| {
                    hazard
                        .recovery
                        .is_none_or(|intent| courses.contains(&(intent.episode_id, intent.course)))
                });
            }
            !batch.pending.is_empty()
        });
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
                    if pending.id == update.id && pending.recovery.is_none() {
                        pending.write = match pending.write {
                            HazardWrite::Open { .. } => HazardWrite::Open { packet: false },
                            HazardWrite::Packet | HazardWrite::Observe => HazardWrite::Observe,
                        };
                    }
                }
            }
        }
        let pending = VecDeque::from(updates.hazards);
        if !pending.is_empty() {
            self.queue.push_back(Batch { frame, pending });
        }
    }

    pub async fn flush(&mut self) -> Option<HazardCommit> {
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
            let result = persist(
                &self.store,
                &self.evidence,
                &batch.frame,
                pending,
                &self.origin,
            )
            .await;
            match result {
                outcome @ (Ok(_)
                | Err(
                    StoreError::NotFound
                    | StoreError::Domain(DomainError::Resolved | DomainError::Stale),
                )) => {
                    let commit = match outcome {
                        Ok(commit) => commit,
                        Err(error) => pending.recovery.map(|intent| HazardCommit {
                            scope: intent.scope,
                            incident: intent.incident_id,
                            packet: None,
                            recovery: Some((intent, RecoveryOutcome::Superseded)),
                            resolved: matches!(error, StoreError::Domain(DomainError::Resolved)),
                        }),
                    };
                    self.retry_at = None;
                    batch.pending.pop_front();
                    if batch.pending.is_empty() {
                        self.queue.pop_front();
                    }
                    return commit;
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
) -> Result<Option<HazardCommit>, StoreError> {
    if let Some(intent) = hazard.recovery
        && !frame
            .observation_time
            .fresh(utc_ms(), Instant::now(), Duration::from_secs(3))
    {
        // A prior attempt may have reached its transaction before the
        // caller deadline. Ask storage for the idempotent acknowledgement.
        let outcome = store.apply_recovery(intent, frame.observation_time).await?;
        return Ok(Some(HazardCommit {
            scope: frame.scope,
            incident: hazard.id,
            packet: None,
            recovery: Some((intent, outcome)),
            resolved: outcome == RecoveryOutcome::Applied,
        }));
    }
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
            (hazard.directly_visible && frame.visibility == Visibility::Good)
                .then_some(frame.scene_signature),
            opening,
        )
        .await?;
    if incident.resolution.is_some() {
        let recovery = if let Some(intent) = hazard.recovery {
            let outcome = store.apply_recovery(intent, frame.observation_time).await?;
            Some((intent, outcome))
        } else {
            None
        };
        return Ok(Some(HazardCommit {
            scope: frame.scope,
            incident: incident.id,
            packet: None,
            recovery,
            resolved: true,
        }));
    }
    if incident.last_source_us != hazard.source_us
        || (opening
            && (incident.evidence.is_some()
                || incident.evidence_state == EvidenceState::Unavailable))
    {
        return Ok(hazard.recovery.map(|intent| HazardCommit {
            scope: frame.scope,
            incident: incident.id,
            packet: None,
            recovery: Some((intent, RecoveryOutcome::Superseded)),
            resolved: false,
        }));
    }
    if matches!(
        hazard.write,
        HazardWrite::Observe | HazardWrite::Open { packet: false }
    ) {
        return Ok(None);
    }
    if !opening
        && let Some(stamp) = incident.evidence
        && stamp.expires_at == frame.observation_time.expires_at()
    {
        // A timed-out write may have committed before its caller retried.
        // The packet's immutable clock matches this exact source observation.
        let existing = store.evidence_packet(incident.id, stamp.packet_id).await?;
        if existing.images.iter().any(|image| {
            image.kind == sw_protocol::EvidenceImageKind::Overview
                && (image.source_s * 1_000_000.0).round() as i64 == hazard.source_us
        }) {
            let result = if incident.evidence_state == EvidenceState::Persisted
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
            return packet_committed(store, frame, hazard, result).await;
        }
    }
    let evidence = evidence.clone();
    let onset = hazard.segment_start_us;
    let region = hazard.region;
    let episode = hazard.lineage.episode_id;
    let after_gap = hazard.segment_start_us > hazard.first_source_us;
    let frame_copy = frame.clone();
    let selected = tokio::task::spawn_blocking(move || {
        evidence.select(&frame_copy, onset, region, episode, after_gap)
    })
    .await
    .map_err(|_| StoreError::Unavailable)?;
    if selected.images.is_empty() {
        if incident.evidence_state == EvidenceState::Pending {
            store.evidence_unavailable(incident.id, utc_ms()).await?;
        }
        return packet_committed(
            store,
            frame,
            hazard,
            PacketResult::Unavailable {
                at: frame.captured_at,
            },
        )
        .await;
    }
    let coverage = selected.coverage;
    let mut limitations = Vec::new();
    if !frame.live {
        limitations.push(EvidenceLimitation::NonLive);
    }
    if hazard.limited {
        limitations.push(EvidenceLimitation::PartialVisibility);
    }
    if after_gap {
        limitations.push(EvidenceLimitation::ObservationGap);
    }
    if !selected.has_pre_event {
        limitations.push(EvidenceLimitation::PreEventUnavailable);
    }
    let gap_budget_ms = 1_500 / u32::from(frame.sampling.evidence_fps);
    if coverage.max_gap_ms > gap_budget_ms
        || u32::from(coverage.received_samples) * 4 < u32::from(coverage.expected_samples) * 3
    {
        limitations.push(EvidenceLimitation::SparseCoverage);
    }
    let context = EvidenceContext {
        observed_interval: EvidenceInterval {
            start_source_s: hazard.segment_start_us.min(hazard.source_us) as f64 / 1_000_000.0,
            end_source_s: hazard.source_us as f64 / 1_000_000.0,
        },
        coverage,
        sampling: Some(selected.sampling),
        local_facts: hazard.facts,
        lineage: Some(hazard.lineage),
        recovery: hazard.recovery.map(RecoveryIntent::observation),
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
    packet_committed(store, frame, hazard, result).await
}

async fn packet_committed(
    store: &Store,
    frame: &CameraFrame,
    hazard: &Hazard,
    packet: PacketResult,
) -> Result<Option<HazardCommit>, StoreError> {
    let recovery = if let Some(intent) = hazard.recovery {
        let outcome = store.apply_recovery(intent, frame.observation_time).await?;
        Some((intent, outcome))
    } else {
        None
    };
    Ok(Some(HazardCommit {
        scope: frame.scope,
        incident: hazard.id,
        packet: Some(packet),
        recovery,
        resolved: false,
    }))
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
            segment_start_us: 0,
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
            directly_visible: true,
            recovery: None,
            lineage: EvidenceLineage {
                episode_id: Uuid::nil(),
                fire_track_id: Some(Uuid::nil()),
                association: EpisodeAssociation::Continuous,
            },
            write: HazardWrite::Open { packet: false },
        }
    }

    #[test]
    fn medical_gap_keeps_a_low_subjects_incident_without_granting_identity() {
        let scope = SourceScope {
            camera_id: Uuid::new_v4(),
            scene_revision: 1,
            epoch: 1,
        };
        let mut policy = HazardPolicies::new(scope);
        let incident = Uuid::new_v4();
        let episode = Uuid::new_v4();
        let make_hazard = |episode_id, onset_observed, source_us| {
            let mut hazard = fire(0, EvidenceVisibility::Good);
            hazard.reason = if onset_observed {
                "suspected_fall"
            } else {
                "persistent_low_posture"
            };
            hazard.facts = LocalFacts::Medical {
                episode_id,
                track_id: Counter(1),
                previous_episode_id: None,
                onset_observed,
                identity_uncertain: false,
                recovery_seconds: 0.0,
                clear_eligible: false,
            };
            hazard.lineage.episode_id = episode_id;
            hazard.source_us = source_us;
            hazard.first_source_us = source_us;
            hazard.segment_start_us = source_us;
            hazard
        };
        let initial = make_hazard(episode, true, 0);
        policy
            .published
            .insert(incident, Published::new(&initial, Instant::now(), true));
        let new_episode = Uuid::new_v4();
        let mut after_gap = [make_hazard(new_episode, false, 1_600_000)];
        policy.associate_incidents(&mut after_gap, 1_600_000);
        let hazard = &after_gap[0];
        assert_eq!(hazard.id, incident);
        assert_eq!(hazard.lineage.episode_id, new_episode);
        assert_eq!(hazard.lineage.association, EpisodeAssociation::Uncertain);
        assert_eq!(hazard.first_source_us, 0);
        assert_eq!(hazard.segment_start_us, 1_600_000);
        assert!(matches!(
            hazard.facts,
            LocalFacts::Medical {
                identity_uncertain: true,
                clear_eligible: false,
                ..
            }
        ));
        assert!(hazard.recovery.is_none());

        let mut different = [make_hazard(Uuid::new_v4(), false, 2_000_000)];
        different[0].region.x1 = 120.0;
        different[0].region.x2 = 220.0;
        policy.associate_incidents(&mut different, 2_000_000);
        assert_ne!(different[0].id, incident);

        let mut second_fall = [make_hazard(Uuid::new_v4(), true, 5_000_000)];
        policy.associate_incidents(&mut second_fall, 5_000_000);
        assert_ne!(second_fall[0].id, incident);
        assert_eq!(second_fall[0].first_source_us, 5_000_000);
    }

    #[test]
    fn material_changes_continue_after_two_updates_and_replenish_packet_budget() {
        let initial = fire(0, EvidenceVisibility::Limited);
        let start = Instant::now();
        let mut budget = PacketBudget::default();
        assert!(budget.take(start, true));
        let mut published = Published::new(&initial, start, true);
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
        let mut published = Published::new(&hazard, start, false);
        published.retry_packet = Some(start + PACKET_RETRY);
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
        let mut published = Published::new(&initial, start, true);
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

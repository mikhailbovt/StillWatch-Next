use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Box2d, Category, MedicalSettings,
    pose::{Pose, cropped_translation, displacement, standing_bend, visible_ascent},
};

const EPSILON: f64 = 1e-6;
pub(crate) const MAX_TRACKS: usize = 128;
const HISTORY_SAMPLES: usize = 120;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    Upright,
    Seated,
    Low,
    Uncertain,
}

impl Posture {
    pub(crate) fn supported(self) -> bool {
        matches!(self, Self::Upright | Self::Seated)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPhase {
    Observing,
    FollowingConcern,
    OrdinaryActivity,
    RecoveredActivity,
    VisibilityLost,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    None,
    FallLike,
    SlowDescent,
    ControlledPattern,
    UnknownOrigin,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryBasis {
    UprightActivity,
    PriorSupportedPosture,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MotionEvidence {
    pub classifier_version: String,
    pub episode_id: Uuid,
    pub previous_episode_id: Option<Uuid>,
    pub track_id: u64,
    pub epoch: u64,
    pub started_ts: f64,
    pub observed_ts: f64,
    pub last_seen_ts: f64,
    pub phase: MotionPhase,
    pub transition: TransitionKind,
    pub posture: Posture,
    pub onset_observed: bool,
    pub identity_uncertain: bool,
    pub recovery_seconds: f64,
    pub recovery_basis: Option<RecoveryBasis>,
    pub clear_eligible: bool,
    pub features: Option<MotionFeatures>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct MotionFeatures {
    pub normalized_descent: f64,
    pub visible_samples: usize,
    pub reliable_pose: u8,
}

impl MotionEvidence {
    fn new(track_id: u64, epoch: u64, ts: f64) -> Self {
        Self {
            classifier_version: "causal-motion-v4".into(),
            episode_id: Uuid::new_v4(),
            previous_episode_id: None,
            track_id,
            epoch,
            started_ts: ts,
            observed_ts: ts,
            last_seen_ts: ts,
            phase: MotionPhase::Observing,
            transition: TransitionKind::None,
            posture: Posture::Uncertain,
            onset_observed: false,
            identity_uncertain: false,
            recovery_seconds: 0.0,
            recovery_basis: None,
            clear_eligible: false,
            features: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MedicalReason {
    RapidPostureChange,
    PartialBodyDescent,
    PersistentLowPosture,
    ViewLostAfterSuspicion,
    SlowPostureDescent,
    SupportPatternLost,
    PostureTransition,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MedicalCandidate {
    pub candidate_id: Uuid,
    pub category: Category,
    pub reason: MedicalReason,
    pub first_ts: f64,
    pub last_ts: f64,
    #[serde(rename = "box")]
    pub region: Box2d,
    pub track_id: u64,
    pub priority: u8,
    pub limited: bool,
    pub epoch: u64,
    pub motion: Option<MotionEvidence>,
}

impl MedicalCandidate {
    pub(crate) fn from_pose(
        pose: &Pose,
        epoch: u64,
        reason: MedicalReason,
        first_ts: f64,
        region: Box2d,
        priority: u8,
        limited: bool,
    ) -> Self {
        Self {
            candidate_id: Uuid::new_v4(),
            category: Category::Medical,
            reason,
            first_ts,
            last_ts: pose.ts(),
            region,
            track_id: pose.observation.track_id.expect("associated pose"),
            priority,
            limited,
            epoch,
            motion: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct MotionClassifier {
    tracks: BTreeMap<u64, Track>,
}

impl MotionClassifier {
    pub fn clear(&mut self) {
        self.tracks.clear();
    }

    pub fn track(&self, id: u64) -> Option<&Track> {
        self.tracks.get(&id)
    }

    pub fn snapshot(&self) -> impl Iterator<Item = &MotionEvidence> {
        self.tracks.values().map(|track| &track.evidence)
    }

    pub fn visible(&self, ts: f64) -> impl Iterator<Item = (&MotionEvidence, Box2d)> {
        self.tracks.values().filter_map(move |track| {
            let latest = track.history.back().filter(|pose| pose.ts() == ts)?;
            Some((&track.evidence, latest.observation.region))
        })
    }

    pub fn update(
        &mut self,
        observations: &[Pose],
        ts: f64,
        epoch: u64,
        settings: MedicalSettings,
        mut candidates: Vec<MedicalCandidate>,
    ) -> Vec<MedicalCandidate> {
        let mut seen = BTreeSet::new();
        let mut extra = Vec::new();
        for pose in observations {
            let obs = &pose.observation;
            let Some(id) = obs.track_id else {
                continue;
            };
            seen.insert(id);
            if let Some(track) = self.tracks.get(&id) {
                if ts <= track.evidence.last_seen_ts {
                    continue;
                }
            } else {
                if self.tracks.len() >= MAX_TRACKS
                    && let Some((&oldest, _)) = self.tracks.iter().min_by(|a, b| {
                        a.1.evidence
                            .last_seen_ts
                            .total_cmp(&b.1.evidence.last_seen_ts)
                    })
                {
                    self.tracks.remove(&oldest);
                }
                self.tracks.insert(id, Track::new(id, epoch, ts));
            }
            let ambiguous = obs.association_uncertain
                || observations.iter().any(|other| {
                    other.observation.track_id != Some(id)
                        && other.observation.region.iou(obs.region) > 0.5
                });
            let local: Vec<_> = candidates.iter().filter(|c| c.track_id == id).collect();
            let track = self.tracks.get_mut(&id).expect("current track");
            track.update(pose, ts, settings, ambiguous, &local, &mut extra);
        }
        self.tracks.retain(|id, track| {
            let age = ts - track.evidence.last_seen_ts;
            if !seen.contains(id) && age + EPSILON >= settings.lost_track_seconds {
                track.evidence.clear_eligible = false;
                track.evidence.phase = MotionPhase::VisibilityLost;
                track.evidence.observed_ts = ts;
                track.recovery_since = None;
                track.baseline_since = None;
                track.reobserved = None;
            }
            age <= settings.dedup_seconds
        });
        candidates.extend(extra);
        for candidate in &mut candidates {
            if let Some(track) = self.tracks.get_mut(&candidate.track_id) {
                track.concern = true;
                if track.evidence.transition == TransitionKind::None {
                    track.evidence.transition = TransitionKind::UnknownOrigin;
                }
                track.emitted = true;
                candidate.motion = Some(track.evidence.clone());
                if track.evidence.transition == TransitionKind::UnknownOrigin
                    && matches!(
                        candidate.reason,
                        MedicalReason::RapidPostureChange | MedicalReason::PartialBodyDescent
                    )
                {
                    candidate.limited = true;
                }
                if track.evidence.transition == TransitionKind::ControlledPattern
                    && candidate.reason == MedicalReason::RapidPostureChange
                {
                    candidate.reason = MedicalReason::PostureTransition;
                }
            }
        }
        candidates
    }
}

pub(crate) struct Track {
    evidence: MotionEvidence,
    pub history: VecDeque<Pose>,
    baseline: Option<Pose>,
    baseline_since: Option<f64>,
    recovery_since: Option<f64>,
    last_recovery_ts: Option<f64>,
    low_since: Option<f64>,
    pub concern: bool,
    emitted: bool,
    support: Option<Pose>,
    last_low_ts: Option<f64>,
    unresolved_landing: bool,
    activity_resumed: bool,
    recovery_observed: bool,
    reobserved: Option<(f64, Pose)>,
}

impl Track {
    fn new(track_id: u64, epoch: u64, ts: f64) -> Self {
        Self {
            evidence: MotionEvidence::new(track_id, epoch, ts),
            history: VecDeque::new(),
            baseline: None,
            baseline_since: None,
            recovery_since: None,
            last_recovery_ts: None,
            low_since: None,
            concern: false,
            emitted: false,
            support: None,
            last_low_ts: None,
            unresolved_landing: false,
            activity_resumed: false,
            recovery_observed: false,
            reobserved: None,
        }
    }

    fn new_episode(&mut self, start: f64, after_activity: bool) {
        let old = &self.evidence;
        let previous = after_activity.then_some(old.episode_id);
        self.evidence = MotionEvidence::new(old.track_id, old.epoch, start);
        self.evidence.previous_episode_id = previous;
        self.concern = false;
        self.emitted = false;
        self.recovery_since = None;
        self.last_recovery_ts = None;
        self.low_since = None;
        self.support = None;
        self.unresolved_landing = false;
        self.activity_resumed = false;
        self.recovery_observed = false;
        self.reobserved = None;
    }

    fn update(
        &mut self,
        pose: &Pose,
        ts: f64,
        settings: MedicalSettings,
        ambiguous: bool,
        local: &[&MedicalCandidate],
        extra: &mut Vec<MedicalCandidate>,
    ) {
        use MedicalReason::*;
        use TransitionKind as Transition;
        let obs = &pose.observation;
        let epoch = self.evidence.epoch;
        let gap = !self.history.is_empty()
            && ts - self.evidence.last_seen_ts > settings.lost_track_seconds + EPSILON;
        if gap || ambiguous {
            self.reobserved = None;
        }
        if gap {
            self.support = None;
            self.activity_resumed = false;
            self.baseline_since = None;
            self.recovery_since = None;
            self.low_since = None;
            self.evidence.clear_eligible = false;
            self.evidence.identity_uncertain |= self.concern;
            if self.evidence.transition == Transition::ControlledPattern {
                self.new_episode(ts, false);
            }
        }
        if ambiguous {
            self.evidence.identity_uncertain = true;
            self.activity_resumed = false;
            self.baseline = None;
            self.history.clear();
        } else if !self.concern {
            self.evidence.identity_uncertain = false;
        }
        let mut sample_count = self
            .history
            .iter()
            .rev()
            .take_while(|p| ts - p.ts() <= 5.0)
            .count();
        let last = (sample_count > 0).then(|| self.history.back()).flatten();
        let mut last_ts = last.map(Pose::ts);
        let mut last_posture = last.map(|p| p.posture);
        let reference = self
            .history
            .iter()
            .skip(self.history.len() - sample_count)
            .filter(|p| !p.observation.association_uncertain)
            .reduce(|best, p| if p.height() > best.height() { p } else { best });
        let (descent, shoulder) =
            reference.map_or((None, None), |prior| displacement(prior, pose, settings));
        let mut drop = descent.unwrap_or_default();
        let mut slow = reference.is_some_and(|prior| {
            (1.0..=5.0).contains(&(ts - prior.ts()))
                && drop >= 0.25
                && shoulder.is_some_and(|delta| delta >= settings.pre_drop_ratio)
                && !obs.association_uncertain
                && (pose.height() < prior.height() * 0.78 || pose.low)
                && (pose.low
                    || (pose.reliable && pose.body_visible)
                    || (pose.torso_visible
                        && prior.torso_visible
                        && pose.torso_length < prior.torso_length * 0.75))
                && pose.torso_visible
                && (!obs.truncated || pose.low)
                && (pose.body_visible
                    || self
                        .history
                        .iter()
                        .rev()
                        .take(sample_count)
                        .any(|p| p.body_visible))
                && last.is_some_and(|p| {
                    !cropped_translation(p, pose, Some(&self.history), self.concern, settings)
                })
                && !standing_bend(prior, pose)
        });
        let reference = reference.map(|p| (p.ts(), p.observation.region));
        let mut support = if self.unresolved_landing {
            None
        } else {
            self.controlled(pose, settings).cloned()
        };
        let mut rapid = if support.is_some() {
            None
        } else {
            local
                .iter()
                .copied()
                .find(|c| matches!(c.reason, RapidPostureChange | PartialBodyDescent))
        };
        let recent_drop = !obs.association_uncertain
            && last.is_some_and(|p| {
                displacement(p, pose, settings)
                    .0
                    .is_some_and(|delta| delta > 0.035)
            });
        let material = rapid.is_some() || (slow && recent_drop);
        let onset = rapid.map_or_else(
            || {
                if slow {
                    reference.map_or(ts, |r| r.0)
                } else {
                    ts
                }
            },
            |c| c.first_ts,
        );
        if material
            && self.concern
            && self.evidence.identity_uncertain
            && !ambiguous
            && let Some((since, baseline)) = &self.reobserved
            && onset - since >= settings.recovery_seconds - EPSILON
            && ts - baseline.ts() <= 0.4 + EPSILON
        {
            let baseline = baseline.clone();
            self.new_episode(onset, false);
            self.baseline_since = Some(baseline.ts());
            self.baseline = Some(baseline);
        }
        let new_low = (self.recovery_observed && pose.low)
            .then(|| {
                local
                    .iter()
                    .filter(|c| {
                        c.reason == PersistentLowPosture
                            && self.last_recovery_ts.is_some_and(|time| c.first_ts > time)
                    })
                    .map(|c| c.first_ts)
                    .min_by(f64::total_cmp)
            })
            .flatten();
        if (material && (self.activity_resumed || (self.recovery_observed && !local.is_empty())))
            || new_low.is_some()
        {
            let continuous = self.activity_resumed && !self.evidence.identity_uncertain;
            self.new_episode(
                new_low.unwrap_or(if continuous { onset } else { ts }),
                continuous,
            );
            if !continuous {
                self.baseline = None;
                self.history.clear();
                sample_count = 0;
                last_ts = None;
                last_posture = None;
                drop = 0.0;
            }
            if new_low.is_some() || !continuous {
                rapid = None;
                support = None;
                slow = false;
                self.unresolved_landing = local.iter().any(|c| c.reason == PartialBodyDescent);
            }
        }
        if let Some(rapid) = rapid {
            self.evidence.transition = Transition::FallLike;
            self.evidence.onset_observed = true;
            self.evidence.started_ts = self.evidence.started_ts.min(rapid.first_ts);
            self.concern = true;
            self.unresolved_landing |= rapid.reason == PartialBodyDescent;
        } else if let Some(support) = support {
            self.evidence.transition = Transition::ControlledPattern;
            self.evidence.onset_observed = true;
            self.concern = true;
            self.support = Some(support);
        } else if slow
            && recent_drop
            && self.evidence.transition != Transition::FallLike
            && !(self.evidence.transition == Transition::ControlledPattern
                && self.support.is_some())
            && let Some((start, region)) = reference
        {
            self.evidence.transition = Transition::SlowDescent;
            self.evidence.onset_observed = true;
            self.concern = true;
            if local.is_empty() && !self.emitted {
                extra.push(MedicalCandidate::from_pose(
                    pose,
                    epoch,
                    SlowPostureDescent,
                    start,
                    region.union(obs.region),
                    2,
                    !pose.reliable,
                ));
            }
        }
        if self.evidence.transition == Transition::ControlledPattern {
            let shift = self
                .support
                .as_ref()
                .map_or(0.0, |p| (pose.center_y - p.center_y) / p.height());
            if shift < -0.18 {
                self.support = None;
            } else if shift > 0.18 {
                self.new_episode(last_ts.unwrap_or(ts), self.activity_resumed);
                self.evidence.transition = Transition::SlowDescent;
                self.evidence.onset_observed = true;
                self.concern = true;
                extra.push(MedicalCandidate::from_pose(
                    pose,
                    epoch,
                    SupportPatternLost,
                    self.evidence.started_ts,
                    obs.region,
                    2,
                    !pose.torso_visible,
                ));
            }
        }
        if pose.low {
            self.unresolved_landing |= rapid.is_some();
            self.low_since.get_or_insert(ts);
            self.last_low_ts = Some(ts);
            self.recovery_since = None;
        } else if pose.posture.supported() || self.last_low_ts.is_none_or(|time| ts - time > 0.6) {
            self.low_since = None;
        }
        if !local.is_empty()
            || (self
                .low_since
                .is_some_and(|time| ts - time >= settings.low_pose_seconds)
                && !obs.limited
                && !obs.truncated)
        {
            self.concern = true;
            if self.evidence.transition == Transition::None {
                self.evidence.transition = Transition::UnknownOrigin;
            }
        }
        if self.concern {
            self.assess_recovery(pose, ts, material, settings);
        } else {
            self.evidence.phase = MotionPhase::Observing;
            if pose.reliable && pose.posture.supported() && drop.abs() < 0.08 {
                if last_posture.is_some_and(|previous| previous != pose.posture) {
                    self.baseline_since = None;
                }
                if ts - *self.baseline_since.get_or_insert(ts) >= 1.0 {
                    self.baseline = Some(pose.clone());
                }
            } else {
                self.baseline_since = None;
            }
        }
        self.update_reobserved(pose, ts, ambiguous, material);
        self.evidence.observed_ts = ts;
        self.evidence.last_seen_ts = ts;
        self.evidence.posture = pose.posture;
        self.evidence.features = Some(MotionFeatures {
            normalized_descent: (drop * 10_000.0).round_ties_even() / 10_000.0,
            visible_samples: sample_count + 1,
            reliable_pose: u8::from(pose.reliable),
        });
        if self.history.len() == HISTORY_SAMPLES {
            self.history.pop_front();
        }
        self.history.push_back(pose.clone());
    }

    fn controlled(&self, pose: &Pose, settings: MedicalSettings) -> Option<&Pose> {
        if !pose.low || !pose.torso_visible || self.unresolved_landing {
            return None;
        }
        let sequence: Vec<_> = self
            .history
            .iter()
            .filter(|p| pose.ts() - p.ts() <= 5.0)
            .chain([pose])
            .collect();
        // Require separate observations across both preparation and descent.
        // Four seated samples span one second at 3 fps.
        if sequence.len() < 6 || sequence.iter().any(|p| !p.torso_visible) {
            return None;
        }
        for (end, &reference) in sequence[..sequence.len() - 1].iter().enumerate() {
            if pose.ts() - reference.ts() < 0.8 - EPSILON {
                continue;
            }
            let start =
                sequence[..=end].partition_point(|p| reference.ts() - p.ts() > 1.2 + EPSILON);
            let prepared = &sequence[start..=end];
            if prepared.len() < 4
                || reference.ts() - prepared[0].ts() < 0.8 - EPSILON
                || prepared.windows(2).any(|pair| {
                    let gap = pair[1].ts() - pair[0].ts();
                    gap <= 0.0 || gap > settings.lost_track_seconds + EPSILON
                })
                || prepared
                    .iter()
                    .any(|p| !p.reliable || p.posture != Posture::Seated)
            {
                continue;
            }
            let (low, high) = prepared
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p.center_y), hi.max(p.center_y))
                });
            let height = reference.height();
            let recline = &sequence[end..];
            if recline.len() >= 3
                && high - low <= height * 0.08
                && recline
                    .iter()
                    .all(|p| (p.center_y - reference.center_y).abs() <= height * 0.18)
                && recline.windows(2).all(|p| {
                    let elapsed = p[1].ts() - p[0].ts();
                    elapsed > 0.0
                        && elapsed <= settings.lost_track_seconds + EPSILON
                        && (p[1].center_y - p[0].center_y).abs() / height / elapsed <= 0.55
                })
            {
                // The loop excludes the current sample. The result always belongs to history.
                return self
                    .history
                    .get(self.history.len() - (sequence.len() - 1) + end);
            }
        }
        None
    }

    fn assess_recovery(&mut self, pose: &Pose, ts: f64, material: bool, settings: MedicalSettings) {
        let baseline_return = self.baseline.as_ref().is_some_and(|prior| {
            prior.posture == Posture::Seated
                && pose.posture == Posture::Seated
                && pose.height() >= prior.height() * 0.85
                && (pose.center_y - prior.center_y).abs() < prior.height() * 0.18
        });
        let certain = !self.evidence.identity_uncertain;
        let rising = !self.activity_resumed
            && !self.recovery_observed
            && certain
            && pose.reliable
            && pose.posture == Posture::Upright
            && self
                .history
                .iter()
                .any(|p| visible_ascent(p, pose, settings.drop_window_seconds));
        let recovering = pose.reliable
            && !rising
            && certain
            && (pose.posture == Posture::Upright || baseline_return);
        if recovering && !material {
            self.recovery_since.get_or_insert(ts);
            self.last_recovery_ts = Some(ts);
        } else if material
            || rising
            || pose.low
            || !certain
            || self
                .last_recovery_ts
                .is_none_or(|time| ts - time > 0.4 + EPSILON)
        {
            self.recovery_since = None;
        }
        let seconds = self.recovery_since.map_or(0.0, |time| (ts - time).max(0.0));
        let ordinary = self.evidence.transition == TransitionKind::ControlledPattern
            && self.support.is_some()
            && self
                .low_since
                .is_some_and(|time| ts - time >= settings.ordinary_activity_seconds)
            && pose.torso_visible
            && certain;
        let phase = if ordinary {
            MotionPhase::OrdinaryActivity
        } else if recovering && seconds >= settings.recovery_seconds {
            MotionPhase::RecoveredActivity
        } else {
            MotionPhase::FollowingConcern
        };
        self.evidence.phase = phase;
        self.evidence.recovery_seconds = seconds;
        self.evidence.recovery_basis = recovering.then_some(if baseline_return {
            RecoveryBasis::PriorSupportedPosture
        } else {
            RecoveryBasis::UprightActivity
        });
        self.evidence.clear_eligible = matches!(
            phase,
            MotionPhase::OrdinaryActivity | MotionPhase::RecoveredActivity
        );
        self.activity_resumed |= self.evidence.clear_eligible;
        self.recovery_observed |= phase == MotionPhase::RecoveredActivity;
    }

    fn update_reobserved(&mut self, pose: &Pose, ts: f64, ambiguous: bool, material: bool) {
        if self.concern && self.evidence.identity_uncertain && !ambiguous && !material {
            let continuous = self
                .reobserved
                .as_ref()
                .filter(|(_, prior)| ts - prior.ts() <= 0.4 + EPSILON);
            if pose.reliable && pose.posture.supported() {
                let since = continuous
                    .filter(|(_, prior)| prior.posture == pose.posture)
                    .map_or(ts, |(since, _)| *since);
                self.reobserved = Some((since, pose.clone()));
            } else if pose.low || continuous.is_none() {
                self.reobserved = None;
            }
        } else {
            self.reobserved = None;
        }
    }
}

#[cfg(test)]
mod cadence_tests {
    use std::sync::Arc;

    use crate::{Keypoint, Observation, ObservationLabel};

    use super::*;

    fn supported_recline(at: f64, drop: f64) -> Pose {
        let settled = at >= 1.4;
        let low = at >= 2.3;
        let height = if low { 65.0 } else { 100.0 };
        let center_y = 65.0
            + drop
            + if settled {
                ((at - 1.4) * 12.0).min(13.0)
            } else {
                0.0
            };
        Pose {
            observation: Arc::new(Observation {
                capture_ts: at,
                region: Box2d {
                    x1: 10.0,
                    y1: 20.0,
                    x2: 100.0,
                    y2: 20.0 + height,
                },
                score: 0.95,
                label: ObservationLabel::Person,
                track_id: Some(1),
                keypoints: vec![Keypoint(0.0, 0.0, 0.0); 17],
                limited: false,
                truncated: false,
                association_uncertain: false,
                torso_histogram: Vec::new(),
            }),
            center_y,
            lean: Some(if low { 2.0 } else { 0.1 }),
            low,
            posture: if low {
                Posture::Low
            } else if settled {
                Posture::Uncertain
            } else {
                Posture::Seated
            },
            reliable: true,
            torso_visible: true,
            legs_visible: true,
            body_visible: true,
            shoulder_y: Some(center_y - 20.0),
            torso_length: 35.0,
        }
    }

    #[test]
    fn controlled_descent_uses_observed_span_and_coverage_at_each_allowed_pose_rate() {
        let settings = MedicalSettings::default();
        for fps in [3, 4, 5] {
            for offset in [0.0, 0.04] {
                let mut track = Track::new(1, 1, 0.0);
                for step in 0..fps * 3 {
                    let jitter = if step % 2 == 0 { 0.0 } else { 0.008 };
                    let at = f64::from(step) / f64::from(fps) + offset + jitter;
                    track.history.push_back(supported_recline(at, 0.0));
                }
                let current = supported_recline(3.0 + offset, 0.0);
                assert!(
                    track.controlled(&current, settings).is_some(),
                    "{fps} fps, {offset} s"
                );
                let rapid = supported_recline(current.ts(), 35.0);
                assert!(track.controlled(&rapid, settings).is_none());
                track
                    .history
                    .retain(|sample| sample.ts() <= 1.45 || sample.ts() >= 2.4);
                assert!(
                    track.controlled(&current, settings).is_none(),
                    "missing descent, {fps} fps"
                );
                track.unresolved_landing = true;
                assert!(track.controlled(&current, settings).is_none());
            }
        }
    }
}

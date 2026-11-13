use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Deserialize;

use crate::{
    HazardPolicy, MedicalCandidate, MedicalReason, MotionEvidence, Observation, PoseTracker,
    Posture, TrackingError,
    motion::{MAX_TRACKS, MotionClassifier},
    pose::{Pose, cropped_translation, displacement, standing_bend},
};

const EPSILON: f64 = 1e-6;
const DESCENT_SAMPLES: usize = 40;
const HISTORY_TRACKS: usize = MAX_TRACKS * 2;

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct MedicalSettings {
    pub drop_ratio: f64,
    pub pre_drop_ratio: f64,
    pub drop_window_seconds: f64,
    pub low_pose_seconds: f64,
    pub low_aspect_ratio: f64,
    pub keypoint_confidence: f64,
    pub lost_track_seconds: f64,
    pub dedup_seconds: f64,
    pub recovery_seconds: f64,
    pub ordinary_activity_seconds: f64,
}

impl Default for MedicalSettings {
    fn default() -> Self {
        Self {
            drop_ratio: 0.28,
            pre_drop_ratio: 0.10,
            drop_window_seconds: 0.8,
            low_pose_seconds: 2.0,
            low_aspect_ratio: 1.2,
            keypoint_confidence: 0.4,
            lost_track_seconds: 0.6,
            dedup_seconds: 12.0,
            recovery_seconds: 3.0,
            ordinary_activity_seconds: 2.0,
        }
    }
}

#[derive(Default)]
pub struct MedicalRules {
    settings: MedicalSettings,
    epoch: Option<u64>,
    tracker: PoseTracker,
    motion: MotionClassifier,
    subjects: BTreeMap<u64, Subject>,
}

pub struct MedicalSample {
    pub observations: Vec<Observation>,
    pub source_seconds: f64,
    pub source_epoch: u64,
}

impl HazardPolicy<MedicalSample> for MedicalRules {
    type Outcome = Result<Vec<MedicalCandidate>, TrackingError>;

    fn assess(&mut self, sample: MedicalSample) -> Self::Outcome {
        self.update(
            sample.observations,
            sample.source_seconds,
            sample.source_epoch,
        )
    }
}

#[derive(Default)]
struct Subject {
    history: VecDeque<Pose>,
    descent_boundary: Option<(f64, u8)>,
    low_since: Option<f64>,
    suspicion: Option<MedicalCandidate>,
    watch: Option<MedicalCandidate>,
    lost_reported: bool,
}

impl MedicalRules {
    pub fn reset(&mut self) {
        self.epoch = None;
        self.tracker.reset();
        self.motion.clear();
        self.subjects.clear();
    }

    pub fn snapshot(&self) -> impl Iterator<Item = &MotionEvidence> {
        self.motion.snapshot()
    }

    pub fn update(
        &mut self,
        observations: Vec<Observation>,
        ts: f64,
        epoch: u64,
    ) -> Result<Vec<MedicalCandidate>, TrackingError> {
        self.start_epoch(epoch);
        let associated = self.tracker.update(observations, ts, epoch)?;
        Ok(self.update_associated(associated, ts, epoch))
    }

    fn start_epoch(&mut self, epoch: u64) {
        if self.epoch != Some(epoch) {
            self.reset();
            self.epoch = Some(epoch);
        }
    }

    fn update_associated(
        &mut self,
        associated: Vec<Observation>,
        ts: f64,
        epoch: u64,
    ) -> Vec<MedicalCandidate> {
        let poses: Vec<_> = associated
            .into_iter()
            .map(|o| Pose::measure(o, self.settings))
            .collect();
        let mut candidates = Vec::new();
        let mut seen = BTreeSet::new();
        for pose in &poses {
            let Some(id) = pose.observation.track_id else {
                continue;
            };
            seen.insert(id);
            if self.subjects.len() >= HISTORY_TRACKS
                && !self.subjects.contains_key(&id)
                && let Some((&oldest, _)) = self.subjects.iter().min_by(|a, b| {
                    a.1.history
                        .back()
                        .map_or(f64::NEG_INFINITY, Pose::ts)
                        .total_cmp(&b.1.history.back().map_or(f64::NEG_INFINITY, Pose::ts))
                })
            {
                self.subjects.remove(&oldest);
            }
            let track = self.motion.track(id);
            if let Some(candidate) = self.subjects.entry(id).or_default().observe(
                pose,
                ts,
                epoch,
                self.settings,
                track.map(|t| &t.history),
                track.is_some_and(|t| t.concern),
            ) {
                candidates.push(candidate);
            }
        }
        self.subjects.retain(|id, subject| {
            let Some(last) = subject.history.back() else {
                return false;
            };
            let age = ts - last.ts();
            if !seen.contains(id)
                && !subject.lost_reported
                && age + EPSILON >= self.settings.lost_track_seconds
                && let Some(old) = subject.suspicion.as_ref().or(subject.watch.as_ref())
            {
                let mut lost = old.clone();
                lost.reason = MedicalReason::ViewLostAfterSuspicion;
                lost.last_ts = ts;
                lost.limited = true;
                lost.region = last.observation.region;
                candidates.push(lost);
                subject.lost_reported = true;
            }
            age <= self.settings.dedup_seconds
        });
        self.motion
            .update(&poses, ts, epoch, self.settings, candidates)
    }
}

impl Subject {
    fn observe(
        &mut self,
        pose: &Pose,
        ts: f64,
        epoch: u64,
        settings: MedicalSettings,
        motion_history: Option<&VecDeque<Pose>>,
        previous_concern: bool,
    ) -> Option<MedicalCandidate> {
        use MedicalReason::*;
        let obs = &pose.observation;
        if obs.association_uncertain {
            self.history.clear();
            self.descent_boundary = Some((pose.ts(), 0));
        } else if let Some((_, clean)) = self.descent_boundary.filter(|(_, clean)| *clean < 2) {
            self.descent_boundary = Some((pose.ts(), clean + 1));
        }
        while self.history.front().is_some_and(|p| ts - p.ts() > 4.0) {
            self.history.pop_front();
        }
        let body_context = pose.body_visible
            || (previous_concern && pose.legs_visible)
            || (obs.keypoints.is_empty() && !obs.truncated)
            || self.history.iter().any(|p| p.body_visible);
        let translation = self.history.back().is_some_and(|old| {
            cropped_translation(old, pose, motion_history, previous_concern, settings)
        });
        if translation && !previous_concern {
            self.watch = None;
        }
        if pose.low && !obs.association_uncertain {
            self.low_since.get_or_insert(ts);
        } else {
            self.low_since = None;
        }
        let mut candidate = None;
        if !obs.limited && (body_context || pose.low || obs.truncated) {
            let excluded = self
                .descent_boundary
                .map_or(f64::NEG_INFINITY, |(until, _)| until);
            let rotation_context = rotation_context(&self.history, settings.lost_track_seconds);
            if !obs.association_uncertain && !translation {
                for prior in self.history.iter().filter(|p| p.ts() > excluded) {
                    if !(0.15..=settings.drop_window_seconds + EPSILON).contains(&(ts - prior.ts()))
                        || prior.observation.association_uncertain
                        || standing_bend(prior, pose)
                    {
                        continue;
                    }
                    let prior_box = prior.observation.region;
                    let region = prior_box.union(obs.region);
                    if partial_descent(prior, pose) {
                        candidate = Some(MedicalCandidate::from_pose(
                            pose,
                            epoch,
                            PartialBodyDescent,
                            prior.ts(),
                            region,
                            2,
                            true,
                        ));
                        break;
                    }
                    let torso_pair = pose.has_torso_reference() && prior.has_torso_reference();
                    if !body_context && (obs.truncated || !torso_pair) {
                        continue;
                    }
                    let (Some(drop), shoulder) = displacement(prior, pose, settings) else {
                        continue;
                    };
                    let leaning = torso_pair
                        && prior.lean.is_some_and(|lean| lean < 0.7)
                        && pose.lean.is_some_and(|lean| lean > 1.4)
                        && obs.region.height() < prior_box.height() * 0.85;
                    let compressed = obs.region.height() < prior_box.height() * 0.9 || pose.low;
                    if (drop >= settings.pre_drop_ratio
                        && compressed
                        && (pose.posture != Posture::Upright || !pose.reliable)
                        && shoulder.is_none_or(|delta| delta >= settings.pre_drop_ratio))
                        || (leaning && (drop >= settings.pre_drop_ratio || rotation_context))
                        || (body_context
                            && torso_pair
                            && drop >= settings.drop_ratio
                            && shoulder.is_some_and(|delta| delta >= 0.2))
                    {
                        self.watch = Some(MedicalCandidate::from_pose(
                            pose,
                            epoch,
                            ViewLostAfterSuspicion,
                            prior.ts(),
                            region,
                            2,
                            true,
                        ));
                    }
                    let changed = obs.region.height() < prior_box.height() * 0.75 || pose.low;
                    if (drop >= settings.drop_ratio
                        && changed
                        && shoulder.is_none_or(|delta| delta >= settings.pre_drop_ratio))
                        || forward_collapse(prior, pose, previous_concern)
                        || (leaning && pose.low && drop >= settings.pre_drop_ratio)
                    {
                        candidate = Some(MedicalCandidate::from_pose(
                            pose,
                            epoch,
                            RapidPostureChange,
                            prior.ts(),
                            region,
                            3,
                            !pose.has_torso_reference(),
                        ));
                        break;
                    }
                }
            }
            if candidate.is_none()
                && pose.low
                && let Some(since) = self
                    .low_since
                    .filter(|since| ts - since >= settings.low_pose_seconds)
            {
                candidate = Some(MedicalCandidate::from_pose(
                    pose,
                    epoch,
                    PersistentLowPosture,
                    since,
                    obs.region,
                    1,
                    !pose.has_torso_reference(),
                ));
            }
            if candidate.is_none()
                && !pose.torso_visible
                && self.history.iter().any(|p| p.torso_visible)
                && let Some(watch) = self.watch.as_ref().filter(|c| ts - c.last_ts <= 1.5)
            {
                candidate = Some(MedicalCandidate::from_pose(
                    pose,
                    epoch,
                    ViewLostAfterSuspicion,
                    watch.first_ts,
                    obs.region,
                    1,
                    !pose.has_torso_reference(),
                ));
            }
        }
        if let Some(candidate) = &mut candidate {
            candidate.last_ts = ts;
            self.suspicion = Some(candidate.clone());
            self.lost_reported = false;
        }
        if self.history.len() == DESCENT_SAMPLES {
            self.history.pop_front();
        }
        self.history.push_back(pose.clone());
        if !pose.low
            && self.watch.as_ref().is_some_and(|watch| {
                ts - watch.last_ts > 1.0
                    || (pose.reliable
                        && pose.posture == Posture::Upright
                        && ts - watch.last_ts >= 0.2 - EPSILON)
            })
        {
            self.watch = None;
        }
        candidate
    }
}

fn forward_collapse(old: &Pose, new: &Pose, previous_concern: bool) -> bool {
    let height = old.observation.region.height();
    (previous_concern || old.body_visible)
        && old.torso_visible
        && new.torso_visible
        && old.torso_length >= height * 0.2
        && new.torso_length < old.torso_length * 0.6
        && new
            .shoulder_y
            .zip(old.shoulder_y)
            .is_some_and(|(y, previous)| (y - previous) / height.max(1.0) > 0.1)
        && new.observation.region.height() < height * 0.8
}

fn rotation_context(history: &VecDeque<Pose>, lost_track_seconds: f64) -> bool {
    let mut recent = history.iter().rev().take(4);
    let Some(last) = recent.next() else {
        return false;
    };
    let usable =
        |p: &Pose| !p.observation.association_uncertain && p.torso_visible && p.torso_length >= 8.0;
    if history.len() < 4 || !usable(last) {
        return false;
    }
    let mut next = last;
    for prior in recent {
        let dt = next.ts() - prior.ts();
        if dt <= 0.0 || dt > lost_track_seconds + EPSILON || !usable(prior) {
            return false;
        }
        next = prior;
    }
    last.ts() - next.ts() >= 0.6 - EPSILON
}

fn partial_descent(old: &Pose, new: &Pose) -> bool {
    let a = &old.observation;
    let b = &new.observation;
    let height = old.height();
    if !a.truncated
        || !b.truncated
        || b.region.height() > height * 0.65
        || b.region.y1 - a.region.y1 < height * 0.3
        || (b.region.y2 - a.region.y2).abs() > height * 0.15
        || a.keypoints.len().min(b.keypoints.len()) < 7
    {
        return false;
    }
    let shoulders = |obs: &Observation| (obs.keypoints[5], obs.keypoints[6]);
    let (al, ar) = shoulders(a);
    let (bl, br) = shoulders(b);
    if al.2.min(ar.2).min(bl.2).min(br.2) < 0.65 {
        return false;
    }
    let descent = (bl.1 - al.1).min(br.1 - ar.1) / height;
    let span = al.distance(ar);
    let foreshortened = span >= 10.0
        && bl.distance(br) <= span * 0.6
        && b.keypoints.len() >= 13
        && b.keypoints[11].2.min(b.keypoints[12].2) >= 0.65;
    descent >= 0.2
        || (b.region.height() <= height * 0.55
            && (descent >= 0.05 || (descent >= 0.02 && foreshortened)))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use flate2::read::GzDecoder;
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    #[derive(Deserialize)]
    struct Scenario {
        scenario: String,
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Step {
        operation: String,
        category: String,
        route: u32,
        recorded_association: Option<bool>,
        source_seconds: Option<f64>,
        epoch: Option<u64>,
        settings: Option<MedicalSettings>,
        observations: Option<Vec<Observation>>,
        tracked_observations: Option<Vec<Observation>>,
        candidates: Option<Vec<Value>>,
        motion: Option<Vec<Value>>,
    }

    fn compare(actual: &Value, expected: &Value, ids: &mut BTreeMap<Uuid, u64>, path: &str) {
        match (actual, expected) {
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "{path} keys");
                for (key, expected) in b {
                    let actual = &a[key];
                    let child = format!("{path}.{key}");
                    if matches!(
                        key.as_str(),
                        "candidate_id" | "episode_id" | "previous_episode_id"
                    ) && let (Some(actual), Some(expected)) =
                        (actual.as_str(), expected.as_u64())
                    {
                        let id = Uuid::parse_str(actual).unwrap();
                        let existing = ids.values().any(|&mapped| mapped == expected);
                        if let Some(old) = ids.insert(id, expected) {
                            assert_eq!(old, expected, "{child} continuity");
                        } else {
                            assert!(!existing, "{child} episode unexpectedly changed");
                        }
                    } else {
                        compare(actual, expected, ids, &child);
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{path} actual {a:?}, expected {b:?}");
                for (i, (a, b)) in a.iter().zip(b).enumerate() {
                    compare(a, b, ids, &format!("{path}[{i}]"));
                }
            }
            (Value::Number(a), Value::Number(b)) => {
                let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                assert!((a - b).abs() <= 1e-9, "{path} actual {a}, expected {b}");
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }

    #[test]
    fn medical_rules_match_locked_legacy_candidate_and_recovery_traces() {
        let fixture =
            include_bytes!("../../../tests/fixtures/observations/legacy-rules-v2.jsonl.gz");
        let mut compared = 0;
        for line in BufReader::new(GzDecoder::new(&fixture[..])).lines() {
            let trace: Scenario = serde_json::from_str(&line.unwrap()).unwrap();
            let mut routes = BTreeMap::<u32, MedicalRules>::new();
            let mut ids = BTreeMap::new();
            for (i, step) in trace.steps.into_iter().enumerate() {
                if step.category != "medical" {
                    continue;
                }
                let rules = routes.entry(step.route).or_default();
                if step.operation == "reset" {
                    rules.reset();
                    continue;
                }
                rules.settings = step.settings.unwrap();
                let ts = step.source_seconds.unwrap();
                let epoch = step.epoch.unwrap();
                let actual = if step.recorded_association == Some(true) {
                    rules.start_epoch(epoch);
                    rules.update_associated(step.tracked_observations.unwrap(), ts, epoch)
                } else {
                    rules.update(step.observations.unwrap(), ts, epoch).unwrap()
                };
                let path = format!("{} step {i}", trace.scenario);
                compare(
                    &serde_json::to_value(&actual).unwrap(),
                    &Value::Array(step.candidates.unwrap()),
                    &mut ids,
                    &format!("{path} candidates"),
                );
                let mut expected = step.motion.unwrap();
                expected.sort_by_key(|motion| motion["track_id"].as_u64().unwrap());
                let snapshot: Vec<_> = rules.snapshot().collect();
                compare(
                    &serde_json::to_value(&snapshot).unwrap(),
                    &Value::Array(expected),
                    &mut ids,
                    &format!("{path} motion"),
                );
                compared += 1;
            }
        }
        assert_eq!(compared, 1239);
    }
}

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Box2d, HazardPolicy, Observation, ObservationLabel, SourceScope};

const MAX_REGIONS: usize = 64;
pub(crate) const CONTINUITY_US: i64 = 1_500_000;
pub(crate) const WEAK_MATCH_IOU: f64 = 0.55;
const HISTORY_US: i64 = 5_000_000;
const HISTORY_SAMPLES: usize = 24;
const MAX_QUIET_US: i64 = 8_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Good,
    Limited,
    Poor,
}

#[derive(Clone, Copy, Debug)]
pub struct FireSettings {
    pub confidence: f64,
    pub immediate_confidence: f64,
}

impl Default for FireSettings {
    fn default() -> Self {
        Self {
            confidence: 0.20,
            immediate_confidence: 0.65,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FireDecision {
    Observing,
    ControlledContext,
    Review,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct FireFacts {
    pub flame_visible: bool,
    pub smoke_visible: bool,
    pub apparent_growth: bool,
    pub smoke_growth: bool,
    pub escaped_controlled_region: bool,
    pub controlled_region_overlap: Option<f64>,
    pub continuity_samples: u16,
    pub detector_confidence: f64,
    pub strong_detection: bool,
    pub visibility: Visibility,
}

pub struct FireSample<'a> {
    pub scope: SourceScope,
    pub source_us: i64,
    pub dimensions: (u32, u32),
    pub regions: &'a [Observation],
    pub weak_proposals: &'a [Observation],
    pub visibility: Visibility,
    /// Measure the current pixels in a source box. The flag allows local flame
    /// contrast when evaluating a flame observation in a dark scene.
    pub region_visibility: &'a dyn Fn(Box2d, bool) -> Visibility,
    /// Fraction of the current flame box inside a reviewed region.
    /// None represents missing or invalidated scene context.
    pub controlled_overlap: &'a dyn Fn(Box2d) -> Option<f64>,
}

pub struct FireEpisode {
    pub id: Uuid,
    pub track_id: Uuid,
    pub first_source_us: i64,
    pub segment_start_source_us: i64,
    pub source_us: i64,
    pub region: Box2d,
    pub decision: FireDecision,
    pub facts: FireFacts,
    pub association: FireAssociation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FireAssociation {
    New,
    Continuous,
    Reobserved,
    Split,
    Ambiguous,
}

pub struct FirePolicy {
    settings: FireSettings,
    scope: Option<SourceScope>,
    last_us: Option<i64>,
    dimensions: (u32, u32),
    regions: Vec<Region>,
    episodes: crate::fire_episode::FireEpisodeManager,
}

struct Region {
    id: Uuid,
    episode_id: Option<Uuid>,
    segment_start: i64,
    last_us: i64,
    region: Box2d,
    history: VecDeque<Measurement>,
    was_contained: bool,
    missing_since: Option<i64>,
}

#[derive(Clone, Copy)]
struct Measurement {
    at: i64,
    flame_area: Option<f64>,
    smoke_area: Option<f64>,
}

struct Cluster {
    flame: Option<Box2d>,
    smoke: Option<Box2d>,
    combined: Box2d,
    confidence: f64,
    visibility: Visibility,
}

impl<'a> HazardPolicy<FireSample<'a>> for FirePolicy {
    type Outcome = Vec<FireEpisode>;

    fn assess(&mut self, sample: FireSample<'a>) -> Self::Outcome {
        self.update(sample)
    }
}

impl FirePolicy {
    pub fn contains_episode(&self, id: Uuid) -> bool {
        self.episodes.contains(id)
    }

    pub fn record_resolution(&mut self, id: Uuid) {
        self.episodes.record_resolution(id);
    }

    pub fn new(settings: FireSettings) -> Option<Self> {
        if !(0.20..=1.0).contains(&settings.confidence)
            || !(settings.confidence..=1.0).contains(&settings.immediate_confidence)
        {
            return None;
        }
        Some(Self {
            settings,
            scope: None,
            last_us: None,
            dimensions: (0, 0),
            regions: Vec::new(),
            episodes: crate::fire_episode::FireEpisodeManager::default(),
        })
    }

    pub fn update(&mut self, sample: FireSample<'_>) -> Vec<FireEpisode> {
        let (width, height) = sample.dimensions;
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let same_scene = self.scope == Some(sample.scope) && self.dimensions == sample.dimensions;
        if same_scene && self.last_us.is_some_and(|last| sample.source_us <= last) {
            return Vec::new();
        }
        if !same_scene {
            self.scope = Some(sample.scope);
            self.dimensions = sample.dimensions;
            self.regions.clear();
            self.episodes = crate::fire_episode::FireEpisodeManager::default();
        }
        let gap = !same_scene
            || self
                .last_us
                .is_none_or(|last| sample.source_us.saturating_sub(last) > CONTINUITY_US);
        self.last_us = Some(sample.source_us);
        self.regions
            .retain(|old| sample.source_us.saturating_sub(old.last_us) <= MAX_QUIET_US);
        if gap {
            for old in &mut self.regions {
                old.clear_history();
                old.missing_since = None;
            }
        }
        let frame_area = f64::from(width) * f64::from(height);
        let mut clusters = Vec::<Cluster>::new();
        let mut observations: Vec<_> = sample
            .regions
            .iter()
            .filter(|o| {
                o.label != ObservationLabel::Person
                    && o.valid()
                    && o.score >= self.settings.confidence
                    && o.region.area() >= 1.0
                    && o.region.x1 >= 0.0
                    && o.region.y1 >= 0.0
                    && o.region.x2 <= f64::from(width)
                    && o.region.y2 <= f64::from(height)
            })
            .collect();
        observations.sort_by(|a, b| b.score.total_cmp(&a.score));
        for observed in observations.into_iter().take(MAX_REGIONS) {
            let visibility = (sample.region_visibility)(
                observed.region,
                observed.label == ObservationLabel::Fire,
            );
            if visibility == Visibility::Poor {
                continue;
            }
            if let Some(cluster) = clusters
                .iter_mut()
                .find(|c| adjacent(c.combined, observed.region))
            {
                cluster.add(observed, visibility);
            } else {
                let mut cluster = Cluster {
                    flame: None,
                    smoke: None,
                    combined: observed.region,
                    confidence: observed.score,
                    visibility,
                };
                cluster.add(observed, visibility);
                clusters.push(cluster);
            }
        }
        let mut used = vec![false; self.regions.len()];
        let mut reports = Vec::with_capacity(clusters.len());
        for cluster in clusters {
            let matched = self
                .regions
                .iter()
                .enumerate()
                .filter(|(index, region)| {
                    !used[*index] && adjacent(region.region, cluster.combined)
                })
                .max_by(|(_, a), (_, b)| {
                    a.region
                        .iou(cluster.combined)
                        .total_cmp(&b.region.iou(cluster.combined))
                })
                .map(|(index, _)| index);
            let track = if let Some(index) = matched {
                used[index] = true;
                index
            } else if self.regions.len() < MAX_REGIONS {
                self.regions.push(Region {
                    id: Uuid::new_v4(),
                    episode_id: None,
                    segment_start: sample.source_us,
                    last_us: sample.source_us,
                    region: cluster.combined,
                    history: VecDeque::new(),
                    was_contained: false,
                    missing_since: None,
                });
                used.push(true);
                self.regions.len() - 1
            } else {
                // Existing corroborated episodes retain their slots. Reclaim a stale unmatched slot.
                let Some((index, _)) = self
                    .regions
                    .iter()
                    .enumerate()
                    .filter(|(i, r)| {
                        !used[*i] && sample.source_us.saturating_sub(r.last_us) > CONTINUITY_US
                    })
                    .min_by_key(|(_, r)| r.last_us)
                else {
                    continue;
                };
                used[index] = true;
                self.regions[index] = Region {
                    id: Uuid::new_v4(),
                    episode_id: None,
                    segment_start: sample.source_us,
                    last_us: sample.source_us,
                    region: cluster.combined,
                    history: VecDeque::new(),
                    was_contained: false,
                    missing_since: None,
                };
                index
            };
            let region = &mut self.regions[track];
            let report = region.observe(&sample, cluster, frame_area, self.settings);
            reports.push((track, region.episode_id, report));
        }
        let visible_proposals: Vec<_> = sample
            .weak_proposals
            .iter()
            .filter(|weak| {
                weak.label != ObservationLabel::Person
                    && weak.valid()
                    && (f64::from(0.12_f32)..=f64::from(0.20_f32)).contains(&weak.score)
                    && (sample.region_visibility)(weak.region, weak.label == ObservationLabel::Fire)
                        != Visibility::Poor
            })
            .collect();
        for (index, region) in self.regions.iter_mut().enumerate() {
            if used[index] || region.history.is_empty() {
                continue;
            }
            let supported = visible_proposals.iter().any(|weak| {
                weak.region.iou(region.region) >= WEAK_MATCH_IOU
                    && region.history.back().is_some_and(|last| match weak.label {
                        ObservationLabel::Fire => last.flame_area.is_some(),
                        ObservationLabel::Smoke => last.smoke_area.is_some(),
                        ObservationLabel::Person => false,
                    })
            });
            if supported {
                region.end_absence(sample.source_us);
                continue;
            }
            match sample.visibility_in(region.region) {
                Visibility::Good => {
                    let since = *region.missing_since.get_or_insert(sample.source_us);
                    if sample.source_us.saturating_sub(since) > 600_000 {
                        region.clear_history();
                    }
                }
                Visibility::Poor => {
                    region.clear_history();
                    region.missing_since = None;
                }
                Visibility::Limited => region.missing_since = None,
            }
        }
        self.episodes
            .update(&sample, &visible_proposals, &mut reports);
        reports
            .into_iter()
            .map(|(track, prior, mut report)| {
                let region = &mut self.regions[track];
                if report.association == FireAssociation::Ambiguous {
                    region.clear_history();
                    region.episode_id = prior;
                } else {
                    if prior.is_some_and(|prior| prior != report.id) {
                        // The current measurement belongs to the new segment.
                        region.history.retain(|entry| entry.at == sample.source_us);
                        region.segment_start = sample.source_us;
                        region.was_contained = report
                            .facts
                            .controlled_region_overlap
                            .is_some_and(|overlap| overlap >= 0.98);
                        report.segment_start_source_us = sample.source_us;
                        report.facts.continuity_samples = region.history.len() as u16;
                    }
                    region.episode_id = Some(report.id);
                }
                report
            })
            .collect()
    }
}

impl FireSample<'_> {
    pub(crate) fn visibility_in(&self, region: Box2d) -> Visibility {
        if self.visibility == Visibility::Good {
            (self.region_visibility)(region, false)
        } else {
            self.visibility
        }
    }
}

impl Cluster {
    fn add(&mut self, observation: &Observation, visibility: Visibility) {
        let part = if observation.label == ObservationLabel::Fire {
            &mut self.flame
        } else {
            &mut self.smoke
        };
        *part = Some(part.map_or(observation.region, |old| old.union(observation.region)));
        self.combined = self.combined.union(observation.region);
        self.confidence = self.confidence.max(observation.score);
        if visibility == Visibility::Limited {
            self.visibility = visibility;
        }
    }
}

impl Region {
    fn clear_history(&mut self) {
        self.history.clear();
        self.was_contained = false;
    }

    fn end_absence(&mut self, at: i64) {
        if self
            .missing_since
            .take()
            .is_some_and(|since| at.saturating_sub(since) > 900_000)
        {
            self.clear_history();
        }
    }

    fn observe(
        &mut self,
        sample: &FireSample<'_>,
        cluster: Cluster,
        frame_area: f64,
        settings: FireSettings,
    ) -> FireEpisode {
        let at = sample.source_us;
        self.end_absence(at);
        if at.saturating_sub(self.last_us) > CONTINUITY_US {
            self.clear_history();
        }
        self.region = cluster.combined;
        self.last_us = at;
        let overlap = cluster
            .flame
            .and_then(sample.controlled_overlap)
            .filter(|overlap| (0.0..=1.0).contains(overlap));
        let contained = overlap.is_some_and(|value| value >= 0.98);
        let escaped = self.was_contained && overlap.is_some_and(|value| value < 0.90);
        self.was_contained = (self.was_contained || contained) && overlap.is_some();
        while self.history.len() >= HISTORY_SAMPLES
            || self
                .history
                .front()
                .is_some_and(|m| at.saturating_sub(m.at) > HISTORY_US)
        {
            self.history.pop_front();
        }
        if cluster.visibility != Visibility::Poor {
            if self.history.is_empty() {
                self.segment_start = at;
            }
            self.history.push_back(Measurement {
                at,
                flame_area: cluster.flame.map(|region| region.area() / frame_area),
                smoke_area: cluster.smoke.map(|region| region.area() / frame_area),
            });
        }
        let facts = FireFacts {
            flame_visible: cluster.flame.is_some(),
            smoke_visible: cluster.smoke.is_some(),
            apparent_growth: grows(&self.history, |m| m.flame_area),
            smoke_growth: grows(&self.history, |m| m.smoke_area),
            escaped_controlled_region: escaped,
            controlled_region_overlap: overlap,
            continuity_samples: self.history.len() as u16,
            detector_confidence: cluster.confidence,
            strong_detection: cluster.confidence >= settings.immediate_confidence,
            visibility: cluster.visibility,
        };
        let corroborated = self
            .history
            .front()
            .is_some_and(|first| at.saturating_sub(first.at) >= 300_000)
            && facts.continuity_samples >= 2;
        let controlled = contained
            && cluster.visibility == Visibility::Good
            && !facts.smoke_visible
            && !facts.apparent_growth
            && corroborated;
        let decision = if controlled {
            FireDecision::ControlledContext
        } else if cluster.visibility != Visibility::Poor
            && (escaped
                || facts.apparent_growth
                || facts.smoke_growth
                || ((facts.strong_detection || corroborated)
                    && !(contained && !facts.smoke_visible)))
        {
            FireDecision::Review
        } else {
            FireDecision::Observing
        };
        FireEpisode {
            id: self.id,
            track_id: self.id,
            first_source_us: self.segment_start,
            segment_start_source_us: self.segment_start,
            source_us: at,
            region: cluster.combined,
            decision,
            facts,
            association: FireAssociation::New,
        }
    }
}

fn adjacent(a: Box2d, b: Box2d) -> bool {
    if a.iou(b) >= 0.10 || a.contained_overlap(b) >= 0.25 {
        return true;
    }
    let (ax, ay) = a.center();
    let (bx, by) = b.center();
    let gap_x = (a.x1 - b.x2).max(b.x1 - a.x2).max(0.0);
    let gap_y = (a.y1 - b.y2).max(b.y1 - a.y2).max(0.0);
    let scale = a.width().max(a.height()).max(b.width()).max(b.height());
    gap_x.hypot(gap_y) <= scale * 0.15 && (ax - bx).hypot(ay - by) <= scale * 0.75
}

fn grows(history: &VecDeque<Measurement>, area: impl Fn(&Measurement) -> Option<f64>) -> bool {
    let Some(last) = history.back() else {
        return false;
    };
    if area(last).is_none() {
        return false;
    }
    let mut areas = [0.0; HISTORY_SAMPLES];
    let mut count = 0;
    let mut earliest = last.at;
    // An absent class has no measured box. Compare observations of the same
    // class and restart its trend after a gap in coverage.
    for measurement in history.iter().rev() {
        let Some(observed) = area(measurement) else {
            continue;
        };
        if earliest.saturating_sub(measurement.at) > CONTINUITY_US {
            break;
        }
        areas[count] = observed;
        count += 1;
        earliest = measurement.at;
    }
    if count < 4 || last.at.saturating_sub(earliest) < 1_200_000 {
        return false;
    }
    let median = |values: &mut [f64]| {
        values.sort_unstable_by(f64::total_cmp);
        (values[(values.len() - 1) / 2] + values[values.len() / 2]) * 0.5
    };
    let (late, early) = areas[..count].split_at_mut(count.div_ceil(2));
    let early = median(early);
    let late = median(late);
    late > early * 1.55 && late - early >= 0.0005
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> SourceScope {
        SourceScope {
            camera_id: Uuid::new_v4(),
            epoch: 1,
            scene_revision: 1,
        }
    }

    fn observation(label: ObservationLabel, score: f64, right: f64) -> Observation {
        Observation {
            capture_ts: 0.0,
            region: Box2d {
                x1: 10.0,
                y1: 10.0,
                x2: right,
                y2: 40.0,
            },
            score,
            label,
            track_id: None,
            keypoints: Vec::new(),
            limited: false,
            truncated: false,
            association_uncertain: false,
            torso_histogram: Vec::new(),
        }
    }

    fn sample(
        policy: &mut FirePolicy,
        scope: SourceScope,
        at: i64,
        item: Observation,
        visible: Visibility,
        overlap: Option<f64>,
    ) -> FireEpisode {
        policy
            .update(FireSample {
                scope,
                source_us: at,
                dimensions: (200, 150),
                regions: &[item],
                weak_proposals: &[],
                visibility: visible,
                region_visibility: &|_, _| visible,
                controlled_overlap: &|_| overlap,
            })
            .pop()
            .unwrap()
    }

    #[test]
    fn gaps_and_epochs_cannot_corroborate_a_weak_detection() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        let smoke = observation(ObservationLabel::Smoke, 0.3, 40.0);
        for (source, ts) in [
            (scope, 0),
            (scope, 30_000_000),
            (SourceScope { epoch: 2, ..scope }, 0),
        ] {
            let event = sample(
                &mut policy,
                source,
                ts,
                smoke.clone(),
                Visibility::Good,
                None,
            );
            assert_eq!(event.decision, FireDecision::Observing);
            assert_eq!(event.facts.continuity_samples, 1);
        }
    }

    #[test]
    fn visible_support_interrupts_observed_absence_without_confirming_a_fire() {
        let scope = scope();
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let smoke = observation(ObservationLabel::Smoke, 0.32, 40.0);
        let weak = observation(ObservationLabel::Smoke, 0.16, 40.0);
        let mut update = |at, regions, weak_proposals| {
            policy.update(FireSample {
                scope,
                source_us: at,
                dimensions: (200, 150),
                regions,
                weak_proposals,
                visibility: Visibility::Good,
                region_visibility: &|_, _| Visibility::Good,
                controlled_overlap: &|_| None,
            })
        };
        let first = update(0, std::slice::from_ref(&smoke), &[]).pop().unwrap();
        assert_eq!(first.decision, FireDecision::Observing);
        assert!(update(250_000, &[], &[]).is_empty());
        assert!(update(500_000, &[], std::slice::from_ref(&weak)).is_empty());
        assert!(update(800_000, &[], &[]).is_empty());
        assert!(update(1_050_000, &[], std::slice::from_ref(&weak)).is_empty());
        let next = update(1_300_000, std::slice::from_ref(&smoke), &[])
            .pop()
            .unwrap();
        assert_eq!(next.id, first.id);
        assert_eq!(next.facts.continuity_samples, 2);
        assert_eq!(next.decision, FireDecision::Review);
    }

    #[test]
    fn resolved_fire_requires_visible_quiet_in_its_own_region() {
        let scope = scope();
        let flame = observation(ObservationLabel::Fire, 0.95, 40.0);
        let weak = observation(ObservationLabel::Fire, 0.16, 40.0);
        let mut elsewhere = weak.clone();
        elsewhere.region.x1 += 100.0;
        elsewhere.region.x2 += 100.0;
        for (proposals, visibility, new_episode) in [
            (std::slice::from_ref(&weak), Visibility::Good, false),
            (&[][..], Visibility::Poor, false),
            (std::slice::from_ref(&elsewhere), Visibility::Good, true),
        ] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let first = sample(&mut policy, scope, 0, flame.clone(), Visibility::Good, None);
            assert_eq!(first.decision, FireDecision::Review);
            policy.record_resolution(first.id);
            for at in (1..=8).map(|slot| slot * 500_000) {
                assert!(
                    policy
                        .update(FireSample {
                            scope,
                            source_us: at,
                            dimensions: (200, 150),
                            regions: &[],
                            weak_proposals: proposals,
                            visibility: Visibility::Good,
                            region_visibility: &|_, _| visibility,
                            controlled_overlap: &|_| None,
                        })
                        .is_empty()
                );
            }
            let returned = sample(
                &mut policy,
                scope,
                4_500_000,
                flame.clone(),
                Visibility::Good,
                None,
            );
            assert_eq!(returned.id != first.id, new_episode);
            assert_eq!(returned.decision == FireDecision::Review, new_episode);
        }
    }

    #[test]
    fn a_brief_visibility_change_preserves_established_quiet_for_reignition() {
        for (return_us, intermediate_samples, new_episode) in [
            (5_000_000, 1, true),
            (6_000_000, 3, false),
            (6_000_000, 0, false),
        ] {
            let scope = scope();
            let flame = observation(ObservationLabel::Fire, 0.95, 40.0);
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let first = sample(&mut policy, scope, 0, flame.clone(), Visibility::Good, None);
            policy.record_resolution(first.id);
            for slot in 1..=8 + intermediate_samples {
                let visibility = if slot <= 8 {
                    Visibility::Good
                } else {
                    Visibility::Limited
                };
                policy.update(FireSample {
                    scope,
                    source_us: slot * 500_000,
                    dimensions: (200, 150),
                    regions: &[],
                    weak_proposals: &[],
                    visibility: Visibility::Good,
                    region_visibility: &|_, _| visibility,
                    controlled_overlap: &|_| None,
                });
            }
            let returned = sample(
                &mut policy,
                scope,
                return_us,
                flame.clone(),
                Visibility::Good,
                None,
            );
            let sustained = sample(
                &mut policy,
                scope,
                return_us + 500_000,
                flame,
                Visibility::Good,
                None,
            );
            assert_eq!(returned.id != first.id, new_episode);
            assert_eq!(returned.decision == FireDecision::Review, new_episode);
            assert_eq!(sustained.id, returned.id);
        }
    }

    #[test]
    fn dark_frame_preserves_only_the_regions_with_visible_support() {
        let scope = scope();
        let smoke = observation(ObservationLabel::Smoke, 0.32, 40.0);
        let weak = observation(ObservationLabel::Smoke, 0.16, 40.0);
        let mut other = observation(ObservationLabel::Fire, 0.32, 180.0);
        other.region.x1 = 150.0;
        for (other_visible, weak_visible) in [(true, false), (false, true), (false, false)] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let mut update = |at, regions, weak_proposals, visibility| {
                policy.update(FireSample {
                    scope,
                    source_us: at,
                    dimensions: (200, 150),
                    regions,
                    weak_proposals,
                    visibility,
                    region_visibility: &|_, _| Visibility::Limited,
                    controlled_overlap: &|_| None,
                })
            };
            let first = update(0, std::slice::from_ref(&smoke), &[], Visibility::Good);
            assert_eq!(first[0].decision, FireDecision::Observing);
            let regions = if other_visible {
                std::slice::from_ref(&other)
            } else {
                &[]
            };
            let proposals = if weak_visible {
                std::slice::from_ref(&weak)
            } else {
                &[]
            };
            let middle = update(500_000, regions, proposals, Visibility::Poor);
            assert_eq!(middle.len(), usize::from(other_visible));
            let next = update(
                1_000_000,
                std::slice::from_ref(&smoke),
                &[],
                Visibility::Good,
            );
            assert_eq!(
                next[0].facts.continuity_samples,
                if weak_visible { 2 } else { 1 }
            );
            assert_eq!(
                next[0].decision,
                if weak_visible {
                    FireDecision::Review
                } else {
                    FireDecision::Observing
                }
            );
        }
    }

    #[test]
    fn supported_pause_and_observed_absence_have_distinct_fire_outcomes() {
        let scope = scope();
        let smoke = observation(ObservationLabel::Smoke, 0.32, 40.0);
        let weak = observation(ObservationLabel::Smoke, 0.16, 40.0);
        for retain in [false, true] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let mut update = |at, regions, weak_proposals| {
                policy.update(FireSample {
                    scope,
                    source_us: at,
                    dimensions: (200, 150),
                    regions,
                    weak_proposals,
                    visibility: Visibility::Good,
                    region_visibility: &|_, _| Visibility::Good,
                    controlled_overlap: &|_| None,
                })
            };
            assert!(update(0, &[], std::slice::from_ref(&weak)).is_empty());
            let initial = update(100_000, std::slice::from_ref(&smoke), &[])
                .pop()
                .unwrap();
            assert_eq!(initial.decision, FireDecision::Observing);
            for at in [400_000, 800_000, 1_200_000] {
                let support = if retain {
                    std::slice::from_ref(&weak)
                } else {
                    &[]
                };
                assert!(update(at, &[], support).is_empty());
            }
            let next = update(1_400_000, std::slice::from_ref(&smoke), &[])
                .pop()
                .unwrap();
            assert_eq!(next.facts.continuity_samples, if retain { 2 } else { 1 });
            assert_eq!(
                next.decision,
                if retain {
                    FireDecision::Review
                } else {
                    FireDecision::Observing
                }
            );
        }
    }

    #[test]
    fn split_merge_and_dropout_keep_a_review_episode_with_fresh_segments() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        let wide = observation(ObservationLabel::Fire, 0.95, 100.0);
        let left = observation(ObservationLabel::Fire, 0.95, 40.0);
        let mut right = left.clone();
        right.region.x1 = 65.0;
        right.region.x2 = 100.0;
        let mut assess = |at, regions: &[Observation]| {
            policy.update(FireSample {
                scope,
                source_us: at,
                dimensions: (200, 150),
                regions,
                weak_proposals: &[],
                visibility: Visibility::Good,
                region_visibility: &|_, _| Visibility::Good,
                controlled_overlap: &|_| None,
            })
        };
        let original = assess(0, std::slice::from_ref(&wide)).pop().unwrap();
        assert_ne!(original.id, original.track_id);
        assert_eq!(original.decision, FireDecision::Review);
        let split = assess(500_000, &[left, right]);
        assert_eq!(split.len(), 2);
        assert_ne!(split[0].track_id, split[1].track_id);
        assert!(split.iter().all(|part| part.id == original.id));
        assert!(
            split
                .iter()
                .any(|part| part.association == FireAssociation::Split)
        );
        let merged = assess(1_000_000, std::slice::from_ref(&wide))
            .pop()
            .unwrap();
        assert_eq!(merged.id, original.id);
        for at in (2..=9).map(|at| at * 1_000_000) {
            assert!(assess(at, &[]).is_empty());
        }
        let mut smoke = wide;
        smoke.label = ObservationLabel::Smoke;
        let reobserved = assess(9_100_000, &[smoke]).pop().unwrap();
        assert_eq!(reobserved.id, original.id);
        assert_ne!(reobserved.track_id, original.track_id);
        assert_eq!(reobserved.association, FireAssociation::Reobserved);
        assert_eq!(reobserved.first_source_us, 0);
        assert_eq!(reobserved.segment_start_source_us, 9_100_000);
        assert_eq!(reobserved.facts.continuity_samples, 1);
    }

    #[test]
    fn escape_of_one_part_rearms_the_whole_resolved_episode_once() {
        let scope = scope();
        let assess = |policy: &mut FirePolicy, source_us, regions: &[Observation]| {
            policy.update(FireSample {
                scope,
                source_us,
                dimensions: (200, 150),
                regions,
                weak_proposals: &[],
                visibility: Visibility::Good,
                region_visibility: &|_, _| Visibility::Good,
                controlled_overlap: &|b| Some((100.0_f64.min(b.x2) - b.x1) / (b.x2 - b.x1)),
            })
        };
        for escape_first in [false, true] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let smoke = observation(ObservationLabel::Smoke, 0.95, 100.0);
            let first = assess(&mut policy, 0, &[smoke]).pop().unwrap();
            assert_eq!(first.decision, FireDecision::Review);
            let mut parts = [
                observation(ObservationLabel::Fire, 0.95, 40.0),
                observation(ObservationLabel::Fire, 0.95, 100.0),
            ];
            parts[1].region.x1 = 65.0;
            let split = assess(&mut policy, 500_000, &parts);
            assert_eq!(split.len(), 2);
            assert!(split.iter().all(|part| part.id == first.id));
            policy.record_resolution(first.id);
            parts[1].region.x2 = 120.0;
            if escape_first {
                parts.reverse();
            }
            let next = assess(&mut policy, 1_000_000, &parts);
            assert_eq!(next.len(), 2);
            let escaped = next
                .iter()
                .find(|part| part.facts.escaped_controlled_region)
                .unwrap();
            assert_eq!(escaped.decision, FireDecision::Review);
            assert_ne!(escaped.id, first.id);
            assert!(!policy.contains_episode(first.id));
            assert!(next.iter().all(|part| part.id == escaped.id
                && part.first_source_us == 1_000_000
                && part.segment_start_source_us == 1_000_000));
            let continued = assess(&mut policy, 1_500_000, &parts);
            assert!(continued.iter().all(|part| part.id == escaped.id
                && part.segment_start_source_us == 1_000_000
                && part.facts.continuity_samples == 2));
        }
    }

    #[test]
    fn locally_visible_reignition_requires_quiet_observed_before_darkness() {
        let flame = observation(ObservationLabel::Fire, 0.95, 40.0);
        let scope = scope();
        use Visibility::{Good, Limited, Poor};
        for (quiet_samples, blind_gap, overview, local, rearm) in [
            (7, false, Poor, Limited, true),
            (6, false, Poor, Limited, false),
            (7, true, Poor, Limited, false),
            (7, false, Limited, Limited, true),
            (6, false, Good, Good, false),
            (7, false, Good, Good, true),
        ] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let first = sample(&mut policy, scope, 0, flame.clone(), Visibility::Good, None);
            policy.record_resolution(first.id);
            let mut at = 0;
            let mut update = |regions: &[Observation], overview, local| {
                at += 500_000;
                policy.update(FireSample {
                    scope,
                    source_us: at,
                    dimensions: (200, 150),
                    regions,
                    weak_proposals: &[],
                    visibility: overview,
                    region_visibility: &|_, _| local,
                    controlled_overlap: &|_| None,
                })
            };
            for _ in 0..quiet_samples {
                assert!(update(&[], Visibility::Good, Visibility::Good).is_empty());
            }
            if blind_gap {
                assert!(update(&[], Visibility::Poor, Visibility::Poor).is_empty());
            }
            let mut observe_flame = || {
                update(std::slice::from_ref(&flame), overview, local)
                    .pop()
                    .unwrap()
            };
            let returned = observe_flame();
            assert_eq!(returned.facts.visibility, local);
            assert_eq!(returned.decision == FireDecision::Review, rearm);
            assert_eq!(returned.id != first.id, rearm);
            assert_eq!(observe_flame().id, returned.id);
        }
    }

    #[test]
    fn locally_visible_flame_can_open_a_limited_review_in_a_dark_scene() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        let flame = observation(ObservationLabel::Fire, 0.95, 40.0);
        let sample = |at, roi| FireSample {
            scope,
            source_us: at,
            dimensions: (200, 150),
            regions: std::slice::from_ref(&flame),
            weak_proposals: &[],
            visibility: Visibility::Poor,
            region_visibility: roi,
            controlled_overlap: &|_| None,
        };
        assert!(
            policy
                .update(sample(0, &|_, _| Visibility::Poor))
                .is_empty()
        );
        let visible = policy.update(sample(500_000, &|_, _| Visibility::Limited));
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].decision, FireDecision::Review);
        assert_eq!(visible[0].facts.visibility, Visibility::Limited);
        assert_eq!(visible[0].facts.continuity_samples, 1);
    }

    #[test]
    fn nearby_merge_preserves_independent_fires_and_resolution_needs_a_new_onset() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        let left = observation(ObservationLabel::Fire, 0.95, 30.0);
        let mut right = left.clone();
        right.region.x1 = 80.0;
        right.region.x2 = 100.0;
        let mut merged = left.clone();
        merged.region.x2 = 100.0;
        let assess = |policy: &mut FirePolicy, at, regions: &[Observation]| {
            policy.update(FireSample {
                scope,
                source_us: at,
                dimensions: (200, 150),
                regions,
                weak_proposals: &[],
                visibility: Visibility::Good,
                region_visibility: &|_, _| Visibility::Good,
                controlled_overlap: &|_| None,
            })
        };
        let separate = assess(&mut policy, 0, &[left.clone(), right.clone()]);
        assert_eq!(separate.len(), 2);
        assert_ne!(separate[0].id, separate[1].id);
        let merged = assess(&mut policy, 500_000, &[merged]).pop().unwrap();
        assert_eq!(merged.association, FireAssociation::Ambiguous);
        assert_eq!(merged.decision, FireDecision::Observing);
        let separated = assess(&mut policy, 1_000_000, &[left.clone(), right]);
        assert_eq!(separated[0].id, separate[0].id);
        assert_eq!(separated[1].id, separate[1].id);

        policy.record_resolution(separate[0].id);
        let stationary = assess(&mut policy, 1_500_000, std::slice::from_ref(&left))
            .pop()
            .unwrap();
        assert_eq!(stationary.id, separate[0].id);
        assert_eq!(stationary.decision, FireDecision::Observing);
        for at in (2..=5).map(|at| at * 1_000_000) {
            assess(&mut policy, at, &[]);
        }
        let reignition = assess(&mut policy, 5_500_000, std::slice::from_ref(&left))
            .pop()
            .unwrap();
        assert_ne!(reignition.id, separate[0].id);
        assert_eq!(reignition.decision, FireDecision::Review);
        let continued = assess(&mut policy, 6_000_000, &[left]).pop().unwrap();
        assert_eq!(continued.id, reignition.id);
    }

    #[test]
    fn a_brief_merge_preserves_quiet_only_for_resolved_episodes() {
        let scope = scope();
        let left = observation(ObservationLabel::Fire, 0.95, 30.0);
        let mut right = left.clone();
        right.region.x1 = 80.0;
        right.region.x2 = 100.0;
        let mut merged = left.clone();
        merged.region.x2 = 100.0;
        for (right_resolved, quiet_samples, merged_samples, rearm) in [
            (true, 7, 1, true),
            (false, 7, 1, false),
            (true, 6, 1, false),
            (true, 7, 4, false),
        ] {
            let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
            let update = |policy: &mut FirePolicy, at, regions: &[Observation]| {
                policy.update(FireSample {
                    scope,
                    source_us: at,
                    dimensions: (200, 150),
                    regions,
                    weak_proposals: &[],
                    visibility: Visibility::Good,
                    region_visibility: &|_, _| Visibility::Good,
                    controlled_overlap: &|_| None,
                })
            };
            let initial = update(&mut policy, 0, &[left.clone(), right.clone()]);
            assert_eq!(initial.len(), 2);
            assert!(
                initial
                    .iter()
                    .all(|episode| episode.decision == FireDecision::Review)
            );
            assert_ne!(initial[0].id, initial[1].id);
            policy.record_resolution(initial[0].id);
            if right_resolved {
                policy.record_resolution(initial[1].id);
            }
            for tick in 1..=quiet_samples {
                assert!(update(&mut policy, tick * 500_000, &[]).is_empty());
            }
            for tick in 1..=merged_samples {
                let uncertain = update(
                    &mut policy,
                    (quiet_samples + tick) * 500_000,
                    std::slice::from_ref(&merged),
                )
                .pop()
                .unwrap();
                assert_eq!(uncertain.association, FireAssociation::Ambiguous);
                assert_eq!(uncertain.decision, FireDecision::Observing);
            }
            let returned = update(
                &mut policy,
                (quiet_samples + merged_samples + 1) * 500_000,
                std::slice::from_ref(&left),
            )
            .pop()
            .unwrap();
            assert_eq!(returned.id != initial[0].id, rearm);
            assert_eq!(returned.decision == FireDecision::Review, rearm);
        }
    }

    #[test]
    fn controlled_context_holds_attention_until_the_flame_expands() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        let flame = observation(ObservationLabel::Fire, 0.95, 40.0);
        let first = sample(
            &mut policy,
            scope,
            0,
            flame.clone(),
            Visibility::Good,
            Some(1.0),
        );
        assert_eq!(first.decision, FireDecision::Observing);
        let second = sample(
            &mut policy,
            scope,
            500_000,
            flame.clone(),
            Visibility::Good,
            Some(1.0),
        );
        assert_eq!(second.decision, FireDecision::ControlledContext);
        let escaped = sample(
            &mut policy,
            scope,
            1_000_000,
            observation(ObservationLabel::Fire, 0.4, 62.0),
            Visibility::Good,
            Some(0.57),
        );
        assert_eq!(escaped.id, second.id);
        assert_eq!(escaped.decision, FireDecision::Review);
        assert!(escaped.facts.escaped_controlled_region);
        let obscured = policy.update(FireSample {
            scope,
            source_us: 1_500_000,
            dimensions: (200, 150),
            regions: &[flame],
            weak_proposals: &[],
            visibility: Visibility::Poor,
            region_visibility: &|_, _| Visibility::Poor,
            controlled_overlap: &|_| Some(1.0),
        });
        assert!(obscured.is_empty());
    }

    #[test]
    fn configured_confidence_and_robust_growth_are_separate() {
        let mut policy = FirePolicy::new(FireSettings {
            immediate_confidence: 0.9,
            ..FireSettings::default()
        })
        .unwrap();
        let scope = scope();
        let first = sample(
            &mut policy,
            scope,
            0,
            observation(ObservationLabel::Fire, 0.8, 32.0),
            Visibility::Good,
            None,
        );
        assert_eq!(first.decision, FireDecision::Observing);
        assert!(!first.facts.strong_detection);
        for (i, right) in [32.0, 52.0, 52.0].into_iter().enumerate() {
            let event = sample(
                &mut policy,
                scope,
                (i as i64 + 1) * 500_000,
                observation(ObservationLabel::Fire, 0.4, right),
                Visibility::Good,
                None,
            );
            assert_eq!(event.decision, FireDecision::Review);
            assert_eq!(event.facts.apparent_growth, i == 2);
        }
    }

    #[test]
    fn missing_class_is_not_a_smaller_box_for_a_growth_comparison() {
        let mut policy = FirePolicy::new(FireSettings::default()).unwrap();
        let scope = scope();
        for (i, label) in [
            ObservationLabel::Smoke,
            ObservationLabel::Smoke,
            ObservationLabel::Fire,
            ObservationLabel::Fire,
        ]
        .into_iter()
        .enumerate()
        {
            let event = sample(
                &mut policy,
                scope,
                i as i64 * 500_000,
                observation(label, 0.45, 40.0),
                Visibility::Good,
                Some(1.0),
            );
            assert!(!event.facts.apparent_growth);
            assert!(!event.facts.smoke_growth);
            if i == 3 {
                assert_eq!(event.decision, FireDecision::ControlledContext);
            }
        }
    }

    #[test]
    fn growth_uses_recent_measurements_of_the_current_class() {
        let history = [Some(0.01), Some(0.01), None, Some(0.02), Some(0.022), None]
            .into_iter()
            .enumerate()
            .map(|(index, flame_area)| Measurement {
                at: index as i64 * 500_000,
                flame_area,
                smoke_area: Some(0.01),
            })
            .collect::<VecDeque<_>>();
        assert!(!grows(&history, |sample| sample.flame_area));
        let mut visible = history;
        visible.pop_back();
        assert!(grows(&visible, |sample| sample.flame_area));
        visible[3].at = 2_500_000;
        visible[4].at = 3_000_000;
        assert!(!grows(&visible, |sample| sample.flame_area));
    }
}

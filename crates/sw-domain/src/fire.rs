use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Box2d, Observation, ObservationLabel, SourceScope};

const MAX_REGIONS: usize = 64;
const CONTINUITY_US: i64 = 1_500_000;
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
    pub visibility: Visibility,
    /// Fraction of the current flame box inside a reviewed region.
    /// None represents missing or invalidated scene context.
    pub controlled_overlap: &'a dyn Fn(Box2d) -> Option<f64>,
}

pub struct FireEpisode {
    pub id: Uuid,
    pub first_source_us: i64,
    pub source_us: i64,
    pub region: Box2d,
    pub decision: FireDecision,
    pub facts: FireFacts,
}

pub struct FirePolicy {
    settings: FireSettings,
    scope: Option<SourceScope>,
    last_us: Option<i64>,
    dimensions: (u32, u32),
    regions: Vec<Region>,
}

struct Region {
    id: Uuid,
    started_us: i64,
    last_us: i64,
    region: Box2d,
    history: VecDeque<Measurement>,
    was_contained: bool,
}

#[derive(Clone, Copy)]
struct Measurement {
    at: i64,
    flame_area: f64,
    smoke_area: f64,
}

struct Cluster {
    flame: Option<Box2d>,
    smoke: Option<Box2d>,
    combined: Box2d,
    confidence: f64,
}

impl FirePolicy {
    pub fn contains_episode(&self, id: Uuid) -> bool {
        self.regions.iter().any(|region| region.id == id)
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
        }
        let gap = !same_scene
            || self
                .last_us
                .is_none_or(|last| sample.source_us.saturating_sub(last) > CONTINUITY_US);
        self.last_us = Some(sample.source_us);
        self.regions
            .retain(|old| sample.source_us.saturating_sub(old.last_us) <= MAX_QUIET_US);
        if gap || sample.visibility == Visibility::Poor {
            for old in &mut self.regions {
                old.history.clear();
                old.was_contained = false;
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
            if let Some(cluster) = clusters
                .iter_mut()
                .find(|c| adjacent(c.combined, observed.region))
            {
                cluster.add(observed);
            } else {
                let mut cluster = Cluster {
                    flame: None,
                    smoke: None,
                    combined: observed.region,
                    confidence: observed.score,
                };
                cluster.add(observed);
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
                &mut self.regions[index]
            } else if self.regions.len() < MAX_REGIONS {
                self.regions.push(Region {
                    id: Uuid::new_v4(),
                    started_us: sample.source_us,
                    last_us: sample.source_us,
                    region: cluster.combined,
                    history: VecDeque::new(),
                    was_contained: false,
                });
                used.push(true);
                self.regions.last_mut().expect("new fire region")
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
                    started_us: sample.source_us,
                    last_us: sample.source_us,
                    region: cluster.combined,
                    history: VecDeque::new(),
                    was_contained: false,
                };
                &mut self.regions[index]
            };
            reports.push(track.observe(&sample, cluster, frame_area, self.settings));
        }
        reports
    }
}

impl Cluster {
    fn add(&mut self, observation: &Observation) {
        let part = if observation.label == ObservationLabel::Fire {
            &mut self.flame
        } else {
            &mut self.smoke
        };
        *part = Some(part.map_or(observation.region, |old| old.union(observation.region)));
        self.combined = self.combined.union(observation.region);
        self.confidence = self.confidence.max(observation.score);
    }
}

impl Region {
    fn observe(
        &mut self,
        sample: &FireSample<'_>,
        cluster: Cluster,
        frame_area: f64,
        settings: FireSettings,
    ) -> FireEpisode {
        let at = sample.source_us;
        if at.saturating_sub(self.last_us) > CONTINUITY_US {
            self.history.clear();
            self.was_contained = false;
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
        if sample.visibility != Visibility::Poor {
            self.history.push_back(Measurement {
                at,
                flame_area: cluster.flame.map_or(0.0, Box2d::area) / frame_area,
                smoke_area: cluster.smoke.map_or(0.0, Box2d::area) / frame_area,
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
            visibility: sample.visibility,
        };
        let corroborated = self
            .history
            .front()
            .is_some_and(|first| at.saturating_sub(first.at) >= 300_000)
            && facts.continuity_samples >= 2;
        let controlled = contained
            && sample.visibility == Visibility::Good
            && !facts.smoke_visible
            && !facts.apparent_growth
            && corroborated;
        let decision = if controlled {
            FireDecision::ControlledContext
        } else if sample.visibility != Visibility::Poor
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
            first_source_us: self.started_us,
            source_us: at,
            region: cluster.combined,
            decision,
            facts,
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

fn grows(history: &VecDeque<Measurement>, area: impl Fn(&Measurement) -> f64) -> bool {
    let count = history.len();
    if count < 4
        || history
            .back()
            .expect("last measurement")
            .at
            .saturating_sub(history[0].at)
            < 1_200_000
    {
        return false;
    }
    let median = |mut values: Vec<f64>| {
        values.sort_unstable_by(f64::total_cmp);
        (values[(values.len() - 1) / 2] + values[values.len() / 2]) * 0.5
    };
    let early = median(history.iter().take(count / 2).map(&area).collect());
    let late = median(history.iter().skip(count / 2).map(area).collect());
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
                visibility: visible,
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
        let obscured = sample(
            &mut policy,
            scope,
            1_500_000,
            flame,
            Visibility::Poor,
            Some(1.0),
        );
        assert_eq!(obscured.decision, FireDecision::Observing);
        assert_eq!(obscured.facts.continuity_samples, 0);
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
}

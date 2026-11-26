use std::{collections::VecDeque, sync::Arc};

use crate::{Keypoint, MedicalSettings, Observation, Posture};

const EPSILON: f64 = 1e-6;
const LEGS: [[usize; 3]; 2] = [[11, 13, 15], [12, 14, 16]];
const BODY: [[usize; 4]; 2] = [[5, 11, 13, 15], [6, 12, 14, 16]];

#[derive(Clone, Copy)]
struct Vector(f64, f64);

impl Vector {
    fn point(point: Keypoint) -> Self {
        Self(point.0, point.1)
    }

    fn length(self) -> f64 {
        self.0.hypot(self.1)
    }

    fn difference(self, other: Self) -> Self {
        Self(self.0 - other.0, self.1 - other.1)
    }

    fn divided(self, divisor: f64) -> Self {
        Self(self.0 / divisor, self.1 / divisor)
    }

    fn dot(self, other: Self) -> f64 {
        self.0 * other.0 + self.1 * other.1
    }
}

fn mean(points: impl Iterator<Item = Vector>) -> Option<Vector> {
    let (x, y, n) = points.fold((0.0, 0.0, 0), |(x, y, n), p| (x + p.0, y + p.1, n + 1));
    (n > 0).then(|| Vector(x, y).divided(f64::from(n)))
}

fn median(mut numbers: Vec<f64>) -> f64 {
    numbers.sort_unstable_by(f64::total_cmp);
    let n = numbers.len();
    if n.is_multiple_of(2) {
        (numbers[n / 2 - 1] + numbers[n / 2]) * 0.5
    } else {
        numbers[n / 2]
    }
}

fn median_vector(vectors: &[Vector]) -> Vector {
    Vector(
        median(vectors.iter().map(|p| p.0).collect()),
        median(vectors.iter().map(|p| p.1).collect()),
    )
}

fn point(obs: &Observation, index: usize, minimum: f64) -> Option<Keypoint> {
    obs.keypoints.get(index).copied().filter(|p| p.2 >= minimum)
}

fn shared(before: &Observation, after: &Observation, index: usize, minimum: f64) -> Option<Vector> {
    let old = Vector::point(point(before, index, minimum)?);
    let new = Vector::point(point(after, index, minimum)?);
    Some(new.difference(old))
}

fn supported_leg(
    obs: &Observation,
    chain: [usize; 3],
    confidence: f64,
) -> Option<(Keypoint, Keypoint, Keypoint)> {
    Some((
        point(obs, chain[0], confidence)?,
        point(obs, chain[1], confidence)?,
        point(obs, chain[2], confidence)?,
    ))
}

fn leg_geometry((hip, knee, ankle): (Keypoint, Keypoint, Keypoint)) -> (f64, f64, f64) {
    let thigh = Vector::point(hip).difference(Vector::point(knee));
    let calf = Vector::point(ankle).difference(Vector::point(knee));
    let lengths = (thigh.length(), calf.length());
    (
        lengths.0.min(lengths.1),
        lengths.0 + lengths.1,
        thigh.dot(calf) / (lengths.0 * lengths.1),
    )
}

#[derive(Clone)]
pub(crate) struct Pose {
    pub observation: Arc<Observation>,
    pub center_y: f64,
    pub lean: Option<f64>,
    pub low: bool,
    pub posture: Posture,
    pub reliable: bool,
    pub torso_visible: bool,
    pub legs_visible: bool,
    pub body_visible: bool,
    pub shoulder_y: Option<f64>,
    pub torso_length: f64,
}

impl Pose {
    pub fn measure(observation: Observation, settings: MedicalSettings) -> Self {
        let landmark = |index| point(&observation, index, settings.keypoint_confidence);
        let midpoint =
            |indices: [usize; 2]| mean(indices.into_iter().filter_map(landmark).map(Vector::point));
        let shoulder = midpoint([5, 6]);
        let hip = midpoint([11, 12]);
        let axis = shoulder.zip(hip).map(|(s, h)| h.difference(s));
        let lean = axis.map(|v| v.0.abs() / v.1.abs().max(1.0));
        let torso_length = axis.map_or(0.0, Vector::length);
        let region = observation.region;
        let center_y = shoulder
            .zip(hip)
            .map_or(region.center().1, |(s, h)| (s.1 + h.1) * 0.5);
        let aspect = region.width() / region.height().max(1.0);
        let legs_visible = (13..=16).any(|k| landmark(k).is_some());
        let ankles = [15, 16].into_iter().filter_map(landmark);
        let extended_below = hip.is_some_and(|h| {
            ankles
                .clone()
                .map(|p| p.1)
                .min_by(f64::total_cmp)
                .is_some_and(|y| y - h.1 > torso_length * 0.8)
        });
        let horizontal = lean.is_some_and(|value| value > 1.4) && torso_length >= 10.0;
        let inverted = axis.is_some_and(|v| -v.1 > (torso_length * 0.25).max(8.0));
        let low = (horizontal
            && (aspect >= 0.85 || !extended_below)
            && (legs_visible || !observation.truncated))
            || (aspect >= settings.low_aspect_ratio
                && legs_visible
                && lean.is_none_or(|v| v >= 0.7))
            || (inverted && legs_visible && !extended_below)
            || (aspect >= settings.low_aspect_ratio
                && !observation.truncated
                && shoulder.is_none()
                && hip.is_none());
        let mut measured_legs = 0;
        let mut standing = false;
        for chain in LEGS {
            let Some((a, b, c)) = supported_leg(&observation, chain, settings.keypoint_confidence)
            else {
                continue;
            };
            let thigh = a.distance(b);
            let calf = c.distance(b);
            if thigh * calf > 1.0 {
                measured_legs += 1;
                let (_, length, cosine) = leg_geometry((a, b, c));
                let angle = cosine.clamp(-1.0, 1.0).acos().to_degrees();
                standing |= angle >= 125.0 && c.1 - a.1 >= (torso_length * 0.8).max(length * 0.6);
            }
        }
        let torso_visible = !observation.limited && axis.is_some();
        let reliable = torso_visible
            && observation.track_id.is_some()
            && measured_legs > 0
            && torso_length >= (region.height() * 0.12).max(8.0);
        let leg_extent = hip.map_or(0.0, |h| {
            ankles
                .map(|p| p.1 - h.1)
                .max_by(f64::total_cmp)
                .unwrap_or(0.0)
        });
        let posture = if low {
            Posture::Low
        } else if reliable
            && lean.is_some_and(|v| v < 0.7)
            && axis.is_some_and(|v| v.1 > (torso_length * 0.6).max(6.0))
            && leg_extent <= torso_length * 2.6
        {
            if standing {
                Posture::Upright
            } else {
                Posture::Seated
            }
        } else {
            Posture::Uncertain
        };
        let body_visible = torso_visible
            && BODY.into_iter().any(|chain| {
                chain
                    .iter()
                    .all(|&k| landmark(k).is_some_and(|p| p.2 >= 0.65))
                    && observation.keypoints[chain[0]].distance(observation.keypoints[chain[3]])
                        > region.height() * 0.55
            });
        Self {
            observation: Arc::new(observation),
            center_y,
            lean,
            low,
            posture,
            reliable,
            torso_visible,
            legs_visible,
            body_visible,
            shoulder_y: shoulder.map(|s| s.1),
            torso_length,
        }
    }

    pub fn ts(&self) -> f64 {
        self.observation.capture_ts
    }

    pub fn height(&self) -> f64 {
        self.observation.region.height().max(1.0)
    }

    pub fn has_torso_reference(&self) -> bool {
        self.observation.keypoints.len() >= 13 && self.lean.is_some()
    }
}

pub(crate) fn displacement(
    before: &Pose,
    after: &Pose,
    settings: MedicalSettings,
) -> (Option<f64>, Option<f64>) {
    let group = |joints: [usize; 2]| {
        let (sum, count) = joints
            .into_iter()
            .filter_map(|k| {
                shared(
                    &before.observation,
                    &after.observation,
                    k,
                    settings.keypoint_confidence,
                )
            })
            .fold((0.0, 0), |(sum, count), v| (sum + v.1, count + 1));
        (
            (count > 0).then(|| sum / f64::from(count) / before.height()),
            count,
        )
    };
    let (shoulder, shoulders) = group([5, 6]);
    let (hip, hips) = group([11, 12]);
    if let Some(y) = shoulder.filter(|_| shoulders + hips >= 2) {
        (Some(hip.map_or(y, |h| (y + h) * 0.5)), shoulder)
    } else if before.observation.keypoints.is_empty() && after.observation.keypoints.is_empty() {
        let a = before.observation.region;
        let b = after.observation.region;
        (
            Some((b.y1 + b.y2 - a.y1 - a.y2) * 0.5 / before.height()),
            None,
        )
    } else {
        (None, None)
    }
}

pub(crate) fn standing_bend(before: &Pose, after: &Pose) -> bool {
    let old = &before.observation;
    let new = &after.observation;
    let dt = after.ts() - before.ts();
    if old.association_uncertain
        || new.association_uncertain
        || dt <= 0.0
        || dt > 5.0
        || before.posture != Posture::Upright
        || !before.reliable
        || !after.torso_visible
    {
        return false;
    }
    let height = before.height();
    LEGS.into_iter().any(|chain| {
        chain.into_iter().enumerate().all(|(i, k)| {
            shared(old, new, k, 0.65).is_some_and(|offset| {
                offset.1.abs() <= height * 0.08 && (i != 2 || offset.length() <= height * 0.08)
            })
        }) && [old, new].into_iter().all(|obs| {
            let Some(leg) = supported_leg(obs, chain, 0.65) else {
                return false;
            };
            let (shortest, length, cosine) = leg_geometry(leg);
            shortest >= height * 0.1
                && cosine <= 150_f64.to_radians().cos()
                && leg.2.1 - leg.0.1 >= (height * 0.35).max(length * 0.85)
        })
    })
}

pub(crate) fn visible_ascent(before: &Pose, after: &Pose, window: f64) -> bool {
    let old = &before.observation;
    let new = &after.observation;
    let dt = after.ts() - before.ts();
    let height = before.height();
    if dt <= 0.0
        || dt > window + EPSILON
        || old.limited
        || old.association_uncertain
        || new.association_uncertain
        || after.height() < height * 1.2
        || old.keypoints.len().min(new.keypoints.len()) < 17
    {
        return false;
    }
    if [(5, 0.1), (6, 0.1), (11, 0.05), (12, 0.05)]
        .into_iter()
        .any(|(k, fraction)| {
            shared(old, new, k, 0.65).is_none_or(|offset| -offset.1 < height * fraction)
        })
    {
        return false;
    }
    LEGS.into_iter().any(|chain| {
        let Some(old_leg) = supported_leg(old, chain, 0.65) else {
            return false;
        };
        let Some(new_leg) = supported_leg(new, chain, 0.65) else {
            return false;
        };
        let (old_shortest, _, old_cosine) = leg_geometry(old_leg);
        let (new_shortest, _, new_cosine) = leg_geometry(new_leg);
        old_shortest.min(new_shortest) >= (height * 0.08).max(3.0)
            && old_cosine >= 125_f64.to_radians().cos()
            && new_cosine <= 150_f64.to_radians().cos()
    })
}

pub(crate) fn cropped_translation(
    before: &Pose,
    after: &Pose,
    history: Option<&VecDeque<Pose>>,
    concern: bool,
    settings: MedicalSettings,
) -> bool {
    edge_translation(before, after, history, concern, settings)
        || (!concern && history.is_some_and(|past| continued_translation(before, after, past)))
}

fn edge_translation(
    before: &Pose,
    after: &Pose,
    history: Option<&VecDeque<Pose>>,
    concern: bool,
    settings: MedicalSettings,
) -> bool {
    let recent = |seconds| {
        history
            .into_iter()
            .flatten()
            .filter(move |p| (0.0..=seconds).contains(&(before.ts() - p.ts())))
    };
    let a = &before.observation;
    let b = &after.observation;
    let elapsed = after.ts() - before.ts();
    if !(a.truncated || b.truncated || recent(0.6 + EPSILON).any(|p| p.observation.truncated))
        || elapsed <= 0.0
        || elapsed > settings.lost_track_seconds + EPSILON
    {
        return false;
    }
    let old = if before.torso_visible {
        before
    } else {
        history
            .into_iter()
            .flatten()
            .filter(|p| {
                before.ts() - p.ts() <= 1.0
                    && p.torso_visible
                    && !p.observation.association_uncertain
            })
            .reduce(|best, p| {
                if p.torso_length > best.torso_length {
                    p
                } else {
                    best
                }
            })
            .unwrap_or(before)
    };
    if !old.torso_visible
        || old.lean.is_none_or(|v| v >= 0.7)
        || old.torso_length < 10.0
        || after.low
        || after.body_visible
        || after.lean.is_some_and(|v| v >= 0.7)
    {
        return false;
    }
    let head = |minimum| (0..5).find(|&k| shared(a, b, k, minimum).is_some());
    let strong = head(0.65);
    let (head_index, weak) = match strong {
        Some(k) => (k, false),
        None if history.is_some()
            && !concern
            && !a.association_uncertain
            && !b.association_uncertain =>
        {
            let Some(k) = head(settings.keypoint_confidence) else {
                return false;
            };
            (k, true)
        }
        None => return false,
    };
    if [5, 6].into_iter().any(|k| shared(a, b, k, 0.65).is_none()) {
        return false;
    }
    let offsets: Vec<_> = [head_index, 5, 6, 11, 12]
        .into_iter()
        .filter_map(|k| {
            shared(
                a,
                b,
                k,
                if k == head_index {
                    settings.keypoint_confidence.min(0.65)
                } else {
                    0.65
                },
            )
        })
        .collect();
    let center = mean(offsets.iter().copied()).expect("shared shoulders");
    let scales: Vec<_> = recent(0.6 + EPSILON)
        .filter(|p| {
            p.torso_visible
                && p.lean.is_some_and(|v| v < 0.7)
                && !p.observation.association_uncertain
        })
        .map(|p| p.torso_length)
        .collect();
    let scale = if scales.is_empty() {
        old.torso_length
    } else {
        old.torso_length.max(median(scales))
    };
    if weak || head_index != 0 {
        let past: Vec<_> = recent(0.8 + EPSILON).collect();
        if past.len() < 3
            || past[past.len() - 1].ts() - past[0].ts() < 0.4 - EPSILON
            || past.iter().any(|p| {
                p.low
                    || p.observation.association_uncertain
                    || [5, 6]
                        .into_iter()
                        .any(|k| point(&p.observation, k, 0.65).is_none())
            })
            || (weak
                && past
                    .iter()
                    .filter(|p| point(&p.observation, head_index, 0.65).is_some())
                    .count()
                    < 3)
        {
            return false;
        }
        let shoulders = |from: &Pose, to: &Pose| {
            mean(
                [5, 6]
                    .into_iter()
                    .filter_map(|k| shared(&from.observation, &to.observation, k, 0.65)),
            )
            .map(|offset| offset.divided(to.ts() - from.ts()))
        };
        let mut velocities = Vec::with_capacity(past.len() - 1);
        for pair in past.windows(2) {
            if !short_step(pair[0].ts(), pair[1].ts()) {
                return false;
            }
            let Some(velocity) = shoulders(pair[0], pair[1]) else {
                return false;
            };
            velocities.push(velocity);
        }
        if shoulders(before, after).is_none_or(|velocity| {
            !consistent_velocity(median_vector(&velocities), velocity, scale)
        }) {
            return false;
        }
    }
    center.length() / elapsed <= scale * 2.0
        && offsets
            .iter()
            .all(|p| p.difference(center).length() <= scale * 0.25)
}

fn continued_translation(before: &Pose, after: &Pose, history: &VecDeque<Pose>) -> bool {
    let interior = !after.observation.truncated;
    if after.observation.association_uncertain
        || (interior && before.observation.association_uncertain)
        || !short_step(before.ts(), after.ts())
        || after.low
        || after.body_visible
        || !before.torso_visible
        || before.torso_length < 12.0
        || before.lean.is_none_or(|v| v >= 0.7)
        || after.lean.is_none_or(|v| v >= 0.7)
    {
        return false;
    }
    let recent: Vec<_> = history
        .iter()
        .filter(|p| (0.0..=0.8 + EPSILON).contains(&(before.ts() - p.ts())))
        .collect();
    if recent.len() < 3
        || recent[recent.len() - 1].ts() - recent[0].ts() < 0.4 - EPSILON
        || !history.iter().any(|p| {
            (0.0..=2.0).contains(&(before.ts() - p.ts()))
                && p.body_visible
                && p.posture == Posture::Upright
        })
        || recent
            .iter()
            .any(|p| p.low || p.observation.association_uncertain)
        || recent.windows(2).any(|p| !short_step(p[0].ts(), p[1].ts()))
    {
        return false;
    }
    [(5, 11), (6, 12)].into_iter().any(|(shoulder, hip)| {
        let points: Option<Vec<_>> = recent
            .iter()
            .copied()
            .chain([after])
            .map(|p| {
                let s = point(&p.observation, shoulder, 0.65)?;
                let h = point(&p.observation, hip, if interior { 0.65 } else { 0.4 })?;
                let axis = Vector::point(h).difference(Vector::point(s));
                Some((p.ts(), axis, Vector((s.0 + h.0) * 0.5, (s.1 + h.1) * 0.5)))
            })
            .collect();
        let Some(points) = points else {
            return false;
        };
        let last = points.len() - 1;
        let scale = median(points[..last].iter().map(|p| p.1.length()).collect());
        let ratio = points[last].1.length() / scale;
        let axis_change = points[last].1.difference(points[last - 1].1).length();
        if scale < 12.0
            || points
                .iter()
                .any(|p| p.1.1 / p.1.length().max(1.0) < std::f64::consts::FRAC_1_SQRT_2)
            || !(0.75..=1.33).contains(&ratio)
            || axis_change > scale * 0.25
            || (interior && (!(0.8..=1.25).contains(&ratio) || axis_change > scale * 0.2))
        {
            return false;
        }
        let velocities: Vec<_> = points
            .windows(2)
            .map(|p| p[1].2.difference(p[0].2).divided(p[1].0 - p[0].0))
            .collect();
        let current = velocities[last - 1];
        let prior = median_vector(&velocities[..last - 1]);
        if interior
            && (prior.0.abs() < prior.1.abs().max(scale * 0.3)
                || current.0.abs() < current.1.abs()
                || current.length() > prior.length() * 1.5 + scale * 0.2
                || (current.1 - prior.1).abs() > scale * 0.3
                || prior.dot(current) < prior.length() * current.length() * 0.9)
        {
            return false;
        }
        consistent_velocity(prior, current, scale)
    })
}

fn short_step(before: f64, after: f64) -> bool {
    let elapsed = after - before;
    elapsed > 0.0 && elapsed <= 0.4 + EPSILON
}

fn consistent_velocity(prior: Vector, current: Vector, scale: f64) -> bool {
    let speed = prior.length();
    speed >= scale * 0.2
        && current.length() <= speed * 1.75 + scale * 0.2
        && prior.dot(current) >= speed * current.length() * 0.8
}

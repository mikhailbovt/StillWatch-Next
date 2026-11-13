use std::collections::BTreeMap;

use thiserror::Error;

use crate::{Keypoint, Observation, ObservationLabel};

const TRACK_LIMIT: usize = 128;
const TENTATIVE_RETENTION: f64 = 0.6 + 1e-6;
const COST_THRESHOLD: f64 = 0.55;
const TORSO: [usize; 4] = [5, 6, 11, 12];
const HEAD_TORSO: [usize; 5] = [0, 5, 6, 11, 12];

#[derive(Debug, Error)]
pub enum TrackingError {
    #[error("invalid pose observation")]
    InvalidObservation,
    #[error("track identity sequence exhausted")]
    Exhausted,
    #[error("assignment failed")]
    Assignment,
}

struct Track {
    current: Observation,
    previous: Option<Observation>,
    hits: u32,
    confirmed: bool,
    occluded: bool,
}

/// Source-time association. Each camera owns its tracker and resets it at an epoch change.
pub struct PoseTracker {
    tracks: BTreeMap<u64, Track>,
    next_id: u64,
    epoch: Option<u64>,
}

impl Default for PoseTracker {
    fn default() -> Self {
        Self {
            tracks: BTreeMap::new(),
            next_id: 1,
            epoch: None,
        }
    }
}

impl PoseTracker {
    pub fn reset(&mut self) {
        self.tracks.clear();
        self.next_id = 1;
        self.epoch = None;
    }

    pub fn update(
        &mut self,
        mut observations: Vec<Observation>,
        ts: f64,
        epoch: u64,
    ) -> Result<Vec<Observation>, TrackingError> {
        if !ts.is_finite()
            || observations.len() > 300
            || observations
                .iter()
                .any(|obs| !obs.valid() || obs.label != ObservationLabel::Person)
        {
            return Err(TrackingError::InvalidObservation);
        }
        if self.epoch != Some(epoch) {
            self.reset();
            self.epoch = Some(epoch);
        }
        self.tracks.retain(|_, track| {
            ts - track.current.capture_ts
                <= if track.confirmed {
                    3.0
                } else {
                    TENTATIVE_RETENTION
                }
        });
        observations.sort_by(|a, b| b.score.total_cmp(&a.score));
        let mut unique = Vec::with_capacity(observations.len().min(TRACK_LIMIT));
        for obs in observations.into_iter().take(TRACK_LIMIT) {
            if !unique.iter().any(|other| duplicate(&obs, other)) {
                unique.push(obs);
            }
        }
        let tracks: Vec<_> = self.tracks.iter().collect();
        let costs: Vec<Vec<_>> = unique
            .iter()
            .map(|obs| tracks.iter().map(|(_, track)| cost(track, obs)).collect())
            .collect();
        let matches = matches(&unique, &tracks, &costs)?;
        let mut output = Vec::with_capacity(unique.len());
        for (index, mut obs) in unique.into_iter().enumerate() {
            let (tid, previous, uncertain, hits, confirmed) = if let Some((tid, ambiguous)) =
                matches[index]
            {
                let old = self.tracks.get(&tid).ok_or(TrackingError::Assignment)?;
                let previous = &old.current;
                let weak_growth = !previous.keypoints.is_empty()
                    && !obs.keypoints.is_empty()
                    && scale_ratios(previous, &obs).is_some_and(|ratios| {
                        ratios.is_empty() || (ratios.len() == 1 && ratios[0] > 1.8)
                    });
                let uncertain =
                    ambiguous || old.occluded || appearance_conflict(previous, &obs) || weak_growth;
                let hits = if ts - previous.capture_ts <= TENTATIVE_RETENTION {
                    old.hits.saturating_add(1)
                } else {
                    1
                };
                let confirmed =
                    old.confirmed || hits >= 3 || obs.score >= 0.5 || obs.track_id.is_some();
                (tid, Some(previous.clone()), uncertain, hits, confirmed)
            } else {
                let tid = self.next_id;
                self.next_id = self
                    .next_id
                    .checked_add(1)
                    .ok_or(TrackingError::Exhausted)?;
                (
                    tid,
                    None,
                    false,
                    1,
                    obs.score >= 0.5 || obs.track_id.is_some(),
                )
            };
            obs.track_id = Some(tid);
            obs.association_uncertain = uncertain;
            if confirmed {
                output.push(obs.clone());
            }
            self.tracks.insert(
                tid,
                Track {
                    current: obs,
                    previous,
                    hits,
                    confirmed,
                    occluded: false,
                },
            );
        }
        for index in 0..output.len() {
            let obs = &output[index];
            let body_joints = HEAD_TORSO
                .iter()
                .filter(|&&k| obs.keypoints.get(k).is_some_and(|p| p.2 >= 0.4))
                .count();
            if !obs.keypoints.is_empty()
                && body_joints < 2
                && output.iter().any(|other| {
                    other.track_id != obs.track_id
                        && obs.region.contained_overlap(other.region) > 0.3
                })
            {
                if let Some(track) = obs.track_id.and_then(|id| self.tracks.get_mut(&id)) {
                    track.occluded = true;
                    track.current.association_uncertain = true;
                }
                output[index].association_uncertain = true;
            }
        }
        for (id, track) in &mut self.tracks {
            if track.confirmed
                && !output.iter().any(|item| item.track_id == Some(*id))
                && output
                    .iter()
                    .any(|item| track.current.region.contained_overlap(item.region) > 0.3)
            {
                track.occluded = true;
            }
        }
        while self.tracks.len() > TRACK_LIMIT {
            let oldest = self
                .tracks
                .iter()
                .min_by(|a, b| a.1.current.capture_ts.total_cmp(&b.1.current.capture_ts))
                .map(|(id, _)| *id)
                .ok_or(TrackingError::Assignment)?;
            self.tracks.remove(&oldest);
        }
        Ok(output)
    }
}

fn matches(
    observations: &[Observation],
    tracks: &[(&u64, &Track)],
    costs: &[Vec<f64>],
) -> Result<Vec<Option<(u64, bool)>>, TrackingError> {
    let mut result = vec![None; observations.len()];
    let mut used = vec![false; tracks.len()];
    for high_confidence in [true, false] {
        for confirmed in [true, false] {
            let rows: Vec<_> = observations
                .iter()
                .enumerate()
                .filter_map(|(i, obs)| {
                    ((obs.score >= 0.5) == high_confidence && result[i].is_none()).then_some(i)
                })
                .collect();
            let columns: Vec<_> = tracks
                .iter()
                .enumerate()
                .filter_map(|(i, (_, track))| {
                    (track.confirmed == confirmed && !used[i]).then_some(i)
                })
                .collect();
            if rows.is_empty() || columns.is_empty() {
                continue;
            }
            let matrix: Vec<Vec<f64>> = rows
                .iter()
                .map(|&row| {
                    columns
                        .iter()
                        .map(|&column| {
                            let value = costs[row][column];
                            if value <= COST_THRESHOLD { value } else { 1e6 }
                        })
                        .chain(std::iter::repeat_n(0.6, rows.len()))
                        .collect()
                })
                .collect();
            for (&row, assigned) in rows.iter().zip(assign(&matrix)?) {
                let Some(&column) = columns.get(assigned) else {
                    continue;
                };
                let threshold = costs[row][column] + 0.08;
                let ambiguous = tracks.iter().enumerate().any(|(j, (_, track))| {
                    j != column && track.confirmed && costs[row][j] <= threshold
                }) || observations.iter().enumerate().any(|(i, obs)| {
                    i != row && obs.score >= 0.5 && !obs.limited && costs[i][column] <= threshold
                });
                result[row] = Some((*tracks[column].0, ambiguous));
                used[column] = true;
            }
        }
    }
    Ok(result)
}

// Rectangular Hungarian assignment. Each row includes its own finite dummy column.
fn assign(costs: &[Vec<f64>]) -> Result<Vec<usize>, TrackingError> {
    let rows = costs.len();
    let columns = costs.first().ok_or(TrackingError::Assignment)?.len();
    let mut u = vec![0.0; rows + 1];
    let mut v = vec![0.0; columns + 1];
    let mut owner = vec![0; columns + 1];
    let mut previous = vec![0; columns + 1];
    for row in 1..=rows {
        owner[0] = row;
        let mut column = 0;
        let mut distance = vec![f64::INFINITY; columns + 1];
        let mut used = vec![false; columns + 1];
        loop {
            used[column] = true;
            let active_row = owner[column];
            let mut delta = f64::INFINITY;
            let mut next = 0;
            for candidate in 1..=columns {
                if used[candidate] {
                    continue;
                }
                let reduced = costs[active_row - 1][candidate - 1] - u[active_row] - v[candidate];
                if reduced < distance[candidate] {
                    distance[candidate] = reduced;
                    previous[candidate] = column;
                }
                if distance[candidate] < delta {
                    delta = distance[candidate];
                    next = candidate;
                }
            }
            if !delta.is_finite() {
                return Err(TrackingError::Assignment);
            }
            for candidate in 0..=columns {
                if used[candidate] {
                    u[owner[candidate]] += delta;
                    v[candidate] -= delta;
                } else {
                    distance[candidate] -= delta;
                }
            }
            column = next;
            if owner[column] == 0 {
                break;
            }
        }
        while column > 0 {
            let predecessor = previous[column];
            owner[column] = owner[predecessor];
            column = predecessor;
        }
    }
    let mut result = vec![0; rows];
    for (column, row) in owner.into_iter().enumerate().skip(1) {
        if row != 0 {
            result[row - 1] = column - 1;
        }
    }
    Ok(result)
}

fn cost(track: &Track, obs: &Observation) -> f64 {
    let old = &track.current;
    let dt = obs.capture_ts - old.capture_ts;
    if dt <= 0.0
        || dt > 3.0
        || scale_discontinuity(old, obs)
        || unanchored_expansion(old, obs)
        || track.previous.as_ref().is_some_and(|previous| {
            !previous.association_uncertain
                && obs.capture_ts - previous.capture_ts <= 0.8 + 1e-6
                && scale_discontinuity(previous, obs)
        })
    {
        return f64::INFINITY;
    }
    let scale = scale(old, obs);
    let (ox, oy) = old.region.center();
    let (nx, ny) = obs.region.center();
    let (dx, dy) = track
        .previous
        .as_ref()
        .filter(|previous| {
            old.capture_ts > previous.capture_ts
                && old.region.height().min(previous.region.height())
                    >= old.region.height().max(previous.region.height()) * 0.8
        })
        .map_or((0.0, 0.0), |previous| {
            let (px, py) = previous.region.center();
            let step = dt.min(0.4) / (old.capture_ts - previous.capture_ts);
            (
                ((ox - px) * step).clamp(-scale * 0.4, scale * 0.4),
                ((oy - py) * step).clamp(-scale * 0.4, scale * 0.4),
            )
        });
    let distance = (nx - ox - dx).hypot(ny - oy - dy) / scale;
    let overlap = old.region.contained_overlap(obs.region);
    let descent = dt <= 0.8
        && (nx - ox).abs() < scale * 0.35
        && 0.0 < ny - oy
        && ny - oy < scale * 1.25
        && obs.region.height() < old.region.height() * 0.85;
    if !descent && (distance > 0.7 || (overlap < 0.05 && distance > 0.4)) {
        return f64::INFINITY;
    }
    let mut joints: Vec<_> = HEAD_TORSO
        .into_iter()
        .filter_map(|k| {
            let (a, b) = pair(old, obs, k, 0.4)?;
            Some((a.0 + dx - b.0).hypot(a.1 + dy - b.1) / scale)
        })
        .collect();
    joints.sort_unstable_by(f64::total_cmp);
    let joint_distance = joints.get(joints.len() / 2).copied().unwrap_or(distance);
    if joint_distance > if descent { 1.25 } else { 0.8 } {
        return f64::INFINITY;
    }
    let cost = 0.45 * distance + 0.35 * joint_distance.min(1.0) + 0.2 * (1.0 - overlap);
    if descent { cost.min(0.5) } else { cost }
}

fn duplicate(obs: &Observation, other: &Observation) -> bool {
    let scale = scale(obs, other);
    let distance = |k| pair(obs, other, k, 0.4).map(|(a, b)| a.distance(b) / scale);
    let mut distances: Vec<_> = [0, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        .into_iter()
        .filter_map(distance)
        .collect();
    distances.sort_unstable_by(f64::total_cmp);
    let median = distances.get(distances.len() / 2).copied().unwrap_or(1.0);
    let heads: Vec<_> = (0..5).filter_map(distance).collect();
    let duplicate = distances.len() >= 4
        && ((median < 0.06 && distances[((distances.len() - 1) as f64 * 0.9) as usize] < 0.12)
            || (heads.len() >= 2 && heads.iter().all(|d| *d < 0.04) && median < 0.12));
    let overlap = other.region.contained_overlap(obs.region);
    (overlap > 0.6 && duplicate)
        || (other.score >= 0.35
            && obs.score < other.score * 0.5
            && overlap > 0.9
            && (median < 0.3 || distances.len() < 2))
        || same_upper_body(obs, other, overlap)
}

fn same_upper_body(a: &Observation, b: &Observation, overlap: f64) -> bool {
    if a.keypoints.len().min(b.keypoints.len()) < 7
        || overlap < 0.9
        || a.region.area().min(b.region.area()) > 0.7 * a.region.area().max(b.region.area())
    {
        return false;
    }
    let epsilon = (a
        .region
        .width()
        .min(a.region.height())
        .min(b.region.width())
        .min(b.region.height())
        * 0.08)
        .max(2.0);
    if [5, 6]
        .into_iter()
        .any(|k| !pair(a, b, k, 0.7).is_some_and(|(a, b)| a.distance(b) <= epsilon))
    {
        return false;
    }
    let shared: Vec<_> = (0..5).filter(|&k| pair(a, b, k, 0.4).is_some()).collect();
    if shared.is_empty()
        || shared
            .iter()
            .any(|&k| a.keypoints[k].distance(b.keypoints[k]) > epsilon)
    {
        return false;
    }
    [a, b].into_iter().all(|obs| {
        let shoulders = Keypoint(
            (obs.keypoints[5].0 + obs.keypoints[6].0) * 0.5,
            (obs.keypoints[5].1 + obs.keypoints[6].1) * 0.5,
            0.0,
        );
        shared
            .iter()
            .any(|&k| obs.keypoints[k].distance(shoulders) >= epsilon * 2.0)
    })
}

fn scale_ratios(before: &Observation, after: &Observation) -> Option<Vec<f64>> {
    if after.region.height() <= before.region.height() * 1.4 {
        return None;
    }
    let ratios = [(0, 5), (0, 6), (5, 6), (5, 11), (6, 12), (11, 12)]
        .into_iter()
        .filter_map(|(a, b)| {
            let (old_a, new_a) = pair(before, after, a, 0.4)?;
            let (old_b, new_b) = pair(before, after, b, 0.4)?;
            let old_length = old_a.distance(old_b);
            (old_length >= 10.0).then(|| new_a.distance(new_b) / old_length)
        })
        .collect();
    Some(ratios)
}

fn scale_discontinuity(before: &Observation, after: &Observation) -> bool {
    let Some(mut ratios) = scale_ratios(before, after).filter(|values| values.len() >= 2) else {
        return false;
    };
    ratios.sort_unstable_by(f64::total_cmp);
    (ratios[(ratios.len() - 1) / 2] + ratios[ratios.len() / 2]) * 0.5 > 1.8
}

fn unanchored_expansion(before: &Observation, after: &Observation) -> bool {
    let region = before.region;
    if before.truncated
        || after.truncated
        || region.width() < region.height() * 1.2
        || after.region.height() <= region.height() * 1.8
        || after.region.width() <= region.width() * 1.4
        || TORSO
            .iter()
            .any(|&k| pair(before, after, k, 0.65).is_none())
    {
        return false;
    }
    let tolerance = (region.width().max(region.height()) * 0.2).max(4.0);
    (0..before.keypoints.len().min(after.keypoints.len()))
        .filter_map(|k| pair(before, after, k, 0.65))
        .all(|(a, b)| a.distance(b) > tolerance)
}

fn appearance_conflict(before: &Observation, after: &Observation) -> bool {
    let dt = after.capture_ts - before.capture_ts;
    if before.torso_histogram.is_empty()
        || after.torso_histogram.is_empty()
        || dt <= 0.0
        || dt > 0.3 + 1e-6
    {
        return false;
    }
    for obs in [before, after] {
        if TORSO
            .iter()
            .any(|&k| obs.keypoints.get(k).is_none_or(|point| point.2 < 0.4))
        {
            return false;
        }
        let p = &obs.keypoints;
        let dx = (p[11].0 + p[12].0) * 0.5 - (p[5].0 + p[6].0) * 0.5;
        let dy = (p[11].1 + p[12].1) * 0.5 - (p[5].1 + p[6].1) * 0.5;
        let length = dx.hypot(dy);
        if length < 12.0 || dy / length < std::f64::consts::FRAC_1_SQRT_2 {
            return false;
        }
    }
    let similarity: f64 = before
        .torso_histogram
        .iter()
        .zip(&after.torso_histogram)
        .map(|(a, b)| (a * b).sqrt())
        .sum();
    (1.0 - similarity).max(0.0).sqrt() > 0.75
}

fn pair(a: &Observation, b: &Observation, k: usize, minimum: f64) -> Option<(Keypoint, Keypoint)> {
    let (a, b) = (*a.keypoints.get(k)?, *b.keypoints.get(k)?);
    (a.2.min(b.2) >= minimum).then_some((a, b))
}

fn scale(a: &Observation, b: &Observation) -> f64 {
    a.region
        .height()
        .max(a.region.width())
        .max(b.region.height())
        .max(b.region.width())
        .max(40.0)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use flate2::read::GzDecoder;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Trace {
        scenario: String,
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Step {
        operation: String,
        category: String,
        route: u32,
        #[serde(default)]
        recorded_association: bool,
        source_seconds: Option<f64>,
        epoch: Option<u64>,
        observations: Option<Vec<Observation>>,
        tracked_observations: Option<Vec<Observation>>,
    }

    #[test]
    fn association_matches_locked_legacy_observation_traces() {
        let fixture =
            include_bytes!("../../../tests/fixtures/observations/legacy-rules-v2.jsonl.gz");
        let mut count = 0;
        for line in BufReader::new(GzDecoder::new(&fixture[..])).lines() {
            let trace: Trace = serde_json::from_str(&line.unwrap()).unwrap();
            let mut trackers = BTreeMap::<u32, PoseTracker>::new();
            for (index, step) in trace.steps.into_iter().enumerate() {
                if step.category != "medical" || step.recorded_association {
                    continue;
                }
                let tracker = trackers.entry(step.route).or_default();
                if step.operation == "reset" {
                    tracker.reset();
                    continue;
                }
                let actual = tracker
                    .update(
                        step.observations.unwrap(),
                        step.source_seconds.unwrap(),
                        step.epoch.unwrap(),
                    )
                    .unwrap();
                assert_eq!(
                    actual,
                    step.tracked_observations.unwrap(),
                    "{} step {}",
                    trace.scenario,
                    index
                );
                count += 1;
            }
        }
        assert!(count > 1_000);
    }
}

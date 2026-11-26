use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    Box2d, FireAssociation, FireDecision, FireEpisode, FireSample, Observation, Visibility,
    fire::{CONTINUITY_US, WEAK_MATCH_IOU},
};

const MAX_EPISODES: usize = 128;
const REOBSERVATION_US: i64 = 30_000_000;
const RESOLVED_QUIET_US: i64 = 3_000_000;

type TrackReport = (usize, Option<Uuid>, FireEpisode);

#[derive(Default)]
pub(crate) struct FireEpisodeManager {
    episodes: HashMap<Uuid, Episode>,
    previous_frame_us: Option<i64>,
}

struct Episode {
    first_us: i64,
    last_us: i64,
    parts: Vec<Box2d>,
    reviewed: bool,
    resolved: bool,
    quiet_since: Option<i64>,
    last_verified_quiet_us: Option<i64>,
}

impl FireEpisodeManager {
    pub fn contains(&self, id: Uuid) -> bool {
        self.episodes.contains_key(&id)
    }

    pub fn record_resolution(&mut self, id: Uuid) {
        if let Some(episode) = self.episodes.get_mut(&id) {
            episode.resolved = true;
        }
    }

    pub fn update(
        &mut self,
        sample: &FireSample<'_>,
        visible_proposals: &[&Observation],
        reports: &mut [TrackReport],
    ) {
        let at = sample.source_us;
        let visibility = sample.visibility;
        self.episodes
            .retain(|_, episode| at.saturating_sub(episode.last_us) <= REOBSERVATION_US);
        let previous = self.previous_frame_us.replace(at);
        let continuous = previous.is_some_and(|last| at.saturating_sub(last) <= CONTINUITY_US);

        // Compare all tracks against the previous observation before changing geometry.
        // Otherwise a large first result can swallow a distinct second ignition.
        let mut associations = Vec::with_capacity(reports.len());
        let mut ambiguity_clears_quiet = HashSet::new();
        for (_, prior, report) in &*reports {
            let mut candidates = self.episodes.iter().filter(|(_, episode)| {
                (episode.reviewed || at.saturating_sub(episode.last_us) <= CONTINUITY_US)
                    && episode.overlaps(report.region)
            });
            let matching = candidates.next();
            if let (Some(first), Some(second)) = (matching, candidates.next()) {
                let merged = [first, second].into_iter().chain(candidates);
                // A brief merge can hide a new ignition until its own contour is visible.
                // Retain earlier quiet evidence only if every matching episode was resolved
                // and recently verified quiet. The merged contour cannot open a Review.
                if merged
                    .clone()
                    .any(|(_, episode)| !episode.resolved || !episode.observed_quiet_recently(at))
                {
                    ambiguity_clears_quiet.extend(merged.map(|(&id, _)| id));
                }
                associations.push((None, FireAssociation::Ambiguous, None));
                continue;
            }
            let prior_episode =
                prior.and_then(|id| self.episodes.get(&id).map(|episode| (id, episode)));
            let (id, relation) = match prior_episode {
                Some((id, episode))
                    if episode.overlaps(report.region)
                        && (episode.reviewed
                            || at.saturating_sub(episode.last_us) <= CONTINUITY_US) =>
                {
                    (
                        Some(id),
                        if at.saturating_sub(episode.last_us) <= CONTINUITY_US {
                            FireAssociation::Continuous
                        } else {
                            FireAssociation::Reobserved
                        },
                    )
                }
                Some((id, episode))
                    if matching.is_none()
                        && !episode.reviewed
                        && at.saturating_sub(episode.last_us) <= CONTINUITY_US =>
                {
                    (Some(id), FireAssociation::Continuous)
                }
                _ => match matching {
                    Some((&id, episode)) if at.saturating_sub(episode.last_us) <= CONTINUITY_US => {
                        (Some(id), FireAssociation::Split)
                    }
                    Some((&id, _)) => (Some(id), FireAssociation::Reobserved),
                    None => (None, FireAssociation::New),
                },
            };
            let quiet_resolution = id.filter(|&id| {
                let episode = &self.episodes[&id];
                // The first frames of a new ignition can obscure the old region
                // before a contour localizes it. Retain verified quiet briefly
                // while the scene remains visible and observations stay continuous.
                let observed_quiet = episode.observed_quiet_recently(at);
                let visible_change = visibility == Visibility::Good
                    && (report.facts.escaped_controlled_region
                        || (report.facts.flame_visible
                            && report.facts.smoke_growth
                            && report.facts.apparent_growth));
                episode.resolved && continuous && (observed_quiet || visible_change)
            });
            associations.push((id, relation, quiet_resolution));
        }
        // A material change rearms one whole episode, including its other
        // unambiguously associated contours from this frame.
        let mut replacement_ids = HashMap::new();
        for &(_, _, replaced) in &associations {
            if let Some(id) = replaced {
                self.episodes.remove(&id);
                replacement_ids.entry(id).or_insert_with(Uuid::new_v4);
            }
        }

        let mut seen = HashMap::<Uuid, Vec<Box2d>>::new();
        for ((_, prior, report), &(id, relation, _)) in reports.iter_mut().zip(&associations) {
            report.association = relation;
            if relation == FireAssociation::Ambiguous {
                // Keep the prior tracks and their individual histories during a merge.
                // Joined geometry alone supplies no identity for a new Review card.
                report.id = prior.unwrap_or(report.track_id);
                report.decision = FireDecision::Observing;
                continue;
            }
            let id = if let Some(&replacement) = id.and_then(|old| replacement_ids.get(&old)) {
                report.association = FireAssociation::New;
                report.segment_start_source_us = at;
                report.facts.continuity_samples = 1;
                replacement
            } else {
                id.unwrap_or_else(Uuid::new_v4)
            };
            if !self.episodes.contains_key(&id) && self.episodes.len() >= MAX_EPISODES {
                let oldest = self
                    .episodes
                    .iter()
                    .filter(|(id, _)| {
                        !seen.contains_key(id)
                            && associations
                                .iter()
                                .all(|(matched, _, _)| matched.as_ref() != Some(*id))
                    })
                    .min_by_key(|(id, episode)| {
                        (!episode.resolved, episode.reviewed, episode.last_us, **id)
                    })
                    .map(|(&id, _)| id);
                let oldest = oldest.expect("more episode slots than observed tracks");
                self.episodes.remove(&oldest);
            }
            let episode = self.episodes.entry(id).or_insert(Episode {
                first_us: report.segment_start_source_us,
                last_us: at,
                parts: Vec::new(),
                reviewed: false,
                resolved: false,
                quiet_since: None,
                last_verified_quiet_us: None,
            });
            report.first_source_us = episode.first_us;
            report.id = id;
            if episode.resolved {
                report.decision = FireDecision::Observing;
            }
            episode.reviewed |= report.decision == FireDecision::Review;
            episode.last_us = at;
            episode.quiet_since = None;
            episode.last_verified_quiet_us = None;
            seen.entry(id).or_default().push(report.region);
        }
        for (id, episode) in &mut self.episodes {
            if let Some(parts) = seen.remove(id) {
                episode.parts = parts;
                continue;
            }
            let obscured = reports.iter().any(|(_, _, report)| {
                report.association == FireAssociation::Ambiguous && episode.overlaps(report.region)
            });
            let quiet = episode.reviewed
                && !obscured
                && episode.parts.iter().all(|&part| {
                    sample.visibility_in(part) == Visibility::Good
                        && !visible_proposals
                            .iter()
                            .any(|weak| weak.region.iou(part) >= WEAK_MATCH_IOU)
                });
            if !continuous || ambiguity_clears_quiet.contains(id) || visibility != Visibility::Good
            {
                episode.quiet_since = None;
                episode.last_verified_quiet_us = None;
            }
            if quiet {
                let since = *episode.quiet_since.get_or_insert(at);
                if at.saturating_sub(since) >= RESOLVED_QUIET_US {
                    episode.last_verified_quiet_us = Some(at);
                }
            } else {
                episode.quiet_since = None;
            }
        }
    }
}

impl Episode {
    fn observed_quiet_recently(&self, at: i64) -> bool {
        self.last_verified_quiet_us
            .is_some_and(|quiet| at.saturating_sub(quiet) <= CONTINUITY_US)
    }

    fn overlaps(&self, region: Box2d) -> bool {
        self.parts
            .iter()
            .any(|&part| credible_overlap(part, region))
    }
}

fn credible_overlap(previous: Box2d, current: Box2d) -> bool {
    previous.iou(current) >= 0.20 || previous.contained_overlap(current) >= 0.60
}

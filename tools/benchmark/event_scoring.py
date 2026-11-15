"""Score immutable openings against annotated episodes and measured source coverage."""

from collections import Counter, defaultdict
from dataclasses import dataclass
import math
from statistics import median


@dataclass(frozen=True)
class Episode:
    id: str
    first_visible: float
    last_negative: float | None
    left_censored: bool
    roi: tuple | None = None
    subject: str | None = None
    strata: tuple = ()

    def window(self, before, delay):
        start = self.last_negative if self.last_negative is not None else self.first_visible
        return (self.first_visible if self.left_censored else max(0.0, start - before),
                self.first_visible + delay)


def episodes(clip):
    if "events" in clip:
        return [Episode(e["id"], e["onset"]["first_visible_s"], e["onset"]["last_negative_s"],
                        e["onset"]["last_negative_s"] is None, tuple(e["roi"]),
                        e.get("subject_id"), tuple(e["strata"])) for e in clip["events"]]
    onset = clip["onset_s"]
    if onset is None:
        return []
    last_negative = clip.get("last_negative_s")
    return [Episode(clip["file"], onset, last_negative, onset == 0 and last_negative is None)]


def coverage_intervals(samples, task, duration, minimum_accepted_fraction, max_gap_periods):
    """Conservative source-time estimate from adjacent snapshots of actual task PTS and counters."""
    totals = defaultdict(int)
    intervals = defaultdict(list)
    latest = None
    for previous, current in zip(samples, samples[1:]):
        epoch = current.get("source_epoch")
        old_pts = previous.get(f"{task}_source_s")
        new_pts = current.get(f"{task}_source_s")
        if (epoch is None or old_pts is None or new_pts is None or epoch != previous.get("source_epoch")
                or current["capture_state"] not in ("replaying", "completed")
                or previous["capture_state"] != "replaying"
                or current["analysis_state"] != "ready" or previous["analysis_state"] != "ready"):
            continue
        span_us = round((new_pts - old_pts) * 1_000_000)
        frames = current[f"{task}_frames"] - previous[f"{task}_frames"]
        rate = current[f"{task}_fps"]
        if (frames <= 0 or not 0 < span_us * rate <= max_gap_periods * 1_000_000
                or frames * 1_000_000 < span_us * rate * minimum_accepted_fraction):
            continue
        start, end = max(0.0, old_pts), min(duration, new_pts)
        if start >= end:
            continue
        entries = intervals[epoch]
        if entries and start <= entries[-1][1] + 0.000001:
            entries[-1][1] = max(entries[-1][1], end)
        else:
            entries.append([start, end])
        totals[epoch] += frames
        latest = epoch
    # One coherent epoch can support a time window. Do not add the same replay twice.
    seconds = lambda epoch: sum(end - start for start, end in intervals[epoch])
    epoch = max(intervals, key=lambda item: (seconds(item), item == latest), default=None)
    return {"epoch": epoch, "intervals_s": intervals[epoch] if epoch is not None else [],
            "samples_accepted_in_intervals": totals[epoch] if epoch is not None else 0,
            "eligible_seconds": seconds(epoch) if epoch is not None else 0.0}


def covered_window(coverage, start, end):
    start = max(0.0, start)
    return any(lo <= start + 0.000001 and hi + 0.000001 >= end for lo, hi in coverage["intervals_s"])


def overlap(first, second):
    intersection = max(0.0, min(first[2], second[2]) - max(first[0], second[0])) * max(
        0.0, min(first[3], second[3]) - max(first[1], second[1]))
    area = lambda box: (box[2] - box[0]) * (box[3] - box[1])
    union = area(first) + area(second) - intersection
    return intersection / union if union > 0 else 0.0


def event_matches(event, episode, expected_epoch, camera_id, minimum_iou, grounded):
    if expected_epoch is not None and event.get("epoch") != expected_epoch:
        return False
    if grounded and (camera_id is None or event.get("camera_id") != camera_id or expected_epoch is None):
        return False
    if episode.roi is not None and (event.get("roi") is None or overlap(event["roi"], episode.roi) < minimum_iou):
        return False
    return episode.subject is None or event.get("subject_id") == episode.subject


def assign(candidates):
    """Deterministic maximum one-to-one matching. Prefer earlier openings when choices tie."""
    owner = {}

    def take(episode, seen):
        for event in candidates[episode]:
            if event in seen:
                continue
            seen.add(event)
            if event not in owner or take(owner[event], seen):
                owner[event] = episode
                return True
        return False

    # Insert in reverse order so a chronologically earlier episode gets its first feasible choice.
    for episode in reversed(range(len(candidates))):
        take(episode, set())
    return {episode: event for event, episode in owner.items()}


def score_clip(clip, run, thresholds):
    duration = clip["duration_s"]
    category = clip.get("category", "medical" if clip["group"].startswith("medical") else "fire")
    task = "fire" if category == "fire" else "pose"
    grounded = "events" in clip
    measured = run.get("coverage", {}).get("source_time", {}).get(task)
    events = sorted((event for event in run["events"] if event["category"] == category),
                    key=lambda event: (event["opened_source_s"], event.get("id", "")))
    truth = sorted(episodes(clip), key=lambda event: (event.first_visible, event.id))
    before = thresholds.get("category_before_onset_tolerance_s", {}).get(category, thresholds["before_onset_tolerance_s"])
    delay = thresholds["max_delay_s"]
    windows = [episode.window(before, delay) for episode in truth]
    epoch = measured["epoch"] if measured is not None else None

    def has_grounding(event):
        return (event.get("camera_id") is not None and event.get("epoch") is not None
                and event.get("roi") is not None and (task != "pose" or event.get("subject_id") is not None))

    available = grounded and run.get("camera_id") is not None and epoch is not None and all(
        has_grounding(event) for event in events
    )
    matches = [
        [index for index, event in enumerate(events)
         if start <= event["opened_source_s"] <= min(duration, end)
         and event_matches(event, episode, epoch, run.get("camera_id"), thresholds["minimum_roi_iou"], grounded)]
        for episode, (start, end) in zip(truth, windows, strict=True)
    ]
    matched = assign(matches)
    used = set(matched.values())
    duplicate_events = {event for i in matched for event in matches[i] if event not in used}
    scores = []
    for i, episode in enumerate(truth):
        start, end = windows[i]
        event = events[matched[i]] if i in matched else None
        right_censored = duration < end
        timely_covered = measured is not None and covered_window(measured, start, min(end, duration))
        grounding_missing = grounded and not available
        if right_censored:
            status = "right_censored"
        elif grounded and measured is None:
            status = "invalid_coverage"
        elif grounding_missing:
            status = "unverified_grounding"
        elif event is not None:
            observed = measured is None or any(lo - 0.000001 <= event["opened_source_s"] <= hi + 0.000001
                                               for lo, hi in measured["intervals_s"])
            status = "timely" if observed else "invalid_coverage"
        elif measured is not None and not timely_covered:
            status = "invalid_coverage"
        else:
            status = "miss"
        scores.append({"id": episode.id, "status": status, "left_censored": episode.left_censored,
                       "right_censored": right_censored, "window_s": [start, end], "strata": episode.strata,
                       "coverage_measured": measured is not None, "window_covered": timely_covered,
                       "grounded": grounded and not grounding_missing,
                       "matched_event_id": event.get("id") if event else None,
                       "first_recorded_opening_source_s": events[0]["opened_source_s"] if events else None,
                       "reaction_from_clip_start_s": (events[0]["opened_source_s"]
                                                       if episode.left_censored and events and not grounded else None),
                       "delay_s": event["opened_source_s"] - episode.first_visible if event else None,
                       "delay_interval_s": ([event["opened_source_s"] - episode.first_visible,
                                             event["opened_source_s"] - episode.last_negative]
                                            if event and episode.last_negative is not None else None)})
    annotation = clip.get("annotation", "positive" if truth else "negative")
    negative = annotation == "negative"
    counts = Counter(event["attention"] for event in events) if negative else Counter()
    eligible_negative = (measured["eligible_seconds"] if measured is not None else 0.0) if negative else 0.0
    total_negative = duration if negative else 0.0
    return {
        "episodes": scores, "duplicates_in_window": len(duplicate_events),
        "early_events": sum(event["opened_source_s"] < windows[0][0] for event in events) if windows else 0,
        "late_events": sum(event["opened_source_s"] > max(end for _, end in windows) for event in events) if windows else 0,
        "unmatched_in_window": sum(index not in used and index not in duplicate_events
                                   and any(start <= event["opened_source_s"] <= end for start, end in windows)
                                   for index, event in enumerate(events)),
        "negative_events": dict(counts), "nominal_negative_seconds": total_negative,
        "verified_negative_seconds": eligible_negative, "unknown_negative_seconds": total_negative - eligible_negative,
        "uncertain_annotation_seconds": duration if annotation == "uncertain" else 0.0,
        "below_negative_coverage_floor": negative and measured is not None
        and eligible_negative / duration < thresholds["min_verified_negative_fraction"],
        "coverage_measured": measured is not None,
    }


def wilson(matches, trials):
    if not trials:
        return None
    fraction = matches / trials
    z = 1.95996398454
    center = (fraction + z * z / (2 * trials)) / (1 + z * z / trials)
    width = z * math.sqrt(fraction * (1 - fraction) / trials + z * z / (4 * trials**2)) / (1 + z * z / trials)
    return [max(0, center - width), min(1, center + width)]


def summarize(clips, runs):
    groups = defaultdict(list)
    for clip in clips:
        groups[clip["group"]].append((clip, runs[clip["file"]]["scoring"]))
    summary = {}
    for group, rows in sorted(groups.items()):
        onset = [e for _, score in rows for e in score["episodes"] if not e["left_censored"]]
        visible_at_start = [e for _, score in rows for e in score["episodes"] if e["left_censored"]]
        matched = [e for e in onset if e["status"] == "timely"]
        misses = [e for e in onset if e["status"] == "miss"]
        counts = Counter()
        for _, score in rows:
            counts.update(score["negative_events"])
        nominal = sum(score["nominal_negative_seconds"] for _, score in rows) / 3600
        verified = sum(score["verified_negative_seconds"] for _, score in rows) / 3600
        summary[group] = {
            "clips": len(rows), "positive_episodes": len(onset), "matched": len(matched),
            "missed_episodes": [e["id"] for e in misses],
            "right_censored": [e["id"] for e in onset if e["status"] == "right_censored"],
            "invalid_coverage": [e["id"] for e in onset if e["status"] == "invalid_coverage"],
            "unverified_grounding": [e["id"] for e in onset if e["status"] == "unverified_grounding"],
            "visible_at_start": visible_at_start,
            "incomplete_positive_windows": [e["id"] for e in onset
                                            if not e["right_censored"] and not e["window_covered"]],
            "duplicates_in_window": sum(score["duplicates_in_window"] for _, score in rows),
            "early_events": sum(score["early_events"] for _, score in rows),
            "late_events": sum(score["late_events"] for _, score in rows),
            "unmatched_in_window": sum(score["unmatched_in_window"] for _, score in rows),
            "median_delay_s": median(e["delay_s"] for e in matched) if matched else None,
            "recall_wilson_95_iid_descriptive": wilson(len(matched), len(matched) + len(misses)),
            "nominal_negative_camera_hours": nominal, "verified_negative_camera_hours": verified,
            "unknown_negative_camera_hours": max(0.0, nominal - verified),
            "uncertain_annotation_camera_hours": sum(score["uncertain_annotation_seconds"] for _, score in rows) / 3600,
            "negative_clips_below_coverage_floor": [clip["file"] for clip, score in rows
                                                    if score["below_negative_coverage_floor"]],
            "events_in_negative_clips": dict(sorted(counts.items())),
            "nominal_exposure_is_descriptive": True,
            "independent_source_sessions": (len({clip["lineage"]["session_id"] for clip, _ in rows})
                                            if all("lineage" in clip for clip, _ in rows) else None),
        }
    return summary

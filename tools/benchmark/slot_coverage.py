"""Measure source coverage from ordered native task completion receipts."""

from bisect import bisect_left, bisect_right
from collections import Counter, defaultdict
import json
from operator import itemgetter

SECOND_US = 1_000_000
OUTCOMES = {"accepted", "timeout", "worker_failed", "cancelled", "stale", "scope", "generation"}


def receipt_frame(row):
    return tuple(row[key] for key in (
        "camera_id", "epoch", "scene_revision", "generation", "ordinal", "source_us", "task"))


def read_receipts(path, model_sha256, *, pose_frames=None):
    by_task = defaultdict(list)
    openings = defaultdict(dict)
    awaiting_policy = {}
    with path.open(encoding="utf-8") as stream:
        header = json.loads(stream.readline())
        version = header.get("version")
        if (header.get("type") != "header" or version not in (1, 2)
                or header.get("model_manifest_sha256") != model_sha256):
            raise ValueError("Analysis receipts describe a different model pack or format")
        count = 0
        for count, line in enumerate(stream, start=1):
            row = json.loads(line)
            kind = row.get("type")
            if row.get("sequence") != count or kind not in ("task", "policy", "opening"):
                raise ValueError("Analysis receipts are truncated or missing records")
            if kind == "task":
                by_task[row["camera_id"], row["task"]].append(row)
                if version == 2 and row["outcome"] == "accepted":
                    awaiting_policy[receipt_frame(row)] = row
            elif kind == "policy":
                key = receipt_frame(row)
                if version != 2 or key not in awaiting_policy:
                    raise ValueError("Policy receipt has no matching accepted task")
                task = awaiting_policy.pop(key)
                if pose_frames is not None and row["task"] == "pose":
                    pose_frames.setdefault(row["camera_id"], []).append((task, row["details"]["pose_observations"]))
            else:
                stages = openings[row["incident_id"]]
                stage = row["stage"]
                if stage not in ("decision", "journaled", "committed"):
                    raise ValueError("Invalid opening receipt stage")
                stages[stage] = row
        if awaiting_policy:
            raise ValueError("Policy receipts are missing for accepted tasks")
    return by_task, openings, count


def slot_coverage(receipts, origins, fps, duration, minimum_fraction, max_gap_periods):
    groups = defaultdict(list)
    for row in receipts:
        groups[str(row["epoch"]), str(row["scene_revision"]), row["generation"]].append(row)
    results = []
    for scope, rows in groups.items():
        source = origins.get(scope[0])
        if source is None:
            continue
        origin, _ = source
        physical = []
        counters = Counter()
        previous_ordinal = previous_pts = base_origin = last_slot = None
        for row in rows:
            source_us, ordinal, slot, outcome = (row[key] for key in ("source_us", "ordinal", "slot", "outcome"))
            if (outcome not in OUTCOMES or (previous_ordinal is not None
                    and (ordinal <= previous_ordinal or source_us <= previous_pts))):
                raise ValueError("Analysis receipts are out of source order")
            previous_ordinal, previous_pts = ordinal, source_us
            if slot is None:
                counters["accepted_extra" if outcome == "accepted" else "rejected_extra"] += 1
                continue
            if base_origin is None:
                base_origin = source_us
            if (slot != (source_us - base_origin) * fps // SECOND_US
                    or (last_slot is not None and slot <= last_slot)):
                raise ValueError("Analysis receipts do not agree with the source slot clock")
            counters["skipped_slots"] += slot if last_slot is None else slot - last_slot - 1
            last_slot = slot
            if outcome != "accepted":
                counters[f"rejected_{outcome}"] += 1
                continue
            counters["accepted_base"] += 1
            relative = source_us - origin
            if not physical or (relative - physical[-1][-1][0]) * fps > max_gap_periods * SECOND_US:
                physical.append([])
            physical[-1].append([relative, slot])
        intervals = []
        for block in physical:
            start = max(0.0, block[0][0] / SECOND_US)
            stop = min(duration, block[-1][0] / SECOND_US)
            if stop > start and fraction(block, 0, len(block) - 1) >= minimum_fraction:
                intervals.append([start, stop])
        results.append({
            "epoch": scope[0], "scene_revision": scope[1], "capture_generation": scope[2],
            "intervals_s": intervals, "eligible_seconds": sum(end - start for start, end in intervals),
            "physical_blocks": physical, "source_slots": dict(counters),
        })
    selected = max(results, key=lambda item: (item["eligible_seconds"],
                   item["source_slots"].get("accepted_base", 0)), default={
        "epoch": None, "intervals_s": [], "eligible_seconds": 0.0, "physical_blocks": [], "source_slots": {},
    })
    return {**selected, "method": "native_slot_receipts", "minimum_slot_fraction": minimum_fraction,
            "scope_count": len(groups)}


def fraction(block, first, last):
    return (last - first + 1) / (block[last][1] - block[first][1] + 1)


def covered_slot_window(coverage, start, end):
    start = round(max(0.0, start) * SECOND_US)
    end = round(end * SECOND_US)
    for block in coverage["physical_blocks"]:
        first = bisect_right(block, start + 1, key=itemgetter(0)) - 1
        last = bisect_left(block, end - 1, key=itemgetter(0))
        if 0 <= first <= max(first, last) < len(block):
            if fraction(block, first, max(first, last)) >= coverage["minimum_slot_fraction"]:
                return True
    return False

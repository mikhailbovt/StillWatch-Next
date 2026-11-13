"""Compare frozen local replay candidates against a hash-checked video manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import shutil
import sqlite3
import subprocess
import sys
import time
from collections import defaultdict
from contextlib import closing
from pathlib import Path
from statistics import median
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tests/e2e"))
from replay_core import run_core, until


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def rust_batch(args, clips, media, model_hash, ort_hash, configured):
    with TemporaryDirectory(prefix="stillwatch-heldout-") as directory:
        root = Path(directory)
        data = root / "data"
        sources = root / "sources"
        sources.mkdir()
        for index, clip in enumerate(clips):
            source = media / clip["file"]
            shutil.copyfile(source, sources / f"{index}{source.suffix}")
        with run_core(args.core, args.worker, data, sources, root / "core.log",
                      "--model-pack", str(args.models), "--model-manifest-sha256", model_hash,
                      "--ort-library", str(args.runtime), "--ort-library-sha256", ort_hash,
                      "--inference-workers", str(len(clips)), "--inference-threads", "3") as (_, client):
            cameras = {}
            for index, clip in enumerate(clips):
                config = {"name": f"Evaluation {index}", "enabled": True,
                          "source": {"type": "replay", "file": f"{index}{Path(clip['file']).suffix}", "pace": True}}
                if configured and "controlled_fire_regions" in clip:
                    config["controlled_fire_regions"] = clip["controlled_fire_regions"]
                status, camera = client.call("POST", "/api/v2/cameras", config, key=f"clip-{index}")
                if status != 201 or camera["live"]:
                    raise RuntimeError(f"Unable to start replay {clip['file']}: {status}")
                cameras[camera["id"]] = clip

            samples = defaultdict(list)
            end = time.monotonic() + max(clip["duration_s"] for clip in clips) + 20
            while time.monotonic() < end:
                completed = True
                for camera, clip in cameras.items():
                    state = client.camera(camera)
                    if state["runtime"]["state"] == "completed":
                        continue
                    completed = False
                    status, analysis = client.call("GET", f"/api/v2/cameras/{camera}/analysis-status")
                    if status != 200:
                        raise RuntimeError(f"Cannot measure replay coverage for {clip['file']}")
                    if state["runtime"]["state"] in {"replaying", "clock_hold", "retrying"}:
                        samples[camera].append(analysis)
                if completed:
                    break
                time.sleep(0.25)
            else:
                raise RuntimeError("Timed out waiting for the original clips to finish")

            with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
                def capture_report():
                    current = db.execute("SELECT count(*) FROM incidents").fetchone()[0]
                    rows = db.execute("SELECT incident FROM change_events WHERE opened=1 ORDER BY sequence").fetchall()
                    if len(rows) != current:
                        return None
                    return (rows, db.execute("SELECT count(*) FROM notification_outbox").fetchone()[0])

                # Allow the last accepted inference result and evidence write to settle.
                time.sleep(1.0)
                rows, outbox = until(capture_report)
                if outbox:
                    raise RuntimeError("An offline replay unexpectedly queued a delivery")
            found = defaultdict(list)
            for row, in rows:
                event = json.loads(row)
                found[event["scope"]["camera_id"]].append({
                    "category": event["category"], "opened_source_s": event["last_source_us"] / 1_000_000,
                    "attention": event["attention"], "reason": event["detection_reason"],
                })
            results = {}
            for camera, clip in cameras.items():
                observations = samples[camera]
                pose = max((int(item["pose_frames"]) for item in observations), default=0)
                fire = max((int(item["fire_frames"]) for item in observations), default=0)
                if pose == 0 or fire == 0:
                    raise RuntimeError(f"Inference did not cover {clip['file']}")
                results[clip["file"]] = {"events": found[camera], "coverage": {
                    "pose_frames_max_sampled": pose, "fire_frames_max_sampled": fire,
                    "samples": len(observations),
                    "analysis_unready_fraction": sum(item["state"] != "ready" for item in observations) / len(observations),
                }}
            return results


def legacy(args, clips, media):
    if args.legacy_python is None:
        return None
    result = subprocess.check_output([
        "git", "-C", str(args.legacy_root), "rev-parse", "HEAD",
    ], text=True).strip()
    expected = json.loads((Path(__file__).resolve().parents[2] / "baseline.json").read_text())["revision"]
    if result != expected:
        raise ValueError("Reference checkout does not match baseline.json")
    helper = Path(__file__).with_name("legacy_replay.py")
    return json.loads(subprocess.check_output([
        str(args.legacy_python), "-B", str(helper), str(args.legacy_root), str(args.legacy_models),
        str(media), str(args.manifest),
    ]))


def wilson(matches, trials):
    if not trials:
        return None
    fraction = matches / trials
    z = 1.95996398454
    center = (fraction + z * z / (2 * trials)) / (1 + z * z / trials)
    width = z * math.sqrt(fraction * (1 - fraction) / trials + z * z / (4 * trials**2)) / (1 + z * z / trials)
    return [max(0, center - width), min(1, center + width)]


def summarize(clips, runs, thresholds):
    results = {}
    for group in sorted({clip["group"] for clip in clips}):
        scenes = [clip for clip in clips if clip["group"] == group]
        category = "medical" if group.startswith("medical") else "fire"
        positive = [clip for clip in scenes if clip["onset_s"] is not None]
        negative_hours = sum(clip["duration_s"] for clip in scenes if clip["onset_s"] is None) / 3600
        delays, missed, duplicates, early, late = [], [], 0, 0, 0
        unmatched = defaultdict(int)
        for clip in scenes:
            events = sorted((event for event in runs[clip["file"]]["events"] if event["category"] == category),
                            key=lambda event: event["opened_source_s"])
            if clip["onset_s"] is None:
                for event in events:
                    unmatched[event["attention"]] += 1
                continue
            onset = clip["onset_s"]
            start = onset - thresholds["before_onset_tolerance_s"]
            end = onset + thresholds["max_delay_s"]
            matched = [event for event in events if start <= event["opened_source_s"] <= end]
            early += sum(event["opened_source_s"] < start for event in events)
            late += sum(event["opened_source_s"] > end for event in events)
            if not matched:
                missed.append(clip["file"])
                continue
            delays.append(matched[0]["opened_source_s"] - onset)
            duplicates += len(matched) - 1
        statistics = {"clips": len(scenes), "positive_episodes": len(positive), "matched": len(delays),
                      "missed_files": missed, "duplicates_in_window": duplicates,
                      "early_events": early, "late_events": late, "median_delay_s": median(delays) if delays else None,
                      "recall_wilson_95_iid_descriptive": wilson(len(delays), len(positive)),
                      "negative_camera_hours": negative_hours}
        if negative_hours:
            levels = set(unmatched) | {"review", "urgent"} if "coverage" in next(iter(runs.values())) else {"legacy_candidate"}
            statistics["events_in_negative_clips"] = {level: unmatched[level] for level in sorted(levels)}
            statistics["descriptive_events_per_camera_hour"] = {level: unmatched[level] / negative_hours for level in sorted(levels)}
            statistics["zero_event_poisson_upper_95_per_camera_hour"] = {
                level: -math.log(0.05) / negative_hours for level in sorted(levels) if unmatched[level] == 0
            }
        results[group] = statistics
    return results


def run(args):
    manifest = json.loads(args.manifest.read_text())
    clips = manifest["clips"]
    media = args.media.resolve(strict=True)
    for clip in clips:
        file = (media / clip["file"]).resolve(strict=True)
        if not file.is_relative_to(media) or digest(file) != clip["sha256"] or clip["cloud_consent"]:
            raise ValueError(f"Invalid local-only evaluation input {clip['file']}")
    model_hash = digest(args.models / "models.lock.json")
    ort_hash = digest(args.runtime)
    reference = legacy(args, clips, media)
    outputs = {}
    for start in range(0, len(clips), args.parallel):
        group = clips[start:start + args.parallel]
        outputs.update(rust_batch(args, group, media, model_hash, ort_hash, configured=True))
        print(f"Evaluated {start + len(group)}/{len(clips)} clips through Rust Core", flush=True)
    controlled = [clip for clip in clips if "controlled_fire_regions" in clip]
    comparison = {}
    # Use the same batches so the comparison keeps the scheduler load unchanged.
    for start in range(0, len(clips), args.parallel):
        group = clips[start:start + args.parallel]
        if any("controlled_fire_regions" in clip for clip in group):
            other = rust_batch(args, group, media, model_hash, ort_hash, configured=False)
            comparison.update({clip["file"]: other[clip["file"]] for clip in group
                               if "controlled_fire_regions" in clip})
    report = {
        "manifest_sha256": digest(args.manifest), "core_sha256": digest(args.core),
        "worker_sha256": digest(args.worker), "model_manifest_sha256": model_hash,
        "ort_sha256": ort_hash, "inference_threads_per_replay_lane": 3, "parallel_lanes": args.parallel,
        "rust": summarize(clips, outputs, manifest["thresholds"]), "per_clip_rust": outputs,
        "controlled_without_regions": comparison,
        "controlled_without_regions_summary": summarize(controlled, comparison, manifest["thresholds"]),
        "legacy": summarize(clips, reference["clips"], manifest["thresholds"]) if reference else None,
        "legacy_provenance": reference["provenance"] if reference else None,
        "per_clip_legacy": reference["clips"] if reference else None,
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "models", "runtime", "manifest", "media", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    for name in ("legacy-python", "legacy-root", "legacy-models"):
        parser.add_argument(f"--{name}", type=Path)
    parser.add_argument("--parallel", type=int, choices=range(1, 5), default=3)
    args = parser.parse_args()
    legacy_args = (args.legacy_python, args.legacy_root, args.legacy_models)
    if any(legacy_args) and not all(legacy_args):
        parser.error("provide all three legacy options to compare the pinned Python baseline")
    for name in ("core", "worker", "models", "runtime", "manifest", "media", "legacy_python", "legacy_root", "legacy_models"):
        if (path := getattr(args, name)) is not None:
            setattr(args, name, path.resolve(strict=True))
    run(args)

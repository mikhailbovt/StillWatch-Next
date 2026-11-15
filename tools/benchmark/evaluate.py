"""Compare frozen local replay candidates against a hash-checked video manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sqlite3
import subprocess
import sys
import time
from collections import defaultdict
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

from event_scoring import coverage_intervals, episodes, score_clip, summarize
from fast_rc_contract import validate_schema

ROOT = Path(__file__).resolve().parents[2]
QUALITY_CONTRACT = ROOT / "qualification/quality-contract.json"
DEVELOPMENT_CORPORA = ROOT / "models/evaluations/development-corpora.json"
sys.path.insert(0, str(ROOT / "tests/e2e"))
from replay_core import run_core, until


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def sample_camera(state, first_pts):
    runtime = state["runtime"]
    analysis = runtime["analysis"]
    epoch = state["source_epoch"]
    origin = runtime.get("first_source_us")
    if origin is not None:
        first_pts[epoch] = int(origin)
    result = {"capture_state": runtime["state"],
              "analysis_state": analysis["state"], "source_epoch": epoch,
              "source_timestamps_supported": origin is not None}
    settings = state["config"].get("sampling", {})
    for task, default in (("pose", 5), ("fire", 2)):
        result[f"{task}_fps"] = settings.get(f"{task}_fps", default)
        result[f"{task}_frames"] = int(analysis[f"{task}_frames"])
        observed = analysis.get(f"last_{task}_source_us")
        result[f"{task}_source_s"] = ((int(observed) - first_pts[epoch]) / 1_000_000
                                      if epoch in first_pts and observed is not None
                                      and analysis["source_epoch"] == epoch else None)
    return result


def opening_event(event, committed_at, origins):
    scope = event["scope"]
    epoch = str(scope["epoch"])
    source_us = event["last_source_us"]
    origin = origins.get(epoch)
    return {
        "id": event["id"], "category": event["category"], "camera_id": scope["camera_id"], "epoch": epoch,
        "scene_revision": str(scope["scene_revision"]), "opened_source_us": str(source_us),
        "opened_source_s": (source_us - (origin if origin is not None else 0)) / 1_000_000,
        "source_origin_verified": origin is not None, "opening_recorded_utc_ms": committed_at,
        "opening_observed_host_utc_ms": event.get("opening_observed_host_utc"),
        "opening_delayed": event.get("opening_delayed", False),
        "attention": event["attention"], "reason": event["detection_reason"],
    }


def rust_batch(args, clips, media, model_hash, ort_hash, coverage_contract, configured):
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
            samples = defaultdict(list)
            source_origins = defaultdict(dict)
            for index, clip in enumerate(clips):
                config = {"name": f"Evaluation {index}", "enabled": True,
                          "source": {"type": "replay", "file": f"{index}{Path(clip['file']).suffix}", "pace": True}}
                if configured and "controlled_fire_regions" in clip:
                    config["controlled_fire_regions"] = clip["controlled_fire_regions"]
                status, camera = client.call("POST", "/api/v2/cameras", config, key=f"clip-{index}")
                if status != 201 or camera["live"]:
                    raise RuntimeError(f"Unable to start replay {clip['file']}: {status}")
                cameras[camera["id"]] = clip
                # Catch fast starts whilst the other camera probes are still being issued.
                for camera_id in cameras:
                    state = client.camera(camera_id)
                    samples[camera_id].append(sample_camera(state, source_origins[camera_id]))

            terminal = set()
            end = time.monotonic() + max(clip["duration_s"] for clip in clips) + 20
            while time.monotonic() < end:
                for camera in cameras:
                    if camera in terminal:
                        continue
                    state = client.camera(camera)
                    finished = state["runtime"]["state"] == "completed"
                    samples[camera].append(sample_camera(state, source_origins[camera]))
                    if finished:
                        terminal.add(camera)
                if len(terminal) == len(cameras):
                    break
                time.sleep(0.25)

            with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
                def capture_report():
                    current = db.execute("SELECT count(*) FROM incidents").fetchone()[0]
                    rows = db.execute("SELECT incident, occurred_at FROM change_events WHERE opened=1 ORDER BY sequence").fetchall()
                    if len(rows) != current:
                        return None
                    return (rows, db.execute("SELECT count(*) FROM notification_outbox").fetchone()[0])

                # Allow the last accepted inference result and evidence write to settle.
                time.sleep(1.0)
                rows, outbox = until(capture_report)
                if outbox:
                    raise RuntimeError("An offline replay unexpectedly queued a delivery")
            found = defaultdict(list)
            for row, committed_at in rows:
                event = json.loads(row)
                camera = event["scope"]["camera_id"]
                found[camera].append(opening_event(event, committed_at, source_origins[camera]))
            results = {}
            for camera, clip in cameras.items():
                observations = samples[camera]
                intervals = {task: coverage_intervals(
                    observations, task, clip["duration_s"], coverage_contract["min_accepted_source_rate_fraction"],
                    coverage_contract["max_inter_observation_periods_for_development_replay"],
                ) for task in ("pose", "fire")} if any(row["source_timestamps_supported"] for row in observations) else {}
                results[clip["file"]] = {"camera_id": camera, "events": found[camera], "coverage": {
                    **{f"{task}_frames_max_sampled": max(row[f"{task}_frames"] for row in observations)
                       for task in ("pose", "fire")},
                    "sample_count": len(observations), "terminal_state": observations[-1]["capture_state"],
                    "completed": camera in terminal, "source_time": intervals,
                    "analysis_unready_fraction": sum(item["analysis_state"] != "ready" for item in observations) / len(observations),
                }, "delivery_measured": False}
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


def load_manifest(path, manifest_digest, contract, development):
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") == 2:
        validate_schema("dataset", manifest)
    clips = manifest["clips"]
    if not clips or len({clip["file"] for clip in clips}) != len(clips):
        raise ValueError("Evaluation requires distinct clip entries")
    if manifest.get("schema_version") == 2:
        lineage_splits = {}
        for clip in clips:
            lineage, split = clip["lineage"], clip["split"]
            if lineage["source_id"] not in manifest["sources"]:
                raise ValueError(f"Missing source lineage in {clip['file']}")
            identities = [(field, lineage[field]) for field in ("scene_id", "camera_id", "session_id")]
            identities.append(("group", clip["group"]))
            identities.extend(("subject", subject) for subject in lineage.get("subject_ids", []))
            identities.extend(("original_media", segment["sha256"]) for segment in lineage["original_segments"])
            identities.append(("clip_media", clip["sha256"]))
            for identity in identities:
                if lineage_splits.setdefault(identity, split) != split:
                    raise ValueError(f"Source lineage crosses evaluation splits: {identity}")
            truth = episodes(clip)
            if any(event["category"] != clip["category"] for event in clip["events"]):
                raise ValueError(f"Episode category differs from its clip in {clip['file']}")
            if len({event.id for event in truth}) != len(truth):
                raise ValueError(f"Duplicate episode ID in {clip['file']}")
            for event in truth:
                x1, y1, x2, y2 = event.roi
                if not (0 <= x1 < x2 <= 1 and 0 <= y1 < y2 <= 1
                        and 0 <= event.first_visible < clip["duration_s"]
                        and (event.last_negative is None or 0 <= event.last_negative < event.first_visible)):
                    raise ValueError(f"Invalid onset interval or ROI in {clip['file']}")
                if event.subject is not None and event.subject not in lineage.get("subject_ids", []):
                    raise ValueError(f"Missing subject lineage in {clip['file']}")
                if event.left_censored and event.first_visible != 0:
                    raise ValueError(f"Left-censored episode must be visible at the start of {clip['file']}")
    elif manifest.get("schema_version", 1) == 1:
        corpus = next((item for item in development["corpora"] if item["sha256"] == manifest_digest), {})
        overrides = corpus.get("annotation_overrides", {})
        clips = [dict(clip, **overrides.get(clip["file"], {})) for clip in clips]
    else:
        raise ValueError("Unsupported evaluation manifest schema")
    thresholds = {
        **manifest.get("thresholds", {}),
        "minimum_roi_iou": contract["matching"]["minimum_roi_iou"],
        "min_verified_negative_fraction": contract["coverage"]["min_verified_negative_fraction"],
    }
    if manifest.get("schema_version") == 2:
        thresholds.update(max_delay_s=contract["matching"]["max_delay_s"], before_onset_tolerance_s=0,
                          category_before_onset_tolerance_s={
                              task: contract["matching"][f"{task}_before_onset_tolerance_s"]
                              for task in ("fire", "medical")})
    return clips, thresholds


def score_run(clips, outputs, thresholds):
    for clip in clips:
        outputs[clip["file"]]["scoring"] = score_clip(clip, outputs[clip["file"]], thresholds)
    return summarize(clips, outputs)


def run(args):
    contract = json.loads(QUALITY_CONTRACT.read_text(encoding="utf-8"))
    manifest_digest = digest(args.manifest)
    development = json.loads(DEVELOPMENT_CORPORA.read_text(encoding="utf-8"))
    clips, thresholds = load_manifest(args.manifest, manifest_digest, contract, development)
    scoring_inputs = {"report_version": 2, "manifest_sha256": manifest_digest,
                      "quality_contract_sha256": digest(QUALITY_CONTRACT),
                      "development_corpora_sha256": digest(DEVELOPMENT_CORPORA)}
    if args.rescore is not None:
        previous = json.loads(args.rescore.read_text(encoding="utf-8"))
        if previous["manifest_sha256"] != manifest_digest or set(previous["per_clip_rust"]) != {c["file"] for c in clips}:
            raise ValueError("Saved report does not cover the exact frozen clip manifest")
        report = {**scoring_inputs, "method": "rescore_saved_openings", "input_report_sha256": digest(args.rescore),
                  "rust": score_run(clips, previous["per_clip_rust"], thresholds),
                  "per_clip_rust": previous["per_clip_rust"],
                  "legacy": score_run(clips, previous["per_clip_legacy"], thresholds) if previous.get("per_clip_legacy") else None}
        args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
        return
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
        outputs.update(rust_batch(args, group, media, model_hash, ort_hash, contract["coverage"], configured=True))
        print(f"Evaluated {start + len(group)}/{len(clips)} clips through Rust Core", flush=True)
    controlled = [clip for clip in clips if "controlled_fire_regions" in clip]
    comparison = {}
    # Use the same batches so the comparison keeps the scheduler load unchanged.
    for start in range(0, len(clips), args.parallel):
        group = clips[start:start + args.parallel]
        if any("controlled_fire_regions" in clip for clip in group):
            other = rust_batch(args, group, media, model_hash, ort_hash, contract["coverage"], configured=False)
            comparison.update({clip["file"]: other[clip["file"]] for clip in group
                               if "controlled_fire_regions" in clip})
    report = {
        **scoring_inputs, "method": "paced_local_replay", "core_sha256": digest(args.core),
        "worker_sha256": digest(args.worker), "model_manifest_sha256": model_hash,
        "ort_sha256": ort_hash, "inference_threads_per_replay_lane": 3, "parallel_lanes": args.parallel,
        "rust": score_run(clips, outputs, thresholds), "per_clip_rust": outputs,
        "controlled_without_regions": comparison,
        "controlled_without_regions_summary": score_run(controlled, comparison, thresholds),
        "legacy": score_run(clips, reference["clips"], thresholds) if reference else None,
        "legacy_provenance": reference["provenance"] if reference else None,
        "per_clip_legacy": reference["clips"] if reference else None,
    }
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    for name in ("core", "worker", "models", "runtime", "media", "rescore"):
        parser.add_argument(f"--{name}", type=Path)
    for name in ("legacy-python", "legacy-root", "legacy-models"):
        parser.add_argument(f"--{name}", type=Path)
    parser.add_argument("--parallel", type=int, choices=range(1, 5), default=3)
    args = parser.parse_args()
    legacy_args = (args.legacy_python, args.legacy_root, args.legacy_models)
    if any(legacy_args) and not all(legacy_args):
        parser.error("provide all three legacy options to compare the pinned Python baseline")
    native_args = (args.core, args.worker, args.models, args.runtime, args.media)
    if args.rescore is None and not all(native_args):
        parser.error("native replay requires --core, --worker, --models, --runtime and --media")
    if args.rescore is not None and (any(native_args) or any(legacy_args)):
        parser.error("--rescore reads an existing report without launching a runtime")
    for name in ("core", "worker", "models", "runtime", "manifest", "media", "legacy_python", "legacy_root", "legacy_models", "rescore"):
        if (path := getattr(args, name)) is not None:
            setattr(args, name, path.resolve(strict=True))
    if args.rescore is not None and args.output.resolve() == args.rescore:
        parser.error("preserve the original report and choose a different --output path")
    run(args)

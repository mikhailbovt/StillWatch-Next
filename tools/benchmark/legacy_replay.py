"""Run the original StillWatch replay in its separately locked Python environment."""

import asyncio
import contextlib
import hashlib
import importlib.metadata
import importlib.util
import json
import sys
from pathlib import Path
from tempfile import TemporaryDirectory

from event_scoring import coverage_intervals, normalized_region


QUALITY_CONTRACT = Path(__file__).resolve().parents[2] / "qualification/quality-contract.json"


def measured_clip(result, trace_path, clip, rates, coverage_contract):
    openings = {}
    originals = {}
    for item in result["events"]:
        if item["category"] not in ("fire", "medical"):
            continue
        event = {"id": item["incident_id"], "category": item["category"], "camera_id": item["camera_id"],
                 "epoch": str(item["epoch"]), "opened_source_s": item["detected_ts"],
                 "attention": "legacy_candidate", "reason": item["reason"]}
        openings[item["incident_id"]] = event
        if item["candidate_refs"]:
            reference = item["candidate_refs"][0]["candidate_id"]
            if reference in originals:
                raise ValueError("Legacy opening candidate is associated with multiple incidents")
            originals[reference] = event

    clock = result["source_clock"]
    use_source_time = (clock["mode"] == "native_pts" and clock["nominal_frames"] <= 1
                       and not (clock["invalid_timestamps"] or clock["regressed_timestamps"]))
    samples = {task: [] for task in rates}
    source_origin = None
    trace_digest = hashlib.sha256()
    with trace_path.open("rb") as handle:
        for line in handle:
            trace_digest.update(line)
            frame = json.loads(line)
            source_us = round(frame["source_ts"] * 1_000_000)
            if source_origin is None:
                source_origin = source_us
            elapsed_us = source_us - source_origin
            for task in frame["inferred_routes"]:
                hit = {"capture_state": "replaying", "analysis_state": "ready", "source_epoch": "1",
                       f"{task}_source_s": elapsed_us / 1_000_000, f"{task}_fps": rates[task],
                       f"{task}_frames": len(samples[task]) + 1}
                samples[task].append(hit)
            for candidate in frame["candidates"]:
                event = originals.pop(candidate["candidate_id"], None)
                if event is None:
                    continue
                candidate_us = round(candidate["last_ts"] * 1_000_000)
                if (str(candidate["epoch"]) != event["epoch"] or candidate["category"] != event["category"]
                        or round(event["opened_source_s"] * 1_000_000) != candidate_us):
                    raise ValueError("Legacy incident does not match its original candidate")
                event.update(opened_source_s=(candidate_us - source_origin) / 1_000_000, reason=candidate["reason"])
                if not use_source_time or candidate_us != source_us:
                    continue
                region = candidate["box"]
                if candidate["category"] == "medical":
                    track_id = candidate["track_id"]
                    motion = candidate.get("motion") or {}
                    people = [person for person in frame["observations"]["pose"]
                              if track_id is not None and person["track_id"] == track_id
                              and not person["association_uncertain"]
                              and round(person["capture_ts"] * 1_000_000) == source_us]
                    certain = (len(people) == 1 and motion.get("identity_uncertain") is False
                               and motion.get("track_id") == track_id)
                    event.update(track_id=track_id, identity_uncertain=not certain)
                    if not certain:
                        continue
                    region = people[0]["box"]
                roi = normalized_region(region, frame["width"], frame["height"])
                if roi is not None:
                    event.update(roi=roi, opened_clip_us=str(elapsed_us))
    if trace_digest.hexdigest() != result["trace_sha256"]:
        raise ValueError("Legacy trace differs from the replay output fingerprint")
    if {task: len(rows) for task, rows in samples.items()} != result["inference_calls"]:
        raise ValueError("Legacy trace does not account for each inference call")
    intervals = {task: coverage_intervals(rows, task, clip["duration_s"],
                                          coverage_contract["min_accepted_source_rate_fraction"],
                                          coverage_contract["max_inter_observation_periods_for_development_replay"])
                 for task, rows in samples.items()} if use_source_time else {}
    return {"camera_id": result["camera_id"], "events": list(openings.values()),
            "inference_calls": result["inference_calls"], "trace_sha256": result["trace_sha256"],
            "source_clock": clock, "coverage": {"source_time": intervals}}


async def main(legacy, weights, media, manifest):
    sys.path.insert(0, str(legacy / "src"))
    source = legacy / "tools/replay.py"
    spec = importlib.util.spec_from_file_location("stillwatch_legacy_replay", source)
    replay = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(replay)
    from stillwatch.runtime import Analysis
    from stillwatch.telemetry import Telemetry

    settings = replay.load_settings(legacy / "configs/default.yaml").model_copy(update={
        "pose_weights": weights / "pose-balanced.safetensors",
        "fire_weights": weights / "fire.safetensors",
    })
    models = json.loads((legacy / "models.lock.json").read_text())
    for path in (settings.pose_weights, settings.fire_weights):
        expected = next(model["tensor_sha256"] for model in models["models"].values() if Path(model["local_path"]).name == path.name)
        with path.open("rb") as file:
            if hashlib.file_digest(file, "sha256").hexdigest() != expected:
                raise ValueError("Legacy weights do not match the public baseline lock")
    quality = QUALITY_CONTRACT.read_bytes()
    rates = {"pose": settings.pose_fps, "fire": settings.fire_fps}
    coverage = json.loads(quality)["coverage"]
    with contextlib.redirect_stdout(sys.stderr), TemporaryDirectory(prefix="stillwatch-reference-replay-") as directory:
        settings = settings.model_copy(update={"data_dir": Path(directory) / "runtime"})
        analysis = Analysis(settings, Telemetry())
        results = {}
        for index, clip in enumerate(json.loads(manifest.read_text())["clips"]):
            output = Path(directory) / str(index)
            result = await replay.replay(media / clip["file"], settings, output, analysis=analysis)
            if result["source_sha256"] != clip["sha256"] or not all(
                result["routes"][kind] == "ready" and result["inference_calls"][kind] > 0 for kind in ("pose", "fire")
            ):
                raise RuntimeError("Legacy replay did not process the locked source and both models")
            results[clip["file"]] = measured_clip(result | {"camera_id": settings.camera_id}, output / "observations.jsonl",
                                                  clip, rates, coverage)
            print(f"Evaluated reference clip {index + 1}", file=sys.stderr, flush=True)
    print(json.dumps({"clips": results, "provenance": {
        "config_hash": settings.fingerprint,
        "implementation_hash": result["implementation_hash"], "replay_sha256": result["replay_sha256"],
        "quality_contract_sha256": hashlib.sha256(quality).hexdigest(),
        "python_version": sys.version.split()[0],
        "installed_versions": {name: importlib.metadata.version(name)
                               for name in ("opencv-python", "ultralytics", "torch", "torchvision", "safetensors")},
    }}))


if __name__ == "__main__":
    asyncio.run(main(*(Path(value) for value in sys.argv[1:])))

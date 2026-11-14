"""Measure the production detector's fire observations on timestamped local clips."""

from __future__ import annotations

import argparse
from contextlib import ExitStack, contextmanager
import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess
from tempfile import TemporaryDirectory
import uuid

from event_scoring import covered_window, overlap


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def exact(stream, size):
    data = stream.read(size)
    if len(data) != size:
        raise RuntimeError("Unexpected end of native worker output")
    return data


@contextmanager
def process(executable, *command):
    with subprocess.Popen([str(executable), *command], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as child:
        try:
            yield child
        finally:
            if child.poll() is None:
                child.terminate()


def replay_policy(policy, header, rgb, result):
    item = {"request": result["request"], "elapsed_ms": 0, "pose": None, "fire": result["fire"]}
    payload = json.dumps(item, separators=(",", ":")).encode()
    policy.stdin.write(struct.pack("<I", len(payload)))
    policy.stdin.write(payload)
    policy.stdin.write(header)
    policy.stdin.write(rgb)
    policy.stdin.flush()
    length, = struct.unpack("<I", exact(policy.stdout, 4))
    if not 0 < length <= 1_500_000:
        raise RuntimeError("Invalid fire policy response length")
    trace = json.loads(exact(policy.stdout, length))
    if trace["source_us"] != result["source_us"]:
        raise RuntimeError("Fire policy response does not match the requested frame")
    return trace


def analyze(inference, executable, clip, path, window, interval_us, diagnostic, policy):
    command = {"source": {"kind": "replay", "file": str(path), "pace": False}, "masks": []}
    start, stop = window
    next_us = math.ceil(start * 1_000_000)
    samples = []
    if policy:
        policy.stdin.write(b"\0" * 4)
    with process(executable, "capture") as capture:
        capture.stdin.write(json.dumps(command).encode())
        capture.stdin.close()
        first_pts = None
        while prefix := capture.stdout.read(4):
            header = prefix + exact(capture.stdout, 52)
            if prefix != b"SWF1":
                raise RuntimeError("Unexpected capture protocol version")
            length = struct.unpack_from("<I", header, 16)[0]
            if not 0 < length <= 3840 * 2160 * 3:
                raise RuntimeError("Invalid capture frame length")
            rgb = exact(capture.stdout, length)
            if header[4:6] != b"\x01\x00":
                continue
            pts = struct.unpack_from("<q", header, 40)[0]
            if first_pts is None:
                first_pts = pts
            source_us = pts - first_pts
            if source_us > stop * 1_000_000:
                break
            if source_us < next_us:
                continue
            next_us = source_us + interval_us
            request = uuid.uuid4()
            inference.stdin.write(request.bytes + b"\x02" + header)
            inference.stdin.write(rgb)
            inference.stdin.flush()
            response_length = struct.unpack("<I", exact(inference.stdout, 4))[0]
            if not 0 < response_length <= 1_500_000:
                raise RuntimeError("Invalid inference response length")
            result = json.loads(exact(inference.stdout, response_length))
            if (result["request"] != str(request) or result.get("pose") is not None or result["fire"] is None
                    or (diagnostic and result.get("source_us") != str(pts))):
                raise RuntimeError("Invalid inference response")
            sample = {"source_us": source_us, "observations": [
                {"label": item["label"], "score": item["score"], "box": item["box"]}
                for item in result["fire"]
            ]}
            if diagnostic:
                sample.update(source_pts_us=str(pts), frame_number=str(struct.unpack_from("<Q", header, 24)[0]),
                              rgb_sha256=hashlib.sha256(rgb).hexdigest(), source_dimensions=result["source_dimensions"],
                              trace=result["trace"])
            if policy:
                sample["policy"] = replay_policy(policy, header, rgb, result)
            if len(samples) == 2048:
                raise RuntimeError("Diagnostic window exceeds the maximum of 2,048 sampled frames")
            samples.append(sample)
    if not samples:
        raise RuntimeError(f"No native timestamps in the selected window for {clip['file']}")
    return {
        "file": clip["file"], "sha256": clip["sha256"], "onset_s": clip["onset_s"],
        "window_s": window, "samples": samples,
    }


def annotate(trace, annotation, opening_run, limits):
    roi = annotation["roi"]
    min_iou, min_score = limits["minimum_iou"], limits["minimum_diagnostic_score"]
    cls = annotation["class"]
    expected = "fire" if cls == "flame" else cls
    first_raw = first_above_cutoff = first_accepted = None
    matched_proposals = 0
    top_score = None
    policy_regions = []
    for sample in trace["samples"]:
        h, w = sample["source_dimensions"]

        def associated(region):
            return region is not None and overlap([region["x1"] / w, region["y1"] / h, region["x2"] / w,
                                                   region["y2"] / h], roi) >= min_iou

        candidates = [item for item in sample["trace"][cls]["top"] if associated(item["source_box"])]
        at = sample["source_us"]
        if candidates:
            best = candidates[0]
            top_score = best["score"] if top_score is None else max(top_score, best["score"])
            if best["score"] >= min_score:
                matched_proposals += 1
                if first_raw is None:
                    first_raw = {"source_us": at, "anchor": best["anchor"], "score": best["score"],
                                 "disposition": best["disposition"]}
            if first_above_cutoff is None and best["score"] > sample["trace"]["legacy_threshold"]:
                first_above_cutoff = {"source_us": at, "anchor": best["anchor"], "score": best["score"],
                                      "disposition": best["disposition"]}
        emitted = [o for o in sample["observations"] if o["label"] == expected and associated(o["box"])]
        if first_accepted is None and emitted:
            first_accepted = {"source_us": at, "score": max(o["score"] for o in emitted)}
        for region in sample.get("policy", {}).get("regions", []):
            facts = region["facts"]
            if associated(region["region"]) and facts["smoke_visible" if cls == "smoke" else "flame_visible"]:
                policy_regions.append({"source_us": at, "decision": region["decision"],
                                       "visibility": facts["visibility"], "continuity_samples": facts["continuity_samples"],
                                       "strong_detection": facts["strong_detection"]})
    start, end = trace["window_s"]
    saved_openings = [event for event in opening_run.get("events", []) if event["category"] == "fire"]
    first_durable = min(saved_openings, key=lambda event: event["opened_source_s"], default=None)
    within = first_durable is not None and start <= first_durable["opened_source_s"] <= end
    source_coverage = opening_run.get("coverage", {}).get("source_time", {}).get("fire")
    # The saved Core openings were sampled on a separate paced pass over the same clip.
    attribution = ("no_matching_top_k_proposal" if first_raw is None else
                   "matching_proposal_below_legacy_cutoff" if first_above_cutoff is None else
                   "matching_proposal_lost_in_postprocess" if first_accepted is None else
                   "matching_observations_available_downstream_stage_unresolved" if not within else
                   "in_window_durable_opening_grounding_unverified")
    matched_review = next((region for region in policy_regions if region["decision"] == "review"), None)
    policy_stage = "unknown"
    if trace["samples"][0].get("policy") is not None:
        if matched_review:
            policy_stage = "review_on_identical_frames"
        elif policy_regions and all(region["visibility"] == "poor" for region in policy_regions):
            policy_stage = "poor_visibility"
        elif policy_regions:
            policy_stage = "no_review_from_matching_policy_region"
        else:
            policy_stage = "no_matching_policy_region"
    return {"roi": roi, "target_class": cls, "attribution": attribution, "matching_samples": matched_proposals,
            "max_matched_raw_score": top_score, "first_matching_raw": first_raw, "first_matching_over_score_cutoff": first_above_cutoff,
            "first_matching_accepted": first_accepted, "first_recorded_core_fire_opening": first_durable,
            "opening_roi_verified": False, "same_frame_selection_as_core": False,
            "opening_pass_has_full_window_coverage": covered_window(source_coverage, start, end) if source_coverage else None,
            "policy_quality_and_continuity_stage": policy_stage,
            "first_matching_policy_region": policy_regions[0] if policy_regions else None,
            "first_matching_policy_review": matched_review,
            "delivery_intent": "not_applicable_replay" if opening_run else "unknown"}


def run(args):
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    selections = json.loads(args.regions.read_text(encoding="utf-8")) if args.regions else None
    manifest_sha = digest(args.manifest)
    if selections is not None and (selections["schema_version"] != 1 or selections["manifest_sha256"] != manifest_sha):
        raise ValueError("Diagnostic regions must pin the same frozen source manifest")
    regions = selections["clips"] if selections else {}
    clips = [clip for clip in manifest["clips"] if (clip["file"] in regions if selections else
             clip.get("onset_s") is not None and clip["group"].startswith("fire_"))]
    if not clips or (selections and set(regions) != {clip["file"] for clip in clips}):
        raise ValueError("Diagnostic clips must belong to the frozen source manifest")
    if args.diagnostic and len(clips) > 32:
        raise ValueError("A diagnostic run accepts at most 32 clips")
    if selections:
        if any(not 0 < selections["limits"][field] <= 1 for field in ("minimum_iou", "minimum_diagnostic_score")):
            raise ValueError("Diagnostic limits must be greater than zero and at most one")
        for entry in regions.values():
            x1, y1, x2, y2 = entry["roi"]
            if entry["class"] not in ("smoke", "flame") or not (0 <= x1 < x2 <= 1 and 0 <= y1 < y2 <= 1):
                raise ValueError("Invalid diagnostic search region")
    config = {
        "library": str(args.runtime), "library_sha256": digest(args.runtime),
        "models": str(args.models), "manifest_sha256": digest(args.models / "models.lock.json"),
        "threads": 3,
    }
    opening_report = json.loads(args.openings.read_text(encoding="utf-8")) if args.openings else None
    if opening_report and opening_report["manifest_sha256"] != manifest_sha:
        raise ValueError("Saved openings must pin the same frozen source manifest")
    output = {"schema_version": 2 if args.diagnostic else 1, "sampling_interval_us": args.interval_ms * 1000,
              "worker_sha256": digest(args.worker), "manifest_sha256": manifest_sha,
              "model_manifest_sha256": config["manifest_sha256"],
              "runtime_sha256": config["library_sha256"]}
    if selections:
        output["diagnostic_regions_sha256"] = digest(args.regions)
        output["diagnostic_limits"] = selections["limits"]
    if opening_report:
        output["opening_report_sha256"] = digest(args.openings)
        output["opening_core_sha256"] = opening_report.get("core_sha256")
    if args.policy_core:
        output["policy_core_sha256"] = digest(args.policy_core)
        output["policy_scope"] = "same_sampled_rgb_default_unreviewed_scene_without_scheduler_or_persistence"
    encode = json.JSONEncoder(separators=(",", ":"), allow_nan=False)
    with TemporaryDirectory(prefix="fire-diagnostic-", dir=args.output.parent) as temporary:
        report = Path(temporary) / "report.json"
        with ExitStack() as stack:
            out = stack.enter_context(report.open("w", encoding="utf-8", newline="\n"))
            inference = stack.enter_context(process(args.worker, "diagnose-fire" if args.diagnostic else "infer"))
            policy = stack.enter_context(process(args.policy_core, "trace-fire-policy")) if args.policy_core else None
            if policy and exact(policy.stdout, 4) != b"SWP1":
                raise RuntimeError("Core does not support the local policy trace")
            out.write(encode.encode(output)[:-1] + ',"clips":[')
            body = json.dumps(config).encode()
            inference.stdin.write(struct.pack("<I", len(body)) + body)
            inference.stdin.flush()
            if exact(inference.stdout, 4) != (b"SWD1" if args.diagnostic else b"SWI1"):
                raise RuntimeError("Inference worker failed to load the locked model pack")
            for index, clip in enumerate(clips):
                path = (args.media / clip["file"]).resolve(strict=True)
                if not path.is_relative_to(args.media) or digest(path) != clip["sha256"]:
                    raise ValueError(f"Media does not match the frozen manifest for {clip['file']}")
                onset = clip["onset_s"]
                entry = regions.get(clip["file"], {})
                window = entry["window_s"] if "window_s" in entry else (
                    max(0, onset - manifest["thresholds"]["before_onset_tolerance_s"]),
                    min(clip["duration_s"], onset + manifest["thresholds"]["max_delay_s"]))
                if not 0 <= window[0] < window[1] <= clip["duration_s"] or window[1] - window[0] > 30:
                    raise ValueError(f"Invalid or excessive diagnostic window for {clip['file']}")
                result = analyze(inference, args.worker, clip, path, window, args.interval_ms * 1000, args.diagnostic, policy)
                if entry:
                    prior = opening_report["per_clip_rust"].get(clip["file"], {}) if opening_report else {}
                    result["stages"] = annotate(result, entry, prior, selections["limits"])
                if index:
                    out.write(",")
                out.write(encode.encode(result))
                print(f"Read {len(result['samples'])} frames for {clip['file']}", flush=True)
            out.write("]}\n")
        report.replace(args.output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("manifest", "media", "worker", "runtime", "models"):
        parser.add_argument(f"--{field}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--interval-ms", type=int, default=100)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--diagnostic", action="store_true", help="Collect bounded pre-threshold anchor diagnostics.")
    parser.add_argument("--regions", type=Path, help="Frozen diagnostic clip windows and manually selected search regions.")
    parser.add_argument("--openings", type=Path, help="Optionally compare a prior report from the same clip manifest.")
    parser.add_argument("--policy-core", type=lambda path: Path(path).resolve(strict=True),
                        help="Run the default Core fire policy on exactly the sampled frames.")
    arguments = parser.parse_args()
    if not 50 <= arguments.interval_ms <= 1000:
        parser.error("--interval-ms must be between 50 and 1000")
    if (arguments.regions or arguments.openings or arguments.policy_core) and not arguments.diagnostic:
        parser.error("--regions, --openings and --policy-core require --diagnostic")
    run(arguments)

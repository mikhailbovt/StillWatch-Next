"""Compare Fire geometries or one detector on identical native development frames."""

import argparse
from contextlib import ExitStack
import hashlib
import json
import math
from pathlib import Path
import struct
from time import perf_counter_ns

from compare_fire_backends import call_inference, call_policy
from event_scoring import overlap
from fire_observations import digest, exact, process


VARIANTS = {"rect640": ("infer-v2-proposals",), "rect960": ("diagnose-fire-scale", "960"),
            "tiles640": ("diagnose-fire-scale", "tiles")}


def samples(worker, media, start, stop, fps):
    start_us, stop_us = round(start * 1e6), round(stop * 1e6)
    origin = None
    last_slot = -1
    with process(worker, "capture") as child:
        child.stdin.write(json.dumps({"source": {"kind": "replay", "file": str(media), "pace": False},
                                     "masks": []}).encode())
        child.stdin.close()
        while marker := child.stdout.read(4):
            if marker != b"SWF1":
                raise RuntimeError("invalid capture response")
            header = marker + exact(child.stdout, 52)
            width, height, length = struct.unpack_from("<III", header, 8)
            if not (1 <= width <= 3840 and 1 <= height <= 2160 and length == width * height * 3):
                raise RuntimeError("invalid capture dimensions")
            rgb = exact(child.stdout, length)
            if header[4:6] != b"\x01\x00":
                continue
            pts, = struct.unpack_from("<q", header, 40)
            origin = pts if origin is None else origin
            elapsed = pts - origin
            if elapsed >= stop_us:
                break
            slot = (elapsed - start_us) * fps // 1_000_000
            if elapsed >= start_us and slot > last_slot:
                last_slot = slot
                yield header, rgb, width, height, pts, elapsed


def p95(values):
    return sorted(values)[math.ceil(0.95 * len(values)) - 1]


def evaluate(args, clip, window, annotation, min_iou, workers, policies, alternative):
    path = (args.media / clip["file"]).resolve(strict=True)
    if not path.is_relative_to(args.media) or digest(path) != clip["sha256"]:
        raise ValueError("clip does not match the pinned manifest")
    for policy in policies.values():
        policy.stdin.write(b"\0" * 4)
    source_hash = hashlib.sha256()
    frames = []
    clock = []
    dimensions = set()
    rows = {name: [] for name in policies}
    target = {"smoke": "smoke", "flame": "fire"}[annotation["class"]] if annotation else None
    target_fact = "smoke_visible" if target == "smoke" else "flame_visible"
    names = list(policies)
    for header, rgb, width, height, pts, elapsed in samples(args.worker, path, *window, args.fps):
        source_hash.update(header)
        source_hash.update(rgb)
        frames.append(elapsed)
        dimensions.add((width, height))

        def match(box):
            return annotation and overlap([box["x1"] / width, box["y1"] / height,
                                           box["x2"] / width, box["y2"] / height], annotation["roi"]) >= min_iou

        # Rotate order across frames so each model sees the same distribution of positions.
        offset = (len(frames) - 1) % len(names)
        for name in names[offset:] + names[:offset]:
            start = perf_counter_ns()
            if name == "alternative":
                result, timing = alternative.evaluate(rgb, width, height, pts)
            else:
                result, timing = call_inference(workers[name], header, rgb, timings=True)
            round_trip_us = (perf_counter_ns() - start) // 1000
            trace = call_policy(policies[name], header, rgb, result)
            if int(trace["source_us"]) != pts:
                raise RuntimeError("policy response does not match the requested frame")
            reviews = [row for row in trace["regions"] if row["decision"] == "review"]
            observations = result["inference"]["fire"]
            proposals = result.get("fire_proposals", [])
            rows[name].append({
                "source_us": elapsed, "round_trip_us": round_trip_us,
                "preprocess_us": timing[1], "fire_run_us": timing[4], "decode_us": timing[5],
                "observations": len(observations), "weak_proposals": len(proposals),
                "matching_observation": any(o["label"] == target and match(o["box"]) for o in observations),
                "matching_weak_proposal": any(o["label"] == target and match(o["box"]) for o in proposals),
                "matching_review": any(r["facts"][target_fact] and match(r["region"]) for r in reviews),
                "reviews": len(reviews),
            })
        clock.append(pts)
    if not frames or any(b <= a for a, b in zip(clock, clock[1:])):
        raise RuntimeError("comparison needs increasing native source coverage")
    by_variant = {}
    for name, values in rows.items():
        times = {f"first_{key}_us": next((r["source_us"] for r in values if r[key]), None)
                 for key in ("matching_observation", "matching_weak_proposal", "matching_review", "reviews")}
        cost = {f"p95_{key}": p95([r[key] for r in values])
                for key in ("round_trip_us", "preprocess_us", "fire_run_us", "decode_us")}
        by_variant[name] = {**times, **cost, "review_samples": sum(r["reviews"] > 0 for r in values),
                            "frames": values}
    return {"clip": clip["file"], "group": clip["group"], "window_s": window, "onset_s": clip.get("onset_s"),
            "dimensions": sorted(dimensions), "samples": len(frames), "first_sample_us": frames[0],
            "last_sample_us": frames[-1], "max_sample_gap_us": max((b - a for a, b in zip(frames, frames[1:])), default=0),
            "source_frame_sha256": source_hash.hexdigest(), "variants": by_variant}


def run(args):
    manifest_sha = digest(args.manifest)
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    windows = json.loads(args.windows.read_text(encoding="utf-8"))
    regions = json.loads(args.regions.read_text(encoding="utf-8"))
    if windows["manifest_sha256"] != manifest_sha or regions["manifest_sha256"] != manifest_sha:
        raise ValueError("windows and regions must pin the development manifest")
    clips = {clip["file"]: clip for clip in manifest["clips"]}
    if not 1 <= len(windows["clips"]) <= 32 or windows["clips"].keys() - clips.keys():
        raise ValueError("unknown or excessive clip selection")
    pack_digest = digest(args.models / "models.lock.json")
    config = {"models": str(args.models), "manifest_sha256": pack_digest, "library": str(args.runtime),
              "library_sha256": digest(args.runtime), "threads": args.threads}
    if args.cuda:
        provider = args.runtime.with_name("libonnxruntime_providers_cuda.so")
        shared = args.runtime.with_name("libonnxruntime_providers_shared.so")
        config["cuda"] = {"device": 0, "provider_sha256": digest(provider), "shared_sha256": digest(shared)}
    reports = []
    variants = VARIANTS
    alternative = None
    if args.alternative is not None:
        from compare_fire_model import Candidate

        variants = {"rect640": VARIANTS["rect640"]}
        alternative = Candidate(args.alternative, args.threads, args.cuda)
    with ExitStack() as stack:
        workers = {name: stack.enter_context(process(args.worker, *command)) for name, command in variants.items()}
        names = [*variants, "alternative"] if alternative is not None else list(variants)
        policies = {name: stack.enter_context(process(args.core, "trace-fire-policy")) for name in names}
        for worker in workers.values():
            payload = json.dumps(config).encode()
            worker.stdin.write(struct.pack("<I", len(payload)) + payload)
            worker.stdin.flush()
        for worker in workers.values():
            if exact(worker.stdout, 4) != b"SWI2":
                raise RuntimeError("invalid inference handshake")
        for policy in policies.values():
            if exact(policy.stdout, 4) != b"SWP1":
                raise RuntimeError("invalid policy handshake")
        for filename, spec in windows["clips"].items():
            clip = clips[filename]
            onset = clip.get("onset_s")
            limit = clip["duration_s"]
            window = spec.get("window_s")
            if window is None and onset is not None:
                window = [max(0, onset - 2), min(limit, onset + 8)]
            if not window or not 0 <= window[0] < window[1] <= limit or window[1] - window[0] > 30:
                raise ValueError("invalid or excessive test window")
            annotation = regions["clips"].get(filename)
            row = evaluate(args, clip, window, annotation, regions["limits"]["minimum_iou"], workers, policies, alternative)
            reports.append(row)
            print(filename, row["samples"], [(name, data["first_matching_review_us"], data["review_samples"])
                                             for name, data in row["variants"].items()], flush=True)
    args.output.write_text(json.dumps({
        "scope": "development_only_matched_native_frames_default_policy_no_storage_or_delivery",
        "fps": args.fps, "threads_per_worker": args.threads, "provider": "cuda" if args.cuda else "cpu",
        "roi_match_iou": regions["limits"]["minimum_iou"],
        "manifest_sha256": manifest_sha, "regions_sha256": digest(args.regions), "windows_sha256": digest(args.windows),
        "worker_sha256": digest(args.worker), "core_sha256": digest(args.core), "model_manifest_sha256": pack_digest,
        "runtime_sha256": config["library_sha256"], "cuda": config.get("cuda"),
        "alternative": alternative.provenance if alternative else None, "clips": reports,
    }, indent=2, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("manifest", "windows", "regions", "media", "worker", "core", "models", "runtime"):
        parser.add_argument(f"--{option}", required=True, type=lambda p: Path(p).resolve(strict=True))
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--fps", type=int, choices=(2, 3, 5), default=2)
    parser.add_argument("--threads", type=int, choices=range(1, 9), default=4)
    parser.add_argument("--cuda", action="store_true")
    parser.add_argument("--alternative", type=Path, help="compare the pinned development ONNX against native 640")
    run(parser.parse_args())

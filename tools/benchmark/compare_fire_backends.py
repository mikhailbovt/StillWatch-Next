"""Compare CPU and CUDA Fire on the same capture headers, RGB frames and source clock."""

import argparse
from contextlib import ExitStack
import hashlib
import json
from pathlib import Path
import struct
import uuid

from fire_observations import digest, exact, process


def call_inference(child, header, rgb, *, timings=False):
    key = uuid.uuid4()
    child.stdin.write(key.bytes + b"\x02" + header + rgb)
    child.stdin.flush()
    size, = struct.unpack("<I", exact(child.stdout, 4))
    if not 0 < size <= 1_500_000:
        raise ValueError("invalid worker result")
    outcome = json.loads(exact(child.stdout, size))
    timing = exact(child.stdout, 28)
    result = outcome["inference"]
    if result["request"] != str(key) or result["pose"] is not None or result["fire"] is None:
        raise ValueError("invalid Fire response")
    result["request"] = str(uuid.UUID(int=0))
    result["elapsed_ms"] = 0
    return (outcome, struct.unpack("<7I", timing)) if timings else outcome


def call_policy(child, header, rgb, result):
    data = json.dumps(result, separators=(",", ":")).encode()
    child.stdin.write(struct.pack("<I", len(data)) + data + header + rgb)
    child.stdin.flush()
    size, = struct.unpack("<I", exact(child.stdout, 4))
    if not 0 < size <= 1_500_000:
        raise ValueError("invalid policy result")
    trace = json.loads(exact(child.stdout, size))
    expected_us, = struct.unpack_from("<q", header, 40)
    if int(trace["source_us"]) != expected_us:
        raise ValueError("policy response belongs to a different source timestamp")
    return trace


def decisions(trace):
    return sorted((r["decision"], r["facts"]["visibility"],
                   r["facts"]["flame_visible"], r["facts"]["smoke_visible"])
                  for r in trace["regions"])


def sample_slot(source_us, start_us, fps, phase_us):
    elapsed_us = source_us - start_us - phase_us
    return None if elapsed_us < 0 else elapsed_us * fps // 1_000_000


def compare_window(args, clip, window, phase_us, workers, policies):
    path = (args.media / clip["file"]).resolve(strict=True)
    if not path.is_relative_to(args.media) or digest(path) != clip["sha256"]:
        raise ValueError("clip does not match development manifest")
    for policy in policies.values():
        policy.stdin.write(b"\0" * 4)
    sample_hash = hashlib.sha256()
    source_start = None
    first_review = dict.fromkeys(workers)
    reviews = dict.fromkeys(workers, 0)
    differences = []
    graph_differences = []
    selected_source_us = []
    last_slot = -1
    start_us, stop_us = (round(s * 1_000_000) for s in window)
    with process(args.worker, "capture") as capture:
        capture.stdin.write(json.dumps({
            "source": {"kind": "replay", "file": str(path), "pace": False}, "masks": [],
        }).encode())
        capture.stdin.close()
        while marker := capture.stdout.read(4):
            if marker != b"SWF1":
                raise ValueError("invalid capture frame")
            header = marker + exact(capture.stdout, 52)
            length, = struct.unpack_from("<I", header, 16)
            if not 0 < length <= 3840 * 2160 * 3:
                raise ValueError("invalid frame size")
            rgb = exact(capture.stdout, length)
            if header[4:6] != b"\x01\x00":
                continue
            pts, = struct.unpack_from("<q", header, 40)
            if source_start is None:
                source_start = pts
            elapsed = pts - source_start
            if elapsed >= stop_us:
                break
            slot = sample_slot(elapsed, start_us, args.fps, phase_us)
            if slot is None or slot <= last_slot:
                continue
            last_slot = slot
            selected_source_us.append(elapsed)
            sample_hash.update(header)
            sample_hash.update(rgb)
            outputs = {name: call_inference(worker, header, rgb) for name, worker in workers.items()}
            if "cuda_graph" in outputs and outputs["cuda_graph"] != outputs["cuda"]:
                graph_differences.append(elapsed)
            values = {name: decisions(call_policy(policies[name], header, rgb, output))
                      for name, output in outputs.items()}
            for name, row in values.items():
                if any(item[0] == "review" for item in row):
                    reviews[name] += 1
                    if first_review[name] is None:
                        first_review[name] = elapsed
            if any(value != values["cpu"] for value in values.values()):
                differences.append({"source_us": elapsed, **values})
    if not selected_source_us:
        raise ValueError("empty comparison window")
    report = {"clip": clip["file"], "window_s": window, "phase_offset_us": phase_us,
              "matched_input_digest": sample_hash.hexdigest(), "selected_source_us": selected_source_us,
              "samples": len(selected_source_us), "review_samples": list(reviews.values()),
              "first_review_us": list(first_review.values()), "policy_differences": differences}
    if "cuda_graph" in workers:
        report["cuda_graph_observation_differences_us"] = graph_differences
    return report


def run(args):
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    selections = json.loads(args.windows.read_text(encoding="utf-8"))
    manifest_hash = digest(args.manifest)
    if selections["manifest_sha256"] != manifest_hash:
        raise ValueError("comparison windows must pin the development manifest")
    windows = selections["clips"]
    clips = {clip["file"]: clip for clip in manifest["clips"]}
    if not 1 <= len(windows) <= 32 or windows.keys() - clips.keys():
        raise ValueError("unknown or excessive clip selection")
    provider, shared = args.cuda_provider, args.cuda_shared
    reports = []
    pack_digest = digest(args.models / "models.lock.json")
    configurations = {}
    cuda = {"device": args.cuda_device, "provider_sha256": digest(provider), "shared_sha256": digest(shared)}
    for name, runtime in (("cpu", args.cpu_runtime), ("cuda", args.cuda_runtime)):
        body = {"models": str(args.models), "manifest_sha256": pack_digest,
                "library": str(runtime), "library_sha256": digest(runtime), "threads": 4}
        configurations[name] = body if name == "cpu" else body | {"cuda": cuda}
    if args.cuda_graph_shape is not None:
        configurations["cuda_graph"] = configurations["cuda"] | {"cuda": cuda | {"graph_shape": args.cuda_graph_shape}}
    with ExitStack() as stack:
        workers = {name: stack.enter_context(process(args.worker, "infer-v2-proposals")) for name in configurations}
        policies = {name: stack.enter_context(process(args.policy_core, "trace-fire-policy")) for name in configurations}
        for policy in policies.values():
            if exact(policy.stdout, 4) != b"SWP1":
                raise ValueError("invalid policy handshake")
        for name, worker in workers.items():
            wire = json.dumps(configurations[name]).encode()
            worker.stdin.write(struct.pack("<I", len(wire)) + wire)
            worker.stdin.flush()
            if exact(worker.stdout, 4) != b"SWI2":
                raise ValueError(f"invalid worker handshake for {name}")
        for filename, spec in windows.items():
            clip = clips[filename]
            onset = clip.get("onset_s")
            window = spec.get("window_s")
            if window is None and onset is not None:
                window = [max(0, onset - 2), min(clip["duration_s"], onset + 8)]
            if not window or not 0 <= window[0] < window[1] <= clip["duration_s"] or window[1] - window[0] > 30:
                raise ValueError("invalid or excessive test window")
            phase_us = spec.get("phase_offset_us", 0)
            if type(phase_us) is not int or not 0 <= phase_us * args.fps < 1_000_000:
                raise ValueError("phase offset must be an integer within one sampling period")
            row = compare_window(args, clip, window, phase_us, workers, policies)
            reports.append(row)
            print(f"{filename} {row['samples']} matched frames, {len(row['policy_differences'])} policy differences", flush=True)
    report = {"manifest_sha256": manifest_hash, "windows_sha256": digest(args.windows),
              "worker_sha256": digest(args.worker), "core_sha256": digest(args.policy_core),
              "model_manifest_sha256": pack_digest, "cpu_runtime_sha256": digest(args.cpu_runtime),
              "cuda_runtime_sha256": digest(args.cuda_runtime), "cuda_provider_sha256": digest(provider),
              "cuda_shared_sha256": digest(shared), "fps": args.fps, "backends": list(configurations),
              "development_fire_policy_equivalent": all(not row["policy_differences"] for row in reports),
              "scope": "matched native frames and detector-to-policy output; storage and delivery excluded",
              "clips": reports}
    graph_equal = all(not row.get("cuda_graph_observation_differences_us") for row in reports)
    if args.cuda_graph_shape is not None:
        report.update(cuda_graph_shape=args.cuda_graph_shape, cuda_graph_observations_equal=graph_equal)
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    return 0 if report["development_fire_policy_equivalent"] and graph_equal else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "windows", "media", "worker", "policy-core", "models", "cpu-runtime",
                 "cuda-runtime", "cuda-provider", "cuda-shared"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--fps", type=int, choices=(2, 3, 5, 8), default=2)
    parser.add_argument("--cuda-device", type=int, default=0)
    parser.add_argument("--cuda-graph-shape", type=int, nargs=2, metavar=("HEIGHT", "WIDTH"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 0 <= args.cuda_device <= 65535:
        parser.error("CUDA device index must be in 0..65535")
    if args.cuda_graph_shape and any(not 32 <= side <= 640 or side % 32 for side in args.cuda_graph_shape):
        parser.error("CUDA Graph requires positive stride-32 tensor dimensions up to 640")
    raise SystemExit(run(args))

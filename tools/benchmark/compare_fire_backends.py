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
    return json.loads(exact(child.stdout, size))


def decisions(trace):
    return sorted((r["decision"], r["facts"]["visibility"],
                   r["facts"]["flame_visible"], r["facts"]["smoke_visible"])
                  for r in trace["regions"])


def evaluate(args, clip, window, workers, policies):
    path = (args.media / clip["file"]).resolve(strict=True)
    if not path.is_relative_to(args.media) or digest(path) != clip["sha256"]:
        raise ValueError("clip does not match development manifest")
    for policy in policies:
        policy.stdin.write(b"\0" * 4)
    sample_hash = hashlib.sha256()
    source_start = None
    first_review = [None, None]
    reviews = [0, 0]
    mismatches = []
    samples = 0
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
            if elapsed < start_us:
                continue
            if elapsed >= stop_us:
                break
            slot = (elapsed - start_us) * args.fps // 1_000_000
            if slot <= last_slot:
                continue
            last_slot = slot
            sample_hash.update(header)
            sample_hash.update(rgb)
            values = [decisions(call_policy(policy, header, rgb, call_inference(worker, header, rgb)))
                      for worker, policy in zip(workers, policies, strict=True)]
            for i, row in enumerate(values):
                if any(item[0] == "review" for item in row):
                    reviews[i] += 1
                    if first_review[i] is None:
                        first_review[i] = elapsed
            if values[0] != values[1]:
                mismatches.append({"source_us": elapsed, "cpu": values[0], "cuda": values[1]})
            samples += 1
    if not samples:
        raise ValueError("empty comparison window")
    return {"clip": clip["file"], "window_s": window, "matched_input_digest": sample_hash.hexdigest(),
            "samples": samples, "review_samples": reviews, "first_review_us": first_review,
            "policy_differences": mismatches}


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
    with ExitStack() as stack:
        workers = [stack.enter_context(process(args.worker, "infer-v2-proposals")) for _ in range(2)]
        policies = [stack.enter_context(process(args.policy_core, "trace-fire-policy")) for _ in range(2)]
        for policy in policies:
            if exact(policy.stdout, 4) != b"SWP1":
                raise ValueError("invalid policy handshake")
        for index, (worker, runtime) in enumerate(zip(workers, [args.cpu_runtime, args.cuda_runtime], strict=True)):
            body = {"models": str(args.models), "manifest_sha256": pack_digest,
                    "library": str(runtime), "library_sha256": digest(runtime), "threads": 4}
            if index == 1:
                body["cuda"] = {"device": 0, "provider_sha256": digest(provider), "shared_sha256": digest(shared)}
            wire = json.dumps(body).encode()
            worker.stdin.write(struct.pack("<I", len(wire)) + wire)
            worker.stdin.flush()
            if exact(worker.stdout, 4) != b"SWI2":
                raise ValueError("invalid worker handshake")
        for filename, spec in windows.items():
            clip = clips[filename]
            onset = clip.get("onset_s")
            window = spec.get("window_s")
            if window is None and onset is not None:
                window = [max(0, onset - 2), min(clip["duration_s"], onset + 8)]
            if not window or not 0 <= window[0] < window[1] <= clip["duration_s"] or window[1] - window[0] > 30:
                raise ValueError("invalid or excessive test window")
            row = evaluate(args, clip, window, workers, policies)
            reports.append(row)
            print(f"{filename} {row['samples']} matched frames, {len(row['policy_differences'])} policy differences", flush=True)
    report = {"manifest_sha256": manifest_hash, "windows_sha256": digest(args.windows),
              "worker_sha256": digest(args.worker), "core_sha256": digest(args.policy_core),
              "model_manifest_sha256": pack_digest, "cpu_runtime_sha256": digest(args.cpu_runtime),
              "cuda_runtime_sha256": digest(args.cuda_runtime), "cuda_provider_sha256": digest(provider),
              "cuda_shared_sha256": digest(shared), "fps": args.fps,
              "development_fire_policy_equivalent": all(not row["policy_differences"] for row in reports),
              "scope": "matched native frames and detector-to-policy output; storage and delivery excluded",
              "clips": reports}
    args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    return 0 if report["development_fire_policy_equivalent"] else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "windows", "media", "worker", "policy-core", "models", "cpu-runtime",
                 "cuda-runtime", "cuda-provider", "cuda-shared"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--fps", type=int, choices=(2, 3, 5, 8), default=2)
    parser.add_argument("--output", type=Path, required=True)
    raise SystemExit(run(parser.parse_args()))

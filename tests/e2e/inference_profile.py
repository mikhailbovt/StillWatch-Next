"""Native worker shape inventory and artifact fences."""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import time
import uuid


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read(stream, length):
    value = stream.read(length)
    if len(value) != length:
        raise RuntimeError(f"short read of expected {length} bytes")
    return value


def request_frame(child, ordinal, width, height, path):
    rgb = Path(path).read_bytes()
    identifier = uuid.uuid4()
    pts = ordinal * 200_000
    assert len(rgb) == width * height * 3
    header = struct.pack("<4sBBHIIIIQqqii", b"SWF1", 1, 0, 0, width, height, len(rgb), 0,
                         ordinal, pts, pts, 1, 1_000_000)
    began = time.monotonic()
    child.stdin.write(identifier.bytes + b"\x03" + header + rgb)
    child.stdin.flush()
    length, = struct.unpack("<I", read(child.stdout, 4))
    assert 0 < length <= 1_500_000
    response = json.loads(read(child.stdout, length))
    timing = struct.unpack("<7I", read(child.stdout, 28))
    result = response["inference"]
    if result.pop("request") != str(identifier):
        raise ValueError("reply UUID mismatch")
    del result["elapsed_ms"]
    assert result["pose"] is not None and result["fire"] is not None
    elapsed = (time.monotonic() - began) * 1000
    assert elapsed < 2_000, elapsed
    return response, elapsed, timing


def execute(args, config, frames, repeats, expected_ready):
    started = time.monotonic()
    with subprocess.Popen([args.worker, "infer-v2-proposals"], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as child:
        body = json.dumps(config).encode()
        try:
            child.stdin.write(struct.pack("<I", len(body)) + body)
            child.stdin.flush()
            ready = read(child.stdout, 4)
            startup = (time.monotonic() - started) * 1_000
            assert ready == expected_ready and startup < (60_000 if args.cuda else 30_000), (ready, startup)
            if expected_ready != b"SWI2":
                return None
            replies = [request_frame(child, i + 1, w, h, p) for i, (w, h, p) in enumerate(frames * repeats)]
            child.stdin.close()
            assert child.wait(timeout=3) == 0
            return replies, startup
        finally:
            if child.poll() is None:
                child.kill()


def run(args):
    models = Path(args.models).resolve()
    manifest = models / "models.lock.json"
    pack = json.loads(manifest.read_text(encoding="utf-8"))
    fixtures = json.loads((models / "parity-report.json").read_text(encoding="utf-8"))["cases"]
    unique = {}
    for name, case in fixtures.items():
        shape = tuple(case["tensor_hw"])
        if case["image"]:
            height, width, _ = case["image"]["rgb_shape"]
            path = models / "fixtures" / f"{name}.rgb"
            assert digest(path) == case["image"]["rgb_sha256"]
            unique.setdefault(shape, []).append((width, height, path))
    if args.warm_shape:
        selected = [tuple(value) for value in args.warm_shape]
        assert len(selected) == len(set(selected)) and set(selected) <= set(unique)
    else:
        selected = list(unique)
        assert len(selected) >= 2
    frames = [unique[shape][0] for shape in selected]
    config = {
        "library": str(Path(args.runtime).resolve()), "library_sha256": digest(args.runtime),
        "models": str(models), "manifest_sha256": digest(manifest), "threads": args.threads,
        "live_profile": {"pose_balanced_sha256": pack["models"]["pose_balanced"]["sha256"],
                         "fire_sha256": pack["models"]["fire"]["sha256"],
                         "warm_shapes": selected},
    }
    if args.cuda:
        provider, shared = args.cuda
        config["cuda"] = {"device": args.cuda_device, "provider_sha256": digest(provider), "shared_sha256": digest(shared)}
    if args.cuda_graph:
        assert args.cuda and len(selected) == 1
        shape, = selected
        frames = unique[shape]
        assert len({digest(frame[2]) for frame in frames}) >= 2
        config["cuda"]["graph_shape"] = shape
    bad = config | {"live_profile": config["live_profile"] | {"pose_balanced_sha256": pack["models"]["pose"]["sha256"]}}
    execute(args, bad, frames, 1, b"SWIM")
    bad = config | {"live_profile": config["live_profile"] | {"warm_shapes": [[1, 1]]}}
    execute(args, bad, frames, 1, b"SWIM")
    result = {"worker_sha256": digest(args.worker), "model_manifest_sha256": digest(manifest),
              "runtime_sha256": digest(args.runtime), "device": "cuda" if args.cuda else "cpu",
              "warm_shapes": selected, "threads": args.threads, "unapproved_roles_and_shapes_rejected": True}
    original, startup = execute(args, config, frames, args.repeats, b"SWI2")
    result["host_input"] = metrics(original, startup)
    if args.cuda_graph:
        other_shape = next(shape for shape in unique if shape != selected[0])
        mismatch = config | {"live_profile": config["live_profile"] | {"warm_shapes": [other_shape]}}
        execute(args, mismatch, frames, 1, b"SWIM")
        dynamic = config | {"cuda": {key: value for key, value in config["cuda"].items() if key != "graph_shape"}}
        baseline, startup = execute(args, dynamic, frames, args.repeats, b"SWI2")
        equal = [row[0] for row in original] == [row[0] for row in baseline]
        assert equal, "Captured and dynamic observations or fire proposals differ"
        result.update(cuda_graph_shape=selected[0], graph_observations_equal=equal,
                      dynamic_host_input=metrics(baseline, startup))
    print(json.dumps(result))


def metrics(replies, startup):
    durations = sorted(row[1] for row in replies)
    prep = sorted(row[2][1] for row in replies)
    pose = sorted(row[2][2] for row in replies)
    fire = sorted(row[2][4] for row in replies)
    index = (95 * len(durations) + 99) // 100 - 1
    return {"requests": len(durations), "startup_ms": startup, "round_trip_p95_ms": durations[index],
            "preprocess_p95_us": prep[index], "pose_run_p95_us": pose[index], "fire_run_p95_us": fire[index],
            "max_reply_ms": durations[-1]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("worker", "runtime", "models"):
        parser.add_argument(f"--{arg}", required=True)
    parser.add_argument("--cuda", nargs=2, metavar=("CUDA_PROVIDER", "SHARED_PROVIDER"))
    parser.add_argument("--cuda-device", type=int, default=0)
    parser.add_argument("--cuda-graph", action="store_true")
    parser.add_argument("--threads", type=int, choices=range(1, 65), default=4)
    parser.add_argument("--warm-shape", type=int, nargs=2, action="append", metavar=("HEIGHT", "WIDTH"))
    parser.add_argument("--repeats", type=int, choices=range(1, 65), default=10)
    run(parser.parse_args())

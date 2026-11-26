"""Compare Rust fall features with Training Pack v3 on native PoseBalanced receipts.

This tests the input tensor and source PTS. Tracker IDs serve as pairs for the
comparison. No labels, scores or event quality are inferred from them.
"""

from argparse import ArgumentParser
from bisect import bisect_left, bisect_right
from collections import defaultdict
import hashlib
from importlib.util import module_from_spec, spec_from_file_location
import json
from pathlib import Path
import subprocess

import numpy as np


CONTRACT = "stillwatch-causal-pose-v2"
SHAPE = (128, 21)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load_reference(path, expected):
    if digest(path) != expected:
        raise ValueError("Training reference SHA-256 differs")
    spec = spec_from_file_location("pose_training_reference", path)
    if spec is None or spec.loader is None:
        raise ValueError("Expected a Python source file for the training reference")
    reference = module_from_spec(spec)
    spec.loader.exec_module(reference)
    if (reference.CONTRACT, reference.CHANNELS, reference.STEPS) != (CONTRACT, *SHAPE):
        raise ValueError("Training reference describes a different feature contract")
    return reference


def load_native_poses(path):
    receipt_path = path.with_suffix(".receipt.json")
    receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    trace_digest = digest(path)
    if (receipt["schema"] != "stillwatch-native-pose-review-v1" or not receipt["core_tracking_executed"]
            or trace_digest != receipt["observations_sha256"]):
        raise ValueError("Expected native tracked poses and a matching receipt")
    tracks = defaultdict(list)
    with path.open(encoding="utf-8") as stream:
        for line in stream:
            frame = json.loads(line)
            for person in frame["detections"]:
                track = person["detection_id"]
                epoch = frame["source_epoch"]
                tracks[epoch, track].append({
                    "source_seconds": frame["source_seconds"],
                    "source_epoch": epoch,
                    "person_chain_id": str(track),
                    "region": person["region"],
                    "keypoints": person["keypoints"],
                    "association_uncertain": person["association_uncertain"],
                })
    return receipt, tracks


def request_window(process, rows, anchor, scope, width, height):
    epoch, track_id = scope
    observations = [{
        "source_seconds": row["source_seconds"],
        "source_epoch": epoch,
        "track_id": track_id,
        "region": dict(zip(("x1", "y1", "x2", "y2"), row["region"], strict=True)),
        "keypoints": row["keypoints"],
        "association_uncertain": row["association_uncertain"],
    } for row in rows]
    request = {"anchor": {"source_seconds": anchor, "source_epoch": epoch, "track_id": track_id,
                          "width": width, "height": height}, "observations": observations}
    process.stdin.write(json.dumps(request, allow_nan=False) + "\n")
    process.stdin.flush()
    response = process.stdout.readline()
    if not response:
        raise ValueError("Native feature runner ended before returning the tensor")
    return json.loads(response)


def compare(reference, process, path):
    receipt, tracks = load_native_poses(path)
    width, height = receipt["width"], receipt["height"]
    comparisons, changed = 0, 0
    max_abs = 0.0
    for (epoch, track_id), rows in tracks.items():
        times = [row["source_seconds"] for row in rows]
        for time in times:
            for anchor in (time, time + 0.1):
                first = max(0, bisect_left(times, anchor - 5.0 - 1e-9) - 1)
                # The extra sample exercises exclusion of a later frame.
                last = bisect_right(times, anchor + 1e-9) + 1
                selected = rows[first:last]
                expected, reference_receipt = reference.build_window(
                    selected, anchor, epoch, str(track_id), width, height)
                actual = request_window(process, selected, anchor, (epoch, track_id), width, height)
                native = np.asarray(actual["channels"], dtype=np.float32)
                if (actual["contract"] != CONTRACT or native.shape != SHAPE or not np.isfinite(native).all()
                        or actual["source_pts"] != reference_receipt["source_pts_for_grid"]):
                    raise ValueError(f"Wrong shape, contract or source PTS for {path.name} at {anchor}")
                delta = np.abs(native - expected[0])
                maximum = float(np.max(delta))
                if maximum > 1e-6:
                    raise ValueError(f"Feature mismatch for {path.name} at {anchor}, track {track_id}: {maximum}")
                changed += int(np.count_nonzero(delta))
                max_abs = max(max_abs, maximum)
                comparisons += 1
    if comparisons == 0:
        raise ValueError(f"No tracked PoseBalanced streams in {path}")
    return {"pose_receipt_sha256": digest(path.with_suffix(".receipt.json")),
            "pose_trace_sha256": receipt["observations_sha256"], "video_sha256": receipt["video_sha256"],
            "pose_model_sha256": receipt["pose_model_sha256"], "provider": receipt["provider"],
            "track_count": len(tracks), "windows": comparisons, "differing_feature_values": changed,
            "maximum_abs_error": max_abs}


def main():
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--reference-sha256", required=True)
    parser.add_argument("--pose", type=Path, action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    reference = load_reference(args.reference, args.reference_sha256)
    with subprocess.Popen([str(args.rust.resolve(strict=True))], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          text=True, encoding="utf-8") as process:
        try:
            observations = [compare(reference, process, path) for path in args.pose]
        finally:
            process.stdin.close()
        if process.wait() != 0:
            raise ValueError("Native feature runner failed")
    report = {"contract": CONTRACT, "batch_shape": [1, *SHAPE], "atol": 1e-6,
              "rust_sha256": digest(args.rust), "reference_sha256": args.reference_sha256,
              "windows": sum(row["windows"] for row in observations),
              "maximum_abs_error": max(row["maximum_abs_error"] for row in observations),
              "observations": observations}
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
    print(json.dumps(report))


if __name__ == "__main__":
    main()

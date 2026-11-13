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
import uuid


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def exact(stream, size):
    data = stream.read(size)
    if len(data) != size:
        raise RuntimeError("Unexpected end of native worker output")
    return data


@contextmanager
def process(executable, command):
    with subprocess.Popen([str(executable), command], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as child:
        try:
            yield child
        finally:
            if child.poll() is None:
                child.terminate()


def analyze(inference, executable, clip, path, window, interval_us):
    command = {"source": {"kind": "replay", "file": str(path), "pace": False}, "masks": []}
    start, stop = window
    next_us = math.ceil(start * 1_000_000)
    samples = []
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
            if result["request"] != str(request) or result["pose"] is not None or result["fire"] is None:
                raise RuntimeError("Invalid inference response")
            samples.append({"source_us": source_us, "observations": [
                {"label": item["label"], "score": item["score"], "box": item["box"]}
                for item in result["fire"]
            ]})
    if not samples:
        raise RuntimeError(f"No native timestamps in the selected window for {clip['file']}")
    return {
        "file": clip["file"], "sha256": clip["sha256"], "onset_s": clip["onset_s"],
        "window_s": window, "samples": samples,
    }


def run(args):
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    clips = [clip for clip in manifest["clips"] if clip.get("onset_s") is not None
             and clip["group"].startswith("fire_")]
    if not clips:
        raise ValueError("The manifest contains no annotated fire onset")
    config = {
        "library": str(args.runtime), "library_sha256": digest(args.runtime),
        "models": str(args.models), "manifest_sha256": digest(args.models / "models.lock.json"),
        "threads": 3,
    }
    output = {"schema_version": 1, "sampling_interval_us": args.interval_ms * 1000,
              "worker_sha256": digest(args.worker), "manifest_sha256": digest(args.manifest),
              "model_manifest_sha256": config["manifest_sha256"],
              "runtime_sha256": config["library_sha256"], "clips": []}
    with process(args.worker, "infer") as inference:
        body = json.dumps(config).encode()
        inference.stdin.write(struct.pack("<I", len(body)) + body)
        inference.stdin.flush()
        if exact(inference.stdout, 4) != b"SWI1":
            raise RuntimeError("Inference worker failed to load the locked model pack")
        for clip in clips:
            path = (args.media / clip["file"]).resolve(strict=True)
            if not path.is_relative_to(args.media) or digest(path) != clip["sha256"]:
                raise ValueError(f"Media does not match the frozen manifest for {clip['file']}")
            onset = clip["onset_s"]
            window = (max(0, onset - manifest["thresholds"]["before_onset_tolerance_s"]),
                      min(clip["duration_s"], onset + manifest["thresholds"]["max_delay_s"]))
            output["clips"].append(analyze(inference, args.worker, clip, path, window, args.interval_ms * 1000))
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("manifest", "media", "worker", "runtime", "models"):
        parser.add_argument(f"--{field}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--interval-ms", type=int, default=100)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    if not 50 <= arguments.interval_ms <= 1000:
        parser.error("--interval-ms must be between 50 and 1000")
    run(arguments)

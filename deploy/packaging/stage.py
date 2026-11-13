"""Assemble a native CPU payload with verified artifacts and an inventory."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import re
import shutil
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LOCK = ROOT / "models/native-deps.lock.json"
MODEL_FILES = ("models.lock.json", "parity-report.json", "pose.onnx", "pose_balanced.onnx", "fire.onnx")
BINARIES = ("stillwatch-core", "stillwatch-worker", "stillwatch-cli", "stillwatch-alarm-agent")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def copy(source, destination, expected=None):
    if not source.is_file():
        raise ValueError(f"Missing artifact {source.name}")
    if expected is not None and digest(source) != expected:
        raise ValueError(f"Digest mismatch for {source.name}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def capture_libraries(args, system, out, spec, target):
    capture = args.capture_libav
    license_path = capture / "share/stillwatch-ffmpeg"
    recipe = ROOT / "tools/dev"
    patch = "ffmpeg-same-peer.patch"
    if digest(license_path / patch) != spec["patch_sha256"]:
        raise ValueError("Capture libraries were built with a different FFmpeg patch")
    for library, expected in spec[target].items():
        directory = "bin" if system == "windows" else "lib"
        destination = "bin" if system == "windows" else "capture-runtime"
        copy(capture / directory / library, out / destination / library, expected)
    for name in ("LICENSE", "source.txt", "configure.txt"):
        copy(license_path / name, out / "licenses" / f"ffmpeg-capture-{name}")
    copy(recipe / patch, out / "licenses" / patch, spec["patch_sha256"])
    copy(recipe / "build-ffmpeg-capture.sh", out / "licenses/build-ffmpeg-capture.sh")
    archive = f"ffmpeg-{spec['native_version']}.tar.xz"
    copy(args.capture_source, out / "licenses" / archive, spec["source_sha256"])
    if system == "linux":
        worker = out / "bin/stillwatch-worker"
        worker.rename(out / "bin/stillwatch-worker-native")
        copy(ROOT / "deploy/linux/stillwatch-worker", worker)
        worker.chmod(0o755)
    return {
        "version": spec["native_version"],
        "source_sha256": spec["source_sha256"],
        "patch_sha256": spec["patch_sha256"],
    }


def media_encoder(args, out, suffix, spec, target):
    filename = "ffmpeg" + suffix
    expected = spec[target]["executable_sha256"]
    copy(args.encoder / "bin" / filename, out / "encoder" / filename, expected)
    (out / "encoder/ffmpeg.sha256").write_text(expected + "\n", encoding="ascii", newline="\n")
    notices = args.encoder / "share/stillwatch-encoder"
    for name in ("FFmpeg-LICENSE", "OpenH264-LICENSE", "configure.txt", "source.txt"):
        copy(notices / name, out / "licenses" / f"encoder-{name}")
    archive = f"openh264-{spec['openh264_version']}.tar.gz"
    copy(notices / archive, out / "licenses" / archive, spec["openh264_source_sha256"])
    copy(ROOT / "tools/dev/build-ffmpeg-encoder.sh", out / "licenses/build-ffmpeg-encoder.sh")
    return {"version": spec["native_version"], "openh264": spec["openh264_version"], "sha256": expected}


def main(args):
    system = platform.system().lower()
    if system not in ("windows", "linux") or platform.machine().lower() not in ("amd64", "x86_64"):
        raise ValueError("The pinned native payload supports Windows and Linux x64")
    if args.output.exists() and any(args.output.iterdir()):
        raise ValueError("Use an empty output directory")
    args.output.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if system == "windows" else ""
    native = json.loads(LOCK.read_text(encoding="utf-8"))
    target = f"{system}-x64"
    ort = native["ort"][target]
    mtx = native["mediamtx"][target]
    out = args.output
    for name in BINARIES:
        copy(args.binaries / (name + suffix), out / "bin" / (name + suffix))
    if system == "windows":
        if args.vc_runtime is None:
            raise ValueError("Supply the locked Microsoft Visual C++ x64 redistributable directory")
        for name, expected in native["vc_runtime"][target].items():
            copy(args.vc_runtime / name, out / "bin" / name, expected)
    capture = capture_libraries(args, system, out, native["ffmpeg_capture"], target)
    encoder = media_encoder(args, out, suffix, native["ffmpeg_encoder"], target)
    ort_path = args.native / ort["library"]
    ort_file = ort_path.name
    ort_root = ort_path.parent.parent
    copy(ort_path, out / "runtime" / ort_file, ort["library_sha256"])
    for name in ("LICENSE", "ThirdPartyNotices.txt"):
        copy(ort_root / name, out / "licenses" / f"onnxruntime-{name}")
    media = args.mediamtx / ("mediamtx" + suffix)
    copy(media, out / "bin" / media.name, mtx["executable_sha256"])
    copy(args.mediamtx / "LICENSE", out / "licenses/mediamtx-LICENSE")
    copy(ROOT / "LICENSE", out / "licenses/stillwatch-LICENSE")
    copy(ROOT / "NOTICE", out / "licenses/stillwatch-NOTICE")
    copy(LOCK, out / "licenses/native-deps.lock.json")
    copy(ROOT / "docs/operations.md", out / "docs/operations.md")
    for doc in ("firebase-worker.md", "remote.md"):
        copy(ROOT / "docs" / doc, out / "docs" / doc)
    copy(ROOT / "deploy/nginx/stillwatch.conf", out / "docs/nginx/stillwatch.conf")
    worker = (out / "bin" / ("stillwatch-worker" + suffix)).resolve()
    report = subprocess.run([str(worker), "capture-doctor"], timeout=10, check=True, text=True, capture_output=True)
    if json.loads(report.stdout).get("rtsp_peer_confined") is not True:
        raise ValueError("Staged capture worker lacks RTSP peer confinement")
    model_digest = None
    if args.models is not None:
        if args.model_sha256 is None or not re.fullmatch(r"[0-9a-f]{64}", args.model_sha256):
            raise ValueError("A trusted model manifest SHA-256 is required")
        model_digest = args.model_sha256
        for name in MODEL_FILES:
            copy(args.models / name, out / "models" / name, model_digest if name == "models.lock.json" else None)
        if args.model_notices is not None:
            copy(args.model_notices, out / "licenses/model-notices")
        completed = subprocess.run([
            str(worker), "doctor",
            str((out / "runtime" / ort_file).resolve()), ort["library_sha256"],
            str((out / "models").resolve()), model_digest,
        ], timeout=30, check=True, text=True, capture_output=True)
        if json.loads(completed.stdout).get("models") != 3:
            raise ValueError("Model pack startup verification did not succeed")
    revision = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
    status = subprocess.check_output(["git", "-C", str(ROOT), "status", "--porcelain"], text=True)
    with (ROOT / "rust-toolchain.toml").open("rb") as toolchain:
        rust_version = tomllib.load(toolchain)["toolchain"]["channel"]
    metadata = {
        "schema_version": 1, "version": args.version, "target": target, "profile": "cpu-fp32",
        "source_revision": revision, "source_dirty": bool(status),
        "rust_toolchain": rust_version,
        "cargo_lock_sha256": digest(ROOT / "Cargo.lock"), "native_lock_sha256": digest(LOCK),
        "api_major": 2, "api_minor": 0, "model_manifest_sha256": model_digest, "ort": ort_file,
        "ort_sha256": ort["library_sha256"], "mediamtx_sha256": mtx["executable_sha256"],
        "ffmpeg_capture": capture, "ffmpeg_encoder": encoder,
        "files": {path.relative_to(out).as_posix(): digest(path) for path in sorted(out.rglob("*")) if path.is_file()},
    }
    (out / "manifest.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    print(f"Staged {target} CPU payload with {len(metadata['files'])} files")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binaries", "native", "mediamtx", "encoder", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--capture-libav", type=Path, required=True)
    parser.add_argument("--capture-source", type=Path, required=True)
    parser.add_argument("--vc-runtime", type=Path)
    parser.add_argument("--models", type=Path)
    parser.add_argument("--model-sha256")
    parser.add_argument("--model-notices", type=Path)
    main(parser.parse_args())

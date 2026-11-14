"""Run the iOS Companion against a native Core with a synthetic masked camera."""

from __future__ import annotations

import argparse
import base64
import hashlib
import ipaddress
import json
import socket
import sqlite3
import subprocess
import time
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import UUID

from replay_core import run_core, until


def port(address, kind=socket.SOCK_STREAM):
    with socket.socket(type=kind) as listener:
        listener.bind((address, 0))
        return listener.getsockname()[1]


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(args):
    address = args.address
    with TemporaryDirectory(prefix="stillwatch-ios-") as directory:
        root = Path(directory)
        sources = root / "sources"
        sources.mkdir()
        data = root / "data"
        subprocess.run([
            str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "testsrc2=size=320x240:rate=12:duration=120", "-an", "-threads", "1",
            "-c:v", "libx264", "-preset", "ultrafast", str(sources / "fixture.mp4"),
        ], check=True)
        with run_core(
            args.bin / "stillwatch-core", args.bin / "stillwatch-worker",
            data, sources, root / "core.log",
            "--lan-listen", f"{address}:{port(address)}",
            "--mediamtx", str(args.mediamtx), "--mediamtx-sha256", sha256(args.mediamtx),
            "--media-encoder", str(args.ffmpeg), "--media-encoder-sha256", sha256(args.ffmpeg),
            "--media-ice-udp", f"{address}:{port(address, socket.SOCK_DGRAM)}",
        ) as (_, client):
            code, camera = client.call("POST", "/api/v2/cameras", {
                "name": "Masked camera", "enabled": True,
                "source": {"type": "replay", "file": "fixture.mp4", "pace": True, "repeat": True},
                "privacy_masks": [{"left": 0, "top": 0, "right": 5000, "bottom": 10000}],
            }, key="ios-video-fixture")
            assert code == 201, (code, camera)
            camera_id = camera["id"]
            until(lambda: client.camera(camera_id)["runtime"]["state"] == "replaying", 20)
            with closing(sqlite3.connect(data / "stillwatch.sqlite3", timeout=3)) as db, db:
                now = time.time_ns() // 1_000_000
                db.execute(
                    "INSERT INTO health_events(camera_id, code, opened_at, ended_at) VALUES (?, ?, ?, ?)",
                    (UUID(camera_id).bytes, "camera_frozen", now - 60_000, now),
                )
            offer = subprocess.check_output([
                str(args.bin / "stillwatch-cli"), "pair", "--data-dir", str(data),
                "--local", client.address.removeprefix("http://"), "--role", "viewer",
                "--camera", camera_id, "--scope", "streams-read", "--json",
            ])
            settings = root / "test.json"
            settings.write_text(json.dumps({"STILLWATCH_QA_OFFER": base64.b64encode(offer).decode("ascii")}))
            result = subprocess.run([
                "flutter", "test", "--no-pub", "-d", args.device,
                f"--dart-define-from-file={settings}", "integration_test/core_test.dart",
            ], cwd=args.app, timeout=420)
            code, devices = client.call("GET", "/api/v2/devices")
            assert code == 200
            print(json.dumps({"enrolled_devices": len(devices), "transport_fixture": "native_core"}), flush=True)
            result.check_returncode()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("bin", "ffmpeg", "mediamtx", "app"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--address", type=ipaddress.IPv4Address, required=True)
    parser.add_argument("--device", required=True)
    args = parser.parse_args()
    if not args.address.is_private or args.address.is_unspecified or args.address.is_multicast:
        parser.error("The fixture needs a private IPv4 address")
    args.address = str(args.address)
    run(args)

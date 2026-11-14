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
from uuid import UUID, uuid4

from replay_core import run_core, until


def port(address, kind=socket.SOCK_STREAM):
    with socket.socket(type=kind) as listener:
        listener.bind((address, 0))
        return listener.getsockname()[1]


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def prepare_alert(database, camera, incident):
    now = time.time_ns() // 1_000_000
    state = {
        "id": str(incident), "scope": {"camera_id": camera, "epoch": 1, "scene_revision": 1},
        "category": "fire", "revision": 1, "detection_reason": "iOS alert qualification",
        "first_source_us": 0, "last_source_us": 1_000_000, "live": True,
        "created_at": now, "updated_at": now, "assessment": "immediate_visible_concern",
        "attention": "urgent", "attention_cycle": 1, "acknowledgement": None,
        "resolution": None, "evidence": None, "evidence_state": "unavailable",
        "assessed_evidence_version": None,
    }
    with closing(sqlite3.connect(database, timeout=3)) as db, db:
        db.execute(
            "INSERT INTO incidents(id, camera_id, revision, created_at, updated_at, resolved, attention,"
            " state, creation_hash) VALUES (?, ?, 1, ?, ?, 0, 2, ?, ?)",
            (incident.bytes, UUID(camera).bytes, now, now, json.dumps(state), bytes(32)),
        )
        db.execute(
            "INSERT INTO notification_outbox(incident_id, attention_cycle, attention, state,"
            " created_at, expires_at, next_attempt_at) VALUES (?, 1, 2, 'queued', ?, ?, ?)",
            (incident.bytes, now, now + 120_000, now),
        )


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
            # A separate fixture isolates transport from the model's alert policy.
            code, alert_camera = client.call("POST", "/api/v2/cameras", {
                "name": "Alert fixture", "enabled": False,
                "source": {"type": "replay", "file": "fixture.mp4", "pace": True},
            }, key="ios-alert-fixture")
            assert code == 201, (code, alert_camera)
            incident = uuid4()
            offer = subprocess.check_output([
                str(args.bin / "stillwatch-cli"), "pair", "--data-dir", str(data),
                "--local", client.address.removeprefix("http://"), "--role", "viewer",
                "--camera", camera_id, "--camera", alert_camera["id"], "--scope", "streams-read", "--json",
            ])
            settings = root / "test.json"
            settings.write_text(json.dumps({
                "STILLWATCH_QA_OFFER": base64.b64encode(offer).decode("ascii"),
                "STILLWATCH_QA_ALERT_INCIDENT": str(incident),
            }))
            with subprocess.Popen([
                "flutter", "test", "--no-pub", "-d", args.device,
                f"--dart-define-from-file={settings}", "integration_test/core_test.dart",
            ], cwd=args.app) as test:
                try:
                    def paired():
                        if test.poll() is not None:
                            raise subprocess.CalledProcessError(test.returncode, test.args)
                        return client.call("GET", "/api/v2/devices")[1]

                    until(paired, 360)
                    prepare_alert(data / "stillwatch.sqlite3", alert_camera["id"], incident)
                    if test.wait(timeout=180) != 0:
                        raise subprocess.CalledProcessError(test.returncode, test.args)
                finally:
                    if test.poll() is None:
                        test.kill()
            code, devices = client.call("GET", "/api/v2/devices")
            assert code == 200
            device, = devices
            code, record = client.call("GET", f"/api/v2/incidents/{incident}")
            assert code == 200 and record["acknowledgement"] is None
            with closing(sqlite3.connect(data / "stillwatch.sqlite3", timeout=3)) as db:
                receipt, = db.execute(
                    "SELECT r.device_id FROM notification_receipts r JOIN notification_outbox n"
                    " ON r.alert_id = n.id WHERE n.incident_id = ?",
                    (incident.bytes,),
                ).fetchall()
            assert receipt[0] == UUID(device["id"]).bytes
            print(json.dumps({"enrolled_devices": len(devices), "transport_fixture": "native_core",
                              "ios_system_notification": True, "receipt_without_ack": True}), flush=True)


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

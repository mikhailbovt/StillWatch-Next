"""Cut over a schema-1 snapshot and read its masked history from native Core."""

import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
import time
from uuid import uuid4

from replay_core import run_core


def run(args):
    with TemporaryDirectory(prefix="stillwatch-migration-") as temporary:
        root = Path(temporary)
        source = root / "legacy"
        source.mkdir()
        old_id, overview_id, packet_id = (uuid4() for _ in range(3))
        camera = uuid4()
        jpeg = source / "incidents" / old_id.hex / packet_id.hex / "0.jpg"
        jpeg.parent.mkdir(parents=True)
        subprocess.run([
            str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "color=black:size=128x96", "-threads", "1", "-frames:v", "1", str(jpeg),
        ], check=True)
        expected_jpeg = jpeg.read_bytes()
        database = source / "stillwatch.sqlite3"
        now = time.time()
        with closing(sqlite3.connect(database)) as conn, conn:
            conn.executescript("""
                PRAGMA user_version = 1;
                CREATE TABLE incidents (id TEXT PRIMARY KEY, updated REAL, body TEXT);
                CREATE TABLE packets (id TEXT PRIMARY KEY, incident_id TEXT, created REAL, metadata TEXT, files TEXT);
                CREATE TABLE assessments (id INTEGER PRIMARY KEY, incident_id TEXT, created REAL, packet TEXT, result TEXT);
                CREATE TABLE feedback (id INTEGER PRIMARY KEY, incident_id TEXT, created REAL, operator TEXT, action TEXT);
                CREATE TABLE calls (id INTEGER PRIMARY KEY, provider TEXT, started REAL, reserved INTEGER);
            """)
            for ident, category, lifecycle in [(old_id, "medical", "ACKNOWLEDGED"), (overview_id, "overview", "CLOSED")]:
                body = {
                    "incident_id": ident.hex, "camera_id": camera.hex, "category": category,
                    "reason": "fixture", "first_ts": 2, "last_ts": 4.5, "epoch": 8,
                    "created_at": now - 100, "updated_at": now - 50,
                    "lifecycle": lifecycle, "analysis_status": "URGENT",
                }
                conn.execute("INSERT INTO incidents VALUES (?, ?, ?)", (ident.hex, now - 50, json.dumps(body)))
            packet = {
                "packet_id": packet_id.hex, "incident_id": old_id.hex, "version": 2,
                "images": [{"capture_ts": 3, "kind": "overview"}, {"capture_ts": 4, "kind": "crop"}],
            }
            conn.execute("INSERT INTO packets VALUES (?, ?, ?, ?, ?)", (
                packet_id.hex, old_id.hex, now - 60, json.dumps(packet),
                json.dumps([jpeg.relative_to(source / "incidents").as_posix()]),
            ))
            conn.execute("INSERT INTO assessments VALUES (1, ?, ?, ?, ?)", (
                old_id.hex, now - 59, json.dumps(packet),
                json.dumps({"provider": "openai", "medical_status": "URGENT"}),
            ))
            conn.execute("INSERT INTO feedback VALUES (1, ?, ?, 'legacy-operator', 'confirmed')", (
                old_id.hex, now - 55,
            ))
        original_hash = hashlib.sha256(database.read_bytes()).hexdigest()
        config = root / "migration-config.json"
        config.write_text(json.dumps({
            "camera": {
                "name": "Former camera", "enabled": True,
                "source": {"type": "replay", "file": "replacement.mp4", "pace": True},
                "privacy_masks": [{"left": 0, "top": 0, "right": 150, "bottom": 1000}],
            },
        }), encoding="utf-8")
        target = root / "data"
        command = [
            str(args.cli), "migrate", "--legacy-data", str(source),
            "--destination", str(target), "--config", str(config),
        ]
        dry = json.loads(subprocess.check_output([*command, "--dry-run"], text=True))
        assert not target.exists()
        actual = json.loads(subprocess.check_output(command, text=True))
        assert dry["snapshot_sha256"] == actual["snapshot_sha256"]
        assert hashlib.sha256(database.read_bytes()).hexdigest() == original_hash
        assert hashlib.sha256((target / database.name).read_bytes()).hexdigest() == actual["destination_database_sha256"]
        assert actual["historical_images_listed"] == 2 and actual["historical_images_available"] == 1
        assert actual["assessments_archived"] == actual["feedback_archived"] == 1
        assert actual["camera_id"] == str(camera) and not actual["camera_enabled"] and actual["privacy_masks_need_review"]
        with closing(sqlite3.connect(target / database.name)) as conn:
            assert conn.execute("SELECT legacy_provider FROM legacy_assessments").fetchone() == ("openai",)
            assert conn.execute("SELECT count(*) FROM notification_outbox").fetchone() == (0,)
            assert not conn.execute("PRAGMA foreign_key_check").fetchall()
        sources = root / "replays"
        sources.mkdir()
        with run_core(args.core, args.worker, target, sources, root / "core.log") as (_, client):
            assert not client.camera(str(camera))["config"]["enabled"]
            status, item = client.call("GET", f"/api/v2/incidents/{old_id}")
            assert status == 200 and item["assessment"] == "unknown" and item["needs_reassessment"]
            assert item["acknowledgement"] and item["lifecycle"] == "open" and not item["live"]
            assert item["evidence_state"] == "partial"
            path = f"/api/v2/incidents/{old_id}/evidence/{packet_id}"
            status, packet = client.call("GET", path)
            assert status == 200 and [i["availability"] for i in packet["images"]] == ["available", "unavailable"]
            assert client.call("GET", f"{path}/0", binary=True) == (200, expected_jpeg)
            status, archived = client.call("GET", f"/api/v2/incidents/{overview_id}")
            assert status == 200 and archived["category"] == "overview" and archived["lifecycle"] == "resolved"
            status, feed = client.call("GET", "/api/v2/alerts")
            assert status == 200 and feed["alerts"] == [] and feed.get("next") is None
        print(json.dumps({"read_only_source": True, "dry_run": True, "historic_evidence": True,
                          "preserved_incident_ids": True, "no_historic_alerts": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "ffmpeg"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    run(parser.parse_args())

"""Exercise metadata quota and WAL backpressure through the native Core API."""

from __future__ import annotations

import argparse
from contextlib import closing, contextmanager
import json
from pathlib import Path
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
import time

from replay_core import run_core, until
from sdk_core import open_incident


QUOTA = 4 * 1024 * 1024
WAL_THRESHOLD = 16 * 1024 * 1024


def camera(client, key):
    return client.call(
        "POST", "/api/v2/cameras",
        {"name": key, "enabled": False, "source": {"type": "replay", "file": "unused.mp4", "pace": True}},
        key=key,
        timeout=22,  # Match the camera setup deadline used by the SDKs.
    )


def readiness(client, code, state):
    actual, body = client.call("GET", "/health/ready")
    return actual == code and body["state"] == state


def active_codes(client):
    code, body = client.call("GET", "/api/v2/health-events?active_only=true")
    assert code == 200, body
    return {event["code"] for event in body["events"]}


def metadata_quota(args, root, inputs):
    data = root / "quota-data"
    db_path = data / "stillwatch.sqlite3"
    with run_core(args.core, args.worker, data, inputs, root / "quota-core.log") as (_, client):
        assert readiness(client, 200, "ready")
        code, saved = camera(client, "retain-camera")
        assert code == 201, saved
        open_incident(db_path, saved["id"], 1)
        saved_incident, = client.call("GET", "/api/v2/incidents")[1]["incidents"]
        with closing(sqlite3.connect(db_path)) as setup:
            setup.execute("CREATE TABLE quota_fixture(payload BLOB)")
            setup.execute("INSERT INTO quota_fixture VALUES (zeroblob(?))", (QUOTA - QUOTA // 8,))
            setup.commit()
    before = db_path.stat().st_size
    assert 0 < before < QUOTA
    with run_core(args.core, args.worker, data, inputs, root / "quota-fill.log",
                  "--database-quota-bytes", str(QUOTA)) as (_, client):
        # A fixture places the database near its limit. Core itself attempts every write
        # after launch, so the test exercises the writer's own max_page_count.
        for index in range(400):
            code, result = camera(client, f"quota-camera-{index}")
            if code == 507:
                assert result["code"] == "storage_unavailable", result
                break
            assert code == 201, result
        else:
            raise AssertionError("Core did not reach its metadata page limit")
        assert readiness(client, 503, "degraded")
        assert client.camera(saved["id"])["id"] == saved["id"]
        assert client.call("GET", f"/api/v2/incidents/{saved_incident['id']}") == (200, saved_incident)
    assert db_path.stat().st_size <= QUOTA

    settings = root / "core-settings.json"
    settings.write_text(json.dumps({"database_quota_bytes": QUOTA}), encoding="utf-8")
    with run_core(args.core, args.worker, data, inputs, root / "quota-resume.log",
                  "--settings-file", str(settings)) as (_, client):
        until(lambda: "database_budget_low" in active_codes(client))
        assert readiness(client, 503, "degraded")
        if args.cli is not None:
            output = subprocess.check_output(
                [str(args.cli), "doctor", "--data-dir", str(data),
                 "--local", client.address.removeprefix("http://")],
                text=True, timeout=15,
            )
            report = json.loads(output)
            assert report["readiness"]["state"] == "degraded"
            assert "database_budget_low" in report["active_host_health"]
            assert client.token not in output and str(root) not in output
        with closing(sqlite3.connect(db_path)) as setup:
            setup.execute("DROP TABLE quota_fixture")
            setup.commit()
        until(lambda: readiness(client, 200, "ready"))
        assert camera(client, f"quota-camera-{index}")[0] == 201
    with run_core(args.core, args.worker, data, inputs, root / "quota-finished.log",
                  "--settings-file", str(settings)) as (_, client):
        until(lambda: "database_budget_low" not in active_codes(client))
        assert client.camera(saved["id"])["id"] == saved["id"]
    with closing(sqlite3.connect(db_path)) as setup:
        assert setup.execute("PRAGMA quick_check").fetchone() == ("ok",)
    return index


@contextmanager
def pinned_wal(database):
    with closing(sqlite3.connect(database)) as writer:
        writer.execute("CREATE TABLE wal_fixture(payload TEXT)")
        writer.execute("INSERT INTO wal_fixture VALUES('initial')")
        writer.commit()
        assert writer.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0
        with closing(sqlite3.connect(database)) as reader:
            reader.execute("BEGIN")
            assert reader.execute("SELECT payload FROM wal_fixture").fetchone() == ("initial",)
            writer.execute("UPDATE wal_fixture SET payload = hex(zeroblob(?))", (WAL_THRESHOLD // 2,))
            writer.commit()
            wal_path = Path(f"{database}-wal")
            assert wal_path.stat().st_size >= WAL_THRESHOLD
            yield wal_path
            reader.commit()


def held_wal(args, root, inputs):
    data = root / "wal-data"
    db_path = data / "stillwatch.sqlite3"
    wal_path = Path(f"{db_path}-wal")
    with run_core(args.core, args.worker, data, inputs, root / "wal-core.log") as (_, client):
        code, saved = camera(client, "read-during-wal-pressure")
        assert code == 201, saved
        with pinned_wal(db_path):
            code, saved_after_threshold = camera(client, "committed-before-checkpoint")
            assert code == 201, saved_after_threshold
            assert readiness(client, 503, "degraded")
            assert client.call("GET", "/health/live")[0] == 200
            assert client.camera(saved["id"])["id"] == saved["id"]
            held_bytes = wal_path.stat().st_size
            code, failed = camera(client, "resume-after-wal")
            assert code == 507 and failed["code"] == "storage_unavailable", failed
            assert wal_path.stat().st_size == held_bytes
        until(lambda: readiness(client, 200, "ready"))
        code, resumed = camera(client, "resume-after-wal")
        assert code == 201, resumed
        assert wal_path.stat().st_size < WAL_THRESHOLD
        assert client.camera(saved_after_threshold["id"])["id"] == saved_after_threshold["id"]
    return held_bytes


def external_writer_lock(args, root, inputs):
    data = root / "busy-data"
    database = data / "stillwatch.sqlite3"
    with run_core(args.core, args.worker, data, inputs, root / "busy-core.log") as (_, client):
        code, camera_before = camera(client, "keep-during-foreign-writer")
        assert code == 201
        with closing(sqlite3.connect(database)) as foreign:
            foreign.execute("BEGIN IMMEDIATE")
            began = time.monotonic()
            code, rejected = camera(client, "after-release")
            duration = (time.monotonic() - began) * 1_000
            assert code == 503 and rejected["code"] == "storage_unavailable", rejected
            assert client.call("GET", "/health/live")[0] == 200
            assert client.camera(camera_before["id"])["id"] == camera_before["id"]
            assert foreign.execute("SELECT count(*) FROM cameras").fetchone() == (1,)
            foreign.rollback()
        recovered = time.monotonic()
        code, created = camera(client, "after-release")
        recovery = (time.monotonic() - recovered) * 1_000
        assert code == 201 and camera(client, "after-release") == (201, created)
        with closing(sqlite3.connect(database)) as db:
            assert db.execute("PRAGMA integrity_check").fetchone() == ("ok",)
            assert db.execute("SELECT count(*) FROM cameras").fetchone() == (2,)
    return {"sqlite_busy_response": 503, "busy_duration_ms": duration, "recovery_ms": recovery,
            "same_operation_committed_once_after_unlock": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=name != "cli")
    options = parser.parse_args()
    with TemporaryDirectory(prefix="stillwatch-storage-limit-") as temporary:
        root = Path(temporary)
        inputs = root / "inputs"
        inputs.mkdir()
        print(json.dumps({
            "writes_before_metadata_limit": metadata_quota(options, root, inputs),
            "wal_bytes_with_reader": held_wal(options, root, inputs),
            "incidents_readable_in_storage_pressure": True,
            "idempotent_retry_after_wal_recovery": True,
            "redacted_doctor_during_pressure": options.cli is not None,
            **external_writer_lock(options, root, inputs),
        }))

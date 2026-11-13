"""Verify the native desktop agent's per-device receipt, restart and revocation."""

import argparse
from contextlib import ExitStack, closing
import json
from pathlib import Path
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
import time
from uuid import UUID, uuid4

from replay_core import run_core, until
from rtsp_core import free_port, launch
from sdk_core import create_camera, issue


def insert_urgent(database, camera, incident):
    when = time.time_ns() // 1_000_000
    body = {
        "id": str(incident), "scope": {"camera_id": camera, "epoch": 1, "scene_revision": 1},
        "revision": 1, "category": "fire", "detection_reason": "Alert transport qualification",
        "first_source_us": 0, "last_source_us": 1000000, "live": True,
        "created_at": when, "updated_at": when, "assessment": "immediate_visible_concern",
        "attention": "urgent", "attention_cycle": 1, "acknowledgement": None,
        "resolution": None, "evidence": None, "evidence_state": "unavailable",
        "assessed_evidence_version": None,
    }
    with closing(sqlite3.connect(database, timeout=3)) as conn, conn:
        conn.execute(
            "INSERT INTO incidents(id,camera_id,revision,created_at,updated_at,resolved,attention,state,creation_hash)"
            " VALUES (?,?,1,?,?,0,2,?,?)",
            (incident.bytes, UUID(camera).bytes, when, when, json.dumps(body), bytes(32)),
        )
        conn.execute(
            "INSERT INTO notification_outbox(incident_id,attention_cycle,attention,state,created_at,expires_at,next_attempt_at)"
            " VALUES (?,1,2,'queued',?,?,?)",
            (incident.bytes, when, when + 120000, when),
        )


def run(args):
    with TemporaryDirectory(prefix="stillwatch-alarm-") as temporary:
        root = Path(temporary)
        data = root / "data"
        sources = root / "sources"
        sources.mkdir()
        state = root / "alarm-state"
        database = data / "stillwatch.sqlite3"
        address = f"127.0.0.1:{free_port()}"
        with run_core(
            args.core, args.worker, data, sources, root / "core.log",
            "--lan-listen", address,
        ) as (_, client):
            visible = create_camera(client, "Alarm camera")
            offer = issue(args.cli, client, data, visible)
            subprocess.run([
                str(args.agent), "enroll", "--state", str(state), "--label", "Alarm qualification",
            ], input=json.dumps(offer), text=True, check=True)
            status, devices = client.call("GET", "/api/v2/devices")
            assert status == 200
            device, = devices
            incidents = [uuid4(), uuid4()]
            for incident in incidents:
                insert_urgent(database, visible, incident)

            def receipts():
                with closing(sqlite3.connect(database, timeout=3)) as conn:
                    return conn.execute(
                        "SELECT n.incident_id, r.device_id, r.client_seen_at FROM notification_receipts r"
                        " JOIN notification_outbox n ON n.id = r.alert_id ORDER BY r.alert_id"
                    ).fetchall()

            command = [str(args.agent), "run", "--state", str(state)]
        with launch(command, root / "first-agent.log") as first_agent:
            time.sleep(3)
            assert first_agent.poll() is None, "The enrolled agent exited during an offline startup"
            assert receipts() == []
            with run_core(
                args.core, args.worker, data, sources, root / "delivery-core.log",
                "--lan-listen", address,
            ) as (_, client):
                until(lambda: len(receipts()) == len(incidents), 15)
                first = receipts()
                assert [row[:2] for row in first] == [(incident.bytes, UUID(device["id"]).bytes) for incident in incidents]
                for incident in incidents:
                    code, record = client.call("GET", f"/api/v2/incidents/{incident}")
                    assert code == 200 and record["acknowledgement"] is None

        with run_core(
            args.core, args.worker, data, sources, root / "restart-core.log",
            "--lan-listen", address,
        ) as (_, client):
            with launch(command, root / "second-agent.log") as restarted:
                until(lambda: "alarm agent connected" in (root / "second-agent.log").read_text(errors="replace"))
                assert receipts() == first
                assert client.call("DELETE", f"/api/v2/devices/{device['id']}")[0] == 204
                assert restarted.wait(timeout=15) != 0
                assert receipts() == first
            print(json.dumps({"native_agent": True, "per_device_receipt": True, "restart_dedup": True,
                              "offline_startup": True, "simultaneous_urgent": len(incidents),
                              "operator_ack_independent": True, "device_revocation": True}))

        monitor_state = root / "monitor-state"
        monitor_log = root / "monitor.log"
        with ExitStack() as monitor:
            with run_core(
                args.core, args.worker, data, sources, root / "heartbeat-core.log",
                "--lan-listen", address,
            ) as (_, client):
                offer = issue(args.cli, client, data, visible)
                subprocess.run([
                    str(args.agent), "enroll", "--state", str(monitor_state), "--label", "Heartbeat qualification",
                ], input=json.dumps(offer), text=True, check=True)
                monitor.enter_context(launch([
                    str(args.agent), "monitor", "--state", str(monitor_state),
                    "--interval-seconds", "2", "--consecutive-failures", "2",
                ], monitor_log))
                until(lambda: '"state":"ready"' in monitor_log.read_text(errors="replace"))
            until(lambda: '"state":"unreachable"' in monitor_log.read_text(errors="replace"), 15)
            with run_core(
                args.core, args.worker, data, sources, root / "heartbeat-restart.log",
                "--lan-listen", address,
            ):
                until(lambda: monitor_log.read_text(errors="replace").count('"state":"ready"') == 2, 15)
        print(json.dumps({"independent_process": True, "offline_debounce": True, "restart_recovery": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "agent"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    run(parser.parse_args())

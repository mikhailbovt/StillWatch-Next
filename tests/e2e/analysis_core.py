"""Exercise the real Rust ONNX inference route on two credited demo clips."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import subprocess
import sqlite3
import uuid
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import capture_children, run_core, until, workers_alive
from rtsp_core import check_port, free_port, launch


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(args):
    baseline = json.loads((Path(__file__).resolve().parents[2] / "baseline.json").read_text(encoding="utf-8"))
    assert digest(args.demo) == baseline["demonstration"]["sha256"]
    manifest = digest(args.models / "models.lock.json")
    assert manifest == args.manifest_sha256
    assert digest(args.runtime) == args.runtime_sha256
    with TemporaryDirectory(prefix="stillwatch-analysis-") as temporary:
        root = Path(temporary)
        for name, start in (("pose", 4), ("fire", 40)):
            subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-ss", str(start),
                            "-i", str(args.demo), "-t", "8", "-vf", "scale=640:360", "-an",
                            "-c:v", "libx264", "-threads", "1", str(root / f"{name}.mp4")], check=True)
        model_options = (
            "--model-pack", str(args.models), "--model-manifest-sha256", args.manifest_sha256,
            "--ort-library", str(args.runtime), "--ort-library-sha256", args.runtime_sha256,
        )
        with run_core(args.core, args.worker, root / "data", root, root / "core.log", *model_options) as (_, client):
            ids = {}
            for name in ("pose", "fire"):
                status, camera = client.call("POST", "/api/v2/cameras", {
                    "name": name, "enabled": True, "source": {"type": "replay", "file": f"{name}.mp4", "pace": True}
                }, key=name)
                assert status == 201 and not camera["live"], (status, camera)
                ids[name] = camera["id"]
            observed = {"pose": False, "fire": False}

            def detected():
                for name, camera in ids.items():
                    status, analysis = client.call("GET", f"/api/v2/cameras/{camera}/analysis-status")
                    assert status == 200, (status, analysis)
                    if analysis["state"] != "ready" or not analysis["source_epoch"]:
                        continue
                    if name == "pose" and (analysis["people"] or 0) > 0:
                        observed[name] = True
                    if name == "fire" and (analysis["fire_regions"] or 0) + (analysis["smoke_regions"] or 0) > 0:
                        observed[name] = True
                return all(observed.values())

            until(detected, 12)

            def committed_evidence():
                status, page = client.call("GET", "/api/v2/incidents")
                assert status == 200, (status, page)
                return next((
                    item for item in page["incidents"]
                    if item["category"] == "fire" and item["camera_id"] == ids["fire"]
                    and item["evidence_packet_id"] and item["evidence_state"] == "persisted"
                ), None)

            incident = until(committed_evidence, 12)
            assert incident["attention"] == "review" and not incident["live"]
            origin = incident["analysis_origin"]
            assert origin["model_manifest_sha256"] == manifest
            path = f"/api/v2/incidents/{incident['id']}/evidence/{incident['evidence_packet_id']}"
            status, packet = client.call("GET", path)
            assert status == 200, (status, packet)
            assert packet["camera_id"] == incident["camera_id"]
            assert packet["source_epoch"] == incident["source_epoch"]
            assert packet["context"]["local_facts"]["type"] == "fire"
            assert packet["context"]["policy_version"] == origin["policy_version"]
            assert "non_live" in packet["context"]["limitations"]
            assert any(image["kind"] == "overview" and image["availability"] == "available" for image in packet["images"])
            for image in packet["images"]:
                status, body = client.call("GET", f"{path}/{image['ordinal']}", binary=True)
                assert status == 200 and hashlib.sha256(body).hexdigest() == image["sha256"]
            with closing(sqlite3.connect(root / "data" / "stillwatch.sqlite3")) as db:
                assert db.execute("select count(*) from notification_outbox").fetchone() == (0,)
                row = db.execute("select state from incidents where id = ?", (uuid.UUID(incident["id"]).bytes,)).fetchone()
                assert json.loads(row[0])["analysis_origin"] == origin
            print(json.dumps({"native_ort": True, "supervised_worker": True, "sources": 2,
                              "people_observed": True, "fire_or_smoke_observed": True,
                              "persisted_evidence": True, "analysis_origin_persisted": True,
                              "replay_did_not_enqueue_alert": True}))
        if args.mediamtx:
            verify_live_outbox(args, root, model_options)
        if args.worker_faults:
            verify_inference_recovery(args, root, model_options)


def inference_worker(parent):
    workers = []
    for pid in capture_children(parent):
        try:
            command = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        except FileNotFoundError:
            continue
        if command[1:] == [b"infer", b""]:
            workers.append(pid)
    return workers[0] if len(workers) == 1 else None


def verify_inference_recovery(args, root, model_options):
    with run_core(args.core, args.worker, root / "faults", root, root / "fault-core.log",
                  *model_options, "--inference-workers", "1") as (process, client):
        status, camera = client.call("POST", "/api/v2/cameras", {
            "name": "Inference recovery", "enabled": True,
            "source": {"type": "replay", "file": "pose.mp4", "pace": True, "repeat": True},
        }, key="inference-recovery")
        assert status == 201
        camera_id = camera["id"]
        path = f"/api/v2/cameras/{camera_id}/analysis-status"

        def current():
            status, value = client.call("GET", path)
            assert status == 200
            return value

        def ready():
            value = current()
            return value if value["state"] == "ready" and int(value["fire_frames"]) > 0 else None

        initial = until(ready, 20)
        for fault, failure in ((signal.SIGSTOP, "worker_timeout"), (signal.SIGKILL, "worker_failed")):
            pid = until(lambda: inference_worker(process.pid))
            os.kill(pid, fault)

            def degraded():
                _, value = client.call("GET", "/api/v2/status")
                return value["inference"]["failure"] == failure

            until(degraded)
            state = current()
            assert state["state"] == "degraded"
            assert all(state[name] is None for name in ("people", "fire_regions", "smoke_regions"))
            assert client.call("GET", "/health/ready")[0] == 200

            def recovered():
                value = ready()
                if value is None or workers_alive({pid}):
                    return None
                if value["source_epoch"] == initial["source_epoch"]:
                    return value if int(value["fire_frames"]) > int(initial["fire_frames"]) else None
                return value if int(value["source_epoch"]) > int(initial["source_epoch"]) else None

            initial = until(recovered, 30)
            assert inference_worker(process.pid) != pid
        print(json.dumps({"native_inference_hang_reaped": True, "native_inference_crash_restarted": True,
                          "unavailable_observations_hidden": True, "core_stayed_live": True}))


def verify_live_outbox(args, root, model_options):
    port = free_port()
    peer = f"127.0.0.1:{port}"
    config = root / "live.yml"
    config.write_text(f"""logLevel: warn
rtsp: true
rtspTransports: [tcp]
rtspAddress: {peer}
rtmp: false
hls: false
webrtc: false
srt: false
moq: false
paths:
  fixture:
""")
    publish = [str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1",
               "-i", str(root / "fire.mp4"), "-an", "-c:v", "copy",
               "-f", "rtsp", "-rtsp_transport", "tcp", f"rtsp://{peer}/fixture"]
    data = root / "live"
    with launch([str(args.mediamtx), str(config)], root / "live-media.log"):
        until(lambda: check_port(port))
        with launch(publish, root / "live-publisher.log"):
            with run_core(args.core, args.worker, data, root, root / "live-core.log",
                          "--rtsp-peer", peer, *model_options) as (_, client):
                source = {"type": "rtsp", "peer": peer, "stream_path": "/fixture",
                          "secure": False, "transport": "tcp", "credential_ref": None}
                body = {"name": "Live baseline fixture", "source": source, "enabled": True}
                key = str(uuid.uuid4())

                def create():
                    status, camera = client.call("POST", "/api/v2/cameras", body, key=key)
                    return camera if status == 201 else None

                camera = until(create, 20)

                def alert():
                    status, page = client.call("GET", "/api/v2/incidents")
                    assert status == 200, (status, page)
                    return next((item for item in page["incidents"] if item["category"] == "fire"
                                 and item["camera_id"] == camera["id"] and item["evidence_state"] == "persisted"), None)

                incident = until(alert, 25)
                assert incident["live"] and incident["attention"] == "review", incident
                with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
                    intents = db.execute("select attention, attention_cycle, state from notification_outbox "
                                         "where incident_id = ?", (uuid.UUID(incident["id"]).bytes,)).fetchall()
                    assert intents == [(1, 1, "queued")], intents
                print(json.dumps({"live_rtsp": True, "persisted_evidence": True,
                                  "incident_and_outbox": True, "outbox_intents_in_cycle": len(intents)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "ffmpeg", "demo", "models", "runtime"):
        parser.add_argument(f"--{name}", type=lambda value: Path(value).resolve(strict=True), required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--runtime-sha256", required=True)
    parser.add_argument("--mediamtx", type=lambda value: Path(value).resolve(strict=True))
    parser.add_argument("--worker-faults", action="store_true", help="Suspend and kill the actual Linux ONNX worker")
    options = parser.parse_args()
    if options.worker_faults and not Path("/proc/self/task").is_dir():
        parser.error("--worker-faults requires Linux procfs")
    run(options)

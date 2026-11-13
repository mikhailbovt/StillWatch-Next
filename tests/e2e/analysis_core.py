"""Exercise the real Rust ONNX inference route on two credited demo clips."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sqlite3
import uuid
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until
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
            path = f"/api/v2/incidents/{incident['id']}/evidence/{incident['evidence_packet_id']}"
            status, packet = client.call("GET", path)
            assert status == 200, (status, packet)
            assert packet["camera_id"] == incident["camera_id"]
            assert packet["source_epoch"] == incident["source_epoch"]
            assert packet["context"]["local_facts"]["type"] == "fire"
            assert "non_live" in packet["context"]["limitations"]
            assert any(image["kind"] == "overview" and image["availability"] == "available" for image in packet["images"])
            for image in packet["images"]:
                status, body = client.call("GET", f"{path}/{image['ordinal']}", binary=True)
                assert status == 200 and hashlib.sha256(body).hexdigest() == image["sha256"]
            with closing(sqlite3.connect(root / "data" / "stillwatch.sqlite3")) as db:
                assert db.execute("select count(*) from notification_outbox").fetchone() == (0,)
            print(json.dumps({"native_ort": True, "supervised_worker": True, "sources": 2,
                              "people_observed": True, "fire_or_smoke_observed": True,
                              "persisted_evidence": True, "replay_did_not_enqueue_alert": True}))
        if args.mediamtx:
            verify_live_outbox(args, root, model_options)


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
    run(parser.parse_args())

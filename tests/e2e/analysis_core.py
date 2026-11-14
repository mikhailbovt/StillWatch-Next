"""Exercise the real Rust ONNX inference route on two credited demo clips."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import shutil
import socket
import subprocess
import sqlite3
import time
import urllib.error
import urllib.request
import uuid
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import capture_children, run_core, until, workers_alive
from rtsp_core import check_port, free_port, launch


def health_events(client, code, camera_id=None, *, active_only=False):
    status, page = client.call("GET", f"/api/v2/health-events?active_only={str(active_only).lower()}")
    assert status == 200, (status, page)
    return [
        event for event in page["events"]
        if event["code"] == code and event["camera_id"] == camera_id
    ]


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
            assert packet["observed_host_utc"] <= packet["persisted_at"] < packet["expires_at"]
            assert packet["fresh_when_persisted"]
            assert incident["opening_observed_host_utc"] <= incident["created_at"]
            assert packet["context"]["local_facts"]["type"] == "fire"
            assert packet["context"]["policy_version"] == origin["policy_version"]
            assert "non_live" in packet["context"]["limitations"]
            assert any(image["kind"] == "overview" and image["availability"] == "available" for image in packet["images"])
            for image in packet["images"]:
                status, body = client.call("GET", f"{path}/{image['ordinal']}", binary=True)
                assert status == 200 and hashlib.sha256(body).hexdigest() == image["sha256"]
            with closing(sqlite3.connect(root / "data" / "stillwatch.sqlite3")) as db:
                assert db.execute("select count(*) from notification_outbox").fetchone() == (0,)
                identifier = uuid.UUID(incident["id"]).bytes
                row = db.execute("select state from incidents where id = ?", (identifier,)).fetchone()
                assert json.loads(row[0])["analysis_origin"] == origin
                facts, = db.execute("select facts from hazard_observations where incident_id = ? "
                                    "order by source_us limit 1", (identifier,)).fetchone()
                assert json.loads(facts)[0]["type"] == "fire"
            print(json.dumps({"native_ort": True, "supervised_worker": True, "sources": 2,
                              "people_observed": True, "fire_or_smoke_observed": True,
                              "persisted_evidence": True, "analysis_origin_persisted": True,
                              "replay_did_not_enqueue_alert": True}))
        if args.mediamtx:
            verify_live_outbox(args, root, model_options)
        if args.source_freeze:
            verify_repeated_frames(args, root, model_options)
        if args.worker_faults:
            verify_inference_recovery(args, root, model_options)
        if args.model_fault:
            verify_corrupt_model(args, root, model_options)
        if args.cadence:
            verify_cadence(args, root, model_options)
        if args.camera_sampling:
            verify_camera_sampling(args, root, model_options)
        if args.timing:
            verify_timing(args, root, model_options)
        if args.slow_source:
            verify_slow_source(args, root, model_options)
        if args.capture_recovery:
            verify_capture_recovery(args, root, model_options)
        if args.retention:
            verify_preaged_retention(args, root, model_options)


def verify_timing(args, root, model_options):
    with run_core(args.core, args.worker, root / "timing-data", root, root / "timing.log",
                  *model_options) as (_, client):
        status, camera = client.call("POST", "/api/v2/cameras", {
            "name": "Timing", "enabled": True,
            "source": {"type": "replay", "file": "pose.mp4", "pace": True, "repeat": True},
        }, key="timing")
        assert status == 201
        camera_id = camera["id"]
        path = f"/api/v2/cameras/{camera_id}/analysis-metrics"
        assert client.call("GET", path, authorized=False)[0] == 401

        def metrics(minimum):
            status, report = client.call("GET", path)
            assert status == 200, (status, report)
            return report if report and int(report["counts"]["pose"]["accepted"]) >= minimum else None

        first = until(lambda: metrics(6), 15)
        worker = first["worker"]

        def total(entries):
            return sum(map(int, entries))

        assert 500_000 in first["latency_bounds_us"] and 1_000_000 in first["latency_bounds_us"]
        assert all(len(buckets) == len(first["latency_bounds_us"]) + 1 for buckets in worker.values())
        replies = total(worker["round_trip_us"])
        assert replies >= 6, first
        for field in ("input_read_us", "preprocess_us", "serialize_us"):
            assert total(worker[field]) == (0 if args.legacy_worker else replies), first
        if not args.legacy_worker:
            assert total(worker["pose_run_us"]) == total(worker["pose_decode_us"]) >= 6
            assert total(worker["fire_run_us"]) == total(worker["fire_decode_us"]) > 0
        for task in ("pose", "fire"):
            counts = first["counts"][task]
            assert total(first[task]["receive_to_dispatch_us"]) == int(counts["dispatched"])
            assert total(first[task]["receive_to_accept_us"]) == int(counts["accepted"])
            assert first[task]["maximum_receive_to_accept_us"] <= 3_000_000

        def lag_report():
            lag = client.camera(camera_id)["runtime"].get("relative_source_lag_ms")
            return (lag,) if lag is not None else None

        lag, = until(lag_report, 5)
        assert isinstance(lag, int) and abs(lag) < 2000, lag

        def rotated():
            current = metrics(int(first["counts"]["pose"]["accepted"]) + 6)
            return current if current and int(current["source_epoch"]) > int(first["source_epoch"]) else None

        second = until(rotated, 18)
        for task in ("pose", "fire"):
            assert all(int(v) >= int(first["counts"][task][k]) for k, v in second["counts"][task].items())
        code, core_status = client.call("GET", "/api/v2/status")
        assert code == 200
        assert int(core_status["inference"]["totals"]["pose"]["accepted"]) >= int(second["counts"]["pose"]["accepted"])
        print(json.dumps({"timed_ipc": not args.legacy_worker, "reply_histogram_count": replies,
                          "relative_source_lag_ms": lag, "counters_cross_epoch": True}))


def verify_cadence(args, root, model_options):
    for fps in (2, 15):
        name = f"cadence-{fps}.mp4"
        subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-i", str(args.demo),
                        "-vf", f"scale=640:360,fps={fps}", "-t", "45", "-an", "-c:v", "libx264",
                        "-threads", "1", str(root / name)], check=True)
    with run_core(args.core, args.worker, root / "cadence-data", root, root / "cadence.log",
                  *model_options, "--inference-workers", "2", "--inference-threads", "3") as (_, client):
        cameras = {}
        for fps in (2, 15):
            status, camera = client.call("POST", "/api/v2/cameras", {
                "name": f"Source at {fps} fps", "enabled": True,
                "source": {"type": "replay", "file": f"cadence-{fps}.mp4", "pace": True},
            }, key=f"cadence-{fps}")
            assert status == 201 and "cadence" not in camera["runtime"]["analysis"]
            cameras[fps] = camera["id"]

        def available():
            status, page = client.call("GET", "/api/v2/cameras")
            assert status == 200
            rows = {camera["id"]: camera["runtime"].get("analysis") for camera in page["cameras"]}
            return rows if all(rows.get(camera, {}).get("cadence") for camera in cameras.values()) else None

        rows = until(available, 25)
        for fps, camera in cameras.items():
            analysis = rows[camera]
            assert analysis["state"] == "ready", analysis
            cadence = analysis["cadence"]
            assert cadence["below_target"] == (fps == 2), (fps, cadence)
            assert cadence["pose_target_per_minute"] == 240 and cadence["fire_target_per_minute"] == 90
            status, details = client.call("GET", f"/api/v2/cameras/{camera}/analysis-status")
            assert status == 200 and details["source_epoch"] == analysis["source_epoch"]
            assert details["cadence"]["below_target"] == cadence["below_target"]
        print(json.dumps({"analysis_cadence": {fps: rows[camera]["cadence"] for fps, camera in cameras.items()}}))

        issue = "inference_below_target"
        slow = cameras[2]
        started, = until(lambda: health_events(client, issue, slow, active_only=True), 12)
        assert not health_events(client, issue, cameras[15], active_only=True)
        status, _ = client.call("PATCH", f"/api/v2/cameras/{slow}", {
            "source": {"type": "replay", "file": "cadence-15.mp4", "pace": True},
        }, key="cadence-recovered", revision=client.camera(slow)["revision"])
        assert status == 200

        def recovered():
            event, = health_events(client, issue, slow)
            return event if event["ended_at"] else None

        ended = until(recovered, 28)
        assert ended["sequence"] == started["sequence"]
        assert ended["ended_at"] is not None
        active = client.camera(slow)["runtime"]["analysis"]
        assert active["state"] == "ready" and not active["cadence"]["below_target"]
        print(json.dumps({"sustained_cadence_issue_recorded_and_ended": True}))


def verify_camera_sampling(args, root, model_options):
    # Delay the fire scene to give the evidence ring several seconds to warm up.
    subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-ss", "40",
                    "-i", str(args.demo), "-t", "25", "-an", "-c:v", "libx264", "-threads", "1",
                    "-vf", "scale=640:360,fps=15,tpad=start_duration=5:start_mode=add",
                    str(root / "sampling.mp4")], check=True)
    data = root / "sampling-data"
    config = {
        "name": "Configured sampling", "enabled": True,
        "source": {"type": "replay", "file": "sampling.mp4", "pace": True},
        "sampling": {"pose_fps": 3, "fire_fps": 1, "evidence_fps": 2},
    }
    with run_core(args.core, args.worker, data, root, root / "sampling.log", *model_options) as (_, client):
        assert client.call("GET", "/bootstrap/info", authorized=False)[1]["camera_sampling"] is True
        invalid = config | {"sampling": config["sampling"] | {"fire_fps": 0}}
        assert client.call("POST", "/api/v2/cameras", invalid, key="sampling-invalid")[0] == 400
        status, camera = client.call("POST", "/api/v2/cameras", config, key="sampling-create")
        assert status == 201 and camera["config"]["sampling"] == config["sampling"], camera
        path = f"/api/v2/cameras/{camera['id']}"

        def cadence():
            current = client.camera(camera["id"])
            return current["runtime"]["analysis"].get("cadence")

        reduced = until(cadence, 25)
        before_priority = client.camera(camera["id"])
        assert "priority" not in before_priority["config"]
        assert client.call("GET", "/bootstrap/info", authorized=False)[1]["camera_priority"]
        assert before_priority["runtime"]["analysis"]["extra_fire_frames"] == "0"
        assert 0 <= before_priority["runtime"]["analysis"]["last_queue_age_ms"] <= 3_000
        status, _ = client.call("PATCH", path, {"priority": "highest"},
                                key="invalid-priority", revision=before_priority["revision"])
        assert status == 400
        assert 144 <= reduced["pose_frames_per_minute"] <= 180, reduced
        assert 45 <= reduced["fire_frames_per_minute"] <= 60, reduced
        assert reduced["pose_target_per_minute"] == 144 and reduced["fire_target_per_minute"] == 45, reduced
        assert not reduced["below_target"], reduced

        def evidence():
            _, page = client.call("GET", "/api/v2/incidents")
            return next((item for item in page["incidents"] if item["camera_id"] == camera["id"]
                         and item["evidence_state"] == "persisted"), None)

        incident = until(evidence, 10)
        status, packet = client.call("GET", f"/api/v2/incidents/{incident['id']}/evidence/{incident['evidence_packet_id']}")
        assert status == 200, packet
        coverage = packet["context"]["coverage"]
        assert 3 <= coverage["received_samples"] <= coverage["expected_samples"] <= 17, coverage
        assert coverage["max_gap_ms"] >= 400, coverage
        status, prioritized = client.call("PATCH", path, {"priority": "high"},
                                          key="camera-priority", revision=before_priority["revision"])
        assert status == 200 and prioritized["config"]["priority"] == "high", prioritized
        assert int(prioritized["source_epoch"]) > int(before_priority["source_epoch"]), prioritized

    with run_core(args.core, args.worker, data, root, root / "sampling-restart.log", *model_options) as (_, client):
        current = client.camera(camera["id"])
        assert current["config"]["sampling"] == config["sampling"]
        assert current["config"]["priority"] == "high"
        status, updated = client.call("PATCH", path, {
            "sampling": {"pose_fps": 5, "fire_fps": 2, "evidence_fps": 5},
            "priority": "normal",
        }, key="sampling-default", revision=current["revision"])
        assert status == 200 and not {"sampling", "priority"} & updated["config"].keys(), updated
        assert int(updated["source_epoch"]) > int(current["source_epoch"]), updated
        restored = until(cadence, 25)
        assert restored["pose_target_per_minute"] == 240 and restored["fire_target_per_minute"] == 90, restored
        assert restored["pose_frames_per_minute"] > reduced["pose_frames_per_minute"], restored
        assert restored["fire_frames_per_minute"] > reduced["fire_frames_per_minute"], restored
    print(json.dumps({"camera_sampling_persisted": True, "camera_priority_persisted": True, "configured_cadence": reduced,
                      "configured_evidence_coverage": coverage, "restored_default_cadence": restored}))


def inference_worker(parent):
    workers = []
    for pid in capture_children(parent):
        try:
            command = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        except FileNotFoundError:
            continue
        if command[1:] in ([b"infer", b""], [b"infer-v2", b""]):
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
        metrics_path = f"/api/v2/cameras/{camera_id}/analysis-metrics"
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
            if fault == signal.SIGSTOP:
                code, report = client.call("GET", metrics_path)
                assert code == 200 and sum(int(row["timeout"]) for row in report["counts"].values()) > 0
            until(lambda: health_events(client, "inference_unavailable", active_only=True))

            def recovered():
                value = ready()
                if value is None or workers_alive({pid}):
                    return None
                if value["source_epoch"] == initial["source_epoch"]:
                    return value if int(value["fire_frames"]) > int(initial["fire_frames"]) else None
                return value if int(value["source_epoch"]) > int(initial["source_epoch"]) else None

            initial = until(recovered, 30)
            assert inference_worker(process.pid) != pid
            until(lambda: not health_events(client, "inference_unavailable", active_only=True), 5)
        assert len(health_events(client, "inference_unavailable")) == 2
        print(json.dumps({"native_inference_hang_reaped": True, "native_inference_crash_restarted": True,
                          "unavailable_observations_hidden": True, "core_stayed_live": True,
                          "inference_failures_recorded_and_ended": True}))


def verify_corrupt_model(args, root, model_options):
    damaged = root / "corrupt-model-pack"
    damaged.mkdir()
    shutil.copyfile(args.models / "models.lock.json", damaged / "models.lock.json")
    manifest = json.loads((damaged / "models.lock.json").read_bytes())
    for name, entry in manifest["models"].items():
        artifact = entry["artifact"]
        if name == "fire":
            (damaged / artifact).write_bytes(b"rejected artifact")
        else:
            shutil.copyfile(args.models / artifact, damaged / artifact)
    assert manifest["models"]["fire"]["artifact"]
    data = root / "corrupt-model-data"
    with run_core(args.core, args.worker, data, root, root / "corrupt-model-core.log",
                  "--model-pack", str(damaged), *model_options[2:]) as (_, client):
        initial = until(lambda: health_events(client, "model_pack_unverified", active_only=True))
        assert len(initial) == 1, initial
        assert health_events(client, "inference_unavailable", active_only=True)
        _, status = client.call("GET", "/api/v2/status")
        assert status["inference"]["failure"] == "model_pack_unavailable", status
        assert client.call("GET", "/health/ready")[0] == 200
    with run_core(args.core, args.worker, data, root, root / "verified-model-core.log",
                  *model_options) as (_, client):
        ended = until(
            lambda: event if (event := health_events(client, "model_pack_unverified")[0])["ended_at"] else None
        )
        assert ended["sequence"] == initial[0]["sequence"]
        assert ended["opened_at"] == initial[0]["opened_at"]
        assert not health_events(client, "inference_unavailable", active_only=True)
        assert client.call("GET", "/api/v2/incidents")[1]["incidents"] == []
    print(json.dumps({"model_hash_mismatch_recorded": True, "same_interval_ended_after_restart": True}))


def rtsp_router(root, name):
    port = free_port()
    peer = f"127.0.0.1:{port}"
    config = root / f"{name}.yml"
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
    return port, peer, config


def verify_slow_source(args, root, model_options):
    port, peer, config = rtsp_router(root, "slow-source")
    fixture = root / "paced-source.mp4"
    subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=15:duration=45",
                    "-c:v", "libx264", "-threads", "1", "-preset", "ultrafast",
                    "-tune", "zerolatency", "-g", "15", str(fixture)], check=True)
    publish = [str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-readrate", "0.5",
               "-i", str(fixture), "-an", "-c:v", "copy", "-f", "rtsp",
               "-rtsp_transport", "tcp", f"rtsp://{peer}/fixture"]
    with launch([str(args.mediamtx), str(config)], root / "slow-source-media.log"):
        until(lambda: check_port(port))
        with launch(publish, root / "slow-source-publisher.log") as publisher:
            with run_core(args.core, args.worker, root / "slow-source-data", root, root / "slow-source.log",
                          "--rtsp-peer", peer, *model_options) as (_, client):
                body = {"name": "Half-rate stream", "enabled": True, "source": {
                    "type": "rtsp", "peer": peer, "stream_path": "/fixture",
                    "secure": False, "transport": "tcp", "credential_ref": None,
                }}

                def add_camera():
                    status, camera = client.call("POST", "/api/v2/cameras", body, key="half-rate")
                    return camera if status == 201 else None

                camera = until(add_camera, 15)
                camera_id = camera["id"]

                def full_window():
                    assert publisher.poll() is None
                    camera = client.camera(camera_id)
                    return camera if camera["runtime"]["analysis"].get("wall_cadence") else None

                video = until(full_window, 77)
                runtime = video["runtime"]
                source = runtime["analysis"]["cadence"]
                wall = runtime["analysis"]["wall_cadence"]
                lag = runtime["relative_source_lag_ms"]
                assert video["source_epoch"] == camera["source_epoch"] or int(video["source_epoch"]) == 1, video
                assert runtime["state"] == "streaming" and not source["below_target"], runtime
                assert wall["window_seconds"] == 60 and 90 <= wall["pose_accepted"] <= 170, wall
                assert 36 <= wall["fire_accepted"] <= 68, wall
                assert 25_000 <= lag <= 36_000, runtime
                print(json.dumps({"half_rate_native_rtsp": True, "source_cadence": source,
                                  "wall_cadence": wall, "relative_source_lag_ms": lag}))


def verify_capture_recovery(args, root, model_options):
    port, peer, config = rtsp_router(root, "capture-retry")
    publish = [str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1",
               "-i", str(root / "fire.mp4"), "-an", "-c:v", "copy", "-f", "rtsp",
               "-rtsp_transport", "tcp", f"rtsp://{peer}/fixture"]
    data = root / "capture-retry-data"
    with launch([str(args.mediamtx), str(config)], root / "capture-retry-media.log"):
        until(lambda: check_port(port))
        with run_core(args.core, args.worker, data, root, root / "capture-retry.log",
                      "--rtsp-peer", peer, "--inference-workers", "2", *model_options) as (process, client):
            source = {"type": "rtsp", "peer": peer, "stream_path": "/fixture", "secure": False,
                      "transport": "tcp", "credential_ref": None}

            def progress(camera_id, epoch_after=None):
                camera = client.camera(camera_id)
                analysis = camera["runtime"]["analysis"]
                epoch = int(camera["source_epoch"])
                if ((epoch_after is None or epoch > epoch_after)
                        and analysis["state"] == "ready"
                        and int(analysis["pose_frames"]) >= 2 and int(analysis["fire_frames"]) >= 2
                        and int(analysis["source_epoch"]) == epoch):
                    return camera
                return None

            with launch(publish, root / "capture-retry-publisher.log"):
                def create():
                    status, camera = client.call("POST", "/api/v2/cameras", {
                        "name": "Primary RTSP", "source": source, "enabled": True,
                    }, key="primary")
                    return camera if status == 201 else None

                primary = until(create, 15)["id"]
                def add_replay(name, repeat):
                    status, camera = client.call("POST", "/api/v2/cameras", {
                        "name": name, "enabled": True, "source": {
                            "type": "replay", "file": "pose.mp4", "pace": True, "repeat": repeat,
                        },
                    }, key="witness-replay" if repeat else "finite-replay")
                    assert status == 201, (status, camera)
                    return camera["id"]

                witness = add_replay("Parallel camera", True)
                final = add_replay("Finite replay", False)
                initial = until(lambda: progress(primary), 15)
                until(lambda: progress(witness), 12)
                initial_epoch = int(initial["source_epoch"])
                with closing(sqlite3.connect(data / "stillwatch.sqlite3", timeout=5)) as db, db:
                    # Reject only epoch updates for the disposable RTSP camera.
                    # The literal is a UUID returned by the test's own Core.
                    target = uuid.UUID(primary).hex
                    db.execute(f"""CREATE TRIGGER fail_epoch BEFORE UPDATE OF source_epoch ON cameras
                        WHEN old.id = X'{target}' BEGIN SELECT RAISE(ABORT, 'injected epoch write'); END""")
                baseline = int(client.camera(witness)["runtime"]["analysis"]["pose_frames"])
            until(lambda: health_events(client, "camera_offline", primary, active_only=True), 17)
            lost_at = time.monotonic()
            terminal = until(lambda: value if (value := client.camera(final))["runtime"]["state"] == "completed"
                             else None, 15)
            with launch(publish, root / "capture-retry-publisher-recovered.log"):
                time.sleep(2)
                failed = client.camera(primary)
                assert int(failed["source_epoch"]) == initial_epoch, failed
                assert failed["runtime"]["state"] in ("starting", "retrying")
                code, witness_metrics = client.call("GET", f"/api/v2/cameras/{witness}/analysis-metrics")
                assert code == 200 and int(witness_metrics["counts"]["pose"]["accepted"]) > baseline
                with closing(sqlite3.connect(data / "stillwatch.sqlite3", timeout=5)) as db, db:
                    db.execute("DROP TRIGGER fail_epoch")
                restored = until(lambda: progress(primary, initial_epoch), 35)
                assert restored["revision"] == initial["revision"]
                assert process.poll() is None
                def recovered_evidence():
                    status, page = client.call("GET", "/api/v2/incidents")
                    assert status == 200
                    return next((event for event in page["incidents"] if event["camera_id"] == primary
                                 and event["source_epoch"] == restored["source_epoch"]
                                 and event["category"] == "fire" and event["evidence_state"] == "persisted"), None)

                event = until(recovered_evidence, 20)
                with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
                    assert db.execute("SELECT count(*) FROM notification_outbox WHERE incident_id = ?",
                                      (uuid.UUID(event["id"]).bytes,)).fetchone() == (1,)
                until(lambda: not health_events(client, "camera_offline", primary, active_only=True), 8)
                offline, = health_events(client, "camera_offline", primary)
                assert offline["ended_at"] is not None
                completed = client.camera(final)
                assert completed["source_epoch"] == terminal["source_epoch"]
                assert completed["runtime"]["state"] == "completed"
                print(json.dumps({"capture_epoch_write_recovered": True, "parallel_camera_progressed": True,
                                  "finite_replay_kept_completed": True, "outage_after_health_event_seconds":
                                  round(time.monotonic() - lost_at, 3)}))


def verify_preaged_retention(args, root, model_options):
    from sdk_core import open_incident

    data = root / "retention-data"
    database = data / "stillwatch.sqlite3"
    quota = 5 * 1024 * 1024
    now = time.time_ns() // 1_000_000
    old = now - 31 * 86_400_000
    count = 4097
    with run_core(args.core, args.worker, data, root, root / "retention-seed.log") as (_, client):
        code, camera = client.call("POST", "/api/v2/cameras", {
            "name": "Retention canary", "source": {
                "type": "replay", "file": "pose.mp4", "pace": True, "repeat": True,
            }, "enabled": True,
        }, key="retention-camera")
        assert code == 201, camera
        camera_id = camera["id"]
        open_incident(database, camera_id, 1)
        open_incident(database, camera_id, 2)
        opened = client.call("GET", "/api/v2/snapshot")[1]
        live, expired = sorted(opened["incidents"], key=lambda item: item["detection_reason"])
        origin = bytes.fromhex(expired["id"].replace("-", ""))
        code, result = client.call("POST", f"/api/v2/incidents/{expired['id']}/resolution",
                                   {"reason": "Reviewed"}, revision=expired["revision"], key="retention-resolution")
        assert code == 200, result
        code, acknowledged = client.call("POST", f"/api/v2/incidents/{live['id']}/acknowledgements",
                                         {}, revision=live["revision"], key="retention-acknowledge")
        assert code == 200, acknowledged
    # Pre-age resolved metadata and administrative rows. Hold the fixture's
    # SQLite transaction separate from serving writes and the fault run.
    with closing(sqlite3.connect(database)) as db, db:
        db.execute("UPDATE incidents SET updated_at = ? WHERE id = ?", (old, origin))
        db.execute("UPDATE change_events SET occurred_at = ? WHERE incident_id = ?", (old, origin))
        db.execute("""WITH RECURSIVE rows(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM rows WHERE i < ?)
            INSERT INTO idempotency_records
            SELECT zeroblob(16), 'retention-test', CAST(i AS TEXT), zeroblob(32),
                   json_object('sample',hex(zeroblob(224))), ? FROM rows""", (count, old))
        db.execute("""WITH RECURSIVE rows(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM rows WHERE i < ?)
            INSERT INTO trusted_devices
            SELECT CAST(printf('%016d',i) AS BLOB), zeroblob(32), 'expired', '{}', ?, ? FROM rows""",
                   (count, old, old))
        db.execute("""INSERT INTO device_sessions
            SELECT id, CAST(id||zeroblob(16) AS BLOB), ?, CAST(zeroblob(16)||id AS BLOB), ?
            FROM trusted_devices""", (old, old))
        db.execute("""INSERT INTO notification_outbox(incident_id, attention_cycle, attention, state,
                      created_at, expires_at, next_attempt_at)
                      VALUES (?,1,1,'expired',?,?,?)""", (origin, old, old, old))
        outbox_id = db.execute("SELECT last_insert_rowid()").fetchone()[0]
        db.execute("""INSERT INTO notification_receipts
            SELECT ?, id, ? FROM trusted_devices""", (outbox_id, old))
    before = database.stat().st_size
    assert quota * 0.90 <= before < quota, before
    with run_core(args.core, args.worker, data, root, root / "retention-core.log",
                  "--database-quota-bytes", str(quota), *model_options) as (_, client):
        def analysis():
            status, payload = client.call("GET", f"/api/v2/cameras/{camera_id}/analysis-metrics")
            if status == 200 and payload is not None:
                return tuple(int(payload["counts"][name]["accepted"]) for name in ("pose", "fire"))
            return 0, 0

        started = time.monotonic()
        before_analysis = until(lambda: rates if min(rates := analysis()) > 0 else None, 10)

        def retained():
            with closing(sqlite3.connect(database)) as db:
                debt = tuple(db.execute(f"SELECT count(*) FROM {table}").fetchone()[0] for table in (
                    "notification_receipts", "device_sessions", "trusted_devices",
                ))
            return not any(debt)

        until(retained, 85)
        after_analysis = analysis()
        assert all(b > a for a, b in zip(before_analysis, after_analysis)), (before_analysis, after_analysis)
        assert client.call("GET", f"/api/v2/incidents/{expired['id']}")[0] == 404
        assert client.call("GET", f"/api/v2/incidents/{live['id']}")[0] == 200
        code, problem = client.call("GET", f"/api/v2/changes?cursor={opened['cursor']}")
        assert code == 409 and problem["code"] == "cursor_expired", (code, problem)
        assert client.call("POST", f"/api/v2/incidents/{live['id']}/acknowledgements",
                           {}, revision=live["revision"], key="retention-acknowledge") == (200, acknowledged)
        with closing(sqlite3.connect(database)) as db:
            assert db.execute("PRAGMA foreign_key_check").fetchall() == []
            assert db.execute("PRAGMA integrity_check").fetchone() == ("ok",)
            assert db.execute("SELECT count(*) FROM idempotency_records WHERE operation = 'retention-test'").fetchone() == (0,)
        status, control = client.call("GET", "/api/v2/status")
        assert status == 200
        print(json.dumps({
            "preaged_rows_per_queue": count, "database_bytes": before, "quota_bytes": quota,
            "retention_drain_seconds": round(time.monotonic() - started, 3),
            "canary_accepted_pose_fire": (before_analysis, after_analysis),
            "writer_maximum_operation_ms": control["writer"]["maximum_operation_ms"],
            "open_incident_and_command_replay_preserved": True, "foreign_key_check": "ok",
        }))


def verify_live_outbox(args, root, model_options):
    port, peer, config = rtsp_router(root, "live")
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


def verify_repeated_frames(args, root, model_options):
    port, peer, config = rtsp_router(root, "repeated")
    # One uninterrupted sender advances PTS through 6 s of movement, 11 s of
    # repeated decoded pixels, and a second moving segment.
    fixture = root / "repeat-pixels.mp4"
    subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi",
                    "-i", "testsrc2=size=320x240:rate=12:duration=30",
                    "-vf", "select=not(between(t\\,6\\,17)),fps=12",
                    "-c:v", "libx264", "-qp", "0", "-threads", "1", "-preset", "ultrafast",
                    "-tune", "zerolatency", "-g", "12", str(fixture)], check=True)
    publish = [str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-i", str(fixture),
               "-c:v", "copy", "-f", "rtsp", "-rtsp_transport", "tcp",
               f"rtsp://{peer}/fixture"]
    # Keyframes are losslessly repeated so decoder noise cannot imitate movement.
    with launch([str(args.mediamtx), str(config)], root / "repeated-media.log"):
        until(lambda: check_port(port))
        data = root / "repeated-data"
        with run_core(args.core, args.worker, data, root, root / "repeated-core.log",
                      *model_options, "--rtsp-peer", peer, *snapshot_flags(args)) as (_, client):
            with launch(publish, root / "repeated-publisher.log") as publisher:
                source = {"type": "rtsp", "peer": peer, "stream_path": "/fixture",
                          "secure": False, "transport": "tcp", "credential_ref": None}
                body = {"name": "Repeated RTSP frames", "source": source, "enabled": True}

                def start_camera():
                    code, created = client.call("POST", "/api/v2/cameras", body, key="repeated-source")
                    return created if code == 201 else None

                camera = until(start_camera, 12)
                cam_id = camera["id"]
                path = f"/api/v2/cameras/{cam_id}/analysis-status"

                def normal():
                    item = client.camera(cam_id)
                    code, analysis = client.call("GET", path)
                    assert code == 200
                    if item["runtime"]["state"] == "streaming" and analysis["state"] == "ready":
                        assert not item["runtime"].get("repeated_frame", False)
                        return item
                    return None

                normal_camera = until(normal, 8)
                expect_snapshot(args, client, cam_id, 200)
                frozen = until(lambda: value if (value := client.camera(cam_id))["runtime"].get("repeated_frame") else None, 22)
                assert frozen["runtime"]["state"] == "unavailable"
                assert frozen["source_epoch"] == normal_camera["source_epoch"]
                status, analysis = client.call("GET", path)
                assert status == 200 and analysis["state"] == "degraded", analysis
                assert analysis["people"] is analysis["smoke_regions"] is analysis["fire_regions"] is None
                assert client.call("GET", "/health/ready")[0] == 200
                expect_snapshot(args, client, cam_id, 503)
                freeze_events = until(lambda: health_events(client, "camera_frozen", cam_id), 4)
                assert len(freeze_events) == 1 and freeze_events[0]["ended_at"] is None, freeze_events
                assert not health_events(client, "camera_offline", cam_id, active_only=True)

                def recovered():
                    item = normal()
                    return item if item and int(item["source_epoch"]) > int(frozen["source_epoch"]) else None

                try:
                    after = until(recovered, 12)
                except AssertionError as error:
                    raise AssertionError({
                        "frozen_epoch": frozen["source_epoch"],
                        "final_camera": client.camera(cam_id)["runtime"],
                        "final_analysis": client.call("GET", path)[1],
                        "publisher_exit": publisher.poll(),
                    }) from error
                assert int(after["runtime"]["frame"]) > int(frozen["runtime"]["frame"])
                expect_snapshot(args, client, cam_id, 200)
                until(lambda: not health_events(client, "camera_frozen", cam_id, active_only=True), 4)
                ended, = health_events(client, "camera_frozen", cam_id)
                assert ended["sequence"] == freeze_events[0]["sequence"]
                assert publisher.poll() is None, "The RTSP publisher should still be connected"
            until(lambda: health_events(client, "camera_offline", cam_id, active_only=True), 12)
            status, _ = client.call("PATCH", f"/api/v2/cameras/{cam_id}", {"enabled": False},
                                    key="disable-after-disconnect", revision=client.camera(cam_id)["revision"])
            assert status == 200
            assert not health_events(client, "camera_offline", cam_id, active_only=True)
        with run_core(args.core, args.worker, data, root, root / "restarted-core.log",
                      *model_options, "--rtsp-peer", peer) as (_, client):
            assert health_events(client, "camera_frozen", cam_id) == [ended]
            assert health_events(client, "camera_offline", cam_id)[0]["ended_at"] is not None
    print(json.dumps({"repeated_pixels_detected_with_native_pts": True, "observations_withheld": True,
                      "moving_pixels_rotated_source_epoch": True, "rtsp_publisher_unchanged": True,
                      "freeze_and_disconnect_history_survived_restart": True,
                      "snapshot_unavailable_during_hold": True if args.media_encoder else None}))


def snapshot_flags(args):
    if args.media_encoder is None:
        return []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    return ["--mediamtx", str(args.mediamtx), "--mediamtx-sha256", digest(args.mediamtx),
            "--media-encoder", str(args.media_encoder), "--media-encoder-sha256", digest(args.media_encoder),
            "--media-ice-udp", f"127.0.0.1:{port}"]


def expect_snapshot(args, client, camera_id, expected):
    if args.media_encoder is None:
        return
    request = urllib.request.Request(f"{client.address}/api/v2/cameras/{camera_id}/snapshot.jpg",
                                     headers={"Authorization": f"Bearer {client.token}"})
    try:
        response = urllib.request.urlopen(request, timeout=3)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        assert response.status == expected, (response.status, expected)
        if expected == 200:
            assert response.headers.get("Cache-Control") == "no-store"
            assert 0 <= int(response.headers["X-Frame-Age-Ms"]) <= 3000
            assert response.read(3) == b"\xff\xd8\xff"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "ffmpeg", "demo", "models", "runtime"):
        parser.add_argument(f"--{name}", type=lambda value: Path(value).resolve(strict=True), required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--runtime-sha256", required=True)
    parser.add_argument("--mediamtx", type=lambda value: Path(value).resolve(strict=True))
    parser.add_argument("--media-encoder", type=lambda value: Path(value).resolve(strict=True),
                        help="Also verify JPEG availability during the source-freeze test")
    parser.add_argument("--worker-faults", action="store_true", help="Suspend and kill the actual Linux ONNX worker")
    parser.add_argument("--model-fault", action="store_true", help="Reject a corrupted, disposable copy of the model pack")
    parser.add_argument("--source-freeze", action="store_true", help="Verify repeated RTSP pixels with advancing PTS")
    parser.add_argument("--cadence", action="store_true", help="Verify measured rates for 2 fps and 15 fps replay cameras")
    parser.add_argument("--camera-sampling", action="store_true", help="Verify camera-specific sampling, evidence and restart")
    parser.add_argument("--timing", action="store_true", help="Verify histograms, task counts and lag across replay epochs")
    parser.add_argument("--legacy-worker", action="store_true", help="Expect v1 worker timing compatibility")
    parser.add_argument("--slow-source", action="store_true", help="Measure a 15 fps RTSP fixture delivered at half speed")
    parser.add_argument("--capture-recovery", action="store_true", help="Exercise a targeted failure of SQLite epoch writes")
    parser.add_argument("--retention", action="store_true", help="Drain pre-aged history near the SQLite quota during inference")
    options = parser.parse_args()
    if options.worker_faults and not Path("/proc/self/task").is_dir():
        parser.error("--worker-faults requires Linux procfs")
    if options.source_freeze and not options.mediamtx:
        parser.error("--source-freeze requires --mediamtx")
    if (options.slow_source or options.capture_recovery) and not options.mediamtx:
        parser.error("--slow-source and --capture-recovery require --mediamtx")
    if options.media_encoder and not options.source_freeze:
        parser.error("--media-encoder requires --source-freeze")
    run(options)

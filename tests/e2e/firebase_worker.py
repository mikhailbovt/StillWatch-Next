"""Exercise the locked Firebase worker transport without sending data to Firebase."""

from __future__ import annotations

import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
import secrets
import sqlite3
import struct
import subprocess
import time
import urllib.error
import urllib.request
import uuid
from contextlib import closing
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until
from rtsp_core import check_port, free_port, launch

ROOT = Path(__file__).resolve().parents[2]


def request(origin, route, token=None, body=None, *, host=None, foreign_origin=None, binary=False,
            timeout=5, request_id=None, include_headers=False):
    headers = {}
    if token is not None:
        headers["Authorization"] = "Bearer " + token
    if host is not None:
        headers["Host"] = host
    if foreign_origin is not None:
        headers["Origin"] = foreign_origin
    if request_id is not None:
        headers["X-StillWatch-Request-Id" if route == "/evaluate" else "X-StillWatch-Request"] = request_id
    if body is not None:
        headers["Content-Type"] = "application/octet-stream" if binary else "application/json"
    req = urllib.request.Request(origin + route, data=body, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read(512 * 1024)
        return (response.status, body, response.headers) if include_headers else (response.status, body)


def private(path, value):
    path.write_text(value, encoding="ascii")
    if os.name != "nt":
        path.chmod(0o600)


def fire_packet(route="cloud_only", timeout_ms=1000):
    jpeg = (ROOT / "tests/fixtures/evidence/black.jpg").read_bytes()
    metadata = {
        "schema_version": "1.0",
        "packet_id": str(uuid.uuid4()),
        "incident_id": str(uuid.uuid4()),
        "camera_id": str(uuid.uuid4()),
        "source_epoch": "1",
        "scene_revision": "1",
        "evidence_version": "1",
        "category": "fire",
        "context": {"local_facts": {"type": "fire"}, "prompt_version": "fire-evidence-1"},
        "attachments": [{"id": "f0", "sha256": hashlib.sha256(jpeg).hexdigest(),
                         "kind": "overview", "source_s": 1.0, "bytes": len(jpeg)}],
        "route": route,
        "timeout_ms": timeout_ms,
    }
    encoded = json.dumps(metadata).encode()
    return struct.pack("<I", len(encoded)) + encoded + jpeg


def verify_waiting_browser(origin, core_credential, browser_credential):
    packet = fire_packet("local_only", 3000)
    ready = b'{"state":"ready"}'
    assert request(origin, "/evaluate", core_credential, packet,
                   binary=True, request_id="invalid")[0] == 400

    def healthy():
        code, body = request(origin, "/health", core_credential)
        return code == 200 and json.loads(body)["local_state"] == "ready"

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        browser = pool.submit(request, origin, "/device/poll", browser_credential, ready, include_headers=True)
        until(healthy, 2)
        assert not browser.done()
        assert request(origin, "/device/poll", browser_credential, ready)[0] == 409
        request_id = str(uuid.uuid4())
        evaluation = pool.submit(request, origin, "/evaluate", core_credential, packet,
                                 binary=True, request_id=request_id, include_headers=True)
        status, body, headers = browser.result(timeout=3)
        assert status == 200 and body == packet
        assert headers["X-StillWatch-Request"] == request_id
        assert request(origin, "/device/attestation", browser_credential, b"{}", request_id=request_id)[0] == 409
        assert request(origin, "/device/result", browser_credential, b"{}", request_id=str(uuid.uuid4()))[0] == 409
        failed = b'{"error":"inference_unavailable"}'
        assert request(origin, "/device/result", browser_credential, failed, request_id=request_id)[0] == 204
        code, _, headers = evaluation.result(timeout=3)
        assert code == 503 and headers["X-StillWatch-Request-Id"] == request_id
        assert request(origin, "/device/result", browser_credential, failed, request_id=request_id)[0] == 409
    assert request(origin, "/device/poll", browser_credential, ready, timeout=7)[0] == 204

    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        evaluation = pool.submit(request, origin, "/evaluate", core_credential,
                                 fire_packet("local_only", 600), binary=True)
        status, _, headers = request(origin, "/device/poll", browser_credential, ready, include_headers=True)
        assert status == 200
        assert evaluation.result(timeout=3)[0] == 504
        assert request(origin, "/device/result", browser_credential, failed,
                       request_id=headers["X-StillWatch-Request"])[0] == 409


def run(args):
    node = args.node.resolve(strict=True)
    entry = ROOT / "packages/firebase-worker/src/server.js"
    with TemporaryDirectory(prefix="stillwatch-firebase-") as folder:
        root = Path(folder)
        config = root / "config.json"
        core_credential = secrets.token_hex(32)
        browser_credential = secrets.token_hex(32)
        core_token = root / "core-token"
        browser_token = root / "browser-token"
        app_check = root / "app-check"
        private(core_token, core_credential)
        private(browser_token, browser_credential)
        private(app_check, "no-issued-token-in-offline-fixture")
        fake_public_config = {
            "appCheck": "token_file", "model": "gemini-3.5-flash-lite",
            "firebase": {"apiKey": "offline_fixture", "projectId": "offline-fixture",
                         "appId": "1:000:web:offline-fixture"},
        }
        port = free_port()
        origin = f"http://127.0.0.1:{port}"
        config.write_text(json.dumps(fake_public_config | {"mode": "cloud_node"}), encoding="utf8")
        encoded = lambda value: base64.urlsafe_b64encode(json.dumps(value).encode()).rstrip(b"=").decode()
        valid_claims = {"exp": int(time.time()) + 120, "sub": "1:000:web:offline-fixture",
                        "iss": "https://firebaseappcheck.googleapis.com/000", "aud": ["projects/offline-fixture"]}
        with launch([str(node), str(entry), str(config), str(port), str(core_token), str(app_check)],
                    root / "cloud.log"):
            until(lambda: check_port(port))
            assert request(origin, "/health")[0] == 401
            assert request(origin, "/health", core_credential, host="example.invalid")[0] == 403
            assert request(origin, "/health", core_credential, foreign_origin="https://example.invalid")[0] == 403
            status, body = request(origin, "/health", core_credential)
            assert status == 200 and json.loads(body)["sdk_version"] == "13.3.0"
            assert request(origin, "/evaluate", core_credential, fire_packet(), binary=True)[0] == 502
            # These fixtures only exercise local claim checks. No token is
            # presented to Firebase by the health endpoint.
            for claims, expected in ((valid_claims, "renewal_due"),
                                     (valid_claims | {"exp": int(time.time()) + 3600}, "ready"),
                                     (valid_claims | {"exp": 1}, "unavailable"),
                                     (valid_claims | {"sub": "1:000:web:other"}, "unavailable"),
                                     (valid_claims | {"aud": ["projects/other"]}, "unavailable")):
                private(app_check, f"{encoded({'alg': 'RS256'})}.{encoded(claims)}.fixture")
                status, body = request(origin, "/health", core_credential)
                capabilities = json.loads(body)
                assert status == 200 and capabilities["app_check_state"] == expected
                assert capabilities["cloud_transport"] is (expected != "unavailable")
            if args.core:
                with run_core(args.core, args.worker, root / "core", root, root / "core.log",
                              "--semantic-worker", f"127.0.0.1:{port}",
                              "--semantic-token-file", str(core_token),
                              "--semantic-daily-limit", "1") as (_, client):
                    from analysis_core import health_events

                    def connected():
                        _, state = client.call("GET", "/api/v2/semantic-status")
                        return state if state["state"] == "degraded" and state["sdk_version"] else None

                    state = until(connected, 10)
                    assert state["sdk_version"] == "13.3.0"
                    assert state["last_inference_source"] is None and state["used_today"] == 0
                    assert client.call("GET", "/health/ready")[0] == 200
                    unavailable, = until(lambda: health_events(client, "semantic_unavailable", active_only=True), 5)

                    # Seed one committed usage reservation from this UTC day.
                    # The worker only reports capabilities. No evidence is submitted.
                    from sdk_core import create_camera, open_incident

                    database = root / "core" / "stillwatch.sqlite3"
                    camera = create_camera(client, "Budget reference camera")
                    open_incident(database, camera, 0)
                    with closing(sqlite3.connect(database)) as db, db:
                        incident, = db.execute("SELECT id FROM incidents").fetchone()
                        packet = uuid.uuid4().bytes
                        now = time.time_ns() // 1_000_000
                        db.execute(
                            "INSERT INTO evidence_packets(id, incident_id, version, incident_revision, "
                            "captured_at, expires_at, request_hash) VALUES (?, ?, 1, 1, ?, ?, ?)",
                            (packet, incident, now, now + 60000, bytes(32)),
                        )
                        db.execute(
                            "INSERT INTO semantic_attempts(packet_id, route, incident_id, camera_id, started_at) "
                            "VALUES (?, 'cloud_only', ?, ?, ?)",
                            (packet, incident, uuid.UUID(camera).bytes, now),
                        )
                    until(lambda: health_events(client, "semantic_budget_exhausted", active_only=True), 10)
                    assert len(health_events(client, "semantic_unavailable", active_only=True)) == 1
                    private(app_check, f"{encoded({'alg': 'RS256'})}.{encoded(valid_claims)}.fixture")
                    until(lambda: not health_events(client, "semantic_unavailable", active_only=True), 10)
                    ended, = health_events(client, "semantic_unavailable")
                    assert ended["sequence"] == unavailable["sequence"] and ended["ended_at"]
                    assert len(health_events(client, "semantic_budget_exhausted", active_only=True)) == 1
        port = free_port()
        origin = f"http://127.0.0.1:{port}"
        config.write_text(json.dumps(fake_public_config | {"mode": "hybrid_web"}), encoding="utf8")
        with launch([str(node), str(entry), str(config), str(port), str(core_token), str(app_check),
                     str(browser_token)], root / "hybrid.log"):
            until(lambda: check_port(port))
            assert request(origin, "/")[0] == 200
            assert request(origin, "/hybrid.js")[0] == 200
            assert request(origin, "/device/config", core_credential)[0] == 401
            status, body = request(origin, "/device/config", browser_credential)
            assert status == 200 and set(json.loads(body)) == {"firebase", "model"}
            assert request(origin, "/device/attestation", browser_credential, b"{}")[0] == 409
            assert request(origin, "/evaluate", core_credential, fire_packet(), binary=True)[0] == 503
            verify_waiting_browser(origin, core_credential, browser_credential)
        print(json.dumps({"node_and_hybrid_started": True, "scope_and_origin_enforced": True,
                          "invalid_app_check_fails_closed": True, "core_capability_handshake": bool(args.core),
                          "bounded_browser_long_poll": True, "local_only_denies_attestation": True,
                          "health_route_and_quota_independent": bool(args.core),
                          "firebase_network_inference": False}))


def evaluate_live(origin, token_file, route, expect_source):
    token = token_file.read_text(encoding="ascii").strip()
    payload = fire_packet()
    length = struct.unpack("<I", payload[:4])[0]
    meta = json.loads(payload[4:4 + length])
    meta["timeout_ms"] = 15_000
    meta["route"] = route
    meta["context"]["limitations"] = ["partial_visibility", "non_live"]
    wire = json.dumps(meta).encode("utf-8")
    status, body = request(origin, "/evaluate", token,
                           struct.pack("<I", len(wire)) + wire + payload[4 + length:], binary=True, timeout=20)
    assert status == 200, (status, json.loads(body).get("error"))
    result = json.loads(body)
    sources = {"local_only": {"on_device"}, "cloud_only": {"in_cloud"},
               "prefer_local": {"on_device", "in_cloud"}}
    expected = {expect_source} if expect_source else sources[route]
    assert result["inference_source"] in expected and result["sdk_version"] == "13.3.0", (
        result["inference_source"], result["sdk_version"])
    print(json.dumps({"real_firebase_inference": True, "input": "synthetic_black_jpeg",
                      "model": result["model"], "route": result["inference_source"],
                      "usage_total_tokens": result["usage_total_tokens"],
                      "classification": result["observation"]["classification"]}))


def live(args):
    private_root = args.live_config.resolve(strict=True)
    token_file = private_root / "core-worker"
    if args.running_port:
        evaluate_live(f"http://127.0.0.1:{args.running_port}", token_file, args.route, args.expect_source)
        return
    port = free_port()
    origin = f"http://127.0.0.1:{port}"
    entry = ROOT / "packages/firebase-worker/src/server.js"
    with TemporaryDirectory(prefix="stillwatch-firebase-live-") as folder:
        root = Path(folder)
        with launch([str(args.node), str(entry), str(private_root / "config.json"), str(port),
                     str(token_file), str(private_root / "app-check-debug")], root / "live.log"):
            until(lambda: check_port(port))
            if args.demo:
                core_round_trip(args, root, port, token_file)
            else:
                evaluate_live(origin, token_file, args.route, args.expect_source)


def core_round_trip(args, root, port, token_file):
    baseline = json.loads((ROOT / "baseline.json").read_text())
    with args.demo.open("rb") as demo:
        assert hashlib.file_digest(demo, "sha256").hexdigest() == baseline["demonstration"]["sha256"]
    path = root / "fire.mp4"
    subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-ss", "40", "-i", str(args.demo),
                    "-t", "8", "-vf", "scale=640:360", "-an", "-c:v", "libx264", "-threads", "1", str(path)], check=True)
    data = root / "core"
    with run_core(args.core, args.worker, data, root, root / "core.log",
                  "--model-pack", str(args.models), "--model-manifest-sha256", args.manifest_sha256,
                  "--ort-library", str(args.runtime), "--ort-library-sha256", args.runtime_sha256,
                  "--semantic-worker", f"127.0.0.1:{port}", "--semantic-token-file", str(token_file),
                  "--semantic-include-replay", "--semantic-daily-limit", "2") as (_, client):
        def semantic_ready():
            code, status = client.call("GET", "/api/v2/semantic-status")
            return code == 200 and status["state"] == "available"

        until(semantic_ready, 12)
        cameras = {}
        for consent in (False, True):
            status, camera = client.call("POST", "/api/v2/cameras", {
                "name": f"Firebase QA {consent}", "enabled": True, "cloud_consent": consent,
                "source": {"type": "replay", "file": path.name, "pace": True},
                # The publicly credited demo includes a UI panel above the fire video.
                "privacy_masks": [{"left": 0, "top": 0, "right": 10000, "bottom": 1000}],
            }, key=f"consent-{consent}")
            assert status == 201 and not camera["live"], (status, camera)
            cameras[consent] = camera["id"]
        with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
            db.row_factory = sqlite3.Row

            def result():
                return db.execute("select * from semantic_attempts where outcome='applied' and report is not null").fetchone()

            evaluation = until(result, 25)
            report = json.loads(evaluation["report"])
            assert report["inference_source"] == "in_cloud" and report["sdk_version"] == "13.3.0"
            assert str(uuid.UUID(bytes=evaluation["camera_id"])) == cameras[True]
            denied_id = uuid.UUID(cameras[False]).bytes
            until(lambda: db.execute("select count(*) from incidents where camera_id=?", (denied_id,)).fetchone()[0])
            until(lambda: all(client.camera(camera)["runtime"]["state"] == "completed" for camera in cameras.values()), 12)
            time.sleep(1)
            assert db.execute("select count(*) from semantic_attempts where camera_id=?", (denied_id,)).fetchone()[0] == 0
            assert db.execute("select count(*) from notification_outbox").fetchone()[0] == 0
        code, status = client.call("GET", "/api/v2/semantic-status")
        assert code == 200 and status["last_inference_source"] == "in_cloud"
        print(json.dumps({"core_real_evidence_applied": True, "inference_source": "in_cloud", "sdk": "13.3.0",
                          "camera_without_consent_not_submitted": True, "no_live_alerts": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--core", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--worker", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--live-config", type=Path)
    parser.add_argument("--running-port", type=int)
    parser.add_argument("--route", choices=("local_only", "cloud_only", "prefer_local"), default="cloud_only")
    parser.add_argument("--expect-source", choices=("on_device", "in_cloud"))
    for name in ("demo", "ffmpeg", "models", "runtime"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--manifest-sha256")
    parser.add_argument("--runtime-sha256")
    options = parser.parse_args()
    if bool(options.core) != bool(options.worker):
        parser.error("--core and --worker are required together")
    if options.demo and not all((options.live_config, options.core, options.ffmpeg, options.models, options.runtime,
                                 options.manifest_sha256, options.runtime_sha256)):
        parser.error("--demo requires the live cloud Node configuration and the locked Core/model/runtime options")
    if options.live_config:
        live(options)
    else:
        run(options)

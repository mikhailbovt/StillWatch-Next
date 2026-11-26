"""Negotiate and decode two real WHEP sessions on a masked camera."""

from __future__ import annotations

import argparse
import asyncio
import base64
import contextlib
import hashlib
import io
import ipaddress
import json
import os
import secrets
import signal
import socket
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

import av
from aioice import TransportPolicy
from aiortc import RTCConfiguration, RTCIceServer, RTCPeerConnection, RTCSessionDescription

from lan_core import Client as TlsClient, encode_uuid, refresh_request, sign
from replay_core import capture_children, run_core, until, workers_alive
from rtsp_core import check_port, launch
from sdk_core import issue


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def free_port(address, kind=socket.SOCK_STREAM):
    with socket.socket(type=kind) as sock:
        sock.bind((address, 0))
        return sock.getsockname()[1]


def request(lan, token, method, path, data=None, content_type=None):
    headers = {"Authorization": f"Bearer {token}"}
    if content_type:
        headers["Content-Type"] = content_type
    http = urllib.request.Request(lan.endpoint + path, data, headers, method=method)
    try:
        response = lan.opener.open(http, timeout=12)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, dict(response.headers), response.read(512 * 1024)


async def connect(lan, session, camera, turn_url, turn_certificate, *, mask_right=False,
                  profile="preview", dimensions=(320, 240), offer_status=201, verify_mask=True, auth_lock=None):
    guard = auth_lock if auth_lock is not None else contextlib.nullcontext()

    def issue():
        status, grant = lan.request("POST", "/api/v2/stream-sessions", {"camera_id": camera, "profile": profile},
                                    session["access_token"])
        assert status in (201, 503), (status, grant)
        return grant if status == 201 else None
    async with guard:
        grant = await asyncio.to_thread(until, issue, 8)
    assert grant["redacted"] and grant["codec"] == "h264"
    ice_servers = []
    if turn_url:
        server, = grant["ice_servers"]
        assert server["url"] == turn_url
        ice_servers.append(RTCIceServer(server["url"], server["username"], server["credential"]))
    else:
        assert not grant.get("ice_servers")
    peer = RTCPeerConnection(RTCConfiguration(iceServers=ice_servers))
    receiver = asyncio.Queue()

    @peer.on("track")
    def on_track(track):
        receiver.put_nowait(track)

    transceiver = peer.addTransceiver("video", direction="recvonly")
    # aiortc exposes ICE configuration, but relay-only is on its aioice connection.
    ice = transceiver.receiver.transport.transport.iceGatherer._connection
    if turn_url:
        ice._transport_policy = TransportPolicy.RELAY
    if turn_certificate:
        assert ice.turn_ssl
        ice.turn_ssl = ssl.create_default_context(cafile=str(turn_certificate))
    try:
        await peer.setLocalDescription(await peer.createOffer())
        if turn_url:
            assert {candidate.type for candidate in ice.local_candidates} == {"relay"}
        async with guard:
            status, headers, body = await asyncio.to_thread(
                request, lan, session["access_token"], "POST", grant["whep_path"],
                peer.localDescription.sdp.encode("utf-8"), "application/sdp")
        assert status == offer_status, (status, body.decode(errors="replace"))
        if status != 201:
            assert json.loads(body)["title"] == "Stream capacity reached"
            await peer.close()
            return None
        assert next(value for key, value in headers.items() if key.lower() == "location") == grant["whep_path"]
        assert b"H264" in body
        await peer.setRemoteDescription(RTCSessionDescription(type="answer", sdp=body.decode("utf-8")))
        track = await asyncio.wait_for(receiver.get(), 10)
        try:
            frame = await asyncio.wait_for(track.recv(), 12)
        except TimeoutError as error:
            local_candidates = sum(line.startswith("a=candidate:") for line in peer.localDescription.sdp.splitlines())
            remote_candidates = sum(line.startswith("a=candidate:") for line in body.decode().splitlines())
            raise AssertionError(
                f"No decoded frame (connection={peer.connectionState}, ICE={peer.iceConnectionState}, "
                f"local candidates={local_candidates}, remote candidates={remote_candidates})"
            ) from error
        if turn_url:
            assert ice._nominated[1].local_candidate.type == "relay"
        if dimensions is not None:
            assert (frame.width, frame.height) == dimensions, (frame.width, frame.height)
        if verify_mask:
            rgb = frame.to_ndarray(format="rgb24")
            left, right = rgb[:, :frame.width * 42 // 100], rgb[:, frame.width * 59 // 100:]
            masked, visible = (right, left) if mask_right else (left, right)
            assert masked.max() < 25, "Masked region contains source pixels"
            assert visible.max() > 100, "Unmasked region is empty"
        return peer, track, grant
    except (Exception, asyncio.CancelledError):
        await peer.close()
        with contextlib.suppress(OSError, TimeoutError):
            async with guard:
                await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"])
        raise


async def rotate_during_negotiation(lan, session, offer, key, node, camera, turn_url, turn_certificate):
    # Exercise an actual credential rotation between issuing a grant and exchanging SDP.
    send = lan.request
    old_access = session["access_token"]

    def rotate_after_grant(method, path, body, token):
        status, grant = send(method, path, body, token)
        if status == 201:
            assert path == "/api/v2/stream-sessions"
            proof = refresh_request(node, key, offer, session)
            code, rotated = send("POST", "/api/v2/device-session/refresh", proof)
            assert code == 200 and rotated["access_token"] != old_access
            session.update(rotated)
        return status, grant

    with patch.object(lan, "request", rotate_after_grant):
        peer, _, grant = await connect(lan, session, camera, turn_url, turn_certificate)
    try:
        assert (await asyncio.to_thread(request, lan, old_access, "GET", "/api/v2/status"))[0] == 401
        assert (await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"]))[0] == 204
    finally:
        await peer.close()


async def viewers(lan, session, camera, local, turn_url, turn_certificate):
    status, headers, jpeg = request(lan, session["access_token"], "GET", f"/api/v2/cameras/{camera}/snapshot.jpg")
    assert status == 200 and jpeg[:3] == b"\xff\xd8\xff"
    headers = {key.lower(): value for key, value in headers.items()}
    assert 0 <= int(headers["x-frame-age-ms"]) <= 3000, headers["x-frame-age-ms"]
    assert headers["cache-control"] == "no-store"
    with av.open(io.BytesIO(jpeg), format="mjpeg") as container:
        image = next(container.decode(video=0)).to_ndarray(format="rgb24")
        assert image[:, :135, :].max() < 25 and image[:, 190:, :].max() > 100
    a, b = await asyncio.gather(
        connect(lan, session, camera, turn_url, turn_certificate),
        connect(lan, session, camera, turn_url, turn_certificate),
    )
    try:
        assert a[2]["id"] != b[2]["id"]
        status, _, _ = request(lan, session["access_token"], "DELETE", a[2]["whep_path"])
        assert status == 204
        await asyncio.wait_for(b[1].recv(), 3)
        assert local.call("DELETE", f"/api/v2/devices/{session['device']['id']}")[0] == 204
        deadline = asyncio.get_running_loop().time() + 12
        while asyncio.get_running_loop().time() < deadline:
            try:
                await asyncio.wait_for(b[1].recv(), 2)
            except (TimeoutError, EOFError, av.error.EOFError):
                break
            except Exception as error:
                if type(error).__name__ == "MediaStreamError":
                    break
                raise
        else:
            raise AssertionError("Revoked device still receives frames")
        assert request(lan, session["access_token"], "GET", f"/api/v2/cameras/{camera}/snapshot.jpg")[0] == 401
    finally:
        await a[0].close()
        await b[0].close()


async def mask_change(lan, session, camera, local, turn_url, turn_certificate, **playback):
    previous, track, grant = await connect(lan, session, camera, turn_url, turn_certificate, **playback)
    try:
        old = local.camera(camera)
        status, updated = await asyncio.to_thread(local.call, "PATCH", f"/api/v2/cameras/{camera}", {
            "privacy_masks": [{"left": 5000, "top": 0, "right": 10000, "bottom": 10000}],
        }, key=uuid.uuid4().hex, revision=old["revision"])
        assert status == 200 and int(updated["scene_revision"]) > int(grant["scene_revision"])
        closed, _, _ = await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"])
        assert closed == 404, closed
        deadline = asyncio.get_running_loop().time() + 8
        while asyncio.get_running_loop().time() < deadline:
            try:
                await asyncio.wait_for(track.recv(), 2)
            except (TimeoutError, EOFError, av.error.EOFError):
                break
            except Exception as error:
                if type(error).__name__ == "MediaStreamError":
                    break
                raise
        else:
            raise AssertionError("Previous stream continued after the mask changed")
        replacement, _, current = await connect(lan, session, camera, turn_url, turn_certificate, mask_right=True, **playback)
        try:
            assert current["scene_revision"] == updated["scene_revision"]
            assert current["id"] != grant["id"]
            code, _, _ = await asyncio.to_thread(request, lan, session["access_token"], "DELETE", current["whep_path"])
            assert code == 204
        finally:
            await replacement.close()
    finally:
        await previous.close()


def dual_sources(stack, args, root, sources, encode):
    port = free_port("127.0.0.1")
    peer = f"127.0.0.1:{port}"
    router_file = root / "camera-router.json"
    router_file.write_text(json.dumps({
        "rtspAddress": peer, "rtspTransports": ["tcp"], "logLevel": "error",
        "rtmp": False, "hls": False, "webrtc": False, "srt": False, "moq": False,
        "paths": {"analysis": {}, "detail": {}, "wrong_geometry": {}},
    }), encoding="utf-8")
    stack.enter_context(launch([str(args.mediamtx), str(router_file)], root / "camera-router.log"))
    until(lambda: check_port(port))
    publishers = {}
    for path, size in {"analysis": "320x240", "detail": "640x480", "wrong_geometry": "640x360"}.items():
        clip = sources / f"{path}.mp4"
        subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                        f"testsrc2=size={size}:rate=12", "-t", "8", *encode, "-g", "12", "-bf", "0",
                        str(clip)], check=True)
        publishers[path] = [str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1",
                            "-i", str(clip), "-c:v", "copy", "-f", "rtsp", "-rtsp_transport", "tcp",
                            f"rtsp://{peer}/{path}"]
    for path in ("analysis", "wrong_geometry"):
        stack.enter_context(launch(publishers[path], root / f"{path}.log"))
    return {"type": "rtsp", "peer": peer, "stream_path": "/analysis", "main_stream_path": "/detail",
            "secure": False, "transport": "tcp", "credential_ref": None}, publishers["detail"]


async def distinct_detail(lan, session, camera_id, local, core, turn_url, turn_certificate, publisher, check_inference):
    token = session["access_token"]
    children = await asyncio.to_thread(capture_children, core.pid)
    assert local.camera(camera_id)["dimensions"] == {"width": 320, "height": 240}

    for stop_source in (False, True):
        a = await connect(lan, session, camera_id, turn_url, turn_certificate, profile="detail", dimensions=(640, 480))
        try:
            detail_children = await asyncio.to_thread(capture_children, core.pid)
            assert len(detail_children - children) == (1 if os.name == "nt" else 2), (children, detail_children)
            b = await connect(lan, session, camera_id, turn_url, turn_certificate, profile="detail", dimensions=(640, 480))
            try:
                assert await asyncio.to_thread(capture_children, core.pid) == detail_children
                status, _, _ = await asyncio.to_thread(request, lan, token, "DELETE", a[2]["whep_path"])
                assert status == 204
                await asyncio.wait_for(b[1].recv(), 3)
                analysis = local.camera(camera_id)

                def same_analysis_keeps_capturing():
                    current = local.camera(camera_id)
                    runtime = current["runtime"]
                    if (runtime["state"] != "streaming" or current["source_epoch"] != analysis["source_epoch"]
                            or int(runtime["frame"]) <= int(analysis["runtime"]["frame"])):
                        return False
                    return not check_inference or all(
                        int(runtime["analysis"][key]) > int(analysis["runtime"]["analysis"][key])
                        for key in ("pose_frames", "fire_frames")
                    )

                if stop_source:
                    publisher.terminate()
                    await asyncio.to_thread(publisher.wait, 6)
                else:
                    status, _, _ = await asyncio.to_thread(request, lan, token, "DELETE", b[2]["whep_path"])
                    assert status == 204
                await asyncio.to_thread(until, lambda: capture_children(core.pid) == children, 12)
                await asyncio.to_thread(until, same_analysis_keeps_capturing, 6)
            finally:
                await b[0].close()
        finally:
            await a[0].close()


@contextlib.contextmanager
def turn_server(path, address, root, openssl):
    if path is None:
        yield None, None, []
        return
    ip = ipaddress.IPv4Address(address)
    if not ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_unspecified:
        raise ValueError("The isolated TURN test needs a private interface address")
    port = free_port(address)
    secret = secrets.token_hex(32)
    secret_path = root / "turn-secret"
    secret_path.write_text(secret, encoding="ascii")
    secret_path.chmod(0o600)
    certificate = None
    transport = "no-tls\n"
    scheme = "turn"
    if openssl:
        certificate = root / "turn.pem"
        key = root / "turn.key"
        subprocess.run([str(openssl), "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
                        "-noenc", "-days", "1", "-subj", f"/CN={address}", "-addext", f"subjectAltName=IP:{address}",
                        "-keyout", str(key), "-out", str(certificate)], check=True, capture_output=True)
        transport = f"no-tcp\ntls-listening-port={port}\ncert={certificate}\npkey={key}\n"
        scheme = "turns"
    relay_start = 24000 + secrets.randbelow(4096) * 4
    config = root / "turn.conf"
    config.write_text(f"""listening-ip={address}
relay-ip={address}
listening-port={port}
min-port={relay_start}
max-port={relay_start + 31}
realm=stillwatch-qualification.invalid
pidfile={root / 'turn.pid'}
userdb={root / 'turn.db'}
log-file=stdout
no-stdout-log
no-cli
{transport}no-dtls
no-udp
no-tcp-relay
no-multicast-peers
relay-threads=1
total-quota=12
use-auth-secret
static-auth-secret={secret}
denied-peer-ip=0.0.0.0-255.255.255.255
allowed-peer-ip={address}
""", encoding="ascii")
    config.chmod(0o600)

    def listening():
        try:
            with socket.create_connection((address, port), timeout=0.25):
                return True
        except OSError:
            return False

    with launch([str(path), "-c", str(config)], root / "turn.log"):
        until(listening)
        url = f"{scheme}:{address}:{port}?transport=tcp"
        yield url, certificate, ["--media-turn", url, "--media-turn-secret-file", str(secret_path)]


def run(args):
    with TemporaryDirectory(prefix="stillwatch-whep-") as folder:
        root = Path(folder)
        sources = root / "sources"
        sources.mkdir()
        encode = ["-c:v", "libx264", "-threads", "1", "-preset", "ultrafast"]
        if args.input_codec == "hevc":
            encode = ["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "pools=none:frame-threads=1:log-level=error"]
        source = {"type": "replay", "file": "fixture.mp4", "pace": True}
        if not args.dual_rtsp:
            subprocess.run([str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                            "testsrc2=size=320x240:rate=12", "-t", "60", *encode,
                            str(sources / "fixture.mp4")], check=True)
        lan_port = free_port(args.address)
        ice_port = free_port(args.address, socket.SOCK_DGRAM)
        data = root / "data"
        encoder = args.encoder or args.ffmpeg
        with contextlib.ExitStack() as stack:
            turn_url, turn_certificate, turn_flags = stack.enter_context(
                turn_server(args.turnserver, args.address, root, args.turn_tls_openssl),
            )
            flags = ["--lan-listen", f"{args.address}:{lan_port}"]
            if args.dual_rtsp:
                source, publish_detail = dual_sources(stack, args, root, sources, encode)
                flags.extend(["--rtsp-peer", source["peer"]])
                detail = stack.enter_context(launch(publish_detail, root / "detail.log"))
            if args.nginx:
                from proxy_core import proxy_profile

                proxy = stack.enter_context(proxy_profile(args.nginx, args.openssl, root))
                flags.extend(proxy.flags)
            stack.enter_context(patch.dict(os.environ, {"MTX_WEBRTC": "false", "RTSP_CONFKEY": "fixture"}))
            media_flags = [
                *flags, "--mediamtx", str(args.mediamtx),
                "--mediamtx-sha256", sha(args.mediamtx), "--media-encoder", str(encoder),
                "--media-encoder-sha256", sha(encoder), "--media-ice-udp", f"{args.address}:{ice_port}",
                *turn_flags,
            ]
            if args.detail_budget_test:
                media_flags.extend(["--media-detail-frame-bytes", str(640 * 480 * 3 * 3 - 1)])
            if args.media_admission:
                settings = root / "media-limits.json"
                settings.write_text(json.dumps({"media_workload_limits": {
                    "preview_publishers": 1, "detail_publishers": 1, "detail_decoders": 1,
                    "viewer_sessions": 4, "publisher_units": 3, "preview_units": 1,
                    "detail_units": 2, "detail_decoder_units": 1, "snapshot_encoders": 1,
                    "snapshot_requests_per_minute": 16,
                }}), encoding="utf-8")
                media_flags.extend(["--settings-file", str(settings)])
            if args.model_pack:
                media_flags.extend(["--model-pack", str(args.model_pack),
                                    "--model-manifest-sha256", sha(args.model_pack / "models.lock.json"),
                                    "--ort-library", str(args.ort_runtime), "--ort-library-sha256", sha(args.ort_runtime)])
            process, local = stack.enter_context(run_core(
                args.core, args.worker, data, sources, root / "core.log", *media_flags,
            ))
            config_file, = data.glob(".stillwatch-media-*/mediamtx.json")
            config = json.loads(config_file.read_text(encoding="utf-8"))
            assert config["authInternalUsers"][1]["pass"].startswith("sha256:")
            if turn_url:
                assert "password" not in config["webrtcICEServers2"][0]
            masks = [{"left": 0, "top": 0, "right": 5000, "bottom": 10000}]
            if args.dual_rtsp:
                def probe_inputs():
                    status, report = local.call("POST", "/api/v2/source-probes", {"source": source, "privacy_masks": masks})
                    return report if status == 200 else None

                report = until(probe_inputs, 12)
                assert report["dimensions"] == {"width": 320, "height": 240}
                assert report["main_stream"]["dimensions"] == {"width": 640, "height": 480}, report
                assert report["main_stream"]["codec"] == args.input_codec, report
                for stream in (report, report["main_stream"]):
                    assert stream["source_rate"]["quality"] == "observed", report
                    assert stream["source_rate"]["observed_millifps"] == 12_000, report
                for path in ("rtsp://127.0.0.1:1/detail", source["stream_path"], "/wrong_geometry"):
                    status, problem = local.call("POST", "/api/v2/source-probes", {
                        "source": source | {"main_stream_path": path}, "privacy_masks": masks,
                    })
                    assert status == 400, (status, problem)
                    if path == "/wrong_geometry":
                        assert "aspect ratio" in problem["detail"], problem
            status, camera = local.call("POST", "/api/v2/cameras", {
                "name": "Mask regression", "enabled": True, "source": source, "privacy_masks": masks,
            }, key=uuid.uuid4().hex)
            assert status == 201, (status, camera)
            camera_id = camera["id"]
            offer = issue(args.cli, local, data, camera_id, proxy=bool(args.nginx))
            cert = base64.urlsafe_b64decode(offer["tls_certificate_der"] + "=" * (-len(offer["tls_certificate_der"]) % 4))
            assert hashlib.sha256(cert).hexdigest() == offer["tls_certificate_sha256"]
            lan = TlsClient(offer["endpoint"], cert)
            key = root / "viewer.key"
            public = sign(args.node, key)
            label = "QA WHEP viewer"
            proof = (b"stillwatch/pair/v2\0" + encode_uuid(offer["core_instance_id"]) + encode_uuid(offer["challenge"])
                     + offer["secret"].encode() + label.encode())
            status, session = lan.request("POST", "/api/v2/pairing/finish", {
                "challenge": offer["challenge"], "secret": offer["secret"], "label": label,
                "public_key": public, "signature": sign(args.node, key, proof),
            })
            assert status == 201
            runtime_state = "streaming" if args.dual_rtsp else "replaying"
            until(lambda: local.camera(camera_id)["runtime"]["state"] == runtime_state)
            if args.detail_budget_test:
                asyncio.run(connect(lan, session, camera_id, turn_url, turn_certificate, profile="detail", offer_status=429))
            if args.mask_change and not args.detail_budget_test:
                playback = {"profile": "detail", "dimensions": (640, 480)} if args.dual_rtsp else {}
                asyncio.run(mask_change(lan, session, camera_id, local, turn_url, turn_certificate, **playback))
                status, _ = local.call("PATCH", f"/api/v2/cameras/{camera_id}", {
                    "privacy_masks": masks,
                }, key=uuid.uuid4().hex, revision=local.camera(camera_id)["revision"])
                assert status == 200
                until(lambda: local.camera(camera_id)["runtime"]["state"] == runtime_state)
            if args.dual_rtsp and not args.detail_budget_test:
                if args.model_pack:
                    until(lambda: local.camera(camera_id)["runtime"]["analysis"]["state"] == "ready", 20)
                asyncio.run(distinct_detail(lan, session, camera_id, local, process, turn_url, turn_certificate, detail,
                                            args.model_pack is not None))
                stack.enter_context(launch(publish_detail, root / "detail-restart.log"))
                until(probe_inputs, 12)
                async def restart_detail():
                    peer, _, grant = await connect(lan, session, camera_id, turn_url, turn_certificate,
                                                   profile="detail", dimensions=(640, 480))
                    try:
                        assert request(lan, session["access_token"], "DELETE", grant["whep_path"])[0] == 204
                    finally:
                        await peer.close()
                asyncio.run(restart_detail())
            if args.media_admission:
                asyncio.run(rotate_during_negotiation(lan, session, offer, key, args.node, camera_id,
                                                       turn_url, turn_certificate))

                async def reject_excess():
                    peer, track, grant = await connect(lan, session, camera_id, turn_url, turn_certificate,
                                                       profile="detail", dimensions=(640, 480))
                    token = session["access_token"]
                    try:
                        _, limits = local.call("GET", "/api/v2/status")
                        assert limits["media"]["workload_limits"]["publisher_units"] == 3
                        status, problem = lan.request("POST", "/api/v2/stream-sessions",
                                                      {"camera_id": camera_id, "profile": "preview"}, token)
                        assert status == 429, (status, problem)
                        before = local.camera(camera_id)["runtime"]["analysis"]
                        await asyncio.wait_for(track.recv(), 3)
                        if args.model_pack:
                            def accepted():
                                current = local.camera(camera_id)["runtime"]["analysis"]
                                return all(int(current[key]) > int(before[key]) for key in ("pose_frames", "fire_frames"))
                            await asyncio.to_thread(until, accepted, 8)
                        jpeg = f"/api/v2/cameras/{camera_id}/snapshot.jpg"
                        elapsed = (lambda: time.clock_gettime(time.CLOCK_MONOTONIC_RAW)) if sys.platform.startswith("linux") else time.perf_counter
                        for shot in range(16):
                            status, headers, data = await asyncio.to_thread(request, lan, token, "GET", jpeg)
                            assert status == 200 and data.startswith(b"\xff\xd8"), status
                            assert {name.lower(): value for name, value in headers.items()}.get("cache-control") == "no-store", headers
                            if shot == 0:
                                first_finished = elapsed()
                        assert (await asyncio.to_thread(request, lan, token, "GET", jpeg))[0] == 429
                    finally:
                        assert request(lan, token, "DELETE", grant["whep_path"])[0] == 204
                        await peer.close()
                    def available():
                        status, response = lan.request("POST", "/api/v2/stream-sessions",
                                                       {"camera_id": camera_id, "profile": "preview"}, token)
                        assert status in (201, 429), (status, response)
                        return response if status == 201 else None
                    preview = await asyncio.to_thread(until, available, 8)
                    assert request(lan, token, "DELETE", preview["whep_path"])[0] == 204
                    # Check expiry against physical elapsed time, including when the OS disciplines its clock.
                    while (remaining := first_finished + 60.20 - elapsed()) > 0:
                        await asyncio.sleep(min(remaining, 0.25))
                    status, headers, data = await asyncio.to_thread(request, lan, token, "GET", jpeg)
                    assert status == 200 and data.startswith(b"\xff\xd8"), status
                    assert 0 <= int({k.lower(): v for k, v in headers.items()}["x-frame-age-ms"]) <= 3000
                asyncio.run(reject_excess())
            if not args.media_admission:
                asyncio.run(viewers(lan, session, camera_id, local, turn_url, turn_certificate))
            if args.crash_restart:
                old_epoch = int(local.camera(camera_id)["source_epoch"])
                children = capture_children(process.pid)
                assert len(children) >= 2, children
                descriptors = []
                try:
                    for pid in children:
                        descriptors.append(os.pidfd_open(pid))
                    process.kill()
                    process.wait(timeout=6)
                    until(lambda: not workers_alive(children), 8)
                finally:
                    for descriptor in descriptors:
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        finally:
                            os.close(descriptor)
                assert config_file.is_file()
                with run_core(args.core, args.worker, data, sources, root / "restart.log", *media_flags) as (_, restored):
                    until(lambda: restored.call("GET", "/api/v2/status")[1]["media"]["router_ready"])
                    until(lambda: restored.camera(camera_id)["runtime"]["state"] == runtime_state)
                    assert int(restored.camera(camera_id)["source_epoch"]) > old_epoch
                    assert not config_file.exists()
        print(json.dumps({"native_mediamtx": True, "input_codec": args.input_codec, "whep_h264": True, "viewers": 2,
                          "privacy_mask_verified_on_decoded_video": True, "revoke_stops_stream": not args.media_admission,
                          "router_config_has_no_cleartext_credentials": True, "ignores_inherited_router_settings": True,
                          "mask_change_retires_previous_stream": args.mask_change and not args.detail_budget_test,
                          "dual_rtsp_shared_detail_and_independent_analysis": args.dual_rtsp and not args.detail_budget_test,
                          "actual_inference_continues": bool(args.model_pack) and args.dual_rtsp and not args.detail_budget_test,
                          "detail_frame_budget_rejects_without_blocking_preview": args.detail_budget_test,
                          "media_workload_rejects_excess_and_releases_budget": args.media_admission,
                          "decoded_after_grant_credential_rotation": args.media_admission,
                          "viewer_turn_over_tcp": bool(turn_url), "turn_tls": bool(turn_certificate),
                          "https_proxy": bool(args.nginx), "foreground_crash_restart": args.crash_restart}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "ffmpeg", "mediamtx", "node"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--address", required=True, help="Private IPv4 address assigned to an active LAN interface")
    parser.add_argument("--input-codec", choices=("h264", "hevc"), default="h264")
    parser.add_argument("--mask-change", action="store_true")
    parser.add_argument("--dual-rtsp", action="store_true")
    parser.add_argument("--detail-budget-test", action="store_true")
    parser.add_argument("--media-admission", action="store_true")
    parser.add_argument("--model-pack", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--ort-runtime", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--encoder", type=lambda path: Path(path).resolve(strict=True),
                        help="Use a separately built production media encoder")
    parser.add_argument("--turnserver", type=lambda path: Path(path).resolve(strict=True),
                        help="Run an isolated coturn and require a nominated TURN/TCP candidate")
    parser.add_argument("--turn-tls-openssl", type=lambda path: Path(path).resolve(strict=True),
                        help="Create and trust a temporary TURN certificate and require TURN/TLS")
    parser.add_argument("--crash-restart", action="store_true", help="Kill foreground Core and reuse its ICE address on Linux.")
    from proxy_core import proxy_arguments

    args = proxy_arguments(parser)
    if args.turn_tls_openssl and not args.turnserver:
        parser.error("--turn-tls-openssl requires --turnserver")
    if args.crash_restart and sys.platform != "linux":
        parser.error("--crash-restart requires Linux")
    if args.detail_budget_test and not args.dual_rtsp:
        parser.error("--detail-budget-test requires --dual-rtsp")
    if args.media_admission and (not args.dual_rtsp or args.detail_budget_test):
        parser.error("--media-admission requires --dual-rtsp with an available detail budget")
    if bool(args.model_pack) != bool(args.ort_runtime):
        parser.error("--model-pack and --ort-runtime must be used together")
    run(args)

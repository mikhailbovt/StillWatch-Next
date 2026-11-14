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
import socket
import ssl
import subprocess
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

import av
from aioice import TransportPolicy
from aiortc import RTCConfiguration, RTCIceServer, RTCPeerConnection, RTCSessionDescription

from lan_core import Client as TlsClient, encode_uuid, sign
from replay_core import run_core, until
from rtsp_core import launch
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


async def connect(lan, session, camera, turn_url, turn_certificate):
    token = session["access_token"]
    def issue():
        status, grant = lan.request("POST", "/api/v2/stream-sessions", {"camera_id": camera, "profile": "preview"}, token)
        assert status in (201, 503), (status, grant)
        return grant if status == 201 else None
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
        status, headers, body = await asyncio.to_thread(
            request, lan, token, "POST", grant["whep_path"], peer.localDescription.sdp.encode("utf-8"), "application/sdp")
        assert status == 201, (status, body.decode(errors="replace"))
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
        rgb = frame.to_ndarray(format="rgb24")
        if turn_url:
            assert ice._nominated[1].local_candidate.type == "relay"
        assert frame.width == 320 and frame.height == 240, (frame.width, frame.height)
        assert rgb[:, :135, :].max() < 25, "Masked region contains source pixels"
        assert rgb[:, 190:, :].max() > 100, "Unmasked region is empty"
        return peer, track, grant
    except Exception:
        await peer.close()
        raise


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
            if args.nginx:
                from proxy_core import proxy_profile

                proxy = stack.enter_context(proxy_profile(args.nginx, args.openssl, root))
                flags.extend(proxy.flags)
            stack.enter_context(patch.dict(os.environ, {"MTX_WEBRTC": "false", "RTSP_CONFKEY": "fixture"}))
            _, local = stack.enter_context(run_core(
                args.core, args.worker, data, sources, root / "core.log",
                *flags, "--mediamtx", str(args.mediamtx),
                "--mediamtx-sha256", sha(args.mediamtx), "--media-encoder", str(encoder),
                "--media-encoder-sha256", sha(encoder), "--media-ice-udp", f"{args.address}:{ice_port}",
                *turn_flags,
            ))
            config_file, = data.glob(".stillwatch-media-*/mediamtx.json")
            config = json.loads(config_file.read_text(encoding="utf-8"))
            assert config["authInternalUsers"][1]["pass"].startswith("sha256:")
            if turn_url:
                assert "password" not in config["webrtcICEServers2"][0]
            status, camera = local.call("POST", "/api/v2/cameras", {
                "name": "Mask regression", "enabled": True, "source": {"type": "replay", "file": "fixture.mp4", "pace": True},
                "privacy_masks": [{"left": 0, "top": 0, "right": 5000, "bottom": 10000}],
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
            until(lambda: local.camera(camera_id)["runtime"]["state"] == "replaying")
            asyncio.run(viewers(lan, session, camera_id, local, turn_url, turn_certificate))
        print(json.dumps({"native_mediamtx": True, "input_codec": args.input_codec, "whep_h264": True, "viewers": 2,
                          "privacy_mask_verified_on_decoded_video": True, "revoke_stops_stream": True,
                          "router_config_has_no_cleartext_credentials": True, "ignores_inherited_router_settings": True,
                          "viewer_turn_over_tcp": bool(turn_url), "turn_tls": bool(turn_certificate),
                          "https_proxy": bool(args.nginx)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "ffmpeg", "mediamtx", "node"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--address", required=True, help="Private IPv4 address assigned to an active LAN interface")
    parser.add_argument("--input-codec", choices=("h264", "hevc"), default="h264")
    parser.add_argument("--encoder", type=lambda path: Path(path).resolve(strict=True),
                        help="Use a separately built production media encoder")
    parser.add_argument("--turnserver", type=lambda path: Path(path).resolve(strict=True),
                        help="Run an isolated coturn and require a nominated TURN/TCP candidate")
    parser.add_argument("--turn-tls-openssl", type=lambda path: Path(path).resolve(strict=True),
                        help="Create and trust a temporary TURN certificate and require TURN/TLS")
    from proxy_core import proxy_arguments

    args = proxy_arguments(parser)
    if args.turn_tls_openssl and not args.turnserver:
        parser.error("--turn-tls-openssl requires --turnserver")
    run(args)

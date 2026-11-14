"""Probe and reconnect real RTSP/RTP TCP and UDP decoders over loopback."""

from __future__ import annotations

import argparse
import base64
import contextlib
import json
import os
import socket
import subprocess
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def rtp_ports():
    for _ in range(20):
        with socket.socket(type=socket.SOCK_DGRAM) as rtp, socket.socket(type=socket.SOCK_DGRAM) as rtcp:
            rtp.bind(("127.0.0.1", 0))
            port = rtp.getsockname()[1]
            if port % 2 == 1:
                continue
            try:
                rtcp.bind(("127.0.0.1", port + 1))
                return port, port + 1
            except OSError:
                continue
    raise RuntimeError("Unable to reserve a consecutive local RTP port pair")


@contextlib.contextmanager
def launch(args, log_file):
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with log_file.open("wb") as log:
        child = subprocess.Popen(args, cwd=log_file.parent, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=flags)
        try:
            yield child
        except Exception:
            print(log_file.read_text(errors="replace")[-4000:])
            raise
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)


def check_port(port):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.3):
            return True
    except OSError:
        return False


def windows_machine_secret(path, clear):
    command = """Add-Type -AssemblyName System.Security
$clear = [Convert]::FromBase64String([Console]::ReadLine())
$secret = [Security.Cryptography.ProtectedData]::Protect(
    $clear, $null, [Security.Cryptography.DataProtectionScope]::LocalMachine)
[Console]::Write([Convert]::ToBase64String($secret))"""
    result = subprocess.run(
        ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command],
        input=base64.b64encode(clear).decode("ascii") + "\n",
        text=True, capture_output=True, check=True, timeout=15,
    )
    path.write_bytes(base64.b64decode(result.stdout, validate=True))


def run(core, worker, mediamtx, ffmpeg, dpapi=False):
    with TemporaryDirectory(prefix="stillwatch-rtsp-") as temp:
        root = Path(temp)
        port = free_port()
        rtp, rtcp = rtp_ports()
        peer = f"127.0.0.1:{port}"
        config = root / "media.yml"
        config.write_text(f"""logLevel: info
rtsp: true
rtspTransports: [udp, tcp]
rtspAddress: {peer}
rtpAddress: 127.0.0.1:{rtp}
rtcpAddress: 127.0.0.1:{rtcp}
rtmp: false
hls: false
webrtc: false
srt: false
moq: false
authInternalUsers:
- user: any
  ips: [127.0.0.1]
  permissions:
  - action: publish
    path: fixture
- user: fixture-reader
  pass: fixture-password
  ips: [127.0.0.1]
  permissions:
  - action: read
    path: fixture
paths:
  fixture:
""")
        source_dir = root / "files"
        source_dir.mkdir()
        credentials_dir = root / "credentials"
        credentials_dir.mkdir()
        credential_id = str(uuid.uuid4())
        cred = credentials_dir / f"{credential_id}.json"
        clear = json.dumps({"username": "fixture-reader", "password": "fixture-password"})
        cred.write_text(clear)
        encrypted = cred.with_suffix(".dpapi")
        if dpapi:
            windows_machine_secret(encrypted, clear.encode("utf-8"))
            # A protected file must take precedence even if a legacy copy is present.
            cred.write_text("{}")
        if os.name != "nt":
            cred.chmod(0o600)
        publish = [str(ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi",
                   "-i", "testsrc2=size=192x144:rate=10", "-c:v", "libx264", "-threads", "1",
                   "-preset", "ultrafast", "-tune", "zerolatency", "-g", "10", "-f", "rtsp",
                   "-rtsp_transport", "tcp", f"rtsp://{peer}/fixture"]
        with launch([str(mediamtx), str(config)], root / "media.log"):
            until(lambda: check_port(port))
            with run_core(core, worker, root / "data", source_dir, root / "core.log",
                          "--rtsp-peer", peer, "--camera-credentials-dir", str(credentials_dir)) as (_, client):
                ids = []
                with launch(publish, root / "publisher.log") as publisher:
                    for transport in ("tcp", "udp"):
                        source = {"type": "rtsp", "peer": peer, "stream_path": "/fixture",
                                  "secure": False, "transport": transport, "credential_ref": credential_id}
                        assert client.call("POST", "/api/v2/source-probes", {"source": source | {"peer": f"127.0.0.1:{free_port()}"}})[0] == 400

                        def probe():
                            if publisher.poll() is not None:
                                raise RuntimeError("RTSP test publisher exited")
                            status, details = client.call("POST", "/api/v2/source-probes", {"source": source})
                            return details if status == 200 else None

                        details = until(probe, 20)
                        assert details["live"] and details["dimensions"] == {"width": 192, "height": 144}
                        assert details["codec"] == "h264" and details["capture_capacity"]["fits"], details
                        status, camera = client.call("POST", "/api/v2/cameras", {"name": transport, "source": source, "enabled": True}, key=transport)
                        assert status == 201 and camera["live"], (status, camera)
                        assert "fixture-password" not in json.dumps(camera)
                        ids.append(camera["id"])
                    def streaming():
                        cameras = [client.camera(camera_id) for camera_id in ids]
                        return cameras if all(c["runtime"]["state"] == "streaming" and int(c["runtime"]["frame"]) >= 3 for c in cameras) else None
                    initial = until(streaming, 15)
                    assert all(camera["runtime"]["codec"] == "h264" for camera in initial)
                    if dpapi:
                        wrong_id = str(uuid.uuid4())
                        (credentials_dir / f"{wrong_id}.json").write_text(clear)
                        (credentials_dir / f"{wrong_id}.dpapi").write_bytes(b"invalid DPAPI fixture")
                        failed = client.call("POST", "/api/v2/source-probes",
                                             {"source": source | {"credential_ref": wrong_id}})
                        assert failed[0] == 400 and "fixture-password" not in json.dumps(failed)
                        cred.unlink()
                until(lambda: any(client.camera(camera_id)["runtime"]["state"] in ("retrying", "starting") for camera_id in ids))
                assert client.call("GET", "/health/ready")[0] == 200
                with launch(publish, root / "publisher-restart.log"):
                    def recovered():
                        current = streaming()
                        return current and all(int(a["source_epoch"]) > int(b["source_epoch"]) for a, b in zip(current, initial, strict=True))
                    until(recovered, 20)
        print(json.dumps({"platform": os.name, "rtsp_tcp": True, "rtsp_udp": True, "credential_reference": True,
                          "reconnect": True, "unlisted_peer_rejected": True,
                          "windows_dpapi": dpapi, "invalid_encrypted_file_blocks_fallback": dpapi}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ("core", "worker", "mediamtx", "ffmpeg"):
        parser.add_argument(f"--{argument}", type=Path, required=True)
    parser.add_argument("--dpapi", action="store_true", help="Use a Windows machine-protected credential fixture.")
    options = parser.parse_args()
    if options.dpapi and os.name != "nt":
        parser.error("--dpapi requires Windows")
    run(*(getattr(options, arg).resolve(strict=True) for arg in ("core", "worker", "mediamtx", "ffmpeg")),
        dpapi=options.dpapi)

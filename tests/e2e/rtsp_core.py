"""Probe and reconnect real RTSP/RTP TCP and UDP decoders over loopback."""

from __future__ import annotations

import argparse
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


def run(core, worker, mediamtx, ffmpeg):
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
        cred.write_text(json.dumps({"username": "fixture-reader", "password": "fixture-password"}))
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
                        status, camera = client.call("POST", "/api/v2/cameras", {"name": transport, "source": source, "enabled": True}, key=transport)
                        assert status == 201 and camera["live"], (status, camera)
                        assert "fixture-password" not in json.dumps(camera)
                        ids.append(camera["id"])
                    def streaming():
                        cameras = [client.camera(camera_id) for camera_id in ids]
                        return cameras if all(c["runtime"]["state"] == "streaming" and int(c["runtime"]["frame"]) >= 3 for c in cameras) else None
                    initial = until(streaming, 15)
                until(lambda: any(client.camera(camera_id)["runtime"]["state"] in ("retrying", "starting") for camera_id in ids))
                assert client.call("GET", "/health/ready")[0] == 200
                with launch(publish, root / "publisher-restart.log"):
                    def recovered():
                        current = streaming()
                        return current and all(int(a["source_epoch"]) > int(b["source_epoch"]) for a, b in zip(current, initial, strict=True))
                    until(recovered, 20)
        print(json.dumps({"platform": os.name, "rtsp_tcp": True, "rtsp_udp": True, "credential_reference": True,
                          "reconnect": True, "unlisted_peer_rejected": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ("core", "worker", "mediamtx", "ffmpeg"):
        parser.add_argument(f"--{argument}", type=Path, required=True)
    options = parser.parse_args()
    run(*(getattr(options, arg).resolve(strict=True) for arg in ("core", "worker", "mediamtx", "ffmpeg")))

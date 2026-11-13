"""Decode authenticated RTSPS using a dedicated OS trust store for a test CA."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until
from rtsp_core import check_port, free_port, launch


def certificates(openssl, root, ca_store):
    def invoke(*args):
        subprocess.run([str(openssl), *args], cwd=root, check=True, capture_output=True)

    for issuer in ("trusted", "unknown"):
        invoke("req", "-new", "-newkey", "rsa:2048", "-nodes", "-x509", "-days", "1",
               "-subj", f"/CN=StillWatch RTSPS {issuer} test CA", "-keyout", f"{issuer}.key",
               "-out", f"{issuer}.pem", "-addext", "basicConstraints=critical,CA:TRUE",
               "-addext", "keyUsage=critical,keyCertSign,cRLSign")
    for name, issuer, address in (("correct", "trusted", "127.0.0.1"),
                                  ("wrong-ip", "trusted", "127.0.0.2"),
                                  ("wrong-ca", "unknown", "127.0.0.1")):
        invoke("req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=camera-fixture",
               "-keyout", f"{name}.key", "-out", f"{name}.csr")
        extension = root / f"{name}.ext"
        extension.write_text(f"basicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\n"
                             f"extendedKeyUsage=serverAuth\nsubjectAltName=IP:{address}\n")
        invoke("x509", "-req", "-in", f"{name}.csr", "-CA", f"{issuer}.pem", "-CAkey", f"{issuer}.key",
               "-CAcreateserial", "-out", f"{name}.pem", "-days", "1", "-extfile", extension.name)
    if ca_store is not None:
        with ca_store.open("wb") as store:
            store.write((root / "trusted.pem").read_bytes())


def run(core, worker, mediamtx, ffmpeg, openssl, ca_store):
    with TemporaryDirectory(prefix="stillwatch-rtsps-") as temp:
        root = Path(temp)
        certificates(openssl, root, ca_store)
        port, secure = free_port(), free_port()
        peer = f"127.0.0.1:{secure}"
        media = root / "media.yml"
        sources = root / "sources"
        sources.mkdir()
        credentials = root / "credentials"
        credentials.mkdir()
        credential = str(uuid.uuid4())
        reference = credentials / f"{credential}.json"
        reference.write_text(json.dumps({"username": "fixture", "password": "fixture-password"}))
        if os.name != "nt":
            reference.chmod(0o600)
        publish = [str(ffmpeg), "-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi",
                   "-i", "testsrc2=size=192x144:rate=10", "-c:v", "libx264", "-threads", "1",
                   "-preset", "ultrafast", "-tune", "zerolatency", "-g", "10", "-f", "rtsp",
                   "-rtsp_transport", "tcp", f"rtsp://127.0.0.1:{port}/fixture"]
        source = {"type": "rtsp", "peer": peer, "stream_path": "/fixture", "secure": True,
                  "transport": "tcp", "credential_ref": credential}
        cases = ("correct", "wrong-ip", "wrong-ca") if ca_store is not None else ("wrong-ca",)
        with run_core(core, worker, root / "data", sources, root / "core.log",
                      "--rtsp-peer", peer, "--camera-credentials-dir", str(credentials)) as (_, client):
            for name in cases:
                media.write_text(f"""logLevel: info
rtsp: true
rtspEncryption: optional
rtspTransports: [tcp]
rtspAddress: 127.0.0.1:{port}
rtspsAddress: {peer}
rtspServerCert: {name}.pem
rtspServerKey: {name}.key
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
- user: fixture
  pass: fixture-password
  ips: [127.0.0.1]
  permissions:
  - action: read
    path: fixture
paths:
  fixture:
""")
                log = root / f"{name}-media.log"
                with launch([str(mediamtx), str(media)], log):
                    until(lambda: check_port(port))
                    with launch(publish, root / f"{name}-publish.log") as publisher:
                        until(lambda: "is publishing to path 'fixture'" in log.read_text(errors="replace"))
                        assert publisher.poll() is None
                        status, details = client.call("POST", "/api/v2/source-probes", {"source": source})
                        if name == "correct":
                            assert status == 200 and details["dimensions"] == {"width": 192, "height": 144}, details
                            status, camera = client.call("POST", "/api/v2/cameras", {
                                "name": "RTSPS", "enabled": True, "source": source,
                            }, key="rtsps-qa")
                            assert status == 201 and camera["live"], camera
                            until(lambda: int(client.camera(camera["id"])["runtime"]["frame"] or 0) >= 3)
                            assert client.call("DELETE", f"/api/v2/cameras/{camera['id']}",
                                               key="rtsps-remove", revision=camera["revision"])[0] == 204
                        else:
                            assert status == 400, (name, status, details)
                            assert "fixture-password" not in json.dumps(details)
    print(json.dumps({"platform": os.name, "trusted_camera_decoded": "correct" in cases,
                      "trusted_wrong_ip_rejected": "wrong-ip" in cases, "unknown_ca_rejected": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "mediamtx", "ffmpeg", "openssl"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--isolated-ca-store", type=Path,
                        help="Writable CA bundle in a disposable test container or mount namespace")
    args = parser.parse_args()
    run(*(getattr(args, name).resolve(strict=True) for name in ("core", "worker", "mediamtx", "ffmpeg", "openssl")),
        args.isolated_ca_store)

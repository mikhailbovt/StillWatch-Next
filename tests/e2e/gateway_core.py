"""Verify the outbound relay with a real TLS-terminating Core endpoint."""

from __future__ import annotations

import argparse
import contextlib
import http.client
import json
import secrets
import socket
import ssl
import subprocess
import urllib.request
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.error import HTTPError, URLError

from lan_core import Client, create_camera, encode_uuid, sign
from proxy_core import proxy_profile
from replay_core import run_core, until
from rtsp_core import free_port, launch


def private_token(path):
    path.write_text(secrets.token_hex(32) + "\n", encoding="ascii", newline="\n")
    path.chmod(0o600)
    return path


def make_identity(openssl, root, address):
    cert, key = root / f"{address}.der", root / f"{address}.key"
    pem = cert.with_suffix(".pem")
    key_pem = key.with_suffix(".pem.key")
    commands = [
        ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-days", "1",
         "-subj", f"/CN={address}", "-addext", f"subjectAltName=IP:{address}",
         "-addext", "basicConstraints=critical,CA:FALSE", "-addext", "extendedKeyUsage=serverAuth",
         "-keyout", str(key_pem), "-out", str(pem)],
        ["pkcs8", "-topk8", "-nocrypt", "-in", str(key_pem), "-outform", "DER", "-out", str(key)],
    ]
    for command in commands:
        subprocess.run([str(openssl), *command], check=True, capture_output=True)
    cert.write_bytes(ssl.PEM_cert_to_DER_cert(pem.read_text(encoding="ascii")))
    key.chmod(0o600)
    return cert, key


def unique_ports(count):
    ports = set()
    while len(ports) < count:
        ports.add(free_port())
    return ports


def denied(remote):
    try:
        remote.request("GET", "/bootstrap/info")
    except (OSError, URLError):
        return True
    return False


def run(args):
    with TemporaryDirectory(prefix="stillwatch-relay-") as folder, contextlib.ExitStack() as stack:
        root = Path(folder)
        data, sources = root / "data", root / "sources"
        sources.mkdir()
        public, connector, nginx = unique_ports(3)
        relay_name = args.relay_address
        certificate, key = make_identity(args.openssl, root, relay_name)
        other = root / "other-identity"
        other.mkdir()
        wrong, _ = make_identity(args.openssl, other, relay_name)
        token = private_token(root / "relay.token")
        wrong_token = private_token(root / "wrong.token")
        proxy = stack.enter_context(proxy_profile(args.nginx, args.openssl, root, port=public, listen_port=nginx))
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.load_verify_locations(cadata=ssl.DER_cert_to_PEM_cert(proxy.der))
        remote = Client(proxy.endpoint, proxy.der)
        public_address = args.relay_address
        _, local = stack.enter_context(run_core(args.core, args.worker, data, sources, root / "core.log", *proxy.flags))
        visible = create_camera(local, "Visible")
        hidden = create_camera(local, "Hidden")
        _, offer = local.call("POST", "/api/v2/pairing", {
            "role": "viewer", "camera_ids": [visible["id"]], "scopes": ["streams_read"],
        })
        assert offer["endpoint"] == proxy.endpoint
        instance = offer["core_instance_id"]
        bind = f"{public_address}:{connector}"
        gateway = str(args.gateway)
        serve = [gateway, "serve", "--listen", f"{public_address}:{public}", "--connector-listen", bind,
                 "--certificate-der", str(certificate), "--private-key-der", str(key), "--token-file", str(token)]

        def tls_connection():
            connection = http.client.HTTPSConnection(proxy.endpoint.removeprefix("https://"),
                                                     context=context, timeout=4)
            if args.relay_namespace:
                connection._create_connection = lambda _, timeout, source: socket.create_connection(
                    (public_address, public), timeout, source)
            return connection

        if args.relay_namespace:
            serve = ["ip", "netns", "exec", args.relay_namespace, *serve]
            class RoutedHttps(urllib.request.HTTPSHandler):
                def https_open(self, request):
                    return self.do_open(lambda *_, **__: tls_connection(), request)
            remote.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), RoutedHttps(context=context))

        def connect(cert, credential):
            return [gateway, "connect", "--relay", bind, "--relay-name", relay_name,
                    "--relay-certificate-der", str(cert), "--token-file", str(credential),
                    "--upstream", f"127.0.0.1:{nginx}", "--connections", "16"]

        def core_visible():
            try:
                status, info = remote.request("GET", "/bootstrap/info")
                return status == 200 and info["instance_id"] == instance
            except (OSError, URLError):
                return False

        with launch(serve, root / "serve.log") as original:
            until(lambda: "listening" in (root / "serve.log").read_text(encoding="utf-8"))
            assert denied(remote)
            with launch(connect(wrong, token), root / "wrong-cert.log"), \
                 launch(connect(certificate, wrong_token), root / "wrong-token.log"):
                for _ in range(3):
                    assert denied(remote)
            with launch(connect(certificate, token), root / "connect.log"):
                until(core_visible)
                with socket.create_connection((public_address, public), timeout=2) as plain:
                    plain.sendall(b"GET / HTTP/1.0\r\n\r\n")
                    try:
                        assert not plain.recv(32)
                    except ConnectionResetError:
                        pass
                assert remote.request("GET", "/api/v2/cameras", token=local.token)[0] == 401
                label = "QA remote viewer"
                device_key = root / "device.key"
                public_key = sign(args.node, device_key)
                proof = b"stillwatch/pair/v2\0" + encode_uuid(instance) + encode_uuid(offer["challenge"])
                proof += offer["secret"].encode() + label.encode()
                status, session = remote.request("POST", "/api/v2/pairing/finish", {
                    "challenge": offer["challenge"], "secret": offer["secret"], "label": label,
                    "public_key": public_key, "signature": sign(args.node, device_key, proof),
                })
                assert status == 201
                access = session["access_token"]
                assert remote.request("GET", f"/api/v2/cameras/{visible['id']}", token=access)[0] == 200
                assert remote.request("GET", f"/api/v2/cameras/{hidden['id']}", token=access)[0] == 404
                try:
                    with remote.opener.open(proxy.endpoint + "/api/v2/replay-uploads", data=b"", timeout=4):
                        raise AssertionError("relay accepted a replay upload")
                except HTTPError as error:
                    with error:
                        assert error.code == 403
            until(lambda: denied(remote))
            with launch(connect(certificate, token), root / "reconnect.log") as outbound:
                until(core_visible)
                original.terminate()
                original.wait(timeout=5)
                assert denied(remote)
                stack.enter_context(launch(serve, root / "restart.log"))
                until(core_visible)
                assert outbound.poll() is None
                with contextlib.closing(tls_connection()) as client:
                    client.request("GET", "/api/v2/changes", headers={"Authorization": f"Bearer {access}"})
                    with client.getresponse() as response:
                        assert response.status == 200
                        assert response.headers["Content-Type"].startswith("text/event-stream")
                device = session["device"]["id"]
                assert local.call("DELETE", f"/api/v2/devices/{device}")[0] in (200, 204)
                assert remote.request("GET", "/api/v2/cameras", token=access)[0] == 401
        print(json.dumps({"gateway_tls_and_token": True, "private_tls_endpoint": True,
                          "plain_http_refused": True, "scoped_pairing": True,
                          "relay_reconnect": True, "sse": True, "revoke": True,
                          "relay_namespace": args.relay_namespace is not None}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for item in ("core", "worker", "gateway", "nginx", "openssl", "node"):
        parser.add_argument(f"--{item}", type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--relay-address", default="127.0.0.1")
    parser.add_argument("--relay-namespace", help="Existing Linux network namespace with the supplied address")
    run(parser.parse_args())

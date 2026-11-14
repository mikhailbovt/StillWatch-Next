"""Pair and rotate real LAN devices over the pinned Core TLS listener."""

from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
import http.client
import json
import ssl
import sqlite3
import subprocess
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until
from rtsp_core import free_port

SIGNER = """
const fs = require('node:fs');
const crypto = require('node:crypto');
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
if (input.action === 'create') {
    const keys = crypto.generateKeyPairSync('ed25519');
    fs.writeFileSync(input.path, keys.privateKey.export({type: 'pkcs8', format: 'pem'}), {mode: 0o600});
    process.stdout.write(keys.publicKey.export({type: 'spki', format: 'der'}).subarray(-32).toString('hex'));
} else {
    const key = crypto.createPrivateKey(fs.readFileSync(input.path));
    process.stdout.write(crypto.sign(null, Buffer.from(input.message, 'base64'), key).toString('hex'));
}
"""


def sign(node, path, message=None):
    payload = {"path": str(path), "action": "create" if message is None else "sign"}
    if message is not None:
        payload["message"] = base64.b64encode(message).decode("ascii")
    return subprocess.run([str(node), "-e", SIGNER], input=json.dumps(payload), text=True,
                          capture_output=True, check=True).stdout


def encode_uuid(text):
    return uuid.UUID(text).bytes


class Client:
    def __init__(self, endpoint, certificate=None):
        context = ssl.create_default_context()
        if certificate:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.load_verify_locations(cadata=ssl.DER_cert_to_PEM_cert(certificate))
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                                                  urllib.request.HTTPSHandler(context=context))
        self.endpoint = endpoint
        self.context = context

    def request(self, method, path, body=None, token=None, extra=None):
        headers = dict(extra or {})
        if token:
            headers["Authorization"] = f"Bearer {token}"
        if body is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(self.endpoint + path, method=method, headers=headers,
                                         data=None if body is None else json.dumps(body).encode("utf-8"))
        try:
            response = self.opener.open(request, timeout=4)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(64 * 1024)
            return response.status, json.loads(raw) if raw else None

def offer_from_cli(cli, local, data, camera):
    command = [str(cli), "pair", "--local", local, "--data-dir", str(data),
               "--role", "viewer", "--camera", camera, "--scope", "streams-read", "--json"]
    result = subprocess.run(command, text=True, check=True, capture_output=True)
    return json.loads(result.stdout)


def run(args):
    with TemporaryDirectory(prefix="stillwatch-lan-") as temporary:
        root = Path(temporary)
        data = root / "data"
        local_root = root / "replays"
        local_root.mkdir()
        address = f"127.0.0.1:{free_port()}"
        database = data / "stillwatch.sqlite3"
        from alarm_agent import insert_urgent
        with run_core(args.core, args.worker, data, local_root, root / "core.log",
                      "--lan-listen", address) as (_, local):
            visible = create_camera(local, "Visible")
            hidden = create_camera(local, "Hidden")
            local_address = local.address.removeprefix("http://")
            offer = offer_from_cli(args.cli, local_address, data, visible["id"])
            assert offer["endpoint"] == f"https://{address}"
            cert = base64.urlsafe_b64decode(offer["tls_certificate_der"] + "=" * (-len(offer["tls_certificate_der"]) % 4))
            assert hashlib.sha256(cert).hexdigest() == offer["tls_certificate_sha256"]
            try:
                Client(offer["endpoint"]).request("GET", "/bootstrap/info")
            except urllib.error.URLError as error:
                assert isinstance(error.reason, ssl.SSLCertVerificationError)
            else:
                raise AssertionError("Unpinned connection unexpectedly trusted")
            lan = Client(offer["endpoint"], cert)
            assert lan.request("GET", "/bootstrap/info")[1]["instance_id"] == offer["core_instance_id"]
            assert lan.request("GET", "/api/v2/cameras", token=local.token)[0] == 401
            key = root / "device.key"
            label = "QA viewer"
            public = sign(args.node, key)
            proof = b"stillwatch/pair/v2\0" + encode_uuid(offer["core_instance_id"]) + encode_uuid(offer["challenge"])
            proof += offer["secret"].encode() + label.encode()
            pairing = {"challenge": offer["challenge"], "secret": offer["secret"], "label": label,
                       "public_key": public, "signature": sign(args.node, key, proof)}
            assert lan.request("POST", "/api/v2/pairing/finish", pairing | {"signature": "0" * 128})[0] == 401
            status, session = lan.request("POST", "/api/v2/pairing/finish", pairing)
            assert status == 201 and session["device"]["role"] == "viewer"
            assert lan.request("POST", "/api/v2/pairing/finish", pairing)[0] == 401
            token = session["access_token"]
            device_id = session["device"]["id"]
            status, page = lan.request("GET", "/api/v2/cameras?limit=1", token=token)
            assert status == 200 and [item["id"] for item in page["cameras"]] == [visible["id"]] and page["next"] is None
            assert lan.request("GET", f"/api/v2/cameras/{hidden['id']}", token=token)[0] == 404
            assert lan.request("GET", "/api/v2/devices", token=token)[0] == 403
            assert lan.request("POST", "/api/v2/cameras", visible["config"], token)[0] == 403
            assert lan.request("GET", "/api/v2/cameras", token=token,
                               extra={"Origin": "https://example.invalid"})[0] == 403
            alerts = [uuid.uuid4() for _ in range(131)]
            for incident in alerts:
                insert_urgent(database, visible["id"], incident)
            insert_urgent(database, hidden["id"], uuid.uuid4())
            status, first_page = lan.request("GET", "/api/v2/alerts", token=token)
            assert status == 200 and first_page["next"] and len(first_page["alerts"]) == 128
            assert [item["incident_id"] for item in first_page["alerts"]] == list(map(str, alerts[:128]))
            receipt = first_page["alerts"][0]
            for _ in range(2):
                assert lan.request("POST", f"/api/v2/alerts/{receipt['id']}/seen",
                                   {"attention_cycle": receipt["attention_cycle"]}, token=token)[0] == 204
            late = uuid.uuid4()
            insert_urgent(database, visible["id"], late)
            path = "/api/v2/alerts?after=" + first_page["next"]
            assert local.call("GET", path)[0] == 409
            status, tail = lan.request("GET", path, token=token)
            assert status == 200 and tail.get("next") is None and tail["head"] == first_page["head"]
            assert [item["incident_id"] for item in tail["alerts"]] == list(map(str, alerts[128:]))
            assert local.call("GET", f"/api/v2/incidents/{alerts[0]}")[1]["acknowledgement"] is None
            refresh = refresh_request(args.node, key, offer, session)
            assert lan.request("GET", "/bootstrap/info")[1]["refresh_replay"] is True
            def rotated_on_disk():
                with closing(sqlite3.connect(data / "stillwatch.sqlite3")) as db:
                    return db.execute("SELECT count(*) FROM refresh_receipts").fetchone()[0] == 1
            # Keep the socket open until SQLite commits and discard the response
            # without reading it, regardless of when the TLS server wrote it.
            with closing(http.client.HTTPSConnection(address, context=lan.context, timeout=4)) as connection:
                connection.request("POST", "/api/v2/device-session/refresh", json.dumps(refresh),
                                   {"Content-Type": "application/json"})
                until(rotated_on_disk)
            assert lan.request("GET", "/api/v2/alerts", token=token)[0] == 401
        with run_core(args.core, args.worker, data, local_root, root / "restart.log", "--lan-listen", address) as (_, local):
            until(lambda: lan.request("GET", "/bootstrap/info")[0] == 200)
            with ThreadPoolExecutor(max_workers=2) as pool:
                first, second = list(pool.map(lambda _: lan.request("POST", "/api/v2/device-session/refresh", refresh), range(2)))
            assert first == second and first[0] == 200
            rotated = first[1]
            assert rotated["refresh_token"] != session["refresh_token"]
            assert lan.request("GET", "/api/v2/alerts", token=rotated["access_token"])[0] == 200
            assert lan.request("GET", path, token=rotated["access_token"])[0] == 409
            assert lan.request("POST", "/api/v2/device-session/refresh",
                               refresh_request(args.node, key, offer, session))[0] == 401
            assert lan.request("GET", "/api/v2/alerts", token=rotated["access_token"])[0] == 401
            status, devices = local.call("GET", "/api/v2/devices")
            assert status == 200 and devices[0]["id"] == device_id and devices[0]["revoked_at"]
            next_offer = offer_from_cli(args.cli, local.address.removeprefix("http://"), data, visible["id"])
            assert next_offer["tls_certificate_sha256"] == offer["tls_certificate_sha256"]
        print(json.dumps({"certificate_pin": True, "tls_identity_survives_restart": True, "ed25519_pairing": True,
                          "scope": True, "single_use_pairing": True, "refresh_rotation_and_reuse_revoke": True,
                          "lost_response_restart_and_concurrent_replay": True,
                          "alert_pages_scoped_and_stable": True, "alert_count": len(alerts)}))


def refresh_request(node, key, offer, session):
    device = session["device"]["id"]
    nonce = str(uuid.uuid4())
    token = session["refresh_token"]
    proof = b"stillwatch/refresh/v2\0" + encode_uuid(offer["core_instance_id"]) + encode_uuid(device) + encode_uuid(nonce)
    return {"device_id": device, "nonce": nonce, "refresh_token": token,
            "signature": sign(node, key, proof + token.encode())}


def create_camera(local, name):
    status, camera = local.call("POST", "/api/v2/cameras", {"name": name, "enabled": False,
                                "source": {"type": "replay", "file": f"{name}.mp4", "pace": True}},
                                key=uuid.uuid4().hex)
    assert status == 201, (status, camera)
    return camera


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "node"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    run(parser.parse_args())

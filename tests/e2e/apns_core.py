"""Verify scoped iOS APNs dispatch through a native Core and an isolated loopback receiver."""

import argparse
import base64
from contextlib import closing, contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from queue import Queue
import secrets
import sqlite3
import threading
from tempfile import TemporaryDirectory
import time
from uuid import UUID, uuid4

from alarm_agent import insert_urgent
from lan_core import Client, encode_uuid, sign
from replay_core import run_core, until
from rtsp_core import free_port
from sdk_core import create_camera, issue

REGISTRATION = "/api/v2/alerts/ios/registration"
NOTICES = "/api/v2/alerts/ios/notices/"


@contextmanager
def receiver(token):
    deliveries = Queue()
    responses = Queue()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            if self.path != "/deliver" or self.headers.get("Authorization") != "Bearer " + token:
                self.send_error(401)
                return
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            finished = threading.Event()
            deliveries.put((body, finished))
            code = responses.get(timeout=5)
            self.send_response(code)
            self.end_headers()
            finished.set()

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server.server_port, deliveries, responses
        finally:
            server.shutdown()
            thread.join(timeout=5)


def pair(args, local, data, path, scope):
    offer = issue(args.cli, local, data, scope)
    encoded = offer["tls_certificate_der"]
    certificate = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
    client = Client(offer["endpoint"], certificate)
    label = path.stem
    public = sign(args.node, path)
    proof = b"stillwatch/pair/v2\0" + encode_uuid(offer["core_instance_id"]) + encode_uuid(offer["challenge"])
    proof += offer["secret"].encode() + label.encode()
    code, session = client.request("POST", "/api/v2/pairing/finish", {
        "challenge": offer["challenge"], "secret": offer["secret"], "label": label,
        "public_key": public, "signature": sign(args.node, path, proof),
    })
    assert code == 201
    return client, session["access_token"], session["device"]["id"]


def run(args):
    with TemporaryDirectory(prefix="stillwatch-apns-") as folder:
        root = Path(folder)
        data = root / "data"
        sources = root / "sources"
        sources.mkdir()
        credential = secrets.token_hex(32)
        credential_file = root / "delivery-token"
        credential_file.touch(mode=0o600)
        credential_file.write_text(credential, encoding="ascii")
        device_token = secrets.token_hex(29)
        lan = f"127.0.0.1:{free_port()}"
        database = data / "stillwatch.sqlite3"

        def query(sql, *values):
            with closing(sqlite3.connect(database)) as db, db:
                return db.execute(sql, values).fetchall()

        with receiver(credential) as (port, deliveries, responses):
            def core(log):
                return run_core(args.core, args.worker, data, sources, root / log,
                                "--lan-listen", lan, "--apns-worker", f"127.0.0.1:{port}",
                                "--apns-token-file", str(credential_file))

            with core("first.log") as (_, local):
                scoped = create_camera(local, "Scoped")
                hidden = create_camera(local, "Hidden")
                client, access, device_id = pair(args, local, data, root / "first-device", scoped)
                _, second_access, _ = pair(args, local, data, root / "second-device", scoped)
                assert client.request("GET", "/bootstrap/info")[1]["ios_push"] is True
                assert local.call("PUT", REGISTRATION, {"token": device_token})[0] == 403
                assert client.request("PUT", REGISTRATION, {"token": "AA:%"}, access)[0] == 400
                assert client.request("PUT", REGISTRATION, {"token": device_token}, access)[0] == 204
                assert client.request("PUT", REGISTRATION, {"token": device_token}, access)[0] == 204
                stored, = query("SELECT encrypted_token FROM ios_push_registrations")
                assert device_token.encode() not in stored[0]
                insert_urgent(database, hidden, uuid4())
                insert_urgent(database, scoped, uuid4(), live=False)
                incident = uuid4()
                insert_urgent(database, scoped, incident)
                responses.put(503)
                first, finished = deliveries.get(timeout=8)
                assert finished.wait(timeout=3)
                assert set(first) == {"token", "notice", "expires_at"} and first["token"] == device_token
                assert str(UUID(first["notice"])) == first["notice"]
            with core("restarted.log") as (_, local):
                responses.put(204)
                second, finished = deliveries.get(timeout=12)
                assert first == second and finished.wait(timeout=3)
                until(lambda: query("SELECT count(*) FROM ios_push_deliveries WHERE provider_accepted_at IS NOT NULL")[0][0] == 1)
                assert query("SELECT count(*) FROM notification_receipts") == [(0,)]
                path = NOTICES + first["notice"]
                assert client.request("GET", path, token=second_access)[0] == 404
                code, notice = client.request("GET", path, token=access)
                assert code == 200 and notice["alert"]["incident_id"] == str(incident)
                alert = notice["alert"]
                assert client.request("POST", f"/api/v2/alerts/{alert['id']}/seen",
                                      {"attention_cycle": alert["attention_cycle"]}, access)[0] == 204
                assert query("SELECT count(*) FROM notification_receipts") == [(1,)]
                assert local.call("GET", f"/api/v2/incidents/{incident}")[1]["acknowledgement"] is None
                query("UPDATE notification_outbox SET expires_at = ? WHERE incident_id = ?",
                      time.time_ns() // 1_000_000 - 1, incident.bytes)
                assert client.request("GET", path, token=access)[0] == 404
                only_visible = uuid4()
                insert_urgent(database, scoped, only_visible)
                third, finished = deliveries.get(timeout=8)
                assert third["notice"] != first["notice"]
                assert local.call("DELETE", f"/api/v2/devices/{device_id}", key=uuid4().hex)[0] == 204
                responses.put(204)
                assert finished.wait(timeout=3)
                assert client.request("GET", NOTICES + third["notice"], token=access)[0] == 401
                assert query("SELECT count(*) FROM ios_push_registrations") == [(0,)]
                assert query("SELECT count(*) FROM ios_push_deliveries") == [(0,)]
                insert_urgent(database, scoped, uuid4())
                time.sleep(2)
                assert deliveries.empty()
        print(json.dumps({"native_core": True, "apple_provider_fixture": True, "scope_and_no_replay_push": True,
                          "registration_encrypted": True, "minimal_envelope": True, "stable_retry_after_restart": True,
                          "acceptance_receipt_ack_distinct": True, "expiry_and_inflight_revocation": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli", "node"):
        parser.add_argument(f"--{name}", required=True, type=lambda v: Path(v).resolve(strict=True))
    run(parser.parse_args())

"""Run four public clients against the same native Core and one change stream."""

from __future__ import annotations

import argparse
import asyncio
from contextlib import ExitStack, closing
import json
import sqlite3
import subprocess
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import UUID, uuid4

from replay_core import run_core
from rtsp_core import free_port

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "tests/fixtures/contracts/sdk.json"


def issue(cli, local, data, camera, *, proxy=False):
    arguments = [str(cli), "pair", "--local", local.address.removeprefix("http://"), "--data-dir", str(data),
                 "--role", "viewer", "--camera", camera, "--scope", "streams-read", "--json"]
    if proxy:
        arguments.append("--proxy")
    return json.loads(subprocess.check_output(arguments, text=True))


def create_camera(local, name):
    status, result = local.call("POST", "/api/v2/cameras", {
        "name": name, "enabled": False, "source": {"type": "replay", "file": "unused.mp4", "pace": True},
    }, key=uuid4().hex)
    assert status == 201
    return result["id"]


def open_incident(database, camera, index):
    ident = uuid4()
    when = time.time_ns() // 1_000_000
    row = {
        "id": str(ident), "scope": {"camera_id": camera, "epoch": 1, "scene_revision": 1},
        "revision": 1, "category": "fire", "detection_reason": "fixture " + str(index),
        "first_source_us": 0, "last_source_us": 1_000_000, "live": True,
        "created_at": when, "updated_at": when, "assessment": "concern", "attention": "review",
        "attention_cycle": 1, "acknowledgement": None, "resolution": None, "evidence": None,
        "evidence_state": "unavailable", "assessed_evidence_version": None,
    }
    payload = json.dumps(row)
    with closing(sqlite3.connect(database, timeout=3)) as connection, connection:
        connection.execute(
            "INSERT INTO incidents(id, camera_id, revision, created_at, updated_at, resolved, attention, "
            "state, creation_hash) VALUES (?,?,?,?,?,?,?,?,?)",
            (ident.bytes, UUID(camera).bytes, 1, when, when, 0, 1, payload, bytes(32)),
        )
        connection.execute(
            "INSERT INTO change_events(incident_id, revision, occurred_at, opened, incident) VALUES (?,?,?,?,?)",
            (ident.bytes, 1, when, 1, payload),
        )


def python_client():
    from stillwatch_sdk import AsyncClient, Client, CoreError, Cursor, counter

    fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))
    for value in fixture["counter_valid"]:
        assert counter(value) == value
    for value in fixture["counter_invalid"]:
        try:
            counter(value)
        except CoreError:
            pass
        else:
            raise AssertionError("invalid counter accepted")

    options = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
    registration = Client.enroll(options["offer"], "SDK QA Python")
    with Client(registration.core, seed=registration.seed, credentials=registration.credentials, persist=lambda _: None) as client:
        assert client.core()["instance_id"] == registration.core.instance
        assert [camera["id"] for camera in client.cameras()["cameras"]] == [options["camera"]]
    registration.credentials["access_expires_at"] = "2001-01-01T00:00:00Z"

    async def check():
        refreshes = 0

        async def persist(_):
            nonlocal refreshes
            refreshes += 1

        async with AsyncClient(registration.core, seed=registration.seed, credentials=registration.credentials, persist=persist) as client:
            snapshot, _, cameras = await asyncio.gather(client.snapshot(), client.snapshot(), client.cameras())
            assert refreshes == 1
            assert [camera["id"] for camera in cameras["cameras"]] == [options["camera"]]
            stream = client.changes(Cursor.parse(snapshot["cursor"]))
            try:
                event = await asyncio.wait_for(anext(stream), timeout=30)
                assert event.data["payload"]["camera_id"] == options["camera"]
            finally:
                await stream.aclose()

    asyncio.run(check())
    print("python sync/async pairing, refresh and scoped SSE verified")


def run(args):
    with TemporaryDirectory(prefix="stillwatch-sdk-") as folder:
        root = Path(folder)
        sources = root / "sources"
        sources.mkdir()
        data = root / "data"
        with ExitStack() as stack:
            flags = ["--lan-listen", f"127.0.0.1:{free_port()}"]
            if args.nginx:
                from proxy_core import proxy_profile

                proxy = stack.enter_context(proxy_profile(args.nginx, args.openssl, root))
                flags.extend(proxy.flags)
            _, local = stack.enter_context(run_core(args.core, args.worker, data, sources, root / "core.log", *flags))
            visible = create_camera(local, "Visible SDK camera")
            hidden = create_camera(local, "Hidden SDK camera")
            commands = {
                "python": [sys.executable, __file__, "--python-client"],
                "dart": [str(args.dart), "run", "bin/check_core.dart", str(FIXTURE)],
                "typescript": [str(args.node), "scripts/check-core.mjs", str(FIXTURE)],
                "rust": [str(args.rust_client), str(FIXTURE)],
            }
            processes = {}
            try:
                for name, command in commands.items():
                    configuration = root / (name + ".json")
                    with configuration.open("x", encoding="utf-8") as stream:
                        if sys.platform != "win32":
                            configuration.chmod(0o600)
                        json.dump({
                            "camera": visible,
                            "offer": issue(args.cli, local, data, visible, proxy=bool(args.nginx)),
                        }, stream)
                    working = ROOT / ("packages/stillwatch-" + name) if name in ("dart", "typescript") else ROOT
                    processes[name] = subprocess.Popen([*command, str(configuration)], cwd=working)
                for index in range(16):
                    if all(process.poll() is not None for process in processes.values()):
                        break
                    time.sleep(1)
                    for camera in (hidden, visible):
                        open_incident(data / "stillwatch.sqlite3", camera, index)
                statuses = {name: process.wait(timeout=20) for name, process in processes.items()}
                assert set(statuses.values()) == {0}, statuses
            finally:
                for process in processes.values():
                    if process.poll() is None:
                        process.kill()
                        process.wait()
        print(json.dumps({"sdk": list(commands), "pinned_tls": True, "scoped_sse": True,
                          "counter_fixtures": True, "https_proxy": bool(args.nginx)}))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--python-client":
        python_client()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        for name in ("core", "worker", "cli", "node", "dart", "rust-client"):
            parser.add_argument("--" + name, type=lambda path: Path(path).resolve(strict=True), required=True)
        from proxy_core import proxy_arguments

        run(proxy_arguments(parser))

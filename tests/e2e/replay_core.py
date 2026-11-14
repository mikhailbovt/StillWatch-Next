"""Exercise native Core, SQLite, and four supervised file decoders over loopback."""

from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import json
import os
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from tempfile import TemporaryDirectory


def until(check, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("timed out waiting for native Core state")


def capture_children(pid):
    if os.name == "nt":
        script = f"Get-CimInstance Win32_Process -Filter 'ParentProcessId = {pid} AND Name = \"stillwatch-worker.exe\"' | Select-Object -ExpandProperty ProcessId"
        result = subprocess.check_output(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script], text=True)
    else:
        result = subprocess.run(["ps", "--ppid", str(pid), "-o", "pid="], text=True, capture_output=True).stdout
    return {int(value) for value in result.split()}


def workers_alive(pids):
    if not pids:
        return set()
    if os.name == "nt":
        ids = ",".join(str(pid) for pid in sorted(pids))
        script = f"Get-Process -Id @({ids}) -ErrorAction SilentlyContinue | Where-Object ProcessName -EQ 'stillwatch-worker' | Select-Object -ExpandProperty Id"
        result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script], text=True, capture_output=True)
        return {int(value) for value in result.stdout.split()}
    result = subprocess.run(["ps", "-p", ",".join(map(str, sorted(pids))), "-o", "pid=,stat="], text=True, capture_output=True)
    return {int(pid) for pid, state in (row.split() for row in result.stdout.splitlines()) if not state.startswith("Z")}


class Client:
    def __init__(self, address, token):
        self.address = address
        self.token = token

    def call(self, method, path, body=None, *, key=None, revision=None, authorized=True, binary=False):
        headers = {"Accept": "image/jpeg" if binary else "application/json"}
        if authorized:
            headers["Authorization"] = f"Bearer {self.token}"
        if key is not None:
            headers["Idempotency-Key"] = key
        if revision is not None:
            headers["If-Match"] = f'"{revision}"'
        data = None
        if body is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(body).encode()
        request = urllib.request.Request(self.address + path, data, headers, method=method)
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(4 * 1024 * 1024 if binary else 512 * 1024)
            return response.status, raw if binary else json.loads(raw) if raw else None

    def camera(self, camera):
        status, result = self.call("GET", f"/api/v2/cameras/{camera}")
        assert status == 200, (status, result)
        return result

    def upload(self, body, *, content_type="application/octet-stream", token=True, length=None):
        headers = {"Content-Type": content_type}
        if token:
            headers["Authorization"] = f"Bearer {self.token}"
        if length is not None:
            headers["Content-Length"] = str(length)
        request = urllib.request.Request(self.address + "/api/v2/replay-uploads", body, headers, method="POST")
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            return response.status, json.loads(response.read(64 * 1024))


@contextlib.contextmanager
def run_core(core, worker, data, sources, logs, *extra):
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    endpoint = f"http://127.0.0.1:{port}"
    command = [str(core), "--listen", f"127.0.0.1:{port}", "--data-dir", str(data), "--replay-root", str(sources), "--worker", str(worker), *extra]
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with logs.open("wb") as log:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=flags)
        try:
            def ready():
                if process.poll() is not None:
                    raise RuntimeError(f"Core exited at startup with code {process.returncode}")
                try:
                    with urllib.request.urlopen(endpoint + "/health/live", timeout=0.5) as response:
                        return response.status == 200
                except (OSError, urllib.error.URLError):
                    return False

            until(ready)
            token = (data / "local-access-token").read_text().strip()
            yield process, Client(endpoint, token)
        except Exception:
            log.flush()
            print(logs.read_text(errors="replace"))
            raise
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def run(core, worker, ffmpeg):
    with TemporaryDirectory(prefix="stillwatch-e2e-") as temp:
        root = Path(temp)
        sources = root / "sources"
        sources.mkdir()
        for index in range(4):
            width = 160 + index * 16
            subprocess.run([str(ffmpeg), "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                            f"testsrc2=size={width}x120:rate=20:duration=24", "-c:v", "libx264", "-threads", "1",
                            str(sources / f"camera-{index}.mp4")], check=True)
        data = root / "data"
        source = {"type": "replay", "file": "camera-0.mp4", "pace": True}
        probe_path = "/api/v2/source-probes"
        with run_core(core, worker, data, sources, root / "core.log") as (process, client):
            assert client.call("GET", "/api/v2/cameras", authorized=False)[0] == 401
            assert client.call("GET", "/health/ready") == (200, {"state": "ready", "api": {"major": 2, "minor": 0}})
            assert client.upload(b"test", token=False)[0] == 401
            assert client.upload(b"test", content_type="application/json")[0] == 415
            assert client.upload(b"test", length=128 * 1024 * 1024 + 1)[0] == 413
            assert client.upload(b"test")[0] == 400
            assert not list((sources / ".stillwatch-uploads").iterdir())
            fixture = (sources / "camera-0.mp4").read_bytes()
            code, uploaded = client.upload(fixture)
            assert code == 201, uploaded
            assert uploaded["bytes"] == str(len(fixture))
            assert uploaded["source_probe"] == {
                "dimensions": {"width": 160, "height": 120}, "live": False, "native_pts": True, "codec": "h264",
            }
            assert (sources / uploaded["file"]).read_bytes() == fixture
            assert client.call("GET", "/bootstrap/info", authorized=False)[1]["camera_probe_capacity"]
            status, report = client.call("POST", probe_path, {"source": source})
            assert status == 200 and report["codec"] == "h264", (status, report)
            assert report["capture_capacity"] == {
                "fits": True, "available_camera_slots": 4,
                "available_raw_bytes": 128 * 1024 * 1024,
                "required_raw_bytes": 160 * 120 * 9,
            }

            def add(index):
                file = uploaded["file"] if index == 0 else f"camera-{index}.mp4"
                config = {"name": f"Replay {index}", "enabled": True, "source": {"type": "replay", "file": file, "pace": True}}
                for _ in range(25):
                    status, result = client.call("POST", "/api/v2/cameras", config, key=f"create-{index}")
                    if status == 201:
                        assert not result["live"]
                        return result["id"]
                    assert status == 429, (status, result)
                    time.sleep(0.15)
                raise AssertionError("probe capacity did not recover")

            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                ids = list(pool.map(add, range(4)))
            assert len(set(ids)) == 4

            def replaying():
                cameras = [client.camera(camera) for camera in ids]
                return cameras if all(c["runtime"]["state"] == "replaying" and int(c["runtime"]["frame"]) >= 5 for c in cameras) else None

            initial = until(replaying)
            assert [c["dimensions"]["width"] for c in initial] == [160, 176, 192, 208]
            assert {c["source_epoch"] for c in initial} == {"1"}
            assert all(c["runtime"]["dropped"] == "0" for c in initial)
            assert all(c["runtime"]["codec"] == "h264" for c in initial)

            def capacity(camera_id=None):
                request = {"source": source}
                if camera_id:
                    request["camera_id"] = camera_id
                status, report = client.call("POST", probe_path, request)
                assert status == 200, (status, report)
                return report["capture_capacity"]

            full = capacity()
            assert not full["fits"] and full["available_camera_slots"] == 0
            replacement = capacity(ids[0])
            assert replacement["fits"] and replacement["available_camera_slots"] == 1
            assert replacement["available_raw_bytes"] - full["available_raw_bytes"] == 160 * 120 * 9
            assert client.call("POST", probe_path, {"source": source, "camera_id": str(uuid.uuid4())})[0] == 404
            overflow = {"name": "Fifth", "enabled": True, "source": source}
            assert client.call("POST", "/api/v2/cameras", overflow, key="over-capacity")[1]["code"] == "capacity_exceeded"
            status, disabled = client.call("POST", "/api/v2/cameras", overflow | {"enabled": False}, key="disabled")
            assert status == 201, (status, disabled)
            assert capacity(disabled["id"]) == full
            rejected = source | {"file": "../unlisted.mp4"}
            assert client.call("POST", probe_path, {"source": rejected})[0] == 400

            path = f"/api/v2/cameras/{ids[0]}"
            assert client.call("PATCH", path, {"source": source | {"file": "missing.mp4"}}, key="missing", revision=1)[0] == 400
            assert client.camera(ids[0])["revision"] == "1"
            mask = [{"left": 0, "top": 0, "right": 5000, "bottom": 5000}]
            status, changed = client.call("PATCH", path, {"privacy_masks": mask}, key="privacy", revision=1)
            assert status == 200 and changed["revision"] == "2" and changed["scene_revision"] == "2"
            changed = until(lambda: (c if int((c := client.camera(ids[0]))["source_epoch"]) >= 2 and c["runtime"]["state"] == "replaying" else None))
            assert changed["config"]["privacy_masks"] == mask

            before_crash = until(replaying)
            children = until(lambda: pids if len((pids := capture_children(process.pid))) == 4 else None)
            victim = min(children)
            os.kill(victim, signal.SIGTERM)

            def reconnected():
                cameras = replaying()
                if not cameras:
                    return None
                changed_epochs = [after["source_epoch"] != before["source_epoch"] for before, after in zip(before_crash, cameras, strict=True)]
                return cameras if sum(changed_epochs) == 1 else None

            after_crash = until(reconnected)
            last_children = until(lambda: pids if len((pids := capture_children(process.pid))) == 4 and victim not in pids else None)
            instance = client.call("GET", "/bootstrap/info", authorized=False)[1]["instance_id"]
            process.terminate()
            process.wait(timeout=12)
            if os.name != "nt":
                assert process.returncode == 0
            until(lambda: not workers_alive(last_children))

        with run_core(core, worker, data, sources, root / "restart.log") as (_, client):
            assert client.call("GET", "/bootstrap/info", authorized=False)[1]["instance_id"] == instance
            restarted = until(replaying)
            assert all(int(after["source_epoch"]) > int(before["source_epoch"]) for before, after in zip(after_crash, restarted, strict=True))
            revision = restarted[0]["revision"]
            assert client.call("DELETE", f"/api/v2/cameras/{ids[0]}", key="delete", revision=revision)[0] == 204
            assert client.call("GET", f"/api/v2/cameras/{ids[0]}")[0] == 404
        with run_core(core, worker, root / "one-slot", sources, root / "one-slot.log",
                      "--active-cameras", "1") as (_, client):
            drafts = [overflow | {"name": f"Draft {index}"} for index in range(2)]
            for draft in drafts:
                status, report = client.call("POST", probe_path, {"source": draft["source"]})
                assert status == 200 and report["capture_capacity"]["fits"], (status, report)
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                saves = [
                    pool.submit(client.call, "POST", "/api/v2/cameras", draft, key=f"draft-{index}")
                    for index, draft in enumerate(drafts)
                ]
                results = [save.result() for save in saves]
            assert sorted(status for status, _ in results) == [201, 503], results
            assert next(report for status, report in results if status == 503)["code"] == "capacity_exceeded"
        with run_core(core, worker, root / "small-budget", sources, root / "small-budget.log",
                      "--raw-frame-bytes", "16384") as (_, client):
            status, report = client.call("POST", probe_path, {"source": source})
            assert status == 200 and report["codec"] == "h264", (status, report)
            assert report["capture_capacity"] == {
                "fits": False, "available_camera_slots": 4,
                "available_raw_bytes": 16384, "required_raw_bytes": 160 * 120 * 9,
            }
            status, report = client.call("POST", "/api/v2/cameras", overflow, key="raw-memory-cap")
            assert status == 503 and report["code"] == "capacity_exceeded", (status, report)
        print(json.dumps({"platform": os.name, "concurrent_replays": 4, "worker_restart_isolated": True,
                          "persisted_registry_restarted": True, "worker_cleanup": True,
                          "codec_and_capture_preflight": True, "capacity_and_revision_checks": True}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--ffmpeg", type=Path, required=True)
    options = parser.parse_args()
    run(options.core.resolve(strict=True), options.worker.resolve(strict=True), options.ffmpeg.resolve(strict=True))

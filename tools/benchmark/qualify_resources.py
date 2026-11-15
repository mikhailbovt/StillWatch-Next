"""Observe native process resources and Core-owned leases through churn and drain."""

from contextlib import closing
from pathlib import Path
import sqlite3
import sys
import time
from uuid import uuid4

import psutil

from fast_rc_contract import write_json
from qualify_steady import resources


class ChurnResources:
    def __init__(self, fixture, settings, root, receiver):
        self.fixture = fixture
        self.settings = settings
        self.data = root / "data"
        self.begin = time.monotonic()
        self.records = []
        self.processes = set()
        self.record("baseline")
        if self.records:
            lan, session, _, _ = receiver
            code, status = lan.request("GET", "/api/v2/status", token=session["access_token"])
            if code != 200 or "resources" in status:
                raise RuntimeError("Local resource diagnostics were exposed on the network listener")

    def status(self):
        code, status = self.fixture["local"].call("GET", "/api/v2/status")
        if code != 200:
            raise RuntimeError(f"Core resource status returned {code}")
        return status

    def measured_usage(self, leader):
        profile = self.fixture["profile"]
        usage = resources(leader, self.data, self.settings.get("cuda_device"), profile["host"]["accelerator"])
        threads = {}
        for pid, started in usage["pids"]:
            try:
                process = psutil.Process(pid)
                if process.create_time() == started:
                    threads[str(pid)] = process.num_threads()
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        usage["threads_by_pid"] = threads
        if sys.platform == "linux":
            try:
                rollup = Path(f"/proc/{leader}/smaps_rollup").read_text(encoding="ascii")
            except OSError:
                return usage
            fields = {"Anonymous", "Private_Clean", "Private_Dirty", "Pss"}
            pages = {}
            for line in rollup.splitlines():
                key, _, value = line.partition(":")
                if key in fields:
                    amount, unit = value.split()
                    if unit == "kB":
                        pages[key.lower() + "_bytes"] = int(amount) * 1024
            usage["core_smaps"] = pages
        return usage

    def record(self, kind, completed=()):
        status = self.status()
        if status.get("resources") is None:
            return
        process = self.fixture["process"]
        usage = self.measured_usage(process.pid)
        self.processes.update((pid, created) for pid, created in usage["pids"])
        self.records.append({"elapsed_s": time.monotonic() - self.begin, "kind": kind, "completed": list(completed),
                             "core": process.pid, "usage": usage, "leases": status["resources"]})

    def drain(self, output):
        if not self.records:
            return
        local = self.fixture["local"]
        for camera in self.fixture["cameras"].values():
            revision = local.camera(camera)["revision"]
            code, _ = local.call("PATCH", f"/api/v2/cameras/{camera}", {"enabled": False}, revision=revision,
                                 key=uuid4().hex)
            if code != 200:
                raise RuntimeError(f"Camera disable returned {code}")
        leader = self.fixture["process"].pid
        profile = self.fixture["profile"]
        began = time.monotonic()
        deadline = began + profile["budgets"]["rtsp_recovery_ms"] / 1_000
        while True:
            status = self.status()
            usage = self.measured_usage(leader)
            current = {tuple(record) for record in usage["pids"]}
            detached = []
            for pid, created in self.processes - current:
                try:
                    if psutil.Process(pid).create_time() == created:
                        detached.append(pid)
                except psutil.NoSuchProcess:
                    continue
            leases = status["resources"]
            writer = status["writer"]
            with closing(sqlite3.connect((self.data / "stillwatch.sqlite3").as_uri() + "?mode=ro", uri=True)) as database:
                cameras = database.execute("SELECT count(*), coalesce(sum(bytes_reserved), 0) FROM cameras WHERE enabled = 1 AND deleted = 0").fetchone()
                jobs, = database.execute(
                    "SELECT count(*) FROM semantic_jobs WHERE state IN ('queued','claimed','dispatched','response_ready')").fetchone()
                retained = {table: database.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
                            for table in ("artifacts", "incidents", "notification_receipts")}
            expected_children = profile["models"]["workers"] + 1
            if (usage["children"] == expected_children and not detached and leases["media"] is not None
                    and all(value == 0 for value in leases["media"].values())
                    and leases["capture_frame_bytes_in_use"] == leases["evidence_write_bytes_in_use"] == 0
                    and writer["queued"] == 0 and writer["running_ms"] is None and not any(cameras) and jobs == 0):
                break
            if time.monotonic() >= deadline:
                break
            time.sleep(0.2)
        write_json(output, {"samples": self.records,
                            "drain": {"elapsed_ms": (time.monotonic() - began) * 1_000,
                                      "usage": usage, "leases": leases, "writer": writer, "detached": sorted(detached),
                                      "enabled_cameras": cameras[0], "reserved_frame_bytes": cameras[1],
                                      "semantic_jobs": jobs, "retained": retained}})

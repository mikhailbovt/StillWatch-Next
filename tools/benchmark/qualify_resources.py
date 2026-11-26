"""Observe native process resources and Core-owned leases through churn and drain."""

from contextlib import closing
from pathlib import Path
import sqlite3
import sys
import time
from uuid import uuid4

import psutil

from fast_rc_contract import (
    CHURN_GROWTH_BUCKET_SECONDS, CORE_RESTART_RESOURCE_AGE_S, SHORT_PROTOCOL, native_clock, native_contract, write_json,
)
from process_identity import process_identity
from qualify_steady import latest, resources


class ChurnResources:
    def __init__(self, fixture, settings, root, receiver):
        self.fixture = fixture
        self.settings = settings
        self.data = root / "data"
        self.clock_name, self.now = native_clock()
        self.short = fixture["profile"].get("protocol") == SHORT_PROTOCOL
        self.restart = native_contract(SHORT_PROTOCOL)["short_resources"] if self.short else None
        self.begin = settings["monotonic_origin"] if self.short else self.now()
        self.monotonic_begin = time.monotonic()
        self.records = []
        self.processes = set()
        self.component_executables = {
            Path(settings[key]).stem.lower(): role
            for key, role in (("mediamtx", "media_router"), ("encoder", "media_encoder"))
            if settings.get(key)
        }
        worker = Path(settings["worker"]).stem.lower()
        self.worker_names = {worker, f"{worker}-native"}
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
        usage = resources(leader, self.data, self.settings.get("cuda_device"), profile["host"]["accelerator"],
                          tracked=self.processes)
        self.processes = {tuple(item) for item in usage["pids"]}
        threads = {}
        roles = {}
        for pid, started in usage["pids"]:
            try:
                process = psutil.Process(pid)
                if process_identity(process) == (pid, started):
                    threads[str(pid)] = process.num_threads()
                    roles[str(pid)] = self.process_role(process, leader)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        usage["threads_by_pid"] = threads
        usage["roles_by_pid"] = roles
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

    def process_role(self, process, leader):
        if process.pid == leader:
            return "core"
        executable = Path(process.exe()).stem.lower()
        if executable in self.worker_names:
            command = process.cmdline()[1:2]
            if command == ["capture"]:
                return "capture"
            if command and command[0] in ("infer", "infer-v2", "infer-v2-proposals"):
                return "inference"
        # Record only known roles. Paths and process arguments can contain credentials.
        return self.component_executables.get(executable, "other")

    def record(self, kind, completed=(), *, core_ready_s=None):
        status = self.status()
        if status.get("resources") is None:
            return
        process = self.fixture["process"]
        cameras = None
        if self.short and kind == "core_restart_settled":
            cameras = {name: latest(camera, self.fixture["local"], clock=self.now)
                       for name, camera in self.fixture["cameras"].items()}
        usage = self.measured_usage(process.pid)
        row = {"elapsed_s": self.now() - self.begin, "host_monotonic_elapsed_s": time.monotonic() - self.monotonic_begin,
               "kind": kind, "completed": list(completed), "core": process.pid, "usage": usage, "leases": status["resources"]}
        if core_ready_s is not None or (completed and kind in {"core_restart", "core_restart_settled"}):
            row["core_started_s"] = self.fixture["core_timing"]["started_at"] - self.begin
        if core_ready_s is not None:
            row["core_ready_s"] = core_ready_s
        if cameras is not None:
            row["camera_states"] = cameras
        self.records.append(row)
        return row

    def record_settled_restart(self, completed, *, core_ready_s=None):
        if self.short:
            deadline = self.begin + core_ready_s + self.restart["core_restart_sample_age_seconds"]
        else:
            deadline = self.fixture["core_timing"]["started_at"] + CORE_RESTART_RESOURCE_AGE_S
        while (remaining := deadline - self.now()) > 0:
            time.sleep(min(remaining, 0.2))
        return self.record("core_restart_settled", completed, core_ready_s=core_ready_s)

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
        began = self.now()
        deadline = began + profile["budgets"]["rtsp_recovery_ms"] / 1_000
        while True:
            status = self.status()
            usage = self.measured_usage(leader)
            detached = usage["detached"]
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
            if self.now() >= deadline:
                break
            time.sleep(0.2)
        write_json(output, {"elapsed_clock": self.clock_name, "sampling_started_monotonic_s": self.monotonic_begin,
                            **({"protocol": SHORT_PROTOCOL, "core_restart_sample_reference":
                                self.restart["core_restart_sample_reference"],
                                "core_restart_resource_age_s": self.restart["core_restart_sample_age_seconds"]}
                               if self.short else {"growth_bucket_seconds": CHURN_GROWTH_BUCKET_SECONDS,
                                                   "core_restart_resource_age_s": CORE_RESTART_RESOURCE_AGE_S}),
                            "samples": self.records, "drain": {"elapsed_ms": (self.now() - began) * 1_000,
                                      "usage": usage, "leases": leases, "writer": writer, "detached": detached,
                                      "enabled_cameras": cameras[0], "reserved_frame_bytes": cameras[1],
                                      "semantic_jobs": jobs, "retained": retained}})

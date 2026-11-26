"""Regression tests for native short-protocol evidence and contract selection."""

import unittest

from fast_rc_contract import (
    PROTOCOL, SHORT_PROTOCOL, contract_digests, fault_batches, native_contract, short_fault_schedule,
)
from validate_report import short_restart_resources
from validate_steady import Steady


def steady_trace():
    run = Steady.__new__(Steady)
    run.short = True
    run.clock = "QueryPerformanceCounter"
    run.limits = native_contract(SHORT_PROTOCOL)["quiet_load"]
    run.cameras = ["one", "two", "three", "four"]
    run.start = {"protocol": SHORT_PROTOCOL, "elapsed_clock": run.clock, "warmup_elapsed_s": 10,
                 "cameras": dict.fromkeys(run.cameras), "viewer_frames": {"0": 3}}
    run.end = {"protocol": SHORT_PROTOCOL, "elapsed_clock": run.clock, "elapsed_s": 720.4,
               "viewer_renewals": 3, "credential_rotations": 1, "decoded_snapshots_by_rotation": {"0": 8, "1": 4},
               "renewal_evidence": [{"tick": tick, "elapsed_s": tick + 0.05, "frames_before": {"0": tick + 2}}
                                    for tick in (180, 360, 540)]}
    run.workload = {"viewer_sessions": 1, "snapshot_requests_per_minute": 1, "evidence_fps": 1}
    run.sources = {"sources": [], "viewers": []}
    run.model = {"backend": "cpu_fp32"}
    run.budgets = {"rss_bytes": 1000, "disk_bytes": 1000, "viewer_recovery_ms": 2000,
                   "headroom_bytes": {"rss": 100, "disk": 100},
                   "max_children": 10, "max_os_handles": 100, "max_open_sockets": 100,
                   "short_growth_bytes": {"steady": {"rss_bytes": 40}}}
    run.observations = {"isolated": True, "exit_code": 0}
    run.rows = [
        {"tick": tick, "elapsed_s": tick + 0.1, "cameras": dict.fromkeys(run.cameras),
         "resources": {"rss_bytes": 400 + (20 if tick >= 660 else 0), "disk_bytes": 300,
                       "children": 4, "os_handles": 25, "open_sockets": 15} if tick % 5 == 0 else None,
         "viewer_frames": {"0": tick + 3}, "credential_rotations": int(tick >= 510),
         "snapshots": tick // 60, "snapshots_ok": tick // 60}
        for tick in range(721)
    ]
    return run


class ShortContractTests(unittest.TestCase):
    def test_protocol_digests_and_evidence_inventory(self):
        old = native_contract(PROTOCOL)
        short = native_contract(SHORT_PROTOCOL)
        self.assertEqual(old["cases"], short["cases"])
        self.assertEqual(sum(map(len, short["cases"].values())), 32)
        self.assertEqual(old["quiet_load"]["minimum_seconds"], 3600)
        self.assertEqual(short["quiet_load"]["minimum_seconds"], 720)
        before, after = contract_digests(), contract_digests(SHORT_PROTOCOL)
        self.assertEqual(before.keys(), after.keys())
        self.assertEqual({key for key in before if before[key] != after[key]}, {"native"})
        with self.assertRaisesRegex(ValueError, "Unknown Fast RC protocol"):
            native_contract("stillwatch-short-resource-component-v1")

    def test_short_rtsp_schedule_needs_each_camera_and_separate_batches(self):
        short = native_contract(SHORT_PROTOCOL)
        names = [f"cam-{index}" for index in range(16)]
        schedule = []
        for batch in range(6):
            for camera in names[8 * (batch % 2):8 * (batch % 2 + 1)]:
                schedule.append({"id": f"rtsp-{batch}-{camera}", "kind": "rtsp_reconnect",
                                 "camera": camera, "batch": batch})
        for kind, count in short["minimum_completed_fault_cycles"].items():
            if kind != "rtsp_reconnect":
                schedule.extend({"id": f"{kind}-{index}", "kind": kind, "camera": names[0]} for index in range(count))
        cameras = set(names)
        self.assertEqual(short_fault_schedule(short, list(fault_batches(schedule)), cameras)["minimum_each"], 3)
        for label, invalid in (
            ("missing camera coverage", [row | {"camera": names[1]} if index == 0 else row
                                         for index, row in enumerate(schedule)]),
            ("three batches", [row | {"batch": row["batch"] // 2} if "batch" in row else row for row in schedule]),
        ):
            with self.subTest(label=label), self.assertRaises(ValueError):
                short_fault_schedule(short, list(fault_batches(invalid)), cameras)

    def test_short_peak_includes_terminal_sample_and_requires_every_scheduled_sample(self):
        trace = steady_trace()
        self.assertEqual(trace.interval()[2], "PASS")
        _, observed, verdict = trace.resources()
        self.assertEqual(verdict, "PASS")
        self.assertEqual(observed["sample_count"], 145)
        self.assertEqual(observed["growth_bytes"], {"rss_bytes": 20})
        trace.rows[5]["resources"] = None
        self.assertEqual(trace.resources()[2], "INCOMPLETE")
        trace.rows[-1]["resources"]["rss_bytes"] = 901
        self.assertEqual(trace.resources()[2], "FAIL")

    def test_short_interval_rejects_late_ticks_and_incomplete_camera_inventory(self):
        for reason in ("late tick", "partial interval", "missing camera"):
            with self.subTest(reason=reason):
                trace = steady_trace()
                if reason == "late tick":
                    trace.rows[180]["elapsed_s"] = 182.01
                elif reason == "partial interval":
                    trace.rows = trace.rows[:21]
                    trace.end["elapsed_s"] = 20.1
                else:
                    trace.rows[50]["cameras"].pop("three")
                self.assertEqual(trace.interval()[2], "INCOMPLETE")

    def test_short_requires_decode_after_each_renewal_and_authenticated_jpegs_after_rotation(self):
        trace = steady_trace()
        self.assertEqual(trace.media()[2], "PASS")
        trace.end["decoded_snapshots_by_rotation"] = {"0": 12}
        self.assertEqual(trace.media()[2], "INCOMPLETE")
        trace.end["decoded_snapshots_by_rotation"] = {"0": 8, "1": 4}
        trace.end["renewal_evidence"].pop(1)
        self.assertEqual(trace.media()[2], "INCOMPLETE")

    def test_restart_growth_binds_ready_age_pid_identity_and_camera_epochs(self):
        def camera(epoch, count):
            return {"epoch": str(epoch), "frame": str(count), "capture_state": "streaming",
                    "analysis": {"state": "ready", "source_epoch": str(epoch),
                                 "pose_frames": str(count), "fire_frames": str(count)}}

        rules = native_contract(SHORT_PROTOCOL)["short_resources"]
        ready = {"front": camera(2, 3), "side": camera(2, 2)}
        recovered = {"completed": ["restart"], "core": 101, "core_started_s": 31, "core_ready_s": 34,
                     "elapsed_s": 34.2, "usage": {"pids": [[101, 1600.0]]}}
        settled = recovered | {"elapsed_s": 54.3, "camera_states": {"front": camera(2, 103), "side": camera(2, 102)}}
        ledger = {"started_s": 30, "finished_s": 38, "fault": {"replacement_core_pid": 101}}
        binding = {"restart": (ledger, {"ready": ready, "ready_s": 34})}
        self.assertTrue(short_restart_resources([recovered], [settled], binding, rules))
        for row in (settled | {"elapsed_s": 51.1}, settled | {"usage": {"pids": [[101, 1601.0]]}},
                    settled | {"camera_states": {"front": camera(2, 103), "side": camera(3, 2)}}):
            with self.subTest(row=row):
                self.assertFalse(short_restart_resources([recovered], [row], binding, rules))


if __name__ == "__main__":
    unittest.main()

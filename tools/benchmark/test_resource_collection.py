import json
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
from threading import Timer
import unittest
from unittest.mock import patch

import psutil

from evaluate import replay_directory
from fast_rc_contract import sha256, write_json
from process_identity import process_identity
from qualify_faults import batch_spacing
from qualify_steady import resources, snapshot_problem
from validate_steady import Steady
from validate_report import FAULTS, Validation, churn_growth_blocks, comparable_restart_resources


class ResourceCollection(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "win32", "requires Windows file sharing")
    def test_replay_cleanup_waits_for_an_inherited_log_to_close(self):
        with replay_directory() as workspace:
            log = workspace / "core.log"
            handle = log.open("wb")
            self.addCleanup(handle.close)
            with self.assertRaises(PermissionError) as error:
                log.unlink()
            self.assertEqual(error.exception.winerror, 32)
            closer = Timer(0.15, handle.close)
            closer.start()
            self.addCleanup(closer.join)
        self.assertFalse(workspace.exists())

    def test_restart_growth_matches_the_same_core_at_a_fixed_age(self):
        recovered = [{"completed": ["restart-1"], "core": 101, "core_started_s": 30, "elapsed_s": 35},
                     {"completed": ["restart-2"], "core": 102, "core_started_s": 60, "elapsed_s": 64}]
        settled = [recovered[0] | {"elapsed_s": 50.1}, recovered[1] | {"elapsed_s": 80.4}]
        self.assertTrue(comparable_restart_resources(recovered, settled, 20))

        for changed in ({"completed": ["restart-2"]}, {"core": 102}, {"core_started_s": 31},
                        {"elapsed_s": 50.6}, {"elapsed_s": 34}):
            with self.subTest(changed=changed):
                self.assertFalse(comparable_restart_resources(recovered, [settled[0] | changed, settled[1]], 20))
        self.assertFalse(comparable_restart_resources(recovered, [settled[0]], 20))
        self.assertFalse(comparable_restart_resources(recovered, [settled[0], settled[0]], 20))
        self.assertFalse(comparable_restart_resources(
            [recovered[0], recovered[1] | {"core_started_s": 50}],
            [settled[0], settled[1] | {"core_started_s": 50, "elapsed_s": 70}], 20))

    def test_both_restart_phases_keep_the_hard_peak_even_without_growth_evidence(self):
        validator = Validation.__new__(Validation)
        validator.run = {}
        validator.profile = {
            "models": {"workers": 1, "backend": "cpu_fp32"},
            "budgets": {"rss_bytes": 1000, "disk_bytes": 1000, "headroom_bytes": {"rss": 200, "disk": 0},
                        "max_children": 2, "max_os_handles": 100, "max_open_sockets": 100,
                        "rtsp_recovery_ms": 1000, "growth_horizon_seconds": 86400},
        }
        validator.contract = {"minimum_completed_fault_cycles": dict.fromkeys(FAULTS.values(), 1)}
        usage = {"rss_bytes": 750, "disk_bytes": 500, "children": 2, "os_handles": 20, "open_sockets": 10}
        recovery = {"elapsed_s": 10, "core_started_s": 5, "completed": ["restart"], "core": 101, "usage": usage}
        rows = [{"kind": "baseline", "elapsed_s": 0, "usage": usage},
                recovery | {"kind": "core_restart"},
                recovery | {"kind": "core_restart_settled", "elapsed_s": 25.6}]
        drain = {"usage": usage, "detached": [], "elapsed_ms": 10, "leases": {"media": {},
                 "capture_frame_bytes_in_use": 0, "evidence_write_bytes_in_use": 0},
                 "enabled_cameras": 0, "reserved_frame_bytes": 0, "semantic_jobs": 0,
                 "writer": {"queued": 0, "running_ms": None}}
        with TemporaryDirectory() as directory:
            trace = Path(directory) / "churn.json"
            with (patch.object(validator, "artifact", return_value=trace) as artifact,
                  patch.object(validator, "read_case", return_value=(None, [], "NOT_RUN"))):
                write_json(trace, {"core_restart_resource_age_s": 20, "samples": rows, "drain": drain})
                self.assertEqual(validator.churn_check({})[2], "INCOMPLETE")
                for index in (1, 2):
                    with self.subTest(index=index):
                        samples = rows.copy()
                        samples[index] = samples[index] | {"usage": usage | {"rss_bytes": 801}}
                        path = trace.with_name(f"peak-{index}.json")
                        write_json(path, {"core_restart_resource_age_s": 20, "samples": samples, "drain": drain})
                        artifact.return_value = path
                        _, observed, result = validator.churn_check({})
                        self.assertEqual(result, "FAIL")
                        self.assertEqual(observed["failures"], ["rss_bytes"])
                        self.assertNotIn("core_restart/rss_bytes", observed["growth_upper_bytes"])

    def test_churn_growth_requires_complete_separate_minutes(self):
        def records(times):
            return [{"elapsed_s": value, "usage": {"rss_bytes": index}}
                    for index, value in enumerate(times)]

        short = records([100, 109, 119, 125, 136.53])
        self.assertEqual(churn_growth_blocks(short, "rss_bytes", 60), {})
        full = records([100, 125, 160, 190, 220, 245, 280, 310])
        self.assertEqual(churn_growth_blocks(full, "rss_bytes", 60),
                         {0.5: [0, 1], 1.5: [2, 3], 2.5: [4, 5]})

    def test_full_restart_batch_window_allows_separate_growth_samples(self):
        planned = [[{"kind": "core_restart"}] for _ in range(10)]
        limits = {"core_restart": 10}
        self.assertEqual(batch_spacing(planned, limits), {"core_restart": 1200 / 9})
        self.assertEqual(batch_spacing(planned[:9], limits), {})

    def test_full_rtsp_schedule_spans_fifteen_minutes_for_rss_measurement(self):
        planned = [[{"kind": "rtsp_reconnect"} for _ in range(4)] for _ in range(50)]
        limits = {"rtsp_reconnect": 200}
        self.assertEqual(batch_spacing(planned, limits), {"rtsp_reconnect": 900 / 49})
        self.assertEqual(batch_spacing(planned[:49], limits), {})

    @unittest.skipUnless(sys.platform == "linux", "Linux /proc start clock")
    def test_process_start_identity_survives_a_wall_clock_offset_change(self):
        pid = psutil.Process().pid
        with patch.object(psutil.Process, "create_time", return_value=1_794_804_151.28):
            before = process_identity(psutil.Process(pid))
        with patch.object(psutil.Process, "create_time", return_value=1_794_804_148.28):
            after = process_identity(psutil.Process(pid))
        self.assertEqual(before, after)
        self.assertEqual(before[0], pid)
        self.assertIsInstance(before[1], int)

    def test_churn_counts_a_surviving_worker_outside_the_process_tree(self):
        command = [sys.executable, "-B", "-c", "import time; print('ready', flush=True); time.sleep(10)"]
        with (TemporaryDirectory() as directory, subprocess.Popen(command, stdout=subprocess.PIPE) as leader,
              subprocess.Popen(command, stdout=subprocess.PIPE) as worker):
            try:
                self.assertEqual(leader.stdout.readline().strip(), b"ready")
                self.assertEqual(worker.stdout.readline().strip(), b"ready")
                data = Path(directory)
                outside = psutil.Process(worker.pid)
                identity = process_identity(outside)
                detached = sorted([outside.pid, *(child.pid for child in outside.children(recursive=True))])
                baseline = resources(leader.pid, data)
                tree = resources(leader.pid, data, tracked={identity})
                self.assertEqual(tree["detached"], detached)
                self.assertEqual(tree["children"], baseline["children"] + len(detached))
                self.assertEqual(tree["rss_bytes"], sum(tree["rss_by_pid"].values()))
                self.assertIn(str(worker.pid), tree["rss_by_pid"])
                if sys.platform == "win32":
                    private = tree["private_bytes_by_pid"]
                    self.assertEqual(private.keys(), tree["rss_by_pid"].keys())
                    self.assertGreater(private[str(worker.pid)], 0)
                reused_identity = (worker.pid, identity[1] - 1)
                tree = resources(leader.pid, data, tracked={reused_identity})
                self.assertEqual(tree["detached"], [])
                self.assertEqual(tree["children"], baseline["children"])
            finally:
                worker.terminate()
                leader.terminate()


class SnapshotCollection(unittest.TestCase):
    def test_interrupted_interval_preserves_the_observed_http_failure(self):
        with TemporaryDirectory() as directory:
            root = Path(directory)
            files = {k: root / (k + ".json") for k in ("sources", "start", "samples", "snapshot-failures")}
            write_json(files["sources"], {"sources": [{"id": "camera"}], "viewers": []})
            write_json(files["start"], {"cameras": {"camera": "id"}, "viewer_frames": {"0": 1}})
            row = {"tick": 0, "elapsed_s": 0.1, "viewer_frames": {"0": 2}, "snapshots": 0, "snapshots_ok": 0}
            files["samples"].write_text(json.dumps(row) + "\n", encoding="utf-8")
            failure = {"request": 1, "camera": "id", "status": 429, "reason": "http_status", "elapsed_s": 0.2}
            write_json(files["snapshot-failures"], {"requests": 1, "decoded": 0, "failures": [failure]})
            workload = {"camera_count": 1, "viewer_sessions": 1, "snapshot_requests_per_minute": 8,
                        "evidence_fps": 1, "source_manifest_digest": sha256(files["sources"])}
            profile = {"workload": workload, "models": {}, "budgets": {"viewer_recovery_ms": 1000}, "host": {"os": "linux"}}
            run = Steady(files.get, profile, {"quiet_load": {"minimum_seconds": 3600}},
                         {"observations": {"isolated": True, "exit_code": 1}})
            _, observed, verdict = run.media()
            self.assertEqual(verdict, "FAIL")
            self.assertEqual(observed["snapshot_failures"], [failure])
            self.assertEqual(run.interval()[2], "INCOMPLETE")

    def test_distinguishes_http_failure_from_stale_or_invalid_jpeg(self):
        cache = {"cache-control": "no-store"}
        self.assertEqual(snapshot_problem(429, {}, b""), "http_status")
        self.assertEqual(snapshot_problem(200, {}, b""), "cache_policy")
        self.assertEqual(snapshot_problem(200, cache, b""), "frame_age_header")
        self.assertEqual(snapshot_problem(200, cache | {"x-frame-age-ms": "3001"}, b""), "frame_age")
        self.assertEqual(snapshot_problem(200, cache | {"x-frame-age-ms": "3000"}, b"invalid"), "jpeg_decode")


if __name__ == "__main__":
    unittest.main()

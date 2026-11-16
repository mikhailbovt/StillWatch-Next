from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import psutil

from process_identity import process_identity
from qualify_steady import resources, snapshot_problem


class ResourceCollection(unittest.TestCase):
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
                reused_identity = (worker.pid, identity[1] - 1)
                tree = resources(leader.pid, data, tracked={reused_identity})
                self.assertEqual(tree["detached"], [])
                self.assertEqual(tree["children"], baseline["children"])
            finally:
                worker.terminate()
                leader.terminate()


class SnapshotCollection(unittest.TestCase):
    def test_distinguishes_http_failure_from_stale_or_invalid_jpeg(self):
        cache = {"cache-control": "no-store"}
        self.assertEqual(snapshot_problem(429, {}, b""), "http_status")
        self.assertEqual(snapshot_problem(200, {}, b""), "cache_policy")
        self.assertEqual(snapshot_problem(200, cache, b""), "frame_age_header")
        self.assertEqual(snapshot_problem(200, cache | {"x-frame-age-ms": "3001"}, b""), "frame_age")
        self.assertEqual(snapshot_problem(200, cache | {"x-frame-age-ms": "3000"}, b"invalid"), "jpeg_decode")


if __name__ == "__main__":
    unittest.main()

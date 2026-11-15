from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest

import psutil

from qualify_steady import resources, snapshot_problem


class ResourceCollection(unittest.TestCase):
    def test_churn_counts_a_surviving_worker_outside_the_process_tree(self):
        command = [sys.executable, "-B", "-c", "import time; print('ready', flush=True); time.sleep(10)"]
        with (TemporaryDirectory() as directory, subprocess.Popen(command, stdout=subprocess.PIPE) as leader,
              subprocess.Popen(command, stdout=subprocess.PIPE) as worker):
            try:
                self.assertEqual(leader.stdout.readline().strip(), b"ready")
                self.assertEqual(worker.stdout.readline().strip(), b"ready")
                data = Path(directory)
                outside = psutil.Process(worker.pid)
                identity = (outside.pid, outside.create_time())
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

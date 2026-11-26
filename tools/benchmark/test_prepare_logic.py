from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from fast_rc import Run, harness_digests
from fast_rc_contract import checked_json, execution_host_matches, sha256
from prepare_logic import archive_programs


class LogicalTestArchive(unittest.TestCase):
    def test_later_build_cannot_replace_archived_executable(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "sw_engine-cargo-name.exe"
            executable.write_bytes(b"first build")
            pinned = {"path": str(executable), "digest": sha256(executable)}

            output = root / "logical.json"
            archived = archive_programs(output, {"sw-engine": pinned})["sw-engine"]
            executable.write_bytes(b"later build")

            self.assertEqual(sha256(Path(archived["path"])), pinned["digest"])
            self.assertEqual(archive_programs(output, {"sw-engine": pinned})["sw-engine"], archived)
            with self.assertRaisesRegex(ValueError, "changed while archiving sw-engine"):
                archive_programs(root / "other.json", {"sw-engine": pinned})


class QualificationSourceDigests(unittest.TestCase):
    def test_changed_source_is_recorded_in_the_sealed_run(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            module = root / "tools/benchmark/qualify.py"
            module.parent.mkdir(parents=True)
            module.write_bytes(b"first collector")
            with patch("fast_rc.ROOT", root), patch("fast_rc.validate", return_value=2):
                run = Run.__new__(Run)
                run.root = root
                run.started = 0.0
                run.now = lambda: 10.0
                run.run = {"harness": {"source_digests": harness_digests()}, "stage_errors": [],
                           "interrupted": False}
                module.write_bytes(b"changed collector")
                run.seal()
            sealed = checked_json(root / "run-manifest.json")
            self.assertEqual(sealed["stage_errors"], ["Qualification source changed during the run: tools/benchmark/qualify.py"])
            self.assertNotEqual(sealed["harness"]["source_digests"]["tools/benchmark/qualify.py"], sha256(module))


class QualificationHostFingerprint(unittest.TestCase):
    def test_eight_kib_ram_drift_fails_the_frozen_fingerprint(self):
        actual = {"platform": "linux", "architecture": "x86_64", "os_release": "test-kernel",
                  "cpu": "test-cpu", "logical_cores": 24, "core_runtime_threads": 4,
                  "ram_bytes": 33_319_698_432, "accelerator": None}
        locked = {key: value for key, value in actual.items() if key != "platform"}
        locked["os"] = "linux"

        self.assertTrue(execution_host_matches(actual, locked, actual))
        increased = actual | {"ram_bytes": actual["ram_bytes"] + 8192}
        self.assertFalse(execution_host_matches(increased, locked, actual))
        self.assertFalse(execution_host_matches(increased, locked))

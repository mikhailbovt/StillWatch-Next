from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from fast_rc import Run, harness_digests
from fast_rc_contract import checked_json, sha256
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
                run = SimpleNamespace(root=root, elapsed=lambda: 10,
                                      run={"harness": {"source_digests": harness_digests()}, "stage_errors": []})
                module.write_bytes(b"changed collector")
                Run.seal(run)
            sealed = checked_json(root / "run-manifest.json")
            self.assertEqual(sealed["stage_errors"], ["Qualification source changed during the run: tools/benchmark/qualify.py"])
            self.assertNotEqual(sealed["harness"]["source_digests"]["tools/benchmark/qualify.py"], sha256(module))

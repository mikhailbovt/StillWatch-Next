"""Interrupted native runs preserve the last complete, nonpassing manifest."""

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from fast_rc import Run
from fast_rc_contract import PROTOCOL, checked_evidence, checked_json


class RunManifestTests(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.run = Run.__new__(Run)
        self.run.root = Path(directory.name)
        (self.run.root / "raw").mkdir()
        self.path = self.run.root / "run-manifest.json"
        self.run.started = 1.0
        self.run.now = lambda: 2.0
        self.run.run = {"protocol": PROTOCOL, "run_id": "manifest-fixture", "interrupted": False,
                        "candidate_digest": "sha256:" + "a" * 64,
                        "profile_digest": "sha256:" + "b" * 64, "cases": {}}

    def test_completed_proof_is_visible_before_final_seal(self):
        case = "artifacts.exact-digests-and-release-features"
        self.run.persist_manifest(interrupted=True)
        self.assertEqual(checked_json(self.path)["cases"], {})
        self.run.proof("preflight", case, 0.5, {}, {})
        partial = checked_json(self.path)
        self.assertTrue(partial["interrupted"])
        self.assertFalse(self.run.run["interrupted"])
        proof = checked_json(checked_evidence(self.run.root, partial["cases"][case]["proof"]))
        self.assertEqual(proof["run_id"], partial["run_id"])
        self.assertLessEqual(proof["finished_s"], partial["native_wall_seconds"])

        self.run.persist_manifest(interrupted=False)
        finished = checked_json(self.path)
        self.assertFalse(finished["interrupted"])
        self.assertEqual(finished["cases"], partial["cases"])

    def test_interrupted_write_preserves_the_previous_manifest(self):
        self.run.persist_manifest(interrupted=True)
        before = self.path.read_bytes()

        def write_partial(path, _):
            path.write_text("{", encoding="utf-8")
            raise KeyboardInterrupt

        with patch("fast_rc.write_json", write_partial), self.assertRaises(KeyboardInterrupt):
            self.run.persist_manifest(interrupted=False)

        self.assertEqual(self.path.read_bytes(), before)
        self.run.persist_manifest(interrupted=False)
        self.assertFalse(checked_json(self.path)["interrupted"])


if __name__ == "__main__":
    unittest.main()

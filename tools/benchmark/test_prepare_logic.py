from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from fast_rc_contract import sha256
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

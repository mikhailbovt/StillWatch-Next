"""Check evidence chronology and native disk-full test isolation."""

import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from qualify_storage import exhausted_windows_volume


class EvidenceTimes(unittest.TestCase):
    def test_utc_order_uses_the_instant_across_fraction_and_offset_formats(self):
        from analysis_core import verify_evidence_times

        packet = {"observed_host_utc": "2026-05-01T12:00:00Z", "persisted_at": "2026-05-01T12:00:00.009Z",
                  "expires_at": "2026-05-01T12:00:00.01Z"}
        incident = {"opening_observed_host_utc": "2026-05-01T14:00:00.000+02:00",
                    "created_at": "2026-05-01T12:00:00Z"}
        verify_evidence_times(packet, incident)
        with self.assertRaises(AssertionError):
            verify_evidence_times(packet | {"observed_host_utc": "2026-05-01T12:00:00.010Z"}, incident)
        with self.assertRaises(AssertionError):
            verify_evidence_times(packet | {"persisted_at": packet["expires_at"]}, incident)
        with self.assertRaises(AssertionError):
            verify_evidence_times(packet, incident | {"created_at": "2026-05-01T11:59:59.999Z"})


@unittest.skipUnless(os.name == "nt", "Windows drive roots and junctions")
class StorageVolumeTests(unittest.TestCase):
    def test_rejects_a_directory_inside_a_drive(self):
        with TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ValueError, "drive root"):
                with exhausted_windows_volume(root / "blobs", root):
                    self.fail("The collector used a directory on the runner volume")
            self.assertEqual(list(root.iterdir()), [])

    def test_rejects_the_same_drive_as_sqlite(self):
        with TemporaryDirectory() as temp:
            root = Path(temp)
            with self.assertRaisesRegex(ValueError, "dedicated bounded volume"):
                with exhausted_windows_volume(root / "blobs", Path(root.anchor)):
                    self.fail("The collector used the SQLite volume")
            self.assertEqual(list(root.iterdir()), [])

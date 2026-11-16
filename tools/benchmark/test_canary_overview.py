"""Require the native receipt to be accompanied by its actual evidence image."""

import hashlib
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock, patch

from fast_rc_contract import ROOT, evidence
from qualify_faults import read_canary_overview
from validate_report import canary_overview_matches


class CanaryOverview(unittest.TestCase):
    def test_collector_requires_matching_packet_and_decoded_bytes(self):
        jpeg = (ROOT / "tests/fixtures/evidence/black.jpg").read_bytes()
        image = {"kind": "overview", "availability": "available", "source_s": 1,
                 "ordinal": 2, "sha256": hashlib.sha256(jpeg).hexdigest()}
        incident = {"id": "new-opening", "evidence_packet_id": "new-packet",
                    "camera_id": "camera", "source_epoch": "2", "evidence_version": "4"}
        packet = {"incident_id": incident["id"], "packet_id": incident["evidence_packet_id"],
                  "camera_id": incident["camera_id"], "source_epoch": incident["source_epoch"],
                  "evidence_version": incident["evidence_version"], "images": [image]}
        lan = Mock()
        lan.request.return_value = 200, packet
        headers = {"Content-Type": "image/jpeg"}
        with patch("qualify_faults.request", return_value=(200, headers, jpeg)) as download:
            metadata, body = read_canary_overview(lan, "pair-token", incident)
            self.assertEqual(body, jpeg)
            self.assertEqual(metadata["sha256"], image["sha256"])
            download.assert_called_once_with(lan, "pair-token", "GET", "/api/v2/incidents/new-opening/evidence/new-packet/2")
            download.return_value = 200, headers, jpeg[:-2]
            self.assertIsNone(read_canary_overview(lan, "pair-token", incident))
            bad = b"unreadable"
            image["sha256"] = hashlib.sha256(bad).hexdigest()
            download.return_value = 200, headers, bad
            self.assertIsNone(read_canary_overview(lan, "pair-token", incident))
            download.reset_mock()
            packet["source_epoch"] = "1"
            self.assertIsNone(read_canary_overview(lan, "pair-token", incident))
            download.assert_not_called()

    def test_validator_reopens_the_exact_overview_bytes(self):
        jpeg = (ROOT / "tests/fixtures/evidence/black.jpg").read_bytes()
        incident = {"id": "new-opening", "source_epoch": "2"}
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "canary.json"
            image = path.with_suffix(".jpg")
            image.write_bytes(jpeg)
            raw = {"overview": {"incident_id": incident["id"], "source_epoch": "2",
                                "packet_id": "00000000-0000-0000-0000-000000000002", "ordinal": 2,
                                "bytes": len(jpeg), "sha256": hashlib.sha256(jpeg).hexdigest(),
                                "artifact": evidence(root, image)}}
            self.assertTrue(canary_overview_matches(path, raw, incident))
            self.assertFalse(canary_overview_matches(path, raw, dict(incident, id="old-opening")))
            self.assertFalse(canary_overview_matches(path, {}, incident))
            image.write_bytes(jpeg[:-3] + bytes([jpeg[-3] ^ 1]) + jpeg[-2:])
            self.assertFalse(canary_overview_matches(path, raw, incident))


if __name__ == "__main__":
    unittest.main()

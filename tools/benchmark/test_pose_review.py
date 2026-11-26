"""Regression coverage for native review input and tracked Pose provenance."""

import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from pose_review import export_pose_review
from slot_coverage import read_receipts


class PoseReviewTests(unittest.TestCase):
    def test_frozen_accepted_slot_becomes_review_input_without_backdating_history(self):
        model = {"profile": "cpu_fp32_rect_640", "sha256": "a" * 64,
                 "provider": "CPUExecutionProvider", "runtime_sha256": "b" * 64, "cuda": None}
        header = {"type": "header", "version": 2, "pose_balanced": model, "model_manifest_sha256": "c" * 64}
        frame = {"camera_id": "last", "epoch": 3, "scene_revision": 1, "generation": "current",
                 "ordinal": 7, "source_us": 1_200_033, "task": "pose"}
        task = frame | {"type": "task", "sequence": 1, "slot": 2, "outcome": "accepted"}
        person = {"track_id": 1, "box": dict(x1=1, y1=2, x2=3, y2=4),
                  "keypoints": [[0, 1, 0.9]] * 17, "association_uncertain": True, "limited": False}
        pose = {"width": 640, "height": 480, "long_gap_reset": True, "confirmed": [person],
                "tentative_history": [[person | {"track_id": 8}, person]]}
        policy = frame | {"type": "policy", "sequence": 2, "details": {"pose_observations": pose}}
        with TemporaryDirectory() as folder:
            trace = Path(folder) / "analysis.jsonl"
            trace.write_text("".join(json.dumps(row) + "\n" for row in (header, task, policy)), encoding="utf-8")
            frames = {}
            _, _, count = read_receipts(trace, header["model_manifest_sha256"], pose_frames=frames)
            self.assertEqual(count, 2)
            clip = {"file": "recording.mp4", "sha256": "d" * 64}
            output = trace.with_name("poses.jsonl")
            origins = {"3": (1_000_000, (640, 480))}
            export_pose_review(trace, {"rows": count}, frames["last"], origins, clip, output)
            row = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual((row["frame_index_zero_based"], row["source_seconds"], row["slot"]), (6, 0.200033, 2))
            self.assertEqual([item["detection_id"] for item in row["detections"]], [1])
            self.assertTrue(row["detections"][0]["association_uncertain"] and row["long_gap_reset"])
            proof = json.loads(output.with_suffix(".receipt.json").read_text(encoding="utf-8"))
            self.assertEqual((proof["pose_profile"], proof["pose_model_sha256"]), ("rect640", model["sha256"]))
            self.assertFalse(proof["physical_identity_reviewed"])
            missing = trace.with_name("missing.jsonl")
            self.assertEqual(export_pose_review(trace, {"rows": count}, [], origins, clip, missing),
                             {"status": "unavailable", "reason": "no_accepted_pose"})
            self.assertFalse(missing.exists())
            changed_scope = task | {"generation": "new"}
            with self.assertRaisesRegex(ValueError, "single capture scope"):
                export_pose_review(trace, {"rows": count}, frames["last"] + [(changed_scope, pose)],
                                   origins, clip, trace.with_name("invalid.jsonl"))


if __name__ == "__main__":
    unittest.main()

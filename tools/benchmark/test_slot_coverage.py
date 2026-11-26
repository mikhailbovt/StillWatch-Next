import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from event_scoring import covered_window
from slot_coverage import read_receipts, slot_coverage


def base_receipts(timestamps):
    origin = timestamps[0]
    return [{"epoch": 1, "scene_revision": 1, "generation": "g", "outcome": "accepted",
             "source_us": pts, "ordinal": index, "slot": (pts - origin) * 5 // 1_000_000}
            for index, pts in enumerate(timestamps)]


class SlotCoverageTests(unittest.TestCase):
    def test_irregular_upfall_timestamps_keep_the_occupied_slots(self):
        root = Path(__file__).resolve().parents[2]
        manifest = json.loads((root / "models/evaluations/upfall-camera1-six-trials-v1.manifest.json").read_bytes())
        positives = [clip for clip in manifest["clips"] if clip["annotation"] == "positive"]
        self.assertEqual(len(positives), 3)
        for clip in positives:
            with self.subTest(clip=clip["file"]):
                timestamps = sorted(map(int, clip["subject_frames"]))
                by_slot = {}
                for pts in timestamps:
                    by_slot.setdefault((pts - timestamps[0]) * 5 // 1_000_000, pts)
                rows = base_receipts(list(by_slot.values()))
                coverage = slot_coverage(rows, {"1": (0, None)}, 5, clip["duration_s"], 0.8, 2)
                onset = clip["events"][0]["onset"]
                self.assertEqual(coverage["source_slots"]["skipped_slots"], 0)
                self.assertTrue(covered_window(coverage, onset["last_negative_s"] - 0.35,
                                               onset["first_visible_s"] + 8))

    def test_rejected_slots_lower_the_rate_and_exploration_cannot_fill_a_gap(self):
        rows = base_receipts([0, 200_000, 400_000, 600_000, 800_000, 1_000_000, 1_200_000])
        rows[1]["outcome"] = "timeout"
        coverage = slot_coverage(rows, {"1": (0, None)}, 5, 2, 0.8, 2)
        self.assertTrue(covered_window(coverage, 0, 1.2))
        self.assertFalse(covered_window(coverage, 0, 0.4))
        self.assertEqual(coverage["source_slots"]["rejected_timeout"], 1)
        rows[2]["slot"] = None
        coverage = slot_coverage(rows, {"1": (0, None)}, 5, 2, 0.8, 2)
        self.assertEqual(coverage["source_slots"]["accepted_extra"], 1)
        self.assertEqual(coverage["source_slots"]["skipped_slots"], 1)
        self.assertFalse(covered_window(coverage, 0, 1.2))

    def test_incomplete_collection_has_no_implicit_slot_coverage(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "receipts.jsonl"
            header = {"type": "header", "version": 1, "model_manifest_sha256": "model"}
            for broken in ({"type": "truncated"}, {"type": "task", "sequence": 2}):
                path.write_text(f"{json.dumps(header)}\n{json.dumps(broken)}\n", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "truncated or missing"):
                    read_receipts(path, "model")

    def test_each_accepted_task_has_a_policy_receipt_for_the_same_frame(self):
        frame = dict(camera_id="camera", epoch=1, scene_revision=1, generation="capture",
                     ordinal=2, source_us=500_000, task="fire")
        header = dict(type="header", version=2, model_manifest_sha256="model")
        task = dict(frame, type="task", sequence=1, outcome="accepted", slot=1)
        policy = dict(frame, type="policy", sequence=2, details={})
        with TemporaryDirectory() as directory:
            path = Path(directory) / "receipts.jsonl"
            for records, error in (
                ([task], "Policy receipts are missing"),
                ([task, dict(policy, epoch=2)], "no matching accepted task"),
                ([task, policy], None),
            ):
                path.write_text("".join(json.dumps(row) + "\n" for row in [header, *records]), encoding="utf-8")
                if error is None:
                    by_task, _, count = read_receipts(path, "model")
                    self.assertEqual(by_task["camera", "fire"], [task])
                    self.assertEqual(count, 2)
                else:
                    with self.assertRaisesRegex(ValueError, error):
                        read_receipts(path, "model")

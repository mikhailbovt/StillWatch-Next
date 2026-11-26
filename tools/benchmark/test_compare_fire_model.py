import importlib.util
import unittest


@unittest.skipUnless(
    all(importlib.util.find_spec(name) for name in ("cv2", "numpy", "onnxruntime")),
    "requires the Fire model comparison dependencies",
)
class CandidateProposals(unittest.TestCase):
    def evaluate(self, rows, *, height=320):
        import numpy as np

        from compare_fire_model import ANCHORS, SIDE, Candidate

        tensor = np.zeros((1, 6, ANCHORS), dtype=np.float32)
        for index, (box, fire, smoke) in enumerate(rows):
            tensor[0, :, index] = (*box, fire, smoke)

        class TensorSession:
            def run(self, *_):
                return [tensor]

        candidate = Candidate.__new__(Candidate)
        candidate.canvas = np.full((SIDE, SIDE, 3), 114, np.uint8)
        candidate.session = TensorSession()
        output, _ = candidate.evaluate(bytes(SIDE * height * 3), SIDE, height, 1_000_000)
        return output["fire_proposals"]

    def test_duplicates_of_strong_detection_leave_room_for_distinct_flame(self):
        smoke = (50, 160, 30, 30)
        flame = (240, 200, 20, 20)
        proposals = self.evaluate(
            [(smoke, 0, 0.70)] + [(smoke, 0, 0.19)] * 12 + [(flame, 0.15, 0)]
        )
        self.assertEqual([item["label"] for item in proposals], ["fire"])

    def test_repeated_weak_detections_leave_room_for_distinct_flame(self):
        smoke = (50, 160, 30, 30)
        flame = (240, 200, 20, 20)
        proposals = self.evaluate([(smoke, 0, 0.19)] * 12 + [(flame, 0.15, 0)])
        self.assertEqual([item["label"] for item in proposals], ["smoke", "fire"])

    def test_letterbox_padding_leaves_room_for_distinct_flame(self):
        padding = (50, 20, 30, 20)
        flame = (240, 160, 20, 20)
        proposals = self.evaluate([(padding, 0, 0.19)] * 12 + [(flame, 0.15, 0)], height=160)
        self.assertEqual([item["label"] for item in proposals], ["fire"])
        self.assertEqual(proposals[0]["box"], {"x1": 230, "y1": 70, "x2": 250, "y2": 90})

    def test_overlapping_proposals_of_different_classes_are_preserved(self):
        box = (50, 160, 30, 30)
        proposals = self.evaluate([(box, 0, 0.19), (box, 0.15, 0)])
        self.assertEqual([item["label"] for item in proposals], ["smoke", "fire"])

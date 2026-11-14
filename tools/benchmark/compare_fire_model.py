"""Development adapter for the pinned 320x320 CCTV-AI Fire candidate."""

import hashlib
import uuid
from time import perf_counter_ns

import cv2
import numpy as np
import onnxruntime as ort


SHA256 = "f2699b753e78be8d392bfa63e64bf102ea3ceb8b6086a59dfd8192b61a0c6fad"
SOURCE = "https://huggingface.co/fiacecson20/cctv-ai-fire-smoke/tree/343990e42d99a5d27e9f35fc7c80880dc5f43f45"
SIDE = 320
ANCHORS = 2100
LABELS = ("fire", "smoke")


class Candidate:
    def __init__(self, path, threads, cuda):
        if path.stat().st_size != 12139911:
            raise ValueError("candidate graph length does not match the pinned artifact")
        graph = path.read_bytes()
        if hashlib.sha256(graph).hexdigest() != SHA256:
            raise ValueError("candidate graph does not match the pinned artifact")
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.inter_op_num_threads = 1
        provider = ("CUDAExecutionProvider", {"use_tf32": "0", "cudnn_conv_algo_search": "HEURISTIC",
                                              "cudnn_conv_use_max_workspace": "0"}) if cuda else "CPUExecutionProvider"
        self.session = ort.InferenceSession(graph, options, providers=[provider])
        if cuda and self.session.get_providers()[0] != provider[0]:
            raise RuntimeError("candidate session did not load CUDA")
        inputs = [(i.name, i.type, i.shape) for i in self.session.get_inputs()]
        outputs = [(o.name, o.type, o.shape) for o in self.session.get_outputs()]
        if inputs != [("images", "tensor(float)", [1, 3, SIDE, SIDE])] or outputs != [
            ("output0", "tensor(float)", [1, 6, ANCHORS]),
        ]:
            raise ValueError("unexpected candidate tensor contract")
        names = self.session.get_modelmeta().custom_metadata_map.get("names")
        if names != "{0: 'fire', 1: 'smoke'}":
            raise ValueError("unexpected candidate label order")
        cv2.setNumThreads(1)
        self.canvas = np.full((SIDE, SIDE, 3), 114, np.uint8)
        self.session.run(["output0"], {"images": np.zeros((1, 3, SIDE, SIDE), np.float32)})
        self.provenance = {"graph_sha256": SHA256, "source": SOURCE, "onnxruntime_python": ort.__version__,
                           "opencv": cv2.__version__, "numpy": np.__version__, "providers": self.session.get_providers(),
                           "input_shape": [1, 3, SIDE, SIDE], "labels": list(LABELS)}

    def evaluate(self, rgb, width, height, pts):
        start = perf_counter_ns()
        gain = min(SIDE / width, SIDE / height)
        resized_w, resized_h = max(1, round(width * gain)), max(1, round(height * gain))
        x, y = (SIDE - resized_w) // 2, (SIDE - resized_h) // 2
        pixels = np.frombuffer(rgb, np.uint8).reshape(height, width, 3)
        self.canvas.fill(114)
        cv2.resize(pixels, (resized_w, resized_h), dst=self.canvas[y:y + resized_h, x:x + resized_w],
                   interpolation=cv2.INTER_LINEAR)
        tensor = np.ascontiguousarray(self.canvas.transpose(2, 0, 1)[None], dtype=np.float32) / np.float32(255.0)
        inference = perf_counter_ns()
        values, = self.session.run(["output0"], {"images": tensor})
        decoding = perf_counter_ns()
        if not np.isfinite(values).all() or (values[0, 4:] < 0).any() or (values[0, 4:] > 1).any():
            raise ValueError("invalid detector output")
        boxes, scores = values[0, :4].T, values[0, 4:].T
        labels = np.argmax(scores, axis=1)
        scores = scores[np.arange(ANCHORS), labels]
        confident = np.flatnonzero(scores > np.float32(0.20))
        weak = np.flatnonzero((scores >= np.float32(0.12)) & (scores <= np.float32(0.20)))
        confident = confident[np.argsort(-scores[confident], kind="stable")]
        weak = weak[np.argsort(-scores[weak], kind="stable")][:8]
        sx, sy = np.float32(resized_w / width), np.float32(resized_h / height)

        def project(coordinate, origin, scale, bound):
            return float(np.clip((np.float32(coordinate) - origin) / scale, 0, bound))

        def iou(a, b):
            ix = max(0.0, min(a[2], b[2]) - max(a[0], b[0]))
            iy = max(0.0, min(a[3], b[3]) - max(a[1], b[1]))
            total = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - ix * iy
            return ix * iy / max(1.0, total)

        def candidates(indices, accepted, weak=False):
            kept = []
            for i in indices:
                cx, cy, w, h = boxes[i]
                if w <= 0 or h <= 0:
                    continue
                rect = tuple(map(float, (cx - w * np.float32(0.5), cy - h * np.float32(0.5),
                                         cx + w * np.float32(0.5), cy + h * np.float32(0.5))))
                label = LABELS[labels[i]]
                if not weak and any(label == a[0] and iou(rect, a[1]) > 0.70 for a in kept):
                    continue
                left, top, right, bottom = rect
                mapped = (project(left, x, sx, width), project(top, y, sy, height),
                          project(right, x, sx, width), project(bottom, y, sy, height))
                if (mapped[2] - mapped[0]) * (mapped[3] - mapped[1]) < (4.0 if weak else 1.0):
                    continue
                if weak and any(label == a[0] and iou(mapped, a[2]) > 0.45 for a in accepted):
                    continue
                observation = {"capture_ts": pts / 1e6, "label": label, "score": float(scores[i]),
                               "box": dict(zip(("x1", "y1", "x2", "y2"), mapped, strict=True))}
                kept.append((label, rect, mapped, observation))
                if len(kept) == 300:
                    break
            return kept

        kept = candidates(confident, ())
        proposals = candidates(weak, kept, weak=True)
        end = perf_counter_ns()
        return ({"inference": {"request": str(uuid.UUID(int=0)), "elapsed_ms": 0, "pose": None,
                               "fire": [o[3] for o in kept]}, "fire_proposals": [o[3] for o in proposals]},
                (0, (inference - start) // 1000, 0, 0, (decoding - inference) // 1000, (end - decoding) // 1000, 0))

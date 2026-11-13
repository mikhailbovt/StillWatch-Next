"""Build a local, CPU FP32 ONNX pack from the exact Stillwatch 0.5.0 graph."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory

BASELINE = "b0d20a20d38a5bbc22178d150999e57cc9e7bfb3"
RUNTIME = "1.31.0"
OPSET = 17
SIZE = 640
MODELS = ("pose", "pose_balanced", "fire")
SQUARE = "cpu_fp32_square_640"
RECTANGULAR = "cpu_fp32_rect_640"


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path: Path, data: object) -> None:
    path.write_text(json.dumps(data, indent=2, sort_keys=True, allow_nan=False) + "\n", encoding="utf-8", newline="\n")


def verify_baseline(root: Path, weights: Path) -> dict:
    # Check the graph loader and its local imports before loading any Python from it.
    def git(*args: str) -> bytes:
        return subprocess.check_output(["git", "-C", str(root), *args])

    if git("rev-parse", "HEAD").decode().strip() != BASELINE:
        raise ValueError(f"Expected baseline {BASELINE}")
    paths = ["src/stillwatch", "models.lock.json", "uv.lock"]
    if git("status", "--porcelain", "--untracked-files=all", "--", *paths).strip():
        raise ValueError("Baseline source and lock files must match the pinned checkout")
    lock = json.loads((root / "models.lock.json").read_text(encoding="utf-8"))["models"]
    for key in MODELS:
        entry = lock[key]
        raw = weights / "sources" / f"{key}.pt"
        tensor = weights / Path(entry["local_path"]).name
        if raw.stat().st_size != entry["size"] or sha256(raw) != entry["sha256"]:
            raise ValueError(f"Locked source artifact mismatch for {key}")
        if sha256(tensor) != entry["tensor_sha256"]:
            raise ValueError(f"Locked tensor artifact mismatch for {key}")
    return lock


def tensor_cases(fixtures: Path, images: list[Path], rectangular: bool):
    import cv2
    import numpy as np
    from ultralytics.data.augment import LetterBox

    fixtures.mkdir()
    shape = (1, 3, SIZE, SIZE)
    rng = np.random.default_rng(0x5357)
    cases = {
        "black": (np.zeros(shape, dtype=np.float32), None),
        "gray": (np.full(shape, np.float32(114 / 255), dtype=np.float32), None),
        "seeded_noise": (rng.random(shape, dtype=np.float32), None),
    }
    for i, path in enumerate(images):
        encoded = path.read_bytes()
        bgr = cv2.imdecode(np.frombuffer(encoded, dtype=np.uint8), cv2.IMREAD_COLOR)
        if bgr is None:
            raise ValueError(f"Unable to decode parity input {i}")
        rgb = np.ascontiguousarray(bgr[:, :, ::-1])
        case = f"reference_{i:02}"
        rgb_path = fixtures / f"{case}.rgb"
        rgb.tofile(rgb_path)
        padded = LetterBox(new_shape=(SIZE, SIZE), auto=rectangular, stride=32)(image=bgr)
        tensor = np.ascontiguousarray(padded[:, :, ::-1].transpose(2, 0, 1)[None], dtype=np.float32)
        tensor /= np.float32(255)
        cases[case] = (
            tensor,
            {
                "encoded_sha256": hashlib.sha256(encoded).hexdigest(),
                "rgb_sha256": sha256(rgb_path),
                "rgb_shape": list(rgb.shape),
            },
        )
    for case, (tensor, image) in cases.items():
        path = fixtures / f"{case}.f32"
        tensor.astype("<f4", copy=False).tofile(path)
        yield case, tensor, {"tensor_sha256": sha256(path), "tensor_hw": list(tensor.shape[-2:]), "image": image}


def export_one(key: str, entry: dict, weights: Path, directory: Path, cases: list, rectangular: bool) -> tuple[dict, dict]:
    import copy

    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch
    from stillwatch.perception import load_safe_model
    from ultralytics.nn.modules import Detect

    task = entry["task"]
    weights_path = weights / Path(entry["local_path"]).name
    reference = load_safe_model(weights_path, task).model.float().eval().fuse(verbose=False)
    model = copy.deepcopy(reference)
    for head in model.modules():
        if isinstance(head, Detect):
            head.export = True
            head.format = "onnx"
            head.dynamic = rectangular
            head.max_det = 300
            head.xyxy = False
            head.shape = None

    path = directory / f"{key}.onnx"
    with torch.inference_mode():
        dummy = torch.from_numpy(cases[0][1])
        model(dummy)
        torch.onnx.export(
            model,
            dummy,
            str(path),
            input_names=["images"],
            output_names=["output0"],
            opset_version=OPSET,
            dynamo=False,
            external_data=False,
            dynamic_axes={"images": {2: "height", 3: "width"}, "output0": {2: "anchors"}} if rectangular else None,
        )

    graph = onnx.load(path, load_external_data=False)
    if any(tensor.data_location == onnx.TensorProto.EXTERNAL for tensor in graph.graph.initializer):
        raise ValueError("Exporter produced untracked external data")
    end_to_end = bool(getattr(reference, "end2end", False))
    if task == "pose":
        shape = [1, 300, 57] if end_to_end else [1, 56, 8400]
        layout = "pose_xyxy_end_to_end" if end_to_end else "pose_xywh_keypoints"
        confidence, iou = 0.15, 0.50
    else:
        if end_to_end:
            raise ValueError("Unexpected end-to-end fire head")
        shape, layout = [1, 6, 8400], "fire_xywh_class_scores"
        confidence, iou = 0.20, 0.70
    manifest_input = [1, 3, -1, -1] if rectangular else [1, 3, SIZE, SIZE]
    manifest_output = [*shape[:2], -1] if rectangular else shape
    if rectangular:
        # TorchScript leaves the invariant dimensions of a dynamic Concat symbolic.
        # Lock batch/channels in the graph and verify them again for every case.
        for dim, value in zip(graph.graph.output[0].type.tensor_type.shape.dim[:2], shape[:2], strict=True):
            if dim.HasField("dim_value") and dim.dim_value != value:
                raise ValueError(f"Unexpected fixed dimension for {key}")
            dim.dim_value = value
        onnx.save(graph, path)
    onnx.checker.check_model(graph, full_check=True)
    options = ort.SessionOptions()
    options.intra_op_num_threads = 4
    session = ort.InferenceSession(str(path), sess_options=options, providers=["CPUExecutionProvider"])
    input_shape = session.get_inputs()[0].shape
    output_shape = session.get_outputs()[0].shape
    def normalized(shape):
        return [v if isinstance(v, int) else -1 for v in shape]
    if normalized(input_shape) != manifest_input or normalized(output_shape) != manifest_output:
        raise ValueError(f"Unexpected ONNX I/O layout for {key}: {input_shape} -> {output_shape}")

    report = {}
    for name, tensor, metadata in cases:
        with torch.inference_mode():
            expected = reference(torch.from_numpy(tensor))[0].numpy()
        actual = session.run(["output0"], {"images": tensor})[0]
        h, w = tensor.shape[-2:]
        case_shape = [*shape[:2], sum((h // stride) * (w // stride) for stride in (8, 16, 32))]
        if list(expected.shape) != case_shape or list(actual.shape) != case_shape:
            raise ValueError(f"Unexpected output shape for {key}/{name}")
        if not (np.isfinite(expected).all() and np.isfinite(actual).all()):
            raise ValueError(f"Non-finite output for {key}/{name}")
        expected_path = directory / "fixtures" / f"{key}_{name}.f32"
        expected.astype("<f4", copy=False).tofile(expected_path)
        error = np.abs(actual.astype(np.float64) - expected)
        if end_to_end:
            actual_scores, expected_scores = actual[0, :, 4], expected[0, :, 4]
        elif task == "pose":
            actual_scores, expected_scores = actual[0, 4], expected[0, 4]
        else:
            actual_scores, expected_scores = actual[0, 4:].max(0), expected[0, 4:].max(0)
        report[name] = {
            "raw_max_abs": float(error.max()),
            "raw_mean_abs": float(error.mean()),
            "max_abs_ranked_confidence": float(np.max(np.abs(np.sort(actual_scores) - np.sort(expected_scores)))),
            "min_reference_threshold_distance": float(np.min(np.abs(expected_scores - confidence))),
            "above_threshold_reference": int((expected_scores > confidence).sum()),
            "above_threshold_onnx": int((actual_scores > confidence).sum()),
            "reference_sha256": sha256(expected_path),
        }
        if metadata["image"] is not None:
            rgb = np.fromfile(directory / "fixtures" / f"{name}.rgb", dtype=np.uint8).reshape(metadata["image"]["rgb_shape"])
            report[name]["observations"] = reference_observations(expected, rgb, (h, w), entry, confidence, iou, end_to_end)

    metadata = {
        "artifact": path.name,
        "sha256": sha256(path),
        "byte_length": path.stat().st_size,
        "source": entry,
        "opset": OPSET,
        "external_data": [],
        "input": {
            "name": "images", "dtype": "float32", "shape": manifest_input,
            "layout": "nchw", "colour": "rgb", "divisor": 255,
            "resize": "opencv_inter_linear", "padding": 114, "centered_letterbox": True,
        },
        "output": {
            "name": "output0", "dtype": "float32", "shape": manifest_output,
            "layout": layout,
            "keypoint_map": "coco17_xy_confidence" if task == "pose" else None,
            "nms": "none" if end_to_end else "class_aware",
            "legacy_confidence": confidence,
            "legacy_iou": None if end_to_end else iou,
        },
    }
    return metadata, report


def reference_observations(values, rgb, tensor_hw, entry, confidence, iou, end_to_end):
    import numpy as np
    import torch
    import torchvision  # The baseline predictor warms up this NMS backend.
    from ultralytics.utils import nms, ops

    from stillwatch.perception import torso_histogram, visible_keypoints

    if not torchvision.__version__:
        raise RuntimeError("Missing torchvision provenance")
    rows = nms.non_max_suppression(
        torch.from_numpy(values.copy()), conf_thres=confidence, iou_thres=iou,
        nc=len(entry["classes"]), end2end=end_to_end,
    )[0]
    pose = entry["task"] == "pose"
    height, width = rgb.shape[:2]
    ops.scale_boxes(tensor_hw, rows[:, :4], rgb.shape)
    if pose and len(rows):
        joints = ops.scale_coords(tensor_hw, rows[:, 6:].reshape(len(rows), 17, 3), rgb.shape).tolist()
        bgr = np.ascontiguousarray(rgb[:, :, ::-1])
    observations = []
    for index, row in enumerate(rows.tolist()):
        x1, y1, x2, y2, score, label, *_ = row
        points = visible_keypoints(joints[index], width, height) if pose else []
        observations.append({
            "capture_ts": 0.0,
            "box": {"x1": x1, "y1": y1, "x2": x2, "y2": y2},
            "score": score, "label": entry["classes"][str(int(label))],
            "keypoints": points, "limited": pose and y2 - y1 < 40,
            "truncated": pose and (x1 <= 3 or y1 <= 3 or x2 >= width - 3 or y2 >= height - 3),
            "torso_histogram": torso_histogram(bgr, points) if pose else [],
        })
    return observations


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--weights", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--image", type=Path, action="append", default=[])
    parser.add_argument("--profile", choices=(SQUARE, RECTANGULAR), default=RECTANGULAR)
    args = parser.parse_args()
    baseline = args.baseline.resolve(strict=True)
    weights = args.weights.resolve(strict=True)
    output = args.output.resolve()
    if output.exists():
        parser.error("The output directory already exists")
    lock = verify_baseline(baseline, weights)
    output.parent.mkdir(parents=True, exist_ok=True)
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(baseline / "src"))

    with TemporaryDirectory(prefix=".stillwatch-export-", dir=output.parent) as staging:
        scratch = Path(staging)
        (scratch / "Ultralytics").mkdir()
        os.environ["YOLO_CONFIG_DIR"] = str(scratch)
        os.environ["YOLO_AUTOINSTALL"] = "false"
        os.environ["YOLO_OFFLINE"] = "true"
        os.environ["TORCH_FORCE_WEIGHTS_ONLY_LOAD"] = "1"

        import cv2
        import onnx
        import onnxruntime as ort
        import torch
        import ultralytics

        if platform.python_version() != "3.12.14" or ort.__version__ != RUNTIME:
            raise ValueError("Run the exporter using its locked toolchain")
        torch.set_num_threads(4)
        cv2.setNumThreads(1)
        pack = scratch / "pack"
        pack.mkdir()
        rectangular = args.profile == RECTANGULAR
        cases = list(tensor_cases(pack / "fixtures", args.image, rectangular))
        exports = {}
        reports = {}
        for key in MODELS:
            exports[key], reports[key] = export_one(key, lock[key], weights, pack, cases, rectangular)
            print(f"Exported and executed {key}", flush=True)
        write_json(pack / "parity-report.json", {
            "baseline": BASELINE, "profile": args.profile,
            "cases": {name: metadata for name, _, metadata in cases},
            "models": reports,
        })
        write_json(pack / "models.lock.json", {
            "schema_version": 2, "profile": args.profile, "execution_provider": "cpu",
            "baseline": BASELINE, "baseline_lock_sha256": sha256(baseline / "models.lock.json"),
            "onnxruntime": RUNTIME, "models": exports,
            "exporter": {
                "source_sha256": sha256(Path(__file__)), "dependency_lock_sha256": sha256(Path(__file__).with_name("uv.lock")),
                "python": platform.python_version(), "onnx": onnx.__version__, "torch": torch.__version__,
                "ultralytics": ultralytics.__version__, "fused": True, "dynamo": False,
            },
            "parity_report_sha256": sha256(pack / "parity-report.json"),
        })
        pack.rename(output)


if __name__ == "__main__":
    main()

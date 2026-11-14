"""Run the frozen Pose gray tensor against the pinned original reference model."""

import argparse
import hashlib
import importlib
import json
import subprocess
import sys
from pathlib import Path


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def load_original(path, expected_hash, expected_modules):
    import torch

    if digest(path) != expected_hash:
        raise ValueError("original checkpoint hash mismatch")
    prefixes = ("ultralytics.nn.modules.", "ultralytics.nn.tasks.", "torch.nn.modules.")
    approved = []
    for name in torch.serialization.get_unsafe_globals_in_checkpoint(path):
        module, symbol = name.rsplit(".", 1)
        if not name.startswith(prefixes) or symbol.startswith("_"):
            raise ValueError(f"unapproved checkpoint global {name}")
        cls = getattr(importlib.import_module(module), symbol)
        if not isinstance(cls, type) or not issubclass(cls, torch.nn.Module):
            raise ValueError(f"unapproved checkpoint class {name}")
        approved.append((cls, name))
    with torch.serialization.safe_globals(approved):
        checkpoint = torch.load(path, map_location="cpu", weights_only=True)
    model = checkpoint.get("ema") or checkpoint["model"]
    modules = {name: type(layer).__module__ + "." + type(layer).__qualname__
               for name, layer in model.named_modules(remove_duplicate=False)}
    encoded = json.dumps(modules, sort_keys=True, separators=(",", ":")).encode()
    if hashlib.sha256(encoded).hexdigest() != expected_modules:
        raise ValueError("original module inventory differs")
    return model.float().eval().fuse(verbose=False)


def run(args):
    import numpy as np
    import torch

    sys.path.insert(0, str(args.reference / "src"))
    from stillwatch.perception import load_safe_model

    manifest = args.pack / "models.lock.json"
    if digest(manifest) != args.manifest_sha256:
        raise ValueError("model manifest hash mismatch")
    locked = json.loads(manifest.read_text(encoding="utf-8"))
    commit = subprocess.check_output(["git", "-C", str(args.reference), "rev-parse", "HEAD"], text=True).strip()
    if commit != locked["baseline"]:
        raise ValueError("reference source revision mismatch")
    entry = locked["models"]["pose"]
    parity = args.pack / "parity-report.json"
    if digest(parity) != locked["parity_report_sha256"]:
        raise ValueError("parity report hash mismatch")
    report = json.loads(parity.read_text(encoding="utf-8"))
    original = args.weights / Path(entry["source"]["local_path"]).name
    source = args.pack / "fixtures/gray.f32"
    expected = args.pack / "fixtures/pose_gray.f32"
    if digest(original) != entry["source"]["tensor_sha256"]:
        raise ValueError("reference weight hash mismatch")
    if digest(source) != report["cases"]["gray"]["tensor_sha256"]:
        raise ValueError("fixture input hash mismatch")
    if digest(expected) != report["models"]["pose"]["gray"]["reference_sha256"]:
        raise ValueError("fixture reference output hash mismatch")
    h, w = report["cases"]["gray"]["tensor_hw"]
    anchors = sum(h // stride * (w // stride) for stride in (8, 16, 32))
    x = np.fromfile(source, dtype="<f4").reshape(1, 3, h, w)
    y = np.fromfile(expected, dtype="<f4").reshape(1, 56, anchors)
    torch.set_num_threads(4)
    torch.backends.cuda.matmul.allow_tf32 = False
    torch.backends.cudnn.allow_tf32 = False
    torch.backends.cudnn.benchmark = False
    model = load_safe_model(original, "pose").model.float().eval().fuse(verbose=False)
    device = torch.device(args.device)
    model = model.to(device)
    input_tensor = torch.from_numpy(x).to(device)
    with torch.inference_mode():
        copies = [model(input_tensor)[0].cpu().numpy().copy() for _ in range(3)]
        original_error = None
        if args.original_checkpoint:
            source_meta = entry["source"]
            original_model = load_original(args.original_checkpoint, source_meta["sha256"],
                                           source_meta["module_types_sha256"]).to(device)
            original_output = original_model(input_tensor)[0].cpu().numpy()
            delta = np.abs(original_output.astype(np.float64) - copies[0])
            original_error = {"checkpoint_sha256": source_meta["sha256"], "max_abs": float(delta.max()),
                              "mean_abs": float(delta.mean()), "identical": np.array_equal(original_output, copies[0])}
    actual = copies[0]
    if actual.shape != y.shape or not all(np.isfinite(a).all() for a in copies):
        raise ValueError("invalid pose output tensor")
    error = np.abs(actual.astype(np.float64) - y)
    threshold = 0.15
    summary = {
        "device": args.device,
        "accelerator": torch.cuda.get_device_name(device) if device.type == "cuda" else None,
        "pytorch": torch.__version__,
        "pytorch_cuda": torch.version.cuda,
        "reference_commit": commit,
        "cudnn": torch.backends.cudnn.version(),
        "source_sha256": digest(source),
        "reference_sha256": digest(expected),
        "weight_sha256": digest(original),
        "tensor_shape": list(actual.shape),
        "max_abs": float(error.max()),
        "mean_abs": float(error.mean()),
        "max_abs_by_output_channel": error.max(axis=(0, 2)).tolist(),
        "mean_abs_by_output_channel": error.mean(axis=(0, 2)).tolist(),
        "anchor_crossings": int(np.count_nonzero((actual[:, 4] > threshold) != (y[:, 4] > threshold))),
        "confidence_near_threshold": int(np.count_nonzero(np.abs(y[:, 4] - threshold) < 0.001)),
        "identical_repeats": all(np.array_equal(actual, copy) for copy in copies[1:]),
        "original_vs_reconstructed": original_error,
    }
    args.output.write_text(json.dumps(summary, indent=2, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("reference", "weights", "pack", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--device", choices=("cpu", "cuda"), required=True)
    parser.add_argument("--original-checkpoint", type=Path)
    run(parser.parse_args())

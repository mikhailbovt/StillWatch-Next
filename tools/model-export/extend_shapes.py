"""Extend a locked rectangular model pack with reference cases from the same weights."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
from tempfile import TemporaryDirectory

from export import MODELS, RECTANGULAR, sha256, write_json


def read_json(path, expected):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"SHA-256 mismatch for {path}")
    return json.loads(data)


def copy_checked(source, target, expected):
    shutil.copyfile(source, target)
    if sha256(target) != expected:
        raise ValueError(f"SHA-256 mismatch for {source}")


def read_pack(directory, sha256):
    lock = read_json(directory / "models.lock.json", sha256)
    if lock["schema_version"] != 3 or lock["profile"] != RECTANGULAR or set(lock["models"]) != set(MODELS):
        raise ValueError("Expected a versioned rectangular model pack")
    report = read_json(directory / "parity-report.json", lock["parity_report_sha256"])
    if any(report[key] != lock[key] for key in ("baseline", "profile")):
        raise ValueError("Parity report does not match its model pack")
    return lock, report


def extend(base, base_sha256, reference, reference_sha256, output):
    lock, report = read_pack(base, base_sha256)
    source, source_report = read_pack(reference, reference_sha256)
    if any(source[key] != lock[key] for key in ("baseline_lock_sha256", "models", "onnxruntime", "execution_provider")):
        raise ValueError("Reference pack must use the same models and runtime version")
    contract, source_contract = lock["contract"], source["contract"]
    if any(source_contract[key] != contract[key] for key in ("source_revision", "roles", "observation_spec_sha256")):
        raise ValueError("Reference pack uses a different observation contract")

    shapes = {tuple(shape) for shape in contract["tensor_shapes"]}
    added = {tuple(shape) for shape in source_contract["tensor_shapes"]} - shapes
    cases = [(name, case) for name, case in source_report["cases"].items() if tuple(case["tensor_hw"]) in added]
    if not added or {tuple(case["tensor_hw"]) for _, case in cases} != added:
        raise ValueError("Each new shape must have a locked reference case")
    if len(shapes | added) > 16 or len(report["cases"]) + len(cases) > 64:
        raise ValueError("Expanded pack exceeds the worker's shape or reference limits")
    if output.exists():
        raise FileExistsError(output)

    with TemporaryDirectory(prefix=".stillwatch-shapes-", dir=output.parent) as scratch:
        pack = Path(scratch) / "pack"
        (pack / "fixtures").mkdir(parents=True)
        for model, spec in lock["models"].items():
            artifact = f"{model}.onnx"
            if spec["artifact"] != artifact:
                raise ValueError("Unexpected artifact in rectangular model pack")
            copy_checked(base / artifact, pack / artifact, spec["sha256"])

        def copy_case(directory, parity, name, target, case):
            if not all(re.fullmatch(r"[a-z0-9_]{1,40}", item) for item in (name, target)):
                raise ValueError("Invalid reference case name")
            files = [("", ".f32", case["tensor_sha256"])]
            if case.get("image") is not None:
                files.append(("", ".rgb", case["image"]["rgb_sha256"]))
            for model in lock["models"]:
                files.append((f"{model}_", ".f32", parity["models"][model][name]["reference_sha256"]))
            for prefix, suffix, expected in files:
                copy_checked(directory / "fixtures" / f"{prefix}{name}{suffix}",
                             pack / "fixtures" / f"{prefix}{target}{suffix}", expected)

        for name, case in report["cases"].items():
            copy_case(base, report, name, name, case)
        for name, case in cases:
            height, width = case["tensor_hw"]
            target = f"{height}x{width}_{name}"
            if target in report["cases"]:
                raise ValueError(f"Reference case already exists: {target}")
            copy_case(reference, source_report, name, target, case)
            report["cases"][target] = case
            for model in lock["models"]:
                report["models"][model][target] = source_report["models"][model][name]

        write_json(pack / "parity-report.json", report)
        lock["parity_report_sha256"] = sha256(pack / "parity-report.json")
        contract["tensor_shapes"] = sorted(shapes | added)
        write_json(pack / "models.lock.json", lock)
        pack.rename(output)
    return sha256(output / "models.lock.json")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--base-sha256", required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--reference-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(extend(**vars(args)))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"{error}\n")

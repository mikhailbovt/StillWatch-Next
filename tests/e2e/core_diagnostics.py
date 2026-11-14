"""Verify redacted diagnostics for an installed, disabled, or unavailable model runtime."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core, until


def check(args, root, name, options, expected):
    data = root / name
    source = root / "inputs"
    abandoned = data / ".stillwatch-media-Abc123"
    abandoned.mkdir(parents=True)
    (abandoned / "mediamtx.json").write_text("{}")
    with_extra = data / ".stillwatch-media-Efg456"
    with_extra.mkdir()
    (with_extra / "mediamtx.json").write_text("{}")
    retained = with_extra / "unrelated.txt"
    retained.write_text("keep")
    unrelated = data / ".stillwatch-media-operator"
    unrelated.mkdir()
    (unrelated / "mediamtx.json").write_text("{}")
    with run_core(args.core, args.worker, data, source, root / f"{name}.log", *options) as (_, client):
        assert not abandoned.exists()
        assert not (with_extra / "mediamtx.json").exists() and retained.read_text() == "keep"
        assert (unrelated / "mediamtx.json").is_file()
        code, _ = client.call("GET", "/api/v2/status", authorized=False)
        assert code == 401, code

        def initialized():
            code, status = client.call("GET", "/api/v2/status")
            assert code == 200 and status["media"] is None, (code, status)
            inference = status["inference"]
            return status if inference["state"] != "starting" else None

        status = until(initialized, 35)
        inference = status["inference"].copy()
        if expected["backend"] is not None:
            totals = inference.pop("totals")
            assert set(totals) == {"pose", "fire"}
            assert all(int(value) == 0 for counts in totals.values() for value in counts.values())
        assert inference == expected, status
        assert client.call("GET", "/health/ready")[0] == 200
        report_path = root / f"{name}-diagnostics.json"
        local = client.address.removeprefix("http://")
        subprocess.run([str(args.cli), "doctor", "--local", local, "--data-dir", str(data),
                        "--output", str(report_path)], check=True, timeout=15)
        report = report_path.read_text(encoding="utf-8")
        recorded = json.loads(report)["status"]
        writer = recorded.pop("writer")
        earlier = status.pop("writer")
        assert "camera_coverage" not in recorded and status.pop("camera_coverage") == []
        assert writer["queue_capacity"] == earlier["queue_capacity"] and writer["queue_high_water"] >= earlier["queue_high_water"]
        assert int(writer["completed"]) >= int(earlier["completed"])
        assert recorded == status
        assert str(root) not in report and client.token not in report and local not in report
        code, bootstrap = client.call("GET", "/bootstrap/info", authorized=False)
        assert code == 200 and bootstrap["instance_id"] not in report


def run(args):
    def expectation(state, backend, ready, failure=None):
        return {"state": state, "backend": backend, "ready_workers": ready,
                "configured_workers": int(backend is not None), "failure": failure}

    with TemporaryDirectory(prefix="stillwatch-diagnostics-") as temporary:
        root = Path(temporary)
        (root / "inputs").mkdir()
        check(args, root, "disabled", [], expectation("disabled", None, 0))
        if args.models:
            options = ["--model-pack", str(args.models), "--model-manifest-sha256", args.manifest_sha256,
                       "--ort-library", str(args.runtime), "--ort-library-sha256", args.runtime_sha256]
            check(args, root, "available", options, expectation("ready", "cpu", 1))
            missing = options.copy()
            missing[1] = str(root / "missing-models")
            check(args, root, "missing", missing, expectation("degraded", "cpu", 0, "model_pack_unavailable"))
            missing = options.copy()
            missing[5] = str(root / "missing-ort-library")
            check(args, root, "runtime", missing, expectation("degraded", "cpu", 0, "runtime_unavailable"))
    print(json.dumps({"redacted": True, "authorization_required": True, "disabled_profile": True,
                      "abandoned_media_config_removed": True,
                      "model_and_runtime_profiles": args.models is not None}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "cli"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True), required=True)
    for name in ("models", "runtime"):
        parser.add_argument(f"--{name}", type=lambda path: Path(path).resolve(strict=True))
    parser.add_argument("--manifest-sha256")
    parser.add_argument("--runtime-sha256")
    args = parser.parse_args()
    supplied = (args.models, args.runtime, args.manifest_sha256, args.runtime_sha256)
    if any(supplied) and not all(supplied):
        parser.error("model and runtime options must be supplied together")
    run(args)

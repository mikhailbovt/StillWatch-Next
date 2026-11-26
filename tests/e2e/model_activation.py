"""Switch two qualified packs through the native selection and Core startup paths."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path
from tempfile import TemporaryDirectory

from analysis_core import digest
from replay_core import run_core, until


def run(args):
    packs = (args.square, args.rectangular)
    manifests = [digest(pack / "models.lock.json") for pack in packs]
    assert len(set(manifests)) == 2
    assert digest(args.runtime) == args.runtime_sha256
    with TemporaryDirectory(prefix="stillwatch-model-rollout-") as temporary:
        root = Path(temporary)
        sources = root / "sources"
        sources.mkdir()
        subprocess.run([
            str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-ss", "40", "-i", str(args.demo),
            "-t", "8", "-vf", "scale=640:360", "-an", "-c:v", "libx264", "-threads", "1",
            str(sources / "fire.mp4"),
        ], check=True)
        selection = root / "selected-model.json"
        settings = root / "core.json"
        settings.write_text(json.dumps({"model_selection_file": str(selection)}), encoding="utf-8")
        data = root / "data"

        def publish(index, sha=None):
            return subprocess.run([
                str(args.worker), "activate-model-pack", str(selection),
                str(args.runtime), args.runtime_sha256, str(packs[index]), sha or manifests[index],
            ], capture_output=True, text=True, timeout=90)

        def check_worker():
            process = subprocess.run([
                str(args.core), "--data-dir", str(root / "unused"), "--check-inference",
                "--worker", str(args.worker), "--settings-file", str(settings),
            ], capture_output=True, text=True, timeout=40)
            assert process.returncode == 0, process.stderr
            assert not (root / "unused").exists()

        def publish_qualified(index):
            process = publish(index)
            assert process.returncode == 0, process.stderr
            assert json.loads(process.stdout)["qualified"] is True
            assert json.loads(selection.read_bytes())["manifest_sha256"] == manifests[index]
            check_worker()

        def add_and_observe(client, key, expected):
            status, camera = client.call("POST", "/api/v2/cameras", {
                "name": key, "enabled": True,
                "source": {"type": "replay", "file": "fire.mp4", "pace": True},
            }, key=key)
            assert status == 201, (status, camera)

            def incident():
                code, page = client.call("GET", "/api/v2/incidents")
                assert code == 200, (code, page)
                return next((item for item in page["incidents"] if item["camera_id"] == camera["id"]), None)

            item = until(incident, 15)
            assert item["category"] == "fire", item
            assert item["analysis_origin"]["model_manifest_sha256"] == expected, item
            return item

        publish_qualified(0)
        with run_core(args.core, args.worker, data, sources, root / "before.log",
                      "--settings-file", str(settings)) as (_, client):
            existing = add_and_observe(client, "before-change", manifests[0])
            before = selection.read_bytes()
            rejected = publish(1, "0" * 64)
            assert rejected.returncode != 0 and selection.read_bytes() == before, rejected.stderr
            publish_qualified(1)
            add_and_observe(client, "before-restart", manifests[0])

        with run_core(args.core, args.worker, data, sources, root / "after.log",
                      "--settings-file", str(settings)) as (_, client):
            status, saved = client.call("GET", f"/api/v2/incidents/{existing['id']}")
            assert status == 200, (status, saved)
            assert saved["analysis_origin"] == existing["analysis_origin"]
            add_and_observe(client, "after-restart", manifests[1])

        publish_qualified(0)
        with run_core(args.core, args.worker, data, sources, root / "rollback.log",
                      "--settings-file", str(settings)) as (_, client):
            add_and_observe(client, "after-rollback", manifests[0])
    print(json.dumps({
        "failed_validation_kept_selection": True, "running_worker_kept_previous_pack": True,
        "restart_switched_pack": True, "incident_versions_persisted": True, "rollback_ready": True,
    }))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "ffmpeg", "demo", "square", "rectangular", "runtime"):
        parser.add_argument(f"--{name}", type=lambda value: Path(value).resolve(strict=True), required=True)
    parser.add_argument("--runtime-sha256", required=True)
    run(parser.parse_args())

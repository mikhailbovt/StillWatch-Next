"""Run the original StillWatch replay in its separately locked Python environment."""

import asyncio
import contextlib
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from tempfile import TemporaryDirectory


async def main(legacy, weights, media, manifest):
    sys.path.insert(0, str(legacy / "src"))
    source = legacy / "tools/replay.py"
    spec = importlib.util.spec_from_file_location("stillwatch_legacy_replay", source)
    replay = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(replay)
    from stillwatch.runtime import Analysis
    from stillwatch.telemetry import Telemetry

    settings = replay.load_settings(legacy / "configs/default.yaml").model_copy(update={
        "pose_weights": weights / "pose-balanced.safetensors",
        "fire_weights": weights / "fire.safetensors",
    })
    models = json.loads((legacy / "models.lock.json").read_text())
    for path in (settings.pose_weights, settings.fire_weights):
        expected = next(model["tensor_sha256"] for model in models["models"].values() if Path(model["local_path"]).name == path.name)
        with path.open("rb") as file:
            if hashlib.file_digest(file, "sha256").hexdigest() != expected:
                raise ValueError("Legacy weights do not match the public baseline lock")
    with contextlib.redirect_stdout(sys.stderr), TemporaryDirectory(prefix="stillwatch-reference-replay-") as directory:
        settings = settings.model_copy(update={"data_dir": Path(directory) / "runtime"})
        analysis = Analysis(settings, Telemetry())
        results = {}
        for index, clip in enumerate(json.loads(manifest.read_text())["clips"]):
            result = await replay.replay(media / clip["file"], settings, Path(directory) / str(index), analysis=analysis)
            if result["source_sha256"] != clip["sha256"] or not all(
                result["routes"][kind] == "ready" and result["inference_calls"][kind] > 0 for kind in ("pose", "fire")
            ):
                raise RuntimeError("Legacy replay did not process the locked source and both models")
            results[clip["file"]] = {"events": [
                {"category": item["category"], "opened_source_s": item["detected_ts"],
                 "attention": "legacy_candidate", "reason": item["reason"]}
                for item in result["events"] if item["category"] in {"fire", "medical"}
            ], "inference_calls": result["inference_calls"], "trace_sha256": result["trace_sha256"]}
            print(f"Evaluated reference clip {index + 1}", file=sys.stderr, flush=True)
    print(json.dumps({"clips": results, "provenance": {
        "config_hash": settings.fingerprint,
        "implementation_hash": result["implementation_hash"], "replay_sha256": result["replay_sha256"],
    }}))


if __name__ == "__main__":
    asyncio.run(main(*(Path(value) for value in sys.argv[1:])))

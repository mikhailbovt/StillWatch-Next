"""Extract deterministic, pixel-free observation traces from the locked Stillwatch tests."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import subprocess
import sys
from contextlib import ExitStack, contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
IDENTIFIERS = {"candidate_id", "episode_id", "previous_episode_id"}
POLICY_FIELDS = (
    "drop_ratio", "pre_drop_ratio", "drop_window_seconds", "low_pose_seconds",
    "low_aspect_ratio", "keypoint_confidence", "minimum_person_height", "lost_track_seconds",
    "recovery_seconds", "ordinary_activity_seconds", "fire_confidence", "fire_immediate_confidence",
    "dedup_seconds",
)


class Recorder:
    def __init__(self):
        self.test = None
        self.traces = {}
        self.routes = {}
        self.ids = {}
        self.in_frame = False

    @contextmanager
    def record(self, test):
        self.test = test
        self.routes = {}
        self.ids = {}
        try:
            yield
        finally:
            self.test = None
            self.routes.clear()

    def route(self, rules):
        if rules not in self.routes:
            self.routes[rules] = len(self.routes)
        return self.routes[rules]

    def normalize(self, value):
        if isinstance(value, list):
            return [self.normalize(item) for item in value]
        if isinstance(value, dict):
            normalized = {}
            for key, item in value.items():
                if key in IDENTIFIERS and item is not None:
                    if item not in self.ids:
                        self.ids[item] = len(self.ids) + 1
                    normalized[key] = self.ids[item]
                else:
                    normalized[key] = self.normalize(item)
            return normalized
        return value

    def append(self, value):
        if self.test is not None:
            self.traces.setdefault(self.test, []).append(value)

    def wrap(self, cls, category, patches):
        from stillwatch.tracking import PoseTracker

        update, reset = cls.update, cls.reset
        recorder = self

        def traced_update(rules, observations, ts, epoch):
            if recorder.test is None:
                return update(rules, observations, ts, epoch)
            input_observations = [dump_observation(o) for o in observations]
            recorder.in_frame = True
            try:
                result = update(rules, observations, ts, epoch)
            finally:
                recorder.in_frame = False
            frame = {
                "operation": "frame", "category": category, "route": recorder.route(rules),
                "source_seconds": ts, "epoch": epoch,
                "settings": {name: getattr(rules.settings, name) for name in POLICY_FIELDS},
                "observations": input_observations,
                "candidates": recorder.normalize([c.model_dump(mode="json") for c in result]),
            }
            if category == "medical":
                frame["recorded_association"] = not isinstance(rules.tracker, PoseTracker)
                frame["tracked_observations"] = [dump_observation(o) for o in rules.observations]
                frame["motion"] = recorder.normalize([m.model_dump(mode="json") for m in rules.motion.snapshot()])
            recorder.append(frame)
            return result

        def traced_reset(rules):
            result = reset(rules)
            if recorder.test is not None and not recorder.in_frame:
                recorder.append({"operation": "reset", "category": category, "route": recorder.route(rules)})
            return result

        patches.enter_context(patch.object(cls, "update", traced_update))
        patches.enter_context(patch.object(cls, "reset", traced_reset))


def dump_observation(observation):
    return observation.model_dump(mode="json") | {"torso_histogram": list(observation.torso_histogram)}


def clean_baseline(path, expected):
    def git(*args):
        return subprocess.check_output(["git", "-C", str(path), *args])

    if git("rev-parse", "HEAD").decode().strip() != expected:
        raise ValueError("Unexpected legacy revision")
    if git("status", "--porcelain", "--untracked-files=all", "--", "src", "tests", "uv.lock").strip():
        raise ValueError("Legacy source, tests and lock must match the pinned checkout")
    return hashlib.sha256(git("show", f"{expected}:uv.lock")).hexdigest()


def run(baseline, output):
    import pytest

    from stillwatch.triggers import FireRules, MedicalRules
    from stillwatch.types import Box, Observation
    from stillwatch.config import Settings

    recorder = Recorder()

    class Plugin:
        @pytest.hookimpl(wrapper=True)
        def pytest_runtest_protocol(self, item, nextitem):
            with recorder.record(item.nodeid):
                return (yield)

    with ExitStack() as patches, TemporaryDirectory(prefix="stillwatch-legacy-") as scratch:
        recorder.wrap(MedicalRules, "medical", patches)
        recorder.wrap(FireRules, "fire", patches)
        original = Path.cwd()
        os.chdir(baseline)
        try:
            result = pytest.main(["-q", "-p", "no:cacheprovider", "tests", "--basetemp", scratch], plugins=[Plugin()])
        finally:
            os.chdir(original)
        if result != pytest.ExitCode.OK:
            raise SystemExit(int(result))
        # Preserve a known defect as a differential fixture.
        with recorder.record("counterexample::fire_gaps_and_epochs"):
            rules = FireRules(Settings())
            box = Box(x1=0, y1=0, x2=40, y2=40)
            for ts, epoch in ((0, 1), (30, 1), (0, 2)):
                obs = Observation(capture_ts=ts, label="smoke", score=0.3, box=box)
                rules.update([obs], ts, epoch)

    metadata = json.loads((ROOT / "baseline.json").read_text(encoding="utf-8"))
    metadata = {"revision": metadata["revision"], "python_lock_sha256": metadata["python_lock_sha256"], "schema_version": 2}
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as stream, gzip.GzipFile(filename="", mode="wb", fileobj=stream, mtime=0) as compressed:
        for test, steps in sorted(recorder.traces.items()):
            row = {"baseline": metadata, "scenario": test, "steps": steps}
            compressed.write((json.dumps(row, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode())
    print(f"Recorded {len(recorder.traces)} scenarios and {sum(map(len, recorder.traces.values()))} steps")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    metadata = json.loads((ROOT / "baseline.json").read_text(encoding="utf-8"))
    baseline = args.baseline.resolve(strict=True)
    if clean_baseline(baseline, metadata["revision"]) != metadata["python_lock_sha256"]:
        raise ValueError("Legacy dependency lock digest changed")
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(baseline / "src"))
    run(baseline, args.output.resolve())


if __name__ == "__main__":
    main()

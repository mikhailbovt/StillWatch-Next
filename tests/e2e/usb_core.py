"""Check an explicitly named host camera through the fully masked probe path."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_core import run_core


def run(core, worker, name):
    with TemporaryDirectory(prefix="stillwatch-usb-") as temporary:
        root = Path(temporary)
        with run_core(core, worker, root / "data", root, root / "core.log") as (_, client):
            status, devices = client.call("GET", "/api/v2/capture-devices")
            assert status == 200, (status, devices)
            matching = [item for item in devices if item["name"] == name]
            assert len(matching) == 1, "Expected one explicitly named capture device"
            status, result = client.call("POST", "/api/v2/source-probes", {
                "source": {"type": "usb", "device_id": matching[0]["device_id"]},
                "privacy_masks": [{"left": 0, "top": 0, "right": 10000, "bottom": 10000}],
            })
            assert status == 200 and result["live"], (status, result)
            print(json.dumps({"device_probe": "passed", "mask": "entire_frame", "dimensions": result["dimensions"],
                              "native_pts": result["native_pts"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--device-name", required=True)
    args = parser.parse_args()
    run(args.core.resolve(strict=True), args.worker.resolve(strict=True), args.device_name)

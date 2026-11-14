"""Build frozen native Rust test executables before starting the Fast RC clock."""

import argparse
import json
from pathlib import Path
import subprocess

from fast_rc_contract import ROOT, sha256, write_json

PACKAGES = ("stillwatch-core", "sw-engine", "sw-domain", "sw-storage")


def code_identity():
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    status = subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True)
    if status:
        raise ValueError("Freeze the source checkout before building the logical test executables")
    return {"source_revision": revision, "cargo_lock_sha256": sha256(ROOT / "Cargo.lock")[7:]}


def run(output):
    source = code_identity()
    arguments = ["cargo", "test", "--release", "--locked", "--no-run", "--message-format=json"]
    for package in PACKAGES:
        arguments.extend(["-p", package])
    with subprocess.Popen(arguments, cwd=ROOT, stdout=subprocess.PIPE, text=True) as build:
        programs = {}
        for line in build.stdout:
            item = json.loads(line)
            if item.get("reason") != "compiler-artifact" or not item.get("executable") or not item["profile"]["test"]:
                continue
            crate = item["target"]["name"].replace("_", "-")
            if crate in PACKAGES:
                file = Path(item["executable"]).resolve(strict=True)
                if crate in programs:
                    raise ValueError(f"Multiple logical test executables for {crate}")
                programs[crate] = {"path": str(file), "digest": sha256(file)}
        if build.wait() != 0 or set(programs) != set(PACKAGES):
            raise ValueError("Logical test executable build did not cover all packages")
    if code_identity() != source:
        raise ValueError("The source checkout changed during the logical test build")
    write_json(output, {**source, "programs": programs})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    run(parser.parse_args().output)

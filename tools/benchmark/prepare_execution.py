"""Freeze the Core-readable execution profile for a Fast RC native workload."""

import argparse
from pathlib import Path

from fast_rc_contract import checked_json, contract_digests, execution_profile, sha256, validate_schema, write_json


def run(args):
    profile = checked_json(args.profile)
    validate_schema("profile", profile)
    if (profile["candidate_digest"] != sha256(args.candidate)
            or profile["contract_digests"] != contract_digests()
            or profile["workload"]["source_manifest_digest"] != sha256(args.source_manifest)):
        raise ValueError("The profile, candidate, sources and current qualification contracts must match")
    decoder = checked_json(args.capture_doctor)
    if decoder.get("rtsp_peer_confined") is not True:
        raise ValueError("The staged capture worker lacks RTSP peer confinement")
    execution = execution_profile(profile, checked_json(args.candidate), checked_json(args.source_manifest),
                                  checked_json(args.execution_host), decoder["decoder_identity"],
                                  args.raw_frame_bytes, args.cuda_device)
    args.output.mkdir(parents=True)
    path = args.output / "execution.json"
    write_json(path, execution)
    profile["execution_profile_digest"] = sha256(path)
    write_json(args.output / "profile.json", profile)
    print(profile["execution_profile_digest"])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("candidate", "profile", "source-manifest", "execution-host", "capture-doctor", "output"):
        parser.add_argument(f"--{arg}", required=True, type=Path)
    parser.add_argument("--raw-frame-bytes", type=int, default=256 * 1024**2)
    parser.add_argument("--cuda-device", type=int)
    try:
        run(parser.parse_args())
    except (OSError, KeyError, TypeError, ValueError) as error:
        parser.exit(2, f"{error}\n")

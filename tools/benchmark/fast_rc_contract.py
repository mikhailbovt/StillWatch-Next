"""File formats shared by the Fast RC collector, runner and validator."""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = "fast-rc-v2"
EXECUTION_PROTOCOL = "stillwatch-execution-v1"
CONTRACTS = {
    "native": ROOT / "qualification/fast-rc-v2.json",
    "quality": ROOT / "qualification/quality-contract.json",
    "release": ROOT / "qualification/release-requirements.json",
}
STAGES = ("preflight", "logical", "faults_and_capacity", "storage_and_crash", "quiet_load", "final")
DIGEST = re.compile(r"sha256:[a-f0-9]{64}")
RUN_ID = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}")
MAX_JSON = 32 * 1024 * 1024
DATABASE_PRESSURE_STATUS = {"sqlite_busy": 503, "pinned_wal": 507}
RELEASE_DIAGNOSTICS = {
    "trace-fire-policy": "core",
    "trace-pose-association": "core",
    "diagnose-fire-scale": "worker",
}
LOGIC_CASES = {
    "clock.source-monotonic-utc-boundaries": (
        ("stillwatch-core", "health::tests::host_clock_intervals_require_observed_stability"),
        ("sw-engine", "task_clock::tests::exact_sixty_seconds_keep_fire_phase_at_fifteen_and_ntsc_rates"),
        ("sw-engine", "task_clock::tests::extra_samples_skips_and_generation_changes_do_not_shift_base_phase"),
        ("sw-storage", "tests::scoped_alert_receipts_do_not_acknowledge_and_expired_alerts_do_not_replay"),
        ("sw-storage", "tests::refresh_replays_one_signed_rotation_across_reopen_and_revokes_new_proof_reuse"),
    ),
    "state.seeded-invariants": (
        ("sw-engine", "task_clock::tests::seeded_claims_reject_stale_replies_without_losing_new_work"),
        ("sw-domain", "fire::tests::gaps_and_epochs_cannot_corroborate_a_weak_detection"),
        ("sw-domain", "fire::tests::supported_pause_and_observed_absence_have_distinct_fire_outcomes"),
        ("sw-storage", "tests::a_cancelled_queued_command_does_not_mutate_and_retry_is_idempotent"),
        ("sw-storage", "tests::medical_recovery_retries_after_partial_evidence_and_checks_the_current_course"),
        ("sw-storage", "tests::new_source_epoch_can_link_but_never_inherits_prior_evidence_and_alerts"),
    ),
    "storage.incident-outbox-and-blob-cutpoints": (
        ("sw-storage", "tests::incident_change_and_outbox_rollback_together"),
        ("sw-storage", "tests::process_crash_around_incident_and_outbox_commit_is_idempotent"),
        ("sw-storage", "tests::evidence_commit_failure_leaves_a_reconcilable_orphan"),
        ("sw-storage", "tests::process_crash_around_jpeg_and_metadata_recovers_the_same_storage"),
        ("sw-storage", "tests::process_crash_during_retirement_keeps_live_references"),
        ("sw-storage", "tests::evidence_retention_tracks_references_across_restarts"),
        ("sw-storage", "tests::uncertain_blob_bytes_recount_in_maintenance_slices"),
        ("sw-storage", "tests::quota_partial_packet_and_missing_blob_are_visible_after_restart"),
        ("sw-storage", "tests::startup_recovers_pending_evidence_when_metadata_space_returns"),
    ),
    "storage.semantic-recovery-cutpoints": (
        ("sw-storage", "tests::semantic_consent_controls_claim_and_invalidates_inflight_evidence"),
        ("sw-storage", "tests::local_evidence_does_not_queue_disabled_semantics"),
        ("sw-storage", "tests::a_running_command_can_commit_after_a_completion_timeout"),
    ),
    "storage.backup-publish-cutpoint": (
        ("sw-storage", "tests::backup_crash_before_publish_reclaims_the_staged_snapshot"),
    ),
    "storage.restore-integrity-and-generation": (
        ("sw-storage", "tests::restore_checks_blob_integrity_and_rotates_stream_generation"),
    ),
}


def sha256(path):
    with path.open("rb") as stream:
        return "sha256:" + hashlib.file_digest(stream, "sha256").hexdigest()


def checked_execution_profile(path, profile, candidate, sources_path=None):
    if sha256(path) != profile.get("execution_profile_digest"):
        raise ValueError("The execution-profile digest differs from the frozen profile")
    data = checked_json(path)
    if data.get("protocol") == EXECUTION_PROTOCOL:
        if sources_path is None or sha256(sources_path) != profile["workload"]["source_manifest_digest"]:
            raise ValueError("The Core execution profile requires its frozen native source manifest")
        artifacts = data["artifacts"]
        device = artifacts["cuda"]["device"] if artifacts["cuda"] is not None else None
        expected = execution_profile(profile, candidate, checked_json(sources_path), data["host"],
                                     artifacts["decoder_identity"], data["raw_frame_bytes"], device)
        if data != expected:
            raise ValueError("The Core execution profile differs from the frozen native workload")
        return data
    expected = {
        "schema_version": 1,
        "id": profile["profile_id"],
        "candidate_digest": profile["candidate_digest"],
        "runtime_digest": "sha256:" + candidate["ort_sha256"],
        "native_lock_digest": "sha256:" + candidate["native_lock_sha256"],
        **{field: profile[field] for field in ("host", "models", "workload", "budgets")},
    }
    if data != expected:
        raise ValueError("The diagnostic profile describes a different native payload, host or workload")
    return None


def rtsp_workload(streams):
    classes = {(s["codec"], s["width"], s["height"], round(s["fps"] * 1000)) for s in streams}
    if len(classes) != 1:
        raise ValueError("Core admission requires one measured RTSP source class per stream type")
    (codec, width, height, millifps), = classes
    return {"kind": "rtsp_tcp", "codec": codec, "dimensions": {"width": width, "height": height},
            "min_millifps": millifps, "max_millifps": millifps}


def detail_output_dimensions(stream):
    source_width, source_height = stream["width"], stream["height"]
    width = max(2, min(1280, source_width) // 2 * 2)
    height = (source_height * width + source_width) // (2 * source_width) * 2
    return [width, height]


def cuda_graph_shape(model):
    return model["tensor_shapes"][0][-2:] if model.get("cuda_graph") else None


def execution_profile(profile, candidate, manifest, host, decoder, raw_bytes, cuda_device):
    model, workload, budgets = (profile[key] for key in ("models", "workload", "budgets"))
    if workload["semantic_route"] != "disabled":
        raise ValueError("A semantic route requires its own native workload collector")
    sources = manifest["sources"][:workload["camera_count"]]
    if len(sources) != workload["camera_count"]:
        raise ValueError("The inventory does not cover the declared camera workload")
    source = rtsp_workload(sources)
    width, height = (source["dimensions"][axis] for axis in ("width", "height"))
    main_streams = {entry["id"]: entry["detail"] for entry in sources if "detail" in entry}
    detail_viewers = {entry["camera"] for entry in manifest["viewers"] if entry["profile"] == "detail"}
    if not main_streams.keys() <= detail_viewers or len(main_streams) > workload["detail_publishers"]:
        raise ValueError("Each declared main stream needs a detail viewer in the frozen workload")
    detail_source = rtsp_workload(main_streams.values()) if main_streams else None
    detail_decoders = len(main_streams)
    if type(raw_bytes) is not int or not width * height * 9 * len(sources) <= raw_bytes <= 2**31:
        raise ValueError("Raw-frame capacity does not cover the declared sources")
    if not isinstance(decoder, str) or not 1 <= len(decoder) <= 256:
        raise ValueError("The capture worker did not report a decoder identity")
    locked = profile["host"]
    host_fields = ("architecture", "os_release", "cpu", "logical_cores", "ram_bytes")
    if host != {"platform": locked["os"], **{key: locked[key] for key in host_fields},
                "accelerator": host["accelerator"]}:
        raise ValueError("Core host fingerprint differs from the frozen profile")
    accelerator = host["accelerator"]
    gpu = locked["accelerator"]
    if (accelerator is None) != (gpu is None) or (accelerator and (
            accelerator["name"] != gpu["name"] or accelerator["driver"] != gpu["driver"]
            or accelerator["memory_mib"] * 1024**2 != gpu["vram_bytes"])):
        raise ValueError("Core accelerator differs from the frozen profile")
    windows = locked["os"] == "windows"
    ext = ".exe" if windows else ""
    files = candidate["files"]
    cuda = None
    if model["backend"] == "cuda_fp32":
        if type(cuda_device) is not int or not 0 <= cuda_device <= 65535:
            raise ValueError("A CUDA profile must identify the measured device")
        provider, shared = ("onnxruntime_providers_cuda.dll", "onnxruntime_providers_shared.dll") if windows else (
            "libonnxruntime_providers_cuda.so", "libonnxruntime_providers_shared.so")
        cuda = {"device": cuda_device, "provider": files["runtime/" + provider], "shared": files["runtime/" + shared]}
        if (shape := cuda_graph_shape(model)) is not None:
            gain = min(640 / width, 640 / height)
            source_shape = [(max(1, round(side * gain)) + 31) // 32 * 32 for side in (height, width)]
            if shape != source_shape:
                raise ValueError("The captured CUDA graph shape must match the native analysis sources")
            cuda["graph_shape"] = shape
    preview, detail = (workload[key] for key in ("preview_publishers", "detail_publishers"))
    return {
        "protocol": EXECUTION_PROTOCOL, "id": profile["profile_id"],
        "candidate_sha256": profile["candidate_digest"][7:], "native_contract_sha256": profile["contract_digests"]["native"][7:],
        "host": host, "inference_workers": model["workers"], "inference_threads": model["threads_per_worker"],
        "artifacts": {"core": files["bin/stillwatch-core" + ext], "worker": files["bin/stillwatch-worker" + ext],
                      "mediamtx": candidate["mediamtx_sha256"], "encoder": files["encoder/ffmpeg" + ext],
                      "model_manifest": candidate["model_manifest_sha256"], "ort": candidate["ort_sha256"],
                      "pose_balanced": model["roles"]["person"]["digest"][7:], "fire": model["roles"]["hazard"]["digest"][7:],
                      "cuda": cuda, "decoder_identity": decoder},
        "work": {"cameras": len(sources), "source": source,
                 "sampling": {key: workload[key] for key in ("pose_fps", "fire_fps", "evidence_fps")},
                 "min_pose_per_wall_minute": workload["pose_fps"] * 57,
                 "min_fire_per_wall_minute": workload["fire_fps"] * 57, "detail_source": detail_source,
                 "media": {"preview_publishers": preview, "detail_publishers": detail, "detail_decoders": detail_decoders,
                           "viewer_sessions": workload["viewer_sessions"], "publisher_units": preview + detail + detail_decoders,
                           "preview_units": 1, "detail_units": 1, "detail_decoder_units": int(bool(detail_decoders)), "snapshot_encoders": 1,
                           "snapshot_requests_per_minute": workload["snapshot_requests_per_minute"]}},
        "tensor_shapes": model["tensor_shapes"],
        "semantic": None if workload["semantic_route"] == "disabled" else workload["semantic_route"],
        "raw_frame_bytes": raw_bytes, "detail_frame_bytes": 128 * 1024**2,
        "evidence_quota_bytes": 500 * 1024**2, "database_quota_bytes": 2 * 1024**3,
        "limits": {"peak_rss_bytes": budgets["rss_bytes"] - budgets["headroom_bytes"]["rss"],
                   "peak_vram_mib": (budgets["vram_bytes"] - budgets["headroom_bytes"]["vram"]) // 1024**2,
                   "peak_disk_bytes": budgets["disk_bytes"] - budgets["headroom_bytes"]["disk"],
                   "maximum_children": budgets["max_children"], "maximum_open_sockets": budgets["max_open_sockets"]},
    }


def checked_json(path):
    def unique(items):
        obj = dict(items)
        if len(obj) != len(items):
            raise ValueError(f"Duplicate JSON key in {path.name}")
        return obj

    def invalid_constant(value):
        raise ValueError(f"Invalid number {value} in {path.name}")

    with path.open("rb") as file:
        payload = file.read(MAX_JSON + 1)
    if len(payload) > MAX_JSON:
        raise ValueError(f"JSON exceeds the size limit: {path.name}")
    return json.loads(payload, object_pairs_hook=unique, parse_constant=invalid_constant)


def write_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as file:
        json.dump(value, file, indent=2, allow_nan=False)
        file.write("\n")


def member(root, value):
    path = PurePosixPath(value)
    if (not path.parts or path.is_absolute() or ".." in path.parts or "\\" in value
            or ":" in value or path.as_posix() != value):
        raise ValueError(f"Invalid evidence or artifact path: {value!r}")
    candidate = root.joinpath(*path.parts)
    if not candidate.is_file() or candidate.resolve(strict=True) != candidate:
        raise ValueError(f"Missing or redirected evidence or artifact: {value}")
    return candidate


def evidence(root, path):
    relative = path.relative_to(root).as_posix()
    return {"path": relative, "digest": sha256(member(root, relative))}


def checked_evidence(root, record):
    if not isinstance(record, dict) or set(record) != {"path", "digest"} or not DIGEST.fullmatch(record["digest"]):
        raise ValueError("Invalid evidence reference")
    path = member(root, record["path"])
    if sha256(path) != record["digest"]:
        raise ValueError(f"Evidence digest differs: {record['path']}")
    return path


def number(value):
    return type(value) in (int, float) and value >= 0 and math.isfinite(value)


def fresh_opening(event, start_ms):
    observed = event.get("opening_observed_host_utc_ms")
    committed = event.get("utc_ms")
    return (type(observed) is int and type(committed) is int and 0 < start_ms <= observed <= committed
            and event.get("opening_delayed") is False)


def capture_progressed(before, after):
    return (after["capture_state"] == "streaming" and int(after["epoch"]) == int(before["epoch"])
            and int(after["frame"]) > int(before["frame"]))


def camera_recovered(before, after, epoch_changes):
    analysis = after["analysis"]
    if after["capture_state"] != "streaming" or not analysis or analysis["state"] != "ready":
        return False
    tasks = ("pose_frames", "fire_frames")
    if epoch_changes:
        return int(after["epoch"]) > int(before["epoch"]) and all(int(analysis[key]) > 0 for key in tasks)
    return (capture_progressed(before, after) and bool(before["analysis"])
            and all(int(analysis[key]) > int(before["analysis"][key]) for key in tasks))


def camera_quiet(snapshot):
    analysis = snapshot["analysis"] or {}
    return (snapshot["capture_state"] == "streaming" and analysis.get("state") == "ready"
            and analysis.get("fire_regions") == analysis.get("smoke_regions") == 0)


def quiet_requirements(case):
    quiet = case.get("require_quiet_before_fault", False)
    duration = case.get("quiet_source_ms", 0)
    if (type(quiet) is not bool or (quiet and case["kind"] != "viewer_grant_offer_close")
            or type(duration) is not int or not 0 <= duration <= 10_000 or (duration and not quiet)):
        raise ValueError("Invalid viewer quiet-scene precondition")
    return quiet, duration * 1_000


def quiet_fire_sample(snapshot):
    analysis = snapshot["analysis"] or {}
    epoch = snapshot["epoch"]
    if (not camera_quiet(snapshot) or analysis.get("source_epoch") != epoch
            or analysis.get("last_fire_source_us") is None):
        return None
    return epoch, int(analysis["fire_frames"]), int(analysis["last_fire_source_us"])


def consecutive_quiet_samples(previous, current):
    return (previous[0] == current[0] and current[1] == previous[1] + 1
            and 0 < current[2] - previous[2] <= 1_500_000)


def verified_quiet_window(snapshots, minimum_us):
    if not 2 <= len(snapshots) <= 32:
        return False
    samples = [quiet_fire_sample(row) for row in snapshots]
    return (all(sample is not None for sample in samples)
            and all(consecutive_quiet_samples(a, b) for a, b in zip(samples, samples[1:]))
            and samples[-1][2] - samples[0][2] >= minimum_us)


def inference_worker_replaced(fault, count):
    before = {tuple(identity) for identity in fault["workers_before"]}
    after = {tuple(identity) for identity in fault["workers_after"]}
    retired = tuple(fault["retired_worker"])
    if not all(type(pid) is int and pid > 0 and number(created) and created > 0 for pid, created in before | after):
        return False
    return len(before) == len(after) == count and before - after == {retired} and len(after - before) == 1


def inference_camera_interrupted(before, during):
    return ((before["analysis"] or {}).get("state") == "ready" and during["capture_state"] == "streaming"
            and during["epoch"] == before["epoch"] and not during.get("source_lagging")
            and (during["analysis"] or {}).get("state") in {"degraded", "starting"})


def inference_cameras_recovered(recovery, names):
    before, interrupted, after = (recovery[key] for key in ("before", "interrupted", "after"))
    if set(before) != names or set(after) != names or not interrupted or not set(interrupted) <= names:
        return False
    return (all(camera_recovered(before[name], after[name], False) for name in names)
            and all(inference_camera_interrupted(before[name], during) and camera_recovered(during, after[name], False)
                    for name, during in interrupted.items()))


def contract_digests():
    return {name: sha256(path) for name, path in CONTRACTS.items()}


def validate_schema(name, data):
    from jsonschema import Draft202012Validator

    schema = checked_json(ROOT / "qualification" / "schemas" / f"{name}-v2.schema.json")
    invalid = next(Draft202012Validator(schema).iter_errors(data), None)
    if invalid is not None:
        location = "/".join(map(str, invalid.absolute_path)) or "root"
        raise ValueError(f"Invalid {name} at {location}: {invalid.message}")

"""File formats shared by the Fast RC collector, runner and validator."""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = "fast-rc-v2"
CONTRACTS = {
    "native": ROOT / "qualification/fast-rc-v2.json",
    "quality": ROOT / "qualification/quality-contract.json",
    "release": ROOT / "qualification/release-requirements.json",
}
STAGES = ("preflight", "logical", "faults_and_capacity", "storage_and_crash", "quiet_load", "final")
DIGEST = re.compile(r"sha256:[a-f0-9]{64}")
RUN_ID = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}")
MAX_JSON = 32 * 1024 * 1024
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
        ("sw-storage", "tests::evidence_commit_failure_leaves_a_reconcilable_orphan"),
        ("sw-storage", "tests::process_crash_around_jpeg_and_metadata_recovers_the_same_storage"),
        ("sw-storage", "tests::evidence_retention_tracks_references_across_restarts"),
        ("sw-storage", "tests::uncertain_blob_bytes_recount_in_maintenance_slices"),
        ("sw-storage", "tests::quota_partial_packet_and_missing_blob_are_visible_after_restart"),
        ("sw-storage", "tests::startup_recovers_pending_evidence_when_metadata_space_returns"),
    ),
    "storage.semantic-recovery-cutpoints": (
        ("sw-storage", "tests::semantic_consent_controls_claim_and_invalidates_inflight_evidence"),
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


def checked_execution_profile(path, profile, candidate):
    if sha256(path) != profile.get("execution_profile_digest"):
        raise ValueError("The execution-profile digest differs from the frozen profile")
    expected = {
        "schema_version": 1,
        "id": profile["profile_id"],
        "candidate_digest": profile["candidate_digest"],
        "runtime_digest": "sha256:" + candidate["ort_sha256"],
        "native_lock_digest": "sha256:" + candidate["native_lock_sha256"],
        **{field: profile[field] for field in ("host", "models", "workload", "budgets")},
    }
    if checked_json(path) != expected:
        raise ValueError("The execution profile describes a different native payload, host or workload")


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


def contract_digests():
    return {name: sha256(path) for name, path in CONTRACTS.items()}


def validate_schema(name, data):
    from jsonschema import Draft202012Validator

    schema = checked_json(ROOT / "qualification" / "schemas" / f"{name}-v2.schema.json")
    invalid = next(Draft202012Validator(schema).iter_errors(data), None)
    if invalid is not None:
        location = "/".join(map(str, invalid.absolute_path)) or "root"
        raise ValueError(f"Invalid {name} at {location}: {invalid.message}")

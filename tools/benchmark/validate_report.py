"""Validate one immutable Fast RC v2 run bundle and issue its release verdict."""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import errno
import hashlib
import json
import math
from pathlib import Path
import re
import statistics
import sys
from uuid import UUID

from fast_rc_contract import (
    CONTRACTS, DATABASE_PRESSURE_STATUS, DIGEST, DURATION_CLOCKS, LOGIC_CASES, PROTOCOL,
    RELEASE_DIAGNOSTICS, ROOT, RUN_ID, STAGES,
    camera_quiet, camera_recovered, capture_progressed, checked_evidence, checked_execution_profile, checked_json, contract_digests,
    cuda_graph_shape, event_regression_cases, evidence_location, fresh_opening,
    inference_cameras_recovered, inference_worker_replaced, member, number, quiet_requirements, sha256,
    validate_schema, verified_quiet_window, write_json,
)
from validate_steady import Steady, resource_caps, upper_growth

MODEL_KEYS = ("artifact_validation", "active_profile_numeric", "active_profile_observations",
              "active_profile_events", "full_pack_numeric")
CHECKS = {
    "evaluator.censoring-coverage-and-grounding": "event_scoring",
    "storage.refresh-lost-response-and-restart": "lan_refresh",
    "storage.pre-aged-retention-at-volume": "retention",
    "storage.pinned-wal-reader": "pinned_wal",
    "storage.sqlite-quota-and-busy": "sqlite_pressure",
    "capacity.ramp-and-concurrent-admission": "capacity_ramp",
    "capacity.overload-and-recovery": "capacity_overload",
}
FAULTS = {
    "source.reconnect-completed": "rtsp_reconnect",
    "worker.inference-restart-completed": "inference_child_restart",
    "service.core-restart-completed": "core_restart",
    "media.viewer-cycle-completed": "viewer_grant_offer_close",
}


def canary_matches(incidents, planned, start_ms):
    if any("opening_observed_host_utc_ms" not in incident for incident in incidents):
        raise ValueError("Canary evidence does not record when the opening frame was observed")
    return [incident for incident in incidents
            if all(incident.get(key) == planned[key] for key in ("category", "camera"))
            and int(incident["source_epoch"]) == int(planned["source_epoch"])
            and planned["earliest_source_us"] <= int(incident["source_us"]) <= planned["latest_source_us"]
            and fresh_opening(incident, start_ms) and incident["live"] is True
            and incident["evidence_state"] == "persisted" and incident["acknowledged"] is False
            and incident["utc_ms"] <= incident["receipt_utc_ms"]]


def canary_overview_matches(raw_path, raw, incident):
    try:
        image = raw["overview"]
        artifact = image["artifact"]
        location = evidence_location(artifact)
        if (image["incident_id"] != incident["id"] or int(image["source_epoch"]) != int(incident["source_epoch"])
                or str(UUID(image["packet_id"])) != image["packet_id"]
                or type(image["ordinal"]) is not int or not 0 <= image["ordinal"] < 16
                or artifact["digest"] != f"sha256:{image['sha256']}"
                or location != raw_path.with_suffix(".jpg").name):
            return False
        path = member(raw_path.parent, location)
        with path.open("rb") as handle:
            jpeg = handle.read(512 * 1024 + 1)
        return (4 <= len(jpeg) <= 512 * 1024 and len(jpeg) == image["bytes"]
                and hashlib.sha256(jpeg).hexdigest() == image["sha256"]
                and jpeg.startswith(b"\xff\xd8") and jpeg.endswith(b"\xff\xd9"))
    except (AttributeError, KeyError, OSError, TypeError, ValueError):
        return False


def persisted_blobs_clean(checks):
    return (checks.get("integrity") == ["ok"] and checks.get("invalid_references") == 0
            and checks.get("duplicate_attention_cycles") == 0 and checks.get("verified_blobs", 0) > 0
            and checks["expected_blobs"] == checks["verified_blobs"])


class Validation:
    def __init__(self, root):
        self.root = root.resolve(strict=True)
        self.contract = checked_json(CONTRACTS["native"])
        self.manifest_path = member(self.root, "run-manifest.json")
        self.run = checked_json(self.manifest_path)
        self.profile = self.input("profile")
        validate_schema("profile", self.profile)
        if "elapsed_clock" in self.run and self.run["elapsed_clock"] != DURATION_CLOCKS[self.profile["host"]["os"]]:
            raise ValueError("Run elapsed clock differs from the frozen host platform")
        self.candidate = self.input("candidate")
        self.problems = list(self.run["stage_errors"])
        if self.candidate.get("source_dirty") is True:
            self.problems.append("Candidate was staged from a modified source checkout")
        self.results = []
        self.models = dict.fromkeys(MODEL_KEYS, "not_run")
        self.models["observation_spec_sha256"] = sha256(ROOT / "qualification/observation-parity-v1.json")[7:]
        self.backend = self.profile["models"]["backend"]
        self.steady = None
        self.core_profile = None
        if (self.run["protocol"] != PROTOCOL or not RUN_ID.fullmatch(self.run["run_id"])
                or self.run["candidate_digest"] != self.profile["candidate_digest"]
                or self.run["contract_digests"] != contract_digests()
                or self.profile["contract_digests"] != self.run["contract_digests"]
                or self.profile["protocol"] != PROTOCOL):
            raise ValueError("Run inputs do not match the Fast RC v2 contracts and pinned candidate")
        if not number(self.run["native_wall_seconds"]):
            raise ValueError("Native run duration is missing or invalid")
        for key in ("source_video_seconds", "virtual_clock_span_seconds"):
            if self.run.get(key) is not None and not number(self.run[key]):
                raise ValueError(f"Invalid {key}")
        expected = {case for stage in STAGES for case in self.contract["cases"][stage]}
        unknown = set(self.run["cases"]) - expected
        if unknown:
            raise ValueError(f"Unknown Fast RC case IDs: {sorted(unknown)}")

    def input(self, name):
        path = checked_evidence(self.root, self.run["inputs"][name])
        if sha256(path) != self.run[name + "_digest"]:
            raise ValueError(f"The frozen {name} digest differs")
        return checked_json(path)

    def require_elapsed_clock(self, measured):
        if "elapsed_clock" in self.run and measured.get("elapsed_clock") != self.run["elapsed_clock"]:
            raise ValueError("The collector used a different elapsed clock from the run")

    def read_case(self, case, stage):
        item = self.run["cases"].get(case)
        if item is None:
            return None, [], "NOT_RUN"
        if not isinstance(item, dict) or set(item) != {"stage", "proof"} or item["stage"] != stage:
            raise ValueError("Invalid case record or wrong stage")
        record = item["proof"]
        proof = checked_json(checked_evidence(self.root, record))
        identity = (proof["protocol"], proof["run_id"], proof["candidate_digest"], proof["profile_digest"])
        if identity != (PROTOCOL, self.run["run_id"], self.run["candidate_digest"], self.run["profile_digest"]):
            raise ValueError("Case evidence belongs to a different run, candidate or profile")
        if proof["case"] != case or not number(proof["started_s"]) or not number(proof["finished_s"]):
            raise ValueError("Bad case identity or negative time")
        if not proof["started_s"] <= proof["finished_s"] <= self.run["native_wall_seconds"]:
            raise ValueError("Case time is outside the measured run")
        records = [record, *proof["evidence"]]
        for source in proof["evidence"]:
            checked_evidence(self.root, source)
        if not proof["evidence"]:
            raise ValueError("Case has no raw evidence")
        ceiling = self.contract["stage_boundaries_seconds"][stage][1]
        pending = proof["finished_s"] > ceiling or proof.get("deadline_exceeded") is True
        return proof, records, "INCOMPLETE" if pending else None

    def artifact_check(self, proof):
        observations = proof["observations"]
        files = self.candidate["files"]
        if not isinstance(files, dict) or not files or len(files) > 2048:
            raise ValueError("Candidate inventory is empty or too large")
        platform_name = self.profile["host"]["os"]
        expected = {"verified_artifacts": len(files), "target_os": platform_name, "clean_source_revision": True,
                    "release_diagnostic_commands": []}
        manifest = self.candidate.get("target", "")
        layout = self.profile["host"].get("architecture") == "x86_64" and platform_name in manifest
        models = self.profile["models"]
        roles_match = {role: model["id"] for role, model in models["roles"].items()} == {"person": "pose_balanced", "hazard": "fire"}
        models_match = models["manifest_digest"] == "sha256:" + str(self.candidate.get("model_manifest_sha256")) and all(
            model["digest"] == "sha256:" + str(files.get(f"models/{model['id']}.onnx")) for model in models["roles"].values())
        exact = layout and observations["actual_digests"] == files and observations["unexpected"] == []
        if not exact:
            changed = sorted(name for name, digest in files.items()
                             if observations["actual_digests"].get(name) != digest)
            return expected, {"differing_artifacts": changed, "unexpected": observations["unexpected"],
                              "target_os": observations["target_os"], "inventory_matches": False}, "FAIL"
        command_lines = self.read_text(self.artifact(proof, "worker-usage")) + self.read_text(self.artifact(proof, "core-help"))
        actual_host = checked_json(self.artifact(proof, "execution-host"))
        locked_host = self.profile["host"]
        host_fields = ("architecture", "os_release", "cpu", "logical_cores", "ram_bytes")
        host_matches = (actual_host["platform"] == locked_host["os"]
                        and all(actual_host[key] == locked_host[key] for key in host_fields))
        if "core_runtime_threads" in locked_host:
            host_matches = host_matches and actual_host.get("core_runtime_threads") == locked_host["core_runtime_threads"]
        acceleration = actual_host["accelerator"]
        if locked_host["accelerator"] is None:
            host_matches = host_matches and acceleration is None
        elif acceleration is not None:
            locked_gpu = locked_host["accelerator"]
            host_matches = (host_matches and acceleration["name"] == locked_gpu["name"]
                            and acceleration["driver"] == locked_gpu["driver"]
                            and acceleration["memory_mib"] * 1024 * 1024 == locked_gpu["vram_bytes"])
        else:
            host_matches = False
        decoder_matches = None
        if self.core_profile is not None:
            decoder = checked_json(self.artifact(proof, "capture-doctor"))
            decoder_matches = (decoder.get("rtsp_peer_confined") is True
                               and decoder.get("decoder_identity") == self.core_profile["artifacts"]["decoder_identity"])
            host_matches = host_matches and actual_host == self.core_profile["host"]
        observed = {
            "verified_artifacts": len(observations["actual_digests"]),
            "target_os": observations["target_os"],
            "clean_source_revision": self.candidate.get("source_dirty") is False
            and re.fullmatch(r"[a-f0-9]{40}", str(self.candidate.get("source_revision"))) is not None,
            "release_diagnostic_commands": sorted(set(re.findall(r"\b(?:diagnose-fire-scale|trace-fire-policy|trace-pose-association)\b", command_lines))),
        }
        valid = (host_matches and decoder_matches is not False and roles_match and models_match and observed == expected
                 and "verify-model-pack" in command_lines and "--data-dir" in command_lines)
        observed["inventory_matches"] = exact
        observed["host_matches"] = host_matches
        observed["decoder_matches"] = decoder_matches
        observed["active_roles_match"] = roles_match and models_match
        if not valid:
            return expected, observed, "FAIL"
        if "release_diagnostic_exit_codes" not in observations:
            return expected, observed, "INCOMPLETE"
        open_commands, missing = [], []
        exits = observations["release_diagnostic_exit_codes"]
        for name, tool in RELEASE_DIAGNOSTICS.items():
            code = exits.get(name)
            if code is None:
                missing.append(name)
                continue
            transcript = self.read_text(self.artifact(proof, f"reject-{name}"))
            rejected = (code == 2 and f"unexpected argument '{name}'" in transcript if tool == "core"
                        else code == 1 and "usage: stillwatch-worker decode-file" in transcript)
            if not rejected:
                open_commands.append(name)
        observed["release_diagnostic_commands"] = open_commands
        observed["release_diagnostic_probes_missing"] = missing
        result = "FAIL" if open_commands else "INCOMPLETE" if missing else "PASS"
        return expected, observed, result

    def artifact(self, proof, role):
        roles = proof.get("artifacts", {})
        record = roles[role]
        if record not in proof["evidence"]:
            raise ValueError(f"Missing evidence for {role}")
        return checked_evidence(self.root, record)

    @staticmethod
    def read_text(path):
        with path.open("rb") as stream:
            return stream.read(2 * 1024 * 1024).decode("utf-8", errors="replace")

    def model_check(self, case, proof):
        data = checked_json(self.artifact(proof, "parity-output"))
        active = case == "models.active-numeric-observation-event-gates"
        expected = {"provider": "cuda" if self.backend == "cuda_fp32" else "cpu", "observation_spec_sha256": self.models["observation_spec_sha256"],
                    "artifact_validation": "pass", "cuda_graph_shape": cuda_graph_shape(self.profile["models"])}
        observed = {key: data.get(key) for key in expected}
        for model in ("pose", "pose_balanced", "fire"):
            if not isinstance(data.get("models", {}).get(model), dict) or not data["models"][model]:
                raise ValueError(f"Missing numerical fixture results for {model}")
            if expected["cuda_graph_shape"] is not None:
                executions = data["models"][model].values()
                captured_images = sum(item.get("cuda_graph") is True and "image" in item for item in executions)
                if (model == "pose" and any(item.get("cuda_graph") is True for item in executions)
                        or model != "pose" and captured_images < 2):
                    raise ValueError(f"Missing locked CUDA graph observations for {model}")
        if observed != expected or data.get("full_pack_numeric") not in {"pass", "fail"}:
            return expected, observed, "FAIL"
        for key in MODEL_KEYS:
            if key != "active_profile_events":
                self.models[key] = data[key]
        if not active:
            expected["full_pack_numeric"] = ["pass", "fail"] if self.backend == "cuda_fp32" else ["pass"]
            observed["full_pack_numeric"] = data["full_pack_numeric"]
            observed["numeric_failures"] = data["numeric_failures"]
            acceptable = observed["full_pack_numeric"] in expected["full_pack_numeric"]
            return expected, observed, "PASS" if acceptable else "FAIL"
        gates = ("active_profile_numeric", "active_profile_observations", "active_profile_events")
        expected.update(dict.fromkeys(gates, "pass"))
        if "event-output" in proof.get("artifacts", {}):
            _, _, verdict = self.event_regression_check(proof)
            self.models["active_profile_events"] = {"PASS": "pass", "FAIL": "fail", "INCOMPLETE": "not_run"}[verdict]
        observed.update({key: self.models[key] for key in gates})
        if "fail" in (observed[key] for key in gates):
            verdict = "FAIL"
        elif observed == expected:
            verdict = "PASS"
        else:
            verdict = "INCOMPLETE"
        return expected, observed, verdict

    def event_regression_check(self, proof):
        pin = self.run["inputs"].get("event_parity")
        if (pin is None or proof["artifacts"]["event-output"] != pin
                or self.run["event_parity_digest"] != pin["digest"]):
            raise ValueError("The regression case does not use the frozen event parity evidence")
        return self.read_event_parity(checked_json(self.artifact(proof, "event-output")), proof)

    def read_event_parity(self, events, proof):
        binding = {"candidate_digest": self.run["candidate_digest"], "profile_digest": self.run["profile_digest"],
                   "policy_version": self.profile["models"]["policy_version"]}
        if not isinstance(events, dict) or any(events.get(name) != value for name, value in binding.items()):
            raise ValueError("The measured event parity belongs to a different configuration")
        required, cases = event_regression_cases(events)
        passed, failed = set(), set()
        for name, item in cases.items():
            reference, observed = item.get("reference"), item.get("observed")
            if not (isinstance(reference, dict) and reference and isinstance(observed, dict) and observed):
                continue
            sources = item.get("evidence", [])
            if not sources:
                continue
            for record in sources:
                role = "event-source/" + evidence_location(record)
                attachment = {"path": "inputs/" + role, "digest": record["digest"]}
                if proof["artifacts"].get(role) != attachment:
                    raise ValueError("A model regression source differs from the frozen ledger")
                self.artifact(proof, role)
            target = json.dumps(reference, sort_keys=True, allow_nan=False)
            measured = json.dumps(observed, sort_keys=True, allow_nan=False)
            (passed if target == measured else failed).add(name)
        missing = required - passed - failed
        expected = {"regressions": sorted(required)}
        observed = {"passed": sorted(passed), "failed": sorted(failed), "not_run": sorted(missing)}
        return expected, observed, "FAIL" if failed else "INCOMPLETE" if missing else "PASS"

    def warmup_check(self, proof):
        model = self.profile["models"]
        operator = "cuda" if self.backend == "cuda_fp32" else "cpu"
        capture = cuda_graph_shape(model)
        limits = self.contract["deadlines_ms"]
        expected = {"shapes": sorted(shape[-2:] for shape in model["tensor_shapes"]),
                    "threads": model["threads_per_worker"], "provider": operator,
                    "cuda_graph_shape": capture,
                    "model_contract_schema": 3, "compatible_policy": model["policy_version"],
                    "model_roles": {role: item["id"] for role, item in model["roles"].items()},
                    "startup_ms_maximum": limits[operator + "_worker_startup"],
                    "inference_ms_maximum": limits["live_inference"]}
        transcript = self.read_text(self.artifact(proof, "stdout"))
        rows = self.json_lines(transcript)
        code = proof["observations"]["exit_code"]
        if not rows:
            result = "FAIL" if code is not None and code != 0 and not proof.get("deadline_exceeded") else "INCOMPLETE"
            return expected, {"exit_code": code}, result
        measured = rows[-1]
        host = measured["host_input"]
        observed = {"shapes": sorted(measured["warm_shapes"]), "threads": measured["threads"],
                    "provider": measured["device"], "startup_ms": host["startup_ms"],
                    "cuda_graph_shape": measured.get("cuda_graph_shape"),
                    "requests": host["requests"], "max_reply_ms": host["max_reply_ms"], "exit_code": code,
                    "unapproved_roles_and_shapes_rejected": measured["unapproved_roles_and_shapes_rejected"]}
        worker_name = "bin/stillwatch-worker.exe" if self.profile["host"]["os"] == "windows" else "bin/stillwatch-worker"
        exact = (measured["worker_sha256"] == self.candidate["files"][worker_name]
                 and measured["runtime_sha256"] == self.candidate["ort_sha256"]
                 and "sha256:" + measured["model_manifest_sha256"] == model["manifest_digest"])
        valid = (exact and code == 0 and all(observed[key] == expected[key] for key in ("shapes", "threads", "provider", "cuda_graph_shape"))
                 and (capture is None or measured.get("graph_observations_equal") is True)
                 and number(observed["startup_ms"]) and number(observed["max_reply_ms"])
                 and observed["startup_ms"] <= expected["startup_ms_maximum"]
                 and observed["max_reply_ms"] <= expected["inference_ms_maximum"]
                 and observed["requests"] >= len(expected["shapes"]) and observed["unapproved_roles_and_shapes_rejected"] is True)
        if not valid:
            return expected, observed, "FAIL"
        if "model-lock" not in proof["artifacts"]:
            return expected, observed, "INCOMPLETE"
        lock = self.artifact(proof, "model-lock")
        if sha256(lock) != model["manifest_digest"]:
            raise ValueError("Native warmup evidence includes a different model lock")
        manifest = checked_json(lock)
        observed["model_contract_schema"] = manifest["schema_version"]
        if observed["model_contract_schema"] != 3:
            return expected, observed, "INCOMPLETE"
        contract = manifest["contract"]
        observed.update({"compatible_policy": contract["compatible_policy"], "model_roles": contract["roles"],
                         "model_contract_shapes": contract["tensor_shapes"]})
        digests = {role: "sha256:" + manifest["models"][name]["sha256"] for role, name in contract["roles"].items()}
        inventory = observed["model_contract_shapes"]
        shapes_match = capture in inventory if capture is not None else sorted(inventory) == expected["shapes"]
        matches = (observed["compatible_policy"] == expected["compatible_policy"]
                   and observed["model_roles"] == expected["model_roles"]
                   and digests == {role: item["digest"] for role, item in model["roles"].items()}
                   and shapes_match
                   and contract["observation_spec_sha256"] == self.models["observation_spec_sha256"])
        return expected, observed, "PASS" if matches else "FAIL"

    @staticmethod
    def json_lines(logfile):
        lines = []
        for line in logfile.splitlines():
            if line.startswith("{"):
                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(data, dict):
                    lines.append(data)
        return lines

    def smoke_check(self, case, proof):
        logfile = self.read_text(self.artifact(proof, "stdout"))
        expected = {"exit_code": 0, "checks": CHECKS[case]}
        code = proof["observations"]["exit_code"]
        if CHECKS[case] == "event_scoring":
            match = re.search(r"Ran ([0-9]+) tests? in", logfile)
            observed = {"exit_code": code, "tests": int(match.group(1)) if match else 0}
            expected["minimum_tests"] = 6
            complete = observed["tests"] >= expected["minimum_tests"] and re.search(r"(?m)^OK\s*$", logfile)
        else:
            lines = self.json_lines(logfile)
            observed = {"exit_code": code, "samples": lines}
            if CHECKS[case] == "lan_refresh":
                complete = any(row.get("lost_response_restart_and_concurrent_replay") is True
                               and row.get("ed25519_pairing") is True for row in lines)
            elif CHECKS[case] == "pinned_wal":
                complete = any(row.get("idempotent_retry_after_wal_recovery") is True
                               and row.get("incidents_readable_in_storage_pressure") is True
                               and row.get("wal_bytes_with_reader", 0) >= 16 * 1024 * 1024 for row in lines)
            elif CHECKS[case] == "sqlite_pressure":
                recovery_limit = self.profile["budgets"]["storage_recovery_ms"]
                expected["recovery_ms_maximum"] = recovery_limit
                busy = next((row for row in lines if row.get("sqlite_busy_response") == 503), None)
                if (busy is not None and number(busy.get("recovery_ms"))
                        and busy["recovery_ms"] > recovery_limit):
                    return expected, observed, "FAIL"
                complete = (busy is not None and number(busy.get("busy_duration_ms")) and number(busy.get("recovery_ms"))
                            and busy.get("same_operation_committed_once_after_unlock") is True
                            and "writes_before_metadata_limit" in busy and busy.get("incidents_readable_in_storage_pressure") is True)
            elif CHECKS[case] == "capacity_ramp":
                maximum = self.profile["workload"]["camera_count"]
                targets = sorted({*[count for count in self.contract["capacity_ramp_cameras"] if count <= maximum], maximum})
                expected["ramp_camera_counts"] = targets
                complete = any(sorted(row.get("last_slot_statuses", [])) == [200, 503] and row.get("repeated_result") is True
                               and row.get("camera_slot_reuse_with_live_analysis") == maximum
                               and row.get("ramps") == [{"count": count, "streams_with_pose_and_fire": count} for count in targets]
                               for row in lines)
            elif CHECKS[case] == "capacity_overload":
                expected.update({"excess_requests": 16, "capacity_errors": 16, "capture_cameras": self.profile["workload"]["camera_count"],
                                 "recovery_ms_maximum": self.profile["budgets"]["rtsp_recovery_ms"],
                                 "control_ready_http": 200})
                load = next((row for row in lines if row.get("excess_requests") == expected["excess_requests"]), {})
                if load:
                    self.require_elapsed_clock(load)
                durations = load.get("excess_latency_ms", [])
                p95 = self.contract["quiet_load"]["api_read_control_p95_ms"]
                expected["api_p95_ms_maximum"] = p95
                complete = (len(durations) == expected["excess_requests"] and all(number(item) for item in durations)
                            and number(load.get("admission_recovery_ms")))
                if complete:
                    measured = sorted(durations)[math.ceil(len(durations) * 0.95) - 1]
                    observed["api_p95_ms"] = measured
                    maximum = expected["capture_cameras"]
                    if (load["excess_rejected"] != expected["capacity_errors"] or measured > p95
                            or load["admission_recovery_ms"] > expected["recovery_ms_maximum"]
                            or load["control_ready_http"] != 200 or load["active_cameras_progressed"] != maximum
                            or load["camera_slot_reuse_with_live_analysis"] != maximum):
                        return expected, observed, "FAIL"
            else:
                complete = any(self.preaged_retention(row) for row in lines)
        if code != 0:
            result = "FAIL" if code is not None and not proof.get("deadline_exceeded") else "INCOMPLETE"
        else:
            result = "PASS" if complete else "INCOMPLETE"
        return expected, observed, result

    @staticmethod
    def preaged_retention(row):
        before_after = row.get("canary_accepted_pose_fire")
        if not isinstance(before_after, list) or len(before_after) != 2:
            return False
        before, after = before_after
        return (row.get("preaged_rows_per_queue", 0) >= 4097 and row.get("foreign_key_check") == "ok"
                and row.get("open_incident_and_command_replay_preserved") is True
                and len(before) == len(after) == 2 and all(right > left for left, right in zip(before, after)))

    def fault_check(self, case, proof):
        require_overview = self.run.get("canary_overview") is True
        if "elapsed_clock" in self.run:
            self.require_elapsed_clock(checked_json(self.artifact(proof, "settings")))
        kind = FAULTS.get(case)
        with self.artifact(proof, "fault-ledger").open("rb") as handle:
            data = handle.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise ValueError("Fault ledger exceeds its bound")
        ledger = [json.loads(line) for line in data.splitlines()]
        schedule = self.artifact(proof, "fault-schedule")
        source = self.artifact(proof, "sources")
        workload = self.profile["workload"]
        if sha256(schedule) != workload["fault_schedule_digest"] or sha256(source) != workload["source_manifest_digest"]:
            raise ValueError("Fault inputs differ from the frozen workload")
        scheduled = checked_json(schedule)["faults"]
        expected_faults = {fault["id"]: fault for fault in scheduled}
        if len(expected_faults) != len(scheduled):
            raise ValueError("Fault schedule repeats an ID")
        faults = [entry for entry in ledger if kind is None or entry["kind"] == kind]
        completed = Counter()
        limits = self.profile["budgets"]
        failed = []
        missing_overviews = []
        seen = set()
        seen_incidents = set()
        tracked = None
        for row in faults:
            name = row["id"]
            planned = expected_faults.get(name)
            if name in seen or planned is None or any(row[key] != planned[key] for key in ("kind", "camera")):
                raise ValueError("Fault ledger repeats or adds an unplanned case")
            seen.add(name)
            prepared = row.get("prior_incidents_resolved", 0)
            if type(prepared) is not int or prepared < 0 or (prepared and not planned.get("resolve_before_fault")):
                raise ValueError("Fault ledger includes an unplanned incident resolution")
            quiet, quiet_duration_us = quiet_requirements(planned)
            if row.get("finished_s") is None:
                if row.get("fault") and number(row.get("ended_s")) and row["ended_s"] >= row["deadline_s"]:
                    failed.append({"id": name, "reason": row["error"], "observed_ms": (row["ended_s"] - row["started_s"]) * 1_000})
                continue
            started, finished = row["started_s"], row["finished_s"]
            if not number(started) or not number(finished) or not proof["started_s"] <= started <= finished <= proof["finished_s"]:
                raise ValueError("Fault timing is invalid or outside its observation window")
            recovery = {"rtsp_reconnect": "rtsp_recovery_ms", "inference_child_restart": "inference_recovery_ms",
                        "core_restart": "core_recovery_ms", "viewer_grant_offer_close": "viewer_recovery_ms"}
            if (finished - started) * 1_000 > limits[recovery[row["kind"]]]:
                failed.append({"id": name, "observed_recovery_ms": (finished - started) * 1_000,
                               "maximum_ms": limits[recovery[row["kind"]]]})
            canary = row.get("canary")
            if canary is None:
                continue
            raw_path = checked_evidence(self.root, canary)
            raw = checked_json(raw_path)
            if raw.get("planned") != planned["canary"] or not isinstance(raw.get("incidents"), list):
                raise ValueError("Canary is missing the frozen event specification or native incident rows")
            if quiet and not camera_quiet(raw["source_before"]):
                raise ValueError("The viewer was started outside the planned quiet scene")
            if quiet_duration_us:
                samples = raw.get("quiet_observations", [])
                if not verified_quiet_window(samples, quiet_duration_us) or samples[-1] != raw["source_before"]:
                    raise ValueError("The viewer proof lacks the planned sequence of quiet observations")
            progress = row.get("capture_progress_s")
            new_epoch = row["kind"] in {"rtsp_reconnect", "core_restart"}
            if not number(progress) or not started <= progress <= finished or not camera_recovered(
                    raw["source_before"], raw["source_after"], new_epoch):
                continue
            fault = row["fault"]
            if row["kind"] == "inference_child_restart":
                observed_recovery = raw.get("inference_recovery")
                if not observed_recovery:
                    continue
                if tracked is None:
                    tracked = {item["id"] for item in checked_json(source)["sources"][:workload["camera_count"]]}
                    if len(tracked) != workload["camera_count"]:
                        raise ValueError("Inference recovery source input repeats or omits cameras")
                camera = row["camera"]
                if (not inference_worker_replaced(fault, self.profile["models"]["workers"])
                        or not inference_cameras_recovered(observed_recovery, tracked)
                        or observed_recovery["before"][camera] != raw["source_before"]
                        or observed_recovery["after"][camera] != raw["source_after"]):
                    continue
            if row["kind"] == "viewer_grant_offer_close" and (
                    fault.get("viewer_frames_decoded") != 2 or fault.get("stream_closed_status") != 204):
                continue
            if row["kind"] == "core_restart" and (
                    not all(type(fault.get(key)) is int and fault[key] > 0 for key in ("retired_core_pid", "replacement_core_pid"))
                    or fault["retired_core_pid"] == fault["replacement_core_pid"]):
                continue
            if row["kind"] == "rtsp_reconnect" and not 1 <= fault.get("stopped_publishers", 0) <= self.profile["workload"]["camera_count"]:
                continue
            start_utc = raw.get("started_utc_ms")
            if not number(start_utc):
                continue
            matches = [row for row in canary_matches(raw["incidents"], planned["canary"], start_utc)
                       if row["id"] not in seen_incidents]
            if len(matches) != 1 or planned["canary"]["camera"] != planned["camera"]:
                failed.append({"id": name, "fresh_unique_matching_canaries": len(matches)})
                continue
            if require_overview and not canary_overview_matches(raw_path, raw, matches[0]):
                missing_overviews.append(name)
                continue
            seen_incidents.add(matches[0]["id"])
            completed[planned["camera"]] += 1
        minimums = self.contract["minimum_completed_fault_cycles"]
        required_count = minimums[kind] if kind else max(len(scheduled), sum(minimums.values()))
        expected = {"completed_cycles": required_count, "canary_after_each": True}
        observed = {"completed_cycles": completed.total(), "by_camera": dict(completed), "failures": failed}
        if require_overview:
            expected["verified_overview_after_each"] = True
            observed["unverified_overviews"] = missing_overviews
        complete = bool(required_count) and completed.total() >= required_count
        if kind == "rtsp_reconnect":
            each = self.contract["rtsp_reconnect_min_each"]
            cameras = self.contract["rtsp_reconnect_cameras"]
            expected.update({"minimum_cameras": cameras, "minimum_cycles_each": each})
            complete = complete and sum(value >= each for value in completed.values()) >= cameras
        return expected, observed, "FAIL" if failed else "PASS" if complete else "INCOMPLETE"

    def final_canary_check(self, proof):
        schedule = self.artifact(proof, "fault-schedule")
        source = self.artifact(proof, "sources")
        workload = self.profile["workload"]
        if (sha256(schedule) != workload["fault_schedule_digest"] or sha256(source) != workload["source_manifest_digest"]):
            raise ValueError("Final canary input differs from the frozen workload")
        planned = checked_json(schedule)["final_canary"]
        cameras = {row["id"] for row in checked_json(source)["sources"][:workload["camera_count"]]}
        maximum = self.profile["budgets"]["core_recovery_ms"]
        expected = {"canary": planned, "ready_cameras": sorted(cameras), "recovery_ms_maximum": maximum,
                    "sqlite_integrity_and_references": True, "persisted_evidence_and_device_receipt": True}
        if "native-canary" not in proof["artifacts"]:
            return expected, {"exit_code": proof["observations"]["exit_code"]}, "INCOMPLETE"
        raw_path = self.artifact(proof, "native-canary")
        raw = checked_json(raw_path)
        self.require_elapsed_clock(raw)
        elapsed = raw["elapsed_ms"]
        if (raw["planned"] != planned or not number(raw["started_utc_ms"]) or not number(elapsed)
                or elapsed > (proof["finished_s"] - proof["started_s"]) * 1_000):
            raise ValueError("Invalid final canary identity or observation interval")
        matching = canary_matches(raw["incidents"], planned, raw["started_utc_ms"])
        observed = {"ready_cameras": raw["ready_cameras"], "recovery_ms": elapsed, "matching_live_incidents": matching,
                    "integrity": raw["integrity"], "invalid_references": raw["invalid_references"]}
        passed = (len(cameras) == workload["camera_count"] and sorted(raw["ready_cameras"]) == sorted(cameras)
                  and raw["integrity"] == ["ok"] and raw["invalid_references"] == 0
                  and elapsed <= maximum and len(matching) == 1 and proof["observations"]["exit_code"] == 0)
        if self.run.get("canary_overview") is True:
            expected["verified_overview"] = True
            observed["verified_overview"] = len(matching) == 1 and canary_overview_matches(raw_path, raw, matching[0])
            if passed and not observed["verified_overview"]:
                return expected, observed, "INCOMPLETE"
        return expected, observed, "PASS" if passed else "FAIL"

    def blob_volume_check(self, proof):
        schedule = self.artifact(proof, "fault-schedule")
        source = self.artifact(proof, "sources")
        workload = self.profile["workload"]
        if sha256(schedule) != workload["fault_schedule_digest"] or sha256(source) != workload["source_manifest_digest"]:
            raise ValueError("Blob volume input differs from the frozen workload")
        planned = checked_json(schedule)["storage_enospc"]
        maximum = self.profile["budgets"]["storage_recovery_ms"]
        required = {"maximum_volume_bytes": 4 * 1024 * 1024, "probe_errno": errno.ENOSPC,
                    "recovery_ms_maximum": maximum, "canaries": planned}
        if "storage" not in proof["artifacts"]:
            return required, {"exit_code": proof["observations"]["exit_code"]}, "INCOMPLETE"
        raw = checked_json(self.artifact(proof, "storage"))
        self.require_elapsed_clock(raw)
        if raw["planned"] != planned or not number(raw["released_utc_ms"]) or not number(raw["recovery_ms"]):
            raise ValueError("Invalid storage fixture identity or timing")
        pressure, known = raw["pressure_event"], planned["pressure_canary"]
        pressure_exercised = (all(pressure[key] == known[key] for key in ("camera", "category"))
                              and int(pressure["epoch"]) == int(known["source_epoch"]) and pressure["live"] is True
                              and known["earliest_source_us"] <= int(pressure["source_us"]) <= known["latest_source_us"]
                              and pressure["failed_image_writes"] > 0 and pressure["utc_ms"] <= raw["released_utc_ms"])
        matches = [row for row in canary_matches(raw["incidents"], planned["recovered_canary"], raw["released_utc_ms"])
                   if row["id"] != pressure["id"]]
        checks = raw["checks"]
        passed = (pressure_exercised and len(matches) == 1 and raw["recovery_ms"] <= maximum
                  and raw["probe_errno"] == errno.ENOSPC and 0 < raw["free_bytes_after_release"] <= raw["volume_bytes"] <= required["maximum_volume_bytes"]
                  and persisted_blobs_clean(checks) and proof["observations"]["exit_code"] == 0)
        return required, raw, "PASS" if passed else "FAIL"

    def storage_canary_check(self, proof):
        workload = self.profile["workload"]
        schedule = self.artifact(proof, "fault-schedule")
        source = self.artifact(proof, "sources")
        if sha256(schedule) != workload["fault_schedule_digest"] or sha256(source) != workload["source_manifest_digest"]:
            raise ValueError("Storage recovery inputs differ from the frozen workload")
        planned = checked_json(schedule)["storage_database"]
        cameras = {row["id"] for row in checked_json(source)["sources"][:workload["camera_count"]]}
        minimum_wal = 16 * 1024 * 1024
        maximum = self.profile["budgets"]["storage_recovery_ms"]
        required = {"canaries": planned, "fresh_database_and_blob_canaries": True, "minimum_pinned_wal_bytes": minimum_wal,
                    "statuses": DATABASE_PRESSURE_STATUS, "maximum_recovery_ms": maximum,
                    "surviving_capture_cameras": sorted(cameras)}
        if (set(planned) != DATABASE_PRESSURE_STATUS.keys() or len(cameras) != workload["camera_count"]
                or any(spec["camera"] not in cameras for spec in planned.values())):
            raise ValueError("Storage recovery schedule omits or adds a fault or camera")
        if "database" not in proof["artifacts"]:
            return required, {"exit_code": proof["observations"]["exit_code"]}, "INCOMPLETE"
        raw = checked_json(self.artifact(proof, "database"))
        verdicts, used_incidents = {}, set()
        for kind, status in DATABASE_PRESSURE_STATUS.items():
            trial = raw["cases"].get(kind)
            if trial is None:
                verdicts[kind] = "INCOMPLETE"
                continue
            self.require_elapsed_clock(trial)
            if trial["planned"] != planned[kind] or not number(trial["released_utc_ms"]) or not number(trial["recovery_ms"]):
                raise ValueError(f"Invalid storage recovery identity or timing for {kind}")
            pressure = trial["during_pressure"]
            before, during = pressure["before"], pressure["during"]
            matches = [event for event in canary_matches(trial["incidents"], planned[kind], trial["released_utc_ms"])
                       if event["id"] not in used_incidents]
            held_wal = kind != "pinned_wal" or (number(pressure["wal_bytes"]) and pressure["wal_bytes"] >= minimum_wal)
            selected = planned[kind]["camera"]
            passed = (set(before) == set(during) == cameras
                      and all(capture_progressed(before[name], during[name]) for name in cameras)
                      and pressure["status"] == status and pressure["code"] == "storage_unavailable"
                      and number(pressure["refusal_ms"]) and pressure["live_status"] == 200 and held_wal
                      and trial["idempotent_write"] is True and trial["recovery_ms"] <= maximum
                      and camera_recovered(before[selected], trial["source_after_release"], True)
                      and len(matches) == 1 and persisted_blobs_clean(trial["checks"]))
            verdicts[kind] = "PASS" if passed else "FAIL"
            if passed:
                used_incidents.add(matches[0]["id"])
        blob, _, pending = self.read_case("storage.blob-enospc-and-reconciliation", "storage_and_crash")
        verdicts["blob_enospc"] = "INCOMPLETE"
        if blob is not None:
            state = self.blob_volume_check(blob)[2]
            verdicts["blob_enospc"] = pending if state == "PASS" and pending else state
        observed = {"statuses": verdicts, **raw}
        complete = all(value == "PASS" for value in verdicts.values()) and proof["observations"]["exit_code"] == 0
        return required, observed, "FAIL" if "FAIL" in verdicts.values() else "PASS" if complete else "INCOMPLETE"

    def logic_check(self, case, proof):
        catalog = checked_json(self.artifact(proof, "logical-test-binaries"))
        if any(catalog.get(field) != self.candidate.get(field) for field in ("source_revision", "cargo_lock_sha256")):
            raise ValueError("The logical executables are outside the frozen candidate source")
        requirements = LOGIC_CASES[case]
        expected = {"required_tests": [name for _, name in requirements]}
        statuses = proof["observations"]["statuses"]
        if len(statuses) != len(requirements):
            raise ValueError("Logical test inventory does not match the case")
        observed = {"passed": [], "failed": [], "not_run": []}
        for index, ((program, test), status) in enumerate(zip(requirements, statuses, strict=True)):
            if not DIGEST.fullmatch(catalog["programs"][program]["digest"]):
                raise ValueError("Logical test executable digest is invalid")
            log = self.read_text(self.artifact(proof, str(index)))
            if status["exit_code"] is None or status.get("deadline_exceeded"):
                observed["not_run"].append(test)
            elif status["exit_code"] == 0 and f"test {test} ... ok" in log:
                observed["passed"].append(test)
            else:
                observed["failed"].append(test)
            if "seeded_claims_" in test:
                expected["minimum_seeded_transitions"] = 100_000
                raw = next((row for row in self.json_lines(log) if row.get("scenario") == "task_claim_fencing"), {})
                observed["seeded_transitions"] = raw.get("transitions", 0)
                if observed["seeded_transitions"] < expected["minimum_seeded_transitions"] and test in observed["passed"]:
                    observed["passed"].remove(test)
                    observed["not_run"].append(test)
        return expected, observed, "FAIL" if observed["failed"] else "INCOMPLETE" if observed["not_run"] else "PASS"

    def criteria(self):
        for stage in STAGES:
            for case in self.contract["cases"][stage]:
                default = {"stage": stage, "native_evidence_required": True}
                sources = []
                try:
                    proof, sources, pending = self.read_case(case, stage)
                    if proof is None:
                        expected, observed, result = default, None, pending
                    elif case == "artifacts.exact-digests-and-release-features":
                        expected, observed, result = self.artifact_check(proof)
                    elif case in {"models.active-numeric-observation-event-gates", "models.all-role-numeric-report"}:
                        expected, observed, result = self.model_check(case, proof)
                    elif case == "models.native-warmup-all-shapes":
                        expected, observed, result = self.warmup_check(proof)
                    elif case == "quality.required-regressions":
                        expected, observed, result = self.event_regression_check(proof)
                    elif case in CHECKS:
                        expected, observed, result = self.smoke_check(case, proof)
                    elif case in LOGIC_CASES:
                        expected, observed, result = self.logic_check(case, proof)
                    elif case in FAULTS or case == "pipeline.canary-after-each-fault":
                        expected, observed, result = self.fault_check(case, proof)
                    elif case == "pipeline.final-live-canary":
                        expected, observed, result = self.final_canary_check(proof)
                    elif case == "resources.churn-drain-and-cleanup":
                        expected, observed, result = self.churn_check(proof)
                    elif case == "storage.blob-enospc-and-reconciliation":
                        expected, observed, result = self.blob_volume_check(proof)
                    elif case == "pipeline.canary-after-storage-faults":
                        expected, observed, result = self.storage_canary_check(proof)
                    elif stage == "quiet_load":
                        expected, observed, result = self.steady_check(case, proof)
                    else:
                        expected, observed, result = default, {"reason": "No validated evidence format for this case"}, "INCOMPLETE"
                    if result == "PASS" and pending:
                        result = pending
                except (KeyError, TypeError, ValueError, OSError, OverflowError) as error:
                    expected, observed, result = default, {"error": str(error)}, "INCOMPLETE"
                self.results.append({"id": case, "required": expected, "observed": observed,
                                     "result": result, "evidence": sources})

    def steady_check(self, case, proof):
        binding = (proof["observations"], proof["artifacts"])
        if self.steady is None:
            def artifacts(name):
                return self.artifact(proof, name) if name in proof["artifacts"] else None

            self.steady = (binding, Steady(artifacts, self.profile, self.contract, proof))
        if self.steady[0] != binding:
            raise ValueError("The quiet-load criteria require observations from the same continuous interval")
        run = self.steady[1]
        metric = {
            "capacity.complete-isolated-quiet-interval": run.interval,
            "capacity.every-camera-cadence-latency-and-source-lag": run.cadence,
            "capacity.declared-viewer-snapshot-evidence-load": run.media,
            "resources.high-water-and-growth": run.resources,
            "pipeline.useful-detections-under-load": run.detections,
        }[case]
        return metric()

    def churn_check(self, proof):
        trace = checked_json(self.artifact(proof, "churn"))
        self.require_elapsed_clock(trace)
        samples, drain = trace["samples"], trace["drain"]
        caps = resource_caps(self.profile)
        budgets = self.profile["budgets"]
        children = self.profile["models"]["workers"] + 1
        expected = {"children_after_drain": children, "maximum_drain_ms": budgets["rtsp_recovery_ms"],
                    "resource_max_with_headroom": caps, "growth_horizon_seconds": budgets["growth_horizon_seconds"]}
        if not 1 <= len(samples) <= 2049 or samples[0]["kind"] != "baseline":
            raise ValueError("Churn resource evidence is missing its ready baseline")
        if any(not number(row["elapsed_s"]) for row in samples) or any(
                left["elapsed_s"] >= right["elapsed_s"] for left, right in zip(samples, samples[1:])):
            raise ValueError("Churn samples do not have strictly ordered native times")
        usage = [row["usage"] for row in samples] + [drain["usage"]]
        peaks = {key: max((row[key] for row in usage if number(row.get(key))), default=None) for key in caps}
        complete = all(all(number(row.get(key)) for row in usage) for key in caps)
        failures = [key for key, peak in peaks.items() if peak is not None and peak > caps[key]]
        workload = defaultdict(list)
        sampled = Counter()
        for row in samples[1:]:
            if row["completed"]:
                workload[row["kind"]].append(row)
                sampled.update(row["completed"])
        growth = {}
        for kind in FAULTS.values():
            records = workload[kind]
            completed = {case for row in records for case in row["completed"]}
            enough_cycles = len(completed) >= self.contract["minimum_completed_fault_cycles"][kind]
            complete = complete and enough_cycles
            length = len(records) // 3
            if length == 0:
                complete = False
                continue
            for key in caps.keys() & {"rss_bytes", "vram_bytes"}:
                blocks = {}
                for index in range(0, 3 * length, length):
                    group = records[index:index + length]
                    if all(number(row["usage"].get(key)) for row in group):
                        minute = statistics.median(row["elapsed_s"] for row in group) / 60
                        blocks[minute] = [row["usage"][key] for row in group]
                if len(blocks) != 3:
                    complete = False
                    continue
                addition = upper_growth(blocks, budgets["growth_horizon_seconds"])
                growth[f"{kind}/{key}"] = addition
                peak = max(row["usage"][key] for row in records if number(row["usage"].get(key)))
                if enough_cycles and peak + addition > caps[key]:
                    failures.append(f"{kind}/{key}")
        media = drain["leases"]["media"]
        drained = (drain["usage"]["children"] == children and not drain["detached"]
                   and media is not None and all(value == 0 for value in media.values())
                   and all(drain["leases"][key] == 0 for key in ("capture_frame_bytes_in_use", "evidence_write_bytes_in_use"))
                   and all(drain[key] == 0 for key in ("enabled_cameras", "reserved_frame_bytes", "semantic_jobs"))
                   and drain["writer"]["queued"] == 0 and drain["writer"]["running_ms"] is None
                   and drain["elapsed_ms"] <= expected["maximum_drain_ms"])
        if not drained:
            failures.append("drain")
        fault, _, pending = self.read_case("pipeline.canary-after-each-fault", "faults_and_capacity")
        if fault is not None:
            schedule = checked_json(self.artifact(fault, "fault-schedule"))
            complete = (complete and pending is None and sampled == Counter(row["id"] for row in schedule["faults"])
                        and self.fault_check("pipeline.canary-after-each-fault", fault)[2] == "PASS")
        else:
            complete = False
        observed = {"peaks": peaks, "growth_upper_bytes": growth, "drain": drain, "failures": failures,
                    "sample_count": len(samples), "faults_sampled_after_recovery": len(sampled)}
        return expected, observed, "FAIL" if failures else "PASS" if complete else "INCOMPLETE"

    def report(self):
        duration = self.run["native_wall_seconds"]
        if duration > self.contract["budget_seconds"]:
            self.problems.append("Native run exceeded the Fast RC v2 deadline")
        if self.run.get("interrupted") is True:
            self.problems.append("Orchestration was interrupted before final cleanup")
        execution_profile_digest = self.profile.get("execution_profile_digest")
        if execution_profile_digest:
            try:
                pin = self.run["inputs"]["execution_profile"]
                source = self.root / "inputs/sources.json"
                self.core_profile = checked_execution_profile(checked_evidence(self.root, pin), self.profile, self.candidate,
                                                              source if source.is_file() else None)
                if self.core_profile is None:
                    self.problems.append("The frozen diagnostic profile cannot be activated by Core")
            except (KeyError, OSError, TypeError, ValueError) as error:
                self.problems.append(f"Execution-profile evidence is invalid: {error}")
        else:
            self.problems.append("The frozen profile has no execution-profile sidecar digest")
        self.criteria()
        if self.problems and all(row["result"] == "PASS" for row in self.results):
            self.results[-1]["result"] = "INCOMPLETE"
        statuses = Counter(row["result"] for row in self.results)
        verdict = "FAIL" if statuses["FAIL"] else "PASS" if statuses["PASS"] == len(self.results) else "INCOMPLETE"
        return {
            "protocol": PROTOCOL, "run_id": self.run["run_id"],
            "candidate_digest": self.run["candidate_digest"], "profile_digest": self.run["profile_digest"],
            **({"execution_profile_digest": execution_profile_digest} if execution_profile_digest else {}),
            "run_manifest_digest": sha256(self.manifest_path), "active_model_results": self.models,
            "native_wall_seconds": duration, "source_video_seconds": self.run.get("source_video_seconds"),
            "virtual_clock_span_seconds": self.run.get("virtual_clock_span_seconds"),
            "verdict": verdict, "criteria": self.results,
            "coverage": {"required_cases": len(self.results), "completed_cases": statuses["PASS"] + statuses["FAIL"]},
            "limitations": self.problems,
        }


def validate(root, output):
    result = Validation(root).report()
    validate_schema("report", result)
    write_json(output, result)
    print(f"{result['verdict']} {result['run_id']} ({result['coverage']['completed_cases']}/{result['coverage']['required_cases']} cases)")
    return {"PASS": 0, "FAIL": 1, "INCOMPLETE": 2}[result["verdict"]]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        sys.exit(validate(args.run, args.output or args.run / "report.json"))
    except (ImportError, OSError, KeyError, ValueError, TypeError) as error:
        print(f"INCOMPLETE: {error}", file=sys.stderr)
        sys.exit(2)

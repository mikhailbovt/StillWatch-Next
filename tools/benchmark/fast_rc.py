"""Run the Fast RC v2 collectors against a frozen StillWatch payload and profile."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import time
from uuid import uuid4

from fast_rc_contract import (
    CONTRACTS, LOGIC_CASES, PROTOCOL, ROOT, STAGES,
    checked_execution_profile, checked_json, contract_digests, evidence, member, sha256, validate_schema, write_json,
)
from validate_report import validate


class Run:
    def __init__(self, args):
        self.args = args
        self.candidate = checked_json(args.candidate)
        self.profile = checked_json(args.profile)
        validate_schema("profile", self.profile)
        self.core_profile = None
        if args.execution_profile is not None:
            self.core_profile = checked_execution_profile(
                args.execution_profile, self.profile, self.candidate, args.source_manifest)
        if self.core_profile is not None:
            selected = self.core_profile["artifacts"]["cuda"]
            if (self.core_profile["raw_frame_bytes"] != args.raw_frame_bytes
                    or args.cuda_device != (selected["device"] if selected else None)):
                raise ValueError("Native runner device or frame budget differs from the Core execution profile")
        contracts = contract_digests()
        if self.profile["contract_digests"] != contracts or self.profile["protocol"] != PROTOCOL:
            raise ValueError("Profile was frozen against different qualification contracts")
        self.contract = checked_json(CONTRACTS["native"])
        self.root = args.output.resolve()
        self.setup = args.candidate.parent.resolve(strict=True)
        if self.root.is_relative_to(self.setup):
            raise ValueError("Keep the evidence directory outside the frozen candidate payload")
        self.root.mkdir(parents=True, exist_ok=False)
        (self.root / "raw").mkdir()
        (self.root / "inputs").mkdir()
        self.started = time.monotonic()
        self.files = self.candidate["files"]
        if not isinstance(self.files, dict) or not self.files or len(self.files) > 2048:
            raise ValueError("Candidate inventory must contain between one and 2048 files")
        self.run = {
            "protocol": PROTOCOL, "run_id": uuid4().hex, "started_utc": datetime.now(timezone.utc).isoformat(),
            "contract_digests": contracts, "cases": {}, "inputs": {},
            "host": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
            "source_video_seconds": None, "virtual_clock_span_seconds": None, "interrupted": False, "stage_errors": [],
        }
        for name in ("candidate", "profile", "execution_profile"):
            path = getattr(args, name)
            if path is not None:
                copied = self.root / "inputs" / f"{name}.json"
                shutil.copyfile(path, copied)
                pin = evidence(self.root, copied)
                self.run["inputs"][name] = pin
                self.run[f"{name}_digest"] = pin["digest"]
        if self.run["candidate_digest"] != self.profile["candidate_digest"]:
            raise ValueError("Profile pins a different candidate manifest")
        self.os = platform.system().casefold()
        self.ext = ".exe" if self.os == "windows" else ""
        if self.os != self.profile["host"]["os"] or platform.machine().lower() not in {"x86_64", "amd64"}:
            raise ValueError("Runner is on a different host platform than the frozen profile")
        self.core = self.setup / "bin" / f"stillwatch-core{self.ext}"
        self.worker = self.setup / "bin" / f"stillwatch-worker{self.ext}"
        self.cli = self.setup / "bin" / f"stillwatch-cli{self.ext}"
        self.media = self.setup / "bin" / f"mediamtx{self.ext}"
        self.ffmpeg = self.setup / "encoder" / f"ffmpeg{self.ext}"
        self.runtime = self.setup / "runtime" / self.candidate["ort"]
        self.models = self.setup / "models"
        self.env = os.environ.copy()
        self.env.pop("PYTHONOPTIMIZE", None)
        self.verified = False
        self.process = None
        self.run["fixture_artifacts"] = {
            name: {"name": path.name, "digest": sha256(path)}
            for name in ("demo", "fixture_ffmpeg", "node")
            if (path := getattr(args, name)) is not None
        }

    def elapsed(self):
        return round(time.monotonic() - self.started, 6)

    def capture(self, stage, label, argv):
        directory = self.root / "raw" / stage
        directory.mkdir(exist_ok=True)
        out = directory / f"{label}.log"
        started = self.elapsed()
        deadline = self.started + self.contract["stage_boundaries_seconds"][stage][1]
        options = ({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt"
                   else {"start_new_session": True})
        with out.open("xb") as stream:
            if time.monotonic() >= deadline:
                stream.write(b"Stage deadline was reached before this command started.\n")
                return {"exit_code": None, "started_s": started, "finished_s": self.elapsed(),
                        "deadline_exceeded": True}, out
            try:
                self.process = subprocess.Popen(list(map(str, argv)), cwd=ROOT, env=self.env,
                                                stdin=subprocess.DEVNULL, stdout=stream,
                                                stderr=subprocess.STDOUT, **options)
            except OSError as error:
                stream.write((f"Collector could not start: {error}\n").encode("utf-8"))
                return {"exit_code": None, "started_s": started, "finished_s": self.elapsed(),
                        "collector_unavailable": True}, out
            exceeded = False
            try:
                code = self.process.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                exceeded = True
                self.stop()
                code = self.process.returncode
            except KeyboardInterrupt:
                self.stop()
                raise
            finally:
                self.process = None
        return {"exit_code": code, "started_s": started, "finished_s": self.elapsed(),
                "deadline_exceeded": exceeded}, out

    def stop(self):
        process = self.process
        if process is None or process.poll() is not None:
            return
        if os.name == "nt":
            subprocess.run(["taskkill.exe", "/pid", str(process.pid), "/t", "/f"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
        else:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.wait(timeout=10)

    def proof(self, stage, case, started, observations, artifacts, *, deadline=False):
        path = self.root / "raw" / stage / f"{case}.json"
        path.parent.mkdir(exist_ok=True)
        sources = {role: evidence(self.root, value) for role, value in artifacts.items()}
        write_json(path, {
            "protocol": PROTOCOL, "run_id": self.run["run_id"], "candidate_digest": self.run["candidate_digest"],
            "profile_digest": self.run["profile_digest"], "case": case, "started_s": started,
            "finished_s": self.elapsed(), "deadline_exceeded": deadline, "observations": observations,
            "artifacts": sources, "evidence": list(sources.values()),
        })
        self.run["cases"][case] = {"stage": stage, "proof": evidence(self.root, path)}

    def inputs(self):
        expected = set(self.files)
        actual = {path.relative_to(self.setup).as_posix() for path in self.setup.rglob("*") if path.is_file()}
        actual.discard(self.args.candidate.name)
        digests = {}
        for name in sorted(expected & actual):
            try:
                digests[name] = sha256(member(self.setup, name))[7:]
            except ValueError:
                continue
        return {"actual_digests": digests, "unexpected": sorted(actual - expected), "target_os": self.os}

    def preflight(self):
        stage = "preflight"
        began = self.elapsed()
        observed = self.inputs()
        path = self.root / "raw" / stage / "candidate-scan.json"
        path.parent.mkdir(exist_ok=True)
        write_json(path, observed)
        self.verified = observed["actual_digests"] == self.files and not observed["unexpected"]
        if self.verified:
            _, worker = self.capture(stage, "worker-usage", [self.worker, "--help"])
            _, core = self.capture(stage, "core-help", [self.core, "--help"])
            command = [self.core, "--data-dir", self.root / "host-probe", "--execution-host"]
            cuda = self.cuda_paths()
            if cuda:
                command.extend(["--cuda-device", self.args.cuda_device, "--model-pack", self.models,
                                "--model-manifest-sha256", self.candidate["model_manifest_sha256"],
                                "--ort-library", self.runtime, "--ort-library-sha256", self.candidate["ort_sha256"],
                                "--cuda-provider-sha256", self.files[cuda[0]], "--cuda-shared-sha256", self.files[cuda[1]]])
            _, host = self.capture(stage, "execution-host", command)
            sources = {"worker-usage": worker, "core-help": core, "execution-host": host}
            if self.core_profile is not None:
                _, decoder = self.capture(stage, "capture-doctor", [self.worker, "capture-doctor"])
                sources["capture-doctor"] = decoder
        else:
            sources = {}
        self.proof(stage, "artifacts.exact-digests-and-release-features", began, observed,
                   {"candidate-scan": path, **sources})
        model_lock = self.models / "models.lock.json"
        if not self.verified or not self.runtime.is_file() or not model_lock.is_file():
            return
        if sha256(model_lock) != self.profile["models"]["manifest_digest"]:
            return
        model_evidence = self.root / "inputs" / "model-lock.json"
        shutil.copyfile(model_lock, model_evidence)
        flags = []
        warm_options = []
        if self.profile["models"]["backend"] == "cuda_fp32":
            cuda = self.cuda_paths()
            if not cuda:
                return
            flags = ["--cuda", str(self.args.cuda_device), self.files[cuda[0]], self.files[cuda[1]]]
            warm_options = ["--cuda", member(self.setup, cuda[0]), member(self.setup, cuda[1]),
                            "--cuda-device", self.args.cuda_device]
        for shape in self.profile["models"]["tensor_shapes"]:
            batch, channels, height, width = shape
            if batch != 1 or channels != 3:
                raise ValueError("Warmup requires NCHW RGB tensor shapes with a batch of one")
            warm_options.extend(["--warm-shape", height, width])
        self.script(stage, "models.native-warmup-all-shapes", "warmup", ROOT / "tests/e2e/inference_profile.py",
                    "--worker", self.worker, "--runtime", self.runtime, "--models", self.models,
                    "--threads", self.profile["models"]["threads_per_worker"], *warm_options,
                    input_artifacts={"model-lock": model_evidence})
        observed, raw = self.capture(stage, "parity-output", [self.worker, "verify-model-pack", self.runtime,
                                     self.candidate["ort_sha256"], self.models, self.candidate["model_manifest_sha256"], *flags])
        # The worker can exit nonzero with a complete full-pack report on narrowed CUDA profiles.
        report = self.root / "raw/preflight/parity.json"
        try:
            transcript = raw.read_text(encoding="utf-8", errors="replace")
            start = transcript.find("{\n")
            if start < 0:
                return
            decoded, _ = json.JSONDecoder().raw_decode(transcript, idx=start)
            write_json(report, decoded)
        except ValueError:
            return
        for case in ("models.active-numeric-observation-event-gates", "models.all-role-numeric-report"):
            sources = {"worker-log": raw, "parity-output": report}
            if case == "models.active-numeric-observation-event-gates" and self.args.event_parity is not None:
                restored = self.root / "inputs" / "event-parity.json"
                shutil.copyfile(self.args.event_parity, restored)
                sources["event-output"] = restored
            self.proof(stage, case, observed["started_s"], observed, sources,
                       deadline=observed.get("deadline_exceeded", False))

    def cuda_paths(self):
        if self.profile["models"]["backend"] != "cuda_fp32" or self.args.cuda_device is None:
            return None
        by_kind = []
        for suffix in ("cuda", "shared"):
            base = "onnxruntime_providers_" + suffix
            matching = [name for name in self.files if Path(name).name.startswith((base, "lib" + base))]
            if len(matching) != 1:
                return None
            by_kind.append(matching[0])
        return by_kind

    def logical(self):
        stage = "logical"
        self.script(stage, "evaluator.censoring-coverage-and-grounding", "event-scoring",
                    ROOT / "tools/benchmark/test_event_scoring.py")
        fixture_ffmpeg = self.args.fixture_ffmpeg or self.ffmpeg
        if self.verified and all(item.is_file() for item in (self.runtime, fixture_ffmpeg, self.media)) and self.args.demo:
            self.script(stage, "storage.pre-aged-retention-at-volume", "retention", ROOT / "tests/e2e/analysis_core.py",
                        "--core", self.core, "--worker", self.worker, "--models", self.models,
                        "--runtime", self.runtime, "--manifest-sha256", self.candidate["model_manifest_sha256"],
                        "--runtime-sha256", self.candidate["ort_sha256"], "--demo", self.args.demo,
                        "--ffmpeg", fixture_ffmpeg, "--mediamtx", self.media, "--retention")
        self.logical_binaries(stage)

    def logical_binaries(self, stage):
        if self.args.logic_test_binaries is None or not self.verified:
            return
        manifest = self.root / "inputs" / "logical-test-binaries.json"
        if not manifest.exists():
            shutil.copyfile(self.args.logic_test_binaries, manifest)
        entries = checked_json(manifest)
        if any(entries.get(name) != self.candidate.get(name) for name in ("source_revision", "cargo_lock_sha256")):
            raise ValueError("Logical test binaries were built from a different candidate source")
        programs = {}
        for name, entry in entries["programs"].items():
            executable = Path(entry["path"]).resolve(strict=True)
            if sha256(executable) != entry["digest"]:
                raise ValueError(f"Logical executable digest differs for {name}")
            programs[name] = executable
        for case in self.contract["cases"][stage]:
            tests = LOGIC_CASES.get(case)
            if tests is None:
                continue
            started = self.elapsed()
            statuses = []
            outputs = {"logical-test-binaries": manifest}
            for index, (program, test) in enumerate(tests):
                label = f"{case}-{index}"
                outcome, log = self.capture(stage, label, [programs[program], "--exact", test, "--nocapture"])
                outputs[str(index)] = log
                statuses.append(outcome)
            self.proof(stage, case, started, {"statuses": statuses}, outputs,
                       deadline=any(item.get("deadline_exceeded") for item in statuses))

    def script(self, stage, case, name, script, *args, input_artifacts=None):
        observed, raw = self.capture(stage, name, [sys.executable, "-B", script, *args])
        self.proof(stage, case, observed["started_s"], observed, {"stdout": raw, **(input_artifacts or {})},
                   deadline=observed.get("deadline_exceeded", False))

    def storage(self):
        if not self.verified:
            return
        stage = "storage_and_crash"
        args = ("--core", self.core, "--worker", self.worker, "--cli", self.cli)
        observed, log = self.capture(stage, "storage-quota-wal", [sys.executable, "-B", ROOT / "tests/e2e/storage_budget.py", *args])
        for case in ("storage.sqlite-quota-and-busy", "storage.pinned-wal-reader"):
            self.proof(stage, case, observed["started_s"], observed, {"stdout": log},
                       deadline=observed.get("deadline_exceeded", False))
        if self.args.node is not None:
            self.script("storage_and_crash", "storage.refresh-lost-response-and-restart", "lan-refresh",
                        ROOT / "tests/e2e/lan_core.py", *args, "--node", self.args.node)
        self.logical_binaries(stage)
        if (not self.args.blob_enospc or self.args.node is None or self.args.fixture_ffmpeg is None
                or self.args.source_manifest is None or self.args.fault_schedule is None):
            return
        unshare = shutil.which("unshare") if self.os == "linux" else None
        if unshare is None:
            raise ValueError("The blob ENOSPC collector needs a Linux mount namespace")
        schedule = self.pinned_fault_schedule()
        if "storage_enospc" not in checked_json(schedule):
            raise ValueError("The frozen fault schedule has no storage ENOSPC canaries")
        source, settings = self.native_settings()
        native = self.root / "raw" / stage / "blob-enospc.json"
        options = native.with_name("blob-enospc-settings.json")
        write_json(options, settings | {"fault_schedule": str(schedule)})
        observations, log = self.capture(stage, "blob-enospc", [unshare, "--mount", "--propagation", "private", "--",
                                         sys.executable, "-B", ROOT / "tools/benchmark/qualify.py",
                                         "--native-blob-enospc", options, native])
        evidence_files = {"sources": source, "settings": options, "fault-schedule": schedule, "stdout": log}
        if native.is_file():
            evidence_files["storage"] = native
        self.proof(stage, "storage.blob-enospc-and-reconciliation", observations["started_s"], observations, evidence_files,
                   deadline=observations.get("deadline_exceeded", False))

    def native_settings(self):
        source = self.root / "inputs" / "sources.json"
        if not source.exists():
            shutil.copyfile(self.args.source_manifest, source)
        if sha256(source) != self.profile["workload"]["source_manifest_digest"]:
            raise ValueError("Native source manifest is outside the frozen workload")
        settings = {
            "profile": str(self.root / "inputs/profile.json"), "source_manifest": str(self.args.source_manifest),
            "core": str(self.core), "worker": str(self.worker), "cli": str(self.cli), "node": str(self.args.node),
            "models": str(self.models), "runtime": str(self.runtime), "runtime_digest": self.candidate["ort_sha256"],
            "model_digest": self.candidate["model_manifest_sha256"], "raw_frame_bytes": self.args.raw_frame_bytes,
            "mediamtx": str(self.media), "mediamtx_digest": self.candidate["mediamtx_sha256"],
            "encoder": str(self.ffmpeg), "encoder_digest": self.files[self.ffmpeg.relative_to(self.setup).as_posix()],
            "fixture_ffmpeg": str(self.args.fixture_ffmpeg), "address": self.args.address,
        }
        if self.core_profile is not None:
            settings["execution_profile"] = self.core_profile
        if self.profile["models"]["backend"] == "cuda_fp32":
            cuda = self.cuda_paths()
            if not cuda:
                raise ValueError("The frozen CUDA workload is missing its provider libraries")
            settings["cuda_args"] = ["--cuda-device", str(self.args.cuda_device), "--cuda-provider-sha256", self.files[cuda[0]],
                                     "--cuda-shared-sha256", self.files[cuda[1]]]
            settings["cuda_device"] = self.args.cuda_device
        return source, settings

    def quiet_load(self):
        if not self.verified or self.args.source_manifest is None or self.args.node is None or self.args.fixture_ffmpeg is None:
            return
        stage = "quiet_load"
        output = self.root / "raw" / stage / "native-quiet"
        output.parent.mkdir(exist_ok=True)
        source, settings = self.native_settings()
        options = output.parent / "settings.json"
        write_json(options, settings)
        observations, log = self.capture(stage, "quiet-load", [sys.executable, "-B", ROOT / "tools/benchmark/qualify.py",
                                         "--native-load", "--settings", options, "--out", output,
                                         "--seconds", self.args.diagnostic_load_seconds or self.contract["quiet_load"]["minimum_seconds"]])
        observations["isolated"] = self.args.isolated and self.args.diagnostic_load_seconds is None
        artifacts = {"sources": source, "settings": options, "stdout": log}
        for name, filename in (("start", "start.json"), ("end", "end.json"), ("samples", "samples.jsonl"),
                               ("warmup-timeout", "readiness-timeout.json")):
            path = output / filename
            if path.is_file():
                artifacts[name] = path
        if "end" in artifacts:
            spans = checked_json(artifacts["end"]).get("source_seconds_by_camera", {})
            if len(spans) == self.profile["workload"]["camera_count"]:
                self.run["source_video_seconds"] = sum(spans.values())
        for case in self.contract["cases"][stage]:
            self.proof(stage, case, observations["started_s"], observations, artifacts,
                       deadline=observations.get("deadline_exceeded", False))

    def faults(self):
        if not self.verified or self.args.source_manifest is None or self.args.fixture_ffmpeg is None:
            return
        stage = "faults_and_capacity"
        output = self.root / "raw" / stage / "native-faults"
        output.parent.mkdir(exist_ok=True)
        source, settings = self.native_settings()
        capacity_options = output.parent / "capacity-settings.json"
        write_json(capacity_options, settings)
        observations, log = self.capture(stage, "capacity-ramp", [sys.executable, "-B", ROOT / "tools/benchmark/qualify.py",
                                         "--native-capacity", capacity_options])
        for case in ("capacity.ramp-and-concurrent-admission", "capacity.overload-and-recovery"):
            self.proof(stage, case, observations["started_s"], observations,
                       {"sources": source, "settings": capacity_options, "stdout": log}, deadline=observations.get("deadline_exceeded", False))
        if self.args.fault_schedule is None or self.args.node is None:
            return
        schedule = self.pinned_fault_schedule()
        settings.update(fault_schedule=str(schedule), monotonic_origin=self.started)
        options = output.parent / "settings.json"
        write_json(options, settings)
        observations, log = self.capture(stage, "faults", [sys.executable, "-B", ROOT / "tools/benchmark/qualify.py",
                                         "--native-faults", "--settings", options, "--out", output, "--evidence-root", self.root])
        ledger = output / "ledger.jsonl"
        if not ledger.is_file():
            raise ValueError("Fault collector returned without a ledger")
        for case in (
            "source.reconnect-completed", "worker.inference-restart-completed", "service.core-restart-completed",
            "media.viewer-cycle-completed", "pipeline.canary-after-each-fault",
        ):
            self.proof(stage, case, observations["started_s"], observations,
                       {"sources": source, "settings": options, "fault-schedule": schedule, "fault-ledger": ledger, "stdout": log},
                       deadline=observations.get("deadline_exceeded", False))

    def pinned_fault_schedule(self):
        schedule = self.root / "inputs" / "faults.json"
        if not schedule.exists():
            shutil.copyfile(self.args.fault_schedule, schedule)
        if sha256(schedule) != self.profile["workload"]["fault_schedule_digest"]:
            raise ValueError("The native fault schedule differs from the frozen workload")
        return schedule

    def final(self):
        stage = "final"
        self.logical_binaries(stage)
        if (not self.verified or self.args.source_manifest is None or self.args.fault_schedule is None
                or self.args.fixture_ffmpeg is None or self.args.node is None):
            return
        schedule = self.pinned_fault_schedule()
        if "final_canary" not in checked_json(schedule):
            return
        source, settings = self.native_settings()
        output = self.root / "raw" / stage
        output.mkdir(exist_ok=True)
        options = output / "canary-settings.json"
        native = output / "canary.json"
        write_json(options, settings | {"fault_schedule": str(schedule)})
        observations, log = self.capture(stage, "live-canary", [sys.executable, "-B", ROOT / "tools/benchmark/qualify.py",
                                         "--native-final-canary", options, native])
        artifacts = {"sources": source, "settings": options, "fault-schedule": schedule, "stdout": log}
        if native.is_file():
            artifacts["native-canary"] = native
        self.proof(stage, "pipeline.final-live-canary", observations["started_s"], observations, artifacts,
                   deadline=observations.get("deadline_exceeded", False))

    def execute(self):
        collectors = {"preflight": self.preflight, "logical": self.logical,
                      "faults_and_capacity": self.faults, "storage_and_crash": self.storage, "quiet_load": self.quiet_load,
                      "final": self.final}
        for stage in ("preflight", *self.args.stage):
            if stage not in collectors:
                continue
            try:
                collectors[stage]()
            except (OSError, KeyError, TypeError, ValueError, subprocess.SubprocessError) as error:
                self.run["stage_errors"].append(f"{stage}: {error}")

    def seal(self):
        self.run["native_wall_seconds"] = self.elapsed()
        self.run["finished_utc"] = datetime.now(timezone.utc).isoformat()
        write_json(self.root / "run-manifest.json", self.run)
        return validate(self.root, self.root / "report.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for item in ("candidate", "profile", "execution-profile", "node", "demo", "fixture-ffmpeg", "source-manifest", "fault-schedule",
                 "logic-test-binaries", "event-parity"):
        parser.add_argument(f"--{item}", type=lambda p: Path(p).resolve(strict=True), required=item in {"candidate", "profile"})
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--stage", action="append", choices=STAGES[1:], help="Limit execution to selected stages after inputs are verified")
    parser.add_argument("--cuda-device", type=int)
    parser.add_argument("--address", default="127.0.0.1", help="Private interface for the local TLS/ICE workload")
    parser.add_argument("--raw-frame-bytes", type=int, default=256 * 1024 * 1024)
    parser.add_argument("--isolated", action="store_true", help="Attest that the native workload has exclusive use of this host")
    parser.add_argument("--blob-enospc", action="store_true", help="Use a private Linux mount namespace for the native 4 MiB ENOSPC fixture")
    parser.add_argument("--diagnostic-load-seconds", type=int, choices=range(1, 3600), help="Run a shorter trace that cannot qualify the quiet interval")
    args = parser.parse_args()
    args.stage = tuple(stage for stage in STAGES[1:] if args.stage is None or stage in args.stage)
    try:
        run = Run(args)
        try:
            run.execute()
        except KeyboardInterrupt:
            run.run["interrupted"] = True
        return run.seal()
    except (ImportError, OSError, KeyError, TypeError, ValueError, subprocess.SubprocessError) as error:
        print(f"INCOMPLETE: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

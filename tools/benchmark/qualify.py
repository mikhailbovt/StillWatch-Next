"""Measure actual CPU Core capture and inference on synthetic and licensed replay."""

from __future__ import annotations

import argparse
from collections import Counter
import csv
import hashlib
import json
import math
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory
from uuid import uuid4

import psutil

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tests/e2e"))
from replay_core import run_core, until


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def timing_percentiles(values):
    if not values:
        return {key: None for key in ("p50", "p95", "p99")}
    ordered = sorted(values)
    return {f"p{percent}": round(ordered[math.ceil(percent / 100 * len(ordered)) - 1], 4)
            for percent in (50, 95, 99)}


def command(args, fixture, duration):
    subprocess.run([
        str(args.ffmpeg), "-hide_banner", "-loglevel", "error", "-y", "-stream_loop", "-1",
        "-i", str(args.demo), "-t", str(duration), "-vf", f"scale={args.width}:-2", "-an",
        "-r", "15", "-c:v", "libx264", "-threads", "2", "-preset", "ultrafast", str(fixture),
    ], check=True)


def stats(process):
    main = psutil.Process(process.pid)
    processes = [main, *main.children(recursive=True)]
    rss = threads = 0
    counters = {}
    for item in processes:
        try:
            with item.oneshot():
                resident = item.memory_info().rss
                counter = item.cpu_times()
                created = item.create_time()
                thread_count = item.num_threads()
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
        rss += resident
        counters[(item.pid, created)] = counter.user + counter.system
        threads += thread_count
    return rss, counters, threads, len(counters)


def data_size(directory):
    total = 0
    for path in directory.rglob("*"):
        try:
            if path.is_file():
                total += path.stat().st_size
        except FileNotFoundError:
            pass
    return total


def measure(args, root, fixture, count, samples):
    directory = root / str(count)
    directory.mkdir()
    options = [
        "--model-pack", str(args.models),
        "--model-manifest-sha256", args.manifest_sha256,
        "--ort-library", str(args.runtime),
        "--ort-library-sha256", args.runtime_sha256,
        "--active-cameras", str(count),
        "--raw-frame-bytes", str(args.raw_bytes),
        "--inference-threads", str(args.threads),
        "--inference-workers", str(args.workers),
    ]
    if args.cuda_device is not None:
        options += [
            "--cuda-device", str(args.cuda_device),
            "--cuda-provider-sha256", args.cuda_provider_sha256,
            "--cuda-shared-sha256", args.cuda_shared_sha256,
        ]
    ids = []
    data = directory / "data"
    with run_core(args.core, args.worker, data, fixture.parent,
                  directory / "core.log", *options) as (process, local):
        for index in range(count):
            status, camera = local.call("POST", "/api/v2/cameras", {
                "name": f"Benchmark {index + 1}", "enabled": True,
                "source": {"type": "replay", "file": fixture.name, "pace": True, "repeat": args.repeat},
            }, key=uuid4().hex)
            if status != 201:
                raise RuntimeError(f"camera admission failed with {status}")
            ids.append(camera["id"])

        def capture_ready():
            return all(local.camera(camera)["runtime"]["state"] == "replaying" for camera in ids)

        until(capture_ready, 30)
        time.sleep(args.warmup)
        began = time.monotonic()
        end = began + args.seconds
        timings = []
        observations = {}
        states = Counter()
        rss = []
        native = []
        workers = []
        disks = []
        heartbeat_path = args.output / "heartbeat.json"
        heartbeat_temp = heartbeat_path.with_suffix(".tmp")
        cpu_totals = stats(process)[1]
        cpu_seconds = 0.0
        while time.monotonic() < end:
            interval = time.monotonic()
            point = interval - began
            resident, cpu_now, threads, processes = stats(process)
            cpu_seconds += sum(max(0.0, seconds - cpu_totals.get(identity, 0.0))
                               for identity, seconds in cpu_now.items())
            cpu_totals = cpu_now
            rss.append(resident)
            native.append(threads)
            workers.append(processes)
            disks.append(data_size(data))
            for camera in ids:
                started = time.monotonic()
                status, item = local.call("GET", f"/api/v2/cameras/{camera}/analysis-status")
                latency = 1000 * (time.monotonic() - started)
                timings.append(latency)
                if status != 200:
                    raise RuntimeError(f"analysis status returned {status}")
                states[item["state"]] += 1
                observations.setdefault(camera, []).append((point, item))
                samples.writerow([
                    count, round(point, 3), camera, item["state"], item["source_epoch"],
                    item["pose_frames"], item["fire_frames"], item["last_analysis_age_ms"],
                    item["last_worker_ms"], resident, threads, processes, disks[-1], round(latency, 4),
                ])
            if args.repeat:
                heartbeat_temp.write_text(json.dumps({
                    "cameras": count, "elapsed_seconds": round(point, 2), "rss_bytes": resident,
                    "data_bytes": disks[-1], "processes": processes, "analysis_state_counts": dict(states),
                }) + "\n", encoding="utf-8")
                heartbeat_temp.replace(heartbeat_path)
            time.sleep(max(0, min(end, interval + args.sample_every) - time.monotonic()))

        elapsed = time.monotonic() - began
        cpu_seconds += sum(max(0.0, seconds - cpu_totals.get(identity, 0.0))
                           for identity, seconds in stats(process)[1].items())
        cpu = 100 * cpu_seconds / elapsed
        rates = {}
        for camera, rows in observations.items():
            span = rows[-1][0] - rows[0][0]
            pairs = [(a, b) for (_, a), (_, b) in zip(rows, rows[1:])
                     if a["source_epoch"] is not None and b["source_epoch"] is not None]
            totals = dict.fromkeys(("pose", "fire"), 0)
            for a, b in pairs:
                for task in totals:
                    new = int(b[f"{task}_frames"])
                    previous = int(a[f"{task}_frames"]) if a["source_epoch"] == b["source_epoch"] else 0
                    totals[task] += max(0, new - previous)
            rates[camera] = {
                **{f"{task}_fps": round(total / span, 3) if pairs and span else None for task, total in totals.items()},
                "last_epoch": next((row["source_epoch"] for _, row in reversed(rows) if row["source_epoch"]), None),
            }
        worker_times = [item["last_worker_ms"] for rows in observations.values() for _, item in rows
                        if item["last_worker_ms"] is not None]
        frame_ages = [int(item["last_analysis_age_ms"]) for rows in observations.values() for _, item in rows
                      if item["last_analysis_age_ms"] is not None]
        api = timing_percentiles(timings)
        inference = timing_percentiles(worker_times)
        age = timing_percentiles(frame_ages)
        return {
            "cameras": count, "seconds": round(elapsed, 3), "api_p95_ms": api["p95"],
            "inference_worker_p95_ms_sampled": inference["p95"],
            "analysis_age_p95_ms_sampled": age["p95"],
            "timing_percentiles_ms": {"api": api, "inference_worker_sampled": inference, "analysis_age_sampled": age},
            "rss_median_mib": round(statistics.median(rss) / 2**20, 2),
            "rss_max_mib": round(max(rss) / 2**20, 2),
            "rss_start_mib": round(rss[0] / 2**20, 2), "rss_end_mib": round(rss[-1] / 2**20, 2),
            "data_start_bytes": disks[0], "data_end_bytes": disks[-1], "data_max_bytes": max(disks),
            "cpu_percent_one_core": round(cpu, 2), "threads_max": max(native), "processes_max": max(workers),
            "cpu_measurement": "sampled lower bound across process lifetimes",
            "analysis_state_counts": dict(states), "analysis_samples": len(timings),
            "per_camera": rates,
        }


def run(args):
    if args.seconds < 5 or not all(1 <= count <= 32 for count in args.cameras) or not 0.25 <= args.sample_every < args.seconds:
        raise ValueError("use at least five seconds, one to thirty-two cameras, and a sampling period shorter than the run")
    args.output.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix="stillwatch-benchmark-") as temporary:
        root = Path(temporary)
        fixture = root / "bench.mp4"
        if args.prepared_video:
            shutil.copyfile(args.prepared_video, fixture)
        else:
            duration = 150 if args.repeat else args.seconds + args.warmup + 2 * max(args.cameras) + 30
            command(args, fixture, duration)
        header = [
            "cameras", "seconds", "camera_id", "analysis_state", "epoch", "pose_frames", "fire_frames",
            "analysis_age_ms", "worker_ms", "tree_rss_bytes", "tree_threads", "tree_processes", "data_bytes", "api_ms",
        ]
        report = {
            "platform": platform.platform(), "processor": platform.processor(),
            "physical_cores": psutil.cpu_count(logical=False), "logical_cores": psutil.cpu_count(),
            "system_ram_bytes": psutil.virtual_memory().total,
            "core_sha256": digest(args.core), "worker_sha256": digest(args.worker),
            "ffmpeg_sha256": digest(args.ffmpeg), "demo_sha256": digest(args.demo),
            "model_manifest_sha256": args.manifest_sha256, "runtime_sha256": args.runtime_sha256,
            "profile": "cuda-fp32" if args.cuda_device is not None else "cpu-fp32",
            "cuda_device": args.cuda_device, "threads_per_worker": args.threads, "inference_workers": args.workers,
            "raw_frame_bytes": args.raw_bytes,
            "fixture_sha256": digest(fixture), "repeat": args.repeat, "sample_every_seconds": args.sample_every,
            "width": args.width, "warmup_seconds": args.warmup, "results": [],
        }
        with (args.output / "samples.csv").open("w", encoding="utf-8", newline="", buffering=1) as handle:
            samples = csv.writer(handle)
            samples.writerow(header)
            for count in args.cameras:
                result = measure(args, root, fixture, count, samples)
                handle.flush()
                report["results"].append(result)
                (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
                print(json.dumps({key: value for key, value in result.items() if key != "per_camera"}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "worker", "demo", "models", "runtime", "ffmpeg"):
        parser.add_argument("--" + name, type=lambda path: Path(path).resolve(strict=True), required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--runtime-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=35)
    parser.add_argument("--warmup", type=int, default=6)
    parser.add_argument("--width", type=int, default=640)
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--cuda-device", type=int)
    parser.add_argument("--cuda-provider-sha256")
    parser.add_argument("--cuda-shared-sha256")
    parser.add_argument("--raw-bytes", type=int, default=256 * 1024 * 1024)
    parser.add_argument("--cameras", type=int, nargs="+", default=[1, 4, 8, 16])
    parser.add_argument("--prepared-video", type=Path)
    parser.add_argument("--repeat", action="store_true", help="Loop paced replay with a new source epoch at each boundary")
    parser.add_argument("--sample-every", type=float, default=1)
    options = parser.parse_args()
    if options.cuda_device is not None and (
        not options.cuda_provider_sha256 or not options.cuda_shared_sha256 or options.workers != 1
    ):
        parser.error("CUDA requires one worker and the trusted digests of both provider libraries")
    run(options)

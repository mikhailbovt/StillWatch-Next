"""Calculate quiet-load gates from ordered native observations and task histograms."""

from collections import Counter, defaultdict
import json
import math
import statistics

from fast_rc_contract import checked_json, number, sha256


def percentile(values, fraction):
    return sorted(values)[math.ceil(len(values) * fraction) - 1] if values else None


def task_distribution(before, after, task, limit95_ms, limit99_ms):
    bounds = after["latency_bounds_us"]
    if before["latency_bounds_us"] != bounds:
        raise ValueError("Native latency buckets changed during the quiet interval")
    delta = [int(b) - int(a) for a, b in zip(before[task]["receive_to_accept_us"],
                                              after[task]["receive_to_accept_us"], strict=True)]
    counts = {key: int(value) - int(before["counts"][task][key]) for key, value in after["counts"][task].items()}
    if len(delta) != len(bounds) + 1 or min(delta) < 0 or min(counts.values()) < 0:
        raise ValueError("Native task histogram or counters reset during the quiet interval")
    size = sum(delta)
    if counts["accepted"] != size:
        raise ValueError("Each accepted result must contribute one receive-to-accept sample")

    def included(cutoff_us):
        return sum(count for upper, count in zip(bounds, delta) if upper <= cutoff_us)

    return counts | {"latency_samples": size, "timely_p95_count": included(limit95_ms * 1000),
                     "timely_p99_count": included(limit99_ms * 1000),
                     "maximum_age_us": after[task]["maximum_receive_to_accept_us"]}


def read_samples(path):
    if path is None:
        return []
    with path.open("rb") as stream:
        rows = []
        while raw := stream.readline(256 * 1024):
            if len(raw) == 256 * 1024 or len(rows) >= 3601:
                raise ValueError("Quiet sample count or row exceeded its fixed limit")
            row = json.loads(raw)
            if row["tick"] != len(rows) or not number(row["elapsed_s"]):
                raise ValueError("Quiet tick order or clock is invalid")
            if rows and row["elapsed_s"] <= rows[-1]["elapsed_s"]:
                raise ValueError("Quiet sample time did not advance")
            rows.append(row)
    return rows


class Steady:
    def __init__(self, artifacts, profile, contract, proof):
        self.rows = read_samples(artifacts("samples"))
        self.start = self.opt_json(artifacts("start"))
        self.end = self.opt_json(artifacts("end"))
        self.sources_path = artifacts("sources")
        self.sources = checked_json(self.sources_path)
        self.workload = profile["workload"]
        self.budgets = profile["budgets"]
        self.model = profile["models"]
        self.limits = contract["quiet_load"]
        self.observations = proof["observations"]
        self.cameras = [item["id"] for item in self.sources["sources"][:self.workload["camera_count"]]]
        if len(set(self.cameras)) != self.workload["camera_count"] or self.workload["source_manifest_digest"] != sha256(self.sources_path):
            raise ValueError("Steady input differs from the pinned camera workload")
        if self.start and set(self.start["cameras"]) != set(self.cameras):
            raise ValueError("Steady barrier did not enumerate the declared cameras")

    @staticmethod
    def opt_json(path):
        return checked_json(path) if path is not None else None

    def interval(self):
        minimum = self.limits["minimum_seconds"]
        required = {"seconds": minimum, "camera_count": len(self.cameras), "planned_faults": 0, "isolated": True}
        last = self.rows[-1] if self.rows else {}
        observed = {"seconds": self.end["elapsed_s"] if self.end else None, "ticks": len(self.rows),
                    "camera_count": len(self.start["cameras"]) if self.start else 0,
                    "isolated": self.observations["isolated"], "exit_code": self.observations["exit_code"]}
        entered = bool(self.start and self.rows)
        complete = bool(entered and self.end and self.observations["exit_code"] == 0
                        and self.end["elapsed_s"] >= minimum and len(self.rows) == minimum + 1
                        and last["elapsed_s"] >= minimum and observed["isolated"] is True)
        verdict = "PASS" if complete else "INCOMPLETE"
        return required, observed, verdict

    def cadence(self):
        pose, fire = (self.workload[key] for key in ("pose_fps", "fire_fps"))
        floor = self.limits["minimum_wall_accepted_fraction"]
        span = self.limits["wall_window_seconds"]
        required = {"wall_pose_min": math.ceil(pose * floor * span), "wall_fire_min": math.ceil(fire * floor * span),
                    "source_pose_per_min": math.ceil(pose * self.limits["minimum_source_pose_fraction"] * 60),
                    "source_fire_per_min": math.ceil(fire * self.limits["minimum_source_fire_fraction"] * 60),
                    "camera_count": len(self.cameras), "api_p95_max_ms": self.limits["api_read_control_p95_ms"],
                    "receive_to_accept_p95_ms": self.limits["receive_to_accept_p95_ms"],
                    "receive_to_accept_p99_ms": self.limits["receive_to_accept_p99_ms"],
                    "max_result_age_ms": self.limits["max_accepted_result_age_ms"]}
        failures = Counter()
        minima = defaultdict(dict)
        latency_samples = defaultdict(list)
        missing = Counter()
        epochs = defaultdict(set)
        for row in self.rows:
            for camera in self.cameras:
                sample = row["cameras"].get(camera)
                if sample is None:
                    missing[camera] += 1
                    continue
                analysis = sample["analysis"]
                epochs[camera].add(sample["epoch"])
                latency_samples[camera].append(sample["api_ms"])
                if sample["capture_state"] != "streaming" or not analysis or analysis["state"] != "ready":
                    failures[camera + ".state"] += 1
                    continue
                if sample.get("source_lagging") is True:
                    failures[camera + ".source_lag"] += 1
                if row["tick"] >= 60:
                    last = self.rows[row["tick"] - 60]["cameras"].get(camera, {})
                    current_lag = sample.get("relative_source_lag_ms")
                    last_lag = last.get("relative_source_lag_ms")
                    if type(current_lag) is int and type(last_lag) is int:
                        if current_lag - last_lag > self.limits["relative_source_lag_max_growth_ms_per_60s"]:
                            failures[camera + ".source_lag_growth"] += 1
                    else:
                        missing[camera + ".source_lag"] += 1
                wall = analysis.get("wall_cadence")
                source = analysis.get("cadence")
                if not wall or not source or wall["window_seconds"] != span:
                    missing[camera + ".cadence"] += 1
                    continue
                for name, value in {
                    "wall_pose_min": wall["pose_accepted"], "wall_fire_min": wall["fire_accepted"],
                    "source_pose_per_min": source["pose_frames_per_minute"], "source_fire_per_min": source["fire_frames_per_minute"],
                }.items():
                    minima[camera][name] = min(minima[camera].get(name, value), value)
                    if value < required[name]:
                        failures[camera + "." + name] += 1
        distributions = {}
        for camera, timings in latency_samples.items():
            if len(epochs[camera]) != 1:
                failures[camera + ".epoch"] += 1
            api = percentile(timings, .95)
            if api > required["api_p95_max_ms"]:
                failures[camera + ".api_latency"] += 1
            distributions[camera] = {"api_p95_ms": api}
            if not self.start or not self.end:
                continue
            for task in ("pose", "fire"):
                row = task_distribution(self.start["metrics"][camera], self.end["metrics"][camera], task,
                                        required["receive_to_accept_p95_ms"], required["receive_to_accept_p99_ms"])
                distributions[camera][task] = row
                count = row["latency_samples"]
                if (not count or row["timely_p95_count"] < math.ceil(count * 0.95)
                        or row["timely_p99_count"] < math.ceil(count * 0.99)
                        or row["maximum_age_us"] > required["max_result_age_ms"] * 1_000):
                    failures[camera + f".{task}_latency"] += 1
        complete = not missing and self.interval()[2] == "PASS"
        observed = {"minimum_by_camera": minima, "missing": missing, "latency": distributions, "failures": failures}
        return required, observed, "FAIL" if failures else "PASS" if complete else "INCOMPLETE"

    def media(self):
        required = {"decoded_viewer_sessions": self.workload["viewer_sessions"],
                    "snapshot_requests_per_minute": self.workload["snapshot_requests_per_minute"],
                    "snapshot_decode_fraction": 1, "evidence_fps": self.workload["evidence_fps"]}
        if not self.rows:
            return required, {}, "INCOMPLETE"
        last = self.rows[-1]
        start = self.start["viewer_frames"]
        viewers = {key: frames - start.get(key, 0) for key, frames in last["viewer_frames"].items()}
        snapshots = last["snapshots"]
        observed = {"decoded_frames_by_viewer": viewers, "snapshot_requests": snapshots, "snapshots_decoded": last["snapshots_ok"],
                    "expected_snapshot_requests": last["tick"] * required["snapshot_requests_per_minute"] // 60}
        if snapshots != last["snapshots_ok"]:
            return required, observed, "FAIL"
        coverage = len(viewers) == required["decoded_viewer_sessions"] and all(count > 0 for count in viewers.values())
        if not coverage or observed["expected_snapshot_requests"] != snapshots or self.interval()[2] != "PASS":
            return required, observed, "INCOMPLETE"
        return required, observed, "PASS"

    def resources(self):
        budgets = self.budgets
        caps = {"rss_bytes": budgets["rss_bytes"] - budgets["headroom_bytes"]["rss"],
                "disk_bytes": budgets["disk_bytes"] - budgets["headroom_bytes"]["disk"],
                "children": budgets["max_children"], "os_handles": budgets["max_os_handles"], "open_sockets": budgets["max_open_sockets"]}
        if self.model["backend"] == "cuda_fp32":
            caps["vram_bytes"] = budgets["vram_bytes"] - budgets["headroom_bytes"]["vram"]
        horizon = budgets["growth_horizon_seconds"]
        series = {key: [] for key in caps}
        for row in self.rows:
            if not row["resources"]:
                continue
            for key in caps:
                value = row["resources"].get(key)
                if number(value):
                    series[key].append((row["tick"], value))
        peaks = {key: max((value for _, value in rows), default=None) for key, rows in series.items()}
        growth = {}
        for key in {"rss_bytes", "disk_bytes", "vram_bytes"} & series.keys():
            blocks = defaultdict(list)
            for tick, value in series[key]:
                blocks[tick // 60].append(value)
            if len(blocks) >= 60:
                growth[key] = upper_growth(blocks, horizon)
        failures = [key for key, peak in peaks.items() if peak is not None and peak > caps[key]]
        failures.extend(key for key, addition in growth.items() if peaks[key] + addition > caps[key])
        complete = (self.interval()[2] == "PASS" and all(len(rows) >= 720 for rows in series.values())
                    and len(growth) == (3 if "vram_bytes" in caps else 2))
        return ({"resource_max_with_headroom": caps, "growth_horizon_seconds": horizon},
                {"peaks": peaks, "upper_growth_bytes": growth, "failures": sorted(set(failures))},
                "FAIL" if failures else "PASS" if complete else "INCOMPLETE")

    def detections(self):
        canaries = self.sources.get("canaries", [])
        required = {"expected_known_canaries": len(canaries), "active_live_detector": True}
        if not self.end or not canaries:
            return required, {}, "INCOMPLETE"
        missed = []
        for canary in canaries:
            if not any(event["live"] and event["category"] == canary["category"] and event["camera"] == canary["camera"]
                       and canary["earliest_source_us"] <= int(event["source_us"]) <= canary["latest_source_us"]
                       for event in self.end["openings"]):
                missed.append(canary["id"])
        complete = self.interval()[2] == "PASS"
        return required, {"openings": len(self.end["openings"]), "missed": missed}, (
            "FAIL" if missed and complete else "PASS" if complete else "INCOMPLETE")


def upper_growth(blocks, horizon):
    # One value per minute prevents high-frequency polling from inflating the sample size.
    minutes = sorted(blocks)
    samples = [statistics.median(blocks[minute]) for minute in minutes]
    fit = statistics.linear_regression(minutes, samples)
    remaining = [value - fit.intercept - fit.slope * minute for minute, value in zip(minutes, samples, strict=True)]
    centered = sum((minute - statistics.mean(minutes)) ** 2 for minute in minutes)
    standard_error = math.sqrt(sum(value * value for value in remaining) / (len(minutes) - 2) / centered)
    return max(0.0, fit.slope + 2 * standard_error) * (horizon / 60)

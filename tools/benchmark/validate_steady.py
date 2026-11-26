"""Calculate quiet-load gates from ordered native observations and task histograms."""

from collections import Counter, defaultdict
import json
import math
import statistics

from fast_rc_contract import (
    DURATION_CLOCKS, SHORT_PROTOCOL, checked_json, detail_output_dimensions, fresh_opening, number, sha256,
)


def resource_caps(profile):
    budgets = profile["budgets"]
    caps = {"rss_bytes": budgets["rss_bytes"] - budgets["headroom_bytes"]["rss"],
            "disk_bytes": budgets["disk_bytes"] - budgets["headroom_bytes"]["disk"],
            "children": budgets["max_children"], "os_handles": budgets["max_os_handles"],
            "open_sockets": budgets["max_open_sockets"]}
    if profile["models"]["backend"] == "cuda_fp32":
        caps["vram_bytes"] = budgets["vram_bytes"] - budgets["headroom_bytes"]["vram"]
    return caps


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


def viewer_gaps_ms(rows, start):
    progress = {key: (count, 0.0) for key, count in start.items()}
    longest = dict.fromkeys(start, 0.0)
    for row in rows:
        counters = row["viewer_frames"]
        if counters.keys() != start.keys():
            return None
        for key, count in counters.items():
            previous, advanced_at = progress[key]
            if type(count) is not int or count < previous:
                return None
            now = row["elapsed_s"]
            if count > previous:
                progress[key] = count, now
            else:
                longest[key] = max(longest[key], (now - advanced_at) * 1000)
    return longest


def measured_wall_counts(before, after, span):
    start, end = before.get("elapsed_clock_s"), after.get("elapsed_clock_s")
    if (not number(start) or not number(end) or end <= start
            or before["epoch"] != after["epoch"] or not before.get("analysis")):
        return None
    first, last = before["analysis"], after["analysis"]
    counts = [int(last[f"{task}_frames"]) - int(first[f"{task}_frames"]) for task in ("pose", "fire")]
    if min(counts) < 0:
        raise ValueError("Accepted model counters regressed during the quiet interval")
    return tuple(count * span / (end - start) for count in counts)


class Steady:
    def __init__(self, artifacts, profile, contract, proof):
        self.rows = read_samples(artifacts("samples"))
        self.start = self.opt_json(artifacts("start"))
        self.end = self.opt_json(artifacts("end"))
        self.partial_snapshots = self.opt_json(artifacts("snapshot-failures")) if self.end is None else None
        self.sources_path = artifacts("sources")
        self.sources = checked_json(self.sources_path)
        self.workload = profile["workload"]
        self.budgets = profile["budgets"]
        self.model = profile["models"]
        self.clock = DURATION_CLOCKS[profile["host"]["os"]]
        self.short = contract.get("protocol") == SHORT_PROTOCOL
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
        required = {"seconds": minimum, "elapsed_clock": self.clock, "camera_count": len(self.cameras),
                    "planned_faults": 0, "isolated": True}
        last = self.rows[-1] if self.rows else {}
        observed = {"seconds": self.end["elapsed_s"] if self.end else None, "ticks": len(self.rows),
                    "camera_count": len(self.start["cameras"]) if self.start else 0,
                    "isolated": self.observations["isolated"], "exit_code": self.observations["exit_code"],
                    "elapsed_clock": self.end.get("elapsed_clock") if self.end else None}
        entered = bool(self.start and self.rows)
        complete = bool(entered and self.end and self.start.get("elapsed_clock") == observed["elapsed_clock"] == self.clock
                        and self.observations["exit_code"] == 0
                        and self.end["elapsed_s"] >= minimum and len(self.rows) == minimum + 1
                        and last["elapsed_s"] >= minimum and observed["isolated"] is True)
        if self.short:
            warmup = self.start.get("warmup_elapsed_s") if self.start else None
            late = [row["tick"] for row in self.rows if not
                    0 <= row["elapsed_s"] - row["tick"] <= self.limits["max_tick_lateness_seconds"]]
            required.update(warmup_budget_seconds=self.limits["warmup_budget_seconds"],
                            max_tick_lateness_seconds=self.limits["max_tick_lateness_seconds"])
            observed.update(warmup_elapsed_s=warmup, late_ticks=late)
            complete = (complete and number(warmup) and warmup <= required["warmup_budget_seconds"] and not late
                        and self.start.get("protocol") == self.end.get("protocol") == SHORT_PROTOCOL
                        and last["elapsed_s"] <= observed["seconds"] <= minimum + self.limits["max_terminal_overrun_seconds"]
                        and all(row["cameras"].keys() == self.start["cameras"].keys() for row in self.rows))
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
        clock_verified = self.start and self.start.get("elapsed_clock") == self.clock
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
                values = {}
                # Early rolling readings can contain startup samples from before the measured interval.
                if row["elapsed_s"] >= span:
                    wall = analysis.get("wall_cadence")
                    if wall and wall["window_seconds"] == span:
                        values.update(wall_pose_min=wall["pose_accepted"], wall_fire_min=wall["fire_accepted"])
                        if clock_verified and row["tick"] >= span:
                            before = self.rows[row["tick"] - span]["cameras"].get(camera, {})
                            measured = measured_wall_counts(before, sample, span)
                            if measured is None:
                                missing[camera + ".elapsed_clock_cadence"] += 1
                            else:
                                values.update(wall_pose_min=min(values["wall_pose_min"], measured[0]),
                                              wall_fire_min=min(values["wall_fire_min"], measured[1]))
                    else:
                        missing[camera + ".wall_cadence"] += 1
                if row["elapsed_s"] >= self.limits["source_window_seconds"]:
                    source = analysis.get("cadence")
                    if source:
                        values.update(source_pose_per_min=source["pose_frames_per_minute"],
                                      source_fire_per_min=source["fire_frames_per_minute"])
                    else:
                        missing[camera + ".source_cadence"] += 1
                for name, value in values.items():
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
        complete = (not missing and set(minima) == set(self.cameras)
                    and all(len(values) == 4 for values in minima.values()) and self.interval()[2] == "PASS")
        observed = {"minimum_by_camera": minima, "missing": missing, "latency": distributions, "failures": failures}
        return required, observed, "FAIL" if failures else "PASS" if complete else "INCOMPLETE"

    def media(self):
        required = {"decoded_viewer_sessions": self.workload["viewer_sessions"],
                    "snapshot_requests_per_minute": self.workload["snapshot_requests_per_minute"],
                    "snapshot_decode_fraction": 1, "evidence_fps": self.workload["evidence_fps"],
                    "maximum_viewer_gap_ms": self.budgets["viewer_recovery_ms"]}
        if not self.rows or not self.start:
            return required, {}, "INCOMPLETE"
        main_streams = {stream["id"]: stream["detail"] for stream in self.sources["sources"] if "detail" in stream}
        detail_geometry = {str(index): detail_output_dimensions(main_streams[viewer["camera"]])
                           for index, viewer in enumerate(self.sources["viewers"])
                           if viewer["profile"] == "detail" and viewer["camera"] in main_streams}
        if detail_geometry:
            required["detail_frame_geometry"] = detail_geometry
        last = self.rows[-1]
        start = self.start["viewer_frames"]
        viewers = {key: frames - start.get(key, 0) for key, frames in last["viewer_frames"].items()}
        expected_ids = {str(index) for index in range(required["decoded_viewer_sessions"])}
        gaps = viewer_gaps_ms(self.rows, start) if start.keys() == expected_ids else None
        stalled = sorted(key for key, gap in (gaps or {}).items() if gap > required["maximum_viewer_gap_ms"])
        snapshots = last["snapshots"]
        observed = {"decoded_frames_by_viewer": viewers, "snapshot_requests": snapshots, "snapshots_decoded": last["snapshots_ok"],
                    "expected_snapshot_requests": last["tick"] * required["snapshot_requests_per_minute"] // 60,
                    "maximum_observed_viewer_gap_ms": gaps, "stalled_viewers": stalled}
        geometries = last.get("viewer_geometry", {})
        if geometries:
            observed["frame_geometry_by_viewer"] = geometries
        failures = (self.end.get("snapshot_failures", []) if self.end is not None else
                    (self.partial_snapshots or {}).get("failures", []))
        if failures:
            observed["snapshot_failures"] = failures
        if any(key in geometries and geometries[key] != size for key, size in detail_geometry.items()):
            return required, observed, "FAIL"
        if stalled or failures or snapshots != last["snapshots_ok"]:
            return required, observed, "FAIL"
        coverage = gaps is not None and all(count > 0 for count in viewers.values())
        if (not coverage or not detail_geometry.keys() <= geometries.keys()
                or observed["expected_snapshot_requests"] != snapshots or self.interval()[2] != "PASS"):
            return required, observed, "INCOMPLETE"
        if self.short:
            required.update(viewer_renewals=self.limits["viewer_renewal_at_seconds"],
                            access_rotations=self.limits["minimum_real_access_rotations"])
            renewed, rotated = self.short_media(expected_ids)
            observed.update(verified_viewer_renewals=renewed, access_rotation_verified=rotated)
            if renewed != required["viewer_renewals"] or not rotated:
                return required, observed, "INCOMPLETE"
        return required, observed, "PASS"

    def short_media(self, viewer_ids):
        scheduled = self.limits["viewer_renewal_at_seconds"]
        last = self.rows[-1]
        renewals = self.end.get("renewal_evidence", [])
        verified = []
        if (isinstance(renewals, list) and all(isinstance(item, dict) for item in renewals)
                and [item.get("tick") for item in renewals] == scheduled
                and self.end.get("viewer_renewals") == len(scheduled)):
            after = [item.get("frames_before", {}) for item in renewals[1:]] + [last["viewer_frames"]]
            for renewal, following in zip(renewals, after, strict=True):
                tick = renewal["tick"]
                counters = renewal.get("frames_before", {})
                at = renewal.get("elapsed_s")
                if (isinstance(counters, dict) and isinstance(following, dict)
                        and number(at) and tick <= at <= self.rows[tick]["elapsed_s"]
                        and counters.keys() == following.keys() == viewer_ids
                        and all(type(count) is int and type(following[key]) is int
                                and self.rows[tick - 1]["viewer_frames"][key] <= count
                                <= self.rows[tick]["viewer_frames"][key] and following[key] > count
                                for key, count in counters.items())):
                    verified.append(tick)
        rotations = [row.get("credential_rotations") for row in self.rows]
        counts = self.end.get("decoded_snapshots_by_rotation", {})
        final = rotations[-1]
        generations = {str(value) for value in rotations if type(value) is int}
        rotated = (isinstance(counts, dict) and all(type(value) is int for value in rotations) and rotations[0] == 0
                   and all(0 <= b - a <= 1 for a, b in zip(rotations, rotations[1:]))
                   and final == self.end.get("credential_rotations")
                   and final >= self.limits["minimum_real_access_rotations"]
                   and all(key in generations and type(value) is int and value > 0
                           for key, value in counts.items())
                   and sum(counts.values()) == last["snapshots_ok"]
                   and "0" in counts and str(final) in counts)
        return verified, rotated

    def resources(self):
        budgets = self.budgets
        caps = resource_caps({"budgets": budgets, "models": self.model})
        if self.short:
            return self.short_resources(caps)
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
        for key in sorted({"rss_bytes", "disk_bytes", "vram_bytes"} & series.keys()):
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

    def short_resources(self, caps):
        limits = self.limits
        period = limits["resource_sample_period_seconds"]
        duration = limits["minimum_seconds"]
        budget = self.budgets["short_growth_bytes"]["steady"]
        samples = {row["tick"]: row["resources"] for row in self.rows if row["resources"] is not None}
        expected_ticks = set(range(0, duration + 1, period))
        complete = (self.interval()[2] == "PASS" and samples.keys() == expected_ticks
                    and all(all(number(sample.get(key)) for key in caps) for sample in samples.values()))
        peaks = {key: max((sample[key] for sample in samples.values() if number(sample.get(key))), default=None)
                 for key in caps}
        failures = [key for key, peak in peaks.items() if peak is not None and peak > caps[key]]
        growth = {}
        if complete:
            first = range(0, 60, period)
            last = range(duration - 60, duration, period)
            for key in sorted(caps.keys() & {"rss_bytes", "vram_bytes"}):
                growth[key] = (statistics.median(samples[tick][key] for tick in last)
                               - statistics.median(samples[tick][key] for tick in first))
                if growth[key] > budget[key]:
                    failures.append(key + "/growth")
        return ({"resource_max_with_headroom": caps, "short_growth_bytes": budget,
                 "samples": limits["minimum_resource_samples"]},
                {"peaks": peaks, "growth_bytes": growth, "sample_count": len(samples), "failures": failures},
                "FAIL" if failures else "PASS" if complete else "INCOMPLETE")

    def detections(self):
        canaries = self.sources.get("canaries", [])
        required = {"expected_known_canaries": len(canaries), "active_live_detector": True}
        if not self.end or not canaries:
            return required, {}, "INCOMPLETE"
        started_utc = self.end.get("started_utc_ms")
        if not number(started_utc) or any("opening_observed_host_utc_ms" not in event for event in self.end["openings"]):
            return required, {"opening_frame_time_recorded": False}, "INCOMPLETE"
        missed = []
        for canary in canaries:
            if not any(event["live"] and event["category"] == canary["category"] and event["camera"] == canary["camera"]
                       and fresh_opening(event, started_utc)
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

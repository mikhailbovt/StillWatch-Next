"""Inject a frozen sequence of native failures and capture an end-to-end canary after each."""

import argparse
import asyncio
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import sqlite3
from tempfile import TemporaryDirectory
import time
from uuid import UUID, uuid4

import psutil

from fast_rc_contract import (
    CONTRACTS, camera_quiet, camera_recovered, checked_json, consecutive_quiet_samples, evidence, fresh_opening,
    inference_camera_interrupted, inference_cameras_recovered, inference_worker_replaced, native_clock, quiet_fire_sample,
    quiet_requirements, sha256, write_json,
)
from process_identity import PROCESS_START_CLOCK, process_identity
from qualify_resources import ChurnResources
from qualify_steady import decodes_jpeg, latest, native_fixture, openings, pair_owner
from lan_core import refresh_request
from replay_core import until
from whep_core import connect, request

LIMITS = {"rtsp_reconnect": "rtsp_recovery_ms", "inference_child_restart": "inference_recovery_ms",
          "core_restart": "core_recovery_ms", "viewer_grant_offer_close": "viewer_recovery_ms"}


def inference_children(process):
    result = {}
    for child in psutil.Process(process.pid).children(recursive=True):
        try:
            if "infer-v2-proposals" in child.cmdline():
                result[process_identity(child)] = child
        except psutil.NoSuchProcess:
            continue
    return result


async def viewer_cycle(fixture, receiver, name, resources):
    lan, session, _, _ = receiver
    peer, track, grant = await connect(lan, session, fixture["cameras"][name], None, None, dimensions=None, verify_mask=False)
    try:
        first = await asyncio.wait_for(track.recv(), timeout=5)
        second = await asyncio.wait_for(track.recv(), timeout=5)
        if first.pts == second.pts:
            raise ValueError("The viewer did not decode two distinct frame timestamps")
        if resources is not None:
            await asyncio.to_thread(resources.record, "viewer_active")
        code, _, _ = await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"])
        if code != 204:
            raise ValueError(f"Viewer close returned {code}")
        return {"viewer_frames_decoded": 2, "stream_closed_status": code}
    finally:
        await peer.close()


def inject(fixture, cases, receiver, resources=None):
    kind = cases[0]["kind"]
    if kind == "rtsp_reconnect":
        stopped = {case["camera"]: fixture["publishers"][case["camera"]] for case in cases}
        for process in stopped.values():
            process.terminate()
        for process in stopped.values():
            process.wait(timeout=10)
        local = fixture["local"]
        cameras = fixture["cameras"]
        until(lambda: all(local.camera(cameras[name])["runtime"]["state"] != "streaming" for name in stopped), 15)
        for name in stopped:
            fixture["publishers"][name] = fixture["start_publisher"](name)
        return {"stopped_publishers": len(stopped)}
    if kind == "inference_child_restart":
        previous = inference_children(fixture["process"])
        if not previous:
            raise ValueError("No inference worker child was found")
        ident = min(previous)
        previous[ident].kill()
        previous[ident].wait(timeout=10)
        return {"retired_worker": list(ident), "workers_before": [list(identity) for identity in sorted(previous)],
                "process_start_clock": PROCESS_START_CLOCK}
    if kind == "core_restart":
        old = fixture["process"]
        old.kill()
        old.wait(timeout=10)
        fixture["process"], fixture["local"] = fixture["start_core"]()
        return {"retired_core_pid": old.pid, "replacement_core_pid": fixture["process"].pid}
    return asyncio.run(viewer_cycle(fixture, receiver, cases[0]["camera"], resources))


def refresh_receiver(settings, receiver):
    lan, session, offer, key = receiver
    if (datetime.fromisoformat(session["access_expires_at"]) - datetime.now(timezone.utc)).total_seconds() > 90:
        return
    proof = refresh_request(settings["node"], key, offer, session)
    code, replacement = lan.request("POST", "/api/v2/device-session/refresh", proof)
    if code != 200:
        raise ValueError(f"Canary receiver refresh returned {code}")
    session.update(replacement)


def delivery_for(lan, token, incident):
    next_page = "/api/v2/alerts"
    for _ in range(32):
        code, page = lan.request("GET", next_page, token=token)
        if code != 200:
            return None
        for alert in page["alerts"]:
            if alert["incident_id"] == incident:
                return alert
        if not page.get("next"):
            break
        next_page = "/api/v2/alerts?after=" + page["next"]
    return None


def read_canary_overview(lan, token, incident):
    packet_id = incident["evidence_packet_id"]
    if packet_id is None:
        return None
    path = f"/api/v2/incidents/{incident['id']}/evidence/{packet_id}"
    status, packet = lan.request("GET", path, token=token)
    if (status != 200 or packet["packet_id"] != packet_id or packet["incident_id"] != incident["id"]
            or any(packet[key] != incident[key] for key in ("camera_id", "source_epoch", "evidence_version"))):
        return None
    image = max((image for image in packet["images"]
                 if image["kind"] == "overview" and image["availability"] == "available"),
                key=lambda image: image["source_s"], default=None)
    if image is None:
        return None
    code, headers, jpeg = request(lan, token, "GET", f"{path}/{image['ordinal']}")
    media_type = next((value for name, value in headers.items() if name.lower() == "content-type"), None)
    if (code != 200 or media_type != "image/jpeg" or hashlib.sha256(jpeg).hexdigest() != image["sha256"]
            or not decodes_jpeg(jpeg)):
        return None
    return {"incident_id": incident["id"], "packet_id": packet_id, "ordinal": image["ordinal"],
            "source_epoch": incident["source_epoch"], "sha256": image["sha256"], "bytes": len(jpeg)}, jpeg


def canary(database, fixture, receiver, expected, earliest_utc, *, image_path=None):
    events = openings(database, fixture["cameras"], earliest_utc)
    lan, session, _, _ = receiver
    token = session["access_token"]
    for event in events:
        if (not event["live"] or event["camera"] != expected["camera"] or event["category"] != expected["category"]
                or not fresh_opening(event, earliest_utc)):
            continue
        if (int(event["epoch"]) != int(expected["source_epoch"])
                or not expected["earliest_source_us"] <= int(event["source_us"]) <= expected["latest_source_us"]):
            continue
        status, incident = lan.request("GET", f"/api/v2/incidents/{event['id']}", token=token)
        if (status != 200 or incident["acknowledgement"] is not None or incident["evidence_state"] != "persisted"
                or int(incident["source_epoch"]) != int(event["epoch"])):
            continue
        alert = delivery_for(lan, token, event["id"])
        if alert is None:
            continue
        overview = read_canary_overview(lan, token, incident) if image_path is not None else None
        if image_path is not None and overview is None:
            continue
        code, _ = lan.request("POST", f"/api/v2/alerts/{alert['id']}/seen", {"attention_cycle": alert["attention_cycle"]}, token=token)
        if code != 204:
            continue
        device = UUID(session["device"]["id"]).bytes
        with closing(sqlite3.connect(database)) as db:
            receipt = db.execute("SELECT client_seen_at FROM notification_receipts WHERE alert_id=? AND device_id=?",
                                 (int(alert["id"]), device)).fetchone()
        if receipt is not None:
            proof = {"planned": expected, "incidents": [event | {"source_epoch": str(event["epoch"]),
                                                                "evidence_state": incident["evidence_state"],
                                                                "receipt_utc_ms": receipt[0], "acknowledged": False}]}
            if overview is not None:
                metadata, jpeg = overview
                with image_path.open("xb") as handle:
                    handle.write(jpeg)
                proof["overview"] = metadata | {"artifact": evidence(image_path.parent, image_path)}
            return proof
    return None


def close_canary(local, incident):
    path = f"/api/v2/incidents/{incident}"
    code, active = local.call("GET", path)
    if code != 200:
        raise RuntimeError("The completed canary could not be opened for cleanup")
    if active["resolution"] is not None:
        return False
    closed, _ = local.call("POST", path + "/resolution", {"reason": "Fast RC controlled canary completed"},
                           key=uuid4().hex, revision=active["revision"])
    if closed != 200:
        raise RuntimeError(f"The completed canary could not be resolved: {closed}")
    return True


def prepare_canary(database, fixture, case):
    if not case.get("resolve_before_fault"):
        return 0
    planned = case["canary"]
    camera = UUID(fixture["cameras"][planned["camera"]]).bytes
    with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)) as db:
        rows = db.execute("SELECT id FROM incidents WHERE camera_id = ? AND resolved = 0 "
                          "AND json_extract(state, '$.live') = 1 AND json_extract(state, '$.category') = ?",
                          (camera, planned["category"])).fetchall()
    return sum(close_canary(fixture["local"], str(UUID(bytes=ident))) for ident, in rows)


def batches(schedule):
    pending = deque(schedule)
    while pending:
        case = pending.popleft()
        batch = [case]
        if case["kind"] == "rtsp_reconnect" and "batch" in case:
            while pending and pending[0]["kind"] == case["kind"] and pending[0].get("batch") == case["batch"]:
                batch.append(pending.popleft())
        if len({entry["camera"] for entry in batch}) != len(batch):
            raise ValueError("A fault batch repeats a camera")
        yield batch


def fault_clock(settings):
    supplied = settings.get("elapsed_clock")
    if supplied is None:
        return time.monotonic
    clock_name, now = native_clock()
    if supplied != clock_name:
        raise ValueError("The fault origin was recorded on a different clock")
    return now


def run(settings, output, evidence_root):
    output.mkdir(parents=True, exist_ok=False)
    ledger = output / "ledger.jsonl"
    schedule_path = Path(settings["fault_schedule"])
    profile = checked_json(Path(settings["profile"]))
    if sha256(schedule_path) != profile["workload"]["fault_schedule_digest"]:
        raise ValueError("Fault schedule does not match the declared workload")
    schedule = checked_json(schedule_path)["faults"]
    if not schedule or len(schedule) > 1_024:
        raise ValueError("The frozen fault schedule must contain between one and 1024 operations")
    for case in schedule:
        if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,79}", case["id"]) or case["kind"] not in LIMITS:
            raise ValueError("Invalid fault label or kind")
        if case["canary"]["camera"] != case["camera"]:
            raise ValueError("The canary must demonstrate recovery on the camera named for the fault")
        if type(case.get("resolve_before_fault", False)) is not bool:
            raise ValueError("The canary preparation flag must be a boolean")
        quiet_requirements(case)
    if len({case["id"] for case in schedule}) != len(schedule):
        raise ValueError("Fault schedule contains a duplicate ID")

    now = fault_clock(settings)

    def offset():
        return now() - settings["monotonic_origin"]

    with TemporaryDirectory(prefix="stillwatch-faults-") as scratch, ledger.open("x", encoding="utf-8", buffering=1) as log:
        root = Path(scratch)
        database = root / "data/stillwatch.sqlite3"
        with native_fixture(settings, root, use_canary_media=True) as fixture:
            def ready():
                states = [latest(camera, fixture["local"]) for camera in fixture["cameras"].values()]
                return all(row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready" for row in states)

            until(ready, 60, clock=now)
            receiver = pair_owner(settings, fixture["local"], root)
            resources = ChurnResources(fixture, settings, root, receiver)
            for batch in batches(schedule):
                refresh_receiver(settings, receiver)
                preparation_utc = time.time_ns() // 1_000_000
                kind = batch[0]["kind"]
                affected = {case["camera"]: fixture["cameras"][case["camera"]] for case in batch}
                tracked = fixture["cameras"] if kind == "inference_child_restart" else affected
                before = None
                quiet, quiet_duration_us = quiet_requirements(batch[0])
                quiet_observations = []
                if quiet:
                    case, = batch
                    name = case["camera"]
                    prepared = {case["id"]: 0}
                    last_sample = None
                    longest_quiet_us = 0

                    def quiet_sample():
                        nonlocal last_sample, longest_quiet_us
                        sample = latest(affected[name], fixture["local"])
                        last_sample = sample
                        if quiet_duration_us == 0:
                            if not camera_quiet(sample):
                                return None
                            prepared[case["id"]] = prepare_canary(database, fixture, case)
                            return sample
                        cursor = quiet_fire_sample(sample)
                        if cursor is None:
                            quiet_observations.clear()
                            return None
                        if quiet_observations:
                            previous = quiet_fire_sample(quiet_observations[-1])
                            if previous == cursor:
                                return None
                            if not consecutive_quiet_samples(previous, cursor):
                                quiet_observations.clear()
                        if not quiet_observations:
                            # Keep the next opening available for the fault canary.
                            # Resolving during the preceding active scene can open
                            # another episode before this quiet interval begins.
                            prepared[case["id"]] += prepare_canary(database, fixture, case)
                        quiet_observations.append(sample)
                        if len(quiet_observations) > 32:
                            quiet_observations.pop(0)
                        elapsed_us = cursor[2] - quiet_fire_sample(quiet_observations[0])[2]
                        longest_quiet_us = max(longest_quiet_us, elapsed_us)
                        return sample if elapsed_us >= quiet_duration_us else None

                    began = offset()
                    try:
                        before = {name: until(quiet_sample, 20, clock=now)}
                    except AssertionError:
                        path = output / (case["id"] + ".json")
                        write_json(path, {"planned": case["canary"], "required_quiet_source_us": quiet_duration_us,
                                          "longest_quiet_source_us": longest_quiet_us, "last_sample": last_sample,
                                          "quiet_observations": quiet_observations})
                        log.write(json.dumps({"id": case["id"], "kind": kind, "camera": name, "started_s": began,
                                              "ended_s": offset(), "finished_s": None, "error": "Quiet scene precondition expired",
                                              "precondition": evidence(evidence_root, path)}) + "\n")
                        continue
                else:
                    prepared = {case["id"]: prepare_canary(database, fixture, case) for case in batch}
                started = offset()
                began_utc = time.time_ns() // 1_000_000
                deadline = started + profile["budgets"][LIMITS[kind]] / 1_000
                if before is None:
                    before = {name: latest(camera, fixture["local"]) for name, camera in tracked.items()}
                try:
                    fault = inject(fixture, batch, receiver, resources)
                except (OSError, RuntimeError, ValueError, TimeoutError, AssertionError) as error:
                    for case in batch:
                        log.write(json.dumps({"id": case["id"], "kind": kind, "camera": case["camera"],
                                              "started_s": started, "ended_s": offset(), "finished_s": None,
                                              "error": str(error), "deadline_s": deadline}) + "\n")
                    continue
                outstanding = {case["camera"]: case for case in batch}
                completed = []
                window = {case["camera"]: None for case in batch}
                recovery = None
                if kind == "inference_child_restart":
                    recovery = {"before": before, "interrupted": {}, "after": {}}
                while outstanding and offset() <= deadline:
                    if recovery is not None:
                        recovery["after"] = {name: latest(camera, fixture["local"]) for name, camera in tracked.items()}
                        for name, current in recovery["after"].items():
                            if inference_camera_interrupted(before[name], current):
                                recovery["interrupted"].setdefault(name, current)
                        workers = inference_children(fixture["process"])
                        fault["workers_after"] = [list(identity) for identity in sorted(workers)]
                        if (not inference_worker_replaced(fault, profile["models"]["workers"])
                                or not inference_cameras_recovered(recovery, tracked.keys())):
                            time.sleep(0.2)
                            continue
                    for name, case in list(outstanding.items()):
                        current = (recovery["after"][name] if recovery is not None
                                   else latest(affected[name], fixture["local"]))
                        if not camera_recovered(before[name], current, kind in {"rtsp_reconnect", "core_restart"}):
                            continue
                        window[name] = window[name] or offset()
                        path = output / (case["id"] + ".json")
                        verified = canary(database, fixture, receiver, case["canary"], began_utc,
                                          image_path=path.with_suffix(".jpg"))
                        if verified is None:
                            continue
                        write_json(path, verified | {"started_utc_ms": began_utc, "source_before": before[name],
                                                      "source_after": current, "inference_recovery": recovery,
                                                      "quiet_observations": quiet_observations})
                        row = {"id": case["id"], "kind": kind, "camera": name, "started_s": started,
                               "finished_s": offset(), "capture_progress_s": window[name], "fault": fault,
                               "prior_incidents_resolved": prepared[case["id"]],
                               "canary": evidence(evidence_root, path)}
                        log.write(json.dumps(row) + "\n")
                        completed.append(case["id"])
                        outstanding.pop(name)
                        close_canary(fixture["local"], verified["incidents"][0]["id"])
                    if outstanding:
                        time.sleep(0.2)
                for name, case in outstanding.items():
                    path = output / (case["id"] + ".json")
                    observed = openings(database, fixture["cameras"], preparation_utc)
                    write_json(path, {"planned": case["canary"], "started_utc_ms": began_utc,
                                      "preparation_utc_ms": preparation_utc, "source_before": before[name],
                                      "source_after": latest(affected[name], fixture["local"]),
                                      "inference_recovery": recovery,
                                      "quiet_observations": quiet_observations,
                                      "pre_fault_openings": [event for event in observed
                                                             if event["camera"] == name and event["utc_ms"] < began_utc],
                                      "incidents": [event for event in observed if event["utc_ms"] >= began_utc]})
                    log.write(json.dumps({"id": case["id"], "kind": kind, "camera": name, "started_s": started,
                                          "ended_s": offset(), "finished_s": None, "capture_progress_s": window[name],
                                          "deadline_s": deadline, "fault": fault,
                                          "prior_incidents_resolved": prepared[case["id"]],
                                          "canary": evidence(evidence_root, path),
                                          "error": "Recovery did not complete with the pinned live canary"}) + "\n")
                resources.record(kind, completed)
            resources.drain(output / "churn-resources.json")


def final_canary(settings, output):
    clock_name, now = native_clock()
    profile = checked_json(Path(settings["profile"]))
    schedule = Path(settings["fault_schedule"])
    if sha256(schedule) != profile["workload"]["fault_schedule_digest"]:
        raise ValueError("Final canary schedule differs from the frozen workload")
    planned = checked_json(schedule)["final_canary"]
    limit_ms = profile["budgets"]["core_recovery_ms"]
    with TemporaryDirectory(prefix="stillwatch-final-") as scratch:
        root = Path(scratch)
        database = root / "data/stillwatch.sqlite3"
        started = now()
        earliest = time.time_ns() // 1_000_000
        deadline = started + limit_ms / 1_000
        with native_fixture(settings, root, use_canary_media=True) as fixture:
            receiver = pair_owner(settings, fixture["local"], root)
            ready = []
            verified = None
            while now() <= deadline:
                states = {name: latest(camera, fixture["local"]) for name, camera in fixture["cameras"].items()}
                ready = [name for name, row in states.items()
                         if row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready"
                         and all(int(row["analysis"][key]) > 0 for key in ("pose_frames", "fire_frames"))]
                if len(ready) == len(states):
                    verified = canary(database, fixture, receiver, planned, earliest,
                                      image_path=output.with_suffix(".jpg"))
                    if verified is not None:
                        break
                time.sleep(0.2)
            elapsed = (now() - started) * 1_000
            if verified is not None:
                close_canary(fixture["local"], verified["incidents"][0]["id"])
            with closing(sqlite3.connect(database)) as db:
                integrity = [status for status, in db.execute("PRAGMA integrity_check")]
                invalid_references = len(db.execute("PRAGMA foreign_key_check").fetchall())
            write_json(output, (verified or {"planned": planned, "incidents": []}) | {
                "started_utc_ms": earliest, "elapsed_ms": elapsed, "elapsed_clock": clock_name,
                "ready_cameras": ready, "integrity": integrity, "invalid_references": invalid_references})


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("settings", "out", "evidence-root"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args(argv)
    run(checked_json(args.settings), args.out, args.evidence_root.resolve(strict=True))


def capacity(settings):
    clock_name, now = native_clock()
    with TemporaryDirectory(prefix="stillwatch-ramp-") as scratch:
        with native_fixture(settings, Path(scratch), start_enabled=False) as fixture:
            local = fixture["local"]
            cameras = list(fixture["cameras"].values())
            limits = checked_json(CONTRACTS["native"])["capacity_ramp_cameras"]
            targets = sorted({*[count for count in limits if count <= len(cameras)], len(cameras)})
            samples = []

            def enable(camera, value, key=None):
                revision = local.camera(camera)["revision"]
                return local.call("PATCH", f"/api/v2/cameras/{camera}", {"enabled": value}, revision=revision,
                                  key=key or uuid4().hex)

            def live(ids):
                states = [latest(camera, local) for camera in ids]
                if all(row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready"
                       and all(int(row["analysis"][key]) >= 5 for key in ("pose_frames", "fire_frames")) for row in states):
                    return states
                return None

            active = 0
            for target in targets:
                while active < min(target, len(cameras) - 1):
                    status, _ = enable(cameras[active], True)
                    assert status == 200
                    active += 1
                if active == target:
                    states = until(lambda: live(cameras[:active]), 45, clock=now)
                    samples.append({"count": target, "streams_with_pose_and_fire": len(states)})
            template = local.camera(cameras[-1])["config"]
            code, spare = local.call("POST", "/api/v2/cameras", template | {"name": "last-slot-challenger", "enabled": False},
                                     key=uuid4().hex)
            assert code == 201
            competing_ids = [cameras[-1], spare["id"]]
            with ThreadPoolExecutor(max_workers=2) as pool:
                outcomes = list(pool.map(lambda camera: enable(camera, True), competing_ids))
            statuses = [status for status, _ in outcomes]
            if sorted(statuses) != [200, 503]:
                raise AssertionError(f"Last-slot admission returned {statuses}")
            rejected = outcomes[statuses.index(503)][1]
            assert rejected["code"] == "capacity_exceeded"
            winner, waiting = competing_ids[statuses.index(200)], competing_ids[statuses.index(503)]
            active_ids = [*cameras[:-1], winner]
            live_at_max = until(lambda: live(active_ids), 45, clock=now)
            samples.append({"count": len(cameras), "streams_with_pose_and_fire": len(live_at_max)})
            def excess_request(_):
                started = now()
                response = enable(waiting, True)
                return response, (now() - started) * 1_000

            with ThreadPoolExecutor(max_workers=4) as pool:
                excess = list(pool.map(excess_request, range(16)))
            rejected_count = sum(status == 503 and payload["code"] == "capacity_exceeded"
                                 for (status, payload), _ in excess)
            limit = fixture["profile"]["budgets"]["rtsp_recovery_ms"] / 1_000
            health = local.call("GET", "/health/ready")[0]

            def progressing():
                current = [latest(camera, local) for camera in active_ids]
                return current if all(camera_recovered(before, after, False)
                                      for before, after in zip(live_at_max, current, strict=True)) else None

            progress = until(progressing, limit, clock=now)
            released = now()
            assert enable(winner, False)[0] == 200
            cell = uuid4().hex
            old_revision = local.camera(waiting)["revision"]
            request_body = {"enabled": True}
            first = local.call("PATCH", f"/api/v2/cameras/{waiting}", request_body, key=cell, revision=old_revision)
            duplicate = local.call("PATCH", f"/api/v2/cameras/{waiting}", request_body, key=cell, revision=old_revision)
            assert first[0] == duplicate[0] == 200, (first[0], duplicate[0])
            assert all(first[1][key] == duplicate[1][key] for key in ("id", "revision", "config"))
            assert enable(winner, True)[0] == 503
            restored = until(lambda: live([*cameras[:-1], waiting]), limit, clock=now)
            print(json.dumps({"ramps": samples, "last_slot_statuses": statuses, "repeated_result": True,
                              "camera_slot_reuse_with_live_analysis": len(restored),
                              "admission_recovery_ms": (now() - released) * 1_000, "elapsed_clock": clock_name,
                              "excess_requests": len(excess), "excess_rejected": rejected_count,
                              "excess_statuses": [status for (status, _), _ in excess],
                              "excess_latency_ms": [duration for _, duration in excess],
                              "control_ready_http": health, "active_cameras_progressed": len(progress)}))

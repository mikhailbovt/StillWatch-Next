"""Inject a frozen sequence of native failures and capture an end-to-end canary after each."""

import argparse
import asyncio
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sqlite3
from tempfile import TemporaryDirectory
import time
from uuid import UUID, uuid4

import psutil

from fast_rc_contract import CONTRACTS, checked_json, evidence, sha256, write_json
from qualify_steady import latest, native_fixture, openings, pair_owner
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
                result[(child.pid, child.create_time())] = child
        except psutil.NoSuchProcess:
            continue
    return result


def recovered(before, after, epoch_changes):
    if after["capture_state"] != "streaming" or not after["analysis"] or after["analysis"]["state"] != "ready":
        return False
    previous_epoch, new_epoch = int(before["epoch"]), int(after["epoch"])
    if epoch_changes:
        return new_epoch > previous_epoch and all(int(after["analysis"][key]) > 0 for key in ("pose_frames", "fire_frames"))
    return (new_epoch == previous_epoch and int(after["frame"]) > int(before["frame"])
            and all(int(after["analysis"][key]) > int(before["analysis"][key]) for key in ("pose_frames", "fire_frames")))


async def viewer_cycle(fixture, receiver, name):
    lan, session, _, _ = receiver
    peer, track, grant = await connect(lan, session, fixture["cameras"][name], None, None, dimensions=None, verify_mask=False)
    try:
        first = await asyncio.wait_for(track.recv(), timeout=5)
        second = await asyncio.wait_for(track.recv(), timeout=5)
        if first.pts == second.pts:
            raise ValueError("The viewer did not decode two distinct frame timestamps")
        code, _, _ = await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"])
        if code != 204:
            raise ValueError(f"Viewer close returned {code}")
        return {"viewer_frames_decoded": 2, "stream_closed_status": code}
    finally:
        await peer.close()


def inject(fixture, cases, receiver):
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
        until(lambda: bool(inference_children(fixture["process"]).keys() - previous.keys()), 65)
        return {"reaped_child_pid": ident[0], "replacement_pids": sorted(pid for pid, _ in inference_children(fixture["process"]))}
    if kind == "core_restart":
        old = fixture["process"]
        old.kill()
        old.wait(timeout=10)
        fixture["process"], fixture["local"] = fixture["start_core"]()
        return {"retired_core_pid": old.pid, "replacement_core_pid": fixture["process"].pid}
    return asyncio.run(viewer_cycle(fixture, receiver, cases[0]["camera"]))


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


def canary(database, fixture, receiver, expected, earliest_utc):
    events = openings(database, fixture["cameras"], earliest_utc)
    lan, session, _, _ = receiver
    token = session["access_token"]
    for event in events:
        if not event["live"] or event["camera"] != expected["camera"] or event["category"] != expected["category"]:
            continue
        if (int(event["epoch"]) != int(expected["source_epoch"])
                or not expected["earliest_source_us"] <= int(event["source_us"]) <= expected["latest_source_us"]):
            continue
        status, incident = lan.request("GET", f"/api/v2/incidents/{event['id']}", token=token)
        if status != 200 or incident["acknowledgement"] is not None or incident["evidence_state"] != "persisted":
            continue
        alert = delivery_for(lan, token, event["id"])
        if alert is None:
            continue
        code, _ = lan.request("POST", f"/api/v2/alerts/{alert['id']}/seen", {"attention_cycle": alert["attention_cycle"]}, token=token)
        if code != 204:
            continue
        device = UUID(session["device"]["id"]).bytes
        with closing(sqlite3.connect(database)) as db:
            receipt = db.execute("SELECT client_seen_at FROM notification_receipts WHERE alert_id=? AND device_id=?",
                                 (int(alert["id"]), device)).fetchone()
        if receipt is not None:
            return {"planned": expected, "incidents": [event | {"source_epoch": str(event["epoch"]),
                                                                  "evidence_state": incident["evidence_state"],
                                                                  "receipt_utc_ms": receipt[0], "acknowledged": False}]}
    return None


def close_canary(local, verified):
    incident, = verified["incidents"]
    path = f"/api/v2/incidents/{incident['id']}"
    code, active = local.call("GET", path)
    if code != 200:
        raise RuntimeError("The completed canary could not be opened for cleanup")
    if active["resolution"] is not None:
        return
    closed, _ = local.call("POST", path + "/resolution", {"reason": "Fast RC controlled canary completed"},
                           key=uuid4().hex, revision=active["revision"])
    if closed != 200:
        raise RuntimeError(f"The completed canary could not be resolved: {closed}")


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
    if len({case["id"] for case in schedule}) != len(schedule):
        raise ValueError("Fault schedule contains a duplicate ID")

    def offset():
        return time.monotonic() - settings["monotonic_origin"]

    with TemporaryDirectory(prefix="stillwatch-faults-") as scratch, ledger.open("x", encoding="utf-8", buffering=1) as log:
        root = Path(scratch)
        database = root / "data/stillwatch.sqlite3"
        with native_fixture(settings, root) as fixture:
            def ready():
                states = [latest(camera, fixture["local"]) for camera in fixture["cameras"].values()]
                return all(row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready" for row in states)

            until(ready, 60)
            receiver = pair_owner(settings, fixture["local"], root)
            for batch in batches(schedule):
                refresh_receiver(settings, receiver)
                started = offset()
                began_utc = time.time_ns() // 1_000_000
                kind = batch[0]["kind"]
                deadline = started + profile["budgets"][LIMITS[kind]] / 1_000
                before = {case["camera"]: latest(fixture["cameras"][case["camera"]], fixture["local"]) for case in batch}
                affected = {case["camera"]: fixture["cameras"][case["camera"]] for case in batch}
                try:
                    fault = inject(fixture, batch, receiver)
                except (OSError, RuntimeError, ValueError, TimeoutError, AssertionError) as error:
                    for case in batch:
                        log.write(json.dumps({"id": case["id"], "kind": kind, "camera": case["camera"],
                                              "started_s": started, "ended_s": offset(), "finished_s": None,
                                              "error": str(error), "deadline_s": deadline}) + "\n")
                    continue
                outstanding = {case["camera"]: case for case in batch}
                window = {case["camera"]: None for case in batch}
                while outstanding and offset() <= deadline:
                    for name, case in list(outstanding.items()):
                        current = latest(affected[name], fixture["local"])
                        if not recovered(before[name], current, kind in {"rtsp_reconnect", "core_restart"}):
                            continue
                        window[name] = window[name] or offset()
                        verified = canary(database, fixture, receiver, case["canary"], began_utc)
                        if verified is None:
                            continue
                        path = output / (case["id"] + ".json")
                        write_json(path, verified)
                        row = {"id": case["id"], "kind": kind, "camera": name, "started_s": started,
                               "finished_s": offset(), "capture_progress_s": window[name], "fault": fault,
                               "canary": evidence(evidence_root, path)}
                        log.write(json.dumps(row) + "\n")
                        outstanding.pop(name)
                        close_canary(fixture["local"], verified)
                    if outstanding:
                        time.sleep(0.2)
                for name, case in outstanding.items():
                    path = output / (case["id"] + ".json")
                    write_json(path, {"planned": case["canary"], "incidents": openings(database, fixture["cameras"], began_utc)})
                    log.write(json.dumps({"id": case["id"], "kind": kind, "camera": name, "started_s": started,
                                          "ended_s": offset(), "finished_s": None, "capture_progress_s": window[name],
                                          "deadline_s": deadline, "fault": fault,
                                          "canary": evidence(evidence_root, path),
                                          "error": "Recovery did not complete with the pinned live canary"}) + "\n")


def final_canary(settings, output):
    profile = checked_json(Path(settings["profile"]))
    schedule = Path(settings["fault_schedule"])
    if sha256(schedule) != profile["workload"]["fault_schedule_digest"]:
        raise ValueError("Final canary schedule differs from the frozen workload")
    planned = checked_json(schedule)["final_canary"]
    limit_ms = profile["budgets"]["core_recovery_ms"]
    with TemporaryDirectory(prefix="stillwatch-final-") as scratch:
        root = Path(scratch)
        database = root / "data/stillwatch.sqlite3"
        started = time.monotonic()
        earliest = time.time_ns() // 1_000_000
        deadline = started + limit_ms / 1_000
        with native_fixture(settings, root) as fixture:
            receiver = pair_owner(settings, fixture["local"], root)
            ready = []
            verified = None
            while time.monotonic() <= deadline:
                states = {name: latest(camera, fixture["local"]) for name, camera in fixture["cameras"].items()}
                ready = [name for name, row in states.items()
                         if row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready"
                         and all(int(row["analysis"][key]) > 0 for key in ("pose_frames", "fire_frames"))]
                if len(ready) == len(states):
                    verified = canary(database, fixture, receiver, planned, earliest)
                    if verified is not None:
                        break
                time.sleep(0.2)
            elapsed = (time.monotonic() - started) * 1_000
            if verified is not None:
                close_canary(fixture["local"], verified)
            with closing(sqlite3.connect(database)) as db:
                integrity = [status for status, in db.execute("PRAGMA integrity_check")]
                invalid_references = len(db.execute("PRAGMA foreign_key_check").fetchall())
            write_json(output, {"planned": planned, "started_utc_ms": earliest, "elapsed_ms": elapsed,
                                "ready_cameras": ready, "integrity": integrity, "invalid_references": invalid_references,
                                "incidents": verified["incidents"] if verified is not None else []})


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("settings", "out", "evidence-root"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args(argv)
    run(checked_json(args.settings), args.out, args.evidence_root.resolve(strict=True))


def capacity(settings):
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
                    states = until(lambda: live(cameras[:active]), 45)
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
            live_at_max = until(lambda: live([*cameras[:-1], winner]), 45)
            samples.append({"count": len(cameras), "streams_with_pose_and_fire": len(live_at_max)})
            assert enable(winner, False)[0] == 200
            cell = uuid4().hex
            old_revision = local.camera(waiting)["revision"]
            request_body = {"enabled": True}
            first = local.call("PATCH", f"/api/v2/cameras/{waiting}", request_body, key=cell, revision=old_revision)
            duplicate = local.call("PATCH", f"/api/v2/cameras/{waiting}", request_body, key=cell, revision=old_revision)
            assert first[0] == duplicate[0] == 200, (first[0], duplicate[0])
            assert all(first[1][key] == duplicate[1][key] for key in ("id", "revision", "config"))
            assert enable(winner, True)[0] == 503
            recovered = until(lambda: live([*cameras[:-1], waiting]), 45)
            print(json.dumps({"ramps": samples, "last_slot_statuses": statuses, "repeated_result": True,
                              "camera_slot_reuse_with_live_analysis": len(recovered)}))

"""Measure live evidence recovery after native storage faults in isolated data directories."""

from contextlib import closing, contextmanager
import errno
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
import time
from uuid import UUID

from fast_rc_contract import (
    DATABASE_PRESSURE_STATUS, MAX_BLOB_VOLUME_BYTES, camera_recovered, capture_progressed,
    checked_json, native_clock, sha256, write_json,
)
from qualify_faults import canary, close_canary, inject
from qualify_steady import latest, native_fixture, openings, pair_owner
from replay_core import until
from storage_budget import camera, pinned_wal, readiness

@contextmanager
def exhausted_volume(path):
    path.mkdir(parents=True)
    subprocess.run(["mount", "-t", "tmpfs", "-o", "size=4m,nr_inodes=1024,mode=0700,nosuid,nodev,noexec",
                    "stillwatch-rc-blobs", path], check=True)
    try:
        info = os.statvfs(path)
        size = info.f_blocks * info.f_frsize
        if not 0 < size <= MAX_BLOB_VOLUME_BYTES or path.stat().st_dev == path.parent.stat().st_dev:
            raise ValueError("The isolated blob volume does not have its own bounded filesystem")
        filler = path / "qualification-fill"
        with filler.open("xb") as handle:
            os.posix_fallocate(handle.fileno(), 0, info.f_bavail * info.f_frsize)
        with (path / "qualification-probe").open("xb") as handle:
            try:
                os.posix_fallocate(handle.fileno(), 0, info.f_frsize)
            except OSError as error:
                if error.errno != errno.ENOSPC:
                    raise
                code = error.errno
            else:
                raise AssertionError("A separate write did not reproduce ENOSPC")
        (path / "qualification-probe").unlink()
        yield filler, size, code
    finally:
        subprocess.run(["umount", path], check=True)


@contextmanager
def exhausted_windows_volume(path, volume):
    from ctypes import WinDLL, WinError, create_unicode_buffer, get_last_error

    if volume != Path(volume.anchor) or len(volume.drive) != 2 or not volume.is_mount():
        raise ValueError("Supply the drive root of a dedicated Windows test volume")
    path.parent.mkdir(parents=True, exist_ok=True)
    size = shutil.disk_usage(volume).total
    device = volume.stat().st_dev
    if not 0 < size <= MAX_BLOB_VOLUME_BYTES or device == path.parent.stat().st_dev:
        raise ValueError("The Windows blob fixture requires a dedicated bounded volume of at most 4 MiB")
    if any(entry.name.casefold() not in {"system volume information", "$recycle.bin"}
           for entry in volume.iterdir()):
        raise ValueError("The Windows blob fixture requires an empty test volume")
    api = WinDLL("kernel32", use_last_error=True)
    volume_name = create_unicode_buffer(50)
    if not api.GetVolumeNameForVolumeMountPointW(str(volume), volume_name, len(volume_name)):
        raise WinError(get_last_error())
    path.mkdir()
    mount = str(path.resolve()) + "\\"
    with TemporaryDirectory(prefix="stillwatch-blobs-", dir=volume) as scratch:
        if not api.SetVolumeMountPointW(mount, volume_name.value):
            raise WinError(get_last_error())
        try:
            if path.stat().st_dev != device:
                raise ValueError("The blob mount does not resolve to the dedicated Windows test volume")
            filler = Path(scratch) / "qualification-fill"
            probe = Path(scratch) / "qualification-probe"
            block = bytes(64 * 1024)
            with filler.open("xb", buffering=0) as contents, probe.open("xb", buffering=0) as handle:
                remaining = shutil.disk_usage(volume).free
                if not 0 < remaining <= size:
                    raise ValueError("The dedicated Windows test volume has invalid free capacity")
                while remaining:
                    written = contents.write(block[:remaining])
                    if not written:
                        raise OSError("The test volume filler stopped making progress")
                    remaining -= written
                os.fsync(contents.fileno())
                try:
                    handle.write(block)
                except OSError as error:
                    if error.errno != errno.ENOSPC:
                        raise
                    code = error.errno
                else:
                    raise AssertionError("A separate write did not reproduce ENOSPC")
            probe.unlink()
            yield filler, size, code
        finally:
            if not api.DeleteVolumeMountPointW(mount):
                raise WinError(get_last_error())


def unavailable_evidence(database, fixture, expected, since_utc_ms):
    for event in openings(database, fixture["cameras"], since_utc_ms):
        if (not event["live"] or event["camera"] != expected["camera"] or event["category"] != expected["category"]
                or int(event["epoch"]) != int(expected["source_epoch"])
                or not expected["earliest_source_us"] <= int(event["source_us"]) <= expected["latest_source_us"]):
            continue
        with closing(sqlite3.connect(database)) as db:
            incident = UUID(event["id"]).bytes
            failure = db.execute(
                """SELECT count(*) FROM evidence_images i JOIN evidence_packets p ON p.id = i.packet_id
                   WHERE p.incident_id = ? AND i.failure = 'io' AND i.digest IS NULL""", (incident,)).fetchone()[0]
        if failure:
            return event | {"failed_image_writes": failure}
    return None


def persisted_blobs(database, directory, incident):
    with closing(sqlite3.connect(database)) as db:
        records = db.execute(
            """SELECT DISTINCT a.digest, a.byte_length FROM artifacts a
               JOIN evidence_images i ON i.digest = a.digest JOIN evidence_packets p ON p.id = i.packet_id
               WHERE p.incident_id = ? AND a.available = 1 AND i.retired = 0""", (UUID(incident).bytes,)).fetchall()
        integrity = [row for row, in db.execute("PRAGMA integrity_check")]
        references = len(db.execute("PRAGMA foreign_key_check").fetchall())
        duplicate_cycles = db.execute(
            """SELECT count(*) FROM (SELECT incident_id, attention_cycle FROM notification_outbox
               GROUP BY incident_id, attention_cycle HAVING count(*) > 1)""").fetchone()[0]
    verified = 0
    for digest, length in records:
        path = directory / f"{digest}.jpg"
        if path.is_file() and path.stat().st_size == length and sha256(path) == f"sha256:{digest}":
            verified += 1
    return {"integrity": integrity, "invalid_references": references, "duplicate_attention_cycles": duplicate_cycles,
            "expected_blobs": len(records), "verified_blobs": verified}


@contextmanager
def external_writer(database):
    with closing(sqlite3.connect(database)) as writer:
        writer.execute("BEGIN IMMEDIATE")
        yield
        writer.rollback()


def database_trial(fixture, receiver, database, planned, kind, limit_ms):
    local = fixture["local"]
    cameras = fixture["cameras"]
    clock_name, now = native_clock()

    def snapshot():
        return {name: latest(camera_id, local) for name, camera_id in cameras.items()}

    def ready():
        states = snapshot()
        return states if all(state["capture_state"] == "streaming"
                             and (state["analysis"] or {}).get("state") == "ready" for state in states.values()) else None

    before = until(ready, 60, clock=now)
    attempt = "recovered-" + kind
    pressure = pinned_wal(database) if kind == "pinned_wal" else external_writer(database)
    with pressure as wal:
        if wal is not None:
            camera(local, "committed-before-live-checkpoint")
        began = now()
        try:
            status, body = camera(local, attempt)
        except TimeoutError as error:
            raise RuntimeError("Timed out waiting for the camera write response during storage pressure") from error
        refusal_ms = (now() - began) * 1_000
        live_status, _ = local.call("GET", "/health/live")
        wal_bytes = wal.stat().st_size if wal is not None else None
        during = snapshot()
        deadline = now() + 5
        while not all(capture_progressed(before[name], state) for name, state in during.items()) and now() < deadline:
            time.sleep(0.05)
            during = snapshot()
    released = now()
    utc_ms = time.time_ns() // 1_000_000
    source = planned["camera"]
    proof = {"planned": planned, "during_pressure": {"before": before, "during": during, "status": status,
             "code": (body or {}).get("code"), "live_status": live_status, "wal_bytes": wal_bytes,
             "refusal_ms": refusal_ms}, "released_utc_ms": utc_ms, "elapsed_clock": clock_name, "idempotent_write": False}
    end = released + limit_ms / 1_000
    verified = None
    try:
        until(lambda: readiness(local, 200, "ready"), max(0, end - now()), clock=now)
        created, saved = camera(local, attempt)
        proof["idempotent_write"] = created == 201 and camera(local, attempt) == (201, saved)
        inject(fixture, [{"kind": "rtsp_reconnect", "camera": source}], receiver)

        def fresh_event():
            if camera_recovered(before[source], latest(cameras[source], local), epoch_changes=True):
                return canary(database, fixture, receiver, planned, utc_ms)
            return None

        verified = until(fresh_event, max(0, end - now()), clock=now)
    except (AssertionError, TimeoutError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        proof["recovery_error"] = str(error)
    proof["recovery_ms"] = (now() - released) * 1_000
    proof["source_after_release"] = latest(cameras[source], local)
    proof["incidents"] = verified["incidents"] if verified else []
    proof["checks"] = persisted_blobs(database, database.parent / "blobs", proof["incidents"][0]["id"]) if verified else {}
    if verified is not None:
        close_canary(local, verified["incidents"][0]["id"])
    return proof


def run_database(settings, output):
    profile = checked_json(Path(settings["profile"]))
    schedule = Path(settings["fault_schedule"])
    if sha256(schedule) != profile["workload"]["fault_schedule_digest"]:
        raise ValueError("Storage database schedule differs from the frozen workload")
    planned = checked_json(schedule)["storage_database"]
    if set(planned) != DATABASE_PRESSURE_STATUS.keys():
        raise ValueError("Storage database schedule must cover a writer lock and a pinned WAL reader")
    limit_ms = profile["budgets"]["storage_recovery_ms"]
    with TemporaryDirectory(prefix="stillwatch-live-sqlite-") as temporary:
        root = Path(temporary)
        database = root / "data/stillwatch.sqlite3"
        with native_fixture(settings, root, use_canary_media=True) as fixture:
            receiver = pair_owner(settings, fixture["local"], root)
            trials, errors = {}, {}
            for kind in DATABASE_PRESSURE_STATUS:
                try:
                    trials[kind] = database_trial(fixture, receiver, database, planned[kind], kind, limit_ms)
                except (AssertionError, RuntimeError, OSError, sqlite3.Error, subprocess.SubprocessError) as error:
                    errors[kind] = str(error)
            write_json(output, {"cases": trials, "collection_errors": errors})


def run(settings, output):
    clock_name, now = native_clock()
    profile = checked_json(Path(settings["profile"]))
    schedule = Path(settings["fault_schedule"])
    if sha256(schedule) != profile["workload"]["fault_schedule_digest"]:
        raise ValueError("Storage canary schedule differs from the frozen workload")
    planned = checked_json(schedule)["storage_enospc"]
    camera = planned["pressure_canary"]["camera"]
    if planned["recovered_canary"]["camera"] != camera:
        raise ValueError("The recovery case must exercise the same camera as the pressure case")
    with TemporaryDirectory(prefix="stillwatch-blob-volume-") as temporary:
        root = Path(temporary)
        blobs = root / "data/blobs"
        database = blobs.parent / "stillwatch.sqlite3"
        volume = Path(settings["windows_blob_volume"]) if "windows_blob_volume" in settings else None
        pressure = exhausted_windows_volume(blobs, volume) if volume is not None else exhausted_volume(blobs)
        with pressure as (filler, size, code):
            began = time.time_ns() // 1_000_000
            with native_fixture(settings, root, use_canary_media=True) as fixture:
                local = fixture["local"]
                source = fixture["cameras"][camera]
                receiver = pair_owner(settings, local, root)
                failed_event = until(lambda: unavailable_evidence(database, fixture, planned["pressure_canary"], began), 30, clock=now)
                before = latest(source, local)
                assert before["capture_state"] == "streaming" and before["analysis"]["state"] == "ready"
                close_canary(local, failed_event["id"])
                released_utc = time.time_ns() // 1_000_000
                released = now()
                filler.unlink()
                released_bytes = shutil.disk_usage(volume or blobs).free
                # Start the pinned positive scene again on a new source epoch.
                inject(fixture, [{"kind": "rtsp_reconnect", "camera": camera}], receiver)

                def fresh_event():
                    if camera_recovered(before, latest(source, local), epoch_changes=True):
                        return canary(database, fixture, receiver, planned["recovered_canary"], released_utc)
                    return None

                limit = profile["budgets"]["storage_recovery_ms"]
                try:
                    verified = until(fresh_event, max(0, released + limit / 1_000 - now()), clock=now)
                except AssertionError:
                    verified = None
                elapsed = (now() - released) * 1_000
                incidents = verified["incidents"] if verified else []
                checks = persisted_blobs(database, blobs, incidents[0]["id"]) if incidents else {}
                write_json(output, {"planned": planned, "volume_bytes": size, "probe_errno": code,
                                    "free_bytes_after_release": released_bytes, "pressure_event": failed_event,
                                    "released_utc_ms": released_utc, "elapsed_clock": clock_name,
                                    "recovery_ms": elapsed, "incidents": incidents,
                                    "source_after_release": latest(source, local),
                                    "openings_after_release": openings(database, fixture["cameras"], released_utc),
                                    "checks": checks})

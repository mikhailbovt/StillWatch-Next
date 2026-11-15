"""Collect a native steady interval with live RTSP, decoded WHEP and JPEG load."""

import argparse
import asyncio
import base64
from contextlib import ExitStack, closing, contextmanager
from datetime import datetime, timezone
import hashlib
import io
import ipaddress
import json
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import sys
from tempfile import TemporaryDirectory
import time
from uuid import uuid4

import av
import psutil

from fast_rc_contract import ROOT, checked_json, member, sha256, write_json

sys.path.insert(0, str(ROOT / "tests/e2e"))
from lan_core import Client as PinnedClient, encode_uuid, refresh_request, sign
from replay_core import run_core, until
from rtsp_core import check_port, launch
from whep_core import connect, free_port, request


def read_sources(path, profile):
    manifest = checked_json(path)
    sources = manifest["sources"]
    if len(sources) < profile["workload"]["camera_count"] or len(sources) > 64:
        raise ValueError("The fixture inventory must cover the declared camera count")
    seen = set()
    for source in sources:
        name = source["id"]
        if not re.fullmatch(r"[a-z][a-z0-9-]{0,47}", name) or name in seen:
            raise ValueError("Invalid or repeated camera fixture ID")
        seen.add(name)
        path_to_clip = member(path.parent, source["file"])
        if path_to_clip.suffix.lower() not in {".mp4", ".mkv"} or sha256(path_to_clip) != source["digest"]:
            raise ValueError(f"Unverified fixture media for {name}")
        with av.open(str(path_to_clip)) as container:
            stream, = container.streams.video
            native = (stream.codec_context.name, stream.width, stream.height, float(stream.average_rate))
            if native[:3] != (source["codec"], source["width"], source["height"]) or abs(native[3] - source["fps"]) > 0.02:
                raise ValueError(f"Declared media parameters differ for {name}")
        if source["phase"] not in {"aligned", "staggered"} or not 0 <= source["delay_s"] <= 20:
            raise ValueError(f"Invalid source phase for {name}")
        source["media"] = path_to_clip
    selected = sources[:profile["workload"]["camera_count"]]
    if len({source["digest"] for source in selected}) != len(selected):
        raise ValueError("Steady camera fixtures must use distinct media digests")
    return {source["id"]: source for source in selected}, manifest["viewers"]


def publisher(ffmpeg, peer, source):
    return [str(ffmpeg), "-hide_banner", "-loglevel", "warning", "-nostdin", "-re", "-stream_loop", "-1", "-i", str(source["media"]),
            "-map", "0:v:0", "-an", "-c:v", "copy", "-f", "rtsp", "-rtsp_transport", "tcp", f"rtsp://{peer}/{source['id']}"]


def pair_owner(settings, client, root):
    output = subprocess.check_output([settings["cli"], "pair", "--local", client.address.removeprefix("http://"),
                                      "--data-dir", str(root / "data"), "--role", "owner", "--json"], text=True, timeout=15)
    offer = json.loads(output)
    encoded = offer["tls_certificate_der"]
    certificate = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
    pinned = PinnedClient(offer["endpoint"], certificate)
    key = root / "qualification-device.key"
    label = "Fast RC receiver"
    public = sign(settings["node"], key)
    proof = b"stillwatch/pair/v2\0" + encode_uuid(offer["core_instance_id"]) + encode_uuid(offer["challenge"])
    proof += offer["secret"].encode() + label.encode()
    response, session = pinned.request("POST", "/api/v2/pairing/finish", {
        "challenge": offer["challenge"], "secret": offer["secret"], "label": label,
        "public_key": public, "signature": sign(settings["node"], key, proof),
    })
    if response != 201:
        raise RuntimeError(f"Steady load receiver pairing returned {response}")
    return pinned, session, offer, key


def latest(camera, local):
    began = time.monotonic()
    result = local.camera(camera)
    elapsed = (time.monotonic() - began) * 1_000
    state = result["runtime"]
    return {"epoch": result["source_epoch"], "capture_state": state["state"], "frame": state["frame"],
            "relative_source_lag_ms": state.get("relative_source_lag_ms"), "source_lagging": state.get("source_lagging"),
            "analysis": state.get("analysis"), "api_ms": elapsed}


def metrics(local, cameras):
    collected = {}
    for name, camera in cameras.items():
        code, value = local.call("GET", f"/api/v2/cameras/{camera}/analysis-metrics")
        if code != 200 or value is None:
            raise RuntimeError(f"Analysis metrics unavailable for {name}: {code}")
        collected[name] = value
    return collected


def gpu_used_bytes(device, declared):
    try:
        values = subprocess.check_output(["nvidia-smi", f"--id={device}", "--query-gpu=memory.used,memory.total,driver_version",
                                          "--format=csv,noheader,nounits"], text=True, stderr=subprocess.DEVNULL, timeout=2)
        used, capacity, driver = (value.strip() for value in values.strip().split(","))
        used, capacity = int(used), int(capacity)
        if driver == declared["driver"] and capacity * 1024 * 1024 == declared["vram_bytes"] and 0 <= used <= capacity:
            return used * 1024 * 1024
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    return None


def resources(pid, data, device=None, accelerator=None):
    core = psutil.Process(pid)
    tree = [core, *core.children(recursive=True)]
    measurements = {"rss_bytes": 0, "disk_bytes": 0, "children": len(tree) - 1,
                    "os_handles": 0, "open_sockets": 0, "pids": []}
    for child in tree:
        try:
            measurements["rss_bytes"] += child.memory_info().rss
            measurements["pids"].append([child.pid, child.create_time()])
            if measurements["os_handles"] is not None:
                measures = child.num_handles if sys.platform == "win32" else child.num_fds
                measurements["os_handles"] += measures()
            if measurements["open_sockets"] is not None:
                measurements["open_sockets"] += len(child.net_connections(kind="inet"))
        except psutil.NoSuchProcess:
            continue
        except psutil.AccessDenied:
            measurements["os_handles"] = None
            measurements["open_sockets"] = None
    for path in data.rglob("*"):
        if path.is_file():
            try:
                measurements["disk_bytes"] += path.stat().st_size
            except FileNotFoundError:
                continue
    if device is not None:
        # The exclusive-host limit includes the display and driver allocations on this device.
        measurements["vram_bytes"] = gpu_used_bytes(device, accelerator)
    return measurements


async def view(lan, session, camera, logical_id, profile, counts):
    peer, track, grant = await connect(lan, session, camera, None, None, profile=profile,
                                       dimensions=None, verify_mask=False)
    counts[logical_id] = counts.get(logical_id, 0) + 1
    last_pts = None
    try:
        while True:
            frame = await asyncio.wait_for(track.recv(), timeout=5)
            if frame.pts != last_pts:
                counts[logical_id] += 1
                last_pts = frame.pts
    finally:
        await peer.close()
        await asyncio.to_thread(request, lan, session["access_token"], "DELETE", grant["whep_path"])


def openings(database, cameras, start_ms):
    reverse = {camera: name for name, camera in cameras.items()}
    with closing(sqlite3.connect(database)) as db:
        rows = db.execute("SELECT occurred_at, incident FROM change_events WHERE opened=1 AND occurred_at >= ? ORDER BY sequence",
                          (start_ms,))
        result = []
        for utc, payload in rows:
            incident = json.loads(payload)
            scope = incident["scope"]
            if scope["camera_id"] in reverse:
                result.append({"id": incident["id"], "camera": reverse[scope["camera_id"]], "epoch": scope["epoch"],
                               "utc_ms": utc, "category": incident["category"], "attention": incident["attention"],
                               "source_us": incident["first_source_us"], "live": incident["live"]})
        return result


async def collect(settings, profile, sources, viewers, cameras, process, local, root, output, seconds):
    workload = profile["workload"]
    lan, session, offer, key = await asyncio.to_thread(pair_owner, settings, local, root)
    viewer_frames = {}
    tasks = []

    def start_viewers():
        return [asyncio.create_task(view(lan, session, cameras[viewer["camera"]], str(index), viewer["profile"], viewer_frames))
                for index, viewer in enumerate(viewers)]

    async def stop_viewers():
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)

    async def camera_sample():
        rows = await asyncio.gather(*(asyncio.to_thread(latest, camera, local) for camera in cameras.values()))
        return dict(zip(cameras, rows, strict=True))

    def check_viewers():
        for task in tasks:
            if task.done():
                task.result()
                raise RuntimeError("WHEP viewer closed during its observation window")

    tasks = start_viewers()
    try:
        warmup_end = time.monotonic() + 95
        while True:
            check_viewers()
            sample = await camera_sample()
            if (all(row["capture_state"] == "streaming" and row["analysis"] and row["analysis"]["state"] == "ready"
                    and row["analysis"].get("wall_cadence") and row["analysis"].get("cadence") for row in sample.values())
                    and len(viewer_frames) == len(viewers) and all(value >= 2 for value in viewer_frames.values())):
                break
            if time.monotonic() >= warmup_end:
                write_json(output / "readiness-timeout.json", {"cameras": sample, "viewer_frames": viewer_frames})
                raise RuntimeError("Steady readiness barrier expired")
            await asyncio.sleep(0.5)
        start_metrics = await asyncio.to_thread(metrics, local, cameras)
        started = time.monotonic()
        start_utc_ms = time.time_ns() // 1_000_000
        write_json(output / "start.json", {"cameras": cameras, "source_utc_ms": start_utc_ms, "metrics": start_metrics,
                                            "viewer_frames": viewer_frames.copy(), "source_scope": sample})
        snapshots = viewer_renewals = snapshots_ok = 0
        with (output / "samples.jsonl").open("x", encoding="utf-8", newline="\n", buffering=1) as file:
            for tick in range(seconds + 1):
                await asyncio.sleep(max(0, started + tick - time.monotonic()))
                check_viewers()
                token_expiry = datetime.fromisoformat(session["access_expires_at"])
                if (token_expiry - datetime.now(timezone.utc)).total_seconds() < 90:
                    proof = await asyncio.to_thread(refresh_request, settings["node"], key, offer, session)
                    code, rotated = await asyncio.to_thread(lan.request, "POST", "/api/v2/device-session/refresh", proof)
                    if code != 200:
                        raise RuntimeError(f"Steady receiver refresh returned {code}")
                    session.update(rotated)
                if tick and tick % 1500 == 0:
                    await stop_viewers()
                    tasks = start_viewers()
                    viewer_renewals += 1
                per_camera = await camera_sample()
                target = tick * workload["snapshot_requests_per_minute"] // 60
                # At most the scheduled rate for this clock interval, with no catch-up bursts.
                for _ in range(target - snapshots):
                    camera = list(cameras.values())[snapshots % len(cameras)]
                    code, headers, jpeg = await asyncio.to_thread(
                        request, lan, session["access_token"], "GET", f"/api/v2/cameras/{camera}/snapshot.jpg")
                    headers = {key.lower(): value for key, value in headers.items()}
                    if code == 200 and headers.get("cache-control") == "no-store" and 0 <= int(headers["x-frame-age-ms"]) <= 3000:
                        with av.open(io.BytesIO(jpeg), format="mjpeg") as image:
                            if next(image.decode(video=0), None) is not None:
                                snapshots_ok += 1
                    snapshots += 1
                usage = await asyncio.to_thread(resources, process.pid, root / "data", settings.get("cuda_device"),
                                                profile["host"]["accelerator"]) if tick % 5 == 0 else None
                file.write(json.dumps({"tick": tick, "elapsed_s": time.monotonic() - started,
                                       "cameras": per_camera, "viewer_frames": viewer_frames,
                                       "snapshots": snapshots, "snapshots_ok": snapshots_ok, "resources": usage}) + "\n")
        interval = time.monotonic() - started
        end_metrics = await asyncio.to_thread(metrics, local, cameras)
        observed = await asyncio.to_thread(openings, root / "data/stillwatch.sqlite3", cameras, start_utc_ms)
        source_spans = {}
        for camera in cameras:
            first, last = sample[camera], per_camera[camera]
            before = (first["analysis"] or {}).get("last_pose_source_us")
            after = (last["analysis"] or {}).get("last_pose_source_us")
            if first["epoch"] == last["epoch"] and before is not None and after is not None and int(after) >= int(before):
                source_spans[camera] = (int(after) - int(before)) / 1_000_000
        write_json(output / "end.json", {"elapsed_s": interval, "metrics": end_metrics, "openings": observed,
                                          "viewer_renewals": viewer_renewals, "snapshots": snapshots, "snapshots_ok": snapshots_ok,
                                          "source_camera_count": len(sources), "source_seconds_by_camera": source_spans})
    finally:
        await stop_viewers()


def run(settings, output, seconds):
    output.mkdir(parents=True, exist_ok=False)
    with TemporaryDirectory(prefix="stillwatch-steady-") as scratch:
        run_native(settings, output, Path(scratch), seconds)


def run_native(settings, output, root, seconds):
    with native_fixture(settings, root) as fixture:
        asyncio.run(collect(settings, fixture["profile"], fixture["sources"], fixture["viewers"],
                            fixture["cameras"], fixture["process"], fixture["local"], root, output, seconds))


@contextmanager
def native_fixture(settings, root, *, start_enabled=True):
    profile_path = Path(settings["profile"])
    source_path = Path(settings["source_manifest"])
    profile = checked_json(profile_path)
    workload = profile["workload"]
    if sha256(source_path) != workload["source_manifest_digest"]:
        raise ValueError("Source manifest differs from the frozen workload")
    if workload["semantic_route"] != "disabled":
        raise ValueError("The steady collector requires an independent native profile for the semantic route")
    sources, viewers = read_sources(source_path, profile)
    distinct = {kind: set() for kind in ("preview", "detail")}
    for viewer in viewers:
        if viewer["camera"] not in sources or viewer["profile"] not in distinct:
            raise ValueError("The viewer schedule references an unknown camera or mode")
        distinct[viewer["profile"]].add(viewer["camera"])
    if (len(viewers) != workload["viewer_sessions"] or len(distinct["preview"]) != workload["preview_publishers"]
            or len(distinct["detail"]) != workload["detail_publishers"]):
        raise ValueError("Viewer schedule differs from the declared workload")
    lan_host = str(ipaddress.IPv4Address(settings["address"]))
    if not ipaddress.IPv4Address(lan_host).is_private:
        raise ValueError("Use a private test interface address for the qualification receivers")
    router_port = free_port("127.0.0.1")
    peer = f"127.0.0.1:{router_port}"
    router = root / "rtsp.json"
    write_json(router, {"rtspAddress": peer, "rtspTransports": ["tcp"], "logLevel": "warn", "rtmp": False,
                        "hls": False, "webrtc": False, "srt": False, "moq": False, "paths": {name: {} for name in sources}})
    options = ["--model-pack", settings["models"], "--model-manifest-sha256", settings["model_digest"],
               "--ort-library", settings["runtime"], "--ort-library-sha256", settings["runtime_digest"],
               "--inference-workers", str(profile["models"]["workers"]), "--inference-threads", str(profile["models"]["threads_per_worker"]),
               "--active-cameras", str(len(sources)), "--raw-frame-bytes", str(settings["raw_frame_bytes"]), "--rtsp-peer", peer,
               "--lan-listen", f"{lan_host}:{free_port(lan_host)}", "--mediamtx", settings["mediamtx"],
               "--mediamtx-sha256", settings["mediamtx_digest"], "--media-encoder", settings["encoder"],
               "--media-encoder-sha256", settings["encoder_digest"], "--media-ice-udp", f"{lan_host}:{free_port(lan_host, socket.SOCK_DGRAM)}",
               *settings.get("cuda_args", [])]
    cameras = {}
    with ExitStack() as stack:
        stack.enter_context(launch([settings["mediamtx"], str(router)], root / "source-router.log"))
        until(lambda: check_port(router_port))
        begin = time.monotonic()
        publishers = {}
        publisher_contexts = {}

        def start_publisher(name):
            if name in publisher_contexts:
                publisher_contexts.pop(name).close()
            running = publisher_contexts[name] = stack.enter_context(ExitStack())
            index = sum(1 for _ in root.glob(f"source-{name}-*.log"))
            return running.enter_context(launch(publisher(settings["fixture_ffmpeg"], peer, sources[name]),
                                                root / f"source-{name}-{index}.log"))

        for name, source in sorted(sources.items(), key=lambda pair: pair[1]["delay_s"]):
            time.sleep(max(0, begin + source["delay_s"] - time.monotonic()))
            publishers[name] = start_publisher(name)

        def start_core():
            index = sum(1 for _ in root.glob("core-*.log"))
            return stack.enter_context(run_core(Path(settings["core"]), Path(settings["worker"]), root / "data", root,
                                                 root / f"core-{index}.log", *options))

        process, local = start_core()
        rates = {key: workload[key] for key in ("pose_fps", "fire_fps", "evidence_fps")}
        for name in sources:
            code, camera = local.call("POST", "/api/v2/cameras", {
                "name": name, "enabled": start_enabled, "sampling": rates,
                "source": {"type": "rtsp", "peer": peer, "stream_path": "/" + name, "secure": False,
                           "transport": "tcp", "credential_ref": None},
            }, key=uuid4().hex)
            if code != 201:
                raise RuntimeError(f"Camera fixture {name} was rejected: {code}")
            cameras[name] = camera["id"]
        yield {"profile": profile, "sources": sources, "viewers": viewers, "cameras": cameras, "process": process,
               "local": local, "publishers": publishers, "start_publisher": start_publisher, "start_core": start_core}


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--settings", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--seconds", type=int, default=3600, choices=range(1, 3601))
    args = parser.parse_args(argv)
    run(checked_json(args.settings), args.out, args.seconds)

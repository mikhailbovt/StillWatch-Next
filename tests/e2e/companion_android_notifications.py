"""Android delivery fixture for the signed Companion, with a real LAN Core and ADB."""

import argparse
from contextlib import ExitStack, closing
import json
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
from tempfile import TemporaryDirectory
import time
from uuid import uuid4
import xml.etree.ElementTree as ET

from alarm_agent import insert_urgent
from replay_core import run_core, until
from rtsp_core import free_port
from sdk_core import create_camera, issue

APP = "com.stillwatch.stillwatch_companion"
LAYOUT = "/data/local/tmp/stillwatch-qa.xml"


class Android:
    def __init__(self, adb, serial):
        self.command = [str(adb), "-s", serial]

    def adb(self, *command):
        result = subprocess.run(self.command + list(map(str, command)),
                                capture_output=True, text=True, timeout=25, check=True)
        return result.stdout

    def input(self, *args):
        self.adb("shell", "input", *args)

    def labels(self):
        self.adb("shell", "uiautomator", "dump", LAYOUT)
        return [node.attrib for node in ET.fromstring(self.adb("shell", "cat", LAYOUT)).iter("node")]

    def tap(self, label, *, scroll=False, exact=False):
        for _ in range(8 if scroll else 3):
            for node in self.labels():
                values = (node["text"], node["content-desc"])
                matches = label in values if exact else any(label in value for value in values)
                if matches:
                    self.tap_bounds(node)
                    return
            if scroll:
                width, height = map(int, re.findall(r"(\d+)x(\d+)", self.adb("shell", "wm", "size"))[-1])
                self.input("swipe", width // 2, height * 3 // 4, width // 2, height // 3, 380)
        raise AssertionError(f"Control was not found: {label}")

    def tap_bounds(self, node):
        left, top, right, bottom = map(int, re.findall(r"\d+", node["bounds"]))
        self.input("tap", (left + right) // 2, (top + bottom) // 2)

    def edit(self, index, expected):
        fields = [node for node in self.labels() if node["class"] == "android.widget.EditText"]
        assert len(fields) == expected
        self.tap_bounds(fields[index])

    def text(self, value):
        # The Android shell drops characters in long sequences of injected keys.
        for offset in range(0, len(value), 80):
            self.adb("shell", "input text " + shlex.quote(value[offset:offset + 80]))

    def open(self):
        self.adb("shell", "am", "start", "-W", "-n", APP + "/.MainActivity")

    def records(self, channel):
        items = self.adb("shell", "dumpsys", "notification", "--noredact").splitlines()
        return [re.search(r"\bkey=(\S+)", line).group(1) for line in items
                if "NotificationRecord(" in line and f"pkg={APP} " in line
                and f"channel={channel} " in line and "Notification(channel=" in line]

    def pair(self, offer):
        self.open()
        self.tap("Add a Core", exact=True)
        self.edit(1, 2)
        self.text(json.dumps(offer, separators=(",", ":")))
        self.input("keyevent", 4)
        self.tap("Review offer", exact=True)
        self.edit(0, 1)
        self.input("keycombination", 113, 29)
        self.text("Android-QA")
        self.input("keyevent", 4)
        self.tap("Trust and pair", exact=True)


def run(args):
    android = Android(args.adb, args.serial)
    assert android.adb("shell", "getprop", "sys.boot_completed").strip() == "1"
    # This is a delivery fixture. It requires an already installed APK on a clean test device.
    assert "package:" in android.adb("shell", "pm", "path", APP)
    with TemporaryDirectory(prefix="stillwatch-android-delivery-") as directory:
        root = Path(directory)
        data = root / "core"
        sources = root / "sources"
        sources.mkdir()
        port = free_port()
        endpoint = f"tcp:{port}"
        android.adb("reverse", endpoint, endpoint)
        try:
            def start(log):
                return run_core(args.core, args.worker, data, sources, root / log,
                                "--lan-listen", f"127.0.0.1:{port}")

            with ExitStack() as cores:
                process, client = cores.enter_context(start("core.log"))
                camera = create_camera(client, "Android delivery QA")
                offer = issue(args.cli, client, data, camera)
                android.pair(offer)
                android.tap("Settings")
                android.tap("Monitor in the background", scroll=True)
                android.tap("Allow", exact=True)
                until(lambda: android.records("stillwatch-monitoring"), 20)
                android.input("keyevent", 3)
                resumed = re.search(r"topResumedActivity=.*", android.adb("shell", "dumpsys", "activity", "activities"))
                assert resumed and APP not in resumed.group()
                assert not android.records("stillwatch-urgent")
                database = data / "stillwatch.sqlite3"

                def receipt(ident):
                    with closing(sqlite3.connect(database)) as db:
                        return db.execute(
                            "SELECT count(*) FROM notification_receipts AS r "
                            "JOIN notification_outbox AS n ON n.id=r.alert_id WHERE n.incident_id=?",
                            (ident.bytes,),
                        ).fetchone()[0]

                first = uuid4()
                insert_urgent(database, camera, first)
                until(lambda: receipt(first) == 1, 35)
                first_record, = android.records("stillwatch-urgent")
                code, incident = client.call("GET", f"/api/v2/incidents/{first}")
                assert code == 200 and incident["acknowledgement"] is None
                process.terminate()
                process.wait(timeout=15)
                second = uuid4()
                insert_urgent(database, camera, second)
                time.sleep(5)
                assert receipt(second) == 0 and first_record in android.records("stillwatch-urgent")
                cores.enter_context(start("restarted-core.log"))
                until(lambda: receipt(second) == 1, 45)
                assert len(android.records("stillwatch-urgent")) == 2
                android.open()
                android.tap("Settings")
                android.tap("Monitor in the background", scroll=True)
                until(lambda: not android.records("stillwatch-monitoring"), 15)
                android.input("keyevent", 3)
                third = uuid4()
                insert_urgent(database, camera, third)
                time.sleep(7)
                assert receipt(third) == 0
                print(json.dumps({"fixture": "sqlite_delivery_canary", "new_alert_after_home": True,
                                  "native_active_notification": True, "ack_independent": True,
                                  "retained_during_disconnect": True, "recovered_after_reconnect": True,
                                  "stopped_by_user": True}))
        finally:
            android.adb("shell", "am", "force-stop", APP)
            android.adb("reverse", "--remove", endpoint)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("adb", "core", "worker", "cli"):
        parser.add_argument(f"--{executable}", required=True, type=lambda p: Path(p).resolve(strict=True))
    parser.add_argument("--serial", required=True)
    run(parser.parse_args())

"""Identify native processes independently of PID reuse and Linux wall-clock shifts."""

from pathlib import Path
import sys

import psutil


PROCESS_START_CLOCK = "proc_stat_start_ticks" if sys.platform == "linux" else "windows_creation_utc"


def process_identity(process):
    pid = process.pid
    if sys.platform != "linux":
        return pid, process.create_time()
    path = Path(psutil.PROCFS_PATH) / str(pid) / "stat"
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        raise psutil.NoSuchProcess(pid) from None
    except PermissionError:
        raise psutil.AccessDenied(pid) from None
    # Field 22 follows the parenthesized command name, which may contain ') '.
    fields = raw.rsplit(b") ", 1)[1].split()
    return pid, int(fields[19])

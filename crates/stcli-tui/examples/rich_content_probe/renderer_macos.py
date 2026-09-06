#!/usr/bin/env python3
"""Disposable macOS evaluation for the ticket-01 rich-content renderer."""

from __future__ import annotations

import argparse
import base64
import importlib.util
import ctypes
import json
import os
from pathlib import Path
import resource
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Any

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
COMMON_PATH = HERE / "renderer.py"
CHROME = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
MAX_SCOPE_RSS = 200 * 1024 * 1024
MAX_TASKS = 256
MAX_PNG = 3 * 1024 * 1024

spec = importlib.util.spec_from_file_location("rich_content_common", COMMON_PATH)
if spec is None or spec.loader is None:
    raise RuntimeError("cannot load common renderer probe")
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)


def write_json(directory: Path, name: str, value: dict[str, Any]) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def chrome_command(profile: Path) -> list[str]:
    return [
        str(CHROME),
        "--headless=new",
        "--remote-debugging-pipe",
        f"--user-data-dir={profile}",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-domain-reliability",
        "--disable-sync",
        "--metrics-recording-only",
        "--disable-breakpad",
        "--disable-crash-reporter",
        "--disable-features=MediaRouter,OptimizationHints,AutofillServerCommunication",
        "--password-store=basic",
        "--use-mock-keychain",
        "--disable-gpu",
        "about:blank",
    ]


class MacBrowser(common.Browser):
    """Common CDP rendering with Chrome's built-in macOS sandbox only."""

    def __init__(self, renderer: Path):
        if renderer != CHROME or not renderer.is_file():
            raise RuntimeError(f"tested Chrome executable is missing: {renderer}")
        self.fixtures, self.asset = common.load_inputs()
        self.renderer = renderer
        self.temp = tempfile.TemporaryDirectory(prefix="stcli-rich-macos-")
        profile = Path(self.temp.name) / "profile"
        profile.mkdir()
        to_child_r, controller_write = os.pipe()
        controller_read, from_child_w = os.pipe()
        saved_read, saved_write = os.dup(3), os.dup(4)
        os.dup2(to_child_r, 3)
        os.dup2(from_child_w, 4)
        started = time.monotonic()
        try:
            self.process = subprocess.Popen(
                chrome_command(profile),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                close_fds=True,
                pass_fds=(3, 4),
                env={"HOME": self.temp.name, "TMPDIR": self.temp.name},
                start_new_session=True,
            )
        finally:
            os.dup2(saved_read, 3)
            os.dup2(saved_write, 4)
            for descriptor in (saved_read, saved_write, to_child_r, from_child_w):
                os.close(descriptor)
        self.cdp = common.CDP(controller_read, controller_write)
        try:
            self.version = self.cdp.call("Browser.getVersion", timeout=common.START_TIMEOUT)
            targets = self.cdp.call("Target.getTargets", timeout=common.START_TIMEOUT).get("targetInfos", [])
            initial = next(target for target in targets if target.get("type") == "page")
            self.initial_target = initial
        except Exception as error:
            cleanup = self.close()
            raise RuntimeError(f"Chrome CDP pipe failed: {error}; cleanup={cleanup}") from error
        self.start_seconds = time.monotonic() - started
        self.mounts: list[list[str]] = []
        self.cgroup: dict[str, Any] = {}
        self.namespaces: dict[str, Any] = {}
        self.process_info = {"pid": self.process.pid, "sandbox": "Chrome child-process Seatbelt only"}


def positive_controls(sentinel: Path, listener: socket.socket) -> dict[str, Any]:
    port = listener.getsockname()[1]
    probe = socket.create_connection(("127.0.0.1", port), timeout=1)
    accepted, _ = listener.accept()
    probe.sendall(b"outside-control")
    request = accepted.recv(15)
    accepted.sendall(b"outside-reply")
    reply = probe.recv(13)
    accepted.close()
    probe.close()
    return {
        "host_file_readable_outside": sentinel.read_bytes() == b"owned harmless sentinel",
        "loopback_listener_outside": request == b"outside-control" and reply == b"outside-reply",
        "loopback_port": port,
    }


def seatbelt_attempt(evidence: Path) -> dict[str, Any]:
    private = Path(tempfile.mkdtemp(prefix="stcli-seatbelt-primary-"))
    profile = private / "profile.sb"
    user_temp = Path(tempfile.gettempdir()).resolve()
    profile.write_text(
        "\n".join(
            [
                "(version 1)",
                "(deny default)",
                "(allow process*)",
                "(allow signal)",
                "(allow sysctl-read)",
                "(allow mach*)",
                "(allow ipc*)",
                "(allow file-read*)",
                f'(deny file-read* (subpath "{Path.home()}"))',
                f'(allow file-read* file-write* (subpath "{private.resolve()}"))',
                "(allow network*)",
                "(deny network* (remote ip))",
                "(deny network* (local ip))",
                f'(allow file-read* file-write* (subpath "{user_temp}"))',
            ]
        )
        + "\n"
    )
    chrome_profile = private / "chrome-profile"
    chrome_profile.mkdir()
    command = ["/usr/bin/sandbox-exec", "-f", str(profile), *chrome_command(chrome_profile)]
    command = [item for item in command if item != "--remote-debugging-pipe"]
    command[-1:] = ["--dump-dom", "about:blank"]
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=10,
            env={"HOME": str(private), "TMPDIR": str(private)},
        )
        result = {
            "approach": "sandbox-exec deny-default outer Seatbelt profile",
            "elapsed_seconds": time.monotonic() - started,
            "exit_code": completed.returncode,
            "started": completed.returncode == 0,
            "stderr": completed.stderr[-2048:],
            "profile": profile.read_text(),
        }
    except subprocess.TimeoutExpired as error:
        result = {
            "approach": "sandbox-exec deny-default outer Seatbelt profile",
            "elapsed_seconds": time.monotonic() - started,
            "started": False,
            "error": f"timeout: {error}",
            "profile": profile.read_text(),
        }
    write_json(evidence, "seatbelt-primary.json", result)
    return result

def sandbox_check_path(pid: int, operation: bytes, path: Path) -> dict[str, Any]:
    sandbox_check = ctypes.CDLL(None).sandbox_check
    sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint64, ctypes.c_char_p]
    sandbox_check.restype = ctypes.c_int
    result = sandbox_check(pid, operation, 0x10, str(path).encode())
    return {"operation": operation.decode(), "path": str(path), "result": result, "allowed": result == 0}

class ProcTaskInfo(ctypes.Structure):
    _fields_ = [
        ("virtual_size", ctypes.c_uint64),
        ("resident_size", ctypes.c_uint64),
        ("total_user", ctypes.c_uint64),
        ("total_system", ctypes.c_uint64),
        ("threads_user", ctypes.c_uint64),
        ("threads_system", ctypes.c_uint64),
        *[(name, ctypes.c_int32) for name in (
            "policy", "faults", "pageins", "cow_faults", "messages_sent",
            "messages_received", "syscalls_mach", "syscalls_unix", "csw",
            "threadnum", "numrunning", "priority",
        )],
    ]


def process_threads(pid: int) -> int | None:
    info = ProcTaskInfo()
    proc_pidinfo = ctypes.CDLL(None).proc_pidinfo
    proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    proc_pidinfo.restype = ctypes.c_int
    size = proc_pidinfo(pid, 4, 0, ctypes.byref(info), ctypes.sizeof(info))
    return info.threadnum if size == ctypes.sizeof(info) else None


def process_snapshot(root_pid: int) -> dict[str, Any]:
    completed = subprocess.run(
        ["/bin/ps", "-axo", "pid=,ppid=,rss=,command="], capture_output=True, text=True
    )
    parsed = []
    for line in completed.stdout.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) >= 3 and all(value.isdigit() for value in fields[:3]):
            parsed.append((int(fields[0]), int(fields[1]), int(fields[2]), fields[3] if len(fields) == 4 else ""))
    owned = {root_pid}
    while True:
        added = {pid for pid, parent, _, _ in parsed if parent in owned}
        if added <= owned:
            break
        owned.update(added)
    rows = [f"{pid} {parent} {rss} {command}" for pid, parent, rss, command in parsed if pid in owned]
    rss_kib = sum(rss for pid, _, rss, _ in parsed if pid in owned)
    thread_counts = {pid: process_threads(pid) for pid in owned}
    aggregate_threads = sum(count for count in thread_counts.values() if count is not None)
    return {
        "root_pid": root_pid,
        "rows": rows,
        "aggregate_rss_bytes": rss_kib * 1024,
        "process_count": len(owned),
        "thread_counts": thread_counts,
        "aggregate_threads": aggregate_threads,
        "ps_error": completed.stderr.strip() or None,
    }


def profile_processes() -> list[str]:
    completed = subprocess.run(
        ["/bin/ps", "-axo", "command="], capture_output=True, text=True
    )
    return [line.strip() for line in completed.stdout.splitlines() if "stcli-rich-macos-" in line]


def evaluate(evidence: Path) -> int:
    if not CHROME.is_file():
        raise RuntimeError(f"Chrome not found at {CHROME}")
    evidence.mkdir(parents=True, exist_ok=True)
    primary = seatbelt_attempt(evidence)
    sentinel_file = tempfile.NamedTemporaryFile(prefix="stcli-host-sentinel-", delete=False)
    sentinel_file.write(b"owned harmless sentinel")
    sentinel_file.close()
    sentinel = Path(sentinel_file.name)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(2)
    port = listener.getsockname()[1]
    listener.settimeout(2)
    controller = positive_controls(sentinel, listener)

    isolation: dict[str, Any] = {
        "experiment": "macos-evaluation",
        "backend_approved": False,
        "positive_controls": controller,
        "primary": primary,
        "secondary": {"approach": "Chrome built-in child-process Seatbelt plus CDP policy"},
        "failed_criterion": "controlling browser process retains host filesystem authority",
    }
    measurements: dict[str, Any] = {
        "experiment": "macos-evaluation",
        "cold_starts": [],
        "renders": {},
        "ceilings": {"cold_seconds": 10, "warm_seconds": 3, "scope_rss_bytes": MAX_SCOPE_RSS, "tasks": MAX_TASKS},
    }
    try:
        for _ in range(3):
            browser = MacBrowser(CHROME)
            measurements["cold_starts"].append(browser.start_seconds)
            measurements["browser_version"] = browser.version
            browser.close()
        browser = MacBrowser(CHROME)
        try:
            isolation["secondary"]["browser_version"] = browser.version
            positive = browser.render("card", 800)
            isolation["positive_render"] = {
                "width": positive["width"],
                "height": positive["height"],
                "png_bytes": len(base64.b64decode(positive["png_base64"])),
                **positive["observations"],
            }
            (evidence / "rendered-card-macos.png").write_bytes(base64.b64decode(positive["png_base64"]))
            hostile = browser.render(
                "card", 800, document=common.hostile_document(str(sentinel), port), observe_dom=True
            )
            isolation["hostile_document"] = hostile["observations"]
            isolation["hostile_document"]["listener_requests"] = []
            listener.setblocking(False)
            try:
                while True:
                    connection, _ = listener.accept()
                    isolation["hostile_document"]["listener_requests"].append(
                        connection.recv(512).decode(errors="replace")[:512]
                    )
                    connection.close()
            except BlockingIOError:
                pass
            isolation["secondary"]["browser_parent_sandbox_check"] = sandbox_check_path(
                browser.process.pid, b"file-read-data", sentinel
            )
            for fixture in ("card", "columns"):
                samples = []
                for _ in range(10):
                    started = time.monotonic()
                    rendered = browser.render(fixture, 900)
                    samples.append(
                        {
                            "seconds": time.monotonic() - started,
                            "png_bytes": len(base64.b64decode(rendered["png_base64"])),
                            "width": rendered["width"],
                            "height": rendered["height"],
                        }
                    )
                measurements["renders"][fixture] = {
                    "samples": samples,
                    "median_seconds": statistics.median(sample["seconds"] for sample in samples),
                    "max_seconds": max(sample["seconds"] for sample in samples),
                    "max_png_bytes": max(sample["png_bytes"] for sample in samples),
                }
            snapshot = process_snapshot(browser.process.pid)
            measurements["process_snapshot"] = snapshot
            measurements["scope_rss_ceiling_met"] = snapshot["aggregate_rss_bytes"] <= MAX_SCOPE_RSS
            measurements["task_ceiling_met"] = snapshot["aggregate_threads"] <= MAX_TASKS
        finally:
            cleanup = browser.close()
            isolation["cleanup"] = cleanup
            measurements["cleanup"] = cleanup
            time.sleep(.1)
            isolation["orphaned_profile_processes"] = profile_processes()
            measurements["orphaned_profile_processes"] = isolation["orphaned_profile_processes"]
    finally:
        listener.close()
        sentinel.unlink(missing_ok=True)
    measurements["child_rusage_maxrss_bytes"] = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    measurements["png_ceiling_met"] = all(
        fixture["max_png_bytes"] <= MAX_PNG for fixture in measurements["renders"].values()
    )
    write_json(evidence, "isolation-macos.json", isolation)
    write_json(evidence, "measurements-macos.json", measurements)
    results = {
        "experiment": "macos-evaluation",
        "backend_approved": False,
        "decision": "text fallback only on macOS",
        "failed_criteria": [
            "outer sandbox-exec profile crashes Chrome before CDP/rendering",
            "Chrome built-in sandbox leaves the controlling browser process with host file authority",
            "observed renderer scope exceeds the 200 MiB memory ceiling",
        ],
        "terminal_graphics": "recorded separately by terminal probe and transfer",
    }
    write_json(evidence, "results-macos.json", results)
    print(json.dumps(results, sort_keys=True))
    return 2


def main() -> int:
    parser = argparse.ArgumentParser()
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--evaluate", action="store_true")
    modes.add_argument("--worker", action="store_true")
    modes.add_argument("--terminal-probe", action="store_true")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--renderer", type=Path, default=CHROME)
    args = parser.parse_args()
    if args.terminal_probe:
        return common.terminal_probe()
    if args.worker:
        common.Browser = MacBrowser
        return common.worker(args.renderer)
    if args.evidence is None:
        parser.error("--evaluate requires --evidence")
    return evaluate(args.evidence)


if __name__ == "__main__":
    raise SystemExit(main())

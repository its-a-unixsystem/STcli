#!/usr/bin/env python3
"""Controller-side evaluator for the disposable sandboxed WKWebView helper."""

from __future__ import annotations

import argparse
import base64
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import shutil
import signal
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
COMMON_PATH = HERE / "renderer.py"
HELPER = HERE / "webkit_helper/build/RichContentWebKitProbe.app/Contents/MacOS/RichContentWebKitProbe"
BUNDLE = HERE / "webkit_helper/build/RichContentWebKitProbe.app"
CONTAINER = Path.home() / "Library/Containers/dev.stcli.rich-content-webkit-probe"
MAX_MEMORY = 200 * 1024 * 1024
MAX_THREADS = 256
MAX_PNG_EVIDENCE = 3 * 1024 * 1024


spec = importlib.util.spec_from_file_location("rich_content_common", COMMON_PATH)
if spec is None or spec.loader is None:
    raise RuntimeError("cannot load common renderer probe")
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)


def write_json(directory: Path, name: str, value: dict[str, Any]) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / name
    temporary = target.with_suffix(target.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, target)


def command_output(command: list[str]) -> dict[str, Any]:
    completed = subprocess.run(command, capture_output=True, text=True)
    return {
        "command": command,
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def parse_processes() -> dict[int, dict[str, Any]]:
    completed = subprocess.run(
        ["/bin/ps", "-axo", "pid=,ppid=,rss=,command="], capture_output=True, text=True, check=True
    )
    processes = {}
    for line in completed.stdout.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) >= 3 and fields[0].isdigit() and fields[1].isdigit() and fields[2].isdigit():
            processes[int(fields[0])] = {
                "pid": int(fields[0]),
                "ppid": int(fields[1]),
                "rss_bytes": int(fields[2]) * 1024,
                "command": fields[3] if len(fields) == 4 else "",
            }
    return processes


def responsibility_pid(pid: int) -> int | None:
    try:
        function = ctypes.CDLL(None).responsibility_get_pid_responsible_for_pid
    except AttributeError:
        return None
    function.argtypes = [ctypes.c_int]
    function.restype = ctypes.c_int
    value = function(pid)
    return value if value > 0 else None


def attributed_processes(root_pid: int, baseline: set[int]) -> tuple[str, list[dict[str, Any]]]:
    processes = parse_processes()
    owned = {root_pid}
    while True:
        children = {pid for pid, row in processes.items() if row["ppid"] in owned}
        if children <= owned:
            break
        owned.update(children)
    responsible = {
        pid for pid, row in processes.items()
        if "com.apple.WebKit" in row["command"] and responsibility_pid(pid) == root_pid
    }
    if responsible:
        owned.update(responsible)
        method = "parent tree plus responsibility_get_pid_responsible_for_pid"
    else:
        correlated = {
            pid for pid, row in processes.items()
            if pid not in baseline and "com.apple.WebKit" in row["command"]
        }
        owned.update(correlated)
        method = "parent tree plus controlled-run timing correlation against pre-launch baseline"
    rows = [processes[pid] for pid in sorted(owned) if pid in processes]
    return method, rows


def sandbox_check_path(pid: int, operation: bytes, path: Path) -> dict[str, Any]:
    function = ctypes.CDLL(None).sandbox_check
    function.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint64, ctypes.c_char_p]
    function.restype = ctypes.c_int
    result = function(pid, operation, 0x10, str(path).encode())
    return {"pid": pid, "operation": operation.decode(), "path": str(path), "result": result, "allowed": result == 0}


class ProcTaskInfo(ctypes.Structure):
    _fields_ = [
        ("virtual_size", ctypes.c_uint64), ("resident_size", ctypes.c_uint64),
        ("total_user", ctypes.c_uint64), ("total_system", ctypes.c_uint64),
        ("threads_user", ctypes.c_uint64), ("threads_system", ctypes.c_uint64),
        *[(name, ctypes.c_int32) for name in (
            "policy", "faults", "pageins", "cow_faults", "messages_sent",
            "messages_received", "syscalls_mach", "syscalls_unix", "csw",
            "threadnum", "numrunning", "priority",
        )],
    ]


class RUsageInfoV4(ctypes.Structure):
    _fields_ = [
        ("ri_uuid", ctypes.c_uint8 * 16),
        *[(name, ctypes.c_uint64) for name in (
            "ri_user_time", "ri_system_time", "ri_pkg_idle_wkups", "ri_interrupt_wkups",
            "ri_pageins", "ri_wired_size", "ri_resident_size", "ri_phys_footprint",
            "ri_proc_start_abstime", "ri_proc_exit_abstime", "ri_child_user_time",
            "ri_child_system_time", "ri_child_pkg_idle_wkups", "ri_child_interrupt_wkups",
            "ri_child_pageins", "ri_child_elapsed_abstime", "ri_diskio_bytesread",
            "ri_diskio_byteswritten", "ri_cpu_time_qos_default", "ri_cpu_time_qos_maintenance",
            "ri_cpu_time_qos_background", "ri_cpu_time_qos_utility", "ri_cpu_time_qos_legacy",
            "ri_cpu_time_qos_user_initiated", "ri_cpu_time_qos_user_interactive",
            "ri_billed_system_time", "ri_serviced_system_time", "ri_logical_writes",
            "ri_lifetime_max_phys_footprint", "ri_instructions", "ri_cycles",
            "ri_billed_energy", "ri_serviced_energy", "ri_interval_max_phys_footprint",
            "ri_runnable_time", "ri_flags",
        )],
    ]


def process_metrics(pid: int) -> dict[str, Any]:
    library = ctypes.CDLL(None)
    task = ProcTaskInfo()
    library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    library.proc_pidinfo.restype = ctypes.c_int
    task_size = library.proc_pidinfo(pid, 4, 0, ctypes.byref(task), ctypes.sizeof(task))
    usage = RUsageInfoV4()
    library.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    library.proc_pid_rusage.restype = ctypes.c_int
    usage_result = library.proc_pid_rusage(pid, 4, ctypes.byref(usage))
    return {
        "threads": task.threadnum if task_size == ctypes.sizeof(task) else None,
        "phys_footprint_bytes": usage.ri_phys_footprint if usage_result == 0 else None,
        "lifetime_max_phys_footprint_bytes": usage.ri_lifetime_max_phys_footprint if usage_result == 0 else None,
        "user_time_ns": usage.ri_user_time if usage_result == 0 else None,
        "system_time_ns": usage.ri_system_time if usage_result == 0 else None,
    }


def process_snapshot(root_pid: int, baseline: set[int]) -> dict[str, Any]:
    method, rows = attributed_processes(root_pid, baseline)
    for row in rows:
        row.update(process_metrics(row["pid"]))
        row["responsible_pid"] = responsibility_pid(row["pid"])
    return {
        "attribution_method": method,
        "attribution_limit": "Timing correlation can over-attribute unrelated WebKit services created during the controlled two-second window; shared service reuse can under-attribute.",
        "rows": rows,
        "process_count": len(rows),
        "aggregate_rss_bytes": sum(row["rss_bytes"] for row in rows),
        "aggregate_phys_footprint_bytes": sum(row["phys_footprint_bytes"] or 0 for row in rows),
        "aggregate_threads": sum(row["threads"] or 0 for row in rows),
        "shared_memory_note": "Per-process RSS and physical footprint sums can double-count shared WebKit mappings.",
    }


def directory_snapshot(root: Path) -> list[str]:
    if not root.exists():
        return []
    return sorted(str(path.relative_to(root)) for path in root.rglob("*") if path.is_file())


class Listener:
    def __init__(self) -> None:
        self.socket = socket.socket()
        self.socket.bind(("127.0.0.1", 0))
        self.socket.listen(16)
        self.socket.settimeout(0.1)
        self.port = self.socket.getsockname()[1]
        self.requests: list[str] = []
        self.stopped = False
        self.thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self.thread.start()

    def _run(self) -> None:
        while not self.stopped:
            try:
                connection, _ = self.socket.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            try:
                connection.settimeout(0.2)
                self.requests.append(connection.recv(1024).decode(errors="replace")[:1024])
            except OSError as error:
                self.requests.append(f"read-error: {error}")
            finally:
                connection.close()

    def close(self) -> None:
        self.stopped = True
        self.socket.close()
        self.thread.join(timeout=1)


class Worker:
    def __init__(self, fixtures: Path):
        if not HELPER.is_file():
            raise RuntimeError(f"helper missing: {HELPER}")
        self.fixtures = fixtures
        self.baseline = set(parse_processes())
        self.started = time.monotonic()
        self.process = subprocess.Popen(
            [str(HELPER), "--worker", "--fixtures-dir", str(fixtures)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1,
        )

    def render(self, ident: int, fixture: str, width: int, *, document: bytes | None = None,
               observe: bool = False, assets: list[bytes] | None = None, timeout: float = 10) -> dict[str, Any]:
        assert self.process.stdin is not None and self.process.stdout is not None
        request: dict[str, Any] = {"id": ident, "fixture": fixture, "width": width}
        if document is not None:
            request["document_base64"] = base64.b64encode(document).decode()
        if observe:
            request["observe"] = True
        if assets is not None:
            request["assets_base64"] = [base64.b64encode(asset).decode() for asset in assets]
        elif fixture in ("card", "columns"):
            request["document_base64"] = base64.b64encode((self.fixtures / f"{fixture}.html").read_bytes()).decode()
        self.process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        line: list[str] = []
        error: list[BaseException] = []

        def read_reply() -> None:
            try:
                line.append(self.process.stdout.readline())
            except BaseException as caught:
                error.append(caught)

        thread = threading.Thread(target=read_reply, daemon=True)
        thread.start()
        thread.join(timeout)
        if thread.is_alive():
            raise TimeoutError("helper response deadline exceeded")
        if error:
            raise RuntimeError(common.compact_error(error[0]))
        if not line or not line[0]:
            stderr = self.process.stderr.read() if self.process.stderr else ""
            raise RuntimeError(f"helper closed response pipe: {stderr[-1024:]}")
        return json.loads(line[0])

    def close(self) -> dict[str, Any]:
        started = time.monotonic()
        if self.process.stdin:
            self.process.stdin.close()
        try:
            code = self.process.wait(timeout=0.75)
            killed = False
        except subprocess.TimeoutExpired:
            self.process.kill()
            code = self.process.wait(timeout=2)
            killed = True
        stderr = self.process.stderr.read() if self.process.stderr else ""
        return {"exit_code": code, "forced_kill": killed, "seconds": time.monotonic() - started, "stderr": stderr[-2048:]}


def positive_controls(sentinel: Path, listener: Listener) -> dict[str, Any]:
    probe = socket.create_connection(("127.0.0.1", listener.port), timeout=1)
    probe.sendall(b"outside-control")
    probe.close()
    time.sleep(0.15)
    return {
        "host_file_readable_outside": sentinel.read_bytes() == b"owned harmless sentinel",
        "loopback_listener_outside": any("outside-control" in request for request in listener.requests),
        "loopback_port": listener.port,
    }


def hostile_document(sentinel: Path, port: int) -> bytes:
    original = common.hostile_document(str(sentinel), port).decode()
    return original.replace("file:///proc/self/status", "file:///etc/passwd").encode()


def asset_control_document() -> bytes:
    return b'<!doctype html><html><body><img src="stcli-probe://render/assets/emblem.png"></body></html>'


def cookie_document(value: str) -> bytes:
    return f'<!doctype html><html><body><script>document.cookie="state={value}"</script><p>{value}</p></body></html>'.encode()


def build_and_sign() -> float:
    started = time.monotonic()
    subprocess.run([str(HERE / "webkit_helper/build.sh")], check=True)
    return time.monotonic() - started


def entitlement_evidence() -> dict[str, Any]:
    return {
        "bundle": str(BUNDLE),
        "binary": str(HELPER),
        "codesign_details": command_output(["/usr/bin/codesign", "-dvvv", str(BUNDLE)]),
        "effective_entitlements": command_output(["/usr/bin/codesign", "-d", "--entitlements", "-", str(BUNDLE)]),
        "gatekeeper": command_output(["/usr/sbin/spctl", "-a", "-vv", str(BUNDLE)]),
        "launch_ipc": "Controller directly execs the signed bundle Mach-O and uses piped stdin/stdout JSON lines.",
        "entitlement_policy": ["com.apple.security.app-sandbox", "com.apple.security.network.client"],
        "distribution_note": "Ad-hoc signing proves local sandbox behavior, not end-user Gatekeeper installation. Developer ID signing and notarization remain untested prerequisites.",
    }


def run_isolation(evidence: Path, fixtures: Path) -> dict[str, Any]:
    sentinel_handle = tempfile.NamedTemporaryFile(prefix="stcli-host-sentinel-", delete=False)
    sentinel_handle.write(b"owned harmless sentinel")
    sentinel_handle.close()
    sentinel = Path(sentinel_handle.name)
    listener = Listener()
    listener.start()
    controller = positive_controls(sentinel, listener)
    controller_hits = len(listener.requests)
    before = directory_snapshot(CONTAINER)
    worker = Worker(fixtures)
    isolation: dict[str, Any] = {
        "experiment": "macos-webkit",
        "positive_controls": controller,
        "storage": {"before": before},
        "network_authority": {
            "os_level": "com.apple.security.network.client is mandatory for WKWebView and permits outbound connections from the helper process.",
            "document_level": "Content rules plus navigation delegate deny external document requests.",
        },
    }
    try:
        card = worker.render(1, "card", 800, observe=True)
        isolation["card"] = {
            "width": card.get("width"), "height": card.get("height"),
            "png_bytes": len(base64.b64decode(card["png_base64"])),
            "observations": card.get("observations", {}),
        }
        paths = [sentinel, Path.home() / ".zshrc", Path.home() / ".ssh/config"]
        isolation["helper_sandbox_checks"] = [
            sandbox_check_path(worker.process.pid, operation, path)
            for path in paths for operation in (b"file-read-data", b"file-write-data")
        ]
        hostile = worker.render(2, "hostile", 800, document=hostile_document(sentinel, listener.port), observe=True)
        time.sleep(0.2)
        hostile_observations = hostile.get("observations", {})
        hostile_observations["listener_requests"] = listener.requests[controller_hits:]
        isolation["hostile_document"] = hostile_observations
        asset = (fixtures / "asset.png").read_bytes()
        asset_reply = worker.render(3, "document", 800, document=asset_control_document(), observe=True, assets=[asset])
        isolation["asset_positive_control"] = asset_reply.get("observations", {})
        isolation["storage"]["after_job_a"] = directory_snapshot(CONTAINER)
        cookie_a = worker.render(4, "document", 800, document=cookie_document("a"), observe=True)
        cookie_b = worker.render(5, "document", 800, document=cookie_document("b"), observe=True)
        isolation["storage"]["after_job_b"] = directory_snapshot(CONTAINER)
        isolation["storage"]["job_a_cookie"] = cookie_a.get("observations", {}).get("cookie")
        isolation["storage"]["job_b_cookie"] = cookie_b.get("observations", {}).get("cookie")
        persistent_markers = [path for path in isolation["storage"]["after_job_b"] if "Cookie" in path or "WebKitCache" in path]
        isolation["storage"]["persistent_cookie_cache_files"] = persistent_markers
        snapshot = process_snapshot(worker.process.pid, worker.baseline)
        checks = []
        for row in snapshot["rows"]:
            for operation in (b"file-read-data", b"file-write-data"):
                checks.append(sandbox_check_path(row["pid"], operation, sentinel))
        isolation["process_snapshot"] = snapshot
        isolation["child_sandbox_checks"] = checks
    finally:
        isolation["cleanup"] = worker.close()
        listener.close()
        sentinel.unlink(missing_ok=True)
        shutil.rmtree(CONTAINER, ignore_errors=True)
        time.sleep(0.2)
        isolation["cleanup"]["container_removed"] = not CONTAINER.exists()
        isolation["cleanup"]["remaining_attributed_processes"] = attributed_processes(worker.process.pid, worker.baseline)[1]
    file_denials = all(not item["allowed"] for item in isolation["helper_sandbox_checks"] + isolation["child_sandbox_checks"])
    hostile_ok = (
        isolation["hostile_document"].get("marker_state") == "original"
        and isolation["hostile_document"].get("nested_marker_count") == 0
        and not isolation["hostile_document"].get("listener_requests")
    )
    asset_ok = isolation["asset_positive_control"].get("asset_natural_width", 0) > 0
    storage_ok = not isolation["storage"]["persistent_cookie_cache_files"] and isolation["storage"].get("job_b_cookie", "") == ""
    isolation["assertions"] = {
        "file_denials": file_denials,
        "hostile_document_denied": hostile_ok,
        "digest_bound_asset_loaded": asset_ok,
        "ephemeral_storage": storage_ok,
    }
    write_json(evidence, "macos-webkit-isolation.json", isolation)
    entitlements = entitlement_evidence()
    entitlements["process_tree"] = isolation.get("process_snapshot", {})
    write_json(evidence, "macos-webkit-entitlements.json", entitlements)
    return isolation


def error_for(worker: Worker, ident: int, *, document: bytes, assets: list[bytes] | None = None) -> str | None:
    return worker.render(ident, "document", 800, document=document, assets=assets).get("error")


def fake_png(size: int) -> bytes:
    data = bytearray(size)
    data[:8] = b"\x89PNG\r\n\x1a\n"
    data[12:16] = b"IHDR"
    data[16:20] = struct.pack(">I", 1)
    data[20:24] = struct.pack(">I", 1)
    return bytes(data)


def run_measurements(evidence: Path, fixtures: Path, build_seconds: float) -> dict[str, Any]:
    cold = []
    cold_snapshots = []
    for ident in range(3):
        started = time.monotonic()
        worker = Worker(fixtures)
        try:
            reply = worker.render(ident + 1, "card", 900)
            cold.append({
                "seconds": time.monotonic() - started,
                "width": reply.get("width"), "height": reply.get("height"),
                "png_bytes": len(base64.b64decode(reply["png_base64"])),
            })
            cold_snapshots.append(process_snapshot(worker.process.pid, worker.baseline))
        finally:
            worker.close()

    worker = Worker(fixtures)
    measurements: dict[str, Any] = {
        "experiment": "macos-webkit", "build_seconds": build_seconds,
        "cold_paths": cold, "cold_process_snapshots": cold_snapshots,
        "warm": {},
        "sampling": "Endpoint process snapshot after renders plus one endpoint snapshot per cold path; not continuous peak sampling.",
        "reference_goals": {"old_macos_bytes": MAX_MEMORY, "linux_scope_bytes": 1024 * 1024 * 1024, "threads": MAX_THREADS},
    }
    try:
        sequence = {}
        for fixture in ("card", "columns"):
            samples = []
            for index in range(10):
                started = time.monotonic()
                reply = worker.render(100 + len(samples), fixture, 900, observe=True)
                samples.append({
                    "seconds": time.monotonic() - started,
                    "width": reply.get("width"), "height": reply.get("height"),
                    "scale_factor": reply.get("scale_factor"),
                    "png_bytes": len(base64.b64decode(reply["png_base64"])),
                    "protocol_bytes": len(json.dumps(reply, separators=(",", ":")).encode()) + 1,
                })
                if index == 0:
                    data = base64.b64decode(reply["png_base64"])
                    name = "macos-webkit-rendered-card.png" if fixture == "card" else "macos-webkit-rendered-columns.png"
                    (evidence / name).write_bytes(data)
                    sequence[fixture] = reply
            measurements["warm"][fixture] = {
                "samples": samples,
                "median_seconds": statistics.median(sample["seconds"] for sample in samples),
                "max_seconds": max(sample["seconds"] for sample in samples),
                "max_png_bytes": max(sample["png_bytes"] for sample in samples),
                "max_protocol_bytes": max(sample["protocol_bytes"] for sample in samples),
            }
        card_800 = worker.render(300, "card", 800)
        card_900 = worker.render(301, "card", 900)
        card_800_png = base64.b64decode(card_800["png_base64"])
        card_900_png = base64.b64decode(card_900["png_base64"])
        measurements["fidelity"] = {
            "card_800": {"width": card_800["width"], "height": card_800["height"], "png_bytes": len(card_800_png)},
            "card_900": {"width": card_900["width"], "height": card_900["height"], "png_bytes": len(card_900_png)},
            "viewport_change_not_stale": card_800_png != card_900_png and card_800["width"] != card_900["width"],
            "literal_worker_invocation": False,
            "https_fixture_asset_loaded": sequence["card"].get("observations", {}).get("asset_natural_width", 0) > 0,
            "font_note": "Fixture requests DejaVu Sans; the tested Mac used WebKit font substitution because no font was installed or mounted.",
            "columns_three_column_source_rule": "grid-template-columns:repeat(3,minmax(0,1fr))",
        }
        tiny = b"<html><body>ok</body></html>"
        oversized = b"x" * (256 * 1024 + 1)
        big_asset = fake_png(1024 * 1024 + 1)
        aggregate_asset = fake_png(1024 * 1024)
        pathological = b"<html><style>body{height:999999px}</style><body>x</body></html>"
        measurements["bounds"] = {
            "html": error_for(worker, 400, document=oversized),
            "asset": error_for(worker, 401, document=tiny, assets=[big_asset]),
            "aggregate_assets": error_for(worker, 402, document=tiny, assets=[aggregate_asset] * 5),
            "height": error_for(worker, 403, document=pathological),
        }
        cancel_worker = Worker(fixtures)
        try:
            started = time.monotonic()
            cancel_worker.process.terminate()
            cancel_worker.process.wait(timeout=0.75)
            measurements["bounds"]["sigterm_seconds"] = time.monotonic() - started
            measurements["bounds"]["sigterm_under_750_ms"] = True
        except subprocess.TimeoutExpired:
            cancel_worker.process.kill()
            cancel_worker.process.wait()
            measurements["bounds"]["sigterm_seconds"] = time.monotonic() - started
            measurements["bounds"]["sigterm_under_750_ms"] = False
        snapshot = process_snapshot(worker.process.pid, worker.baseline)
        measurements["process_snapshot"] = snapshot
        measurements["memory_reference_met"] = snapshot["aggregate_phys_footprint_bytes"] <= MAX_MEMORY
        measurements["thread_ceiling_met"] = snapshot["aggregate_threads"] <= MAX_THREADS
        measurements["png_evidence_ceiling_met"] = all(
            fixture["max_png_bytes"] <= MAX_PNG_EVIDENCE for fixture in measurements["warm"].values()
        )
    finally:
        measurements["cleanup"] = worker.close()
        shutil.rmtree(CONTAINER, ignore_errors=True)
    write_json(evidence, "macos-webkit-measurements.json", measurements)
    return measurements


def self_check(fixtures: Path) -> int:
    encoded = base64.b64encode((fixtures / "card.html").read_bytes()).decode()
    completed = subprocess.run(
        [str(HELPER), "--self-test", "--fixtures-dir", str(fixtures), "--self-test-document-base64", encoded],
        capture_output=True, text=True,
    )
    print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, file=sys.stderr, end="")
    return completed.returncode


def create_results(evidence: Path, isolation: dict[str, Any] | None, measurements: dict[str, Any] | None) -> dict[str, Any]:
    if isolation is None:
        isolation = json.loads((evidence / "macos-webkit-isolation.json").read_text())
    if measurements is None:
        measurements = json.loads((evidence / "macos-webkit-measurements.json").read_text())
    bounds = measurements["bounds"]
    criteria = {
        "ac1_parent_child_file_denials": isolation["assertions"]["file_denials"],
        "ac2_ephemeral_storage": isolation["assertions"]["ephemeral_storage"],
        "ac3_hostile_document_denied": isolation["assertions"]["hostile_document_denied"] and isolation["assertions"]["digest_bound_asset_loaded"],
        "ac4_snapshot_and_resize": measurements["fidelity"]["viewport_change_not_stale"] and not measurements["fidelity"]["https_fixture_asset_loaded"],
        "ac5_bounds": bounds["html"] == "document exceeds HTML ceiling" and bounds["asset"] == "asset exceeds ceiling" and bounds["aggregate_assets"] == "aggregate assets exceed ceiling" and bounds["height"] == "output exceeds 4096 px ceiling" and bounds["sigterm_under_750_ms"],
        "memory_reference": measurements["memory_reference_met"],
        "thread_ceiling": measurements["thread_ceiling_met"],
    }
    machine_conditions = all(criteria.values())
    results = {
        "experiment": "macos-webkit",
        "backend_approved": False,
        "machine_conditions_met": machine_conditions,
        "verdict": "eligible candidate pending human decision" if machine_conditions else "rejected by measured criteria",
        "criteria": criteria,
        "residual_authority": "Required com.apple.security.network.client permits helper process outbound connections even though document-level requests were denied.",
        "decision_required": "Whether mandatory process-level outbound authority is acceptable is a user decision; this evaluation does not approve production integration.",
        "fidelity_gap": "The unchanged card fixture's https emblem is blocked; only the supplementary digest-bound stcli-probe asset loads.",
        "distribution_gap": "Ad-hoc signing is rejected by Gatekeeper; Developer ID signing and notarization were not tested.",
        "evidence": ["macos-webkit-entitlements.json", "macos-webkit-isolation.json", "macos-webkit-measurements.json"],
    }
    write_json(evidence, "results.json", results)
    return results


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--evidence", type=Path, default=HERE / "../../../../docs/experiments/rich-content-renderer/macos-webkit")
    parser.add_argument("--fixtures-dir", type=Path, default=HERE / "fixtures")
    parser.add_argument("--isolation", action="store_true")
    parser.add_argument("--measure", action="store_true")
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--terminal-probe", action="store_true")
    parser.add_argument("--terminal-transfer", type=Path)
    args = parser.parse_args()
    fixtures = args.fixtures_dir.resolve()
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    if args.terminal_probe:
        return common.terminal_probe()
    if args.terminal_transfer:
        return common.terminal_transfer(args.terminal_transfer, evidence)
    build_seconds = build_and_sign()
    if args.self_check:
        return self_check(fixtures)
    isolation = run_isolation(evidence, fixtures) if args.isolation else None
    measurements = run_measurements(evidence, fixtures, build_seconds) if args.measure else None
    if args.isolation or args.measure:
        if isolation is not None and measurements is not None:
            results = create_results(evidence, isolation, measurements)
            print(json.dumps(results, indent=2, sort_keys=True))
        else:
            print(json.dumps({"isolation_written": isolation is not None, "measurements_written": measurements is not None}, sort_keys=True))
        return 0
    return 2


if __name__ == "__main__":
    raise SystemExit(main())

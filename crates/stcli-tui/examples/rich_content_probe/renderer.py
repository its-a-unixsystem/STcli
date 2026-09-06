#!/usr/bin/env python3
"""Ticket-01 disposable rich-content renderer experiment. Not an application API."""

from __future__ import annotations

import argparse
import base64
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
from typing import Any, Callable

ORIGIN = "https://stcli-render.invalid"
ASSET_URL = ORIGIN + "/assets/emblem.png"
ASSET_SHA256 = "f331033487acbbe3714bda038a21e91ef640c9f224748bb7078b9bd5e2eb4817"
CSP = "sandbox; default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src https://stcli-render.invalid; font-src 'none'; connect-src 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
MAX_HTML = 256 * 1024
MAX_ASSET = 1024 * 1024
MAX_ASSET_PIXELS = 1_048_576
MAX_ASSETS_TOTAL = 4 * 1024 * 1024
MAX_WIDTH = 1600
MAX_HEIGHT = 4096
MAX_PNG = 32 * 1024 * 1024
MAX_RESPONSE = 48 * 1024 * 1024
START_TIMEOUT = 10.0
RENDER_TIMEOUT = 3.0
FIXTURE_DIR = Path(__file__).resolve().parent / "fixtures"
FONT = Path("/usr/share/fonts/TTF/DejaVuSans.ttf")


def compact_error(exc: BaseException) -> str:
    text = f"{type(exc).__name__}: {exc}".replace("\n", " ").replace("\r", " ")
    return "".join(c for c in text if c >= " " and c != "\x7f")[:512]


def emit(value: dict[str, Any]) -> None:
    data = json.dumps(value, separators=(",", ":"), sort_keys=True)
    if len(data.encode()) > MAX_RESPONSE:
        data = json.dumps({"error": "response exceeds 48 MiB ceiling"}, separators=(",", ":"))
    print(data, flush=True)


def write_evidence(directory: Path, name: str, value: dict[str, Any]) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / name
    temporary = target.with_suffix(target.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, target)


def png_dimensions(data: bytes, pixel_limit: int = MAX_ASSET_PIXELS) -> tuple[int, int]:
    if len(data) < 24 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError("input is not a PNG")
    width, height = struct.unpack(">II", data[16:24])
    if not width or not height or width * height > pixel_limit:
        raise ValueError("PNG exceeds decoded-pixel ceiling")
    return width, height


def load_inputs() -> tuple[dict[str, bytes], bytes]:
    fixtures = {}
    total = 0
    for name in ("card", "columns"):
        data = (FIXTURE_DIR / f"{name}.html").read_bytes()
        total += len(data)
        if len(data) > MAX_HTML:
            raise ValueError(f"{name} exceeds HTML ceiling")
        fixtures[name] = data
    if total > MAX_HTML:
        raise ValueError("combined fixture HTML exceeds ceiling")
    asset = (FIXTURE_DIR / "asset.png").read_bytes()
    if len(asset) > MAX_ASSET or len(asset) > MAX_ASSETS_TOTAL:
        raise ValueError("approved asset exceeds byte ceiling")
    if hashlib.sha256(asset).hexdigest() != ASSET_SHA256:
        raise ValueError("approved asset digest mismatch")
    png_dimensions(asset)
    return fixtures, asset


def cgroup_observation() -> dict[str, Any]:
    line = next((x for x in Path("/proc/self/cgroup").read_text().splitlines() if x.startswith("0::")), None)
    if not line:
        raise RuntimeError("unified cgroup v2 membership unavailable")
    relative = line[3:].lstrip("/")
    root = Path("/sys/fs/cgroup") / relative
    values = {}
    for key in ("memory.max", "memory.swap.max", "pids.max", "cpu.max", "memory.peak", "cpu.stat", "pids.current", "pids.peak"):
        path = root / key
        if path.exists():
            values[key] = path.read_text().strip()[:2048]
    expected = {"memory.max": "1073741824", "memory.swap.max": "0", "pids.max": "256", "cpu.max": "200000 100000"}
    mismatches = {key: {"expected": wanted, "actual": values.get(key)} for key, wanted in expected.items() if values.get(key) != wanted}
    return {"path": "/" + relative, "values": values, "expected": expected, "effective": not mismatches, "mismatches": mismatches}


def namespace_observation(pid: int) -> dict[str, str]:
    result = {}
    for name in ("user", "pid", "mnt", "net", "ipc", "uts", "cgroup"):
        try:
            result[name] = os.readlink(f"/proc/{pid}/ns/{name}")
        except OSError as exc:
            result[name] = compact_error(exc)
    return result
def descendants(pid: int) -> list[int]:
    found: list[int] = []
    pending = [pid]
    while pending:
        parent = pending.pop()
        try:
            children = [int(value) for value in Path(f"/proc/{parent}/task/{parent}/children").read_text().split()]
        except (OSError, ValueError):
            children = []
        found.extend(children)
        pending.extend(children)
    return found



def process_observation(pid: int) -> dict[str, Any]:
    status = Path(f"/proc/{pid}/status").read_text(errors="replace")
    selected = {}
    for line in status.splitlines():
        key, _, value = line.partition(":")
        if key in {"Pid", "PPid", "NSpid", "NoNewPrivs", "Seccomp", "Seccomp_filters", "CapEff", "Threads"}:
            selected[key] = value.strip()
    sockets = []
    for table in ("tcp", "tcp6"):
        path = Path(f"/proc/{pid}/net/{table}")
        if path.exists():
            for row in path.read_text().splitlines()[1:]:
                fields = row.split()
                if len(fields) > 3 and fields[3] == "0A":
                    sockets.append({"table": table, "local": fields[1]})
    return {"status": selected, "listening_sockets": sockets}

def chromium_process_observations(root_pid: int) -> list[dict[str, Any]]:
    observations = []
    for pid in [root_pid, *descendants(root_pid)]:
        try:
            executable = os.readlink(f"/proc/{pid}/exe")
            if Path(executable).name != "chromium":
                continue
            command = Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
            observation = process_observation(pid)
            observation["pid"] = pid
            observation["command"] = command[:512]
            observations.append(observation)
        except OSError:
            continue
    return observations


def diagnostic_boundary(renderer: Path, sentinel: str, port: int) -> dict[str, Any]:
    fontconfig = tempfile.NamedTemporaryFile("w", prefix="stcli-fonts-", suffix=".conf", delete=False)
    fontconfig.write('<?xml version="1.0"?><fontconfig><dir>/fonts</dir></fontconfig>')
    fontconfig.close()
    script = (
        "import json,os,socket;"
        f"p={sentinel!r};port={port};"
        "r={'host_file_visible':os.path.exists(p),'run_user_visible':os.path.exists('/run/user/1000'),"
        "'host_proc_visible':os.path.exists('/proc/1/root/home')};"
        "s=socket.socket();s.settimeout(.25);"
        "\ntry:s.connect(('127.0.0.1',port));r['loopback_reachable']=True"
        "\nexcept OSError:r['loopback_reachable']=False"
        "\nprint(json.dumps(r))"
    )
    command = ["bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv",
               "--proc", "/proc", "--dev", "/dev", "--size", str(128 * 1024 * 1024), "--tmpfs", "/tmp",
               "--dir", "/fonts", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64",
               "--ro-bind", "/usr/lib", "/usr/lib", "--ro-bind", "/usr/bin/python3", "/usr/bin/python3",
               "--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache", "--ro-bind", str(FONT), "/fonts/DejaVuSans.ttf",
               "--ro-bind", fontconfig.name, "/etc/fonts/fonts.conf", "--setenv", "PATH", "/usr/bin",
               "--setenv", "HOME", "/tmp/home", "--setenv", "LANG", "C.UTF-8", "--", "/usr/bin/python3", "-c", script]
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=2, check=True)
        result = json.loads(completed.stdout)
        result["same_namespace_policy"] = True
        return result
    finally:
        os.unlink(fontconfig.name)


def hostile_document(sentinel: str, port: int) -> bytes:
    loopback = f"http://127.0.0.1:{port}"
    return f'''<!doctype html><html data-state="original"><head>
<meta http-equiv="refresh" content="0;url={loopback}/redirect-marker">
<style>@import url("{loopback}/css-marker");@font-face{{font-family:x;src:url("{loopback}/font-marker")}}body{{background-image:url("file://{sentinel}")}}#marker{{background:url("{loopback}/css-image-marker");font-family:x}}</style>
<script>document.documentElement.dataset.state="script-ran"</script>
<script src="{loopback}/script-marker"></script></head>
<body id="marker" data-state="original" onload="this.dataset.state='onload-ran'" onerror="this.dataset.state='onerror-ran'">
<img src="{loopback}/image-marker" onerror="this.dataset.state='image-handler-ran'">
<img src="file://{sentinel}"><img src="file:///proc/self/status"><img src="file:///proc/self/fd/3">
<iframe srcdoc="<p id=nested-marker>srcdoc marker</p>"></iframe><iframe src="data:text/html,nested-data-marker"></iframe>
<iframe src="javascript:document.body.textContent='javascript-marker'"></iframe><iframe src="chrome://version"></iframe>
<iframe src="{loopback}/json/version"></iframe><iframe src="ws://127.0.0.1:{port}/devtools/browser/control"></iframe>
<object data="{loopback}/object-marker"></object><embed src="{loopback}/embed-marker">
<a href="javascript:document.documentElement.dataset.state='javascript-ran'">inactive link</a>
</body></html>'''.encode()


def dom_isolation_observation(cdp: "CDP", session: str) -> dict[str, Any]:
    document = cdp.call("DOM.getDocument", {"depth": -1, "pierce": True}, session=session)
    root = document["root"]["nodeId"]
    marker = cdp.call("DOM.querySelector", {"nodeId": root, "selector": "#marker"}, session=session).get("nodeId", 0)
    attributes = cdp.call("DOM.getAttributes", {"nodeId": marker}, session=session).get("attributes", []) if marker else []
    marker_attributes = dict(zip(attributes[::2], attributes[1::2]))
    frames = cdp.call("Page.getFrameTree", session=session).get("frameTree", {})
    history = cdp.call("Page.getNavigationHistory", session=session)
    current = history.get("currentIndex", 0)
    entries = history.get("entries", [])
    current_url = entries[current].get("url", "") if 0 <= current < len(entries) else ""
    child_urls = [child.get("frame", {}).get("url", "") for child in frames.get("childFrames", [])]
    search = cdp.call("DOM.performSearch", {"query": "nested-marker OR nested-data-marker OR javascript-marker"}, session=session)
    count = search.get("resultCount", 0)
    if search.get("searchId"):
        cdp.call("DOM.discardSearchResults", {"searchId": search["searchId"]}, session=session)
    loaded_urls = [url for url in child_urls if url not in ("", "about:blank", "about:srcdoc", "chrome-error://chromewebdata/")]
    return {"marker_state": marker_attributes.get("data-state"), "main_url": current_url,
            "child_frame_urls": child_urls, "nested_marker_count": count,
            "nested_document_loaded": bool(loaded_urls or count)}

class CDP:
    def __init__(self, read_fd: int, write_fd: int):
        self.read_fd = read_fd
        self.write_fd = write_fd
        self.buffer = bytearray()
        self.next_id = 1
        self.pending: dict[int, dict[str, Any]] = {}
        self.events: list[dict[str, Any]] = []

    def close(self) -> None:
        for fd in (self.read_fd, self.write_fd):
            try:
                os.close(fd)
            except OSError:
                pass

    def send(self, method: str, params: dict[str, Any] | None = None, session: str | None = None) -> int:
        ident = self.next_id
        self.next_id += 1
        message: dict[str, Any] = {"id": ident, "method": method}
        if params is not None:
            message["params"] = params
        if session is not None:
            message["sessionId"] = session
        payload = json.dumps(message, separators=(",", ":")).encode() + b"\0"
        view = memoryview(payload)
        while view:
            written = os.write(self.write_fd, view)
            view = view[written:]
        return ident

    def receive(self, timeout: float) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            marker = self.buffer.find(0)
            if marker >= 0:
                raw = bytes(self.buffer[:marker])
                del self.buffer[: marker + 1]
                if len(raw) > MAX_RESPONSE:
                    raise RuntimeError("CDP frame exceeds ceiling")
                return json.loads(raw)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("CDP response deadline exceeded")
            ready, _, _ = __import__("select").select([self.read_fd], [], [], remaining)
            if not ready:
                raise TimeoutError("CDP response deadline exceeded")
            chunk = os.read(self.read_fd, 65536)
            if not chunk:
                raise EOFError("Chromium CDP pipe closed")
            self.buffer.extend(chunk)
            if len(self.buffer) > MAX_RESPONSE:
                raise RuntimeError("CDP input buffer exceeds ceiling")

    def call(self, method: str, params: dict[str, Any] | None = None, session: str | None = None,
             timeout: float = RENDER_TIMEOUT, handler: Callable[[dict[str, Any]], None] | None = None) -> dict[str, Any]:
        ident = self.send(method, params, session)
        deadline = time.monotonic() + timeout
        while ident not in self.pending:
            message = self.receive(max(0.001, deadline - time.monotonic()))
            if "id" in message:
                self.pending[int(message["id"])] = message
            elif handler:
                handler(message)
            else:
                self.events.append(message)
        response = self.pending.pop(ident)
        if "error" in response:
            error = response["error"]
            raise RuntimeError(f"CDP {method} failed: {error.get('message', 'unknown error')}")
        return response.get("result", {})

    def pump(self, timeout: float, handler: Callable[[dict[str, Any]], None]) -> None:
        message = self.receive(timeout)
        if "id" in message:
            self.pending[int(message["id"])] = message
        else:
            handler(message)


class Browser:
    def __init__(self, renderer: Path):
        if not renderer.is_absolute() or not renderer.is_file() or not os.access(renderer, os.X_OK):
            raise RuntimeError("renderer must be an executable absolute path")
        if not FONT.is_file() or not Path("/etc/ld.so.cache").is_file():
            raise RuntimeError("required loader cache or DejaVu font missing")
        self.fixtures, self.asset = load_inputs()
        self.renderer = renderer
        self.temp = tempfile.TemporaryDirectory(prefix="stcli-rich-probe-")
        fontconfig = Path(self.temp.name) / "fonts.conf"
        fontconfig.write_text('<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig><dir>/fonts</dir><cachedir>/tmp/font-cache</cachedir></fontconfig>', encoding="utf-8")
        mounts = [
            ["--ro-bind", "/usr/lib/chromium", "/usr/lib/chromium"],
            ["--ro-bind", "/usr/lib", "/usr/lib"],
            ["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"],
            ["--ro-bind", str(FONT), "/fonts/DejaVuSans.ttf"],
            ["--ro-bind", str(fontconfig), "/etc/fonts/fonts.conf"],
        ]
        loader_links = [("--symlink", "usr/lib", "/lib"), ("--symlink", "usr/lib", "/lib64")]
        self.mounts = mounts + loader_links
        command = ["bwrap", "--unshare-all", "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv",
                   "--proc", "/proc", "--dev", "/dev", "--size", str(128 * 1024 * 1024), "--tmpfs", "/tmp",
                   "--dir", "/fonts", "--setenv", "PATH", "/usr/lib/chromium:/usr/bin", "--setenv", "HOME", "/tmp/home",
                   "--setenv", "LANG", "C.UTF-8", "--setenv", "FONTCONFIG_FILE", "/etc/fonts/fonts.conf"]
        for link in loader_links:
            command.extend(link)
        for mount in mounts:
            command.extend(mount)
        command.extend(["--", str(renderer), "--headless=new", "--remote-debugging-pipe", "--user-data-dir=/tmp/profile",
                        "--no-first-run", "--no-default-browser-check", "--disable-background-networking", "--disable-component-update",
                        "--disable-domain-reliability", "--disable-sync", "--metrics-recording-only", "--disable-breakpad",
                        "--disable-features=MediaRouter,OptimizationHints,AutofillServerCommunication", "--password-store=basic",
                        "--use-mock-keychain", "--disable-gpu", "about:blank"])
        to_child_r, controller_write = os.pipe()
        controller_read, from_child_w = os.pipe()
        saved_read = os.dup(3)
        saved_write = os.dup(4)
        os.dup2(to_child_r, 3)
        os.dup2(from_child_w, 4)
        started = time.monotonic()
        try:
            self.process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                            stderr=subprocess.PIPE, close_fds=True, pass_fds=(3, 4), env={})
        finally:
            os.dup2(saved_read, 3)
            os.dup2(saved_write, 4)
            for fd in (saved_read, saved_write, to_child_r, from_child_w):
                os.close(fd)
        self.cdp = CDP(controller_read, controller_write)
        try:
            self.version = self.cdp.call("Browser.getVersion", timeout=START_TIMEOUT)
            targets = self.cdp.call("Target.getTargets", timeout=START_TIMEOUT).get("targetInfos", [])
            initial = next((target for target in targets if target.get("type") == "page"), None)
            if not initial or initial.get("url") != "about:blank":
                raise RuntimeError("initial about:blank page target is unavailable")
            self.initial_target = {"targetId": initial.get("targetId"), "url": initial.get("url"),
                                   "attached": initial.get("attached", False)}
        except Exception as exc:
            cleanup = self.close()
            raise RuntimeError(f"CDP pipe handshake failed: {exc}; browser: {cleanup.get('stderr', 'no stderr')}") from exc
        self.start_seconds = time.monotonic() - started
        if self.start_seconds > START_TIMEOUT:
            self.close()
            raise TimeoutError("cold startup exceeds 10 second ceiling")
        self.cgroup = cgroup_observation()
        if not self.cgroup["effective"]:
            self.close()
            raise RuntimeError("effective cgroup ceilings do not match mandatory policy")
        own = namespace_observation(os.getpid())
        isolated_pid = next(
            (pid for pid in [self.process.pid, *descendants(self.process.pid)]
             if all(namespace_observation(pid).get(boundary) != own.get(boundary)
                    for boundary in ("mnt", "net", "pid", "ipc", "uts"))),
            None,
        )
        if isolated_pid is None:
            self.close()
            raise RuntimeError("Bubblewrap did not expose an isolated Chromium descendant")
        self.namespaces = namespace_observation(isolated_pid)
        self.process_info = process_observation(isolated_pid)
        self.process_info["observed_pid"] = isolated_pid
        if self.process_info["listening_sockets"]:
            self.close()
            raise RuntimeError("sandbox exposes a listening network socket")
    def target_diagnostic(self) -> dict[str, Any]:
        result: dict[str, Any] = {}
        try:
            target = self.cdp.call("Target.createTarget", {"url": "about:blank", "newWindow": False},
                                   timeout=RENDER_TIMEOUT)["targetId"]
            result["default_context_target"] = "created"
            self.cdp.call("Target.closeTarget", {"targetId": target}, timeout=RENDER_TIMEOUT)
        except Exception as exc:
            result["default_context_target"] = compact_error(exc)
        context = None
        try:
            context = self.cdp.call("Target.createBrowserContext", {"disposeOnDetach": True},
                                    timeout=RENDER_TIMEOUT)["browserContextId"]
            target = self.cdp.call("Target.createTarget", {"url": "about:blank", "browserContextId": context,
                                                           "newWindow": True}, timeout=RENDER_TIMEOUT)["targetId"]
            result["fresh_context_target"] = "created"
            self.cdp.call("Target.closeTarget", {"targetId": target}, timeout=RENDER_TIMEOUT)
        except Exception as exc:
            result["fresh_context_target"] = compact_error(exc)
        finally:
            if context:
                try:
                    self.cdp.call("Target.disposeBrowserContext", {"browserContextId": context}, timeout=1.0)
                except Exception:
                    pass
        return result

    def _reply(self, method: str, params: dict[str, Any], session: str) -> None:
        self.cdp.call(method, params, session=session, timeout=RENDER_TIMEOUT)

    def render(self, fixture: str, width: int, deadline: float = RENDER_TIMEOUT,
               document: bytes | None = None, observe_dom: bool = False) -> dict[str, Any]:
        if fixture not in self.fixtures:
            raise ValueError("unknown fixture")
        if isinstance(width, bool) or not isinstance(width, int) or width < 160 or width > MAX_WIDTH:
            raise ValueError("width outside 160..1600")
        content = self.fixtures[fixture] if document is None else document
        if len(content) > MAX_HTML:
            raise ValueError("document exceeds HTML ceiling")
        end = time.monotonic() + deadline
        context = self.cdp.call("Target.createBrowserContext", {"disposeOnDetach": True}, timeout=deadline)["browserContextId"]
        unexpected: list[str] = []
        denied: list[str] = []
        served_main = False
        loaded = False
        session = ""
        try:
            target = self.cdp.call("Target.createTarget", {"url": "about:blank", "browserContextId": context, "newWindow": True}, timeout=deadline)["targetId"]
            session = self.cdp.call("Target.attachToTarget", {"targetId": target, "flatten": True}, timeout=deadline)["sessionId"]
            def handler(event: dict[str, Any]) -> None:
                nonlocal served_main, loaded
                method = event.get("method")
                sid = event.get("sessionId", session)
                params = event.get("params", {})
                if method == "Page.loadEventFired" and sid == session:
                    loaded = True
                elif method == "Target.attachedToTarget":
                    info = params.get("targetInfo", {})
                    if info.get("targetId") != target:
                        unexpected.append(str(info.get("type", "unknown"))[:64])
                        child_session = params.get("sessionId")
                        if child_session:
                            self.cdp.call("Target.closeTarget", {"targetId": info["targetId"]}, timeout=max(.05, end-time.monotonic()))
                elif method == "Fetch.requestPaused" and sid == session:
                    request_id = params["requestId"]
                    request = params.get("request", {})
                    url = request.get("url", "")
                    resource = params.get("resourceType", "")
                    if url == ORIGIN + f"/{fixture}.html" and resource == "Document" and not served_main:
                        served_main = True
                        body = base64.b64encode(content).decode()
                        headers = [{"name": "Content-Type", "value": "text/html; charset=utf-8"},
                                   {"name": "Content-Security-Policy", "value": CSP},
                                   {"name": "Cache-Control", "value": "no-store"}]
                        self._reply("Fetch.fulfillRequest", {"requestId": request_id, "responseCode": 200, "responseHeaders": headers, "body": body}, session)
                    elif url == ASSET_URL and resource == "Image" and fixture == "card":
                        self._reply("Fetch.fulfillRequest", {"requestId": request_id, "responseCode": 200,
                                    "responseHeaders": [{"name": "Content-Type", "value": "image/png"}, {"name": "Cache-Control", "value": "no-store"}],
                                    "body": base64.b64encode(self.asset).decode()}, session)
                    else:
                        denied.append(f"{resource}:{url[:160]}")
                        self._reply("Fetch.failRequest", {"requestId": request_id, "errorReason": "BlockedByClient"}, session)
            session_commands = (
                ("Page.enable", {}),
                ("Runtime.enable", {}),
                ("Emulation.setScriptExecutionDisabled", {"value": True}),
                ("Emulation.setDeviceMetricsOverride", {"width": width, "height": 600, "deviceScaleFactor": 1, "mobile": False}),
                ("Fetch.enable", {"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}),
                ("Target.setAutoAttach", {"autoAttach": True, "waitForDebuggerOnStart": True, "flatten": True}),
            )
            self.cdp.call("Browser.setDownloadBehavior", {"behavior": "deny", "browserContextId": context},
                          timeout=max(.05, end-time.monotonic()), handler=handler)
            for method, params in session_commands:
                self.cdp.call(method, params, session=session, timeout=max(.05, end-time.monotonic()), handler=handler)
            self.cdp.call("Page.navigate", {"url": ORIGIN + f"/{fixture}.html"}, session=session,
                          timeout=max(.05, end-time.monotonic()), handler=handler)
            while not loaded and time.monotonic() < end:
                self.cdp.pump(max(.01, end-time.monotonic()), handler)
            if not loaded or not served_main:
                raise TimeoutError("document did not complete within render deadline")
            metrics = self.cdp.call("Page.getLayoutMetrics", session=session, timeout=max(.05, end-time.monotonic()), handler=handler)
            size = metrics.get("cssContentSize") or metrics.get("contentSize") or {}
            height = max(1, int(float(size.get("height", 0)) + .999))
            actual_width = width
            if actual_width > MAX_WIDTH or height > MAX_HEIGHT or actual_width * height > MAX_WIDTH * MAX_HEIGHT:
                raise ValueError("rendered output exceeds pixel ceiling")
            shot = self.cdp.call("Page.captureScreenshot", {"format": "png", "fromSurface": True,
                                 "captureBeyondViewport": True, "clip": {"x": 0, "y": 0, "width": actual_width, "height": height, "scale": 1}},
                                 session=session, timeout=max(.05, end-time.monotonic()), handler=handler)
            encoded = shot.get("data", "")
            if len(encoded) > (MAX_PNG * 4 // 3 + 8):
                raise ValueError("PNG exceeds encoded response ceiling")
            raw = base64.b64decode(encoded, validate=True)
            if len(raw) > MAX_PNG:
                raise ValueError("PNG exceeds 32 MiB ceiling")
            png_width, png_height = png_dimensions(raw, MAX_WIDTH * MAX_HEIGHT)
            if png_width != actual_width or png_height != height:
                raise RuntimeError("screenshot dimensions do not match layout")
            observations = {"denied_requests": denied, "unexpected_targets": unexpected}
            if observe_dom:
                observations["dom"] = dom_isolation_observation(self.cdp, session)
            return {"width": actual_width, "height": height, "png_base64": encoded, "observations": observations}
        finally:
            try:
                self.cdp.call("Target.disposeBrowserContext", {"browserContextId": context}, timeout=1.0)
            except Exception:
                pass

    def close(self) -> dict[str, Any]:
        observation: dict[str, Any] = {"graceful": False, "reaped": False}
        cdp = getattr(self, "cdp", None)
        process = getattr(self, "process", None)
        if cdp:
            try:
                cdp.call("Browser.close", timeout=1.0)
                observation["graceful"] = True
            except Exception:
                pass
            cdp.close()
        if process:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=1)
            observation["reaped"] = process.poll() is not None
            if process.stderr:
                diagnostic = process.stderr.read(4096).decode(errors="replace")
                if diagnostic:
                    bounded = diagnostic if len(diagnostic) <= 512 else diagnostic[:240] + " ... " + diagnostic[-267:]
                    observation["stderr"] = compact_error(RuntimeError(bounded))
                else:
                    observation["stderr"] = ""
        temporary = getattr(self, "temp", None)
        if temporary:
            temporary.cleanup()
            observation["temporary_removed"] = not Path(temporary.name).exists()
        return observation

    def __enter__(self) -> "Browser": return self
    def __exit__(self, *_: Any) -> None: self.close()


def worker(renderer: Path) -> int:
    browser: Browser | None = None
    last = time.monotonic()
    try:
        for line in sys.stdin.buffer:
            if len(line) > 4096:
                emit({"id": 0, "error": "request exceeds 4096-byte framing ceiling"}); continue
            try:
                request = json.loads(line)
                ident = request.get("id")
                if isinstance(ident, bool) or not isinstance(ident, int) or ident < 0 or ident > 2**64 - 1:
                    raise ValueError("id must be u64")
                if set(request) != {"id", "fixture", "width"}:
                    raise ValueError("worker accepts only id, fixture, and width")
                if browser is not None and time.monotonic() - last > 30:
                    browser.close(); browser = None
                if browser is None:
                    browser = Browser(renderer)
                result = browser.render(request["fixture"], request["width"])
                result.pop("observations", None)
                result["id"] = ident
                emit(result)
                last = time.monotonic()
            except Exception as exc:
                response = {"id": request.get("id", 0) if isinstance(locals().get("request"), dict) else 0, "error": compact_error(exc)}
                emit(response)
                if browser is not None:
                    browser.close(); browser = None
    finally:
        if browser is not None:
            browser.close()
    return 0


def isolation(renderer: Path, evidence: Path) -> int:
    result: dict[str, Any] = {"experiment": "ticket-01", "mode": "isolation", "backend_approved": False,
                              "ceilings": {"html_bytes": MAX_HTML, "asset_bytes": MAX_ASSET, "asset_pixels": MAX_ASSET_PIXELS,
                                           "aggregate_asset_bytes": MAX_ASSETS_TOTAL, "output": [MAX_WIDTH, MAX_HEIGHT],
                                           "png_bytes": MAX_PNG, "response_bytes": MAX_RESPONSE, "startup_seconds": START_TIMEOUT,
                                           "render_seconds": RENDER_TIMEOUT}}
    sentinel = tempfile.NamedTemporaryFile(prefix="stcli-host-sentinel-", delete=False)
    sentinel.write(b"owned harmless sentinel"); sentinel.close()
    listener = socket.socket(); listener.bind(("127.0.0.1", 0)); listener.listen(1)
    loopback_port = listener.getsockname()[1]
    client = socket.create_connection(("127.0.0.1", loopback_port), timeout=1)
    accepted, _ = listener.accept()
    client.sendall(b"outside-control")
    loopback_reachable = accepted.recv(15) == b"outside-control"
    accepted.sendall(b"outside-reply")
    loopback_reachable = loopback_reachable and client.recv(13) == b"outside-reply"
    accepted.close(); client.close()
    result["positive_controls"] = {"host_file_readable_outside": Path(sentinel.name).read_bytes() == b"owned harmless sentinel",
                                   "loopback_listener_outside": loopback_reachable, "loopback_port": loopback_port}
    try:
        browser = Browser(renderer)
        result.update({"browser_version": browser.version, "initial_target": browser.initial_target,
                       "target_diagnostic": browser.target_diagnostic(), "mounts": browser.mounts,
                       "cgroup": browser.cgroup, "namespaces": browser.namespaces,
                       "process": browser.process_info,
                       "chromium_processes": chromium_process_observations(browser.process.pid),
                       "boundary_probe": diagnostic_boundary(renderer, sentinel.name, loopback_port)})
        try:
            rendered = browser.render("card", 800)
            result.update({"browser_version": browser.version, "mounts": browser.mounts,
                           "cgroup": browser.cgroup, "namespaces": browser.namespaces, "process": browser.process_info,
                           "positive_render": {"width": rendered["width"], "height": rendered["height"],
                                               "png_bytes": len(base64.b64decode(rendered["png_base64"])),
                                               **rendered["observations"]}})
            hostile = browser.render("card", 800, document=hostile_document(sentinel.name, loopback_port), observe_dom=True)
            result["hostile_document"] = hostile["observations"]
            result["hostile_document"]["png_bytes"] = len(base64.b64decode(hostile["png_base64"]))
            result["hostile_document"]["listener_requests"] = []
            listener.setblocking(False)
            try:
                while True:
                    connection, _ = listener.accept()
                    result["hostile_document"]["listener_requests"].append(connection.recv(512).decode(errors="replace")[:512])
                    connection.close()
            except BlockingIOError:
                pass
            boundary = result["boundary_probe"]
            dom = result["hostile_document"].get("dom", {})
            processes = result["chromium_processes"]
            renderer_processes = [process for process in processes if "--type=renderer" in process.get("command", "")]
            process_boundary_holds = (
                bool(renderer_processes)
                and all(not process.get("listening_sockets") for process in processes)
                and all(process.get("status", {}).get("CapEff") == "0000000000000000" for process in renderer_processes)
                and all(process.get("status", {}).get("NoNewPrivs") == "1" for process in renderer_processes)
                and all(process.get("status", {}).get("Seccomp") == "2" for process in renderer_processes)
                and all(int(process.get("status", {}).get("Seccomp_filters", "0")) >= 1 for process in renderer_processes)
            )
            result["backend_approved"] = (
                all(result["positive_controls"].get(key) for key in ("host_file_readable_outside", "loopback_listener_outside"))
                and result["cgroup"].get("effective") is True
                and not result["hostile_document"]["listener_requests"]
                and not boundary.get("host_file_visible")
                and not boundary.get("host_proc_visible")
                and not boundary.get("loopback_reachable")
                and not boundary.get("run_user_visible")
                and boundary.get("same_namespace_policy") is True
                and dom.get("marker_state") == "original"
                and dom.get("main_url") == ORIGIN + "/card.html"
                and not dom.get("nested_document_loaded")
                and not result["hostile_document"].get("unexpected_targets")
                and process_boundary_holds
            )
            try: browser.render("card", MAX_WIDTH + 1)
            except Exception as exc: result["oversize_denial"] = compact_error(exc)
            try: browser.render("columns", 800, .000001)
            except Exception as exc: result["deadline_denial"] = compact_error(exc)
        finally:
            result["cleanup"] = browser.close()
    except Exception as exc:
        result["no_backend"] = compact_error(exc)
    finally:
        listener.close(); os.unlink(sentinel.name)
    write_evidence(evidence, "isolation.json", result); emit(result)
    return 0 if result["backend_approved"] else 2


def measure(renderer: Path, evidence: Path) -> int:
    result: dict[str, Any] = {"experiment": "ticket-01", "mode": "measure", "backend_approved": False, "cold_starts": [], "renders": {}}
    try:
        result["worker_cold_paths"] = worker_cold_samples(renderer)
        for _ in range(3):
            browser = Browser(renderer); result["cold_starts"].append(browser.start_seconds); browser.close()
        browser = Browser(renderer)
        try:
            result.update({"browser_version": browser.version, "mounts": browser.mounts, "cgroup": browser.cgroup,
                           "namespaces": browser.namespaces, "process": browser.process_info})
            for fixture in ("card", "columns"):
                samples = []
                for _ in range(10):
                    start = time.monotonic(); rendered = browser.render(fixture, 900); elapsed = time.monotonic() - start
                    samples.append({"seconds": elapsed, "png_bytes": len(base64.b64decode(rendered["png_base64"])),
                                    "width": rendered["width"], "height": rendered["height"]})
                result["renders"][fixture] = {"samples": samples, "median_seconds": statistics.median(x["seconds"] for x in samples),
                                               "max_seconds": max(x["seconds"] for x in samples),
                                               "max_png_bytes": max(x["png_bytes"] for x in samples)}
            result["backend_approved"] = True
        finally:
            result["cleanup"] = browser.close()
    except Exception as exc:
        result["no_backend"] = compact_error(exc)
    write_evidence(evidence, "measurements.json", result); emit(result)
    return 0 if result["backend_approved"] else 2


def worker_cold_samples(renderer: Path) -> list[float]:
    samples = []
    helper = Path(__file__).resolve()
    for index in range(3):
        unit = f"stcli-rich-cold-{os.getpid()}-{index}"
        command = ["systemd-run", "--user", "--scope", "--quiet", f"--unit={unit}",
                   "-p", "MemoryMax=1G", "-p", "MemorySwapMax=0", "-p", "TasksMax=256", "-p", "CPUQuota=200%",
                   "python3", str(helper), "--worker", "--renderer", str(renderer)]
        started = time.monotonic()
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        request = b'{"id":1,"fixture":"card","width":900}\n'
        stdout, _ = process.communicate(request, timeout=START_TIMEOUT)
        elapsed = time.monotonic() - started
        reply = json.loads(stdout)
        if reply.get("id") != 1 or "png_base64" not in reply or process.returncode != 0:
            raise RuntimeError("cold worker path did not return the rendered card")
        samples.append(elapsed)
    return samples


def tty_exchange(query: bytes, pattern: bytes, timeout: float = .5) -> tuple[bytes, float]:
    fd = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    old = termios.tcgetattr(fd); changed = termios.tcgetattr(fd)
    disabled = b"\0" if isinstance(changed[6][termios.VMIN], bytes) else 0
    changed[3] &= ~(termios.ICANON | termios.ECHO); changed[6][termios.VMIN] = disabled; changed[6][termios.VTIME] = disabled
    start = time.monotonic(); response = bytearray()
    try:
        termios.tcsetattr(fd, termios.TCSANOW, changed)
        os.write(fd, query); termios.tcdrain(fd)
        while time.monotonic() - start < timeout:
            ready, _, _ = __import__("select").select(
                [fd], [], [], max(0, timeout - (time.monotonic() - start))
            )
            if ready:
                try: response.extend(os.read(fd, 4096))
                except BlockingIOError: pass
                if pattern in response: return bytes(response), time.monotonic() - start
        return bytes(response), time.monotonic() - start
    finally:
        termios.tcsetattr(fd, termios.TCSANOW, old); os.close(fd)

def terminal_probe() -> int:
    result: dict[str, Any] = {"supported": False, "cell_width": 0, "cell_height": 0, "reason": "terminal did not acknowledge Kitty graphics within 500 ms"}
    try:
        response, _ = tty_exchange(b"\x1b_Gi=31,a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\", b"OK", .5)
        fd = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY)
        try:
            rows, columns, pixel_width, pixel_height = struct.unpack("HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8))
        finally:
            os.close(fd)
        if b"OK" in response and rows and columns and pixel_width and pixel_height:
            result = {"supported": True, "cell_width": max(1, pixel_width // columns),
                      "cell_height": max(1, pixel_height // rows), "reason": "Kitty graphics acknowledged"}
        elif response:
            result["reason"] = "terminal replied without complete Kitty graphics and cell metrics"
    except Exception as exc:
        result["reason"] = compact_error(exc)
    emit(result); return 0


def terminal_transfer(png: Path, evidence: Path) -> int:
    result: dict[str, Any] = {"experiment": "ticket-01", "mode": "terminal-transfer", "acknowledged": False}
    try:
        data = png.read_bytes()
        if len(data) > MAX_PNG: raise ValueError("PNG exceeds transfer ceiling")
        png_dimensions(data, MAX_WIDTH * MAX_HEIGHT)
        encoded = base64.b64encode(data); chunks = [encoded[i:i+4096] for i in range(0, len(encoded), 4096)]
        image_id = 918273
        payload = bytearray()
        for index, chunk in enumerate(chunks):
            more = 1 if index + 1 < len(chunks) else 0
            controls = f"a=t,t=d,f=100,i={image_id},q=0,m={more}" if index == 0 else f"m={more}"
            payload.extend(b"\x1b_G" + controls.encode() + b";" + chunk + b"\x1b\\")
        fd = os.open("/dev/tty", os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        old = termios.tcgetattr(fd); changed = termios.tcgetattr(fd)
        disabled = b"\0" if isinstance(changed[6][termios.VMIN], bytes) else 0
        changed[3] &= ~(termios.ICANON | termios.ECHO); changed[6][termios.VMIN] = disabled; changed[6][termios.VTIME] = disabled
        started = time.monotonic(); response = bytearray()
        try:
            termios.tcsetattr(fd, termios.TCSANOW, changed)
            view = memoryview(payload)
            while view:
                try:
                    written = os.write(fd, view)
                    view = view[written:]
                except BlockingIOError:
                    __import__("select").select([], [fd], [], .5)
            termios.tcdrain(fd)
            flushed = time.monotonic()
            while time.monotonic() - flushed < .5 and f"i={image_id};OK".encode() not in response:
                ready, _, _ = __import__("select").select(
                    [fd], [], [], max(0, .5 - (time.monotonic() - flushed))
                )
                if ready:
                    try: response.extend(os.read(fd, 4096))
                    except BlockingIOError: pass
            acknowledged_at = time.monotonic()
        finally:
            termios.tcsetattr(fd, termios.TCSANOW, old); os.close(fd)
        try: tty_exchange(f"\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\".encode(), b"never", .01)
        except Exception: pass
        result.update({"acknowledged": f"i={image_id};OK".encode() in response, "png_bytes": len(data),
                       "protocol_bytes": len(payload), "write_flush_seconds": flushed - started,
                       "ack_seconds_after_flush": acknowledged_at - flushed, "image_id": image_id})
    except Exception as exc:
        result["error"] = compact_error(exc)
    write_evidence(evidence, "terminal-transfer.json", result); emit(result)
    return 0 if result["acknowledged"] else 2


def main() -> int:
    parser = argparse.ArgumentParser(description="ticket-01 disposable isolated rich-content renderer")
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--worker", action="store_true"); modes.add_argument("--isolation", action="store_true")
    modes.add_argument("--measure", action="store_true"); modes.add_argument("--terminal-probe", action="store_true")
    modes.add_argument("--terminal-transfer", type=Path, metavar="PNG")
    parser.add_argument("--renderer", type=Path, default=Path("/usr/lib/chromium/chromium"))
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    if args.terminal_probe: return terminal_probe()
    if args.terminal_transfer:
        if args.evidence is None: parser.error("--terminal-transfer requires --evidence")
        return terminal_transfer(args.terminal_transfer, args.evidence)
    if args.worker: return worker(args.renderer)
    if args.evidence is None: parser.error("--isolation/--measure require --evidence")
    return isolation(args.renderer, args.evidence) if args.isolation else measure(args.renderer, args.evidence)


if __name__ == "__main__":
    try: raise SystemExit(main())
    except KeyboardInterrupt: raise SystemExit(130)

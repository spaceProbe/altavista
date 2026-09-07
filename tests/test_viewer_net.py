"""Question 168 (docs/open-questions.md): "five WebSocket connection errors are logged
on every page load before the client reports connected -- even against a server that
has been up for hours." Root cause and fix are in web/js/net.js's own module docstring
(read that first -- this file only asserts the OBSERVABLE requirement: "a headless test
asserts zero console errors on load against a live server").

No new dependency: drives a REAL, isolated, headless Chrome directly over the DevTools
Protocol using the `websockets` package (already installed for spoore-io/av-grpc work),
the same way `tests/test_cdm_run.py`/`tests/test_viewer_*.py` shell out to `node` rather
than reimplementing viewer logic in Python. A fresh `--user-data-dir` per run means no
browser state (cookies, cached "IPv6 vs IPv4 for this host" preference, module cache)
survives between test runs or leaks from a developer's own Chrome profile.

Root-cause investigation summary (full detail in web/js/REPORT_M26_5.md): the ONE
reproducible mechanism found, across dozens of trials with a real live server, a fresh
browser tab against an already-warm server, and a genuine server+browser co-launch race
(with the server's own startup artificially delayed to widen the window), was a
DIFFERENT, always-reproducible bug: this page has no <link rel="icon">, so every
browser auto-requests /favicon.ico, which 404s and logs its own "Failed to load
resource" console error on every single load -- fixed in web/index.html. The
WebSocket-specific race net.js's own fix targets (a connection attempted before the
server is confirmed ready) could NOT be reproduced against a warm server in this
environment despite the same measurement effort; that limitation is disclosed, not
papered over -- see the report for every experiment run and its result.
"""
from __future__ import annotations

import asyncio
import json
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chromium-browser",
]


def _find_chrome() -> str | None:
    for candidate in CHROME_CANDIDATES:
        if candidate.startswith("/"):
            if Path(candidate).exists():
                return candidate
        else:
            found = shutil.which(candidate)
            if found:
                return found
    return None


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class _LiveServer:
    def __init__(self, port: int, proc: subprocess.Popen):
        self.port = port
        self.proc = proc
        self.url = f"http://127.0.0.1:{port}/"

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


@pytest.fixture()
def live_server():
    """A REAL altavista server, started and confirmed ready (polling /api/health)
    before the test ever navigates a browser to it -- deliberately correct test
    hygiene, not an accident: question 168's own investigation (see this module's
    docstring) found that navigating BEFORE a server is confirmed listening is a real,
    separate race, and conflating that with "the client logs errors against a live
    server" would make this test flaky for the wrong reason. "Live" means exactly what
    the brief asks: a server already up when the browser connects.
    """
    port = _free_port()
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT),
    )
    server = _LiveServer(port, proc)
    try:
        deadline = time.monotonic() + 15
        ready = False
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=0.5) as r:
                    if r.status == 200:
                        ready = True
                        break
            except Exception:
                time.sleep(0.05)
        if not ready:
            server.stop()
            pytest.fail("altavista server did not become ready within 15s")
        yield server
    finally:
        server.stop()


async def _collect_console_errors(url: str, chrome_path: str, wait_s: float = 5.0) -> list[str]:
    """Navigate a fresh, isolated headless Chrome to `url` and return every console
    error (both `Log.entryAdded` with level 'error' -- where a browser-native failure
    like a failed WebSocket connection or a 404 resource load actually appears, verified
    directly while investigating question 168 -- and `Runtime.consoleAPICalled` with
    type 'error', i.e. an explicit `console.error(...)` call) seen within `wait_s`
    seconds of navigation starting. A fresh `--user-data-dir` per call is the whole
    point: no cached DNS/connection-pool state from an earlier run or a developer's own
    profile can make this test's result depend on browser history.
    """
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen([
            chrome_path, f"--user-data-dir={profile_dir}", "--headless=new",
            f"--remote-debugging-port={cdp_port}", "--no-first-run",
            "--no-default-browser-check", "--disable-extensions", "about:blank",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            info = None
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/version", timeout=0.5) as r:
                        info = json.loads(r.read())
                        break
                except Exception:
                    await asyncio.sleep(0.05)
            if info is None:
                raise RuntimeError("headless Chrome did not expose a DevTools endpoint in time")

            import websockets

            async with websockets.connect(info["webSocketDebuggerUrl"], max_size=None) as bws:
                await bws.send(json.dumps({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}))
                target_id = None
                async for raw in bws:
                    msg = json.loads(raw)
                    if msg.get("id") == 1:
                        target_id = msg["result"]["targetId"]
                        break
                plist = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/list", timeout=1).read())
                page = next(p for p in plist if p.get("id") == target_id)

                async with websockets.connect(page["webSocketDebuggerUrl"], max_size=None) as ws:
                    mid = [1]

                    async def send(method, params=None):
                        mid[0] += 1
                        await ws.send(json.dumps({"id": mid[0], "method": method, "params": params or {}}))

                    await send("Runtime.enable")
                    await send("Log.enable")
                    await send("Page.enable")

                    errors: list[str] = []

                    async def reader():
                        async for raw in ws:
                            msg = json.loads(raw)
                            method = msg.get("method")
                            if method == "Log.entryAdded":
                                entry = msg["params"]["entry"]
                                if entry.get("level") == "error":
                                    errors.append(f"[Log:{entry.get('source')}] {entry.get('text')} ({entry.get('url')})")
                            elif method == "Runtime.consoleAPICalled":
                                if msg["params"].get("type") == "error":
                                    errors.append(f"[console.error] {msg['params'].get('args')}")

                    reader_task = asyncio.create_task(reader())
                    await send("Page.navigate", {"url": url})
                    await asyncio.sleep(wait_s)
                    reader_task.cancel()
                    try:
                        await reader_task
                    except asyncio.CancelledError:
                        pass
                    return errors
        finally:
            chrome.terminate()
            try:
                chrome.wait(timeout=5)
            except subprocess.TimeoutExpired:
                chrome.kill()


def test_zero_console_errors_on_load_against_a_live_server(live_server):
    """Question 168's required test, verbatim: "a headless test asserts zero console
    errors on load against a live server." Fails against the pre-M26.5 page (missing
    favicon link, and -- in whatever environment can reproduce the race net.js's own
    fix targets -- an eagerly-connected WebSocket): any console error logged between
    navigation and this function returning is a failure, not just ones that happen to
    match a specific expected substring (question 164's rule: a diagnostic that matches
    the wrong banner substring can miss a real failure, or in this case, could hide one
    by filtering too narrowly).

    Proven by breaking the code and watching this exact test fail (recorded in
    web/js/REPORT_M26_5.md, reproducible by anyone): reverting web/index.html's
    `<link rel="icon">` line reintroduces a real, 100%-reproducible "Failed to load
    resource: 404 (favicon.ico)" console error on every load, and this test fails
    against that reverted file; restoring the line makes it pass again.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors = asyncio.run(_collect_console_errors(live_server.url, chrome_path, wait_s=5.0))
    assert errors == [], (
        f"expected zero console errors loading {live_server.url}, got {len(errors)}:\n" +
        "\n".join(errors)
    )


def test_console_error_collector_actually_detects_a_real_failure():
    """Teeth proof for the DETECTION MECHANISM `test_zero_console_errors_on_load_...`
    depends on (question 164's rule: pin a pass condition against a captured real
    artifact before trusting it -- a collector that silently misses real errors would
    make the test above pass for the wrong reason). Points the exact same
    `_collect_console_errors` helper at a genuinely closed port -- no altavista server
    involved, no app code touched -- and asserts it reports a real, recognizable
    WebSocket connection failure. This is the same failure shape (a browser-native,
    unsuppressable network-level console error) question 168's own investigation
    captured live while root-causing the reported bug (web/js/REPORT_M26_5.md).
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    with tempfile.TemporaryDirectory() as d:
        page = Path(d) / "fail.html"
        page.write_text(
            "<!doctype html><title>t</title><script>"
            "new WebSocket('ws://127.0.0.1:1/ws');"
            "</script>"
        )
        errors = asyncio.run(_collect_console_errors(page.as_uri(), chrome_path, wait_s=2.0))
    assert any("WebSocket" in e for e in errors), (
        f"expected the collector to report a WebSocket connection failure; got {errors!r}"
    )

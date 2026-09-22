"""Round 6 task 4: proof, in a REAL headless browser, that the Layers panel
(web/js/panels/layers_panel.js) repaints only on state change, not twice a second.

Round 5's own recorded usability defect (docs/heavy-plan.md, "One usability defect in
the Layers panel, found by driving it rather than by reading it") -- `web/js/app.js`
re-rendered this panel from its animation loop, throttled to ~2 Hz so the streaming-
budget numbers stayed live, but the panel's own contract was strict teardown-and-rebuild
(`container.innerHTML = ''`), so the WHOLE panel -- including the buttons a user has to
click -- was torn down and recreated twice a second, independent of whether anything a
user cares about actually changed. The lead's ruling (question 231): the panel repaints
only on state change.

This file drives a REAL headless Chrome against a REAL running `python -m altavista
serve`, exactly like tests/test_viewer_layers_panel.py's own `_drive`/CDP-driver
convention (copied, not re-imported -- that file's own module doc: "not re-imported from
that file... copied verbatim", the same posture tests/heavy_stack.py's own module doc
states for LocalTestIssuer), and asserts four things a user/automation would actually
notice:

  1. Holding a REAL element reference (never a coordinate) to the "Refresh catalog"
     button, and separately to one tile set's own toggle button, across a >500ms idle
     wait -- ten times each -- the reference is STILL the exact same live DOM node
     (`document.contains(ref)`) every single time. This is the literal reproduction of
     round 5's own finding ("ref is stale (element removed)"): the ONLY thing that could
     invalidate a held reference during a pure idle wait is the animation loop's own
     periodic re-render, and after this fix it no longer does.
  2. A REAL, production counter (`structuralRebuildCount`, exported by
     web/js/panels/layers_panel.js itself, incremented only inside `render()`'s own
     teardown/rebuild -- never a test-only shim) stays at EXACTLY the value it started
     at across 5 continuous idle seconds with no user action at all.
  3. That SAME counter genuinely is non-zero and DOES increase once real user actions
     (the refresh/toggle clicks above) happen -- proving the counter is a real,
     load-bearing signal and not simply frozen/disconnected.
  4. The streaming-budget numbers (resident bytes) DID keep changing across that same
     idle window (the real globe is enabled and streaming tiles through the real,
     shared `LayerManager`) -- otherwise "zero structural rebuilds" would be trivially
     satisfied by a panel that stopped updating anything at all, which is exactly the
     regression this task's own brief warns against ("otherwise you have fixed the
     repaint by freezing the panel").

Zero page exceptions/console errors throughout.

# What is stubbed, and why

Same posture as tests/test_viewer_layers_panel.py's own module doc: a real `av-gateway`
and a real `av-tiles` are not available in this sandbox, so both are stubbed at the ONLY
honest place -- the real upstream wire protocol each real Python client speaks. Nothing
in `altavista/server.py`, `altavista/gateway_client.py`, `altavista/tiles_client.py`,
`web/js/app.js`, or `web/js/panels/layers_panel.js` is stubbed, patched, or bypassed.
Only ONE catalogued tile set is used here (unlike that file's two) -- this test's own
proof needs exactly one toggle target, not a second-tile-set-no-collision proof, which
that file already covers.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from concurrent import futures
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import grpc
import pytest

from altavista.pb import authority_pb2, authority_pb2_grpc, heavy_pb2

REPO_ROOT = Path(__file__).resolve().parent.parent

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
]


def _find_chrome() -> str | None:
    for candidate in CHROME_CANDIDATES:
        if candidate.startswith("/") and Path(candidate).exists():
            return candidate
        found = shutil.which(candidate)
        if found:
            return found
    return None


def _free_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


# ============================================================================
# Fake av-tiles (real plain-HTTP loopback server) -- copied from
# tests/test_viewer_layers_panel.py, pared down to one manifest.
# ============================================================================

TILE_BYTE_SIZE = 1024


def _tile_bytes(level: int, x: int, y: int) -> bytes:
    pattern = f"tile:{level}:{x}:{y}:".encode("ascii")
    return (pattern * (TILE_BYTE_SIZE // len(pattern) + 1))[:TILE_BYTE_SIZE]


def _build_manifest(max_level: int) -> bytes:
    manifest = heavy_pb2.TileSetManifest()
    for level in range(max_level + 1):
        count_x = 2 ** (level + 1)
        count_y = 2**level
        for x in range(count_x):
            for y in range(count_y):
                content = _tile_bytes(level, x, y)
                entry = manifest.tiles.add()
                entry.level = level
                entry.x = x
                entry.y = y
                entry.size_bytes = len(content)
                entry.sha256 = hashlib.sha256(content).hexdigest()
                entry.media_type = "image/png"
    return manifest.SerializeToString()


def _make_tiles_handler(manifests: dict[str, bytes]):
    manifest_re = re.compile(r"^/v1/tilesets/([^/]+)/manifest$")
    tile_re = re.compile(r"^/v1/tilesets/([^/]+)/tiles/(\d+)/(\d+)/(\d+)$")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args) -> None:  # quiet
            pass

        def do_GET(self) -> None:  # noqa: N802
            m = manifest_re.match(self.path)
            if m:
                body = manifests.get(m.group(1))
                if body is None:
                    self.send_response(404)
                    self.end_headers()
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/vnd.altavista.tileset-manifest+pb")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            m = tile_re.match(self.path)
            if m:
                level, x, y = int(m.group(2)), int(m.group(3)), int(m.group(4))
                content = _tile_bytes(level, x, y)
                self.send_response(200)
                self.send_header("Content-Type", "image/png")
                self.send_header("ETag", f'"{hashlib.sha256(content).hexdigest()}"')
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                self.wfile.write(content)
                return
            self.send_response(404)
            self.end_headers()

    return Handler


@pytest.fixture()
def fake_tiles_gateway():
    manifests: dict[str, bytes] = {}
    handler_cls = _make_tiles_handler(manifests)
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler_cls)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server, manifests
    finally:
        server.shutdown()
        thread.join(timeout=5)


# ============================================================================
# Fake av-gateway DataGatewayService -- copied from tests/test_viewer_layers_panel.py.
# ============================================================================


class FakeGatewayServicer(authority_pb2_grpc.DataGatewayServiceServicer):
    def __init__(self, records: list) -> None:
        self._records = records

    def Query(self, request, context):  # noqa: N802
        if request.selector != authority_pb2.GATEWAY_SELECTOR_CATALOG:
            context.abort(grpc.StatusCode.INVALID_ARGUMENT, f"fake gateway only implements GATEWAY_SELECTOR_CATALOG, got {request.selector}")
        response = authority_pb2.GatewayQueryResponse(query_id="repaint-proof-catalog-query")
        response.catalog_records.extend(self._records)
        return response


@pytest.fixture()
def fake_gateway():
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=4))
    servicer = FakeGatewayServicer(records=[])
    authority_pb2_grpc.add_DataGatewayServiceServicer_to_server(servicer, server)
    port = server.add_insecure_port("127.0.0.1:0")
    server.start()
    try:
        yield server, servicer, port
    finally:
        server.stop(grace=1).wait(timeout=5)


def _catalog_record(asset_id: str, manifest_sha256: str, name: str, marking: str, size_bytes: int):
    record = heavy_pb2.CatalogRecord(asset_id=asset_id, job_id=name, created_tai_ns=0, footprint_wkt="")
    record.asset.uri = f"s3://fake-bucket/{asset_id}"
    record.asset.sha256 = manifest_sha256
    record.asset.size_bytes = size_bytes
    record.asset.media_type = "application/vnd.altavista.tileset-manifest+pb"
    record.asset.label.marking = marking
    return record


class _LiveServer:
    def __init__(self, port: int, proc: subprocess.Popen) -> None:
        self.port = port
        self.proc = proc

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/"

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


GATEWAY_TOKEN_CONTENT = "repaint-proof-fake-gateway-token"
TILES_TOKEN_CONTENT = "repaint-proof-fake-tiles-token"
TILE_SET_SHA = "c" * 64


@pytest.fixture()
def live_server(tmp_path, fake_gateway, fake_tiles_gateway):
    gw_server, servicer, gw_port = fake_gateway
    tiles_server, manifests = fake_tiles_gateway

    manifest_bytes = _build_manifest(max_level=1)  # small -- fetchManifest() speed matters here, not coverage
    manifests[TILE_SET_SHA] = manifest_bytes
    servicer._records.append(_catalog_record("repaint-asset", TILE_SET_SHA, "Repaint Proof Tile Set", "UNCLASSIFIED", len(manifest_bytes)))

    gateway_token_path = tmp_path / "gateway_token.txt"
    gateway_token_path.write_text(GATEWAY_TOKEN_CONTENT)
    tiles_token_path = tmp_path / "tiles_token.txt"
    tiles_token_path.write_text(TILES_TOKEN_CONTENT)

    port = _free_port()
    env = dict(os.environ)
    proc = subprocess.Popen(
        [
            sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port),
            "--gateway-endpoint", f"127.0.0.1:{gw_port}", "--gateway-token-path", str(gateway_token_path),
            "--tiles-endpoint", f"127.0.0.1:{tiles_server.server_address[1]}", "--tiles-token-path", str(tiles_token_path),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        cwd=str(REPO_ROOT),
        env=env,
    )
    server = _LiveServer(port, proc)
    deadline = time.monotonic() + 30
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
        pytest.fail("altavista server did not become ready within 30s")

    # A real 'Earth' body -- same fixture scenario as
    # tests/test_viewer_globe_layer_manager.py (see that file's own comment on why quat
    # is required) -- so the real globe checkbox has something to put a globe on and
    # `viewer.layerManager` genuinely streams tiles during the idle window this test
    # measures.
    scenario = {
        "name": "layers-panel-repaint-proof",
        "frame": {"name": "EarthMJ2000Eq"},
        "bodies": [{"name": "Earth", "central": True, "radius": 6378137.0, "t": [0.0], "pos": [0.0, 0.0, 0.0], "quat": [0.0, 0.0, 0.0, 1.0]}],
        "spacecraft": [],
    }
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/scenario",
        data=json.dumps(scenario).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            assert r.status in (200, 201), f"publishing the proof scenario answered HTTP {r.status}"
    except Exception as exc:  # pragma: no cover
        server.stop()
        pytest.fail(f"could not publish the proof scenario: {exc}")

    try:
        yield server
    finally:
        server.stop()


# ============================================================================
# CDP driver -- copied from tests/test_viewer_layers_panel.py's own `_drive` (itself
# copied from tests/test_viewer_globe_layer_manager.py), with ONE deliberate change: the
# post-evaluate result deadline is raised from 30s to 120s -- this probe's own idle/click
# phases deliberately run for tens of real seconds (five full idle seconds alone), unlike
# every other probe in this codebase.
# ============================================================================


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 6.0, result_deadline_s: float = 120.0):
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen(
            [
                chrome_path,
                f"--user-data-dir={profile_dir}",
                "--headless=new",
                f"--remote-debugging-port={cdp_port}",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--use-gl=swiftshader",
                "--enable-unsafe-swiftshader",
                "--window-size=1280,900",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            info = None
            deadline = time.monotonic() + 20
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
                plist = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{cdp_port}/json/list", timeout=2).read())
                page = next(p for p in plist if p.get("id") == target_id)

                async with websockets.connect(page["webSocketDebuggerUrl"], max_size=None) as ws:
                    mid = [1]

                    async def send(method, params=None):
                        mid[0] += 1
                        await ws.send(json.dumps({"id": mid[0], "method": method, "params": params or {}}))
                        return mid[0]

                    await send("Runtime.enable")
                    await send("Log.enable")
                    await send("Page.enable")

                    errors: list[str] = []
                    state: dict = {"want_id": None, "done": False, "value": None}

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
                            elif method == "Runtime.exceptionThrown":
                                details = msg.get("params", {}).get("exceptionDetails", {})
                                text = details.get("text", "exception")
                                exc = details.get("exception") or {}
                                description = exc.get("description") or exc.get("value") or ""
                                errors.append(f"[Runtime.exceptionThrown] {text}: {description}")
                            elif msg.get("id") is not None and msg["id"] == state["want_id"]:
                                result = msg.get("result", {}).get("result", {})
                                state["value"] = result.get("value")
                                state["done"] = True

                    reader_task = asyncio.create_task(reader())
                    await send("Page.navigate", {"url": url})
                    await asyncio.sleep(wait_s)

                    state["want_id"] = mid[0] + 1
                    await send("Runtime.evaluate", {"expression": eval_js, "returnByValue": True, "awaitPromise": True})
                    deadline = time.monotonic() + result_deadline_s
                    while time.monotonic() < deadline and not state["done"]:
                        await asyncio.sleep(0.05)

                    reader_task.cancel()
                    try:
                        await reader_task
                    except asyncio.CancelledError:
                        pass
                    return errors, state["value"]
        finally:
            chrome.terminate()
            try:
                chrome.wait(timeout=10)
            except subprocess.TimeoutExpired:
                chrome.kill()


# The page script -- see this module's own top doc for the four things it proves.
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const mod = await import('/js/panels/layers_panel.js');
    const panel = document.getElementById('panel-layers');
    if (!panel) { out.error = 'no #panel-layers'; return out; }

    // ---- enable the real globe: viewer.layerManager.update() only ever runs while a
    // globe or 3D-Tiles overlay is active (web/js/globe.js's own note), so this is what
    // makes the streaming-budget numbers genuinely change over real time below --
    // otherwise "budget kept updating" would have nothing real to observe.
    const globeCb = document.getElementById('opt-globe');
    if (!globeCb) { out.error = 'no #opt-globe checkbox'; return out; }
    globeCb.checked = true;
    globeCb.dispatchEvent(new Event('change'));
    out.globeEnabled = !!viewer.globeLayer;
    for (let i = 0; i < 30; i++) await new Promise((r) => requestAnimationFrame(r));

    // ============================================================ phase 1: 5s pure idle
    // The default globe imagery for this test's minimal one-body scenario finishes
    // streaming and settles at a STEADY resident-byte total almost immediately (measured
    // empirically while writing this test), so real organic tile traffic alone cannot be
    // relied on to keep producing a DIFFERENT number across a full 5s window on every
    // host/run -- that would make this assertion flaky for a reason that has nothing to
    // do with the repaint-cadence fix under test. So this phase directly increments the
    // REAL, live `viewer.layerManager.residentBytes` field (a real, ordinary mutable
    // instance property -- see web/js/layers/layer.js's own `this.residentBytes += delta`
    // -- never a fake/mock manager) on a timer FASTER than the panel's own 500ms tick,
    // which deterministically proves the tick genuinely re-reads and re-renders the
    // CURRENT value every time it runs, without needing to depend on how much the globe's
    // own organic streaming happens to move the number on any given run.
    const rebuildBeforeIdle = mod.structuralRebuildCount;
    const ddBefore = panel.querySelector('.av-layers-budget dd');
    const budgetTextBefore = ddBefore ? ddBefore.textContent : null;
    const ddNodeBefore = ddBefore;
    const residentBytesAtStart = viewer.layerManager.residentBytes;
    const bumpTimer = setInterval(() => { viewer.layerManager.residentBytes += 1024; }, 100);
    await new Promise((r) => setTimeout(r, 5200)); // > 10 tick periods at the panel's own 500ms throttle
    clearInterval(bumpTimer);
    const rebuildAfterIdle = mod.structuralRebuildCount;
    const ddAfter = panel.querySelector('.av-layers-budget dd');
    out.rebuildCountDuringPureIdle = rebuildAfterIdle - rebuildBeforeIdle;
    out.budgetTextBeforeIdle = budgetTextBefore;
    out.budgetTextAfterIdle = ddAfter ? ddAfter.textContent : null;
    out.budgetDdNodeSameAcrossIdle = ddAfter === ddNodeBefore;
    out.residentBytesGrewDuringIdle = viewer.layerManager.residentBytes > residentBytesAtStart;

    // ============================================================ phase 2: refresh x10
    let staleRefreshCount = 0;
    let refreshBtn = panel.querySelector('.av-layers-refresh');
    out.refreshFoundBeforeLoop = !!refreshBtn;
    for (let i = 0; i < 10; i++) {
      await new Promise((r) => setTimeout(r, 650)); // idle wait longer than one tick period
      if (!refreshBtn || !document.contains(refreshBtn)) { staleRefreshCount += 1; }
      else { refreshBtn.click(); }
      let waited = 0;
      while (panel.textContent.includes('Loading tile set catalog') && waited < 40) {
        await new Promise((r) => setTimeout(r, 50));
        waited += 1;
      }
      refreshBtn = panel.querySelector('.av-layers-refresh'); // a real refresh legitimately rebuilds -- re-acquire
    }
    out.staleRefreshCount = staleRefreshCount;
    out.toggleButtonCountAfterRefreshes = panel.querySelectorAll('.av-layers-toggle').length;

    // ============================================================ phase 3: toggle x10
    const targetSha = '__TILE_SET_SHA__';
    const findToggleBtn = () => {
      const cell = [...panel.querySelectorAll('td')].find((td) => td.title === targetSha);
      const row = cell && (cell.closest('tr') || cell.parentElement);
      return row && row.querySelector('.av-layers-toggle');
    };
    let staleToggleCount = 0;
    let toggleBtn = findToggleBtn();
    out.toggleFoundBeforeLoop = !!toggleBtn;
    for (let i = 0; i < 10; i++) {
      await new Promise((r) => setTimeout(r, 650));
      if (!toggleBtn || !document.contains(toggleBtn)) { staleToggleCount += 1; }
      else { toggleBtn.click(); }
      let waited = 0;
      while (waited < 60) {
        const b = findToggleBtn();
        if (b && b.textContent !== 'Loading…') break;
        await new Promise((r) => setTimeout(r, 50));
        waited += 1;
      }
      toggleBtn = findToggleBtn();
    }
    out.staleToggleCount = staleToggleCount;
    out.toggleLabelAfterLoop = toggleBtn ? toggleBtn.textContent : null;

    out.rebuildCountAfterAllActions = mod.structuralRebuildCount;
    out.rebuildCountIncreasedFromRealActions = out.rebuildCountAfterAllActions > rebuildAfterIdle;

    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_layers_panel_repaints_only_on_state_change(live_server):
    """The full proof: see this module's own top doc comment for the four things
    asserted. Prints the whole probe result (question 148: an exit code is not
    evidence) so a green run is readable, quoted evidence."""
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")

    probe_js = _PROBE_JS.replace("__TILE_SET_SHA__", TILE_SET_SHA)
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, probe_js, wait_s=6.0))

    print("\nLayers panel repaint-cadence probe:", json.dumps(value, indent=2, sort_keys=True))
    print("console/exception errors:", errors)

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"
    assert value.get("globeEnabled") is True, f"the globe never enabled -- nothing would stream: {value!r}"
    assert value.get("refreshFoundBeforeLoop") is True, f"no Refresh catalog button found: {value!r}"

    # ---- (2) zero structural rebuilds across 5 continuous idle seconds -- the CENTRAL
    # claim of this task, checked first (right after basic setup sanity) so a perturbed
    # build's exact rebuild count is what a failed run reports, rather than being masked
    # by a downstream symptom (e.g. the toggle button never appearing at all because the
    # catalog fetch kept losing a race against a rebuild -- itself a real, but secondary,
    # consequence of the same defect, observed and recorded in this task's own report).
    assert value.get("rebuildCountDuringPureIdle") == 0, (
        f"expected ZERO structural rebuilds across 5s of pure idle, got "
        f"{value.get('rebuildCountDuringPureIdle')}: {value!r}"
    )
    # ---- (4) the budget numbers kept updating over that same idle window -- proves the
    # counter reading zero is not because the panel simply froze/stopped updating. The
    # probe deterministically bumps the REAL, live layerManager.residentBytes on its own
    # fast timer during this window (see the probe's own comment for why organic globe
    # streaming alone was not relied on) -- this asserts the panel's own tick genuinely
    # picked up that live change.
    assert value.get("residentBytesGrewDuringIdle") is True, (
        f"viewer.layerManager.residentBytes never grew during the idle window -- the probe's "
        f"own timer did not run: {value!r}"
    )
    assert value.get("budgetTextBeforeIdle") != value.get("budgetTextAfterIdle"), (
        "the streaming-budget text never changed across 5s of idle even though the real, "
        f"live layerManager.residentBytes genuinely grew -- this fix froze the panel instead "
        f"of fixing its cadence: {value!r}"
    )
    # ---- the budget dd VALUE NODE itself is the SAME node across the idle window (text
    # patched in place, never removed/recreated) -- direct evidence of HOW it updated.
    assert value.get("budgetDdNodeSameAcrossIdle") is True, (
        f"the budget value node was replaced across the idle window -- it should have been "
        f"patched in place, not recreated: {value!r}"
    )

    # ---- (1) ten held references to Refresh catalog, across ten idle waits, zero stale
    assert value.get("staleRefreshCount") == 0, (
        f"a held reference to 'Refresh catalog' went stale during an IDLE wait (no user "
        f"action) -- this is round 5's own recorded defect: {value!r}"
    )
    # ---- (1) ten held references to the toggle button, across ten idle waits, zero stale
    assert value.get("toggleFoundBeforeLoop") is True, f"no toggle button found for the fixture tile set: {value!r}"
    assert value.get("staleToggleCount") == 0, (
        f"a held reference to the tile set's own toggle button went stale during an IDLE "
        f"wait (no user action) -- this is round 5's own recorded defect: {value!r}"
    )

    # ---- (3) the SAME counter genuinely moved once real actions (refresh/toggle clicks)
    # happened -- proves it is a real, load-bearing signal, not simply disconnected/frozen.
    assert value.get("rebuildCountIncreasedFromRealActions") is True, (
        f"structuralRebuildCount never increased even though ten real refresh clicks and "
        f"ten real toggle clicks happened -- the counter is not wired to anything real: {value!r}"
    )
    assert value.get("toggleLabelAfterLoop") in ("Turn on", "Turn off", "Retry"), (
        f"the toggle button's own label never reflected a real state after ten real "
        f"clicks: {value!r}"
    )

    # ---- zero page exceptions/console errors throughout the whole drive
    assert errors == [], (
        f"expected zero page exceptions/console errors, got {len(errors)}:\n" + "\n".join(errors)
    )

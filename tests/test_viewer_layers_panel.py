"""Question 228 finding 2, browser half: the proof that a user can select a catalogued
tile set from the Layers panel (web/js/panels/layers_panel.js) and see its tiles
requested through the ONE shared `LayerManager`, with zero page exceptions.

Round 4 delivered the globe/`LayerManager` wiring. A previous worker this round
delivered the server half: `GET /api/catalog/tilesets` (`altavista/server.py`, backed by
`altavista/gateway_client.py`). This file drives the browser half these two rounds set
up for -- a REAL headless Chrome against a REAL running `python -m altavista serve`,
clicking the Layers panel's own real "Refresh catalog" and "Turn on" buttons (never
reaching past the panel into `GatewayImageryLayerAdapter` directly), and asserts from
observable network/scene-graph facts, never from a counter alone.

# What is stubbed, and why (this task's own brief: "stub it at the server boundary
# honestly ... do not stub the browser's fetch")

Two real backend services this route/adapter chain depends on are not available in this
sandbox: a real `av-gateway` (needs a real PostGIS-backed catalog tier) and a real
`av-tiles` (this task's own budget explicitly rules out building the Rust binaries or
touching Docker -- the host-wide docker-test lock is held by another team). Both are
stubbed at the ONLY place that is honest: the real upstream wire protocol each real
Python client (`altavista.gateway_client`/`altavista.tiles_client`, NEITHER modified by
this task) actually speaks --
  - `FakeGatewayServicer` below is a real `grpc.server` implementing
    `altavista.v1.DataGatewayService.Query` (the exact generated
    `authority_pb2_grpc.DataGatewayServiceServicer`), answering `GatewayQueryRequest`
    with real `heavy_pb2.CatalogRecord`s -- the REAL, unmodified `gateway_client.
    list_tile_sets` dials this over a real (loopback) gRPC channel exactly as it would
    dial a real `av-gateway`.
  - `_TilesHTTPHandler` below is a real (loopback) plain-HTTP server implementing
    `av-tiles`' own two GET routes (`/v1/tilesets/<sha>/manifest`,
    `/v1/tilesets/<sha>/tiles/<level>/<x>/<y>`) -- the REAL, unmodified
    `tiles_client.proxy_get` dials this exactly as it would dial a real `av-tiles`.
Nothing in `altavista/server.py`, `altavista/gateway_client.py`, `altavista/
tiles_client.py`, `web/js/app.js`, `web/js/panels/layers_panel.js`, or `web/js/layers/
gateway_imagery_layer.js` is stubbed, patched, or bypassed -- every one of those runs
for real, in a real subprocess / real browser, over real (loopback) sockets.

# What this test independently verifies, not merely reads back

`servicer.captured_tokens` and `TILE_REQUEST_LOG` (below) are populated by the FAKE
UPSTREAM servers themselves, at the point a real request actually arrives over the
network -- this is what makes "the server attaches its own token, read fresh per call"
and "the browser's toggle genuinely requested this tile set's bytes" real, observable
facts from a vantage point outside the code under test, not a value the code under test
handed back about itself.
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
# The fake av-tiles gateway -- a real plain-HTTP server (av-tiles' own transport,
# altavista/tiles_client.py's own module doc: "host:port of a running av-tiles
# gateway"), serving a REAL TileSetManifest (heavy_pb2, the same message
# web/js/layers/tileset_manifest.js's own hand-rolled decoder reads field 7 of) and
# real tile bytes with a correct ETag (the real gateway's own sha256-hex convention --
# tests/test_viewer_tiles_route.py's own byte-for-byte proof, not repeated here since
# that test cannot run in this sandbox, but GatewayImageryLayerAdapter.load()
# recomputes and checks this hash for real regardless of who served it).
# ============================================================================

TILE_BYTE_SIZE = 1024


def _tile_bytes(level: int, x: int, y: int) -> bytes:
    pattern = f"tile:{level}:{x}:{y}:".encode("ascii")
    return (pattern * (TILE_BYTE_SIZE // len(pattern) + 1))[:TILE_BYTE_SIZE]


def _build_manifest(max_level: int) -> bytes:
    """Every tile a default `enableGlobe()` (`maxLevel` defaults to 2, web/js/globe.js)
    could ever select, at TILE_BYTE_SIZE bytes each -- generous coverage so whichever
    tiles the real screen-space-error selection actually picks for this test's real
    camera position are always present in the manifest (never a manifest/selection
    mismatch that would silently fall back to the estimate and prove nothing about the
    manifest path)."""
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


TILE_REQUEST_LOG: list[tuple[str, str]] = []  # (manifest_sha256, "level/x/y") -- appended by the handler thread
_TILE_REQUEST_LOG_LOCK = threading.Lock()


def _make_tiles_handler(manifests: dict[str, bytes]):
    manifest_re = re.compile(r"^/v1/tilesets/([^/]+)/manifest$")
    tile_re = re.compile(r"^/v1/tilesets/([^/]+)/tiles/(\d+)/(\d+)/(\d+)$")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args) -> None:  # quiet -- pytest -s already shows what matters
            pass

        def do_GET(self) -> None:  # noqa: N802 - stdlib method name
            m = manifest_re.match(self.path)
            if m:
                sha = m.group(1)
                body = manifests.get(sha)
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
                sha, level, x, y = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4))
                with _TILE_REQUEST_LOG_LOCK:
                    TILE_REQUEST_LOG.append((sha, f"{level}/{x}/{y}"))
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
# The fake av-gateway DataGatewayService -- a real grpc.server implementing the real
# generated servicer (altavista/pb/altavista/v1/authority_pb2_grpc.py), answering the
# REAL GatewayQueryRequest the REAL, unmodified altavista.gateway_client.list_tile_sets
# builds. Captures every caller_token it actually receives (populated from the network
# boundary, not read back from the code under test) so this test can independently
# confirm "the server attaches its own token, read fresh per call" (gateway_client.py's
# own module doc) really happened for THIS route.
# ============================================================================


class FakeGatewayServicer(authority_pb2_grpc.DataGatewayServiceServicer):
    def __init__(self, records: list) -> None:
        self._records = records
        self.captured_tokens: list[str] = []
        self.captured_selectors: list[int] = []

    def Query(self, request, context):  # noqa: N802 - grpc's own generated method name
        self.captured_tokens.append(request.caller_token)
        self.captured_selectors.append(request.selector)
        if request.selector != authority_pb2.GATEWAY_SELECTOR_CATALOG:
            context.abort(grpc.StatusCode.INVALID_ARGUMENT, f"fake gateway only implements GATEWAY_SELECTOR_CATALOG, got {request.selector}")
        response = authority_pb2.GatewayQueryResponse(query_id="fake-catalog-query-1")
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


# ============================================================================
# The real viewer server, pointed at both fakes above through the REAL, unmodified
# --gateway-endpoint/--gateway-token-path/--tiles-endpoint/--tiles-token-path CLI flags
# (altavista/__main__.py). Question 199: the child's environment is a COPY passed as
# env=; this fixture never assigns to os.environ.
# ============================================================================


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


GATEWAY_TOKEN_CONTENT = "fake-gateway-bearer-token-for-this-test-only"
TILES_TOKEN_CONTENT = "fake-tiles-bearer-token-for-this-test-only"

TILE_SET_A_SHA = "a" * 64  # a syntactically valid-looking sha256 hex string, this test's own fixture identity
TILE_SET_B_SHA = "b" * 64


@pytest.fixture()
def live_server(tmp_path, fake_gateway, fake_tiles_gateway):
    gw_server, servicer, gw_port = fake_gateway
    tiles_server, manifests = fake_tiles_gateway

    manifest_bytes = _build_manifest(max_level=2)
    manifests[TILE_SET_A_SHA] = manifest_bytes
    manifests[TILE_SET_B_SHA] = manifest_bytes
    servicer._records.extend([
        _catalog_record("asset-a", TILE_SET_A_SHA, "Fixture Tile Set A", "UNCLASSIFIED", len(manifest_bytes)),
        _catalog_record("asset-b", TILE_SET_B_SHA, "Fixture Tile Set B", "CUI", len(manifest_bytes)),
    ])

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

    # A real 'Earth' body, exactly like tests/test_viewer_globe_layer_manager.py's own
    # fixture scenario (see that file's own comment on why quat is required).
    scenario = {
        "name": "layers-panel-proof",
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
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the proof scenario: {exc}")

    try:
        yield server
    finally:
        server.stop()


# ============================================================================
# CDP driver -- copied from tests/test_viewer_globe_layer_manager.py's own `_drive`
# (that file's module doc: "the lead's browser drive"), same three-channel exception
# collector (question 211's own binding rule: Runtime.exceptionThrown is the ONLY
# channel an uncaught exception reaches). Not re-imported from that file (pytest test
# modules are not meant to import each other's internals) -- copied verbatim, the same
# "infra worth copying, not re-deriving" posture tests/heavy_stack.py's own module doc
# states for LocalTestIssuer.
# ============================================================================


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 6.0):
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
                    deadline = time.monotonic() + 30
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


# The page script: drive the REAL page exactly as a user would --
#   1. click the REAL "Tiled globe (Earth)" checkbox (#opt-globe) so the shared
#      LayerManager actually ticks every frame (web/js/scene.js's update() only calls
#      layerManager.update() while a globe or 3D-Tiles overlay is active -- see
#      web/js/globe.js's own note and this task's own report for why a THIRD,
#      independent driver must never be added instead);
#   2. click the Layers panel's own real "Refresh catalog" button (never fetch
#      /api/catalog/tilesets directly from this probe -- that would prove nothing about
#      app.js's own wiring);
#   3. click that same panel's own real "Turn on" button for the fixture tile set --
#      exercising layers_panel.js's render()/onToggleLayer through app.js's real
#      toggleGatewayLayer, never constructing a GatewayImageryLayerAdapter directly from
#      this probe;
#   4. pump real animation frames (the scenario reached the page over the REAL
#      WebSocket the live_server fixture's POST /api/scenario triggered, so app.js's own
#      module-scoped `scenario` is set for real and its own requestAnimationFrame loop
#      drives viewer.update() -- unlike test_viewer_globe_layer_manager.py's probe,
#      nothing here bypasses app.js's private state);
#   5. read the answer off the scene graph (a real texture bound to a globe tile mesh --
#      the SAME assertion test_viewer_globe_layer_manager.py already makes, proving the
#      Layers panel's new layer coexists on the shared manager without regressing the
#      globe's own default imagery) AND off the LayerManager's own counters for the
#      gateway layer specifically (countsByLayer, residentBytes, byteCostSource via a
#      fresh, independent plan() call against the real, currently-registered adapter).
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const { layerIdForManifest } = await import('/js/panels/layers_panel.js');

    // ---- 1. the real globe checkbox (a real user click, real 'change' listener)
    const globeCb = document.getElementById('opt-globe');
    if (!globeCb) { out.error = 'no #opt-globe checkbox in the page'; return out; }
    globeCb.checked = true;
    globeCb.dispatchEvent(new Event('change'));
    out.globeEnabledAfterClick = !!viewer.globeLayer;

    // ---- 2. the real "Refresh catalog" button inside the (chooser-reachable, not
    // necessarily currently-visible-in-a-pane) Layers panel content element.
    const panel = document.getElementById('panel-layers');
    if (!panel) { out.error = 'no #panel-layers element in the page'; return out; }
    const refreshBtn = panel.querySelector('.av-layers-refresh');
    if (!refreshBtn) { out.error = 'no .av-layers-refresh button rendered'; return out; }
    refreshBtn.click();
    // The fetch this triggers is async; poll for the table to actually appear.
    let waited = 0;
    while (!panel.querySelector('.av-layers-toggle') && waited < 100) {
      await new Promise((r) => setTimeout(r, 50));
      waited += 1;
    }
    out.catalogTextAfterRefresh = panel.textContent.slice(0, 400);
    const toggleButtons = [...panel.querySelectorAll('.av-layers-toggle')];
    out.toggleButtonCount = toggleButtons.length;
    if (toggleButtons.length === 0) { out.error = 'catalog fetch never populated any toggle button'; return out; }

    // ---- 3. the real "Turn on" button for the fixture tile set this test built
    // (found by its manifest sha, preserved verbatim in the row's own title attribute
    // -- layers_panel.js's own shortSha()/title convention, never guessed by position).
    const targetSha = '__TILE_SET_A_SHA__';
    const shaCell = [...panel.querySelectorAll('td')].find((td) => td.title === targetSha);
    if (!shaCell) { out.error = `no row found for manifest sha ${targetSha}`; out.rowTitles = [...panel.querySelectorAll('td')].map((td) => td.title); return out; }
    const row = shaCell.closest('tr') || shaCell.parentElement;
    const toggleBtn = row.querySelector('.av-layers-toggle');
    if (!toggleBtn) { out.error = 'target row has no toggle button'; return out; }
    toggleBtn.click();

    // ---- 4. pump real animation frames -- app.js's own real per-frame loop (the
    // scenario arrived over the real WebSocket before this script ran, so
    // app.js's private `scenario` is already set for real).
    for (let i = 0; i < 240; i++) {
      await new Promise((r) => requestAnimationFrame(r));
    }

    // render() tears down and REBUILDS the panel's whole DOM subtree on every call
    // (the panel contract every panel in this codebase follows) -- the `row`/
    // `toggleBtn` nodes captured above are stale the instant a later render() runs
    // (e.g. the moment the toggle settles from 'loading' to 'on'), so the CURRENT
    // state is read back by re-querying the live DOM fresh, never from those stale
    // references.
    const freshShaCell = [...panel.querySelectorAll('td')].find((td) => td.title === targetSha);
    const freshRow = freshShaCell && (freshShaCell.closest('tr') || freshShaCell.parentElement);
    const freshToggleBtn = freshRow && freshRow.querySelector('.av-layers-toggle');
    out.toggleRowStatusText = freshRow ? freshRow.textContent : null;
    out.toggleButtonLabelAfter = freshToggleBtn ? freshToggleBtn.textContent : null;

    // ---- 5a. the globe's own default imagery still draws (no regression from the
    // new layer coexisting on the shared manager) -- byte-for-byte the same scene-graph
    // walk test_viewer_globe_layer_manager.py's own probe makes.
    const group = viewer.globeLayer && viewer.globeLayer.group;
    let meshes = 0, withTexture = 0;
    if (group) {
      group.traverse((o) => {
        if (!o.isMesh) return;
        meshes += 1;
        const map = o.material && o.material.map;
        const img = map && map.image;
        if (map && img && (img.width || img.naturalWidth) && (img.height || img.naturalHeight)) withTexture += 1;
      });
    }
    out.meshCount = meshes;
    out.meshesWithBoundTexture = withTexture;

    // ---- 5b. the gateway layer's own counters, and an INDEPENDENT fresh plan() call
    // (never trusting the manager's residentBytes total alone).
    const lm = viewer.layerManager;
    const gatewayLayerId = layerIdForManifest(targetSha);
    out.registeredLayers = [...lm._layers.keys()];
    out.gatewayLayerRegistered = out.registeredLayers.includes(gatewayLayerId);
    out.countsByLayer = lm.countsByLayer();
    out.residentBytes = lm.residentBytes;
    out.memoryBudgetBytes = lm.memoryBudgetBytes;
    out.withinBudget = lm.residentBytes <= lm.memoryBudgetBytes;
    out.softViolationCount = lm.softViolationCount;
    out.deferredCount = lm.deferredCount;
    out.failedCount = lm.failedCount;
    out.failureNames = lm.failureNames();
    const gatewayLayer = lm._layers.get(gatewayLayerId);
    if (gatewayLayer) {
      out.gatewayManifestLoaded = gatewayLayer.manifestLoaded;
      // A fresh, independent plan() call, straight from the real registered adapter,
      // over the REAL tiles the globe is showing right now (globe.js's own
      // `mesh.userData.tile`, the exact tile object buildTileMesh() attached) -- never
      // read back from a cache this test itself populated, and never a synthetic tile
      // list that could happen to dodge the real manifest coverage.
      const realTilesNow = group ? group.children.filter((m) => m.userData && m.userData.tile).map((m) => m.userData.tile) : [];
      const freshPlan = gatewayLayer.plan({
        tiles: realTilesNow, cameraEcef: { x: 20000000, y: 0, z: 0 }, screenHeightPx: 900, fovYRad: 0.87,
      });
      out.freshPlanTileCount = freshPlan.length;
      out.byteCostSourcesOnFreshPlan = [...new Set(freshPlan.map((r) => r.byteCostSource))];
    }
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_selecting_a_catalogued_tile_set_from_the_layers_panel_requests_its_tiles(live_server, fake_gateway, fake_tiles_gateway):
    """Selects the Layers panel's own real controls, exactly as a user would, and
    asserts from three independent vantage points that the real path ran end to end:
    (1) the scene graph (the globe's own imagery still draws -- no regression), (2) the
    real LayerManager's own counters for the gateway layer specifically, and (3) the
    FAKE av-gateway/av-tiles servers' own captured requests (never read back from the
    browser's self-report alone)."""
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")

    probe_js = _PROBE_JS.replace("__TILE_SET_A_SHA__", TILE_SET_A_SHA)
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, probe_js, wait_s=6.0))

    print("\nLayers panel probe:", json.dumps(value, indent=2, sort_keys=True))
    _, servicer, _ = fake_gateway
    print("fake gateway captured tokens:", servicer.captured_tokens)
    print("fake tiles gateway request log:", TILE_REQUEST_LOG)

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"

    # ---- the catalog route was really fetched and really rendered
    assert value.get("toggleButtonCount", 0) >= 2, f"expected two rows (two fixture tile sets), got: {value!r}"
    assert "not loaded yet" not in value.get("catalogTextAfterRefresh", ""), (
        f"the catalog still shows its pre-fetch state after clicking Refresh: {value!r}"
    )

    # ---- the toggle actually turned the layer on, through app.js's real callback path
    assert value.get("gatewayLayerRegistered") is True, (
        f"the gateway layer was never registered on viewer.layerManager -- the toggle did not "
        f"reach the manager at all: {value!r}"
    )
    assert value.get("toggleButtonLabelAfter") == "Turn off", (
        f"the toggle button never flipped to the 'on' label: {value!r}"
    )
    assert "active" in value.get("toggleRowStatusText", ""), f"the row's own status cell never showed 'active': {value!r}"

    # ---- fetchManifest() genuinely completed, and the manifest path (never the
    # fallback estimate) is what the live, registered adapter reports on a FRESH,
    # independently-triggered plan() call.
    assert value.get("gatewayManifestLoaded") is True, f"fetchManifest() never completed: {value!r}"
    assert value.get("freshPlanTileCount", 0) > 0, f"a fresh, independent plan() call over the globe's real current tiles produced nothing: {value!r}"
    assert value.get("byteCostSourcesOnFreshPlan") == ["manifest"], (
        f"a fresh plan() call, independent of anything this test cached, must tag every request "
        f"'manifest' (never the fallback estimate) once fetchManifest() has resolved: {value!r}"
    )

    # ---- resident bytes stay within budget, no soft violations, from the real manager
    assert value.get("withinBudget") is True, f"the manager's byte budget was exceeded: {value!r}"
    assert value.get("softViolationCount") == 0, f"soft budget violation: {value!r}"

    # ---- the globe's own default imagery is unaffected by the new layer sharing the manager
    assert value.get("meshCount", 0) > 0, f"the globe built no tile meshes at all: {value!r}"
    assert value.get("meshesWithBoundTexture", 0) > 0, f"no globe tile mesh has a texture bound: {value!r}"

    # ---- the fixture tile set's own tiles were genuinely requested -- verified at the
    # FAKE UPSTREAM server itself (network boundary), never from the browser's own
    # self-report alone.
    counts = value.get("countsByLayer") or {}
    gateway_counts = None
    for layer_id, c in counts.items():
        if layer_id.startswith("gateway-tileset:") and TILE_SET_A_SHA in layer_id:
            gateway_counts = c
    assert gateway_counts is not None, f"no per-layer counts for the toggled tile set: {value!r}"
    assert (gateway_counts.get("resident", 0) + gateway_counts.get("pending", 0)) > 0, (
        f"the manager never admitted or started a single request for the toggled tile set: {value!r}"
    )
    with _TILE_REQUEST_LOG_LOCK:
        requests_for_a = [r for r in TILE_REQUEST_LOG if r[0] == TILE_SET_A_SHA]
    assert requests_for_a, (
        f"the fake av-tiles gateway never received a single real tile GET for {TILE_SET_A_SHA}; "
        f"full request log: {TILE_REQUEST_LOG!r}"
    )

    # ---- the server attached its OWN token to the catalog query, read fresh, never one
    # the browser supplied (gateway_client.py's own contract) -- verified at the fake
    # gRPC servicer itself.
    assert GATEWAY_TOKEN_CONTENT in servicer.captured_tokens, (
        f"the catalog route never presented the server's own configured token to the gateway: "
        f"{servicer.captured_tokens!r}"
    )

    # ---- zero page exceptions/console errors throughout the whole drive
    assert errors == [], (
        f"expected zero page exceptions/console errors, got {len(errors)}:\n" + "\n".join(errors)
    )


def test_the_catalog_route_reports_not_configured_honestly_when_no_gateway_is_set(tmp_path):
    """The other half of this task's own brief: "if the route says the gateway is not
    configured, say exactly that -- 'not configured' must never render as 'no tile
    sets'". Drives a REAL `python -m altavista serve` with NEITHER --gateway-endpoint
    NOR --tiles-endpoint (the ordinary default every existing test server already uses),
    clicks the real Refresh button, and asserts the panel shows the server's OWN typed
    message, never the empty-catalog notice."""
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")

    port = _free_port()
    env = dict(os.environ)
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT), env=env,
    )
    server = _LiveServer(port, proc)
    try:
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
            pytest.fail("altavista server did not become ready within 30s")

        probe_js = r"""
        (async () => {
          const out = {};
          const panel = document.getElementById('panel-layers');
          const btn = panel.querySelector('.av-layers-refresh');
          btn.click();
          let waited = 0;
          while (panel.textContent.includes('Loading tile set catalog') && waited < 100) {
            await new Promise((r) => setTimeout(r, 50));
            waited += 1;
          }
          out.text = panel.textContent;
          return out;
        })()
        """
        errors, value = asyncio.run(_drive(server.url, chrome_path, probe_js, wait_s=4.0))
        assert value is not None, f"probe returned nothing; console: {errors}"
        text = value.get("text", "")
        assert "no data gateway is configured" in text, f"expected the server's own real message in the panel, got: {text!r}"
        assert "No tile sets in the catalog." not in text, (
            f"'not configured' rendered as 'no tile sets' -- exactly the regression this task's own "
            f"brief names. Panel text: {text!r}"
        )
        # A real user click against a real, unconfigured route inevitably produces ONE
        # real network-level log line -- Chrome's own DevTools "Failed to load
        # resource: ... 503" report for any fetch() that receives a non-2xx status,
        # confirmed empirically while writing this test (see this task's own report).
        # That is not a page exception and not a bug in this panel's own code -- it is
        # the unavoidable, honest cost of the "Refresh catalog" button being a REAL
        # fetch rather than something pre-cooked; catching it here proves the request
        # really happened. Zero OTHER errors (a JS exception, a console.error, or any
        # unrelated network failure) is still asserted -- nothing here weakens the
        # "zero page exceptions" gate for anything this panel's own code controls.
        unexpected = [e for e in errors if "503" not in e or "/api/catalog/tilesets" not in e]
        assert unexpected == [], f"expected only the one, real, expected 503 network log line -- got other errors too: {unexpected}"
        assert any("503" in e and "/api/catalog/tilesets" in e for e in errors), (
            f"expected the real 'Refresh catalog' click to genuinely reach the unconfigured route and "
            f"produce its own real 503 -- saw none at all, which would mean the button never fired a "
            f"real fetch: {errors}"
        )
    finally:
        server.stop()

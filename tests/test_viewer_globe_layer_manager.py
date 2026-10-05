"""Question 228 finding 2, the proof that the wiring is real IN A BROWSER.

The lead's browser drive found that nothing in `web/js/app.js` or `web/js/scene.js`
imported `web/js/layers/`, so the globe loaded imagery by its own path and a user could
not see a gateway tile set. Round 4 wired `Viewer` to own exactly one `LayerManager` and
`enableGlobe()` to pass it into `GlobeLayer`, which means **the manager-routed path is now
the production path** -- the one every user meets. A wiring change that is only exercised
by node harnesses is not proven: the node harnesses drive `GlobeLayer` directly and never
construct a `Viewer`, never run `enableGlobe()`, and never bind a texture to a mesh.

So this file drives a REAL headless Chrome against a REAL running viewer server and
asserts, from the scene graph rather than from a counter or a load callback, that a
texture is actually bound to the globe's tile meshes -- "drawn" means drawn -- while zero
page exceptions occur.

The exception collector subscribes to THREE distinct CDP signals, and the third is not
optional (question 211): this platform's console-error gate once reported zero errors
against a page that threw, because it only listened for `Log.entryAdded` and
`Runtime.consoleAPICalled`. An uncaught exception is delivered ONLY as
`Runtime.exceptionThrown`. `test_the_exception_collector_sees_an_uncaught_exception`
below pins that this collector really does catch one, against a page built to throw --
a gate is tested against a page that fails before it is trusted.
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
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


@pytest.fixture()
def live_server():
    """A real `altavista serve`, confirmed listening before any browser connects.

    Question 199: the child's environment is a COPY passed as `env=`; this test never
    assigns to `os.environ`.
    """
    port = _free_port()
    env = dict(os.environ)
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
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

    # Publish a minimal, real scenario so there is an 'Earth' body for the globe to sit
    # on. Deliberately the SMALLEST scenario that exercises the path under test -- one
    # central body with Earth's radius and a single trajectory sample -- rather than a
    # heavyweight run bundle, because what is being proved here is the layer/texture
    # wiring, not scenario decoding (which `tests/test_viewer_panels.py` already covers
    # against the real frozen demo bundle).
    scenario = {
        "name": "globe-layer-manager-proof",
        "frame": {"name": "EarthMJ2000Eq"},
        "bodies": [
            {
                "name": "Earth",
                "central": True,
                "radius": 6378137.0,
                "t": [0.0],
                "pos": [0.0, 0.0, 0.0],
                # `quat` is required, not optional: `BodyInterp.orientation`
                # (web/js/interp.js) reads `this.quat.length` unguarded, so a body
                # without it throws a TypeError on EVERY frame. Found by this very
                # test's own exception collector, which is the point of subscribing to
                # `Runtime.exceptionThrown` -- the frames were failing silently and the
                # globe drew nothing, with no other signal that anything was wrong.
                "quat": [0.0, 0.0, 0.0, 1.0],
            }
        ],
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


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 6.0):
    """Navigate a fresh, isolated headless Chrome to `url`, collect every error on all
    three CDP channels, then evaluate `eval_js` and return `(errors, value)`.

    A fresh `--user-data-dir` per call is deliberate: no cached connection or profile
    state from an earlier run can change this test's result.
    """
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
                # A REAL viewport. The globe's LOD is screen-space error driven
                # (`web/js/globe_lod.js::selectTiles` takes `screenHeightPx`), so a
                # zero-height canvas selects zero tiles and the whole proof passes
                # vacuously with nothing drawn. Measured: without this flag the probe
                # reported meshCount 0 / residentBytes 0 against correctly wired code.
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
                                # Question 211. An uncaught exception reaches us ONLY here.
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


# The page script: enable the globe (which, after round 4, routes through the Viewer's one
# LayerManager), pump frames, then read the ANSWER OFF THE SCENE GRAPH -- walking the
# globe layer's group for a mesh whose material.map is a real texture with real
# dimensions. A counter would prove only that something was counted.
_PROBE_JS = r"""
(async () => {
  const out = {step: 'start'};
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window (window.altavistaViewer unset)'; return out; }
    out.hasLayerManager = !!viewer.layerManager;
    out.budgetBytes = viewer.layerManager ? viewer.layerManager.memoryBudgetBytes : null;
    // A scenario must be loaded before there is an 'Earth' body to put a globe on --
    // exactly what `loadScenario()` in web/js/app.js does (`viewer.setScenario(sc)`).
    // The scenario is fetched from the server's own same-origin route; the pytest
    // fixture publishes it to `POST /api/scenario` first, so the shape crossing the
    // wire here is the real published shape, not one this page invented.
    const names = await (await fetch('/api/scenarios')).json();
    const pick = Array.isArray(names) ? names : (names.names || names.scenarios || []);
    out.scenarioNames = pick;
    if (!pick.length) { out.error = 'the server offers no scenarios'; return out; }
    const name = typeof pick[0] === 'string' ? pick[0] : (pick[0].name || pick[0].id);
    out.scenarioLoaded = name;
    const sc = await (await fetch('/api/scenario/' + encodeURIComponent(name))).json();
    viewer.setScenario(sc);
    out.bodies = (sc.bodies || []).map((b) => b.name || b.id);
    const ok = viewer.enableGlobe('Earth', {});
    out.enableGlobeReturned = ok;
    if (!ok) { out.error = "enableGlobe('Earth') returned false"; return out; }
    out.globeLayerPresent = !!viewer.globeLayer;
    out.globeUsesManager = !!(viewer.globeLayer && viewer.globeLayer.layerManager);
    // Pump real frames so selection + admission + texture load all actually happen.
    // `viewer.update(t)` is driven explicitly because it is exactly what web/js/app.js's
    // own `requestAnimationFrame(frame)` loop calls (`web/js/scene.js::update`, which
    // reaches `_syncGlobeLayer(cam)`); that loop skips the viewer while app.js's own
    // module-scoped `scenario` is null, and this probe set the scenario on the Viewer
    // directly rather than through app.js's private `loadScenario`. Driving the same
    // per-frame entry point keeps this a test of the real path, not of a shortcut.
    for (let i = 0; i < 120; i++) {
      viewer.update(0);
      await new Promise((r) => requestAnimationFrame(r));
    }
    const group = viewer.globeLayer && viewer.globeLayer.group;
    let meshes = 0, withTexture = 0, dims = null;
    if (group) {
      group.traverse((o) => {
        if (!o.isMesh) return;
        meshes += 1;
        const map = o.material && o.material.map;
        const img = map && map.image;
        if (map && img && (img.width || img.naturalWidth) && (img.height || img.naturalHeight)) {
          withTexture += 1;
          if (!dims) dims = [img.width || img.naturalWidth, img.height || img.naturalHeight];
        }
      });
    }
    out.meshCount = meshes;
    out.meshesWithBoundTexture = withTexture;
    out.textureDims = dims;
    const lm = viewer.layerManager;
    if (lm) {
      out.residentBytes = lm.residentBytes;
      out.residentCount = lm.resident.size;
      out.softViolationCount = lm.softViolationCount;
      out.deferredCount = lm.deferredCount;
      out.failedCount = lm.failedCount;
      out.failureNames = lm.failureNames();
      out.withinBudget = lm.residentBytes <= lm.memoryBudgetBytes;
      out.registeredLayers = [...lm._layers.keys()];
    }
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_the_globe_streams_through_the_layer_manager_and_binds_a_texture(live_server):
    """The production path, in a real browser, asserted from the scene graph.

    After round 4, `Viewer` owns one `LayerManager` and `enableGlobe()` always hands it to
    `GlobeLayer`, so this exercises exactly what a user meets. The assertions that matter:
    the manager exists and the globe is using it; at least one tile mesh has a REAL
    texture bound with non-zero dimensions; the manager's own budget invariant holds; and
    the page threw nothing.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, _PROBE_JS, wait_s=6.0))

    # Question 148: show the artifact. `pytest -s` prints exactly what the browser saw,
    # so a green run is readable evidence rather than an exit code.
    print("\nscene-graph probe:", json.dumps(value, indent=2, sort_keys=True))

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"

    assert value.get("hasLayerManager") is True, (
        "the Viewer does not own a LayerManager -- question 228 finding 2 is not closed: " f"{value!r}"
    )
    assert value.get("globeUsesManager") is True, (
        "enableGlobe() did not route the GlobeLayer through the Viewer's LayerManager: " f"{value!r}"
    )
    assert value.get("meshCount", 0) > 0, f"the globe built no tile meshes at all: {value!r}"
    assert value.get("meshesWithBoundTexture", 0) > 0, (
        "no globe tile mesh has a texture bound -- the tiles were requested but never drawn. "
        f"Scene-graph probe: {value!r}"
    )
    assert value.get("withinBudget") is True, f"the manager's byte budget was exceeded: {value!r}"
    assert value.get("softViolationCount") == 0, f"soft budget violation in the live viewer: {value!r}"
    assert errors == [], (
        f"expected zero page exceptions/console errors driving the globe, got {len(errors)}:\n"
        + "\n".join(errors)
    )


# Heavy cleanup round 1 (questions 235/237): the merged-view per-tick update. The viewer
# makes exactly ONE `LayerManager.update(view)` call per render tick, with a view composed
# from the globe's and the entities' wanted sets, so (a) the imagery tiles stream and are
# bound while entities are present -- the round-7 failure mode was that two partial views
# per tick cancelled each other's loads and no imagery tile ever finished -- and (b) the
# markers and trails stay resident, are re-planned every frame, and are drawn. The page
# builds its own scenario (an Earth in correct kilometres plus two spacecraft in LEO, so
# there are real entities to plan) and drives `viewer.update(t)` for real; every answer is
# read off the scene graph and the manager's own maps.
_MERGED_VIEW_PROBE_JS = r"""
(async () => {
  const out = {step: 'start'};
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window (window.altavistaViewer unset)'; return out; }
    const A = 6378.137;
    const sc = {
      name: 'merged-view-proof',
      frame: { name: 'EarthMJ2000Eq' },
      bodies: [{ name: 'Earth', central: true, radius: A, t: [0.0], pos: [0, 0, 0], quat: [0, 0, 0, 1] }],
      spacecraft: [
        { name: 'Alpha', color: '#54a0ff', t: [0.0, 0.01, 0.02], pos: [A + 500, 0, 0, A + 500, 60, 0, A + 500, 120, 0], vel: [0, 7.6, 0, 0, 7.6, 0, 0, 7.6, 0] },
        { name: 'Bravo', color: '#ff6b6b', t: [0.0, 0.01, 0.02], pos: [A + 520, 0, 30, A + 520, 60, 30, A + 520, 120, 30], vel: [0, 7.6, 0, 0, 7.6, 0, 0, 7.6, 0] },
      ],
    };
    viewer.setScenario(sc);
    // Park the camera at LEO altitude so the globe's LOD selects a real, multi-tile set
    // (the default whole-scenario framing would sit tens of thousands of km out and
    // select only the two roots).
    viewer.camera.position.set(7.4, 0.6, 0.9);
    viewer.controls.target.set(0, 0, 0);
    viewer.camera.lookAt(0, 0, 0);
    const ok = viewer.enableGlobe('Earth', {});
    out.enableGlobeReturned = ok;
    if (!ok) { out.error = "enableGlobe('Earth') returned false"; return out; }

    // Count the manager's update calls against the viewer's own ticks: exactly one each.
    const lm = viewer.layerManager;
    // (web/js/app.js's own requestAnimationFrame loop also ticks the viewer while this
    // probe runs, so the viewer's ticks are counted too, not assumed to be this loop's.)
    let updateCalls = 0;
    let ticks = 0;
    const viewKeys = new Set();
    const origUpdate = lm.update.bind(lm);
    lm.update = (view) => {
      updateCalls += 1;
      Object.keys(view).forEach((k) => viewKeys.add(k));
      return origUpdate(view);
    };
    const origViewerUpdate = viewer.update.bind(viewer);
    viewer.update = (t) => { ticks += 1; return origViewerUpdate(t); };

    const FRAMES = 120;
    for (let i = 0; i < FRAMES; i++) {
      viewer.update(0.01);
      await new Promise((r) => requestAnimationFrame(r));
    }
    out.frames = FRAMES;
    out.updateCalls = updateCalls;
    out.viewerTicks = ticks;
    out.viewKeys = [...viewKeys].sort();

    // ---- imagery: bound textures on the globe's tile meshes (scene graph)
    let meshes = 0, withTexture = 0;
    viewer.globeLayer.group.traverse((o) => {
      if (!o.isMesh) return;
      meshes += 1;
      const map = o.material && o.material.map;
      const img = map && map.image;
      if (map && img && (img.width || img.naturalWidth)) withTexture += 1;
    });
    out.meshCount = meshes;
    out.meshesWithBoundTexture = withTexture;
    const counts = lm.countsByLayer();
    out.countsByLayer = counts;
    out.imageryResident = counts.imagery ? counts.imagery.resident : null;
    out.cancelledCount = lm.cancelledCount;
    out.byteCostRevisionCount = lm.byteCostRevisionCount;
    out.softViolationCount = lm.softViolationCount;
    out.residentBytes = lm.residentBytes;
    out.budget = lm.memoryBudgetBytes;

    // ---- entities: resident in the manager AND drawn in the scene graph
    out.entityResident = {
      markers: ['Alpha', 'Bravo'].filter((n) => lm.resident.has('entity-markers\u0000' + n)),
      trails: ['Alpha', 'Bravo'].filter((n) => lm.resident.has('entity-trails\u0000' + n)),
    };
    let markerCount = 0, markerKeys = [], markerMeshVisible = null;
    viewer._entityGroups.markers.traverse((o) => {
      if (o.isInstancedMesh) { markerCount = o.count; markerKeys = o.userData.residentKeys.slice(); markerMeshVisible = o.visible; }
    });
    const trailNames = [];
    viewer._entityGroups.trails.traverse((o) => { if (o.isLine) trailNames.push(o.userData.spacecraft); });
    out.markerCount = markerCount;
    out.markerKeys = markerKeys;
    out.markerMeshVisible = markerMeshVisible;
    out.markerGroupVisible = viewer._entityGroups.markers.visible;
    out.trailNames = trailNames.sort();
    out.trailGroupVisible = viewer._entityGroups.trails.visible;

    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_the_globe_streams_and_the_entities_are_resident_and_drawn_through_one_merged_update(live_server):
    """Heavy cleanup round 1, proof D, in a real browser against the real `Viewer`.

    With a globe active AND entities loaded, the viewer's one shared manager must (a) get
    exactly one `update()` per `viewer.update()` tick carrying BOTH halves' keys, (b) finish
    streaming imagery -- tiles bound to the globe's meshes, none cancelled by the entity
    half (`cancelledCount` stays 0: in this scene nothing else ever drops a wanted tile
    while the camera is parked) -- and (c) keep the markers and trails resident and drawn.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, _MERGED_VIEW_PROBE_JS, wait_s=6.0))

    print("\nmerged-view probe:", json.dumps(value, indent=2, sort_keys=True))

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"

    # (a) one merged update per tick, carrying the globe's AND the entities' keys.
    assert value["viewerTicks"] >= value["frames"], value
    assert value["updateCalls"] == value["viewerTicks"], (
        f"expected exactly one LayerManager.update per viewer.update tick: {value!r}"
    )
    for key in ("tiles", "cameraEcef", "markers", "trails"):
        assert key in value["viewKeys"], f"the composed view never carried '{key}': {value['viewKeys']!r}"

    # (b) imagery streamed through the same manager and was drawn.
    assert value["meshCount"] > 2, f"the camera should select a multi-tile globe: {value!r}"
    assert value["meshesWithBoundTexture"] > 0, (
        f"no globe tile mesh has a texture bound while entities are present: {value!r}"
    )
    assert value["imageryResident"] and value["imageryResident"] > 0, f"no imagery tile became resident: {value!r}"
    assert value["cancelledCount"] == 0, f"a wanted load was cancelled while the camera was parked: {value!r}"

    # (c) the entities are resident AND drawn.
    assert value["entityResident"] == {"markers": ["Alpha", "Bravo"], "trails": ["Alpha", "Bravo"]}, value
    assert value["markerCount"] == 2 and value["markerKeys"] == ["Alpha", "Bravo"], value
    assert value["markerMeshVisible"] is True and value["markerGroupVisible"] is True, value
    assert value["trailNames"] == ["Alpha", "Bravo"] and value["trailGroupVisible"] is True, value

    # The shared budget held, and nothing was re-costed (stable per-tick entity requests).
    assert value["residentBytes"] <= value["budget"], value
    assert value["softViolationCount"] == 0, value
    assert value["byteCostRevisionCount"] == 0, value
    assert errors == [], (
        f"expected zero page exceptions/console errors, got {len(errors)}:\n" + "\n".join(errors)
    )


_DECODE_PROBE_JS = r"""
(async () => {
  const out = {step: 'start'};
  try {
    const mod = await import('/js/layers/gateway_imagery_layer.js');
    const resp = await fetch('/fixtures/tiles/0/0/0.png');
    out.fetchOk = resp.ok;
    out.fetchStatus = resp.status;
    const bytes = await resp.arrayBuffer();
    out.byteLength = bytes.byteLength;
    const texture = await mod.decodeTileBytesToTexture(bytes);
    out.isTexture = texture.isTexture === true;
    out.decodeMode = texture.userData.decodeMode;
    out.imageWidth = texture.image && texture.image.width;
    out.imageHeight = texture.image && texture.image.height;
    texture.dispose();
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_decode_tile_bytes_to_texture_really_decodes_a_real_png_in_a_real_browser(live_server):
    """Round 6 (docs/open-questions.md question 231's ruling) -- manager review of
    that task's own report: the two node-based real-gateway proofs
    (`web/js/layers_stream_check.mjs` and its GlobeLayer probe) are structurally
    stuck in `decodeTileBytesToTexture`'s "no `createImageBitmap` at all" fallback
    mode, because node has no `createImageBitmap` -- true even though the real
    `av-tiles` gateway those harnesses drive serves genuinely real PNG bytes
    (`crates/av-jobs/src/tiler.rs`'s own `image::codecs::png` encoder). A provenance
    tag alone ("this mesh's texture came from gateway-a") is NOT proof that gateway-a's
    own real pixels reached the screen -- it is equally true, and indistinguishable
    from a provenance assertion alone, when every one of gateway-a's tiles silently
    decoded to the SAME opaque-white 1x1 placeholder. This is the one, minimal,
    reuses-existing-infra way to close that gap: no new docker-gated fixture (per the
    manager's own instruction) -- `live_server` is the SAME plain `python -m altavista
    serve` subprocess `test_the_globe_streams_through_the_layer_manager_and_binds_a_
    texture` above already uses (no tiles gateway configured, no docker at all), and
    `web/fixtures/tiles/0/0/0.png` is an EXISTING, already-committed real 64x64 PNG
    this repo's own default imagery fixture set already serves. This test fetches
    those real bytes over a real HTTP request in a real browser (which DOES have
    `createImageBitmap`) and feeds them directly to the real, exported
    `decodeTileBytesToTexture` (the exact function `GatewayImageryLayerAdapter.load()`
    calls after its own SHA-256/ETag verification) -- proving the real-decode branch
    genuinely decodes a real PNG into a real, correctly-dimensioned texture, not just
    that the branch exists and returns SOMETHING texture-shaped.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, _DECODE_PROBE_JS, wait_s=2.0))

    print("\nreal PNG decode probe:", json.dumps(value, indent=2, sort_keys=True))

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"
    assert value.get("fetchOk") is True, f"could not fetch the real fixture PNG: {value!r}"
    assert value.get("byteLength", 0) > 0, f"the fetched PNG was empty: {value!r}"
    assert value.get("isTexture") is True, f"decodeTileBytesToTexture did not return a real THREE.Texture: {value!r}"
    assert value.get("decodeMode") == "createImageBitmap", (
        f"expected the REAL decode branch (a real browser has createImageBitmap), not a fallback: {value!r}"
    )
    # The real, known dimensions of web/fixtures/tiles/0/0/0.png (confirmed directly
    # from its own IHDR chunk, this task's own report) -- a placeholder texture could
    # never produce these, only a genuine decode of these exact real bytes could.
    assert value.get("imageWidth") == 64, f"expected the real fixture PNG's own real width (64): {value!r}"
    assert value.get("imageHeight") == 64, f"expected the real fixture PNG's own real height (64): {value!r}"
    assert errors == [], f"expected zero page exceptions/console errors, got {len(errors)}:\n" + "\n".join(errors)


def test_the_exception_collector_sees_an_uncaught_exception():
    """Teeth for the detection mechanism the test above depends on (question 211).

    Points the SAME `_drive` helper at a page that throws an uncaught `TypeError` from a
    deferred task -- the class of failure delivered only as `Runtime.exceptionThrown`. If
    this collector could not see it, the test above would pass against a page that
    crashed, which is exactly the defect question 211 records.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    with tempfile.TemporaryDirectory() as d:
        page = Path(d) / "throws.html"
        page.write_text(
            "<!doctype html><title>t</title><script>"
            "setTimeout(function(){ null.boom; }, 10);"
            "</script>"
        )
        errors, _ = asyncio.run(_drive(page.as_uri(), chrome_path, "1", wait_s=2.0))
    assert any("exceptionThrown" in e for e in errors), (
        "the collector did NOT report an uncaught exception; it cannot be trusted to prove "
        f"zero exceptions anywhere else. Got: {errors!r}"
    )

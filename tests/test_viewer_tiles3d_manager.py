"""Round 5 (docs/open-questions.md question 228/decision 9, round 4's own deferral,
ratified question 229): the 3D Tiles overlay's real per-tile content fetch is now
routed through the one per-viewer ``LayerManager`` (``web/js/layers/layer.js``), via
``Tiles3DLayerAdapter``/``ManagerGatedTilesFetchPlugin`` (``web/js/layers/
tiles3d_layer.js`` -- see that file's own module docstring, "Fetch gating", for the
full mechanism, and ``web/js/tiles_layer.js``'s own module docstring, "Round 5", for
how ``TilesOverlayLayer`` wires it into the live vendored ``TilesRenderer``).

This is a NEW test file, deliberately separate from ``tests/
test_viewer_layers_budget.py`` (already modified for a different, in-flight commit
this task must not touch) -- its own node harness is ``web/js/
tiles3d_manager_check.mjs`` (also new), never an extension of ``web/js/
layers_budget_check.mjs``. It also never touches ``tests/test_viewer_tiles_route.py``
(the real-docker-stack suite this task must not run) or any of the pre-existing
node checks/tests this task's own report documents as unchanged in outcome (``web/js/
tiles3d_check.mjs`` via ``tests/test_viewer_globe.py``, ``web/js/layers_check.mjs`` /
``web/js/layers_budget_check.mjs`` via ``tests/test_viewer_layers.py`` / ``tests/
test_viewer_layers_budget.py``, ``web/js/globe_lod_check.mjs``, ``web/js/
gateway_imagery_layer_check.mjs``).

What ``web/js/tiles3d_manager_check.mjs`` proves, asserted on here (see that file's
own module docstring for the full reasoning behind each):
  1. ``noDuplicateFetch`` -- the vendored renderer's own ``fetchData`` hook (the real
     ``ManagerGatedTilesFetchPlugin.fetchData()`` method, called directly with the
     same argument shapes a live ``TilesRenderer.requestTileContents()`` uses) never
     causes a second real network fetch for a tile the manager already admitted --
     proved twice, once for repeated asks about the SAME tile, once (the fixture's
     own multi-key/one-URL collision, see the check's own module docstring) for many
     DIFFERENT tiles that happen to share a URL, which must each still get their own
     independent fetch (dedup is per TILE, never per URL).
  2. ``budgetGates`` -- a wanted set whose cost, recomputed independently from a
     FRESH ``plan()`` call (never by trusting the manager's own stored total), exceeds
     the declared budget still never lets resident bytes exceed it, and
     ``deferredCount`` moves.
  3. ``cancellationReal`` -- a request admitted but not yet settled, then dropped by a
     camera jump, has its manager-issued ``AbortController`` genuinely aborted AND its
     own load promise genuinely reject with a real ``AbortError`` (awaited and
     caught, not inferred from a counter).
  4. ``releaseRefetches`` -- evicting a resident tile and later re-wanting the same
     key causes a real, fresh network attempt (``release()`` genuinely drops the
     cached fetch, never leaves a stale entry silently reused).
  5. ``realByteReconciliation`` -- once a real fetch resolves with a real
     ``Content-Length`` different from the declared estimate, a FRESH ``plan()`` call
     reports that real number, and the manager's own (unmodified) reconciliation pass
     picks it up -- this task's own analogue of round 4's defect 1 ("a cost captured
     at admission that nothing re-read").
  6. ``crossContaminationFixed`` / ``crossContaminationCounterfactual`` -- registering
     the overlay's adapter on the SAME manager a globe-shaped imagery layer is also
     registered on, and driving both from ONE merged ``update()`` call per tick
     (``web/js/scene.js``'s own round-5 orchestration) never evicts/cancels the
     imagery layer's own still-wanted resident entry -- and the counterfactual proves
     this was a REAL hazard (the naive "call update() twice, once per layer" pattern
     this task's own scene.js/globe.js/tiles_layer.js changes deliberately avoid DOES
     evict it), not a hypothetical one invented to justify the fix.

Disclosed, deliberate limitation shared with ``tests/test_viewer_globe.py``'s own
M16.4 section: this is ``node``, not a browser -- there is no live vendored
``TilesRenderer`` in ``web/js/tiles3d_manager_check.mjs`` at all (confirmed directly
while building this, see ``web/js/tiles_layer.js``'s module docstring: it needs
``window``/``requestAnimationFrame``). ``ManagerGatedTilesFetchPlugin`` is exercised
for REAL there (the exact exported class and method a live renderer calls); only the
CALLER (a live ``TilesRenderer``) is stood in for. The live-headless-Chrome half of
this proof, mirroring ``tests/test_viewer_globe_layer_manager.py``'s own pattern, is
below (``test_live_...``) -- see that test's own skip/xfail reason if the vendored
renderer's live fetch cannot be observed to complete within this task's own budget.
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
TILES3D_MANAGER_CHECK = REPO_ROOT / "web" / "js" / "tiles3d_manager_check.mjs"

NODE = shutil.which("node")

# ---------------------------------------------------------------------- live Chrome
# Duplicated from tests/test_viewer_globe_layer_manager.py on purpose: that file's
# `live_server`/`_drive`/`_find_chrome`/`_free_port`/`_LiveServer` are module-private
# (leading underscore) and this codebase has no shared conftest.py (checked directly:
# there is none under tests/) -- every existing *_layer_manager.py-style live test
# duplicates this same real-Chrome-over-CDP harness rather than share it, and this
# file follows that same, already-established convention.
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
    """A real ``altavista serve``, confirmed listening before any browser connects,
    with a scenario that has an ``Earth`` body (for the globe) published to it --
    same shape and same "question 199: the child's environment is a COPY passed as
    ``env=``" discipline as ``tests/test_viewer_globe_layer_manager.py``'s own
    fixture of the same name.
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

    scenario = {
        "name": "tiles3d-manager-live-proof",
        "frame": {"name": "EarthMJ2000Eq"},
        "bodies": [
            {
                "name": "Earth",
                "central": True,
                "radius": 6378137.0,
                "t": [0.0],
                "pos": [0.0, 0.0, 0.0],
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
    three CDP channels (question 211: `Runtime.exceptionThrown` is the ONLY channel an
    uncaught exception is ever delivered on), then evaluate `eval_js` and return
    `(errors, value)`. Byte-for-byte the same shape as
    tests/test_viewer_globe_layer_manager.py's own `_drive` -- see that file for the
    full reasoning behind each flag/step.
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


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; this task's own harness "
            "drives the real Tiles3DLayerAdapter/ManagerGatedTilesFetchPlugin/"
            "LayerManager classes as real ES modules (see module docstring) rather "
            "than porting their logic to Python, so it cannot proceed without node."
        )
    return NODE


def _run_node_json(script: Path) -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(script)],
        cwd=str(script.parent),
        capture_output=True,
        text=True,
        timeout=30,
        env={},  # question 154/199: no network, no inherited/mutated process env
    )
    assert proc.returncode == 0, (
        f"node {script.name} exited {proc.returncode}\n"
        f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{script.name} did not print valid JSON: {proc.stdout!r}\nstderr: {proc.stderr}")


@pytest.fixture(scope="module")
def tiles3d_manager_data() -> dict:
    return _run_node_json(TILES3D_MANAGER_CHECK)


@pytest.fixture(scope="module")
def tiles3d_manager_run_1_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(TILES3D_MANAGER_CHECK)], cwd=str(TILES3D_MANAGER_CHECK.parent),
        capture_output=True, text=True, timeout=30, env={},
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    return proc.stdout


@pytest.fixture(scope="module")
def tiles3d_manager_run_2_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(TILES3D_MANAGER_CHECK)], cwd=str(TILES3D_MANAGER_CHECK.parent),
        capture_output=True, text=True, timeout=30, env={},
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    return proc.stdout


# ==================================================================================
# Determinism (same discipline as tests/test_viewer_globe.py's own tiles3d_check.mjs
# proof and tests/test_viewer_layers_budget.py's own layers_budget_check.mjs proof):
# two independent `node` process runs of the SAME harness must print byte-identical
# JSON -- this harness's own camera path/fixture/fake loaders are fully deterministic
# (design constraint g: injected/simulated time only, never a real clock; question
# 154: no network).
# ==================================================================================
def test_tiles3d_manager_check_is_deterministic_across_process_runs(
    tiles3d_manager_run_1_raw: str, tiles3d_manager_run_2_raw: str,
) -> None:
    assert tiles3d_manager_run_1_raw == tiles3d_manager_run_2_raw, (
        "web/js/tiles3d_manager_check.mjs produced different output across two "
        "independent process runs -- something in its own camera path/fixture/fake "
        "loaders is not deterministic"
    )


# ==================================================================================
# 1. No duplicate fetch
# ==================================================================================
def test_no_duplicate_fetch_for_the_same_tile(tiles3d_manager_data: dict) -> None:
    d = tiles3d_manager_data["noDuplicateFetch"]
    assert d["fetchDataFallbackCount"] == 0, (
        "this proof's every URL should have been recognised by the adapter's own "
        "plan() -- a nonzero fallback count means the test setup itself is wrong, "
        "not the mechanism under test"
    )
    # Four asks (3 before settle, 1 after) plus one manager-driven admission must
    # still be exactly ONE real call to the injected loader.
    assert d["fetchCallCount"] == 1
    assert d["loaderInvocationCount"] == 1
    assert d["loaderInvocationCountForRootUrl"] == 1
    assert d["askedAfterSettleIsOk"] is True


def test_dedup_is_per_tile_not_per_url(tiles3d_manager_data: dict) -> None:
    """This project's own fixture (web/fixtures/3dtiles/tileset.json) gives every
    tile the same relative content URI -- see web/js/tiles3d_manager_check.mjs's own
    module docstring. A cache keyed by URL (an earlier version of this task's own
    Tiles3DLayerAdapter, caught and fixed before landing -- see this task's report)
    would silently let one tile's fetch satisfy an unrelated tile's own resident
    accounting; this asserts that does not happen.
    """
    d = tiles3d_manager_data["noDuplicateFetch"]["manyKeysSharedOneUrl"]
    assert d["distinctUrlsAmongThem"] == 1, "test setup: expected the fixture's own URL collision"
    assert d["distinctTileKeysWanted"] > 1
    assert d["admittedKeyCount"] == d["distinctTileKeysWanted"]
    assert d["dedupIsPerKeyNotPerUrl"] is True, (
        f"expected one real fetch per admitted KEY ({d['admittedKeyCount']}), got "
        f"{d['loaderInvocationCount']} -- dedup is leaking across tiles that merely "
        f"share a URL"
    )


# ==================================================================================
# 2. Budget genuinely gates admission
# ==================================================================================
def test_budget_gates_the_overlay(tiles3d_manager_data: dict) -> None:
    d = tiles3d_manager_data["budgetGates"]
    assert d["wantedExceedsBudget"] is True, (
        "test setup: this proof requires a wanted set that genuinely exceeds the "
        "budget, or 'the budget gates it' proves nothing"
    )
    assert d["everyStepWithinBudget"] is True, (
        f"residentBytes exceeded memoryBudgetBytes at some step: {d['steps']}"
    )
    assert d["residentBytesRecomputedMatches"] is True, (
        "residentBytes disagreed with an independently-recomputed total (summed "
        "from a FRESH plan() call, never from the manager's own stored entries) -- "
        "see this file's module docstring on why that independent recomputation "
        "matters"
    )
    assert d["finalDeferredCount"] > 0, (
        "expected at least one request to have been deferred by the budget over "
        "this camera path"
    )
    assert d["softViolationCount"] == 0, (
        "a soft violation means the hard admission invariant (layer.js's own, "
        "unmodified) was broken -- see that file's own module docstring"
    )


# ==================================================================================
# 3. Cancellation is real
# ==================================================================================
def test_cancellation_is_real(tiles3d_manager_data: dict) -> None:
    d = tiles3d_manager_data["cancellationReal"]
    assert d["watchedKeyDroppedFromWanted"] is True, (
        "test setup: the watched key should have dropped out of the wanted set on "
        "the camera jump"
    )
    assert d["signalAborted"] is True, (
        "the manager-issued AbortController for a request that fell out of the "
        "wanted set was never aborted"
    )
    assert d["cancelledCountMoved"] is True
    assert d["cancelledCountDelta"] > 0
    assert d["loadPromiseRejectedWithAbortError"] is True, (
        "the REAL load() promise (recovered from the adapter's own fetch cache, "
        "never a promise invented by the test harness) did not reject with a real "
        "AbortError -- cancellation must be observable on the actual promise the "
        "manager's own load() call produced, not merely inferred from a counter"
    )


# ==================================================================================
# 4. release() genuinely frees the adapter's own resource
# ==================================================================================
def test_release_frees_and_a_rewant_refetches(tiles3d_manager_data: dict) -> None:
    d = tiles3d_manager_data["releaseRefetches"]
    assert d["residentAfterFirstLoad"] is True, "test setup: the root tile should have loaded and become resident"
    assert d["invocationsAfterFirstLoad"] == 1
    assert d["evictedAfterElsewhereWanted"] is True, (
        "test setup: a tiny budget plus wanting something else should have evicted "
        "the root tile"
    )
    assert d["evictedCount"] > 0
    assert d["refetchedAfterRelease"] is True, (
        f"re-wanting an evicted tile did not trigger a fresh fetch "
        f"(invocations {d['invocationsAfterFirstLoad']} -> {d['invocationsAfterRewant']}) "
        f"-- release() left a stale cached fetch entry behind"
    )


# ==================================================================================
# 5. The bytes the manager accounts are the bytes actually transferred
# ==================================================================================
def test_real_byte_cost_is_reconciled(tiles3d_manager_data: dict) -> None:
    d = tiles3d_manager_data["realByteReconciliation"]
    assert d["byteCostSourceBeforeLoad"] == "fallback-estimate"
    assert d["byteCostBeforeLoad"] == d["defaultEstimate"]
    assert d["declaredContentLength"] != d["defaultEstimate"], (
        "test setup: the stub's declared Content-Length must differ from the "
        "fallback estimate, or reconciliation proves nothing"
    )
    assert d["freshPlanNowReportsRealBytes"] is True, (
        f"a FRESH plan() call after the real fetch resolved still reported "
        f"{d['byteCostAfterLoad']!r} ({d['byteCostSourceAfterLoad']!r}), not the "
        f"real declared length {d['declaredContentLength']!r} ('measured') -- "
        f"round 4's defect 1 (a cost captured at admission that nothing re-read), "
        f"reintroduced"
    )
    assert d["byteCostRevisionCountMoved"] is True, (
        "LayerManager's own (unmodified) reconciliation pass never fired"
    )
    assert d["residentBytesNowReflectsReal"] is True, (
        f"residentBytes ({d['residentBytesAfter']}) does not equal the real "
        f"declared Content-Length ({d['declaredContentLength']}) after reconciliation"
    )


# ==================================================================================
# 6. No cross-layer contamination when sharing one manager with the globe
# ==================================================================================
def test_no_cross_contamination_when_globe_shares_the_manager(tiles3d_manager_data: dict) -> None:
    fixed = tiles3d_manager_data["crossContaminationFixed"]
    assert fixed["imageryResidentAfterTick1"] is True
    assert fixed["imageryStillResidentAtEnd"] is True, (
        "a fake globe-shaped imagery layer's own still-wanted resident entry was "
        "evicted merely because the 3D Tiles overlay ALSO shares the manager -- "
        "see web/js/tiles_layer.js's/web/js/globe.js's own 'Round 5' module "
        "docstrings for the one-merged-call-per-tick discipline this guards"
    )
    assert fixed["imageryReleaseCount"] == 0, "imagery's own resident entry should never have been released"
    assert fixed["imageryLoadCount"] == 2, (
        f"imagery's own loader should have been called exactly once per its two "
        f"tiles, never re-triggered by an unrelated layer's own update() call; got "
        f"{fixed['imageryLoadCount']}"
    )
    assert fixed["managerCancelledCount"] == 0
    assert fixed["managerEvictedCount"] == 0


def test_cross_contamination_counterfactual_proves_the_hazard_was_real(tiles3d_manager_data: dict) -> None:
    """The measured PROOF this was a real hazard, not a hypothetical excuse for the
    fix: the SAME setup, driven by the naive "call update() twice, once per layer,
    each with only that layer's own fields" pattern this task's own scene.js/globe.js
    changes deliberately avoid, DOES evict/thrash the imagery layer's own resident
    entry. If this test ever starts passing (i.e. the naive pattern stops being
    harmful), that is itself worth investigating -- it would mean this proof no
    longer demonstrates what it claims to.
    """
    counter = tiles3d_manager_data["crossContaminationCounterfactual"]
    assert counter["imageryStillResidentAtEnd"] is False, (
        "expected the naive two-calls-per-tick pattern to evict imagery's own "
        "resident entry -- if it did not, this counterfactual no longer proves the "
        "hazard was real"
    )
    assert counter["managerCancelledCount"] > 0
    assert counter["imageryLoadCount"] > 2, (
        "expected the naive pattern to cause repeated re-fetch thrash (more than "
        "the two tiles imagery originally wanted)"
    )


# ==================================================================================
# Live headless Chrome (mirrors tests/test_viewer_globe_layer_manager.py's own pattern
# -- a real `altavista serve` process, spawned directly by this test via
# `subprocess.Popen`, driven by a real headless Chrome over CDP/websockets)
# ==================================================================================
# The page script: enable the globe AND load the 3D Tiles overlay onto the SAME
# `Viewer.layerManager`, pump real animation frames (so real fetch/parse/admission all
# actually happen against a real, same-origin, same-process server -- no Browser-pane
# preview-tool snapshot involved, see this task's own report for why that tool could
# not be used here), then read the ANSWER OFF THE SCENE GRAPH (real mesh geometry
# under `tilesOverlay.renderer.group`, a real texture bound to a globe tile mesh) and
# off the real `LayerManager`/`Tiles3DLayerAdapter` instances themselves -- never only
# a counter.
_PROBE_JS = r"""
(async () => {
  const out = {step: 'start'};
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window (window.altavistaViewer unset)'; return out; }
    const names = await (await fetch('/api/scenarios')).json();
    const pick = Array.isArray(names) ? names : (names.names || names.scenarios || []);
    if (!pick.length) { out.error = 'the server offers no scenarios'; return out; }
    const name = typeof pick[0] === 'string' ? pick[0] : (pick[0].name || pick[0].id);
    const sc = await (await fetch('/api/scenario/' + encodeURIComponent(name))).json();
    viewer.setScenario(sc);

    const globeOk = viewer.enableGlobe('Earth', {});
    out.enableGlobeReturned = globeOk;
    if (!globeOk) { out.error = "enableGlobe('Earth') returned false"; return out; }

    viewer.loadTilesOverlay('./fixtures/3dtiles/tileset.json');
    await viewer.tilesOverlay.ready; // the tree fetch/parse this adapter's plan() needs

    out.globeLayerPresent = !!viewer.globeLayer;
    out.tilesOverlayPresent = !!viewer.tilesOverlay;
    out.tilesOverlayUsesManager = !!(viewer.tilesOverlay && viewer.tilesOverlay._layerManager === viewer.layerManager);
    out.registeredLayers = viewer.layerManager ? [...viewer.layerManager._layers.keys()] : [];

    // Pump real frames -- exactly web/js/app.js's own requestAnimationFrame loop
    // calls (Viewer.update(t) -> _syncGlobeLayer + tilesOverlay.update(!globeLayer),
    // this task's own round-5 orchestration). 240 frames (double the globe-only
    // proof's own 120): the vendored TilesRenderer needs real network fetch + real
    // GLTFLoader parse time on top of what the globe alone needs.
    for (let i = 0; i < 240; i++) {
      viewer.update(0);
      await new Promise((r) => requestAnimationFrame(r));
    }

    // Globe: real texture bound (same assertion tests/test_viewer_globe_layer_manager.py makes).
    let globeMeshes = 0, globeWithTexture = 0;
    const globeGroup = viewer.globeLayer && viewer.globeLayer.group;
    if (globeGroup) {
      globeGroup.traverse((o) => {
        if (!o.isMesh) return;
        globeMeshes += 1;
        const map = o.material && o.material.map;
        const img = map && map.image;
        if (map && img && (img.width || img.naturalWidth)) globeWithTexture += 1;
      });
    }
    out.globeMeshCount = globeMeshes;
    out.globeMeshesWithBoundTexture = globeWithTexture;

    // 3D Tiles overlay: real geometry under the VENDORED renderer's own group --
    // proves bytes this task's plugin gated actually reached parseTile and became a
    // drawable mesh, not merely that a fetch happened.
    let tileMeshes = 0, tileMeshesWithVertices = 0;
    const tilesGroup = viewer.tilesOverlay && viewer.tilesOverlay.renderer.group;
    if (tilesGroup) {
      tilesGroup.traverse((o) => {
        if (!o.isMesh) return;
        tileMeshes += 1;
        const pos = o.geometry && o.geometry.attributes && o.geometry.attributes.position;
        if (pos && pos.count > 0) tileMeshesWithVertices += 1;
      });
    }
    out.tileMeshCount = tileMeshes;
    out.tileMeshesWithVertices = tileMeshesWithVertices;

    const adapter = viewer.tilesOverlay && viewer.tilesOverlay._adapter;
    out.adapterPresent = !!adapter;
    out.adapterFetchCallCount = adapter ? adapter.fetchCallCount : null;
    out.adapterFetchDataFallbackCount = adapter ? adapter.fetchDataFallbackCount : null;

    const lm = viewer.layerManager;
    if (lm) {
      out.residentBytes = lm.residentBytes;
      out.pendingBytes = lm.pendingBytes;
      out.softViolationCount = lm.softViolationCount;
      out.withinBudget = lm.residentBytes <= lm.memoryBudgetBytes;
      out.perLayerCounts = lm.countsByLayer();
      out.cancelledCount = lm.cancelledCount;
      out.evictedCount = lm.evictedCount;
    }
    out.scheduleStats = viewer.tilesOverlay ? viewer.tilesOverlay.getScheduleStats() : null;
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_live_globe_and_tiles_overlay_share_one_manager_without_page_exceptions(live_server):
    """The production path, in a real browser, asserted from the scene graph -- the
    live version of this file's own headless `crossContaminationFixed` proof above,
    and the one place `ManagerGatedTilesFetchPlugin` is exercised against a REAL
    vendored `TilesRenderer`, not this file's own headless stand-in caller.

    Assertions that matter: both the globe and the 3D Tiles overlay register on the
    SAME `Viewer.layerManager`; the globe still binds a real texture (proves round 5
    did not regress round 4's own live proof, `tests/
    test_viewer_globe_layer_manager.py`); the 3D Tiles overlay's vendored renderer
    built at least one real mesh with real vertex data (proves the plugin-gated bytes
    actually reached `parseTile`, not merely that a fetch happened);
    `adapterFetchDataFallbackCount` is reported (any nonzero value is a genuine,
    disclosed finding about how often the renderer's own traversal diverges from this
    adapter's `plan()` in a live scene, not a hidden failure -- see this file's module
    docstring and `web/js/layers/tiles3d_layer.js`'s own module docstring on that
    divergence); the manager's own budget invariant holds; and the page threw nothing.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, _PROBE_JS, wait_s=6.0))

    print("\ntiles3d + globe scene-graph probe:", json.dumps(value, indent=2, sort_keys=True))
    if errors:
        print("console/page errors:\n" + "\n".join(errors))

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"

    assert value.get("enableGlobeReturned") is True
    assert value.get("tilesOverlayUsesManager") is True, (
        f"TilesOverlayLayer was not routed through the Viewer's own LayerManager: {value!r}"
    )
    assert set(value.get("registeredLayers") or []) >= {"imagery", "terrain", "tiles3d"}, (
        f"expected all three layers registered on the one shared manager: {value!r}"
    )

    assert value.get("globeMeshCount", 0) > 0
    assert value.get("globeMeshesWithBoundTexture", 0) > 0, (
        f"the globe's own live proof regressed: no globe tile mesh has a texture "
        f"bound while the 3D Tiles overlay also shares the manager: {value!r}"
    )

    assert value.get("adapterPresent") is True
    assert value.get("adapterFetchCallCount", 0) > 0, (
        f"the adapter never started a single real fetch over 240 frames: {value!r}"
    )
    assert value.get("tileMeshCount", 0) > 0, (
        f"the vendored TilesRenderer built no mesh at all under its own group -- "
        f"bytes were fetched but never reached parseTile/rendered: {value!r}"
    )
    assert value.get("tileMeshesWithVertices", 0) > 0, (
        f"every 3D Tiles mesh had empty/missing vertex data: {value!r}"
    )

    assert value.get("withinBudget") is True, f"the manager's byte budget was exceeded: {value!r}"
    assert value.get("softViolationCount") == 0, f"soft budget violation in the live viewer: {value!r}"

    assert errors == [], (
        f"expected zero page exceptions/console errors driving the globe + 3D Tiles "
        f"overlay together, got {len(errors)}:\n" + "\n".join(errors)
    )

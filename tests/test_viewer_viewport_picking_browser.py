"""Question 254 item 3 (heavy open-items round): picking in an extra viewport uses THAT viewport's
own placement of the shared entity objects, proven in a REAL headless Chrome against a REAL
`python -m altavista serve`.

The defect: since question 252 the shared entity objects (marker spheres, event meshes, the glTF
model, ...) are written per drawing view at render time (`Viewer._placeSharedEntities`), but between
ticks the scene graph holds the PRIMARY's placement, and `viewer.pick(camera, ...)` raycast that
state whatever camera it was given. A second viewport framed ~10 m from the 1.5 m Chaser model draws
the model where its own placement puts it; the primary's placement is 0.26-0.74 m away at LEO,
1.9-5.1 m at GEO, 15-54 m at lunar distance, so a click on the model as drawn could miss it. And the
model's meshes were not a pick target at all (only the 7 px marker at its origin).

What this does (the same kernel as the node check, web/js/viewport_picking_check.mjs, imported into
the page), at LEO 7,000 km, GEO 42,164 km and lunar 384,400 km, with the primary after `fit()` and
framed on the Chaser: through the REAL `viewer.pick(vp.camera, ndcX, ndcY)` of the real page (real
WebGL renderers, real ticks) -- picks at the Chaser model's float64-truth projected origin and 12
points across its truth silhouette (each must resolve to the Chaser), points off the silhouette
(none may), the Target's marker, the scene graph unchanged bit for bit by a pick, the primary's
picks against the old target list, and the cost of one pick. Then the page's own double-click
handler (web/js/app.js `mintViewport`, minted through the layout manager's "3D Viewport" factory):
synthetic `dblclick` MouseEvents at the model's projected pixel in the viewport's real canvas, with
`viewer.setViewportFocus` replaced by a recorder so the camera is not moved: a click on the model
focuses the Chaser, a click well off it does not.

The harness helpers (`_find_chrome`/`_free_port`/`_LiveServer`/`_drive`) are
tests/test_viewer_viewport_precision_browser.py's own, duplicated per this repo's convention.
"""
from __future__ import annotations

import asyncio
import json
import math
import os
import shutil
import subprocess
import sys
from fixture_profiles import FIXTURE_PROFILE_ARGS  # noqa: E402 -- test-only profile dir (question 264)
import tempfile
import time
import urllib.request
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
]

SCENARIO_NAME = "Viewport picking"
MU_EARTH_KM3_S2 = 398600.4418
INCLINATION_DEG = 51.6
EPOCH_A1MJD = 34000.0
STEP_S = 5.0
N_SAMPLES = 400
CHASER_LEAD_M = 30.0
MODEL_URL = "/js/fixtures/entity_model_fixture.gltf"
COV_DIAG_KM2 = [1.0, 0.25, 0.0625]
DISTANCES = (("LEO", 7000.0), ("GEO", 42164.0), ("lunar", 384400.0))


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


def _http_post_json(url: str, payload: dict) -> None:
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        assert r.status in (200, 201), f"publishing to {url} answered HTTP {r.status}"


def _scenario(radius_km: float) -> dict:
    """Target and Chaser on one circular orbit of `radius_km`, the Chaser 30 m ahead in track and
    carrying the committed glTF fixture as its `model`, both carrying a closed-form diagonal
    covariance (so the ellipsoid and the keep-out volume exist), plus one event on the Chaser
    in the middle of the measured window."""
    from altavista.model import BodyTrack, Event, Frame, ScenarioData, Trajectory

    n = math.sqrt(MU_EARTH_KM3_S2 / radius_km ** 3)
    inc = math.radians(INCLINATION_DEG)
    speed = n * radius_km
    lead = (CHASER_LEAD_M / 1000.0) / radius_km
    block = [COV_DIAG_KM2[0], 0.0, 0.0, 0.0, COV_DIAG_KM2[1], 0.0, 0.0, 0.0, COV_DIAG_KM2[2]]

    def track(name, phase, color, **kw):
        ts, pos, vel, cov = [], [], [], []
        for k in range(N_SAMPLES):
            s = k * STEP_S
            a = n * s + phase
            ts.append(EPOCH_A1MJD + s / 86400.0)
            pos.append([radius_km * math.cos(a), radius_km * math.sin(a) * math.cos(inc),
                        radius_km * math.sin(a) * math.sin(inc)])
            vel.append([-speed * math.sin(a), speed * math.cos(a) * math.cos(inc),
                        speed * math.cos(a) * math.sin(inc)])
            cov.append(list(block))
        return Trajectory(name=name, t=ts, pos=pos, vel=vel, color=color, cov=cov, cov_dim=3, **kw)

    target = track("Target", 0.0, "#54a0ff")
    chaser = track("Chaser", lead, "#ff6b6b", model=MODEL_URL)
    ts = target.t
    earth = BodyTrack(name="Earth", radius=6378.137, flattening=1 / 298.257223563, color="#2e6fbd",
                      central=True, t=[ts[0], ts[-1]], pos=[[0, 0, 0], [0, 0, 0]],
                      quat=[[0, 0, 0, 1], [0, 0, 0, 1]])
    data = ScenarioData(
        name=f"{SCENARIO_NAME} {radius_km:g}", frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
        spacecraft=[target, chaser], bodies=[earth],
        events=[Event(name="Burn", t=EPOCH_A1MJD + 612.0 / 86400.0, type="maneuver", spacecraft="Chaser")],
        frames=[{"id": "EarthMJ2000Eq", "axes": "AXES_KIND_MJ2000_EQ", "body": "Earth"}],
    )
    return data.to_dict()


@pytest.fixture()
def live_server():
    port = _free_port()
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", *FIXTURE_PROFILE_ARGS, "--host", "127.0.0.1", "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT), env=dict(os.environ),
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
    try:
        _http_post_json(server.url.rstrip("/") + "/api/scenario", _scenario(DISTANCES[0][1]))
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the scenario: {exc}")
    try:
        yield server
    finally:
        server.stop()


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 4.0, eval_timeout_s: float = 900.0):
    """tests/test_viewer_entity_framing_browser.py's `_drive` (all three CDP error channels:
    Log.entryAdded errors, console error/warning, Runtime.exceptionThrown), with the evaluation
    deadline a parameter (the frame-time runs take minutes under software GL)."""
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen(
            [
                chrome_path, f"--user-data-dir={profile_dir}", "--headless=new",
                f"--remote-debugging-port={cdp_port}", "--no-first-run", "--no-default-browser-check",
                "--disable-extensions", "--use-gl=swiftshader", "--enable-unsafe-swiftshader",
                "--window-size=1900,800", "about:blank",
            ],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
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
                                if msg["params"].get("type") in ("error", "warning"):
                                    errors.append(f"[console.{msg['params'].get('type')}] {msg['params'].get('args')}")
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
                    deadline = time.monotonic() + eval_timeout_s
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


# The page script. `__SCENARIOS__` is replaced by the three scenarios (JSON).
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start', runs: {}, clicks: {} };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three');
    const M = await import('/js/entities_viewport_precision_check.mjs');
    const P = await import('/js/viewport_picking_check.mjs');
    const SC = __SCENARIOS__;
    const until = async (cond, what, ms = 60000) => {
      const end = performance.now() + ms;
      while (performance.now() < end) {
        if (cond()) return true;
        await new Promise((r) => requestAnimationFrame(r));
      }
      throw new Error('timed out waiting for ' + what);
    };
    const frames = (n) => new Promise((resolve) => {
      let k = 0; const step = () => { if (++k >= n) resolve(); else requestAnimationFrame(step); };
      requestAnimationFrame(step);
    });
    await until(() => viewer.spacecraft.has('Chaser') && document.querySelectorAll('#focus-select option').length > 0, 'the scenario to load');

    // the page's default layout mints its own viewports (icrf, ric, globe); the proof is three
    // views in total -- the primary and two extra viewports -- so those are removed first
    out.removedViewports = viewer.viewportList();
    for (const id of viewer.viewportList()) viewer.removeViewport(id);
    // viewport A is minted the way the layout manager's "3D Viewport" pane type mints it
    // (app.js mintViewport: own canvas, own Viewport, the real dblclick -> viewer.pick handler)
    const factory = window.altavistaLayoutManager && window.altavistaLayoutManager.panelFactories && window.altavistaLayoutManager.panelFactories.viewport;
    out.hasFactory = typeof factory === 'function';
    if (!out.hasFactory) { out.error = 'no 3D-viewport pane factory on window.altavistaLayoutManager'; return out; }
    const wrapper = factory('A');
    wrapper.style.cssText = 'position:fixed;left:0;top:0;width:300px;height:500px;z-index:-1';
    const canvasA = wrapper.querySelector('canvas');
    canvasA.style.cssText = 'display:block;width:300px;height:500px';
    document.body.appendChild(wrapper);
    viewer.resize();
    const A = viewer.viewports.get('A');
    A.controls.enableDamping = false;
    const cB = document.createElement('canvas');
    cB.style.cssText = 'position:fixed;left:300px;top:0;width:400px;height:300px;z-index:-1';
    document.body.appendChild(cB);
    const B = viewer.addViewport('B', cB);
    B.controls.enableDamping = false;
    await frames(3);
    viewer.resize();
    const rectA = canvasA.getBoundingClientRect();
    out.env = {
      userAgent: navigator.userAgent, viewports: viewer.viewportList(), devicePixelRatio: window.devicePixelRatio,
      aRect: [rectA.left, rectA.top, rectA.width, rectA.height], aAspect: A.camera.aspect,
      aCss: [canvasA.clientWidth, canvasA.clientHeight],
    };

    const settleScenario = async (id) => {
      viewer.setScenario(SC[id]);
      await until(() => { const e = viewer._entityModelEntities.get('Chaser'); return !!(e && e.modelLoaded); }, 'the Chaser model to load');
      await until(() => viewer._entityMarkerResidentNames.length === 2 && viewer._entityTrailLines.size === 2, 'the markers and trails to be admitted');
      await frames(3);
    };

    // record what the page's double-click handler asks for, without moving the camera
    const focusCalls = [];
    const origFocus = viewer.setViewportFocus.bind(viewer);
    const origPick = viewer.pick.bind(viewer);
    const pickResults = [];
    const spyOn = () => {
      viewer.setViewportFocus = (id, name) => { focusCalls.push([id, name]); };
      viewer.pick = (camera, x, y) => { const r = origPick(camera, x, y); pickResults.push(r); return r; };
    };
    const spyOff = () => { viewer.setViewportFocus = origFocus; viewer.pick = origPick; };
    const dbl = (canvas, ndc) => {
      const r = canvas.getBoundingClientRect();
      const ev = new MouseEvent('dblclick', {
        bubbles: true, cancelable: true,
        clientX: r.left + (ndc[0] + 1) / 2 * r.width, clientY: r.top + (1 - ndc[1]) / 2 * r.height,
      });
      focusCalls.length = 0; pickResults.length = 0;
      // the scene graph and the placed view, immediately before and after this ONE click
      // (the baseline is the state between ticks: the primary placed)
      viewer._placeSharedEntities(null);
      const stateBefore = P.sceneState(viewer), placedBefore = viewer._placedView;
      canvas.dispatchEvent(ev);
      const changed = P.firstDifference(stateBefore, P.sceneState(viewer)) !== null || viewer._placedView !== placedBefore;
      return { calls: focusCalls.slice(), pick: pickResults.slice(), changed };
    };

    for (const D of M.DISTANCES) {
      await settleScenario(D.id);
      for (const mode of ['fit', 'framed']) {
        // all synchronous from here to the end of the run: the page's own frame loop cannot interleave
        M.arrange(viewer, A, B, mode);
        M.runTicks(viewer);
        const key = D.id + '/' + mode;
        const r = P.pickingRun(viewer, A, B, mode);
        const checks = [];
        P.judge(key, r, (name, pass, detail) => checks.push({ name, pass: !!pass, detail }));
        const dA = A.camera.getWorldPosition(new THREE.Vector3())
          .distanceTo(viewer.spacecraft.get('Chaser').interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(1e-3)) * M.SCENE_UNIT_M;
        out.runs[key] = { run: r, checks, chaserDistanceFromViewportA_m: dA };

        if (mode === 'fit') {
          // the page's own double-click handler, in viewport A's real canvas
          const proof = P.modelPickProof(viewer, A);
          spyOn();
          const inside = proof.inside.points.map((ndc) => ({ ndc, ...dbl(canvasA, ndc) }));
          const off = proof.off.points.map((ndc) => ({ ndc, ...dbl(canvasA, ndc) }));
          spyOff();
          const isChaser = (c) => c.pick.length === 1 && c.pick[0] && c.pick[0].kind === 'spacecraft' && c.pick[0].name === 'Chaser'
            && c.calls.length === 1 && c.calls[0][0] === 'A' && c.calls[0][1] === 'Chaser';
          out.clicks[D.id] = {
            changedSceneGraph: inside.concat(off).filter((c) => c.changed).length, clicks: inside.length + off.length,
            inside: { total: inside.length, focusedChaser: inside.filter(isChaser).length, failed: inside.filter((c) => !isChaser(c)).slice(0, 3) },
            off: { total: off.length, focusedChaser: off.filter(isChaser).length, widths: proof.off.widths, failed: off.filter(isChaser).slice(0, 3) },
          };
        }
      }
    }
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def _probe_js() -> str:
    scenarios = {name: _scenario(radius) for name, radius in DISTANCES}
    return _PROBE_JS.replace("__SCENARIOS__", json.dumps(scenarios))


def test_picking_in_an_extra_viewport_uses_that_viewports_placement(live_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, v = asyncio.run(_drive(live_server.url, chrome_path, _probe_js(), wait_s=4.0))

    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v
    print("\nenvironment:", json.dumps(v["env"], sort_keys=True), "viewports removed first:", v["removedViewports"])
    assert sorted(v["env"]["viewports"]) == ["A", "B"], v["env"]["viewports"]
    # the pane's camera aspect is the canvas's, so a click's pixel maps to the camera's NDC
    r = v["env"]["aRect"]
    assert abs(v["env"]["aAspect"] - r[2] / r[3]) < 1e-9, v["env"]

    total = passed = 0
    failures: list[str] = []
    for key, run in v["runs"].items():
        s = run["run"]
        print(f"{key}: A {s['A']['inside']['ok']}/{s['A']['inside']['total']} inside, "
              f"{s['A']['off']['ok']}/{s['A']['off']['total']} off ({s['A']['off']['widths']} widths), "
              f"B marker {s['B']['ok']}/{s['B']['total']}, "
              + (f"primary {s['primary']['inside']['ok']}/{s['primary']['inside']['total']} inside, "
                 f"{s['primary']['off']['ok']}/{s['primary']['off']['total']} off, " if s["primary"] else "")
              + f"legacy agree {s['legacy']['agree']}/{s['legacy']['legacyHits']}, "
              f"scene graph unchanged: {s['unchanged'] is None}, "
              f"pick cost A median {s['cost']['A']['median']:.3f} ms (p95 {s['cost']['A']['p95']:.3f}), "
              f"primary median {s['cost']['primary']['median']:.3f} ms")
        assert 2.0 < run["chaserDistanceFromViewportA_m"] < 40.0, (key, run["chaserDistanceFromViewportA_m"])
        for c in run["checks"]:
            total += 1
            passed += bool(c["pass"])
            if not c["pass"]:
                failures.append(f"{c['name']}: {json.dumps(c['detail'])[:300]}")
    print(f"real-Chrome viewer.pick checks: {passed}/{total} passed")

    for distance, _radius in DISTANCES:
        c = v["clicks"][distance]
        print(f"{distance}: dblclick on the model in viewport A's canvas: {c['inside']['focusedChaser']}/{c['inside']['total']} focus the Chaser; "
              f"{c['off']['focusedChaser']}/{c['off']['total']} of the clicks {c['off']['widths']} model-widths off do; "
              f"{c['changedSceneGraph']}/{c['clicks']} single clicks changed the scene graph or the placed view")
        if c["changedSceneGraph"] != 0 or c["clicks"] < 18:
            failures.append(f"{distance}: {c['changedSceneGraph']}/{c['clicks']} single dblclicks changed the scene graph or _placedView")
        if not (c["inside"]["total"] == 13 and c["inside"]["focusedChaser"] == 13):
            failures.append(f"{distance}: dblclick on the model focuses the Chaser {c['inside']['focusedChaser']}/{c['inside']['total']}")
        if not (c["off"]["total"] >= 4 and c["off"]["focusedChaser"] == 0):
            failures.append(f"{distance}: dblclick off the model focuses the Chaser {c['off']['focusedChaser']}/{c['off']['total']}")

    assert failures == [], "\n".join(failures)
    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

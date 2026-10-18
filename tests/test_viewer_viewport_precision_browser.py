"""Question 240 (defect 3 of the heavy carry-over round; question 252, the heavy precision round):
every viewport draws the shared entity objects relative to ITS OWN floating origin, proven in a
REAL headless Chrome against a REAL `python -m altavista serve`.

The defect: the entity objects (the spacecraft marker spheres, the instanced entity markers, the
trail lines, the covariance ellipsoids and keep-out volumes, the glTF model, the event meshes) are
one set of objects drawn by every viewport's renderer, and they used to be written once per tick
relative to the PRIMARY camera's floating origin (`Math.fround` in `_toLocal`, Float32 instance
matrices and trail buffers). A second viewport framed ~10 m from the Chaser, while the primary's
origin was elsewhere (after `fit()` it is at the Earth), drew the model 0.04-0.25 m off at LEO,
metres off at GEO and tens of metres off at lunar distance, hopping between ticks.

What this measures (the same kernel as the node check, web/js/entities_viewport_precision_check.mjs,
imported into the page): for each of three viewports (the primary and two extra ones, each with its
own WebGLRenderer), right after that viewport's REAL renderer has drawn (the page's
`renderer.render` is wrapped; the real `viewer.update(t)` drives the ticks), the final float32 vertex
position in that viewport's own eye frame -- the object's `modelViewMatrix` that three set for that
draw, rounded to float32, applied to the object's float32 vertex data with every multiply and add
rounded to float32 -- against the float64 truth (the spacecraft's interpolator position, plus for
a model vertex its node chain, the entity's attitude and the scale, into the same camera's f64
`matrixWorldInverse`). Per (viewport, class) the maximum error and the maximum tick-to-tick hop
of the error vector, at three orbit radii (LEO 7,000 km, GEO 42,164 km, lunar 384,400 km), with the
primary measured twice: after `fit()` (origin at the frame origin, the worst case for the extra
viewports) and framed on the Chaser. Viewport A is portrait and Focus on the Chaser, viewport B is
landscape and Focus on the Target. Every class is on.

Also measured here: the frame time of `viewer.update(t)` with three viewports (median, p95 over
300 ticks), the cost of the placement itself per viewport, the GL buffer uploads per context per
tick, and the console (zero errors, warnings and exceptions).

The scenario is the RPO geometry of examples/05_rpo_ric.py in closed form (no GMAT needed), built
through the repo's own `ScenarioData` so the wire shape is the producer's. The harness helpers
(`_find_chrome`/`_free_port`/`_LiveServer`/`_drive`) are tests/test_viewer_entity_framing_browser.py's
own, duplicated per this repo's convention.
"""
from __future__ import annotations

import asyncio
import json
import math
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
sys.path.insert(0, str(REPO_ROOT))

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
]

SCENARIO_NAME = "Viewport precision"
MU_EARTH_KM3_S2 = 398600.4418
INCLINATION_DEG = 51.6
EPOCH_A1MJD = 34000.0
# Five-second samples: the trail's newest vertex is the sample nearest the clock, so the finer
# the sampling the nearer that vertex is to every viewport's focus (a float32 vertex that is
# 200 km from a viewport's origin cannot be better than ~7 mm whatever the placement does).
STEP_S = 5.0
N_SAMPLES = 400
CHASER_LEAD_M = 30.0
MODEL_URL = "/js/fixtures/entity_model_fixture.gltf"
COV_DIAG_KM2 = [1.0, 0.25, 0.0625]
DISTANCES = (("LEO", 7000.0), ("GEO", 42164.0), ("lunar", 384400.0))
CLASSES = ("sphere", "marker", "trail", "ellipsoid", "keepout", "model", "event")
TICKS = 24
CENTIMETRE_M = 0.01
FRAME_BUDGET_MS = 1000.0 / 60.0   # docs/heavy-plan.md's H5b-2 budget: 16.7 ms
TIMING_TICKS = 300


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
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
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


# The page script. `__SCENARIOS__` is replaced by the three scenarios (JSON); everything else
# is read off the live scene graph and the real renderers.
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start', runs: {} };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three');
    const M = await import('/js/entities_viewport_precision_check.mjs');
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

    // the page's default layout mints its own viewports (icrf, ric, globe); the measurement is
    // three views in total -- the primary and two extra viewports -- so those are removed first
    out.removedViewports = viewer.viewportList();
    for (const id of viewer.viewportList()) viewer.removeViewport(id);
    // two extra viewports: their own canvases and WebGLRenderers
    const mk = (id, w, h) => {
      const c = document.createElement('canvas');
      c.style.cssText = `position:fixed;left:0;top:0;width:${w}px;height:${h}px;z-index:-1`;
      document.body.appendChild(c);
      const vp = viewer.addViewport(id, c);
      vp.controls.enableDamping = false;
      return vp;
    };
    const A = mk('A', 300, 500);   // portrait; Focus on the Chaser
    const B = mk('B', 400, 300);   // landscape; Focus on the Target
    await frames(3);
    const hasPlacement = typeof viewer._placeSharedEntities === 'function';
    out.env = {
      userAgent: navigator.userAgent, hasPlacement,
      primaryCanvasCss: [viewer.canvas.clientWidth, viewer.canvas.clientHeight], primaryAspect: viewer.camera.aspect,
      viewports: viewer.viewportList(), devicePixelRatio: window.devicePixelRatio,
      aCss: [A.canvas.clientWidth, A.canvas.clientHeight], bCss: [B.canvas.clientWidth, B.canvas.clientHeight],
    };

    const settleScenario = async (id) => {
      viewer.setScenario(SC[id]);
      await until(() => { const e = viewer._entityModelEntities.get('Chaser'); return !!(e && e.modelLoaded); }, 'the Chaser model to load');
      await until(() => viewer._entityMarkerResidentNames.length === 2 && viewer._entityTrailLines.size === 2, 'the markers and trails to be admitted');
      await frames(3);
    };

    // ------------------------------------------------------------- precision, 3 radii x 2 primary modes
    for (const D of M.DISTANCES) {
      await settleScenario(D.id);
      for (const mode of ['fit', 'framed']) {
        // all synchronous from here to the end of the run: the page's own frame loop cannot interleave
        M.arrange(viewer, A, B, mode);
        M.disableCulling(viewer);
        const rec = new M.Recorder();
        const undo = M.instrumentViewer(viewer, rec);
        M.runTicks(viewer);
        undo();
        const chaser = viewer.spacecraft.get('Chaser').interp.at(viewer._lastT, new THREE.Vector3()).multiplyScalar(1e-3);
        const oA = A.floatingOrigin.getOrigin(viewer._originFrameId), oP = viewer.floatingOrigin.getOrigin(viewer._originFrameId);
        out.runs[D.id + '/' + mode] = {
          summary: rec.summary(),
          identity: M.primaryIdentity(viewer),
          placedForPrimary: viewer._placedView === null,
          options: { ...viewer.entityOptions },
          geometry: {
            chaserDistanceFromViewportA_m: A.camera.getWorldPosition(new THREE.Vector3()).distanceTo(chaser) * M.SCENE_UNIT_M,
            originDifferenceAvsPrimary: Math.hypot(oA.x - oP.x, oA.y - oP.y, oA.z - oP.z),
            viewportARenderShift: A.renderGroup.position.length(),
          },
        };
      }
    }

    // ------------------------------------------------------------- frame time, 3 viewports (LEO, primary after fit())
    await settleScenario('LEO');
    M.arrange(viewer, A, B, 'fit');
    M.disableCulling(viewer);
    const renderers = [viewer.renderer, A.renderer, B.renderer];
    const tickAt = (k) => viewer.update(M.T0_DAYS + (k * M.DT_S) / 86400);
    const stat = (ts) => {
      const s = [...ts].sort((a, b) => a - b);
      return { n: s.length, median: s[s.length >> 1], p95: s[Math.floor(s.length * 0.95)], max: s[s.length - 1],
               mean: s.reduce((a, b) => a + b, 0) / s.length };
    };
    const run = (n, fn) => { const ts = []; for (let k = 0; k < n; k++) { const a = performance.now(); fn(k); ts.push(performance.now() - a); } return ts; };
    const WARM = 40, N = __TIMING_TICKS__;
    out.timing = {};
    // the CPU side of a tick with the GL work removed: the renders replaced by three's own
    // prologue (world matrices) and the scene's onBeforeRender hook -- where the placement runs
    const origRender = renderers.map((r) => r.render);
    renderers.forEach((r) => {
      r.render = (scene, camera) => {
        if (scene.matrixWorldAutoUpdate === true) scene.updateMatrixWorld();
        if (camera.parent === null && camera.matrixWorldAutoUpdate === true) camera.updateMatrixWorld();
        if (scene.isScene === true) scene.onBeforeRender(r, scene, camera, null);
      };
    });
    for (let k = 0; k < WARM; k++) tickAt(k);
    out.timing.updateNoGL = stat(run(N, (k) => tickAt(WARM + k)));
    // the placement's own cost: wrap the placement / per-tick entity methods and time them over N ticks
    const acc = {};
    const wrap = (name, keyOf) => {
      const orig = viewer[name];
      if (typeof orig !== 'function') return () => {};
      viewer[name] = function (...args) {
        const a = performance.now();
        const r = orig.apply(this, args);
        const key = keyOf ? keyOf(args) : name;
        acc[key] = (acc[key] || 0) + (performance.now() - a);
        return r;
      };
      return () => { viewer[name] = orig; };
    };
    const vpName = (args) => '_placeSharedEntities:' + (args[0] ? args[0].id : 'primary');
    const unwraps = [
      wrap('_placeSharedEntities', vpName), wrap('_updateEntities'),
      // the pre-fix per-tick path (absent after the fix)
      wrap('_updateEntityMarkers'), wrap('_updateEntityTrails'), wrap('_updateEntityCovariance'), wrap('_updateEntityModels'),
    ];
    for (let k = 0; k < N; k++) tickAt(WARM + k);
    unwraps.forEach((u) => u());
    out.timing.placementMsPerTick = Object.fromEntries(Object.entries(acc).map(([k, v]) => [k, v / N]));
    renderers.forEach((r, i) => { r.render = origRender[i]; });
    // the real renders: the tick as the page runs it (the GL commands are queued ...)
    for (let k = 0; k < WARM; k++) tickAt(k);
    out.timing.update = stat(run(N, (k) => tickAt(WARM + k)));
    // ... and with the GPU drained after every tick (what the software rasterizer really costs)
    out.timing.updateFinish = stat(run(N, (k) => { tickAt(WARM + N + k); for (const r of renderers) r.getContext().finish(); }));

    // GL buffer uploads per context per tick (real renderers)
    {
      const counts = renderers.map(() => ({ bufferSubData: 0, bufferData: 0 }));
      const restore = renderers.map((r, i) => {
        const gl = r.getContext();
        const sub = gl.bufferSubData, dat = gl.bufferData;
        gl.bufferSubData = function (...a) { counts[i].bufferSubData += 1; return sub.apply(this, a); };
        gl.bufferData = function (...a) { counts[i].bufferData += 1; return dat.apply(this, a); };
        return () => { delete gl.bufferSubData; delete gl.bufferData; };
      });
      const K = 20;
      for (let k = 0; k < K; k++) tickAt(WARM + k);
      restore.forEach((f) => f());
      out.uploadsPerTick = { primary: counts[0], A: counts[1], B: counts[2], ticks: K };
      for (const c of counts) { c.bufferSubData /= K; c.bufferData /= K; }
    }
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def _probe_js(timing_ticks: int = TIMING_TICKS) -> str:
    scenarios = {name: _scenario(radius) for name, radius in DISTANCES}
    return (_PROBE_JS.replace("__SCENARIOS__", json.dumps(scenarios))
            .replace("__TIMING_TICKS__", str(timing_ticks)))


def _table(runs: dict, views=("primary", "A", "B")) -> str:
    lines = []
    for key, run in runs.items():
        g = run["geometry"]
        lines.append(f"== {key}  (viewport A {g['chaserDistanceFromViewportA_m']:.1f} m from the Chaser; "
                     f"|O_A - O_primary| = {g['originDifferenceAvsPrimary']:.3f} scene units)")
        lines.append("view     " + "".join(f"{c:>20s}" for c in CLASSES) + "     (max error m / max hop m)")
        for view in views:
            row = f"{view:8s} "
            for cls in CLASSES:
                s = run["summary"].get(view, {}).get(cls)
                row += f"{(f'{s['maxErr']:.2e}/{s['maxHop']:.2e}' if s else '-'):>20s}"
            lines.append(row)
    return "\n".join(lines)


def _load() -> tuple[float, float, float]:
    return os.getloadavg()


def test_every_viewport_draws_the_shared_entity_objects_relative_to_its_own_origin(live_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    load_before = _load()
    errors, v = asyncio.run(_drive(live_server.url, chrome_path, _probe_js(), wait_s=4.0))
    load_after = _load()

    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v

    print("\nenvironment:", json.dumps(v["env"], sort_keys=True), "viewports removed first:", v["removedViewports"])
    print("host load average before/after:", [round(x, 2) for x in load_before], [round(x, 2) for x in load_after])
    print("\n" + _table(v["runs"]))
    print("\nframe time of viewer.update(t), three viewports (ms):", json.dumps(v["timing"], indent=1, sort_keys=True))
    print("GL buffer uploads per context per tick:", json.dumps(v["uploadsPerTick"], sort_keys=True))
    print("console/exception messages:", errors)

    assert v["env"]["hasPlacement"] is True, "scene.js has no _placeSharedEntities: the fix is not in this tree"
    assert sorted(v["env"]["viewports"]) == ["A", "B"], v["env"]["viewports"]

    # ------------------------------------------------ question 46's centimetre bound, every viewport, every class
    for distance, _radius in DISTANCES:
        for mode in ("fit", "framed"):
            key = f"{distance}/{mode}"
            run = v["runs"][key]
            g = run["geometry"]
            # the case is real: A is framed on the 1.5 m model, and after fit() its origin is far from the primary's
            assert 2.0 < g["chaserDistanceFromViewportA_m"] < 40.0, (key, g)
            if mode == "fit":
                assert g["originDifferenceAvsPrimary"] > 1.0, (key, g)
            assert all(run["options"].values()), (key, run["options"])  # every entity class on
            views = ("A", "B") if mode == "fit" else ("primary", "A", "B")
            for view in views:
                for cls in CLASSES:
                    s = run["summary"][view][cls]
                    assert s["n"] >= TICKS and s["skipped"] == 0, f"{key} {view}/{cls} was not measured on every tick: {s}"
                    assert s["maxErr"] <= CENTIMETRE_M, (
                        f"{key}: viewport {view} draws the {cls} {s['maxErr']:.4g} m off its true position (> 1 cm)\n" + _table({key: run}))
                    assert s["maxHop"] <= CENTIMETRE_M, (
                        f"{key}: viewport {view}'s {cls} hops {s['maxHop']:.4g} m between ticks (> 1 cm)\n" + _table({key: run}))
            # the primary's placement is exactly `_toLocal`'s value (bit for bit), and the viewport loop restored it
            assert run["identity"] == [], (key, run["identity"][:5])
            assert run["placedForPrimary"] is True, key

    # ------------------------------------------------ the frame-time budget on what this change adds
    t = v["timing"]
    place = v["timing"]["placementMsPerTick"]
    placement_total = sum(ms for name, ms in place.items() if name.startswith("_placeSharedEntities:"))
    # all three views are placed every tick; the cost of placing for one view is a small fraction of a frame
    assert {"_placeSharedEntities:primary", "_placeSharedEntities:A", "_placeSharedEntities:B"} <= set(place), place
    assert placement_total < 0.1 * FRAME_BUDGET_MS, (
        f"placing the shared entities for three views costs {placement_total:.3f} ms per tick (> 10 % of 16.7 ms): {place}")
    assert t["updateNoGL"]["p95"] < FRAME_BUDGET_MS, (
        f"update(t) without the GL work (placement included) misses the 16.7 ms budget: {t['updateNoGL']}")

    # each GL context uploads the dynamic entity buffers no more than once per tick, as before
    # (three's WebGLAttributes compares the attribute's version with the version uploaded to THAT context)
    up = v["uploadsPerTick"]
    for view in ("primary", "A", "B"):
        assert up[view]["bufferSubData"] <= 3.0, (view, up)

    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

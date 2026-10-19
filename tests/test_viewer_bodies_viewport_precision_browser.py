"""Question 254 item 4 (heavy open-items round): every viewport draws the bodies, the tiled globe and
the body-fixed frame relative to ITS OWN floating origin, proven in a REAL headless Chrome against a
REAL `python -m altavista serve`. The bodies half of tests/test_viewer_viewport_precision_browser.py
(question 252, the shared entity objects).

The defect: the body meshes, their sub-pixel dots, the tiled globe's group and the body-fixed frame
the 3D Tiles overlay hangs from are one set of objects drawn by every viewport's renderer, and they
used to be written once per tick relative to the PRIMARY camera's floating origin (`Math.fround` in
`_toLocal`). An extra viewport framed ~10 m from a spacecraft near a body drew the body where the
primary's rounding put it; one shared dot was sized for the primary's distance; and the body-fixed
frame was parented under the frame node, so it missed the render shift altogether (7,000 km off the
body at LEO whenever the primary's origin sat at a LEO spacecraft).

What this measures (the kernel of web/js/bodies_viewport_precision_check.mjs imported into the page):
for each of three viewports (the primary and two extra ones, each with its own WebGLRenderer),
right after that viewport's REAL renderer has drawn, the final float32 vertex position in that
viewport's own eye frame (the object's `modelViewMatrix` that three set for that draw, rounded to
float32, applied to the float32 vertex data with every multiply and add rounded) against the
float64 truth, split into the total, what the placement alone got wrong (the same placement in f64)
and the GPU's float32 floor (the exact placement through the same float32 pipeline). Classes: the
body nearest the framed spacecraft (centre and the vertices nearest the camera), a 10 km body 100 km
from it, a sub-pixel dot (Mars), the globe's tile vertices (`enableGlobe` on the nearest body) and
probe meshes in the body-fixed frame; at LEO 7,000 km, GEO 42,164 km and a low lunar orbit (1,800 km
from the Moon's centre), the primary measured after `fit()` and framed on the Chaser, the globe off
and on. Also here: the per-view dot decisions, picking a body in the extra viewports, the cost of
the placement per view, and the console (zero errors, warnings and exceptions).

The harness helpers (`_find_chrome`/`_free_port`/`_LiveServer`/`_drive`) are
tests/test_viewer_viewport_precision_browser.py's own, duplicated per this repo's convention.
`AV_BODIES_DUMP=<path>` writes the page's whole result as JSON (a diagnostic for comparing two trees).
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

SCENES = ("LEO", "GEO", "lunar")
MODES = ("fit", "framed")
CONFIGS = ("plain", "globe")
TICKS = 24
CENTIMETRE_M = 0.01
PLACEMENT_BOUND_M = 1e-3
FRAME_BUDGET_MS = 1000.0 / 60.0
TIMING_TICKS = 300
CLASSES_PLAIN = ("bodyCentre", "bodyVertex", "rockCentre", "dot:Mars", "bodyFixedOrigin", "bodyFixedSurface")
CLASSES_GLOBE = ("globe", "rockCentre", "dot:Mars", "bodyFixedOrigin", "bodyFixedSurface")


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


def _py_wire_keys() -> tuple[set[str], set[str]]:
    """The producer's own wire keys for a body and a spacecraft (`BodyTrack.to_dict`,
    `Trajectory.to_dict`): the scenes the page builds in JS must have the same shape."""
    from altavista.model import BodyTrack, Trajectory

    body = BodyTrack(name="B", radius=1.0, t=[0.0, 1.0], pos=[[0, 0, 0], [0, 0, 0]], quat=[[0, 0, 0, 1], [0, 0, 0, 1]])
    sc = Trajectory(name="S", t=[0.0], pos=[[7000.0, 0, 0]], vel=[[0, 7.5, 0]], model="/m.gltf",
                    cov=[[1, 0, 0, 0, 1, 0, 0, 0, 1]], cov_dim=3)
    return set(body.to_dict()), set(sc.to_dict())


def _seed_scenario() -> dict:
    """A small LEO scene built through the repo's own `ScenarioData`, published so the page loads;
    the page then loads the three measured scenes itself."""
    from altavista.model import BodyTrack, Frame, ScenarioData, Trajectory

    n = math.sqrt(398600.4418 / 7000.0 ** 3)
    ts = [34000.0 + 5.0 * k / 86400.0 for k in range(8)]
    pos = [[7000 * math.cos(n * 5 * k), 7000 * math.sin(n * 5 * k), 0.0] for k in range(8)]
    vel = [[-7000 * n * math.sin(n * 5 * k), 7000 * n * math.cos(n * 5 * k), 0.0] for k in range(8)]
    earth = BodyTrack(name="Earth", radius=6378.137, flattening=1 / 298.257223563, color="#2e6fbd", central=True,
                      t=[ts[0], ts[-1]], pos=[[0, 0, 0], [0, 0, 0]], quat=[[0, 0, 0, 1], [0, 0, 0, 1]])
    data = ScenarioData(
        name="Bodies viewport precision seed", frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
        spacecraft=[Trajectory(name="Chaser", t=ts, pos=pos, vel=vel, color="#ff6b6b"),
                    Trajectory(name="Target", t=ts, pos=pos, vel=vel, color="#54a0ff")],
        bodies=[earth], frames=[{"id": "EarthMJ2000Eq", "axes": "AXES_KIND_MJ2000_EQ", "body": "Earth"}],
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
        _http_post_json(server.url.rstrip("/") + "/api/scenario", _seed_scenario())
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the scenario: {exc}")
    try:
        yield server
    finally:
        server.stop()


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 4.0, eval_timeout_s: float = 900.0):
    """tests/test_viewer_viewport_precision_browser.py's `_drive` (all three CDP error channels:
    Log.entryAdded errors, console error/warning, Runtime.exceptionThrown)."""
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


# The page script. `__TIMING_TICKS__` is replaced; everything else is read off the live scene graph and
# the real renderers.
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start', runs: {}, picking: {}, timing: {} };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three');
    const E = await import('/js/entities_viewport_precision_check.mjs');
    const M = await import('/js/bodies_viewport_precision_check.mjs');
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

    // the page's default layout mints its own viewports; the measurement is three views in total
    out.removedViewports = viewer.viewportList();
    for (const id of viewer.viewportList()) viewer.removeViewport(id);
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
    out.env = {
      userAgent: navigator.userAgent, hasPlaceBodies: typeof viewer._placeBodies === 'function',
      primaryCanvasCss: [viewer.canvas.clientWidth, viewer.canvas.clientHeight],
      viewports: viewer.viewportList(), devicePixelRatio: window.devicePixelRatio,
      aCss: [A.canvas.clientWidth, A.canvas.clientHeight], bCss: [B.canvas.clientWidth, B.canvas.clientHeight],
    };
    const sc0 = M.bodiesScenario('LEO');
    out.wire = { bodyKeys: Object.keys(sc0.bodies[0]).sort(), spacecraftKeys: Object.keys(sc0.spacecraft[1]).sort() };

    const settleScenario = async (id) => {
      viewer.setScenario(M.bodiesScenario(id));
      await until(() => { const e = viewer._entityModelEntities.get('Chaser'); return !!(e && e.modelLoaded); }, 'the Chaser model to load');
      await until(() => viewer._entityMarkerResidentNames.length === 2 && viewer._entityTrailLines.size === 2, 'the markers and trails to be admitted');
      await frames(3);
    };

    // ------------------------------------------------------------- precision: 3 scenes x 2 primary modes x globe off/on
    for (const S of M.SCENES) {
      await settleScenario(S.id);
      const near = M.NEAR_BODY[S.id];
      viewer.update(E.T0_DAYS);
      const probes = M.attachOverlayProbes(viewer, near, 'Chaser');
      for (const mode of M.MODES) {
        for (const config of M.CONFIGS) {
          // all synchronous from here to the end of the run: the page's own frame loop cannot interleave
          const r = M.measureRun(viewer, A, B, S.id, mode, config, probes);
          r.identity = M.primaryBodiesIdentity(viewer, {});
          r.placedForPrimary = viewer._placedView === null;
          out.runs[`${S.id}/${mode}/${config}`] = r;
        }
      }
      // picking a body in the extra viewports (the globe off: the sphere is the body's mesh)
      for (const mode of M.MODES) {
        E.arrange(viewer, A, B, mode);
        viewer.disableGlobe();
        M.disableBodyCulling(viewer);
        E.runTicks(viewer, 3);
        for (const [label, view] of [['A', A], ['B', B]]) out.picking[`${S.id}/${mode}/${label}`] = M.bodyPickProof(viewer, view, near);
      }
    }

    // ------------------------------------------------------------- the cost of the placement (LEO, the Earth's globe on, the overlay's frame, 3 views)
    await settleScenario('LEO');
    viewer.update(E.T0_DAYS);
    const probes = M.attachOverlayProbes(viewer, 'Earth', 'Chaser');
    E.arrange(viewer, A, B, 'framed'); // the primary close to the Earth: the finest tile selection (most tiles)
    M.enableGlobeForMeasurement(viewer, 'Earth');
    E.runTicks(viewer, 6);
    out.timing.tiles = viewer.globeLayer._meshes.size;
    const renderers = [viewer.renderer, A.renderer, B.renderer];
    const tickAt = (k) => viewer.update(E.T0_DAYS + (k * E.DT_S) / 86400);
    const stat = (ts) => {
      const s = [...ts].sort((a, b) => a - b);
      return { n: s.length, median: s[s.length >> 1], p95: s[Math.floor(s.length * 0.95)], max: s[s.length - 1] };
    };
    const run = (n, fn) => { const ts = []; for (let k = 0; k < n; k++) { const a = performance.now(); fn(k); ts.push(performance.now() - a); } return ts; };
    // one call per tick per view, as the viewer makes them: a new epoch, then the placement.
    // performance.now() is coarsened to 0.1 ms in a page, so each sample is the mean of a batch of 200 calls.
    const batchStat = (fn, batches = 60, batch = 200) => {
      for (let i = 0; i < 40; i++) fn();
      const ts = [];
      for (let i = 0; i < batches; i++) { const a = performance.now(); for (let k = 0; k < batch; k++) fn(); ts.push((performance.now() - a) / batch); }
      return stat(ts);
    };
    out.timing.placeSharedEntities = {};
    out.timing.placeBodies = {};
    for (const [name, view] of [['primary', null], ['A', A], ['B', B]]) {
      out.timing.placeSharedEntities[name] = batchStat(() => { viewer._placeEpoch++; viewer._placeSharedEntities(view); });
      if (typeof viewer._placeBodies === 'function') out.timing.placeBodies[name] = batchStat(() => { viewer._placeEpoch++; viewer._placeBodies(view); });
    }
    // update(t) with the GL work removed (three's prologue and the scene's onBeforeRender hook stay)
    const origRender = renderers.map((r) => r.render);
    renderers.forEach((r) => {
      r.render = (scene, camera) => {
        if (scene.matrixWorldAutoUpdate === true) scene.updateMatrixWorld();
        if (camera.parent === null && camera.matrixWorldAutoUpdate === true) camera.updateMatrixWorld();
        if (scene.isScene === true) scene.onBeforeRender(r, scene, camera, null);
      };
    });
    const WARM = 40, N = __TIMING_TICKS__;
    for (let k = 0; k < WARM; k++) tickAt(k);
    out.timing.updateNoGL = stat(run(N, (k) => tickAt(WARM + k)));
    renderers.forEach((r, i) => { r.render = origRender[i]; });
    for (let k = 0; k < WARM; k++) tickAt(k);
    out.timing.update = stat(run(N, (k) => tickAt(WARM + k)));
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def _probe_js(timing_ticks: int = TIMING_TICKS) -> str:
    return _PROBE_JS.replace("__TIMING_TICKS__", str(timing_ticks))


def _fmt(s: dict | None) -> str:
    return f"{s['maxErr']:.2e}/{s['maxHop']:.2e}" if s else "-"


def _table(runs: dict, views=("primary", "A", "B")) -> str:
    lines = []
    for key, run in runs.items():
        g = run["geometry"]
        lines.append(f"== {key}  (viewport A {g['chaserDistanceFromViewportA_m']:.1f} m from the Chaser; "
                     f"|O_A - O_primary| = {g['originDifferenceAvsPrimary']:.3f} scene units; {g['tiles']} globe tiles)")
        classes = CLASSES_GLOBE if key.endswith("/globe") else CLASSES_PLAIN
        lines.append("view     " + "".join(f"{c:>34s}" for c in classes)
                     + "     (on-screen max error m / max hop m  [placement-only error m | float32 floor m])")
        for view in views:
            row = f"{view:8s}"
            for cls in classes:
                t = run["total"].get(view, {}).get(cls)
                if t:
                    p = run["place"][view][cls]["maxErr"]
                    f = run["floor"][view][cls]["maxErr"]
                    row += f"{_fmt(t) + f' [{p:.0e}|{f:.0e}]':>34s}"
                else:
                    row += f"{'-':>34s}"
            lines.append(row)
    return "\n".join(lines)


def _load() -> tuple[float, float, float]:
    return os.getloadavg()


def test_every_viewport_draws_the_bodies_relative_to_its_own_origin(live_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    load_before = _load()
    errors, v = asyncio.run(_drive(live_server.url, chrome_path, _probe_js(), wait_s=4.0))
    load_after = _load()

    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v
    if os.environ.get("AV_BODIES_DUMP"):
        Path(os.environ["AV_BODIES_DUMP"]).write_text(json.dumps(v))

    print("\nenvironment:", json.dumps(v["env"], sort_keys=True), "viewports removed first:", v["removedViewports"])
    print("host load average before/after:", [round(x, 2) for x in load_before], [round(x, 2) for x in load_after])
    print("\n" + _table(v["runs"]))
    print("\npicking:", {k: (p["ok"], p["picks"], f"{p['maxHitErrM']:.1e} m") for k, p in v["picking"].items()})
    print("\ncost of the placement per view per tick (ms, mean of 200-call batches):",
          json.dumps({"placeSharedEntities": v["timing"]["placeSharedEntities"], "placeBodies": v["timing"]["placeBodies"],
                      "tiles": v["timing"]["tiles"]}, indent=1, sort_keys=True))
    print("frame time of viewer.update(t), three viewports, LEO, globe on (ms):",
          json.dumps({"updateNoGL": v["timing"]["updateNoGL"], "update": v["timing"]["update"]}, indent=1, sort_keys=True))
    print("console/exception messages:", errors)

    assert v["env"]["hasPlaceBodies"] is True, "scene.js has no _placeBodies: the change is not in this tree"
    assert sorted(v["env"]["viewports"]) == ["A", "B"], v["env"]["viewports"]
    py_body, py_spacecraft = _py_wire_keys()
    assert set(v["wire"]["bodyKeys"]) == py_body, (v["wire"]["bodyKeys"], sorted(py_body))
    assert set(v["wire"]["spacecraftKeys"]) <= py_spacecraft, (v["wire"]["spacecraftKeys"], sorted(py_spacecraft))

    # ------------------------------------------------ every run: measured, placed exactly, floor-limited
    for scene in SCENES:
        for mode in MODES:
            for config in CONFIGS:
                key = f"{scene}/{mode}/{config}"
                run = v["runs"][key]
                g = run["geometry"]
                assert 2.0 < g["chaserDistanceFromViewportA_m"] < 40.0, (key, g)
                if mode == "fit":
                    assert g["originDifferenceAvsPrimary"] > 1.0, (key, g)
                if config == "globe":
                    assert g["tiles"] >= 2, (key, g)
                classes = CLASSES_GLOBE if config == "globe" else CLASSES_PLAIN
                for view in ("primary", "A", "B"):
                    for cls in classes:
                        s = run["total"][view][cls]
                        assert s["n"] >= TICKS and s["skipped"] == 0, f"{key} {view}/{cls} was not measured on every tick: {s}"
                for view in ("A", "B"):
                    for cls in classes:
                        p = run["place"][view][cls]
                        assert p["maxErr"] <= PLACEMENT_BOUND_M and p["maxHop"] <= PLACEMENT_BOUND_M, (
                            f"{key}: viewport {view} places the {cls} {p['maxErr']:.4g} m off its true position\n" + _table({key: run}))
                        if not cls.startswith("dot:"):
                            t, f = run["total"][view][cls], run["floor"][view][cls]
                            assert t["maxErr"] <= 2 * f["maxErr"] + CENTIMETRE_M, (
                                f"{key}: {view}/{cls} on-screen {t['maxErr']:.4g} m vs the float32 floor {f['maxErr']:.4g} m\n" + _table({key: run}))
                    # a small near body is within question 46's centimetre bound in the extra viewports
                    r = run["total"][view]["rockCentre"]
                    assert r["maxErr"] <= CENTIMETRE_M and r["maxHop"] <= CENTIMETRE_M, (key, view, r)
                # the primary: bit-identical to `_toLocal`, the body-fixed frame where the body is, restored after the viewport loop
                assert run["identity"] == [], (key, run["identity"][:5])
                assert run["placedForPrimary"] is True, key
                # per-view dots: drawn exactly when the disc is under 1.5 px in that view, 1.5 px in radius there
                wrong = [d for d in run["decisions"] if not d["borderline"] and d["expectDot"] != d["drawnDot"]]
                assert wrong == [], (key, wrong[:3])
                for view in ("primary", "A", "B"):
                    for cls in ("dot:Mars", "dot:Debris"):
                        s = run["aux"].get(view, {}).get(f"{cls} radius-1.5 px")
                        if s:
                            assert s["maxErr"] <= 0.01, (key, view, cls, s)
                if mode == "fit" and config == "plain":
                    dd = lambda view: [d for d in run["decisions"] if d["view"] == view and d["body"] == "Debris"]
                    assert all(d["drawnDot"] for d in dd("primary")) and not any(d["drawnDot"] for d in dd("A")), (key, dd("primary")[:1], dd("A")[:1])

    # ------------------------------------------------ picking a body in an extra viewport
    for key, p in v["picking"].items():
        assert p["picks"] >= 8 and p["ok"] == p["picks"], (key, p)
        assert p["maxHitErrM"] <= PLACEMENT_BOUND_M and p["changed"] == [], (key, p)

    # ------------------------------------------------ the frame-time budget on what this change adds
    place = v["timing"]["placeSharedEntities"]
    bodies = v["timing"]["placeBodies"]
    assert set(place) == {"primary", "A", "B"} and set(bodies) == {"primary", "A", "B"}, (place, bodies)
    per_tick_total = sum(s["median"] for s in place.values())
    assert per_tick_total < 0.1 * FRAME_BUDGET_MS, (
        f"placing the shared entities and the bodies for three views costs {per_tick_total:.3f} ms per tick (> 10 % of 16.7 ms): {place}")
    assert v["timing"]["updateNoGL"]["p95"] < FRAME_BUDGET_MS, (
        f"update(t) without the GL work (placement included) misses the 16.7 ms budget: {v['timing']['updateNoGL']}")

    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

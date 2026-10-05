"""Cleanup round (questions 235/237): focusing a spacecraft FRAMES it.

The lead's browser drive of the entities (docs/teamlog/2026-09-02-team-1.md, 2026-09-22):
"a 3 km ellipsoid at the Earth-framed camera is a dot, and neither Focus on the spacecraft
nor Reset view brings the camera to a scale where it can be seen". This file makes that a
number and proves the fix in a REAL headless Chrome against a REAL `python -m altavista
serve`, driving the page the way a user does (the Layers panel's own checkbox, the Focus
dropdown's own `change` event, the spacecraft list's own "focus" link, the Reset view
button) and reading every answer off the live scene graph, never off a counter:

  - the scenario is a closed-form circular LEO orbit (7,000 km radius, 51.6 deg) carrying a
    closed-form diagonal covariance, so the covariance ellipsoid is exactly 3 km x 1.5 km x
    0.75 km (3 sigma of 1, 0.5, 0.25 km) at every sample -- no GMAT needed, which also keeps
    the proof runnable on a host without it. (The ellipsoid-from-a-real-run path is
    tests/test_viewer_entities_browser.py's job; this file is about the camera.)
  - BEFORE the action (the Earth-framed camera `fit()` leaves, with and without a click on
    Reset view) the ellipsoid's bounding sphere is under 3 px tall on the canvas;
  - AFTER Focus on the spacecraft its bounding sphere is 50-65 % of the canvas height (the
    stated fraction is 60 % of the narrower field of view, which projects to ~58 % at this
    camera's FOV), the sphere is entirely inside the camera's near/far, in front of the
    camera, and the floating origin has been moved onto the entity (camera and entity both
    within 0.05 scene units of the render origin -- question 46's float32 bound), and the
    camera FACES the entity (its forward axis within 0.001 degrees of the direction to it:
    before scene.js's `_updateEntitiesCamera`, with the floating origin on a LEO
    spacecraft the camera was aimed at the Earth's centre, 62 degrees off a target 1.5 km
    away -- OrbitControls aims at its local-space target as if it were a world point);
  - the camera never zooms by itself: ticks, a class toggled off and on, and the clock
    running leave the camera-to-target distance unchanged; only the next explicit Focus
    (the list's "focus" link) re-frames, to the new, smaller extent;
  - a focus on a body and Reset view put the whole-scenario near/far and zoom floor back;
  - zero console errors, warnings and exceptions on the page.

The harness helpers (`_find_chrome`/`_free_port`/`_LiveServer`/`_drive`) are
tests/test_viewer_entities_browser.py's own, duplicated per this repo's convention.
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

SCENARIO_NAME = "Entity framing"
SPACECRAFT = "Sat"
MU_EARTH_KM3_S2 = 398600.4418
ORBIT_RADIUS_KM = 7000.0
INCLINATION_DEG = 51.6
EPOCH_A1MJD = 34000.0
STEP_S = 60.0
N_SAMPLES = 120
# Closed-form diagonal covariance (km^2): 3 sigma semi-axes are exactly 3, 1.5, 0.75 km.
COV_DIAG_KM2 = [1.0, 0.25, 0.0625]
ELLIPSOID_SIGMA = 3
LONGEST_SEMI_AXIS_KM = ELLIPSOID_SIGMA * math.sqrt(max(COV_DIAG_KM2))

# The stated fraction (scene.js ENTITY_FRAMING_FRACTION): the bounding sphere's angular
# diameter is 60 % of the narrower FOV. At this page's camera (45 degree FOV, landscape) the
# perspective projection of that is tan(13.5 deg)/tan(22.5 deg) = 0.58 of the canvas height.
BAND_LOW, BAND_HIGH = 0.50, 0.65
BEFORE_MAX_PX = 3.0  # "under a few pixels"


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


def _framing_scenario() -> dict:
    """A closed-form circular orbit with a closed-form constant covariance, built through
    the repo's own ScenarioData so the wire shape is the producer's, not hand-typed."""
    from altavista.model import BodyTrack, Frame, ScenarioData, Trajectory

    n = math.sqrt(MU_EARTH_KM3_S2 / ORBIT_RADIUS_KM ** 3)  # rad/s
    inc = math.radians(INCLINATION_DEG)
    speed = n * ORBIT_RADIUS_KM
    ts, pos, vel, cov = [], [], [], []
    block = [COV_DIAG_KM2[0], 0.0, 0.0, 0.0, COV_DIAG_KM2[1], 0.0, 0.0, 0.0, COV_DIAG_KM2[2]]
    for k in range(N_SAMPLES):
        s = k * STEP_S
        a = n * s
        ts.append(EPOCH_A1MJD + s / 86400.0)
        pos.append([ORBIT_RADIUS_KM * math.cos(a),
                    ORBIT_RADIUS_KM * math.sin(a) * math.cos(inc),
                    ORBIT_RADIUS_KM * math.sin(a) * math.sin(inc)])
        vel.append([-speed * math.sin(a),
                    speed * math.cos(a) * math.cos(inc),
                    speed * math.cos(a) * math.sin(inc)])
        cov.append(list(block))
    sat = Trajectory(name=SPACECRAFT, t=ts, pos=pos, vel=vel, color="#54a0ff", cov=cov, cov_dim=3)
    earth = BodyTrack(name="Earth", radius=6378.137, flattening=1 / 298.257223563, color="#2e6fbd",
                      central=True, t=[ts[0], ts[-1]], pos=[[0, 0, 0], [0, 0, 0]],
                      quat=[[0, 0, 0, 1], [0, 0, 0, 1]])
    data = ScenarioData(
        name=SCENARIO_NAME, frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
        spacecraft=[sat], bodies=[earth],
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
        _http_post_json(server.url.rstrip("/") + "/api/scenario", _framing_scenario())
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the framing scenario: {exc}")
    try:
        yield server
    finally:
        server.stop()


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 4.0):
    """Identical to tests/test_viewer_entities_browser.py's `_drive` (all three CDP error
    channels: Log.entryAdded errors, console error/warning, Runtime.exceptionThrown)."""
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen(
            [
                chrome_path, f"--user-data-dir={profile_dir}", "--headless=new",
                f"--remote-debugging-port={cdp_port}", "--no-first-run", "--no-default-browser-check",
                "--disable-extensions", "--use-gl=swiftshader", "--enable-unsafe-swiftshader",
                # Wide and short, so the viewport pane is a landscape canvas (height is then
                # the limiting dimension and the band below is literally "of the viewport
                # height"); the test asserts the aspect rather than assuming it.
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
                    deadline = time.monotonic() + 90
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


# The page script. Everything is read off the live scene graph (mesh.matrixWorld, the
# camera's own matrices, the canvas's own client height), after driving the page's own
# controls. `waitFrames` blocks on real requestAnimationFrame ticks bounded by wall-clock
# time (never a fixed sleep as synchronisation).
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three');
    const scene = await import('/js/scene.js');
    const { SCALE } = scene;
    const NAME = 'Sat';
    const until = async (cond, what, ms = 20000) => {
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

    await until(() => viewer.spacecraft.has(NAME) && document.querySelectorAll('#focus-select option').length > 0, 'the scenario to load');
    const covRec = () => viewer._entityCovarianceMeshes.get(NAME);

    // ---- turn on Covariance ellipsoids through the Layers panel's own checkbox
    // (re-queried on every use: the panel re-renders when an entity option changes, which
    // replaces its checkbox elements)
    const covCheckbox = () => [...document.querySelectorAll('.av-layers-entities li')]
      .find((li) => li.textContent.includes('Covariance ellipsoids'))?.querySelector('input[type=checkbox]');
    const setCov = (on) => {
      const cb = covCheckbox();
      if (!cb) throw new Error('no Covariance ellipsoids checkbox in the Layers panel DOM');
      if (cb.checked !== on) { cb.checked = on; cb.dispatchEvent(new Event('change', { bubbles: true })); }
    };
    await until(() => !!covCheckbox(), 'the Layers panel Entities section');
    setCov(true);
    await until(() => viewer.entityOptions.covarianceEllipsoids && covRec() && covRec().mesh.visible, 'the ellipsoid to be drawn');

    const canvas = viewer.canvas;
    const cam = viewer.camera;
    out.canvas = { w: canvas.clientWidth, h: canvas.clientHeight, aspect: cam.aspect, fov: cam.fov };

    // ---- measure the ellipsoid's bounding sphere and bbox as the camera sees them
    const measure = () => {
      cam.updateMatrixWorld(true);
      cam.updateProjectionMatrix();
      const mesh = covRec().mesh;
      mesh.updateWorldMatrix(true, false);
      const C = new THREE.Vector3(), Q = new THREE.Quaternion(), S = new THREE.Vector3();
      mesh.matrixWorld.decompose(C, Q, S);
      const R = Math.max(S.x, S.y, S.z);
      const camPos = cam.getWorldPosition(new THREE.Vector3());
      const toC = C.clone().sub(camPos);
      const D = toC.length();
      const dirv = toC.clone().divideScalar(D);
      const up = new THREE.Vector3().setFromMatrixColumn(cam.matrixWorld, 1).normalize();
      const H = canvas.clientHeight;
      const r = { D, R, sphereDiameterPx: null, sphereHeightFraction: null, bboxHeightFraction: null, inFront: null,
                  near: cam.near, far: cam.far, nearestSurface: D - R, farthestSurface: D + R };
      if (D > R) {
        // The sphere's silhouette: tangent points T = C - dirv*R^2/D +- up*R*sqrt(1-R^2/D^2)
        // (from the tangency condition (T - cam) . (T - C) = 0), projected through the
        // camera's own projection.
        const shift = (R * R) / D, lift = R * Math.sqrt(1 - (R * R) / (D * D));
        const t1 = C.clone().addScaledVector(dirv, -shift).addScaledVector(up, lift).project(cam);
        const t2 = C.clone().addScaledVector(dirv, -shift).addScaledVector(up, -lift).project(cam);
        r.sphereHeightFraction = Math.abs(t1.y - t2.y) / 2;
        r.sphereDiameterPx = r.sphereHeightFraction * H;
      }
      // The ellipsoid itself: a lat/long grid of its surface, through its own matrixWorld.
      let ymin = Infinity, ymax = -Infinity, behind = 0;
      for (let i = 0; i <= 24; i++) {
        const th = (Math.PI * i) / 24;
        for (let j = 0; j < 48; j++) {
          const ph = (2 * Math.PI * j) / 48;
          const p = new THREE.Vector3(Math.sin(th) * Math.cos(ph), Math.sin(th) * Math.sin(ph), Math.cos(th)).applyMatrix4(mesh.matrixWorld);
          const v = p.clone().applyMatrix4(cam.matrixWorldInverse);
          if (v.z >= 0) behind += 1;
          const n = p.project(cam);
          ymin = Math.min(ymin, n.y); ymax = Math.max(ymax, n.y);
        }
      }
      r.bboxHeightFraction = (ymax - ymin) / 2;
      r.surfacePointsBehindCamera = behind;
      r.centreViewZ = C.clone().applyMatrix4(cam.matrixWorldInverse).z;
      r.inFront = r.centreViewZ < 0;
      // Is the camera actually FACING the entity? (Angle between its forward axis and the
      // direction to the ellipsoid's centre, degrees.) Without scene.js's `_updateEntitiesCamera`, OrbitControls aims at a
      // local-space target as if it were a world-space point; see scene.js
      // `_updateEntitiesCamera`.
      const fwd = new THREE.Vector3(0, 0, -1).applyQuaternion(cam.getWorldQuaternion(new THREE.Quaternion()));
      r.offAxisDeg = (Math.acos(Math.min(1, Math.max(-1, fwd.dot(dirv)))) * 180) / Math.PI;
      r.cameraPosLen = cam.position.length();
      r.meshPosLen = mesh.position.length();
      r.targetDistance = cam.position.distanceTo(viewer.controls.target);
      r.minDistance = viewer.controls.minDistance;
      r.originSceneUnits = (() => { const o = viewer.floatingOrigin.getOrigin(viewer.originFrameId); return [o.x, o.y, o.z]; })();
      return r;
    };

    // ---- BEFORE: the Earth-framed camera fit() leaves, then after a click on Reset view
    out.extentSource = viewer.entityExtent(NAME);
    out.before = measure();
    document.getElementById('btn-reset').click();
    await frames(3);
    out.beforeReset = measure();
    out.nearFarAfterReset = { near: cam.near, far: cam.far, minDistance: viewer.controls.minDistance,
                              fitNear: Math.max(viewer.fitRadius * 1e-6, 1e-4), fitFar: Math.max(viewer.fitRadius * 1e4, 1e6) };

    // ---- Focus the spacecraft through the Focus dropdown's own change event (applyView)
    const sel = document.getElementById('focus-select');
    sel.value = NAME;
    sel.dispatchEvent(new Event('change', { bubbles: true }));
    await frames(3);
    out.after = measure();
    out.extentAfterFocus = viewer.entityExtent(NAME);
    out.expectedDistance = scene.entityFramingDistance(out.extentAfterFocus.radius, cam.fov, cam.aspect);
    out.focusName = viewer.focus;
    // The true absolute position of the focus, from the viewer's own interpolator, vs the
    // ellipsoid's world position in render space + the origin shift (question 46).
    {
      const abs = new THREE.Vector3();
      viewer.spacecraft.get(NAME).interp.at(viewer._lastT, abs);
      const mesh = covRec().mesh;
      const world = mesh.getWorldPosition(new THREE.Vector3());
      const g = viewer._entitiesGroup.position;
      out.jitter = {
        renderedAbsKm: [(world.x) / SCALE, (world.y) / SCALE, (world.z) / SCALE],
        trueAbsKm: [abs.x, abs.y, abs.z],   // TrajectoryInterp.at() is in km
        groupShiftLen: Math.hypot(g.x, g.y, g.z),
      };
    }

    // ---- the camera never zooms by itself: real frames pass, the class is toggled off/on
    // Start the page's own clock (the Play button), so real time passes while the camera
    // is watched; bounded on the clock actually advancing, not on a sleep.
    const d0 = cam.position.distanceTo(viewer.controls.target);
    const t0 = viewer._lastT;
    document.getElementById('btn-play').click();
    await until(() => viewer._lastT - t0 > 5 / 86400, 'the clock to advance by at least 5 s');
    await frames(10);
    setCov(false);
    await frames(5);
    out.extentWhileOff = viewer.entityExtent(NAME);
    const dOff = cam.position.distanceTo(viewer.controls.target);
    setCov(true);
    await until(() => covRec().mesh.visible, 'the ellipsoid to be drawn again');
    await frames(5);
    const d1 = cam.position.distanceTo(viewer.controls.target);
    out.noAutoZoom = { d0, dOff, d1, clockAdvancedDays: viewer._lastT - t0, afterReenable: measure() };

    // ---- explicit re-Focus re-frames to the CURRENT extent: ellipsoid off -> marker extent
    setCov(false);
    await frames(3);
    document.querySelector('#sc-list .go').click();   // the list's own "focus" link
    await frames(3);
    out.reframed = {
      extent: viewer.entityExtent(NAME),
      distance: cam.position.distanceTo(viewer.controls.target),
      expected: scene.entityFramingDistance(viewer.entityExtent(NAME).radius, cam.fov, cam.aspect),
      near: cam.near, minDistance: viewer.controls.minDistance,
    };
    setCov(true);
    await until(() => covRec().mesh.visible, 'the ellipsoid to be drawn again');
    document.querySelector('#sc-list .go').click();
    await frames(3);
    out.reframedAgain = measure();

    // ---- focus a body: whole-scenario near/far/zoom floor come back
    sel.value = 'Earth';
    sel.dispatchEvent(new Event('change', { bubbles: true }));
    await frames(3);
    out.afterBodyFocus = { near: cam.near, far: cam.far, minDistance: viewer.controls.minDistance, fitNear: out.nearFarAfterReset.fitNear, fitFar: out.nearFarAfterReset.fitFar };

    // ---- and Reset view after a framing
    document.querySelector('#sc-list .go').click();
    await frames(3);
    out.framedAgainNear = cam.near;
    document.getElementById('btn-reset').click();
    await frames(3);
    out.afterReset = { near: cam.near, far: cam.far, minDistance: viewer.controls.minDistance, framed: viewer._entityFramed };

    // ---- a second (per-viewport) camera focused on the spacecraft faces it too. Its focus
    // still uses the old central-body distance (extent framing is the primary camera's),
    // so this asserts only the aim: forward axis vs the direction to the entity.
    {
      const vpCanvas = document.createElement('canvas');
      vpCanvas.style.cssText = 'position:fixed;left:0;top:0;width:400px;height:300px;z-index:-1';
      document.body.appendChild(vpCanvas);
      const vp = viewer.addViewport('aim-check', vpCanvas);
      viewer.setViewportFocus('aim-check', NAME);
      await frames(10);
      vp.camera.updateMatrixWorld(true);
      const camPos = vp.camera.getWorldPosition(new THREE.Vector3());
      const fwd = new THREE.Vector3(0, 0, -1).applyQuaternion(vp.camera.getWorldQuaternion(new THREE.Quaternion()));
      const tgt = viewer.spacecraft.get(NAME).marker.getWorldPosition(new THREE.Vector3());
      const toT = tgt.clone().sub(camPos).normalize();
      out.viewport = {
        renderGroupShiftLen: vp.renderGroup.position.length(),
        distance: camPos.distanceTo(tgt),
        offAxisDeg: (Math.acos(Math.min(1, Math.max(-1, fwd.dot(toT)))) * 180) / Math.PI,
      };
    }

    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_focus_frames_the_spacecraft_by_its_own_extent(live_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, v = asyncio.run(_drive(live_server.url, chrome_path, _PROBE_JS, wait_s=4.0))

    print("\nentity-framing probe:", json.dumps(v, indent=2, sort_keys=True))
    print("console/exception messages:", errors)
    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v

    # The scenario's own ellipsoid is the one this file's docstring states: 3 km longest semi-axis.
    assert v["extentSource"]["source"] == "covariance", v["extentSource"]
    assert math.isclose(v["extentSource"]["radius"], LONGEST_SEMI_AXIS_KM * 1e-3, rel_tol=1e-6), v["extentSource"]
    assert v["canvas"]["h"] > 300 and v["canvas"]["aspect"] > 1, f"canvas too small/portrait for the band: {v['canvas']}"

    # ------------------------------------------------ BEFORE: the lead's observation, as a number
    for key in ("before", "beforeReset"):
        b = v[key]
        assert b["inFront"] is True, b
        assert b["sphereDiameterPx"] < BEFORE_MAX_PX, (
            f"{key}: at the Earth-framed camera the 3 km ellipsoid's bounding sphere should be a dot "
            f"(< {BEFORE_MAX_PX} px), measured {b['sphereDiameterPx']:.3f} px at camera distance "
            f"{b['D']:.2f} scene units"
        )
    # Reset view restores the whole-scenario depth range.
    r = v["nearFarAfterReset"]
    assert math.isclose(r["near"], r["fitNear"], rel_tol=1e-12) and math.isclose(r["far"], r["fitFar"], rel_tol=1e-12), r
    assert r["minDistance"] == 1e-3, r

    # ------------------------------------------------ AFTER Focus: framed by the entity's own extent
    a = v["after"]
    assert v["focusName"] == SPACECRAFT
    assert BAND_LOW <= a["sphereHeightFraction"] <= BAND_HIGH, (
        f"after Focus the ellipsoid's bounding sphere should span {BAND_LOW:.0%}-{BAND_HIGH:.0%} of the canvas "
        f"height, measured {a['sphereHeightFraction']:.3f} ({a['sphereDiameterPx']:.1f} px of {v['canvas']['h']} px); "
        f"camera {a['D']:.5f} scene units from the entity (expected {v['expectedDistance']:.5f})"
    )
    assert a["sphereDiameterPx"] > 100, a
    assert a["bboxHeightFraction"] <= a["sphereHeightFraction"] + 1e-9, "the ellipsoid must lie inside its own bounding sphere"
    assert 0.15 <= a["bboxHeightFraction"] <= 1.0, f"the drawn ellipsoid itself is not visibly framed: {a}"
    assert math.isclose(a["D"], v["expectedDistance"], rel_tol=1e-6), (a["D"], v["expectedDistance"])
    # In front of the camera and entirely inside near/far (nothing clipped).
    assert a["inFront"] is True and a["surfacePointsBehindCamera"] == 0, a
    assert a["near"] < a["nearestSurface"] and a["farthestSurface"] < a["far"], a
    assert a["near"] <= 0.05 * a["D"], f"near plane not tightened to the framing distance: {a}"
    # The Earth, ~7 scene units behind a LEO entity, is still inside far.
    assert a["far"] > 7.0 + 6.4, a
    # Question 46: the floating origin sits on the entity (small render-space numbers) and
    # the render-space + origin reconstruction of its position equals the true position.
    assert a["cameraPosLen"] < 0.05 and a["meshPosLen"] < 0.05, (
        f"camera/entity are not near the render origin (float32 jitter bound): {a['cameraPosLen']}, {a['meshPosLen']}"
    )
    j = v["jitter"]
    err_m = 1000.0 * math.dist(j["renderedAbsKm"], j["trueAbsKm"])
    # rendered (world) = group shift + mesh.position; world position already includes the shift.
    assert err_m < 0.01, f"entity world position vs true absolute position: {err_m} m ({j})"
    assert a["minDistance"] <= 1e-3 and math.isclose(a["minDistance"], min(1e-3, 0.1 * a["D"]), rel_tol=1e-9), a["minDistance"]
    assert a["offAxisDeg"] < 1e-3, (
        f"the camera is not facing the framed entity: its forward axis is {a['offAxisDeg']:.3f} degrees off the "
        f"entity (origin shift {j['groupShiftLen']:.3f} scene units; OrbitControls aims at a local-space target as "
        f"if it were world-space)"
    )

    # ------------------------------------------------ the camera never zooms by itself
    n = v["noAutoZoom"]
    assert n["clockAdvancedDays"] > 0, f"the clock did not advance, so 'no auto zoom' proved nothing: {n}"
    # 1e-6 of a ~13 km distance is ~1 cm: tighter than float32 rounding of the orbit target
    # in the per-tick follow (~1e-8 relative, scene.js `update()`), orders of magnitude
    # below any real zoom (a re-frame changes the distance by a factor of 10).
    for k in ("dOff", "d1"):
        assert math.isclose(n[k], n["d0"], rel_tol=1e-6), f"camera distance changed by itself ({k}): {n}"
    assert v["extentWhileOff"]["source"] == "marker", v["extentWhileOff"]
    assert 0.5 <= n["afterReenable"]["sphereHeightFraction"] <= 0.65, n["afterReenable"]

    # ------------------------------------------------ an explicit Focus re-frames to the new extent
    rf = v["reframed"]
    assert rf["extent"]["source"] == "marker", rf
    assert math.isclose(rf["distance"], rf["expected"], rel_tol=1e-6), rf
    assert rf["distance"] < n["d0"] / 5, f"re-focus on the marker extent did not move the camera in: {rf} vs {n['d0']}"
    assert rf["minDistance"] < 1e-3 and rf["near"] < rf["distance"], rf
    ra = v["reframedAgain"]
    assert BAND_LOW <= ra["sphereHeightFraction"] <= BAND_HIGH, ra

    # ------------------------------------------------ restore on body focus and on Reset view
    bf = v["afterBodyFocus"]
    assert math.isclose(bf["near"], bf["fitNear"], rel_tol=1e-12) and math.isclose(bf["far"], bf["fitFar"], rel_tol=1e-12), bf
    assert bf["minDistance"] == 1e-3, bf
    # While framed, near follows the framing distance (1 % of it), not the scenario's value.
    assert math.isclose(v["framedAgainNear"], 0.01 * ra["D"], rel_tol=1e-6), (v["framedAgainNear"], ra["D"])
    assert ra["near"] < ra["nearestSurface"] and ra["farthestSurface"] < ra["far"], ra
    ar = v["afterReset"]
    assert math.isclose(ar["near"], bf["fitNear"], rel_tol=1e-12) and math.isclose(ar["far"], bf["fitFar"], rel_tol=1e-12), ar
    assert ar["minDistance"] == 1e-3 and ar["framed"] is False, ar

    # The per-viewport camera (scene.js `_updateViewport`) had the same local-vs-world aim
    # defect: with its origin rebased onto the spacecraft it faced the Earth's centre.
    vpm = v["viewport"]
    assert vpm["renderGroupShiftLen"] > 1.0, f"the viewport's origin was not rebased onto the spacecraft, so this proved nothing: {vpm}"
    assert vpm["offAxisDeg"] < 1e-3, (
        f"the per-viewport camera is not facing its focused spacecraft: {vpm['offAxisDeg']:.4f} degrees off ({vpm})"
    )

    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

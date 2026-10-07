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

The second half of the file (question 239, task 1) covers a spacecraft whose only drawn thing is
its glTF model, and the entity marker's screen-space size; see the comment block above
`RPO_SCENARIO_NAME`.

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


# =====================================================================================
# Screen-space entity markers, and a model-only spacecraft at Focus (heavy carry-over
# round, question 239, task 1).
#
# Before: the entity marker was an instanced sphere of 3e-4 scene units (300 m) radius, and it
# was the floor of `Viewer.entityExtent`, so Focus on the RPO Chaser framed at marker scale (a
# 600 km sphere) and its true-size 1.5 m glTF model (a millionth of that) was invisible. Now the
# marker is a disc of a fixed number of screen pixels (its own vertex shader, per draw call, so
# every viewport sees the same pixel size) and sets no extent; the model's bounding sphere does.
#
# The scenario is the RPO geometry of examples/05_rpo_ric.py in closed form (no GMAT needed):
# Target and Chaser on one 7,000 km circular orbit, the Chaser 30 m ahead in track, the Chaser
# declaring the committed glTF fixture (a 1 x 1 x 1.5 m tetrahedron) as its `model`.
# =====================================================================================
RPO_SCENARIO_NAME = "Entity framing RPO"
MODEL_URL = "/js/fixtures/entity_model_fixture.gltf"
CHASER_LEAD_M = 30.0
# The framing sphere is 60 % of the narrower FOV (45 degrees vertical, landscape canvas);
# perspective makes its projected diameter 58 % of the canvas height.
FRAMING_SPHERE_BAND = (0.50, 0.65)
# The marker's diameter in CSS pixels (ENTITY_MARKER_RADIUS_PX = 5 -> 10 px) and the tolerance
# a disc rasterised at fractional-pixel centres by 4x MSAA is read back within: one pixel on
# each side of the diameter; the ratio between the two distances must be within 10 %.
MARKER_DIAMETER_PX = 10.0
MARKER_DIAMETER_TOL_PX = 1.5
MARKER_RATIO_TOL = 0.10


def _rpo_scenario() -> dict:
    from altavista.model import BodyTrack, Frame, ScenarioData, Trajectory

    n = math.sqrt(MU_EARTH_KM3_S2 / ORBIT_RADIUS_KM ** 3)
    inc = math.radians(INCLINATION_DEG)
    speed = n * ORBIT_RADIUS_KM
    lead = (CHASER_LEAD_M / 1000.0) / ORBIT_RADIUS_KM  # radians of orbit angle

    def track(name, phase, color, **kw):
        ts, pos, vel = [], [], []
        for k in range(N_SAMPLES):
            s = k * STEP_S
            a = n * s + phase
            ts.append(EPOCH_A1MJD + s / 86400.0)
            pos.append([ORBIT_RADIUS_KM * math.cos(a),
                        ORBIT_RADIUS_KM * math.sin(a) * math.cos(inc),
                        ORBIT_RADIUS_KM * math.sin(a) * math.sin(inc)])
            vel.append([-speed * math.sin(a),
                        speed * math.cos(a) * math.cos(inc),
                        speed * math.cos(a) * math.sin(inc)])
        return Trajectory(name=name, t=ts, pos=pos, vel=vel, color=color, **kw)

    target = track("Target", 0.0, "#54a0ff")
    chaser = track("Chaser", lead, "#ff6b6b", model=MODEL_URL)
    ts = target.t
    earth = BodyTrack(name="Earth", radius=6378.137, flattening=1 / 298.257223563, color="#2e6fbd",
                      central=True, t=[ts[0], ts[-1]], pos=[[0, 0, 0], [0, 0, 0]],
                      quat=[[0, 0, 0, 1], [0, 0, 0, 1]])
    data = ScenarioData(
        name=RPO_SCENARIO_NAME, frame=Frame("EarthMJ2000Eq", "Earth", "MJ2000Eq"),
        spacecraft=[target, chaser], bodies=[earth],
        frames=[{"id": "EarthMJ2000Eq", "axes": "AXES_KIND_MJ2000_EQ", "body": "Earth"}],
    )
    return data.to_dict()


@pytest.fixture()
def rpo_server():
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
        _http_post_json(server.url.rstrip("/") + "/api/scenario", _rpo_scenario())
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the RPO scenario: {exc}")
    try:
        yield server
    finally:
        server.stop()


# Page helpers shared by the two probes below (prepended to each).
_RPO_PROBE_PRELUDE = r"""
const viewer = window.altavistaViewer;
if (!viewer) throw new Error('no viewer on window');
const THREE = await import('three');
const scene = await import('/js/scene.js');
const NAME = 'Chaser';
const until = async (cond, what, ms = 30000) => {
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
await until(() => { const e = viewer._entityModelEntities.get(NAME); return !!(e && e.modelLoaded); }, 'the Chaser model to load');
// the Layers panel's own checkbox for an entity class, found by its label text
const setClass = async (labelText, on) => {
  const find = () => [...document.querySelectorAll('.av-layers-entities li')]
    .find((li) => li.textContent.includes(labelText))?.querySelector('input[type=checkbox]');
  await until(() => !!find(), 'the ' + labelText + ' checkbox');
  const cb = find();
  if (cb.checked !== on) { cb.checked = on; cb.dispatchEvent(new Event('change', { bubbles: true })); }
};
const canvas = viewer.canvas;
// Read the drawing buffer right after a render, in the same task (the buffer is cleared
// after compositing, the canvas is not preserveDrawingBuffer).
const grab = (renderer, cv) => {
  const c2 = document.createElement('canvas');
  c2.width = cv.width; c2.height = cv.height;
  const ctx = c2.getContext('2d', { willReadFrequently: true });
  ctx.drawImage(cv, 0, 0);
  return ctx.getImageData(0, 0, c2.width, c2.height);
};
// Pixels that differ between two grabs, inside the (inclusive) pixel box, threshold on the
// sum of the three channel differences.
const diffIn = (a, b, box, thr = 24) => {
  const W = a.width;
  let n = 0, x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity, r = 0, g = 0, bl = 0;
  for (let y = Math.max(0, box.y0); y <= Math.min(a.height - 1, box.y1); y++) {
    for (let x = Math.max(0, box.x0); x <= Math.min(W - 1, box.x1); x++) {
      const i = (y * W + x) * 4;
      const d = Math.abs(a.data[i] - b.data[i]) + Math.abs(a.data[i + 1] - b.data[i + 1]) + Math.abs(a.data[i + 2] - b.data[i + 2]);
      if (d > thr) {
        n += 1; x0 = Math.min(x0, x); x1 = Math.max(x1, x); y0 = Math.min(y0, y); y1 = Math.max(y1, y);
        r += a.data[i]; g += a.data[i + 1]; bl += a.data[i + 2];
      }
    }
  }
  return { count: n, x0, x1, y0, y1, width: n ? x1 - x0 + 1 : 0, height: n ? y1 - y0 + 1 : 0,
           meanRGB: n ? [r / n, g / n, bl / n] : null };
};
"""

_MODEL_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
""" + _RPO_PROBE_PRELUDE + r"""
    // ---- ONLY the markers and models classes on (trails off; ellipsoid/keep-out off by default)
    await setClass('Trails', false);
    await setClass('glTF models', true);
    await until(() => viewer.entityOptions.models && !viewer.entityOptions.trails, 'the class options');
    out.options = { ...viewer.entityOptions };

    // ---- Focus on the Chaser through the Focus dropdown's own change event
    const sel = document.getElementById('focus-select');
    sel.value = NAME;
    sel.dispatchEvent(new Event('change', { bubbles: true }));
    await frames(4);
    const cam = viewer.camera;
    const entity = viewer._entityModelEntities.get(NAME);
    out.extent = viewer.entityExtent(NAME);
    out.focus = viewer.focus;
    out.canvasCss = { w: canvas.clientWidth, h: canvas.clientHeight, aspect: cam.aspect, fov: cam.fov };
    out.drawing = { w: canvas.width, h: canvas.height, pixelRatio: viewer.renderer.getPixelRatio() };
    out.distance = cam.position.distanceTo(viewer.controls.target);
    out.expectedDistance = scene.entityFramingDistance(out.extent.radius, cam.fov, cam.aspect);
    out.depth = { near: cam.near, far: cam.far };

    // ---- where the model's vertices are, as the camera sees them
    cam.updateMatrixWorld(true); cam.updateProjectionMatrix();
    entity.group.updateWorldMatrix(true, true);
    const origin = entity.group.getWorldPosition(new THREE.Vector3());
    const camPos = cam.getWorldPosition(new THREE.Vector3());
    let xmin = Infinity, xmax = -Infinity, ymin = Infinity, ymax = -Infinity, zmin = Infinity, zmax = -Infinity;
    let behind = 0, nVert = 0, maxDist = 0;
    entity.group.traverse((o) => {
      if (!o.isMesh) return;
      const pos = o.geometry.attributes.position;
      for (let i = 0; i < pos.count; i++) {
        const w = new THREE.Vector3().fromBufferAttribute(pos, i).applyMatrix4(o.matrixWorld);
        maxDist = Math.max(maxDist, w.distanceTo(origin));
        const viewZ = w.clone().applyMatrix4(cam.matrixWorldInverse).z;
        if (viewZ >= 0) behind += 1;
        zmin = Math.min(zmin, -viewZ); zmax = Math.max(zmax, -viewZ);
        const n = w.project(cam);
        xmin = Math.min(xmin, n.x); xmax = Math.max(xmax, n.x);
        ymin = Math.min(ymin, n.y); ymax = Math.max(ymax, n.y);
        nVert += 1;
      }
    });
    const Wd = canvas.width, Hd = canvas.height;
    // NDC -> drawing-buffer pixels (y down)
    const px = (nx) => (nx * 0.5 + 0.5) * Wd, py = (ny) => (1 - (ny * 0.5 + 0.5)) * Hd;
    const bbox = { x0: Math.floor(px(xmin)), x1: Math.ceil(px(xmax)), y0: Math.floor(py(ymax)), y1: Math.ceil(py(ymin)) };
    out.model = {
      vertices: nVert, verticesBehindCamera: behind,
      farthestVertexFromMarkerScene: maxDist, // == the extent radius by construction
      bboxCssW: (bbox.x1 - bbox.x0) / out.drawing.pixelRatio,
      bboxCssH: (bbox.y1 - bbox.y0) / out.drawing.pixelRatio,
      bboxHeightFractionOfCanvas: (ymax - ymin) / 2,
      bboxWidthFractionOfCanvas: (xmax - xmin) / 2,
      viewDepthMin: zmin, viewDepthMax: zmax,
      scaleOfGroupNode: entity.group.children[0] && entity.group.children[0].scale.x,
    };
    // the MODEL's bounding sphere (centre = the marker position, radius = the farthest vertex
    // from it, measured above off the geometry, not read from entityExtent) as projected
    {
      const C = origin, R = maxDist, D = C.distanceTo(camPos);
      const dirv = C.clone().sub(camPos).normalize();
      const up = new THREE.Vector3().setFromMatrixColumn(cam.matrixWorld, 1).normalize();
      const shift = (R * R) / D, lift = R * Math.sqrt(1 - (R * R) / (D * D));
      const t1 = C.clone().addScaledVector(dirv, -shift).addScaledVector(up, lift).project(cam);
      const t2 = C.clone().addScaledVector(dirv, -shift).addScaledVector(up, -lift).project(cam);
      out.sphere = { D, R, heightFraction: Math.abs(t1.y - t2.y) / 2, nearestSurface: D - R, farthestSurface: D + R };
    }

    // ---- is it DRAWN? render with the models class on and off, diff the pixels in its box.
    // (Everything else -- the Earth, the stars, the markers, the other spacecraft -- is
    // identical between the two renders, so any difference is the model.)
    const rg = 3; // pad the box by a few pixels
    const box = { x0: bbox.x0 - rg, x1: bbox.x1 + rg, y0: bbox.y0 - rg, y1: bbox.y1 + rg };
    const renderNow = () => { viewer.renderer.render(viewer.scene, cam); return grab(viewer.renderer, canvas); };
    const withModel = renderNow();
    const modelGroupVisible = viewer._entityGroups.models.visible && entity.group.visible;
    viewer._entityGroups.models.visible = false;
    const withoutModel = renderNow();
    viewer._entityGroups.models.visible = true;
    const d = diffIn(withModel, withoutModel, box);
    // and outside the box (padded generously) the two renders must agree -- the diff is the model
    const wholeFrame = diffIn(withModel, withoutModel, { x0: 0, x1: Wd - 1, y0: 0, y1: Hd - 1 });
    out.drawn = {
      modelGroupVisible, box, boxArea: (box.x1 - box.x0 + 1) * (box.y1 - box.y0 + 1),
      diffInBox: d, diffWholeFrame: wholeFrame.count,
      diffBoxAreaFraction: d.count / ((box.x1 - box.x0 + 1) * (box.y1 - box.y0 + 1)),
    };

    // ---- depth precision at the framing distance. The renderer runs a logarithmic depth buffer
    // whose per-fragment term is `1 + w` in float32 (three.js), so for w around 6e-6 scene units
    // two surfaces closer together than a few centimetres cannot be ordered. Measured here: a
    // NEAR (red) quad drawn first and a FAR (green) quad drawn second, `sep` metres apart, both
    // facing the camera at the framing distance; red is the right answer, green means the depth
    // buffer could not tell them apart. The model's own parts are 0.5-1.5 m apart in depth.
    viewer._entityGroups.models.visible = false;
    const fwd = new THREE.Vector3(0, 0, -1).applyQuaternion(cam.getWorldQuaternion(new THREE.Quaternion()));
    const quad = (color, dist, order) => {
      const q = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), new THREE.MeshBasicMaterial({ color, side: THREE.DoubleSide }));
      q.scale.setScalar(out.distance * 0.2);
      q.quaternion.copy(cam.getWorldQuaternion(new THREE.Quaternion()));
      q.position.copy(camPos).addScaledVector(fwd, dist);
      q.renderOrder = order;
      viewer.scene.add(q);
      return q;
    };
    const sx = Math.floor(canvas.width / 2) + 40, sy = Math.floor(canvas.height / 2) + 40;
    out.depthOrder = {};
    for (const sepM of [0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 1.5]) {
      const near = quad(0xff0000, out.distance, 0), far = quad(0x00ff00, out.distance + sepM * 1e-6, 1);
      const img = renderNow();
      const i = (sy * img.width + sx) * 4;
      out.depthOrder[String(sepM)] = img.data[i] > img.data[i + 1] ? 'near' : 'far';
      viewer.scene.remove(near); viewer.scene.remove(far);
    }
    viewer._entityGroups.models.visible = true;
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_focus_on_a_model_only_spacecraft_frames_and_draws_the_model(rpo_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, v = asyncio.run(_drive(rpo_server.url, chrome_path, _MODEL_PROBE_JS, wait_s=4.0))

    print("\nmodel-focus probe:", json.dumps(v, indent=2, sort_keys=True))
    print("console/exception messages:", errors)
    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v

    # Only the markers and models classes are on.
    assert v["options"] == {"markers": True, "trails": False, "covarianceEllipsoids": False,
                            "keepOutVolumes": False, "models": True}, v["options"]
    assert v["focus"] == "Chaser"
    assert v["canvasCss"]["aspect"] > 1, v["canvasCss"]

    # The model fills the framing fraction. The framing sphere (60 % of the narrower FOV,
    # 58 % of the canvas height in perspective) is the minimal sphere about the marker
    # position containing the model; the model's projected box is inside it and touches it.
    lo, hi = FRAMING_SPHERE_BAND
    assert lo <= v["sphere"]["heightFraction"] <= hi, v["sphere"]
    # ... and equals the closed form (scene.js projectedSphereHeightFraction) to rounding.
    R, D, fov = v["sphere"]["R"], v["sphere"]["D"], v["canvasCss"]["fov"]
    closed_form = (R / math.sqrt(D * D - R * R)) / math.tan(math.radians(fov) / 2)
    assert math.isclose(v["sphere"]["heightFraction"], closed_form, rel_tol=1e-6), (v["sphere"], closed_form)
    m = v["model"]
    assert m["verticesBehindCamera"] == 0 and m["vertices"] > 0, m
    assert 1e-6 <= m["farthestVertexFromMarkerScene"] <= 2.5e-6, m  # a 1 x 1 x 1.5 m shell, 1 m = 1e-6 scene units
    # The extent Focus framed with is that sphere: the model's, not the 300 m marker floor.
    assert v["extent"]["source"] == "model", v["extent"]
    assert math.isclose(v["extent"]["radius"], m["farthestVertexFromMarkerScene"], rel_tol=1e-6), v
    assert math.isclose(v["distance"], v["expectedDistance"], rel_tol=1e-6), (v["distance"], v["expectedDistance"])
    # The model's projected box, as a fraction of the canvas height. It lies inside the framing
    # sphere (upper bound). It does NOT fill the sphere: the sphere is centred on the
    # spacecraft's marker position, and this fixture's origin is the corner of its 1 x 1 x 1.5 m
    # shell, so the model occupies one side of the sphere (measured 0.37 of its diameter in this
    # view). The lower bound, 0.30 of the sphere's diameter, is what separates "framed to the
    # model" from "framed to the 300 m marker": there the model is 1.5e-6 / 3e-4 = 0.5 % of
    # the sphere's radius and a hundred times under the bound.
    longer = max(m["bboxHeightFractionOfCanvas"], m["bboxWidthFractionOfCanvas"])
    assert longer <= v["sphere"]["heightFraction"] * 1.001, (longer, v["sphere"])
    assert longer >= 0.30 * v["sphere"]["heightFraction"], (longer, v["sphere"])
    assert m["bboxCssH"] > 100 or m["bboxCssW"] > 100, m

    # Nothing is clipped: the whole sphere is inside near/far and in front of the camera.
    assert v["depth"]["near"] < v["sphere"]["nearestSurface"], (v["depth"], v["sphere"])
    assert v["sphere"]["farthestSurface"] < v["depth"]["far"], (v["depth"], v["sphere"])
    assert m["viewDepthMin"] > v["depth"]["near"] and m["viewDepthMax"] < v["depth"]["far"], (m, v["depth"])

    # Drawn, not merely placed: the pixels inside the box differ between a render with the
    # model and one without, they differ NOWHERE else on the canvas, and a tetrahedron covers a
    # sizeable part of its own projected box.
    dr = v["drawn"]
    assert dr["modelGroupVisible"] is True, dr
    assert dr["diffInBox"]["count"] > 0.15 * dr["boxArea"], (
        f"the model is placed but not drawn: only {dr['diffInBox']['count']} of {dr['boxArea']} pixels in its box "
        f"change when the model is hidden ({dr})"
    )
    assert dr["diffWholeFrame"] == dr["diffInBox"]["count"], dr
    assert max(dr["diffInBox"]["meanRGB"]) > 20, f"the model's pixels are black on black: {dr['diffInBox']}"

    # Depth precision at the framed distance: surfaces 0.25 m or more apart in depth are ordered
    # correctly (the fixture's parts are 0.5-1.5 m apart); the 2-5 cm cases are printed, not asserted.
    for sep in ("0.25", "0.5", "1", "1.5"):
        assert v["depthOrder"][sep] == "near", f"depth buffer mis-orders surfaces {sep} m apart: {v['depthOrder']}"

    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )


_MARKER_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
""" + _RPO_PROBE_PRELUDE + r"""
    // markers on (the default), trails off, models off (the default): the entity marker alone
    await setClass('Trails', false);
    out.options = { ...viewer.entityOptions };
    // the per-spacecraft s.marker (a different, 7 px sphere) is hidden, so only the entity
    // marker is measured; nothing in the per-tick path resets s.marker.visible
    for (const s of viewer.spacecraft.values()) s.marker.visible = false;
    const sel = document.getElementById('focus-select');
    sel.value = NAME;
    sel.dispatchEvent(new Event('change', { bubbles: true }));
    await frames(4);
    const t = viewer._lastT;

    const mesh = viewer._entityMarkerMesh;
    const ci = viewer._entityMarkerResidentNames.indexOf(NAME);
    out.markerMesh = { count: mesh.count, names: viewer._entityMarkerResidentNames.slice(), frustumCulled: mesh.frustumCulled };
    const sphere = new THREE.SphereGeometry(1, 16, 12);

    // The measurement. `place` positions a camera at distance d from its target along its
    // current direction, `viewer.update(t)` brings every per-tick matrix up to date (and
    // renders), then in the SAME synchronous block: both spacecraft's instances are put on the
    // Chaser's matrix (the Target is 30 m away and would overlap the Chaser's marker at the far
    // distance), the mesh is rendered with the markers group on and off, and the pixels that
    // differ in a window about the canvas centre are the marker.
    //   mode 'screen'      -- the code under test
    //   mode 'attenuated'  -- the OLD design, rebuilt: a plain material and a sphere of a fixed
    //                         WORLD radius (chosen so that it is 10 px across at the far
    //                         distance), which is what a perturbed/old marker does
    const FAR = 2e-2;
    const measure = (renderer, camera, cv, controls, d, mode) => {
      const target = controls.target;
      const dir = camera.position.clone().sub(target).normalize();
      camera.position.copy(target).addScaledVector(dir, d);
      camera.near = d * 0.01; camera.far = 1e6; camera.updateProjectionMatrix();
      controls.minDistance = 1e-9; controls.maxDistance = 1e8;
      viewer.update(t);
      const realDistance = camera.position.distanceTo(target);
      const m = new THREE.Matrix4();
      mesh.getMatrixAt(ci, m);
      const pos = new THREE.Vector3(), q = new THREE.Quaternion(), sc = new THREE.Vector3();
      m.decompose(pos, q, sc);
      const geometry0 = mesh.geometry, material0 = mesh.material, before0 = mesh.onBeforeRender;
      let radiusWorld = null;
      if (mode === 'attenuated') {
        radiusWorld = 5 * (2 * FAR * Math.tan((camera.fov * Math.PI) / 360) / (cv.clientHeight));
        mesh.geometry = sphere;
        mesh.material = new THREE.MeshBasicMaterial({ vertexColors: true, side: THREE.DoubleSide });
        mesh.onBeforeRender = () => {};
        m.compose(pos, q, new THREE.Vector3(radiusWorld, radiusWorld, radiusWorld));
      }
      for (let i = 0; i < mesh.count; i++) mesh.setMatrixAt(i, m);
      mesh.instanceMatrix.needsUpdate = true;
      const grabNow = () => { renderer.render(viewer.scene, camera); return grab(renderer, cv); };
      const on = grabNow();
      viewer._entityGroups.markers.visible = false;
      const off = grabNow();
      viewer._entityGroups.markers.visible = true;
      const cx = Math.floor(cv.width / 2), cy = Math.floor(cv.height / 2), win = 60;
      const r = diffIn(on, off, { x0: cx - win, x1: cx + win, y0: cy - win, y1: cy + win }, 24);
      mesh.geometry = geometry0; mesh.material = material0; mesh.onBeforeRender = before0;
      const pr = renderer.getPixelRatio();
      return { d, realDistance, mode, canvasCss: [cv.clientWidth, cv.clientHeight], pixelRatio: pr,
               widthCss: r.width / pr, heightCss: r.height / pr, areaCssSq: r.count / (pr * pr),
               centre: r.count ? [(r.x0 + r.x1) / 2 - cx, (r.y0 + r.y1) / 2 - cy] : null,
               windowCss: (2 * win + 1) / pr, radiusWorld };
    };
    const NEAR = 2e-5;
    out.primary = {
      near: measure(viewer.renderer, viewer.camera, canvas, viewer.controls, NEAR, 'screen'),
      far: measure(viewer.renderer, viewer.camera, canvas, viewer.controls, FAR, 'screen'),
      attenuatedNear: measure(viewer.renderer, viewer.camera, canvas, viewer.controls, NEAR, 'attenuated'),
      attenuatedFar: measure(viewer.renderer, viewer.camera, canvas, viewer.controls, FAR, 'attenuated'),
    };

    // ---- a second viewport: its own camera, its own (smaller) canvas, the SAME shared mesh
    const vpCanvas = document.createElement('canvas');
    vpCanvas.style.cssText = 'position:fixed;left:0;top:0;width:400px;height:300px;z-index:-1';
    document.body.appendChild(vpCanvas);
    const vp = viewer.addViewport('marker-check', vpCanvas);
    viewer.setViewportFocus('marker-check', NAME);
    await frames(6);
    vp.controls.enableDamping = false;
    out.viewport = {
      near: measure(vp.renderer, vp.camera, vpCanvas, vp.controls, 5e-5, 'screen'),
      far: measure(vp.renderer, vp.camera, vpCanvas, vp.controls, 5e-2, 'screen'),
      attenuatedNear: measure(vp.renderer, vp.camera, vpCanvas, vp.controls, 5e-5, 'attenuated'),
    };
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_entity_marker_is_a_constant_number_of_pixels_at_any_distance_in_every_viewport(rpo_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, v = asyncio.run(_drive(rpo_server.url, chrome_path, _MARKER_PROBE_JS, wait_s=4.0))

    print("\nmarker-size probe:", json.dumps(v, indent=2, sort_keys=True))
    print("console/exception messages:", errors)
    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v
    assert v["options"]["markers"] is True and v["options"]["models"] is False, v["options"]
    assert v["markerMesh"]["count"] == 2 and sorted(v["markerMesh"]["names"]) == ["Chaser", "Target"], v["markerMesh"]

    def settled(m):
        # the camera really is at the distance asked for (the controls did not clamp it)
        assert math.isclose(m["realDistance"], m["d"], rel_tol=1e-6), m
        return m

    # ------------------------------------------------------------ the marker is 10 px, always
    for where, key in (("primary", "near"), ("primary", "far"), ("viewport", "near"), ("viewport", "far")):
        m = settled(v[where][key])
        for dim in ("widthCss", "heightCss"):
            assert abs(m[dim] - MARKER_DIAMETER_PX) <= MARKER_DIAMETER_TOL_PX, (
                f"{where}/{key}: the entity marker is {m[dim]:.2f} px {dim[:-3]}, expected "
                f"{MARKER_DIAMETER_PX} +- {MARKER_DIAMETER_TOL_PX} px at camera distance {m['d']} scene units ({m})"
            )
        # drawn at the centre of the view, which is where the focused spacecraft is
        assert abs(m["centre"][0]) <= 1.5 and abs(m["centre"][1]) <= 1.5, m
    # a 1000x change of camera distance changes the on-screen size by less than MARKER_RATIO_TOL
    p_near, p_far = v["primary"]["near"], v["primary"]["far"]
    assert math.isclose(p_far["d"] / p_near["d"], 1000.0, rel_tol=1e-9), (p_near["d"], p_far["d"])
    assert math.isclose(p_far["widthCss"], p_near["widthCss"], rel_tol=MARKER_RATIO_TOL), (p_near, p_far)
    assert math.isclose(p_far["areaCssSq"], p_near["areaCssSq"], rel_tol=2 * MARKER_RATIO_TOL), (p_near, p_far)
    # two viewports with different canvas sizes agree with each other
    vp_near, vp_far = v["viewport"]["near"], v["viewport"]["far"]
    assert vp_near["canvasCss"] != p_near["canvasCss"], (vp_near["canvasCss"], p_near["canvasCss"])
    assert math.isclose(vp_far["widthCss"], vp_near["widthCss"], rel_tol=MARKER_RATIO_TOL), (vp_near, vp_far)
    assert math.isclose(vp_near["widthCss"], p_near["widthCss"], rel_tol=MARKER_RATIO_TOL), (vp_near, p_near)

    # ------------------------------------------------ teeth: the same measurement on the old design
    # A marker of a fixed WORLD size (the old 3e-4-radius sphere, here 10 px across at the far
    # distance) is a point at the far distance and fills the window at the near one: the ratio
    # the assertions above bound at 10 % is more than 5x here, in both viewports.
    a_near, a_far = v["primary"]["attenuatedNear"], v["primary"]["attenuatedFar"]
    assert abs(a_far["widthCss"] - MARKER_DIAMETER_PX) <= MARKER_DIAMETER_TOL_PX, a_far  # calibration: 10 px at the far distance
    assert a_near["widthCss"] >= 5 * a_far["widthCss"], (
        f"the measurement cannot tell a size-attenuated marker from a screen-space one: {a_near} vs {a_far}"
    )
    assert not math.isclose(a_near["widthCss"], a_far["widthCss"], rel_tol=MARKER_RATIO_TOL)
    assert v["viewport"]["attenuatedNear"]["widthCss"] >= 3 * MARKER_DIAMETER_PX, v["viewport"]["attenuatedNear"]

    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

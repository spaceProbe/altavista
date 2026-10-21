"""Question 254 item 3b (heavy open-items round): the spacecraft markers and event meshes are a fixed
size on screen IN EVERY VIEW that draws them, with the clock running, proven in a REAL headless Chrome
against a REAL `python -m altavista serve`.

The defect: each marker is ONE object drawn by every viewport, and `update(t)` scaled it once per
tick from the closest camera of all views, measured before `_updateViewport` moved the extra
viewports' cameras. With the clock running a close-up viewport (cameras follow focus) drew the marker
sized for where its camera was a tick earlier -- thousands of pixels -- and a viewport farther than the
closest camera drew it sub-pixel. `Viewer._sizeMarkers(vp)` now sizes them for the view about to draw,
from that view's own camera, canvas height and fov.

What this does (the same kernel as the node check, web/js/marker_pixel_size_check.mjs, imported into
the page), at LEO 7,000 km, GEO 42,164 km and lunar 384,400 km, with the primary after `fit()` and
framed on the Chaser: 24 ticks of a running clock through the page's REAL viewer and REAL WebGL
renderers (the primary, viewport A minted like the layout manager's "3D Viewport" pane ~10 m from the
Chaser, viewport B farther), and after EACH draw, each marker's and event mesh's radius in pixels from
the drawn state (the float32 model-view matrix, the view's own canvas height and the camera's
projection) against the declared 3.5 px / 4.5 px; a camera moved and a canvas resized within one
epoch; the cost of sizing one view. Ends with the page's console clean.

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

SCENARIO_NAME = "Marker pixel size"
MU_EARTH_KM3_S2 = 398600.4418
INCLINATION_DEG = 51.6
EPOCH_A1MJD = 34000.0
STEP_S = 5.0
N_SAMPLES = 400
CHASER_LEAD_M = 30.0
MODEL_URL = "/js/fixtures/entity_model_fixture.gltf"
COV_DIAG_KM2 = [1.0, 0.25, 0.0625]
DISTANCES = (("LEO", 7000.0), ("GEO", 42164.0), ("lunar", 384400.0))
TOLERANCE = 0.001


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
    covariance, plus one event on the Chaser in the middle of the measured window."""
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
    deadline a parameter."""
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
  const out = { step: 'start', runs: {} };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const M = await import('/js/entities_viewport_precision_check.mjs');
    const P = await import('/js/marker_pixel_size_check.mjs');
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
    cB.style.cssText = 'position:fixed;left:300px;top:0;width:400px;height:640px;z-index:-1';
    document.body.appendChild(cB);
    const B = viewer.addViewport('B', cB);
    B.controls.enableDamping = false;
    await frames(3);
    viewer.resize();
    out.env = {
      userAgent: navigator.userAgent, viewports: viewer.viewportList(), devicePixelRatio: window.devicePixelRatio,
      canvases: { primary: [viewer.canvas.clientWidth, viewer.canvas.clientHeight], A: [A.canvas.clientWidth, A.canvas.clientHeight], B: [B.canvas.clientWidth, B.canvas.clientHeight] },
    };

    const settleScenario = async (id) => {
      viewer.setScenario(SC[id]);
      await until(() => { const e = viewer._entityModelEntities.get('Chaser'); return !!(e && e.modelLoaded); }, 'the Chaser model to load');
      await until(() => viewer._entityMarkerResidentNames.length === 2 && viewer._entityTrailLines.size === 2, 'the markers and trails to be admitted');
      await frames(3);
    };

    for (const D of M.DISTANCES) {
      await settleScenario(D.id);
      for (const mode of ['fit', 'framed']) {
        // all synchronous from here to the end of the run: the page's own frame loop cannot interleave
        M.arrange(viewer, A, B, mode);
        M.disableCulling(viewer);
        const key = D.id + '/' + mode;
        const dist = P.viewDistancesM(viewer, M.T0_DAYS);
        const rec = P.sizeRun(viewer);
        const checks = [];
        const check = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
        P.judgeSizes(key, rec, check);
        const leftBad = P.formulaMismatches(viewer, viewer.canvas.clientHeight || viewer._referenceCanvasHeight());
        check(key + ': with two extra viewports the tick leaves the primary\'s own sizing in the scene graph (' + leftBad.length + ' differ)', leftBad.length === 0, leftBad.slice(0, 3));
        const within = {};
        for (const [name, view] of [['primary', null], ['A', A], ['B', B]]) {
          within[name] = P.moveWithinEpoch(viewer, view);
          P.judgeWithinEpoch(key, name, within[name], check);
        }
        const cost = { primary: P.timeSizing(viewer, null), A: P.timeSizing(viewer, A), B: P.timeSizing(viewer, B) };
        out.runs[key] = { distancesM: dist, rows: rec.rows(), checks, within, cost };

        if (mode === 'fit') {
          // the same, with the PAGE's own clock playing (1 min/s) and its own requestAnimationFrame
          // loop ticking (`app.js` frame(): clock -> viewer.update(t) -> every viewport), measured
          // after every real draw of the three views over >= 25 frames
          const speed = document.getElementById('speed');
          speed.value = '60'; speed.dispatchEvent(new Event('change'));
          const lrec = new P.SizeRecorder();
          const undoL = P.instrumentSizes(viewer, lrec);
          const play = document.getElementById('btn-play');
          play.click();
          await frames(2); // the page's loop is now ticking from the clock's own time
          const tStart = viewer._lastT;
          const lcheck = (name, pass, detail) => checks.push({ name, pass: !!pass, detail });
          try {
            await until(() => lrec.rows().length === 9 && lrec.rows().every((r) => r.n >= 25), 'the page\'s own playing clock to draw 25 frames', 120000);
          } finally {
            play.click(); // pause
            undoL();
          }
          const advancedS = (viewer._lastT - tStart) * 86400;
          P.judgeSizes(key + ' [the page\'s own clock playing]', lrec, lcheck);
          lcheck(key + ' [the page\'s own clock playing]: the clock advanced (' + advancedS.toFixed(1) + ' s of simulated time over the run)', advancedS > 5, advancedS);
          out.runs[key].live = { rows: lrec.rows(), advancedS };
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


def test_every_view_draws_every_marker_at_its_declared_size_with_the_clock_running(live_server):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, v = asyncio.run(_drive(live_server.url, chrome_path, _probe_js(), wait_s=4.0))

    assert v is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not v.get("error"), f"probe reported an error: {v.get('error')}\nconsole: {errors}"
    assert v["step"] == "done", v
    print("\nenvironment:", json.dumps(v["env"], sort_keys=True), "viewports removed first:", v["removedViewports"])
    assert sorted(v["env"]["viewports"]) == ["A", "B"], v["env"]["viewports"]
    # the views have real, differently sized canvases (the sizing is per canvas height)
    heights = {k: c[1] for k, c in v["env"]["canvases"].items()}
    assert heights["A"] == 500 and heights["B"] == 640 and heights["primary"] > 0, heights
    assert len(set(heights.values())) == 3, heights

    total = passed = 0
    failures: list[str] = []
    for key, run in v["runs"].items():
        d = run["distancesM"]
        worst = {}
        for r in run["rows"]:
            k = (r["view"], r["kind"])
            worst[k] = max(worst.get(k, 0.0), r["maxDev"])
            assert r["n"] >= 24, r
        print(f"{key}: A {d['A']['Chaser']:.1f} m from the Chaser, B {d['B']['Target']:.0f} m from the Target; "
              + ", ".join(f"{view} {kind} max dev {dev * 100:.4f}%" for (view, kind), dev in sorted(worst.items()))
              + f"; sizing median {run['cost']['primary']['median']:.5f} ms (primary), "
              f"{run['cost']['A']['median']:.5f} ms (A), {run['cost']['B']['median']:.5f} ms (B)")
        assert 2.0 < d["A"]["Chaser"] < 40.0 and d["B"]["Target"] > 5 * d["A"]["Chaser"], d
        assert len(run["rows"]) == 9, [(r["view"], r["kind"], r["name"]) for r in run["rows"]]
        if "live" in run:
            live = run["live"]
            lworst = {}
            for r in live["rows"]:
                lworst[(r["view"], r["kind"])] = max(lworst.get((r["view"], r["kind"]), 0.0), r["maxDev"])
                if r["maxDev"] > TOLERANCE:
                    failures.append(f"{key} [page clock]: {r['view']} {r['kind']} {r['name']} drawn {r['minPx']}..{r['maxPx']} px, max deviation {r['maxDev']:.4%}")
            print(f"{key} [the page's own clock playing, {live['advancedS']:.0f} s simulated, "
                  f"{min(r['n'] for r in live['rows'])}+ draws per object]: "
                  + ", ".join(f"{view} {kind} max dev {dev * 100:.4f}%" for (view, kind), dev in sorted(lworst.items())))
        for r in run["rows"]:
            if r["maxDev"] > TOLERANCE:
                failures.append(f"{key}: {r['view']} {r['kind']} {r['name']} drawn {r['minPx']}..{r['maxPx']} px, max deviation {r['maxDev']:.4%}")
        for c in run["checks"]:
            total += 1
            passed += bool(c["pass"])
            if not c["pass"]:
                failures.append(f"{c['name']}: {json.dumps(c['detail'])[:300]}")
        for view, cost in run["cost"].items():
            if cost["median"] >= 0.05:
                failures.append(f"{key}: sizing {view} costs {cost['median']} ms (budget 0.05)")
    print(f"real-Chrome marker pixel-size checks: {passed}/{total} passed")

    assert failures == [], "\n".join(failures)
    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions, got {len(errors)}:\n" + "\n".join(errors)
    )

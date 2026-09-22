"""Heavy round 7 (question 233): "H6 is accepted as a module with proofs, not as a
viewer feature: nothing under `web/js/entities/` is wired into `scene.js`/`app.js`...
the plan closes only after the lead sees an entity drawn from a run." This file is that
proof, driven against a REAL running `python -m altavista serve` and a REAL headless
Chrome, following `tests/test_viewer_globe_layer_manager.py`'s own harness exactly (the
`_find_chrome`/`_free_port`/`_LiveServer`/`_drive` helpers below are that file's,
duplicated per this repo's own existing convention -- every other browser-driving test
file in this tree duplicates the same block rather than sharing a conftest.py).

Two scenarios are published:

  - "RPO demo": a REAL GMAT run, via `examples/05_rpo_ric.py`'s own recipe (built
    in-process here rather than shelled out to, so the fixture can publish it at a
    known port -- identical Scenario/spacecraft/propagate/frame_ric calls, same
    numbers). Used for markers/trails visibility, the per-class toggle behaviour, and
    the live RIC-frame jitter bound (question 46) -- exactly the "drawn from a run"
    proof question 233 asks for.
  - "RPO demo + entities fixture": the SAME real GMAT-propagated scenario, fetched back
    and given two additive fields no producer populates yet for a real run (confirmed
    directly against `altavista/scenario.py` -- it never sets `Trajectory.cov`/
    `cov_dim`, and `Trajectory.model` is never emitted by `to_dict()` at all, see this
    task's own report): `Target` gets a real, closed-form-known diagonal covariance
    (`cov`/`covDim`) at every native sample, and `Chaser` gets `model` pointing at the
    already-committed `web/js/fixtures/entity_model_fixture.gltf` (served statically at
    `/js/fixtures/entity_model_fixture.gltf`). This is the "fixture scenario you
    publish yourself" this task's own brief calls for -- real orbital motion (so a VVLH
    fallback attitude genuinely varies over time), not a toy straight line.
"""
from __future__ import annotations

import asyncio
import copy
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

RPO_SCENARIO_NAME = "RPO demo"
FIXTURE_SCENARIO_NAME = "RPO demo + entities fixture"
MODEL_FIXTURE_URL = "/js/fixtures/entity_model_fixture.gltf"
ELLIPSOID_SIGMA = 3
KEEPOUT_MARGIN_KM = 0.050
# Same closed-form diagonal covariance web/js/entities_scene_check.mjs uses -- semi-axes
# are the diagonal entries' own square roots (no eigenvector rotation to account for).
COV_DIAG_KM2 = [0.000064, 0.000016, 0.000004]
EXPECTED_ELLIPSOID_SEMI_AXES_KM = sorted(((v ** 0.5) * ELLIPSOID_SIGMA for v in COV_DIAG_KM2), reverse=True)


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


def _http_get_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


def _http_post_json(url: str, payload: dict) -> None:
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        assert r.status in (200, 201), f"publishing to {url} answered HTTP {r.status}"


def _build_and_publish_fixture_scenario(base_url: str) -> None:
    """Real GMAT run (examples/05_rpo_ric.py's own recipe, built in-process so it can
    target this fixture's own port) -> "RPO demo", then a second scenario derived from
    the SAME real trajectories with `cov`/`model` added -> "RPO demo + entities
    fixture" (see this file's own module docstring for why -- no real producer
    populates either field for a Python-scenario run today)."""
    import altavista as gv

    sc = gv.Scenario("RPO demo", frame="EarthMJ2000Eq")
    target = sc.spacecraft(
        "Target", epoch="01 Jan 2026 00:00:00.000",
        keplerian=dict(SMA=6878.0, ECC=0.0005, INC=51.6, RAAN=45.0, AOP=0.0, TA=0.0),
        DryMass=500.0, color="#54a0ff",
    )
    tx, ty, tz, tvx, tvy, tvz = target.cartesian()
    speed = (tvx ** 2 + tvy ** 2 + tvz ** 2) ** 0.5
    ux, uy, uz = tvx / speed, tvy / speed, tvz / speed
    chaser = sc.spacecraft(
        "Chaser", epoch="01 Jan 2026 00:00:00.000",
        cartesian=[tx + 0.030 * ux, ty + 0.030 * uy, tz + 0.030 * uz, tvx, tvy, tvz],
        DryMass=450.0, color="#ff6b6b",
    )
    sc.propagate([target, chaser], hours=2.33, step=15)
    sc.frame_ric(target)
    sc.publish(url=base_url)

    import urllib.parse

    published = _http_get_json(
        base_url.rstrip("/") + f"/api/scenario/{urllib.parse.quote(RPO_SCENARIO_NAME)}"
    )
    fixture = copy.deepcopy(published)
    fixture["name"] = FIXTURE_SCENARIO_NAME

    for spacecraft in fixture["spacecraft"]:
        if spacecraft["name"] == "Target":
            n_samples = len(spacecraft["t"])
            block = [
                COV_DIAG_KM2[0], 0.0, 0.0,
                0.0, COV_DIAG_KM2[1], 0.0,
                0.0, 0.0, COV_DIAG_KM2[2],
            ]
            spacecraft["cov"] = block * n_samples
            spacecraft["covDim"] = 3
        elif spacecraft["name"] == "Chaser":
            spacecraft["model"] = MODEL_FIXTURE_URL

    _http_post_json(base_url.rstrip("/") + "/api/scenario", fixture)


@pytest.fixture()
def live_server():
    port = _free_port()
    env = dict(os.environ)
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT), env=env,
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
        _build_and_publish_fixture_scenario(server.url)
    except Exception as exc:  # pragma: no cover - surfaced as a test failure, never hidden
        server.stop()
        pytest.fail(f"could not build/publish the proof scenarios: {exc}")

    try:
        yield server
    finally:
        server.stop()


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 6.0):
    """Identical to tests/test_viewer_globe_layer_manager.py's own `_drive` -- see that
    file's own module docstring for why all three CDP error channels matter (question
    211)."""
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
                    deadline = time.monotonic() + 60
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


# The page script: load the REAL GMAT-run "RPO demo" scenario, tick real frames, then
# read the ANSWER OFF THE SCENE GRAPH (never a counter alone, this round's own rule).
_PROBE_JS = r"""
(async () => {
  const out = { step: 'start' };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three'); // resolved via index.html's own <script type="importmap">, same as every module this page already loaded
    const { SCALE } = await import('/js/scene.js');

    // ---------------------------------------------------------------- RPO demo (real run)
    const rpoSc = await (await fetch('/api/scenario/' + encodeURIComponent('RPO demo'))).json();
    viewer.setScenario(rpoSc);
    viewer.setViewFrame('Target_ric', 'Chaser');
    let t = rpoSc.t0;
    for (let i = 0; i < 40; i++) {
      viewer.update(t);
      await new Promise((r) => requestAnimationFrame(r));
    }
    out.entityOptionsDefault = { ...viewer.entityOptions };

    // markers/trails: present, right count, provenance tag
    let markerMeshFound = false, markerCount = 0, markerProv = null, markerVisible = null;
    viewer._entityGroups.markers.traverse((o) => {
      if (o.isInstancedMesh) { markerMeshFound = true; markerCount = o.count; markerProv = o.userData.sourceLayerId; }
    });
    markerVisible = viewer._entityGroups.markers.visible;
    let trailLineCount = 0; const trailProv = [];
    viewer._entityGroups.trails.traverse((o) => { if (o.isLine) { trailLineCount += 1; trailProv.push(o.userData.sourceLayerId); } });
    out.rpo = {
      markerMeshFound, markerCount, markerProv, markerVisible, trailLineCount, trailProv,
      residentMarkerNames: viewer._entityMarkerResidentNames.slice(),
    };

    // jitter (question 46): a drawn marker instance's WORLD position, reconstructed
    // from the scene graph, vs. the TRUE absolute position computed directly from the
    // same TrajectoryInterp the viewer itself holds -- never re-derived from the
    // rendered value.
    const idx = viewer._entityMarkerResidentNames.indexOf('Chaser');
    if (idx >= 0) {
      const m4 = new THREE.Matrix4();
      viewer._entityMarkerMesh.getMatrixAt(idx, m4);
      viewer._entityMarkerMesh.updateMatrixWorld(true);
      const world = new THREE.Matrix4().multiplyMatrices(viewer._entityMarkerMesh.matrixWorld, m4);
      const pos = new THREE.Vector3().setFromMatrixPosition(world);
      const renderedAbsKm = [pos.x / SCALE, pos.y / SCALE, pos.z / SCALE];
      const trueAbs = new THREE.Vector3();
      viewer.spacecraft.get('Chaser').interp.at(t, trueAbs);
      const errKm = Math.hypot(renderedAbsKm[0] - trueAbs.x, renderedAbsKm[1] - trueAbs.y, renderedAbsKm[2] - trueAbs.z);
      out.rpo.jitterErrM = errKm * 1000;
      out.rpo.renderedAbsKm = renderedAbsKm;
      out.rpo.trueAbsKm = [trueAbs.x, trueAbs.y, trueAbs.z];
    } else {
      out.rpo.jitterErrM = null;
    }

    // toggle: markers off must remove exactly markers, nothing else
    const beforeToggle = {
      trails: viewer._entityGroups.trails.visible,
      cov: viewer._entityGroups.covarianceEllipsoids.visible,
      keepout: viewer._entityGroups.keepOutVolumes.visible,
      models: viewer._entityGroups.models.visible,
    };
    viewer.setEntityClassEnabled('markers', false);
    viewer.update(t);
    out.rpo.toggleOff = {
      markersVisible: viewer._entityGroups.markers.visible,
      othersUnchanged: viewer._entityGroups.trails.visible === beforeToggle.trails
        && viewer._entityGroups.covarianceEllipsoids.visible === beforeToggle.cov
        && viewer._entityGroups.keepOutVolumes.visible === beforeToggle.keepout
        && viewer._entityGroups.models.visible === beforeToggle.models,
    };
    viewer.setEntityClassEnabled('markers', true); // restore

    // ---------------------------------------------------------- entities fixture (cov/model)
    const fixSc = await (await fetch('/api/scenario/' + encodeURIComponent('RPO demo + entities fixture'))).json();
    viewer.setScenario(fixSc);
    viewer.setEntityClassEnabled('covarianceEllipsoids', true);
    viewer.setEntityClassEnabled('keepOutVolumes', true);
    viewer.setEntityClassEnabled('models', true);
    const t2 = fixSc.t0;
    let modelLoaded = false;
    for (let i = 0; i < 200 && !modelLoaded; i++) {
      viewer.update(t2);
      await new Promise((r) => requestAnimationFrame(r));
      const entity = viewer._entityModelEntities.get('Chaser');
      modelLoaded = !!(entity && entity.modelLoaded);
    }
    out.fixture = { modelLoaded };

    // ellipsoid + keepout: found, tagged, world semi-axes
    let ellMesh = null, koMesh = null;
    viewer._entityGroups.covarianceEllipsoids.traverse((o) => { if (o.isMesh) ellMesh = o; });
    viewer._entityGroups.keepOutVolumes.traverse((o) => { if (o.isMesh) koMesh = o; });
    if (ellMesh) {
      ellMesh.updateWorldMatrix(true, false);
      const s = new THREE.Vector3(), p = new THREE.Vector3(), q = new THREE.Quaternion();
      ellMesh.matrixWorld.decompose(p, q, s);
      out.fixture.ellipsoid = {
        found: true, sourceLayerId: ellMesh.userData.sourceLayerId, kind: ellMesh.userData.kind,
        worldScaleSceneUnits: [s.x, s.y, s.z], visible: ellMesh.visible,
      };
    } else { out.fixture.ellipsoid = { found: false }; }
    if (koMesh) {
      koMesh.updateWorldMatrix(true, false);
      const s = new THREE.Vector3(), p = new THREE.Vector3(), q = new THREE.Quaternion();
      koMesh.matrixWorld.decompose(p, q, s);
      out.fixture.keepout = {
        found: true, sourceLayerId: koMesh.userData.sourceLayerId, kind: koMesh.userData.kind,
        worldScaleSceneUnits: [s.x, s.y, s.z], visible: koMesh.visible,
      };
    } else { out.fixture.keepout = { found: false }; }

    // model: attached, tagged, attitude tracks (quaternion changes over the orbit) --
    // an EXPLICIT baseline tick at t2 first (never trust whatever quaternion the
    // loading loop above happened to leave it at, which could itself already be
    // mid-transition), then a real, large epoch jump (~0.3 days, several LEO orbits)
    // so a VVLH-fallback rotation is unmistakable, not a rounding-sized artifact.
    // Deliberately NO `await` between a `viewer.update(t)` call and reading its
    // result: `web/js/app.js`'s OWN background requestAnimationFrame loop is also
    // running on this same page (this probe drove the Viewer directly, bypassing
    // app.js's own `scenario`/`clock` state, exactly like
    // tests/test_viewer_globe_layer_manager.py's own probe already does) and calls
    // `viewer.update(0)` on every frame while app.js's own `scenario` stays null --
    // yielding to the event loop between "set t" and "read the result" lets that
    // OTHER call land in between and overwrite it with t=0's state (found live,
    // debugging this exact test: a `viewer.update(t1)` followed by `await
    // requestAnimationFrame` reproducibly reverted the quaternion back to t0's value).
    // Reading synchronously, right after the update() call that set it, is immune --
    // JS's own run-to-completion semantics guarantee nothing else runs in between.
    const modelEntity = viewer._entityModelEntities.get('Chaser');
    if (modelEntity && modelEntity.modelLoaded) {
      viewer.update(t2);
      const q1 = modelEntity.group.quaternion.clone();
      const bodyNodeQ1 = viewer.frameGraph.frame('Chaser_body').object3D.quaternion.clone();
      viewer.update(t2 + 0.3);
      const q2 = modelEntity.group.quaternion.clone();
      const bodyNodeQ2 = viewer.frameGraph.frame('Chaser_body').object3D.quaternion.clone();
      out.fixture.model = {
        attached: true, sourceLayerId: modelEntity.group.userData.sourceLayerId,
        visible: modelEntity.group.visible,
        quaternionChanged: q1.angleTo(q2) > 1e-3,
        bodyNodeQuaternionChanged: bodyNodeQ1.angleTo(bodyNodeQ2) > 1e-3,
        entityMatchesBodyNode: q2.angleTo(bodyNodeQ2) < 1e-9,
        meshChildCount: modelEntity.group.children.length,
      };
    } else {
      out.fixture.model = { attached: false };
    }

    // console/exception counts are read by the Python side from the CDP channel, not here
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def test_entities_drawn_from_a_real_run(live_server):
    """The production path, in a real browser: a real GMAT-propagated scenario is
    loaded, markers/trails are drawn through the ONE shared LayerManager with real
    provenance tags, the RIC-frame jitter bound holds at the RPO range, a toggle
    removes exactly one class, and (against the entities fixture built from the same
    real run) a covariance ellipsoid/keep-out volume/glTF model with tracking
    attitude are all real, drawn scene-graph objects -- not merely counted.
    """
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    errors, value = asyncio.run(_drive(live_server.url, chrome_path, _PROBE_JS, wait_s=6.0))

    print("\nentities browser probe:", json.dumps(value, indent=2, sort_keys=True))
    print("console/exception messages:", errors)

    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"

    # -------------------------------------------------------------------- defaults
    assert value["entityOptionsDefault"] == {
        "markers": True, "trails": True, "covarianceEllipsoids": False,
        "keepOutVolumes": False, "models": False,
    }, f"entity class defaults do not match this round's own table: {value['entityOptionsDefault']!r}"

    # ---------------------------------------------------------------- markers/trails
    rpo = value["rpo"]
    assert rpo["markerMeshFound"] is True, f"no entity marker InstancedMesh in the scene graph: {rpo!r}"
    assert rpo["markerCount"] == 2, f"expected 2 resident entity markers (Target, Chaser): {rpo!r}"
    assert rpo["markerProv"] == "entity-markers", f"marker mesh missing/wrong provenance tag: {rpo!r}"
    assert set(rpo["residentMarkerNames"]) == {"Target", "Chaser"}, rpo["residentMarkerNames"]
    assert rpo["trailLineCount"] == 2, f"expected 2 entity trail lines: {rpo!r}"
    assert all(p == "entity-trails" for p in rpo["trailProv"]), f"a trail line is missing its provenance tag: {rpo!r}"

    # -------------------------------------------------------------- jitter (question 46)
    assert rpo["jitterErrM"] is not None, "jitter measurement did not run (Chaser marker not resident)"
    assert rpo["jitterErrM"] < 0.01, (
        f"entity marker world position vs. true absolute position exceeded the centimetre "
        f"bound in the live RIC-frame scene: {rpo['jitterErrM']} m "
        f"(rendered={rpo['renderedAbsKm']!r} true={rpo['trueAbsKm']!r})"
    )

    # -------------------------------------------------------------------------- toggle
    assert rpo["toggleOff"]["markersVisible"] is False, "turning markers off did not clear the markers group's own visibility"
    assert rpo["toggleOff"]["othersUnchanged"] is True, f"toggling markers off changed some OTHER class's visibility: {rpo['toggleOff']!r}"

    # --------------------------------------------------------- ellipsoid / keep-out / model
    fx = value["fixture"]
    assert fx["ellipsoid"]["found"] is True, f"no covariance ellipsoid mesh drawn: {fx!r}"
    assert fx["ellipsoid"]["sourceLayerId"] == "entity-ellipsoids"
    assert fx["ellipsoid"]["kind"] == "covariance-ellipsoid"
    assert fx["ellipsoid"]["visible"] is True
    got_ell = sorted(fx["ellipsoid"]["worldScaleSceneUnits"], reverse=True)
    expected_ell_scene_units = [v * (1e-3) for v in EXPECTED_ELLIPSOID_SEMI_AXES_KM]  # SCALE = 1e-3 scene units/km
    for got, exp in zip(got_ell, expected_ell_scene_units):
        assert math.isclose(got, exp, rel_tol=1e-6, abs_tol=1e-12), (
            f"ellipsoid world semi-axes (scene units) do not match the closed-form covariance: "
            f"got={got_ell} expected={expected_ell_scene_units}"
        )

    assert fx["keepout"]["found"] is True, f"no keep-out volume mesh drawn: {fx!r}"
    assert fx["keepout"]["sourceLayerId"] == "entity-keepout"
    assert fx["keepout"]["kind"] == "keepout-volume"
    assert fx["keepout"]["visible"] is True
    got_ko = sorted(fx["keepout"]["worldScaleSceneUnits"], reverse=True)
    expected_ko_scene_units = [(v + KEEPOUT_MARGIN_KM) * 1e-3 for v in EXPECTED_ELLIPSOID_SEMI_AXES_KM]
    for got, exp in zip(got_ko, expected_ko_scene_units):
        assert math.isclose(got, exp, rel_tol=1e-6, abs_tol=1e-12), (
            f"keep-out world semi-axes (scene units) do not equal the ellipsoid's plus the "
            f"{KEEPOUT_MARGIN_KM} km margin: got={got_ko} expected={expected_ko_scene_units}"
        )

    assert fx["modelLoaded"] is True, "the glTF model entity never finished loading within the probe's own timeout"
    assert fx["model"]["attached"] is True, f"model entity did not report modelLoaded: {fx!r}"
    assert fx["model"]["sourceLayerId"] == "entity-models"
    assert fx["model"]["visible"] is True
    assert fx["model"]["meshChildCount"] > 0, "the loaded glTF's own scene graph has no children attached"
    assert fx["model"]["quaternionChanged"] is True, (
        "the model entity's quaternion did not change between two real orbital epochs -- attitude is not tracking "
        f"the body-frame node: {fx['model']!r}"
    )

    # ------------------------------------------------------------------------- console-clean
    assert errors == [], (
        f"expected zero console warnings/errors/page exceptions/unhandled rejections, got {len(errors)}:\n"
        + "\n".join(errors)
    )


def test_the_exception_collector_sees_an_uncaught_exception():
    """Teeth for the detection mechanism this file's own console-clean assertion
    depends on -- identical to tests/test_viewer_globe_layer_manager.py's own test of
    the same shape (question 211)."""
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
        f"the collector did NOT report an uncaught exception; got: {errors!r}"
    )

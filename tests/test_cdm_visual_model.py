"""Question 239: a run produced by the kernel and published by ``av-run`` can carry a model.

``SystemInstance.visual_model_uri`` (``proto/altavista/v1/system.proto`` field 9) is the DRM's
declaration of the glTF the viewer draws for the entity an instance embodies; the executor
copies it onto ``Trajectory.visual_model_uri`` (``trajectory.proto`` field 12);
``altavista.cdm.cdm_trajectory_to_viewer_json`` turns a non-empty one into
``altavista.model.Trajectory.model`` (validated by ``check_model_ref``), so the published
scenario's spacecraft carries ``model`` and the viewer, which already draws ``model``, draws it.

What is proven here:

* adapter, in-process: ``model`` is emitted when the CDM trajectory declares one and omitted when
  it does not; the model-less viewer JSON is byte-identical to a literal captured from the tree
  as it was BEFORE the field existed (``PRE_CHANGE_VIEWER_JSON``: produced by running the same
  fixed trajectory through ``cdm_trajectory_to_viewer_json`` in the ``develop`` checkout); an
  invalid value is refused with ``CdmAdapterError``;
* both publishing routes, ``POST /api/cdm/run`` and ``POST /api/cdm/trajectory``, pass it through;
* end to end on the real binary: ``av-run`` on ``drms/leo_1day_orbital_native_model.*`` (the
  smallest GMAT-free bundle declaring a model) is published to a live ``python -m altavista
  serve``; the published scenario's spacecraft carries ``model``; and in real headless Chrome, with
  the viewer's ``models`` entity class on, the glTF loads and measures 1.000 x 1.000 x 1.500 m.
  The same run through the model-less bundle publishes no ``model`` key and the viewer loads none.

Run it holding a cargo slot, like every test that launches workspace binaries::

    scripts/dev/cargo-slot --hold -- .venv/bin/python -m pytest -q -rs tests/test_cdm_visual_model.py

The browser half skips (visibly, under ``-rs``) only when no Chrome is installed.
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
import urllib.parse
import urllib.request
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista import cdm as cdm_adapter
from altavista.pb import trajectory_pb2
from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app
from altavista.test_env import resolve_gmat_root

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_SLOT = REPO_ROOT / "scripts" / "dev" / "cargo-slot"
AV_RUN_BIN = REPO_ROOT / "target" / "debug" / "av-run"
DRMS = REPO_ROOT / "drms"
SYSTEM_PATH = DRMS / "leo_1day_orbital_native.system.yaml"
MODEL_BUNDLE = ("leo_1day_orbital_native_model.drm.yaml", "leo_1day_orbital_native_model.sos.yaml")
PLAIN_BUNDLE = ("leo_1day_orbital_native.drm.yaml", "leo_1day_orbital_native.sos.yaml")

MODEL_FIXTURE_URL = "/js/fixtures/entity_model_fixture.gltf"
# The fixture's own bounds, [0,0,0]-[1,1,1.5] m (web/js/fixtures/entity_model_fixture.gltf).
MODEL_FIXTURE_SIZE_M = [1.0, 1.0, 1.5]

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"
BUILD_TIMEOUT_S = 3600
RUN_TIMEOUT_S = 300

CHROME_CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
]

# `cdm_trajectory_to_viewer_json(_fixed_cdm_trajectory()).to_dict()`, serialised with json.dumps,
# captured by running the same fixed trajectory through the `develop` checkout (the tree before
# `visual_model_uri` existed). A model-less trajectory must still serialise to exactly this.
PRE_CHANGE_VIEWER_JSON = (
    '{"name": "leo", "label": "leo", "color": null, "t": [31041.50042863868, 31041.503900860902, '
    '31041.507373083125], "pos": [6878.0, 0.0, -0.0, 6879.0, 0.25, -0.1255, 6880.0, 0.5, -0.251], '
    '"vel": [0.0, 7.61225, 0.1, 0.0005, 7.61225, 0.101, 0.001, 7.61225, 0.102], "attitude": [], '
    '"cov": [], "covDim": 0, "stateSpaceId": "altavista.cartesian_pos_vel_6"}'
)


def _fixed_cdm_trajectory() -> trajectory_pb2.Trajectory:
    t = trajectory_pb2.Trajectory(
        id="leo-trajectory", entity_id="leo", state_space_id="gmat.orbital.cartesian6",
        frame_id="EarthMJ2000Eq", interpolation=trajectory_pb2.INTERPOLATION_HERMITE_VELOCITY)
    base = 1767225637000000000
    for i in range(3):
        s = t.samples.add()
        s.tai_ns = base + i * 300_000_000_000
        s.mean.extend([6878000.0 + 1000.0 * i, 250.0 * i, -125.5 * i, 0.5 * i, 7612.25, 100.0 + i])
    return t


# --------------------------------------------------------------------------- adapter (in-process)
def test_a_model_less_trajectory_serialises_byte_identically_to_the_pre_change_capture():
    tr = cdm_adapter.cdm_trajectory_to_viewer_json(_fixed_cdm_trajectory())
    assert tr.model is None
    assert "model" not in tr.to_dict()
    assert json.dumps(tr.to_dict()) == PRE_CHANGE_VIEWER_JSON


def test_a_declared_visual_model_uri_becomes_the_viewer_trajectorys_model():
    cdm = _fixed_cdm_trajectory()
    cdm.visual_model_uri = MODEL_FIXTURE_URL
    tr = cdm_adapter.cdm_trajectory_to_viewer_json(cdm)
    assert tr.model == MODEL_FIXTURE_URL
    d = tr.to_dict()
    assert d["model"] == MODEL_FIXTURE_URL
    # Nothing else moves: the model-less capture plus exactly one `model` key.
    expected = json.loads(PRE_CHANGE_VIEWER_JSON)
    expected["model"] = MODEL_FIXTURE_URL
    assert d == expected


@pytest.mark.parametrize("bad", [" ", "\t\n"])
def test_an_invalid_visual_model_uri_is_refused(bad):
    cdm = _fixed_cdm_trajectory()
    cdm.visual_model_uri = bad
    with pytest.raises(cdm_adapter.CdmAdapterError, match="visual_model_uri"):
        cdm_adapter.cdm_trajectory_to_viewer_json(cdm)


def test_cdm_trajectory_route_passes_the_model_through_and_refuses_an_invalid_one(tmp_path):
    client = TestClient(create_app(texture_dir=tmp_path, web_dir=tmp_path))
    cdm = _fixed_cdm_trajectory()
    cdm.visual_model_uri = MODEL_FIXTURE_URL
    resp = client.post("/api/cdm/trajectory", content=cdm.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    scenario = client.get(f"/api/scenario/{resp.json()['name']}").json()
    assert [s.get("model") for s in scenario["spacecraft"]] == [MODEL_FIXTURE_URL]

    cdm.visual_model_uri = "  "
    resp = client.post("/api/cdm/trajectory", content=cdm.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400, resp.text
    assert "visual_model_uri" in resp.text


# --------------------------------------------------------------------------- av-run (real binary)
def _env() -> dict:
    gmat_root, reason = resolve_gmat_root()
    if gmat_root is None:
        pytest.fail(f"GMAT_ROOT does not resolve ({reason}); the native model reads its data files from it")
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    env["GMAT_ROOT"] = gmat_root
    env.setdefault("CARGO_BUILD_JOBS", "4")
    return env


@pytest.fixture(scope="module")
def av_run_bin() -> Path:
    proc = subprocess.run(
        [sys.executable, str(CARGO_SLOT), "build", "-p", "av-run", "--bin", "av-run"],
        cwd=str(REPO_ROOT), env=_env(), capture_output=True, text=True, timeout=BUILD_TIMEOUT_S)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-run failed (rc={proc.returncode}):\n"
                    f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    assert AV_RUN_BIN.is_file(), f"expected {AV_RUN_BIN} after a successful build"
    return AV_RUN_BIN


def _av_run(binary: Path, bundle, run_id: str, out: Path) -> bytes:
    drm, sos = bundle
    proc = subprocess.run(
        [str(binary), "--drm", str(DRMS / drm), "--sos", str(DRMS / sos), "--system", str(SYSTEM_PATH),
         "--run-id", run_id, "--out", str(out)],
        cwd=str(REPO_ROOT), env=_env(), capture_output=True, text=True, timeout=RUN_TIMEOUT_S)
    if proc.returncode != 0:
        pytest.fail(f"av-run failed on {drm} (rc={proc.returncode}):\n"
                    f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return out.read_bytes()


@pytest.fixture(scope="module")
def model_run_bytes(av_run_bin, tmp_path_factory) -> bytes:
    return _av_run(av_run_bin, MODEL_BUNDLE, "test-visual-model-declared", tmp_path_factory.mktemp("vm") / "model.bin")


@pytest.fixture(scope="module")
def plain_run_bytes(av_run_bin, tmp_path_factory) -> bytes:
    return _av_run(av_run_bin, PLAIN_BUNDLE, "test-visual-model-undeclared", tmp_path_factory.mktemp("vm") / "plain.bin")


def test_av_run_puts_the_declared_model_on_the_wire_trajectory(model_run_bytes, plain_run_bytes):
    declared = run_pb2.RunProducts()
    declared.ParseFromString(model_run_bytes)
    assert list(declared.trajectories) == ["leo"]
    assert declared.trajectories["leo"].visual_model_uri == MODEL_FIXTURE_URL
    plain = run_pb2.RunProducts()
    plain.ParseFromString(plain_run_bytes)
    assert plain.trajectories["leo"].visual_model_uri == ""
    # The only difference between the two runs' samples is none: the field changes nothing physical.
    assert list(declared.trajectories["leo"].samples[-1].mean) == list(plain.trajectories["leo"].samples[-1].mean)


def test_cdm_run_route_publishes_the_model_on_the_spacecraft(model_run_bytes, plain_run_bytes, tmp_path):
    client = TestClient(create_app(texture_dir=tmp_path, web_dir=tmp_path))
    for data, expected in ((model_run_bytes, MODEL_FIXTURE_URL), (plain_run_bytes, None)):
        resp = client.post("/api/cdm/run", content=data, headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        scenario = client.get(f"/api/scenario/{urllib.parse.quote(resp.json()['name'])}").json()
        assert len(scenario["spacecraft"]) == 1
        sc = scenario["spacecraft"][0]
        assert sc["name"] == "leo"
        if expected is None:
            assert "model" not in sc, "a model-less run must publish no model key"
        else:
            assert sc["model"] == expected


# --------------------------------------------------------------------------- real Chrome
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
        self.names: dict[str, str] = {}

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/"

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def _post_run(base_url: str, data: bytes) -> str:
    req = urllib.request.Request(base_url.rstrip("/") + "/api/cdm/run", data=data,
                                 headers={"Content-Type": "application/x-protobuf"}, method="POST")
    with urllib.request.urlopen(req, timeout=60) as r:
        assert r.status in (200, 201), f"publishing to /api/cdm/run answered HTTP {r.status}"
        return json.loads(r.read())["name"]


@pytest.fixture()
def live_server(model_run_bytes, plain_run_bytes):
    port = _free_port()
    proc = subprocess.Popen(
        [sys.executable, "-m", "altavista", "serve", "--host", "127.0.0.1", "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=str(REPO_ROOT), env=dict(os.environ))
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
        server.names["model"] = _post_run(server.url, model_run_bytes)
        server.names["plain"] = _post_run(server.url, plain_run_bytes)
    except Exception as exc:  # surfaced as a failure, never hidden
        server.stop()
        pytest.fail(f"could not publish the av-run products to POST /api/cdm/run: {exc}")
    try:
        yield server
    finally:
        server.stop()


async def _drive(url: str, chrome_path: str, eval_js: str, wait_s: float = 6.0):
    """The headless-Chrome driver of ``tests/test_viewer_entities_browser.py`` (duplicated per
    this repo's convention): all three CDP error channels are collected (question 211)."""
    with tempfile.TemporaryDirectory() as profile_dir:
        cdp_port = _free_port()
        chrome = subprocess.Popen(
            [chrome_path, f"--user-data-dir={profile_dir}", "--headless=new", f"--remote-debugging-port={cdp_port}",
             "--no-first-run", "--no-default-browser-check", "--disable-extensions", "--use-gl=swiftshader",
             "--enable-unsafe-swiftshader", "--window-size=1280,900", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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
                                exc = details.get("exception") or {}
                                errors.append(f"[Runtime.exceptionThrown] {details.get('text', 'exception')}: "
                                              f"{exc.get('description') or exc.get('value') or ''}")
                            elif msg.get("id") is not None and msg["id"] == state["want_id"]:
                                state["value"] = msg.get("result", {}).get("result", {}).get("value")
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


# The page script: load a PUBLISHED av-run scenario (fetched from the live server, as the app
# does), switch the `models` entity class on, wait for the entity's glTF to load (bounded on
# wall-clock time), and measure the drawn model's world extent in metres with the attitude
# temporarily identity -- the measurement `tests/test_viewer_entities_browser.py` makes.
_PROBE_JS_TEMPLATE = r"""
(async () => {
  const out = { step: 'start' };
  try {
    const viewer = window.altavistaViewer;
    if (!viewer) { out.error = 'no viewer on window'; return out; }
    const THREE = await import('three');
    const { SCALE } = await import('/js/scene.js');
    const sc = await (await fetch('/api/scenario/' + encodeURIComponent(__NAME__))).json();
    const json = sc.spacecraft.find((s) => s.name === 'leo');
    out.declaredModel = json && json.model !== undefined ? json.model : null;
    out.spacecraftNames = sc.spacecraft.map((s) => s.name);
    viewer.setScenario(sc);
    viewer.setEntityClassEnabled('models', true);
    const t = sc.t0;
    let loaded = false;
    const deadline = performance.now() + __WAIT_MS__;
    while (performance.now() < deadline && !loaded) {
      viewer.update(t);
      await new Promise((r) => requestAnimationFrame(r));
      const entity = viewer._entityModelEntities.get('leo');
      loaded = !!(entity && entity.modelLoaded);
    }
    const entity = viewer._entityModelEntities.get('leo');
    out.modelLoaded = loaded;
    out.hasModelEntity = !!entity;
    if (entity && entity.modelLoaded) {
      viewer.update(t);
      const g = entity.group;
      const savedQ = g.quaternion.clone();
      g.quaternion.identity();
      g.updateWorldMatrix(true, true);
      const size = new THREE.Box3().setFromObject(g).getSize(new THREE.Vector3());
      const wp = new THREE.Vector3(), wq = new THREE.Quaternion(), ws = new THREE.Vector3();
      g.matrixWorld.decompose(wp, wq, ws);
      g.quaternion.copy(savedQ);
      g.updateWorldMatrix(true, true);
      const mPerUnit = 1 / (1e-3 * SCALE);
      out.sourceLayerId = g.userData.sourceLayerId;
      out.meshChildCount = g.children.length;
      out.worldSizeMetres = [size.x * mPerUnit, size.y * mPerUnit, size.z * mPerUnit];
      out.groupWorldScale = [ws.x, ws.y, ws.z];
    }
    out.step = 'done';
  } catch (e) {
    out.error = String((e && e.stack) || e);
  }
  return out;
})()
"""


def _probe(server: _LiveServer, name: str, wait_ms: int):
    chrome_path = _find_chrome()
    if not chrome_path:
        pytest.skip("no Chrome/Chromium binary found on this host; cannot drive a headless browser")
    js = _PROBE_JS_TEMPLATE.replace("__NAME__", json.dumps(name)).replace("__WAIT_MS__", str(wait_ms))
    errors, value = asyncio.run(_drive(server.url, chrome_path, js, wait_s=6.0))
    print("\nvisual-model browser probe:", json.dumps(value, indent=2, sort_keys=True))
    print("console/exception messages:", errors)
    assert value is not None, f"the page probe returned nothing; console errors were: {errors}"
    assert not value.get("error"), f"probe reported an error: {value.get('error')}\nconsole: {errors}"
    return errors, value


def test_a_published_av_run_with_a_declared_model_draws_it_at_its_true_size(live_server):
    errors, value = _probe(live_server, live_server.names["model"], wait_ms=20000)
    assert value["spacecraftNames"] == ["leo"]
    assert value["declaredModel"] == MODEL_FIXTURE_URL, (
        f"the scenario av-run published to /api/cdm/run does not carry the declared model: {value!r}")
    assert value["modelLoaded"] is True, f"the viewer never finished loading the declared glTF: {value!r}"
    assert value["sourceLayerId"] == "entity-models"
    assert value["meshChildCount"] > 0
    assert value["groupWorldScale"] == pytest.approx([1.0, 1.0, 1.0], rel=1e-9)
    for axis, got, expected in zip("xyz", value["worldSizeMetres"], MODEL_FIXTURE_SIZE_M):
        assert math.isclose(got, expected, rel_tol=0.01), (
            f"the drawn model's world {axis} extent is {got} m, the fixture's own bound is {expected} m: {value!r}")
    assert errors == [], "expected zero console warnings/errors/page exceptions, got:\n" + "\n".join(errors)


def test_a_published_av_run_with_no_declared_model_draws_none(live_server):
    errors, value = _probe(live_server, live_server.names["plain"], wait_ms=3000)
    assert value["spacecraftNames"] == ["leo"]
    assert value["declaredModel"] is None
    assert value["modelLoaded"] is False
    assert value["hasModelEntity"] is False, f"a model entity exists for a run that declared no model: {value!r}"
    assert errors == [], "expected zero console warnings/errors/page exceptions, got:\n" + "\n".join(errors)

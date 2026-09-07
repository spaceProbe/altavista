"""M16.3 (question 5's first demo bridge): "a DRM authored in Python, propagated with GMAT
dynamics, shown on the custom globe and in ICRF, reproducible from its config hash."

This is the end-to-end test for the link between the Rust DRM executor and the altavista
viewer: build ``crates/av-run`` (module-scoped fixture, same "cargo build, fail loudly on a
build error" pattern as ``tests/test_dynamics_service_rs.py``'s ``server_bin`` fixture), run
it against the **golden maneuver DRM** (``drms/leo_1day_maneuver_vnb.{drm,sos}.yaml`` +
``drms/leo_1day_golden.system.yaml``, the same fixture ``crates/av-kernel/tests/
drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn`` pins against), POST the real wire
bytes it produces into a real ``altavista`` server (``altavista.server.create_app`` +
``fastapi.testclient.TestClient`` -- the exact route code a live ``uvicorn`` process runs,
without a real socket), and read the published scenario back both directly (as JSON) and
through the viewer's own headless harness (``web/js/verify_cdm_run.mjs``, which imports and
runs the *real*, shipped ``web/js/cdm_run.js`` under ``node`` -- see
``tests/test_viewer_jitter.py``'s module docstring for why this project never re-derives
viewer-side JS logic in Python).

What is checked, and why each check is a real test (this task's own "name the wrong
implementation it would fail against" requirement):

* The published scenario's ``meta.configHash`` equals the DRM's own declared ``hash:`` field
  in ``leo_1day_maneuver_vnb.drm.yaml`` -- not the ``SosConfiguration``'s hash, not empty, not
  a constant. Fails against a wrong ``POST /api/cdm/run`` handler that drops
  ``Provenance.config_hash`` on the floor, carries the wrong ``Provenance`` (e.g. the
  ``SosConfiguration``'s own hash, a different real-looking hex string), or hardcodes a
  placeholder.
* The trajectory's first/last epoch converts (through ``altavista.cdm.tai_ns_to_a1mjd``, the
  same conversion the whole viewer path uses) to exactly the DRM's own declared
  ``scenario.start_tai_ns``/``end_tai_ns``. Fails against an executor or wire-encoding bug
  that drops the first/last sample, or a server-side conversion bug that silently shifts
  epochs.
* The one ``EVENT_KIND_MANEUVER`` event's epoch matches
  ``goldens/leo_1day_maneuver_vnb.json``'s own ``burn_epoch_a1mjd`` -- independent
  confirmation (via the real wire bundle, not a hand re-derivation) that the maneuver event
  the executor actually emitted lines up with the golden's own recorded burn epoch.
* The published scenario carries exactly two ``EVENT_KIND_LIFECYCLE`` events (run start/end)
  and one ``EVENT_KIND_MANEUVER`` event, each surfacing as the correct ``type`` string
  through ``altavista.cdm.cdm_event_to_viewer_event`` -- fails against a converter that maps
  every kind to the same label, or a server handler that drops events.
* The headless harness (``web/js/verify_cdm_run.mjs``, running the real, shipped
  ``web/js/cdm_run.js``) reports the identical epochs, event kinds and config hash read
  straight from the published scenario JSON -- proving the *viewer's own code*, not just the
  Python conversion layer, surfaces them (the info line, the event-kind data feeding the
  timeline).
* A separate, explicitly synthetic bundle (not from a real DRM run -- the golden maneuver DRM
  has no ``FAULT_TARGET_KIND_DYNAMICS`` fault to draw a real one from) proves
  ``EVENT_KIND_FAULT`` reaches the exact same server/viewer path as the two kinds the real run
  produces, so all three of this task's required kinds (MANEUVER, FAULT, LIFECYCLE) are
  proven to reach the timeline -- two through a real GMAT-propagated run, one through the
  same code path fed a hand-built, protocol-honest wire bundle.

**M17.2 (question 121)**: the wire form itself changed -- ``av-run`` now emits a real
``altavista.v1.RunProducts`` message (``altavista.pb.altavista.v1.run_pb2.RunProducts``), not
the ad hoc ``AVRUN1`` length-prefixed concatenation this file used to hand-encode/decode
(``_encode_run_bundle``, ``altavista.cdm.parse_run_wire``/``RunBundle``, all deleted). Every
test above is otherwise unchanged in intent -- same golden maneuver DRM, same assertions --
only the bytes on the wire and the ``Content-Type`` header changed
(``application/x-protobuf``, matching ``POST /api/cdm/trajectory``). Added for this task:
JSON-transcoding acceptance, malformed-binary/malformed-JSON 400 refusals (mirroring
``tests/test_cdm_adapter.py``'s equivalent checks for ``/api/cdm/trajectory``), and a direct
decode of the real ``av-run`` output proving ``RunProducts.frames``/``.dropped_in_flight_
messages`` are genuinely populated, not just accepted-and-ignored fields.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path

import pytest
from fastapi.testclient import TestClient
from google.protobuf import json_format

import altavista as gv
from altavista import cdm as cdm_adapter
from altavista import profile as profile_loader
from altavista.model import Frame
from altavista.pb import core_pb2, trajectory_pb2
from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parents[1]
DRM_PATH = REPO_ROOT / "drms" / "leo_1day_maneuver_vnb.drm.yaml"
SOS_PATH = REPO_ROOT / "drms" / "leo_1day_maneuver_vnb.sos.yaml"
SYSTEM_PATH = REPO_ROOT / "drms" / "leo_1day_golden.system.yaml"

# The two-instance demo fixture (question 123, M17.3): a real DYNAMICS fault on `demo_flt`
# and a VNB maneuver on `demo_mvr`, so one real GMAT run emits FAULT, MANEUVER and LIFECYCLE
# together. It replaces the hand-built FAULT bundle this module used before M17.3 existed.
DEMO_DRM_PATH = REPO_ROOT / "drms" / "demo_two_instance.drm.yaml"
DEMO_SOS_PATH = REPO_ROOT / "drms" / "demo_two_instance.sos.yaml"
DEMO_SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance.system.yaml"
# M19.4 (question 131): the native range-condition controller's own SystemDefinition, a
# separate file (av-kernel's own schema loader parses exactly one SystemDefinition per
# document) -- required alongside DEMO_SYSTEM_PATH or av-run refuses with an unknown
# system_id ("demo_ctrl_sys") for drms/demo_two_instance.sos.yaml's own third instance.
DEMO_CTRL_SYSTEM_PATH = REPO_ROOT / "drms" / "demo_two_instance_ctrl.system.yaml"
# drms/demo_two_instance.drm.yaml: scenario.start_tai_ns + 1800 s.
DEMO_FAULT_TAI_NS = 1767227437000000000
GOLDEN_PATH = REPO_ROOT / "goldens" / "leo_1day_maneuver_vnb.json"
GOLDEN = json.loads(GOLDEN_PATH.read_text())

VERIFY_SCRIPT = REPO_ROOT / "web" / "js" / "verify_cdm_run.mjs"

RUSTUP_PATH_PREFIX = "/opt/homebrew/opt/rustup/bin"


def _cargo_env() -> dict:
    env = dict(os.environ)
    env["PATH"] = f"{RUSTUP_PATH_PREFIX}:{env.get('PATH', '')}"
    return env


def _declared_drm_hash() -> str:
    """The DRM fixture's own declared ``hash:`` field, read from the YAML text itself
    (rather than hardcoded here) so this test tracks the fixture rather than silently
    passing against a stale, hand-copied value if the fixture is ever regenerated."""
    m = re.search(r'^hash:\s*"([0-9a-f]{64})"', DRM_PATH.read_text(), re.MULTILINE)
    assert m, f"{DRM_PATH} has no top-level hash: \"<64 hex chars>\" field"
    return m.group(1)


DECLARED_DRM_HASH = _declared_drm_hash()


def _declared_scenario_window():
    """``(start_tai_ns, end_tai_ns)`` as declared in the DRM YAML -- parsed straight out of
    the fixture text (small, fixed grammar: `key: value` lines), not duplicated as literals,
    so a future change to the fixture's window cannot silently desync this test from it."""
    text = DRM_PATH.read_text()
    start = int(re.search(r"start_tai_ns:\s*(\d+)", text).group(1))
    end = int(re.search(r"end_tai_ns:\s*(\d+)", text).group(1))
    return start, end


# --------------------------------------------------------------------------- av-run fixtures
@pytest.fixture(scope="module")
def av_run_bin():
    """Builds ``crates/av-run``'s binary once for the module -- a build failure is a real
    failure of this task (same posture as ``tests/test_dynamics_service_rs.py``'s
    ``server_bin`` fixture), never silently skipped."""
    proc = subprocess.run(
        ["cargo", "build", "-p", "av-run", "--bin", "av-run"],
        cwd=str(REPO_ROOT), env=_cargo_env(), capture_output=True, text=True, timeout=900)
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p av-run failed:\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    binary = REPO_ROOT / "target" / "debug" / "av-run"
    assert binary.is_file(), f"expected {binary} after a successful cargo build"
    return binary


@pytest.fixture(scope="module")
def run_bundle_bytes(av_run_bin, tmp_path_factory) -> bytes:
    """Runs the real ``av-run`` binary against the golden maneuver DRM (real GMAT
    propagation, real hash verification, real event emission -- crates/av-kernel/src/drm's
    executor, unmodified) and returns the exact wire bytes it wrote to ``--out``."""
    out = tmp_path_factory.mktemp("av_run") / "bundle.bin"
    proc = subprocess.run(
        [str(av_run_bin), "--drm", str(DRM_PATH), "--sos", str(SOS_PATH), "--system", str(SYSTEM_PATH),
         "--run-id", "test-cdm-run-e2e", "--out", str(out)],
        cwd=str(REPO_ROOT), capture_output=True, text=True, timeout=180)
    if proc.returncode != 0:
        pytest.fail(f"av-run failed (rc={proc.returncode}):\n--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return out.read_bytes()


@pytest.fixture(scope="module")
def demo_run_bundle_bytes(av_run_bin, tmp_path_factory) -> bytes:
    """The real two-instance demo bundle: `av-run` over `drms/demo_two_instance.*`, whose
    executor emits a genuine `EVENT_KIND_FAULT` from a real `FAULT_TARGET_KIND_DYNAMICS`
    fault -- no hand-built message anywhere in the path."""
    out = tmp_path_factory.mktemp("av_run_demo") / "demo.bin"
    proc = subprocess.run(
        [str(av_run_bin), "--drm", str(DEMO_DRM_PATH), "--sos", str(DEMO_SOS_PATH),
         "--system", str(DEMO_SYSTEM_PATH), "--system", str(DEMO_CTRL_SYSTEM_PATH),
         "--run-id", "test-cdm-run-demo", "--out", str(out)],
        cwd=str(REPO_ROOT), capture_output=True, text=True, timeout=600)
    if proc.returncode != 0:
        pytest.fail(f"av-run failed on the demo fixture (rc={proc.returncode}):\n"
                    f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}")
    return out.read_bytes()


@pytest.fixture()
def client(tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)
    return TestClient(app)


@pytest.fixture()
def published_scenario(client: TestClient, run_bundle_bytes: bytes) -> dict:
    """POSTs the real ``av-run`` wire bytes (now genuine ``altavista.v1.RunProducts`` binary
    protobuf, question 121) to the real ``POST /api/cdm/run`` route and reads the resulting
    scenario back through ``GET /api/scenario/{name}`` -- the exact JSON shape a browser's
    ``loadScenario()`` (web/js/app.js) receives."""
    resp = client.post("/api/cdm/run", content=run_bundle_bytes, headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    return resp.json()


def _require_node():
    import shutil
    node = shutil.which("node")
    if node is None:
        pytest.skip("node is not installed; the headless harness (web/js/verify_cdm_run.mjs) needs it")
    return node


def _run_headless_harness(scenario: dict, duration_text: str, tmp_path: Path, *,
                          focus_name: str = None, epoch: float = None) -> dict:
    """Runs ``web/js/verify_cdm_run.mjs`` (the real, shipped harness -- see that file's
    own module docstring) against ``scenario``. When ``focus_name``/``epoch`` are both
    given, the harness also interpolates that spacecraft's position/velocity at that
    A1MJD epoch through the real, shipped ``web/js/interp.js`` ``TrajectoryInterp``
    (M17.1, question 122) -- ``repr(epoch)`` is passed through so the child process gets
    the same full ``float`` precision this process holds, not a truncated string.
    """
    node = _require_node()
    scenario_path = tmp_path / "scenario.json"
    scenario_path.write_text(json.dumps(scenario))
    args = [node, str(VERIFY_SCRIPT), str(scenario_path), duration_text]
    if focus_name is not None and epoch is not None:
        args += [focus_name, repr(epoch)]
    proc = subprocess.run(args, cwd=str(VERIFY_SCRIPT.parent), capture_output=True, text=True, timeout=30)
    assert proc.returncode == 0, f"node verify_cdm_run.mjs exited {proc.returncode}\nstdout: {proc.stdout}\nstderr: {proc.stderr}"
    return json.loads(proc.stdout)


# --------------------------------------------------------------------------- the real run
def test_config_hash_matches_the_drms_own_declared_hash(published_scenario):
    """The viewer's info line must show *this run's own* config hash -- the DRM's declared
    hash, verified by the executor before it ran anything (crates/av-kernel/src/drm/hash.rs).
    Would fail if the server dropped Provenance.config_hash, substituted the
    SosConfiguration's own hash, or hardcoded/emptied the field.
    """
    assert published_scenario["meta"]["configHash"] == DECLARED_DRM_HASH


def test_trajectory_epoch_span_matches_the_declared_scenario_window(published_scenario):
    """The published trajectory's first/last epoch (t0/t1, A1MJD) must equal the DRM's own
    declared start_tai_ns/end_tai_ns, converted through the same altavista.cdm epoch
    conversion the whole viewer path uses. Fails against a dropped first/last sample, an
    off-by-one-sample bug in the wire encoder/decoder, or a wrong epoch conversion.
    """
    start_tai_ns, end_tai_ns = _declared_scenario_window()
    assert published_scenario["t0"] == pytest.approx(cdm_adapter.tai_ns_to_a1mjd(start_tai_ns), abs=1e-9)
    assert published_scenario["t1"] == pytest.approx(cdm_adapter.tai_ns_to_a1mjd(end_tai_ns), abs=1e-9)


def test_maneuver_event_epoch_matches_the_golden_burn_epoch(published_scenario):
    """The one MANEUVER event's epoch must match goldens/leo_1day_maneuver_vnb.json's own
    recorded burn_epoch_a1mjd -- independent confirmation, through the real wire bundle,
    that the executor's own applied-maneuver epoch reaches the viewer unchanged. Fails
    against an executor/adapter bug that reports the *declared* (pre-reconciliation) burn
    epoch instead of the epoch actually applied, or drops/duplicates the maneuver event.
    """
    maneuvers = [e for e in published_scenario["events"] if e["type"] == "maneuver"]
    assert len(maneuvers) == 1, f"expected exactly one maneuver event, got {maneuvers}"
    assert maneuvers[0]["t"] == pytest.approx(GOLDEN["burn_epoch_a1mjd"], abs=1e-9)


def test_lifecycle_and_maneuver_event_kinds_reach_the_published_timeline(published_scenario):
    """The golden maneuver DRM's real run produces exactly two LIFECYCLE events (run
    start/end) and one MANEUVER event (crates/av-kernel/src/drm/events.rs's own documented
    emission list) -- this checks all three actually reach ScenarioData.events with the
    right `type` label. Fails against a converter that maps every kind to "marker" (the
    pre-M16.3 default for an unrecognized kind), or a handler that drops events entirely.
    """
    kinds = sorted(e["type"] for e in published_scenario["events"])
    assert kinds == sorted(["lifecycle", "lifecycle", "maneuver"])


# --------------------------------------------------------------------------- the headless harness
def test_headless_harness_reads_back_the_same_hash_and_epochs(published_scenario, tmp_path):
    """The viewer's own JS (web/js/cdm_run.js, run for real under node -- not reimplemented
    in Python) must report the identical config hash and event epochs the Python-side
    assertions above already checked directly on the JSON. Fails if web/js/cdm_run.js's
    formatScenarioInfo silently drops the hash from the info line, or if eventKinds/
    eventEpochs disagree with the raw data -- i.e. it proves the *viewer's* read of the data,
    not just the server's production of it.
    """
    result = _run_headless_harness(published_scenario, "2.0 h", tmp_path)
    assert result["configHash"] == DECLARED_DRM_HASH
    assert DECLARED_DRM_HASH in result["info"], f"config hash missing from the info line: {result['info']!r}"
    assert sorted(result["eventKinds"]) == sorted(["lifecycle", "lifecycle", "maneuver"])
    expected_epochs = sorted(e["t"] for e in published_scenario["events"])
    assert sorted(result["eventEpochs"]) == pytest.approx(expected_epochs, abs=1e-9)


def test_scenario_with_no_config_hash_is_unaffected():
    """M16.3 is additive: a scenario with no `meta.configHash` (every pre-existing publishing
    path) must format its info line exactly as before -- proves formatScenarioInfo's hash
    suffix is opt-in, not a shape change forced on every scenario. Fails against an
    implementation that always appends a "config" suffix (e.g. an empty one) even when no
    hash was ever published.
    """
    node = _require_node()
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        result = _run_headless_harness(
            {"frame": {"name": "EarthMJ2000Eq"}, "spacecraft": [{"name": "a"}, {"name": "b"}], "events": []},
            "1.0 h", Path(d))
    assert result["info"] == "EarthMJ2000Eq · 1.0 h · 2 spacecraft"
    assert result["configHash"] is None


# --------------------------------------------------------------------------- FAULT kind (synthetic)
def _build_run_products(trajectories, events, provenance: core_pb2.Provenance, *, run_id: str = "",
                        dropped_in_flight_messages: int = 0, frames=()) -> run_pb2.RunProducts:
    """A `run_pb2.RunProducts` built directly from the real generated bindings (never a
    hand-rolled byte layout -- question 121 deleted the old `AVRUN1` framing this file used to
    hand-encode/decode) -- `trajectories` is a list of `(instance_id, Trajectory)` pairs, since
    `RunProducts.trajectories` is a proto map keyed by `SystemInstance.id`.
    """
    rp = run_pb2.RunProducts(run_id=run_id, events=list(events), provenance=provenance,
                             dropped_in_flight_messages=dropped_in_flight_messages, frames=list(frames))
    for instance_id, traj in trajectories:
        rp.trajectories[instance_id].CopyFrom(traj)
    return rp


def test_fault_event_kind_reaches_the_same_server_and_viewer_path(client: TestClient, tmp_path, demo_run_bundle_bytes: bytes):
    """FAULT, MANEUVER and LIFECYCLE all reach the viewer from ONE real GMAT run.

    Before M17.3 every DRM fixture declared a single instance and none declared a fault, so
    this test fed a hand-built `RunProducts` to prove the third event kind. `drms/
    demo_two_instance.*` (question 123) now carries a real `FAULT_TARGET_KIND_DYNAMICS` fault
    on `demo_flt` and a VNB maneuver on `demo_mvr`, so the synthetic bundle is gone: these
    bytes come from the real executor via `av-run`.

    Fails against a converter or server handler that special-cases only MANEUVER/LIFECYCLE and
    drops or mislabels FAULT, and -- unlike the synthetic version -- also against an executor
    that stops emitting a real fault's event, or emits it at the wrong epoch or instance.
    """
    resp = client.post("/api/cdm/run", content=demo_run_bundle_bytes,
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    scenario = client.get(f"/api/scenario/{name}").json()

    faults = [e for e in scenario["events"] if e["type"] == "fault"]
    assert len(faults) == 1, f"expected exactly one fault event, got {scenario['events']}"
    assert faults[0]["t"] == pytest.approx(cdm_adapter.tai_ns_to_a1mjd(DEMO_FAULT_TAI_NS), abs=1e-9)

    kinds = {e["type"] for e in scenario["events"]}
    assert {"fault", "maneuver"} <= kinds, f"one real run must carry both kinds, got {kinds}"

    result = _run_headless_harness(scenario, "0.0 h", tmp_path)
    assert "fault" in result["eventKinds"]


# --------------------------------------------------------------------------- binary vs. JSON transcoding (question 121)
def test_publish_cdm_run_accepts_json_transcoding_and_reconstructs_the_same_run(client: TestClient):
    """`/api/cdm/run` must accept JSON, exactly like `/api/cdm/trajectory` does (this task's
    own requirement). Fails against a handler that only ever tries `ParseFromString` regardless
    of `Content-Type` (a JSON body would fail to parse as binary protobuf and this would 400),
    or one that reconstructs a *different* run than the JSON actually describes.
    """
    traj = trajectory_pb2.Trajectory(id="json-traj", entity_id="json-veh", frame_id="EarthMJ2000Eq")
    ev = trajectory_pb2.Event(id="ev1", entity_id="json-veh", tai_ns=5_000_000_000, kind=trajectory_pb2.EVENT_KIND_MARKER, name="ev1")
    provenance = core_pb2.Provenance(config_hash="json-hash", run_id="test-json-run")
    run_products = _build_run_products([("json-veh", traj)], [ev], provenance, run_id="test-json-run")

    payload = json_format.MessageToJson(run_products)
    resp = client.post("/api/cdm/run", content=payload, headers={"content-type": "application/json"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    scenario = client.get(f"/api/scenario/{name}").json()

    assert scenario["meta"]["configHash"] == "json-hash"
    assert scenario["meta"]["runId"] == "test-json-run"
    assert scenario["spacecraft"][0]["name"] == "json-veh"
    assert scenario["events"][0]["type"] == "marker"
    assert scenario["events"][0]["t"] == pytest.approx(cdm_adapter.tai_ns_to_a1mjd(5_000_000_000), abs=1e-9)


def test_publish_cdm_run_rejects_malformed_binary_body(client: TestClient):
    """Mirrors `tests/test_cdm_adapter.py::test_endpoint_rejects_malformed_binary_body` for
    `/api/cdm/run`. Fails against a handler that lets a `DecodeError` propagate as an
    unhandled 500 instead of a typed 400.
    """
    resp = client.post("/api/cdm/run", content=b"\xff\xff\xff not a protobuf message \x00\x01",
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400
    assert "malformed" in resp.json()["detail"].lower()


def test_publish_cdm_run_rejects_malformed_json_body(client: TestClient):
    """Mirrors `tests/test_cdm_adapter.py::test_endpoint_rejects_malformed_json_body` for
    `/api/cdm/run`."""
    resp = client.post("/api/cdm/run", content="{not valid json",
                       headers={"content-type": "application/json"})
    assert resp.status_code == 400
    assert "malformed" in resp.json()["detail"].lower()


def test_run_bundle_bytes_are_a_real_run_products_message_with_frames_and_scores(run_bundle_bytes: bytes):
    """Decodes the real `av-run` output (the golden VNB maneuver DRM) directly with the
    generated `run_pb2.RunProducts` bindings, independent of the `POST /api/cdm/run` handler --
    proves `av_kernel::drm::executor::RunProducts::to_proto` genuinely populates `frames` (not
    an empty list) for a real run, cross-language: Rust wrote these bytes, Python decodes them.
    Fails against an executor that never calls `collect_frames` (frames would be empty here) or
    one that derives the wrong body/axes for the golden's own `EarthMJ2000Eq`
    `spacecraft.CoordinateSystem`.
    """
    run_products = run_pb2.RunProducts()
    run_products.ParseFromString(run_bundle_bytes)
    assert not run_bundle_bytes.startswith(b"AVRUN1"), "the AVRUN1 magic must be gone"
    assert run_products.run_id == "test-cdm-run-e2e"
    assert run_products.provenance.config_hash == DECLARED_DRM_HASH
    assert "leo_mvr" in run_products.trajectories
    assert run_products.dropped_in_flight_messages == 0, "the golden maneuver DRM declares no ports/connections"

    frame_ids = {f.id: f for f in run_products.frames}
    assert "EarthMJ2000Eq" in frame_ids, run_products.frames
    earth = frame_ids["EarthMJ2000Eq"]
    assert earth.body == "Earth"
    assert earth.axes == core_pb2.AXES_KIND_MJ2000_EQ


# --------------------------------------------------------------------------- M17.1: frames on the
# CDM ingest path (question 122)
#
# The test directly above proves `RunProducts.frames` itself is populated for the golden
# maneuver DRM -- and, decoded there, it names exactly one frame, `EarthMJ2000Eq`
# (`spacecraft.CoordinateSystem` in `drms/leo_1day_golden.system.yaml`, verified again by
# hand against a real `av-run` binary while building this task: no `AXES_KIND_ICRF` entry
# exists anywhere in this fixture's own bundle). So "the viewer offers ICRF" cannot be
# proven end-to-end against *this* fixture without either a `drms/**`/`crates/**` change
# (both off limits to this task) or inventing a `FrameDefinition` the bundle never
# declared (which this task's own honesty rules forbid: "never synthesizing a
# plausible-looking FrameDefinition"). The tests below instead split the claim into its
# two honest parts: the real golden maneuver run proves the *general* frame-threading
# mechanism (any declared frame reaches the viewer, validated, in the Python-scenario
# shape); a second, synthetic-but-protocol-honest bundle -- built the same way
# `test_fault_event_kind_reaches_the_same_server_and_viewer_path` above already proves a
# third event kind through this same endpoint when the golden DRM has none to draw one
# from -- proves the ICRF-specific path with real numbers (a real GMAT-propagated orbit,
# recorded directly in ICRF by a Python `altavista.Scenario`, round-tripped through the
# same CDM converters a real producer would use).
def test_run_frames_populate_the_scene_frame_list_through_frame_registry(published_scenario):
    """The published scenario's `frames` list must be the real, ``altavista.frames.
    FrameRegistry``-validated ``EarthMJ2000Eq`` `RunProducts.frames` declares -- not
    empty (the pre-M17.1 state question 122 describes: "the viewer synthesizes a root
    frame"), and not a verbatim copy of the wire `FrameDefinition` either.

    `gmatName` is the tell: `av-run`'s own wire bytes never set it (verified directly,
    `earth.gmat_name == ""` on the raw `RunProducts.frames` entry decoded in the test
    above -- `av_kernel::drm::executor::registry_default_frame`'s own `..Default::
    default()` leaves it unset, since only a real frame *service* fills it, per
    core.proto's own doc comment). A non-empty `gmatName` here can only come from
    `FrameRegistry.register()` actually running and building a real GMAT
    `CoordinateSystem` for it. Fails against: (a) an implementation that leaves
    `ScenarioData.frames` empty (RunProducts.frames accepted-and-ignored, M17.2's own
    prior state); (b) one that copies `run_products.frames` into the scene JSON
    unvalidated (protobuf-JSON-transcoded verbatim) -- `gmatName` would be empty and
    `parentFrameId` would be absent *for the wrong reason* (never computed, rather than
    correctly omitted because the root rule computed ``""``).

    Also checks `ScenarioData.frame` (the *entities* frame) itself now carries its real
    `origin`/`axes` (`{"name": "EarthMJ2000Eq", "origin": "Earth", "axes": "MJ2000Eq"}`)
    rather than the bare `{"name": ..., "origin": "Earth", "axes": "MJ2000Eq"}` *default*
    every frame this endpoint could not resolve used to silently take before this task --
    this fixture's own frame happens to coincide with that default, so
    `test_publish_cdm_run_accepts_json_transcoding_and_reconstructs_the_same_run`'s
    `frame_id="EarthMJ2000Eq"` case would not have caught a regression back to the old
    hardcoded fallback; this test's own frame-list assertions above are what actually
    would.
    """
    frames = published_scenario["frames"]
    # M18.1 (question 124/10): the mandatory frames for the central body -- ICRF,
    # MJ2000 equatorial and body-fixed -- are always on the wire whatever coordinate
    # system the instance propagated in, so a consumer may view the trajectory in any
    # frame the producer can realize. Before M18.1 this was the propagation frame alone.
    assert [f["id"] for f in frames] == ["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"], frames
    fd = next(f for f in frames if f["id"] == "EarthMJ2000Eq")
    assert fd["body"] == "Earth"
    assert fd["axes"] == "AXES_KIND_MJ2000_EQ"
    assert fd["gmatName"], f"gmatName must be set by a real FrameRegistry.register() call, got {fd!r}"
    assert "parentFrameId" not in fd, "a body-axes frame is the registry root; parentFrameId must be omitted (question 76)"

    assert published_scenario["frame"] == {"name": "EarthMJ2000Eq", "origin": "Earth", "axes": "MJ2000Eq"}


def test_run_frame_list_matches_the_equivalent_python_scenario_shape(published_scenario):
    """The CDM-ingested run's `frames` list must be the *same wire shape*
    (`altavista.frames.FrameRegistry`-validated protobuf-JSON dicts: `id`/`body`/`axes`/
    `gmatName`, `description` when set, `parentFrameId` when non-root) a Python-built
    `altavista.Scenario` with the same frame already produces via `Scenario._build_frames`
    -- the shape `web/js/scene.js`'s `_buildFrameGraph` already knows how to consume.
    `id`/`body`/`axes`/`gmatName` must match exactly (only `description`'s free text may
    differ -- one path's text says "altavista frame ...", the other "registry default for
    GMAT CoordinateSystem ..." -- different words for the same registered
    CoordinateSystem, not a shape difference).

    Fails against an implementation that builds `ScenarioData.frames` a *different* way
    for a CDM-ingested run than a Python scenario -- e.g. hand-building a dict with
    snake_case keys, omitting `gmatName`, or skipping `FrameRegistry` entirely and
    reusing the wire `FrameDefinition`'s own (always-empty, for this producer) `gmat_name`
    -- since `web/js/scene.js`/`web/js/frames.js` (this task's read-only reference) parse
    exactly this dict shape and nothing else.
    """
    python_scenario = gv.Scenario("m17-1-shape-comparison", frame="EarthMJ2000Eq")
    python_data = python_scenario.build()  # no spacecraft needed: _build_frames only needs self.frame.origin

    python_frames = {fd["id"]: fd for fd in python_data.frames}
    cdm_frames = {fd["id"]: fd for fd in published_scenario["frames"]}
    assert "EarthMJ2000Eq" in python_frames and "EarthMJ2000Eq" in cdm_frames

    py_fd, cdm_fd = python_frames["EarthMJ2000Eq"], cdm_frames["EarthMJ2000Eq"]
    assert py_fd.keys() == cdm_fd.keys(), (py_fd, cdm_fd)
    for key in py_fd.keys() - {"description"}:
        assert py_fd[key] == cdm_fd[key], (key, py_fd, cdm_fd)


def test_headless_harness_reports_the_real_frame_graph_for_the_ingested_run(published_scenario, tmp_path):
    """The viewer's own frame-graph-consuming code (`web/js/frames.js`'s real
    `FrameGraph`/`orderFrameDefsByParent`, driven by `web/js/verify_cdm_run.mjs`'s
    `frameGraphFacts` exactly the way `web/js/scene.js`'s `_buildFrameGraph` does --
    see that harness's own doc comment) must build a real node for the entities frame
    from the published scenario's `frames` list, not the synthesized-root fallback
    `scene.js` falls back to (with a `console.warn`) when a scenario's own frame is
    missing from `frames` entirely -- exactly the pre-M17.1 state question 122
    describes. Fails against an implementation that populates `ScenarioData.frames`
    with entries under the *wrong* ids (e.g. always `"EarthMJ2000Eq"` regardless of the
    trajectory's real `frame_id`) -- `entitiesFrameInGraph` would still read `True` only
    by coincidence for this fixture; the explicit `frameIds`/`frameAxesById` check below
    would catch a wrong-axes regression that a bare boolean could not.
    """
    result = _run_headless_harness(published_scenario, "2.0 h", tmp_path)
    assert result["frameGraph"]["entitiesFrameInGraph"] is True
    assert result["frameGraph"]["frameIds"] == ["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"]
    assert result["frameGraph"]["frameAxesById"] == {
        "EarthBodyFixed": "AXES_KIND_BODY_FIXED",
        "EarthICRF": "AXES_KIND_ICRF",
        "EarthMJ2000Eq": "AXES_KIND_MJ2000_EQ",
    }


def test_synthetic_icrf_run_shows_an_icrf_option_and_places_the_trajectory_correctly(client: TestClient, tmp_path):
    """The ICRF-specific half of question 122 / question 5's demo ("shown ... in ICRF"),
    proven with real numbers since the golden maneuver DRM itself never declares an ICRF
    frame (see this section's own header comment): a real GMAT-propagated LEO orbit is
    recorded directly in ICRF by a Python `altavista.Scenario`, converted to a CDM
    `Trajectory` + `FrameDefinition` with the *same*, unmodified `altavista.cdm.
    trajectory_to_cdm`/`frame_definition_for` a real producer would use, packaged as a
    protocol-honest `RunProducts` (mirroring `test_fault_event_kind_reaches_the_same_
    server_and_viewer_path`'s own synthetic-bundle pattern above), and POSTed to the
    real `POST /api/cdm/run` route.

    Checks, in order:
    1. **The ICRF option is actually there**, not a coincidence of an empty list: exactly
       one `AXES_KIND_ICRF` entry in `frames`, `id == "EarthICRF"`, and the *entities*
       frame itself (`scenario["frame"]`) is that same ICRF frame -- fails against an
       implementation that special-cases `AXES_KIND_MJ2000_EQ` only (works for the golden
       maneuver fixture, silently drops any other axes kind) or one that always falls
       back to the altavista Earth/MJ2000Eq default regardless of what was actually
       declared.
    2. **The real, shipped frame graph (`web/js/frames.js`, driven the same way
       `web/js/scene.js` would) has a node for that ICRF frame** -- fails against a
       converter that emits a `frames` entry the viewer's own `FrameGraph`/
       `orderFrameDefsByParent` cannot actually consume (e.g. a malformed
       `parentFrameId` chain, or key names that do not match the documented wire
       shape) -- a check the JSON-shape assertions in (1) alone would not catch, since
       they never exercise the JS consumer at all.
    3. **The trajectory is placed correctly *in* that frame**: the real cubic-Hermite
       `TrajectoryInterp` (`web/js/interp.js`), run by the same headless harness,
       interpolates the CDM-ingested spacecraft's position/velocity at an *off-sample*
       epoch (the midpoint between two recorded samples, so this is not merely checking
       that raw samples were copied through) to within 1e-6 km / 1e-6 km/s of
       interpolating the *original* Python-scenario trajectory the CDM bundle was built
       from, through the identical `TrajectoryInterp` code, on a hand-built minimal
       scenario carrying that same raw data. Both sides use the exact same interpolation
       function on (up to CDM round-trip rounding) the same sample grid, so this is not
       measuring interpolation error at all -- it isolates the CDM round-trip's own
       floating-point/unit-conversion precision (km <-> m, A1MJD <-> TAI ns, both exact
       or near-exact in float64). A direct measurement while writing this test showed
       agreement to about 1e-12 km; 1e-6 km (1 mm) is deliberately generous headroom
       above that, not a value tuned to just barely pass. Fails against a frame-id
       mismatch that silently interpolates the *wrong* spacecraft or an off-by-one
       sample-ordering bug in the CDM round trip (trajectory_to_cdm sorts by epoch
       explicitly; a regression there would show up as a large, not a rounding-level,
       discrepancy here).
    """
    python_scenario = gv.Scenario("m17-1-icrf-synthetic", frame="EarthICRF")
    sat = python_scenario.spacecraft(
        "Sat", epoch="01 Jan 2026 00:00:00.000",
        keplerian=dict(SMA=6878.0, ECC=0.001, INC=51.6, RAAN=30.0, AOP=0.0, TA=0.0))
    python_scenario.propagate(sat, hours=1, step=60)
    python_data = python_scenario.build()
    py_traj = python_data.spacecraft[0]
    assert len(py_traj.t) >= 12, "need enough samples for an interior, off-sample query epoch"

    cdm_traj = cdm_adapter.trajectory_to_cdm(py_traj, entity_id="Sat", frame_id="EarthICRF")
    frame_def = cdm_adapter.frame_definition_for(python_data.frame, frame_id="EarthICRF")
    provenance = core_pb2.Provenance(config_hash="synthetic-icrf-hash", run_id="test-icrf-run")
    run_products = _build_run_products([("Sat", cdm_traj)], [], provenance, run_id="test-icrf-run", frames=[frame_def])

    resp = client.post("/api/cdm/run", content=run_products.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    scenario = client.get(f"/api/scenario/{name}").json()

    # (1) the ICRF option is actually there.
    icrf_frames = [f for f in scenario["frames"] if f["axes"] == "AXES_KIND_ICRF"]
    assert len(icrf_frames) == 1, scenario["frames"]
    assert icrf_frames[0]["id"] == "EarthICRF"
    assert scenario["frame"] == {"name": "EarthICRF", "origin": "Earth", "axes": "ICRF"}

    # (2)+(3): the real frame graph has the node, and the real interpolator places the
    # trajectory correctly in it. Query epoch: the midpoint of samples 10/11, strictly
    # off the recorded grid.
    t_query = 0.5 * (py_traj.t[10] + py_traj.t[11])
    ingested = _run_headless_harness(scenario, "1.0 h", tmp_path, focus_name="Sat", epoch=t_query)
    assert ingested["frameGraph"]["entitiesFrameInGraph"] is True
    assert ingested["frameGraph"]["frameAxesById"].get("EarthICRF") == "AXES_KIND_ICRF"

    python_minimal_scenario = {
        "frame": {"name": "EarthICRF"},
        "spacecraft": [{
            "name": "Sat", "t": py_traj.t,
            "pos": [c for p in py_traj.pos for c in p],
            "vel": [c for v in py_traj.vel for c in v],
        }],
        "events": [],
    }
    reference = _run_headless_harness(python_minimal_scenario, "1.0 h", tmp_path, focus_name="Sat", epoch=t_query)

    assert ingested["interpolated"]["pos"] == pytest.approx(reference["interpolated"]["pos"], abs=1e-6)
    assert ingested["interpolated"]["vel"] == pytest.approx(reference["interpolated"]["vel"], abs=1e-6)


def test_publish_cdm_trajectory_accepts_frames_alongside_in_a_json_envelope(client: TestClient):
    """M17.1 (question 122), item 2: `POST /api/cdm/trajectory` threads frames supplied
    *alongside* the `Trajectory` through the same `altavista.cdm.frames_to_viewer_json`
    `POST /api/cdm/run` uses. `altavista.v1.Trajectory` has no `frames` field and the
    binary-protobuf body is exactly one `Trajectory` message's bytes with no room for a
    second one without a new declared wrapper message (`proto/**` read-only, no proto
    change authorized -- see `altavista/server.py`'s own doc comment on this endpoint for
    why the binary path is therefore unchanged and cannot carry frames). The JSON path
    additively accepts ``{"trajectory": <Trajectory JSON>, "frames": [<FrameDefinition
    JSON>, ...]}`` -- a plain JSON convention, not a new `.proto` message -- distinguished
    from a bare `Trajectory` body (every pre-M17.1 caller, still accepted unchanged; see
    `tests/test_cdm_adapter.py`'s existing JSON-transcoding tests, still passing
    unmodified) by the presence of a top-level `"trajectory"` key, which is not one of
    `Trajectory`'s own camelCase JSON field names.

    Fails against an implementation that ignores the `frames` key entirely (`frames`
    would stay `[]` and `frame.axes` would stay at the altavista Earth/MJ2000Eq default
    rather than the declared `ICRF`), or one that breaks the bare-`Trajectory`-body case
    by always expecting the envelope shape.
    """
    cdm_traj = trajectory_pb2.Trajectory(id="env-traj", entity_id="env-veh", frame_id="EarthICRF")
    cdm_traj.samples.add(tai_ns=0, mean=[7000000.0, 0.0, 0.0, 0.0, 7500.0, 0.0])
    frame_def = core_pb2.FrameDefinition(id="EarthICRF", body="Earth", axes=core_pb2.AXES_KIND_ICRF)

    envelope = {
        "trajectory": json.loads(json_format.MessageToJson(cdm_traj)),
        "frames": [json.loads(json_format.MessageToJson(frame_def))],
    }
    resp = client.post("/api/cdm/trajectory", content=json.dumps(envelope),
                       headers={"content-type": "application/json"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    scenario = client.get(f"/api/scenario/{name}").json()

    assert scenario["frame"] == {"name": "EarthICRF", "origin": "Earth", "axes": "ICRF"}
    icrf_frames = [f for f in scenario["frames"] if f["axes"] == "AXES_KIND_ICRF"]
    assert len(icrf_frames) == 1 and icrf_frames[0]["id"] == "EarthICRF"


# --------------------------------------------------------------------------- cdm_event_to_viewer_event
@pytest.mark.parametrize("kind,expected_type", [
    (trajectory_pb2.EVENT_KIND_MANEUVER, "maneuver"),
    (trajectory_pb2.EVENT_KIND_FAULT, "fault"),
    (trajectory_pb2.EVENT_KIND_LIFECYCLE, "lifecycle"),
    (trajectory_pb2.EVENT_KIND_MARKER, "marker"),
])
def test_cdm_event_to_viewer_event_maps_every_required_kind(kind, expected_type):
    """Direct unit coverage of the one place EventKind -> viewer `type` mapping happens.
    Fails against an implementation that maps every kind to the same label (e.g. always
    "marker", the pre-M16.3 default for the one kind event_to_cdm's own inverse never had to
    handle before), or that swaps two labels.
    """
    ev = trajectory_pb2.Event(id="e1", name="e1", tai_ns=0, kind=kind, detail="d")
    viewer_ev = cdm_adapter.cdm_event_to_viewer_event(ev)
    assert viewer_ev.type == expected_type


def test_cdm_event_to_viewer_event_labels_an_unmapped_kind_honestly():
    """A kind this module has no explicit mapping for (e.g. CONTACT_START) must not be
    silently coerced to "marker" -- it gets the enum's own name instead, so nothing is
    misreported as a kind it is not. Fails against an implementation with a blanket
    `.get(kind, "marker")` fallback.
    """
    ev = trajectory_pb2.Event(id="e1", name="e1", tai_ns=0, kind=trajectory_pb2.EVENT_KIND_CONTACT_START)
    viewer_ev = cdm_adapter.cdm_event_to_viewer_event(ev)
    assert viewer_ev.type == "contact_start"


# ==================================================================================
# M18.2 (docs/open-questions.md question 125, decided by the lead): "the CDM run path
# emits no bodies" -- POST /api/cdm/run's ScenarioData.bodies was always [], so the
# viewer's globe (which needs an `Earth` body entry, web/js/scene.js's `enableGlobe`)
# was unavailable for every `av-run` run. The decided fix: derive the scene's body list
# from the origin bodies `RunProducts.frames` actually names (never invented), sample
# them with the *existing* `altavista.bodies.BodySampler`, and record in `meta` that the
# list came from the bundle's own frames.
#
# `tests/fixtures/demo_two_instance.runproducts.bin` is a real `RunProducts` message a
# real `av-run` binary produced from `drms/demo_two_instance.*` (M19.4, question 131: 3
# trajectories -- demo_flt/demo_mvr/demo_ctrl -- 10 events, 3 frames -- EarthBodyFixed/
# EarthICRF/EarthMJ2000Eq, question 10's own mandatory-frame set -- origin body `Earth`
# throughout) -- frozen so these
# tests do not have to invoke `cargo`/`av-run` themselves (this task's own environment
# rule: no `cargo`, no touching `crates/**`, a concurrent worker owns that rebuild).
# `demo_run_bundle_bytes` above (built live via `av_run_bin`) is the equivalent,
# non-frozen fixture the rest of this file already uses for the same DRM.
FROZEN_DEMO_RUN_BUNDLE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance.runproducts.bin"


@pytest.fixture(scope="module")
def frozen_demo_bundle_bytes() -> bytes:
    return FROZEN_DEMO_RUN_BUNDLE_PATH.read_bytes()


@pytest.fixture()
def published_frozen_demo_scenario(client: TestClient, frozen_demo_bundle_bytes: bytes) -> dict:
    resp = client.post("/api/cdm/run", content=frozen_demo_bundle_bytes,
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    return resp.json()


def test_bodies_are_derived_from_run_products_frames_and_nothing_else(published_frozen_demo_scenario):
    """The published scenario's `bodies` list must contain exactly the origin bodies
    the frozen bundle's own `RunProducts.frames` names -- here, exactly one entry,
    `Earth` (the fixture's only frame, `EarthMJ2000Eq`, origin body `Earth`) -- and the
    honest provenance note in `meta` must say so.

    Fails against: (a) the pre-M18.2 implementation, which never populates `bodies` at
    all (`bodies` would be `[]`); (b) an implementation that reuses
    `altavista.scenario.Scenario.default_bodies()`'s Python-scenario convenience list
    (which always adds a Sun/Luna pair) instead of driving strictly off the wire --
    `names` would then be `{"Earth", "Sun", "Luna"}`, not `{"Earth"}`; (c) an
    implementation that never sets `meta["bodiesSource"]`.
    """
    names = {b["name"] for b in published_frozen_demo_scenario["bodies"]}
    assert names == {"Earth"}, published_frozen_demo_scenario["bodies"]
    assert published_frozen_demo_scenario["meta"]["bodiesSource"] == "RunProducts.frames"


def test_earth_body_is_central_and_sampled_over_the_runs_own_span(published_frozen_demo_scenario):
    """The derived `Earth` body must be marked `central` (it is the entities frame's own
    origin) and sampled across the *run's own* epoch span (`scenario["t0"]`/`["t1"]`,
    the same span `ScenarioData.span()` computes from the ingested trajectories) -- not
    a single hardcoded epoch and not an empty sample stream despite real trajectory data
    being present.

    Fails against an implementation that samples a fixed/degenerate time window
    (e.g. always `[t0]` regardless of the run's real span, or samples ignoring `t1`
    entirely), or one that leaves `t`/`pos`/`quat` empty for a run that does have
    spacecraft data (`BodySampler.track()` only returns an empty track when handed an
    empty `times` list, which should not happen here).
    """
    bodies = {b["name"]: b for b in published_frozen_demo_scenario["bodies"]}
    earth = bodies["Earth"]
    assert earth["central"] is True
    assert len(earth["t"]) >= 2, earth["t"]
    assert earth["t"][0] == pytest.approx(published_frozen_demo_scenario["t0"], abs=1e-9)
    assert earth["t"][-1] == pytest.approx(published_frozen_demo_scenario["t1"], abs=1e-9)
    assert len(earth["pos"]) == len(earth["t"]) * 3
    assert len(earth["quat"]) == len(earth["t"]) * 4


def test_headless_harness_shows_the_globe_would_enable_for_the_ingested_run(published_frozen_demo_scenario, tmp_path):
    """The actual point of M18.2: `Viewer.enableGlobe('Earth')` (web/js/scene.js) must
    succeed for a scenario ingested through `POST /api/cdm/run`. `enableGlobe`'s entire
    guard is `this.bodies.get(bodyName)` truthy, and `this.bodies` is populated 1:1 from
    `sc.bodies` by `setScenario()` -- see `web/js/verify_cdm_run.mjs`'s additive
    `globeFacts()` (extending the existing headless harness this repo already uses for
    CDM-run verification, rather than a new one) for exactly why checking `sc.bodies`
    here is checking the same fact `enableGlobe` itself checks, not an approximation of
    it, and why a full `Viewer` cannot be constructed under plain `node` at all
    (`web/js/scene_jitter_harness.mjs`'s module docstring: `Viewer`'s constructor needs
    a real `THREE.WebGLRenderer`/canvas).

    Fails against the pre-M18.2 implementation (`bodies == []` always) -- and against
    the earlier draft of this task's own server.py wiring, before `ScenarioData.bodies`
    was actually populated from `bodies_from_frames`'s return value.
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "0.0 h", tmp_path)
    assert result["globe"]["wouldEnableGlobe"] is True
    assert "Earth" in result["globe"]["bodyNames"]


# ==================================================================================
# M19.5 (docs/open-questions.md question 132, decided by the lead): the globe's
# imagery source is a profile setting -- altavista/server.py's Hub.put() stamps the
# active profile's imagery config onto every published scenario, and the required
# test is that the *attribution string actually reaches the viewer*, not merely the
# raw published JSON. Reuses this module's own `client`/frozen-bundle fixtures (the
# `tests/fixtures/demo_two_instance.runproducts.bin` this task's brief names) so this
# needs neither `cargo` nor a live `av-run` build.
# ==================================================================================

def test_published_scenario_carries_the_active_profiles_imagery_config(published_frozen_demo_scenario):
    """`POST /api/cdm/run` (like every publish path) must carry the server's active
    profile's `imagery` config -- here the default ("design") profile,
    `profiles/design.yaml`. Fails against a server that never wires
    `altavista.profile.load_imagery_config` into `Hub.put` at all (the scenario would
    have no `imagery` key), or one that hardcodes a different attribution/template
    than what the profile file actually declares.
    """
    expected = profile_loader.load_imagery_config(profile_loader.DEFAULT_PROFILE_ID)
    assert published_frozen_demo_scenario["imagery"] == expected


def test_attribution_string_reaches_the_headless_viewer_harness(published_frozen_demo_scenario, tmp_path):
    """The actual point of M19.5's third required test: the attribution string must
    reach *the viewer's own code path*, not just sit in the raw server JSON. Runs the
    real, shipped `web/js/verify_cdm_run.mjs` (extended, M19.5, to report
    `globe.imagery` off the scenario object exactly as `web/js/app.js` would read it)
    against the real published scenario. Fails against a harness/viewer read that drops
    or renames the field (e.g. `globeFacts()` never surfacing `scenario.imagery` at
    all), or a server that publishes a non-empty `imagery` object with a blank/missing
    `attribution` (an imagery source with an attribution string nothing displays is not
    done, per this task's own brief).
    """
    declared = profile_loader.load_imagery_config(profile_loader.DEFAULT_PROFILE_ID)
    assert published_frozen_demo_scenario["imagery"]["attribution"] == declared["attribution"]
    assert declared["attribution"].strip(), "the profile's own attribution string must not be blank"

    result = _run_headless_harness(published_frozen_demo_scenario, "0.0 h", tmp_path)
    assert result["globe"]["imagery"] is not None, "verify_cdm_run.mjs's globeFacts() did not surface scenario.imagery"
    assert result["globe"]["imagery"]["attribution"] == declared["attribution"]
    assert result["globe"]["imagery"]["urlTemplate"] == declared["urlTemplate"]
    assert result["globe"]["imagery"]["maxLevel"] == declared["maxLevel"]


def test_bodies_from_frames_dedupes_a_body_named_by_more_than_one_frame():
    """A body named by more than one frame (e.g. a bundle declaring both
    `EarthMJ2000Eq` and a sibling `EarthICRF`, both origin `Earth` -- exactly what a
    concurrent worker on this same fixture was adding while this task ran) must be
    sampled exactly once, not once per frame naming it.

    Fails against an implementation that appends one `BodyTrack` per matching
    `FrameDefinition` without deduping by body name (would produce two `"Earth"`
    entries here).
    """
    frame = Frame(name="EarthMJ2000Eq", origin="Earth", axes="MJ2000Eq")
    fd_a = core_pb2.FrameDefinition(id="EarthMJ2000Eq", body="Earth", axes=core_pb2.AXES_KIND_MJ2000_EQ)
    fd_b = core_pb2.FrameDefinition(id="EarthICRF", body="Earth", axes=core_pb2.AXES_KIND_ICRF)
    tracks = cdm_adapter.bodies_from_frames([fd_b, fd_a], frame=frame, span=(31041.5, 31041.6))
    assert [t.name for t in tracks] == ["Earth"]


def test_bodies_from_frames_never_adds_a_body_no_frame_names():
    """Body identity comes from the wire, never invented (question 125's decided
    contract, verbatim): no frames at all, and a frame whose `origin` names a
    `platform_id` rather than a `body`, must both yield an empty body list -- neither
    case may fall back to the entities frame's own `origin` (`frame.origin == "Earth"`
    here) the way `altavista.scenario.Scenario.default_bodies()` does for a
    Python-authored scenario.

    Fails against an implementation that always includes `frame.origin` regardless of
    whether any `FrameDefinition` actually names it as a `body`, or one that treats any
    `WhichOneof("origin")` case (not just `"body"`) as naming a body.
    """
    frame = Frame(name="EarthMJ2000Eq", origin="Earth", axes="MJ2000Eq")
    assert cdm_adapter.bodies_from_frames([], frame=frame, span=(31041.5, 31041.6)) == []

    fd_platform = core_pb2.FrameDefinition(id="GroundFrame", platform_id="gs1", axes=core_pb2.AXES_KIND_RIC)
    assert cdm_adapter.bodies_from_frames([fd_platform], frame=frame, span=(31041.5, 31041.6)) == []


def test_bodies_from_frames_raises_a_typed_error_for_an_unknown_body():
    """A frame naming an origin body this process's GMAT solar system has no data for
    must raise :class:`altavista.cdm.UnknownBodyError` -- never a silent skip (the body
    simply missing from the returned list with no other signal) and never a bare,
    untyped exception escaping from underneath `altavista.bodies.BodySampler`.

    Fails against an implementation that wraps the sampler call in a bare
    `try/except: continue` (silently drops the unknown body, returns a list missing it
    with no error at all), or one that lets `BodySampler`'s own `AttributeError`/GMAT
    exception propagate uncaught instead of the documented, typed
    :class:`~altavista.cdm.CdmAdapterError` subclass every other error path in this
    module already uses.
    """
    frame = Frame(name="EarthMJ2000Eq", origin="Earth", axes="MJ2000Eq")
    fd_bad = core_pb2.FrameDefinition(id="BadFrame", body="Xylophonia", axes=core_pb2.AXES_KIND_LOCAL_CARTESIAN)
    with pytest.raises(cdm_adapter.UnknownBodyError):
        cdm_adapter.bodies_from_frames([fd_bad], frame=frame, span=(31041.5, 31041.6))


def test_publish_cdm_run_rejects_a_frame_naming_an_unknown_body_with_a_typed_4xx(client: TestClient):
    """End-to-end: `POST /api/cdm/run` must turn an unknown-body frame into a typed 400
    (status code and JSON error body both asserted), never a 500 and never a silent
    200 with an incomplete body list.

    The malformed frame here uses `AXES_KIND_LOCAL_CARTESIAN` (frameless, no GMAT
    `CoordinateSystem` construction -- `altavista.frames.FrameRegistry._register_local_
    cartesian`) rather than a body-axes kind like `MJ2000Eq`, deliberately: a body-axes
    `FrameDefinition` naming an unknown origin body fails *frame registration itself*
    (`altavista.frames.FrameRegistry.register()`, verified directly while building this
    task to raise a raw, un-typed `gmatpy` `APIException` for an unknown origin body --
    a separate, pre-existing gap in that module from an earlier milestone, out of this
    task's scope (`altavista/frames.py` is not in this task's file list) and not
    reachable from a real `av-run` producer's own frames, which only ever emit
    ENU/NED-free body-axes definitions for a body GMAT already knows about). Using
    `LOCAL_CARTESIAN` isolates *this* task's own new code path
    (`altavista.cdm.bodies_from_frames`, run deliberately before frame-registry
    validation in `altavista/server.py` -- see that handler's own comment) from that
    unrelated gap, so this test exercises exactly the error-typing this task added, not
    a coincidental interaction with a different module's bug.

    Fails against an implementation that lets the exception propagate as an unhandled
    500 (this endpoint's own `except cdm_adapter.UnknownBodyError` clause missing or
    misplaced after frame registration), or one that silently drops the unknown body
    and returns 200 with `bodies: []`.
    """
    fd_bad = core_pb2.FrameDefinition(id="BadFrame", body="Xylophonia", axes=core_pb2.AXES_KIND_LOCAL_CARTESIAN)
    provenance = core_pb2.Provenance(config_hash="bad-body-hash", run_id="test-bad-body")
    run_products = _build_run_products([], [], provenance, run_id="test-bad-body", frames=[fd_bad])

    resp = client.post("/api/cdm/run", content=run_products.SerializeToString(),
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 400, resp.text
    detail = resp.json()["detail"]
    assert "Xylophonia" in detail, detail


# ==================================================================================
# M20.2 (docs/open-questions.md questions 134, 135, 136 -- viewer half -- and E-27, all
# decided by the lead from the M19 demo drive): four viewer defects, all against the
# CDM ingest path this file already publishes (`published_frozen_demo_scenario`).
#
# Question 134's root cause, found and demonstrated while building this task (not
# where the lead's two named candidates pointed): `altavista/cdm.py:474`'s
# `central=(name == frame.origin)` is correct today -- verified directly, both by
# reading it and by publishing `tests/fixtures/demo_two_instance.runproducts.bin`
# through a real server and inspecting the resulting scenario JSON's
# `bodies[0].central` (`True`) -- and `computeFitRadius()` (web/js/scene.js,
# extracted from `setScenario()` by this task) already correctly incorporates it: the
# whole-scenario `fitRadius` for this fixture is `19.1344089` scene units (Earth's own
# `6378.1363 km * SCALE * 3`), and `fit()`'s own `fitRadius * FIT_DISTANCE_FACTOR`
# camera distance (`45.9`) is nowhere near Earth's own radius. The real bug was
# `web/js/scene.js`'s `setViewFrame()`: its default camera distance for a focus-less
# view of a frame (`sep === 0`) was a flat `1e-4` scene units (~100 m) -- a sensible
# default for a genuine entity-relative RIC/VNB/VVLH frame, whose origin *is* a
# spacecraft, but applied unconditionally, including to a body-axes CDM frame (e.g.
# EarthICRF/EarthBodyFixed, M18.1's mandatory central-body frame set) whose origin is
# a ~6378 km-radius planet -- putting the camera 100 m from Earth's centre, literally
# inside the globe. This is reachable only on the CDM ingest path because M18.1 is
# what offers a body-axes frame *other than* the scenario's own to switch into in the
# first place -- a Python scenario typically has just the one frame in the picker, so
# "the Python scenario path fits correctly" (question 134's own observation) followed
# directly, not coincidentally. `defaultFrameViewRadius()` (exported from scene.js)
# fixes this with body-scale awareness; `resetTargetFrameId()` and the refit-on-
# frame-change logic in `setViewFrame()`'s origin branch fix E-27 (switching frames
# leaves the scene out of view; "Reset view" used to force the frame picker back to
# its first option, so there was no way back into ICRF once there).
#
# Verified live, in addition to the headless tests below: publishing this exact
# frozen fixture to a running `altavista serve` and driving the real, shipped
# `web/js/scene.js`'s `Viewer` in an actual browser (real `THREE.WebGLRenderer`, not
# the headless harness) reproduced the pre-fix bug exactly (switching to `EarthICRF`
# with no focus rendered nothing but a single marker -- the near-zero-position
# `demo_ctrl` native controller -- and the inside of the Earth mesh) and confirmed the
# fix end to end: switching to `EarthICRF` places the camera at exactly `3.0x` Earth's
# own radius (`defaultFrameViewRadius`'s `radius*SCALE*3`), and a simulated "Reset
# view" click (`viewer.focus = null; viewer.fit();`, matching the fixed `app.js`
# handler) leaves the camera in `EarthICRF` at that same safe distance -- never
# resetting to `EarthMJ2000Eq` the way the pre-M20.2 implementation always did.
def test_reset_camera_distance_is_within_fit_radii_bound_for_an_ingested_run(published_frozen_demo_scenario, tmp_path):
    """Question 134's required test, on the real ingested demo fixture: after "Reset
    view" (the entities-frame case -- `fit()`'s `_fitOrigin()`), the camera distance
    must be between two and three `fitRadius` (scene.js's own defined unit, `fit()`'s
    literal `fitRadius * FIT_DISTANCE_FACTOR` formula, `FIT_DISTANCE_FACTOR == 2.4`),
    AND `fitRadius` itself must actually reflect the central body's own padded radius
    (`radius * SCALE * 3`) -- not a degenerate value dominated only by trajectory
    extent or the `1e-3` floor.

    The second assertion is what makes this a real test rather than a tautology: given
    `fit()`'s formula, `distance == fitRadius * 2.4` always trivially sits in
    [2*fitRadius, 3*fitRadius] regardless of what `fitRadius` itself is. Fails against
    an implementation that (a) omits `bodies[].central` from `computeFitRadius()`'s
    `maxR` loop, or an ingest path where `bodies` comes back empty (the pre-M18.2
    state) -- `fitRadius` would collapse to the trajectory-only extent (a few scene
    units for this fixture, far below `centralBodyRadiusSceneUnits * 3`), and the
    camera would land close to or inside the globe even though the 2-3x *ratio* check
    alone would still pass; or (b) a `fit()` distance multiplier moved outside [2, 3]
    ("guessing a fit constant", explicitly disallowed by this task's own brief).
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    fit = result["fit"]
    central_r = fit["centralBodyRadiusSceneUnits"]
    assert central_r is not None, "expected the ingested run's Earth body to be marked central"

    # fitRadius genuinely reflects the central body (not a degenerate/tiny value).
    assert fit["fitRadius"] == pytest.approx(central_r * 3, rel=1e-9), (
        f"fitRadius {fit['fitRadius']} does not match the central body's own padded "
        f"radius ({central_r} * 3 = {central_r * 3}) -- computeFitRadius() may not be "
        f"including bodies[].central in its maxR loop"
    )

    # The literal required bound: camera distance between 2x and 3x fitRadius.
    assert 2 * fit["fitRadius"] <= fit["originResetDistance"] <= 3 * fit["fitRadius"], (
        f"reset camera distance {fit['originResetDistance']} is not between 2x and 3x "
        f"fitRadius ({fit['fitRadius']})"
    )

    # The direct, non-circular "not inside the Earth" check: distance vs. the body's
    # own raw radius (independent ground truth straight off the scenario JSON, not
    # derived from the same fitRadius computation being tested).
    assert fit["originResetDistance"] > central_r * 3, (
        f"reset camera distance {fit['originResetDistance']} does not clear even the "
        f"central body's own 3x-padded radius ({central_r * 3}) -- the camera would "
        f"be at or inside the globe"
    )


def test_default_frame_radius_is_body_scale_for_a_body_axes_frame(published_frozen_demo_scenario, tmp_path):
    """Question 134/E-27's actual root-cause fix, and this task's required "switching
    frames refits" test: `defaultFrameViewRadius()` (web/js/scene.js), the function
    `setViewFrame()`'s non-origin branch calls for a focus-less view, must scale with
    the target frame's own body (`EarthBodyFixed`/`EarthICRF`, both `body: "Earth"` on
    the wire, M18.1's mandatory central-body frame set) -- not the flat RPO-scale
    constant.

    Fails against a wrong implementation that "only reparents" -- i.e. keeps the
    pre-M20.2 `setViewFrame()`, whose default radius for `sep === 0` was
    unconditionally `1e-4` scene units regardless of which frame was being switched
    into. Against that implementation, `nonOriginFrameDefaultRadii["EarthICRF"]` would
    be exactly `1e-4`, about 64000x smaller than Earth's own scene-unit radius here
    (`6.3781363`) -- landing the camera 100 m from Earth's centre, deep inside the
    6378 km-radius globe, the moment a user switches to ICRF (or clicks "Reset view"
    while already there, question 134/E-27's "keep the currently selected frame" fix).
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    fit = result["fit"]
    central_r = fit["centralBodyRadiusSceneUnits"]
    radii = fit["nonOriginFrameDefaultRadii"]
    assert "EarthICRF" in radii and "EarthBodyFixed" in radii, radii

    for frame_id in ("EarthICRF", "EarthBodyFixed"):
        assert radii[frame_id] == pytest.approx(central_r * 3, rel=1e-9), (
            f"defaultFrameViewRadius() for {frame_id!r} is {radii[frame_id]}, expected "
            f"the body-scale value {central_r * 3} (Earth's own radius * SCALE * 3) -- "
            f"got the flat RPO-scale 1e-4 constant instead? that means a switch into "
            f"this frame (or a Reset while parked in it) still lands the camera inside "
            f"the globe"
        )
        assert radii[frame_id] > central_r, (
            f"{frame_id}'s default view radius ({radii[frame_id]}) does not even clear "
            f"Earth's own raw radius ({central_r}) -- the camera would be inside the body"
        )


def test_reset_view_keeps_the_currently_selected_frame(published_frozen_demo_scenario, tmp_path):
    """Question 134/E-27's "Reset view must keep the currently selected frame" half:
    `resetTargetFrameId()` (web/js/scene.js) -- the exact function `fit()` calls for
    its own frame-targeting decision, not a parallel description of it -- must return
    whatever frame the camera is CURRENTLY parented in, never the scenario's base
    entities frame.

    Fails against the pre-M20.2 implementation, which always forced the camera back
    to the entities frame on "Reset view" (`this._cameraFrameId = this._originFrameId`
    unconditionally in the old `fit()`, and `app.js`'s handler additionally reset the
    frame picker to its first option) -- that implementation's equivalent of this
    function would return `originFrameId` regardless of `cameraFrameId`, so this
    fixture's own two distinct frame ids (`EarthMJ2000Eq` the base frame,
    `EarthBodyFixed` the "currently selected" one simulated here) would collapse to
    the same value.
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    fit = result["fit"]
    assert fit["otherFrameId"] is not None, "expected the ingested run to declare more than one frame (M18.1)"
    assert fit["otherFrameId"] in result["frameGraph"]["frameIds"], (
        "otherFrameId must be a real declared frame, not the entities frame itself"
    )
    assert fit["resetTargetWhenCameraInOtherFrame"] == fit["otherFrameId"], (
        f"resetTargetFrameId() returned {fit['resetTargetWhenCameraInOtherFrame']!r} "
        f"for a camera parked in {fit['otherFrameId']!r} -- Reset view is forcing the "
        f"camera back to a different frame instead of keeping the one currently selected"
    )


def test_hud_reflects_the_view_frame_not_the_scenario_base_frame(published_frozen_demo_scenario, tmp_path):
    """Question 135's required test: `hudText()` (web/js/cdm_run.js, the exact
    function app.js's render loop calls every frame) must reflect the CURRENT view
    frame, not the scenario's static base `frame.name`.

    Fails against the pre-M20.2 implementation
    (`` `${scenario.name} · ${scenario.frame.name} · ...}` ``), which ignores its
    "current view frame" argument entirely and always prints the scenario's base
    frame -- this fixture's own two distinct frame ids (`EarthMJ2000Eq` the base,
    `EarthBodyFixed` a real declared sibling, M18.1) would then produce the *same* HUD
    string for both, which the assertion below rules out.
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    hud = result["hud"]
    assert hud["otherFrame"] is not None and hud["otherFrame"] != hud["baseFrame"]
    assert hud["hudAtBaseFrame"] != hud["hudAtOtherFrame"], (
        "hudText() produced the same HUD string for two different view frame ids -- "
        "it may be ignoring its viewFrameId argument the way the pre-M20.2 "
        "`scenario.frame.name`-based HUD line always did"
    )
    assert hud["baseFrame"] in hud["hudAtBaseFrame"]
    assert hud["otherFrame"] in hud["hudAtOtherFrame"]
    assert hud["baseFrame"] not in hud["hudAtOtherFrame"]
    # Focus half: "focus <name>", not "origin", once a spacecraft is focused.
    # The harness focuses `scenario.spacecraft[0]`, so derive the name rather than
    # hard-coding it: M20.1 (question 133) stopped rendering `demo_ctrl` at all -- a native
    # controller declaring `native.controller.scalar6` has no position class, so it is no
    # longer a focusable spacecraft and the first rendered entry is now `demo_flt`.
    rendered = [sc["name"] for sc in published_frozen_demo_scenario["spacecraft"]]
    assert "demo_ctrl" not in rendered, (
        "the non-physical native controller must not be published as a renderable "
        "spacecraft (question 133); got " + repr(rendered)
    )
    assert f"focus {rendered[0]}" in hud["hudAtBaseFrameWithFocus"]
    assert "origin" not in hud["hudAtBaseFrameWithFocus"]


def test_viewport_pane_title_derives_from_the_frame_actually_shown(published_frozen_demo_scenario, tmp_path):
    """Question 169's required test, verbatim: "a viewport's title equals the frame it
    is actually showing." `viewportPaneTitle()` (web/js/cdm_run.js, called every frame
    by web/js/app.js's render loop through web/js/layout/layout_manager.js's
    `setPaneTitle`) must name the frame a viewport's camera is ACTUALLY parented in,
    never a role name baked into the layout's intent.

    The lead's own bug report (docs/open-questions.md question 169): the RPO default
    layout's first pane is titled "ICRF" (its ROLE in web/js/layout/default_layouts.js's
    `buildRpoTripleViewportLayout()`) while the viewport's own HUD reads
    `EarthMJ2000Eq`, because the Python RPO scenario declares no ICRF frame and the
    viewport falls back to the entities frame it actually got. This test must FAIL
    against exactly that wrong implementation -- a title hardcoded to the pane's
    intended role regardless of the real frame (the pre-M26.5
    `web/js/layout/layout_manager.js` `PANEL_TITLES[ICRF_PANEL_ID] = '3D View -- ICRF'`
    literal) -- which is why it asserts both that the real frame's id appears in the
    title AND that the substring "ICRF" does NOT, for a pane actually showing
    EarthMJ2000Eq: a hardcoded-role implementation passes the first half by accident
    (every pane's title contains "3D View") but fails the second.
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    paneTitle = result["paneTitle"]
    assert paneTitle["beforeAnyFrameIsKnown"] == "3D View", (
        "a pane with no frame assigned yet must show the base label alone, never a "
        "frame name it cannot back up"
    )
    assert paneTitle["whenActuallyShowingEarthMJ2000Eq"] == "3D View -- EarthMJ2000Eq", (
        f"expected the pane title to name the frame actually shown; got "
        f"{paneTitle['whenActuallyShowingEarthMJ2000Eq']!r}"
    )
    assert "ICRF" not in paneTitle["whenActuallyShowingEarthMJ2000Eq"], (
        "a pane actually showing EarthMJ2000Eq must never claim ICRF -- exactly the "
        "mislabelling the lead found live (question 169)"
    )
    assert paneTitle["whenActuallyShowingEarthICRF"] == "3D View -- EarthICRF", (
        "the same pane, actually parented in EarthICRF, must show THAT frame -- proving "
        "the title is a real function of the frame argument, not a constant"
    )


def test_frame_picker_shows_frame_id_with_description_as_tooltip(published_frozen_demo_scenario, tmp_path):
    """Question 136's viewer-half required test: `frameOptionLabel()` (web/js/cdm_run.js,
    the exact function app.js's frame picker/"Frames" list both call) must show the
    frame **id** as the option text, with the producer's `description` as the tooltip
    -- never the reverse.

    Deliberately does not assert anything about the *content* of `description` (a
    concurrent producer-side task is rewriting it to be a human frame description
    rather than a process note) -- only that whatever it is, it lands in the tooltip,
    never the visible label. Fails against the pre-M20.2 `fd.description || fd.id`
    (both places in app.js), which shows this fixture's own long producer prose
    ('registry default for GMAT CoordinateSystem "EarthBodyFixed" ...', 'Declared
    explicitly (question 124): ICRF about Earth...') as the visible option text.
    """
    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    labels = {entry["id"]: entry for entry in result["frameLabels"]}
    assert set(labels) == {"EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"}
    for frame_id, entry in labels.items():
        assert entry["text"] == frame_id, (
            f"frame picker text for {frame_id!r} is {entry['text']!r}, expected the "
            f"bare id -- description prose leaking into the visible label?"
        )
        assert entry["title"], f"frame picker tooltip for {frame_id!r} is empty"
        # Every frame in this fixture carries a real, non-id description (verified via
        # the raw published scenario's own `frames` list) -- the tooltip must actually
        # be that description, not a re-derived copy of the id.
        assert entry["title"] != frame_id, (
            f"frame picker tooltip for {frame_id!r} fell back to the bare id even "
            f"though this fixture's own FrameDefinition declares a real description"
        )


# ==================================================================================
# M21.2 (docs/open-questions.md question 140, decided by the lead, closing the M20
# re-drive gap left open by M20.2 above): "an event whose instance has no position
# class appears on the timeline only, never as a 3D label."
#
# Post-M21.3 status, checked directly by `test_frozen_demo_ctrl_has_no_trajectory_at_all`
# below: the frozen fixture's own `demo_ctrl` instance now declares an *empty* state
# space (`native.controller.empty`) and RunProducts.trajectories carries no entry for it
# at all (2 trajectories, not 3) -- M21.3 went further than M20.1's "no position class"
# case, which left an (unrendered but still real) trajectory on the wire. Because of
# this, `demo_ctrl` was already absent from `published_frozen_demo_scenario["spacecraft"]`
# (`test_hud_reflects_the_view_frame_not_the_scenario_base_frame` above already checks
# this) *before* this task started, and `web/js/scene.js`'s pre-existing
# `mesh.visible = !!sc0` (`sc0` a `Map.get(ev.spacecraft)` lookup against the rendered
# spacecraft) already happened to evaluate `false` for `demo_ctrl`'s events on this
# specific fixture -- so the original symptom (a label literally drawn at Earth's centre)
# does **not** reproduce against the current fixture+code by simply looking at a running
# viewer: the label was already invisible, not merely mispositioned-but-shown.
#
# This is exactly the "same bug wearing a different hat" the task warned about, though:
# that `!!sc0` check was an *implicit* consequence of a Map miss, not a documented rule,
# so it gave the right answer only because `sc.spacecraft` happens to already be
# filtered by `has_position_class` (question 133/M20.1) server-side -- nothing in
# `web/js/scene.js` said so, and nothing was independently testable without a live
# WebGL `Viewer`. This task extracts that exact membership test into an exported,
# documented, headlessly-testable function (`eventHasRenderedInstance`, web/js/scene.js)
# and wires `web/js/verify_cdm_run.mjs`'s new `event3dLabelFacts` through it, so the
# rule is enforced and provable, not merely incidentally true today. The test below
# would fail against a WRONG implementation that gives every `ev.spacecraft`-bearing
# event a 3D label unconditionally (e.g. `eventHasRenderedInstance` returning
# `!!ev.spacecraft` without checking `spacecraftNames` membership at all) -- exactly
# question 140's literal bug, restated as a boolean instead of a scene-graph position.
# ==================================================================================
def test_frozen_demo_ctrl_has_no_trajectory_at_all(frozen_demo_bundle_bytes):
    """Confirms this task's own stated fixture change (M21.3, beyond M20.1's original
    question 133 scope): `demo_ctrl` has no entry in `RunProducts.trajectories` at all
    (2 entries, not 3), so there is no position trajectory to filter -- the instance is
    absent from the wire entirely, not merely present-but-non-Cartesian. Fails if a
    future fixture regeneration quietly restores a (still non-renderable) trajectory
    entry for `demo_ctrl`, which would silently change which code path this task's
    other assertions are actually exercising.
    """
    run_products = run_pb2.RunProducts()
    run_products.ParseFromString(frozen_demo_bundle_bytes)
    assert sorted(run_products.trajectories) == ["demo_flt", "demo_mvr"], dict(run_products.trajectories)
    ctrl_events = {e.name for e in run_products.events if e.entity_id == "demo_ctrl"}
    assert ctrl_events == {"run_start", "run_end"}, (
        "expected demo_ctrl to still emit its two lifecycle events even though it has "
        f"no trajectory at all; got {ctrl_events}"
    )


def test_events_of_a_non_rendered_instance_are_timeline_only_never_a_3d_label(
    published_frozen_demo_scenario, tmp_path,
):
    """The required headless test for question 140's decided rule, both halves in one
    assertion (a fix that drops `demo_ctrl`'s events entirely, rather than merely
    hiding their 3D label, must fail this too):

    1. The timeline listing is unchanged: `demo_ctrl`'s `run_start`/`run_end` events are
       still present in the published scenario's own `events` list (what
       `web/js/app.js`'s `buildLists`/`buildTicks` render on the sidebar list and the
       timeline ticks), each with its real name and epoch -- not dropped, not stripped
       of its `spacecraft` tag.
    2. Zero of `demo_ctrl`'s events would get a 3D label: `web/js/scene.js`'s real,
       shipped `eventHasRenderedInstance()` (run here via `web/js/verify_cdm_run.mjs`'s
       `event3dLabelFacts`, not reimplemented in Python) reports `wouldGetA3dLabel is
       False` for both, since `demo_ctrl` never appears in `scenario["spacecraft"]`
       (question 133/M20.1's `has_position_class` filter, now compounded by M21.3's "no
       trajectory at all").

    As a control, every event belonging to `demo_flt`/`demo_mvr` (both real, rendered
    spacecraft on this fixture) still gets `wouldGetA3dLabel is True` -- proving this
    isn't a blanket "no event ever gets a 3D label" regression.

    Fails against: (a) an implementation that never filters at all
    (`eventHasRenderedInstance` returning `!!ev.spacecraft`, question 140's literal
    bug restated as a boolean) -- `demo_ctrl`'s two events would report `True`; (b) an
    implementation that instead drops `demo_ctrl`'s events from the published scenario
    to "solve" the 3D-label problem -- the timeline assertions below would fail first.
    """
    events_by_name_and_sc = {
        (e["name"], e.get("spacecraft")): e for e in published_frozen_demo_scenario["events"]
    }
    run_start = events_by_name_and_sc[("run_start", "demo_ctrl")]
    run_end = events_by_name_and_sc[("run_end", "demo_ctrl")]
    assert run_start["t"] == pytest.approx(31041.50042863868, abs=1e-6)
    assert run_end["t"] == pytest.approx(31041.58376197201, abs=1e-6)
    assert run_start["type"] == "lifecycle" and run_end["type"] == "lifecycle"

    assert "demo_ctrl" not in {s["name"] for s in published_frozen_demo_scenario["spacecraft"]}, (
        "demo_ctrl must not be a rendered spacecraft (question 133/M20.1, compounded by "
        "M21.3's 'no trajectory at all')"
    )

    result = _run_headless_harness(published_frozen_demo_scenario, "2.0 h", tmp_path)
    facts = result["event3dLabels"]
    ctrl_facts = [f for f in facts if f["spacecraft"] == "demo_ctrl"]
    assert len(ctrl_facts) == 2, f"expected demo_ctrl's two lifecycle events, got {facts}"
    assert all(f["wouldGetA3dLabel"] is False for f in ctrl_facts), (
        f"demo_ctrl has no rendered instance (no position class / no trajectory) but at "
        f"least one of its events would still get a 3D label: {ctrl_facts}"
    )

    rendered_facts = [f for f in facts if f["spacecraft"] in ("demo_flt", "demo_mvr")]
    assert rendered_facts, "expected at least one event tagged to a rendered spacecraft"
    assert all(f["wouldGetA3dLabel"] is True for f in rendered_facts), (
        f"a real, rendered spacecraft's own event unexpectedly got no 3D label: {rendered_facts}"
    )


# ==================================================================================
# M26.4b (docs/open-questions.md question 165, "Scores on the viewer payload"): POST
# /api/cdm/run now threads RunProducts.scores into the published scenario as an
# additive `scores` object keyed by name, each entry `{value, unit, passed}` (`passed`
# null for a measure of effectiveness), plus `meta.scoresSource = "RunProducts.scores"`.
#
# Exercised against `drms/demo_attitude_control.*` (M22.4, "closed-loop attitude
# control"), which declares a real Objective (`controller_pointing_error_at_end`, a
# target/tolerance pass criterion) and a real measure of effectiveness
# (`controller_seq_at_end`, no pass criterion at all) --
# `crates/av-kernel/tests/drm_attitude_control.rs` pins this exact seeded run's
# `products.scores["controller_pointing_error_at_end"].passed == Some(true)`, so this
# file's own "must be a PASSED objective" assertion is not an invented expectation.
#
# `tests/fixtures/demo_attitude_control.runproducts.bin` is a real `RunProducts`
# message a real `av-run` binary produced from:
#   av-run --drm drms/demo_attitude_control.drm.yaml --sos drms/demo_attitude_control.sos.yaml
#          --system drms/demo_attitude_control_truth.system.yaml
#          --system drms/demo_attitude_control_startracker.system.yaml
#          --system drms/demo_attitude_control_imu.system.yaml
#          --system drms/demo_attitude_control_controller.system.yaml
# (the four BINDING_KIND_MODEL instances drms/demo_attitude_control.sos.yaml's own
# topology declares) -- frozen so these tests do not have to invoke `cargo`/`av-run`
# themselves (this task's own environment rule: no `cargo` except to build/run
# `av-run` for a fresh bundle -- this one was produced that way and then frozen, same
# posture as `FROZEN_DEMO_RUN_BUNDLE_PATH` above).
#
# This DRM is a native, non-physical attitude-only closed loop (state spaces
# "attitude.control_demo.space" / "imu.control_demo.bias_state", no position class) --
# both its instances are filtered out of `spacecraft` by M20.1's has_position_class
# rule and it declares no frames, so `bodies`/`frames`/`spacecraft` are correctly empty
# here; only `scores` (this section's own point) and `events` (5997 real
# EVENT_KIND_PORT_COMMAND events the closed loop emits every 10 Hz step, plus 9
# lifecycle events) are populated.
# ==================================================================================
FROZEN_ATTITUDE_CONTROL_BUNDLE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_attitude_control.runproducts.bin"


@pytest.fixture(scope="module")
def attitude_control_bundle_bytes() -> bytes:
    return FROZEN_ATTITUDE_CONTROL_BUNDLE_PATH.read_bytes()


@pytest.fixture(scope="module")
def attitude_control_run_products(attitude_control_bundle_bytes: bytes) -> run_pb2.RunProducts:
    """The same frozen bytes, decoded directly with the generated bindings -- the
    independent reference this section's assertions check the server's JSON against,
    never a value invented in this test file."""
    rp = run_pb2.RunProducts()
    rp.ParseFromString(attitude_control_bundle_bytes)
    return rp


@pytest.fixture()
def published_attitude_control_scenario(client: TestClient, attitude_control_bundle_bytes: bytes) -> dict:
    resp = client.post("/api/cdm/run", content=attitude_control_bundle_bytes,
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    return resp.json()


def test_frozen_attitude_control_bundle_declares_the_expected_two_scores(attitude_control_run_products):
    """Sanity check on the frozen fixture itself, independent of the server: it must
    still declare exactly `controller_pointing_error_at_end` (an Objective, `passed`
    set to `True`) and `controller_seq_at_end` (a measure of effectiveness, `passed`
    unset) -- guards against a future fixture regeneration silently changing which
    score names/kinds the rest of this section's assertions exercise.
    """
    rp = attitude_control_run_products
    assert sorted(rp.scores) == ["controller_pointing_error_at_end", "controller_seq_at_end"]
    assert rp.scores["controller_pointing_error_at_end"].HasField("passed")
    assert rp.scores["controller_pointing_error_at_end"].passed is True
    assert not rp.scores["controller_seq_at_end"].HasField("passed")


def test_scores_are_threaded_into_the_scenario_payload_with_the_approved_wire_shape(
    published_attitude_control_scenario, attitude_control_run_products,
):
    """The core of question 165: `POST /api/cdm/run` must thread `RunProducts.scores`
    into the published scenario as an additive `scores` object keyed by name, each
    entry `{value, unit, passed}` -- checked here against the SAME frozen bundle's own
    raw, independently-decoded `RunProducts.scores` (never a value invented in this
    test), so a server bug that drops a field, gets `unit` wrong, or reports the wrong
    `value` cannot pass by accident. This IS "the panel shows the demo's pointing
    objective with its pass state, from a real av-run bundle" -- the published wire
    payload the panel's `objectiveRows()` (already proven correct at the binding level
    by tests/test_viewer_panels.py) consumes directly.

    Fails against: the pre-M26.4b implementation (`"scores"` key absent from the
    published JSON entirely -- a `KeyError` here, not merely a wrong value); a handler
    that reports the wrong `value` (e.g. an accidental unit-conversion factor); or one
    that gets `unit` wrong (e.g. hardcoding `UNIT_DIMENSIONLESS` for everything).
    """
    scores = published_attitude_control_scenario["scores"]
    rp = attitude_control_run_products
    pointing = scores["controller_pointing_error_at_end"]
    assert pointing["value"] == rp.scores["controller_pointing_error_at_end"].value
    assert pointing["unit"] == "UNIT_RADIAN"
    assert pointing["passed"] is True, "the demo's real pointing objective is a PASSED objective on this seeded run"

    seq = scores["controller_seq_at_end"]
    assert seq["value"] == rp.scores["controller_seq_at_end"].value
    assert seq["unit"] == "UNIT_DIMENSIONLESS"

    assert published_attitude_control_scenario["meta"]["scoresSource"] == "RunProducts.scores"


def test_measure_of_effectiveness_passed_is_null_not_false_on_the_wire(
    published_attitude_control_scenario,
):
    """Question 165's own literal requirement, guarded at the WIRE level -- M26.4's
    binding-level test (`web/js/panels_check.mjs`'s `objectiveRows` checks) already
    guards this at the JS-binding level against a hand-attached fixture; this is the
    server's own JSON, straight off `GET /api/scenario/{name}`. `controller_seq_at_end`
    has no target/tolerance at all (a measure of effectiveness, ADR-005 sec 6) -- its
    `passed` key must be present and exactly `None` (JSON `null`): never coerced to
    `False` (which would misreport it as a FAILED objective, the obvious wrong
    implementation), and never merely absent (which a
    `google.protobuf.json_format.MessageToDict`-on-each-`ScoreResult` implementation
    would produce -- indistinguishable, to a consumer checking for the key, from a
    server that silently dropped the field).
    """
    scores = published_attitude_control_scenario["scores"]
    assert "controller_seq_at_end" in scores
    assert "passed" in scores["controller_seq_at_end"], (
        "the 'passed' key must be present (as null), not omitted, for a measure of effectiveness"
    )
    assert scores["controller_seq_at_end"]["passed"] is None
    assert scores["controller_seq_at_end"]["passed"] is not False


def test_measure_of_effectiveness_passed_is_a_literal_json_null_in_the_raw_response_body(
    client: TestClient, attitude_control_bundle_bytes: bytes,
):
    """Same fact as the test above, proven against the raw HTTP response bytes rather
    than the parsed Python dict: 'the key is absent' and 'the key is present with value
    null' both parse to no `"passed"` entry vs. `"passed": None` under Python's `json`
    module, so a bug that omits the key would still slip past a naive
    `dict.get("passed") is None` check (`{}.get("passed")` is also `None`) -- the test
    above already guards against that by asserting key presence separately, and this
    test corroborates it directly against the literal bytes on the wire.
    """
    resp = client.post("/api/cdm/run", content=attitude_control_bundle_bytes,
                       headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    m = re.search(r'"controller_seq_at_end"\s*:\s*\{[^}]*\}', resp.text)
    assert m, f"controller_seq_at_end score object not found in the raw response body: {resp.text[:500]!r}..."
    assert re.search(r'"passed"\s*:\s*null', m.group(0)), (
        f"expected a literal 'passed: null' inside {m.group(0)!r} -- the measure of "
        f"effectiveness must not omit the key or coerce it to false"
    )


def test_scores_is_additive_and_empty_by_default_meta_still_stamps_scoressource(
    client: TestClient, run_bundle_bytes: bytes,
):
    """M26.4b is additive: `run_bundle_bytes` (the golden maneuver DRM, no `scores:`
    section declared at all in `drms/leo_1day_maneuver_vnb.drm.yaml`) must still publish
    a `scores` key -- empty, not absent -- and `meta.scoresSource` is stamped even when
    there is nothing to report, matching the established `bodiesSource` convention
    (`altavista/server.py`'s own "present even when the derived list is empty" note).
    Fails against an implementation that only adds the `scores` key when the map is
    non-empty (a consumer checking `"scores" in scenario` would then disagree about
    whether this endpoint threads scores at all, depending on which DRM produced it).
    """
    resp = client.post("/api/cdm/run", content=run_bundle_bytes, headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    scenario = resp.json()
    assert scenario["scores"] == {}
    assert scenario["meta"]["scoresSource"] == "RunProducts.scores"

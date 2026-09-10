"""F3c (this task's own brief, following F3b's panel and worker A's ``POST /api/cdm/sweep``
route): the JOIN test. F3b's ``web/js/panels/feasibility_panel.js`` was so far proven only
against a hand-authored fixture (``web/js/fixtures/feasibility_sweep_fixture.json``,
``tests/test_viewer_feasibility_panel.py``) and F3b's own ``web/js/app.js``
``openFeasibilitySample()`` tried to open a sample straight from the browser via
``fetch(drawRow.productsUri)`` -- which cannot work, for two independent reasons verified
against the source before anything here was changed:

1. ``SweepSample.products_uri`` is the sample's **directory**, not a file --
   ``crates/av-sweep/src/bin/av-sweep/study.rs``'s ``finalize()`` (read directly for this
   task) sets it via
   ``std::fs::canonicalize(&r.sample_dir).map(|p| p.display().to_string())...`` -- the real
   ``RunProducts`` bytes are one path segment further in, at
   ``<products_uri>/run_products.pb``.
2. It is an absolute **local filesystem path** on whichever machine ran the study (e.g.
   ``/private/var/folders/.../sample_p0_d0``). A browser's own ``fetch()`` of that string
   resolves it against the page's own origin and asks the viewer's HTTP server for a path
   shaped like that, which serves nothing -- confirmed by reading
   ``web/js/app.js``'s pre-fix ``openFeasibilitySample()`` (``fetch(drawRow.productsUri)``
   followed by ``POST /api/cdm/run`` of the response bytes) side by side with (1).

This file is the fix's own proof, with the real, shipped code and no reimplementation:

* ``test_panel_checks_pass_against_the_real_server_published_sweep`` publishes the real,
  frozen ``tests/fixtures/demo_two_instance_sweep.sweepresults.bin`` through the real
  ``POST /api/cdm/sweep`` route, then hands the resulting **published scenario's own
  ``sweep`` dict** (never the hand-authored fixture) to the real, shipped
  ``web/js/panels_check.mjs`` harness's new "11. feasibility panel against the SERVER'S OWN
  output" section, proving ``feasibility_panel.js``'s real functions against the server's
  real payload for the first time.
* ``test_open_sample_publishes_the_real_run_with_its_real_trajectories_and_scores`` proves
  ``POST /api/cdm/sweep/sample`` end to end. The frozen sweep fixture's own recorded
  ``productsUri`` values point at a temp directory from the run that produced them, which no
  longer exists on this machine -- so this test does NOT fake the route's behaviour. It
  instead rewrites ONE sample's ``products_uri`` (in a copy of the real, decoded
  ``SweepResults`` protobuf, before re-publishing it) to a temp directory this test itself
  creates, and places the repo's existing real
  ``tests/fixtures/demo_two_instance.runproducts.bin`` in it as ``run_products.pb``. Only
  the LOCATION is test-controlled; every byte the route reads once it gets there is the
  real, frozen ``RunProducts`` fixture, decoded through the real route.
* ``test_open_sample_refuses_a_failed_sample_as_not_openable`` proves the "sample failed ->
  not openable" refusal against a sample whose ``error`` is genuinely non-empty (mirroring
  ``crates/av-sweep``'s own "a failed sample never got far enough to have a products_uri"
  invariant).
* A battery of typed-refusal tests, one per refusal ``POST /api/cdm/sweep/sample``'s own
  docstring documents, each with its own test function and its own exact-cause assertion.
* ``test_caller_supplied_products_uri_is_never_trusted`` is this task's break-and-restore
  evidence for the identity-not-path rule: a request that supplies its own ``productsUri``
  field alongside a legitimate ``sweepId``/``pointIndex``/``drawIndex`` for a FAILED sample
  must still be refused -- the server never even looks at a caller-supplied path. See this
  task's own report for the exact captured failure text when the route was temporarily
  broken to trust that field, and the restore diff.
"""
from __future__ import annotations

import json
import math
import shutil
import subprocess
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parent.parent
PANELS_CHECK = REPO_ROOT / "web" / "js" / "panels_check.mjs"
SWEEP_FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance_sweep.sweepresults.bin"
RUN_FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance.runproducts.bin"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/panels_check.mjs "
                     "drives real ES modules and is intentionally not ported to Python.")
    return NODE


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def sweep_fixture_bytes() -> bytes:
    assert SWEEP_FIXTURE_PATH.is_file(), f"missing {SWEEP_FIXTURE_PATH}"
    return SWEEP_FIXTURE_PATH.read_bytes()


@pytest.fixture(scope="module")
def sweep_fixture_results(sweep_fixture_bytes: bytes) -> run_pb2.SweepResults:
    sr = run_pb2.SweepResults()
    sr.ParseFromString(sweep_fixture_bytes)
    return sr


@pytest.fixture(scope="module")
def run_fixture_bytes() -> bytes:
    assert RUN_FIXTURE_PATH.is_file(), f"missing {RUN_FIXTURE_PATH}"
    return RUN_FIXTURE_PATH.read_bytes()


@pytest.fixture(scope="module")
def run_fixture_products(run_fixture_bytes: bytes) -> run_pb2.RunProducts:
    rp = run_pb2.RunProducts()
    rp.ParseFromString(run_fixture_bytes)
    return rp


@pytest.fixture()
def client(tmp_path) -> TestClient:
    app = create_app(texture_dir=tmp_path, web_dir=tmp_path)
    return TestClient(app)


# --------------------------------------------------------------------------- helpers
def _publish_sweep(client: TestClient, sr: run_pb2.SweepResults) -> dict:
    """Publishes ``sr`` through the real ``POST /api/cdm/sweep`` route and reads the
    resulting scenario back through ``GET /api/scenario/{name}`` -- the exact JSON shape
    a browser's ``loadScenario()`` receives."""
    resp = client.post("/api/cdm/sweep", content=sr.SerializeToString(),
                        headers={"content-type": "application/x-protobuf"})
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]
    resp = client.get(f"/api/scenario/{name}")
    assert resp.status_code == 200, resp.text
    return resp.json()


def _sweep_with_real_products_at(sweep_fixture_bytes: bytes, run_fixture_bytes: bytes, tmp_path: Path,
                                  *, point_index: int, draw_index: int) -> run_pb2.SweepResults:
    """A copy of the real, frozen sweep fixture with ONE sample's ``products_uri`` rewritten
    to a temp directory this test creates, containing the repo's real, existing
    ``tests/fixtures/demo_two_instance.runproducts.bin`` as ``run_products.pb``. This is the
    brief's own required approach: the frozen fixture's own recorded ``productsUri`` points
    at a temp directory from the run that originally produced it, which no longer exists on
    this machine, so faking a filesystem hit would prove nothing -- only the LOCATION is
    test-controlled here; every byte the route reads once it gets there is the real, frozen
    ``RunProducts`` fixture, unmodified.
    """
    sr = run_pb2.SweepResults()
    sr.ParseFromString(sweep_fixture_bytes)
    sample = next(s for s in sr.samples if s.point_index == point_index and s.draw_index == draw_index)
    sample_dir = tmp_path / f"real_sample_p{point_index}_d{draw_index}"
    sample_dir.mkdir()
    (sample_dir / "run_products.pb").write_bytes(run_fixture_bytes)
    sample.products_uri = str(sample_dir)
    return sr


def _sweep_with_failed_sample_at(sweep_fixture_bytes: bytes, *, point_index: int, draw_index: int,
                                  error_text: str) -> run_pb2.SweepResults:
    """A copy of the real, frozen sweep fixture with ONE sample rewritten to look like a
    genuinely failed sample: ``error`` set, ``products_uri``/``scores``/``seeds`` cleared --
    the exact shape ``crates/av-sweep/src/aggregate.rs``'s own documented "a failed sample
    never got far enough to derive any scores/seeds/products_uri" rule describes."""
    sr = run_pb2.SweepResults()
    sr.ParseFromString(sweep_fixture_bytes)
    sample = next(s for s in sr.samples if s.point_index == point_index and s.draw_index == draw_index)
    sample.error = error_text
    sample.products_uri = ""
    sample.ClearField("scores")
    sample.ClearField("seeds")
    return sr


def _open_sample(client: TestClient, *, sweep_id: str, point_index: int, draw_index: int, extra: dict | None = None):
    body = {"sweepId": sweep_id, "pointIndex": point_index, "drawIndex": draw_index}
    if extra:
        body.update(extra)
    return client.post("/api/cdm/sweep/sample", json=body)


def _population_aggregate(values: list[float]) -> tuple[float, float]:
    """The same population mean/stdDev formula ``crates/av-sweep/src/aggregate.rs``
    documents and implements (read, not edited) -- computed here directly off the decoded
    protobuf, independent of ``altavista.server._sweep_results_to_dict`` (the function
    actually under test)."""
    n = len(values)
    mean = sum(values) / n
    std_dev = math.sqrt(sum((v - mean) ** 2 for v in values) / n)
    return mean, std_dev


def _quat_conjugate_rotate(qx, qy, qz, qw, vx, vy, vz):
    """See tests/test_viewer_panels.py's own identical helper for the full derivation
    comment -- duplicated verbatim here rather than imported (this repo's existing viewer
    test files each stay self-contained; tests/test_viewer_feasibility_panel.py's own
    docstring states the same convention)."""
    ux, uy, uz = -qx, -qy, -qz
    c1x = uy * vz - uz * vy
    c1y = uz * vx - ux * vz
    c1z = ux * vy - uy * vx
    c2x = uy * c1z - uz * c1y
    c2y = uz * c1x - ux * c1z
    c2z = ux * c1y - uy * c1x
    return (vx + 2 * qw * c1x + 2 * c2x,
            vy + 2 * qw * c1y + 2 * c2y,
            vz + 2 * qw * c1z + 2 * c2z)


WGS84_A_M = 6378137.0
WGS84_B_M = 6356752.314245
WGS84_E2 = 1 - (WGS84_B_M ** 2) / (WGS84_A_M ** 2)


def _ecef_to_geodetic_iterative(x_m, y_m, z_m):
    p = math.hypot(x_m, y_m)
    lon = math.atan2(y_m, x_m)
    lat = math.atan2(z_m, p * (1 - WGS84_E2))
    for _ in range(10):
        sin_lat = math.sin(lat)
        n = WGS84_A_M / math.sqrt(1 - WGS84_E2 * sin_lat * sin_lat)
        h = p / math.cos(lat) - n
        lat = math.atan2(z_m, p * (1 - WGS84_E2 * n / (n + h)))
    sin_lat = math.sin(lat)
    n = WGS84_A_M / math.sqrt(1 - WGS84_E2 * sin_lat * sin_lat)
    h = p / math.cos(lat) - n
    return math.degrees(lon), math.degrees(lat), h


def _run_panels_check(input_payload: dict, tmp_path: Path) -> dict:
    node = _require_node()
    path = tmp_path / "panels_input.json"
    path.write_text(json.dumps(input_payload))
    proc = subprocess.run([node, str(PANELS_CHECK), str(path)],
                           cwd=str(PANELS_CHECK.parent), capture_output=True, text=True, timeout=30)
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"panels_check.mjs did not print valid JSON (exit {proc.returncode})\n"
                              f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}")


def _failed(data: dict, substring: str) -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched substring {substring!r} -- panels_check.mjs's check names changed?"
    return matches


# ============================================================================
# 1. the panel's own code, run against the SERVER'S REAL published sweep (not the
#    hand-authored fixture) -- the actual point of this task.
# ============================================================================
def test_panel_checks_pass_against_the_real_server_published_sweep(
        client: TestClient, sweep_fixture_bytes: bytes, sweep_fixture_results: run_pb2.SweepResults,
        run_fixture_bytes: bytes, run_fixture_products: run_pb2.RunProducts, tmp_path):
    """Publishes the real, frozen demo_two_instance_sweep study through the real
    ``POST /api/cdm/sweep`` route and hands the resulting scenario's own ``sweep`` dict to
    ``web/js/panels_check.mjs``'s new "server-sweep " section, which drives the real
    ``scoreNames``/``gridAxes``/``axisLevels``/``gridRows``/``drawRows``/``isSampleOpenable``
    against it. Every expectation is computed independently, in this function, directly off
    the decoded ``run_pb2.SweepResults`` protobuf -- never by calling
    ``altavista.server._sweep_results_to_dict`` (the very function whose output is under
    test) and never copied from the hand-authored ``feasibility_sweep_fixture.json`` (whose
    own score-name set is NOT the same as the real study's -- the real study has a third
    score, ``demo_flt_cd_at_end``, the hand-authored fixture never included; a check that
    silently reused the hand-authored fixture's "exactly two names" assumption against real
    data would be proving the wrong thing).

    Fails against a ``POST /api/cdm/sweep`` (or ``_sweep_results_to_dict``) regression that
    drops a score/axis/sample, mis-sorts points/samples, or corrupts a value along the way
    -- and, on the panel side, against a `feasibility_panel.js` that only happens to work
    for the shape of the hand-authored fixture (e.g. one that silently assumed exactly two
    scores).
    """
    scenario = _publish_sweep(client, sweep_fixture_results)
    sweep = scenario["sweep"]
    assert sweep["sweepId"] == "demo_two_instance_sweep"

    expected_score_names = sorted({a.name for a in sweep_fixture_results.aggregates})
    assert len(expected_score_names) == 3, "test assumption: the real study declares 3 scores"
    expected_axis_keys = sorted({k for s in sweep_fixture_results.samples for k in s.axis_values.keys()})
    expected_axis_levels = {
        k: sorted({s.axis_values[k] for s in sweep_fixture_results.samples if k in s.axis_values})
        for k in expected_axis_keys
    }
    point0_samples = sorted((s for s in sweep_fixture_results.samples if s.point_index == 0),
                             key=lambda s: s.draw_index)
    point0_mvr_values = [s.scores["demo_mvr_rmag_at_end"].value for s in point0_samples]
    mean, std_dev = _population_aggregate(point0_mvr_values)

    # Sections 1-9 of panels_check.mjs need a real, well-formed `scenario` to run against
    # without throwing (this file's own job is only the new section 11) -- duplicated
    # fixture-building, same convention tests/test_viewer_feasibility_panel.py's own module
    # docstring documents ("this repo's existing viewer test files each stay self-contained").
    run_resp = client.post("/api/cdm/run", content=run_fixture_bytes, headers={"content-type": "application/x-protobuf"})
    assert run_resp.status_code == 200, run_resp.text
    run_name = run_resp.json()["name"]
    published_run_scenario = client.get(f"/api/scenario/{run_name}").json()

    flt = next(s for s in published_run_scenario["spacecraft"] if s["name"] == "demo_flt")
    earth = next(b for b in published_run_scenario["bodies"] if b["name"] == "Earth")
    qx, qy, qz, qw = earth["quat"][0:4]
    ex, ey, ez = earth["pos"][0:3]
    sx, sy, sz = flt["pos"][0:3]
    rel_km = (sx - ex, sy - ey, sz - ez)
    body_fixed_km = _quat_conjugate_rotate(qx, qy, qz, qw, *rel_km)
    lon_deg, lat_deg, alt_m = _ecef_to_geodetic_iterative(*(c * 1000 for c in body_fixed_km))
    t0, t1 = published_run_scenario["t0"], published_run_scenario["t1"]
    fault = next(e for e in published_run_scenario["events"] if e["type"] == "fault")
    expected_fault_pct = (fault["t"] - t0) / (t1 - t0) * 100
    published_run_scenario["scores"] = {}

    data = _run_panels_check({
        "scenario": published_run_scenario,
        "expectedGroundTrack": {"lonDeg": lon_deg, "latDeg": lat_deg, "altM": alt_m},
        "expectedFaultTimelinePercent": expected_fault_pct,
        "serverFeasibilitySweep": sweep,
        "expectedServerScoreNames": expected_score_names,
        "expectedServerAxisKeys": expected_axis_keys,
        "expectedServerAxisLevels": expected_axis_levels,
        "expectedServerPoint0Mvr": {"mean": mean, "stdDev": std_dev},
        "expectedServerPoint0MvrDraws": point0_mvr_values,
    }, tmp_path)

    print(f"\npanels_check.mjs (server-sweep join): {len(data['checks'])} checks total")
    server_checks = _matched(data, "server-sweep ")
    n_fail = sum(1 for c in server_checks if not c["pass"])
    print(f"  {len(server_checks)} server-sweep checks, {n_fail} failing")
    for c in server_checks:
        print(f"  [{'PASS' if c['pass'] else 'FAIL'}] {c['name']}")
    failed = _failed(data, "server-sweep ")
    assert not failed, f"server-sweep checks failed against the real server-published sweep: {failed}"


# ============================================================================
# 2. opening a sample end to end -- the actual defect fix.
# ============================================================================
def test_open_sample_publishes_the_real_run_with_its_real_trajectories_and_scores(
        client: TestClient, sweep_fixture_bytes: bytes, run_fixture_bytes: bytes,
        run_fixture_products: run_pb2.RunProducts, tmp_path):
    """The end-to-end proof: ``POST /api/cdm/sweep`` publishes a study, then
    ``POST /api/cdm/sweep/sample`` -- given only ``{sweepId, pointIndex, drawIndex}``, never
    a path -- opens point 0 draw 0's real run. That sample's ``products_uri`` was rewritten
    (see ``_sweep_with_real_products_at``'s own docstring) to a temp directory holding the
    real ``tests/fixtures/demo_two_instance.runproducts.bin`` bytes as ``run_products.pb``;
    everything downstream of that rewrite is the real route reading a real file and running
    the real ``_run_products_to_scenario_data`` conversion.

    Fails against: a route that never actually reads/decodes the file (the published
    scenario would be empty or wrong); one that reads ``<products_uri>`` itself instead of
    ``<products_uri>/run_products.pb`` (SweepSample.products_uri is a directory -- this
    task's own defect); or a conversion that silently drops trajectories/scores.
    """
    sr = _sweep_with_real_products_at(sweep_fixture_bytes, run_fixture_bytes, tmp_path, point_index=0, draw_index=0)
    _publish_sweep(client, sr)

    resp = _open_sample(client, sweep_id=sr.sweep_id, point_index=0, draw_index=0)
    assert resp.status_code == 200, resp.text
    name = resp.json()["name"]

    scenario = client.get(f"/api/scenario/{name}").json()
    assert scenario["meta"]["configHash"] == run_fixture_products.provenance.config_hash
    published_names = {s["name"] for s in scenario["spacecraft"]}
    assert "demo_flt" in published_names, published_names
    assert "demo_mvr" in published_names, published_names
    assert len(scenario["events"]) == len(run_fixture_products.events)
    # The real fixture's own 3 scores (tests/test_viewer_panels.py's own "objectiveRows:
    # exactly 3 rows" section 4 comment) must all reach this published run scenario.
    assert set(scenario["scores"].keys()) == {"demo_flt_rmag_at_end", "demo_mvr_rmag_at_end", "demo_flt_cd_at_end"}
    assert scenario["scores"]["demo_flt_rmag_at_end"]["passed"] is True


# ============================================================================
# 3. the "sample failed -> not openable" refusal.
# ============================================================================
def test_open_sample_refuses_a_failed_sample_as_not_openable(client: TestClient, sweep_fixture_bytes: bytes):
    """A sample whose ``error`` is genuinely non-empty must be refused -- there is nothing
    at a products_uri to open (crates/av-sweep's own "a failed sample never got far enough
    to derive a products_uri" rule). Fails against a route that ignores ``error`` and tries
    to open ``productsUri`` anyway (empty string -> a confusing, wrongly-worded 404 instead
    of an honest "this sample failed" refusal), or one that returns 200 for a failed sample.
    """
    error_text = "axis names demo_flt.spacecraft.DragArea = 999.0, which is outside its declared bound [0, 30]"
    sr = _sweep_with_failed_sample_at(sweep_fixture_bytes, point_index=1, draw_index=0, error_text=error_text)
    _publish_sweep(client, sr)

    resp = _open_sample(client, sweep_id=sr.sweep_id, point_index=1, draw_index=0)
    assert resp.status_code == 409, resp.text
    assert "failed" in resp.json()["detail"].lower()
    assert error_text in resp.json()["detail"]


# ============================================================================
# 4. typed refusals, each its own test.
# ============================================================================
def test_open_sample_refuses_unknown_sweep_id(client: TestClient):
    """No sweep named ``sweepId`` has ever been published to this server (the hub has no
    scenario named ``sweep:{sweepId}``) -- fails against a route that 500s on a missing key
    instead of a typed 404 naming the sweep it looked for.
    """
    resp = _open_sample(client, sweep_id="never_published", point_index=0, draw_index=0)
    assert resp.status_code == 404, resp.text
    assert "never_published" in resp.json()["detail"]


def test_open_sample_refuses_unknown_point_index(client: TestClient, sweep_fixture_bytes: bytes,
                                                   sweep_fixture_results: run_pb2.SweepResults):
    """A published sweep with no such point index -- fails against a route that lets a
    ``StopIteration``/``IndexError`` propagate as a 500 instead of a typed 404.
    """
    _publish_sweep(client, sweep_fixture_results)
    resp = _open_sample(client, sweep_id=sweep_fixture_results.sweep_id, point_index=999, draw_index=0)
    assert resp.status_code == 404, resp.text
    assert "999" in resp.json()["detail"]


def test_open_sample_refuses_unknown_draw_index(client: TestClient, sweep_fixture_bytes: bytes,
                                                 sweep_fixture_results: run_pb2.SweepResults):
    """A real point with no such draw index -- fails the same way as the unknown-point case,
    for the inner (samples) lookup rather than the outer (points) one.
    """
    _publish_sweep(client, sweep_fixture_results)
    resp = _open_sample(client, sweep_id=sweep_fixture_results.sweep_id, point_index=0, draw_index=999)
    assert resp.status_code == 404, resp.text
    assert "999" in resp.json()["detail"]


def test_open_sample_refuses_missing_file_on_host(client: TestClient, sweep_fixture_bytes: bytes, tmp_path):
    """A succeeded sample whose recorded ``productsUri`` names a directory that does not
    exist on THIS host (e.g. a sweep published here after being produced elsewhere, or a
    study whose sample directories were since deleted) -- this is the honest, expected
    failure mode this route's own docstring documents, not a bug to route around. Fails
    against a route that raises an unhandled ``FileNotFoundError`` (500) instead of a typed
    404 naming the exact path it looked for.
    """
    sr = run_pb2.SweepResults()
    sr.ParseFromString(sweep_fixture_bytes)
    sample = next(s for s in sr.samples if s.point_index == 0 and s.draw_index == 0)
    missing_dir = tmp_path / "never_created"
    sample.products_uri = str(missing_dir)
    _publish_sweep(client, sr)

    resp = _open_sample(client, sweep_id=sr.sweep_id, point_index=0, draw_index=0)
    assert resp.status_code == 404, resp.text
    assert str(missing_dir) in resp.json()["detail"]
    assert "does not exist on this host" in resp.json()["detail"]


def test_open_sample_refuses_malformed_run_products_file(client: TestClient, sweep_fixture_bytes: bytes, tmp_path):
    """A ``run_products.pb`` that exists on disk but is not a valid ``RunProducts`` message
    -- fails against a route that lets ``DecodeError`` propagate as a 500 instead of a typed
    400 naming the exact file that would not decode.
    """
    sr = run_pb2.SweepResults()
    sr.ParseFromString(sweep_fixture_bytes)
    sample = next(s for s in sr.samples if s.point_index == 0 and s.draw_index == 0)
    sample_dir = tmp_path / "garbage_sample"
    sample_dir.mkdir()
    (sample_dir / "run_products.pb").write_bytes(b"\xff\xfe\x00\x01not a protobuf message at all")
    sample.products_uri = str(sample_dir)
    _publish_sweep(client, sr)

    resp = _open_sample(client, sweep_id=sr.sweep_id, point_index=0, draw_index=0)
    assert resp.status_code == 400, resp.text
    assert "does not decode" in resp.json()["detail"]
    assert "RunProducts" in resp.json()["detail"]


@pytest.mark.parametrize("body", [
    {"sweepId": "", "pointIndex": 0, "drawIndex": 0},
    {"sweepId": "x", "pointIndex": "0", "drawIndex": 0},
    {"sweepId": "x", "pointIndex": 0, "drawIndex": "0"},
    {"sweepId": "x", "pointIndex": True, "drawIndex": 0},
    {"pointIndex": 0, "drawIndex": 0},
])
def test_open_sample_refuses_malformed_request_body(client: TestClient, body: dict):
    """A body missing a required identity field, or one carrying the wrong JSON type (a
    string where an integer is required, or a bool -- which Python's ``int`` would
    otherwise silently accept, since ``bool`` is an ``int`` subclass) -- fails against a
    route that coerces types silently instead of refusing with a typed 400.
    """
    resp = client.post("/api/cdm/sweep/sample", json=body)
    assert resp.status_code == 400, resp.text


# ============================================================================
# 5. identity, never a path -- the break-and-restore target.
# ============================================================================
def test_caller_supplied_products_uri_is_never_trusted(client: TestClient, sweep_fixture_bytes: bytes,
                                                         run_fixture_bytes: bytes, tmp_path):
    """The core safety claim of ``POST /api/cdm/sweep/sample``'s own docstring: the caller
    sends an identity, never a path -- a ``productsUri`` field in the request body, even one
    pointing at a directory that genuinely holds a real ``run_products.pb``, must have NO
    EFFECT. This test targets a FAILED sample specifically (recorded ``productsUri == ""``,
    ``error`` non-empty) so the two possible implementations diverge sharply: the correct
    one refuses with the "sample failed" 409 exactly as it would with no extra field at all;
    an implementation that reads a caller-supplied path (the arbitrary-file-read bug this
    route's own docstring argues against) would instead find the injected file and wrongly
    return 200. See this task's own report for the exact captured failure text from a real
    break-and-restore of this line.
    """
    error_text = "deliberately failed for this identity-not-path test"
    sr = _sweep_with_failed_sample_at(sweep_fixture_bytes, point_index=2, draw_index=1, error_text=error_text)
    _publish_sweep(client, sr)

    injected_dir = tmp_path / "attacker_supplied_real_run"
    injected_dir.mkdir()
    (injected_dir / "run_products.pb").write_bytes(run_fixture_bytes)

    resp = _open_sample(client, sweep_id=sr.sweep_id, point_index=2, draw_index=1,
                         extra={"productsUri": str(injected_dir)})
    assert resp.status_code == 409, (
        f"a caller-supplied productsUri must be ignored -- expected the same 'sample failed' "
        f"refusal as with no extra field, got {resp.status_code}: {resp.text}")
    assert "failed" in resp.json()["detail"].lower()
    assert error_text in resp.json()["detail"]

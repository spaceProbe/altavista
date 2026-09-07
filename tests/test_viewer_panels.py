"""M26.4 "the three panels" tests (docs/ui-rework-plan.md's M26.4 milestone;
docs/open-questions.md question 162: run products & scores, a 2D companion map,
console/log).

Same "run the real code, don't port it" discipline as tests/test_viewer_viewport.py /
test_viewer_layout.py / test_viewer_globe.py: this file publishes the real, frozen
``tests/fixtures/demo_two_instance.runproducts.bin`` fixture through the real
``POST /api/cdm/run`` route (exactly like tests/test_cdm_run.py's own
``published_frozen_demo_scenario`` fixture), decodes the SAME fixture's real
``RunProducts.scores`` directly with the generated ``run_pb2`` bindings (never invented
numbers), hand-computes one ground-track sample and one timeline percentage
independently in this file (a different ECEF<->geodetic algorithm and a from-scratch
quaternion rotation, not a call into any of this project's own JS/Python code -- see
each test's own docstring), and hands all of it to ``web/js/panels_check.mjs``, which
drives the real, shipped ES modules and reports one JSON object of named checks.

See web/js/REPORT_M26_4.md for the full account, in particular the FINDING that
``RunProducts.scores`` never reaches the client on any existing publish path today --
this file's own fixture explicitly ADDS a real, decoded ``scores`` key on top of the
live-server-published scenario JSON so ``run_products_panel.js``'s binding logic is
proven correct against real data, independent of that server-side gap.
"""
from __future__ import annotations

import json
import math
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest
from fastapi.testclient import TestClient
from google.protobuf import json_format

from altavista.pb.altavista.v1 import run_pb2
from altavista.server import create_app

REPO_ROOT = Path(__file__).resolve().parent.parent
PANELS_CHECK = REPO_ROOT / "web" / "js" / "panels_check.mjs"
FROZEN_DEMO_RUN_BUNDLE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_two_instance.runproducts.bin"
# M26.4b (docs/open-questions.md question 165): the closed-loop attitude-control demo
# (M22.4) -- a real Objective (`controller_pointing_error_at_end`, PASSES on this exact
# seeded run per crates/av-kernel/tests/drm_attitude_control.rs) and a real measure of
# effectiveness (`controller_seq_at_end`, no pass criterion). Unlike
# `FROZEN_DEMO_RUN_BUNDLE_PATH` above, this scenario's `scores` come straight off the
# REAL, live-server-published JSON (`altavista/server.py`'s own M26.4b wiring) -- no
# Python-side patching -- since that wiring is this task's own subject, not a gap being
# worked around.
FROZEN_ATTITUDE_CONTROL_BUNDLE_PATH = REPO_ROOT / "tests" / "fixtures" / "demo_attitude_control.runproducts.bin"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/panels_check.mjs "
                     "drives real ES modules and is intentionally not ported to Python.")
    return NODE


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def frozen_demo_bundle_bytes() -> bytes:
    return FROZEN_DEMO_RUN_BUNDLE_PATH.read_bytes()


@pytest.fixture(scope="module")
def run_products(frozen_demo_bundle_bytes: bytes) -> run_pb2.RunProducts:
    """The real, decoded ``RunProducts`` message -- used only to pull ``scores`` out
    (never threaded through ``ScenarioData`` server-side today, see this module's own
    docstring / web/js/REPORT_M26_4.md), and for the hand-computed ground-track check
    below (this run's own real Earth quaternion / demo_flt position, not synthetic
    data)."""
    rp = run_pb2.RunProducts()
    rp.ParseFromString(frozen_demo_bundle_bytes)
    return rp


@pytest.fixture(scope="module")
def published_frozen_demo_scenario(frozen_demo_bundle_bytes: bytes) -> dict:
    """POSTs the real frozen fixture to the real ``POST /api/cdm/run`` route and reads
    the resulting scenario back through ``GET /api/scenario/{name}`` -- the exact JSON
    shape a browser's ``loadScenario()`` (web/js/app.js) receives. Mirrors
    tests/test_cdm_run.py's own fixture of the same shape (kept self-contained here,
    not imported, matching this repo's existing test_viewer_*.py convention of not
    cross-importing between viewer test files)."""
    with tempfile.TemporaryDirectory() as d:
        app = create_app(texture_dir=Path(d), web_dir=Path(d))
        client = TestClient(app)
        resp = client.post("/api/cdm/run", content=frozen_demo_bundle_bytes,
                            headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        name = resp.json()["name"]
        resp = client.get(f"/api/scenario/{name}")
        assert resp.status_code == 200, resp.text
        return resp.json()


@pytest.fixture(scope="module")
def published_attitude_control_scenario() -> dict:
    """POSTs the real, frozen closed-loop attitude-control bundle to the real
    ``POST /api/cdm/run`` route and reads the published scenario back -- exactly
    ``published_frozen_demo_scenario`` above, for the M26.4b (question 165) fixture.
    ``scores`` here is whatever the live server actually threads through, unpatched.
    """
    with tempfile.TemporaryDirectory() as d:
        app = create_app(texture_dir=Path(d), web_dir=Path(d))
        client = TestClient(app)
        resp = client.post("/api/cdm/run", content=FROZEN_ATTITUDE_CONTROL_BUNDLE_PATH.read_bytes(),
                            headers={"content-type": "application/x-protobuf"})
        assert resp.status_code == 200, resp.text
        name = resp.json()["name"]
        resp = client.get(f"/api/scenario/{name}")
        assert resp.status_code == 200, resp.text
        return resp.json()


def _quat_conjugate_rotate(qx, qy, qz, qw, vx, vy, vz):
    """Rotate vector v by the CONJUGATE of quaternion (qx,qy,qz,qw) -- i.e. q^-1 * v,
    the standard quaternion-vector rotation formula (v' = v + 2w(u x v) + 2(u x (u x
    v)), u = conjugate's vector part) written directly here from the textbook formula,
    not imported from web/js/interp.js or any other project code. This is the
    independent half of the hand-computed ground-track check below."""
    ux, uy, uz = -qx, -qy, -qz  # conjugate's vector part
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
    """Independent ECEF -> geodetic conversion via fixed-point iteration on latitude
    (Newton-style, converges in a handful of steps) -- deliberately a DIFFERENT
    algorithm from web/js/globe_lod.js's `ecefToGeodeticDeg` (Bowring's 1976 closed
    form), written from scratch here, so this test is a genuine independent
    cross-check rather than a round trip through the code under test."""
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


@pytest.fixture(scope="module")
def panels_check_input_path(tmp_path_factory, published_frozen_demo_scenario, run_products,
                            published_attitude_control_scenario):
    """Builds the one JSON file web/js/panels_check.mjs reads: the real published
    scenario, PLUS a real, decoded `scores` key (google.protobuf.json_format's own
    MessageToDict transcoding of the fixture's own RunProducts.scores -- the exact wire
    shape a future server change would send, see web/js/REPORT_M26_4.md), PLUS the two
    hand-computed expected values below.
    """
    scenario = dict(published_frozen_demo_scenario)
    scenario["scores"] = {
        name: json_format.MessageToDict(sr, preserving_proto_field_name=False)
        for name, sr in run_products.scores.items()
    }

    flt = next(s for s in scenario["spacecraft"] if s["name"] == "demo_flt")
    earth = next(b for b in scenario["bodies"] if b["name"] == "Earth")
    # Hand-compute sample 0's expected lon/lat: Earth's own position is (0,0,0) at every
    # sample (it is the central/origin body), so the relative position is simply
    # demo_flt's own recorded position; Earth's quaternion sample 0 applies with NO spin
    # correction at t == t[0] exactly (BodyInterp's own spin term is
    # spinRate*(t-t[0]) == 0 there) -- picked deliberately so this hand computation does
    # not have to replicate BodyInterp's slerp-between-samples logic to get an exact,
    # independently-checkable answer.
    assert earth["t"][0] == flt["t"][0], "test assumption: Earth and demo_flt share their first sample epoch"
    qx, qy, qz, qw = earth["quat"][0:4]
    ex, ey, ez = earth["pos"][0:3]
    sx, sy, sz = flt["pos"][0:3]
    rel_km = (sx - ex, sy - ey, sz - ez)
    body_fixed_km = _quat_conjugate_rotate(qx, qy, qz, qw, *rel_km)
    lon_deg, lat_deg, alt_m = _ecef_to_geodetic_iterative(*(c * 1000 for c in body_fixed_km))

    t0, t1 = scenario["t0"], scenario["t1"]
    fault = next(e for e in scenario["events"] if e["type"] == "fault")
    expected_fault_pct = (fault["t"] - t0) / (t1 - t0) * 100

    payload = {
        "scenario": scenario,
        "expectedGroundTrack": {"lonDeg": lon_deg, "latDeg": lat_deg, "altM": alt_m},
        "expectedFaultTimelinePercent": expected_fault_pct,
        # M26.4b (question 165): the real, live-server-published attitude-control
        # scenario -- see published_attitude_control_scenario's own docstring.
        "attitudeControlScenario": published_attitude_control_scenario,
    }
    d = tmp_path_factory.mktemp("panels_check")
    path = d / "panels_input.json"
    path.write_text(json.dumps(payload))
    return path


@pytest.fixture(scope="module")
def panels_data(panels_check_input_path) -> dict:
    node = _require_node()
    proc = subprocess.run([node, str(PANELS_CHECK), str(panels_check_input_path)],
                           cwd=str(PANELS_CHECK.parent), capture_output=True, text=True, timeout=30)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"panels_check.mjs did not print valid JSON (exit {proc.returncode})\n"
                              f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}")
    return data


def _failed(data: dict, substring: str) -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched substring {substring!r} -- panels_check.mjs's check names changed?"
    return matches


# ============================================================== map panel: geodesy
def test_ecef_to_geodetic_hand_derivable_special_cases(panels_data):
    """Fails against an `ecefToGeodeticDeg` that swaps lon/lat, gets the polar/
    equatorial radius roles backwards, or mishandles the polar-axis special case --
    each sub-check's expected value is derivable from the ellipsoid's own symmetry
    (never computed by calling the function under test)."""
    failed = _failed(panels_data, 'ecefToGeodeticDeg')
    assert not failed, f"hand-derivable geodetic special cases failed: {failed}"


def test_body_fixed_position_identity_wiring(panels_data):
    """Fails against a `bodyFixedPositionKm` that forgets to subtract the body's own
    position, or applies the body quaternion without inverting it (either bug is
    invisible when the quaternion is identity AND the position is zero -- this test
    instead uses identity+zero specifically to isolate a THIRD class of bug: dropped/
    swapped arguments in the plumbing itself)."""
    failed = _failed(panels_data, 'bodyFixedPositionKm')
    assert not failed, f"identity wiring check failed: {failed}"


def test_ground_track_matches_independent_hand_computation(panels_data):
    """The load-bearing check (M26.4's own required test): a real demo_flt ground-track
    sample's lon/lat, computed by web/js/ground_track.js against the REAL, ingested
    demo run, must match a value hand-computed independently in this Python file (a
    different ECEF<->geodetic algorithm -- fixed-point iteration, not Bowring's closed
    form -- and a from-scratch quaternion rotation formula, see
    `_ecef_to_geodetic_iterative`/`_quat_conjugate_rotate` above). Fails against a
    `groundTrack`/`bodyFixedPositionKm` that gets the rotation direction backwards
    (applies q instead of q^-1), forgets the km->m conversion before calling
    `ecefToGeodeticDeg`, or transposes x/y/z components -- any of these moves a real
    ~6871 km-magnitude position well outside this test's 1e-6 degree tolerance, not by
    a rounding amount.
    """
    failed = _failed(panels_data, 'groundTrack: real demo_flt sample 0')
    assert not failed, f"ground-track hand-computation cross-check failed: {failed}"
    failed2 = _failed(panels_data, 'single-point API')
    assert not failed2


def test_ground_track_one_entry_per_sample(panels_data):
    """Fails against a groundTrack() that densifies/resamples instead of using the
    trajectory's own recorded epochs (this panel's own contract, see
    web/js/ground_track.js's docstring)."""
    failed = _failed(panels_data, 'one entry per recorded')
    assert not failed


# ============================================================ map panel: tiles / no network
def test_map_tiles_use_the_offline_imagery_profile_and_no_network(panels_data):
    """Fails against a map panel that hardcodes a real tile-server URL (or any
    absolute/`://` URL) instead of `sc.imagery.urlTemplate` -- the offline fixture
    profile (M19.5, question 132) -- or against a tile-count formula that disagrees
    with the globe's own `tileCountX`/`tileCountY`."""
    failed = _failed(panels_data, 'map tiles:')
    assert not failed, f"map tile / no-network checks failed: {failed}"


def test_map_mosaic_row_order_is_north_at_top(panels_data):
    """Fails against a mosaic that renders the globe's own y=0-at-south tile addressing
    directly top-to-bottom (i.e. forgets to flip rows) -- the real, user-visible bug
    this check exists for: without the flip, the map would render upside down (south at
    the top)."""
    failed = _failed(panels_data, 'mosaicRows:')
    assert not failed, f"mosaic row-order checks failed: {failed}"


def test_lon_lat_to_percent_hand_derivable(panels_data):
    """Fails against a `lonLatToPercent` with an inverted y axis (south-at-top) or a
    non-linear/wrongly-scaled x axis -- each expected value here is derivable directly
    from the plate-carree projection's own definition (linear in both axes), not by
    calling the function under test."""
    failed = _failed(panels_data, 'lonLatToPercent:')
    assert not failed, f"lon/lat->percent checks failed: {failed}"


# ===================================================== run products panel: scores
def test_objective_rows_real_fixture_pass_state(panels_data):
    """M26.4's own required test: objectives render with their pass state. Fails
    against an implementation that (a) never distinguishes objective from measure (b)
    coerces an absent `passed` (a measure of effectiveness, ADR-005 sec 6) to `false`
    instead of `null` -- either bug would misreport `demo_flt_rmag_at_end` (a real
    PASSED objective in the fixture) or the two real measures."""
    failed = _failed(panels_data, 'objectiveRows:')
    assert not failed, f"objective/measure pass-state checks failed: {failed}"


# ============================================ run products panel: server-threaded scores (M26.4b)
def test_pointing_objective_and_its_pass_state_from_a_real_server_threaded_bundle(panels_data):
    """The M26.4b required test, verbatim: "the panel shows the demo's pointing
    objective with its pass state, from a real av-run bundle." Unlike
    `test_objective_rows_real_fixture_pass_state` above (M26.4, which proves the
    binding is correct against a Python-attached `scores` key because the server did
    not send one yet), this exercises the REAL, live-server-published scenario for the
    closed-loop attitude-control demo -- `altavista/server.py`'s own M26.4b wiring
    produced `scenario.scores` here, nothing in this test path patches it in. Fails
    against a server that never threads `RunProducts.scores` at all (`scores` would be
    `{}`, `objectiveRows` would return no rows), or a client binding that coerces the
    real `controller_seq_at_end` measure's null `passed` to `false`.
    """
    failed = _failed(panels_data, 'server-threaded scores:')
    assert not failed, f"server-threaded scores checks failed: {failed}"


# ===================================================== run products panel: timeline events
def test_port_command_and_fault_events_linked_to_correct_timeline_epoch(panels_data):
    """M26.4's own required test, verbatim: "a port command / fault event links to the
    correct timeline epoch." Fails against an implementation that also includes
    lifecycle/maneuver events (not what this panel links), drops/mutates an event's `t`
    on the way through `timelineEvents()`, or computes the wrong timeline percentage
    for it (`eventTimelinePercent`'s check compares against `(ev.t - t0) / (t1 - t0) *
    100` computed independently in this file from the real fixture's own t0/t1/ev.t,
    not by calling the function under test first)."""
    failed = _failed(panels_data, 'timelineEvents:')
    assert not failed, f"timeline-linked event filtering checks failed: {failed}"
    failed2 = _failed(panels_data, 'eventTimelinePercent:')
    assert not failed2, f"timeline percent hand-computation cross-check failed: {failed2}"


# ===================================================== console panel: provenance
def test_console_panel_shows_real_provenance_and_config_hash(panels_data):
    """M26.4's own required test: provenance and the config hash appear. Fails against
    a `provenanceLines` that drops `meta.configHash`/`meta.runId` or renders a
    hardcoded/placeholder value instead of the real fixture's own hash."""
    failed = _failed(panels_data, 'provenanceLines:')
    assert not failed, f"provenance/config-hash checks failed: {failed}"


# ================================================================ layout: attachM264Panels
def test_m264_panels_attach_without_regressing_the_m26_3_default_layout_tests(panels_data):
    """Fails against a change that (wrongly) adds the 3 new panels INSIDE
    `buildBaseSidebarViewportLayout`/`buildRpoTripleViewportLayout`/
    `defaultLayoutForScenario` themselves -- exactly the mistake that would regress
    tests/test_viewer_viewport.py's `test_ordinary_scenario_keeps_pre_m26_3_default`
    (asserts exactly 2 leaves) and `test_default_layout_is_icrf_beside_ric_beside_globe`
    (asserts exactly 4 leaves), both of which this task's own brief says must stay
    green. Also fails against an `attachM264Panels` that drops/renames an
    already-present leaf instead of purely adding the 3 new ones alongside it."""
    failed = _failed(panels_data, 'attachM264Panels:')
    assert not failed, f"attachM264Panels layout checks failed: {failed}"


# ------------------------------------------------------------------------------ overall
def test_panels_check_report(panels_data, capsys):
    """Prints the full named-check table (not a correctness assertion on its own --
    the individual test functions above are) so a CI log / `pytest -q -s` carries the
    real pass/fail detail, per this task's incremental-reporting requirement."""
    with capsys.disabled():
        print(f"\npanels_check.mjs: {len(panels_data['checks'])} checks, allPass={panels_data['allPass']}")
        for c in panels_data["checks"]:
            mark = "PASS" if c["pass"] else "FAIL"
            print(f"  [{mark}] {c['name']}")
    assert panels_data["allPass"] is True, "panels_check.mjs reported at least one failing check -- see the printed table above (-s)"

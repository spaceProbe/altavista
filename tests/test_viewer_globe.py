"""M15.4 viewer precision/LOD tests: the quadtree WGS84 globe (web/js/globe_lod.js's
tile-selection/load-scheduling arithmetic) and its compatibility with the per-frame
floating origin (web/js/origin.js, tested in tests/test_viewer_jitter.py -- NOT this
task's file, only extended additively via web/js/scene_jitter_harness.mjs, which *is*
this task's file).

Same "run the real code, don't port it" discipline as tests/test_viewer_jitter.py: this
file shells out to `node` to run two CLI harnesses committed alongside the modules --
``web/js/globe_lod_check.mjs`` (tile selection/scheduling) and
``web/js/scene_jitter_harness.mjs`` (extended with an additive ``RPO_with_globe`` key,
see that file) -- and asserts on the JSON each prints to stdout. Nothing here
reimplements globe_lod.js's ellipsoid geometry, screen-space-error, or scheduler
arithmetic in Python.

What each test would catch (this task's standing review requirement -- "for each test,
be able to name the wrong implementation it would fail against"):

* ``test_tile_selection_is_deterministic_across_process_runs``: an implementation that
  returns tiles in an order sensitive to Map/Set insertion accidents (or omits the
  final canonical sort `selectTiles()` documents) instead of the always-sorted output
  this task's binding rule requires ("Determinism on the tile-selection path: sorted
  iteration, no dependence on Map/Set insertion accidents").
* ``test_tile_budget_is_respected``: a load scheduler with no eviction logic --
  `globe_lod_check.mjs`'s fixed camera path visits far more distinct tiles (over its
  whole run) than the configured resident budget, and the budget is deliberately sized
  *between* a single step's own selection size and that running total, so eviction
  must actually run for the budget to hold (see that harness's module docstring).
* ``test_tile_loading_is_cancelled_on_camera_move``: a scheduler that queues every
  requested load and never cancels a stale one once its tile falls out of the current
  selection -- the fixed camera path includes a deliberate large jump to the opposite
  side of the globe before the previous step's loads have all "completed" (simulated
  slow completion, see the harness), so a real cancellation must happen.
* ``test_lod_refines_with_camera_distance``: a selector that ignores camera distance
  entirely (e.g. a fixed traversal depth, or no screen-space-error computation at all)
  -- the GEO-altitude step must select coarser (lower max level) tiles than the
  LEO-altitude close-up step for the same screen/FOV settings.
* ``test_rpo_centimetre_precision_holds_with_globe_present`` /
  ``test_globe_present_matches_no_globe_baseline_exactly`` /
  ``test_globe_tile_vertices_stay_body_local_scale``: see
  ``web/js/scene_jitter_harness.mjs``'s ``measureRpoWithGlobePresent()`` docstring --
  in short, a bug that let globe tile construction leak into the RPO measurement's own
  frame id (a frame-id typo, shared mutable scratch state) would change the "with
  globe" RPO number away from the pristine baseline even if it stayed under the
  centimetre bound, and a bug that built tile vertices from a body's *absolute*
  (origin-frame) position instead of body-local ellipsoid coordinates would blow the
  small-magnitude (~Earth-radius-in-scene-units) expectation the floating-origin
  compatibility argument rests on.

M16.4 additions below (``web/js/tiles3d_check.mjs`` / ``web/js/tiles3d_geo_check.mjs``,
both new files under this task's ``*_check.mjs`` ownership): close M15.4's two
disclosed shortcuts for the 3D Tiles overlay (web/js/tiles_layer.js) -- "a fixed demo
transform, not real geo-referencing" and "streaming-layer budget/cancellation ... not
yet reused for the 3D Tiles overlay". What each new test would catch:

* ``test_tiles3d_selection_is_deterministic_across_process_runs``: same class of bug
  as the globe's own determinism test, for ``selectTiles3D`` (web/js/tiles_layer.js)
  instead of ``selectTiles`` -- an implementation that drops the final
  ``compareTileIds3D`` sort, or otherwise lets Map/Set iteration order leak into the
  result.
* ``test_tiles3d_budget_is_respected`` / ``test_tiles3d_loading_is_cancelled_on_camera_move``:
  the *same* ``TileLoadScheduler`` class the globe uses (globe_lod.js), reused for the
  3D Tiles overlay's own tile ids via its generalized ``keyFn`` parameter -- a
  scheduler with no eviction, or one that never cancels a stale pending load on a
  large camera jump, fails these exactly as it would for the globe.
* ``test_tiles3d_lod_refines_with_camera_distance``: a selector that ignores camera
  distance or a tile's own *authored* ``geometricError`` (read straight from
  tileset.json, unlike the globe's derived-from-level number).
* ``test_tiles3d_root_transform_is_real_geo_reference`` /
  ``test_tiles3d_up_basis_matches_wgs84_ellipsoid_normal``: the fixture's
  ``root.transform`` must be a genuine WGS84 East-North-Up frame anchored at the
  geodetic point ``tileset.extras.geoReference`` declares -- would fail against M15.4's
  original fixture (no ``root.transform`` at all) or against any transform not
  actually derived from real geodetic placement (e.g. identity, or numbers unrelated
  to the declared anchor).
* ``test_tiles3d_overlay_moves_with_body_fixed_frame``: the overlay group, placed via
  ``placeOverlayInBodyFixedFrame`` and reparented into a *rotating* body-fixed frame
  node, must move in world space exactly as that rotation predicts. Fails outright (not
  "by some amount") against M15.4's original code, which parented the overlay under
  the *entities* (non-rotating) frame at a small fixed offset unrelated to geography --
  see ``web/js/tiles3d_geo_check.mjs``'s module docstring, which also records this
  exact old-vs-new behavioural difference as measured directly against a
  hand-built stand-in for the old code.

M19.5 additions below (``web/js/globe_imagery_check.mjs``, this task's own harness,
following the exact same "real node process, assert on its JSON" pattern as every
harness above -- docs/open-questions.md question 132, decided by the lead: "the imagery
source is a profile setting ... an XYZ/WMTS URL template and an attribution string; no
network in tests"):

* ``test_globe_imagery_configured_template_is_used_verbatim_with_no_network_call``: the
  headline test. Fails against an implementation that silently falls back to
  ``DEFAULT_IMAGERY_URL`` instead of the profile-configured template (the actual
  regression this task exists to fix), or one that re-encodes/re-prefixes the
  substituted URL -- see ``web/js/globe_imagery_check.mjs``'s module docstring for
  exactly how its configured template is chosen to make either bug detectable. "No
  network call" is structural, not observed: the harness's stub loader is incapable of
  I/O, so a URL only ever appears in ``recordedUrls`` if it was routed through that stub.
* ``test_globe_imagery_default_profile_yields_offline_fixture``: an implementation
  whose default (``imageryUrl`` omitted, what a call site with no profile wiring at all
  would do) no longer resolves to the offline fixture template every ``profiles/*.yaml``
  declares as its own default (cross-checked against those files directly in
  ``tests/test_profiles.py``).
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
GLOBE_LOD_CHECK = REPO_ROOT / "web" / "js" / "globe_lod_check.mjs"
SCENE_JITTER_HARNESS = REPO_ROOT / "web" / "js" / "scene_jitter_harness.mjs"
TILES3D_CHECK = REPO_ROOT / "web" / "js" / "tiles3d_check.mjs"
TILES3D_GEO_CHECK = REPO_ROOT / "web" / "js" / "tiles3d_geo_check.mjs"
GLOBE_IMAGERY_CHECK = REPO_ROOT / "web" / "js" / "globe_imagery_check.mjs"

# Never loosened -- the same bound tests/test_viewer_jitter.py uses for the RPO scene
# (docs/open-questions.md Q46's "centimetre stability in RIC").
CENTIMETRE_BOUND_M = 0.01

# Earth's WGS84 equatorial radius in scene units (web/js/scene.js's SCALE = 1e-3 scene
# units/km * 6378.137 km) -- what "body-local, not absolute-frame-relative" tile
# vertices should stay near regardless of camera distance from Earth's centre. Wide
# margin (10x) around the real value (~6.378) -- this bound exists to catch "vertices
# built from an absolute, origin-frame-relative position" (which would be orders of
# magnitude larger at LEO/Mars/RPO scale, not merely 2-3x), not to pin the exact tile
# geometry to a specific segment count or tile-corner extent.
GLOBE_VERTEX_MAX_MAGNITUDE_BOUND_SCENE_UNITS = 65.0

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; the viewer's globe modules are "
            "ES modules and this test intentionally runs them for real (see module "
            "docstring) rather than porting the arithmetic to Python, so it cannot "
            "proceed without node. Install node to run it."
        )
    return NODE


def _run_node_json(script: Path) -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(script)],
        cwd=str(script.parent),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert proc.returncode == 0, (
        f"node {script.name} exited {proc.returncode}\n"
        f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{script.name} did not print valid JSON: {proc.stdout!r}\nstderr: {proc.stderr}")


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def globe_lod_data() -> dict:
    return _run_node_json(GLOBE_LOD_CHECK)


@pytest.fixture(scope="module")
def globe_lod_run_1_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(GLOBE_LOD_CHECK)], cwd=str(GLOBE_LOD_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0
    return proc.stdout


@pytest.fixture(scope="module")
def globe_lod_run_2_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(GLOBE_LOD_CHECK)], cwd=str(GLOBE_LOD_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0
    return proc.stdout


@pytest.fixture(scope="module")
def scene_jitter_data() -> dict:
    return _run_node_json(SCENE_JITTER_HARNESS)


@pytest.fixture(scope="module")
def tiles3d_data() -> dict:
    return _run_node_json(TILES3D_CHECK)


@pytest.fixture(scope="module")
def tiles3d_run_1_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(TILES3D_CHECK)], cwd=str(TILES3D_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0
    return proc.stdout


@pytest.fixture(scope="module")
def tiles3d_run_2_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(TILES3D_CHECK)], cwd=str(TILES3D_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0
    return proc.stdout


@pytest.fixture(scope="module")
def tiles3d_geo_data() -> dict:
    return _run_node_json(TILES3D_GEO_CHECK)


@pytest.fixture(scope="module")
def globe_imagery_data() -> dict:
    return _run_node_json(GLOBE_IMAGERY_CHECK)


# ------------------------------------------------------------------- tile selection
def test_tile_selection_is_deterministic_across_process_runs(globe_lod_run_1_raw, globe_lod_run_2_raw):
    """Two independent `node` process invocations of globe_lod_check.mjs, over the same
    fixed camera path, must print byte-identical JSON -- "same tile set, same order",
    this task's required test. See this module's docstring for what a failure here
    would mean.
    """
    assert globe_lod_run_1_raw == globe_lod_run_2_raw, (
        "globe_lod_check.mjs produced different output across two independent process "
        "runs over the same fixed camera path -- tile selection is not deterministic"
    )
    # Parse once (both runs are identical) to also sanity-check the shape/steps count.
    d = json.loads(globe_lod_run_1_raw)
    assert len(d["steps"]) == 8
    for step in d["steps"]:
        assert step["tileKeys"] == sorted(step["tileKeys"], key=lambda k: tuple(int(p) for p in k.split("/"))), (
            f"step's tileKeys are not canonically sorted: {step['tileKeys']}"
        )


def test_tile_budget_is_respected(globe_lod_data):
    assert globe_lod_data["budgetRespected"] is True, (
        f"resident tile count exceeded the budget: max observed "
        f"{globe_lod_data['maxResidentObserved']} > budget {globe_lod_data['residentBudget']}"
    )
    assert globe_lod_data["maxResidentObserved"] <= globe_lod_data["residentBudget"]
    # The budget is only a meaningful test if eviction actually had to run -- see this
    # module's docstring ("deliberately sized between a single step's own selection
    # size and the running total").
    assert globe_lod_data["evictedCount"] > 0, (
        "expected the load scheduler to have evicted at least one resident tile over "
        "this camera path (the path touches more distinct tiles than the budget); "
        "evictedCount == 0 would mean this test never actually exercised eviction"
    )


def test_tile_loading_is_cancelled_on_camera_move(globe_lod_data):
    assert globe_lod_data["cancelledCount"] > 0, (
        "expected the deliberate large camera jump (step 4 -> step 5 in "
        "globe_lod_check.mjs's CAMERA_PATH) to cancel at least one still-pending tile "
        "load from the previous view; cancelledCount == 0 would mean stale loads are "
        "never cancelled on camera move"
    )


def test_lod_refines_with_camera_distance(globe_lod_data):
    """docs/architecture.md's Globe bullet: LOD selection driven by camera distance
    *and* screen-space error -- this is the "camera distance" half, made concrete: the
    GEO-altitude step (step 0) must select strictly coarser tiles than the LEO
    close-up step (step 3), for the same screen/FOV settings.
    """
    assert globe_lod_data["maxLevelFar"] < globe_lod_data["maxLevelNear"], (
        f"expected the GEO-altitude step's max tile level ({globe_lod_data['maxLevelFar']}) "
        f"to be strictly less than the LEO close-up step's ({globe_lod_data['maxLevelNear']}) "
        f"-- LOD does not appear to respond to camera distance"
    )
    assert globe_lod_data["maxLevelFar"] == 0, "the GEO-altitude step should select only the two root tiles"


def test_globe_lod_report(globe_lod_data, capsys):
    """Not a correctness assertion -- prints the measured tile-selection/scheduler
    numbers so `pytest -q -s` (or any CI log) carries the real values.
    """
    with capsys.disabled():
        print("\nglobe LOD tile selection (web/js/globe_lod_check.mjs):")
        print(f"  residentBudget={globe_lod_data['residentBudget']} "
              f"maxResidentObserved={globe_lod_data['maxResidentObserved']} "
              f"budgetRespected={globe_lod_data['budgetRespected']}")
        print(f"  cancelledCount={globe_lod_data['cancelledCount']} evictedCount={globe_lod_data['evictedCount']}")
        print(f"  maxLevelFar(GEO)={globe_lod_data['maxLevelFar']} maxLevelNear(LEO)={globe_lod_data['maxLevelNear']}")
        for i, step in enumerate(globe_lod_data["steps"]):
            print(f"  step {i}: alt={step['camera']['altM']:>10.0f} m  "
                  f"tiles={len(step['tileKeys']):>3}  maxLevel={step['maxLevelSelected']}  "
                  f"resident={step['residentSize']:>3}  pending={step['pendingSize']:>3}")


# ------------------------------------------------------- floating-origin compatibility
def test_rpo_centimetre_precision_holds_with_globe_present(scene_jitter_data):
    """This task's hard constraint, proved: "a target-centred RPO view must keep
    centimetre precision while the globe is visible." Never loosen
    CENTIMETRE_BOUND_M to make this pass (same rule as
    tests/test_viewer_jitter.py's RPO bound).
    """
    d = scene_jitter_data["RPO_with_globe"]
    assert d["errWithM"] < CENTIMETRE_BOUND_M, (
        f"RPO separation error with the globe present {d['errWithM']:.6g} m exceeds "
        f"the centimetre bound"
    )


def test_globe_present_matches_no_globe_baseline_exactly(scene_jitter_data):
    """The stronger claim than "still under the bound": the RPO measurement, run
    through a FloatingOrigin instance that also rendered real globe tile vertices
    (under a different frame id), must be bit-for-bit identical to a pristine
    baseline. See web/js/scene_jitter_harness.mjs's measureRpoWithGlobePresent()
    docstring for exactly what a mismatch here would mean (frame-id cross-
    contamination, aliased scratch state, etc.).
    """
    d = scene_jitter_data["RPO_with_globe"]
    assert d["matchesBaselineExactly"] is True, (
        "the RPO measurement changed when a real globe tile build (under a separate "
        "'earth-body' frame id) shared the same FloatingOrigin instance -- this "
        "should be impossible if the globe and the RPO frame are genuinely isolated"
    )
    assert d["tileCount"] > 0, "expected the LEO-altitude camera to select at least one globe tile"


def test_globe_tile_vertices_stay_body_local_scale(scene_jitter_data):
    """The mechanism behind the compatibility claim (globe.js's module docstring):
    tile vertices are body-local WGS84-ellipsoid coordinates, not the body's absolute
    origin-frame position, so they stay at Earth-radius scale (~6.378 scene units)
    regardless of how far the camera/RPO scene is from the current floating-origin
    origin -- this is *why* they never need floating-origin treatment. See this
    module's docstring for what building vertices from an absolute position instead
    would do to this number.
    """
    d = scene_jitter_data["RPO_with_globe"]
    mag = d["globeVertexMaxMagnitudeSceneUnits"]
    assert mag < GLOBE_VERTEX_MAX_MAGNITUDE_BOUND_SCENE_UNITS, (
        f"globe tile vertex magnitude {mag:.6g} scene units exceeds the body-local-scale "
        f"bound ({GLOBE_VERTEX_MAX_MAGNITUDE_BOUND_SCENE_UNITS}) -- tile vertices may "
        f"have been built from an absolute (origin-frame-relative) position instead of "
        f"body-local WGS84 ellipsoid coordinates"
    )


def test_globe_precision_report(scene_jitter_data, capsys):
    """Not a correctness assertion -- prints the real measured numbers, per this
    task's honesty requirements ("no unrecorded approximations... report the real
    numbers").
    """
    d = scene_jitter_data["RPO_with_globe"]
    baseline = scene_jitter_data["RPO"]
    with capsys.disabled():
        print("\nRPO precision with the globe present (web/js/scene_jitter_harness.mjs):")
        print(f"  errWithM (globe present)  = {d['errWithM']:.6e} m  (bound {CENTIMETRE_BOUND_M} m)")
        print(f"  errWithM (no-globe baseline) = {baseline['errWithM']:.6e} m")
        print(f"  matchesBaselineExactly = {d['matchesBaselineExactly']}")
        print(f"  globe tiles selected at RPO/LEO camera distance = {d['tileCount']}")
        print(f"  globe tile vertex max magnitude = {d['globeVertexMaxMagnitudeSceneUnits']:.6f} scene units "
              f"(bound {GLOBE_VERTEX_MAX_MAGNITUDE_BOUND_SCENE_UNITS})")


# ==================================================================================
# M16.4: 3D Tiles overlay -- real geo-referencing + reused budget/cancellation
# ==================================================================================

def test_tiles3d_selection_is_deterministic_across_process_runs(tiles3d_run_1_raw, tiles3d_run_2_raw):
    """Two independent `node` process invocations of tiles3d_check.mjs, over the same
    fixed camera path, must print byte-identical JSON. See this module's docstring for
    what a failure here would mean.
    """
    assert tiles3d_run_1_raw == tiles3d_run_2_raw, (
        "tiles3d_check.mjs produced different output across two independent process "
        "runs over the same fixed camera path -- 3D Tiles overlay tile selection is "
        "not deterministic"
    )
    d = json.loads(tiles3d_run_1_raw)
    assert len(d["steps"]) == 8
    for step in d["steps"]:
        assert step["tileIds"] == sorted(step["tileIds"], key=lambda tid: tuple(int(p) for p in tid.split("."))), (
            f"step's tileIds are not canonically sorted: {step['tileIds']}"
        )


def test_tiles3d_budget_is_respected(tiles3d_data):
    assert tiles3d_data["budgetRespected"] is True, (
        f"resident tile count exceeded the budget: max observed "
        f"{tiles3d_data['maxResidentObserved']} > budget {tiles3d_data['residentBudget']}"
    )
    assert tiles3d_data["maxResidentObserved"] <= tiles3d_data["residentBudget"]
    # Only a meaningful test if eviction actually had to run -- residentBudget is
    # deliberately below both a single "close" step's own selection size and the
    # path's running total of distinct ids (see tiles3d_check.mjs's module docstring).
    assert tiles3d_data["evictedCount"] > 0, (
        "expected the load scheduler to have evicted at least one resident tile over "
        "this camera path; evictedCount == 0 would mean this test never actually "
        "exercised eviction"
    )


def test_tiles3d_loading_is_cancelled_on_camera_move(tiles3d_data):
    assert tiles3d_data["cancelledCount"] > 0, (
        "expected the deliberate jump from one corner of the fixture's footprint to "
        "the opposite corner to cancel at least one still-pending tile load from the "
        "previous view; cancelledCount == 0 would mean stale loads are never "
        "cancelled on camera move"
    )


def test_tiles3d_lod_refines_with_camera_distance(tiles3d_data):
    """The 3D-Tiles-overlay equivalent of test_lod_refines_with_camera_distance
    above: the far step must select only the tileset's root; the close step must
    refine all the way to the fixture's deepest authored level.
    """
    assert tiles3d_data["maxLevelFar"] < tiles3d_data["maxLevelNear"], (
        f"expected the far step's max tile level ({tiles3d_data['maxLevelFar']}) to be "
        f"strictly less than the close step's ({tiles3d_data['maxLevelNear']}) -- LOD "
        f"does not appear to respond to camera distance or to the tile's own authored "
        f"geometricError"
    )
    assert tiles3d_data["maxLevelFar"] == 0, "the far step should select only the tileset root"


def test_tiles3d_report(tiles3d_data, capsys):
    """Not a correctness assertion -- prints the measured tile-selection/scheduler
    numbers so `pytest -q -s` (or any CI log) carries the real values.
    """
    with capsys.disabled():
        print("\n3D Tiles overlay LOD tile selection (web/js/tiles3d_check.mjs):")
        print(f"  residentBudget={tiles3d_data['residentBudget']} "
              f"maxResidentObserved={tiles3d_data['maxResidentObserved']} "
              f"budgetRespected={tiles3d_data['budgetRespected']}")
        print(f"  cancelledCount={tiles3d_data['cancelledCount']} evictedCount={tiles3d_data['evictedCount']}")
        print(f"  maxLevelFar={tiles3d_data['maxLevelFar']} maxLevelNear={tiles3d_data['maxLevelNear']} "
              f"tilesetTileCount={tiles3d_data['tilesetTileCount']}")
        for step in tiles3d_data["steps"]:
            print(f"  {step['camera']['label']:>12}: count={len(step['tileIds']):>3}  "
                  f"maxLevel={step['maxLevelSelected']}  resident={step['residentSize']:>3}  "
                  f"pending={step['pendingSize']:>3}")


def test_tiles3d_root_transform_is_real_geo_reference(tiles3d_geo_data):
    """The fixture's root.transform must be genuinely derived from the geodetic point
    it claims (tileset.extras.geoReference), not an arbitrary or absent matrix -- see
    web/js/tiles3d_geo_check.mjs's module docstring for exactly what this would catch
    (M15.4's original fixture had no root.transform at all).
    """
    d = tiles3d_geo_data
    names = {c["name"]: c["pass"] for c in d["checks"]}
    assert names["fixture declares extras.geoReference"] is True
    assert names["fixture has a root.transform at all (M15.4 had none)"] is True
    assert names["root.transform translation column == geodeticToEcef(declared lon/lat/height) to sub-millimetre"] is True
    assert names["root.transform translation magnitude is Earth-surface scale (not a small demo offset)"] is True
    assert d["translationErrM"] < 1e-6, f"translation vs. declared geodetic metadata differs by {d['translationErrM']} m"


def test_tiles3d_up_basis_matches_wgs84_ellipsoid_normal(tiles3d_geo_data):
    """The root.transform's rotation must be a genuine orthonormal East-North-Up frame
    whose "up" is the true WGS84 ellipsoid normal at the anchor point -- not merely
    *some* rotation, and not the sphere-normal approximation (position direction) that
    would also happen to look plausible near the equator but is measurably wrong at a
    real latitude (this fixture's anchor is ~40 deg N).
    """
    names = {c["name"]: c["pass"] for c in tiles3d_geo_data["checks"]}
    assert names["East/North/Up basis is unit length"] is True
    assert names["East/North/Up basis is orthogonal"] is True
    assert names["Up basis vector matches the true WGS84 ellipsoid normal (not merely the sphere/position direction)"] is True


def test_tiles3d_overlay_moves_with_body_fixed_frame(tiles3d_geo_data):
    """The real fix for M15.4's "fixed demo transform" shortcut: an overlay group
    placed via placeOverlayInBodyFixedFrame and reparented into a *rotating*
    body-fixed frame node must move in world space exactly as that rotation predicts.
    Fails outright against the old (entities-frame, fixed-offset) implementation --
    see web/js/tiles3d_geo_check.mjs's module docstring, which records the old
    behaviour measured directly (rotating the body-fixed frame moves the overlay not
    at all under the old code).
    """
    names = {c["name"]: c["pass"] for c in tiles3d_geo_data["checks"]}
    assert names["group reparented under the body-fixed frame node"] is True
    assert names["group has identity local position (no re-applied demo offset)"] is True
    assert names["group scale is SCENE_UNITS_PER_METRE (metres -> scene units, same constant globe.js uses)"] is True
    assert names["unrotated body-fixed frame: overlay world position == geo-referenced ECEF point (scene units)"] is True
    assert names["rotating the body-fixed frame rotates the overlay world position exactly as predicted"] is True
    assert names["rotation actually changed the overlay world position (not a no-op)"] is True
    assert names["root frame itself was never perturbed"] is True
    assert tiles3d_geo_data["allPass"] is True


def test_tiles3d_geo_report(tiles3d_geo_data, capsys):
    """Not a correctness assertion -- prints the real measured numbers."""
    d = tiles3d_geo_data
    with capsys.disabled():
        print("\n3D Tiles overlay geo-reference (web/js/tiles3d_geo_check.mjs):")
        print(f"  ecefFromTransform = {d['ecefFromTransform']}")
        print(f"  ecefFromMetadata  = {d['ecefFromMetadata']}")
        print(f"  translationErrM   = {d['translationErrM']:.3e} m")
        print(f"  geoReference      = {d['geoReference']}")
        print(f"  allPass = {d['allPass']} ({len(d['checks'])} checks)")


# ==================================================================================
# M19.5: the globe's imagery source is a profile setting (question 132)
# ==================================================================================

def test_globe_imagery_configured_template_is_used_verbatim_with_no_network_call(globe_imagery_data):
    """See this module's docstring. A reviewer can see this would fail against a
    GlobeLayer that silently used ``DEFAULT_IMAGERY_URL`` instead of the configured
    template: ``configuredRecordedUrls`` would then contain ``'./fixtures/tiles/...'``
    paths instead of ``https://tiles.example.test/v3/...`` ones, and would not match
    ``configuredExpectedUrls`` (built by web/js/globe_imagery_check.mjs's own,
    independently-written substitution, not by importing globe.js's ``urlForTile``).
    """
    d = globe_imagery_data
    assert d["configuredTileCount"] > 0, "expected the LEO-altitude camera to select at least one tile"
    assert d["configuredRecordedUrls"] == d["configuredExpectedUrls"], (
        "recorded tile-load URLs do not match the configured template substituted "
        "verbatim -- the URL was re-encoded, re-prefixed, or not built from the "
        "configured template at all"
    )
    assert all("./fixtures/tiles/" not in u for u in d["configuredRecordedUrls"]), (
        "a configured imagery template must never silently fall back to the default "
        "offline-fixture template"
    )
    assert d["verbatimTemplateUsed"] is True
    assert d["noRealNetworkIoAttempted"] is True


def test_globe_imagery_default_profile_yields_offline_fixture(globe_imagery_data):
    """GlobeLayer's own default (no ``imageryUrl`` opt at all) must still be the offline
    fixture template every ``profiles/*.yaml`` declares (cross-checked directly against
    those files in ``tests/test_profiles.py``'s
    ``test_every_profiles_default_imagery_matches_globe_js``). Fails if globe.js's
    ``DEFAULT_IMAGERY_URL`` ever drifts from that declared default.
    """
    d = globe_imagery_data
    assert d["defaultTileCount"] > 0
    assert d["defaultImageryUrlUsed"] == "./fixtures/tiles/{z}/{x}/{y}.png"
    assert all(u.startswith("./fixtures/tiles/") and u.endswith(".png") for u in d["defaultRecordedUrls"]), (
        f"default-template recorded URLs are not all under the offline fixture path: "
        f"{d['defaultRecordedUrls']}"
    )
    assert d["defaultProfileUsesTheOfflineFixture"] is True


def test_globe_imagery_report(globe_imagery_data, capsys):
    """Not a correctness assertion -- prints the real measured URLs."""
    d = globe_imagery_data
    with capsys.disabled():
        print("\nglobe imagery source (web/js/globe_imagery_check.mjs):")
        print(f"  configured template: {d['configuredTemplate']}")
        print(f"  configured tiles requested: {d['configuredTileCount']}")
        print(f"  first configured URL: {d['configuredRecordedUrls'][0]}")
        print(f"  default imageryUrl used: {d['defaultImageryUrlUsed']}")
        print(f"  default tiles requested: {d['defaultTileCount']}")
        print(f"  verbatimTemplateUsed={d['verbatimTemplateUsed']} "
              f"defaultProfileUsesTheOfflineFixture={d['defaultProfileUsesTheOfflineFixture']}")

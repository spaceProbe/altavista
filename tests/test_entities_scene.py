"""Heavy round 7 (question 233), task 1's own proof item 2.

Runs `web/js/entities_scene_check.mjs` under node: the real, shipped `LayerManager`
(web/js/layers/layer.js) plus the real H6 entity adapters (web/js/entities/) plus a
real THREE scene graph -- the plain-`node`-reachable half of the H6 wiring proof (a
real `web/js/scene.js` `Viewer` needs a real WebGL context, which plain `node` does not
have). See `tests/test_viewer_entities_browser.py` for the real-browser half that
drives the actual `Viewer`/`app.js`.

Same discipline as `tests/test_entities_layer.py`: no budget/ellipsoid arithmetic is
re-implemented in Python; the JSON on stdout names every check, and this file reports
the failing check NAMES rather than trusting a bare exit code (question 148: "an exit
code is not evidence").
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_scene_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/entities/ and "
            "web/js/layers/ are ES modules, run for real under node."
        )
    return NODE


@pytest.fixture(scope="module")
def data() -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=30,
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode}): "
            f"{proc.stdout!r}\nstderr: {proc.stderr}"
        )


def test_all_entities_scene_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"entities scene checks failed: {failed}"
    assert data["allPass"] is True


def test_check_names_are_distinct(data):
    """Round 6 defect 6 (this round's own COMMON brief): two checks sharing one name
    make a failure ambiguous. Assert distinct names count == check count directly,
    not merely that the meta-check inside the harness itself says so."""
    names = [c["name"] for c in data["checks"]]
    assert len(names) == len(set(names)), f"duplicate check names: {names}"


def test_kind_separation_is_real_not_accidental(data):
    """LayerManager.imageryLayers() (round 6, 16efe57) must never include an entity
    marker/trail adapter under its default kind, and the same manager mechanism must
    genuinely include one that is misconfigured with kind: 'imagery' -- proving the
    exclusion is load-bearing, not a coincidence of the field never being read."""
    by_name = {c["name"]: c for c in data["checks"]}
    assert by_name["kind_markerDefaultNotImagery"]["pass"] is True
    assert by_name["kind_trailDefaultNotImagery"]["pass"] is True
    assert by_name["kind_managerImageryLayersExcludesEntityAdaptersByDefault"]["pass"] is True
    assert by_name["kind_forcedImageryKindDemonstratesTheHazard"]["pass"] is True


def test_one_shared_budget(data):
    by_name = {c["name"]: c for c in data["checks"]}
    assert by_name["budget_entityMarkersAdmittedOnSharedManager"]["pass"] is True
    assert by_name["budget_entityTrailsAdmittedOnSharedManager"]["pass"] is True
    assert by_name["budget_oneManagerOneResidentByteTotal"]["pass"] is True


def test_scene_graph_provenance(data):
    by_name = {c["name"]: c for c in data["checks"]}
    assert by_name["sceneGraph_markerInstancedMeshFoundWithProvenance"]["pass"] is True
    assert by_name["sceneGraph_trailLinesFoundWithProvenance"]["pass"] is True


def test_ellipsoid_and_keepout_world_semi_axes(data):
    by_name = {c["name"]: c for c in data["checks"]}
    assert by_name["ellipsoid_semiAxesMatchClosedForm"]["pass"] is True
    assert by_name["ellipsoid_worldSemiAxesKmMatchesEllipsoidSemiAxesKm"]["pass"] is True
    assert by_name["keepout_worldSemiAxesKmEqualsEllipsoidPlusMargin"]["pass"] is True


def test_entities_scene_report(data, capsys):
    with capsys.disabled():
        print("\nentities scene check (web/js/entities_scene_check.mjs):")
        print(f"  allPass={data['allPass']} checks={len(data['checks'])}")

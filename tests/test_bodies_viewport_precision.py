"""Question 254 item 4 (heavy open-items round): the bodies, the tiled globe and the body-fixed
frame are placed against EACH viewport's own floating origin, as the shared entity objects are
since question 252.

Same "run the real code, don't port it" discipline as tests/test_entities_viewport_precision.py:
this shells out to `node` to run web/js/bodies_viewport_precision_check.mjs, which binds the real
`Viewer.prototype` of web/js/scene.js (`setScenario`, `addViewport`, `update(t)`, `pick`,
`enableGlobe`, `_placeBodies`, ...) onto a minimal stand-in (WebGL, the DOM and OrbitControls are
the only stand-ins) and prints one JSON object of named checks (exit 1 when any fails, `FAIL`
lines and a `N passed, M failed` summary on stderr). Nothing here reimplements the geometry.

What it measures (see the .mjs module docstring), at LEO 7,000 km, GEO 42,164 km and a low lunar
orbit (1,800 km from the Moon's centre), for the primary after `fit()` and framed on the Chaser,
with the tiled globe off and on, in three views (the primary and two extra viewports, the first
framed ~10 m from the 1.5 m model): the final float32 eye-space vertex positions of the nearest
body's mesh (centre and the vertices nearest the camera), a small near body, a sub-pixel dot, the
globe's tile vertices and probe meshes hung in the body-fixed frame, against the float64 truth,
split into what the placement got wrong and the GPU's float32 floor. The real-browser half is
tests/test_viewer_bodies_viewport_precision_browser.py.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "bodies_viewport_precision_check.mjs"
BEFORE = REPO_ROOT / "web" / "js" / "fixtures" / "bodies_viewport_precision_before_primary.json"
NODE = shutil.which("node")

SCENES = ("LEO", "GEO", "lunar")
MODES = ("fit", "framed")
CONFIGS = ("plain", "globe")
SUMMARY = "bodies_viewport_precision_check: "


def _run(env_extra: dict | None = None) -> tuple[int, dict, str]:
    if NODE is None:
        pytest.skip("node is not installed; the check runs web/js/scene.js for real")
    env = dict(os.environ)
    env.update(env_extra or {})
    proc = subprocess.run([NODE, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True,
                          timeout=600, env=env)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout[:2000]!r}\nstderr: {proc.stderr[:4000]}")
    return proc.returncode, data, proc.stderr


@pytest.fixture(scope="module")
def result() -> tuple[int, dict, str]:
    return _run()


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched {substring!r} -- did the check names in {CHECK.name} change?"
    return matches


def test_the_check_exits_zero_prints_its_summary_and_every_check_passes(result):
    rc, data, stderr = result
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert failed == [], failed[:10]
    assert data["allPass"] is True and rc == 0
    last = stderr.strip().splitlines()[-1]
    assert last.startswith(SUMMARY) and f"{len(data['checks'])} passed, 0 failed" in last, last
    assert len(data["checks"]) >= 700, len(data["checks"])


def test_a_failing_check_exits_nonzero_with_fail_lines():
    """The placement bound tightened below float64's own rounding at Mars's distance must fail checks,
    print FAIL lines and the summary, and exit 1."""
    rc, data, stderr = _run({"AV_PRECISION_BOUND_M": "1e-12"})
    assert data["allPass"] is False and any(not c["pass"] for c in data["checks"])
    assert rc == 1
    assert "FAIL " in stderr and "failed (placement bound" in stderr.strip().splitlines()[-1]


@pytest.mark.parametrize("scene", SCENES)
@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("config", CONFIGS)
def test_the_placement_error_in_the_extra_viewports_is_zero_for_every_class(result, scene, mode, config):
    """Not vacuous: every class was measured on every tick in every view, and the placement-only
    error (f64, no GPU rounding) of viewports A and B is under a millimetre."""
    _, data, _ = result
    key = f"{scene}/{mode}/{config}"
    run = data["table"][key]
    classes = ["rockCentre", "dot:Mars", "bodyFixedOrigin", "bodyFixedSurface"]
    classes += ["globe"] if config == "globe" else ["bodyCentre", "bodyVertex"]
    for view in ("primary", "A", "B"):
        for cls in classes:
            s = run["total"][view][cls]
            assert s["n"] >= 24 and s["skipped"] == 0, (key, view, cls, s)
    for view in ("A", "B"):
        for cls in classes:
            p = run["place"][view][cls]
            assert p["maxErr"] <= 1e-3 and p["maxHop"] <= 1e-3, (key, view, cls, p)
    assert 2.0 < run["geometry"]["chaserDistanceFromViewportA_m"] < 40.0, run["geometry"]
    if config == "globe":
        # level 0 (2 tiles) from the far camera; finer tiles when the primary is framed close to the body
        assert run["geometry"]["tiles"] >= (10 if (scene, mode) in (("LEO", "framed"), ("lunar", "framed")) else 2), run["geometry"]


@pytest.mark.parametrize("scene", SCENES)
@pytest.mark.parametrize("mode", MODES)
@pytest.mark.parametrize("config", CONFIGS)
def test_the_primarys_numbers_are_unchanged_and_its_placement_bit_identical(result, scene, mode, config):
    _, data, _ = result
    key = f"{scene}/{mode}/{config}"
    for fragment in ("the primary's on-screen error, hop and placement error",
                     "hold exactly _toLocal's values, bit for bit", "the tick leaves the primary placed"):
        matched = [c for c in _matched(data, f"{key}: ") if fragment in c["name"]]
        assert len(matched) == 1 and matched[0]["pass"], (fragment, matched)
    before = json.loads(BEFORE.read_text())["runs"][key]
    assert len(before) >= 3, before


def test_every_view_draws_dots_for_its_own_camera(result):
    _, data, _ = result
    matched = _matched(data, "draws a body's dot exactly when its disc is under 1.5 px in THAT view")
    assert len(matched) == 12 and all(m["pass"] for m in matched)
    matched = _matched(data, "Debris is a dot from the far primary and a disc from viewport A")
    assert len(matched) == 3 and all(m["pass"] for m in matched)


@pytest.mark.parametrize("scene", SCENES)
def test_picking_a_body_in_an_extra_viewport_resolves_at_its_true_position(result, scene):
    _, data, _ = result
    for mode in MODES:
        for view in ("A", "B"):
            p = data["picking"][f"{scene}/{mode}/{view}"]
            assert p["picks"] >= 8 and p["ok"] == p["picks"], p
            assert p["maxHitErrM"] <= 1e-3 and p["changed"] == [], p


def test_breaks_the_old_behaviour_is_seen_by_this_measurement(result):
    """Teeth: placing every draw for the primary's origin (the pre-change behaviour) is measured as
    the defect by this same measurement, in the same process; the primary is unaffected."""
    _, data, _ = result
    matched = _matched(data, "BREAKS:")
    assert len(matched) == 3 and all(m["pass"] for m in matched), matched
    broken = data["table"]["lunar/fit/plain/BREAKS (every draw placed for the primary origin)"]["place"]["A"]
    assert broken["bodyCentre"]["maxErr"] > 1.0 and broken["rockCentre"]["maxErr"] > 1.0, broken

"""Question 240 (defect 3 of the heavy carry-over round; question 252, the heavy precision
round): every viewport draws the shared entity objects relative to ITS OWN floating origin.

Same "run the real code, don't port it" discipline as tests/test_viewer_viewport.py: this file
shells out to `node` to run web/js/entities_viewport_precision_check.mjs, which binds the real
`Viewer.prototype` of web/js/scene.js (`setScenario`, `addViewport`, `setViewportFocus`,
`update(t)`, `_placeSharedEntities`, ...) onto a minimal stand-in (WebGL, the DOM and
OrbitControls are the only stand-ins), with real `Viewport`s, a real `InstancedMesh`, a real
`ResidentEntityScene` / `LayerManager` and the real parsed glTF fixture, and prints one JSON
object of named checks. Nothing here reimplements any of that arithmetic in Python.

What the checks measure (see the .mjs module docstring): for each of three viewports (the
primary and two extra ones), at LEO (7,000 km), GEO (42,164 km) and lunar (384,400 km) distance,
the final float32 vertex position in that viewport's own eye frame (the object's modelViewMatrix
rounded to float32, applied to its float32 vertex data with every operation rounded to float32)
against the float64 truth, for the marker spheres, the instanced markers, the trails, the
covariance ellipsoids, the keep-out volumes, the glTF model and an event mesh; maximum error and
maximum tick-to-tick hop, each under question 46's centimetre bound. The primary's numbers must
be the ones the code gave before the fix (literals in the .mjs, taken from commit c6c463a).

The real-browser half is tests/test_viewer_viewport_precision_browser.py.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_viewport_precision_check.mjs"
NODE = shutil.which("node")

DISTANCES = ("LEO", "GEO", "lunar")
CLASSES = ("sphere", "marker", "trail", "ellipsoid", "keepout", "model", "event")


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; the check is an ES module that runs "
            "web/js/scene.js for real (see module docstring), so it cannot proceed without node."
        )
    return NODE


def _run(env_extra: dict | None = None) -> tuple[int, dict]:
    node = _require_node()
    env = dict(os.environ)
    env.update(env_extra or {})
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=300, env=env,
    )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout[:2000]!r}\nstderr: {proc.stderr[:4000]}"
        )
    return proc.returncode, data


@pytest.fixture(scope="module")
def result() -> tuple[int, dict]:
    return _run()


def _failed(data: dict, substring: str = "") -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched {substring!r} -- did the check names in {CHECK.name} change?"
    return matches


def test_the_check_exits_zero_and_every_check_passes(result):
    """Question 233: a failing check must not exit 0, and a passing run must."""
    rc, data = result
    failed = _failed(data)
    assert not failed, f"{len(failed)} check(s) failed:\n" + "\n".join(failed[:20])
    assert data["allPass"] is True
    assert rc == 0, f"all checks passed but the process exited {rc}"


def test_a_failing_check_exits_nonzero():
    """The same run with the bound tightened a thousandfold (below what float32 can do at the
    framing distance) must report failures and exit nonzero."""
    rc, data = _run({"AV_PRECISION_BOUND_M": "1e-6"})
    assert data["allPass"] is False and _failed(data), "tightening the bound did not fail any check"
    assert rc != 0, "checks failed but the process exited 0"


@pytest.mark.parametrize("distance", DISTANCES)
@pytest.mark.parametrize("mode", ("fit", "framed"))
def test_every_viewport_and_class_was_measured_and_is_under_a_centimetre(result, distance, mode):
    """Not vacuous: every (viewport, class) was measured on every tick; viewports A and B are
    held to the bound in both primary modes, the primary only when it is framed on the Chaser
    (with its origin at the frame origin it keeps the pre-fix numbers, see the next test)."""
    _, data = result
    key = f"{distance}/{mode}"
    views = ("A", "B") if mode == "fit" else ("primary", "A", "B")
    for view in views:
        for cls in CLASSES:
            matched = _matched(data, f"{key}: {view}/{cls} ")
            # measured / error / hop
            assert len(matched) >= 3, (key, view, cls, [m["name"] for m in matched])
            bad = [m["name"] for m in matched if not m["pass"]]
            assert not bad, bad
    table = data["table"][key]
    for view in views:
        for cls in CLASSES:
            s = table[view][cls]
            assert s["maxErr"] <= 0.01 and s["maxHop"] <= 0.01, (key, view, cls, s)


@pytest.mark.parametrize("distance", DISTANCES)
@pytest.mark.parametrize("mode", ("fit", "framed"))
def test_the_primarys_numbers_are_unchanged(result, distance, mode):
    """The primary's maximum error and hop equal the numbers measured on the code before the fix
    (the literals in the .mjs), and after a full tick the scene graph holds exactly `_toLocal`'s
    values bit for bit."""
    _, data = result
    key = f"{distance}/{mode}"
    assert not _failed(data, f"{key}: primary/"), _failed(data, f"{key}: primary/")
    assert not _failed(data, f"{key}: after the tick the scene graph holds exactly the primary's pre-fix values")
    assert not _failed(data, f"{key}: the tick leaves the primary placed")


@pytest.mark.parametrize("distance", DISTANCES)
def test_the_defect_case_is_real_a_viewport_origin_far_from_the_primarys(result, distance):
    """With the primary fit() to the frame origin the extra viewports' origins are 7, 42 and 384
    scene units away from it: the case the defect needs (a cm-level answer could otherwise come
    from the two origins happening to coincide)."""
    _, data = result
    g = data["table"][f"{distance}/fit"]["geometry"]
    assert g["originDifferenceAvsPrimary"] > 1.0, g
    assert 2.0 < g["chaserDistanceFromViewportA_m"] < 40.0, g


def test_the_placement_is_per_view_cached_and_keyed_by_the_drawing_camera(result):
    _, data = result
    for fragment in (
        "Viewer places the shared entity objects per drawing view",
        "placed for viewport A: the Chaser sphere",
        "placed for viewport A: the model group, ellipsoid and keep-out",
        "placed for viewport A: the instanced marker",
        "placed for viewport A: the Float32 instance matrix holds only the small",
        "placed for viewport A: the trail line carries the f64 shift difference",
        "already placed for this view in this epoch",
        "a new epoch (a tick, a rebase) writes the buffers again",
        "scene.onBeforeRender places for the drawing camera",
    ):
        matched = _matched(data, fragment)
        assert all(m["pass"] for m in matched), matched


def test_breaks_the_old_behaviour_is_seen_by_this_measurement(result):
    """Teeth: placing every draw for the primary's origin (the pre-fix behaviour) is measured as
    the defect, by this same measurement, in the same process; the primary is unaffected."""
    _, data = result
    matched = _matched(data, "BREAKS:")
    assert len(matched) == 2 and all(m["pass"] for m in matched), matched
    broken = data["table"]["LEO/fit/BREAKS (every draw placed for the primary origin)"]["A"]
    assert broken["sphere"]["maxErr"] > 0.1 and broken["marker"]["maxErr"] > 0.1, broken

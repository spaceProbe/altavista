"""Question 254 item 3 (heavy open-items round): picking in an extra viewport uses THAT viewport's
own placement of the shared entity objects, and the glTF model is a pick target.

Same "run the real code, don't port it" discipline as tests/test_entities_viewport_precision.py:
this shells out to `node` to run web/js/viewport_picking_check.mjs, which binds the real
`Viewer.prototype` of web/js/scene.js (`setScenario`, `addViewport`, `setViewportFocus`,
`update(t)`, `pick`, ...) onto a minimal stand-in (WebGL, the DOM and OrbitControls are the only
stand-ins) and prints one JSON object of named checks (exit 1 when any fails). Nothing here
reimplements the geometry in Python.

What it checks (see the .mjs module docstring), at LEO 7,000 km, GEO 42,164 km and lunar 384,400 km,
for a viewport framed ~10 m from the 1.5 m Chaser model: `Viewer.pick(vp.camera, ...)` at the
model's float64-truth projected origin and 12 points across its truth silhouette resolves to the
Chaser; points off the silhouette do not; the Target's marker resolves to the Target; `pick()`
leaves the scene graph bit for bit as it found it; the primary camera's picks agree with the old
target list. The real-browser half is tests/test_viewer_viewport_picking_browser.py.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "viewport_picking_check.mjs"
NODE = shutil.which("node")

DISTANCES = ("LEO", "GEO", "lunar")


@pytest.fixture(scope="module")
def result() -> tuple[int, dict]:
    if NODE is None:
        pytest.skip("node is not installed; the check runs web/js/scene.js for real")
    proc = subprocess.run([NODE, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=300)
    first = proc.stdout.split("\n", 1)[0]
    try:
        data = json.loads(first)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout[:2000]!r}\nstderr: {proc.stderr[:4000]}"
        )
    return proc.returncode, data


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched {substring!r} -- did the check names in {CHECK.name} change?"
    return matches


def test_check_passes_and_exits_zero(result):
    code, data = result
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert failed == [], failed
    assert data["allPass"] is True
    assert code == 0


@pytest.mark.parametrize("distance", DISTANCES)
def test_model_silhouette_resolves_to_the_chaser_in_the_extra_viewport(result, distance):
    _, data = result
    for mode in ("fit", "framed"):
        r = data["table"][f"{distance}/{mode}"]
        assert r["A"]["inside"]["total"] == 13 and r["A"]["inside"]["ok"] == 13, r["A"]["inside"]
        assert r["A"]["off"]["total"] >= 4 and r["A"]["off"]["ok"] == r["A"]["off"]["total"], r["A"]["off"]
        assert r["B"]["ok"] == r["B"]["total"] >= 3, r["B"]


@pytest.mark.parametrize("distance", DISTANCES)
def test_pick_leaves_the_scene_graph_untouched_and_the_primary_placed(result, distance):
    _, data = result
    for mode in ("fit", "framed"):
        r = data["table"][f"{distance}/{mode}"]
        assert r["unchanged"] is None, r["unchanged"]
        assert r["placedBefore"] == "primary" and r["placedAfter"] == "primary"


def test_primary_picks_agree_with_the_old_target_list(result):
    _, data = result
    checks = _matched(data, "agree with the old target list")
    assert len(checks) == 6 and all(c["pass"] for c in checks)

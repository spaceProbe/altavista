"""Question 254 item 3b (heavy open-items round): the spacecraft markers and event meshes are a
fixed size on screen IN EVERY VIEW that draws them, on every tick of a running clock.

Same "run the real code, don't port it" discipline as tests/test_viewport_picking.py: this shells out
to `node` to run web/js/marker_pixel_size_check.mjs, which binds the real `Viewer.prototype` of
web/js/scene.js onto a minimal stand-in (WebGL, the DOM and OrbitControls are the only stand-ins) and
prints one JSON object of named checks (exit 1 when any fails). Nothing here reimplements the
geometry in Python.

What it checks (see the .mjs module docstring), at LEO 7,000 km, GEO 42,164 km and lunar 384,400 km,
with the primary after `fit()` and framed on the Chaser, for the primary and two extra viewports
(A ~10 m from the Chaser, B farther) over 24 ticks of a running clock: each marker's and event's
radius as drawn (the float32 model-view matrix, the view's own canvas height and fov) is within 0.1%
of the declared 3.5 px / 4.5 px; a primary alone has the scales the code gave before the change, bit
for bit; a camera moved or a canvas resized within one epoch is sized again; the sizing is cheap.
The real-browser half is tests/test_viewer_marker_pixel_size_browser.py.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "marker_pixel_size_check.mjs"
NODE = shutil.which("node")

DISTANCES = ("LEO", "GEO", "lunar")
MODES = ("fit", "framed")


@pytest.fixture(scope="module")
def result() -> tuple[int, dict]:
    if NODE is None:
        pytest.skip("node is not installed; the check runs web/js/scene.js for real")
    proc = subprocess.run([NODE, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=300)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout[:2000]!r}\nstderr: {proc.stderr[:4000]}"
        )
    data["stderr"] = proc.stderr
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
    assert "marker_pixel_size_check: " in data["stderr"] and " 0 failed" in data["stderr"]


@pytest.mark.parametrize("distance", DISTANCES)
@pytest.mark.parametrize("mode", MODES)
def test_every_view_draws_every_marker_at_its_declared_size_on_every_tick(result, distance, mode):
    _, data = result
    rows = data["table"][f"{distance}/{mode}"]["rows"]
    # 3 views x (2 spacecraft markers + 1 event), every one drawn on all 24 ticks
    assert len(rows) == 9, [(r["view"], r["kind"], r["name"]) for r in rows]
    for r in rows:
        assert r["n"] >= 24, r
        assert r["maxDev"] <= 0.001, r


@pytest.mark.parametrize("distance", DISTANCES)
def test_viewport_a_is_a_close_up_and_b_is_farther(result, distance):
    _, data = result
    d = data["table"][f"{distance}/fit"]["distancesM"]
    assert 2 < d["A"]["Chaser"] < 40, d
    assert d["B"]["Target"] > 5 * d["A"]["Chaser"], d


def test_a_primary_alone_keeps_the_scales_it_had_bit_for_bit(result):
    _, data = result
    checks = _matched(data, "primary only: the digest of all")
    assert len(checks) == 6 and all(c["pass"] for c in checks)
    checks = _matched(data, "is bit for bit the documented formula's")
    assert len(checks) == 6 and all(c["pass"] for c in checks)


def test_a_camera_or_canvas_changed_within_an_epoch_is_sized_again(result):
    _, data = result
    checks = _matched(data, "within one epoch")
    assert len(checks) == 18 and all(c["pass"] for c in checks)

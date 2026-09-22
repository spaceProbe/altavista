"""H6 (docs/heavy-plan.md), scope item 1: covariance ellipsoids and keep-out volumes.

Runs the real, shipped ES modules -- web/js/entities/covariance_ellipsoid.js,
web/js/entities/keepout_volume.js, web/js/entities/ellipsoid_mesh.js -- under node, via
web/js/entities_ellipsoid_check.mjs, and asserts on the JSON it prints. Same discipline
as tests/test_viewer_jitter.py: no arithmetic is re-implemented in Python, `node` is
required (skipped, not faked, if absent).

What the harness proves (see entities_ellipsoid_check.mjs's own module docstring for
the full detail): a diagonal covariance's semi-axes match the closed-form truth
exactly; a rotated covariance's semi-axes are unchanged and its recovered axes match a
known rotation built independently of anything in web/js/entities/; a 6x6 covariance's
position block is extracted correctly and ignores the velocity block; a keep-out
volume's margin is applied exactly and point-containment agrees with an independently
evaluated quadratic form; a built THREE.Mesh's WORLD-SPACE scale (read from the scene
graph, through a scaled parent) matches the closed-form semi-axes; every declared error
path (missing sigma, missing margin, asymmetric input, non-PSD input) actually throws
CovarianceShapeError.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_ellipsoid_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/entities/ is ES modules, run for real under node (see this file's own module docstring).")
    return NODE


@pytest.fixture(scope="module")
def data() -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=30,
    )
    # `CHECK` sets process.exitCode = 1 when allPass is false (manager review, round 6,
    # matching web/js/layout/layout_tree_check.mjs) -- but we still want to parse and report
    # the JSON in that case, so the failing CHECK NAMES are what a failure says rather than a
    # bare exit code. See tests/test_viewer_layout.py's own fixture for the same reasoning. A
    # non-JSON stdout (a real crash) is still a hard failure, below.
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{CHECK.name} did not print valid JSON (exit {proc.returncode}): {proc.stdout!r}\nstderr: {proc.stderr}")


def test_all_ellipsoid_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"covariance ellipsoid checks failed: {failed}"
    assert data["allPass"] is True


def test_diagonal_covariance_is_exact_closed_form(data):
    """sigma=2, cov=diag(4,9,16) km^2 -- the semi-axes MUST be exactly
    sigma*sqrt(eigenvalue) = [8,6,4] km, sorted descending; this is the closed-form
    ground truth a diagonal matrix's own eigendecomposition gives for free, no
    eigensolver needed to know it independently.
    """
    got = data["diagonalMeasuredSemiAxesKm"]
    expected = data["diagExpectedSemiAxesKm"]
    assert got == pytest.approx(expected, abs=1e-9), f"diagonal-covariance semi-axes {got} != closed-form {expected}"


def test_rotated_covariance_semi_axes_unchanged(data):
    """A rotation cannot change a covariance's eigenvalues -- the rotated-covariance
    scenario's semi-axes must equal the SAME closed-form values as the diagonal case.
    """
    got = data["rotatedMeasuredSemiAxesKm"]
    expected = data["diagExpectedSemiAxesKm"]
    assert got == pytest.approx(expected, abs=1e-6), f"rotated-covariance semi-axes {got} != {expected}"


def test_keepout_margin_applied_exactly(data):
    expected = [a + 0.5 for a in data["diagonalMeasuredSemiAxesKm"]]
    assert data["keepOutSemiAxesKm"] == pytest.approx(expected, abs=1e-9)


def test_mesh_world_scale_matches_closed_form(data):
    """The scene-graph assertion: a built THREE.Mesh's world-space scale (read via
    matrixWorld decomposition, through a scaled parent group) must equal the same
    closed-form semi-axes -- never trusted from a counter alone.
    """
    got = data["meshWorldSemiAxesKm"]
    expected = data["diagExpectedSemiAxesKm"]
    assert got == pytest.approx(expected, abs=1e-9)


def test_ellipsoid_report(data, capsys):
    with capsys.disabled():
        print(f"\ncovariance ellipsoid (sigma={data['sigma']}):")
        print(f"  diagonal semi-axes (closed form) = {data['diagExpectedSemiAxesKm']}")
        print(f"  diagonal semi-axes (measured)    = {data['diagonalMeasuredSemiAxesKm']}")
        print(f"  rotated semi-axes (measured)     = {data['rotatedMeasuredSemiAxesKm']}")
        print(f"  keep-out semi-axes (margin 0.5)  = {data['keepOutSemiAxesKm']}")
        print(f"  mesh world semi-axes             = {data['meshWorldSemiAxesKm']}")

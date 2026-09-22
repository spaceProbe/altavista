"""H6 (docs/heavy-plan.md), scope item 4: "the RIC jitter test extended to a model at a
ten-metre RPO range (question 46)."

Runs web/js/entities_jitter_check.mjs under node -- see that file's own module
docstring for exactly what it measures and which real, shipped modules it reuses
(scene_jitter_harness.mjs's own `buildTrack`/`SCENES`, interp.js's `TrajectoryInterp`,
origin.js's `FloatingOrigin`, the vendored GLTFLoader via
web/js/entities/model_entity.js's `parseGLTFAsset`, against the same fixture
web/js/entities_model_check.mjs already proves parses correctly). Same discipline as
tests/test_viewer_jitter.py: no arithmetic reimplemented in Python, `node` required
(skipped, not faked, if absent), and the "without floating origin" bound is asserted to
FAIL -- never loosened or removed -- so this stays a proof the module is load-bearing
for a real model at RPO range, not merely present.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_jitter_check.mjs"
FIXTURE = REPO_ROOT / "web" / "js" / "fixtures" / "entity_model_fixture.gltf"

NODE = shutil.which("node")

CENTIMETRE_BOUND_M = 0.01  # docs/open-questions.md Q46's own bound, restated here (never loosened)


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; this test intentionally runs the real viewer modules under node rather than porting the arithmetic to Python (see this file's own module docstring).")
    return NODE


@pytest.fixture(scope="module")
def data() -> dict:
    if not FIXTURE.exists():
        pytest.skip(f"{FIXTURE} is missing -- this repo commits it (hand-authored, embedded-buffer glTF, no GMAT process needed).")
    node = _require_node()
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0, (
        f"node {CHECK.name} exited {proc.returncode}\nstdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"{CHECK.name} did not print valid JSON: {proc.stdout!r}\nstderr: {proc.stderr}")


def test_model_at_rpo_range_holds_centimetre_stability_with_floating_origin(data):
    """A real glTF model's own mesh vertices (up to 1.5 m from the model's local
    origin), attached to a spacecraft 10 m from another at LEO altitude, with a real
    non-identity attitude applied, must stay within the centimetre bound once
    reconstructed through the floating origin.
    """
    err = data["errWithM"]
    assert err < CENTIMETRE_BOUND_M, (
        f"model-at-RPO-range floating-origin error {err:.6g} m exceeds the centimetre bound"
    )


def test_model_at_rpo_range_fails_without_floating_origin(data):
    """The load-bearing assertion (never loosened or removed to make the suite green,
    per this file's own module docstring): WITHOUT the floating origin, the same real
    model, at the same 10 m range, must EXCEED the centimetre bound -- proving the
    origin is necessary for a real model's own mesh vertices, not just for a
    zero-extent trajectory point.
    """
    err = data["errWithoutM"]
    assert err > CENTIMETRE_BOUND_M, (
        f"expected the no-floating-origin path to exceed the centimetre bound for a model "
        f"at RPO range, but measured only {err:.6g} m"
    )


def test_every_model_vertex_individually_holds_the_bound(data):
    """Not just the worst-case max: every one of the model's own real mesh vertices
    must individually hold the centimetre bound -- a bug that only affected, say, the
    farthest vertex would still be a real defect even if the overall max happened to
    be dominated by a different vertex.
    """
    per_vertex = data["perVertexMaxErrWithM"]
    assert len(per_vertex) == data["vertexCount"] > 0
    for i, err in enumerate(per_vertex):
        assert err < CENTIMETRE_BOUND_M, f"vertex {i}: error {err:.6g} m exceeds the centimetre bound"


def test_entities_jitter_report(data, capsys):
    with capsys.disabled():
        print(
            f"\nmodel-at-{data['sepM']}m-RPO-range jitter (web/js/entities_jitter_check.mjs):\n"
            f"  vertexCount={data['vertexCount']} farthestVertexOffsetM={data['farthestVertexOffsetM']}\n"
            f"  errWithM={data['errWithM']:.6e}  errWithoutM={data['errWithoutM']:.6e}  "
            f"bound={CENTIMETRE_BOUND_M}\n"
            f"  perVertexMaxErrWithM={data['perVertexMaxErrWithM']}"
        )

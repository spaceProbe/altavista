"""H6 (docs/heavy-plan.md), scope item 2: glTF asset models with attitude from the
attitude stream.

Runs web/js/entities/model_entity.js under node, via web/js/entities_model_check.mjs,
against the hand-authored web/js/fixtures/entity_model_fixture.gltf (embedded-buffer,
texture-free, 4 vertices / 3 triangles). Same discipline as tests/test_viewer_jitter.py:
no glTF parsing or slerp arithmetic is re-implemented in Python; `node` is required
(skipped, not faked, if absent). The vendored `web/vendor/three/addons/loaders/
GLTFLoader.js` does the real parsing; `web/js/interp.js`'s real, unmodified
`BodyInterp` drives attitude.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_model_check.mjs"
FIXTURE = REPO_ROOT / "web" / "js" / "fixtures" / "entity_model_fixture.gltf"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/entities/ is ES modules, run for real under node (see this file's own module docstring).")
    return NODE


@pytest.fixture(scope="module")
def data() -> dict:
    if not FIXTURE.exists():
        pytest.skip(f"{FIXTURE} is missing -- this repo commits it (a hand-authored, embedded-buffer glTF, no GMAT process needed to regenerate it).")
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


def test_all_model_entity_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"model entity checks failed: {failed}"
    assert data["allPass"] is True


def test_real_gltf_parse_reproduces_fixture_vertices(data):
    """The vendored GLTFLoader must reproduce the exact vertex positions this task
    wrote into the fixture's own base64 buffer -- read from the parsed scene graph,
    not merely 'the loader didn't throw'.
    """
    checks_by_name = {c["name"]: c for c in data["checks"]}
    assert checks_by_name["realGLTFParse_vertexPositionsMatchFixtureGroundTruth"]["pass"] is True


def test_attitude_wired_through_body_interp_and_is_not_identity(data):
    """A ModelEntity given a real BodyInterp WITH a non-trivial quat track must end up
    with a NON-identity quaternion after update() -- proving the attitude stream is
    actually wired through, not silently defaulting.
    """
    checks_by_name = {c["name"]: c for c in data["checks"]}
    assert checks_by_name["attitudeWiredThroughBodyInterp_matchesDirectCall"]["pass"] is True
    assert checks_by_name["attitudeWiredThroughBodyInterp_isNotIdentity"]["pass"] is True


def test_missing_attitude_degrades_to_identity_and_model_does_not_disappear(data):
    """This task's own brief, verbatim: 'a model whose body has no attitude must
    degrade to the identity orientation, not throw and not disappear.' Both halves
    checked directly: the quaternion is identity, AND the model's mesh is still a
    child of the entity's group.
    """
    checks_by_name = {c["name"]: c for c in data["checks"]}
    assert checks_by_name["missingQuatDegradesToIdentity_quaternionIsIdentity"]["pass"] is True
    assert checks_by_name["missingQuatDegradesToIdentity_modelStillInSceneGraph"]["pass"] is True


def test_model_entity_report(data, capsys):
    with capsys.disabled():
        print("\nentity model checks (web/js/entities_model_check.mjs):")
        for c in data["checks"]:
            print(f"  [{'PASS' if c['pass'] else 'FAIL'}] {c['name']}")

"""H6 (docs/heavy-plan.md), scope item 3: instanced markers and trails, budgeted like
the layers.

Runs web/js/entities/entities_instanced_layer.js under node, via
web/js/entities_layer_check.mjs, against the REAL, shipped `LayerManager`
(web/js/layers/layer.js) -- the identical class web/js/layers_budget_check.mjs already
proves for imagery. Same discipline as tests/test_viewer_jitter.py: no budget/admission
arithmetic is re-implemented in Python; `node` is required (skipped, not faked, if
absent).
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "entities_layer_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip("node is not installed in this environment; web/js/entities/ and web/js/layers/ are ES modules, run for real under node (see this file's own module docstring).")
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


def test_all_entity_layer_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"entity layer checks failed: {failed}"
    assert data["allPass"] is True


def test_hard_budget_never_exceeded(data):
    """A wanted set 2.13x a deliberately small budget must never push
    residentBytes + pendingBytes over memoryBudgetBytes, at any sampled step."""
    assert data["residentBytes"] + data["pendingBytes"] <= data["scenario"]["memoryBudgetBytes"]


def test_soft_violation_never_fires_for_this_scenario(data):
    assert data["softViolationCount"] == 0


def test_deferral_and_cancellation_are_real(data):
    """Both counters must be nonzero for this over-budget, camera-move scenario --
    a zero here would mean the scenario never actually exercised what it claims to."""
    assert data["deferredCount"] > 0, "expected some request to be deferred by the hard budget"
    assert data["cancelledCount"] > 0, "expected the camera-move to cancel at least one in-flight request"


def test_adapters_genuinely_honour_the_abort_signal(data):
    """Isolated from LayerManager's own bookkeeping (see entities_layer_check.mjs's own
    module docstring): MarkerLayerAdapter/TrailLayerAdapter.load() must themselves
    reject when aborted, both pre-aborted and mid-flight.
    """
    checks_by_name = {c["name"]: c for c in data["checks"]}
    for name in [
        "markerAdapter_loadRejectsWhenPreAborted", "markerAdapter_loadRejectsMidFlight",
        "trailAdapter_loadRejectsWhenPreAborted", "trailAdapter_loadRejectsMidFlight",
    ]:
        assert checks_by_name[name]["pass"] is True, f"{name} failed: {checks_by_name[name]['detail']}"


def test_scene_graph_matches_resident_set(data):
    """'Assert from the scene graph, never a counter alone' (this round's rule): the
    built THREE.InstancedMesh/THREE.Line objects' own transforms/vertices must match
    the ORIGINAL input marker/trail data, not merely have the right count.
    """
    checks_by_name = {c["name"]: c for c in data["checks"]}
    assert checks_by_name["sceneGraph_markerInstanceTransformsMatchOriginalPositions"]["pass"] is True
    assert checks_by_name["sceneGraph_trailLineVerticesMatchOriginalPoints"]["pass"] is True


def test_entity_layer_report(data, capsys):
    with capsys.disabled():
        s = data["scenario"]
        print("\nentity markers/trails layer budget (web/js/entities_layer_check.mjs):")
        print(f"  markers={s['markerCount']} trails={s['trailCount']}x{s['trailPoints']}pts "
              f"declaredBytes={s['totalDeclaredBytes']} budget={s['memoryBudgetBytes']} "
              f"ratio={s['overBudgetRatio']:.2f}x maxConcurrentLoads={s['maxConcurrentLoads']}")
        print(f"  residentBytes={data['residentBytes']} pendingBytes={data['pendingBytes']} "
              f"deferredCount={data['deferredCount']} cancelledCount={data['cancelledCount']} "
              f"softViolationCount={data['softViolationCount']}")
        print(f"  finalCounts={data['finalCounts']}")

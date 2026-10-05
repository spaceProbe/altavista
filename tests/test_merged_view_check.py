"""Heavy cleanup round 1 (questions 235/237): the merged-view per-tick `LayerManager.update`.

Runs `web/js/merged_view_check.mjs` under node: the real `LayerManager`, a real
`GlobeLayer` in its composer mode, the real entity adapters and `ResidentEntityScene`,
driven through `updateComposed` (`web/js/layers/layer.js`) -- the one per-tick driver a
real `Viewer` uses. Two properties are proved there:

  B. with a globe AND entities on one manager, no imagery load is cancelled by the entity
     half of the view, and the loads complete (the round-7 failure mode, inverted); and
  C. an entity payload the manager evicts under a tight budget is re-admitted and drawn
     again once the pressure lifts, with residency and the scene graph read off the
     manager and the groups, never off a counter.

The real-browser half (a real `Viewer` with a real globe, entities and Chrome) is
`tests/test_viewer_globe_layer_manager.py`.

Same discipline as `tests/test_entities_scene.py`: nothing is re-implemented in Python;
the JSON on stdout names every check, and this file reports the failing check NAMES
rather than trusting a bare exit code (question 148: "an exit code is not evidence").
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
CHECK = REPO_ROOT / "web" / "js" / "merged_view_check.mjs"

NODE = shutil.which("node")

PROOF_B = [
    "B_oneManagerUpdatePerTick",
    "B_imageryLoadsStayInFlightAcrossEntityOnlyChanges",
    "B_noImageryLoadCancelledByEntityOnlyChange",
    "B_imageryLoadsCompleteThroughTheOneManager",
    "B_globeMeshesBoundToTheResidentTextures",
    "B_entitiesResidentAlongsideTheImagery",
    "B_noSoftBudgetViolation",
    "B_controlTwoPartialUpdatesCancelImagery",
]

PROOF_C = [
    "C_entitiesResidentAndDrawnBeforeAnyPressure",
    "C_stable_residentEntityReplanIsNoOpNotAReloadPerTick",
    "C_wantedEntityPayloadsAreNotEvictedUnderPressure",
    "C_entityPayloadsEvictedByTheManagerUnderPressure",
    "C_evictedEntitiesAreNotDrawnInTheSceneGraph",
    "C_reWantedEntityStaysAbsentWhileItDoesNotFit",
    "C_notDrawnWhileWantedButDeferred",
    "C_reAdmittedOncePressureLifts",
    "C_drawnAgainAfterReAdmission",
    "C_budgetHeldAtEveryObservedTick",
    "C_reloadLeavesNoStaleEntityResidencyOrSceneObjects",
    "C_reloadedSceneAdmitsAndDrawsFromACleanSlate",
]


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/layers/, web/js/globe.js and "
            "web/js/entities/ are ES modules, run for real under node."
        )
    return NODE


@pytest.fixture(scope="module")
def data() -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(CHECK)], cwd=str(CHECK.parent), capture_output=True, text=True, timeout=60,
    )
    try:
        parsed = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{CHECK.name} did not print valid JSON (exit {proc.returncode}): "
            f"{proc.stdout!r}\nstderr: {proc.stderr}"
        )
    # An exit code that disagrees with the JSON's own verdict is itself a defect
    # (question 233): the check must exit non-zero exactly when `allPass` is false.
    assert (proc.returncode == 0) == bool(parsed["allPass"]), (
        f"{CHECK.name} exit code {proc.returncode} disagrees with allPass={parsed['allPass']}"
    )
    return parsed


def _by_name(data: dict) -> dict:
    return {c["name"]: c for c in data["checks"]}


def test_all_merged_view_checks_pass(data):
    failed = [c["name"] for c in data["checks"] if not c["pass"]]
    assert not failed, f"merged-view checks failed: {failed}"
    assert data["allPass"] is True


def test_check_names_are_distinct(data):
    names = [c["name"] for c in data["checks"]]
    assert len(names) == len(set(names)), f"duplicate check names: {names}"


@pytest.mark.parametrize("name", PROOF_B)
def test_proof_b_no_imagery_load_cancelled_by_the_entity_half(data, name):
    check = _by_name(data)[name]
    assert check["pass"] is True, f"{name}: {check['detail']!r}"


@pytest.mark.parametrize("name", PROOF_C)
def test_proof_c_an_evicted_entity_payload_comes_back(data, name):
    check = _by_name(data)[name]
    assert check["pass"] is True, f"{name}: {check['detail']!r}"


def test_merged_view_report(data, capsys):
    by_name = _by_name(data)
    with capsys.disabled():
        print("\nmerged-view check (web/js/merged_view_check.mjs):")
        print(f"  allPass={data['allPass']} checks={len(data['checks'])}")
        print(f"  B control (two partial updates per tick): {by_name['B_controlTwoPartialUpdatesCancelImagery']['detail']}")
        print(f"  C eviction: {by_name['C_entityPayloadsEvictedByTheManagerUnderPressure']['detail']}")

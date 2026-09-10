"""M26.2 windowing core tests: the hand-rolled binary split-tree tiling model
(docs/open-questions.md question 161, "Decided by the user: hand-rolled tiling
(binary split tree, drag handles, collapse to a rail, layouts as data persisted per
viewer and shareable as JSON). No dependency, no build step.") and the panel set it
carries (question 162: "all four" panels planned for v1 -- this milestone only builds
the windowing core itself, not the panels).

Same "run the real code, don't port it" discipline as tests/test_viewer_jitter.py and
tests/test_viewer_globe.py: this file shells out to `node` to run
web/js/layout/layout_tree_check.mjs, the real, shipped, framework-free ES modules
under web/js/layout/ (split_tree.js, persistence.js, default_layouts.js -- no Three.js,
no DOM, so plain `node` is enough, exactly like web/js/origin.js). Nothing here
reimplements the tree/validation/persistence logic in Python; every assertion below
reads a field out of the JSON that harness prints.

web/js/layout/layout_manager.js (DOM rendering, drag handles, keyboard equivalents,
the export/import toolbar, the corrupt-layout error banner) is deliberately NOT
exercised here -- it requires a real `document` (there is no jsdom or similar in this
repo, and this task adds no new dependency). It is verified instead by an interactive
browser check (recorded in web/js/layout/REPORT.md) that the panes render, a drag on a
handle resizes them, and the keyboard equivalent on a focused handle does too.

See web/js/layout/layout_tree_check.mjs's own module docstring for the full
"what would this fail against" story per check group; the summaries below are a
shorter pointer back to it, per this task's standing review requirement.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
LAYOUT_TREE_CHECK = REPO_ROOT / "web" / "js" / "layout" / "layout_tree_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/layout/split_tree.js "
            "and friends are framework-free ES modules and this test intentionally "
            "runs them for real (see module docstring) rather than porting the tree "
            "logic to Python, so it cannot proceed without node. Install node to run it."
        )
    return NODE


def _run_node_json(script: Path) -> dict:
    node = _require_node()
    proc = subprocess.run(
        [node, str(script)],
        cwd=str(script.parent),
        capture_output=True,
        text=True,
        timeout=30,
    )
    # layout_tree_check.mjs sets process.exitCode = 1 when allPass is false -- but we
    # still want to parse and report the JSON in that case rather than treating it as
    # a hard failure to run, so don't assert returncode == 0 here (unlike the other
    # harnesses' _run_node_json, which never intentionally fail this way). A non-JSON
    # stdout (a real crash) is still caught below.
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{script.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}"
        )


@pytest.fixture(scope="module")
def layout_data() -> dict:
    return _run_node_json(LAYOUT_TREE_CHECK)


@pytest.fixture(scope="module")
def layout_run_1_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(LAYOUT_TREE_CHECK)], cwd=str(LAYOUT_TREE_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    return proc.stdout


@pytest.fixture(scope="module")
def layout_run_2_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(LAYOUT_TREE_CHECK)], cwd=str(LAYOUT_TREE_CHECK.parent),
        capture_output=True, text=True, timeout=30,
    )
    return proc.stdout


def _names(data: dict) -> dict:
    return {c["name"]: c["pass"] for c in data["checks"]}


# --------------------------------------------------------------------------- determinism
def test_layout_checks_are_deterministic_across_process_runs(layout_run_1_raw, layout_run_2_raw):
    """Two independent `node` process invocations must print byte-identical JSON --
    the tree operations must not depend on any process-lifetime state leaking into
    output ordering (e.g. Map/Set iteration, Date.now(), Math.random()). Fails
    against an implementation that, for example, generates ids from a global counter
    seeded from wall-clock time instead of a simple in-process increment reset per
    module load.
    """
    assert layout_run_1_raw == layout_run_2_raw, (
        "layout_tree_check.mjs produced different output across two independent "
        "process runs -- the split-tree operations are not deterministic"
    )


# --------------------------------------------------------------------------- split
def test_split_leaf_creates_two_children_and_keeps_the_original_leaf(layout_data):
    """Fails against an implementation that discards or renames the original leaf
    instead of keeping it (with its own id and panelId) as one of the new split's two
    children -- see layout_tree_check.mjs's own docstring, split.* group.
    """
    names = _names(layout_data)
    assert names["split.resultHasThreeLeaves"] is True
    assert names["split.originalLeafSurvivesWithSamePanelId"] is True
    assert names["split.newLeafHasRequestedPanelId"] is True
    assert names["split.resultIsAValidTree"] is True
    assert names["split.autoGeneratedIdsUseThePlainPrefixedCounterScheme"] is True


# --------------------------------------------------------------------------- close
def test_close_pane_returns_sibling_unchanged(layout_data):
    """Fails against an implementation that removes the closed leaf but leaves its
    parent split node in place (a split with only one child left), or that closes the
    wrong side of the split -- see layout_tree_check.mjs's close.* group. The
    surviving sibling must be deep-equal to the exact leaf object it was before the
    sibling was ever split off, proving the sibling "takes the space" rather than
    being rebuilt.
    """
    names = _names(layout_data)
    assert names["close.returnsToTwoLeaves"] is True
    assert names["close.survivingSiblingIsUnchanged"] is True
    assert names["close.rootStructureMatchesOriginalBase"] is True


def test_close_last_pane_is_rejected(layout_data):
    """Fails against an implementation with no guard for closing the tree's only
    remaining leaf (it would return an empty/null tree, leaving nothing to render).
    """
    names = _names(layout_data)
    assert names["close.rejectsLastPane"] is True
    assert names["close.rejectsLastPaneWithLayoutValidationError"] is True


# --------------------------------------------------------------------------- resize
def test_resize_updates_ratio_and_clamps_out_of_range_values(layout_data):
    """Fails against an implementation with no clamping on resizeSplit -- a pane
    could be resized to (or past) zero or negative width. Never loosen the clamp
    bounds asserted here to make an unclamped implementation pass; 0.05/0.95 are
    split_tree.js's own documented MIN_RATIO/MAX_RATIO.
    """
    names = _names(layout_data)
    assert names["resize.updatesExactRatio"] is True
    assert names["resize.clampsAboveMax"] is True
    assert names["resize.clampsBelowMin"] is True
    assert names["resize.unknownSplitIdThrows"] is True


# ------------------------------------------------------------------ collapse / restore
def test_collapse_to_rail_and_restore_preserve_the_split_ratio(layout_data):
    """The load-bearing claim for "collapse to a rail" (question 161): collapsing and
    restoring a pane must return its ancestor split to the *exact* ratio it had
    before -- fails against an implementation that resets the ratio (e.g. to 0.5 or
    to a hardcoded rail-adjacent value) as a side effect of collapsing, which would
    mean a user's careful resize is lost every time they collapse and restore a pane.
    """
    names = _names(layout_data)
    assert names["collapse.setsCollapsedTrue"] is True
    assert names["collapse.preservesAncestorRatio"] is True
    assert names["collapse.restoreSetsCollapsedFalse"] is True
    assert names["collapse.restorePreservesAncestorRatioExactly"] is True
    assert names["collapse.restoreReproducesPreCollapseTreeExactly"] is True


# --------------------------------------------------------------------- serialize round trip
def test_serialize_deserialize_round_trip_is_identical(layout_data):
    """The required test itself (this task's brief: "a serialize -> deserialize round
    trip that returns an identical tree"). Uses a tree with mixed row/column splits
    and a collapsed leaf, not just the trivial two-leaf case, so a field that only
    breaks under nesting (or a `collapsed: false` that gets dropped by an
    over-eager "omit falsy defaults" serializer) would be caught.
    """
    names = _names(layout_data)
    assert names["roundtrip.deserializedEqualsOriginalByValue"] is True
    assert names["roundtrip.reserializedJsonIsByteIdentical"] is True
    assert names["roundtrip.collapsedFlagSurvives"] is True


# ------------------------------------------------------------------- invalid rejection
INVALID_CASE_NAMES = [
    "malformedJsonText", "missingChildrenOnSplit", "oneChildInsteadOfTwo",
    "duplicateIds", "ratioOutOfRange", "ratioZero", "unknownNodeType",
    "unknownDirection", "leafMissingPanelId", "nullTree",
]


@pytest.mark.parametrize("case", INVALID_CASE_NAMES)
def test_invalid_layout_json_is_rejected_with_a_visible_error(layout_data, case):
    """The required test itself (this task's brief: "an invalid layout JSON is
    rejected with a visible error, not silently replaced by a default"). Fails
    against an implementation that catches its own parse/validation failure
    internally and quietly returns a default tree instead of throwing
    LayoutValidationError -- the exact silent-fallback failure mode the brief warns
    would "hide a corrupt saved layout forever". Each parametrized case is a
    distinct, independently-triggerable malformed payload -- see
    layout_tree_check.mjs's badCases list for the exact JSON of each.
    """
    names = _names(layout_data)
    assert names[f"invalidRejected.{case}"] is True, (
        f"deserializeLayout did not reject the '{case}' malformed payload with a "
        f"LayoutValidationError"
    )


# --------------------------------------------------------------------------- persistence
def test_persisted_layout_round_trips(layout_data):
    names = _names(layout_data)
    assert names["persistence.roundTripsAValidSavedLayout"] is True
    assert names["persistence.emptyStorageYieldsDefaultWithNoError"] is True


def test_persisted_corrupt_layout_is_not_silently_replaced(layout_data):
    """The other required test from this task's brief, at the persistence layer
    rather than the pure deserialize layer: loadLayout() must surface a non-null
    error for a corrupt stored layout AND must never overwrite the corrupt string in
    storage on the caller's behalf. Fails against an implementation that "self-heals"
    by calling saveLayout() with a fresh default the moment it notices the stored
    value is bad -- that would permanently destroy the only evidence the corruption
    ever happened, which is exactly the silent-fallback failure mode this task's
    brief prohibits.
    """
    names = _names(layout_data)
    assert names["persistence.corruptStoredLayoutYieldsNonNullError"] is True
    assert names["persistence.corruptStoredLayoutStillReturnsAUsableDefaultTree"] is True
    assert names["persistence.corruptStoredStringIsNeverOverwritten"] is True


# ------------------------------------------------------------------- default layouts
def test_default_layout_for_known_profile_imagery(layout_data):
    """Every profiles/*.yaml file's imagery section is byte-identical today (see
    web/js/layout/REPORT.md) -- this checks the one registered signature resolves to
    the base sidebar+viewport layout. Fails against an implementation that returns an
    arbitrary/different tree for the exact imagery every shipped profile declares.
    """
    names = _names(layout_data)
    assert names["defaultLayout.knownProfileImageryResolvesToBaseLayout"] is True


def test_default_layout_lookup_is_a_real_dispatch_not_a_constant(layout_data):
    """Fails against an implementation of defaultLayoutForImagery() that ignores its
    registry and always returns one hardcoded tree regardless of what was
    registered -- proven by registering a second, distinct imagery signature and
    checking the result actually changes.
    """
    names = _names(layout_data)
    assert names["defaultLayout.unrecognizedImageryFallsBackToAValidLayoutRatherThanThrowing"] is True
    assert names["defaultLayout.registeringANewSignatureActuallyChangesTheResult"] is True


def test_assign_panel_swaps_a_leafs_content_and_displaces_any_prior_owner(layout_data):
    """Question 167's decision, verbatim: "a pane header menu can swap a pane's
    panel." `assignPanel()` (web/js/layout/split_tree.js) is the tree-edit half of
    both the chooser and the header-menu swap. Fails against an implementation that
    (a) does not actually change the target leaf's panelId, or (b) -- the real,
    specific hazard this checks for -- moves an already-placed SINGLETON panel (e.g.
    the sidebar) onto a new leaf without clearing it off its previous one, which would
    leave two leaves claiming the same panelId; `LayoutManager.render()` would then
    silently attach the one real DOM element to whichever leaf happens to render last,
    leaving the other blank with no chooser and no visible explanation.
    """
    names = _names(layout_data)
    for name in (
        "assignPanel.setsTheTargetLeafsPanelId",
        "assignPanel.leavesOtherLeavesAlone",
        "assignPanel.movingAnAlreadyPlacedSingletonKeepsPanelIdsUnique",
        "assignPanel.displacedLeafGetsAFreshEmptyPlaceholderNotTheOldPanelId",
        "assignPanel.targetLeafActuallyGotTheMovedPanel",
        "assignPanel.unknownLeafIdThrowsLayoutValidationError",
    ):
        assert names[name] is True, f"{name} failed"


def test_available_panel_choices_is_a_real_registry_not_a_fixed_list(layout_data):
    """Question 167's decision, verbatim: "every empty pane shows a chooser listing
    the registered panel types." Fails against a chooser hardcoded to a fixed list
    (would not shrink when a singleton type is already placed elsewhere) or one that
    forgets a registered type entirely.
    """
    names = _names(layout_data)
    for name in (
        "availablePanelChoices.factoryTypeAlwaysOffered",
        "availablePanelChoices.singletonAlreadyUsedByANOTHERLeafIsNotOffered",
        "availablePanelChoices.singletonUsedByTHISSAMELeafIsStillOffered(notANoOpBlock)",
        "availablePanelChoices.everyRegisteredTypeAccountedFor",
    ):
        assert names[name] is True, f"{name} failed"


# ---------------------------------------------------------------- F5.1 (question 197)
def test_sweep_carrying_scenario_gets_a_sweep_shaped_default_layout(layout_data):
    """Question 197's own required text: "when the selected scenario carries a `sweep`
    key ... and the layout is unmodified, the default layout is a sweep-shaped one:
    sidebar, the feasibility panel given the wide/primary share, run products, and
    console." Fails against a `hasSweep` that never detects the real fixture study's
    `sweep` key, a `buildSweepStudyLayout` missing the feasibility leaf (or carrying an
    extra map leaf that does not belong in this shape), or a `defaultLayoutTreeForScenario`
    that does not actually dispatch to it for a sweep-carrying scenario -- see
    layout_tree_check.mjs's own F5.1 section for the exact fixture-driven assertions.
    """
    names = _names(layout_data)
    for name in (
        "hasSweep.trueForAScenarioCarryingTheRealFixtureStudysSweepKey",
        "hasSweep.falseForAnOrdinaryScenarioWithNoSweepKey",
        "hasSweep.falseForNullScenario",
        "hasSweep.falseForAMalformedNonObjectSweepValue",
        "buildSweepStudyLayout.exactlyFourLeavesSidebarFeasibilityRunProductsConsole",
        "buildSweepStudyLayout.hasNoMapLeaf(thisIsNotAttachM264PanelsPlusFeasibility)",
        "buildSweepStudyLayout.isAValidTree",
        "defaultLayoutTreeForScenario.selectingTheFixtureStudyYieldsALeafForTheFeasibilityPanel",
        "defaultLayoutTreeForScenario.sweepScenarioResolvesToExactlyBuildSweepStudyLayout",
        "defaultLayoutTreeForScenario.nullScenarioDegradesToTheOrdinaryDefaultRatherThanThrowing",
        "defaultLayoutTreeForScenario.sweepTakesPrecedenceOverAnRicFrameWhenBothAreSomehowPresent(pinnedDispatchOrder)",
    ):
        assert names[name] is True, f"{name} failed"


def test_ordinary_scenario_default_layout_is_unregressed_by_the_sweep_default(layout_data):
    """The other half of question 197's requirement: a scenario with no `sweep` key
    must still resolve to EXACTLY what `attachM264Panels(defaultLayoutForScenario(sc))`
    computed before this task -- no feasibility leaf, byte-identical tree. Fails against
    an implementation that widens the sweep dispatch to also catch ordinary scenarios,
    or that changes `attachM264Panels`/`defaultLayoutForScenario` themselves (which
    web/js/panels_check.mjs's own "attachM264Panels:" checks separately guard).
    """
    names = _names(layout_data)
    assert names["defaultLayoutTreeForScenario.ordinaryNoSweepScenarioNeverGetsTheFeasibilityLeaf"] is True
    assert names["defaultLayoutTreeForScenario.ordinaryScenarioIsByteIdenticalToAttachM264PanelsOfDefaultLayoutForScenario(noRegression)"] is True


def test_customized_or_persisted_layout_is_never_replaced_by_the_sweep_default(layout_data):
    """"A user who has arranged or persisted their own layout must NOT get their layout
    replaced when they select a sweep scenario" (this task's own brief, restating the
    pre-existing `_userHasCustomized` guard's own purpose, unweakened by this task).
    Fails against an `applyDefaultForScenario` that drops or bypasses that guard --
    verified here by actually constructing a `LayoutManager` (a minimal hand-rolled
    fake `document`, layout_tree_check.mjs's own convention -- no jsdom dependency)
    with a real PERSISTED layout already in storage, then handing it a sweep-carrying
    scenario and asserting its tree is completely unchanged.
    """
    names = _names(layout_data)
    assert names["layoutManager.freshInstanceIsNotCustomized"] is True
    assert names["layoutManager.applyDefaultForScenarioAppliesTheSweepLayoutForAnUnmodifiedLayout"] is True
    assert names["layoutManager.constructorLoadingAPersistedLayoutMarksItCustomized"] is True
    assert names["layoutManager.customizedGuardBlocksTheSweepDefaultFromReplacingAPersistedLayout"] is True


def test_layout_report(layout_data, capsys):
    """Not a correctness assertion -- prints the full checks list so `pytest -q -s`
    (or any CI log) carries every individual check's pass/fail, per this task's
    standing review requirement ("I will list the test functions actually present
    against the ones this brief required").
    """
    with capsys.disabled():
        print("\nM26.2 windowing core checks (web/js/layout/layout_tree_check.mjs):")
        for c in layout_data["checks"]:
            mark = "PASS" if c["pass"] else "FAIL"
            print(f"  [{mark}] {c['name']}")
        print(f"  allPass = {layout_data['allPass']} ({len(layout_data['checks'])} checks)")
        assert layout_data["allPass"] is True

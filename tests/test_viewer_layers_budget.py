"""Round 4 (docs/heavy-plan.md, docs/open-questions.md question 228 -- the lead's own
browser drive): `LayerManager`'s memory budget becomes a HARD admission limit instead
of a post-hoc eviction target. The lead measured, in a real browser, a 40 MB
`memoryBudgetBytes` against a real 3,147,060-byte-per-tile imagery set reaching a 96 MB
resident set -- because `update()` admitted a load for every wanted request with no
reference to the budget at all, and `_evictIfNeeded` refuses to evict anything in the
current wanted set, so once the wanted set alone exceeded the budget there was nothing
left it was willing to evict.

Same "run the real code, don't port it" discipline as tests/test_viewer_layers.py: this
file shells out to a real `node` process running `web/js/layers_budget_check.mjs`
(which drives the real `LayerManager` -- see that file's own module docstring for the
full scenario: 32 tiles of 3,147,060 bytes each, 2.40x a 40 MiB budget, spread 1/4/11/16
across levels 0-3) and asserts on the JSON it prints to stdout. Nothing here
reimplements `LayerManager`'s admission/eviction arithmetic.

What each test would catch (this task's standing review requirement -- "for each test,
be able to name the wrong implementation it would fail against"):

* ``test_resident_and_pending_bytes_never_exceed_budget``: an admission check that only
  compares `residentBytes` against the budget (never `residentBytes + pendingBytes`)
  would let two loads admitted back to back, each individually fitting against
  `residentBytes` alone, together resolve into more resident bytes than the budget
  allows -- caught by asserting `maxResidentPlusPendingBytes` (sampled after EVERY
  `update()` and after every individual load settles, not just at the end) never
  exceeds the budget, not merely that the final snapshot happens to.
* ``test_soft_violation_never_fires``: an admission check with an off-by-one, or one
  that only runs `_evictIfNeeded` but never actually gates `update()`'s own admission
  loop, would let `residentBytes` cross the budget and `_evictIfNeeded`'s own
  soft-violation branch would fire (a nonzero `softViolationCount`) -- this is the
  precise, provable form of "the budget is never crossed" H5 originally claimed and
  round 4 exists to make actually true (see layer.js's own module docstring and
  `_evictIfNeeded`'s doc comment on why this is a tripwire, not a mechanism).
* ``test_budget_deferral_actually_happened``: a harness (or an implementation) that
  never actually drives the manager over budget would report `deferredCount == 0`,
  which would mean this whole check proved nothing about the fix -- see this task's
  own binding rule, "a check that cannot fail is this track's own recorded defect
  shape" (round 3 defects 1, 2 and 3).
* ``test_steady_state_is_coarse_tiles_not_fine_ones``: an admission order that walks
  `comparePriority` (raw screen-space error) instead of `compareAdmission` (coarser
  levels first) would let the finest, highest-sseError level-3 tiles fill the entire
  budget first, leaving the single level-0 root tile -- the one tile that covers the
  whole view -- deferred forever; this test reads the harness's own
  `coarseBeforeFineOk` (see that file's own `coarseBeforeFineHolds` for the precise
  rule it checks) and the level histogram directly, and would fail if any level-3 tile
  were ever resident while level 0/1/2 were not yet fully saturated.
* ``test_camera_move_does_not_deadlock``: a hard admission limit with no
  make-room-before-admission eviction step would deadlock the instant the wanted set
  changes (phase 2's own "camera move" to a disjoint 32-tile set) -- the budget stays
  entirely full of the now-unwanted phase-1 tiles and nothing new can ever be admitted,
  which would show up here as `phase2CameraMove.settled == False` (the safety cap was
  hit, not a real fixed point) or `phase1TilesStillResidentAfterPhase2 > 0` (the old,
  unwanted tiles were never evicted to make room for the new ones).
* ``test_byte_totals_match_the_leads_measured_scenario``: a harness that quietly
  changed the tile count, per-tile byte cost, or budget while iterating would no longer
  be driving the lead's own literal measured case (2.40x a 40 MiB budget) -- pinned
  directly against the numbers question 228 and this task's own brief state.

Manager review of this task's own round-4 admission fix found a SECOND, real defect:
a resident entry's `byteCost` was captured once, at admission (`_onLoaded`), and never
reconciled if its OWN layer later declared a different cost for the same key --
exactly the window `GatewayImageryLayerAdapter.fetchManifest()` opens (round 4's own
manifest fix charges a fallback estimate until the manifest resolves, then the tile's
real, usually much larger, manifest-declared size). Measured directly: 20 tiles
admitted at a 262,144-byte estimate (5,242,880 bytes accounted), true cost 3,147,060
bytes each (62,941,200 bytes, 50% over a 41,943,040-byte budget) -- `residentBytes`
stayed at 5,242,880 and `softViolationCount` stayed 0 throughout. Fixed by a
reconciliation pass in `update()` (see layer.js's own comment, immediately after
cancellation) and proven by `web/js/layers_budget_check.mjs`'s own phase 3 (its own
isolated manager, never mixed with phase 1/2's):

* ``test_invariant_holds_at_every_sample_point``: the independent check (never reads
  `residentBytes`/`pendingBytes` back off themselves -- recomputes the truth by
  calling `layer.plan(view)` again and summing the freshly-planned cost for whichever
  globalKeys are currently resident/pending) that would have caught the manager
  review's own defect directly, without already knowing what to look for -- see
  `web/js/layers_budget_check.mjs`'s own `invariantCheck`/`trueBytesFromFreshPlan`.
* ``test_reconciliation_recoverable_case_absorbs_cleanly``: phase 3a (10 tiles, whose
  revised total still fits the budget) -- proves the reconciliation arithmetic itself
  is exact: `residentBytes` changes by EXACTLY the declared delta, with no eviction
  and no soft violation.
* ``test_reconciliation_irreconcilable_case_is_reported_honestly``: phase 3b (20
  tiles, the manager review's own exact probe shape) -- proves the fix does not
  pretend an impossible budget is possible: `residentBytes` becomes EXACTLY the true
  sum (not the stale estimate), the revision is counted, and `softViolationCount`
  becomes nonzero -- an honest report of an unavoidable overage, not a bug.

See ``web/js/layers_budget_check.mjs``'s own module docstring for the "teeth" proof
against the UNFIXED `layer.js` (git blob ``8d2146c5d7e50f7aae1a5a0ce19508f0010b6e6a``):
run over the identical 32-tile/40 MiB scenario, the unfixed manager admitted all 32
tiles (100,705,920 resident bytes, 2.40x the budget, matching the lead's own ~96 MB/40
MB measurement) with ``softViolationCount`` 38 and ``evictedCount`` 0 -- recorded in
this task's own report, not reproduced as an automated test here (the unfixed file is
not part of this repository's working tree; reproducing that run as a pytest fixture
would mean shipping a second, stale copy of layer.js purely to keep failing). The
SAME applies to the byteCost-reconciliation defect above: recorded in this task's own
report (`residentMatches: False`, discrepancy -57,698,320 bytes, reproduced directly
against this repository's own working tree by temporarily disabling only the
reconciliation pass), not reproduced as an automated test here for the identical
reason.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
BUDGET_CHECK = REPO_ROOT / "web" / "js" / "layers_budget_check.mjs"

NODE = shutil.which("node")

TILE_BYTES = 3_147_060
MEMORY_BUDGET_BYTES = 40 * 1024 * 1024  # 41,943,040 -- the lead's own measured budget
TOTAL_TILE_COUNT = 32
LEVEL_SHAPE = [
    {"level": 0, "count": 1},
    {"level": 1, "count": 4},
    {"level": 2, "count": 11},
    {"level": 3, "count": 16},
]


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/layers/ is ES modules "
            "and this test intentionally runs the real module for real (see module "
            "docstring) rather than porting LayerManager's arithmetic to Python, so it "
            "cannot proceed without node. Install node to run it."
        )
    return NODE


def _run_node_raw() -> str:
    node = _require_node()
    proc = subprocess.run(
        [node, str(BUDGET_CHECK)],
        cwd=str(BUDGET_CHECK.parent),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert proc.returncode == 0, (
        f"node {BUDGET_CHECK.name} exited {proc.returncode}\n"
        f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    return proc.stdout


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def budget_run_1_raw() -> str:
    return _run_node_raw()


@pytest.fixture(scope="module")
def budget_run_2_raw() -> str:
    return _run_node_raw()


@pytest.fixture(scope="module")
def budget_data(budget_run_1_raw: str) -> dict:
    try:
        return json.loads(budget_run_1_raw)
    except json.JSONDecodeError:
        raise AssertionError(f"{BUDGET_CHECK.name} did not print valid JSON: {budget_run_1_raw!r}")


# ------------------------------------------------------------------------ determinism
def test_budget_check_is_deterministic_across_process_runs(budget_run_1_raw, budget_run_2_raw):
    """Two independent `node` process invocations, over the same fixed scenario, must
    print byte-identical JSON -- an implementation whose admission order depends on
    Map/Set insertion order or a non-total comparator would not.
    """
    assert budget_run_1_raw == budget_run_2_raw, (
        "web/js/layers_budget_check.mjs produced different output across two "
        "independent process runs over the same fixed scenario"
    )


# ------------------------------------------------------------ the lead's own scenario
def test_byte_totals_match_the_leads_measured_scenario(budget_data):
    scenario = budget_data["scenario"]
    assert scenario["tileBytes"] == TILE_BYTES
    assert scenario["memoryBudgetBytes"] == MEMORY_BUDGET_BYTES
    assert scenario["totalTileCount"] == TOTAL_TILE_COUNT
    assert scenario["levelShape"] == LEVEL_SHAPE
    assert scenario["totalWantedBytes"] == TOTAL_TILE_COUNT * TILE_BYTES == 100_705_920
    # "a wanted set 2.40 times the budget" -- this task's own brief, stated exactly.
    assert scenario["wantedOverBudgetRatio"] == pytest.approx(2.40, abs=0.005)


# ---------------------------------------------------------------------- the invariant
def test_resident_bytes_never_exceed_budget(budget_data):
    assert budget_data["maxResidentBytes"] <= MEMORY_BUDGET_BYTES, (
        f"maxResidentBytes {budget_data['maxResidentBytes']} exceeded the budget "
        f"{MEMORY_BUDGET_BYTES} at {budget_data['maxResidentBytesAt']} -- the hard "
        f"admission limit did not hold"
    )
    # Sampled after every update() AND after every load settles (see the harness's own
    # module docstring) -- both phase snapshots must independently hold too, not just
    # the combined maximum.
    assert budget_data["phase1"]["maxResidentBytes"] <= MEMORY_BUDGET_BYTES
    assert budget_data["phase2CameraMove"]["maxResidentBytes"] <= MEMORY_BUDGET_BYTES


def test_resident_and_pending_bytes_never_exceed_budget(budget_data):
    """The invariant `residentBytes + pendingBytes <= memoryBudgetBytes` (layer.js's
    own `update()` comment, "THE INVARIANT") -- checked separately from
    `residentBytes` alone because `pendingBytes` is what makes the invariant hold
    across the async gap between admission and settlement; an implementation that
    checked only `residentBytes` at admission time could admit two loads back to back
    that, once both settle, resolve into more resident bytes than the budget allows.
    """
    assert budget_data["maxResidentPlusPendingBytes"] <= MEMORY_BUDGET_BYTES, (
        f"maxResidentPlusPendingBytes {budget_data['maxResidentPlusPendingBytes']} "
        f"exceeded the budget {MEMORY_BUDGET_BYTES} at "
        f"{budget_data['maxResidentPlusPendingBytesAt']}"
    )
    assert budget_data["phase1"]["maxResidentPlusPendingBytes"] <= MEMORY_BUDGET_BYTES
    assert budget_data["phase2CameraMove"]["maxResidentPlusPendingBytes"] <= MEMORY_BUDGET_BYTES


def test_soft_violation_never_fires(budget_data):
    """`softViolationCount` must be exactly 0 -- see `_evictIfNeeded`'s own doc comment
    in layer.js: under the round-4 invariant this branch is unreachable BY
    CONSTRUCTION, not merely unlikely, so this is the provable form of "the budget is
    never crossed" (H5's original, now-true claim).
    """
    assert budget_data["softViolationCount"] == 0, (
        f"expected _evictIfNeeded's soft-violation branch to never fire (it is "
        f"unreachable under the round-4 admission invariant); got "
        f"{budget_data['softViolationCount']}"
    )
    assert budget_data["phase1"]["softViolationCount"] == 0
    assert budget_data["phase2CameraMove"]["softViolationCount"] == 0


def test_budget_deferral_actually_happened(budget_data):
    """`deferredCount > 0` proves this harness genuinely drove the manager over
    budget -- a run that never deferred anything would mean the budget was never
    actually exercised, and `softViolationCount == 0`/`maxResidentBytes <= budget`
    would be true for the wrong reason (this task's own binding rule: "a check that
    cannot fail proves nothing" -- round 3 defects 1, 2 and 3 were exactly this shape).
    """
    assert budget_data["deferredCount"] > 0, (
        "expected at least one budget deferral over this run -- a wanted set 2.40x "
        "the budget that deferred nothing would mean this check never actually "
        "exercised the hard admission limit"
    )
    assert budget_data["phase1"]["deferredCount"] > 0, (
        "expected phase 1 alone (the lead's own literal scenario) to have deferred "
        "at least one request"
    )
    assert budget_data["phase2CameraMove"]["deferredCount"] > 0
    assert budget_data["lastStepDeferred"] >= 0  # always defined; 0 is valid for the LAST step specifically


# --------------------------------------------------------------- coarse-before-fine
def test_steady_state_is_coarse_tiles_not_fine_ones(budget_data):
    """The resident steady state must be exactly the coarse end of the wanted set --
    see web/js/layers_budget_check.mjs's own `coarseBeforeFineHolds` for the precise
    rule: walking levels ascending, once a level is found that is not fully resident,
    every finer level must have zero resident tiles. A implementation that admitted by
    `comparePriority` (raw screen-space error, finer-is-higher in this harness's own
    deliberately adversarial sseError assignment -- see that file's module docstring)
    instead of `compareAdmission` (coarser levels first) would fail this: the finest
    level-3 tiles would fill the budget first and the level-0 root tile -- the one
    tile that makes the view show ANYTHING at all -- would never be admitted.
    """
    for phase_key in ("phase1", "phase2CameraMove"):
        phase = budget_data[phase_key]
        assert phase["coarseBeforeFineOk"] is True, (
            f"{phase_key}: a finer-level tile was resident while a coarser-level tile "
            f"from the same wanted set was not -- histogram: {phase['levelHistogram']}"
        )
        histogram = {row["level"]: row for row in phase["levelHistogram"]}
        # The two coarsest levels (0 and 1) must be FULLY saturated -- with a 40 MiB
        # budget and 3,147,060-byte tiles, 1 + 4 = 5 tiles (15,735,300 bytes) is
        # comfortably affordable, so a correct admission order has no excuse to leave
        # either partial.
        assert histogram[0]["resident"] == histogram[0]["wanted"] == 1, (
            f"{phase_key}: the single level-0 root tile must be resident (covers the "
            f"whole view) -- got {histogram[0]}"
        )
        assert histogram[1]["resident"] == histogram[1]["wanted"] == 4, (
            f"{phase_key}: all 4 level-1 tiles must be resident -- got {histogram[1]}"
        )
        # No level-3 (finest) tile may EVER be resident in this scenario: even level 0
        # + 1 + 2 in full (1+4+11=16 tiles, 50,353,260 bytes) exceeds the 41,943,040-byte
        # budget, so level 2 itself cannot even fully saturate, let alone level 3.
        assert histogram[3]["resident"] == 0, (
            f"{phase_key}: expected zero level-3 (finest) tiles resident -- a fine "
            f"tile displaced a coarse one; got {histogram[3]}"
        )
        # Level 2 (the level at which the budget is exhausted) is resident for SOME
        # but not all of its own tiles -- partial saturation is exactly what "the
        # budget is exhausted here" looks like.
        assert 0 < histogram[2]["resident"] < histogram[2]["wanted"], (
            f"{phase_key}: expected level 2 to be the level at which the budget runs "
            f"out (partially, not fully, resident) -- got {histogram[2]}"
        )
        # 1 + 4 + 8 = 13 tiles is exactly floor(41,943,040 / 3,147,060) -- the largest
        # number of uniform 3,147,060-byte tiles that fit the budget at all.
        resident_total = sum(row["resident"] for row in phase["levelHistogram"])
        assert resident_total == 13 == (MEMORY_BUDGET_BYTES // TILE_BYTES)


# ------------------------------------------------------------------------ no deadlock
def test_camera_move_does_not_deadlock(budget_data):
    """Deliverable 3 (deadlock avoidance): once phase 1 settles, the harness swaps in
    an entirely disjoint 32-tile wanted set (a "camera move") -- every phase-1 tile is
    now unwanted. A hard admission limit with no make-room-before-admission eviction
    step would deadlock here: the budget stays entirely full of the now-unwanted
    phase-1 tiles and nothing new can ever be admitted. This test is the concrete,
    countable proof that does not happen.
    """
    phase2 = budget_data["phase2CameraMove"]
    assert phase2["settled"] is True, (
        "expected phase 2 to reach a genuine fixed point (not the harness's own "
        "MAX_ITERATIONS safety cap) -- a hard admission limit with no "
        "make-room-before-admission eviction step would never converge here"
    )
    assert phase2["evictedCount"] > 0, (
        "expected at least one now-unwanted phase-1 tile to have been evicted to make "
        "room for phase 2's own admissions"
    )
    assert phase2["phase1TilesStillResidentAfterPhase2"] == 0, (
        f"expected every phase-1 tile to have been evicted by the time phase 2 "
        f"settled (none of them are wanted any more); got "
        f"{phase2['phase1TilesStillResidentAfterPhase2']} still resident -- the "
        f"manager did not fully recover the budget for the new view"
    )
    # The new steady state is exactly as coarse-first-saturated as phase 1's own --
    # symmetric scenario, symmetric outcome.
    resident_total = sum(row["resident"] for row in phase2["levelHistogram"])
    assert resident_total == 13


# ------------------------------------------------------------------------- counts
def test_counts_are_reported_unconditionally(budget_data):
    """Distinct tiles ever loaded/cancelled/evicted/failed, reported unconditionally
    -- see web/js/layers_check.mjs's own identical discipline for softViolationCount/
    failedCount. This harness's own scenario never rejects a load and never lets a
    request outlive the static wanted set it was admitted under mid-phase, so
    cancelledCount/failedCount are expected exactly 0 -- asserted directly (not
    merely printed), so a future change to the harness that accidentally started
    exercising either path would be caught here, not silently absorbed.
    """
    # 13 tiles fit in phase 1, all 13 are evicted for phase 2, and phase 2 admits its
    # own 13 -- 26 distinct successful loads in total.
    assert budget_data["loadedCount"] == 26
    assert budget_data["invocationCount"] == 26
    assert budget_data["cancelledCount"] == 0
    assert budget_data["evictedCount"] == 13
    assert budget_data["failedCount"] == 0
    assert budget_data["failureNames"] == []


# --------------------------------------------------- byteCost reconciliation (manager review)
RECONCILE_ESTIMATE_BYTES = 262_144
RECONCILE_TRUE_BYTES = TILE_BYTES  # 3,147,060


def test_invariant_holds_at_every_sample_point(budget_data):
    """The independent invariant check (web/js/layers_budget_check.mjs's own
    `invariantCheck`/`trueBytesFromFreshPlan`) must hold at every phase-1/phase-2
    sample AND at every phase-3 snapshot -- see this module's own docstring for why
    this check is genuinely independent (it recomputes truth by calling `plan()`
    again, never by re-reading `resident`/`pending`'s own stored `byteCost`) and what
    a `False` here would mean: `residentBytes`/`pendingBytes` disagreeing with what
    the layer would charge those same keys right now.
    """
    assert budget_data["invariantHeldEveryStep"] is True, (
        "expected the independent invariant check to hold at every phase-1/phase-2 "
        "sample point"
    )
    assert budget_data["phase1"]["invariantHeldEveryStep"] is True
    assert budget_data["phase2CameraMove"]["invariantHeldEveryStep"] is True
    for phase_key in ("recoverable", "irreconcilable"):
        phase = budget_data["phase3Reconcile"][phase_key]
        for snapshot_key in ("before", "afterOneUpdate", "after"):
            inv = phase[snapshot_key]["invariant"]
            assert inv["residentMatches"] is True, (
                f"phase3Reconcile.{phase_key}.{snapshot_key}: residentBytes disagreed "
                f"with the freshly-planned truth by {inv['residentDiscrepancyBytes']} "
                f"bytes -- a resident entry's stored byteCost went stale relative to "
                f"what its layer currently plans for it"
            )
            assert inv["residentDiscrepancyBytes"] == 0
            assert inv["pendingMatches"] is True
            assert inv["pendingDiscrepancyBytes"] == 0


def test_reconciliation_recoverable_case_absorbs_cleanly(budget_data):
    """Phase 3a: 10 tiles admitted at the 262,144-byte fallback estimate, then
    flipped to the 3,147,060-byte true cost -- 10 * 3,147,060 = 31,470,600 still fits
    the 41,943,040-byte budget, so reconciliation alone (no eviction) must absorb it
    cleanly. An implementation that reconciled `byteCost` but got the arithmetic
    wrong (summed instead of replaced, applied the delta to the wrong entry, dropped
    a request) would fail the exact-byte assertions below.
    """
    p = budget_data["phase3Reconcile"]["recoverable"]
    assert p["tileCount"] == 10
    assert p["estimateBytes"] == RECONCILE_ESTIMATE_BYTES
    assert p["trueBytes"] == RECONCILE_TRUE_BYTES
    assert p["settledBefore"] is True and p["settledAfter"] is True
    assert p["before"]["residentCount"] == 10
    assert p["before"]["accountedResidentBytes"] == 10 * RECONCILE_ESTIMATE_BYTES == 2_621_440
    assert p["afterOneUpdate"]["accountedResidentBytes"] == 10 * RECONCILE_TRUE_BYTES == 31_470_600, (
        "expected residentBytes to become EXACTLY the true reconciled total after "
        "the single update() call immediately following the cost flip"
    )
    assert p["after"]["accountedResidentBytes"] == 31_470_600
    assert p["after"]["residentCount"] == 10, "no tile should have been evicted -- the revised total fits the budget"
    expected_delta = 10 * (RECONCILE_TRUE_BYTES - RECONCILE_ESTIMATE_BYTES)
    assert p["byteCostRevisionCount"] == 10
    assert p["byteCostRevisionCountDelta"] == 10
    assert p["byteCostRevisionBytes"] == expected_delta == 28_849_160
    assert p["softViolationCountDelta"] == 0, (
        "expected no soft violation -- the revised total fits comfortably under budget"
    )
    assert p["evictedCount"] == 0


def test_reconciliation_irreconcilable_case_is_reported_honestly(budget_data):
    """Phase 3b: 20 tiles admitted at the 262,144-byte fallback estimate (5,242,880
    bytes accounted), then flipped to the 3,147,060-byte true cost -- 20 * 3,147,060
    = 62,941,200 bytes, 50% over the 41,943,040-byte budget, with every one of the 20
    tiles still wanted, so nothing is evictable. This is the manager review's own
    exact probe shape. The fix's job here is not to make an impossible budget
    possible -- it is to stop the manager from lying about it:
      - `residentBytes` must become EXACTLY the true sum (62,941,200), not the stale
        5,242,880 the manager review found (an implementation that reconciled the
        COUNT but not the byte total, or vice versa, fails this).
      - `byteCostRevisionCount`/`byteCostRevisionBytes` must record the revision.
      - `softViolationCount` -- which the manager review's own probe found silently
        reading 0 while the truth was 50% over budget -- must become nonzero.
    """
    p = budget_data["phase3Reconcile"]["irreconcilable"]
    assert p["tileCount"] == 20
    assert p["before"]["accountedResidentBytes"] == 20 * RECONCILE_ESTIMATE_BYTES == 5_242_880, (
        "probe setup invariant: this is the manager review's own exact 'before' number"
    )
    assert p["afterOneUpdate"]["accountedResidentBytes"] == 20 * RECONCILE_TRUE_BYTES == 62_941_200, (
        f"expected residentBytes to become the TRUE reconciled total "
        f"(62,941,200 bytes) once the reconciliation pass ran, not stay stuck at the "
        f"stale estimate (5,242,880 bytes) -- got "
        f"{p['afterOneUpdate']['accountedResidentBytes']}"
    )
    assert p["after"]["accountedResidentBytes"] == 62_941_200
    assert p["after"]["residentCount"] == 20, (
        "every one of the 20 tiles is still wanted -- none may be evicted to make "
        "an impossible budget artificially satisfiable"
    )
    expected_delta = 20 * (RECONCILE_TRUE_BYTES - RECONCILE_ESTIMATE_BYTES)
    assert p["byteCostRevisionCount"] == 20
    assert p["byteCostRevisionCountDelta"] == 20
    assert p["byteCostRevisionBytes"] == expected_delta == 57_698_320
    assert p["softViolationCountDelta"] > 0, (
        "expected _evictIfNeeded's soft-violation tripwire to fire honestly -- "
        "residentBytes is genuinely, unavoidably over budget once the revision is "
        "accounted for correctly, with nothing unwanted left to evict; a "
        "softViolationCountDelta of 0 here would mean the manager is still hiding "
        "the violation, exactly the manager review's own reported defect"
    )
    assert p["evictedCount"] == 0, "nothing was evictable (every tile stays wanted), so nothing should have been evicted"


# ------------------------------------------------------------- LayerManager.removeLayer (round 5, item D)
def test_remove_layer_setup_is_sound(budget_data):
    """Sanity-check phase 4's own setup, before trusting anything `removeLayer`
    itself did: both layers' resident/pending/failed keys must be present exactly as
    the scenario intends, BEFORE `removeLayer` runs. A setup bug here (e.g. a shared
    `view.requests` object letting one layer's plan() see the other's keys, the exact
    bug an earlier draft of this harness had -- see web/js/layers_budget_check.mjs's
    own `makeRemoveLayerProbeLayer` doc comment) would make every assertion below
    pass or fail for the wrong reason.
    """
    before = budget_data["phase4RemoveLayer"]["before"]
    assert before["residentHasR"] is True and before["residentHasOR"] is True
    assert before["pendingHasP"] is True and before["pendingHasOP"] is True
    assert before["failedHasF"] is True and before["failedHasOF"] is True
    assert before["pendingBytes"] == 4000  # P (2000) + OP (2000)
    assert before["residentBytes"] == 2000  # R (1000) + OR (1000)
    assert before["cancelledCount"] == 0 and before["evictedCount"] == 0
    assert before["targetReleased"] == [] and before["otherReleased"] == []
    assert before["layersRegistered"] == ["rl-other", "rl-target"]


def test_remove_layer_aborts_its_own_pending_loads(budget_data):
    """Doc comment claim 1: pending loads belonging to the removed layer are
    aborted, `pendingBytes` is debited by exactly their byteCost, and
    `cancelledCount` moves. A `removeLayer` that only did `this._layers.delete(id)`
    (a plausible naive implementation) would fail every assertion here: 'P' would
    stay in `pending` forever, `pendingBytes` would stay at 4000, and
    `cancelledCount` would stay 0.
    """
    before, after = budget_data["phase4RemoveLayer"]["before"], budget_data["phase4RemoveLayer"]["after"]
    assert after["pendingHasP"] is False, "the removed layer's own pending load ('P') must be gone"
    assert after["pendingBytes"] == before["pendingBytes"] - 2000 == 2000, (
        "pendingBytes must be debited by EXACTLY P's byteCost (2000), leaving only "
        "OP's own 2000"
    )
    assert after["cancelledCount"] == before["cancelledCount"] + 1 == 1


def test_remove_layer_evicts_its_own_resident_entries_through_evict_entry(budget_data):
    """Doc comment claim 2: resident entries belonging to the removed layer are
    evicted through `_evictEntry` -- so the layer's own `release(localKey)` is
    called and `evictedCount` moves, never a bare `Map` delete (which would drop the
    entry from `resident` but never call `release()`, leaking whatever the layer's
    own release() is responsible for freeing -- a GPU texture, an AbortController,
    etc). `targetReleased == ['R']` is the direct proof `release()` was actually
    invoked, with the correct LOCAL key (not the globalKey) -- a naive
    `this.resident.delete(globalKey)` would leave `targetReleased` empty forever.
    """
    before, after = budget_data["phase4RemoveLayer"]["before"], budget_data["phase4RemoveLayer"]["after"]
    assert after["residentHasR"] is False, "the removed layer's own resident entry ('R') must be gone"
    assert after["residentBytes"] == before["residentBytes"] - 1000 == 1000, (
        "residentBytes must be debited by EXACTLY R's byteCost (1000), leaving only "
        "OR's own 1000"
    )
    assert after["evictedCount"] == before["evictedCount"] + 1 == 1
    assert after["targetReleased"] == ["R"], (
        f"expected the removed layer's own release() to have been called with "
        f"EXACTLY its own local key 'R' -- got {after['targetReleased']}"
    )


def test_remove_layer_drops_its_own_failed_blacklist_entries(budget_data):
    """Doc comment claim 3: `_failed`-blacklisted globalKeys belonging to the removed
    layer's id are dropped. Checked two ways: the blacklist Map itself
    (`failedHasF`), and -- the stronger, load-bearing proof -- that a FRESH layer
    registered under the SAME id afterward is actually able to load that same key,
    which only a real blacklist-drop (not just a Map-contents illusion) can produce.
    """
    after = budget_data["phase4RemoveLayer"]["after"]
    assert after["failedHasF"] is False, "the removed layer's own blacklist entry ('F') must be dropped"
    assert budget_data["phase4RemoveLayer"]["freshLayerAdmittedF"] is True, (
        "a FRESH layer registered under the SAME id ('rl-target') afterward must be "
        "able to have 'F' admitted -- a surviving blacklist entry would make "
        "update()'s own `if (this._failed.has(r.globalKey)) continue;` skip it "
        "forever, regardless of what the fresh layer's own load() would have done"
    )


def test_remove_layer_never_touches_another_registered_layer(budget_data):
    """Doc comment claim 4 (the control): none of `removeLayer`'s bookkeeping may
    touch a DIFFERENT registered layer's pending/resident/failed entries.
    `rl-other`'s own 'OR' (resident)/'OP' (pending)/'OF' (failed) must all still be
    exactly as they were, and `otherReleased` must stay empty (`rl-other`'s own
    `release()` must never be called by removing a DIFFERENT layer).
    """
    before, after = budget_data["phase4RemoveLayer"]["before"], budget_data["phase4RemoveLayer"]["after"]
    assert after["residentHasOR"] is True
    assert after["pendingHasOP"] is True
    assert after["failedHasOF"] is True
    assert after["otherReleased"] == before["otherReleased"] == []
    assert after["layersRegistered"] == ["rl-other"], (
        "expected only 'rl-target' to have been removed from the manager's own "
        "registered-layer set"
    )


def test_remove_layer_on_an_unregistered_id_is_a_genuine_noop(budget_data):
    """The doc comment's own explicit clause: "No-op if `id` is not registered." A
    second `removeLayer` call, on an id that was never registered, must leave every
    counter and every layer's own state byte-for-byte identical to right before it.
    """
    after = budget_data["phase4RemoveLayer"]["after"]
    noop = budget_data["phase4RemoveLayer"]["afterNoopOnUnknownId"]
    assert noop == after, (
        f"removeLayer() on an unregistered id changed manager state -- before: "
        f"{after}, after: {noop}"
    )


# ------------------------------------------------------------------------------ report
def test_budget_report(budget_data, capsys):
    """Not a correctness assertion -- prints the measured numbers so `pytest -q -s`
    (or any CI log) carries the real values, matching tests/test_viewer_layers.py's
    own `test_layers_report` convention.
    """
    with capsys.disabled():
        print("\nstreaming layers budget check (web/js/layers_budget_check.mjs):")
        print(f"  scenario: {budget_data['scenario']}")
        print(f"  maxResidentBytes={budget_data['maxResidentBytes']} at {budget_data['maxResidentBytesAt']} "
              f"(budget={MEMORY_BUDGET_BYTES})")
        print(f"  maxResidentPlusPendingBytes={budget_data['maxResidentPlusPendingBytes']} "
              f"at {budget_data['maxResidentPlusPendingBytesAt']}")
        print(f"  softViolationCount={budget_data['softViolationCount']} "
              f"deferredCount={budget_data['deferredCount']} lastStepDeferred={budget_data['lastStepDeferred']}")
        print(f"  byteCostRevisionCount={budget_data['byteCostRevisionCount']} "
              f"byteCostRevisionBytes={budget_data['byteCostRevisionBytes']} "
              f"invariantHeldEveryStep={budget_data['invariantHeldEveryStep']}")
        print(f"  phase1: {budget_data['phase1']}")
        print(f"  phase2CameraMove: {budget_data['phase2CameraMove']}")
        print(f"  phase3Reconcile.recoverable: {budget_data['phase3Reconcile']['recoverable']}")
        print(f"  phase3Reconcile.irreconcilable: {budget_data['phase3Reconcile']['irreconcilable']}")
        print(f"  loadedCount={budget_data['loadedCount']} cancelledCount={budget_data['cancelledCount']} "
              f"evictedCount={budget_data['evictedCount']} failedCount={budget_data['failedCount']}")

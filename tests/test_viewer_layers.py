"""H5a (docs/heavy-plan.md milestone H5, first half): the streaming-layer module
(web/js/layers/) -- a priority queue by screen-space error and view distance, a
declared memory budget in bytes, and cancellation of in-flight requests on a view
move, wrapping the globe's imagery loader, the globe's terrain loader (a typed, named
refusal -- no terrain loader exists in this codebase yet, see
web/js/layers/terrain_layer.js) and the vendored 3DTilesRendererJS overlay behind one
interface (web/js/layers/layer.js).

Same "run the real code, don't port it" discipline as tests/test_viewer_globe.py: this
file shells out to a real `node` process running `web/js/layers_check.mjs` (which
drives the real `LayerManager` and the three real adapters over a fixed, scripted
camera path with injected, network-free loader stubs -- see that file's own module
docstring) and asserts on the JSON it prints to stdout. Nothing here reimplements
LayerManager's priority-sort, byte-budget/eviction, or cancellation logic in Python --
except `test_priority_order_matches_stated_rule` below, which *deliberately*
recomputes the expected order independently in Python from the raw per-request
sse/distance/key numbers the harness prints, specifically so this test cannot be
fooled by a bug in the JS comparator itself (re-reading `orderedKeys` and asserting it
equals itself would prove nothing).

What each test would catch (this task's standing review requirement -- "for each
test, be able to name the wrong implementation it would fail against"):

* ``test_layers_check_is_deterministic_across_process_runs``: an implementation whose
  merged, cross-layer priority order depends on `Map`/`Set` insertion order, object
  property enumeration order, or a non-total comparator (two requests that compare
  "equal" under `comparePriority` without being the literal same request) -- would
  print a different order (or the same order with request objects swapped) on a
  second independent process run over the same fixed camera path.
* ``test_byte_budget_is_respected_and_eviction_actually_ran``: a manager with no
  eviction logic at all (`maxResidentBytesObserved` would climb unbounded past
  ``memoryBudgetBytes``), *or* one that copies `globe_lod.js`'s `TileLoadScheduler`
  and evicts by resident *item count* instead of resident *byte total* -- with three
  layers whose per-item byte costs differ by two orders of magnitude (a 256x256 RGBA
  imagery tile at 262,144 bytes vs. a glTF-bearing 3D tile at 2,621,440 bytes, see
  web/js/layers/imagery_layer.js and web/js/layers/tiles3d_layer.js), an item-count
  budget would let the resident byte total run far past any byte cap while still
  reporting a "budget respected" item count. See web/js/layers_check.mjs's module
  docstring for exactly how ``MEMORY_BUDGET_BYTES`` was sized (measured, not
  estimated) so this test cannot pass merely because the budget was never approached.
* ``test_cancellation_on_view_jump``: a manager that queues every requested load and
  never cancels a stale one once its request falls out of the current plan -- the
  fixed camera path's 'close-sw-2' -> 'jump-ne' step is a deliberate jump to the
  diagonally-opposite corner of the 3D Tiles fixture's small footprint before the
  previous step's pending loads have all "completed" (a bounded per-step completion
  count, see the harness), so a real cancellation must happen.
* ``test_priority_order_matches_stated_rule``: a comparator that does not actually
  implement "descending screen-space error, ties broken by ascending view distance,
  ties broken by ascending key" -- e.g. one that sorts by distance first, or that
  breaks ties by insertion order instead of by key. This test recomputes the expected
  order in Python from each request's raw `sseError`/`viewDistanceM`/`globalKey`
  (never reading `orderedKeys` to derive its own expectation), so a bug in the JS
  comparator itself cannot pass by construction.
* ``test_all_three_layers_share_one_manager_interface``: an adapter whose `plan()`
  method itself starts a load (eagerly fetching instead of only declaring demand, so
  `LayerManager` would not actually own the priority queue/budget/cancellation for
  that layer) -- caught by ``planNeverLoadsDirectly``; and a manager whose resident
  bookkeeping is fragmented per-layer instead of unified, so a caller would need to
  know which layer produced a given item before it could look up the loaded payload
  -- caught by ``residentPayloadLookupWorks``, which exercises
  ``LayerManager.getResidentPayload()`` directly and would fail if that single
  lookup method did not work regardless of which layer's item it names.
* ``test_terrain_adapter_is_a_typed_named_refusal_not_a_silent_stub``: a terrain
  adapter that silently resolves with a fabricated payload (a silent stub) instead of
  rejecting with a specifically-named error type -- this test pins the exact string
  ``"TerrainLoaderNotImplementedError"``, not merely "some error was thrown", so a
  refactor that renames the class without updating this test is itself caught (the
  binding rule: "an exit code is not evidence, and a gap is recorded, never hidden").
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
LAYERS_CHECK = REPO_ROOT / "web" / "js" / "layers_check.mjs"

NODE = shutil.which("node")


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
        [node, str(LAYERS_CHECK)],
        cwd=str(LAYERS_CHECK.parent),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert proc.returncode == 0, (
        f"node {LAYERS_CHECK.name} exited {proc.returncode}\n"
        f"stdout: {proc.stdout}\nstderr: {proc.stderr}"
    )
    return proc.stdout


# --------------------------------------------------------------------------- fixtures
@pytest.fixture(scope="module")
def layers_run_1_raw() -> str:
    return _run_node_raw()


@pytest.fixture(scope="module")
def layers_run_2_raw() -> str:
    return _run_node_raw()


@pytest.fixture(scope="module")
def layers_data(layers_run_1_raw: str) -> dict:
    try:
        return json.loads(layers_run_1_raw)
    except json.JSONDecodeError:
        raise AssertionError(f"{LAYERS_CHECK.name} did not print valid JSON: {layers_run_1_raw!r}")


# ------------------------------------------------------------------------ determinism
def test_layers_check_is_deterministic_across_process_runs(layers_run_1_raw, layers_run_2_raw):
    """Two independent `node` process invocations of layers_check.mjs, over the same
    fixed camera path, must print byte-identical JSON -- see this module's docstring
    for what a failure here would mean.
    """
    assert layers_run_1_raw == layers_run_2_raw, (
        "web/js/layers_check.mjs produced different output across two independent "
        "process runs over the same fixed camera path -- the priority-ordered plan "
        "is not deterministic"
    )
    d = json.loads(layers_run_1_raw)
    assert len(d["steps"]) == 7
    for step in d["steps"]:
        assert step["orderedKeys"] == [r["globalKey"] for r in step["requests"]], (
            "step's orderedKeys must equal requests in printed order -- the harness's "
            "own bookkeeping is internally inconsistent"
        )


# ------------------------------------------------------------------------- byte budget
def test_byte_budget_is_respected_and_eviction_actually_ran(layers_data):
    assert layers_data["budgetRespected"] is True, (
        f"resident byte total exceeded the budget: max observed "
        f"{layers_data['maxResidentBytesObserved']} > budget {layers_data['memoryBudgetBytes']}"
    )
    assert layers_data["maxResidentBytesObserved"] <= layers_data["memoryBudgetBytes"]
    # The budget is only a meaningful test if eviction actually had to run -- see
    # web/js/layers_check.mjs's module docstring ("deliberately sized ... so eviction
    # must run partway through the path").
    assert layers_data["evictedCount"] > 0, (
        "expected LayerManager to have evicted at least one resident item over this "
        "camera path (the path's cumulative distinct-byte total, measured with "
        "eviction disabled, is well over MEMORY_BUDGET_BYTES); evictedCount == 0 "
        "would mean the budget held only because it was never actually exercised"
    )
    # Every step's own snapshot must also individually respect the budget (not just
    # the run's overall maximum) -- catches an implementation that evicts only once
    # at the very end instead of after every step.
    for step in layers_data["steps"]:
        assert step["residentBytes"] <= layers_data["memoryBudgetBytes"], (
            f"step '{step['label']}' resident bytes {step['residentBytes']} exceeded "
            f"the budget {layers_data['memoryBudgetBytes']} -- budget was not enforced "
            f"after every step"
        )


def test_byte_budget_accounts_real_per_layer_costs(layers_data):
    """The budget is declared in bytes, not a tile/item count (design constraint b):
    a 256x256 RGBA imagery tile and a glTF-bearing 3D tile must carry genuinely
    different byteCost values in the printed requests, otherwise the "byte budget"
    is really just a disguised item-count budget (the actual defect H5 exists to fix
    relative to globe_lod.js's TileLoadScheduler -- see web/js/layers/layer.js's
    module docstring).
    """
    assert layers_data["imageryTileBytes"] == 256 * 256 * 4
    assert layers_data["defaultTile3DBytes"] == layers_data["imageryTileBytes"] * 10
    seen_byte_costs = {r["layerId"]: r["byteCost"] for step in layers_data["steps"] for r in step["requests"]}
    assert seen_byte_costs["imagery"] == layers_data["imageryTileBytes"]
    assert seen_byte_costs["tiles3d"] == layers_data["defaultTile3DBytes"]
    assert seen_byte_costs["imagery"] != seen_byte_costs["tiles3d"]


# ------------------------------------------------------------------------ cancellation
def test_cancellation_on_view_jump(layers_data):
    assert layers_data["cancelledCount"] > 0, (
        "expected the deliberate large camera jump ('close-sw-2' -> 'jump-ne' in "
        "web/js/layers_check.mjs's CAMERA_PATH) to cancel at least one still-pending "
        "request from the previous view; cancelledCount == 0 would mean stale "
        "in-flight requests are never cancelled on a view move"
    )
    jump_step = next(s for s in layers_data["steps"] if s["label"] == "jump-ne")
    assert jump_step["cancelledThisStep"] > 0, (
        "expected the 'jump-ne' step specifically (not merely the run's cumulative "
        "total) to have cancelled at least one in-flight request -- see the harness's "
        "module docstring for why this step is the deliberate large jump"
    )


# --------------------------------------------------------------------- priority order
def _expected_order(requests: list[dict]) -> list[str]:
    """Independently recompute the stated priority rule (design constraint d):
    descending sseError, ties broken by ascending viewDistanceM, ties broken by
    ascending globalKey -- from the raw per-request numbers the harness printed,
    never by reading `orderedKeys` (which is the JS comparator's own answer, and
    could not catch a bug in that same comparator).
    """
    ordered = sorted(requests, key=lambda r: (-r["sseError"], r["viewDistanceM"], r["globalKey"]))
    return [r["globalKey"] for r in ordered]


def test_priority_order_matches_stated_rule(layers_data):
    for step in layers_data["steps"]:
        expected = _expected_order(step["requests"])
        assert step["orderedKeys"] == expected, (
            f"step '{step['label']}': printed order does not match the stated rule "
            f"(descending screen-space error, ties by ascending view distance, ties "
            f"by ascending key) recomputed independently in Python"
        )
        # The rule must also be a *total* order (design constraint d): no two
        # distinct requests may tie on every field.
        seen = set()
        for r in step["requests"]:
            tie_key = (r["sseError"], r["viewDistanceM"], r["globalKey"])
            assert tie_key not in seen, f"duplicate request tie key in step '{step['label']}': {tie_key}"
            seen.add(tie_key)


# ------------------------------------------------------------------- one interface
def test_all_three_layers_share_one_manager_interface(layers_data):
    """Design constraint a: 'no caller outside web/js/layers/ has to know which of
    the three it is talking to'. See this module's docstring for what a failure here
    would mean for `planNeverLoadsDirectly` and `residentPayloadLookupWorks`.
    """
    assert layers_data["planNeverLoadsDirectly"] is True, (
        "an adapter's plan() started a load by itself -- plan() must only declare "
        "demand; only LayerManager.update() may start a load, in priority order, "
        "against the shared budget"
    )
    assert layers_data["residentPayloadLookupWorks"] is True, (
        "LayerManager.getResidentPayload() failed to return a resident item's payload "
        "by its globalKey alone -- a caller should never need to know which layer "
        "produced a given resident entry"
    )
    counts = layers_data["perLayerCounts"]
    assert set(counts.keys()) == {"imagery", "terrain", "tiles3d"}
    assert counts["imagery"]["resident"] > 0, "expected at least one imagery tile to have loaded and become resident"
    assert counts["tiles3d"]["resident"] > 0, "expected at least one 3D tile to have loaded and become resident"
    # Every layer's requests are visible in the one shared, priority-ordered plan --
    # not siloed per layer.
    layer_ids_seen = {r["layerId"] for step in layers_data["steps"] for r in step["requests"]}
    assert layer_ids_seen == {"imagery", "terrain", "tiles3d"}, (
        f"expected all three layers' requests to appear in the merged plan, got {layer_ids_seen}"
    )


# --------------------------------------------------------------------- terrain refusal
def test_terrain_adapter_is_a_typed_named_refusal_not_a_silent_stub(layers_data):
    assert layers_data["terrainRefusalCount"] > 0, (
        "expected at least one terrain load attempt to have been refused; "
        "terrainRefusalCount == 0 would mean the terrain layer's requests were never "
        "actually routed through LayerManager.update(), or that they silently "
        "resolved instead of being refused"
    )
    assert layers_data["terrainRefusalName"] == "TerrainLoaderNotImplementedError", (
        f"expected the terrain layer's refusal to be the named, typed error "
        f"'TerrainLoaderNotImplementedError' (a disclosed gap, not a silent stub -- "
        f"see web/js/layers/terrain_layer.js), got {layers_data['terrainRefusalName']!r}"
    )
    # Terrain never resolves, so it must never become resident or charge the budget.
    assert layers_data["perLayerCounts"]["terrain"]["resident"] == 0
    assert layers_data["perLayerCounts"]["terrain"]["pending"] == 0


# ------------------------------------------------------------------------------ report
def test_layers_report(layers_data, capsys):
    """Not a correctness assertion -- prints the measured budget/cancellation/
    eviction numbers so `pytest -q -s` (or any CI log) carries the real values.
    """
    with capsys.disabled():
        print("\nstreaming layers (web/js/layers_check.mjs):")
        print(f"  memoryBudgetBytes={layers_data['memoryBudgetBytes']} "
              f"maxResidentBytesObserved={layers_data['maxResidentBytesObserved']} "
              f"budgetRespected={layers_data['budgetRespected']}")
        print(f"  cancelledCount={layers_data['cancelledCount']} evictedCount={layers_data['evictedCount']}")
        print(f"  perLayerCounts={layers_data['perLayerCounts']}")
        print(f"  terrainRefusalCount={layers_data['terrainRefusalCount']} "
              f"terrainRefusalName={layers_data['terrainRefusalName']}")
        print(f"  planNeverLoadsDirectly={layers_data['planNeverLoadsDirectly']} "
              f"residentPayloadLookupWorks={layers_data['residentPayloadLookupWorks']}")
        for step in layers_data["steps"]:
            print(f"  step {step['label']:>12}: requests={len(step['requests']):>3}  "
                  f"resident={step['residentCount']:>3}  "
                  f"pending={step['pendingCount']:>3}  residentBytes={step['residentBytes']:>11}  "
                  f"cancelled={step['cancelledThisStep']:>2}  evicted={step['evictedThisStep']:>2}")

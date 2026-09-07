"""M26.3 multiple-3D-viewports tests (docs/ui-rework-plan.md's M26.3 milestone;
docs/open-questions.md questions 161/162: "multiple 3D viewports sharing one scene and
clock").

Same "run the real code, don't port it" discipline as tests/test_viewer_jitter.py,
tests/test_viewer_globe.py and tests/test_viewer_layout.py: this file shells out to
`node` to run web/js/viewport_check.mjs, which drives the real, shipped ES modules
(web/js/viewport.js, web/js/scene.js's exported pure functions, web/js/frames.js,
web/js/origin.js, web/js/layout/default_layouts.js) and prints one JSON object of named
checks. Nothing here reimplements any of that arithmetic in Python.

See web/js/viewport_check.mjs's own module docstring for exactly what it can and cannot
exercise (its top comment: `Viewer`'s own per-viewport orchestration methods --
addViewport, setViewportFrame, pick -- construct a real THREE.WebGLRenderer and are
therefore verified by manual browser check instead, recorded in web/js/REPORT_M26_3.md
-- the identical, pre-existing gap tests/test_viewer_jitter.py's own module docstring
names for the single-viewport case).
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
VIEWPORT_CHECK = REPO_ROOT / "web" / "js" / "viewport_check.mjs"

NODE = shutil.which("node")


def _require_node() -> str:
    if NODE is None:
        pytest.skip(
            "node is not installed in this environment; web/js/viewport.js and friends "
            "are ES modules and this test intentionally runs them for real (see module "
            "docstring) rather than porting the logic to Python, so it cannot proceed "
            "without node. Install node to run it."
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
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(
            f"{script.name} did not print valid JSON (exit {proc.returncode})\n"
            f"stdout: {proc.stdout!r}\nstderr: {proc.stderr}"
        )
    return data


@pytest.fixture(scope="module")
def viewport_data() -> dict:
    return _run_node_json(VIEWPORT_CHECK)


def _failed(data: dict, substring: str) -> list[str]:
    return [c["name"] for c in data["checks"] if substring in c["name"] and not c["pass"]]


def _matched(data: dict, substring: str) -> list[dict]:
    matches = [c for c in data["checks"] if substring in c["name"]]
    assert matches, f"no checks matched substring {substring!r} -- viewport_check.mjs's check names changed?"
    return matches


# --------------------------------------------------------------------------- layers
def test_viewport_layers_are_independent(viewport_data):
    """Fails against a `Viewport`/`allocateViewportLayer` implementation that hands out
    a fixed or non-incrementing THREE.Layers bit -- every viewport's own trajectory-line
    clones would then render into every OTHER viewport too (web/js/viewport.js's module
    docstring: this is what THREE.Layers exists to prevent here)."""
    _matched(viewport_data, "layer")
    failed = _failed(viewport_data, "layer")
    assert not failed, f"viewport layer checks failed: {failed}"


def test_each_viewport_has_its_own_floating_origin_instance(viewport_data):
    """Fails against a `Viewport` constructor that shares one module-level
    `FloatingOrigin` (or defaults the parameter to a shared singleton) instead of
    constructing `new FloatingOrigin()` per instance."""
    failed = _failed(viewport_data, "each viewport gets its own FloatingOrigin instance")
    assert not failed


# --------------------------------------------------------------------------- picking
def test_picking_resolves_against_the_correct_viewport(viewport_data):
    """Fails against a `pick()` that ignores the camera argument and always resolves
    through one hardcoded (e.g. the primary) camera -- the BREAKS check here
    demonstrates exactly that wrong implementation's output and shows it differs from
    the correct, per-viewport-camera result."""
    failed = _failed(viewport_data, "pick")
    assert not failed, f"picking checks failed: {failed}"
    breaks = _matched(viewport_data, "BREAKS: a picker hardcoded to one camera")
    assert breaks[0]["pass"] is True, "the hardcoded-camera wrong-implementation demonstration itself did not reproduce the expected wrong answer"


# --------------------------------------------------------------------- origin shift math
def test_compute_origin_shift_pure_arithmetic(viewport_data):
    """Fails against a `computeOriginShift` that does not gate on the enabled flags
    (docs/open-questions.md Q46 -- floating origin is switchable per frame) and always
    uses the raw origin values even when disabled."""
    failed = _failed(viewport_data, "computeOriginShift")
    assert not failed, f"computeOriginShift checks failed: {failed}"


# ------------------------------------------------------------- RPO figure, per viewport
RPO_PINNED_FIGURE_M = 3.385366653674282e-06
CENTIMETRE_BOUND_M = 0.01


def test_rpo_baseline_matches_pinned_figure(viewport_data):
    """The exact figure this task's brief pins: 3.385366653674282e-06 m. If this ever
    moves, docs/ui-rework-plan.md M26.3's own instruction is to stop and report, not
    adjust the tolerance -- so this is an exact `==`, never a bound."""
    failed = _failed(viewport_data, "RPO baseline matches the pinned figure exactly")
    assert not failed


@pytest.mark.parametrize("viewport_id", ["icrf", "ric", "globe"])
def test_rpo_figure_bit_identical_per_viewport(viewport_data, viewport_id):
    """Fails against an implementation where a viewport's own `FloatingOrigin` is not
    genuinely independent (e.g. accidentally reused/aliased across viewports, or
    mutated by construction order) -- any such implementation would make at least one
    viewport's RPO figure drift away from the pinned baseline, even if only in the last
    digits."""
    failed = _failed(viewport_data, f"RPO figure for viewport '{viewport_id}'")
    assert not failed, f"viewport {viewport_id!r} RPO precision checks failed: {failed}"


def test_shared_floating_origin_across_viewports_breaks_rpo_precision(viewport_data):
    """The hard-constraint proof, worked backwards: docs/ui-rework-plan.md M26.3 warns
    that "a naive implementation that shares one origin across cameras ... will move
    that number." This test asserts the BREAKS checks in viewport_check.mjs actually
    demonstrate that -- via the real per-frame `setEnabledForFrame` mechanism
    (web/js/origin.js) a shared `FloatingOrigin` instance would expose across viewports
    -- reproducing exactly the documented "no floating origin" precision failure
    (>1 cm, matching origin.js's own without-floating-origin equivalence) rather than a
    made-up number, and that it is measurably different from the correct, per-viewport
    baseline."""
    failed = _failed(viewport_data, "BREAKS: sharing one FloatingOrigin")
    assert not failed, f"shared-origin-bug demonstration checks failed to reproduce the bug: {failed}"
    failed2 = _failed(viewport_data, "BREAKS: the shared-origin contamination reproduces")
    assert not failed2


# ------------------------------------------------------------------------- shared clock
def test_one_clock_drives_every_viewport(viewport_data):
    """Fails against a design where each viewport keeps its own FrameGraph/clock instead
    of reading the ONE shared `FrameGraph.update(t, scale)` call -- the BREAKS check
    demonstrates exactly that wrong design (two independent FrameGraph instances,
    advanced to different epochs) diverging, in contrast to the shared-clock checks
    above it, which never can."""
    failed = _failed(viewport_data, "shared clock")
    assert not failed, f"shared clock checks failed: {failed}"
    breaks = _matched(viewport_data, "BREAKS: two independent per-viewport clocks")
    assert breaks[0]["pass"] is True, "the independent-clocks wrong-design demonstration did not actually diverge as expected"


# --------------------------------------------------------------------- default RPO layout
def test_default_layout_is_icrf_beside_ric_beside_globe(viewport_data):
    """Fails against a `defaultLayoutForScenario` that (like every profile's imagery,
    byte-identical today) keys off imagery alone and therefore never selects the
    triple-viewport layout for any real scenario, or one that selects it in the wrong
    left-to-right arrangement. Also fails against a layout that drops the sidebar leaf
    entirely (a real bug this task's own manual browser check caught -- see
    web/js/REPORT_M26_3.md) -- the sidebar is still real, load-bearing UI."""
    failed = _failed(viewport_data, "RPO default layout is sidebar beside")
    assert not failed
    failed2 = _failed(viewport_data, "RPO-shaped scenario gets exactly 4 leaves")
    assert not failed2


def test_ordinary_scenario_keeps_pre_m26_3_default(viewport_data):
    """Non-regression: a scenario with no declared RIC frame must still get the
    pre-M26.3 sidebar+viewport default (web/js/layout/default_layouts.js's
    buildBaseSidebarViewportLayout, unchanged)."""
    failed = _failed(viewport_data, "ordinary scenario still gets the pre-M26.3")
    assert not failed


def test_has_ric_frame_detection(viewport_data):
    failed = _failed(viewport_data, "hasRicFrame")
    assert not failed, f"hasRicFrame checks failed: {failed}"


# ------------------------------------------------------------- question 167: chooser/swap
def test_empty_pane_offers_a_chooser_of_registered_panel_types(viewport_data):
    """Question 167's decision, verbatim: "every empty pane shows a chooser listing
    the registered panel types." Fails against a chooser hardcoded to a fixed list (or
    one that never filters by what is already placed) -- `availablePanelChoices`
    (web/js/layout/default_layouts.js) must offer the always-available "3D Viewport"
    factory type plus every singleton type not already placed elsewhere in the tree,
    and must NOT offer one that already is (this test's own "Sidebar" case)."""
    for substring in (
        "split creates a genuinely empty pane",
        'empty pane is offered "3D Viewport"',
        'empty pane is NOT offered "Sidebar"',
        "empty pane is offered every singleton type not yet placed anywhere",
    ):
        failed = _failed(viewport_data, substring)
        assert not failed, f"{substring!r} checks failed: {failed}"


def test_choosing_3d_viewport_creates_an_independent_viewport(viewport_data):
    """Question 167's required test, verbatim: "choosing '3D viewport' creates a
    viewport with its own frame and focus independent of existing ones." Fails against
    a chooser/factory that hands out the SAME existing Viewport instance (or a shared
    layer/FloatingOrigin) to a second pane instead of minting a genuinely new one --
    web/js/viewport.js's whole module docstring on why that isolation is load-bearing
    for the RPO precision figure, not merely cosmetic."""
    for substring in (
        "newly-minted viewport gets its own THREE.Layers bit",
        "newly-minted viewport gets its OWN FloatingOrigin instance",
        "independently-minted viewports keep independent cameraFrameId",
        "independently-minted viewports keep independent focus",
        "assignPanel gives the empty pane the newly-minted viewport's panelId",
    ):
        failed = _failed(viewport_data, substring)
        assert not failed, f"{substring!r} checks failed: {failed}"


def test_pane_header_menu_can_swap_a_panel_without_duplicating_it(viewport_data):
    """Question 167's decision, verbatim: "a pane header menu can swap a pane's
    panel." Fails against an `assignPanel` that does not displace a moved singleton
    panel's previous leaf -- two leaves would then claim the same panelId, and
    `LayoutManager.render()` would silently attach the one real DOM element to
    whichever leaf renders last, leaving the other blank with no chooser and no
    indication why (this test's own uniqueness assertions catch that directly)."""
    for substring in (
        "every leaf's panelId is unique after assigning a viewport",
        "swapping in an already-placed singleton (sidebar) keeps every panelId unique",
        'the pane that used to hold "sidebar" is now empty again',
        'the target pane now genuinely holds "sidebar"',
        'the displaced pane is NOT offered "Sidebar" again',
        'the displaced pane is still offered "3D Viewport"',
    ):
        failed = _failed(viewport_data, substring)
        assert not failed, f"{substring!r} checks failed: {failed}"


# ------------------------------------------------------------------------------ overall
def test_viewport_check_report(viewport_data, capsys):
    """Not a correctness assertion on its own -- prints the full named-check table so
    `pytest -q -s` (or any CI log) carries the real pass/fail detail, per this task's
    incremental-reporting requirement. The real correctness assertions are the
    individual test functions above; this only guards against a check silently
    vanishing from viewport_check.mjs's own list."""
    with capsys.disabled():
        print(f"\nviewport_check.mjs: {len(viewport_data['checks'])} checks, allPass={viewport_data['allPass']}")
        for c in viewport_data["checks"]:
            mark = "PASS" if c["pass"] else "FAIL"
            print(f"  [{mark}] {c['name']}")
    assert viewport_data["allPass"] is True, "viewport_check.mjs reported at least one failing check -- see the printed table above (-s)"

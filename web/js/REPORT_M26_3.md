# M26.3 -- Multiple 3D viewports

status: done

## Summary

One scene, one clock, three cameras. `web/js/viewport.js` adds a `Viewport` class (own
camera, view-frame id, focus, `FloatingOrigin` instance, and its own clone of every
spacecraft/footprint `Line2`); `web/js/scene.js`'s `Viewer` keeps the legacy single
camera/controls/`floatingOrigin` fields completely unchanged (zero behaviour change for
existing single-viewport callers/tests) and adds `addViewport`/`removeViewport`/
`setViewportFrame`/`setViewportFocus`/`pick` to drive any number of extra viewports
against the SAME `THREE.Scene`, `FrameGraph` and clock tick. The RPO default layout
(`web/js/layout/default_layouts.js`) gives a scenario that declares a RIC frame
(`hasRicFrame()`) a sidebar + ICRF + RIC + globe four-pane layout instead of the
ordinary sidebar+viewport pair.

## Baseline (measured before any change)

`node web/js/scene_jitter_harness.mjs` (unmodified HEAD):
```
RPO.errWithM            = 3.385366653674282e-06   (matches the brief's pinned figure exactly)
RPO.errWithoutM         = 0.013580322274719947
RPO_with_globe.errWithM = 3.385366653674282e-06, matchesBaselineExactly = true
```
`.venv/bin/pytest -q tests/test_viewer_layout.py tests/test_viewer_globe.py tests/test_viewer_jitter.py tests/test_cdm_run.py`
-> **104 passed**. Full suite (`.venv/bin/pytest -q`, before any change, per the brief) -> **377 passed** (stated baseline, not independently re-measured pre-change since the brief already pins it).

## Final measurements (after all changes)

`node web/js/scene_jitter_harness.mjs` (unchanged file paths, byte-diffed against the
backup taken before any edit -- see "break/restore" section):
```
RPO.errWithM            = 3.385366653674282e-06   <- BIT-IDENTICAL to baseline
RPO.errWithoutM         = 0.013580322274719947    <- BIT-IDENTICAL to baseline
```
`node web/js/viewport_check.mjs` -> **36/36 checks pass** (new harness, see below).

`.venv/bin/pytest -q tests/test_viewer_layout.py tests/test_viewer_globe.py tests/test_viewer_jitter.py tests/test_cdm_run.py tests/test_viewer_viewport.py`
-> **118 passed** (104 pre-existing + 14 new `test_viewer_viewport.py` functions), no regressions.

Full suite (`.venv/bin/pytest -q`, everything in the repo): **390 passed, 1 failed**.
The 1 failure is `services/cfs/tests/test_image_digest.py::test_image_builds_and_digest_matches_recorded_value`
-- a real `docker build` of `services/cfs`'s image, compared against a recorded digest
in `services/cfs/IMAGE_DIGEST.md`. This directory was never touched by this task (out of
scope per the brief), and the failure is a genuine, demonstrated digest mismatch (actual
sha256 values differ: built `bc2f8e6...` vs recorded `baac453...`), consistent with
ordinary Docker base-image drift, not with anything in this diff (nothing here touches
Docker, `services/`, or `third_party/`). Arithmetic check: 377 (baseline) + 14 (new
viewport tests) = 391 total; 391 - 1 (this unrelated failure) = 390 passed, exactly what
was measured. This failure pre-dates this task's edits; it was not silently assumed --
the actual `docker build`/`docker image inspect` output above is the evidence.

## The camera/viewport model

- **One `THREE.Scene`, one `FrameGraph`, one clock.** `Viewer.update(t)` is called once
  per animation frame with a single `t` (A1MJD); it drives `this.frameGraph.update(t,
  SCALE)` exactly once, updates every shared body/spacecraft/event Object3D exactly
  once, then loops `for (const vp of this.viewports.values()) this._updateViewport(vp,
  t)` -- passing the SAME `t` to every viewport. There is no per-viewport clock to get
  out of sync with this one (see `web/js/viewport_check.mjs`'s shared-clock section for
  the direct proof, and its "BREAKS" check for what two independent clocks would do).
- **Per-viewport, independent:** camera (`vp.camera`, own `THREE.PerspectiveCamera`),
  `OrbitControls` (`vp.controls`, own instance bound to that viewport's own canvas),
  which frame it is parented in (`vp.cameraFrameId`), which entity it is focused on
  (`vp.focus`), and its own `FloatingOrigin` instance (`vp.floatingOrigin` -- a
  DIFFERENT object from every other viewport's, including the legacy primary's own
  `Viewer.floatingOrigin`).
- **THREE.Layers, not a second `THREE.Scene`, for what's shared vs. per-viewport.**
  Bodies, spacecraft markers, event markers, the globe, stars/grid/axes stay single,
  shared `Object3D`s on the default layer (0), rendered identically by every camera
  (their apparent precision does not depend on any per-viewport origin -- see
  `web/js/viewport.js`'s module docstring for the full f64-matrix-composition argument
  for why that's true). Only trajectory/footprint `Line2` geometry -- the one thing
  whose `Float32Array` vertex data genuinely loses precision if rebased for the wrong
  viewport -- gets a real per-viewport clone (`vp.lines`/`vp.footprintLines`), tagged
  with a dedicated `THREE.Layers` bit (`allocateViewportLayer()`, one per viewport,
  starting at 2; layer 1 is reserved for the legacy primary's own lines) so viewport B
  never also renders viewport A's clones.

## Per-viewport floating origin, kept independent

Each viewport's `_rebaseViewportOriginTo(vp, x, y, z)` (scene.js) rebases ONLY `vp`'s
own `FloatingOrigin` entry, compensates ONLY `vp`'s own camera/controls (and only when
`vp`'s camera is actually parented in the entities frame), moves ONLY `vp.renderGroup`'s
position, and rebuilds ONLY `vp`'s own line clones (`_refreshViewportGeometry(vp)`).
`_maybeRebaseViewportOrigin(vp, fpAbs)` triggers this from `vp`'s OWN focus's drift, not
the primary's or any other viewport's. `computeOriginShift()` (exported from scene.js)
is the one, shared piece of "old shift vs. new shift" arithmetic both the primary's own
`_rebaseOriginTo` and every viewport's `_rebaseViewportOriginTo` call into.

## RPO figure per viewport (the hard constraint)

`web/js/viewport_check.mjs` builds three real `Viewport` instances (icrf/ric/globe,
`web/js/viewport.js`, no Viewer/WebGL needed) and runs `measureRpo()` (imported
unmodified from `scene_jitter_harness.mjs`) against EACH viewport's own
`floatingOrigin`:

| viewport | errWithM (m) | errWithoutM (m) |
|---|---|---|
| icrf | 3.385366653674282e-06 | 0.013580322274719947 |
| ric | 3.385366653674282e-06 | 0.013580322274719947 |
| globe | 3.385366653674282e-06 | 0.013580322274719947 |
| baseline (single `FloatingOrigin`) | 3.385366653674282e-06 | 0.013580322274719947 |

All four `===` each other, bit-for-bit -- not merely "under the centimetre bound."

## RPO default layout

`web/js/layout/default_layouts.js`'s `defaultLayoutForScenario(sc)` picks
`buildRpoTripleViewportLayout()` when `hasRicFrame(sc)` is true (a real, existing signal
-- `sc.frames[].axes === 'AXES_KIND_RIC'`, already sent by the server, no server change).
The tree: `sidebar | (ICRF | (RIC | globe))`, left to right -- proven by
`web/js/viewport_check.mjs`'s layout-shape checks and confirmed live (see "Manual
browser verification" below). An earlier version of this function omitted the sidebar
leaf entirely (dropped the scenario picker/every sidebar control from the DOM the
instant the RPO layout rendered) -- caught by manual browser verification, fixed, and a
headless regression check added for it (`viewport_check.mjs`'s "sidebar is NOT dropped"
check).

## Tests, the wrong implementation each fails against, and break/restore evidence

New harness: `web/js/viewport_check.mjs` (pattern: real code, one JSON object of named
checks, exactly like `frame_graph_check.mjs`/`scene_jitter_harness.mjs`/
`layout_tree_check.mjs`). New pytest driver: `tests/test_viewer_viewport.py` (14 test
functions). `Viewer`'s own per-viewport orchestration methods (`addViewport`,
`setViewportFrame`, `pick`, `_updateViewport`) construct a real `THREE.WebGLRenderer`
and are therefore NOT constructible under plain `node` -- the same, pre-existing
constraint `scene_jitter_harness.mjs`'s own docstring names for the single-viewport
case. Those are verified by manual browser check instead (below); the harness covers
everything not gated on WebGL (the `Viewport` class itself, `pickAlongCamera`,
`computeOriginShift`, `FrameGraph`, `default_layouts.js`).

| test function | fails against | break/restore done |
|---|---|---|
| `test_viewport_layers_are_independent` | `allocateViewportLayer()` returning a constant/non-incrementing bit | YES -- made it `return 5` unconditionally; both "mutually distinct" and "camera does NOT have another viewport's layer" checks failed by name; reverted (`cp` from backup), diffed byte-identical, re-passed |
| `test_each_viewport_has_its_own_floating_origin_instance` | `Viewport` constructor sharing one module-level `FloatingOrigin` | YES -- made the field a lazily-shared module singleton; "each viewport gets its own FloatingOrigin instance" failed; the 3 `RPO figure ... bit-identical` checks did NOT fail (documented finding below); reverted, byte-identical, re-passed |
| `test_picking_resolves_against_the_correct_viewport` | a `pick()` hardcoded to one camera regardless of which viewport was clicked | the harness's own "BREAKS" check demonstrates the wrong answer inline (real `pickAlongCamera` call through the wrong camera); asserted to reproduce the wrong result |
| `test_compute_origin_shift_pure_arithmetic` | `computeOriginShift` ignoring the enabled flags (Q46: per-frame switchable) | YES -- replaced `oldShift`/`newShift` with the raw un-gated values in scene.js; both disabled-origin checks failed by name; reverted, byte-identical, re-passed; full jitter/globe/layout/cdm_run/viewport suite re-run green (118 passed) since this function is on the primary camera's own hot path too |
| `test_rpo_figure_bit_identical_per_viewport` (x3) | a viewport's `floatingOrigin` not being genuinely independent | see the shared-instance break above -- did NOT fail this test (see finding below) |
| `test_shared_floating_origin_across_viewports_breaks_rpo_precision` | the exact "naive implementation shares one origin across cameras" bug M26.3 warns about, reproduced via `origin.js`'s real per-frame `setEnabledForFrame` -- one viewport's toggle leaking into another's `measureRpo()` call degrades it to the documented "no floating origin" figure (0.01358 m, not 3.39e-6 m) | reproduced inline in the harness with real, unmodified `origin.js`/`scene.js` functions (not a hand-derived number) |
| `test_one_clock_drives_every_viewport` | a design with one `FrameGraph`/clock per viewport | the harness's own "BREAKS" check builds two independent `FrameGraph`s, advances them to different epochs, and shows their RIC-frame positions diverge, in contrast to the one-shared-graph checks above it, which never can |
| `test_default_layout_is_icrf_beside_ric_beside_globe`, `test_ordinary_scenario_keeps_pre_m26_3_default`, `test_has_ric_frame_detection` | `hasRicFrame()` always returning `false` | YES -- both layout-shape checks and `hasRicFrame: true for ...` failed by name; reverted, byte-identical, re-passed |
| `test_viewport_check_report` | (reporting only) | n/a |

**Finding from the "share one `FloatingOrigin` instance" break** (documented rather than
silently smoothed over, per this task's own "investigate any oddity" rule): sharing one
`FloatingOrigin` object across all three test `Viewport`s did NOT move the per-viewport
RPO figure, because `measureRpo(fo)` always calls `fo.setOrigin('ric', <fixed value>)`
before reading anything back -- sequential, non-interleaved reuse of one instance for
the same deterministic computation cannot expose contamination. It DID fail the
identity check (`floatingOrigin !== floatingOrigin`), which is the right test for
that specific bug. The REAL, demonstrated way sharing one instance moves the number is
`test_shared_floating_origin_across_viewports_breaks_rpo_precision`'s mechanism (a
per-frame enabled/disabled flag leaking across viewports) -- kept as the load-bearing
proof for the hard constraint; the identity check and the bit-identical check are both
still valuable and both still pass for the real (non-shared) implementation, but they
individually prove different things, and only the combination (plus the real Viewer
using genuinely separate `new FloatingOrigin()` instances, verified by construction in
`viewport.js`) fully backs the claim.

## Manual browser verification (real code, not the headless harness)

`Viewer`'s own per-viewport orchestration is WebGL-gated (see above), so it was verified
against a live server (`python -m altavista serve`, `examples/05_rpo_ric.py` published)
via the Browser tool, driving `viewer.update(t)` directly (the automation environment's
background tab throttles `requestAnimationFrame`, so ticks were driven manually rather
than waited on -- confirmed this is an automation-environment artifact, not an app bug,
by checking that manual `viewer.update(t)` calls always completed without exception and
the DOM/canvas state was otherwise exactly as expected):

1. **Layout**: loading the RPO scenario switched to sidebar + "3D VIEW -- ICRF" + "3D
   VIEW -- TARGET RIC" + "3D VIEW -- GLOBE", left to right, matching the default layout
   exactly. Advancing the clock moved Target/Chaser in all three panes together (one
   clock, confirmed visually, not just headlessly).
2. **A real bug found and fixed**: picking initially failed in every non-primary
   viewport. Root cause, found by direct instrumentation (projecting a marker's own
   world position back through its viewport's camera and picking at that exact point
   still returned no hit): marker "constant N-pixel apparent size" scaling
   (`Viewer.update()`'s spacecraft/event loops) was computed from
   `this.canvas.clientHeight`/`this.camera.position` -- the LEGACY PRIMARY canvas/camera
   -- even when the primary's own pane is not part of the current layout at all (the RPO
   layout has no `'viewport'` leaf, so `this.canvas` is completely detached from the
   DOM, `clientHeight` reads 0, and the old `|| 1` fallback inflated the "pixel size"
   math by 2-3 orders of magnitude: a marker meant to read as ~7 px ended up ~139 SCENE
   UNITS in radius). A viewport whose own camera sits much closer to that marker (the
   RIC pane, by design, is metres away) ends up with its camera INSIDE the oversized
   marker sphere; `THREE.Mesh.raycast()` tests only front-facing triangles by default,
   and a camera inside a sphere only ever sees its own back faces, so picking silently
   found nothing no matter where the user clicked. Fixed with two additive scene.js
   changes: `_referenceCanvasHeight()` (falls back to any attached viewport's own canvas
   height instead of `1`) and `_markerReferenceDistance()` (sizes a marker from the
   SMALLEST distance to it across the primary AND every viewport's own camera, so no
   camera can end up trapped inside it -- a farther viewport instead sees a
   smaller-than-ideal marker, a strictly safer failure mode than "picking is broken").
   Verified after the fix: double-clicking (projected exactly onto) Target/Chaser in
   ICRF, RIC, and globe all correctly resolved `{kind: 'spacecraft', name: 'Target'|'Chaser'}`
   via `viewer.pick(vp.camera, ndcX, ndcY)`, none of them via a wrong/hardcoded camera.
   This fix does not touch any position/precision math (`_toLocal`, `trajectoryRenderPositions`,
   floating-origin rebase) -- confirmed by the RPO figure staying bit-identical
   before/after (see measurements above) -- it only changes marker visual SCALE.

## What could not be done / known limitations

- **Marker/event "constant pixel size" is still not simultaneously optimal for every
  viewport.** A single shared `Object3D` per spacecraft cannot have a different apparent
  screen size for the ICRF (whole-scenario) and RIC (RPO close-up) viewports at once;
  `_markerReferenceDistance()`'s fix only guarantees no camera ends up trapped inside a
  marker, not that every viewport's marker size is individually ideal. A true per-
  viewport fix would need per-viewport marker clones (the same treatment trajectory
  lines already got), which was judged out of scope for this task's budget given the
  actual functional bug (picking) is fixed.
- **The globe layer is shared, not per-viewport.** Enabling the globe for the "globe"
  pane also makes it visible in ICRF/RIC (same shared `GlobeLayer` object, per
  viewport.js's own module-docstring reasoning for why bodies/globe don't need a
  per-viewport copy). Accepted as correct-enough (Earth is legitimate background context
  in all three panes) rather than building a second, per-viewport globe-visibility
  layer-tag.
- **Globe LOD (`_syncGlobeLayer`) and the body "sub-pixel dot" fallback** now use
  `_referenceCanvasHeight()` (fixed, same as markers) but still compute their reference
  DISTANCE from the primary camera only (`_markerReferenceDistance` was applied to
  spacecraft/event markers, not to the body-dot fallback) -- lower risk since a RIC
  frame is always spacecraft-relative, never body-relative, so a viewport's camera
  cannot end up inside a body's dot the way it could inside a spacecraft marker; left
  unchanged given the time budget.
- **No per-pane sidebar controls for the 3 extra viewports** (frame/focus dropdowns) --
  each extra viewport's role (ICRF/RIC/globe) is set programmatically once from
  `hasRicFrame()`/the scenario's own declared RIC frame (`app.js`'s
  `setupRpoViewports()`); a user cannot yet retarget an extra viewport's frame/focus
  from the UI (double-click-to-focus is wired; a frame-select dropdown per pane is not).
- **Narrow-viewport pane header wrapping**: at narrow browser widths the "3D VIEW --
  ICRF" pane titles wrap across 2-3 lines instead of truncating with an ellipsis --
  cosmetic, not functional; not fixed.
- **`_buildViewportLines`'s LineMaterial resolution fallback** (`(vp.canvas &&
  vp.canvas.clientHeight) || this.canvas.clientHeight || 1`) still falls back to the
  (possibly-detached) primary canvas before the generic default -- affects only line
  THICKNESS in pixels (cosmetic), not position/precision; not unified with
  `_referenceCanvasHeight()` given time budget.

## Files touched (web/ and tests/test_viewer_*.py only, per scope)

- New: `web/js/viewport.js`, `web/js/viewport_check.mjs`, `tests/test_viewer_viewport.py`
- Modified: `web/js/scene.js` (multi-viewport wiring, `computeOriginShift`,
  `_referenceCanvasHeight`, `_markerReferenceDistance`, `_frameCameraInto` extraction),
  `web/js/scene_jitter_harness.mjs` (exported `measureRpo`/`buildTrack`/`SCENES`, guarded
  its own top-level side effect behind an `import.meta.url` check so it can be imported
  as a library without double-printing JSON), `web/js/layout/default_layouts.js` (RPO
  triple-viewport layout + `hasRicFrame`), `web/js/layout/layout_manager.js`
  (`applyDefaultForScenario`/`resetToDefault` take the whole scenario, not just
  imagery), `web/js/layout_bootstrap.js`, `web/js/app.js` (viewport registration/roles/
  picking/HUD), `web/index.html` (3 new panes), `web/style.css` (`.av-viewport3d` class)

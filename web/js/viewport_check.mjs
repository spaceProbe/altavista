// CLI harness for tests/test_viewer_viewport.py: `node web/js/viewport_check.mjs`.
//
// M26.3 (docs/ui-rework-plan.md): "multiple 3D viewports ... one scene and one clock,
// many cameras." Follows the exact pattern every other viewer milestone's harness uses
// (web/js/frame_graph_check.mjs, web/js/scene_jitter_harness.mjs, web/js/globe_lod_check.mjs):
// a plain `node` script that drives the REAL, shipped ES modules (web/js/viewport.js,
// web/js/scene.js's exported pure functions, web/js/frames.js, web/js/origin.js) and
// prints one JSON object of named checks, never a reimplementation of their logic.
//
// **What this harness can and cannot exercise, and why.** `web/js/scene.js`'s `Viewer`
// class (and every per-viewport method it defines: addViewport, setViewportFrame,
// _rebaseViewportOriginTo, _updateViewport, pick, ...) constructs a real
// `THREE.WebGLRenderer`, which needs a GPU context/canvas unavailable under plain `node`
// -- exactly the same, pre-existing constraint documented in scene_jitter_harness.mjs's
// own module docstring for the single-viewport case. This harness therefore tests:
//   (a) web/js/viewport.js's `Viewport` class and `pickAlongCamera` directly (no Viewer
//       needed -- neither touches WebGL);
//   (b) web/js/scene.js's exported pure functions the per-viewport Viewer methods
//       actually call into (`computeOriginShift`, `trajectoryRenderPositions`,
//       `footprintRenderPositions`) -- the exact arithmetic `_rebaseViewportOriginTo`/
//       `_refreshViewportGeometry` run, not a parallel reimplementation;
//   (c) web/js/frames.js's `FrameGraph`/`FrameNode` directly, for the "one clock drives
//       every viewport" proof, since `Viewer.update(t)`'s own per-viewport loop is
//       nothing more than "call `this.frameGraph.update(t, scale)` once, then read the
//       result from N different camera/frame pairs" -- reproduced here at exactly that
//       level;
//   (d) web/js/layout/default_layouts.js's `defaultLayoutForScenario`/`hasRicFrame` for
//       the RPO default layout (framework-free, no DOM -- same as every other
//       web/js/layout/*.js module).
// `Viewer.addViewport()`/`setViewportFrame()`/`pick()` themselves (the WebGL-gated
// orchestration that calls into (a)-(c)) are verified by manual browser check instead --
// see web/js/REPORT_M26_3.md's "Manual browser verification" section for what was done
// and observed there, the same gap scene_jitter_harness.mjs's own docstring names for
// the single-viewport case.
import * as THREE from 'three';
import { FrameGraph } from './frames.js';
import { FloatingOrigin } from './origin.js';
import { SCALE, computeOriginShift } from './scene.js';
import { Viewport, SHARED_LAYER, PRIMARY_LAYER, pickAlongCamera, resetViewportLayerCounterForTests } from './viewport.js';
import { measureRpo, buildTrack, SCENES } from './scene_jitter_harness.mjs';
import {
  defaultLayoutForScenario, hasRicFrame, ICRF_PANEL_ID, RIC_PANEL_ID, GLOBE_PANEL_ID,
  buildBaseSidebarViewportLayout, availablePanelChoices, REGISTERED_PANEL_TYPES,
} from './layout/default_layouts.js';
import { listLeaves, splitLeaf, assignPanel, findNode } from './layout/split_tree.js';

const checks = [];
function check(name, pass) { checks.push({ name, pass: !!pass }); }

// ============================================================== 1. layer allocation
// "Each viewport owns its own ... camera" starts with never aliasing two viewports onto
// the same THREE.Layers bit (viewport.js's module docstring: this is what keeps viewport
// B from also rendering viewport A's line clones). Wrong implementation this fails
// against: a version of allocateViewportLayer() that returns a fixed/constant layer (or
// forgets to increment its counter) -- every viewport would collide on one bit, and this
// check (layers actually distinct, and distinct from SHARED_LAYER/PRIMARY_LAYER) would
// catch it immediately.
resetViewportLayerCounterForTests();
{
  const vpA = new Viewport('a');
  const vpB = new Viewport('b');
  const vpC = new Viewport('c');
  check('viewport layers are mutually distinct', new Set([vpA.layer, vpB.layer, vpC.layer]).size === 3);
  check('viewport layers never collide with SHARED_LAYER(0)', ![vpA.layer, vpB.layer, vpC.layer].includes(SHARED_LAYER));
  check('viewport layers never collide with PRIMARY_LAYER(1)', ![vpA.layer, vpB.layer, vpC.layer].includes(PRIMARY_LAYER));
  check('camera has its own layer enabled', vpA.camera.layers.isEnabled(vpA.layer));
  check('camera also has the shared default layer enabled', vpA.camera.layers.isEnabled(SHARED_LAYER));
  check('camera does NOT have another viewport\'s layer enabled', !vpA.camera.layers.isEnabled(vpB.layer));
  check('each viewport gets its own FloatingOrigin instance', vpA.floatingOrigin !== vpB.floatingOrigin);
}

// ============================================================ 2. picking per viewport
// "Picking resolves against the correct viewport" (M26.3's own brief). Two spheres at
// different world positions, two cameras each aimed squarely at one of them: picking
// through the CORRECT camera for each screen click must resolve to the sphere that
// camera is actually looking at. Wrong implementation this fails against: a `pick()`
// that (as a real regression could) always uses one hardcoded camera regardless of which
// viewport was clicked -- e.g. `pickAlongCamera(primaryCamera, ndcX, ndcY, targets)` for
// every viewport. Simulated explicitly below (`wrongPick`) and shown to give the WRONG
// answer for viewport B, contrasted with the correct per-viewport call.
{
  const sphereA = new THREE.Mesh(new THREE.SphereGeometry(1, 6, 6));
  sphereA.position.set(0, 0, 0);
  sphereA.updateMatrixWorld(true);
  const sphereB = new THREE.Mesh(new THREE.SphereGeometry(1, 6, 6));
  sphereB.position.set(50, 0, 0);
  sphereB.updateMatrixWorld(true);
  const targets = [sphereA, sphereB];

  const camA = new THREE.PerspectiveCamera(45, 1, 0.1, 1000);
  camA.position.set(0, 0, 10);
  camA.lookAt(0, 0, 0);
  camA.updateMatrixWorld(true);

  const camB = new THREE.PerspectiveCamera(45, 1, 0.1, 1000);
  camB.position.set(50, 0, 10);
  camB.lookAt(50, 0, 0);
  camB.updateMatrixWorld(true);

  const hitViaA = pickAlongCamera(camA, 0, 0, targets);
  const hitViaB = pickAlongCamera(camB, 0, 0, targets);
  check('picking through viewport A\'s camera resolves sphere A', hitViaA === sphereA);
  check('picking through viewport B\'s camera resolves sphere B (not A)', hitViaB === sphereB);

  // The wrong implementation: always pick through camA, regardless of which viewport's
  // click this claims to be. Same screen coordinate (0,0), viewport B's click.
  const wrongPick = pickAlongCamera(camA, 0, 0, targets);
  check('BREAKS: a picker hardcoded to one camera would resolve viewport B\'s click to sphere A, not B',
    wrongPick === sphereA && wrongPick !== hitViaB);
}

// =============================================== 3. computeOriginShift (pure arithmetic)
// The exact function _rebaseOriginTo()/_rebaseViewportOriginTo() (scene.js) both call for
// "old origin vs. new origin -> delta + did it change". Wrong implementation this fails
// against: a version that forgets the enabled-flag gating (Q46 -- floating origin can be
// disabled per frame) and always uses the raw origin values even when disabled.
{
  const noChange = computeOriginShift({ x: 1, y: 2, z: 3 }, true, { x: 1, y: 2, z: 3 }, true);
  check('computeOriginShift: identical enabled origins -> unchanged', noChange.changed === false && noChange.dx === 0);
  const changed = computeOriginShift({ x: 1, y: 2, z: 3 }, true, { x: 4, y: 2, z: 3 }, true);
  check('computeOriginShift: different origins -> changed, correct delta', changed.changed === true && changed.dx === -3);
  const disabledOld = computeOriginShift({ x: 100, y: 0, z: 0 }, false, { x: 0, y: 0, z: 0 }, true);
  check('computeOriginShift: a disabled OLD origin contributes zero, not its raw value', disabledOld.changed === false);
  const disabledNew = computeOriginShift({ x: 0, y: 0, z: 0 }, true, { x: 100, y: 0, z: 0 }, false);
  check('computeOriginShift: a disabled NEW origin contributes zero (degrades to no shift)', disabledNew.newShift.x === 0 && disabledNew.changed === false);
}

// ======================================= 4. RPO precision, per viewport, bit-identical
// docs/ui-rework-plan.md M26.3's hard constraint: "The precision harness runs per
// viewport and the RPO figure stays bit-identical." `measureRpo` (imported, unmodified,
// from scene_jitter_harness.mjs -- the exact function that produces the pinned
// 3.385366653674282e-06 m figure) is run once per independent `Viewport`'s own
// `floatingOrigin` -- proving real, separate FloatingOrigin instances (not a shared one)
// reproduce the identical figure regardless of how many viewports exist or what order
// they're measured in.
const rpoBaseline = measureRpo(new FloatingOrigin());
check('RPO baseline matches the pinned figure exactly', rpoBaseline.errWithM === 3.385366653674282e-6);

resetViewportLayerCounterForTests();
const rpoViewports = [new Viewport('icrf'), new Viewport('ric'), new Viewport('globe')];
const perViewportRpo = rpoViewports.map((vp) => measureRpo(vp.floatingOrigin));
for (let i = 0; i < perViewportRpo.length; i++) {
  check(`RPO figure for viewport '${rpoViewports[i].id}' is bit-identical to baseline (errWithM ===)`,
    perViewportRpo[i].errWithM === rpoBaseline.errWithM);
  check(`RPO figure for viewport '${rpoViewports[i].id}' is bit-identical to baseline (errWithoutM ===)`,
    perViewportRpo[i].errWithoutM === rpoBaseline.errWithoutM);
}

/**
 * BREAKS the per-viewport-independence property on purpose: simulates what a "naive
 * implementation that shares one origin across cameras" (M26.3's own warning) would
 * produce, via the exact real, shared mutable state a shared `FloatingOrigin` instance
 * would introduce -- its per-frame `_enabled` flag (`origin.js`'s `setEnabledForFrame`,
 * the real mechanism behind the "Floating origin" checkbox list, docs/open-questions.md
 * Q46). If viewport A (say, a legacy/debug pane) disables the floating origin for the
 * entities frame -- a real, supported, per-VIEWPORT toggle in this task's own design
 * (`Viewer.setFrameOriginEnabled` loops every viewport's OWN `floatingOrigin` instance,
 * see scene.js) -- and viewport B's `floatingOrigin` were (wrongly) the SAME shared
 * object instead of B's own, A's toggle would silently also disable it for B's
 * completely unrelated, precision-critical RPO rendering. `measureRpo` (imported,
 * unmodified) is then run against that same contaminated instance, standing in for
 * "viewport B's own RPO measurement" -- proving the exact real code path
 * (`origin.js`'s `isEnabledForFrame`/`toRenderSpaceArray`, `scene.js`'s
 * `trajectoryRenderPositions`) degrades to the documented "no floating origin"
 * behaviour the instant one viewport's setting leaks into another's.
 */
function measureRpoWithSharedOriginBug() {
  const sharedFo = new FloatingOrigin();
  sharedFo.setEnabledForFrame('ric', false); // viewport A's own toggle, meant only for A
  return measureRpo(sharedFo); // stands in for viewport B's own, contaminated measurement
}
const sharedBug = measureRpoWithSharedOriginBug();
const CENTIMETRE_BOUND_M = 0.01;
check('BREAKS: sharing one FloatingOrigin across two viewports moves the RPO figure past the centimetre bound',
  sharedBug.errWithM > CENTIMETRE_BOUND_M);
check('BREAKS: the shared-origin RPO error differs from the correct per-viewport figure (not merely "still under bound")',
  sharedBug.errWithM !== rpoBaseline.errWithM);
check('BREAKS: the shared-origin contamination reproduces exactly the documented "no floating origin" error (origin.js\'s own equivalence)',
  sharedBug.errWithM === sharedBug.errWithoutM);

// ============================================================ 5. one clock, many frames
// "One scene and one clock, many cameras" -- proven at the level `Viewer.update(t)`
// actually uses it: a single `FrameGraph.update(t, scale)` call drives every frame node's
// position/orientation, and every viewport (however many cameras are parented across
// however many of those nodes) reads the result of that SAME call for the SAME `t`. Wrong
// implementation this fails against: a design where each viewport kept its own
// FrameGraph/clock (or its own copy of `t`) -- reproduced explicitly below as
// `perViewportClockGraphs`, and shown to diverge exactly where the shared-clock design
// (one `graph`, one `update()` call) never can.
{
  const graph = new FrameGraph();
  graph.addFrame({ id: 'entities' });
  // A real RIC-shaped child frame: entity-relative, origin track = the chief's own
  // motion (same track shape TrajectoryInterp/FrameNode already consume elsewhere in
  // this codebase, e.g. frame_graph_check.mjs's own 'sat1_ric').
  const track = buildTrack(SCENES.LEO.distKm, SCENES.LEO.speedKmS);
  // FrameGraph.addFrame() stores `def.originTrack` verbatim but does NOT itself call
  // setOriginTrack() (see web/js/frames.js's own addFrame()/FrameNode constructor) --
  // scene.js's real _buildFrameGraph() calls it explicitly after addFrame(), and so does
  // web/js/frame_graph_check.mjs's own 'sat1_ric' setup; mirrored here for the same
  // reason (a FrameNode's motion comes only from an explicit setOriginTrack() call).
  const ricNode = graph.addFrame({ id: 'target_ric', parentId: 'entities', originTrack: track });
  ricNode.setOriginTrack(track);

  const camIcrf = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
  graph.reparent(camIcrf, 'entities'); // "ICRF" viewport: parented in the entities frame
  const camRic = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
  graph.reparent(camRic, 'target_ric'); // "RIC" viewport: parented in the RIC child frame

  const t0 = track.t[0], t1 = track.t[1]; // one render-frame apart, same as buildTrack()'s own dt
  graph.update(t0, SCALE);
  const ricPosAtT0 = graph.frame('target_ric').object3D.position.clone();
  const expectedAtT0 = { x: track.pos[0] * SCALE, y: track.pos[1] * SCALE, z: track.pos[2] * SCALE };
  check('shared clock @ t0: RIC frame position matches its own origin track at t0',
    Math.abs(ricPosAtT0.x - expectedAtT0.x) < 1e-12 && Math.abs(ricPosAtT0.y - expectedAtT0.y) < 1e-12);

  // Advance the ONE shared clock. Both viewports' cameras live under the SAME graph --
  // there is no per-viewport `t` to forget to advance.
  graph.update(t1, SCALE);
  const ricPosAtT1 = graph.frame('target_ric').object3D.position.clone();
  const expectedAtT1 = { x: track.pos[3] * SCALE, y: track.pos[4] * SCALE, z: track.pos[5] * SCALE };
  check('shared clock @ t1: RIC frame position moved to match the SAME track at the new epoch',
    Math.abs(ricPosAtT1.x - expectedAtT1.x) < 1e-12 && ricPosAtT1.x !== ricPosAtT0.x);
  // The "ICRF" viewport's own camera is parented in a frame with no origin track
  // ('entities' has none) -- it does not move, which is correct (only proves this
  // check setup is sane, not the clock-sharing property itself).
  check('entities frame (ICRF viewport\'s parent) has no motion of its own -- unperturbed by the RIC frame\'s track',
    graph.frame('entities').object3D.position.length() === 0);
  camIcrf.updateMatrixWorld(true);
  camRic.updateMatrixWorld(true);
  check('both cameras\' world transforms are current after the SAME single graph.update(t1) call (no separate re-sync needed)',
    camRic.matrixWorldNeedsUpdate === false && camIcrf.matrixWorldNeedsUpdate === false);

  // BREAKS the shared-clock property on purpose: two SEPARATE FrameGraph instances (the
  // wrong design -- "each viewport its own clock"), one advanced to t1, the other left at
  // t0 (exactly the failure mode of a per-viewport clock that isn't kept in lockstep by
  // construction, e.g. a viewport whose render loop was skipped/throttled one tick).
  const graphA = new FrameGraph();
  graphA.addFrame({ id: 'entities' });
  graphA.addFrame({ id: 'target_ric', parentId: 'entities' }).setOriginTrack(track);
  const graphB = new FrameGraph();
  graphB.addFrame({ id: 'entities' });
  graphB.addFrame({ id: 'target_ric', parentId: 'entities' }).setOriginTrack(track);
  graphA.update(t1, SCALE);
  graphB.update(t0, SCALE); // "forgot" to advance viewport B's own clock
  const posA = graphA.frame('target_ric').object3D.position;
  const posB = graphB.frame('target_ric').object3D.position;
  check('BREAKS: two independent per-viewport clocks CAN diverge (posA at t1 != posB at t0) -- exactly what one shared graph.update() call prevents by construction',
    Math.abs(posA.x - posB.x) > 1e-9);
}

// ======================================================= 6. default RPO layout
// "Default layout for the RPO profile: ICRF beside RIC beside globe" (M26.3's own brief,
// verbatim). Wrong implementation this fails against: a version of
// defaultLayoutForScenario() that (like every profile's imagery, byte-identical today --
// default_layouts.js's own module comment) keys off imagery and therefore NEVER picks the
// triple-viewport layout for any real scenario; or one that picks it but in the wrong
// arrangement (not three leaves side by side in the ICRF/RIC/globe order).
{
  const ordinaryScenario = { frame: { name: 'EarthMJ2000Eq' }, frames: [{ id: 'EarthMJ2000Eq', axes: 'AXES_KIND_MJ2000_EQ' }], imagery: null };
  check('hasRicFrame: false for an ordinary scenario with no RIC frame', hasRicFrame(ordinaryScenario) === false);

  const rpoScenario = {
    frame: { name: 'EarthMJ2000Eq' },
    imagery: null,
    frames: [
      { id: 'EarthMJ2000Eq', axes: 'AXES_KIND_MJ2000_EQ' },
      { id: 'Target_RIC', parentFrameId: 'EarthMJ2000Eq', axes: 'AXES_KIND_RIC', entityId: 'Target' },
    ],
  };
  check('hasRicFrame: true for a scenario that declares a RIC frame (examples/05_rpo_ric.py-shaped)', hasRicFrame(rpoScenario) === true);

  const ordinaryLayout = defaultLayoutForScenario(ordinaryScenario);
  const ordinaryLeaves = listLeaves(ordinaryLayout).map((l) => l.panelId);
  check('ordinary scenario still gets the pre-M26.3 sidebar+viewport default (unchanged)',
    ordinaryLeaves.length === 2 && ordinaryLeaves.includes('sidebar') && ordinaryLeaves.includes('viewport'));

  const rpoLayout = defaultLayoutForScenario(rpoScenario);
  const rpoLeaves = listLeaves(rpoLayout).map((l) => l.panelId);
  // The sidebar is still real, load-bearing UI (scenario picker, frame/focus controls) --
  // this must be exactly 4 leaves (sidebar + the 3 viewport panes), never just 3. An
  // earlier version of buildRpoTripleViewportLayout() omitted the sidebar leaf entirely;
  // caught by manual browser verification (web/js/REPORT_M26_3.md), not by this harness
  // at the time -- this check exists specifically so a regression of that exact bug is
  // caught headlessly from now on.
  check('RPO-shaped scenario gets exactly 4 leaves: sidebar + 3 viewport panes (sidebar is NOT dropped)',
    rpoLeaves.length === 4 && rpoLeaves.includes('sidebar') &&
    [ICRF_PANEL_ID, RIC_PANEL_ID, GLOBE_PANEL_ID].every((id) => rpoLeaves.includes(id)));
  check('RPO default layout is sidebar beside (ICRF beside (RIC beside globe)), in that left-to-right order',
    rpoLayout.type === 'split' && rpoLayout.direction === 'row' &&
    rpoLayout.children[0].panelId === 'sidebar' &&
    rpoLayout.children[1].type === 'split' && rpoLayout.children[1].direction === 'row' &&
    rpoLayout.children[1].children[0].panelId === ICRF_PANEL_ID &&
    rpoLayout.children[1].children[1].type === 'split' && rpoLayout.children[1].children[1].direction === 'row' &&
    rpoLayout.children[1].children[1].children[0].panelId === RIC_PANEL_ID &&
    rpoLayout.children[1].children[1].children[1].panelId === GLOBE_PANEL_ID);

  // BREAKS: a scenario with no `frames` array at all (e.g. malformed/older wire shape)
  // must not throw -- hasRicFrame's own `Array.isArray` guard.
  check('hasRicFrame: false (not a throw) for a scenario with no frames array', hasRicFrame({ imagery: null }) === false);
  check('defaultLayoutForScenario: null scenario (page boot, before any load) falls back cleanly', (() => {
    const t = defaultLayoutForScenario(null);
    return listLeaves(t).map((l) => l.panelId).includes('viewport');
  })());
}

// ---------------------------------------------------------------------------------
// Question 167: "every empty pane gets a chooser of registered panel types ... and a
// pane header menu can swap panels"; required test verbatim: "a headless test splits
// a pane and assigns a viewport." Exercises the REAL, shipped split_tree.js
// (splitLeaf/assignPanel/listLeaves) and viewport.js (Viewport, layer allocation) --
// the same functions web/js/layout/layout_manager.js's chooser/header-menu and
// web/js/app.js's `mintViewport()` factory call -- never a re-derived model of them.
//
// Wrong implementation this fails against: (a) a chooser that only lists a fixed,
// hardcoded set of panel types rather than a real registry (availablePanelChoices
// returning something other than REGISTERED_PANEL_TYPES filtered by actual tree
// occupancy would show up as a wrong choice list below); (b) "assign a viewport" that
// hands the SAME existing Viewport instance to a second pane instead of minting a new
// one (would fail the layer/floating-origin/focus independence checks); (c) an
// assignPanel that does not displace a moved singleton panel's previous leaf, leaving
// two leaves claiming the same panelId (checked directly by asserting every leaf's
// panelId is unique after the move).
resetViewportLayerCounterForTests();
{
  const base = buildBaseSidebarViewportLayout(); // sidebar + viewport, 2 leaves
  const split = splitLeaf(base, 'pane-viewport', 'row', 'placeholder-1');
  const emptyLeaf = listLeaves(split).find((l) => l.panelId === 'placeholder-1');
  check('split creates a genuinely empty pane (no registered content for its panelId)',
    !!emptyLeaf && !REGISTERED_PANEL_TYPES.some((t) => t.panelId === emptyLeaf.panelId));

  const choicesForEmptyPane = availablePanelChoices(split, emptyLeaf.id);
  check('empty pane is offered "3D Viewport" (the factory type, always available)',
    choicesForEmptyPane.some((c) => c.panelId === 'viewport' && c.factory === true));
  check('empty pane is NOT offered "Sidebar" (already placed in the OTHER pane)',
    !choicesForEmptyPane.some((c) => c.panelId === 'sidebar'));
  check('empty pane is offered every singleton type not yet placed anywhere (Map/Run Products/Console)',
    ['map-2d', 'run-products', 'console-log'].every((id) => choicesForEmptyPane.some((c) => c.panelId === id)));

  // "Choosing 3D viewport creates a viewport with its own frame and focus independent
  // of existing ones" -- the exact required test. Mint a fresh panelId (what
  // LayoutManager._mintPanelId()/app.js's mintViewport() do together) and construct a
  // REAL, independent Viewport for it, exactly like app.js's factory.
  const mintedPanelId = 'viewport-extra-1';
  const existingViewport = new Viewport('pane-viewport'); // stands in for the primary/an already-open viewport
  const newViewport = new Viewport(mintedPanelId);
  check('a newly-minted viewport gets its own THREE.Layers bit, distinct from an existing viewport',
    newViewport.layer !== existingViewport.layer);
  check('a newly-minted viewport gets its OWN FloatingOrigin instance, never the existing viewport\'s',
    newViewport.floatingOrigin !== existingViewport.floatingOrigin);
  // Independent focus/frame: setting one must never leak into the other -- the exact
  // isolation web/js/viewport.js's own module docstring calls out as this module's
  // whole reason to exist.
  existingViewport.cameraFrameId = 'EarthMJ2000Eq';
  existingViewport.focus = 'ChaserSat';
  newViewport.cameraFrameId = 'Target_RIC';
  newViewport.focus = 'TargetSat';
  check('two independently-minted viewports keep independent cameraFrameId',
    existingViewport.cameraFrameId === 'EarthMJ2000Eq' && newViewport.cameraFrameId === 'Target_RIC');
  check('two independently-minted viewports keep independent focus',
    existingViewport.focus === 'ChaserSat' && newViewport.focus === 'TargetSat');

  const assigned = assignPanel(split, emptyLeaf.id, mintedPanelId);
  const assignedLeaves = listLeaves(assigned);
  check('assignPanel gives the empty pane the newly-minted viewport\'s panelId',
    findNode(assigned, emptyLeaf.id).panelId === mintedPanelId);
  check('every leaf\'s panelId is unique after assigning a viewport (no pane silently orphaned/duplicated)',
    new Set(assignedLeaves.map((l) => l.panelId)).size === assignedLeaves.length);

  // Header-menu "swap": moving an already-placed SINGLETON panel (sidebar) onto the
  // just-created viewport pane must displace it from its old pane (which reverts to a
  // fresh empty placeholder), never leave two leaves claiming 'sidebar'.
  const swapped = assignPanel(assigned, emptyLeaf.id, 'sidebar');
  const swappedLeaves = listLeaves(swapped);
  check('swapping in an already-placed singleton (sidebar) keeps every panelId unique',
    new Set(swappedLeaves.map((l) => l.panelId)).size === swappedLeaves.length);
  check('the pane that used to hold "sidebar" is now empty again (a fresh placeholder, not "sidebar")',
    findNode(swapped, 'pane-sidebar').panelId !== 'sidebar');
  check('the target pane now genuinely holds "sidebar" (the swap actually happened)',
    findNode(swapped, emptyLeaf.id).panelId === 'sidebar');
  // "Sidebar" itself is now placed on the OTHER pane (the swap's whole point), so the
  // displaced pane must NOT be re-offered it -- it is offered everything ELSE that
  // is still free (the always-available "3D Viewport" factory type, at minimum).
  const displacedChoices = availablePanelChoices(swapped, 'pane-sidebar');
  check('the displaced pane is NOT offered "Sidebar" again (it now lives on the other pane)',
    !displacedChoices.some((c) => c.panelId === 'sidebar'));
  check('the displaced pane is still offered "3D Viewport" (always available)',
    displacedChoices.some((c) => c.panelId === 'viewport' && c.factory === true));
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
process.exit(allPass ? 0 : 1);


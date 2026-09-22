// M26.2 windowing core: a default layout per profile (question 161). See
// web/js/layout/REPORT.md's "Profile imagery is NOT currently profile-distinguishing"
// finding for the full story -- short version: the only profile-derived signal that
// reaches the client at all today is `scenario.imagery` ({urlTemplate, attribution,
// maxLevel}, stamped on by altavista/server.py's Hub from the active profile's
// profiles/*.yaml `imagery:` section), and all four shipped profiles
// (feasibility/design/execution/analysis) declare byte-identical imagery today. This
// module builds a real, generic lookup keyed by imagery signature -- not a fake
// per-profile branch -- so it does the right thing today (one default for everyone,
// because there is genuinely only one distinct imagery source in the repo) and is
// ready to differentiate the moment a profile's imagery actually diverges, without
// needing a server change (no new field is required: the signature is derived
// entirely from data the server already sends).
//
// This module never touches the server API surface and adds no new scenario field --
// it only reads `scenario.imagery`, which altavista/server.py already sends.

import { createLeaf, createSplit, listLeaves } from './split_tree.js';

// The two content panels that exist today (web/js/app.js's #sidebar and #viewport,
// moved into panes unchanged -- see web/js/layout/layout_manager.js). Explicit,
// stable ids (not the auto-incrementing counter) so a persisted/exported default
// layout never depends on process-lifetime state.
export function buildBaseSidebarViewportLayout() {
  return createSplit(
    'row',
    0.22, // ~280px of a ~1280px window -- matches today's static #app grid-template-columns
    [
      createLeaf('sidebar', { id: 'pane-sidebar' }),
      createLeaf('viewport', { id: 'pane-viewport' }),
    ],
    { id: 'split-root' },
  );
}

function imagerySignature(imagery) {
  const urlTemplate = imagery && typeof imagery.urlTemplate === 'string' ? imagery.urlTemplate : null;
  const maxLevel = imagery && typeof imagery.maxLevel === 'number' ? imagery.maxLevel : null;
  return JSON.stringify({ urlTemplate, maxLevel });
}

// Real lookup table, not a stub: registerDefaultLayoutForImagery lets a future
// profile with genuinely different imagery get a genuinely different default layout
// (e.g. M26.3's "ICRF beside RIC beside globe" default for the RPO profile,
// docs/ui-rework-plan.md's M26.3 milestone) without touching this function's shape.
const registry = new Map();

export function registerDefaultLayoutForImagery(imagery, treeFactory) {
  registry.set(imagerySignature(imagery), treeFactory);
}

// The one entry that exists today: every shipped profile's offline-fixture imagery
// (web/fixtures/gen_globe_tiles.py) resolves to the base sidebar+viewport split.
registerDefaultLayoutForImagery(
  { urlTemplate: './fixtures/tiles/{z}/{x}/{y}.png', maxLevel: 2 },
  buildBaseSidebarViewportLayout,
);

export function defaultLayoutForImagery(imagery) {
  const factory = registry.get(imagerySignature(imagery));
  return factory ? factory() : buildBaseSidebarViewportLayout();
}

// ------------------------------------------------------------- M26.3: multi-viewport default
// "Default layout for the RPO profile: ICRF beside RIC beside globe" (docs/ui-rework-
// plan.md's M26.3 milestone, verbatim). There is no distinct "RPO profile" file in
// profiles/*.yaml -- every shipped profile's imagery is byte-identical today (this
// module's own top comment), so imagery signature cannot be what selects this layout.
// What genuinely, already reaches the client and distinguishes "this is an RPO-shaped
// scenario" is `scenario.frames`: an entity-relative RIC frame only exists in a
// scenario's frame list when the scenario itself declared one (`Scenario.frame_ric()`,
// examples/05_rpo_ric.py) -- ordinary scenarios never carry an AXES_KIND_RIC entry. This
// is real scenario data the server already sends (altavista/scenario.py's `_build_frames`
// -> the wire `frames` list, unchanged by this task), not a new field and not a guess.
export function hasRicFrame(sc) {
  return !!(sc && Array.isArray(sc.frames) && sc.frames.some((fd) => fd && fd.axes === 'AXES_KIND_RIC'));
}

// Panel ids for the three M26.3 3D-viewport panes -- distinct from the single legacy
// 'viewport' panel id (buildBaseSidebarViewportLayout() above), which stays exactly what
// it was for every non-RPO scenario. web/js/layout_bootstrap.js registers matching
// contentProviders entries; web/js/app.js registers a real Viewport (web/js/viewport.js)
// against each one once a scenario with a RIC frame loads (viewer.addViewport()).
export const ICRF_PANEL_ID = 'viewport-icrf';
export const RIC_PANEL_ID = 'viewport-ric';
export const GLOBE_PANEL_ID = 'viewport-globe';

// The sidebar (scenario picker, spacecraft/body/event lists, frame/focus controls,
// timebar-adjacent options) is still real UI a user needs regardless of how many 3D
// panes are open -- `buildBaseSidebarViewportLayout()` above keeps it for the ordinary
// case, and this RPO default keeps it too (at the SAME 0.22 share of the window that
// layout already uses), with the three 3D panes filling the rest: "ICRF beside RIC
// beside globe" describes the 3D-viewport portion, not a claim that the sidebar
// disappears. (An earlier version of this function omitted the sidebar leaf entirely --
// caught by manual browser verification, web/js/REPORT_M26_3.md's own account of it:
// switching into the RPO layout silently dropped the scenario picker and every other
// sidebar control from the DOM, since LayoutManager.render() rebuilds its whole subtree
// from the tree's own leaves.)
export function buildRpoTripleViewportLayout() {
  return createSplit(
    'row',
    0.22,
    [
      createLeaf('sidebar', { id: 'pane-sidebar' }),
      createSplit(
        'row',
        0.34,
        [
          createLeaf(ICRF_PANEL_ID, { id: 'pane-viewport-icrf' }),
          createSplit(
            'row',
            0.5,
            [
              createLeaf(RIC_PANEL_ID, { id: 'pane-viewport-ric' }),
              createLeaf(GLOBE_PANEL_ID, { id: 'pane-viewport-globe' }),
            ],
            { id: 'split-ric-globe' },
          ),
        ],
        { id: 'split-icrf-rest' },
      ),
    ],
    { id: 'split-sidebar-rest' },
  );
}

/**
 * The real default-layout entry point as of M26.3: prefers `defaultLayoutForImagery`'s
 * existing per-imagery lookup, but selects `buildRpoTripleViewportLayout()` first when
 * the scenario itself declares a RIC frame (`hasRicFrame`, above) -- an RPO-shaped
 * scenario, regardless of which profile published it (imagery is not what
 * distinguishes it, see this section's own top comment). Falls back to
 * `defaultLayoutForImagery(sc && sc.imagery)` for every other scenario, unchanged from
 * before M26.3. `sc` may be `null` (no scenario loaded yet, e.g. at page boot -- the
 * caller, web/js/layout/layout_manager.js's constructor, has no scenario to pass yet),
 * in which case this degrades to exactly `defaultLayoutForImagery(null)` did before.
 * @param {object|null} sc a wire scenario object (or null)
 */
export function defaultLayoutForScenario(sc) {
  if (hasRicFrame(sc)) return buildRpoTripleViewportLayout();
  return defaultLayoutForImagery(sc && sc.imagery);
}

// --------------------------------------------------------------- M26.4: the three panels
// "Run products and scores", "2D companion map", "console/log" (docs/ui-rework-plan.md's
// M26.4 milestone) -- each hosted in its own pane, stacked to the right of whatever
// `defaultLayoutForScenario` already returned (the 2-leaf base OR the 4-leaf RPO triple-
// viewport tree, unchanged either way). Deliberately a SEPARATE, additive wrapper
// function rather than a change to `buildBaseSidebarViewportLayout`/
// `buildRpoTripleViewportLayout`/`defaultLayoutForScenario` themselves: those three are
// asserted by name in web/js/viewport_check.mjs (`test_ordinary_scenario_keeps_pre_m26_3_
// default`'s exact "2 leaves", `test_default_layout_is_icrf_beside_ric_beside_globe`'s
// exact "4 leaves") -- this task's own instruction is "do not regress
// tests/test_viewer_viewport.py", so those two functions' OUTPUT SHAPE for a bare
// `defaultLayoutForScenario(sc)` call must stay byte-identical. `web/js/layout/
// layout_manager.js`'s `applyDefaultForScenario`/`resetToDefault`/constructor call this
// wrapper on top, which is the one integration point NOT covered by any pre-existing
// headless assertion (layout_manager.js needs a real `document`, verified by browser
// check only -- see web/js/layout/REPORT.md's own note on this).
export const RUN_PRODUCTS_PANEL_ID = 'run-products';
export const MAP_PANEL_ID = 'map-2d';
export const CONSOLE_PANEL_ID = 'console-log';
// F3b (docs/feasibility-plan.md's F3 milestone): the feasibility-study panel
// (web/js/panels/feasibility_panel.js) -- registered below in REGISTERED_PANEL_TYPES
// (reachable through the M26.5 pane chooser/header-menu, question 167). NOT added to
// attachM264Panels()'s own default tree: attachM264Panels' exact output shape ("exactly
// 5 leaves" / "exactly 7 leaves") is asserted by name in web/js/panels_check.mjs's own
// "attachM264Panels:" checks, and those must stay green UNTOUCHED -- adding a 4th
// default panel there would change that shape. Most scenarios have no `sweep` key at
// all (this panel's own render() shows an honest "no study in this scenario" notice for
// exactly that case), so it is still not part of the ORDINARY default
// (`defaultLayoutForScenario`/`attachM264Panels`, unchanged by F5.1 below) -- showing it
// by default in EVERY layout would mean most users see a permanently-empty pane most of
// the time, and the chooser already exists precisely for "a panel a user wants
// sometimes, not always" (question 167's own stated purpose).
//
// F5.1 (question 197) supersedes the other half of this reasoning for the one case
// where a feasibility study genuinely IS the point of the scenario: see
// `hasSweep`/`buildSweepStudyLayout`/`defaultLayoutTreeForScenario` below, the sweep-
// specific sibling of `hasRicFrame`/`buildRpoTripleViewportLayout`/
// `defaultLayoutForScenario` above. A sweep-carrying, still-unmodified layout now
// defaults to a tree that DOES include this panel, at its wide/primary share -- the
// "permanently-empty pane" argument does not apply there, since a sweep-carrying
// scenario's whole reason for existing is the study this panel shows.
export const FEASIBILITY_PANEL_ID = 'feasibility';

/**
 * Wrap an already-built layout tree with the three M26.4 panels stacked in a column to
 * its right. Pure and generic over `tree`'s own shape (works identically whether `tree`
 * is the 2-leaf base or the 4-leaf RPO triple-viewport tree) -- it only ever adds
 * new splits/leaves, never inspects or reshapes what `tree` already contains, so
 * whatever `defaultLayoutForScenario` decided stays intact as this new tree's first
 * child, verbatim (same node objects, same ids).
 * @param {object} tree a valid split-tree node (split_tree.js shape)
 * @returns {object} a NEW tree; `tree` itself is never mutated
 */
// ------------------------------------------------------------- M26.5: panel registry (question 167)
// "Every empty pane shows a chooser listing the registered panel types ... and a pane
// header menu can swap a pane's panel" (the lead's decision, verbatim). Lives here --
// framework-free, alongside the other panel-id constants and the existing
// `hasRicFrame`/`defaultLayoutForScenario` panel-selection logic -- rather than in
// web/js/layout/layout_manager.js (DOM-only, browser-check-only per that module's own
// docstring), so BOTH `availablePanelChoices` below and the panel-id list itself are
// testable headlessly under plain `node` (web/js/layout/layout_tree_check.mjs), like
// every other panel-selection rule in this file.
//   - `factory: true` (3D Viewport only): no uniqueness constraint -- choosing it
//     always mints a brand-new panelId and a brand-new Viewport (its own frame and
//     focus, web/js/viewport.js), so it is always offered.
//   - every other entry names a SINGLETON content element that already exists
//     (web/js/layout_bootstrap.js's `contentProviders`) and can only ever be attached
//     to one pane's DOM at a time -- `availablePanelChoices` offers it only when no
//     OTHER leaf in the tree currently claims it.
// R3.5b: the command console panel's own id (web/js/panels/command_panel.js) --
// singleton, like every other non-viewport REGISTERED_PANEL_TYPES entry (see that
// list's own comment). Declared here, ahead of REGISTERED_PANEL_TYPES, so both that
// list and this file's own execution-profile default-layout wrapper
// (`attachCommandPanel` below) reference the exact same constant.
export const COMMAND_PANEL_ID = 'command-console';

// Round 5 (question 228 finding 2, browser half): the Layers panel's own id
// (web/js/panels/layers_panel.js) -- a user selects a catalogued tile set here and sees
// it drawn through the shared `LayerManager` (web/js/scene.js's `viewer.layerManager`).
// Singleton, like every other non-viewport REGISTERED_PANEL_TYPES entry (see that
// list's own comment); declared here, ahead of REGISTERED_PANEL_TYPES, for the same
// reason COMMAND_PANEL_ID is.
//
// Round 5 threaded this constant only into REGISTERED_PANEL_TYPES (chooser-reachable
// everywhere, in no default layout) -- see heavy-plan.md's round-5 status, decision 11,
// and this constant's own history in version control for that reasoning in full: doing
// so unconditionally back then would have changed `defaultLayoutTreeForScenario`'s
// output shape for EVERY scenario, including the ordinary/no-profile case
// web/js/layout/layout_tree_check.mjs's own
// "defaultLayoutTreeForScenario.ordinaryScenarioIsByteIdenticalToAttachM264PanelsOf
// DefaultLayoutForScenario(noRegression)" check pins as a named regression guard for a
// DIFFERENT feature (F5.1's sweep default).
//
// Round 6 (question 231, "Also next round" -- the lead's ratification, verbatim: "the
// Layers panel joins the execution and design default layouts once F5.1's regression
// guard is updated with it"): now threaded in, but only CONDITIONALLY -- see
// `attachLayersPanel` and `isDesignProfile` below, applied from
// `defaultLayoutTreeForScenario` when `isExecutionProfile(sc) || isDesignProfile(sc)`.
// The ordinary/no-profile scenario `layout_tree_check.mjs`'s guard pins is neither
// execution- nor design-shaped, so that check's pinned tree is genuinely unaffected and
// unchanged by this round's edit -- `layout_tree_check.mjs` gained NEW checks instead,
// pinning the execution- and design-profile trees byte-identically in the same way.
// Still chooser-reachable everywhere via REGISTERED_PANEL_TYPES below, exactly as
// before -- this round only adds default-layout presence for two of the four profiles,
// it does not change reachability for any of them.
export const LAYERS_PANEL_ID = 'layers';

export const REGISTERED_PANEL_TYPES = [
  { panelId: 'viewport', label: '3D Viewport', factory: true },
  { panelId: MAP_PANEL_ID, label: '2D Map' },
  { panelId: RUN_PRODUCTS_PANEL_ID, label: 'Run Products & Scores' },
  { panelId: CONSOLE_PANEL_ID, label: 'Console / Log' },
  { panelId: 'sidebar', label: 'Sidebar' },
  // F3b: singleton, like every other non-viewport entry above -- offered whenever no
  // OTHER leaf in the tree currently holds it (availablePanelChoices' own existing rule).
  { panelId: FEASIBILITY_PANEL_ID, label: 'Feasibility Study' },
  // R3.5b (question 201(d)'s own text: "the console panel lives in the execution
  // profile's default layout only" -- ONLY about the DEFAULT layout, not about
  // availability. Registered here unconditionally so any pane, in ANY profile, can be
  // swapped to the command console through the M26.5 chooser/header-menu, exactly like
  // FEASIBILITY_PANEL_ID above already is outside its own one profile-shaped default.
  { panelId: COMMAND_PANEL_ID, label: 'Command Console' },
  // Round 5: the Layers panel, chooser-reachable in every profile like
  // COMMAND_PANEL_ID/FEASIBILITY_PANEL_ID above. Round 6 (question 231) additionally
  // threads it into the execution and design default layouts -- see LAYERS_PANEL_ID's
  // own comment above and `attachLayersPanel` below.
  { panelId: LAYERS_PANEL_ID, label: 'Layers' },
];

/**
 * Registered panel types a pane may be assigned to RIGHT NOW: every `factory` type,
 * plus every singleton type not already claimed by a DIFFERENT leaf in `tree`. Pure
 * and DOM-free -- `layout_manager.js`'s own `_availableChoices` is a thin wrapper
 * around this (`availablePanelChoices(this.tree, node.id)`), so the exact same rule
 * that decides what the chooser/header-menu SHOW is what a headless test can assert
 * against, with no DOM required.
 * @param {object} tree a valid split-tree node (split_tree.js shape)
 * @param {string} currentLeafId the leaf being offered choices for (excluded from
 *   "used elsewhere" -- a leaf's own current panelId, if any, does not block itself)
 * @param {Array} [types] defaults to REGISTERED_PANEL_TYPES
 */
export function availablePanelChoices(tree, currentLeafId, types = REGISTERED_PANEL_TYPES) {
  const usedElsewhere = new Set(
    listLeaves(tree).filter((l) => l.id !== currentLeafId).map((l) => l.panelId),
  );
  return types.filter((c) => c.factory || !usedElsewhere.has(c.panelId));
}

export function attachM264Panels(tree) {
  return createSplit(
    'row',
    0.7,
    [
      tree,
      createSplit(
        'column',
        0.4,
        [
          createLeaf(RUN_PRODUCTS_PANEL_ID, { id: 'pane-run-products' }),
          createSplit(
            'column',
            0.55,
            [
              createLeaf(MAP_PANEL_ID, { id: 'pane-map-2d' }),
              createLeaf(CONSOLE_PANEL_ID, { id: 'pane-console-log' }),
            ],
            { id: 'split-map-console' },
          ),
        ],
        { id: 'split-panels-right' },
      ),
    ],
    { id: 'split-m264-outer' },
  );
}

// ------------------------------------------------------- F5.1: sweep default layout
// "When the selected scenario carries a `sweep` key ... and the layout is unmodified,
// the default layout is a sweep-shaped one: sidebar, the feasibility panel given the
// wide/primary share, run products, and console" (question 197's own required text).
// `hasSweep` mirrors `hasRicFrame` above exactly: real data the server already sends
// (`scenario.sweep`, published by `POST /api/cdm/sweep` -- see web/js/panels/
// feasibility_panel.js's own top comment for the wire shape), not a new field and not
// a guess. A scenario published by any OTHER route has no `sweep` key at all (that
// panel's own module comment, same posture `gridRows`/`scoreNames` there already take
// on a malformed/absent sweep) -- `hasSweep` only checks for the key's presence, it
// does not validate the sweep's own internal shape (that is `feasibility_panel.js`'s
// job, at render time, not this module's).
export function hasSweep(sc) {
  return !!(sc && sc.sweep && typeof sc.sweep === 'object');
}

// Deliberately NOT wrapped by `attachM264Panels` (unlike the ordinary/RPO cases,
// which both feed into it via `defaultLayoutTreeForScenario` below) -- a sweep-shaped
// default replaces the map panel with the feasibility panel entirely rather than
// adding a 5th/7th leaf on top of the M26.4 three-panel set: the brief's own required
// shape is exactly "sidebar, the feasibility panel ..., run products, and console" --
// four leaves, no 2D map. (A user who wants the map back for a sweep scenario can
// still reach it through the M26.5 pane chooser/header-menu, same as any other
// registered panel type -- see `REGISTERED_PANEL_TYPES` above.)
export function buildSweepStudyLayout() {
  return createSplit(
    'row',
    0.22,
    [
      createLeaf('sidebar', { id: 'pane-sidebar' }),
      createSplit(
        'row',
        0.7, // feasibility gets the wide/primary share, matching attachM264Panels' own 0.7 for its own "main content" side
        [
          createLeaf(FEASIBILITY_PANEL_ID, { id: 'pane-feasibility' }),
          createSplit(
            'column',
            0.5,
            [
              createLeaf(RUN_PRODUCTS_PANEL_ID, { id: 'pane-run-products' }),
              createLeaf(CONSOLE_PANEL_ID, { id: 'pane-console-log' }),
            ],
            { id: 'split-sweep-run-products-console' },
          ),
        ],
        { id: 'split-sweep-feasibility-rest' },
      ),
    ],
    { id: 'split-sweep-sidebar-rest' },
  );
}

/**
 * The one real default-layout entry point `web/js/layout/layout_manager.js` calls (its
 * `applyDefaultForScenario`/`resetToDefault`/constructor's `defaultFactory`, all three --
 * see that module's own comment on why a single shared function rather than three
 * separate call sites each re-deciding the same branch). Selects `buildSweepStudyLayout()`
 * first when the scenario carries a `sweep` key (`hasSweep`, above) -- deliberately
 * BEFORE the RIC-frame/imagery dispatch `defaultLayoutForScenario` already does, so a
 * sweep-carrying scenario always gets the sweep layout even in the (today, never
 * actually occurring on any real publish route) case where it also happened to declare
 * a RIC frame; falls back to exactly `attachM264Panels(defaultLayoutForScenario(sc))`
 * for every other scenario -- BYTE-IDENTICAL to what `layout_manager.js` computed
 * before this task, so `defaultLayoutForScenario`'s and `attachM264Panels`' own asserted
 * output shapes (web/js/viewport_check.mjs, web/js/panels_check.mjs) are untouched.
 * `sc` may be `null` (no scenario loaded yet); `hasSweep(null)` is `false`, so this
 * degrades to the same pre-F5.1 default in that case too.
 * @param {object|null} sc a wire scenario object (or null)
 */
function _ordinaryOrSweepLayout(sc) {
  if (hasSweep(sc)) return buildSweepStudyLayout();
  return attachM264Panels(defaultLayoutForScenario(sc));
}

// ------------------------------------------------------- R3.5b: execution-profile default
// docs/open-questions.md question 201(d), verbatim: "the console panel lives in the
// execution profile's default layout only." This module's own top comment explains
// what "profile" even means to the browser today -- as of this task, exactly ONE new
// signal: `scenario.profileId` (`altavista/server.py`'s `Hub`, stamped from
// `create_app(profile=...)` the same way `scenario.imagery` already is). A scenario
// carrying no `profileId` at all (every scenario published before this change, and
// every test fixture that builds a `Hub` without one) is NOT execution-shaped by
// definition -- this degrades to "not execution" rather than guessing, exactly this
// task's own required rule.
export function isExecutionProfile(sc) {
  return !!(sc && sc.profileId === 'execution');
}

// Deliberately a SEPARATE, additive wrapper -- exactly the same posture
// `attachM264Panels` already takes relative to `defaultLayoutForScenario`/
// `buildRpoTripleViewportLayout` (see that function's own doc comment): every existing
// default-layout function stays byte-identical for every non-execution profile (or a
// profile-less scenario), because this wrapper is only ever CALLED for
// `isExecutionProfile(sc) === true`. It works generically over whatever tree
// `_ordinaryOrSweepLayout` already decided (the 5-leaf ordinary tree, the 7-leaf RPO
// tree, or the 4-leaf sweep tree) -- adding exactly one new leaf, never touching what
// was already there, mirroring `attachM264Panels`'s own "never inspects or reshapes
// `tree`'s own shape" contract.
//
// Placement/share (this task's own open design decision, decided here): a narrow
// right-hand strip at a 0.78/0.22 split -- the SAME 0.22 share `buildBaseSidebarViewportLayout`
// already uses for its own sidebar, and deliberately narrower than `attachM264Panels`'s
// own 0.3 share for its whole 3-panel stack (run products + map + console): the command
// console is one focused, occasional-use panel (propose/review/authorize), not a
// permanently-referenced dashboard the way run products/map/console are, so it does not
// need an equal claim on screen space. It is the OUTERMOST wrapper (applied after
// `_ordinaryOrSweepLayout`, never nested inside it), so every leaf that shape already
// contained (sidebar, viewport, and -- for the ordinary/RPO cases -- run products/map/
// console, or -- for the sweep case -- feasibility/run products/console) keeps the
// exact proportions it already had relative to EACH OTHER; only the whole thing shrinks
// to make room for this one new strip.
export function attachCommandPanel(tree) {
  return createSplit(
    'row',
    0.78,
    [
      tree,
      createLeaf(COMMAND_PANEL_ID, { id: 'pane-command-console' }),
    ],
    { id: 'split-command-console' },
  );
}

// --------------------------------------------------- Round 6 (question 231): Layers panel
// The lead's ratification, verbatim (question 231, "Also next round"): "the Layers panel
// joins the execution and design default layouts once F5.1's regression guard is updated
// with it." `isDesignProfile` mirrors `isExecutionProfile` exactly -- same signal
// (`scenario.profileId`), same degrade-never-guess posture: a scenario carrying no
// `profileId` at all is neither execution- nor design-shaped, so it is unaffected by
// either check.
export function isDesignProfile(sc) {
  return !!(sc && sc.profileId === 'design');
}

// Deliberately a SEPARATE, additive wrapper -- the exact same posture `attachCommandPanel`
// already takes (see that function's own doc comment): every existing default-layout
// function stays byte-identical for every OTHER profile (feasibility, analysis, or a
// profile-less scenario), because this wrapper is only ever called when
// `isExecutionProfile(sc) || isDesignProfile(sc)` is true (see `defaultLayoutTreeForScenario`
// below). Works generically over whatever tree `_ordinaryOrSweepLayout` already decided,
// exactly like `attachCommandPanel`.
//
// Placement/share (this task's own open design decision, decided here): a narrow
// right-hand strip, same style as `attachCommandPanel`'s own 0.78/0.22 strip but a touch
// narrower -- 0.82/0.18 -- because the Layers panel's own content (web/js/panels/
// layers_panel.js: a catalogued-tile-set table plus two streaming-budget numbers) needs
// less width than the command console's rationale/decision/trail/token UI. In
// `defaultLayoutTreeForScenario`, this wrapper is applied BEFORE `attachCommandPanel`
// (i.e. nested INSIDE it) for the execution profile, so the command console --
// "one focused, occasional-use panel", `attachCommandPanel`'s own doc comment -- keeps
// the true outermost/rightmost position it already had; the Layers panel sits just
// inside it, sharing the rest of the window with the sidebar/viewport/run-products/map/
// console (or feasibility) content exactly as before, only slightly narrower to make
// room for this one new strip. For the design profile, which gets no command console at
// all, this is the only extra wrapper applied, so the Layers panel is the outermost
// strip there.
export function attachLayersPanel(tree) {
  return createSplit(
    'row',
    0.82,
    [
      tree,
      createLeaf(LAYERS_PANEL_ID, { id: 'pane-layers' }),
    ],
    { id: 'split-layers-panel' },
  );
}

/**
 * The one real default-layout entry point (unchanged call sites, see this function's
 * own pre-R3.5b doc comment above `_ordinaryOrSweepLayout`) -- wraps the result with
 * `attachLayersPanel` when, and only when, `isExecutionProfile(sc) || isDesignProfile(sc)`
 * is true (question 231, Round 6), and further wraps with `attachCommandPanel` when,
 * and only when, `isExecutionProfile(sc)` is true (R3.5b, unchanged). For every other
 * profile (feasibility, analysis) or a profile-less scenario (`sc` may still be `null`,
 * exactly as before), this is BYTE-IDENTICAL to what this function computed before this
 * task: both `isExecutionProfile(null)` and `isDesignProfile(null)` are `false`, so the
 * pre-existing "no scenario loaded yet" behaviour is completely unaffected.
 * @param {object|null} sc a wire scenario object (or null)
 */
export function defaultLayoutTreeForScenario(sc) {
  let tree = _ordinaryOrSweepLayout(sc);
  if (isExecutionProfile(sc) || isDesignProfile(sc)) tree = attachLayersPanel(tree);
  if (isExecutionProfile(sc)) tree = attachCommandPanel(tree);
  return tree;
}

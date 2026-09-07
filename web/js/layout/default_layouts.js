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
export const REGISTERED_PANEL_TYPES = [
  { panelId: 'viewport', label: '3D Viewport', factory: true },
  { panelId: MAP_PANEL_ID, label: '2D Map' },
  { panelId: RUN_PRODUCTS_PANEL_ID, label: 'Run Products & Scores' },
  { panelId: CONSOLE_PANEL_ID, label: 'Console / Log' },
  { panelId: 'sidebar', label: 'Sidebar' },
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

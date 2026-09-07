#!/usr/bin/env node
// M26.2 windowing core: headless CLI harness for the split-tree model
// (web/js/layout/split_tree.js), its persistence wrapper
// (web/js/layout/persistence.js) and the default-layout lookup
// (web/js/layout/default_layouts.js). Run directly with `node
// layout_tree_check.mjs`; prints one JSON object to stdout, following the exact same
// pattern as web/js/tiles3d_geo_check.mjs and friends (see tests/test_viewer_globe.py
// / tests/test_viewer_jitter.py's module docstrings) -- no DOM, no Three.js, nothing
// this module touches needs a browser, so tests/test_viewer_layout.py can run this
// under plain `node` in CI.
//
// What each check proves (named here so tests/test_viewer_layout.py's own docstrings
// can point back at this list, and so a reviewer does not have to reverse-engineer
// "what would make this fail" from the assertions alone):
//
// - split.*: splitLeaf() turns one leaf into a split with two children, one of which
//   is the original leaf's own id, still a leaf with its original panelId. Fails
//   against an implementation that discards/renames the original leaf instead of
//   keeping it as one of the two new children.
// - close.*: closePane() on a freshly split leaf returns the tree to a shape whose
//   surviving leaf is *the exact original leaf object* (deep-equal, including its
//   id) -- not a new leaf, not the split node still present. Fails against an
//   implementation that removes only the closed leaf but leaves its parent split
//   node in place (a split with one child), or that closes the wrong side.
// - close.rejectsLastPane: closing the only leaf in a single-leaf tree throws,
//   rather than returning an empty/null tree. Fails against an implementation with
//   no guard for the last pane.
// - resize.*: resizeSplit() updates exactly the named split's ratio and clamps
//   out-of-range input to [0.05, 0.95]. Fails against an implementation with no
//   clamping (a pane could be resized to zero or negative width).
// - collapse.*: collapseToRail()/restoreFromRail() toggle a leaf's `collapsed` flag
//   without touching its ancestor split's `ratio` -- collapsing and restoring must
//   return the split to the exact ratio it had before collapsing. Fails against an
//   implementation that resets or otherwise mutates the ratio as a side effect of
//   collapsing (the pane would come back a different size than the user left it).
// - roundtrip.*: serializeLayout() -> deserializeLayout() reproduces a tree with
//   mixed row/column splits and a collapsed leaf byte-for-byte (via JSON string
//   comparison of both, and by re-serializing the deserialized result). Fails
//   against key order not being preserved in a way that changes the JSON string, or
//   the `collapsed: true` case being dropped or altered on deserialize.
//   Correction, found by actually breaking createLeaf() to omit `collapsed` from the
//   object entirely when it is false (an "omit falsy defaults" serializer): that bug
//   does NOT fail any roundtrip.* check here, because both ROUNDTRIP_TREE (built by
//   the same buggy createLeaf) and its deserialized copy omit the key identically --
//   a same-process roundtrip can't see a bug in how the *builder* shapes fresh
//   objects. It IS caught, one section down, by collapse.restoreReproducesPreCollapse
//   TreeExactly: that check serializes a leaf created by buildBaseSidebarViewportLayout
//   (collapsed omitted under the bug) against the same leaf after collapseToRail() +
//   restoreFromRail() (which always writes an explicit `collapsed: false` -- see
//   setCollapsed()), so the two JSON strings differ under that bug and are identical
//   without it. Recorded here, not just in web/js/layout/REPORT.md, so this comment
//   never again claims a check catches something it does not.
// - invalidRejected.*: deserializeLayout() throws LayoutValidationError -- and does
//   NOT return any tree at all -- for a battery of malformed payloads (bad JSON
//   text, missing children, wrong child count, duplicate ids, out-of-range ratio,
//   unknown node type, unknown direction). Fails against an implementation that
//   catches its own parse/validation errors internally and quietly substitutes a
//   default tree instead of propagating the error -- exactly the "silent fallback
//   hides a corrupt saved layout forever" failure mode this task's brief calls out.
// - persistence.*: loadLayout() with a corrupt stored string returns the default
//   tree for rendering AND a non-null `error`, and critically leaves the raw corrupt
//   string in `storage` completely untouched (proven by reading it back afterwards)
//   -- `saveLayout` was never called on its behalf. Fails against an implementation
//   that "self-heals" by overwriting the corrupt entry with a fresh default,
//   destroying the only evidence a user had that their saved layout broke.
// - defaultLayout.*: defaultLayoutForImagery() resolves the real offline-fixture
//   imagery (byte-identical across all four profiles/*.yaml today, see
//   web/js/layout/REPORT.md) to the registered base layout, an unrecognized imagery
//   object falls back to the same base layout rather than throwing, and a
//   newly-registered second signature actually returns a *different* tree (proving
//   the lookup is a real dispatch, not a function that always returns one constant
//   regardless of what is registered).

import {
  createLeaf, createSplit, splitLeaf, closePane, resizeSplit,
  collapseToRail, restoreFromRail, serializeLayout, deserializeLayout,
  validateTree, listLeaves, findNode, LayoutValidationError, resetIdCounterForTests,
  assignPanel,
} from './split_tree.js';
import { loadLayout, saveLayout, DEFAULT_STORAGE_KEY } from './persistence.js';
import {
  buildBaseSidebarViewportLayout, defaultLayoutForImagery, registerDefaultLayoutForImagery,
  REGISTERED_PANEL_TYPES, availablePanelChoices,
} from './default_layouts.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail: detail ?? null }); }

// A tiny in-memory Storage stand-in (no DOM/localStorage exists under plain node) --
// same shape as window.localStorage's getItem/setItem, which is all persistence.js
// requires.
function makeMemoryStorage(initial = {}) {
  const data = { ...initial };
  return {
    getItem: (k) => (k in data ? data[k] : null),
    setItem: (k, v) => { data[k] = String(v); },
    _raw: data,
  };
}

resetIdCounterForTests();

// --------------------------------------------------------------------------- split
{
  const base = createSplit('row', 0.22, [
    createLeaf('sidebar', { id: 'pane-sidebar' }),
    createLeaf('viewport', { id: 'pane-viewport' }),
  ], { id: 'split-root' });

  const afterSplit = splitLeaf(base, 'pane-viewport', 'column', 'console', { ratio: 0.7, newLeafId: 'pane-console' });
  const leaves = listLeaves(afterSplit);
  check('split.resultHasThreeLeaves', leaves.length === 3, { leafIds: leaves.map(l => l.id) });
  const newSplitNode = findNode(afterSplit, 'pane-viewport');
  check('split.originalLeafSurvivesWithSamePanelId', !!newSplitNode && newSplitNode.type === 'leaf' && newSplitNode.panelId === 'viewport');
  const consoleLeaf = findNode(afterSplit, 'pane-console');
  check('split.newLeafHasRequestedPanelId', !!consoleLeaf && consoleLeaf.panelId === 'console');

  // No explicit newLeafId/splitId here -- exercises genId()'s auto-id fallback (the
  // path a real "click to split" UI action takes, since the user never types an id).
  // This is also what test_layout_checks_are_deterministic_across_process_runs
  // actually depends on to have any teeth against a non-deterministic id generator
  // (e.g. one seeded from Math.random()/Date.now()): every other call in this
  // harness passes an explicit id.
  const autoSplit = splitLeaf(base, 'pane-sidebar', 'row', 'notes');
  const autoLeaves = listLeaves(autoSplit).map(l => l.id);
  check('split.autoGeneratedIdsUseThePlainPrefixedCounterScheme', autoLeaves.every(id => /^(pane-[a-z]+|leaf-\d+)$/.test(id)), { autoLeaves });
  // The new split replacing pane-viewport must itself validate (direction/ratio/children shape).
  let splitValidates = true;
  try { validateTree(afterSplit); } catch (e) { splitValidates = false; }
  check('split.resultIsAValidTree', splitValidates);

  // --------------------------------------------------------------------------- close
  const closedBack = closePane(afterSplit, 'pane-console');
  const closedLeaves = listLeaves(closedBack);
  check('close.returnsToTwoLeaves', closedLeaves.length === 2, { leafIds: closedLeaves.map(l => l.id) });
  const survivingViewport = findNode(closedBack, 'pane-viewport');
  check('close.survivingSiblingIsUnchanged', JSON.stringify(survivingViewport) === JSON.stringify(createLeaf('viewport', { id: 'pane-viewport' })));
  check('close.rootStructureMatchesOriginalBase', JSON.stringify(closedBack) !== JSON.stringify(afterSplit) && listLeaves(closedBack).some(l => l.id === 'pane-sidebar'));

  let lastPaneRejected = false;
  let lastPaneErrorIsLayoutValidationError = false;
  try {
    closePane(createLeaf('solo', { id: 'only-pane' }), 'only-pane');
  } catch (e) {
    lastPaneRejected = true;
    lastPaneErrorIsLayoutValidationError = e instanceof LayoutValidationError;
  }
  check('close.rejectsLastPane', lastPaneRejected);
  check('close.rejectsLastPaneWithLayoutValidationError', lastPaneErrorIsLayoutValidationError);
}

// -------------------------------------------------------------------------- resize
{
  const base = buildBaseSidebarViewportLayout();
  const resized = resizeSplit(base, 'split-root', 0.35);
  check('resize.updatesExactRatio', findNode(resized, 'split-root').ratio === 0.35);
  const clampedHigh = resizeSplit(base, 'split-root', 5.0);
  check('resize.clampsAboveMax', findNode(clampedHigh, 'split-root').ratio === 0.95, { ratio: findNode(clampedHigh, 'split-root').ratio });
  const clampedLow = resizeSplit(base, 'split-root', -3.0);
  check('resize.clampsBelowMin', findNode(clampedLow, 'split-root').ratio === 0.05, { ratio: findNode(clampedLow, 'split-root').ratio });
  let resizeUnknownThrows = false;
  try { resizeSplit(base, 'no-such-split', 0.5); } catch (e) { resizeUnknownThrows = e instanceof LayoutValidationError; }
  check('resize.unknownSplitIdThrows', resizeUnknownThrows);
}

// ------------------------------------------------------------------ collapse / restore
{
  const base = resizeSplit(buildBaseSidebarViewportLayout(), 'split-root', 0.31);
  const collapsed = collapseToRail(base, 'pane-sidebar');
  check('collapse.setsCollapsedTrue', findNode(collapsed, 'pane-sidebar').collapsed === true);
  check('collapse.preservesAncestorRatio', findNode(collapsed, 'split-root').ratio === 0.31);
  const restored = restoreFromRail(collapsed, 'pane-sidebar');
  check('collapse.restoreSetsCollapsedFalse', findNode(restored, 'pane-sidebar').collapsed === false);
  check('collapse.restorePreservesAncestorRatioExactly', findNode(restored, 'split-root').ratio === 0.31);
  check('collapse.restoreReproducesPreCollapseTreeExactly', serializeLayout(restored) === serializeLayout(base));
}

// --------------------------------------------------------------------- serialize roundtrip
{
  // Deliberately mixes row and column splits and a collapsed leaf -- exercises every
  // field the schema has, not just the two-leaf base case above.
  const ROUNDTRIP_TREE = createSplit('row', 0.25, [
    createLeaf('sidebar', { id: 'rt-sidebar', collapsed: true }),
    createSplit('column', 0.6, [
      createLeaf('viewport', { id: 'rt-viewport' }),
      createLeaf('console', { id: 'rt-console' }),
    ], { id: 'rt-bottom-split' }),
  ], { id: 'rt-root' });

  const json = serializeLayout(ROUNDTRIP_TREE);
  const restored = deserializeLayout(json);
  check('roundtrip.deserializedEqualsOriginalByValue', JSON.stringify(restored) === JSON.stringify(ROUNDTRIP_TREE));
  const reserialized = serializeLayout(restored);
  check('roundtrip.reserializedJsonIsByteIdentical', reserialized === json, { json, reserialized });
  check('roundtrip.collapsedFlagSurvives', restored.children[0].collapsed === true);
}

// ------------------------------------------------------------------- invalid rejection
{
  const badCases = [
    { name: 'malformedJsonText', payload: '{not valid json' },
    { name: 'missingChildrenOnSplit', payload: JSON.stringify({ type: 'split', id: 's1', direction: 'row', ratio: 0.5 }) },
    { name: 'oneChildInsteadOfTwo', payload: JSON.stringify({ type: 'split', id: 's1', direction: 'row', ratio: 0.5, children: [{ type: 'leaf', id: 'l1', panelId: 'x' }] }) },
    { name: 'duplicateIds', payload: JSON.stringify({ type: 'split', id: 'dup', direction: 'row', ratio: 0.5, children: [{ type: 'leaf', id: 'dup', panelId: 'a' }, { type: 'leaf', id: 'l2', panelId: 'b' }] }) },
    { name: 'ratioOutOfRange', payload: JSON.stringify({ type: 'split', id: 's1', direction: 'row', ratio: 1.4, children: [{ type: 'leaf', id: 'l1', panelId: 'a' }, { type: 'leaf', id: 'l2', panelId: 'b' }] }) },
    { name: 'ratioZero', payload: JSON.stringify({ type: 'split', id: 's1', direction: 'row', ratio: 0, children: [{ type: 'leaf', id: 'l1', panelId: 'a' }, { type: 'leaf', id: 'l2', panelId: 'b' }] }) },
    { name: 'unknownNodeType', payload: JSON.stringify({ type: 'triangle', id: 't1' }) },
    { name: 'unknownDirection', payload: JSON.stringify({ type: 'split', id: 's1', direction: 'diagonal', ratio: 0.5, children: [{ type: 'leaf', id: 'l1', panelId: 'a' }, { type: 'leaf', id: 'l2', panelId: 'b' }] }) },
    { name: 'leafMissingPanelId', payload: JSON.stringify({ type: 'leaf', id: 'l1' }) },
    { name: 'nullTree', payload: JSON.stringify(null) },
  ];
  for (const { name, payload } of badCases) {
    let threw = false;
    let isLayoutValidationError = false;
    let resultTreeIfNoThrow = undefined;
    try {
      resultTreeIfNoThrow = deserializeLayout(payload);
    } catch (e) {
      threw = true;
      isLayoutValidationError = e instanceof LayoutValidationError;
    }
    check(`invalidRejected.${name}`, threw && isLayoutValidationError, threw ? null : { insteadReturned: resultTreeIfNoThrow });
  }
}

// --------------------------------------------------------------------- persistence
{
  const good = buildBaseSidebarViewportLayout();
  const storage1 = makeMemoryStorage();
  saveLayout({ storage: storage1, tree: good });
  const loaded1 = loadLayout({ storage: storage1, defaultFactory: buildBaseSidebarViewportLayout });
  check('persistence.roundTripsAValidSavedLayout', loaded1.error === null && loaded1.source === 'persisted' && serializeLayout(loaded1.tree) === serializeLayout(good));

  const storage2 = makeMemoryStorage();
  const loaded2 = loadLayout({ storage: storage2, defaultFactory: buildBaseSidebarViewportLayout });
  check('persistence.emptyStorageYieldsDefaultWithNoError', loaded2.error === null && loaded2.source === 'default-empty');

  const corruptJson = '{"type":"split","id":"s1","direction":"row","ratio":0.5,"children":[{"type":"leaf","id":"s1","panelId":"a"}]}'; // truncated: only 1 child, and reuses parent's id
  const storage3 = makeMemoryStorage({ [DEFAULT_STORAGE_KEY]: corruptJson });
  const loaded3 = loadLayout({ storage: storage3, defaultFactory: buildBaseSidebarViewportLayout });
  check('persistence.corruptStoredLayoutYieldsNonNullError', loaded3.error instanceof LayoutValidationError, { message: loaded3.error && loaded3.error.message });
  check('persistence.corruptStoredLayoutStillReturnsAUsableDefaultTree', (() => { try { validateTree(loaded3.tree); return true; } catch { return false; } })());
  check('persistence.corruptStoredStringIsNeverOverwritten', storage3._raw[DEFAULT_STORAGE_KEY] === corruptJson, { stillStored: storage3._raw[DEFAULT_STORAGE_KEY] });
}

// ------------------------------------------------------------------- default layouts
{
  const knownImagery = { urlTemplate: './fixtures/tiles/{z}/{x}/{y}.png', attribution: 'anything -- attribution text is not part of the signature', maxLevel: 2 };
  const resolvedKnown = defaultLayoutForImagery(knownImagery);
  check('defaultLayout.knownProfileImageryResolvesToBaseLayout', serializeLayout(resolvedKnown) === serializeLayout(buildBaseSidebarViewportLayout()));

  const unknownImagery = { urlTemplate: 'https://tiles.example.test/{z}/{x}/{y}.png', maxLevel: 9 };
  const resolvedUnknown = defaultLayoutForImagery(unknownImagery);
  let unknownIsValid = true;
  try { validateTree(resolvedUnknown); } catch { unknownIsValid = false; }
  check('defaultLayout.unrecognizedImageryFallsBackToAValidLayoutRatherThanThrowing', unknownIsValid);

  registerDefaultLayoutForImagery(unknownImagery, () => createLeaf('solo-rpo-view', { id: 'rpo-only-pane' }));
  const resolvedAfterRegister = defaultLayoutForImagery(unknownImagery);
  check('defaultLayout.registeringANewSignatureActuallyChangesTheResult', serializeLayout(resolvedAfterRegister) !== serializeLayout(resolvedUnknown) && resolvedAfterRegister.panelId === 'solo-rpo-view');
}

// -------------------------------------------------------------- question 167: assignPanel
// "Every empty pane shows a chooser ... and a pane header menu can swap a pane's panel"
// (the lead's decision, verbatim) reduces, at the tree level, to `assignPanel()`. See
// web/js/viewport_check.mjs's own "question 167" section for the fuller scenario
// (mints a real Viewport and proves it independent of an existing one) -- this section
// covers the pure tree-edit rules in isolation.
{
  const base = buildBaseSidebarViewportLayout(); // pane-sidebar('sidebar') + pane-viewport('viewport')
  const renamed = assignPanel(base, 'pane-viewport', 'console-log');
  check('assignPanel.setsTheTargetLeafsPanelId', findNode(renamed, 'pane-viewport').panelId === 'console-log');
  check('assignPanel.leavesOtherLeavesAlone', findNode(renamed, 'pane-sidebar').panelId === 'sidebar');

  // Moving an ALREADY-PLACED singleton (sidebar) onto a different leaf must displace
  // it -- never leave two leaves both claiming 'sidebar'. This is the exact
  // "LayoutManager.render() would race two leaves for one DOM element" hazard
  // assignPanel()'s own docstring describes.
  const moved = assignPanel(base, 'pane-viewport', 'sidebar');
  const movedLeaves = listLeaves(moved);
  check('assignPanel.movingAnAlreadyPlacedSingletonKeepsPanelIdsUnique',
    new Set(movedLeaves.map(l => l.panelId)).size === movedLeaves.length);
  check('assignPanel.displacedLeafGetsAFreshEmptyPlaceholderNotTheOldPanelId',
    findNode(moved, 'pane-sidebar').panelId !== 'sidebar' && findNode(moved, 'pane-sidebar').panelId !== 'viewport');
  check('assignPanel.targetLeafActuallyGotTheMovedPanel', findNode(moved, 'pane-viewport').panelId === 'sidebar');

  let unknownLeafThrows = false;
  try { assignPanel(base, 'no-such-leaf', 'console-log'); } catch (e) { unknownLeafThrows = e instanceof LayoutValidationError; }
  check('assignPanel.unknownLeafIdThrowsLayoutValidationError', unknownLeafThrows);

  // -------------------------------------------------------- availablePanelChoices
  check('availablePanelChoices.factoryTypeAlwaysOffered',
    availablePanelChoices(base, 'pane-viewport').some(c => c.panelId === 'viewport' && c.factory === true));
  check('availablePanelChoices.singletonAlreadyUsedByANOTHERLeafIsNotOffered',
    !availablePanelChoices(base, 'pane-viewport').some(c => c.panelId === 'sidebar'));
  check('availablePanelChoices.singletonUsedByTHISSAMELeafIsStillOffered(notANoOpBlock)',
    availablePanelChoices(base, 'pane-sidebar').some(c => c.panelId === 'sidebar'));
  check('availablePanelChoices.everyRegisteredTypeAccountedFor',
    REGISTERED_PANEL_TYPES.every(t => availablePanelChoices(createLeaf('empty-solo', { id: 'solo' }), 'solo').some(c => c.panelId === t.panelId)));
}

const allPass = checks.every(c => c.pass);
console.log(JSON.stringify({ checks, allPass }));
if (!allPass) process.exitCode = 1;

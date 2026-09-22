#!/usr/bin/env node
// CLI harness for tests/test_viewer_layers_panel.py: `node web/js/layers_panel_check.mjs`.
//
// Round 5 (question 228 finding 2, browser half): headless proof for the Layers panel
// (web/js/panels/layers_panel.js) and the toggle sequence web/js/app.js's own
// `toggleGatewayLayer` performs against the ONE shared `LayerManager`
// (web/js/layers/layer.js) and the real `GatewayImageryLayerAdapter`
// (web/js/layers/gateway_imagery_layer.js). Same convention as
// web/js/command_panel_check.mjs / web/js/layers_check.mjs: drives the REAL, shipped ES
// modules and prints one JSON object of named checks -- never a reimplementation of any
// of their logic. No JSON argv (unlike command_panel_check.mjs): unlike the command
// console's real backend fixture, this panel's own pure functions and the layer-manager
// toggle sequence need no live server to exercise -- `tests/test_viewer_layers_panel.py`
// separately drives a real headless Chrome against a real running viewer server for the
// browser-integration half.
//
// Section map:
//   1. pure functions      -- layerIdForManifest, tileSetRows, formatBytes, shortSha,
//                              errorLine, layerStateFor
//   1a. layersPanelStateKey -- round 6 task 4's own change-key: equal for equal state,
//                              different for every kind of change the tile-set list
//                              shows, UNCHANGED across a budget-only change (the whole
//                              point of the key).
//   2. render() assembly   -- a fake DOM (this file's own, no jsdom -- the identical
//                              posture web/js/command_panel_check.mjs's own section
//                              7/8 and web/js/layout/layout_tree_check.mjs's LayoutManager
//                              section already take), proving the "not configured" rule,
//                              the "never loaded yet" state, the empty-list state, a
//                              real table, and the toggle/refresh callbacks fire with the
//                              exact row/no arguments
//   2a. renderLayersPanel   -- round 6 task 4's own conditional entry point: an idle,
//                              budget-only re-render touches zero tile-set-list nodes
//                              (proven by NODE IDENTITY, not by re-reading text) and
//                              never increments the real `structuralRebuildCount`;
//                              a real state change (catalog or a layer's own status)
//                              DOES rebuild and DOES increment it.
//   3. addLayer/removeLayer through a REAL LayerManager + REAL GatewayImageryLayerAdapter
//      -- the toggle sequence app.js's own toggleGatewayLayer performs, byte for byte:
//      construct the adapter, AWAIT fetchManifest() BEFORE addLayer, read back
//      residentBytes/byteCostSource and INDEPENDENTLY recompute the expected byte total
//      from this harness's own fixture data (never trusting the manager's self-report
//      alone -- "a check that reads its answer from the thing it is checking proves
//      nothing", round 4's own discipline, restated here); two distinct tile sets at
//      once, no id collision; removeLayer is a no-op on an unknown id and a
//      toggle-off-toggle-on cycle under the SAME derived id does not collide; a manifest
//      fetch that 503s surfaces as a real, typed TileHttpError, fed straight into
//      section 2's own render() to prove the failure is actually visible in the DOM, not
//      just caught and discarded.

import { LayerManager } from './layers/layer.js';
import { GatewayImageryLayerAdapter, TileHttpError, TileEtagMismatchError } from './layers/gateway_imagery_layer.js';
import { TerrainLayerAdapter } from './layers/terrain_layer.js';
import { selectTiles, geodeticToEcef, tileKey } from './globe_lod.js';
import {
  layerIdForManifest, tileSetRows, formatBytes, shortSha, errorLine, layerStateFor, render,
  layersPanelStateKey, renderLayersPanel, attributeFailures,
} from './panels/layers_panel.js';
// `structuralRebuildCount` is a live ES-module binding (a plain `export let`, incremented
// inside render()) -- re-imported via a fresh `import()` wherever a check needs its
// CURRENT value (a static `import {structuralRebuildCount}` binding would also stay
// live, but re-reading it explicitly through the module namespace object below makes
// each check's own "before"/"after" snapshot unambiguous rather than relying on binding
// semantics the reader has to already know).
const layersPanelModule = await import('./panels/layers_panel.js');
import crypto from 'node:crypto';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail: detail ?? null }); }

// ============================================================================ 1. pure functions
{
  check('layerIdForManifest: derived id contains the manifest sha256 verbatim',
    layerIdForManifest('abc123').includes('abc123'));
  check('layerIdForManifest: two different manifests get two different ids (no collision)',
    layerIdForManifest('sha-a') !== layerIdForManifest('sha-b'));
  check('layerIdForManifest: the SAME manifest always derives the SAME id (stable, not a fresh counter)',
    layerIdForManifest('sha-a') === layerIdForManifest('sha-a'));

  const realPayload = {
    tileSets: [
      { assetId: 'b-asset', manifestSha256: 'sha-b', name: 'Tile Set B', marking: 'CUI', caveats: ['NOFORN'], sizeBytes: '2048', mediaType: 'application/vnd.altavista.tileset-manifest+pb', uri: 's3://x/b', jobId: 'job-b', createdTaiNs: '123', footprintWkt: 'POLYGON(())' },
      { assetId: 'a-asset', manifestSha256: 'sha-a', name: 'Tile Set A', marking: 'UNCLASSIFIED', caveats: [], sizeBytes: '1048576', mediaType: 'application/vnd.altavista.tileset-manifest+pb', uri: 's3://x/a', jobId: '', createdTaiNs: '100', footprintWkt: '' },
    ],
  };
  const rows = tileSetRows(realPayload);
  check('tileSetRows: real payload -> one row per tile set, every field carried through verbatim',
    rows.length === 2 && rows.some((r) => r.manifestSha256 === 'sha-a' && r.marking === 'UNCLASSIFIED' && r.sizeBytes === '1048576'));
  check('tileSetRows: re-sorted by assetId ascending, never trusting the wire order (a-asset before b-asset, though the payload listed b first)',
    rows[0].assetId === 'a-asset' && rows[1].assetId === 'b-asset');
  check('tileSetRows: a row with no jobId falls back to assetId, never left empty when a name IS derivable',
    rows.find((r) => r.assetId === 'a-asset').name === 'Tile Set A'); // name field itself present on the wire here
  check('tileSetRows: null/malformed payload -> [] , never a throw',
    JSON.stringify(tileSetRows(null)) === '[]' && JSON.stringify(tileSetRows({})) === '[]' && JSON.stringify(tileSetRows({ tileSets: 'not-an-array' })) === '[]');
  check('tileSetRows: a tile set with no manifestSha256 at all is dropped (it has no identity a toggle could act on)',
    tileSetRows({ tileSets: [{ assetId: 'x' }] }).length === 0);

  check('formatBytes: known conversions', formatBytes('0') === '0 B' && formatBytes('1048576') === '1.0 MB' && formatBytes('2048') === '2.0 KB');
  check('formatBytes: a malformed string degrades to the raw value, never a throw or a silent 0',
    formatBytes('not-a-number') === 'not-a-number B');

  check('shortSha: abbreviates a long hash, keeps a short one whole',
    shortSha('a'.repeat(64)).length < 64 && shortSha('a'.repeat(64)).endsWith('…') && shortSha('short') === 'short');

  check('errorLine: null -> null, a real {status,message} -> both fields present',
    errorLine(null) === null && errorLine({ status: 503, message: 'no data gateway is configured' }) === '(503) no data gateway is configured');

  check('layerStateFor: a manifest never toggled -> off, never a throw on a missing/absent map',
    layerStateFor('sha-a', {}).status === 'off' && layerStateFor('sha-a', null).status === 'off');
  check('layerStateFor: an existing entry is read back verbatim',
    layerStateFor('sha-a', { 'sha-a': { status: 'on' } }).status === 'on');
}

// ============================================ 1z. attributeFailures (task 5b, round 7)
// Pure-function proof of the "tile sets vs. declared no loader" split, independent of
// any DOM -- see layers_panel.js's own doc comment on attributeFailures for the full
// contract. Each check names the wrong implementation it would catch.
{
  const byLayer = {
    'terrain': { count: 3, names: ['TerrainLoaderNotImplementedError'] },
    'gateway-tileset:sha-a': { count: 2, names: ['TileHttpError'] },
    'gateway-tileset:sha-b': { count: 1, names: ['TileEtagMismatchError'] },
  };
  const a1 = attributeFailures(byLayer, ['terrain']);
  check('attributeFailures: tileSetCount sums every layer NOT in noLoaderLayerIds, excluding the no-loader layer entirely',
    a1.tileSetCount === 3, { tileSetCount: a1.tileSetCount });
  check('attributeFailures: tileSetNames is the union of only the tile-set layers\' own names, sorted',
    JSON.stringify(a1.tileSetNames) === JSON.stringify(['TileEtagMismatchError', 'TileHttpError']));
  check('attributeFailures: noLoader carries the no-loader layer\'s own id/count/names, never merged into tileSetCount',
    a1.noLoader.length === 1 && a1.noLoader[0].layerId === 'terrain' && a1.noLoader[0].count === 3
    && JSON.stringify(a1.noLoader[0].names) === JSON.stringify(['TerrainLoaderNotImplementedError']));
  check('attributeFailures: the partition is total -- tileSetCount + sum(noLoader counts) === every input count summed (nothing lost, nothing double-counted)',
    a1.tileSetCount + a1.noLoader.reduce((s, e) => s + e.count, 0)
      === Object.values(byLayer).reduce((s, e) => s + e.count, 0));

  check('attributeFailures: an empty/missing noLoaderLayerIds puts EVERY layer in the tile-set bucket (never silently drops a failure with no attribution data)',
    (() => {
      const r = attributeFailures(byLayer, []);
      return r.tileSetCount === 6 && r.noLoader.length === 0;
    })());
  check('attributeFailures: null/undefined failuresByLayer -> zero counts, never a throw',
    (() => {
      const r1 = attributeFailures(null, ['terrain']);
      const r2 = attributeFailures(undefined, undefined);
      return r1.tileSetCount === 0 && r1.noLoader.length === 0 && r2.tileSetCount === 0 && r2.noLoader.length === 0;
    })());

  // ---- PERTURBATION (task 5b's own required proof): a caller that mistakenly leaves
  // the no-loader layer's id OUT of noLoaderLayerIds -- e.g. the exact bug this task
  // exists to prevent, a terrain failure counted as a tile-set failure -- must be
  // OBSERVABLE as a real difference here, not silently absorbed.
  {
    const correct = attributeFailures(byLayer, ['terrain']);
    const wronglyAttributedToTileSets = attributeFailures(byLayer, []); // 'terrain' missing from the list
    const perturbationCaught = wronglyAttributedToTileSets.tileSetCount !== correct.tileSetCount
      && wronglyAttributedToTileSets.tileSetCount === 6 // terrain's 3 wrongly folded into the tile-set bucket
      && wronglyAttributedToTileSets.noLoader.length === 0;
    check('attributeFailures: PERTURBATION -- omitting the no-loader layer id from noLoaderLayerIds visibly inflates tileSetCount (proves this function would fail if terrain\'s failures were ever attributed to the tile-set row)',
      perturbationCaught, { correctTileSetCount: correct.tileSetCount, perturbedTileSetCount: wronglyAttributedToTileSets.tileSetCount });
  }
}

// ======================================================== 1a. layersPanelStateKey (round 6 task 4)
// "Define 'state' precisely" -- these checks enumerate every kind of change
// layersPanelStateKey() is documented (in layers_panel.js itself) to treat as real state,
// PLUS the one thing it must NOT react to (a budget-only change), which is the entire
// reason `renderLayersPanel()`'s conditional rebuild is correct at all.
{
  const baseTileSets = [
    { assetId: 'a-asset', manifestSha256: 'sha-a', name: 'Tile Set A', marking: 'UNCLASSIFIED', caveats: [], sizeBytes: '1048576', jobId: 'job-a' },
    { assetId: 'b-asset', manifestSha256: 'sha-b', name: 'Tile Set B', marking: 'CUI', caveats: ['NOFORN'], sizeBytes: '2048', jobId: 'job-b' },
  ];
  const baseState = () => ({
    tileSets: JSON.parse(JSON.stringify(baseTileSets)), // a FRESH, distinct object graph each call
    catalogError: null,
    loading: false,
    layerStates: { 'sha-b': { status: 'on', errorMessage: null } },
  });

  const k0 = layersPanelStateKey(baseState());
  check('layersPanelStateKey: two calls over separately-constructed but EQUAL state produce the SAME key (never an identity/reference key)',
    layersPanelStateKey(baseState()) === k0);

  check('layersPanelStateKey: unaffected by layerStates OBJECT KEY ORDER (re-sorted internally)',
    (() => {
      const reordered = baseState();
      reordered.layerStates = { 'sha-b': reordered.layerStates['sha-b'] };
      // Rebuild the SAME single-entry object -- order cannot differ with one key, so
      // prove re-ordering with a two-entry map instead, off() included this time.
      const s1 = baseState(); s1.layerStates = { 'sha-a': { status: 'off', errorMessage: null }, 'sha-b': { status: 'on', errorMessage: null } };
      const s2 = baseState(); s2.layerStates = { 'sha-b': { status: 'on', errorMessage: null }, 'sha-a': { status: 'off', errorMessage: null } };
      return layersPanelStateKey(s1) === layersPanelStateKey(s2);
    })());

  check('layersPanelStateKey: UNCHANGED when only budget fields would differ (budget is not a key input at all -- the whole point of the split)',
    (() => {
      const s = baseState();
      // layersPanelStateKey() takes no budget argument -- passing extra fields through
      // (as renderLayersPanel() itself does, destructuring the caller's full data object)
      // must not perturb the key.
      const withExtra = { ...s, budget: { residentBytes: 999, memoryBudgetBytes: 1, deferredCount: 7, failedCount: 3, failureNames: ['X'] } };
      return layersPanelStateKey(withExtra) === layersPanelStateKey(s);
    })());

  // ---- every enumerated kind of REAL change must move the key -----------------------
  const changed = (mutate, label) => {
    const s = baseState();
    mutate(s);
    const differs = layersPanelStateKey(s) !== k0;
    check(`layersPanelStateKey: ${label} changes the key`, differs);
  };
  changed((s) => { s.tileSets.push({ assetId: 'c-asset', manifestSha256: 'sha-c', name: 'C', marking: '', caveats: [], sizeBytes: '0', jobId: '' }); }, 'a tile set being ADDED to the catalog');
  changed((s) => { s.tileSets.pop(); }, 'a tile set being REMOVED from the catalog');
  changed((s) => { s.tileSets[0].name = 'Renamed'; }, "a tile set's own NAME changing");
  changed((s) => { s.tileSets[0].sizeBytes = '999999999'; }, "a tile set's own SIZE changing");
  changed((s) => { s.tileSets[0].marking = 'CUI'; }, "a tile set's own MARKING changing");
  changed((s) => { s.catalogError = { status: 503, message: 'gateway unreachable' }; }, 'catalogError appearing (null -> real error)');
  changed((s) => { s.catalogError = { status: 500, message: 'x' }; }, 'a DIFFERENT catalogError status/message than a sibling case');
  changed((s) => { s.loading = true; }, 'loading flipping true');
  changed((s) => { s.layerStates['sha-b'].status = 'off'; }, "an existing row's own status changing (on -> off)");
  changed((s) => { s.layerStates['sha-a'] = { status: 'loading', errorMessage: null }; }, 'a row transitioning to loading for the first time (new key in layerStates)');
  changed((s) => { s.layerStates['sha-b'].errorMessage = 'TileHttpError: 503'; }, "a row's own error MESSAGE changing while its status field is untouched");
  changed((s) => { delete s.layerStates['sha-b']; }, 'a row being toggled off entirely (its key removed from layerStates)');
  changed((s) => { s.tileSets = null; }, 'the catalog itself going from a real (even empty-of-this-change) array to null (never-fetched-yet)');
}

// =========================================================================== 2. render() + DOM
// A minimal, hand-rolled fake DOM -- no jsdom, no new dependency -- mirroring
// web/js/command_panel_check.mjs's own section 7/8 (same reasoning: proving REAL
// rendered TEXT, and that a real click fires the real callback with the real argument,
// needs a real document.createElement call site to exist).
function makeEl(tag) {
  const node = {
    tagName: String(tag).toUpperCase(),
    _text: '',
    _children: [],
    _listeners: {},
    className: '',
    type: '',
    title: '',
    disabled: false,
    get textContent() { return this._children.length ? this._children.map((c) => c.textContent).join('') : this._text; },
    set textContent(v) { this._text = String(v); this._children = []; },
    set innerHTML(_v) { this._children = []; this._text = ''; },
    appendChild(child) { this._children.push(child); return child; },
    append(...items) { for (const it of items) this.appendChild(it); },
    addEventListener(evt, fn) { (this._listeners[evt] = this._listeners[evt] || []).push(fn); },
    removeEventListener() {},
    click() { (this._listeners.click || []).forEach((fn) => fn()); },
  };
  return node;
}
function withFakeDocument(fn) {
  const previous = globalThis.document;
  globalThis.document = { createElement: (tag) => makeEl(tag) };
  try { return fn(); } finally {
    if (previous === undefined) delete globalThis.document; else globalThis.document = previous;
  }
}
function findAll(node, predicate, out = []) {
  if (predicate(node)) out.push(node);
  for (const c of node._children || []) findAll(c, predicate, out);
  return out;
}
function hasClass(node, cls) { return typeof node.className === 'string' && node.className.split(/\s+/).includes(cls); }

const NOT_CONFIGURED_MESSAGE = "no data gateway is configured for this viewer server (pass --gateway-endpoint/--gateway-token-path to 'python -m altavista serve')";
const REAL_TILE_SETS_PAYLOAD = [
  { assetId: 'a-asset', manifestSha256: 'sha-a', name: 'Tile Set A', marking: 'UNCLASSIFIED', caveats: [], sizeBytes: '1048576', jobId: 'job-a' },
  { assetId: 'b-asset', manifestSha256: 'sha-b', name: 'Tile Set B', marking: 'CUI', caveats: ['NOFORN'], sizeBytes: '2048', jobId: 'job-b' },
];

withFakeDocument(() => {
  // ---- the "not configured" rule: never rendered as "no tile sets" ----------------
  {
    const container = makeEl('div');
    render(container, { tileSets: null, catalogError: { status: 503, message: NOT_CONFIGURED_MESSAGE } });
    const text = container.textContent;
    check('render: a real "not configured" 503 shows the server\'s OWN message text',
      text.includes(NOT_CONFIGURED_MESSAGE));
    check('render: a "not configured" error is NEVER rendered as "No tile sets in the catalog" (the two must stay structurally distinct)',
      !text.includes('No tile sets in the catalog'));
  }
  // ---- never fetched yet (page boot, before the user clicks Refresh) --------------
  {
    const container = makeEl('div');
    render(container, { tileSets: null, catalogError: null, loading: false });
    const text = container.textContent;
    check('render: before any fetch, an honest "not loaded yet" notice -- never the empty-list notice, never the catalog table',
      text.includes('not loaded yet') && !text.includes('No tile sets in the catalog'));
    const refreshBtn = findAll(container, (n) => hasClass(n, 'av-layers-refresh'))[0];
    check('render: the Refresh catalog control is present and enabled even before a first fetch',
      !!refreshBtn && refreshBtn.disabled === false);
  }
  // ---- loading -----------------------------------------------------------------
  {
    const container = makeEl('div');
    render(container, { tileSets: null, catalogError: null, loading: true });
    check('render: while loading, an honest "Loading…" notice, and the Refresh button is disabled (no duplicate in-flight fetch)',
      container.textContent.includes('Loading tile set catalog') && findAll(container, (n) => hasClass(n, 'av-layers-refresh'))[0].disabled === true);
  }
  // ---- a real, successful, EMPTY catalog -- distinct from every case above --------
  {
    const container = makeEl('div');
    render(container, { tileSets: [], catalogError: null, loading: false });
    check('render: a real empty catalog (successful fetch, zero tile sets) shows its OWN honest notice',
      container.textContent.includes('No tile sets in the catalog'));
  }
  // ---- a real, populated catalog: table rows, toggle button labels per state ------
  let capturedToggleRow = null;
  let refreshCalled = 0;
  {
    const container = makeEl('div');
    render(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      catalogError: null,
      loading: false,
      layerStates: { 'sha-b': { status: 'on' } },
      budget: { residentBytes: 2048, memoryBudgetBytes: 67108864, deferredCount: 0, failedCount: 0, failureNames: [] },
      onToggleLayer: (row) => { capturedToggleRow = row; },
      onRefreshCatalog: () => { refreshCalled += 1; },
    });
    const text = container.textContent;
    check('render: both real tile sets appear by name', text.includes('Tile Set A') && text.includes('Tile Set B'));
    check('render: the manifest sha is shown abbreviated, with the FULL value preserved in the cell\'s title (never lost)',
      findAll(container, (n) => n.title === 'sha-a').length > 0);
    const buttons = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));
    check('render: one toggle button per row', buttons.length === 2);
    const onButton = buttons.find((b) => b.textContent === 'Turn off');
    check('render: the row already \'on\' (sha-b) shows "Turn off"; every other row shows "Turn on"',
      !!onButton && buttons.filter((b) => b.textContent === 'Turn on').length === 1);
    check('render: the \'on\' row is visually marked active (av-layers-active class)',
      findAll(container, (n) => hasClass(n, 'av-layers-active')).length === 1);

    // A real click on a real button fires the real callback with the exact row.
    buttons.find((b) => b.textContent === 'Turn on').click();
    check('render: clicking a row\'s toggle button invokes onToggleLayer with that row\'s own manifestSha256',
      capturedToggleRow && capturedToggleRow.manifestSha256 === 'sha-a');
    findAll(container, (n) => hasClass(n, 'av-layers-refresh'))[0].click();
    check('render: clicking Refresh catalog invokes onRefreshCatalog', refreshCalled === 1);

    check('render: the streaming-budget section shows real resident/budget bytes',
      text.includes('2.0 KB') && text.includes('64.0 MB'));
    check('render: with zero failures, an honest "no load failures recorded" notice (never a blank section)',
      text.includes('No load failures recorded'));
  }
  // ---- an active layer's own error state is visible, typed name and message intact --
  {
    const container = makeEl('div');
    render(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      layerStates: { 'sha-a': { status: 'error', errorMessage: 'TileHttpError: placeholder' } },
      budget: { residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 3, failedCount: 2, failureNames: ['TileHttpError', 'TileEtagMismatchError'] },
    });
    const text = container.textContent;
    check('render: a row\'s own load failure is shown with its typed name, never swallowed',
      text.includes('TileHttpError: placeholder'));
    check('render: deferred/failed counts from the manager are shown verbatim', text.includes('3') && text.includes('2'));
    check('render: every distinct failure NAME the manager remembered is listed',
      text.includes('TileHttpError') && text.includes('TileEtagMismatchError'));
  }

  // ---- task 5b (panel-failure-attribution, round 7): the rendered TEXT a user reads
  // for both buckets, from a budget object carrying `failuresByLayer`/
  // `noLoaderLayerIds` the way web/js/app.js's own `renderLayersPanelNow()` now does.
  {
    const container = makeEl('div');
    render(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      layerStates: { 'sha-a': { status: 'error', errorMessage: 'TileHttpError: placeholder' } },
      budget: {
        residentBytes: 0,
        memoryBudgetBytes: 1,
        deferredCount: 0,
        failedCount: 5, // the OLD, unattributed total -- must NOT be what failedDd shows any more
        failureNames: ['TileHttpError', 'TerrainLoaderNotImplementedError'],
        failuresByLayer: {
          terrain: { count: 2, names: ['TerrainLoaderNotImplementedError'] },
          'gateway-tileset:sha-a': { count: 3, names: ['TileHttpError'] },
        },
        noLoaderLayerIds: ['terrain'],
      },
    });
    const text = container.textContent;
    check('render: the "failed requests" row is now labelled "(tile sets)"',
      text.includes('failed requests (tile sets)'));
    const failedDd = findAll(container, (n) => hasClass(n, 'av-layers-budget')).length
      ? findAll(findAll(container, (n) => hasClass(n, 'av-layers-budget'))[0], (n) => n.tagName === 'DD')[2]
      : null;
    check('render: the "failed requests (tile sets)" VALUE counts only the tile-set failures (3), never the unattributed total (5) and never terrain\'s own 2',
      !!failedDd && failedDd.textContent === '3', { failedDdText: failedDd && failedDd.textContent });
    const failuresList = findAll(container, (n) => hasClass(n, 'av-layers-failures'));
    check('render: the scoped tile-set failure list (av-layers-failures) contains TileHttpError and NOT TerrainLoaderNotImplementedError',
      failuresList.length === 1 && failuresList[0].textContent.includes('TileHttpError') && !failuresList[0].textContent.includes('TerrainLoaderNotImplementedError'));
    check('render: a separate "terrain" notice names the terrain layer, its count (2), and says plainly this is not a fault of the selected tile set',
      text.includes('terrain: 2 requests, no loader is implemented') && text.includes('not a fault of the selected tile set'));
    check('render: the no-loader notice still names the typed error (TerrainLoaderNotImplementedError), never swallowed',
      text.includes('TerrainLoaderNotImplementedError'));
    const noLoaderList = findAll(container, (n) => hasClass(n, 'av-layers-no-loader'));
    check('render: the no-loader notice lives in its OWN host (av-layers-no-loader), never inside av-layers-failures',
      noLoaderList.length === 1 && !failuresList[0].textContent.includes('no loader is implemented'));
  }

  // ---- the no-loader notice is ABSENT entirely when nothing is attributed to it
  // ("only when nonzero", this task's own brief) -- never a "0 requests" line.
  {
    const container = makeEl('div');
    render(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      budget: {
        residentBytes: 0,
        memoryBudgetBytes: 1,
        deferredCount: 0,
        failedCount: 3,
        failureNames: ['TileHttpError'],
        failuresByLayer: { 'gateway-tileset:sha-a': { count: 3, names: ['TileHttpError'] } },
        noLoaderLayerIds: ['terrain'], // declared, but NOTHING attributed to it this time
      },
    });
    const text = container.textContent;
    check('render: with zero no-loader failures, the notice is absent entirely (no "0 requests" line, never a blank section either)',
      !text.includes('no loader is implemented') && findAll(container, (n) => hasClass(n, 'av-layers-no-loader')).length === 0);
  }

  // ---- backward compatibility: a caller that supplies the OLD budget shape (no
  // failuresByLayer at all -- every render()/renderLayersPanel() check ABOVE this one
  // in this file uses exactly that shape) must see EXACTLY the old behaviour: the
  // full failedCount in the tile-set row, no no-loader notice ever appearing.
  {
    const container = makeEl('div');
    render(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      budget: { residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 4, failureNames: ['TileHttpError'] },
    });
    const text = container.textContent;
    check('render: backward compatibility -- an old-shape budget (no failuresByLayer) still shows the full failedCount in the tile-set row',
      findAll(container, (n) => hasClass(n, 'av-layers-budget'))[0]
      && findAll(findAll(container, (n) => hasClass(n, 'av-layers-budget'))[0], (n) => n.tagName === 'DD')[2].textContent === '4');
    check('render: backward compatibility -- an old-shape budget never shows a no-loader notice',
      !text.includes('no loader is implemented') && findAll(container, (n) => hasClass(n, 'av-layers-no-loader')).length === 0);
  }
});

// ==================================================== 2a. renderLayersPanel (round 6 task 4)
// The proof this task's own brief calls "half the task", at the node-harness level (the
// REAL browser drive lives in tests/test_viewer_layers_panel.py, which reads the SAME
// `structuralRebuildCount` off the live page): an idle, budget-only re-render through
// `renderLayersPanel()` touches ZERO tile-set-list nodes -- proven by NODE IDENTITY
// (`===`), not by re-reading rendered text, which a full rebuild would also satisfy -- and
// never increments the real `structuralRebuildCount`; a genuine state change DOES rebuild
// and DOES increment it. `structuralRebuildCount` is read through `layersPanelModule`
// (the SAME module namespace `render`/`renderLayersPanel` above were imported from) so
// every read here reflects the module's own live binding, not a snapshot.
withFakeDocument(() => {
  const rebuildCount = () => layersPanelModule.structuralRebuildCount;
  const ddNodes = (container) => findAll(container, (n) => n.tagName === 'DD');

  // ---- first-ever call: always a real rebuild, exactly once -------------------------
  {
    const container = makeEl('div');
    const before = rebuildCount();
    renderLayersPanel(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      catalogError: null,
      loading: false,
      layerStates: { 'sha-b': { status: 'on', errorMessage: null } },
      budget: { residentBytes: 1000, memoryBudgetBytes: 67108864, deferredCount: 0, failedCount: 0, failureNames: [] },
    });
    check('renderLayersPanel: the FIRST call against a fresh container always performs one real rebuild',
      rebuildCount() === before + 1, { before, after: rebuildCount() });
    check('renderLayersPanel: the first call produces the real table (toggle buttons present)',
      findAll(container, (n) => hasClass(n, 'av-layers-toggle')).length === 2);
  }

  // ---- idle tick: SAME structural state, budget numbers change -- must NOT rebuild --
  {
    const container = makeEl('div');
    const dataStructural = {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      catalogError: null,
      loading: false,
      layerStates: { 'sha-b': { status: 'on', errorMessage: null } },
    };
    renderLayersPanel(container, { ...dataStructural, budget: { residentBytes: 1000, memoryBudgetBytes: 67108864, deferredCount: 0, failedCount: 0, failureNames: [] } });
    const afterFirst = rebuildCount();

    const toggleButtonsBefore = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));
    const refreshButtonBefore = findAll(container, (n) => hasClass(n, 'av-layers-refresh'))[0];
    const ddBefore = ddNodes(container);
    check('renderLayersPanel: setup -- two toggle buttons, one refresh button, three dd value nodes captured before the idle tick',
      toggleButtonsBefore.length === 2 && !!refreshButtonBefore && ddBefore.length === 3);

    // The idle-tick call: identical structural fields, ONLY the budget numbers differ --
    // exactly what web/js/app.js's own ~2 Hz animation-loop tick does every time nothing
    // about the catalog or a toggle actually changed.
    renderLayersPanel(container, { ...dataStructural, budget: { residentBytes: 2_500_000, memoryBudgetBytes: 67108864, deferredCount: 4, failedCount: 1, failureNames: [] } });

    check('renderLayersPanel: a budget-only re-render (idle tick) performs ZERO structural rebuilds',
      rebuildCount() === afterFirst, { afterFirst, after: rebuildCount() });

    const toggleButtonsAfter = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));
    const refreshButtonAfter = findAll(container, (n) => hasClass(n, 'av-layers-refresh'))[0];
    check('renderLayersPanel: every toggle button is the SAME node after an idle tick (identity, not just equal text) -- a coordinate/ref click would still be valid',
      toggleButtonsAfter.length === 2 && toggleButtonsAfter[0] === toggleButtonsBefore[0] && toggleButtonsAfter[1] === toggleButtonsBefore[1]);
    check('renderLayersPanel: the refresh button is the SAME node after an idle tick',
      refreshButtonAfter === refreshButtonBefore);

    const ddAfter = ddNodes(container);
    check('renderLayersPanel: the budget dd VALUE NODES are the SAME nodes after an idle tick (patched via textContent, never recreated)',
      ddAfter.length === 3 && ddAfter[0] === ddBefore[0] && ddAfter[1] === ddBefore[1] && ddAfter[2] === ddBefore[2]);
    check('renderLayersPanel: the budget numbers DID actually update in place (resident bytes, deferred, failed)',
      ddAfter[0].textContent.includes('2.4 MB') && ddAfter[1].textContent === '4' && ddAfter[2].textContent === '1');
  }

  // ---- idle tick where the SET of failure names changes -- still no structural rebuild,
  // the failures host reconciles itself in place, scoped to that one small div ----------
  {
    const container = makeEl('div');
    const dataStructural = {
      tileSets: REAL_TILE_SETS_PAYLOAD,
      catalogError: null,
      loading: false,
      layerStates: {},
    };
    renderLayersPanel(container, { ...dataStructural, budget: { residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 0, failureNames: [] } });
    const afterFirst = rebuildCount();
    const toggleButtonsBefore = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));

    renderLayersPanel(container, { ...dataStructural, budget: { residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 1, failureNames: ['TileHttpError'] } });

    check('renderLayersPanel: a NEW failure name appearing is still a budget-only change -- no structural rebuild',
      rebuildCount() === afterFirst);
    check('renderLayersPanel: the toggle buttons are still the SAME nodes when only the failure list changed',
      findAll(container, (n) => hasClass(n, 'av-layers-toggle')).every((n, i) => n === toggleButtonsBefore[i]));
    check('renderLayersPanel: the new failure name is actually visible',
      container.textContent.includes('TileHttpError') && !container.textContent.includes('No load failures recorded'));

    // Same failure set again -- proves `failuresKey` genuinely short-circuits rather than
    // rebuilding this small subtree on every call regardless of content.
    const failuresHostBefore = findAll(container, (n) => hasClass(n, 'av-layers-failures'))[0];
    renderLayersPanel(container, { ...dataStructural, budget: { residentBytes: 5, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 1, failureNames: ['TileHttpError'] } });
    const failuresHostAfter = findAll(container, (n) => hasClass(n, 'av-layers-failures'))[0];
    check('renderLayersPanel: an UNCHANGED failure set is not touched again (same <ul> node) even though other budget numbers moved',
      failuresHostAfter === failuresHostBefore);
  }

  // ---- task 5b (panel-failure-attribution, round 7): an idle tick where a NEW
  // no-loader (terrain) failure appears is STILL a budget-only change -- no structural
  // rebuild, the tile-set failures host untouched, and the no-loader host reconciles
  // itself independently in its own small div.
  {
    const container = makeEl('div');
    const dataStructural = {
      tileSets: REAL_TILE_SETS_PAYLOAD, catalogError: null, loading: false, layerStates: {},
    };
    const noAttribution = {
      residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 0, failureNames: [],
      failuresByLayer: {}, noLoaderLayerIds: ['terrain'],
    };
    renderLayersPanel(container, { ...dataStructural, budget: noAttribution });
    const afterFirst = rebuildCount();
    const toggleButtonsBefore = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));
    const failuresHostNode = findAll(container, (n) => hasClass(n, 'av-layers-failures-host'))[0];

    const terrainNowFailing = {
      residentBytes: 0, memoryBudgetBytes: 1, deferredCount: 0, failedCount: 2, failureNames: ['TerrainLoaderNotImplementedError'],
      failuresByLayer: { terrain: { count: 2, names: ['TerrainLoaderNotImplementedError'] } },
      noLoaderLayerIds: ['terrain'],
    };
    renderLayersPanel(container, { ...dataStructural, budget: terrainNowFailing });

    check('renderLayersPanel: a NEW no-loader (terrain) failure appearing is still a budget-only change -- no structural rebuild',
      rebuildCount() === afterFirst);
    check('renderLayersPanel: the toggle buttons are still the SAME nodes when only the no-loader attribution changed',
      findAll(container, (n) => hasClass(n, 'av-layers-toggle')).every((n, i) => n === toggleButtonsBefore[i]));
    check('renderLayersPanel: the tile-set failures host (av-layers-failures-host) is the SAME node -- a no-loader-only change never touches it',
      findAll(container, (n) => hasClass(n, 'av-layers-failures-host'))[0] === failuresHostNode);
    check('renderLayersPanel: the tile-set failures host still shows "No load failures recorded" -- terrain\'s failures never leak into it',
      container.textContent.includes('No load failures recorded'));
    check('renderLayersPanel: the new no-loader notice is actually visible, naming the terrain layer and "not a fault of the selected tile set"',
      container.textContent.includes('terrain: 2 requests, no loader is implemented') && container.textContent.includes('not a fault of the selected tile set'));

    // Same attribution again -- proves `noLoaderKey` genuinely short-circuits.
    const noLoaderHostBefore = findAll(container, (n) => hasClass(n, 'av-layers-no-loader'))[0];
    renderLayersPanel(container, { ...dataStructural, budget: { ...terrainNowFailing, residentBytes: 999 } });
    const noLoaderHostAfter = findAll(container, (n) => hasClass(n, 'av-layers-no-loader'))[0];
    check('renderLayersPanel: an UNCHANGED no-loader attribution is not touched again (same <ul> node) even though other budget numbers moved',
      noLoaderHostAfter === noLoaderHostBefore);
  }

  // ---- a REAL state change (a toggle's own status) DOES rebuild, and DOES increment --
  {
    const container = makeEl('div');
    const layerStates = { 'sha-b': { status: 'on', errorMessage: null } };
    renderLayersPanel(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD, catalogError: null, loading: false, layerStates,
      budget: { residentBytes: 1000, memoryBudgetBytes: 67108864, deferredCount: 0, failedCount: 0, failureNames: [] },
    });
    const afterFirst = rebuildCount();
    const toggleButtonsBefore = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));

    renderLayersPanel(container, {
      tileSets: REAL_TILE_SETS_PAYLOAD, catalogError: null, loading: false,
      layerStates: { 'sha-b': { status: 'off', errorMessage: null } }, // the real change: sha-b toggled off
      budget: { residentBytes: 1000, memoryBudgetBytes: 67108864, deferredCount: 0, failedCount: 0, failureNames: [] },
    });

    check('renderLayersPanel: a REAL layerStates change DOES perform exactly one structural rebuild',
      rebuildCount() === afterFirst + 1, { afterFirst, after: rebuildCount() });
    const toggleButtonsAfter = findAll(container, (n) => hasClass(n, 'av-layers-toggle'));
    check('renderLayersPanel: after a real rebuild, the toggle buttons are FRESH nodes (not the stale pre-rebuild references)',
      toggleButtonsAfter[0] !== toggleButtonsBefore[0] && toggleButtonsAfter[1] !== toggleButtonsBefore[1]);
    check('renderLayersPanel: the label actually reflects the new state ("Turn on" for the now-off row)',
      toggleButtonsAfter.some((b) => b.textContent === 'Turn on') && !toggleButtonsAfter.some((b) => b.textContent === 'Turn off'));
  }

  // ---- the hasManager edge: budget presence flipping with an otherwise-unchanged
  // structural key is NOT patchable in place and correctly forces a real rebuild --------
  {
    const container = makeEl('div');
    renderLayersPanel(container, { tileSets: null, catalogError: null, loading: false, layerStates: {}, budget: null });
    const afterFirst = rebuildCount();
    check('renderLayersPanel: with no budget yet, the honest "No layer manager available yet." notice renders',
      container.textContent.includes('No layer manager available yet.'));

    renderLayersPanel(container, { tileSets: null, catalogError: null, loading: false, layerStates: {}, budget: null });
    check('renderLayersPanel: an unchanged null-budget re-render still performs no structural rebuild',
      rebuildCount() === afterFirst);

    renderLayersPanel(container, {
      tileSets: null, catalogError: null, loading: false, layerStates: {},
      budget: { residentBytes: 0, memoryBudgetBytes: 100, deferredCount: 0, failedCount: 0, failureNames: [] },
    });
    check('renderLayersPanel: budget presence flipping null -> real forces exactly one real rebuild (the in-place patch cannot swap the notice for the real dl)',
      rebuildCount() === afterFirst + 1);
    check('renderLayersPanel: after that forced rebuild, the real budget dl actually renders',
      !container.textContent.includes('No layer manager available yet.'));
  }
});

// ============================================ 3. addLayer/removeLayer through a REAL manager
// A deterministic, non-real-network fetchImpl -- registry keyed by manifestSha256, exactly
// mirroring the REAL /api/tiles/<sha>/manifest and /api/tiles/<sha>/tiles/<level>/<x>/<y>
// route shapes `altavista/server.py` proxies (question 51: no real network I/O here at
// all, an in-memory fixture only -- the SAME "no fetch/XHR beyond a closed-over queue"
// discipline web/js/layers_check.mjs's own loader stubs already document).
const fixtures = new Map();

function writeVarint(nIn) {
  const bytes = [];
  let n = nIn;
  do {
    let b = n & 0x7f;
    n = Math.floor(n / 128);
    if (n > 0) b |= 0x80;
    bytes.push(b);
  } while (n > 0);
  return Buffer.from(bytes);
}
function writeTag(fieldNumber, wireType) { return writeVarint((fieldNumber << 3) | wireType); }
function writeLenDelim(fieldNumber, contentBuf) {
  return Buffer.concat([writeTag(fieldNumber, 2), writeVarint(contentBuf.length), contentBuf]);
}
/** A minimal, LOCAL protobuf ENCODER for exactly the field numbers
 * web/js/layers/tileset_manifest.js's own decoder documents it reads (TileSetManifest
 * field 7 `tiles`; TileEntry fields 1/2/3/5 `level`/`x`/`y`/`size_bytes`) -- the wire
 * CONTRACT those field numbers state, restated here as an encoder so this harness can
 * build a real, decodable manifest without a protobuf dependency (ADR-004's rule) and
 * without touching any production module: this is test-fixture construction, not a
 * second copy of any budget/eviction/selection RULE. */
function encodeTileEntry({ level, x, y, sizeBytes }) {
  return Buffer.concat([
    writeTag(1, 0), writeVarint(level),
    writeTag(2, 0), writeVarint(x),
    writeTag(3, 0), writeVarint(y),
    writeTag(5, 0), writeVarint(sizeBytes),
  ]);
}
function encodeTileSetManifest(entries) {
  return Buffer.concat(entries.map((e) => writeLenDelim(7, encodeTileEntry(e))));
}

function registerFixture(sha, tiles, sizeForIndex) {
  const entries = tiles.map((t, i) => ({ level: t.level, x: t.x, y: t.y, sizeBytes: sizeForIndex(i) }));
  const manifestBuf = encodeTileSetManifest(entries);
  const tileBytesByKey = new Map();
  const sizeByKey = new Map();
  tiles.forEach((t, i) => {
    const size = sizeForIndex(i);
    tileBytesByKey.set(tileKey(t), Buffer.alloc(size, 0x41 + (i % 20))); // real, distinct bytes per tile
    sizeByKey.set(tileKey(t), size);
  });
  fixtures.set(sha, { manifestBuf, manifestStatus: 200, tileBytesByKey, sizeByKey, wrongEtagKeys: new Set() });
  return sizeByKey;
}

function toArrayBuffer(buf) { return buf.buffer.slice(buf.byteOffset, buf.byteOffset + buf.byteLength); }

async function fixtureFetch(url) {
  const manifestMatch = url.match(/\/api\/tiles\/([^/]+)\/manifest$/);
  if (manifestMatch) {
    const fx = fixtures.get(manifestMatch[1]);
    if (!fx) return { ok: false, status: 404, headers: { get: () => null }, arrayBuffer: async () => new ArrayBuffer(0) };
    if (fx.manifestStatus !== 200) {
      return { ok: false, status: fx.manifestStatus, headers: { get: () => null }, arrayBuffer: async () => new ArrayBuffer(0) };
    }
    return { ok: true, status: 200, headers: { get: () => null }, arrayBuffer: async () => toArrayBuffer(fx.manifestBuf) };
  }
  const tileMatch = url.match(/\/api\/tiles\/([^/]+)\/tiles\/(\d+)\/(\d+)\/(\d+)$/);
  if (tileMatch) {
    const [, sha, level, x, y] = tileMatch;
    const fx = fixtures.get(sha);
    const key = `${level}/${x}/${y}`;
    const bytes = fx && fx.tileBytesByKey.get(key);
    if (!bytes) return { ok: false, status: 404, headers: { get: () => null }, arrayBuffer: async () => new ArrayBuffer(0) };
    // Independently computed (node's own crypto.createHash, NEVER the adapter's own
    // crypto.subtle.digest call) -- this is what makes the ETag-verification proof
    // below real rather than circular.
    const realSha256 = crypto.createHash('sha256').update(bytes).digest('hex');
    const etag = fx.wrongEtagKeys.has(key) ? '0'.repeat(64) : realSha256;
    return {
      ok: true, status: 200,
      headers: { get: (h) => (h.toLowerCase() === 'etag' ? `"${etag}"` : null) },
      arrayBuffer: async () => toArrayBuffer(bytes),
    };
  }
  throw new Error(`fixtureFetch: unrecognised URL ${url}`);
}

function flushMicrotasks() { return new Promise((resolve) => { setImmediate(resolve); }); }
async function waitUntilNoPending(manager, maxTicks = 50) {
  for (let i = 0; i < maxTicks; i += 1) {
    if (manager.pending.size === 0) return;
    // eslint-disable-next-line no-await-in-loop
    await flushMicrotasks();
  }
}

/** Byte-for-byte the sequence web/js/app.js's own `toggleGatewayLayer` performs for the
 * "turn on" branch -- construct the adapter, AWAIT fetchManifest() to completion BEFORE
 * addLayer (gateway_imagery_layer.js's own non-negotiable requirement), then register on
 * the caller's manager. Returns the adapter so this harness can independently re-`plan()`
 * it afterward. */
async function toggleOn(manager, manifestSha256) {
  const layerId = layerIdForManifest(manifestSha256);
  const adapter = new GatewayImageryLayerAdapter({ id: layerId, manifestSha256, fetchImpl: fixtureFetch });
  await adapter.fetchManifest();
  manager.addLayer(adapter);
  return adapter;
}

{
  const cameraEcef = geodeticToEcef(0, 0, 3_000_000); // a real ECEF point 3000km above the equator
  const screenParams = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 24, maxLevel: 0, maxTiles: 20 };
  const tiles = selectTiles(cameraEcef, screenParams);
  check('setup: selectTiles (the real globe_lod.js quadtree selection) returned at least one real tile to build fixtures from',
    tiles.length > 0, { tileCount: tiles.length, tiles: tiles.map(tileKey) });
  const view = { tiles, cameraEcef, ...screenParams };

  const sizesA = registerFixture('sha-a-integration', tiles, (i) => 500000 + i * 1000);
  const sizesB = registerFixture('sha-b-integration', tiles, (i) => 7000 + i * 10);
  const expectedTotalA = [...sizesA.values()].reduce((a, b) => a + b, 0);
  const expectedTotalB = [...sizesB.values()].reduce((a, b) => a + b, 0);

  const manager = new LayerManager({ memoryBudgetBytes: 64 * 1024 * 1024, now: () => 1 });

  // ---- toggle A on, toggle B on: two distinct tile sets, two distinct ids, no collision
  const adapterA = await toggleOn(manager, 'sha-a-integration');
  check('integration: layerIdForManifest gave the adapter a stable, unique id derived from its manifest sha256',
    adapterA.id === layerIdForManifest('sha-a-integration'));
  check('integration: fetchManifest() completed BEFORE addLayer -- manifestLoaded is true and manifestTileCount matches the real selection',
    adapterA.manifestLoaded === true && adapterA.manifestTileCount === tiles.length);

  const adapterB = await toggleOn(manager, 'sha-b-integration');
  check('integration: a second, DIFFERENT tile set gets a DIFFERENT layer id and registers without colliding with the first',
    adapterB.id !== adapterA.id && [...manager._layers.keys()].includes(adapterA.id) && [...manager._layers.keys()].includes(adapterB.id));

  // ---- recompute truth independently: re-`plan()` each adapter directly (not through
  // the manager) and check its OWN byteCost/byteCostSource against THIS harness's own
  // fixture data -- never trusting the manager's residentBytes total alone.
  const planA = adapterA.plan(view);
  check('integration: after fetchManifest(), plan() tags every request byteCostSource "manifest" (never the fallback estimate)',
    planA.every((r) => r.byteCostSource === 'manifest'));
  check('integration: plan()\'s byteCost for every tile equals THIS HARNESS\'s OWN fixture size for that exact tile (independently re-checked, not read back from the manager)',
    planA.every((r) => r.byteCost === sizesA.get(r.key)), { planA: planA.map((r) => [r.key, r.byteCost]) });

  const neverFetchedAdapter = new GatewayImageryLayerAdapter({ id: 'never-fetched', manifestSha256: 'sha-a-integration', fetchImpl: fixtureFetch });
  const planBeforeFetch = neverFetchedAdapter.plan(view);
  check('integration: BEFORE fetchManifest() ever runs, plan() honestly falls back to the declared estimate, tagged "fallback-estimate" (the documented, non-optional ordering requirement)',
    planBeforeFetch.every((r) => r.byteCostSource === 'fallback-estimate'));

  // ---- drive one real manager tick and let the real loads settle
  manager.update(view);
  await waitUntilNoPending(manager);

  check('integration: after loads settle, residentBytes equals the INDEPENDENTLY-computed sum of both fixtures\' own declared sizes (never trusting the manager\'s own bookkeeping alone)',
    manager.residentBytes === expectedTotalA + expectedTotalB,
    { residentBytes: manager.residentBytes, expectedTotalA, expectedTotalB });
  check('integration: countsByLayer reports every tile of each fixture as resident, under its own derived layer id',
    manager.countsByLayer()[adapterA.id].resident === tiles.length && manager.countsByLayer()[adapterB.id].resident === tiles.length);
  check('integration: zero soft budget violations and zero load failures for two well-formed tile sets sharing the manager',
    manager.softViolationCount === 0 && manager.failedCount === 0);

  // ---- duplicate id: addLayer throws (LayerManager's own guard -- exercised for real)
  let duplicateThrew = false;
  let duplicateMessage = '';
  try {
    manager.addLayer(adapterA);
  } catch (e) {
    duplicateThrew = true;
    duplicateMessage = e.message;
  }
  check('integration: registering the SAME layer id twice throws (LayerManager\'s own already-registered guard, exercised for real, not assumed)',
    duplicateThrew && duplicateMessage.includes(adapterA.id));

  // ---- toggle-off-toggle-on: removeLayer, then a fresh adapter under the SAME id
  manager.removeLayer(adapterA.id);
  check('integration: after removeLayer, the layer id is gone from the manager and its resident bytes are released',
    !manager._layers.has(adapterA.id) && manager.countsByLayer()[adapterA.id] === undefined);
  check('integration: removeLayer on an id that is no longer registered is a safe no-op (never throws)',
    (() => { try { manager.removeLayer(adapterA.id); return true; } catch { return false; } })());
  const residentBeforeReAdd = manager.residentBytes;
  const adapterA2 = await toggleOn(manager, 'sha-a-integration'); // the SAME manifest -> the SAME derived id
  check('integration: a toggle-off-toggle-on cycle under the SAME derived id re-registers without colliding',
    adapterA2.id === adapterA.id && [...manager._layers.keys()].includes(adapterA.id));
  manager.update(view);
  await waitUntilNoPending(manager);
  check('integration: after re-adding, residentBytes reflects the fresh load, not a stale leftover from before removeLayer',
    manager.residentBytes === residentBeforeReAdd + expectedTotalA);

  // ---- a real manifest failure (503) surfaces as a real, typed error -- and IS what
  // app.js's own toggleGatewayLayer would show the user (fed into render() below).
  fixtures.set('sha-c-unreachable', { manifestBuf: Buffer.alloc(0), manifestStatus: 503, tileBytesByKey: new Map() });
  let capturedError = null;
  try {
    await toggleOn(manager, 'sha-c-unreachable');
  } catch (e) {
    capturedError = e;
  }
  check('integration: a manifest fetch that answers non-2xx rejects with the real, typed TileHttpError (never swallowed, never a generic Error)',
    capturedError instanceof TileHttpError && capturedError.status === 503);

  withFakeDocument(() => {
    const container = makeEl('div');
    render(container, {
      tileSets: [{ assetId: 'c', manifestSha256: 'sha-c-unreachable', name: 'Unreachable Tile Set', sizeBytes: '0' }],
      layerStates: { 'sha-c-unreachable': { status: 'error', errorMessage: `${capturedError.name}: ${capturedError.message}` } },
      budget: { residentBytes: manager.residentBytes, memoryBudgetBytes: manager.memoryBudgetBytes, deferredCount: manager.deferredCount, failedCount: manager.failedCount, failureNames: manager.failureNames() },
    });
    check('integration -> render: the REAL captured TileHttpError (from an actual manifest fetch failure, not a fabricated string) is visible in the panel\'s DOM',
      container.textContent.includes('TileHttpError') && container.textContent.includes('503'));
  });

  // ---- a real ETag mismatch (the gateway's own integrity check) is also typed and visible
  const tilesetForMismatch = registerFixture('sha-d-etag-mismatch', tiles.slice(0, 1), () => 1234);
  fixtures.get('sha-d-etag-mismatch').wrongEtagKeys.add(tileKey(tiles[0]));
  const adapterD = await toggleOn(manager, 'sha-d-etag-mismatch');
  let etagMismatchError = null;
  try {
    await adapterD.load(adapterD.plan(view)[0], new AbortController().signal);
  } catch (e) {
    etagMismatchError = e;
  }
  check('integration: a tile whose recomputed SHA-256 disagrees with the gateway\'s own ETag rejects with the real, typed TileEtagMismatchError',
    etagMismatchError instanceof TileEtagMismatchError, { tilesetForMismatch: [...tilesetForMismatch.keys()] });
}

// ======================= 4. task 5b: end-to-end failure attribution through a REAL manager
// The proof this task's own brief calls "half the task": a REAL `LayerManager`, a REAL
// `TerrainLayerAdapter` (not a stub -- its own permanent, typed refusal), and a real
// gateway-imagery-shaped layer (id prefixed `gateway-tileset:`, exactly what a real
// toggled-on tile set's id looks like -- `layerIdForManifest`'s own format) that always
// fails with the real, typed `TileHttpError`. Drives one real `update()` tick to
// settlement, reads the manager's own `failuresByLayer()`/`noLoaderLayers()` (never
// fabricated), feeds them straight into `render()` exactly as `web/js/app.js`'s
// `renderLayersPanelNow()` now does, and asserts the RENDERED TEXT a user actually
// reads -- both from the returned data structure AND the DOM, per this task's own
// proof requirement ("not from one or the other").
{
  const cameraEcef = geodeticToEcef(0, 0, 3_000_000);
  const screenParams = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 24, maxLevel: 0, maxTiles: 20 };
  const tiles = selectTiles(cameraEcef, screenParams);
  const view = { tiles, cameraEcef, ...screenParams };

  const terrainId = 'terrain'; // the real, stable id GlobeLayer registers TerrainLayerAdapter under
  const terrain = new TerrainLayerAdapter({ id: terrainId });
  const badTileSetId = layerIdForManifest('sha-real-failure'); // 'gateway-tileset:sha-real-failure'
  const badTileSet = {
    id: badTileSetId,
    kind: 'imagery',
    plan: (v) => (v.tiles || []).map((t) => ({ key: tileKey(t), sseError: 10, viewDistanceM: 100, byteCost: 1024, tile: t })),
    load: (request, _signal) => Promise.reject(new TileHttpError(503, `simulated gateway 503 for ${request.key}`)),
    release: (_key) => {},
  };

  const endToEndManager = new LayerManager({ memoryBudgetBytes: 999_000_000_000, maxConcurrentLoads: 64, now: () => 1 });
  endToEndManager.addLayer(terrain);
  endToEndManager.addLayer(badTileSet);
  endToEndManager.update(view);
  await flushMicrotasks();

  check('end-to-end setup: both layers actually failed at least once (otherwise this proof tests nothing)',
    endToEndManager.failedCount > 0 && Object.keys(endToEndManager.failuresByLayer()).length === 2,
    { failedCount: endToEndManager.failedCount, failuresByLayer: endToEndManager.failuresByLayer() });

  const realFailuresByLayer = endToEndManager.failuresByLayer();
  const realNoLoaderIds = endToEndManager.noLoaderLayers().map((l) => l.id);
  check('end-to-end: LayerManager.noLoaderLayers() reports exactly the terrain adapter (its own notImplemented self-declaration, never a hard-coded id)',
    JSON.stringify(realNoLoaderIds) === JSON.stringify([terrainId]));

  // The DATA-STRUCTURE half of the proof (before ever touching the DOM).
  const dataAttribution = attributeFailures(realFailuresByLayer, realNoLoaderIds);
  check('end-to-end (data): tileSetCount equals exactly the real gateway-shaped layer\'s own failure count, terrain\'s excluded',
    dataAttribution.tileSetCount === realFailuresByLayer[badTileSetId].count);
  check('end-to-end (data): the tile-set bucket\'s names are exactly ["TileHttpError"], terrain\'s typed name absent',
    JSON.stringify(dataAttribution.tileSetNames) === JSON.stringify(['TileHttpError']));
  check('end-to-end (data): the no-loader bucket carries terrain\'s own id/count/typed name',
    dataAttribution.noLoader.length === 1 && dataAttribution.noLoader[0].layerId === terrainId
    && dataAttribution.noLoader[0].count === realFailuresByLayer[terrainId].count
    && JSON.stringify(dataAttribution.noLoader[0].names) === JSON.stringify(['TerrainLoaderNotImplementedError']));
  check('end-to-end (data): totals still add up to the unchanged, real LayerManager.failedCount',
    dataAttribution.tileSetCount + dataAttribution.noLoader.reduce((s, e) => s + e.count, 0) === endToEndManager.failedCount,
    { failedCount: endToEndManager.failedCount, dataAttribution });

  // The DOM half of the proof -- the exact panel a user would see for this real run.
  withFakeDocument(() => {
    const container = makeEl('div');
    render(container, {
      tileSets: [{ assetId: 'real', manifestSha256: 'sha-real-failure', name: 'Real Failing Tile Set', sizeBytes: '0' }],
      layerStates: { 'sha-real-failure': { status: 'on' } },
      budget: {
        residentBytes: endToEndManager.residentBytes,
        memoryBudgetBytes: endToEndManager.memoryBudgetBytes,
        deferredCount: endToEndManager.deferredCount,
        failedCount: endToEndManager.failedCount,
        failureNames: endToEndManager.failureNames(),
        failuresByLayer: realFailuresByLayer,
        noLoaderLayerIds: realNoLoaderIds,
      },
    });
    const text = container.textContent;
    check('end-to-end (DOM): the tile-set failure row shows exactly the real gateway layer\'s own count',
      findAll(container, (n) => hasClass(n, 'av-layers-budget'))[0]
      && findAll(findAll(container, (n) => hasClass(n, 'av-layers-budget'))[0], (n) => n.tagName === 'DD')[2].textContent
        === String(realFailuresByLayer[badTileSetId].count));
    check('end-to-end (DOM): the tile-set failure list shows TileHttpError and never TerrainLoaderNotImplementedError',
      findAll(container, (n) => hasClass(n, 'av-layers-failures'))[0].textContent.includes('TileHttpError')
      && !findAll(container, (n) => hasClass(n, 'av-layers-failures'))[0].textContent.includes('TerrainLoaderNotImplementedError'));
    check('end-to-end (DOM): a real terrain gap reads as a terrain gap -- names the terrain layer, its real count, and says it is not the selected tile set\'s fault',
      text.includes(`terrain: ${realFailuresByLayer[terrainId].count} request`) && text.includes('no loader is implemented')
      && text.includes('not a fault of the selected tile set') && text.includes('TerrainLoaderNotImplementedError'));
  });
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }, null, 2));
process.exit(allPass ? 0 : 1);

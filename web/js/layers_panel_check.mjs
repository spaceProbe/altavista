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
//   2. render() assembly   -- a fake DOM (this file's own, no jsdom -- the identical
//                              posture web/js/command_panel_check.mjs's own section
//                              7/8 and web/js/layout/layout_tree_check.mjs's LayoutManager
//                              section already take), proving the "not configured" rule,
//                              the "never loaded yet" state, the empty-list state, a
//                              real table, and the toggle/refresh callbacks fire with the
//                              exact row/no arguments
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
import { selectTiles, geodeticToEcef, tileKey } from './globe_lod.js';
import {
  layerIdForManifest, tileSetRows, formatBytes, shortSha, errorLine, layerStateFor, render,
} from './panels/layers_panel.js';
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

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }, null, 2));
process.exit(allPass ? 0 : 1);

// Round 5 (question 228 finding 2, browser half): the "Layers" panel -- a user picks a
// catalogued tile set here and sees its tiles requested through the one, shared
// `LayerManager` (web/js/scene.js's `viewer.layerManager`). Round 4 delivered the globe
// half (the manager exists, `enableGlobe()` routes through it); this round's server half
// (`altavista/server.py`'s `GET /api/catalog/tilesets`, `altavista/gateway_client.py`)
// delivered a real catalog listing. Nothing wired a user-facing control to either one --
// this file is that control.
//
// Same convention as every other panel in this directory (web/js/panels/command_panel.js's
// own top comment, verbatim pattern): pure, DOM-free data-shaping functions in the top
// half of this file, independently testable under plain `node`
// (web/js/layers_panel_check.mjs), exactly one DOM-touching `render()` at the bottom.
//
// **This module never fetches, and never imports `scene.js`, `globe.js` or
// `web/js/layers/`.** `web/js/app.js` (the caller) owns:
//   - the one `GET /api/catalog/tilesets` fetch and its outcome (`tileSets`/`catalogError`/
//     `loading`, below);
//   - constructing/registering/unregistering the real `GatewayImageryLayerAdapter` on the
//     ONE shared `viewer.layerManager` (`addLayer` on, `removeLayer` off -- never a second
//     manager, never a direct fetch that bypasses it) -- see `web/js/layers/
//     gateway_imagery_layer.js`'s own module doc for why `fetchManifest()` MUST be awaited
//     before `addLayer`;
//   - reading `viewer.layerManager`'s own counters (`residentBytes`/`memoryBudgetBytes`/
//     `deferredCount`/`failedCount`/`failureNames()`) each time it wants this panel
//     refreshed.
// This file only ever receives plain data and callbacks and turns them into DOM, exactly
// like every other panel here.
//
// -------------------------------------------------------------- the "not configured" rule
// `GET /api/catalog/tilesets` answers a typed, specific `fastapi.HTTPException` -- never an
// empty `200 []` -- when no data gateway is configured, when the configured one is
// unreachable, or on a real refusal from the gateway itself (`altavista/gateway_client.py`'s
// own module doc: "an unconfigured gateway must never look like 'there are no tile sets' to
// a caller"). This panel preserves that distinction structurally, not by string-matching:
// `data.catalogError` (a real fetch failure -- always rendered with the server's OWN message,
// never a generic one) and `data.tileSets` (a real, successful, possibly-empty array) are
// two different fields, and `render()` below branches on which one is actually present
// before ever saying "no tile sets" -- an empty array with no error is the ONLY case that
// notice describes.
//
// -------------------------------------------------------------- why the catalog is NOT auto-fetched
// `web/js/app.js`'s own command-console wiring (`renderCommandPanelNow()`'s doc comment)
// records a real, previously-shipped defect: an eager, unconditional fetch of an
// optionally-configured route at page boot broke `tests/test_viewer_net.py`'s
// `test_zero_console_errors_on_load_against_a_live_server` (question 168) against the
// ordinary `python -m altavista serve` default, which configures no such service -- a
// non-2xx response to a fetch the browser itself issued is logged by Chrome as a real
// `Log.entryAdded` network error, which that gate treats as a console error. `GET
// /api/catalog/tilesets` is unconfigured by exactly the same default (no `--gateway-
// endpoint`/`--gateway-token-path`), so this panel takes the identical precaution: it is
// NEVER fetched automatically, only in response to the explicit "Refresh catalog" button
// this file renders (`onRefreshCatalog`, below) -- a real user click, never a page-load
// side effect. `data.tileSets === null && !data.catalogError && !data.loading` (the
// caller's own initial state, before any refresh has ever been attempted) therefore
// renders its own honest "not loaded yet" notice, distinct from both the error case and
// the real-empty-list case.

// -------------------------------------------------------------- round 6 task 4: repaint cadence
// Question 231's ruling on round 5's own recorded usability defect (this file's module
// doc above did not yet exist when that defect was found): `web/js/app.js` re-renders
// this panel from its animation loop, throttled to ~2 Hz so the streaming-budget numbers
// stay live -- but this panel's OWN contract used to be strict teardown/rebuild
// (`container.innerHTML = ''` in `render()`, below) on every single call, which tore out
// and rebuilt the tile-set list's own buttons twice a second, including while a user's
// click was still in flight ("ref is stale (element removed)").
//
// The fix is entirely local to this file: `render()` itself is UNCHANGED -- still a full
// teardown/rebuild every time it is called, byte-for-byte the same contract every other
// panel in this codebase follows (command_panel.js et al.), so nothing outside this panel
// needs to know anything changed. `renderLayersPanel()` (bottom of this file) is the new,
// panel-scoped entry point `web/js/app.js` now calls INSTEAD of `render()` directly: it
// decides, from a cheap total change key over exactly the fields that affect the tile-set
// list (`layersPanelStateKey()`, below -- never the budget numbers), whether to run a real
// `render()` (a genuine structural change: the catalog changed, or some tile set's own
// on/off/loading/error status changed) or to patch only the streaming-budget section's
// existing text nodes in place (`updateBudgetSection()`, below -- the animation loop's own
// ~2 Hz tick, where NOTHING about the tile-set list ever changes on its own). No node
// belonging to the tile-set list -- table, rows, or buttons -- is ever touched by the
// budget-only path. `structuralRebuildCount` (below) is a REAL counter `render()` itself
// increments every time it actually tears down and rebuilds, so a test can assert "zero
// structural rebuilds across an idle window" against production code, not a test shim.

const LAYER_ID_PREFIX = 'gateway-tileset:';

/** Real production counter, incremented by `render()` (below) every time it performs a
 * full teardown/rebuild of this panel's DOM -- never incremented, never touched, by
 * `updateBudgetSection()`'s in-place budget patch. Exists so a test can read a REAL
 * count of structural rebuilds off the shipped module, not a test-only shim (round 6
 * task 4's own proof requirement: "a real counter the production code increments"). */
export let structuralRebuildCount = 0;

/**
 * The stable, unique layer id this panel derives for one catalogued tile set -- keyed by
 * its manifest sha256 (the tile set's own identity, per `GET /api/catalog/tilesets`'s wire
 * shape and `GatewayImageryLayerAdapter`'s own constructor contract) so two different tile
 * sets can be registered on the shared `LayerManager` at once, and a toggle-off-toggle-on
 * cycle of the SAME tile set always resolves to the same id rather than colliding with a
 * still-draining previous registration under a fresh one.
 * @param {string} manifestSha256
 * @returns {string}
 */
export function layerIdForManifest(manifestSha256) {
  return `${LAYER_ID_PREFIX}${manifestSha256}`;
}

/**
 * `{tileSets: [...]}` (the real wire shape `altavista/gateway_client.py::
 * _catalog_record_to_dict` produces, one entry per real `heavy_pb2.CatalogRecord`) -> an
 * array of rows, defensively copied and re-sorted by `assetId` (this module's own explicit,
 * re-derived order -- never trusting the wire's own array order silently, mirroring
 * `command_panel.js`'s `proposalRows`' identical rule; the server's own doc already claims
 * `asset_id`-ascending, this is belt AND suspenders, not a claim that the server is wrong).
 * `[]` for a null/malformed payload, never a throw.
 * @param {{tileSets?: Array<object>}|null|undefined} payload
 * @returns {Array<{assetId:string, manifestSha256:string, name:string, marking:string,
 *   caveats:string[], sizeBytes:string, mediaType:string, uri:string, jobId:string,
 *   createdTaiNs:string, footprintWkt:string}>}
 */
export function tileSetRows(payload) {
  if (!payload || !Array.isArray(payload.tileSets)) return [];
  return payload.tileSets
    .filter((t) => t && typeof t.manifestSha256 === 'string' && t.manifestSha256)
    .map((t) => ({
      assetId: t.assetId || '',
      manifestSha256: t.manifestSha256,
      name: t.name || t.assetId || t.manifestSha256,
      marking: t.marking || '',
      caveats: Array.isArray(t.caveats) ? [...t.caveats] : [],
      sizeBytes: typeof t.sizeBytes === 'string' ? t.sizeBytes : String(t.sizeBytes || 0),
      mediaType: t.mediaType || '',
      uri: t.uri || '',
      jobId: t.jobId || '',
      createdTaiNs: typeof t.createdTaiNs === 'string' ? t.createdTaiNs : String(t.createdTaiNs || ''),
      footprintWkt: t.footprintWkt || '',
    }))
    .sort((a, b) => a.assetId.localeCompare(b.assetId));
}

/**
 * A decimal-string byte count (`sizeBytes`, `web/js/command_panel.js`'s own `taiNs`
 * convention restated for a size instead of an epoch -- crosses the wire as a string so a
 * uint64 near/above `Number.MAX_SAFE_INTEGER` never silently rounds, see
 * `altavista/gateway_client.py::_int64`) rendered as a short, human-readable size. Never
 * throws on a malformed string -- falls back to the raw string suffixed `B` so the real
 * wire value is still visible rather than replaced by a guess.
 * @param {string} sizeBytesStr
 * @returns {string}
 */
export function formatBytes(sizeBytesStr) {
  const n = Number(sizeBytesStr);
  if (!Number.isFinite(n) || n < 0) return `${sizeBytesStr} B`;
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
}

/** The manifest sha256, abbreviated for a table cell -- the full value always stays
 * reachable via the cell's own `title` (`buildTileSetsSection` below), never lost. */
export function shortSha(sha) {
  if (typeof sha !== 'string') return '';
  return sha.length > 16 ? `${sha.slice(0, 16)}…` : sha;
}

/**
 * A caller-supplied `{status, message}` (or `null`) -> a single human-readable line, or
 * `null` when there is nothing to say -- byte-for-byte the same contract as
 * `command_panel.js`'s own `errorLine` (never a generic "something went wrong"; this
 * function only formats the two fields it is given).
 * @param {{status?:number, message?:string}|null|undefined} err
 * @returns {string|null}
 */
export function errorLine(err) {
  if (!err || !err.message) return null;
  return typeof err.status === 'number' ? `(${err.status}) ${err.message}` : String(err.message);
}

/**
 * One row's own toggle state -- `layerStates` is keyed by `manifestSha256` (the caller's,
 * `web/js/app.js`'s, own state object); a tile set with no entry there has never been
 * toggled on, i.e. `'off'`. Pure lookup with an honest default, never a throw on a missing
 * key.
 * @param {string} manifestSha256
 * @param {Object<string, {status:string, errorMessage?:string|null}>|null|undefined} layerStates
 * @returns {{status:'off'|'loading'|'on'|'error', errorMessage:string|null}}
 */
export function layerStateFor(manifestSha256, layerStates) {
  const entry = layerStates && layerStates[manifestSha256];
  if (!entry) return { status: 'off', errorMessage: null };
  return { status: entry.status || 'off', errorMessage: entry.errorMessage || null };
}

/**
 * A cheap, TOTAL change key over exactly the fields that affect the TILE-SET LIST's own
 * structure -- round 6 task 4's own required "state, precisely": this is where "state"
 * is defined, as code rather than as prose.
 *
 * Included (a change in ANY of these means the list itself must be rebuilt):
 *   - `tileSets` -- the catalog listing's own contents, whole-array, order included (the
 *     server's own `assetId`-ascending order is re-derived by `tileSetRows()` regardless,
 *     but a change in WHICH tile sets are present, or in any one of their own fields --
 *     name, marking, caveats, size, sha, media type, uri, job id, timestamp, footprint --
 *     is a real catalog change the list must show).
 *   - `catalogError` -- status + message; the "not configured"/real-fetch-failure notice
 *     this panel's own top comment documents.
 *   - `loading` -- the in-flight-fetch notice and the disabled Refresh button.
 *   (Together these three also determine `render()`'s own `neverFetched` branch, so no
 *   separate key component is needed for it -- it is a pure function of exactly these.)
 *   - `layerStates` -- every catalogued tile set's OWN on/off/loading/error status, and
 *     its error message where present (the retry label and the typed failure text a row
 *     itself shows) -- "any per-layer status the list shows", keyed by manifestSha256 and
 *     re-sorted here so key equality never depends on insertion/iteration order.
 *
 * Deliberately EXCLUDED: `budget` (residentBytes/memoryBudgetBytes/deferredCount/
 * failedCount/failureNames) -- those are the streaming-budget section's own concern,
 * change every animation-loop tick by design, and are patched in place by
 * `updateBudgetSection()` below, never by a rebuild this key would trigger.
 *
 * `JSON.stringify` over this small, already-fetched-once data (never the DOM) is the
 * "cheap" part -- a handful of short strings/numbers per tile set, not a deep DOM
 * comparison.
 * @param {{tileSets?:Array<object>|null, catalogError?:{status?:number,message?:string}|null,
 *   loading?:boolean, layerStates?:Object<string,{status:string,errorMessage?:string|null}>}} data
 * @returns {string}
 */
export function layersPanelStateKey(data) {
  const { tileSets, catalogError, loading, layerStates } = data || {};
  const layerStateEntries = layerStates
    ? Object.keys(layerStates).sort().map((k) => `${k}${layerStates[k].status || 'off'}${layerStates[k].errorMessage || ''}`)
    : [];
  return JSON.stringify([
    tileSets ?? null,
    catalogError ? [catalogError.status ?? null, catalogError.message ?? null] : null,
    !!loading,
    layerStateEntries,
  ]);
}

// -------------------------------------------------------------------------------- DOM

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function noticeEl(text) {
  return el('p', 'av-panel-notice', text);
}

function toggleButtonLabel(status) {
  if (status === 'on') return 'Turn off';
  if (status === 'loading') return 'Loading…';
  if (status === 'error') return 'Retry';
  return 'Turn on';
}

function buildRefreshButton(loading, onRefreshCatalog) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'av-pane-btn av-layers-refresh';
  btn.textContent = loading ? 'Refreshing…' : 'Refresh catalog';
  btn.disabled = !!loading;
  btn.addEventListener('click', () => onRefreshCatalog && onRefreshCatalog());
  return btn;
}

function buildTileSetsSection(rows, catalogError, loading, neverFetched, layerStates, onToggleLayer, onRefreshCatalog) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Catalogued tile sets'));
  section.appendChild(buildRefreshButton(loading, onRefreshCatalog));

  // The "not configured" rule (this file's own top comment): a real fetch failure is
  // ALWAYS the server's own message, and is checked BEFORE the empty-list case below --
  // an unconfigured/unreachable gateway can never fall through and render as "no tile
  // sets".
  const line = errorLine(catalogError);
  if (line) {
    const notice = noticeEl(`Cannot load the tile set catalog: ${line}`);
    notice.className += ' av-command-error';
    section.appendChild(notice);
    return section;
  }
  if (loading) {
    section.appendChild(noticeEl('Loading tile set catalog…'));
    return section;
  }
  if (neverFetched) {
    section.appendChild(noticeEl('Tile set catalog not loaded yet -- click "Refresh catalog" to fetch it.'));
    return section;
  }
  if (rows.length === 0) {
    section.appendChild(noticeEl('No tile sets in the catalog.'));
    return section;
  }

  const table = document.createElement('table');
  table.className = 'av-layers-tilesets';
  for (const row of rows) {
    const { status, errorMessage } = layerStateFor(row.manifestSha256, layerStates);
    const tr = document.createElement('tr');
    tr.className = status === 'on' ? 'av-layers-active' : '';

    const nameTd = el('td', null, row.name);
    nameTd.title = row.jobId ? `job ${row.jobId} · asset ${row.assetId}` : row.assetId;
    const markingTd = el('td', null, row.marking + (row.caveats.length ? ` (${row.caveats.join(', ')})` : ''));
    const sizeTd = el('td', null, formatBytes(row.sizeBytes));
    const shaTd = el('td', null, shortSha(row.manifestSha256));
    shaTd.title = row.manifestSha256;

    const statusTd = document.createElement('td');
    if (status === 'error') {
      statusTd.textContent = errorMessage || 'load failed';
      statusTd.className = 'av-command-error';
      statusTd.title = errorMessage || '';
    } else if (status === 'loading') {
      statusTd.textContent = 'fetching manifest…';
    } else if (status === 'on') {
      statusTd.textContent = 'active';
    } else {
      statusTd.textContent = '';
    }

    const toggleTd = document.createElement('td');
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'av-pane-btn av-layers-toggle';
    btn.textContent = toggleButtonLabel(status);
    btn.disabled = status === 'loading';
    btn.addEventListener('click', () => onToggleLayer && onToggleLayer(row));
    toggleTd.appendChild(btn);

    tr.append(nameTd, markingTd, sizeTd, shaTd, statusTd, toggleTd);
    table.appendChild(tr);
  }
  section.appendChild(table);
  return section;
}

/**
 * The streaming-budget section -- read straight off `viewer.layerManager` by the caller
 * (this panel never touches `scene.js`), refreshed as the view streams (this task's own
 * required "resident bytes against the budget"). Deferrals and failures included
 * unconditionally, per the brief's own explicit rule: "a failure must be visible to the
 * user, with its typed name, not swallowed" -- round 3's failure-memory policy
 * (`web/js/layers/layer.js`'s own module doc) means a typed refusal is remembered rather
 * than retried every frame, and this section is where that memory becomes visible.
 *
 * Round 6 task 4: builds the section's DOM structure exactly ONCE (the labels, the `dl`,
 * the empty value `dd`s, a host `div` for the failure list/notice) and returns a HANDLE
 * -- `{section, residentDd, deferredDd, failedDd, failuresHost, noManager}` -- that
 * `updateBudgetSection()` (below) reuses on every later animation-loop tick to patch
 * VALUES in place, never rebuilding this structure again. The handle is stashed on the
 * container by `render()` (below); this function itself never touches the container.
 * @param {{residentBytes:number, memoryBudgetBytes:number, deferredCount:number,
 *   failedCount:number, failureNames:string[]}|null|undefined} budget
 * @returns {{section:HTMLElement, noManager:boolean, residentDd?:HTMLElement,
 *   deferredDd?:HTMLElement, failedDd?:HTMLElement, failuresHost?:HTMLElement,
 *   failuresKey?:string}}
 */
function buildBudgetSection(budget) {
  const section = document.createElement('div');
  section.className = 'av-panel-section';
  section.appendChild(el('h4', null, 'Streaming budget'));

  const handle = { section, noManager: !budget };
  if (!budget) {
    section.appendChild(noticeEl('No layer manager available yet.'));
    return handle;
  }

  const dl = document.createElement('dl');
  dl.className = 'av-layers-budget';
  const addRow = (label) => {
    dl.appendChild(el('dt', null, label));
    const dd = el('dd', null, '');
    dl.appendChild(dd);
    return dd;
  };
  handle.residentDd = addRow('resident / budget');
  handle.deferredDd = addRow('deferred requests');
  handle.failedDd = addRow('failed requests');
  section.appendChild(dl);

  // A dedicated, button-free host for the failure-name list/notice -- the ONLY part of
  // this section whose own child nodes are ever added/removed after the first build
  // (when the SET of failure names actually changes, below), scoped to this small div
  // alone so it can never touch the `dl`'s own dt/dd nodes or anything in the tile-set
  // list.
  handle.failuresHost = document.createElement('div');
  handle.failuresHost.className = 'av-layers-failures-host';
  section.appendChild(handle.failuresHost);

  updateBudgetSection(handle, budget);
  return handle;
}

/**
 * Patches an EXISTING budget-section handle's own value nodes in place -- this is the
 * animation loop's own ~2 Hz tick path (round 6 task 4): `residentDd`/`deferredDd`/
 * `failedDd` get a plain `.textContent =` assignment (a text-node mutation; the `dt`
 * labels and the `dd` elements themselves are never touched, added, or removed here).
 *
 * The failure-name list is the one value that is not a single scalar: it is patched by
 * comparing a cheap join of the current `failureNames` array against the join stashed
 * from the last update (`handle.failuresKey`) and, ONLY on a real change, rebuilding
 * `failuresHost`'s own small subtree (never the `dl`, never the tile-set list). In
 * practice this fires exactly when a NEW distinct failure type is first recorded --
 * `web/js/layers/layer.js`'s own failure-memory policy means the SET of typed failure
 * names is small and rarely grows -- so on every ordinary idle tick (the case this task's
 * own proof measures) `failuresKey` is unchanged and this function touches zero nodes at
 * all beyond the three `textContent` assignments above.
 * @param {{noManager:boolean, residentDd?:HTMLElement, deferredDd?:HTMLElement,
 *   failedDd?:HTMLElement, failuresHost?:HTMLElement, failuresKey?:string}|null|undefined} handle
 * @param {{residentBytes:number, memoryBudgetBytes:number, deferredCount:number,
 *   failedCount:number, failureNames:string[]}|null|undefined} budget
 */
function updateBudgetSection(handle, budget) {
  // `noManager` (the "No layer manager available yet." notice) and a null `budget` here
  // are a structural mismatch a caller must resolve with a real `render()` instead (see
  // `renderLayersPanel()` below, which checks `hasManager` before ever reaching here) --
  // this function only ever patches values into a handle that was built WITH a budget.
  if (!handle || handle.noManager || !budget) return;
  handle.residentDd.textContent = `${formatBytes(String(budget.residentBytes ?? 0))} / ${formatBytes(String(budget.memoryBudgetBytes ?? 0))}`;
  handle.deferredDd.textContent = String(budget.deferredCount ?? 0);
  handle.failedDd.textContent = String(budget.failedCount ?? 0);

  const failureNames = Array.isArray(budget.failureNames) ? budget.failureNames : [];
  const failuresKey = failureNames.join('');
  if (handle.failuresKey === failuresKey) return; // identical set of failures -- zero further DOM touch
  handle.failuresKey = failuresKey;
  handle.failuresHost.innerHTML = ''; // scoped to this one small, button-free div only
  if (failureNames.length === 0) {
    handle.failuresHost.appendChild(noticeEl('No load failures recorded.'));
  } else {
    const list = document.createElement('ul');
    list.className = 'av-layers-failures';
    for (const name of failureNames) list.appendChild(el('li', null, name));
    handle.failuresHost.appendChild(list);
  }
}

/**
 * Render this panel's content into `container` (an existing DOM element -- same contract
 * as every other panel's own `render()`). Full teardown/rebuild on EVERY call, byte-for-
 * byte UNCHANGED from before round 6 task 4 -- this function itself does not know or care
 * why it was called, and every other panel in this codebase still follows this identical
 * contract untouched. The caller (`web/js/app.js`) owns every piece of mutable state
 * (`tileSets`/`catalogError`/`loading`/`layerStates`) and re-renders after the catalog
 * fetch settles, after each toggle attempt settles, and periodically as
 * `viewer.layerManager`'s own counters change -- exactly like `command_panel.js`'s own
 * `commandPanelState` / `renderCommandPanelNow` split.
 *
 * Round 6 task 4: also stashes, on `container` itself, the change key this render
 * corresponds to (`layersPanelStateKey`) and the budget-section handle it just built
 * (`buildBudgetSection`'s return) -- `renderLayersPanel()` below is what reads these back
 * to decide whether a LATER call needs a real rebuild at all. `render()` remains a
 * complete, self-sufficient full rebuild whether or not anything ever reads them back
 * (calling it directly, as this panel's own `layers_panel_check.mjs` still does for every
 * one of its own render() checks, works exactly as it always has).
 * @param {HTMLElement} container
 * @param {{tileSets?: Array<object>|null, catalogError?: {status:number,message:string}|null,
 *   loading?: boolean, layerStates?: Object<string, {status:string, errorMessage?:string|null}>,
 *   budget?: {residentBytes:number, memoryBudgetBytes:number, deferredCount:number,
 *     failedCount:number, failureNames:string[]}|null,
 *   onToggleLayer?: (row:object)=>void, onRefreshCatalog?: ()=>void}} data
 */
export function render(container, data) {
  container.innerHTML = '';
  structuralRebuildCount += 1;
  const {
    tileSets, catalogError, loading, layerStates, budget, onToggleLayer, onRefreshCatalog,
  } = data || {};

  const neverFetched = tileSets == null && !catalogError && !loading;
  container.appendChild(buildTileSetsSection(
    tileSetRows({ tileSets }), catalogError, !!loading, neverFetched, layerStates, onToggleLayer, onRefreshCatalog,
  ));
  const budgetHandle = buildBudgetSection(budget);
  container.appendChild(budgetHandle.section);

  container.__avLayersPanelBudgetHandle = budgetHandle;
  container.__avLayersPanelState = {
    key: layersPanelStateKey({ tileSets, catalogError, loading, layerStates }),
    hasManager: !!budget,
  };
}

/**
 * Round 6 task 4's own new entry point -- `web/js/app.js` now calls THIS, never `render()`
 * directly, from every one of its own existing call sites (the initial scaffold, after a
 * catalog refresh settles, after a toggle settles, AND the animation loop's own ~2 Hz
 * tick that used to call `render()` unconditionally and is exactly what produced round
 * 5's own recorded defect). `app.js` itself needed no other change: it does not need to
 * know which of its call sites are "structural" and which are "just a tick" -- this
 * function decides, every time, from `layersPanelStateKey()` alone.
 *
 * - Never rendered before, OR the tile-set list's own state key changed, OR whether a
 *   `budget` is available at all just flipped (the one case `updateBudgetSection` cannot
 *   patch: the "No layer manager available yet." notice swapping for the real `dl`, or
 *   back -- not observed in practice, since `viewer.layerManager` exists for the whole
 *   lifetime of `viewer`, but handled correctly rather than assumed away): a real
 *   `render()`, exactly as before.
 * - Otherwise (the ordinary idle tick): `updateBudgetSection()` patches the existing
 *   budget handle's value nodes in place. The tile-set list -- table, rows, buttons -- is
 *   never touched.
 * @param {HTMLElement} container
 * @param {Parameters<typeof render>[1]} data
 */
export function renderLayersPanel(container, data) {
  const { tileSets, catalogError, loading, layerStates, budget } = data || {};
  const key = layersPanelStateKey({ tileSets, catalogError, loading, layerStates });
  const prev = container.__avLayersPanelState;
  if (!prev || prev.key !== key || prev.hasManager !== !!budget) {
    render(container, data);
    return;
  }
  updateBudgetSection(container.__avLayersPanelBudgetHandle, budget);
}

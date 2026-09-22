// Tiles3DLayerAdapter -- wraps the vendored 3DTilesRendererJS overlay behind the
// Layer interface (./layer.js). It does not reimplement 3D Tiles tile-tree
// screen-space-error selection: it imports `selectTiles3D`/`regionBoundingSphereEcef`
// from `web/js/tiles_layer.js`, the one place that arithmetic lives (itself already
// built on `web/js/globe_lod.js`'s `dist`/`sseFromGeometricError`, per that file's own
// module docstring) -- question 218's "no second copy of a budget/eviction rule"
// reasoning applied one level up, to selection, not just eviction.
//
// Round 5 (docs/open-questions.md question 228's decision, round 4's decision 9
// deferral, ratified in question 229): routes the overlay's actual per-tile CONTENT
// FETCH through the one per-viewer `LayerManager`, not a second, independent
// scheduler. This file's own module docstring below ("Fetch gating") is the mechanism;
// `web/js/tiles_layer.js`'s module docstring covers how `TilesOverlayLayer` wires it
// into the live vendored `TilesRenderer`, and `web/VIEWER.md` has the disclosed
// boundary of what this does and does not cover.
//
// ------------------------------------------------------------------ Fetch gating
// The vendored `TilesRenderer` (web/vendor/3d-tiles-renderer/build/renderer-tyqPdeD-.js,
// pinned 0.5.2) decides WHICH tiles to request via its OWN internal, `errorTarget`-driven
// traversal (`requestTileContents`) -- a second, independent selection from this
// codebase's own `selectTiles3D`, not something this task replaces (that would be the
// "adapter owns the traversal" rewrite the task brief itself flags as likely beyond
// this task's budget; see tiles_layer.js's module docstring for the measured evidence).
// What the vendored renderer DOES expose, real and public in the pinned source (checked
// directly, not assumed): a plugin system -- `registerPlugin(plugin)`, and
// `invokeOnePlugin(fn)` trying each registered plugin's method before falling back to
// the renderer's own default (`renderer-tyqPdeD-.js` lines ~507-534) -- and, per tile,
// `requestTileContents()` calls `invokeOnePlugin((p) => p.fetchData && p.fetchData(url,
// {...this.fetchOptions, signal}))` (same file, ~line 768), where `signal` is a REAL
// per-tile `AbortSignal` the renderer itself creates and aborts on eviction. Whatever a
// plugin's `fetchData` resolves with flows straight into the renderer's own
// `.json()`/`.arrayBuffer()` -> `parseTile` -> mesh-under-`group` pipeline unchanged --
// this project does not reimplement glTF parsing (Q44's ratified decision).
//
// `ManagerGatedTilesFetchPlugin` (below) is registered on the live `TilesRenderer` and
// makes THIS ADAPTER's `_fetchForKey` the one and only place a real network fetch for a
// tile's content is ever started, for every URL this adapter's own `plan()` currently
// recognises (`_urlToKey`, refreshed every `plan()` call, translates the renderer's own
// `url` argument back to this adapter's local tile key):
//   - The renderer's own `fetchData` call (plugin, "wants this tile now") and
//     `LayerManager.update()`'s own `load()` call (adapter, "the manager admitted this
//     tile now") both resolve through `_fetchForKey`'s ONE cache, keyed by this
//     adapter's own LOCAL TILE KEY -- deliberately not by URL (see `_fetchesByKey`'s
//     own constructor comment for a real, measured reason: this project's own fixture
//     gives every tile the same relative content URI, and a URL-keyed cache would
//     silently let one tile's fetch satisfy an unrelated tile's request too).
//     Whichever of the two (renderer or manager) arrives first for a given key creates
//     the entry, the other reuses it. If the renderer asks before the manager has
//     admitted the owning request, the plugin gets a PENDING promise
//     (`awaitManagedFetch`) that is not fulfilled until `load()` -- called only once
//     `LayerManager.update()` decides to admit -- actually starts the fetch. This is
//     what makes "the manager decides what loads, when, within budget" literally true
//     for the overlay's real network bytes, not merely a throttle alongside a fetch
//     that already happened -- and it is structurally impossible for `_fetchForKey` to
//     call the injected loader twice for the same key (`fetchCallCount` is incremented
//     in exactly one branch of exactly one method; see that method's own comment and
//     `web/js/tiles3d_manager_check.mjs`'s direct proof by counting calls).
//   - A content URL the renderer's own traversal wants that this adapter's most recent
//     `plan()` does NOT recognise (the two selections genuinely can diverge -- different
//     algorithms over the same tree) falls back to a direct, UNGATED fetch
//     (`fetchDataFallbackCount`, counted, never silent) rather than hanging forever --
//     see `ManagerGatedTilesFetchPlugin.fetchData`'s own comment.
//   - `_realByteCostByKey`, populated once a real fetch resolves (from the `Response`'s
//     own `Content-Length` header -- real, server-declared, transferred bytes, not a
//     second read of the body `parseTile` still needs untouched), closes this task's own
//     analogue of round 4's defect 1 ("a cost captured at admission that nothing
//     re-read"): once populated, `plan()` reports the REAL size for that key, and
//     `LayerManager.update()`'s own existing reconciliation pass (layer.js, unmodified)
//     picks up the difference and adjusts `residentBytes` by exactly that delta --
//     the SAME mechanism `GatewayImageryLayerAdapter`'s manifest-driven byteCost already
//     uses, applied here to a per-tile HTTP response instead of a manifest.
import { selectTiles3D, regionBoundingSphereEcef } from '../tiles_layer.js';
import { dist, sseFromGeometricError } from '../globe_lod.js';
import { IMAGERY_TILE_BYTES } from './imagery_layer.js';

/** A glTF-bearing 3D tile's estimated resident byte cost, used when a tile's own
 * `tileset.json` entry carries no `content.byteLength` (which this project's fixture
 * generator, `web/fixtures/gen_3dtiles_fixture.py`, does not emit -- the 3D Tiles
 * spec does not require it, and `web/js/tiles_layer.js`'s `parseTileset3D` does not
 * currently read one) AND no real, measured size has been observed yet for that tile
 * (`_realByteCostByKey`, see this file's module docstring). A declared, documented
 * estimate (design constraint b), not a measurement -- deliberately an order of
 * magnitude above `imagery_layer.js`'s `IMAGERY_TILE_BYTES` (derived from it directly,
 * so the relationship stays true if that constant ever changes), matching this design
 * constraint's own framing of "a glTF-bearing 3D tile" as the heaviest of the three
 * per-item costs this module's byte budget has to reconcile.
 */
export const DEFAULT_TILE3D_BYTES = IMAGERY_TILE_BYTES * 10;

/** `Content-Length`, if the payload is a real `fetch()` `Response` and declares one --
 * `undefined` for anything else (a headless test's stub payload, a `Response` with no
 * declared length, ...), never a guess. Never reads the response BODY (`.arrayBuffer()`/
 * `.json()`): the vendored renderer's own `parseTile` pipeline is the one and only
 * consumer of the body (this file's module docstring, "Fetch gating") -- reading it a
 * second time here would either throw (a `Response` body streams once) or silently race
 * the renderer's own read, neither acceptable. */
function realByteLengthOf(payload) {
  if (!payload || typeof payload.headers?.get !== 'function') return undefined;
  const raw = payload.headers.get('content-length');
  if (raw == null) return undefined;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : undefined;
}

/** A `DOMException`-shaped `AbortError` in every target environment (the platform
 * constructor in a browser or a recent `node`; node18's `AbortController` already
 * throws exactly this shape natively when `signal.throwIfAborted()` is available, but
 * this file targets the lowest common shape rather than depending on that method). */
function makeAbortError() {
  try {
    return new DOMException('The operation was aborted.', 'AbortError');
  } catch {
    const e = new Error('The operation was aborted.');
    e.name = 'AbortError';
    return e;
  }
}

export class Tiles3DLayerAdapter {
  /**
   * @param {{id?: string, tree: {nodes: Map, rootId: string}, loader: Function, defaultByteCost?: number, resolveContentUrl?: (uri: string) => string}} opts
   *   `tree` is `web/js/tiles_layer.js`'s `parseTileset3D(tilesetJson)` output.
   *   `loader(request, signal): Promise<any>` is the injected per-tile content
   *   loader -- in live wiring, `(request, signal) => fetch(request.url, { signal })`
   *   (`web/js/tiles_layer.js`'s `TilesOverlayLayer`, see that file's module
   *   docstring); in a headless test, a network-free stub, same "loader as an
   *   injected stub" shape every adapter in this module uses (design constraint f).
   *   `resolveContentUrl(contentUri): string` turns a tile's raw, tileset-relative
   *   `content.uri` into the SAME absolute URL the vendored renderer itself resolves
   *   it to (`new URL(uri, basePath)`, the renderer's own `requestTileContents`) --
   *   required for `_urlToKey`/`awaitManagedFetch` to recognise the renderer's own
   *   `fetchData(url, ...)` calls as the same tile `plan()` already described.
   *   Defaults to the identity function (an already-absolute `contentUri`, exactly
   *   what every headless test's own fixture already uses).
   */
  constructor({
    id = 'tiles3d', tree, loader, defaultByteCost = DEFAULT_TILE3D_BYTES,
    resolveContentUrl = (uri) => uri,
  } = {}) {
    this.id = id;
    this._tree = tree;
    this._loader = loader;
    this._defaultByteCost = defaultByteCost;
    this._resolveContentUrl = resolveContentUrl;
    // Round 5 -- see this file's module docstring, "Fetch gating", for the full
    // mechanism. `_fetchesByKey`: Map<localKey, FetchEntry>, FetchEntry is either
    // `{started: true, promise}` (a real fetch is in flight or settled) or
    // `{started: false, waiters: [{resolve, reject}]}` (the renderer asked for this
    // TILE, via its content URL, before the manager admitted the owning request).
    // Keyed by this adapter's own LOCAL tile key -- deliberately NOT by URL: this
    // project's own fixture (`web/fixtures/gen_3dtiles_fixture.py`) happens to give
    // every tile node the SAME relative `content.uri` ("tile.glb"), so a cache keyed
    // by URL would silently let one key's real fetch satisfy a DIFFERENT key's own
    // `load()`/`awaitManagedFetch()` call too -- caught directly while building this
    // (a real, measured regression in `web/js/layers_check.mjs`'s own
    // `cancelledCount`/`evictedCount`/per-step resident counts before this was keyed
    // by tile key instead, see this task's own report). "No duplicate fetch" (this
    // task's own requirement) is about ONE tile's content never being fetched twice
    // -- once by the renderer's own traversal, once by the manager's own admission --
    // never about two DIFFERENT tiles that happen to reference the same bytes; a real
    // tileset's distinct tiles reference distinct files, and this project's own
    // per-key `release(key)` (below) already operates at this same granularity, so
    // keying the fetch cache identically is the natural, narrower-scoped choice, not
    // merely a fix applied after the fact. The ONE place a FetchEntry transitions
    // from `{started: false}` to `{started: true}` is `_fetchForKey`, called only
    // from `load()` (i.e. only once `LayerManager.update()` has decided to admit the
    // request) -- never from `awaitManagedFetch` (the plugin's own call), which only
    // ever reads or registers a waiter, never starts a fetch itself. This asymmetry
    // is the entire gate: the renderer can ASK, but only the manager's own admitted
    // `load()` can actually START a network request.
    this._fetchesByKey = new Map();
    // url -> local key (tile id), rebuilt on every `plan()` call (never merged
    // across calls) -- "the set of content URLs this adapter's OWN selectTiles3D
    // wants THIS tick", which is what lets `ManagerGatedTilesFetchPlugin` tell a URL
    // this adapter recognises from one only the vendored renderer's own internal
    // traversal wants (the two selections' genuine divergence -- see module
    // docstring), and what lets `awaitManagedFetch` translate the renderer's own
    // `url` argument back to the local key `_fetchesByKey` is actually indexed by.
    // A URL shared by several keys (this project's own fixture, see above) maps to
    // only the LAST key `plan()` happened to visit for it -- `awaitManagedFetch`'s
    // own doc comment covers exactly what that means for a shared-URL renderer ask.
    this._urlToKey = new Map();
    this._realByteCostByKey = new Map();
    // Running totals, never reset -- same "a thing not counted is not a thing this
    // project accepts" discipline `LayerManager`'s own `deferredCount`/`failedCount`
    // use (layer.js's constructor doc comment).
    this.fetchCallCount = 0;
    this.fetchDataFallbackCount = 0;
  }

  /**
   * `view.cameraEcef`/`screenHeightPx`/`fovYRad` are shared with the other adapters
   * (see `ImageryLayerAdapter.plan`); `view.sseThreshold`/`maxLevel`/`maxTiles` are
   * this layer's own `selectTiles3D` traversal knobs, passed through unchanged
   * (`undefined` falls back to `selectTiles3D`'s own defaults, exactly as calling it
   * directly would).
   */
  plan(view) {
    const {
      cameraEcef, screenHeightPx, fovYRad, sseThreshold, maxLevel, maxTiles,
    } = view;
    const ids = selectTiles3D(this._tree, cameraEcef, {
      screenHeightPx, fovYRad, sseThreshold, maxLevel, maxTiles,
    });
    this._urlToKey = new Map();
    return ids.map((id) => {
      const node = this._tree.nodes.get(id);
      const { center, radius } = regionBoundingSphereEcef(node.region);
      const d = Math.max(dist(cameraEcef, center) - radius, radius * 0.01, 1);
      const url = node.contentUri ? this._resolveContentUrl(node.contentUri) : null;
      if (url) this._urlToKey.set(url, id);
      const realBytes = this._realByteCostByKey.get(id);
      const byteCost = node.byteLength || realBytes || this._defaultByteCost;
      const byteCostSource = node.byteLength
        ? 'tileset-declared'
        : (realBytes !== undefined ? 'measured' : 'fallback-estimate');
      return {
        key: id,
        sseError: sseFromGeometricError(node.geometricError, d, screenHeightPx, fovYRad),
        viewDistanceM: d,
        byteCost,
        byteCostSource,
        // level (round 4, question 228): this node's own tree DEPTH, not a byte-cost
        // or screen-space notion -- `web/js/tiles_layer.js`'s own node ids are
        // dot-joined path strings ("0", "0.1", "0.2.0", ...), and `id.split('.').
        // length - 1` is exactly `selectTiles3D`'s own `level` local (see that
        // function's own traversal in tiles_layer.js) recovered from the id alone, so
        // this is not a second, independently-derived notion of depth -- the root
        // ("0") is level 0, the coarsest, matching layer.js's own `level` convention.
        level: id.split('.').length - 1,
        contentUri: node.contentUri,
        url,
      };
    });
  }

  /** Delegates to the injected loader THROUGH `_fetchForKey` (this file's module
   * docstring, "Fetch gating") when the request has a content URL; a request with no
   * content (an empty/bounding-volume-only node, which this project's own fixture
   * never emits but the 3D Tiles spec allows) falls through to the injected loader
   * directly -- there is no URL for a renderer-side `awaitManagedFetch` call to ever
   * correlate back to this key, so gating it through that cache would be meaningless,
   * not merely redundant. */
  load(request, signal) {
    if (!request.url) return this._loader(request, signal);
    return this._fetchForKey(request.key, request, signal);
  }

  /**
   * The ONE method that may ever call `this._loader` for a given KEY (see the
   * constructor's own comment on `_fetchesByKey`) -- called only from `load()`, i.e.
   * only once `LayerManager.update()` has admitted the request. If a
   * `ManagerGatedTilesFetchPlugin.fetchData` call already registered waiters for this
   * key (the renderer asked before the manager admitted it), they are resolved/
   * rejected with this SAME real fetch's own outcome -- never a second fetch for the
   * SAME key. Records the real byte length (`realByteLengthOf`) for `plan()`'s own
   * reconciliation, see this file's module docstring.
   * @returns {Promise<any>}
   */
  _fetchForKey(key, request, signal) {
    const existing = this._fetchesByKey.get(key);
    if (existing && existing.started) return existing.promise;
    this.fetchCallCount += 1;
    const promise = this._loader(request, signal);
    promise.then((payload) => {
      const bytes = realByteLengthOf(payload);
      if (bytes !== undefined) this._realByteCostByKey.set(key, bytes);
    }, () => {}); // measurement is best-effort; a rejection is handled by whoever awaits `promise` itself
    this._fetchesByKey.set(key, { started: true, promise });
    if (existing && existing.waiters) {
      for (const w of existing.waiters) promise.then(w.resolve, w.reject);
    }
    return promise;
  }

  /**
   * Called by `ManagerGatedTilesFetchPlugin.fetchData` on behalf of the vendored
   * renderer's own traversal -- NEVER starts a fetch itself (see `_fetchForKey`'s own
   * comment on the asymmetry that makes this the actual budget gate). `url` is
   * translated to this adapter's own local key via `_urlToKey` (populated by the most
   * recent `plan()` call; the caller -- `ManagerGatedTilesFetchPlugin` -- has already
   * checked `_urlToKey.has(url)` before calling, so `key` here is never `undefined`
   * in practice, but this method still degrades to a same-shape rejected promise
   * rather than throwing if it somehow were, matching this codebase's own "never a
   * silent gap, never a hard crash for a caller-observable edge" discipline). Returns
   * the real fetch's promise immediately if `load()` already started (or finished)
   * it; otherwise registers a waiter that `_fetchForKey` resolves the moment `load()`
   * eventually does start it. If `signal` (the renderer's OWN per-tile
   * `AbortSignal`, not the manager's) aborts first -- the renderer decided this tile
   * is no longer wanted before the manager ever admitted it -- the waiter is
   * withdrawn and rejected with a real `AbortError`, exactly what an aborted `fetch()`
   * itself would have produced, so the renderer's own `t.name === "AbortError"`
   * handling (checked directly against the pinned source, see this file's module
   * docstring) treats it identically to a fetch it started and cancelled itself.
   */
  awaitManagedFetch(url, signal) {
    const key = this._urlToKey.get(url);
    if (key === undefined) {
      return Promise.reject(new Error(`Tiles3DLayerAdapter.awaitManagedFetch: '${url}' is not a URL this adapter's most recent plan() recognised`));
    }
    const existing = this._fetchesByKey.get(key);
    if (existing && existing.started) return existing.promise;
    return new Promise((resolve, reject) => {
      let entry = existing;
      if (!entry) {
        entry = { started: false, waiters: [] };
        this._fetchesByKey.set(key, entry);
      }
      const waiter = { resolve, reject };
      entry.waiters.push(waiter);
      if (signal) {
        signal.addEventListener('abort', () => {
          const idx = entry.waiters.indexOf(waiter);
          if (idx !== -1) {
            entry.waiters.splice(idx, 1);
            reject(makeAbortError());
          } // else: already settled by _fetchForKey -- nothing to withdraw
        }, { once: true });
      }
    });
  }

  /** Frees THIS ADAPTER's own resource for `key`: the cached fetch entry (real or
   * still-pending) and any measured real byte cost, so a later re-want performs a
   * genuinely fresh, unreconciled fetch rather than silently reusing stale bytes or a
   * stale size estimate. The vendored `TilesRenderer`'s own `lruCache` owns real GPU/
   * mesh disposal for whatever it actually built from those bytes (unaffected by this
   * adapter, by design -- see this file's module docstring and Q44's ratified
   * "wrap, don't reimplement" decision); this method's own, honest claim is narrower
   * than "frees the renderer's GPU resource" and is exactly what this adapter
   * actually owns. */
  release(key) {
    this._realByteCostByKey.delete(key);
    this._fetchesByKey.delete(key);
  }
}

/**
 * A `TilesRenderer` plugin (`registerPlugin`, real and public in the pinned vendored
 * source -- see `Tiles3DLayerAdapter`'s own module docstring for the exact call sites
 * this was checked against) that makes `adapter` the sole gate on every per-tile
 * content fetch this renderer's own traversal wants, for every URL `adapter`'s most
 * recent `plan()` recognises -- round 5's concrete answer to "the renderer exposes a
 * hook/override for its fetch... that our adapter can drive" (this task's brief).
 */
export class ManagerGatedTilesFetchPlugin {
  /**
   * @param {{adapter: Tiles3DLayerAdapter, fallbackFetch?: Function}} opts
   *   `fallbackFetch(url, options): Promise<Response>` -- the platform `fetch` by
   *   default -- used ONLY for a content URL `adapter`'s current `plan()` does not
   *   recognise (see class docstring; `adapter.fetchDataFallbackCount` counts every
   *   use, never silently).
   */
  constructor({ adapter, fallbackFetch = (...args) => fetch(...args) }) {
    this.name = 'MANAGER_GATED_TILES_FETCH_PLUGIN';
    this._adapter = adapter;
    this._fallbackFetch = fallbackFetch;
  }

  /** Invoked by the vendored renderer's own `requestTileContents()`, once per tile it
   * decides (via its own internal traversal) needs content -- `options.signal` is the
   * renderer's own per-tile `AbortSignal`, already wired through to
   * `awaitManagedFetch`/`_fetchForKey` unchanged (never a second signal invented). */
  fetchData(url, options) {
    if (!this._adapter._urlToKey.has(url)) {
      this._adapter.fetchDataFallbackCount += 1;
      return this._fallbackFetch(url, options);
    }
    return this._adapter.awaitManagedFetch(url, options && options.signal);
  }
}

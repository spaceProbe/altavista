// web/js/layers/ -- the streaming-layer module (docs/heavy-plan.md milestone H5,
// first half / task H5a; docs/open-questions.md questions 44, 45, 46, 51). This file
// defines the one interface every streaming layer implements, and `LayerManager`, the
// single owner of:
//   1. a priority queue ordered by screen-space error (descending) and view distance
//      (ascending) -- see `comparePriority` below, which states and enforces the
//      exact rule;
//   2. a declared memory budget in BYTES (`memoryBudgetBytes`), not a tile/item count
//      -- see `LayerManager`'s own docstring for why a count is not a byte budget
//      once three layers with very different per-item costs (a 256x256 RGBA imagery
//      tile, a terrain mesh, a glTF-bearing 3D tile) share one manager;
//   3. cancellation of in-flight requests, via the platform `AbortController`/
//      `AbortSignal` (docs/open-questions.md question 48 -- available in node and
//      every target browser, not a bespoke token type), when the view moves and a
//      request drops out of the current plan.
//
// The one interface (a "Layer"), implemented by `./imagery_layer.js`,
// `./terrain_layer.js` and `./tiles3d_layer.js` -- the globe's imagery loader, the
// globe's terrain loader, and the vendored 3DTilesRendererJS overlay, respectively:
//   - `id: string` -- stable, unique per layer instance registered on one manager.
//   - `plan(view): Request[]` -- the requests this layer wants for `view` *right
//     now*. Never itself loads anything (see `web/js/layers_check.mjs`'s
//     `planNeverLoadsDirectly` and
//     `tests/test_viewer_layers.py::test_all_three_layers_share_one_manager_interface`
//     for the pinned proof) -- `plan()` is pure declaration of demand, so `LayerManager` alone
//     decides what actually starts loading, in priority order, against the shared
//     budget. Each `Request` is a plain object:
//       - `key: string` -- stable, unique *within this layer*. `LayerManager`
//         combines it with the layer's `id` (see `globalKeyFor`) so two layers may
//         reuse the same local key with no collision.
//       - `sseError: number` -- screen-space error, in pixels, of this content at
//         the current view (computed by reusing `web/js/globe_lod.js`'s/
//         `web/js/tiles_layer.js`'s own screen-space-error arithmetic -- never a
//         second copy of it, see each adapter's file).
//       - `viewDistanceM: number` -- camera-to-content distance, in metres.
//       - `byteCost: number` -- this request's estimated resident byte cost once
//         loaded (see each adapter's file for its documented estimate).
//   - `load(request, signal): Promise<any>` -- start loading one `Request` (the
//     exact object `plan()` returned). Must respect `signal` (a platform
//     `AbortSignal`): reject once it aborts. Resolves with whatever payload the
//     layer considers "loaded" (a texture, a mesh, a parsed glTF node -- opaque to
//     `LayerManager`, which only ever accounts its `byteCost`, never inspects it).
//   - `release(key): void` -- called once, with this layer's local `key`, when a
//     previously-resident item is evicted (LRU, see `LayerManager._evictIfNeeded`);
//     free whatever `load()` produced.
//
// No caller outside `web/js/layers/` needs to know which of the three it is talking
// to: every adapter's public surface is exactly the Layer interface above, and
// `LayerManager` is the only thing any caller drives (`addLayer` once per layer,
// `update(view)` once per frame/tick).
//
// Relationship to `web/js/globe_lod.js`'s `TileLoadScheduler` (binding rule: no
// second, independently-maintained copy of a budget/eviction rule -- either build on
// `TileLoadScheduler` or supersede it and say so, question 218's reasoning about
// `av-label` applied one level up in this codebase's own JS). `LayerManager`
// **supersedes** `TileLoadScheduler` for anything that goes through
// `web/js/layers/`: `TileLoadScheduler`'s budget is a tile *count*, which is only a
// byte-budget proxy when every resident item costs the same number of bytes -- true
// for the globe alone (uniform imagery tiles) and for the 3D Tiles overlay alone
// (uniform per-node estimate), which is exactly why `globe.js` and `tiles_layer.js`
// could reuse the *same* class for each separately (see `tiles_layer.js`'s module
// docstring). It stops being true the moment imagery, terrain and 3D Tiles share one
// budget, because their per-item byte costs differ by orders of magnitude -- so this
// module does not reuse `TileLoadScheduler`'s Map/eviction code, it reimplements the
// same LRU-until-under-budget *policy* against a byte total instead of an item count
// (`LayerManager._evictIfNeeded` below -- compare directly against
// `TileLoadScheduler._evictIfNeeded` in `globe_lod.js`: same shape, `resident.size`
// replaced by `residentBytes`). `globe_lod.js` is untouched by this task (see
// `web/VIEWER.md`'s new section) -- it keeps exporting `TileLoadScheduler` exactly as
// before, so `web/js/globe_lod_check.mjs` and `web/js/tiles3d_check.mjs` keep passing
// byte for byte, unchanged.

/** @typedef {{key:string, sseError:number, viewDistanceM:number, byteCost:number}} Request */

/** Combine a layer's `id` with one of its local request keys into the single
 * string key `LayerManager` actually indexes `resident`/`pending` by -- `\u0000`
 * (never a legal character in either half) makes the join unambiguous without a
 * parser, the same "no legal collision" reasoning `tileKey()` gets for free from
 * `/`-joining non-negative integers in `globe_lod.js`. */
export function globalKeyFor(layerId, localKey) {
  return `${layerId}\u0000${localKey}`;
}

/**
 * The priority-queue rule (design constraint d, binding): descending screen-space
 * error (the content that looks worst on screen is loaded first), ties broken by
 * ascending view distance (nearer first), ties broken by ascending global key (the
 * layer id then the local key, `globalKeyFor` above) so the order is always total
 * and deterministic -- two requests are only ever "equal" under this comparator if
 * they are the literal same request. No dependence on `Map`/`Set` insertion order or
 * any other traversal accident -- the same discipline `globe_lod.js`'s
 * `compareTiles`/`tiles_layer.js`'s `compareTileIds3D` already apply to tile
 * selection, applied here to cross-layer request priority.
 * @param {Request & {globalKey: string}} a
 * @param {Request & {globalKey: string}} b
 */
export function comparePriority(a, b) {
  if (a.sseError !== b.sseError) return b.sseError - a.sseError;
  if (a.viewDistanceM !== b.viewDistanceM) return a.viewDistanceM - b.viewDistanceM;
  if (a.globalKey === b.globalKey) return 0;
  return a.globalKey < b.globalKey ? -1 : 1;
}

/**
 * Owns the priority queue, the byte budget and cancellation across every registered
 * `Layer` (see this file's module docstring for the interface and the relationship
 * to `TileLoadScheduler`). One `LayerManager` is meant to be shared by every layer a
 * viewer scene needs (imagery, terrain, 3D Tiles), which is the entire point: one
 * budget, one queue, one cancellation policy, regardless of which layer a resident
 * byte belongs to.
 */
export class LayerManager {
  /**
   * @param {{memoryBudgetBytes:number, now?: () => number}} opts `memoryBudgetBytes`
   *   is the hard cap `residentBytes` is evicted back under after every `update()`
   *   (see `_evictIfNeeded`). `now` (design constraint g: "no clocks slept, ever" --
   *   inject time rather than reading a real clock in anything test-observable)
   *   defaults to the platform `performance.now`; it is stored as `loadedAt` on each
   *   resident entry as a *string* (design constraint h: a timestamp that could ever
   *   reach the browser/JSON crosses as a string, never a number -- see
   *   `_onLoaded`), never used for eviction ordering itself (that is step-counted
   *   LRU, exactly `TileLoadScheduler`'s `lastUsedStep`/`_step` scheme, which needs
   *   no clock at all).
   */
  constructor({ memoryBudgetBytes, now = defaultNow } = {}) {
    if (!(Number.isFinite(memoryBudgetBytes) && memoryBudgetBytes > 0)) {
      throw new TypeError('LayerManager: memoryBudgetBytes must be a positive finite number of bytes');
    }
    this.memoryBudgetBytes = memoryBudgetBytes;
    this._now = now;
    /** @type {Map<string, import('./layer.js').Layer>} */
    this._layers = new Map();
    /** @type {Map<string, {layerId:string, localKey:string, byteCost:number, lastUsedStep:number, payload:any, loadedAt:string}>} */
    this.resident = new Map();
    /** @type {Map<string, {layerId:string, localKey:string, byteCost:number, controller:AbortController, startedStep:number}>} */
    this.pending = new Map();
    this.residentBytes = 0;
    this.cancelledCount = 0;
    this.evictedCount = 0;
    this._step = 0;
  }

  /** Register one layer (its `id` must be unique on this manager). */
  addLayer(layer) {
    if (this._layers.has(layer.id)) {
      throw new Error(`LayerManager: a layer with id '${layer.id}' is already registered`);
    }
    this._layers.set(layer.id, layer);
  }

  /**
   * One "frame"/tick: ask every registered layer what it wants for `view`, merge
   * and priority-sort the result (`comparePriority`), cancel any in-flight request
   * that fell out of the new plan, start a load for anything newly wanted, and run
   * eviction. Never awaits a `load()` promise -- results land later via
   * `_onLoaded`/`_onFailed`, exactly like `TileLoadScheduler.update()` never blocks
   * on a load either (there, the harness/caller simulates completion explicitly;
   * here, `load()` is a real `Promise`, but `update()` itself still returns
   * synchronously with the priority-ordered plan for this step).
   * @param {any} view whatever shape every registered layer's `plan()` agrees on
   *   (see each adapter file for what it reads off `view`); opaque to `LayerManager`.
   * @returns {Array<Request & {layerId:string, globalKey:string}>} the merged,
   *   priority-ordered plan for this step (already sorted -- this *is* the "priority
   *   order the harness reports" design constraint d requires be pinned in a test).
   */
  update(view) {
    this._step += 1;
    const allRequests = [];
    for (const layer of this._layers.values()) {
      const reqs = layer.plan(view) || [];
      for (const r of reqs) {
        allRequests.push({
          ...r,
          layerId: layer.id,
          globalKey: globalKeyFor(layer.id, r.key),
        });
      }
    }
    allRequests.sort(comparePriority);

    const wanted = new Set(allRequests.map((r) => r.globalKey));

    // Cancellation: any pending load whose request is no longer wanted is aborted
    // right here, synchronously -- `cancelledCount` is incremented at the point of
    // `abort()`, not from the (later, microtask-scheduled) rejection handler, so it
    // is observable immediately after `update()` returns, exactly the property
    // `TileLoadScheduler.update()` already has for the globe.
    for (const [globalKey, p] of this.pending) {
      if (!wanted.has(globalKey)) {
        p.controller.abort();
        this.pending.delete(globalKey);
        this.cancelledCount += 1;
      }
    }

    // Start a load for anything newly wanted; refresh LRU for anything already
    // resident. `raw` is preserved (spread above) so `layer.load()` receives back
    // exactly the object its own `plan()` produced, including any layer-private
    // fields (e.g. `url`, `contentUri`) beyond the four the interface requires.
    for (const r of allRequests) {
      if (this.resident.has(r.globalKey)) {
        this.resident.get(r.globalKey).lastUsedStep = this._step;
        continue;
      }
      if (this.pending.has(r.globalKey)) continue;
      const controller = new AbortController();
      this.pending.set(r.globalKey, {
        layerId: r.layerId, localKey: r.key, byteCost: r.byteCost, controller, startedStep: this._step,
      });
      const layer = this._layers.get(r.layerId);
      layer.load(r, controller.signal).then(
        (payload) => this._onLoaded(r, controller, payload),
        () => this._onFailed(r.globalKey, controller),
      );
    }

    this._evictIfNeeded(wanted);
    return allRequests;
  }

  _onLoaded(request, controller, payload) {
    const p = this.pending.get(request.globalKey);
    // Guard against a stale resolution: the pending entry for this key may already
    // be gone (cancelled) or may belong to a *newer* load started after a cancel+
    // re-request race (a fresh AbortController, `p.controller !== controller`) --
    // either way, a promise settling after its own bookkeeping was superseded must
    // be ignored, not allowed to resurrect a evicted/cancelled entry.
    if (!p || p.controller !== controller) return;
    this.pending.delete(request.globalKey);
    this.resident.set(request.globalKey, {
      layerId: request.layerId,
      localKey: request.key,
      byteCost: request.byteCost,
      lastUsedStep: this._step,
      payload,
      loadedAt: String(this._now()), // design constraint h: a string, never a number
    });
    this.residentBytes += request.byteCost;
    this._evictIfNeeded();
  }

  _onFailed(globalKey, controller) {
    const p = this.pending.get(globalKey);
    if (!p || p.controller !== controller) return;
    this.pending.delete(globalKey);
    // A rejection (abort, a typed refusal such as
    // TerrainLoaderNotImplementedError, or any other loader error) never becomes
    // resident and never charges the byte budget -- there is nothing to evict here,
    // only bookkeeping to drop. Cancellation itself is already counted, in
    // `update()`, at `abort()` time; this handler exists so an *uncancelled* load
    // that simply fails (e.g. the terrain layer's permanent refusal) also cleans up
    // `pending`, not so it double-counts a cancellation.
  }

  /** LRU-until-at-or-under-budget eviction, by byte total (design constraint b),
   * never touching a request key in `protectedKeys` (this step's own wanted set, so
   * a tile freshly selected this very step is never evicted to make room for
   * itself). Directly comparable to `TileLoadScheduler._evictIfNeeded` in
   * `globe_lod.js`: identical LRU policy, `resident.size > residentBudget` (a count)
   * replaced by `residentBytes > memoryBudgetBytes` (a byte total) -- see this
   * file's module docstring for why that is a supersession, not a second
   * independent implementation of the same rule.
   * @param {Set<string>} [protectedKeys]
   */
  _evictIfNeeded(protectedKeys) {
    while (this.residentBytes > this.memoryBudgetBytes) {
      let victimKey = null;
      let victim = null;
      for (const [globalKey, entry] of this.resident) {
        if (protectedKeys && protectedKeys.has(globalKey)) continue;
        // Least-recently-used first; ties broken by ascending globalKey so the
        // victim choice never depends on Map iteration order (design constraint d's
        // "total and deterministic" discipline, applied here too).
        if (
          victim === null
          || entry.lastUsedStep < victim.lastUsedStep
          || (entry.lastUsedStep === victim.lastUsedStep && globalKey < victimKey)
        ) {
          victim = entry;
          victimKey = globalKey;
        }
      }
      if (victimKey === null) break; // everything resident is protected; budget soft-violated this step
      this.resident.delete(victimKey);
      this.residentBytes -= victim.byteCost;
      const layer = this._layers.get(victim.layerId);
      if (layer) layer.release(victim.localKey);
      this.evictedCount += 1;
    }
  }

  /** Read-only lookup a caller outside `web/js/layers/` can use without knowing
   * which layer produced a given resident item -- the concrete form of "no caller
   * outside web/js/layers/ has to know which of the three it is talking to" for
   * already-loaded content. Returns `undefined` if `globalKey` is not resident. */
  getResidentPayload(globalKey) {
    return this.resident.get(globalKey)?.payload;
  }

  /** Per-layer resident/pending counts -- for reporting/debugging (this task's
   * harness prints these; see `web/js/layers_check.mjs`). */
  countsByLayer() {
    const counts = {};
    for (const id of this._layers.keys()) counts[id] = { resident: 0, pending: 0 };
    for (const entry of this.resident.values()) counts[entry.layerId].resident += 1;
    for (const entry of this.pending.values()) counts[entry.layerId].pending += 1;
    return counts;
  }
}

function defaultNow() {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

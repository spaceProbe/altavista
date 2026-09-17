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
   * @param {{memoryBudgetBytes:number, now?: () => number, maxConcurrentLoads?: number}} opts
   *   `memoryBudgetBytes` is the hard cap `residentBytes` is evicted back under after
   *   every `update()` (see `_evictIfNeeded`). `now` (design constraint g: "no clocks
   *   slept, ever" -- inject time rather than reading a real clock in anything
   *   test-observable) defaults to the platform `performance.now`; it is stored as
   *   `loadedAt` on each resident entry as a *string* (design constraint h: a
   *   timestamp that could ever reach the browser/JSON crosses as a string, never a
   *   number -- see `_onLoaded`), never used for eviction ordering itself (that is
   *   step-counted LRU, exactly `TileLoadScheduler`'s `lastUsedStep`/`_step` scheme,
   *   which needs no clock at all).
   *
   *   `maxConcurrentLoads` (manager review finding on H5a: "the priority queue is a
   *   sorted list, not a queue, because nothing is ever deferred") is the hard cap on
   *   `pending.size` -- `update()` starts a load for a newly-wanted request only
   *   while `pending.size < maxConcurrentLoads`; everything past the cap stays merely
   *   "wanted" and is started on a later `update()`, still in priority order, as
   *   slots free (a load completes/`_onLoaded`/`_onFailed`, or is cancelled). Default
   *   6: a real browser caps concurrent HTTP/1.1 connections to one origin at ~6
   *   (Chrome/Firefox/Safari's long-standing shared limit -- `av-tiles` is fronted by
   *   nginx in every deployed tier, docs/heavy-plan.md's H7, and this codebase makes
   *   no assumption that the fronting proxy negotiates HTTP/2 multiplexing in every
   *   environment this runs in), so 6 is the real constraint this cap models, not an
   *   arbitrary number.
   */
  constructor({ memoryBudgetBytes, now = defaultNow, maxConcurrentLoads = 6 } = {}) {
    if (!(Number.isFinite(memoryBudgetBytes) && memoryBudgetBytes > 0)) {
      throw new TypeError('LayerManager: memoryBudgetBytes must be a positive finite number of bytes');
    }
    if (!(Number.isInteger(maxConcurrentLoads) && maxConcurrentLoads > 0)) {
      throw new TypeError('LayerManager: maxConcurrentLoads must be a positive integer');
    }
    this.memoryBudgetBytes = memoryBudgetBytes;
    this.maxConcurrentLoads = maxConcurrentLoads;
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
    // The current wanted set, remembered across calls -- see `_evictIfNeeded`'s own
    // doc comment for why `update()` and `_onLoaded()` must protect the SAME set
    // (previously `update()` passed its freshly-computed `wanted` and `_onLoaded()`
    // passed none at all, an inconsistency a manager review flagged on H5a).
    // Starts empty (nothing is protected before the first `update()` ever runs).
    this._wantedKeys = new Set();
    // Incremented every time `_evictIfNeeded`'s own `break` fires because every
    // resident item is protected and the budget still could not be brought back
    // under `memoryBudgetBytes` -- see `_evictIfNeeded`'s doc comment: this is what
    // makes a green `budgetRespected` unable to hide a soft violation silently.
    this.softViolationCount = 0;

    // Failure memory (Finding 1, manager review finding on H5b-2, corrective round 3
    // -- root-caused directly by the manager's own probe: two layers, identical
    // per-request priorities, one whose load() always resolves and one whose load()
    // always rejects, ten requests each, maxConcurrentLoads 6, thirty update() steps
    // with every settled promise drained between steps -- measured 175 failed load
    // attempts, 5/10 good tiles never loaded). Without this, `_onFailed` had no
    // memory of a failed globalKey at all, so `update()` (below) replanned the exact
    // same permanently-failing request on literally every subsequent step: it took a
    // concurrent-load slot, failed again, and repeated forever. When such a request
    // sorts ahead of everything else in priority order (`comparePriority`), it alone
    // can consume every slot, every step, starving every lower-priority request
    // behind it for as long as it stays wanted.
    //
    // The policy, stated in full:
    //   - `_onFailed` records a failed globalKey in `_failed` (this globalKey's
    //     `.name` and the step it failed on -- see `_onFailed`, below);
    //   - `update()` never starts a new load for a globalKey present in `_failed`;
    //   - `update()` clears a globalKey's `_failed` entry the moment that key drops
    //     OUT of the current wanted set for even one step -- so a key that stops
    //     being wanted and later returns (the view changed and came back) is retried
    //     completely fresh, exactly as if it had never failed. Concretely: a
    //     genuinely transient failure recovers the next time the view changes enough
    //     to make the key drop out of plan() even briefly; a permanently-failing key
    //     whose owning layer keeps declaring it as wanted forever (e.g. the terrain
    //     adapter's disclosed, permanent refusal, see terrain_layer.js) stays
    //     blacklisted for as long as it stays wanted, which is exactly the intended
    //     fix -- it is asked for once, not once per step, forever.
    //   - `failedCount` (a running total, never reset) and `_failureNames` (the
    //     distinct `.name`s ever seen) are updated on every recorded failure, and
    //     both are reported by web/js/layers_check.mjs and
    //     web/js/layers_stream_check.mjs unconditionally -- a failure that is not
    //     counted is not a failure this project accepts.
    //
    // What this policy deliberately does NOT do:
    //   - no exponential backoff or any other retry timer -- there is no "wait N
    //     seconds and try again" state; a blacklisted key is retried on the very next
    //     update() it becomes wanted again after having dropped out, however soon or
    //     late that happens to be, never on a schedule;
    //   - no retry budget or attempt cap per key -- there is no "retry up to N times
    //     then give up permanently" counter; the ONLY thing that ever clears a
    //     blacklist entry is the key leaving the wanted set, and the ONLY thing that
    //     ever creates one is a single failure;
    //   - no distinction between a permanent, typed refusal
    //     (TerrainLoaderNotImplementedError) and a transient failure (a flaky network
    //     error, an aborted fetch that was not itself a cancellation) -- both are
    //     blacklisted identically while the key stays wanted; "retried once the view
    //     changes" is what makes a transient failure recoverable at all, not a
    //     special case written for it.
    /** @type {Map<string, {name:string, step:number}>} */
    this._failed = new Map();
    this.failedCount = 0;
    /** @type {Set<string>} */
    this._failureNames = new Set();
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
    this._wantedKeys = wanted;

    // Failure-memory upkeep (see the constructor's own doc comment for the full
    // policy): a blacklisted key is forgotten the moment it drops out of the wanted
    // set -- checked here, once per update(), against the wanted set this step just
    // computed, so a key that stops being wanted for even one step returns to a clean
    // slate the next time something wants it again.
    for (const key of this._failed.keys()) {
      if (!wanted.has(key)) this._failed.delete(key);
    }

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

    // Start a load for anything newly wanted, in priority order, only while a
    // concurrent-load slot is free (manager review finding on H5a -- see the
    // constructor's own doc comment for `maxConcurrentLoads`: this is what makes the
    // priority order load-bearing rather than merely a sorted list nothing ever
    // reads). `allRequests` is already sorted by `comparePriority` above, so walking
    // it in order and stopping new starts once `pending.size` reaches the cap is
    // exactly "start the top-N by priority, defer the rest" -- a request past the cap
    // this step is simply left neither resident nor pending, so it is asked for
    // again (and reconsidered in priority order against whatever has freed up) on
    // every later `update()` until a slot opens. Refresh LRU for anything already
    // resident. `r` is preserved (spread above) so `layer.load()` receives back
    // exactly the object its own `plan()` produced, including any layer-private
    // fields (e.g. `url`, `contentUri`) beyond the four the interface requires.
    for (const r of allRequests) {
      if (this.resident.has(r.globalKey)) {
        this.resident.get(r.globalKey).lastUsedStep = this._step;
        continue;
      }
      if (this.pending.has(r.globalKey)) continue;
      if (this._failed.has(r.globalKey)) continue; // blacklisted while still wanted -- see constructor's failure-memory policy
      if (this.pending.size >= this.maxConcurrentLoads) continue; // deferred to a later update()
      const controller = new AbortController();
      this.pending.set(r.globalKey, {
        layerId: r.layerId, localKey: r.key, byteCost: r.byteCost, controller, startedStep: this._step,
      });
      const layer = this._layers.get(r.layerId);
      layer.load(r, controller.signal).then(
        (payload) => this._onLoaded(r, controller, payload),
        (err) => this._onFailed(r.globalKey, controller, err),
      );
    }

    this._evictIfNeeded();
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

  /** How many concurrent-load slots are free right now -- `maxConcurrentLoads -
   * pending.size`, never negative. Exposed for callers/harnesses that want to
   * report queue depth; `update()` itself only ever reads `pending.size` directly. */
  freeLoadSlots() {
    return Math.max(0, this.maxConcurrentLoads - this.pending.size);
  }

  _onFailed(globalKey, controller, err) {
    const p = this.pending.get(globalKey);
    if (!p || p.controller !== controller) return;
    this.pending.delete(globalKey);
    // A rejection (abort, a typed refusal such as
    // TerrainLoaderNotImplementedError, or any other loader error) never becomes
    // resident and never charges the byte budget -- there is nothing to evict here,
    // only bookkeeping to drop. Cancellation itself is already counted, in
    // `update()`, at `abort()` time (and a cancelled request's `pending` entry is
    // already gone by the time its rejection reaches here, so the guard above
    // already excludes it from everything below) -- this handler exists so an
    // *uncancelled* load that simply fails also cleans up `pending`, not so it
    // double-counts a cancellation.
    //
    // Failure memory (constructor's own doc comment has the full policy): record
    // this globalKey as blacklisted -- `update()` will not start a new load for it
    // again until it drops out of the wanted set for at least one step. `failedCount`
    // and `_failureNames` are updated unconditionally, for every genuine failure this
    // guard lets through, so a caller/harness can always observe that a failure
    // happened, never just infer it from an absence.
    const name = (err && err.name) || 'Error';
    this._failed.set(globalKey, { name, step: this._step });
    this.failedCount += 1;
    this._failureNames.add(name);
  }

  /** Sorted array of every distinct error `.name` `_onFailed` has ever recorded --
   * see the constructor's own doc comment on the failure-memory policy. Exposed as a
   * plain array (not the internal `Set`) so a caller/harness can `JSON.stringify` it
   * directly. */
  failureNames() {
    return [...this._failureNames].sort();
  }

  /** LRU-until-at-or-under-budget eviction, by byte total (design constraint b),
   * never touching a request key in `this._wantedKeys` (the CURRENT wanted set --
   * see below for why "current" now means the same thing at both call sites).
   * Directly comparable to `TileLoadScheduler._evictIfNeeded` in `globe_lod.js`:
   * identical LRU policy, `resident.size > residentBudget` (a count) replaced by
   * `residentBytes > memoryBudgetBytes` (a byte total) -- see this file's module
   * docstring for why that is a supersession, not a second independent
   * implementation of the same rule.
   *
   * Protection semantics chosen (manager review finding on H5a): `update()` and
   * `_onLoaded()` used to disagree -- `update()` called this with its
   * freshly-computed wanted set, `_onLoaded()` called it with none at all, so a load
   * that completed BETWEEN two `update()` calls could evict something the current
   * view still wants, while eviction running synchronously inside `update()` never
   * could. Both call sites now protect the SAME thing: `this._wantedKeys`, the most
   * recent `update()`'s wanted set (persisted on the instance, not recomputed) --
   * chosen over "protect nothing" because the alternative lets a tile this step
   * itself just selected be evicted to make room for another tile the same step
   * selected, which is a worse bug (thrash within one step) than the one this
   * fixes. The cost of "protect everything currently wanted" is that the budget
   * becomes soft-violable: if every resident byte belongs to the wanted set and the
   * total still exceeds `memoryBudgetBytes`, there is nothing left this function is
   * willing to evict, and it gives up (`break`, below) rather than evict something
   * the view still needs. That branch is never silent: `softViolationCount` (see the
   * constructor) is incremented every time it fires, specifically so a harness that
   * only checks `residentBytes <= memoryBudgetBytes` after the fact cannot be fooled
   * by a run where the check passed merely because nothing was ever big enough to
   * trigger the soft-violation path -- report `softViolationCount` alongside any
   * budget assertion (see `web/js/layers_check.mjs` and `web/js/layers_stream_check.mjs`).
   */
  _evictIfNeeded() {
    const protectedKeys = this._wantedKeys;
    while (this.residentBytes > this.memoryBudgetBytes) {
      let victimKey = null;
      let victim = null;
      for (const [globalKey, entry] of this.resident) {
        if (protectedKeys.has(globalKey)) continue;
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
      if (victimKey === null) {
        // Everything resident is protected; budget soft-violated this step -- see
        // this method's own doc comment. Counted, never silent.
        this.softViolationCount += 1;
        break;
      }
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

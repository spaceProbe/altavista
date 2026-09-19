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
//      tile, a terrain mesh, a glTF-bearing 3D tile) share one manager. As of round 4
//      (below) this is a HARD admission limit, not merely a post-hoc eviction target;
//   3. cancellation of in-flight requests, via the platform `AbortController`/
//      `AbortSignal` (docs/open-questions.md question 48 -- available in node and
//      every target browser, not a bespoke token type), when the view moves and a
//      request drops out of the current plan.
//
// Round 4 (docs/open-questions.md question 228, the lead's own browser drive):
// finding 1 measured the defect this module shipped with -- with a 40 MB
// `memoryBudgetBytes` and a real 3,147,060-byte-per-tile imagery set, the resident set
// reached 96 MB in a real browser, because `update()` admitted a load for EVERY wanted
// request with no reference to the budget at all, and `_evictIfNeeded` (below) refuses
// to evict anything in the current wanted set -- so once the wanted set alone exceeded
// the budget there was nothing left it was willing to evict, and it gave up
// (`softViolationCount`) rather than actually bound `residentBytes`. The budget was a
// regret recorded after the fact, not a limit. The fix, in full:
//   - `update()` now starts a load for a request ONLY while `residentBytes +
//     pendingBytes + request.byteCost <= memoryBudgetBytes` (`pendingBytes`, new,
//     the summed byteCost of everything currently in `pending` -- reserved capacity
//     for loads in flight, which is what makes the invariant hold across the async gap
//     between admission and `_onLoaded`) -- see `update()`'s own comment for the exact
//     invariant this creates and why it makes `_evictIfNeeded`'s soft-violation branch
//     unreachable by construction, not merely unlikely.
//   - Admission is walked in a SEPARATE order from the one `update()` reports/returns:
//     coarser levels first (`compareAdmission`, below), so the tiles that cover the
//     whole view are never starved out by the finest tiles a hard limit would
//     otherwise let fill the budget first, leaving the user looking at nothing.
//   - Before admitting a request that does not currently fit, `update()` evicts
//     least-recently-used UNWANTED resident entries (never anything in the current
//     wanted set) to try to make room -- otherwise a hard limit alone would deadlock
//     the viewer the moment the camera moves: the budget stays full of tiles the old
//     view wanted, and nothing new can ever be admitted. A request that still does not
//     fit after that is deferred (`deferredCount`/`lastStepDeferred`, new) and
//     `update()` moves on to the NEXT request in admission order, never `break`s.
// `web/js/layers_budget_check.mjs` (new) is this fix's own proof: a wanted set 2.4x
// the budget, asserting `softViolationCount === 0`, `deferredCount > 0`, and that the
// resident steady state is exactly the coarse tiles the budget can actually afford.
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
//       - `level: number` (OPTIONAL, round 4 -- see question 228's finding 1 below) --
//         how coarse this content is: 0 is the coarsest representation a layer can
//         produce, larger numbers are finer (a deeper quadtree/tile-tree level).
//         Populated by every adapter that has a real notion of coarseness --
//         `./imagery_layer.js`/`./terrain_layer.js` use the selected tile's own
//         `tile.level`; `./tiles3d_layer.js` uses the selected node's own path depth
//         (`id.split('.').length - 1`, see that file's `plan()`). A layer with no
//         meaningful level (none of the four adapters this module ships today, but the
//         interface leaves room for one) may omit it entirely; `LayerManager` then
//         treats it as level 0, the coarsest possible -- see `compareAdmission` below
//         for why defaulting an unknown level to "coarsest" is the safe choice (it
//         biases an unlevelled layer's content toward being admitted early, same as
//         genuinely coarse content, rather than starving it behind every leveled
//         layer's finest requests).
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

/** @typedef {{key:string, sseError:number, viewDistanceM:number, byteCost:number, level?:number}} Request */

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
 * The ADMISSION-order rule (round 4, question 228's decision: "deferred requests wait
 * in priority order, coarser levels admitted first, so there is always something to
 * draw"), deliberately a DIFFERENT total order from `comparePriority` above:
 * ascending `level` first (0 -- coarsest -- wins; a missing `level` is treated as 0,
 * see this file's module docstring and the `Request` typedef), falling through to
 * `comparePriority` for everything tied at the same level.
 *
 * Why two orders, not one: `comparePriority` answers "what does the view want most,
 * right now" (highest screen-space error first) -- that is what `update()` still
 * RETURNS, unchanged, because `tests/test_viewer_layers.py` pins that order and a
 * caller's rendering/debug view of "what's most urgently wanted" should not depend on
 * how the budget happens to be enforced this step. `compareAdmission` instead answers
 * "what should be let through the budget first" -- under a HARD admission limit,
 * always admitting the highest-sseError (typically the finest, most zoomed-in) content
 * first can fill the entire budget with fine detail before the coarse tiles that cover
 * the whole view ever get a turn, leaving the user looking at nothing at all. Coarser
 * levels-first inverts that: the tiles that make the view show SOMETHING are always
 * considered for admission before the tiles that merely refine an already-covered
 * area. `update()` walks a separate, freshly-sorted copy of the plan by this
 * comparator for admission only; the array it returns/reports is untouched.
 *
 * Like `comparePriority`, this must be a TOTAL, deterministic order with no dependence
 * on `Map`/`Set` iteration order -- ties (including the level tie-break itself falling
 * through) are resolved by `comparePriority`, which itself bottoms out at ascending
 * `globalKey`, so two distinct requests never compare equal under this comparator
 * either.
 * @param {Request & {globalKey: string}} a
 * @param {Request & {globalKey: string}} b
 */
export function compareAdmission(a, b) {
  const aLevel = a.level ?? 0;
  const bLevel = b.level ?? 0;
  if (aLevel !== bLevel) return aLevel - bLevel;
  return comparePriority(a, b);
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
   *   `memoryBudgetBytes` is, as of round 4 (question 228's decision -- see this
   *   file's module docstring), a HARD admission limit: `update()` starts a load for a
   *   request only while `residentBytes + pendingBytes + request.byteCost <=
   *   memoryBudgetBytes` (see `update()`'s own comment for the exact invariant this
   *   creates). `_evictIfNeeded` still runs every step, kept as the tripwire that
   *   PROVES the invariant rather than the thing that enforces it now -- see that
   *   method's own doc comment. `now` (design constraint g: "no clocks
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
   *   while `pending.size < maxConcurrentLoads` AND the round-4 budget admission
   *   condition above also holds (BOTH conditions, not either -- see `update()`'s own
   *   comment); everything past the cap stays merely
   *   "wanted" and is started on a later `update()`, still in admission order, as
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
    // Round 4 (question 228): the summed byteCost of every entry currently in
    // `pending` -- reserved capacity for loads that have been admitted but have not
    // yet settled. Maintained incrementally (never recomputed by summing `pending`)
    // at every place an entry enters or leaves `pending`: admission in `update()`
    // (+=), cancellation in `update()` (-=), `_onLoaded` (-=, the entry moves to
    // `resident` instead), `_onFailed` (-=). This is what makes
    // `residentBytes + pendingBytes <= memoryBudgetBytes` hold across the ASYNC gap
    // between "a load was admitted" and "that load settled" -- without it, two loads
    // admitted back to back, each individually fitting against `residentBytes` alone,
    // could together resolve into more resident bytes than the budget allows.
    this.pendingBytes = 0;
    this.cancelledCount = 0;
    this.evictedCount = 0;
    // Round 4 (question 228): a running total (never reset), incremented once per
    // request per `update()` that this manager wanted to start but could not, SOLELY
    // because admitting it would have broken `residentBytes + pendingBytes +
    // request.byteCost <= memoryBudgetBytes` even after evicting every evictable
    // (unwanted) resident entry -- see `update()`'s own comment. Deliberately does
    // NOT count a request held back only by `pending.size >= maxConcurrentLoads`, or
    // one blacklisted by the failure-memory policy below: those are not budget
    // deferrals, and counting them here would make `deferredCount > 0` prove nothing
    // about the budget specifically (a run could report a nonzero count purely from
    // concurrency pressure with the budget never actually binding).
    this.deferredCount = 0;
    // How many requests were counted into `deferredCount` on the MOST RECENT
    // `update()` alone -- reset to 0 at the top of every `update()`, unlike
    // `deferredCount` itself. Lets a caller/harness see per-step deferral pressure
    // (e.g. "did this specific camera jump defer anything") without having to diff
    // `deferredCount` across two calls itself.
    this.lastStepDeferred = 0;
    // Round 4 follow-up (manager review): a running total (never reset), incremented
    // once per already-resident WANTED request per `update()` whose freshly-planned
    // `byteCost` differed from its stored one -- see `update()`'s own reconciliation
    // pass, immediately after cancellation, for the full reasoning (this is the fix
    // for a resident entry's cost never being revisited after admission, which
    // `GatewayImageryLayerAdapter`'s own `fetchManifest()` window makes a real, not
    // hypothetical, hole).
    this.byteCostRevisionCount = 0;
    // The NET SIGNED byte delta every reconciliation has ever applied (never reset) --
    // an upward revision (the fallback estimate was too LOW) contributes a positive
    // number, a downward one a negative number; this can itself be negative overall
    // if downward revisions dominate a run. `residentBytes` always equals what it
    // would equal by re-summing `resident` from scratch, because every reconciliation
    // applies this exact signed delta to `residentBytes` too (see `update()`) -- this
    // counter is a running record of "how much correction was needed", not a
    // component `residentBytes` is derived from.
    this.byteCostRevisionBytes = 0;
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
    // makes a green `budgetRespected` unable to hide a soft violation silently. Round
    // 4's own admission check makes this unreachable for an ADMISSION-time violation
    // (see `update()`'s own "THE INVARIANT" comment); the round-4 follow-up byteCost
    // reconciliation pass (also in `update()`, and `byteCostRevisionCount`/
    // `byteCostRevisionBytes` above) is the one remaining path that can legitimately
    // make this fire -- a revision that pushes an already-resident, still-wanted
    // entry's true cost up with nothing unwanted left to evict for it is a genuine,
    // honestly-reported violation, not a bug in this counter.
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
   * Unregister a layer previously added under `id` -- `addLayer`'s own lifecycle
   * counterpart, added by the round-4 wiring task (question 228 finding 2:
   * "wire the globe through the LayerManager") the moment a REAL caller needed it.
   * `web/js/scene.js`'s `Viewer` creates exactly ONE `LayerManager` per viewer ("one
   * manager, one budget", this file's own module docstring) and keeps it for the
   * viewer's whole lifetime, but `enableGlobe()`/`disableGlobe()` construct and
   * dispose a FRESH `GlobeLayer` instance on every call -- each of which registers
   * its own imagery/terrain adapters on that SAME long-lived manager, under the SAME
   * stable ids (`'imagery'`/`'terrain'`). Without a way to unregister the OLD
   * adapters first, a second `enableGlobe()` call would throw on `addLayer`'s own
   * already-registered guard above, and worse, an old, disposed `GlobeLayer`'s stale
   * `textureLoader` would stay silently registered forever, still declaring demand
   * (`plan()`) and consuming shared budget for content nothing renders any more --
   * exactly the zombie-registration failure mode this method exists to make
   * impossible. This is an ADDITIVE lifecycle method: no existing caller/check/test
   * ever calls it, so every one of them is unaffected -- proven by re-running
   * `web/js/layers_check.mjs`/`web/js/layers_budget_check.mjs` byte-for-byte
   * unchanged after adding it (see this task's own report).
   *
   * Cancels every pending load this layer owns (identical abort()/pendingBytes/
   * cancelledCount bookkeeping to `update()`'s own cancellation loop, above), evicts
   * every resident entry this layer owns (via `_evictEntry`, so `release(localKey)`
   * and `evictedCount` get the same treatment any other eviction gets), drops any
   * `_failed`-blacklisted globalKeys belonging to it (so a FUTURE layer registered
   * under the same id starts with a clean slate, never inheriting a stale
   * blacklist), and removes it from `_layers`. No-op if `id` is not registered.
   */
  removeLayer(id) {
    if (!this._layers.has(id)) return;
    for (const [globalKey, p] of this.pending) {
      if (p.layerId !== id) continue;
      p.controller.abort();
      this.pending.delete(globalKey);
      this.pendingBytes -= p.byteCost; // see constructor's own doc comment on pendingBytes
      this.cancelledCount += 1;
    }
    for (const [globalKey, entry] of this.resident) {
      if (entry.layerId === id) this._evictEntry(globalKey, entry);
    }
    const prefix = `${id} `; // globalKeyFor's own join -- see that function's doc comment
    for (const globalKey of this._failed.keys()) {
      if (globalKey.startsWith(prefix)) this._failed.delete(globalKey);
    }
    this._layers.delete(id);
  }

  /**
   * One "frame"/tick: ask every registered layer what it wants for `view`, merge
   * and priority-sort the result (`comparePriority`), cancel any in-flight request
   * that fell out of the new plan, admit (start a load for) anything newly wanted
   * that fits the hard byte budget -- walked in `compareAdmission`'s own, separately
   * sorted order (coarser levels first), evicting unwanted LRU entries to make room
   * where needed, deferring what still does not fit -- and run the eviction tripwire.
   * See this file's module docstring ("Round 4") and the admission loop's own inline
   * comments, below, for the full policy. Never awaits a `load()` promise -- results
   * land later via
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
        this.pendingBytes -= p.byteCost; // see constructor's own doc comment on pendingBytes
        this.cancelledCount += 1;
      }
    }

    // Round 4 follow-up (manager review of round 4's own admission fix): RECONCILE
    // every already-resident WANTED entry's stored `byteCost` against its latest
    // PLANNED cost, BEFORE this step's own admissions are evaluated. Without this, a
    // resident entry's `byteCost` is captured once, at admission (`_onLoaded`), and
    // never revisited -- `update()`'s own LRU-refresh branch, just below, used to
    // touch only `lastUsedStep`. That is a real hole in "the invariant", not a
    // hypothetical one: `GatewayImageryLayerAdapter.plan()` (round 4's own manifest
    // fix, `./gateway_imagery_layer.js`) charges the constructor's declared FALLBACK
    // estimate for a tile until `fetchManifest()` resolves, then the tile's real,
    // usually much larger, manifest-declared size for the SAME globalKey -- any tile
    // admitted during that window kept the stale, smaller estimate forever, so
    // `residentBytes` (and therefore the whole `residentBytes + pendingBytes <=
    // memoryBudgetBytes` invariant `update()`'s own admission check relies on) could
    // silently understate reality by an order of magnitude while every counter
    // (`softViolationCount` included) kept reading clean. Measured directly (this
    // task's own report): 20 tiles admitted at a 262,144-byte estimate (5,242,880
    // bytes accounted) whose manifest-true cost is 3,147,060 bytes each (62,941,200
    // bytes, 50% over a 41,943,040-byte budget) left `residentBytes` at 5,242,880 and
    // `softViolationCount` at 0 -- a hard budget a stale estimate can walk straight
    // through is not a hard budget.
    //
    // The fix: for every CURRENTLY WANTED request whose globalKey is resident, if the
    // freshly-planned `byteCost` differs from the entry's stored one, update the
    // stored value and adjust `residentBytes` by EXACTLY the signed delta (never
    // recomputed by re-summing `resident` -- same incremental discipline as
    // `pendingBytes`, see that field's own doc comment) -- this can only ever be
    // exact, never let `residentBytes` drift from the true sum over `resident`, by
    // construction (a plain `+=`/`-=` of the same delta applied to the one entry that
    // changed). `byteCostRevisionCount` (a running total, never reset) and
    // `byteCostRevisionBytes` (the net SIGNED delta, never reset -- an upward
    // revision adds a positive number, a downward one a negative number, so this can
    // itself be negative if downward revisions dominate over a run) record every
    // reconciliation, unconditionally, for the identical reason `deferredCount`/
    // `failedCount` are unconditional: a revision that is not counted is not a
    // revision this project accepts.
    //
    // `_evictIfNeeded()` (below) runs IMMEDIATELY after this pass, before this step's
    // own new admissions are considered -- an upward revision is the ONE way
    // `residentBytes` can rise WITHOUT a new admission going through the hard
    // admission check, so it is only fair that eviction gets first crack at absorbing
    // it before this step tries to admit anything else against a budget the revision
    // may have just broken. See `_evictIfNeeded`'s own doc comment for what this means
    // for that method's own "unreachable by construction" claim: it no longer is,
    // for this one path specifically.
    for (const r of allRequests) {
      const entry = this.resident.get(r.globalKey);
      if (entry && entry.byteCost !== r.byteCost) {
        const delta = r.byteCost - entry.byteCost;
        entry.byteCost = r.byteCost;
        this.residentBytes += delta;
        this.byteCostRevisionCount += 1;
        this.byteCostRevisionBytes += delta;
      }
    }
    this._evictIfNeeded(); // may now genuinely fire the soft-violation tripwire -- see that method's own doc comment

    // Round 4 (question 228) -- ADMISSION, walked in a SEPARATE order from
    // `allRequests`/`comparePriority` above: `update()` still RETURNS the plan sorted
    // by `comparePriority` unchanged (`tests/test_viewer_layers.py` pins that order),
    // but decides what to admit by `compareAdmission` (coarser levels first -- see
    // that comparator's own doc comment for why the two orders are deliberately
    // different). This is a fresh sorted COPY; `allRequests` itself, and therefore
    // this function's return value, is never reordered.
    this.lastStepDeferred = 0;
    const admissionOrder = allRequests.slice().sort(compareAdmission);
    for (const r of admissionOrder) {
      if (this.resident.has(r.globalKey)) {
        this.resident.get(r.globalKey).lastUsedStep = this._step;
        continue;
      }
      if (this.pending.has(r.globalKey)) continue;
      if (this._failed.has(r.globalKey)) continue; // blacklisted while still wanted -- see constructor's failure-memory policy
      // `maxConcurrentLoads` is checked BEFORE the budget on purpose: a request held
      // back solely by the concurrency cap is not a budget deferral (constructor's
      // own doc comment on `deferredCount`), so when the cap alone already rules this
      // request out this step, skip straight to the next one without touching the
      // budget/eviction machinery below at all -- nothing here should be attributed
      // to the budget when the budget was never even consulted.
      if (this.pending.size >= this.maxConcurrentLoads) continue; // deferred to a later update(), NOT counted in deferredCount

      // THE INVARIANT (round 4, question 228's decision, restated precisely): this
      // manager maintains `residentBytes + pendingBytes <= memoryBudgetBytes` at
      // every point in time THAT A NEW ADMISSION IS WHAT CHANGED IT. A request is
      // admitted (moved into `pending`) ONLY when `residentBytes + pendingBytes +
      // r.byteCost <= memoryBudgetBytes` continues to hold afterward -- `pendingBytes`
      // is reserved capacity for this exact load, so the invariant survives the async
      // gap between "admitted here" and "settled in `_onLoaded`/`_onFailed`" (a
      // second load admitted before the first settles cannot together overshoot the
      // budget, because each admission re-checks the running total, not just
      // `residentBytes` alone). Because `residentBytes <= residentBytes +
      // pendingBytes` always (byte costs are never negative), maintaining THIS
      // invariant is strictly stronger than merely `residentBytes <=
      // memoryBudgetBytes` -- which is exactly `_evictIfNeeded`'s own while-condition
      // (below) -- for every admission this loop performs.
      //
      // Round 4 FOLLOW-UP (manager review): admission is not the only way
      // `residentBytes` can move. The reconciliation pass just above this loop (see
      // its own comment) is the ONE OTHER way it can rise -- an already-resident,
      // still-wanted entry's declared cost can legitimately change after the fact
      // (`GatewayImageryLayerAdapter.fetchManifest()` resolving being the concrete
      // case this codebase actually has). That pass runs its own `_evictIfNeeded()`
      // immediately, BEFORE this loop, to try to absorb any resulting overage against
      // whatever is currently unwanted -- but if every resident byte belongs to the
      // CURRENT wanted set and the revised total still exceeds budget, there is
      // nothing evictable, and `_evictIfNeeded`'s soft-violation `break` fires
      // legitimately. So: `_evictIfNeeded`'s tripwire is unreachable by construction
      // for anything THIS loop (admission) does, but NOT unreachable overall -- a
      // byteCost revision can genuinely and correctly trigger it, which is the
      // intended behaviour (an honest report of an unavoidable overage), not a
      // regression of the admission invariant above. `softViolationCount` and
      // `byteCostRevisionCount`/`byteCostRevisionBytes` together tell a reader which
      // of the two happened: admission working exactly as designed (the former stays
      // 0), or a cost revision this manager could not fully absorb (the former
      // becomes nonzero, alongside a nonzero revision count for the same step).
      if (this.residentBytes + this.pendingBytes + r.byteCost > this.memoryBudgetBytes) {
        // Try to make room: evict least-recently-used UNWANTED resident entries
        // (never anything in `this._wantedKeys`, the set this exact `update()` just
        // computed) until this request fits, or until nothing evictable remains.
        // Without this, a hard admission limit alone deadlocks the viewer the moment
        // the camera moves: the budget stays entirely full of the OLD view's tiles
        // (now unwanted, since the camera moved) and nothing new could ever be
        // admitted -- the viewer would freeze on a stale image forever. This reuses
        // `_selectEvictionVictim()`, the exact same LRU rule `_evictIfNeeded` itself
        // uses (see that method's own doc comment) -- one victim-choice
        // implementation, not two independently-written copies of the same policy.
        while (this.residentBytes + this.pendingBytes + r.byteCost > this.memoryBudgetBytes) {
          const victim = this._selectEvictionVictim();
          if (victim === null) break; // nothing left this manager is willing to evict
          this._evictEntry(victim.globalKey, victim.entry);
        }
      }
      if (this.residentBytes + this.pendingBytes + r.byteCost > this.memoryBudgetBytes) {
        // Still does not fit even after evicting everything evictable: defer THIS
        // request and move on -- deliberately `continue`, never `break`. A later
        // request in admission order can be smaller (a different layer's byteCost) or
        // coarser (already fits because nothing needs to be evicted for it, or it was
        // the very eviction target above's own sibling at a coarser level) -- `break`
        // here would let one stubborn request starve every request behind it in
        // admission order for the rest of this step, exactly the "sorted list, not a
        // queue" defect deliverable 1 (H5a) already fixed for `maxConcurrentLoads`,
        // reintroduced one level up for the budget.
        this.deferredCount += 1;
        this.lastStepDeferred += 1;
        continue;
      }

      const controller = new AbortController();
      this.pending.set(r.globalKey, {
        layerId: r.layerId, localKey: r.key, byteCost: r.byteCost, controller, startedStep: this._step,
      });
      this.pendingBytes += r.byteCost; // see constructor's own doc comment on pendingBytes
      const layer = this._layers.get(r.layerId);
      layer.load(r, controller.signal).then(
        (payload) => this._onLoaded(r, controller, payload),
        (err) => this._onFailed(r.globalKey, controller, err),
      );
    }

    this._evictIfNeeded(); // tripwire only -- see this method's own comment above and _evictIfNeeded's doc comment
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
    this.pendingBytes -= p.byteCost; // see constructor's own doc comment on pendingBytes -- reserved capacity released, now spent as residentBytes below
    this.resident.set(request.globalKey, {
      layerId: request.layerId,
      localKey: request.key,
      byteCost: request.byteCost,
      lastUsedStep: this._step,
      payload,
      loadedAt: String(this._now()), // design constraint h: a string, never a number
    });
    this.residentBytes += request.byteCost;
    this._evictIfNeeded(); // tripwire only -- see update()'s own comment on the invariant this cannot violate
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
    this.pendingBytes -= p.byteCost; // see constructor's own doc comment on pendingBytes -- reserved capacity released, never spent
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

  /** Choose the LRU eviction victim -- the resident entry with the smallest
   * `lastUsedStep` among everything NOT in `this._wantedKeys` (the CURRENT wanted
   * set), ties broken by ascending `globalKey` so the choice never depends on `Map`
   * iteration order (design constraint d's "total and deterministic" discipline,
   * applied here too). Returns `null` if every resident entry is protected (nothing
   * evictable at all).
   *
   * Factored out (round 4, question 228) so `_evictIfNeeded` (the post-hoc tripwire,
   * below) and `update()`'s own make-room-for-admission step (the round-4 addition
   * that evicts BEFORE admitting a specific request that would not otherwise fit)
   * share exactly ONE victim-selection rule -- never a second, independently-written
   * copy of the same LRU policy. Read-only: callers are responsible for actually
   * removing the returned victim (`_evictEntry`, below) and for re-checking whatever
   * condition they are looping on afterward.
   * @returns {{globalKey: string, entry: {layerId:string, localKey:string, byteCost:number, lastUsedStep:number}} | null}
   */
  _selectEvictionVictim() {
    const protectedKeys = this._wantedKeys;
    let victimKey = null;
    let victim = null;
    for (const [globalKey, entry] of this.resident) {
      if (protectedKeys.has(globalKey)) continue;
      if (
        victim === null
        || entry.lastUsedStep < victim.lastUsedStep
        || (entry.lastUsedStep === victim.lastUsedStep && globalKey < victimKey)
      ) {
        victim = entry;
        victimKey = globalKey;
      }
    }
    return victimKey === null ? null : { globalKey: victimKey, entry: victim };
  }

  /** Actually evict one resident entry chosen by `_selectEvictionVictim` (or any
   * caller that already has a `{globalKey, entry}` pair it has decided to evict):
   * removes it from `resident`, debits `residentBytes`, calls the owning layer's
   * `release(localKey)`, and increments `evictedCount`. The one and only place a
   * resident entry is ever removed for being evicted (both `_evictIfNeeded` and
   * `update()`'s make-room-for-admission step call this, never duplicate its body). */
  _evictEntry(globalKey, entry) {
    this.resident.delete(globalKey);
    this.residentBytes -= entry.byteCost;
    const layer = this._layers.get(entry.layerId);
    if (layer) layer.release(entry.localKey);
    this.evictedCount += 1;
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
   * fixes.
   *
   * **Round 4 (question 228): this method's own while-loop condition,
   * `residentBytes > memoryBudgetBytes`, is UNREACHABLE BY CONSTRUCTION for anything
   * `update()`'s ADMISSION step does** (see that method's own comment on "THE
   * INVARIANT") -- it only ever admits a request while `residentBytes + pendingBytes
   * + byteCost <= memoryBudgetBytes` continues to hold, evicting UNWANTED entries
   * first to make room where needed.
   *
   * **It is NOT unreachable overall, and an earlier version of this comment
   * overclaimed that it was** -- a manager review of round 4's own admission fix
   * found the actual gap directly: a resident entry's `byteCost` was captured once,
   * at admission (`_onLoaded`), and never revisited, so a layer whose declared cost
   * for an already-resident key changed LATER (`GatewayImageryLayerAdapter`'s own
   * `fetchManifest()` resolving after some tiles were already admitted at its
   * fallback estimate is the concrete case this codebase actually has, not a
   * hypothetical one -- see `./gateway_imagery_layer.js`'s own module docstring)
   * could silently push `residentBytes` above budget while every counter, this one
   * included, kept reading clean (measured: 20 tiles at a 262,144-byte estimate,
   * 5,242,880 bytes accounted, true manifest cost 3,147,060 bytes each --
   * 62,941,200 bytes, 50% over a 41,943,040-byte budget -- `softViolationCount`
   * stayed 0 throughout). The fix is `update()`'s own reconciliation pass
   * (immediately before this method's own first call each step -- see that pass's
   * comment for the full policy and `byteCostRevisionCount`/`byteCostRevisionBytes`,
   * the constructor): it is the ONE remaining way `residentBytes` can rise WITHOUT
   * going through the admission check above, and it is exactly why this method (and
   * `softViolationCount`) cannot be reduced to "a tripwire that never fires" --
   * a revision that pushes an already-resident, STILL-WANTED entry's true cost up
   * with nothing unwanted left to evict for it is a genuine, unavoidable violation
   * (this manager correctly refuses to evict something the view still wants to make
   * an impossible budget possible), and this method's own `break` firing for that
   * reason is the CORRECT, honest outcome, not a bug. That branch is never silent:
   * `softViolationCount` (see the constructor) is incremented every time it fires,
   * specifically so a harness that only checks `residentBytes <= memoryBudgetBytes`
   * after the fact cannot be fooled by a run where the check passed merely because
   * nothing was ever big enough to trigger the soft-violation path -- report
   * `softViolationCount` alongside any budget assertion (see
   * `web/js/layers_check.mjs`, `web/js/layers_stream_check.mjs` and
   * `web/js/layers_budget_check.mjs`, whose own phase 1/2 assert it is exactly 0
   * -- provable there, because nothing in those phases ever revises a byteCost --
   * and whose phase 3 asserts the OPPOSITE, that it becomes nonzero, specifically
   * when a revision makes that the honest answer).
   */
  _evictIfNeeded() {
    while (this.residentBytes > this.memoryBudgetBytes) {
      const victim = this._selectEvictionVictim();
      if (victim === null) {
        // Everything resident is protected; budget soft-violated this step -- see
        // this method's own doc comment. Counted, never silent. Unreachable under the
        // round-4 invariant; kept as the tripwire that proves it.
        this.softViolationCount += 1;
        break;
      }
      this._evictEntry(victim.globalKey, victim.entry);
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

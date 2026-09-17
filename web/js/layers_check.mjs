// CLI harness for tests/test_viewer_layers.py: `node web/js/layers_check.mjs`.
//
// Drives the real web/js/layers/ module (LayerManager + the three real adapters:
// ImageryLayerAdapter, TerrainLayerAdapter, Tiles3DLayerAdapter) over a fixed,
// scripted camera path against this project's own real inputs -- a real quadtree
// selection near a real geodetic point (web/js/globe_lod.js's `selectTiles`) and this
// project's own real 3D Tiles fixture (web/fixtures/3dtiles/tileset.json, parsed by
// web/js/tiles_layer.js's real `parseTileset3D`/`selectTiles3D`) -- and prints one
// JSON object of the per-step priority-ordered plan plus the manager's byte-budget/
// cancellation/eviction counters. Same "run the real code, don't port it" discipline
// as web/js/globe_lod_check.mjs/web/js/tiles3d_check.mjs: this file only supplies
// camera data and injected loader stubs, and glues the real exports together --
// nothing here reimplements LayerManager's priority/budget/cancellation arithmetic or
// selectTiles()/selectTiles3D()'s screen-space-error arithmetic.
//
// Question 51 (offline viewer, "structurally incapable of network I/O"): every
// loader this harness injects is a closed-over in-memory queue with no `fetch`/XHR/
// filesystem access of any kind -- see `makeImageryLoaderStub`/`makeTiles3DLoaderStub`
// below, the same discipline web/js/globe_imagery_check.mjs's `makeRecordingLoader`
// already documents for the globe's own imagery loader.
//
// Design constraint g ("no clocks slept, ever"): nothing here reads a real clock for
// anything that affects the printed JSON. `LayerManager`'s own LRU ordering is
// step-counted (`_step`), not time-based; the one place a `now()` is threaded through
// (`LayerManager`'s constructor option, stored per-resident-item as `loadedAt`, a
// *string* per design constraint h) is given a deterministic counter here, not
// `performance.now()`, specifically so re-running this script never depends on how
// fast the host machine happens to be.
//
// How loads actually "complete" without a real clock or a real network: each stub
// loader queues every `load()` call's resolve/reject callbacks (FIFO -- the same
// "oldest pending first" completion order `globe_lod_check.mjs`'s
// `scheduler.completeLoads(n, ...)` already uses) and this harness explicitly
// completes a bounded number of the oldest per step (`completeOldest`), then awaits
// one microtask-queue flush (`flushMicrotasks`, a `setImmediate` round-trip -- not a
// timer-*duration* wait, just a deterministic "let every already-queued Promise
// callback run" barrier) so `LayerManager`'s real `.then()`-driven bookkeeping
// (`_onLoaded`/eviction) has settled before the next step reads `residentBytes`. This
// leaves a realistic backlog of still-pending requests across steps (some loads
// finish, most don't, every step), which is what makes the jump step's cancellation
// (see CAMERA_PATH below) genuine rather than a no-op against an empty queue --
// exactly `globe_lod_check.mjs`'s own reasoning for its own `completeLoads(8, ...)`.
//
// What this proves, and what a wrong implementation would fail against (see also
// tests/test_viewer_layers.py, which asserts on this JSON):
//   - Running this script twice (two independent `node` process invocations) and
//     diffing stdout byte-for-byte catches any implementation whose priority order
//     depends on Map/Set insertion accidents, object property enumeration, or a
//     comparator that is not total -- LayerManager.update() sorts its merged plan
//     explicitly (comparePriority, web/js/layers/layer.js) specifically so this never
//     happens.
//   - `maxResidentBytesObserved <= memoryBudgetBytes` (`budgetRespected`), with
//     `evictedCount > 0` asserted directly, catches a manager with no eviction, or one
//     that evicts by item *count* instead of by *byte total* (the actual H5 defect
//     this module exists to fix -- see web/js/layers/layer.js's module docstring on
//     why globe_lod.js's TileLoadScheduler's count-based budget is not a byte
//     budget). MEMORY_BUDGET_BYTES below was RE-measured for this round (not
//     estimated, and not merely carried over): deliverable 1's fix (layer.js's
//     failure-memory policy, see that file's constructor doc comment) let
//     MAIN_MAX_CONCURRENT_LOADS go back to the class's own real default (6 -- see
//     that constant's own comment below for why the previous round's inflated value
//     of 24 was actually masking Finding 1's starvation defect, not modelling
//     anything real), and a smaller concurrency cap changes how much this path's own
//     working set can be simultaneously resident, so the budget was re-derived
//     against THAT number, not reused from the round-2 measurement. First this exact
//     harness was run with an effectively unlimited budget (999 GB) to measure the
//     path's true cumulative distinct-byte total with eviction disabled --
//     31,981,568 bytes (23 distinct items: 12 imagery tiles + 11 3D-tile requests
//     that ever completed; `steps[*].residentBytes` climbs monotonically from
//     786,432 at 'far' to that ceiling by 'far-2') -- then MEMORY_BUDGET_BYTES =
//     14,000,000 was picked, comfortably below that ceiling (44%) but above the
//     'close-sw' step's own unthrottled resident total (7,340,032), so eviction
//     must run partway through the path for `budgetRespected` to hold, and cannot
//     simply be satisfied by the budget never being approached at all -- the same
//     design `globe_lod_check.mjs`'s RESIDENT_BUDGET documents for its own
//     tile-count budget, with the actual measured numbers recorded here (not
//     estimated) so a later edit to the camera path or completion schedule cannot
//     silently drift the budget into "never binds" territory without this comment
//     visibly going stale. `softViolationCount` (see layer.js's `_evictIfNeeded` doc
//     comment on why "protect everything currently wanted" makes the budget
//     soft-violable) is asserted to be exactly 0 for this run by
//     tests/test_viewer_layers.py -- a budget picked so tight that even the
//     currently-wanted set alone cannot fit under it would make `budgetRespected`
//     true for the wrong reason (nothing left to protect, not real eviction), so a
//     nonzero `softViolationCount` here would mean the chosen budget's margin is not
//     what this comment claims.
//   - `cancelledCount > 0` catches a manager that never cancels a stale in-flight
//     load: the 'close-sw-2' -> 'jump-ne' step is a deliberate jump from one corner
//     of the 3D Tiles fixture's small (~4.4 km) footprint to the diagonally-opposite
//     corner (see web/js/tiles3d_check.mjs's own module docstring for the identical
//     fixture-geometry reasoning), inserted before every pending load from the
//     previous step has completed (`completeOldest` only ever completes a bounded
//     number per step, see above) -- an implementation that queues every requested
//     load and never cancels a stale one would report `cancelledCount === 0`. The
//     'jump-ne' step's own `cancelledThisStep` is asserted `> 0` directly (not merely
//     the run's cumulative total), so a manager that only ever cancels at some OTHER
//     step (by accident of this particular schedule) cannot pass by coincidence.
//   - `terrainRefusalCount > 0`, with `terrainRefusalName` pinned to the exact typed
//     error name, catches a terrain adapter that silently fabricates a fake resolved
//     payload instead of a named, typed refusal (design constraint a's binding rule
//     for a not-yet-implemented loader) -- see web/js/layers/terrain_layer.js's
//     module docstring.
//   - `planNeverLoadsDirectly` catches an adapter whose `plan()` itself starts a
//     load (eagerly fetching instead of only declaring demand) -- see
//     `probePlanNeverLoadsDirectly` below: it calls `plan()` alone, with no
//     `LayerManager` involved at all, and asserts none of the injected loaders'
//     invocation counters moved.
//   - `perLayerResidentCounts` having a nonzero count for both 'imagery' and
//     'tiles3d' (but always 0 for 'terrain', which never resolves) demonstrates the
//     three layers really do share one manager, one budget and one resident map --
//     `LayerManager.getResidentPayload(globalKey)` (exercised directly below,
//     `residentPayloadLookupWorks`) is the concrete "no caller outside
//     web/js/layers/ has to know which of the three it is talking to" proof: one
//     lookup call, keyed only by the globalKey the ordered plan already printed,
//     works identically regardless of which layer produced the entry.
//   - `queueIsLoadBearing.ok` (see `probeQueueIsLoadBearing` below) catches deliverable
//     1's own regression risk directly: a manager that still starts every wanted
//     request regardless of `maxConcurrentLoads` (the priority order computed but
//     never actually enforced -- the exact "sorted list, not a queue" defect a
//     manager review flagged on H5a) would fail `deferredKeyNotStartedFirstUpdate`;
//     one that drops a deferred request forever instead of starting it once a slot
//     frees would fail `deferredKeyStartedSecondUpdateAfterSlotsFreed`.
//   - `starvationProbe.ok` (see `probeStarvationDoesNotBlockGoodLayer` below) catches
//     Finding 1's starvation defect directly: a `_onFailed` with no memory of a
//     failed globalKey re-plans the same permanently-failing request every single
//     `update()`, so it takes a concurrent-load slot, fails again, and repeats
//     forever -- when it sorts ahead of everything else in priority order it alone
//     can consume every slot, every step, and a well-behaved lower-priority layer
//     never gets to load anything at all (`goodResident` stuck at 0). `failedCount`/
//     `failureNames` (both reported unconditionally here and in
//     `web/js/layers_stream_check.mjs`) make the failure-memory policy itself
//     observable, not just this one probe's pass/fail.
import { LayerManager } from './layers/layer.js';
import { ImageryLayerAdapter, IMAGERY_TILE_BYTES } from './layers/imagery_layer.js';
import { TerrainLayerAdapter, TerrainLoaderNotImplementedError } from './layers/terrain_layer.js';
import { Tiles3DLayerAdapter, DEFAULT_TILE3D_BYTES } from './layers/tiles3d_layer.js';
import { selectTiles, tileKey } from './globe_lod.js';
import { selectTiles3D, parseTileset3D, ecefFromRootTransform, enuBasisFromRootTransform } from './tiles_layer.js';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TILESET_PATH = path.join(__dirname, '..', 'fixtures', '3dtiles', 'tileset.json');
const tilesetJson = JSON.parse(fs.readFileSync(TILESET_PATH, 'utf8'));
const tree = parseTileset3D(tilesetJson);

// Deterministic "clock" (design constraint g): a plain incrementing counter, never a
// real time source -- see this file's module docstring.
let fakeNowCounter = 0;
function fakeNow() { fakeNowCounter += 1; return fakeNowCounter; }

/** Waits for every already-queued microtask (Promise `.then` callback) to run, with
 * no timer *duration* involved -- a `setImmediate` round-trip runs after Node's
 * event loop has fully drained the microtask queue for this turn, so this is a
 * deterministic ordering barrier, not a clock wait (design constraint g). */
function flushMicrotasks() {
  return new Promise((resolve) => { setImmediate(resolve); });
}

// ------------------------------------------------------------------ injected loaders
// THREE.TextureLoader-shaped stub (`.load(url, onLoad, onProgress, onError)`, no
// `signal` parameter -- matching the real shape web/js/globe.js's GlobeLayer accepts,
// and exactly why ImageryLayerAdapter.load() itself is what makes cancellation
// observable here, not this loader -- see that adapter's module docstring). No I/O of
// any kind (question 51): every "load" is just an in-memory FIFO entry.
function makeImageryLoaderStub() {
  const inflight = [];
  let invocationCount = 0;
  return {
    invocationCount: () => invocationCount,
    load(url, onLoad, _onProgress, _onError) {
      invocationCount += 1;
      inflight.push({ url, onLoad });
    },
    completeOldest(n) {
      const batch = inflight.splice(0, Math.max(0, n));
      for (const entry of batch) entry.onLoad({ url: entry.url, kind: 'imagery-texture-stub' });
      return batch.length;
    },
    pendingCount: () => inflight.length,
  };
}

// Tiles3DLayerAdapter-shaped stub (`loader(request, signal): Promise`). Unlike the
// imagery stub, this one *does* receive `signal`, so it can (and does) drop a
// cancelled entry from its own FIFO -- realistic for a fetch-based loader, which
// really can abort an in-flight request, unlike THREE.TextureLoader's XHR path (see
// imagery_layer.js's module docstring for that documented asymmetry). No I/O of any
// kind (question 51): every "load" is an in-memory FIFO entry, resolved/rejected by
// this harness only.
function makeTiles3DLoaderStub() {
  const inflight = [];
  let invocationCount = 0;
  function loader(request, signal) {
    invocationCount += 1;
    return new Promise((resolve, reject) => {
      if (signal.aborted) { reject(signal.reason); return; }
      const entry = { request, resolve };
      inflight.push(entry);
      signal.addEventListener('abort', () => {
        const idx = inflight.indexOf(entry);
        if (idx >= 0) inflight.splice(idx, 1);
        reject(signal.reason);
      }, { once: true });
    });
  }
  return {
    loader,
    invocationCount: () => invocationCount,
    completeOldest(n) {
      const batch = inflight.splice(0, Math.max(0, n));
      for (const entry of batch) entry.resolve({ contentUri: entry.request.contentUri, kind: 'tiles3d-content-stub' });
      return batch.length;
    },
    pendingCount: () => inflight.length,
  };
}

// -------------------------------------------------------------------- camera path
// Geo-referenced ENU offsets from the fixture's own real anchor (read from
// tileset.json's root.transform, not a second hardcoded location -- identical
// technique to web/js/tiles3d_check.mjs, so this harness automatically tracks
// whatever the fixture actually declares). far (root-only 3D tile, coarse imagery) ->
// descending -> a close pass over the SW-ish corner (deep 3D-tile refinement,
// mirrored twice so a realistic pending backlog survives into the jump) -> a
// deliberate jump to the diagonally-opposite (NE-ish) corner (mostly-disjoint 3D-tile
// set, exercising real cancellation) -> pull back out to far again.
const anchorEcef = ecefFromRootTransform(tree.rootTransform);
const basis = enuBasisFromRootTransform(tree.rootTransform);
function cameraEcefFromEnu(eastM, northM, upM) {
  return {
    x: anchorEcef.x + basis.east.x * eastM + basis.north.x * northM + basis.up.x * upM,
    y: anchorEcef.y + basis.east.y * eastM + basis.north.y * northM + basis.up.y * upM,
    z: anchorEcef.z + basis.east.z * eastM + basis.north.z * northM + basis.up.z * upM,
  };
}

const CAMERA_PATH = [
  { label: 'far', eastM: 0, northM: 0, upM: 3000000 },
  { label: 'descending', eastM: 0, northM: 0, upM: 200000 },
  { label: 'close-sw', eastM: -2200, northM: -2200, upM: 4000 },
  { label: 'close-sw-2', eastM: -2100, northM: -2100, upM: 4000 },
  { label: 'jump-ne', eastM: 2200, northM: 2200, upM: 4000 },   // deliberate large jump
  { label: 'jump-ne-2', eastM: 2100, northM: 2100, upM: 4000 },
  { label: 'far-2', eastM: 0, northM: 0, upM: 3000000 },
];

const IMG_SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 24, maxLevel: 4, maxTiles: 60 };
const T3D_SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 16, maxLevel: 12, maxTiles: 512 };

// See this file's module docstring for exactly how this value was chosen (strictly
// between one step's own working set and the path's cumulative distinct-byte total).
const MEMORY_BUDGET_BYTES = 14_000_000;

// Bounded per-step completion count (see module docstring: "oldest pending first",
// same technique as globe_lod_check.mjs's completeLoads(8, ...)) -- deliberately well
// below the number of new requests a close/jump step introduces, so a real backlog of
// still-pending requests always survives into the next step.
const COMPLETE_PER_STEP = 3;

// One update() tick per labelled camera position.
const TICKS_PER_POSITION = 1;

// This harness's own LayerManager (below) is built with LayerManager's own real
// default, `maxConcurrentLoads` = 6 (the real per-origin HTTP/1.1 browser connection
// cap -- see layer.js's constructor doc). A previous round of this harness raised
// this to 24 specifically to paper over the "terrain zombie" livelock described
// below -- which was really Finding 1's starvation defect wearing a second costume,
// and hiding a real defect behind a harness knob is exactly the class of failure
// this project calls "a failure that leaves no trace" (this task's own binding
// instructions). With Finding 1's fix in place (layer.js's failure-memory policy,
// see that file's constructor doc comment) the default cap is used for real:
//
// What the livelock WAS, before the fix: `TerrainLayerAdapter.plan()` re-declares
// the same demand every tick (design: a disclosed gap, never a silent stub, see
// terrain_layer.js), tied in screen-space error with its imagery sibling for the
// SAME tile, and `comparePriority` breaks that tie in imagery's favour
// (`'imagery' < 'terrain'` sorts first). Once N tiles' imagery had loaded, each
// one's imagery request dropped out of slot competition (already resident) but its
// terrain twin never did (never succeeded, so it was replanned -- and, with no
// failure memory, RE-ATTEMPTED -- on literally every subsequent `update()`),
// tying the very same top sse rank its now-resident imagery sibling held. Once N
// reached `maxConcurrentLoads`, those N permanently-retried terrain zombies alone
// filled every slot every tick forever.
//
// Why it is gone now: the first time each distinct terrain key fails, it is
// blacklisted (layer.js's `_failed`) for as long as it stays wanted -- so a terrain
// tile that keeps being "wanted" (never resolves) is asked for exactly ONCE, not
// once per tick, and stops competing for slots at all after that. `failedCount`/
// `terrainRefusalCount` (below) are bounded by the number of DISTINCT terrain keys
// this camera path ever wants, not by tick count -- see this file's own printed
// numbers for the measured total.
const MAIN_MAX_CONCURRENT_LOADS = 6;

// ---------------------------------------------------------------------- build layers
const imageryLoaderStub = makeImageryLoaderStub();
const tiles3dLoaderStub = makeTiles3DLoaderStub();

const imageryLayer = new ImageryLayerAdapter({ imageryUrl: './fixtures/tiles/{z}/{x}/{y}.png', loader: imageryLoaderStub });
const terrainLayer = new TerrainLayerAdapter();
const tiles3dLayer = new Tiles3DLayerAdapter({ tree, loader: tiles3dLoaderStub.loader });

// Wrap the terrain layer's load() to count refusals and pin the exact error name,
// without changing its cancellation/promise semantics at all (re-throws the same
// rejection it received) -- see this file's module docstring.
let terrainRefusalCount = 0;
let terrainRefusalName = null;
const _terrainLoad = terrainLayer.load.bind(terrainLayer);
terrainLayer.load = (request, signal) => _terrainLoad(request, signal).catch((err) => {
  terrainRefusalCount += 1;
  terrainRefusalName = err && err.name;
  throw err;
});

// maxConcurrentLoads: MAIN_MAX_CONCURRENT_LOADS, which is the class's own real
// default (6) -- see that constant's own comment above.
const manager = new LayerManager({
  memoryBudgetBytes: MEMORY_BUDGET_BYTES, now: fakeNow, maxConcurrentLoads: MAIN_MAX_CONCURRENT_LOADS,
});
manager.addLayer(imageryLayer);
manager.addLayer(terrainLayer);
manager.addLayer(tiles3dLayer);

/** Design constraint / house-style requirement: "a test that would fail if a caller
 * reached a loader directly" -- calls every adapter's `plan()` in isolation (no
 * `LayerManager` involved) and asserts none of the injected loaders' own invocation
 * counters moved. A `plan()` that eagerly loaded instead of only declaring demand
 * (breaking the entire point of `LayerManager` owning the priority queue/budget/
 * cancellation for *all* layers) would fail this immediately. */
function probePlanNeverLoadsDirectly() {
  const before = { imagery: imageryLoaderStub.invocationCount(), tiles3d: tiles3dLoaderStub.invocationCount() };
  const probeView = {
    cameraEcef: cameraEcefFromEnu(0, 0, 200000),
    screenHeightPx: IMG_SCREEN.screenHeightPx,
    fovYRad: IMG_SCREEN.fovYRad,
    tiles: selectTiles(cameraEcefFromEnu(0, 0, 200000), IMG_SCREEN),
    sseThreshold: T3D_SCREEN.sseThreshold,
    maxLevel: T3D_SCREEN.maxLevel,
    maxTiles: T3D_SCREEN.maxTiles,
  };
  imageryLayer.plan(probeView);
  terrainLayer.plan(probeView);
  tiles3dLayer.plan(probeView);
  const after = { imagery: imageryLoaderStub.invocationCount(), tiles3d: tiles3dLoaderStub.invocationCount() };
  return before.imagery === after.imagery && before.tiles3d === after.tiles3d;
}

const planNeverLoadsDirectly = probePlanNeverLoadsDirectly();

/** Manager review finding on H5a, deliverable 1: "the priority queue is a sorted
 * list, not a queue, because nothing is ever deferred". Proves `maxConcurrentLoads`
 * is load-bearing, on a fresh manager/stub isolated from the main run below (so this
 * probe's outcome cannot be perturbed by anything the main camera path does, and
 * vice versa):
 *   1. A view whose plan has MORE requests than `maxConcurrentLoads` (the 'far'
 *      camera position selects 14 imagery tiles, see globe_lod.js's selectTiles --
 *      far more than the 2-slot cap this probe uses) is run through one `update()`.
 *      The set of requests actually started (`pending`'s keys) must equal exactly
 *      the top-`maxConcurrentLoads` of the plan's own priority order -- proving
 *      "starts loads in priority order only while pending.size < maxConcurrentLoads"
 *      is really what happens, not merely that *some* subset started.
 *   2. A specific lower-priority request (the plan's 3rd-ranked, `plan[2]`) must be
 *      observably NOT started (absent from `pending`) while both slots are full --
 *      this is the half a wrong implementation that starts every wanted request
 *      regardless of the cap would fail (see this file's module docstring: only
 *      proving "started later" without also proving "not started while full" cannot
 *      tell a real queue from a manager that starts everything immediately and this
 *      probe just happened to look at it before completion).
 *   3. Completing both in-flight loads frees two slots; a SECOND `update()` over the
 *      SAME view must then start that same previously-deferred request -- proving
 *      slots really free up and the queue really drains, in priority order, rather
 *      than a request that missed its first window being dropped forever.
 */
async function probeQueueIsLoadBearing() {
  const CAP = 2;
  const stub = makeImageryLoaderStub();
  const layer = new ImageryLayerAdapter({ imageryUrl: './fixtures/tiles/{z}/{x}/{y}.png', loader: stub });
  const mgr = new LayerManager({ memoryBudgetBytes: 999_000_000_000, maxConcurrentLoads: CAP, now: fakeNow });
  mgr.addLayer(layer);
  const cameraEcef = cameraEcefFromEnu(0, 0, 3000000); // 'far': 14 imagery tiles, see selectTiles
  const view = { cameraEcef, screenHeightPx: IMG_SCREEN.screenHeightPx, fovYRad: IMG_SCREEN.fovYRad, tiles: selectTiles(cameraEcef, IMG_SCREEN) };

  const plan1 = mgr.update(view); // already priority-sorted
  const expectedFirstStarted = new Set(plan1.slice(0, CAP).map((r) => r.globalKey));
  const startedFirstUpdate = new Set(mgr.pending.keys());
  const startedFirstUpdateMatchesTopN = plan1.length > CAP
    && startedFirstUpdate.size === CAP
    && [...startedFirstUpdate].every((k) => expectedFirstStarted.has(k));

  const deferredKey = plan1[CAP].globalKey; // the 3rd-ranked request: rank > CAP, must not have started
  const deferredKeyNotStartedFirstUpdate = !mgr.pending.has(deferredKey) && !mgr.resident.has(deferredKey);

  stub.completeOldest(CAP);
  await flushMicrotasks();

  const plan2 = mgr.update(view); // same view: the deferred request is still wanted
  const deferredKeyStartedSecondUpdateAfterSlotsFreed = mgr.pending.has(deferredKey);

  return {
    maxConcurrentLoads: CAP,
    planLength: plan1.length,
    startedFirstUpdateCount: startedFirstUpdate.size,
    startedFirstUpdateMatchesTopN,
    deferredKey,
    deferredKeyNotStartedFirstUpdate,
    deferredKeyStartedSecondUpdateAfterSlotsFreed,
    ok: plan1.length > CAP && startedFirstUpdateMatchesTopN && deferredKeyNotStartedFirstUpdate
      && deferredKeyStartedSecondUpdateAfterSlotsFreed,
  };
}

const queueIsLoadBearing = await probeQueueIsLoadBearing();

/** Named, typed rejection this probe's 'bad' layer always fails with -- so a
 * `failureNames`/`failedCount` assertion has something specific to pin, the same
 * "typed, named, never a generic Error" discipline `TerrainLoaderNotImplementedError`
 * already follows (see layers/terrain_layer.js). */
class AlwaysFailsError extends Error {
  constructor(key) {
    super(`probeStarvationDoesNotBlockGoodLayer: layer 'bad' always fails to load '${key}'`);
    this.name = 'AlwaysFailsError';
  }
}

/** Finding 1 (starvation defect, corrective round 3, manager-root-caused): before
 * this round's fix, `_onFailed` had NO memory of a failed globalKey, so `update()`
 * replanned the exact same permanently-failing request on literally every step, it
 * took a concurrent-load slot, failed again, and repeated forever -- when a
 * permanently-failing request sorts ahead of everything else in priority order
 * (`comparePriority`), it alone can consume every slot, every step, starving every
 * lower-priority request behind it for as long as it stays wanted.
 *
 * This probe reproduces the manager's own shape directly: two layers, `bad` (id
 * sorts before `good` lexically, and `load()` always rejects with the named
 * `AlwaysFailsError` above) and `good` (`load()` always resolves), TEN requests
 * each, every one of the twenty given the EXACT SAME `sseError`/`viewDistanceM` --
 * so every comparison ties and is decided purely by ascending globalKey
 * (`comparePriority`'s own documented tie-break), and `'bad\0t*' < 'good\0t*'` for
 * every index -- meaning all ten `bad` requests occupy the entire top of the
 * priority order, ahead of every single `good` request. `maxConcurrentLoads: 6`
 * (the class's own real default -- see the constructor's own doc comment), 30
 * `update()` steps, every settled promise drained (`flushMicrotasks`) between
 * steps -- the exact same probe shape as `probeQueueIsLoadBearing` above, on its own
 * fresh manager/layers, isolated from the main camera-path run below.
 *
 * BEFORE the fix: nothing remembers a failed key, so all 6 slots are filled by `bad`
 * requests on literally every step (ten always-outranking `bad` requests never run
 * out of turns to be replanned) -- `good` never gets a single slot across all 30
 * steps (`goodResident` stays 0), and `badLoadAttempts` grows by up to 6 every step
 * (measured against today's unfixed layer.js: run this file and see `starvationProbe`
 * in the printed JSON).
 *
 * AFTER the fix: each of the 10 distinct `bad` keys is blacklisted the first time it
 * fails (layer.js's constructor doc comment states the exact policy) and, because
 * this probe's `bad` layer plans the SAME 10 keys every single step (never drops out
 * of the wanted set), none of them is ever retried again for the rest of the run --
 * so `badLoadAttempts` is bounded by N=10 (one attempt per distinct bad key, ever,
 * for as long as a key never leaves the wanted set -- exactly the policy's own
 * stated non-goal: no retry budget, no backoff timer, just "never retried while
 * still wanted"), every slot frees up for `good` immediately, and `good` reaches
 * full residency (10/10) well within the 30-step run.
 */
async function probeStarvationDoesNotBlockGoodLayer() {
  const N = 10;
  const CAP = 6;
  const STEPS = 30;

  function makeImmediateLayer(id, fails) {
    let loadAttempts = 0;
    return {
      id,
      loadAttempts: () => loadAttempts,
      plan(_view) {
        const reqs = [];
        for (let i = 0; i < N; i += 1) {
          reqs.push({ key: `t${i}`, sseError: 10, viewDistanceM: 100, byteCost: 1024 });
        }
        return reqs;
      },
      load(request, _signal) {
        loadAttempts += 1;
        if (fails) return Promise.reject(new AlwaysFailsError(request.key));
        return Promise.resolve({ key: request.key, kind: 'starvation-probe-stub' });
      },
      release(_key) {},
    };
  }

  const bad = makeImmediateLayer('bad', true);
  const good = makeImmediateLayer('good', false);
  const mgr = new LayerManager({ memoryBudgetBytes: 999_000_000_000, maxConcurrentLoads: CAP, now: fakeNow });
  mgr.addLayer(bad);
  mgr.addLayer(good);
  const view = {}; // both layers' plan() above ignore `view` entirely

  for (let step = 0; step < STEPS; step += 1) {
    mgr.update(view);
    await flushMicrotasks();
  }

  const counts = mgr.countsByLayer();
  return {
    steps: STEPS,
    maxConcurrentLoads: CAP,
    badRequestCount: N,
    goodRequestCount: N,
    badLoadAttempts: bad.loadAttempts(),
    goodResident: counts.good.resident,
    badResident: counts.bad.resident,
    pending: mgr.pending.size,
    // Read defensively (not `mgr.failedCount`/`mgr.failureNames()` directly): this
    // probe is also run, deliberately, against today's UNFIXED layer.js (which has
    // neither) to demonstrate the starvation defect itself before the fix lands --
    // see this task's own report for that "before" run's output.
    failedCount: typeof mgr.failedCount === 'number' ? mgr.failedCount : null,
    failureNames: typeof mgr.failureNames === 'function' ? mgr.failureNames() : null,
    ok: counts.good.resident === N && bad.loadAttempts() <= N,
  };
}

const starvationProbe = await probeStarvationDoesNotBlockGoodLayer();

// ------------------------------------------------------------------------- run it
const steps = [];
let maxResidentBytesObserved = 0;

for (const cam of CAMERA_PATH) {
  const cameraEcef = cameraEcefFromEnu(cam.eastM, cam.northM, cam.upM);
  const tiles = selectTiles(cameraEcef, IMG_SCREEN);
  const view = {
    cameraEcef, screenHeightPx: IMG_SCREEN.screenHeightPx, fovYRad: IMG_SCREEN.fovYRad, tiles,
    sseThreshold: T3D_SCREEN.sseThreshold, maxLevel: T3D_SCREEN.maxLevel, maxTiles: T3D_SCREEN.maxTiles,
  };

  const cancelledBefore = manager.cancelledCount;
  const evictedBefore = manager.evictedCount;

  // TICKS_PER_POSITION (1, see above) real update() call(s) at this one camera
  // position. `plan`/`orderedKeys` below are recorded from the first tick (with
  // TICKS_PER_POSITION == 1, the only tick).
  let plan = null;
  for (let tick = 0; tick < TICKS_PER_POSITION; tick += 1) {
    const tickPlan = manager.update(view); // synchronous: priority sort, cancellation, new loads started, eviction
    if (plan === null) plan = tickPlan;
    imageryLoaderStub.completeOldest(COMPLETE_PER_STEP);
    tiles3dLoaderStub.completeOldest(COMPLETE_PER_STEP);
    // Awaiting inside this loop is deliberate: this is a scripted, sequential camera
    // path, not a hot render loop, and each tick must fully settle before the next
    // tick's/step's cancellation decisions are meaningful (see module docstring).
    await flushMicrotasks();
  }

  maxResidentBytesObserved = Math.max(maxResidentBytesObserved, manager.residentBytes);

  steps.push({
    label: cam.label,
    camera: { eastM: cam.eastM, northM: cam.northM, upM: cam.upM },
    orderedKeys: plan.map((r) => r.globalKey), // already priority-sorted by LayerManager.update()
    requests: plan.map((r) => ({
      globalKey: r.globalKey, layerId: r.layerId, key: r.key,
      sseError: r.sseError, viewDistanceM: r.viewDistanceM, byteCost: r.byteCost,
    })),
    residentBytes: manager.residentBytes,
    pendingCount: manager.pending.size,
    residentCount: manager.resident.size,
    cancelledThisStep: manager.cancelledCount - cancelledBefore,
    evictedThisStep: manager.evictedCount - evictedBefore,
    softViolationCount: manager.softViolationCount,
  });
}

// residentPayloadLookupWorks: pick any one resident globalKey (if any) and prove
// LayerManager.getResidentPayload() returns it -- the "one interface, one place to
// ask" proof for already-loaded content (see LayerManager.getResidentPayload's
// docstring and this file's module docstring).
let residentPayloadLookupWorks = null;
const anyResidentKey = manager.resident.keys().next();
if (!anyResidentKey.done) {
  const payload = manager.getResidentPayload(anyResidentKey.value);
  residentPayloadLookupWorks = payload !== undefined
    && (payload.kind === 'imagery-texture-stub' || payload.kind === 'tiles3d-content-stub');
}

const result = {
  steps,
  memoryBudgetBytes: manager.memoryBudgetBytes,
  maxResidentBytesObserved,
  budgetRespected: maxResidentBytesObserved <= manager.memoryBudgetBytes,
  cancelledCount: manager.cancelledCount,
  evictedCount: manager.evictedCount,
  // Whether _evictIfNeeded's soft-violation branch (see layer.js's own doc comment
  // on that method) ever fired over this run -- reported unconditionally, not just
  // when nonzero, so a green budgetRespected can never be hiding it (this file's own
  // module docstring, "byte budget" bullet, restated for this specific counter).
  softViolationCount: manager.softViolationCount,
  maxConcurrentLoads: manager.maxConcurrentLoads,
  queueIsLoadBearing,
  // Failure-memory policy (Finding 1, corrective round 3 -- see layer.js's
  // constructor doc comment for the exact policy statement): reported
  // unconditionally, exactly like softViolationCount above, so a failure is never
  // silently uncounted. This run's terrain layer fails every distinct tile key it is
  // ever asked for (TerrainLoaderNotImplementedError, see terrain_layer.js), so
  // failedCount here is expected to be bounded by the number of distinct terrain
  // keys this camera path ever wants, not by step count.
  // Read defensively -- see probeStarvationDoesNotBlockGoodLayer's own comment: this
  // file is deliberately also run against layer.js before the failure-memory fix
  // lands, and that version has neither field.
  failedCount: typeof manager.failedCount === 'number' ? manager.failedCount : null,
  failureNames: typeof manager.failureNames === 'function' ? manager.failureNames() : null,
  starvationProbe,
  perLayerCounts: manager.countsByLayer(),
  terrainRefusalCount,
  terrainRefusalName,
  planNeverLoadsDirectly,
  residentPayloadLookupWorks,
  imageryTileBytes: IMAGERY_TILE_BYTES,
  defaultTile3DBytes: DEFAULT_TILE3D_BYTES,
  tilesetTileCount: tree.nodes.size,
};

process.stdout.write(JSON.stringify(result));

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
//     budget). MEMORY_BUDGET_BYTES below was chosen by first running this exact
//     harness with an effectively unlimited budget (999 GB) to measure the path's
//     true cumulative distinct-byte total with eviction disabled -- 169,607,168 bytes
//     (98 distinct items: 37 imagery tiles + 61 3D-tile requests that ever completed;
//     `steps[*].residentBytes` climbs monotonically from 6,291,456 at 'far' to that
//     ceiling by 'jump-ne-2' and holds flat through 'far-2') -- then picking
//     MEMORY_BUDGET_BYTES = 120,000,000, comfortably below that ceiling but still
//     well above any single early step's own resident total (e.g. 'close-sw' alone
//     reaches 51,642,368 with room to spare), so eviction must run partway through
//     the path for `budgetRespected` to hold, and cannot simply be satisfied by the
//     budget never being approached at all -- the same design `globe_lod_check.mjs`'s
//     RESIDENT_BUDGET documents for its own tile-count budget, with the actual
//     measured numbers recorded here (not estimated) so a later edit to the camera
//     path or completion schedule cannot silently drift the budget into "never
//     binds" territory without this comment visibly going stale.
//   - `cancelledCount > 0` catches a manager that never cancels a stale in-flight
//     load: the 'close-sw-2' -> 'jump-ne' step is a deliberate jump from one corner
//     of the 3D Tiles fixture's small (~4.4 km) footprint to the diagonally-opposite
//     corner (see web/js/tiles3d_check.mjs's own module docstring for the identical
//     fixture-geometry reasoning), inserted before every pending load from the
//     previous step has completed (`completeOldest` only ever completes a bounded
//     number per step, see above) -- an implementation that queues every requested
//     load and never cancels a stale one would report `cancelledCount === 0`.
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
const MEMORY_BUDGET_BYTES = 120_000_000;

// Bounded per-step completion count (see module docstring: "oldest pending first",
// same technique as globe_lod_check.mjs's completeLoads(8, ...)) -- deliberately well
// below the number of new requests a close/jump step introduces, so a real backlog of
// still-pending requests always survives into the next step.
const COMPLETE_PER_STEP = 15;

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

const manager = new LayerManager({ memoryBudgetBytes: MEMORY_BUDGET_BYTES, now: fakeNow });
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

  const plan = manager.update(view); // synchronous: priority sort, cancellation, new loads started, eviction

  imageryLoaderStub.completeOldest(COMPLETE_PER_STEP);
  tiles3dLoaderStub.completeOldest(COMPLETE_PER_STEP);
  // Awaiting inside this loop is deliberate: this is a scripted, sequential camera
  // path, not a hot render loop, and each step must fully settle before the next
  // step's cancellation decisions are meaningful (see module docstring).
  await flushMicrotasks();

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

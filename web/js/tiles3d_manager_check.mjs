// CLI harness for tests/test_viewer_tiles3d_manager.py: `node web/js/tiles3d_manager_check.mjs`.
//
// Round 5 (docs/open-questions.md question 228/decision 9, round 4's own deferral,
// ratified question 229): proves the 3D Tiles overlay's real per-tile content fetch is
// genuinely gated by the shared `LayerManager` (web/js/layers/layer.js), via
// `Tiles3DLayerAdapter`/`ManagerGatedTilesFetchPlugin` (web/js/layers/tiles3d_layer.js
// -- see that file's own module docstring for the full mechanism). This is a NEW file,
// deliberately separate from `web/js/layers_budget_check.mjs`/`tests/
// test_viewer_layers_budget.py` (already modified for a different, in-flight commit
// this task must not touch) and from `web/js/tiles3d_check.mjs` (the PRE-round-5
// fixture check this task must leave byte-for-byte unchanged -- it drives
// `TileLoadScheduler`/`selectTiles3D` directly and never touches
// `Tiles3DLayerAdapter`/`LayerManager`/`TilesOverlayLayer` at all, so nothing in this
// file or in `web/js/layers/tiles3d_layer.js`'s/`web/js/tiles_layer.js`'s round-5
// changes can move its output -- see this task's own report for the before/after diff).
//
// What this proves, and what a wrong implementation would fail against (this file's
// own `main()`, below, drives each proof with the assertions living in
// `tests/test_viewer_tiles3d_manager.py`, same split as every other *_check.mjs/
// test_viewer_*.py pair in this codebase):
//   1. noDuplicateFetch -- the vendored renderer's own `fetchData` hook (simulated here
//      by calling the REAL `ManagerGatedTilesFetchPlugin.fetchData()` directly, the
//      same method a live `TilesRenderer.requestTileContents()` calls) can ask for the
//      same URL repeatedly, before AND after the manager admits it, without the
//      injected loader ever being invoked more than once for that URL -- counted, not
//      inferred.
//   2. budgetGates -- a wanted set whose independently-recomputed total byteCost
//      exceeds `memoryBudgetBytes` still never lets `residentBytes` (recomputed by
//      summing a FRESH `plan()` call's own byteCost for every currently-resident key,
//      never by trusting the manager's own stored total) exceed the budget, and
//      `deferredCount` moves.
//   3. cancellationReal -- a request admitted (loader invoked, real signal handed out)
//      but not yet settled, then dropped from the wanted set by a camera jump, has its
//      manager-issued `AbortController` genuinely aborted (`signal.aborted`, checked
//      directly) AND its own load promise genuinely rejects with a real `AbortError`
//      (awaited and caught, not inferred from a counter).
//   4. releaseRefetches -- evicting a resident tile (`release(key)`) and later
//      re-wanting the same key causes the injected loader to be invoked AGAIN for that
//      URL (a real, observable second network attempt), proving `release()` actually
//      dropped the cached fetch rather than leaving a stale entry silently reused.
//   5. realByteReconciliation -- once a real fetch resolves with a `Content-Length`
//      different from the constructor's declared estimate, a FRESH `plan()` call
//      reports that real number (never the stale estimate) for the same key, and the
//      manager's own (unmodified) reconciliation pass in `update()` adjusts
//      `residentBytes` by exactly that delta (`byteCostRevisionCount`/
//      `byteCostRevisionBytes` move) -- this task's own analogue of round 4's defect 1.
//   6. noCrossContamination -- registering the overlay's adapter on the SAME manager a
//      fake globe-shaped imagery layer is also registered on, and driving BOTH from
//      ONE merged `update()` call per tick (`web/js/scene.js`'s own round-5
//      orchestration), never evicts/cancels the imagery layer's own still-wanted
//      resident entry. `crossContaminationCounterfactual` is the measured PROOF this
//      was a real hazard, not a hypothetical one this task invented an excuse to fix:
//      the SAME setup, driven by the naive "call update() twice, once per layer, each
//      with only that layer's own fields" pattern this task's own scene.js changes
//      deliberately avoid, DOES evict the imagery layer's resident entry -- see
//      `web/js/tiles_layer.js`'s own "Round 5" module docstring and `web/js/globe.js`'s
//      `update()` docstring for the full reasoning.
//
// Deliberate, disclosed limitation (see also web/VIEWER.md's own new section): this is
// `node`, not a browser -- there is no live vendored `TilesRenderer` here at all (that
// library needs `window`/`requestAnimationFrame`, confirmed directly while building
// this, see tiles_layer.js's own module docstring). `ManagerGatedTilesFetchPlugin` is
// exercised for REAL (the exact exported class, the exact `fetchData(url, options)`
// method signature a live `TilesRenderer.requestTileContents()` calls, checked
// directly against the pinned vendored source -- see tiles3d_layer.js's own module
// docstring for the exact line numbers), only the CALLER (a live TilesRenderer) is
// stood in for by this harness calling that same method directly with the same
// argument shapes. `tests/test_viewer_tiles3d_manager.py` also drives a REAL headless
// Chrome with the REAL vendored renderer for the live half of this proof.
import { LayerManager, globalKeyFor } from './layers/layer.js';
import { Tiles3DLayerAdapter, ManagerGatedTilesFetchPlugin, DEFAULT_TILE3D_BYTES } from './layers/tiles3d_layer.js';
import { selectTiles3D, parseTileset3D, ecefFromRootTransform, enuBasisFromRootTransform } from './tiles_layer.js';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TILESET_PATH = path.join(__dirname, '..', 'fixtures', '3dtiles', 'tileset.json');
const tilesetJson = JSON.parse(fs.readFileSync(TILESET_PATH, 'utf8'));

function freshTree() { return parseTileset3D(tilesetJson); }

const anchorEcef = ecefFromRootTransform(freshTree().rootTransform);
const basis = enuBasisFromRootTransform(freshTree().rootTransform);
function cameraEcefFromEnu(eastM, northM, upM) {
  return {
    x: anchorEcef.x + basis.east.x * eastM + basis.north.x * northM + basis.up.x * upM,
    y: anchorEcef.y + basis.east.y * eastM + basis.north.y * northM + basis.up.y * upM,
    z: anchorEcef.z + basis.east.z * eastM + basis.north.z * northM + basis.up.z * upM,
  };
}
const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 16, maxLevel: 12, maxTiles: 512 };
// 'far' selects only the fixture's root tile ('0') -- a single, stable key this
// harness's cancellation/release/reconciliation proofs (which need to reason about
// ONE specific key across several ticks) can rely on deterministically.
const FAR_VIEW = { cameraEcef: cameraEcefFromEnu(0, 0, 3000000), ...SCREEN };
const CLOSE_SW_VIEW = { cameraEcef: cameraEcefFromEnu(-2200, -2200, 4000), ...SCREEN };
const JUMP_NE_VIEW = { cameraEcef: cameraEcefFromEnu(2200, 2200, 4000), ...SCREEN };

/** A real `fetch()` `Response`-shaped stub (`.ok`, `.status`, `.headers.get`) with a
 * controllable, real `Content-Length` -- never reads/needs a body (see this file's
 * module docstring: nothing here parses glTF). Resolution is manually triggered
 * (`completeOldest`), same "oldest pending first" technique `web/js/layers_check.mjs`'s
 * own stubs use, so a realistic in-flight backlog can survive into a camera jump. */
function makeFetchStub({ contentLength = null } = {}) {
  const inflight = [];
  let invocationCount = 0;
  const callsByUrl = new Map();
  // Per-KEY invocation counts, separate from `callsByUrl`: this project's own
  // fixture gives every tile the SAME relative content URI, so a URL-keyed count
  // conflates "this SPECIFIC tile was fetched again" with "some OTHER tile sharing
  // the same URL was fetched" -- a real confound this harness hit directly while
  // proving `release()` (see `proveReleaseRefetches`, which needs the former, not
  // the latter).
  const callsByKey = new Map();
  function loader(request, signal) {
    invocationCount += 1;
    callsByUrl.set(request.url, (callsByUrl.get(request.url) || 0) + 1);
    callsByKey.set(request.key, (callsByKey.get(request.key) || 0) + 1);
    return new Promise((resolve, reject) => {
      if (signal.aborted) { reject(signal.reason || makeAbortError()); return; }
      const entry = { request, resolve };
      inflight.push(entry);
      signal.addEventListener('abort', () => {
        const idx = inflight.indexOf(entry);
        if (idx >= 0) inflight.splice(idx, 1);
        reject(signal.reason || makeAbortError());
      }, { once: true });
    });
  }
  function makeAbortError() {
    try { return new DOMException('aborted', 'AbortError'); } catch {
      const e = new Error('aborted'); e.name = 'AbortError'; return e;
    }
  }
  return {
    loader,
    invocationCount: () => invocationCount,
    invocationCountFor: (url) => callsByUrl.get(url) || 0,
    invocationCountForKey: (key) => callsByKey.get(key) || 0,
    pendingCount: () => inflight.length,
    completeOldest(n) {
      const batch = inflight.splice(0, Math.max(0, n));
      for (const entry of batch) {
        entry.resolve({
          ok: true,
          status: 200,
          headers: { get: (name) => (name.toLowerCase() === 'content-length' && contentLength != null ? String(contentLength) : null) },
        });
      }
      return batch.length;
    },
  };
}

// A minimal, real (not mocked) Layer -- exactly the interface layer.js documents --
// standing in for GlobeLayer's own ImageryLayerAdapter, so proof 6 (below) registers
// something genuinely independent on the SAME manager, without importing globe.js's
// own THREE/DOM-touching code into this headless harness.
function makeFakeImageryLayer() {
  let releaseCount = 0;
  let loadCount = 0;
  return {
    id: 'imagery',
    releaseCount: () => releaseCount,
    loadCount: () => loadCount,
    plan(view) {
      if (!view.tiles) return [];
      return view.tiles.map((t) => ({
        key: t.key, sseError: 10, viewDistanceM: 100, byteCost: 1000, level: 0,
      }));
    },
    load(_request, signal) {
      loadCount += 1;
      return new Promise((resolve, reject) => {
        if (signal.aborted) { reject(new Error('aborted')); return; }
        resolve('imagery-payload');
      });
    },
    release() { releaseCount += 1; },
  };
}

function flushMicrotasks() {
  return new Promise((resolve) => { setImmediate(resolve); });
}

async function proveNoDuplicateFetch() {
  const tree = freshTree();
  const fetchStub = makeFetchStub();
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  const plugin = new ManagerGatedTilesFetchPlugin({ adapter, fallbackFetch: () => { throw new Error('should never fall back in this proof'); } });
  const manager = new LayerManager({ memoryBudgetBytes: 100_000_000, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);

  manager.update(FAR_VIEW); // plan() runs, populates adapter._urlToKey for the root tile's URL
  const rootRequests = adapter.plan(FAR_VIEW);
  const rootUrl = rootRequests[0].url;

  // The renderer "asks" three times before the manager's own admitted load() has
  // settled -- all three must resolve to the SAME promise the manager's load() itself
  // produced, never a second fetch. Each ask carries its OWN real (never-aborted)
  // AbortSignal, exactly as the real `TilesRenderer.requestTileContents()` always
  // does (this file's module docstring, "Fetch gating") -- never an empty `{}`.
  const askSignal = () => ({ signal: new AbortController().signal });
  const askedBeforeSettle = [
    plugin.fetchData(rootUrl, askSignal()),
    plugin.fetchData(rootUrl, askSignal()),
    plugin.fetchData(rootUrl, askSignal()),
  ];
  await flushMicrotasks();
  // Complete generously (not just the 1 real fetch the FIXED gate produces): a
  // perturbed/broken gate that lets each ask start its own real fetch would leave
  // several inflight entries, and this proof must not hang waiting for ones it never
  // told the stub to complete -- see this task's own report for the perturbation this
  // guards against.
  fetchStub.completeOldest(10);
  await Promise.all(askedBeforeSettle);
  // ...and once more AFTER it has settled (the renderer's own traversal asking again
  // on a later tick) -- must reuse the cached, already-resolved promise. A broken
  // gate would start yet another real fetch here too; complete generously again so
  // this proof cannot hang waiting on one it never told the stub to complete.
  const askedAfterSettlePromise = plugin.fetchData(rootUrl, askSignal());
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  const askedAfterSettle = await askedAfterSettlePromise;

  // Second, DIFFERENT proof, on purpose testing the OTHER direction: this project's
  // own fixture (`web/fixtures/3dtiles/tileset.json`) gives every tile node the SAME
  // relative content URI (`tile.glb`), so a close-in view that wants MANY distinct
  // tile keys wants them all at the SAME resolved URL -- a real, not synthetic,
  // multi-key/one-URL collision. `_fetchesByKey`/`_fetchForKey` are keyed by this
  // adapter's own LOCAL TILE KEY, deliberately NOT by URL (see that field's own
  // constructor comment for why: a URL-keyed cache was tried first and directly
  // regressed `web/js/layers_check.mjs`'s own existing, pinned output against exactly
  // this fixture property -- measured, not theorised, see this task's own report) --
  // so each of these distinct KEYS must still get its OWN independent real fetch, one
  // per key, same as every other adapter in this codebase (`ImageryLayerAdapter`'s own
  // loader is never deduplicated by URL either). This is the necessary OTHER HALF of
  // "no duplicate fetch": deduplication must be scoped to "the same tile asked for
  // twice" (proven above), never accidentally extended to "two different tiles that
  // happen to reference identical bytes", which would silently make one tile stand in
  // for another's own resident accounting.
  const manyKeysTree = freshTree();
  const manyKeysFetchStub = makeFetchStub();
  const manyKeysAdapter = new Tiles3DLayerAdapter({ tree: manyKeysTree, loader: manyKeysFetchStub.loader });
  const manyKeysManager = new LayerManager({ memoryBudgetBytes: 1_000_000_000, maxConcurrentLoads: 64 });
  manyKeysManager.addLayer(manyKeysAdapter);
  manyKeysManager.update(CLOSE_SW_VIEW);
  await flushMicrotasks();
  manyKeysFetchStub.completeOldest(1000);
  await flushMicrotasks();
  const manyKeysRequests = manyKeysAdapter.plan(CLOSE_SW_VIEW);
  const distinctUrls = new Set(manyKeysRequests.map((r) => r.url));
  const admittedKeyCount = [...manyKeysManager.resident.keys()].length + [...manyKeysManager.pending.keys()].length;

  return {
    fetchCallCount: adapter.fetchCallCount,
    loaderInvocationCount: fetchStub.invocationCount(),
    loaderInvocationCountForRootUrl: fetchStub.invocationCountFor(rootUrl),
    askedAfterSettleIsOk: askedAfterSettle && askedAfterSettle.ok === true,
    fetchDataFallbackCount: adapter.fetchDataFallbackCount,
    manyKeysSharedOneUrl: {
      distinctTileKeysWanted: manyKeysRequests.length,
      distinctUrlsAmongThem: distinctUrls.size,
      admittedKeyCount,
      loaderInvocationCount: manyKeysFetchStub.invocationCount(),
      // Each distinct KEY got its own fetch (dedup is per-key, not per-URL) --
      // loaderInvocationCount should equal admittedKeyCount, NOT distinctUrls.size.
      dedupIsPerKeyNotPerUrl: manyKeysFetchStub.invocationCount() === admittedKeyCount,
    },
  };
}

async function proveBudgetGates() {
  const tree = freshTree();
  const fetchStub = makeFetchStub();
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  // Deliberately far below what a full 'close-sw' selection costs at the default
  // per-tile estimate (mirrors tiles3d_check.mjs's own RESIDENT_BUDGET reasoning: a
  // budget strictly between one step's own working set and its full cost).
  const memoryBudgetBytes = DEFAULT_TILE3D_BYTES * 6;
  const manager = new LayerManager({ memoryBudgetBytes, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);

  const steps = [];
  for (const view of [FAR_VIEW, CLOSE_SW_VIEW, CLOSE_SW_VIEW]) {
    manager.update(view);
    fetchStub.completeOldest(50); // drain aggressively -- this proof is about ADMISSION, not backlog
    await flushMicrotasks();
    // Independent recomputation (never trust manager.residentBytes as ground truth on
    // its own): re-derive each resident key's CURRENT byteCost from a FRESH plan()
    // call over the SAME view, then sum. If this ever disagrees with
    // manager.residentBytes, that is itself a finding, not something this harness
    // hides -- see `residentBytesRecomputedMatches`, below.
    const freshByKey = new Map(adapter.plan(view).map((r) => [globalKeyFor(adapter.id, r.key), r.byteCost]));
    let recomputedResidentBytes = 0;
    for (const key of manager.resident.keys()) {
      const cost = freshByKey.has(key) ? freshByKey.get(key) : manager.resident.get(key).byteCost;
      recomputedResidentBytes += cost;
    }
    steps.push({
      residentBytes: manager.residentBytes,
      recomputedResidentBytes,
      pendingBytes: manager.pendingBytes,
      deferredCount: manager.deferredCount,
      withinBudget: manager.residentBytes <= memoryBudgetBytes,
    });
  }

  const wantedAtClose = adapter.plan(CLOSE_SW_VIEW);
  const wantedTotalBytes = wantedAtClose.reduce((s, r) => s + r.byteCost, 0);

  return {
    memoryBudgetBytes,
    wantedTileCountAtClose: wantedAtClose.length,
    wantedTotalBytesAtClose: wantedTotalBytes,
    wantedExceedsBudget: wantedTotalBytes > memoryBudgetBytes,
    steps,
    finalDeferredCount: manager.deferredCount,
    everyStepWithinBudget: steps.every((s) => s.withinBudget),
    residentBytesRecomputedMatches: steps.every((s) => s.residentBytes === s.recomputedResidentBytes),
    softViolationCount: manager.softViolationCount,
  };
}

async function proveCancellationReal() {
  const tree = freshTree();
  const fetchStub = makeFetchStub();
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  const manager = new LayerManager({ memoryBudgetBytes: 100_000_000, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);

  manager.update(CLOSE_SW_VIEW); // admits real loads; NONE completed yet (fetchStub never told to resolve)
  const pendingEntries = [...manager.pending.entries()];
  if (pendingEntries.length === 0) throw new Error('proveCancellationReal: nothing pending -- test setup is wrong');
  const [watchedKey, watchedPending] = pendingEntries[0];
  const watchedSignal = watchedPending.controller.signal;
  const cancelledBefore = manager.cancelledCount;
  // Recover the REAL underlying loader promise adapter.load() produced for this key
  // (via the adapter's own per-key fetch cache -- never a second promise invented by
  // this harness) so this proof can await its own genuine rejection, not merely
  // observe a counter moving elsewhere.
  const watchedFetchPromise = adapter._fetchesByKey.get(watchedPending.localKey).promise;

  manager.update(JUMP_NE_VIEW); // 'close-sw' and 'jump-ne' are mostly-disjoint (see camera-path comment style in tiles3d_check.mjs/layers_check.mjs) -- watchedKey should drop out

  const stillPending = manager.pending.has(watchedKey);
  const stillResident = manager.resident.has(watchedKey);
  let rejectionName = null;
  try {
    await watchedFetchPromise;
  } catch (err) {
    rejectionName = err && err.name;
  }

  return {
    watchedKeyDroppedFromWanted: !stillPending && !stillResident,
    signalAborted: watchedSignal.aborted,
    cancelledCountMoved: manager.cancelledCount > cancelledBefore,
    cancelledCountDelta: manager.cancelledCount - cancelledBefore,
    loadPromiseRejectedWithAbortError: rejectionName === 'AbortError',
  };
}

async function proveReleaseRefetches() {
  const tree = freshTree();
  const fetchStub = makeFetchStub();
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  // A tiny budget that can only ever hold the root tile alone, so wanting the root
  // again after wanting (and fully admitting/resolving) something else forces a real
  // LRU eviction of the root -- exactly the scenario `release()` must handle.
  const memoryBudgetBytes = DEFAULT_TILE3D_BYTES + 1;
  const manager = new LayerManager({ memoryBudgetBytes, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);

  manager.update(FAR_VIEW);
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  await flushMicrotasks();
  const rootRequests = adapter.plan(FAR_VIEW);
  const rootUrl = rootRequests[0].url;
  const rootLocalKey = rootRequests[0].key;
  const rootKey = globalKeyFor(adapter.id, rootLocalKey);
  // Per-KEY, not per-URL (see makeFetchStub's own comment: this fixture's tiles all
  // share one URL, so a URL-keyed count would also see the OTHER tiles this proof
  // wants in between, below).
  const invocationsAfterFirstLoad = fetchStub.invocationCountForKey(rootLocalKey);
  const residentAfterFirstLoad = manager.resident.has(rootKey);

  // Want something else that does NOT include the root -- forces eviction (root is no
  // longer in the wanted set, budget can't hold both anyway).
  manager.update(CLOSE_SW_VIEW);
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  await flushMicrotasks();
  const evictedAfterElsewhereWanted = !manager.resident.has(rootKey) && !manager.pending.has(rootKey);

  // Want the root again -- a genuinely fresh fetch must happen (release() must have
  // dropped the adapter's own cached entry; if it had not, `_fetchForKey` would just
  // return the stale, already-settled promise and invocationCountForKey would not
  // move).
  manager.update(FAR_VIEW);
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  await flushMicrotasks();
  const invocationsAfterRewant = fetchStub.invocationCountForKey(rootLocalKey);

  return {
    memoryBudgetBytes,
    residentAfterFirstLoad,
    invocationsAfterFirstLoad,
    evictedAfterElsewhereWanted,
    evictedCount: manager.evictedCount,
    invocationsAfterRewant,
    refetchedAfterRelease: invocationsAfterRewant > invocationsAfterFirstLoad,
  };
}

async function proveRealByteReconciliation() {
  const tree = freshTree();
  const REAL_BYTES = DEFAULT_TILE3D_BYTES * 3; // deliberately far from the fallback estimate
  const fetchStub = makeFetchStub({ contentLength: REAL_BYTES });
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  const manager = new LayerManager({ memoryBudgetBytes: 100_000_000, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);

  const beforeLoad = adapter.plan(FAR_VIEW)[0];
  manager.update(FAR_VIEW);
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  await flushMicrotasks();

  const revisionCountBefore = manager.byteCostRevisionCount;
  const residentBytesBefore = manager.residentBytes;
  // One more tick over the SAME view: `plan()` now reports the REAL size (see
  // Tiles3DLayerAdapter's own module docstring), and `LayerManager.update()`'s own,
  // UNMODIFIED reconciliation pass is what must pick up the difference -- this proof
  // adds no reconciliation logic of its own.
  manager.update(FAR_VIEW);
  const afterLoad = adapter.plan(FAR_VIEW)[0];

  return {
    declaredContentLength: REAL_BYTES,
    defaultEstimate: DEFAULT_TILE3D_BYTES,
    byteCostBeforeLoad: beforeLoad.byteCost,
    byteCostSourceBeforeLoad: beforeLoad.byteCostSource,
    byteCostAfterLoad: afterLoad.byteCost,
    byteCostSourceAfterLoad: afterLoad.byteCostSource,
    freshPlanNowReportsRealBytes: afterLoad.byteCost === REAL_BYTES && afterLoad.byteCostSource === 'measured',
    residentBytesBefore,
    residentBytesAfter: manager.residentBytes,
    byteCostRevisionCountMoved: manager.byteCostRevisionCount > revisionCountBefore,
    residentBytesNowReflectsReal: manager.residentBytes === REAL_BYTES,
  };
}

/** `driveTwice`: when true, replays the exact bug this task's own `web/js/scene.js`
 * changes avoid -- two independent `manager.update()` calls per tick, one carrying
 * only the imagery-shaped layer's own field (`tiles`), one carrying only the 3D-tiles
 * adapter's own fields -- so this proof can show what WOULD happen without the fix,
 * not just assert the fix works. See this file's own module docstring, proof 6. */
async function runCrossContamination(driveTwice) {
  const tree = freshTree();
  const fetchStub = makeFetchStub();
  const adapter = new Tiles3DLayerAdapter({ tree, loader: fetchStub.loader });
  const imagery = makeFakeImageryLayer();
  const manager = new LayerManager({ memoryBudgetBytes: 100_000_000, maxConcurrentLoads: 64 });
  manager.addLayer(adapter);
  manager.addLayer(imagery);

  const imageryTiles = [{ key: 'tile-A' }, { key: 'tile-B' }];
  // Realistic shapes -- mirroring `GlobeLayer.update()` (web/js/globe.js, always
  // includes cameraEcef/screenHeightPx/fovYRad alongside its own `tiles`) and
  // `TilesOverlayLayer.getManagedView()` (web/js/tiles_layer.js, always includes
  // cameraEcef/screenHeightPx/fovYRad, never `.tiles`) -- so `driveTwice` replays
  // the ACTUAL naive call shapes this task's own scene.js changes avoid, not a
  // stress-test shape (e.g. a view missing cameraEcef entirely) neither real class
  // would ever produce.
  const tilesFragment = { tiles: imageryTiles, ...FAR_VIEW };
  const tiles3dFragment = FAR_VIEW;
  const mergedView = { ...tilesFragment, ...tiles3dFragment };

  // Tick 1: admit imagery's own two tiles.
  if (driveTwice) {
    manager.update(tilesFragment); // imagery-only view -- tiles3d sees nothing this call
    manager.update(tiles3dFragment); // tiles3d-only view -- imagery sees NOTHING this call (the bug)
  } else {
    manager.update(mergedView); // scene.js's own round-5 pattern: one call, every field present
  }
  await flushMicrotasks();
  fetchStub.completeOldest(10);
  await flushMicrotasks();
  const imageryResidentAfterTick1 = imageryTiles.every(
    (t) => manager.resident.has(globalKeyFor('imagery', t.key)),
  );

  // Ticks 2-4: imagery's OWN wanted set never changes -- a correct caller must never
  // evict/cancel it merely because it re-declared the same demand.
  for (let i = 0; i < 3; i += 1) {
    if (driveTwice) {
      manager.update(tilesFragment);
      manager.update(tiles3dFragment);
    } else {
      manager.update(mergedView);
    }
    await flushMicrotasks();
    fetchStub.completeOldest(10);
    await flushMicrotasks();
  }

  return {
    imageryResidentAfterTick1,
    imageryReleaseCount: imagery.releaseCount(),
    imageryLoadCount: imagery.loadCount(), // > imageryTiles.length would mean a real re-fetch happened (thrash)
    managerCancelledCount: manager.cancelledCount,
    managerEvictedCount: manager.evictedCount,
    imageryStillResidentAtEnd: imageryTiles.every((t) => manager.resident.has(globalKeyFor('imagery', t.key))),
  };
}

async function main() {
  const result = {
    noDuplicateFetch: await proveNoDuplicateFetch(),
    budgetGates: await proveBudgetGates(),
    cancellationReal: await proveCancellationReal(),
    releaseRefetches: await proveReleaseRefetches(),
    realByteReconciliation: await proveRealByteReconciliation(),
    crossContaminationFixed: await runCrossContamination(false),
    crossContaminationCounterfactual: await runCrossContamination(true),
  };
  process.stdout.write(JSON.stringify(result));
}

main();

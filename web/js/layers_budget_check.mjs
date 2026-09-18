#!/usr/bin/env node
// CLI harness for tests/test_viewer_layers_budget.py: `node web/js/layers_budget_check.mjs`.
//
// Round 4 (docs/open-questions.md question 228, the lead's own browser drive,
// "Finding 1, the memory budget is crossed"): with `memoryBudgetBytes` = 40 MB and a
// true per-tile cost of 3,147,060 bytes, the resident set reached 96 MB in a real
// browser, because `LayerManager.update()` (web/js/layers/layer.js) admitted a load
// for every wanted request with no reference to the budget at all, and
// `_evictIfNeeded` refuses to evict anything in the current wanted set -- so once the
// wanted set alone exceeded the budget there was nothing left it was willing to
// evict, and it gave up (a "soft violation") rather than bound `residentBytes`. This
// file is that fix's own proof, following `web/js/layers_check.mjs`'s exact
// conventions (a real `LayerManager`, synthetic-but-real `Layer` objects exactly like
// that file's own `probeStarvationDoesNotBlockGoodLayer`, an injected deterministic
// `now`, a FIFO loader stub completed by this harness, `flushMicrotasks` as the only
// ordering barrier -- no clock ever slept, question 154/51: no network, no fetch, no
// filesystem I/O beyond this script's own source).
//
// # The scenario (the lead's own measured numbers, driven synthetically)
//
// 32 tiles of TILE_BYTES=3,147,060 bytes each (100,705,920 bytes of wanted content --
// exactly the lead's own real, measured per-tile cost) against
// MEMORY_BUDGET_BYTES=41,943,040 (40 MiB) -- a wanted set 2.40x the budget, spread
// across levels the way a real quadtree refinement does: 1 tile at level 0 (the
// root), 4 at level 1, 11 at level 2, 16 at level 3 (LEVEL_SHAPE, below) -- a
// plausible quadtree branching shape (each level roughly refining a fraction of its
// parent's footprint), chosen so this harness's own admission-order proof has
// multiple levels to distinguish, not merely "one coarse tile, then everything else".
// Every tile's `level` is explicit in this file's own printed JSON (`wantedTiles`) --
// nothing here reads a real camera/quadtree, the level assignment IS the input, by
// design (this harness proves LayerManager's admission policy, not tile selection,
// which globe_lod_check.mjs/tiles3d_check.mjs already cover for their own modules).
//
// `sseError` is assigned FINER-IS-HIGHER (ascending with level: level 0's one tile is
// the lowest, level 3's sixteen tiles are the highest) -- deliberately the opposite of
// admission order, so this harness's own proof is not accidentally easy: if
// `LayerManager` admitted in `comparePriority` order (the defect this task does NOT
// fix -- that comparator is unchanged, see layer.js) instead of `compareAdmission`
// order (round 4's fix), the finest, highest-sseError level-3 tiles would be admitted
// FIRST here, filling the entire budget with fine detail and leaving every coarse
// tile -- including the single level-0 root tile that covers the whole view --
// deferred forever. A harness that assigned sseError coarser-is-higher (matching
// admission order) would not be able to tell "admits by level" from "admits by raw
// priority" apart at all.
//
// # Three phases: over-budget admission, a camera move, then a byteCost revision
//
// Phase 1 drives the 32-tile scenario above from an empty manager to steady state --
// this is the lead's own literal measured case, and every REQUIRED assertion (see
// tests/test_viewer_layers_budget.py) is checked against it.
//
// Phase 2 (additional, beyond the letter of this task's proof requirement, but
// directly exercising deliverable 3 -- "eviction must make room, or the manager
// deadlocks"): once phase 1 settles, the SAME layer is asked for a totally different,
// equally over-budget 32-tile set (a disjoint `key` prefix, same level shape/byte
// cost) -- modelling a camera move to a new part of the globe. Every phase-1 tile is
// now unwanted. A hard admission limit with no eviction-to-make-room would deadlock
// here: the budget stays entirely full of the old, now-unwanted tiles, and nothing
// new could ever be admitted (see layer.js's own module docstring, "Round 4", third
// bullet). This phase proves that does not happen: `update()` evicts unwanted LRU
// residents to make room, the manager reaches a NEW steady state (not the
// `MAX_ITERATIONS` safety cap), and by the end every phase-1 tile has been evicted
// (`evictedCount` grows) while phase 2's own coarse-first steady state forms, exactly
// symmetric to phase 1's.
//
// Phase 3 (manager review of this task's own round-4 admission fix): its own
// isolated manager/layer, entirely separate from phase 1/2's. Proves `update()`
// reconciles an already-resident, still-wanted entry's stored `byteCost` when its
// layer's `plan()` later charges a different one for the same key -- exactly what a
// real `GatewayImageryLayerAdapter.fetchManifest()` resolving mid-run does (round
// 4's own manifest fix charges a fallback estimate until the manifest resolves, then
// the tile's real, usually larger, manifest-declared size). Two sub-cases: 3a
// (recoverable -- the revised total still fits the budget) and 3b (irreconcilable --
// it does not, and nothing is evictable, so the honest outcome is a genuine
// `softViolationCount` increment, not a silently wrong `residentBytes`). See that
// section's own comment, below, for the exact numbers and reasoning.
//
// # What "settled" means here, and why there is no sleep anywhere
//
// Both phases run the identical loop: call `manager.update(view)` (synchronous:
// cancellation, admission -- including any make-room eviction -- and the eviction
// tripwire), sample `residentBytes`/`pendingBytes`, and -- ONLY if something is
// actually pending -- complete exactly the single OLDEST in-flight load and await one
// `flushMicrotasks()` barrier (a `setImmediate` round trip, not a timer *duration* --
// design constraint g, "no clocks slept, ever", same discipline as every other
// harness in this directory), then sample again. Completing exactly one load at a
// time (not a batch) is what lets this harness satisfy the brief's own requirement to
// sample `residentBytes` "after every load settles", not merely after a batch of
// them. The loop stops the moment an `update()` call finds nothing newly admittable
// AND nothing left pending -- a real, polled condition (mirroring every other
// harness's own "no fixed sleep-then-assume" discipline): with a STATIC wanted set
// (this harness never changes `view.requests` mid-phase) and nothing pending, no
// future `update()` over the SAME view can ever admit anything new (nothing
// unwanted left to evict for anything already tried and deferred), so this is a
// genuine fixed point, not an arbitrary stopping point. `MAX_ITERATIONS` is a safety
// cap only, generous relative to what 32 tiles at `MAX_CONCURRENT_LOADS` concurrency
// actually need (`settled` in the printed JSON says whether the real condition was
// reached, exactly like `web/js/layers_stream_check.mjs`'s own `settledBeforeMaxFrames`).
import { LayerManager, compareAdmission } from './layers/layer.js';

// -------------------------------------------------------------------- deterministic clock
// Design constraint g ("no clocks slept, ever"): a plain incrementing counter, never a
// real time source -- identical to web/js/layers_check.mjs's own `fakeNow`.
let fakeNowCounter = 0;
function fakeNow() { fakeNowCounter += 1; return fakeNowCounter; }

/** Waits for every already-queued microtask to run -- a `setImmediate` round trip, not
 * a timer *duration* -- identical to web/js/layers_check.mjs's own `flushMicrotasks`. */
function flushMicrotasks() {
  return new Promise((resolve) => { setImmediate(resolve); });
}

// ------------------------------------------------------------- the independent invariant check
// Manager review follow-up: "add an invariant check the harness runs at every sample
// point ... cheap, and it would have caught this class of defect on its own."
//
// The first version of this check this file shipped summed `byteCost` back off
// `manager.resident`/`manager.pending`'s own STORED entries -- which, on reflection,
// is NOT independent at all: before the reconciliation fix (see layer.js's own
// `update()`), a resident entry's stored `byteCost` was never updated either, so it
// agreed with `residentBytes` perfectly while both were equally stale relative to
// what the layer's `plan()` would charge that same key RIGHT NOW. Summing stale data
// against other stale data proves the two stale numbers agree with EACH OTHER, not
// that either is honest -- caught only by re-running this file's own phase 3 by hand
// and noticing `residentMatches: true` even on the UNFIXED code (see this task's own
// report). `trueBytesFromFreshPlan`, below, fixes that: it calls `layer.plan(view)`
// AGAIN, right now, and sums the FRESHLY-PLANNED byteCost for whichever globalKeys
// are currently resident/pending -- never reading a stored `byteCost` back off
// `resident`/`pending` at all (a key `plan()` no longer returns -- dropped out of the
// wanted set entirely -- falls back to its own last-stored cost, since there is no
// fresher number to compare it to once a layer has stopped declaring it). This is
// what makes the check genuinely independent, and what would have caught the manager
// review's own reported defect directly, without already knowing what to look for.
function trueBytesFromFreshPlan(mgr, layer, view) {
  const fresh = new Map();
  for (const r of layer.plan(view)) {
    fresh.set(`${layer.id} ${r.key}`, r.byteCost);
  }
  let residentSum = 0;
  for (const [globalKey, entry] of mgr.resident) {
    residentSum += fresh.has(globalKey) ? fresh.get(globalKey) : entry.byteCost;
  }
  let pendingSum = 0;
  for (const [globalKey, entry] of mgr.pending) {
    pendingSum += fresh.has(globalKey) ? fresh.get(globalKey) : entry.byteCost;
  }
  return { residentSum, pendingSum };
}

/** `residentMatches`/`pendingMatches` (booleans) plus the signed discrepancy in bytes
 * for each -- a `false` here, at ANY sample point, means `manager.residentBytes`/
 * `pendingBytes` disagree with what `layer.plan(view)` would charge those same
 * globalKeys RIGHT NOW -- exactly the shape of bug the manager review's own probe
 * found, caught structurally, without needing to already know what to look for.
 */
function invariantCheck(mgr, layer, view) {
  const truth = trueBytesFromFreshPlan(mgr, layer, view);
  return {
    residentMatches: mgr.residentBytes === truth.residentSum,
    residentDiscrepancyBytes: mgr.residentBytes - truth.residentSum,
    pendingMatches: mgr.pendingBytes === truth.pendingSum,
    pendingDiscrepancyBytes: mgr.pendingBytes - truth.pendingSum,
  };
}

// ------------------------------------------------------------------------- the scenario
// The lead's own measured per-tile cost (question 228) -- a 1024-pixel RGB8 PNG tile,
// the same number `web/js/layers_stream_check.mjs`'s own scale runs declare via
// `tileBytes` (see imagery_layer.js's own module docstring for the 3,147,060 figure).
export const TILE_BYTES = 3_147_060;
// 40 MiB, exactly the lead's own browser-measured budget.
export const MEMORY_BUDGET_BYTES = 40 * 1024 * 1024; // 41,943,040

// 1 root tile, then a plausible quadtree refinement shape: each level roughly
// quadruples its predecessor's tile count but only a fraction of the parent tiles
// actually refine (11, not 16, at level 2; 16, not 44, at level 3) -- the same
// "not every tile refines" shape `globe_lod.js`'s own real `selectTiles` produces
// (only tiles whose screen-space error crosses the threshold recurse). Total 32,
// matching the lead's own measured wanted-set size exactly.
export const LEVEL_SHAPE = [
  { level: 0, count: 1 },
  { level: 1, count: 4 },
  { level: 2, count: 11 },
  { level: 3, count: 16 },
];
const TOTAL_TILE_COUNT = LEVEL_SHAPE.reduce((sum, l) => sum + l.count, 0);
if (TOTAL_TILE_COUNT !== 32) throw new Error(`layers_budget_check: LEVEL_SHAPE must total 32 tiles, got ${TOTAL_TILE_COUNT}`);

const MAX_CONCURRENT_LOADS = 6; // LayerManager's own real default -- see layer.js's constructor doc comment.
// Complete exactly ONE oldest in-flight load between update() calls -- see this file's
// own module docstring for why a batch of >1 would under-sample residentBytes/
// pendingBytes relative to what the brief asks for ("after every load settles").
const COMPLETE_BATCH = 1;
// Safety cap only (module docstring's "no sleep" section) -- generous relative to what
// 32 tiles at 6-way concurrency, one completion at a time, actually need (measured:
// see `settled` in the printed JSON; comfortably under 200 real iterations for either
// phase in practice).
const MAX_ITERATIONS = 5000;

/** Build one phase's 32-request wanted set: `keyPrefix` makes phase 2's keys disjoint
 * from phase 1's (so the manager genuinely treats every phase-1 tile as unwanted the
 * instant phase 2's `view.requests` replaces phase 1's, never accidentally reusing a
 * globalKey). `sseError` is assigned FINER-IS-HIGHER on purpose -- see this file's own
 * module docstring, "The scenario", for why that is what gives this harness teeth.
 * `viewDistanceM` is finer-is-closer (a real camera would be nearer to the tiles it
 * has refined into), which is directly why they *have* a high sseError in the first
 * place -- this is only cosmetic realism, `compareAdmission`'s own level-first rule
 * never looks at it.
 */
function buildTileSet(keyPrefix) {
  const requests = [];
  for (const { level, count } of LEVEL_SHAPE) {
    for (let i = 0; i < count; i += 1) {
      requests.push({
        key: `${keyPrefix}L${level}T${i}`,
        level,
        byteCost: TILE_BYTES,
        sseError: (level + 1) * 100 - i, // finer level => higher band; distinct within a level (see module docstring)
        viewDistanceM: 10_000 / (level + 1) + i, // cosmetic only -- see this function's own doc comment
      });
    }
  }
  return requests;
}

/** A minimal, real `Layer` (./layers/layer.js's interface) whose `plan()` returns
 * exactly `view.requests` -- the harness itself controls "what the view wants" by
 * swapping that array between phases, exactly like web/js/layers_check.mjs's own
 * `probeStarvationDoesNotBlockGoodLayer`'s synthetic layers control demand directly
 * rather than deriving it from a real camera. `load()` is the same FIFO,
 * signal-respecting stub shape as that file's own `makeTiles3DLoaderStub` -- no
 * network, no filesystem, no timer of any kind (question 51/154).
 */
function makeBudgetLayer(id) {
  const inflight = [];
  let invocationCount = 0;
  let loadedCount = 0; // distinct requests that ever actually resolved (see report, below)
  function load(request, signal) {
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
    layer: {
      id,
      plan(view) { return view.requests; },
      load,
      release(_key) {}, // nothing this stub owns needs freeing -- see imagery_layer.js's identical no-op reasoning
    },
    invocationCount: () => invocationCount,
    loadedCount: () => loadedCount,
    pendingCount: () => inflight.length,
    completeOldest(n) {
      const batch = inflight.splice(0, Math.max(0, n));
      for (const entry of batch) {
        loadedCount += 1;
        entry.resolve({ kind: 'budget-check-tile-stub', key: entry.request.key });
      }
      return batch.length;
    },
  };
}

// --------------------------------------------------------------------- the manager
const stub = makeBudgetLayer('budget-imagery');
const manager = new LayerManager({
  memoryBudgetBytes: MEMORY_BUDGET_BYTES, now: fakeNow, maxConcurrentLoads: MAX_CONCURRENT_LOADS,
});
manager.addLayer(stub.layer);

/** Runs `manager.update(view)` / complete-one-oldest / `flushMicrotasks` in a loop
 * (see this file's own module docstring, "What settled means here") until a genuine
 * fixed point or `MAX_ITERATIONS`. Returns every sample (`residentBytes`/
 * `pendingBytes` immediately after each `update()` AND immediately after each
 * completed load's bookkeeping has run) plus whether a real fixed point was reached.
 */
async function runUntilSettled(view) {
  const samples = [];
  let iteration = 0;
  let settled = false;
  function sample(event) {
    samples.push({
      iteration,
      event,
      residentBytes: manager.residentBytes,
      pendingBytes: manager.pendingBytes,
      residentPlusPendingBytes: manager.residentBytes + manager.pendingBytes,
      // Manager review follow-up: the independent invariant check, at THIS sample
      // point -- see `invariantCheck`'s own doc comment.
      invariant: invariantCheck(manager, stub.layer, view),
    });
  }
  while (!settled && iteration < MAX_ITERATIONS) {
    iteration += 1;
    manager.update(view);
    sample('afterUpdate');
    if (manager.pending.size > 0) {
      stub.completeOldest(COMPLETE_BATCH);
      await flushMicrotasks(); // lets the completed load's .then() (_onLoaded) run -- see module docstring
      sample('afterSettle');
    } else {
      // Nothing in flight, and this update() call (which always tries admission
      // first) admitted nothing new either -- a genuine fixed point for this STATIC
      // view (see module docstring: nothing pending means no future free-slot event,
      // and every remaining non-resident request was just re-tried and deferred).
      settled = true;
    }
  }
  return { samples, iterations: iteration, settled };
}

// ------------------------------------------------------------------------- phase 1
const phase1Requests = buildTileSet('P1');
const phase1View = { requests: phase1Requests };
const softViolationBeforePhase1 = manager.softViolationCount;
const deferredBeforePhase1 = manager.deferredCount;
const evictedBeforePhase1 = manager.evictedCount;
const phase1 = await runUntilSettled(phase1View);
const phase1SoftViolationDelta = manager.softViolationCount - softViolationBeforePhase1;
const phase1DeferredDelta = manager.deferredCount - deferredBeforePhase1;
const phase1EvictedDelta = manager.evictedCount - evictedBeforePhase1;

/** Per-level {resident, wanted} histogram at the CURRENT instant (called once phase 1
 * -- or phase 2 -- has settled) -- `wanted` from the request list itself (never
 * recomputed some other way), `resident` by cross-referencing `manager.resident`'s own
 * keys against each request's `globalKey` (`globalKeyFor`, mirrored here rather than
 * imported, since this harness only has `layer.id`/`request.key`, exactly what a real
 * caller has -- see layer.js's own `globalKeyFor`, which this harness deliberately
 * does not import so this check is not merely reading the same computation back).
 */
function levelHistogram(requests, layerId) {
  const byLevel = new Map();
  for (const r of requests) {
    if (!byLevel.has(r.level)) byLevel.set(r.level, { level: r.level, wanted: 0, resident: 0 });
    const bucket = byLevel.get(r.level);
    bucket.wanted += 1;
    const globalKey = `${layerId} ${r.key}`; // globalKeyFor's own join rule, restated (see layer.js)
    if (manager.resident.has(globalKey)) bucket.resident += 1;
  }
  return [...byLevel.values()].sort((a, b) => a.level - b.level);
}

/** THE assertion this harness exists to make fail if it is false (module docstring
 * requirement, restated precisely): walking levels ascending (coarsest first), once
 * any level is found that is NOT fully resident (partial -- some but not all of its
 * wanted tiles loaded -- or entirely empty), EVERY finer level must have ZERO
 * resident tiles. Equivalently: there is no level L and a finer level L' > L such
 * that L' has a resident tile while L is not fully saturated -- "no fine tile is
 * resident while a coarse tile from the same wanted set is not." A single pass,
 * O(levels): once `sawIncomplete` flips true, any further `resident > 0` fails it
 * immediately.
 */
function coarseBeforeFineHolds(histogram) {
  let sawIncomplete = false;
  for (const bucket of histogram) {
    if (sawIncomplete && bucket.resident > 0) return false;
    if (bucket.resident < bucket.wanted) sawIncomplete = true;
  }
  return true;
}

const phase1Histogram = levelHistogram(phase1Requests, stub.layer.id);
const phase1CoarseBeforeFineOk = coarseBeforeFineHolds(phase1Histogram);
const phase1MaxResident = phase1.samples.reduce((m, s) => (s.residentBytes > m.value ? { value: s.residentBytes, iteration: s.iteration, event: s.event } : m), { value: -Infinity, iteration: null, event: null });
const phase1MaxResidentPlusPending = phase1.samples.reduce((m, s) => (s.residentPlusPendingBytes > m.value ? { value: s.residentPlusPendingBytes, iteration: s.iteration, event: s.event } : m), { value: -Infinity, iteration: null, event: null });
const phase1InvariantHeldEveryStep = phase1.samples.every((s) => s.invariant.residentMatches && s.invariant.pendingMatches);

// ------------------------------------------------------------------------- phase 2
// A camera move: an entirely disjoint 32-tile wanted set (same shape, same byte
// cost), so every phase-1 tile is now unwanted -- see this file's own module
// docstring, "Two phases", for exactly what this proves (deliverable 3, deadlock
// avoidance) and why it is not deadlocked by construction.
const phase2Requests = buildTileSet('P2');
const phase2View = { requests: phase2Requests };
const softViolationBeforePhase2 = manager.softViolationCount;
const deferredBeforePhase2 = manager.deferredCount;
const evictedBeforePhase2 = manager.evictedCount;
const phase2 = await runUntilSettled(phase2View);
const phase2SoftViolationDelta = manager.softViolationCount - softViolationBeforePhase2;
const phase2DeferredDelta = manager.deferredCount - deferredBeforePhase2;
const phase2EvictedDelta = manager.evictedCount - evictedBeforePhase2;

const phase2Histogram = levelHistogram(phase2Requests, stub.layer.id);
const phase2CoarseBeforeFineOk = coarseBeforeFineHolds(phase2Histogram);
// Every phase-1 tile must be gone from `resident` by the time phase 2 has settled --
// the concrete, countable form of "no deadlock": the old wanted set's own resident
// footprint reached exactly zero, not merely "the new one grew somehow".
const phase1TilesStillResidentAfterPhase2 = phase1Requests.filter(
  (r) => manager.resident.has(`${stub.layer.id} ${r.key}`),
).length;
const phase2MaxResident = phase2.samples.reduce((m, s) => (s.residentBytes > m.value ? { value: s.residentBytes, iteration: s.iteration, event: s.event } : m), { value: -Infinity, iteration: null, event: null });
const phase2MaxResidentPlusPending = phase2.samples.reduce((m, s) => (s.residentPlusPendingBytes > m.value ? { value: s.residentPlusPendingBytes, iteration: s.iteration, event: s.event } : m), { value: -Infinity, iteration: null, event: null });
const phase2InvariantHeldEveryStep = phase2.samples.every((s) => s.invariant.residentMatches && s.invariant.pendingMatches);

// ------------------------------------------------------------------------- phase 3
// Manager review of round 4's own admission fix: a resident entry's `byteCost` was
// captured once, at admission (`_onLoaded`), and never reconciled afterward --
// `update()`'s own LRU-refresh branch touched only `lastUsedStep`. That is exactly
// the window `GatewayImageryLayerAdapter.fetchManifest()` (round 4's own manifest
// fix) opens: `plan()` charges the constructor's FALLBACK estimate until the
// manifest resolves, then the tile's real, usually much larger, manifest-declared
// size for the SAME globalKey -- any tile admitted during that window kept the
// stale, smaller estimate forever, defeating the hard admission invariant silently
// (the manager review's own probe: 20 tiles admitted at a 262,144-byte estimate,
// 5,242,880 bytes accounted; true cost 3,147,060 bytes each, 62,941,200 bytes, 50%
// over a 41,943,040-byte budget; `residentBytes` stayed at 5,242,880 and
// `softViolationCount` stayed 0 throughout).
//
// This phase reproduces that mid-run cost change directly -- no real manifest fetch
// (this harness has no network at all, question 154/51) -- with its OWN isolated
// manager/layer (never touching phase 1/2's own `manager`/`stub`), whose `plan()`
// charges one of two fixed numbers depending on a harness-controlled `costMode`,
// flipped between 'estimate' and 'true' exactly the instant a real
// `fetchManifest()` would resolve.
//
// Two sub-phases:
//   - 3a (RECOVERABLE, 10 tiles): the revised total (31,470,600 bytes) still fits
//     the budget -- reconciliation alone is enough, no eviction is ever needed.
//     Proves the arithmetic itself is exact: `residentBytes` changes by EXACTLY the
//     declared delta, and `byteCostRevisionCount`/`byteCostRevisionBytes` record it.
//   - 3b (IRRECONCILABLE, 20 tiles -- the manager review's own exact probe shape):
//     every revised tile STAYS wanted, and their combined true cost (62,941,200
//     bytes) alone exceeds the budget -- there is nothing UNWANTED to evict, so no
//     implementation can bring `residentBytes` back under budget without evicting
//     something the view still wants, which this manager correctly refuses to do
//     (the same design constraint that made the ORIGINAL, admission-time violation
//     this task's main fix already closed a genuine soft violation, not a bug to
//     paper over). The fix's job here is not to make an impossible budget possible
//     -- it is to stop LYING about it: `residentBytes` must become EXACTLY the true
//     sum (the independent invariant check, above, must hold even here), the
//     revision must be counted, and `softViolationCount` -- which the manager
//     review's own probe found silently reading 0 while the truth was 50% over
//     budget -- must become nonzero, honestly.
const RECONCILE_ESTIMATE_BYTES = 262_144; // IMAGERY_TILE_BYTES -- this codebase's own fallback shape
const RECONCILE_TRUE_BYTES = TILE_BYTES; // the lead's own measured real tile cost, reused from the scenario above

/** A `Layer` whose `plan()` charges one of two fixed per-tile costs depending on
 * `costMode` (harness-controlled, via `setCostMode`) -- everything else is the same
 * FIFO, signal-respecting `load()` stub shape as `makeBudgetLayer`, above. */
function makeReconcileLayer(id) {
  let costMode = 'estimate';
  const inflight = [];
  function load(request, signal) {
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
    layer: {
      id,
      plan(view) {
        const cost = costMode === 'true' ? RECONCILE_TRUE_BYTES : RECONCILE_ESTIMATE_BYTES;
        return view.requests.map((r) => ({ ...r, byteCost: cost }));
      },
      load,
      release(_key) {},
    },
    setCostMode(mode) { costMode = mode; },
    completeOldest(n) {
      const batch = inflight.splice(0, Math.max(0, n));
      for (const entry of batch) entry.resolve({ kind: 'reconcile-stub', key: entry.request.key });
      return batch.length;
    },
    pendingCount: () => inflight.length,
  };
}

/** Same "call update(), complete one oldest if anything is pending, else stop"
 * fixed-point loop as `runUntilSettled`, above, parameterized so phase 3's own
 * isolated manager/stub can reuse the identical discipline without touching phase
 * 1/2's module-level `manager`/`stub` at all. */
async function drainToSettled(mgr, stubLike, view) {
  let iteration = 0;
  let settled = false;
  while (!settled && iteration < MAX_ITERATIONS) {
    iteration += 1;
    mgr.update(view);
    if (mgr.pending.size > 0) {
      stubLike.completeOldest(COMPLETE_BATCH);
      await flushMicrotasks();
    } else {
      settled = true;
    }
  }
  return { iterations: iteration, settled };
}

async function runReconcilePhase(label, tileCount) {
  const stubR = makeReconcileLayer(`reconcile-${label}`);
  const mgr = new LayerManager({ memoryBudgetBytes: MEMORY_BUDGET_BYTES, now: fakeNow, maxConcurrentLoads: MAX_CONCURRENT_LOADS });
  mgr.addLayer(stubR.layer);
  const requests = Array.from({ length: tileCount }, (_, i) => ({
    key: `${label}T${i}`, level: 0, sseError: 100 - i, viewDistanceM: 1000 + i,
  }));
  const view = { requests };

  // Step 1: admit everything at the FALLBACK estimate, to a real fixed point.
  const before = await drainToSettled(mgr, stubR, view);
  const beforeSnapshot = {
    residentCount: mgr.resident.size,
    accountedResidentBytes: mgr.residentBytes,
    trueResidentBytes: trueBytesFromFreshPlan(mgr, stubR.layer, view).residentSum,
    invariant: invariantCheck(mgr, stubR.layer, view),
  };

  // Step 2: flip to the TRUE cost for the SAME keys -- exactly what a resolved
  // `fetchManifest()` does to `GatewayImageryLayerAdapter.plan()`'s own output --
  // then call `update()` ONCE: this single call is the reconciliation moment itself.
  stubR.setCostMode('true');
  const softViolationBeforeFlip = mgr.softViolationCount;
  const revisionCountBeforeFlip = typeof mgr.byteCostRevisionCount === 'number' ? mgr.byteCostRevisionCount : null;
  mgr.update(view);
  const afterOneUpdateSnapshot = {
    residentCount: mgr.resident.size,
    accountedResidentBytes: mgr.residentBytes,
    trueResidentBytes: trueBytesFromFreshPlan(mgr, stubR.layer, view).residentSum,
    invariant: invariantCheck(mgr, stubR.layer, view),
    softViolationCount: mgr.softViolationCount,
  };

  // Step 3: drain to a fixed point again, so any newly-admittable request this
  // reconciliation happened to free room for (3a's own headroom case) actually
  // settles before this phase's own final snapshot is taken.
  const after = await drainToSettled(mgr, stubR, view);

  return {
    label,
    tileCount,
    estimateBytes: RECONCILE_ESTIMATE_BYTES,
    trueBytes: RECONCILE_TRUE_BYTES,
    totalTrueBytes: tileCount * RECONCILE_TRUE_BYTES,
    settledBefore: before.settled,
    before: beforeSnapshot,
    afterOneUpdate: afterOneUpdateSnapshot,
    settledAfter: after.settled,
    after: {
      residentCount: mgr.resident.size,
      accountedResidentBytes: mgr.residentBytes,
      trueResidentBytes: trueBytesFromFreshPlan(mgr, stubR.layer, view).residentSum,
      invariant: invariantCheck(mgr, stubR.layer, view),
    },
    byteCostRevisionCount: typeof mgr.byteCostRevisionCount === 'number' ? mgr.byteCostRevisionCount : null,
    byteCostRevisionCountDelta: typeof mgr.byteCostRevisionCount === 'number' && revisionCountBeforeFlip !== null ? mgr.byteCostRevisionCount - revisionCountBeforeFlip : null,
    byteCostRevisionBytes: typeof mgr.byteCostRevisionBytes === 'number' ? mgr.byteCostRevisionBytes : null,
    softViolationCountDelta: mgr.softViolationCount - softViolationBeforeFlip,
    evictedCount: mgr.evictedCount,
  };
}

// 3a: 10 * 3,147,060 = 31,470,600 <= 41,943,040 -- recoverable by reconciliation alone.
const phase3Recoverable = await runReconcilePhase('R3a', 10);
// 3b: 20 * 3,147,060 = 62,941,200 > 41,943,040, everything still wanted -- irreconcilable, must honestly soft-violate.
const phase3Irreconcilable = await runReconcilePhase('R3b', 20);

// --------------------------------------------------------------------- overall (both phases)
// The REQUIRED top-level numbers (see this file's own module docstring and tests/
// test_viewer_layers_budget.py) are computed over the WHOLE run (both phases
// combined) -- strictly the more rigorous reading of "at every step": the invariant
// (residentBytes + pendingBytes <= memoryBudgetBytes) is claimed to hold at every
// point this manager's own code runs, not just during the one scenario the lead
// literally measured, so this harness checks it across both.
const allSamples = [
  ...phase1.samples.map((s) => ({ ...s, phase: 1 })),
  ...phase2.samples.map((s) => ({ ...s, phase: 2 })),
];
const overallMaxResident = allSamples.reduce(
  (m, s) => (s.residentBytes > m.value ? { value: s.residentBytes, phase: s.phase, iteration: s.iteration, event: s.event } : m),
  { value: -Infinity, phase: null, iteration: null, event: null },
);
const overallMaxResidentPlusPending = allSamples.reduce(
  (m, s) => (s.residentPlusPendingBytes > m.value ? { value: s.residentPlusPendingBytes, phase: s.phase, iteration: s.iteration, event: s.event } : m),
  { value: -Infinity, phase: null, iteration: null, event: null },
);

const result = {
  scenario: {
    tileBytes: TILE_BYTES,
    memoryBudgetBytes: MEMORY_BUDGET_BYTES,
    levelShape: LEVEL_SHAPE,
    totalTileCount: TOTAL_TILE_COUNT,
    totalWantedBytes: TOTAL_TILE_COUNT * TILE_BYTES,
    wantedOverBudgetRatio: (TOTAL_TILE_COUNT * TILE_BYTES) / MEMORY_BUDGET_BYTES,
    maxConcurrentLoads: MAX_CONCURRENT_LOADS,
    completeBatch: COMPLETE_BATCH,
    maxIterations: MAX_ITERATIONS,
  },
  // -------- REQUIRED (tests/test_viewer_layers_budget.py asserts directly on these) --------
  maxResidentBytes: overallMaxResident.value,
  maxResidentBytesAt: { phase: overallMaxResident.phase, iteration: overallMaxResident.iteration, event: overallMaxResident.event },
  maxResidentPlusPendingBytes: overallMaxResidentPlusPending.value,
  maxResidentPlusPendingBytesAt: { phase: overallMaxResidentPlusPending.phase, iteration: overallMaxResidentPlusPending.iteration, event: overallMaxResidentPlusPending.event },
  softViolationCount: manager.softViolationCount,
  deferredCount: manager.deferredCount,
  lastStepDeferred: manager.lastStepDeferred,
  // Manager review follow-up: reported unconditionally too -- see layer.js's
  // constructor doc comment. Phase 1/2's own layer never changes a request's
  // declared byteCost between steps, so both are expected exactly 0 for THIS
  // manager -- phase 3 (below) uses its own, separate, isolated manager
  // specifically so these two top-level numbers stay a clean statement about
  // phase 1/2 alone.
  byteCostRevisionCount: manager.byteCostRevisionCount,
  byteCostRevisionBytes: manager.byteCostRevisionBytes,
  // Manager review follow-up: true at every sample point in phase 1 AND phase 2 --
  // see `invariantCheck`'s own doc comment. A `false` anywhere would mean
  // `residentBytes`/`pendingBytes` have drifted from the true sum over
  // `resident`/`pending`.
  invariantHeldEveryStep: phase1InvariantHeldEveryStep && phase2InvariantHeldEveryStep,
  // Phase 1 IS the lead's own literal measured scenario -- its own histogram/
  // coarse-before-fine result is the primary, required proof.
  phase1: {
    settled: phase1.settled,
    iterations: phase1.iterations,
    softViolationCount: phase1SoftViolationDelta,
    deferredCount: phase1DeferredDelta,
    evictedCount: phase1EvictedDelta,
    maxResidentBytes: phase1MaxResident.value,
    maxResidentBytesAt: { iteration: phase1MaxResident.iteration, event: phase1MaxResident.event },
    maxResidentPlusPendingBytes: phase1MaxResidentPlusPending.value,
    maxResidentPlusPendingBytesAt: { iteration: phase1MaxResidentPlusPending.iteration, event: phase1MaxResidentPlusPending.event },
    residentCount: manager.countsByLayer()[stub.layer.id].resident,
    levelHistogram: phase1Histogram,
    coarseBeforeFineOk: phase1CoarseBeforeFineOk,
    invariantHeldEveryStep: phase1InvariantHeldEveryStep,
  },
  // Phase 2 (additional -- deliverable 3's own deadlock-avoidance proof, see module
  // docstring "Two phases"): not required by the letter of the brief's scenario, but
  // directly exercises the make-room-for-admission eviction path a hard limit alone
  // would deadlock without.
  phase2CameraMove: {
    settled: phase2.settled,
    iterations: phase2.iterations,
    softViolationCount: phase2SoftViolationDelta,
    deferredCount: phase2DeferredDelta,
    evictedCount: phase2EvictedDelta,
    maxResidentBytes: phase2MaxResident.value,
    maxResidentBytesAt: { iteration: phase2MaxResident.iteration, event: phase2MaxResident.event },
    maxResidentPlusPendingBytes: phase2MaxResidentPlusPending.value,
    maxResidentPlusPendingBytesAt: { iteration: phase2MaxResidentPlusPending.iteration, event: phase2MaxResidentPlusPending.event },
    residentCount: manager.countsByLayer()[stub.layer.id].resident,
    levelHistogram: phase2Histogram,
    coarseBeforeFineOk: phase2CoarseBeforeFineOk,
    phase1TilesStillResidentAfterPhase2, // must be 0 -- see this section's own comment above
    invariantHeldEveryStep: phase2InvariantHeldEveryStep,
  },
  // Manager review follow-up (see this file's own "phase 3" section, above, for the
  // full scenario): its own isolated manager, never mixed with phase 1/2's.
  phase3Reconcile: {
    recoverable: phase3Recoverable,
    irreconcilable: phase3Irreconcilable,
  },
  // Distinct tiles ever loaded/cancelled/evicted/failed, over the WHOLE run (module
  // docstring's own required report) -- cancelledCount/failedCount are expected 0
  // (this harness never lets a request outlive the SAME static wanted set it was
  // admitted under mid-phase, and this stub's load() never rejects), reported
  // unconditionally anyway, exactly like every other harness in this directory
  // reports softViolationCount/failedCount unconditionally even when zero.
  loadedCount: stub.loadedCount(),
  invocationCount: stub.invocationCount(),
  cancelledCount: manager.cancelledCount,
  evictedCount: manager.evictedCount,
  failedCount: manager.failedCount,
  failureNames: manager.failureNames(),
  wantedTiles: {
    phase1: phase1Requests.map((r) => ({ key: r.key, level: r.level, byteCost: r.byteCost })),
    phase2: phase2Requests.map((r) => ({ key: r.key, level: r.level, byteCost: r.byteCost })),
  },
};

process.stdout.write(JSON.stringify(result));

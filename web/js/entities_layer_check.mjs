// CLI harness for tests/test_entities_layer.py: `node web/js/entities_layer_check.mjs`.
//
// Proves web/js/entities/entities_instanced_layer.js's `MarkerLayerAdapter`/
// `TrailLayerAdapter` genuinely go through `web/js/layers/layer.js`'s `LayerManager`
// discipline -- "budgeted like the layers... not a simile" (this task's brief) -- using
// the SAME real `LayerManager` class `web/js/layers_budget_check.mjs` already proves
// for imagery, not a second, parallel budget mechanism. No network, no filesystem I/O,
// no real clock (design constraint g -- a plain incrementing counter, identical to
// every other harness in this directory); the only ordering barrier is
// `flushMicrotasks` (a `setImmediate` round trip), never a timer duration.
//
// What is measured:
//   - hardBudgetRespected: `residentBytes + pendingBytes <= memoryBudgetBytes` at every
//     sampled step, for a wanted set (20 markers + 4 trails, 6,384 declared bytes)
//     2.13x a deliberately small 3,000-byte budget.
//   - softViolationCountZero: never fires for this scenario (nothing here ever revises
//     an already-resident byteCost -- that is `web/js/layers_budget_check.mjs`'s own
//     phase 3 job, not this file's).
//   - deferredCountPositive: some request is always deferred (the wanted set alone
//     exceeds the budget, nothing evictable).
//   - cancellationReal: an admission (`pending`, real, loader invoked) is dropped by a
//     synthetic "camera move" (the wanted set shrinks) BEFORE its microtask-deferred
//     load settles -- `LayerManager.update()`'s own synchronous cancellation loop
//     catches it, `cancelledCount` moves, checked directly, not inferred.
//   - sceneGraphMatchesResident: `buildMarkerInstancedMesh`/`buildTrailGroup` produce a
//     REAL `THREE.InstancedMesh`/`THREE.Line` set whose per-instance transforms/colors
//     and line vertex positions are read back and compared against the ORIGINAL input
//     marker/trail data (never against the manager's own bookkeeping) -- "assert from
//     the scene graph, never a counter alone" (this round's rule).
import * as THREE from 'three';
import { LayerManager } from './layers/index.js';
import {
  MarkerLayerAdapter, TrailLayerAdapter, MARKER_INSTANCE_BYTES, TRAIL_POINT_BYTES,
  TRAIL_FIXED_OVERHEAD_BYTES, buildMarkerInstancedMesh, buildTrailGroup,
} from './entities/entities_instanced_layer.js';

let fakeNowCounter = 0;
function fakeNow() { fakeNowCounter += 1; return fakeNowCounter; }
function flushMicrotasks() { return new Promise((resolve) => { setImmediate(resolve); }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }

// ------------------------------------------------------------------------- scenario
const MARKER_COUNT = 20;
const TRAIL_COUNT = 4;
const TRAIL_POINTS = 80;
const MEMORY_BUDGET_BYTES = 3000;
const MAX_CONCURRENT_LOADS = 4;

function buildMarkers() {
  return Array.from({ length: MARKER_COUNT }, (_, i) => ({
    id: `m${i}`,
    positionKm: [i * 0.1, i * 0.2, -i * 0.05],
    color: '#ff9f43',
    sseError: MARKER_COUNT - i, // deterministic, distinct priority order
    viewDistanceM: 1000 + i,
  }));
}
function buildTrails() {
  return Array.from({ length: TRAIL_COUNT }, (_, i) => ({
    id: `tr${i}`,
    pointsKm: Array.from({ length: TRAIL_POINTS }, (_, p) => [p * 0.01 + i, p * 0.02, p * 0.005 - i]),
    color: '#54a0ff',
    sseError: 50 - i,
    viewDistanceM: 2000 + i,
  }));
}

// ------------------------------------------------------------- adapter-level signal proof
// Isolated from LayerManager entirely: `MarkerLayerAdapter.load()`/`TrailLayerAdapter.
// load()` must themselves genuinely reject with the abort reason -- (a) when the signal
// is ALREADY aborted before `load()` is even called, and (b) when it aborts AFTER
// `load()` starts but BEFORE its microtask-deferred construction resolves. Awaited and
// caught directly (`./layer.js`'s own interface contract: "Must respect signal...
// reject once it aborts"), never inferred from `LayerManager`'s own `cancelledCount`
// alone -- that counter moves purely from `update()`'s own bookkeeping (`p.controller.
// abort()`) regardless of whether an adapter's `load()` itself honours the signal, so
// it cannot by itself distinguish an adapter that ignores `signal` from one that
// doesn't. This is that missing, adapter-own-behaviour half of the proof.
async function proveSignalHonoured(adapter, request) {
  const alreadyAborted = new AbortController();
  alreadyAborted.abort(new DOMException('already aborted', 'AbortError'));
  let rejectedWhenPreAborted = false;
  try { await adapter.load(request, alreadyAborted.signal); } catch (e) { rejectedWhenPreAborted = e === alreadyAborted.signal.reason; }

  const midFlight = new AbortController();
  const p = adapter.load(request, midFlight.signal).catch((e) => e);
  midFlight.abort(new DOMException('mid-flight abort', 'AbortError'));
  const settled = await p;
  const rejectedMidFlight = settled === midFlight.signal.reason;

  return { rejectedWhenPreAborted, rejectedMidFlight };
}
const markerAdapter = new MarkerLayerAdapter();
const trailAdapter = new TrailLayerAdapter();
const markerSignalProof = await proveSignalHonoured(markerAdapter, {
  key: 'signal-probe-marker', marker: { positionKm: [1, 2, 3], color: '#fff' },
});
const trailSignalProof = await proveSignalHonoured(trailAdapter, {
  key: 'signal-probe-trail', trail: { pointsKm: [[0, 0, 0], [1, 1, 1]], color: '#fff' },
});
check('markerAdapter_loadRejectsWhenPreAborted', markerSignalProof.rejectedWhenPreAborted, markerSignalProof);
check('markerAdapter_loadRejectsMidFlight', markerSignalProof.rejectedMidFlight, markerSignalProof);
check('trailAdapter_loadRejectsWhenPreAborted', trailSignalProof.rejectedWhenPreAborted, trailSignalProof);
check('trailAdapter_loadRejectsMidFlight', trailSignalProof.rejectedMidFlight, trailSignalProof);
const manager = new LayerManager({
  memoryBudgetBytes: MEMORY_BUDGET_BYTES, now: fakeNow, maxConcurrentLoads: MAX_CONCURRENT_LOADS,
});
manager.addLayer(markerAdapter);
manager.addLayer(trailAdapter);

const allMarkers = buildMarkers();
const allTrails = buildTrails();
const totalDeclaredBytes = allMarkers.length * MARKER_INSTANCE_BYTES
  + allTrails.reduce((s, t) => s + TRAIL_FIXED_OVERHEAD_BYTES + t.pointsKm.length * TRAIL_POINT_BYTES, 0);

const view1 = { markers: allMarkers, trails: allTrails };

// ------------------------------------------------------------------- hard-budget phase
const samples = [];
function sample(tag) {
  samples.push({
    tag,
    residentBytes: manager.residentBytes,
    pendingBytes: manager.pendingBytes,
    residentPlusPendingBytes: manager.residentBytes + manager.pendingBytes,
    softViolationCount: manager.softViolationCount,
    deferredCount: manager.deferredCount,
  });
}

manager.update(view1);
sample('afterFirstUpdate');
// The FIRST update() has admitted up to MAX_CONCURRENT_LOADS requests into `pending`
// (real, real AbortController, not yet resolved -- MarkerLayerAdapter/TrailLayerAdapter's
// own `load()` resolves one microtask later, see entities_instanced_layer.js's own
// `microtaskLoad`). Capture exactly which globalKeys are pending RIGHT NOW, before
// anything has a chance to settle.
const pendingAfterFirstUpdate = [...manager.pending.keys()];
check('firstUpdateAdmittedSomethingIntoPending', pendingAfterFirstUpdate.length > 0, { count: pendingAfterFirstUpdate.length });

// ------------------------------------------------------------------- cancellation proof
// "Camera move": drop every currently-PENDING marker/trail from the next wanted set --
// LayerManager.update()'s own cancellation loop (layer.js) aborts anything in `pending`
// that is no longer wanted, SYNCHRONOUSLY, inside this very call -- before the
// microtask-deferred load() a moment ago even had a chance to resolve.
const pendingMarkerIds = new Set(
  [...manager.pending.values()].filter((p) => p.layerId === markerAdapter.id).map((p) => p.localKey),
);
const pendingTrailIds = new Set(
  [...manager.pending.values()].filter((p) => p.layerId === trailAdapter.id).map((p) => p.localKey),
);
const view2 = {
  markers: allMarkers.filter((m) => !pendingMarkerIds.has(m.id)),
  trails: allTrails.filter((t) => !pendingTrailIds.has(t.id)),
};
const cancelledBefore = manager.cancelledCount;
manager.update(view2);
const cancelledAfter = manager.cancelledCount;
const cancelledDelta = cancelledAfter - cancelledBefore;
check('cancellationReal_droppedPendingRequestsWereCancelled', cancelledDelta === (pendingMarkerIds.size + pendingTrailIds.size), {
  cancelledDelta, expectedCancellations: pendingMarkerIds.size + pendingTrailIds.size, pendingMarkerIds: [...pendingMarkerIds], pendingTrailIds: [...pendingTrailIds],
});
// Every one of those dropped keys must be genuinely gone from `pending` (not merely
// counted) -- checked directly against the manager's own live Map, not against the
// counter just asserted above.
const stillPendingAfterCancel = pendingAfterFirstUpdate.filter((k) => manager.pending.has(k));
check('cancellationReal_cancelledKeysActuallyRemovedFromPending', stillPendingAfterCancel.length === 0, { stillPendingAfterCancel });

// ------------------------------------------------------------------- drain view2 to steady state
sample('afterCancelUpdate');
for (let i = 0; i < 30; i++) {
  manager.update(view2);
  // eslint-disable-next-line no-await-in-loop
  await flushMicrotasks();
  sample(`drain${i}`);
}

const hardBudgetRespected = samples.every((s) => s.residentPlusPendingBytes <= MEMORY_BUDGET_BYTES);
const softViolationCountZero = manager.softViolationCount === 0;
const deferredCountPositive = manager.deferredCount > 0;
check('hardBudgetRespectedEveryStep', hardBudgetRespected, {
  maxResidentPlusPending: Math.max(...samples.map((s) => s.residentPlusPendingBytes)), memoryBudgetBytes: MEMORY_BUDGET_BYTES,
});
check('softViolationCountZero', softViolationCountZero, { softViolationCount: manager.softViolationCount });
check('deferredCountPositive', deferredCountPositive, { deferredCount: manager.deferredCount, totalDeclaredBytes, memoryBudgetBytes: MEMORY_BUDGET_BYTES });

// ------------------------------------------------------------------- scene-graph assertions
const geometry = new THREE.SphereGeometry(0.01, 6, 6);
const markerMesh = buildMarkerInstancedMesh(geometry, manager, markerAdapter.id);
const residentMarkerCount = manager.countsByLayer()[markerAdapter.id].resident;
check('sceneGraph_markerMeshIsRealInstancedMesh', markerMesh.isInstancedMesh === true, {});
check('sceneGraph_markerMeshCountMatchesResident', markerMesh.count === residentMarkerCount, { meshCount: markerMesh.count, residentMarkerCount });

// For every resident marker, the mesh's OWN per-instance matrix (read back via
// getMatrixAt, decomposed) must reproduce that marker's ORIGINAL positionKm -- ground
// truth is the input data this harness built, never the manager's own payload.
let markerTransformsOk = true;
const markerTransformDetail = [];
{
  const m = new THREE.Matrix4();
  const pos = new THREE.Vector3();
  const q = new THREE.Quaternion();
  const scl = new THREE.Vector3();
  for (let i = 0; i < markerMesh.count; i++) {
    markerMesh.getMatrixAt(i, m);
    m.decompose(pos, q, scl);
    const localKey = markerMesh.userData.residentKeys[i];
    const original = allMarkers.find((mk) => mk.id === localKey);
    const ok = original && approxEqual(pos.x, original.positionKm[0], 1e-6)
      && approxEqual(pos.y, original.positionKm[1], 1e-6) && approxEqual(pos.z, original.positionKm[2], 1e-6);
    if (!ok) markerTransformsOk = false;
    markerTransformDetail.push({ localKey, got: [pos.x, pos.y, pos.z], expected: original && original.positionKm });
  }
}
check('sceneGraph_markerInstanceTransformsMatchOriginalPositions', markerTransformsOk, { sample: markerTransformDetail.slice(0, 3), count: markerTransformDetail.length });

const trailGroup = buildTrailGroup(manager, trailAdapter.id);
const residentTrailCount = manager.countsByLayer()[trailAdapter.id].resident;
check('sceneGraph_trailGroupChildCountMatchesResident', trailGroup.children.length === residentTrailCount, {
  childCount: trailGroup.children.length, residentTrailCount,
});
let trailPointsOk = trailGroup.children.length > 0;
const trailDetail = [];
for (const [lineIndex, line] of trailGroup.children.entries()) {
  // Manager review, round 6: indexed by position in the group, not by
  // `line.userData.id` -- buildTrailGroup does not put an id on the object (the
  // comment a few lines below says so in as many words), so that template collapsed
  // every trail's check to the SAME name. Two checks sharing a name make a failure
  // ambiguous and silently collapse under any name-keyed lookup, which is how this
  // file's own pytest reads its results.
  check(`sceneGraph_trailIsRealThreeLine_${lineIndex}`, line.isLine === true, {}); // defensive, always true given buildTrailGroup's own construction
  const posAttr = line.geometry.getAttribute('position');
  // Match this line back to its original trail by first-point value (buildTrailGroup
  // attaches the REAL Line the adapter's own load() built; there is no id stored on
  // the object itself, so identify it by content, independent of manager bookkeeping).
  const firstPoint = [posAttr.getX(0), posAttr.getY(0), posAttr.getZ(0)];
  const original = allTrails.find((t) => approxEqual(t.pointsKm[0][0], firstPoint[0], 1e-6)
    && approxEqual(t.pointsKm[0][1], firstPoint[1], 1e-6) && approxEqual(t.pointsKm[0][2], firstPoint[2], 1e-6));
  if (!original) { trailPointsOk = false; continue; }
  if (posAttr.count !== original.pointsKm.length) { trailPointsOk = false; continue; }
  for (let p = 0; p < original.pointsKm.length; p++) {
    const got = [posAttr.getX(p), posAttr.getY(p), posAttr.getZ(p)];
    const exp = original.pointsKm[p];
    // Float32 round-trip tolerance (this Line's geometry is backed by a Float32Array,
    // ../entities/entities_instanced_layer.js's TrailLayerAdapter.load() -- 1e-6 is
    // generous relative to float32's own ~1e-7 relative precision at these magnitudes,
    // never loosened to paper over a real coordinate swap/scale bug, which would be
    // orders of magnitude larger than this).
    if (!got.every((c, k) => approxEqual(c, exp[k], 1e-6))) { trailPointsOk = false; break; }
  }
  trailDetail.push({ trailId: original.id, pointCount: posAttr.count });
}
check('sceneGraph_trailLineVerticesMatchOriginalPoints', trailPointsOk, { trailDetail });

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({
  allPass,
  checks,
  scenario: {
    markerCount: MARKER_COUNT, trailCount: TRAIL_COUNT, trailPoints: TRAIL_POINTS,
    memoryBudgetBytes: MEMORY_BUDGET_BYTES, maxConcurrentLoads: MAX_CONCURRENT_LOADS,
    totalDeclaredBytes, overBudgetRatio: totalDeclaredBytes / MEMORY_BUDGET_BYTES,
  },
  finalCounts: manager.countsByLayer(),
  softViolationCount: manager.softViolationCount,
  deferredCount: manager.deferredCount,
  cancelledCount: manager.cancelledCount,
  residentBytes: manager.residentBytes,
  pendingBytes: manager.pendingBytes,
}));
// Manager review, round 6: a failing check sets a non-zero exit code, the same way
// web/js/layout/layout_tree_check.mjs already does. Without it `node <check>` exits 0
// on a broken build and the round's own gate line ("the node checks at the final head:
// N of N exit 0") is silently meaningless for this file. The JSON still goes to stdout
// either way, and the pytest side still reports the failing check NAMES rather than a
// bare exit code -- see this check's own test file.
if (!allPass) process.exitCode = 1;

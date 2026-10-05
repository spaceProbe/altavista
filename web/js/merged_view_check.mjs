// CLI harness for tests/test_merged_view_check.py: `node web/js/merged_view_check.mjs`.
//
// Heavy cleanup round 1 (docs/open-questions.md questions 235 and 237): the viewer makes
// exactly ONE `LayerManager.update(view)` call per render tick, with `view` merged from
// every participant's fragment (the globe's `tiles`/`cameraEcef`/..., the entities'
// `markers`/`trails`), instead of admitting markers and trails once and never planning
// them again. This file proves the two properties that change buys, headlessly, against
// the SHIPPED code a real `Viewer` runs per tick -- never a mirror of it:
//   - `updateComposed` (web/js/layers/layer.js), the one driver of the shared manager;
//   - a real `GlobeLayer` (web/js/globe.js) in its composer mode (`planView()` +
//     `commitPlannedView()`), registered on a real `LayerManager` with its real
//     `ImageryLayerAdapter`;
//   - the real `MarkerLayerAdapter`/`TrailLayerAdapter` and `ResidentEntityScene`
//     (web/js/entities/entities_instanced_layer.js), the entities' participant and the
//     scene-graph side of their residency.
// (`web/js/scene.js` itself needs a WebGL context, so the real `Viewer` is proved in a
// browser by tests/test_viewer_globe_layer_manager.py; this file's rigs call
// `updateComposed` with the same participant shapes `Viewer._updateLayerManager` does.)
//
// PROOF B -- no imagery load is cancelled by the entity half. Round 7 measured that two
// partial views driving one manager per tick cancel each other's in-flight loads: with a
// globe active no imagery tile ever finished. Here a globe AND entities share one
// manager, the imagery loader holds every load in flight, and the entity half of the view
// changes between ticks; the manager's own records (`cancelledCount`, the `AbortSignal`
// of every imagery load it started, `countsByLayer()`, `resident`) must show zero
// cancellations, and once the loads are released every wanted tile is resident and bound
// to a globe mesh. `B_controlTwoPartialUpdatesCancelImagery` is the teeth: the SAME rig
// driven the old way (a globe-only call, then an entity-only call, each tick) DOES cancel.
//
// PROOF C -- an entity payload evicted under a tight budget comes back. A budget that
// holds six imagery tiles and (just) the entities plus five, a far camera (2 tiles) and a
// near one (20 tiles): the near camera's wanted imagery presses the manager, which evicts
// the entity payloads by its own LRU rule once they stop being wanted (the markers/trails
// classes are switched off), the scene graph then draws none of them, they are re-wanted
// but do not fit while the pressure lasts (deferred, still not drawn), and once the camera
// moves back and fewer tiles are wanted they are re-admitted and drawn again. Residency is
// read from `manager.resident`, the manager's own `release()` call names the evicted keys,
// and the scene graph is read by traversing the groups -- no counter of mine stands in for
// any of them.
//
// Terrain: the rigs unregister the globe's terrain adapter (`manager.removeLayer`). It is
// the permanently-refusing adapter (terrain_layer.js: `load()` always rejects, nothing is
// ever resident) and every wanted terrain tile is admitted for one microtask before it
// fails, which would make the exact byte arithmetic the tight-budget proof rests on
// depend on microtask order; it is not what either proof is about.
import * as THREE from 'three';
import { GlobeLayer } from './globe.js';
import { SCENE_UNITS_PER_METRE, selectTiles, tileKey } from './globe_lod.js';
import { LayerManager, updateComposed, composeView, globalKeyFor } from './layers/index.js';
import { MarkerLayerAdapter, TrailLayerAdapter, ResidentEntityScene } from './entities/index.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }

const SCREEN_H = 900;
const FOV = (50 * Math.PI) / 180;
const MARKER_ID = 'entity-markers';
const TRAIL_ID = 'entity-trails';
const FAR_ECEF_M = { x: 3.2e7, y: 0, z: 0 };            // selectTiles -> the two level-0 roots
const NEAR_ECEF_M = { x: 6900000, y: 500000, z: 800000 }; // LEO altitude -> 20 tiles at maxLevel 2
const toLocal = (c) => ({ x: c.x * SCENE_UNITS_PER_METRE, y: c.y * SCENE_UNITS_PER_METRE, z: c.z * SCENE_UNITS_PER_METRE });
const FAR = toLocal(FAR_ECEF_M);
const NEAR = toLocal(NEAR_ECEF_M);

const ENTITIES = ['Target', 'Chaser', 'Debris'].map((name, i) => ({
  name,
  positionKm: [i, 0, 0],
  color: ['#54a0ff', '#ff6b6b', '#aaaaaa'][i],
  trailPointsKm: [[i, 0, 0], [i + 1, 0, 0], [i + 2, 0, 0], [i + 3, 0, 0], [i + 4, 0, 0]],
}));

const drain = () => new Promise((resolve) => setImmediate(resolve));

/** `.load(url, onLoad, onProgress, onError)` stub (the `THREE.TextureLoader` shape the
 * imagery adapter takes): no I/O. 'auto' settles each load on a microtask; 'hold' keeps
 * it in flight until `releaseAll()`. */
function makeLoader() {
  const held = [];
  return {
    mode: 'auto',
    calls: 0,
    load(url, onLoad) {
      this.calls += 1;
      const tex = { isStubTexture: true, url, userData: {}, dispose() {} };
      if (this.mode === 'auto') queueMicrotask(() => onLoad(tex));
      else held.push(() => onLoad(tex));
    },
    heldCount() { return held.length; },
    releaseAll() { for (const f of held.splice(0)) f(); },
  };
}

/** One shared manager with a real globe + the real entity adapters and scene, driven
 * through `updateComposed` exactly as `Viewer._updateLayerManager` drives it. */
function makeRig({ budgetBytes, imageryTileBytes, maxConcurrentLoads = 64 }) {
  const manager = new LayerManager({ memoryBudgetBytes: budgetBytes, maxConcurrentLoads });
  const loader = makeLoader();
  const globe = new GlobeLayer({ layerManager: manager, textureLoader: loader, imageryTileBytes });
  manager.removeLayer('terrain'); // see this file's module docstring
  const markerAdapter = new MarkerLayerAdapter({ id: MARKER_ID });
  const trailAdapter = new TrailLayerAdapter({ id: TRAIL_ID });
  manager.addLayer(markerAdapter);
  manager.addLayer(trailAdapter);

  // Observation hooks on the manager's own calls: which entity payloads it released
  // (evicted), how many entity loads it started, and every imagery load's AbortSignal.
  const released = [];
  const entityLoads = [];
  const imagerySignals = [];
  for (const adapter of [markerAdapter, trailAdapter]) {
    const origRelease = adapter.release.bind(adapter);
    adapter.release = (key) => { released.push(globalKeyFor(adapter.id, key)); origRelease(key); };
    const origLoad = adapter.load.bind(adapter);
    adapter.load = (request, signal) => { entityLoads.push(globalKeyFor(adapter.id, request.key)); return origLoad(request, signal); };
  }
  const imageryAdapter = manager.imageryLayers().find((l) => l.id === 'imagery');
  const origImageryLoad = imageryAdapter.load.bind(imageryAdapter);
  imageryAdapter.load = (request, signal) => { imagerySignals.push(signal); return origImageryLoad(request, signal); };

  let updateCalls = 0;
  const origUpdate = manager.update.bind(manager);
  manager.update = (view) => { updateCalls += 1; return origUpdate(view); };

  const options = { markers: true, trails: true };
  const markerGroup = new THREE.Group();
  const trailGroup = new THREE.Group();
  const entityScene = new ResidentEntityScene({
    manager,
    markerLayerId: MARKER_ID,
    trailLayerId: TRAIL_ID,
    markerGroup,
    trailGroup,
    markerGeometry: new THREE.SphereGeometry(1, 6, 4),
    entities: ENTITIES,
    trailMaxPoints: 50,
    isEnabled: (cls) => options[cls],
  });

  let camera = FAR;
  const rig = {
    manager, loader, globe, entityScene, options, markerGroup, trailGroup,
    released, entityLoads, imagerySignals,
    updateCalls: () => updateCalls,
    setCamera(c) { camera = c; },
    /** One render tick: exactly what `Viewer._updateLayerManager` does. Returns the plan. */
    tick() {
      return updateComposed(manager, [
        { planView: () => globe.planView(camera, SCREEN_H, FOV), commit: () => globe.commitPlannedView() },
        entityScene,
      ]);
    },
    /** Tick (then let microtasks settle) until `pred()` holds or `ms` of wall-clock
     * time passes -- the wait blocks on the real condition, never on a tick count. */
    async until(pred, ms = 5000) {
      const deadline = Date.now() + ms;
      for (;;) {
        rig.tick();
        await drain();
        if (pred()) return true;
        if (Date.now() > deadline) return false;
      }
    },
    keys: {
      markers: ENTITIES.map((e) => globalKeyFor(MARKER_ID, e.name)),
      trails: ENTITIES.map((e) => globalKeyFor(TRAIL_ID, e.name)),
    },
    entityResident() {
      return {
        markers: rig.keys.markers.filter((k) => manager.resident.has(k)).length,
        trails: rig.keys.trails.filter((k) => manager.resident.has(k)).length,
      };
    },
    allEntitiesResident() { const r = rig.entityResident(); return r.markers === ENTITIES.length && r.trails === ENTITIES.length; },
    noEntityResident() { const r = rig.entityResident(); return r.markers === 0 && r.trails === 0; },
    imageryResident: () => manager.countsByLayer().imagery.resident,
    /** Every tile of `tiles` has a resident imagery payload (read from the manager's own map). */
    imageryResidentFor: (tiles) => tiles.every((t) => manager.resident.has(globalKeyFor('imagery', tileKey(t)))),
    imageryPending: () => manager.countsByLayer().imagery.pending,
    /** The scene graph, read by traversal. */
    drawn() {
      let markerCount = 0;
      let markerKeys = [];
      markerGroup.traverse((o) => { if (o.isInstancedMesh) { markerCount = o.count; markerKeys = o.userData.residentKeys.slice(); } });
      const trailNames = [];
      trailGroup.traverse((o) => { if (o.isLine) trailNames.push(o.userData.spacecraft); });
      return { markerCount, markerKeys, trailNames: trailNames.sort() };
    },
    meshesWithTexture() {
      let n = 0;
      globe.group.traverse((o) => { if (o.isMesh && o.material.map && o.material.map.isStubTexture) n += 1; });
      return n;
    },
  };
  return rig;
}

const names = ENTITIES.map((e) => e.name);
const sortedNames = [...names].sort();

// ------------------------------------------------------------------- rig preconditions
const wantedFar = selectTiles(FAR_ECEF_M, { screenHeightPx: SCREEN_H, fovYRad: FOV, sseThreshold: 24, maxLevel: 2, maxTiles: 128 });
const wantedNear = selectTiles(NEAR_ECEF_M, { screenHeightPx: SCREEN_H, fovYRad: FOV, sseThreshold: 24, maxLevel: 2, maxTiles: 128 });
check('rig_farCameraWantsFewTilesNearCameraWantsMany', wantedFar.length === 2 && wantedNear.length === 20, {
  far: wantedFar.length, near: wantedNear.length,
});

// ------------------------------------------------------------------------------- PROOF B
{
  // Generous budget: nothing here is under pressure, so any cancellation is the view
  // composition's doing, not the budget's.
  const rig = makeRig({ budgetBytes: 64 * 1024 * 1024, imageryTileBytes: 4096 });
  rig.loader.mode = 'hold';
  rig.setCamera(NEAR);
  const ticks = [];
  let imageryInFlightEveryTick = true;
  const cancelledBefore = rig.manager.cancelledCount;
  for (let i = 0; i < 12; i += 1) {
    // The entity-only change of view: the entity classes flip on and off between ticks
    // (so the entity half of the composed view differs tick to tick) while the globe's
    // half is identical.
    rig.options.markers = i % 2 === 0;
    rig.options.trails = i % 3 !== 0;
    rig.tick();
    await drain();
    ticks.push(i);
    if (rig.imageryPending() === 0) imageryInFlightEveryTick = false;
  }
  rig.options.markers = true;
  rig.options.trails = true;
  rig.tick();
  await drain();
  check('B_oneManagerUpdatePerTick', rig.updateCalls() === ticks.length + 1, { updateCalls: rig.updateCalls(), ticks: ticks.length + 1 });
  check('B_imageryLoadsStayInFlightAcrossEntityOnlyChanges', imageryInFlightEveryTick && rig.imageryPending() === wantedNear.length && rig.loader.heldCount() === wantedNear.length, {
    pending: rig.imageryPending(), held: rig.loader.heldCount(), wantedTiles: wantedNear.length,
  });
  check('B_noImageryLoadCancelledByEntityOnlyChange',
    rig.manager.cancelledCount === cancelledBefore && rig.imagerySignals.length === wantedNear.length && rig.imagerySignals.every((s) => !s.aborted),
    { cancelledCount: rig.manager.cancelledCount, imageryLoadsStarted: rig.imagerySignals.length, abortedSignals: rig.imagerySignals.filter((s) => s.aborted).length });

  rig.loader.releaseAll();
  await drain();
  rig.tick(); // the commit half of this tick binds the textures that just became resident
  await drain();
  check('B_imageryLoadsCompleteThroughTheOneManager', rig.imageryResident() === wantedNear.length && rig.imageryPending() === 0 && rig.manager.failedCount === 0, {
    resident: rig.imageryResident(), pending: rig.imageryPending(), wanted: wantedNear.length, failedCount: rig.manager.failedCount,
  });
  check('B_globeMeshesBoundToTheResidentTextures', rig.meshesWithTexture() === wantedNear.length, {
    meshesWithTexture: rig.meshesWithTexture(), meshes: wantedNear.length,
  });
  const ok = await rig.until(() => rig.allEntitiesResident());
  check('B_entitiesResidentAlongsideTheImagery', ok && rig.drawn().markerCount === ENTITIES.length && rig.drawn().trailNames.join() === sortedNames.join(), {
    entityResident: rig.entityResident(), drawn: rig.drawn(),
  });
  check('B_noSoftBudgetViolation', rig.manager.softViolationCount === 0, { softViolationCount: rig.manager.softViolationCount });
}

// B control: the OLD pattern (a globe-only update, then an entity-only update, per tick)
// on an otherwise identical rig. This is what round 7 measured; it must cancel, or the
// assertions above could not distinguish the two designs.
{
  const rig = makeRig({ budgetBytes: 64 * 1024 * 1024, imageryTileBytes: 4096 });
  rig.loader.mode = 'hold';
  for (let i = 0; i < 6; i += 1) {
    const globeFragment = rig.globe.planView(NEAR, SCREEN_H, FOV);
    rig.manager.update(composeView(globeFragment));
    rig.globe.commitPlannedView();
    rig.manager.update(composeView(rig.entityScene.planView())); // entity-only: imagery is "not wanted" for this call
    await drain();
  }
  rig.loader.releaseAll();
  await drain();
  check('B_controlTwoPartialUpdatesCancelImagery', rig.manager.cancelledCount > 0 && rig.imagerySignals.some((s) => s.aborted) && rig.imageryResident() === 0, {
    cancelledCount: rig.manager.cancelledCount, abortedSignals: rig.imagerySignals.filter((s) => s.aborted).length, imageryResident: rig.imageryResident(),
  });
}

// ------------------------------------------------------------------------------- PROOF C
{
  const T = 4096;                        // declared imagery tile cost
  const M = 6;                           // tiles the budget holds once the entities are gone
  const BUDGET = M * T + 10;             // 10 bytes of slack: less than one marker (76)
  const rig = makeRig({ budgetBytes: BUDGET, imageryTileBytes: T });
  const { manager } = rig;
  let maxResidentBytes = 0;
  let budgetHeldEveryTick = true;
  const observe = () => {
    maxResidentBytes = Math.max(maxResidentBytes, manager.residentBytes);
    if (manager.residentBytes + manager.pendingBytes > BUDGET) budgetHeldEveryTick = false;
  };
  const step = async (pred, ms) => {
    const deadline = Date.now() + (ms || 5000);
    for (;;) {
      rig.tick(); await drain(); observe();
      if (pred()) {
        // Residency changes between ticks (loads settle on microtasks); the scene graph
        // reads it on the next tick, so take that tick before anyone looks at the scene.
        rig.tick(); await drain(); observe();
        return pred();
      }
      if (Date.now() > deadline) return false;
    }
  };

  // Phase 0: far camera, entities on. Everything fits.
  rig.setCamera(FAR);
  const p0 = await step(() => rig.allEntitiesResident() && rig.imageryResidentFor(wantedFar));
  const drawn0 = rig.drawn();
  check('C_entitiesResidentAndDrawnBeforeAnyPressure', p0 && drawn0.markerCount === ENTITIES.length && drawn0.markerKeys.join() === names.join() && drawn0.trailNames.join() === sortedNames.join(), {
    entityResident: rig.entityResident(), drawn: drawn0, imageryResident: rig.imageryResident(),
  });

  // Stability: re-planning resident, unchanged entities for many ticks is a no-op.
  const loadsBefore = rig.entityLoads.length;
  for (let i = 0; i < 40; i += 1) { rig.tick(); await drain(); observe(); }
  check('C_stable_residentEntityReplanIsNoOpNotAReloadPerTick', rig.entityLoads.length === loadsBefore && rig.entityLoads.length === 2 * ENTITIES.length
    && manager.byteCostRevisionCount === 0 && rig.allEntitiesResident() && rig.released.length === 0, {
    entityLoadsStarted: rig.entityLoads.length, expected: 2 * ENTITIES.length, byteCostRevisionCount: manager.byteCostRevisionCount, released: rig.released.length,
  });

  // Phase 1: near camera (20 tiles wanted, room for 5 next to the entities). The entities
  // are still wanted, so the manager defers imagery instead of evicting them.
  rig.setCamera(NEAR);
  const evictedBeforeNear = manager.evictedCount;
  const p1 = await step(() => rig.imageryResident() === 5 && manager.pending.size === 0);
  check('C_wantedEntityPayloadsAreNotEvictedUnderPressure', p1 && rig.allEntitiesResident() && rig.released.length === 0 && manager.deferredCount > 0, {
    imageryResident: rig.imageryResident(), entityResident: rig.entityResident(), entityReleases: rig.released.length, deferredCount: manager.deferredCount,
    evictedSinceNear: manager.evictedCount - evictedBeforeNear,
  });

  // Phase 2: the entity classes go off. Their keys stop being wanted; the near camera's
  // imagery still wants a sixth tile, which does not fit beside them, so the manager's own
  // eviction takes the entity payloads.
  rig.options.markers = false;
  rig.options.trails = false;
  const evictedBeforeDisable = manager.evictedCount;
  const p2 = await step(() => rig.noEntityResident() && rig.imageryResident() === M && manager.pending.size === 0);
  const evictedKeys = [...rig.released].sort();
  const allEntityKeys = [...rig.keys.markers, ...rig.keys.trails].sort();
  check('C_entityPayloadsEvictedByTheManagerUnderPressure', p2 && evictedKeys.join('|') === allEntityKeys.join('|') && manager.evictedCount - evictedBeforeDisable >= allEntityKeys.length, {
    evictedKeys: evictedKeys.map((k) => k.replace('\u0000', ':')), expected: allEntityKeys.map((k) => k.replace('\u0000', ':')),
    evictedCountDelta: manager.evictedCount - evictedBeforeDisable, imageryResident: rig.imageryResident(),
    residentBytes: manager.residentBytes, budget: BUDGET,
  });
  const drawn2 = rig.drawn();
  check('C_evictedEntitiesAreNotDrawnInTheSceneGraph', drawn2.markerCount === 0 && drawn2.markerKeys.length === 0 && drawn2.trailNames.length === 0, { drawn: drawn2 });

  // Phase 3: the classes come back on while the pressure lasts: wanted, planned, but they
  // do not fit beside six wanted tiles -- so they stay absent from residency AND the scene.
  rig.options.markers = true;
  rig.options.trails = true;
  const deferredBefore3 = manager.deferredCount;
  let planHadEntityKeys = true;
  for (let i = 0; i < 10; i += 1) {
    const plan = rig.tick();
    await drain(); observe();
    const planned = new Set(plan.map((r) => r.globalKey));
    if (![...rig.keys.markers, ...rig.keys.trails].every((k) => planned.has(k))) planHadEntityKeys = false;
  }
  const drawn3 = rig.drawn();
  check('C_reWantedEntityStaysAbsentWhileItDoesNotFit', planHadEntityKeys && rig.noEntityResident() && manager.deferredCount > deferredBefore3 && rig.entityLoads.length === 2 * ENTITIES.length, {
    planHadEntityKeys, entityResident: rig.entityResident(), deferredDelta: manager.deferredCount - deferredBefore3, entityLoadsStarted: rig.entityLoads.length,
  });
  check('C_notDrawnWhileWantedButDeferred', drawn3.markerCount === 0 && drawn3.trailNames.length === 0, { drawn: drawn3 });

  // Phase 4: the pressure lifts (the camera moves back, two tiles wanted): the near tiles
  // are unwanted and evictable, the entity requests fit again and are re-admitted.
  rig.setCamera(FAR);
  const p4 = await step(() => rig.allEntitiesResident() && rig.imageryResidentFor(wantedFar));
  check('C_reAdmittedOncePressureLifts', p4 && rig.entityLoads.length === 4 * ENTITIES.length, {
    entityResident: rig.entityResident(), entityLoadsStarted: rig.entityLoads.length, expected: 4 * ENTITIES.length, imageryResident: rig.imageryResident(),
  });
  const drawn4 = rig.drawn();
  check('C_drawnAgainAfterReAdmission', drawn4.markerCount === ENTITIES.length && drawn4.markerKeys.join() === names.join() && drawn4.trailNames.join() === sortedNames.join(), { drawn: drawn4 });
  check('C_budgetHeldAtEveryObservedTick', budgetHeldEveryTick && manager.softViolationCount === 0 && maxResidentBytes <= BUDGET, {
    maxResidentBytes, budget: BUDGET, softViolationCount: manager.softViolationCount, failedCount: manager.failedCount,
  });
}

// ----------------------------------------------- reload / teardown: no stale touch of a disposed scene
{
  const rig = makeRig({ budgetBytes: 64 * 1024 * 1024, imageryTileBytes: 4096 });
  rig.setCamera(FAR);
  rig.tick(); // entity loads are admitted and pending (microtask-deferred), nothing resident yet
  const pendingEntityLoads = [...rig.manager.pending.keys()].filter((k) => k.startsWith(MARKER_ID) || k.startsWith(TRAIL_ID)).length;
  // What `Viewer.clear()` does for a scenario reload: unregister the adapters (cancels
  // pending, evicts resident), then dispose the scene.
  rig.manager.removeLayer(MARKER_ID);
  rig.manager.removeLayer(TRAIL_ID);
  rig.entityScene.dispose();
  await drain();
  await drain();
  const residentEntityEntries = [...rig.manager.resident.values()].filter((e) => e.layerId === MARKER_ID || e.layerId === TRAIL_ID).length;
  let leftovers = 0;
  rig.markerGroup.traverse((o) => { if (o !== rig.markerGroup) leftovers += 1; });
  rig.trailGroup.traverse((o) => { if (o !== rig.trailGroup) leftovers += 1; });
  check('C_reloadLeavesNoStaleEntityResidencyOrSceneObjects', pendingEntityLoads === 2 * ENTITIES.length && residentEntityEntries === 0 && leftovers === 0 && rig.entityScene.markerMesh === null, {
    pendingEntityLoads, residentEntityEntries, leftovers,
  });
  // A fresh scene on the same ids (the next scenario) works from a clean slate.
  rig.manager.addLayer(new MarkerLayerAdapter({ id: MARKER_ID }));
  rig.manager.addLayer(new TrailLayerAdapter({ id: TRAIL_ID }));
  const scene2 = new ResidentEntityScene({
    manager: rig.manager, markerLayerId: MARKER_ID, trailLayerId: TRAIL_ID, markerGroup: rig.markerGroup, trailGroup: rig.trailGroup,
    markerGeometry: new THREE.SphereGeometry(1, 6, 4), entities: ENTITIES, trailMaxPoints: 50,
  });
  const ok = await (async () => {
    const deadline = Date.now() + 5000;
    for (;;) {
      updateComposed(rig.manager, [scene2]);
      await drain();
      if (scene2.markerNames.length === ENTITIES.length && scene2.trailLines.size === ENTITIES.length) return true;
      if (Date.now() > deadline) return false;
    }
  })();
  check('C_reloadedSceneAdmitsAndDrawsFromACleanSlate', ok, { markerNames: scene2.markerNames, trails: [...scene2.trailLines.keys()] });
  scene2.dispose();
}

// -------------------------------------------------------------------------------- report
const distinctNames = new Set(checks.map((c) => c.name));
checks.push({ name: 'meta_allCheckNamesDistinct', pass: distinctNames.size === checks.length, detail: { total: checks.length, distinct: distinctNames.size } });

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
if (!allPass) process.exitCode = 1;

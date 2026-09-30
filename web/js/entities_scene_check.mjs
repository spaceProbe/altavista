// CLI harness for tests/test_entities_scene.py: `node web/js/entities_scene_check.mjs`.
//
// Heavy round 7 (question 233), task 1's own proof item 2: "the real LayerManager + the
// real entity adapters + a real THREE scene graph, asserting from the graph
// (group.traverse), never from a counter alone" -- the plain-`node`-reachable half of
// H6's wiring proof (a real `web/js/scene.js` `Viewer` needs a real WebGL context,
// which plain `node` does not have -- `tests/test_viewer_entities_browser.py` is the
// real-browser half that drives the actual `Viewer`).
//
// What this file proves, against the SAME `LayerManager` class `web/js/layers_check.mjs`/
// `web/js/entities_layer_check.mjs` already exercise (never a second, parallel manager):
//   - kind separation: `MarkerLayerAdapter`/`TrailLayerAdapter` default to a `kind` that
//     is NOT `'imagery'`, so `LayerManager.imageryLayers()` (round 6, `16efe57`) never
//     includes them by construction -- AND that this is a real, load-bearing property
//     of `kind`, not an accident, by also registering an adapter with `kind: 'imagery'`
//     forced and showing THAT one wrongly appears (the deliberate misuse a scene.js
//     wiring bug would look like).
//   - one budget: a real `ImageryLayerAdapter` (web/js/layers/imagery_layer.js) and the
//     entity marker/trail adapters, registered on the SAME manager, both contribute to
//     the ONE `residentBytes` total -- never two independent totals.
//   - scene-graph truth: `buildMarkerInstancedMesh`/`buildTrailGroup`
//     (web/js/entities/entities_instanced_layer.js) produce real THREE objects,
//     findable by `group.traverse`, each carrying `userData.sourceLayerId` (the same
//     provenance discipline `web/js/layers/gateway_imagery_layer.js`'s own
//     `texture.userData.sourceLayerId` already establishes) -- set by the CALLER
//     (mirroring exactly what `web/js/scene.js`'s own wiring does), never invented by
//     the adapter itself.
//   - covariance ellipsoid / keep-out volume: a real, independently-constructed
//     diagonal covariance turned into a real `THREE.Mesh` via `buildEllipsoidMesh`, its
//     WORLD semi-axes (`worldSemiAxesKm`, read from the world matrix, never
//     `mesh.scale` directly) matching `covarianceEllipsoid()`'s own `semiAxesKm` to
//     floating-point tolerance, and the keep-out volume's own world semi-axes matching
//     the ellipsoid's plus the declared margin, per axis.
import { readFile } from 'node:fs/promises';
import * as THREE from 'three';
import { LayerManager } from './layers/index.js';
import { ImageryLayerAdapter } from './layers/imagery_layer.js';
import {
  MarkerLayerAdapter, TrailLayerAdapter, buildMarkerInstancedMesh, buildTrailGroup,
  covarianceEllipsoid, keepOutVolumeFromCovariance, buildEllipsoidMesh, worldSemiAxesKm,
} from './entities/index.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }
function approxEqual(a, b, tol) { return Math.abs(a - b) <= tol; }

// ------------------------------------------------------------- kind separation
{
  const marker = new MarkerLayerAdapter({ id: 'entity-markers' });
  const trail = new TrailLayerAdapter({ id: 'entity-trails' });
  check('kind_markerDefaultNotImagery', marker.kind !== 'imagery', { kind: marker.kind });
  check('kind_trailDefaultNotImagery', trail.kind !== 'imagery', { kind: trail.kind });

  const manager = new LayerManager({ memoryBudgetBytes: 64 * 1024 * 1024 });
  manager.addLayer(marker);
  manager.addLayer(trail);
  const imageryLayers = manager.imageryLayers();
  check('kind_managerImageryLayersExcludesEntityAdaptersByDefault', imageryLayers.length === 0, {
    imageryLayerIds: imageryLayers.map((l) => l.id),
  });

  // The deliberate-misuse demonstration: an entity adapter registered with kind
  // FORCED to 'imagery' DOES wrongly appear -- proving `kind`'s default is what keeps
  // the separation real, not a coincidence of the class never having a `kind` field at
  // all. (Task 1's report also re-runs this exact scenario as a standalone
  // perturbation against the shipped default, to show a check catches a REAL
  // regression, not merely this in-file demonstration.)
  const manager2 = new LayerManager({ memoryBudgetBytes: 64 * 1024 * 1024 });
  const misusedMarker = new MarkerLayerAdapter({ id: 'entity-markers-misused', kind: 'imagery' });
  manager2.addLayer(misusedMarker);
  const wrongImageryLayers = manager2.imageryLayers();
  check('kind_forcedImageryKindDemonstratesTheHazard', wrongImageryLayers.length === 1 && wrongImageryLayers[0].id === 'entity-markers-misused', {
    imageryLayerIds: wrongImageryLayers.map((l) => l.id),
  });
}

// ------------------------------------------------------------- one budget
{
  const manager = new LayerManager({ memoryBudgetBytes: 64 * 1024 * 1024 });
  const imagery = new ImageryLayerAdapter({ id: 'imagery', imageryUrl: 'https://example.invalid/{level}/{x}/{y}.png', loader: { load() {} } });
  const marker = new MarkerLayerAdapter({ id: 'entity-markers' });
  const trail = new TrailLayerAdapter({ id: 'entity-trails' });
  manager.addLayer(imagery);
  manager.addLayer(marker);
  manager.addLayer(trail);

  const markers = [
    { id: 'Target', positionKm: [0, 0, 0], color: '#54a0ff', sseError: 1, viewDistanceM: 1 },
    { id: 'Chaser', positionKm: [0.03, 0, 0], color: '#ff6b6b', sseError: 1, viewDistanceM: 1 },
  ];
  const trails = [
    { id: 'Target', pointsKm: [[0, 0, 0], [1, 0, 0]], color: '#54a0ff', sseError: 1, viewDistanceM: 1 },
    { id: 'Chaser', pointsKm: [[0.03, 0, 0], [1.03, 0, 0]], color: '#ff6b6b', sseError: 1, viewDistanceM: 1 },
  ];
  // `tiles: []` -- deliberately no imagery demand this call (this check's point is the
  // SHARED BUDGET/ACCOUNTING, not imagery's own load path); the imagery adapter is
  // still registered and still costs nothing when it has nothing planned.
  manager.update({ tiles: [], markers, trails });
  await new Promise((resolve) => setImmediate(resolve)); // let the entity adapters' microtask-deferred load() settle

  const counts = manager.countsByLayer();
  check('budget_entityMarkersAdmittedOnSharedManager', counts['entity-markers'].resident === markers.length, { counts });
  check('budget_entityTrailsAdmittedOnSharedManager', counts['entity-trails'].resident === trails.length, { counts });
  check('budget_oneManagerOneResidentByteTotal', manager.residentBytes > 0 && manager.softViolationCount === 0, {
    residentBytes: manager.residentBytes, softViolationCount: manager.softViolationCount,
  });

  // ---------------------------------------------------- scene-graph truth (markers/trails)
  const markerGeom = new THREE.SphereGeometry(1, 8, 6);
  const markerMesh = buildMarkerInstancedMesh(markerGeom, manager, 'entity-markers');
  // The provenance tag is set by the CALLER (web/js/scene.js's own wiring does this
  // identically) -- `buildMarkerInstancedMesh` itself never invents one, matching this
  // module's own "opaque payload" discipline.
  markerMesh.userData.sourceLayerId = 'entity-markers';
  const trailGroup = buildTrailGroup(manager, 'entity-trails');
  for (const line of trailGroup.children) line.userData.sourceLayerId = 'entity-trails';

  const sceneRoot = new THREE.Group();
  sceneRoot.add(markerMesh, trailGroup);

  let foundMarkerMesh = false;
  let foundTrailLines = 0;
  sceneRoot.traverse((obj) => {
    if (obj === markerMesh) foundMarkerMesh = obj.userData.sourceLayerId === 'entity-markers' && obj.count === markers.length;
    if (obj.isLine && obj.userData.sourceLayerId === 'entity-trails') foundTrailLines += 1;
  });
  check('sceneGraph_markerInstancedMeshFoundWithProvenance', foundMarkerMesh, { count: markerMesh.count });
  check('sceneGraph_trailLinesFoundWithProvenance', foundTrailLines === trails.length, { foundTrailLines, expected: trails.length });
}

// ------------------------------------------------------------- covariance ellipsoid / keep-out
{
  const SIGMA = 3;
  const MARGIN_KM = 0.050;
  // A real, independently-constructed diagonal 3x3 covariance (km^2) -- eigenvalues
  // are the diagonal entries themselves (no eigenvector rotation to reason about),
  // so the expected semi-axes are checkable by direct arithmetic, not by trusting the
  // same code under test.
  const covFlat = [
    0.000064, 0, 0,
    0, 0.000016, 0,
    0, 0, 0.000004,
  ]; // sqrt: 0.008, 0.004, 0.002 km 1-sigma -> *3 sigma = 0.024, 0.012, 0.006 km
  const expectedSemiAxesKm = [0.008, 0.004, 0.002].map((s) => s * SIGMA).sort((a, b) => b - a);

  const ell = covarianceEllipsoid(covFlat, 3, { sigma: SIGMA });
  check('ellipsoid_semiAxesMatchClosedForm', ell.semiAxesKm.every((v, i) => approxEqual(v, expectedSemiAxesKm[i], 1e-9)), {
    got: ell.semiAxesKm, expected: expectedSemiAxesKm,
  });

  const mesh = buildEllipsoidMesh(ell, { color: 0xff9f43 });
  const parent = new THREE.Group();
  parent.add(mesh);
  parent.updateMatrixWorld(true);
  const worldAxes = worldSemiAxesKm(mesh);
  check('ellipsoid_worldSemiAxesKmMatchesEllipsoidSemiAxesKm', worldAxes.every((v, i) => approxEqual(v, ell.semiAxesKm[i], 1e-9)), {
    worldAxes, semiAxesKm: ell.semiAxesKm,
  });

  const keepOut = keepOutVolumeFromCovariance(covFlat, 3, { sigma: SIGMA, marginKm: MARGIN_KM });
  const koMesh = buildEllipsoidMesh(keepOut, { color: 0xff3b30, wireframe: true });
  const koParent = new THREE.Group();
  koParent.add(koMesh);
  koParent.updateMatrixWorld(true);
  const koWorldAxes = worldSemiAxesKm(koMesh);
  const expectedKeepOutAxes = ell.semiAxesKm.map((a) => a + MARGIN_KM);
  check('keepout_worldSemiAxesKmEqualsEllipsoidPlusMargin', koWorldAxes.every((v, i) => approxEqual(v, expectedKeepOutAxes[i], 1e-9)), {
    koWorldAxes, expectedKeepOutAxes, marginKm: MARGIN_KM,
  });
  check('keepout_meshTaggedAsKeepoutVolume', koMesh.userData.kind === 'keepout-volume', { kind: koMesh.userData.kind });
  check('ellipsoid_meshTaggedAsCovarianceEllipsoid', mesh.userData.kind === 'covariance-ellipsoid', { kind: mesh.userData.kind });
}

// ------------------------------------------------- the declared per-class defaults
// Manager review, round 7. The browser proof (tests/test_viewer_entities_browser.py)
// already asserts the five entity classes' default on/off state against this round's own
// table, and it catches a change to it -- but that proof needs a real Chrome and
// VISIBLY SKIPS without one, so on a host with no browser a regression in the defaults
// would go entirely uncaught. This check closes that: it is the cheap, always-runnable
// half. `web/js/scene.js` cannot be imported here (it reaches for real DOM/THREE addons
// at module scope, which is exactly why this whole file drives `LayerManager` and the
// entity adapters directly), so the declared literal is read out of the source text --
// a static assertion about a DECLARED default, which is precisely the kind of claim a
// static read can make honestly. The table below is duplicated from the round's brief on
// purpose: if someone changes scene.js's literal, this must fail rather than follow it.
const EXPECTED_ENTITY_DEFAULTS = {
  markers: true, trails: true, covarianceEllipsoids: false, keepOutVolumes: false, models: false,
};
{
  const src = await readFile(new URL('./scene.js', import.meta.url), 'utf8');
  const m = src.match(/this\.entityOptions\s*=\s*\{([^}]*)\}/);
  const parsed = {};
  if (m) {
    for (const part of m[1].split(',')) {
      const kv = part.match(/\s*([A-Za-z]+)\s*:\s*(true|false)\s*/);
      if (kv) parsed[kv[1]] = kv[2] === 'true';
    }
  }
  const keys = Object.keys(EXPECTED_ENTITY_DEFAULTS);
  const matches = m !== null
    && Object.keys(parsed).length === keys.length
    && keys.every((k) => parsed[k] === EXPECTED_ENTITY_DEFAULTS[k]);
  check('defaults_sceneDeclaresExactlyThisRoundsEntityClassDefaults', matches, {
    found: m ? parsed : null, expected: EXPECTED_ENTITY_DEFAULTS,
  });
}

// -------------------------------------------------------------------------------- report
const distinctNames = new Set(checks.map((c) => c.name));
if (distinctNames.size !== checks.length) {
  // Round 6 defect 6 (this round's own COMMON brief): two checks sharing one name make
  // a failure ambiguous. Fail loudly rather than silently collapse.
  checks.push({ name: 'meta_allCheckNamesDistinct', pass: false, detail: { total: checks.length, distinct: distinctNames.size } });
} else {
  checks.push({ name: 'meta_allCheckNamesDistinct', pass: true, detail: { total: checks.length, distinct: distinctNames.size } });
}

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
if (!allPass) process.exitCode = 1;

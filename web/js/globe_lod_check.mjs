// Standalone check, and the harness tests/test_viewer_globe.py drives:
// `node web/js/globe_lod_check.mjs`. It asserts its own checks and exits 1 on any failure
// (question 254 item 5); see "ASSERTS AND EXITS" at the bottom.
//
// Drives web/js/globe_lod.js's real selectTiles()/TileLoadScheduler over a fixed,
// scripted camera path (geodetic lon/lat/altitude -> ECEF, deterministic, no
// randomness) and prints one JSON object of the per-step tile selections plus the
// scheduler's budget/cancellation counters. Nothing here reimplements globe_lod.js's
// arithmetic -- this file only supplies camera data and glues the real exports
// together, same relationship jitter_harness.mjs/scene_jitter_harness.mjs have to
// origin.js/scene.js (see those files' module docstrings).
//
// What this proves, and what a wrong implementation would fail against (see also
// tests/test_viewer_globe.py, which asserts on this JSON):
//   - Running this script twice (two independent `node` process invocations) and
//     diffing `steps` byte-for-byte catches any implementation that returns tiles in
//     an order sensitive to Map/Set insertion accidents, object property enumeration,
//     or any other non-canonical order -- selectTiles() sorts its result explicitly
//     (see its docstring) specifically so this never happens; removing that sort is
//     exactly the regression this guards against.
//   - `maxResidentObserved <= residentBudget` (budgetRespected) catches a scheduler
//     with no eviction: this camera path visits far more distinct tiles across its
//     whole run than the configured budget, so an implementation that just
//     accumulates every ever-loaded tile into `resident` without ever evicting would
//     overshoot the budget partway through and this assertion would catch it.
//   - `cancelledCount > 0` catches a scheduler that never cancels stale in-flight
//     loads: step 4 below is a deliberate large camera jump (LEO over one longitude ->
//     LEO over the opposite side of the planet) inserted *before* the previous step's
//     pending loads have all completed (see `completeLoads` calls below, which
//     intentionally complete only a few pending loads per step, leaving a realistic
//     backlog) -- an implementation that queues every requested load and never
//     cancels a stale one would report cancelledCount === 0 here.
//   - `maxLevelFar < maxLevelNear` catches a selector that ignores camera distance
//     entirely (e.g. always expanding to a fixed depth, or never refining at all):
//     the GEO-altitude step (very far, tiny screen-space error) must select coarser
//     tiles than the LEO close-up step (near, large screen-space error) for the same
//     screen/FOV settings.
import { selectTiles, TileLoadScheduler, geodeticToEcef, tileKey } from './globe_lod.js';

// `minLevel: 0` pins the level floor OFF for the path below: its numbers (the GEO step selecting only
// the two roots, maxResidentObserved against a budget of 32) are about the screen-space-error
// selection and the scheduler BELOW the floor. selectTiles' default floor (question 264, level 2:
// 32 tiles even at GEO) would change them without testing anything new about either. The floor
// itself is checked in the "floor" section at the bottom.
const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 40, maxLevel: 6, maxTiles: 300, minLevel: 0 };

// A fixed, scripted camera path (geodetic lon/lat degrees, altitude metres above the
// WGS84 surface) -- deliberately includes a far (GEO-altitude) step, a close LEO
// step, a large instantaneous jump to the opposite side of the globe (to exercise
// cancellation of the previous step's still-pending loads), and a return pass.
const CAMERA_PATH = [
  { lonDeg: -75, latDeg: 20, altM: 35786000 },  // GEO altitude, over the Americas
  { lonDeg: -74, latDeg: 21, altM: 20000000 },  // descending, same region
  { lonDeg: -73, latDeg: 22, altM: 2000000 },
  { lonDeg: -72, latDeg: 23, altM: 400000 },    // LEO altitude, close -- fine tiles expected
  { lonDeg: 105, latDeg: -22, altM: 400000 },   // big jump: opposite side of the planet
  { lonDeg: 106, latDeg: -21, altM: 380000 },
  { lonDeg: 107, latDeg: -20, altM: 5000000 },  // pull back out
  { lonDeg: 108, latDeg: -19, altM: 36000000 }, // GEO altitude again, different region
];

// residentBudget is deliberately small relative to the number of *distinct* tiles this
// path touches (summed across all 8 steps, comfortably over 100) -- so
// maxResidentObserved staying at/under it is only possible if eviction actually runs;
// an implementation that accumulates every ever-loaded tile into `resident` without
// evicting would overshoot this budget well before the path finishes (see this file's
// module docstring).
const RESIDENT_BUDGET = 32;
const scheduler = new TileLoadScheduler({ residentBudget: RESIDENT_BUDGET });
const steps = [];
let maxResidentObserved = 0;

for (const cam of CAMERA_PATH) {
  const cameraEcef = geodeticToEcef(cam.lonDeg, cam.latDeg, cam.altM);
  const tiles = selectTiles(cameraEcef, SCREEN);
  const selectedKeys = scheduler.update(tiles);
  // Simulate a slow load pipeline: several pending loads finish per step (but not all
  // of them), so a realistic backlog of still-pending tiles carries over into the next
  // step (what makes the jump step's cancellation genuine, not a no-op against an
  // empty queue) while still exercising eviction (some loads do complete).
  scheduler.completeLoads(8, selectedKeys);
  maxResidentObserved = Math.max(maxResidentObserved, scheduler.resident.size);
  steps.push({
    camera: cam,
    tileKeys: tiles.map(tileKey), // already canonically sorted by selectTiles()
    maxLevelSelected: tiles.reduce((m, t) => Math.max(m, t.level), 0),
    residentSize: scheduler.resident.size,
    pendingSize: scheduler.pending.size,
  });
}

const result = {
  steps,
  residentBudget: scheduler.residentBudget,
  maxResidentObserved,
  budgetRespected: maxResidentObserved <= scheduler.residentBudget,
  cancelledCount: scheduler.cancelledCount,
  evictedCount: scheduler.evictedCount,
  maxLevelFar: steps[0].maxLevelSelected,   // GEO-altitude step
  maxLevelNear: steps[3].maxLevelSelected,  // LEO close-up step
};

// ASSERTS AND EXITS: every assertion tests/test_viewer_globe.py makes on `result` (except
// cross-process byte-identity, which needs two processes and stays in the driver) is made
// here too, same thresholds, so the standalone gate fails on a regression by itself. Each
// failure prints to stderr as `FAIL <name>: measured X, bound Y`, then a summary line,
// then exit 1; success exits 0.
const failures = [];
let assertionCount = 0;
function assertBound(name, ok, measured, bound) {
  assertionCount += 1;
  if (!ok) failures.push(`FAIL ${name}: measured ${measured}, bound ${bound}`);
}
const keyParts = (k) => k.split('/').map(Number);
const keyOrder = (a, b) => {
  const pa = keyParts(a);
  const pb = keyParts(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1;
  }
  return 0;
};
assertBound('steps.length == 8', result.steps.length === 8, result.steps.length, '== 8');
result.steps.forEach((step, i) => {
  const sorted = step.tileKeys.every((k, j) => j === 0 || keyOrder(step.tileKeys[j - 1], k) <= 0);
  assertBound(`step ${i} tileKeys canonically sorted`, sorted, JSON.stringify(step.tileKeys.slice(0, 3)), 'ascending numeric (z/x/y) order');
});
assertBound('budgetRespected === true', result.budgetRespected === true, result.budgetRespected, 'true');
assertBound('maxResidentObserved <= residentBudget', result.maxResidentObserved <= result.residentBudget, result.maxResidentObserved, `<= ${result.residentBudget}`);
assertBound('evictedCount > 0 (eviction actually ran)', result.evictedCount > 0, result.evictedCount, '> 0');
assertBound('cancelledCount > 0 (stale loads cancelled on camera jump)', result.cancelledCount > 0, result.cancelledCount, '> 0');
assertBound('maxLevelFar < maxLevelNear', result.maxLevelFar < result.maxLevelNear, result.maxLevelFar, `< ${result.maxLevelNear}`);
assertBound('maxLevelFar == 0 (GEO step selects only the roots)', result.maxLevelFar === 0, result.maxLevelFar, '== 0');

// ---------------------------------------------------------------------------------- floor
// The level floor (question 264). GEO camera, the SCREEN settings otherwise as above.
const geo = geodeticToEcef(-75, 20, 35786000);
const sel = (o) => selectTiles(geo, { ...SCREEN, ...o });
const maxLvl = (t) => t.reduce((m, q) => Math.max(m, q.level), 0);
result.floor = {
  defaultFloorTiles: selectTiles(geo, { ...SCREEN, minLevel: undefined }).length, // omitted -> DEFAULT_MIN_LEVEL
  defaultFloorMaxLevel: maxLvl(selectTiles(geo, { ...SCREEN, minLevel: undefined })),
  floorOffTiles: sel({ minLevel: 0 }).length,
  floor2Tiles: sel({ minLevel: 2 }).length,
  clampedMaxLevel: maxLvl(sel({ minLevel: 2, maxLevel: 1 })),      // floor 2 on a level-1 pyramid -> level 1
  clampedTiles: sel({ minLevel: 2, maxLevel: 1 }).length,
  clampedToZero: sel({ minLevel: 5, maxLevel: 0 }).length,          // maxLevel 0 -> just the two roots
  cappedTiles: sel({ minLevel: 2, maxTiles: 10 }).length,           // the cap wins below the floor
  cappedMaxLevel: maxLvl(sel({ minLevel: 2, maxTiles: 10 })),
};
const F = result.floor;
assertBound('floor: the default floor is level 2 (32 tiles at GEO)', F.defaultFloorTiles === 32 && F.defaultFloorMaxLevel === 2, `${F.defaultFloorTiles} tiles, max level ${F.defaultFloorMaxLevel}`, '32 tiles, level 2');
assertBound('floor: minLevel 0 turns it off (the two roots at GEO)', F.floorOffTiles === 2, F.floorOffTiles, '== 2');
assertBound('floor: the floor costs at most 2 * 4^2 = 32 tiles', F.floor2Tiles === 32, F.floor2Tiles, '== 32');
assertBound('floor: clamped to maxLevel (minLevel 2, maxLevel 1 selects level 1 only)', F.clampedMaxLevel === 1 && F.clampedTiles === 8, `level ${F.clampedMaxLevel}, ${F.clampedTiles} tiles`, 'level 1, 8 tiles');
assertBound('floor: clamped to maxLevel 0 (the two roots)', F.clampedToZero === 2, F.clampedToZero, '== 2');
assertBound('floor: maxTiles wins below the floor (cap 10)', F.cappedTiles <= 10 && F.cappedMaxLevel <= 1, `${F.cappedTiles} tiles, max level ${F.cappedMaxLevel}`, '<= 10 tiles, a level below the floor');

for (const line of failures) process.stderr.write(`${line}\n`);
process.stderr.write(
  `globe_lod_check: ${assertionCount - failures.length} passed, ${failures.length} failed `
  + `(maxResidentObserved=${result.maxResidentObserved}/${result.residentBudget}, cancelled=${result.cancelledCount}, `
  + `evicted=${result.evictedCount}, maxLevelFar=${result.maxLevelFar}, maxLevelNear=${result.maxLevelNear})\n`,
);

// stdout keeps its JSON shape; exit inside the write callback so a piped stdout is flushed.
process.stdout.write(JSON.stringify(result), () => process.exit(failures.length === 0 ? 0 : 1));

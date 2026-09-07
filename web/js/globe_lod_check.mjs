// CLI harness for tests/test_viewer_globe.py: `node web/js/globe_lod_check.mjs`.
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

const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 40, maxLevel: 6, maxTiles: 300 };

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

process.stdout.write(JSON.stringify(result));

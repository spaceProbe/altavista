// CLI harness for tests/test_viewer_globe.py: `node web/js/tiles3d_check.mjs`.
//
// M16.4: closes M15.4's second disclosed shortcut ("streaming-layer budget/
// cancellation exists for the globe only ... not yet reused for the 3D Tiles
// overlay"). Drives web/js/tiles_layer.js's real `selectTiles3D()` -- this project's
// own screen-space-error tile selection over the *actual* tileset.json hierarchy
// (web/fixtures/3dtiles/tileset.json, web/fixtures/gen_3dtiles_fixture.py) -- and
// globe_lod.js's real `TileLoadScheduler`, reused verbatim (only its key-extraction
// function differs from the globe's), over a fixed, scripted camera path. Same
// "run the real code, don't port it" discipline as globe_lod_check.mjs: this file
// only supplies camera data and glues the real exports together.
//
// What this proves, and what a wrong implementation would fail against (see also
// tests/test_viewer_globe.py, which asserts on this JSON):
//   - Running this script twice and diffing `steps` byte-for-byte catches an
//     implementation that returns tile ids in an order sensitive to Map/Set
//     insertion accidents instead of `selectTiles3D`'s explicit final
//     `compareTileIds3D` sort (this task's binding rule, verbatim).
//   - `maxResidentObserved <= residentBudget` (budgetRespected), with
//     `evictedCount > 0` asserted directly, catches a scheduler/integration that
//     never evicts -- this camera path visits 83 distinct tile ids (out of the
//     fixture's 85) while RESIDENT_BUDGET is well below that, and even below a
//     single "close" step's own selection size (43), so eviction must run for the
//     budget to ever hold, exactly the globe's own test methodology.
//   - `cancelledCount > 0` catches a scheduler that never cancels a stale in-flight
//     load: step 3->5 below is a deliberate jump from one corner of the fixture's
//     footprint to the diagonally-opposite corner (mostly-disjoint tile sets, see
//     the module docstring below) before the previous step's pending loads have all
//     "completed" (simulated slow completion, `completeLoads(8, ...)`, same
//     technique as globe_lod_check.mjs).
//   - `maxLevelFar < maxLevelNear` catches a selector that ignores camera distance
//     (or a real per-tile authored `geometricError`) entirely: the far step (step 0)
//     must select only the tileset's root, the close step (step 3) must refine all
//     the way to the fixture's deepest authored level (3).
import { selectTiles3D, parseTileset3D, ecefFromRootTransform, enuBasisFromRootTransform } from './tiles_layer.js';
import { TileLoadScheduler } from './globe_lod.js';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TILESET_PATH = path.join(__dirname, '..', 'fixtures', '3dtiles', 'tileset.json');
const tilesetJson = JSON.parse(fs.readFileSync(TILESET_PATH, 'utf8'));
const tree = parseTileset3D(tilesetJson);

// The camera path is expressed in metres East/North/Up *from the fixture's own
// geo-referenced anchor point* -- read from tileset.json's real root.transform
// (ecefFromRootTransform/enuBasisFromRootTransform), not a second hardcoded
// location, so this harness automatically tracks whatever the fixture actually
// declares.
const anchorEcef = ecefFromRootTransform(tree.rootTransform);
const basis = enuBasisFromRootTransform(tree.rootTransform);
function cameraEcefFromEnu(eastM, northM, upM) {
  return {
    x: anchorEcef.x + basis.east.x * eastM + basis.north.x * northM + basis.up.x * upM,
    y: anchorEcef.y + basis.east.y * eastM + basis.north.y * northM + basis.up.y * upM,
    z: anchorEcef.z + basis.east.z * eastM + basis.north.z * northM + basis.up.z * upM,
  };
}

const SCREEN = { screenHeightPx: 900, fovYRad: (50 * Math.PI) / 180, sseThreshold: 16, maxLevel: 12, maxTiles: 512 };

// A fixed, scripted camera path over the fixture's own small (~4.4 km) footprint:
// far (root only) -> descending -> a close pass over the SW-ish corner (deep
// refinement) -> a deliberate jump to the diagonally-opposite (NE-ish) corner
// (mostly-disjoint tile set from the SW pass, see this file's module docstring) ->
// pull back out to far again. Distances are large enough, relative to the fixture's
// footprint, that "SW-ish"/"NE-ish" only needs to bias which leaves are nearest, not
// land exactly on a specific tile boundary.
const CAMERA_PATH = [
  { label: 'far', eastM: 0, northM: 0, upM: 3000000 },
  { label: 'descending', eastM: 0, northM: 0, upM: 200000 },
  { label: 'mid', eastM: 0, northM: 0, upM: 20000 },
  { label: 'close-sw', eastM: -2200, northM: -2200, upM: 4000 },
  { label: 'close-sw-2', eastM: -2100, northM: -2100, upM: 4000 },
  { label: 'jump-ne', eastM: 2200, northM: 2200, upM: 4000 },
  { label: 'jump-ne-2', eastM: 2100, northM: 2100, upM: 4000 },
  { label: 'far-2', eastM: 0, northM: 0, upM: 3000000 },
];

// Deliberately sized below a single "close" step's own selection (43 tile ids) and
// far below the path's running total of distinct ids (83, out of the fixture's 85) --
// see this file's module docstring, same design as globe_lod_check.mjs's
// RESIDENT_BUDGET.
const RESIDENT_BUDGET = 24;
const scheduler = new TileLoadScheduler({ residentBudget: RESIDENT_BUDGET });
const steps = [];
let maxResidentObserved = 0;

for (const cam of CAMERA_PATH) {
  const cameraEcef = cameraEcefFromEnu(cam.eastM, cam.northM, cam.upM);
  const tileIds = selectTiles3D(tree, cameraEcef, SCREEN);
  // keyFn = identity: this tree's own ids are already the stable string key (see
  // tiles_layer.js's parseTileset3D docstring) -- TileLoadScheduler.update()'s
  // generalized keyFn parameter (globe_lod.js, M16.4) is what makes this the exact
  // same class the globe uses, not a second implementation.
  const selectedKeys = scheduler.update(
    tileIds.map((id) => ({ id })),
    (t) => t.id,
  );
  // Simulate a slow load pipeline, same technique as globe_lod_check.mjs: only a
  // few pending loads finish per step, leaving a realistic backlog so the jump
  // step's cancellation is genuine (not a no-op against an empty queue).
  scheduler.completeLoads(8, selectedKeys);
  maxResidentObserved = Math.max(maxResidentObserved, scheduler.resident.size);
  const maxLevelSelected = tileIds.reduce((m, id) => Math.max(m, id.split('.').length - 1), 0);
  steps.push({
    camera: cam,
    tileIds, // already canonically sorted by selectTiles3D()
    maxLevelSelected,
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
  maxLevelFar: steps[0].maxLevelSelected,     // 'far' step
  maxLevelNear: steps[3].maxLevelSelected,    // 'close-sw' step
  tilesetTileCount: tree.nodes.size,
};

process.stdout.write(JSON.stringify(result));

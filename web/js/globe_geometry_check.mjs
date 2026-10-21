// Standalone check, and the harness tests/test_viewer_globe.py drives:
// `node web/js/globe_geometry_check.mjs`. It asserts its own checks and exits 1 on any failure.
//
// Question 264 (the Blue Marble basemap). The flat-colour fixture every other globe check draws
// cannot show a picture upside down, inside out or faceted, so the real tile mesh gets its own
// geometric contract here, run for real under node against the shipped `web/js/globe.js`
// (a stub texture loader that never does I/O, as web/js/globe_imagery_check.mjs does):
//
//   - FACES OUTWARD: every triangle's winding is counter-clockwise seen from OUTSIDE the
//     ellipsoid (the geometric normal agrees with the WGS84 outward normal). three.js culls the
//     other side, so a clockwise mesh draws the far hemisphere's inside and nothing of the near
//     side: measured in a real Chrome, a camera over lon 0 saw lon 180 (defect found by
//     tests/test_viewer_basemap_orientation_browser.py).
//   - UPRIGHT: tile images are north-up and a texture bound through `applyTileTexture` is
//     uploaded unflipped (`flipY === false`; a TextureLoader texture is `true` by default and was
//     drawn north-south flipped), so the SOUTH edge of a tile must carry v = 1 and the north edge
//     v = 0, and u must grow eastward.
//   - ON ITS TILE: every vertex lies inside its tile's `tileBoundsDeg` (the scheme in
//     globe_lod.js: y = 0 is the south row), with the south-west corner vertex on the south-west
//     corner.
//   - SMOOTH: no quad spans more than 3 degrees of arc at any level it is built at (a fixed 8
//     quads gave a 22.5-degree-faceted level-0 Earth).
//
// BREAKS: each predicate is also run against a deliberately wrong copy of the same mesh (indices
// reversed, v mirrored, the texture left at flipY = true, a coarse segment count) and must say no,
// so the predicates are known to be able to fail.
import * as THREE from 'three';
import { GlobeLayer } from './globe.js';
import { SCENE_UNITS_PER_METRE, WGS84_A_M, WGS84_B_M, tileBoundsDeg, ecefToGeodeticDeg } from './globe_lod.js';

const SCREEN = { screenHeightPx: 900, fovYRad: (45 * Math.PI) / 180 };
const MAX_FACET_DEG = 3.0;

/** A texture loader that answers every request with an empty THREE.Texture (so `applyTileTexture`
 * runs exactly as it does for a real TextureLoader texture: `flipY` is true until it says otherwise). */
function stubLoader() {
  return { load(url, onLoad) { onLoad(new THREE.Texture()); } };
}

function meshesFor(cameraEcefM, opts = {}) {
  const layer = new GlobeLayer({ textureLoader: stubLoader(), ...opts });
  layer.update({ x: cameraEcefM.x * SCENE_UNITS_PER_METRE, y: cameraEcefM.y * SCENE_UNITS_PER_METRE, z: cameraEcefM.z * SCENE_UNITS_PER_METRE },
    SCREEN.screenHeightPx, SCREEN.fovYRad);
  return [...layer._meshes.values()];
}

const outwardNormal = (x, y, z) => {
  const a2 = WGS84_A_M * WGS84_A_M, b2 = WGS84_B_M * WGS84_B_M;
  const n = [x / a2, y / a2, z / b2];
  const l = Math.hypot(...n);
  return n.map((v) => v / l);
};

/** Fraction of triangles whose geometric normal does NOT point inward (1 = every triangle faces
 * outward; 0 = every one faces inward). A polar tile's top row collapses to a point, leaving
 * zero-area triangles that face neither way; those are not counted against it. */
function outwardFraction(geom, indexOverride) {
  const p = geom.getAttribute('position').array;
  const idx = indexOverride || geom.getIndex().array;
  let good = 0, total = 0;
  for (let t = 0; t < idx.length; t += 3) {
    const [a, b, c] = [idx[t] * 3, idx[t + 1] * 3, idx[t + 2] * 3];
    const e1 = [p[b] - p[a], p[b + 1] - p[a + 1], p[b + 2] - p[a + 2]];
    const e2 = [p[c] - p[a], p[c + 1] - p[a + 1], p[c + 2] - p[a + 2]];
    const nrm = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    const cx = (p[a] + p[b] + p[c]) / 3, cy = (p[a + 1] + p[b + 1] + p[c + 1]) / 3, cz = (p[a + 2] + p[b + 2] + p[c + 2]) / 3;
    const o = outwardNormal(cx, cy, cz);
    const area2 = Math.hypot(...nrm);
    const dot = nrm[0] * o[0] + nrm[1] * o[1] + nrm[2] * o[2];
    if (dot >= -1e-9 * area2) good++; // outward, or degenerate
    total++;
  }
  return good / total;
}

/** Vertex grid helpers: the mesh is (segments + 1)^2 vertices, row j = 0 at the south edge. */
function gridInfo(geom) {
  const count = geom.getAttribute('position').count;
  const n = Math.round(Math.sqrt(count));
  return { n, segments: n - 1 };
}

function lonLatOf(geom, vi) {
  const p = geom.getAttribute('position').array;
  const g = ecefToGeodeticDeg(p[vi * 3] / SCENE_UNITS_PER_METRE, p[vi * 3 + 1] / SCENE_UNITS_PER_METRE, p[vi * 3 + 2] / SCENE_UNITS_PER_METRE);
  return { lon: g.lonDeg, lat: g.latDeg };
}

/** UV contract for one mesh: south edge v = 1, north edge v = 0, west u = 0, east u = 1. */
function uvUpright(geom, mirrorV = false) {
  const { n } = gridInfo(geom);
  const uv = geom.getAttribute('uv').array;
  const at = (i, j) => [uv[(j * n + i) * 2], mirrorV ? 1 - uv[(j * n + i) * 2 + 1] : uv[(j * n + i) * 2 + 1]];
  const sw = at(0, 0), se = at(n - 1, 0), nw = at(0, n - 1), ne = at(n - 1, n - 1);
  const eq = (a, b) => Math.abs(a - b) < 1e-9;
  return eq(sw[0], 0) && eq(sw[1], 1) && eq(se[0], 1) && eq(se[1], 1) && eq(nw[0], 0) && eq(nw[1], 0) && eq(ne[0], 1) && eq(ne[1], 0);
}

/** Every vertex inside the tile's bounds (a small tolerance for the f32 round trip), and the
 * south-west vertex on the south-west corner. */
function onItsTile(mesh) {
  const geom = mesh.geometry;
  const b = tileBoundsDeg(mesh.userData.tile);
  const { n } = gridInfo(geom);
  const tol = 1e-3;
  const wrapDiff = (a, c) => Math.abs(((a - c + 540) % 360) - 180);
  for (let vi = 0; vi < n * n; vi++) {
    const { lon, lat } = lonLatOf(geom, vi);
    if (lat < b.south - tol || lat > b.north + tol) return false;
    if (Math.abs(lat) > 90 - tol) continue; // a pole has no longitude
    const inLon = (lon >= b.west - tol && lon <= b.east + tol) || wrapDiff(lon, b.east) < tol || wrapDiff(lon, b.west) < tol;
    if (!inLon) return false;
  }
  const sw = lonLatOf(geom, 0);
  return Math.abs(sw.lat - b.south) < tol && (Math.abs(sw.lat) > 90 - tol || wrapDiff(sw.lon, b.west) < tol);
}

/** Largest angle (degrees) between two adjacent vertices as seen from the centre. */
function maxFacetDeg(geom) {
  const { n } = gridInfo(geom);
  const p = geom.getAttribute('position').array;
  const ang = (i, j) => {
    const a = [p[i * 3], p[i * 3 + 1], p[i * 3 + 2]], c = [p[j * 3], p[j * 3 + 1], p[j * 3 + 2]];
    const d = (a[0] * c[0] + a[1] * c[1] + a[2] * c[2]) / (Math.hypot(...a) * Math.hypot(...c));
    return (Math.acos(Math.max(-1, Math.min(1, d))) * 180) / Math.PI;
  };
  let worst = 0;
  for (let j = 0; j < n; j++) for (let i = 0; i < n; i++) {
    const v = j * n + i;
    if (i + 1 < n) worst = Math.max(worst, ang(v, v + 1));
    if (j + 1 < n) worst = Math.max(worst, ang(v, v + n));
  }
  return worst;
}

// ----------------------------------------------------------------------- the cameras
// Far (the two level-0 roots) and LEO-close (a deeper selection), plus a mid-distance one.
const CAMERAS = {
  far: { x: 20410038.4, y: 0, z: 0 },
  mid: { x: 9567000, y: 1200000, z: 2400000 },
  leo: { x: 6900000, y: 500000, z: 800000 },
};
// `far0` is the far camera with the level floor off (`minLevel: 0`): the two level-0 roots, whose
// quads are the largest and so the ones the facet bound is about. With the default floor (2) the
// far camera selects level 2 tiles instead (`far`).
const FAR_ROOTS = { name: 'far0', cam: CAMERAS.far, opts: { minLevel: 0 } };

const result = { cameras: {}, breaks: {} };
const failures = [];
let assertionCount = 0;
function assertBound(name, ok, measured, bound) {
  assertionCount += 1;
  if (!ok) failures.push(`FAIL ${name}: measured ${measured}, bound ${bound}`);
}

const allLevels = new Set();
const RUNS = [...Object.entries(CAMERAS).map(([name, cam]) => ({ name, cam, opts: {} })), FAR_ROOTS];
for (const { name, cam, opts } of RUNS) {
  const meshes = meshesFor(cam, opts);
  const levels = [...new Set(meshes.map((m) => m.userData.tile.level))].sort();
  levels.forEach((l) => allLevels.add(l));
  let worstOutward = 1, uvOk = true, tileOk = true, facet = 0, flipYAll = true, bound = 0;
  for (const m of meshes) {
    worstOutward = Math.min(worstOutward, outwardFraction(m.geometry));
    uvOk = uvOk && uvUpright(m.geometry);
    tileOk = tileOk && onItsTile(m);
    facet = Math.max(facet, maxFacetDeg(m.geometry));
    if (m.material.map) { bound++; flipYAll = flipYAll && m.material.map.flipY === false; }
  }
  result.cameras[name] = { meshes: meshes.length, levels, worstOutwardFraction: worstOutward, uvUpright: uvOk, onItsTile: tileOk, maxFacetDeg: facet, texturesBound: bound, allFlipYFalse: flipYAll };
  assertBound(`${name}: tiles were selected`, meshes.length > 0, meshes.length, '> 0');
  assertBound(`${name}: every triangle faces outward`, worstOutward === 1, worstOutward, '1 (all of them)');
  assertBound(`${name}: south edge v = 1, north edge v = 0, west u = 0, east u = 1`, uvOk, uvOk, 'true');
  assertBound(`${name}: every vertex lies on its tile (tileBoundsDeg), south-west corner first`, tileOk, tileOk, 'true');
  assertBound(`${name}: no quad spans more than ${MAX_FACET_DEG} degrees`, facet <= MAX_FACET_DEG, facet.toFixed(2), `<= ${MAX_FACET_DEG}`);
  assertBound(`${name}: every bound texture is uploaded unflipped (flipY false)`, bound === meshes.length && flipYAll, `${bound} of ${meshes.length} bound, flipYAll=${flipYAll}`, 'all bound and false');
}
assertBound('the cameras together reach a level deeper than 0', [...allLevels].some((l) => l >= 2), [...allLevels].join(','), 'some level >= 2');

// ------------------------------------------------------------------------- BREAKS
const sample = meshesFor(CAMERAS.leo)[0];
const reversed = Uint32Array.from(sample.geometry.getIndex().array).reverse();
// reversing the whole array reverses each triangle AND the triangle order: every triangle flips
const reversedFraction = outwardFraction(sample.geometry, reversed);
result.breaks.reversedIndicesOutwardFraction = reversedFraction;
assertBound('BREAKS: a mesh with its indices reversed faces inward (but for its zero-area polar triangles)', reversedFraction < 0.05, reversedFraction, '< 0.05');
result.breaks.mirroredVIsUpright = uvUpright(sample.geometry, true);
assertBound('BREAKS: a mesh with v mirrored is not upright', result.breaks.mirroredVIsUpright === false, result.breaks.mirroredVIsUpright, 'false');
const rawTex = new THREE.Texture();
result.breaks.untouchedTextureFlipY = rawTex.flipY;
assertBound('BREAKS: a bare TextureLoader-style texture has flipY true until applyTileTexture runs', rawTex.flipY === true, rawTex.flipY, 'true');
const coarse = meshesFor(CAMERAS.far, { segments: 8, minLevel: 0 });
const coarseFacet = Math.max(...coarse.map((m) => maxFacetDeg(m.geometry)));
result.breaks.coarseFacetDeg = coarseFacet;
assertBound('BREAKS: the old fixed 8 segments gave a facet far above the bound at level 0', coarseFacet > 10, coarseFacet.toFixed(1), '> 10');

for (const line of failures) process.stderr.write(`${line}\n`);
process.stderr.write(`globe_geometry_check: ${assertionCount - failures.length} passed, ${failures.length} failed\n`);
process.stdout.write(JSON.stringify(result), () => process.exit(failures.length === 0 ? 0 : 1));

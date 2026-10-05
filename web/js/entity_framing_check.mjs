// CLI harness: `node web/js/entity_framing_check.mjs` (cleanup round, questions 235/237).
//
// Proves scene.js's pure entity-framing helpers -- `entityFramingDistance`,
// `projectedSphereHeightFraction`, `entityFramingDepthRange` -- against ground truth
// computed HERE, never by calling the helper twice:
//   - the closed-form distance for the case the lead observed (a 3 km sphere, 50 degree
//     FOV), as a literal number worked out by hand: d = R / sin(0.6 * 25 degrees);
//   - the projected size, measured by projecting the sphere's silhouette (its tangent
//     points, derived from the tangency condition (T - cam) . T = 0, not from the
//     helper's own asin) through a real THREE.PerspectiveCamera's projection matrix, and
//     reading back the angular size the tangent rays subtend -- so the "fills 60 % of the
//     FOV" claim is a measurement of the camera, not of the formula;
//   - the lead's observation as a number: at the pre-cleanup Earth-framed distance
//     (1.5 x the Earth's radius from the target, the old `setFocus` constant) the same
//     sphere is under one pixel tall on a 900 px viewport;
//   - portrait aspect (narrower FOV is the horizontal one), linearity in the extent,
//     argument validation, and the depth range (nothing clipped, Earth still inside far).
// Every check has a distinct name; a failing check prints the detail and exits 1.
import * as THREE from 'three';
import {
  SCALE, ENTITY_FRAMING_FRACTION,
  entityFramingDistance, projectedSphereHeightFraction, entityFramingDepthRange,
} from './scene.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }
const near = (a, b, rel) => Math.abs(a - b) <= rel * Math.max(Math.abs(a), Math.abs(b), 1e-300);
const deg = (r) => (r * 180) / Math.PI;
function throws(fn) { try { fn(); return false; } catch (e) { return true; } }

/** Project sphere (radius R, centre at the origin) from a camera on +z at `dist` looking
 * at the origin, through a real THREE.PerspectiveCamera. The silhouette's tangent points
 * satisfy (T - cam) . T = 0 on the sphere: T = (R sqrt(1 - R^2/d^2), 0, R^2/d) in the
 * x-z plane (and the same rotated into y for the vertical). Returns the NDC extent
 * of the sphere along y (=diameter / viewport height) and x (= diameter / width), and the
 * angle between the two tangent rays at the camera (the sphere's angular diameter). */
function measureSphere(R, dist, fovDeg, aspect) {
  const cam = new THREE.PerspectiveCamera(fovDeg, aspect, dist * 1e-3, dist * 1e3);
  cam.position.set(0, 0, dist);
  cam.lookAt(0, 0, 0);
  cam.updateMatrixWorld(true);
  cam.updateProjectionMatrix();
  const tz = (R * R) / dist;
  const tt = R * Math.sqrt(1 - (R * R) / (dist * dist));
  const top = new THREE.Vector3(0, tt, tz).project(cam);
  const side = new THREE.Vector3(tt, 0, tz).project(cam);
  const toTop = new THREE.Vector3(0, tt, tz - dist).normalize();
  const toBottom = new THREE.Vector3(0, -tt, tz - dist).normalize();
  return {
    heightFraction: top.y, // NDC y of the top tangent point == (diameter/2)/(height/2)
    widthFraction: side.x,
    angularDiameterDeg: deg(toTop.angleTo(toBottom)),
  };
}

// ----------------------------------------------------------- 3 km sphere, 50 degree FOV
{
  const R = 3 * SCALE;            // 3 km radius, scene units
  const FOV = 50, ASPECT = 16 / 9;
  const d = entityFramingDistance(R, FOV, ASPECT);
  // Hand-worked: R / sin(0.6 * 25 degrees) = 3 km / sin(15 degrees) = 3 / 0.25881904510252 km.
  const expectedKm = 3 / 0.25881904510252074;
  check('fraction_is_stated_value', ENTITY_FRAMING_FRACTION === 0.6, { ENTITY_FRAMING_FRACTION });
  check('distance_3km_sphere_50deg_closed_form', near(d / SCALE, expectedKm, 1e-9), { gotKm: d / SCALE, expectedKm });

  const m = measureSphere(R, d, FOV, ASPECT);
  check('angular_size_is_60pct_of_fov_by_projection', near(m.angularDiameterDeg, 0.6 * FOV, 1e-9), { got: m.angularDiameterDeg, want: 0.6 * FOV });
  // Perspective projection: tan(15 degrees) / tan(25 degrees) = 0.5746...
  const wantHeight = Math.tan((15 * Math.PI) / 180) / Math.tan((25 * Math.PI) / 180);
  check('projected_height_fraction_by_projection', near(m.heightFraction, wantHeight, 1e-9), { got: m.heightFraction, want: wantHeight });
  check('projected_height_fraction_matches_helper', near(projectedSphereHeightFraction(R, d, FOV), m.heightFraction, 1e-9),
    { helper: projectedSphereHeightFraction(R, d, FOV), measured: m.heightFraction });
  check('projected_height_in_band_0p5_to_0p65', m.heightFraction > 0.5 && m.heightFraction < 0.65, { heightFraction: m.heightFraction });
  check('landscape_view_height_is_the_limiting_dimension', m.widthFraction < m.heightFraction, { w: m.widthFraction, h: m.heightFraction });

  // The lead's observation as a number: the pre-cleanup constant for a spacecraft focus
  // was 1.5 x the central body's radius (Earth, 6378.137 km) from the target.
  const oldDistance = 6378.137 * SCALE * 1.5;
  const old = measureSphere(R, oldDistance, FOV, ASPECT);
  const oldPx = old.heightFraction * 900;
  check('pre_cleanup_distance_is_under_one_pixel', oldPx < 1, { px_of_900: oldPx, oldDistanceKm: oldDistance / SCALE });
  check('new_distance_is_far_closer_than_old_constant', d < oldDistance / 500, { newKm: d / SCALE, oldKm: oldDistance / SCALE });
}

// ----------------------------------------------------------------- other FOV / aspect
for (const [fov, aspect] of [[30, 1.6], [50, 1], [75, 2.4], [110, 1.2]]) {
  const R = 0.0015;
  const d = entityFramingDistance(R, fov, aspect);
  const m = measureSphere(R, d, fov, aspect);
  check(`angular_size_fov${fov}_aspect${aspect}`, near(m.angularDiameterDeg, 0.6 * fov, 1e-9), { got: m.angularDiameterDeg, want: 0.6 * fov });
  check(`fits_view_fov${fov}_aspect${aspect}`, m.heightFraction < 1 && m.widthFraction < 1, m);
}

// ------------------------------------------- portrait: the narrower FOV is the horizontal one
{
  const R = 0.003, FOV = 50, ASPECT = 0.5;
  const hFovDeg = deg(2 * Math.atan(Math.tan((FOV * Math.PI) / 360) * ASPECT));
  const d = entityFramingDistance(R, FOV, ASPECT);
  const m = measureSphere(R, d, FOV, ASPECT);
  check('portrait_uses_horizontal_fov', near(m.angularDiameterDeg, 0.6 * hFovDeg, 1e-9), { got: m.angularDiameterDeg, want: 0.6 * hFovDeg, hFovDeg });
  check('portrait_sphere_fits_width', m.widthFraction < 1 && m.widthFraction > m.heightFraction, m);
  const landscape = entityFramingDistance(R, FOV, 16 / 9);
  check('portrait_distance_exceeds_landscape', d > landscape, { portrait: d, landscape });
}

// ------------------------------------------------------------ depends on the extent, only
{
  const d1 = entityFramingDistance(0.0015, 50, 1.5);
  const d10 = entityFramingDistance(0.015, 50, 1.5);
  check('distance_linear_in_extent', near(d10 / d1, 10, 1e-12), { ratio: d10 / d1 });
  check('distance_differs_between_a_marker_and_an_ellipsoid', entityFramingDistance(3e-4, 50, 1.5) < entityFramingDistance(0.003, 50, 1.5) / 5, {});
  // A larger share of the view means a closer camera.
  check('distance_shrinks_as_fraction_grows', entityFramingDistance(1, 50, 1.5, 0.3) > entityFramingDistance(1, 50, 1.5, 0.6), {});
  // Narrower FOV => further back for the same extent.
  check('distance_grows_as_fov_narrows', entityFramingDistance(1, 20, 1.5) > entityFramingDistance(1, 60, 1.5), {});
}

// ----------------------------------------------------------------- argument validation
check('rejects_zero_extent', throws(() => entityFramingDistance(0, 50, 1.5)), {});
check('rejects_negative_extent', throws(() => entityFramingDistance(-1, 50, 1.5)), {});
check('rejects_nan_extent', throws(() => entityFramingDistance(NaN, 50, 1.5)), {});
check('rejects_zero_fov', throws(() => entityFramingDistance(1, 0, 1.5)), {});
check('rejects_180_fov', throws(() => entityFramingDistance(1, 180, 1.5)), {});
check('rejects_zero_aspect', throws(() => entityFramingDistance(1, 50, 0)), {});
check('rejects_fraction_of_one', throws(() => entityFramingDistance(1, 50, 1.5, 1)), {});

// -------------------------------------------------------------------------- depth range
{
  const FAR_FLOOR = 1e6, CAP = 1e-3;
  let allClear = true; const bad = [];
  for (const fov of [10, 30, 50, 90, 120, 170]) {
    for (const R of [3e-4, 0.003, 0.5]) {
      const d = entityFramingDistance(R, fov, 1.5);
      const r = entityFramingDepthRange(d, R, FAR_FLOOR, CAP);
      const ok = r.near > 0 && r.near < d - R && r.far >= FAR_FLOOR && r.far > d + R && r.minDistance <= CAP && r.minDistance <= d * 0.1 + 1e-18;
      if (!ok) { allClear = false; bad.push({ fov, R, d, r }); }
    }
  }
  check('depth_range_never_clips_the_framed_sphere', allClear, bad);
  const d = entityFramingDistance(0.003, 50, 1.5);
  const r = entityFramingDepthRange(d, 0.003, FAR_FLOOR, CAP);
  // The Earth is ~7 scene units behind a LEO entity: far must still reach it.
  check('depth_range_far_reaches_the_earth_behind', r.far > 7 + 6.4, { far: r.far });
  check('depth_range_near_is_one_percent_of_distance', near(r.near, d * 0.01, 1e-12), { near: r.near, d });
  check('zoom_floor_drops_below_whole_scenario_floor_for_a_small_marker', entityFramingDepthRange(entityFramingDistance(3e-4, 50, 1.5), 3e-4, FAR_FLOOR, CAP).minDistance < CAP, {});
}

const failed = checks.filter((c) => !c.pass);
for (const c of checks) process.stdout.write(`${c.pass ? 'PASS' : 'FAIL'} ${c.name}${c.pass ? '' : ' ' + JSON.stringify(c.detail)}\n`);
process.stdout.write(`${checks.length - failed.length}/${checks.length} checks passed\n`);
if (failed.length) process.exitCode = 1;

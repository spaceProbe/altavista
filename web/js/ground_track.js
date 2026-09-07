// M26.4 (docs/ui-rework-plan.md): the 2D companion map's ground-track/footprint/current-
// position projection. "Ground track" is exactly "where is this spacecraft's sub-point on
// the body's surface, over time" -- which needs the spacecraft's position **in the body-
// fixed frame**, not the inertial/entities frame the wire actually records positions in.
//
// The scene JSON records `spacecraft[i].pos` in the scenario's own `frame` (for the demo
// two-instance run, `EarthMJ2000Eq` -- confirmed by decoding
// tests/fixtures/demo_two_instance.runproducts.bin directly, see web/js/REPORT_M26_4.md),
// and separately records each body's own linear position + orientation quaternion over
// time (`bodies[i].t/pos/quat`, `altavista/model.py`'s `BodyTrack`: "Rotation from
// body-fixed to the scenario frame at each sample, as a unit quaternion [x,y,z,w]" --
// read verbatim from that dataclass's own doc comment, not assumed). That body quaternion
// is *already sent to the client* (it drives `web/js/scene.js`'s own body mesh
// orientation, `BodyInterp.orientation()`, `web/js/interp.js`) -- this module reuses that
// exact class rather than re-deriving body rotation a second time, and adds nothing to
// the wire: the whole computation is "take two tracks the server already sends, and
// combine them," matching this task's "client-side only" scope exactly.
//
// Framework-light: imports THREE only for the Vector3/Quaternion scratch objects
// `BodyInterp`/`TrajectoryInterp` (interp.js) already use -- neither needs a renderer or
// a DOM, so this module runs headlessly under plain `node`
// (web/js/ground_track_check.mjs, tests/test_viewer_panels.py), exactly like
// frames.js/origin.js/globe_lod.js.
import * as THREE from 'three';
import { BodyInterp, TrajectoryInterp } from './interp.js';
import { ecefToGeodeticDeg } from './globe_lod.js';

const KM_TO_M = 1000;

const _scPos = new THREE.Vector3();
const _bodyPos = new THREE.Vector3();
const _bodyQuat = new THREE.Quaternion();
const _rel = new THREE.Vector3();

/**
 * A scenario-frame position (km, e.g. one spacecraft trajectory sample, or a footprint
 * centre) at epoch `t` (A1MJD) -> that same point expressed in `bodyTrack`'s own
 * body-fixed frame (km). `bodyTrack` is one `sc.bodies[i]`-shaped object (`{t, pos,
 * quat, spinRate}`, `BodyTrack.to_dict()`'s wire shape). Pure arithmetic: subtract the
 * body's own (interpolated) position, then apply the INVERSE of the body's own
 * (interpolated) body-fixed-to-scenario-frame quaternion -- `v_bodyFixed = q^-1 *
 * v_scenarioFrame`, the exact inverse of the rotation `BodyTrack`'s own doc comment
 * declares (`v_scenarioFrame = q * v_bodyFixed`).
 * @param {{x:number,y:number,z:number}} posKm
 * @param {object} bodyTrack
 * @param {number} t A1MJD
 * @returns {{x:number,y:number,z:number}} km, body-fixed
 */
// Shared by bodyFixedPositionKm/groundTrack below (a prior version of this file
// duplicated this arithmetic in both -- caught by panels_check.mjs's own "single-point
// API agrees with the batch API" consistency check during this task's break-and-restore
// testing, see web/js/REPORT_M26_4.md: a bug introduced in one copy did not show up in
// the other, which is exactly the risk a shared helper removes). `bodyInterp` is
// supplied by the caller (constructed once per trajectory in `groundTrack`'s loop,
// once per call in `bodyFixedPositionKm`) rather than built here, so `groundTrack`
// keeps its "one BodyInterp per call, not per sample" performance property.
function _bodyFixedFromInterp(posKm, bodyInterp, t) {
  _scPos.set(posKm.x, posKm.y, posKm.z);
  bodyInterp.position(t, _bodyPos);
  bodyInterp.orientation(t, _bodyQuat);
  _rel.copy(_scPos).sub(_bodyPos);
  _rel.applyQuaternion(_bodyQuat.clone().invert());
  return { x: _rel.x, y: _rel.y, z: _rel.z };
}

export function bodyFixedPositionKm(posKm, bodyTrack, t) {
  return _bodyFixedFromInterp(posKm, new BodyInterp(bodyTrack), t);
}

/**
 * `bodyFixedPositionKm` (above), then WGS84 ECEF metres -> geodetic lon/lat/height
 * (`globe_lod.js`'s `ecefToGeodeticDeg`, km converted to metres here -- the one place
 * this module crosses the km<->m boundary). `bodyTrack.radius`/`flattening` are not
 * consulted: WGS84's own constants are what `ecefToGeodeticDeg` uses (matching the
 * globe's own tile geometry, `globe_lod.js`'s module docstring) -- a non-Earth body
 * would need its own ellipsoid constants, out of scope for this task (Earth is the only
 * body the demo runs and M19.5's offline imagery fixture cover).
 * @returns {{lonDeg:number, latDeg:number, altM:number}}
 */
export function lonLatFromScenarioFramePosition(posKm, bodyTrack, t) {
  const bf = bodyFixedPositionKm(posKm, bodyTrack, t);
  const { lonDeg, latDeg, heightM } = ecefToGeodeticDeg(bf.x * KM_TO_M, bf.y * KM_TO_M, bf.z * KM_TO_M);
  return { lonDeg, latDeg, altM: heightM };
}

/**
 * The full ground track for one spacecraft trajectory (`sc.spacecraft[i]`-shaped: `{t,
 * pos}`, flat km arrays -- `Trajectory.to_dict()`'s wire shape): one `{t, lonDeg,
 * latDeg}` per RECORDED sample (not densified/interpolated -- the same discrete set of
 * epochs the trajectory itself carries, mirroring how `web/js/app.js`'s event list/
 * ticks use each event's own recorded `t` directly rather than resampling).
 * `bodyInterp` is constructed once and reused across every sample (this function's own
 * hot path -- avoids reallocating `BodyInterp`'s internal scratch quaternions per
 * sample the way calling `lonLatFromScenarioFramePosition` per-sample would).
 * @param {{t:number[], pos:number[]}} trajectory
 * @param {object} bodyTrack
 * @returns {{t:number, lonDeg:number, latDeg:number}[]}
 */
export function groundTrack(trajectory, bodyTrack) {
  const bodyInterp = new BodyInterp(bodyTrack);
  const out = [];
  const n = trajectory.t.length;
  for (let i = 0; i < n; i++) {
    const t = trajectory.t[i];
    const posKm = { x: trajectory.pos[3 * i], y: trajectory.pos[3 * i + 1], z: trajectory.pos[3 * i + 2] };
    const bf = _bodyFixedFromInterp(posKm, bodyInterp, t);
    const { lonDeg, latDeg } = ecefToGeodeticDeg(bf.x * KM_TO_M, bf.y * KM_TO_M, bf.z * KM_TO_M);
    out.push({ t, lonDeg, latDeg });
  }
  return out;
}

/**
 * The "current position" ground point at an arbitrary epoch `t` (not necessarily a
 * recorded sample) -- interpolates the spacecraft's own trajectory with
 * `TrajectoryInterp` (Hermite-with-velocity, the same class/contract every other
 * spacecraft-position consumer in this codebase uses -- `web/js/scene.js`'s own marker
 * positioning) rather than nearest-sample, so the map's "current position" dot agrees
 * with where the 3D view's own marker is at the same clock time.
 * @param {{t:number[], pos:number[], vel:number[]}} trajectory
 * @param {object} bodyTrack
 * @param {number} t A1MJD
 * @returns {{lonDeg:number, latDeg:number, altM:number}}
 */
export function currentGroundPosition(trajectory, bodyTrack, t) {
  const scInterp = new TrajectoryInterp(trajectory);
  const pos = new THREE.Vector3();
  scInterp.at(t, pos);
  return lonLatFromScenarioFramePosition({ x: pos.x, y: pos.y, z: pos.z }, bodyTrack, t);
}

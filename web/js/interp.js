// Interpolation of sampled trajectories and body tracks.
import * as THREE from 'three';

const SEC_PER_DAY = 86400;

// ---------------------------------------------------------------------------------------
// Interpolation by declared StateSpace component class (ADR-005 sec 3, M7.1,
// docs/open-questions.md question 88). Mirrors
// crates/av-kernel/src/interpolate.rs::{classify, interpolate_by_state_space} label-for-
// label and unit-for-unit -- the same declared `StateSpace` (protobuf-JSON-transcoded,
// the scene JSON's additive `stateSpaces` list, altavista/scenario.py's
// `_build_state_spaces`) classifies identically on both sides of the Rust/Python/JS
// boundary. Used when a raw `mean` vector (a CDM `TrajectorySample`, or its JSON
// transcoding) needs interpolating generically -- today's live viewer path instead
// consumes altavista's already-shaped `{pos, vel, attitude}` JSON (TrajectoryInterp /
// QuaternionTrackInterp below, unaffected by any of this), but ADR-006's scene service
// is explicitly headed toward the browser receiving raw CDM Trajectory samples, and this
// is the class-aware interpolator that path needs, tested standalone here rather than
// invented only when that day comes.
export class InterpolationError extends Error {
  constructor(message, detail) {
    super(message);
    this.name = 'InterpolationError';
    Object.assign(this, detail || {});
  }
}

const POSITION_VELOCITY_LABELS = ['pos_x', 'pos_y', 'pos_z', 'vel_x', 'vel_y', 'vel_z'];
const POSITION_VELOCITY_UNITS = [
  'UNIT_METER', 'UNIT_METER', 'UNIT_METER',
  'UNIT_METER_PER_SECOND', 'UNIT_METER_PER_SECOND', 'UNIT_METER_PER_SECOND',
];
const QUATERNION_LABELS = ['q_x', 'q_y', 'q_z', 'q_w'];

// Every physical Unit core.proto declares except UNIT_UNSPECIFIED (never classifiable --
// see classifyStateSpace's doc comment) is a "rates, masses, scalars" candidate, per
// ADR-005 sec 3 -- mirrors interpolate.rs::is_linear_scalar_unit (which accepts the same
// set: everything but Unspecified) rather than listing each unit name twice.
const NEVER_LINEAR_UNITS = new Set(['UNIT_UNSPECIFIED']);

function isDiscreteLabel(label) {
  const l = (label || '').toLowerCase();
  return l.includes('mode') || l.includes('count');
}

function isNeverInterpolatedLabel(label) {
  const l = (label || '').toLowerCase();
  return l.startsWith('phi_') || l.startsWith('stm_') || l.startsWith('cov_');
}

function componentUnit(c) {
  return c.unit || 'UNIT_UNSPECIFIED';
}

function isPositionVelocityPrefix(comps) {
  for (let k = 0; k < 6; k++) {
    if (comps[k].label !== POSITION_VELOCITY_LABELS[k]) return false;
    if (componentUnit(comps[k]) !== POSITION_VELOCITY_UNITS[k]) return false;
  }
  return true;
}

function isQuaternionGroup(comps) {
  for (let k = 0; k < 4; k++) {
    if (comps[k].label !== QUATERNION_LABELS[k]) return false;
    if (componentUnit(comps[k]) !== 'UNIT_DIMENSIONLESS') return false;
  }
  return true;
}

/**
 * Classify every component of a declared StateSpace (`{id, components: [{label, unit},
 * ...]}`, the protobuf-JSON transcoding this codebase's scene-JSON `stateSpaces` key and
 * `google.protobuf.json_format.MessageToDict` both produce) into contiguous groups, in
 * order, covering every component exactly once -- mirrors
 * crates/av-kernel/src/interpolate.rs::classify's table (position/velocity, quaternion,
 * linear scalar, zero-order-hold, or a typed refusal) component-for-component. Throws
 * InterpolationError for a component neither convention places, never guessing from
 * index position (the one place ADR-005 itself is positional -- "the first six of a
 * Cartesian space" -- is checked by label+unit here too, not by index alone).
 */
export function classifyStateSpace(stateSpace) {
  const comps = stateSpace.components || [];
  const n = comps.length;
  const groups = [];
  let i = 0;
  if (n >= 6 && isPositionVelocityPrefix(comps)) {
    groups.push({ start: 0, len: 6, cls: 'position_velocity' });
    i = 6;
  }
  while (i < n) {
    if (i + 4 <= n && isQuaternionGroup(comps.slice(i, i + 4))) {
      groups.push({ start: i, len: 4, cls: 'quaternion' });
      i += 4;
      continue;
    }
    const c = comps[i];
    const label = c.label || '';
    if (isNeverInterpolatedLabel(label)) {
      throw new InterpolationError(
        `state space ${stateSpace.id}: component ${i} (${label}) is a state-transition-matrix or covariance element; ADR-005 sec 3 never interpolates these`,
        { stateSpaceId: stateSpace.id, index: i, label });
    }
    const unit = componentUnit(c);
    if (unit === 'UNIT_DIMENSIONLESS' && isDiscreteLabel(label)) {
      groups.push({ start: i, len: 1, cls: 'zero_order_hold' });
    } else if (!NEVER_LINEAR_UNITS.has(unit)) {
      groups.push({ start: i, len: 1, cls: 'linear' });
    } else {
      throw new InterpolationError(
        `state space ${stateSpace.id}: component ${i} (${label}, unit ${unit}) cannot be classified for interpolation`,
        { stateSpaceId: stateSpace.id, index: i, label, unit });
    }
    i += 1;
  }
  return groups;
}

/** Tolerance the unit-norm assertion in interpolateByStateSpace checks against -- matches
 * crates/av-kernel/src/interpolate.rs::QUATERNION_UNIT_NORM_TOL exactly. */
export const QUATERNION_UNIT_NORM_TOL = 1e-6;

function normQuat(q) {
  return Math.hypot(q[0], q[1], q[2], q[3]);
}

const _hq0 = new THREE.Quaternion();
const _hq1 = new THREE.Quaternion();

/** Shortest-path normalized slerp of a scalar-last quaternion `[x,y,z,w]`, reusing
 * THREE.Quaternion's own (shortest-path) slerp implementation -- the exact same one
 * QuaternionTrackInterp below already uses -- rather than a second, hand-rolled formula
 * that could silently drift from it. */
function slerpXyzw(q0, q1, s) {
  _hq0.set(q0[0], q0[1], q0[2], q0[3]);
  _hq1.set(q1[0], q1[1], q1[2], q1[3]);
  _hq0.slerp(_hq1, s);
  return [_hq0.x, _hq0.y, _hq0.z, _hq0.w];
}

/**
 * Cubic-Hermite-with-velocity of one raw 6-vector `[pos_x,pos_y,pos_z,vel_x,vel_y,vel_z]`
 * pair, `t0`/`t1`/`t` in the same units (A1MJD days, matching this file's other classes --
 * `dt` below is converted to seconds exactly like TrajectoryInterp.segment does). Kept as
 * a standalone function (rather than requiring a full TrajectoryInterp instance) so
 * interpolateByStateSpace can apply it to an arbitrary 6-component slice of a StateSpace-
 * declared vector; a cross-check test (`web/js/attitude_slerp_check.mjs` /
 * tests/test_viewer_jitter.py) proves this agrees with TrajectoryInterp.segment on
 * identical input, so the two are not free to silently drift apart.
 */
function hermiteVelocity6(t0, s0, t1, s1, t) {
  const dt = (t1 - t0) * SEC_PER_DAY;
  const s = dt <= 0 ? 0 : ((t - t0) * SEC_PER_DAY) / dt;
  const s2 = s * s, s3 = s2 * s;
  const h00 = 2 * s3 - 3 * s2 + 1, h10 = s3 - 2 * s2 + s, h01 = -2 * s3 + 3 * s2, h11 = s3 - s2;
  const dh00 = 6 * s2 - 6 * s, dh10 = 3 * s2 - 4 * s + 1, dh01 = -6 * s2 + 6 * s, dh11 = 3 * s2 - 2 * s;
  const out = new Array(6);
  for (let i = 0; i < 3; i++) {
    const p0 = s0[i], v0 = s0[3 + i], p1 = s1[i], v1 = s1[3 + i];
    out[i] = h00 * p0 + h10 * dt * v0 + h01 * p1 + h11 * dt * v1;
    out[3 + i] = dt <= 0 ? v0 : (dh00 * p0 + dh10 * dt * v0 + dh01 * p1 + dh11 * dt * v1) / dt;
  }
  return out;
}

/**
 * Interpolate a full StateSpace-declared sample at `t`, given two bracketing samples
 * `(t0, s0)`/`(t1, s1)`, by classifying every component (classifyStateSpace) rather than
 * assuming position by index. Mirrors
 * crates/av-kernel/src/interpolate.rs::interpolate_by_state_space's rules exactly:
 * cubic-Hermite-with-velocity for a declared Cartesian position/velocity prefix,
 * normalized slerp (unit norm asserted) for a `q_x..q_w` group, linear for a scalar,
 * zero-order hold (nearer endpoint) for a discrete mode/counter -- an unclassifiable
 * component is a thrown InterpolationError, never a silent fallback.
 */
export function interpolateByStateSpace(stateSpace, t0, s0, t1, s1, t) {
  const n = (stateSpace.components || []).length;
  if (s0.length !== n || s1.length !== n) {
    throw new InterpolationError(
      `state space ${stateSpace.id}: declares ${n} component(s) but got s0.length=${s0.length}, s1.length=${s1.length}`,
      { stateSpaceId: stateSpace.id });
  }
  const groups = classifyStateSpace(stateSpace);
  const dt = t1 - t0;
  const s = dt === 0 ? 0 : Math.min(1, Math.max(0, (t - t0) / dt));
  const out = new Array(n).fill(0);
  for (const { start, cls } of groups) {
    if (cls === 'position_velocity') {
      const seg = hermiteVelocity6(t0, s0.slice(start, start + 6), t1, s1.slice(start, start + 6), t);
      for (let k = 0; k < 6; k++) out[start + k] = seg[k];
    } else if (cls === 'quaternion') {
      const q0 = s0.slice(start, start + 4), q1 = s1.slice(start, start + 4);
      const n0 = normQuat(q0), n1 = normQuat(q1);
      if (Math.abs(n0 - 1) > QUATERNION_UNIT_NORM_TOL || Math.abs(n1 - 1) > QUATERNION_UNIT_NORM_TOL) {
        throw new InterpolationError(
          `state space ${stateSpace.id}: quaternion group at component ${start} is not unit norm (|q0|=${n0}, |q1|=${n1})`,
          { stateSpaceId: stateSpace.id, index: start, n0, n1 });
      }
      const q = slerpXyzw(q0, q1, s);
      for (let k = 0; k < 4; k++) out[start + k] = q[k];
    } else if (cls === 'linear') {
      out[start] = s0[start] + (s1[start] - s0[start]) * s;
    } else { // zero_order_hold
      out[start] = s < 0.5 ? s0[start] : s1[start];
    }
  }
  return out;
}

/** Index i such that times[i] <= t < times[i+1], clamped to [0, n-2]. */
export function findSegment(t, times) {
  const n = times.length;
  if (n < 2) return 0;
  if (t <= times[0]) return 0;
  if (t >= times[n - 1]) return n - 2;
  let lo = 0, hi = n - 1;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (times[mid] <= t) lo = mid; else hi = mid;
  }
  return lo;
}

/** Cubic Hermite interpolation of a spacecraft track (positions km, velocities km/s). */
export class TrajectoryInterp {
  constructor(track) {
    this.t = track.t;
    this.pos = track.pos;
    this.vel = track.vel;
    this.n = track.t.length;
    this.t0 = this.n ? track.t[0] : 0;
    this.t1 = this.n ? track.t[this.n - 1] : 0;
  }

  /** Position (km) at A1MJD t into `out` (THREE.Vector3); optionally velocity (km/s)
   * into `outVel` too (M5.2: frame axes need both -- see web/js/frames.js's
   * axesRIC/axesVNB/axesVVLH). Clamps outside the span. `outVel` is only computed
   * when supplied, so existing 2-arg callers (scene.js's marker/trajectory
   * positioning) are unaffected. */
  at(t, out, outVel) {
    const n = this.n;
    if (n === 0) {
      if (outVel) outVel.set(0, 0, 0);
      return out.set(0, 0, 0);
    }
    if (n === 1 || t <= this.t0) {
      if (outVel) outVel.set(this.vel[0], this.vel[1], this.vel[2]);
      return out.set(this.pos[0], this.pos[1], this.pos[2]);
    }
    if (t >= this.t1) {
      const k = 3 * (n - 1);
      if (outVel) outVel.set(this.vel[k], this.vel[k + 1], this.vel[k + 2]);
      return out.set(this.pos[k], this.pos[k + 1], this.pos[k + 2]);
    }
    let i = findSegment(t, this.t);
    // skip zero-length segments (impulsive maneuvers duplicate the epoch)
    while (i < n - 2 && this.t[i + 1] - this.t[i] <= 0) i++;
    return this.segment(i, t, out, outVel);
  }

  /** Hermite evaluation on segment i at A1MJD t; optionally the segment's Hermite
   * *derivative* (km/s) into `outVel` -- the exact same cubic evaluated for position,
   * differentiated w.r.t. s and scaled by ds/dt = 1/dt (chain rule), not a separate
   * approximation. At a segment boundary (s=0 or s=1) this reduces exactly to the
   * recorded sample velocity (dh10=1,others=0 at s=0 -> v[a]; dh11=1,others=0 at s=1
   * -> v[b]), which web/js/fixtures/gen_ric_fixture.py's fixture relies on. */
  segment(i, t, out, outVel) {
    const ta = this.t[i], tb = this.t[i + 1];
    const dt = (tb - ta) * SEC_PER_DAY;
    if (dt <= 0) {
      const k = 3 * (i + 1);
      if (outVel) outVel.set(this.vel[k], this.vel[k + 1], this.vel[k + 2]);
      return out.set(this.pos[k], this.pos[k + 1], this.pos[k + 2]);
    }
    const s = Math.min(1, Math.max(0, ((t - ta) * SEC_PER_DAY) / dt));
    const s2 = s * s, s3 = s2 * s;
    const h00 = 2 * s3 - 3 * s2 + 1, h10 = s3 - 2 * s2 + s, h01 = -2 * s3 + 3 * s2, h11 = s3 - s2;
    const a = 3 * i, b = 3 * (i + 1);
    const p = this.pos, v = this.vel;
    out.x = h00 * p[a] + h10 * dt * v[a] + h01 * p[b] + h11 * dt * v[b];
    out.y = h00 * p[a + 1] + h10 * dt * v[a + 1] + h01 * p[b + 1] + h11 * dt * v[b + 1];
    out.z = h00 * p[a + 2] + h10 * dt * v[a + 2] + h01 * p[b + 2] + h11 * dt * v[b + 2];
    if (outVel) {
      const dh00 = 6 * s2 - 6 * s, dh10 = 3 * s2 - 4 * s + 1, dh01 = -6 * s2 + 6 * s, dh11 = 3 * s2 - 2 * s;
      const inv = 1 / dt;
      outVel.x = (dh00 * p[a] + dh10 * dt * v[a] + dh01 * p[b] + dh11 * dt * v[b]) * inv;
      outVel.y = (dh00 * p[a + 1] + dh10 * dt * v[a + 1] + dh01 * p[b + 1] + dh11 * dt * v[b + 1]) * inv;
      outVel.z = (dh00 * p[a + 2] + dh10 * dt * v[a + 2] + dh01 * p[b + 2] + dh11 * dt * v[b + 2]) * inv;
    }
    return out;
  }

  /**
   * Densified polyline for drawing: returns {points: Float64Array (km, xyz...), times: Float64Array}.
   * Each sample segment is subdivided so consecutive points subtend < ~1.5 deg from the origin.
   *
   * `points` is kept in double precision (Float64Array, not Float32Array) deliberately: these
   * are *absolute* coordinates that can reach Mars-distance magnitudes (~2.3e5 scene units), and
   * the whole point of web/js/origin.js's floating-origin pipeline is to subtract a nearby f64
   * origin from a f64 position before ever casting to f32 for the GPU (see origin.js's module
   * docstring). Returning a Float32Array here would round these points down to float32 before
   * scene.js/origin.js ever got a chance to subtract the origin, silently defeating the fix --
   * the precision lost at this step can never be recovered downstream.
   */
  polyline(maxSub = 48) {
    const pts = [], times = [];
    const tmp = new THREE.Vector3();
    if (this.n === 0) return { points: new Float64Array(0), times: new Float64Array(0) };
    pts.push(this.pos[0], this.pos[1], this.pos[2]);
    times.push(this.t[0]);
    for (let i = 0; i < this.n - 1; i++) {
      const ta = this.t[i], tb = this.t[i + 1];
      const dt = (tb - ta) * SEC_PER_DAY;
      if (dt <= 0) continue;
      const a = 3 * i;
      const r = Math.hypot(this.pos[a], this.pos[a + 1], this.pos[a + 2]) || 1;
      const vmag = Math.hypot(this.vel[a], this.vel[a + 1], this.vel[a + 2]);
      const angle = (vmag * dt) / r;          // radians swept (approx.)
      const sub = Math.max(1, Math.min(maxSub, Math.ceil(angle / 0.025)));
      for (let k = 1; k <= sub; k++) {
        const t = ta + (tb - ta) * (k / sub);
        this.segment(i, t, tmp);
        pts.push(tmp.x, tmp.y, tmp.z);
        times.push(t);
      }
    }
    return { points: new Float64Array(pts), times: new Float64Array(times) };
  }
}

/** Body position (linear) and orientation (slerp + spin about the pole between samples). */
export class BodyInterp {
  constructor(body) {
    this.t = body.t;
    this.pos = body.pos;
    this.quat = body.quat;
    this.n = body.t.length;
    this.spinRate = body.spinRate || 0;      // deg/day
    this._qa = new THREE.Quaternion();
    this._qb = new THREE.Quaternion();
    this._qz = new THREE.Quaternion();
    this._z = new THREE.Vector3(0, 0, 1);
  }

  position(t, out) {
    const n = this.n;
    if (n === 0) return out.set(0, 0, 0);
    if (n === 1 || t <= this.t[0]) return out.set(this.pos[0], this.pos[1], this.pos[2]);
    if (t >= this.t[n - 1]) {
      const k = 3 * (n - 1);
      return out.set(this.pos[k], this.pos[k + 1], this.pos[k + 2]);
    }
    const i = findSegment(t, this.t);
    const s = (t - this.t[i]) / (this.t[i + 1] - this.t[i]);
    const a = 3 * i, b = 3 * (i + 1);
    out.x = this.pos[a] + (this.pos[b] - this.pos[a]) * s;
    out.y = this.pos[a + 1] + (this.pos[b + 1] - this.pos[a + 1]) * s;
    out.z = this.pos[a + 2] + (this.pos[b + 2] - this.pos[a + 2]) * s;
    return out;
  }

  _sampleQuat(i, t, out) {
    const k = 4 * i;
    out.set(this.quat[k], this.quat[k + 1], this.quat[k + 2], this.quat[k + 3]);
    if (this.spinRate) {
      const ang = THREE.MathUtils.degToRad(this.spinRate * (t - this.t[i]));
      this._qz.setFromAxisAngle(this._z, ang);
      out.multiply(this._qz);   // spin about the body's own pole
    }
    return out;
  }

  orientation(t, out) {
    const n = this.n;
    if (n === 0 || this.quat.length < 4) return out.identity();
    if (n === 1 || t <= this.t[0]) return this._sampleQuat(0, t, out);
    if (t >= this.t[n - 1]) return this._sampleQuat(n - 1, t, out);
    const i = findSegment(t, this.t);
    const s = (t - this.t[i]) / (this.t[i + 1] - this.t[i]);
    this._sampleQuat(i, t, this._qa);
    this._sampleQuat(i + 1, t, this._qb);
    return out.copy(this._qa).slerp(this._qb, s);
  }
}

/**
 * Slerped quaternion track: `{t: [...], quat: [[x,y,z,w], ...]}` sampled at A1MJD
 * epochs -- a *real* attitude stream (as opposed to a derived/fallback orientation),
 * M5.2's groundwork for sensor footprints. `web/js/frames.js`'s per-entity body-frame
 * node (`FrameNode.setAttitudeTrack`) uses this when the scenario JSON supplies one
 * (`altavista/model.py`'s additive `Trajectory.attitude`), falling back to a
 * nadir-pointing VVLH computed from the entity's own state otherwise -- see
 * `web/js/scene.js`'s `_buildFrameGraph` for where that fallback decision is made and
 * labelled. Slerp only (no Hermite/derivative): unlike position/velocity, a
 * quaternion stream carries no separately-recorded angular-rate sample to fit a
 * cubic to, so plain great-circle interpolation between consecutive samples is the
 * honest choice here -- the same one `BodyInterp.orientation` above already makes for
 * body orientation, not a new approximation invented for this class.
 */
export class QuaternionTrackInterp {
  constructor(track) {
    this.t = track.t;
    this.quat = track.quat;
    this.n = track.t.length;
    this._qa = new THREE.Quaternion();
    this._qb = new THREE.Quaternion();
  }

  _sample(i, out) {
    const k = 4 * i;
    return out.set(this.quat[k], this.quat[k + 1], this.quat[k + 2], this.quat[k + 3]);
  }

  at(t, out) {
    const n = this.n;
    if (n === 0) return out.identity();
    if (n === 1 || t <= this.t[0]) return this._sample(0, out);
    if (t >= this.t[n - 1]) return this._sample(n - 1, out);
    const i = findSegment(t, this.t);
    const s = (t - this.t[i]) / (this.t[i + 1] - this.t[i]);
    this._sample(i, this._qa);
    this._sample(i + 1, this._qb);
    return out.copy(this._qa).slerp(this._qb, s);
  }
}

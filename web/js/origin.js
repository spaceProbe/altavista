// Configurable floating origin.
//
// The problem (see web/js/scene.js): every position the existing viewer draws is
// kept as an *absolute* scene coordinate (scene units = km * SCALE, SCALE = 1e-3),
// and trajectory geometry is written straight into a `Float32Array`
// (`LineGeometry.setPositions(...)` in scene.js). A 32-bit float has ~7.2 decimal
// digits of precision; once the absolute magnitude of a coordinate grows past a few
// hundred scene units (Moon distance is ~384 scene units, Mars distance ~2.3e5), the
// float32 quantization step itself is metres to tens of kilometres, independent of
// anything the GPU later does with the value. That quantization is what this module
// removes for any node that opts in.
//
// The fix is standard "floating origin" / relative-to-eye rendering: keep the
// authoritative position in f64 (a plain JS `number`, which *is* f64 -- never round
// that value through a Float32Array before subtracting), subtract a nearby f64
// *origin* from it in double precision, and only convert the small remainder to f32
// for the GPU. Because the remainder is small, the float32 quantization step on it is
// small too.
//
// This module is deliberately framework-free (no Three.js import): it is the one
// place the arithmetic lives, so the shipped renderer and tests/test_viewer_jitter.py
// (invoked over `node`, see that file) exercise the exact same code, not a
// reimplementation of it.

/** @typedef {{x: number, y: number, z: number}} Vec3 */

/**
 * Tracks one f64 origin per frame graph node (keyed by frame id), plus a fallback
 * "global" origin (key `null`) and a per-frame enable/disable flag that falls back to
 * a global switch. This is what makes the floating origin "configurable for every
 * frame" (docs/open-questions.md Q46): a caller can turn it off for one frame (e.g. to
 * A/B it, or because that frame's own scale never needs it) without touching any
 * other frame, and the global flag is the one-line kill switch.
 */
export class FloatingOrigin {
  constructor({ globalEnabled = true } = {}) {
    /** @type {Map<string, Vec3>} frameId -> origin (f64) */
    this._origins = new Map();
    /** @type {Map<string, boolean>} frameId -> enabled override */
    this._enabled = new Map();
    this.globalEnabled = globalEnabled;
  }

  /** Per-frame override; `null`/`undefined` clears the override (fall back to global). */
  setEnabledForFrame(frameId, enabled) {
    if (enabled === null || enabled === undefined) this._enabled.delete(frameId);
    else this._enabled.set(frameId, !!enabled);
  }

  isEnabledForFrame(frameId) {
    return this._enabled.has(frameId) ? this._enabled.get(frameId) : this.globalEnabled;
  }

  /** Re-base: set frame `frameId`'s origin to an f64 point (e.g. the newly-focused
   * entity's current absolute position). Cheap and exact -- this is a plain object
   * assignment, no geometry touched, callable every time the camera's focus changes
   * or (for a moving frame) whenever drift from the current origin gets large. */
  setOrigin(frameId, x, y, z) {
    this._origins.set(frameId, { x, y, z });
  }

  /** Current origin for a frame; `{0,0,0}` if never set (equivalent to "no shift"). */
  getOrigin(frameId) {
    return this._origins.get(frameId) || { x: 0, y: 0, z: 0 };
  }

  /**
   * The render-space (GPU-ready, f32-precision) coordinates of an absolute f64
   * position `pos` under frame `frameId`. All subtraction happens in f64 first
   * (`pos.x - origin.x` on plain JS numbers); `Math.fround` is applied once, last,
   * to model exactly what happens the instant this value is written into a
   * `Float32Array` vertex buffer or uploaded as a GPU uniform -- it does not
   * introduce any additional rounding beyond what the GPU pipeline already performs.
   *
   * When the frame's floating origin is disabled (`isEnabledForFrame` false), the
   * origin used is `{0,0,0}`, i.e. this degenerates to `Math.fround(pos)` -- exactly
   * today's scene.js behaviour of storing the absolute coordinate directly. That is
   * intentional: disabling floating origin for a frame should reproduce the
   * pre-existing behaviour precisely, not a different approximation.
   *
   * @param {string} frameId
   * @param {Vec3} pos absolute position, f64, scene units
   * @returns {Vec3} render-space position, f32-precision
   */
  toRenderSpace(frameId, pos) {
    const origin = this.isEnabledForFrame(frameId) ? this.getOrigin(frameId) : { x: 0, y: 0, z: 0 };
    return {
      x: Math.fround(pos.x - origin.x),
      y: Math.fround(pos.y - origin.y),
      z: Math.fround(pos.z - origin.z),
    };
  }

  /**
   * Batch form of `toRenderSpace` for a flat `[x0,y0,z0,x1,y1,z1,...]` f64 array
   * (what interp.js's `TrajectoryInterp.polyline()` produces before scaling) --
   * the intended drop-in replacement for scene.js's:
   *   `scaled[i] = poly.points[i] * SCALE`
   * which today writes the *absolute* coordinate straight into a Float32Array.
   * Here the SCALE multiply is folded into `pos` (caller pre-scales, or pass a
   * `scale` factor) and the origin subtraction happens before the f32 cast.
   *
   * @param {string} frameId
   * @param {ArrayLike<number>} pointsXYZ flat absolute positions (already in scene units)
   * @returns {Float32Array}
   */
  toRenderSpaceArray(frameId, pointsXYZ) {
    const origin = this.isEnabledForFrame(frameId) ? this.getOrigin(frameId) : { x: 0, y: 0, z: 0 };
    const n = pointsXYZ.length;
    const out = new Float32Array(n);
    for (let i = 0; i < n; i += 3) {
      out[i] = Math.fround(pointsXYZ[i] - origin.x);
      out[i + 1] = Math.fround(pointsXYZ[i + 1] - origin.y);
      out[i + 2] = Math.fround(pointsXYZ[i + 2] - origin.z);
    }
    return out;
  }
}

/**
 * The baseline this module replaces, kept here (not reimplemented in the test) so
 * tests/test_viewer_jitter.py can prove the floating-origin path is actually
 * necessary rather than assuming it. Mirrors scene.js's current, unmodified
 * behaviour: the absolute position is written into a Float32Array vertex buffer
 * (`Math.fround(pos)`) and the camera's own world position ends up in the
 * modelView matrix's translation the same way once uploaded as a GPU uniform
 * (`Math.fround(eye)`); the GPU then differences the two in float32 hardware.
 * Both roundings are modelled explicitly rather than skipped, since skipping either
 * one would understate the real error.
 *
 * @param {Vec3} pos absolute position, f64, scene units
 * @param {Vec3} eye absolute camera position, f64, scene units
 * @returns {Vec3} what ends up on screen, f32-precision, camera-relative
 */
export function toRenderSpaceNoOrigin(pos, eye) {
  const px = Math.fround(pos.x), py = Math.fround(pos.y), pz = Math.fround(pos.z);
  const ex = Math.fround(eye.x), ey = Math.fround(eye.y), ez = Math.fround(eye.z);
  return { x: Math.fround(px - ex), y: Math.fround(py - ey), z: Math.fround(pz - ez) };
}

/** Exact (f64) displacement between two absolute positions -- the ground truth a
 * jitter measurement compares a render-space result against. Never rounded. */
export function trueRelative(pos, eye) {
  return { x: pos.x - eye.x, y: pos.y - eye.y, z: pos.z - eye.z };
}

/** Euclidean length of a Vec3 (f64). */
export function length(v) {
  return Math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z);
}

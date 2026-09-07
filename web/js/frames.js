// The viewer's frame graph.
//
// docs/architecture.md §4 "Presentation plane" describes this module: "a scene node
// per declared frame, transforms from the frame service, the camera parented to any
// frame (Earth-fixed, inertial, body-centred, spacecraft-relative). Switching frames
// is re-parenting, not re-loading."
//
// One `FrameNode` wraps one Three.js `Group` per `FrameDefinition` (proto
// altavista.v1.core.FrameDefinition). Nodes are arranged as a tree via each
// definition's `parentId` (a viewer-side field: the CDM's `FrameDefinition` itself
// has no explicit parent pointer today -- axes/origin implies one, e.g. an
// AXES_KIND_RIC frame's natural parent is its `reference_body`'s inertial frame; the
// scene service is expected to supply `parentId` alongside each definition). A
// frame's Group transform composes with its parent's automatically through Three's
// normal scene-graph matrix chain -- that composition is not reimplemented here.
//
// A frame whose origin moves (its `entity_id` is a spacecraft, e.g. an RIC/VNB/VVLH
// frame) gets its position from a sampled track using the CDM's declared
// interpolation contract: Hermite with velocity. That is exactly interp.js's
// `TrajectoryInterp`, reused here rather than re-implemented, so a frame's own motion
// and a spacecraft's rendered trajectory never disagree about how to interpolate
// between two samples.
import * as THREE from 'three';
import { TrajectoryInterp, QuaternionTrackInterp } from './interp.js';

// ---------------------------------------------------------------------------- axes
// M5.2: client-side RIC/VNB/VVLH axes, computed from a reference entity's
// interpolated state `r` (position, any consistent unit -- direction only, so km vs.
// scene units never matters) and `v` (velocity, same frame). Conventions pinned by
// proto/altavista/v1/core.proto's AxesKind doc comments (read-only ground truth for
// this task) and realized server-side (GMAT) by altavista/frames.py's
// `_OBJECT_REFERENCED_FIELDS` (also read-only) -- this is the same convention,
// re-derived here so the browser can rotate a frame node every render tick without a
// round trip to the frame service:
//
//   RIC:  X = R = unit(r),         Z = N = unit(r x v),  Y = Z x X (= N x R, in-track)
//   VNB:  X = V = unit(v),         Y = N = unit(r x v),   Z = X x Y
//   VVLH: Z = -R = -unit(r),       Y = -N = -unit(r x v), X = Y x Z (= N x R)
//
// Each function fills three *unit* output vectors (already-allocated scratch, reused
// by the caller -- these run every render tick, so no per-call allocation). The
// result is right-handed and orthonormal by construction (two axes are built from a
// unit vector and a cross product of two already-orthogonal unit vectors; the third
// completes the set via one more cross product), which is exactly what
// `web/js/fixtures/gen_ric_fixture.py` / `web/js/ric_axes_check.mjs` check against
// GMAT's own `CoordinateConverter`-realized ObjectReferenced axes to 1e-9.
/** RIC (radial / in-track / cross-track): X=R, Z=N. */
export function axesRIC(r, v, outX, outY, outZ) {
  outX.copy(r).normalize();
  outZ.copy(r).cross(v).normalize();
  outY.copy(outZ).cross(outX); // Y = Z x X; already unit (Z, X orthonormal)
}

/** VNB (velocity / normal / binormal): X=V, Y=N. */
export function axesVNB(r, v, outX, outY, outZ) {
  outX.copy(v).normalize();
  outY.copy(r).cross(v).normalize();
  outZ.copy(outX).cross(outY); // Z = X x Y
}

/** VVLH (local-vertical/local-horizontal), pinned 2026-09-02 (question 73):
 * Z=-R (nadir), Y=-N, X = Y x Z (= N x R, in-track). */
export function axesVVLH(r, v, outX, outY, outZ) {
  outZ.copy(r).normalize().negate();
  outY.copy(r).cross(v).normalize().negate();
  outX.copy(outY).cross(outZ); // X = Y x Z
}

const AXES_FUNCS = { ric: axesRIC, vnb: axesVNB, vvlh: axesVVLH };

/** Dispatch to `axesRIC`/`axesVNB`/`axesVVLH` by lowercase kind string. */
export function axesForKind(kind, r, v, outX, outY, outZ) {
  const fn = AXES_FUNCS[kind];
  if (!fn) throw new Error(`frames.js: unknown axes kind '${kind}' (expected ric/vnb/vvlh)`);
  fn(r, v, outX, outY, outZ);
}

const _basisM4 = new THREE.Matrix4();

/**
 * Build the child(local frame)-to-parent quaternion from three orthonormal axis unit
 * vectors already expressed in the parent frame's coordinates -- i.e. `outX`/`outY`/
 * `outZ` are exactly what `axesForKind` produces. `THREE.Matrix4.makeBasis` sets the
 * matrix's *columns* to `[x, y, z]`, which is precisely the "local axes expressed in
 * parent coordinates" transform `Object3D.quaternion` expects (parent = the frame
 * node's own parent in the graph; the child's local +X/+Y/+Z point along
 * `outX`/`outY`/`outZ` once this quaternion is applied) -- not derived by hand here
 * a second time, reused from THREE's own (tested) basis/quaternion conversion.
 */
export function quaternionFromAxes(outX, outY, outZ, out) {
  _basisM4.makeBasis(outX, outY, outZ);
  return out.setFromRotationMatrix(_basisM4);
}

// ------------------------------------------------------- fixed_rotation_q (M19.2, question 129)
// DISPLAY CONVENIENCE, NOT A SECOND SOURCE OF TRUTH. Since ADR-002's fourth amendment
// (question 128) the producer emits a trajectory in its declared frame by converting EVERY
// sample through GMAT's CoordinateConverter::Convert. Those converted samples are
// authoritative; this fixed rotation only lets the viewer re-view an already-converted
// trajectory in a sibling inertial frame without a round trip to the producer. Where the two
// disagree, the samples win.
//
// Disclosed magnitude of that disagreement: the ICRF/FK5 bias is physically constant, but GMAT
// realizes it by Lagrange-interpolating a table (AxisSystem.cpp's RotationMatrixFromICRFToFK5,
// ICRFFile.cpp) carrying a measured ~1.78e-14 rad/s linear residual. The producer converts each
// sample against that time-varying interpolation while this code applies ONE fixed quaternion for
// the whole run, so the two drift apart by ~1.5e-9 rad over a day -- of order 1 cm at LEO radius
// over a day-long run, above the repository's 1e-4 m golden tolerance class. Sub-centimetre
// viewing geometry must come from the converted samples, not from applying this quaternion to a
// trajectory expressed in another frame. See altavista/FRAMES.md for the full account.
// The tolerance FrameDefinition.fixed_rotation_q (core.proto field 13) must satisfy to be
// accepted as a genuine unit quaternion -- mirrors av-kernel's own
// FIXED_ROTATION_UNIT_NORM_TOLERANCE (crates/av-kernel/src/drm/executor.rs): loose next to the
// ~1e-15 floating-point noise a correct computation produces, tight enough to catch a real bug.
const FIXED_ROTATION_UNIT_NORM_TOLERANCE = 1e-9;

/**
 * The client-side half of `fixed_rotation_q`'s wire contract (`core.proto` field 13: "Exactly 0
 * or 4 entries" scalar-first `[w, x, y, z]`, unit if non-empty) -- the producer
 * (`av-kernel::drm::executor::validate_fixed_rotation_q`) enforces the identical rule before
 * this ever reaches the wire, but a consumer trusting "a consumer needs nothing outside the
 * bundle" (docs/architecture.md) must not silently paper over a violation either: this throws,
 * never truncates/pads/renormalizes, on a malformed value.
 *
 * **Direction: the wire value is inverted before it becomes `object3D.quaternion`.** The wire's
 * own contract is "parent -> this": `av-kernel` computes it as GMAT's own `CoordinateConverter::
 * Convert(from=parent, to=this)` rotation, i.e. `v_this = q_wire * v_parent` (`altavista/frames.py`'s
 * `FrameRegistry.rotation_matrix(from_id, to_id, ...)` is the identical GMAT convention, read-only
 * ground truth for `ric_axes_fixture.json`). `THREE.Object3D`'s own convention is the opposite:
 * a node's `quaternion` maps its *local* coordinates into its *parent*'s, `v_parent = q * v_local`
 * -- proven by this same file's `quaternionFromAxes`/`axesForKind` (RIC/VNB/VVLH), whose output is
 * verified byte-for-byte against `altavista.frames.FrameRegistry.rotation_matrix(from=parent,
 * to=child)`'s matrix *columns* (`ric_axes_fixture.json`'s own `rotationConvention`: "axes kind's
 * i-th basis vector expressed in [the parent] coordinates" -- i.e. exactly `q_wire^{-1}`'s own
 * rotation, from=child/this, to=parent). So `v_parent = q_wire^{-1} * v_local(this)`, and this
 * function returns `q_wire^{-1}` (`.invert()`, the conjugate of a unit quaternion -- exact, no
 * numerical loss) rather than the raw wire value, so `FrameNode.update()` can `.copy()` it onto
 * `object3D.quaternion` directly, exactly like the RIC/attitude cases already do.
 *
 * Returns `null` for an absent/empty `fixedRotationQ` (no fixed rotation declared for this
 * frame -- e.g. the body's own MJ2000Eq, which is the reference itself). Otherwise returns a
 * `THREE.Quaternion` built from the wire's scalar-first order and then inverted as above;
 * `THREE.Quaternion`'s own constructor is scalar-LAST (x, y, z, w), so the reordering happens
 * here, once, rather than at every call site.
 *
 * @param {number[]|null|undefined} fixedRotationQ wire `[w, x, y, z]` or empty/absent.
 * @param {string} frameId for the error message only.
 * @returns {THREE.Quaternion|null}
 */
export function fixedRotationQuaternion(fixedRotationQ, frameId) {
  if (!fixedRotationQ || fixedRotationQ.length === 0) return null;
  if (fixedRotationQ.length !== 4) {
    throw new Error(`frame '${frameId}': fixedRotationQ has ${fixedRotationQ.length} entries; must be exactly 0 or 4`);
  }
  const [w, x, y, z] = fixedRotationQ;
  const norm = Math.sqrt(w * w + x * x + y * y + z * z);
  if (Math.abs(norm - 1.0) > FIXED_ROTATION_UNIT_NORM_TOLERANCE) {
    throw new Error(`frame '${frameId}': fixedRotationQ norm ${norm} is not within ${FIXED_ROTATION_UNIT_NORM_TOLERANCE} of 1.0 (a unit quaternion is required)`);
  }
  return new THREE.Quaternion(x, y, z, w).invert();
}

export class FrameNode {
  /** @param {{id: string, parentId?: string|null, axesKind?: string|null}} def a
   *   FrameDefinition-shaped object; `axesKind` (M5.2) is `'ric'|'vnb'|'vvlh'|null`,
   *   normalized from the wire `AxesKind` enum name by scene.js's `_buildFrameGraph`. */
  constructor(def) {
    this.def = def;
    this.id = def.id;
    this.object3D = new THREE.Group();
    this.object3D.name = `frame:${def.id}`;
    this._originTrack = null; // TrajectoryInterp | null: only set for a moving-origin frame
    this._attitudeTrack = null; // QuaternionTrackInterp | null: a real attitude stream (M5.2 body frames)
    // M5.2: when set (and `_originTrack` is present), `update()` derives this node's
    // quaternion every tick from the origin track's own interpolated (r, v) via
    // axesForKind -- this is exactly the RIC/VNB/VVLH case, since a declared
    // entity-relative frame's reference entity IS its origin entity
    // (altavista/scenario.py's `_declare_object_referenced_frame`:
    // `reference_entity_id == entity_id`), so the same track already gives both.
    this.axesKind = def.axesKind || null;
    // M19.2 (question 129, E-24): a body-centred inertial frame's constant rotation relative
    // to its own body's MJ2000Eq (e.g. ICRF's frame bias) -- null when `def.fixedRotationQ` is
    // absent/empty (no fixed rotation declared for this frame, e.g. MJ2000Eq itself, the
    // reference). Validated once here, not re-validated every render tick.
    this._fixedRotationQ = fixedRotationQuaternion(def.fixedRotationQ, def.id);
    this._tmp = new THREE.Vector3();    // scratch: unscaled (km) position -- also `r` for axesForKind
    this._tmpVel = new THREE.Vector3(); // scratch: velocity (km/s) -- `v` for axesForKind
    this._ax = new THREE.Vector3();
    this._ay = new THREE.Vector3();
    this._az = new THREE.Vector3();
    this._tmpQ = new THREE.Quaternion();
  }

  /**
   * Declare this frame's origin motion as a sampled track: `{t, pos, vel}` with `t`
   * A1MJD and flat `pos`/`vel` arrays in km / km-s -- the same shape
   * `TrajectoryInterp` (interp.js) already consumes for spacecraft trajectories.
   * Pass `null` for a frame whose origin does not move relative to its parent
   * (e.g. a body-fixed or ICRF frame, whose parent already carries the motion, or a
   * frame fixed to its parent by construction).
   */
  setOriginTrack(track) {
    this._originTrack = track ? new TrajectoryInterp(track) : null;
  }

  /**
   * M5.2: declare a *real* attitude quaternion stream for this frame (a per-entity
   * body frame with actual attitude data, as opposed to one whose orientation is
   * derived from RIC/VNB/VVLH axes math) -- `{t: [...], quat: [[x,y,z,w], ...]}`,
   * A1MJD epochs. Takes priority over `axesKind`-derived orientation in `update()`
   * when set (see that method). Pass `null` to clear it (falls back to `axesKind`,
   * e.g. nadir-pointing VVLH -- `web/js/scene.js`'s `_buildFrameGraph` decides which
   * one applies per entity and labels the fallback in the frame's `description`, so
   * the UI never silently substitutes one for the other).
   */
  setAttitudeTrack(track) {
    this._attitudeTrack = track ? new QuaternionTrackInterp(track) : null;
  }

  /**
   * Update this node's local transform (relative to its parent frame) for epoch `t`
   * (A1MJD). `scale` converts km to scene units (`SCALE` in scene.js). Orientation
   * priority: (1) a real attitude stream (`setAttitudeTrack`), slerped; (2)
   * `axesKind`-derived RIC/VNB/VVLH axes from this node's own origin track (M5.2 --
   * the whole point of this task: a camera parented to this node's `object3D` now
   * rotates with it, since Three's normal parent-child matrix composition applies
   * this quaternion to every descendant); (3) a constant `fixedRotationQ` (M19.2,
   * question 129) -- a body-centred inertial frame with no origin track and no
   * `axesKind` (ICRF, MJ2000Ec) applies its own fixed bias rotation relative to its
   * body's MJ2000Eq every tick, closing the E-24 defect (such a node used to keep
   * identity orientation forever); (4) an explicitly-supplied `quaternion` argument
   * (attitude sourced elsewhere, e.g. a future frame/attitude service -- unused by any
   * current caller, kept for that case). A frame with none of the above keeps whatever
   * orientation it already had (identity, for every frame kind this module builds
   * today apart from RIC/VNB/VVLH, body frames, and now a fixed-rotation frame).
   */
  update(t, scale, quaternion) {
    if (this._originTrack) {
      // Fill velocity too only when this node actually needs it (axesKind set) --
      // TrajectoryInterp.at()'s outVel param is skipped entirely otherwise, so a
      // plain translating frame (no axesKind) pays nothing extra.
      this._originTrack.at(t, this._tmp, this.axesKind ? this._tmpVel : undefined);
      this.object3D.position.set(this._tmp.x * scale, this._tmp.y * scale, this._tmp.z * scale);
    }
    if (this._attitudeTrack) {
      this._attitudeTrack.at(t, this._tmpQ);
      this.object3D.quaternion.copy(this._tmpQ);
    } else if (this.axesKind && this._originTrack) {
      axesForKind(this.axesKind, this._tmp, this._tmpVel, this._ax, this._ay, this._az);
      quaternionFromAxes(this._ax, this._ay, this._az, this._tmpQ);
      this.object3D.quaternion.copy(this._tmpQ);
    } else if (this._fixedRotationQ) {
      this.object3D.quaternion.copy(this._fixedRotationQ);
    } else if (quaternion) {
      this.object3D.quaternion.copy(quaternion);
    }
  }
}

/**
 * Topologically sort frame-definition-shaped objects (`{id, parentId, ...}`, e.g. the
 * wire-shaped entries `web/js/scene.js` builds from the scene JSON's additive `frames`
 * list, M4.1) so every parent precedes its children.
 *
 * `FrameGraph.addFrame()` legitimately accepts a definition whose `parentId` is not
 * registered *yet* -- its own docstring says so, since a stream can deliver
 * definitions in any order -- but it does so by silently parenting the node under the
 * graph root instead of deferring, which is the wrong tree for a *batch* list (the
 * whole `frames` array arrives at once) if that list happens to list a child before
 * its parent. Sorting with this function first removes the ambiguity: every
 * `addFrame()` call in the sorted order finds its parent (if any) already present.
 *
 * Throws (never silently drops or reparents to root) if a `parentId` references an id
 * that is not present in `defs` at all (a dangling reference -- mirrors
 * altavista/frames.py's `FrameParentMissingError`) or if the parent chain cycles
 * (mirrors `FrameCycleError`).
 *
 * @param {Array<{id: string, parentId?: string|null}>} defs
 * @returns {Array} a new array, same objects, parent-before-child order
 */
export function orderFrameDefsByParent(defs) {
  const byId = new Map(defs.map(d => [d.id, d]));
  for (const d of defs) {
    if (d.parentId && !byId.has(d.parentId)) {
      throw new Error(`frame '${d.id}' references parentId '${d.parentId}', which is not in the supplied frame list`);
    }
  }
  const ordered = [];
  const placed = new Set();
  const visiting = new Set();
  function place(d) {
    if (placed.has(d.id)) return;
    if (visiting.has(d.id)) throw new Error(`frame parent cycle detected at '${d.id}'`);
    visiting.add(d.id);
    if (d.parentId) place(byId.get(d.parentId));
    visiting.delete(d.id);
    placed.add(d.id);
    ordered.push(d);
  }
  for (const d of defs) place(d);
  return ordered;
}

export class FrameGraph {
  constructor() {
    /** @type {Map<string, FrameNode>} */
    this.nodes = new Map();
    this.root = new THREE.Group();
    this.root.name = 'frame-graph-root';
  }

  /**
   * Register one `FrameDefinition`-shaped object as a graph node, parented under
   * `def.parentId`'s node (or the graph root if `parentId` is unset/unknown --
   * unknown is legitimate: definitions can arrive from the frame service in any
   * order, before their declared parent). Returns the new node.
   */
  addFrame(def) {
    if (this.nodes.has(def.id)) throw new Error(`frame '${def.id}' already registered`);
    const node = new FrameNode(def);
    this.nodes.set(def.id, node);
    const parent = def.parentId ? this.nodes.get(def.parentId) : null;
    const parentObject3D = parent ? parent.object3D : this.root;
    parentObject3D.add(node.object3D);
    return node;
  }

  /** @returns {FrameNode} */
  frame(id) {
    const n = this.nodes.get(id);
    if (!n) throw new Error(`unknown frame '${id}'`);
    return n;
  }

  has(id) {
    return this.nodes.has(id);
  }

  /**
   * Re-parent `object3D` (the camera, an entity's marker/trail group, anything)
   * under frame `frameId`. This is the entire "switch frames" operation: Three's
   * `Object3D.attach` re-links the node into the new parent's subtree *and*
   * recomputes `object3D`'s local transform so its world position/orientation are
   * unchanged (nothing visually jumps), without touching `object3D.geometry`,
   * `object3D.material`, or any child -- no dispose, no re-create, no buffer
   * upload. Identity of every retained object (geometry, BufferAttribute, whatever
   * the caller parented) survives by construction, because this call never
   * constructs a replacement for any of them; it only edits `parent`/`children`
   * links and `position`/`quaternion`/`scale`. Proven in
   * tests/test_viewer_jitter.py::test_frame_switch_is_reparent_not_reload (invoked
   * over `node`, since it only needs Three's core Object3D graph, not a browser).
   */
  reparent(object3D, frameId) {
    const node = this.frame(frameId);
    node.object3D.attach(object3D);
    return node;
  }

  /** The frame id `object3D` is currently parented under (searching up through
   * intermediate non-frame groups, e.g. an entity's own local group), or `null` if
   * it is under the graph root / not attached to this graph at all. */
  frameOf(object3D) {
    let p = object3D.parent;
    while (p) {
      for (const node of this.nodes.values()) if (node.object3D === p) return node.id;
      p = p.parent;
    }
    return null;
  }

  /**
   * Update every node's local transform for epoch `t` (A1MJD); `scale` as in
   * `FrameNode.update`. Parent-relative composition into world transforms is
   * Three's normal `updateMatrixWorld`, run by the renderer on the next `render()`
   * call -- not duplicated here.
   */
  update(t, scale) {
    for (const node of this.nodes.values()) node.update(t, scale);
  }
}

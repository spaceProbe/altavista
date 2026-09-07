// M26.3: the multiple-3D-viewports data model (docs/ui-rework-plan.md's M26.3 milestone,
// docs/open-questions.md questions 161/162: "multiple 3D viewports sharing one scene and
// clock"). This module owns exactly what a viewport needs that must NOT be shared with
// any other viewport: its own camera, its own view-frame/focus state, its own floating
// origin, and its own copy of the precision-sensitive trajectory/footprint line geometry
// (see this module's own docstring on `Viewport.lines`/`Viewport.footprintLines` for why
// those -- and only those -- need a per-viewport copy). Everything else in the scene
// (bodies, spacecraft markers, event markers, the globe, stars/grid/axes) is a single
// shared Object3D, rendered by every viewport's camera unchanged -- see web/js/scene.js's
// module docstring for the "one scene, many cameras" split this module is one half of.
//
// **Why only trajectory/footprint LINES need a per-viewport copy, and nothing else does.**
// A `THREE.Object3D.position` (body mesh, spacecraft marker, event marker) is a plain JS
// `number` triple -- f64, never quantized through a `Float32Array` -- and Three's own
// `matrixWorld` composition (`Object3D.updateMatrixWorld`) stays in f64 all the way up to
// the final `camera.matrixWorldInverse * object.matrixWorld` product; only the *finished*
// modelview matrix is cast to float32 for the GPU uniform upload, by which point any large
// common absolute-position term shared by camera and object (both descend from the same
// frame-graph ancestor) has already cancelled algebraically in f64 arithmetic. A shared
// mesh's *rendered* precision therefore does not depend on which viewport's floating
// origin is currently in effect -- it would render identically precisely whether or not
// `web/js/scene.js`'s `_entitiesGroup` had ever been rebased at all.
//
// A trajectory line's `LineGeometry` vertex buffer is different: it is *data*, written
// once into a real `Float32Array` (`web/js/scene.js`'s `trajectoryRenderPositions()`),
// and that quantization happens BEFORE any camera/matrix math runs at all. An absolute
// (un-rebased) coordinate at LEO-orbit scale (~7 scene units) quantizes to ~0.8 m of
// error baked directly into the stored vertex -- independent of any camera. This is
// exactly what `web/js/origin.js`'s `FloatingOrigin` fixes, by subtracting a nearby f64
// origin *before* the float32 cast. Two viewports that need cm precision on two
// *different* parts of a trajectory (e.g. an RIC close-up of an RPO pair, vs. a
// whole-scenario ICRF view) cannot share one rebased vertex buffer -- rebasing it for one
// viewport's focus necessarily de-precises it (in absolute terms; still float32-exact
// relative to the OLD origin) for the other's. Hence: one `LineGeometry`/`Line2` per
// spacecraft *per viewport*, each independently origin-corrected via that viewport's own
// `FloatingOrigin` instance (never a shared one -- see docs/ui-rework-plan.md M26.3's own
// "a naive implementation that shares one origin across cameras ... will move that
// number" warning, and web/js/viewport_check.mjs's `measureRpoWithSharedOriginBug` for the
// concrete, measured proof of what sharing one does to the RPO figure).
//
// **THREE.Layers, not per-viewport THREE.Scene.** The scene graph genuinely is shared --
// "one scene" -- so a second `THREE.Scene` per viewport is not built here. Instead, each
// viewport's own line clones live in a `THREE.Group` (`renderGroup`) tagged with a
// dedicated, unique `THREE.Layers` bit (`allocateViewportLayer()`); that viewport's
// camera enables *both* the shared default layer (0 -- bodies/markers/globe/stars/etc,
// visible to every viewport) and its own bit, and no other viewport's bit. This is what
// keeps viewport B from also rendering viewport A's line clones (which would otherwise sit
// at very nearly, but not exactly, the same world position as B's own clone -- harmless
// visually at these render widths but a wasteful, confusing duplicate draw, and NOT the
// "per-viewport, independent" design this task calls for).
import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { FloatingOrigin } from './origin.js';

// Layer 0 is Three's default (every Object3D and every Camera starts enabled on it) --
// used here for objects every viewport must see (bodies, markers, globe, stars/grid/axes).
export const SHARED_LAYER = 0;
// web/js/scene.js's own single, legacy default camera (`Viewer.camera`, never wrapped in
// a `Viewport` instance -- see that file's module docstring for why) gets its own
// dedicated layer for its trajectory/footprint lines too, so it is symmetric with every
// other viewport and never double-renders against a later-added extra viewport. Layer 1
// is reserved for it; `allocateViewportLayer()` below starts handing out bits at 2.
export const PRIMARY_LAYER = 1;

let _layerCounter = 2;
/** Hand out the next free `THREE.Layers` bit (2..31 -- 0 is shared, 1 is the primary
 * viewport, see above) for a new `Viewport`. Three's `Layers` mask is a 32-bit int, so
 * this throws rather than silently wrapping once exhausted -- 30 simultaneous extra
 * viewports is far beyond any real layout this task builds, and wrapping would silently
 * alias two viewports onto the same bit (exactly the "shares one [layer] across cameras"
 * bug class this module exists to avoid for the floating origin). */
export function allocateViewportLayer() {
  if (_layerCounter > 31) {
    throw new Error('viewport.js: exhausted THREE.Layers bits (max 30 simultaneous extra viewports)');
  }
  return _layerCounter++;
}
/** Test-only: reset the module-global layer counter so independent headless test runs
 * (each importing this module once, per web/js/viewport_check.mjs's own process) get
 * deterministic, reproducible layer numbers instead of accumulating across test cases
 * that construct many `Viewport`s in one process. Never called from browser code. */
export function resetViewportLayerCounterForTests() { _layerCounter = 2; }

/**
 * One viewport: an independent camera, view-frame/focus state, floating origin, and
 * render group holding this viewport's own trajectory/footprint line clones. Constructed
 * standalone (no DOM/canvas/WebGL required -- see the constructor) so it is fully
 * testable under plain `node`; `attachCanvas()` is the one method that needs a real
 * browser (creates a `THREE.WebGLRenderer` and `OrbitControls`, both DOM-bound).
 *
 * Ownership split with web/js/scene.js's `Viewer` (the "one scene" half): `Viewer` owns
 * the shared `THREE.Scene`, frame graph, bodies/spacecraft/event data and their single
 * shared Object3Ds, and drives the one shared clock (`update(t)`, called once per render
 * frame). A `Viewport` owns everything listed in this class's fields below and nothing
 * else -- it holds no reference to the scenario data itself (`Viewer` passes in what a
 * clone needs: a spacecraft's already-built `poly`/color/`segCount`).
 */
export class Viewport {
  /** @param {string} id stable, caller-chosen identifier (e.g. 'icrf', 'ric', 'globe'). */
  constructor(id) {
    this.id = id;
    this.layer = allocateViewportLayer();
    this.camera = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
    this.camera.up.set(0, 0, 1);
    this.camera.layers.enable(this.layer); // + default layer 0, already enabled
    this.canvas = null;
    this.labelLayer = null;
    this.renderer = null;
    this.controls = null;
    // Which frame graph node (web/js/frames.js's FrameGraph, owned by the shared Viewer)
    // this viewport's camera is currently parented in -- set by Viewer.addViewport()/
    // setViewportFrame(), mirroring scene.js's own `_cameraFrameId` for the legacy single
    // camera. `null` until a Viewer actually attaches this viewport to its frame graph.
    this.cameraFrameId = null;
    this.focus = null; // spacecraft/body name, or null (= this viewport's frame's own origin)
    // One FloatingOrigin PER VIEWPORT (see module docstring) -- never shared with another
    // Viewport instance, and never the same object as web/js/scene.js's own
    // `Viewer.floatingOrigin` (the legacy primary viewport's origin).
    this.floatingOrigin = new FloatingOrigin({ globalEnabled: true });
    this.focusPrev = new THREE.Vector3();
    this.renderGroup = new THREE.Group();
    this.renderGroup.name = `viewport-render:${id}`;
    this.renderGroup.layers.set(this.layer);
    // name -> { line: Line2, segCount: number } -- this viewport's own trajectory line
    // clone per spacecraft, independently origin-corrected (web/js/scene.js's
    // _buildViewportLines()/_refreshViewportGeometry()).
    this.lines = new Map();
    // name -> { line: Line2 } -- same idea for sensor footprint rings.
    this.footprintLines = new Map();
    // name -> HTMLElement, only when labelLayer is set (see scene.js's
    // _buildViewportLabels()) -- this viewport's own label DOM clones, positioned every
    // tick from THIS viewport's own camera (never the primary's).
    this.labels = null;
    this._tmp = new THREE.Vector3();
    this._tmpAbs = new THREE.Vector3();
  }

  /**
   * Wire this viewport to a real canvas (browser only): creates its own
   * `THREE.WebGLRenderer` and `OrbitControls`, both bound to `canvas`. Idempotent-unsafe
   * by design (throws if already attached) -- a viewport's canvas is not meant to be
   * swapped live; remove and recreate the Viewport instead (Viewer.removeViewport() then
   * addViewport()).
   */
  attachCanvas(canvas, labelLayer, { minDistance = 1e-3, maxDistance = 1e8 } = {}) {
    if (this.renderer) throw new Error(`viewport '${this.id}': attachCanvas() called twice`);
    this.canvas = canvas;
    this.labelLayer = labelLayer || null;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, logarithmicDepthBuffer: true });
    this.renderer.setPixelRatio(Math.min((typeof window !== 'undefined' && window.devicePixelRatio) || 1, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.controls = new OrbitControls(this.camera, canvas);
    this.controls.enableDamping = true;
    this.controls.dampingFactor = 0.08;
    this.controls.minDistance = minDistance;
    this.controls.maxDistance = maxDistance;
    this.resize();
  }

  resize() {
    if (!this.canvas || !this.renderer) return;
    const w = this.canvas.clientWidth || 1, h = this.canvas.clientHeight || 1;
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
    for (const c of this.lines.values()) c.line.material.resolution.set(w, h);
    for (const c of this.footprintLines.values()) c.line.material.resolution.set(w, h);
  }

  /** Dispose this viewport's own GPU resources (its line clones' geometry/material and
   * its renderer, if any). Does NOT touch `renderGroup`'s scene-graph parent -- the
   * caller (Viewer.removeViewport()) is responsible for `renderGroup.removeFromParent()`
   * (this class knows nothing about the frame graph it was parented under). */
  dispose() {
    for (const c of this.lines.values()) { c.line.geometry.dispose(); c.line.material.dispose(); }
    for (const c of this.footprintLines.values()) { c.line.geometry.dispose(); c.line.material.dispose(); }
    this.lines.clear();
    this.footprintLines.clear();
    if (this.renderer) this.renderer.dispose();
  }
}

/**
 * Cast a ray from camera `camera` through NDC point `(ndcX, ndcY)` (each in [-1, 1], the
 * standard `THREE.Raycaster.setFromCamera` convention) and return the nearest hit among
 * `targets`, or `null`. Exported as a plain function (not a `Viewport`/`Viewer` method)
 * specifically so it is testable headlessly against synthetic `THREE.Object3D`s and
 * `THREE.Camera`s -- no renderer, no DOM, no Viewer needed (`THREE.Raycaster` is pure
 * CPU-side geometry math, same as everything else this module does under plain `node`).
 *
 * This is the entire "picking resolves against the correct viewport" mechanism
 * (docs/ui-rework-plan.md M26.3): pass the CLICKED viewport's own `camera` (not always
 * `viewer.camera`, the legacy primary -- that is exactly the wrong-implementation this
 * function's own test breaks against, see web/js/viewport_check.mjs) and the ray is cast
 * from that viewport's own eye/frame, so two viewports looking at the same screen
 * coordinate from different cameras correctly resolve to different world objects (or the
 * same one, when that is geometrically correct) without either needing to know about
 * `THREE.Layers` at all -- every pick target (body mesh, spacecraft marker, event mesh)
 * is a single object shared by every viewport (see module docstring), so no per-viewport
 * layer filtering is needed for picking to be correct, only the right camera.
 *
 * @param {THREE.Camera} camera
 * @param {number} ndcX
 * @param {number} ndcY
 * @param {THREE.Object3D[]} targets
 * @returns {THREE.Object3D|null}
 */
export function pickAlongCamera(camera, ndcX, ndcY, targets) {
  const raycaster = new THREE.Raycaster();
  raycaster.setFromCamera({ x: ndcX, y: ndcY }, camera);
  const hits = raycaster.intersectObjects(targets, false);
  return hits.length ? hits[0].object : null;
}

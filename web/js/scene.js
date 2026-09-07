// Three.js scene: bodies, spacecraft, trajectories, labels, lighting, camera.
//
// M3.1: the scene graph *is* the frame graph (web/js/frames.js), and every position
// this file writes into a GPU-visible buffer goes through the configurable floating
// origin (web/js/origin.js) instead of being stored as an absolute float32 coordinate.
// M4.1 consumes the scene JSON's additive `frames` list (altavista/model.py's
// ScenarioData.to_dict(), docs/open-questions.md question 78) to build a real
// multi-node frame graph instead of one hardcoded node. See web/VIEWER.md for the
// full writeup; the short version:
//   - `this.frameGraph` holds one Group per `FrameDefinition` in `sc.frames`,
//     topologically ordered by `orderFrameDefsByParent` (frames.js) before being
//     added so a batch list is never misparented under root by arrival order. Bodies,
//     spacecraft, trajectories and events are always parented under the *entities*
//     frame (`this._originFrameId`, the scenario's own declared frame, `sc.frame.name`
//     -- see `_buildFrameGraph`); the camera can be reparented to *any* frame node in
//     the graph via `setViewFrame` (e.g. an entity-relative RIC frame, for close-up
//     RPO viewing), independent of where the entities themselves live. Nothing is
//     ever added to `this.scene` directly.
//   - `this.floatingOrigin` (one `FloatingOrigin`, keyed by frame id) rebases the
//     *entities* frame's origin to the current focus target's absolute position
//     whenever drift exceeds `ORIGIN_REBASE_DRIFT` scene units (see
//     `_rebaseOriginTo`/`_maybeRebaseOrigin`), regardless of which frame the camera is
//     parented in -- this is what keeps trajectory vertex buffers precise even while
//     viewing from a different frame. origin.js documents the mechanism but
//     deliberately leaves the rebase *policy* to the caller; this is that policy.
//   - A non-entities frame the camera is parented in (e.g. RIC) needs no floating
//     origin of its own: its Group.position is set every tick from a sampled origin
//     track (`FrameNode.setOriginTrack`/`update`, frames.js) as a plain f64
//     `THREE.Vector3` -- never rounded through a `Float32Array` -- so the "large"
//     component of the camera's world position lives there exactly, and the camera's
//     own *local* position (small, RPO-scale) is what actually needs precision. See
//     `setViewFrame`'s docstring for the full argument.
//   - Trajectory line geometry (`LineGeometry.setPositions`) is built from the f64
//     polyline `interp.js`'s `TrajectoryInterp.polyline()` produces, origin-subtracted
//     in f64 and cast to f32 exactly once via `FloatingOrigin.toRenderSpaceArray` --
//     never written as an absolute float32 coordinate.
import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { Line2 } from 'three/addons/lines/Line2.js';
import { LineGeometry } from 'three/addons/lines/LineGeometry.js';
import { LineMaterial } from 'three/addons/lines/LineMaterial.js';
import { TrajectoryInterp, BodyInterp, findSegment } from './interp.js';
import { FrameGraph, orderFrameDefsByParent } from './frames.js';
import { FloatingOrigin } from './origin.js';
import { GlobeLayer } from './globe.js';
import { TilesOverlayLayer, placeOverlayInBodyFixedFrame } from './tiles_layer.js';
// M26.3 (docs/ui-rework-plan.md): the multiple-3D-viewports data model. See viewport.js's
// own module docstring for the full design (per-viewport camera/frame/focus/floating
// origin/line-clones, THREE.Layers instead of a second THREE.Scene) -- this file is the
// "one scene" half: it owns the shared scene graph/frame graph/bodies/spacecraft/clock,
// and this import is what lets it also drive N independent cameras against that one
// scene. `PRIMARY_LAYER` tags the legacy default camera's (`this.camera`, below) own
// trajectory/footprint lines so it is symmetric with every viewport `addViewport()`
// creates later and never double-renders against one.
import { Viewport, PRIMARY_LAYER, pickAlongCamera } from './viewport.js';

export const SCALE = 1e-3;          // scene units per km (1 unit = 1000 km)
// Camera distance `_fitOrigin()` ("Reset view" in the entities frame) places the
// camera at, as a multiple of `fitRadius` -- exported (M20.2, question 134) so
// tests/test_cdm_run.py's headless harness can assert "camera distance after reset"
// against this exact, real constant rather than a value re-typed independently in
// Python (which could silently drift from scene.js's own number).
export const FIT_DISTANCE_FACTOR = 2.4;
const MARKER_PX = 7;                // spacecraft marker diameter on screen
const EVENT_PX = 9;
// Auto-rebase policy (origin.js leaves this to the caller): rebase the floating
// origin whenever the focus target drifts more than this many scene units from the
// current origin. At float32's ~7.2 decimal digits, a coordinate of magnitude V
// (scene units, 1 unit = 1e6 m) quantizes to roughly V * 1.19e-7 * 1e6 m = V * 0.119 m.
// 0.02 scene units (20 km) of tolerated drift therefore bounds the *worst case right
// before a rebase* at ~2.4 mm -- comfortably under both the sub-metre (general scene)
// and centimetre (RIC/RPO) bounds from docs/open-questions.md Q46, with margin.
const ORIGIN_REBASE_DRIFT = 0.02;
const ORIGIN_REBASE_DRIFT_SQ = ORIGIN_REBASE_DRIFT * ORIGIN_REBASE_DRIFT;
// OrbitControls zoom clamp for the entities frame (whole-scenario, orbital scale).
// setViewFrame() derives a much smaller pair for an RPO-scale frame (e.g. RIC) so a
// user can actually zoom in there -- these are what fit()/setFocus() restore when
// switching back.
const ENTITIES_MIN_DISTANCE = 1e-3;
const ENTITIES_MAX_DISTANCE = 1e8;
// M5.2: wire AxesKind enum name (proto/altavista/v1/core.proto, protobuf3 JSON
// mapping) -> frames.js's lowercase axesKind, for the three kinds a frame node
// rotates itself for (RIC/VNB/VVLH). Every other axes kind (MJ2000Eq, BodyFixed, ...)
// maps to `undefined` -- no client-side rotation for those, unchanged from M4.1.
const AXES_KIND_MAP = { AXES_KIND_RIC: 'ric', AXES_KIND_VNB: 'vnb', AXES_KIND_VVLH: 'vvlh' };

export class Viewer {
  constructor(canvas, labelLayer) {
    this.canvas = canvas;
    this.labelLayer = labelLayer;
    this.renderer = new THREE.WebGLRenderer({ canvas, antialias: true, logarithmicDepthBuffer: true });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.scene = new THREE.Scene();
    this.scene.background = new THREE.Color(0x000000);

    // Frame graph + floating origin (see the module docstring above). `_originFrameId`
    // ('root' until a scenario is loaded) is the *entities* frame -- bodies,
    // spacecraft, events. `_cameraFrameId` is whichever frame the camera is currently
    // parented in; it starts equal to `_originFrameId` and only diverges via
    // `setViewFrame`. `clear()` recreates `this.frameGraph` (and `_buildFrameGraph`
    // re-registers every node) on every `setScenario()` call, the same way
    // `this.bodies`/`this.spacecraft` are rebuilt, so a previous scenario's frames
    // never linger.
    this._originFrameId = 'root';
    this._cameraFrameId = 'root';
    this.frameGraph = new FrameGraph();
    this.frameGraph.addFrame({ id: this._originFrameId });
    this.scene.add(this.frameGraph.root);
    // Rendered entities (bodies, spacecraft, events, and by default the camera) are
    // parented under this intermediate Group, a child of the entities *frame node*
    // itself -- never directly under the frame node. This is what keeps a true
    // FrameDefinition node's own transform an honest, always-zero anchor (safe as an
    // entity-relative child frame's parent, e.g. RIC -- see `setViewFrame`'s
    // docstring) while still letting `_rebaseOriginTo` move *something* to absorb the
    // floating-origin shift. `FrameGraph.frameOf()` already documents and supports
    // exactly this "intermediate non-frame groups" pattern. Recreated fresh in
    // `_buildFrameGraph` on every `setScenario()` call, same as `this.frameGraph`.
    this._entitiesGroup = new THREE.Group();
    this._entitiesGroup.name = 'entities-render-group';
    this.frameGraph.frame(this._originFrameId).object3D.add(this._entitiesGroup);
    this.floatingOrigin = new FloatingOrigin({ globalEnabled: true });
    this.floatingOrigin.setOrigin(this._originFrameId, 0, 0, 0);

    this.camera = new THREE.PerspectiveCamera(45, 1, 1e-3, 1e9);
    this.camera.up.set(0, 0, 1);
    this.camera.position.set(30, -30, 20);
    // M26.3: the legacy default camera's own dedicated layer (see viewport.js's module
    // docstring) -- default layer 0 stays enabled too (shared bodies/markers/globe/
    // stars), this is additive.
    this.camera.layers.enable(PRIMARY_LAYER);
    this._entitiesGroup.attach(this.camera);
    this.controls = new OrbitControls(this.camera, canvas);
    this.controls.enableDamping = true;
    this.controls.dampingFactor = 0.08;
    this.controls.minDistance = ENTITIES_MIN_DISTANCE;
    this.controls.maxDistance = ENTITIES_MAX_DISTANCE;
    // M26.3: extra viewports beyond this legacy default camera (docs/ui-rework-plan.md's
    // M26.3, "one scene and one clock, many cameras") -- id -> Viewport (web/js/
    // viewport.js). Empty until addViewport() is called; the legacy this.camera/
    // this.controls/this.floatingOrigin above are NOT wrapped in a Viewport instance
    // (kept exactly as every pre-M26.3 method already expects, zero behaviour change for
    // single-viewport use) -- see viewport.js's module docstring and this file's own
    // addViewport()/setViewportFrame() for how an extra viewport's independent state is
    // kept separate from these.
    this.viewports = new Map();

    this.ambient = new THREE.AmbientLight(0xffffff, 0.12);
    this.sun = new THREE.DirectionalLight(0xffffff, 2.6);
    this.sun.position.set(1, 0.3, 0.2);
    this.scene.add(this.ambient, this.sun, this.sun.target);

    this.stars = makeStars();
    this.scene.add(this.stars);
    this.axes = new THREE.AxesHelper(1);
    this.axes.visible = false;
    this.scene.add(this.axes);
    this.grid = new THREE.GridHelper(1, 20, 0x2a3446, 0x1a2130);
    this.grid.rotation.x = Math.PI / 2;
    this.grid.visible = false;
    this.scene.add(this.grid);

    this.textureLoader = new THREE.TextureLoader();
    this.scenario = null;
    this.bodies = new Map();       // name -> {mesh, interp, data, label}
    this.spacecraft = new Map();   // name -> {marker, line, interp, data, label, poly}
    this.events = [];              // {mesh, label, data, interp}
    this.footprints = new Map();   // name -> {line, data, lastIndex} -- M6.3, see setScenario()
    // M15.4: the tiled globe (a GlobeLayer bound to one body, see enableGlobe()) and
    // the 3D Tiles overlay (a TilesOverlayLayer, see loadTilesOverlay()) -- both null
    // until explicitly requested (app.js's "Tiled globe" checkbox / "Load 3D Tiles
    // fixture" button), and both dropped by clear() like every other per-scenario
    // render state (see clear() below).
    this.globeLayer = null;
    this.globeBodyName = null;
    this.tilesOverlay = null;
    this.options = { labels: true, trail: 'full', stars: true };
    this.focus = null;             // object name or null (= frame origin)
    this._focusPrev = new THREE.Vector3();
    this._tmp = new THREE.Vector3();
    this._tmpAbs = new THREE.Vector3();  // scratch: absolute (origin-independent) position
    this._tmpQ = new THREE.Quaternion();
    this._sunDir = new THREE.Vector3(1, 0.3, 0.2);
    this.fitRadius = 30;
    this.resize();
  }

  // ------------------------------------------------------------------ setup
  resize() {
    const w = this.canvas.clientWidth || 1, h = this.canvas.clientHeight || 1;
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
    for (const s of this.spacecraft.values()) s.line.material.resolution.set(w, h);
    for (const f of this.footprints.values()) f.line.material.resolution.set(w, h);
    // M26.3: every extra viewport owns its own canvas/renderer/line materials -- resize
    // them too (Viewport.resize() is a no-op if that viewport has no canvas attached).
    for (const vp of this.viewports.values()) vp.resize();
  }

  /**
   * M26.3 fix: the canvas height used for "constant N-pixel apparent size" math (marker/
   * event scale, body sub-pixel dot threshold, globe LOD screen-space error) --
   * previously always `this.canvas.clientHeight || 1`, i.e. the LEGACY primary
   * canvas. That is wrong the instant the primary's own pane is not part of the
   * CURRENT layout at all (the RPO default layout, web/js/layout/default_layouts.js's
   * buildRpoTripleViewportLayout(), has no 'viewport' leaf) -- `this.canvas` is then
   * detached from the visible DOM, `clientHeight` reads 0, and the `|| 1` fallback
   * silently substitutes a 1-pixel-tall canvas, inflating every "constant pixel size"
   * computation by two to three orders of magnitude (found live: a spacecraft marker
   * meant to read as ~7 px ended up ~139 SCENE UNITS in radius -- large enough for a
   * viewport's own camera to end up *inside* the marker mesh, which made picking
   * against it silently fail every time, since THREE.Mesh.raycast excludes the
   * material's back faces by default and a camera inside a sphere only ever sees its
   * inside/back faces -- see web/js/REPORT_M26_3.md's manual-browser-verification
   * section for the full account of how this was found and confirmed).
   *
   * Falls back, in order: this.canvas itself (still correct and cheapest when the
   * legacy primary pane IS part of the current layout); the first extra viewport whose
   * OWN canvas is currently attached and sized; a fixed, reasonable default (matches
   * this method's own docstring reasoning -- never 1).
   */
  /**
   * M26.3 fix (found by the same manual-browser verification as `_referenceCanvasHeight`
   * above): the distance a marker's "constant N-pixel apparent size" scale is computed
   * from used to be unconditionally `marker.position.distanceTo(this.camera.position)`
   * -- the LEGACY primary camera's own distance, however irrelevant that is to any other
   * viewport. For the RPO default layout specifically this is not merely cosmetic: the
   * "Target"/"Chaser" markers' scale, sized for the whole-scenario ICRF distance
   * (tens of scene units), ends up hundreds of kilometres in radius -- and the RIC
   * viewport's own camera sits only tens of metres from that same marker (an RPO
   * close-up, by design). The RIC camera therefore ends up INSIDE the oversized marker
   * sphere; `THREE.Mesh.raycast()` (web/vendor/three/three.core.js) tests only
   * front-facing triangles by default, and a camera inside a sphere only ever sees its
   * own back faces -- so `viewer.pick()` silently found nothing in the RIC pane no
   * matter where the user double-clicked, even dead-centre on "Target" itself. Found
   * live: projecting Target's own world position back through the RIC camera and
   * picking at that exact NDC point still returned `null`, traced to
   * `marker.scale.x` being ~0.5 SCENE UNITS (~520 km) against a camera only ~5.8e-5
   * units (~58 m) away.
   *
   * Fix: use the SMALLEST distance from this marker's world position to ANY currently
   * active camera (the legacy primary's, and every registered viewport's own) --
   * `getWorldPosition()` on both sides, since a viewport's camera may be parented in a
   * completely different frame (e.g. RIC) than the marker (always under the entities
   * frame), so a plain `.position`-to-`.position` comparison (valid only within one
   * shared parent) is not enough. This does not give every viewport a "correct" N-pixel
   * marker size simultaneously -- a single shared Object3D fundamentally cannot, when
   * viewports differ in scale by orders of magnitude (see viewport.js's own module
   * docstring on why only trajectory LINES got a per-viewport copy, not markers) -- but
   * it guarantees the marker is never larger than a small fraction of the CLOSEST
   * camera's own distance to it, so no camera can end up trapped inside it. A farther
   * viewport then sees a smaller-than-ideal (possibly sub-pixel) marker instead of a
   * broken one -- a strictly safer failure mode.
   */
  _markerReferenceDistance(markerWorldPos) {
    let min = markerWorldPos.distanceTo(this.camera.getWorldPosition(this._tmpCamWorld || (this._tmpCamWorld = new THREE.Vector3())));
    for (const vp of this.viewports.values()) {
      const d = markerWorldPos.distanceTo(vp.camera.getWorldPosition(this._tmpCamWorld));
      if (d < min) min = d;
    }
    return min;
  }

  _referenceCanvasHeight() {
    if (this.canvas.clientHeight) return this.canvas.clientHeight;
    for (const vp of this.viewports.values()) {
      if (vp.canvas && vp.canvas.clientHeight) return vp.canvas.clientHeight;
    }
    return 600;
  }

  clear() {
    for (const b of this.bodies.values()) { disposeMesh(b.mesh); disposeMesh(b.dot); b.label.remove(); }
    for (const s of this.spacecraft.values()) { s.line.geometry.dispose(); s.line.material.dispose(); s.label.remove(); }
    for (const e of this.events) { disposeMesh(e.mesh); e.label.remove(); }
    for (const f of this.footprints.values()) { f.line.geometry.dispose(); f.line.material.dispose(); }
    this.bodies.clear(); this.spacecraft.clear(); this.events = []; this.footprints.clear();
    // M26.3: every extra viewport's own trajectory/footprint line CLONES (viewport.js's
    // module docstring -- why they exist) are just as scenario-specific as the primary's
    // own `s.line`/`f.line` above; dispose their GPU resources and drop the Maps the same
    // way, but keep the Viewport instance itself (camera/controls/renderer/floatingOrigin
    // persist across a scenario reload -- only what depends on the OLD scenario's
    // spacecraft/footprints is torn down here). setScenario() rebuilds fresh clones after
    // _buildFrameGraph() below, the same way it rebuilds this.spacecraft/this.footprints.
    for (const vp of this.viewports.values()) {
      for (const c of vp.lines.values()) { c.line.geometry.dispose(); c.line.material.dispose(); }
      for (const c of vp.footprintLines.values()) { c.line.geometry.dispose(); c.line.material.dispose(); }
      vp.lines.clear();
      vp.footprintLines.clear();
      if (vp.labels) {
        for (const el of vp.labels.bodies.values()) el.remove();
        for (const el of vp.labels.spacecraft.values()) el.remove();
        for (const el of vp.labels.events) el.remove();
        vp.labels = null;
      }
    }
    // M15.4: drop the globe/3D-Tiles-overlay per scenario, same as every other
    // rendered object above -- the body they were attached to no longer exists once
    // this.bodies is cleared, and re-enabling either after a new scenario loads is a
    // deliberate user action (app.js's checkbox/button), not something to silently
    // carry over.
    // globeLayer.group (a sibling of _entitiesGroup) and tilesOverlay.group (M16.4: a
    // child of a body-fixed frame node, see _bodyFixedFrame) were both parented
    // somewhere under the *old* this.frameGraph.root, which is itself discarded a few
    // lines down when it is removed from this.scene -- no separate scene.remove()
    // call is needed for either group, only disposal of their GPU resources
    // (geometry/texture/material).
    if (this.globeLayer) { this.globeLayer.dispose(); this.globeLayer = null; this.globeBodyName = null; }
    if (this.tilesOverlay) { this.tilesOverlay.dispose(); this.tilesOverlay = null; this._tilesOverlayBodyName = null; }
    // Fresh, EMPTY frame graph per scenario -- drops the previous scenario's frames
    // (and every body/spacecraft/event object parented under them, whose GPU
    // resources were just disposed above) in one step, the same way
    // `this.bodies`/`this.spacecraft` are rebuilt from scratch rather than patched.
    // `_buildFrameGraph` (called right after, from `setScenario`) populates it and
    // re-parents the camera; left empty here so `clear()` stays a pure teardown step.
    this.scene.remove(this.frameGraph.root);
    this.frameGraph = new FrameGraph();
    this.scene.add(this.frameGraph.root);
  }

  setScenario(sc) {
    this.clear();
    this.scenario = sc;
    this._buildFrameGraph(sc);
    const rootObj = this._entitiesGroup;
    for (const b of sc.bodies) {
      const mesh = makeBodyMesh(b, this.textureLoader);
      // a screen-space dot so distant bodies stay visible when their disc is sub-pixel
      const dot = new THREE.Mesh(new THREE.SphereGeometry(1, 10, 8),
        new THREE.MeshBasicMaterial({ color: new THREE.Color(b.color || '#888888') }));
      dot.visible = false;
      rootObj.add(mesh, dot);
      const label = this._label(b.name, 'body');
      this.bodies.set(b.name, { mesh, dot, interp: new BodyInterp(b), data: b, label, visible: true });
    }
    const w = this.canvas.clientWidth || 1, h = this.canvas.clientHeight || 1;
    for (const s of sc.spacecraft) {
      const interp = new TrajectoryInterp(s);
      // `poly.points` is f64 (km, absolute) -- see interp.js's polyline() docstring.
      // trajectoryRenderPositions() (below) does the scale + origin-subtract + single
      // Math.fround; it's the exact function _refreshOriginRelativeGeometry() calls on
      // every rebase and web/js/scene_jitter_harness.mjs drives headlessly (see
      // tests/test_viewer_jitter.py) -- not reimplemented in either place.
      const poly = interp.polyline();
      const geometry = new LineGeometry();
      geometry.setPositions(trajectoryRenderPositions(poly, this.floatingOrigin, this._originFrameId));
      const material = new LineMaterial({ color: new THREE.Color(s.color || '#ffffff'), linewidth: 1.6, transparent: true, opacity: 0.9 });
      material.resolution.set(w, h);
      const line = new Line2(geometry, material);
      line.computeLineDistances();
      line.layers.set(PRIMARY_LAYER); // M26.3: see viewport.js's module docstring
      const marker = new THREE.Mesh(new THREE.SphereGeometry(1, 12, 10),
        new THREE.MeshBasicMaterial({ color: new THREE.Color(s.color || '#ffffff') }));
      rootObj.add(line, marker);
      const label = this._label(s.label || s.name, 'sc');
      label.style.color = s.color || '#fff';
      this.spacecraft.set(s.name, { marker, line, interp, data: s, label, poly, visible: true, segCount: poly.times.length - 1 });
    }
    // M21.2 (question 140): `spacecraftNames` is exactly `this.spacecraft`'s own key set
    // -- computed once here rather than per-event -- fed to the shared, headlessly
    // testable `eventHasRenderedInstance()` (this module, see its own docstring for why
    // "present in this list" is the real has-a-position-class fact, not a name/id
    // heuristic) so an event whose instance was never rendered (no position class, or
    // -- M21.3 -- no trajectory at all) never gets a 3D mesh/label, only ever a timeline
    // entry (`web/js/app.js`'s `buildLists`/`buildTicks`, untouched by this).
    const spacecraftNames = [...this.spacecraft.keys()];
    for (const ev of sc.events || []) {
      const hasRenderedInstance = eventHasRenderedInstance(ev, spacecraftNames);
      const sc0 = hasRenderedInstance ? this.spacecraft.get(ev.spacecraft) : null;
      const mesh = new THREE.Mesh(new THREE.OctahedronGeometry(1), new THREE.MeshBasicMaterial({ color: 0xffd166 }));
      if (sc0) { sc0.interp.at(ev.t, this._tmpAbs).multiplyScalar(SCALE); this._toLocal(this._tmpAbs, mesh.position); }
      else mesh.position.set(0, 0, 0);
      mesh.visible = hasRenderedInstance;
      rootObj.add(mesh);
      const label = this._label(ev.name, 'event');
      this.events.push({ mesh, label, data: ev, interp: sc0 ? sc0.interp : null });
    }
    // Sensor footprints (M6.3, altavista/scenario.py's Scenario.footprint()): a closed
    // ring per declared footprint, updated to the *nearest* recorded sample each tick
    // in update() below -- unlike trajectories/bodies there is no interpolation
    // contract between ring samples (see altavista/model.py's Footprint docstring), so
    // this is deliberately a discrete "jump to nearest sample" rather than a smoothed
    // curve. Geometry starts as a placeholder pair of points; update() fills it in on
    // the very first tick regardless of `t` (lastIndex starts at -1, which never
    // equals a real findSegment() result).
    for (const fp of sc.footprints || []) {
      if (!fp.t || !fp.t.length) continue;
      const material = new LineMaterial({ color: new THREE.Color(fp.color || '#00e5ff'), linewidth: 1.2, transparent: true, opacity: 0.85 });
      material.resolution.set(w, h);
      const geometry = new LineGeometry();
      geometry.setPositions(new Float32Array([0, 0, 0, 0, 0, 0]));
      const line = new Line2(geometry, material);
      line.computeLineDistances();
      line.visible = false;
      line.layers.set(PRIMARY_LAYER); // M26.3: see viewport.js's module docstring
      rootObj.add(line);
      this.footprints.set(fp.name, { line, data: fp, lastIndex: -1 });
    }
    // M20.2 (question 134): the whole-scenario framing radius, computed by the same
    // exported, headlessly-testable function on both the Python-scenario and the
    // CDM-ingest path (they share this one JS consumer either way) -- see
    // computeFitRadius()'s own docstring below for the central-body/trajectory-extent
    // formula and tests/test_viewer_globe.py for the ingested-run regression test.
    this.fitRadius = computeFitRadius(sc.bodies, sc.spacecraft);
    this.axes.scale.setScalar(this.fitRadius * 0.6);
    this.grid.scale.setScalar(this.fitRadius * 2.5 / 1);
    this.focus = null;
    this.fit();
    // M26.3: every extra viewport's own line clones/labels were dropped in clear() above
    // (the old scenario's spacecraft/footprints they were built from no longer exist) --
    // rebuild them fresh against the NEW this.spacecraft/this.footprints, re-parent each
    // viewport's renderGroup under the freshly-rebuilt entities frame node (_buildFrameGraph
    // above discarded the old one), and reset each viewport to the entities frame with no
    // focus -- the same reset every pre-existing single-viewport reload already gives the
    // primary camera two lines up (this.focus = null; this.fit()). A caller that wants a
    // viewport to keep playing a specific role across a reload (e.g. app.js's "RIC" pane)
    // re-applies setViewportFrame() after setScenario() returns, exactly like app.js's
    // existing frame-select dropdown already needs re-picking after every load.
    for (const vp of this.viewports.values()) {
      this.frameGraph.frame(this._originFrameId).object3D.add(vp.renderGroup);
      // The old frame graph (and every node in it, including whichever one vp.camera was
      // previously parented in, e.g. a now-gone RIC frame) was just discarded by
      // _buildFrameGraph() above -- re-parent explicitly rather than relying on wherever
      // the camera happened to be left attached.
      vp.renderGroup.add(vp.camera);
      this._buildViewportLines(vp);
      this._buildViewportLabels(vp);
      vp.cameraFrameId = this._originFrameId;
      vp.focus = null;
      this._fitViewportOrigin(vp);
    }
  }

  /**
   * Build `this.frameGraph` from the scenario's additive `sc.frames` list (M4.1,
   * altavista/model.py's `ScenarioData.to_dict()`, docs/open-questions.md question 78):
   * one `FrameGraph` node per `FrameDefinition`, normalized from the protobuf-JSON
   * wire shape (`parentFrameId` -> `parentId`, `originTrack` passed through) and
   * topologically ordered (`orderFrameDefsByParent`) so a batch list is never
   * misparented under root by arrival order (see that function's docstring).
   *
   * `this._originFrameId` -- the frame bodies/spacecraft/events are parented under --
   * is always `sc.frame.name`: the scenario's own declared frame, present in the wire
   * JSON both as the (unchanged, pre-M4.1) top-level `frame` field and, when its axes
   * are CDM-mappable, as an entry in `frames` (altavista/scenario.py's `_build_frames`).
   * When it is *not* CDM-mappable (e.g. `"BodyInertial"`, GMAT's Topocentric -- no
   * `altavista.v1.AxesKind` covers them, see `altavista/cdm.py`'s `frame_definition_for`
   * docstring), `frames` simply omits it; this method then synthesizes a bare root
   * node with that id -- logged with `console.warn`, not a silent fallback -- so the
   * scene still renders exactly as it did before M4.1's `frames` list existed.
   *
   * M5.2 additions (rotating relative frames): a `sc.frames` entry whose `axes` is
   * `AXES_KIND_RIC`/`_VNB`/`_VVLH` gets `axesKind` set (`AXES_KIND_MAP` above), so its
   * `FrameNode` computes and applies its own quaternion every tick from the entity's
   * interpolated state (see `frames.js`'s `axesForKind`/`FrameNode.update`) -- a
   * camera parented there (`setViewFrame`) rotates with it via Three's ordinary
   * parent-child composition, nothing extra needed here.
   *
   * M19.2 addition (`docs/open-questions.md` question 129, E-24): a `sc.frames` entry
   * carrying a non-empty `fixedRotationQ` (protobuf-JSON camelCase of
   * `FrameDefinition.fixed_rotation_q`, field 13 -- a body-centred inertial frame's
   * constant rotation relative to its own body's MJ2000Eq, e.g. the ICRF frame bias)
   * is passed straight through to the node's `def`; `frames.js`'s `FrameNode`
   * constructor turns it into a `THREE.Quaternion` (`fixedRotationQuaternion`,
   * validating the wire's own contract -- exactly 0 or 4 entries, unit if 4) and
   * `update()` applies it whenever the node has no real attitude stream and no
   * `axesKind`-derived rotation, closing the defect this frame graph shipped with
   * since M4.1: an inertial frame (ICRF, MJ2000Ec) with no origin track used to keep
   * identity orientation forever, indistinguishable from its own body's MJ2000Eq.
   * This method also now appends
   * one synthetic **body-frame** node per spacecraft (`${name}_body`, parented under
   * the entities frame): a real attitude quaternion stream when
   * `sc.spacecraft[].attitude` supplies one (`altavista/model.py`'s additive
   * `Trajectory.attitude`), else a nadir-pointing VVLH fallback derived from that same
   * spacecraft's own track -- groundwork for sensor footprints (per-entity, so a
   * future footprint cone/frustum has a body frame to parent under). The fallback is
   * never silent: `fallback: true` on the node's `def` and a `" (fallback: ..."` verb
   * baked into `description` are both set here, and `frameList()`/`app.js` surface it
   * in the "Frames" UI.
   */
  _buildFrameGraph(sc) {
    this._originFrameId = sc.frame.name;
    const defs = (sc.frames || []).map(fd => ({
      id: fd.id,
      parentId: fd.parentFrameId || null,
      originTrack: fd.originTrack || null,
      description: fd.description || fd.id,
      axesKind: AXES_KIND_MAP[fd.axes] || null,
      fixedRotationQ: fd.fixedRotationQ || null,
      // M20.2 (question 134/E-27): the wire FrameDefinition's own origin body (e.g.
      // "Earth" for EarthICRF/EarthBodyFixed/EarthMJ2000Eq -- M18.1's mandatory
      // central-body frame set, always offered for a CDM-ingested run), when it names
      // one -- null for an entity-relative RIC/VNB/VVLH frame or the synthesized root
      // fallback below. Additive: consumed only by setViewFrame()'s
      // defaultFrameViewRadius() so a focus-less view of a body-axes frame can be
      // scaled to that body's own radius instead of the fixed RPO-scale default meant
      // for a spacecraft-relative frame.
      body: fd.body || null,
    }));
    if (!defs.some(d => d.id === this._originFrameId)) {
      console.warn(
        `altavista: scenario frame '${this._originFrameId}' has no matching entry in ` +
        `'frames' (its axes likely have no altavista.v1.AxesKind -- see ` +
        `altavista/cdm.py's frame_definition_for); synthesizing a root frame node so ` +
        `the scene still renders.`
      );
      defs.unshift({ id: this._originFrameId, parentId: null, originTrack: null, description: this._originFrameId, axesKind: null });
    }
    for (const d of orderFrameDefsByParent(defs)) {
      const node = this.frameGraph.addFrame(d);
      if (d.originTrack && d.originTrack.t && d.originTrack.t.length) node.setOriginTrack(d.originTrack);
    }
    // Per-entity body frames (M5.2 groundwork for sensor footprints) -- see this
    // method's docstring. Added *after* the sc.frames-derived nodes above (all of
    // which are guaranteed parented by now, entities frame included) so `parentId:
    // this._originFrameId` always resolves. Not part of `sc.frames` at all (no
    // FrameRegistry/GMAT realization backs these yet -- AXES_KIND_PLATFORM_BODY
    // requires an `attitude_source` the frame service doesn't implement until P2, per
    // proto/altavista/v1/core.proto's own comment on that field), so this is real,
    // labelled viewer-side groundwork, not a claim that the frame service validated
    // these.
    for (const s of sc.spacecraft || []) {
      if (!s.t || !s.t.length) continue;
      const hasAttitude = Array.isArray(s.attitude) && s.attitude.length === s.t.length * 4;
      const label = s.label || s.name;
      const node = this.frameGraph.addFrame({
        id: `${s.name}_body`,
        parentId: this._originFrameId,
        axesKind: hasAttitude ? null : 'vvlh',
        description: hasAttitude
          ? `${label} body (attitude stream)`
          : `${label} body (fallback: nadir VVLH -- no attitude stream)`,
        fallback: !hasAttitude,
      });
      node.setOriginTrack({ t: s.t, pos: s.pos, vel: s.vel });
      if (hasAttitude) node.setAttitudeTrack({ t: s.t, quat: s.attitude });
    }
    // Fresh render group for this scenario's entities frame -- see the constructor's
    // comment on `_entitiesGroup` for why entities (and the camera, by default) are
    // parented here rather than directly under the frame node.
    this._entitiesGroup = new THREE.Group();
    this._entitiesGroup.name = 'entities-render-group';
    this.frameGraph.frame(this._originFrameId).object3D.add(this._entitiesGroup);
    this._cameraFrameId = this._originFrameId;
    this._entitiesGroup.attach(this.camera);
    this.floatingOrigin.setOrigin(this._originFrameId, 0, 0, 0);
  }

  /**
   * List of `{id, description, fallback}` for every frame node currently in the
   * graph, in insertion order (`sc.frames`-derived nodes, parent-before-child, then
   * M5.2's per-entity body frames) -- for the UI's frame/origin selectors
   * (`web/js/app.js`). `fallback` (M5.2) is true for a body frame that had no
   * attitude stream to consume and fell back to nadir-pointing VVLH -- already baked
   * into `description` too (never a silently-unlabelled substitution), exposed
   * separately so the UI can style it distinctly if it wants to.
   */
  /**
   * The frame id the camera is CURRENTLY parented in (M20.2, question 135): whatever
   * `setViewFrame()`/`fit()` last set `_cameraFrameId` to -- the entities frame's own
   * id until a frame switch, unlike `this.scenario.frame.name`, which is always the
   * scenario's *base* declared frame and never changes after `setScenario()`.
   * `app.js`'s HUD reads this (plus `this.focus`) so it reflects the actual current
   * view, not just the scenario's own static frame field.
   */
  get viewFrameId() {
    return this._cameraFrameId;
  }

  frameList() {
    const out = [];
    for (const node of this.frameGraph.nodes.values()) {
      out.push({ id: node.id, description: node.def.description || node.id, fallback: !!node.def.fallback });
    }
    return out;
  }

  /**
   * Reparent the camera into frame `frameId` (any node in `this.frameGraph`, not only
   * the entities frame) and aim it at `focusName` (a spacecraft/body name, or `null`
   * for the frame's own origin) -- the mechanism behind "focus the chaser in the
   * target's RIC frame" (M4.1 item 4).
   *
   * When `frameId` is the entities frame (`this._originFrameId`), this reduces to the
   * existing `setFocus`/floating-origin behaviour, unchanged.
   *
   * Otherwise (the camera moves to a *different* frame, e.g. an entity-relative RIC
   * frame declared via `Scenario.frame_ric()`): no per-frame `FloatingOrigin` rebase is
   * needed for the camera itself. `FrameNode.update()` (frames.js, driven every tick
   * by `frameGraph.update()` in `update()` below) already moves that frame's own
   * `Group.position` to the origin entity's *absolute* position via its declared
   * `originTrack`, as a plain `THREE.Vector3` -- a JS `number` triple, i.e. full f64,
   * never rounded through a `Float32Array`. Three's CPU-side scene-graph matrices
   * (`Object3D.matrixWorld`/`Matrix4.elements`) are plain JS-number arrays too, so
   * that "large" absolute component composes through the whole parent chain in f64;
   * it is cast to float32 only once, at GPU-uniform upload
   * (`modelViewMatrix`) and at vertex-buffer write (`LineGeometry.setPositions`).
   * The vertex-buffer path is already origin-subtracted for the entities frame
   * (`trajectoryRenderPositions`, unaffected by camera parentage). For the
   * `modelViewMatrix` upload: with the camera's *local* position kept small (an
   * RPO-scale offset from the frame's own origin, set below) and every entity's
   * `matrixWorld` sharing the same common ancestor the camera's chain passes through
   * (true whenever the RIC/VNB/VVLH frame's parent -- its `reference_body`'s MJ2000Eq
   * frame, question 76 -- coincides with the entities frame, which
   * `altavista/scenario.py`'s `_build_frames` arranges for the common case of a
   * scenario declared in that same body's MJ2000Eq frame), the large common term
   * cancels in `camera.matrixWorldInverse * object.matrixWorld` to f64 (not f32)
   * rounding error -- negligible at these scales. This is why only the camera's local
   * offset needs to be kept small here, not a second floating-origin instance per
   * frame. See `web/VIEWER.md` for the full writeup and `tests/test_viewer_jitter.py`
   * for the RPO centimetre bound this is built to support.
   */
  setViewFrame(frameId, focusName) {
    if (frameId !== this._originFrameId && !this.frameGraph.has(frameId)) return;
    this.focus = focusName || null;
    // M20.2 (question 134/E-27): whether this call actually changes which frame the
    // camera is parented in -- captured before `_cameraFrameId` is overwritten below,
    // so both branches can tell "just re-aiming within the same frame" (e.g. a
    // focus-only change) from "switching frames", which must refit (never leave the
    // camera at a stale position/near/far from whatever frame it was previously in --
    // see the origin-frame branch below for the concrete failure this fixes).
    const frameChanged = frameId !== this._cameraFrameId;
    if (frameChanged) {
      this._cameraFrameId = frameId;
      // The entities frame's render group (`_entitiesGroup`), never the bare frame
      // node, for the entities-frame case -- see the constructor's comment on
      // `_entitiesGroup` and this method's own docstring above.
      const target = frameId === this._originFrameId ? this._entitiesGroup : this.frameGraph.frame(frameId).object3D;
      target.attach(this.camera);
    }
    if (frameId === this._originFrameId) {
      // Restore the whole-scenario zoom clamp (see the non-entities branch below for
      // why it needs to shrink there) before delegating to the existing behaviour.
      this.controls.minDistance = ENTITIES_MIN_DISTANCE;
      this.controls.maxDistance = ENTITIES_MAX_DISTANCE;
      // M20.2 (E-27, "switching the view frame must refit the camera"): `attach()`
      // above preserves the camera's *world* position/orientation across the
      // reparent -- correct for keeping visual continuity within one frame, but wrong
      // for a real frame switch: coming from an RPO-scale frame (e.g. RIC, camera a
      // few hundred metres from a spacecraft, near/far sized for that same tiny
      // scale), the camera would otherwise land at that same absolute position/near/
      // far here, often well past the whole-scenario `far` plane computed below or
      // clipped to a sliver of the scene -- "the scene leaves view entirely", the
      // literal defect this fixes. `_fitOrigin()` is the exact function `fit()` uses
      // for this same frame, so a real reset and a frame-switch-into-the-entities-
      // frame share one formula. Only run on an actual frame change (not a focus-only
      // re-aim) so switching *which spacecraft* to focus on, unchanged since M4.1,
      // does not surprise the user with an unrequested zoom reset.
      if (frameChanged) this._fitOrigin();
      this.setFocus(this.focus);
      return;
    }
    // M26.3: extracted to `_frameCameraInto` (shared with `setViewportFrame()`, the
    // per-viewport equivalent of this whole method) -- identical formula, byte-for-byte,
    // to what this branch computed inline before M26.3 (body-scale-aware default radius,
    // near/far/zoom-clamp sizing, and the world-space `lookAt` correction OrbitControls'
    // own local-space `target` needs once the camera's parent frame has a non-negligible
    // world position of its own -- see `_frameCameraInto`'s own docstring for the full
    // per-step reasoning, preserved there rather than duplicated here).
    this._frameCameraInto(this.camera, this.controls, frameId, this.focus);
  }

  /** World-space position of a named spacecraft/body's rendered marker/mesh, into
   * `out`, or `null` if `name` is unset or unknown. Used by `setViewFrame`/`update()`
   * to aim the camera at whatever is focused regardless of which frame it lives under
   * (`Object3D.getWorldPosition` walks the real scene graph, so this is correct no
   * matter how deep the object's own frame is nested). */
  _focusWorldPosition(name, out) {
    if (!name) return null;
    if (this.spacecraft.has(name)) return this.spacecraft.get(name).marker.getWorldPosition(out);
    if (this.bodies.has(name)) return this.bodies.get(name).mesh.getWorldPosition(out);
    return null;
  }

  /** Enable/disable the floating origin for one frame (docs/open-questions.md Q46's
   * "switchable ... for every frame" half). Only the entities frame
   * (`this._originFrameId`) currently carries rendered geometry, so only toggling
   * *that* frame's id has a visible effect live; the call is real (not stubbed) for
   * every other frame id too, matching `origin.js`'s per-frame API and
   * `tests/test_viewer_jitter.py`'s direct coverage of it. */
  setFrameOriginEnabled(frameId, enabled) {
    this.floatingOrigin.setEnabledForFrame(frameId, enabled);
    if (frameId === this._originFrameId) {
      const o = this.floatingOrigin.getOrigin(frameId);
      this._rebaseOriginTo(o.x, o.y, o.z, true);
    }
    // M26.3: per-frame, per-viewport (Q46's "for every frame and view, not only the
    // focused one") -- each viewport's own FloatingOrigin gets the same enabled flag for
    // this frame id, independent of every other viewport's own setting for it.
    for (const vp of this.viewports.values()) {
      vp.floatingOrigin.setEnabledForFrame(frameId, enabled);
      if (frameId === this._originFrameId) {
        const vo = vp.floatingOrigin.getOrigin(frameId);
        this._rebaseViewportOriginTo(vp, vo.x, vo.y, vo.z, true);
      }
    }
  }

  // -------------------------------------------------------- floating origin plumbing
  /** Origin-relative (local, small) coordinates of absolute position `pos` under the
   * scene's one frame, written into `out` (a THREE.Vector3). Thin wrapper around
   * origin.js's `FloatingOrigin.toRenderSpace` -- the exact function the trajectory
   * geometry path uses too, not a parallel implementation. */
  _toLocal(pos, out) {
    const r = this.floatingOrigin.toRenderSpace(this._originFrameId, pos);
    return out.set(r.x, r.y, r.z);
  }

  /** Rebase the floating origin to absolute point (x,y,z): compensates the camera and
   * `controls.target` (both expressed in the entities frame's local, origin-relative
   * space -- only when the camera is actually parented there; see below) so their
   * world position is unchanged, moves `this._entitiesGroup`'s position (**not** the
   * frame node's own `object3D.position` -- see the constructor's comment on
   * `_entitiesGroup` for why: a frame node must stay an honest, always-zero anchor so
   * an entity-relative child frame's absolute origin track composes correctly) to
   * carry the new origin (f64, exact -- Object3D.position is a plain THREE.Vector3,
   * never a Float32Array), then rebuilds the geometry that was built once from static
   * data (trajectory lines, event markers) rather than recomputed every frame. Skipped
   * when neither the origin value nor the enabled/disabled state actually changed,
   * unless `force` is set (used by `fit()` and `setFloatingOriginEnabled()`, where a
   * rebuild is wanted even if the numeric origin happens to already match). */
  _rebaseOriginTo(x, y, z, force = false) {
    const fo = this.floatingOrigin;
    const oldEnabled = fo.isEnabledForFrame(this._originFrameId);
    const old = fo.getOrigin(this._originFrameId);
    fo.setOrigin(this._originFrameId, x, y, z);
    const newEnabled = fo.isEnabledForFrame(this._originFrameId);
    const shift = computeOriginShift(old, oldEnabled, { x, y, z }, newEnabled);
    if (!force && !shift.changed) return;
    const { dx, dy, dz, newShift } = shift;
    if (this._cameraFrameId === this._originFrameId) {
      // Only meaningful when the camera actually sits in the entities frame's
      // render group -- otherwise camera.position/controls.target are expressed in a
      // *different* frame's local space entirely (set by setViewFrame()) and this
      // shift does not apply to them.
      this.camera.position.x += dx; this.camera.position.y += dy; this.camera.position.z += dz;
      this.controls.target.x += dx; this.controls.target.y += dy; this.controls.target.z += dz;
    }
    this._entitiesGroup.position.set(newShift.x, newShift.y, newShift.z);
    this._refreshOriginRelativeGeometry();
  }

  /** Called once per render frame with the focus target's *absolute* position: rebase
   * the origin when drift exceeds ORIGIN_REBASE_DRIFT. This is the policy origin.js's
   * module docstring explicitly leaves to the caller ("no automatic origin re-basing
   * policy is implemented" -- VIEWER.md's now-closed integration gap). */
  _maybeRebaseOrigin(fpAbs) {
    const origin = this.floatingOrigin.getOrigin(this._originFrameId);
    const dx = fpAbs.x - origin.x, dy = fpAbs.y - origin.y, dz = fpAbs.z - origin.z;
    if (dx * dx + dy * dy + dz * dz > ORIGIN_REBASE_DRIFT_SQ) this._rebaseOriginTo(fpAbs.x, fpAbs.y, fpAbs.z);
  }

  /** Rebuild everything that was written from absolute data *once* rather than
   * recomputed every render frame, so it reflects the current origin: trajectory line
   * vertex buffers (the actual precision fix -- f64 CPU, origin-subtracted, f32
   * upload) and static event-marker positions. Body/spacecraft marker positions don't
   * need this: they're recomputed fresh from absolute interpolated data every
   * `update(t)` tick regardless (see below), so they never go stale between rebases. */
  _refreshOriginRelativeGeometry() {
    for (const s of this.spacecraft.values()) {
      if (!s.poly || s.poly.points.length < 6) continue;
      s.line.geometry.setPositions(trajectoryRenderPositions(s.poly, this.floatingOrigin, this._originFrameId));
      s.line.computeLineDistances();
      // LineGeometry.setPositions rebuilds instanceStart/instanceEnd from scratch,
      // resetting instanceCount to the full segment count; reapply the 'full' trail
      // policy here (matches setOptions()). 'past' mode is re-derived from `t` every
      // update() tick regardless, including the one this rebase happened inside.
      if (this.options.trail === 'full') s.line.geometry.instanceCount = s.segCount;
    }
    for (const e of this.events) {
      if (!e.interp) continue;
      e.interp.at(e.data.t, this._tmpAbs).multiplyScalar(SCALE);
      this._toLocal(this._tmpAbs, e.mesh.position);
    }
    // Footprint rings are also static per-sample geometry (like trajectory lines) --
    // rebuild the currently-shown ring (if any) in the new origin-relative space
    // rather than waiting for its sample index to change on some future tick.
    for (const f of this.footprints.values()) {
      if (f.lastIndex < 0) continue;
      const ring = f.data.ring[f.lastIndex];
      if (!ring || ring.length < 9) continue;
      f.line.geometry.setPositions(footprintRenderPositions(ring, this.floatingOrigin, this._originFrameId));
      f.line.computeLineDistances();
    }
  }

  /** Pick footprint `f`'s nearest recorded sample to A1MJD `t` and rebuild its ring
   * geometry only when that sample actually changes (cheap no-op most ticks). Hides
   * the ring when the nearest sample's boresight missed the ellipsoid entirely
   * (`ring` too short to close a loop -- altavista/model.py's Footprint docstring: a
   * ray that misses is omitted, never fabricated). */
  _updateFootprint(f, t) {
    const times = f.data.t;
    if (!times || times.length === 0) { f.line.visible = false; return; }
    let idx = findSegment(t, times);
    if (idx < times.length - 1 && (t - times[idx]) > (times[idx + 1] - t)) idx += 1;
    if (idx === f.lastIndex) return;
    f.lastIndex = idx;
    const ring = f.data.ring[idx];
    if (!ring || ring.length < 9) { f.line.visible = false; return; }
    f.line.geometry.setPositions(footprintRenderPositions(ring, this.floatingOrigin, this._originFrameId));
    f.line.computeLineDistances();
    f.line.visible = true;
  }

  /** Enable/disable the floating origin globally (docs/open-questions.md Q46's
   * "switchable globally" half; `floatingOrigin.setEnabledForFrame(id, bool|null)` is
   * the per-frame half of the same API, already exercised by
   * tests/test_viewer_jitter.py against origin.js directly). Forces an immediate
   * geometry refresh so the effect is visible without waiting for the next drift-
   * triggered rebase. */
  setFloatingOriginEnabled(enabled) {
    this.floatingOrigin.globalEnabled = !!enabled;
    const o = this.floatingOrigin.getOrigin(this._originFrameId);
    this._rebaseOriginTo(o.x, o.y, o.z, true);
    // M26.3: "switchable globally" (Q46) means every viewport, not only the primary --
    // each viewport's OWN FloatingOrigin gets the same global flag and an immediate
    // forced refresh, exactly mirroring the primary's own _rebaseOriginTo(..., true) call
    // above (never a silent per-viewport exception to a "global" toggle).
    for (const vp of this.viewports.values()) {
      vp.floatingOrigin.globalEnabled = !!enabled;
      const vo = vp.floatingOrigin.getOrigin(this._originFrameId);
      this._rebaseViewportOriginTo(vp, vo.x, vo.y, vo.z, true);
    }
  }

  // ------------------------------------------------------------------------ M26.3: viewports
  /**
   * Register a new, independent viewport (docs/ui-rework-plan.md's M26.3: "each viewport
   * owns its own view frame, focus, floating origin and camera"). `canvas`/`labelLayer`
   * are optional (a headless caller -- tests -- can construct a Viewport with no DOM at
   * all; see viewport.js's `attachCanvas`); when given, this viewport gets its own
   * `THREE.WebGLRenderer`/`OrbitControls` bound to that canvas, entirely independent of
   * `this.renderer`/`this.controls` (the legacy primary camera's own).
   *
   * The new viewport starts parented in the current entities frame
   * (`this._originFrameId`), unfocused, framed like a fresh "Reset view" -- call
   * `setViewportFrame(id, frameId, focusName)` afterward to give it a real role (e.g. the
   * RIC frame for an RPO close-up, or `enableGlobe`'s body for a globe pane -- globe.js's
   * GlobeLayer itself stays a single shared object on the default layer, visible to every
   * viewport once enabled, same as bodies/markers; see viewport.js's module docstring for
   * why only trajectory/footprint lines need a per-viewport copy).
   * @param {string} id
   * @param {HTMLCanvasElement} [canvas]
   * @param {HTMLElement} [labelLayer]
   * @returns {Viewport}
   */
  addViewport(id, canvas, labelLayer) {
    if (this.viewports.has(id)) throw new Error(`viewport '${id}' already registered`);
    const vp = new Viewport(id);
    if (canvas) vp.attachCanvas(canvas, labelLayer, { minDistance: ENTITIES_MIN_DISTANCE, maxDistance: ENTITIES_MAX_DISTANCE });
    this.frameGraph.frame(this._originFrameId).object3D.add(vp.renderGroup);
    // Starts parented under vp.renderGroup (this viewport's own entities-frame render
    // group -- see setViewportFrame()'s own comment on why, not the bare frame node).
    vp.renderGroup.add(vp.camera);
    this._buildViewportLines(vp);
    this._buildViewportLabels(vp);
    vp.cameraFrameId = this._originFrameId;
    this.viewports.set(id, vp);
    this._fitViewportOrigin(vp);
    return vp;
  }

  /** Tear down and unregister viewport `id`: disposes its own line clones/renderer (GPU
   * resources -- viewport.js's `Viewport.dispose()`), removes its render group from the
   * frame graph, and drops the label clones. No-op if `id` is unknown. */
  removeViewport(id) {
    const vp = this.viewports.get(id);
    if (!vp) return;
    vp.renderGroup.removeFromParent();
    vp.dispose();
    if (vp.labels) {
      for (const el of vp.labels.bodies.values()) el.remove();
      for (const el of vp.labels.spacecraft.values()) el.remove();
      for (const el of vp.labels.events) el.remove();
    }
    this.viewports.delete(id);
  }

  viewportList() {
    return [...this.viewports.keys()];
  }

  /** Build (or rebuild, after a scenario reload) viewport `vp`'s own trajectory/footprint
   * `Line2` clones -- one per `this.spacecraft`/`this.footprints` entry, each geometry
   * built via the exact same exported `trajectoryRenderPositions`/`footprintRenderPositions`
   * the primary camera's own lines use (setScenario() above), just against `vp`'s own
   * `floatingOrigin` instead of `this.floatingOrigin`. See viewport.js's module docstring
   * for why a per-viewport copy is required here specifically (and not for bodies/
   * markers/events). */
  _buildViewportLines(vp) {
    const w = (vp.canvas && vp.canvas.clientWidth) || this.canvas.clientWidth || 1;
    const h = (vp.canvas && vp.canvas.clientHeight) || this.canvas.clientHeight || 1;
    for (const [name, s] of this.spacecraft) {
      const geometry = new LineGeometry();
      geometry.setPositions(trajectoryRenderPositions(s.poly, vp.floatingOrigin, this._originFrameId));
      const material = new LineMaterial({ color: new THREE.Color(s.data.color || '#ffffff'), linewidth: 1.6, transparent: true, opacity: 0.9 });
      material.resolution.set(w, h);
      const line = new Line2(geometry, material);
      line.computeLineDistances();
      line.layers.set(vp.layer);
      if (this.options.trail === 'full') line.geometry.instanceCount = s.segCount;
      line.visible = s.visible && this.options.trail !== 'off';
      vp.renderGroup.add(line);
      vp.lines.set(name, { line, segCount: s.segCount });
    }
    for (const [name, f] of this.footprints) {
      const material = new LineMaterial({ color: new THREE.Color(f.data.color || '#00e5ff'), linewidth: 1.2, transparent: true, opacity: 0.85 });
      material.resolution.set(w, h);
      const geometry = new LineGeometry();
      geometry.setPositions(new Float32Array([0, 0, 0, 0, 0, 0]));
      const line = new Line2(geometry, material);
      line.computeLineDistances();
      line.visible = false;
      line.layers.set(vp.layer);
      vp.renderGroup.add(line);
      vp.footprintLines.set(name, { line, lastIndex: -1 });
    }
  }

  /** Clone every body/spacecraft/event label div into viewport `vp`'s own `labelLayer`
   * (a no-op if `vp.labelLayer` is unset -- a headless/label-less viewport). Each clone
   * is positioned every tick from `vp`'s OWN camera in `_updateViewportLabels`, never
   * the primary's -- "labels ... per viewport" (M26.3's own brief). */
  _buildViewportLabels(vp) {
    if (!vp.labelLayer) { vp.labels = null; return; }
    vp.labels = { bodies: new Map(), spacecraft: new Map(), events: [] };
    for (const [name, b] of this.bodies) vp.labels.bodies.set(name, this._cloneLabelInto(vp.labelLayer, b.label));
    for (const [name, s] of this.spacecraft) vp.labels.spacecraft.set(name, this._cloneLabelInto(vp.labelLayer, s.label));
    for (const e of this.events) vp.labels.events.push(this._cloneLabelInto(vp.labelLayer, e.label));
  }

  _cloneLabelInto(layer, srcEl) {
    const el = document.createElement('div');
    el.className = srcEl.className;
    el.textContent = srcEl.textContent;
    el.style.color = srcEl.style.color;
    el.style.display = srcEl.style.display;
    layer.appendChild(el);
    return el;
  }

  /**
   * Reparent viewport `id`'s camera into frame `frameId` and aim it at `focusName` --
   * the per-viewport equivalent of `setViewFrame()` above (M4.1's "focus the chaser in
   * the target's RIC frame", now per viewport rather than only for the single legacy
   * camera). Shares `setViewFrame()`'s exact framing math (`defaultFrameViewRadius`,
   * body-scale awareness, the world-space `lookAt` correction for a non-entities frame)
   * via the small private helpers both now call
   * (`_frameCameraInto`/`_focusWorldPosition`) rather than a second, hand-duplicated copy.
   */
  setViewportFrame(id, frameId, focusName) {
    const vp = this.viewports.get(id);
    if (!vp) throw new Error(`unknown viewport '${id}'`);
    if (frameId !== this._originFrameId && !this.frameGraph.has(frameId)) return;
    vp.focus = focusName || null;
    const frameChanged = frameId !== vp.cameraFrameId;
    if (frameChanged) {
      vp.cameraFrameId = frameId;
      // `vp.renderGroup` (never its parent -- the frame node itself must stay a
      // zero-transform anchor, same reasoning as `_entitiesGroup`, see the constructor's
      // comment on it): this is what makes `vp.camera.position` origin-relative (small)
      // when parented in the entities frame, so `_rebaseViewportOriginTo`'s camera-
      // position compensation (which assumes exactly this parenting) is correct.
      const target = frameId === this._originFrameId ? vp.renderGroup : this.frameGraph.frame(frameId).object3D;
      target.attach(vp.camera);
    }
    if (frameId === this._originFrameId) {
      if (vp.controls) { vp.controls.minDistance = ENTITIES_MIN_DISTANCE; vp.controls.maxDistance = ENTITIES_MAX_DISTANCE; }
      if (frameChanged) this._fitViewportOrigin(vp);
      this._setViewportFocus(vp, vp.focus);
      return;
    }
    this._frameCameraInto(vp.camera, vp.controls, frameId, vp.focus);
  }

  /** Shared "point a camera at a frame/focus, sized to that frame's own scale" math --
   * extracted from setViewFrame() so setViewportFrame() above reuses it exactly rather
   * than re-deriving the same formula a second time. `camera`/`controls` are whichever
   * viewport's own (controls may be null for a headless/DOM-less viewport, in which case
   * only the camera itself is positioned -- there is no OrbitControls target to set). */
  _frameCameraInto(camera, controls, frameId, focusName) {
    const frameObj = this.frameGraph.frame(frameId).object3D;
    const originWorld = frameObj.getWorldPosition(new THREE.Vector3());
    let targetWorld = originWorld.clone();
    const focusWorld = this._focusWorldPosition(focusName, new THREE.Vector3());
    if (focusWorld) targetWorld = focusWorld;
    const local = frameObj.worldToLocal(targetWorld.clone());
    const originLocal = frameObj.worldToLocal(originWorld.clone());
    const sep = local.distanceTo(originLocal);
    const frameDef = this.frameGraph.frame(frameId).def;
    const bodyEntry = frameDef && frameDef.body ? this.bodies.get(frameDef.body) : null;
    const radius = sep > 0 ? sep * 3 : defaultFrameViewRadius(bodyEntry ? bodyEntry.data.radius : undefined);
    if (controls) controls.target.copy(local);
    const dir = new THREE.Vector3(1, -1.2, 0.7).normalize();
    camera.position.copy(local).add(dir.multiplyScalar(radius));
    camera.near = Math.max(radius * 1e-4, 1e-9);
    camera.far = Math.max(radius * 1e4, 1e-3);
    camera.updateProjectionMatrix();
    if (controls) {
      controls.minDistance = Math.max(radius * 1e-3, 1e-9);
      controls.maxDistance = Math.max(radius * 1e3, 1e-3);
      controls.update();
    }
    camera.lookAt(targetWorld);
  }

  /** Per-viewport equivalent of `_fitOrigin()`: reset viewport `vp`'s camera within the
   * entities frame, rebased to (0,0,0), sized from `this.fitRadius` (shared -- the
   * whole-scenario extent is not viewport-specific). */
  _fitViewportOrigin(vp) {
    const r = this.fitRadius;
    const dir = new THREE.Vector3(1, -1.2, 0.7).normalize();
    this._rebaseViewportOriginTo(vp, 0, 0, 0, true);
    if (vp.controls) vp.controls.target.set(0, 0, 0);
    vp.camera.position.copy(dir.multiplyScalar(r * FIT_DISTANCE_FACTOR));
    vp.camera.near = Math.max(r * 1e-6, 1e-4);
    vp.camera.far = Math.max(r * 1e4, 1e6);
    vp.camera.updateProjectionMatrix();
    if (vp.controls) vp.controls.update();
    vp.focusPrev.set(0, 0, 0);
  }

  /** Per-viewport `fit()` ("Reset view", keeping the viewport's currently selected frame,
   * same policy as `fit()`/`resetTargetFrameId` above for the primary camera). */
  fitViewport(id) {
    const vp = this.viewports.get(id);
    if (!vp) throw new Error(`unknown viewport '${id}'`);
    const targetFrameId = resetTargetFrameId(vp.cameraFrameId, this._originFrameId);
    if (targetFrameId === this._originFrameId) {
      if (vp.controls) { vp.controls.minDistance = ENTITIES_MIN_DISTANCE; vp.controls.maxDistance = ENTITIES_MAX_DISTANCE; }
      this._fitViewportOrigin(vp);
      return;
    }
    this.setViewportFrame(id, targetFrameId, null);
  }

  _setViewportFocus(vp, name) {
    vp.focus = name || null;
    const p = this._focusPositionFor(vp.focus, this._lastT ?? 0, vp._tmp);
    const local = vp.floatingOrigin.toRenderSpace(this._originFrameId, p);
    if (vp.controls) vp.controls.target.set(local.x, local.y, local.z);
    vp.focusPrev.copy(p);
    if (name && this.bodies.has(name)) {
      const b = this.bodies.get(name);
      const d = b.data.radius * SCALE * 4;
      const off = new THREE.Vector3().subVectors(vp.camera.position, vp.controls ? vp.controls.target : new THREE.Vector3(local.x, local.y, local.z));
      if (off.length() > d * 20 || off.length() < d * 0.1) off.setLength(d);
      vp.camera.position.set(local.x, local.y, local.z).add(off);
    } else if (name && this.spacecraft.has(name)) {
      const off = new THREE.Vector3().subVectors(vp.camera.position, vp.controls ? vp.controls.target : new THREE.Vector3(local.x, local.y, local.z));
      const central = [...this.bodies.values()].find(b => b.data.central);
      const d = central ? central.data.radius * SCALE * 1.5 : this.fitRadius * 0.3;
      if (off.length() > d * 20) off.setLength(d);
      vp.camera.position.set(local.x, local.y, local.z).add(off);
    }
    if (vp.controls) vp.controls.update();
  }

  /** Set which body/spacecraft viewport `id` is focused on, within whatever frame it is
   * currently parented in. Per-viewport equivalent of `setFocus()`. */
  setViewportFocus(id, name) {
    const vp = this.viewports.get(id);
    if (!vp) throw new Error(`unknown viewport '${id}'`);
    this._setViewportFocus(vp, name);
  }

  // ----------------------------------------------------- M26.3: per-viewport floating origin
  /** Per-viewport equivalent of `_maybeRebaseOrigin` -- drift-triggered rebase against
   * `vp`'s OWN `floatingOrigin`, using `vp.focus`'s absolute position, entirely
   * independent of any other viewport's (including the primary's) drift/rebase state.
   * This -- one `FloatingOrigin` instance per viewport, checked against that viewport's
   * own focus -- is the actual mechanism that keeps two viewports' precision independent
   * (see web/js/viewport_check.mjs's `measureRpoWithSharedOriginBug` for what breaks if a
   * single shared instance is used here instead). */
  _maybeRebaseViewportOrigin(vp, fpAbs) {
    const origin = vp.floatingOrigin.getOrigin(this._originFrameId);
    const dx = fpAbs.x - origin.x, dy = fpAbs.y - origin.y, dz = fpAbs.z - origin.z;
    if (dx * dx + dy * dy + dz * dz > ORIGIN_REBASE_DRIFT_SQ) this._rebaseViewportOriginTo(vp, fpAbs.x, fpAbs.y, fpAbs.z);
  }

  /** Per-viewport equivalent of `_rebaseOriginTo`: rebases `vp.floatingOrigin`'s entry for
   * the entities frame, compensates `vp.camera`/`vp.controls.target` (only if `vp`'s
   * camera is actually parented in the entities frame right now) and moves `vp.renderGroup`
   * (never the frame node itself -- same reasoning as `_entitiesGroup`, see the
   * constructor's comment on it) to carry the new origin, then rebuilds `vp`'s own line
   * clones (`_refreshViewportGeometry`) -- and ONLY `vp`'s own; no other viewport's
   * `floatingOrigin`/lines/camera are read or written by this call. */
  _rebaseViewportOriginTo(vp, x, y, z, force = false) {
    const fo = vp.floatingOrigin;
    const oldEnabled = fo.isEnabledForFrame(this._originFrameId);
    const old = fo.getOrigin(this._originFrameId);
    fo.setOrigin(this._originFrameId, x, y, z);
    const newEnabled = fo.isEnabledForFrame(this._originFrameId);
    // Same pure shift computation `_rebaseOriginTo` (the primary camera's own rebase)
    // uses -- one implementation of "old shift vs. new shift" for both, not two that
    // could silently drift apart. See its own docstring (this file, above) and
    // web/js/viewport_check.mjs's direct test of it.
    const shift = computeOriginShift(old, oldEnabled, { x, y, z }, newEnabled);
    if (!force && !shift.changed) return;
    const { dx, dy, dz, newShift } = shift;
    if (vp.cameraFrameId === this._originFrameId) {
      vp.camera.position.x += dx; vp.camera.position.y += dy; vp.camera.position.z += dz;
      if (vp.controls) { vp.controls.target.x += dx; vp.controls.target.y += dy; vp.controls.target.z += dz; }
    }
    vp.renderGroup.position.set(newShift.x, newShift.y, newShift.z);
    this._refreshViewportGeometry(vp);
  }

  /** Rebuild viewport `vp`'s own trajectory-line/footprint-ring vertex buffers against its
   * current `floatingOrigin` -- the per-viewport equivalent of
   * `_refreshOriginRelativeGeometry()`, touching only `vp.lines`/`vp.footprintLines`. */
  _refreshViewportGeometry(vp) {
    for (const [name, s] of this.spacecraft) {
      const clone = vp.lines.get(name);
      if (!clone || !s.poly || s.poly.points.length < 6) continue;
      clone.line.geometry.setPositions(trajectoryRenderPositions(s.poly, vp.floatingOrigin, this._originFrameId));
      clone.line.computeLineDistances();
      if (this.options.trail === 'full') clone.line.geometry.instanceCount = s.segCount;
    }
    for (const [name, f] of this.footprints) {
      const clone = vp.footprintLines.get(name);
      if (!clone || f.lastIndex < 0) continue;
      const ring = f.data.ring[f.lastIndex];
      if (!ring || ring.length < 9) continue;
      clone.line.geometry.setPositions(footprintRenderPositions(ring, vp.floatingOrigin, this._originFrameId));
      clone.line.computeLineDistances();
      clone.lastIndex = f.lastIndex;
    }
  }

  // -------------------------------------------------------------------- M26.3: per-tick
  /**
   * Update and render viewport `vp` for the CURRENT tick -- called once per viewport from
   * `update(t)` below, after the shared per-tick state (frame graph, body/spacecraft/
   * event/footprint absolute positions) has already been computed for this same `t`. This
   * is the "one clock ... many cameras" half made concrete: `t` is passed in by `update(t)`
   * (never read from any per-viewport clock of its own -- there is no such thing), so
   * every viewport this loop visits this tick renders the exact same epoch as the primary
   * camera and every other viewport, by construction (see web/js/viewport_check.mjs's
   * shared-clock check for the headless proof, at the frame-graph level, that one
   * `FrameGraph.update(t, scale)` call is what every viewport's frame position ultimately
   * reads from).
   */
  _updateViewport(vp, t) {
    const fp = this._focusPositionFor(vp.focus, t, vp._tmp);
    this._maybeRebaseViewportOrigin(vp, fp);
    const cam = vp.camera;
    if (vp.cameraFrameId === this._originFrameId) {
      if (!fp.equals(vp.focusPrev)) {
        const delta = new THREE.Vector3().subVectors(fp, vp.focusPrev);
        cam.position.add(delta);
        const local = vp.floatingOrigin.toRenderSpace(this._originFrameId, fp);
        if (vp.controls) vp.controls.target.set(local.x, local.y, local.z);
        vp.focusPrev.copy(fp);
      }
      if (vp.controls) vp.controls.update();
    } else {
      vp.focusPrev.copy(fp);
      const frameObj = this.frameGraph.frame(vp.cameraFrameId).object3D;
      let worldPos = vp.focus ? this._focusWorldPosition(vp.focus, vp._tmpAbs) : null;
      if (!worldPos) worldPos = frameObj.getWorldPosition(vp._tmpAbs);
      const local = frameObj.worldToLocal(worldPos.clone());
      if (vp.controls) {
        const delta = new THREE.Vector3().subVectors(local, vp.controls.target);
        cam.position.add(delta);
        vp.controls.target.copy(local);
        vp.controls.update();
      }
      cam.lookAt(worldPos);
    }
    // Footprint ring clones follow the primary's own nearest-sample index
    // (_updateFootprint(f, t), already run for `this.footprints` earlier in update(t)) --
    // rebuild only the viewports whose clone is now stale (index changed since this
    // viewport's own last refresh), same "cheap no-op most ticks" shape as the primary.
    for (const [name, f] of this.footprints) {
      const clone = vp.footprintLines.get(name);
      if (!clone) continue;
      if (f.lastIndex < 0) { clone.line.visible = false; continue; }
      if (f.lastIndex !== clone.lastIndex) {
        const ring = f.data.ring[f.lastIndex];
        if (ring && ring.length >= 9) {
          clone.line.geometry.setPositions(footprintRenderPositions(ring, vp.floatingOrigin, this._originFrameId));
          clone.line.computeLineDistances();
          clone.line.visible = true;
        } else {
          clone.line.visible = false;
        }
        clone.lastIndex = f.lastIndex;
      }
    }
    if (vp.renderer) vp.renderer.render(this.scene, cam);
    if (this.options.labels && vp.labels) this._updateViewportLabels(vp);
  }

  _updateViewportLabels(vp) {
    if (!vp.canvas) return;
    const w = vp.canvas.clientWidth, h = vp.canvas.clientHeight;
    for (const [name, b] of this.bodies) {
      const el = vp.labels.bodies.get(name);
      const d = b.mesh.position.distanceTo(vp.camera.position);
      const r = b.data.radius * SCALE;
      this._placeLabel(el, b.mesh, b.visible && d > r * 1.05, vp.camera, w, h);
    }
    for (const [name, s] of this.spacecraft) this._placeLabel(vp.labels.spacecraft.get(name), s.marker, s.visible, vp.camera, w, h);
    for (let i = 0; i < this.events.length; i++) this._placeLabel(vp.labels.events[i], this.events[i].mesh, this.events[i].mesh.visible, vp.camera, w, h);
  }

  /** Shared label-placement math -- `_updateLabels()` (primary) and
   * `_updateViewportLabels()` (every extra viewport) both call this with their own
   * camera/label-element/dimensions rather than duplicating the projection math. */
  _placeLabel(el, mesh, visible, camera, w, h) {
    if (!el) return;
    if (!visible) { el.style.display = 'none'; return; }
    const v = this._tmp.setFromMatrixPosition(mesh.matrixWorld).project(camera);
    if (v.z > 1 || v.z < -1) { el.style.display = 'none'; return; }
    el.style.display = '';
    el.style.left = ((v.x + 1) / 2 * w) + 'px';
    el.style.top = ((1 - v.y) / 2 * h) + 'px';
  }

  // -------------------------------------------------------------------- M26.3: picking
  /**
   * Resolve a click at NDC `(ndcX, ndcY)` (each in [-1, 1]) against camera `camera` --
   * "picking resolves against the correct viewport" (M26.3's own brief): the caller
   * (app.js) passes whichever viewport's own camera the click landed in (`this.camera`
   * for the primary pane, or `viewer.viewports.get(id).camera` for any other pane), and
   * this method casts the ray from THAT camera, never a hardcoded one -- see
   * viewport.js's exported `pickAlongCamera` (the actual raycast, testable headlessly
   * with no Viewer at all) for the mechanism and its own docstring for why no
   * `THREE.Layers` filtering is needed here (every pick target is a single object shared
   * by every viewport).
   * @param {THREE.Camera} camera
   * @param {number} ndcX
   * @param {number} ndcY
   * @returns {{kind: 'body'|'spacecraft'|'event', name: string}|null}
   */
  pick(camera, ndcX, ndcY) {
    const targets = [];
    for (const b of this.bodies.values()) if (b.visible && b.mesh.visible) targets.push(b.mesh);
    for (const s of this.spacecraft.values()) if (s.visible && s.marker.visible) targets.push(s.marker);
    for (const e of this.events) if (e.mesh.visible) targets.push(e.mesh);
    const hit = pickAlongCamera(camera, ndcX, ndcY, targets);
    if (!hit) return null;
    for (const [name, b] of this.bodies) if (b.mesh === hit) return { kind: 'body', name };
    for (const [name, s] of this.spacecraft) if (s.marker === hit) return { kind: 'spacecraft', name };
    for (const e of this.events) if (e.mesh === hit) return { kind: 'event', name: e.data.name };
    return null;
  }

  // ------------------------------------------------------------------ M15.4: globe
  /**
   * Enable the quadtree tiled globe (web/js/globe.js's GlobeLayer) for body
   * `bodyName` ('Earth' by default). Replaces that body's plain textured sphere
   * (`makeBodyMesh()`'s `mesh`) with LOD-selected WGS84 ellipsoid tiles; the sphere
   * mesh itself is only hidden, not disposed, so `disableGlobe()` can restore it
   * without rebuilding anything. Never silent: returns `false` and logs a
   * `console.warn` if `bodyName` isn't in the current scenario (mirrors
   * `_buildFrameGraph`'s "never a silent fallback" convention elsewhere in this file).
   * @param {string} [bodyName]
   * @param {object} [opts] forwarded to `new GlobeLayer(opts)` (imageryUrl, sseThreshold, maxLevel, maxTiles, residentBudget, segments, textureLoader)
   * @returns {boolean}
   */
  enableGlobe(bodyName = 'Earth', opts = {}) {
    const b = this.bodies.get(bodyName);
    if (!b) { console.warn(`altavista: enableGlobe('${bodyName}') -- no such body in the current scenario`); return false; }
    if (this.globeLayer) { this.globeLayer.dispose(); this._entitiesGroup.remove(this.globeLayer.group); }
    this.globeLayer = new GlobeLayer(opts);
    this.globeBodyName = bodyName;
    this._entitiesGroup.add(this.globeLayer.group);
    b.mesh.visible = false;
    return true;
  }

  /** Undo enableGlobe(): dispose the tile geometry/textures, restore the plain sphere. */
  disableGlobe() {
    if (!this.globeLayer) return;
    this.globeLayer.dispose();
    this._entitiesGroup.remove(this.globeLayer.group);
    this.globeLayer = null;
    const b = this.globeBodyName ? this.bodies.get(this.globeBodyName) : null;
    if (b) b.mesh.visible = b.visible;
    this.globeBodyName = null;
  }

  /**
   * Per-tick globe upkeep, called from update() below once the current body
   * positions/quaternions are known. Tracks `body.mesh`'s already origin-relative
   * transform exactly (plain position/quaternion copy -- see globe.js's module
   * docstring for why the globe never needs its own floating-origin treatment), then
   * computes the camera's position **in that body's local frame** (scene units) via
   * `worldToLocal` and hands it to `GlobeLayer.update()`, which does the real
   * screen-space-error tile selection (web/js/globe_lod.js).
   */
  _syncGlobeLayer(cam) {
    const layer = this.globeLayer;
    const b = this.bodies.get(this.globeBodyName);
    if (!layer || !b) return;
    layer.group.position.copy(b.mesh.position);
    layer.group.quaternion.copy(b.mesh.quaternion);
    layer.group.updateMatrixWorld(true);
    const camLocal = layer.group.worldToLocal(cam.getWorldPosition(this._tmpAbs));
    const h = this._referenceCanvasHeight();
    layer.update(camLocal, h, THREE.MathUtils.degToRad(cam.fov));
  }

  // ------------------------------------------------------------- M15.4/M16.4: 3D Tiles overlay
  /**
   * Lazily create (or return) a `FrameGraph` node representing body `bodyName`'s
   * *body-fixed* frame -- rotates and translates with the body itself (Earth's real
   * ECEF-equivalent orientation), unlike `this._originFrameId` (the entities frame,
   * inertial). This is a real `FrameNode`/`frameGraph.addFrame()` registration, not a
   * plain `THREE.Group` -- so `frameGraph.reparent()`/`frameOf()` work on it like any
   * other frame, and a consumer (currently only the 3D Tiles overlay, M16.4) can be
   * "placed into the body-fixed frame through the frame graph", this task's brief,
   * verbatim.
   *
   * Not itself driven by `FrameNode.update()`'s own origin-track/axesKind machinery
   * (frames.js): a body's real position/orientation already comes from `BodyInterp`
   * (interp.js) once per tick in `update()` below, straight onto `b.mesh.position`/
   * `b.mesh.quaternion` -- re-deriving the same motion a second way here (e.g. via a
   * synthetic originTrack) would risk the two ever disagreeing. `_syncBodyFixedFrame`
   * instead keeps this node in lockstep with the already-computed body mesh, the same
   * pattern `_syncGlobeLayer` already uses for `globeLayer.group` (a plain sibling,
   * not a frame node) -- parented at `this._originFrameId` (the entities frame), the
   * body mesh's own parent, so copying `b.mesh`'s *local* position/quaternion
   * reproduces its exact world transform (both are children of parents with zero
   * local transform relative to each other; see `update()`'s body loop and this
   * class's constructor comments on `_entitiesGroup`).
   *
   * Scoped narrowly to what the tiles overlay needs: a scenario's own declared
   * `AXES_KIND_BODY_FIXED` frames (if any, in `sc.frames`) are unaffected and still
   * do not rotate (`AXES_KIND_MAP` has no entry for them) -- a pre-existing,
   * out-of-scope gap for this task, not newly introduced here (see web/VIEWER.md).
   */
  _bodyFixedFrame(bodyName) {
    const id = `body-fixed:${bodyName}`;
    if (!this.frameGraph.has(id)) {
      this.frameGraph.addFrame({ id, parentId: this._originFrameId, axesKind: null });
    }
    return this.frameGraph.frame(id);
  }

  /** Keep body `bodyName`'s body-fixed frame node's local transform equal to its
   * already-updated mesh's local transform this tick -- see `_bodyFixedFrame`'s
   * docstring for why a plain copy (not a re-derived track) is correct here. No-op
   * if the body doesn't exist this tick (frame node, if any, simply stays at its
   * last known transform) or if nothing has ever asked for this body's frame. */
  _syncBodyFixedFrame(bodyName) {
    const id = `body-fixed:${bodyName}`;
    if (!this.frameGraph.has(id)) return;
    const b = this.bodies.get(bodyName);
    if (!b) return;
    const node = this.frameGraph.frame(id).object3D;
    node.position.copy(b.mesh.position);
    node.quaternion.copy(b.mesh.quaternion);
  }

  /**
   * Load one 3D Tiles tileset through the vendored NASA AMMOS 3DTilesRendererJS,
   * behind this codebase's own layer interface (web/js/tiles_layer.js's
   * `TilesOverlayLayer` -- docs/open-questions.md Q44). This is an **overlay layer,
   * not the globe** (item 4 of this task's brief): unlike `enableGlobe()` above, it
   * shares no geometry/tile-scheduling code with `GlobeLayer` (`web/js/globe.js`) --
   * but it now *does* reuse the same `web/js/globe_lod.js`, both for
   * `SCENE_UNITS_PER_METRE` (`placeOverlayInBodyFixedFrame`) and for
   * `TileLoadScheduler` (inside `TilesOverlayLayer` itself, see that file's module
   * docstring) -- and it is placed for real: `placeOverlayInBodyFixedFrame` reads the
   * tileset's own root.transform (real ECEF metres, via the vendored TilesRenderer's
   * own premultiply of loaded content -- see tiles_layer.js) and reparents the
   * overlay's group into `bodyName`'s *body-fixed* frame node
   * (`_bodyFixedFrame`/`_syncBodyFixedFrame` above), replacing M15.4's fixed demo
   * scale/offset in the (non-rotating) entities frame.
   * @param {string} url
   * @param {string} [bodyName] which body's body-fixed frame to geo-reference
   *   against -- defaults to 'Earth', matching every example scenario that loads a
   *   3D Tiles overlay today.
   */
  loadTilesOverlay(url, bodyName = 'Earth') {
    this.clearTilesOverlay();
    this.tilesOverlay = new TilesOverlayLayer(url);
    this.tilesOverlay.attachCamera(this.camera, this.renderer);
    this._tilesOverlayBodyName = bodyName;
    const frameNode = this._bodyFixedFrame(bodyName);
    placeOverlayInBodyFixedFrame(this.tilesOverlay.group, this.frameGraph, frameNode.id);
    this._syncBodyFixedFrame(bodyName); // avoid one tick of stale (identity) orientation before the next update()
  }

  clearTilesOverlay() {
    if (!this.tilesOverlay) return;
    this.tilesOverlay.group.removeFromParent();
    this.tilesOverlay.dispose();
    this.tilesOverlay = null;
    this._tilesOverlayBodyName = null;
  }

  _label(text, cls) {
    const el = document.createElement('div');
    el.className = 'label ' + cls;
    el.textContent = text;
    this.labelLayer.appendChild(el);
    return el;
  }

  /**
   * Full camera reset within the CURRENTLY selected view frame (M20.2, question
   * 134/E-27's "Reset view must keep the currently selected frame" half): re-frames
   * the camera in `this._cameraFrameId` -- `_fitOrigin()`'s whole-scenario
   * `fitRadius` math (byte-identical to the pre-M20.2 `fit()`) when parented in the
   * entities frame, or delegates to `setViewFrame()`'s own framing -- now body-scale
   * aware, see `defaultFrameViewRadius()` -- for any other frame. Never forces the
   * camera back to the entities frame the way the pre-M20.2 implementation always
   * did: that, combined with `app.js`'s Reset handler forcing the frame picker back
   * to its first option, is what used to strand a user with no way to land in ICRF
   * with the scene framed (question 134/E-27, decided together).
   */
  fit() {
    const targetFrameId = resetTargetFrameId(this._cameraFrameId, this._originFrameId);
    if (targetFrameId === this._originFrameId) {
      this.controls.minDistance = ENTITIES_MIN_DISTANCE;
      this.controls.maxDistance = ENTITIES_MAX_DISTANCE;
      this._fitOrigin();
      return;
    }
    this.setViewFrame(targetFrameId, null);
  }

  /**
   * The entities-frame camera reset: `fitRadius * FIT_DISTANCE_FACTOR` scene units
   * out along a fixed direction, near/far sized off `fitRadius`, floating origin
   * rebased to (0,0,0).
   * Extracted verbatim from the pre-M20.2 `fit()` (identical order of operations,
   * identical constants -- this is a pure refactor, not a behaviour change) so a
   * plain "Reset view" while already in the entities frame and a frame switch *into*
   * the entities frame (`setViewFrame()`'s own origin branch, E-27's "switching
   * frames must refit" half) share exactly one implementation instead of two that
   * could drift apart.
   */
  _fitOrigin() {
    const r = this.fitRadius;
    const dir = new THREE.Vector3(1, -1.2, 0.7).normalize();
    // Rebase the floating origin back to (0,0,0) so camera.position/controls.target
    // below (both expressed in the frame's local, origin-relative space) coincide
    // with absolute coordinates.
    this._rebaseOriginTo(0, 0, 0, true);
    this.controls.target.set(0, 0, 0);
    this.camera.position.copy(dir.multiplyScalar(r * FIT_DISTANCE_FACTOR));
    this.camera.near = Math.max(r * 1e-6, 1e-4);
    this.camera.far = Math.max(r * 1e4, 1e6);
    this.camera.updateProjectionMatrix();
    this.controls.update();
    this._focusPrev.set(0, 0, 0);
  }

  setFocus(name) {
    this.focus = name || null;
    const p = this._focusPosition(this._lastT ?? 0, this._tmp); // absolute
    const local = this.floatingOrigin.toRenderSpace(this._originFrameId, p);
    this.controls.target.set(local.x, local.y, local.z);
    this._focusPrev.copy(p);
    if (name && this.bodies.has(name)) {
      const b = this.bodies.get(name);
      const d = b.data.radius * SCALE * 4;
      const off = new THREE.Vector3().subVectors(this.camera.position, this.controls.target);
      if (off.length() > d * 20 || off.length() < d * 0.1) off.setLength(d);
      this.camera.position.set(local.x, local.y, local.z).add(off);
    } else if (name && this.spacecraft.has(name)) {
      const off = new THREE.Vector3().subVectors(this.camera.position, this.controls.target);
      const central = [...this.bodies.values()].find(b => b.data.central);
      const d = central ? central.data.radius * SCALE * 1.5 : this.fitRadius * 0.3;
      if (off.length() > d * 20) off.setLength(d);
      this.camera.position.set(local.x, local.y, local.z).add(off);
    }
    this.controls.update();
    // The camera/target above used whatever origin was current; a drift-triggered
    // rebase happens on the very next update(t) tick (1/60s later) if the new focus
    // is far from it -- see _maybeRebaseOrigin.
  }

  setVisible(kind, name, visible) {
    const m = kind === 'body' ? this.bodies : this.spacecraft;
    const o = m.get(name);
    if (!o) return;
    o.visible = visible;
    if (kind === 'body') { o.mesh.visible = visible; if (!visible) o.dot.visible = false; }
    else {
      o.marker.visible = visible;
      o.line.visible = visible && this.options.trail !== 'off';
      // M26.3: every viewport's own clone of this spacecraft's line follows the same
      // visibility (a shared, per-entity property -- not viewport-specific).
      for (const vp of this.viewports.values()) {
        const clone = vp.lines.get(name);
        if (clone) clone.line.visible = visible && this.options.trail !== 'off';
      }
    }
    o.label.style.display = visible ? '' : 'none';
  }

  setOptions(opts) {
    Object.assign(this.options, opts);
    this.stars.visible = this.options.stars;
    this.axes.visible = !!this.options.axes;
    this.grid.visible = !!this.options.grid;
    for (const s of this.spacecraft.values()) {
      s.line.visible = s.visible && this.options.trail !== 'off';
      if (this.options.trail === 'full') s.line.geometry.instanceCount = s.segCount;
    }
    this.labelLayer.style.display = this.options.labels ? '' : 'none';
    // M26.3: propagate trail/labels options to every viewport's own line clones/label
    // layer -- a shared, per-scenario option (not per-viewport), so it must not silently
    // apply only to the primary pane.
    for (const [name, s] of this.spacecraft) {
      for (const vp of this.viewports.values()) {
        const clone = vp.lines.get(name);
        if (!clone) continue;
        clone.line.visible = s.visible && this.options.trail !== 'off';
        if (this.options.trail === 'full') clone.line.geometry.instanceCount = clone.segCount;
      }
    }
    for (const vp of this.viewports.values()) {
      if (vp.labelLayer) vp.labelLayer.style.display = this.options.labels ? '' : 'none';
    }
  }

  _focusPosition(t, out) {
    return this._focusPositionFor(this.focus, t, out);
  }

  /** M26.3: `_focusPosition`'s own logic, generalized to any focus name -- extracted so
   * per-viewport code (`_updateViewport` below) can compute a DIFFERENT viewport's own
   * `focus`'s absolute position without touching `this.focus` (the legacy primary
   * camera's focus). `_focusPosition(t, out)` above is unchanged in behaviour, now just a
   * one-line call into this with `this.focus`. */
  _focusPositionFor(name, t, out) {
    if (name && this.bodies.has(name)) return this.bodies.get(name).interp.position(t, out).multiplyScalar(SCALE);
    if (name && this.spacecraft.has(name)) return this.spacecraft.get(name).interp.at(t, out).multiplyScalar(SCALE);
    return out.set(0, 0, 0);
  }

  // ------------------------------------------------------------------ per frame
  update(t) {
    this._lastT = t;
    const cam = this.camera;
    // Structural completeness: FrameGraph.update() drives any frame with a declared
    // origin track (interp.js's TrajectoryInterp, Hermite w/ velocity). `root` has
    // none -- its motion is the floating-origin rebase policy below, not a physical
    // trajectory -- so this is currently a no-op, kept here so a future frame with a
    // real origin track (once the CDM sends a FrameDefinition list, see VIEWER.md's
    // escalation) is driven correctly without another wiring pass.
    this.frameGraph.update(t, SCALE);

    // Entities-frame precision, regardless of which frame the *camera* is parented
    // in: keep the floating origin rebased near the focus target's absolute position,
    // so trajectory vertex buffers (always rendered under this._originFrameId) stay
    // sub-metre/centimetre precise even while viewing from a different frame (e.g.
    // RIC) via setViewFrame(). fp is *absolute*.
    const fp = this._focusPosition(t, this._tmp);
    this._maybeRebaseOrigin(fp);

    if (this._cameraFrameId === this._originFrameId) {
      // follow focus target -- fp is *absolute*; camera.position/controls.target are
      // *local* (origin-relative). A delta between two absolute positions is identical
      // to the delta between their local representations as long as the origin doesn't
      // change in between (origin.js's Vec3 subtraction is linear), which is exactly
      // what _rebaseOriginTo's own camera/target compensation guarantees across a
      // rebase -- so this delta-follow logic needs no origin-awareness of its own.
      if (!fp.equals(this._focusPrev)) {
        const delta = new THREE.Vector3().subVectors(fp, this._focusPrev);
        cam.position.add(delta);
        const local = this.floatingOrigin.toRenderSpace(this._originFrameId, fp);
        this.controls.target.set(local.x, local.y, local.z);
        this._focusPrev.copy(fp);
      }
      this.controls.update();
    } else {
      // Camera parented to a different frame (e.g. RIC, via setViewFrame): that
      // frame's own Group already tracks its origin entity's absolute motion each
      // tick (frameGraph.update() above); only the orbit target -- which entity the
      // camera looks at -- needs re-aiming here, in that frame's *local* space (see
      // setViewFrame's docstring for why this stays precise without a per-frame
      // floating-origin rebase).
      this._focusPrev.copy(fp); // stay in sync so switching back doesn't jump
      const frameObj = this.frameGraph.frame(this._cameraFrameId).object3D;
      let worldPos = this.focus ? this._focusWorldPosition(this.focus, this._tmpAbs) : null;
      if (!worldPos) worldPos = frameObj.getWorldPosition(this._tmpAbs); // no focus: look at the frame's own origin
      const local = frameObj.worldToLocal(worldPos.clone());
      const delta = new THREE.Vector3().subVectors(local, this.controls.target);
      cam.position.add(delta);
      this.controls.target.copy(local);
      this.controls.update();
      // See setViewFrame's docstring: OrbitControls.update()'s internal
      // `object.lookAt(this.target)` needs a *world*-space target, but `this.target`
      // here is `local` (correct for its own position/offset math) -- the error is
      // negligible for the entities frame but not here, so it is corrected explicitly
      // every tick, not only on the initial setViewFrame() call.
      this.camera.lookAt(worldPos);
    }

    // bodies
    const h0 = this._referenceCanvasHeight();
    const pxPerUnitAt = (d) => h0 / (2 * d * Math.tan(THREE.MathUtils.degToRad(cam.fov / 2)));
    for (const b of this.bodies.values()) {
      b.interp.position(t, this._tmpAbs).multiplyScalar(SCALE);
      this._toLocal(this._tmpAbs, b.mesh.position);
      b.interp.orientation(t, b.mesh.quaternion);
      if (b.data.name === 'Sun') this._sunDir.copy(b.mesh.position);
      // sub-pixel disc -> draw a 3 px dot instead
      const d = b.mesh.position.distanceTo(cam.position);
      const discPx = b.data.radius * SCALE * pxPerUnitAt(d);
      const useDot = b.visible && discPx < 1.5;
      b.dot.visible = useDot;
      if (useDot) {
        b.dot.position.copy(b.mesh.position);
        b.dot.scale.setScalar(Math.max(1.5 / pxPerUnitAt(d), 1e-6));
      }
    }
    // M15.4/M16.4: globe tiles + 3D Tiles overlay, both no-ops when neither is
    // enabled. The body-fixed frame sync must run after the bodies loop above (it
    // copies b.mesh's just-updated local transform) and before the overlay's own
    // update() (which reads camera position through that same frame's world matrix).
    if (this._tilesOverlayBodyName) this._syncBodyFixedFrame(this._tilesOverlayBodyName);
    if (this.globeLayer) this._syncGlobeLayer(cam);
    if (this.tilesOverlay) this.tilesOverlay.update();
    // lighting from the Sun (direction only; the small origin shift folded into
    // b.mesh.position above is negligible next to interplanetary distance, so the
    // normalized direction is unaffected)
    if (this._sunDir.lengthSq() > 0) {
      this.sun.position.copy(this._sunDir).normalize().multiplyScalar(this.fitRadius * 10);
      this.sun.target.position.set(0, 0, 0);
    }
    // spacecraft markers, constant pixel size
    const h = this._referenceCanvasHeight();
    const pxScale = 2 * Math.tan(THREE.MathUtils.degToRad(cam.fov / 2)) / h;
    for (const s of this.spacecraft.values()) {
      s.interp.at(t, this._tmpAbs).multiplyScalar(SCALE);
      this._toLocal(this._tmpAbs, s.marker.position);
      const d = this._markerReferenceDistance(s.marker.getWorldPosition(this._tmpMarkerWorld || (this._tmpMarkerWorld = new THREE.Vector3())));
      s.marker.scale.setScalar(Math.max(d * pxScale * MARKER_PX * 0.5, 1e-6));
      if (this.options.trail === 'past' && s.poly.times.length > 1) {
        const k = findSegment(t, s.poly.times);
        s.line.geometry.instanceCount = t >= s.poly.times[s.poly.times.length - 1] ? s.segCount : Math.max(0, k);
      }
      const inSpan = t >= s.interp.t0 - 1e-9 && t <= s.interp.t1 + 1e-9;
      s.marker.material.opacity = inSpan ? 1 : 0.35;
      s.marker.material.transparent = !inSpan;
    }
    // sensor footprints: jump to the nearest recorded sample (no interpolation
    // contract between rings -- see the Footprint docstring and setScenario() above)
    for (const f of this.footprints.values()) {
      this._updateFootprint(f, t);
    }
    for (const e of this.events) {
      const d = this._markerReferenceDistance(e.mesh.getWorldPosition(this._tmpMarkerWorld || (this._tmpMarkerWorld = new THREE.Vector3())));
      e.mesh.scale.setScalar(Math.max(d * pxScale * EVENT_PX * 0.5, 1e-6));
    }
    this.renderer.render(this.scene, cam);
    if (this.options.labels) this._updateLabels();
    // M26.3: every extra viewport, driven by this SAME tick's `t` -- see _updateViewport's
    // own docstring for why this is the "one clock ... many cameras" mechanism made
    // concrete (there is no per-viewport clock to get out of sync with this one).
    for (const vp of this.viewports.values()) this._updateViewport(vp, t);
  }

  _updateLabels() {
    const w = this.canvas.clientWidth, h = this.canvas.clientHeight;
    const v = this._tmp;
    // `mesh.matrixWorld` (not `mesh.position`, which is local/origin-relative) is what
    // Vector3.project() needs -- it's already current at this point since
    // WebGLRenderer.render() (just called above) updates the whole scene graph's
    // world matrices, so this reads it rather than recomputing (Object3D's own
    // getWorldPosition() would call updateWorldMatrix() again, redundantly).
    const place = (el, mesh, visible) => {
      if (!visible) { el.style.display = 'none'; return; }
      v.setFromMatrixPosition(mesh.matrixWorld).project(this.camera);
      if (v.z > 1 || v.z < -1) { el.style.display = 'none'; return; }
      el.style.display = '';
      el.style.left = ((v.x + 1) / 2 * w) + 'px';
      el.style.top = ((1 - v.y) / 2 * h) + 'px';
    };
    for (const b of this.bodies.values()) {
      // offset label beyond the visible disc
      const d = b.mesh.position.distanceTo(this.camera.position);
      const r = b.data.radius * SCALE;
      place(b.label, b.mesh, b.visible && d > r * 1.05);
    }
    for (const s of this.spacecraft.values()) place(s.label, s.marker, s.visible);
    for (const e of this.events) place(e.label, e.mesh, e.mesh.visible);
  }
}

// ---------------------------------------------------------------------- helpers

/**
 * The whole-scenario camera-framing radius `setScenario()` stores as `this.fitRadius`
 * (M20.2, question 134): the largest of (a) 3x the equatorial radius of whichever
 * `bodies` entry is marked `central` (scene units, `SCALE`-converted from km -- the
 * same padding `makeBodyMesh()` needs none of, since the mesh itself is scaled to the
 * *unpadded* radius) and (b) the farthest any `spacecraft` trajectory sample gets
 * from the frame's own absolute origin, floored at `1e-3` scene units so an empty/
 * degenerate scenario never yields a zero or negative radius.
 *
 * Exported (not inlined in `setScenario()`) specifically so
 * `tests/test_viewer_globe.py`'s headless harness can call this *exact* function
 * against a real ingested-CDM-run scenario JSON (`web/js/verify_cdm_run.mjs`) without
 * constructing a full `Viewer` (WebGL-only -- see `trajectoryRenderPositions()`'s own
 * docstring for the identical constraint). `fit()` then places the camera at
 * `fitRadius * 2.4` scene units out (see `_fitOrigin()`) -- always >= 3 body radii
 * from the central body's own centre when one is present, i.e. never inside it,
 * *provided* this function actually included that body's term. Question 134's root
 * cause was not here (`bodies_from_frames`'s `central` flag and this loop's own
 * formula are both correct on the CDM ingest path today -- verified directly, both by
 * reading `altavista/cdm.py:474` and by publishing the real `tests/fixtures/
 * demo_two_instance.runproducts.bin` fixture through a live server and inspecting the
 * resulting scenario JSON's `bodies[0].central`); it is in `setViewFrame()`'s
 * *separate* default-radius formula for a non-origin frame, which used a flat
 * RPO-scale constant with no central-body awareness at all until this task fixed it
 * (`defaultFrameViewRadius()` below) -- this function is unchanged in behaviour from
 * before M20.2, extracted here only so it is independently testable.
 *
 * @param {Array<{central?: boolean, radius: number}>} bodies `sc.bodies` (radius km).
 * @param {Array<{t: number[], pos: number[]}>} spacecraft `sc.spacecraft` (pos km,
 *   flat [x0,y0,z0,...], absolute -- the same shape `TrajectoryInterp` consumes).
 * @returns {number} scene units, always >= 1e-3.
 */
export function computeFitRadius(bodies, spacecraft) {
  let maxR = 0;
  for (const b of bodies || []) {
    if (b.central) maxR = Math.max(maxR, b.radius * SCALE * 3);
  }
  for (const s of spacecraft || []) {
    const interp = new TrajectoryInterp(s);
    const poly = interp.polyline();
    for (let i = 0; i < poly.points.length; i += 3) {
      maxR = Math.max(maxR, Math.hypot(poly.points[i] * SCALE, poly.points[i + 1] * SCALE, poly.points[i + 2] * SCALE));
    }
  }
  return Math.max(maxR, 1e-3);
}

/**
 * `setViewFrame()`'s default viewing distance when re-centring on a frame's own
 * origin with no focus given (`sep === 0`) -- M20.2's fix for question 134/E-27.
 *
 * `bodyRadiusKm` is the radius (km) of the body the target frame is centred on, when
 * it names one (a body-axes CDM frame -- EarthICRF/EarthBodyFixed/EarthMJ2000Eq,
 * M18.1's mandatory central-body frame set, always offered for a CDM-ingested run --
 * looked up by `setViewFrame()` from `this.bodies.get(frameDef.body)`), or
 * `undefined` for a frame with no body of its own (an entity-relative RIC/VNB/VVLH
 * frame, whose origin *is* a spacecraft, RPO scale).
 *
 * With a body radius: `radius*SCALE*3`, exactly `computeFitRadius()`'s own
 * central-body padding, so a focus-less view of e.g. EarthICRF frames the whole body
 * the same conservative amount `fit()` would. Without one: the original `1e-4` scene
 * units (~100 m), a reasonable zoomed-in RPO default -- unchanged from before M20.2.
 *
 * Before this fix, `setViewFrame()` used the `1e-4` RPO default unconditionally,
 * including for a body-axes frame: switching to EarthICRF with no focus put the
 * camera 100 m from Earth's centre -- 1/64000th of Earth's own ~6378 km radius,
 * literally inside the globe. Only reachable on the CDM ingest path (M18.1 is what
 * offers a body-axes frame other than the scenario's own to switch into at all; a
 * Python scenario typically has just the one frame in the picker), matching question
 * 134's own "this is specific to the CDM-run ingest path" / "the Python scenario path
 * fits correctly" observations exactly.
 *
 * @param {number|undefined} bodyRadiusKm
 * @returns {number} scene units.
 */
export function defaultFrameViewRadius(bodyRadiusKm) {
  return typeof bodyRadiusKm === 'number' ? bodyRadiusKm * SCALE * 3 : 1e-4;
}

/**
 * The frame id `fit()` ("Reset view") targets -- M20.2, question 134/E-27's "Reset
 * view must keep the currently selected frame" half: whatever the camera is
 * CURRENTLY parented in (`cameraFrameId`), never forced back to the entities frame
 * (`originFrameId`) the way the pre-M20.2 `fit()` always did
 * (`this._cameraFrameId = this._originFrameId` unconditionally, combined with
 * `app.js`'s Reset handler forcing the frame picker back to its first option --
 * together, no way to land in e.g. ICRF with the scene framed). `fit()` calls this
 * function for its own targeting decision (not a parallel, only-in-a-comment
 * description of it), so this is the actual, load-bearing implementation, exported
 * so a headless test can assert the contract directly: a wrong implementation that
 * ignores `cameraFrameId` and always returns `originFrameId` reproduces the exact
 * pre-M20.2 bug.
 * @param {string} cameraFrameId
 * @param {string} originFrameId
 * @returns {string}
 */
export function resetTargetFrameId(cameraFrameId, originFrameId) {
  return cameraFrameId;
}

/**
 * Build the render-space (small, f32-precision) vertex buffer for one trajectory's
 * densified polyline, from its raw f64-km samples -- exactly what `setScenario()` and
 * `_refreshOriginRelativeGeometry()` above pass straight to `LineGeometry.
 * setPositions()`. Exported (not a private method) specifically so
 * `web/js/scene_jitter_harness.mjs` (see tests/test_viewer_jitter.py) can import and
 * call this *exact* function headlessly under `node`, without constructing a full
 * `Viewer` (which needs a real WebGL context via `THREE.WebGLRenderer` -- the only
 * thing standing between this function and a real browser; nothing in this function
 * touches WebGL). This is the one place that turns f64 absolute km samples into f32
 * GPU-ready render-space coordinates for a trajectory line; there is no second copy.
 *
 * @param {{points: Float64Array}} poly `TrajectoryInterp.polyline()`'s return value
 *   (flat f64 [x0,y0,z0,x1,y1,z1,...] km) -- or anything shaped like it.
 * @param {import('./origin.js').FloatingOrigin} floatingOrigin
 * @param {string} frameId
 * @returns {Float32Array} ready for `LineGeometry.setPositions()`
 */
/**
 * Pure "old origin vs. new origin" shift arithmetic shared by `_rebaseOriginTo` (the
 * legacy primary camera's own rebase) and `_rebaseViewportOriginTo` (M26.3, every extra
 * viewport's own rebase) -- one implementation, not two that could silently diverge.
 * Both origins are `{enabled ? value : {0,0,0}}` in effect (an origin whose floating-
 * origin toggle is off contributes no shift, matching origin.js's own
 * `isEnabledForFrame`-gated behaviour in `toRenderSpace`/`toRenderSpaceArray`) -- this
 * function only does the "effective old minus effective new" subtraction and reports
 * whether it actually changed anything, so a caller can skip a geometry rebuild when
 * nothing moved (unless it wants to force one, e.g. `fit()`).
 *
 * Exported (not inlined) specifically so `web/js/viewport_check.mjs` can assert on this
 * exact arithmetic headlessly -- see that harness for the direct test, and for what
 * calling this with the same `FloatingOrigin` instance for two different viewports'
 * "old"/"new" would do (the very bug this module's per-viewport design avoids by giving
 * every viewport its own `FloatingOrigin` instance in the first place).
 *
 * @param {{x:number,y:number,z:number}} oldOrigin
 * @param {boolean} oldEnabled
 * @param {{x:number,y:number,z:number}} newOrigin
 * @param {boolean} newEnabled
 * @returns {{dx:number,dy:number,dz:number,newShift:{x:number,y:number,z:number},changed:boolean}}
 */
export function computeOriginShift(oldOrigin, oldEnabled, newOrigin, newEnabled) {
  const oldShift = oldEnabled ? oldOrigin : { x: 0, y: 0, z: 0 };
  const newShift = newEnabled ? newOrigin : { x: 0, y: 0, z: 0 };
  return {
    dx: oldShift.x - newShift.x,
    dy: oldShift.y - newShift.y,
    dz: oldShift.z - newShift.z,
    newShift,
    changed: oldShift.x !== newShift.x || oldShift.y !== newShift.y || oldShift.z !== newShift.z,
  };
}

export function trajectoryRenderPositions(poly, floatingOrigin, frameId) {
  const pts = poly.points;
  if (pts.length < 6) return new Float32Array([0, 0, 0, 0, 0, 0]);
  const scaledAbs = new Float64Array(pts.length);
  for (let i = 0; i < pts.length; i++) scaledAbs[i] = pts[i] * SCALE;
  return floatingOrigin.toRenderSpaceArray(frameId, scaledAbs);
}

/**
 * A footprint ring (`altavista/model.py`'s `Footprint.ring[i]`: a flat `[x0,y0,z0,...]`
 * km array, absolute, in the scenario frame -- already the same frame/units as
 * spacecraft `pos`) -> render-space `Float32Array` for `LineGeometry.setPositions`,
 * exactly the same scale + f64 origin-subtract + single `Math.fround` pipeline
 * `trajectoryRenderPositions` uses above (never a separately-rounded path). The first
 * point is appended again at the end to close the loop -- `Line2` draws an open
 * polyline, and the ring has no other "this is a loop" marker on the wire.
 */
export function footprintRenderPositions(ringFlat, floatingOrigin, frameId) {
  if (!ringFlat || ringFlat.length < 9) return new Float32Array([0, 0, 0, 0, 0, 0]);
  const n = ringFlat.length;
  const scaledAbs = new Float64Array(n + 3);
  for (let i = 0; i < n; i++) scaledAbs[i] = ringFlat[i] * SCALE;
  scaledAbs[n] = scaledAbs[0]; scaledAbs[n + 1] = scaledAbs[1]; scaledAbs[n + 2] = scaledAbs[2];
  return floatingOrigin.toRenderSpaceArray(frameId, scaledAbs);
}

/**
 * M21.2 (question 140, decided by the lead): "an event whose instance has no position
 * class appears on the timeline only, never as a 3D label." Whether wire-shaped event
 * `ev` (`altavista/model.py`'s `Event`, e.g. `{name, t, spacecraft, detail, type}`) gets a
 * 3D marker/label in the scene: true only when `ev.spacecraft` names an entry actually
 * present in `spacecraftNames` (`sc.spacecraft`'s own `name`s on the wire, or this
 * class's `this.spacecraft` Map keys in a live Viewer).
 *
 * This is never a name/id heuristic (no check for `"_ctrl"`, `"demo_ctrl"`, or any
 * other literal) -- it is exactly the membership test `setScenario()` below already
 * needs to look up the event's own rendered spacecraft entry, extracted here so it is
 * callable, and testable headlessly (`web/js/verify_cdm_run.mjs`'s `event3dLabelFacts`,
 * `tests/test_cdm_run.py`), without a live WebGL `Viewer` (see `trajectoryRenderPositions`'s
 * own module-level neighbours above for the same "no THREE.WebGLRenderer under plain
 * node" constraint).
 *
 * Why membership in `spacecraftNames` *is* "this instance has a position class", not a
 * coincidental proxy for it: `sc.spacecraft` is built server-side
 * (`altavista/server.py`'s `POST /api/cdm/run` handler) by skipping every `None`
 * `altavista.cdm.cdm_trajectory_to_viewer_json` returns -- and that function returns
 * `None` precisely when the instance's declared state space fails
 * `altavista.cdm.has_position_class` (question 133/M20.1). An instance excluded on those
 * grounds (e.g. a native controller's `native.controller.*` state space, or -- M21.3 --
 * one with no trajectory in `RunProducts.trajectories` at all) is therefore never a key
 * in `spacecraftNames`, so this function needs no independent state-space lookup of its
 * own to give the right answer -- it reads the *consequence* of that upstream filter.
 *
 * An event with no `spacecraft` at all (e.g. a scenario-wide `dropped_in_flight_messages`
 * lifecycle event) is likewise never renderable in 3D. The event itself is untouched
 * either way -- `web/js/app.js`'s `buildLists`/`buildTicks` read `sc.events` directly
 * and always list every event on the timeline; this function only ever gates the 3D
 * mesh/label half (`setScenario()`'s `mesh.visible` and `_updateLabels()`'s existing
 * `place(e.label, e.mesh, e.mesh.visible)` gate below).
 */
export function eventHasRenderedInstance(ev, spacecraftNames) {
  return !!(ev && ev.spacecraft && spacecraftNames.includes(ev.spacecraft));
}

function makeBodyMesh(b, loader) {
  const geometry = new THREE.SphereGeometry(1, 96, 64);
  geometry.rotateX(Math.PI / 2);   // poles on +z, texture centre (lon 0) on +x
  const color = new THREE.Color(b.color || '#888888');
  let material;
  if (b.name === 'Sun') {
    material = new THREE.MeshBasicMaterial({ color: 0xffffff });
  } else {
    material = new THREE.MeshPhongMaterial({ color: 0xffffff, shininess: 6, specular: 0x111111 });
  }
  if (b.texture) {
    loader.load(b.texture, (tex) => {
      tex.colorSpace = THREE.SRGBColorSpace;
      tex.anisotropy = 8;
      material.map = tex;
      material.needsUpdate = true;
    }, undefined, () => { material.color = color; });
  } else {
    material.color = color;
  }
  const mesh = new THREE.Mesh(geometry, material);
  const r = b.radius * SCALE;
  mesh.scale.set(r, r, r * (1 - (b.flattening || 0)));
  return mesh;
}

function makeStars(count = 6000, radius = 5e7) {
  const pos = new Float32Array(count * 3);
  for (let i = 0; i < count; i++) {
    const u = Math.random() * 2 - 1, phi = Math.random() * Math.PI * 2;
    const s = Math.sqrt(1 - u * u);
    pos[3 * i] = radius * s * Math.cos(phi);
    pos[3 * i + 1] = radius * s * Math.sin(phi);
    pos[3 * i + 2] = radius * u;
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute('position', new THREE.BufferAttribute(pos, 3));
  const m = new THREE.PointsMaterial({ color: 0xffffff, size: 1.5, sizeAttenuation: false, transparent: true, opacity: 0.8 });
  const pts = new THREE.Points(g, m);
  pts.frustumCulled = false;
  return pts;
}

function disposeMesh(mesh) {
  if (mesh.geometry) mesh.geometry.dispose();
  if (mesh.material) {
    if (mesh.material.map) mesh.material.map.dispose();
    mesh.material.dispose();
  }
}

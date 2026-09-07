# Viewer: frame graph and floating origin (M2.3 built, M3.1 wired in, M4.1 multi-frame, M5.2 rotating frames, M7.1 declared state spaces, M15.4 tiled globe + 3D Tiles overlay, M16.4 3D Tiles overlay geo-referencing + reused streaming budget, M17.1 CDM ingest frame threading)

Two additive ES modules under `web/js/` (`origin.js`, `frames.js`, built in M2.3), now
wired into the live viewer (`scene.js`/`app.js`/`index.html`, M3.1), plus a headless CI
test (`tests/test_viewer_jitter.py`) that proves both the standalone modules and the
live scene-building code path against the real code, not a description of it. They
implement the first two items of the viewer module list in `docs/architecture.md` §4
"Presentation plane":

> *Frame graph*: a scene node per declared frame, transforms from the frame service,
> the camera parented to any frame (Earth-fixed, inertial, body-centred,
> spacecraft-relative). Switching frames is re-parenting, not re-loading.
>
> *Precision*: double-precision state on the CPU, floating-origin (relative-to-eye)
> rendering on the GPU, logarithmic depth.

and the answer to Q46 in `docs/open-questions.md`:

> floating origin per frame with centimetre stability in RIC and sub-metre elsewhere,
> and the floating-origin mechanism must be configurable so it can be enabled for
> every frame and view, not only the focused one.

## The problem these fix

Before M3.1, `web/js/scene.js` kept exactly one scene origin at all times: everything
(Earth, Moon, Mars, every spacecraft) was positioned in one absolute coordinate frame
in scene units (`SCALE = 1e-3`, 1 unit = 1000 km), and a spacecraft's trajectory line
was written straight into a `Float32Array` (`LineGeometry.setPositions(scaled)`). A
32-bit float carries about 7.2 decimal digits; once a coordinate's absolute magnitude
reaches a few hundred scene units (Moon distance is ~384 scene units, Mars distance
~2.3x10^5), the quantization step of storing it as float32 is itself metres to
hundreds of metres -- independent of anything the GPU does afterwards.
`tests/test_viewer_jitter.py` measures this directly (table below), both for
`origin.js`'s arithmetic in isolation and for the live `scene.js` code path.

## 1. Frame graph (`web/js/frames.js`)

- `FrameNode` wraps one Three.js `Group` per `FrameDefinition`
  (`proto/altavista/v1/core.proto`). `FrameGraph` holds a tree of these, parented
  according to each definition's `parentId` (a viewer-side field -- the CDM message
  itself has no parent pointer; the scene service is expected to supply one alongside
  each `FrameDefinition`, since axes/origin already implies a natural parent, e.g. an
  `AXES_KIND_RIC` frame's parent is its `reference_body`'s inertial frame).
- A frame whose origin moves (an entity-relative RIC/VNB/VVLH frame) gets its position
  from a sampled track via `FrameNode.setOriginTrack`, which hands the track straight
  to `interp.js`'s existing `TrajectoryInterp` -- the same Hermite-with-velocity
  interpolation a spacecraft's own trajectory uses (the CDM's declared interpolation
  contract, `docs/architecture.md` §2). Frame motion and trajectory motion are never
  interpolated two different ways.
- `FrameGraph.reparent(object3D, frameId)` is `Object3D.attach`: it re-links the
  object into the target frame's `Group` and recomputes its local transform so its
  world position/orientation are unchanged (nothing visually jumps), touching only
  `parent`/`children`/`position`/`quaternion`/`scale` on the object being moved. It
  never touches `object3D.geometry`, `.material`, or any child. **This is what makes
  "switching frames is re-parenting, not re-loading" structurally true**: there is no
  code path in `reparent` that can dispose or recreate geometry.

### How this was tested

`web/js/frame_graph_check.mjs` (run headlessly under `node`, no browser/DOM/WebGL
needed -- `frames.js` only touches Three's core `Object3D` graph) builds one
trajectory's `BufferGeometry`/material/vertex buffer exactly once, switches the camera
across three frames -- Earth-inertial, Moon-centred, and a spacecraft-relative RIC
frame -- several times in sequence, and after every switch checks `===` identity
(object reference, not value equality) of the geometry, the material, the vertex
`BufferAttribute`, and its backing `Float32Array`. `tests/test_viewer_jitter.py`'s
`test_frame_switch_is_reparent_not_reload` runs that script and asserts every one of
its 28 checks passed.

## 2. Floating origin (`web/js/origin.js`)

Framework-free (no Three.js import) on purpose: it is the one place the arithmetic
lives, so the shipped renderer and the CI test exercise the exact same code (see
[Test/module relationship](#testmodule-relationship-not-a-port)).

- `FloatingOrigin` keeps one f64 origin per frame id (`Map<string, {x,y,z}>`, plain
  JS numbers -- a JS `number` *is* IEEE-754 double precision, so nothing here ever
  round-trips the authoritative position through a `Float32Array` before subtracting).
- `setOrigin(frameId, x, y, z)` re-bases a frame's origin. The *policy* for when to
  call it lives in `scene.js` (see §3 below) -- `origin.js` itself only stores the
  value.
- `toRenderSpace(frameId, pos)` / `toRenderSpaceArray(frameId, pointsXYZ)` compute
  `pos - origin` in f64, then apply `Math.fround` exactly once, last.
- **Configurable per frame and globally**: `setEnabledForFrame(frameId, enabled)`
  overrides one frame; `globalEnabled` is the default every frame without an override
  falls back to. Disabling a frame's floating origin makes `toRenderSpace` use
  `{0,0,0}` as the origin, i.e. `Math.fround(pos)` -- **exactly** the pre-M3.1
  `scene.js` behaviour, not a different approximation.
- `toRenderSpaceNoOrigin(pos, eye)` is the *baseline* the module replaces, kept here
  specifically so the jitter test can prove the fix is necessary.
- Logarithmic depth (`logarithmicDepthBuffer: true`) is enabled in `scene.js`'s
  `WebGLRenderer` construction; floating origin and log-depth address different
  failure modes (vertex-position quantization vs. depth-buffer precision) and are used
  together.

## 3. Live wiring (`web/js/scene.js`, `app.js`, `index.html`) -- M3.1

**The scene graph is the frame graph.** `Viewer` owns one `FrameGraph`
(`this.frameGraph`) and one `FloatingOrigin` (`this.floatingOrigin`), keyed by
`this._originFrameId` (`'root'`). Every body, spacecraft marker/line, event marker and
the camera itself are parented under `frameGraph.frame('root').object3D` via
`frameGraph.reparent(...)`, never added to `this.scene` directly. `clear()` (called at
the top of every `setScenario()`) discards the previous scenario's `FrameGraph` wholesale
and builds a fresh one, the same way `this.bodies`/`this.spacecraft` are rebuilt from
scratch rather than patched.

**As of M4.1, the scenario JSON's additive `frames` list drives a real multi-node
graph** -- see [§4 below](#4-multi-frame-graph-and-per-frame-viewing-m41) for the wire
shape and `Viewer._buildFrameGraph()`. Bodies, spacecraft, events and (by default) the
camera are parented under the *entities* frame (`this._originFrameId`, the scenario's
own declared frame, `sc.frame.name`); the camera can be reparented to any other node in
the graph (e.g. an entity-relative RIC frame) via `Viewer.setViewFrame()`.
`frame_graph_check.mjs`'s identity checks (now 38, up from 28) additionally exercise
this from a wire-shaped, deliberately out-of-order `frames` payload.

### Floating-origin rebase policy

`origin.js` deliberately leaves "when to call `setOrigin`" to the caller. `scene.js`
supplies that policy:

- **On by default**, and switchable both **globally** (the "Floating origin" checkbox
  in the sidebar, `Viewer.setFloatingOriginEnabled(enabled)` →
  `floatingOrigin.globalEnabled`) and **per frame**
  (`viewer.floatingOrigin.setEnabledForFrame(frameId, enabled)` -- exercised by
  `tests/test_viewer_jitter.py` against `origin.js` directly; with only one frame in
  today's data, per-frame and global are observably identical live, but the API is
  real, not stubbed).
- **Auto-rebase on drift**: `Viewer._maybeRebaseOrigin`, called once per `update(t)`
  tick, rebases the origin to the current focus target's absolute position whenever
  drift exceeds `ORIGIN_REBASE_DRIFT` (0.02 scene units, ~20 km). At float32's ~7.2
  decimal digits, a coordinate of magnitude *V* scene units quantizes to roughly
  *V* × 0.119 m, so a 20 km drift tolerance bounds the worst case right before a
  rebase at ~2.4 mm -- comfortably under both the sub-metre and centimetre bounds,
  with margin. This closes the gap the M2.3 report left open ("no automatic origin
  re-basing policy is implemented").
- **Rebase mechanics** (`Viewer._rebaseOriginTo`): compensates `camera.position` and
  `controls.target` (both expressed in the frame's local, origin-relative space) by
  the exact origin delta so their *world* position is unchanged, moves the frame's own
  `Group.position` to carry the new origin (f64, exact -- `Object3D.position` is a
  plain `THREE.Vector3`, never a `Float32Array`), then rebuilds the geometry that was
  built once from static data rather than recomputed every frame (trajectory lines,
  event markers -- see `_refreshOriginRelativeGeometry`). Skipped (no-op) when neither
  the origin value nor the enabled state actually changed, unless forced.
- Camera parenting: the camera is `frameGraph.reparent`-ed into `'root'` in the
  constructor (and again on every `setScenario()`, since `clear()` rebuilds the frame
  graph). `WebGLRenderer.render()` updates the whole scene graph's world matrices each
  frame regardless of camera parentage -- confirmed against the vendored
  `three.module.js`/`three.core.js` source (`Camera.updateMatrixWorld` overrides the
  base implementation to also refresh `matrixWorldInverse`, and fires as part of the
  normal scene traversal once the camera is a descendant of `scene`).

### Trajectory geometry: f64 CPU → f32 offsets

This is the actual fix for Mars-distance and RPO shimmering, item 3 of the M3.1 brief:

1. **`interp.js`'s `TrajectoryInterp.polyline()` now returns `Float64Array` points**,
   not `Float32Array`. This was a real, separate bug: even with a perfect
   origin-subtraction step downstream, `polyline()` was rounding its Hermite-evaluated
   samples to float32 *before* scene.js ever got them, silently defeating any later
   fix. Densification/subdivision logic is unchanged -- only the output typed array.
2. **`scene.js` exports `trajectoryRenderPositions(poly, floatingOrigin, frameId)`**
   (near the bottom of the file, alongside `makeBodyMesh`/`makeStars`): scales
   `poly.points` (f64 km) to scene units (still f64), then calls
   `floatingOrigin.toRenderSpaceArray(frameId, scaledAbs)` -- the origin subtraction
   and the single `Math.fround` happen in exactly that one place in `origin.js`, never
   reimplemented. `Viewer.setScenario()` and `Viewer._refreshOriginRelativeGeometry()`
   (called on every origin rebase) both call this *exact* exported function; there is
   no second copy.
3. Where the origin subtraction happens, concretely: `trajectoryRenderPositions()` →
   `FloatingOrigin.toRenderSpaceArray()` in `origin.js`. Nowhere else.

Body/spacecraft/event marker positions don't need the same "rebuild on rebase"
treatment: they're recomputed fresh from absolute f64 interpolated data every
`update(t)` tick via `Viewer._toLocal()` (a thin wrapper around
`floatingOrigin.toRenderSpace`), so they never go stale between rebases. Only geometry
built once from static data (trajectory line vertex buffers, event marker positions
set at scenario-load time) needs an explicit refresh, which the rebase helper performs.

### Jitter test: real path, not a parallel one

`web/js/scene_jitter_harness.mjs` (new in M3.1) drives the pipeline one level above
`origin.js`'s own isolated arithmetic test: it builds a real `{t, pos, vel}` track,
runs it through `interp.js`'s real `TrajectoryInterp.polyline()`, and feeds the result
into `scene.js`'s real, exported `trajectoryRenderPositions()` -- the exact function
`setScenario()`/`_refreshOriginRelativeGeometry()` call. **This is the live
scene-building code path, not a reimplementation of it.** The one thing it doesn't
exercise is `THREE.WebGLRenderer` construction (needs a real GPU context/canvas,
unavailable under plain `node`); nothing in `TrajectoryInterp` or
`trajectoryRenderPositions` touches WebGL, so `trajectoryRenderPositions` is exported
as a plain function specifically so this harness can call it without constructing a
full `Viewer`. That gap is closed by the manual browser verification below, not by a
second harness.

## 4. Multi-frame graph and per-frame viewing (M4.1)

M3.1 wired the frame graph and floating origin, but the scene JSON only ever supplied
one `Frame`; M4.1 removes that limitation end to end, per
docs/open-questions.md question 78's decision.

### The `frames` wire key

`altavista/model.py`'s `ScenarioData.to_dict()` gains an **additive** `frames` key: a
list of plain dicts, each the `google.protobuf.json_format.MessageToDict` transcoding
(default settings: camelCase, enums as string names, default-valued fields omitted) of
one `altavista.v1.FrameDefinition`, produced through `altavista.frames.FrameRegistry` by
`altavista/scenario.py`'s `Scenario._build_frames()` (never hand-built). Every entry:

- the scenario's own frame (`sc.frame`), when its axes have an `altavista.v1.AxesKind`
  counterpart (`altavista/cdm.py`'s `frame_definition_for` -- `MJ2000Eq`/`MJ2000Ec`/
  `BodyFixed`/`ICRF` only; `BodyInertial`/`Topocentric` etc. have none and are simply
  omitted, not approximated -- see `_buildFrameGraph`'s fallback below);
- the scenario's central body's `MJ2000Eq` and `BodyFixed` frames (always present,
  regardless of the scenario's own axes) -- **deduped** against the scenario's own
  frame when they coincide (the common case, e.g. `frame="EarthMJ2000Eq"`): one
  registered entry, not two describing the same origin+axes;
- one entry per entity-relative frame declared with `Scenario.frame_ric()` /
  `frame_vnb()` / `frame_vvlh()` (new in M4.1), each with `parentFrameId` filled per
  question 76's rule (the reference body's `MJ2000Eq` frame -- reused/deduped against
  the central-body entries above, not a second copy).

One extra key, **not** part of `FrameDefinition` itself, is added when applicable:
`originTrack` -- `{"t": [...], "pos": [...], "vel": [...]}` (A1MJD / km / km-s, the
same flat layout `Trajectory.to_dict()` uses), present only for an entity-relative
frame, built by `Scenario._origin_track_for()` by reexpressing the origin entity's own
recorded trajectory in the frame's parent frame via GMAT's `CoordinateConverter`. This
is a documented altavista wire-format extension (the CDM message has no origin-motion
field of its own); `web/js/frames.js`'s `FrameNode.setOriginTrack` is what consumes it.

Existing keys are unchanged: `frame`, `spacecraft[].pos/vel`, `bodies[].pos`, etc. all
keep their pre-M4.1 meaning, units and shape --
`tests/test_script_prep.py::test_scenario_data_json_shape` pins `"frames": []` as the
default for a `ScenarioData` that never populates it.

### `web/js/frames.js`: `orderFrameDefsByParent`

`FrameGraph.addFrame()` accepts a definition whose declared parent isn't registered
*yet* (legitimate for a stream delivered in any order) by silently parenting it under
the graph root -- correct for that use case, wrong for a *batch* list (the whole
`frames` array arrives at once) that happens to list a child before its parent. The
new exported `orderFrameDefsByParent(defs)` topologically sorts a definition array
first (parent-before-child), throwing (never silently dropping/misparenting) on a
dangling `parentId` or a parent cycle -- the viewer-side counterparts of
`altavista/frames.py`'s `FrameParentMissingError`/`FrameCycleError`. Exercised by
`frame_graph_check.mjs` against a deliberately out-of-order, protobuf-JSON-shaped
payload (a RIC frame listed *before* its own parent).

### `web/js/scene.js`: the entities render group, and why it exists

`Viewer._buildFrameGraph(sc)` normalizes each wire entry (`parentFrameId` ->
`parentId`, `originTrack` passed through), orders them, and calls `addFrame` +
`setOriginTrack` for each. `this._originFrameId` (the *entities* frame -- bodies,
spacecraft, events) is always `sc.frame.name`; when `frames` omits it (the
axes-unmappable case above), a synthetic root node is added with a logged
`console.warn` -- never a silent fallback.

Bodies/spacecraft/events, and the camera by default, are parented under
**`this._entitiesGroup`**, an intermediate `THREE.Group` that is itself a child of the
entities *frame node* -- **not directly under the frame node**. This is a real
correctness fix, not a stylistic choice: `_rebaseOriginTo` (the floating-origin
mechanism, unchanged in spirit from M3.1) needs to move *something*'s position to
absorb the origin shift, but a true `FrameDefinition` node must stay an honest,
always-zero anchor -- otherwise an entity-relative *child* frame (e.g. RIC) parented
under it would have its own absolute `originTrack` position silently double-counted
against the entities frame's current rebase shift, corrupting where that child frame
(and anything parented under it, e.g. the camera) actually renders. `FrameGraph.
frameOf()`'s own docstring already anticipated "intermediate non-frame groups" for
exactly this kind of case.

### `Viewer.setViewFrame(frameId, focusName)`: the camera, in any frame

This is the mechanism behind "focus the chaser in the target's RIC frame"
(`examples/05_rpo_ric.py`): the camera can be reparented into *any* node in the frame
graph, independent of where the entities themselves are rendered.

- When `frameId` is the entities frame, this reduces to the pre-M4.1 `setFocus()`
  behaviour unchanged (camera back under `_entitiesGroup`, whole-scenario zoom clamp
  restored).
- Otherwise, the camera is attached directly to the target frame node. No second
  `FloatingOrigin` instance is needed for that frame: its `Group.position` already
  carries its origin entity's full-precision absolute position every tick (via
  `FrameNode.update`, a plain `THREE.Vector3` -- f64, never rounded through a
  `Float32Array`), and Three's CPU-side scene-graph matrices (`Object3D.matrixWorld`,
  `Matrix4.elements`) are plain JS-number (f64) arrays too, so that large absolute
  component composes correctly through the parent chain and cancels to f64 (not f32)
  rounding error against any sibling entity sharing the same ancestor -- which the
  central-body dedup above arranges for the common case. Only the camera's *local*
  offset (an RPO-scale distance) needs to stay small, which `setViewFrame` sizes from
  the focused entity's actual separation (falling back to a fixed heuristic when
  nothing is focused -- see [Integration status](#integration-status)).
  `camera.near`/`far` and `controls.minDistance`/`maxDistance` are also re-derived
  from that same scale here (the entities frame's whole-scenario values would
  otherwise clip everything, or block zooming in at all) and restored on return to the
  entities frame.
- `camera.lookAt(targetWorld)` is called explicitly, every tick, using a properly
  world-space point -- see [Integration status](#integration-status) for why
  `OrbitControls.update()`'s own internal `lookAt` call cannot be trusted here.

`Viewer.update(t)`'s per-tick logic mirrors this split: the floating-origin rebase
(`_maybeRebaseOrigin`) always runs, keyed to the entities frame, regardless of which
frame the camera is in (trajectory precision must not depend on where you're looking
from); a second, separate block re-aims `controls.target`/`camera.lookAt` every tick
only when the camera is in a non-entities frame.

### `Viewer.setFrameOriginEnabled(frameId, enabled)`

The per-frame counterpart of `setFloatingOriginEnabled` (Q46: "switchable ... for
every frame, not only the focused one") -- wired to a checkbox per frame in the
sidebar's "Frames" section. See [Integration status](#integration-status) for why only
the entities frame's checkbox has a visible effect today.

### `Scenario.frame_ric()` / `frame_vnb()` / `frame_vvlh()` (Python)

`altavista/scenario.py` gains these three methods (M4.1): each declares an
entity-relative frame (`sc.frame_ric(target_sat)` -> a frame id string), realized
through `altavista.frames.FrameRegistry` -- never hand-built -- at `build()`/`publish()`
time, and included in the next `to_dict()`'s `frames` list.
`examples/05_rpo_ric.py` is the worked example: a target and a chaser ~30 m apart in
LEO, the target's RIC frame declared, viewable close-up via the "Frames" -> "View
frame" selector plus "Focus" = the chaser.

## Measured jitter (metres)

### `origin.js` arithmetic in isolation (`jitter_harness.mjs`, unchanged from M2.3)

From `tests/test_viewer_jitter.py::test_jitter_report`:

| Scene | With floating origin | Without floating origin | Bound |
|---|---:|---:|---:|
| LEO | 1.01e-6 m | 0.134 m | sub-metre (1 m) |
| Moon distance | 3.24e-7 m | 0.935 m | sub-metre (1 m) |
| Mars distance | 1.15e-5 m | **544.7 m** | sub-metre (1 m) |
| 10 m RPO (RIC) | 2.53e-7 m | **0.0136 m** | centimetre (0.01 m) |

### Live scene-building code path (`scene_jitter_harness.mjs`, new in M3.1)

From `tests/test_viewer_jitter.py::test_scene_trajectory_jitter_report` -- real
`TrajectoryInterp.polyline()` + real `scene.js` `trajectoryRenderPositions()`:

| Scene | With floating origin | Without floating origin | Bound |
|---|---:|---:|---:|
| LEO | 1.01e-6 m | 0.115 m | sub-metre (1 m) |
| Moon distance | 3.24e-7 m | 2.06 m | sub-metre (1 m) |
| Mars distance | 7.77e-6 m | **1877.7 m** | sub-metre (1 m) |
| 10 m RPO (RIC) | 3.39e-6 m | **0.0136 m** | centimetre (0.01 m) |

The two tables are close but not identical -- expected, since they exercise slightly
different real code (a raw vector subtraction in `jitter_harness.mjs` vs. a Hermite
basis-function evaluation through `polyline()`/`segment()` in
`scene_jitter_harness.mjs`); neither is loosened to match the other. Notably, the live
code path's Moon-distance "without" value (2.06 m) exceeds the sub-metre bound where
the isolated-arithmetic table's did not (0.935 m) -- a real, honestly-reported
difference from evaluating through the Hermite basis functions rather than a plain
subtraction, not a discrepancy that was smoothed over.

Method (see `jitter_harness.mjs`/`scene_jitter_harness.mjs` for the exact scene data):
LEO/Moon/Mars each model one moving object -- the origin is rebased to the object's
position (a focus-change event), then the render-space position is evaluated one
render frame (1/60 s) of real orbital motion later, using realistic orbital speeds
(LEO ~7.67 km/s, lunar ~1.02 km/s, Mars heliocentric ~24.07 km/s) and a
position/velocity direction pair that is deliberately *not* axis-aligned (reusing the
exact unit vectors of `altavista/FRAMES.md`'s own worked example state, scaled to each
scene's distance and speed). The RPO scene models two spacecraft 10 m apart at LEO
altitude and measures their rendered *separation* (the RIC/VVLH quantity of interest),
with the origin rebased to the chief's own position.

**Both required failures are real, not asserted-then-loosened**, in both tables: Mars
distance and the RPO scene both exceed their bounds without the floating origin, in
both the isolated-arithmetic and live-scene-path measurements.
`test_no_floating_origin_fails_at_mars_distance` /
`test_no_floating_origin_fails_rpo_centimetre` (isolated) and
`test_scene_trajectory_no_floating_origin_fails_at_mars_distance` /
`test_scene_trajectory_no_floating_origin_fails_rpo_centimetre` (live path) assert
exactly these failures persist -- if any ever started passing, that would mean the
module had stopped mattering and needs investigating, not that the assertion should be
deleted.

## Test/module relationship: not a port

`tests/test_viewer_jitter.py` does not reimplement `origin.js`'s, `frames.js`'s or
`scene.js`'s arithmetic in Python. It shells out to `node` (`subprocess.run`) to
execute three CLI harnesses committed alongside the modules -- `web/js/jitter_harness.
mjs`, `web/js/frame_graph_check.mjs` and `web/js/scene_jitter_harness.mjs` -- which
import the real ES modules directly and print one JSON object of
measurements/checks to stdout; the Python test parses that JSON and asserts on it.
There is exactly one implementation of each piece of arithmetic; the test and the
browser both run it. If `node` is not installed, the test `pytest.skip`s with an
explanation rather than falling back to a Python re-implementation.

`web/node_modules/three/` is a small Node-only module-resolution shim (its
`package.json` documents this), needed because `frames.js` and `scene.js` import
Three.js via the bare specifier `'three'` (matching every other module in `web/js/`,
resolved in the browser by `web/index.html`'s `<script type="importmap">`) and plain
`node` has no import-map support. `three.module.js` and `addons/` inside it are
symlinks back to the single vendored copy at `web/vendor/three/`; nothing is
duplicated, and it is inert for the browser. `scene_jitter_harness.mjs` imports
`scene.js` under `node` without ever constructing a `Viewer` (so `THREE.
WebGLRenderer` -- the one thing that needs a real GPU context -- is never touched);
importing the module and calling its exported `trajectoryRenderPositions` function
works because ES module import only executes top-level code, and `Viewer`'s
`WebGLRenderer` construction lives inside the constructor, never called.

## Configuration reference

| What | How |
|---|---|
| Enable/disable floating origin globally (also wired to the sidebar checkbox) | `viewer.setFloatingOriginEnabled(true \| false)` |
| Enable/disable floating origin for one frame | `viewer.floatingOrigin.setEnabledForFrame(frameId, true \| false \| null)` (`null` clears the override, falling back to global) |
| Re-base the origin manually | `viewer._rebaseOriginTo(x, y, z, force?)` (scene units, f64; normally driven automatically -- see the rebase policy above) |
| Get GPU-ready coordinates for one point | `floatingOrigin.toRenderSpace(frameId, pos)` |
| Get GPU-ready coordinates for a flat polyline | `floatingOrigin.toRenderSpaceArray(frameId, pointsXYZ)` |
| Build a trajectory's render-space vertex buffer | `trajectoryRenderPositions(poly, floatingOrigin, frameId)` (exported from `scene.js`) |
| Register a frame graph node | `frameGraph.addFrame({id, parentId})` |
| Give a frame moving-origin motion | `frameGraph.frame(id).setOriginTrack({t, pos, vel})` (Hermite w/ velocity, via `interp.js`) |
| Switch what frame an object is parented to | `frameGraph.reparent(object3D, frameId)` |
| Order a wire-shaped `frames` list parent-before-child (M4.1) | `orderFrameDefsByParent(defs)` (exported from `frames.js`) |
| Move the camera to any frame + entity, precisely (M4.1) | `viewer.setViewFrame(frameId, focusName \| null)` |
| Enable/disable floating origin for one frame, from the UI (M4.1) | `viewer.setFrameOriginEnabled(frameId, true \| false)` |
| Declare an entity-relative frame (Python, M4.1) | `scenario.frame_ric(entity)` / `.frame_vnb(entity)` / `.frame_vvlh(entity)` -> frame id |
| Enable/disable the tiled globe for a body (M15.4) | `viewer.enableGlobe(bodyName?, opts?)` / `viewer.disableGlobe()` |
| Select LOD tiles for a camera position (M15.4) | `selectTiles(cameraEcef, opts)` (exported from `globe_lod.js`) |
| Load/clear a 3D Tiles overlay (M15.4) | `viewer.loadTilesOverlay(url, opts?)` / `viewer.clearTilesOverlay()` |

## 5. Rotating relative frames (M5.2)

Through M4.1, an RIC/VNB/VVLH frame node **translated** with its origin entity's
recorded track but never **rotated** -- `FrameNode.update()` only ever received an
explicit `quaternion` argument, which no live caller ever passed. M5.2 removes that
limitation: the axes are now computed **client-side**, every render tick, from the
reference entity's own interpolated state.

### Axes math (`web/js/frames.js`)

`axesRIC`/`axesVNB`/`axesVVLH` each take a position `r` and velocity `v` (any
consistent unit -- direction only, so km vs. scene units never matters) and fill three
orthonormal unit output vectors, per `proto/altavista/v1/core.proto`'s pinned
`AxesKind` doc comments (read-only ground truth for this task) -- the same convention
`altavista/frames.py`'s `_OBJECT_REFERENCED_FIELDS` realizes server-side via GMAT:

- **RIC**: `X = R = unit(r)`, `Z = N = unit(r x v)`, `Y = Z x X` (in-track).
- **VNB**: `X = V = unit(v)`, `Y = N = unit(r x v)`, `Z = X x Y`.
- **VVLH** (ratified 2026-09-02, question 73): `Z = -R`, `Y = -N`, `X = Y x Z` (= `N x
  R`, in-track only on a circular orbit).

`quaternionFromAxes(x, y, z, out)` builds the child(local frame)-to-parent quaternion
from those three vectors via `THREE.Matrix4.makeBasis` + `setFromRotationMatrix` --
reusing Three's own basis/quaternion conversion, not a hand-derived one.

### `FrameNode.update()`: applied every tick

A declared entity-relative frame's reference entity is always its own origin entity
(`altavista/scenario.py`'s `_declare_object_referenced_frame`:
`reference_entity_id == entity_id`), so the **same** origin track
(`FrameNode.setOriginTrack`) already gives both the frame's position (via
`TrajectoryInterp.at`) and, now, its orientation: `interp.js`'s `TrajectoryInterp.at()`
gained an optional `outVel` parameter (a new `segment()` also fills it, from the exact
same cubic Hermite basis, differentiated -- not a second approximation), so a frame
node with `axesKind` set (`'ric'|'vnb'|'vvlh'`, normalized from the wire `AxesKind`
enum name by `scene.js`'s `_buildFrameGraph`) gets both `r` and `v` from one
interpolation call and derives its quaternion from them every `update(t, scale)`.
**A camera parented under such a node rotates with it automatically** -- this is
nothing more than Three's ordinary parent-child matrix composition once the frame
node's own quaternion is correct; no camera-specific code was needed.

The visible signature (see the browser verification below): with the camera in a
target's RIC frame, the target holds still on screen while Earth's lit face/terrain
visibly rotates underneath it, because the camera's *local* offset from the frame
stays roughly constant while the frame's own quaternion (and hence the camera's world
orientation and the world position implied by its small local offset) sweeps around
once per orbit.

### Ground-truth check: client axes vs. GMAT, to 1e-9

`web/js/fixtures/gen_ric_fixture.py` builds `examples/05_rpo_ric.py`'s Target orbit
through the real `altavista.Scenario` propagation path, then uses
`altavista.frames.FrameRegistry.rotation_matrix()` (read-only ground truth for this
task -- GMAT's own `CoordinateConverter`/`ObjectReferenced` axis system) to record the
RIC/VNB/VVLH rotation matrix at five epochs spread across the trajectory, committing
the result as `web/js/fixtures/ric_axes_fixture.json`. `web/js/ric_axes_check.mjs`
(invoked by `tests/test_viewer_jitter.py::test_ric_vnb_vvlh_axes_match_gmat`) loads
that fixture, runs the **real** `interp.js`/`frames.js` code (not a port) to compute
the client's own axes and quaternion at each epoch, and compares element-by-element.

**Every tested epoch is an exact trajectory-sample knot**, not an arbitrary
intermediate time -- `interp.js`'s cubic Hermite interpolation reduces *exactly* to
the recorded sample position/velocity at a segment boundary (a checkable property of
the Hermite basis functions, not an approximation; see the fixture generator's module
docstring for the derivation). This isolates what the check is actually meant to prove
-- do the client's axes formulas agree with GMAT's own for an *identical* input state
-- from a separate, already-covered question (how well a Hermite fit tracks GMAT's
true dynamics *between* samples, i.e. this file's own jitter-bound tests above).
Mixing the two would make a 1e-9 bound meaningless.

Measured (`.venv/bin/python -m pytest -q -s tests/test_viewer_jitter.py`, real output):

```
RIC/VNB/VVLH axes vs GMAT (web/js/fixtures/ric_axes_fixture.json): worst residual = 4.710e-16 (bound 1e-09) at {'t': 31041.573345305344, 'kind': 'vvlh'}
```

Worst residual **4.7e-16** -- 6+ orders of magnitude inside the 1e-9 bound, and
consistent with IEEE-754 double-precision arithmetic noise (both sides compute the
same cross-products/normalizations from the same input, in f64). The bound is never
loosened.

### Per-entity body frames: groundwork for sensor footprints

`Viewer._buildFrameGraph()` also registers one **body-frame** `FrameNode` per
spacecraft (`${name}_body`), parented under the entities frame, independent of
`sc.frames` (no `FrameRegistry`/GMAT realization backs these yet --
`AXES_KIND_PLATFORM_BODY` needs an `attitude_source` the frame service doesn't
implement until P2, per that field's own comment in
`proto/altavista/v1/core.proto`). Orientation source, per entity:

- A **real attitude quaternion stream**, when `altavista/model.py`'s additive
  `Trajectory.attitude` (`[x, y, z, w]` per sample, parallel to `t`) is non-empty --
  consumed via `interp.js`'s new `QuaternionTrackInterp` (plain slerp between
  consecutive samples). **M6.3**: this is no longer only schema groundwork --
  `altavista.scenario.Scenario` now populates it for a spacecraft with an *explicitly*
  configured GMAT attitude model (`NadirPointing`, `CoordinateSystemFixed`, `Spinner`,
  or a CCSDS-AEM file), sampled alongside the trajectory during `propagate()`/
  `maneuver()`. See `altavista/FRAMES.md`'s "Attitude sampling and sensor footprints"
  section for the producer side (quaternion convention, GMAT quirks it ran into, and
  how the convention was verified) -- this viewer-side consumer code is unchanged from
  M5.2, no JS edit was needed to make a real stream render correctly.
- Otherwise, a **nadir-pointing VVLH fallback** derived from the entity's own
  position/velocity (the same `axesVVLH` function, with the entity as its own
  reference).

**The fallback is never silent.** `frameList()` (`scene.js`) exposes `fallback: true`
on these nodes, `description` bakes in an explicit `" (fallback: nadir VVLH -- no
attitude stream)"` suffix, and `app.js` additionally applies a `.frame-fallback` CSS
class (amber, italic, `web/style.css`) to that entry in the "Frames" sidebar list.
**M6.3**: `examples/01_leo_propagate.py`'s ISS now configures `NadirPointing`, so its
body frame shows `"ISS body (attitude stream)"` (fallback gone, confirmed live -- see
"Browser verification (M6.3)" below); SunSync, which never configures an attitude
model, is unchanged and still shows `"SunSync body (fallback: nadir VVLH -- no
attitude stream)"`.

`app.js`'s "Frames" UI (`frameSelect`/`frameOriginList`) now sources from
`viewer.frameList()` (built by `Viewer.setScenario()`) instead of re-deriving from
`sc.frames` by hand -- this also removes a small pre-existing duplication (app.js used
to reimplement scene.js's "entities frame missing from `sc.frames`, synthesize a root
option" logic itself) and means body frames, which have no `sc.frames` entry at all,
show up in the same selectors/checkboxes for free.

### Browser verification (M5.2)

`.venv/bin/python -m altavista serve` (port 8765), `python examples/05_rpo_ric.py`
published "RPO demo", driven via the Browser pane tools:

- **Frame switch into `Target_ric`**: selecting `Target_ric` in "View frame" and
  `Chaser` in "Focus" re-parents the camera into the target's RIC frame; both
  spacecraft render at RPO scale (tens of metres apart), confirming the M4.1
  re-parenting mechanism still works once the frame also carries a rotating
  quaternion.
- **Target holds still while Earth rotates around it**: with "View frame" =
  `Target_ric` and "Focus" = the frame's own origin (the target), the Target label
  stayed within a few pixels of screen centre across four widely-spaced epochs (t=0,
  ~28 min, ~70 min, ~119 min into the 2.33 h scenario -- more than one full ~93 min
  LEO orbit), while Earth's lit face/terrain visibly rotated to a completely different
  orientation at each epoch (coastline silhouette -> brown continental terrain ->
  ocean-dominated night terminator -> a third, distinct mixed day/night view).
- **Body-frame fallback label**: `read_page` on the "Frames" -> "View frame" dropdown
  showed `"Target body (fallback: nadir VVLH -- no attitude stream)"` and `"Chaser
  body (fallback: nadir VVLH -- no attitude stream)"` as real option text (not just
  styled); the same pattern was independently confirmed against `examples/01_leo_
  propagate.py`'s "LEO demo" (`ISS_body`/`SunSync_body`, also both fallback-labelled).
  Selecting `ISS_body` as the view frame and `Earth` as focus rendered Earth correctly
  from that nadir-pointing-VVLH-oriented body frame.
- **No console errors**: `read_console_messages` returned "No console logs" (no
  errors, no warnings) throughout scenario load, frame switches, timeline scrubbing
  (four epochs), a body-frame view switch, and "Reset view" -- across both the RPO and
  LEO scenarios.

### Sensor footprint rendering (M6.3)

The first consumer of a real attitude stream: `Viewer` (`scene.js`) now also builds one
closed `Line2` ring per entry of the scenario JSON's additive `footprints` list
(`altavista.model.Footprint`, produced by `Scenario.footprint()` -- see
`altavista/FRAMES.md`), stored in a new `this.footprints` map parallel to
`this.spacecraft`/`this.bodies`. Unlike trajectories or attitude, a footprint ring has
no interpolation contract between samples (a set of ellipsoid-intersection points has
no natural curve to fit), so `Viewer._updateFootprint()` picks the *nearest* recorded
sample by time (`findSegment` from `interp.js`, the same primitive the spacecraft
"past"-trail feature already uses) each tick and only rebuilds the `LineGeometry` when
that nearest index actually changes -- a cheap no-op most frames. `ring`/`center` are
already absolute scenario-frame km (like `pos`), so the new
`footprintRenderPositions()` helper reuses exactly `trajectoryRenderPositions`'s
pipeline (`* SCALE` in f64, origin-subtract via `FloatingOrigin.toRenderSpaceArray`,
one `Math.fround` at the very end) -- never a separately-rounded path -- with one
addition: the first point is appended again at the end to close the open `Line2`
polyline into a visual loop. `resize()`/`clear()`/`_refreshOriginRelativeGeometry()`
(the floating-origin rebase hook) were all extended in step, the same way they already
handle `this.spacecraft`.

### Browser verification (M6.3)

`.venv/bin/python -m altavista serve` (port 8765), `python examples/01_leo_propagate.py`
published "LEO demo" (ISS now configures `NadirPointing` and declares a 10-degree
footprint), driven via the Browser pane tools:

- **Fallback label gone for the attitude-carrying spacecraft**: both a direct DOM query
  (`document.querySelectorAll('li')`, filtered to entries whose text matches `/body/i`)
  and the "View frame" `<select>`'s accessibility-tree option list showed exactly
  `"ISS body (attitude stream)"` (`fallback: false`) and `"SunSync body (fallback:
  nadir VVLH -- no attitude stream)"` (`fallback: true`) -- confirming the wiring end
  to end, not just that data was present in the JSON.
- **Scenario JSON payload** (`GET /api/scenario/LEO demo`, fetched directly, bypassing
  the client entirely): `footprints[0]` has 278 `t`/`center`/`ring` entries, matching
  ISS's own 278 `t`/`attitude` samples exactly (`attitude` length `1112 == 278 * 4`,
  the same invariant `scene.js`'s `hasAttitude` check relies on); `ring[0]` has 32
  points, the declared `n_points`.
- **Footprint ring drawn on the globe**: visually confirmed with a second, throwaway
  scenario (`Scenario.footprint(..., half_angle_deg=45.0)`, published separately under
  a different name so it never touched "LEO demo") -- a large-radius ring is far easier
  to spot at a screenshot's resolution than ISS's real 10-degree/~70 km one. Zooming
  the camera toward the spacecraft (via `focus`) showed the declared magenta ring
  (`color="#ff00ff"`) drawn directly on the globe surface, centred under the marker,
  exactly where the sub-boresight point should be. The 10-degree production ring
  (ISS's own, in "LEO demo") is the same code path with a smaller, correctly-computed
  radius (see `altavista/FRAMES.md`'s measured 2.3 km sphere-vs-ellipsoid discrepancy for
  this exact geometry) -- not re-screenshotted separately, since the rendering
  mechanism is identical and already exercised by the enlarged case.
- **No console errors**: `read_console_messages` returned "No console logs" throughout
  scenario load, playback, focus changes, and camera zoom/pan, across both scenarios.
- **Headless (non-browser) checks unaffected**: `node web/js/frame_graph_check.mjs`
  and `node web/js/ric_axes_check.mjs` (worst residual `4.71e-16`, bound `1e-9`) both
  still pass after the `scene.js` changes.

## 6. Declared state spaces and interpolation by component class (M7.1)

`docs/open-questions.md` question 88 accepted M6.3's attitude-in-the-state-vector shape
(position xyz, velocity xyz, quaternion xyzw scalar-last) with two conditions:

**(a) A declared `StateSpace`, not an ad hoc id string.** `altavista.cdm.state_space_for`
(new) returns the actual `altavista.v1.StateSpace` message -- component labels and units
-- for the two ids this codebase uses (`altavista.cartesian_pos_vel_6`,
`altavista.cartesian_pos_vel_6_attitude_quat_4`); `crates/av-kernel/src/trajectory.rs`'s
`state_space_for` declares the identical shape Rust-side (same labels, same order, same
units, cross-checked by both crates' test suites), plus the pre-existing GMAT-side id
`gmat.orbital.cartesian6` for the same 6-component shape. The scene JSON gains an
additive `stateSpaces` list (`altavista.scenario.Scenario._build_state_spaces`,
protobuf-JSON-transcoded, the same `MessageToDict` convention `frames` already uses) and
every `spacecraft[i]` entry now carries a `stateSpaceId` (`altavista/model.py`'s
`Trajectory.to_dict()`) naming which entry of `stateSpaces` its `pos`/`vel`/`attitude`
shape corresponds to -- both additive, existing scenarios round-trip unchanged
(`tests/test_script_prep.py::test_scenario_data_json_shape`). `altavista.cdm.CdmBundle`
gained a parallel `state_spaces` list, populated by `scenario_to_cdm` for every
`state_space_id` its trajectories actually ended up with.

**(b) The interpolation contract, by component class**, per
`docs/adr/005-simulation-kernel.md` §3's table (implemented here; none of that ADR's
other, still-Proposed refactors):

| class | rule |
|---|---|
| position/velocity (first six of a Cartesian space) | cubic Hermite with velocity |
| unit quaternion (`q_x, q_y, q_z, q_w`) | normalized slerp, unit norm asserted |
| rates, masses, scalars | linear |
| STM, covariance | never interpolated |
| discrete modes, counters | zero-order hold |

`web/js/interp.js` gains `classifyStateSpace`/`interpolateByStateSpace`, mirroring
`crates/av-kernel/src/interpolate.rs`'s `classify`/`interpolate_by_state_space`
component-for-component: both classify strictly from a component's declared `label`
and `unit` (never index position, past the one place the ADR itself is positional --
"the first six of a Cartesian space"), and both **throw/return a typed error**
(`InterpolationError` in JS, `InterpolationError`/`UnknownStateSpaceError` in Rust) for a
component neither convention can place, rather than silently falling back to linear or
to `hermite_velocity`'s own past-six nearest-endpoint pass-through. Quaternion groups
reuse `THREE.Quaternion.slerp` (already proven, already what `QuaternionTrackInterp`
uses) rather than a third hand-rolled slerp formula; the Hermite group reuses
`crate::interpolate::hermite_velocity` in Rust and a standalone, cross-checked
`hermiteVelocity6` in JS (see that function's doc comment for exactly what the
cross-check proves).

**This is new, tested infrastructure, not yet load-bearing in the live render path.**
Today's viewer consumes altavista's already-shaped `{pos, vel, attitude}` JSON
(`TrajectoryInterp`/`QuaternionTrackInterp`, unchanged, still what `scene.js`'s frame
graph actually calls every tick -- see §5 above), not a raw CDM `TrajectorySample.mean`
vector that would need generic component classification. ADR-006 (the scene service,
not yet built) is what points the browser at raw CDM `Trajectory`/`StateSpace` messages
directly; `classifyStateSpace`/`interpolateByStateSpace` are the class-aware
interpolator that day needs, proven correct now (headless, via `node`) rather than
invented only when ADR-006 lands.

### Required tests, measured

`web/js/attitude_slerp_check.mjs` (invoked by
`tests/test_viewer_jitter.py::test_attitude_interpolation_checks_all_pass` and friends):

- **Unit norm and continuity across a `q`/`-q` sign flip.** Two representations of the
  same physical orientation (5&deg; and 10&deg; about Z from identity, the second one
  negated -- the classic silent bug) slerped through both `QuaternionTrackInterp` and
  `interpolateByStateSpace`: every one of 21 sub-samples stays unit-norm to `1e-12`, and
  the largest single-step rotation is under 2&deg; (the short way -- the long way around
  would average ~17.5&deg;/step over the same 21 samples). The identical Rust-side
  property is `crates/av-kernel/src/interpolate.rs`'s
  `slerp_between_antipodal_representations_takes_the_short_path_and_stays_unit_norm`.
- **GMAT `NadirPointing` fine-vs-coarse-slerp measured error.**
  `web/js/fixtures/gen_nadir_attitude_fixture.py` (real GMAT, the same
  `_M63_KEPLERIAN`-orbit/`Attitude="NadirPointing"` mechanism `tests/test_frames.py`
  validates to `1e-9` against true nadir) records 241 fine attitude samples (5 s
  spacing, 1200 s span); every 12th sample (21 total, 60 s spacing) is slerped with the
  real `QuaternionTrackInterp` and compared against every fine sample in between.
  **Measured: max error 4.71e-05 deg, mean error 1.89e-05 deg** (60 s coarse spacing,
  LEO). `tests/test_viewer_jitter.py`'s bound (`NADIR_SLERP_MAX_ERROR_DEG_BOUND =
  0.01`&deg;, ~200x the measured value) is headroom for a different fixture's own
  honest variation, not a number tuned to the one committed fixture -- the real number
  is printed unconditionally by `test_nadir_pointing_slerp_error_report`, never hidden
  behind a bound.
- **The viewer's body frame uses the interpolated quaternion, through the real code
  path.** A `FrameNode` given the fixture's coarse attitude track via
  `setAttitudeTrack()` is sampled with `update()` at four fine epochs; its resulting
  `Group.quaternion` matches `QuaternionTrackInterp.at()` at the same epoch to better
  than `1e-9` degrees at every one -- the same object, not a value that merely looks
  similar. This is the existing M5.2 `FrameNode`/`QuaternionTrackInterp` wiring
  (unchanged), proven end to end rather than only unit-tested piecewise.

## Integration status

**Wired into `web/js/app.js`'s live render loop and `scene.js`'s `Viewer` class**, and
manually verified end-to-end in a real browser against all five `examples/*.py`
scripts and the JSON publish path (see the M3.1 and M4.1 task reports for
screenshots/console output). What's still explicitly deferred or approximated (M4.1):

- **`Object3D.lookAt`/`OrbitControls` local-vs-world mismatch, worked around, not
  fixed at the source.** `OrbitControls.update()` (vendored, unmodified) ends with
  `this.object.lookAt(this.target)`; `Object3D.lookAt()` treats its argument as a
  *world*-space point (it derives the eye from `this.matrixWorld` but uses the passed
  target as-is), while `controls.target` here is set to a *local* (parent-relative)
  point -- correct for OrbitControls' own position/zoom/pan math, which never mixes
  the two, but wrong for that one internal `lookAt` call whenever the camera's parent
  frame has a non-negligible world-space position of its own. This was **always** a
  latent inaccuracy (confirmed by measurement: ~8.4&deg; off, `dot`&asymp;0.989, when
  focusing an entity after a large floating-origin rebase in the pre-M4.1
  single-frame code) but negligible there because the entities frame's typical
  offset-to-shift ratio kept it small. It becomes severe for a frame parented far from
  the origin at RPO scale (an entity-relative RIC frame's Group.position is the
  origin entity's full orbital-scale absolute position, while the intended viewing
  radius is metres) -- confirmed the hard way during this task's browser
  verification (camera pointed roughly 95&deg; off target, i.e. nothing on screen).
  **Fix**: `Viewer.setViewFrame()` and `Viewer.update()`'s non-entities-frame branch
  call `this.camera.lookAt(targetWorld)` explicitly, every tick, using a properly
  world-space point, immediately after `controls.update()` -- overriding only the
  orientation OrbitControls got wrong, not its (correct) position/zoom/pan math. The
  entities-frame path is untouched (out of this task's scope, and the ~1&deg;-scale
  inaccuracy there has no visible effect at whole-scenario zoom levels) but is a real,
  pre-existing, now-documented finding, not something this integration introduced.
- **`setViewFrame`'s default viewing radius (no focus set) is a fixed heuristic**
  (`1e-4` scene units, ~100 m) tuned for RPO/entity-relative frames, not derived from
  the target frame's actual scale -- switching to a body-scale frame (e.g.
  `EarthBodyFixed`) with no focus set gives a not-very-useful extreme close-up (nothing
  visible but a distant trajectory sliver) until a focus is picked. Not a crash, not
  silent (still logs nothing wrong, since nothing *is* wrong -- just a poor default);
  confirmed live against `LEO demo`'s `EarthBodyFixed` frame. Worth a scale-aware
  default (e.g. derived from `fitRadius` for a body-centred frame, from a declared
  RIC/VNB/VVLH separation scale otherwise) as a follow-up, not built here.
- Per-frame floating-origin toggling now has a dedicated UI (the "Frames" section's
  checkbox list, wired to `Viewer.setFrameOriginEnabled`) -- but only the entities
  frame's checkbox has a *visible* effect today, since bodies/spacecraft/trajectories
  are always rendered under that one frame (see [§4](#4-multi-frame-graph-and-per-frame-viewing-m41)
  for why the frame graph doesn't need a second `FloatingOrigin` instance per frame
  for camera-only frames like RIC). The other checkboxes are real, non-stubbed calls
  into `floatingOrigin.setEnabledForFrame` (exercised directly by
  `tests/test_viewer_jitter.py`), just not yet observable live.
- The auto-rebase drift threshold (`ORIGIN_REBASE_DRIFT` in `scene.js`, 20 km) is one
  fixed constant for the entities frame, not tuned per frame/scale -- unchanged from
  M3.1; still reasonable since only the entities frame uses it (RIC-frame viewing
  doesn't need a drift-triggered rebase at all, see §4).

## Escalations

None outstanding from M3.1 (the single-frame `FrameDefinition.parentId`/`ScenarioData`
gap that section used to describe is resolved -- see
[§4](#4-multi-frame-graph-and-per-frame-viewing-m41)). M4.1's two findings above
(the OrbitControls lookAt fix, and the default-radius heuristic) are documented as
approximations/follow-ups, not escalations: both have a working, honest resolution
already in place. None outstanding from M5.2 either: axes math is derived directly
from the pinned, ratified `AxesKind` conventions (no open question consumed), and the
GMAT-agreement check (§5) hits 4.7e-16, far inside the required 1e-9 bound with
margin to spare.

**None outstanding from M15.4.** `3d-tiles-renderer` (NASA AMMOS 3DTilesRendererJS)
was successfully fetched and vendored offline (this environment had outbound network
access to `registry.npmjs.org` during development -- checked explicitly before
proceeding, see the task report; nothing is fetched from a CDN or the network at
*test* or *run* time, only this one-time vendoring step). Two real approximations are
recorded in §7 above, not escalations: the 3D Tiles overlay's placement is a fixed
demo transform rather than true geo-referencing, and the streaming-layer module's own
budget/cancellation/label-enforcement policy (question 44) is only built for the
globe (`TileLoadScheduler`) so far, not yet layered on top of the 3D Tiles overlay
too -- both have a working, honest, documented resolution today, not a silent gap.

**One finding from M17.1 (question 122), not an escalation in the blocking sense --
recorded plainly per this task's own honesty rules:** the golden maneuver DRM (this
task's only real-run fixture) never declares an ICRF frame -- `RunProducts.frames`
carries `EarthMJ2000Eq` only, verified directly off a real `av-run` binary's output and
against `drms/leo_1day_golden.system.yaml`'s own `spacecraft.CoordinateSystem`. The
ICRF-threading *mechanism* itself is fully general and proven correct with real
GMAT-propagated numbers (§8's synthetic bundle, both in the test suite and live in the
browser); what is missing is a producer that actually declares an ICRF frame for this
DRM, which needs a `drms/**`/`crates/**` change -- both outside this task's file
ownership, and changing the DRM's own propagation frame would break its parity with
`goldens/leo_1day_maneuver_vnb.json` (computed in MJ2000Eq) besides. See §8 for the full
writeup.

**One handoff from M7.1, not an escalation:** the Rust dynamics service
(`crates/av-dynamics-service`) and the DRM executor
(`crates/av-kernel/src/drm/executor.rs`) both produce `Trajectory`s with a fixed
`state_space_id` but neither is owned by this task (per its file-ownership rules), and
neither crate currently depends on `av-kernel` (the dynamics service doesn't depend on
it at all; the executor is in the same crate but a different, excluded module). The
declared `StateSpace` helper (`av_kernel::trajectory::state_space_for`) is built and
tested; wiring those two call sites is recorded in this task's own report for the
engineering manager to apply once both sides have landed, not attempted here.

## 7. Tiled globe and 3D Tiles overlay (M15.4)

Builds module list items 3 and 4 of `docs/architecture.md` §4 "Presentation plane":

> *Globe*: WGS84 ellipsoid quadtree with imagery and terrain tiles from our tile
> gateway, screen-space-error LOD, decoding in worker threads, budgeted loading.
>
> *Streaming layers*: overlays arrive as tiles or chunks through a priority queue with
> a memory budget and cancellation ... 3D Tiles support through NASA AMMOS's
> `3DTilesRendererJS` is an option to evaluate, not a dependency.

and `docs/open-questions.md` question 44's ratified answer: "adopt 3DTilesRendererJS
behind our layer interface, vendored like Three.js; our streaming-layer module owns
budgets, cancellation and label enforcement." **No CesiumJS** (ADR-000, unchanged) --
both the globe and the 3D Tiles overlay are this codebase's own modules.

### `web/js/globe_lod.js`: the quadtree, framework-free

Same reasoning as `origin.js` (§2 above): no Three.js import, so the shipped renderer
and `web/js/globe_lod_check.mjs`'s headless `node` harness exercise the *exact same*
tile-selection arithmetic, never a description of it.

- **Tile addressing**: a geographic (EPSG:4326-style "plate carree") scheme, not Web
  Mercator -- level 0 is 2 tiles covering the whole 360x180 degree globe, each level
  doubles both axes (`tileCountX(level) = 2**(level+1)`, `tileCountY(level) =
  2**level`). No polar singularity to special-case, unlike Mercator.
- **WGS84 geometry**: `geodeticToEcef(lonDeg, latDeg, heightM)` is the standard
  closed-form geodetic-to-ECEF conversion (metres); `tileBoundingSphere` and
  `tileVertexPositions` build on it.
- **Screen-space-error LOD** (`screenSpaceErrorPx`): the standard `geometricError *
  screenHeight / (2 * distance * tan(fovY/2))` projection (the same formula
  Cesium/3D Tiles use), with `geometricErrorAtLevel` derived from the tiling scheme
  (halves each level) rather than read from an authored tileset (this quadtree is
  generated, not authored).
- **`selectTiles(cameraEcef, opts)`**: breadth-first from the two root tiles, refining
  into a tile's 4 children (`tileChildren`, a **fixed** NW/NE/SW/SE order) when its
  screen-space error exceeds `sseThreshold` and a tile/depth budget allows it, else
  selecting the tile as a leaf. **The returned array is always sorted by `compareTiles`
  (level, x, y) before return** -- this is what makes tile selection deterministic
  across independent `node` process runs (this task's binding rule: "sorted iteration,
  no dependence on Map/Set insertion accidents"), not merely that the traversal happens
  to be order-stable today.
- **`TileLoadScheduler`**: budgeted (`residentBudget`, an LRU cache with real
  eviction) and cancellable (`AbortController` per pending load; a tile that falls out
  of the current selection before its load "completes" is aborted, `cancelledCount`
  counts it) -- framework-free, uses only the platform `AbortController`.
- **`tileVertexPositions(tile, segments)`**: the exact body-local, origin-independent
  vertex-position math both `web/js/globe.js`'s `buildTileMesh()` (the browser
  renderer) and `web/js/scene_jitter_harness.mjs`'s `measureRpoWithGlobePresent()`
  (the precision proof, see below) call -- one implementation, not a port.

### `web/js/globe.js`: the Three.js renderer

The only file in this pair that imports Three.js (so `globe_lod_check.mjs` never needs
a WebGL context). `GlobeLayer.update(cameraLocalPos, screenHeightPx, fovYRad)` calls
`selectTiles`/`TileLoadScheduler.update` and reconciles its `THREE.Group`'s children:
build+add a `BufferGeometry` (position/normal/uv, from `tileVertexPositions` +
`ellipsoidNormal`, a WGS84 gradient-normal computation -- not `normalize(position)`,
which is subtly wrong once flattening is nonzero) for each newly selected tile,
dispose+remove one for each tile that fell out of selection. Imagery is loaded from a
**configurable XYZ-shaped URL template** (`{z}/{x}/{y}`, default
`./fixtures/tiles/{z}/{x}/{y}.png`) via `THREE.TextureLoader`; a tile whose imagery
fails to load falls back to a flat-colour material -- mirrors `scene.js`'s existing
`makeBodyMesh()` texture-load-failure fallback exactly, never a hard error.

**Floating-origin compatibility (the hard constraint):** tile vertices are built
directly in **body-local** WGS84-ellipsoid units (`tileVertexPositions`), never the
body's absolute (origin-frame-relative) position. A body's radius in scene units is
always small (Earth: ~6.378, `SCALE = 1e-3` km/unit) regardless of how far the body
itself is from the current floating-origin origin, so tile vertices never need
`origin.js`'s f64-subtract-then-f32-cast pipeline at all -- the same reason
`makeBodyMesh()`'s sphere geometry is built in unit-sphere-then-`mesh.scale()` space
rather than absolute coordinates. `Viewer._syncGlobeLayer()` (`scene.js`) tracks
`GlobeLayer.group`'s position/quaternion to the body's own already origin-relative
`mesh.position`/`mesh.quaternion` every tick (a plain copy, parented as a *sibling* of
the body mesh, not a child of it, so tile vertices are never additionally scaled by the
body mesh's own `(r,r,r*(1-flattening))` factor -- flattening is instead baked directly
into each vertex via `WGS84_A_M`/`WGS84_B_M`). The globe therefore never writes into
the *entities* frame's shared, origin-relative trajectory vertex buffers that the RPO
precision bound is about -- it is architecturally incapable of disturbing them, not
merely tested not to.

**Proved, not just argued** -- `web/js/scene_jitter_harness.mjs`'s additive
`measureRpoWithGlobePresent()` (invoked by `tests/test_viewer_globe.py`, extending the
existing jitter harness rather than a parallel one, per this task's brief): builds real
globe tile vertices (`selectTiles`/`tileVertexPositions`, the exact functions
`GlobeLayer` uses) for a camera at the RPO scene's own LEO altitude, runs them through
the **same** `FloatingOrigin` instance the RPO measurement uses but under a *different*
frame id (`'earth-body'` vs. `'ric'`), then reruns the RPO measurement against that
same, now-globe-touched instance and compares it **bit-for-bit** to a pristine
baseline. Measured (`.venv/bin/python -m pytest -q -s tests/test_viewer_globe.py`):

```
RPO precision with the globe present (web/js/scene_jitter_harness.mjs):
  errWithM (globe present)  = 3.385367e-06 m  (bound 0.01 m)
  errWithM (no-globe baseline) = 3.385367e-06 m
  matchesBaselineExactly = True
  globe tiles selected at RPO/LEO camera distance = 38
  globe tile vertex max magnitude = 6.378137 scene units (bound 65.0)
```

`errWithM` is **identical** (not merely both-under-bound) to the pre-existing RPO
number in the table under §"Measured jitter" above (`3.385366653674282e-06` m,
`3.39e-06` in that table) -- the globe genuinely does not touch the RPO/RIC frame's
floating origin, and the globe's own tile vertices stay at Earth-radius scale
(6.378 scene units) regardless of the RPO camera's LEO-altitude distance from Earth's
centre, which is *why* they never needed floating-origin treatment in the first place.
The centimetre bound (`CENTIMETRE_BOUND_M = 0.01`) is never loosened.

### Offline imagery fixture

`web/fixtures/gen_globe_tiles.py` (no PIL/Pillow -- not installed in `.venv`; a small
hand-rolled PNG encoder, stdlib `zlib`/`struct` only) generates the **complete**
quadtree pyramid through level 2 (2 + 8 + 32 = 42 tiles, ~168 KB total, each a solid
colour with a darker 2px border so tile boundaries are visible) under
`web/fixtures/tiles/{z}/{x}/{y}.png`. `GlobeLayer` defaults to `maxLevel: 2`, so with
this fixture complete through level 2, **no camera direction requests a tile the
fixture doesn't have** under default settings -- deliberately avoiding a fixture with
intentional gaps, which would otherwise turn every missed tile into a browser console
error (Chrome logs "Failed to load resource: 404" for any failed request, independent
of whether application code handles it gracefully) and violate this task's clean-console
requirement. The texture-load-failure fallback path (flat colour, see above) still
exists and still matters for a real tile gateway or a `maxLevel` raised past what this
fixture covers; it is simply not exercised by the default browser-verification path
with this complete fixture. **The headless `node` harnesses never fetch these images at
all** -- tile *selection* (`selectTiles`) is pure geometry/screen-space-error math, and
`measureRpoWithGlobePresent()` only calls `tileVertexPositions` (no `TextureLoader`,
which needs `document` and is unavailable under plain `node` regardless).

### `web/js/tiles_layer.js`: the 3D Tiles overlay layer interface

**Vendored: NASA AMMOS 3DTilesRendererJS, npm `3d-tiles-renderer` 0.5.2, Apache-2.0** --
`web/vendor/3d-tiles-renderer/{LICENSE,VERSION.txt,build/}`. Only the `./three` entry
point is vendored (`build/index.three.js` -> `renderer-3xKvdklX.js` ->
`renderer-tyqPdeD-.js`, which has **zero** external dependencies of its own); the
pmtiles/vector-tile plugin bundles (which pull in `pbf`, `pmtiles`,
`@mapbox/vector-tile` -- dependencies this project neither wants nor uses) are neither
vendored nor imported. That entry point itself imports `three/addons/loaders/
GLTFLoader.js` and `three/addons/utils/{BufferGeometryUtils,SkeletonUtils}.js`, newly
vendored at `web/vendor/three/addons/{loaders,utils}/` from the matching r185 `three`
npm package -- the same MIT-licensed library already vendored at `web/vendor/three/`,
covered by its existing `LICENSE` file, not a new dependency. `GLTFLoader.js`'s own
transitive imports (checked against the tarball) are exactly those two files plus
`three` itself -- nothing else needed vendoring.

`web/js/tiles_layer.js`'s `TilesOverlayLayer` is "our layer interface" (question 44):
every call site goes through this one small class (`attachCamera`, `update`,
`dispose`, `.group`), never `web/vendor/3d-tiles-renderer/` directly -- swapping the
vendored library later, or adding the streaming-layer module's own budget/cancellation/
label-enforcement policy (not built here, see Escalations below), touches this one
file. `Viewer.loadTilesOverlay(url, opts)`/`clearTilesOverlay()` (`scene.js`) wire it
into the live viewer, driven every tick from `update()` alongside the globe.

**This is explicitly an overlay layer, not the globe** (this task's brief, item 4): no
attempt is made to geo-locate the tileset's content against the frame graph or the
floating origin -- `loadTilesOverlay`'s `opts.scale`/`opts.offset` place it as a small,
fixed, clearly-a-demo transform near the current view, only so a fixture tileset can be
confirmed to load and render through the real vendored loader. True geo-referencing of
a 3D Tiles overlay is follow-up work (see Escalations).

`web/fixtures/gen_3dtiles_fixture.py` generates the fixture from scratch (no
pygltflib/PIL): `web/fixtures/3dtiles/tileset.json` (one root tile, 3D Tiles 1.1,
`content` pointing directly at a `.glb` -- confirmed against the vendored renderer's
own content-type switch, which handles `.glb` via `GLTFLoader` directly, same as
`.gltf`, no b3dm wrapper needed) and `tile.glb` (one flat triangle, hand-built GLB
container via stdlib `struct`/`json`). Parse-correctness was checked directly against
the real vendored `GLTFLoader` under `node` (`loader.parse()` on the raw bytes, no
DOM/fetch needed for that step) before the browser verification below.

### Required tests (`tests/test_viewer_globe.py`)

Two headless `node` harnesses, invoked exactly like `tests/test_viewer_jitter.py`'s
(subprocess, JSON on stdout, no arithmetic ported to Python):

- **`web/js/globe_lod_check.mjs`**: tile selection/scheduling over a fixed, scripted 8-step
  camera path (geodetic lon/lat/altitude -> ECEF; GEO altitude, descending to LEO
  close-up, a deliberate large jump to the opposite side of the globe, then back out).
- **`web/js/scene_jitter_harness.mjs`**: extended (additively) with `RPO_with_globe`,
  see above.

| Test | What it would catch |
|---|---|
| `test_tile_selection_is_deterministic_across_process_runs` | Two independent `node` invocations must print byte-identical JSON; catches an implementation that returns tiles in an order sensitive to Map/Set insertion accidents or omits `selectTiles`'s final canonical sort. |
| `test_tile_budget_is_respected` | Catches a load scheduler with no eviction logic -- the fixed camera path visits far more distinct tiles than the budget (deliberately sized between one step's own selection size and the running total), so eviction must actually run; `evictedCount > 0` is asserted directly so this can't pass by accident (budget never actually challenged). |
| `test_tile_loading_is_cancelled_on_camera_move` | Catches a scheduler that never cancels a stale in-flight load -- the path's step 4->5 jump happens before the previous step's loads (deliberately slow-completed, see the harness) have all finished. |
| `test_lod_refines_with_camera_distance` | Catches a selector that ignores camera distance entirely (fixed depth, or no screen-space-error computation) -- the GEO-altitude step must select strictly coarser tiles than the LEO close-up step. |
| `test_rpo_centimetre_precision_holds_with_globe_present` | The centimetre bound, with the globe present -- never loosened. |
| `test_globe_present_matches_no_globe_baseline_exactly` | The stronger claim: bit-for-bit identical to a pristine (no-globe) baseline, not merely "still under the bound" -- catches a frame-id typo, aliased scratch state, or any other cross-contamination between the globe's frame id and the RPO measurement's own. |
| `test_globe_tile_vertices_stay_body_local_scale` | Catches tile vertices built from a body's *absolute* (origin-frame-relative) position instead of body-local WGS84 coordinates -- would blow the small-magnitude (~Earth-radius) expectation the whole compatibility argument rests on. |

Measured (`.venv/bin/python -m pytest -q -s tests/test_viewer_globe.py`):

```
globe LOD tile selection (web/js/globe_lod_check.mjs):
  residentBudget=32 maxResidentObserved=32 budgetRespected=True
  cancelledCount=24 evictedCount=5
  maxLevelFar(GEO)=0 maxLevelNear(LEO)=5
```

### Browser verification

`.venv/bin/python -m altavista serve` (port 8765), `examples/01_leo_propagate.py`
("LEO demo") and `examples/05_rpo_ric.py` ("RPO demo") published, driven via the
Browser pane tools:

- **Globe toggle + LOD-by-distance**: the "Globe (M15.4)" sidebar section's "Tiled
  globe (Earth)" checkbox replaces Earth's plain textured sphere with the quadtree
  (confirmed via `read_page`'s accessibility tree showing the info line update, and
  screenshots reviewed live in this session -- see the task report for what was seen at
  each zoom step). Zooming in step by step showed level-0 (2 whole-hemisphere tiles),
  then level-1 (4 visibly bordered quadrant tiles), then level-2 (finer tiles) selected
  in turn -- confirmed both visually (distinct per-tile colours/borders from the
  fixture) and via `read_network_requests` (`GET .../fixtures/tiles/0/...`, then
  `.../1/...`, then `.../2/...`, all `200 OK` against the complete level-0..2 pyramid).
  Unchecking the box restored the original textured sphere exactly (`disableGlobe()`).
- **3D Tiles overlay**: "Load 3D Tiles fixture" fetched `fixtures/3dtiles/tileset.json`
  and `fixtures/3dtiles/tile.glb` (both confirmed `200 OK` via
  `read_network_requests`) through the vendored `TilesRenderer`.
- **Existing viewer unaffected**: both scenarios' spacecraft/trajectories/frame
  switching rendered exactly as before M15.4 (frame graph, floating origin, sensor
  footprint, attitude fallback labelling all unchanged, no globe/tiles code touches
  any of that machinery when disabled -- the default state).
- **Console**: `read_console_messages` showed exactly one line for the whole session,
  a `[warn]` (not an error) from the vendored 3D Tiles renderer itself --
  `"TilesRenderer: tiles versions at 1.1 or higher have limited support"` -- an
  upstream, informational notice about this fixture's declared 3D Tiles 1.1
  `asset.version`, not an application bug. Zero `[error]` entries throughout scenario
  switching, globe enable/disable, LOD zoom steps, and the 3D Tiles overlay load.

## 8. CDM ingest frame threading (M17.1, question 122)

Closes the last gap in question 5's first demo: *"a DRM authored in Python, propagated
with GMAT dynamics, shown on the custom globe **and in ICRF**, reproducible from its
config hash."* Before this task, neither `POST /api/cdm/run` nor
`POST /api/cdm/trajectory` populated the scene's `frames` list, so `scene.js`'s
`_buildFrameGraph` always hit its documented fallback ("synthesizing a root frame node")
for a CDM-ingested run -- no frame selector options at all, ICRF included.

**What changed (`altavista/cdm.py`, `altavista/server.py` -- both owned by this task; no file
in `web/js/` needed a behaviour change beyond a small headless-harness addition, since
the browser's frame-consuming code -- `web/js/frames.js`/`scene.js` -- already worked
correctly the moment it was given a real, non-empty `frames` list).**

- `altavista.cdm.frames_to_viewer_json(frame_defs)`: threads any sequence of already-declared
  `altavista.v1.FrameDefinition`s (in practice, `RunProducts.frames`, M17.2) through a
  fresh `altavista.frames.FrameRegistry` -- the same registry `altavista/scenario.py`'s
  `Scenario._build_frames` (this task's read-only reference) uses for a Python-built
  scenario -- and returns the identical protobuf-JSON wire shape
  `ScenarioData.to_dict()["frames"]` already carries. Every entry is genuinely validated
  against real GMAT (`gmatName` gets filled by `FrameRegistry.register()`, never copied
  verbatim from the wire, which never carries one); `parent_frame_id` gets question 76's
  deterministic fill. Raises `CdmAdapterError` (never a bare `altavista.frames.FrameError`)
  if GMAT rejects a declared definition.
- `altavista.cdm.viewer_frame_for(frame_def)`: the entities frame (`ScenarioData.frame`)
  now gets its *real* `origin`/`axes` from the bundle's own matching declared
  `FrameDefinition`, not the hardcoded Earth/MJ2000Eq `altavista.model.Frame` default every
  frame this endpoint could not resolve used to silently fall back to.
- `POST /api/cdm/run` calls both, unconditionally, for every ingested run.
- `POST /api/cdm/trajectory` (bare `altavista.v1.Trajectory`, no `frames` field, and a
  binary-protobuf body that cannot carry a second message without a new declared wrapper
  type -- `proto/**` read-only, no proto change authorized) additively accepts frames
  *only* on its JSON-transcoding path, as a plain JSON envelope
  `{"trajectory": <Trajectory JSON>, "frames": [<FrameDefinition JSON>, ...]}`,
  distinguished from a bare `Trajectory` body by the presence of a top-level
  `"trajectory"` key (never one of `Trajectory`'s own camelCase field names). This is a
  JSON convention, not a new `.proto` message; the binary-protobuf path is completely
  unchanged and still cannot carry frames -- a caller wanting real, validated frames for a
  single trajectory should use `POST /api/cdm/run` instead. **Judgment call, not an
  escalation**: `altavista.v1.Batch`/`BatchMessage` (`envelope.proto`) was considered as
  the "already-declared, no-proto-change" carrier instead, and rejected as unnecessary
  machinery (`Any` type-URL packing/unpacking) for what only needs to reach one demo
  bridge; if the lead wants a typed envelope instead of this JSON convention, that is a
  straightforward follow-up.

### The ICRF finding (read this before assuming "ICRF now always shows up")

`RunProducts.frames` for the **golden maneuver DRM**
(`drms/leo_1day_maneuver_vnb.{drm,sos}.yaml` + `drms/leo_1day_golden.system.yaml`) --
this task's only available real-run fixture -- contains **exactly one** entry,
`EarthMJ2000Eq`, and nothing else. Verified twice: directly, by building `av-run` and
decoding a real run's output bytes with the generated `run_pb2.RunProducts` bindings
(no ICRF entry anywhere), and by reading `drms/leo_1day_golden.system.yaml`'s own
`spacecraft.CoordinateSystem: EarthMJ2000Eq` parameter, which is exactly what
`av_kernel::drm::binding.rs` uses as `Trajectory.frame_id` and what
`crates/av-kernel/src/drm/executor.rs`'s `collect_frames`/`registry_default_frame`
derive `RunProducts.frames` from. **This is not something this task's own files could
fix**: `crates/**` and `drms/**` are both off limits, and even without that rule,
changing the DRM's own propagation frame would break its parity with
`goldens/leo_1day_maneuver_vnb.json` (computed in MJ2000Eq). So pushing *this* fixture
through `POST /api/cdm/run` and looking at the frame selector shows one option,
`EarthMJ2000Eq`, honestly -- **not** ICRF, confirmed live in the browser (see below).

`frames_to_viewer_json`/`viewer_frame_for` never invent a frame the bundle did not
declare -- an ICRF sibling for a body that only declared MJ2000Eq is not synthesized,
per this task's own "never a plausible-looking `FrameDefinition`" rule. The mechanism
itself is fully general (any declared `AxesKind`, ICRF included, threads through
correctly -- proven with real numbers below); what is missing for *this specific DRM* is
upstream of this task's ownership: a producer that actually declares an ICRF frame (a
future DRM whose `spacecraft.CoordinateSystem` is `EarthICRF`, or a richer `av-kernel`
frame registry that reports ICRF alongside MJ2000Eq for the same body). Recorded here as
a real, open finding, not smoothed over.

### Required tests (`tests/test_cdm_run.py`)

Split into the two honest halves the finding above implies:

- **General mechanism, real run**: `test_run_frames_populate_the_scene_frame_list_
  through_frame_registry`, `test_run_frame_list_matches_the_equivalent_python_scenario_
  shape`, `test_headless_harness_reports_the_real_frame_graph_for_the_ingested_run` --
  all against the real golden maneuver run (`published_scenario`). Prove: `frames` is
  populated and `FrameRegistry`-validated (`gmatName` set), the entities frame gets its
  real origin/axes, the list is structurally identical to what a Python scenario's own
  `Scenario._build_frames` produces for the same frame, and the real, shipped
  `web/js/frames.js` `FrameGraph` actually builds a node from it (not the synthesized-root
  fallback).
- **ICRF-specific, synthetic-but-real numbers**: `test_synthetic_icrf_run_shows_an_icrf_
  option_and_places_the_trajectory_correctly` -- mirrors the pre-existing `test_fault_
  event_kind_reaches_the_same_server_and_viewer_path` pattern (a protocol-honest,
  hand-built `RunProducts` filling a gap the golden DRM fixture cannot). A real
  GMAT-propagated LEO orbit is recorded directly in ICRF by a Python `altavista.Scenario`,
  converted with the same unmodified `trajectory_to_cdm`/`frame_definition_for` a real
  producer would use, and POSTed. Checks the ICRF option is genuinely present (not an
  empty-list coincidence), that the real `FrameGraph` builds a node for it, and that the
  real cubic-Hermite `TrajectoryInterp` (`web/js/interp.js`) places the ingested
  trajectory, at an off-sample query epoch, within 1e-6 km/1e-6 km/s of interpolating the
  original Python trajectory the bundle was built from -- both through the identical
  interpolation code, so this isolates the CDM round trip's own floating-point/unit
  precision (observed ~1e-12 km), not interpolation error.
- **`/api/cdm/trajectory` with frames alongside**: `test_publish_cdm_trajectory_accepts_
  frames_alongside_in_a_json_envelope` -- the envelope above, an ICRF frame, checked the
  same way.
- **`web/js/verify_cdm_run.mjs`** (owned by this task) gained `frameGraphFacts` (drives
  the real `FrameGraph`/`orderFrameDefsByParent` from a scenario's `frames` list, exactly
  the wire-shape normalization the non-owned `web/js/frame_graph_check.mjs` already
  demonstrates against a hand-built list) and an optional interpolated-state report
  (real `TrajectoryInterp`), both additive to its existing hash/event-kind checks.

### Browser verification

`.venv/bin/python -m altavista serve --port 8799`; the real golden maneuver run pushed via
`av-run --server http://127.0.0.1:8799` (`run:browser-verify`) and a second,
synthetic-but-real ICRF bundle pushed the same way `test_synthetic_icrf_run_...` builds
one (`run:browser-verify-icrf`), both through the Browser pane tools:

> **Historical log (M17), superseded — kept as the record of what was observed then.** Three
> things below are no longer true of the current viewer. (1) The real demo run now offers
> `EarthICRF` and `EarthBodyFixed` alongside `EarthMJ2000Eq`: M18.1 made question 10's mandatory
> frames always present in `RunProducts.frames`, so the "no ICRF option" finding is closed.
> (2) The frame picker shows the frame **id** with the description as a tooltip (question 136),
> not the producer description as the option text, so the quoted option labels no longer appear.
> (3) `setViewFrame`'s "re-parent, do not refit" behaviour was a defect, not a design: question
> 134 / E-27 made a frame switch refit the camera and made "Reset view" keep the selected frame.

- `run:browser-verify` (the real golden maneuver run): info line
  `EarthMJ2000Eq · 2.0 h · 1 spacecraft · config 744e8632...` (the DRM's own declared
  hash), events list showing `run_start`/`burn1`/`run_end`, "View frame" dropdown has
  exactly one option, `"registry default for GMAT CoordinateSystem ... (body Earth, axes
  Mj2000Eq)"` -- **no ICRF option**, honestly, matching the finding above.
- `run:browser-verify-icrf` (the synthetic ICRF bundle): info line
  `EarthICRF · 2.0 h · 1 spacecraft · config browser-demo-icrf-hash`; "View frame"
  dropdown has `"altavista frame 'EarthICRF' (Earth ICRF)"` selected by default (index 0,
  the entities frame) alongside the per-entity `Sat_body` fallback node. Selecting
  `Sat_body` re-parented the camera there (a close-up view, the trajectory now a long
  diagonal line, exactly `setViewFrame`'s documented re-parent-not-reload behaviour);
  selecting `"altavista frame 'EarthICRF'"` again and clicking "Reset view" restored the
  original whole-orbit view.
- **Console**: `read_console_messages` returned "No console logs" (zero entries, error or
  otherwise) throughout scenario switching, both frame selections, and Reset view -- in
  particular, no `"synthesizing a root frame node"` warning, confirming the real frame
  graph was used, not the fallback, for either scenario.
- Server killed after (`kill`, verified with `ps aux | grep altavista` afterward: no
  process left).

## 9. Static asset caching (M21.1, question 139)

`altavista/server.py` serves `web/` (both the `StaticFiles` mount and the separate `/` ->
`index.html` route) with `Cache-Control: no-cache` and an ETag on every response
(`RevalidatingStaticFiles`, a thin `StaticFiles` subclass that adds the header onto whatever
Starlette's own ETag/conditional-request logic returns). `no-cache` means "revalidate before
use", not "don't store" -- an unchanged file still comes back as a 304, it just never gets
served from the browser's cache without a round trip first.

This closes the correctness hazard the lead hit after M20: a browser held a heuristically
"still fresh" `cdm_run.js` while loading a newly deployed `app.js`, and the viewer died at
module import because the two files were from different deploys. Revalidation on every load
makes that impossible -- the browser always asks the server first.

**This is not the final answer.** Revalidation costs a round trip per asset per load, and it
still depends on the server being reachable to confirm freshness. The eventual fix, for an
offline/no-revalidation build, is **content-hashed asset filenames** (e.g. `app.abc123.js`):
a filename that changes only when its content changes can be cached forever (`immutable`)
with no revalidation at all, because a stale reference to an old hash simply 404s instead of
silently serving mismatched modules. That is follow-on work, not done here -- `web/js/**` has
no build step yet, so there is nothing to hash filenames of today.

## Explicitly not built (P1 / P3, per `docs/architecture.md` §4's module list)

- **Globe terrain**: imagery only -- no terrain/elevation tiles (the "terrain tiles"
  half of the Globe bullet), no worker-thread decoding (P1 scope is small PNG imagery
  tiles built on the main thread; a real tile gateway serving larger/compressed
  payloads would need this).
- **Streaming layers' own budget/cancellation/label-enforcement policy** (question
  44's "our streaming-layer module owns budgets, cancellation and label enforcement")
  is not yet layered on top of `TilesOverlayLayer` -- today it's a thin pass-through to
  the vendored `TilesRenderer`'s own (real, working) budget/LOD machinery, not a
  second policy layer this codebase owns. `web/js/globe_lod.js`'s
  `TileLoadScheduler` *is* that owned policy layer, but only for the globe today, not
  yet reused for the 3D Tiles overlay.
- **3D Tiles overlay geo-referencing**: `loadTilesOverlay`'s placement is a fixed demo
  transform (scaled down, positioned near the current view), not derived from the
  tileset's real geographic region via the frame graph -- see this section's own
  writeup above.
- **Entities**: no instanced markers beyond what `scene.js` already draws, no covariance ellipsoids, contact lines, or occlusion-aware labels. **M6.3 built the first sensor footprint** (a cone-vs-WGS84-ellipsoid ring, see §5's "Sensor footprint rendering") -- a single ring per declared footprint, discrete (nearest-sample) in time, not yet a full 3D cone/frustum mesh, and not yet exposed in the sidebar UI as its own toggleable list (only visible on the globe, and via the underlying `footprints` JSON).
- **Views as data**: no view-profile persistence (frames/layers/camera rigs as declared data).
- WebGPU path (open-questions.md Q47) -- WebGL2 only, as today.

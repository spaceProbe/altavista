// web/js/entities/model_entity.js -- H6 scope item 2: "glTF asset models with attitude
// from the attitude stream. Load with the vendored GLTFLoader. The attitude comes from
// the same path the viewer already uses -- BodyInterp.orientation(t, out) in
// web/js/interp.js."
//
// This module does not reimplement glTF parsing (the same "Q44 ratified decision"
// web/js/tiles_layer.js's/web/js/layers/tiles3d_layer.js's own module docstrings
// already cite for the 3D Tiles overlay, applied here to a single standalone asset):
// it imports `GLTFLoader` from `web/vendor/three/addons/loaders/GLTFLoader.js`
// (already vendored, per this round's rules -- no new loader dependency added), and
// calls its own `.parse(data, path, onLoad, onError)` directly rather than `.load()`,
// so this module never performs its own network fetch either -- a caller supplies
// `data` (an ArrayBuffer for a .glb, or a JSON string/object for a .gltf) however it
// obtained it (a real `fetch()` in the browser, a file read under `node` for a
// headless check -- see web/js/entities_model_check.mjs). That is also what keeps this
// module usable from a `Layer.load()` adapter later (`./entities_instanced_layer.js`
// covers markers/trails, not this file -- see this task's report for why glTF ASSET
// loading is not itself run through `LayerManager` in this round).
//
// Attitude: this module does NOT reimplement `BodyInterp.orientation`'s missing-quat
// guard (web/js/interp.js, "Question 229 / round-4 defect 4" -- a body with no `quat`
// falls back to identity, never throws). `ModelEntity.update(t)` below calls whatever
// `orientation(t, out)`-shaped object it was given (a real `BodyInterp` or
// `QuaternionTrackInterp`-`.at()`-shaped source, see `wrapAttitudeAt` below) and
// trusts THAT object's own guard -- the identical discipline `web/js/frames.js`'s
// `FrameNode` already applies to the same interp classes. The ADDITIONAL guard this
// file owns is one level up: a `ModelEntity` constructed with NO attitude source at
// all (not even a `BodyInterp` wrapping a body with no `quat`) also degrades to
// identity, every frame, rather than leaving `group.quaternion` at whatever it was
// last set to -- see `update()`'s own comment.
import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

/** Adapts `QuaternionTrackInterp`'s `.at(t, out)` method name to the
 * `orientation(t, out)` name `ModelEntity` expects (the same name `BodyInterp` already
 * uses) -- a thin rename, not a reimplementation of either class's interpolation
 * arithmetic (`web/js/interp.js` owns that, unchanged, unimported-from-twice). Useful
 * when a model's attitude is a real recorded quaternion track
 * (`altavista/model.py`'s `Trajectory.attitude`, M5.2) rather than a celestial body's
 * `BodyInterp`.
 * @param {{at:(t:number, out:any)=>any}} quaternionTrackInterp
 */
export function wrapAttitudeAt(quaternionTrackInterp) {
  return { orientation: (t, out) => quaternionTrackInterp.at(t, out) };
}

/** One glTF-model-bearing entity: a `THREE.Group` (the loaded glTF's own scene root)
 * whose quaternion is driven, once per `update(t)` call, from an injected attitude
 * source -- never a second, independent quaternion-interpolation implementation. */
export class ModelEntity {
  /**
   * @param {{id:string, sceneUnitsPerMetre:number, group?:THREE.Object3D, attitudeSource?:{orientation:Function}|null}} opts
   *   `sceneUnitsPerMetre` is REQUIRED (a finite number > 0): glTF 2.0 measures every
   *   linear distance in metres ("the units for all linear distances are meters"), and
   *   the scene's own unit is whatever the caller's scene says it is (the viewer's is
   *   1000 km, `web/js/scene.js`'s `SCALE`, so `1e-3 * SCALE` = 1e-6 per metre). This
   *   module cannot know that, and a default of 1 would be exactly the defect this
   *   argument exists to prevent (a 1.5 m model drawn 1,500 km across), so there is no
   *   default: a caller that omits it gets a `TypeError` at construction, not a
   *   wrong-sized model on screen. See `attachModel()` for where it is applied.
   *   `group` defaults to a fresh empty `THREE.Group` (a caller building a
   *   `ModelEntity` before a glTF asset resolves, e.g. while budgeted/deferred by
   *   `LayerManager`, can still register position/frame-graph parenting immediately
   *   and swap `group`'s children in once `attachModel()` resolves -- see that
   *   method's own comment).
   */
  constructor({ id, sceneUnitsPerMetre, group = new THREE.Group(), attitudeSource = null } = {}) {
    if (!(Number.isFinite(sceneUnitsPerMetre) && sceneUnitsPerMetre > 0)) {
      throw new TypeError(
        `ModelEntity '${id}': sceneUnitsPerMetre is required and must be a finite number > 0 `
        + `(glTF lengths are metres; the scene's own unit is the caller's to state), got ${sceneUnitsPerMetre}`,
      );
    }
    this.id = id;
    this.sceneUnitsPerMetre = sceneUnitsPerMetre;
    this.group = group;
    this._attitudeSource = attitudeSource;
    this.gltf = null;
    // True once a real glTF scene has been attached (`attachModel`) -- lets a caller
    // (or a headless check) distinguish "the model has not loaded yet, this is a
    // placeholder empty group" from "the model genuinely has zero attitude data", the
    // same "never silently substitute one meaning for another" discipline
    // `web/js/scene.js`'s own VVLH-fallback labelling already applies (see this file's
    // own module docstring / `web/js/interp.js`'s `QuaternionTrackInterp` doc comment).
    this.modelLoaded = false;
  }

  setAttitudeSource(source) { this._attitudeSource = source; }

  /** Replace `this.group`'s children with a loaded glTF's own scene graph, reparenting
   * the glTF scene's children directly into the STABLE `this.group` (rather than
   * swapping which object IS `this.group`) so anything that already parented itself
   * under `entity.group` (a frame-graph node, `web/js/frames.js`'s `reparent`) is
   * unaffected by a model attaching later -- the same "switching is re-parenting, not
   * re-loading" identity-preservation discipline `docs/architecture.md` sec 4 already
   * requires for frames, applied here to an entity's own render group.
   *
   * The glTF content is metres, so it goes under ONE inner node named
   * `entity-model-metres` whose uniform scale is `sceneUnitsPerMetre`, applied here once
   * and nowhere else; `this.group` itself keeps unit scale (it carries the attitude
   * quaternion, and it is what a caller positions and parents to, so a scale on it would
   * leak into every child and every position copied onto it). After this call the
   * group's world bounding box is the glTF's metre bounds times `sceneUnitsPerMetre`.
   *
   * Materials are drawn two-sided (`doubleSided`, default true), whatever the glTF says.
   * A glTF material is single-sided unless it declares `doubleSided`, and a single-sided
   * mesh is back-face culled: an asset that is open (a shell, a thin plate, a panel) or
   * wound inside out is then invisible from half the directions the camera can take, and
   * which half follows the spacecraft's attitude. Measured on the committed fixture
   * (`web/js/fixtures/entity_model_fixture.gltf`: three triangles at a corner, wound so
   * their normals point into the corner): framed from the usual side, zero of its roughly
   * 10,800 projected pixels were drawn single-sided. The cost of two-sided drawing for a
   * single spacecraft model is negligible, so a viewer that has to show whatever model a
   * run declares draws it from every side; pass `doubleSided: false` to keep each
   * material's own `side`.
   * @param {{scene: THREE.Object3D}} gltf a GLTFLoader onLoad result
   * @param {{doubleSided?: boolean}} [opts]
   */
  attachModel(gltf, { doubleSided = true } = {}) {
    this.gltf = gltf;
    while (this.group.children.length) this.group.remove(this.group.children[0]);
    const metres = new THREE.Group();
    metres.name = 'entity-model-metres';
    metres.scale.setScalar(this.sceneUnitsPerMetre);
    for (const child of gltf.scene.children.slice()) metres.add(child);
    if (doubleSided) {
      metres.traverse((obj) => {
        if (!obj.isMesh || !obj.material) return;
        for (const material of Array.isArray(obj.material) ? obj.material : [obj.material]) {
          material.side = THREE.DoubleSide;
          material.needsUpdate = true;
        }
      });
    }
    this.group.add(metres);
    this.modelLoaded = true;
    return this.group;
  }

  /** Set `this.group.quaternion` for epoch `t`. Degrades to the identity orientation
   * (never throws, never leaves a stale quaternion) both when NO attitude source was
   * ever given, and -- one level down, unchanged -- whenever the given source's own
   * `orientation(t, out)` itself degrades to identity (a `BodyInterp` whose body has
   * no `quat`, see this module's own doc comment). Returns `this.group` for chaining,
   * matching `BodyInterp.position`/`.orientation`'s own "return out" convention. */
  update(t) {
    if (this._attitudeSource && typeof this._attitudeSource.orientation === 'function') {
      this._attitudeSource.orientation(t, this.group.quaternion);
    } else {
      this.group.quaternion.identity();
    }
    return this.group;
  }
}

/** Promise-ify `GLTFLoader.parse` -- the one place this module calls into the vendored
 * loader. `data` is an `ArrayBuffer` (binary .glb) or a JSON string/object (text
 * .gltf, embedded buffers) -- exactly what `GLTFLoader.parse(data, path, onLoad,
 * onError)` itself accepts (web/vendor/three/addons/loaders/GLTFLoader.js, `parse()`'s
 * own branch on `data` being an ArrayBuffer vs. text, near its own KHR_BINARY_GLTF
 * check). `path` is the base path glTF's own relative URIs (buffers/images) resolve
 * against -- irrelevant for an embedded-buffer asset (this task's own fixture, see
 * web/js/entities_model_check.mjs) but required by every non-embedded real asset.
 * @param {GLTFLoader} loader
 * @param {ArrayBuffer|string|object} data
 * @param {string} [path]
 * @returns {Promise<object>} the GLTFLoader onLoad result ({scene, scenes, animations, ...})
 */
export function parseGLTFAsset(loader, data, path = '') {
  return new Promise((resolve, reject) => {
    loader.parse(data, path, resolve, reject);
  });
}

/** Convenience: parse a glTF asset and build a `ModelEntity` from it in one call.
 * `sceneUnitsPerMetre` is required, exactly as for `ModelEntity`'s constructor.
 * @param {{id:string, sceneUnitsPerMetre:number, data:ArrayBuffer|string|object, path?:string, loader?:GLTFLoader, attitudeSource?:{orientation:Function}|null}} opts
 * @returns {Promise<ModelEntity>}
 */
export async function createModelEntityFromGLTF({
  id, sceneUnitsPerMetre, data, path = '', loader = new GLTFLoader(), attitudeSource = null,
}) {
  const entity = new ModelEntity({ id, sceneUnitsPerMetre, attitudeSource });
  const gltf = await parseGLTFAsset(loader, data, path);
  entity.attachModel(gltf);
  return entity;
}

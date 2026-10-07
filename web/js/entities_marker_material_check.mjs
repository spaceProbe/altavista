// CLI harness for tests/test_entities_marker_material.py: `node web/js/entities_marker_material_check.mjs`.
//
// The screen-space entity marker (web/js/entities/entities_instanced_layer.js,
// `createScreenSpaceMarkerMaterial`) is a vertex-shader rewrite of three's MeshBasicMaterial. Under
// node there is no GL context, so what is proved here is everything around the shader that a
// browser drive would only show as "the marker vanished": the rewrite applies to the vendored
// three's real `basic` vertex shader and refuses (rather than silently doing nothing) when that
// shader no longer has the chunk it replaces; the viewport size the shader divides by is read
// from the renderer that is about to draw; the instanced mesh the residency scene builds carries
// the material, the per-draw hook and `frustumCulled = false`; and the reason for that last one
// is demonstrated on three's own `Frustum` (an `InstancedMesh` caches one bounding sphere the
// first time it is tested, so an instance that has moved since is culled while it is in view). The
// pixel size itself is measured in real Chrome by tests/test_viewer_entity_framing_browser.py.
import * as THREE from 'three';
import { LayerManager } from './layers/index.js';
import {
  MarkerLayerAdapter, TrailLayerAdapter, ResidentEntityScene,
  createScreenSpaceMarkerMaterial, bindMarkerViewport, ENTITY_MARKER_RADIUS_PX,
} from './entities/entities_instanced_layer.js';

const checks = [];
function check(name, pass, detail) { checks.push({ name, pass: !!pass, detail }); }
function throws(fn) { try { fn(); return false; } catch { return true; } }

// ---- the material, and its shader rewrite against the real vendored basic vertex shader
const material = createScreenSpaceMarkerMaterial({ radiusPx: 5 });
check('material_isMeshBasicWithInstanceColours', material.isMeshBasicMaterial === true && material.vertexColors === true, { type: material.type });
check('material_defaultRadiusConstant', ENTITY_MARKER_RADIUS_PX === 5 && createScreenSpaceMarkerMaterial().userData.markerUniforms.uMarkerRadiusPx.value === 5, { ENTITY_MARKER_RADIUS_PX });
check('material_badRadiusRefused', [0, -1, NaN, Infinity].every((r) => throws(() => createScreenSpaceMarkerMaterial({ radiusPx: r }))), {});

const basic = THREE.ShaderLib.basic;
check('shaderLib_basicStillHasTheProjectVertexChunk', basic.vertexShader.includes('#include <project_vertex>'), {});
const shader = { vertexShader: basic.vertexShader, uniforms: {} };
material.onBeforeCompile(shader);
const v = shader.vertexShader;
check('shader_projectVertexChunkReplaced', !v.includes('#include <project_vertex>'), {});
check('shader_declaresBothUniforms', v.includes('uniform float uMarkerRadiusPx;') && v.includes('uniform vec2 uViewportPx;'), {});
check('shader_projectsTheInstanceCentreOnly', v.includes('vec4 mvPosition = vec4( 0.0, 0.0, 0.0, 1.0 );') && v.includes('mvPosition = instanceMatrix * mvPosition;'), {});
check('shader_offsetIsPixelsOverViewportTimesW', v.includes('gl_Position.xy += transformed.xy * ( uMarkerRadiusPx * 2.0 / uViewportPx ) * gl_Position.w;'), {});
check('shader_uniformsAreTheSharedObjects',
  shader.uniforms.uMarkerRadiusPx === material.userData.markerUniforms.uMarkerRadiusPx
  && shader.uniforms.uViewportPx === material.userData.markerUniforms.uViewportPx, {});
check('shader_stillIncludesTheLogDepthChunk', v.includes('#include <logdepthbuf_vertex>'), {});
check('shader_refusesAVertexShaderWithoutTheChunk', throws(() => material.onBeforeCompile({ vertexShader: 'void main() {}', uniforms: {} })), {});
check('material_ownProgramCacheKey', material.customProgramCacheKey() !== new THREE.MeshBasicMaterial().customProgramCacheKey(), { key: material.customProgramCacheKey() });

// ---- the viewport the shader divides by comes from the renderer about to draw
const fakeRenderer = (w, h) => ({ getViewport: (t) => t.set(0, 0, w, h) });
bindMarkerViewport(material, fakeRenderer(842, 595));
const u = material.userData.markerUniforms.uViewportPx.value;
check('viewport_setFromThePrimaryRenderer', u.x === 842 && u.y === 595, { x: u.x, y: u.y });
bindMarkerViewport(material, fakeRenderer(400, 300));
check('viewport_setFromAnotherRenderer_sameMaterial', u.x === 400 && u.y === 300, { x: u.x, y: u.y });
bindMarkerViewport(material, fakeRenderer(0, 0));
check('viewport_neverZero', u.x === 1 && u.y === 1, { x: u.x, y: u.y });

// ---- the residency scene's mesh
const manager = new LayerManager({ memoryBudgetBytes: 1 << 20, maxConcurrentLoads: 4, now: () => 0 });
manager.addLayer(new MarkerLayerAdapter({ id: 'entity-markers' }));
manager.addLayer(new TrailLayerAdapter({ id: 'entity-trails' }));
const group = new THREE.Group();
const scene = new ResidentEntityScene({
  manager, markerLayerId: 'entity-markers', trailLayerId: 'entity-trails', markerGroup: group, trailGroup: new THREE.Group(),
  markerGeometry: new THREE.CircleGeometry(1, 24), trailMaxPoints: 4,
  entities: [{ name: 'A', positionKm: [0, 0, 0], color: '#ff0000', trailPointsKm: [] }], markerRadiusPx: 7,
});
const mesh = scene.markerMesh;
check('scene_meshIsInstancedWithTheScreenSpaceMaterial', mesh.isInstancedMesh === true && mesh.material.userData.markerUniforms !== undefined, {});
check('scene_radiusOptionReachesTheShader', mesh.material.userData.markerUniforms.uMarkerRadiusPx.value === 7, {});
check('scene_frustumCullingOff', mesh.frustumCulled === false, {});
mesh.onBeforeRender(fakeRenderer(640, 480));
const mu = mesh.material.userData.markerUniforms.uViewportPx.value;
check('scene_onBeforeRenderBindsTheDrawingRenderer', mu.x === 640 && mu.y === 480, { x: mu.x, y: mu.y });
scene.dispose();

// ---- why frustumCulled is off: three caches ONE bounding sphere for an InstancedMesh, the first
// time the mesh is frustum-tested, and never recomputes it. The viewer moves every instance every
// tick (floating-origin rebases shift all render-space coordinates by up to ~7 scene units), so
// a sphere cached while the instance was somewhere else culls the mesh while the instance is
// in view. Reproduced on three's own Frustum: first tested with the instance far off to the
// side (culled, correctly), then the instance is moved to the centre of the view.
const stale = new THREE.InstancedMesh(new THREE.CircleGeometry(1, 8), new THREE.MeshBasicMaterial(), 1);
const cam = new THREE.PerspectiveCamera(45, 1, 0.1, 100);
cam.position.set(0, 0, 5); cam.updateMatrixWorld(true);
const frustum = new THREE.Frustum().setFromProjectionMatrix(new THREE.Matrix4().multiplyMatrices(cam.projectionMatrix, cam.matrixWorldInverse));
stale.setMatrixAt(0, new THREE.Matrix4().makeTranslation(100, 0, 0));
const culledWhileFarOff = !frustum.intersectsObject(stale);
stale.setMatrixAt(0, new THREE.Matrix4().makeTranslation(0, 0, 0)); // now dead centre, 5 units in front of the camera
stale.instanceMatrix.needsUpdate = true;
const culledAfterMoveToCentre = !frustum.intersectsObject(stale);
check('hazard_movedInstanceStaysCulledByTheStaleBoundingSphere', culledWhileFarOff && culledAfterMoveToCentre, { culledWhileFarOff, culledAfterMoveToCentre });
stale.computeBoundingSphere();
check('hazard_recomputingTheSphereWouldHaveShownIt', frustum.intersectsObject(stale) === true, {});

const allPass = checks.every((c) => c.pass);
process.stdout.write(JSON.stringify({ allPass, checks }));
if (!allPass) process.exitCode = 1;

// Barrel export for web/js/entities/ -- H6 (docs/heavy-plan.md), "the entity module".
// A caller outside this directory imports from here, never reaching into an
// individual file directly -- the same convention web/js/layers/index.js already
// establishes for its own directory (see that file's own module docstring).
export {
  covarianceEllipsoid, symmetricEigen3, quaternionFromAxes, positionCovarianceBlock, CovarianceShapeError,
} from './covariance_ellipsoid.js';
export { keepOutVolumeFromCovariance, pointInsideKeepOut } from './keepout_volume.js';
export { buildEllipsoidMesh, worldSemiAxesKm } from './ellipsoid_mesh.js';
export {
  ModelEntity, wrapAttitudeAt, parseGLTFAsset, createModelEntityFromGLTF,
} from './model_entity.js';
export {
  MarkerLayerAdapter, TrailLayerAdapter, MARKER_INSTANCE_BYTES, TRAIL_POINT_BYTES,
  TRAIL_FIXED_OVERHEAD_BYTES, buildMarkerInstancedMesh, buildTrailGroup,
} from './entities_instanced_layer.js';

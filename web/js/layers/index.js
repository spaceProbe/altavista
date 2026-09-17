// Barrel export for web/js/layers/ -- the streaming-layer module (docs/heavy-plan.md
// H5). A caller outside this directory that wants the layer manager or any adapter
// imports from here, never reaching into an individual file directly -- the concrete
// form of "no caller outside web/js/layers/ has to know which of the three it is
// talking to" for *this* module's own public surface (see layer.js's module
// docstring for the interface itself).
export { LayerManager, comparePriority, globalKeyFor } from './layer.js';
export { ImageryLayerAdapter, IMAGERY_TILE_BYTES } from './imagery_layer.js';
export { TerrainLayerAdapter, TerrainLoaderNotImplementedError, TERRAIN_MESH_BYTES } from './terrain_layer.js';
export { Tiles3DLayerAdapter, DEFAULT_TILE3D_BYTES } from './tiles3d_layer.js';

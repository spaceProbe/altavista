//! P3a (`docs/heavy-plan.md` H3, 3D Tiles half; this round's open item 5, second bullet):
//! the 3D Tiles output `crate::tiler::TilerExecutor` writes for `output == "tiles3d"` -- a
//! real `tileset.json` (3D Tiles 1.0) whose leaf/interior nodes each carry one `.pnts`
//! (binary point cloud) content tile, geo-referenced by a real `root.transform`.
//!
//! # Why a point cloud, not a mesh (`.b3dm`)
//!
//! `heavy.proto`'s own `TileSetKind::TILE_SET_KIND_TILES3D` doc comment already commits this
//! track to "a `tileset.json` root plus `.pnts` point-cloud tiles" -- this module keeps that
//! commitment rather than inventing a second shape. The reason, independent of that existing
//! text: a `.b3dm` (batched 3D model) tile's content is a glTF binary, which would mean this
//! crate writing a second, from-scratch binary geometry encoder (`crate::png`'s own module
//! doc already draws the line at "no second image format is what this task is about" for a
//! *much* smaller format than glTF; a glTF encoder is a materially larger, and materially
//! more failure-prone, undertaking within one round). `.pnts` is the smaller, fully-
//! specifiable target the task brief names: a 28-byte fixed header, a feature-table JSON
//! chunk, and a feature-table binary chunk -- three primitives this module can build and
//! document exactly, the same "hand-rolled, standards-conformant, no ambiguity" bar
//! `crate::png` already holds itself to for PNG.
//!
//! # `.pnts` layout (Cesium 3D Tiles 1.0 point cloud; byte for byte)
//!
//! ```text
//! offset  size  field                          value
//! 0       4     magic                           "pnts" (ASCII)
//! 4       4     version                         u32 LE, = 1
//! 8       4     byteLength                       u32 LE, this tile's total byte length
//! 12      4     featureTableJSONByteLength       u32 LE
//! 16      4     featureTableBinaryByteLength     u32 LE
//! 20      4     batchTableJSONByteLength         u32 LE, = 0 (no batch table this round)
//! 24      4     batchTableBinaryByteLength       u32 LE, = 0
//! 28      *     featureTableJSON                 UTF-8 JSON, padded with trailing 0x20
//!                                                 (space) bytes to a 4-byte boundary -- the
//!                                                 same chunk-padding convention glTF's own
//!                                                 binary container uses for its JSON chunk,
//!                                                 reused here rather than invented, so the
//!                                                 byte immediately after this chunk always
//!                                                 starts 4-byte aligned.
//! 28+len featureTableBinary                       POSITION then RGB, tightly packed (see
//!                                                 below); no padding between the two.
//! ```
//!
//! `byteLength` (offset 8) is redundant with the file's own actual length (every reader can
//! compute it from the bytes it has), but is part of the spec's fixed header and is filled in
//! honestly here, not left zero.
//!
//! Feature table JSON: exactly `{"POINTS_LENGTH": N, "POSITION": {"byteOffset": 0}, "RGB":
//! {"byteOffset": N*12}}` -- `POINTS_LENGTH` is the point count; `POSITION` (per the 3D Tiles
//! Feature Table spec's own semantic table, no `componentType`/`type` override needed) is
//! implicitly `VEC3` of `FLOAT`, three little-endian `f32`s per point; `RGB` (same spec table)
//! is implicitly `VEC3` of `UNSIGNED_BYTE`, three bytes per point. Encoded via `serde_json`
//! (already resolved in this workspace's `Cargo.lock` -- see `Cargo.toml`'s own dependency
//! comment for why this crate did not carry it before this round) rather than a hand-rolled
//! JSON writer: `serde_json::Map`'s default (non-`preserve_order`) backing is a `BTreeMap`,
//! so key order in the encoded object is always the same, deterministic, sorted order for the
//! same key set -- checked directly against this workspace's own `Cargo.lock` (no
//! `indexmap` entry anywhere in `serde_json`'s own dependency list, meaning the
//! `preserve_order` feature is off, workspace-wide) rather than assumed.
//!
//! Feature table binary: `N` positions (each 3 little-endian `f32`s, tile-LOCAL metres --
//! see "Geo-referencing" below) immediately followed by `N` RGB triples (each 3 raw bytes,
//! the exact `[r, g, b]` `crate::tiler::sample_nearest` already returns for the same tile-
//! local pixel).
//!
//! # `tileset.json` shape
//!
//! Standard 3D Tiles 1.0 JSON: `{"asset": {"version": "1.0"}, "geometricError": ..., "extras":
//! {"geoReference": {...}}, "root": {...}}`. Every tile node (root included) is
//! `{"boundingVolume": {"region": [west, south, east, north, minHeight, maxHeight] (radians/
//! metres, the exact 6-number shape `web/js/tiles_layer.js::parseTileset3D` reads)},
//! "geometricError": ..., "refine": "REPLACE", "content": {"uri": ...}, "children": [...]}` --
//! `"refine"` is written explicitly on every node (root included) rather than relied on to be
//! inherited from an ancestor, since this crate's own consuming parser
//! (`web/js/tiles_layer.js::parseTileset3D`) reads `tileJson.refine` per node with no
//! inheritance fallback of its own, and an explicit value is legal per spec regardless. The
//! synthetic grouping root (see "Tree shape" below) omits `"content"` -- a content-less tile
//! that exists only to group its children is standard 3D Tiles, not a workaround.
//!
//! Every `content.uri` is that tile's own `TileEntry.object_key` -- the manifest's own
//! backend-independent, content-addressed key (`crate::runner::content_addressed_key`), the
//! same value every other `TileSetKind` already uses for exactly this reason (this crate's
//! own module doc on `tiler`, "The manifest-vs-sink ordering constraint"). This module invents
//! no second addressing scheme for 3D Tiles content.
//!
//! # Geo-referencing: `root.transform`
//!
//! Per the 3D Tiles spec (and exactly the convention `web/fixtures/gen_3dtiles_fixture.py`
//! and `web/js/tiles_layer.js::ecefFromRootTransform`/`enuBasisFromRootTransform` already
//! establish for this codebase's *other* 3D Tiles fixture), `root.transform` is a 16-number,
//! column-major 4x4 matrix: columns 0-2 (indices 0,1,2 / 4,5,6 / 8,9,10) are the anchor
//! point's East/North/Up unit basis vectors in ECEF, and column 3 (indices 12,13,14) is the
//! anchor point's own ECEF position in metres; index 15 is always `1.0`. [`geodetic_to_ecef`]
//! and [`enu_basis`] below are the exact same closed-form WGS84 formulas
//! `web/js/globe_lod.js::geodeticToEcef` and `web/fixtures/gen_3dtiles_fixture.py::
//! enu_basis` already implement (same `WGS84_A_M`/`WGS84_B_M` constants) -- this is this
//! crate's own third independent implementation of that one formula (Rust, alongside the
//! existing JS and the fixture-authoring Python), the same "duplicated, deliberately, across
//! independently-owned language boundaries" reasoning `crate::scheme`'s own module doc
//! already gives for the tile-layout arithmetic, not a new pattern this module invents.
//!
//! **The anchor point** is this tile set's own source raster bounds' geometric centre
//! (`(west+east)/2, (south+north)/2`, height 0) -- a single, deterministic point derivable
//! from the same input every other tile in this run is built from, with no additional
//! parameter this task's `JobSpec.parameters` would need to carry. Every point in every
//! content tile is expressed in this one anchor's local East-North-Up frame: for a point at
//! ECEF `p`, its local coordinates are `(dot(p - anchor, east), dot(p - anchor, north),
//! dot(p - anchor, up))`. This is an exact, distance-independent change of basis (a rotation
//! plus a translation, not a small-region tangent-plane approximation) -- `p` is recovered
//! exactly as `anchor + local.x*east + local.y*north + local.z*up`, for any `p` on the
//! ellipsoid, however far from the anchor. `tileset.extras.geoReference` records the anchor's
//! own `lonDeg`/`latDeg`/`heightM`, the same field shape
//! `web/fixtures/gen_3dtiles_fixture.py`'s own fixture already uses, so a reader (or this
//! crate's own anchored Python test) can recompute `root.transform` from that recorded point
//! and confirm they agree, exactly the cross-check
//! `web/js/tiles3d_geo_check.mjs` already performs against the *other* fixture in this
//! codebase.
//!
//! # Tree shape
//!
//! 3D Tiles requires exactly one `root` tile object; this crate's own tiling scheme
//! (`crate::scheme`) has TWO tiles at level 0 (a whole globe is two side-by-side
//! hemispheres, `crate::scheme`'s own module doc) -- so `tileset.root` is always a synthetic,
//! content-less grouping node whose `children` are whichever `min_level` tiles
//! `crate::scheme::tiles_covering` actually returns for this run's raster (one node when the
//! raster only spans one of the two level-0 tiles, two when it spans both; never hard-coded
//! to two). Its own `boundingVolume.region` is the union of those children's own regions
//! (each `crate::scheme` level nests exactly -- a level-L tile's bounds are, by the scheme's
//! own halving arithmetic, exactly the union of its four level-(L+1) children's bounds -- so
//! computing a parent's region as that union is exact, not approximate), and its
//! `geometricError` is `2 *` the `min_level` tiles' own (shared) `geometricError`, always
//! larger than every descendant's (later levels' `geometricError` only ever shrinks -- see
//! [`geometric_error_for_level`]), a safe, disclosed margin rather than a tight bound this
//! grouping node does not need. Every level from `min_level` to `max_level` gets its own
//! content tile (mirroring `output == "imagery"`/`"terrain"`, which likewise store every
//! level, not leaves only) -- a level-L node's `children` are whichever of its four
//! `crate::scheme`-defined children (`(level+1, 2x, 2y)`, `(2x+1, 2y)`, `(2x, 2y+1)`,
//! `(2x+1, 2y+1)`, the identical fixed south-row-before-north-row, west-before-east order
//! `web/js/globe_lod.js::tileChildren` uses) are themselves present in this run's own
//! `max_level`-bounded tile set (a raster whose bounds cut across a tile's middle can leave
//! some, not all, of its four children overlapping the raster -- `crate::scheme::
//! tiles_covering`'s own documented behaviour -- so this module checks presence rather than
//! assuming all four).
//!
//! # Determinism
//!
//! Every geometric input (the anchor point, each tile's bounds, each sampled point's
//! position/colour) is a pure function of the source raster's bytes and `JobSpec.parameters`
//! -- the identical determinism argument `crate::tiler`'s own module doc already makes for
//! imagery, extended here to `f64`/`f32` WGS84 arithmetic, which is exact-per-IEEE754 and
//! therefore identical across processes and platforms this workspace targets (no
//! transcendental function is used at library-version-dependent precision beyond what
//! `crate::scheme`'s own already-shipped arithmetic already relies on `f64` `sin`/`cos`/
//! `sqrt`/`atan2` for). `serde_json`'s own object-key ordering is deterministic for a fixed
//! key set (see above) and its number formatting (`ryu`, the shortest round-trippable
//! representation) is a pure function of the `f64` value, not of the local system's own
//! settings.

use av_cdm::pb;

use crate::runner::content_addressed_key;
use crate::scheme::{tile_bounds_deg, tile_count_x, tiles_covering, BoundsDeg, Tile};

/// WGS84 equatorial (semi-major) radius, metres -- identical value to
/// `web/js/globe_lod.js::WGS84_A_M` and `web/fixtures/gen_3dtiles_fixture.py::WGS84_A_M`.
pub const WGS84_A_M: f64 = 6_378_137.0;
/// WGS84 polar (semi-minor) radius, metres -- identical value to `web/js/globe_lod.js::
/// WGS84_B_M` and `web/fixtures/gen_3dtiles_fixture.py::WGS84_B_M`.
pub const WGS84_B_M: f64 = 6_356_752.314245;

fn wgs84_e2() -> f64 {
    1.0 - (WGS84_B_M * WGS84_B_M) / (WGS84_A_M * WGS84_A_M)
}

/// WGS84 geodetic (degrees, degrees, metres above the ellipsoid) -> ECEF metres -- the exact
/// closed-form formula `web/js/globe_lod.js::geodeticToEcef` implements (see this module's
/// own doc, "Geo-referencing").
pub fn geodetic_to_ecef(lon_deg: f64, lat_deg: f64, height_m: f64) -> (f64, f64, f64) {
    let e2 = wgs84_e2();
    let lon = lon_deg.to_radians();
    let lat = lat_deg.to_radians();
    let (sin_lat, cos_lat) = (lat.sin(), lat.cos());
    let n = WGS84_A_M / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let x = (n + height_m) * cos_lat * lon.cos();
    let y = (n + height_m) * cos_lat * lon.sin();
    let z = (n * (1.0 - e2) + height_m) * sin_lat;
    (x, y, z)
}

/// East/North/Up unit basis vectors (ECEF) at a geodetic point -- the exact formula
/// `web/fixtures/gen_3dtiles_fixture.py::enu_basis` implements (see this module's own doc).
/// Returns `(east, north, up)`, each `[x, y, z]`.
pub fn enu_basis(lon_deg: f64, lat_deg: f64) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let lon = lon_deg.to_radians();
    let lat = lat_deg.to_radians();
    let (sin_lat, cos_lat) = (lat.sin(), lat.cos());
    let (sin_lon, cos_lon) = (lon.sin(), lon.cos());
    let east = [-sin_lon, cos_lon, 0.0];
    let north = [-sin_lat * cos_lon, -sin_lat * sin_lon, cos_lat];
    let up = [cos_lat * cos_lon, cos_lat * sin_lon, sin_lat];
    (east, north, up)
}

/// The 16-number, column-major `root.transform` matrix anchored at `(lon_deg, lat_deg,
/// height_m)` -- see this module's own doc, "Geo-referencing", for the exact column layout.
pub fn root_transform_matrix(lon_deg: f64, lat_deg: f64, height_m: f64) -> [f64; 16] {
    let (ox, oy, oz) = geodetic_to_ecef(lon_deg, lat_deg, height_m);
    let (east, north, up) = enu_basis(lon_deg, lat_deg);
    [
        east[0], east[1], east[2], 0.0, //
        north[0], north[1], north[2], 0.0, //
        up[0], up[1], up[2], 0.0, //
        ox, oy, oz, 1.0,
    ]
}

/// Projects ECEF point `p` into the local East-North-Up frame anchored at `anchor_ecef` with
/// basis `(east, north, up)` -- see this module's own doc, "Geo-referencing", for why this is
/// an exact change of basis, not a small-region approximation.
pub fn ecef_to_local_enu(p: (f64, f64, f64), anchor_ecef: (f64, f64, f64), east: [f64; 3], north: [f64; 3], up: [f64; 3]) -> (f64, f64, f64) {
    let d = [p.0 - anchor_ecef.0, p.1 - anchor_ecef.1, p.2 - anchor_ecef.2];
    let dot = |v: [f64; 3]| d[0] * v[0] + d[1] * v[1] + d[2] * v[2];
    (dot(east), dot(north), dot(up))
}

/// Media type for a `.pnts` content tile this module writes -- an AltaVista-owned `vnd`
/// media-type string naming a real, spec-conformant Cesium 3D Tiles 1.0 point cloud (this
/// crate did not invent the `.pnts` binary layout, only this descriptive string for it --
/// the same "our own vnd string for a real format" convention `crate::raster::MAGIC`'s own
/// module doc already uses for the AVRASTER input format).
pub const PNTS_TILE_MEDIA_TYPE: &str = "application/vnd.altavista.tiles3d-pnts+bin";
/// Media type for the `tileset.json` output this module writes -- the ordinary, widely used
/// media type for JSON text.
pub const TILESET_JSON_MEDIA_TYPE: &str = "application/json";

const PNTS_HEADER_LEN: usize = 28;

/// Encodes one `.pnts` content tile from `positions_local_m` (tile-local ENU metres, one
/// `[x, y, z]` per point) and `colors_rgb` (one `[r, g, b]` per point, same length, same
/// point order) -- see this module's own doc for the exact byte layout.
fn encode_pnts(positions_local_m: &[[f32; 3]], colors_rgb: &[[u8; 3]]) -> Vec<u8> {
    debug_assert_eq!(positions_local_m.len(), colors_rgb.len(), "encode_pnts: one color per position");
    let points_length = positions_local_m.len() as u64;
    let position_byte_offset: u64 = 0;
    let position_bytes = points_length * 12;
    let rgb_byte_offset = position_bytes;

    let feature_table_json = serde_json::json!({
        "POINTS_LENGTH": points_length,
        "POSITION": {"byteOffset": position_byte_offset},
        "RGB": {"byteOffset": rgb_byte_offset},
    });
    let mut ft_json_bytes = serde_json::to_vec(&feature_table_json).expect("a serde_json::Value built from only strings/numbers always encodes");
    // Pad to a 4-byte boundary with trailing spaces (0x20) -- the same JSON-chunk padding
    // convention glTF's own binary container uses (this module's own doc).
    while !ft_json_bytes.len().is_multiple_of(4) {
        ft_json_bytes.push(0x20);
    }

    let mut ft_bin = Vec::with_capacity((position_bytes + points_length * 3) as usize);
    for p in positions_local_m {
        for c in p {
            ft_bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for c in colors_rgb {
        ft_bin.extend_from_slice(c);
    }

    let byte_length = PNTS_HEADER_LEN as u64 + ft_json_bytes.len() as u64 + ft_bin.len() as u64;
    let mut out = Vec::with_capacity(byte_length as usize);
    out.extend_from_slice(b"pnts");
    out.extend_from_slice(&1u32.to_le_bytes()); // version
    out.extend_from_slice(&(byte_length as u32).to_le_bytes());
    out.extend_from_slice(&(ft_json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&(ft_bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // batch table JSON byte length
    out.extend_from_slice(&0u32.to_le_bytes()); // batch table binary byte length
    out.extend_from_slice(&ft_json_bytes);
    out.extend_from_slice(&ft_bin);
    out
}

/// `crate::tiler`'s own module-doc formula for `web/js/globe_lod.js`'s geometric-error
/// convention (halving ground distance per level), adapted to this tiler's own `tile_size`
/// (points per tile edge) instead of `web/js/globe_lod.js::geometricErrorAtLevel`'s
/// hard-coded 256-texel assumption -- our own point density varies with `tile_size`, so using
/// it here (rather than copying the JS's fixed constant) keeps `geometricError` an honest
/// measure of *this* tile set's own ground resolution per point.
pub fn geometric_error_for_level(level: u32, tile_size: u32) -> f64 {
    let equator_circumference_m = 2.0 * std::f64::consts::PI * WGS84_A_M;
    (equator_circumference_m / tile_count_x(level) as f64) / tile_size as f64
}

/// One content tile's worth of point-cloud bytes, built from `raster` at `tile`'s own bounds
/// -- `tile_size * tile_size` points, one per tile-local pixel centre (the identical
/// `(px, py)` grid and pixel-centre formula `crate::tiler::sample_nearest` already uses),
/// each point's colour the same `[r, g, b]` an imagery tile would show at that pixel, and
/// its position on the WGS84 ellipsoid surface (height 0) at that pixel's own geodetic
/// point, expressed in the anchor's local ENU frame (see this module's own doc). Seven
/// parameters (`raster`/`tile`/`tile_size` plus the anchor's own ECEF point and its three
/// ENU basis vectors) -- at clippy's default `too_many_arguments` threshold, not over it, so
/// no lint suppression is needed here.
pub fn render_tiles3d_tile(
    raster: &crate::raster::Raster,
    tile: Tile,
    tile_size: u32,
    anchor_ecef: (f64, f64, f64),
    east: [f64; 3],
    north: [f64; 3],
    up: [f64; 3],
) -> Vec<u8> {
    let bounds = tile_bounds_deg(tile);
    let n = (tile_size as usize) * (tile_size as usize);
    let mut positions = Vec::with_capacity(n);
    let mut colors = Vec::with_capacity(n);
    for py in 0..tile_size {
        for px in 0..tile_size {
            let rgb = crate::tiler::sample_nearest(raster, &bounds, tile_size, px, py);
            let (lon, lat) = crate::tiler::tile_pixel_center_lonlat(&bounds, tile_size, px, py);
            let ecef = geodetic_to_ecef(lon, lat, 0.0);
            let (lx, ly, lz) = ecef_to_local_enu(ecef, anchor_ecef, east, north, up);
            positions.push([lx as f32, ly as f32, lz as f32]);
            colors.push(rgb);
        }
    }
    encode_pnts(&positions, &colors)
}

/// `[west, south, east, north, minHeight, maxHeight]` radians/metres -- the exact 6-number
/// `region` bounding-volume shape `web/js/tiles_layer.js::parseTileset3D` reads
/// (`bv.region`). `minHeight`/`maxHeight` are both `0.0`: every point cloud this module
/// writes sits on the ellipsoid surface (height 0, this module's own doc).
fn tile_region_rad(bounds: &BoundsDeg) -> [f64; 6] {
    [bounds.west.to_radians(), bounds.south.to_radians(), bounds.east.to_radians(), bounds.north.to_radians(), 0.0, 0.0]
}

fn union_bounds(bounds_list: &[BoundsDeg]) -> BoundsDeg {
    let mut out = bounds_list[0];
    for b in &bounds_list[1..] {
        out.west = out.west.min(b.west);
        out.south = out.south.min(b.south);
        out.east = out.east.max(b.east);
        out.north = out.north.max(b.north);
    }
    out
}

/// One built content tile, keyed by address -- what [`build_tileset_json`] needs to look up a
/// tile's own `object_key` for `content.uri` and to test child presence.
pub struct Tiles3dEntry {
    pub tile: Tile,
    pub object_key: String,
}

/// Builds `tileset.json` (see this module's own doc for the exact shape) from `entries`
/// (every content tile this run built, any order -- looked up by address, not by list
/// position) and the anchor `(lon_deg, lat_deg, height_m)` used to build `root_transform`.
/// `levels` carries the subset of `crate::tiler::TilerParams`'s own fields this function
/// needs (this module does not depend on `crate::tiler`'s private `TilerParams` type) --
/// grouped into one struct, along with `anchor` as one `(lon_deg, lat_deg, height_m)` tuple,
/// so this function stays at clippy's default `too_many_arguments` budget rather than passing
/// six/seven independent scalars.
pub struct Tiles3dLevels {
    pub min_level: u32,
    pub max_level: u32,
    pub tile_size: u32,
}

pub fn build_tileset_json(raster_bounds: &BoundsDeg, levels: Tiles3dLevels, entries: &[Tiles3dEntry], anchor: (f64, f64, f64)) -> serde_json::Value {
    use std::collections::{BTreeMap, BTreeSet};
    let Tiles3dLevels { min_level, max_level, tile_size } = levels;
    let (anchor_lon_deg, anchor_lat_deg, anchor_height_m) = anchor;

    let mut object_key_by_addr: BTreeMap<(u32, u32, u32), &str> = BTreeMap::new();
    let mut present: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    for e in entries {
        let addr = (e.tile.level, e.tile.x, e.tile.y);
        object_key_by_addr.insert(addr, &e.object_key);
        present.insert(addr);
    }

    fn build_node(tile: Tile, tile_size: u32, max_level: u32, present: &BTreeSet<(u32, u32, u32)>, object_key_by_addr: &BTreeMap<(u32, u32, u32), &str>) -> serde_json::Value {
        let bounds = tile_bounds_deg(tile);
        let object_key = object_key_by_addr[&(tile.level, tile.x, tile.y)];
        let mut children_json: Vec<serde_json::Value> = Vec::new();
        if tile.level < max_level {
            let cl = tile.level + 1;
            let x0 = tile.x * 2;
            let y0 = tile.y * 2;
            // Fixed south-row-before-north-row, west-before-east order -- see this module's
            // own doc ("Tree shape").
            for (cx, cy) in [(x0, y0), (x0 + 1, y0), (x0, y0 + 1), (x0 + 1, y0 + 1)] {
                if present.contains(&(cl, cx, cy)) {
                    children_json.push(build_node(Tile { level: cl, x: cx, y: cy }, tile_size, max_level, present, object_key_by_addr));
                }
            }
        }
        let mut node = serde_json::json!({
            "boundingVolume": {"region": tile_region_rad(&bounds)},
            "geometricError": geometric_error_for_level(tile.level, tile_size),
            "refine": "REPLACE",
            "content": {"uri": object_key},
        });
        if !children_json.is_empty() {
            node.as_object_mut().unwrap().insert("children".to_string(), serde_json::Value::Array(children_json));
        }
        node
    }

    let min_level_tiles = tiles_covering(raster_bounds, min_level);
    assert!(!min_level_tiles.is_empty(), "build_tileset_json: no min_level tiles -- caller must not call this with an empty tile set");
    let root_children: Vec<serde_json::Value> = min_level_tiles.iter().map(|t| build_node(*t, tile_size, max_level, &present, &object_key_by_addr)).collect();
    let root_region = union_bounds(&min_level_tiles.iter().map(|t| tile_bounds_deg(*t)).collect::<Vec<_>>());
    let root_geometric_error = 2.0 * geometric_error_for_level(min_level, tile_size);
    let root_transform = root_transform_matrix(anchor_lon_deg, anchor_lat_deg, anchor_height_m);

    serde_json::json!({
        "asset": {"version": "1.0"},
        "geometricError": root_geometric_error,
        "extras": {
            "geoReference": {
                "lonDeg": anchor_lon_deg,
                "latDeg": anchor_lat_deg,
                "heightM": anchor_height_m,
            }
        },
        "root": {
            "boundingVolume": {"region": tile_region_rad(&root_region)},
            "geometricError": root_geometric_error,
            "refine": "REPLACE",
            "transform": root_transform.to_vec(),
            "children": root_children,
        }
    })
}

/// The content-addressed object key a `.pnts` tile's own bytes will be stored under -- a
/// thin wrapper so `crate::tiler` does not need to import `crate::runner::
/// content_addressed_key` itself just for this one call site.
pub fn object_key_for(prefix: &str, sha256_hex: &str) -> String {
    content_addressed_key(prefix, sha256_hex)
}

/// `pb::TileEntry` for one `.pnts` content tile -- mirrors `crate::tiler::
/// render_and_describe_tile`'s shape for imagery, so `TileSetManifest.tiles` carries the
/// identical field set regardless of `kind`.
pub fn describe_tile(tile: Tile, pnts_bytes: &[u8], key_prefix: &str) -> pb::TileEntry {
    let sha256_hex = crate::hash::hex_encode(&openssl::sha::sha256(pnts_bytes));
    let object_key = object_key_for(key_prefix, &sha256_hex);
    pb::TileEntry {
        level: tile.level,
        x: tile.x,
        y: tile.y,
        sha256: sha256_hex,
        size_bytes: pnts_bytes.len() as u64,
        uri: String::new(),
        media_type: PNTS_TILE_MEDIA_TYPE.to_string(),
        object_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geodetic_to_ecef_matches_a_hand_computed_equator_prime_meridian_point() {
        // lon=0, lat=0, height=0 -> ECEF (WGS84_A_M, 0, 0) exactly (N = a at the equator,
        // sin_lat = 0).
        let (x, y, z) = geodetic_to_ecef(0.0, 0.0, 0.0);
        assert!((x - WGS84_A_M).abs() < 1e-6);
        assert!(y.abs() < 1e-9);
        assert!(z.abs() < 1e-9);
    }

    #[test]
    fn geodetic_to_ecef_matches_a_hand_computed_north_pole_point() {
        // lat=90 -> ECEF (0, 0, N*(1-e2)) with N = a/sqrt(1 - e2) = a/sqrt(1-e2); at lat=90,
        // N*(1-e2) reduces to b (the semi-minor axis) exactly, a well-known WGS84 identity.
        let (x, y, z) = geodetic_to_ecef(0.0, 90.0, 0.0);
        assert!(x.abs() < 1e-6);
        assert!(y.abs() < 1e-6);
        assert!((z - WGS84_B_M).abs() < 1e-6);
    }

    #[test]
    fn enu_basis_is_orthonormal_at_an_arbitrary_point() {
        let (east, north, up) = enu_basis(-105.2705, 40.0150);
        let len = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!((len(east) - 1.0).abs() < 1e-12);
        assert!((len(north) - 1.0).abs() < 1e-12);
        assert!((len(up) - 1.0).abs() < 1e-12);
        assert!(dot(east, north).abs() < 1e-12);
        assert!(dot(east, up).abs() < 1e-12);
        assert!(dot(north, up).abs() < 1e-12);
    }

    #[test]
    fn ecef_to_local_enu_round_trips_exactly() {
        let anchor = geodetic_to_ecef(-105.2705, 40.0150, 1650.0);
        let (east, north, up) = enu_basis(-105.2705, 40.0150);
        let p = geodetic_to_ecef(-105.30, 40.05, 12.0); // a nearby point, arbitrary
        let (lx, ly, lz) = ecef_to_local_enu(p, anchor, east, north, up);
        let recovered = (anchor.0 + lx * east[0] + ly * north[0] + lz * up[0], anchor.1 + lx * east[1] + ly * north[1] + lz * up[1], anchor.2 + lx * east[2] + ly * north[2] + lz * up[2]);
        assert!((recovered.0 - p.0).abs() < 1e-6);
        assert!((recovered.1 - p.1).abs() < 1e-6);
        assert!((recovered.2 - p.2).abs() < 1e-6);
    }

    #[test]
    fn encode_pnts_produces_a_well_formed_header_and_chunks() {
        let positions = vec![[1.0f32, 2.0, 3.0], [4.0, 5.0, 6.0]];
        let colors = vec![[10u8, 20, 30], [40, 50, 60]];
        let bytes = encode_pnts(&positions, &colors);
        assert_eq!(&bytes[0..4], b"pnts");
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(version, 1);
        let byte_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        assert_eq!(byte_length, bytes.len());
        let ft_json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let ft_bin_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        assert_eq!(u32::from_le_bytes(bytes[20..24].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 0);
        assert_eq!(ft_json_len % 4, 0, "feature table JSON chunk must be padded to a 4-byte boundary");
        assert_eq!(28 + ft_json_len + ft_bin_len, byte_length);
        assert_eq!(ft_bin_len, 2 * 12 + 2 * 3);

        let ft_json_bytes = &bytes[28..28 + ft_json_len];
        let parsed: serde_json::Value = serde_json::from_slice(ft_json_bytes.trim_ascii_end()).unwrap();
        assert_eq!(parsed["POINTS_LENGTH"], 2);
        assert_eq!(parsed["POSITION"]["byteOffset"], 0);
        assert_eq!(parsed["RGB"]["byteOffset"], 24);

        let ft_bin = &bytes[28 + ft_json_len..];
        let x0 = f32::from_le_bytes(ft_bin[0..4].try_into().unwrap());
        assert_eq!(x0, 1.0);
        let rgb0 = &ft_bin[24..27];
        assert_eq!(rgb0, &[10, 20, 30]);
    }

    #[test]
    fn encoding_the_same_points_twice_is_byte_identical() {
        let positions = vec![[1.0f32, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        let colors = vec![[1u8, 2, 3], [4, 5, 6], [7, 8, 9]];
        let a = encode_pnts(&positions, &colors);
        let b = encode_pnts(&positions, &colors);
        assert_eq!(a, b);
    }
}

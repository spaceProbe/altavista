#!/usr/bin/env python3
"""Generate an offline 3D Tiles fixture for the vendored 3DTilesRendererJS overlay
layer (web/js/tiles_layer.js, M15.4 / M16.4).

The binding rule for this task is "no network at test time" -- the browser
verification loads this exact fixture, never a real tile server. Rather than commit a
downloaded sample tileset (an unclear-provenance binary blob), this script *generates*
one small, valid tileset from scratch: a small quad-subdivided region hierarchy (root
+ 3 levels of children, 1 + 4 + 16 + 64 = 85 tiles) whose content is the same tiny
glTF binary (.glb, one triangle) referenced by every node (3D Tiles 1.1 supports glTF
content without the older b3dm wrapper -- confirmed against the vendored renderer's own
content-type switch, web/vendor/3d-tiles-renderer/build/renderer-3xKvdklX.js, which
handles the "glb" extension via GLTFLoader directly, same as "gltf").

M16.4 (this revision): the single-tile M15.4 fixture had no `root.transform` at all --
only a `boundingVolume.region` -- so there was nothing for the viewer to geo-reference
against; M15.4's own escalation admitted the overlay used "a fixed demo transform, not
real geo-referencing against the frame graph". This revision adds:

  - A real `root.transform`: a 4x4 column-major matrix (3D Tiles spec convention,
    identical to how real-world geo-referenced tilesets such as Cesium ion's are
    anchored) built from a genuine WGS84 East-North-Up frame anchored at one geodetic
    point (ANCHOR_LON_DEG/ANCHOR_LAT_DEG/ANCHOR_HEIGHT_M below): its translation
    column is that point's ECEF position in metres (the closed-form geodetic->ECEF
    conversion, the exact same formula as web/js/globe_lod.js's `geodeticToEcef`,
    reimplemented here only because this is a one-time offline fixture-data-generation
    script, not tested arithmetic -- the JS side never re-derives these numbers, it
    reads this matrix and independently recomputes `geodeticToEcef(lonDeg, latDeg,
    heightM)` from the declared `extras.geoReference` fields to cross-check the two
    agree, see tests/test_viewer_globe.py's tiles-3D-geo-reference tests); its
    rotation columns are that point's East/North/Up unit basis vectors, also in ECEF.
    Per the 3D Tiles spec, the vendored TilesRenderer premultiplies every loaded
    tile's local matrix by this cumulative transform (checked directly against
    preprocessNode/parseTile in the pinned 0.5.2 source), so tile content loaded
    through `web/js/tiles_layer.js` ends up positioned in real ECEF metres without any
    second, hand-rolled transform in this codebase's own code.
  - `tileset.extras.geoReference`: the anchor point's lon/lat/height in the open
    `extras` field the 3D Tiles spec reserves for exactly this (application-specific
    metadata) -- an honest, inspectable trail from the authored transform back to the
    geodetic point it was derived from, not a hidden coincidence.
  - A real multi-level quadtree of regions (same NW/SW/SE/NE-style fixed subdivision
    order as web/js/globe_lod.js's `tileChildren`) so `web/js/tiles_layer.js`'s own
    screen-space-error tile selection has more than one tile to ever choose between,
    and a resident budget can be sized so eviction must actually run (this task's
    standing requirement) -- see web/js/tiles3d_check.mjs.

No external dependencies (no pygltflib, no PIL) -- the GLB container format is simple
enough to build with the stdlib's `struct`/`json`, and this keeps the generator itself
inside web/fixtures/ (this task's own file ownership) auditable in a few dozen lines.

Run: .venv/bin/python web/fixtures/gen_3dtiles_fixture.py
Output: web/fixtures/3dtiles/tileset.json, web/fixtures/3dtiles/tile.glb
"""
from __future__ import annotations

import json
import math
import struct
from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent / "3dtiles"

GLTF_MAGIC = 0x46546C67  # 'glTF'
CHUNK_TYPE_JSON = 0x4E4F534A
CHUNK_TYPE_BIN = 0x004E4942

# WGS84 ellipsoid constants -- identical values to web/js/globe_lod.js's
# WGS84_A_M/WGS84_B_M (the canonical implementation the viewer actually runs); kept
# here only to author this fixture's numbers, never imported by the viewer.
WGS84_A_M = 6378137.0
WGS84_B_M = 6356752.314245
WGS84_E2 = 1 - (WGS84_B_M * WGS84_B_M) / (WGS84_A_M * WGS84_A_M)

# Geodetic anchor point for the fixture's root.transform: an arbitrary but fixed,
# plausible real-world location (Boulder, Colorado) -- unlike M15.4's fixture, this
# location IS load-bearing now: it's what the geo-referencing tests check the
# transform against.
ANCHOR_LON_DEG = -105.2705
ANCHOR_LAT_DEG = 40.0150
ANCHOR_HEIGHT_M = 1650.0

# The root tile's footprint: a small patch (~4.4 km square) around the anchor point,
# quad-subdivided MAX_LEVEL times (1 + 4 + 16 + 64 = 85 tiles total) -- enough tiles
# for a real screen-space-error selection to have more than one option, and for a
# resident budget to force real eviction (see web/js/tiles3d_check.mjs).
HALF_DEG = 0.02
MAX_LEVEL = 3
ROOT_GEOMETRIC_ERROR = 400.0


def _pad(data: bytes, align: int, fill: bytes) -> bytes:
    rem = len(data) % align
    return data if rem == 0 else data + fill * (align - rem)


def build_triangle_glb() -> bytes:
    """One flat triangle (a stand-in "asset"), positions + uint16 indices, no
    material/texture (glTF's implicit default material is enough for a fixture whose
    entire point is proving the *loader* wires up, not testing shading)."""
    positions = struct.pack("<9f", 0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0, 0.0)
    indices = struct.pack("<3H", 0, 1, 2)
    bin_unpadded = positions + indices
    bin_chunk_data = _pad(bin_unpadded, 4, b"\x00")

    gltf_json = {
        "asset": {"version": "2.0", "generator": "web/fixtures/gen_3dtiles_fixture.py"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1, "mode": 4}]}],
        "accessors": [
            {
                "bufferView": 0, "byteOffset": 0, "componentType": 5126, "count": 3,
                "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [10.0, 10.0, 0.0],
            },
            {"bufferView": 1, "byteOffset": 0, "componentType": 5123, "count": 3, "type": "SCALAR"},
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": len(positions), "target": 34962},
            {"buffer": 0, "byteOffset": len(positions), "byteLength": len(indices), "target": 34963},
        ],
        "buffers": [{"byteLength": len(bin_unpadded)}],
    }
    json_chunk_data = _pad(json.dumps(gltf_json, separators=(",", ":")).encode("utf-8"), 4, b" ")

    json_chunk = struct.pack("<II", len(json_chunk_data), CHUNK_TYPE_JSON) + json_chunk_data
    bin_chunk = struct.pack("<II", len(bin_chunk_data), CHUNK_TYPE_BIN) + bin_chunk_data
    total_len = 12 + len(json_chunk) + len(bin_chunk)
    header = struct.pack("<III", GLTF_MAGIC, 2, total_len)
    return header + json_chunk + bin_chunk


def geodetic_to_ecef(lon_deg: float, lat_deg: float, height_m: float) -> tuple[float, float, float]:
    """Standard closed-form geodetic -> ECEF conversion, metres -- the same formula as
    web/js/globe_lod.js's `geodeticToEcef` (the viewer's one canonical implementation;
    this Python copy exists only to author fixture data, and is cross-checked by the
    JS side reading it back, see this file's module docstring)."""
    lon, lat = math.radians(lon_deg), math.radians(lat_deg)
    sin_lat, cos_lat = math.sin(lat), math.cos(lat)
    n = WGS84_A_M / math.sqrt(1 - WGS84_E2 * sin_lat * sin_lat)
    x = (n + height_m) * cos_lat * math.cos(lon)
    y = (n + height_m) * cos_lat * math.sin(lon)
    z = (n * (1 - WGS84_E2) + height_m) * sin_lat
    return x, y, z


def enu_basis(lon_deg: float, lat_deg: float) -> tuple[tuple[float, float, float], ...]:
    """East/North/Up unit vectors (ECEF) at a geodetic point -- the standard
    local-tangent-plane basis (Cesium's `eastNorthUpToFixedFrame`, Vermeille's ENU)."""
    lon, lat = math.radians(lon_deg), math.radians(lat_deg)
    sin_lat, cos_lat = math.sin(lat), math.cos(lat)
    sin_lon, cos_lon = math.sin(lon), math.cos(lon)
    east = (-sin_lon, cos_lon, 0.0)
    north = (-sin_lat * cos_lon, -sin_lat * sin_lon, cos_lat)
    up = (cos_lat * cos_lon, cos_lat * sin_lon, sin_lat)
    return east, north, up


def root_transform_matrix() -> list[float]:
    """A 4x4 column-major matrix (3D Tiles spec convention) mapping the tileset's
    local (glTF Z-up-after-conversion) coordinates to ECEF metres: columns 0-2 are the
    anchor's East/North/Up unit basis, column 3 is the anchor's ECEF position."""
    ox, oy, oz = geodetic_to_ecef(ANCHOR_LON_DEG, ANCHOR_LAT_DEG, ANCHOR_HEIGHT_M)
    east, north, up = enu_basis(ANCHOR_LON_DEG, ANCHOR_LAT_DEG)
    return [
        east[0], east[1], east[2], 0.0,
        north[0], north[1], north[2], 0.0,
        up[0], up[1], up[2], 0.0,
        ox, oy, oz, 1.0,
    ]


def build_node(level: int, i_lon: int, i_lat: int, max_level: int) -> dict:
    """One tile at `level` covering quadrant (i_lon, i_lat) of a `2**level`-per-axis
    subdivision of the root's [ANCHOR +/- HALF_DEG] footprint -- region bounding
    volumes in radians (3D Tiles spec), geometricError halving each level (halving is
    this fixture's own authoring choice, same convention web/js/globe_lod.js's
    synthetic quadtree uses, but here it is real per-node *authored* data read
    straight from the JSON, not re-derived from level by the viewer).

    A `region` bounding volume is, per the 3D Tiles spec, *never* affected by a
    tile's `transform` (confirmed directly against the vendored renderer's own
    `setRegionData` call, which -- unlike its sphere/box counterparts -- is never
    passed the cumulative transform matrix): it always describes an absolute
    WGS84 geodetic footprint. So these regions are centred on the *same*
    ANCHOR_LON_DEG/ANCHOR_LAT_DEG the root.transform's ECEF translation encodes --
    anchoring them anywhere else (e.g. near (0,0), as this fixture's first M16.4
    draft mistakenly did) would describe geographic bounds nowhere near where the
    transform actually places the tile's rendered content, which is exactly the
    kind of inconsistency a genuinely geo-referenced tileset must not have.
    """
    n = 2 ** level
    step = (2 * HALF_DEG) / n
    west = ANCHOR_LON_DEG - HALF_DEG + i_lon * step
    south = ANCHOR_LAT_DEG - HALF_DEG + i_lat * step
    node = {
        "boundingVolume": {
            "region": [
                math.radians(west), math.radians(south),
                math.radians(west + step), math.radians(south + step),
                0, 100,
            ]
        },
        "geometricError": ROOT_GEOMETRIC_ERROR / (2 ** level),
        "refine": "REPLACE",
        "content": {"uri": "tile.glb"},
    }
    if level < max_level:
        # Fixed SW/SE/NW/NE child order (south row before north row, west before
        # east within a row) -- the same fixed order web/js/globe_lod.js's
        # `tileChildren` uses for its own quadrant split. Determinism of the
        # viewer's own tile selection comes from its final canonical sort
        # (`compareTileIds3D` in web/js/tiles_layer.js), not from this order being
        # preserved -- but keeping it consistent with the globe avoids a gratuitous
        # difference between the two quadtree conventions in this codebase.
        node["children"] = [
            build_node(level + 1, i_lon * 2 + di_lon, i_lat * 2 + di_lat, max_level)
            for di_lat in (0, 1)
            for di_lon in (0, 1)
        ]
    return node


def build_tileset() -> dict:
    root = build_node(0, 0, 0, MAX_LEVEL)
    root["transform"] = root_transform_matrix()
    return {
        "asset": {"version": "1.1"},
        "geometricError": ROOT_GEOMETRIC_ERROR,
        "extras": {
            "geoReference": {
                "lonDeg": ANCHOR_LON_DEG,
                "latDeg": ANCHOR_LAT_DEG,
                "heightM": ANCHOR_HEIGHT_M,
                "note": (
                    "root.transform's translation column (indices 12-14) is "
                    "geodeticToEcef(lonDeg, latDeg, heightM); its 3 rotation columns "
                    "(indices 0-2, 4-6, 8-10) are the East/North/Up unit basis at "
                    "that point. web/js/tiles_layer.js and "
                    "tests/test_viewer_globe.py cross-check this against "
                    "web/js/globe_lod.js's geodeticToEcef -- see gen_3dtiles_fixture.py."
                ),
            }
        },
        "root": root,
    }


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    glb = build_triangle_glb()
    (OUT_DIR / "tile.glb").write_bytes(glb)
    tileset = build_tileset()
    (OUT_DIR / "tileset.json").write_text(json.dumps(tileset, indent=2) + "\n")

    def count(node: dict) -> int:
        return 1 + sum(count(c) for c in node.get("children", []))

    print(
        f"wrote {OUT_DIR / 'tileset.json'} ({count(tileset['root'])} tiles) and "
        f"{OUT_DIR / 'tile.glb'} ({len(glb)} bytes)"
    )


if __name__ == "__main__":
    main()

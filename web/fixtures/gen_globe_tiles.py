#!/usr/bin/env python3
"""Generate a small, fully offline set of raster imagery tiles for the tiled globe
(web/js/globe.js, M15.4) -- so the headless node harnesses (web/js/globe_lod_check.mjs,
the extended web/js/scene_jitter_harness.mjs) and the browser verification never touch
a real tile server (this task's "no network at test time" binding rule).

No external dependencies (no PIL/Pillow -- not installed in .venv, see this task's own
exploration) -- PNG is simple enough (a handful of scanline-filtered, zlib-deflated
RGB rows plus IHDR/IDAT/IEND chunks) to encode with the stdlib's `zlib`/`struct`, kept
here rather than adding a new Python dependency for a fixture generator.

Tile scheme matches web/js/globe_lod.js exactly (not a second copy of the addressing
math): level L has `2**(L+1)` columns x `2**L` rows, a whole 360x180 degree globe.
Each tile is rendered as a flat colour (deterministic per z/x/y, so a screenshot can be
visually cross-checked against which tile is which) with a 2px darker border so tile
boundaries are visible in the browser verification.

Run: .venv/bin/python web/fixtures/gen_globe_tiles.py
Output: web/fixtures/tiles/{z}/{x}/{y}.png
"""
from __future__ import annotations

import colorsys
import struct
import zlib
from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent / "tiles"
TILE_PX = 64
BORDER_PX = 2

# Levels to generate: the *complete* quadtree pyramid through level 2 (2 + 8 + 32 = 42
# tiles, each a tiny few-hundred-byte PNG -- "a few generated tiles" in aggregate size,
# per this task's brief, even though the tile *count* covers every level-2 cell).
# web/js/globe.js's GlobeLayer defaults to maxLevel=2, so with this fixture complete
# through level 2, no camera direction can make the browser verification's default
# LOD settings request a tile this fixture doesn't have -- deliberately avoiding a
# fixture with intentional gaps, which would otherwise make every missed tile's 404 a
# browser console error (Chrome logs "Failed to load resource: 404" for any failed
# request, independent of whether application code handles it gracefully), and this
# task requires a clean console during browser verification. GlobeLayer's texture-load
# failure path (falls back to a flat-colour material, mirrors
# web/js/scene.js's makeBodyMesh()) still exists and still matters for a real tile
# gateway or a maxLevel raised past what this fixture covers -- it is simply not
# exercised by the default browser-verification path with this complete fixture.
LEVEL0_TILES = [(0, 0, 0), (0, 1, 0)]
LEVEL1_TILES = [(1, x, y) for x in range(4) for y in range(2)]
LEVEL2_TILES = [(2, x, y) for x in range(8) for y in range(4)]
ALL_TILES = LEVEL0_TILES + LEVEL1_TILES + LEVEL2_TILES


def _png_chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def encode_png(width: int, height: int, rgb_rows: list[bytes]) -> bytes:
    """Minimal PNG encoder: 8-bit RGB, filter type 0 (None) per scanline."""
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + row for row in rgb_rows)
    idat = zlib.compress(raw, 9)
    return sig + _png_chunk(b"IHDR", ihdr) + _png_chunk(b"IDAT", idat) + _png_chunk(b"IEND", b"")


def tile_color(level: int, x: int, y: int) -> tuple[int, int, int]:
    """Deterministic, visually distinct colour per tile: hue walks with x/y, lightness
    drops with level (so higher-detail tiles read as visually "closer"/darker),
    matching the debug-tile convention many slippy-map viewers use."""
    nx = 2 ** (level + 1)
    hue = ((x + y * 0.5) / max(nx, 1)) % 1.0
    light = 0.75 - 0.12 * level
    r, g, b = colorsys.hls_to_rgb(hue, max(light, 0.2), 0.65)
    return int(r * 255), int(g * 255), int(b * 255)


def render_tile(level: int, x: int, y: int) -> bytes:
    r, g, b = tile_color(level, x, y)
    border = (max(r - 60, 0), max(g - 60, 0), max(b - 60, 0))
    rows = []
    for py in range(TILE_PX):
        on_border_row = py < BORDER_PX or py >= TILE_PX - BORDER_PX
        row = bytearray()
        for px in range(TILE_PX):
            on_border_col = px < BORDER_PX or px >= TILE_PX - BORDER_PX
            c = border if (on_border_row or on_border_col) else (r, g, b)
            row += bytes(c)
        rows.append(bytes(row))
    return encode_png(TILE_PX, TILE_PX, rows)


def main() -> None:
    n = 0
    for level, x, y in ALL_TILES:
        d = OUT_DIR / str(level) / str(x)
        d.mkdir(parents=True, exist_ok=True)
        (d / f"{y}.png").write_bytes(render_tile(level, x, y))
        n += 1
    print(f"wrote {n} tiles under {OUT_DIR}")


if __name__ == "__main__":
    main()

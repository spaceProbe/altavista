//! The globe's tile layout, in Rust. This is the **same** scheme `web/js/globe_lod.js`
//! (lines 105-120: `tileCountX`, `tileCountY`, `tileBoundsDeg`) already implements --
//! geographic (EPSG:4326-style, "plate carree"), NOT Web Mercator: level 0 is 2 tiles side
//! by side (a whole 360x180 degree globe, no polar singularity to work around), and each
//! level doubles both axes.
//!
//! # Why this is duplicated in Rust rather than shared with the JS
//!
//! `web/js/globe_lod.js` is JavaScript; there is no shared home a Rust crate and a
//! browser-facing ES module can both pull this arithmetic from without inventing a build
//! step (WASM, codegen, or similar) far out of scope for this task. So the four lines of
//! arithmetic ([`tile_count_x`], [`tile_count_y`], [`tile_bounds_deg`]) are duplicated,
//! deliberately, exactly the way `crates/av-jobs::hash` duplicates `av_edge::hash`'s
//! `GENESIS`/`chain_hash` primitive across two independently-owned crates rather than
//! forcing a dependency edge that should not exist.
//!
//! **What keeps the two from drifting is the pinning test below**
//! (`tile_bounds_deg_matches_hand_derived_values_from_globe_lod_js`): its expected
//! `BoundsDeg` values are derived by hand from the exact formula `web/js/globe_lod.js`
//! documents, not by calling [`tile_bounds_deg`] itself, and a reviewer can re-derive every
//! one of them from that file's own comment alone. If a future change to either side's
//! formula ever lets the two drift, this test is what catches it -- there is no other
//! mechanism (no shared crate, no codegen, no CI cross-check of the JS file) that would.

/// One tile address: `(level, x, y)`, exactly as `web/js/globe_lod.js`'s own `tileKey`/
/// `compareTiles` address a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tile {
    pub level: u32,
    pub x: u32,
    pub y: u32,
}

/// A tile's geodetic bounds, WGS84 degrees -- `web/js/globe_lod.js::tileBoundsDeg`'s own
/// return shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundsDeg {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

/// This scheme's own identity string -- `TileSetManifest.scheme` (`heavy.proto`) is always
/// this constant, never a literal repeated at each call site.
pub const SCHEME_ID: &str = "geographic-plate-carree-2x1";

/// `2^(level+1)` -- `web/js/globe_lod.js::tileCountX`, verbatim.
pub fn tile_count_x(level: u32) -> u32 {
    2u32.pow(level + 1)
}

/// `2^level` -- `web/js/globe_lod.js::tileCountY`, verbatim.
pub fn tile_count_y(level: u32) -> u32 {
    2u32.pow(level)
}

/// Tile -> geodetic bounds, byte-for-byte the arithmetic `web/js/globe_lod.js::
/// tileBoundsDeg` documents: `dLon = 360 / tileCountX(level)`, `dLat = 180 /
/// tileCountY(level)`, `west = -180 + x*dLon`, `south = -90 + y*dLat`, `east = west + dLon`,
/// `north = south + dLat`.
pub fn tile_bounds_deg(tile: Tile) -> BoundsDeg {
    let nx = tile_count_x(tile.level) as f64;
    let ny = tile_count_y(tile.level) as f64;
    let d_lon = 360.0 / nx;
    let d_lat = 180.0 / ny;
    let west = -180.0 + tile.x as f64 * d_lon;
    let south = -90.0 + tile.y as f64 * d_lat;
    BoundsDeg { west, south, east: west + d_lon, north: south + d_lat }
}

/// Every tile at `level` whose bounds overlap `bounds`, treating both the tile grid and
/// `bounds` as half-open intervals on each axis (`[west, east)` / `[south, north)`) -- a
/// tile whose own edge exactly touches `bounds`' edge with zero-area intersection is NOT
/// included, the same convention `[west, east)` grid columns use amongst themselves so
/// adjacent tiles never both claim the shared boundary. Returned in `(level, x, y)`
/// ascending order -- `web/js/globe_lod.js::compareTiles`'s
/// own canonical order, "so two runs produce byte-identical output regardless of map
/// iteration". Every returned tile satisfies `0 <= x < tile_count_x(level)` and
/// `0 <= y < tile_count_y(level)` -- a `bounds` that reaches past +-180/+-90 is clamped to
/// the grid's own extent before the covering columns/rows are computed, never allowed to
/// produce an out-of-range tile index.
pub fn tiles_covering(bounds: &BoundsDeg, level: u32) -> Vec<Tile> {
    let nx = tile_count_x(level);
    let ny = tile_count_y(level);
    let d_lon = 360.0 / nx as f64;
    let d_lat = 180.0 / ny as f64;

    // Clamp the query bounds to the grid's own [-180, 180] x [-90, 90] extent first, so a
    // caller-supplied bounds that reaches past the globe's own edge can never compute a
    // column/row index outside [0, nx)/[0, ny).
    let west = bounds.west.max(-180.0);
    let east = bounds.east.min(180.0);
    let south = bounds.south.max(-90.0);
    let north = bounds.north.min(90.0);
    if west >= east || south >= north {
        return Vec::new();
    }

    // Column x spans [-180 + x*dLon, -180 + (x+1)*dLon); it overlaps [west, east) iff
    // -180 + x*dLon < east AND -180 + (x+1)*dLon > west, i.e. iff x < (east+180)/dLon AND
    // x > (west+180)/dLon - 1. floor/ceil on those two bounds, then clamp into [0, nx).
    let x_min = (((west + 180.0) / d_lon).floor() as i64).max(0);
    // `east` is `bounds`' own exclusive edge: the first column index whose START is >= east
    // is the exclusive upper bound of the overlapping range, i.e. ceil((east+180)/dLon).
    let x_max_excl = (((east + 180.0) / d_lon).ceil() as i64).min(nx as i64);
    let y_min = (((south + 90.0) / d_lat).floor() as i64).max(0);
    let y_max_excl = (((north + 90.0) / d_lat).ceil() as i64).min(ny as i64);

    let mut out = Vec::new();
    if x_min >= x_max_excl || y_min >= y_max_excl {
        return out;
    }
    // (level, x, y) ascending -- `web/js/globe_lod.js::compareTiles`'s own order
    // (`a.level - b.level || a.x - b.x || a.y - b.y`) sorts by x before y, so x is the
    // OUTER loop here and y the inner one.
    for x in x_min..x_max_excl {
        for y in y_min..y_max_excl {
            out.push(Tile { level, x: x as u32, y: y as u32 });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- P1a acceptance evidence: pin the Rust against the JS -------------------------

    /// Hand-derived expected bounds for a fixed set of tiles, computed from
    /// `web/js/globe_lod.js`'s own documented formula -- NOT by calling
    /// [`tile_bounds_deg`] itself. Arithmetic shown per tile below.
    #[test]
    fn tile_bounds_deg_matches_hand_derived_values_from_globe_lod_js() {
        // (0,0,0): level 0 -> nx = 2^(0+1) = 2, ny = 2^0 = 1.
        //   dLon = 360/2 = 180, dLat = 180/1 = 180.
        //   west = -180 + 0*180 = -180, south = -90 + 0*180 = -90.
        //   east = -180 + 180 = 0, north = -90 + 180 = 90.
        assert_eq!(tile_bounds_deg(Tile { level: 0, x: 0, y: 0 }), BoundsDeg { west: -180.0, south: -90.0, east: 0.0, north: 90.0 });

        // (0,1,0): same level 0 grid (nx=2, ny=1, dLon=180, dLat=180), x=1.
        //   west = -180 + 1*180 = 0, south = -90 + 0*180 = -90.
        //   east = 0 + 180 = 180, north = -90 + 180 = 90.
        assert_eq!(tile_bounds_deg(Tile { level: 0, x: 1, y: 0 }), BoundsDeg { west: 0.0, south: -90.0, east: 180.0, north: 90.0 });

        // (1,0,0): level 1 -> nx = 2^(1+1) = 4, ny = 2^1 = 2.
        //   dLon = 360/4 = 90, dLat = 180/2 = 90.
        //   west = -180 + 0*90 = -180, south = -90 + 0*90 = -90.
        //   east = -180 + 90 = -90, north = -90 + 90 = 0.
        assert_eq!(tile_bounds_deg(Tile { level: 1, x: 0, y: 0 }), BoundsDeg { west: -180.0, south: -90.0, east: -90.0, north: 0.0 });

        // (1,3,1): level 1 grid again (nx=4, ny=2, dLon=90, dLat=90), x=3, y=1.
        //   west = -180 + 3*90 = 90, south = -90 + 1*90 = 0.
        //   east = 90 + 90 = 180, north = 0 + 90 = 90.
        assert_eq!(tile_bounds_deg(Tile { level: 1, x: 3, y: 1 }), BoundsDeg { west: 90.0, south: 0.0, east: 180.0, north: 90.0 });

        // (3,5,2): level 3 -> nx = 2^(3+1) = 16, ny = 2^3 = 8.
        //   dLon = 360/16 = 22.5, dLat = 180/8 = 22.5.
        //   west = -180 + 5*22.5 = -180 + 112.5 = -67.5, south = -90 + 2*22.5 = -90 + 45 = -45.
        //   east = -67.5 + 22.5 = -45, north = -45 + 22.5 = -22.5.
        assert_eq!(tile_bounds_deg(Tile { level: 3, x: 5, y: 2 }), BoundsDeg { west: -67.5, south: -45.0, east: -45.0, north: -22.5 });
    }

    #[test]
    fn tile_count_matches_the_documented_powers_of_two() {
        assert_eq!(tile_count_x(0), 2);
        assert_eq!(tile_count_y(0), 1);
        assert_eq!(tile_count_x(1), 4);
        assert_eq!(tile_count_y(1), 2);
        assert_eq!(tile_count_x(3), 16);
        assert_eq!(tile_count_y(3), 8);
    }

    // -- tiles_covering ------------------------------------------------------------------

    /// A bbox that partially overlaps a tile boundary at level 1 (nx=4, ny=2, dLon=90,
    /// dLat=90; tile columns at west edges -180,-90,0,90; rows at south edges -90,0).
    /// bounds = {west:-100, south:-10, east:20, north:50} straddles column boundaries at
    /// -90 and 0, and row boundary at 0:
    ///   - columns overlapping [-100,20): col0 [-180,-90) overlaps (-100 < -90? no wait,
    ///     -100 is inside col0 [-180,-90)); col1 [-90,0) overlaps; col2 [0,90) overlaps
    ///     (20 > 0). col3 [90,180) does not (20 < 90).
    ///   - rows overlapping [-10,50): row0 [-90,0) overlaps (-10 inside); row1 [0,90)
    ///     overlaps (50 > 0, 0 < 50).
    ///
    /// `web/js/globe_lod.js::compareTiles` sorts `level`, then `x`, then `y` -- x is the
    /// PRIMARY sort key, so the expected order below groups by x (0, then 1, then 2), each
    /// with its two overlapping y values (0, then 1) -- NOT grouped by y. Expected tiles,
    /// (level,x,y) ascending: (1,0,0),(1,0,1),(1,1,0),(1,1,1),(1,2,0),(1,2,1)
    #[test]
    fn tiles_covering_returns_exactly_the_overlapping_tiles_in_order() {
        let bounds = BoundsDeg { west: -100.0, south: -10.0, east: 20.0, north: 50.0 };
        let got = tiles_covering(&bounds, 1);
        let expected = vec![
            Tile { level: 1, x: 0, y: 0 },
            Tile { level: 1, x: 0, y: 1 },
            Tile { level: 1, x: 1, y: 0 },
            Tile { level: 1, x: 1, y: 1 },
            Tile { level: 1, x: 2, y: 0 },
            Tile { level: 1, x: 2, y: 1 },
        ];
        assert_eq!(got, expected);
    }

    #[test]
    fn tiles_covering_never_returns_a_tile_outside_the_grid() {
        // A bounds far outside the globe's own extent must clamp, never overflow/panic and
        // never emit x >= tile_count_x(level) or y >= tile_count_y(level).
        let bounds = BoundsDeg { west: -1000.0, south: -1000.0, east: 1000.0, north: 1000.0 };
        for level in 0..5 {
            let tiles = tiles_covering(&bounds, level);
            let nx = tile_count_x(level);
            let ny = tile_count_y(level);
            assert_eq!(tiles.len() as u64, nx as u64 * ny as u64, "level {level} must cover the whole grid");
            for t in &tiles {
                assert!(t.x < nx, "{t:?} x out of range at level {level}");
                assert!(t.y < ny, "{t:?} y out of range at level {level}");
            }
            // Ascending (level, x, y) order.
            for w in tiles.windows(2) {
                assert!((w[0].level, w[0].x, w[0].y) < (w[1].level, w[1].x, w[1].y));
            }
        }
    }

    #[test]
    fn tiles_covering_a_single_point_bbox_returns_empty() {
        // A zero-area bounds (west == east or south == north) covers nothing.
        let bounds = BoundsDeg { west: 10.0, south: 10.0, east: 10.0, north: 10.0 };
        assert!(tiles_covering(&bounds, 2).is_empty());
    }

    #[test]
    fn tiles_covering_whole_globe_at_level_0_returns_both_tiles_in_order() {
        let bounds = BoundsDeg { west: -180.0, south: -90.0, east: 180.0, north: 90.0 };
        let got = tiles_covering(&bounds, 0);
        assert_eq!(got, vec![Tile { level: 0, x: 0, y: 0 }, Tile { level: 0, x: 1, y: 0 }]);
    }
}

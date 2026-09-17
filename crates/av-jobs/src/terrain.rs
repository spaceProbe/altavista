//! P3a (`docs/heavy-plan.md` H3, terrain half; this round's open item 5, first bullet): the
//! terrain tile payload `crate::tiler::TilerExecutor` writes for `output == "terrain"` -- a
//! small, self-describing, hand-decodable binary heightmap, media type
//! `crate::tiler::TERRAIN_TILE_MEDIA_TYPE` (`"application/vnd.altavista.terrain-tile+raw"`,
//! the exact string `heavy.proto`'s own `TileSetKind::TILE_SET_KIND_TERRAIN` doc comment
//! already named before this round's implementation existed).
//!
//! # Why a new hand-rolled binary format, not a 16-bit PNG
//!
//! This crate's own `png.rs` is an 8-bit RGB truecolour encoder (PNG colour type 2) --
//! extending it to a 16-bit greyscale channel (colour type 0, bit depth 16) is a second,
//! independently-tested encoder path this task's own brief explicitly rules out ("do not
//! invent a PNG 16-bit encoder ... adding a second image format is not what this task is
//! about"). A single scalar sample per grid point, with no filter/compression ambiguity at
//! all, is simpler to specify exactly and simpler to decode by hand than PNG's chunk/zlib/
//! filter machinery would buy for no benefit here -- there is no colour, no palette, no
//! interlacing to reuse from PNG's own design.
//!
//! # Layout (byte for byte -- a reader needs nothing but this comment to build a decoder)
//!
//! ```text
//! offset  size  field             value
//! 0       8     magic             "AVTERRHM" (ASCII, no NUL terminator; AltaVista TERRain
//!                                  Height Map)
//! 8       4     version           u32 LE, = 1
//! 12      4     samples_per_side  u32 LE (equal to the tiler's own `tile_size` -- the
//!                                  heightmap is a samples_per_side x samples_per_side grid
//!                                  of scalar samples, ONE sample per grid point, not
//!                                  samples_per_side+1 shared-edge vertices: `sample_index`
//!                                  below uses the identical pixel-CENTRE convention
//!                                  `crate::tiler::sample_nearest` already documents, so a
//!                                  terrain sample and an imagery pixel at the same tile-
//!                                  local (px, py) are sampled at the exact same geodetic
//!                                  point)
//! 16      4     sample_type       u32 LE, = 1 (the only value this version defines:
//!                                  `SAMPLE_TYPE_I16`, a signed 16-bit integer, little-endian)
//! 20      4     height_unit       u32 LE, = 1 (the only value this version defines:
//!                                  `HEIGHT_UNIT_METRES_WHOLE` -- the i16 sample value IS the
//!                                  elevation in whole metres; there is deliberately no scale
//!                                  or offset factor anywhere in this header, so decoding a
//!                                  sample is exactly `i16::from_le_bytes(...)`, nothing more)
//! 24      *     samples           samples_per_side * samples_per_side * 2 bytes: i16 LE,
//!                                  row-major, NORTH ROW FIRST -- the identical orientation
//!                                  `crate::raster`'s own AVRASTER format and this tile's own
//!                                  PNG sibling (`crate::tiler::render_imagery_tile`) already
//!                                  use, so `samples[0]` is the tile's own north-west-most
//!                                  sample and a reader never has to reconcile two different
//!                                  row-order conventions within the same tile set.
//! ```
//!
//! `HEADER_LEN` (24 bytes) is fixed; there is no variable-length header field.
//!
//! # How a viewer is expected to read this
//!
//! Check the 8-byte magic and the `version`/`sample_type`/`height_unit` fields against the
//! constants this module exports (never assume version 1 without checking -- a later version
//! may define new `sample_type`/`height_unit` values this decoder does not understand, and
//! must refuse rather than silently misinterpret); then read `samples_per_side` once, confirm
//! the remaining byte count equals `samples_per_side^2 * 2`, and read that many little-endian
//! `i16`s in row-major, north-row-first order. Sample `(px, py)` (0-indexed, `px` west-to-
//! east, `py` north-to-south) is `samples[py * samples_per_side + px]`, and its geodetic
//! sample point is exactly `crate::tiler`'s own module-doc formula for `(lon, lat)` at tile-
//! local pixel `(px, py)` -- the terrain tiler and the imagery tiler share that one formula
//! (`crate::tiler::tile_pixel_center_lonlat`), so a terrain sample and the corresponding
//! imagery pixel in a tile set built from the same tile address always describe the same
//! ground point.
//!
//! # How the sample value is derived from the input raster (this round's own decision)
//!
//! `crate::tiler`'s only input format is `crate::raster`'s AVRASTER RGB8 raster (the same one
//! `output == "imagery"` reads) -- this round adds no second, elevation-specific input
//! format, since doing so would be exactly the kind of scope creep this task's own brief
//! warns against for the output side. Reusing the RGB8 input for elevation therefore needs an
//! explicit, deterministic derivation from a sampled `[r, g, b]` triple to an `i16` height:
//! this module (via `crate::tiler::render_terrain_tile`) takes `r` as the value's high byte
//! and `g` as its low byte, i.e. `((r as u16) << 8 | g as u16) as i16` -- a single bit-pack
//! with no multiplication, scale factor, or rounding to re-derive, giving a signed elevation
//! range of -32768..=32767 metres from two of the raster's three channels. `b` is read by
//! `sample_nearest` but deliberately unused by this derivation: two channels are already
//! enough to fill this format's own `i16` sample type exactly, and using a fixed, disclosed
//! subset of the input (rather than folding `b` in with some additional scale factor) keeps
//! the derivation a single bit-pack, not an arithmetic expression a reviewer has to double
//! check for overflow or rounding. A later round that wants three-channel precision (a wider
//! sample type, e.g. i32) is a new `sample_type` value, not a change to this one.
//!
//! # Determinism
//!
//! [`encode`] is a pure function of `samples_per_side` and `samples`; there is no filter
//! selection, no compressor, and no floating-point sample type to round differently across
//! platforms (the `i16` sample type is exact, fixed-width, twos-complement integer data,
//! identical on every target this workspace builds for). The same tile, resampled from the
//! same input raster with the same nearest-neighbour formula, therefore encodes to
//! byte-identical bytes on every run -- see `crates/av-jobs/tests/tiler_terrain.rs`'s own
//! determinism assertions.

/// `"AVTERRHM"` -- see this module's own doc for the full layout this pins.
pub const MAGIC: &[u8; 8] = b"AVTERRHM";
pub const VERSION: u32 = 1;
/// The only `sample_type` this version defines: a signed 16-bit integer, little-endian.
pub const SAMPLE_TYPE_I16: u32 = 1;
/// The only `height_unit` this version defines: the sample value IS the elevation in whole
/// metres, no scale or offset factor.
pub const HEIGHT_UNIT_METRES_WHOLE: u32 = 1;
/// `8 (magic) + 4 (version) + 4 (samples_per_side) + 4 (sample_type) + 4 (height_unit)`.
pub const HEADER_LEN: usize = 8 + 4 + 4 + 4 + 4;

/// What [`decode`] refuses. Every variant names exactly which well-formedness rule failed --
/// the same discipline `crate::raster::RasterError` already established for this crate's
/// other self-describing binary format.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TerrainTileError {
    #[error("terrain tile too short to contain even the fixed {HEADER_LEN}-byte header ({len} byte(s))")]
    TooShortForHeader { len: usize },
    #[error("bad magic: expected {MAGIC:?}, got {got:?}")]
    BadMagic { got: [u8; 8] },
    #[error("unsupported version {got}: only version 1 is defined")]
    UnsupportedVersion { got: u32 },
    #[error("unsupported sample_type {got}: only {SAMPLE_TYPE_I16} (signed i16) is defined")]
    UnsupportedSampleType { got: u32 },
    #[error("unsupported height_unit {got}: only {HEIGHT_UNIT_METRES_WHOLE} (whole metres) is defined")]
    UnsupportedHeightUnit { got: u32 },
    #[error("samples_per_side must be nonzero")]
    ZeroSamplesPerSide,
    #[error("declared size (samples_per_side={samples_per_side} squared * 2 = {expected} byte(s)) does not match the {actual} sample byte(s) actually present")]
    SizeMismatch { samples_per_side: u32, expected: u64, actual: u64 },
}

/// A decoded terrain tile: `samples_per_side^2` elevation samples, row-major, north row
/// first (see this module's own doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainTile {
    pub samples_per_side: u32,
    pub samples: Vec<i16>,
}

/// Encodes `samples` (row-major, north row first, exactly `samples_per_side^2` values) as a
/// complete terrain tile per this module's own layout. `samples.len()` must equal
/// `samples_per_side^2` exactly -- the same "malformed input is a caller bug, not a silent
/// truncation" contract `crate::png::encode_rgb8` already uses, enforced here with a debug
/// assertion (this function's one caller, `crate::tiler::render_terrain_tile`, always builds
/// `samples` by construction to exactly this length, so a mismatch here would be this crate's
/// own bug, not a malformed external input -- unlike `crate::png::encode_rgb8`, whose caller
/// contract is public API this crate does not control end to end).
pub fn encode(samples_per_side: u32, samples: &[i16]) -> Vec<u8> {
    debug_assert_eq!(samples.len(), samples_per_side as usize * samples_per_side as usize, "encode: samples.len() must equal samples_per_side^2");
    let mut out = Vec::with_capacity(HEADER_LEN + samples.len() * 2);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&samples_per_side.to_le_bytes());
    out.extend_from_slice(&SAMPLE_TYPE_I16.to_le_bytes());
    out.extend_from_slice(&HEIGHT_UNIT_METRES_WHOLE.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Decodes `bytes` as a terrain tile (see this module's own doc), refusing every malformed
/// case with a typed [`TerrainTileError`]. Not called by `crate::tiler` (which only ever
/// encodes its own output), but kept as this format's own reference decoder -- exactly the
/// role `crate::raster::decode` plays for its own format -- and exercised directly by this
/// crate's own round-trip tests.
pub fn decode(bytes: &[u8]) -> Result<TerrainTile, TerrainTileError> {
    if bytes.len() < HEADER_LEN {
        return Err(TerrainTileError::TooShortForHeader { len: bytes.len() });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[0..8]);
    if &magic != MAGIC {
        return Err(TerrainTileError::BadMagic { got: magic });
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != VERSION {
        return Err(TerrainTileError::UnsupportedVersion { got: version });
    }
    let samples_per_side = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if samples_per_side == 0 {
        return Err(TerrainTileError::ZeroSamplesPerSide);
    }
    let sample_type = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if sample_type != SAMPLE_TYPE_I16 {
        return Err(TerrainTileError::UnsupportedSampleType { got: sample_type });
    }
    let height_unit = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    if height_unit != HEIGHT_UNIT_METRES_WHOLE {
        return Err(TerrainTileError::UnsupportedHeightUnit { got: height_unit });
    }

    let expected = samples_per_side as u64 * samples_per_side as u64 * 2;
    let actual = (bytes.len() - HEADER_LEN) as u64;
    if actual != expected {
        return Err(TerrainTileError::SizeMismatch { samples_per_side, expected, actual });
    }

    let mut samples = Vec::with_capacity((samples_per_side * samples_per_side) as usize);
    let mut i = HEADER_LEN;
    while i < bytes.len() {
        samples.push(i16::from_le_bytes(bytes[i..i + 2].try_into().unwrap()));
        i += 2;
    }
    Ok(TerrainTile { samples_per_side, samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_fixture() -> (u32, Vec<i16>) {
        (2, vec![-32768, -1, 0, 32767])
    }

    #[test]
    fn decode_accepts_a_well_formed_terrain_tile() {
        let (n, samples) = valid_fixture();
        let bytes = encode(n, &samples);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.samples_per_side, 2);
        assert_eq!(decoded.samples, samples);
    }

    #[test]
    fn encode_then_decode_round_trips_for_a_range_of_values() {
        let n = 4u32;
        let samples: Vec<i16> = (0..16).map(|i| (i * 4001 - 32000) as i16).collect();
        let bytes = encode(n, &samples);
        assert_eq!(bytes.len(), HEADER_LEN + 16 * 2);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.samples_per_side, n);
        assert_eq!(decoded.samples, samples);
    }

    #[test]
    fn decode_refuses_too_short_for_header() {
        let err = decode(&[0u8; 10]).unwrap_err();
        assert_eq!(err, TerrainTileError::TooShortForHeader { len: 10 });
    }

    #[test]
    fn decode_refuses_bad_magic() {
        let (n, samples) = valid_fixture();
        let mut bytes = encode(n, &samples);
        bytes[0] = b'X';
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, TerrainTileError::BadMagic { .. }));
    }

    #[test]
    fn decode_refuses_unsupported_version() {
        let (n, samples) = valid_fixture();
        let mut bytes = encode(n, &samples);
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err, TerrainTileError::UnsupportedVersion { got: 2 });
    }

    #[test]
    fn decode_refuses_zero_samples_per_side() {
        let mut bytes = encode(1, &[0]);
        bytes[12..16].copy_from_slice(&0u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err, TerrainTileError::ZeroSamplesPerSide);
    }

    #[test]
    fn decode_refuses_unsupported_sample_type() {
        let (n, samples) = valid_fixture();
        let mut bytes = encode(n, &samples);
        bytes[16..20].copy_from_slice(&99u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err, TerrainTileError::UnsupportedSampleType { got: 99 });
    }

    #[test]
    fn decode_refuses_unsupported_height_unit() {
        let (n, samples) = valid_fixture();
        let mut bytes = encode(n, &samples);
        bytes[20..24].copy_from_slice(&99u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err, TerrainTileError::UnsupportedHeightUnit { got: 99 });
    }

    #[test]
    fn decode_refuses_a_size_mismatch() {
        let (n, samples) = valid_fixture();
        let mut bytes = encode(n, &samples);
        bytes.push(0); // one extra byte
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, TerrainTileError::SizeMismatch { .. }));
    }

    #[test]
    fn encoding_the_same_samples_twice_is_byte_identical() {
        let n = 8u32;
        let samples: Vec<i16> = (0..64).map(|i| (i * 997 - 30000) as i16).collect();
        let a = encode(n, &samples);
        let b = encode(n, &samples);
        assert_eq!(a, b);
    }
}

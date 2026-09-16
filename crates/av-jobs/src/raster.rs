//! The input raster format `crates/av-jobs::tiler::TilerExecutor` reads for `output ==
//! "imagery"`: a small, hand-decodable byte layout, media type
//! `"application/vnd.altavista.raster+raw"`, chosen (rather than PNG/TIFF/GeoTIFF) because
//! this crate must not add a raster-decoding dependency and must produce its own raw pixel
//! source that [`decode`] can parse with nothing but `std`.
//!
//! # Layout (byte for byte -- a reader needs nothing but this comment to build one by hand)
//!
//! ```text
//! offset  size  field      value
//! 0       8     magic      "AVRASTER" (ASCII, no NUL terminator)
//! 8       4     version    u32 LE, = 1
//! 12      4     width      u32 LE, pixels
//! 16      4     height     u32 LE, pixels
//! 20      4     channels   u32 LE, = 3 for imagery (RGB, u8 per channel)
//! 24      8     west       f64 LE, WGS84 degrees
//! 32      8     south      f64 LE, WGS84 degrees
//! 40      8     east       f64 LE, WGS84 degrees
//! 48      8     north      f64 LE, WGS84 degrees
//! 56      *     pixels     width*height*channels bytes, row-major, NORTH ROW FIRST
//! ```
//!
//! `pixels[0]` is therefore the north-west-most pixel, and pixel bytes advance west-to-east
//! within a row, then north-to-south across rows -- matching `crate::scheme::BoundsDeg`'s
//! own `{west, south, east, north}` orientation, so [`crate::tiler`]'s resampler can map a
//! tile's geodetic bounds straight onto raster row/column indices with no extra
//! orientation bookkeeping.
//!
//! [`decode`] refuses every malformed case with a typed [`RasterError`], never a panic and
//! never a silently-truncated or silently-padded read.

/// What [`decode`] refuses. Every variant names exactly which well-formedness rule failed.
/// `PartialEq` only (not `Eq`) -- several variants carry `f64` fields, which have no `Eq`
/// impl.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RasterError {
    #[error("raster too short to contain even the fixed 56-byte header ({len} byte(s))")]
    TooShortForHeader { len: usize },
    #[error("bad magic: expected \"AVRASTER\", got {got:?}")]
    BadMagic { got: [u8; 8] },
    #[error("unsupported version {got}: only version 1 is defined")]
    UnsupportedVersion { got: u32 },
    #[error("declared size (width={width} * height={height} * channels={channels} = {expected} byte(s)) does not match the {actual} pixel byte(s) actually present")]
    SizeMismatch { width: u32, height: u32, channels: u32, expected: u64, actual: u64 },
    #[error("west ({west}) must be strictly less than east ({east})")]
    WestNotLessThanEast { west: f64, east: f64 },
    #[error("south ({south}) must be strictly less than north ({north})")]
    SouthNotLessThanNorth { south: f64, north: f64 },
    #[error("channels must be 3 (RGB) for imagery; got {got}")]
    UnsupportedChannels { got: u32 },
    #[error("width and height must both be nonzero")]
    ZeroDimension,
}

pub const MAGIC: &[u8; 8] = b"AVRASTER";
pub const HEADER_LEN: usize = 8 + 4 + 4 + 4 + 4 + 8 + 8 + 8 + 8; // = 56

/// A decoded raster: geodetic bounds plus row-major (north row first) RGB8 pixel bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub channels: u32,
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    pub pixels: Vec<u8>,
}

impl Raster {
    pub fn bounds(&self) -> crate::scheme::BoundsDeg {
        crate::scheme::BoundsDeg { west: self.west, south: self.south, east: self.east, north: self.north }
    }

    /// The RGB8 pixel at raster column/row `(col, row)` (`row` counted from the north,
    /// matching this format's own storage order) -- `None` if either index is out of range.
    pub fn pixel_rgb(&self, col: u32, row: u32) -> Option<[u8; 3]> {
        if col >= self.width || row >= self.height {
            return None;
        }
        let idx = (row as u64 * self.width as u64 + col as u64) as usize * self.channels as usize;
        Some([self.pixels[idx], self.pixels[idx + 1], self.pixels[idx + 2]])
    }
}

/// Decodes `bytes` as an AltaVista raw raster (see this module's own doc for the exact
/// layout), refusing every malformed case with a typed [`RasterError`].
pub fn decode(bytes: &[u8]) -> Result<Raster, RasterError> {
    if bytes.len() < HEADER_LEN {
        return Err(RasterError::TooShortForHeader { len: bytes.len() });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[0..8]);
    if &magic != MAGIC {
        return Err(RasterError::BadMagic { got: magic });
    }
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != 1 {
        return Err(RasterError::UnsupportedVersion { got: version });
    }
    let width = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let channels = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let west = f64::from_le_bytes(bytes[24..32].try_into().unwrap());
    let south = f64::from_le_bytes(bytes[32..40].try_into().unwrap());
    let east = f64::from_le_bytes(bytes[40..48].try_into().unwrap());
    let north = f64::from_le_bytes(bytes[48..56].try_into().unwrap());

    if width == 0 || height == 0 {
        return Err(RasterError::ZeroDimension);
    }
    if channels != 3 {
        return Err(RasterError::UnsupportedChannels { got: channels });
    }
    // `west >= east` would silently accept a NaN bound (every comparison against NaN is
    // false, `f64` being only partially ordered) -- checking for `Some(Ordering::Less)`
    // explicitly refuses NaN too, rather than treating it as "not less than, so refuse"
    // only by accident of `>=`'s own definition.
    if west.partial_cmp(&east) != Some(std::cmp::Ordering::Less) {
        return Err(RasterError::WestNotLessThanEast { west, east });
    }
    if south.partial_cmp(&north) != Some(std::cmp::Ordering::Less) {
        return Err(RasterError::SouthNotLessThanNorth { south, north });
    }

    let expected = width as u64 * height as u64 * channels as u64;
    let actual = (bytes.len() - HEADER_LEN) as u64;
    if actual != expected {
        return Err(RasterError::SizeMismatch { width, height, channels, expected, actual });
    }

    Ok(Raster { width, height, channels, west, south, east, north, pixels: bytes[HEADER_LEN..].to_vec() })
}

/// Encodes a [`Raster`]-shaped set of fields back into this format's own bytes -- used only
/// by this crate's own tests to build fixture input rasters (never by production code,
/// which only ever reads a raster an upstream producer already wrote).
#[cfg(test)]
pub fn encode(width: u32, height: u32, west: f64, south: f64, east: f64, north: f64, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + pixels.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&west.to_le_bytes());
    out.extend_from_slice(&south.to_le_bytes());
    out.extend_from_slice(&east.to_le_bytes());
    out.extend_from_slice(&north.to_le_bytes());
    out.extend_from_slice(pixels);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_fixture() -> Vec<u8> {
        // 2x1 RGB raster covering the whole western hemisphere's western half, arbitrary
        // bounds, two distinct pixels.
        let pixels = [10u8, 20, 30, 40, 50, 60];
        encode(2, 1, -180.0, -90.0, 0.0, 90.0, &pixels)
    }

    #[test]
    fn decode_accepts_a_well_formed_raster() {
        let bytes = valid_fixture();
        let raster = decode(&bytes).unwrap();
        assert_eq!(raster.width, 2);
        assert_eq!(raster.height, 1);
        assert_eq!(raster.channels, 3);
        assert_eq!(raster.west, -180.0);
        assert_eq!(raster.south, -90.0);
        assert_eq!(raster.east, 0.0);
        assert_eq!(raster.north, 90.0);
        assert_eq!(raster.pixel_rgb(0, 0), Some([10, 20, 30]));
        assert_eq!(raster.pixel_rgb(1, 0), Some([40, 50, 60]));
        assert_eq!(raster.pixel_rgb(2, 0), None);
    }

    #[test]
    fn decode_refuses_too_short_for_header() {
        let err = decode(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, RasterError::TooShortForHeader { len: 10 }));
    }

    #[test]
    fn decode_refuses_bad_magic() {
        let mut bytes = valid_fixture();
        bytes[0] = b'X';
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, RasterError::BadMagic { .. }));
    }

    #[test]
    fn decode_refuses_unsupported_version() {
        let mut bytes = valid_fixture();
        bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert_eq!(err, RasterError::UnsupportedVersion { got: 2 });
    }

    #[test]
    fn decode_refuses_a_size_mismatch() {
        let mut bytes = valid_fixture();
        bytes.push(0); // one extra pixel byte
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, RasterError::SizeMismatch { .. }));
    }

    #[test]
    fn decode_refuses_west_not_less_than_east() {
        let pixels = [1u8, 2, 3];
        let bytes = encode(1, 1, 10.0, -10.0, 10.0, 10.0, &pixels); // west == east
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, RasterError::WestNotLessThanEast { .. }));
    }

    #[test]
    fn decode_refuses_south_not_less_than_north() {
        let pixels = [1u8, 2, 3];
        let bytes = encode(1, 1, -10.0, 10.0, 10.0, 10.0, &pixels); // south == north
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, RasterError::SouthNotLessThanNorth { .. }));
    }

    #[test]
    fn decode_refuses_channels_not_three() {
        let mut bytes = valid_fixture();
        bytes[20..24].copy_from_slice(&4u32.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        assert!(matches!(err, RasterError::UnsupportedChannels { got: 4 }));
    }

    #[test]
    fn decode_refuses_zero_width_or_height() {
        let pixels: [u8; 0] = [];
        let bytes = encode(0, 1, -10.0, -10.0, 10.0, 10.0, &pixels);
        assert_eq!(decode(&bytes).unwrap_err(), RasterError::ZeroDimension);
        let bytes = encode(1, 0, -10.0, -10.0, 10.0, 10.0, &pixels);
        assert_eq!(decode(&bytes).unwrap_err(), RasterError::ZeroDimension);
    }
}

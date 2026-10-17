//! ClickHouse's `RowBinary` format, for exactly the column types the study schema uses
//! (`String`, `UInt32`, `UInt64`, `Int32`, `Int64`, `Float64`, `Bool`, `Nullable(Bool)`,
//! `Nullable(Float64)` and `DateTime64` as a skipped server-side default).
//!
//! # Why a binary format
//!
//! The study store promises that every `f64` round-trips **bit for bit**: `-0.0`, subnormals,
//! `+-inf` and NaN (payload included). A text format would have to print and re-parse every
//! double (and `-0.0`, `nan` and `inf` are exactly where text formats disagree between
//! servers and versions); `RowBinary` is the IEEE-754 bytes themselves, little-endian, with no
//! text step to get wrong. `tests/clickhouse_store.rs` proves the round trip against the real
//! server rather than assuming it.
//!
//! # Wire shape, as ClickHouse documents it
//!
//! A row is its columns' values concatenated, with no framing between rows and no header.
//! Fixed-width integers and floats are little-endian. `String` is an unsigned LEB128 length
//! followed by that many bytes. `Bool` is one byte (0 or 1). `Nullable(T)` is one flag byte
//! (1 = NULL, 0 = present) followed, only when present, by the `T` value.
//!
//! [`Decoder`] is strict: a truncated value, a non-UTF-8 string, a `Bool` or null flag that is
//! not 0 or 1, and bytes left over after the last row are all typed errors, because a reply that
//! was cut off mid-stream (the server can fail after it has already started answering) must never
//! decode as a shorter, valid-looking study.

use std::fmt;

/// Why a `RowBinary` buffer did not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowBinaryError {
    /// The buffer ended inside a value.
    Truncated { wanted: usize, available: usize, offset: usize },
    /// A `String` was not valid UTF-8 (every string in the study schema is a proto3 `string`).
    InvalidUtf8 { offset: usize },
    /// A `Bool` byte other than 0 or 1.
    InvalidBool { value: u8, offset: usize },
    /// A `Nullable` flag byte other than 0 or 1.
    InvalidNullFlag { value: u8, offset: usize },
    /// A `String` length that does not fit this platform's `usize`, or a LEB128 over 10 bytes.
    BadLength { offset: usize },
    /// Bytes remained after the last expected row.
    TrailingBytes { count: usize, offset: usize },
}

impl fmt::Display for RowBinaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowBinaryError::Truncated { wanted, available, offset } => {
                write!(f, "RowBinary truncated at byte {offset}: wanted {wanted} byte(s), {available} left")
            }
            RowBinaryError::InvalidUtf8 { offset } => write!(f, "RowBinary string at byte {offset} is not valid UTF-8"),
            RowBinaryError::InvalidBool { value, offset } => write!(f, "RowBinary Bool at byte {offset} is {value}, not 0 or 1"),
            RowBinaryError::InvalidNullFlag { value, offset } => write!(f, "RowBinary Nullable flag at byte {offset} is {value}, not 0 or 1"),
            RowBinaryError::BadLength { offset } => write!(f, "RowBinary string length at byte {offset} is malformed or too large"),
            RowBinaryError::TrailingBytes { count, offset } => write!(f, "RowBinary has {count} unexpected trailing byte(s) from byte {offset}"),
        }
    }
}

impl std::error::Error for RowBinaryError {}

/// Appends values in `RowBinary` encoding.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Takes the buffer, leaving the encoder empty (to send a chunk and continue).
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// The IEEE-754 bits, little-endian: `-0.0`, subnormals, infinities and every NaN payload
    /// survive exactly.
    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }

    pub fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    pub fn string(&mut self, v: &str) {
        self.varuint(v.len() as u64);
        self.buf.extend_from_slice(v.as_bytes());
    }

    pub fn nullable_bool(&mut self, v: Option<bool>) {
        match v {
            None => self.buf.push(1),
            Some(b) => {
                self.buf.push(0);
                self.bool(b);
            }
        }
    }

    pub fn nullable_f64(&mut self, v: Option<f64>) {
        match v {
            None => self.buf.push(1),
            Some(x) => {
                self.buf.push(0);
                self.f64(x);
            }
        }
    }

    fn varuint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }
}

/// Reads `RowBinary` values from a byte slice.
#[derive(Debug)]
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// The number of bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Whether every byte has been consumed.
    pub fn is_done(&self) -> bool {
        self.pos == self.buf.len()
    }

    /// Errors with [`RowBinaryError::TrailingBytes`] unless every byte has been consumed.
    pub fn finish(&self) -> Result<(), RowBinaryError> {
        if self.is_done() {
            Ok(())
        } else {
            Err(RowBinaryError::TrailingBytes { count: self.buf.len() - self.pos, offset: self.pos })
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], RowBinaryError> {
        let available = self.buf.len() - self.pos;
        if n > available {
            return Err(RowBinaryError::Truncated { wanted: n, available, offset: self.pos });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], RowBinaryError> {
        let bytes = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, RowBinaryError> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u32(&mut self) -> Result<u32, RowBinaryError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, RowBinaryError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32, RowBinaryError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64, RowBinaryError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub fn f64(&mut self) -> Result<f64, RowBinaryError> {
        Ok(f64::from_bits(u64::from_le_bytes(self.array()?)))
    }

    pub fn bool(&mut self) -> Result<bool, RowBinaryError> {
        let offset = self.pos;
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(RowBinaryError::InvalidBool { value, offset }),
        }
    }

    pub fn string(&mut self) -> Result<String, RowBinaryError> {
        let len_offset = self.pos;
        let len = self.varuint()?;
        let len = usize::try_from(len).map_err(|_| RowBinaryError::BadLength { offset: len_offset })?;
        let offset = self.pos;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| RowBinaryError::InvalidUtf8 { offset })
    }

    fn null_flag(&mut self) -> Result<bool, RowBinaryError> {
        let offset = self.pos;
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(RowBinaryError::InvalidNullFlag { value, offset }),
        }
    }

    pub fn nullable_bool(&mut self) -> Result<Option<bool>, RowBinaryError> {
        if self.null_flag()? {
            Ok(None)
        } else {
            Ok(Some(self.bool()?))
        }
    }

    pub fn nullable_f64(&mut self) -> Result<Option<f64>, RowBinaryError> {
        if self.null_flag()? {
            Ok(None)
        } else {
            Ok(Some(self.f64()?))
        }
    }

    fn varuint(&mut self) -> Result<u64, RowBinaryError> {
        let offset = self.pos;
        let mut value: u64 = 0;
        for shift in (0..70).step_by(7) {
            let byte = self.u8()?;
            // The tenth byte may carry only the top bit of a u64.
            if shift == 63 && byte > 1 {
                return Err(RowBinaryError::BadLength { offset });
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(RowBinaryError::BadLength { offset })
    }
}

/// Walks `bytes` as exactly one row of the given ClickHouse column types and returns the number of
/// bytes it spans. Used by unit tests to prove each row encoder writes the same layout the DDL
/// declares (the encoders and the DDL are written separately, and an insert names its columns in
/// the DDL's order, so a drift between them would otherwise only show against a live server).
#[cfg(test)]
pub fn row_len_by_types(types: &[&str], bytes: &[u8]) -> Result<usize, RowBinaryError> {
    let mut d = Decoder::new(bytes);
    for ty in types {
        skip_value(&mut d, ty)?;
    }
    Ok(d.pos)
}

#[cfg(test)]
fn skip_value(d: &mut Decoder<'_>, ty: &str) -> Result<(), RowBinaryError> {
    if let Some(inner) = ty.strip_prefix("Nullable(").and_then(|r| r.strip_suffix(')')) {
        if d.null_flag()? {
            return Ok(());
        }
        return skip_value(d, inner);
    }
    match ty {
        "String" => d.string().map(|_| ()),
        "UInt32" | "Int32" => d.take(4).map(|_| ()),
        "UInt64" | "Int64" | "Float64" => d.take(8).map(|_| ()),
        "Bool" => d.bool().map(|_| ()),
        other => panic!("row_len_by_types: unsupported column type {other:?} (test-only helper)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scalar_round_trips_including_the_hard_floats() {
        let floats = [
            0.0,
            -0.0,
            1.5,
            -1.5,
            f64::MIN_POSITIVE,
            f64::from_bits(1),                      // the smallest subnormal
            f64::from_bits(0x000f_ffff_ffff_ffff),  // the largest subnormal
            f64::MAX,
            f64::MIN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::from_bits(0x7ff8_0000_0000_0001),  // a quiet NaN with a payload
            f64::from_bits(0xfff8_dead_beef_0001),  // a negative NaN with a payload
            f64::from_bits(0x7ff0_0000_0000_0001),  // a signalling NaN
        ];
        let mut e = Encoder::new();
        for f in floats {
            e.f64(f);
        }
        e.u32(u32::MAX);
        e.u64(u64::MAX);
        e.i32(i32::MIN);
        e.i64(i64::MIN);
        e.bool(true);
        e.bool(false);
        let bytes = e.finish();
        let mut d = Decoder::new(&bytes);
        for f in floats {
            assert_eq!(d.f64().unwrap().to_bits(), f.to_bits(), "bit-exact: {f:?}");
        }
        assert_eq!(d.u32().unwrap(), u32::MAX);
        assert_eq!(d.u64().unwrap(), u64::MAX);
        assert_eq!(d.i32().unwrap(), i32::MIN);
        assert_eq!(d.i64().unwrap(), i64::MIN);
        assert!(d.bool().unwrap());
        assert!(!d.bool().unwrap());
        d.finish().unwrap();
    }

    #[test]
    fn strings_nullables_and_long_lengths_round_trip() {
        let long = "x".repeat(300); // a two-byte LEB128 length
        let huge = "y".repeat(20_000); // a three-byte LEB128 length
        let mut e = Encoder::new();
        e.string("");
        e.string("héllo \u{0} wörld \u{1F680}");
        e.string(&long);
        e.string(&huge);
        e.nullable_bool(None);
        e.nullable_bool(Some(true));
        e.nullable_bool(Some(false));
        e.nullable_f64(None);
        e.nullable_f64(Some(-0.0));
        e.nullable_f64(Some(f64::NAN));
        let bytes = e.finish();
        let mut d = Decoder::new(&bytes);
        assert_eq!(d.string().unwrap(), "");
        assert_eq!(d.string().unwrap(), "héllo \u{0} wörld \u{1F680}");
        assert_eq!(d.string().unwrap(), long);
        assert_eq!(d.string().unwrap(), huge);
        assert_eq!(d.nullable_bool().unwrap(), None);
        assert_eq!(d.nullable_bool().unwrap(), Some(true));
        assert_eq!(d.nullable_bool().unwrap(), Some(false));
        assert_eq!(d.nullable_f64().unwrap(), None);
        assert_eq!(d.nullable_f64().unwrap().unwrap().to_bits(), (-0.0f64).to_bits());
        assert!(d.nullable_f64().unwrap().unwrap().is_nan());
        d.finish().unwrap();
    }

    #[test]
    fn the_layout_matches_clickhouses_documented_rowbinary() {
        // A string "abc" is 0x03 'a' 'b' 'c'; a u32 is little-endian; NULL is the single byte 1.
        let mut e = Encoder::new();
        e.string("abc");
        e.u32(0x0102_0304);
        e.nullable_f64(None);
        e.nullable_bool(Some(true));
        assert_eq!(e.finish(), vec![3, b'a', b'b', b'c', 4, 3, 2, 1, 1, 0, 1]);
    }

    #[test]
    fn truncation_bad_flags_bad_utf8_and_trailing_bytes_are_typed_errors() {
        assert!(matches!(Decoder::new(&[1, 2, 3]).u32(), Err(RowBinaryError::Truncated { wanted: 4, available: 3, offset: 0 })));
        assert!(matches!(Decoder::new(&[5, b'a']).string(), Err(RowBinaryError::Truncated { .. })));
        assert!(matches!(Decoder::new(&[2]).bool(), Err(RowBinaryError::InvalidBool { value: 2, offset: 0 })));
        assert!(matches!(Decoder::new(&[7]).nullable_f64(), Err(RowBinaryError::InvalidNullFlag { value: 7, offset: 0 })));
        assert!(matches!(Decoder::new(&[2, 0xff, 0xfe]).string(), Err(RowBinaryError::InvalidUtf8 { offset: 1 })));
        // An 11-byte LEB128, and a tenth byte carrying more than the top bit, are not lengths.
        assert!(matches!(Decoder::new(&[0x80; 11]).string(), Err(RowBinaryError::BadLength { .. })));
        let mut too_big = vec![0xff; 9];
        too_big.push(0x02);
        assert!(matches!(Decoder::new(&too_big).string(), Err(RowBinaryError::BadLength { .. })));
        let mut d = Decoder::new(&[1, 2]);
        d.u8().unwrap();
        assert!(matches!(d.finish(), Err(RowBinaryError::TrailingBytes { count: 1, offset: 1 })));
    }

    #[test]
    fn row_len_by_types_walks_every_supported_type() {
        let mut e = Encoder::new();
        e.string("k");
        e.u64(1);
        e.nullable_bool(None);
        e.nullable_f64(Some(1.0));
        e.i32(3);
        e.bool(true);
        let bytes = e.finish();
        assert_eq!(row_len_by_types(&["String", "UInt64", "Nullable(Bool)", "Nullable(Float64)", "Int32", "Bool"], &bytes).unwrap(), bytes.len());
        // A schema that disagrees with the bytes does not walk cleanly.
        assert!(row_len_by_types(&["String", "UInt64", "Nullable(Bool)", "Nullable(Float64)", "Int64", "Bool"], &bytes).is_err());
    }
}

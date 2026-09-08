//! CCSDS Space Packet framing for FRAMED ports whose `Port.schema` is `"ccsds.spp"`
//! (`docs/open-questions.md` question 149; `docs/sil-plan.md`'s M22 milestone, M22.3). A
//! `PacketCodec` (`proto/altavista/v1/packet.proto`), declared per APID in the owning
//! `SystemDefinition.packet_codecs` and hashed with it, says how one CCSDS Space Packet
//! (CCSDS 133.0-B-2) maps to and from a port's user data. This module is the single place
//! that packs/unpacks the primary header and the declared `PacketField`s -- both
//! `crate::drm::schema` (load-time validation, question 149's "typed load error" rule) and any
//! future FRAMED-port runtime call into it, so there is exactly one implementation of the
//! header layout and the field bit arithmetic to get right, not two that could silently
//! disagree.
//!
//! ## Primary header (CCSDS 133.0-B-2 section 4.1.3), 6 bytes, big-endian
//!
//! ```text
//! byte 0: [version:3][type:1][sec_hdr_flag:1][apid[10:8]:3]
//! byte 1: [apid[7:0]:8]
//! byte 2: [sequence_flags:2][sequence_count[13:8]:6]
//! byte 3: [sequence_count[7:0]:8]
//! byte 4: [packet_data_length[15:8]:8]
//! byte 5: [packet_data_length[7:0]:8]
//! ```
//! `version` is always `0` (CCSDS Version-1). `type` is `PacketCodec.is_command` (`1` =
//! telecommand, `0` = telemetry -- the proto field's own doc comment). `sec_hdr_flag` is `1`
//! whenever `PacketCodec.secondary_header_bytes > 0`. `sequence_flags` is always `0b11`
//! ("unsegmented") because a FRAMED port carries exactly one space packet per `PortMessage`
//! (this module's own scope -- see the module doc comment on the task this shipped with):
//! there is no multi-packet segmentation to represent. `sequence_count` is a 14-bit value
//! supplied by the caller (`encode_packet`'s own parameter -- this module holds no counter
//! state; a running per-APID counter, if wanted, is the caller's business).
//!
//! ## Packet data length: this task's explicit convention
//!
//! CCSDS 133.0-B-2's own general rule is "one fewer than the number of octets in the Packet
//! Data Field", and the Packet Data Field is the secondary header (if any) plus the user data
//! field. **This module instead computes the length count from `user_data_bytes` alone**,
//! including `secondary_header_bytes` -- i.e.
//! `packet_data_length = secondary_header_bytes + user_data_bytes - 1`, per CCSDS
//! 133.0-B 4.1.2.5: the field counts the octets of the *Packet Data Field* (secondary
//! header plus user data) minus one. An earlier M22.3 draft computed it as
//! `user_data_bytes - 1` on an explicit instruction that was the manager's own briefing
//! error; the two coincide only when there is no secondary header, so it was corrected in
//! review before any flight software met it.
//!
//! ## Fields: big-endian, `scale`/`offset` as the engineering conversion
//!
//! `PacketField.bit_offset`/`bit_width` address bits from the start of the user data field
//! (after the primary header and any secondary header), MSB-first (big-endian bit order,
//! matching CCSDS's own big-endian byte order) -- [`read_bitfield_u64`]/[`write_bitfield_u64`]
//! below. `UINT`/`INT`/`FLOAT32`/`FLOAT64` all go through the same `f64` "engineering" value
//! ([`FieldValue::Numeric`]): `engineering = raw * scale + offset` on decode (`scale` defaulting
//! to `1.0` when the declared value is `0.0`, per the proto field's own doc comment), and the
//! algebraic inverse on encode. `BYTES` fields are raw byte copies ([`FieldValue::Bytes`]) --
//! `scale`/`offset` have no meaning for them and are not applied.
//!
//! ## Typed faults, never a silent drop (question 149)
//!
//! [`decode_packet`] returns a typed [`CodecError`] for an unknown APID, a packet shorter than
//! the primary header, a packet whose length field disagrees with its declared codec, or a
//! total byte length that disagrees with `secondary_header_bytes + user_data_bytes` -- the same
//! "never a silent drop" rule question 149 states for the port layer. [`encode_packet`]
//! likewise refuses a missing field value, a value that will not fit its declared bit width
//! after the inverse scale/offset conversion, or a field extent past `user_data_bytes` (the
//! same check [`validate_codec`] already ran at load time, repeated here defensively so this
//! module never indexes out of bounds even if handed a codec that skipped load-time
//! validation, e.g. directly from a test). Wiring these into the router's own recorded
//! port-fault bookkeeping (`drm::fault`, owned by a concurrent worker this batch) is out of
//! this module's scope; a typed `Result` is the contract a future caller maps onto that.

use std::collections::BTreeMap;

use av_cdm::pb;

/// The CCSDS primary header is always exactly 6 octets (CCSDS 133.0-B-2 section 4.1.3).
pub const PRIMARY_HEADER_LEN: usize = 6;

/// The 11-bit APID field's maximum value (`2^11 - 1`).
const MAX_APID: u32 = 0x7FF;
/// The 14-bit sequence count field's maximum value (`2^14 - 1`).
const MAX_SEQUENCE_COUNT: u32 = 0x3FFF;
/// "Unsegmented" sequence flags (CCSDS 133.0-B-2 table 4-3) -- the only value this module ever
/// writes, since a FRAMED port carries one whole space packet per `PortMessage` (see the
/// module doc comment).
const SEQUENCE_FLAGS_UNSEGMENTED: u8 = 0b11;

/// `apid -> PacketCodec`, ordered (question 149's "an ordered APID map"). **Always a
/// `BTreeMap`, never a `HashMap`** -- this is an output-shaping path (iteration order of the
/// codec set matters for anything that ever enumerates it, e.g. a future hash or log), and
/// `spoore` ADR-004 / this crate's own `lib.rs` module doc comment both fix `BTreeMap` as the
/// determinism convention for exactly this kind of map.
pub type ApidMap = BTreeMap<u32, pb::PacketCodec>;

/// One packet field's value, at the *engineering* level (`scale`/`offset` already applied on
/// decode; about to be inverted on encode) -- never the raw on-wire bits. `Numeric` covers
/// `UINT`/`INT`/`FLOAT32`/`FLOAT64` uniformly (the proto's own `engineering = raw * scale +
/// offset` formula is defined once, via `double`, regardless of the raw field's type); `Bytes`
/// covers `BYTES` fields, copied verbatim (`scale`/`offset` do not apply to them).
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Numeric(f64),
    Bytes(Vec<u8>),
}

/// A CCSDS space packet, decoded: the primary header's own fields plus every declared
/// `PacketField`, keyed by [`pb::PacketField::name`]. `fields` is a `BTreeMap` for the same
/// determinism reason [`ApidMap`] is.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPacket {
    pub apid: u32,
    pub is_command: bool,
    pub sequence_count: u16,
    pub fields: BTreeMap<String, FieldValue>,
}

/// Every way a `PacketCodec`/packet can be malformed, typed rather than silently dropped or
/// panicking (question 149). Load-time variants ([`validate_codec`]/
/// [`validate_system_packet_codecs`], called from `crate::drm::schema`) and runtime
/// ([`encode_packet`]/[`decode_packet`]) variants share this one enum, the same way
/// `crate::router::RouterError` covers both its own load-time and (today, none) runtime
/// checks.
#[derive(Debug, Clone, PartialEq)]
pub enum CodecError {
    /// `PacketCodec.apid` does not fit the CCSDS 11-bit APID field (0..=2047).
    ApidOutOfRange { codec_id: String, apid: u32 },
    /// `PacketCodec.user_data_bytes` is `0` (the packet data length field, `user_data_bytes -
    /// 1`, would underflow) or exceeds `65536` (the length count would not fit the 16-bit
    /// field once decremented).
    InvalidUserDataBytes { codec_id: String, user_data_bytes: u32, reason: String },
    /// `PacketField.type` was `PACKET_FIELD_TYPE_UNSPECIFIED`.
    UnspecifiedFieldType { codec_id: String, field: String },
    /// `PacketField.bit_width` is `0`, or disagrees with a fixed-width type (`FLOAT32` must be
    /// 32, `FLOAT64` must be 64, `BYTES` must be a multiple of 8, `INT` must be 2..=64, `UINT`
    /// must be 1..=64), or (`BYTES` only) `bit_offset` is not byte-aligned -- this module's own
    /// `BYTES` implementation copies whole bytes, so a `BYTES` field can only be realized
    /// starting on a byte boundary.
    InvalidFieldWidth { codec_id: String, field: String, type_name: &'static str, bit_offset: u32, bit_width: u32, reason: String },
    /// `bit_offset + bit_width` runs past `user_data_bytes * 8` -- question 149's own example
    /// of a load-time-only check.
    FieldExtentExceedsUserData { codec_id: String, field: String, bit_offset: u32, bit_width: u32, user_data_bytes: u32 },
    /// Two `PacketCodec`s in the same `SystemDefinition.packet_codecs` declared the same
    /// `apid`.
    DuplicateApid { apid: u32, first_codec_id: String, second_codec_id: String },

    /// `encode_packet`'s `sequence_count` argument does not fit the CCSDS 14-bit sequence
    /// count field (0..=16383).
    SequenceCountOutOfRange { sequence_count: u32 },
    /// `encode_packet`'s `secondary_header` argument's length did not equal the codec's own
    /// declared `secondary_header_bytes`.
    SecondaryHeaderLengthMismatch { codec_id: String, expected: u32, actual: usize },
    /// `encode_packet` was not given a value for a declared field.
    MissingFieldValue { codec_id: String, field: String },
    /// `encode_packet` was given a [`FieldValue`] variant that does not match the field's
    /// declared type (a `Bytes` value for a numeric field, or vice versa).
    FieldValueTypeMismatch { codec_id: String, field: String, expected: &'static str },
    /// A numeric engineering value did not fit its field once the inverse scale/offset
    /// conversion was applied (out of range for the declared bit width, or a `BYTES` value of
    /// the wrong length).
    FieldValueOutOfRange { codec_id: String, field: String, reason: String },

    /// `decode_packet` was given fewer than [`PRIMARY_HEADER_LEN`] bytes -- there is no
    /// primary header to even read an APID from.
    PacketTooShortForHeader { actual: usize },
    /// `decode_packet`'s parsed APID names no codec in the [`ApidMap`] it was given.
    UnknownApid { apid: u32 },
    /// The primary header's packet data length field, converted back to a user-data byte
    /// count (`length_field + 1`, the inverse of this module's own encode convention -- see
    /// the module doc comment's "Packet data length" section), disagrees with the matched
    /// codec's own declared `user_data_bytes`.
    PacketDataLengthMismatch { apid: u32, codec_id: String, expected_user_data_bytes: u32, actual_user_data_bytes: u32 },
    /// The buffer `decode_packet` was given is not exactly `PRIMARY_HEADER_LEN +
    /// secondary_header_bytes + user_data_bytes` long (checked after the length-field
    /// cross-check above already passed, so this catches a caller handing over the wrong
    /// number of bytes even when the header's own internal length field was self-consistent).
    PacketLengthMismatch { apid: u32, codec_id: String, expected_total_bytes: usize, actual_total_bytes: usize },

    /// [`measurements_from_field_values`] was handed a declared noise covariance (`r`, row-major)
    /// for `measurement_id` that failed [`av_cdm::covariance::check_spd_row_major`] -- question
    /// 173's own "if `r` is populated it must be SPD" rule, caught rather than silently shipped.
    /// A caller bug (a badly-built diagonal, or a dimension that does not match `z`), never a
    /// condition a well-formed DRM run should hit -- see that function's own doc comment.
    MeasurementNoiseNotSpd { measurement_id: String, reason: String },
    /// [`measurements_from_field_values`] found `field` (a non-empty `PacketField.target`, so a
    /// measurement component is expected from it) with no entry at all in the `values` map it was
    /// given -- question 149's own no-silent-drop rule: a targeted field losing its component
    /// silently would quietly shrink a `Measurement.z` with nothing to say so. A caller bug (every
    /// declared field should always be present in what [`decode_packet`] returns), never a
    /// condition a well-formed caller should hit -- see that function's own doc comment. An
    /// empty-`target` field is not this: it is still skipped silently, by design.
    MeasurementFieldValueMissing { codec_id: String, field: String },
    /// [`measurements_from_field_values`] found `field` (a non-empty `PacketField.target`) whose
    /// decoded value was [`FieldValue::Bytes`], not [`FieldValue::Numeric`] -- a `Measurement`'s
    /// `z` is `f64` components only, and question 149's own no-silent-drop rule means this is a
    /// typed refusal, not a quietly-shrunk `z`. An empty-`target` field is not this: it is still
    /// skipped silently, by design, regardless of its value's type.
    MeasurementFieldValueNotNumeric { codec_id: String, field: String },
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::ApidOutOfRange { codec_id, apid } => write!(f, "codec {codec_id:?}: apid {apid} does not fit the CCSDS 11-bit APID field (0..=2047)"),
            CodecError::InvalidUserDataBytes { codec_id, user_data_bytes, reason } => write!(f, "codec {codec_id:?}: user_data_bytes {user_data_bytes} invalid: {reason}"),
            CodecError::UnspecifiedFieldType { codec_id, field } => write!(f, "codec {codec_id:?} field {field:?}: type is PACKET_FIELD_TYPE_UNSPECIFIED"),
            CodecError::InvalidFieldWidth { codec_id, field, type_name, bit_offset, bit_width, reason } => {
                write!(f, "codec {codec_id:?} field {field:?} ({type_name}, bit_offset={bit_offset}, bit_width={bit_width}): {reason}")
            }
            CodecError::FieldExtentExceedsUserData { codec_id, field, bit_offset, bit_width, user_data_bytes } => write!(
                f,
                "codec {codec_id:?} field {field:?}: bit_offset {bit_offset} + bit_width {bit_width} runs past user_data_bytes {user_data_bytes} ({} bits)",
                (*user_data_bytes as u64) * 8
            ),
            CodecError::DuplicateApid { apid, first_codec_id, second_codec_id } => {
                write!(f, "apid {apid} is declared by both codec {first_codec_id:?} and codec {second_codec_id:?}")
            }
            CodecError::SequenceCountOutOfRange { sequence_count } => write!(f, "sequence_count {sequence_count} does not fit the CCSDS 14-bit sequence count field (0..=16383)"),
            CodecError::SecondaryHeaderLengthMismatch { codec_id, expected, actual } => {
                write!(f, "codec {codec_id:?}: secondary_header is {actual} byte(s), but secondary_header_bytes declares {expected}")
            }
            CodecError::MissingFieldValue { codec_id, field } => write!(f, "codec {codec_id:?}: no value supplied for field {field:?}"),
            CodecError::FieldValueTypeMismatch { codec_id, field, expected } => write!(f, "codec {codec_id:?} field {field:?}: expected a {expected} value"),
            CodecError::FieldValueOutOfRange { codec_id, field, reason } => write!(f, "codec {codec_id:?} field {field:?}: {reason}"),
            CodecError::PacketTooShortForHeader { actual } => write!(f, "packet is {actual} byte(s), shorter than the {PRIMARY_HEADER_LEN}-byte primary header"),
            CodecError::UnknownApid { apid } => write!(f, "apid {apid} names no declared PacketCodec"),
            CodecError::PacketDataLengthMismatch { apid, codec_id, expected_user_data_bytes, actual_user_data_bytes } => write!(
                f,
                "apid {apid} (codec {codec_id:?}): packet data length field implies {actual_user_data_bytes} user data byte(s), but the codec declares user_data_bytes {expected_user_data_bytes}"
            ),
            CodecError::PacketLengthMismatch { apid, codec_id, expected_total_bytes, actual_total_bytes } => write!(
                f,
                "apid {apid} (codec {codec_id:?}): expected a {expected_total_bytes}-byte packet (primary header + secondary header + user data), got {actual_total_bytes}"
            ),
            CodecError::MeasurementNoiseNotSpd { measurement_id, reason } => write!(f, "measurement {measurement_id:?}: declared noise covariance is not SPD: {reason}"),
            CodecError::MeasurementFieldValueMissing { codec_id, field } => write!(f, "codec {codec_id:?}: measurement field {field:?} declares a target but has no decoded value"),
            CodecError::MeasurementFieldValueNotNumeric { codec_id, field } => write!(f, "codec {codec_id:?}: measurement field {field:?} declares a target but its decoded value is not Numeric"),
        }
    }
}
impl std::error::Error for CodecError {}

// --------------------------------------------------------------------------------------
// Load-time validation (question 149, called from `crate::drm::schema`)
// --------------------------------------------------------------------------------------

/// Validate one `PacketCodec` against the checks question 149 asks for at load time: a field
/// extent past `user_data_bytes`, a `bit_width` disagreeing with its declared type, an
/// unspecified field type, or a `user_data_bytes`/`apid` that cannot be represented on the
/// wire at all. Does **not** check for a duplicate `apid` against any other codec -- that is
/// [`validate_system_packet_codecs`]'s job, since it alone sees the whole set.
pub fn validate_codec(codec: &pb::PacketCodec) -> Result<(), CodecError> {
    if codec.apid > MAX_APID {
        return Err(CodecError::ApidOutOfRange { codec_id: codec.id.clone(), apid: codec.apid });
    }
    if codec.user_data_bytes == 0 || codec.user_data_bytes > 65536 {
        return Err(CodecError::InvalidUserDataBytes {
            codec_id: codec.id.clone(),
            user_data_bytes: codec.user_data_bytes,
            reason: "must be in 1..=65536 so the CCSDS packet data length field (secondary_header_bytes + user_data_bytes - 1) fits its 16-bit wire representation".to_string(),
        });
    }
    let user_data_bits = (codec.user_data_bytes as u64) * 8;
    for field in &codec.fields {
        let ty = pb::PacketFieldType::try_from(field.r#type).unwrap_or(pb::PacketFieldType::Unspecified);
        if ty == pb::PacketFieldType::Unspecified {
            return Err(CodecError::UnspecifiedFieldType { codec_id: codec.id.clone(), field: field.name.clone() });
        }
        if field.bit_width == 0 {
            return Err(CodecError::InvalidFieldWidth {
                codec_id: codec.id.clone(),
                field: field.name.clone(),
                type_name: ty.as_str_name(),
                bit_offset: field.bit_offset,
                bit_width: field.bit_width,
                reason: "bit_width must be nonzero".to_string(),
            });
        }
        let bit_end = (field.bit_offset as u64) + (field.bit_width as u64);
        if bit_end > user_data_bits {
            return Err(CodecError::FieldExtentExceedsUserData {
                codec_id: codec.id.clone(),
                field: field.name.clone(),
                bit_offset: field.bit_offset,
                bit_width: field.bit_width,
                user_data_bytes: codec.user_data_bytes,
            });
        }
        let width_ok = match ty {
            pb::PacketFieldType::Uint => field.bit_width <= 64,
            pb::PacketFieldType::Int => (2..=64).contains(&field.bit_width),
            pb::PacketFieldType::Float32 => field.bit_width == 32,
            pb::PacketFieldType::Float64 => field.bit_width == 64,
            pb::PacketFieldType::Bytes => field.bit_width % 8 == 0 && field.bit_offset % 8 == 0,
            pb::PacketFieldType::Unspecified => unreachable!("checked above"),
        };
        if !width_ok {
            let reason = match ty {
                pb::PacketFieldType::Uint => "UINT bit_width must be 1..=64".to_string(),
                pb::PacketFieldType::Int => "INT bit_width must be 2..=64".to_string(),
                pb::PacketFieldType::Float32 => "FLOAT32 bit_width must be exactly 32 (IEEE-754 binary32)".to_string(),
                pb::PacketFieldType::Float64 => "FLOAT64 bit_width must be exactly 64 (IEEE-754 binary64)".to_string(),
                pb::PacketFieldType::Bytes => "BYTES bit_width must be a multiple of 8 and bit_offset must be byte-aligned".to_string(),
                pb::PacketFieldType::Unspecified => unreachable!("checked above"),
            };
            return Err(CodecError::InvalidFieldWidth {
                codec_id: codec.id.clone(),
                field: field.name.clone(),
                type_name: ty.as_str_name(),
                bit_offset: field.bit_offset,
                bit_width: field.bit_width,
                reason,
            });
        }
    }
    Ok(())
}

/// Validate every codec in a `SystemDefinition.packet_codecs` list and fold them into the
/// ordered [`ApidMap`] question 149 asks for -- the single source of truth `crate::drm::schema`
/// calls at load time and any future runtime decode path reuses, so the two can never
/// disagree about what counts as a valid codec set.
pub fn validate_system_packet_codecs(codecs: &[pb::PacketCodec]) -> Result<ApidMap, CodecError> {
    let mut map: ApidMap = BTreeMap::new();
    for codec in codecs {
        validate_codec(codec)?;
        if let Some(existing) = map.insert(codec.apid, codec.clone()) {
            return Err(CodecError::DuplicateApid { apid: codec.apid, first_codec_id: existing.id, second_codec_id: codec.id.clone() });
        }
    }
    Ok(map)
}

// --------------------------------------------------------------------------------------
// Bit-level field packing (big-endian, MSB-first -- CCSDS's own byte order)
// --------------------------------------------------------------------------------------

/// Read `bit_width` (1..=64) bits starting at `bit_offset`, MSB-first, from `data`, returned
/// right-aligned in a `u64`. A direct bit-by-bit loop rather than a byte-shifting trick:
/// `bit_width`/`bit_offset` here are always small (a packet field, not a bulk buffer), so the
/// O(bit_width) cost is irrelevant, and a loop this literal is easy to check by hand against
/// the CCSDS bit numbering it implements (bit 0 of a byte is its MSB) -- exactly what the
/// hand-computed tests below do.
fn read_bitfield_u64(data: &[u8], bit_offset: u32, bit_width: u32) -> u64 {
    let mut value: u64 = 0;
    for i in 0..bit_width {
        let bit_pos = bit_offset + i;
        let byte_idx = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8);
        let bit = (data[byte_idx] >> bit_in_byte) & 1;
        value = (value << 1) | (bit as u64);
    }
    value
}

/// The inverse of [`read_bitfield_u64`]: write the low `bit_width` bits of `value` into `data`
/// starting at `bit_offset`, MSB-first.
fn write_bitfield_u64(data: &mut [u8], bit_offset: u32, bit_width: u32, value: u64) {
    for i in 0..bit_width {
        let bit_pos = bit_offset + i;
        let byte_idx = (bit_pos / 8) as usize;
        let bit_in_byte = 7 - (bit_pos % 8);
        let bit = ((value >> (bit_width - 1 - i)) & 1) as u8;
        data[byte_idx] = (data[byte_idx] & !(1 << bit_in_byte)) | (bit << bit_in_byte);
    }
}

/// Two's-complement sign-extend a `bit_width`-bit raw value (already right-aligned in `raw` by
/// [`read_bitfield_u64`]) to a full `i64`.
fn sign_extend(raw: u64, bit_width: u32) -> i64 {
    let shift = 64 - bit_width;
    ((raw << shift) as i64) >> shift
}

/// The largest value an unsigned field of `bit_width` bits can hold.
fn max_uint(bit_width: u32) -> u64 {
    if bit_width >= 64 {
        u64::MAX
    } else {
        (1u64 << bit_width) - 1
    }
}

/// The `(min, max)` a two's-complement signed field of `bit_width` bits can hold.
fn int_range(bit_width: u32) -> (i64, i64) {
    if bit_width >= 64 {
        (i64::MIN, i64::MAX)
    } else {
        let max = (1i64 << (bit_width - 1)) - 1;
        let min = -(1i64 << (bit_width - 1));
        (min, max)
    }
}

fn effective_scale(scale: f64) -> f64 {
    // "scale defaults to 1 when 0 is given" -- PacketField.scale's own proto doc comment.
    if scale == 0.0 {
        1.0
    } else {
        scale
    }
}

fn numeric_value(value: &FieldValue, codec_id: &str, field: &str) -> Result<f64, CodecError> {
    match value {
        FieldValue::Numeric(v) => Ok(*v),
        FieldValue::Bytes(_) => Err(CodecError::FieldValueTypeMismatch { codec_id: codec_id.to_string(), field: field.to_string(), expected: "Numeric" }),
    }
}

/// Defensive re-check of one field's extent against the buffer it is about to read/write --
/// the same arithmetic [`validate_codec`] already ran at load time, repeated here so this
/// module never indexes out of bounds even when handed a codec that skipped that load-time
/// call (this module's own unit tests build `pb::PacketCodec` values directly, without going
/// through `crate::drm::schema`).
fn check_field_extent(field: &pb::PacketField, user_data_bytes: u32, codec_id: &str) -> Result<(), CodecError> {
    let bit_end = (field.bit_offset as u64) + (field.bit_width as u64);
    if bit_end > (user_data_bytes as u64) * 8 {
        return Err(CodecError::FieldExtentExceedsUserData {
            codec_id: codec_id.to_string(),
            field: field.name.clone(),
            bit_offset: field.bit_offset,
            bit_width: field.bit_width,
            user_data_bytes,
        });
    }
    Ok(())
}

fn read_field(user_data: &[u8], field: &pb::PacketField, codec_id: &str) -> Result<FieldValue, CodecError> {
    let ty = pb::PacketFieldType::try_from(field.r#type).unwrap_or(pb::PacketFieldType::Unspecified);
    let scale = effective_scale(field.scale);
    match ty {
        pb::PacketFieldType::Uint => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, field.bit_width);
            Ok(FieldValue::Numeric((raw as f64) * scale + field.offset))
        }
        pb::PacketFieldType::Int => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, field.bit_width);
            let signed = sign_extend(raw, field.bit_width);
            Ok(FieldValue::Numeric((signed as f64) * scale + field.offset))
        }
        pb::PacketFieldType::Float32 => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, 32) as u32;
            Ok(FieldValue::Numeric((f32::from_bits(raw) as f64) * scale + field.offset))
        }
        pb::PacketFieldType::Float64 => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, 64);
            Ok(FieldValue::Numeric(f64::from_bits(raw) * scale + field.offset))
        }
        pb::PacketFieldType::Bytes => {
            let start = (field.bit_offset / 8) as usize;
            let len = (field.bit_width / 8) as usize;
            Ok(FieldValue::Bytes(user_data[start..start + len].to_vec()))
        }
        pb::PacketFieldType::Unspecified => Err(CodecError::UnspecifiedFieldType { codec_id: codec_id.to_string(), field: field.name.clone() }),
    }
}

fn write_field(user_data: &mut [u8], field: &pb::PacketField, value: &FieldValue, codec_id: &str) -> Result<(), CodecError> {
    let ty = pb::PacketFieldType::try_from(field.r#type).unwrap_or(pb::PacketFieldType::Unspecified);
    let scale = effective_scale(field.scale);
    match ty {
        pb::PacketFieldType::Uint => {
            let eng = numeric_value(value, codec_id, &field.name)?;
            let raw = (eng - field.offset) / scale;
            if !raw.is_finite() || raw < 0.0 || raw.round() > max_uint(field.bit_width) as f64 {
                return Err(CodecError::FieldValueOutOfRange {
                    codec_id: codec_id.to_string(),
                    field: field.name.clone(),
                    reason: format!("engineering value {eng} does not fit an unsigned {}-bit field after the inverse scale/offset conversion", field.bit_width),
                });
            }
            write_bitfield_u64(user_data, field.bit_offset, field.bit_width, raw.round() as u64);
        }
        pb::PacketFieldType::Int => {
            let eng = numeric_value(value, codec_id, &field.name)?;
            let raw = (eng - field.offset) / scale;
            let (min, max) = int_range(field.bit_width);
            if !raw.is_finite() || raw.round() < min as f64 || raw.round() > max as f64 {
                return Err(CodecError::FieldValueOutOfRange {
                    codec_id: codec_id.to_string(),
                    field: field.name.clone(),
                    reason: format!("engineering value {eng} does not fit a signed {}-bit field after the inverse scale/offset conversion", field.bit_width),
                });
            }
            let raw_i64 = raw.round() as i64;
            write_bitfield_u64(user_data, field.bit_offset, field.bit_width, (raw_i64 as u64) & max_uint(field.bit_width));
        }
        pb::PacketFieldType::Float32 => {
            let eng = numeric_value(value, codec_id, &field.name)?;
            let raw = ((eng - field.offset) / scale) as f32;
            write_bitfield_u64(user_data, field.bit_offset, 32, raw.to_bits() as u64);
        }
        pb::PacketFieldType::Float64 => {
            let eng = numeric_value(value, codec_id, &field.name)?;
            let raw = (eng - field.offset) / scale;
            write_bitfield_u64(user_data, field.bit_offset, 64, raw.to_bits());
        }
        pb::PacketFieldType::Bytes => {
            let bytes = match value {
                FieldValue::Bytes(b) => b,
                FieldValue::Numeric(_) => return Err(CodecError::FieldValueTypeMismatch { codec_id: codec_id.to_string(), field: field.name.clone(), expected: "Bytes" }),
            };
            let expected_len = (field.bit_width / 8) as usize;
            if bytes.len() != expected_len {
                return Err(CodecError::FieldValueOutOfRange {
                    codec_id: codec_id.to_string(),
                    field: field.name.clone(),
                    reason: format!("BYTES field expects {expected_len} byte(s), got {}", bytes.len()),
                });
            }
            let start = (field.bit_offset / 8) as usize;
            user_data[start..start + expected_len].copy_from_slice(bytes);
        }
        pb::PacketFieldType::Unspecified => return Err(CodecError::UnspecifiedFieldType { codec_id: codec_id.to_string(), field: field.name.clone() }),
    }
    Ok(())
}

// --------------------------------------------------------------------------------------
// Encode / decode
// --------------------------------------------------------------------------------------

/// Encode one CCSDS space packet for `codec`: primary header, `secondary_header` verbatim
/// (must be exactly `codec.secondary_header_bytes` long), then every declared `PacketField`
/// packed from `values` (keyed by [`pb::PacketField::name`]). `sequence_flags` is always
/// "unsegmented" (see the module doc comment); `sequence_count` is the caller's own 14-bit
/// counter value.
pub fn encode_packet(codec: &pb::PacketCodec, sequence_count: u16, secondary_header: &[u8], values: &BTreeMap<String, FieldValue>) -> Result<Vec<u8>, CodecError> {
    if codec.apid > MAX_APID {
        return Err(CodecError::ApidOutOfRange { codec_id: codec.id.clone(), apid: codec.apid });
    }
    if sequence_count as u32 > MAX_SEQUENCE_COUNT {
        return Err(CodecError::SequenceCountOutOfRange { sequence_count: sequence_count as u32 });
    }
    if secondary_header.len() != codec.secondary_header_bytes as usize {
        return Err(CodecError::SecondaryHeaderLengthMismatch { codec_id: codec.id.clone(), expected: codec.secondary_header_bytes, actual: secondary_header.len() });
    }
    if codec.user_data_bytes == 0 || codec.user_data_bytes > 65536 {
        return Err(CodecError::InvalidUserDataBytes {
            codec_id: codec.id.clone(),
            user_data_bytes: codec.user_data_bytes,
            reason: "must be in 1..=65536 so the CCSDS packet data length field (secondary_header_bytes + user_data_bytes - 1) fits its 16-bit wire representation".to_string(),
        });
    }

    let user_data_bytes = codec.user_data_bytes as usize;
    let mut user_data = vec![0u8; user_data_bytes];
    for field in &codec.fields {
        check_field_extent(field, codec.user_data_bytes, &codec.id)?;
        let value = values.get(&field.name).ok_or_else(|| CodecError::MissingFieldValue { codec_id: codec.id.clone(), field: field.name.clone() })?;
        write_field(&mut user_data, field, value, &codec.id)?;
    }

    let packet_data_field_bytes = secondary_header.len() + user_data_bytes;
    let sec_hdr_flag: u8 = if codec.secondary_header_bytes > 0 { 1 } else { 0 };
    let type_bit: u8 = if codec.is_command { 1 } else { 0 };
    let byte0 = (type_bit << 4) | (sec_hdr_flag << 3) | (((codec.apid >> 8) & 0x07) as u8);
    let byte1 = (codec.apid & 0xFF) as u8;
    let byte2 = (SEQUENCE_FLAGS_UNSEGMENTED << 6) | (((sequence_count >> 8) & 0x3F) as u8);
    let byte3 = (sequence_count & 0xFF) as u8;
    // Packet data length (CCSDS 133.0-B, 4.1.2.5): the number of octets in the *Packet Data
    // Field* minus one. The Packet Data Field is the secondary header plus the user data, so
    // this is `secondary_header_bytes + user_data_bytes - 1`. An earlier M22.3 draft used
    // `user_data_bytes - 1`, which silently drops the secondary header from the count and only
    // coincides with the standard when there is no secondary header; the manager review
    // corrected it (that instruction was the manager's own briefing error, not the worker's).
    // `packet_data_field_bytes` is checked to be in 1..=65536 above, so this cannot underflow
    // and the result always fits u16.
    let packet_data_length = (packet_data_field_bytes as u32) - 1;
    let byte4 = ((packet_data_length >> 8) & 0xFF) as u8;
    let byte5 = (packet_data_length & 0xFF) as u8;

    let mut packet = Vec::with_capacity(PRIMARY_HEADER_LEN + secondary_header.len() + user_data_bytes);
    packet.extend_from_slice(&[byte0, byte1, byte2, byte3, byte4, byte5]);
    packet.extend_from_slice(secondary_header);
    packet.extend_from_slice(&user_data);
    Ok(packet)
}

/// Decode one CCSDS space packet against the codec its APID names in `apid_map`. See the
/// module doc comment's "Typed faults" section for exactly which malformed shapes are refused
/// and how.
pub fn decode_packet(apid_map: &ApidMap, data: &[u8]) -> Result<DecodedPacket, CodecError> {
    if data.len() < PRIMARY_HEADER_LEN {
        return Err(CodecError::PacketTooShortForHeader { actual: data.len() });
    }
    let (byte0, byte1, byte2, byte3) = (data[0], data[1], data[2], data[3]);
    let length_field = ((data[4] as u32) << 8) | (data[5] as u32);
    let apid = (((byte0 & 0x07) as u32) << 8) | (byte1 as u32);
    let is_command = ((byte0 >> 4) & 0x1) == 1;
    let sequence_count = (((byte2 & 0x3F) as u16) << 8) | (byte3 as u16);

    let codec = apid_map.get(&apid).ok_or(CodecError::UnknownApid { apid })?;

    // Inverse of encode_packet: the length field counts the whole Packet Data Field
    // (secondary header + user data) minus one, per CCSDS 133.0-B 4.1.2.5.
    let secondary_header_bytes = codec.secondary_header_bytes as usize;
    let user_data_bytes = codec.user_data_bytes as usize;
    let declared_packet_data_field_bytes = (length_field as usize) + 1;
    let expected_packet_data_field_bytes = secondary_header_bytes + user_data_bytes;
    if declared_packet_data_field_bytes != expected_packet_data_field_bytes {
        let actual_user_data_bytes = declared_packet_data_field_bytes.saturating_sub(secondary_header_bytes) as u32;
        return Err(CodecError::PacketDataLengthMismatch { apid, codec_id: codec.id.clone(), expected_user_data_bytes: codec.user_data_bytes, actual_user_data_bytes });
    }
    let expected_total = PRIMARY_HEADER_LEN + secondary_header_bytes + user_data_bytes;
    if data.len() != expected_total {
        return Err(CodecError::PacketLengthMismatch { apid, codec_id: codec.id.clone(), expected_total_bytes: expected_total, actual_total_bytes: data.len() });
    }

    let user_data = &data[PRIMARY_HEADER_LEN + secondary_header_bytes..expected_total];
    let mut fields = BTreeMap::new();
    for field in &codec.fields {
        check_field_extent(field, codec.user_data_bytes, &codec.id)?;
        let value = read_field(user_data, field, &codec.id)?;
        fields.insert(field.name.clone(), value);
    }
    Ok(DecodedPacket { apid, is_command, sequence_count, fields })
}

// --------------------------------------------------------------------------------------
// Question 173 (M25.3): FRAMED telemetry -> CDM `Measurement`s, via `PacketField.target`.
// --------------------------------------------------------------------------------------

/// Build every CDM `Measurement` (`av_cdm::pb::Measurement`) one FRAMED **telemetry** packet's
/// declared field values map to, per `packet.proto`'s own `PacketField.target` doc comment:
/// `"<measurement_id>/<label>"` for a mapped field, empty ("recorded but not mapped") for one
/// that is not. This is `PacketField.target`'s first telemetry consumer -- `crate::drm::
/// gmat_command::resolve_gmat_command_port` (M25.2b) was its first consumer at all, for a
/// **command**'s single writable-parameter target; this is the same field, read the same way,
/// for telemetry's many-components-per-packet shape.
///
/// **Never invents a measurement.** `codec.is_command == true` (a command, not telemetry) or a
/// codec whose fields all declare an empty `target` both return an empty `Vec` -- nothing is
/// synthesized for a packet whose codec does not map it (question 173's own required test).
///
/// **Grouping and order.** Fields are walked in `codec.fields`' own declared order (the codec's
/// authored, hashed order -- deterministic, and the only order this module has any business
/// treating as meaningful). A field's `target` is split on its *last* `/`: the part before is
/// the measurement id, the part after is a human label this function does not otherwise use.
/// Every field sharing one measurement id becomes that one `Measurement`'s `z`, in the order
/// those fields were declared -- so one packet with fields mapped to two different measurement
/// ids (not used by any codec in this workspace today, but not refused either) produces two
/// `Measurement`s. A field with an empty `target` is skipped, not defaulted -- that is the
/// declared "recorded but not mapped" case, and is silent by design. A field with a **non-empty**
/// `target` (a measurement component is expected from it) whose value is absent from `values`, or
/// whose value is [`FieldValue::Bytes`] (this module's `z` is `f64` only), is a typed error
/// ([`CodecError::MeasurementFieldValueMissing`] / [`CodecError::MeasurementFieldValueNotNumeric`])
/// -- question 149's own no-silent-drop rule: unlike the empty-`target` case, this is a caller bug
/// (should not happen -- every declared field is always present in what [`decode_packet`] returns,
/// and every caller here supplies exactly the values it is about to -- or just did -- pass to
/// [`encode_packet`]), and a well-formed measurement component quietly disappearing from `z` must
/// never pass silently.
///
/// **`r` (SPD, checked) only where the caller can honestly declare it.** `noise_by_measurement_id`
/// supplies a row-major covariance for a measurement id the caller knows a genuine noise model
/// for (e.g. an IMU's independent per-axis white noise, `sigma^2` on the diagonal); a measurement
/// id absent from that map gets an empty `r` -- question 173's own "leave `r` empty rather than
/// inventing a value" rule, for e.g. a star tracker's unit-quaternion measurement, whose declared
/// `noise_sigma_rad` is a 3-parameter small-angle tangent-space sigma, not a diagonal covariance
/// on the 4 over-parameterized `[qx,qy,qz,qw]` components (the unit-norm constraint means the
/// honest `R` needs a linearizing Jacobian this function does not have and this task does not
/// build). A supplied `r` is checked SPD via [`av_cdm::covariance::check_spd_row_major`]
/// (question 173: "if `r` is populated it must be SPD") -- [`CodecError::MeasurementNoiseNotSpd`]
/// on failure, never shipped unchecked.
pub fn measurements_from_field_values(
    codec: &pb::PacketCodec,
    values: &BTreeMap<String, FieldValue>,
    epoch_ns: i64,
    sensor_id: &str,
    frame_id: &str,
    noise_by_measurement_id: &BTreeMap<String, Vec<f64>>,
) -> Result<Vec<pb::Measurement>, CodecError> {
    if codec.is_command {
        return Ok(Vec::new());
    }
    // Preserve first-seen order (a plain `Vec`, not a `BTreeMap`, keyed by measurement id):
    // insertion order here is `codec.fields`' own declared order, not alphabetical.
    let mut groups: Vec<(String, Vec<f64>)> = Vec::new();
    for field in &codec.fields {
        if field.target.is_empty() {
            continue;
        }
        let measurement_id = match field.target.rsplit_once('/') {
            Some((id, _label)) => id.to_string(),
            None => field.target.clone(),
        };
        let value = values.get(&field.name).ok_or_else(|| CodecError::MeasurementFieldValueMissing { codec_id: codec.id.clone(), field: field.name.clone() })?;
        let v = match value {
            FieldValue::Numeric(v) => *v,
            FieldValue::Bytes(_) => return Err(CodecError::MeasurementFieldValueNotNumeric { codec_id: codec.id.clone(), field: field.name.clone() }),
        };
        match groups.iter_mut().find(|(id, _)| *id == measurement_id) {
            Some((_, z)) => z.push(v),
            None => groups.push((measurement_id, vec![v])),
        }
    }
    let mut out = Vec::with_capacity(groups.len());
    for (measurement_id, z) in groups {
        let r = match noise_by_measurement_id.get(&measurement_id) {
            Some(r) => {
                av_cdm::covariance::check_spd_row_major(r, z.len(), &format!("Measurement {measurement_id:?} noise (sensor {sensor_id:?})"))
                    .map_err(|e| CodecError::MeasurementNoiseNotSpd { measurement_id: measurement_id.clone(), reason: e.to_string() })?;
                r.clone()
            }
            None => Vec::new(),
        };
        out.push(pb::Measurement { measurement_id, z, r, epoch_ns, sensor_id: sensor_id.to_string(), frame_id: frame_id.to_string(), ..Default::default() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, bit_offset: u32, bit_width: u32, ty: pb::PacketFieldType, scale: f64, offset: f64) -> pb::PacketField {
        pb::PacketField { name: name.to_string(), bit_offset, bit_width, r#type: ty as i32, unit: pb::Unit::Unspecified as i32, scale, offset, target: String::new() }
    }

    fn codec(id: &str, apid: u32, is_command: bool, secondary_header_bytes: u32, user_data_bytes: u32, fields: Vec<pb::PacketField>) -> pb::PacketCodec {
        pb::PacketCodec { id: id.to_string(), apid, is_command, secondary_header_bytes, fields, user_data_bytes, description: String::new() }
    }

    // -----------------------------------------------------------------------------------
    // Hand-computed CCSDS packets, pinned against bytes derived from CCSDS 133.0-B-2
    // directly (not against this module's own encoder/decoder output on either side).
    //
    // Scenario: APID 291 (0x123), telemetry (type bit 0), no secondary header, one UINT
    // field spanning all 4 user-data bytes carrying 0x01020304, sequence_count 42.
    //
    // Header bit layout (CCSDS 133.0-B-2 sec 4.1.3), computed by hand:
    //   291 decimal = 0b100100011 (9 bits) = 0b001_00100011 as 11 bits
    //     -> top 3 bits (apid[10:8])  = 0b001
    //     -> bottom 8 bits (apid[7:0]) = 0b00100011 = 0x23
    //   byte0 = version(000) type(0) sec_hdr_flag(0) apid[10:8](001) = 0b00000001 = 0x01
    //   byte1 = apid[7:0] = 0x23
    //   sequence_count 42 = 0b00000000101010 (14 bits)
    //     -> top 6 bits = 0b000000, bottom 8 bits = 0b00101010 = 0x2A
    //   byte2 = sequence_flags(11, unsegmented) seq[13:8](000000) = 0b11000000 = 0xC0
    //   byte3 = seq[7:0] = 0x2A
    //   packet_data_length = user_data_bytes(4) - 1 = 3 = 0x0003 -> byte4=0x00, byte5=0x03
    //   user data = 0x01 0x02 0x03 0x04 (the field's raw big-endian bytes)
    // Full packet: 01 23 C0 2A 00 03 01 02 03 04  (10 bytes)
    // -----------------------------------------------------------------------------------

    const HAND_COMPUTED_PACKET: [u8; 10] = [0x01, 0x23, 0xC0, 0x2A, 0x00, 0x03, 0x01, 0x02, 0x03, 0x04];

    fn hand_computed_codec() -> pb::PacketCodec {
        codec("tm_hand", 291, false, 0, 4, vec![field("word", 0, 32, pb::PacketFieldType::Uint, 1.0, 0.0)])
    }

    /// `encode_packet`'s own output pinned against the byte sequence above, derived from the
    /// standard's bit layout by hand, not by running the encoder and trusting it. Fails
    /// against any wrong-but-self-consistent header layout that a pure round-trip test could
    /// never catch -- e.g. swapping the version/type/sec_hdr_flag/apid[10:8] bit order within
    /// byte 0, or an off-by-one on the packet data length (`user_data_bytes` instead of
    /// `user_data_bytes - 1`, which would encode byte5 as 0x04, not 0x03).
    #[test]
    fn hand_computed_encode_matches_ccsds_bit_layout_pinned_by_hand() {
        let c = hand_computed_codec();
        let mut values = BTreeMap::new();
        values.insert("word".to_string(), FieldValue::Numeric(0x01020304u32 as f64));
        let packet = encode_packet(&c, 42, &[], &values).expect("encodes");
        assert_eq!(packet, HAND_COMPUTED_PACKET);
    }

    /// `decode_packet` against the *same* hand-written byte array, checked independently of
    /// the encoder above -- proves decode implements the identical header layout in reverse,
    /// not merely that encode/decode agree with each other (a self-consistent but
    /// standard-violating layout would still round-trip; it would not reproduce this literal
    /// byte array or these exact field values).
    #[test]
    fn hand_computed_decode_matches_ccsds_bit_layout_pinned_by_hand() {
        let c = hand_computed_codec();
        let mut map = ApidMap::new();
        map.insert(291, c);
        let decoded = decode_packet(&map, &HAND_COMPUTED_PACKET).expect("decodes");
        assert_eq!(decoded.apid, 291);
        assert!(!decoded.is_command);
        assert_eq!(decoded.sequence_count, 42);
        assert_eq!(decoded.fields.get("word"), Some(&FieldValue::Numeric(0x01020304u32 as f64)));
    }

    /// Big-endian byte order, asserted against the hand-written expected byte string (not a
    /// round trip): `0x01 0x02 0x03 0x04` decodes to `0x01020304`, not the little-endian
    /// `0x04030201`. Fails against an implementation that reads/writes fields little-endian.
    #[test]
    fn field_byte_order_is_big_endian_against_a_hand_written_byte_string() {
        let user_data: [u8; 4] = [0x01, 0x02, 0x03, 0x04];
        let f = field("word", 0, 32, pb::PacketFieldType::Uint, 1.0, 0.0);
        let raw = read_bitfield_u64(&user_data, f.bit_offset, f.bit_width);
        assert_eq!(raw, 0x01020304, "big-endian: the first byte on the wire is the most significant");
        assert_ne!(raw, 0x04030201, "a little-endian implementation would read this value instead");
    }

    /// The packet data length field is `user_data_bytes - 1`, asserted against a hand-computed
    /// byte sequence (byte5 = 0x03 for user_data_bytes = 4), not against the encoder's own
    /// output. Fails against the classic off-by-one: an implementation that writes
    /// `user_data_bytes` (byte5 = 0x04) directly, forgetting the `- 1`.
    #[test]
    fn packet_data_length_field_is_the_packet_data_field_minus_one_pinned_by_hand() {
        assert_eq!(HAND_COMPUTED_PACKET[4], 0x00);
        assert_eq!(HAND_COMPUTED_PACKET[5], 0x03, "user_data_bytes=4, so the length count must be 4-1=3, not 4");
    }

    /// A packet whose length field was written the "forgot the -1" way (0x04 instead of 0x03)
    /// is refused as a typed `PacketDataLengthMismatch`, not silently accepted as a
    /// differently-shaped packet -- this is the off-by-one's failure mode from the *decode*
    /// side: a decoder that reads the length field as `user_data_bytes` directly (rather than
    /// `length_field + 1`) would accept this malformed packet instead of refusing it.
    #[test]
    fn a_packet_with_the_off_by_one_forgotten_is_a_typed_length_mismatch() {
        let mut bad = HAND_COMPUTED_PACKET;
        bad[5] = 0x04; // the "forgot to subtract 1" mistake
        let c = hand_computed_codec();
        let mut map = ApidMap::new();
        map.insert(291, c);
        let err = decode_packet(&map, &bad).unwrap_err();
        assert!(matches!(err, CodecError::PacketDataLengthMismatch { expected_user_data_bytes: 4, actual_user_data_bytes: 5, .. }), "{err:?}");
    }

    // -----------------------------------------------------------------------------------
    // Round trip, every PacketFieldType, default scale/offset (exact -- no float rounding:
    // scale=1.0/offset=0.0 make `raw * scale + offset` and its inverse identity operations in
    // IEEE-754 for every value used here).
    // -----------------------------------------------------------------------------------

    fn all_types_codec() -> pb::PacketCodec {
        codec(
            "all_types",
            100,
            false,
            0,
            // uint32(4) + int16(2) + float32(4) + float64(8) + bytes(2) = 20 bytes, byte-aligned
            20,
            vec![
                field("u", 0, 32, pb::PacketFieldType::Uint, 1.0, 0.0),
                field("i", 32, 16, pb::PacketFieldType::Int, 1.0, 0.0),
                field("f32", 48, 32, pb::PacketFieldType::Float32, 1.0, 0.0),
                field("f64", 80, 64, pb::PacketFieldType::Float64, 1.0, 0.0),
                field("b", 144, 16, pb::PacketFieldType::Bytes, 1.0, 0.0),
            ],
        )
    }

    /// Fails against any field-type branch (of the five) whose encode/decode pair is not a
    /// true inverse -- e.g. a sign-extension bug on `INT` (wrong for a negative value), a
    /// `FLOAT32`/`FLOAT64` bit-width mixup, or a `BYTES` copy at the wrong offset.
    #[test]
    fn round_trips_every_packet_field_type_with_default_scale_offset() {
        let c = all_types_codec();
        let mut values = BTreeMap::new();
        values.insert("u".to_string(), FieldValue::Numeric(4_000_000_000.0));
        values.insert("i".to_string(), FieldValue::Numeric(-12345.0));
        values.insert("f32".to_string(), FieldValue::Numeric(3.5)); // exact in binary32
        values.insert("f64".to_string(), FieldValue::Numeric(987654.321));
        values.insert("b".to_string(), FieldValue::Bytes(vec![0xDE, 0xAD]));

        let packet = encode_packet(&c, 0, &[], &values).expect("encodes");
        let mut map = ApidMap::new();
        map.insert(100, c);
        let decoded = decode_packet(&map, &packet).expect("decodes");

        assert_eq!(decoded.fields.get("u"), Some(&FieldValue::Numeric(4_000_000_000.0)));
        assert_eq!(decoded.fields.get("i"), Some(&FieldValue::Numeric(-12345.0)));
        assert_eq!(decoded.fields.get("f32"), Some(&FieldValue::Numeric(3.5)));
        assert_eq!(decoded.fields.get("f64"), Some(&FieldValue::Numeric(987654.321)));
        assert_eq!(decoded.fields.get("b"), Some(&FieldValue::Bytes(vec![0xDE, 0xAD])));
    }

    /// Round trip with a nontrivial `scale`/`offset` conversion (question 149's "engineering
    /// value" contract). `scale`/`offset` are both exact powers of two so the forward and
    /// inverse arithmetic is exact in `f64` -- this test asserts exact equality, deliberately,
    /// rather than introducing any float tolerance (none is disclosed anywhere in this module,
    /// and this test does not need one). Fails against an implementation that applies `scale`
    /// only on encode (or only on decode), or that does not default `scale` away from `0.0`.
    #[test]
    fn round_trips_a_scale_and_offset_conversion_exactly() {
        let c = codec("scaled", 200, false, 0, 4, vec![field("raw_counts", 0, 32, pb::PacketFieldType::Uint, 0.5, 10.0)]);
        let mut values = BTreeMap::new();
        // engineering = raw * 0.5 + 10.0; raw = 200 -> engineering = 110.0 exactly.
        values.insert("raw_counts".to_string(), FieldValue::Numeric(110.0));
        let packet = encode_packet(&c, 0, &[], &values).expect("encodes");
        // The raw wire value must be 200 (0x000000C8), confirming the inverse conversion ran.
        assert_eq!(&packet[6..10], &[0x00, 0x00, 0x00, 0xC8]);
        let mut map = ApidMap::new();
        map.insert(200, c);
        let decoded = decode_packet(&map, &packet).expect("decodes");
        assert_eq!(decoded.fields.get("raw_counts"), Some(&FieldValue::Numeric(110.0)));
    }

    /// A `scale` of exactly `0.0` (the field's own zero value) defaults to `1.0`, per
    /// `PacketField.scale`'s own proto doc comment -- fails against an implementation that
    /// divides by the declared `0.0` (a NaN/inf poisoned value) instead of defaulting it.
    #[test]
    fn a_declared_scale_of_zero_defaults_to_one() {
        let c = codec("unscaled", 201, false, 0, 4, vec![field("v", 0, 32, pb::PacketFieldType::Uint, 0.0, 0.0)]);
        let mut values = BTreeMap::new();
        values.insert("v".to_string(), FieldValue::Numeric(42.0));
        let packet = encode_packet(&c, 0, &[], &values).expect("encodes");
        let mut map = ApidMap::new();
        map.insert(201, c);
        let decoded = decode_packet(&map, &packet).expect("decodes");
        assert_eq!(decoded.fields.get("v"), Some(&FieldValue::Numeric(42.0)));
    }

    // -----------------------------------------------------------------------------------
    // Typed faults: unknown APID, malformed packet
    // -----------------------------------------------------------------------------------

    /// Fails against a decoder that silently drops (returns `None`/ignores) a packet whose
    /// APID it does not recognize, instead of a typed refusal (question 149).
    #[test]
    fn an_unknown_apid_is_a_typed_error() {
        let map = ApidMap::new();
        let err = decode_packet(&map, &HAND_COMPUTED_PACKET).unwrap_err();
        assert!(matches!(err, CodecError::UnknownApid { apid: 291 }), "{err:?}");
    }

    /// A packet whose total byte length disagrees with `secondary_header_bytes +
    /// user_data_bytes` (even though its own internal length field is self-consistent) is a
    /// typed error, not a panic on out-of-bounds slicing or a silently truncated decode.
    #[test]
    fn a_packet_with_the_wrong_total_length_is_a_typed_error() {
        let c = hand_computed_codec();
        let mut map = ApidMap::new();
        map.insert(291, c);
        let mut truncated = HAND_COMPUTED_PACKET.to_vec();
        truncated.truncate(9); // one byte short of the declared 10
        let err = decode_packet(&map, &truncated).unwrap_err();
        assert!(matches!(err, CodecError::PacketLengthMismatch { expected_total_bytes: 10, actual_total_bytes: 9, .. }), "{err:?}");
    }

    #[test]
    fn a_packet_shorter_than_the_primary_header_is_a_typed_error() {
        let map = ApidMap::new();
        let err = decode_packet(&map, &[0x01, 0x23]).unwrap_err();
        assert!(matches!(err, CodecError::PacketTooShortForHeader { actual: 2 }), "{err:?}");
    }

    #[test]
    fn is_command_selects_the_telecommand_type_bit() {
        let c = codec("tc", 5, true, 0, 1, vec![field("cmd", 0, 8, pb::PacketFieldType::Uint, 1.0, 0.0)]);
        let mut values = BTreeMap::new();
        values.insert("cmd".to_string(), FieldValue::Numeric(1.0));
        let packet = encode_packet(&c, 0, &[], &values).expect("encodes");
        assert_eq!(packet[0] & 0b0001_0000, 0b0001_0000, "type bit (bit 4 of byte 0) must be set for a telecommand");
        let mut map = ApidMap::new();
        map.insert(5, c);
        let decoded = decode_packet(&map, &packet).expect("decodes");
        assert!(decoded.is_command);
    }

    /// The case that distinguishes CCSDS 133.0-B 4.1.2.5 from the `user_data_bytes - 1` rule an
    /// earlier M22.3 draft used: with a secondary header present the two disagree, and only the
    /// standard's rule (Packet Data Field = secondary header + user data, minus one) is right.
    /// Hand-derived, not round-tripped: apid=6, telecommand, 2-byte secondary header, 1 user
    /// data byte, so the Packet Data Field is 3 octets and the length field is 3 - 1 = 2.
    /// Fails against `user_data_bytes - 1` (which would encode byte5 as 0x00, claiming a 1-octet
    /// Packet Data Field for a packet that actually carries 3) -- the exact defect this rule
    /// correction fixed, and the reason a round-trip test alone was not enough to catch it.
    #[test]
    fn packet_data_length_counts_the_secondary_header_pinned_by_hand() {
        let c = codec("pdl_sec_hdr", 6, true, 2, 1, vec![field("v", 0, 8, pb::PacketFieldType::Uint, 1.0, 0.0)]);
        let mut values = BTreeMap::new();
        values.insert("v".to_string(), FieldValue::Numeric(255.0));
        let packet = encode_packet(&c, 0, &[0xAA, 0xBB], &values).expect("encodes");
        // Packet Data Field = 2 (secondary header) + 1 (user data) = 3 octets; 3 - 1 = 2.
        assert_eq!(packet[4], 0x00, "packet data length high byte");
        assert_eq!(packet[5], 0x02, "packet data length low byte: (2 + 1) - 1, not (1) - 1");
        // And it decodes back through the same rule.
        let mut map = ApidMap::new();
        map.insert(6, c.clone());
        let decoded = decode_packet(&map, &packet).expect("decodes");
        assert_eq!(decoded.fields.get("v"), Some(&FieldValue::Numeric(255.0)));
    }

    /// Fixed a pre-existing defect found while landing M22.2 (star tracker/IMU sensors): this
    /// test previously had no `#[test]` attribute of its own (a stray duplicated `#[test]` sat
    /// on the *previous* function instead, above its own doc comment -- rustc's
    /// `duplicate_macro_attributes` lint flags exactly this), so it silently never ran. Restored
    /// to an actual, executed test; its body and assertions are unchanged.
    #[test]
    fn secondary_header_bytes_are_carried_verbatim_between_primary_header_and_user_data() {
        let c = codec("with_sec_hdr", 6, true, 2, 1, vec![field("v", 0, 8, pb::PacketFieldType::Uint, 1.0, 0.0)]);
        let mut values = BTreeMap::new();
        values.insert("v".to_string(), FieldValue::Numeric(255.0));
        let packet = encode_packet(&c, 0, &[0xAA, 0xBB], &values).expect("encodes");
        // 6-byte primary header + 2-byte secondary header + 1-byte user data.
        assert_eq!(packet.len(), 9);
        assert_eq!(&packet[6..8], &[0xAA, 0xBB]);
        assert_eq!(packet[8], 0xFF);
        // Hand-derived primary header for apid=6 (11 bits: 00000000110 -> top3=000, bottom8=0x06),
        // is_command=true, sec_hdr_flag=1, sequence_count=0, user_data_bytes=1 -> length=0.
        assert_eq!(&packet[0..6], &[0x18, 0x06, 0xC0, 0x00, 0x00, 0x02]);
        let mut map = ApidMap::new();
        map.insert(6, c);
        let decoded = decode_packet(&map, &packet).expect("decodes");
        assert_eq!(decoded.fields.get("v"), Some(&FieldValue::Numeric(255.0)));
    }

    // -----------------------------------------------------------------------------------
    // Load-time validation (question 149)
    // -----------------------------------------------------------------------------------

    /// Fails against a loader that accepts a field extending past the declared
    /// `user_data_bytes` instead of refusing it at load time.
    #[test]
    fn a_field_extent_past_user_data_bytes_is_a_typed_load_error() {
        let c = codec("bad", 1, false, 0, 2, vec![field("too_long", 0, 32, pb::PacketFieldType::Uint, 1.0, 0.0)]);
        let err = validate_codec(&c).unwrap_err();
        assert!(matches!(err, CodecError::FieldExtentExceedsUserData { ref field, .. } if field == "too_long"), "{err:?}");
    }

    /// Fails against a loader that accepts a `FLOAT32` field declared with a `bit_width` other
    /// than 32 (e.g. reusing a `UINT`-shaped field's width unchanged).
    #[test]
    fn a_fixed_width_type_disagreeing_with_bit_width_is_a_typed_load_error() {
        let c = codec("bad", 1, false, 0, 4, vec![field("f", 0, 16, pb::PacketFieldType::Float32, 1.0, 0.0)]);
        let err = validate_codec(&c).unwrap_err();
        assert!(matches!(err, CodecError::InvalidFieldWidth { ref field, bit_width: 16, .. } if field == "f"), "{err:?}");
    }

    #[test]
    fn a_zero_bit_width_is_a_typed_load_error() {
        let c = codec("bad", 1, false, 0, 4, vec![field("f", 0, 0, pb::PacketFieldType::Uint, 1.0, 0.0)]);
        let err = validate_codec(&c).unwrap_err();
        assert!(matches!(err, CodecError::InvalidFieldWidth { ref field, bit_width: 0, .. } if field == "f"), "{err:?}");
    }

    /// Fails against a validator that never checks `apid` for duplicates across codecs (only
    /// per-field checks), or that silently keeps the last-seen codec for a repeated `apid`
    /// (a `HashMap`/`BTreeMap::insert` overwrite with no error).
    #[test]
    fn duplicate_apids_across_codecs_are_a_typed_load_error() {
        let a = codec("a", 42, false, 0, 1, vec![]);
        let b = codec("b", 42, true, 0, 1, vec![]);
        let err = validate_system_packet_codecs(&[a, b]).unwrap_err();
        assert!(matches!(err, CodecError::DuplicateApid { apid: 42, ref first_codec_id, ref second_codec_id } if first_codec_id == "a" && second_codec_id == "b"), "{err:?}");
    }

    #[test]
    fn distinct_apids_build_an_ordered_map() {
        let a = codec("a", 42, false, 0, 1, vec![]);
        let b = codec("b", 7, true, 0, 1, vec![]);
        let map = validate_system_packet_codecs(&[a, b]).expect("valid");
        // BTreeMap: iteration order is by key, so apid 7 must come before apid 42 regardless
        // of insertion order -- proof this is not a HashMap masquerading as ordered by luck.
        let apids: Vec<u32> = map.keys().copied().collect();
        assert_eq!(apids, vec![7, 42]);
    }

    // -----------------------------------------------------------------------------------
    // Question 173 (M25.3): `measurements_from_field_values`.
    // -----------------------------------------------------------------------------------

    fn targeted_field(name: &str, target: &str) -> pb::PacketField {
        pb::PacketField { name: name.to_string(), bit_offset: 0, bit_width: 64, r#type: pb::PacketFieldType::Float64 as i32, unit: pb::Unit::Unspecified as i32, scale: 1.0, offset: 0.0, target: target.to_string() }
    }

    fn numeric_values(pairs: &[(&str, f64)]) -> BTreeMap<String, FieldValue> {
        pairs.iter().map(|(k, v)| (k.to_string(), FieldValue::Numeric(*v))).collect()
    }

    /// The headline positive case: four fields sharing one measurement id (the part of `target`
    /// before its last `/`) become one `Measurement`, `z` in the codec's own declared field
    /// order (not alphabetical -- `qw` sorts before `qx` by name, but must not come first here).
    /// **Fails against an implementation that orders `z` by field *name*** (e.g. iterating a
    /// `BTreeMap<String, FieldValue>` keyed by name instead of `codec.fields`): that would
    /// produce `[qw, qx, qy, qz]` (alphabetical), not `[qx, qy, qz, qw]` (declared/physical).
    #[test]
    fn fields_sharing_a_target_prefix_become_one_measurement_in_declared_field_order() {
        let c = codec(
            "st",
            100,
            false,
            0,
            32,
            vec![targeted_field("qx", "altavista.attitude_q4/qx"), targeted_field("qy", "altavista.attitude_q4/qy"), targeted_field("qz", "altavista.attitude_q4/qz"), targeted_field("qw", "altavista.attitude_q4/qw")],
        );
        let values = numeric_values(&[("qx", 0.1), ("qy", 0.2), ("qz", 0.3), ("qw", 0.9)]);
        let out = measurements_from_field_values(&c, &values, 42_000, "startracker_1", "", &BTreeMap::new()).expect("valid");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].measurement_id, "altavista.attitude_q4");
        assert_eq!(out[0].z, vec![0.1, 0.2, 0.3, 0.9], "z must be in codec.fields' own declared order, not alphabetical by field name");
        assert_eq!(out[0].epoch_ns, 42_000);
        assert_eq!(out[0].sensor_id, "startracker_1");
        assert!(out[0].frame_id.is_empty());
        assert!(out[0].r.is_empty(), "no noise was declared for this measurement id -- r must stay empty, never invented");
    }

    /// The required negative case (question 173's own "the one that matters most"): a codec
    /// whose fields all declare an empty `target` produces **no** `Measurement` at all -- not a
    /// zero-filled or guessed one. **Fails against an implementation that invents a measurement
    /// id from the codec's own `id`/`apid` when no field declares a `target`** (or that treats
    /// an empty-string target as a valid, if boring, measurement id).
    #[test]
    fn a_codec_with_no_declared_target_produces_no_measurement() {
        let c = codec("st", 100, false, 0, 32, vec![field("qx", 0, 64, pb::PacketFieldType::Float64, 1.0, 0.0), field("qy", 64, 64, pb::PacketFieldType::Float64, 1.0, 0.0)]);
        let values = numeric_values(&[("qx", 0.1), ("qy", 0.2)]);
        let out = measurements_from_field_values(&c, &values, 1, "s", "", &BTreeMap::new()).expect("valid");
        assert!(out.is_empty(), "a codec that maps nothing must produce nothing: {out:?}");
    }

    /// Commands never become measurements, even if (hypothetically) a field declared a target --
    /// `is_command` alone gates this, before any field is even inspected. Fails against an
    /// implementation that only checks `target`, not `is_command`.
    #[test]
    fn a_command_codec_produces_no_measurement_even_with_a_declared_target() {
        let c = codec("cmd", 200, true, 0, 8, vec![targeted_field("value", "Cd")]);
        let values = numeric_values(&[("value", 220.0)]);
        let out = measurements_from_field_values(&c, &values, 1, "s", "", &BTreeMap::new()).expect("valid");
        assert!(out.is_empty(), "{out:?}");
    }

    /// `r` is populated only for a measurement id the caller declared noise for, and is checked
    /// SPD (question 173: "if r is populated it must be SPD"). Fails against an implementation
    /// that skips the SPD check (would silently ship a caller's malformed covariance).
    #[test]
    fn r_is_populated_only_for_a_declared_measurement_id_and_is_checked_spd() {
        let c = codec("imu", 101, false, 0, 24, vec![targeted_field("wx", "imu.gyro3/wx"), targeted_field("wy", "imu.gyro3/wy"), targeted_field("wz", "imu.gyro3/wz")]);
        let values = numeric_values(&[("wx", 1.0), ("wy", 2.0), ("wz", 3.0)]);
        let sigma2 = 1e-4;
        let r = vec![sigma2, 0.0, 0.0, 0.0, sigma2, 0.0, 0.0, 0.0, sigma2];
        let mut noise = BTreeMap::new();
        noise.insert("imu.gyro3".to_string(), r.clone());
        let out = measurements_from_field_values(&c, &values, 1, "imu_1", "", &noise).expect("valid");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].r, r);
    }

    /// **Broken-and-restored teeth for the SPD check itself**: a declared "noise" matrix that is
    /// not symmetric positive definite (here: a negative diagonal entry) must be a typed refusal,
    /// never shipped as-is. Fails against an implementation that assigns `r` unconditionally
    /// without calling `check_spd_row_major` at all.
    #[test]
    fn a_non_spd_declared_noise_is_a_typed_refusal() {
        let c = codec("imu", 101, false, 0, 8, vec![targeted_field("wx", "imu.gyro3/wx")]);
        let values = numeric_values(&[("wx", 1.0)]);
        let mut noise = BTreeMap::new();
        noise.insert("imu.gyro3".to_string(), vec![-1.0]);
        let err = measurements_from_field_values(&c, &values, 1, "imu_1", "", &noise).unwrap_err();
        assert!(matches!(err, CodecError::MeasurementNoiseNotSpd { ref measurement_id, .. } if measurement_id == "imu.gyro3"), "{err:?}");
    }

    /// Defect D1 (question 149's no-silent-drop rule): a field with a non-empty `target` whose
    /// value is entirely absent from `values` is a typed error, never a quietly-shrunk `z`.
    /// **Fails against the pre-fix implementation** (`let Some(FieldValue::Numeric(v)) = ... else
    /// { continue; }`), which would silently skip `wy` and return `Ok` with a 2-component `z`
    /// instead of refusing -- confirmed by reverting this one line locally and re-running: the old
    /// code returns `Ok([1.0, 3.0])`, not an error (break-and-restore evidence in the task report).
    #[test]
    fn a_targeted_field_with_no_value_at_all_is_a_typed_error_not_a_silent_drop() {
        let c = codec("imu", 101, false, 0, 24, vec![targeted_field("wx", "imu.gyro3/wx"), targeted_field("wy", "imu.gyro3/wy"), targeted_field("wz", "imu.gyro3/wz")]);
        // `wy` is deliberately missing from `values` -- e.g. a caller bug that built the map from
        // a stale field list.
        let values = numeric_values(&[("wx", 1.0), ("wz", 3.0)]);
        let err = measurements_from_field_values(&c, &values, 1, "imu_1", "", &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, CodecError::MeasurementFieldValueMissing { ref codec_id, ref field } if codec_id == "imu" && field == "wy"), "{err:?}");
    }

    /// Defect D1's other silent-drop path: a field with a non-empty `target` whose decoded value
    /// is present but is [`FieldValue::Bytes`] (not `Numeric`) is a typed error, never silently
    /// skipped. **Fails against the pre-fix implementation**, which would silently skip `wy` (the
    /// `let Some(FieldValue::Numeric(v)) = values.get(...) else { continue }` pattern matches
    /// `None` on a `Bytes` value too) and return `Ok([1.0, 3.0])` instead of refusing.
    #[test]
    fn a_targeted_field_with_a_bytes_value_is_a_typed_error_not_a_silent_drop() {
        let c = codec("imu", 101, false, 0, 24, vec![targeted_field("wx", "imu.gyro3/wx"), targeted_field("wy", "imu.gyro3/wy"), targeted_field("wz", "imu.gyro3/wz")]);
        let mut values = numeric_values(&[("wx", 1.0), ("wz", 3.0)]);
        values.insert("wy".to_string(), FieldValue::Bytes(vec![0, 1, 2]));
        let err = measurements_from_field_values(&c, &values, 1, "imu_1", "", &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, CodecError::MeasurementFieldValueNotNumeric { ref codec_id, ref field } if codec_id == "imu" && field == "wy"), "{err:?}");
    }

    /// Regression: an empty-`target` field is still skipped silently (never an error) even when it
    /// sits alongside targeted fields whose own values are present and fine -- the empty-`target`
    /// "recorded but not mapped" case (already covered alone by
    /// [`a_codec_with_no_declared_target_produces_no_measurement`]) must not be caught by the two
    /// new typed errors above just because it shares a codec with targeted fields. The empty-target
    /// field's own value is a `Bytes` value here specifically to prove the type-mismatch check
    /// above is gated on a non-empty `target`, not merely on `FieldValue::Bytes` appearing anywhere
    /// in `values`.
    #[test]
    fn an_empty_target_field_is_still_skipped_silently_alongside_targeted_fields() {
        let c = codec(
            "imu",
            101,
            false,
            0,
            32,
            vec![targeted_field("wx", "imu.gyro3/wx"), field("raw_status", 64, 64, pb::PacketFieldType::Bytes, 1.0, 0.0), targeted_field("wz", "imu.gyro3/wz")],
        );
        let mut values = numeric_values(&[("wx", 1.0), ("wz", 3.0)]);
        values.insert("raw_status".to_string(), FieldValue::Bytes(vec![0xAB]));
        let out = measurements_from_field_values(&c, &values, 1, "imu_1", "", &BTreeMap::new()).expect("empty-target field must not error");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].z, vec![1.0, 3.0], "raw_status (empty target) contributes nothing to z");
    }
}

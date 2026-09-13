//! A minimal, from-scratch re-implementation of exactly the subset of
//! `av_kernel::codec`'s CCSDS Space Packet decode algorithm
//! (`crates/av-kernel/src/codec.rs` -- `decode_packet`/`read_field`/`read_bitfield_u64`)
//! this plugin needs: reading the primary header and every declared **numeric**
//! `PacketField` (UINT/INT/FLOAT32/FLOAT64) out of one packet's user data, at the
//! engineering level (`scale`/`offset` already applied, per `packet.proto`'s own
//! `PacketField.scale`/`.offset` doc comment).
//!
//! # Why this is duplicated, not shared, with `av-kernel`
//!
//! `av-edge` (this crate) is a pure, network-free library the whole edge track depends on
//! (`crate`'s own module doc). `av-kernel` cannot become one of its dependencies:
//! `av-kernel` depends on `gmat-sys` (a native GMAT FFI build, `Cargo.toml`'s own
//! `links = "gmatffi"`) directly, and, transitively through
//! `av-lockstep -> av-grpc -> tonic`, on a full gRPC transport stack -- exactly the kind
//! of dependency `docs/edge-plan.md` milestone E4's own task brief says this crate must
//! never gain, even indirectly (`crate::plugin`'s own module doc, "no transport
//! dependency"). So this module re-implements the handful of pure bit-arithmetic
//! functions this plugin actually needs to decode one telemetry packet, rather than
//! pulling in a crate that would drag GMAT and gRPC into every consumer of this library.
//!
//! This is **not** a second, independently-evolving copy of the CCSDS bit layout that
//! could silently drift from `av-kernel`'s own: `crates/av-edge/tests/plugin_replay.rs`
//! (a test binary, which -- unlike this library -- is never shipped and never becomes
//! part of any other crate's own dependency tree, so it may freely dev-depend on
//! `av-kernel`) decodes the same committed fixture packets through both
//! [`decode_numeric_fields`] and `av_kernel::codec::decode_packet` and asserts the two
//! agree, field for field, byte for byte.
//!
//! # What this module deliberately does not support
//!
//! - `PacketFieldType::BYTES` fields: refused as [`PacketError::UnsupportedFieldType`].
//!   No codec this plugin decodes (`drms/demo_ground_segment_flight.system.yaml`'s `x`/
//!   `y`/`z`, all FLOAT64) declares one, and a byte-valued field could not become a CDM
//!   `Measurement.z` component (`f64`) anyway.
//! - Commands (`PacketCodec.is_command == true`): refused as [`PacketError::IsCommand`]
//!   -- this plugin only ever replays telemetry (`edge.proto`'s own `MeasurementSchema`
//!   doc comment; `av_kernel::codec::measurements_from_field_values`'s identical check).
//! - Encoding: this module only ever reads a packet a run already produced; it has no
//!   `encode_packet` counterpart.
//! - An `ApidMap`/multi-codec dispatch: [`decode_numeric_fields`] takes exactly one
//!   already-selected `PacketCodec` (`crate::plugin::PluginConfig` names one port and one
//!   codec per source), and checks the packet's own header APID matches it rather than
//!   looking a codec up by APID.

use std::collections::BTreeMap;

use av_cdm::pb;

/// The CCSDS primary header is always exactly 6 octets (CCSDS 133.0-B-2 section 4.1.3) --
/// identical constant to `av_kernel::codec::PRIMARY_HEADER_LEN`.
pub const PRIMARY_HEADER_LEN: usize = 6;

/// The 11-bit APID field's maximum value (`2^11 - 1`).
const MAX_APID: u32 = 0x7FF;

/// Everything that can go wrong decoding one packet against a declared [`pb::PacketCodec`],
/// typed rather than silently dropped or panicking -- mirroring `av_kernel::codec::
/// CodecError`'s own no-silent-drop rule (`docs/open-questions.md` question 149) for the
/// numeric-only subset this module implements.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PacketError {
    #[error("packet is {actual} byte(s), shorter than the {PRIMARY_HEADER_LEN}-byte CCSDS primary header")]
    TooShortForHeader { actual: usize },
    #[error("codec {codec_id:?} declares apid {apid}, which does not fit the CCSDS 11-bit APID field (0..={MAX_APID})")]
    ApidOutOfRange { codec_id: String, apid: u32 },
    #[error("codec {codec_id:?} declares apid {declared}, but the packet's own primary header carries apid {actual}")]
    ApidMismatch { codec_id: String, declared: u32, actual: u32 },
    #[error("codec {codec_id:?} is a command codec (is_command=true); this decoder only ever replays telemetry")]
    IsCommand { codec_id: String },
    #[error("packet's own declared packet-data length disagrees with codec {codec_id:?}: expected {expected} user-data byte(s), computed {actual}")]
    LengthMismatch { codec_id: String, expected: u32, actual: u32 },
    #[error("packet is {actual} byte(s) total; codec {codec_id:?} expects exactly {expected}")]
    TotalLengthMismatch { codec_id: String, expected: usize, actual: usize },
    #[error("codec {codec_id:?} field {field:?} (bit_offset={bit_offset}, bit_width={bit_width}) extends past its {user_data_bytes}-byte user data field")]
    FieldExtentExceedsUserData { codec_id: String, field: String, bit_offset: u32, bit_width: u32, user_data_bytes: u32 },
    #[error("codec {codec_id:?} field {field:?} has a PacketFieldType this numeric-only decoder does not support (BYTES, or PACKET_FIELD_TYPE_UNSPECIFIED)")]
    UnsupportedFieldType { codec_id: String, field: String },
}

/// Read `bit_width` (1..=64) bits starting at `bit_offset`, MSB-first, from `data`, returned
/// right-aligned in a `u64` -- byte-for-byte the same algorithm as
/// `av_kernel::codec::read_bitfield_u64` (CCSDS's own big-endian, MSB-first bit numbering).
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

/// Two's-complement sign-extend a `bit_width`-bit raw value (already right-aligned in `raw`
/// by [`read_bitfield_u64`]) to a full `i64` -- identical to `av_kernel::codec::sign_extend`.
fn sign_extend(raw: u64, bit_width: u32) -> i64 {
    let shift = 64 - bit_width;
    ((raw << shift) as i64) >> shift
}

/// "scale defaults to 1 when 0 is given" -- `PacketField.scale`'s own proto doc comment,
/// identical to `av_kernel::codec::effective_scale`.
fn effective_scale(scale: f64) -> f64 {
    if scale == 0.0 {
        1.0
    } else {
        scale
    }
}

fn read_numeric_field(user_data: &[u8], field: &pb::PacketField, codec_id: &str) -> Result<f64, PacketError> {
    let ty = pb::PacketFieldType::try_from(field.r#type).unwrap_or(pb::PacketFieldType::Unspecified);
    let scale = effective_scale(field.scale);
    match ty {
        pb::PacketFieldType::Uint => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, field.bit_width);
            Ok((raw as f64) * scale + field.offset)
        }
        pb::PacketFieldType::Int => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, field.bit_width);
            Ok((sign_extend(raw, field.bit_width) as f64) * scale + field.offset)
        }
        pb::PacketFieldType::Float32 => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, 32) as u32;
            Ok((f32::from_bits(raw) as f64) * scale + field.offset)
        }
        pb::PacketFieldType::Float64 => {
            let raw = read_bitfield_u64(user_data, field.bit_offset, 64);
            Ok(f64::from_bits(raw) * scale + field.offset)
        }
        pb::PacketFieldType::Bytes | pb::PacketFieldType::Unspecified => Err(PacketError::UnsupportedFieldType { codec_id: codec_id.to_string(), field: field.name.clone() }),
    }
}

/// Decode `data` (one CCSDS space packet payload -- exactly what a `PortTrafficRecord.
/// payload` carries for a FRAMED port whose `Port.schema` is `"ccsds.spp"`) against
/// `codec`'s declared numeric fields, returning every field's engineering value keyed by
/// [`pb::PacketField::name`] (a `BTreeMap`, matching `av_kernel::codec::DecodedPacket.
/// fields`'s own determinism convention). See this module's own doc comment for exactly
/// which shapes are refused rather than decoded.
pub fn decode_numeric_fields(codec: &pb::PacketCodec, data: &[u8]) -> Result<BTreeMap<String, f64>, PacketError> {
    if codec.is_command {
        return Err(PacketError::IsCommand { codec_id: codec.id.clone() });
    }
    if codec.apid > MAX_APID {
        return Err(PacketError::ApidOutOfRange { codec_id: codec.id.clone(), apid: codec.apid });
    }
    if data.len() < PRIMARY_HEADER_LEN {
        return Err(PacketError::TooShortForHeader { actual: data.len() });
    }
    let (byte0, byte1) = (data[0], data[1]);
    let length_field = ((data[4] as u32) << 8) | (data[5] as u32);
    let apid = (((byte0 & 0x07) as u32) << 8) | (byte1 as u32);
    if apid != codec.apid {
        return Err(PacketError::ApidMismatch { codec_id: codec.id.clone(), declared: codec.apid, actual: apid });
    }

    // Inverse of av_kernel::codec::encode_packet: the length field counts the whole Packet
    // Data Field (secondary header + user data) minus one, per CCSDS 133.0-B 4.1.2.5.
    let secondary_header_bytes = codec.secondary_header_bytes as usize;
    let user_data_bytes = codec.user_data_bytes as usize;
    let declared_packet_data_field_bytes = (length_field as usize) + 1;
    let expected_packet_data_field_bytes = secondary_header_bytes + user_data_bytes;
    if declared_packet_data_field_bytes != expected_packet_data_field_bytes {
        let actual_user_data_bytes = declared_packet_data_field_bytes.saturating_sub(secondary_header_bytes) as u32;
        return Err(PacketError::LengthMismatch { codec_id: codec.id.clone(), expected: codec.user_data_bytes, actual: actual_user_data_bytes });
    }
    let expected_total = PRIMARY_HEADER_LEN + secondary_header_bytes + user_data_bytes;
    if data.len() != expected_total {
        return Err(PacketError::TotalLengthMismatch { codec_id: codec.id.clone(), expected: expected_total, actual: data.len() });
    }

    let user_data = &data[PRIMARY_HEADER_LEN + secondary_header_bytes..expected_total];
    let mut out = BTreeMap::new();
    for field in &codec.fields {
        let bit_end = (field.bit_offset as u64) + (field.bit_width as u64);
        if bit_end > (codec.user_data_bytes as u64) * 8 {
            return Err(PacketError::FieldExtentExceedsUserData {
                codec_id: codec.id.clone(),
                field: field.name.clone(),
                bit_offset: field.bit_offset,
                bit_width: field.bit_width,
                user_data_bytes: codec.user_data_bytes,
            });
        }
        let value = read_numeric_field(user_data, field, &codec.id)?;
        out.insert(field.name.clone(), value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, bit_offset: u32, bit_width: u32, ty: pb::PacketFieldType) -> pb::PacketField {
        pb::PacketField { name: name.to_string(), bit_offset, bit_width, r#type: ty as i32, unit: pb::Unit::Unspecified as i32, scale: 1.0, offset: 0.0, target: String::new() }
    }

    fn float64_codec() -> pb::PacketCodec {
        pb::PacketCodec {
            id: "test_float64_codec".to_string(),
            apid: 500,
            is_command: false,
            secondary_header_bytes: 0,
            user_data_bytes: 24,
            description: String::new(),
            fields: vec![field("x", 0, 64, pb::PacketFieldType::Float64), field("y", 64, 64, pb::PacketFieldType::Float64), field("z", 128, 64, pb::PacketFieldType::Float64)],
        }
    }

    /// Hand-encode a packet the same way `av_kernel::codec::encode_packet` would (per that
    /// module's own hand-computed test vector, `hand_computed_encode_matches_ccsds_bit_
    /// layout_pinned_by_hand`): a real header plus the three FLOAT64 values' big-endian bit
    /// patterns, so this test does not depend on this module's own encoder existing (there
    /// isn't one) or `av_kernel::codec::encode_packet` at all.
    fn encode_float64_packet(apid: u32, sequence_count: u16, x: f64, y: f64, z: f64) -> Vec<u8> {
        let byte0 = (((apid >> 8) & 0x07) as u8) & 0x07;
        let byte1 = (apid & 0xFF) as u8;
        let byte2 = (0b11u8 << 6) | (((sequence_count >> 8) & 0x3F) as u8);
        let byte3 = (sequence_count & 0xFF) as u8;
        let user_data_bytes = 24u32;
        let packet_data_length = user_data_bytes - 1;
        let byte4 = ((packet_data_length >> 8) & 0xFF) as u8;
        let byte5 = (packet_data_length & 0xFF) as u8;
        let mut packet = vec![byte0, byte1, byte2, byte3, byte4, byte5];
        packet.extend_from_slice(&x.to_bits().to_be_bytes());
        packet.extend_from_slice(&y.to_bits().to_be_bytes());
        packet.extend_from_slice(&z.to_bits().to_be_bytes());
        packet
    }

    #[test]
    fn decodes_three_float64_fields_at_engineering_scale() {
        let codec = float64_codec();
        let packet = encode_float64_packet(500, 7, -2_169_088.33, -6_490_320.48, 3_263_896.20);
        let fields = decode_numeric_fields(&codec, &packet).expect("decodes");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields["x"], -2_169_088.33);
        assert_eq!(fields["y"], -6_490_320.48);
        assert_eq!(fields["z"], 3_263_896.20);
    }

    #[test]
    fn refuses_a_packet_too_short_for_the_primary_header() {
        let codec = float64_codec();
        let err = decode_numeric_fields(&codec, &[0u8; 3]).unwrap_err();
        assert!(matches!(err, PacketError::TooShortForHeader { actual: 3 }), "{err:?}");
    }

    #[test]
    fn refuses_an_apid_mismatch_between_the_header_and_the_codec() {
        let codec = float64_codec();
        let packet = encode_float64_packet(501, 0, 0.0, 0.0, 0.0); // wrong apid in the header
        let err = decode_numeric_fields(&codec, &packet).unwrap_err();
        assert!(matches!(err, PacketError::ApidMismatch { declared: 500, actual: 501, .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_command_codec() {
        let mut codec = float64_codec();
        codec.is_command = true;
        let packet = encode_float64_packet(500, 0, 0.0, 0.0, 0.0);
        let err = decode_numeric_fields(&codec, &packet).unwrap_err();
        assert!(matches!(err, PacketError::IsCommand { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_bytes_field_type_as_unsupported() {
        let mut codec = float64_codec();
        codec.fields.push(field("raw", 128, 64, pb::PacketFieldType::Bytes));
        codec.user_data_bytes = 32;
        let mut packet = encode_float64_packet(500, 0, 0.0, 0.0, 0.0);
        packet[4] = 0;
        packet[5] = 31; // user_data_bytes now 32, packet_data_length = 31
        packet.extend_from_slice(&[0u8; 8]);
        let err = decode_numeric_fields(&codec, &packet).unwrap_err();
        assert!(matches!(err, PacketError::UnsupportedFieldType { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_field_extending_past_the_user_data_field() {
        let mut codec = float64_codec();
        codec.fields[2].bit_offset = 200; // 200 + 64 > 24*8 = 192
        let packet = encode_float64_packet(500, 0, 0.0, 0.0, 0.0);
        let err = decode_numeric_fields(&codec, &packet).unwrap_err();
        assert!(matches!(err, PacketError::FieldExtentExceedsUserData { .. }), "{err:?}");
    }
}

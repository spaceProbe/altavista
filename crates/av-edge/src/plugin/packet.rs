//! An adapter over `av_codec`'s CCSDS Space Packet decoder, narrowed to exactly the subset
//! this plugin needs: reading the primary header and every declared **numeric**
//! `PacketField` (UINT/INT/FLOAT32/FLOAT64) out of one packet's user data, at the
//! engineering level (`scale`/`offset` already applied, per `packet.proto`'s own
//! `PacketField.scale`/`.offset` doc comment).
//!
//! # Question 205: no more duplicated bit arithmetic
//!
//! Before this task, `av-edge` could not depend on the real CCSDS decoder at all: it lived
//! in `av_kernel::codec`, and `av-kernel` depends on `gmat-sys` (native GMAT FFI) directly
//! and, transitively through `av-lockstep -> av-grpc`, on a full gRPC transport stack --
//! exactly the kind of dependency `docs/edge-plan.md` milestone E4's own task brief says
//! this crate must never gain, even indirectly (`crate::plugin`'s own module doc, "no
//! transport dependency"). So this module used to *reimplement* the primary-header parsing
//! and bit-level field packing from scratch.
//!
//! Question 205 extracted that codec into its own crate, `av-codec` (`crates/av-codec`,
//! moved out of `crates/av-kernel/src/codec.rs`), which depends on neither `gmat-sys` nor
//! any transport crate. `av-edge` now depends on `av-codec` for real (`Cargo.toml`'s
//! `[dependencies]`, not a dev-dependency), so this module no longer needs its own copy of
//! [`av_codec::read_bitfield_u64`]/[`av_codec::decode_packet`]'s bit arithmetic -- it calls
//! them. What remains here is a thin adapter, not a second implementation:
//!
//! - `av_codec::decode_packet` takes a whole `ApidMap` (a *set* of codecs) and returns every
//!   declared field, `BYTES` included, keyed by name; a header APID absent from that map is
//!   `CodecError::UnknownApid`. This plugin instead names exactly one already-selected
//!   `PacketCodec` per source (`crate::plugin::PluginConfig`: one port, one codec) and only
//!   ever replays **telemetry**'s numeric fields (`crate::plugin`'s own module doc) -- so
//!   [`decode_numeric_fields`] keeps the narrower distinctions [`PacketError`] has always
//!   made (`ApidMismatch` against the one expected codec, `IsCommand`, numeric-only) and
//!   builds a one-entry `av_codec::ApidMap` under the hood to call the real decoder.
//! - **One narrow, documented ordering difference from the old from-scratch decoder**: for a
//!   codec whose fields are *both* malformed (a field extending past `user_data_bytes`) and
//!   unsupported (a `BYTES`/`UNSPECIFIED` field) at the same time, `av_codec::decode_packet`
//!   checks each field's *extent* before its *type*, per field, in declared order -- so a
//!   `BYTES` field followed by an extent-invalid field now reports the later field's
//!   `FieldExtentExceedsUserData` rather than the earlier field's `UnsupportedFieldType` (the
//!   old single-pass implementation checked type immediately after extent for each field and
//!   so stopped at the `BYTES` field first). No codec this plugin decodes today
//!   (`drms/demo_ground_segment_flight.system.yaml`'s `x`/`y`/`z`, all FLOAT64, no `BYTES`
//!   field at all) can trigger this, and no test in this crate's own suite depends on the
//!   old ordering -- recorded here rather than silently changed.
//!
//! `crates/av-kernel/tests/edge_plugin_codec_crosscheck.rs` (moved there from this crate's
//! own `tests/plugin_replay.rs` -- question 205's ruling that `av-edge` may not build
//! `gmat-sys`, so the `av_kernel`-cross-check test itself had to move, not just this
//! module's implementation) still cross-checks [`decode_numeric_fields`] against
//! `av_kernel::codec::decode_packet` (the *same* `av_codec` crate, re-exported) over all 900
//! fixture records, byte for byte -- proving this adapter and the decoder it now calls
//! directly still agree with what `av-kernel` itself sees.
//!
//! # What this module deliberately does not support
//!
//! - `PacketFieldType::BYTES` fields: refused as [`PacketError::UnsupportedFieldType`].
//!   No codec this plugin decodes (`drms/demo_ground_segment_flight.system.yaml`'s `x`/
//!   `y`/`z`, all FLOAT64) declares one, and a byte-valued field could not become a CDM
//!   `Measurement.z` component (`f64`) anyway.
//! - Commands (`PacketCodec.is_command == true`): refused as [`PacketError::IsCommand`]
//!   -- this plugin only ever replays telemetry (`edge.proto`'s own `MeasurementSchema`
//!   doc comment; `av_codec::measurements_from_field_values`'s identical check).
//! - Encoding: this module only ever reads a packet a run already produced; it has no
//!   `encode_packet` counterpart.
//! - An `ApidMap`/multi-codec dispatch: [`decode_numeric_fields`] takes exactly one
//!   already-selected `PacketCodec` (`crate::plugin::PluginConfig` names one port and one
//!   codec per source), and checks the packet's own header APID matches it rather than
//!   looking a codec up by APID.

use std::collections::BTreeMap;

use av_cdm::pb;

/// The CCSDS primary header is always exactly 6 octets (CCSDS 133.0-B-2 section 4.1.3) --
/// the same constant `av_codec::PRIMARY_HEADER_LEN` names, kept as a local alias so this
/// module's error messages (`PacketError::TooShortForHeader`) can interpolate it directly.
pub const PRIMARY_HEADER_LEN: usize = av_codec::PRIMARY_HEADER_LEN;

/// The 11-bit APID field's maximum value (`2^11 - 1`).
const MAX_APID: u32 = 0x7FF;

/// Everything that can go wrong decoding one packet against a declared [`pb::PacketCodec`],
/// typed rather than silently dropped or panicking -- mirroring `av_codec::CodecError`'s own
/// no-silent-drop rule (`docs/open-questions.md` question 149) for the numeric-only,
/// single-codec subset this module implements.
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

/// Translate an `av_codec::CodecError` [`av_codec::decode_packet`] returned into this
/// module's own [`PacketError`] shape. `decode_packet` is called here only after this
/// module's own pre-checks (short header, apid mismatch, command, apid range) already ran,
/// so its `PacketTooShortForHeader`/`UnknownApid` variants are unreachable in practice --
/// `codec_id` is threaded through from the caller rather than read off `CodecError` itself
/// (`UnknownApid`/`PacketTooShortForHeader` carry no `codec_id`, having failed before a codec
/// was even matched).
fn translate(codec_id: &str, err: av_codec::CodecError) -> PacketError {
    match err {
        av_codec::CodecError::PacketDataLengthMismatch { expected_user_data_bytes, actual_user_data_bytes, .. } => {
            PacketError::LengthMismatch { codec_id: codec_id.to_string(), expected: expected_user_data_bytes, actual: actual_user_data_bytes }
        }
        av_codec::CodecError::PacketLengthMismatch { expected_total_bytes, actual_total_bytes, .. } => {
            PacketError::TotalLengthMismatch { codec_id: codec_id.to_string(), expected: expected_total_bytes, actual: actual_total_bytes }
        }
        av_codec::CodecError::FieldExtentExceedsUserData { field, bit_offset, bit_width, user_data_bytes, .. } => {
            PacketError::FieldExtentExceedsUserData { codec_id: codec_id.to_string(), field, bit_offset, bit_width, user_data_bytes }
        }
        av_codec::CodecError::UnspecifiedFieldType { field, .. } => PacketError::UnsupportedFieldType { codec_id: codec_id.to_string(), field },
        other => unreachable!("av_codec::decode_packet returned a variant this single-codec, pre-checked caller never expects: {other:?}"),
    }
}

/// Decode `data` (one CCSDS space packet payload -- exactly what a `PortTrafficRecord.
/// payload` carries for a FRAMED port whose `Port.schema` is `"ccsds.spp"`) against
/// `codec`'s declared numeric fields, returning every field's engineering value keyed by
/// [`pb::PacketField::name`] (a `BTreeMap`, matching `av_codec::DecodedPacket.fields`'s own
/// determinism convention). See this module's own doc comment for exactly which shapes are
/// refused rather than decoded, and for the one narrow ordering difference from the
/// from-scratch decoder this replaced.
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
    let actual_apid = (((byte0 & 0x07) as u32) << 8) | (byte1 as u32);
    if actual_apid != codec.apid {
        return Err(PacketError::ApidMismatch { codec_id: codec.id.clone(), declared: codec.apid, actual: actual_apid });
    }

    // codec.apid == actual_apid, just checked, so this one-entry ApidMap always resolves --
    // av_codec::decode_packet's own UnknownApid is unreachable here (see `translate`'s doc).
    let mut apid_map = av_codec::ApidMap::new();
    apid_map.insert(codec.apid, codec.clone());
    let decoded = av_codec::decode_packet(&apid_map, data).map_err(|e| translate(&codec.id, e))?;

    let mut out = BTreeMap::new();
    for field in &codec.fields {
        match decoded.fields.get(&field.name) {
            Some(av_codec::FieldValue::Numeric(v)) => {
                out.insert(field.name.clone(), *v);
            }
            // `av_codec::decode_packet` decodes BYTES fields rather than refusing them (it
            // serves callers that want them); this plugin's own contract does not, so a
            // BYTES value here -- or, defensively, a field `av_codec` did not populate at
            // all, which should not happen for a field it just finished decoding -- is this
            // module's own UnsupportedFieldType, not a panic or a silent gap in `out`.
            Some(av_codec::FieldValue::Bytes(_)) | None => {
                return Err(PacketError::UnsupportedFieldType { codec_id: codec.id.clone(), field: field.name.clone() });
            }
        }
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

    /// Hand-encode a packet the same way `av_codec::encode_packet` would (per that module's
    /// own hand-computed test vector, `hand_computed_encode_matches_ccsds_bit_layout_pinned_
    /// by_hand`): a real header plus the three FLOAT64 values' big-endian bit patterns, so
    /// this test does not depend on this module's own encoder existing (there isn't one) or
    /// `av_codec::encode_packet` at all.
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

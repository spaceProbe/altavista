//! Produces genuine `av_kernel::codec::encode_packet` output for the M22.4 demo's real
//! `PacketCodec`s (`drms/demo_attitude_control_imu.system.yaml`'s apid-201 IMU codec,
//! `drms/demo_attitude_control_controller.system.yaml`'s apid-300 wheel-torque command codec)
//! so `services/cfs`'s C-side CCSDS implementation can be checked against bytes the kernel's
//! own codec actually produced -- not against the C side's own encoder alone (M23.2's brief).
//!
//! Usage: `cargo run --manifest-path services/cfs/tests/golden_gen/Cargo.toml -- <case>`
//! where `<case>` is `imu` or `wheel_torque`. Prints one JSON object to stdout:
//! `{"apid":.., "sequence_count":.., "values": {...}, "encoded_hex": "..."}`. The Python test
//! (`services/cfs/tests/test_ccsds_golden.py`) also uses this binary to *decode* an
//! independently-produced C-encoded buffer, by passing `--decode <hex>` for the same case, to
//! prove the round trip the other direction too.

use std::collections::BTreeMap;
use std::env;

use av_cdm::pb;
use av_kernel::codec::{decode_packet, encode_packet, validate_system_packet_codecs, FieldValue};

fn imu_codec() -> pb::PacketCodec {
    let mk = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Unspecified as i32,
        scale: 1.0,
        offset: 0.0,
        target: String::new(),
    };
    pb::PacketCodec {
        id: "imu_meas_codec".to_string(),
        apid: 201,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 48,
        description: "IMU rate + specific-force measurement (M22.4)".to_string(),
        fields: vec![mk("wx", 0), mk("wy", 64), mk("wz", 128), mk("ax", 192), mk("ay", 256), mk("az", 320)],
    }
}

fn wheel_torque_codec() -> pb::PacketCodec {
    let mk = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Unspecified as i32,
        scale: 1.0,
        offset: 0.0,
        target: String::new(),
    };
    pb::PacketCodec {
        id: "controller_wheel_torque_out_codec".to_string(),
        apid: 300,
        is_command: true,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "reaction wheel torque command (M22.4)".to_string(),
        fields: vec![mk("tau_1", 0), mk("tau_2", 64), mk("tau_3", 128)],
    }
}

/// Fixed, arbitrary-but-documented field values for each case -- not round numbers, so a bit-
/// packing bug (byte-order, off-by-one bit offset) cannot hide behind a value whose bytes are
/// mostly zero.
fn imu_values() -> BTreeMap<String, FieldValue> {
    let mut v = BTreeMap::new();
    v.insert("wx".to_string(), FieldValue::Numeric(0.001234500000));
    v.insert("wy".to_string(), FieldValue::Numeric(-0.002469000001));
    v.insert("wz".to_string(), FieldValue::Numeric(0.000098765432));
    v.insert("ax".to_string(), FieldValue::Numeric(-9.80665));
    v.insert("ay".to_string(), FieldValue::Numeric(0.031415926535));
    v.insert("az".to_string(), FieldValue::Numeric(0.271828182845));
    v
}

fn wheel_torque_values() -> BTreeMap<String, FieldValue> {
    let mut v = BTreeMap::new();
    v.insert("tau_1".to_string(), FieldValue::Numeric(-0.0044721359));
    v.insert("tau_2".to_string(), FieldValue::Numeric(0.0089442719));
    v.insert("tau_3".to_string(), FieldValue::Numeric(0.0));
    v
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex")).collect()
}

fn values_to_json(values: &BTreeMap<String, FieldValue>) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (k, v) in values {
        let FieldValue::Numeric(n) = v else { panic!("only numeric fields used in this fixture") };
        obj.insert(k.clone(), serde_json::json!(n));
    }
    serde_json::Value::Object(obj)
}

fn imu_packet_bytes() -> Vec<u8> {
    encode_packet(&imu_codec(), 42, &[], &imu_values()).expect("encode_packet")
}

fn wheel_torque_packet_bytes() -> Vec<u8> {
    encode_packet(&wheel_torque_codec(), 7, &[], &wheel_torque_values()).expect("encode_packet")
}

/// Prints the prost `encode_to_vec()` bytes of a `LockstepStepRequest` carrying one `PortMessage`
/// input (the real apid-201 IMU packet bytes as its payload) -- used by
/// `services/cfs/tests/test_lockstep_messages.py` to prove
/// `services/cfs/apps/io_lockstep/fsw/src/lockstep_messages.c`'s hand-written protobuf decoder
/// agrees with the real prost-generated encoding, not merely with its own encoder.
fn print_lockstep_step_request() {
    let msg = pb::LockstepStepRequest {
        sequence: 5,
        until_tai_ns: 123_456_789_000,
        inputs: vec![pb::PortMessage { port: "imu_meas".to_string(), tai_ns: 123_456_789_000, payload: imu_packet_bytes() }],
    };
    println!("{}", serde_json::json!({ "encoded_hex": hex(&prost::Message::encode_to_vec(&msg)) }));
}

fn print_lockstep_bind_request() {
    let msg = pb::LockstepBindRequest {
        run_id: "run-cfs-1".to_string(),
        instance: "adcs_flight".to_string(),
        ports: vec![],
        start_tai_ns: 100_000_000_000,
        base_period_ns: 100_000_000,
        step_period_ns: 100_000_000,
        seed: 42,
        parameters: Default::default(),
    };
    println!("{}", serde_json::json!({ "encoded_hex": hex(&prost::Message::encode_to_vec(&msg)) }));
}

/// The reverse direction: prints the prost-encoded bytes of a `LockstepStepResponse` carrying
/// one `PortMessage` output (the apid-300 wheel-torque packet), so the test can check our own
/// `lockstep_encode_step_response` byte-for-byte against the real encoding.
fn print_lockstep_step_response() {
    let msg = pb::LockstepStepResponse {
        sequence: 5,
        reached_tai_ns: 123_456_789_000,
        outputs: vec![pb::PortMessage { port: "wheel_torque_out".to_string(), tai_ns: 123_456_789_000, payload: wheel_torque_packet_bytes() }],
        named_outputs: Default::default(),
    };
    println!("{}", serde_json::json!({ "encoded_hex": hex(&prost::Message::encode_to_vec(&msg)) }));
}

fn print_lockstep_bind_response() {
    let msg = pb::LockstepBindResponse { lockstep_capable: true, binding_hash: "deadbeef".to_string(), version: "io_lockstep/0.1".to_string(), refusal_reason: String::new() };
    println!("{}", serde_json::json!({ "encoded_hex": hex(&prost::Message::encode_to_vec(&msg)) }));
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let case = args.get(1).map(String::as_str).unwrap_or("");
    match case {
        "lockstep_step_request" => {
            print_lockstep_step_request();
            return;
        }
        "lockstep_bind_request" => {
            print_lockstep_bind_request();
            return;
        }
        "lockstep_step_response" => {
            print_lockstep_step_response();
            return;
        }
        "lockstep_bind_response" => {
            print_lockstep_bind_response();
            return;
        }
        _ => {}
    }
    let (codec, values, sequence_count): (pb::PacketCodec, BTreeMap<String, FieldValue>, u16) = match case {
        "imu" => (imu_codec(), imu_values(), 42),
        "wheel_torque" => (wheel_torque_codec(), wheel_torque_values(), 7),
        other => {
            eprintln!("usage: cfs-golden-gen <imu|wheel_torque|lockstep_step_request|lockstep_bind_request|lockstep_step_response|lockstep_bind_response> [--decode <hex>]");
            eprintln!("unknown case: {other:?}");
            std::process::exit(2);
        }
    };

    if args.get(2).map(String::as_str) == Some("--decode") {
        let hex_in = args.get(3).expect("--decode needs a hex argument");
        let map = validate_system_packet_codecs(std::slice::from_ref(&codec)).expect("codec validates");
        let decoded = decode_packet(&map, &unhex(hex_in)).expect("decode_packet");
        let mut fields = serde_json::Map::new();
        for (k, v) in &decoded.fields {
            let FieldValue::Numeric(n) = v else { continue };
            fields.insert(k.clone(), serde_json::json!(n));
        }
        println!(
            "{}",
            serde_json::json!({
                "apid": decoded.apid,
                "is_command": decoded.is_command,
                "sequence_count": decoded.sequence_count,
                "fields": fields,
            })
        );
        return;
    }

    let encoded = encode_packet(&codec, sequence_count, &[], &values).expect("encode_packet");
    println!(
        "{}",
        serde_json::json!({
            "apid": codec.apid,
            "is_command": codec.is_command,
            "sequence_count": sequence_count,
            "user_data_bytes": codec.user_data_bytes,
            "values": values_to_json(&values),
            "encoded_hex": hex(&encoded),
        })
    );
}

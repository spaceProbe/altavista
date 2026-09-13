//! E4b (`docs/edge-plan.md` milestone E4, "the plugin as a labelled container"): generates
//! `services/edge-plugin/plugin-config.default.json`, the `PluginConfig` JSON document
//! baked into the runtime image (`services/edge-plugin/Dockerfile`) as the demo DRM's
//! default configuration.
//!
//! Deliberately generated, not hand-typed: `PluginConfig::codec_bytes`/`label_bytes` are
//! each a `prost::Message::encode_to_vec` byte string, which `serde`'s default `Vec<u8>`
//! representation renders as a JSON array of small integers (dozens of entries) -- exactly
//! the kind of value a human should never transcribe by hand (question 164's "captured
//! artifact, not a guess" rule, applied to a JSON fixture instead of a hash). The config
//! built here is byte-for-byte the same `PluginConfig` `tests/plugin_replay.rs::config` and
//! `crates/av-ingest/tests/plugin_wire.rs::config` already build (this is the demo DRM's
//! one pinned configuration, used everywhere in this milestone) -- this file only adds the
//! "serialise it to the committed JSON file" step neither of those needed.
//!
//! `#[ignore]`, like `crates/av-kernel/tests/generate_e4a_ground_segment_fixture.rs`: run
//! by hand, once, and only again if `tests/plugin_replay.rs::config`'s own fields ever
//! change (at which point the JSON this writes and that function's own definition would
//! silently disagree if this generator were not re-run -- there is no automatic check tying
//! the committed JSON back to that function, so re-run this by hand after any such change).
//!
//! Regenerate with:
//! ```text
//! export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
//! export GMAT_ROOT="/Users/probe/code/AltaVista/GMAT R2026a"
//! export CFS_MIRROR_DIR=/Users/probe/code/AltaVista/third_party/mirrors
//! cargo test -p av-edge --test generate_edge_plugin_default_config -- --ignored --nocapture
//! ```

use av_edge::pb;
use av_edge::plugin::{BatchingRule, Pacing, PluginConfig};

/// Identical to `tests/plugin_replay.rs::flight_codec` -- see that file's own comment for
/// where every field comes from (`drms/demo_ground_segment_flight.system.yaml`).
fn flight_codec() -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Meter as i32,
        scale: 1.0,
        offset: 0.0,
        target: String::new(),
    };
    pb::PacketCodec {
        id: "flight_tm_out_codec".to_string(),
        apid: 500,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "own Earth-fixed Cartesian position telemetry, encoded (M25.1)".to_string(),
        fields: vec![f("x", 0), f("y", 64), f("z", 128)],
    }
}

/// Identical to `tests/plugin_replay.rs::config` / `crates/av-ingest/tests/
/// plugin_wire.rs::config` -- the demo DRM's one pinned `PluginConfig`.
fn config() -> PluginConfig {
    let codec = flight_codec();
    let label = pb::Label { marking: "CUI".to_string(), caveats: vec![] };
    PluginConfig {
        producer_id: "demo-ground-segment-flight-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "flight".to_string(),
        port: "tm_out".to_string(),
        direction: pb::PortDirection::Out as i32,
        codec_bytes: PluginConfig::encode_codec(&codec),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: "earth_fixed_demo_frame".to_string(),
        sensor_id: "ground-segment-flight".to_string(),
        measurement_id: "flight_position".to_string(),
        shard_key: "ground-segment-demo".to_string(),
        noise_r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

#[test]
#[ignore]
fn generate_default_plugin_config_json() {
    let cfg = config();
    cfg.validate().expect("the demo DRM's own pinned config must validate");
    let json = serde_json::to_string_pretty(&cfg).expect("PluginConfig serialises");

    // Round-trip check before writing anything -- never commit a file this same process
    // could not read back.
    let round_tripped: PluginConfig = serde_json::from_str(&json).expect("the JSON this test is about to write must deserialise back to a PluginConfig");
    assert_eq!(round_tripped, cfg, "round trip must be lossless");

    let out_path = PathBufFromManifest::edge_plugin_default_config_path();
    std::fs::write(&out_path, format!("{json}\n")).unwrap_or_else(|e| panic!("writing {}: {e}", out_path.display()));
    println!("wrote {}", out_path.display());
}

/// Tiny local helper (not worth a whole module) resolving `services/edge-plugin/
/// plugin-config.default.json` relative to this crate's own manifest dir, exactly the way
/// `tests/plugin_replay.rs::fixtures_dir` resolves its own fixture directory.
struct PathBufFromManifest;
impl PathBufFromManifest {
    fn edge_plugin_default_config_path() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/edge-plugin/plugin-config.default.json")
    }
}

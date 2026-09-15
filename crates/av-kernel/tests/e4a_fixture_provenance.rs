//! Question 210 (lead, 2026-09-15): "a fixture whose recorded provenance hash names a DRM
//! that no longer exists is not reproducible from its hash, which is this platform's one
//! rule." Round 5 regenerated `crates/av-edge/tests/fixtures/ground_segment/` so its
//! recorded provenance names `drms/demo_ground_segment.drm.yaml` as it is committed today.
//!
//! **This file is the guard that keeps it true.** Before it existed the relationship was
//! documented in that fixture's own `README.md` and asserted by nothing, which is exactly how
//! round 4 was able to move the DRM's `hash:` and leave the fixture naming a DRM that no
//! longer existed anywhere in the tree -- for a whole round, invisibly. Round 3's own open
//! item 4 states the principle this file applies: *a number nothing guards is a number that
//! will drift silently.* If anyone edits `drms/demo_ground_segment*.yaml` again without
//! re-running `tests/generate_e4a_ground_segment_fixture.rs`, this test fails and names both
//! values.
//!
//! # Why this test lives in `crates/av-kernel`
//!
//! Computing a DRM's canonical hash needs `av_kernel::drm::{schema, hash}`, and question
//! 205's ruling is that `av-edge` may not build `gmat-sys` -- so, exactly like
//! `tests/edge_plugin_codec_crosscheck.rs` and `tests/generate_e4a_ground_segment_fixture.rs`
//! before it, this test reads `av-edge`'s committed fixture read-only by an explicit relative
//! path from THIS crate's own `CARGO_MANIFEST_DIR`. No network, no GMAT call, no filesystem
//! write anywhere in this file (question 154: offline and deterministic) -- it parses two
//! committed YAML files and two committed `.pb` files and compares strings.

use std::path::PathBuf;

use av_cdm::pb;
use av_kernel::drm::{hash, schema};
use openssl::sha::sha256;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `av-edge`'s own fixture directory, read from `crates/av-kernel`'s own `CARGO_MANIFEST_DIR`
/// -- see this module's own "Why this test lives in `crates/av-kernel`" section.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

fn read_drms(name: &str) -> String {
    let path = repo_root().join("drms").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn load_run_products() -> pb::RunProducts {
    let bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    <pb::RunProducts as prost::Message>::decode(bytes.as_slice()).expect("run_products.pb decodes as altavista.v1.RunProducts")
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The committed fixture's recorded generation provenance must name the DRM **as it is
/// committed right now** -- both the DRM's own declared `hash:` field and the hash recomputed
/// from its content, which `verify_drm_hash` proves are the same value.
#[test]
fn the_committed_e4a_fixture_records_the_current_demo_drms_own_hash() {
    let drm: pb::DesignReferenceMission = schema::parse_drm_yaml(&read_drms("demo_ground_segment.drm.yaml")).expect("demo_ground_segment.drm.yaml parses");

    // Not `drm.hash` on its own: a declared hash that does not match its own content would
    // otherwise let this test pass against a DRM the kernel itself would refuse to run.
    let computed = hash::verify_drm_hash(&drm).expect("the committed DRM's declared hash must match its own content");

    let products = load_run_products();
    let provenance = products.provenance.as_ref().expect("RunProducts carries a Provenance");

    println!("drms/demo_ground_segment.drm.yaml declared hash: {}", drm.hash);
    println!("recomputed canonical hash:                      {computed}");
    println!("fixture RunProducts.provenance.config_hash:     {}", provenance.config_hash);

    assert_eq!(
        provenance.config_hash, computed,
        "the committed E4a fixture's recorded provenance names a DRM that is not the one in the tree (question 210). \
         Regenerate it: `cargo test -p av-kernel --test generate_e4a_ground_segment_fixture -- --ignored --nocapture`, \
         then re-pin crates/av-edge/tests/fixtures/ground_segment/README.md from that run's own printed output."
    );

    // Every trajectory carries the same config hash (`executor::execute` reuses the value
    // `verify_drm_hash` returned), so a partial regeneration cannot hide here either.
    assert!(!products.trajectories.is_empty(), "the fixture must carry at least one trajectory");
    for (instance_id, trajectory) in &products.trajectories {
        assert_eq!(trajectory.config_hash, computed, "Trajectory {instance_id:?}'s config_hash must name the same DRM as the run's own provenance");
    }
}

/// `RunProducts.port_traffic_hash` must be the SHA-256 of the committed `port_traffic.pb`'s
/// exact bytes. `av_edge::plugin::verify_port_traffic_log` already checks this at replay time,
/// but only inside `av-edge`'s own tests; asserting it here means a regeneration that writes
/// one of the two files and not the other is caught by the kernel suite as well, beside the
/// provenance assertion it belongs with.
#[test]
fn the_committed_port_traffic_sidecar_hashes_to_the_value_run_products_records() {
    let products = load_run_products();
    let bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    let actual = hex_encode(&sha256(&bytes));

    println!("port_traffic.pb: {} bytes, sha256 {actual}", bytes.len());
    println!("RunProducts.port_traffic_hash: {}", products.port_traffic_hash);

    assert_eq!(
        actual, products.port_traffic_hash,
        "the committed port_traffic.pb does not hash to the value the committed run_products.pb records for it -- \
         the two fixture files are from different runs. Regenerate both together."
    );
}

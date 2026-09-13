//! Regenerates `crates/av-edge/tests/fixtures/ground_segment/`'s two committed files
//! (`docs/edge-plan.md` milestone E4a, the simulated-asset plugin): `run_products.pb` and
//! `port_traffic.pb`, the exact `RunProducts`/`PortTrafficLog` bytes a real execution of
//! `drms/demo_ground_segment.*.yaml` produces.
//!
//! **Not a test of correctness** -- `crates/av-kernel/tests/drm_ground_segment.rs` already
//! covers the ground-segment DRM's own behaviour (the rise-and-set pass, the contact events).
//! This file's only job is to run that same DRM once more with `RunConfig::products_dir` set,
//! and copy the two resulting files to where `crates/av-edge`'s plugin tests read them from --
//! so those tests are offline and deterministic (question 154), never re-running GMAT/av-kernel
//! themselves. `#[ignore]`d: it writes into another crate's `tests/fixtures/` directory, which
//! must never happen as a side effect of an ordinary `cargo test` run.
//!
//! # Regenerating
//!
//! ```text
//! export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
//! export GMAT_ROOT="/Users/probe/code/AltaVista/GMAT R2026a"
//! cargo test -p av-kernel --test generate_e4a_ground_segment_fixture -- --ignored --nocapture
//! ```
//!
//! This must only ever be re-run if `drms/demo_ground_segment*.yaml` themselves change (their
//! own committed `hash:` fields would need repinning too, via `cargo run -p av-kernel --example
//! drm_hash`) -- routine test runs must never regenerate these files, which is exactly why this
//! test carries `#[ignore]` and this module doc names the manual command rather than any CI
//! hook running it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, schema, RunConfig};
use gmat_sys::Gmat;
use prost::Message as _;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

/// Where `crates/av-edge`'s own plugin tests read the committed fixture bytes from --
/// `CARGO_MANIFEST_DIR` is this crate's own directory (`crates/av-kernel`), so this walks up
/// one level and across to the sibling crate, exactly like `drms_path` above walks up to the
/// repository-root `drms/` directory.
fn av_edge_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

#[test]
#[ignore = "writes into crates/av-edge/tests/fixtures/ground_segment/ -- run manually, see this file's module doc"]
fn regenerate_the_ground_segment_fixture_for_av_edges_plugin_tests() {
    let _engine = gmat_sys::engine_lock();

    let drm: DesignReferenceMission = schema::parse_drm_yaml(&read("demo_ground_segment.drm.yaml")).expect("DRM parses");
    let sos: SosConfiguration = schema::parse_sos_yaml(&read("demo_ground_segment.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_ground_segment_flight");
    let ground = load_system("demo_ground_segment_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let out_dir = av_edge_fixture_dir();
    std::fs::create_dir_all(&out_dir).unwrap_or_else(|e| panic!("creating {}: {e}", out_dir.display()));

    // A separate scratch products_dir (not out_dir itself): `execute` writes `port_traffic.pb`
    // under `RunConfig::products_dir` verbatim, with no name this test controls, and this test
    // wants that file committed under a stable, documented name (`port_traffic.pb`, matching
    // what `execute` actually calls it -- see `crate::drm::executor`'s own `write_port_traffic_
    // sidecar`) alongside `run_products.pb` (this test's own chosen name for the `RunProducts`
    // wire bytes, mirroring `crates/av-run`'s own `--out` convention).
    let scratch_dir = std::env::temp_dir().join(format!("av-edge-e4a-fixture-gen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch_dir);
    std::fs::create_dir_all(&scratch_dir).unwrap();

    let cfg = RunConfig {
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "e4a-demo-ground-segment-fixture".to_string(),
        error_mode: Default::default(),
        products_dir: Some(scratch_dir.clone()),
        replay: None,
        command_source: None,
    };
    let products = execute(cfg).expect("the ground-segment DRM executes end to end");
    assert!(!products.port_traffic_hash.is_empty(), "products_dir was set, so a real PortTrafficLog sidecar must have been written and hashed");

    let run_products_bytes = products.to_proto().encode_to_vec();
    let run_products_path = out_dir.join("run_products.pb");
    std::fs::write(&run_products_path, &run_products_bytes).unwrap_or_else(|e| panic!("writing {}: {e}", run_products_path.display()));

    let sidecar_src = scratch_dir.join("port_traffic.pb");
    let sidecar_bytes = std::fs::read(&sidecar_src).unwrap_or_else(|e| panic!("reading {}: {e}", sidecar_src.display()));
    let sidecar_dst = out_dir.join("port_traffic.pb");
    std::fs::write(&sidecar_dst, &sidecar_bytes).unwrap_or_else(|e| panic!("writing {}: {e}", sidecar_dst.display()));

    println!("wrote {} ({} bytes)", run_products_path.display(), run_products_bytes.len());
    println!("wrote {} ({} bytes)", sidecar_dst.display(), sidecar_bytes.len());
    println!("port_traffic_hash = {}", products.port_traffic_hash);
    println!("run_id            = {}", products.provenance.run_id);

    let _ = std::fs::remove_dir_all(&scratch_dir);
}

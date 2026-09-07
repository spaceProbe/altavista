//! M8.3 acceptance test (ADR-005 section 5 / spoore ADR-004): two runs of the *same* DRM
//! produce byte-identical serialized `Trajectory` output.
//!
//! This drives the full `executor::execute` pipeline (not just `drm::fault`'s functions in
//! isolation, unlike `tests/faults_seeded.rs`) over a DRM that: is entirely GMAT-free (a native
//! `"accel.x"` binding, like `drm_executor.rs`'s own fault-splitting test, so it runs without a
//! GMAT install); declares one `FAULT_TARGET_KIND_DYNAMICS` fault, so the fault-splitting path
//! this crate actually executes today is exercised, not skipped; and declares a non-empty
//! `Scenario.seeds` map, to prove that carrying seeds through hashing and loading does not
//! itself introduce any run-to-run variation (`executor::execute`'s pipeline does not yet read
//! `Scenario.seeds` for anything -- see `drm::fault`'s "Integration note" doc comment -- so
//! this test cannot by itself prove PORT/SENSOR determinism end-to-end; `tests/faults_seeded.rs`
//! proves the seeded realization itself is reproducible, in isolation, for those two kinds).
//!
//! Bytes, not a summary: both runs' `av_cdm::pb::Trajectory` are encoded with
//! `prost::Message::encode_to_vec` (the same canonical encoding `drm::hash` uses) and compared
//! as `Vec<u8>` -- `assert_eq!` on the encoded bytes, not on parsed fields, a tolerance, or a
//! hash of a hash.

use std::collections::BTreeMap;

use av_cdm::pb::{Binding, BindingKind, DesignReferenceMission, DrmOptions, Fault, FaultTargetKind, ModelBinding, Parameter, Scenario, SosConfiguration, SystemDefinition, SystemInstance};
use av_kernel::drm::{execute, hash, RunConfig};
use gmat_sys::Gmat;

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}
fn hashed_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}
fn hashed_sos(mut sos: SosConfiguration) -> SosConfiguration {
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}
fn hashed_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// Build the same fault-bearing, seed-bearing, GMAT-free DRM `drm_matches_the_golden_arc`'s
/// sibling test in `drm_executor.rs` uses for its own native-binding fault test, plus a
/// populated `Scenario.seeds` map -- everything [`execute`] needs to run once.
fn seeded_fault_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sys = hashed_system(SystemDefinition {
        id: "accel_sys".to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", 1.0),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    });
    let sos = hashed_sos(SosConfiguration {
        id: "accel_sos_det".to_string(),
        instances: vec![SystemInstance {
            name: "veh".to_string(),
            system_id: "accel_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "accel_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "accel_drm_det".to_string(),
        sos_configuration_id: "accel_sos_det".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: 2_000_000_000,
            // Populated even though execute() does not yet consume it for anything -- proves
            // a non-empty Scenario.seeds round-trips through hashing/loading without changing
            // the run (see the module doc comment).
            seeds: BTreeMap::from([("f1".to_string(), 777_777u64), ("f2".to_string(), 1u64)]),
            faults: vec![Fault {
                id: "f1".to_string(),
                tai_ns: 1_000_000_000,
                target_kind: FaultTargetKind::Dynamics as i32,
                instance: "veh".to_string(),
                target: "accel.x".to_string(),
                kind: "parameter".to_string(),
                params: BTreeMap::from([("value".to_string(), 5.0)]),
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    (drm, sos, systems)
}

/// The acceptance criterion: run the same DRM twice (same `run_id`, same everything) and
/// compare the serialized `Trajectory` bytes exactly.
#[test]
fn two_runs_of_the_same_drm_produce_byte_identical_trajectories() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = seeded_fault_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let run = || {
        let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-determinism".to_string(), error_mode: Default::default() };
        let mut products = execute(cfg).expect("DRM executes end to end");
        products.trajectories.remove("veh").expect("instance produced a trajectory")
    };

    let first = run();
    let second = run();

    let first_bytes = prost::Message::encode_to_vec(&first);
    let second_bytes = prost::Message::encode_to_vec(&second);

    assert!(!first_bytes.is_empty(), "sanity: a real trajectory was actually encoded");
    assert_eq!(first_bytes, second_bytes, "two runs of the same DRM must produce byte-identical encoded Trajectory output (ADR-004 replay-exactness)");

    // Not just "the bytes happen to match" -- confirm the run actually did something
    // non-trivial (the fault split, at least two segments and a moved final state), so an
    // empty/degenerate run can't pass this test vacuously.
    assert_eq!(first.segments.len(), 2, "one DYNAMICS fault -> two dynamics segments");
    let last = first.samples.last().expect("at least one sample");
    assert!((last.mean[0] - 4.0).abs() < 1e-9, "x(2s) = {} (closed-form: 0.5+1*1+0.5*5*1^2 = 4.0)", last.mean[0]);
}

/// The same content, re-encoded from scratch as a brand-new `DesignReferenceMission`/
/// `SosConfiguration`/`SystemDefinition` (not the same Rust value re-run -- a second,
/// independently-built bundle whose fields are equal but whose objects are not the same
/// allocation), still produces identical bytes -- rules out "determinism" being an artifact of
/// reusing the exact same in-memory structs.
#[test]
fn a_freshly_rebuilt_but_content_identical_drm_produces_the_same_bytes() {
    let _engine = gmat_sys::engine_lock();
    let (drm_a, sos_a, systems_a) = seeded_fault_bundle();
    let (drm_b, sos_b, systems_b) = seeded_fault_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let cfg_a = RunConfig { gmat: &gmat, drm: &drm_a, sos: &sos_a, systems: &systems_a, run_id: "test-run-determinism-fresh".to_string(), error_mode: Default::default() };
    let mut products_a = execute(cfg_a).expect("DRM executes end to end");
    let a = products_a.trajectories.remove("veh").unwrap();

    let cfg_b = RunConfig { gmat: &gmat, drm: &drm_b, sos: &sos_b, systems: &systems_b, run_id: "test-run-determinism-fresh".to_string(), error_mode: Default::default() };
    let mut products_b = execute(cfg_b).expect("DRM executes end to end");
    let b = products_b.trajectories.remove("veh").unwrap();

    assert_eq!(prost::Message::encode_to_vec(&a), prost::Message::encode_to_vec(&b));
}

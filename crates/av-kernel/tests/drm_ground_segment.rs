//! M25.1 (`docs/sil-plan.md`'s M25 milestone: "ground segment as a system"): the standing scope
//! rule's own "one DRM fixture instantiates it through `execute()`" requirement for
//! `crate::drm::ground::GroundStationModel`, exercised against `drms/demo_ground_segment.*.yaml`
//! (`crate::drm::binding::ConstantAccelModel`, native flight telemetry source, connected over a
//! real FRAMED `crate::router::Router` link with a declared, non-zero latency, to a real
//! `GroundStationModel`).
//!
//! **Expected result, stated before running** (`drms/demo_ground_segment_flight.system.yaml`'s
//! own header comment has the geometry derivation): one rise-and-set pass over the declared
//! Cape-Canaveral-like site, elevation crossing the 10-degree mask upward near t=50s and downward
//! near t=795s, at a 1 Hz kernel step -- exactly one `EVENT_KIND_CONTACT_START` and one
//! `EVENT_KIND_CONTACT_END` event, both on the `ground` instance, in that order, with the start
//! epoch strictly before the end epoch and both landing inside `[40s, 60s]`/`[785s, 815s]` of the
//! declared start (a few kernel steps' slack around the linearly-interpolated crossing, not an
//! exact-second assertion -- the interpolation itself is unit-tested precisely in
//! `crate::drm::ground::tests::contact_windows_interpolates_a_single_rise_and_set_pass`).
//!
//! Fails against an implementation that never wires `"ground."` dispatch into
//! `crate::registry::kind_for` at all (the DRM would be refused before propagation starts) or
//! whose `AnyModel::GroundStation::step_with_ports` arm silently reaches a trait default instead
//! of delegating (no contact events would ever be produced, and `ground` would report
//! `output.ground.in_contact@end == 0.0` throughout even after a real rise-and-set pass) or whose
//! `ConstantAccelModel::emit_framed` never actually broadcasts (the ground instance's `tm_in`
//! `Inbox` would always be empty and no elevation could ever be computed at all).
//!
//! Every test here constructs a real `gmat_sys::Gmat` handle (`RunConfig.gmat`) even though this
//! DRM never binds a `"gmat."` instance -- `execute` takes `&Gmat` unconditionally (ADR-002, GMAT
//! is a process-wide singleton) -- and takes `gmat_sys::engine_lock()` first, per this
//! repository's existing convention (`tests/drm_attitude_sensors.rs`'s own copy of this note).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, EventKind, MeasureOfEffectiveness, SosConfiguration, SystemDefinition, Unit};
use av_kernel::drm::{execute, hash, schema, RunConfig};
use gmat_sys::Gmat;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

fn load_ground_segment_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_ground_segment.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_ground_segment.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_ground_segment_flight");
    let ground = load_system("demo_ground_segment_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: "test-run-drm-ground-segment".to_string(), error_mode: Default::default() , products_dir: None, replay: None }
}

/// Mirrors `tests/drm_attitude_sensors.rs::rehash_drm`'s own doc comment: not a way to bypass
/// `execute`'s tamper check, the opposite -- keeps a deliberately mutated in-memory fixture
/// variant (here, the added `MeasureOfEffectiveness`) honestly self-consistent.
fn rehash_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

const START_TAI_NS: i64 = 1_767_225_637_000_000_000;

#[test]
fn the_ground_segment_drm_runs_through_execute_and_reports_one_rise_and_set_pass() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_ground_segment_bundle();
    let mut drm = drm;
    drm.measures = vec![MeasureOfEffectiveness { name: "ground_in_contact_at_end".to_string(), expression: "output.ground.in_contact@end".to_string(), unit: Unit::Dimensionless as i32 }];
    let drm = rehash_drm(drm);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems)).expect("the ground-segment DRM executes end to end");

    // `GroundStationModel::state_dim() == 0` (a fixed geodetic site has no propagated physical
    // state) -- "ground" deliberately produces no Trajectory entry, the same "emits no
    // trajectory" rule `drm_attitude_sensors.rs`'s own test already established for
    // `StarTrackerModel`. `flight` (a 6-dimensional ConstantAccelModel) does.
    assert_eq!(products.trajectories.len(), 1, "flight produces a trajectory; ground deliberately does not (state_dim() == 0)");
    assert!(products.trajectories.contains_key("flight"));
    assert!(!products.trajectories.contains_key("ground"));

    let contact_events: Vec<_> = products.events.iter().filter(|e| e.entity_id == "ground" && (e.kind == EventKind::ContactStart as i32 || e.kind == EventKind::ContactEnd as i32)).collect();
    assert_eq!(contact_events.len(), 2, "exactly one rise and one set: got {contact_events:?}");
    assert_eq!(contact_events[0].kind, EventKind::ContactStart as i32, "the rising edge (AOS) must be reported before the falling edge (LOS)");
    assert_eq!(contact_events[1].kind, EventKind::ContactEnd as i32);
    assert!(contact_events[0].tai_ns < contact_events[1].tai_ns, "AOS epoch must precede LOS epoch");

    // AOS near t=50s, LOS near t=795s (see drms/demo_ground_segment_flight.system.yaml's own
    // header comment) -- a generous +-10s window around each, well outside the 1 Hz sampling
    // grid's own interpolation error, but tight enough to fail against a wrong sign/rotation in
    // the elevation formula (which would report a wildly different crossing time or none at all).
    let aos_offset_s = (contact_events[0].tai_ns - START_TAI_NS) as f64 / 1e9;
    let los_offset_s = (contact_events[1].tai_ns - START_TAI_NS) as f64 / 1e9;
    assert!((40.0..60.0).contains(&aos_offset_s), "AOS at {aos_offset_s}s, expected near 50s");
    assert!((785.0..815.0).contains(&los_offset_s), "LOS at {los_offset_s}s, expected near 795s");

    // `output.ground.in_contact@end` must read 0.0 (the pass has already ended well before the
    // 900s run horizon) -- the declared MeasureOfEffectiveness above, resolved the same
    // "output.<instance>.<name>@time" way `drm_attitude_sensors.rs` reads `StepResult.
    // outputs["seq"]`.
    let in_contact_at_end = products.scores.get("ground_in_contact_at_end").expect("declared MeasureOfEffectiveness scored");
    assert_eq!(in_contact_at_end.value, 0.0, "the pass has already ended by the run's own horizon (900s > LOS ~795s)");
}

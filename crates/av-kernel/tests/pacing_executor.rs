//! The executor's side of real-time pacing (question 242, ADR-005 section 2), below what a
//! `BINDING_KIND_BOARD` instance can do today: `classify_binding` still refuses a board until
//! hilprep-3a, so no paced run can complete here. What is proven: the one predicate, the typed
//! refusals around it, that the decision to pace is made before any binding is touched, and that
//! a lockstep run carries no pacing product at all. The kernel half (`run_with_ports_paced`) is
//! `tests/pacing_kernel.rs`; the byte-for-byte lockstep proof is the baseline hash comparison
//! recorded in the task report.
//!
//! GMAT-free: runs in both feature states of `av-kernel`.

use std::collections::BTreeMap;

use av_cdm::pb::{Binding, BindingKind, BoardBinding, DesignReferenceMission, DrmOptions, ModelBinding, Parameter, Scenario, SosConfiguration, SystemDefinition, SystemInstance};
use av_kernel::drm::executor::{board_instance_names, run_requires_real_time};
use av_kernel::drm::{execute, hash, DrmError, RunConfig};
use prost::Message;

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}

fn accel_system() -> SystemDefinition {
    let mut sys = SystemDefinition {
        id: "accel_sys".to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", 1.0),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            Parameter { name: "frame_id".to_string(), string_value: "test.frame".to_string(), ..Default::default() },
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    };
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

fn model_instance(name: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: "accel_sys".to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "accel_sys".to_string() })) }),
        step_rate_hz: 10.0,
        ..Default::default()
    }
}

fn board_instance(name: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: "accel_sys".to_string(),
        binding: Some(Binding { kind: BindingKind::Board as i32, config: Some(av_cdm::pb::binding::Config::Board(BoardBinding::default())) }),
        step_rate_hz: 10.0,
        ..Default::default()
    }
}

fn bundle(instances: Vec<SystemInstance>, options: DrmOptions) -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let sys = accel_system();
    let mut sos = SosConfiguration { id: "pace_sos".to_string(), instances, ..Default::default() };
    sos.hash = hash::canonical_sos_hash(&sos);
    let mut drm = DesignReferenceMission {
        id: "pace_drm".to_string(),
        sos_configuration_id: "pace_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 1_000_000_000, ..Default::default() }),
        options: Some(options),
        ..Default::default()
    };
    drm.hash = hash::canonical_drm_hash(&drm);
    (drm, sos, BTreeMap::from([(sys.id.clone(), sys)]))
}

fn opts(real_time: bool, covariance: bool) -> DrmOptions {
    DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, real_time, covariance, ..Default::default() }
}

fn run(drm: &DesignReferenceMission, sos: &SosConfiguration, systems: &BTreeMap<String, SystemDefinition>) -> Result<av_kernel::drm::RunProducts, DrmError> {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    execute(RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm,
        sos,
        systems,
        run_id: "test-pacing-executor".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    })
}

#[test]
fn run_requires_real_time_is_true_exactly_when_some_instance_is_bound_to_a_board() {
    let none = SosConfiguration::default();
    assert!(!run_requires_real_time(&none));
    let models_only = SosConfiguration { instances: vec![model_instance("a"), model_instance("b")], ..Default::default() };
    assert!(!run_requires_real_time(&models_only));
    assert!(board_instance_names(&models_only).is_empty());
    let unbound = SosConfiguration { instances: vec![SystemInstance { name: "u".to_string(), ..Default::default() }], ..Default::default() };
    assert!(!run_requires_real_time(&unbound), "an instance with no binding is not a board");
    let mixed = SosConfiguration { instances: vec![model_instance("a"), board_instance("hw"), board_instance("hw2")], ..Default::default() };
    assert!(run_requires_real_time(&mixed));
    assert_eq!(board_instance_names(&mixed), vec!["hw".to_string(), "hw2".to_string()]);
}

/// ADR-005 section 2: pacing is entered only when a board is bound. The flag alone keeps its
/// typed refusal (and is not honoured as lockstep).
#[test]
fn the_real_time_flag_without_a_board_keeps_its_typed_refusal() {
    let (drm, sos, systems) = bundle(vec![model_instance("veh")], opts(true, false));
    let err = run(&drm, &sos, &systems).expect_err("real_time without a board is refused");
    assert!(matches!(err, DrmError::RealTimeNotSupported), "got {err:?}");
    assert!(err.to_string().contains("no instance is bound to a board"), "{err}");
}

/// The decision to pace is taken at the top of `execute`, before any binding is classified:
/// pacing and covariance are refused together, typed, whatever the binding does later.
#[test]
fn pacing_and_covariance_are_refused_together_before_any_binding_is_touched() {
    let (drm, sos, systems) = bundle(vec![model_instance("veh"), board_instance("hw")], opts(false, true));
    let err = run(&drm, &sos, &systems).expect_err("a board plus covariance is refused");
    match err {
        DrmError::InvalidDrmOptions { reason } => assert!(reason.contains("real-time pacing") && reason.contains("covariance"), "{reason}"),
        other => panic!("expected InvalidDrmOptions, got {other:?}"),
    }
}

/// With a board bound the flag is irrelevant (forced), and the run proceeds to classification,
/// which refuses `BINDING_KIND_BOARD` until hilprep-3a lands. When 3a replaces this refusal this
/// assertion is the one to change: the run then paces.
#[test]
fn a_board_instance_passes_the_real_time_gate_whatever_the_flag_says_and_reaches_classification() {
    for flag in [false, true] {
        let (drm, sos, systems) = bundle(vec![board_instance("hw")], opts(flag, false));
        let err = run(&drm, &sos, &systems).expect_err("classify_binding refuses a board until hilprep-3a");
        assert!(matches!(err, DrmError::UnsupportedBinding { .. }), "real_time = {flag}: got {err:?}");
    }
}

/// A lockstep run has no pacing product: the field is `None`, no event is a pacing event, and the
/// encoded `RunProducts` decodes with `pacing` unset.
#[test]
fn a_lockstep_run_carries_no_pacing_report_and_no_overrun_event() {
    let (drm, sos, systems) = bundle(vec![model_instance("veh")], opts(false, false));
    let products = run(&drm, &sos, &systems).expect("lockstep run");
    assert!(products.pacing.is_none());
    assert!(products.events.iter().all(|e| !av_kernel::pacing::is_overrun_event(e)));
    let wire = products.to_proto();
    assert!(wire.pacing.is_none());
    let decoded = av_cdm::pb::RunProducts::decode(wire.encode_to_vec().as_slice()).expect("decodes");
    assert!(decoded.pacing.is_none());
}

/// The wire field round-trips when present, and is the only difference it makes to the encoding.
#[test]
fn a_present_pacing_report_round_trips_and_is_the_only_thing_that_changes_the_bytes() {
    let (drm, sos, systems) = bundle(vec![model_instance("veh")], opts(false, false));
    let mut products = run(&drm, &sos, &systems).expect("lockstep run");
    let plain = products.to_proto().encode_to_vec();
    let report = av_cdm::pb::PacingReport { mode: av_cdm::pb::PacingMode::RealTime as i32, ticks_paced: 10, ..Default::default() };
    products.pacing = Some(report.clone());
    let paced = products.to_proto().encode_to_vec();
    assert!(paced.starts_with(&plain), "the new field is appended after every existing field");
    assert_eq!(&paced[plain.len()..plain.len() + 1], &[0x52], "field 10, length-delimited");
    let decoded = av_cdm::pb::RunProducts::decode(paced.as_slice()).unwrap();
    assert_eq!(decoded.pacing, Some(report));
}

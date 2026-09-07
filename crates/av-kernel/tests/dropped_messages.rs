//! M14.4 required test: in-flight port messages still queued when a run ends are not silently
//! dropped any more (`crate::router::Router`'s own module doc comment's "still pending when the
//! run ends" note; `av_kernel::drm::executor::execute`'s own module doc comment references this
//! file). A non-zero dropped count reaches `RunProducts.provenance.attributes[
//! "dropped_in_flight_messages"]` and produces exactly one `EVENT_KIND_LIFECYCLE` event naming
//! it; a zero count is still recorded on provenance (never omitted), but produces no such event.
//!
//! No GMAT dependency and no `services/lockstep-ref` subprocess: both fixtures are native
//! (GMAT-free) `"native.constant_accel"` instances exchanging a SIGNAL through a declared
//! `Connection`, the same `port.emit`/`port.emit_value`/`port.consume` vocabulary `tests/
//! drm_shared_run.rs`/`tests/ports_router.rs` already use.

use std::collections::BTreeMap;

use av_cdm::pb::{
    Binding, BindingKind, Connection, DesignReferenceMission, DrmOptions, EventKind, ModelBinding, Parameter, Port, PortDirection, PortKind, PortTiming, Scenario, SosConfiguration,
    SystemDefinition, SystemInstance,
};
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
fn model_binding(system_id: &str) -> Binding {
    Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }
}
fn native_instance(name: &str, system_id: &str, step_rate_hz: f64) -> SystemInstance {
    SystemInstance { name: name.to_string(), system_id: system_id.to_string(), binding: Some(model_binding(system_id)), step_rate_hz, ..Default::default() }
}
fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default() }
}

fn signal_port(name: &str, direction: PortDirection, latency_ns: i64) -> Port {
    Port { name: name.to_string(), kind: PortKind::Signal as i32, direction: direction as i32, timing: if latency_ns == 0 { None } else { Some(PortTiming { latency_ns, ..Default::default() }) }, ..Default::default() }
}

/// A zero-acceleration native instance -- physics is not what this file is about, only port
/// delivery -- carrying whichever `port.*` parameters/`ports` the caller adds (mirrors `tests/
/// drm_shared_run.rs::native_system`).
fn native_system(id: &str, ports: Vec<Port>, extra_parameters: Vec<Parameter>) -> SystemDefinition {
    let mut parameters = vec![sparam("frame_id", "test.frame"), param("state.px", 0.0), param("state.py", 0.0), param("state.pz", 0.0), param("state.vx", 0.0), param("state.vy", 0.0), param("state.vz", 0.0)];
    parameters.extend(extra_parameters);
    hashed_system(SystemDefinition { id: id.to_string(), dynamics_model: "native.constant_accel".to_string(), state_space_id: "gmat.orbital.cartesian6".to_string(), ports, parameters, ..Default::default() })
}

fn producer_system(id: &str, latency_ns: i64) -> SystemDefinition {
    native_system(id, vec![signal_port("out", PortDirection::Out, latency_ns)], vec![sparam("port.emit", "out"), param("port.emit_value", 1.0)])
}
fn consumer_system(id: &str) -> SystemDefinition {
    native_system(id, vec![signal_port("in", PortDirection::In, 0)], vec![sparam("port.consume", "in")])
}

const SCENARIO_DURATION_S: i64 = 2;

fn build(sos_id: &str, drm_id: &str, connections: Vec<Connection>, latency_ns: i64) -> (BTreeMap<String, SystemDefinition>, SosConfiguration, DesignReferenceMission) {
    let producer_sys = producer_system("dropped_producer_sys", latency_ns);
    let consumer_sys = consumer_system("dropped_consumer_sys");
    let sos = hashed_sos(SosConfiguration {
        id: sos_id.to_string(),
        instances: vec![native_instance("producer", &producer_sys.id, 1.0), native_instance("consumer", &consumer_sys.id, 1.0)],
        connections,
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: SCENARIO_DURATION_S * 1_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 1.0, ..Default::default() }),
        ..Default::default()
    });
    let mut systems = BTreeMap::new();
    systems.insert(producer_sys.id.clone(), producer_sys);
    systems.insert(consumer_sys.id.clone(), consumer_sys);
    (systems, sos, drm)
}

/// Required test: every message the producer emits (2, one per native step over a 2 s / 1 Hz
/// scenario) carries 10 s of latency on *each* declared port end (20 s total, via the
/// `"latency"` link model summing both ends) -- far longer than the 2 s scenario, so neither
/// message's own availability epoch is ever reached before the run ends: both are lost, not
/// merely late.
///
/// **What this would catch:** an implementation that never reads `Router::pending_count` at all
/// (provenance would be missing the key, or the `.expect` below would panic); one that reads
/// `Router::has_pending` and writes a hardcoded `"1"` regardless of the real count (the exact
/// `"2"` assertion below would fail the moment the count is anything but coincidentally 1); or
/// one that records the count but never emits the companion `EVENT_KIND_LIFECYCLE` event (the
/// event-count/kind assertions below would fail).
#[test]
fn dropped_in_flight_messages_reach_provenance_and_emit_exactly_one_lifecycle_event() {
    let _engine = gmat_sys::engine_lock();
    let connections = vec![Connection { from_instance: "producer".to_string(), from_port: "out".to_string(), to_instance: "consumer".to_string(), to_port: "in".to_string(), link_model: "latency".to_string() }];
    let (systems, sos, drm) = build("dropped_sos", "dropped_drm", connections, 10_000_000_000);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-dropped")).expect("the DRM executes even though two messages never get delivered");

    assert_eq!(products.provenance.attributes.get("dropped_in_flight_messages").map(String::as_str), Some("2"), "{:?}", products.provenance.attributes);
    // M17.2 (question 121): the same count as a first-class RunProducts field, and on the
    // wire form -- fails against an implementation that leaves the new field at its default
    // (0) while the provenance attribute string still (correctly) says "2", i.e. the two
    // would silently disagree.
    assert_eq!(products.dropped_in_flight_messages, 2);
    assert_eq!(products.to_proto().dropped_in_flight_messages, 2);

    let dropped_events: Vec<_> = products.events.iter().filter(|e| e.kind == EventKind::Lifecycle as i32 && e.name == "dropped_in_flight_messages").collect();
    assert_eq!(dropped_events.len(), 1, "exactly one LIFECYCLE event naming the drop, no more, no fewer: {:?}", products.events);
    assert_eq!(dropped_events[0].values.get("dropped_count"), Some(&2.0));
    assert_eq!(dropped_events[0].tai_ns, SCENARIO_DURATION_S * 1_000_000_000, "recorded at the run's own end epoch");
    assert_eq!(dropped_events[0].entity_id, "", "a run-level event, not tied to any single instance");
}

/// Required test, the zero-count counterpart: a run that never declares a connection at all has
/// nothing to route and nothing left pending, so the count is unconditionally recorded as `"0"`
/// (never omitted -- `crate::drm::executor::build_run_provenance`'s own doc comment: "the same
/// ... this crate reports honestly rather than omitting when it happens to be uninteresting"),
/// and no `EVENT_KIND_LIFECYCLE` "dropped_in_flight_messages" event is emitted at all.
///
/// **What this would catch:** an implementation that always emits the LIFECYCLE event regardless
/// of the count (a spurious event would appear here where the brief requires none), or one that
/// omits the provenance attribute entirely when the count is zero rather than recording `"0"`
/// (the `.get(...)` assertion below would see `None` instead of `Some("0")`).
#[test]
fn zero_dropped_in_flight_messages_is_recorded_as_zero_and_emits_no_lifecycle_event() {
    let _engine = gmat_sys::engine_lock();
    let (systems, sos, drm) = build("not_dropped_sos", "not_dropped_drm", vec![], 0);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-run-not-dropped")).expect("the DRM executes with no connections declared at all");

    assert_eq!(products.provenance.attributes.get("dropped_in_flight_messages").map(String::as_str), Some("0"), "{:?}", products.provenance.attributes);
    assert_eq!(products.dropped_in_flight_messages, 0, "M17.2: the first-class field must also honestly record zero, not merely default to it");
    assert!(!products.events.iter().any(|e| e.kind == EventKind::Lifecycle as i32 && e.name == "dropped_in_flight_messages"), "{:?}", products.events);
}

//! Required tests 1 and 2 for `docs/open-questions.md` question 130 (M19.3): applied SIGNAL
//! port commands recorded as `EVENT_KIND_PORT_COMMAND` CDM events, referenced from the
//! applying instance's own `Trajectory.event_ids`, without opening a segment or changing
//! `dynamics_hash`.
//!
//! **Why a fresh, short-duration fixture rather than `drms/demo_two_instance.*`.** That fixture
//! wires `demo_mvr` to emit its own live `output.rmag` on every one of its ~72,000 native steps
//! over the full 2 h demo window, and `demo_flt` to consume (and therefore apply) it on
//! essentially every one of its own steps too -- a real, honest consequence of question 130's
//! decision ("every applied command"), but not something a test can usefully enumerate "the
//! count and each event's attributes" against (see this task's own report for the disclosed
//! fixture-size consequence on `tests/fixtures/demo_two_instance.runproducts.bin`). This file
//! reuses the identical mechanism -- the same SIGNAL emit-rmag/consume-into-Cd wiring
//! `drms/demo_two_instance.sos.yaml` already uses, and the same no-drag JGM2 8x8 + Sun/Moon
//! force model `tests/drm_executor.rs`'s own `replay_sys` fixture already uses -- over a
//! deliberately short, 3-native-step scenario (0.1 s period, 0.3 s total) so the applied-command
//! count is small and exact enough to enumerate by hand. Question 130's *physical* requirement
//! (a command that actually changes propagated dynamics) is covered separately, at the
//! `gmat-sys` level, by `crates/gmat-sys/tests/gmat_port_cd_command.rs`'s own drag-inclusive
//! fixture -- reused there per this task's brief, not rebuilt here.
//!
//! Two native steps sorted alphabetically before the emitter (`"port_cmd_rcv"` <
//! `"port_cmd_snd"`) means the receiver steps first at every tied native epoch
//! (`docs/open-questions.md` question 108's own instance-name tie-break, `crate::router`'s
//! module doc comment) -- so a command emitted at step *k* is applied at the receiver's own
//! step *k+1*, never step *k* itself: over 3 native steps, the receiver applies EXACTLY 2
//! commands (nothing at step 1: no message has ever been sent yet), the same "N-1" shape
//! `crates/av-kernel/tests/demo_two_instance.rs::
//! demo_two_instance_signal_port_delivers_a_real_gmat_to_gmat_command`'s own doc comment
//! already describes for the real demo fixture.

use std::collections::BTreeMap;

use av_cdm::pb::{
    Binding, BindingKind, Connection, DesignReferenceMission, DrmOptions, EventKind, MeasureOfEffectiveness, ModelBinding, Parameter, Port, PortDirection, PortKind, Scenario, SosConfiguration,
    SystemDefinition, SystemInstance, Unit,
};
use av_kernel::drm::{execute, hash, RunConfig, RunProducts};
use gmat_sys::Gmat;
use prost::Message;

const START_TAI_NS: i64 = 1_767_225_637_000_000_000; // 01 Jan 2026 00:00:00 UTC, this repo's usual epoch.
const PERIOD_NS: i64 = 100_000_000; // 0.1 s, 10 Hz.
const NATIVE_STEPS: i64 = 3;
const END_TAI_NS: i64 = START_TAI_NS + PERIOD_NS * NATIVE_STEPS;
const SENDER: &str = "port_cmd_snd";
const RECEIVER: &str = "port_cmd_rcv";
const SYSTEM_ID: &str = "port_cmd_sys";
const OUT_PORT: &str = "cd_cmd_out";
const IN_PORT: &str = "cd_cmd_in";

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}

/// Field-for-field the same physics `tests/drm_executor.rs`'s own `replay_sys` uses (JGM2 8x8 +
/// Luna/Sun, no drag -- this file is about the event *mechanism*, not about a command changing
/// the physics, which `gmat_port_cd_command.rs` already covers at the gmat-sys level), plus the
/// two SIGNAL ports and the two `output.*` declarations this test needs to cross-check delivered
/// values against.
fn system_def() -> SystemDefinition {
    SystemDefinition {
        id: SYSTEM_ID.to_string(),
        dynamics_model: "gmat.earth.jgm2_8x8.sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        ports: vec![
            Port { name: OUT_PORT.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, ..Default::default() },
            Port { name: IN_PORT.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, ..Default::default() },
        ],
        parameters: vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            sparam("force_model.point_masses", "Luna,Sun"),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Keplerian"),
            param("spacecraft.SMA", 6878.0),
            param("spacecraft.ECC", 0.001),
            param("spacecraft.INC", 51.6),
            param("spacecraft.RAAN", 30.0),
            param("spacecraft.AOP", 0.0),
            param("spacecraft.TA", 0.0),
            param("spacecraft.DryMass", 500.0),
            param("spacecraft.Cd", 2.2),
            param("spacecraft.Cr", 1.8),
            param("spacecraft.DragArea", 5.0),
            param("spacecraft.SRPArea", 5.0),
            Parameter { name: "output.rmag".to_string(), unit: Unit::Meter as i32, ..Default::default() },
            Parameter { name: "output.cd".to_string(), unit: Unit::Dimensionless as i32, ..Default::default() },
        ],
        ..Default::default()
    }
}

fn instance(name: &str, overrides: Vec<Parameter>) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: SYSTEM_ID.to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: SYSTEM_ID.to_string() })) }),
        step_rate_hz: 10.0,
        parameter_overrides: overrides,
        ..Default::default()
    }
}

fn build(run_id: &str) -> (SosConfiguration, DesignReferenceMission, BTreeMap<String, SystemDefinition>) {
    let mut sys = system_def();
    let mut sos = SosConfiguration {
        id: format!("{run_id}_sos"),
        instances: vec![
            instance(RECEIVER, vec![sparam("port.consume", IN_PORT), sparam("port.consume_parameter", "Cd")]),
            instance(SENDER, vec![sparam("port.emit", OUT_PORT), sparam("port.emit_output", "rmag")]),
        ],
        connections: vec![Connection { from_instance: SENDER.to_string(), from_port: OUT_PORT.to_string(), to_instance: RECEIVER.to_string(), to_port: IN_PORT.to_string(), link_model: String::new() }],
        ..Default::default()
    };
    sos.hash = hash::canonical_sos_hash(&sos);
    let mut drm = DesignReferenceMission {
        id: format!("{run_id}_drm"),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, ..Default::default() }),
        measures: vec![MeasureOfEffectiveness { name: "rcv_cd_at_end".to_string(), expression: format!("output.{RECEIVER}.cd@end"), unit: Unit::Dimensionless as i32 }],
        options: Some(DrmOptions { covariance: false, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    };
    drm.hash = hash::canonical_drm_hash(&drm);
    sys.hash = hash::canonical_system_hash(&sys);
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    (sos, drm, systems)
}

fn run(run_id: &str) -> RunProducts {
    let (sos, drm, systems) = build(run_id);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.to_string(), error_mode: Default::default() };
    execute(cfg).expect("the port-command DRM executes end to end")
}

fn rmag(mean: &[f64]) -> f64 {
    (mean[0] * mean[0] + mean[1] * mean[1] + mean[2] * mean[2]).sqrt()
}

/// **Required test 1**: the run emits EXACTLY the expected `EVENT_KIND_PORT_COMMAND` events --
/// the count (2, per this file's own module doc comment) and every attribute question 130
/// requires, cross-checked against the sender's own independently-recorded trajectory sample
/// (not merely "some event exists" or "the count is nonzero"). Also proves requirement 2 (each
/// event is referenced from `Trajectory.event_ids`).
///
/// Fails against: an implementation that emits a `PortCommand` event even when nothing was
/// applied (e.g. once per step regardless of whether the inbox had anything, which would report
/// 3 events here, not 2, and the first would carry no real predecessor value); one that gets the
/// count right but the wrong `entity_id`/`parameter`/`sender`/`epoch`/`value`; one that never
/// adds the event's id to `Trajectory.event_ids` at all (`event_ids` would stay empty, its
/// pre-M19.3 default -- see `crate::trajectory::build_trajectory`); or one that attributes the
/// wrong sender (e.g. the receiver's own name, or empty, instead of `"port_cmd_snd"`).
#[test]
fn the_commanded_run_emits_exactly_the_expected_port_command_events() {
    let _engine = gmat_sys::engine_lock();
    let products = run("test-port-cmd-events");

    let port_events: Vec<&av_cdm::pb::Event> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32).collect();
    assert_eq!(port_events.len(), 2, "exactly 2 applied commands over 3 native steps (receiver sorts before sender, so step 1 has nothing to apply yet): got {port_events:#?}");

    let traj_snd = products.trajectories.get(SENDER).expect("sender produced a trajectory");
    let traj_rcv = products.trajectories.get(RECEIVER).expect("receiver produced a trajectory");

    let mut seen_epochs = Vec::new();
    for e in &port_events {
        assert_eq!(e.entity_id, RECEIVER, "a port command applied by the receiver must be tied to the receiver's own entity_id, not the sender's or nobody's");
        assert_eq!(e.name, "Cd", "name must be the commanded parameter");
        assert_eq!(e.reference_id, IN_PORT, "reference_id must name the port the command arrived on");

        let prov = e.provenance.as_ref().expect("port_command_event always sets provenance");
        assert_eq!(prov.attributes.get("instance"), Some(&RECEIVER.to_string()));
        assert_eq!(prov.attributes.get("parameter"), Some(&"Cd".to_string()));
        assert_eq!(prov.attributes.get("sender"), Some(&SENDER.to_string()), "the sender must be the emitting instance, resolved from the router, not empty or the receiver's own name");
        assert_eq!(prov.attributes.get("epoch"), Some(&e.tai_ns.to_string()), "the epoch string attribute must agree with the typed tai_ns field");
        let value_attr: f64 = prov.attributes.get("value").expect("value attribute present").parse().expect("value attribute parses as a float");
        assert_eq!(value_attr, *e.values.get("value").expect("typed values[\"value\"] present"), "the string and typed forms of the applied value must agree exactly");

        // Cross-check: the applied value must equal the SENDER's own rmag at the identical
        // epoch (its own trajectory sample, an entirely independent recording of the same
        // number) -- proves the router really did carry the sender's own live value across,
        // not some other reading. Not asserted bit-exact: `value_attr` is GMAT's own `RMAG`
        // real parameter (`gmat_sys::model::OUTPUT_RMAG`, read straight off the spacecraft
        // object), while `expected_rmag` is Rust's own `sqrt(x^2+y^2+z^2)` recomputed from the
        // SAME sample's own Cartesian mean -- `gmat_sys::model::GmatModel`'s own module doc
        // comment measures these two independent computations of the identical physical
        // quantity agreeing only to sub-micrometre, never bit-for-bit; 1e-6 m is that same
        // documented bound, not a loosened tolerance chosen to make this pass.
        let snd_sample = traj_snd.samples.iter().find(|s| s.tai_ns == e.tai_ns).unwrap_or_else(|| panic!("sender has a sample at {}: {:?}", e.tai_ns, traj_snd.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>()));
        let expected_rmag = rmag(&snd_sample.mean);
        let rmag_delta = (value_attr - expected_rmag).abs();
        assert!(rmag_delta < 1e-6, "the applied Cd value ({value_attr}) must equal the sender's own rmag at the same epoch ({expected_rmag}) to sub-micrometre precision; got |delta| = {rmag_delta} m");
        assert!(expected_rmag > 6_000_000.0 && expected_rmag < 8_000_000.0, "sanity: a LEO rmag must be a few thousand km, got {expected_rmag}");

        seen_epochs.push(e.tai_ns);

        // Requirement 2: referenced from the applying instance's own Trajectory.event_ids.
        assert!(traj_rcv.event_ids.contains(&e.id), "the receiver's own Trajectory.event_ids must reference this port-command event's id ({:?}); got {:?}", e.id, traj_rcv.event_ids);
    }
    seen_epochs.sort_unstable();
    seen_epochs.dedup();
    assert_eq!(seen_epochs.len(), 2, "the two applied commands must land at two DIFFERENT epochs (one per native step), not the same one twice");

    // The last applied value must also be exactly what the receiver's own Cd reads back as at
    // the end of the run (nothing steps the receiver again after the last applied command in
    // this 3-step scenario, so its final Cd IS the last commanded value).
    let last_value = *port_events.iter().max_by_key(|e| e.tai_ns).unwrap().values.get("value").unwrap();
    let cd_at_end = products.scores.get("rcv_cd_at_end").expect("the declared MeasureOfEffectiveness evaluated").value;
    assert_eq!(cd_at_end, last_value, "the receiver's own output.cd@end must equal the last applied command's own recorded value");
    assert!((cd_at_end - 2.2).abs() > 1000.0, "sanity: the receiver's Cd must have moved far off its declared default of 2.2 -- got {cd_at_end}");

    // Sanity: `dynamics_hash` is unaffected -- both instances share the identical
    // configuration (`SYSTEM_ID`), so a per-command hash change would show up as a mismatch
    // between the two even though nothing about their own declared configuration differs.
    assert_eq!(
        traj_rcv.segments.len(),
        1,
        "no fault/maneuver boundary was declared, and question 130 explicitly forbids opening a segment per command -- the receiver must still have exactly one segment"
    );
    assert_eq!(traj_rcv.segments[0].dynamics_hash, traj_snd.segments[0].dynamics_hash, "both instances share an identical configuration and must hash identically regardless of the command the receiver alone applied");
}

/// **Required test 2**: two independent `execute()` calls over the identical configuration
/// (same `SosConfiguration`/`DesignReferenceMission`/`SystemDefinition`, same `run_id`) --
/// which, since GMAT's own propagation and the router's own delivery are both deterministic,
/// also apply the identical command stream -- produce byte-identical `RunProducts`, port-command
/// events included. Mirrors `tests/drm_executor.rs::
/// running_the_identical_drm_twice_in_one_process_produces_byte_identical_products`'s own
/// "same run_id both times" methodology, extended to a DRM that actually carries
/// `EVENT_KIND_PORT_COMMAND` events this time.
///
/// Fails against: any source of nondeterminism this task's own new code could have introduced
/// -- a `BTreeMap`-free iteration somewhere in the new `Vec<AppliedCommand>`/
/// `AppliedPortCommand` plumbing, a wall-clock read, or GMAT's own object-namespace hazard
/// resurfacing for a SIGNAL-wired instance specifically (M18.4 fixed the general case; this is
/// the first byte-identical replay proof for an instance that also applies port commands).
#[test]
fn two_runs_with_equal_configuration_and_equal_command_events_are_byte_identical() {
    let _engine = gmat_sys::engine_lock();
    let run_id = "test-port-cmd-replay";
    let products1 = run(run_id);
    let products2 = run(run_id);

    // Sanity: this is a real, non-vacuous comparison -- both runs actually produced the port
    // command events this test is about, not two empty (and therefore trivially equal) runs.
    let count1 = products1.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32).count();
    assert_eq!(count1, 2, "sanity: the first run produced the expected port-command events");

    let bytes1 = products1.to_proto().encode_to_vec();
    let bytes2 = products2.to_proto().encode_to_vec();
    assert_eq!(bytes1, bytes2, "running the identical port-command DRM twice (same run_id) in one process must produce byte-identical RunProducts");
}

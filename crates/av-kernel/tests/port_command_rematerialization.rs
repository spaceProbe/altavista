//! **Required test for M20.3** (`docs/open-questions.md` question 137): whatever this task
//! decided about a fault/maneuver re-materialization's effect on the "last applied value" cache
//! `gmat_sys::model::GmatModel::step_with_ports` now keeps (see that method's own doc comment
//! and its `last_applied` field's doc comment for the decision and its justification).
//!
//! **The trap this file exists to catch.** A fault or maneuver boundary re-materializes a
//! brand-new `GmatModel` for every currently active model instance
//! (`av_kernel::drm::executor::run_shared_group`'s own doc comment, "Faults and maneuvers"
//! section) -- `fault::rebind_gmat_spec_at_state`/`binding::materialize_gmat` rebuild the live
//! GMAT object straight from the plan's own spec, which resets a previously SIGNAL-commanded
//! field (here, `Cd`) back to whatever the spec itself declares. This task's decision is that
//! the "last applied value" cache is scoped to the `GmatModel` instance and therefore does NOT
//! survive that rebuild: a command applied after the boundary is judged a first application
//! again, even if its value happens to equal what was cached before the boundary, because the
//! live object's own value genuinely did revert at the boundary -- reporting nothing there
//! would claim a value is in effect that was never actually reapplied post-rebind.
//!
//! **Fixture.** `rcv` (GMAT-bound, JGM2 8x8 + Luna/Sun, field-for-field the same physics
//! `tests/port_command_events.rs::system_def` and `tests/drm_executor.rs`'s own `replay_sys`
//! already use) consumes a SIGNAL port into its own `Cd`. `snd` (native `ConstantAccelModel`)
//! emits a FIXED constant value on that port every step, unconditionally
//! (`port.emit`/`port.emit_value`, the same mechanism `tests/drm_shared_run.rs`'s own producer
//! uses) -- deliberately never varying, so any second `EVENT_KIND_PORT_COMMAND` this run
//! produces can only be explained by the cache having been reset, never by the commanded value
//! having genuinely changed. A `FAULT_TARGET_KIND_DYNAMICS` fault on `rcv` (targeting
//! `spacecraft.DryMass`, unrelated to `Cd`) sits at the run's own midpoint, landing on the
//! sample grid -- proving every active instance is re-materialized at a boundary regardless of
//! which field that boundary itself touches, exactly as `run_shared_group`'s own doc comment
//! states.
//!
//! Sorted alphabetically, `"rcv"` < `"snd"`, so the receiver steps first at any tied native
//! epoch (`docs/open-questions.md` question 108's own instance-name tie-break) -- a command
//! `snd` emits at step *k* is applied by `rcv` at step *k+1*, never step *k* itself, the same
//! "N-1" shape `tests/port_command_events.rs`'s own module doc comment already describes.

use std::collections::BTreeMap;

use av_cdm::pb::{
    Binding, BindingKind, Connection, DesignReferenceMission, DrmOptions, EventKind, Fault, FaultTargetKind, ModelBinding, Parameter, Port, PortDirection, PortKind, Scenario, SosConfiguration,
    SystemDefinition, SystemInstance, Unit,
};
use av_kernel::drm::{execute, hash, RunConfig};
use gmat_sys::Gmat;

const START_TAI_NS: i64 = 1_767_225_637_000_000_000; // 01 Jan 2026 00:00:00 UTC, this repo's usual epoch.
const PERIOD_NS: i64 = 100_000_000; // 0.1 s, 10 Hz.
const NATIVE_STEPS: i64 = 10;
const END_TAI_NS: i64 = START_TAI_NS + PERIOD_NS * NATIVE_STEPS;
/// The run's own midpoint (native step 5 of 10) -- strictly inside `[START_TAI_NS,
/// END_TAI_NS)` and exactly on the `sample_interval_s` grid, as `DrmError::
/// FaultEpochNotOnSampleGrid` requires.
const FAULT_TAI_NS: i64 = START_TAI_NS + PERIOD_NS * 5;
const RECEIVER: &str = "rcv";
const SENDER: &str = "snd";
const RECEIVER_SYS: &str = "rematerialize_rcv_sys";
const SENDER_SYS: &str = "rematerialize_snd_sys";
const OUT_PORT: &str = "cd_cmd_out";
const IN_PORT: &str = "cd_cmd_in";
/// The value `snd` emits on every single step of the run, without exception -- a native
/// `ConstantAccelModel`'s own `port.emit_value` is fixed, hashed configuration
/// (`av_kernel::drm::binding::ConstantAccelSpec::emit`), never touched by anything this fixture
/// does, so every message `rcv` ever consumes carries this exact bit pattern.
const CONST_CD: f64 = 4.4;

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}

fn receiver_system_def() -> SystemDefinition {
    SystemDefinition {
        id: RECEIVER_SYS.to_string(),
        dynamics_model: "gmat.earth.jgm2_8x8.sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        ports: vec![Port { name: IN_PORT.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, ..Default::default() }],
        parameters: vec![
            sparam("port.consume", IN_PORT),
            sparam("port.consume_parameter", "Cd"),
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
            Parameter { name: "output.cd".to_string(), unit: Unit::Dimensionless as i32, ..Default::default() },
        ],
        ..Default::default()
    }
}

/// A native `ConstantAccelModel`-classified system: zero acceleration, zero initial state
/// (physics is irrelevant to this test), emitting [`CONST_CD`] on [`OUT_PORT`] unconditionally,
/// every step (`crate::drm::binding::ConstantAccelSpec`'s own doc comment on `"port.emit"` +
/// `"port.emit_value"`, no `condition.*` declared).
fn sender_system_def() -> SystemDefinition {
    SystemDefinition {
        id: SENDER_SYS.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        ports: vec![Port { name: OUT_PORT.to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, ..Default::default() }],
        parameters: vec![
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
            sparam("port.emit", OUT_PORT),
            param("port.emit_value", CONST_CD),
        ],
        ..Default::default()
    }
}

fn instance(name: &str, system_id: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }),
        step_rate_hz: 10.0,
        ..Default::default()
    }
}

fn build(run_id: &str) -> (SosConfiguration, DesignReferenceMission, BTreeMap<String, SystemDefinition>) {
    let mut rcv_sys = receiver_system_def();
    let mut snd_sys = sender_system_def();
    let mut sos = SosConfiguration {
        id: format!("{run_id}_sos"),
        instances: vec![instance(RECEIVER, RECEIVER_SYS), instance(SENDER, SENDER_SYS)],
        connections: vec![Connection { from_instance: SENDER.to_string(), from_port: OUT_PORT.to_string(), to_instance: RECEIVER.to_string(), to_port: IN_PORT.to_string(), link_model: String::new() }],
        ..Default::default()
    };
    sos.hash = hash::canonical_sos_hash(&sos);
    let mut drm = DesignReferenceMission {
        id: format!("{run_id}_drm"),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario {
            start_tai_ns: START_TAI_NS,
            end_tai_ns: END_TAI_NS,
            faults: vec![Fault {
                id: "rematerialize_at_midpoint".to_string(),
                tai_ns: FAULT_TAI_NS,
                target_kind: FaultTargetKind::Dynamics as i32,
                instance: RECEIVER.to_string(),
                target: "spacecraft.DryMass".to_string(),
                kind: "parameter".to_string(),
                params: BTreeMap::from([("value".to_string(), 480.0)]),
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(DrmOptions { covariance: false, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    };
    drm.hash = hash::canonical_drm_hash(&drm);
    rcv_sys.hash = hash::canonical_system_hash(&rcv_sys);
    snd_sys.hash = hash::canonical_system_hash(&snd_sys);
    let mut systems = BTreeMap::new();
    systems.insert(rcv_sys.id.clone(), rcv_sys);
    systems.insert(snd_sys.id.clone(), snd_sys);
    (sos, drm, systems)
}

/// **The decisive assertion.** Exactly TWO `EVENT_KIND_PORT_COMMAND` events over a run whose
/// commanded value never actually changes (`snd` emits the identical [`CONST_CD`] on every
/// single step, by construction) -- one at the receiver's own first-ever applied command
/// (before the fault), and a SECOND at its first applied command after the fault's
/// re-materialization, because that re-materialization reset the live object's own `Cd` back to
/// the spec's declared `2.2` and the freshly rebuilt `GmatModel`'s own cache starts empty.
///
/// **Fails against:** an implementation whose "last applied value" cache survives
/// re-materialization (e.g. one kept in `ModelSpanState`/`HeteroSystemEntry` rather than on the
/// freshly-constructed `GmatModel` itself, or one deliberately carried across
/// `materialize_plan_at_boundary` calls) -- such an implementation would judge the post-fault
/// command "unchanged" (it matches [`CONST_CD`], still cached from before the fault) and
/// wrongly suppress it, reporting only ONE `EVENT_KIND_PORT_COMMAND` event for the whole run
/// even though the live object's own `Cd` genuinely reverted to `2.2` and was then
/// re-commanded to [`CONST_CD`] a second time.
#[test]
fn a_command_reapplied_after_a_fault_rematerialization_emits_a_second_event_even_though_its_value_never_changed() {
    let _engine = gmat_sys::engine_lock();
    let (sos, drm, systems) = build("test-port-cmd-rematerialize");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-port-cmd-rematerialize".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("the rematerialization-boundary DRM executes end to end");

    let port_events: Vec<&av_cdm::pb::Event> = products.events.iter().filter(|e| e.kind == EventKind::PortCommand as i32).collect();
    assert_eq!(
        port_events.len(),
        2,
        "exactly 2 port-command events: one before the fault boundary (this instance's first-ever \
         applied command) and one after it (the boundary's own re-materialization resets the cache, \
         so the next applied command -- even though its value never changed -- is judged a first \
         application again); got {port_events:#?}"
    );

    // Every event really did carry the SAME commanded value -- the point of the fixture: two
    // events here can only be explained by the cache resetting, never by the value changing.
    for e in &port_events {
        assert_eq!(e.values.get("value"), Some(&CONST_CD), "the commanded value never actually changes in this fixture -- both events must carry the identical value");
        assert_eq!(e.entity_id, RECEIVER);
        assert_eq!(e.name, "Cd");
    }

    // One event strictly before the fault epoch, one exactly AT it -- not two ticks of the
    // same span, and not both on the same side of the boundary. `AppliedCommand::
    // applied_tai_ns` is the applying step's own START epoch (`av_dynamics::AppliedCommand`'s
    // own doc comment), and the post-fault span's very first native step starts exactly at
    // `seg_start` (the fault's own `tai_ns`) -- so the second event's epoch is `FAULT_TAI_NS`
    // itself, not strictly after it.
    let mut epochs: Vec<i64> = port_events.iter().map(|e| e.tai_ns).collect();
    epochs.sort_unstable();
    assert!(epochs[0] < FAULT_TAI_NS, "the first port-command event must land before the fault boundary; got {epochs:?}");
    assert_eq!(epochs[1], FAULT_TAI_NS, "the second port-command event must land exactly at the fault boundary -- the START epoch of the first applied command in the re-materialized post-fault span; got {epochs:?}");

    // Sanity: the fault really did split the receiver's own trajectory (a real
    // re-materialization occurred, not a no-op) and, per question 130 (M19.3, unaffected by
    // this task), the command stream never opens a segment of its own -- exactly 2 segments,
    // one per fault, not 3 or more.
    let traj_rcv = products.trajectories.get(RECEIVER).expect("receiver produced a trajectory");
    assert_eq!(traj_rcv.segments.len(), 2, "one DYNAMICS fault -> two segments; the command stream itself must never split a segment");
    assert_ne!(traj_rcv.segments[0].dynamics_hash, traj_rcv.segments[1].dynamics_hash, "the DryMass fault must have changed the receiver's own dynamics_hash");
}

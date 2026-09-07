//! Shared fixtures for `tests/expr_objectives.rs` and `tests/expr_goldens.rs`: the same three
//! entirely GMAT-free (`"native.constant_accel"`) DRMs `examples/gen_expr_goldens.rs` defines
//! and pins into `goldens/expr_straight_accel.json` / `goldens/expr_fault_split_accel.json` /
//! `goldens/expr_range_duration.json`. Deliberately duplicated from the example rather than
//! shared through the crate's own public API (this fixture-building code has no reason to be
//! `av-kernel`'s own public surface) -- a `tests/` subdirectory (not a bare `tests/*.rs` file)
//! so cargo does not treat this as its own test binary, per the ordinary Rust convention for
//! cross-integration-test helpers.
//!
//! Kept byte-for-byte in step with `examples/gen_expr_goldens.rs`'s own `straight_accel_case`/
//! `fault_split_accel_case`/`range_duration_case`: if this module and the example ever disagree,
//! the golden files the example produced no longer describe what this crate's own tests
//! exercise, which `tests/expr_goldens.rs` would immediately catch as a value mismatch (not
//! silently pass).

use std::collections::BTreeMap;

use av_cdm::pb::{
    Binding, BindingKind, DesignReferenceMission, DrmOptions, Fault, FaultTargetKind, MeasureOfEffectiveness, ModelBinding, Objective, Parameter, Scenario, SosConfiguration, SystemDefinition,
    SystemInstance, Unit,
};
use av_kernel::drm::hash;

pub fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
pub fn sparam(name: &str, s: &str) -> Parameter {
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

fn accel_system(id: &str, ax: f64) -> SystemDefinition {
    positioned_accel_system(id, ax, 0.0)
}

/// Like `accel_system`, but with a declared initial `pos_x` other than 0 -- used by
/// [`range_duration_case`] to give its two entities distinct starting positions (`range(a, b)`
/// needs two positions apart to have anything to measure).
fn positioned_accel_system(id: &str, ax: f64, x0: f64) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", ax),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            sparam("frame_id", "test.frame"),
            param("state.px", x0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    })
}

fn one_instance_sos(id: &str, instance_name: &str, system_id: &str, step_rate_hz: f64) -> SosConfiguration {
    hashed_sos(SosConfiguration {
        id: id.to_string(),
        instances: vec![SystemInstance {
            name: instance_name.to_string(),
            system_id: system_id.to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }),
            step_rate_hz,
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// Two `SystemInstance`s in one `SosConfiguration`, each bound to its own `SystemDefinition` --
/// [`range_duration_case`]'s two entities. `#[allow(dead_code)]` for the same reason `GoldenCase
/// ::name` carries it above: `tests/expr_objectives.rs` compiles this module too but does not
/// call `range_duration_case` (the only caller), so its own dependency `two_instance_sos` would
/// otherwise warn as dead in that binary alone.
#[allow(dead_code)]
fn two_instance_sos(id: &str, name_a: &str, system_id_a: &str, name_b: &str, system_id_b: &str, step_rate_hz: f64) -> SosConfiguration {
    let instance = |name: &str, system_id: &str| SystemInstance {
        name: name.to_string(),
        system_id: system_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: system_id.to_string() })) }),
        step_rate_hz,
        ..Default::default()
    };
    hashed_sos(SosConfiguration { id: id.to_string(), instances: vec![instance(name_a, system_id_a), instance(name_b, system_id_b)], ..Default::default() })
}

pub struct GoldenCase {
    // Read by `tests/expr_goldens.rs` (which golden file to load); `tests/expr_objectives.rs`
    // does not use it -- each `tests/*.rs` binary compiles this shared module on its own, so
    // that binary alone would otherwise see it as dead code.
    #[allow(dead_code)]
    pub name: &'static str,
    pub drm: DesignReferenceMission,
    pub sos: SosConfiguration,
    pub systems: BTreeMap<String, SystemDefinition>,
    pub objectives: Vec<Objective>,
    pub measures: Vec<MeasureOfEffectiveness>,
}

/// `ax = 1 m/s^2` constant, 10 s at 10 Hz, `sample_interval_s = 0.1`.
///
/// `objectives`/`measures` are set on `DesignReferenceMission` itself (not just `GoldenCase`):
/// question 93's `execute()` now evaluates `cfg.drm.objectives`/`.measures` directly into
/// `RunProducts.scores`, so the DRM must actually declare them for that path to have anything
/// real to score (they are also hashed with the DRM, per ADR-005 sec 6/question 11).
pub fn straight_accel_case() -> GoldenCase {
    let sys = accel_system("straight_accel_sys", 1.0);
    let sos = one_instance_sos("straight_accel_sos", "veh", "straight_accel_sys", 10.0);
    let objectives = vec![
        Objective { name: "final_x_near_50m".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 50.0, tolerance: 0.01, unit: Unit::Meter as i32 },
        Objective { name: "final_vx_near_5".to_string(), expression: "entity.veh.vel_x@end".to_string(), target: 5.0, tolerance: 0.01, unit: Unit::MeterPerSecond as i32 },
    ];
    let measures = vec![
        MeasureOfEffectiveness { name: "mean_x".to_string(), expression: "mean(entity.veh.pos_x)".to_string(), unit: Unit::Meter as i32 },
        MeasureOfEffectiveness { name: "integral_vx".to_string(), expression: "integral(entity.veh.vel_x)".to_string(), unit: Unit::Meter as i32 },
        MeasureOfEffectiveness { name: "max_vx".to_string(), expression: "max(entity.veh.vel_x)".to_string(), unit: Unit::MeterPerSecond as i32 },
        MeasureOfEffectiveness { name: "final_x".to_string(), expression: "final(entity.veh.pos_x)".to_string(), unit: Unit::Meter as i32 },
    ];
    let drm = hashed_drm(DesignReferenceMission {
        id: "straight_accel_drm".to_string(),
        sos_configuration_id: "straight_accel_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 10_000_000_000, ..Default::default() }),
        objectives: objectives.clone(),
        measures: measures.clone(),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);

    GoldenCase {
        name: "expr_straight_accel",
        drm,
        sos,
        systems,
        objectives,
        measures,
    }
}

/// `ax = 1 m/s^2` for `[0, 1) s`, a `FAULT_TARGET_KIND_DYNAMICS` fault named `"accel_change"` at
/// `t = 1 s` changes it to `5 m/s^2` through `t = 2 s`, `10 Hz`, `sample_interval_s = 0.1` --
/// the same arc `tests/drm_executor.rs`'s
/// `a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state` pins by hand.
///
/// **All five expressions are declared on `DesignReferenceMission.objectives`/`.measures`
/// itself, including the three that reference `event.*`** (M9.3, question 95):
/// `crate::drm::executor::execute` now emits a real `EVENT_KIND_FAULT` `Event` for this fault
/// (`crate::drm::events::fault_event`), named after `Fault.id` (`"accel_change"` -- a `Fault` has
/// no separate `name` field) -- `entity.veh.vel_x@accel_change` and `event.accel_change.t` both
/// resolve against that real event, and `count(event.fault)` counts it (this fault is a
/// `FAULT_TARGET_KIND_DYNAMICS`/`"parameter"` change, not an impulsive maneuver, so it is
/// `EVENT_KIND_FAULT`, not `EVENT_KIND_MANEUVER` -- see `crate::drm::events`'s own module doc
/// comment for why this executor never emits `EVENT_KIND_MANEUVER` at all). Before M9.3 these
/// three expressions were kept off the DRM and evaluated only against a test-built
/// `ExprRunProducts` carrying a synthetic, caller-supplied `Event` (`RunProducts.events` was
/// always empty) -- that limitation is gone now that `execute()` itself produces real events.
pub fn fault_split_accel_case() -> GoldenCase {
    let sys = accel_system("fault_accel_sys", 1.0);
    let sos = one_instance_sos("fault_accel_sos", "veh", "fault_accel_sys", 10.0);
    let fault_tai_ns = 1_000_000_000;
    let objectives = vec![
        Objective { name: "final_x_near_4m".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 4.0, tolerance: 0.01, unit: Unit::Meter as i32 },
        Objective { name: "vx_at_fault_near_1".to_string(), expression: "entity.veh.vel_x@accel_change".to_string(), target: 1.0, tolerance: 0.01, unit: Unit::MeterPerSecond as i32 },
    ];
    let measures = vec![
        MeasureOfEffectiveness { name: "final_vx".to_string(), expression: "entity.veh.vel_x@end".to_string(), unit: Unit::MeterPerSecond as i32 },
        MeasureOfEffectiveness { name: "fault_count".to_string(), expression: "count(event.fault)".to_string(), unit: Unit::Dimensionless as i32 },
        MeasureOfEffectiveness { name: "accel_change_epoch".to_string(), expression: "event.accel_change.t".to_string(), unit: Unit::Second as i32 },
    ];
    let drm = hashed_drm(DesignReferenceMission {
        id: "fault_accel_drm".to_string(),
        sos_configuration_id: "fault_accel_sos".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: 2_000_000_000,
            faults: vec![Fault {
                id: "accel_change".to_string(),
                tai_ns: fault_tai_ns,
                target_kind: FaultTargetKind::Dynamics as i32,
                instance: "veh".to_string(),
                target: "accel.x".to_string(),
                kind: "parameter".to_string(),
                params: BTreeMap::from([("value".to_string(), 5.0)]),
                ..Default::default()
            }],
            ..Default::default()
        }),
        objectives: objectives.clone(),
        measures: measures.clone(),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);

    GoldenCase { name: "expr_fault_split_accel", drm, sos, systems, objectives, measures }
}

/// The amended grammar's own worked example (`docs/adr/005-simulation-kernel.md`'s amendment
/// 2026-09-02): `duration(range(a, b) < 100 m)`, run against real DRM products with two
/// entities named literally `a` and `b`, matching the amendment's own variable names.
///
/// `a`: `ax = 1 m/s^2` from rest at the origin, so `pos_x_a(t) = 0.5 t^2`. `b`: stationary at
/// `pos_x_b = 200 m`. Both entirely GMAT-free (`"native.constant_accel"`), 20 s at 10 Hz
/// (`sample_interval_s = 0.1`, 201 samples). Closed form: `range(a, b)(t) = |200 - 0.5 t^2|`,
/// and since `0.5 * 20^2 = 200` exactly, `a` never overtakes `b` within the run -- the
/// expression under the absolute value stays non-negative throughout, so
/// `range(a, b)(t) = 200 - 0.5 t^2`, monotonically decreasing from 200 m (t=0) to 0 m (t=20,
/// `a` exactly reaches `b`). The `< 100 m` crossing is `200 - 0.5 t^2 = 100`, i.e.
/// `t = sqrt(200) ~= 14.142136 s` -- irrational, so no 0.1 s grid sample lands on it (the
/// nearest samples, `t=14.1 s` at 100.595 m and `t=14.2 s` at 99.18 m, bracket the crossing with
/// clear margin on both sides, not a floating-point-fragile near-equality). Under this
/// evaluator's zero-order-hold-from-the-left-sample `duration` rule
/// (`crate::expr::eval`'s module doc comment), the condition's true samples are `t=14.2 s`
/// through `t=20.0 s` inclusive (58 samples, `k=142..=199` of 200 intervals), each contributing
/// its own `0.1 s` interval: `duration = 58 * 0.1 s = 5.8 s`.
///
/// `#[allow(dead_code)]`: only `tests/expr_goldens.rs` calls this, not `tests/
/// expr_objectives.rs` (which compiles this same module) -- see `two_instance_sos`'s own doc
/// comment.
#[allow(dead_code)]
pub fn range_duration_case() -> GoldenCase {
    let sys_a = positioned_accel_system("range_duration_a_sys", 1.0, 0.0);
    let sys_b = positioned_accel_system("range_duration_b_sys", 0.0, 200.0);
    let sos = two_instance_sos("range_duration_sos", "a", "range_duration_a_sys", "b", "range_duration_b_sys", 10.0);
    let objectives = vec![Objective {
        name: "duration_within_100m_near_5_8s".to_string(),
        expression: "duration(range(a, b) < 100 m)".to_string(),
        target: 5.8,
        tolerance: 0.05,
        unit: Unit::Second as i32,
    }];
    let measures = vec![
        MeasureOfEffectiveness { name: "range_at_end".to_string(), expression: "range(a, b)@end".to_string(), unit: Unit::Meter as i32 },
        MeasureOfEffectiveness { name: "range_at_start".to_string(), expression: "range(a, b)@start".to_string(), unit: Unit::Meter as i32 },
        MeasureOfEffectiveness { name: "min_range".to_string(), expression: "min(range(a, b))".to_string(), unit: Unit::Meter as i32 },
        MeasureOfEffectiveness { name: "max_range".to_string(), expression: "max(range(a, b))".to_string(), unit: Unit::Meter as i32 },
    ];
    // No event.* references in this case's expressions, so the full set is declared on the DRM
    // itself (unlike fault_split_accel_case) -- execute() scores every one of these directly.
    let drm = hashed_drm(DesignReferenceMission {
        id: "range_duration_drm".to_string(),
        sos_configuration_id: "range_duration_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 20_000_000_000, ..Default::default() }),
        objectives: objectives.clone(),
        measures: measures.clone(),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let mut systems = BTreeMap::new();
    systems.insert(sys_a.id.clone(), sys_a);
    systems.insert(sys_b.id.clone(), sys_b);

    GoldenCase { name: "expr_range_duration", drm, sos, systems, objectives, measures }
}

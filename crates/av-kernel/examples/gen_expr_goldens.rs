//! Generate the ADR-005 sec 6 (as corrected by the lead's amendment 2026-09-02) expression-
//! language goldens: `goldens/expr_straight_accel.json`, `goldens/expr_fault_split_accel.json`
//! and `goldens/expr_range_duration.json`. Run explicitly, never from a test (the same
//! convention `goldens/gen_leo_1day.py` documents in its own module doc comment):
//!
//! ```text
//! cargo run -p av-kernel --example gen_expr_goldens -- --reason "..."
//! ```
//!
//! Three small, entirely GMAT-free DRMs (`"native.constant_accel"` bindings, like
//! `tests/drm_executor.rs`'s own `a_dynamics_fault_splits_the_run_into_two_segments_with_
//! continuous_state` -- chosen for exactly the same reason: no GMAT install needed, and a
//! closed-form constant-acceleration solution to check the pinned numbers against by hand) are
//! run through [`av_kernel::drm::execute`] (which now also evaluates every declared
//! `Objective`/`MeasureOfEffectiveness` itself, into `RunProducts.scores` -- question 93); this
//! generator re-evaluates them independently with
//! [`av_kernel::expr::objective::evaluate_objective`]/`evaluate_moe` against the same
//! `RunProducts.trajectories` (`tests/expr_goldens.rs` cross-checks that the two agree). What is
//! pinned is the **evaluated score** (value, unit, pass/fail) for each objective/MoE, not the
//! trajectory itself (that is what `goldens/leo_1day_*.json` already pin, for the physics).
//!
//! Deliberately includes one objective pinned to **fail** (`final_vx_near_5`, expecting 5 m/s
//! against an actual 10 m/s): the honesty rule this task states -- "do not pin a golden score
//! you know is wrong" -- is about not silently *changing* what the run actually produces, not
//! about only ever pinning passing objectives; a golden that could never fail would not catch a
//! regression that broke pass/fail itself.
//!
//! `expr_fault_split_accel` also demonstrates the `event.<name>.t` / `count(event.<kind>)`
//! reference forms, resolved against the real `EVENT_KIND_FAULT` `Event`
//! `av_kernel::drm::executor::execute` itself now emits for the DRM's one applied
//! `FAULT_TARGET_KIND_DYNAMICS` fault (`docs/open-questions.md` question 95, M9.3 -- see
//! `av_kernel::drm::events`'s own module doc comment) -- `run_case` below reads them straight
//! off `RunProducts.events`, not a generator-supplied stand-in.
//!
//! `expr_range_duration` pins the amendment's own worked example, `duration(range(a, b) <
//! 100 m)`, against a real two-entity DRM (entities literally named `a`/`b`) -- see
//! `range_duration_case`'s own doc comment for the closed-form derivation of the pinned
//! `duration = 5.8 s`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{
    Binding, BindingKind, DesignReferenceMission, DrmOptions, Fault, FaultTargetKind, MeasureOfEffectiveness, ModelBinding, Objective, Parameter, Scenario, SosConfiguration, SystemDefinition,
    SystemInstance, Unit,
};
use av_kernel::drm::{execute, hash, DrmError, RunConfig};
use av_kernel::expr::objective::{evaluate_moe, evaluate_objective};
use av_kernel::expr::ExprRunProducts;
use gmat_sys::Gmat;
use serde::Serialize;
use sha2::Digest;

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

fn accel_system(id: &str, ax: f64) -> SystemDefinition {
    positioned_accel_system(id, ax, 0.0)
}

/// Like `accel_system`, but with a declared initial `pos_x` other than 0 -- used by
/// `range_duration_case` to give its two entities distinct starting positions.
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
/// `range_duration_case`'s two entities.
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

/// A DRM whose evaluated objective/MoE scores this run pins.
struct GoldenCase {
    /// `goldens/expr_<name>.json`.
    name: &'static str,
    drm: DesignReferenceMission,
    sos: SosConfiguration,
    systems: BTreeMap<String, SystemDefinition>,
    instance_name: &'static str,
    objectives: Vec<Objective>,
    measures: Vec<MeasureOfEffectiveness>,
}

/// `ax = 1 m/s^2` constant, 10 s at 10 Hz, `sample_interval_s = 0.1`. Closed form:
/// `x(10) = 0.5*1*10^2 = 50 m`, `vx(10) = 1*10 = 10 m/s`.
fn straight_accel_case() -> GoldenCase {
    let sys = accel_system("straight_accel_sys", 1.0);
    let sos = one_instance_sos("straight_accel_sos", "veh", "straight_accel_sys", 10.0);
    let objectives = vec![
        Objective { name: "final_x_near_50m".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 50.0, tolerance: 0.01, unit: Unit::Meter as i32 },
        // Deliberately pinned to fail -- see the module doc comment.
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

    GoldenCase { name: "expr_straight_accel", drm, sos, systems, instance_name: "veh", objectives, measures }
}

/// The same `"accel.x"` fault-split arc `tests/drm_executor.rs`'s own required test 5 pins by
/// hand: `ax = 1 m/s^2` for `[0, 1) s`, a `FAULT_TARGET_KIND_DYNAMICS` fault named
/// `"accel_change"` at `t = 1 s` changes it to `5 m/s^2` through `t = 2 s`, `10 Hz`,
/// `sample_interval_s = 0.1`. Closed form: `x(1s) = 0.5`, `vx(1s) = 1.0`;
/// `x(2s) = x(1s) + vx(1s)*1 + 0.5*5*1^2 = 4.0`, `vx(2s) = vx(1s) + 5*1 = 6.0`.
/// See `tests/expr_common/mod.rs`'s own copy of this function for why all five expressions,
/// including the three that reference `event.*`, are declared on
/// `DesignReferenceMission.objectives`/`.measures` itself (M9.3, question 95: `execute()` now
/// emits a real `EVENT_KIND_FAULT` event for this fault, named after `Fault.id`).
fn fault_split_accel_case() -> GoldenCase {
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

    GoldenCase { name: "expr_fault_split_accel", drm, sos, systems, instance_name: "veh", objectives, measures }
}

/// The amended grammar's own worked example (`docs/adr/005-simulation-kernel.md`'s amendment
/// 2026-09-02): `duration(range(a, b) < 100 m)`, run against real DRM products with two
/// entities named literally `a` and `b`, matching the amendment's own variable names. See
/// `tests/expr_common/mod.rs`'s own copy of this function for the full closed-form derivation
/// of `duration = 5.8 s`.
fn range_duration_case() -> GoldenCase {
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

    GoldenCase { name: "expr_range_duration", drm, sos, systems, instance_name: "a,b", objectives, measures }
}

#[derive(Serialize)]
struct PinnedObjective {
    name: String,
    expression: String,
    target: f64,
    tolerance: f64,
    unit: String,
    value: f64,
    computed_unit: String,
    pass: bool,
}

#[derive(Serialize)]
struct PinnedMoe {
    name: String,
    expression: String,
    unit: String,
    value: f64,
    computed_unit: String,
}

#[derive(Serialize)]
struct Golden {
    name: String,
    generated: String,
    reason: String,
    drm_id: String,
    drm_hash: String,
    instance: String,
    objectives: Vec<PinnedObjective>,
    measures: Vec<PinnedMoe>,
    sha256: String,
}

fn run_case(gmat: &Gmat, case: &GoldenCase, reason: &str) -> Result<Golden, DrmError> {
    let cfg = RunConfig { gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: format!("gen-{}", case.name), error_mode: Default::default() , products_dir: None };
    let products = execute(cfg)?;
    let scenario = case.drm.scenario.as_ref().expect("every case declares a scenario");
    let run = ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &products.trajectories, &products.events);

    let mut objectives = Vec::new();
    for obj in &case.objectives {
        let result = evaluate_objective(obj, &run).unwrap_or_else(|e| panic!("case {:?}, objective {:?}: {e}", case.name, obj.name));
        objectives.push(PinnedObjective {
            name: obj.name.clone(),
            expression: obj.expression.clone(),
            target: obj.target,
            tolerance: obj.tolerance,
            unit: av_kernel::expr::units::unit_display(av_cdm::pb::Unit::try_from(obj.unit).unwrap_or(av_cdm::pb::Unit::Unspecified)),
            value: result.value,
            computed_unit: av_kernel::expr::units::unit_display(result.unit),
            pass: result.pass,
        });
    }
    let mut measures = Vec::new();
    for moe in &case.measures {
        let result = evaluate_moe(moe, &run).unwrap_or_else(|e| panic!("case {:?}, measure {:?}: {e}", case.name, moe.name));
        measures.push(PinnedMoe {
            name: moe.name.clone(),
            expression: moe.expression.clone(),
            unit: av_kernel::expr::units::unit_display(av_cdm::pb::Unit::try_from(moe.unit).unwrap_or(av_cdm::pb::Unit::Unspecified)),
            value: result.value,
            computed_unit: av_kernel::expr::units::unit_display(result.unit),
        });
    }

    Ok(Golden {
        name: case.name.to_string(),
        generated: "recorded-by-generator".to_string(), // overwritten below with a real timestamp
        reason: reason.to_string(),
        drm_id: case.drm.id.clone(),
        drm_hash: case.drm.hash.clone(),
        instance: case.instance_name.to_string(),
        objectives,
        measures,
        sha256: String::new(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let reason = match args.iter().position(|a| a == "--reason").and_then(|i| args.get(i + 1)) {
        Some(r) => r.clone(),
        None => {
            eprintln!("usage: gen_expr_goldens --reason \"why this golden is (re)generated\"");
            std::process::exit(1);
        }
    };

    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup (unused by the GMAT-free native.constant_accel bindings below, but execute() takes &Gmat unconditionally)");

    let goldens_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens");
    let now = format!("{:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap());

    for case in [straight_accel_case(), fault_split_accel_case(), range_duration_case()] {
        let name = case.name;
        let mut golden = run_case(&gmat, &case, &reason).unwrap_or_else(|e| panic!("case {name:?}: DRM execution failed: {e}"));
        golden.generated = now.clone();
        let body = serde_json::to_string_pretty(&golden).unwrap();
        let mut hasher = sha2::Sha256::new();
        hasher.update(body.as_bytes());
        golden.sha256 = format!("{:x}", hasher.finalize());
        let out_path = goldens_dir.join(format!("{name}.json"));
        let final_body = serde_json::to_string_pretty(&golden).unwrap();
        std::fs::write(&out_path, final_body + "\n").unwrap_or_else(|e| panic!("writing {out_path:?}: {e}"));
        println!("wrote {}", out_path.display());
    }
}

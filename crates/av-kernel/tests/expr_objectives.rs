//! Integration coverage for M8.2 (ADR-005 sec 6): the full pipeline end to end --
//! `av_kernel::drm::execute` (a real DRM run, GMAT-free `"native.constant_accel"` bindings) ->
//! `av_kernel::expr::RunProducts` -> `av_kernel::expr::objective::evaluate_objective`/
//! `evaluate_moe` (parse -> typecheck -> evaluate) -- not just the unit tests already inside
//! `src/expr/*.rs`, which only ever construct a `Trajectory` fixture by hand. This is the one
//! place in the crate's test suite that proves an `Objective`/`MeasureOfEffectiveness` declared
//! on a real `DesignReferenceMission` is actually evaluated against what the executor produces
//! -- the gap `crate::drm::executor`'s own module doc comment and `docs/open-questions.md`
//! question 90 note ("objectives/MoEs currently parse and hash but are not evaluated").
//!
//! Fixtures shared with `tests/expr_goldens.rs` and `examples/gen_expr_goldens.rs` live in
//! `tests/expr_common/mod.rs`.

use av_kernel::drm::{execute, RunConfig};
use av_kernel::expr::objective::{evaluate_moe, evaluate_objective};
use av_kernel::expr::{ExprError, ExprRunProducts};
use gmat_sys::Gmat;

#[path = "expr_common/mod.rs"]
mod expr_common;
use expr_common::{fault_split_accel_case, straight_accel_case};

/// `entity.veh.pos_x@end`, `mean`/`integral`/`max`/`final` aggregates, an objective that
/// passes and one that (deliberately) fails, all against a real executor-produced trajectory.
/// Also checks `execute()`'s own `RunProducts.scores` (question 93) agree with evaluating the
/// same objectives/measures independently against `execute()`'s `trajectories` -- proving
/// `execute()` itself, not just a caller re-deriving an `ExprRunProducts`, evaluates real
/// scores over real run products.
#[test]
fn straight_accel_objectives_and_moes_match_the_closed_form_solution() {
    let _engine = gmat_sys::engine_lock();
    let case = straight_accel_case();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: "test-expr-straight-accel".to_string(), error_mode: Default::default() , products_dir: None };
    let products = execute(cfg).expect("DRM executes end to end");

    let scenario = case.drm.scenario.as_ref().unwrap();
    let run = ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &products.trajectories, &products.events);

    // x(10) = 0.5*1*10^2 = 50 m -- passes within 0.01 m.
    let final_x = evaluate_objective(&case.objectives[0], &run).unwrap();
    assert!((final_x.value - 50.0).abs() < 1e-6, "{final_x:?}");
    assert!(final_x.pass);
    let final_x_score = &products.scores[&case.objectives[0].name];
    assert!((final_x_score.value - final_x.value).abs() < 1e-9);
    assert_eq!(final_x_score.passed, Some(true));

    // vx(10) = 10 m/s, declared target 5 m/s: outside tolerance -- must fail, not be silently
    // coerced or dropped.
    let final_vx = evaluate_objective(&case.objectives[1], &run).unwrap();
    assert!((final_vx.value - 10.0).abs() < 1e-6, "{final_vx:?}");
    assert!(!final_vx.pass);
    assert_eq!(products.scores[&case.objectives[1].name].passed, Some(false));

    // mean(pos_x) over 101 samples of 0.5*(0.1*i)^2, i=0..=100: 0.5*0.01*mean(i^2) =
    // 0.005 * (338350/101) = 16.75 exactly (mean(i^2) for i=0..=100 has a closed form).
    let mean_x = evaluate_moe(&case.measures[0], &run).unwrap();
    assert!((mean_x.value - 16.75).abs() < 1e-9, "{mean_x:?}");
    assert_eq!(products.scores[&case.measures[0].name].passed, None, "a MeasureOfEffectiveness never carries pass/fail");

    // integral(vel_x) dt over [0,10] = x(10) - x(0) = 50 m (trapezoidal is exact here: vel_x
    // is piecewise-linear between native samples).
    let integral_vx = evaluate_moe(&case.measures[1], &run).unwrap();
    assert!((integral_vx.value - 50.0).abs() < 1e-6, "{integral_vx:?}");

    let max_vx = evaluate_moe(&case.measures[2], &run).unwrap();
    assert!((max_vx.value - 10.0).abs() < 1e-6, "{max_vx:?}");

    let final_x_agg = evaluate_moe(&case.measures[3], &run).unwrap();
    assert_eq!(final_x_agg.value, final_x.value, "final(ref) reads the same last sample as ref@end");

    // RunProducts.provenance (question 93): the run's own overall provenance carries the DRM's
    // own hash, and the run_id round-trips.
    assert_eq!(products.provenance.config_hash, case.drm.hash);
    assert_eq!(products.provenance.run_id, "test-expr-straight-accel");

    // RunProducts.events (question 95, M9.3): no faults declared, so exactly the one instance's
    // own run_start/run_end EVENT_KIND_LIFECYCLE pair -- sorted (epoch, id), so run_start (the
    // earlier epoch) comes first.
    assert_eq!(products.events.len(), 2, "{:?}", products.events);
    assert!(products.events.iter().all(|e| e.entity_id == "veh" && e.kind == av_cdm::pb::EventKind::Lifecycle as i32));
    assert_eq!(products.events[0].name, "run_start");
    assert_eq!(products.events[0].tai_ns, scenario.start_tai_ns);
    assert_eq!(products.events[1].name, "run_end");
    assert_eq!(products.events[1].tai_ns, scenario.end_tai_ns);
}

/// The fault-split arc: `entity.veh.vel_x@<event name>` resolves the `@time` production's
/// fourth alternative ("an event name resolves to its epoch") against the real
/// `EVENT_KIND_FAULT` `Event` `execute()` itself now emits for the one applied DYNAMICS fault
/// (question 95, M9.3), `count(event.fault)` and `event.accel_change.t` both resolve against
/// that same event.
#[test]
fn fault_split_accel_objectives_resolve_at_time_by_event_name() {
    let _engine = gmat_sys::engine_lock();
    let case = fault_split_accel_case();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: "test-expr-fault-split".to_string(), error_mode: Default::default() , products_dir: None };
    let products = execute(cfg).expect("DRM executes end to end");

    // RunProducts.events (question 95, M9.3): the one applied DYNAMICS fault (EVENT_KIND_FAULT,
    // named after Fault.id) plus this instance's own run_start/run_end EVENT_KIND_LIFECYCLE
    // pair, sorted (epoch, id) -- the fault sits strictly between the two lifecycle events.
    assert_eq!(products.events.len(), 3, "{:?}", products.events);
    let fault_event = products.events.iter().find(|e| e.kind == av_cdm::pb::EventKind::Fault as i32).expect("one FAULT event");
    assert_eq!(fault_event.name, "accel_change");
    assert_eq!(fault_event.reference_id, "accel_change");
    assert_eq!(fault_event.tai_ns, 1_000_000_000);
    assert_eq!(fault_event.values.get("value"), Some(&5.0), "the fault's realized (here, deterministic-by-construction) params");
    assert_eq!(products.events.iter().filter(|e| e.kind == av_cdm::pb::EventKind::Lifecycle as i32).count(), 2);

    let scenario = case.drm.scenario.as_ref().unwrap();
    let run = ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &products.trajectories, &products.events);

    // x(2s) = 4.0 m (0.5*1*1^2 over [0,1) then continuing at 5 m/s^2 for [1,2]).
    let final_x = evaluate_objective(&case.objectives[0], &run).unwrap();
    assert!((final_x.value - 4.0).abs() < 1e-6, "{final_x:?}");
    assert!(final_x.pass);

    // vel_x@accel_change (the event name) == vel_x at t=1s == 1.0 m/s.
    let vx_at_fault = evaluate_objective(&case.objectives[1], &run).unwrap();
    assert!((vx_at_fault.value - 1.0).abs() < 1e-6, "{vx_at_fault:?}");
    assert!(vx_at_fault.pass);

    let final_vx = evaluate_moe(&case.measures[0], &run).unwrap();
    assert!((final_vx.value - 6.0).abs() < 1e-6, "{final_vx:?}");

    let count = evaluate_moe(&case.measures[1], &run).unwrap();
    assert_eq!(count.value, 1.0);

    let epoch = evaluate_moe(&case.measures[2], &run).unwrap();
    assert!((epoch.value - 1.0).abs() < 1e-9, "{epoch:?}");
}

/// A malformed reference against a real run is a typed [`ExprError`], not a panic or a silent
/// zero. `expr_common`'s cases never declare a broken objective, so this builds a DRM whose one
/// objective references an unknown entity and checks `execute()` itself refuses it (question 93
/// / `DrmError::InvalidExpression`) -- the load-time validation path, not a caller re-deriving
/// an `ExprRunProducts` by hand.
#[test]
fn an_unknown_entity_in_a_declared_objective_is_refused_at_load() {
    let _engine = gmat_sys::engine_lock();
    let mut case = straight_accel_case();
    case.drm.objectives = vec![av_cdm::pb::Objective { name: "bogus".to_string(), expression: "entity.nope.pos_x@end".to_string(), target: 0.0, tolerance: 1.0, unit: 0 }];
    case.drm.hash = av_kernel::drm::hash::canonical_drm_hash(&case.drm);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: "test-expr-unknown-entity".to_string(), error_mode: Default::default() , products_dir: None };
    let err = execute(cfg).unwrap_err();
    match err {
        av_kernel::drm::DrmError::InvalidExpression { name, reason } => {
            assert_eq!(name, "bogus");
            assert!(reason.contains("nope"), "{reason:?}");
        }
        other => panic!("expected DrmError::InvalidExpression, got {other:?}"),
    }
}

/// The same malformed reference, checked directly against `crate::expr` (no executor involved)
/// -- proves the underlying `ExprError` this crate's evaluator itself produces is
/// `UnknownEntity`, the specific error `DrmError::InvalidExpression` above wraps.
#[test]
fn an_unknown_entity_against_a_real_run_is_a_typed_expr_error() {
    let _engine = gmat_sys::engine_lock();
    let case = straight_accel_case();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &case.drm, sos: &case.sos, systems: &case.systems, run_id: "test-expr-unknown-entity-2".to_string(), error_mode: Default::default() , products_dir: None };
    let products = execute(cfg).expect("DRM executes end to end");
    let scenario = case.drm.scenario.as_ref().unwrap();
    let run = ExprRunProducts::new(scenario.start_tai_ns, scenario.end_tai_ns, &products.trajectories, &[]);

    let bogus = av_cdm::pb::Objective { name: "bogus".to_string(), expression: "entity.nope.pos_x@end".to_string(), target: 0.0, tolerance: 1.0, unit: 0 };
    let err = evaluate_objective(&bogus, &run).unwrap_err();
    assert!(matches!(err, ExprError::UnknownEntity { ref id, .. } if id == "nope"), "{err:?}");
}

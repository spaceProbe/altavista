//! Evaluate a parsed, typechecked amended-grammar expression (`docs/adr/
//! 005-simulation-kernel.md`'s amendment 2026-09-02) against a run's products, producing a
//! unit-tagged number. Mirrors `crate::expr::typecheck::check`'s structure node-for-node (see
//! that module's doc comment for why the two passes are kept separate) but also carries the
//! actual numeric value: [`Value`] instead of a bare [`av_cdm::pb::Unit`].
//!
//! ## Aggregates: what "the run" means for `min`/`max`/`mean`/`final`/`integral`
//!
//! ADR-005 sec 6 lists the aggregate names but not their exact numerical definition (which
//! samples, which quadrature rule). This module's choice, disclosed here rather than left
//! implicit:
//! - `min`/`max`/`mean`/`final` reduce over the referenced entity/output/`range` series' own
//!   **native samples** (`Trajectory.samples`, or an output's own series, or `range`'s series --
//!   see [`series_of`]) -- not a resampled grid -- `mean` is the unweighted arithmetic mean of
//!   those sample values, and `final` is the value at the series' last sample (equivalent to
//!   that reference `@end`, but read directly rather than re-interpolated).
//! - `integral` is the trapezoidal quadrature of the series over its own native samples (exact
//!   for a piecewise-linear signal, which is what cubic-Hermite-sampled position/velocity
//!   approximates between samples at the granularity `DrmOptions.sample_interval_s` already
//!   fixes); its unit is the referenced quantity's unit composed with seconds
//!   (`crate::expr::units::mul_div_unit`).
//!
//! ## `range(<a>, <b>)` and `duration(<condition>)`: the amendment's worked example
//!
//! The amendment's own worked example, `duration(range(a, b) < 100 m)`, needs numerical
//! definitions the ADR's prose does not spell out any more precisely than the five aggregates
//! above -- disclosed choices, not resolved by inventing unstated ADR text:
//! - **`range(<a>, <b>)`** is the Euclidean distance between the two named entities'
//!   `pos_x`/`pos_y`/`pos_z` components, in metres (`ExprRunProducts::range_at`/`range_series`).
//!   `<a>`/`<b>` name entities directly (a bare identifier, e.g. `range(leo, gs)`), the same way
//!   `entity.<id>...` names one -- not a general two-argument function over arbitrary
//!   expressions, since a distance needs two *position vectors*, not two scalars.
//! - **`duration(<condition>)`** requires its one argument to literally be a `comparison`
//!   (`crate::expr::ast::Expr::Compare`) whose left side is a bare series (an
//!   `entity.<id>.<component>` reference or a `range(<a>, <b>)` call, no `@time`) and whose
//!   right side evaluates to a single scalar of the same unit (in the worked example, `100 m`).
//!   The condition is evaluated pointwise at the series' own native sample epochs, **zero-order
//!   hold from the left endpoint of each interval** (the same "hold until the next sample"
//!   convention `docs/adr/005-simulation-kernel.md` section 3 already uses for discrete state
//!   components, applied here to a derived boolean condition rather than a literal `StateSpace`
//!   component): `duration` is the sum of `t[i+1] - t[i]` over every interval whose *left*
//!   sample satisfies the comparison. This is a genuine numerical choice (a linear-interpolation
//!   crossing-time rule would give a different, sub-sample-accurate answer) -- picked for the
//!   same reason `min`/`max`/`mean`/`final`/`integral` above stick to native samples rather than
//!   a resampled grid: no interpolation contract exists for an arbitrary boolean condition, only
//!   for the `StateSpace` components the amendment's own section 3 already classifies.

use av_cdm::pb::Unit;

use crate::expr::ast::{BinOp, Expr, TimeSpec};
use crate::expr::error::ExprError;
use crate::expr::runproducts::{ExprRunProducts, Series};
use crate::expr::units::{add_sub_unit, mul_div_unit};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Value {
    pub number: f64,
    pub unit: Unit,
}

enum RefShape<'e> {
    Entity { id: &'e str, component: &'e str },
    Output { instance: &'e str, name: &'e str },
    EventEpoch { name: &'e str },
    EventKindOnly { kind: &'e str },
}

fn classify_ref<'e>(path: &'e [String], pos: usize) -> Result<RefShape<'e>, ExprError> {
    match path {
        [a, id, component] if a == "entity" => Ok(RefShape::Entity { id, component }),
        [a, instance, name] if a == "output" => Ok(RefShape::Output { instance, name }),
        [a, name, t] if a == "event" && t == "t" => Ok(RefShape::EventEpoch { name }),
        [a, kind] if a == "event" => Ok(RefShape::EventKindOnly { kind }),
        _ => Err(ExprError::UnknownReferenceShape { path: path.join("."), pos }),
    }
}

fn range_entity_id(e: &Expr) -> Result<&str, ExprError> {
    match e {
        Expr::Ref { path, .. } if path.len() == 1 => Ok(path[0].as_str()),
        _ => Err(ExprError::RangeArgumentMustBeEntityId { pos: e.pos() }),
    }
}

fn range_entity_ids(args: &[Expr], pos: usize) -> Result<(&str, &str), ExprError> {
    if args.len() != 2 {
        return Err(ExprError::WrongArity { name: "range".to_string(), expected: 2, got: args.len(), pos });
    }
    Ok((range_entity_id(&args[0])?, range_entity_id(&args[1])?))
}

fn eval_ref(path: &[String], pos: usize, run: &ExprRunProducts, allow_bare: bool) -> Result<Value, ExprError> {
    match classify_ref(path, pos)? {
        RefShape::Entity { .. } | RefShape::Output { .. } => {
            if allow_bare {
                Err(ExprError::AggregateArgumentMustBeBareReference { name: "(internal)".to_string(), pos })
            } else {
                Err(ExprError::BareReferenceOutsideAggregate { path: path.join("."), pos })
            }
        }
        RefShape::EventEpoch { name } => Ok(Value { number: run.event_epoch_seconds(name, pos)?, unit: Unit::Second }),
        RefShape::EventKindOnly { .. } => Err(ExprError::UnknownReferenceShape { path: path.join("."), pos }),
    }
}

fn eval_at(inner: &Expr, time: &TimeSpec, pos: usize, run: &ExprRunProducts) -> Result<Value, ExprError> {
    let tai_ns = run.resolve_time(time, pos)?;
    match inner {
        Expr::Ref { path, pos: ref_pos } => match classify_ref(path, *ref_pos)? {
            RefShape::Entity { id, component } => {
                let (number, unit) = run.entity_component_at(id, component, tai_ns, *ref_pos)?;
                Ok(Value { number, unit })
            }
            RefShape::Output { instance, name } => {
                let (number, unit) = run.output_at(instance, name, tai_ns, *ref_pos)?;
                Ok(Value { number, unit })
            }
            _ => Err(ExprError::UnknownReferenceShape { path: path.join("."), pos: *ref_pos }),
        },
        Expr::Call { name, args, pos: call_pos } if name == "range" => {
            let (a, b) = range_entity_ids(args, *call_pos)?;
            Ok(Value { number: run.range_at(a, b, tai_ns, *call_pos)?, unit: Unit::Meter })
        }
        _ => Err(ExprError::AtNotApplicable { pos }),
    }
}

/// The series form of a bare aggregate/`duration`-condition argument: an `entity`/`output`
/// reference, or a `range(<a>, <b>)` call. See the module doc comment.
fn series_of(expr: &Expr, run: &ExprRunProducts) -> Result<Series, ExprError> {
    match expr {
        Expr::Ref { path, pos } => match classify_ref(path, *pos)? {
            RefShape::Entity { id, component } => run.entity_component_series(id, component, *pos),
            RefShape::Output { instance, name } => run.output_series(instance, name, *pos).cloned(),
            _ => Err(ExprError::AggregateArgumentMustBeBareReference { name: "(internal)".to_string(), pos: *pos }),
        },
        Expr::Call { name, args, pos } if name == "range" => {
            let (a, b) = range_entity_ids(args, *pos)?;
            run.range_series(a, b, *pos)
        }
        _ => Err(ExprError::AggregateArgumentMustBeBareReference { name: "(internal)".to_string(), pos: expr.pos() }),
    }
}

fn trapezoidal_integral_seconds(series: &Series) -> f64 {
    let mut total = 0.0;
    for w in series.epochs_tai_ns.windows(2).zip(series.values.windows(2)) {
        let ((t0, t1), (v0, v1)) = ((w.0[0], w.0[1]), (w.1[0], w.1[1]));
        let dt_s = (t1 - t0) as f64 * 1e-9;
        total += 0.5 * (v0 + v1) * dt_s;
    }
    total
}

fn eval_aggregate(name: &str, args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Value, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: name.to_string(), expected: 1, got: args.len(), pos });
    }
    let series = series_of(&args[0], run).map_err(|e| match e {
        ExprError::AggregateArgumentMustBeBareReference { .. } => ExprError::AggregateArgumentMustBeBareReference { name: name.to_string(), pos },
        other => other,
    })?;
    if series.values.is_empty() {
        return Err(ExprError::EmptyTrajectory { id: format!("{args:?}"), pos });
    }
    let number = match name {
        "min" => series.values.iter().cloned().fold(f64::INFINITY, f64::min),
        "max" => series.values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        "mean" => series.values.iter().sum::<f64>() / series.values.len() as f64,
        "final" => *series.values.last().unwrap(),
        "integral" => trapezoidal_integral_seconds(&series),
        _ => unreachable!("caller only dispatches recognized aggregate names"),
    };
    let unit = if name == "integral" { mul_div_unit("*", series.unit, Unit::Second, pos)? } else { series.unit };
    Ok(Value { number, unit })
}

fn eval_count(args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Value, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: "count".to_string(), expected: 1, got: args.len(), pos });
    }
    let Expr::Ref { path, pos: ref_pos } = &args[0] else {
        return Err(ExprError::AggregateArgumentMustBeBareReference { name: "count".to_string(), pos });
    };
    let RefShape::EventKindOnly { kind } = classify_ref(path, *ref_pos)? else {
        return Err(ExprError::AggregateArgumentMustBeBareReference { name: "count".to_string(), pos });
    };
    Ok(Value { number: run.count_events_by_kind(kind, *ref_pos)?, unit: Unit::Dimensionless })
}

/// `duration(<condition>)` -- see the module doc comment for the zero-order-hold numerical
/// rule. `condition` must already have survived `crate::expr::typecheck::duration_unit`
/// (matching unit on both sides), but `evaluate` can be called without a prior `check()` (this
/// module's own tests do), so the unit is re-verified here defensively rather than assumed.
fn eval_duration(args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Value, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: "duration".to_string(), expected: 1, got: args.len(), pos });
    }
    let Expr::Compare { op, lhs, rhs, pos: cmp_pos } = &args[0] else {
        return Err(ExprError::DurationArgumentMustBeComparison { pos });
    };
    let series = series_of(lhs, run)?;
    let threshold = eval_inner(rhs, run, false)?;
    if series.unit != threshold.unit {
        return Err(ExprError::UnitMismatch { op: op.symbol(), left: series.unit, right: threshold.unit, pos: *cmp_pos });
    }
    if series.values.len() < 2 {
        return Err(ExprError::EmptyTrajectory { id: format!("{lhs:?}"), pos: *cmp_pos });
    }
    let mut total_s = 0.0;
    for i in 0..series.values.len() - 1 {
        if op.holds(series.values[i], threshold.number) {
            total_s += (series.epochs_tai_ns[i + 1] - series.epochs_tai_ns[i]) as f64 * 1e-9;
        }
    }
    Ok(Value { number: total_s, unit: Unit::Second })
}

fn eval_inner(expr: &Expr, run: &ExprRunProducts, allow_bare: bool) -> Result<Value, ExprError> {
    match expr {
        Expr::Number { value, unit, .. } => Ok(Value { number: *value, unit: *unit }),
        Expr::Neg { expr, .. } => {
            let v = eval_inner(expr, run, allow_bare)?;
            Ok(Value { number: -v.number, unit: v.unit })
        }
        Expr::Binary { op, lhs, rhs, pos } => {
            let l = eval_inner(lhs, run, allow_bare)?;
            let r = eval_inner(rhs, run, allow_bare)?;
            match op {
                BinOp::Add => Ok(Value { number: l.number + r.number, unit: add_sub_unit("+", l.unit, r.unit, *pos)? }),
                BinOp::Sub => Ok(Value { number: l.number - r.number, unit: add_sub_unit("-", l.unit, r.unit, *pos)? }),
                BinOp::Mul => Ok(Value { number: l.number * r.number, unit: mul_div_unit("*", l.unit, r.unit, *pos)? }),
                BinOp::Div => Ok(Value { number: l.number / r.number, unit: mul_div_unit("/", l.unit, r.unit, *pos)? }),
            }
        }
        // Reached only for a scalar-valued comparison (never `duration`'s own argument, which
        // `eval_duration` extracts and evaluates itself without recursing back through here --
        // see that function). `allow_bare` threads through unchanged, matching `typecheck`.
        Expr::Compare { op, lhs, rhs, pos: _ } => {
            let l = eval_inner(lhs, run, allow_bare)?;
            let r = eval_inner(rhs, run, allow_bare)?;
            Ok(Value { number: if op.holds(l.number, r.number) { 1.0 } else { 0.0 }, unit: Unit::Dimensionless })
        }
        Expr::At { expr, time, pos } => eval_at(expr, time, *pos, run),
        Expr::Ref { path, pos } => eval_ref(path, *pos, run, allow_bare),
        Expr::Call { name, args, pos } => match name.as_str() {
            "min" | "max" | "mean" | "final" | "integral" => eval_aggregate(name, args, *pos, run),
            "count" => eval_count(args, *pos, run),
            "duration" => eval_duration(args, *pos, run),
            "range" => {
                if allow_bare {
                    Err(ExprError::AggregateArgumentMustBeBareReference { name: "(internal)".to_string(), pos: *pos })
                } else {
                    Err(ExprError::BareReferenceOutsideAggregate { path: "range(...)".to_string(), pos: *pos })
                }
            }
            other => Err(ExprError::UnknownFunction { name: other.to_string(), pos: *pos }),
        },
    }
}

/// Evaluate `expr` against `run`. Callers wanting the "parse-time unit mismatch" property
/// should call [`crate::expr::typecheck::check`] first (see that module's doc comment) --
/// [`crate::expr::objective::evaluate_objective`]/`evaluate_moe` already do this, in that
/// order, so a unit problem is always reported before this function computes anything.
pub fn evaluate(expr: &Expr, run: &ExprRunProducts) -> Result<Value, ExprError> {
    eval_inner(expr, run, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parser::parse;
    use av_cdm::pb::{Trajectory, TrajectorySample};
    use std::collections::BTreeMap;

    fn straight_line_traj() -> BTreeMap<String, Trajectory> {
        // pos_x(t) = t (m), vel_x = 1 m/s constant, sampled every 1 s for 10 s.
        let samples: Vec<TrajectorySample> = (0..=10).map(|i| TrajectorySample { tai_ns: i * 1_000_000_000, mean: vec![i as f64, 0.0, 0.0, 1.0, 0.0, 0.0], ..Default::default() }).collect();
        let mut map = BTreeMap::new();
        map.insert(
            "veh".to_string(),
            Trajectory { id: "t".to_string(), entity_id: "veh".to_string(), state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(), samples, ..Default::default() },
        );
        map
    }

    /// A stationary entity 15 m ahead of `veh` on the x axis -- `range(veh, gs)` therefore
    /// grows from 15 m (t=0) past 100 m once `veh`'s own `pos_x` clears 85/115 m, giving
    /// `duration(range(veh, gs) < 100 m)` a hand-checkable closed-form answer.
    fn straight_line_and_stationary_traj() -> BTreeMap<String, Trajectory> {
        let mut map = straight_line_traj();
        let samples: Vec<TrajectorySample> = (0..=10).map(|i| TrajectorySample { tai_ns: i * 1_000_000_000, mean: vec![15.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() }).collect();
        map.insert(
            "gs".to_string(),
            Trajectory { id: "t2".to_string(), entity_id: "gs".to_string(), state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(), samples, ..Default::default() },
        );
        map
    }

    #[test]
    fn evaluates_arithmetic_with_unit_propagation() {
        let map: BTreeMap<String, Trajectory> = BTreeMap::new();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("2 m + 3 m * 2").unwrap();
        let v = evaluate(&e, &run).unwrap();
        assert_eq!(v, Value { number: 8.0, unit: Unit::Meter });
    }

    #[test]
    fn evaluates_entity_refs_and_aggregates_over_a_straight_line() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);

        assert_eq!(evaluate(&parse("entity.veh.pos_x@end").unwrap(), &run).unwrap(), Value { number: 10.0, unit: Unit::Meter });
        assert_eq!(evaluate(&parse("entity.veh.pos_x@start").unwrap(), &run).unwrap(), Value { number: 0.0, unit: Unit::Meter });
        assert_eq!(evaluate(&parse("final(entity.veh.pos_x)").unwrap(), &run).unwrap(), Value { number: 10.0, unit: Unit::Meter });
        assert_eq!(evaluate(&parse("max(entity.veh.pos_x)").unwrap(), &run).unwrap(), Value { number: 10.0, unit: Unit::Meter });
        assert_eq!(evaluate(&parse("min(entity.veh.pos_x)").unwrap(), &run).unwrap(), Value { number: 0.0, unit: Unit::Meter });
        assert_eq!(evaluate(&parse("mean(entity.veh.pos_x)").unwrap(), &run).unwrap(), Value { number: 5.0, unit: Unit::Meter });
        // integral(vel_x) dt over [0,10] at 1 m/s constant = 10 m, and unit composes to Meter.
        assert_eq!(evaluate(&parse("integral(entity.veh.vel_x)").unwrap(), &run).unwrap(), Value { number: 10.0, unit: Unit::Meter });
    }

    #[test]
    fn interpolates_at_a_time_between_native_samples() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let v = evaluate(&parse("entity.veh.pos_x@2.5 s").unwrap(), &run).unwrap();
        assert!((v.number - 2.5).abs() < 1e-9, "{v:?}");
        assert_eq!(v.unit, Unit::Meter);
    }

    #[test]
    fn a_bare_reference_outside_an_aggregate_is_a_typed_error_not_a_series_dump() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let err = evaluate(&parse("entity.veh.pos_x").unwrap(), &run).unwrap_err();
        assert!(matches!(err, ExprError::BareReferenceOutsideAggregate { .. }));
    }

    #[test]
    fn range_at_a_time_is_the_euclidean_distance_between_two_entities() {
        let map = straight_line_and_stationary_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        // veh.pos_x(0) = 0, gs.pos_x = 15 constant -> range(veh, gs)@start = 15 m.
        let v = evaluate(&parse("range(veh, gs)@start").unwrap(), &run).unwrap();
        assert_eq!(v, Value { number: 15.0, unit: Unit::Meter });
        // veh.pos_x(10) = 10 -> range@end = |10 - 15| = 5 m.
        let v2 = evaluate(&parse("range(veh, gs)@end").unwrap(), &run).unwrap();
        assert!((v2.number - 5.0).abs() < 1e-9, "{v2:?}");
        assert_eq!(v2.unit, Unit::Meter);
    }

    /// The amendment's own worked example: `duration(range(a, b) < 100 m)`. `veh.pos_x(t) = t`,
    /// `gs.pos_x = 15` constant, so `range(veh, gs)(t) = |t - 15|`, always `<= 15 < 100` over
    /// `t in [0, 10]` -- the condition holds at every sample, so the pinned answer is the whole
    /// 10 s window.
    #[test]
    fn duration_of_the_worked_example_over_a_condition_that_holds_throughout() {
        let map = straight_line_and_stationary_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let v = evaluate(&parse("duration(range(veh, gs) < 100 m)").unwrap(), &run).unwrap();
        assert!((v.number - 10.0).abs() < 1e-9, "{v:?}");
        assert_eq!(v.unit, Unit::Second);
    }

    /// A condition that holds for only part of the run: `entity.veh.pos_x < 5 m` is true for
    /// samples `t = 0..4` (pos_x = 0..4) and false from `t = 5` on -- zero-order hold from the
    /// left sample of each 1 s interval gives exactly 5 s (`t=0` through `t=4` each contribute
    /// their own 1 s interval to `t=5`).
    #[test]
    fn duration_of_a_condition_that_holds_for_part_of_the_run() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let v = evaluate(&parse("duration(entity.veh.pos_x < 5 m)").unwrap(), &run).unwrap();
        assert!((v.number - 5.0).abs() < 1e-9, "{v:?}");
    }

    #[test]
    fn duration_requires_a_comparison_argument() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let err = evaluate(&parse("duration(entity.veh.pos_x)").unwrap(), &run).unwrap_err();
        assert!(matches!(err, ExprError::DurationArgumentMustBeComparison { .. }), "{err:?}");
    }

    #[test]
    fn a_top_level_comparison_evaluates_to_0_or_1_dimensionless() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        assert_eq!(evaluate(&parse("entity.veh.pos_x@end < 100 m").unwrap(), &run).unwrap(), Value { number: 1.0, unit: Unit::Dimensionless });
        assert_eq!(evaluate(&parse("entity.veh.pos_x@end > 100 m").unwrap(), &run).unwrap(), Value { number: 0.0, unit: Unit::Dimensionless });
    }

    #[test]
    fn range_and_duration_are_now_recognized_functions_unlike_before_the_amendment() {
        // Before the amendment these were `UnknownFunction`; the amendment's whole point is
        // that they are real now. `range`/`duration` still refuse a malformed call, but with a
        // shape-specific error, never `UnknownFunction`.
        let map: BTreeMap<String, Trajectory> = BTreeMap::new();
        let run = ExprRunProducts::new(0, 10, &map, &[]);
        let err = evaluate(&parse("range(1, 2)").unwrap(), &run).unwrap_err();
        assert!(!matches!(err, ExprError::UnknownFunction { .. }), "{err:?}");
        let err2 = evaluate(&parse("duration(1)").unwrap(), &run).unwrap_err();
        assert!(matches!(err2, ExprError::DurationArgumentMustBeComparison { .. }), "{err2:?}");
    }
}

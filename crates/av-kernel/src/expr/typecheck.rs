//! Unit propagation as its own pass, run **before** [`crate::expr::eval::evaluate`] touches a
//! single sample value -- ADR-005 sec 6: "a unit mismatch is a parse-time error, not a runtime
//! surprise." [`check`] only ever calls `ExprRunProducts`'s `*_unit` accessors
//! (`entity_component_unit`, `output_unit`, `range_unit`) and checks event *existence* --
//! declared metadata (a `StateSpace`'s component labels/units, which events/outputs a run
//! declares), never `Trajectory.samples`' actual numbers or an output series' actual values. A
//! mismatch this pass finds is reported before any arithmetic on real numbers has happened,
//! deterministically and the same way on every run of the same expression against the same
//! run's declared shape. `crate::drm::executor::execute` runs this pass **at load, before any
//! propagation** (question 93 / `DrmError::InvalidExpression`), against a `ExprRunProducts`
//! built from every declared instance's shape alone (empty `samples`) -- exactly the property
//! this module's "never touches sample data" guarantees makes possible.
//!
//! Mirrors [`crate::expr::eval::evaluate`]'s structure node-for-node (see that module's doc
//! comment) but threads a [`av_cdm::pb::Unit`] instead of a [`crate::expr::eval::Value`] --
//! the two are kept as separate passes, not factored into one generic function, specifically so
//! this one's "never touches sample data" property is visible in its own code, not merely
//! asserted.
//!
//! ## `allow_bare`: where a series-valued reference is meaningful
//!
//! `entity.<id>.<component>` and `range(<a>, <b>)`, without a `postfix` `@time`, denote a whole
//! time series, not a scalar -- meaningful only as the direct argument of an aggregate
//! (`min`/`max`/`mean`/`final`/`integral`, `count`) or as one side of the `comparison` inside a
//! `duration(...)` condition. `allow_bare` threads that context down through `check_inner`;
//! `Expr::Compare`'s own arm passes its *own* `allow_bare` through to both operands unchanged
//! (rather than hard-coding `false`), which is what lets `duration(range(a, b) < 100 m)`'s
//! condition -- built by [`duration_unit`] calling `check_inner(&args[0], run, true)` -- resolve
//! its bare `range(a, b)` on the left while an ordinary top-level comparison (`allow_bare =
//! false`, from [`check`]) still refuses a bare series on either side.

use av_cdm::pb::Unit;

use crate::expr::ast::{BinOp, Expr, TimeSpec};
use crate::expr::error::ExprError;
use crate::expr::runproducts::ExprRunProducts;
use crate::expr::units::{add_sub_unit, mul_div_unit};

/// `ref := ident ('.' ident)*` shape recognized by this evaluator (see
/// `crate::expr::runproducts`'s module doc comment): `entity.<id>.<component>`,
/// `output.<instance>.<name>`, `event.<name>.t`, or (only valid inside `count(...)`)
/// `event.<kind>`.
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

/// `range(<a>, <b>)`'s two arguments: each must be a bare, single-segment `Expr::Ref` (a plain
/// entity id, e.g. `range(leo, gs)` -- not `entity.<id>.<component>`, which would be ambiguous
/// about which component names the distance).
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

/// Validate a `TimeSpec` exists (an `EventName` must actually name a known event) without
/// reading any trajectory sample.
fn check_time(time: &TimeSpec, run: &ExprRunProducts) -> Result<(), ExprError> {
    match time {
        TimeSpec::Start | TimeSpec::End | TimeSpec::OffsetSeconds { .. } => Ok(()),
        TimeSpec::EventName { name, pos } => run.event_epoch_seconds(name, *pos).map(|_| ()),
    }
}

/// `min`/`max`/`mean`/`final`/`integral`: exactly one argument, a bare `entity`/`output`
/// reference or a bare `range(<a>, <b>)` call, both series-valued (ADR-005 sec 6 prose:
/// `min(ref)`).
fn aggregate_unit(name: &str, args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Unit, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: name.to_string(), expected: 1, got: args.len(), pos });
    }
    let unit = match &args[0] {
        Expr::Ref { path, pos: ref_pos } => match classify_ref(path, *ref_pos)? {
            RefShape::Entity { id, component } => run.entity_component_unit(id, component, *ref_pos)?,
            RefShape::Output { instance, name: out_name } => run.output_unit(instance, out_name, *ref_pos)?,
            _ => return Err(ExprError::AggregateArgumentMustBeBareReference { name: name.to_string(), pos }),
        },
        Expr::Call { name: fname, args: fargs, pos: call_pos } if fname == "range" => {
            let (a, b) = range_entity_ids(fargs, *call_pos)?;
            run.range_unit(a, b, *call_pos)?
        }
        _ => return Err(ExprError::AggregateArgumentMustBeBareReference { name: name.to_string(), pos }),
    };
    if name == "integral" {
        mul_div_unit("*", unit, Unit::Second, pos)
    } else {
        Ok(unit)
    }
}

fn count_unit(args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Unit, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: "count".to_string(), expected: 1, got: args.len(), pos });
    }
    let Expr::Ref { path, pos: ref_pos } = &args[0] else {
        return Err(ExprError::AggregateArgumentMustBeBareReference { name: "count".to_string(), pos });
    };
    let RefShape::EventKindOnly { kind } = classify_ref(path, *ref_pos)? else {
        return Err(ExprError::AggregateArgumentMustBeBareReference { name: "count".to_string(), pos });
    };
    // Existence of the kind name is checked without touching any event instance data.
    run.count_events_by_kind(kind, *ref_pos)?;
    Ok(Unit::Dimensionless)
}

/// `duration(<condition>)`: the one argument must literally be a `comparison` node (the
/// amendment's own worked example, `duration(range(a, b) < 100 m)`) -- its unit equality is
/// already exactly what `Expr::Compare`'s own `check_inner` arm enforces, so this just requires
/// the shape and re-runs that same check with `allow_bare = true` (a bare series is meaningful
/// on either side of a `duration` condition, unlike an ordinary top-level comparison).
fn duration_unit(args: &[Expr], pos: usize, run: &ExprRunProducts) -> Result<Unit, ExprError> {
    if args.len() != 1 {
        return Err(ExprError::WrongArity { name: "duration".to_string(), expected: 1, got: args.len(), pos });
    }
    if !matches!(args[0], Expr::Compare { .. }) {
        return Err(ExprError::DurationArgumentMustBeComparison { pos });
    }
    check_inner(&args[0], run, true)?;
    Ok(Unit::Second)
}

/// `postfix := primary ['@' time]`: only an `entity.<id>.<component>`/`output.<instance>.<name>`
/// reference, or a `range(<a>, <b>)` call, has a time-indexed meaning.
fn check_at(inner: &Expr, time: &TimeSpec, pos: usize, run: &ExprRunProducts) -> Result<Unit, ExprError> {
    check_time(time, run)?;
    match inner {
        Expr::Ref { path, pos: ref_pos } => match classify_ref(path, *ref_pos)? {
            RefShape::Entity { id, component } => run.entity_component_unit(id, component, *ref_pos),
            RefShape::Output { instance, name } => run.output_unit(instance, name, *ref_pos),
            _ => Err(ExprError::UnknownReferenceShape { path: path.join("."), pos: *ref_pos }),
        },
        Expr::Call { name, args, pos: call_pos } if name == "range" => {
            let (a, b) = range_entity_ids(args, *call_pos)?;
            run.range_unit(a, b, *call_pos)
        }
        _ => Err(ExprError::AtNotApplicable { pos }),
    }
}

fn check_inner(expr: &Expr, run: &ExprRunProducts, allow_bare: bool) -> Result<Unit, ExprError> {
    match expr {
        Expr::Number { unit, .. } => Ok(*unit),
        Expr::Neg { expr, .. } => check_inner(expr, run, allow_bare),
        Expr::Binary { op, lhs, rhs, pos } => {
            let l = check_inner(lhs, run, allow_bare)?;
            let r = check_inner(rhs, run, allow_bare)?;
            match op {
                BinOp::Add => add_sub_unit("+", l, r, *pos),
                BinOp::Sub => add_sub_unit("-", l, r, *pos),
                BinOp::Mul => mul_div_unit("*", l, r, *pos),
                BinOp::Div => mul_div_unit("/", l, r, *pos),
            }
        }
        // Non-associative (the AST never nests a Compare inside a Compare -- see
        // `crate::expr::ast::CmpOp`'s doc comment); `allow_bare` threads through unchanged so a
        // `duration(...)` condition can compare a bare series against a scalar (see the module
        // doc comment) while an ordinary comparison stays scalar-only.
        Expr::Compare { op, lhs, rhs, pos } => {
            let l = check_inner(lhs, run, allow_bare)?;
            let r = check_inner(rhs, run, allow_bare)?;
            if l == r {
                Ok(Unit::Dimensionless)
            } else {
                Err(ExprError::UnitMismatch { op: op.symbol(), left: l, right: r, pos: *pos })
            }
        }
        Expr::At { expr, time, pos } => check_at(expr, time, *pos, run),
        Expr::Ref { path, pos } => match classify_ref(path, *pos)? {
            RefShape::Entity { id, component } => {
                if !allow_bare {
                    return Err(ExprError::BareReferenceOutsideAggregate { path: path.join("."), pos: *pos });
                }
                run.entity_component_unit(id, component, *pos)
            }
            RefShape::Output { instance, name } => {
                if !allow_bare {
                    return Err(ExprError::BareReferenceOutsideAggregate { path: path.join("."), pos: *pos });
                }
                run.output_unit(instance, name, *pos)
            }
            // `event.<name>.t` is already a scalar (an epoch), valid anywhere -- not gated by
            // `allow_bare`.
            RefShape::EventEpoch { name } => {
                run.event_epoch_seconds(name, *pos)?;
                Ok(Unit::Second)
            }
            RefShape::EventKindOnly { .. } => Err(ExprError::UnknownReferenceShape { path: path.join("."), pos: *pos }),
        },
        Expr::Call { name, args, pos } => match name.as_str() {
            "min" | "max" | "mean" | "final" | "integral" => aggregate_unit(name, args, *pos, run),
            "count" => count_unit(args, *pos, run),
            "duration" => duration_unit(args, *pos, run),
            "range" => {
                if !allow_bare {
                    return Err(ExprError::BareReferenceOutsideAggregate { path: "range(...)".to_string(), pos: *pos });
                }
                let (a, b) = range_entity_ids(args, *pos)?;
                run.range_unit(a, b, *pos)
            }
            other => Err(ExprError::UnknownFunction { name: other.to_string(), pos: *pos }),
        },
    }
}

/// Propagate units through `expr` against `run`'s declared shape (never its sample values).
/// The single, top-level `ref` is not allowed to be bare (`allow_bare = false`) -- only inside
/// an aggregate's own argument position, or a `duration(...)` condition, is a bare
/// series-valued reference meaningful.
pub fn check(expr: &Expr, run: &ExprRunProducts) -> Result<Unit, ExprError> {
    check_inner(expr, run, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parser::parse;
    use crate::expr::runproducts::ExprRunProducts;
    use av_cdm::pb::{Trajectory, TrajectorySample};
    use std::collections::BTreeMap;

    fn run_with_one_entity() -> BTreeMap<String, Trajectory> {
        let mut map = BTreeMap::new();
        map.insert(
            "veh".to_string(),
            Trajectory {
                id: "t".to_string(),
                entity_id: "veh".to_string(),
                state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(),
                samples: vec![
                    TrajectorySample { tai_ns: 0, mean: vec![0.0; 6], ..Default::default() },
                    TrajectorySample { tai_ns: 10_000_000_000, mean: vec![1.0; 6], ..Default::default() },
                ],
                ..Default::default()
            },
        );
        map
    }

    fn run_with_two_entities() -> BTreeMap<String, Trajectory> {
        let mut map = run_with_one_entity();
        map.insert(
            "gs".to_string(),
            Trajectory {
                id: "t2".to_string(),
                entity_id: "gs".to_string(),
                state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(),
                samples: vec![
                    TrajectorySample { tai_ns: 0, mean: vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() },
                    TrajectorySample { tai_ns: 10_000_000_000, mean: vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() },
                ],
                ..Default::default()
            },
        );
        map
    }

    #[test]
    fn a_unit_mismatch_is_caught_before_any_evaluation() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        // pos_x is meters, vel_x is m/s: adding them is a unit mismatch.
        let e = parse("entity.veh.pos_x@end + entity.veh.vel_x@end").unwrap();
        let err = check(&e, &run).unwrap_err();
        assert!(matches!(err, ExprError::UnitMismatch { left: Unit::Meter, right: Unit::MeterPerSecond, .. }), "{err:?}");
    }

    #[test]
    fn integral_of_a_velocity_is_a_distance() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("integral(entity.veh.vel_x)").unwrap();
        assert_eq!(check(&e, &run), Ok(Unit::Meter));
    }

    #[test]
    fn a_bare_reference_outside_an_aggregate_is_refused() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("entity.veh.pos_x").unwrap();
        assert!(matches!(check(&e, &run), Err(ExprError::BareReferenceOutsideAggregate { .. })));
    }

    #[test]
    fn scaling_by_a_dimensionless_literal_is_fine() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("entity.veh.pos_x@end / 2").unwrap();
        assert_eq!(check(&e, &run), Ok(Unit::Meter));
    }

    #[test]
    fn a_top_level_comparison_of_two_meter_quantities_is_dimensionless() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("entity.veh.pos_x@end < 100 m").unwrap();
        assert_eq!(check(&e, &run), Ok(Unit::Dimensionless));
    }

    #[test]
    fn a_comparison_across_mismatched_units_is_refused() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("entity.veh.pos_x@end < 100 s").unwrap();
        assert!(matches!(check(&e, &run), Err(ExprError::UnitMismatch { left: Unit::Meter, right: Unit::Second, .. })));
    }

    #[test]
    fn range_at_time_typechecks_to_meter() {
        let map = run_with_two_entities();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("range(veh, gs)@end").unwrap();
        assert_eq!(check(&e, &run), Ok(Unit::Meter));
    }

    #[test]
    fn duration_of_the_sec_6_worked_example_typechecks_to_seconds() {
        let map = run_with_two_entities();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("duration(range(veh, gs) < 100 m)").unwrap();
        assert_eq!(check(&e, &run), Ok(Unit::Second));
    }

    #[test]
    fn a_bare_range_outside_duration_or_an_aggregate_is_refused() {
        let map = run_with_two_entities();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("range(veh, gs)").unwrap();
        assert!(matches!(check(&e, &run), Err(ExprError::BareReferenceOutsideAggregate { .. })));
    }

    #[test]
    fn duration_requires_a_comparison_argument() {
        let map = run_with_two_entities();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("duration(range(veh, gs))").unwrap();
        assert!(matches!(check(&e, &run), Err(ExprError::DurationArgumentMustBeComparison { .. })));
    }

    #[test]
    fn at_time_on_something_other_than_a_reference_or_range_is_refused() {
        let map = run_with_one_entity();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let e = parse("(1 m)@end").unwrap();
        assert!(matches!(check(&e, &run), Err(ExprError::AtNotApplicable { .. })));
    }
}

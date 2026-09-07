//! Evaluate `Objective`/`MeasureOfEffectiveness` (`proto/altavista/v1/system.proto`) against a
//! run's products -- ADR-005 sec 6: "An `Objective` passes when `|value - target| <= tolerance`.
//! A `MeasureOfEffectiveness` evaluates to a value with a unit, recorded per run ...".
//!
//! Both entry points run the same three-stage pipeline [`crate::expr`]'s module doc comment
//! describes -- [`crate::expr::parser::parse`], then [`crate::expr::typecheck::check`], then
//! [`crate::expr::eval::evaluate`] -- so a unit problem is always caught before any arithmetic
//! on real sample values runs, never silently coerced.
//!
//! ## Declared `unit` vs. the expression's own propagated unit
//!
//! `Objective.unit`/`MeasureOfEffectiveness.unit` (`proto/altavista/v1/system.proto`) is
//! optional metadata, not an instruction to convert: this evaluator has no notion of "the same
//! quantity in a different unit" (`crate::expr::units`'s module doc comment -- the CDM `Unit`
//! enum is a closed set of already-SI units, not a dimension with multiple spellings). When
//! `unit` is declared (non-`UNIT_UNSPECIFIED`), it must therefore equal exactly what the
//! expression itself propagates to, checked at the same typecheck step as every other unit rule
//! -- [`crate::expr::error::ExprError::DeclaredUnitMismatch`] otherwise, before evaluation. When
//! `unit` is left `UNIT_UNSPECIFIED`, no such check is made and the result carries whatever unit
//! the expression itself propagates to.
//!
//! `Objective.target`/`.tolerance` are declared as bare `double`s with no unit field of their
//! own (`proto/altavista/v1/system.proto`): ADR-005 sec 6's pass rule, `|value - target| <=
//! tolerance`, is applied directly against the expression's own computed number, in the
//! expression's own propagated unit -- the same unit `Objective.unit` names when declared.

use av_cdm::pb::{MeasureOfEffectiveness, Objective, Unit};

use crate::expr::error::ExprError;
use crate::expr::eval::{evaluate, Value};
use crate::expr::parser::parse;
use crate::expr::runproducts::ExprRunProducts;
use crate::expr::typecheck::check;

/// One `Objective`'s evaluated result: the computed value and unit, the declared target and
/// tolerance carried through for the caller's own reporting, and the ADR-005 sec 6 pass rule
/// already applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectiveResult {
    pub value: f64,
    pub unit: Unit,
    pub target: f64,
    pub tolerance: f64,
    /// `|value - target| <= tolerance` (ADR-005 sec 6).
    pub pass: bool,
}

/// One `MeasureOfEffectiveness`'s evaluated result: "a value with a unit."
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoeResult {
    pub value: f64,
    pub unit: Unit,
}

fn declared_unit(raw: i32) -> Unit {
    Unit::try_from(raw).unwrap_or(Unit::Unspecified)
}

fn parse_check_eval(expression: &str, declared: Unit, run: &ExprRunProducts) -> Result<Value, ExprError> {
    let expr = parse(expression)?;
    let computed_unit = check(&expr, run)?;
    if declared != Unit::Unspecified && declared != computed_unit {
        return Err(ExprError::DeclaredUnitMismatch { declared, computed: computed_unit });
    }
    evaluate(&expr, run)
}

/// Parse, typecheck (unit-propagate) and evaluate `obj.expression` against `run`, then apply
/// ADR-005 sec 6's pass rule. A parse, unit or evaluation problem is returned as-is (the same
/// [`ExprError`] the three underlying stages produce) rather than folded into `pass = false` --
/// a malformed or unit-broken objective is a configuration error, not a failing run.
pub fn evaluate_objective(obj: &Objective, run: &ExprRunProducts) -> Result<ObjectiveResult, ExprError> {
    let unit = declared_unit(obj.unit);
    let v = parse_check_eval(&obj.expression, unit, run)?;
    let pass = (v.number - obj.target).abs() <= obj.tolerance;
    Ok(ObjectiveResult { value: v.number, unit: v.unit, target: obj.target, tolerance: obj.tolerance, pass })
}

/// Parse, typecheck and evaluate `moe.expression` against `run`.
pub fn evaluate_moe(moe: &MeasureOfEffectiveness, run: &ExprRunProducts) -> Result<MoeResult, ExprError> {
    let unit = declared_unit(moe.unit);
    let v = parse_check_eval(&moe.expression, unit, run)?;
    Ok(MoeResult { value: v.number, unit: v.unit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{Trajectory, TrajectorySample};
    use std::collections::BTreeMap;

    fn straight_line_traj() -> BTreeMap<String, Trajectory> {
        // pos_x(t) = t (m), sampled every 1 s for 10 s.
        let samples: Vec<TrajectorySample> = (0..=10).map(|i| TrajectorySample { tai_ns: i * 1_000_000_000, mean: vec![i as f64, 0.0, 0.0, 1.0, 0.0, 0.0], ..Default::default() }).collect();
        let mut map = BTreeMap::new();
        map.insert(
            "veh".to_string(),
            Trajectory { id: "t".to_string(), entity_id: "veh".to_string(), state_space_id: crate::trajectory::CARTESIAN_POS_VEL_6_ID.to_string(), samples, ..Default::default() },
        );
        map
    }

    #[test]
    fn objective_passes_within_tolerance() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let obj = Objective { name: "final_x".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 10.0, tolerance: 0.5, unit: Unit::Meter as i32 };
        let result = evaluate_objective(&obj, &run).unwrap();
        assert_eq!(result.value, 10.0);
        assert_eq!(result.unit, Unit::Meter);
        assert!(result.pass);
    }

    #[test]
    fn objective_fails_outside_tolerance() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let obj = Objective { name: "final_x".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 5.0, tolerance: 0.5, unit: Unit::Unspecified as i32 };
        let result = evaluate_objective(&obj, &run).unwrap();
        assert_eq!(result.value, 10.0);
        assert!(!result.pass);
    }

    #[test]
    fn objective_with_a_wrong_declared_unit_is_a_typed_error_before_evaluation() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let obj = Objective { name: "final_x".to_string(), expression: "entity.veh.pos_x@end".to_string(), target: 10.0, tolerance: 0.5, unit: Unit::Second as i32 };
        let err = evaluate_objective(&obj, &run).unwrap_err();
        assert!(matches!(err, ExprError::DeclaredUnitMismatch { declared: Unit::Second, computed: Unit::Meter }), "{err:?}");
    }

    #[test]
    fn moe_evaluates_to_a_value_with_a_unit() {
        let map = straight_line_traj();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let moe = MeasureOfEffectiveness { name: "mean_x".to_string(), expression: "mean(entity.veh.pos_x)".to_string(), unit: Unit::Meter as i32 };
        let result = evaluate_moe(&moe, &run).unwrap();
        assert_eq!(result, MoeResult { value: 5.0, unit: Unit::Meter });
    }

    #[test]
    fn a_malformed_expression_is_returned_as_a_parse_error_not_folded_into_pass_false() {
        let map: BTreeMap<String, Trajectory> = BTreeMap::new();
        let run = ExprRunProducts::new(0, 10_000_000_000, &map, &[]);
        let obj = Objective { name: "bad".to_string(), expression: "1 +".to_string(), target: 0.0, tolerance: 1.0, unit: Unit::Unspecified as i32 };
        assert!(matches!(evaluate_objective(&obj, &run), Err(ExprError::UnexpectedEof { .. })));
    }
}

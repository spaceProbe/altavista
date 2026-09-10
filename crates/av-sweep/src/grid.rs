//! Grid expansion for a `ParameterSweep` (F1a, `docs/feasibility-plan.md`): turning
//! `ParameterSweep.axes` into the ordered list of concrete grid points a study runs, pinned by
//! hand rather than left to whatever a generic cartesian-product implementation happens to
//! produce. See [`expand_grid`]'s own doc comment for the exact, decided semantics.
//!
//! ## Question 192(c): two kinds of axis target, one flat key space
//!
//! An axis targets EITHER a system instance parameter (`instance` + `parameter`) OR a scenario
//! event value (`event_id` + `value_key`, e.g. a maneuver's `dv_x`) -- never both, never neither
//! ([`validate_axis_target`]). Both kinds still land in the same `SweepSample.axis_values`
//! `map<string, double>` (`run.proto` was not given a second field for this -- this crate's hard
//! boundary forbids editing `proto/**`), so the two key *forms* must never collide:
//!
//! - A parameter axis' key is `"{instance}.{parameter}"`, unchanged since F1a.
//! - An event axis' key is `"{EVENT_AXIS_KEY_PREFIX}{event_id}.{value_key}"` --
//!   `"event:{event_id}.{value_key}"`.
//!
//! **Why a prefix check at validation time, not just "pick a separator", closes this for real.**
//! `crates/av-kernel/src/drm/schema.rs`'s `RawSystemInstance.name` (and every `Parameter.name`)
//! is parsed as a bare `String` with no character restriction at all (grepped: no regex, no
//! `is_ascii_*` gate, nothing under `crates/av-kernel/src/drm/` validates instance or parameter
//! name *characters* at load time; every real name in `drms/*.yaml` today happens to use only
//! `[a-zA-Z0-9_.]`, but that is convention, not an enforced constraint this crate may rely on).
//! Since `instance` and `parameter` are therefore unrestricted, NO choice of separator is safe
//! merely by "a real name would never contain it" -- an adversarial (or just unlucky) instance
//! literally named e.g. `"event:demo_flt"` with parameter `"spacecraft.DragArea"` produces the
//! parameter-axis key `"event:demo_flt.spacecraft.DragArea"`, which *does* start with
//! `"event:"` and would collide with the event-axis key namespace by pure string equality if an
//! event happened to be named `demo_flt` with `value_key` `spacecraft.DragArea`.
//!
//! [`validate_axis_target`] therefore does not merely pick a prefix and hope -- it REFUSES
//! ([`SweepError::ReservedAxisKeyPrefix`]) any parameter axis whose own key would start with
//! `EVENT_AXIS_KEY_PREFIX`, at validation time (called from both `crate::schema::parse_sweep_yaml`
//! and [`expand_grid`], so this holds for a YAML-authored sweep and a programmatically-built
//! `pb::ParameterSweep` alike). After that refusal, the two key sets are disjoint by
//! construction, independent of what any instance or parameter is actually named: every accepted
//! event-axis key starts with `EVENT_AXIS_KEY_PREFIX` (built that way, unconditionally); every
//! accepted parameter-axis key is proven NOT to (the one case where it would, is refused). See
//! `tests::an_adversarially_named_instance_cannot_collide_with_the_event_axis_key_namespace` for
//! the adversarial case above, made concrete, and `crates/av-sweep/REPORT.md` for the same
//! analysis written out for a reader who has not read this module's source.

use std::collections::{BTreeMap, HashSet};

use av_cdm::pb;

use crate::error::SweepError;

/// The reserved prefix every event-axis key starts with -- see the module doc comment's
/// "Question 192(c)" section for why a parameter axis whose own key would start with this is
/// refused rather than merely assumed not to happen.
pub const EVENT_AXIS_KEY_PREFIX: &str = "event:";

/// What one axis targets -- exactly one of these two shapes, enforced by
/// [`validate_axis_target`], never bare `instance`/`parameter` fields on [`AxisValue`] itself (a
/// third, ambiguous shape that could not distinguish "this axis is a parameter axis with a
/// forgotten event target" from "this axis is an event axis with a forgotten parameter target").
#[derive(Debug, Clone, PartialEq)]
pub enum AxisTarget {
    Parameter { instance: String, parameter: String },
    Event { event_id: String, value_key: String },
}

impl AxisTarget {
    /// This target's own `SweepSample.axis_values` key -- see the module doc comment for the two
    /// key forms and the collision analysis.
    pub fn key(&self) -> String {
        match self {
            AxisTarget::Parameter { instance, parameter } => format!("{instance}.{parameter}"),
            AxisTarget::Event { event_id, value_key } => format!("{EVENT_AXIS_KEY_PREFIX}{event_id}.{value_key}"),
        }
    }
}

/// One axis's value at one grid point: the target it applies to (a `SystemInstance`/`Parameter`
/// or a scenario event's value key, question 192(c)) and the value to apply there.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisValue {
    pub target: AxisTarget,
    pub value: f64,
}

impl AxisValue {
    /// This axis value's own `SweepSample.axis_values` key -- delegates to
    /// [`AxisTarget::key`].
    pub fn key(&self) -> String {
        self.target.key()
    }
}

/// One point in the expanded grid: `point_index` is this point's position in [`expand_grid`]'s
/// returned `Vec` (row-major, last declared axis fastest -- see that function's doc comment),
/// and `values` carries one [`AxisValue`] per declared axis, in `ParameterSweep.axes`'
/// declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct GridPoint {
    pub point_index: u32,
    pub values: Vec<AxisValue>,
}

impl GridPoint {
    /// The `map<string, double>` `SweepSample.axis_values` wants: one entry per axis, keyed by
    /// [`AxisValue::key`] (either `"{instance}.{parameter}"` or
    /// `"{EVENT_AXIS_KEY_PREFIX}{event_id}.{value_key}"`). A `BTreeMap` (not `HashMap`) so two
    /// callers building this map from the same `GridPoint` always iterate/serialize it in the
    /// same order.
    pub fn axis_values_map(&self) -> BTreeMap<String, f64> {
        self.values.iter().map(|v| (v.key(), v.value)).collect()
    }
}

/// Which target `axis` names, WITHOUT validating it -- callers run [`validate_axis_target`]
/// first (it is what establishes that exactly one branch below applies); this only picks the
/// branch [`validate_axis_target`] proved consistent.
fn axis_target(axis: &pb::SweepAxis) -> AxisTarget {
    if !axis.event_id.is_empty() || !axis.value_key.is_empty() {
        AxisTarget::Event { event_id: axis.event_id.clone(), value_key: axis.value_key.clone() }
    } else {
        AxisTarget::Parameter { instance: axis.instance.clone(), parameter: axis.parameter.clone() }
    }
}

/// Question 192(c)'s "exactly one target per axis" rule, plus the reserved-key-prefix rule that
/// keeps a parameter axis' key from ever colliding with an event axis' key -- see the module doc
/// comment for the full analysis. Called from both `crate::schema::parse_sweep_yaml` (so a bad
/// YAML axis is refused "at load") and [`expand_grid`] (so a programmatically-built
/// `pb::ParameterSweep`, never having gone through YAML parsing at all, cannot slip an invalid
/// axis past this rule either) -- deliberately ONE function both call, never two copies of the
/// same check that could drift apart.
pub fn validate_axis_target(axis: &pb::SweepAxis) -> Result<(), SweepError> {
    // F2b manager review defect: the "both targets" test must be on ANY field of each target,
    // not on a fully-declared one. Checking `has_param && has_event` (both COMPLETE) let an axis
    // carrying a complete parameter target plus a HALF-declared event target -- e.g.
    // `instance`+`parameter`+`event_id`, `value_key` forgotten -- through as a plain parameter
    // axis with `event_id` silently dropped, which is exactly the "a typo must never silently
    // sweep something else" failure `UndeclaredParameter`/`UndeclaredEventValueKey` exist to
    // prevent, one level up. Any field of the other target being present now makes the axis
    // ambiguous, whether that target is complete or not; the error carries all four fields, so
    // the message names the half-declared one.
    let param_any = !axis.instance.is_empty() || !axis.parameter.is_empty();
    let event_any = !axis.event_id.is_empty() || !axis.value_key.is_empty();
    if param_any && event_any {
        return Err(SweepError::AxisBothTargets {
            instance: axis.instance.clone(),
            parameter: axis.parameter.clone(),
            event_id: axis.event_id.clone(),
            value_key: axis.value_key.clone(),
        });
    }

    let has_param = !axis.instance.is_empty() && !axis.parameter.is_empty();
    let has_event = !axis.event_id.is_empty() && !axis.value_key.is_empty();
    if has_param {
        let key = format!("{}.{}", axis.instance, axis.parameter);
        if key.starts_with(EVENT_AXIS_KEY_PREFIX) {
            return Err(SweepError::ReservedAxisKeyPrefix { instance: axis.instance.clone(), parameter: axis.parameter.clone(), prefix: EVENT_AXIS_KEY_PREFIX });
        }
        return Ok(());
    }
    if has_event {
        return Ok(());
    }
    // Nothing at all, or exactly one field of exactly one target: a target was intended but not
    // finished. `param_any && event_any` above already caught the cross-target case.
    Err(SweepError::AxisMissingTarget {
        instance: axis.instance.clone(),
        parameter: axis.parameter.clone(),
        event_id: axis.event_id.clone(),
        value_key: axis.value_key.clone(),
    })
}

/// Expand `sweep.axes` into the study's full grid. Semantics (decided, not open -- see
/// `docs/feasibility-plan.md`'s F1 milestone and this crate's own task brief):
///
/// - **Explicit values.** If `axis.values` is non-empty, use it verbatim. Refuses
///   ([`SweepError::AmbiguousAxisDeclaration`]) if `min`/`max`/`steps` are *also* non-zero --
///   an ambiguous declaration (both an explicit list and a range) is never silently resolved
///   by preferring one over the other.
/// - **A range.** If `axis.values` is empty: requires `steps >= 2`
///   ([`SweepError::AxisStepsBelowMinimum`] otherwise -- this is also what an axis declaring
///   neither `values` nor a usable range looks like, since `steps` then defaults to `0`) and
///   `max > min` ([`SweepError::AxisRangeNotIncreasing`] otherwise); value *i* =
///   `min + i*(max-min)/(steps-1)` for `i in 0..steps`, with the **last value forced to exactly
///   `max`** (never a rounded near-miss from the division above).
/// - **Exactly one target per axis** (question 192(c)): [`validate_axis_target`] is run on
///   every axis before anything else below, so a malformed or ambiguous target (or a parameter
///   axis whose key would collide with the reserved event-axis-key namespace) is refused before
///   this function does any other work.
/// - **Duplicate axes.** Two axes naming the same target (`instance`+`parameter`, OR
///   `event_id`+`value_key`) is refused ([`SweepError::DuplicateAxis`]) -- keyed on
///   [`AxisTarget::key`], so a parameter axis and an event axis are never confused with each
///   other by this check either (their keys are disjoint, see the module doc comment).
/// - **Ordering.** Row-major, with the **last declared axis varying fastest** -- axes
///   `A=[1,2]`, `B=[10,20,30]` (in that declaration order) produce points, in `point_index`
///   order: `(1,10),(1,20),(1,30),(2,10),(2,20),(2,30)`.
/// - **Zero axes is legal.** A sweep with no axes at all (a pure Monte Carlo study over
///   `monte_carlo_draws` alone) expands to exactly one point with an empty `values` list.
pub fn expand_grid(sweep: &pb::ParameterSweep) -> Result<Vec<GridPoint>, SweepError> {
    let mut seen = HashSet::new();
    for axis in &sweep.axes {
        validate_axis_target(axis)?;
        let key = axis_target(axis).key();
        if !seen.insert(key.clone()) {
            return Err(SweepError::DuplicateAxis { key });
        }
    }

    // One Vec<f64> of concrete values per declared axis, in declaration order.
    let mut axis_value_lists: Vec<(&pb::SweepAxis, Vec<f64>)> = Vec::with_capacity(sweep.axes.len());
    for axis in &sweep.axes {
        // `validate_axis_target(axis)` already ran, above, for every axis in `sweep.axes`
        // (including this one) before this loop starts -- so by this point `axis` is known to
        // have exactly one well-formed target, and `axis_target(axis)` below is safe to call
        // unconditionally: it just picks the one branch `validate_axis_target` already proved
        // consistent, per its own doc comment.
        let values = if !axis.values.is_empty() {
            if axis.min != 0.0 || axis.max != 0.0 || axis.steps != 0 {
                return Err(SweepError::AmbiguousAxisDeclaration { target: axis_target(axis).key() });
            }
            axis.values.clone()
        } else {
            if axis.steps < 2 {
                return Err(SweepError::AxisStepsBelowMinimum { target: axis_target(axis).key(), steps: axis.steps });
            }
            // `!(axis.max > axis.min)` is refused by clippy's `neg_cmp_op_on_partial_ord`
            // (`f64` is only `PartialOrd`, not `Ord` -- NaN makes it incomparable, and the lint
            // wants that made explicit rather than expressed as a negated `>`): written via
            // `partial_cmp` instead, which also correctly refuses a NaN `min`/`max` (its
            // `partial_cmp` is `None`, so this is never `Some(Greater)` either).
            if axis.max.partial_cmp(&axis.min) != Some(std::cmp::Ordering::Greater) {
                return Err(SweepError::AxisRangeNotIncreasing { target: axis_target(axis).key(), min: axis.min, max: axis.max });
            }
            let steps = axis.steps;
            let span = axis.max - axis.min;
            (0..steps)
                .map(|i| if i == steps - 1 { axis.max } else { axis.min + (i as f64) * span / ((steps - 1) as f64) })
                .collect()
        };
        axis_value_lists.push((axis, values));
    }

    // Row-major expansion, last declared axis fastest: iterating axes in declaration order,
    // outer loop over the combinations built so far, inner loop over this axis's own values,
    // produces exactly that ordering (verified by hand in
    // `tests::two_axes_expand_row_major_with_the_last_axis_fastest`). Also handles the
    // zero-axes case correctly with no special casing: the fold starts from `vec![vec![]]`
    // (one empty combination) and, with no axes to fold over, stays exactly that.
    let mut combos: Vec<Vec<f64>> = vec![vec![]];
    for (_, values) in &axis_value_lists {
        let mut next = Vec::with_capacity(combos.len() * values.len());
        for combo in &combos {
            for v in values {
                let mut c = combo.clone();
                c.push(*v);
                next.push(c);
            }
        }
        combos = next;
    }

    let points = combos
        .into_iter()
        .enumerate()
        .map(|(point_index, combo)| {
            let values = combo
                .into_iter()
                .zip(axis_value_lists.iter())
                .map(|(value, (axis, _))| AxisValue { target: axis_target(axis), value })
                .collect();
            GridPoint { point_index: point_index as u32, values }
        })
        .collect();
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(instance: &str, parameter: &str) -> pb::SweepAxis {
        pb::SweepAxis { instance: instance.to_string(), parameter: parameter.to_string(), ..Default::default() }
    }

    #[test]
    fn explicit_values_expand_in_declared_order() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { values: vec![5.0, 1.0, 9.0], ..axis("leo", "cd") }], ..Default::default() };
        let points = expand_grid(&sweep).expect("expands");
        let values: Vec<f64> = points.iter().map(|p| p.values[0].value).collect();
        assert_eq!(values, vec![5.0, 1.0, 9.0], "declared order, not sorted");
        assert_eq!(points.iter().map(|p| p.point_index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn two_axes_expand_row_major_with_the_last_axis_fastest() {
        let sweep = pb::ParameterSweep {
            axes: vec![
                pb::SweepAxis { values: vec![1.0, 2.0], ..axis("a_inst", "a_param") },
                pb::SweepAxis { values: vec![10.0, 20.0, 30.0], ..axis("b_inst", "b_param") },
            ],
            ..Default::default()
        };
        let points = expand_grid(&sweep).expect("expands");
        let pairs: Vec<(f64, f64)> = points.iter().map(|p| (p.values[0].value, p.values[1].value)).collect();
        assert_eq!(pairs, vec![(1.0, 10.0), (1.0, 20.0), (1.0, 30.0), (2.0, 10.0), (2.0, 20.0), (2.0, 30.0)], "row-major, last axis (b) fastest");
        assert_eq!(points.iter().map(|p| p.point_index).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_range_expands_to_steps_values_with_an_exact_max_endpoint() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 0.0, max: 1.0, steps: 3, ..axis("leo", "cd") }], ..Default::default() };
        let points = expand_grid(&sweep).expect("expands");
        let values: Vec<f64> = points.iter().map(|p| p.values[0].value).collect();
        assert_eq!(values, vec![0.0, 0.5, 1.0]);
        assert_eq!(*values.last().unwrap(), 1.0, "last value must be exactly max, never a rounded near-miss");

        // A min/max/steps triple where `min + (steps-1)*(max-min)/(steps-1)` -- the *unforced*
        // formula -- does NOT land exactly on `max` in f64 arithmetic (found by brute-force
        // search over random triples; see REPORT.md for the search script): computed here is
        // -14.463000000000001, one ULP-scale off -14.463. This is the genuine regression test
        // for "never a rounded near-miss" -- the steps=3, 0.0..1.0 case above is too clean an
        // input to exercise it at all (`(steps-1)*(max-min)/(steps-1)` collapses to an exact
        // `1.0` for those particular numbers regardless of whether the endpoint is forced).
        let sweep2 = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: -56.68, max: -14.463, steps: 3, ..axis("leo", "cd") }], ..Default::default() };
        let points2 = expand_grid(&sweep2).expect("expands");
        assert_eq!(points2.last().unwrap().values[0].value, -14.463, "forced exact max endpoint even when the unforced formula would round to a near-miss");
    }

    #[test]
    fn zero_axes_is_a_single_empty_point() {
        let sweep = pb::ParameterSweep { axes: vec![], monte_carlo_draws: 5, ..Default::default() };
        let points = expand_grid(&sweep).expect("expands");
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].point_index, 0);
        assert!(points[0].values.is_empty());
        assert!(points[0].axis_values_map().is_empty());
    }

    #[test]
    fn refuses_values_and_a_range_declared_together() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { values: vec![1.0], min: 0.0, max: 5.0, steps: 3, ..axis("leo", "cd") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AmbiguousAxisDeclaration { ref target } if target == "leo.cd"), "{err:?}");
        assert!(err.to_string().contains("axis on leo.cd:"), "{err}");
    }

    #[test]
    fn refuses_steps_below_two() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 0.0, max: 5.0, steps: 1, ..axis("leo", "cd") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisStepsBelowMinimum { ref target, steps: 1 } if target == "leo.cd"), "{err:?}");
        assert!(err.to_string().contains("axis on leo.cd:"), "{err}");

        // An axis declaring neither `values` nor a usable range (everything left at its proto3
        // zero default) is the same refusal: steps defaults to 0, which is < 2.
        let empty_sweep = pb::ParameterSweep { axes: vec![axis("leo", "cd")], ..Default::default() };
        let err2 = expand_grid(&empty_sweep).unwrap_err();
        assert!(matches!(err2, SweepError::AxisStepsBelowMinimum { steps: 0, .. }), "{err2:?}");
    }

    #[test]
    fn refuses_a_max_not_greater_than_min() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 5.0, max: 5.0, steps: 3, ..axis("leo", "cd") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisRangeNotIncreasing { ref target, min, max } if target == "leo.cd" && min == 5.0 && max == 5.0), "{err:?}");
        assert!(err.to_string().contains("axis on leo.cd:"), "{err}");

        let inverted = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 5.0, max: 1.0, steps: 3, ..axis("leo", "cd") }], ..Default::default() };
        assert!(matches!(expand_grid(&inverted).unwrap_err(), SweepError::AxisRangeNotIncreasing { .. }));
    }

    #[test]
    fn refuses_duplicate_axes() {
        let sweep = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { values: vec![1.0], ..axis("leo", "cd") }, pb::SweepAxis { values: vec![2.0], ..axis("leo", "cd") }],
            ..Default::default()
        };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::DuplicateAxis { ref key } if key == "leo.cd"), "{err:?}");
    }

    fn event_axis(event_id: &str, value_key: &str) -> pb::SweepAxis {
        pb::SweepAxis { event_id: event_id.to_string(), value_key: value_key.to_string(), ..Default::default() }
    }

    /// Question 192(c): an axis declaring neither a full parameter target nor a full event
    /// target (every field left at its proto3 zero default) is refused, naming what was actually
    /// given (all empty) rather than a generic "invalid axis".
    #[test]
    fn refuses_an_axis_with_no_target_at_all() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis::default()], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(
            matches!(err, SweepError::AxisMissingTarget { ref instance, ref parameter, ref event_id, ref value_key } if instance.is_empty() && parameter.is_empty() && event_id.is_empty() && value_key.is_empty()),
            "{err:?}"
        );
    }

    /// A partially-declared parameter target (instance set, parameter left empty) is also
    /// "missing", not silently treated as a usable axis -- proof `validate_axis_target` checks
    /// BOTH fields of a target, not just "is either field non-empty".
    #[test]
    fn refuses_a_partially_declared_parameter_target() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { instance: "leo".to_string(), values: vec![1.0], ..Default::default() }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisMissingTarget { ref instance, .. } if instance == "leo"), "{err:?}");
    }

    /// Question 192(c): an axis declaring BOTH a full parameter target and a full event target
    /// at once is ambiguous and refused, never silently resolved by preferring one.
    #[test]
    fn refuses_an_axis_declaring_both_targets_at_once() {
        let sweep = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { instance: "leo".to_string(), parameter: "cd".to_string(), event_id: "burn1".to_string(), value_key: "dv_x".to_string(), values: vec![1.0], ..Default::default() }],
            ..Default::default()
        };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(
            matches!(err, SweepError::AxisBothTargets { ref instance, ref parameter, ref event_id, ref value_key }
                if instance == "leo" && parameter == "cd" && event_id == "burn1" && value_key == "dv_x"),
            "{err:?}"
        );
    }

    /// F2b manager review defect: a COMPLETE parameter target alongside a HALF-declared event
    /// target (and the mirror case) is ambiguous too, and must be refused rather than silently
    /// resolved by dropping the incomplete half. Fails against the first F2b implementation,
    /// which tested `has_param && has_event` on two COMPLETE targets: it accepted both axes
    /// below as ordinary axes, silently discarding the half-declared other target -- the exact
    /// "a typo must never silently sweep something else" failure the surrounding refusals exist
    /// to prevent.
    #[test]
    fn refuses_an_axis_mixing_a_complete_target_with_a_half_declared_other_target() {
        // Complete parameter target + `event_id` present, `value_key` forgotten.
        let param_plus_half_event = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { instance: "leo".to_string(), parameter: "cd".to_string(), event_id: "burn1".to_string(), values: vec![1.0], ..Default::default() }],
            ..Default::default()
        };
        let err = expand_grid(&param_plus_half_event).unwrap_err();
        assert!(
            matches!(err, SweepError::AxisBothTargets { ref instance, ref parameter, ref event_id, ref value_key }
                if instance == "leo" && parameter == "cd" && event_id == "burn1" && value_key.is_empty()),
            "{err:?}"
        );

        // The mirror: complete event target + a stray `instance`, `parameter` forgotten.
        let event_plus_half_param = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { instance: "leo".to_string(), values: vec![1.0], ..event_axis("burn1", "dv_x") }],
            ..Default::default()
        };
        let err2 = expand_grid(&event_plus_half_param).unwrap_err();
        assert!(
            matches!(err2, SweepError::AxisBothTargets { ref instance, ref parameter, ref event_id, ref value_key }
                if instance == "leo" && parameter.is_empty() && event_id == "burn1" && value_key == "dv_x"),
            "{err2:?}"
        );
    }

    /// Question 195 follow-up: `AmbiguousAxisDeclaration`'s message must name an EVENT axis'
    /// real target too, not the empty `instance`/`parameter` it doesn't have. A test that only
    /// `matches!`-checked the variant shape would still pass against the unfixed implementation
    /// (which always raised with `axis.instance.clone()`/`axis.parameter.clone()`, both empty
    /// strings for an event axis) -- asserting on the rendered `Display` string is the only way
    /// to actually pin "the message names the real cause".
    #[test]
    fn ambiguous_axis_declaration_names_an_event_target() {
        let sweep = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { values: vec![1.0], min: 0.0, max: 5.0, steps: 3, ..event_axis("burn1", "dv_x") }],
            ..Default::default()
        };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AmbiguousAxisDeclaration { ref target } if target == "event:burn1.dv_x"), "{err:?}");
        let rendered = err.to_string();
        assert!(rendered.contains("event:burn1.dv_x"), "message must name the event target: {rendered}");
        assert!(!rendered.contains("axis on ."), "message must not fall back to the empty-target artefact: {rendered}");
    }

    /// Question 195 follow-up: `AxisStepsBelowMinimum` on an event axis -- see
    /// `ambiguous_axis_declaration_names_an_event_target`'s doc comment for why the assertion
    /// must be on the rendered message, not only the variant shape.
    #[test]
    fn axis_steps_below_minimum_names_an_event_target() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 0.0, max: 5.0, steps: 1, ..event_axis("burn1", "dv_x") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisStepsBelowMinimum { ref target, steps: 1 } if target == "event:burn1.dv_x"), "{err:?}");
        let rendered = err.to_string();
        assert!(rendered.contains("event:burn1.dv_x"), "message must name the event target: {rendered}");
        assert!(!rendered.contains("axis on ."), "message must not fall back to the empty-target artefact: {rendered}");
    }

    /// Question 195 follow-up: `AxisRangeNotIncreasing` on an event axis -- see
    /// `ambiguous_axis_declaration_names_an_event_target`'s doc comment for why the assertion
    /// must be on the rendered message, not only the variant shape.
    #[test]
    fn axis_range_not_increasing_names_an_event_target() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 5.0, max: 5.0, steps: 3, ..event_axis("burn1", "dv_x") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisRangeNotIncreasing { ref target, min, max } if target == "event:burn1.dv_x" && min == 5.0 && max == 5.0), "{err:?}");
        let rendered = err.to_string();
        assert!(rendered.contains("event:burn1.dv_x"), "message must name the event target: {rendered}");
        assert!(!rendered.contains("axis on ."), "message must not fall back to the empty-target artefact: {rendered}");
    }

    /// An event axis (`event_id` + `value_key`) expands exactly like a parameter axis over its
    /// own explicit values -- same grid machinery, a different target.
    #[test]
    fn an_event_axis_expands_over_explicit_values() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { values: vec![15.0, 25.0], ..event_axis("burn1", "dv_x") }], ..Default::default() };
        let points = expand_grid(&sweep).expect("expands");
        let values: Vec<f64> = points.iter().map(|p| p.values[0].value).collect();
        assert_eq!(values, vec![15.0, 25.0]);
        assert_eq!(points[0].values[0].target, AxisTarget::Event { event_id: "burn1".to_string(), value_key: "dv_x".to_string() });
    }

    /// Two event axes on the same `(event_id, value_key)` are refused as duplicates too -- the
    /// duplicate check must key on the FULL target, not merely `(instance, parameter)` (which
    /// would be empty/empty for both and never even distinguish these from each other, let alone
    /// catch the real duplication).
    #[test]
    fn refuses_two_event_axes_on_the_same_event_id_and_value_key() {
        let sweep = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { values: vec![1.0], ..event_axis("burn1", "dv_x") }, pb::SweepAxis { values: vec![2.0], ..event_axis("burn1", "dv_x") }],
            ..Default::default()
        };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::DuplicateAxis { ref key } if key == "event:burn1.dv_x"), "{err:?}");
    }

    /// A parameter axis and an event axis are never confused as duplicates of each other even
    /// when their "natural" identity strings might otherwise look related -- their keys are
    /// disjoint by construction (see the module doc comment), so this must succeed.
    #[test]
    fn a_parameter_axis_and_an_event_axis_are_never_treated_as_duplicates_of_each_other() {
        let sweep = pb::ParameterSweep {
            axes: vec![pb::SweepAxis { values: vec![1.0], ..axis("burn1", "dv_x") }, pb::SweepAxis { values: vec![2.0], ..event_axis("burn1", "dv_x") }],
            ..Default::default()
        };
        let points = expand_grid(&sweep).expect("a parameter axis instance=burn1/parameter=dv_x and an event axis event_id=burn1/value_key=dv_x must coexist");
        // One explicit value per axis -> a 1x1 = 1-point grid (two AxisValue entries at that one
        // point, one per axis) -- the point under test is that expand_grid does not refuse this
        // as a duplicate, not the size of the grid itself.
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].values.len(), 2, "one AxisValue per declared axis");
        let keys: std::collections::HashSet<String> = points[0].values.iter().map(|v| v.key()).collect();
        assert_eq!(keys, std::collections::HashSet::from(["burn1.dv_x".to_string(), "event:burn1.dv_x".to_string()]));
    }

    /// Question 192(c)'s collision analysis, made concrete: `crates/av-kernel/src/drm/schema.rs`
    /// places no character restriction on `SystemInstance.name` at all, so an instance can
    /// legitimately be named to literally start with the reserved event-axis-key prefix. Proves
    /// this is refused at validation time (`ReservedAxisKeyPrefix`), not merely assumed impossible
    /// -- the adversarial case the module doc comment's collision analysis describes.
    #[test]
    fn an_adversarially_named_instance_cannot_collide_with_the_event_axis_key_namespace() {
        // Constructed so the naive "just format instance.parameter" key would be
        // "event:demo_flt.spacecraft.DragArea" -- exactly the form
        // AxisTarget::Event{event_id:"demo_flt", value_key:"spacecraft.DragArea"}.key() produces.
        let adversarial = pb::SweepAxis { instance: "event:demo_flt".to_string(), parameter: "spacecraft.DragArea".to_string(), values: vec![1.0], ..Default::default() };
        let sweep = pb::ParameterSweep { axes: vec![adversarial], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(
            matches!(err, SweepError::ReservedAxisKeyPrefix { ref instance, ref parameter, prefix } if instance == "event:demo_flt" && parameter == "spacecraft.DragArea" && prefix == EVENT_AXIS_KEY_PREFIX),
            "{err:?}"
        );

        // Sanity: the colliding event axis it would have collided with is itself accepted and
        // produces exactly the key the parameter axis above was refused for trying to reach.
        let event_sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { values: vec![1.0], ..event_axis("demo_flt", "spacecraft.DragArea") }], ..Default::default() };
        let event_points = expand_grid(&event_sweep).expect("the event axis itself is perfectly valid");
        assert_eq!(event_points[0].values[0].key(), "event:demo_flt.spacecraft.DragArea");
    }
}

//! Grid expansion for a `ParameterSweep` (F1a, `docs/feasibility-plan.md`): turning
//! `ParameterSweep.axes` into the ordered list of concrete grid points a study runs, pinned by
//! hand rather than left to whatever a generic cartesian-product implementation happens to
//! produce. See [`expand_grid`]'s own doc comment for the exact, decided semantics.

use std::collections::{BTreeMap, HashSet};

use av_cdm::pb;

use crate::error::SweepError;

/// One axis's value at one grid point: the `SystemInstance`/`Parameter` it targets and the
/// value to apply there.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisValue {
    pub instance: String,
    pub parameter: String,
    pub value: f64,
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
    /// The `map<string, double>` `SweepSample.axis_values` wants: `"instance.parameter"` ->
    /// value, one entry per axis. A `BTreeMap` (not `HashMap`) so two callers building this map
    /// from the same `GridPoint` always iterate/serialize it in the same order.
    pub fn axis_values_map(&self) -> BTreeMap<String, f64> {
        self.values.iter().map(|v| (format!("{}.{}", v.instance, v.parameter), v.value)).collect()
    }
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
/// - **Duplicate axes.** Two axes naming the same `instance`+`parameter` is refused
///   ([`SweepError::DuplicateAxis`]).
/// - **Ordering.** Row-major, with the **last declared axis varying fastest** -- axes
///   `A=[1,2]`, `B=[10,20,30]` (in that declaration order) produce points, in `point_index`
///   order: `(1,10),(1,20),(1,30),(2,10),(2,20),(2,30)`.
/// - **Zero axes is legal.** A sweep with no axes at all (a pure Monte Carlo study over
///   `monte_carlo_draws` alone) expands to exactly one point with an empty `values` list.
pub fn expand_grid(sweep: &pb::ParameterSweep) -> Result<Vec<GridPoint>, SweepError> {
    let mut seen = HashSet::new();
    for axis in &sweep.axes {
        if !seen.insert((axis.instance.clone(), axis.parameter.clone())) {
            return Err(SweepError::DuplicateAxis { instance: axis.instance.clone(), parameter: axis.parameter.clone() });
        }
    }

    // One Vec<f64> of concrete values per declared axis, in declaration order.
    let mut axis_value_lists: Vec<(&pb::SweepAxis, Vec<f64>)> = Vec::with_capacity(sweep.axes.len());
    for axis in &sweep.axes {
        let values = if !axis.values.is_empty() {
            if axis.min != 0.0 || axis.max != 0.0 || axis.steps != 0 {
                return Err(SweepError::AmbiguousAxisDeclaration { instance: axis.instance.clone(), parameter: axis.parameter.clone() });
            }
            axis.values.clone()
        } else {
            if axis.steps < 2 {
                return Err(SweepError::AxisStepsBelowMinimum { instance: axis.instance.clone(), parameter: axis.parameter.clone(), steps: axis.steps });
            }
            // `!(axis.max > axis.min)` is refused by clippy's `neg_cmp_op_on_partial_ord`
            // (`f64` is only `PartialOrd`, not `Ord` -- NaN makes it incomparable, and the lint
            // wants that made explicit rather than expressed as a negated `>`): written via
            // `partial_cmp` instead, which also correctly refuses a NaN `min`/`max` (its
            // `partial_cmp` is `None`, so this is never `Some(Greater)` either).
            if axis.max.partial_cmp(&axis.min) != Some(std::cmp::Ordering::Greater) {
                return Err(SweepError::AxisRangeNotIncreasing { instance: axis.instance.clone(), parameter: axis.parameter.clone(), min: axis.min, max: axis.max });
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
                .map(|(value, (axis, _))| AxisValue { instance: axis.instance.clone(), parameter: axis.parameter.clone(), value })
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
        assert!(matches!(err, SweepError::AmbiguousAxisDeclaration { ref instance, ref parameter } if instance == "leo" && parameter == "cd"), "{err:?}");
    }

    #[test]
    fn refuses_steps_below_two() {
        let sweep = pb::ParameterSweep { axes: vec![pb::SweepAxis { min: 0.0, max: 5.0, steps: 1, ..axis("leo", "cd") }], ..Default::default() };
        let err = expand_grid(&sweep).unwrap_err();
        assert!(matches!(err, SweepError::AxisStepsBelowMinimum { steps: 1, .. }), "{err:?}");

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
        assert!(matches!(err, SweepError::AxisRangeNotIncreasing { min, max, .. } if min == 5.0 && max == 5.0), "{err:?}");

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
        assert!(matches!(err, SweepError::DuplicateAxis { ref instance, ref parameter } if instance == "leo" && parameter == "cd"), "{err:?}");
    }
}

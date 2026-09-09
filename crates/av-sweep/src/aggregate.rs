//! Per-point aggregates across draws (F2, `docs/feasibility-plan.md`'s F2 milestone): for every
//! `(point_index, score name)` pair, a [`pb::ScoreAggregate`] summarizing that score's values
//! across the draws at that grid point. Consumed by `src/bin/av-sweep/study.rs`'s `run_study` to
//! fill `SweepResults.aggregates`, which F1b left empty.
//!
//! ## Rules, decided, documented here because a reader will otherwise assume the wrong one
//!
//! - **Grouping.** Samples are grouped by `(point_index, score name)` -- the same score name at
//!   two different grid points is two separate aggregate rows; two different score names at the
//!   same point are two separate rows.
//! - **Only successful samples contribute.** A [`pb::SweepSample`] with a non-empty `error` is
//!   skipped entirely -- it contributes to no aggregate row for any score name. A failed sample
//!   never breaks aggregation (no panic, no `Err`); its failure is already visible in
//!   `SweepResults.samples` itself, so aggregation simply does not count it.
//! - **`ScoreAggregate.draws` is the number of draws that actually contributed, NOT the sweep's
//!   declared `monte_carlo_draws`.** These two numbers are equal exactly when every draw at that
//!   point succeeded, and differ exactly when at least one draw at that point failed. A reader
//!   who assumes `draws` is the declared count will silently miscompute a per-draw rate (e.g.
//!   "fraction of draws that ran") from it; `draws` here is always "how many numbers actually
//!   went into `mean`/`std_dev`/`min`/`max`/`pass_fraction` below", full stop.
//! - **A `(point, name)` pair with zero contributing draws produces no aggregate row at all** --
//!   never a placeholder row of zeros or `NaN`. If every draw at a point failed (or a point's
//!   samples never reported a given score name), that absence is already recorded in
//!   `SweepResults.samples` (every sample there either carries the score or a non-empty `error`);
//!   inventing a zeroed-out aggregate row on top would silently look like real, measured data.
//! - **`mean` is a plain left-to-right sum divided by `n`** (`values.iter().sum::<f64>() / n as
//!   f64`), specifically NOT a compensated (Kahan or pairwise) summation. This is a deliberate
//!   choice: it is what a reader doing the arithmetic by hand (or in a spreadsheet) reproduces
//!   exactly for the tiny per-point draw counts this crate ever sees (single digits to low tens),
//!   and this module's own pinned test (`aggregate_pins_mean_std_min_max_by_hand`) is exact
//!   equality against that same hand arithmetic -- a compensated summation could, in principle,
//!   land a floating-point ULP away from it.
//! - **`std_dev` is the POPULATION standard deviation** -- `sqrt(sum((x - mean)^2) / n)`, dividing
//!   by `n`, never by `n - 1` (the sample/Bessel-corrected standard deviation). This is the
//!   opposite of what a reader coming from introductory statistics usually assumes by default
//!   (`n - 1`), so it is called out here prominently: the draws recorded for a grid point ARE the
//!   whole population of realizations this study ran at that point, not a sample drawn from some
//!   larger population being estimated -- there is nothing "left out" to correct for. A concrete,
//!   load-bearing consequence: with exactly one contributing draw (`n == 1`), the population
//!   formula gives exactly `0.0` (the single value's own distance from the mean, which is itself,
//!   is zero) instead of a `0/0` division that the `n - 1` formula would hit.
//! - **`min`/`max`** are the plain min/max over the contributing values (never over all recorded
//!   draws, including failed ones -- see "only successful samples contribute" above).
//! - **`pass_fraction`** is `Some(passed_count as f64 / n as f64)` when EVERY contributing
//!   `ScoreResult` for that `(point, name)` carries `passed` (i.e. the score is an `Objective` at
//!   every draw); it is `None` when NONE of them do (a `MeasureOfEffectiveness`, which carries no
//!   pass/fail concept at all -- ADR-005 sec 6). A `(point, name)` group where `passed` is set on
//!   some contributing draws and unset on others is refused with
//!   [`SweepError::MixedPassCriterion`]: the same score name cannot be an `Objective` in one draw
//!   and a `MeasureOfEffectiveness` in another within a single study -- that would mean the DRM's
//!   own scoring declaration changed mid-study, which this crate never silently tolerates.
//! - **Output is sorted by `(point_index, name)`.** [`aggregate`] groups into a `BTreeMap<(u32,
//!   String), _>`, whose iteration order is already exactly this sort order (`u32`'s numeric
//!   `Ord`, then `String`'s lexicographic `Ord`), so no separate sort step is needed -- but the
//!   ordering is asserted directly by this module's own
//!   `aggregates_are_sorted_by_point_then_name` test rather than left as an accident of the
//!   chosen container.

use std::collections::BTreeMap;

use av_cdm::pb;

use crate::error::SweepError;

/// One contributing draw's value and pass criterion for one `(point, score name)` group.
struct Contribution {
    value: f64,
    passed: Option<bool>,
}

/// Builds every `(point_index, score name)` -> [`pb::ScoreAggregate`] this batch of samples
/// supports -- see the module doc comment for the exact, decided rules. Never panics; the one
/// refusal ([`SweepError::MixedPassCriterion`]) is a typed `Err`, not a panic, because it names a
/// real data inconsistency a caller may want to report without crashing the whole study run.
pub fn aggregate(samples: &[pb::SweepSample]) -> Result<Vec<pb::ScoreAggregate>, SweepError> {
    // BTreeMap<(point_index, name), contributions> -- iteration order IS the required
    // (point_index, name) sort order; see the module doc comment's last bullet.
    let mut groups: BTreeMap<(u32, String), Vec<Contribution>> = BTreeMap::new();

    for sample in samples {
        // "Only successful samples contribute" -- a failed sample's scores map is always empty
        // by construction (`study.rs::failed_sample`), but the error check is the real gate, not
        // an accidentally-empty map, so it is checked explicitly and first.
        if !sample.error.is_empty() {
            continue;
        }
        for (name, score) in &sample.scores {
            groups.entry((sample.point_index, name.clone())).or_default().push(Contribution { value: score.value, passed: score.passed });
        }
    }

    let mut aggregates = Vec::with_capacity(groups.len());
    for ((point_index, name), contributions) in groups {
        // Every key in `groups` was inserted alongside at least one contribution (see the loop
        // above), so `n >= 1` always holds here -- "zero contributing draws -> no row" is
        // therefore already true by construction: such a (point, name) pair never becomes a key
        // at all, so this loop never needs to skip one.
        let n = contributions.len();
        debug_assert!(n >= 1, "a group is only ever created alongside its first contribution");

        let sum: f64 = contributions.iter().map(|c| c.value).sum();
        let mean = sum / n as f64;

        let sq_diff_sum: f64 = contributions.iter().map(|c| { let d = c.value - mean; d * d }).sum();
        let std_dev = (sq_diff_sum / n as f64).sqrt(); // population variance -- see module doc.

        let min = contributions.iter().map(|c| c.value).fold(f64::INFINITY, f64::min);
        let max = contributions.iter().map(|c| c.value).fold(f64::NEG_INFINITY, f64::max);

        let with_pass_criterion = contributions.iter().filter(|c| c.passed.is_some()).count();
        let pass_fraction = if with_pass_criterion == n {
            let passed_count = contributions.iter().filter(|c| c.passed == Some(true)).count();
            Some(passed_count as f64 / n as f64)
        } else if with_pass_criterion == 0 {
            None
        } else {
            return Err(SweepError::MixedPassCriterion { name, point_index });
        };

        aggregates.push(pb::ScoreAggregate { name, point_index, draws: n as u32, mean, std_dev, min, max, pass_fraction });
    }

    Ok(aggregates)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn score(name: &str, value: f64, passed: Option<bool>) -> pb::ScoreResult {
        pb::ScoreResult { name: name.to_string(), value, passed, ..Default::default() }
    }

    fn ok_sample(point: u32, draw: u32, scores: Vec<(&str, f64, Option<bool>)>) -> pb::SweepSample {
        let mut m = BTreeMap::new();
        for (name, value, passed) in scores {
            m.insert(name.to_string(), score(name, value, passed));
        }
        pb::SweepSample { point_index: point, draw_index: draw, scores: m, error: String::new(), ..Default::default() }
    }

    fn failed_sample(point: u32, draw: u32) -> pb::SweepSample {
        pb::SweepSample { point_index: point, draw_index: draw, error: "deliberately failed for this test".to_string(), ..Default::default() }
    }

    /// Hand-computed arithmetic (also see the module doc comment): four draws with values
    /// `1, 2, 3, 4` at point 0 (even count):
    /// - sum = 1 + 2 + 3 + 4 = 10, mean = 10 / 4 = 2.5
    /// - squared deviations: (1-2.5)^2=2.25, (2-2.5)^2=0.25, (3-2.5)^2=0.25, (4-2.5)^2=2.25
    ///   sum of squared deviations = 2.25+0.25+0.25+2.25 = 5.0
    ///   population variance = 5.0 / 4 = 1.25 (divide by n, NOT n-1=3 -> that would be 5/3=1.6667)
    ///   population std_dev = sqrt(1.25) = 1.118033988749895 (to f64 precision)
    /// - min = 1.0, max = 4.0
    ///
    /// And three draws with values `2, 4, 6` at point 1 (odd count):
    /// - sum = 12, mean = 4.0
    /// - squared deviations: (2-4)^2=4, (4-4)^2=0, (6-4)^2=4 -> sum = 8.0
    ///   population variance = 8.0 / 3 = 2.6666666666666665
    ///   population std_dev = sqrt(8/3) = 1.6329931618554518
    /// - min = 2.0, max = 6.0
    ///
    /// Cross-checked independently in Python (`statistics.mean`/`statistics.pstdev`) --
    /// see this crate's own F2 report for the exact printed values; they matched these to full
    /// f64 precision.
    #[test]
    fn aggregate_pins_mean_std_min_max_by_hand() {
        let samples = vec![
            ok_sample(0, 0, vec![("m", 1.0, None)]),
            ok_sample(0, 1, vec![("m", 2.0, None)]),
            ok_sample(0, 2, vec![("m", 3.0, None)]),
            ok_sample(0, 3, vec![("m", 4.0, None)]),
            ok_sample(1, 0, vec![("m", 2.0, None)]),
            ok_sample(1, 1, vec![("m", 4.0, None)]),
            ok_sample(1, 2, vec![("m", 6.0, None)]),
        ];
        let aggregates = aggregate(&samples).expect("no mixed pass criterion in this fixture");
        assert_eq!(aggregates.len(), 2);

        let p0 = &aggregates[0];
        assert_eq!(p0.point_index, 0);
        assert_eq!(p0.draws, 4);
        assert_eq!(p0.mean, 2.5, "exact: 10.0 / 4.0");
        assert_eq!(p0.min, 1.0);
        assert_eq!(p0.max, 4.0);
        let expected_std0 = 1.25_f64.sqrt();
        assert!((p0.std_dev - expected_std0).abs() < 1e-12, "std_dev={} expected~={expected_std0} (sqrt is the only inexact step)", p0.std_dev);
        assert_eq!(p0.pass_fraction, None);

        let p1 = &aggregates[1];
        assert_eq!(p1.point_index, 1);
        assert_eq!(p1.draws, 3);
        assert_eq!(p1.mean, 4.0, "exact: 12.0 / 3.0");
        assert_eq!(p1.min, 2.0);
        assert_eq!(p1.max, 6.0);
        let expected_std1 = (8.0_f64 / 3.0).sqrt();
        assert!((p1.std_dev - expected_std1).abs() < 1e-12, "std_dev={} expected~={expected_std1}", p1.std_dev);
    }

    /// Same `1, 2, 3, 4` fixture as above, but pinned specifically against the SAMPLE (n-1)
    /// formula to prove the two genuinely differ here and that this module picked the population
    /// one: population std_dev = sqrt(1.25) = 1.1180339887498949; sample std_dev = sqrt(5/3) =
    /// 1.2909944487358056 -- these differ by about 0.173, far above any float tolerance, so a
    /// wrong implementation that divided by `n - 1` fails this test with a value near 1.29, not a
    /// rounding-level mismatch near 1.12.
    #[test]
    fn std_dev_is_the_population_deviation_not_the_sample_deviation() {
        let samples = vec![
            ok_sample(0, 0, vec![("m", 1.0, None)]),
            ok_sample(0, 1, vec![("m", 2.0, None)]),
            ok_sample(0, 2, vec![("m", 3.0, None)]),
            ok_sample(0, 3, vec![("m", 4.0, None)]),
        ];
        let aggregates = aggregate(&samples).unwrap();
        assert_eq!(aggregates.len(), 1);
        let population = 1.25_f64.sqrt();
        let sample_n_minus_1 = (5.0_f64 / 3.0).sqrt();
        assert!(sample_n_minus_1 - population > 0.1, "sanity: the two formulas must differ obviously in this fixture");
        assert!((aggregates[0].std_dev - population).abs() < 1e-12, "got {}, population formula predicts {population}", aggregates[0].std_dev);
        assert!((aggregates[0].std_dev - sample_n_minus_1).abs() > 0.1, "must NOT match the n-1 sample formula ({sample_n_minus_1}), got {}", aggregates[0].std_dev);
    }

    #[test]
    fn a_single_draw_has_zero_standard_deviation() {
        let samples = vec![ok_sample(0, 0, vec![("m", 42.0, None)])];
        let aggregates = aggregate(&samples).unwrap();
        assert_eq!(aggregates.len(), 1);
        assert_eq!(aggregates[0].draws, 1);
        assert_eq!(aggregates[0].std_dev, 0.0, "n=1: the value's own distance from the mean (itself) is exactly zero, not a 0/0 division");
        assert_eq!(aggregates[0].mean, 42.0);
        assert_eq!(aggregates[0].min, 42.0);
        assert_eq!(aggregates[0].max, 42.0);
    }

    /// Three draws declared at point 0, one of which failed -- `draws` on the resulting row must
    /// be 2 (the contributors), not 3 (the declared/attempted count), and the failed draw's
    /// (absent) value must not leak into mean/min/max. The failed draw here is built by hand
    /// (not via the `failed_sample` helper, which -- like the real `study.rs::failed_sample` it
    /// mirrors -- always leaves `scores` empty) with a NON-empty `scores` map on top of its
    /// non-empty `error`: real failed samples never carry scores, but this adversarial shape is
    /// exactly what distinguishes "the code gates on `error`" from a wrong implementation that
    /// instead gates on "scores happens to be empty" -- those two checks agree on every real
    /// input but disagree on this one, so only checking the real-shaped case would let a
    /// scores-emptiness check slip through undetected.
    #[test]
    fn a_failed_draw_is_excluded_and_draws_counts_only_the_contributors() {
        let mut adversarial_failed = failed_sample(0, 1);
        adversarial_failed.scores = BTreeMap::from([("m".to_string(), score("m", 999_999.0, None))]);
        let samples = vec![ok_sample(0, 0, vec![("m", 10.0, None)]), adversarial_failed, ok_sample(0, 2, vec![("m", 20.0, None)])];
        let aggregates = aggregate(&samples).unwrap();
        assert_eq!(aggregates.len(), 1);
        let a = &aggregates[0];
        assert_eq!(a.draws, 2, "only the two successful draws contribute; the declared/attempted count was 3");
        assert_eq!(a.mean, 15.0, "(10 + 20) / 2 -- the failed draw's 999999.0 must never be counted even though it sits right there in `scores`");
        assert_eq!(a.min, 10.0);
        assert_eq!(a.max, 20.0, "999999.0 must never become the max: the gate is `error`, not an accidentally-empty scores map");
    }

    /// Every draw at point 7 failed -- no aggregate row for point 7 at all (never a row of
    /// zeros/NaN). A different point (0) with real data still produces its own row, proving the
    /// all-failed point's absence is not accidentally swallowing everything.
    #[test]
    fn a_point_whose_every_draw_failed_produces_no_aggregate_row() {
        let samples = vec![failed_sample(7, 0), failed_sample(7, 1), ok_sample(0, 0, vec![("m", 5.0, None)])];
        let aggregates = aggregate(&samples).unwrap();
        assert_eq!(aggregates.len(), 1, "only point 0's row; point 7 contributed nothing");
        assert_eq!(aggregates[0].point_index, 0);
        assert!(aggregates.iter().all(|a| a.point_index != 7), "no placeholder row for the all-failed point");
    }

    /// `obj` (an Objective: every contributing draw carries `passed`) gets a real
    /// `Some(pass_fraction)`; `moe` (a MeasureOfEffectiveness: no draw carries `passed`) gets
    /// `None` -- both scores recorded at the same point, in the same samples, so a wrong
    /// implementation that confuses the two score names cannot pass by accident.
    #[test]
    fn pass_fraction_is_set_for_an_objective_and_unset_for_a_measure() {
        let samples = vec![
            ok_sample(0, 0, vec![("obj", 1.0, Some(true)), ("moe", 100.0, None)]),
            ok_sample(0, 1, vec![("obj", 1.0, Some(false)), ("moe", 200.0, None)]),
            ok_sample(0, 2, vec![("obj", 1.0, Some(true)), ("moe", 300.0, None)]),
        ];
        let aggregates = aggregate(&samples).unwrap();
        assert_eq!(aggregates.len(), 2);
        let obj = aggregates.iter().find(|a| a.name == "obj").unwrap();
        let moe = aggregates.iter().find(|a| a.name == "moe").unwrap();
        assert_eq!(obj.pass_fraction, Some(2.0 / 3.0), "2 of 3 passed");
        assert_eq!(moe.pass_fraction, None, "a measure of effectiveness has no pass criterion");
    }

    /// The same score name ("flaky") carries `passed` on one draw and not on another, at the same
    /// point -- a typed refusal, since one score cannot be an Objective in one draw and a Measure
    /// of Effectiveness in another within a single study.
    #[test]
    fn refuses_a_score_that_is_an_objective_in_one_draw_and_a_measure_in_another() {
        let samples = vec![ok_sample(0, 0, vec![("flaky", 1.0, Some(true))]), ok_sample(0, 1, vec![("flaky", 2.0, None)])];
        let err = aggregate(&samples).unwrap_err();
        assert!(matches!(err, SweepError::MixedPassCriterion { ref name, point_index } if name == "flaky" && point_index == 0), "{err:?}");
    }

    /// Three score names across two points, deliberately inserted out of the expected output
    /// order, must come back sorted by `(point_index, name)`.
    #[test]
    fn aggregates_are_sorted_by_point_then_name() {
        let samples = vec![
            ok_sample(1, 0, vec![("zeta", 1.0, None), ("alpha", 1.0, None)]),
            ok_sample(0, 0, vec![("zeta", 1.0, None), ("beta", 1.0, None)]),
        ];
        let aggregates = aggregate(&samples).unwrap();
        let keys: Vec<(u32, &str)> = aggregates.iter().map(|a| (a.point_index, a.name.as_str())).collect();
        assert_eq!(keys, vec![(0, "beta"), (0, "zeta"), (1, "alpha"), (1, "zeta")]);
    }
}

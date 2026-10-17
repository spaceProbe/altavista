//! The measure-of-effectiveness query, answered by ClickHouse.
//!
//! [`aggregates_sql`] returns one `SELECT` that computes every `(point, score)` aggregate **in
//! SQL** from the scores and samples tables of one write, with exactly the rules of
//! [`crate::aggregate::aggregate`]:
//!
//! - failed samples (`error <> ''`) contribute to nothing; `draws` is the number of contributing
//!   values; a `(point, name)` pair with no contributor produces no row (it has no score rows to
//!   group);
//! - the group key is the **map key** of `SweepSample.scores`, not `ScoreResult.name` (which is
//!   why the scores table stores both);
//! - `mean` is a left-to-right sum divided by `n`, `std_dev` the population deviation
//!   `sqrt(sum((x - mean)^2) / n)`, `min`/`max` the `f64::min`/`f64::max` fold, `pass_fraction`
//!   `passed_count / n` only when **every** contributor carries `passed`;
//! - rows come out sorted by `(point_index, name)` (bytewise on the name, like Rust's `String`
//!   order).
//!
//! # Why an order-fixed fold and not `avg()` / `sum()`
//!
//! Bit equality with `aggregate()` needs the same additions in the same order. ClickHouse's
//! `sum`/`avg` add each block's values in whatever order its threads and blocks produce them, so
//! the result is a valid sum but not *this* sum: floating-point addition is not associative.
//! `tests/clickhouse_store.rs` measures it (`native avg()/sum() ...` lines in its output) rather
//! than asserting it from theory. So each group's values are collected with
//! `groupArray((sample_ordinal, value))`, **sorted by `sample_ordinal`** (the position in
//! `SweepResults.samples`, which is draw order for every study `av-sweep` writes because it sorts
//! `(point, draw)`), and folded with `arrayFold`, whose accumulator is updated element by element
//! in array order. The fold's initial value is `-0.0`, because `Iterator::sum::<f64>()` on this
//! toolchain starts from `-0.0` (a sum of only `-0.0` stays `-0.0`; `tests` pins it).
//!
//! `min`/`max` are folded as well, not `min()`/`max()`: `f64::min` ignores a NaN operand and the
//! fold starts at `+inf` / `-inf`, so a group of only NaNs yields `+inf` / `-inf`, where
//! ClickHouse's `min(NaN)` yields NaN. Ties between `-0.0` and `+0.0` resolve the way the
//! aarch64 `fminnm` / `fmaxnm` instructions do (`-0.0 < +0.0`), which is what `f64::min` compiles
//! to on this host; on a target whose `minnum` returns either zero, `aggregate()` itself is
//! unspecified for that one case and bit equality is not claimed there.
//!
//! The only arithmetic left to Rust is reading the reply: the division for `pass_fraction` and
//! the mixed-criterion test are in the SQL too.

use super::schema::{quote_ident, SAMPLES, SCORES};

/// The aggregate query. Parameters: `sid` (the sweep id) and `wid` (the write id), both `String`.
/// Reply columns, in order (`RowBinary`): `name String`, `point_index UInt32`, `n UInt64`,
/// `mean Float64`, `std_dev Float64`, `min Float64`, `max Float64`, `pass_fraction
/// Nullable(Float64)`, `mixed Bool`.
pub fn aggregates_sql() -> String {
    let scores = quote_ident(SCORES);
    let samples = quote_ident(SAMPLES);
    format!(
        "SELECT name, point_index, n, mean, std_dev, min_v, max_v, pass_fraction, mixed
FROM
(
    SELECT
        name, point_index, n, mean,
        sqrt(arrayFold((acc, x) -> acc + (x - mean) * (x - mean), vals, toFloat64(-0.0)) / toFloat64(n)) AS std_dev,
        arrayFold((acc, x) -> if(isNaN(x), acc, if(x < acc, x, if(x = acc AND x = 0 AND bitShiftRight(reinterpretAsUInt64(x), 63) = 1, x, acc))), vals, CAST('inf' AS Float64)) AS min_v,
        arrayFold((acc, x) -> if(isNaN(x), acc, if(x > acc, x, if(x = acc AND x = 0 AND bitShiftRight(reinterpretAsUInt64(x), 63) = 0, x, acc))), vals, CAST('-inf' AS Float64)) AS max_v,
        if(with_pc = n, toFloat64(passed_n) / toFloat64(n), CAST(NULL AS Nullable(Float64))) AS pass_fraction,
        (with_pc > 0 AND with_pc < n) AS mixed
    FROM
    (
        SELECT
            name, point_index, vals, n, with_pc, passed_n,
            arrayFold((acc, x) -> acc + x, vals, toFloat64(-0.0)) / toFloat64(n) AS mean
        FROM
        (
            SELECT
                name, point_index,
                arrayMap(t -> t.2, arraySort(t -> t.1, pairs)) AS vals,
                length(pairs) AS n,
                with_pc, passed_n
            FROM
            (
                SELECT
                    sc.name_key AS name,
                    sa.point_index AS point_index,
                    groupArray((sc.sample_ordinal, sc.value)) AS pairs,
                    countIf(sc.passed IS NOT NULL) AS with_pc,
                    countIf(sc.passed = 1) AS passed_n
                FROM
                    (SELECT sample_ordinal, `key` AS name_key, value, passed FROM {scores} WHERE sweep_id = {{sid:String}} AND write_id = {{wid:String}}) AS sc
                INNER JOIN
                    (SELECT ordinal, point_index FROM {samples} WHERE sweep_id = {{sid:String}} AND write_id = {{wid:String}} AND error = '') AS sa
                    ON sc.sample_ordinal = sa.ordinal
                GROUP BY name, point_index
            )
        )
    )
)
ORDER BY point_index, name
FORMAT RowBinary"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_names_its_two_parameters_and_the_failed_sample_filter() {
        let sql = aggregates_sql();
        assert!(sql.contains("{sid:String}") && sql.contains("{wid:String}"));
        assert!(sql.contains("error = ''"), "failed samples are excluded");
        assert!(sql.contains("arrayFold"), "an order-fixed fold, not avg()/sum()");
        assert!(!sql.contains("avg(") && !sql.contains("sum("), "native float aggregates are not used");
        assert!(sql.ends_with("FORMAT RowBinary"));
    }

    /// The fold's initial accumulator must be what `Iterator::sum::<f64>()` starts from on this
    /// toolchain, or a group whose values are all `-0.0` would disagree in sign.
    #[test]
    fn rusts_f64_sum_starts_from_negative_zero() {
        let only_neg_zero: f64 = [-0.0f64].iter().sum();
        assert_eq!(only_neg_zero.to_bits(), (-0.0f64).to_bits());
        let empty: f64 = [].iter().sum();
        assert_eq!(empty.to_bits(), (-0.0f64).to_bits());
    }

    /// `f64::min` / `f64::max` semantics the SQL fold reproduces.
    #[test]
    fn rusts_f64_min_max_semantics_the_sql_fold_reproduces() {
        assert_eq!(f64::NAN.min(1.0), 1.0);
        assert_eq!(1.0f64.min(f64::NAN), 1.0);
        let all_nan_min = [f64::NAN, f64::NAN].iter().copied().fold(f64::INFINITY, f64::min);
        assert_eq!(all_nan_min, f64::INFINITY);
        let all_nan_max = [f64::NAN].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert_eq!(all_nan_max, f64::NEG_INFINITY);
        // The zero tie: measured on this host, recorded rather than assumed (see the module doc).
        let min_zero = [0.0f64, -0.0].iter().copied().fold(f64::INFINITY, f64::min);
        let min_zero_rev = [-0.0f64, 0.0].iter().copied().fold(f64::INFINITY, f64::min);
        assert_eq!(min_zero.to_bits(), min_zero_rev.to_bits(), "order-independent on this host");
    }
}

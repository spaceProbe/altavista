//! D3: the station-keeping rule. A RULE, not a controller -- no iteration, no wall clock, no
//! randomness: [`evaluate`] is a pure function of the scores it is handed and the knobs it
//! was constructed with.
//!
//! ## The rule
//!
//! Given the named score's value `v` (metres, `UNIT_METER`) and a declared reference radius
//! `r` (metres), the drift is `d = v - r`. A burn is proposed iff `|d| > threshold_m`.
//!
//! ## The burn magnitude -- a declared, deterministic, clamped proportional law
//!
//! `burn_mps = clamp(gain_per_s * d, -max_burn_mps, max_burn_mps)`
//!
//! - `d` (the drift) is in metres.
//! - `gain_per_s` is declared in units of `(m/s)/m`, i.e. `s^-1` -- a proportional gain, not
//!   a physical constant of the orbit; this rule makes no claim about the actual delta-v a
//!   real burn would need to correct the drift, only that a larger drift proposes a larger,
//!   bounded correction.
//! - `burn_mps`, the proposed correction, is in metres per second, clamped to
//!   `[-max_burn_mps, max_burn_mps]` so a single scored anomaly can never propose an
//!   unbounded burn.
//! - Sign convention: `d > 0` (radius too large) proposes a POSITIVE `burn_mps`; this module
//!   makes no claim about which physical direction (posigrade/retrograde) that corresponds
//!   to for a given orbit -- the sign is this rule's own declared correction-magnitude
//!   convention, carried on the proposed `Command`'s payload for whatever consumes it
//!   downstream to interpret against its own frame.
//!
//! ## Refusals (D3: never a default score of zero, never a cross-unit comparison)
//!
//! [`RuleRefusal`] is exhaustive over every way the named score can fail to be usable:
//! absent from the run's scores, present but not `UNIT_METER`, or present and the right unit
//! but not finite (`NaN`/`+-inf`). Every variant is typed and [`Counted`].

use std::collections::BTreeMap;

use av_cdm::pb::{ScoreResult, Unit};

use av_command::counters::Counted;

/// This rule's own declared knobs (D3: every one a command-line argument on the binary that
/// builds this value -- never an environment variable, never a magic number buried in
/// [`evaluate`] itself).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleConfig {
    pub score_name: String,
    pub reference_radius_m: f64,
    pub threshold_m: f64,
    pub gain_per_s: f64,
    pub max_burn_mps: f64,
}

/// Every way [`evaluate`] can refuse the named score -- see the module doc's "Refusals"
/// section.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleRefusal {
    /// `score_name` names no entry in the run's `GatewayQueryResponse.scores` -- never
    /// defaulted to `0.0`.
    ScoreAbsent { score_name: String },
    /// `score_name` is present but its declared [`Unit`] is not `UNIT_METER` -- never
    /// compared across units.
    ScoreWrongUnit { score_name: String, expected: Unit, actual: Unit },
    /// `score_name` is present, `UNIT_METER`, but its value is not finite (`NaN`, `+inf`,
    /// `-inf`).
    ScoreNotFinite { score_name: String, value: f64 },
}

impl Counted for RuleRefusal {
    fn code(&self) -> &'static str {
        match self {
            RuleRefusal::ScoreAbsent { .. } => "rule_score_absent",
            RuleRefusal::ScoreWrongUnit { .. } => "rule_score_wrong_unit",
            RuleRefusal::ScoreNotFinite { .. } => "rule_score_not_finite",
        }
    }
}

impl std::fmt::Display for RuleRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleRefusal::ScoreAbsent { score_name } => write!(f, "score {score_name:?} is absent from this run's scores -- never defaulted to 0.0"),
            RuleRefusal::ScoreWrongUnit { score_name, expected, actual } => {
                write!(f, "score {score_name:?} is {} but this rule requires {} -- never compared across units", actual.as_str_name(), expected.as_str_name())
            }
            RuleRefusal::ScoreNotFinite { score_name, value } => write!(f, "score {score_name:?} = {value} is not finite"),
        }
    }
}

/// What [`evaluate`] decided, for a score that WAS usable (see [`RuleRefusal`] for the ways
/// it might not have been).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleOutcome {
    /// `|drift_m| > threshold_m`: a burn is proposed.
    Burn { drift_m: f64, burn_mps: f64 },
    /// `|drift_m| <= threshold_m`: no proposal. Carries the drift anyway so a caller (the
    /// binary's own "no proposal, and why" line, D5) can report exactly how close it was.
    NoProposalNeeded { drift_m: f64 },
}

/// D3's rule, exactly. `scores` is a `BTreeMap` (never a `HashMap`, D8/ADR-004) -- the exact
/// shape `GatewayQueryResponse.scores` already is.
pub fn evaluate(scores: &BTreeMap<String, ScoreResult>, config: &RuleConfig) -> Result<RuleOutcome, RuleRefusal> {
    let score = scores.get(&config.score_name).ok_or_else(|| RuleRefusal::ScoreAbsent { score_name: config.score_name.clone() })?;

    let actual_unit = Unit::try_from(score.unit).unwrap_or(Unit::Unspecified);
    if actual_unit != Unit::Meter {
        return Err(RuleRefusal::ScoreWrongUnit { score_name: config.score_name.clone(), expected: Unit::Meter, actual: actual_unit });
    }
    if !score.value.is_finite() {
        return Err(RuleRefusal::ScoreNotFinite { score_name: config.score_name.clone(), value: score.value });
    }

    let drift_m = score.value - config.reference_radius_m;
    if drift_m.abs() <= config.threshold_m {
        return Ok(RuleOutcome::NoProposalNeeded { drift_m });
    }

    let raw_burn_mps = config.gain_per_s * drift_m;
    let burn_mps = raw_burn_mps.clamp(-config.max_burn_mps, config.max_burn_mps);
    Ok(RuleOutcome::Burn { drift_m, burn_mps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuleConfig {
        RuleConfig { score_name: "demo_flt_rmag_at_end".to_string(), reference_radius_m: 6_871_000.0, threshold_m: 100.0, gain_per_s: 0.001, max_burn_mps: 5.0 }
    }

    fn scores(name: &str, value: f64, unit: Unit) -> BTreeMap<String, ScoreResult> {
        let mut m = BTreeMap::new();
        m.insert(name.to_string(), ScoreResult { name: name.to_string(), value, unit: unit as i32, passed: None });
        m
    }

    #[test]
    fn a_drift_past_the_threshold_proposes_a_clamped_proportional_burn() {
        let cfg = config();
        // demo_flt_rmag_at_end from the real fixture: 6_870_517.4886757415 m.
        let s = scores(&cfg.score_name, 6_870_517.488_675_741_5, Unit::Meter);
        let outcome = evaluate(&s, &cfg).unwrap();
        match outcome {
            RuleOutcome::Burn { drift_m, burn_mps } => {
                let expected_drift = 6_870_517.488_675_741_5 - cfg.reference_radius_m;
                assert!((drift_m - expected_drift).abs() < 1e-6, "{drift_m}");
                assert!(drift_m.abs() > cfg.threshold_m);
                let expected_burn = (cfg.gain_per_s * expected_drift).clamp(-cfg.max_burn_mps, cfg.max_burn_mps);
                assert!((burn_mps - expected_burn).abs() < 1e-9, "{burn_mps} vs {expected_burn}");
            }
            other => panic!("expected a Burn outcome, got {other:?}"),
        }
    }

    #[test]
    fn the_burn_magnitude_is_clamped_at_the_declared_maximum() {
        let mut cfg = config();
        cfg.gain_per_s = 10.0; // deliberately huge, to force clamping
        let s = scores(&cfg.score_name, cfg.reference_radius_m + 1_000.0, Unit::Meter);
        let outcome = evaluate(&s, &cfg).unwrap();
        match outcome {
            RuleOutcome::Burn { burn_mps, .. } => assert_eq!(burn_mps, cfg.max_burn_mps),
            other => panic!("expected a Burn outcome, got {other:?}"),
        }
    }

    #[test]
    fn a_drift_within_the_threshold_proposes_nothing() {
        let cfg = config();
        let s = scores(&cfg.score_name, cfg.reference_radius_m + 1.0, Unit::Meter);
        let outcome = evaluate(&s, &cfg).unwrap();
        assert!(matches!(outcome, RuleOutcome::NoProposalNeeded { .. }), "{outcome:?}");
    }

    #[test]
    fn a_drift_exactly_at_the_threshold_proposes_nothing() {
        // Boundary convention pinned: `abs(drift) <= threshold` is within tolerance.
        let cfg = config();
        let s = scores(&cfg.score_name, cfg.reference_radius_m + cfg.threshold_m, Unit::Meter);
        let outcome = evaluate(&s, &cfg).unwrap();
        assert!(matches!(outcome, RuleOutcome::NoProposalNeeded { .. }), "{outcome:?}");
    }

    #[test]
    fn an_absent_score_is_refused_typed_and_never_defaulted_to_zero() {
        let cfg = config();
        let empty = BTreeMap::new();
        let err = evaluate(&empty, &cfg).unwrap_err();
        assert_eq!(err, RuleRefusal::ScoreAbsent { score_name: cfg.score_name.clone() });
        assert_eq!(err.code(), "rule_score_absent");
    }

    #[test]
    fn a_wrong_unit_score_is_refused_typed_and_never_compared_across_units() {
        let cfg = config();
        let s = scores(&cfg.score_name, 6_871_050.0, Unit::MeterPerSecond);
        let err = evaluate(&s, &cfg).unwrap_err();
        assert_eq!(err, RuleRefusal::ScoreWrongUnit { score_name: cfg.score_name.clone(), expected: Unit::Meter, actual: Unit::MeterPerSecond });
        assert_eq!(err.code(), "rule_score_wrong_unit");
    }

    #[test]
    fn a_non_finite_score_is_refused_typed() {
        let cfg = config();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let s = scores(&cfg.score_name, bad, Unit::Meter);
            let err = evaluate(&s, &cfg).unwrap_err();
            assert!(matches!(err, RuleRefusal::ScoreNotFinite { .. }), "{err:?}");
            assert_eq!(err.code(), "rule_score_not_finite");
        }
    }

    #[test]
    fn every_refusal_has_a_distinct_stable_code() {
        let codes = [
            RuleRefusal::ScoreAbsent { score_name: "x".to_string() }.code(),
            RuleRefusal::ScoreWrongUnit { score_name: "x".to_string(), expected: Unit::Meter, actual: Unit::Second }.code(),
            RuleRefusal::ScoreNotFinite { score_name: "x".to_string(), value: f64::NAN }.code(),
        ];
        let mut sorted = codes.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
    }
}

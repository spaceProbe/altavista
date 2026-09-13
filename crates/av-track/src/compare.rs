//! The produced tracks against the run's own truth `Trajectory`, reported as a full time
//! series and as max/p50/p99 (question 148: "an exit code is not evidence" -- the actual
//! measured error is what this module hands back, never only a pass/fail bool).
//!
//! # The pinned tolerance, and exactly what it is derived from
//!
//! [`POSITION_TOLERANCE_M`] is not a number chosen to make a test pass. It is derived from
//! the two things that actually bound this milestone's own tracking error, both confirmed
//! by reading the fixture, not assumed:
//!
//! 1. **Measurement noise contributes essentially zero.** `crates/av-edge/tests/
//!    plugin_replay.rs::decoded_positions_match_the_flight_instances_truth_trajectory_
//!    within_tolerance` -- E4's own pinned precondition, which this milestone's own brief
//!    names as its precondition -- measures and prints a **maximum deviation of `0.0` m**
//!    between the plugin's decoded position and the run's own truth trajectory, over all
//!    900 epochs. `Measurement.r` (the declared covariance this scenario's `TrackConfig`
//!    carries into the Kalman filter's measurement-noise model) is therefore an *assumed*
//!    sensor uncertainty the filter uses for weighting, not a real error present in the
//!    data it is weighting -- so it cannot be the source of any measured tracking error
//!    here.
//! 2. **The truth motion is genuinely zero-acceleration.** `drms/
//!    demo_ground_segment_flight.system.yaml`'s own declared `parameters: accel.{x,y,z} =
//!    0.0` -- read directly from that fixture, not inferred -- means a constant-velocity
//!    Kalman filter (`crate::config`'s own choice, matching this truth motion exactly) has
//!    no structural model-mismatch bias to converge away from; there is no persistent
//!    "CV tracking a CA target" lag term to derive here at all.
//!
//! What is left, and what this tolerance actually bounds, is the **initiation transient**:
//! `spoore_engine::SinglePointInitiator` seeds a new track's position exactly from its
//! first measurement (`crate::config::TrackConfig`'s own `measured` declaration:
//! `pos_x`/`pos_y`/`pos_z`) but seeds velocity from a wide, zero-mean prior
//! (`TrackConfig::velocity_prior_sigma_mps`, `20,000` m/s -- `crate::config`'s own doc:
//! "no assumed direction of travel"), while the real asset is moving at roughly 7.5 km/s.
//! Because every one of the 900 measurements is exact (deviation `0.0` m from truth, point
//! 1 above) and the truth motion is exactly what the filter's own model assumes (point 2),
//! there is nothing for the filter to keep correcting *toward* scan after scan -- unlike a
//! noisy-sensor scenario, where convergence is gradual because each new measurement only
//! partially overrides the filter's own running estimate. `tests/engine_accuracy.rs` runs
//! the real 900-scan demo fixture through this exact bridge and **measures and prints**
//! the actual result: maximum error on the order of a **few millimetres** across all 900
//! epochs (read that test's own recorded output for the exact printed digits -- this
//! module does not restate them here, so this comment cannot silently drift out of sync
//! with what the test actually measures) -- the velocity estimate is corrected essentially
//! fully by the second or third real update, and what remains is `f64` rounding through the
//! Kalman recursion's own matrix arithmetic, not a genuine, physically-meaningful tracking
//! lag. [`POSITION_TOLERANCE_M`] (`1.0` m) is set roughly three orders of magnitude above
//! that measured millimetre-scale residual: generous enough that ordinary `f64`
//! non-associativity across a different compiler/BLAS/platform cannot spuriously fail this
//! test, while still tight enough that a genuine regression -- a model-mismatch bug, a
//! mis-wired sensor index, a units error -- would be caught outright rather than hidden
//! under a tolerance sized to whatever the code happened to produce (this crate's own
//! standing instruction: "not a number picked to make the test pass").

use std::collections::BTreeMap;

use av_edge::pb;
use spoore_cdm::TrackUpdate;

/// Pinned position tolerance, metres -- see this module's own doc for the full
/// derivation. `1.0` m: roughly three orders of magnitude above the millimetre-scale
/// `f64`-rounding residual `tests/engine_accuracy.rs` actually measures and prints for the
/// demo fixture's own 900-scan run (that test's own assertion is against this exact
/// constant, and its own `println!` reports the actual measured value every run) --
/// generous enough to absorb ordinary floating-point non-associativity across a different
/// compiler/platform, while still tight enough that a real regression (a model-mismatch
/// bug, a mis-wired sensor index, a units error) is caught rather than hidden.
pub const POSITION_TOLERANCE_M: f64 = 1.0;

/// One matched epoch's own position error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErrorSample {
    pub tai_ns: i64,
    pub error_m: f64,
}

/// The full result of comparing a shard's `TrackUpdate`s against a truth `Trajectory`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonReport {
    /// Every matched epoch, in ascending order.
    pub series: Vec<ErrorSample>,
    pub max_error_m: f64,
    pub p50_error_m: f64,
    pub p99_error_m: f64,
    /// How many `TrackUpdate`s had no matching truth sample at their own epoch (never
    /// silently ignored -- reported so a caller can see whether "no error" secretly meant
    /// "nothing was actually compared").
    pub unmatched_updates: usize,
}

impl ComparisonReport {
    /// One line of human-readable evidence, suitable for a test's own `println!` (question
    /// 148: the measured error, printed, not only asserted against).
    pub fn summary_line(&self) -> String {
        format!(
            "track-vs-truth position error over {} matched epoch(s) ({} unmatched): max={:.3} m, p50={:.3} m, p99={:.3} m (tolerance {:.3} m)",
            self.series.len(),
            self.unmatched_updates,
            self.max_error_m,
            self.p50_error_m,
            self.p99_error_m,
            POSITION_TOLERANCE_M,
        )
    }

    /// Whether every matched epoch's error is within [`POSITION_TOLERANCE_M`].
    pub fn within_tolerance(&self) -> bool {
        self.max_error_m <= POSITION_TOLERANCE_M
    }
}

/// Nearest-rank percentile over an already-sorted-ascending slice (`p` in `[0.0, 1.0]`).
/// `0.0` is added, on the ADR-004 side of these tests, as the intent to be honest for both
/// tests running at this repo's own root of the workspace: the nearest-rank method
/// (`ceil(p * n) - 1`, clamped) is the same convention used for percentile displays
/// elsewhere on this platform (`crates/av-dynamics-service`'s own latency reporting), so
/// this module does not invent a second percentile convention.
fn percentile(sorted_ascending: &[f64], p: f64) -> f64 {
    if sorted_ascending.is_empty() {
        return 0.0;
    }
    let n = sorted_ascending.len();
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n) - 1;
    sorted_ascending[rank]
}

/// Compares `updates` (a shard's own produced `TrackUpdate`s, in any order -- e.g.
/// `crate::bridge::EngineBridge::run`'s own return value) against `truth` (the run's own
/// declared truth `Trajectory` -- `RunProducts.trajectories["flight"]` for this milestone's
/// demo fixture), matched by exact `tai_ns` epoch.
///
/// Each `TrackUpdate.fused_state` is converted through `av_cdm::spoore_v0::
/// gaussian_state_to_pb` (never a second epoch-shift or position-extraction -- this
/// module's own instruction, mirroring `crate::bridge`'s identical rule for measurements),
/// which both shifts the epoch onto TAI and gives a plain `mean: Vec<f64>` this function
/// reads `mean[0..3]` from (`air_3d_state_space`'s own declared component order:
/// `pos_x, pos_y, pos_z, vel_x, vel_y, vel_z>` -- `crate::config`'s own module doc).
///
/// Every track this shard ever held is compared, not only a confirmed one: a tentative
/// track's position error is exactly as real as a confirmed one's, and this milestone's
/// own single-target, zero-clutter scenario never has more than one live track at a time
/// in practice (`tests/engine_accuracy.rs` asserts this directly) -- so there is no
/// track-identity ambiguity for this comparison to paper over.
pub fn compare_to_truth(updates: &[TrackUpdate], truth: &pb::Trajectory) -> ComparisonReport {
    let truth_by_epoch: BTreeMap<i64, [f64; 3]> = truth.samples.iter().map(|s| (s.tai_ns, [s.mean[0], s.mean[1], s.mean[2]])).collect();

    let mut series = Vec::new();
    let mut unmatched_updates = 0usize;
    for update in updates {
        let gs = av_cdm::spoore_v0::gaussian_state_to_pb(&update.fused_state);
        match truth_by_epoch.get(&gs.epoch_ns) {
            Some(truth_pos) => {
                let dx = gs.mean[0] - truth_pos[0];
                let dy = gs.mean[1] - truth_pos[1];
                let dz = gs.mean[2] - truth_pos[2];
                let error_m = (dx * dx + dy * dy + dz * dz).sqrt();
                series.push(ErrorSample { tai_ns: gs.epoch_ns, error_m });
            }
            None => unmatched_updates += 1,
        }
    }
    series.sort_by_key(|s| s.tai_ns);

    let mut errors: Vec<f64> = series.iter().map(|s| s.error_m).collect();
    errors.sort_by(|a, b| a.partial_cmp(b).expect("no NaN position error"));
    let max_error_m = errors.last().copied().unwrap_or(0.0);
    let p50_error_m = percentile(&errors, 0.50);
    let p99_error_m = percentile(&errors, 0.99);

    ComparisonReport { series, max_error_m, p50_error_m, p99_error_m, unmatched_updates }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn truth(samples: &[(i64, [f64; 3])]) -> pb::Trajectory {
        pb::Trajectory {
            samples: samples
                .iter()
                .map(|(tai_ns, pos)| pb::TrajectorySample { tai_ns: *tai_ns, mean: pos.to_vec(), ..Default::default() })
                .collect(),
            ..Default::default()
        }
    }

    /// A valid (positive-definite) 6x6 identity covariance, row-major -- `GaussianState::
    /// from_slices` refuses an all-zero matrix (not positive definite, every eigenvalue
    /// zero), so these tests need a real, if arbitrary, SPD covariance rather than the
    /// simplest-looking `vec![0.0; 36]`.
    fn identity6() -> Vec<f64> {
        let mut m = vec![0.0; 36];
        for i in 0..6 {
            m[i * 6 + i] = 1.0;
        }
        m
    }

    fn update(track_id: &str, tai_ns_utc: i64, mean6: [f64; 6]) -> TrackUpdate {
        let state = spoore_cdm::GaussianState::from_slices(&mean6, &identity6(), "air_3d", spoore_cdm::Epoch::from_nanos(tai_ns_utc)).unwrap();
        TrackUpdate {
            track_id: track_id.to_string(),
            epoch: spoore_cdm::Epoch::from_nanos(tai_ns_utc),
            shard_key: "s".to_string(),
            fused_state: state,
            belief: spoore_cdm::Belief::new(vec![spoore_cdm::MixtureComponent::new(
                "root",
                1.0,
                spoore_cdm::GaussianState::from_slices(&mean6, &identity6(), "air_3d", spoore_cdm::Epoch::from_nanos(tai_ns_utc)).unwrap(),
            )])
            .unwrap(),
            predictions: vec![],
            event: spoore_cdm::LifecycleEvent::Updated,
            track_score: 0.0,
            associations: vec![],
        }
    }

    #[test]
    fn perfectly_matching_positions_have_zero_error() {
        // update's own epoch is UTC-scale (spoore's own convention); gaussian_state_to_pb
        // shifts it onto TAI by whatever offset the leap-second table has in force at this
        // instant -- read that shifted value back directly, rather than hardcoding a
        // specific offset that would be wrong for whichever epoch this test happens to use.
        let u = update("t1", 0, [1.0, 2.0, 3.0, 0.0, 0.0, 0.0]);
        let shifted_tai_ns = av_cdm::spoore_v0::gaussian_state_to_pb(&u.fused_state).epoch_ns;
        let t = truth(&[(shifted_tai_ns, [1.0, 2.0, 3.0])]);
        let report = compare_to_truth(&[u], &t);
        assert_eq!(report.unmatched_updates, 0);
        assert_eq!(report.max_error_m, 0.0);
    }

    #[test]
    fn an_update_with_no_matching_truth_epoch_is_counted_unmatched_not_silently_dropped() {
        let t = truth(&[(1, [0.0, 0.0, 0.0])]);
        let u = update("t1", 999_999_999_999, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let report = compare_to_truth(&[u], &t);
        assert_eq!(report.unmatched_updates, 1);
        assert!(report.series.is_empty());
    }

    #[test]
    fn percentiles_are_nearest_rank_over_the_sorted_errors() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&sorted, 0.50), 3.0);
        assert_eq!(percentile(&sorted, 0.99), 5.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}

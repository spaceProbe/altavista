//! `Propagate`'s sampling loop, run entirely on the GMAT worker thread
//! (`crate::worker::WorkerHandle::run`) -- everything here takes `&GmatModel`/
//! `&StmAugmented<GmatModel>` by reference and returns owned, `Send` values, so
//! `crate::service` can call it from inside a `WorkerHandle::run` closure without any of
//! this module itself needing to know about threads or tonic.
//!
//! **Why there is no `MaxStepAttempts`-style chunking guard here**, unlike
//! `gmat_service.model.GmatModel._run` (`services/gmat-service/README.md`'s "A
//! correctness finding" section, `docs/open-questions.md` question 77): that guard exists
//! because GMAT's own native `Propagator.Step(dt)` sub-steps internally up to
//! `MaxStepAttempts` (default 50) and gives up, leaving the state *partly* advanced, if a
//! single external call spans too many internal steps -- a real, measured failure mode of
//! GMAT's *stateful* propagator object. This crate never calls that API at all: every
//! sample here is produced by `av_dynamics::integrate::Dopri5` (our own adaptive
//! integrator, ADR-002: "the integrator is ours") stepping `gmat_sys::model::GmatModel`'s
//! *pure* `derivatives(state, dt)` function, which has no `MaxStepAttempts`-like failure
//! mode to guard against -- `Dopri5::integrate` always completes (subdividing internally,
//! capped at `max_step`) or returns the model's own `Err`. [`sample_epochs_ns`] still
//! chunks the horizon at `sample_interval_s` (matching `gmat_service`'s external cadence
//! contract and this crate's own README), but that chunking is for **trajectory sampling
//! cadence only**, not survival of an integrator failure mode this crate's integrator does
//! not have.

use av_cdm::covariance::check_spd_row_major;
use av_dynamics::stm::{propagate_covariance as stm_propagate_covariance, StmAugmented};
use av_dynamics::DynamicsModel;
use gmat_sys::model::GmatModel;
use gmat_sys::GmatError;

/// One recorded trajectory point: epoch, physical 6-state, and (only when covariance was
/// requested) the row-major 6x6 covariance at that epoch.
#[derive(Debug, Clone)]
pub struct Sample {
    pub tai_ns: i64,
    pub mean: [f64; 6],
    pub cov: Option<Vec<f64>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PropagateError {
    #[error("GMAT error: {0}")]
    Model(#[from] GmatError),
    #[error("covariance hygiene: {0}")]
    CovarianceHygiene(#[from] av_cdm::covariance::CovarianceHygieneError),
}

/// The epochs to sample at: `seed_epoch_ns`, then every `sample_interval_s` after it, then
/// `horizon_ns` exactly -- **always both endpoints**, matching
/// `gmat_service.model.GmatModel._run`'s own documented cadence contract ("always
/// including t=0 and the final point"). `sample_interval_s` need not evenly divide the
/// horizon: the last regular tick before `horizon_ns` and `horizon_ns` itself may be less
/// than one full interval apart.
///
/// # Panics
///
/// If `sample_interval_s` is not positive, or `horizon_ns <= seed_epoch_ns` -- both are
/// request-validation failures the caller (`crate::service`) must reject before reaching
/// here, not conditions this function recovers from.
pub fn sample_epochs_ns(seed_epoch_ns: i64, horizon_ns: i64, sample_interval_s: f64) -> Vec<i64> {
    assert!(sample_interval_s > 0.0, "sample_interval_s must be positive");
    assert!(horizon_ns > seed_epoch_ns, "horizon_ns must be after seed_epoch_ns");
    let dt_ns = (sample_interval_s * 1e9).round() as i64;
    let dt_ns = dt_ns.max(1);

    let mut epochs = vec![seed_epoch_ns];
    let mut t = seed_epoch_ns;
    while t + dt_ns < horizon_ns {
        t += dt_ns;
        epochs.push(t);
    }
    if *epochs.last().expect("epochs always has at least the seed") != horizon_ns {
        epochs.push(horizon_ns);
    }
    epochs
}

/// Chains `model.step` across consecutive `epochs`, recording each endpoint. Bit-for-bit
/// equivalent to one call spanning the whole horizon would be **not quite** the claim here
/// (`Dopri5`'s adaptive step size restarts at `initial_step` on each chunk boundary rather
/// than carrying over what it learned mid-horizon) -- both are independently within
/// `Dopri5`'s own `rtol`/`atol` at every accepted step, which is the guarantee this
/// function actually needs (see this crate's README for the measured golden-arc agreement
/// either way).
pub fn run_plain(model: &GmatModel, seed: [f64; 6], epochs: &[i64]) -> Result<Vec<Sample>, GmatError> {
    let mut samples = Vec::with_capacity(epochs.len());
    samples.push(Sample { tai_ns: epochs[0], mean: seed, cov: None });

    let mut state = seed;
    for w in epochs.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let result = model.step(&state, t0, &[], t1 - t0)?;
        state = result.state.try_into().expect("GmatModel is a 6-state model");
        samples.push(Sample { tai_ns: t1, mean: state, cov: None });
    }
    Ok(samples)
}

/// Like [`run_plain`], but over the STM-augmented state, threading the **accumulated**
/// `Phi(t0, t)` forward across chunk boundaries rather than reseeding it to the identity
/// each time: `stm_model.step` (the plain `DynamicsModel::step` default, not
/// `step_with_stm`, which always reseeds `Phi = I`) integrates whatever augmented state it
/// is handed, and `d(Phi)/dt = A(t) Phi` is linear and homogeneous in `Phi`, so integrating
/// it forward from the *already-accumulated* `Phi(t0, t_i)` over `[t_i, t_{i+1}]` produces
/// exactly `Phi(t0, t_{i+1})` (the semigroup property `Phi(t0,t_{i+1}) =
/// Phi(t_i,t_{i+1}) Phi(t0,t_i)`, applied by superposition rather than by matrix-multiplying
/// two separately-integrated factors). `P(t) = Phi(t0,t) P0 Phi(t0,t)^T`
/// (`av_dynamics::stm::propagate_covariance`) at every sample, each run through
/// `av_cdm::covariance::check_spd_row_major` before being accepted -- a hygiene failure
/// (`docs/open-questions.md` question 80) aborts the whole call rather than returning a
/// partially-checked trajectory, matching `gmat_service.model.GmatModel
/// .propagate_covariance`'s own `nearest_spd_projection=False`-by-default behaviour (there
/// is no `PropagateRequest` field to read that opt-in from -- see this crate's README).
pub fn run_with_covariance(
    stm_model: &StmAugmented<GmatModel>,
    seed: [f64; 6],
    p0: &[f64],
    epochs: &[i64],
) -> Result<Vec<Sample>, PropagateError> {
    const N: usize = 6;
    let mut aug = StmAugmented::<GmatModel>::seed(&seed); // [state; vec(Phi(t0,t0)=I)]
    let mut samples = Vec::with_capacity(epochs.len());

    // Phi(t0,t0) = I exactly (the seed, not an integration result) -> P(t0) = P0 exactly,
    // modulo the explicit symmetrization propagate_covariance always applies.
    let (cov0, _asym0) = stm_propagate_covariance(&aug[N..N + N * N], p0, N);
    check_spd_row_major(&cov0, N, "av-dynamics-service propagate_covariance sample t0")?;
    samples.push(Sample { tai_ns: epochs[0], mean: seed, cov: Some(cov0) });

    for w in epochs.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        let result = stm_model.step(&aug, t0, &[], t1 - t0)?;
        aug = result.state;
        let mean: [f64; N] = aug[0..N].try_into().expect("StmAugmented<GmatModel> keeps the physical block first");
        let phi = &aug[N..N + N * N];
        let (cov, _asym) = stm_propagate_covariance(phi, p0, N);
        let context = format!("av-dynamics-service propagate_covariance sample at tai_ns={t1}");
        check_spd_row_major(&cov, N, &context)?;
        samples.push(Sample { tai_ns: t1, mean, cov: Some(cov) });
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_epochs_always_includes_both_endpoints_and_is_evenly_spaced_when_it_divides_exactly() {
        let epochs = sample_epochs_ns(0, 86_400_000_000_000, 600.0);
        assert_eq!(epochs.first(), Some(&0));
        assert_eq!(epochs.last(), Some(&86_400_000_000_000));
        assert_eq!(epochs.len(), 145, "86400/600 + 1 = 145 samples expected");
        for w in epochs.windows(2) {
            assert_eq!(w[1] - w[0], 600_000_000_000);
        }
    }

    #[test]
    fn sample_epochs_adds_a_short_final_leg_when_the_interval_does_not_divide_evenly() {
        let epochs = sample_epochs_ns(0, 1_000_000_000_000, 300.0); // 1000s / 300s
        assert_eq!(epochs, vec![0, 300_000_000_000, 600_000_000_000, 900_000_000_000, 1_000_000_000_000]);
    }

    #[test]
    fn sample_epochs_with_interval_longer_than_the_horizon_is_just_the_two_endpoints() {
        let epochs = sample_epochs_ns(0, 100_000_000_000, 600.0);
        assert_eq!(epochs, vec![0, 100_000_000_000]);
    }
}

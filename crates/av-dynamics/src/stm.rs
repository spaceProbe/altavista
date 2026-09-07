//! Propagating covariance through a model's own state transition matrix (ADR-002 second
//! amendment, `docs/adr/002-dynamics-contract.md`), without ever pulling GMAT (or any
//! model-specific machinery) into this crate.
//!
//! Two pieces:
//!
//! - [`StmAugmented`] wraps any [`crate::DynamicsModel`] that declares
//!   [`crate::DynamicsModel::stm_capable`], presenting the augmented `[state; vec(Phi)]`
//!   vector as its own state so the kernel's existing (dimension-agnostic) scheduler and
//!   `Dopri5` integrator drive it exactly as they drive the plain model -- no separate
//!   stepping machinery, matching ADR-002's "the integrator is ours" and the second
//!   amendment's finding that GMAT's own dimension-agnostic `GetDerivatives` does the same
//!   thing internally (a 42-state model is stepped by the identical code path as a 6-state
//!   one). One augmented integration from a system's own epoch produces both the physical
//!   trajectory and `Phi(t0, t)` together -- this is "the kernel integrates it" (never a
//!   read-back of GMAT's own `Spacecraft` STM after the fact).
//! - [`propagate_covariance`] is the linear-algebra half, `P(t) = Phi P0 Phi^T`, explicitly
//!   symmetrized (round-off can otherwise leave `P` measurably asymmetric) with the
//!   asymmetry it corrected reported back rather than silently discarded.

use crate::{DynamicsModel, StepResult};

/// Wraps a [`DynamicsModel`] that declares [`DynamicsModel::stm_capable`], presenting the
/// `state_dim() + state_dim()^2`-length augmented `[state; vec(Phi)]` vector as its own state
/// (`derivatives` here calls the wrapped model's `stm_derivatives`). See the module docs.
pub struct StmAugmented<M: DynamicsModel>(M);

impl<M: DynamicsModel> StmAugmented<M> {
    /// # Panics
    ///
    /// If `model.stm_capable()` is `false` -- `StmAugmented` exists only to drive an
    /// STM-capable model's augmented state through the ordinary `DynamicsModel` machinery, so
    /// wrapping a model that does not declare the capability is a caller bug, caught here
    /// rather than surfacing later as silently-identity covariance.
    pub fn new(model: M) -> Self {
        assert!(
            model.stm_capable(),
            "StmAugmented::new wraps only a model whose stm_capable() is true; got a model \
             that does not declare the STM capability"
        );
        Self(model)
    }

    /// The wrapped model.
    pub fn inner(&self) -> &M {
        &self.0
    }

    /// The augmented initial condition `[x0; vec(I)]` (`Phi(t0, t0) = I`, the exact identity
    /// per the ADR-002 second amendment's measurement) for a physical state `x0` of length
    /// `n`. The natural seed for `Kernel::register_system` (`av-kernel`) when covariance is
    /// requested for a system.
    pub fn seed(x0: &[f64]) -> Vec<f64> {
        let n = x0.len();
        let mut out = vec![0.0; n + n * n];
        out[0..n].copy_from_slice(x0);
        for i in 0..n {
            out[n + i * n + i] = 1.0;
        }
        out
    }
}

impl<M: DynamicsModel> DynamicsModel for StmAugmented<M> {
    type Error = M::Error;

    fn state_dim(&self) -> usize {
        let n = self.0.state_dim();
        n + n * n
    }

    fn derivatives(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], state_dot: &mut [f64]) -> Result<(), Self::Error> {
        self.0.stm_derivatives(state, t_tai_ns, controls, state_dot)
    }

    fn describe(&self) -> av_cdm::pb::ModelInfo {
        self.0.describe()
    }

    fn integrator(&self) -> crate::integrate::Dopri5 {
        self.0.integrator()
    }

    /// Overrides the trait's default `step` (`docs/open-questions.md` question 101, M11.2).
    /// Through M11.1 this method was never overridden, so a caller stepping a `StmAugmented<M>`
    /// (exactly what `av-kernel`'s covariance path does) got the trait's default: one continuous
    /// `Dopri5` integration of the whole `[state; vec(Phi)]` vector via [`derivatives`]
    /// (`self.0.stm_derivatives`) -- which never calls the wrapped model's own
    /// [`DynamicsModel::step_with_stm`] at all. Any `outputs` a model's `step_with_stm` override
    /// populated (`gmat_sys::model::GmatModel::step_with_stm`, mirroring `GmatModel::step`'s own
    /// `OUTPUT_RMAG`/`OUTPUT_CD`) was therefore dead code as far as the covariance path was
    /// concerned -- exactly the gap question 101 names: "a covariance run has fewer products
    /// than a plain run."
    ///
    /// The fix: `step` now delegates to `self.0.step_with_stm` for exactly this native period,
    /// seeded from the *physical* state alone (`state[0..n]`), and composes the returned
    /// **local** STM `Phi(t, t+dt)` (`stm_result.phi`) with the accumulated `Phi(t0, t)` already
    /// carried in `state[n..n+n^2]`: `Phi(t0, t+dt) = Phi(t, t+dt) . Phi(t0, t)`. This is exact
    /// in continuous time (STM composition: `Phi(t0,t2) = Phi(t1,t2) Phi(t0,t1)` for any `t1`
    /// between `t0` and `t2`), not an approximation, and it is what makes a wrapped model's own
    /// `step_with_stm` override -- outputs included -- actually reachable from the covariance
    /// path: `state_dot`/`derivatives` above is unchanged and still drives any caller that
    /// integrates this type directly rather than calling `step`.
    ///
    /// **Numerically**, this is a different scheme than the previous continuous accumulation:
    /// each native period's local STM is now computed from a fresh `Phi(t,t) = I` seed rather
    /// than carried forward as part of one long adaptive integration over the whole run, so
    /// [`crate::integrate::Dopri5`]'s adaptive step-size choices (its error norm is a max over
    /// *every* augmented-state component, `Phi` included, so `Phi`'s own magnitude affects step
    /// selection) can differ slightly between the two schemes. Measured, not assumed: this
    /// crate's own `StmAugmented`-independent `step_with_stm` tests below are unaffected (they
    /// never touch `StmAugmented`), and `crates/av-kernel/tests/golden_acceptance.rs::
    /// kernel_covariance_matches_the_golden_stm_and_propagated_cov` -- which drives exactly this
    /// path against a genuine GMAT-generated STM/covariance golden -- still passes at its
    /// existing tolerance; see this task's own report for the measured numbers.
    fn step(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<StepResult, Self::Error> {
        let n = self.0.state_dim();
        let (x0, phi_old) = state.split_at(n);
        let stm_result = self.0.step_with_stm(x0, t_tai_ns, controls, dt_ns)?;
        let phi_local = &stm_result.phi;

        // Phi(t0, t+dt) = Phi(t, t+dt) . Phi(t0, t) -- row-major n x n throughout, same
        // convention `propagate_covariance` below uses for its own matrix products.
        let mut phi_new = vec![0.0; n * n];
        for i in 0..n {
            for j in 0..n {
                let mut s = 0.0;
                for k in 0..n {
                    s += phi_local[i * n + k] * phi_old[k * n + j];
                }
                phi_new[i * n + j] = s;
            }
        }

        let mut out_state = Vec::with_capacity(n + n * n);
        out_state.extend_from_slice(&stm_result.state);
        out_state.extend_from_slice(&phi_new);
        Ok(StepResult { state: out_state, t_tai_ns: stm_result.t_tai_ns, outputs: stm_result.outputs })
    }
}

/// `P(t) = Phi P0 Phi^T`, `n x n` row-major throughout. Explicitly symmetrizes the result
/// (`0.5 * (P + P^T)`) and returns the maximum off-diagonal asymmetry `Phi P0 Phi^T` had
/// *before* symmetrizing, so a caller can report the correction rather than let an
/// asymmetric matrix through silently (a binding rule: "if `Phi P0 Phi^T` drifts from
/// symmetry by round-off, symmetrize explicitly and say so").
///
/// # Panics
///
/// If `phi.len() != n * n` or `p0.len() != n * n`.
pub fn propagate_covariance(phi: &[f64], p0: &[f64], n: usize) -> (Vec<f64>, f64) {
    assert_eq!(phi.len(), n * n, "propagate_covariance: phi is not {n}x{n}");
    assert_eq!(p0.len(), n * n, "propagate_covariance: p0 is not {n}x{n}");

    // tmp = Phi * P0.
    let mut tmp = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            let mut s = 0.0;
            for k in 0..n {
                s += phi[i * n + k] * p0[k * n + j];
            }
            tmp[i * n + j] = s;
        }
    }
    // p = tmp * Phi^T.
    let mut p = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            let mut s = 0.0;
            for k in 0..n {
                s += tmp[i * n + k] * phi[j * n + k];
            }
            p[i * n + j] = s;
        }
    }

    let mut max_asym = 0.0f64;
    for i in 0..n {
        for j in (i + 1)..n {
            max_asym = max_asym.max((p[i * n + j] - p[j * n + i]).abs());
        }
    }

    let mut sym = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            sym[i * n + j] = 0.5 * (p[i * n + j] + p[j * n + i]);
        }
    }
    (sym, max_asym)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A model with a *known, closed-form* state transition matrix: a planar rotation at
    /// constant angular rate `w` (`x' = A x`, `A = [[0, w], [-w, 0]]`, autonomous -- no
    /// explicit time dependence, so `Phi(t0, t0+dt)` depends only on `dt`).
    /// `Phi(dt) = [[cos(w dt), sin(w dt)], [-sin(w dt), cos(w dt)]]`, det = 1 exactly (a
    /// rotation), giving an independent way to check `stm_derivatives` / `step_with_stm` /
    /// `StmAugmented` / `propagate_covariance` together without any GMAT dependency.
    struct Rotator {
        w: f64,
        calls: Cell<usize>,
    }

    impl DynamicsModel for Rotator {
        type Error = std::convert::Infallible;

        fn state_dim(&self) -> usize {
            2
        }

        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = self.w * state[1];
            out[1] = -self.w * state[0];
            Ok(())
        }

        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo::default()
        }

        fn stm_capable(&self) -> bool {
            true
        }

        fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], augmented_state_dot: &mut [f64]) -> Result<(), Self::Error> {
            self.calls.set(self.calls.get() + 1);
            let n = 2;
            self.derivatives(&augmented_state[0..n], t_tai_ns, controls, &mut augmented_state_dot[0..n])?;
            // d(Phi)/dt = A Phi, A = [[0, w], [-w, 0]].
            let phi = &augmented_state[n..n + n * n];
            let a = [[0.0, self.w], [-self.w, 0.0]];
            for row in 0..n {
                for col in 0..n {
                    let mut s = 0.0;
                    for k in 0..n {
                        s += a[row][k] * phi[k * n + col];
                    }
                    augmented_state_dot[n + row * n + col] = s;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn stm_derivatives_leaves_the_physical_block_identical_to_derivatives() {
        let model = Rotator { w: 0.3, calls: Cell::new(0) };
        let x = [1.0, 2.0];
        let mut plain_dot = [0.0; 2];
        model.derivatives(&x, 0, &[], &mut plain_dot).unwrap();

        let aug = StmAugmented::<Rotator>::seed(&x);
        let mut aug_dot = vec![0.0; 6];
        model.stm_derivatives(&aug, 0, &[], &mut aug_dot).unwrap();
        assert_eq!(&aug_dot[0..2], &plain_dot);
    }

    #[test]
    fn step_with_stm_matches_the_closed_form_rotation_matrix() {
        let w = 0.25;
        let model = Rotator { w, calls: Cell::new(0) };
        let x0 = [1.0, 0.0];
        let dt_ns: i64 = 4_000_000_000; // 4 s
        let dt_s = 4.0;

        let result = model.step_with_stm(&x0, 0, &[], dt_ns).unwrap();
        assert!(model.calls.get() > 0, "stm_derivatives was never called");

        let (c, s) = ((w * dt_s).cos(), (w * dt_s).sin());
        let want_phi = [c, s, -s, c];
        for (got, want) in result.phi.iter().zip(want_phi.iter()) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        // Phi(0, dt) applied to x0 must reproduce the state this same call propagated.
        let want_state = [want_phi[0] * x0[0] + want_phi[1] * x0[1], want_phi[2] * x0[0] + want_phi[3] * x0[1]];
        for (got, want) in result.state.iter().zip(want_state.iter()) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }

        // Independent Liouville check: a rotation is measure-preserving, det(Phi) == 1.
        let det = result.phi[0] * result.phi[3] - result.phi[1] * result.phi[2];
        assert!((det - 1.0).abs() < 1e-9, "det(Phi) = {det}, expected ~1");
    }

    #[test]
    fn phi_t0_t0_is_the_exact_identity() {
        let model = Rotator { w: 1.7, calls: Cell::new(0) };
        let result = model.step_with_stm(&[3.0, -1.0], 0, &[], 0).unwrap();
        // dt = 0: the integrator's `while t < t1 - 1e-9` loop body never runs, so this is
        // exactly the seed, not merely close to it.
        assert_eq!(result.phi, vec![1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn stm_augmented_reports_the_wrapped_models_state_dim_squared_plus_itself() {
        let model = Rotator { w: 1.0, calls: Cell::new(0) };
        let wrapped = StmAugmented::new(model);
        assert_eq!(wrapped.state_dim(), 2 + 4);
    }

    #[test]
    #[should_panic(expected = "stm_capable() is true")]
    fn stm_augmented_refuses_a_model_that_does_not_declare_the_capability() {
        struct NotStmCapable;
        impl DynamicsModel for NotStmCapable {
            type Error = std::convert::Infallible;
            fn state_dim(&self) -> usize {
                2
            }
            fn derivatives(&self, _: &[f64], _: i64, _: &[f64], _: &mut [f64]) -> Result<(), Self::Error> {
                Ok(())
            }
            fn describe(&self) -> av_cdm::pb::ModelInfo {
                av_cdm::pb::ModelInfo::default()
            }
        }
        let _ = StmAugmented::new(NotStmCapable);
    }

    #[test]
    fn propagate_covariance_of_identity_phi_returns_p0_unchanged() {
        let n = 3;
        let identity: Vec<f64> = (0..n * n).map(|k| if k / n == k % n { 1.0 } else { 0.0 }).collect();
        let p0 = vec![4.0, 1.0, 0.0, 1.0, 9.0, 2.0, 0.0, 2.0, 1.0]; // symmetric, arbitrary SPD-looking
        let (p, max_asym) = propagate_covariance(&identity, &p0, n);
        assert_eq!(max_asym, 0.0);
        for (got, want) in p.iter().zip(p0.iter()) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }
    }

    #[test]
    fn propagate_covariance_matches_the_rotation_closed_form_and_preserves_trace() {
        // A rotation conjugates a covariance without changing its eigenvalues, so the trace
        // (sum of variances) is invariant -- an independent physical sanity check beyond
        // "the arithmetic ran".
        let w: f64 = 0.6;
        let dt: f64 = 2.0;
        let (c, s) = ((w * dt).cos(), (w * dt).sin());
        let phi = [c, s, -s, c];
        let p0 = [5.0, 0.0, 0.0, 2.0]; // diagonal, unequal variances
        let (p, max_asym) = propagate_covariance(&phi, &p0, 2);
        assert!(max_asym < 1e-12);
        let trace0 = p0[0] + p0[3];
        let trace1 = p[0] + p[3];
        assert!((trace0 - trace1).abs() < 1e-9, "{trace0} vs {trace1}");
        // p must stay symmetric.
        assert!((p[1] - p[2]).abs() < 1e-15);
    }

    #[test]
    fn propagate_covariance_symmetrizes_and_reports_a_manufactured_asymmetry() {
        // Phi = I, but P0 is deliberately *not* symmetric -- Phi P0 Phi^T = P0 here, so the
        // pre-symmetrization asymmetry is exactly P0's own asymmetry, and the function must
        // both report it and hand back a symmetric result anyway.
        let identity = [1.0, 0.0, 0.0, 1.0];
        let p0_asym = [1.0, 2.0, 5.0, 3.0]; // p0[0][1]=2 != p0[1][0]=5
        let (p, max_asym) = propagate_covariance(&identity, &p0_asym, 2);
        assert!((max_asym - 3.0).abs() < 1e-12, "expected |2-5|=3, got {max_asym}");
        assert_eq!(p[1], p[2], "result must be exactly symmetric after correction");
        assert!((p[1] - 3.5).abs() < 1e-12, "0.5*(2+5) = 3.5");
    }

    // -- StmAugmented::step (question 101, M11.2) --------------------------------------------

    /// Like `Rotator` above, but also overrides `step_with_stm` to populate `StmStepResult
    /// .outputs` -- mirroring exactly why `gmat_sys::model::GmatModel::step_with_stm` is
    /// overridden (question 101): a model whose own `step_with_stm` reports a named output that
    /// `derivatives`/`stm_derivatives` alone could never produce (here, a call counter, standing
    /// in for GmatModel's GMAT-real-parameter reads).
    struct OutputtingRotator {
        w: f64,
        step_with_stm_calls: Cell<usize>,
    }
    impl DynamicsModel for OutputtingRotator {
        type Error = std::convert::Infallible;
        fn state_dim(&self) -> usize {
            2
        }
        fn derivatives(&self, state: &[f64], _t_tai_ns: i64, _controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            out[0] = self.w * state[1];
            out[1] = -self.w * state[0];
            Ok(())
        }
        fn describe(&self) -> av_cdm::pb::ModelInfo {
            av_cdm::pb::ModelInfo::default()
        }
        fn stm_capable(&self) -> bool {
            true
        }
        fn stm_derivatives(&self, augmented_state: &[f64], t_tai_ns: i64, controls: &[f64], out: &mut [f64]) -> Result<(), Self::Error> {
            let n = 2;
            self.derivatives(&augmented_state[0..n], t_tai_ns, controls, &mut out[0..n])?;
            let phi = &augmented_state[n..n + n * n];
            let a = [[0.0, self.w], [-self.w, 0.0]];
            for row in 0..n {
                for col in 0..n {
                    out[n + row * n + col] = (0..n).map(|k| a[row][k] * phi[k * n + col]).sum();
                }
            }
            Ok(())
        }
        fn step_with_stm(&self, state: &[f64], t_tai_ns: i64, controls: &[f64], dt_ns: i64) -> Result<crate::StmStepResult, Self::Error> {
            self.step_with_stm_calls.set(self.step_with_stm_calls.get() + 1);
            // Reuse the trait's own default integration (identical to what a hand-rolled
            // override would do), then attach a named output -- the part a real override
            // (GmatModel's real-parameter read) adds on top.
            let n = self.state_dim();
            let mut aug0 = vec![0.0; n + n * n];
            aug0[0..n].copy_from_slice(state);
            for i in 0..n {
                aug0[n + i * n + i] = 1.0;
            }
            let dt_s = dt_ns as f64 * 1e-9;
            let (aug1, _stats) = self.integrator().integrate(
                |t_rel_s, x, out| {
                    let t_ns = t_tai_ns + (t_rel_s * 1e9).round() as i64;
                    self.stm_derivatives(x, t_ns, controls, out)
                },
                &aug0,
                0.0,
                dt_s,
            )?;
            let (state1, phi) = aug1.split_at(n);
            let mut outputs = std::collections::BTreeMap::new();
            outputs.insert("calls".to_string(), self.step_with_stm_calls.get() as f64);
            Ok(crate::StmStepResult { state: state1.to_vec(), phi: phi.to_vec(), t_tai_ns: t_tai_ns + dt_ns, outputs })
        }
    }

    #[test]
    fn stm_augmented_step_delegates_to_step_with_stm_and_carries_its_outputs() {
        let w = 0.25;
        let model = OutputtingRotator { w, step_with_stm_calls: Cell::new(0) };
        let wrapped = StmAugmented::new(model);
        let x0 = [1.0, 0.0];
        let seed = StmAugmented::<OutputtingRotator>::seed(&x0);

        let dt_ns: i64 = 4_000_000_000; // 4 s
        let result = wrapped.step(&seed, 0, &[], dt_ns).unwrap();
        assert!(wrapped.inner().step_with_stm_calls.get() > 0, "step must reach the wrapped model's own step_with_stm");
        assert_eq!(result.outputs.get("calls"), Some(&1.0), "StepResult.outputs must carry StmStepResult.outputs through unchanged");

        // Phi(0, dt) must match the closed form -- with only one native step, composing
        // against the identity seed must reproduce step_with_stm's own answer exactly.
        let dt_s = 4.0;
        let (c, s) = ((w * dt_s).cos(), (w * dt_s).sin());
        let want_phi = [c, s, -s, c];
        let (mean, phi) = result.state.split_at(2);
        for (got, want) in phi.iter().zip(want_phi.iter()) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        let want_state = [want_phi[0] * x0[0] + want_phi[1] * x0[1], want_phi[2] * x0[0] + want_phi[3] * x0[1]];
        for (got, want) in mean.iter().zip(want_state.iter()) {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
    }

    #[test]
    fn stm_augmented_step_composes_the_local_stm_with_the_accumulated_one_over_two_native_periods() {
        // Two native periods of 2 s each must compose to the same Phi(0, 4s) a single 4 s
        // step_with_stm call would report directly (STM composition is exact for this linear,
        // autonomous system) -- proving `step`'s per-call reseed-and-multiply scheme is not
        // silently accumulating error or dropping the carried-in Phi.
        let w = 0.4;
        let model = OutputtingRotator { w, step_with_stm_calls: Cell::new(0) };
        let wrapped = StmAugmented::new(model);
        let x0 = [1.0, 0.0];
        let seed = StmAugmented::<OutputtingRotator>::seed(&x0);

        let period_ns: i64 = 2_000_000_000; // 2 s
        let after_1 = wrapped.step(&seed, 0, &[], period_ns).unwrap();
        let after_2 = wrapped.step(&after_1.state, period_ns, &[], period_ns).unwrap();
        assert_eq!(wrapped.inner().step_with_stm_calls.get(), 2, "one step_with_stm call per native period");
        assert_eq!(after_2.outputs.get("calls"), Some(&2.0));

        let (_, phi_two_steps) = after_2.state.split_at(2);
        let dt_s = 4.0;
        let (c, s) = ((w * dt_s).cos(), (w * dt_s).sin());
        let want_phi = [c, s, -s, c];
        for (got, want) in phi_two_steps.iter().zip(want_phi.iter()) {
            assert!((got - want).abs() < 1e-8, "{got} vs {want}");
        }
    }
}

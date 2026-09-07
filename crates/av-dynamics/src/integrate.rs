//! A Dormand-Prince 5(4) adaptive integrator over any derivative function (ADR-002: the
//! integrator is ours, validated against GMAT's).
//!
//! Moved here verbatim from `crates/gmat-sys/src/integrate.rs` (M2.1): this is ADR-002's "one
//! [integrator] family ... shared by every domain and binding", not a GMAT-specific detail, so
//! it lives in the crate every dynamics binding depends on rather than in the GMAT one.
//! `gmat-sys` re-exports this module (`pub use av_dynamics::integrate;`) so existing callers
//! (`gmat_sys::integrate::Dopri5`) are unaffected. Not a single line of the algorithm below
//! changed in the move -- see the crate README for the bit-identical verification.

/// Statistics from one integration.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub steps: usize,
    pub rejected: usize,
    pub evaluations: usize,
}

/// Tolerances and step-size control.
#[derive(Debug, Clone, Copy)]
pub struct Dopri5 {
    pub rtol: f64,
    pub atol: f64,
    pub initial_step: f64,
    pub max_step: f64,
}

impl Default for Dopri5 {
    fn default() -> Self {
        Dopri5 { rtol: 1e-12, atol: 1e-12, initial_step: 30.0, max_step: 600.0 }
    }
}

impl Dopri5 {
    /// Integrate `x' = f(t, x)` from `t0` to `t1` (seconds). `f` writes the derivative into
    /// its third argument and returns an error to abort.
    pub fn integrate<E, F>(&self, mut f: F, x0: &[f64], t0: f64, t1: f64) -> Result<(Vec<f64>, Stats), E>
    where
        F: FnMut(f64, &[f64], &mut [f64]) -> Result<(), E>,
    {
        const C: [f64; 7] = [0.0, 1.0 / 5.0, 3.0 / 10.0, 4.0 / 5.0, 8.0 / 9.0, 1.0, 1.0];
        const A: [[f64; 6]; 7] = [
            [0.0; 6],
            [1.0 / 5.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [3.0 / 40.0, 9.0 / 40.0, 0.0, 0.0, 0.0, 0.0],
            [44.0 / 45.0, -56.0 / 15.0, 32.0 / 9.0, 0.0, 0.0, 0.0],
            [19372.0 / 6561.0, -25360.0 / 2187.0, 64448.0 / 6561.0, -212.0 / 729.0, 0.0, 0.0],
            [9017.0 / 3168.0, -355.0 / 33.0, 46732.0 / 5247.0, 49.0 / 176.0, -5103.0 / 18656.0, 0.0],
            [35.0 / 384.0, 0.0, 500.0 / 1113.0, 125.0 / 192.0, -2187.0 / 6784.0, 11.0 / 84.0],
        ];
        const B5: [f64; 7] = [35.0 / 384.0, 0.0, 500.0 / 1113.0, 125.0 / 192.0, -2187.0 / 6784.0, 11.0 / 84.0, 0.0];
        const B4: [f64; 7] = [5179.0 / 57600.0, 0.0, 7571.0 / 16695.0, 393.0 / 640.0, -92097.0 / 339200.0, 187.0 / 2100.0, 1.0 / 40.0];

        let n = x0.len();
        let mut x = x0.to_vec();
        let mut t = t0;
        let mut h = self.initial_step.min(self.max_step);
        let mut stats = Stats::default();
        let mut k = vec![vec![0.0; n]; 7];
        let mut xi = vec![0.0; n];
        let mut x5 = vec![0.0; n];
        let mut x4 = vec![0.0; n];

        while t < t1 - 1e-9 {
            h = h.min(t1 - t).min(self.max_step);
            f(t, &x, &mut k[0])?;
            stats.evaluations += 1;
            for i in 1..7 {
                for j in 0..n {
                    let mut acc = 0.0;
                    for (l, a) in A[i].iter().enumerate().take(i) {
                        acc += a * k[l][j];
                    }
                    xi[j] = x[j] + h * acc;
                }
                f(t + C[i] * h, &xi, &mut k[i])?;
                stats.evaluations += 1;
            }
            let mut err: f64 = 0.0;
            for j in 0..n {
                let mut s5 = 0.0;
                let mut s4 = 0.0;
                for i in 0..7 {
                    s5 += B5[i] * k[i][j];
                    s4 += B4[i] * k[i][j];
                }
                x5[j] = x[j] + h * s5;
                x4[j] = x[j] + h * s4;
                let sc = self.atol + self.rtol * x[j].abs().max(x5[j].abs());
                err = err.max(((x5[j] - x4[j]) / sc).abs());
            }
            if err <= 1.0 {
                t += h;
                x.copy_from_slice(&x5);
                stats.steps += 1;
                let factor = if err > 0.0 { (0.9 * err.powf(-0.2)).clamp(0.2, 5.0) } else { 5.0 };
                h *= factor;
            } else {
                stats.rejected += 1;
                h *= (0.9 * err.powf(-0.25)).max(0.1);
            }
        }
        Ok((x, stats))
    }
}

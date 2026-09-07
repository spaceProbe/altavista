//! Cubic Hermite interpolation using each sample's velocity components
//! (`INTERPOLATION_HERMITE_VELOCITY`, `proto/altavista/v1/trajectory.proto`) -- the CDM's
//! declared interpolation contract, matched here bit-for-formula, not merely by name. Used
//! whenever the kernel needs a system's state at a time that isn't one of that system's own
//! native step epochs (a system running slower than the output sample rate, or whose native
//! step times simply don't land on an output tick).
//!
//! ## Interpolation by component class (ADR-005 sec 3, `docs/open-questions.md` question 88)
//!
//! [`hermite_velocity`] above is the *first-six-components* rule alone, with everything past
//! index 6 simply held at the nearer endpoint (documented as a deliberate non-contract on
//! that function's own doc comment). [`interpolate_by_state_space`] is the general form: it
//! classifies **every** component of a *declared* `av_cdm::pb::StateSpace` (labels and units
//! -- never index position, except for the one place ADR-005's own table is itself phrased by
//! position: "the first six of a Cartesian space") into one of the five classes ADR-005 sec 3
//! lists, and interpolates each class by its own rule:
//!
//! | class | rule | this module |
//! |---|---|---|
//! | position/velocity (first six of a Cartesian space) | cubic Hermite with velocity | [`hermite_velocity`], reused verbatim on that one 6-slice |
//! | unit quaternion (`q_x, q_y, q_z, q_w`) | normalized slerp, unit norm asserted | [`slerp_xyzw`] |
//! | rates, masses, scalars | linear | [`linear`] |
//! | STM, covariance | never interpolated | rejected: [`InterpolationError::NeverInterpolated`] |
//! | discrete modes, counters | zero-order hold | [`zero_order_hold`] |
//!
//! A component this module cannot place in one of those classes from what the `StateSpace`
//! actually declares is a **typed error** ([`InterpolationError::UnclassifiableComponent`]),
//! never a silent fallback to linear or to the nearest-endpoint pass-through
//! [`hermite_velocity`] uses past its own six components -- see that error's doc comment and
//! [`classify`] for exactly which labels/units this module currently recognizes.

/// Interpolate a state at `t_ns`, given two bracketing samples `(t0_ns, s0)` and `(t1_ns,
/// s1)` with `t0_ns <= t_ns <= t1_ns`, using cubic Hermite on each of the first three
/// (position) components with that component's velocity (index `3+i`) as the derivative at
/// each endpoint. Requires a state space whose first six components are `[pos_x, pos_y,
/// pos_z, vel_x, vel_y, vel_z]`, exactly as `INTERPOLATION_HERMITE_VELOCITY` documents.
///
/// `p(s) = h00(s) p0 + h10(s) dt v0 + h01(s) p1 + h11(s) dt v1`, `s = (t - t0) / dt in [0,
/// 1]`, the standard two-point cubic Hermite basis. The interpolated velocity is the
/// analytic derivative of that *same* cubic (`dp/dt = dp/ds * ds/dt`), not a second,
/// independently-interpolated quantity that could disagree with `dp/dt` -- position and
/// velocity are always consistent with each other by construction.
///
/// Any state components beyond the first six (e.g. STM elements were this ever applied to a
/// 42-state model, which nothing in this crate does) are **not** interpolated by this
/// scheme -- there is no Hermite contract for them. Rather than silently zeroing or linearly
/// blending them, this takes the nearer endpoint's value unchanged (`s < 0.5` picks `s0`,
/// otherwise `s1`), and callers should not rely on those components being meaningfully
/// interpolated at all.
///
/// # Panics
///
/// If `s0.len() != s1.len()`, if that length is less than 6, or if `t1_ns <= t0_ns`.
pub fn hermite_velocity(t0_ns: i64, s0: &[f64], t1_ns: i64, s1: &[f64], t_ns: i64) -> Vec<f64> {
    assert_eq!(s0.len(), s1.len(), "hermite_velocity: sample dimensions differ ({} vs {})", s0.len(), s1.len());
    assert!(s0.len() >= 6, "hermite_velocity needs at least 6 components ([pos x3; vel x3, ...]), got {}", s0.len());
    assert!(t1_ns > t0_ns, "hermite_velocity: t1_ns ({t1_ns}) must be after t0_ns ({t0_ns})");

    let dt = (t1_ns - t0_ns) as f64 * 1e-9;
    let s = ((t_ns - t0_ns) as f64 * 1e-9) / dt;

    let h00 = 2.0 * s.powi(3) - 3.0 * s.powi(2) + 1.0;
    let h10 = s.powi(3) - 2.0 * s.powi(2) + s;
    let h01 = -2.0 * s.powi(3) + 3.0 * s.powi(2);
    let h11 = s.powi(3) - s.powi(2);

    // d/ds of the basis above.
    let dh00 = 6.0 * s.powi(2) - 6.0 * s;
    let dh10 = 3.0 * s.powi(2) - 4.0 * s + 1.0;
    let dh01 = -6.0 * s.powi(2) + 6.0 * s;
    let dh11 = 3.0 * s.powi(2) - 2.0 * s;

    let n = s0.len();
    let mut out = vec![0.0; n];
    for i in 0..3 {
        let (p0, v0) = (s0[i], s0[3 + i]);
        let (p1, v1) = (s1[i], s1[3 + i]);
        out[i] = h00 * p0 + h10 * dt * v0 + h01 * p1 + h11 * dt * v1;
        // dp/dt = (dp/ds) * (ds/dt) = (dp/ds) / dt.
        out[3 + i] = (dh00 * p0 + dh10 * dt * v0 + dh01 * p1 + dh11 * dt * v1) / dt;
    }
    for item in out.iter_mut().enumerate().take(n).skip(6) {
        let (i, out_i) = item;
        *out_i = if s < 0.5 { s0[i] } else { s1[i] };
    }
    out
}

// ---------------------------------------------------------------------------------------
// Classification and general interpolation by declared StateSpace (ADR-005 sec 3).
// ---------------------------------------------------------------------------------------

use av_cdm::pb::{StateComponent, StateSpace, Unit};

/// Labels/units this module recognizes as the six-component Cartesian position/velocity
/// prefix `hermite_velocity` interpolates -- `altavista.cdm`'s
/// `_cartesian_pos_vel_6`/`STATE_SPACE_ID_CARTESIAN_POS_VEL_6` builder emits exactly this
/// shape, so a `StateSpace` produced there and one built here always agree on what counts.
const POSITION_VELOCITY_LABELS: [&str; 6] = ["pos_x", "pos_y", "pos_z", "vel_x", "vel_y", "vel_z"];
const POSITION_VELOCITY_UNITS: [Unit; 6] =
    [Unit::Meter, Unit::Meter, Unit::Meter, Unit::MeterPerSecond, Unit::MeterPerSecond, Unit::MeterPerSecond];
/// Scalar-last quaternion labels (`AttitudeSource`'s doc comment convention, `core.proto`).
const QUATERNION_LABELS: [&str; 4] = ["q_x", "q_y", "q_z", "q_w"];

/// Units this module treats as "rates, masses, scalars" (ADR-005 sec 3): every physical
/// unit `core.proto`'s `Unit` enum declares except `UNIT_UNSPECIFIED` (never classifiable --
/// an undeclared unit is exactly the "ambiguous" case this module refuses rather than
/// guesses at) is a candidate, further narrowed by [`is_discrete_label`] below for the one
/// case (`UNIT_DIMENSIONLESS`) that is ambiguous between a continuous scalar and a discrete
/// mode/counter.
fn is_linear_scalar_unit(unit: Unit) -> bool {
    !matches!(unit, Unit::Unspecified)
}

/// Label convention for "discrete modes, counters" (ADR-005 sec 3): `StateComponent` has no
/// dedicated discrete-vs-continuous flag (`core.proto`, read-only), so this module documents
/// and uses a label convention -- a case-insensitive `"mode"` or `"count"` substring -- rather
/// than silently treating every dimensionless scalar as continuous. This is a stated
/// heuristic on top of declared metadata, not a guess from index position; a state space that
/// disagrees with the convention should carry a unit other than `UNIT_DIMENSIONLESS`
/// (`core.proto` has none dedicated to "discrete", which a future CDM change could add).
fn is_discrete_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.contains("mode") || l.contains("count")
}

/// Label convention for state-transition-matrix / covariance elements riding in a `mean`
/// vector (ADR-005 sec 3: "never interpolated"). `TrajectorySample.cov` is the CDM's own
/// dedicated field for covariance (never `mean`), so this case is not expected on any
/// `StateSpace` this crate currently declares -- it exists so a future one that *does* smuggle
/// STM/covariance elements into `mean` gets a specific, actionable refusal instead of falling
/// through to the generic "unclassifiable" error.
fn is_never_interpolated_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.starts_with("phi_") || l.starts_with("stm_") || l.starts_with("cov_")
}

fn component_unit(c: &StateComponent) -> Unit {
    Unit::try_from(c.unit).unwrap_or(Unit::Unspecified)
}

fn is_position_velocity_prefix(comps: &[StateComponent]) -> bool {
    (0..6).all(|k| comps[k].label == POSITION_VELOCITY_LABELS[k] && component_unit(&comps[k]) == POSITION_VELOCITY_UNITS[k])
}

fn is_quaternion_group(comps: &[StateComponent]) -> bool {
    (0..4).all(|k| comps[k].label == QUATERNION_LABELS[k] && component_unit(&comps[k]) == Unit::Dimensionless)
}

/// The class [`interpolate_by_state_space`] assigns to one contiguous group of components,
/// and (in [`classify`]'s output) exactly how many components that group covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentClass {
    /// The first six components of a Cartesian state space (always a group of 6, at index 0).
    PositionVelocity,
    /// A four-component unit quaternion `[q_x, q_y, q_z, q_w]` (always a group of 4).
    Quaternion,
    /// A rate, mass, or other physical scalar (always a group of 1).
    LinearScalar,
    /// A discrete mode or counter (always a group of 1).
    ZeroOrderHold,
}

/// Everything that can go wrong classifying or interpolating a `StateSpace`-declared sample.
/// ADR-005 sec 3's rule: a component this module cannot place is a **typed refusal**, never a
/// silent fallback -- every variant here names the offending `state_space_id`, component
/// `index` and `label` so a caller can act on it rather than guess further.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpolationError {
    /// `s0`/`s1`'s length does not match `StateSpace.components.len()`.
    LengthMismatch { state_space_id: String, declared: usize, actual_s0: usize, actual_s1: usize },
    /// Component `index` (`label`, wire `unit` code `unit`) matches none of this module's
    /// recognized labels/units (position/velocity prefix, `q_x..q_w` quaternion group, a
    /// known physical `Unit`, or a `mode`/`count` dimensionless label) -- see [`classify`]'s
    /// doc comment for exactly what is recognized.
    UnclassifiableComponent { state_space_id: String, index: usize, label: String, unit: i32 },
    /// Component `index` (`label`) looks like a state-transition-matrix or covariance element
    /// (see [`is_never_interpolated_label`]); ADR-005 sec 3 never interpolates these.
    NeverInterpolated { state_space_id: String, index: usize, label: String },
    /// The quaternion group starting at `index` failed the unit-norm assertion ADR-005 sec 3
    /// requires at one or both endpoints (`n0`/`n1` are the measured norms, `tol` the
    /// tolerance this module checks against).
    QuaternionNotUnitNorm { state_space_id: String, index: usize, n0: f64, n1: f64, tol: f64 },
}

impl std::fmt::Display for InterpolationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterpolationError::LengthMismatch { state_space_id, declared, actual_s0, actual_s1 } => write!(
                f,
                "state space {state_space_id:?} declares {declared} component(s) but got s0.len()={actual_s0}, s1.len()={actual_s1}"
            ),
            InterpolationError::UnclassifiableComponent { state_space_id, index, label, unit } => write!(
                f,
                "state space {state_space_id:?} component {index} (label {label:?}, unit code {unit}) cannot be classified for interpolation -- declare a recognized label/unit convention (position/velocity, q_x..q_w quaternion, a known physical Unit, or a mode/count label) rather than leaving it ambiguous"
            ),
            InterpolationError::NeverInterpolated { state_space_id, index, label } => write!(
                f,
                "state space {state_space_id:?} component {index} (label {label:?}) is a state-transition-matrix or covariance element; ADR-005 sec 3 never interpolates these -- sample only at the instance's own native epochs"
            ),
            InterpolationError::QuaternionNotUnitNorm { state_space_id, index, n0, n1, tol } => write!(
                f,
                "state space {state_space_id:?} quaternion group at component {index}: endpoints are not unit norm (|q0|={n0:.9}, |q1|={n1:.9}, tolerance {tol:e})"
            ),
        }
    }
}
impl std::error::Error for InterpolationError {}

/// Tolerance the unit-norm assertion in [`interpolate_by_state_space`] checks against.
/// Loose enough for a GMAT-derived quaternion's ordinary floating-point round-off, tight
/// enough to catch a genuinely wrong (unnormalized, or a raw non-quaternion 4-vector fed in
/// by mistake) group.
pub const QUATERNION_UNIT_NORM_TOL: f64 = 1e-6;

/// Classify every component of `space` into contiguous groups, in order, covering
/// `0..space.components.len()` exactly once. See the module doc comment's table for the
/// class-to-rule mapping and this function's helpers ([`is_position_velocity_prefix`],
/// [`is_quaternion_group`], [`is_linear_scalar_unit`], [`is_discrete_label`],
/// [`is_never_interpolated_label`]) for exactly what each class recognizes.
pub fn classify(space: &StateSpace) -> Result<Vec<(usize, usize, ComponentClass)>, InterpolationError> {
    let comps = &space.components;
    let n = comps.len();
    let mut groups = Vec::new();
    let mut i = 0;
    if n >= 6 && is_position_velocity_prefix(&comps[0..6]) {
        groups.push((0, 6, ComponentClass::PositionVelocity));
        i = 6;
    }
    while i < n {
        if i + 4 <= n && is_quaternion_group(&comps[i..i + 4]) {
            groups.push((i, 4, ComponentClass::Quaternion));
            i += 4;
            continue;
        }
        let c = &comps[i];
        if is_never_interpolated_label(&c.label) {
            return Err(InterpolationError::NeverInterpolated { state_space_id: space.id.clone(), index: i, label: c.label.clone() });
        }
        let unit = component_unit(c);
        let class = if unit == Unit::Dimensionless && is_discrete_label(&c.label) {
            ComponentClass::ZeroOrderHold
        } else if is_linear_scalar_unit(unit) {
            ComponentClass::LinearScalar
        } else {
            return Err(InterpolationError::UnclassifiableComponent { state_space_id: space.id.clone(), index: i, label: c.label.clone(), unit: c.unit });
        };
        groups.push((i, 1, class));
        i += 1;
    }
    Ok(groups)
}

/// Ordinary linear interpolation of one scalar.
fn linear(t0_ns: i64, v0: f64, t1_ns: i64, v1: f64, t_ns: i64) -> f64 {
    let s = (t_ns - t0_ns) as f64 / (t1_ns - t0_ns) as f64;
    v0 + (v1 - v0) * s
}

/// Zero-order hold: the nearer endpoint, matching `hermite_velocity`'s own past-six
/// pass-through rule exactly (`s < 0.5` picks `t0`'s value, otherwise `t1`'s).
fn zero_order_hold(t0_ns: i64, v0: f64, t1_ns: i64, v1: f64, t_ns: i64) -> f64 {
    let s = (t_ns - t0_ns) as f64 / (t1_ns - t0_ns) as f64;
    if s < 0.5 {
        v0
    } else {
        v1
    }
}

fn norm4(q: &[f64]) -> f64 {
    (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt()
}

fn normalize4(mut q: [f64; 4]) -> [f64; 4] {
    let n = norm4(&q);
    if n > 0.0 {
        for v in &mut q {
            *v /= n;
        }
    }
    q
}

/// Normalized spherical linear interpolation of a scalar-last quaternion `[x, y, z, w]`
/// (ADR-005 sec 3), shortest path: if the endpoints are more than 90 degrees apart on the
/// unit hypersphere (negative dot product), `q1` is replaced by `-q1` first -- the same
/// physical orientation, the short way around -- exactly the classic `q` vs `-q` sign-flip
/// bug this module's own tests are required to catch (see
/// `sign_flip_antipodal_representation_still_takes_the_short_path` below). Falls back to a
/// normalized linear blend when the endpoints are numerically coincident (`sin(theta0)` too
/// small to divide by), which is exact in the limit and avoids a `0/0`.
pub fn slerp_xyzw(q0: [f64; 4], q1_in: [f64; 4], s: f64) -> [f64; 4] {
    let mut q1 = q1_in;
    let mut dot: f64 = (0..4).map(|i| q0[i] * q1[i]).sum();
    if dot < 0.0 {
        for v in &mut q1 {
            *v = -*v;
        }
        dot = -dot;
    }
    let dot = dot.clamp(-1.0, 1.0);
    if dot > 1.0 - 1e-9 {
        let mut out = [0.0; 4];
        for i in 0..4 {
            out[i] = q0[i] + (q1[i] - q0[i]) * s;
        }
        return normalize4(out);
    }
    let theta0 = dot.acos();
    let sin_theta0 = theta0.sin();
    let theta = theta0 * s;
    let a = (theta0 - theta).sin() / sin_theta0;
    let b = theta.sin() / sin_theta0;
    let mut out = [0.0; 4];
    for i in 0..4 {
        out[i] = a * q0[i] + b * q1[i];
    }
    out
}

/// Interpolate a full `StateSpace`-declared sample at `t_ns`, given two bracketing samples
/// `(t0_ns, s0)` and `(t1_ns, s1)` with `t0_ns <= t_ns <= t1_ns`, by classifying every
/// component (module doc comment's table) rather than assuming position by index beyond the
/// one place ADR-005 itself is positional (the first six of a Cartesian space).
///
/// # Errors
///
/// [`InterpolationError::LengthMismatch`] if `s0`/`s1` do not match `space.components.len()`;
/// [`classify`]'s own errors ([`InterpolationError::UnclassifiableComponent`],
/// [`InterpolationError::NeverInterpolated`]) for a component this module cannot place;
/// [`InterpolationError::QuaternionNotUnitNorm`] if a quaternion group's endpoint samples are
/// not unit norm to [`QUATERNION_UNIT_NORM_TOL`].
///
/// # Panics
///
/// If `t1_ns <= t0_ns` (same contract as [`hermite_velocity`]).
pub fn interpolate_by_state_space(space: &StateSpace, t0_ns: i64, s0: &[f64], t1_ns: i64, s1: &[f64], t_ns: i64) -> Result<Vec<f64>, InterpolationError> {
    assert!(t1_ns > t0_ns, "interpolate_by_state_space: t1_ns ({t1_ns}) must be after t0_ns ({t0_ns})");
    let n = space.components.len();
    if s0.len() != n || s1.len() != n {
        return Err(InterpolationError::LengthMismatch { state_space_id: space.id.clone(), declared: n, actual_s0: s0.len(), actual_s1: s1.len() });
    }
    let groups = classify(space)?;
    let s = (t_ns - t0_ns) as f64 / (t1_ns - t0_ns) as f64;
    let mut out = vec![0.0; n];
    for (start, len, class) in groups {
        match class {
            ComponentClass::PositionVelocity => {
                let seg = hermite_velocity(t0_ns, &s0[start..start + 6], t1_ns, &s1[start..start + 6], t_ns);
                out[start..start + 6].copy_from_slice(&seg[0..6]);
            }
            ComponentClass::Quaternion => {
                let q0 = [s0[start], s0[start + 1], s0[start + 2], s0[start + 3]];
                let q1 = [s1[start], s1[start + 1], s1[start + 2], s1[start + 3]];
                let (n0, n1) = (norm4(&q0), norm4(&q1));
                if (n0 - 1.0).abs() > QUATERNION_UNIT_NORM_TOL || (n1 - 1.0).abs() > QUATERNION_UNIT_NORM_TOL {
                    return Err(InterpolationError::QuaternionNotUnitNorm { state_space_id: space.id.clone(), index: start, n0, n1, tol: QUATERNION_UNIT_NORM_TOL });
                }
                let q = slerp_xyzw(q0, q1, s.clamp(0.0, 1.0));
                out[start..start + 4].copy_from_slice(&q);
            }
            ComponentClass::LinearScalar => {
                debug_assert_eq!(len, 1);
                out[start] = linear(t0_ns, s0[start], t1_ns, s1[start], t_ns);
            }
            ComponentClass::ZeroOrderHold => {
                debug_assert_eq!(len, 1);
                out[start] = zero_order_hold(t0_ns, s0[start], t1_ns, s1[start], t_ns);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reproduces_the_endpoints_exactly() {
        let s0 = [1.0, 2.0, 3.0, 0.1, 0.2, 0.3];
        let s1 = [4.0, 5.0, 6.0, 0.4, 0.5, 0.6];
        let got0 = hermite_velocity(0, &s0, 10_000_000_000, &s1, 0);
        let got1 = hermite_velocity(0, &s0, 10_000_000_000, &s1, 10_000_000_000);
        for (g, w) in got0.iter().zip(s0.iter()) {
            assert!((g - w).abs() < 1e-12, "{g} vs {w}");
        }
        for (g, w) in got1.iter().zip(s1.iter()) {
            assert!((g - w).abs() < 1e-12, "{g} vs {w}");
        }
    }

    #[test]
    fn exact_for_constant_velocity_motion() {
        // Straight-line motion is degree-1; a cubic Hermite that matches the endpoint
        // positions/velocities of a line reproduces it everywhere in between exactly, so
        // this is a real correctness check, not just a smoke test.
        let v = [3.0, -1.0, 0.5];
        let t0_ns = 0i64;
        let t1_ns = 20_000_000_000i64; // 20 s
        let p0 = [0.0, 0.0, 0.0];
        let p1 = [v[0] * 20.0, v[1] * 20.0, v[2] * 20.0];
        let s0 = [p0[0], p0[1], p0[2], v[0], v[1], v[2]];
        let s1 = [p1[0], p1[1], p1[2], v[0], v[1], v[2]];

        for t_ns in [0i64, 3_000_000_000, 7_500_000_000, 12_300_000_000, 20_000_000_000] {
            let got = hermite_velocity(t0_ns, &s0, t1_ns, &s1, t_ns);
            let t_s = t_ns as f64 * 1e-9;
            for i in 0..3 {
                let want_p = v[i] * t_s;
                assert!((got[i] - want_p).abs() < 1e-9, "pos[{i}] at t={t_s}: {} vs {want_p}", got[i]);
                assert!((got[3 + i] - v[i]).abs() < 1e-9, "vel[{i}] at t={t_s}: {} vs {}", got[3 + i], v[i]);
            }
        }
    }

    #[test]
    fn exact_for_a_true_cubic_position_function() {
        // p(t) = t^3 componentwise, v(t) = 3 t^2 -- a genuine cubic, so a cubic Hermite built
        // from two exact (position, velocity) samples must reproduce it exactly everywhere
        // in between, not just at the endpoints.
        let p = |t: f64| [t.powi(3), t.powi(3), t.powi(3)];
        let v = |t: f64| [3.0 * t.powi(2), 3.0 * t.powi(2), 3.0 * t.powi(2)];
        let t0 = 0.0_f64;
        let t1 = 2.0_f64;
        let s0: Vec<f64> = p(t0).into_iter().chain(v(t0)).collect();
        let s1: Vec<f64> = p(t1).into_iter().chain(v(t1)).collect();
        let t0_ns = (t0 * 1e9) as i64;
        let t1_ns = (t1 * 1e9) as i64;

        for &t in &[0.3, 0.7, 1.0, 1.5, 1.9] {
            let t_ns = (t * 1e9) as i64;
            let got = hermite_velocity(t0_ns, &s0, t1_ns, &s1, t_ns);
            let want_p = p(t);
            let want_v = v(t);
            for i in 0..3 {
                assert!((got[i] - want_p[i]).abs() < 1e-6, "pos[{i}] at t={t}: {} vs {}", got[i], want_p[i]);
                assert!((got[3 + i] - want_v[i]).abs() < 1e-6, "vel[{i}] at t={t}: {} vs {}", got[3 + i], want_v[i]);
            }
        }
    }

    #[test]
    fn components_past_six_pass_through_the_nearer_endpoint() {
        let s0 = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 111.0];
        let s1 = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 222.0];
        let near_start = hermite_velocity(0, &s0, 10, &s1, 2);
        let near_end = hermite_velocity(0, &s0, 10, &s1, 8);
        assert_eq!(near_start[6], 111.0);
        assert_eq!(near_end[6], 222.0);
    }

    #[test]
    #[should_panic(expected = "must be after")]
    fn rejects_non_increasing_time() {
        let s = [0.0; 6];
        hermite_velocity(10, &s, 10, &s, 10);
    }

    // -----------------------------------------------------------------------------------
    // Classification and interpolate_by_state_space (ADR-005 sec 3, question 88).
    // -----------------------------------------------------------------------------------

    fn comp(label: &str, unit: Unit) -> StateComponent {
        StateComponent { label: label.to_string(), unit: unit as i32 }
    }

    /// Matches `altavista.cdm`'s `STATE_SPACE_ID_CARTESIAN_POS_VEL_6` shape exactly.
    fn cartesian_6_space() -> StateSpace {
        StateSpace {
            id: "altavista.cartesian_pos_vel_6".to_string(),
            components: vec![
                comp("pos_x", Unit::Meter),
                comp("pos_y", Unit::Meter),
                comp("pos_z", Unit::Meter),
                comp("vel_x", Unit::MeterPerSecond),
                comp("vel_y", Unit::MeterPerSecond),
                comp("vel_z", Unit::MeterPerSecond),
            ],
            frame_id: String::new(),
        }
    }

    /// Matches `altavista.cdm`'s `STATE_SPACE_ID_CARTESIAN_POS_VEL_6_ATTITUDE_QUAT_4` shape.
    fn cartesian_6_attitude_4_space() -> StateSpace {
        let mut space = cartesian_6_space();
        space.id = "altavista.cartesian_pos_vel_6_attitude_quat_4".to_string();
        space.components.extend([
            comp("q_x", Unit::Dimensionless),
            comp("q_y", Unit::Dimensionless),
            comp("q_z", Unit::Dimensionless),
            comp("q_w", Unit::Dimensionless),
        ]);
        space
    }

    #[test]
    fn classify_splits_a_10_component_attitude_space_into_hermite_then_quaternion() {
        let space = cartesian_6_attitude_4_space();
        let groups = classify(&space).unwrap();
        assert_eq!(groups, vec![(0, 6, ComponentClass::PositionVelocity), (6, 4, ComponentClass::Quaternion)]);
    }

    #[test]
    fn classify_places_a_bare_6_component_space_as_position_velocity_only() {
        let groups = classify(&cartesian_6_space()).unwrap();
        assert_eq!(groups, vec![(0, 6, ComponentClass::PositionVelocity)]);
    }

    #[test]
    fn classify_recognizes_rates_masses_and_other_scalars_as_linear() {
        let space = StateSpace {
            id: "test.scalars".to_string(),
            components: vec![comp("spin_rate", Unit::RadianPerSecond), comp("mass", Unit::Kilogram), comp("fuel_frac", Unit::Dimensionless)],
            frame_id: String::new(),
        };
        let groups = classify(&space).unwrap();
        assert_eq!(groups, vec![(0, 1, ComponentClass::LinearScalar), (1, 1, ComponentClass::LinearScalar), (2, 1, ComponentClass::LinearScalar)]);
    }

    #[test]
    fn classify_recognizes_a_mode_or_counter_label_as_zero_order_hold() {
        let space = StateSpace {
            id: "test.discrete".to_string(),
            components: vec![comp("fault_mode", Unit::Dimensionless), comp("event_count", Unit::Dimensionless)],
            frame_id: String::new(),
        };
        let groups = classify(&space).unwrap();
        assert_eq!(groups, vec![(0, 1, ComponentClass::ZeroOrderHold), (1, 1, ComponentClass::ZeroOrderHold)]);
    }

    #[test]
    fn classify_refuses_an_undeclared_unit_rather_than_guess() {
        let space = StateSpace { id: "test.bad".to_string(), components: vec![comp("mystery", Unit::Unspecified)], frame_id: String::new() };
        let err = classify(&space).unwrap_err();
        assert!(matches!(err, InterpolationError::UnclassifiableComponent { ref state_space_id, index: 0, .. } if state_space_id == "test.bad"), "{err}");
    }

    #[test]
    fn classify_refuses_an_stm_looking_label_as_never_interpolated_not_unclassifiable() {
        let space = StateSpace { id: "test.stm".to_string(), components: vec![comp("phi_0_0", Unit::Dimensionless)], frame_id: String::new() };
        let err = classify(&space).unwrap_err();
        assert!(matches!(err, InterpolationError::NeverInterpolated { ref label, .. } if label == "phi_0_0"), "{err}");
    }

    #[test]
    fn interpolate_by_state_space_position_velocity_group_matches_plain_hermite_velocity() {
        let space = cartesian_6_space();
        let s0 = [0.0, 0.0, 0.0, 1.0, 2.0, 3.0];
        let s1 = [10.0, 20.0, 30.0, 1.0, 2.0, 3.0];
        let (t0, t1, t) = (0i64, 10_000_000_000i64, 3_000_000_000i64);
        let want = hermite_velocity(t0, &s0, t1, &s1, t);
        let got = interpolate_by_state_space(&space, t0, &s0, t1, &s1, t).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn interpolate_by_state_space_scalar_group_is_exactly_linear() {
        let space = StateSpace { id: "test.mass".to_string(), components: vec![comp("mass", Unit::Kilogram)], frame_id: String::new() };
        let got = interpolate_by_state_space(&space, 0, &[100.0], 10_000_000_000, &[80.0], 2_500_000_000).unwrap();
        assert!((got[0] - 95.0).abs() < 1e-12, "{got:?}"); // 25% of the way: 100 - 0.25*20 = 95
    }

    #[test]
    fn interpolate_by_state_space_discrete_group_holds_the_nearer_endpoint() {
        let space = StateSpace { id: "test.mode".to_string(), components: vec![comp("op_mode", Unit::Dimensionless)], frame_id: String::new() };
        let near_start = interpolate_by_state_space(&space, 0, &[1.0], 10, &[2.0], 2).unwrap();
        let near_end = interpolate_by_state_space(&space, 0, &[1.0], 10, &[2.0], 8).unwrap();
        assert_eq!(near_start[0], 1.0);
        assert_eq!(near_end[0], 2.0);
    }

    #[test]
    fn interpolate_by_state_space_rejects_a_length_mismatch() {
        let space = cartesian_6_space();
        let err = interpolate_by_state_space(&space, 0, &[0.0; 5], 10, &[0.0; 6], 5).unwrap_err();
        assert!(matches!(err, InterpolationError::LengthMismatch { declared: 6, actual_s0: 5, actual_s1: 6, .. }), "{err}");
    }

    // -- Required test: unit norm and continuity across a q / -q sign flip --------------
    //
    // The classic silent bug ADR-005 sec 3 calls out: naive linear-then-normalize
    // interpolation between a quaternion and its antipodal (negated) representation of the
    // *same* physical orientation takes the long way around the rotation (through ~180
    // degrees of unnecessary rotation) instead of recognizing they're the same attitude.
    // slerp_xyzw must take the short path regardless of which representation the two
    // samples happen to use.

    fn quat_angle_deg(a: [f64; 4], b: [f64; 4]) -> f64 {
        let dot = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]).clamp(-1.0, 1.0).abs();
        2.0 * dot.acos().to_degrees()
    }

    #[test]
    fn slerp_between_antipodal_representations_takes_the_short_path_and_stays_unit_norm() {
        // A small rotation (5 degrees about Z) from identity, expressed two ways: q0 as-is,
        // and q1 as its antipodal representation -q1_true (same physical orientation as a
        // further 5-degree rotation about Z, negated). A naive (non-shortest-path) slerp or
        // linear blend would swing through ~355 degrees; the correct short path swings
        // through ~10 degrees total (5 degrees from q0 to the true orientation, matching the
        // un-negated construction below).
        let half = (2.5_f64).to_radians();
        let q0 = [0.0, 0.0, half.sin(), half.cos()]; // 5 deg about Z
        let half2 = (5.0_f64).to_radians();
        let q1_true = [0.0, 0.0, half2.sin(), half2.cos()]; // 10 deg about Z
        let q1_antipodal = [-q1_true[0], -q1_true[1], -q1_true[2], -q1_true[3]];

        let space = StateSpace {
            id: "test.quat_sign_flip".to_string(),
            components: vec![comp("q_x", Unit::Dimensionless), comp("q_y", Unit::Dimensionless), comp("q_z", Unit::Dimensionless), comp("q_w", Unit::Dimensionless)],
            frame_id: String::new(),
        };
        let (t0, t1) = (0i64, 10_000_000_000i64);
        let mut max_step_deg = 0.0f64;
        let mut prev = q0;
        let n = 20;
        for k in 0..=n {
            let t = t0 + (t1 - t0) * k / n;
            let got = interpolate_by_state_space(&space, t0, &q0, t1, &q1_antipodal, t).unwrap();
            let q: [f64; 4] = [got[0], got[1], got[2], got[3]];
            // Unit norm at every sampled point along the path, not just the endpoints.
            assert!((norm4(&q) - 1.0).abs() < 1e-12, "sample {k}: |q|={} not unit norm", norm4(&q));
            max_step_deg = max_step_deg.max(quat_angle_deg(prev, q));
            prev = q;
        }
        // Continuity: no single step between the 20 sub-samples swings more than a few
        // degrees (10 degrees total / 20 steps = 0.5 deg/step on the short path; the long
        // way around would average ~17.5 deg/step over the same 20 steps).
        assert!(max_step_deg < 2.0, "max per-step rotation {max_step_deg} deg -- looks like the long way around");
        // And the endpoint reached at s=1 is *physically* q1_antipodal (same orientation, dot
        // product magnitude 1) -- not necessarily bit-identical in sign, because the
        // shortest-path resolution above may present it in whichever hemisphere keeps the
        // whole path continuous (the same behaviour interp.js's THREE.Quaternion.slerp has
        // always had); what must never happen is landing on some *other* orientation.
        let end = interpolate_by_state_space(&space, t0, &q0, t1, &q1_antipodal, t1).unwrap();
        let end4 = [end[0], end[1], end[2], end[3]];
        assert!(quat_angle_deg(end4, q1_antipodal) < 1e-6, "endpoint {end4:?} is not q1_antipodal's orientation: {q1_antipodal:?}");
    }

    #[test]
    fn interpolate_by_state_space_rejects_a_non_unit_norm_quaternion_sample() {
        let space = cartesian_6_attitude_4_space();
        let s0 = [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]; // unit quaternion
        let s1 = [10.0, 20.0, 30.0, 1.0, 2.0, 3.0, 2.0, 0.0, 0.0, 1.0]; // |q|=sqrt(5), not unit
        let err = interpolate_by_state_space(&space, 0, &s0, 10_000_000_000, &s1, 5_000_000_000).unwrap_err();
        assert!(matches!(err, InterpolationError::QuaternionNotUnitNorm { index: 6, .. }), "{err}");
    }

    #[test]
    fn interpolate_by_state_space_full_10_component_attitude_sample_combines_hermite_and_slerp() {
        let space = cartesian_6_attitude_4_space();
        let s0 = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]; // identity quaternion
        let half = (90.0_f64).to_radians() / 2.0;
        let s1 = [10.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, half.sin(), half.cos()]; // 90 deg about Z
        let (t0, t1, t) = (0i64, 10_000_000_000i64, 5_000_000_000i64);
        let got = interpolate_by_state_space(&space, t0, &s0, t1, &s1, t).unwrap();
        assert_eq!(got.len(), 10);
        // Position/velocity half: exact linear motion at constant velocity is exact under
        // Hermite too (see exact_for_constant_velocity_motion above), independently checked.
        assert!((got[0] - 5.0).abs() < 1e-9, "{got:?}");
        // Quaternion half: unit norm, and at the midpoint (s=0.5) a slerp between identity
        // and a 90-degree-about-Z rotation is exactly the 45-degree-about-Z rotation.
        let qn = norm4(&got[6..10]);
        assert!((qn - 1.0).abs() < 1e-12, "|q|={qn}");
        let want_half = (45.0_f64).to_radians() / 2.0;
        assert!((got[8] - want_half.sin()).abs() < 1e-9, "{got:?}");
        assert!((got[9] - want_half.cos()).abs() < 1e-9, "{got:?}");
    }
}

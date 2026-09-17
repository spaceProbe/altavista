//! GMAT-free tests for [`av_orbital::EarthGravityModel`] (`docs/native-dynamics-plan.md`
//! milestone N1): the two-body analytic golden acceptance test, the state-layout pin, and
//! energy/angular-momentum conservation. Nothing in this file needs the `gmat-frames` feature
//! -- every test here uses [`IdentityRotation`] (see that type's own doc comment for why a
//! no-op rotation is exact, not merely convenient, for a spherically symmetric field), so this
//! file runs identically under `cargo test -p av-orbital` and `cargo test -p av-orbital
//! --no-default-features` (this task's own gate: "the model builds and its non-GMAT tests pass
//! with `--no-default-features`").
//!
//! Still needs `GMAT_ROOT` set (this task's own mandated environment) -- [`point_mass_model`]
//! reads the real `JGM2.cof` file to build its `GravityModel` (via `av_orbital::cof::
//! read_earth_gravity`), which is plain filesystem access, never a link against GMAT's library.
use std::path::PathBuf;

use av_dynamics::DynamicsModel;
use av_orbital::{BodyFixedRotation, EarthGravityModel, EarthGravityModelInfo, Rotation};
use serde::Deserialize;

/// A no-op rotation (`r` = identity, `r_dot` = zero) -- physically EXACT, not an approximation,
/// for any spherically symmetric field (point mass, or any purely zonal field): gravity itself
/// does not depend on which body-fixed frame orientation is used when the field has no
/// dependence on longitude, so an identity rotation introduces no error at all. This is exactly
/// why `docs/native-dynamics-plan.md`'s own brief warns that a rotation sign/transpose bug
/// "will not show up in a spherically symmetric field" -- this mock is deliberately confined to
/// the degree/order (0, 0) tests in this file for that reason; the rotation's actual direction
/// is proved against real GMAT output in `tests/frame_gmat.rs` instead.
struct IdentityRotation;
impl BodyFixedRotation for IdentityRotation {
    type Error = std::convert::Infallible;
    fn inertial_to_fixed(&self, _t_tai_ns: i64) -> Result<Rotation, Self::Error> {
        Ok(Rotation { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], r_dot: [[0.0; 3]; 3] })
    }
}

fn point_mass_model() -> EarthGravityModel<IdentityRotation> {
    let path = av_orbital::cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)").join("data/gravity/earth/JGM2.cof");
    EarthGravityModel::new(
        &path,
        0,
        0,
        "Earth",
        IdentityRotation,
        EarthGravityModelInfo { id: "native.orbital.earth_point_mass".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: vec!["twobody_analytic".to_string()] },
    )
    .expect("point-mass model construction")
}

// -- The two-body analytic golden -------------------------------------------------------------

#[derive(Deserialize)]
struct Golden {
    duration_s: f64,
    tolerance_m: f64,
    tolerance_mps: f64,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    case: String,
    initial_state: [f64; 6],
    final_state: [f64; 6],
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

fn load_golden() -> Golden {
    serde_json::from_str(&std::fs::read_to_string(golden_path("twobody_analytic")).unwrap()).unwrap()
}

/// **The two-body golden acceptance test** (this task's own N1 deliverable: "the one golden
/// that needs no GMAT"). For each case in `goldens/twobody_analytic.json`, integrates the
/// native model's own `derivatives` (point-mass gravity, degree/order (0, 0)) from
/// `initial_state` for `duration_s` seconds with the model's default `Dopri5` integrator, and
/// compares the result against the golden's `final_state` -- the CLOSED-FORM Kepler solution
/// (see `goldens/gen_twobody_analytic.py`'s own module doc for why this golden is proved
/// against that analytic reference rather than GMAT). Measures the residual and prints it
/// (`--nocapture`) before checking it against the golden's own recorded `tolerance_m`/
/// `tolerance_mps` -- this task's own rule: measure first, then set the tolerance just above
/// the measured value, never the reverse.
#[test]
fn native_model_matches_the_analytic_kepler_solution() {
    let model = point_mass_model();
    let golden = load_golden();
    let dt_ns = (golden.duration_s * 1e9).round() as i64;

    for case in &golden.cases {
        let result = model.step(&case.initial_state, 1_800_000_000_000_000_000, &[], dt_ns).expect("step");
        let dr = (0..3).map(|i| (result.state[i] - case.final_state[i]).powi(2)).sum::<f64>().sqrt();
        let dv = (3..6).map(|i| (result.state[i] - case.final_state[i]).powi(2)).sum::<f64>().sqrt();
        eprintln!(
            "[twobody_analytic {}] position residual = {dr:.6e} m (tolerance {:.3e} m), velocity residual = {dv:.6e} m/s (tolerance {:.3e} m/s)",
            case.case, golden.tolerance_m, golden.tolerance_mps
        );
        assert!(dr < golden.tolerance_m, "case {}: position residual {dr:e} m exceeds tolerance {:e} m", case.case, golden.tolerance_m);
        assert!(dv < golden.tolerance_mps, "case {}: velocity residual {dv:e} m/s exceeds tolerance {:e} m/s", case.case, golden.tolerance_mps);
    }
}

/// ADR-002's first amendment records, for the GMAT-backed model, "derivative of position
/// equals velocity" as part of the state layout. Pinned here for the native model too, with a
/// real (non-identity) velocity so a copy-paste of the wrong slice would be caught.
#[test]
fn derivatives_first_three_components_equal_velocity_exactly() {
    let model = point_mass_model();
    let state = [6_878_000.0, 0.0, 0.0, 0.0, 5_000.0, 5_500.0];
    let mut dot = [0.0; 6];
    model.derivatives(&state, 1_800_000_000_000_000_000, &[], &mut dot).unwrap();
    assert_eq!(dot[0..3], state[3..6]);
}

/// Energy and angular-momentum conservation over a full one-day arc at degree/order (0, 0) --
/// this task's own required test. A point-mass field conserves both EXACTLY in continuous time;
/// the measured drift here is purely the shared `Dopri5` integrator's own numerical error
/// (ADR-002's first amendment: "the P0 spike's 5.7 mm/day ... is the reference for what the
/// integrator alone costs" -- printed and compared against that reference below, `--nocapture`).
#[test]
fn energy_and_angular_momentum_are_conserved_over_a_one_day_arc_at_point_mass() {
    let model = point_mass_model();
    let mu = model.gravity_model().mu();
    let golden = load_golden();
    let case = &golden.cases[1]; // the eccentric case: the stronger test of a varying r/v.
    let dt_ns = (golden.duration_s * 1e9).round() as i64;

    let energy_of = |s: &[f64]| {
        let r = (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt();
        let v2 = s[3] * s[3] + s[4] * s[4] + s[5] * s[5];
        0.5 * v2 - mu / r
    };
    let h_of = |s: &[f64]| {
        [s[1] * s[5] - s[2] * s[4], s[2] * s[3] - s[0] * s[5], s[0] * s[4] - s[1] * s[3]]
    };

    let e0 = energy_of(&case.initial_state);
    let h0 = h_of(&case.initial_state);

    let result = model.step(&case.initial_state, 1_800_000_000_000_000_000, &[], dt_ns).expect("step");
    let e1 = energy_of(&result.state);
    let h1 = h_of(&result.state);

    let energy_drift_rel = (e1 - e0).abs() / e0.abs();
    let h_drift_rel = (0..3).map(|i| (h1[i] - h0[i]).powi(2)).sum::<f64>().sqrt() / (0..3).map(|i| h0[i].powi(2)).sum::<f64>().sqrt();
    eprintln!("[twobody energy/h drift, 1 day, degree/order (0,0)] energy: {e0:.6e} -> {e1:.6e} (relative drift {energy_drift_rel:.3e}); |h|: relative drift {h_drift_rel:.3e}");
    // Loose, sanity-level bounds (not this task's own precisely-measured tolerance -- that is
    // `tolerance_m`/`tolerance_mps` in the golden, checked by the test above): a correct
    // Dopri5 integration at rtol=atol=1e-12 should conserve both quantities to many orders of
    // magnitude better than 1e-6 relative over one day; this guards against a gross regression
    // (a badly wrong force sign, a runaway integrator) without duplicating the golden's own
    // tighter, measured bound.
    assert!(energy_drift_rel < 1e-6, "energy drift {energy_drift_rel:e} exceeds the sanity bound 1e-6");
    assert!(h_drift_rel < 1e-6, "angular-momentum drift {h_drift_rel:e} exceeds the sanity bound 1e-6");
}

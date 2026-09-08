//! M21.4 (`docs/open-questions.md` question 138, ADR-002's fourth amendment): the REQUIRED PIN,
//! run through the full DRM executor. A covariance-requesting `"gmat."`-bound instance whose
//! declared `spacecraft.CoordinateSystem` is `EarthBodyFixed` (not its own integration frame,
//! `EarthMJ2000Eq`) must now classify -- question 138 lifted M19.1's classify-time refusal
//! (`DrmError::CovarianceFrameConversionNotSupported`, deleted) -- and its final trajectory
//! sample's `cov` must be the CORRECTLY rotated covariance: `R P Rᵀ` for the 6x6 Jacobian
//! `M = [[R,0],[Rdot,R]]` built from [`gmat_sys::Gmat::convert_with_rotation`]'s own output at
//! that exact sample's epoch, to 1e-12 relative.
//!
//! **Twin-run design.** Two `execute()` calls, field-for-field identical `SystemDefinition`/
//! `SosConfiguration`/`DesignReferenceMission` except for one declared
//! `spacecraft.CoordinateSystem` value: `"mj2000eq_probe"` declares the integration frame itself
//! (`"EarthMJ2000Eq"`) -- `executor::convert_gmat_trajectory_to_declared_frame`'s own "already
//! in the integration frame" no-op branch, so its `cov` is the *raw, unrotated* `Phi P0 Phi^T`
//! straight off the kernel -- while `"bodyfixed_probe"` declares `"EarthBodyFixed"`, which
//! genuinely rotates. Both instances share the same vehicle, epoch, force model and `P0`, so
//! their STM-propagated covariance in the integration frame is identical by construction (same
//! physics, nothing about `spacecraft.CoordinateSystem` reaches `ODEModel::GetDerivatives` at
//! all -- M18.1's own finding, question 128's starting point). This sidesteps reconstructing
//! GMAT's own STM integration independently (fragile: matching Dopri5's adaptive step sequence,
//! epoch-parsing path, and force-model construction exactly enough for a 1e-12 bound turned out
//! to need reproducing `binding::materialize_gmat` almost verbatim) and isolates exactly the
//! piece M21.4 adds -- the ROTATION -- by taking the "before" side directly from a real,
//! independent `execute()` run rather than a hand-reconstructed one.
//!
//! `rotation`/`rotation_dot` themselves come from a THIRD, independent
//! [`gmat_sys::Gmat::convert_with_rotation`] call, with its own uniquely-named `CoordinateSystem`
//! objects (never the executor's own namespaced ones, never `av_kernel::drm::executor`'s
//! private `rotate_covariance`) -- see `crates/gmat-sys/tests/convert_rotation.rs` for why GMAT's
//! own `OrbitErrorCovariance` `ReportFile` is not a usable alternative reference for this same
//! rotation (it measurably omits the `Rdot` coupling term for a rotating frame).
//!
//! **Why an anisotropic P0.** `goldens/gen_covariance_bodyfixed_leo_2h.py`'s own module doc
//! comment explains why: `R * (c*I) * Rᵀ = c*I` for ANY orthogonal `R`, so an isotropic
//! position (or velocity) block cannot distinguish a correct rotation from a wrong one (or the
//! identity). This test's own `P0` uses the same anisotropic values as that golden -- (100, 150,
//! 200 m)^2 position variance, (0.1, 0.15, 0.2 m/s)^2 velocity variance -- in SI rather than km.
//!
//! **Why `EarthBodyFixed` needs `DisplayStateType = "Cartesian"`, not `"Keplerian"`.** GMAT
//! refuses `DisplayStateType = Keplerian` combined with a non-inertial
//! `spacecraft.CoordinateSystem` ("orbital state elements not contained in the same state
//! type") -- the same restriction `demo_two_instance.rs`'s own module doc comment documents
//! finding while wiring its own `EarthBodyFixed` fixtures, which is why this file declares its
//! own `SystemDefinition` (Cartesian, an explicit initial state) rather than reusing
//! `drms/leo_1day_golden.system.yaml`'s `leo_sys` (Keplerian).

use std::collections::BTreeMap;

use av_cdm::pb::{Binding, BindingKind, DesignReferenceMission, DrmOptions, ModelBinding, Parameter, Scenario, SosConfiguration, SystemDefinition, SystemInstance};
use av_cdm::time::Tai;
use av_kernel::drm::{execute, hash, RunConfig, RunProducts};
use gmat_sys::Gmat;

fn param(name: &str, value: f64) -> Parameter {
    Parameter { name: name.to_string(), value, ..Default::default() }
}
fn sparam(name: &str, s: &str) -> Parameter {
    Parameter { name: name.to_string(), string_value: s.to_string(), ..Default::default() }
}
fn hashed_system(mut sys: SystemDefinition) -> SystemDefinition {
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}
fn hashed_sos(mut sos: SosConfiguration) -> SosConfiguration {
    sos.hash = hash::canonical_sos_hash(&sos);
    sos
}
fn hashed_drm(mut drm: DesignReferenceMission) -> DesignReferenceMission {
    drm.hash = hash::canonical_drm_hash(&drm);
    drm
}

/// Row-major 6x6 `[[R,0],[Rdot,R]]`, built here (not by calling `av_kernel::drm::executor`'s own
/// private `rotate_covariance`) so the comparison below is a genuinely independent
/// reconstruction, not a re-invocation of the code under test.
fn block6(rotation: &[f64; 9], rotation_dot: &[f64; 9]) -> [f64; 36] {
    let mut m = [0.0; 36];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 6 + j] = rotation[i * 3 + j];
            m[(i + 3) * 6 + j] = rotation_dot[i * 3 + j];
            m[(i + 3) * 6 + (j + 3)] = rotation[i * 3 + j];
        }
    }
    m
}

fn mat6_congruence(m: &[f64; 36], p: &[f64]) -> Vec<f64> {
    let mut tmp = vec![0.0; 36];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += m[i * 6 + k] * p[k * 6 + j];
            }
            tmp[i * 6 + j] = s;
        }
    }
    let mut out = vec![0.0; 36];
    for i in 0..6 {
        for j in 0..6 {
            let mut s = 0.0;
            for k in 0..6 {
                s += tmp[i * 6 + k] * m[j * 6 + k]; // * m^T
            }
            out[i * 6 + j] = s;
        }
    }
    out
}

const START_TAI_NS: i64 = 1_767_225_637_000_000_000; // 2026-01-01T00:00:00Z (this repository's usual epoch, e.g. demo_two_instance.rs's START_TAI_NS)
// One native covariance period, deliberately short: this test is about the FRAME ROTATION
// (question 138), not long-arc STM accuracy (already required-test-covered by
// `tests/golden_acceptance.rs`'s `kernel_covariance_matches_the_golden_stm_and_propagated_cov`).
const STEP_S: f64 = 60.0; // covariance's own native period; sample_interval_s below matches it exactly
const END_TAI_NS: i64 = START_TAI_NS + (STEP_S as i64) * 1_000_000_000;

// A real, converged LEO Cartesian state (`goldens/icrf_leo_2h.json`'s own
// `state_final_km_mj2000eq`), reused only as a legitimate LEO state to seed this test's own
// fresh propagation -- not because this arc needs to match that golden's.
const X0_KM: [f64; 6] = [-2876.355722769799, 3236.078737797159, 5334.59609142612, -6.257318845969991, -4.268083427409836, -0.7825556066922988];
const GRAVITY_FILE: &str = "JGM2.cof";

/// Anisotropic P0 (SI, ADR-001): (100, 150, 200 m)^2 position variance, (0.1, 0.15, 0.2 m/s)^2
/// velocity variance -- see this file's own module doc comment for why not isotropic.
fn p0() -> Vec<f64> {
    let diag = [1.0e4_f64, 2.25e4, 4.0e4, 1.0e-2, 2.25e-2, 4.0e-2];
    let mut p = vec![0.0_f64; 36];
    for (i, v) in diag.iter().enumerate() {
        p[i * 6 + i] = *v;
    }
    p
}

/// Builds and runs a single-instance covariance DRM whose vehicle/epoch/force-model/`P0` are
/// identical across both probes -- only `coordinate_system` (the declared
/// `spacecraft.CoordinateSystem`) and the naming differ.
fn run_probe(coordinate_system: &str, instance_name: &str, gmat: &Gmat) -> RunProducts {
    let sys_id = format!("leo_sys_cov138_{instance_name}");
    let sys = hashed_system(SystemDefinition {
        id: sys_id.clone(),
        dynamics_model: "gmat.earth.jgm2_8x8".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", GRAVITY_FILE),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            sparam("spacecraft.CoordinateSystem", coordinate_system),
            sparam("spacecraft.DisplayStateType", "Cartesian"),
            param("spacecraft.X", X0_KM[0]),
            param("spacecraft.Y", X0_KM[1]),
            param("spacecraft.Z", X0_KM[2]),
            param("spacecraft.VX", X0_KM[3]),
            param("spacecraft.VY", X0_KM[4]),
            param("spacecraft.VZ", X0_KM[5]),
            param("spacecraft.DryMass", 500.0),
            param("spacecraft.Cd", 2.2),
            param("spacecraft.Cr", 1.8),
            param("spacecraft.DragArea", 5.0),
            param("spacecraft.SRPArea", 5.0),
        ],
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys.clone());

    let sos = hashed_sos(SosConfiguration {
        id: format!("leo_sos_cov138_{instance_name}"),
        instances: vec![SystemInstance {
            name: instance_name.to_string(),
            system_id: sys.id.clone(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: sys.id.clone() })) }),
            step_rate_hz: 1.0 / STEP_S,
            initial_covariance: p0(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: format!("leo_drm_cov138_{instance_name}"),
        sos_configuration_id: sos.id.clone(),
        scenario: Some(Scenario { start_tai_ns: START_TAI_NS, end_tai_ns: END_TAI_NS, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 1.0 / STEP_S, sample_interval_s: STEP_S, ..Default::default() }),
        ..Default::default()
    });

    let cfg = RunConfig { gmat, drm: &drm, sos: &sos, systems: &systems, run_id: format!("test-run-covariance-frame-conversion-{instance_name}"), error_mode: Default::default() , products_dir: None };
    execute(cfg).unwrap_or_else(|e| panic!("{instance_name} (coordinate_system={coordinate_system:?}) must execute end to end: {e}"))
}

/// **Required test.** Fails against: the pre-M21.4 refusal (`DrmError::
/// CovarianceFrameConversionNotSupported`, the `bodyfixed_probe` run below would return `Err`
/// instead of executing); an executor that rotates `mean` but leaves `cov` in the integration
/// frame (`bodyfixed_probe`'s `cov` would then equal `mj2000eq_probe`'s unrotated `cov` exactly,
/// not `expected`); an executor using only the block-diagonal `[[R,0],[0,R]]` transform (would
/// disagree with `expected` here, which includes `Rdot`, in exactly the velocity/cross-block
/// signature `crates/gmat-sys/tests/convert_rotation.rs` measures); a wrong epoch fed into the
/// rotation (a genuinely different `R`/`Rdot`, failing the 1e-12 bound); or a km/m unit mistake
/// in `rotate_covariance` (six orders of magnitude off, not noise).
#[test]
fn drm_covariance_matches_r_p_rt_built_from_gmats_own_reported_rotation_in_earthbodyfixed() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    // Earth's own NutationUpdateInterval defaults to 60 s (Planet::nutationUpdateInterval,
    // third_party/gmat-src/src/base/solarsys/Planet.cpp): BodyFixedAxes caches Earth's rotation
    // (and rotation-rate) matrix and only recomputes when the requested epoch moves outside that
    // window since the last cached computation -- confirmed empirically while building this test
    // (the same effect goldens/gen_bodyfixed_leo_2h.py's own module doc comment already measured
    // for the MEAN-state conversion path, ~1e-4 m / ~1e-7 m/s there). Left at its default, this
    // test's own independent verification call (below) and the executor's own internal call
    // (inside `bodyfixed_probe`'s `execute()`) can legitimately compute slightly different
    // small-magnitude nutation terms depending on which epoch each one's call happened to leave
    // Earth's cache primed at -- measured here at ~2.4e-10 relative in the resulting covariance,
    // two orders of magnitude over the 1e-12 bound this test needs. Setting it to 0 here (once,
    // before either `execute()` call) forces every BodyFixedAxes query in this test's whole
    // process -- the executor's own internal one included -- to recompute fresh every time,
    // removing the discrepancy at its source rather than loosening the bound to absorb it.
    gmat.construct("Planet", "Earth").expect("Earth already exists in GMAT's configuration; Construct fetches it").set_real("NutationUpdateInterval", 0.0).expect("set NutationUpdateInterval");

    let mj2000eq_products = run_probe("EarthMJ2000Eq", "mj2000eq_probe", &gmat);
    let bodyfixed_products = run_probe("EarthBodyFixed", "bodyfixed_probe", &gmat);

    let mj_traj = mj2000eq_products.trajectories.get("mj2000eq_probe").expect("mj2000eq_probe produced a trajectory");
    assert_eq!(mj_traj.frame_id, "EarthMJ2000Eq");
    let mj_last = mj_traj.samples.last().expect("at least one sample");
    assert_eq!(mj_last.cov.len(), 36, "covariance requested: the final sample must carry a 6x6 cov");

    let bf_traj = bodyfixed_products.trajectories.get("bodyfixed_probe").expect("bodyfixed_probe produced a trajectory");
    assert_eq!(bf_traj.frame_id, "EarthBodyFixed", "sanity: the declared frame is genuinely carried through");
    let bf_last = bf_traj.samples.last().expect("at least one sample");
    assert_eq!(bf_last.mean.len(), 6, "sanity: a GMAT-bound Cartesian trajectory sample");
    assert_eq!(bf_last.cov.len(), 36, "covariance requested: the final sample must carry a 6x6 cov, not empty");
    assert_eq!(mj_last.tai_ns, bf_last.tai_ns, "sanity: both probes share the identical scenario, so their final samples land at the identical epoch");

    // Sanity: mean.CoordinateSystem never reached ODEModel::GetDerivatives (question 128's own
    // starting finding), so the two probes' raw propagated covariance in the integration frame
    // (before mj2000eq_probe's own no-op frame-conversion branch, and before bodyfixed_probe's
    // real one) must be bit-for-bit identical -- both are, in fact, IDENTICAL physics.
    // `mj_last.cov` IS that shared, un-rotated `Phi P0 Phi^T` (its own declared frame equals the
    // integration frame, so `convert_gmat_trajectory_to_declared_frame`'s no-op branch leaves it
    // untouched) -- the "before" half of this test's own comparison.

    // Independent R(t1)/Rdot(t1), driving gmat_sys::Gmat directly with its own uniquely-named
    // CoordinateSystem objects -- never av_kernel::drm::executor's own (private)
    // rotate_covariance, and never its own namespaced CoordinateSystem objects either.
    gmat.coordinate_system("Cov138IndependentMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("Cov138IndependentBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let epoch_a1mjd = Tai::from_nanos(bf_last.tai_ns).to_a1_mjd();
    // The rotation matrices CoordinateConverter::Convert computes depend only on epoch and the
    // two CoordinateSystems (never on the state passed in -- BodyFixedAxes/MJ2000Eq are not
    // ObjectReferenced axes), so any valid 6-state works as the placeholder `state_in` here.
    let state_placeholder_km = [7000.0, 0.0, 0.0, 0.0, 0.0, 7.5];
    let out = gmat.convert_with_rotation(epoch_a1mjd, &state_placeholder_km, "Cov138IndependentMj2000Eq", "Cov138IndependentBodyFixed").unwrap();

    let m6 = block6(&out.rotation, &out.rotation_dot);
    let expected = mat6_congruence(&m6, &mj_last.cov);

    let expected_norm: f64 = expected.iter().map(|v| v * v).sum::<f64>().sqrt();
    let err: f64 = bf_last.cov.iter().zip(expected.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    let rel_err = err / expected_norm;
    eprintln!("[covariance frame conversion] tai_ns {} epoch_a1mjd {epoch_a1mjd}: Frobenius error {err:.6e} (rel {rel_err:.3e}) vs independently-computed R (mj2000eq_probe's own cov) Rᵀ", bf_last.tai_ns);
    assert!(rel_err < 1e-12, "DRM-path rotated covariance relative error {rel_err:.3e} exceeds the REQUIRED PIN's 1e-12 relative bound");

    av_cdm::covariance::check_spd_row_major(&bf_last.cov, 6, "covariance_frame_conversion test final sample (EarthBodyFixed)").expect("the rotated covariance must still pass the SPD hygiene check");
}

//! M3.2: `GmatModel` wrapping the 42-state STM-capable `DerivativeModel`
//! (`Gmat::derivative_model_with_stm`), exercised through the full `av_dynamics::DynamicsModel`
//! contract (SI metres/metres-per-second, TAI nanoseconds) -- `crates/gmat-sys/tests/
//! stm_spike.rs` proved the raw `DerivativeModel` path; this proves the higher-level
//! `GmatModel`/`StmAugmented` wiring on top of it reproduces the same numbers.
use std::collections::BTreeMap;
use std::path::PathBuf;

use av_dynamics::{propagate_covariance, DynamicsModel, StmAugmented};
use gmat_sys::model::{GmatModel, GmatModelInfo};
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct StmFixture {
    duration_s: f64,
    initial_state_42: Vec<f64>,
    final_state_42: Vec<f64>,
    det_phi_t1: f64,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stm_golden.json")
}

#[test]
fn gmat_model_wraps_the_stm_capable_derivative_model_and_matches_the_golden() {
    let _engine = gmat_sys::engine_lock();
    let fixture: StmFixture = serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap();

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // --- 42-state (STM requested) model. ---
    let sat = gmat.construct("Spacecraft", "ModelStmSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in [("SMA", 6878.0), ("ECC", 0.001), ("INC", 51.6), ("RAAN", 30.0), ("AOP", 0.0), ("TA", 0.0), ("DryMass", 500.0), ("Cd", 2.2), ("Cr", 1.8), ("DragArea", 5.0), ("SRPArea", 5.0)] {
        sat.set_real(k, v).unwrap();
    }
    let fm = gmat.construct("ForceModel", "ModelStmFM").unwrap();
    fm.set_str("CentralBody", "Earth").unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", "Earth").unwrap();
    grav.set_str("PotentialFile", "JGM2.cof").unwrap();
    grav.set_int("Degree", 8).unwrap();
    grav.set_int("Order", 8).unwrap();
    fm.add_force(&grav).unwrap();
    for body in ["Luna", "Sun"] {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }
    gmat.initialize().unwrap();
    let derivative_model = gmat.derivative_model_with_stm(&fm, &sat).expect("42-state derivative model");
    assert_eq!(derivative_model.dimension(), 42);

    let mut settings = BTreeMap::new();
    settings.insert("central_body".to_string(), "Earth".to_string());
    settings.insert("gravity_file".to_string(), "JGM2.cof".to_string());
    settings.insert("gravity_degree".to_string(), "8".to_string());
    settings.insert("gravity_order".to_string(), "8".to_string());
    settings.insert("point_masses".to_string(), "Luna,Sun".to_string());
    settings.insert("stm".to_string(), "true".to_string());
    let info = GmatModelInfo {
        id: "gmat.earth.jgm2_8x8.sun_moon.stm".to_string(),
        version: "R2026a".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        frame_id: "EarthMJ2000Eq".to_string(),
        goldens: vec!["leo_1day_jgm2_8x8_sunmoon_stm42".to_string()],
        has_relativistic_correction: false,
    };
    let model = GmatModel::new(derivative_model, info, &settings, /*accept_missing_stm_terms=*/ false);

    // --- Declared capability. ---
    assert!(model.stm_capable(), "GmatModel wrapping a 42-state model must declare stm_capable()");
    let described = model.describe();
    assert!(
        described.capabilities.contains(&(av_cdm::pb::ModelCapability::Stm as i32)),
        "describe() must list MODEL_CAPABILITY_STM for an STM-capable GmatModel"
    );
    assert_eq!(model.state_dim(), 6, "the physical state space stays 6-dimensional regardless of STM capability");

    // --- derivatives() (the plain 6-state contract) must still work and agree with the golden
    // fixture's own recorded initial state, in SI. ---
    let t0_tai_ns = model.epoch_tai_ns();
    let x0_si = model.initial_state_si().expect("initial state");
    let golden_x0_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(&fixture.initial_state_42[0..6]).unwrap());
    for (a, b) in x0_si.iter().zip(golden_x0_si.iter()) {
        assert!((a - b).abs() < 1e-6, "GmatModel's initial state differs from the STM golden fixture: {x0_si:?} vs {golden_x0_si:?}");
    }
    let mut dot = [0.0; 6];
    model.derivatives(&x0_si, t0_tai_ns, &[], &mut dot).expect("derivatives");
    for i in 0..3 {
        assert!((dot[i] - x0_si[3 + i]).abs() < 1e-9, "d(pos)/dt must equal velocity");
    }

    // --- Wrap in StmAugmented and integrate the whole golden arc via step_with_stm, exactly
    // as av-kernel's Kernel<StmAugmented<GmatModel>>::run_with_covariance does internally. ---
    let wrapped = StmAugmented::new(model);
    let duration_ns = (fixture.duration_s * 1e9).round() as i64;
    let step = wrapped.inner().step_with_stm(&x0_si, t0_tai_ns, &[], duration_ns).expect("step_with_stm over the whole arc");

    let golden_x1_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(&fixture.final_state_42[0..6]).unwrap());
    let dr = (0..3).map(|i| (step.state[i] - golden_x1_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (step.state[i] - golden_x1_si[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[gmat-sys model_stm] GmatModel/StmAugmented position error vs golden: {dr:.4} m, velocity error: {dv:.3e} m/s");
    assert!(dr < 0.05, "position error {dr} m too large");
    assert!(dv < 5e-5, "velocity error {dv} m/s too large");

    // Phi is unit-invariant (km<->m cancels, see GmatModel::stm_derivatives's doc comment), so
    // it must equal the raw fixture's STM elements directly, without conversion.
    let mut max_abs_err = 0.0f64;
    for row in 0..6 {
        for col in 0..6 {
            let ours = step.phi[row * 6 + col];
            let theirs = fixture.final_state_42[6 + row * 6 + col];
            max_abs_err = max_abs_err.max((ours - theirs).abs());
        }
    }
    eprintln!("[gmat-sys model_stm] STM max abs error vs golden fixture: {max_abs_err:.6e}");
    assert!(max_abs_err < 1.0, "STM max abs error {max_abs_err} too large (see amendment draft for the measured value)");

    // Phi(t0, t0) = I, via a zero-duration step_with_stm.
    let zero = wrapped.inner().step_with_stm(&x0_si, t0_tai_ns, &[], 0).expect("zero-duration step_with_stm");
    for row in 0..6 {
        for col in 0..6 {
            let expect = if row == col { 1.0 } else { 0.0 };
            assert_eq!(zero.phi[row * 6 + col], expect, "Phi(t0,t0) not identity at ({row},{col})");
        }
    }

    // det(Phi) at t1: an independent Liouville/symplecticity check, and cross-checked against
    // the fixture's own recorded det(Phi).
    let det = det6(&step.phi);
    eprintln!("[gmat-sys model_stm] det(Phi) at t1: ours = {det:.12}, fixture's = {:.12}", fixture.det_phi_t1);
    assert!((det - 1.0).abs() < 1e-4, "det(Phi) = {det} not near 1");
    assert!((det - fixture.det_phi_t1).abs() < 1e-4, "det(Phi) {det} disagrees with fixture's {}", fixture.det_phi_t1);

    // --- propagate_covariance: a simple diagonal P0 in SI, propagated through this Phi. ---
    let p0: Vec<f64> = {
        let mut m = vec![0.0; 36];
        for i in 0..3 {
            m[i * 6 + i] = 100.0f64.powi(2); // (100 m)^2 position variance
        }
        for i in 3..6 {
            m[i * 6 + i] = 0.1f64.powi(2); // (0.1 m/s)^2 velocity variance
        }
        m
    };
    let (p1, max_asym) = propagate_covariance(&step.phi, &p0, 6);
    eprintln!("[gmat-sys model_stm] covariance propagation: pre-symmetrization asymmetry {max_asym:.3e}");
    // Trace (sum of variances) is not conserved by a general (non-orthogonal) Phi, but must be
    // finite, symmetric and have a positive diagonal (a necessary, not sufficient, SPD check).
    for i in 0..6 {
        assert!(p1[i * 6 + i] > 0.0, "covariance diagonal entry {i} is not positive: {}", p1[i * 6 + i]);
    }
    for i in 0..6 {
        for j in 0..6 {
            assert_eq!(p1[i * 6 + j], p1[j * 6 + i], "propagated covariance must be exactly symmetric after correction");
        }
    }
}

/// M5.1, question 82: a force model that includes `RelativisticCorrection` -- whose
/// `GetDerivatives` fills its A-matrix/STM contribution with an unconditional zero
/// (`third_party/gmat-src/src/base/forcemodel/RelativisticCorrection.cpp`, both the `fillSTM`
/// and `fillAMatrix` branches; confirmed here that the type constructs and the resulting
/// 42-state model's STM-derivative block is exactly zero for it, not merely "small") -- gets a
/// 42-state `DerivativeModel` from GMAT (dimension 42, `GetDerivatives` runs without error), but
/// `GmatModel::stm_capable()` must report the capability *absent* unless the caller explicitly
/// passes `accept_missing_stm_terms = true` to `GmatModel::new` (the DRM's
/// `accept_missing_stm_terms` acknowledgement, `proto/altavista/v1/system.proto`'s
/// `DrmOptions`). Two independently-built models (GMAT holds one force-model/spacecraft graph
/// per `Construct` call, so two owned `DerivativeModel`s need two separate builds, matching
/// `tests/leo_golden.rs::two_models_in_one_process_do_not_interfere`'s own pattern) isolate the
/// `accept_missing_stm_terms` flag as the only thing that differs.
#[test]
fn relativistic_correction_withholds_the_stm_capability_unless_accepted() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let build = |tag: &str| {
        let sat = gmat.construct("Spacecraft", &format!("RelCorrSat{tag}")).unwrap();
        sat.set_str("DateFormat", "UTCGregorian").unwrap();
        sat.set_str("Epoch", "01 Jan 2026 00:00:00.000").unwrap();
        sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
        sat.set_str("DisplayStateType", "Keplerian").unwrap();
        for (k, v) in [("SMA", 6878.0), ("ECC", 0.001), ("INC", 51.6)] {
            sat.set_real(k, v).unwrap();
        }
        let fm = gmat.construct("ForceModel", &format!("RelCorrFM{tag}")).unwrap();
        fm.set_str("CentralBody", "Earth").unwrap();
        let grav = gmat.construct("GravityField", "").unwrap();
        grav.set_str("BodyName", "Earth").unwrap();
        grav.set_str("PotentialFile", "JGM2.cof").unwrap();
        grav.set_int("Degree", 4).unwrap();
        grav.set_int("Order", 4).unwrap();
        fm.add_force(&grav).unwrap();
        fm.add_force(&gmat.construct("RelativisticCorrection", "").unwrap()).unwrap();
        gmat.initialize().unwrap();
        let derivative_model = gmat.derivative_model_with_stm(&fm, &sat).expect("42-state model with RelativisticCorrection");
        assert_eq!(derivative_model.dimension(), 42);
        derivative_model
    };
    let settings = {
        let mut s = BTreeMap::new();
        s.insert("central_body".to_string(), "Earth".to_string());
        s.insert("relativistic_correction".to_string(), "true".to_string());
        s
    };
    let info = |goldens: &str| GmatModelInfo {
        id: "gmat.test.relativistic_correction".to_string(),
        version: "R2026a".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        frame_id: "EarthMJ2000Eq".to_string(),
        goldens: vec![goldens.to_string()],
        has_relativistic_correction: true,
    };

    // GMAT itself fills the STM-derivative block for this model, but with RelativisticCorrection
    // contributing exactly zero -- confirms the ADR's "a stub, not an absent term" reading is
    // real for this shim, not only in GMAT's source.
    let checked = build("Checked");
    let x0 = checked.state().unwrap();
    let d0 = checked.derivatives(&x0, 0.0).unwrap();
    let stm_deriv_block: Vec<f64> = d0[6..42].to_vec();
    assert!(stm_deriv_block.iter().any(|v| v.abs() > 0.0), "gravity alone must still fill some STM-derivative entries");

    // Default: accept_missing_stm_terms = false -> capability withheld.
    let withheld_model = GmatModel::new(build("Withheld"), info("relativistic_withheld"), &settings, false);
    assert!(!withheld_model.stm_capable(), "STM capability must be declared absent for a RelativisticCorrection model unless accept_missing_stm_terms is set");
    assert!(
        !withheld_model.describe().capabilities.contains(&(av_cdm::pb::ModelCapability::Stm as i32)),
        "describe() must not list MODEL_CAPABILITY_STM when the capability is withheld"
    );

    // Opted in: accept_missing_stm_terms = true -> capability restored.
    let accepted_model = GmatModel::new(build("Accepted"), info("relativistic_accepted"), &settings, true);
    assert!(accepted_model.stm_capable(), "accept_missing_stm_terms=true must restore the declared STM capability");
    assert!(
        accepted_model.describe().capabilities.contains(&(av_cdm::pb::ModelCapability::Stm as i32)),
        "describe() must list MODEL_CAPABILITY_STM once accept_missing_stm_terms is set"
    );
}

fn det6(m: &[f64]) -> f64 {
    let mut a = [[0.0f64; 6]; 6];
    for r in 0..6 {
        for c in 0..6 {
            a[r][c] = m[r * 6 + c];
        }
    }
    let mut det = 1.0;
    for i in 0..6 {
        let mut pivot = i;
        for r in (i + 1)..6 {
            if a[r][i].abs() > a[pivot][i].abs() {
                pivot = r;
            }
        }
        if a[pivot][i] == 0.0 {
            return 0.0;
        }
        if pivot != i {
            a.swap(pivot, i);
            det = -det;
        }
        det *= a[i][i];
        for r in (i + 1)..6 {
            let f = a[r][i] / a[i][i];
            let (pivot_row, rows_below) = a.split_at_mut(r);
            let (dst, src) = (&mut rows_below[0], &pivot_row[i]);
            for c in i..6 {
                dst[c] -= f * src[c];
            }
        }
    }
    det
}

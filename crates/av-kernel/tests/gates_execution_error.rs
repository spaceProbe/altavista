//! The Gates maneuver execution error model's acceptance tests (M11.4/M12.1, `docs/
//! open-questions.md` questions 100 and 103 -- see `crates/av-kernel/src/drm/maneuver.rs`'s own
//! module doc comment's "Burn execution error" section for the model itself, the Gates 1963
//! reference, and exactly what `sample_execution_error`/`inject_gates_covariance` compute, and
//! `av_kernel::drm::ExecutionErrorMode`'s own doc comment for question 103's fix: the mode
//! (`Nominal`/`Sampled`) is explicit on `RunConfig`, never inferred from whether the run carries
//! covariance).
//!
//! Required tests (M11.4's original four, plus M12.1's two new ones for question 103):
//!
//! 1. [`sampled_runs_are_byte_identical_across_two_runs_with_the_same_seed`] -- byte-identical
//!    determinism across two `ExecutionErrorMode::Sampled` runs (non-covariance) with the same
//!    seed.
//! 2. [`a_zero_sigma_execution_error_block_reproduces_the_perfect_burn_golden_bit_for_bit`] -- a
//!    zero-sigma block, under `ExecutionErrorMode::Nominal`, reproduces the perfect-burn golden
//!    bit-for-bit (the golden itself, `goldens/leo_1day_maneuver_vnb.json`, is read-only --
//!    reproduced, never regenerated).
//! 3. [`analytic_gates_injection_matches_the_sample_covariance_of_n_sampled_draws`] -- the
//!    analytic injection matches the sample covariance of `N` **explicitly `Sampled`**
//!    (`maneuver::dv_to_apply(ExecutionErrorMode::Sampled, ...)`) draws to a tolerance stated and
//!    derived from sampling theory (standard error ~ `1/sqrt(N)`), with the measured agreement
//!    reported via `eprintln!`. Moved onto the explicit-mode API by M12.1 (question 103) --
//!    `N = 500_000` and the 5-standard-error tolerance are unchanged from M11.4, neither weakened.
//! 4. [`a_proportional_only_execution_error_block_scales_the_analytic_injection_with_dv_squared`]
//!    -- a proportional-only block scales with `|dv|` as expected (deterministically, through
//!    the analytic path -- no sampling tolerance needed for this one).
//! 5. [`a_nominal_plain_run_with_a_non_zero_gates_block_reproduces_the_perfect_burn_golden_bit_for_bit`]
//!    (M12.1, question 103) -- **the regression M11.4's bug would have caused, and the most
//!    important test in this task**: a plain (non-covariance) run declaring a *non-zero* Gates
//!    block, under `ExecutionErrorMode::Nominal`, must still reproduce the perfect-burn golden
//!    bit-for-bit -- before M12.1, the plain path sampled unconditionally regardless of any
//!    declared mode, so this exact case silently applied a dispersed burn to what a designer
//!    declared as a nominal run.
//! 6. [`a_sampled_covariance_run_injects_nothing`] (M12.1, question 103) -- a covariance run
//!    under `ExecutionErrorMode::Sampled` with a non-zero Gates block injects exactly nothing
//!    into `P` (bit-identical to the same run with the block absent) -- the dispersion is
//!    already realized in the sampled mean, so injecting the analytic term on top would
//!    double-count it.
//!
//! Two more, not individually required but proving properties this task's instructions call
//! out by name:
//!
//! - [`adding_a_second_maneuver_event_does_not_shift_the_first_ones_sampled_draw`] -- the
//!   "fresh substream per event id" property (`crate::rng::event_rng`'s own doc comment),
//!   proven end to end through the full DRM executor (`ExecutionErrorMode::Sampled`) rather than
//!   only at the RNG-module level
//!   (`crate::rng::tests::event_substream_is_independent_of_other_events` already covers that
//!   level).
//! - [`the_covariance_path_injects_exactly_the_analytic_gates_term_at_the_burn_epoch`] -- proves
//!   `executor::run_covariance_instance`'s own wiring (not just the pure `maneuver` functions in
//!   isolation) actually calls `inject_gates_covariance` with the right `sigma_m`/`sigma_p`/
//!   triad, and that the commanded dv is applied exactly (never perturbed), under
//!   `ExecutionErrorMode::Nominal` -- the mode M12.1 (question 103) made explicit: before this
//!   task the covariance path always behaved this way regardless of any declared mode (there was
//!   none), which is exactly the bug question 103 fixes on the *other* path (plain runs sampled
//!   unconditionally); this test now names the mode it exercises rather than relying on "the
//!   covariance path's own shape".
//!
//! Every GMAT-touching test here takes `gmat_sys::engine_lock()` first and gives every instance
//! it constructs a name unused by any other test in this crate, per this repository's existing
//! convention (`crates/av-kernel/src/drm/executor.rs`'s module doc comment's "GMAT object
//! naming" section). Tests 1, 3, 4 and the substream-independence test use a native
//! (GMAT-free) `"native.constant_accel"` binding or call `maneuver`'s own pure functions
//! directly -- no GMAT propagation is needed to exercise the sampling/injection math itself, so
//! those tests stay fast; tests 2, 5, 6 and the covariance-wiring test use the golden `leo_sys`
//! `SystemDefinition` (real GMAT propagation) since they are specifically about reproducing a
//! GMAT-derived golden and the covariance path's own wiring.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{
    AxesKind, Binding, BindingKind, DesignReferenceMission, DrmOptions, ManeuverExecutionError, ModelBinding, Parameter, Scenario, ScenarioEvent, SosConfiguration, SystemDefinition, SystemInstance,
};
use av_kernel::drm::maneuver::{self, ParsedExecutionError, ParsedManeuver};
use av_kernel::drm::{execute, hash, schema, ExecutionErrorMode, RunConfig};
use gmat_sys::Gmat;
use serde::Deserialize;

// ----------------------------------------------------------------------------------------
// Shared helpers (mirrors crates/av-kernel/tests/drm_maneuver.rs's own conventions).
// ----------------------------------------------------------------------------------------

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn load_golden_leo_sys() -> BTreeMap<String, SystemDefinition> {
    let sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.system.yaml")).unwrap()).expect("SystemDefinition parses");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    systems
}

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

fn maneuver_event_ee(id: &str, tai_ns: i64, instance: &str, dv: [f64; 3], frame: &str, execution_error: Option<ManeuverExecutionError>) -> ScenarioEvent {
    ScenarioEvent {
        id: id.to_string(),
        tai_ns,
        kind: "maneuver".to_string(),
        instance: instance.to_string(),
        values: BTreeMap::from([("dv_x".to_string(), dv[0]), ("dv_y".to_string(), dv[1]), ("dv_z".to_string(), dv[2])]),
        attributes: BTreeMap::from([("frame_id".to_string(), frame.to_string())]),
        execution_error,
    }
}

fn model_instance(name: &str, sys_id: &str) -> SystemInstance {
    SystemInstance {
        name: name.to_string(),
        system_id: sys_id.to_string(),
        binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: sys_id.to_string() })) }),
        step_rate_hz: 10.0,
        ..Default::default()
    }
}

/// A native, GMAT-free constant-acceleration system -- fast, exact closed-form dynamics, so a
/// test using it never needs to wait on GMAT propagation to exercise the Gates sampling/
/// injection math itself (mirrors `tests/drm_maneuver.rs::accel_system`).
fn accel_system(id: &str) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", 0.0),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            sparam("frame_id", "test.frame"),
            param("state.px", 7_000_000.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 7_500.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    })
}

fn accel_sos_drm(sos_id: &str, drm_id: &str, instance: &str, sys_id: &str, events: Vec<ScenarioEvent>, seeds: BTreeMap<String, u64>) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances: vec![model_instance(instance, sys_id)], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, events, seeds, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

// ----------------------------------------------------------------------------------------
// 1. Byte-identical determinism across two sampled runs with the same seed.
// ----------------------------------------------------------------------------------------

#[test]
fn sampled_runs_are_byte_identical_across_two_runs_with_the_same_seed() {
    let _engine = gmat_sys::engine_lock();
    let sys = accel_system("gates_det_sys");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.5, sigma_magnitude_proportional: 0.01, sigma_pointing_fixed_mps: 0.3, sigma_pointing_proportional_rad: 0.002, seed: "s".to_string() };
    let seeds = BTreeMap::from([("s".to_string(), 4242u64)]);
    let event = maneuver_event_ee("burn1", 1_000_000_000, "veh", [0.0, 5.0, 0.0], "AXES_KIND_ICRF", Some(ee));

    let (sos_a, drm_a) = accel_sos_drm("gates_det_sos_a", "gates_det_drm_a", "veh", "gates_det_sys", vec![event.clone()], seeds.clone());
    let cfg_a = RunConfig { gmat: &gmat, drm: &drm_a, sos: &sos_a, systems: &systems, run_id: "gates-det-a".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_a = execute(cfg_a).expect("run A executes");

    let (sos_b, drm_b) = accel_sos_drm("gates_det_sos_b", "gates_det_drm_b", "veh", "gates_det_sys", vec![event], seeds);
    let cfg_b = RunConfig { gmat: &gmat, drm: &drm_b, sos: &sos_b, systems: &systems, run_id: "gates-det-b".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_b = execute(cfg_b).expect("run B executes");

    let traj_a = products_a.trajectories.get("veh").expect("run A produced a trajectory");
    let traj_b = products_b.trajectories.get("veh").expect("run B produced a trajectory");
    assert_eq!(traj_a.samples.len(), traj_b.samples.len());
    for (sa, sb) in traj_a.samples.iter().zip(traj_b.samples.iter()) {
        assert_eq!(sa.tai_ns, sb.tai_ns);
        assert_eq!(sa.mean, sb.mean, "byte-identical determinism: mean must match exactly at tai_ns {}", sa.tai_ns);
    }
    let mev_a = products_a.events.iter().find(|e| e.name == "burn1").expect("run A's maneuver event");
    let mev_b = products_b.events.iter().find(|e| e.name == "burn1").expect("run B's maneuver event");
    assert_eq!(mev_a.values, mev_b.values, "the sampled applied dv and raw draws must match exactly across two runs with the same seed");
}

// ----------------------------------------------------------------------------------------
// 2. A zero-sigma execution_error block reproduces the perfect-burn golden bit-for-bit.
// ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ManeuverGolden {
    final_state: Vec<f64>,
    tolerance_m: f64,
    tolerance_mps: f64,
}

fn leo_gates_sos_drm(sos_id: &str, drm_id: &str, instance: &str, start: i64, event: ScenarioEvent, seeds: BTreeMap<String, u64>) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances: vec![model_instance(instance, "leo_sys")], ..Default::default() });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: start, end_tai_ns: start + 7_200_000_000_000, events: vec![event], seeds, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 60.0, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

#[test]
fn a_zero_sigma_execution_error_block_reproduces_the_perfect_burn_golden_bit_for_bit() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    // Same epoch/dv leo_1day_maneuver_vnb.drm.yaml uses (drms/README.md's own section).
    let start = 1_767_225_637_000_000_000i64;
    let burn_tai = start + 3_600_000_000_000;

    let (sos_perfect, drm_perfect) =
        leo_gates_sos_drm("gates_zero_sigma_sos_perfect", "gates_zero_sigma_drm_perfect", "leo_gates_perfect", start, maneuver_event_ee("burn1", burn_tai, "leo_gates_perfect", [20.0, 0.0, 0.0], "AXES_KIND_VNB", None), BTreeMap::new());
    let cfg_perfect = RunConfig { gmat: &gmat, drm: &drm_perfect, sos: &sos_perfect, systems: &systems, run_id: "gates-perfect".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_perfect = execute(cfg_perfect).expect("perfect burn executes");

    let zero_ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.0, sigma_magnitude_proportional: 0.0, sigma_pointing_fixed_mps: 0.0, sigma_pointing_proportional_rad: 0.0, seed: "burn_seed".to_string() };
    let (sos_zero, drm_zero) = leo_gates_sos_drm(
        "gates_zero_sigma_sos_zero",
        "gates_zero_sigma_drm_zero",
        "leo_gates_zero",
        start,
        maneuver_event_ee("burn1", burn_tai, "leo_gates_zero", [20.0, 0.0, 0.0], "AXES_KIND_VNB", Some(zero_ee)),
        BTreeMap::from([("burn_seed".to_string(), 1u64)]),
    );
    let cfg_zero = RunConfig { gmat: &gmat, drm: &drm_zero, sos: &sos_zero, systems: &systems, run_id: "gates-zero".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_zero = execute(cfg_zero).expect("zero-sigma execution_error burn executes");

    let traj_perfect = products_perfect.trajectories.get("leo_gates_perfect").expect("perfect-burn trajectory");
    let traj_zero = products_zero.trajectories.get("leo_gates_zero").expect("zero-sigma trajectory");
    assert_eq!(traj_perfect.samples.len(), traj_zero.samples.len());
    for (sp, sz) in traj_perfect.samples.iter().zip(traj_zero.samples.iter()) {
        assert_eq!(sp.tai_ns, sz.tai_ns);
        assert_eq!(sp.mean, sz.mean, "a declared, present, all-zero execution_error block must reproduce a perfect burn bit-for-bit at tai_ns {}", sp.tai_ns);
    }

    // Also checked directly against the read-only golden itself (the same tolerance
    // tests/drm_maneuver.rs::drm_matches_the_maneuver_golden_vnb_burn uses) -- not just
    // self-consistency between these two Rust-side runs.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_vnb.json");
    let g: ManeuverGolden = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let last = traj_zero.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let dr: f64 = (0..3).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv: f64 = (3..6).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[gates zero-sigma golden] |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "final position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "final velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);
}

// ----------------------------------------------------------------------------------------
// 3. The analytic injection matches the sample covariance of N sampled draws.
// ----------------------------------------------------------------------------------------

/// N and the tolerance are fixed **before** looking at the measured result, exactly as this
/// crate's own task instructions require: `N = 500_000` independent draws (each through the
/// real `maneuver::sample_execution_error`, with a distinct `base_seed` per draw and the same
/// event id -- exactly the shape a real Monte Carlo sweep's per-draw seeding would take), and a
/// tolerance of **5 standard errors per matrix element**, derived from sampling theory:
///
/// - a diagonal (variance) entry's standard error is `sigma^2 * sqrt(2 / (N - 1))` (exact for a
///   Gaussian sample-variance estimator);
/// - an off-diagonal entry between two independent, zero-true-covariance Gaussian components has
///   standard error `sigma_a * sigma_b / sqrt(N)` (`Var(sample covariance) ~= Var(a)*Var(b)/N`
///   when the true covariance is `0`).
///
/// 5 standard errors is roughly a 1-in-3,500,000 false-positive rate per element under the null
/// (two-sided Gaussian tail) -- a standard, conservative multiplier fixed by convention before
/// this test's sampling loop ever runs, not tuned after seeing the result.
#[test]
fn analytic_gates_injection_matches_the_sample_covariance_of_n_sampled_draws() {
    const N: usize = 500_000;
    const TOLERANCE_STANDARD_ERRORS: f64 = 5.0;

    // dv commanded exactly along +x with AxesKind::Icrf (identity transform, so r/v are
    // irrelevant): burn_triad's own deterministic completion then puts u = +x, p1 = +z,
    // p2 = -y (see maneuver::tests::burn_triad_is_orthonormal_and_right_handed_with_u_along_dv
    // for the general property and this test's own eprintln for the concrete numbers), so the
    // analytic 3x3 velocity block collapses to exactly diag(sigma_m^2, sigma_p^2, sigma_p^2) in
    // (x, y, z) -- no cross terms to derive by hand, only to check are statistically ~0.
    // Question 103: this test moves onto the explicit-mode API -- `m.execution_error` is now
    // declared `Some`, and each draw goes through `maneuver::dv_to_apply(ExecutionErrorMode::
    // Sampled, ...)` (the same function `executor::run_plain_instance`/`run_covariance_instance`
    // call) rather than the bare `sample_execution_error` free function directly. `N = 500_000`
    // and the 5-standard-error tolerance below are unchanged from before this task.
    let ee = ParsedExecutionError { sigma_magnitude_fixed_mps: 0.02, sigma_magnitude_proportional: 0.001, sigma_pointing_fixed_mps: 0.01, sigma_pointing_proportional_rad: 0.0006, seed: "s".to_string() };
    let m = ParsedManeuver { id: "burn_stat".to_string(), tai_ns: 0, instance: "veh".to_string(), dv: [20.0, 0.0, 0.0], axes: AxesKind::Icrf, execution_error: Some(ee.clone()) };
    let v = (m.dv[0] * m.dv[0] + m.dv[1] * m.dv[1] + m.dv[2] * m.dv[2]).sqrt();
    let (sigma_m, sigma_p) = maneuver::gates_sigmas(&ee, v);

    let mut sum = [0.0f64; 3];
    let mut deltas: Vec<[f64; 3]> = Vec::with_capacity(N);
    for i in 0..N {
        let seeds = BTreeMap::from([("s".to_string(), i as u64 + 1)]);
        let (applied_dv, sampled) = maneuver::dv_to_apply(ExecutionErrorMode::Sampled, &m, &seeds);
        sampled.expect("Sampled with a declared execution_error block must always produce a realization");
        let delta = [applied_dv[0] - m.dv[0], applied_dv[1] - m.dv[1], applied_dv[2] - m.dv[2]];
        for k in 0..3 {
            sum[k] += delta[k];
        }
        deltas.push(delta);
    }
    let n = N as f64;
    let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
    let mut sample_cov = [[0.0f64; 3]; 3];
    for d in &deltas {
        for a in 0..3 {
            for b in 0..3 {
                sample_cov[a][b] += (d[a] - mean[a]) * (d[b] - mean[b]);
            }
        }
    }
    for row in sample_cov.iter_mut() {
        for x in row.iter_mut() {
            *x /= n - 1.0;
        }
    }

    // The analytic side, through the actual production functions the covariance-path executor
    // calls (`inertial_triad`/`inject_gates_covariance`), not a hand re-derived formula.
    let r = [1.0, 0.0, 0.0]; // arbitrary: AxesKind::Icrf ignores r/v entirely.
    let vel = [0.0, 1.0, 0.0];
    let triad = maneuver::inertial_triad(m.axes, m.dv, r, vel);
    let mut p = vec![0.0f64; 36];
    maneuver::inject_gates_covariance(&mut p, 6, sigma_m, sigma_p, triad);
    let analytic = [[p[3 * 6 + 3], p[3 * 6 + 4], p[3 * 6 + 5]], [p[4 * 6 + 3], p[4 * 6 + 4], p[4 * 6 + 5]], [p[5 * 6 + 3], p[5 * 6 + 4], p[5 * 6 + 5]]];

    // Per-axis 1-sigma for the standard-error formulas -- diag(sigma_m, sigma_p, sigma_p) since
    // this test's own dv/frame choice puts u along x and p1/p2 along z/-y (see the eprintln
    // below for the measured triad, which confirms this rather than assuming it silently).
    eprintln!("[gates statistical test] triad u={:?} p1={:?} p2={:?}", triad.0, triad.1, triad.2);
    let sigmas = [sigma_m, sigma_p, sigma_p];

    let mut max_deviation_se = 0.0f64;
    let mut report = String::new();
    for a in 0..3 {
        for b in 0..3 {
            let expected = analytic[a][b];
            let measured = sample_cov[a][b];
            let se = if a == b { sigmas[a] * sigmas[a] * (2.0 / (n - 1.0)).sqrt() } else { sigmas[a] * sigmas[b] / n.sqrt() };
            let deviation_se = (measured - expected).abs() / se;
            max_deviation_se = max_deviation_se.max(deviation_se);
            report.push_str(&format!("  [{a},{b}] analytic={expected:.6e}  sample={measured:.6e}  SE={se:.3e}  |deviation|={deviation_se:.2} SE\n"));
        }
    }
    eprintln!("[gates statistical test] N={N}, sigma_m={sigma_m:.6e}, sigma_p={sigma_p:.6e}\n{report}measured max |deviation| = {max_deviation_se:.2} standard errors (tolerance: {TOLERANCE_STANDARD_ERRORS} SE)");
    assert!(
        max_deviation_se < TOLERANCE_STANDARD_ERRORS,
        "analytic Gates injection disagrees with the N={N} sampled covariance by {max_deviation_se:.2} standard errors, exceeding the {TOLERANCE_STANDARD_ERRORS}-SE tolerance derived from sampling theory -- see the eprintln above for the full 3x3 comparison"
    );
}

// ----------------------------------------------------------------------------------------
// 4. A proportional-only block scales with |dv| as expected (deterministic, analytic path).
// ----------------------------------------------------------------------------------------

#[test]
fn a_proportional_only_execution_error_block_scales_the_analytic_injection_with_dv_squared() {
    // Deterministic (no sampling): the analytic injection is a pure function of
    // (sigma_m, sigma_p, triad), so this needs no statistical tolerance, only a tight numerical
    // margin on floating point arithmetic.
    let ee = ParsedExecutionError { sigma_magnitude_fixed_mps: 0.0, sigma_magnitude_proportional: 0.002, sigma_pointing_fixed_mps: 0.0, sigma_pointing_proportional_rad: 0.0005, seed: "s".to_string() };
    let r = [1.0, 0.0, 0.0];
    let vel = [0.0, 1.0, 0.0];

    let dv_small = [10.0, 0.0, 0.0];
    let dv_large = [40.0, 0.0, 0.0]; // 4x |dv| -> sigma scales 4x (linear) -> variance scales 16x.

    let (sm_small, sp_small) = maneuver::gates_sigmas(&ee, 10.0);
    let (sm_large, sp_large) = maneuver::gates_sigmas(&ee, 40.0);
    assert!((sm_large / sm_small - 4.0).abs() < 1e-12, "sigma_m must scale linearly with |dv| for a proportional-only block: {sm_small} -> {sm_large}");
    assert!((sp_large / sp_small - 4.0).abs() < 1e-12, "sigma_p must scale linearly with |dv| for a proportional-only block: {sp_small} -> {sp_large}");

    let mut p_small = vec![0.0f64; 36];
    maneuver::inject_gates_covariance(&mut p_small, 6, sm_small, sp_small, maneuver::inertial_triad(AxesKind::Icrf, dv_small, r, vel));

    let mut p_large = vec![0.0f64; 36];
    maneuver::inject_gates_covariance(&mut p_large, 6, sm_large, sp_large, maneuver::inertial_triad(AxesKind::Icrf, dv_large, r, vel));

    for a in 3..6 {
        for b in 3..6 {
            let small = p_small[a * 6 + b];
            let large = p_large[a * 6 + b];
            if small.abs() > 1e-30 {
                assert!((large / small - 16.0).abs() < 1e-9, "[{a},{b}]: expected exactly 16x scaling (sigma^2 for a 4x |dv|, proportional-only), got ratio {}", large / small);
            } else {
                assert!(large.abs() < 1e-30, "[{a},{b}]: both should be exactly zero off the diagonal for a dv along a coordinate axis");
            }
        }
    }
}

// ----------------------------------------------------------------------------------------
// 5. (M12.1, question 103) A Nominal plain run with a NON-ZERO Gates block reproduces the
//    perfect-burn golden bit-for-bit -- the exact regression M11.4's bug caused.
// ----------------------------------------------------------------------------------------

#[test]
fn a_nominal_plain_run_with_a_non_zero_gates_block_reproduces_the_perfect_burn_golden_bit_for_bit() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let start = 1_767_225_637_000_000_000i64;
    let burn_tai = start + 3_600_000_000_000;

    let (sos_perfect, drm_perfect) = leo_gates_sos_drm(
        "gates_nominal_nonzero_sos_perfect",
        "gates_nominal_nonzero_drm_perfect",
        "leo_gates_nom_perfect",
        start,
        maneuver_event_ee("burn1", burn_tai, "leo_gates_nom_perfect", [20.0, 0.0, 0.0], "AXES_KIND_VNB", None),
        BTreeMap::new(),
    );
    let cfg_perfect = RunConfig { gmat: &gmat, drm: &drm_perfect, sos: &sos_perfect, systems: &systems, run_id: "gates-nominal-nonzero-perfect".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_perfect = execute(cfg_perfect).expect("perfect burn executes");

    // A substantial, NON-zero Gates block on a *plain* (non-covariance) run under
    // ExecutionErrorMode::Nominal -- before M12.1 (question 103), the plain path sampled
    // unconditionally regardless of any declared mode, so this exact case silently applied a
    // dispersed burn to what a designer declared as a nominal run. It must not: Nominal applies
    // the commanded dv exactly on either path, and a plain run has no P to inject the analytic
    // term into, so a declared block has no effect at all here.
    let nonzero_ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.5, sigma_magnitude_proportional: 0.01, sigma_pointing_fixed_mps: 0.3, sigma_pointing_proportional_rad: 0.002, seed: "burn_seed".to_string() };
    let (sos_nonzero, drm_nonzero) = leo_gates_sos_drm(
        "gates_nominal_nonzero_sos_nonzero",
        "gates_nominal_nonzero_drm_nonzero",
        "leo_gates_nom_nonzero",
        start,
        maneuver_event_ee("burn1", burn_tai, "leo_gates_nom_nonzero", [20.0, 0.0, 0.0], "AXES_KIND_VNB", Some(nonzero_ee)),
        BTreeMap::from([("burn_seed".to_string(), 1u64)]),
    );
    let cfg_nonzero = RunConfig { gmat: &gmat, drm: &drm_nonzero, sos: &sos_nonzero, systems: &systems, run_id: "gates-nominal-nonzero".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_nonzero = execute(cfg_nonzero).expect("nominal run with a non-zero execution_error block executes");

    let traj_perfect = products_perfect.trajectories.get("leo_gates_nom_perfect").expect("perfect-burn trajectory");
    let traj_nonzero = products_nonzero.trajectories.get("leo_gates_nom_nonzero").expect("nominal non-zero trajectory");
    assert_eq!(traj_perfect.samples.len(), traj_nonzero.samples.len());
    for (sp, sn) in traj_perfect.samples.iter().zip(traj_nonzero.samples.iter()) {
        assert_eq!(sp.tai_ns, sn.tai_ns);
        assert_eq!(
            sp.mean, sn.mean,
            "a Nominal plain run must apply the commanded dv exactly regardless of a declared, non-zero execution_error block -- this is the M11.4 regression question 103 fixes, at tai_ns {}",
            sp.tai_ns
        );
    }

    // Also checked directly against the read-only golden itself, exactly as the zero-sigma test
    // above does.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_vnb.json");
    let g: ManeuverGolden = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let last = traj_nonzero.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let dr: f64 = (0..3).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv: f64 = (3..6).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[gates nominal non-zero golden] |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "final position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "final velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);
}

// ----------------------------------------------------------------------------------------
// 6. (M12.1, question 103) A Sampled covariance run injects nothing into P.
// ----------------------------------------------------------------------------------------

#[test]
fn a_sampled_covariance_run_injects_nothing() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dv = [20.0, 0.0, 0.0];
    let ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.05, sigma_magnitude_proportional: 0.002, sigma_pointing_fixed_mps: 0.03, sigma_pointing_proportional_rad: 0.001, seed: "s".to_string() };
    let seeds = BTreeMap::from([("s".to_string(), 99u64)]);

    let (sos_none, drm_none) = cov_leo_sos_drm("gates_sampled_cov_sos_none", "gates_sampled_cov_drm_none", "leo_sampled_cov_none", dv, None, BTreeMap::new());
    let cfg_none = RunConfig { gmat: &gmat, drm: &drm_none, sos: &sos_none, systems: &systems, run_id: "gates-sampled-cov-none".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_none = execute(cfg_none).expect("no-execution-error covariance run executes");

    // Same non-zero block as the covariance-wiring test, but this time under
    // ExecutionErrorMode::Sampled: the drawn dv is applied and, unlike the Nominal path,
    // **nothing** is injected into P -- the dispersion is already realized in the applied
    // dv/mean, so injecting G Q G^T on top would double-count the same uncertainty.
    let (sos_ee, drm_ee) = cov_leo_sos_drm("gates_sampled_cov_sos_ee", "gates_sampled_cov_drm_ee", "leo_sampled_cov_ee", dv, Some(ee), seeds);
    let cfg_ee = RunConfig { gmat: &gmat, drm: &drm_ee, sos: &sos_ee, systems: &systems, run_id: "gates-sampled-cov-ee".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_ee = execute(cfg_ee).expect("with-execution-error covariance run executes under Sampled");

    let mev_none = products_none.events.iter().find(|e| e.name == "burn1").expect("no-EE maneuver event");
    let mev_ee = products_ee.events.iter().find(|e| e.name == "burn1").expect("EE maneuver event");
    let traj_none = products_none.trajectories.get("leo_sampled_cov_none").expect("no-EE trajectory");
    let traj_ee = products_ee.trajectories.get("leo_sampled_cov_ee").expect("EE trajectory");
    let at_burn_none = traj_none.samples.iter().find(|s| s.tai_ns == mev_none.tai_ns).expect("a sample exactly at the burn epoch (no EE)");
    let at_burn_ee = traj_ee.samples.iter().find(|s| s.tai_ns == mev_ee.tai_ns).expect("a sample exactly at the burn epoch (EE)");

    // Sanity: the mean *did* get perturbed (Sampled applies the drawn dv, not the commanded
    // one) -- otherwise this test would trivially pass for the wrong reason, e.g. a broken seed
    // lookup silently falling back to the commanded dv.
    assert_ne!(at_burn_ee.mean, at_burn_none.mean, "Sampled must apply a perturbed dv on the covariance path too, or this test proves nothing");

    // But P must be bit-identical to the same run with the block entirely absent: exactly
    // nothing injected.
    assert_eq!(at_burn_ee.cov, at_burn_none.cov, "a Sampled covariance run must inject exactly nothing into P -- the dispersion is already realized in the sampled mean");
}

// ----------------------------------------------------------------------------------------
// 7. Fresh substream per event id, end to end through the full executor.
// ----------------------------------------------------------------------------------------

#[test]
fn adding_a_second_maneuver_event_does_not_shift_the_first_ones_sampled_draw() {
    let _engine = gmat_sys::engine_lock();
    let sys = accel_system("gates_indep_sys");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.4, sigma_magnitude_proportional: 0.0, sigma_pointing_fixed_mps: 0.2, sigma_pointing_proportional_rad: 0.0, seed: "shared".to_string() };
    let seeds = BTreeMap::from([("shared".to_string(), 777u64)]);
    let event_a = maneuver_event_ee("burn_a", 1_000_000_000, "veh_a", [0.0, 5.0, 0.0], "AXES_KIND_ICRF", Some(ee.clone()));
    let event_b = maneuver_event_ee("burn_b", 1_000_000_000, "veh_b", [0.0, -3.0, 0.0], "AXES_KIND_ICRF", Some(ee));

    // Run "solo": only burn_a's own instance and event are present.
    let sos_solo = hashed_sos(SosConfiguration { id: "gates_indep_sos_solo".to_string(), instances: vec![model_instance("veh_a", "gates_indep_sys")], ..Default::default() });
    let drm_solo = hashed_drm(DesignReferenceMission {
        id: "gates_indep_drm_solo".to_string(),
        sos_configuration_id: "gates_indep_sos_solo".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, events: vec![event_a.clone()], seeds: seeds.clone(), ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let cfg_solo = RunConfig { gmat: &gmat, drm: &drm_solo, sos: &sos_solo, systems: &systems, run_id: "gates-indep-solo".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_solo = execute(cfg_solo).expect("solo run executes");

    // Run "both": burn_a's and burn_b's own instances and events are both present, and both
    // execution_error blocks name the *same* Scenario.seeds key ("shared") -- the case
    // question 100 calls out: two events sharing one named seed still get unrelated substreams,
    // keyed additionally by each event's own id.
    let sos_both = hashed_sos(SosConfiguration {
        id: "gates_indep_sos_both".to_string(),
        instances: vec![model_instance("veh_a", "gates_indep_sys"), model_instance("veh_b", "gates_indep_sys")],
        ..Default::default()
    });
    let drm_both = hashed_drm(DesignReferenceMission {
        id: "gates_indep_drm_both".to_string(),
        sos_configuration_id: "gates_indep_sos_both".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, events: vec![event_a, event_b], seeds, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    let cfg_both = RunConfig { gmat: &gmat, drm: &drm_both, sos: &sos_both, systems: &systems, run_id: "gates-indep-both".to_string(), error_mode: ExecutionErrorMode::Sampled };
    let products_both = execute(cfg_both).expect("both run executes");

    let mev_solo = products_solo.events.iter().find(|e| e.name == "burn_a").expect("solo run's maneuver event");
    let mev_both = products_both.events.iter().find(|e| e.name == "burn_a").expect("both run's burn_a maneuver event");
    assert_eq!(mev_solo.values, mev_both.values, "adding a second maneuver event (even one sharing the same Scenario.seeds key) must not shift burn_a's own sampled draw");
}

// ----------------------------------------------------------------------------------------
// 8. The covariance path's own wiring: it injects exactly the analytic Gates term.
// ----------------------------------------------------------------------------------------

const COV_P0_DIAG: [f64; 6] = [10_000.0, 10_000.0, 10_000.0, 0.01, 0.01, 0.01];
fn cov_p0() -> Vec<f64> {
    let mut p0 = vec![0.0; 36];
    for (i, d) in COV_P0_DIAG.into_iter().enumerate() {
        p0[i * 6 + i] = d;
    }
    p0
}

fn cov_leo_sos_drm(sos_id: &str, drm_id: &str, instance: &str, dv: [f64; 3], execution_error: Option<ManeuverExecutionError>, seeds: BTreeMap<String, u64>) -> (SosConfiguration, DesignReferenceMission) {
    let mut instance_def = model_instance(instance, "leo_sys");
    instance_def.initial_covariance = cov_p0();
    let sos = hashed_sos(SosConfiguration { id: sos_id.to_string(), instances: vec![instance_def], ..Default::default() });
    // Same start epoch the golden fixtures use; the burn lands on the first output period
    // (0.1 s) so the post-burn sample -- Phi(seg_start, seg_start) = I by StmAugmented::seed's
    // own construction -- carries exactly P0's propagated-then-injected value, no further
    // integration to entangle with the comparison below.
    let start = 1_767_225_637_000_000_000i64;
    let burn_tai = start + 100_000_000;
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: start, end_tai_ns: start + 200_000_000, events: vec![maneuver_event_ee("burn1", burn_tai, instance, dv, "AXES_KIND_ICRF", execution_error)], seeds, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

#[test]
fn the_covariance_path_injects_exactly_the_analytic_gates_term_at_the_burn_epoch() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let dv = [20.0, 0.0, 0.0];
    let ee = ManeuverExecutionError { sigma_magnitude_fixed_mps: 0.05, sigma_magnitude_proportional: 0.002, sigma_pointing_fixed_mps: 0.03, sigma_pointing_proportional_rad: 0.001, seed: "s".to_string() };
    let seeds = BTreeMap::from([("s".to_string(), 99u64)]);

    let (sos_none, drm_none) = cov_leo_sos_drm("gates_cov_wiring_sos_none", "gates_cov_wiring_drm_none", "leo_cov_wiring_none", dv, None, BTreeMap::new());
    let cfg_none = RunConfig { gmat: &gmat, drm: &drm_none, sos: &sos_none, systems: &systems, run_id: "gates-cov-none".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_none = execute(cfg_none).expect("no-execution-error covariance run executes");

    let (sos_ee, drm_ee) = cov_leo_sos_drm("gates_cov_wiring_sos_ee", "gates_cov_wiring_drm_ee", "leo_cov_wiring_ee", dv, Some(ee.clone()), seeds);
    let cfg_ee = RunConfig { gmat: &gmat, drm: &drm_ee, sos: &sos_ee, systems: &systems, run_id: "gates-cov-ee".to_string(), error_mode: ExecutionErrorMode::Nominal };
    let products_ee = execute(cfg_ee).expect("with-execution-error covariance run executes");

    let mev_none = products_none.events.iter().find(|e| e.name == "burn1").expect("no-EE maneuver event");
    let mev_ee = products_ee.events.iter().find(|e| e.name == "burn1").expect("EE maneuver event");
    let traj_none = products_none.trajectories.get("leo_cov_wiring_none").expect("no-EE trajectory");
    let traj_ee = products_ee.trajectories.get("leo_cov_wiring_ee").expect("EE trajectory");
    let at_burn_none = traj_none.samples.iter().find(|s| s.tai_ns == mev_none.tai_ns).expect("a sample exactly at the burn epoch (no EE)");
    let at_burn_ee = traj_ee.samples.iter().find(|s| s.tai_ns == mev_ee.tai_ns).expect("a sample exactly at the burn epoch (EE)");

    // Path (b): the commanded dv is applied exactly, execution error never perturbs the mean.
    assert_eq!(at_burn_none.mean, at_burn_ee.mean, "the covariance path must apply the commanded dv exactly regardless of execution_error");

    // The analytic injection, computed independently through the same production functions
    // (`inertial_triad`/`inject_gates_covariance`) the executor itself calls.
    let parsed_ee = ParsedExecutionError {
        sigma_magnitude_fixed_mps: ee.sigma_magnitude_fixed_mps,
        sigma_magnitude_proportional: ee.sigma_magnitude_proportional,
        sigma_pointing_fixed_mps: ee.sigma_pointing_fixed_mps,
        sigma_pointing_proportional_rad: ee.sigma_pointing_proportional_rad,
        seed: ee.seed,
    };
    let v = (dv[0] * dv[0] + dv[1] * dv[1] + dv[2] * dv[2]).sqrt();
    let (sigma_m, sigma_p) = maneuver::gates_sigmas(&parsed_ee, v);
    // dv is declared AXES_KIND_ICRF -- dv_to_inertial is the identity for that axes kind, so
    // the r/v this triad is built against are irrelevant to the result (unlike VNB/RIC/VVLH).
    let triad = maneuver::inertial_triad(AxesKind::Icrf, dv, [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    let mut expected_delta = vec![0.0f64; 36];
    maneuver::inject_gates_covariance(&mut expected_delta, 6, sigma_m, sigma_p, triad);

    let mut max_rel_err = 0.0f64;
    for ((ee_cov, none_cov), expected) in at_burn_ee.cov.iter().zip(at_burn_none.cov.iter()).zip(expected_delta.iter()) {
        let measured_delta = ee_cov - none_cov;
        let scale = expected.abs().max(none_cov.abs()).max(1e-20);
        let rel_err = (measured_delta - expected).abs() / scale;
        max_rel_err = max_rel_err.max(rel_err);
    }
    eprintln!("[gates covariance wiring] max relative error between the executor's P+ - P- and the analytic G Q G^T = {max_rel_err:.3e}");
    assert!(max_rel_err < 1e-9, "the executor's covariance path must inject exactly G Q G^T at the burn boundary -- max relative error {max_rel_err:.3e}");

    av_cdm::covariance::check_spd_row_major(&at_burn_ee.cov, 6, "gates covariance wiring test").expect("SPD hygiene after the Gates injection");
}

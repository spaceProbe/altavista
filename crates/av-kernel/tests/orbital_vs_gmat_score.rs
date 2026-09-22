//! N6 (`docs/native-dynamics-plan.md`)'s own round-4 open item: "a run of the demo DRM with
//! each [model] and the difference reported in the run products as a score" --
//! `orbital_no_gmat_demo.rs` delivered the native-model-alone half; this file is the two-model
//! half. One [`av_kernel::drm::execute`] call, **one products set**, two `SystemInstance`s
//! declaring the field-for-field IDENTICAL physics (`force_model.central_body`/
//! `.gravity_file`/`.gravity_degree`/`.gravity_order`/`.point_masses`,
//! `spacecraft.CoordinateSystem`, `spacecraft.DisplayStateType = "Cartesian"`, and the same
//! six-element Cartesian initial state) against two different `dynamics_model` bindings --
//! `"orbital.jgm2_8x8_sun_moon"` (`ModelKind::Orbital`, the native `av-orbital` model) and
//! `"gmat.earth.jgm2_8x8.sun_moon"` (`ModelKind::Gmat`) -- and two declared
//! `MeasureOfEffectiveness`s (ADR-005 sec 6's amended grammar, `crate::expr::eval`'s own module
//! doc comment) that score the position difference between them as `range(orbital, gmat)`'s own
//! `max`/`final` reduction, in `RunProducts.scores`:
//!
//! - `native_vs_gmat_position_difference_max_m` = `max(range(orbital, gmat))`
//! - `native_vs_gmat_position_difference_final_m` = `final(range(orbital, gmat))`
//!
//! Both left with `MeasureOfEffectiveness.unit` at `UNIT_UNSPECIFIED` deliberately (`crate::
//! expr::objective`'s own module doc comment: a declared unit is checked for equality against
//! the expression's own propagated unit, never used to convert) -- `range`'s own propagated
//! unit is metres (`crate::expr::eval`'s module doc comment), and `RunProducts.scores[name]
//! .unit` therefore comes out as `UNIT_METER` from the evaluator itself, not from anything
//! written here.
//!
//! **The Keplerian -> Cartesian conversion, the golden physics constants, and the `SystemDefinition`
//! shape below are copied from `tests/orbital_no_gmat_demo.rs::orbital_demo_bundle`/
//! `keplerian_to_cartesian_km`** (that file's own module doc comment has the full derivation:
//! `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `initial_state`, JGM2's own `mu`, and why the
//! conversion is written out independently of `av_orbital`/GMAT rather than reused from either).
//! Not factored into a shared helper module: this file and `orbital_no_gmat_demo.rs` are two
//! independent, self-contained proofs (the brief for this task says so explicitly) -- a shared
//! module would make a bug in the conversion invisible to whichever file did not happen to catch
//! it first.
//!
//! ## Arc duration: 7200 s (2 h), not the full 86,400 s (1 day) golden span
//!
//! Chosen, deliberately, shorter than the demo DRM's own full one-day span (disclosed here
//! rather than silently substituted): this repository's own `scripts/dev/cargo-slot` two-slot
//! budget is shared host-wide with whichever other track is building concurrently this round
//! (this task's own report saw a 327 s slot wait on one attempt), and this file's own two-
//! instance run adds a SECOND real `gmat_sys::Gmat`-backed propagation on top of every existing
//! GMAT test already in this suite -- 7200 s is the same span `tests/demo_two_instance.rs`'s own
//! real committed fixture already uses for a two-instance GMAT run, for the identical reason
//! (that file's own module doc comment). At a 300 s step (24 native steps, roughly 1.3 LEO
//! periods -- period ~5580 s for this arc's own SMA, `orbital_no_gmat_demo.rs`'s own comment),
//! enough for `range(orbital, gmat)` to move through a full orbit's worth of relative-position
//! values, not a single, possibly-coincidental sample.
//!
//! **Measured wall time** (this task's own report has the full `--nocapture` transcript): the
//! test binary itself reports the one `#[test]` function here -- `gmat_sys::engine_lock()` +
//! `Gmat::setup` + one `execute()` propagating both instances + the two `MeasureOfEffectiveness`
//! evaluations -- completing in **43.08 s** wall time on this host (`cargo test`'s own "finished
//! in 43.08s" line, separate from compilation time, which this task's report also has in full).
//! N6's demo-DRM-with-both-models exit criterion is about the SCORE existing and being computed
//! correctly from two real, independently propagated trajectories, not about reproducing the
//! full one-day golden window a second time through GMAT, so this shorter, honestly-disclosed
//! span satisfies it without the extra cost.

use std::collections::BTreeMap;

use av_cdm::pb::{Binding, BindingKind, DesignReferenceMission, DrmOptions, MeasureOfEffectiveness, ModelBinding, Parameter, Scenario, SosConfiguration, StateComponent, StateSpace, SystemDefinition, SystemInstance, Unit};
use av_kernel::drm::{execute, hash, RunConfig};

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

/// Copied verbatim from `tests/orbital_no_gmat_demo.rs::keplerian_to_cartesian_km` (see this
/// file's own module doc comment for why this is a copy, not a shared helper). Standard
/// Keplerian -> Cartesian conversion (Vallado's own `R = R3(-Omega) R1(-i) R3(-omega)`
/// composition). Angles in degrees, `sma_km` in kilometres; returns `[X, Y, Z, VX, VY, VZ]` in
/// km / km/s (GMAT's own `spacecraft.*` unit convention).
fn keplerian_to_cartesian_km(sma_km: f64, ecc: f64, inc_deg: f64, raan_deg: f64, aop_deg: f64, ta_deg: f64, mu_m3_s2: f64) -> [f64; 6] {
    let inc = inc_deg.to_radians();
    let raan = raan_deg.to_radians();
    let aop = aop_deg.to_radians();
    let ta = ta_deg.to_radians();
    let a_m = sma_km * 1000.0;
    let p = a_m * (1.0 - ecc * ecc);
    let r = p / (1.0 + ecc * ta.cos());
    let r_pf = [r * ta.cos(), r * ta.sin()];
    let h = (mu_m3_s2 * p).sqrt();
    let v_pf = [-(mu_m3_s2 / h) * ta.sin(), (mu_m3_s2 / h) * (ecc + ta.cos())];

    let (cr, sr) = (raan.cos(), raan.sin());
    let (ci, si) = (inc.cos(), inc.sin());
    let (co, so) = (aop.cos(), aop.sin());
    let r11 = cr * co - sr * so * ci;
    let r12 = -cr * so - sr * co * ci;
    let r21 = sr * co + cr * so * ci;
    let r22 = -sr * so + cr * co * ci;
    let r31 = so * si;
    let r32 = co * si;
    let rotate = |v: [f64; 2]| -> [f64; 3] { [r11 * v[0] + r12 * v[1], r21 * v[0] + r22 * v[1], r31 * v[0] + r32 * v[1]] };
    let pos_m = rotate(r_pf);
    let vel_ms = rotate(v_pf);
    [pos_m[0] / 1000.0, pos_m[1] / 1000.0, pos_m[2] / 1000.0, vel_ms[0] / 1000.0, vel_ms[1] / 1000.0, vel_ms[2] / 1000.0]
}

/// The `gmat.orbital.cartesian6` `StateSpace`, declared explicitly on both `SystemDefinition`s
/// below (question 94, M9.2: additive to and authoritative over `state_space_id`) -- field-for-
/// field identical to `orbital_no_gmat_demo.rs::orbital_demo_bundle`'s own declaration, factored
/// out here only because both of this file's two systems need the identical value.
fn cartesian6_state_space() -> StateSpace {
    StateSpace {
        id: "gmat.orbital.cartesian6".to_string(),
        components: vec![
            StateComponent { label: "pos_x".to_string(), unit: Unit::Meter as i32 },
            StateComponent { label: "pos_y".to_string(), unit: Unit::Meter as i32 },
            StateComponent { label: "pos_z".to_string(), unit: Unit::Meter as i32 },
            StateComponent { label: "vel_x".to_string(), unit: Unit::MeterPerSecond as i32 },
            StateComponent { label: "vel_y".to_string(), unit: Unit::MeterPerSecond as i32 },
            StateComponent { label: "vel_z".to_string(), unit: Unit::MeterPerSecond as i32 },
        ],
        ..Default::default()
    }
}

/// Build the two-instance, one-products-set DRM: `"orbital"` (`dynamics_model =
/// "orbital.jgm2_8x8_sun_moon"`, `ModelKind::Orbital`) and `"gmat"` (`dynamics_model =
/// "gmat.earth.jgm2_8x8.sun_moon"`, `ModelKind::Gmat`), both declaring identical JGM2 8x8 +
/// Sun/Moon physics and the identical Cartesian initial state -- the same golden orbital
/// elements (`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s own `initial_state`: 6878 km / 0.001 /
/// 51.6 deg / 30 deg / 0 / 0) `orbital_no_gmat_demo.rs::orbital_demo_bundle` converts, converted
/// again here (independently -- see this file's own module doc comment) rather than imported,
/// so this file's own input state does not depend on that file's module being compiled.
fn two_model_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let mu_jgm2 = 3.986004415e14_f64;
    let x0_km = keplerian_to_cartesian_km(6878.0, 0.001, 51.6, 30.0, 0.0, 0.0, mu_jgm2);

    // `force_model.*`/`spacecraft.CoordinateSystem`/`.DisplayStateType`/the six Cartesian state
    // fields are IDENTICAL string/numeric values across both systems below -- N6's own "same
    // model, identical parameters" requirement, and this task's own brief.
    let force_model_and_state_params = || -> Vec<Parameter> {
        vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            sparam("force_model.point_masses", "Luna,Sun"),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Cartesian"),
            param("spacecraft.X", x0_km[0]),
            param("spacecraft.Y", x0_km[1]),
            param("spacecraft.Z", x0_km[2]),
            param("spacecraft.VX", x0_km[3]),
            param("spacecraft.VY", x0_km[4]),
            param("spacecraft.VZ", x0_km[5]),
        ]
    };

    let orbital_sys = hashed_system(SystemDefinition {
        id: "orbital_sys".to_string(),
        version: "1".to_string(),
        name: "LEO JGM2 8x8 + Sun/Moon, native orbital model (N6 two-model score)".to_string(),
        dynamics_model: "orbital.jgm2_8x8_sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        state_space: Some(cartesian6_state_space()),
        parameters: force_model_and_state_params(),
        ..Default::default()
    });
    let gmat_sys = hashed_system(SystemDefinition {
        id: "gmat_sys".to_string(),
        version: "1".to_string(),
        name: "LEO JGM2 8x8 + Sun/Moon, real GMAT model (N6 two-model score)".to_string(),
        dynamics_model: "gmat.earth.jgm2_8x8.sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        state_space: Some(cartesian6_state_space()),
        parameters: force_model_and_state_params(),
        ..Default::default()
    });

    // 300 s step for both instances -- the same rate `orbital_no_gmat_demo.rs`'s own
    // `orbital_demo_bundle` uses (that file's own comment: resolves the ~5580 s LEO period many
    // times over without an unnecessarily fine grid), applied identically to the GMAT-bound
    // instance here so neither model gets a finer/coarser integration grid than the other.
    let step_rate_hz = 1.0 / 300.0;
    let sos = hashed_sos(SosConfiguration {
        id: "two_model_sos".to_string(),
        version: "1".to_string(),
        name: "LEO golden physics, native orbital + real GMAT, one products set (N6)".to_string(),
        instances: vec![
            SystemInstance {
                name: "orbital".to_string(),
                system_id: "orbital_sys".to_string(),
                binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "orbital_sys".to_string() })) }),
                step_rate_hz,
                ..Default::default()
            },
            SystemInstance {
                name: "gmat".to_string(),
                system_id: "gmat_sys".to_string(),
                binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "gmat_sys".to_string() })) }),
                step_rate_hz,
                ..Default::default()
            },
        ],
        ..Default::default()
    });

    // Two named measures (this file's own module doc comment) -- `unit` left UNIT_UNSPECIFIED
    // deliberately, see that same doc comment.
    let measures = vec![
        MeasureOfEffectiveness { name: "native_vs_gmat_position_difference_max_m".to_string(), expression: "max(range(orbital, gmat))".to_string(), unit: Unit::Unspecified as i32 },
        MeasureOfEffectiveness { name: "native_vs_gmat_position_difference_final_m".to_string(), expression: "final(range(orbital, gmat))".to_string(), unit: Unit::Unspecified as i32 },
    ];

    // 7200 s (2 h) -- see this file's own module doc comment for the measured wall time this
    // choice is based on. Same start epoch `drms/leo_1day_golden.drm.yaml`/
    // `orbital_no_gmat_demo.rs` both use (2026-01-01T00:00:00Z, that file's own header comment
    // has the leap-second derivation), just a shorter end.
    let drm = hashed_drm(DesignReferenceMission {
        id: "drm_two_model_score".to_string(),
        version: "1".to_string(),
        name: "LEO 2 h, JGM2 8x8 + Sun/Moon, native orbital vs. real GMAT position-difference score (N6)".to_string(),
        sos_configuration_id: "two_model_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_637_000_000_000 + 7_200_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: step_rate_hz, sample_interval_s: 300.0, ..Default::default() }),
        measures,
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(orbital_sys.id.clone(), orbital_sys);
    systems.insert(gmat_sys.id.clone(), gmat_sys);
    (drm, sos, systems)
}

/// N6's own deliverable: the demo DRM's physics run with BOTH models in one products set, the
/// position difference reported as two named `Score`s -- asserted against a real trajectory pair
/// (never `is_ok()`, `orbital_no_gmat_demo.rs`'s own standard, restated in this task's brief).
#[test]
fn demo_drm_runs_both_models_and_scores_their_position_difference() {
    let _engine = gmat_sys::engine_lock();
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup");

    let (drm, sos, systems) = two_model_bundle();
    let cfg = RunConfig {
        gmat: &gmat,
        drm: &drm,
        sos: &sos,
        systems: &systems,
        run_id: "test-orbital-vs-gmat-score".to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    };
    let products = execute(cfg).expect("the two-instance, two-model DRM must execute end to end");

    // (1) Two trajectories, one per instance, each with the expected sample count on the
    // declared output grid, every component finite.
    let traj_orbital = products.trajectories.get("orbital").expect("the \"orbital\" instance produced a trajectory");
    let traj_gmat = products.trajectories.get("gmat").expect("the \"gmat\" instance produced a trajectory");
    assert_eq!(traj_orbital.segments.len(), 1, "no faults declared: exactly one dynamics segment (orbital)");
    assert_eq!(traj_gmat.segments.len(), 1, "no faults declared: exactly one dynamics segment (gmat)");

    // 7200 s at a 300 s step is 24 native steps -- +/- the first/last boundary samples
    // HeteroKernel's own output grid always includes (`orbital_no_gmat_demo.rs`'s own identical
    // reasoning for its own one-day/288-sample count).
    for (label, traj) in [("orbital", traj_orbital), ("gmat", traj_gmat)] {
        assert!(traj.samples.len() >= 24 && traj.samples.len() <= 26, "{label}: expected roughly 24 samples (one every 300 s over 7200 s), got {}", traj.samples.len());
        for s in &traj.samples {
            assert_eq!(s.mean.len(), 6, "{label}: gmat.orbital.cartesian6 is always a 6-element Cartesian state");
            assert!(s.mean.iter().all(|v| v.is_finite()), "{label}: every component of every sample must be finite: {:?}", s.mean);
        }
    }

    // (2) The two segments' own ModelInfo.depth/dynamics_hash distinguish the two models: a
    // consumer reading the products back can tell which model produced which trajectory from
    // data alone. `gmat-sys::model`'s own ModelInfo always reports "gmat-ffi", never "native"
    // (`crates/gmat-sys/src/model.rs`); `av-orbital`'s own reports "native", never a GMAT depth
    // string (`crates/av-orbital/src/model.rs`, the same fact `orbital_no_gmat_demo.rs::
    // orbital_instance_segment_reports_native_depth_not_gmat` already checks for the one-model
    // case).
    assert_eq!(traj_orbital.segments[0].dynamics_depth, "native", "the orbital instance's own segment must report depth \"native\"");
    assert_ne!(traj_gmat.segments[0].dynamics_depth, "native", "the gmat instance's own segment must NOT report depth \"native\" -- got {:?}", traj_gmat.segments[0].dynamics_depth);
    assert_ne!(
        traj_orbital.segments[0].dynamics_hash, traj_gmat.segments[0].dynamics_hash,
        "two different models (even declaring identical physics parameters) must not collide on the same dynamics_hash"
    );

    // (3)/(4) Both named scores present, unit metres, values finite/positive/tolerance-bounded,
    // and max >= final.
    let max_score = products.scores.get("native_vs_gmat_position_difference_max_m").expect("native_vs_gmat_position_difference_max_m evaluated");
    let final_score = products.scores.get("native_vs_gmat_position_difference_final_m").expect("native_vs_gmat_position_difference_final_m evaluated");
    assert_eq!(max_score.unit, Unit::Meter, "native_vs_gmat_position_difference_max_m must be in metres (range's own unit)");
    assert_eq!(final_score.unit, Unit::Meter, "native_vs_gmat_position_difference_final_m must be in metres (range's own unit)");
    assert_eq!(max_score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");
    assert_eq!(final_score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");

    // Full precision, not `{:.6}` (manager review): at six decimals both scores print as
    // "0.000012" and a reader cannot tell whether they are the same f64 or merely close, which
    // is exactly the question the aggregate control below turns on. A measured value this test
    // reports is printed as measured.
    eprintln!(
        "[orbital_vs_gmat_score] native_vs_gmat_position_difference_max_m = {:.17e} m, native_vs_gmat_position_difference_final_m = {:.17e} m",
        max_score.value, final_score.value
    );

    assert!(max_score.value.is_finite(), "max score must be finite, got {}", max_score.value);
    assert!(final_score.value.is_finite(), "final score must be finite, got {}", final_score.value);
    assert!(max_score.value > 0.0, "max score must be strictly greater than zero -- a zero here would mean the same trajectory was scored against itself, the one failure mode that would make this test vacuous; got {}", max_score.value);
    assert!(final_score.value > 0.0, "final score must be strictly greater than zero, for the same reason; got {}", final_score.value);

    // Measured on this host (this task's own report has the exact `--nocapture` transcript this
    // came from): over the 7200 s arc, the native-vs-GMAT position difference was
    // 1.2e-5 m (12 micrometres) at both its worst (max) and at the end (final) -- both models
    // integrate the identical JGM2 8x8 + Sun/Moon force model from the identical initial state
    // over a short (2 h), drag-free arc, so the whole difference is numerical floating-point
    // noise (native Dopri5 vs. GMAT's own integrator, gravity-series evaluation order,
    // floating-point summation order), not a physics disagreement -- consistent with `av-orbital`
    // matching GMAT's own `GetDerivatives` to millimetre-or-better precision elsewhere in this
    // codebase (`docs/native-dynamics-plan.md`'s own M5 arc figure, 68.32 m native residual, is
    // for a much longer, DRAG-inclusive arc, where nonlinear drag sensitivity and a full day of
    // accumulated integration drift both apply -- not a directly comparable number, only the
    // order-of-magnitude context that task's own report already offers). `MAX_DIFFERENCE_
    // TOLERANCE_M`/`FINAL_DIFFERENCE_TOLERANCE_M` below are set at 1000x the measured 1.2e-5 m
    // value (0.012 m, 1.2 cm) -- generous enough to never flake on ordinary floating-point/build/
    // architecture noise, tight enough that a genuine regression (a re-bind bug, a swapped
    // force-model constant, a units error, a wrong initial state) -- which would show up at the
    // metre level, `docs/native-dynamics-plan.md`'s own scale for "something actually changed"
    // -- would still be caught by three orders of magnitude of headroom.
    const MAX_DIFFERENCE_TOLERANCE_M: f64 = 0.012;
    const FINAL_DIFFERENCE_TOLERANCE_M: f64 = 0.012;
    assert!(max_score.value < MAX_DIFFERENCE_TOLERANCE_M, "native_vs_gmat_position_difference_max_m {} m exceeds the measured tolerance {} m", max_score.value, MAX_DIFFERENCE_TOLERANCE_M);
    assert!(final_score.value < FINAL_DIFFERENCE_TOLERANCE_M, "native_vs_gmat_position_difference_final_m {} m exceeds the measured tolerance {} m", final_score.value, FINAL_DIFFERENCE_TOLERANCE_M);

    // max over a series that contains the final sample cannot be smaller than that sample.
    assert!(max_score.value >= final_score.value, "max ({} m) must be >= final ({} m): a max over a series containing the final sample cannot be smaller", max_score.value, final_score.value);

    // **What the assertion above proves, established by measurement rather than by argument
    // (manager review).** The reviewer's first reading was that it proves nothing: two
    // integrators from the same state ought to diverge monotonically over a short drag-free arc,
    // in which case the maximum is attained at the last sample, the two scores are the same f64,
    // and swapping the two expressions would produce an identical pair of numbers that no
    // comparison between them could catch. Printing both at full precision instead of `{:.6}`
    // falsified that reading immediately -- measured on this host:
    //
    //     max   = 1.24971901366127523e-5 m
    //     final = 1.21115561183108794e-5 m
    //
    // The series is NOT monotone: the difference carries an orbital-period ripple (the two
    // integrators differ in phase along the orbit as well as in magnitude), so the maximum falls
    // strictly inside the window and `max > final` by about 3.9e-7 m. The inequality above is
    // therefore a real control -- a swap makes max 1.211e-5 and final 1.250e-5, and it fails.
    // Recorded at length because the wrong reading was the plausible one, and because six-decimal
    // formatting had hidden the distinction: both scores printed as "0.000012".
    //
    // The INDEPENDENT recomputation below stands on its own merits regardless: the range series
    // is rebuilt here from the two trajectories' own samples -- plain Euclidean distance between
    // the position triples, never `crate::expr` -- and both scores are checked against it. It
    // fails against a wrong entity pair, a wrong component set, a units error (metres vs
    // kilometres), an off-by-one in the aggregate's sample window, and any evaluator bug that
    // moves a value. Measured: it agrees with the evaluator to all 17 significant figures on
    // both scores.
    assert_eq!(
        traj_orbital.samples.len(),
        traj_gmat.samples.len(),
        "both instances share one output grid, so the two trajectories must have the same sample count ({} vs {})",
        traj_orbital.samples.len(),
        traj_gmat.samples.len()
    );
    let range_series: Vec<f64> = traj_orbital
        .samples
        .iter()
        .zip(traj_gmat.samples.iter())
        .map(|(o, g)| {
            let (dx, dy, dz) = (o.mean[0] - g.mean[0], o.mean[1] - g.mean[1], o.mean[2] - g.mean[2]);
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .collect();
    let recomputed_max = range_series.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let recomputed_final = *range_series.last().expect("the range series has at least one sample");
    eprintln!(
        "[orbital_vs_gmat_score] independently recomputed: max = {:.17e} m, final = {:.17e} m, over {} samples",
        recomputed_max,
        recomputed_final,
        range_series.len()
    );
    // Bit-exact would be over-specified: the evaluator may sum or select in a different order.
    // A relative 1e-12 is far tighter than any defect this control is aimed at (all of which move
    // the value by orders of magnitude) and far looser than f64 reassociation noise.
    for (what, scored, recomputed) in [
        ("max", max_score.value, recomputed_max),
        ("final", final_score.value, recomputed_final),
    ] {
        let rel = (scored - recomputed).abs() / recomputed.abs().max(f64::MIN_POSITIVE);
        assert!(
            rel < 1e-12,
            "the {what} score the evaluator produced ({scored:.17e} m) disagrees with this test's own \
             independent recomputation from the two trajectories ({recomputed:.17e} m), relative {rel:.3e}"
        );
    }

    // (5) Provenance carries this run's own id and the DRM's own hash -- proves this really went
    // through the real execute() pipeline, not a stub (copied from orbital_no_gmat_demo.rs).
    assert_eq!(traj_orbital.config_hash, drm.hash);
    assert_eq!(traj_gmat.config_hash, drm.hash);
    assert_eq!(products.provenance.run_id, "test-orbital-vs-gmat-score");
}

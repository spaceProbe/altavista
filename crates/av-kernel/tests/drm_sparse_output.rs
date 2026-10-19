//! Question 254 item 14: an output a model emits only on some steps keeps its OWN epochs.
//!
//! `GroundStationModel` emits `elevation_rad` only on a step that decoded a telemetry frame
//! (`in_contact`/`frames_handled` every step). Before this fix the scheduler pushed one shared
//! epoch per step but a name's value only when the model produced it, so a sparse name ended with
//! more epochs than values: `output.ground.elevation_rad@end` indexed past the values (a panic in
//! `ExprRunProducts::output_at`) and every epoch-aware operator (`@time`, `integral`, `duration`)
//! paired a value with the wrong epoch.
//!
//! **Semantics pinned here** (`crate::expr::eval`'s module doc): an operator over a sparse series
//! uses the series' own samples. `@t` between two samples interpolates linearly (what `output_at`
//! already does for a dense series); `@t` outside the series' span is `TimeOutOfRange`;
//! `min`/`max`/`mean`/`final` reduce over the samples; `integral` is the trapezoid over the
//! samples (an interval between two non-adjacent samples is one trapezoid); `duration` holds each
//! sample's verdict to the next sample.
//!
//! **Independence.** Nothing here calls `ground::elevation_of` or any code under test. The flight
//! instance is the closed-form `p(t) = p0 + v0 t` (no acceleration, `drms/demo_ground_segment_
//! flight.system.yaml`), broadcasting its post-step position every 1 s step; the link latency
//! (the SOS connection's two summed 50 ms port latencies, 100 ms) is shorter than the 1 s receiver
//! step, so the frame emitted at second `k` is decoded by the ground step ending at second `k + 1`. The elevation of a frame is recomputed below with this file's own
//! WGS84-ellipsoid-normal formula (`asin(d . n / |d|)`), not the model's ENU-rotation path, from
//! the site and `p0`/`v0` read out of the fixture YAML.
//!
//! The expected sample set, derived from the above (and checked against the executor's result,
//! not assumed from it): no frame arrives at the first step (second 1), the frame emitted at the
//! last step (second 900) is never delivered, so the elevation samples sit at seconds 2..=900
//! (899 of them, against 900 shared epochs). The gap variant additionally drops, with a declared
//! PORT `drop` fault on `flight.tm_out` over `[400.5 s, 500.5 s)`, the frames emitted at seconds
//! 401..=500, so the samples sit at seconds 2..=401 and 502..=900 (799), a 101 s hole mid-pass
//! where the elevation is near its 84 degree peak.
//!
//! Every test takes `gmat_sys::engine_lock()` and constructs a `Gmat` (`execute` takes `&Gmat`
//! unconditionally; this DRM binds no `"gmat."` instance).

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, Fault, FaultTargetKind, MeasureOfEffectiveness, SosConfiguration, SystemDefinition, Unit};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig, RunProducts};
use gmat_sys::Gmat;

fn read(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

fn param(sys: &SystemDefinition, name: &str) -> f64 {
    sys.parameters.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{} declares {name}", sys.id)).value
}

fn measure(name: &str, expression: &str, unit: Unit) -> MeasureOfEffectiveness {
    MeasureOfEffectiveness { name: name.to_string(), expression: expression.to_string(), unit: unit as i32 }
}

/// `elevation_unit`: the ground instance's declared `output.elevation_rad` unit (the fixture's
/// radian; `Dimensionless` for the `integral` variant, since `radian * second` has no declared CDM
/// unit -- `crate::expr::units::mul_table` -- while `dimensionless * second` is `second`).
/// `drop_window`: `Some((start_s, end_s))` adds the PORT `drop` fault on `flight.tm_out`.
fn run(measures: Vec<MeasureOfEffectiveness>, elevation_unit: Unit, drop_window: Option<(f64, f64)>) -> Result<RunProducts, DrmError> {
    let _engine = gmat_sys::engine_lock();
    let mut drm: DesignReferenceMission = schema::parse_drm_yaml(&read("demo_ground_segment.drm.yaml")).expect("DRM parses");
    let sos: SosConfiguration = schema::parse_sos_yaml(&read("demo_ground_segment.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_ground_segment_flight");
    let mut ground = load_system("demo_ground_segment_ground");
    ground.parameters.iter_mut().find(|p| p.name == "output.elevation_rad").expect("declared").unit = elevation_unit as i32;
    ground.hash = hash::canonical_system_hash(&ground);
    let start = drm.scenario.as_ref().expect("scenario").start_tai_ns;
    if let Some((from_s, to_s)) = drop_window {
        let scenario = drm.scenario.as_mut().expect("scenario");
        scenario.seeds.insert("drop_tm".to_string(), 7);
        scenario.faults.push(Fault {
            id: "drop_tm".to_string(),
            tai_ns: start + (from_s * 1e9) as i64,
            duration_ns: ((to_s - from_s) * 1e9) as i64,
            target_kind: FaultTargetKind::Port as i32,
            instance: "flight".to_string(),
            target: "tm_out".to_string(),
            kind: "drop".to_string(),
            ..Default::default()
        });
    }
    drm.measures = measures;
    drm.hash = hash::canonical_drm_hash(&drm);
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let config = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-sparse-output".to_string(), error_mode: Default::default(), products_dir: None, replay: None, command_source: None };
    execute(config)
}

// ---- the independent hand model -------------------------------------------------------------

/// The ellipsoid-normal elevation: `asin(d . n / |d|)`, `d = target - site` (ECEF, m), `n` the unit
/// ellipsoid normal at the site (geodetic latitude/longitude). Written out here from the WGS84
/// constants, not shared with `av_kernel::drm::ground`.
fn hand_elevation(site: &SystemDefinition, target: [f64; 3]) -> f64 {
    let (lat, lon, h) = (param(site, "station.latitude_rad"), param(site, "station.longitude_rad"), param(site, "station.height_m"));
    let (a, f) = (6_378_137.0_f64, 1.0 / 298.257223563);
    let e2 = f * (2.0 - f);
    let n_radius = a / (1.0 - e2 * lat.sin().powi(2)).sqrt();
    let s = [(n_radius + h) * lat.cos() * lon.cos(), (n_radius + h) * lat.cos() * lon.sin(), (n_radius * (1.0 - e2) + h) * lat.sin()];
    let d = [target[0] - s[0], target[1] - s[1], target[2] - s[2]];
    let n = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
    let dot = d[0] * n[0] + d[1] * n[1] + d[2] * n[2];
    (dot / (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()).asin()
}

/// `(second since start, elevation)` of every sample the station must have produced: the frame
/// emitted at second `k` (position `p0 + v0 k`) is decoded by the step ending at second `k + 1`.
/// `dropped_emissions`: the emission seconds a drop fault removes.
fn hand_samples(dropped_emissions: Option<std::ops::RangeInclusive<i64>>) -> Vec<(i64, f64)> {
    let flight = load_system("demo_ground_segment_flight");
    let ground = load_system("demo_ground_segment_ground");
    let p0 = [param(&flight, "state.px"), param(&flight, "state.py"), param(&flight, "state.pz")];
    let v0 = [param(&flight, "state.vx"), param(&flight, "state.vy"), param(&flight, "state.vz")];
    // Emissions at seconds 1..=900; the one at 900 has no later receiver step inside the run.
    (1..=899_i64)
        .filter(|k| !dropped_emissions.as_ref().is_some_and(|d| d.contains(k)))
        .map(|k| {
            let t = k as f64;
            (k + 1, hand_elevation(&ground, [p0[0] + v0[0] * t, p0[1] + v0[1] * t, p0[2] + v0[2] * t]))
        })
        .collect()
}

fn close(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() <= 1e-9 * want.abs().max(1.0), "{what}: got {got}, hand computation {want}");
}

fn score(products: &RunProducts, name: &str) -> f64 {
    products.scores.get(name).unwrap_or_else(|| panic!("measure {name} scored")).value
}

/// Linear interpolation between the two samples bracketing second `t` (the rule `output_at` has
/// always applied to a dense series).
fn hand_interpolate(samples: &[(i64, f64)], t: i64) -> f64 {
    let hi = samples.iter().position(|(s, _)| *s >= t).expect("t inside the span");
    let ((t0, v0), (t1, v1)) = (samples[hi - 1], samples[hi]);
    v0 + (v1 - v0) * (t - t0) as f64 / (t1 - t0) as f64
}

fn hand_integral(samples: &[(i64, f64)]) -> f64 {
    samples.windows(2).map(|w| 0.5 * (w[0].1 + w[1].1) * (w[1].0 - w[0].0) as f64).sum()
}

fn hand_duration_above(samples: &[(i64, f64)], threshold: f64) -> f64 {
    samples.windows(2).filter(|w| w[0].1 > threshold).map(|w| (w[1].0 - w[0].0) as f64).sum()
}

// ---- the proofs -----------------------------------------------------------------------------

/// No hole mid-run: only the first step (no frame yet) and the last emission (never delivered) are
/// missing, i.e. 899 elevation samples against 900 shared epochs. Every operator reads the
/// samples at their own epochs.
#[test]
fn a_sparse_output_scores_on_its_own_samples_when_only_the_first_step_is_missing() {
    let samples = hand_samples(None);
    assert_eq!((samples.len(), samples[0].0, samples.last().unwrap().0), (899, 2, 900), "the hand model's own sample set");
    let products = run(
        vec![
            measure("at_2s", "output.ground.elevation_rad@2 s", Unit::Radian),
            measure("at_100s", "output.ground.elevation_rad@100 s", Unit::Radian),
            measure("at_420s", "output.ground.elevation_rad@420 s", Unit::Radian),
            measure("mean", "mean(output.ground.elevation_rad)", Unit::Radian),
            measure("min", "min(output.ground.elevation_rad)", Unit::Radian),
            measure("max", "max(output.ground.elevation_rad)", Unit::Radian),
            measure("final", "final(output.ground.elevation_rad)", Unit::Radian),
            measure("above_1rad", "duration(output.ground.elevation_rad > 1.0 rad)", Unit::Second),
        ],
        Unit::Radian,
        None,
    )
    .expect("the sparse series scores");
    let at = |s: i64| samples.iter().find(|(t, _)| *t == s).expect("a sample at that second").1;
    println!("hand: @2 {} @100 {} @420 {} mean {} min {} max {} duration>1rad {}", at(2), at(100), at(420), samples.iter().map(|s| s.1).sum::<f64>() / 899.0, samples.iter().map(|s| s.1).fold(f64::INFINITY, f64::min), samples.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max), hand_duration_above(&samples, 1.0));
    // The first sample is at second 2 (index 0 of the values): the one-second shift the shared
    // epochs gave every `@time` read.
    close(score(&products, "at_2s"), at(2), "@2 s");
    close(score(&products, "at_100s"), at(100), "@100 s");
    close(score(&products, "at_420s"), at(420), "@420 s");
    close(score(&products, "mean"), samples.iter().map(|s| s.1).sum::<f64>() / 899.0, "mean");
    close(score(&products, "min"), samples.iter().map(|s| s.1).fold(f64::INFINITY, f64::min), "min");
    close(score(&products, "max"), samples.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max), "max");
    close(score(&products, "final"), at(900), "final");
    close(score(&products, "above_1rad"), hand_duration_above(&samples, 1.0), "duration above 1 rad");
}

/// `@end` of the 899-sample series is its last sample (second 900). Its own test: before the fix
/// this read indexed past the values and panicked, which would hide every other measure's result.
#[test]
fn the_end_of_a_sparse_series_is_its_last_sample() {
    let samples = hand_samples(None);
    let products = run(vec![measure("end", "output.ground.elevation_rad@end", Unit::Radian)], Unit::Radian, None).expect("@end scores");
    println!("hand: @end {}", samples.last().unwrap().1);
    close(score(&products, "end"), samples.last().unwrap().1, "@end");
}

/// A 101 s hole in the middle: 799 samples. `@t` inside the hole interpolates between the two
/// samples around it, the trapezoid and the `duration` hold both span it as one interval.
#[test]
fn a_sparse_output_with_a_mid_run_gap_scores_on_its_own_samples() {
    let samples = hand_samples(Some(401..=500));
    assert_eq!((samples.len(), samples[399].0, samples[400].0), (799, 401, 502), "the hand model's own sample set and the 101 s hole");
    let products = run(
        vec![
            measure("end", "output.ground.elevation_rad@end", Unit::Radian),
            measure("before_gap", "output.ground.elevation_rad@401 s", Unit::Radian),
            measure("in_gap", "output.ground.elevation_rad@450 s", Unit::Radian),
            measure("after_gap", "output.ground.elevation_rad@502 s", Unit::Radian),
            measure("mean", "mean(output.ground.elevation_rad)", Unit::Radian),
            measure("min", "min(output.ground.elevation_rad)", Unit::Radian),
            measure("max", "max(output.ground.elevation_rad)", Unit::Radian),
            measure("above_1rad", "duration(output.ground.elevation_rad > 1.0 rad)", Unit::Second),
            measure("in_contact_in_gap", "output.ground.in_contact@450 s", Unit::Dimensionless),
        ],
        Unit::Radian,
        Some((400.5, 500.5)),
    )
    .expect("the gapped series scores");
    let at = |s: i64| samples.iter().find(|(t, _)| *t == s).expect("a sample at that second").1;
    let (mean, min, max) = (samples.iter().map(|s| s.1).sum::<f64>() / 799.0, samples.iter().map(|s| s.1).fold(f64::INFINITY, f64::min), samples.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max));
    println!("hand: end {} @401 {} @450(interp) {} @502 {} mean {mean} min {min} max {max} duration>1rad {}", at(900), at(401), hand_interpolate(&samples, 450), at(502), hand_duration_above(&samples, 1.0));
    close(score(&products, "end"), at(900), "@end");
    close(score(&products, "before_gap"), at(401), "@401 s");
    close(score(&products, "after_gap"), at(502), "@502 s");
    close(score(&products, "in_gap"), hand_interpolate(&samples, 450), "@450 s, inside the hole");
    close(score(&products, "mean"), mean, "mean");
    close(score(&products, "min"), min, "min");
    close(score(&products, "max"), max, "max");
    // The hole interval [401, 502) starts at a sample above 1 rad and counts as one 101 s hold.
    close(score(&products, "above_1rad"), hand_duration_above(&samples, 1.0), "duration above 1 rad");
    // The dense `in_contact` series (an epoch at every step) is untouched by the sparse neighbour: a gap in telemetry is not a loss of contact.
    assert_eq!(score(&products, "in_contact_in_gap"), 1.0, "in contact (elevation near its peak) through the telemetry gap");
}

/// `integral` needs a unit with a declared product with seconds, so this variant declares the
/// elevation dimensionless (`dimensionless * second = second`): the trapezoid over the samples,
/// the hole being one trapezoid.
#[test]
fn integral_over_a_sparse_output_is_the_trapezoid_over_its_own_samples() {
    let samples = hand_samples(Some(401..=500));
    let products = run(vec![measure("integral", "integral(output.ground.elevation_rad)", Unit::Second)], Unit::Dimensionless, Some((400.5, 500.5))).expect("the integral scores");
    println!("hand: integral {}", hand_integral(&samples));
    close(score(&products, "integral"), hand_integral(&samples), "integral");
    // The hole is one 101 s trapezoid, not 101 one-second ones: a shared-epoch pairing (or a
    // dense assumption) cannot reproduce this number.
    let dense_assumption: f64 = samples.windows(2).map(|w| 0.5 * (w[0].1 + w[1].1)).sum();
    assert!((hand_integral(&samples) - dense_assumption).abs() > 1.0, "the hole must matter to the integral for this test to mean anything");
}

/// A time outside the series' own span is a typed error naming the position, never a panic and
/// never an extrapolation: second 1 is inside the run but before the first elevation sample
/// (second 2).
#[test]
fn a_time_outside_a_sparse_series_span_is_a_typed_error() {
    let err = run(vec![measure("before_first_sample", "output.ground.elevation_rad@1 s", Unit::Radian)], Unit::Radian, None).map(|_| ()).expect_err("@1 s precedes the first elevation sample");
    match err {
        DrmError::InvalidExpression { name, reason } => {
            assert_eq!(name, "before_first_sample");
            assert!(reason.contains("time 1 s is outside"), "{reason}");
        }
        other => panic!("expected a typed InvalidExpression, got {other:?}"),
    }
}

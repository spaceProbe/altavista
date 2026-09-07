//! `Gmat::convert` (ADR-002's fourth amendment, `docs/open-questions.md` question 128): GMAT's
//! own `CoordinateConverter::Convert`, reached through the new `gmatffi_convert_state` shim
//! call. Three things this file pins:
//!
//! - the converted state genuinely matches GMAT's own `ReportFile`-computed `EarthICRF`/
//!   `EarthBodyFixed` state for the same instant (`goldens/icrf_leo_2h.json`/
//!   `goldens/bodyfixed_leo_2h.json`) -- both goldens report GMAT's own `EarthMJ2000Eq` state
//!   *in the same row*, from the same script run, so converting that value and comparing
//!   against the golden's own target-frame column isolates the conversion itself from any
//!   Dopri5-vs-PrinceDormand78 integrator divergence (a real, separate effect this crate's own
//!   `leo_golden.rs` already accepts at a much looser tolerance -- see that test and
//!   `crates/av-kernel/tests/demo_two_instance.rs`'s own end-to-end pins, which do carry that
//!   divergence, for the full-pipeline proof);
//! - a round trip `from -> to -> from` is the identity to 1e-9 m (a required acceptance bar --
//!   note on its own this is a weak test: a silent identity, or any km/m mix consistent between
//!   the two calls, would also pass it -- the golden comparisons above are what actually
//!   exercise the real, physical rotation);
//! - a missing or uninitialized `CoordinateSystem` name is a typed `GmatError`, never a crash
//!   and never a silent identity conversion.
use gmat_sys::Gmat;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct FrameGolden {
    report_last_row: ReportLastRow,
    state_final_km: [f64; 6],
    state_final_km_mj2000eq: [f64; 6],
    tolerance_m: f64,
    tolerance_mps: f64,
}

#[derive(Deserialize)]
struct ReportLastRow {
    epoch_a1mjd: f64,
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

fn load_golden(name: &str) -> FrameGolden {
    serde_json::from_str(&std::fs::read_to_string(golden_path(name)).unwrap()).unwrap()
}

fn dr_dv(a: &[f64; 6], b: &[f64; 6]) -> (f64, f64) {
    let dr = (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt() * 1000.0; // km -> m
    let dv = (3..6).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt() * 1000.0; // km/s -> m/s
    (dr, dv)
}

/// **Required pin: "the LEO golden arc converted to `EarthICRF` matches GMAT's own `ReportFile`
/// in that coordinate system."** `goldens/icrf_leo_2h.json`'s `state_final_km_mj2000eq` and
/// `state_final_km` (ICRF) are GMAT's own two simultaneous reports of the identical propagated
/// physical state (same script run, same `Propagate` command, same instant) -- converting the
/// former through `Gmat::convert` and comparing against the latter isolates the conversion
/// itself, with no independent re-propagation and therefore no integrator divergence at all.
///
/// Fails against: a shim that silently returns its input unchanged (a bare identity -- the
/// ICRF/MJ2000Eq frame bias is ~1.3 m at this altitude, `crates/av-kernel/tests/
/// demo_two_instance.rs`'s own module doc comment measured it directly, far outside
/// `tolerance_m`); a shim with a silent km/m mix (`state_final_km_mj2000eq` fed in as km is
/// correct -- feeding it in as if it were metres, or scaling the output by 1000 in the wrong
/// direction, would miss by three orders of magnitude); or a shim that swaps `from_cs`/`to_cs`
/// (converting MJ2000Eq -> ICRF backwards, i.e. ICRF -> MJ2000Eq, would reintroduce the same
/// ~1.3 m frame-bias-sized error since the rotation is not its own inverse).
#[test]
fn convert_matches_the_genuine_gmat_reportfile_for_earthicrf() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden("icrf_leo_2h");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("ConvertPinMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("ConvertPinIcrf", "Earth", "ICRF").unwrap();
    gmat.initialize().unwrap();

    let converted = gmat.convert(golden.report_last_row.epoch_a1mjd, &golden.state_final_km_mj2000eq, "ConvertPinMj2000Eq", "ConvertPinIcrf").unwrap();
    let (dr_m, dv_mps) = dr_dv(&converted, &golden.state_final_km);
    eprintln!("[convert EarthICRF] |dr| = {dr_m:.6e} m (tol {}), |dv| = {dv_mps:.6e} m/s (tol {})", golden.tolerance_m, golden.tolerance_mps);
    assert!(dr_m < golden.tolerance_m, "EarthICRF position error {dr_m} m exceeds tolerance {} m", golden.tolerance_m);
    assert!(dv_mps < golden.tolerance_mps, "EarthICRF velocity error {dv_mps} m/s exceeds tolerance {} m/s", golden.tolerance_mps);
}

/// **Required pin: "the LEO golden arc converted to `EarthBodyFixed` matches GMAT's own
/// `ReportFile` in that coordinate system."** Same construction as the ICRF pin above, against
/// `goldens/bodyfixed_leo_2h.json`. Deliberately a genuinely different case from ICRF: the
/// body-fixed rotation is time-varying (Earth's own rotation), so a correct implementation must
/// use `report_last_row.epoch_a1mjd` (not, say, the epoch the spacecraft was constructed at, or
/// a fixed epoch) -- see this module doc comment and `docs/adr/002-dynamics-contract.md`'s
/// fourth amendment: "if your epoch handling is wrong, ICRF may still pass while body-fixed
/// fails." Fails against an implementation that ignores the epoch argument (or applies the
/// wrong one): the body-fixed rotation moves by Earth's own ~360deg/day, so a wrong epoch would
/// miss this golden by a large fraction of Earth's radius, not by noise.
///
/// This is also the pin that closes a genuine, pre-existing GMAT modeling restriction this task
/// discovered while wiring `crates/av-kernel/tests/demo_two_instance.rs`'s own end-to-end
/// fixtures: GMAT refuses `DisplayStateType = Keplerian` combined with a non-inertial
/// `spacecraft.CoordinateSystem` ("orbital state elements not contained in the same state
/// type"), and `demo_two_instance.system.yaml`'s `leo_demo_sys` (this repository's shared golden
/// LEO vehicle) declares `DisplayStateType = Keplerian` -- so a `"gmat."`-bound DRM instance
/// cannot declare `spacecraft.CoordinateSystem = "EarthBodyFixed"` while reusing that fixture
/// (an orthogonal, pre-existing GMAT constraint, not a gap in `convert` itself). This direct
/// `Gmat::convert` pin -- operating on a bare Cartesian state vector, with no `Spacecraft`
/// object or `DisplayStateType` involved at all -- is exactly how this repository verifies the
/// `EarthBodyFixed` required pin without needing a DRM instance to hit that restriction; see
/// `demo_two_instance.rs`'s own module doc comment for the full account.
#[test]
fn convert_matches_the_genuine_gmat_reportfile_for_earthbodyfixed() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden("bodyfixed_leo_2h");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("ConvertPinMj2000EqBf", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("ConvertPinBodyFixed", "Earth", "BodyFixed").unwrap();
    gmat.initialize().unwrap();

    let converted = gmat.convert(golden.report_last_row.epoch_a1mjd, &golden.state_final_km_mj2000eq, "ConvertPinMj2000EqBf", "ConvertPinBodyFixed").unwrap();
    let (dr_m, dv_mps) = dr_dv(&converted, &golden.state_final_km);
    eprintln!("[convert EarthBodyFixed] |dr| = {dr_m:.6e} m (tol {}), |dv| = {dv_mps:.6e} m/s (tol {})", golden.tolerance_m, golden.tolerance_mps);
    assert!(dr_m < golden.tolerance_m, "EarthBodyFixed position error {dr_m} m exceeds tolerance {} m", golden.tolerance_m);
    assert!(dv_mps < golden.tolerance_mps, "EarthBodyFixed velocity error {dv_mps} m/s exceeds tolerance {} m/s", golden.tolerance_mps);
}

/// **Required pin: a round trip `from -> to -> from` is the identity to 1e-9 m.** Uses
/// `icrf_leo_2h.json`'s own `state_final_km_mj2000eq` as a real, converged state (no need for it
/// to match any golden itself here). Fails against a conversion whose forward and inverse
/// rotations are not genuine inverses of each other (e.g. a sign error, or the two calls
/// disagreeing on which `CoordinateSystem` is `from`/`to`). Note this test alone cannot catch a
/// silent km/m mix or a silently-wrong epoch (either cancels out identically across a round
/// trip) -- that is exactly why `convert_matches_the_genuine_gmat_reportfile_for_earthicrf`/
/// `..._for_earthbodyfixed` above exist too.
#[test]
fn round_trip_from_mj2000eq_to_icrf_and_back_is_the_identity_to_1e_minus_9_m() {
    let _engine = gmat_sys::engine_lock();
    let golden = load_golden("icrf_leo_2h");
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    gmat.coordinate_system("RoundTripMj2000Eq", "Earth", "MJ2000Eq").unwrap();
    gmat.coordinate_system("RoundTripIcrf", "Earth", "ICRF").unwrap();
    gmat.initialize().unwrap();

    let original = golden.state_final_km_mj2000eq;
    let epoch = golden.report_last_row.epoch_a1mjd;
    let to_icrf = gmat.convert(epoch, &original, "RoundTripMj2000Eq", "RoundTripIcrf").unwrap();
    let back = gmat.convert(epoch, &to_icrf, "RoundTripIcrf", "RoundTripMj2000Eq").unwrap();

    let (dr_m, dv_mps) = dr_dv(&back, &original);
    eprintln!("[round_trip] |dr| = {dr_m:.3e} m, |dv| = {dv_mps:.3e} m/s (both must be < 1e-9 m)");
    assert!(dr_m < 1e-9, "round-trip position residual {dr_m} m exceeds 1e-9 m");
    assert!(dv_mps < 1e-9, "round-trip velocity residual {dv_mps} m/s exceeds 1e-9 m/s");
}

/// A `from_cs`/`to_cs` name GMAT has never heard of is a typed `GmatError` (`gmatffi_last_error`
/// carries GMAT's own "not registered" message), never a crash and never a silent identity
/// conversion -- `shim/gmatffi.h`'s own doc comment on `gmatffi_convert_state`. Fails against an
/// implementation that constructs a CoordinateSystem on the fly for an unrecognized name (a
/// silent, uninitialized/default-axes system -- exactly the "never a silent identity" rule this
/// task's brief forbids) instead of refusing.
#[test]
fn convert_refuses_an_unregistered_coordinate_system_name() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    gmat.coordinate_system("RefuseTestReal", "Earth", "MJ2000Eq").unwrap();
    gmat.initialize().unwrap();

    let state = [7000.0, 0.0, 0.0, 0.0, 0.0, 7.5];
    let err = gmat.convert(31041.5, &state, "RefuseTestReal", "NoSuchCoordinateSystemAtAll").unwrap_err();
    assert!(err.message.contains("not registered"), "expected a \"not registered\" message, got {err:?}");
}

/// A real GMAT object that is NOT a `CoordinateSystem` (e.g. a `Spacecraft`) named as `from_cs`/
/// `to_cs` is also a typed refusal, never a crash from an invalid `dynamic_cast`.
#[test]
fn convert_refuses_a_registered_object_that_is_not_a_coordinate_system() {
    let _engine = gmat_sys::engine_lock();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    gmat.coordinate_system("NotCsTestReal", "Earth", "MJ2000Eq").unwrap();
    gmat.construct("Spacecraft", "NotCsTestSat").unwrap();
    gmat.initialize().unwrap();

    let state = [7000.0, 0.0, 0.0, 0.0, 0.0, 7.5];
    let err = gmat.convert(31041.5, &state, "NotCsTestReal", "NotCsTestSat").unwrap_err();
    assert!(err.message.contains("not a CoordinateSystem"), "expected a \"not a CoordinateSystem\" message, got {err:?}");
}

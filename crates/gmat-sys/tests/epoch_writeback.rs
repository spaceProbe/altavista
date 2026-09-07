//! Question 105 (M12.2): the epoch is now written back to the spacecraft on every step
//! (`gmat_sys::DerivativeModel::sync_spacecraft_epoch_a1mjd`, called from
//! `gmat_sys::model::GmatModel::step`/`step_with_stm` alongside the existing Cartesian
//! write-back, `sync_spacecraft_cartesian_km`). `"RMAG"`/`"Cd"` -- the two outputs
//! `GmatModel::step` already read back (question 99) -- do not depend on epoch at all:
//! `RMAG = sqrt(x^2+y^2+z^2)` and `Cd` are both pure functions of the Cartesian state (or of
//! neither), so neither one could ever have caught the epoch never being written back. This
//! test instead reads a parameter that genuinely does move with epoch: Earth's own rotation,
//! via the spacecraft's Earth-fixed longitude.
//!
//! **Why a second "mirror" spacecraft is needed to reach a genuinely epoch-dependent
//! parameter.** `Spacecraft::GetStateInRepresentation`
//! (`third_party/gmat-src/src/base/spacecraft/Spacecraft.cpp`) only rotates the internal
//! Cartesian state into a body-fixed frame using the spacecraft's own epoch when its display
//! `CoordinateSystem` field differs from GMAT's internal coordinate system. Every spacecraft
//! this crate binds into a `DerivativeModel` has `CoordinateSystem = EarthMJ2000Eq`, matching
//! internal, so reading `"PlanetodeticLON"`/`"PlanetodeticLAT"` (fields `Spacecraft` understands
//! natively -- `Spacecraft::MULT_REP_STRINGS` -- no separate `Parameter` object needed) off the
//! *production* spacecraft would skip that rotation entirely and read the same value at any
//! epoch: precisely the trap `docs/open-questions.md` question 105 warns a test must avoid.
//! (Confirmed empirically before writing this test: the same Cartesian state at two epochs one
//! day apart read `PlanetodeticLON` bit-for-bit identical when `CoordinateSystem =
//! EarthMJ2000Eq` -- it proves nothing.)
//!
//! A second, otherwise-inert "mirror" spacecraft with `CoordinateSystem = EarthFixed` (built
//! from a `CoordinateSystem` object whose `Axes` reference is a constructed `"BodyFixed"` axis
//! system -- the same generic `Construct`/`SetField`/`SetReference` shim calls every other
//! object in this crate already uses, no new shim surface) fixes this: written with the raw
//! internal Cartesian state via `"CartesianX".."CartesianVZ"` (the "hidden" fields
//! `PropagationStateManager` itself uses, which write `state[i] = value` directly with **no**
//! coordinate-system-aware round trip -- unlike `"X".."VZ"`, which would misinterpret the given
//! numbers as already being in the display frame) and the propagated epoch (`"Epoch"`), reading
//! `"PlanetodeticLON"`/`"PlanetodeticLAT"` back genuinely rotates with the spacecraft's own
//! epoch. `goldens/gen_leo_1day_planetodetic_lon.py`'s own doc comment has the full reasoning
//! plus a numerical cross-check (agreement ~4e-9 degrees) between this mirror-spacecraft read
//! and GMAT's own script-engine `Earth.Longitude`/`Earth.Latitude` Parameter for a fixed
//! state+epoch.
//!
//! **The epoch must be written before the Cartesian state on this mirror spacecraft** -- unlike
//! the production spacecraft, where the two calls may go in either order; see
//! `DerivativeModel::sync_spacecraft_epoch_a1mjd`'s doc comment for why. `Spacecraft::SetEpoch`
//! calls `RecomputeStateAtEpoch`, which for a spacecraft whose display `CoordinateSystem`
//! differs from internal *rewrites* the internal state to hold the display-frame value constant
//! across the epoch change -- confirmed empirically: setting epoch alone (state already
//! written) left `PlanetodeticLON` completely unchanged (to 16 significant digits) across a full
//! day, and only re-writing the Cartesian state *afterward* produced the correct, epoch-moved
//! answer. Writing epoch first, then the raw `"CartesianX".."CartesianVZ"` fields (which have no
//! such side effect), avoids this for good.
use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::time::Tai;
use gmat_sys::integrate::Dopri5;
use gmat_sys::Gmat;
use serde::Deserialize;

#[derive(Deserialize)]
struct Gravity {
    file: String,
    degree: i32,
    order: i32,
}

#[derive(Deserialize)]
struct ForceModel {
    central_body: String,
    gravity: Gravity,
    point_masses: Vec<String>,
}

#[derive(Deserialize)]
struct Golden {
    epoch_utc: String,
    spacecraft: BTreeMap<String, f64>,
    force_model: ForceModel,
    duration_s: f64,
}

#[derive(Deserialize)]
struct LonGolden {
    epoch_a1mjd: f64,
    longitude_deg: f64,
    latitude_deg: f64,
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens").join(format!("{name}.json"))
}

#[test]
fn epoch_is_written_back_and_an_earth_fixed_longitude_matches_a_genuine_gmat_reportfile() {
    let _engine = gmat_sys::engine_lock();
    let golden: Golden = serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon")).unwrap()).unwrap();
    let lon_golden: LonGolden =
        serde_json::from_str(&std::fs::read_to_string(golden_path("leo_1day_jgm2_8x8_sunmoon_planetodetic_lon")).unwrap()).unwrap();

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // --- Production spacecraft: exactly the leo_1day golden's own configuration
    // (CoordinateSystem = EarthMJ2000Eq, matching every other test/binding in this crate). ---
    let sat = gmat.construct("Spacecraft", "EpochBackSat").unwrap();
    sat.set_str("DateFormat", "UTCGregorian").unwrap();
    sat.set_str("Epoch", &golden.epoch_utc).unwrap();
    sat.set_str("CoordinateSystem", "EarthMJ2000Eq").unwrap();
    sat.set_str("DisplayStateType", "Keplerian").unwrap();
    for (k, v) in &golden.spacecraft {
        sat.set_real(k, *v).unwrap();
    }
    let fm = gmat.construct("ForceModel", "EpochBackFM").unwrap();
    fm.set_str("CentralBody", &golden.force_model.central_body).unwrap();
    let grav = gmat.construct("GravityField", "").unwrap();
    grav.set_str("BodyName", &golden.force_model.central_body).unwrap();
    grav.set_str("PotentialFile", &golden.force_model.gravity.file).unwrap();
    grav.set_int("Degree", golden.force_model.gravity.degree).unwrap();
    grav.set_int("Order", golden.force_model.gravity.order).unwrap();
    fm.add_force(&grav).unwrap();
    for body in &golden.force_model.point_masses {
        let pm = gmat.construct("PointMassForce", "").unwrap();
        pm.set_str("BodyName", body).unwrap();
        fm.add_force(&pm).unwrap();
    }

    // --- "Mirror" spacecraft: CoordinateSystem = EarthFixed, needed to reach a genuinely
    // epoch-dependent parameter at all (see the module doc comment). ---
    let axes = gmat.construct("BodyFixed", "EpochBackAxes").unwrap();
    let earth_fixed = gmat.construct("CoordinateSystem", "EpochBackEarthFixed").unwrap();
    earth_fixed.set_str("Origin", "Earth").unwrap();
    earth_fixed.set_reference(&axes).unwrap();
    let mirror = gmat.construct("Spacecraft", "EpochBackMirror").unwrap();
    mirror.set_str("DateFormat", "A1ModJulian").unwrap();
    mirror.set_str("CoordinateSystem", "EpochBackEarthFixed").unwrap();

    gmat.initialize().unwrap();

    let model = gmat.derivative_model(&fm, &sat).expect("derivative model");
    let x0 = model.state().unwrap();

    // --- Propagate the whole golden arc with this crate's own integrator over GetDerivatives,
    // exactly as GmatModel::step does internally (DerivativeModel/Dopri5 are km/km-s native --
    // no SI conversion at this layer, see model.rs's own doc comment on that boundary). ---
    let (x1, _stats) = Dopri5::default()
        .integrate(|t, x, out| model.derivatives_into(x, t, out), &x0, 0.0, golden.duration_s)
        .expect("integration");
    let x1_km: [f64; 6] = x1[0..6].try_into().unwrap();

    // --- The production write-back (question 105): epoch first, then Cartesian state, exactly
    // like GmatModel::step/step_with_stm. The new epoch is the model's own construction epoch
    // (converted through the same av_cdm::time::Tai path GmatModel::epoch_tai_ns/step use) plus
    // the arc duration in exact integer TAI nanoseconds. ---
    let t0_a1mjd = model.epoch_a1mjd();
    let t0_tai_ns = Tai::from_a1_mjd(t0_a1mjd).as_nanos();
    let t1_tai_ns = t0_tai_ns + (golden.duration_s * 1e9).round() as i64;
    let t1_a1mjd = Tai::from_nanos(t1_tai_ns).to_a1_mjd();

    model.sync_spacecraft_epoch_a1mjd(t1_a1mjd).expect("sync epoch");
    model.sync_spacecraft_cartesian_km(&x1_km).expect("sync state");

    // Question 105's own honesty requirement: RMAG must still equal the position magnitude of
    // the state just written -- the epoch write-back must not perturb it (RMAG does not depend
    // on epoch at all; see the module doc comment). The full 4 um-tolerance comparison against a
    // genuine GMAT ReportFile lives in
    // av-kernel's drm_rmag_output_matches_a_genuine_gmat_reportfile, unaffected by this change
    // (re-run as part of this task, still passing at its existing tolerance).
    let rmag_km = model.real_parameter("RMAG").unwrap();
    let rmag_from_xyz_km = (x1_km[0].powi(2) + x1_km[1].powi(2) + x1_km[2].powi(2)).sqrt();
    assert!(
        (rmag_km - rmag_from_xyz_km).abs() < 1e-9,
        "RMAG must still equal the position magnitude after the epoch write-back: {rmag_km} vs {rmag_from_xyz_km}"
    );

    // --- The mirror: same final epoch, same final state, but read through a spacecraft whose
    // CoordinateSystem actually differs from GMAT's internal one, so PlanetodeticLON/LAT
    // genuinely rotate with epoch. Epoch first (see module doc comment). ---
    let write_mirror_state = |epoch_a1mjd: f64| {
        mirror.set_str("Epoch", &epoch_a1mjd.to_string()).unwrap();
        for (field, value) in [
            ("CartesianX", x1_km[0]),
            ("CartesianY", x1_km[1]),
            ("CartesianZ", x1_km[2]),
            ("CartesianVX", x1_km[3]),
            ("CartesianVY", x1_km[4]),
            ("CartesianVZ", x1_km[5]),
        ] {
            mirror.set_real(field, value).unwrap();
        }
    };

    write_mirror_state(t1_a1mjd);
    let lon_deg = mirror.real_parameter("PlanetodeticLON").unwrap();
    let lat_deg = mirror.real_parameter("PlanetodeticLAT").unwrap();

    let epoch_diff_days = (t1_a1mjd - lon_golden.epoch_a1mjd).abs();
    let lon_diff_deg = (lon_deg - lon_golden.longitude_deg).abs();
    let lat_diff_deg = (lat_deg - lon_golden.latitude_deg).abs();
    eprintln!(
        "[gmat-sys epoch_writeback] epoch_a1mjd: ours = {t1_a1mjd:.9}, golden = {:.9} (diff {epoch_diff_days:.3e} days); \
         longitude_deg: ours = {lon_deg:.9}, golden = {:.9} (diff {lon_diff_deg:.3e} deg); \
         latitude_deg: ours = {lat_deg:.9}, golden = {:.9} (diff {lat_diff_deg:.3e} deg)",
        lon_golden.epoch_a1mjd, lon_golden.longitude_deg, lon_golden.latitude_deg,
    );

    // 1e-5 deg is generous relative to the measured agreement (see the eprintln! above): the
    // A1MJD f64 write costs at most 252 ns of epoch precision (question 81), which at Earth's
    // ~4.178e-12 deg/ns rotation rate moves this longitude by roughly 1e-9 deg -- three orders
    // of magnitude below this tolerance. The dominant term is the ~5 cm position-tolerance gap
    // between this crate's own Dopri5/GetDerivatives arc and GMAT's own PrinceDormand78 script
    // propagation (the same gap leo_golden.rs's own 0.05 m tolerance already accepts), translated
    // to an angle: 0.05 m / 6.878e6 m radius ~ 4e-7 deg.
    assert!(lon_diff_deg < 1e-5, "longitude {lon_deg} deg vs golden {} deg (diff {lon_diff_deg} deg)", lon_golden.longitude_deg);
    assert!(lat_diff_deg < 1e-5, "latitude {lat_deg} deg vs golden {} deg (diff {lat_diff_deg} deg)", lon_golden.latitude_deg);

    // --- Prove the parameter is genuinely epoch-dependent (the trap this test exists to avoid):
    // reading it with the *stale* construction epoch and the *same correct* final position must
    // give a materially different answer -- close to a full day's Earth rotation, not noise --
    // unlike RMAG/Cd, which M11.1 could never have used to catch a missing epoch write-back. ---
    write_mirror_state(t0_a1mjd);
    let lon_stale_deg = mirror.real_parameter("PlanetodeticLON").unwrap();
    let raw_diff = (lon_deg - lon_stale_deg).abs();
    let stale_diff_deg = raw_diff.min(360.0 - raw_diff); // longitude wraps at +/-180
    eprintln!(
        "[gmat-sys epoch_writeback] longitude with the STALE (construction) epoch = {lon_stale_deg:.6} deg; \
         diff from the correct (propagated-epoch) answer = {stale_diff_deg:.6} deg"
    );
    // The arc is exactly one mean solar day (86400 s), and a mean solar day is ~0.9856 deg short
    // of a full sidereal turn (360 * (86400/86164.0905 s - 1) ~ 0.9856), so a fixed inertial
    // point's Earth-fixed longitude one day later differs by ~0.9856 deg -- not by noise, and not
    // by a near-full turn either. 0.5 deg comfortably separates that from the ~1e-9 deg noise
    // floor a correct epoch achieves (measured above) without assuming more precision than
    // "close to a sidereal-minus-solar-day residual" warrants.
    assert!(
        stale_diff_deg > 0.5,
        "a stale epoch (exactly one day off) must move this parameter by close to Earth's ~0.9856 deg/day \
         solar-vs-sidereal residual, not by noise; got {stale_diff_deg} deg -- if this ever failed it would mean \
         PlanetodeticLON stopped being epoch-dependent for this configuration, invalidating the whole test"
    );
}

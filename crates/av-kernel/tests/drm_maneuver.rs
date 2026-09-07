//! The DRM executor's impulsive-maneuver acceptance tests (M10.1, `docs/open-questions.md`
//! question 97):
//!
//! 1. `drms/leo_1day_maneuver_vnb.drm.yaml` (+ `.sos.yaml`, reusing the golden bundle's own
//!    `leo_sys` `SystemDefinition`) runs through [`av_kernel::drm::execute`] and matches
//!    `goldens/leo_1day_maneuver_vnb.json` -- the same LEO orbit as the M6.1 golden, with one
//!    20 m/s prograde VNB burn applied through altavista's own `Scenario.maneuver` reference path
//!    (`goldens/gen_leo_1day_maneuver_vnb.py`) -- within the same tolerance class as
//!    `leo_1day_jgm2_8x8_sunmoon.json` (0.05 m / 5e-5 m/s).
//! 2. A native (GMAT-free) `"accel.x"` binding, entirely closed-form, proves the boundary
//!    mechanics exactly: position continuity, the velocity jump landing at the right component,
//!    exactly one sample kept at the burn epoch (the post-burn one, not both), two dynamics
//!    segments, and one `EVENT_KIND_MANEUVER` event between the lifecycle pair.
//! 3. Typed load-time refusals: an off-grid maneuver epoch, an unknown instance, and an
//!    unsupported frame -- all refused before any GMAT call.
//! 4. Covariance across a burn (question 97's item 5, "no execution error" -> Phi/P unchanged):
//!    a covariance-enabled run split by a **zero-dv** maneuver event must match, to a tight
//!    numerical tolerance, a run over the identical span with no maneuver event at all -- the
//!    only way the segment-restart/carry-through machinery this task adds could disagree with
//!    "no burn happened" is a bug in exactly the mechanism this task built (see
//!    `av_kernel::drm::executor::run_covariance_instance`'s own "Covariance across a burn" doc
//!    comment section for the argument this test exercises end to end, through the public
//!    `execute()` API only).
//! 5. RIC and VVLH goldens (M11.3, `docs/open-questions.md` question 102; VVLH is the M12.4,
//!    question 106 rename of what this suite called LVLH through M11.3 -- see item 6 below for
//!    why the old name still appears, deliberately, in one fixture):
//!    - `drms/leo_1day_maneuver_ric.drm.yaml` (`AXES_KIND_RIC`) matches
//!      `goldens/leo_1day_maneuver_ric.json` (altavista's `Scenario.maneuver(frame="RIC")`, a real
//!      GMAT `ImpulsiveBurn` whose `CoordinateSystem` is an ObjectReferenced RIC system) at the
//!      same tolerance class as the VNB golden -- this platform's ratified `AxesKind::Ric`
//!      (X=R, Z=N, question 73) is exactly what that ObjectReferenced system realizes.
//!    - `drms/leo_1day_maneuver_vvlh.drm.yaml` (`AXES_KIND_VVLH`) does **not** match
//!      `goldens/leo_1day_maneuver_gmat_lvlh.json` (altavista's `Scenario.maneuver(frame="LVLH")`,
//!      a real GMAT `ImpulsiveBurn` with `CoordinateSystem=Local, Axes=LVLH` -- GMAT's own,
//!      literal local burn axes; the golden file itself was renamed from
//!      `leo_1day_maneuver_lvlh.json` by M12.4 so its name could never be mistaken for this
//!      platform's own convention). Measured and asserted here, not silently avoided: GMAT's
//!      `Axes=LVLH` is empirically X=R, Y=N×R (in-track), Z=N -- the same triad as
//!      `AxesKind::Ric` -- while this crate's ratified `AxesKind::Vvlh` (question 73) is the
//!      different Z=-R,Y=-N,X=N×R (VVLH) convention, so a `ScenarioEvent` honestly declared
//!      `AXES_KIND_VVLH` and run through this executor reproduces neither the burn GMAT's own
//!      "LVLH" applies nor this golden. A second run of the identical `dv`, retagged
//!      `AXES_KIND_RIC`, *does* reproduce the golden -- direct proof that GMAT's `Axes=LVLH` and
//!      `AxesKind::Ric` are the same convention: the record `drm_maneuver_axes_kind_ric_
//!      reproduces_gmats_native_lvlh_burn` pins. See `altavista/FRAMES.md`'s "GMAT ImpulsiveBurn
//!      LVLH vs `AXES_KIND_VVLH`" section for the full write-up; nothing here reorients the
//!      golden or `dv_to_inertial`'s ratified `AxesKind::Vvlh` arm to force a match.
//! 6. The retired name is a typed load error, not silently accepted or reinterpreted (M12.4,
//!    question 106): `core.proto`'s `AxesKind` enum now carries `reserved "AXES_KIND_LVLH";`,
//!    so `AxesKind::from_str_name("AXES_KIND_LVLH")` returns `None` and `maneuver::parse` (called
//!    at DRM load time, see `crate::drm::schema`) refuses it with the existing
//!    `DrmError::InvalidEnumValue` -- no new error variant needed. `drms/
//!    leo_1day_maneuver_lvlh.{drm,sos}.yaml` are kept, deliberately unchanged (still literally
//!    declaring `frame_id: AXES_KIND_LVLH`, the fixture this suite used through M11.3 for the
//!    mismatch this item 5 now measures via the `_vvlh` fixtures instead), solely so this typed
//!    refusal has something real to load and fail on.
//!
//! Every GMAT-touching test here takes `gmat_sys::engine_lock()` first, per this repository's
//! existing convention, and gives every instance it constructs a name unused by any other test
//! in this crate (GMAT's configuration manager is process-global -- see
//! `crates/av-kernel/src/drm/executor.rs`'s module doc comment's "GMAT object naming" section).
//! Item 6's test loads no GMAT object at all (the refusal happens at YAML/schema parse time,
//! before any instance is constructed), so it takes no engine lock.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{
    Binding, BindingKind, DesignReferenceMission, DrmOptions, EventKind, ModelBinding, Parameter, Scenario, ScenarioEvent, SosConfiguration, SystemDefinition, SystemInstance,
};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use gmat_sys::Gmat;
use prost::Message;
use serde::Deserialize;

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

fn maneuver_event(id: &str, tai_ns: i64, instance: &str, dv: [f64; 3], frame: &str) -> ScenarioEvent {
    ScenarioEvent {
        id: id.to_string(),
        tai_ns,
        kind: "maneuver".to_string(),
        instance: instance.to_string(),
        values: BTreeMap::from([("dv_x".to_string(), dv[0]), ("dv_y".to_string(), dv[1]), ("dv_z".to_string(), dv[2])]),
        attributes: BTreeMap::from([("frame_id".to_string(), frame.to_string())]),
        execution_error: None,
    }
}

// ----------------------------------------------------------------------------------------
// 1. Golden: the DRM matches altavista's own VNB burn path.
// ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ManeuverGolden {
    state_post_burn: Vec<f64>,
    final_state: Vec<f64>,
    tolerance_m: f64,
    tolerance_mps: f64,
}

fn maneuver_golden() -> ManeuverGolden {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_vnb.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn dr_dv(mean: &[f64], golden_si: &[f64; 6]) -> (f64, f64) {
    let dr = (0..3).map(|i| (mean[i] - golden_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (mean[i] - golden_si[i]).powi(2)).sum::<f64>().sqrt();
    (dr, dv)
}

#[test]
fn drm_matches_the_maneuver_golden_vnb_burn() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_vnb.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_vnb.sos.yaml")).unwrap()).expect("SosConfiguration parses");

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-maneuver-golden".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("maneuver DRM executes end to end");

    let traj = products.trajectories.get("leo_mvr").expect("the \"leo_mvr\" instance produced a trajectory");
    assert_eq!(traj.segments.len(), 2, "one maneuver -> two dynamics segments");
    // 121 samples: 0, 60, .., 7200 s at 60 s spacing, with exactly one (the post-burn) sample
    // kept at the burn epoch -- see run_span's own doc comment for why this is not 122.
    assert_eq!(traj.samples.len(), 121, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());

    let g = maneuver_golden();
    // The *actual* applied epoch, from the real Event this run produced -- this instance's own
    // `t0_actual` (`ModelHandle::t0_tai_ns`, `crate::registry`) is exactly the declared
    // `Scenario.start_tai_ns` as of M10.3 (question 96 deletes the A1MJD round-trip
    // reconciliation that could once put it a few hundred ns off), so `mev.tai_ns` and the bare
    // declared `Scenario.events[0].tai_ns` now agree exactly too -- looked up by the real Event
    // regardless, so this assertion does not depend on that equality holding.
    // `executor::run_plain_instance` applies the burn, and therefore samples the post-burn
    // state, at this same epoch.
    let mev = products.events.iter().find(|e| e.name == "burn1").expect("the maneuver event");
    let at_burn = traj.samples.iter().find(|s| s.tai_ns == mev.tai_ns).expect("a sample exactly at the maneuver's own applied epoch");
    let golden_post_burn_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.state_post_burn.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&at_burn.mean, &golden_post_burn_si);
    eprintln!("[drm_maneuver] post-burn |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "post-burn position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "post-burn velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);

    let last = traj.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&last.mean, &golden_final_si);
    eprintln!("[drm_maneuver] final    |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "final position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "final velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);

    // Events (question 95/97): run_start, the maneuver, run_end -- sorted (epoch, id).
    assert_eq!(products.events.len(), 3, "{:?}", products.events);
    assert_eq!(products.events[0].name, "run_start");
    assert_eq!(products.events[1].name, "burn1");
    assert_eq!(mev.kind, EventKind::Maneuver as i32);
    assert_eq!(mev.frame_id, "AXES_KIND_VNB");
    assert!((mev.values.get("dv_x").copied().unwrap_or_default() - 20.0).abs() < 1e-9);
    assert!((mev.values.get("dv_mps").copied().unwrap_or_default() - 20.0).abs() < 1e-9);
    assert_eq!(products.events[2].name, "run_end");

    // RunProducts.frames (question 121/122, M17.2; question 10/124, M18.1), on **the golden
    // maneuver DRM** this task's own end-to-end wire test (tests/test_cdm_run.py) actually
    // exercises: the burn's own frame (mev.frame_id == "AXES_KIND_VNB", asserted above) is a
    // different thing from the trajectory's frame -- "leo_mvr" still declares
    // spacecraft.CoordinateSystem = "EarthMJ2000Eq" (the shared leo_1day_golden.system.yaml), so
    // RunProducts.frames must carry that registry-default FrameDefinition even though a
    // maneuver split this run into two dynamics segments. As of M18.1, question 10's mandatory
    // ICRF/MJ2000Eq/BodyFixed frames for the run's own central body (Earth) are always added on
    // top, regardless of which one was actually propagated in -- so EarthICRF/EarthBodyFixed
    // join EarthMJ2000Eq here too. Fails against an executor that only populates frames for an
    // unfaulted/unmaneuvered run, one that confuses the burn's own AXES_KIND_VNB frame_id with
    // the trajectory's own EarthMJ2000Eq one, or one that regressed M18.1's mandatory-frame
    // augmentation.
    assert_eq!(traj.frame_id, "EarthMJ2000Eq");
    assert_eq!(products.frames.len(), 3, "{:?}", products.frames);
    let by_id: std::collections::BTreeMap<&str, &av_cdm::pb::FrameDefinition> = products.frames.iter().map(|f| (f.id.as_str(), f)).collect();
    assert_eq!(by_id.keys().copied().collect::<Vec<_>>(), vec!["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"]);
    assert_eq!(by_id["EarthMJ2000Eq"].origin, Some(av_cdm::pb::frame_definition::Origin::Body("Earth".to_string())));
    assert_eq!(by_id["EarthMJ2000Eq"].axes, av_cdm::pb::AxesKind::Mj2000Eq as i32);
    assert_eq!(by_id["EarthICRF"].axes, av_cdm::pb::AxesKind::Icrf as i32);
    assert_eq!(by_id["EarthBodyFixed"].axes, av_cdm::pb::AxesKind::BodyFixed as i32);

    // RunProducts -> altavista.v1.RunProducts (question 121, M17.2): round-trips through real
    // protobuf bytes for the maneuver golden too -- including the maneuver EVENT_KIND_MANEUVER
    // event itself, which the deleted AVRUN1 framing (crates/av-run's old encode_run_bundle)
    // also had to carry, so this is a like-for-like proof the new wire form loses nothing the
    // old one carried.
    let proto = products.to_proto();
    let decoded = av_cdm::pb::RunProducts::decode(proto.encode_to_vec().as_slice()).expect("valid altavista.v1.RunProducts bytes");
    assert_eq!(decoded.events.len(), 3);
    assert!(decoded.events.iter().any(|e| e.name == "burn1" && e.kind == EventKind::Maneuver as i32));
    assert_eq!(decoded.frames, proto.frames);
}

// ----------------------------------------------------------------------------------------
// 1b. RIC and VVLH goldens (M11.3, question 102; VVLH renamed from LVLH by M12.4, question 106).
// ----------------------------------------------------------------------------------------

/// The RIC counterpart of [`drm_matches_the_maneuver_golden_vnb_burn`]: `AxesKind::Ric`
/// (X=R, Z=N, question 73) is exactly what altavista's ObjectReferenced RIC `CoordinateSystem`
/// realizes, so this is expected to -- and does -- match at the same tight tolerance.
#[test]
fn drm_matches_the_maneuver_golden_ric_burn() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_ric.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_ric.sos.yaml")).unwrap()).expect("SosConfiguration parses");

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-maneuver-golden-ric".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("RIC maneuver DRM executes end to end");

    let traj = products.trajectories.get("leo_mvr_ric").expect("the \"leo_mvr_ric\" instance produced a trajectory");
    assert_eq!(traj.segments.len(), 2, "one maneuver -> two dynamics segments");
    assert_eq!(traj.samples.len(), 121, "{:?}", traj.samples.iter().map(|s| s.tai_ns).collect::<Vec<_>>());

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_ric.json");
    let g: ManeuverGolden = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let mev = products.events.iter().find(|e| e.name == "burn1").expect("the maneuver event");
    let at_burn = traj.samples.iter().find(|s| s.tai_ns == mev.tai_ns).expect("a sample exactly at the maneuver's own applied epoch");
    let golden_post_burn_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.state_post_burn.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&at_burn.mean, &golden_post_burn_si);
    eprintln!("[drm_maneuver RIC] post-burn |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "post-burn position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "post-burn velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);

    let last = traj.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&last.mean, &golden_final_si);
    eprintln!("[drm_maneuver RIC] final    |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "final position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "final velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);

    assert_eq!(mev.frame_id, "AXES_KIND_RIC");
    assert!((mev.values.get("dv_y").copied().unwrap_or_default() - 20.0).abs() < 1e-9);
}

/// GMAT's own `ImpulsiveBurn Axes=LVLH` (`goldens/leo_1day_maneuver_gmat_lvlh.json`) versus this
/// crate's ratified `AxesKind::Vvlh` (question 73; renamed from `AxesKind::Lvlh` by M12.4,
/// question 106): a `ScenarioEvent` honestly declared `AXES_KIND_VVLH`
/// (`drms/leo_1day_maneuver_vvlh.drm.yaml`, the same `dv` the golden used) run through the
/// executor's ratified `dv_to_inertial` does **not** reproduce the golden -- this asserts and
/// quantifies the real, measured mismatch (the 28.284 m/s question 106 records) rather than
/// silently skipping the comparison. This is the "VVLH arm" of the finding; the sibling test
/// below is the "the record that GMAT's Axes=LVLH equals AXES_KIND_RIC" arm. See the module doc
/// comment's item 5 and `altavista/FRAMES.md`.
#[test]
fn drm_maneuver_axes_kind_vvlh_does_not_reproduce_gmats_native_lvlh_burn() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_vvlh.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_vvlh.sos.yaml")).unwrap()).expect("SosConfiguration parses");
    assert_eq!(drm.scenario.as_ref().unwrap().events[0].attributes.get("frame_id").map(String::as_str), Some("AXES_KIND_VVLH"));

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-maneuver-golden-vvlh-as-vvlh".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("VVLH-tagged maneuver DRM executes end to end (the mismatch is numerical, not a load/run error)");

    let traj = products.trajectories.get("leo_mvr_vvlh").expect("the \"leo_mvr_vvlh\" instance produced a trajectory");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_gmat_lvlh.json");
    let g: ManeuverGolden = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let mev = products.events.iter().find(|e| e.name == "burn1").expect("the maneuver event");
    let at_burn = traj.samples.iter().find(|s| s.tai_ns == mev.tai_ns).expect("a sample exactly at the maneuver's own applied epoch");
    let golden_post_burn_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.state_post_burn.as_slice()).unwrap());
    let (dr_burn, dv_burn) = dr_dv(&at_burn.mean, &golden_post_burn_si);
    eprintln!(
        "[drm_maneuver VVLH-as-AXES_KIND_VVLH] post-burn |dr| = {dr_burn:.6} m, |dv| = {dv_burn:.6} m/s -- \
         measured mismatch against GMAT's own Axes=LVLH golden (golden tolerance is {} m / {} m/s)",
        g.tolerance_m, g.tolerance_mps
    );
    // Position is continuous across an impulsive burn (both runs share the identical pre-burn
    // trajectory), so |dr| at the burn epoch itself stays ~0 regardless of which frame
    // convention was used -- the disagreement is a *velocity* jump, so this checks |dv| there.
    // The ratified AxesKind::Vvlh arm sends dv_y=20 m/s along -N (orbit-anti-normal); GMAT's own
    // Axes=LVLH golden applied it along +in-track -- orthogonal directions, so the immediate |dv|
    // mismatch is the full sqrt(20^2+20^2) ~ 28.28 m/s (measured: see the eprintln above -- this
    // is the same 28.284 m/s question 106 records as the reason for the rename).
    // Asserted with wide margin (not tuned to the exact measured value) so this documents "these
    // disagree by a lot", not a brittle pin of the disagreement's exact size.
    assert!(dv_burn > 1.0, "expected AXES_KIND_VVLH to badly mismatch GMAT's native Axes=LVLH burn at the burn epoch, but |dv| = {dv_burn} m/s");

    // An hour of coasting on the wrong post-burn velocity then also diverges position by a lot.
    let last = traj.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let (dr_final, dv_final) = dr_dv(&last.mean, &golden_final_si);
    eprintln!("[drm_maneuver VVLH-as-AXES_KIND_VVLH] final    |dr| = {dr_final:.3} m, |dv| = {dv_final:.6} m/s -- measured mismatch, one hour after the burn");
    assert!(dr_final > 1000.0, "expected AXES_KIND_VVLH to badly mismatch GMAT's native Axes=LVLH burn after propagation, but |dr| = {dr_final} m");
    assert!(dv_final > 1.0, "expected AXES_KIND_VVLH to badly mismatch GMAT's native Axes=LVLH burn after propagation, but |dv| = {dv_final} m/s");
    assert_eq!(mev.frame_id, "AXES_KIND_VVLH");
}

/// The record that GMAT's `Axes=LVLH` equals `AXES_KIND_RIC`: retagging the *identical* `dv`
/// from `leo_1day_maneuver_vvlh.drm.yaml` as `AXES_KIND_RIC` instead of `AXES_KIND_VVLH` **does**
/// reproduce `goldens/leo_1day_maneuver_gmat_lvlh.json` at the tight golden tolerance -- direct
/// proof that GMAT's `ImpulsiveBurn Axes=LVLH` and this crate's ratified `AxesKind::Ric` are the
/// same convention (X=R, Z=N), confirming `altavista/scenario.py::Scenario._fire_impulsive_burn`'s
/// own empirical measurement independently, through the Rust executor rather than altavista. This
/// is the finding question 106 (M12.4) acted on: since `AXES_KIND_RIC`, not the old
/// `AXES_KIND_LVLH` name, is what actually matches GMAT's `Axes=LVLH`, the platform's own
/// convention was renamed to `AXES_KIND_VVLH` so the two could never again be confused by name.
#[test]
fn drm_maneuver_axes_kind_ric_reproduces_gmats_native_lvlh_burn() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let sos = hashed_sos(SosConfiguration {
        id: "leo_sos_maneuver_vvlh_as_ric".to_string(),
        instances: vec![SystemInstance {
            name: "leo_mvr_vvlh_as_ric".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    // Same start/end/burn epochs and dv as leo_1day_maneuver_vvlh.drm.yaml -- only frame_id and
    // the instance/sos ids (GMAT object naming, see the module doc comment) differ.
    let start = 1_767_225_637_000_000_000i64;
    let drm = hashed_drm(DesignReferenceMission {
        id: "drm_leo_1day_maneuver_vvlh_as_ric".to_string(),
        sos_configuration_id: "leo_sos_maneuver_vvlh_as_ric".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: start,
            end_tai_ns: start + 7_200_000_000_000,
            events: vec![maneuver_event("burn1", start + 3_600_000_000_000, "leo_mvr_vvlh_as_ric", [0.0, 20.0, 0.0], "AXES_KIND_RIC")],
            ..Default::default()
        }),
        options: Some(DrmOptions { covariance: false, default_step_rate_hz: 10.0, sample_interval_s: 60.0, ..Default::default() }),
        ..Default::default()
    });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-maneuver-golden-vvlh-as-ric".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("RIC-retagged maneuver DRM executes end to end");

    let traj = products.trajectories.get("leo_mvr_vvlh_as_ric").expect("the instance produced a trajectory");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_maneuver_gmat_lvlh.json");
    let g: ManeuverGolden = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let mev = products.events.iter().find(|e| e.name == "burn1").expect("the maneuver event");
    let at_burn = traj.samples.iter().find(|s| s.tai_ns == mev.tai_ns).expect("a sample exactly at the maneuver's own applied epoch");
    let golden_post_burn_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.state_post_burn.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&at_burn.mean, &golden_post_burn_si);
    eprintln!("[drm_maneuver VVLH-as-AXES_KIND_RIC] post-burn |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "post-burn position error {dr} m exceeds golden tolerance {} m -- AXES_KIND_RIC should reproduce GMAT's Axes=LVLH burn", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "post-burn velocity error {dv} m/s exceeds golden tolerance {} m/s -- AXES_KIND_RIC should reproduce GMAT's Axes=LVLH burn", g.tolerance_mps);

    let last = traj.samples.last().expect("at least one sample");
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).unwrap());
    let (dr, dv) = dr_dv(&last.mean, &golden_final_si);
    eprintln!("[drm_maneuver VVLH-as-AXES_KIND_RIC] final    |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "final position error {dr} m exceeds golden tolerance {} m -- AXES_KIND_RIC should reproduce GMAT's Axes=LVLH burn", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "final velocity error {dv} m/s exceeds golden tolerance {} m/s -- AXES_KIND_RIC should reproduce GMAT's Axes=LVLH burn", g.tolerance_mps);
}

/// M12.4 (question 106), item 7: a DRM that still names the retired `AXES_KIND_LVLH` value
/// must fail at load with the existing typed `DrmError::InvalidEnumValue` -- not silently
/// accepted, not reinterpreted as `AXES_KIND_VVLH`, and not a new error variant.
/// `drms/leo_1day_maneuver_lvlh.drm.yaml` is kept, deliberately unchanged, for exactly this: it
/// still literally declares `frame_id: AXES_KIND_LVLH` (the fixture this suite used, before
/// M12.4, for the mismatch test now above as `drm_maneuver_axes_kind_vvlh_does_not_reproduce_
/// gmats_native_lvlh_burn`). `core.proto`'s `AxesKind` enum carries `reserved
/// "AXES_KIND_LVLH";`, so `AxesKind::from_str_name("AXES_KIND_LVLH")` returns `None` and
/// `maneuver::parse` (invoked by `schema::RawScenario::into_pb`, reached from
/// `schema::parse_drm_yaml` before any GMAT object exists) refuses it -- this test therefore
/// takes no `gmat_sys::engine_lock()` and constructs no GMAT object at all.
#[test]
fn a_drm_naming_axes_kind_lvlh_is_a_typed_load_error() {
    let err = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_maneuver_lvlh.drm.yaml")).unwrap()).expect_err("a DRM naming the retired AXES_KIND_LVLH must be refused at load, not parsed");
    assert!(matches!(err, DrmError::InvalidEnumValue { ref value, .. } if value == "AXES_KIND_LVLH"), "{err:?}");
}

// ----------------------------------------------------------------------------------------
// 2. Native, GMAT-free: exact closed-form check of the boundary mechanics.
// ----------------------------------------------------------------------------------------

fn accel_system(id: &str) -> SystemDefinition {
    hashed_system(SystemDefinition {
        id: id.to_string(),
        dynamics_model: "native.constant_accel".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            param("accel.x", 1.0),
            param("accel.y", 0.0),
            param("accel.z", 0.0),
            sparam("frame_id", "test.frame"),
            param("state.px", 0.0),
            param("state.py", 0.0),
            param("state.pz", 0.0),
            param("state.vx", 0.0),
            param("state.vy", 0.0),
            param("state.vz", 0.0),
        ],
        ..Default::default()
    })
}

/// Not individually required, but exercises "What to build" items 1-3 together, entirely
/// GMAT-free (no GMAT call is ever reachable for a `"native."`-dispatched instance): a 1 s
/// midpoint burn adding 5 m/s in the (inertial) `y` axis to a vehicle accelerating at 1 m/s^2
/// in `x`, checked against the closed-form solution on each half independently -- `x(1s) =
/// 0.5*1*1^2 = 0.5`, `vx(1s) = 1.0` (unaffected by the burn, which only touches `y`); the burn
/// then jumps `vy` from 0 to 5.0 instantly; `x(2s) = x(1s) + vx(1s)*1 + 0.5*1*1^2 = 2.0`,
/// `vx(2s) = 2.0`; `y(2s) = y(1s) + vy(1s..2s)*1 = 0 + 5.0*1 = 5.0` (`accel.y = 0`, so `vy`
/// stays exactly 5.0 after the burn).
#[test]
fn a_maneuver_splits_the_run_and_the_kept_boundary_sample_is_the_post_burn_one() {
    let _engine = gmat_sys::engine_lock();
    let sys = accel_system("mvr_accel_sys");
    let sos = hashed_sos(SosConfiguration {
        id: "mvr_accel_sos".to_string(),
        instances: vec![SystemInstance {
            name: "veh".to_string(),
            system_id: "mvr_accel_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "mvr_accel_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "mvr_accel_drm".to_string(),
        sos_configuration_id: "mvr_accel_sos".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: 2_000_000_000,
            events: vec![maneuver_event("burn1", 1_000_000_000, "veh", [0.0, 5.0, 0.0], "AXES_KIND_ICRF")],
            ..Default::default()
        }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-accel-maneuver".to_string(), error_mode: Default::default() };
    let products = execute(cfg).expect("maneuver-split DRM executes end to end");

    let traj = products.trajectories.get("veh").expect("instance produced a trajectory");
    assert_eq!(traj.segments.len(), 2, "one maneuver -> two dynamics segments");
    // 21 samples: 0, 100ms, .., 2000ms at 10 Hz, with exactly the post-burn sample kept at the
    // burn epoch (1000ms) -- not the pre-burn one, and not both.
    assert_eq!(traj.samples.len(), 21);

    let midpoint = traj.samples.iter().find(|s| s.tai_ns == 1_000_000_000).expect("a sample at the maneuver epoch");
    assert!((midpoint.mean[0] - 0.5).abs() < 1e-9, "x(1s) = {}", midpoint.mean[0]);
    assert!((midpoint.mean[3] - 1.0).abs() < 1e-9, "vx(1s) unaffected by a y-axis burn = {}", midpoint.mean[3]);
    assert!(midpoint.mean[1].abs() < 1e-9, "y(1s) before the burn's position effect = {}", midpoint.mean[1]);
    assert!((midpoint.mean[4] - 5.0).abs() < 1e-9, "vy(1s) must be the POST-burn value (5.0), proving the kept sample is post-burn = {}", midpoint.mean[4]);

    let last = traj.samples.last().unwrap();
    assert_eq!(last.tai_ns, 2_000_000_000);
    assert!((last.mean[0] - 2.0).abs() < 1e-9, "x(2s) = {}", last.mean[0]);
    assert!((last.mean[3] - 2.0).abs() < 1e-9, "vx(2s) = {}", last.mean[3]);
    assert!((last.mean[1] - 5.0).abs() < 1e-9, "y(2s) = {}", last.mean[1]);
    assert!((last.mean[4] - 5.0).abs() < 1e-9, "vy(2s) = {}", last.mean[4]);
    for i in [2, 5] {
        assert!(last.mean[i].abs() < 1e-9, "component {i} should stay exactly 0: {}", last.mean[i]);
    }

    // Events (question 95/97): run_start, the maneuver, run_end -- sorted (epoch, id).
    assert_eq!(products.events.len(), 3, "{:?}", products.events);
    assert_eq!(products.events[0].name, "run_start");
    let mev = &products.events[1];
    assert_eq!(mev.name, "burn1");
    assert_eq!(mev.reference_id, "burn1");
    assert_eq!(mev.kind, EventKind::Maneuver as i32);
    assert_eq!(mev.entity_id, "veh");
    assert_eq!(mev.tai_ns, 1_000_000_000);
    assert_eq!(mev.frame_id, "AXES_KIND_ICRF");
    assert_eq!(mev.values.get("dv_y"), Some(&5.0));
    assert_eq!(products.events[2].name, "run_end");
}

// ----------------------------------------------------------------------------------------
// 3. Typed load-time refusals.
// ----------------------------------------------------------------------------------------

fn short_accel_drm(sos_id: &str, drm_id: &str, instance_name: &str, sys_id: &str, event: ScenarioEvent) -> (SystemDefinition, SosConfiguration, DesignReferenceMission) {
    let sys = accel_system(sys_id);
    let sos = hashed_sos(SosConfiguration {
        id: sos_id.to_string(),
        instances: vec![SystemInstance {
            name: instance_name.to_string(),
            system_id: sys_id.to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: sys_id.to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, events: vec![event], ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sys, sos, drm)
}

#[test]
fn a_maneuver_epoch_off_the_sample_grid_is_refused_before_any_binding() {
    let _engine = gmat_sys::engine_lock();
    // 1_050_000_000 ns is not a multiple of the 100ms (0.1 s) output period.
    let (sys, sos, drm) = short_accel_drm("mvr_offgrid_sos", "mvr_offgrid_drm", "veh", "mvr_offgrid_sys", maneuver_event("burn1", 1_050_000_000, "veh", [1.0, 0.0, 0.0], "AXES_KIND_ICRF"));
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-mvr-offgrid".to_string(), error_mode: Default::default() };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::ManeuverEpochNotOnSampleGrid { ref id, .. } if id == "burn1"), "{err:?}");
}

#[test]
fn a_maneuver_naming_an_unknown_instance_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let (sys, sos, drm) = short_accel_drm("mvr_unknown_sos", "mvr_unknown_drm", "veh", "mvr_unknown_sys", maneuver_event("burn1", 1_000_000_000, "not_a_real_instance", [1.0, 0.0, 0.0], "AXES_KIND_ICRF"));
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-mvr-unknown".to_string(), error_mode: Default::default() };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::UnknownManeuverInstance { ref id, ref instance } if id == "burn1" && instance == "not_a_real_instance"), "{err:?}");
}

#[test]
fn a_maneuver_with_an_unsupported_frame_is_refused_through_the_full_executor() {
    let _engine = gmat_sys::engine_lock();
    let (sys, sos, drm) = short_accel_drm("mvr_frame_sos", "mvr_frame_drm", "veh", "mvr_frame_sys", maneuver_event("burn1", 1_000_000_000, "veh", [1.0, 0.0, 0.0], "AXES_KIND_BODY_FIXED"));
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-mvr-frame".to_string(), error_mode: Default::default() };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::ManeuverFrameNotSupported { ref id, .. } if id == "burn1"), "{err:?}");
}

/// M21.3 (`docs/open-questions.md` question 141): a maneuver naming a native instance with an
/// EMPTY declared state space (no physical position/velocity to jump) is refused with a typed
/// `DrmError::ManeuverTargetNotSixDimensional`, never a panic. Through M20.1, every
/// `"native."`-dispatched instance's own physical state was fixed at 6 dimensions
/// (`CONSTANT_ACCEL_STATE_DIM`), so this boundary-loop path was unreachable; M21.3 makes an
/// instance's own width variable (0 or 6, taken from its declared state space), so
/// `crate::drm::executor::run_shared_group`'s own boundary loop can no longer assume every
/// active model span's carried-over state converts to a `[f64; 6]` array before a maneuver's
/// own dv jump. Fails against an implementation that still does that conversion unconditionally
/// (would panic converting a 0-length slice into `[f64; 6]` the moment this boundary is
/// reached) instead of refusing, typed, before ever touching the state.
#[test]
fn a_maneuver_naming_a_native_instance_with_an_empty_state_space_is_a_typed_refusal_not_a_panic() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition {
        id: "mvr_empty_sys".to_string(),
        dynamics_model: "native.range_condition_controller".to_string(),
        state_space_id: "native.controller.empty".to_string(),
        state_space: Some(av_cdm::pb::StateSpace { id: "native.controller.empty".to_string(), components: vec![], frame_id: String::new() }),
        parameters: vec![sparam("frame_id", "test.frame")],
        ..Default::default()
    });
    let sos = hashed_sos(SosConfiguration {
        id: "mvr_empty_sos".to_string(),
        instances: vec![SystemInstance {
            name: "ctrl".to_string(),
            system_id: "mvr_empty_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "mvr_empty_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "mvr_empty_drm".to_string(),
        sos_configuration_id: "mvr_empty_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 2_000_000_000, events: vec![maneuver_event("burn1", 1_000_000_000, "ctrl", [1.0, 0.0, 0.0], "AXES_KIND_ICRF")], ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-mvr-empty".to_string(), error_mode: Default::default() };
    let err = execute(cfg).unwrap_err();
    assert!(
        matches!(err, DrmError::ManeuverTargetNotSixDimensional { ref instance, ref maneuver_id, state_dim: 0 } if instance == "ctrl" && maneuver_id == "burn1"),
        "{err:?}"
    );
}

// ----------------------------------------------------------------------------------------
// 4. Covariance across a burn (question 97's item 5).
// ----------------------------------------------------------------------------------------

const COV_P0_DIAG: [f64; 6] = [10_000.0, 10_000.0, 10_000.0, 0.01, 0.01, 0.01];

fn cov_p0() -> Vec<f64> {
    let mut p0 = vec![0.0; 36];
    for i in 0..6 {
        p0[i * 6 + i] = COV_P0_DIAG[i];
    }
    p0
}

fn cov_drm(sos_id: &str, drm_id: &str, instance_name: &str, events: Vec<ScenarioEvent>) -> (SosConfiguration, DesignReferenceMission) {
    let sos = hashed_sos(SosConfiguration {
        id: sos_id.to_string(),
        instances: vec![SystemInstance {
            name: instance_name.to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 10.0,
            initial_covariance: cov_p0(),
            ..Default::default()
        }],
        ..Default::default()
    });
    // Same epoch leo_1day_golden.drm.yaml uses, 0.4 s long (4 output periods at 0.1 s) -- fast,
    // GMAT-bound, and long enough that Phi has moved measurably away from the identity.
    let start = 1_767_225_637_000_000_000i64;
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        scenario: Some(Scenario { start_tai_ns: start, end_tai_ns: start + 400_000_000, events, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (sos, drm)
}

/// Question 97 item 5: for an impulsive burn with **no execution error**, Phi is unchanged and
/// P is unchanged. Tested here as "a covariance run split by a zero-dv maneuver must match a
/// run with no maneuver at all", the only externally-observable (through the public `execute()`
/// API) form of that claim -- see `run_covariance_instance`'s own "Covariance across a burn"
/// doc comment for the argument this proves end to end (a bug in the segment-restart/
/// carry-through machinery would show up here as a real disagreement; the *dynamics themselves*
/// are identical between the two runs since a zero-dv "burn" changes nothing physical).
#[test]
fn covariance_is_unchanged_across_a_no_execution_error_burn() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let (sos_burn, drm_burn) = cov_drm("leo_sos_cov_zero_burn", "leo_drm_cov_zero_burn", "leo_cov_zero_burn", vec![maneuver_event("burn1", 1_767_225_637_200_000_000, "leo_cov_zero_burn", [0.0, 0.0, 0.0], "AXES_KIND_ICRF")]);
    let cfg_burn = RunConfig { gmat: &gmat, drm: &drm_burn, sos: &sos_burn, systems: &systems, run_id: "test-run-cov-zero-burn".to_string(), error_mode: Default::default() };
    let products_burn = execute(cfg_burn).expect("covariance DRM with a zero-dv maneuver executes end to end");
    let traj_burn = products_burn.trajectories.get("leo_cov_zero_burn").unwrap();
    assert_eq!(traj_burn.segments.len(), 2, "the zero-dv maneuver still splits the run into two segments");

    let (sos_plain, drm_plain) = cov_drm("leo_sos_cov_no_burn", "leo_drm_cov_no_burn", "leo_cov_no_burn", vec![]);
    let cfg_plain = RunConfig { gmat: &gmat, drm: &drm_plain, sos: &sos_plain, systems: &systems, run_id: "test-run-cov-no-burn".to_string(), error_mode: Default::default() };
    let products_plain = execute(cfg_plain).expect("covariance DRM with no maneuver executes end to end");
    let traj_plain = products_plain.trajectories.get("leo_cov_no_burn").unwrap();
    assert_eq!(traj_plain.segments.len(), 1);

    assert_eq!(traj_burn.samples.len(), traj_plain.samples.len());
    let last_burn = traj_burn.samples.last().unwrap();
    let last_plain = traj_plain.samples.last().unwrap();
    assert_eq!(last_burn.tai_ns, last_plain.tai_ns);

    let dr: f64 = (0..3).map(|i| (last_burn.mean[i] - last_plain.mean[i]).powi(2)).sum::<f64>().sqrt();
    let dv: f64 = (3..6).map(|i| (last_burn.mean[i] - last_plain.mean[i]).powi(2)).sum::<f64>().sqrt();
    let cov_err: f64 = last_burn.cov.iter().zip(last_plain.cov.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    let cov_norm: f64 = last_plain.cov.iter().map(|v| v * v).sum::<f64>().sqrt();
    let cov_rel_err = cov_err / cov_norm;
    eprintln!("[drm_maneuver covariance] split-vs-continuous over 0.4 s: |dr|={dr:.3e} m, |dv|={dv:.3e} m/s, covariance Frobenius error {cov_err:.3e} (rel {cov_rel_err:.3e})");
    assert!(dr < 1e-6, "a zero-dv maneuver split must not move the mean position: {dr} m");
    assert!(dv < 1e-6, "a zero-dv maneuver split must not move the mean velocity: {dv} m/s");
    assert!(cov_rel_err < 1e-6, "covariance relative error {cov_rel_err:.3e} exceeds 1e-6 -- the burn boundary must not perturb P beyond the split-vs-continuous integration noise floor");

    // Sanity: the covariance actually evolved from P0 over 0.4 s (this is not a degenerate
    // "everything stayed at P0" comparison).
    let p0 = cov_p0();
    let moved: f64 = last_plain.cov.iter().zip(p0.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    assert!(moved > 1e-9, "sanity: covariance should have evolved measurably from P0 over 0.4 s, moved = {moved:.3e}");

    av_cdm::covariance::check_spd_row_major(&last_burn.cov, 6, "drm_maneuver covariance test, split run").expect("SPD hygiene");
    av_cdm::covariance::check_spd_row_major(&last_plain.cov, 6, "drm_maneuver covariance test, continuous run").expect("SPD hygiene");
}

/// M13.3: a maneuver landing on the trajectory's own `sample_interval_s` output grid (so
/// [`DrmError::ManeuverEpochNotOnSampleGrid`] does not fire) but *not* on a coarser
/// covariance-requesting instance's own native step grid must still be refused -- not run with a
/// NaN-poisoned covariance silently carried into the post-burn span's own `p0` (see
/// `run_covariance_instance`'s own "Covariance across a burn" doc comment section, and
/// `executor`'s module doc comment's M13.3 note). `step_rate_hz = 2.0` (500 ms) with
/// `sample_interval_s = 0.1` (100 ms): the burn at 300 ms is on the 100 ms grid but not the
/// 500 ms one.
#[test]
fn a_maneuver_on_the_sample_grid_but_off_a_coarser_covariance_grid_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let systems = load_golden_leo_sys();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let start = 1_767_225_637_000_000_000i64;
    let sos = hashed_sos(SosConfiguration {
        id: "leo_sos_cov_offgrid_burn".to_string(),
        instances: vec![SystemInstance {
            name: "leo_cov_offgrid_burn".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 2.0, // 500 ms: coarser than the 100 ms sample_interval_s below
            initial_covariance: cov_p0(),
            ..Default::default()
        }],
        ..Default::default()
    });
    // 300 ms: an exact multiple of the 100 ms sample_interval_s grid, but not of the instance's
    // own 500 ms covariance step.
    let drm = hashed_drm(DesignReferenceMission {
        id: "leo_drm_cov_offgrid_burn".to_string(),
        sos_configuration_id: "leo_sos_cov_offgrid_burn".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: start,
            end_tai_ns: start + 1_000_000_000,
            events: vec![maneuver_event("burn1", start + 300_000_000, "leo_cov_offgrid_burn", [1.0, 0.0, 0.0], "AXES_KIND_ICRF")],
            ..Default::default()
        }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-cov-offgrid-burn".to_string(), error_mode: Default::default() };
    let err = execute(cfg).unwrap_err();
    assert!(
        matches!(err, DrmError::ManeuverEpochNotOnCovarianceGrid { ref id, ref instance, tai_ns, period_ns } if id == "burn1" && instance == "leo_cov_offgrid_burn" && tai_ns == start + 300_000_000 && period_ns == 500_000_000),
        "{err:?}"
    );
}

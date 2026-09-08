//! The DRM executor's required acceptance tests (M6.1, `docs/open-questions.md` question 87):
//!
//! 1. `drms/leo_1day_golden.drm.yaml` (+ its `.sos.yaml`/`.system.yaml`) runs through
//!    [`av_kernel::drm::execute`] and matches `goldens/leo_1day_jgm2_8x8_sunmoon.json` within
//!    its own recorded tolerances.
//! 2. A tampered hash is refused with the typed error.
//! 3. A DRM with `covariance = true` against a `RelativisticCorrection` force model is refused
//!    unless `accept_missing_stm_terms`.
//! 4. Non-model bindings are refused with the typed error -- `BINDING_KIND_RENODE` here since
//!    M13.2 (`docs/open-questions.md` question 107): `a_container_binding_is_refused_through_
//!    the_full_executor` used `BINDING_KIND_CONTAINER` for this through that task, but a
//!    container binding is real, classified behaviour as of M13.2 (see
//!    `crates/av-kernel/tests/drm_container.rs`), so this test now uses `BINDING_KIND_RENODE`
//!    (still Planned/unsupported) to keep exercising "an unsupported binding kind is refused,
//!    never silently skipped" at the full-`execute()` level.
//!
//! Also (not individually required, but "What to build" items 4/5 -- `DrmOptions` driving
//! real behaviour and DYNAMICS fault injection): `a_dynamics_fault_splits_the_run_into_two_
//! segments_with_continuous_state`, entirely GMAT-free (a native `"accel.x"` binding).
//!
//! Every test here constructs a real `gmat_sys::Gmat` handle (`RunConfig.gmat`) even where the
//! DRM under test is refused before any GMAT call is actually made (tests 2-4): `execute`
//! takes `&Gmat` unconditionally so it can bind a `"gmat."`-dispatched instance when one
//! survives validation, and `Gmat::setup` is cheap after the first call in a process
//! (`std::sync::Once`) -- see `crates/av-kernel/src/drm/mod.rs`'s module doc comment. Every
//! test takes `gmat_sys::engine_lock()` first, per this repository's existing convention.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{Binding, BindingKind, Connection, DesignReferenceMission, DrmOptions, ModelBinding, Parameter, Scenario, SosConfiguration, SystemDefinition, SystemInstance, Unit};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig};
use av_kernel::kernel::covariance;
use gmat_sys::Gmat;
use prost::Message;
use serde::Deserialize;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn load_golden_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.drm.yaml")).unwrap()).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.sos.yaml")).unwrap()).expect("SosConfiguration parses");
    let sys = schema::parse_system_definition_yaml(&std::fs::read_to_string(drms_path("leo_1day_golden.system.yaml")).unwrap()).expect("SystemDefinition parses");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    (drm, sos, systems)
}

#[derive(Deserialize)]
struct Golden {
    final_state: Vec<f64>,
    tolerance_m: f64,
    tolerance_mps: f64,
}

fn golden() -> Golden {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Required test 1: the golden DRM, expressed as YAML, run end to end through the executor,
/// matches the golden arc's own recorded tolerance.
#[test]
fn drm_matches_the_golden_arc() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_golden_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-drm-golden-1".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("DRM executes end to end");

    let traj = products.trajectories.get("leo").expect("the \"leo\" instance produced a trajectory");
    assert_eq!(traj.segments.len(), 1, "no faults declared: exactly one dynamics segment");
    let last = traj.samples.last().expect("at least one sample");

    let g = golden();
    let golden_final_si = av_cdm::units::state_km_to_m(<[f64; 6]>::try_from(g.final_state.as_slice()).expect("6-element final_state"));
    let dr = (0..3).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    let dv = (3..6).map(|i| (last.mean[i] - golden_final_si[i]).powi(2)).sum::<f64>().sqrt();
    eprintln!("[drm_executor] {} samples; |dr| = {dr:.4} m (tol {}), |dv| = {dv:.3e} m/s (tol {})", traj.samples.len(), g.tolerance_m, g.tolerance_mps);
    assert!(dr < g.tolerance_m, "DRM-path position error {dr} m exceeds golden tolerance {} m", g.tolerance_m);
    assert!(dv < g.tolerance_mps, "DRM-path velocity error {dv} m/s exceeds golden tolerance {} m/s", g.tolerance_mps);

    // Provenance carries the DRM hash, the SosConfiguration hash, the data-pack hash, and the
    // caller-supplied run_id (question 87's "What to build" item 6).
    assert_eq!(traj.config_hash, drm.hash, "Trajectory.config_hash must be the DRM's own hash");
    let prov = traj.provenance.as_ref().expect("provenance set");
    assert_eq!(prov.config_hash, sos.hash, "Provenance.config_hash must be the SosConfiguration's hash");
    assert_eq!(prov.data_pack_hash, drm.scenario.as_ref().unwrap().data_pack_hash);
    assert_eq!(prov.run_id, "test-run-drm-golden-1");
    assert_eq!(prov.attributes.get("system_definition_id").map(String::as_str), Some("leo_sys"));

    // RunProducts itself (question 93): the run's own overall provenance carries the DRM's own
    // hash and the run id, and the golden DRM declares no objectives/measures, so scores are
    // honestly empty rather than fabricated.
    assert_eq!(products.provenance.config_hash, drm.hash, "RunProducts.provenance.config_hash must be the DRM's own hash");
    assert_eq!(products.provenance.run_id, "test-run-drm-golden-1");
    assert!(products.scores.is_empty(), "the golden DRM declares no objectives/measures");

    // RunProducts.events (question 95, M9.3): no faults declared in this DRM, so exactly the
    // one "leo" instance's own run_start/run_end EVENT_KIND_LIFECYCLE pair -- sorted (epoch,
    // id), so run_start comes first.
    assert_eq!(products.events.len(), 2, "{:?}", products.events);
    assert!(products.events.iter().all(|e| e.entity_id == "leo" && e.kind == av_cdm::pb::EventKind::Lifecycle as i32));
    assert_eq!(products.events[0].name, "run_start");
    assert_eq!(products.events[1].name, "run_end");
    assert!(products.events[0].tai_ns < products.events[1].tai_ns);
    let event_prov = products.events[0].provenance.as_ref().expect("event provenance set");
    assert_eq!(event_prov.config_hash, sos.hash, "Event.provenance.config_hash must be the SosConfiguration's hash, like Trajectory.provenance");
    assert_eq!(event_prov.run_id, "test-run-drm-golden-1");

    // RunProducts.frames (question 121/122, M17.2; question 10/124, M18.1): the golden system
    // declares spacecraft.CoordinateSystem = "EarthMJ2000Eq" (drms/leo_1day_golden.system.yaml),
    // which is exactly "leo"'s own Trajectory.frame_id -- so RunProducts.frames must carry the
    // matching registry-default FrameDefinition, genuinely derived from the real run's own
    // trajectory. As of M18.1, question 10's mandatory ICRF/MJ2000Eq/BodyFixed frames for the
    // run's own central body (Earth, force_model.central_body) are always added on top of that,
    // regardless of which one the instance actually propagated in -- so this run's own
    // EarthMJ2000Eq propagation frame is joined by EarthICRF/EarthBodyFixed even though neither
    // is referenced by any trajectory here. Sorted by id (ADR-004: no HashMap iteration order).
    // Fails against an executor that never calls collect_frames (frames would be empty), one
    // that fabricates a frame unrelated to what the run actually used, or one that regressed
    // M18.1's mandatory-frame augmentation (would leave this at 1 entry again).
    assert_eq!(traj.frame_id, "EarthMJ2000Eq");
    assert_eq!(products.frames.len(), 3, "{:?}", products.frames);
    let by_id: std::collections::BTreeMap<&str, &av_cdm::pb::FrameDefinition> = products.frames.iter().map(|f| (f.id.as_str(), f)).collect();
    assert_eq!(by_id.keys().copied().collect::<Vec<_>>(), vec!["EarthBodyFixed", "EarthICRF", "EarthMJ2000Eq"]);
    assert_eq!(by_id["EarthMJ2000Eq"].origin, Some(av_cdm::pb::frame_definition::Origin::Body("Earth".to_string())));
    assert_eq!(by_id["EarthMJ2000Eq"].axes, av_cdm::pb::AxesKind::Mj2000Eq as i32);
    assert_eq!(by_id["EarthICRF"].axes, av_cdm::pb::AxesKind::Icrf as i32, "question 10's mandatory ICRF frame, present even though nothing propagated in it");
    assert_eq!(by_id["EarthBodyFixed"].axes, av_cdm::pb::AxesKind::BodyFixed as i32, "question 10's mandatory body-fixed frame, present even though nothing propagated in it");

    // RunProducts -> altavista.v1.RunProducts (question 121, M17.2): the real golden run's own
    // RunProducts converts and round-trips through real protobuf bytes with nothing lost --
    // trajectories, events, provenance, frames, and the (here honestly zero) dropped count.
    // Fails against a to_proto that drops the frame, mismatches the trajectory map, or leaves
    // dropped_in_flight_messages/frames at their proto defaults regardless of the real values.
    let proto = products.to_proto();
    let decoded = av_cdm::pb::RunProducts::decode(proto.encode_to_vec().as_slice()).expect("valid altavista.v1.RunProducts bytes");
    assert_eq!(decoded.run_id, "test-run-drm-golden-1");
    assert_eq!(decoded.trajectories.get("leo").map(|t| t.samples.len()), Some(traj.samples.len()));
    assert_eq!(decoded.frames, proto.frames);
    assert_eq!(decoded.dropped_in_flight_messages, 0);
    assert!(decoded.provenance.is_some());
}

/// `docs/open-questions.md` question 108, M13.1's "nothing existing changes" proof: the golden
/// bundle's own canonical hashes -- computed from the parsed `pb::SystemDefinition`/
/// `pb::SosConfiguration`/`pb::DesignReferenceMission`, which now flow through the typed
/// `Port`/`PortTiming` loader (`crate::drm::schema::RawPort`) instead of the old
/// refuse-if-nonempty check -- are byte-identical to what they were before this task, asserted
/// against the literal hex digests hardcoded in `drms/leo_1day_golden.{system,sos,drm}.yaml`
/// themselves (not merely "the loader agrees with itself": these three string literals were
/// copied verbatim from those files, so this is an independent check that does not rely on
/// `execute`'s own internal `hash::verify_*_hash` calls agreeing with anything -- a byte
/// changed anywhere in the canonical protobuf encoding, including in the now-typed empty
/// `ports`/`variants` fields, would flip every one of these digests). `execute` above already
/// exercises the same check internally (it refuses on a mismatch), but this test makes the
/// "byte-identical, not eyeballed" claim explicit and stand on its own.
#[test]
fn the_golden_bundles_canonical_hashes_are_byte_identical_to_before_the_typed_port_loader() {
    let (drm, sos, systems) = load_golden_bundle();
    let sys = systems.get("leo_sys").expect("golden bundle declares leo_sys");

    assert_eq!(hash::canonical_system_hash(sys), "bf49e03be4e63ced9b7442058544d427776e257af88bfeecb343b840220c0f25", "leo_1day_golden.system.yaml's own declared hash, hardcoded here independently of the loader");
    assert_eq!(hash::canonical_sos_hash(&sos), "ab9f12339adacda6ecfad20a2e6f32ec40ce6b50f0bbe01cff0da828cef261e3", "leo_1day_golden.sos.yaml's own declared hash, hardcoded here independently of the loader");
    assert_eq!(hash::canonical_drm_hash(&drm), "4694f388692c23d86e7f54baf5b087b4821b1ff01c037a4e7f82403b7c45bc43", "leo_1day_golden.drm.yaml's own declared hash, hardcoded here independently of the loader");

    // Also the files' own declared `hash` fields, which `parse_*_yaml` copies through
    // unmodified -- confirms the fixture itself was not silently edited underneath this test.
    assert_eq!(sys.hash, "bf49e03be4e63ced9b7442058544d427776e257af88bfeecb343b840220c0f25");
    assert_eq!(sos.hash, "ab9f12339adacda6ecfad20a2e6f32ec40ce6b50f0bbe01cff0da828cef261e3");
    assert_eq!(drm.hash, "4694f388692c23d86e7f54baf5b087b4821b1ff01c037a4e7f82403b7c45bc43");

    // And explicitly: this system's own ports field went through the typed RawPort loader
    // (rather than the old refuse-if-nonempty opaque check) and still came out empty, which is
    // exactly why the hash above is unchanged -- an empty repeated field encodes to zero bytes
    // either way.
    assert!(sys.ports.is_empty(), "the golden system declares no ports");
}

/// M10.2 (`docs/open-questions.md` question 95's second half), updated at M11.1 (question 99):
/// `output.<instance>.rmag@time` resolves a *real* `gmat_sys::model::GmatModel::step`-produced
/// value (`gmat_sys::model::OUTPUT_RMAG`) -- carried through `crate::kernel::HeteroKernel`'s
/// outputs plumbing and this executor's `OUTPUT_PARAMETER_PREFIX`-declared, filtered wiring --
/// checked against a genuine GMAT `ReportFile` reading for the *same* golden arc
/// (`goldens/leo_1day_jgm2_8x8_sunmoon.json`).
///
/// **As of M11.1, `rmag` is no longer computed in Rust.** `GmatModel::step` writes the
/// propagated state back into the bound spacecraft and reads `"RMAG"` through
/// `gmat_sys::DerivativeModel::real_parameter` -- GMAT's own `GetRealParameter`, through the
/// shim's new `gmatffi_get_real_parameter` (question 99) -- so this comparison is now GMAT's own
/// real-parameter subsystem against a second, independent GMAT code path (a script +
/// `ReportFile`), not "this same formula recomputed on this run's own final sample" the way it
/// was through M10.2. This test also checks `output.<instance>.cd@time`
/// (`gmat_sys::model::OUTPUT_CD`), a spacecraft property (`Cd`) this crate never computes or
/// stores in Rust at all -- proving the real-parameter call genuinely reaches into GMAT's own
/// object, not merely that `rmag`'s arithmetic still happens to agree.
///
/// **How the ReportFile value was produced** (`goldens/gen_leo_1day_rmag.py`, run explicitly --
/// its own doc comment has the full rationale -- producing `goldens/
/// leo_1day_jgm2_8x8_sunmoon_rmag.json`, read below): this repository's documented `ReportFile`
/// quirk is a script run with an injected `ReportFile`, `SolverIterations = Current`, never
/// `Execute()` through the object API -- `altavista/scenario.py::prepare_script`/`report_block`'s
/// own pattern. A GMAT script built from this golden's own epoch/spacecraft/force-model/propagator
/// parameters, with `Create ReportFile ...; SolverIterations = Current; Add =
/// {Golden.A1ModJulian, Golden.EarthMJ2000Eq.{X,Y,Z,VX,VY,VZ}, Golden.Earth.RMAG};`, run through
/// `gmat.LoadScript`/`gmat.RunScript` (real script-engine execution, which does write the
/// file). The report's last row (`Precision = 16`): `A1ModJulian=31042.50042863867`,
/// `X=-1869.022450937179`, `Y=3841.870677714497`, `Z=5380.309814761908`,
/// `RMAG=6870.294675573457` (km). That row's X/Y/Z agree with this golden's own recorded
/// `final_state` to <= 1.7 mm (a different GMAT code path -- one big script `Propagate` vs. the
/// golden generator's own chunked `Propagator.Step` loop -- both at `Accuracy = 1e-13`), and its
/// own `RMAG` column agrees with `sqrt(X^2+Y^2+Z^2)` computed from that same row to ~9e-13 km
/// (floating-point noise), confirming the ReportFile mechanism really is reporting GMAT's `RMAG`
/// real parameter, defined exactly as the position-vector magnitude.
#[test]
fn drm_rmag_output_matches_a_genuine_gmat_reportfile() {
    let _engine = gmat_sys::engine_lock();
    let (drm, _sos, systems) = load_golden_bundle();
    // A fresh SystemDefinition id/hash (own instance of leo_1day_golden.system.yaml's leo_sys,
    // plus one declared "output.rmag" parameter -- see crate::drm::executor
    // ::OUTPUT_PARAMETER_PREFIX's doc comment) rather than mutating the golden bundle's own
    // `leo_sys` in place, so `drm_matches_the_golden_arc`'s pinned artifact is untouched.
    let mut sys = systems.get("leo_sys").expect("golden bundle declares leo_sys").clone();
    sys.id = "leo_rmag_sys".to_string();
    sys.parameters.push(Parameter {
        name: "output.rmag".to_string(),
        unit: Unit::Meter as i32,
        description: "position magnitude from the central body -- GMAT's own <Sat>.<Body>.RMAG real parameter, read through gmat_sys::DerivativeModel::real_parameter (question 99), not computed in Rust".to_string(),
        ..Default::default()
    });
    // Question 99's "non-derivable parameter" requirement: Cd is a spacecraft property this
    // crate never computes or stores in Rust anywhere -- it is set once, in C++, from this same
    // SystemDefinition's own "spacecraft.Cd" parameter (leo_1day_golden.system.yaml: 2.2) by
    // `binding::materialize_gmat`, and reading it back correctly after a step proves
    // `real_parameter` genuinely reaches GMAT's own object, not that some Rust-side value merely
    // agrees with itself.
    sys.parameters.push(Parameter {
        name: "output.cd".to_string(),
        unit: Unit::Dimensionless as i32,
        description: "the spacecraft's own Cd (drag coefficient), read through gmat_sys::DerivativeModel::real_parameter -- a value this crate never computes, only relays".to_string(),
        ..Default::default()
    });
    let sys = hashed_system(sys);

    let sos = hashed_sos(SosConfiguration {
        id: "leo_rmag_sos".to_string(),
        instances: vec![SystemInstance {
            // A distinct instance name from every other test in this file/binary
            // ("leo"/"leo_out" are already used) -- see the module doc comment's "GMAT object
            // naming" section: GMAT's configuration manager is process-global, so reusing a
            // name here would collide with objects a different test already constructed.
            name: "leo_rmag".to_string(),
            system_id: "leo_rmag_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_rmag_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let rmag_drm = hashed_drm(DesignReferenceMission {
        id: "leo_rmag_drm".to_string(),
        sos_configuration_id: "leo_rmag_sos".to_string(),
        // The golden's own full one-day scenario window (same start_tai_ns/end_tai_ns
        // leo_1day_golden.drm.yaml declares) -- the ReportFile value above is this arc's own
        // final state, not a shortened stand-in.
        scenario: drm.scenario.clone(),
        measures: vec![
            av_cdm::pb::MeasureOfEffectiveness { name: "rmag_at_end".to_string(), expression: "output.leo_rmag.rmag@end".to_string(), unit: Unit::Meter as i32 },
            av_cdm::pb::MeasureOfEffectiveness { name: "cd_at_end".to_string(), expression: "output.leo_rmag.cd@end".to_string(), unit: Unit::Dimensionless as i32 },
        ],
        options: drm.options,
        ..Default::default()
    });

    let mut systems_map = BTreeMap::new();
    systems_map.insert(sys.id.clone(), sys);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &rmag_drm, sos: &sos, systems: &systems_map, run_id: "test-run-rmag".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("DRM executes end to end");

    let score = &products.scores["rmag_at_end"];
    assert_eq!(score.unit, Unit::Meter);
    assert_eq!(score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");

    // GMAT's own ReportFile value, RMAG column, last row -- goldens/leo_1day_jgm2_8x8_sunmoon
    // _rmag.json, generated by goldens/gen_leo_1day_rmag.py (a genuine GMAT script run, per
    // that generator's own doc comment for the exact settings; see this test's own doc comment
    // for a summary).
    let rmag_golden_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon_rmag.json");
    let rmag_golden: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(rmag_golden_path).unwrap()).unwrap();
    let gmat_reportfile_rmag_m = rmag_golden["rmag_m"].as_f64().expect("rmag_m is a JSON number");
    let err = (score.value - gmat_reportfile_rmag_m).abs();
    eprintln!(
        "[drm_executor rmag] output.leo_rmag.rmag@end = {:.6} m; GMAT ReportFile RMAG = {gmat_reportfile_rmag_m:.6} m; |err| = {err:.6} m",
        score.value
    );
    // Same order of magnitude as the base golden's own 0.05 m position tolerance
    // (goldens/leo_1day_jgm2_8x8_sunmoon.json's tolerance_m) -- rmag is a smooth function of
    // position alone, so its own error should not exceed the position error by much; a looser
    // bound than 0.05 m to leave headroom for this run's own km<->m and epoch round-trips,
    // still two orders of magnitude tighter than a value that would indicate a real bug (a unit
    // or frame mistake would show up as a multi-kilometre disagreement, not a sub-metre one).
    assert!(err < 0.1, "rmag output {} m disagrees with GMAT's own ReportFile value {gmat_reportfile_rmag_m} m by {err} m, exceeding the 0.1 m bound", score.value);

    // Question 99's non-derivable-parameter proof: leo_1day_golden.system.yaml's own
    // "spacecraft.Cd" parameter is 2.2 -- this crate never reads that YAML value back into any
    // Rust-side state once `binding::materialize_gmat` hands it to GMAT via `set_real("Cd",
    // 2.2)`, so the only way `output.leo_rmag.cd@end` can come back as 2.2 is a real
    // `GmatBase::GetRealParameter("Cd")` call succeeding against the actual bound Spacecraft.
    let cd_score = &products.scores["cd_at_end"];
    assert_eq!(cd_score.unit, Unit::Dimensionless);
    assert_eq!(cd_score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");
    assert!(
        (cd_score.value - 2.2).abs() < 1e-9,
        "Cd output {} disagrees with the spacecraft's own configured Cd (2.2) by more than floating-point noise -- gmatffi_get_real_parameter is not reading the real spacecraft object",
        cd_score.value
    );
}

/// M7.2 (question 89): `SystemInstance.initial_covariance` carries the golden's own P0
/// (`drms/leo_1day_golden.sos.yaml`'s `leo` instance -- diag(10000, 10000, 10000, 0.01, 0.01,
/// 0.01) SI, the same seed `tests/golden_acceptance.rs`'s own
/// `kernel_covariance_matches_the_golden_stm_and_propagated_cov` feeds the kernel directly),
/// read by `executor::load_initial_covariance` and run through the *full* DRM executor -- not
/// just the kernel -- over the golden's own one-day arc, and checked against
/// `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.cov_t1_si` with the same Frobenius
/// relative-error bound `golden_acceptance.rs` uses (< 1e-6). This is the "the executor reads
/// `SystemInstance.initial_covariance`" proof -- the former `covariance.p0_row_major`
/// parameter-string convention no longer exists anywhere in this crate, so this is the only
/// path P0 can reach the kernel through now.
///
/// Built as its own `SosConfiguration`/`DesignReferenceMission` (reusing only the golden's
/// `leo_sys` `SystemDefinition` and its `leo` instance's own `initial_covariance` values from
/// `drms/leo_1day_golden.sos.yaml`) rather than mutating `drm_matches_the_golden_arc`'s own
/// loaded bundle in place: instance name `leo` is already used by that test's own GMAT
/// objects (`Drmleo_0...`), and GMAT's configuration manager is process-global -- see the
/// module doc comment's "GMAT object naming" section, and `covariance` module's own note that
/// a second GMAT-touching DRM test needs its own instance naming to stay collision-free. This
/// test's instance is named `leo_cov` for exactly that reason.
#[test]
fn drm_covariance_matches_the_golden_stm_and_propagated_cov() {
    let _engine = gmat_sys::engine_lock();
    let (golden_drm, golden_sos, systems) = load_golden_bundle();
    let p0 = golden_sos.instances[0].initial_covariance.clone();
    assert_eq!(p0.len(), 36, "drms/leo_1day_golden.sos.yaml's \"leo\" instance must carry the golden's 6x6 initial_covariance");

    let sos = hashed_sos(SosConfiguration {
        id: "leo_sos_cov".to_string(),
        instances: vec![SystemInstance {
            name: "leo_cov".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 10.0,
            initial_covariance: p0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "leo_drm_cov".to_string(),
        sos_configuration_id: "leo_sos_cov".to_string(),
        scenario: golden_drm.scenario.clone(),
        options: Some(DrmOptions { covariance: true, ..golden_drm.options.expect("golden DRM has options") }),
        ..Default::default()
    });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-drm-covariance".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("covariance DRM executes end to end");

    let traj = products.trajectories.get("leo_cov").expect("the \"leo_cov\" instance produced a trajectory");
    let last = traj.samples.last().expect("at least one sample");
    assert_eq!(last.cov.len(), 36, "covariance requested: the final sample must carry a 6x6 cov");

    #[derive(Deserialize)]
    struct Stm {
        cov_t1_si: Vec<f64>,
    }
    #[derive(Deserialize)]
    struct GoldenWithStm {
        stm: Stm,
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../goldens/leo_1day_jgm2_8x8_sunmoon.json");
    let g: GoldenWithStm = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let golden_norm: f64 = g.stm.cov_t1_si.iter().map(|v| v * v).sum::<f64>().sqrt();
    let err: f64 = last.cov.iter().zip(g.stm.cov_t1_si.iter()).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    let rel_err = err / golden_norm;
    eprintln!("[drm_executor covariance] Frobenius error {err:.6e} (rel {rel_err:.3e}) vs golden cov_t1_si");
    assert!(rel_err < 1e-6, "DRM-path covariance relative error {rel_err:.3e} exceeds 1e-6 of the golden's own covariance norm");

    av_cdm::covariance::check_spd_row_major(&last.cov, 6, "drm_executor covariance test final sample")
        .expect("the DRM-path final covariance must pass the same SPD hygiene bar the kernel already enforces internally");
}

/// M13.3: an instance's own `step_rate_hz` no longer has to equal `DrmOptions.sample_interval_s`
/// -- this instance propagates covariance at 2 Hz (500 ms) while the DRM's own trajectory is
/// sampled at 10 Hz (100 ms), a combination `run_covariance_instance` refused outright before
/// this task (`DrmError::InvalidDrmOptions`, "covariance requires its effective step rate to
/// equal DrmOptions.sample_interval_s's rate exactly"). Over a 1.5 s scenario (three native
/// covariance steps at 0/500/1000/1500 ms, fifteen 100 ms output ticks), every output epoch on
/// the instance's own 500 ms native grid must carry a real covariance (`av_kernel::kernel::
/// covariance` returns `Some`) and every other epoch must carry none (`None`) -- the full
/// DRM-executor-level proof of `crates/av-kernel/src/kernel.rs`'s own
/// `run_with_covariance_at_a_coarser_instance_period_propagates_only_on_the_native_grid` unit
/// test, driven through real GMAT propagation this time. (Question 111, M14.3: this test used to
/// classify `cov` into `CovarianceAvailability::{Available, UnavailableAtThisSample,
/// NotRequested}` off an all-NaN sentinel for the middle case -- both are deleted; there is only
/// one requested system here ("leo_coarse_cov"), so `None` is unambiguous.)
#[test]
fn covariance_at_a_coarser_step_rate_than_the_sample_interval_succeeds_end_to_end() {
    let _engine = gmat_sys::engine_lock();
    let (_, golden_sos, systems) = load_golden_bundle();
    let p0 = golden_sos.instances[0].initial_covariance.clone();
    assert_eq!(p0.len(), 36, "drms/leo_1day_golden.sos.yaml's \"leo\" instance must carry the golden's 6x6 initial_covariance");

    let sos = hashed_sos(SosConfiguration {
        id: "leo_sos_coarse_cov".to_string(),
        instances: vec![SystemInstance {
            name: "leo_coarse_cov".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 2.0, // 500 ms: coarser than the 100 ms sample_interval_s below
            initial_covariance: p0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let start_tai_ns = 1_767_225_637_000_000_000;
    let drm = hashed_drm(DesignReferenceMission {
        id: "leo_drm_coarse_cov".to_string(),
        sos_configuration_id: "leo_sos_coarse_cov".to_string(),
        scenario: Some(Scenario { start_tai_ns, end_tai_ns: start_tai_ns + 1_500_000_000, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-coarse-cov".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("a covariance step coarser than the sample interval must now succeed end to end (M13.3)");

    let traj = products.trajectories.get("leo_coarse_cov").expect("instance produced a trajectory");
    assert_eq!(traj.samples.len(), 16, "1.5 s at 100 ms output = 16 samples");
    let t0 = traj.samples.first().unwrap().tai_ns;

    let mut available = 0;
    let mut unavailable = 0;
    for s in &traj.samples {
        let on_grid = (s.tai_ns - t0) % 500_000_000 == 0;
        match covariance(s) {
            Some(cov) => {
                assert!(on_grid, "tai_ns {}: real covariance off the instance's own 500 ms native grid", s.tai_ns);
                assert_eq!(cov.len(), 36);
                av_cdm::covariance::check_spd_row_major(cov, 6, "coarse covariance DRM sample").expect("a real covariance sample must pass the SPD hygiene bar");
                available += 1;
            }
            None => {
                // Question 111: no covariance sample here, whether "not requested" or
                // "unavailable at this sample" -- this instance requested covariance, so `None`
                // here is unambiguous and must mean the latter.
                assert!(!on_grid, "tai_ns {}: no covariance but is on the instance's own native grid", s.tai_ns);
                assert!(s.cov.is_empty(), "an unavailable sample's cov must be empty on the wire, got {} entries", s.cov.len());
                unavailable += 1;
            }
        }
    }
    assert_eq!(available, 4, "t = 0, 500, 1000, 1500 ms");
    assert_eq!(unavailable, 12);
}

/// A short (one output-period) covariance-enabled DRM against the golden's own `leo_sys`
/// system definition, for the two load-time-refusal tests below -- fast (a single GMAT step
/// with STM, not the golden's full one-day arc) since these only need to reach
/// `executor::load_initial_covariance`, not produce a physically meaningful trajectory. Each
/// caller passes its own `instance_name` (never `"leo"`/`"leo_cov"`, both already used by
/// other GMAT-touching tests in this file) -- see the module doc comment's "GMAT object
/// naming" section for why a shared name would collide.
fn short_covariance_drm(sos_id: &str, drm_id: &str, instance_name: &str, initial_covariance: Vec<f64>) -> (DesignReferenceMission, SosConfiguration) {
    let sos = hashed_sos(SosConfiguration {
        id: sos_id.to_string(),
        instances: vec![SystemInstance {
            name: instance_name.to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 10.0,
            initial_covariance,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: drm_id.to_string(),
        sos_configuration_id: sos_id.to_string(),
        // Same epoch `leo_1day_golden.drm.yaml` uses (its own comment derives it); only 0.1 s
        // long, one exact sample_interval_s period.
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_637_100_000_000, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });
    (drm, sos)
}

/// An empty `SystemInstance.initial_covariance` under `covariance = true` is refused with the
/// typed error, not a silent default (question 11).
#[test]
fn covariance_requested_without_initial_covariance_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let (_, _, systems) = load_golden_bundle();
    let (drm, sos) = short_covariance_drm("leo_sos_missing_p0", "leo_drm_missing_p0", "leo_missing_p0", vec![]);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-missing-p0".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::MissingInitialCovariance { ref instance } if instance == "leo_missing_p0"), "{err:?}");
}

/// Question 83 applied to P0 itself: a hand-built asymmetric `SystemInstance.initial_covariance`
/// fails the Cholesky-based SPD hygiene check *at load*, before any propagation, and is refused
/// with the typed `DrmError::CovarianceHygiene` (counted via
/// `av_cdm::covariance::spd_check_failures()`) rather than silently accepted or only caught
/// later by `Kernel::run_with_covariance`'s own check of the propagated result.
#[test]
fn a_non_spd_initial_covariance_is_refused_with_the_typed_hygiene_error_and_counted() {
    let _engine = gmat_sys::engine_lock();
    let (_, _, systems) = load_golden_bundle();
    // diag = 1.0 everywhere (finite, otherwise plausible), but (0,1) = 0.5 while (1,0) stays
    // 0.0 -- asymmetric, caught before any Cholesky attempt is even tried
    // (av_cdm::covariance::check_spd's own documented short-circuit order).
    let mut bad_p0 = vec![0.0; 36];
    for i in 0..6 {
        bad_p0[i * 6 + i] = 1.0;
    }
    bad_p0[1] = 0.5;
    let (drm, sos) = short_covariance_drm("leo_sos_bad_p0", "leo_drm_bad_p0", "leo_bad_p0", bad_p0);

    let before = av_cdm::covariance::spd_check_failures();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-bad-p0".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::CovarianceHygiene(_)), "{err:?}");
    assert!(av_cdm::covariance::spd_check_failures() > before, "the load-time SPD failure must be counted (other tests may increment it concurrently, hence '>' not '== before + 1')");
}

/// Required test 2: a tampered hash is refused with the typed error.
#[test]
fn a_tampered_drm_hash_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_golden_bundle();
    // Flip one character of an otherwise-correct hash -- a single-bit tamper, not a
    // conspicuously-wrong value, to actually exercise the comparison rather than an
    // accidentally-obviously-different string.
    let mut chars: Vec<char> = drm.hash.chars().collect();
    chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
    drm.hash = chars.into_iter().collect();

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-tampered".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::HashMismatch { artifact: "DesignReferenceMission", .. }), "{err:?}");
}

/// Also required: a DRM referencing an untampered `SosConfiguration`/`SystemDefinition` but
/// with ITS OWN content changed after the hash was computed (rather than the hash field
/// itself flipped) is refused the same way -- the check compares content to hash, not merely
/// "did the hash field get edited."
#[test]
fn a_drm_whose_content_no_longer_matches_its_declared_hash_is_refused() {
    let _engine = gmat_sys::engine_lock();
    let (mut drm, sos, systems) = load_golden_bundle();
    drm.name = format!("{} (edited after hashing)", drm.name);

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-content-tampered".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::HashMismatch { artifact: "DesignReferenceMission", .. }), "{err:?}");
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

/// Required test 3: `covariance = true` against a `RelativisticCorrection` force model is
/// refused unless `accept_missing_stm_terms` (questions 82/83) -- built directly as `pb`
/// structs (rather than YAML) since this fixture only needs to exist in memory long enough to
/// be refused.
#[test]
fn covariance_with_relativistic_correction_is_refused_unless_accepted() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition {
        id: "rc_sys".to_string(),
        dynamics_model: "gmat.earth.relativistic".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            param("force_model.relativistic_correction", 1.0),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Keplerian"),
            param("spacecraft.SMA", 6878.0),
            param("spacecraft.ECC", 0.001),
            param("spacecraft.INC", 51.6),
        ],
        ..Default::default()
    });
    let sos = hashed_sos(SosConfiguration {
        id: "rc_sos".to_string(),
        instances: vec![SystemInstance {
            name: "rc".to_string(),
            system_id: "rc_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "rc_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "rc_drm".to_string(),
        sos_configuration_id: "rc_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_737_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, accept_missing_stm_terms: false, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-rc".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::MissingStmTermsNotAccepted { .. }), "{err:?}");
}

/// Required test (M18.4, `docs/open-questions.md` question 127): reusing an identical
/// `gmat.*`-bound `SystemInstance.name` across two independent `execute()` calls in the same
/// process must not collide, and the two runs' own `RunProducts` must be byte-identical.
///
/// **Why the identical `run_id` for both calls, not two different ones.** `Provenance.run_id`
/// (`RunConfig.run_id`) is copied straight into `RunProducts.provenance.run_id` --
/// `crate::drm::executor::build_run_provenance` -- so two calls with two different `run_id`s
/// would legitimately produce two different `RunProducts` even with a perfect fix, which would
/// make a byte-for-byte comparison meaningless (it would fail for a reason that has nothing to do
/// with this task). Passing the identical `run_id` both times is the fair, honest comparison: "the
/// same DRM, run twice" only means something once `run_id` itself is held fixed. This also
/// exercises the harder of the two collision shapes this task's own namespacing fix has to survive
/// -- see `crate::drm::executor::gmat_execution_namespace`'s own doc comment for why a bare
/// `run_id` is not enough to namespace GMAT's own object names when it repeats.
///
/// **Fails against:** the pre-M18.4 `binding::materialize_gmat`, which named every GMAT object it
/// constructed from `name_suffix` alone (e.g. `"DrmReplay_0Sat"`/`"DrmReplay_0FM"`, the same
/// string both times here, since neither the instance name nor the segment index differs between
/// the two calls) -- GMAT's configuration manager is process-global, so the second `execute()`
/// call's `Moderator::CreateSpacecraft`/`AddForce` failed against the first call's own
/// already-registered objects (observed in practice as "Attempted to add a GravityField force to
/// the force model for the body Earth, but there is already a GravityField force in place for
/// that body" -- see `tests/demo_two_instance.rs`'s own former `together_products` doc comment for
/// the original diagnosis). Against that implementation this test's second `execute(cfg2)` call
/// returns `Err(DrmError::Model(..))`, not `Ok(..)`, and `.expect(...)` panics.
#[test]
fn running_the_identical_drm_twice_in_one_process_produces_byte_identical_products() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition {
        id: "replay_sys".to_string(),
        dynamics_model: "gmat.earth.jgm2_8x8.sun_moon".to_string(),
        state_space_id: "gmat.orbital.cartesian6".to_string(),
        parameters: vec![
            sparam("force_model.central_body", "Earth"),
            sparam("force_model.gravity_file", "JGM2.cof"),
            param("force_model.gravity_degree", 8.0),
            param("force_model.gravity_order", 8.0),
            sparam("force_model.point_masses", "Luna,Sun"),
            sparam("spacecraft.CoordinateSystem", "EarthMJ2000Eq"),
            sparam("spacecraft.DisplayStateType", "Keplerian"),
            param("spacecraft.SMA", 6878.0),
            param("spacecraft.ECC", 0.001),
            param("spacecraft.INC", 51.6),
            param("spacecraft.RAAN", 30.0),
            param("spacecraft.AOP", 0.0),
            param("spacecraft.TA", 0.0),
            param("spacecraft.DryMass", 500.0),
            param("spacecraft.Cd", 2.2),
            param("spacecraft.Cr", 1.8),
            param("spacecraft.DragArea", 5.0),
            param("spacecraft.SRPArea", 5.0),
        ],
        ..Default::default()
    });
    let sos = hashed_sos(SosConfiguration {
        id: "replay_sos".to_string(),
        instances: vec![SystemInstance {
            name: "replay".to_string(),
            system_id: "replay_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "replay_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "replay_drm".to_string(),
        sos_configuration_id: "replay_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_637_000_000_000 + 100 * 1_000_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 10.0, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    // The identical run_id both times -- see this test's own doc comment for why.
    let run_id = "test-replay-same-run-id".to_string();
    let cfg1 = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default() , products_dir: None, replay: None };
    let products1 = execute(cfg1).expect("the first execute() call succeeds");
    let cfg2 = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: run_id.clone(), error_mode: Default::default() , products_dir: None, replay: None };
    let products2 = execute(cfg2).expect("the SECOND execute() call, reusing the identical gmat.*-bound instance name and run_id, must also succeed -- this is exactly the M18.4 fix");

    // Sanity: a real, non-vacuous run happened both times (at least one sample, one segment --
    // no faults/maneuvers declared here, so a byte-identical-but-empty pair of RunProducts could
    // not silently pass this as a false positive).
    let traj1 = products1.trajectories.get("replay").expect("first run produced a trajectory");
    assert!(!traj1.samples.is_empty(), "sanity: the first run actually propagated samples");
    assert_eq!(traj1.segments.len(), 1, "no faults declared: exactly one dynamics segment");

    // The claim this test exists to prove: byte-identical wire products, encoded the same way
    // `RunProducts::to_proto`/`prost::Message::encode_to_vec` already does for `/api/cdm/run`
    // (question 121) -- the strictest possible comparison, not merely "the same length" or "close
    // enough."
    let bytes1 = products1.to_proto().encode_to_vec();
    let bytes2 = products2.to_proto().encode_to_vec();
    assert_eq!(bytes1, bytes2, "running the identical DRM twice (same run_id) in one process must produce byte-identical RunProducts");
}

/// Required test 4: non-model bindings are refused with the typed error (here,
/// `BINDING_KIND_CONTAINER` -- `binding::classify_binding`'s own unit tests, run without any
/// GMAT dependency, additionally cover `BINDING_KIND_RENODE`/`BINDING_KIND_BOARD`/unset).
#[test]
fn a_renode_binding_is_refused_through_the_full_executor() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition { id: "ground_sys".to_string(), dynamics_model: "native.ground_software".to_string(), ..Default::default() });
    let sos = hashed_sos(SosConfiguration {
        id: "ground_sos".to_string(),
        instances: vec![SystemInstance {
            name: "ground".to_string(),
            system_id: "ground_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Renode as i32, config: Some(av_cdm::pb::binding::Config::Renode(av_cdm::pb::RenodeBinding::default())) }),
            step_rate_hz: 1.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "ground_drm".to_string(),
        sos_configuration_id: "ground_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 100_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-renode".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::UnsupportedBinding { .. }), "{err:?}");
}

/// `docs/open-questions.md` question 108: `SosConfiguration.connections` is validated
/// (`crate::router::Router::build`) as part of `execute`'s own load-time checks, before any
/// instance is even classified or bound -- an undeclared port is refused with the typed
/// `DrmError::Router` error, never discovered only once a run actually tries to exchange a
/// message. `av_kernel::router::Router::build`'s own unit tests
/// (`crates/av-kernel/src/router.rs`) cover the other three refusals (direction mismatch, kind
/// mismatch, unsupported link_model) directly against `Router::build`; this test proves the
/// wiring reaches the real `execute()` entry point end to end. Entirely GMAT-free, same pattern
/// as `a_renode_binding_is_refused_through_the_full_executor` above: the run is refused
/// before binding classification is even reached, so `RunConfig.gmat` is never actually
/// touched.
#[test]
fn a_connection_to_an_undeclared_port_is_refused_through_the_full_executor() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition { id: "port_sys".to_string(), dynamics_model: "native.constant_accel".to_string(), ..Default::default() });
    let sos = hashed_sos(SosConfiguration {
        id: "port_sos".to_string(),
        instances: vec![SystemInstance {
            name: "a".to_string(),
            system_id: "port_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "native.constant_accel".to_string() })) }),
            step_rate_hz: 1.0,
            ..Default::default()
        }],
        connections: vec![Connection { from_instance: "a".to_string(), from_port: "no_such_port".to_string(), to_instance: "a".to_string(), to_port: "also_missing".to_string(), link_model: String::new() }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "port_drm".to_string(),
        sos_configuration_id: "port_sos".to_string(),
        scenario: Some(Scenario { start_tai_ns: 0, end_tai_ns: 100_000_000, ..Default::default() }),
        options: Some(DrmOptions { default_step_rate_hz: 1.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-router".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let err = execute(cfg).unwrap_err();
    assert!(matches!(err, DrmError::Router(av_kernel::router::RouterError::UndeclaredPort { .. })), "{err:?}");
}

/// Not individually required, but exercises "What to build" items 4 and 5 together: a native
/// (non-GMAT) `"accel.x"` binding, run at 10 Hz for 2 s with one `FAULT_TARGET_KIND_DYNAMICS`
/// fault at the 1 s midpoint changing `accel.x` from 1.0 to 5.0 m/s^2 -- entirely GMAT-free
/// (no `state.px`.. state needs no unit crossing, and `"native.constant_accel"` does not
/// match the `"gmat."` dispatch prefix), so it runs even without a GMAT install, and it is the
/// only test in this file that actually exercises `fault::apply_dynamics_fault` /
/// `executor::run_span`'s segment-splitting loop end to end rather than at the unit level.
/// Checked against the closed-form constant-acceleration solution on each half independently
/// (not just "it ran without panicking"): `x(1s) = 0.5*1*1^2 = 0.5`, `vx(1s) = 1.0`, then
/// `x(2s) = x(1s) + vx(1s)*1 + 0.5*5*1^2 = 4.0`, `vx(2s) = vx(1s) + 5*1 = 6.0`.
#[test]
fn a_dynamics_fault_splits_the_run_into_two_segments_with_continuous_state() {
    let _engine = gmat_sys::engine_lock();
    let sys = hashed_system(SystemDefinition {
        id: "accel_sys".to_string(),
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
    });
    let sos = hashed_sos(SosConfiguration {
        id: "accel_sos".to_string(),
        instances: vec![SystemInstance {
            name: "veh".to_string(),
            system_id: "accel_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "accel_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "accel_drm".to_string(),
        sos_configuration_id: "accel_sos".to_string(),
        scenario: Some(Scenario {
            start_tai_ns: 0,
            end_tai_ns: 2_000_000_000,
            faults: vec![av_cdm::pb::Fault {
                id: "f1".to_string(),
                tai_ns: 1_000_000_000,
                target_kind: av_cdm::pb::FaultTargetKind::Dynamics as i32,
                instance: "veh".to_string(),
                target: "accel.x".to_string(),
                kind: "parameter".to_string(),
                params: BTreeMap::from([("value".to_string(), 5.0)]),
                ..Default::default()
            }],
            ..Default::default()
        }),
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-accel-fault".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("fault-split DRM executes end to end");

    let traj = products.trajectories.get("veh").expect("instance produced a trajectory");
    assert_eq!(traj.segments.len(), 2, "one fault -> two dynamics segments");
    assert_ne!(traj.segments[0].dynamics_hash, traj.segments[1].dynamics_hash, "the changed accel.x must change the settings hash");
    // 21 samples: 0, 100ms, .., 2000ms at 10 Hz, with the duplicate boundary sample at the
    // fault epoch (1000ms) dropped rather than emitted twice.
    assert_eq!(traj.samples.len(), 21);

    let midpoint = traj.samples.iter().find(|s| s.tai_ns == 1_000_000_000).expect("a sample at the fault epoch");
    assert!((midpoint.mean[0] - 0.5).abs() < 1e-9, "x(1s) = {}", midpoint.mean[0]);
    assert!((midpoint.mean[3] - 1.0).abs() < 1e-9, "vx(1s) = {}", midpoint.mean[3]);

    let last = traj.samples.last().unwrap();
    assert_eq!(last.tai_ns, 2_000_000_000);
    assert!((last.mean[0] - 4.0).abs() < 1e-9, "x(2s) = {}", last.mean[0]);
    assert!((last.mean[3] - 6.0).abs() < 1e-9, "vx(2s) = {}", last.mean[3]);
    // The fault must not have touched accel.y/accel.z or the other axes' continuity.
    for i in [1, 2, 4, 5] {
        assert!(last.mean[i].abs() < 1e-9, "component {i} should stay exactly 0: {}", last.mean[i]);
    }

    // RunProducts.events (question 95, M9.3): one EVENT_KIND_FAULT for the applied fault plus
    // the instance's own run_start/run_end EVENT_KIND_LIFECYCLE pair, sorted (epoch, id) so the
    // fault (at the 1 s midpoint) sits strictly between the two lifecycle events.
    assert_eq!(products.events.len(), 3, "{:?}", products.events);
    assert_eq!(products.events[0].name, "run_start");
    assert_eq!(products.events[0].tai_ns, 0);
    let fault_event = &products.events[1];
    assert_eq!(fault_event.name, "f1");
    assert_eq!(fault_event.reference_id, "f1");
    assert_eq!(fault_event.kind, av_cdm::pb::EventKind::Fault as i32);
    assert_eq!(fault_event.entity_id, "veh");
    assert_eq!(fault_event.tai_ns, 1_000_000_000);
    assert_eq!(fault_event.values.get("value"), Some(&5.0));
    assert_eq!(products.events[2].name, "run_end");
    assert_eq!(products.events[2].tai_ns, 2_000_000_000);
}

/// Question 95 (M9.3): `output.<instance>.<name>@time` resolves against a real GMAT-bound
/// instance's own derived output (`crate::expr::speed_output`, `|velocity|` computed directly
/// from the trajectory GMAT itself produced -- see that function's own doc comment for why this,
/// not `av_dynamics::StepResult.outputs` or a GMAT `GetRealParameter`-style calculated field, is
/// what this crate can honestly produce). Declared as a `MeasureOfEffectiveness` on a real
/// `DesignReferenceMission`, evaluated by `execute()`'s own scoring pass into `RunProducts.
/// scores` (not just a caller-derived `ExprRunProducts`), and checked against `|velocity|`
/// recomputed independently, directly from the same trajectory's own `vel_x`/`vel_y`/`vel_z`
/// samples -- proving the value flowing through `output.*` really is GMAT's own propagated
/// velocity, not a placeholder. Uses `leo_sys` (the golden bundle's real `"gmat."`-dispatched
/// system definition) over a short, single-output-period span, the same fast-path pattern
/// `short_covariance_drm` already uses, and its own distinct instance name (`"leo_out"`) per the
/// module doc comment's "GMAT object naming" section.
///
/// **`@start`, restored (M10.3, `docs/open-questions.md` question 96).** Through M9.3 this test
/// asserted `@end`, not `@start`: the binding reconciled a GMAT-bound instance's epoch through
/// an A1MJD `f64` round trip (`gmat_sys::model::GmatModel::epoch_tai_ns`), which could miss the
/// declared `Scenario.start_tai_ns` by up to ~124 ns (question 81) -- landing `@start`'s
/// resolved `tai_ns` a hair off the run's own first sample, at exactly the point in the
/// trajectory the interpolation window is at its narrowest (only one native step wide). `@end`
/// dodged this by construction, not by coincidence. Question 96 deletes that round trip:
/// `crate::registry::ModelRegistry::construct_gmat`'s `ModelHandle::t0_tai_ns` is now always the
/// caller's own exact `epoch_tai_ns`, so the run's own first sample lands exactly on
/// `Scenario.start_tai_ns` by construction (asserted directly below) -- there is nothing left to
/// dodge, and this test goes back to `@start`, its original M9.3 form.
#[test]
fn output_speed_resolves_against_a_real_gmat_bound_instance() {
    let _engine = gmat_sys::engine_lock();
    let (_, _, systems) = load_golden_bundle();

    let sos = hashed_sos(SosConfiguration {
        id: "leo_sos_output".to_string(),
        instances: vec![SystemInstance {
            name: "leo_out".to_string(),
            system_id: "leo_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm = hashed_drm(DesignReferenceMission {
        id: "leo_drm_output".to_string(),
        sos_configuration_id: "leo_sos_output".to_string(),
        // Same epoch `leo_1day_golden.drm.yaml` uses (its own comment derives it); only 0.1 s
        // long, one exact sample_interval_s period -- fast, not the golden's full one-day arc.
        scenario: Some(Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_637_100_000_000, ..Default::default() }),
        measures: vec![av_cdm::pb::MeasureOfEffectiveness { name: "speed_at_start".to_string(), expression: "output.leo_out.speed@start".to_string(), unit: av_cdm::pb::Unit::MeterPerSecond as i32 }],
        options: Some(DrmOptions { default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..Default::default() }),
        ..Default::default()
    });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: "test-run-output-speed".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products = execute(cfg).expect("DRM executes end to end");

    let traj = products.trajectories.get("leo_out").expect("instance produced a trajectory");
    assert_eq!(traj.samples.len(), 2, "one exact sample_interval_s period: exactly 2 samples");
    let speed = |s: &av_cdm::pb::TrajectorySample| (s.mean[3].powi(2) + s.mean[4].powi(2) + s.mean[5].powi(2)).sqrt();
    let (s0, s1) = (&traj.samples[0], &traj.samples[1]);
    let scenario_start = drm.scenario.as_ref().unwrap().start_tai_ns;
    // Question 96's direct consequence: the run's own first sample lands exactly on the
    // declared epoch now -- no residual left over from an A1MJD round trip.
    assert_eq!(s0.tai_ns, scenario_start, "the first sample's own epoch must be exactly Scenario.start_tai_ns (question 96: no A1MJD round trip left to introduce a residual)");
    // `@start`/`@end` resolve through the same `output_at`/`entity_component_at` linear
    // interpolation any other `@time` does; computed the general way here (not just read off
    // `s0` verbatim) so this assertion is not hiding that mechanism even though `frac` is
    // exactly 0 given the equality just asserted above.
    let frac = (scenario_start - s0.tai_ns) as f64 / (s1.tai_ns - s0.tai_ns) as f64;
    let expected_speed = speed(s0) + (speed(s1) - speed(s0)) * frac;

    let score = &products.scores["speed_at_start"];
    assert!(
        (score.value - expected_speed).abs() < 1e-9,
        "output.leo_out.speed@start = {} does not match |velocity| = {expected_speed} recomputed independently from the same GMAT-produced trajectory",
        score.value
    );
    assert!(score.value > 1000.0, "a LEO orbital speed should be several km/s, got {} m/s -- sanity check this is really GMAT's own velocity, not a placeholder", score.value);
    assert_eq!(score.passed, None, "a MeasureOfEffectiveness never carries pass/fail");
}

/// Question 101 (M11.2): "product sets must not depend on the run mode." Two DRMs over the same
/// `leo_sys` `SystemDefinition` (each declaring the same `"output.rmag"`/`"output.cd"`
/// parameters -- see `OUTPUT_PARAMETER_PREFIX`'s doc comment in `executor.rs`), one run through
/// `run_plain_instance` (`options.covariance = false`) and one through `run_covariance_instance`
/// (`options.covariance = true`, seeded from the golden's own P0) -- otherwise identical scenario
/// window, step rate, and declared measures.
///
/// **What "same product set" means here, concretely.** `RunProducts` has no field directly
/// exposing a instance's raw named-output `BTreeMap` (`executor::NamedOutputSeries` is a private
/// implementation detail) -- the one way a caller can observe it through the public `execute()`
/// API is by referencing `output.<instance>.<name>@time` in a declared `MeasureOfEffectiveness`
/// and checking the result actually scores rather than failing. `crate::expr::evaluate_moe` only
/// resolves that reference against a series `execute()`'s own `outputs_by_instance` map actually
/// attached for that instance (`executor.rs`'s pass-2 loop, unchanged by this task) -- attached
/// only when the run's own `HeteroKernel::outputs` reported a non-empty series under that exact
/// name. So: **before this task, the covariance-side `execute()` call below would return
/// `DrmError::InvalidExpression` (`ExprError::UnknownOutput`) for both measures** (`
/// StmAugmented::step` never called the wrapped model's `step_with_stm`, so `kernel.outputs`
/// on the covariance path was always empty -- see `run_covariance_instance`'s own doc comment)
/// -- both `execute()` calls succeeding with every declared measure scored, checked by name
/// below, *is* the "identical product name sets" proof, not merely "both runs completed".
///
/// **Values at matching epochs.** `output.<instance>.cd@end` is a static spacecraft property
/// (`Cd`, set once from `SystemDefinition`'s own `"spacecraft.Cd"` parameter and never touched by
/// either integration scheme), so it must agree between the two paths to floating-point noise,
/// not merely "close" -- an independent check that the covariance path's `step_with_stm` override
/// is reading the same real object, not a coincidentally-similar stand-in. `output.<instance>
/// .rmag@end` is derived from two different (but both `rtol = atol = 1e-12`) numerical
/// integrations of the *same* short arc -- the plain path's 6-state `Dopri5` integration vs. the
/// covariance path's 42-state, per-native-period-reseeded one (`av_dynamics::StmAugmented::step`'s
/// own doc comment) -- so it is compared at a tight-but-not-golden-tolerance bound, measured
/// below, not assumed.
#[test]
fn covariance_and_plain_paths_produce_the_same_named_output_set() {
    let _engine = gmat_sys::engine_lock();
    let (golden_drm, golden_sos, systems) = load_golden_bundle();
    let p0 = golden_sos.instances[0].initial_covariance.clone();
    assert_eq!(p0.len(), 36, "drms/leo_1day_golden.sos.yaml's \"leo\" instance must carry the golden's 6x6 initial_covariance");
    let golden_options = golden_drm.options.expect("golden DRM has options");

    let mut sys = systems.get("leo_sys").expect("golden bundle declares leo_sys").clone();
    sys.id = "leo_outputs_sys".to_string();
    sys.parameters.push(Parameter { name: "output.rmag".to_string(), unit: Unit::Meter as i32, ..Default::default() });
    sys.parameters.push(Parameter { name: "output.cd".to_string(), unit: Unit::Dimensionless as i32, ..Default::default() });
    let sys = hashed_system(sys);
    let mut systems_map = BTreeMap::new();
    systems_map.insert(sys.id.clone(), sys);

    // Same scenario window, step rate, and output declarations for both paths -- only
    // `DrmOptions.covariance` (and, necessarily, the instance name and initial_covariance,
    // required only on the covariance side) differ. One output period (0.1 s at 10 Hz): fast,
    // same convention `short_covariance_drm` already uses.
    let scenario = Scenario { start_tai_ns: 1_767_225_637_000_000_000, end_tai_ns: 1_767_225_637_100_000_000, ..Default::default() };

    fn measures_for(instance: &str) -> Vec<av_cdm::pb::MeasureOfEffectiveness> {
        vec![
            av_cdm::pb::MeasureOfEffectiveness { name: "rmag_at_end".to_string(), expression: format!("output.{instance}.rmag@end"), unit: Unit::Meter as i32 },
            av_cdm::pb::MeasureOfEffectiveness { name: "cd_at_end".to_string(), expression: format!("output.{instance}.cd@end"), unit: Unit::Dimensionless as i32 },
        ]
    }

    let sos_plain = hashed_sos(SosConfiguration {
        id: "leo_sos_outputs_plain".to_string(),
        instances: vec![SystemInstance {
            name: "leo_outputs_plain".to_string(),
            system_id: "leo_outputs_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_outputs_sys".to_string() })) }),
            step_rate_hz: 10.0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm_plain = hashed_drm(DesignReferenceMission {
        id: "leo_drm_outputs_plain".to_string(),
        sos_configuration_id: "leo_sos_outputs_plain".to_string(),
        scenario: Some(scenario.clone()),
        measures: measures_for("leo_outputs_plain"),
        options: Some(DrmOptions { covariance: false, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..golden_options }),
        ..Default::default()
    });

    let sos_cov = hashed_sos(SosConfiguration {
        id: "leo_sos_outputs_cov".to_string(),
        instances: vec![SystemInstance {
            name: "leo_outputs_cov".to_string(),
            system_id: "leo_outputs_sys".to_string(),
            binding: Some(Binding { kind: BindingKind::Model as i32, config: Some(av_cdm::pb::binding::Config::Model(ModelBinding { model_id: "leo_outputs_sys".to_string() })) }),
            step_rate_hz: 10.0,
            initial_covariance: p0,
            ..Default::default()
        }],
        ..Default::default()
    });
    let drm_cov = hashed_drm(DesignReferenceMission {
        id: "leo_drm_outputs_cov".to_string(),
        sos_configuration_id: "leo_sos_outputs_cov".to_string(),
        scenario: Some(scenario),
        measures: measures_for("leo_outputs_cov"),
        options: Some(DrmOptions { covariance: true, default_step_rate_hz: 10.0, sample_interval_s: 0.1, ..golden_options }),
        ..Default::default()
    });

    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");

    let cfg_plain = RunConfig { gmat: &gmat, drm: &drm_plain, sos: &sos_plain, systems: &systems_map, run_id: "test-run-outputs-plain".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products_plain = execute(cfg_plain).expect("plain-path DRM executes end to end and scores every declared output measure -- if this fails with InvalidExpression, the plain path's own output set regressed");

    let cfg_cov = RunConfig { gmat: &gmat, drm: &drm_cov, sos: &sos_cov, systems: &systems_map, run_id: "test-run-outputs-cov".to_string(), error_mode: Default::default() , products_dir: None, replay: None };
    let products_cov = execute(cfg_cov).expect(
        "covariance-path DRM executes end to end and scores every declared output measure -- if this fails with InvalidExpression, the covariance path is not carrying StepResult.outputs through (question 101)",
    );

    // Both paths produced a score under exactly the same measure names -- the "same product
    // name set" proof (see this test's own doc comment for why this is what's actually
    // observable through the public API).
    let plain_names: std::collections::BTreeSet<&String> = products_plain.scores.keys().collect();
    let cov_names: std::collections::BTreeSet<&String> = products_cov.scores.keys().collect();
    assert_eq!(plain_names, cov_names, "the plain and covariance paths must score the identical set of declared output names");
    assert_eq!(plain_names, std::collections::BTreeSet::from([&"rmag_at_end".to_string(), &"cd_at_end".to_string()]));

    let rmag_plain = products_plain.scores["rmag_at_end"].value;
    let rmag_cov = products_cov.scores["rmag_at_end"].value;
    let cd_plain = products_plain.scores["cd_at_end"].value;
    let cd_cov = products_cov.scores["cd_at_end"].value;
    let rmag_err = (rmag_plain - rmag_cov).abs();
    let cd_err = (cd_plain - cd_cov).abs();
    eprintln!(
        "[drm_executor outputs] plain vs covariance over one 0.1 s output period: rmag {rmag_plain:.6} m vs {rmag_cov:.6} m (|err| {rmag_err:.6e} m); cd {cd_plain} vs {cd_cov} (|err| {cd_err:.3e})"
    );
    // Cd is a static spacecraft property untouched by either integration scheme -- must agree
    // to floating-point noise, not merely "close".
    assert!(cd_err < 1e-9, "output.cd disagrees between the plain and covariance paths by {cd_err} -- both should read the identical static Cd real parameter");
    assert_eq!(cd_plain, 2.2, "leo_1day_golden.system.yaml declares spacecraft.Cd = 2.2");
    // Rmag comes from two different (6-state vs. 42-state-reseeded) but both rtol=atol=1e-12
    // numerical integrations of the same 0.1 s arc -- a tight bound, not the golden's own
    // sub-millimetre one, since the two schemes are not required to be bit-identical (see
    // av_dynamics::StmAugmented::step's own doc comment), but this is a single short native
    // step over a ~6900 km-radius orbit, so any real regression would show up as metres, not
    // micrometres.
    assert!(rmag_err < 1e-3, "output.rmag disagrees between the plain and covariance paths by {rmag_err} m over a single 0.1 s native step, more than the two integration schemes' own numerical noise floor should allow");
}

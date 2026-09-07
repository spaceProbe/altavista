//! M22.2 (`docs/sil-plan.md`'s M22 milestone paragraph and its Decisions (2026-09-05) decision
//! A; `docs/open-questions.md` questions 142/149/151/152): validates the
//! `drms/demo_attitude_sensors*.yaml` fixture set the task brief asks for ("a DRM fixture under
//! `drms/` in which an attitude instance is measured by both sensors over a FRAMED port").
//!
//! **What this test proves (load-time only -- see `crates/av-kernel/tests/drm_attitude_sensors.
//! rs` for the `execute()`-driven proof M22.2b adds on top):** every artifact parses
//! (`crate::drm::schema`), every declared `hash` field matches that artifact's own canonical
//! hash (ADR-001: nothing exists only by convention, least of all an unhashed fixture), every
//! declared `PacketCodec` is independently valid (`crate::codec::validate_codec`), and the
//! fourteen declared `Connection`s actually build a real `crate::router::Router` against the
//! three declared `SystemDefinition`s' own `Port`s (the same port-kind/direction/existence
//! checks a real DRM run would apply, question 108). Through M22.2 this file's own doc comment
//! disclosed that this fixture did not yet run through `av_kernel::drm::execute` at all (the
//! `"startracker."`/`"imu."` `dynamics_model` prefixes were not yet wired into
//! `crate::registry::kind_for`); M22.2b (`docs/open-questions.md` questions 142/149/151/152)
//! closes that gap -- `crates/av-kernel/tests/drm_attitude_sensors.rs` is the real,
//! `av_kernel::drm::execute`-driven proof of this exact three-instance topology (declared update
//! rate honoured through the full executor path, same-seed/different-seed determinism, both
//! sensors' own maneuver/covariance refusal semantics). The tests in *this* file remain valid
//! and are kept unchanged -- they check something the execute()-driven tests do not duplicate
//! (that the artifacts are independently well-formed at load time, before any binding is
//! attempted) -- plus the pre-existing router-level unit proof,
//! `av_kernel::drm::sensors::tests::attitude_instance_is_measured_by_both_sensors_through_the_
//! real_router` (a `#[cfg(test)]` unit test inside that module, since it needs `pub(crate)`
//! access to `crate::drm::attitude`'s own `AttitudeWheelsModel`/`parse_attitude_spec`), which
//! this wiring did not make stale either.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{SosConfiguration, SystemDefinition};
use av_kernel::codec;
use av_kernel::drm::{hash, schema};
use av_kernel::router::Router;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

/// Every artifact in the fixture set parses, and its declared `hash` matches its own canonical
/// hash -- fails against a fixture whose YAML was hand-edited after the hash was computed (a
/// stale/wrong hash), exactly the tamper/staleness check `av_kernel::drm::execute` itself runs
/// at load time (this test runs the identical `hash::canonical_*_hash` functions directly,
/// without needing a `Gmat` handle or `execute` itself).
#[test]
fn every_artifact_in_the_fixture_set_parses_and_its_declared_hash_matches_its_canonical_hash() {
    let truth = load_system("demo_attitude_sensors_truth");
    assert_eq!(truth.hash, hash::canonical_system_hash(&truth), "demo_attitude_sensors_truth.system.yaml: declared hash is stale");

    let star = load_system("demo_attitude_sensors_startracker");
    assert_eq!(star.hash, hash::canonical_system_hash(&star), "demo_attitude_sensors_startracker.system.yaml: declared hash is stale");

    let imu = load_system("demo_attitude_sensors_imu");
    assert_eq!(imu.hash, hash::canonical_system_hash(&imu), "demo_attitude_sensors_imu.system.yaml: declared hash is stale");

    let sos = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("sos parses");
    assert_eq!(sos.hash, hash::canonical_sos_hash(&sos), "demo_attitude_sensors.sos.yaml: declared hash is stale");

    let drm = schema::parse_drm_yaml(&read("demo_attitude_sensors.drm.yaml")).expect("drm parses");
    assert_eq!(drm.hash, hash::canonical_drm_hash(&drm), "demo_attitude_sensors.drm.yaml: declared hash is stale");
}

/// Both sensor systems' own declared `PacketCodec` (the FRAMED CCSDS output the brief requires)
/// is independently valid -- fails against a codec declaring a field extent past
/// `user_data_bytes`, an unspecified field type, or any other `crate::codec::validate_codec`
/// refusal.
#[test]
fn both_sensor_system_packet_codecs_are_independently_valid() {
    let star = load_system("demo_attitude_sensors_startracker");
    assert_eq!(star.packet_codecs.len(), 1);
    codec::validate_codec(&star.packet_codecs[0]).expect("star tracker codec is valid");
    assert_eq!(star.packet_codecs[0].apid, 100);

    let imu = load_system("demo_attitude_sensors_imu");
    assert_eq!(imu.packet_codecs.len(), 1);
    codec::validate_codec(&imu.packet_codecs[0]).expect("imu codec is valid");
    assert_eq!(imu.packet_codecs[0].apid, 101);
}

/// The declared `Connection`s actually build a real `crate::router::Router` against the three
/// declared `SystemDefinition`s' own `Port`s -- fails against a fixture naming an undeclared
/// port, a direction mismatch (e.g. two `OUT` ports connected to each other), or a `PortKind`
/// mismatch (question 108's own load-time checks, exercised here for real against this exact
/// fixture rather than only against `crate::router`'s own synthetic test fixtures).
#[test]
fn the_declared_connections_build_a_real_router_against_the_declared_ports() {
    let truth = load_system("demo_attitude_sensors_truth");
    let star = load_system("demo_attitude_sensors_startracker");
    let imu = load_system("demo_attitude_sensors_imu");
    let sos: SosConfiguration = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("sos parses");

    assert_eq!(sos.instances.len(), 3);
    assert_eq!(sos.connections.len(), 14, "seven truth ports x two sensor instances");

    let mut systems = BTreeMap::new();
    systems.insert(truth.id.clone(), truth);
    systems.insert(star.id.clone(), star);
    systems.insert(imu.id.clone(), imu);

    let router = Router::build(&sos, &systems).expect("the declared ports/connections are well-formed");
    assert!(!router.has_pending(), "a freshly built router has nothing queued yet");
}

/// The declared `Connection`s name exactly `crate::drm::sensors::TRUTH_PORT_NAMES`, in both
/// directions (attitude -> startracker, attitude -> imu) -- fails against a fixture that
/// silently drifted from that module's own fixed port-name convention (e.g. a typo'd port name
/// that would otherwise still "build" a valid but disconnected router).
#[test]
fn every_connection_names_one_of_the_seven_declared_truth_ports() {
    let sos: SosConfiguration = schema::parse_sos_yaml(&read("demo_attitude_sensors.sos.yaml")).expect("sos parses");
    let mut to_startracker: Vec<&str> = Vec::new();
    let mut to_imu: Vec<&str> = Vec::new();
    for c in &sos.connections {
        assert_eq!(c.from_instance, "attitude");
        assert!(av_kernel::drm::sensors::TRUTH_PORT_NAMES.contains(&c.from_port.as_str()), "unexpected from_port {:?}", c.from_port);
        assert_eq!(c.from_port, c.to_port, "this fixture's own convention: truth port names are identical on both ends");
        match c.to_instance.as_str() {
            "startracker" => to_startracker.push(c.to_port.as_str()),
            "imu" => to_imu.push(c.to_port.as_str()),
            other => panic!("unexpected to_instance {other:?}"),
        }
    }
    to_startracker.sort_unstable();
    to_imu.sort_unstable();
    let mut expected: Vec<&str> = av_kernel::drm::sensors::TRUTH_PORT_NAMES.to_vec();
    expected.sort_unstable();
    assert_eq!(to_startracker, expected);
    assert_eq!(to_imu, expected);
}

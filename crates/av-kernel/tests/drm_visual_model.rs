//! Question 239: `SystemInstance.visual_model_uri` (`proto/altavista/v1/system.proto` field 9) is
//! the DRM's declaration, per instance, of the glTF model the viewer draws for the entity the
//! instance embodies; the executor copies it unchanged onto the `Trajectory.visual_model_uri`
//! (`trajectory.proto` field 12) it emits for that instance.
//!
//! These tests load the real bundle files from `drms/` through the same YAML parsers `av-run`
//! uses (so the YAML key, the hash check and the executor are all on the path):
//!
//! * `leo_1day_orbital_native_model.{drm,sos}.yaml` declares the model on its one instance;
//! * `leo_1day_orbital_native.{drm,sos}.yaml`, the same bundle with no declaration, must keep
//!   every canonical hash it had before the field existed (proto3 omits an empty string, so the
//!   encoding, and so the hash, is unchanged) and must emit an empty `visual_model_uri`.
//!
//! Both bundles use only the native `"orbital."` model, so this file builds and runs in either
//! feature state (no `required-features` entry), like `orbital_no_gmat_demo.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use av_cdm::pb::{DesignReferenceMission, SosConfiguration, SystemDefinition};
use av_kernel::drm::schema::{parse_drm_yaml, parse_sos_yaml, parse_system_definition_yaml};
use av_kernel::drm::{execute, hash, RunConfig};

const DECLARED_URI: &str = "/js/fixtures/entity_model_fixture.gltf";

/// The hashes `drms/leo_1day_orbital_native.{drm,sos,system}.yaml` declared BEFORE this field
/// existed (round 237 / `drms/README.md`); they must not move.
const PRE_CHANGE_NATIVE_DRM_HASH: &str = "fe27db743c73fb74099b221da7d1d9ed09368d7e0b69ea053af1fe384bd80d90";
const PRE_CHANGE_NATIVE_SOS_HASH: &str = "d7ca6b5e0fedb943ba0287812cff87f7ac9104472762dba32e005ada2568e1b9";
const PRE_CHANGE_NATIVE_SYSTEM_HASH: &str = "b99e5135132eda7594fc54fc93dad6dde97ede3a888d638554a3a709fcd82db6";

fn drms_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms")
}

fn read(name: &str) -> String {
    let path = drms_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

struct Bundle {
    drm: DesignReferenceMission,
    sos: SosConfiguration,
    systems: BTreeMap<String, SystemDefinition>,
}

fn load(stem: &str) -> Bundle {
    let drm = parse_drm_yaml(&read(&format!("{stem}.drm.yaml"))).unwrap_or_else(|e| panic!("{stem}.drm.yaml: {e:?}"));
    let sos = parse_sos_yaml(&read(&format!("{stem}.sos.yaml"))).unwrap_or_else(|e| panic!("{stem}.sos.yaml: {e:?}"));
    // Both bundles bind to the one native system file.
    let sys = parse_system_definition_yaml(&read("leo_1day_orbital_native.system.yaml")).expect("the native system file parses");
    let mut systems = BTreeMap::new();
    systems.insert(sys.id.clone(), sys);
    Bundle { drm, sos, systems }
}

fn run(bundle: &Bundle, run_id: &str) -> av_kernel::drm::RunProducts {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    let cfg = RunConfig {
        #[cfg(feature = "gmat")]
        gmat: &gmat,
        drm: &bundle.drm,
        sos: &bundle.sos,
        systems: &bundle.systems,
        run_id: run_id.to_string(),
        error_mode: Default::default(),
        products_dir: None,
        replay: None,
        command_source: None,
    };
    execute(cfg).expect("the native bundle executes")
}

#[test]
fn a_declared_visual_model_uri_reaches_the_emitted_trajectory_unchanged() {
    let bundle = load("leo_1day_orbital_native_model");
    assert_eq!(bundle.sos.instances.len(), 1);
    assert_eq!(bundle.sos.instances[0].visual_model_uri, DECLARED_URI, "the YAML key parses onto the proto field");
    let products = run(&bundle, "test-visual-model-declared");
    assert_eq!(products.trajectories.len(), 1);
    let traj = products.trajectories.get("leo").expect("the declared instance emits a trajectory");
    assert_eq!(traj.visual_model_uri, DECLARED_URI, "the executor copies the instance's declaration onto its trajectory");
    assert!(!traj.samples.is_empty(), "a real trajectory, not an empty shell");
}

#[test]
fn an_undeclared_instance_emits_an_empty_visual_model_uri_and_keeps_its_hashes() {
    let bundle = load("leo_1day_orbital_native");
    assert_eq!(bundle.sos.instances[0].visual_model_uri, "");
    // The pinned hashes are the ones the files declared before the field existed; the executor
    // re-verifies them on every run, and this asserts them against the literals above too.
    assert_eq!(bundle.drm.hash, PRE_CHANGE_NATIVE_DRM_HASH);
    assert_eq!(bundle.sos.hash, PRE_CHANGE_NATIVE_SOS_HASH);
    assert_eq!(hash::canonical_sos_hash(&bundle.sos), PRE_CHANGE_NATIVE_SOS_HASH);
    assert_eq!(hash::canonical_drm_hash(&bundle.drm), PRE_CHANGE_NATIVE_DRM_HASH);
    let sys = bundle.systems.values().next().expect("one system");
    assert_eq!(hash::canonical_system_hash(sys), PRE_CHANGE_NATIVE_SYSTEM_HASH);

    let products = run(&bundle, "test-visual-model-undeclared");
    let traj = products.trajectories.get("leo").expect("the instance emits a trajectory");
    assert_eq!(traj.visual_model_uri, "", "no declaration, no model");
}

#[test]
fn the_declaration_is_configuration_and_enters_the_sos_hash_only_when_set() {
    let bundle = load("leo_1day_orbital_native");
    let base = hash::canonical_sos_hash(&bundle.sos);
    let mut declared = bundle.sos.clone();
    declared.instances[0].visual_model_uri = DECLARED_URI.to_string();
    let with = hash::canonical_sos_hash(&declared);
    assert_ne!(with, base, "a declared model is configuration: it must change the hash");
    declared.instances[0].visual_model_uri.clear();
    assert_eq!(hash::canonical_sos_hash(&declared), base, "clearing it restores the pre-field encoding exactly");
}

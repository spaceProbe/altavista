//! The `av-sweep` binary against the file study store's immutability (question 252): a study is
//! never replaced under its `sweep_id`. Identical content is an idempotent success; different
//! content is a typed refusal that reaches the user as a message and a non-zero exit (never a
//! panic), with the stored study untouched.
//!
//! - `--import-study` (store mode) needs no GMAT.
//! - The study-mode rerun test runs `drms/leo_orbital_native_sweep.*` (12 pure-Rust samples), once
//!   and again into the same `--store-dir`: with the same `--out-dir` the record is byte-identical,
//!   with another `--out-dir` it differs (every sample's `products_uri` is the canonical path of its
//!   sample directory), which is exactly the case that used to silently overwrite the store.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use av_cdm::pb;
use prost::Message;

fn av_sweep_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_av-sweep"))
}

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-sweep-file-store-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The study directory's files, name to bytes.
fn study_bytes(store_dir: &Path, id: &str) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(store_dir.join(id))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn assert_clean_refusal(output: &Output, id: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a refused write must exit non-zero: {stderr}");
    assert!(output.status.code().is_some(), "exited normally, not by a signal: {:?}", output.status);
    assert!(!stderr.contains("panicked"), "a typed error, not a panic: {stderr}");
    assert!(stderr.contains(&format!("study {id:?}")) && stderr.contains("write refused") && stderr.contains("studies are immutable"), "{stderr}");
}

fn study(id: &str, products_uri: &str) -> pb::SweepResults {
    pb::SweepResults {
        sweep_id: id.to_string(),
        sweep_hash: "sweep-hash".to_string(),
        drm_hash: "drm-hash".to_string(),
        samples: vec![pb::SweepSample { run_id: "r".to_string(), products_uri: products_uri.to_string(), ..Default::default() }],
        ..Default::default()
    }
}

fn import(pb_path: &Path, store_dir: &Path) -> Output {
    Command::new(av_sweep_bin()).arg("--import-study").arg(pb_path).arg("--store-dir").arg(store_dir).output().expect("spawning av-sweep")
}

#[test]
fn importing_the_same_study_twice_is_ok_and_a_different_one_under_its_id_is_a_typed_refusal() {
    let dir = scratch("import");
    let store = dir.join("store");
    let (first, second) = (dir.join("first.pb"), dir.join("second.pb"));
    std::fs::write(&first, study("imp", "/out-a/sample_p0_d0").encode_to_vec()).unwrap();
    std::fs::write(&second, study("imp", "/out-b/sample_p0_d0").encode_to_vec()).unwrap();

    let out = import(&first, &store);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stored = study_bytes(&store, "imp");
    assert_eq!(stored.len(), 3);

    let out = import(&first, &store);
    assert!(out.status.success(), "identical content is idempotent: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(study_bytes(&store, "imp"), stored);

    let out = import(&second, &store);
    assert_clean_refusal(&out, "imp");
    assert_eq!(study_bytes(&store, "imp"), stored, "the stored study is unchanged, byte for byte");
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_native_study(out_dir: &Path, store_dir: &Path) -> Output {
    Command::new(av_sweep_bin())
        .arg("--sweep")
        .arg(drms_path("leo_orbital_native_sweep.sweep.yaml"))
        .arg("--drm")
        .arg(drms_path("leo_orbital_native_sweep.drm.yaml"))
        .arg("--sos")
        .arg(drms_path("leo_orbital_native_sweep.sos.yaml"))
        .arg("--system")
        .arg(drms_path("leo_1day_orbital_native.system.yaml"))
        .arg("--out-dir")
        .arg(out_dir)
        .arg("--workers")
        .arg("2")
        .arg("--store-dir")
        .arg(store_dir)
        .output()
        .expect("spawning av-sweep study mode")
}

#[test]
fn rerunning_a_study_into_one_store_is_ok_with_the_same_out_dir_and_refused_with_another() {
    let dir = scratch("rerun");
    let store = dir.join("store");
    let (out_a, out_b) = (dir.join("out-a"), dir.join("out-b"));
    let id = "leo_orbital_native_sweep";

    let first = run_native_study(&out_a, &store);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(String::from_utf8_lossy(&first.stderr).contains("12 sample(s), 0 failed"), "{}", String::from_utf8_lossy(&first.stderr));
    let stored = study_bytes(&store, id);
    assert_eq!(stored.len(), 3);

    // The same sweep again into the same --out-dir reproduces the record byte for byte.
    let again = run_native_study(&out_a, &store);
    assert!(again.status.success(), "the same out-dir reproduces the same study: {}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(study_bytes(&store, id), stored);

    // Another --out-dir changes every sample's products_uri: different content, refused.
    let elsewhere = run_native_study(&out_b, &store);
    assert_clean_refusal(&elsewhere, id);
    assert_eq!(study_bytes(&store, id), stored, "the stored study is unchanged, byte for byte");
    assert!(out_b.join("sweep_results.pb").is_file(), "the study itself finished and is in its --out-dir");
    let _ = std::fs::remove_dir_all(&dir);
}

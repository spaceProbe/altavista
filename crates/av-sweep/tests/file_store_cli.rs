//! The `av-sweep` binary against the file study store's immutability (question 252): a study is
//! never replaced under its `sweep_id`. Identical content is an idempotent success; different
//! content is a typed refusal that reaches the user as a message and a non-zero exit (never a
//! panic), with the stored study untouched.
//!
//! - `--import-study` (store mode) needs no GMAT.
//! - The study-mode rerun test runs `drms/leo_orbital_native_sweep.*` (12 pure-Rust samples), once
//!   and again into the same `--store-dir`: with the same `--out-dir` the record is byte-identical,
//!   with another `--out-dir` it differs (every sample's `products_uri` is the canonical path of its
//!   sample directory), which is exactly the case that used to silently overwrite the store. Since
//!   question 254 item 2 the second case is refused before the first sample runs.

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

    // Another --out-dir changes every sample's products_uri: a study the stored one cannot be
    // reproduced by. Since question 254 item 2 that is refused up front, before the first sample
    // runs: no sample directory and no sweep_results.pb in the new --out-dir.
    let elsewhere = run_native_study(&out_b, &store);
    assert_eq!(tree(&out_b).into_keys().collect::<Vec<_>>(), Vec::<String>::new(), "--out-dir exists and is empty: no sample ran, no sweep_results.pb was written");
    assert_clean_refusal(&elsewhere, id);
    let stderr = String::from_utf8_lossy(&elsewhere.stderr);
    assert!(stderr.contains("refuses this study up front") && stderr.contains("no sample was run") && stderr.contains("samples[p0_d0].products_uri"), "{stderr}");
    assert_eq!(study_bytes(&store, id), stored, "the stored study is unchanged, byte for byte");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- question 254 item 1: an id that becomes a path is one path component -----------------------

/// Every file and directory under `dir` (relative path and size).
fn tree(dir: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            let rel = entry.path().strip_prefix(base).unwrap().display().to_string();
            if meta.is_dir() {
                out.insert(rel, "dir".to_string());
                walk(base, &entry.path(), out);
            } else {
                out.insert(rel, format!("{} bytes", meta.len()));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn assert_refused_as_an_id(output: &Output, what: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{what}: must exit non-zero: {stderr}");
    assert!(output.status.code().is_some(), "{what}: exited normally, not by a signal");
    assert!(!stderr.contains("panicked"), "{what}: a typed error, not a panic: {stderr}");
}

/// `--import-study` of a file whose decoded `sweep_id` is a traversal, and `--read-study` of one,
/// through the real binary: refused, non-zero, and the tree around the store is untouched.
#[test]
fn a_traversal_sweep_id_is_refused_by_import_and_read_and_nothing_is_written() {
    let dir = scratch("traversal");
    let store = dir.join("store");
    let valid = dir.join("valid.pb");
    std::fs::write(&valid, study("good", "/out/sample_p0_d0").encode_to_vec()).unwrap();
    assert!(import(&valid, &store).status.success());

    let ids = ["../x".to_string(), "..".to_string(), dir.join("x").display().to_string(), "a/b".to_string(), "a\\b".to_string(), "a\0b".to_string(), "%2e%2e%2fx".to_string(), "..%2fx".to_string()];
    for (n, id) in ids.iter().enumerate() {
        let pb_path = dir.join(format!("bad{n}.pb"));
        std::fs::write(&pb_path, study(id, "/out/sample_p0_d0").encode_to_vec()).unwrap();
        let before = tree(&dir);
        let out = import(&pb_path, &store);
        assert_refused_as_an_id(&out, &format!("import {id:?}"));
        assert!(String::from_utf8_lossy(&out.stderr).contains("not a valid sweep_id"), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(tree(&dir), before, "import of {id:?} changed the tree");

        // --read-study takes the id on the command line (an argument cannot hold a NUL; the OS
        // refuses to spawn, which is its own refusal).
        if !id.contains('\0') {
            let out_pb = dir.join(format!("read{n}.pb"));
            let out = Command::new(av_sweep_bin()).arg("--read-study").arg(id).arg("--out-pb").arg(&out_pb).arg("--store-dir").arg(&store).output().unwrap();
            assert_refused_as_an_id(&out, &format!("read {id:?}"));
            assert!(String::from_utf8_lossy(&out.stderr).contains("not a valid sweep_id"), "{}", String::from_utf8_lossy(&out.stderr));
            assert!(!out_pb.exists());
            assert_eq!(tree(&dir), before, "read of {id:?} changed the tree");
        }
        assert!(!dir.join("x").exists(), "{id:?}: wrote the traversal target");
    }
    // The valid study is intact and a valid id still reads back.
    let out_pb = dir.join("good.out.pb");
    let out = Command::new(av_sweep_bin()).arg("--read-study").arg("good").arg("--out-pb").arg(&out_pb).arg("--store-dir").arg(&store).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `text` with the line starting `key:` replaced by `replacement`.
fn replace_line(text: &str, key: &str, replacement: &str) -> String {
    let mut done = false;
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            if !done && l.starts_with(key) {
                done = true;
                replacement.to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    assert!(done, "no line starts with {key}");
    lines.join("\n") + "\n"
}

/// Study mode, YAML inputs: a sweep whose `id` is `../x` (hash recomputed so the file is
/// otherwise valid) is refused in preflight when `--store-dir` is given, before `--out-dir` is
/// created or any sample runs; a `SystemDefinition.id` of `../x` is refused before anything is
/// written. Nothing appears outside the scratch folder's inputs.
#[test]
fn a_traversal_id_in_a_sweep_or_system_yaml_is_refused_before_any_sample_runs() {
    let dir = scratch("yaml");
    let sweep_text = std::fs::read_to_string(drms_path("leo_orbital_native_sweep.sweep.yaml")).unwrap();
    let mut sweep = av_sweep::parse_sweep_yaml(&sweep_text).unwrap();
    sweep.id = "../x".to_string();
    let hash = av_sweep::canonical_sweep_hash(&sweep);
    let bad_sweep = dir.join("bad.sweep.yaml");
    std::fs::write(&bad_sweep, replace_line(&replace_line(&sweep_text, "id:", "id: ../x"), "hash:", &format!("hash: \"{hash}\""))).unwrap();
    let sys_text = std::fs::read_to_string(drms_path("leo_1day_orbital_native.system.yaml")).unwrap();
    let bad_system = dir.join("bad.system.yaml");
    std::fs::write(&bad_system, replace_line(&sys_text, "id:", "id: ../x")).unwrap();
    let before = tree(&dir);

    let study_cmd = |sweep_yaml: &Path, system_yaml: &Path, out_dir: &Path, store: Option<&Path>| {
        let mut cmd = Command::new(av_sweep_bin());
        cmd.arg("--sweep").arg(sweep_yaml).arg("--drm").arg(drms_path("leo_orbital_native_sweep.drm.yaml")).arg("--sos").arg(drms_path("leo_orbital_native_sweep.sos.yaml"));
        cmd.arg("--system").arg(system_yaml).arg("--out-dir").arg(out_dir).arg("--workers").arg("2");
        if let Some(store) = store {
            cmd.arg("--store-dir").arg(store);
        }
        cmd.output().unwrap()
    };

    let out = study_cmd(&bad_sweep, &drms_path("leo_1day_orbital_native.system.yaml"), &dir.join("out"), Some(&dir.join("store")));
    assert_refused_as_an_id(&out, "sweep id ../x");
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot name a study directory"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(tree(&dir), before, "refused before --out-dir or the store was created");

    let out = study_cmd(&drms_path("leo_orbital_native_sweep.sweep.yaml"), &bad_system, &dir.join("out"), Some(&dir.join("store")));
    assert_refused_as_an_id(&out, "system id ../x");
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot name a sample file"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(tree(&dir), before);
    assert!(!dir.join("x").exists() && !dir.join("x.pb").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

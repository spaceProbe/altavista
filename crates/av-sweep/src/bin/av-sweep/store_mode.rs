//! Store mode (question 245): move one study between a study store and a file. Never touches
//! GMAT, a sweep or a child process.
//!
//! - `--read-study <sweep_id> --out-pb <file> [--out-json <file>]` loads the study from the one
//!   selected store (`--store-dir` or `--store-clickhouse`) and writes its
//!   `altavista.v1.SweepResults` protobuf (and optionally its canonical JSON) to a file. That file
//!   is exactly what the feasibility panel's data path takes: `POST /api/cdm/sweep` accepts a
//!   `SweepResults` body, so a study that lives only in ClickHouse reaches the panel through this
//!   mode (`tests/test_clickhouse_sweep_panel.py` is that path end to end).
//! - `--import-study <sweep_results.pb>` decodes a finished `SweepResults` and writes it into every
//!   selected store; it is how an existing study (a `sweep_results.pb` from an earlier run, a
//!   frozen fixture) enters a store without re-running GMAT.
//!
//! The study id (`--read-study`'s argument, or the `sweep_id` inside a decoded `--import-study`
//! file) is untrusted input. With `--store-dir` it becomes a directory name, so the file store
//! refuses anything that is not a path component (`..`, `a/b`, an absolute path, NUL, ...; the
//! rule is `av_cdm::path_id`) as `StoreError::InvalidSweepId`, before any path is built; this mode
//! reports it and writes nothing. When both stores are selected for an import, the file store is
//! tried first, so a refused id never reaches ClickHouse either. ClickHouse alone takes any id.
//!
//! The ClickHouse password is read from the environment variable the flags name; nothing here
//! prints it, and the errors printed come from `StoreError`, which never carries it.

use std::path::Path;

use av_cdm::pb;
use av_sweep::clickhouse::ClickHouseConfig;
use av_sweep::json;
use av_sweep::store::{open_store, StoreBackend};
use prost::Message;

use crate::cli::{StoreArgs, StoreOp};

/// Every store the flags selected, as (label for messages, backend).
pub fn selected_backends(store_dir: &Option<std::path::PathBuf>, clickhouse: &Option<ClickHouseConfig>) -> Vec<(String, StoreBackend)> {
    let mut out = Vec::new();
    if let Some(root) = store_dir {
        out.push((format!("the study store at {}", root.display()), StoreBackend::File { root: root.clone() }));
    }
    if let Some(config) = clickhouse {
        out.push((format!("the ClickHouse study store at {} (database {})", config.url, config.database), StoreBackend::ClickHouse(config.clone())));
    }
    out
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

pub fn run_store(args: StoreArgs) -> Result<(), String> {
    let backends = selected_backends(&args.store_dir, &args.clickhouse);
    match args.op {
        StoreOp::ReadStudy { sweep_id, out_pb, out_json } => {
            // The parser guarantees exactly one store for a read.
            let (label, backend) = backends.into_iter().next().ok_or_else(|| "--read-study needs a store".to_string())?;
            let store = open_store(backend).map_err(|e| format!("opening {label}: {e}"))?;
            let results = store
                .get_study(&sweep_id)
                .map_err(|e| format!("reading study {sweep_id:?} from {label}: {e}"))?
                .ok_or_else(|| format!("{label} holds no study {sweep_id:?}"))?;
            write_file(&out_pb, &results.encode_to_vec())?;
            if let Some(path) = out_json {
                write_file(&path, json::sweep_results_to_json(&results).as_bytes())?;
            }
            eprintln!("av-sweep: study {sweep_id:?} ({} sample(s), {} aggregate(s)) read from {label} into {}", results.samples.len(), results.aggregates.len(), out_pb.display());
            Ok(())
        }
        StoreOp::ImportStudy { pb } => {
            let bytes = std::fs::read(&pb).map_err(|e| format!("reading {}: {e}", pb.display()))?;
            let results = pb::SweepResults::decode(bytes.as_slice()).map_err(|e| format!("{}: does not decode as altavista.v1.SweepResults: {e}", pb.display()))?;
            for (label, backend) in backends {
                let mut store = open_store(backend).map_err(|e| format!("opening {label}: {e}"))?;
                let location = store.put_study(&results).map_err(|e| format!("writing study {:?} to {label}: {e}", results.sweep_id))?;
                eprintln!("av-sweep: study {:?} written to {location}", results.sweep_id);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-sweep-store-mode-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every file and directory under `dir` (relative path and size).
    fn tree(dir: &Path) -> BTreeMap<String, String> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
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
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn results(sweep_id: &str) -> pb::SweepResults {
        pb::SweepResults { sweep_id: sweep_id.to_string(), sweep_hash: "h".to_string(), drm_hash: "d".to_string(), ..Default::default() }
    }

    /// Question 254 item 1, store mode: `--read-study <id>` and `--import-study` (whose id comes
    /// out of a decoded `sweep_results.pb`) refuse an id that is not a path component with the
    /// store's typed error text, and neither the store root, its parent nor the output file is
    /// touched: the whole tree is identical before and after and the traversal target is absent.
    #[test]
    fn read_study_and_import_study_refuse_a_traversal_id_and_write_nothing() {
        let parent = temp_dir("traversal");
        let root = parent.join("store");
        let ids = ["../x".to_string(), "..".to_string(), parent.join("x").display().to_string(), "a/b".to_string(), "a\\b".to_string(), "a\0b".to_string(), "%2e%2e%2fx".to_string(), "..%2fx".to_string()];
        for (n, id) in ids.iter().enumerate() {
            // The id as a decoder delivers it: encoded into a protobuf, decoded by run_store.
            let pb_file = parent.join(format!("in{n}.pb"));
            std::fs::write(&pb_file, results(id).encode_to_vec()).unwrap();
            let before = tree(&parent);

            let imported = run_store(StoreArgs { op: StoreOp::ImportStudy { pb: pb_file.clone() }, store_dir: Some(root.clone()), clickhouse: None }).unwrap_err();
            assert!(imported.contains("not a valid sweep_id"), "{id:?}: {imported}");
            assert_eq!(tree(&parent), before, "import of {id:?} changed the tree");

            let out_pb = parent.join(format!("out{n}.pb"));
            let read = run_store(StoreArgs { op: StoreOp::ReadStudy { sweep_id: id.clone(), out_pb: out_pb.clone(), out_json: None }, store_dir: Some(root.clone()), clickhouse: None }).unwrap_err();
            assert!(read.contains("not a valid sweep_id"), "{id:?}: {read}");
            assert!(!out_pb.exists(), "{id:?}: --out-pb was written");
            assert_eq!(tree(&parent), before, "read of {id:?} changed the tree");
            assert!(!parent.join("x").exists() && !root.exists(), "{id:?}");
        }
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// A valid id still round-trips through import and read.
    #[test]
    fn a_valid_id_still_imports_and_reads_back() {
        let parent = temp_dir("valid");
        let root = parent.join("store");
        let pb_file = parent.join("in.pb");
        std::fs::write(&pb_file, results("study-1").encode_to_vec()).unwrap();
        run_store(StoreArgs { op: StoreOp::ImportStudy { pb: pb_file }, store_dir: Some(root.clone()), clickhouse: None }).unwrap();
        let out_pb = parent.join("out.pb");
        run_store(StoreArgs { op: StoreOp::ReadStudy { sweep_id: "study-1".to_string(), out_pb: out_pb.clone(), out_json: None }, store_dir: Some(root), clickhouse: None }).unwrap();
        assert_eq!(pb::SweepResults::decode(std::fs::read(out_pb).unwrap().as_slice()).unwrap(), results("study-1"));
        let _ = std::fs::remove_dir_all(&parent);
    }
}

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

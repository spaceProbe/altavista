//! The study store (F2, `docs/feasibility-plan.md`'s F2 milestone): a trait behind which a
//! completed [`pb::SweepResults`] and its samples get written and read back, and the file-backed
//! implementation of it that exists today.
//!
//! ## Why a trait at all
//!
//! `docs/architecture.md`'s P1 row names ClickHouse as this platform's store for MoEs
//! ("parameter sweeps on the job runner with MoEs in ClickHouse"), and question 191 (`docs/
//! open-questions.md`) decided "the study store is a trait with a file-backed implementation now
//! and ClickHouse later". F2 added the trait and [`FileStudyStore`]; question 245 added the
//! ClickHouse implementation, [`crate::clickhouse::ClickHouseStudyStore`], behind the same trait,
//! so every caller (`src/bin/av-sweep/study.rs`'s `--store-dir` and `--store-clickhouse`) goes
//! through [`StudyStore`] alone. [`open_store`] with [`StoreBackend::ClickHouse`] connects,
//! verifies the live schema (migrating a store that has no tables yet) and returns the store.
//!
//! **The two backends differ in one deliberate way.** [`FileStudyStore::put_study`] overwrites a
//! study of the same `sweep_id`; the ClickHouse store treats a study as immutable: the same id
//! with identical content is an idempotent `Ok`, with different content a typed
//! [`StoreError::RefusedWrite`] (see the `clickhouse` module doc, decision 2).
//!
//! ## On-disk layout ([`FileStudyStore`])
//!
//! ```text
//! <root>/<sweep_id>/sweep_results.pb    -- the whole SweepResults, prost canonical binary encoding
//! <root>/<sweep_id>/sweep_results.json  -- the same message, via crate::json::sweep_results_to_json
//! <root>/<sweep_id>/samples.jsonl       -- one crate::json::sweep_sample_json object per line,
//!                                           one per SweepSample, sorted (point_index, draw_index)
//! ```
//!
//! `sweep_results.pb`/`.json` reuse the exact same encoders `src/json.rs` already has (this
//! task's own brief: "reuse the existing JSON encoder rather than writing a second one") --
//! `samples.jsonl` is not a new encoding, it is [`crate::json::sweep_sample_json`] called once
//! per sample and joined with newlines.
//!
//! This directory tree is deliberately separate from `--out-dir` (`src/bin/av-sweep/study.rs`'s
//! own per-sample workspace: `drm.pb`/`sos.pb`/`sys_*.pb`/`stderr.txt`/`run_products.pb` per
//! sample, which F1b's own tests and this task's per-sample replay both depend on unchanged) --
//! the store is the platform's own read path for a finished study, `--out-dir` is where a sample
//! actually ran and can be reproduced from.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use av_cdm::pb;
use prost::Message;

use crate::json;

/// Every way a study-store operation can be refused -- mirrors `crate::error::SweepError`'s own
/// shape (one variant per refusal, named fields, a message naming the offending path/value).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A filesystem operation ([`std::fs::create_dir_all`]/`write`/`read`/`read_dir`) failed at
    /// `path`. Wraps the real `io::Error`, named at the exact path involved -- never just
    /// "failed". `PathBuf` has no `Display` impl (a path is not guaranteed valid UTF-8), so the
    /// message calls `.display()` explicitly rather than interpolating the field directly.
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, #[source] source: std::io::Error },

    /// `<sweep_id>/sweep_results.pb` existed but did not decode as `altavista.v1.SweepResults`
    /// (truncated write, foreign file, corrupted bytes).
    #[error("{}: does not decode as altavista.v1.SweepResults: {source}", path.display())]
    Decode { path: PathBuf, #[source] source: prost::DecodeError },

    /// The ClickHouse store refused to write study `sweep_id`; `reason` says whether the study
    /// already exists with different content, or the server refused the insert.
    #[error("study {sweep_id:?}: write refused: {reason}")]
    RefusedWrite { sweep_id: String, reason: WriteRefusal },

    /// The live ClickHouse table `table` does not have the shape this build expects: `column` is
    /// the first column that differs, `expected` and `found` describe it (a column's name, type
    /// and position, or `no such column`).
    #[error("ClickHouse schema mismatch in table {table}, column {column}: expected {expected}; found {found}")]
    SchemaMismatch { table: String, column: String, expected: String, found: String },

    /// No connection could be made to the ClickHouse server at `endpoint` (connection refused, no
    /// route, a name that does not resolve, or a timeout); `detail` is the transport's text.
    #[error("ClickHouse at {endpoint} is unreachable: {detail}")]
    Unreachable { endpoint: String, detail: String },

    /// The TLS handshake with `endpoint` failed (an untrusted or mismatching certificate, a
    /// protocol failure) or the TLS configuration could not be loaded; `reason` is OpenSSL's text.
    #[error("TLS failure talking to ClickHouse at {endpoint}: {reason}")]
    Tls { endpoint: String, reason: String },

    /// The ClickHouse URL was refused before any connection: plaintext to a non-loopback host
    /// (question 155), user information in the URL, or a malformed URL.
    #[error("invalid ClickHouse endpoint: {detail}")]
    InvalidEndpoint { detail: String },

    /// The ClickHouse store configuration is unusable (a password environment variable that is
    /// unset or empty, an empty database or user name).
    #[error("invalid ClickHouse store configuration: {detail}")]
    Config { detail: String },

    /// The HTTP exchange with ClickHouse failed in a way that is neither unreachability nor TLS
    /// (a malformed reply, an oversized reply, an I/O error mid-exchange).
    #[error("ClickHouse at {endpoint}: transport error while {operation}: {detail}")]
    Transport { endpoint: String, operation: String, detail: String },

    /// ClickHouse answered `operation` with an exception (`exception.code` is its numeric error
    /// code when the reply carried one).
    #[error("ClickHouse at {endpoint} refused while {operation}: {exception}")]
    Server { endpoint: String, operation: String, exception: crate::clickhouse::ServerException },

    /// ClickHouse's reply to `operation` was unusable or disagreed with the study's own header:
    /// truncated or malformed `RowBinary`, row counts that do not add up, or rows whose SHA-256
    /// differs from the one recorded when they were written.
    #[error("ClickHouse reply unusable while {operation}: {detail}")]
    Corrupt { operation: String, detail: String },

    /// The in-SQL aggregation hit the refusal [`crate::aggregate::aggregate`] gives for the same
    /// data (`SweepError::MixedPassCriterion`).
    #[error("aggregation: {0}")]
    Aggregation(#[from] crate::error::SweepError),
}

/// Why [`StoreError::RefusedWrite`] was returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    /// A study with this `sweep_id` already exists with different content; studies are immutable
    /// in the ClickHouse store (the file store overwrites).
    DifferentContent { existing_write_id: String, existing_sha256: String, new_sha256: String },
    /// The server refused one of the inserts (a read-only user, missing privileges, a full disk).
    Server { code: Option<i64>, name: Option<String>, http_status: u16, message: String },
}

impl std::fmt::Display for WriteRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteRefusal::DifferentContent { existing_write_id, existing_sha256, new_sha256 } => write!(
                f,
                "a study with this id already exists (write {existing_write_id}, content sha256 {existing_sha256}) and the new content (sha256 {new_sha256}) differs; studies are immutable"
            ),
            WriteRefusal::Server { code, name, http_status, message } => {
                write!(f, "the server refused the insert (HTTP {http_status}")?;
                if let Some(code) = code {
                    write!(f, ", code {code}")?;
                }
                if let Some(name) = name {
                    write!(f, " {name}")?;
                }
                write!(f, "): {message}")
            }
        }
    }
}

fn io_err(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io { path: path.to_path_buf(), source }
}

/// One study's identity and size, as listed by [`StudyStore::list_studies`] -- enough for a
/// caller (e.g. a future viewer panel, F3) to pick a study to open without decoding every study's
/// full `SweepResults` up front.
#[derive(Debug, Clone, PartialEq)]
pub struct StudySummary {
    pub sweep_id: String,
    pub sweep_hash: String,
    pub sample_count: usize,
}

/// The seam every study-store backend implements -- see this module's own doc comment for why
/// this exists at all (ClickHouse later, file-backed now, no caller-visible difference). Object
/// -safe deliberately (`&mut self`/`&self`, no generics), so [`open_store`] can return `Box<dyn
/// StudyStore>` and a caller need not know at compile time which backend it holds.
pub trait StudyStore {
    /// Writes `results` to this store, returning the location it wrote to (a path for
    /// [`FileStudyStore`]; a connection-qualified location string for a future ClickHouse-backed
    /// store) -- a plain `String` rather than a backend-specific type, since the only thing a
    /// caller does with it today is log/display it (`src/bin/av-sweep/study.rs`'s own
    /// `--store-dir` wiring).
    fn put_study(&mut self, results: &pb::SweepResults) -> Result<String, StoreError>;

    /// Reads back the study named `sweep_id`, or `Ok(None)` if this store holds no such study --
    /// "not found" is not itself an error condition here (a caller asking about a study that
    /// simply has not been written yet is normal, expected use, not a refusal).
    fn get_study(&self, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError>;

    /// Every study this store currently holds, as a [`StudySummary`] each -- sorted by
    /// `sweep_id` ([`FileStudyStore`]'s own implementation; see
    /// `tests::file_store_lists_the_studies_it_holds`).
    fn list_studies(&self) -> Result<Vec<StudySummary>, StoreError>;
}

/// The file-backed [`StudyStore`] -- see this module's own doc comment for the exact on-disk
/// layout under `root`.
pub struct FileStudyStore {
    root: PathBuf,
}

impl FileStudyStore {
    /// `root` need not exist yet -- [`StudyStore::put_study`] creates `root/<sweep_id>/` (and,
    /// transitively, `root` itself) on demand via `std::fs::create_dir_all`. Takes `impl
    /// AsRef<Path>` rather than `impl Into<PathBuf>` deliberately: `&PathBuf` (what
    /// `src/bin/av-sweep/study.rs`'s own `Option<PathBuf>` field naturally hands this as, via
    /// `if let Some(store_dir) = &args.store_dir`) has no direct `Into<PathBuf>` impl, but does
    /// satisfy `AsRef<Path>` through the standard blanket reference impl.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self { root: root.as_ref().to_path_buf() }
    }

    fn study_dir(&self, sweep_id: &str) -> PathBuf {
        self.root.join(sweep_id)
    }
}

/// [`crate::json::sweep_sample_json`], one call per sample, samples sorted `(point_index,
/// draw_index)` first (F2's own brief: "in `(point, draw)` order") -- a caller's own `samples`
/// slice is never trusted to already be in that order (`FileStudyStore::put_study` is not the
/// only path that could construct a `SweepResults`), so this always sorts a local copy rather
/// than assuming.
fn samples_jsonl(samples: &[pb::SweepSample]) -> String {
    let mut sorted: Vec<&pb::SweepSample> = samples.iter().collect();
    sorted.sort_by_key(|s| (s.point_index, s.draw_index));
    let mut out = String::new();
    for s in sorted {
        out.push_str(&json::sweep_sample_json(s));
        out.push('\n');
    }
    out
}

impl StudyStore for FileStudyStore {
    fn put_study(&mut self, results: &pb::SweepResults) -> Result<String, StoreError> {
        let dir = self.study_dir(&results.sweep_id);
        fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;

        let pb_path = dir.join("sweep_results.pb");
        fs::write(&pb_path, results.encode_to_vec()).map_err(|e| io_err(&pb_path, e))?;

        let json_path = dir.join("sweep_results.json");
        fs::write(&json_path, json::sweep_results_to_json(results)).map_err(|e| io_err(&json_path, e))?;

        let jsonl_path = dir.join("samples.jsonl");
        fs::write(&jsonl_path, samples_jsonl(&results.samples)).map_err(|e| io_err(&jsonl_path, e))?;

        Ok(dir.display().to_string())
    }

    fn get_study(&self, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
        let pb_path = self.study_dir(sweep_id).join("sweep_results.pb");
        if !pb_path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&pb_path).map_err(|e| io_err(&pb_path, e))?;
        let results = pb::SweepResults::decode(bytes.as_slice()).map_err(|e| StoreError::Decode { path: pb_path.clone(), source: e })?;
        Ok(Some(results))
    }

    fn list_studies(&self) -> Result<Vec<StudySummary>, StoreError> {
        if !self.root.exists() {
            // An empty/not-yet-created root holds no studies -- not an error (mirrors
            // get_study's own "not found is not a refusal" stance).
            return Ok(Vec::new());
        }
        let mut summaries = BTreeMap::new(); // sweep_id -> summary, so duplicates can't occur and iteration is already sweep_id-sorted
        for entry in fs::read_dir(&self.root).map_err(|e| io_err(&self.root, e))? {
            let entry = entry.map_err(|e| io_err(&self.root, e))?;
            if !entry.file_type().map_err(|e| io_err(&entry.path(), e))?.is_dir() {
                continue; // a study store directory holds only per-sweep-id subdirectories
            }
            let sweep_id = entry.file_name().to_string_lossy().into_owned();
            if let Some(results) = self.get_study(&sweep_id)? {
                summaries.insert(sweep_id, StudySummary { sweep_id: results.sweep_id.clone(), sweep_hash: results.sweep_hash.clone(), sample_count: results.samples.len() });
            }
        }
        Ok(summaries.into_values().collect())
    }
}

/// Which [`StudyStore`] backend [`open_store`] should open.
#[derive(Debug, Clone)]
pub enum StoreBackend {
    File { root: PathBuf },
    /// A ClickHouse server (question 245). The config names the password's environment variable,
    /// never the password.
    ClickHouse(crate::clickhouse::ClickHouseConfig),
}

/// Opens a [`StudyStore`] backend. `StoreBackend::File` always succeeds (construction alone,
/// [`FileStudyStore::new`], cannot fail). `StoreBackend::ClickHouse` connects, verifies the live
/// schema (migrating a store that has no tables yet, refusing one whose tables differ with
/// [`StoreError::SchemaMismatch`]) and returns the store; an unreachable server, a TLS failure or
/// a bad configuration is a typed error, never a store that pretends to be open.
pub fn open_store(backend: StoreBackend) -> Result<Box<dyn StudyStore>, StoreError> {
    match backend {
        StoreBackend::File { root } => Ok(Box::new(FileStudyStore::new(root))),
        StoreBackend::ClickHouse(config) => Ok(Box::new(crate::clickhouse::ClickHouseStudyStore::connect(&config)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-sweep-store-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(point: u32, draw: u32) -> pb::SweepSample {
        pb::SweepSample {
            point_index: point,
            draw_index: draw,
            seeds: BTreeMap::from([("k".to_string(), 1000 + (point as u64) * 10 + draw as u64)]),
            run_id: format!("study1_p{point}_d{draw}"),
            scores: BTreeMap::from([("m".to_string(), pb::ScoreResult { name: "m".to_string(), value: (point + draw) as f64, ..Default::default() })]),
            ..Default::default()
        }
    }

    fn study(sweep_id: &str, samples: Vec<pb::SweepSample>) -> pb::SweepResults {
        pb::SweepResults { sweep_id: sweep_id.to_string(), sweep_hash: format!("{sweep_id}-hash"), drm_hash: "drm-hash".to_string(), samples, aggregates: Vec::new(), provenance: None }
    }

    #[test]
    fn file_store_round_trips_a_study_through_put_and_get() {
        let root = temp_dir("round-trip");
        let mut store = FileStudyStore::new(&root);
        let results = study("study1", vec![sample(0, 0), sample(0, 1)]);

        let location = store.put_study(&results).expect("put_study");
        assert!(PathBuf::from(&location).exists(), "put_study's returned location must actually exist: {location}");

        let got = store.get_study("study1").expect("get_study").expect("study1 was just written");
        assert_eq!(got, results, "round trip must reproduce the exact message written");

        assert_eq!(store.get_study("no_such_study").expect("get_study on a missing id"), None, "a missing study is Ok(None), not an error");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `samples.jsonl` has exactly one line per sample, each a valid JSON object matching
    /// `crate::json::sweep_sample_json`'s own output, ordered `(point_index, draw_index)` --
    /// proven here by writing the samples in DELIBERATELY SHUFFLED order and checking the file
    /// still comes out point/draw-sorted.
    #[test]
    fn file_store_writes_one_jsonl_line_per_sample_in_point_then_draw_order() {
        let root = temp_dir("jsonl-order");
        let mut store = FileStudyStore::new(&root);
        // Shuffled on purpose: (1,0), (0,1), (0,0), (1,1) -- must come out (0,0),(0,1),(1,0),(1,1).
        let results = study("study2", vec![sample(1, 0), sample(0, 1), sample(0, 0), sample(1, 1)]);
        store.put_study(&results).expect("put_study");

        let jsonl_path = root.join("study2").join("samples.jsonl");
        let text = std::fs::read_to_string(&jsonl_path).expect("reading samples.jsonl");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "one line per sample");

        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("line is not valid JSON: {e}: {line}"));
            assert!(parsed.is_object(), "each line must be one JSON object: {line}");
        }
        // Order check: parse pointIndex/drawIndex back out of each line (proto3 omits the
        // default 0, so point 0 draw 0 has neither key -- checked separately below).
        let point_draw = |line: &str| -> (u32, u32) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let p = v.get("pointIndex").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let d = v.get("drawIndex").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            (p, d)
        };
        let order: Vec<(u32, u32)> = lines.iter().map(|l| point_draw(l)).collect();
        assert_eq!(order, vec![(0, 0), (0, 1), (1, 0), (1, 1)], "samples.jsonl must be in (point, draw) order regardless of the order SweepResults.samples itself carried them in");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_store_lists_the_studies_it_holds() {
        let root = temp_dir("list");
        let mut store = FileStudyStore::new(&root);
        assert_eq!(store.list_studies().expect("list_studies on an empty/nonexistent root"), Vec::new(), "an empty store holds no studies");

        store.put_study(&study("study_b", vec![sample(0, 0)])).expect("put study_b");
        store.put_study(&study("study_a", vec![sample(0, 0), sample(0, 1), sample(1, 0)])).expect("put study_a");

        let summaries = store.list_studies().expect("list_studies");
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].sweep_id, "study_a", "sorted by sweep_id");
        assert_eq!(summaries[0].sweep_hash, "study_a-hash");
        assert_eq!(summaries[0].sample_count, 3);
        assert_eq!(summaries[1].sweep_id, "study_b");
        assert_eq!(summaries[1].sample_count, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The deferred refusal is gone (question 245): `open_store(StoreBackend::ClickHouse)` now
    /// connects, so with nothing listening it is a typed `Unreachable` naming the endpoint, and a
    /// plaintext non-loopback URL is refused before any connection. (The real-server behaviour is
    /// `tests/clickhouse_store.rs`; the fake-server suite is `tests/clickhouse_fake_server.rs`.)
    #[test]
    fn the_clickhouse_backend_connects_or_refuses_with_a_typed_error_never_a_stub() {
        use crate::clickhouse::ClickHouseConfig;
        // Not `.unwrap_err()`: `Box<dyn StudyStore>` has no `Debug` for the panic message.
        let closed_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{closed_port}");
        let mut config = ClickHouseConfig::new(url.clone(), "db", "u");
        config.timeouts = av_http1::Timeouts::uniform(std::time::Duration::from_secs(2));
        let err = match open_store(StoreBackend::ClickHouse(config)) {
            Err(e) => e,
            Ok(_) => panic!("nothing listens on {url}: the open must fail"),
        };
        assert!(matches!(&err, StoreError::Unreachable { endpoint, .. } if *endpoint == url), "{err:?}");
        assert!(err.to_string().contains(&url), "{err}");

        let err = match open_store(StoreBackend::ClickHouse(ClickHouseConfig::new("http://192.0.2.9:8123", "db", "u"))) {
            Err(e) => e,
            Ok(_) => panic!("plaintext to a non-loopback host must be refused"),
        };
        assert!(matches!(err, StoreError::InvalidEndpoint { .. }), "{err:?}");

        // Sanity: the File backend, by contrast, actually works.
        let root = temp_dir("open-store-file");
        let mut store = open_store(StoreBackend::File { root: root.clone() }).expect("StoreBackend::File must succeed");
        store.put_study(&study("study1", vec![sample(0, 0)])).expect("the opened store must actually be usable");
        let _ = std::fs::remove_dir_all(&root);
    }
}

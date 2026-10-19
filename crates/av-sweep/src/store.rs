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
//! **Both backends treat a study as immutable** (question 252; before it the file store
//! overwrote). `put_study` of a `sweep_id` that already holds a study is an idempotent `Ok` when
//! the content is identical (equal [`crate::clickhouse::content_sha256`], the SHA-256 of the
//! canonical protobuf encoding, so NaN payloads and `-0.0` count) and writes nothing; with
//! different content it is a typed [`StoreError::RefusedWrite`] with
//! [`WriteRefusal::DifferentContent`], and the stored study is left byte for byte as it was (see
//! the `clickhouse` module doc, decision 2). A sweep id names one hashed sweep's results.
//!
//! The consequence for a caller that re-runs a study into the same store: the study record carries
//! each sample's `products_uri` (the canonical path of its `--out-dir` sample directory), so the
//! same sweep run again with a *different* `--out-dir` is different content and is refused; the
//! same `--out-dir` reproduces the record byte for byte and is an idempotent `Ok`.
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
//! ## How `put_study` writes, and what it guarantees
//!
//! A study directory is **published whole by one `rename`**: the three files are written and
//! `fsync`ed into a staging directory `<root>/.incoming-<pid>-<n>-<nanos>/`, which is then renamed
//! to `<root>/<sweep_id>/`. `rename(2)` of a directory onto a non-empty directory fails, so it can
//! never replace a study.
//!
//! - **A `sweep_id` is a path component, validated before any path is built** (question 254 item
//!   1). The id comes from a sweep YAML's `id`, a decoded `sweep_results.pb` (`--import-study`) or
//!   `--read-study <id>`: untrusted input. [`av_cdm::path_id::validate_path_component_id`] accepts
//!   1 to 128 bytes of ASCII `[A-Za-z0-9._-]` whose first byte is `[A-Za-z0-9]`; anything else
//!   (`..`, `a/b`, a backslash, an absolute path, NUL, any control character, the empty id) is a
//!   typed [`StoreError::InvalidSweepId`] from `put_study` and `get_study` alike, with no `join`,
//!   `exists` or `create_dir` having run, so nothing is read or written outside the root. (The
//!   rule applies to the file store only: ClickHouse takes the id as a SQL value, never a path,
//!   and has no such restriction.)
//! - **No half-written study is visible.** A reader sees no directory or the complete one.
//! - **A crash leaves the old state.** A crash before the rename leaves only a stray
//!   `.incoming-*` directory, which `list_studies` skips (an operator may delete it). A sweep id
//!   can never begin `.incoming-` -- no valid id begins with a dot -- so a staging directory can
//!   never be mistaken for a study, and a study never hidden from the listing by that prefix. A
//!   crash cannot leave a different study in place, because nothing is ever replaced.
//! - **Two writers racing on one id: the first rename wins.** The loser's rename fails, it reads
//!   the winner and answers as for any existing study: `Ok` if identical, `RefusedWrite` if not.
//!   (Unlike ClickHouse, whose racing writers are not serialized, the file store serializes them
//!   on the directory rename; both outcomes are the same typed results.) This holds across
//!   processes on one local filesystem; it is not claimed for NFS or other network filesystems,
//!   where `rename` atomicity varies.
//! - **An existing study that does not decode** (`sweep_results.pb` truncated or foreign) is
//!   [`StoreError::Decode`], never overwritten: the operator decides what to do with it.
//! - **A directory under the id that holds no `sweep_results.pb` but is not empty** (a torn
//!   directory from the pre-question-252 writer, or a foreign one) is an [`StoreError::Io`]
//!   naming it, not silently written into.
//! - **Healing.** An identical re-put of a study whose `sweep_results.json` or `samples.jsonl`
//!   is *missing* (the old writer wrote `sweep_results.pb` first and could be killed before the
//!   others) writes only the missing file. Files that exist are never rewritten.
//! - **Case-insensitive filesystems** (question 254 item 1; macOS's default APFS is one):
//!   `<root>/Study` and `<root>/study` are one directory there, so ids that differ only in ASCII
//!   case (exact, since ids are ASCII) would otherwise read each other's study. The store has one
//!   rule on every filesystem: **an id that ASCII-case-folds equal to a differently spelled
//!   directory already in the root is refused** with [`StoreError::SweepIdCollision`].
//!   `put_study` checks before it stages anything, and again when its rename fails (the racing
//!   writer path), with one `read_dir` of the root (cost: one directory listing per `put_study`,
//!   plus one per `get_study` that finds nothing or finds another id's study; a `get_study` hit
//!   reads none). `get_study` also requires the decoded `sweep_id` to equal the requested id
//!   exactly: a case-folded hit or a foreign file is that same typed error, never another id's
//!   study and never `None` (`None` would say "no such study" about a directory that holds one).
//!   `list_studies` skips a directory whose stored `sweep_id` differs from its name (it is not a
//!   study `get_study` can open under that name; listing it under the stored id would hand out an
//!   id that resolves to another directory or none). Limit: on a case-sensitive filesystem two
//!   writers racing on case variants of one id in the same instant can both pass the check and
//!   both publish; each study is whole and correct under its own id, and `list_studies` shows both.
//! - **`list_studies` skips a directory whose name is not a valid id**: the store cannot have
//!   written it (a staging directory, an operator's own folder, a name that is not UTF-8), and
//!   `get_study` would refuse the name anyway.
//!
//! This directory tree is deliberately separate from `--out-dir` (`src/bin/av-sweep/study.rs`'s
//! own per-sample workspace: `drm.pb`/`sos.pb`/`sys_*.pb`/`stderr.txt`/`run_products.pb` per
//! sample, which F1b's own tests and this task's per-sample replay both depend on unchanged) --
//! the store is the platform's own read path for a finished study, `--out-dir` is where a sample
//! actually ran and can be reproduced from.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb;
use prost::Message;

use crate::clickhouse::content_sha256;
use crate::json;

/// Name prefix of the staging directories `put_study` builds a study in before renaming it into
/// place. It begins with a dot, which no valid `sweep_id` can, so a staging directory can never be
/// a study directory.
const STAGING_PREFIX: &str = ".incoming-";

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

    /// `sweep_id` is not usable as a directory name (question 254 item 1): see
    /// [`av_cdm::path_id`] for the rule. Returned before any path is built, so nothing was read
    /// or written; the `reason` is the typed [`PathIdError`](av_cdm::path_id::PathIdError).
    #[error("study id {sweep_id:?} is not a valid sweep_id: {reason}")]
    InvalidSweepId { sweep_id: String, reason: av_cdm::path_id::PathIdError },

    /// The id `sweep_id` names, in this store's directory tree, a directory that is another id's:
    /// `dir` is a directory in the root holding (or named for) `existing`, which is not `sweep_id`
    /// but case-folds equal to it (`Study` against `study`: one directory on a case-insensitive
    /// filesystem), or whose decoded `sweep_id` is `existing` (a foreign or renamed directory).
    /// Nothing was written and no other id's study was returned. See the module doc.
    #[error("study id {sweep_id:?} collides with {existing:?} at {}: ids that differ only in ASCII case name one directory on a case-insensitive filesystem, and a directory must hold the study of the id it is named for", dir.display())]
    SweepIdCollision { sweep_id: String, existing: String, dir: PathBuf },

    /// A store refused to write study `sweep_id`; `reason` says whether the study already exists
    /// with different content (either backend), or the ClickHouse server refused the insert.
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
    /// in both stores. `existing_write_id` names the existing copy: ClickHouse's write id, or the
    /// file store's study directory (it has no write ids); `existing_sha256` and `new_sha256` are
    /// [`crate::clickhouse::content_sha256`] of each, the same identity in both backends.
    DifferentContent { existing_write_id: String, existing_sha256: String, new_sha256: String },
    /// The server refused one of the inserts (a read-only user, missing privileges, a full disk).
    Server { code: Option<i64>, name: Option<String>, http_status: u16, message: String },
}

impl std::fmt::Display for WriteRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteRefusal::DifferentContent { existing_write_id, existing_sha256, new_sha256 } => write!(
                f,
                "a study with this id already exists (existing copy {existing_write_id}, content sha256 {existing_sha256}) and the new content (sha256 {new_sha256}) differs; studies are immutable"
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
    ///
    /// Studies are immutable (question 252, both backends): a `sweep_id` that already holds a
    /// study with identical content is an idempotent `Ok` (the existing copy's location, nothing
    /// rewritten); with different content it is [`StoreError::RefusedWrite`] and the stored study
    /// is untouched.
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

    /// `<root>/<sweep_id>`, but only for a valid path-component id: the one place an id becomes
    /// a path, and the validation runs before the `join`.
    fn study_dir(&self, sweep_id: &str) -> Result<PathBuf, StoreError> {
        av_cdm::path_id::validate_path_component_id(sweep_id).map_err(|reason| StoreError::InvalidSweepId { sweep_id: sweep_id.to_string(), reason })?;
        Ok(self.root.join(sweep_id))
    }

    /// Refuses `sweep_id` when the root holds an entry spelled differently that ASCII-case-folds
    /// equal to it (one `read_dir` of the root; a root that does not exist yet holds none). The
    /// entry's own spelling is what the error names, so it is the on-disk one even where the
    /// filesystem would have resolved `sweep_id` to it.
    fn reject_case_variant(&self, sweep_id: &str) -> Result<(), StoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(&self.root, e)),
        };
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&self.root, e))?;
            let name = entry.file_name();
            if let Some(name) = name.to_str().filter(|n| *n != sweep_id && n.eq_ignore_ascii_case(sweep_id)) {
                return Err(StoreError::SweepIdCollision { sweep_id: sweep_id.to_string(), existing: name.to_string(), dir: entry.path() });
            }
        }
        Ok(())
    }

    /// The study in `dir` (which must be `study_dir(sweep_id)`), `None` if it holds no
    /// `sweep_results.pb`. The decoded `sweep_id` must equal `sweep_id` exactly: otherwise the
    /// filesystem resolved the name to another id's directory (a case-folded hit) or the directory
    /// holds a foreign file, and that is [`StoreError::SweepIdCollision`].
    fn read_study(&self, dir: &Path, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
        let pb_path = dir.join("sweep_results.pb");
        if !pb_path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&pb_path).map_err(|e| io_err(&pb_path, e))?;
        let results = pb::SweepResults::decode(bytes.as_slice()).map_err(|e| StoreError::Decode { path: pb_path.clone(), source: e })?;
        if results.sweep_id != sweep_id {
            self.reject_case_variant(sweep_id)?; // a case variant names its real on-disk spelling
            return Err(StoreError::SweepIdCollision { sweep_id: sweep_id.to_string(), existing: results.sweep_id, dir: dir.to_path_buf() });
        }
        Ok(Some(results))
    }

    /// What `put_study` answers when its rename failed: either another writer published this id
    /// (or a case variant of it) in the meantime, or `dir` is a non-empty directory that holds no
    /// study.
    fn settle_failed_rename(&self, dir: &Path, results: &pb::SweepResults, rename_error: std::io::Error) -> Result<String, StoreError> {
        self.reject_case_variant(&results.sweep_id)?;
        match self.read_study(dir, &results.sweep_id)? {
            Some(existing) => settle_existing(dir, &existing, results),
            None => Err(io_err(dir, rename_error)),
        }
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

/// The three files of a study directory, name and bytes (see the module doc's layout).
fn study_files(results: &pb::SweepResults) -> [(&'static str, Vec<u8>); 3] {
    [
        ("sweep_results.pb", results.encode_to_vec()),
        ("sweep_results.json", json::sweep_results_to_json(results).into_bytes()),
        ("samples.jsonl", samples_jsonl(&results.samples).into_bytes()),
    ]
}

/// Creates `path` (which must not exist), writes `bytes` and `fsync`s it, so that a rename of its
/// directory cannot publish a file whose data is still only in the page cache.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| io_err(path, e))?;
    f.write_all(bytes).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

/// A staging-directory name no other writer (process or thread, now or after a pid is reused) shares.
fn staging_dir(root: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    root.join(format!("{STAGING_PREFIX}{}-{}-{nanos}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// The answer for a `sweep_id` that already holds `existing`: identical content is `Ok` with
/// nothing rewritten (apart from a derived file that is missing, see the module doc's "Healing");
/// different content is [`WriteRefusal::DifferentContent`].
fn settle_existing(dir: &Path, existing: &pb::SweepResults, new: &pb::SweepResults) -> Result<String, StoreError> {
    let (existing_sha256, new_sha256) = (content_sha256(existing), content_sha256(new));
    if existing_sha256 != new_sha256 {
        return Err(StoreError::RefusedWrite {
            sweep_id: new.sweep_id.clone(),
            reason: WriteRefusal::DifferentContent { existing_write_id: dir.display().to_string(), existing_sha256, new_sha256 },
        });
    }
    for (name, bytes) in study_files(new).into_iter().skip(1) {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        let tmp = dir.join(format!(".{name}.heal-{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        write_synced(&tmp, &bytes)?;
        fs::rename(&tmp, &path).map_err(|e| io_err(&path, e))?;
    }
    Ok(dir.display().to_string())
}

impl StudyStore for FileStudyStore {
    fn put_study(&mut self, results: &pb::SweepResults) -> Result<String, StoreError> {
        let dir = self.study_dir(&results.sweep_id)?;
        self.reject_case_variant(&results.sweep_id)?;
        if let Some(existing) = self.read_study(&dir, &results.sweep_id)? {
            return settle_existing(&dir, &existing, results);
        }

        // No study under this id: build it whole in a staging directory, then publish it with one
        // rename (module doc, "How put_study writes").
        let parent = dir.parent().unwrap_or(&self.root).to_path_buf();
        fs::create_dir_all(&parent).map_err(|e| io_err(&parent, e))?;
        let stage = staging_dir(&self.root);
        fs::create_dir(&stage).map_err(|e| io_err(&stage, e))?;
        let staged = study_files(results).into_iter().try_for_each(|(name, bytes)| write_synced(&stage.join(name), &bytes));
        if let Err(e) = staged {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        match fs::rename(&stage, &dir) {
            Ok(()) => {
                // Best effort: make the new directory entry durable.
                if let Ok(d) = fs::File::open(&parent) {
                    let _ = d.sync_all();
                }
                Ok(dir.display().to_string())
            }
            Err(rename_error) => {
                let _ = fs::remove_dir_all(&stage);
                self.settle_failed_rename(&dir, results, rename_error)
            }
        }
    }

    fn get_study(&self, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
        let dir = self.study_dir(sweep_id)?;
        let found = self.read_study(&dir, sweep_id)?;
        if found.is_none() {
            self.reject_case_variant(sweep_id)?; // "not found" must not hide a study under a case variant
        }
        Ok(found)
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
            let Ok(sweep_id) = entry.file_name().into_string() else { continue }; // not UTF-8: not a valid id
            if av_cdm::path_id::validate_path_component_id(&sweep_id).is_err() {
                continue; // not a name this store can have written: a staging directory (`.incoming-*`), an operator's folder, ...
            }
            match self.read_study(&entry.path(), &sweep_id) {
                Ok(Some(results)) => {
                    summaries.insert(sweep_id, StudySummary { sweep_id: results.sweep_id.clone(), sweep_hash: results.sweep_hash.clone(), sample_count: results.samples.len() });
                }
                Ok(None) | Err(StoreError::SweepIdCollision { .. }) => {} // no study, or one stored under another id: not listed under this name
                Err(e) => return Err(e),
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

    /// Every regular file under `dir/<id>` with its bytes, modification time and inode, so a test
    /// can prove a put left the study untouched (not rewritten, not replaced by an equal copy).
    fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime, u64)> {
        use std::os::unix::fs::MetadataExt;
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            out.insert(entry.file_name().to_string_lossy().into_owned(), (std::fs::read(entry.path()).unwrap(), meta.modified().unwrap(), meta.ino()));
        }
        out
    }

    fn changed(mut s: pb::SweepResults) -> pb::SweepResults {
        s.samples[0].products_uri = "/elsewhere/sample_p0_d0".to_string();
        s
    }

    fn assert_refused_different(err: &StoreError, id: &str, existing: &pb::SweepResults, new: &pb::SweepResults) {
        match err {
            StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::DifferentContent { existing_write_id, existing_sha256, new_sha256 } } => {
                assert_eq!(sweep_id, id);
                assert!(existing_write_id.ends_with(id), "names the existing copy: {existing_write_id}");
                assert_eq!(*existing_sha256, content_sha256(existing));
                assert_eq!(*new_sha256, content_sha256(new));
                assert_ne!(existing_sha256, new_sha256);
            }
            other => panic!("expected RefusedWrite/DifferentContent, got {other:?}"),
        }
    }

    /// Question 252: an identical re-put is an idempotent `Ok` returning the same location, and
    /// rewrites nothing: every file keeps its bytes, mtime and inode.
    #[test]
    fn an_identical_re_put_is_ok_and_rewrites_nothing() {
        let root = temp_dir("idempotent");
        let mut store = FileStudyStore::new(&root);
        let results = study("s1", vec![sample(0, 0), sample(0, 1)]);
        let first = store.put_study(&results).unwrap();
        let before = snapshot(&root.join("s1"));
        assert_eq!(before.len(), 3);
        std::thread::sleep(std::time::Duration::from_millis(30)); // so a rewrite could not share an mtime

        let second = store.put_study(&results.clone()).expect("identical content is idempotent");
        assert_eq!(second, first, "the same location");
        assert_eq!(snapshot(&root.join("s1")), before, "bytes, mtime and inode of all three files unchanged");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no staging directory left behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Question 252: different content under an existing id is a typed `RefusedWrite` and the
    /// stored study is unchanged byte for byte (and still reads back as the first one).
    #[test]
    fn different_content_under_an_existing_id_is_refused_and_the_stored_study_is_unchanged() {
        let root = temp_dir("refused");
        let mut store = FileStudyStore::new(&root);
        let first = study("s1", vec![sample(0, 0), sample(0, 1)]);
        store.put_study(&first).unwrap();
        let before = snapshot(&root.join("s1"));
        std::thread::sleep(std::time::Duration::from_millis(30));

        let second = changed(first.clone());
        let err = store.put_study(&second).unwrap_err();
        assert_refused_different(&err, "s1", &first, &second);
        let msg = store.put_study(&second).unwrap_err().to_string();
        assert!(msg.contains("\"s1\"") && msg.contains("write refused") && msg.contains("immutable"), "{msg}");
        assert_eq!(snapshot(&root.join("s1")), before, "the stored study is untouched, mtime and inode included");
        assert_eq!(store.get_study("s1").unwrap().unwrap(), first);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no staging directory left behind");

        // A different id is not affected by the refusal.
        store.put_study(&study("s2", vec![sample(0, 0)])).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The identity is the SHA-256 of the canonical encoding, the same function ClickHouse uses
    /// ([`content_sha256`]), so the two backends agree on what "identical" means, bit for bit
    /// where the encoding is: an optional `-0.0` and a NaN payload are different content. A plain
    /// proto3 `double` equal to zero is *not encoded at all* (prost skips `== 0.0`, which is true
    /// of `-0.0`), so there `-0.0` and `0.0` are the same content in both backends -- measured
    /// here, and recorded in the report for question 252.
    #[test]
    fn identity_is_the_canonical_encodings_bits_like_clickhouses() {
        let root = temp_dir("bits");
        let mut store = FileStudyStore::new(&root);
        let with_aggregate = |pass_fraction: f64, mean: f64| {
            let mut s = study("s1", vec![sample(0, 0)]);
            s.aggregates = vec![pb::ScoreAggregate { name: "m".to_string(), point_index: 0, draws: 1, mean, std_dev: 0.0, min: 0.0, max: 0.0, pass_fraction: Some(pass_fraction) }];
            s
        };
        let base = with_aggregate(0.0, 0.0);
        store.put_study(&base).unwrap();
        // Optional field: presence is encoded, so the sign bit of zero is part of the identity.
        let negative = with_aggregate(-0.0, 0.0);
        assert_ne!(content_sha256(&base), content_sha256(&negative));
        assert!(matches!(store.put_study(&negative), Err(StoreError::RefusedWrite { .. })));
        // A NaN payload is part of the identity.
        let quiet = with_aggregate(f64::NAN, 0.0);
        let other_nan = with_aggregate(f64::from_bits(f64::NAN.to_bits() ^ 1), 0.0);
        let nan_root = temp_dir("bits-nan");
        let mut nan_store = FileStudyStore::new(&nan_root);
        nan_store.put_study(&quiet).unwrap();
        assert!(matches!(nan_store.put_study(&other_nan), Err(StoreError::RefusedWrite { .. })));
        // A plain zero double is not on the wire, so -0.0 there is the same content.
        assert_eq!(content_sha256(&with_aggregate(0.0, 0.0)), content_sha256(&with_aggregate(0.0, -0.0)));
        store.put_study(&with_aggregate(0.0, -0.0)).expect("-0.0 in a plain double is not distinguishable in the canonical encoding");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&nan_root);
    }

    /// Two writers racing on one id (threads here, each with its own store handle, as two
    /// processes would have): the first directory rename wins, every loser is told the truth.
    #[test]
    fn racing_writers_of_different_content_yield_exactly_one_winner() {
        for round in 0..20 {
            let root = temp_dir(&format!("race-diff-{round}"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8u32)
                .map(|n| {
                    let (root, barrier) = (root.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        let results = study("race", vec![sample(0, 0), sample(0, n + 1)]);
                        barrier.wait();
                        (n, results.clone(), FileStudyStore::new(&root).put_study(&results))
                    })
                })
                .collect();
            let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners: Vec<_> = outcomes.iter().filter(|(_, _, r)| r.is_ok()).collect();
            assert_eq!(winners.len(), 1, "round {round}: exactly one writer wins");
            let stored = FileStudyStore::new(&root).get_study("race").unwrap().unwrap();
            assert_eq!(stored, winners[0].1, "the stored study is the winner's");
            for (_, results, outcome) in &outcomes {
                if let Err(e) = outcome {
                    assert_refused_different(e, "race", &stored, results);
                }
            }
            let names: Vec<String> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            assert_eq!(names, vec!["race".to_string()], "round {round}: no staging directory left behind");
            // The three files are all there and agree with each other.
            assert_eq!(std::fs::read_to_string(root.join("race/sweep_results.json")).unwrap(), json::sweep_results_to_json(&stored));
            assert_eq!(std::fs::read_to_string(root.join("race/samples.jsonl")).unwrap(), samples_jsonl(&stored.samples));
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// Racing writers of *identical* content all get `Ok` and the study is written once.
    #[test]
    fn racing_writers_of_identical_content_all_succeed() {
        for round in 0..20 {
            let root = temp_dir(&format!("race-same-{round}"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let results = study("race", vec![sample(0, 0), sample(1, 1)]);
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (root, barrier, results) = (root.clone(), barrier.clone(), results.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        FileStudyStore::new(&root).put_study(&results)
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap().expect("identical content is Ok whoever wins");
            }
            assert_eq!(FileStudyStore::new(&root).get_study("race").unwrap().unwrap(), results);
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "round {round}: no staging directory left behind");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// An existing `sweep_results.pb` that does not decode is a typed `Decode` error: never
    /// overwritten, never mistaken for "no study".
    #[test]
    fn an_existing_study_that_does_not_decode_is_a_decode_error_and_is_left_alone() {
        let root = temp_dir("corrupt");
        let mut store = FileStudyStore::new(&root);
        std::fs::create_dir_all(root.join("s1")).unwrap();
        std::fs::write(root.join("s1/sweep_results.pb"), b"\xff\xff not a protobuf").unwrap();
        let before = snapshot(&root.join("s1"));
        let err = store.put_study(&study("s1", vec![sample(0, 0)])).unwrap_err();
        assert!(matches!(&err, StoreError::Decode { path, .. } if path.ends_with("s1/sweep_results.pb")), "{err:?}");
        assert_eq!(snapshot(&root.join("s1")), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The old writer wrote `sweep_results.pb` first and could die before the other two files. An
    /// identical re-put writes exactly the missing ones and touches nothing that exists.
    #[test]
    fn an_identical_re_put_heals_missing_derived_files_and_only_those() {
        let root = temp_dir("heal");
        let mut store = FileStudyStore::new(&root);
        let results = study("s1", vec![sample(0, 0), sample(0, 1)]);
        store.put_study(&results).unwrap();
        let full = snapshot(&root.join("s1"));
        std::fs::remove_file(root.join("s1/samples.jsonl")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));

        store.put_study(&results).unwrap();
        let healed = snapshot(&root.join("s1"));
        assert_eq!(healed.len(), 3, "no stray temp file");
        assert_eq!(healed["samples.jsonl"].0, full["samples.jsonl"].0, "recreated byte for byte");
        assert_eq!(healed["sweep_results.pb"], full["sweep_results.pb"], "an existing file is not rewritten");
        assert_eq!(healed["sweep_results.json"], full["sweep_results.json"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A non-empty directory under the id that holds no study is not written into or replaced: a
    /// typed `Io` error naming it. An empty one is just filled.
    #[test]
    fn a_directory_that_is_not_a_study_is_a_typed_io_error() {
        let root = temp_dir("foreign");
        let mut store = FileStudyStore::new(&root);
        std::fs::create_dir_all(root.join("s1")).unwrap();
        std::fs::write(root.join("s1/notes.txt"), b"mine").unwrap();
        let err = store.put_study(&study("s1", vec![sample(0, 0)])).unwrap_err();
        assert!(matches!(&err, StoreError::Io { path, .. } if path.ends_with("s1")), "{err:?}");
        assert_eq!(std::fs::read(root.join("s1/notes.txt")).unwrap(), b"mine");
        assert!(!root.join("s1/sweep_results.pb").exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "the staging directory was removed");

        std::fs::create_dir_all(root.join("s2")).unwrap();
        store.put_study(&study("s2", vec![sample(0, 0)])).expect("an empty directory is just filled");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every file and directory under `dir` (relative path, kind and size), so a test can prove a
    /// refused call changed nothing anywhere in a whole tree, outside the store root included.
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

    /// The ids a traversal attempt would use against the store under `<parent>/store`, whose
    /// intended victim is `<parent>/x`. The encoded variants are the literal strings a decoder
    /// has not run over; the decoded ones (`../x`) are what a YAML or protobuf decoder delivers.
    fn traversal_ids(parent: &Path) -> Vec<String> {
        vec![
            "../x".into(),
            "..".into(),
            ".".into(),
            parent.join("x").display().to_string(), // absolute: `Path::join` would replace the root with it
            "/x".into(),
            "a/b".into(),
            "a\\b".into(),
            "..\\x".into(),
            "a\0b".into(),
            "\0".into(),
            "".into(),
            "%2e%2e%2fx".into(),
            "..%2fx".into(),
            "a%2Fb".into(),
            ".incoming-1-2-3".into(),
            "x/../x".into(),
        ]
    }

    /// Question 254 item 1: a `sweep_id` that is not a path component is a typed
    /// `InvalidSweepId` from `put_study` and from `get_study`, and nothing is written outside the
    /// store root (or inside it): the whole parent tree is identical before and after, and the
    /// traversal's target `x` does not exist.
    #[test]
    fn a_sweep_id_that_is_not_a_path_component_is_refused_and_nothing_is_written() {
        let parent = temp_dir("traversal");
        let root = parent.join("store");
        let mut store = FileStudyStore::new(&root);
        store.put_study(&study("s1", vec![sample(0, 0)])).unwrap();
        let before = tree(&parent);
        assert!(before.contains_key("store/s1/sweep_results.pb"));

        for id in traversal_ids(&parent) {
            let put = store.put_study(&study(&id, vec![sample(0, 0)])).unwrap_err();
            assert!(matches!(&put, StoreError::InvalidSweepId { sweep_id, .. } if *sweep_id == id), "put_study({id:?}): {put:?}");
            let got = match store.get_study(&id) {
                Err(e) => e,
                Ok(r) => panic!("get_study({id:?}) must be refused, got {r:?}"),
            };
            assert!(matches!(&got, StoreError::InvalidSweepId { sweep_id, .. } if *sweep_id == id), "get_study({id:?}): {got:?}");
            assert!(put.to_string().contains("not a valid sweep_id"), "{put}");
            assert_eq!(tree(&parent), before, "{id:?}: the tree around the store changed");
            assert!(!parent.join("x").exists(), "{id:?}: wrote the traversal target");
        }
        // The valid study is untouched and still the only one listed.
        assert_eq!(store.list_studies().unwrap().iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["s1"]);
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Every refused id in the shared vectors (`data/path_component_id_vectors.json`, the file the
    /// rule's own tests read) is refused by the store with the rule's typed reason, and every
    /// accepted one is a usable study id.
    #[test]
    fn the_shared_vectors_decide_what_the_store_accepts() {
        let vectors: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/path_component_id_vectors.json")).unwrap()).unwrap();
        let parent = temp_dir("vectors");
        let root = parent.join("store");
        let mut store = FileStudyStore::new(&root);
        let before = tree(&parent);
        for entry in vectors["refused"].as_array().unwrap() {
            let id = entry["id"].as_str().unwrap();
            match store.put_study(&study(id, vec![sample(0, 0)])) {
                Err(StoreError::InvalidSweepId { reason, .. }) => assert_eq!(reason.reason(), entry["reason"].as_str().unwrap(), "{id:?}"),
                other => panic!("{id:?} must be refused as an invalid sweep_id, got {other:?}"),
            }
        }
        assert_eq!(tree(&parent), before, "no refusal created the store root or anything else");
        // One store per accepted id: `a` and `A` are different ids that name one directory on a
        // case-insensitive filesystem (macOS's default); in one store the second would be a typed
        // `SweepIdCollision`, never an overwrite (`case_variant_ids_are_refused_*` below).
        for (n, id) in vectors["accepted"].as_array().unwrap().iter().enumerate() {
            let id = id.as_str().unwrap();
            let mut own = FileStudyStore::new(parent.join(format!("accepted-{n}")));
            own.put_study(&study(id, vec![sample(0, 0)])).unwrap_or_else(|e| panic!("{id:?} must be accepted: {e}"));
            assert!(own.get_study(id).unwrap().is_some(), "{id:?}");
        }
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// `list_studies` skips a directory whose name the store could not have written, even when it
    /// holds a decodable `sweep_results.pb`; a valid one beside it is listed.
    #[test]
    fn list_studies_skips_directories_whose_names_are_not_valid_ids() {
        let root = temp_dir("list-invalid");
        let mut store = FileStudyStore::new(&root);
        store.put_study(&study("good", vec![sample(0, 0)])).unwrap();
        for name in [".hidden", "has space", "a%b", "-dash", "caf\u{e9}"] {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("sweep_results.pb"), study("ghost", vec![sample(0, 0)]).encode_to_vec()).unwrap();
        }
        assert_eq!(store.list_studies().unwrap().iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["good"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A staging directory (in flight, or left by a crashed writer) is not a study.
    #[test]
    fn list_studies_skips_staging_directories() {
        let root = temp_dir("staging");
        let mut store = FileStudyStore::new(&root);
        store.put_study(&study("s1", vec![sample(0, 0)])).unwrap();
        let stray = root.join(".incoming-1-2-3");
        std::fs::create_dir_all(&stray).unwrap();
        for (name, bytes) in study_files(&study("ghost", vec![sample(0, 0)])) {
            std::fs::write(stray.join(name), bytes).unwrap();
        }
        let listed = store.list_studies().unwrap();
        assert_eq!(listed.iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["s1"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Whether `dir` is on a case-insensitive filesystem, measured: write `case-probe-a` and ask
    /// whether `CASE-PROBE-A` exists. The case tests assert their premise with this rather than
    /// assume it, so they cannot pass on one kind of filesystem for the other's reason.
    fn is_case_insensitive(dir: &Path) -> bool {
        let probe = dir.join("case-probe-a");
        std::fs::write(&probe, b"").unwrap();
        let insensitive = dir.join("CASE-PROBE-A").exists();
        std::fs::remove_file(&probe).unwrap();
        insensitive
    }

    fn dir_names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn assert_collision(err: &StoreError, requested: &str, existing: &str, dir_name: &str) {
        match err {
            StoreError::SweepIdCollision { sweep_id, existing: found, dir } => {
                assert_eq!(sweep_id, requested);
                assert_eq!(found, existing);
                assert_eq!(dir.file_name().unwrap().to_str().unwrap(), dir_name, "names the on-disk directory: {dir:?}");
                let msg = err.to_string();
                assert!(msg.contains(&format!("{requested:?}")) && msg.contains(&format!("{existing:?}")) && msg.contains(dir_name), "{msg}");
            }
            other => panic!("expected SweepIdCollision, got {other:?}"),
        }
    }

    /// The one rule, run on a store under `root`: `study` is stored; `Study` (and `STUDY`) is
    /// refused by `put_study` and `get_study` whatever the filesystem; the stored study is untouched
    /// and nothing is left staged; `study` itself still reads and re-puts. `insensitive` is the
    /// measured kind of `root`'s filesystem.
    fn assert_case_variants_are_refused(root: &Path, insensitive: bool) {
        let mut store = FileStudyStore::new(root);
        let first = study("study", vec![sample(0, 0), sample(0, 1)]);
        store.put_study(&first).unwrap();
        // The premise: on a case-insensitive filesystem `Study` resolves to the stored directory
        // (the defect's cause); on a case-sensitive one it does not exist.
        assert_eq!(root.join("Study/sweep_results.pb").exists(), insensitive, "the measured filesystem kind");
        let before = snapshot(&root.join("study"));
        std::thread::sleep(std::time::Duration::from_millis(30));

        for variant in ["Study", "STUDY", "sTuDy"] {
            let err = store.put_study(&study(variant, vec![sample(0, 0), sample(0, 1)])).unwrap_err();
            assert_collision(&err, variant, "study", "study");
            let err = store.get_study(variant).unwrap_err();
            assert_collision(&err, variant, "study", "study");
        }
        assert_eq!(snapshot(&root.join("study")), before, "the stored study is untouched, bytes, mtime and inode");
        assert_eq!(dir_names(root), vec!["study".to_string()], "nothing staged left, no second directory");

        assert_eq!(store.get_study("study").unwrap().unwrap(), first);
        let second = store.put_study(&first.clone()).expect("an identical re-put of study is still Ok");
        assert!(second.ends_with("study"), "{second}");
        assert_eq!(snapshot(&root.join("study")), before, "the identical re-put rewrote nothing");
        assert_eq!(store.list_studies().unwrap().iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["study"]);

        // An unrelated id is unaffected, and refuses its own variants.
        store.put_study(&study("other", vec![sample(0, 0)])).unwrap();
        assert_collision(&store.put_study(&study("OTHER", vec![sample(0, 0)])).unwrap_err(), "OTHER", "other", "other");
        assert_eq!(dir_names(root), vec!["other".to_string(), "study".to_string()]);
    }

    /// Question 254 item 1 on this host's filesystem (macOS's default APFS is case-insensitive, and
    /// `is_case_insensitive` measures it rather than assuming it; the kind is printed). On a
    /// case-sensitive filesystem the same assertions hold, for the other reason: `Study` is a
    /// different, absent directory and the explicit check is what refuses it.
    #[test]
    fn case_variant_ids_are_refused_on_this_filesystem() {
        let root = temp_dir("case-variants");
        let insensitive = is_case_insensitive(&root);
        eprintln!("case_variant_ids_are_refused_on_this_filesystem: {} is on a case-{} filesystem", root.display(), if insensitive { "insensitive" } else { "sensitive" });
        assert_case_variants_are_refused(&root, insensitive);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same rule on a case-sensitive volume, where `study` and `Study` could be two
    /// directories: the store refuses the pair there too. Point `AV_SWEEP_CASE_SENSITIVE_DIR` at a
    /// directory on a case-sensitive volume (e.g. `hdiutil create -fs "Case-sensitive APFS" ...`);
    /// without it the test says it was skipped. With both directories already present (written by
    /// an older store or by hand) each still reads under its exact id and both are listed.
    #[test]
    fn case_variant_ids_are_refused_on_a_case_sensitive_volume() {
        let Some(base) = std::env::var_os("AV_SWEEP_CASE_SENSITIVE_DIR").map(PathBuf::from) else {
            eprintln!("SKIPPED case_variant_ids_are_refused_on_a_case_sensitive_volume: AV_SWEEP_CASE_SENSITIVE_DIR is not set (no case-sensitive volume given)");
            return;
        };
        let root = base.join(format!("av-sweep-store-test-cs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(!is_case_insensitive(&root), "AV_SWEEP_CASE_SENSITIVE_DIR ({}) is not on a case-sensitive volume", base.display());
        assert_case_variants_are_refused(&root, false);

        // Both spellings already on disk: each is its own whole study.
        let pair = root.join("pair");
        let mut store = FileStudyStore::new(&pair);
        let (lower, upper) = (study("pair", vec![sample(0, 0)]), study("PAIR", vec![sample(0, 1)]));
        for (name, results) in [("pair", &lower), ("PAIR", &upper)] {
            std::fs::create_dir_all(pair.join(name)).unwrap();
            for (file, bytes) in study_files(results) {
                std::fs::write(pair.join(name).join(file), bytes).unwrap();
            }
        }
        assert_eq!(store.get_study("pair").unwrap().unwrap(), lower);
        assert_eq!(store.get_study("PAIR").unwrap().unwrap(), upper);
        assert_eq!(store.list_studies().unwrap().iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["PAIR", "pair"]);
        assert_collision(&store.put_study(&study("Pair", vec![sample(0, 0)])).unwrap_err(), "Pair", "PAIR", "PAIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory that holds another id's study is not that id's study, on any filesystem:
    /// `get_study` and `put_study` give the typed collision (naming the stored id and the
    /// directory), `list_studies` skips it, and nothing is written.
    #[test]
    fn a_directory_holding_another_ids_study_is_a_collision_not_a_study() {
        let root = temp_dir("foreign-id");
        let mut store = FileStudyStore::new(&root);
        store.put_study(&study("good", vec![sample(0, 0)])).unwrap();
        std::fs::create_dir_all(root.join("s1")).unwrap();
        std::fs::write(root.join("s1/sweep_results.pb"), study("other", vec![sample(0, 0)]).encode_to_vec()).unwrap();
        let before = tree(&root);

        assert_collision(&store.get_study("s1").unwrap_err(), "s1", "other", "s1");
        assert_collision(&store.put_study(&study("s1", vec![sample(0, 0)])).unwrap_err(), "s1", "other", "s1");
        assert_eq!(tree(&root), before, "nothing written");
        assert_eq!(store.list_studies().unwrap().iter().map(|s| s.sweep_id.as_str()).collect::<Vec<_>>(), vec!["good"], "skipped, not listed under its stored id");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The racing-writer path (the rename failed) checks case variants too, and still answers as
    /// before for an exact existing study and for a directory that holds none. Called directly,
    /// because the window between `put_study`'s check and its rename is not one a test can hold open.
    #[test]
    fn the_failed_rename_path_checks_case_variants_too() {
        let root = temp_dir("failed-rename");
        let mut store = FileStudyStore::new(&root);
        let first = study("study", vec![sample(0, 0)]);
        store.put_study(&first).unwrap();
        let failure = || std::io::Error::other("rename failed");

        let err = store.settle_failed_rename(&root.join("Study"), &study("Study", vec![sample(0, 0)]), failure()).unwrap_err();
        assert_collision(&err, "Study", "study", "study");
        let same = store.settle_failed_rename(&root.join("study"), &first, failure()).expect("identical content is Ok");
        assert!(same.ends_with("study"), "{same}");
        let different = store.settle_failed_rename(&root.join("study"), &changed(first.clone()), failure()).unwrap_err();
        assert_refused_different(&different, "study", &first, &changed(first.clone()));
        let no_study = store.settle_failed_rename(&root.join("nothere"), &study("nothere", vec![sample(0, 0)]), failure()).unwrap_err();
        assert!(matches!(&no_study, StoreError::Io { path, .. } if path.ends_with("nothere")), "{no_study:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Writers racing on `study` and `Study` (identical content per spelling): on a
    /// case-insensitive filesystem exactly one spelling wins, every writer of the other gets the
    /// typed collision (at the check, or at the failed rename), and one directory is left. On a
    /// case-sensitive one the window between check and rename is open (module doc), so only the
    /// per-writer outcomes are asserted.
    #[test]
    fn racing_case_variants_leave_one_spelling_on_a_case_insensitive_filesystem() {
        for round in 0..20 {
            let root = temp_dir(&format!("race-case-{round}"));
            let insensitive = is_case_insensitive(&root);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8usize)
                .map(|n| {
                    let (root, barrier) = (root.clone(), barrier.clone());
                    let id = if n % 2 == 0 { "study" } else { "Study" };
                    std::thread::spawn(move || {
                        let results = study(id, vec![sample(0, 0)]);
                        barrier.wait();
                        (id, FileStudyStore::new(&root).put_study(&results))
                    })
                })
                .collect();
            let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let mut winning: Vec<&str> = outcomes.iter().filter(|(_, r)| r.is_ok()).map(|(id, _)| *id).collect();
            winning.dedup();
            for (id, outcome) in &outcomes {
                if let Err(e) = outcome {
                    let other = if *id == "study" { "Study" } else { "study" };
                    assert_collision(e, id, other, other);
                }
            }
            if insensitive {
                assert!(winning.windows(2).all(|w| w[0] == w[1]) && !winning.is_empty(), "round {round}: one spelling wins: {winning:?}");
                let names = dir_names(&root);
                assert_eq!(names.len(), 1, "round {round}: one directory, no staging: {names:?}");
                assert_eq!(names[0], winning[0]);
            }
            let _ = std::fs::remove_dir_all(&root);
        }
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

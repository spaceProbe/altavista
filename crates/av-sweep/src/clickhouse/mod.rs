//! The ClickHouse implementation of the sweep study store (question 245; the decision this
//! replaces is question 191's "a trait with a file-backed implementation now and ClickHouse
//! later").
//!
//! [`ClickHouseStudyStore`] implements [`crate::store::StudyStore`] over ClickHouse's HTTP
//! interface (`client`, built on `av-http1`), in `RowBinary` (`rowbinary`), against explicit
//! columnar tables (`schema`, `rows`), and answers the MoE query in SQL (`moe`).
//!
//! # Decisions
//!
//! 1. **A study's write is all-or-nothing to readers, without transactions.** ClickHouse has no
//!    multi-statement transactions. Every row of one write carries a fresh random `write_id`; the
//!    study's *header* row (`av_sweep_studies`) is inserted **last** and names that write id and
//!    the SHA-256 of the study's canonical protobuf encoding; a reader finds a study through its
//!    header and reads only rows with the header's `write_id`. A writer that crashes (or whose
//!    server refuses a later insert) leaves orphan rows nobody can see; `tests/clickhouse_store.rs`
//!    proves it by inserting a write's rows without its header. Orphans are never read; reclaiming
//!    them is an operator job (`ALTER TABLE ... DELETE WHERE write_id = ...`), not done here.
//! 2. **Studies are immutable, as in [`crate::store::FileStudyStore`] (question 252; the file
//!    store used to overwrite).** `put_study` of a `sweep_id` that exists with byte-identical content (equal
//!    SHA-256 of the canonical encoding, so NaN payloads and an optional field's `-0.0` count; a
//!    plain proto3 `double` of `-0.0` is not encoded, exactly like `0.0`, so there it does not)
//!    is an idempotent `Ok`;
//!    with different content it is [`StoreError::RefusedWrite`]. A sweep id names one hashed
//!    sweep's results; silently replacing them under readers would break the "reproducible from
//!    the hash" promise. Two writers racing on one id are not serialized (there is nothing to
//!    serialize on): the reader's winner is the header with the smallest `(created_at,
//!    write_id)`, and a writer re-reads the winner after inserting its header and reports
//!    [`StoreError::RefusedWrite`] if it lost with different content.
//!    **Two phases (question 254 item 2).** `put_study` is `prepare` then `commit`. `prepare`
//!    re-checks the content against any existing header and inserts every row *except* the header
//!    under a fresh write id (invisible orphans, exactly what a crashed writer leaves); `commit`
//!    inserts the header, the one-row commit point, and re-reads the winner. That re-read also
//!    resolves an insert whose reply was lost: our header present is committed, absent is not. The
//!    re-read (a read-only `SELECT`) is attempted up to [`REREAD_ATTEMPTS`] = 3 times with a fixed
//!    [`REREAD_BACKOFF`] = 200 ms between attempts (at most 400 ms of added sleeping); the header
//!    `INSERT` is never retried. When the insert errors *and* every re-read attempt fails (or the
//!    insert was acknowledged and every attempt fails) the outcome is
//!    [`StoreError::CommitOutcomeUnknown`], naming the write id; re-running the same write
//!    converges. `abort` has nothing to undo (orphans stay invisible). The
//!    coordinator that writes ClickHouse and the file store together is [`crate::study_write`].
//!    `check_plan` reads the whole existing study (verified against its recorded hash, so a
//!    tampered one is `Corrupt`, not accepted) and compares it with the plan ([`crate::plan`]).
//! 3. **The read-back is verified.** `get_study` checks the row counts against the header and the
//!    SHA-256 of the rebuilt message against the header's, so a reply cut off mid-stream or a
//!    server that changed a value is [`StoreError::Corrupt`], never a shorter or altered study.
//! 4. **Opening a store verifies the live schema** (`schema::verify`) before any write, and
//!    migrates only a store that is missing tables; a table whose columns differ is
//!    [`StoreError::SchemaMismatch`], never migrated over. A user who can only read can therefore
//!    still open an already-migrated store.
//!
//! # What is not here
//!
//! No retry, no connection pool, no `ReplacingMergeTree` dedup, no TTL, no sharding, no
//! `ON CLUSTER`: a single-node MergeTree schema, exactly what the docker-pinned image runs.

mod client;
mod moe;
mod rowbinary;
mod rows;
pub mod schema;

use av_cdm::pb;
use prost::Message;

use crate::hash::sha256_hex;
use crate::plan::StudyPlan;
use crate::store::{plan_refusal, PreparedState, PreparedWrite, StoreError, StoreKind, StudyStore, StudySummary, WriteRefusal};

pub use client::{escape_param, ChClient, ClickHouseConfig, ServerException};
pub use rowbinary::{Decoder, Encoder, RowBinaryError};
pub use schema::SchemaState;

use rows::{AssembleError, Header};

/// The winner re-read after a header insert ([`StudyStore::commit`]) is attempted this many times
/// in all before the commit's outcome is declared unknown. Only that read-only `SELECT` is
/// retried; the header `INSERT` never is.
pub const REREAD_ATTEMPTS: u32 = 3;
/// The fixed pause between two attempts of the winner re-read. Worst case added latency before
/// `CommitOutcomeUnknown`: `(REREAD_ATTEMPTS - 1) * REREAD_BACKOFF` = 400 ms of sleeping, plus
/// the time the failing attempts themselves take (each bounded by the client's timeouts).
pub const REREAD_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

/// The ClickHouse-backed [`StudyStore`].
#[derive(Debug, Clone)]
pub struct ClickHouseStudyStore {
    client: ChClient,
}

fn new_write_id() -> String {
    let mut bytes = [0u8; 16];
    openssl::rand::rand_bytes(&mut bytes).expect("OpenSSL's CSPRNG is available");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of the study's canonical protobuf encoding: the identity two copies of a study are
/// compared by. `SweepResults` encodes its maps from `BTreeMap`s (sorted), so this is
/// deterministic, and it is bit-sensitive where a value is encoded: NaN payloads, sample order and
/// an optional field's `-0.0` all change it. A plain proto3 `double` equal to `0.0` -- `-0.0`
/// included, since `-0.0 == 0.0` -- is not encoded at all, so there `-0.0` and `0.0` hash the same
/// (question 252; pinned by `store.rs`'s `identity_is_the_canonical_encodings_bits_like_clickhouses`).
pub fn content_sha256(results: &pb::SweepResults) -> String {
    sha256_hex(&results.encode_to_vec())
}

impl ClickHouseStudyStore {
    /// Connects, verifies the live schema and, when tables are missing, migrates and verifies
    /// again. See the module doc, decision 4.
    pub fn connect(config: &ClickHouseConfig) -> Result<Self, StoreError> {
        let store = Self::connect_unverified(config)?;
        match store.verify_schema()? {
            SchemaState::Ready => {}
            SchemaState::Incomplete { .. } => {
                store.migrate()?;
                match store.verify_schema()? {
                    SchemaState::Ready => {}
                    SchemaState::Incomplete { missing } => {
                        return Err(store.client.corrupt("verifying the schema after migrating", format!("tables still missing after the migration ran: {}", missing.join(", "))));
                    }
                }
            }
        }
        Ok(store)
    }

    /// Builds the client without touching the server (no verification, no migration). For tools
    /// and tests that manage the schema themselves.
    pub fn connect_unverified(config: &ClickHouseConfig) -> Result<Self, StoreError> {
        Ok(Self { client: ChClient::connect(config)? })
    }

    /// The raw client, for administration (creating users, `SHOW CREATE TABLE`) and tests.
    pub fn client(&self) -> &ChClient {
        &self.client
    }

    /// Creates every missing table and records the schema version. Idempotent.
    pub fn migrate(&self) -> Result<(), StoreError> {
        schema::migrate(&self.client)
    }

    /// Compares the live tables with the expected schema.
    pub fn verify_schema(&self) -> Result<SchemaState, StoreError> {
        schema::verify(&self.client)
    }

    fn location(&self, sweep_id: &str, write_id: &str) -> String {
        format!("{}/{}/{}#{}", self.client.url(), self.client.database(), sweep_id, write_id)
    }

    /// The header of the study `sweep_id`, or `None`. With several headers (a lost writer race)
    /// the winner is the smallest `(created_at, write_id)`.
    fn winning_header(&self, sweep_id: &str) -> Result<Option<Header>, StoreError> {
        let sql = format!(
            "SELECT {} FROM `{}` WHERE sweep_id = {{sid:String}} ORDER BY created_at, write_id LIMIT 1 FORMAT RowBinary",
            rows::HEADER_SELECT,
            schema::STUDIES
        );
        let op = "reading the study header";
        let bytes = self.client.query(&sql, &[("sid", sweep_id)], op)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut d = Decoder::new(&bytes);
        let header = rows::decode_header(&mut d).map_err(|e| self.client.corrupt(op, e.to_string()))?;
        d.finish().map_err(|e| self.client.corrupt(op, e.to_string()))?;
        Ok(Some(header))
    }

    /// [`Self::winning_header`], attempted up to [`REREAD_ATTEMPTS`] times with a fixed
    /// [`REREAD_BACKOFF`] between attempts; the last error is returned if every attempt failed.
    fn winning_header_retrying(&self, sweep_id: &str) -> Result<Option<Header>, StoreError> {
        let mut attempt = 1;
        loop {
            match self.winning_header(sweep_id) {
                Err(_) if attempt < REREAD_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(REREAD_BACKOFF);
                }
                other => return other,
            }
        }
    }

    fn select_rows(&self, table: &str, columns: &str, order: &str, sweep_id: &str, write_id: &str, op: &str) -> Result<Vec<u8>, StoreError> {
        let sql = format!("SELECT {columns} FROM `{table}` WHERE sweep_id = {{sid:String}} AND write_id = {{wid:String}} ORDER BY {order} FORMAT RowBinary");
        self.client.query(&sql, &[("sid", sweep_id), ("wid", write_id)], op)
    }

    fn assemble_err(&self, op: &str, e: AssembleError) -> StoreError {
        match e {
            AssembleError::Decode(e) => self.client.corrupt(op, e.to_string()),
            AssembleError::Inconsistent(text) => self.client.corrupt(op, text),
        }
    }

    /// Reads the study written by `header`.
    fn read_study(&self, sweep_id: &str, header: Header) -> Result<pb::SweepResults, StoreError> {
        let wid = header.write_id.as_str();
        let op = "reading the study's samples";
        let bytes = self.select_rows(schema::SAMPLES, rows::SAMPLES_SELECT, "ordinal", sweep_id, wid, op)?;
        let mut samples = rows::decode_samples(&bytes, header.sample_count).map_err(|e| self.assemble_err(op, e))?;

        let op = "reading the samples' axis values";
        let bytes = self.select_rows(schema::AXIS_VALUES, rows::AXIS_SELECT, "sample_ordinal, key", sweep_id, wid, op)?;
        rows::decode_axis_values(&bytes, &mut samples).map_err(|e| self.assemble_err(op, e))?;

        let op = "reading the samples' seeds";
        let bytes = self.select_rows(schema::SEEDS, rows::SEEDS_SELECT, "sample_ordinal, key", sweep_id, wid, op)?;
        rows::decode_seeds(&bytes, &mut samples).map_err(|e| self.assemble_err(op, e))?;

        let op = "reading the samples' scores";
        let bytes = self.select_rows(schema::SCORES, rows::SCORES_SELECT, "sample_ordinal, key", sweep_id, wid, op)?;
        rows::decode_scores(&bytes, &mut samples).map_err(|e| self.assemble_err(op, e))?;

        let op = "reading the study's aggregates";
        let bytes = self.select_rows(schema::AGGREGATES, rows::AGGREGATES_SELECT, "ordinal", sweep_id, wid, op)?;
        let aggregates = rows::decode_aggregates(&bytes, header.aggregate_count).map_err(|e| self.assemble_err(op, e))?;

        let provenance = match header.provenance {
            None => None,
            Some(mut p) => {
                let op = "reading the provenance attributes";
                let bytes = self.select_rows(schema::PROVENANCE_ATTRIBUTES, rows::ATTRIBUTES_SELECT, "key", sweep_id, wid, op)?;
                p.attributes = rows::decode_provenance_attributes(&bytes).map_err(|e| self.assemble_err(op, e))?;
                Some(p)
            }
        };

        let results = pb::SweepResults { sweep_id: sweep_id.to_string(), sweep_hash: header.sweep_hash, drm_hash: header.drm_hash, samples, aggregates, provenance };
        let rebuilt = content_sha256(&results);
        if rebuilt != header.content_sha256 {
            return Err(self.client.corrupt(
                "verifying the study read back",
                format!("study {sweep_id:?} write {wid}: the rows read back hash to {rebuilt}, but the header recorded {}", header.content_sha256),
            ));
        }
        Ok(results)
    }

    fn insert_chunks(&self, table: &str, chunks: Vec<Vec<u8>>, op: &str) -> Result<(), StoreError> {
        let cols = schema::table(table).insert_columns();
        for chunk in chunks {
            self.client.insert_rowbinary(table, &cols, chunk, op)?;
        }
        Ok(())
    }

    /// Inserts every row of the encoded study except its header (invisible until the header
    /// lands, decision 1) and returns the header's `RowBinary` row for [`Self::insert_header`].
    fn insert_rows_without_header(&self, enc: rows::EncodedStudy) -> Result<Vec<u8>, StoreError> {
        self.insert_chunks(schema::SAMPLES, enc.samples, "inserting samples")?;
        self.insert_chunks(schema::AXIS_VALUES, enc.axis_values, "inserting axis values")?;
        self.insert_chunks(schema::SEEDS, enc.seeds, "inserting seeds")?;
        self.insert_chunks(schema::SCORES, enc.scores, "inserting scores")?;
        self.insert_chunks(schema::AGGREGATES, enc.aggregates, "inserting aggregates")?;
        self.insert_chunks(schema::PROVENANCE_ATTRIBUTES, enc.provenance_attributes, "inserting provenance attributes")?;
        Ok(enc.header)
    }

    /// The commit point: the header names the write id, so it goes in last; until it lands,
    /// nothing can read any of the rows above.
    fn insert_header(&self, header: Vec<u8>) -> Result<(), StoreError> {
        self.insert_chunks(schema::STUDIES, vec![header], "inserting the study header")
    }

    /// Inserts every row of `results` under `write_id`, header last unless `with_header` is false.
    fn write_rows(&self, results: &pb::SweepResults, write_id: &str, with_header: bool) -> Result<(), StoreError> {
        let enc = rows::encode_study(results, write_id, &content_sha256(results));
        let header = self.insert_rows_without_header(enc)?;
        if with_header {
            self.insert_header(header)?;
        }
        Ok(())
    }

    /// A server exception on an insert is the typed [`WriteRefusal::Server`]; anything else is
    /// passed through.
    fn refused_insert(sweep_id: &str, e: StoreError) -> StoreError {
        match e {
            StoreError::Server { exception, .. } => StoreError::RefusedWrite {
                sweep_id: sweep_id.to_string(),
                reason: WriteRefusal::Server { code: exception.code, name: exception.name, http_status: exception.http_status, message: exception.message },
            },
            other => other,
        }
    }

    fn different_content(sweep_id: &str, existing: &Header, new_sha256: &str) -> StoreError {
        StoreError::RefusedWrite {
            sweep_id: sweep_id.to_string(),
            reason: WriteRefusal::DifferentContent { existing_write_id: existing.write_id.clone(), existing_sha256: existing.content_sha256.clone(), new_sha256: new_sha256.to_string() },
        }
    }

    /// Inserts a study's rows under a fresh write id **with or without its header** and returns
    /// the write id, skipping `put_study`'s existence check. `with_header = false` is exactly the
    /// state a writer that crashed before its last insert leaves (the orphan-invisibility proof);
    /// `with_header = true` on an id that already has a header is a second writer that lost a race
    /// (the winner-selection proof). Test seam; no production path calls it.
    #[doc(hidden)]
    pub fn write_rows_for_tests(&self, results: &pb::SweepResults, with_header: bool) -> Result<String, StoreError> {
        let write_id = new_write_id();
        self.write_rows(results, &write_id, with_header)?;
        Ok(write_id)
    }

    /// Computes every `(point, score)` aggregate of the stored study `sweep_id` **in SQL**, with
    /// [`crate::aggregate::aggregate`]'s rules; see the `moe` module for the query and why it is
    /// an order-fixed fold. `Ok(None)` when no such study exists.
    pub fn aggregates_in_sql(&self, sweep_id: &str) -> Result<Option<Vec<pb::ScoreAggregate>>, StoreError> {
        let Some(header) = self.winning_header(sweep_id)? else { return Ok(None) };
        let op = "computing the aggregates in SQL";
        let bytes = self.client.query(&moe::aggregates_sql(), &[("sid", sweep_id), ("wid", &header.write_id)], op)?;
        let mut d = Decoder::new(&bytes);
        let mut out = Vec::new();
        // Rows arrive sorted (point_index, name), so the first `mixed` row is the pair
        // `aggregate()` would have refused first.
        let mut first_mixed: Option<(String, u32)> = None;
        while !d.is_done() {
            let row = (|| {
                Ok::<_, RowBinaryError>((d.string()?, d.u32()?, d.u64()?, d.f64()?, d.f64()?, d.f64()?, d.f64()?, d.nullable_f64()?, d.bool()?))
            })();
            let (name, point_index, n, mean, std_dev, min, max, pass_fraction, mixed) = row.map_err(|e| self.client.corrupt(op, e.to_string()))?;
            if mixed {
                first_mixed.get_or_insert((name, point_index));
                continue;
            }
            out.push(pb::ScoreAggregate { name, point_index, draws: n as u32, mean, std_dev, min, max, pass_fraction });
        }
        if let Some((name, point_index)) = first_mixed {
            return Err(StoreError::Aggregation(crate::error::SweepError::MixedPassCriterion { name, point_index }));
        }
        Ok(Some(out))
    }
}

/// The private token of a study staged by [`ClickHouseStudyStore`]'s `prepare`: its rows are in
/// the tables under `write_id`, its header row (not yet inserted) is `header`.
struct ChStage {
    write_id: String,
    sha256: String,
    header: Vec<u8>,
}

impl StudyStore for ClickHouseStudyStore {
    fn kind(&self) -> StoreKind {
        StoreKind::ClickHouse
    }

    fn check_plan(&self, plan: &StudyPlan) -> Result<(), StoreError> {
        let Some(header) = self.winning_header(&plan.sweep_id)? else { return Ok(()) };
        let write_id = header.write_id.clone();
        let existing = self.read_study(&plan.sweep_id, header)?;
        match plan.first_difference(&existing) {
            None => Ok(()),
            Some(difference) => Err(plan_refusal(plan, write_id, &existing, difference)),
        }
    }

    /// Inserts every row except the header under a fresh write id: invisible to every reader
    /// (decision 1), so a prepare that is never committed leaves only orphans. An identical study
    /// already stored writes nothing (so a read-only user can still prepare it).
    fn prepare(&mut self, results: &pb::SweepResults) -> Result<PreparedWrite, StoreError> {
        let sweep_id = results.sweep_id.as_str();
        let sha256 = content_sha256(results);
        if let Some(existing) = self.winning_header(sweep_id)? {
            return if existing.content_sha256 == sha256 { Ok(PreparedWrite::existing(sweep_id, self.location(sweep_id, &existing.write_id))) } else { Err(Self::different_content(sweep_id, &existing, &sha256)) };
        }
        let write_id = new_write_id();
        let enc = rows::encode_study(results, &write_id, &sha256);
        let header = self.insert_rows_without_header(enc).map_err(|e| Self::refused_insert(sweep_id, e))?;
        Ok(PreparedWrite::staged(sweep_id, ChStage { write_id, sha256, header }))
    }

    /// The commit point (decision 1): insert the header, then re-read the winner, which also
    /// decides an insert whose reply was lost. A concurrent writer may have landed a header first;
    /// the reader's winner decides:
    ///
    /// - the winner has our content: `Ok` (whether our header or an identical one is the winner;
    ///   when the insert itself failed but an identical study is there, the study is stored);
    /// - the winner has other content: [`WriteRefusal::DifferentContent`];
    /// - no header at all: the insert's own error (it did not land), or `Corrupt` if it was
    ///   acknowledged;
    /// - the re-read fails every one of its [`REREAD_ATTEMPTS`] attempts:
    ///   [`StoreError::CommitOutcomeUnknown`], the one outcome in which the study may or may not
    ///   be committed. The re-read alone is retried (a read-only `SELECT`, so retrying it cannot
    ///   change anything); the header `INSERT` is never retried.
    fn commit(&mut self, prepared: PreparedWrite) -> Result<String, StoreError> {
        let sweep_id = prepared.sweep_id.clone();
        let ChStage { write_id, sha256, header } = match prepared.into_token::<ChStage>() {
            Ok(stage) => stage,
            Err(PreparedWrite { state: PreparedState::Existing { location }, .. }) => return Ok(location),
            Err(other) => panic!("ClickHouseStudyStore::commit was given a write another store prepared: {other:?}"),
        };
        let inserted = self.insert_header(header).map_err(|e| Self::refused_insert(&sweep_id, e));
        match (self.winning_header_retrying(&sweep_id), inserted) {
            (Ok(Some(winner)), _) if winner.content_sha256 == sha256 => Ok(self.location(&sweep_id, &winner.write_id)),
            (Ok(Some(winner)), _) => Err(Self::different_content(&sweep_id, &winner, &sha256)),
            (Ok(None), Ok(())) => Err(self.client.corrupt("verifying the write", format!("study {sweep_id:?}: the header just inserted is not visible"))),
            (Ok(None), Err(insert_error)) => Err(insert_error),
            (Err(reread), inserted) => Err(StoreError::CommitOutcomeUnknown {
                endpoint: self.client.url().to_string(),
                sweep_id,
                write_id,
                insert: match inserted {
                    Ok(()) => "was acknowledged".to_string(),
                    Err(e) => format!("failed: {e}"),
                },
                reread: format!("{reread} (after {REREAD_ATTEMPTS} attempts, {} ms apart)", REREAD_BACKOFF.as_millis()),
            }),
        }
    }

    /// Nothing to undo: the staged rows have no header, so they are invisible orphans (decision
    /// 1); reclaiming them is an operator job.
    fn abort(&mut self, _prepared: PreparedWrite) -> Result<(), StoreError> {
        Ok(())
    }

    fn get_study(&self, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
        match self.winning_header(sweep_id)? {
            None => Ok(None),
            Some(header) => self.read_study(sweep_id, header).map(Some),
        }
    }

    fn list_studies(&self) -> Result<Vec<StudySummary>, StoreError> {
        let sql = format!("SELECT sweep_id, sweep_hash, sample_count FROM `{}` ORDER BY sweep_id, created_at, write_id FORMAT RowBinary", schema::STUDIES);
        let op = "listing studies";
        let bytes = self.client.query(&sql, &[], op)?;
        let mut d = Decoder::new(&bytes);
        let mut out: Vec<StudySummary> = Vec::new();
        while !d.is_done() {
            let row = (|| Ok::<_, RowBinaryError>((d.string()?, d.string()?, d.u64()?)))();
            let (sweep_id, sweep_hash, sample_count) = row.map_err(|e| self.client.corrupt(op, e.to_string()))?;
            // Rows arrive sorted (sweep_id, created_at, write_id): the first row of each id is the winner.
            if out.last().is_some_and(|s| s.sweep_id == sweep_id) {
                continue;
            }
            out.push(StudySummary { sweep_id, sweep_hash, sample_count: sample_count as usize });
        }
        Ok(out)
    }
}

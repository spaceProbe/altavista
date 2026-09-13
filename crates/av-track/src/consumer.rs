//! A `spoore_io::kafka::PartitionConsumer`-shaped reader over `av_ingest::log::
//! PartitionLog` (question 200(d), `docs/edge-plan.md` milestone E5).
//!
//! # There is no consumer trait in `spoore-io` -- confirmed, not assumed
//!
//! `docs/edge-plan.md`'s E5 milestone says the engine reads accepted measurements "through
//! spoore-io's consumer trait". Reading `/Users/probe/code/spoore/crates/spoore-io/src/
//! kafka.rs` directly: `PartitionConsumer` (that file, `struct PartitionConsumer { inner:
//! BaseConsumer<DefaultConsumerContext> }`) is a concrete struct wrapping `rdkafka`'s own
//! `BaseConsumer` -- not a trait, and not something a second, non-Kafka backend could ever
//! implement, because nothing about it is abstracted: `PartitionConsumer::new` dials real
//! brokers over TCP, and `poll_measurement`/`poll_track_update`/`poll_track_handoff` all
//! call `self.inner.poll(timeout)` on that live connection. There is no `trait
//! PartitionConsumer` anywhere in `spoore-io`, `spoore-cdm`, or any other spoore crate this
//! workspace can see. Question 200(d) is the actual governing decision here: "the durable
//! log is file-backed ... until Redpanda's licence question (6) is settled; the consumer
//! trait is spoore-io's so the broker can replace the files" -- read together with the
//! milestone text, the intent is clearly "build against a shape a broker consumer could
//! later satisfy," not "there already exists a trait to implement." This module is this
//! crate's answer to that intent.
//!
//! # What this module does instead
//!
//! [`MeasurementConsumer`] is a small trait **defined in this crate** (not in `spoore-io`,
//! which this crate does not depend on at all -- see `crate`'s own module doc) whose one
//! method mirrors `spoore_io::kafka::PartitionConsumer::poll_measurement`'s signature and
//! semantics as closely as is honest for a file-backed log rather than a live broker
//! connection: no timeout parameter (a file read never blocks waiting for new data the way
//! a broker poll does -- see "Where this necessarily differs" below), returning `Option<
//! Received<pb::Measurement>>` exactly like the Kafka version returns `Option<Received<
//! spoore_cdm::Measurement>>` (this crate's `Received<T>` is a plain re-statement of
//! `spoore_io::kafka::Received<T>`'s three fields, differing only in `partition`'s type --
//! see below). [`StartOffset`] is a verbatim restatement of `spoore_io::kafka::
//! StartOffset`'s two variants and their meaning. [`LogPartitionConsumer::open`] takes an
//! explicit partition (`shard_key`, this platform's own partition identity -- `crate::log`'s
//! module doc: "a partition (`shard_key`)") exactly as `PartitionConsumer::new` takes an
//! explicit `&[i32]` of partitions to assign; nothing in this module ever commits an
//! offset (there is no `commit` method at all, matching `spoore_io::kafka::
//! PartitionConsumer`'s own "`enable.auto.commit=false` and nothing in this type ever
//! commits" contract verbatim); replay is by explicit offset ([`LogPartitionConsumer::
//! seek`]), never by any hidden position.
//!
//! ADR-004's discipline this mirrors: the log is the ledger, so a consumer never trusts
//! its own decode without re-checking the ledger's own chain first. [`LogPartitionConsumer::
//! open`] verifies the *whole* partition's record-hash chain (`PartitionLog::verify`,
//! `crates/av-ingest/src/log.rs`'s own second, independent chain -- see that module's
//! "Two chains, deliberately" section) before this consumer will hand out even the first
//! measurement; a partition whose chain does not verify is refused outright
//! ([`ConsumerError::ChainBroken`]), never partially served up to the point of the break --
//! there is no way to construct a [`LogPartitionConsumer`] over a tampered partition at
//! all, so "never yields a measurement from it" is true by construction, not by a runtime
//! check this module could forget to make on some code path.
//!
//! # Where this necessarily differs from `spoore_io::kafka::PartitionConsumer`, and why
//!
//! 1. **`partition` is a `String` (the `shard_key`), not an `i32`.** Kafka partitions are
//!    numbered within one named topic; `av_ingest::log::PartitionLog` partitions are named
//!    directly by `shard_key` (one file per key, `crate::log::sanitize_shard_key`) with no
//!    numbering at all. Restating this as a synthetic `i32` would invent an ordering
//!    (`shard_key`s have none) for no benefit; carrying the real key is the honest choice
//!    and costs a real caller nothing (a broker-backed implementation would still need to
//!    map `shard_key` to a Kafka partition index *somewhere*, and that mapping belongs to
//!    whichever component owns the topic layout, not to this trait's return type).
//! 2. **`offset` is a flat, 0-based *measurement* index within the partition, not a Kafka
//!    message offset.** `av_ingest::log::PartitionLog` stores one `MeasurementBatch` per
//!    record, and a batch can (and, for `BatchingRule::PerNMeasurements`, does) carry more
//!    than one `Measurement`; `spoore_io::wire::encode_measurement`/`decode_measurement`
//!    are one-measurement-per-Kafka-message, so `spoore_io`'s own `offset` is already a
//!    per-measurement index by construction and needs no flattening. This module flattens
//!    the same way: `offset` counts individual `Measurement`s in file order (batch 0's
//!    measurements first, in `measurements` order, then batch 1's, ...), so for this
//!    milestone's own fixture (`BatchingRule::PerEpoch`, exactly one measurement per batch)
//!    `offset` and "batch/record index" coincide, and the distinction only matters for a
//!    future source that batches more than one measurement per record. This is a
//!    deliberate, documented choice, not an accident of implementation -- see "Upstream
//!    delta" below for what a real Kafka-backed implementation would need to do here
//!    instead.
//! 3. **No `timeout` parameter, and `StartOffset::Latest` means "nothing," not "whatever
//!    arrives after this point."** A `PartitionLog` is a closed file as far as this
//!    consumer is concerned (it is opened, verified, and read once, up front, in
//!    [`LogPartitionConsumer::open`] -- see below); there is no live tail to wait on, so a
//!    timeout has nothing to wait *for* and `Latest` (Kafka's "live tail only") can only
//!    honestly mean "start after the last record that exists right now," which for a file
//!    already fully read is simply "yield nothing." A real broker-backed implementation of
//!    this same trait would restore both: a `timeout` on `poll_measurement` that actually
//!    blocks, and a `Latest` that means "new records only."
//! 4. **`poll_measurement` takes `&mut self`, not `&self`.** `spoore_io::kafka::
//!    PartitionConsumer::poll_measurement` takes `&self` because `librdkafka`'s
//!    `BaseConsumer::poll` is internally synchronised and advances the broker's own
//!    server-side position, not any state this Rust value owns. This module's cursor is
//!    this value's own field (there is no server to hold position for it), so advancing it
//!    is an ordinary mutation.
//! 5. **The whole partition is read and hash-verified eagerly in [`LogPartitionConsumer::
//!    open`], not polled lazily record by record.** This milestone's own fixture (900
//!    records) comfortably fits in memory, exactly the same sizing argument `crate::
//!    plugin::MeasurementSource`'s own module doc makes for a source's decoded
//!    measurements; a partition too large for that would need a streaming verify-then-
//!    decode pass instead (still possible over the same on-disk framing -- `crates/
//!    av-ingest/src/log.rs`'s own module doc: "a future reader needs nothing but this
//!    comment" -- just not built here, since nothing in this milestone's own fixture needs
//!    it).
//!
//! # Upstream delta: what `spoore-io` would need for a real trait implementation
//!
//! For [`MeasurementConsumer`] (or a trait shaped like it) to become something
//! `spoore_io::kafka::PartitionConsumer` genuinely *implements* -- so the exact same
//! calling code in [`crate::bridge::EngineBridge`] runs against a live broker later, per
//! question 200(d)'s own stated intent -- an upstream PR to `spoore-io` would need to:
//!
//! 1. **Extract a trait** from `PartitionConsumer`'s existing three `poll_*` methods (this
//!    module's own `poll_measurement` is the one this milestone needs; `poll_track_update`/
//!    `poll_track_handoff` would want the same treatment for symmetry, but are out of this
//!    milestone's scope). The natural shape, preserving `spoore_io`'s own semantics
//!    untouched: `fn poll_measurement(&self, timeout: Duration) -> Result<Option<
//!    Received<spoore_cdm::Measurement>>>` -- i.e. keep the timeout and the `&self`
//!    (`BaseConsumer::poll` already tolerates concurrent calls), and let *this* crate's
//!    file-backed implementation be the one that deviates (ignoring the timeout, since it
//!    has nothing to wait for -- deviation 3 above), not the other way around.
//! 2. **Decide what `partition`/`offset` mean generically.** `spoore_io::kafka::
//!    Received<T>.partition: i32` is Kafka-specific; a trait meant to run over both a
//!    broker and a file log needs either (a) a generic `partition: String` (this module's
//!    own choice -- a Kafka implementation would render its `i32` as a decimal string,
//!    losing nothing since nothing compares two partition ids arithmetically), or (b) an
//!    associated type `type PartitionId` per implementation, which is more honest but
//!    forces every caller (`crate::bridge::EngineBridge` included) to be generic over it.
//!    This module took (a) for its own simplicity; the PR would need the lead's call on
//!    which the trait itself should require.
//! 3. **Decide the flattening question (deviation 2 above) at the wire level, not the
//!    trait level.** The cleanest fix is upstream of the trait entirely: teach `av-edge`'s
//!    plugin/batch layer to emit one `MeasurementBatch` per `Measurement` when the target
//!    transport is Kafka-backed (`BatchingRule::PerNMeasurements(1)` already exists and
//!    does exactly this -- see `crate::plugin::BatchingRule`'s own doc), so a broker
//!    consumer's Kafka-native, one-message-per-offset semantics and this crate's own
//!    flat-measurement-index semantics coincide by construction, and neither
//!    implementation of the trait needs to know the other flattens differently.
//! 4. **A feature-gated or dual-crate split** so a caller that only ever reads a file log
//!    (this milestone) is not forced to depend on `rdkafka` (a C library binding, and a
//!    dependency this crate was explicitly asked to keep out of its own tree -- see
//!    `Cargo.toml`) just to name the trait. `spoore-io` already separates `kafka` and
//!    `clickhouse_sink` into distinct modules; splitting `PartitionConsumer`'s new trait
//!    into a dependency-free `spoore-io-core` (or similar) that both `spoore-io::kafka`'s
//!    concrete type and this crate's own `LogPartitionConsumer` could implement, with
//!    `spoore-io` itself re-exporting the trait, would let this crate depend on the trait
//!    alone rather than on `rdkafka` transitively.
//!
//! None of this is built here: this crate defines and uses its own trait, and reports this
//! delta as prose for the lead to turn into an upstream PR description (this milestone's
//! own instruction) rather than opening one against `/Users/probe/code/spoore` itself,
//! which this task must not modify.

use std::path::Path;

use av_edge::pb;
use av_ingest::log::{LogError, PartitionLog, RecoveryReport};

/// Mirrors `spoore_io::kafka::StartOffset` verbatim: two variants, the same names, the
/// same meaning for a live broker. See this module's own doc, point 3, for what `Latest`
/// honestly means against a closed file rather than a live tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOffset {
    /// From this partition's first record -- full replay, exactly `spoore_io::kafka::
    /// StartOffset::Earliest`.
    Earliest,
    /// Against a closed file, "whatever is produced after this consumer starts" can only
    /// mean "nothing, since nothing more will ever be produced into this file by this
    /// consumer's own view of it" -- see this module's doc, deviation 3.
    Latest,
}

/// Mirrors `spoore_io::kafka::Received<T>`'s three fields exactly, except `partition`'s
/// type -- see this module's doc, deviation 1, for why a `String` (this platform's own
/// `shard_key`) rather than Kafka's `i32`.
#[derive(Debug, Clone, PartialEq)]
pub struct Received<T> {
    pub value: T,
    pub partition: String,
    pub offset: u64,
}

/// What can go wrong opening or reading a partition through this consumer. Every variant
/// is a typed refusal, never a panic (this crate's standing convention, `av_edge::plugin::
/// PluginError`'s identical framing).
#[derive(Debug, thiserror::Error)]
pub enum ConsumerError {
    #[error(transparent)]
    Log(#[from] LogError),
    #[error("reading partition {partition:?} at {path}: {source}")]
    Io { partition: String, path: String, #[source] source: std::io::Error },
    /// [`LogPartitionConsumer::open`]'s own refusal when [`PartitionLog::verify`] reports
    /// the partition's record-hash chain does not verify -- "the log is the ledger," so a
    /// tampered ledger yields nothing, ever, from this consumer (this module's own doc,
    /// "What this module does instead").
    #[error("partition {partition:?}'s own record-hash chain does not verify at record {broken_at_sequence}: {detail} -- refusing to hand out any measurement from it")]
    ChainBroken { partition: String, broken_at_sequence: u64, detail: String },
    /// A record's payload failed to decode as `altavista.v1.MeasurementBatch` even though
    /// [`PartitionLog::verify`] reported the chain intact -- not expected to be reachable
    /// (verify's own hash check already makes an undetected content change
    /// cryptographically implausible), but refused explicitly rather than panicking if it
    /// ever is.
    #[error("record {record_index} of partition {partition:?} does not decode as altavista.v1.MeasurementBatch: {detail}")]
    Corrupt { partition: String, record_index: u64, detail: String },
}

/// Mirrors `spoore_io::kafka::PartitionConsumer`'s read surface -- see this module's own
/// doc for exactly how closely, and where it necessarily differs.
pub trait MeasurementConsumer {
    /// The next `Measurement`, in file order, or `None` once this partition (from
    /// wherever this consumer's own [`StartOffset`]/[`LogPartitionConsumer::seek`] started
    /// it) is exhausted. Mirrors `spoore_io::kafka::PartitionConsumer::poll_measurement`'s
    /// return shape exactly (`Option<Received<Measurement>>`); see this module's doc,
    /// deviation 3, for why there is no `timeout` parameter here.
    fn poll_measurement(&mut self) -> Option<Received<pb::Measurement>>;

    /// This consumer's own partition (`shard_key`).
    fn partition(&self) -> &str;

    /// Reposition this consumer's cursor to `offset` (a flat measurement index -- see this
    /// module's doc, deviation 2) for explicit replay, mirroring the "replay by explicit
    /// offset" discipline `spoore_io::kafka::PartitionConsumer::new`'s own `start:
    /// StartOffset` plus manual `assign()` embodies (ADR-004: nothing here commits, so a
    /// caller is always free to rewind). Clamped to this partition's own measurement
    /// count -- seeking past the end is not an error, it simply leaves the next
    /// [`MeasurementConsumer::poll_measurement`] returning `None`, exactly like polling an
    /// already-exhausted live partition would.
    fn seek(&mut self, offset: u64);

    /// How many measurements this partition holds in total, across every record.
    fn len(&self) -> usize;

    /// Whether this partition holds no measurements at all (`clippy::len_without_is_empty`
    /// -- a trait declaring `len` must also declare `is_empty`; a default body over `len`
    /// is honest here since no implementation could ever compute this more cheaply than
    /// `len() == 0`).
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One flattened `(record_index, measurement)` pair -- see this module's doc, deviation 2.
#[derive(Debug)]
struct FlatEntry {
    record_index: u64,
    measurement: pb::Measurement,
}

/// [`MeasurementConsumer`] implemented over a real, on-disk `av_ingest::log::PartitionLog`
/// -- this crate's whole reason to exist. See this module's own doc for the full contract.
#[derive(Debug)]
pub struct LogPartitionConsumer {
    partition: String,
    entries: Vec<FlatEntry>,
    cursor: usize,
}

impl LogPartitionConsumer {
    /// Which partition-file record (0-based) produced the measurement at flat `offset` --
    /// diagnostic-only, and the concrete reason this module's own doc, deviation 2, keeps
    /// `record_index` on [`FlatEntry`] at all: for this milestone's own fixture (one
    /// measurement per record) it always equals `offset` itself, but a future source
    /// batching more than one measurement per record would make the two diverge, and a
    /// caller debugging "which signed batch did this measurement actually arrive in"
    /// needs this, not just the flat count.
    pub fn record_index_of(&self, offset: u64) -> Option<u64> {
        self.entries.get(offset as usize).map(|e| e.record_index)
    }
}

/// Fixed per-record header size, restated from `crates/av-ingest/src/log.rs`'s own module
/// doc ("Record framing": `payload_len: u32 LE` + `record_hash: [u8; 32]`) rather than
/// imported, since that constant is private to that crate (`log::HEADER_LEN`) -- this is
/// the exact same duplication `crates/av-ingest/tests/plugin_wire.rs::
/// read_partition_payloads` already makes, for the identical reason: that module's own doc
/// says explicitly "a future reader needs nothing but this comment," and an independent
/// reader restating the documented framing (rather than depending on a private constant it
/// cannot even name) is precisely what that sentence is inviting.
const HEADER_LEN: usize = 4 + 32;

impl LogPartitionConsumer {
    /// Opens `shard_key`'s partition under `dir` (recovering a torn or corrupt trailing
    /// record exactly as [`PartitionLog::open`] already does -- this module invents no
    /// second recovery mechanism, per this milestone's own instruction: "the log already
    /// detects one ... do not invent a second one"), verifies its whole record-hash chain,
    /// and -- only if that chain is intact -- decodes and flattens every record's
    /// measurements, ready to be polled from `start`.
    ///
    /// # Errors
    ///
    /// [`ConsumerError::ChainBroken`] if [`PartitionLog::verify`] reports the chain does
    /// not verify -- this consumer is then never constructed at all, so there is no way to
    /// poll even one measurement out of a tampered partition. [`ConsumerError::Corrupt`]
    /// if a record's payload fails to decode despite an intact chain (not expected to be
    /// reachable -- see that variant's own doc). [`ConsumerError::Log`]/[`ConsumerError::
    /// Io`] for the underlying filesystem operation.
    ///
    /// Returns `Some(RecoveryReport)` alongside a successfully opened consumer exactly
    /// when [`PartitionLog::open`] itself had to discard a torn or corrupt trailing
    /// record -- surfaced, never swallowed (this crate's own convention, matching `crate::
    /// log::PartitionLog::open`'s own "never silently kept, never silently dropped"
    /// contract).
    pub fn open(dir: &Path, shard_key: &str, start: StartOffset) -> Result<(Self, Option<RecoveryReport>), ConsumerError> {
        let (log, recovery) = PartitionLog::open(dir, shard_key)?;

        let verification = log.verify()?;
        if !verification.ok {
            return Err(ConsumerError::ChainBroken {
                partition: shard_key.to_string(),
                broken_at_sequence: verification.broken_at_sequence,
                detail: verification.detail,
            });
        }

        let entries = decode_all_records(shard_key, log.path())?;
        let cursor = match start {
            StartOffset::Earliest => 0,
            StartOffset::Latest => entries.len(),
        };
        Ok((Self { partition: shard_key.to_string(), entries, cursor }, recovery))
    }
}

impl MeasurementConsumer for LogPartitionConsumer {
    fn poll_measurement(&mut self) -> Option<Received<pb::Measurement>> {
        if self.cursor >= self.entries.len() {
            return None;
        }
        let offset = self.cursor as u64;
        let entry = &self.entries[self.cursor];
        let value = entry.measurement.clone();
        self.cursor += 1;
        Some(Received { value, partition: self.partition.clone(), offset })
    }

    fn partition(&self) -> &str {
        &self.partition
    }

    fn seek(&mut self, offset: u64) {
        self.cursor = (offset as usize).min(self.entries.len());
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Independently reads and decodes every record in `path` (`crates/av-ingest/src/log.rs`'s
/// own documented byte framing -- 4-byte little-endian `payload_len`, 32 raw `record_hash`
/// bytes, then `payload_len` payload bytes, repeated to EOF), flattening every record's
/// `measurements` into one ordered list. Called only after [`PartitionLog::verify`] has
/// already confirmed the whole chain is intact, so a decode failure here is refused
/// ([`ConsumerError::Corrupt`]) rather than trusted, but is not expected to occur (see that
/// variant's own doc).
fn decode_all_records(partition: &str, path: &Path) -> Result<Vec<FlatEntry>, ConsumerError> {
    let bytes = std::fs::read(path).map_err(|source| ConsumerError::Io {
        partition: partition.to_string(),
        path: path.display().to_string(),
        source,
    })?;

    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut record_index = 0u64;
    let len = bytes.len();
    while pos < len {
        // `PartitionLog::open` (already run by the caller, `LogPartitionConsumer::open`)
        // has already truncated away any torn or corrupt trailing bytes, and `verify()`
        // has already confirmed every record's hash -- so `pos + HEADER_LEN <= len` and
        // `payload_end <= len` are guaranteed here for every record; this loop does not
        // re-derive that guarantee, it relies on it (the one place in this module that
        // trusts, rather than re-checks, `PartitionLog`'s own contract).
        let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().expect("4-byte slice")) as usize;
        let payload_start = pos + HEADER_LEN;
        let payload_end = payload_start + payload_len;
        let payload = &bytes[payload_start..payload_end];

        let batch = <pb::MeasurementBatch as prost::Message>::decode(payload).map_err(|e| ConsumerError::Corrupt {
            partition: partition.to_string(),
            record_index,
            detail: e.to_string(),
        })?;

        for measurement in batch.measurements {
            out.push(FlatEntry { record_index, measurement });
        }

        pos = payload_end;
        record_index += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use av_edge::pb::{Label, Measurement, MeasurementBatch, Provenance};
    use av_ingest::ingest::{Ingest, Signer};
    use av_edge::policy::ProducerPolicy;
    use openssl::ec::EcKey;

    const TEST_KEY_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pem");
    const TEST_PUB_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pub.pem");

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-track-consumer-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn build_and_append(dir: &Path, shard_key: &str, count: u64) {
        let signing_key = av_edge::sign::load_signing_key(TEST_KEY_PEM).unwrap();
        let verify_key: EcKey<openssl::pkey::Public> = av_edge::verify::load_verifying_key(TEST_PUB_PEM).unwrap();

        let mut ingest = Ingest::new(dir, None);
        ingest
            .register_producer(ProducerPolicy::new("test-producer", "CUI", vec![], vec!["CUI".to_string()], "CUI", 1_000_000_000_000).unwrap());

        let label = Label { marking: "CUI".to_string(), caveats: vec![] };
        let mut prev_hash = av_edge::hash::GENESIS.to_vec();
        for i in 0..count {
            let mut batch = MeasurementBatch {
                producer_id: "test-producer".to_string(),
                sequence: i + 1,
                label: Some(label.clone()),
                measurements: vec![Measurement {
                    measurement_id: "m".to_string(),
                    z: vec![i as f64, 0.0, 0.0],
                    r: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                    epoch_ns: i as i64,
                    sensor_id: "s".to_string(),
                    shard_key: shard_key.to_string(),
                    ..Default::default()
                }],
                provenance: Some(Provenance::default()),
                batch_tai_ns: i as i64,
                shard_key: shard_key.to_string(),
                ..Default::default()
            };
            av_edge::sign::sign_batch_with_signer(&mut batch, &prev_hash, &signing_key, "").unwrap();
            prev_hash = batch.batch_hash.clone();
            let outcome = ingest.submit(&batch, Signer::Key(verify_key.clone()), i as i64).unwrap();
            assert!(outcome.accepted(), "{outcome:?}");
        }
    }

    #[test]
    fn reads_every_measurement_in_order_with_flat_offsets() {
        let dir = tmp_dir("in-order");
        build_and_append(&dir, "shard-a", 5);

        let (mut consumer, recovery) = LogPartitionConsumer::open(&dir, "shard-a", StartOffset::Earliest).unwrap();
        assert!(recovery.is_none());
        assert_eq!(consumer.len(), 5);

        for i in 0..5u64 {
            let received = consumer.poll_measurement().unwrap();
            assert_eq!(received.offset, i);
            assert_eq!(received.partition, "shard-a");
            assert_eq!(received.value.z[0], i as f64);
        }
        assert!(consumer.poll_measurement().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_offset_latest_yields_nothing_against_a_closed_file() {
        let dir = tmp_dir("latest");
        build_and_append(&dir, "shard-a", 3);

        let (mut consumer, _) = LogPartitionConsumer::open(&dir, "shard-a", StartOffset::Latest).unwrap();
        assert_eq!(consumer.len(), 3);
        assert!(consumer.poll_measurement().is_none(), "StartOffset::Latest against a closed file must yield nothing (this module's own documented deviation)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seek_repositions_the_cursor_for_explicit_replay() {
        let dir = tmp_dir("seek");
        build_and_append(&dir, "shard-a", 5);

        let (mut consumer, _) = LogPartitionConsumer::open(&dir, "shard-a", StartOffset::Earliest).unwrap();
        consumer.seek(3);
        let received = consumer.poll_measurement().unwrap();
        assert_eq!(received.offset, 3);
        assert_eq!(received.value.z[0], 3.0);

        consumer.seek(0);
        let received = consumer.poll_measurement().unwrap();
        assert_eq!(received.offset, 0);

        consumer.seek(1000); // past the end -- clamped, not an error (this module's own doc)
        assert!(consumer.poll_measurement().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tampered_partition_is_refused_outright_and_never_yields_a_measurement() {
        let dir = tmp_dir("tamper");
        build_and_append(&dir, "shard-a", 5);

        // Corrupt one byte in the middle of the file's payload region (past the first
        // record's 36-byte header, so this lands inside real message content) -- the
        // exact scenario this milestone's own test list requires: "corrupt one byte in a
        // temp copy of a log and assert the typed refusal."
        let path = dir.join("shard-a.avlog");
        let mut bytes = std::fs::read(&path).unwrap();
        let flip_at = HEADER_LEN + 4; // inside the first record's encoded Measurement bytes.
        bytes[flip_at] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let err = LogPartitionConsumer::open(&dir, "shard-a", StartOffset::Earliest).unwrap_err();
        match err {
            ConsumerError::ChainBroken { partition, .. } => assert_eq!(partition, "shard-a"),
            other => panic!("expected ChainBroken, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

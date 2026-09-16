//! The library core of `src/bin/av-edge-latency.rs` -- D5a (`docs/p5-plan.md`'s D5 "three
//! placements" milestone; this module is the *measurement driver*, not the container run
//! itself, which a later worker performs). Moved out of `main` (question 148: a real proof
//! needs a real running process; here it additionally needs to be callable from a test with
//! no subprocess and no docker at all) rather than rewritten -- every function below is the
//! same code the binary used to run inline in `main`, unchanged in what it does, just now
//! callable from `tests/multi_target_harness.rs` too.
//!
//! # The two instants, restated once more, because nothing about them may drift
//!
//! `crate::latency`'s own module doc names them precisely; [`drive_placement`] is the *one*
//! place in this whole crate that still reads them, and reads them in exactly the same two
//! lines relative to `EdgeIngestClient::submit_batches` that `src/bin/av-edge-latency.rs`
//! always has: **emit** = `Instant::now()` immediately before `submit_batches(vec![batch.
//! clone()])`; **accept** = `Instant::now()` immediately after that call's `Result` is
//! available and confirmed `accepted`. Adding multi-placement pacing (see "Pacing" below)
//! only ever inserts a `sleep` *before* the emit read, never between emit and accept, and
//! never touches accept's own position at all -- the baseline (question 219, 2026-09-16
//! 04:05: p50 6.8 ms / p99 11.3 ms medians of five, 900 batches) and this module's own
//! zero-argument numbers are measuring the identical quantity.
//!
//! # Modes
//!
//! - **In-process** (no `--target`): `src/bin/av-edge-latency.rs`'s own `main` starts one
//!   real `EdgeIngestService` on an ephemeral loopback port (via [`spawn_in_process_server`])
//!   and drives it as a single placement -- **byte-for-byte the same behaviour this binary
//!   had before this module existed**: same fixture, same default 900 batches, same report
//!   shape (this module's own additive fields aside -- see `src/bin/av-edge-latency.rs`'s own
//!   module doc for the report contract). [`BASELINE_CONFIG`] pins exactly what zero
//!   arguments parses to.
//! - **Targets** (one or more `--target LABEL=ADDR`): drives each already-running, remote
//!   `av-ingest-server` concurrently (via [`drive_all`], one `tokio::spawn`ed task per
//!   placement -- genuine OS-thread-level concurrency on this crate's `rt-multi-thread`
//!   runtime, not a sequential `for` loop over placements) instead of starting anything
//!   in-process. No in-process server is ever started in this mode.
//!
//! # Pacing, and what happens when a placement cannot keep up
//!
//! `--rate` (measurements/s per placement, D5's own declared load -- spoore's budget,
//! question 41, is 1,000/s per shard) is converted to a nanosecond batch-send interval by
//! [`batch_interval_ns`], from the fixture's own **measured** batch size (never assumed --
//! see [`measured_measurements_per_batch`]). [`drive_placement`] then schedules batch `i`'s
//! send at a **fixed anchor**, `clock_zero + i * interval_ns` ([`scheduled_due_at_ns`]),
//! computed once from the placement's own start instant -- never as "sleep `interval_ns`
//! after the previous response arrived." This distinction is exactly what keeps a slow
//! placement from bursting: if batch `i` finishes late, batch `i+1`'s own due time was
//! already in the past the moment batch `i`'s response arrived, so `drive_placement` sends
//! it immediately (no sleep, since `Instant::now() >= due`) and moves on -- it never tries to
//! "catch up" by shortening a later interval or queuing several sends back to back. A
//! placement that cannot sustain the declared rate therefore degrades, batch by batch,
//! toward `AsFastAsPossible`-style back-to-back sends, and the shortfall this produces is
//! visible in the report's own achieved-vs-declared throughput field, never hidden by a
//! pacer that silently absorbs the delay or reappears as a burst later. This binary's own
//! clock is real (`Instant::now`, exactly as `crate::latency`'s module doc already states for
//! the single-placement path); `tokio::time::sleep` reading real wall time here is legitimate
//! for the same reason that module doc gives: this is a binary earning a measurement, not a
//! test (question 199's rule binds tests and library code, not this binary).
//!
//! No pacing at all (`--rate` omitted) behaves exactly as the original binary always did:
//! every batch sent as fast as `submit_batches` returns, no `sleep` call ever reached.
//!
//! # Asserts no threshold -- restated for the multi-placement path
//!
//! Exactly like the original single-placement binary, [`drive_placement`] fails loudly
//! (a typed [`HarnessError`], which `main` turns into a panic) on a connection failure, a
//! malformed verdict count, or an outright rejected batch -- but never on a *slow* accepted
//! one. There is no latency threshold anywhere in this module; a contended host produces
//! worse numbers in the report, not a different exit code.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use av_edge::pb;
use av_edge::plugin::{BatchBuilder, BatchingRule, Pacing, PluginConfig, PortTrafficSource};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::EdgeIngestClient;
use futures_util::stream::{self, StreamExt};
use openssl::ec::EcKey;
use openssl::pkey::Public;

use crate::latency::{summarize, LatencySample, LatencyReport};

// -------------------------------------------------------------------------------------------
// Argument parsing / declared configuration
// -------------------------------------------------------------------------------------------

/// The batch count `src/bin/av-edge-latency.rs` has always defaulted to, and the count the
/// lead's own baseline (question 219) was taken with -- `--batches` omitted reproduces this
/// exactly.
pub const BASELINE_BATCHES: usize = 900;

/// Zero arguments' own parsed configuration, named so `parse_args`'s own zero-argument test
/// can assert equality against it directly rather than re-deriving each field by hand. Every
/// field here is a plain, non-heap value for `Mode::InProcess` specifically, so this is a
/// `const`, not a `fn` -- the strongest available guarantee that "no arguments" has exactly
/// one, fixed, compile-time-checked meaning.
pub const BASELINE_CONFIG: HarnessConfig = HarnessConfig { mode: Mode::InProcess, batches: BASELINE_BATCHES, rate_per_sec: None, in_flight: 1 };

/// One remote placement's own driving address, from `--target LABEL=ADDR`. `label` is
/// whatever the caller chose (D5's own three placements: `edge-a`/`edge-b`/`edge-c`, from
/// `deploy/secdeploy/secsite.altavista-3.toml`); `addr` is a bare `"host:port"` string handed
/// straight to [`EdgeIngestClient::connect_plaintext`] (which itself refuses a non-loopback
/// address -- question 155/202, unchanged by this module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementTarget {
    pub label: String,
    pub addr: String,
}

/// This binary's two mutually exclusive modes -- structurally mutually exclusive (an enum,
/// not two independent booleans that could both be set at once): [`Mode::InProcess`] can
/// never carry a target, and [`Mode::Targets`] is never chosen with zero targets (an empty
/// `--target` list parses to [`Mode::InProcess`] instead -- see [`parse_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// No `--target` was given: `main` starts one real, in-process `EdgeIngestService` on an
    /// ephemeral loopback port and drives it as a single placement -- unchanged from this
    /// binary's original, pre-D5a behaviour.
    InProcess,
    /// One or more `--target` flags were given: `main` drives each already-running, remote
    /// `av-ingest-server` concurrently and never starts anything in-process. Never empty --
    /// [`parse_args`] folds the empty case into [`Mode::InProcess`] instead, so a caller
    /// matching this variant may assume at least one target.
    Targets(Vec<PlacementTarget>),
}

/// This run's own fully-parsed configuration -- [`parse_args`]'s own return type, and this
/// module's single source of truth for "what did the command line ask for."
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessConfig {
    pub mode: Mode,
    /// Batches *per placement*. Never more than the fixture's own total batch count --
    /// `src/bin/av-edge-latency.rs`'s own `main` refuses a larger value rather than
    /// fabricating batches the fixture never produced.
    pub batches: usize,
    /// The declared offered load, measurements/s per placement (D5 declares 1,000/s,
    /// matching spoore's budget -- question 41). `None` when `--rate` was omitted: every
    /// batch is sent as fast as `submit_batches` returns, exactly as this binary always did
    /// before this module existed.
    pub rate_per_sec: Option<f64>,
    /// Batches outstanding concurrently, per placement, sharing that placement's own
    /// schedule (question 148's diagnosed apparatus limit: the driver used to send one batch
    /// at a time per placement and await the response before sending the next, so the
    /// maximum achievable offered rate per placement was `1 / round-trip` -- a property of
    /// this measuring client, not of the platform under test). Default `1` -- **must
    /// reproduce today's behaviour exactly**: [`drive_placement`] keeps the original,
    /// unmodified sequential loop for `in_flight <= 1`, and only takes the concurrent
    /// (`futures_util::stream::StreamExt::buffer_unordered`) path for `in_flight > 1`, so
    /// the default path is not just behaviourally but *structurally* identical to the
    /// pre-`--in-flight` code. The emit/accept instants stay exactly where they always were
    /// -- immediately before `submit_batches` and immediately after its `Result` is
    /// confirmed -- for every batch regardless of `in_flight`, so raising it changes only how
    /// many batches are outstanding at once, never what a single batch's own latency sample
    /// measures.
    pub in_flight: usize,
}

const USAGE: &str = "usage: av-edge-latency [--target LABEL=ADDR]... [--rate MEASUREMENTS_PER_SEC] [--batches N] [--in-flight N]";

/// Parses this binary's own argument vector (including `argv[0]`, discarded exactly like
/// `av-ingest-server`'s own `parse_args` discards it) into a [`HarnessConfig`]. Pure: no I/O,
/// no clock read, callable from a test with a synthetic iterator -- exactly `av-ingest-
/// server`'s own `parse_args` precedent (`src/bin/av-ingest-server.rs`).
///
/// - `--target LABEL=ADDR` (repeatable): a literal `'='` separates `LABEL` from `ADDR`; both
///   must be non-empty, and a repeated `LABEL` is refused (the report keys placements by
///   label, so two placements under the same label would be ambiguous, never silently
///   overwritten).
/// - `--rate N`: `N` must parse as a finite, strictly positive `f64`.
/// - `--batches N`: `N` must parse as a strictly positive `usize` (`0` is refused -- there is
///   nothing to measure over zero batches).
///
/// Zero `--target` flags yields [`Mode::InProcess`]; one or more yields [`Mode::Targets`].
/// There is no separate "in-process" flag to conflict with `--target` -- the two modes are
/// mutually exclusive by construction (`Mode` is an enum, not independent flags), decided
/// solely by whether any `--target` was given.
pub fn parse_args(mut args: impl Iterator<Item = String>) -> Result<HarnessConfig, String> {
    let _argv0 = args.next();

    let mut targets: Vec<PlacementTarget> = Vec::new();
    let mut batches: Option<usize> = None;
    let mut rate: Option<f64> = None;
    let mut in_flight: Option<usize> = None;

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--target" => {
                let raw = value()?;
                let (label, addr) = raw.split_once('=').ok_or_else(|| format!("--target {raw:?} must have the shape LABEL=ADDR (a literal '=' separates them)"))?;
                if label.is_empty() {
                    return Err(format!("--target {raw:?}: LABEL must not be empty"));
                }
                if addr.is_empty() {
                    return Err(format!("--target {raw:?}: ADDR must not be empty"));
                }
                if targets.iter().any(|t| t.label == label) {
                    return Err(format!("--target {raw:?}: label {label:?} was already given by an earlier --target"));
                }
                targets.push(PlacementTarget { label: label.to_string(), addr: addr.to_string() });
            }
            "--rate" => {
                let raw = value()?;
                let parsed: f64 = raw.parse().map_err(|e| format!("--rate {raw:?} is not a valid number: {e}"))?;
                if !parsed.is_finite() || parsed <= 0.0 {
                    return Err(format!("--rate {raw:?} must be a positive, finite number of measurements/s"));
                }
                rate = Some(parsed);
            }
            "--batches" => {
                let raw = value()?;
                let parsed: usize = raw.parse().map_err(|e| format!("--batches {raw:?} is not a valid non-negative integer: {e}"))?;
                if parsed == 0 {
                    return Err("--batches 0 is not a valid batch count -- there is nothing to measure over zero batches".to_string());
                }
                batches = Some(parsed);
            }
            "--in-flight" => {
                let raw = value()?;
                let parsed: usize = raw.parse().map_err(|e| format!("--in-flight {raw:?} is not a valid positive integer: {e}"))?;
                if parsed == 0 {
                    return Err("--in-flight 0 is not valid -- at least one batch must be outstanding per placement".to_string());
                }
                in_flight = Some(parsed);
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }

    let mode = if targets.is_empty() { Mode::InProcess } else { Mode::Targets(targets) };
    Ok(HarnessConfig { mode, batches: batches.unwrap_or(BASELINE_BATCHES), rate_per_sec: rate, in_flight: in_flight.unwrap_or(1) })
}

// -------------------------------------------------------------------------------------------
// Rate -> batch cadence (pure arithmetic)
// -------------------------------------------------------------------------------------------

/// How many measurements the fixture's own batches actually carry, measured from the real,
/// already-built `batches` slice rather than assumed. `docs/edge-plan.md`'s own fixture uses
/// `BatchingRule::PerEpoch` over a one-packet-per-step port traffic log, so every batch this
/// fixture ever produces carries exactly one measurement (`crate::consumer`'s own module doc,
/// point 2, states this as fact about the fixture) -- but this function still measures it
/// rather than hard-coding `1`, so a future fixture change (a different `BatchingRule`, or a
/// port traffic log with more than one record per step) is reflected automatically.
/// `0` for an empty slice (nothing to measure a rate over).
pub fn measured_measurements_per_batch(batches: &[pb::MeasurementBatch]) -> usize {
    if batches.is_empty() {
        return 0;
    }
    let total: usize = batches.iter().map(|b| b.measurements.len()).sum();
    total / batches.len()
}

/// The nanosecond interval between successive batch sends needed to sustain
/// `rate_measurements_per_sec` measurements/s for one placement, given `measurements_per_batch`
/// measurements per batch (from [`measured_measurements_per_batch`] -- never assumed to be
/// `1`, even though this task's own fixture happens to measure `1`). Pure arithmetic, no
/// clock read: `1e9 * measurements_per_batch / rate_measurements_per_sec`, rounded to the
/// nearest nanosecond. `measurements_per_batch` is floored to `1` (a batch carrying zero
/// measurements has no sensible cadence of its own; nothing in this fixture ever produces
/// one, but this function stays total rather than panicking on a caller's degenerate input).
pub fn batch_interval_ns(rate_measurements_per_sec: f64, measurements_per_batch: usize) -> u64 {
    let per_batch = measurements_per_batch.max(1) as f64;
    let ns = 1_000_000_000.0_f64 * per_batch / rate_measurements_per_sec;
    ns.round().clamp(0.0, u64::MAX as f64) as u64
}

/// The declared, fixed-schedule send time (nanoseconds since a placement's own `clock_zero`)
/// for batch `batch_index`, given `interval_ns` (from [`batch_interval_ns`]). A pure function
/// of its two arguments -- see this module's own doc, "Pacing," for why anchoring every
/// batch's due time to `batch_index * interval_ns` (rather than "the previous send's own
/// finish time plus one interval") is what keeps a placement that falls behind from ever
/// bursting to catch up.
pub fn scheduled_due_at_ns(batch_index: usize, interval_ns: u64) -> u64 {
    (batch_index as u64).saturating_mul(interval_ns)
}

/// Measurements/s actually achieved: `measurements_accepted` divided by `wall_ns` (the
/// interval the caller actually measured -- [`PlacementRun::wall_ns`]'s own doc states
/// precisely which interval that is). `0.0` for a zero (or unmeasured) wall interval, never a
/// division producing `inf`/`NaN` in the report.
pub fn throughput_measurements_per_sec(measurements_accepted: usize, wall_ns: u64) -> f64 {
    if wall_ns == 0 {
        return 0.0;
    }
    measurements_accepted as f64 / (wall_ns as f64 / 1_000_000_000.0)
}

// -------------------------------------------------------------------------------------------
// Aggregate statistics (pure function over declared per-placement samples)
// -------------------------------------------------------------------------------------------

/// The aggregate latency report over every placement's own samples, **pooled**: every
/// placement's `Vec<LatencySample>` flattened into one set before [`crate::latency::
/// summarize`] computes min/max/p50/p99 over it -- so the aggregate p50/p99 are the
/// nearest-rank percentiles of the *union* of every placement's own accept-minus-emit
/// latencies, not (say) an average of each placement's own p50/p99. `None` only when every
/// placement contributed zero samples (mirrors [`crate::latency::summarize`]'s own "empty
/// means no report, never a fabricated zero" convention).
pub fn pooled_latency_report(per_placement_samples: &[Vec<LatencySample>]) -> Option<LatencyReport> {
    let pooled: Vec<LatencySample> = per_placement_samples.iter().flatten().copied().collect();
    summarize(&pooled)
}

// -------------------------------------------------------------------------------------------
// The fixture (moved verbatim from the original `main`, so both the binary and
// `tests/multi_target_harness.rs` build the identical batch sequence from the identical
// source).
// -------------------------------------------------------------------------------------------

/// The fixture's own start epoch (`crates/av-edge/tests/fixtures/ground_segment/README.md`)
/// -- restated here, exactly `crates/av-ingest/tests/plugin_wire.rs`'s own identical
/// constant and the original `src/bin/av-edge-latency.rs`'s own `NOW`, for the identical
/// reason: `EdgeIngestConfig`'s staleness check needs a declared "now" to compare batch
/// epochs against.
pub const NOW: i64 = 1_767_225_637_000_000_000;

const TEST_KEY_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pem");
const TEST_PUB_PEM: &[u8] = include_bytes!("../../av-edge/tests/fixtures/test_signing_key.pub.pem");

/// This crate's own fixtures directory -- `crates/av-edge/tests/fixtures/ground_segment`,
/// reached the identical way the original binary and `tests/engine_accuracy.rs` both already
/// reach it (`CARGO_MANIFEST_DIR` is always this crate's own root regardless of which file
/// inside it the macro expands in).
pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures/ground_segment")
}

/// Restated verbatim from the original `src/bin/av-edge-latency.rs` (and `tests/
/// engine_accuracy.rs`'s own identical copy) -- see either's own comment for why this is
/// restated rather than shared: no third crate exists between `av-edge` and `av-track` for a
/// ten-field struct literal to live in.
fn flight_codec() -> pb::PacketCodec {
    let f = |name: &str, bit_offset: u32| pb::PacketField {
        name: name.to_string(),
        bit_offset,
        bit_width: 64,
        r#type: pb::PacketFieldType::Float64 as i32,
        unit: pb::Unit::Meter as i32,
        scale: 1.0,
        offset: 0.0,
        target: String::new(),
    };
    pb::PacketCodec {
        id: "flight_tm_out_codec".to_string(),
        apid: 500,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "own Earth-fixed Cartesian position telemetry, encoded (M25.1)".to_string(),
        fields: vec![f("x", 0), f("y", 64), f("z", 128)],
    }
}

fn plugin_config() -> PluginConfig {
    let codec = flight_codec();
    let label = pb::Label { marking: "CUI".to_string(), caveats: vec![] };
    let track_cfg = crate::config::demo_ground_segment_config();
    PluginConfig {
        producer_id: "demo-ground-segment-flight-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "flight".to_string(),
        port: "tm_out".to_string(),
        direction: pb::PortDirection::Out as i32,
        codec_bytes: PluginConfig::encode_codec(&codec),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: track_cfg.frame_id.clone(),
        sensor_id: track_cfg.sensor_id.clone(),
        measurement_id: "flight_position".to_string(),
        shard_key: track_cfg.shard_key.clone(),
        noise_r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

/// Everything [`load_fixture`] builds: the declared [`PluginConfig`], the full, signed,
/// chained batch sequence (every one of the fixture's own batches -- a caller wanting fewer
/// truncates this slice itself, never asking this function to fabricate a different fixture),
/// the measured per-batch measurement count, and the raw `RunProducts` (for the in-process
/// path's own truth comparison) plus the verify key (for the in-process path's own server
/// construction).
pub struct Fixture {
    pub cfg: PluginConfig,
    pub batches: Vec<pb::MeasurementBatch>,
    pub measurements_per_batch: usize,
    pub run_products: pb::RunProducts,
    pub verify_key: EcKey<Public>,
}

/// Reads and hash-verifies the committed fixture, builds and signs its full batch sequence,
/// and returns everything a caller (the binary's `main`, or a test) needs -- the identical
/// first third of the original `main` function, moved here unchanged.
pub fn load_fixture() -> Fixture {
    let run_products_bytes = std::fs::read(fixtures_dir().join("run_products.pb")).expect("reading run_products.pb");
    let run_products = <pb::RunProducts as prost::Message>::decode(run_products_bytes.as_slice()).expect("decodes as RunProducts");
    let port_traffic_bytes = std::fs::read(fixtures_dir().join("port_traffic.pb")).expect("reading port_traffic.pb");
    let log = av_edge::plugin::verify_port_traffic_log(&port_traffic_bytes, &run_products.port_traffic_hash).expect("hash-verified sidecar");

    let cfg = plugin_config();
    let source = PortTrafficSource::from_log(&log, &cfg).expect("source builds");
    let signing_key = av_edge::sign::load_signing_key(TEST_KEY_PEM).expect("loading test signing key");
    let verify_key = av_edge::verify::load_verifying_key(TEST_PUB_PEM).expect("loading test verifying key");
    let builder = BatchBuilder::new(cfg.batching).expect("batching rule builds");
    let created_tai_ns = run_products.provenance.as_ref().map(|p| p.created_tai_ns).unwrap_or(0);
    let provenance = cfg.batch_provenance(&run_products.run_id, created_tai_ns);
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &signing_key).expect("building/signing batches");
    let measurements_per_batch = measured_measurements_per_batch(&batches);

    Fixture { cfg, batches, measurements_per_batch, run_products, verify_key }
}

// -------------------------------------------------------------------------------------------
// In-process server (moved verbatim from the original `main`)
// -------------------------------------------------------------------------------------------

async fn poll_until_ready(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("nothing at {addr} became ready within the deadline");
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

/// Starts one real `EdgeIngestService` (no client certificate -- the fixture's own producer
/// is authenticated by `verify_key` instead, exactly the original binary's own choice) on an
/// ephemeral loopback port, under `dir` (the durable log directory -- the caller's own, so an
/// in-process caller can read it back afterward with [`crate::consumer::
/// LogPartitionConsumer`], exactly as the original `main` always did). Returns once the port
/// is confirmed accepting connections ([`poll_until_ready`]). Used both by the in-process
/// binary path and by `tests/multi_target_harness.rs` (which starts two or three of these,
/// one per synthetic placement, to exercise [`drive_all`] with no docker involved).
pub async fn spawn_in_process_server(dir: &Path, producer_id: &str, verify_key: EcKey<Public>) -> SocketAddr {
    let mut server_cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 10_000_000_000_000);
    server_cfg.require_client_certificate = false;
    server_cfg.verify_keys.insert(producer_id.to_string(), verify_key);
    let service = Arc::new(EdgeIngestService::new(dir, None, server_cfg, Arc::new(|| NOW)));

    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.expect("binding loopback");
    let addr = listener.local_addr().expect("local_addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    addr
}

// -------------------------------------------------------------------------------------------
// Driving one placement, and driving all of them concurrently
// -------------------------------------------------------------------------------------------

/// One placement's own measured outcome: every accept/emit pair, the wall interval the loop
/// itself measured, and enough identity to label it in the report.
#[derive(Debug, Clone)]
pub struct PlacementRun {
    pub label: String,
    pub addr: String,
    pub batch_count: usize,
    /// Always equal to `batch_count` for a `PlacementRun` this module ever returns --
    /// [`drive_placement`] returns an `Err` the moment any batch is rejected rather than
    /// recording a partial run, so a completed [`PlacementRun`] is, by construction, a fully
    /// accepted one. Kept as its own field (rather than inferred as always-equal-to-
    /// `batch_count` at the report layer) so `tests/multi_target_harness.rs` can assert it
    /// directly per this module's own task brief ("assert every batch was accepted and the
    /// per-placement counts are right").
    pub accepted_count: usize,
    /// The sum of `measurements.len()` over every batch this placement actually sent (== all
    /// of `batches`, since a completed [`PlacementRun`] never stops partway -- see
    /// `accepted_count`'s own doc). Reported separately from `batch_count`/`accepted_count`
    /// rather than assumed equal to either: this fixture's own batches happen to carry
    /// exactly one measurement each (`measured_measurements_per_batch`'s own doc), but this
    /// field states the real, measured total rather than leaning on that fixture-specific
    /// fact.
    pub measurement_count: usize,
    pub samples: Vec<LatencySample>,
    /// Nanoseconds from this placement's own `clock_zero` (read once, immediately before the
    /// first batch's own possible pacing sleep) to the **accept** instant of the *last* batch
    /// in this placement's run. This is the interval [`throughput_measurements_per_sec`] is
    /// computed over for this placement -- stated here once so the report can say precisely
    /// which interval its own throughput number means.
    pub wall_ns: u64,
}

/// Everything that can go wrong driving one placement -- every variant a typed refusal
/// (this crate's standing convention), never a panic; `src/bin/av-edge-latency.rs`'s own
/// `main` turns any of these into a loud failure (a panic, exactly as the original binary's
/// `.expect()`/`assert!()` calls always did) rather than silently degrading a report.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("placement {label:?}: connecting to {addr}: {source}")]
    Connect {
        label: String,
        addr: String,
        #[source]
        source: av_ingest_client::ConnectError,
    },
    #[error("placement {label:?}: Announce RPC to {addr}: {source}")]
    Announce {
        label: String,
        addr: String,
        #[source]
        source: tonic::Status,
    },
    #[error("placement {label:?}: Announce to {addr} was refused (accepted=false)")]
    AnnounceRefused { label: String, addr: String },
    #[error("placement {label:?}: Submit RPC to {addr}: {source}")]
    Submit {
        label: String,
        addr: String,
        #[source]
        source: tonic::Status,
    },
    #[error("placement {label:?}: Submit RPC to {addr} returned {actual} verdict(s) for 1 submitted batch")]
    VerdictCount { label: String, addr: String, actual: usize },
    #[error("placement {label:?}: batch (sequence {sequence}) to {addr} was rejected: {detail}")]
    Rejected { label: String, addr: String, sequence: u64, detail: String },
}

/// Sends the batch at `index` (emit immediately before `submit_batches`, accept immediately
/// after its `Result` is confirmed `accepted` -- the identical two instants and their
/// identical relative position around `submit_batches` that this whole crate's own module
/// doc pins), waiting first for that batch's own fixed-schedule due time when `interval_ns`
/// is `Some` (this module's own doc, "Pacing"). Shared by both the `in_flight <= 1`
/// sequential path and the `in_flight > 1` concurrent path in [`drive_placement`] below, so
/// the two paths can never disagree about what a single batch's own send does.
async fn send_one_batch(client: &mut EdgeIngestClient, label: &str, addr: &str, clock_zero: Instant, index: usize, batch: pb::MeasurementBatch, interval_ns: Option<u64>) -> Result<LatencySample, HarnessError> {
    if let Some(interval) = interval_ns {
        let due = clock_zero + Duration::from_nanos(scheduled_due_at_ns(index, interval));
        let now = Instant::now();
        if due > now {
            tokio::time::sleep(due - now).await;
        }
        // else: this placement has already fallen behind its own declared schedule for this
        // batch -- send immediately (no sleep), exactly as `AsFastAsPossible` would. The
        // *next* batch's own due time stays anchored to `(index+1) * interval_ns` from this
        // placement's original `clock_zero`, never to "now" -- so falling behind once can
        // never compound into sending two batches back to back to "catch up."
    }

    // --- The one clock this whole placement's loop reads: per-batch emit/accept. ------
    let sequence = batch.sequence;
    let emit_ns = (Instant::now() - clock_zero).as_nanos() as u64;
    let verdicts = client.submit_batches(vec![batch]).await.map_err(|source| HarnessError::Submit { label: label.to_string(), addr: addr.to_string(), source })?;
    let accept_ns = (Instant::now() - clock_zero).as_nanos() as u64;

    if verdicts.len() != 1 {
        return Err(HarnessError::VerdictCount { label: label.to_string(), addr: addr.to_string(), actual: verdicts.len() });
    }
    if !verdicts[0].accepted {
        return Err(HarnessError::Rejected { label: label.to_string(), addr: addr.to_string(), sequence, detail: format!("{:?}", verdicts[0]) });
    }
    Ok(LatencySample { emit_ns, accept_ns })
}

/// Connects to `target`, announces `manifest`, then sends every one of `batches` -- with
/// `in_flight` batches outstanding concurrently, sharing this placement's own schedule
/// (`interval_ns`/[`scheduled_due_at_ns`], both unchanged by `in_flight`). `in_flight <= 1`
/// (the default -- see [`HarnessConfig::in_flight`]'s own doc) keeps **the identical,
/// unmodified sequential per-batch loop the original `src/bin/av-edge-latency.rs` always
/// ran**, so this default path stays structurally identical to the pre-`--in-flight` code,
/// not merely behaviourally equivalent to it. `in_flight > 1` instead drives `batches`
/// through [`send_one_batch`] with up to `in_flight` outstanding at once
/// (`futures_util::stream::StreamExt::buffer_unordered` -- a sliding window: as soon as one
/// outstanding send completes, the next batch in order starts, rather than waiting for a
/// whole batch of `in_flight` sends to drain before starting the next group), so the
/// concurrent path is a genuine pipeline, not a series of barriers.
pub async fn drive_placement(target: PlacementTarget, batches: Arc<Vec<pb::MeasurementBatch>>, manifest: pb::PluginManifest, interval_ns: Option<u64>, in_flight: usize) -> Result<PlacementRun, HarnessError> {
    let PlacementTarget { label, addr } = target;

    let mut client = EdgeIngestClient::connect_plaintext(&addr).await.map_err(|source| HarnessError::Connect { label: label.clone(), addr: addr.clone(), source })?;
    let ack = client.announce(manifest).await.map_err(|source| HarnessError::Announce { label: label.clone(), addr: addr.clone(), source })?;
    if !ack.accepted {
        return Err(HarnessError::AnnounceRefused { label, addr });
    }

    let clock_zero = Instant::now();
    let samples: Vec<LatencySample> = if in_flight <= 1 {
        // --- The identical, unmodified sequential loop this function always ran. ----------
        let mut samples: Vec<LatencySample> = Vec::with_capacity(batches.len());
        for (i, batch) in batches.iter().enumerate() {
            let sample = send_one_batch(&mut client, &label, &addr, clock_zero, i, batch.clone(), interval_ns).await?;
            samples.push(sample);
        }
        samples
    } else {
        // --- Up to `in_flight` batches outstanding concurrently, sharing this placement's
        // --- own schedule. `EdgeIngestClient` is `Clone` (a cloned `tonic::transport::
        // Channel` multiplexes independent RPCs over the same HTTP/2 connection -- no new
        // connection per clone), so every outstanding send below shares the one connection
        // `connect_plaintext` opened above, never opening a second one. Each batch is cloned
        // out of `batches` up front (`.cloned()`) into an owned item the spawned future can
        // move without borrowing across an `.await` point -- the identical single clone per
        // batch the sequential path above makes, just made once at a different place.
        let results: Vec<Result<LatencySample, HarnessError>> = stream::iter(batches.iter().cloned().enumerate())
            .map(|(i, batch)| {
                let mut client = client.clone();
                let label = label.clone();
                let addr = addr.clone();
                async move { send_one_batch(&mut client, &label, &addr, clock_zero, i, batch, interval_ns).await }
            })
            .buffer_unordered(in_flight)
            .collect()
            .await;
        let mut samples = Vec::with_capacity(results.len());
        for r in results {
            samples.push(r?);
        }
        samples
    };
    let wall_ns = (Instant::now() - clock_zero).as_nanos() as u64;
    let accepted_count = samples.len();
    let measurement_count: usize = batches.iter().map(|b| b.measurements.len()).sum();

    Ok(PlacementRun { label, addr, batch_count: batches.len(), accepted_count, measurement_count, samples, wall_ns })
}

/// Drives every one of `targets` **concurrently** -- one `tokio::spawn`ed task per placement,
/// not a sequential loop (this module's own doc: "three sequential runs would measure nothing
/// about three placements"). Every task shares the identical `batches`/`manifest`/`in_flight`
/// (an `Arc` clone per task for `batches`, never a re-derivation), and every task's own
/// result -- success or [`HarnessError`] -- is returned in `targets`' own order, so a caller
/// can zip it back against the `targets` it passed in.
pub async fn drive_all(targets: Vec<PlacementTarget>, batches: Arc<Vec<pb::MeasurementBatch>>, manifest: pb::PluginManifest, interval_ns: Option<u64>, in_flight: usize) -> Vec<Result<PlacementRun, HarnessError>> {
    let mut handles = Vec::with_capacity(targets.len());
    for target in targets {
        let batches = Arc::clone(&batches);
        let manifest = manifest.clone();
        handles.push(tokio::spawn(async move { drive_placement(target, batches, manifest, interval_ns, in_flight).await }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await.expect("a placement-driving task panicked -- this is a bug in this binary, not a measured condition"));
    }
    results
}

// -------------------------------------------------------------------------------------------
// Host state (best-effort; extends what the original binary already reported)
// -------------------------------------------------------------------------------------------

/// 1/5/15-minute load averages, best-effort: `sysctl -n vm.loadavg` on macOS (this
/// workspace's own host, per `docs/p5-plan.md`'s D5 status), `/proc/loadavg` on Linux
/// (`spoore`/CI hosts), `None` anywhere neither is available -- never a fabricated number.
/// Shells out rather than reading a syscall directly through a new dependency (`libc` is not
/// in this workspace's dependency tree at all -- `tests/no_object_store.rs`'s own exact
/// direct-dependency list would need updating for one, and this task's brief forbids adding a
/// dependency); a single, cheap, read-only subprocess call, not a network access (question
/// 154 is unaffected).
pub fn load_average() -> Option<[f64; 3]> {
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        let vals: Vec<f64> = text.split_whitespace().take(3).filter_map(|s| s.parse().ok()).collect();
        if vals.len() == 3 {
            return Some([vals[0], vals[1], vals[2]]);
        }
    }
    let output = std::process::Command::new("sysctl").args(["-n", "vm.loadavg"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let inner = text.trim().trim_start_matches('{').trim_end_matches('}');
    let vals: Vec<f64> = inner.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    if vals.len() == 3 {
        Some([vals[0], vals[1], vals[2]])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> impl Iterator<Item = String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    // --- Zero arguments: the pinned baseline path. ------------------------------------

    #[test]
    fn no_arguments_parses_to_the_baseline_config_exactly() {
        let cfg = parse_args(strs(&["av-edge-latency"])).unwrap();
        assert_eq!(cfg, BASELINE_CONFIG);
        assert_eq!(cfg.mode, Mode::InProcess);
        assert_eq!(cfg.batches, 900);
        assert_eq!(cfg.rate_per_sec, None);
        assert_eq!(cfg.in_flight, 1, "question 148: zero arguments must default to in_flight=1, reproducing today's behaviour exactly");
    }

    // --- --target: repeated, labelled, ordered. ----------------------------------------

    #[test]
    fn repeated_target_flags_build_placements_in_order_with_labels_and_addresses_preserved() {
        let cfg = parse_args(strs(&["av-edge-latency", "--target", "edge-a=127.0.0.1:50061", "--target", "edge-b=127.0.0.1:50062", "--target", "edge-c=127.0.0.1:50063"])).unwrap();
        match cfg.mode {
            Mode::Targets(targets) => assert_eq!(
                targets,
                vec![
                    PlacementTarget { label: "edge-a".to_string(), addr: "127.0.0.1:50061".to_string() },
                    PlacementTarget { label: "edge-b".to_string(), addr: "127.0.0.1:50062".to_string() },
                    PlacementTarget { label: "edge-c".to_string(), addr: "127.0.0.1:50063".to_string() },
                ]
            ),
            Mode::InProcess => panic!("expected Mode::Targets"),
        }
    }

    #[test]
    fn a_single_target_still_selects_targets_mode_never_in_process() {
        let cfg = parse_args(strs(&["av-edge-latency", "--target", "only=127.0.0.1:1"])).unwrap();
        assert!(matches!(cfg.mode, Mode::Targets(_)), "one --target must still switch out of Mode::InProcess: {:?}", cfg.mode);
    }

    #[test]
    fn target_without_an_equals_sign_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--target", "no-equals-here"])).unwrap_err();
        assert!(err.contains("LABEL=ADDR"), "{err}");
    }

    #[test]
    fn target_with_an_empty_label_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--target", "=127.0.0.1:1"])).unwrap_err();
        assert!(err.contains("LABEL must not be empty"), "{err}");
    }

    #[test]
    fn target_with_an_empty_address_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--target", "edge-a="])).unwrap_err();
        assert!(err.contains("ADDR must not be empty"), "{err}");
    }

    #[test]
    fn a_repeated_label_is_refused_rather_than_silently_overwritten() {
        let err = parse_args(strs(&["av-edge-latency", "--target", "edge-a=127.0.0.1:1", "--target", "edge-a=127.0.0.1:2"])).unwrap_err();
        assert!(err.contains("already given"), "{err}");
    }

    // --- --rate. -------------------------------------------------------------------------

    #[test]
    fn rate_parses_as_a_positive_f64() {
        let cfg = parse_args(strs(&["av-edge-latency", "--rate", "1000"])).unwrap();
        assert_eq!(cfg.rate_per_sec, Some(1000.0));
    }

    #[test]
    fn zero_rate_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--rate", "0"])).unwrap_err();
        assert!(err.contains("positive"), "{err}");
    }

    #[test]
    fn negative_rate_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--rate", "-5"])).unwrap_err();
        assert!(err.contains("positive"), "{err}");
    }

    #[test]
    fn non_numeric_rate_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--rate", "fast"])).unwrap_err();
        assert!(err.contains("not a valid number"), "{err}");
    }

    // --- --batches. ------------------------------------------------------------------------

    #[test]
    fn batches_parses_as_a_positive_usize_and_overrides_the_default() {
        let cfg = parse_args(strs(&["av-edge-latency", "--batches", "100"])).unwrap();
        assert_eq!(cfg.batches, 100);
    }

    #[test]
    fn zero_batches_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--batches", "0"])).unwrap_err();
        assert!(err.contains("nothing to measure"), "{err}");
    }

    #[test]
    fn unrecognized_flag_is_refused_with_the_usage_string() {
        let err = parse_args(strs(&["av-edge-latency", "--nonsense"])).unwrap_err();
        assert!(err.contains("unrecognized argument"), "{err}");
        assert!(err.contains("usage:"), "{err}");
    }

    #[test]
    fn a_flag_missing_its_value_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--rate"])).unwrap_err();
        assert!(err.contains("requires a value"), "{err}");
    }

    // --- --in-flight (question 148: the measurement-apparatus ceiling). -------------------

    #[test]
    fn in_flight_defaults_to_one_when_omitted() {
        let cfg = parse_args(strs(&["av-edge-latency"])).unwrap();
        assert_eq!(cfg.in_flight, 1);
    }

    #[test]
    fn explicit_in_flight_one_parses_identically_to_the_default() {
        let cfg = parse_args(strs(&["av-edge-latency", "--in-flight", "1"])).unwrap();
        assert_eq!(cfg.in_flight, 1);
        assert_eq!(cfg, BASELINE_CONFIG, "--in-flight 1 must parse to the identical HarnessConfig the default (omitted) path parses to");
    }

    #[test]
    fn in_flight_greater_than_one_parses_and_overrides_the_default() {
        let cfg = parse_args(strs(&["av-edge-latency", "--in-flight", "8"])).unwrap();
        assert_eq!(cfg.in_flight, 8);
    }

    #[test]
    fn zero_in_flight_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--in-flight", "0"])).unwrap_err();
        assert!(err.contains("at least one batch must be outstanding"), "{err}");
    }

    #[test]
    fn negative_in_flight_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--in-flight", "-3"])).unwrap_err();
        assert!(err.contains("not a valid positive integer"), "{err}");
    }

    #[test]
    fn non_numeric_in_flight_is_refused() {
        let err = parse_args(strs(&["av-edge-latency", "--in-flight", "many"])).unwrap_err();
        assert!(err.contains("not a valid positive integer"), "{err}");
    }

    #[test]
    fn in_flight_combines_with_target_rate_and_batches() {
        let cfg = parse_args(strs(&["av-edge-latency", "--target", "a=127.0.0.1:1", "--rate", "10", "--batches", "5", "--in-flight", "4"])).unwrap();
        assert!(matches!(cfg.mode, Mode::Targets(_)));
        assert_eq!(cfg.batches, 5);
        assert_eq!(cfg.rate_per_sec, Some(10.0));
        assert_eq!(cfg.in_flight, 4);
    }

    // --- The mutual exclusion between in-process mode and targets. ------------------------

    #[test]
    fn no_targets_is_structurally_in_process_mode() {
        let cfg = parse_args(strs(&["av-edge-latency", "--batches", "10"])).unwrap();
        assert_eq!(cfg.mode, Mode::InProcess, "with zero --target flags this binary must never start in Targets mode");
    }

    #[test]
    fn any_target_is_structurally_targets_mode_and_can_never_also_be_in_process() {
        // `Mode` is an enum: this is a compile-time guarantee, not just a runtime one, but
        // this test still pins the observable behaviour a caller (main) actually branches on.
        let cfg = parse_args(strs(&["av-edge-latency", "--target", "a=127.0.0.1:1", "--rate", "10", "--batches", "5"])).unwrap();
        assert!(matches!(cfg.mode, Mode::Targets(_)));
        assert_eq!(cfg.batches, 5);
        assert_eq!(cfg.rate_per_sec, Some(10.0));
    }

    // --- Rate -> cadence arithmetic (pure, declared inputs, no sleeping). -----------------

    #[test]
    fn batch_interval_ns_at_spoores_declared_1000_per_second_with_one_measurement_per_batch() {
        // D5's own declared load (question 41): 1,000 measurements/s per shard, and this
        // fixture's own measured batch size is 1 measurement/batch (BatchingRule::PerEpoch
        // over a one-packet-per-step log) -- so the implied cadence is exactly one batch
        // every 1 ms.
        assert_eq!(batch_interval_ns(1000.0, 1), 1_000_000);
    }

    #[test]
    fn batch_interval_ns_scales_with_measurements_per_batch() {
        // 500 measurements/s at 2 measurements/batch is 250 batches/s, i.e. 4 ms/batch.
        assert_eq!(batch_interval_ns(500.0, 2), 4_000_000);
    }

    #[test]
    fn batch_interval_ns_rounds_to_the_nearest_nanosecond() {
        // 3 measurements/s at 1/batch is 1/3 s/batch = 333_333_333.33... ns, rounds to
        // ...333.
        assert_eq!(batch_interval_ns(3.0, 1), 333_333_333);
    }

    #[test]
    fn scheduled_due_at_ns_is_batch_index_times_interval() {
        assert_eq!(scheduled_due_at_ns(0, 1_000_000), 0);
        assert_eq!(scheduled_due_at_ns(1, 1_000_000), 1_000_000);
        assert_eq!(scheduled_due_at_ns(900, 1_000_000), 900_000_000);
    }

    #[test]
    fn measured_measurements_per_batch_over_an_empty_slice_is_zero() {
        assert_eq!(measured_measurements_per_batch(&[]), 0);
    }

    #[test]
    fn measured_measurements_per_batch_counts_real_batches() {
        let batches = vec![
            pb::MeasurementBatch { measurements: vec![pb::Measurement::default()], ..Default::default() },
            pb::MeasurementBatch { measurements: vec![pb::Measurement::default()], ..Default::default() },
        ];
        assert_eq!(measured_measurements_per_batch(&batches), 1);
    }

    #[test]
    fn throughput_is_accepted_count_over_the_measured_wall_interval() {
        // 900 measurements accepted over exactly 900 ms wall time is 1000/s.
        assert_eq!(throughput_measurements_per_sec(900, 900_000_000), 1000.0);
    }

    #[test]
    fn throughput_over_a_zero_wall_interval_is_zero_not_infinite() {
        assert_eq!(throughput_measurements_per_sec(5, 0), 0.0);
    }

    // --- Aggregate statistics: pure function over declared per-placement samples. ---------

    #[test]
    fn pooled_latency_report_is_none_when_every_placement_is_empty() {
        assert!(pooled_latency_report(&[vec![], vec![]]).is_none());
    }

    #[test]
    fn pooled_latency_report_computes_percentiles_over_the_union_of_every_placement() {
        // Placement A: accept-minus-emit latencies 1..=50 ns (as emit=0, accept=i).
        // Placement B: accept-minus-emit latencies 51..=100 ns.
        // Pooled: 1..=100, the identical distribution `crate::latency::summarize`'s own
        // `min_max_p50_p99_over_a_known_distribution` test already pins (p50=50, p99=99),
        // computed here over two separately-declared placements rather than one flat slice --
        // this is exactly the "over pooled samples" definition this module's own doc claims.
        let a: Vec<LatencySample> = (1..=50u64).map(|i| LatencySample { emit_ns: 0, accept_ns: i }).collect();
        let b: Vec<LatencySample> = (51..=100u64).map(|i| LatencySample { emit_ns: 0, accept_ns: i }).collect();
        let report = pooled_latency_report(&[a, b]).unwrap();
        assert_eq!(report.count, 100);
        assert_eq!(report.min_ns, 1);
        assert_eq!(report.max_ns, 100);
        assert_eq!(report.p50_ns, 50);
        assert_eq!(report.p99_ns, 99);
    }

    #[test]
    fn pooled_latency_report_over_one_placement_matches_that_placements_own_summary() {
        let only: Vec<LatencySample> = (1..=10u64).map(|i| LatencySample { emit_ns: 0, accept_ns: i }).collect();
        let direct = summarize(&only).unwrap();
        let pooled = pooled_latency_report(std::slice::from_ref(&only)).unwrap();
        assert_eq!(direct, pooled);
    }
}

//! The latency harness (`docs/edge-plan.md` milestone E5's own instruction: "a binary, not
//! a test assertion"). Runs the **whole** path over a real wire -- plugin batch-build,
//! `EdgeIngestService` (a real `tonic` server), the durable per-partition log, this crate's
//! own `LogPartitionConsumer`, and the engine bridge -- and prints one machine-readable JSON
//! report: batch count, latency min/max/p50/p99, the track-vs-truth comparison, the declared
//! `TrackConfig`'s own content hash, and the host state this binary can see about itself.
//!
//! D5a (`docs/p5-plan.md`'s D5 "three placements" milestone) extended this binary with a
//! **multi-placement** mode: `--target LABEL=ADDR` (repeatable) drives one or more already-
//! running, remote `av-ingest-server` processes concurrently instead of the original
//! single, in-process server. **With no arguments this binary behaves exactly as it always
//! has** -- same in-process `tonic` server on an ephemeral loopback port, same 900-batch
//! default, same original report fields, unchanged in name and meaning (`crate::harness::
//! BASELINE_CONFIG` pins this; `crate::harness::tests::no_arguments_parses_to_the_baseline_
//! config_exactly` asserts it). Every field this binary's report has always emitted is still
//! emitted, with the identical name and meaning, whenever this binary runs in its original
//! (in-process) mode -- the lead's baseline (question 219, 2026-09-16 04:05: p50 6.8 ms / p99
//! 11.3 ms medians of five, 900 batches, chain head `d1d80d0b…`) stays reproducible and
//! field-for-field comparable against a fresh zero-argument run's own JSON.
//!
//! # Flags
//!
//! - `--target LABEL=ADDR` (repeatable): drive an already-running, remote `av-ingest-server`
//!   at `ADDR` (a bare `"host:port"`, loopback only -- `av_ingest_client::EdgeIngestClient::
//!   connect_plaintext`'s own question 155/202 rule, unchanged) under the name `LABEL` (D5's
//!   own three placements: `edge-a`/`edge-b`/`edge-c`, `deploy/secdeploy/
//!   secsite.altavista-3.toml`). Given three `--target`s, this binary drives three
//!   placements, concurrently, and never starts an in-process server at all. See
//!   `crate::harness`'s own module doc for the full mode contract and `crate::harness::
//!   parse_args` for the exact `LABEL=ADDR` grammar and its refusals.
//! - `--rate N`: the declared offered load, **measurements/s per placement** (D5 declares
//!   1,000/s, spoore's own budget -- question 41). Converted to a per-batch send interval via
//!   `crate::harness::batch_interval_ns`, from the fixture's own **measured** batch size
//!   (`crate::harness::measured_measurements_per_batch` -- this fixture measures 1
//!   measurement/batch; a future fixture is not assumed to). Omitted: every batch is sent as
//!   fast as `submit_batches` returns, exactly as this binary always did before `--rate`
//!   existed. See `crate::harness`'s own module doc, "Pacing," for exactly how a placement
//!   that cannot keep up degrades (never a burst, never a silently absorbed delay).
//! - `--batches N`: batches **per placement** (default `900`, the baseline's own count, so a
//!   multi-placement run stays directly comparable to it per placement). Refused if `N`
//!   exceeds the fixture's own total batch count (900) -- this binary truncates the fixture's
//!   real batch sequence, it never fabricates additional batches to reach a larger `N`.
//!
//! # The two instants this measures, restated concretely
//!
//! See `av_track::latency`'s own module doc for the full argument, and `av_track::harness`'s
//! own module doc for exactly how the multi-placement path preserves it; concretely, per
//! batch, per placement: **emit** = `Instant::now()` read immediately before
//! `client.submit_batches(vec![batch])` is called; **accept** = `Instant::now()` read
//! immediately after that call's `Result` is available and confirmed `accepted`. Only
//! `av_track::harness::drive_placement` ever reads `Instant::now()` in this whole path --
//! `av-edge`, `av-ingest`, and this crate's own library code otherwise stay clock-free
//! (question 199); `tokio::time::sleep`'s own read of real time, for `--rate`'s pacing, is the
//! one other place this binary's own real clock is consulted, and only ever *before* a
//! batch's own emit read, never between emit and accept.
//!
//! # This binary's own clock is real; `av-ingest`'s injected clock is not
//!
//! `EdgeIngestService` (the in-process placement only -- a remote `--target`'s own server is
//! whatever the caller started it with) is constructed with a fixed clock closure (`|| NOW`,
//! `NOW` being the fixture's own declared start epoch) purely so the service's *staleness*
//! check has something declared to compare against -- it never affects the latency
//! measurement itself, which is entirely a client-side `Instant` measurement around the RPC
//! call.
//!
//! # This binary asserts no latency threshold
//!
//! A contended timing result is not a result (this milestone's own instruction) -- it prints
//! the numbers and exits `0` whenever the *functional* path succeeded (every batch accepted
//! by every placement; in in-process mode, additionally: the chain verifies and the
//! comparison stayed within `av_track::compare::POSITION_TOLERANCE_M`); a slow run is still a
//! successful run of this binary, just with worse numbers in its own report. This property is
//! unchanged by the multi-placement path: `av_track::harness::drive_placement` fails loudly
//! (a typed error, turned into a panic by `main` below) only on a connection failure, a
//! malformed verdict, or an outright rejected batch -- never on a slow accepted one.

use std::sync::Arc;

use av_edge::pb;
use av_track::bridge::EngineBridge;
use av_track::compare::{compare_to_truth, POSITION_TOLERANCE_M};
use av_track::config::demo_ground_segment_config;
use av_track::consumer::{LogPartitionConsumer, MeasurementConsumer, StartOffset};
use av_track::harness::{self, Fixture, Mode, PlacementRun, PlacementTarget};
use av_track::latency::{summarize, LatencyReport};

/// Question 41: spoore's own p99-through-the-bus budget.
const SPOORE_P99_BUDGET_MS: f64 = 75.0;
/// Question 41: spoore's own per-shard throughput budget.
const SPOORE_THROUGHPUT_BUDGET_PER_PLACEMENT: f64 = 1000.0;
/// The lead's E5 retake (`docs/open-questions.md` question 219, 2026-09-16 04:05): medians of
/// five 900-batch runs on an idle host, chain head `d1d80d0b…`.
const LEAD_BASELINE_P50_MS: f64 = 6.8;
const LEAD_BASELINE_P99_MS: f64 = 11.3;
const LEAD_BASELINE_SOURCE: &str = "docs/open-questions.md question 219 (2026-09-16 04:05): medians of five 900-batch av-edge-latency runs on an idle host, chain head d1d80d0b...; p50 5.2/5.2/6.8/7.0/7.5 ms, p99 7.6/8.8/11.3/11.7/12.5 ms, min 4.1 ms, one fsync per durable append";

#[tokio::main]
async fn main() {
    let config = match harness::parse_args(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("av-edge-latency: {e}");
            std::process::exit(2);
        }
    };

    let fixture = harness::load_fixture();
    let total_available = fixture.batches.len();
    if config.batches > total_available {
        eprintln!("av-edge-latency: --batches {} exceeds this fixture's own {total_available} available batches -- this binary truncates the fixture's real batches, it never fabricates more", config.batches);
        std::process::exit(2);
    }
    let batches: Vec<pb::MeasurementBatch> = fixture.batches[..config.batches].to_vec();
    let batches = Arc::new(batches);
    let manifest = fixture.cfg.manifest().expect("manifest builds");
    let measurements_per_batch = fixture.measurements_per_batch;
    let interval_ns = config.rate_per_sec.map(|rate| harness::batch_interval_ns(rate, measurements_per_batch));

    let host = host_state_json();

    match config.mode {
        Mode::InProcess => run_in_process(fixture, batches, manifest, interval_ns, config.rate_per_sec, host).await,
        Mode::Targets(targets) => run_targets(targets, batches, manifest, interval_ns, config.rate_per_sec, measurements_per_batch, host).await,
    }
}

async fn run_in_process(fixture: Fixture, batches: Arc<Vec<pb::MeasurementBatch>>, manifest: pb::PluginManifest, interval_ns: Option<u64>, declared_rate_per_sec: Option<f64>, host: serde_json::Value) {
    let batch_count = batches.len();
    let measurement_count: usize = batches.iter().map(|b| b.measurements.len()).sum();

    // --- The real wire: a genuine EdgeIngestService on an ephemeral loopback port. ------
    let dir = std::env::temp_dir().join(format!("av-edge-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let addr = harness::spawn_in_process_server(&dir, &fixture.cfg.producer_id, fixture.verify_key.clone()).await;

    let target = PlacementTarget { label: "in-process".to_string(), addr: addr.to_string() };
    let run = harness::drive_placement(target, Arc::clone(&batches), manifest, interval_ns).await.unwrap_or_else(|e| panic!("{e}"));

    // --- The consumer, over the same durable log the wire just wrote. -------------------
    let (mut consumer, recovery) = LogPartitionConsumer::open(&dir, &fixture.cfg.shard_key, StartOffset::Earliest).expect("opening and chain-verifying the partition this run just wrote");
    assert!(recovery.is_none(), "a clean, uninterrupted run must never need crash recovery: {recovery:?}");
    let mut measurements = Vec::with_capacity(consumer.len());
    while let Some(received) = consumer.poll_measurement() {
        measurements.push(received.value);
    }
    assert_eq!(measurements.len(), measurement_count);

    // --- The engine bridge, and the comparison against the fixture's own truth. ---------
    let track_cfg = demo_ground_segment_config();
    let mut bridge = EngineBridge::new(&track_cfg).expect("building the engine bridge");
    let updates = bridge.run(&measurements).expect("running the engine bridge");
    let truth = fixture.run_products.trajectories.get("flight").expect("the flight instance must have a truth Trajectory");
    let comparison = compare_to_truth(&updates, truth);
    eprintln!("{}", comparison.summary_line());

    let _ = std::fs::remove_dir_all(&dir);

    let runs = [run];
    let latency_report = summarize(&runs[0].samples).expect("at least one batch was submitted");

    let chain_head_hex = av_edge::hash::hex_encode(&batches.last().expect("at least one batch was submitted").batch_hash);

    let report = serde_json::json!({
        "mode": "in-process",
        // --- Original, pre-D5a fields: unchanged name and meaning. ----------------------
        "batch_count": batch_count,
        "measurement_count": measurement_count,
        "chain_head_hex": chain_head_hex,
        "latency_ns": latency_ns_json(&latency_report),
        "track_config_hash_hex": av_edge::hash::hex_encode(&track_cfg.config_hash()),
        "track_comparison": {
            "compared_epochs": comparison.series.len(),
            "unmatched_updates": comparison.unmatched_updates,
            "max_error_m": comparison.max_error_m,
            "p50_error_m": comparison.p50_error_m,
            "p99_error_m": comparison.p99_error_m,
            "tolerance_m": POSITION_TOLERANCE_M,
            "within_tolerance": comparison.within_tolerance(),
        },
        // --- D5a additions: additive only, nothing above renamed or repurposed. ----------
        "declared_batches_per_placement": batch_count,
        "measurements_per_batch": fixture.measurements_per_batch,
        "declared_rate_measurements_per_sec_per_placement": declared_rate_per_sec,
        "batch_interval_ns": interval_ns,
        "placements": placements_json(&runs, declared_rate_per_sec),
        "aggregate": aggregate_json(&runs, declared_rate_per_sec),
        "budget": budget_json(),
        "host": host,
        "note": "this binary asserts no latency threshold (E5's own instruction); host quiescence was NOT verified by this binary itself -- the caller must check host state separately before treating these numbers as a clean measurement; in-process mode drove exactly one placement, so 'aggregate' and 'placements[0]' both restate 'latency_ns'/'batch_count'/'measurement_count' above under their own D5a-shaped keys",
    });
    println!("{report}");
}

/// The multi-target path: no in-process server, one already-running remote `av-ingest-server`
/// per `--target`, driven concurrently by `av_track::harness::drive_all`. There is no local
/// durable log for this binary to reopen (each placement's own log lives inside its own
/// remote process), so this report carries no `chain_head_hex`/`track_config_hash_hex`/
/// `track_comparison` -- see this module's own doc and `crate::harness`'s for why those three
/// fields are in-process-only, never a nonsense value standing in for "not applicable."
async fn run_targets(targets: Vec<PlacementTarget>, batches: Arc<Vec<pb::MeasurementBatch>>, manifest: pb::PluginManifest, interval_ns: Option<u64>, declared_rate_per_sec: Option<f64>, measurements_per_batch: usize, host: serde_json::Value) {
    let declared_batches_per_placement = batches.len();
    let results = harness::drive_all(targets, batches, manifest, interval_ns).await;

    let mut runs: Vec<PlacementRun> = Vec::with_capacity(results.len());
    for result in results {
        match result {
            Ok(run) => runs.push(run),
            Err(e) => panic!("{e}"),
        }
    }

    let report = serde_json::json!({
        "mode": "targets",
        "declared_batches_per_placement": declared_batches_per_placement,
        "measurements_per_batch": measurements_per_batch,
        "declared_rate_measurements_per_sec_per_placement": declared_rate_per_sec,
        "batch_interval_ns": interval_ns,
        "placements": placements_json(&runs, declared_rate_per_sec),
        "aggregate": aggregate_json(&runs, declared_rate_per_sec),
        "budget": budget_json(),
        "host": host,
        "note": "this binary asserts no latency threshold (E5's own instruction); host quiescence was NOT verified by this binary itself; targets mode has no local durable log to reopen (each placement's own log lives inside its own remote av-ingest-server process), so chain_head_hex/track_config_hash_hex/track_comparison are not present in this mode's report -- see placements[]/aggregate for this mode's own latency and throughput numbers",
    });
    println!("{report}");
}

fn latency_ns_json(r: &LatencyReport) -> serde_json::Value {
    serde_json::json!({ "count": r.count, "min": r.min_ns, "max": r.max_ns, "p50": r.p50_ns, "p99": r.p99_ns })
}

/// One placement's own D5a report block: identity, counts, latency, wall interval,
/// throughput, and this placement's own comparisons against spoore's budget and the lead's
/// baseline -- recorded as numbers and ratios (this milestone's own instruction: "not
/// rounded away," and D5's own instruction: as recorded numbers, never a pass/fail verdict).
fn placement_json(r: &PlacementRun, declared_rate_per_sec: Option<f64>) -> serde_json::Value {
    let latency = summarize(&r.samples).expect("at least one batch was submitted");
    let throughput = harness::throughput_measurements_per_sec(r.accepted_count, r.wall_ns);
    let p50_ms = latency.p50_ns as f64 / 1_000_000.0;
    let p99_ms = latency.p99_ns as f64 / 1_000_000.0;
    serde_json::json!({
        "label": r.label,
        "addr": r.addr,
        "batch_count": r.batch_count,
        "accepted_count": r.accepted_count,
        "measurement_count": r.measurement_count,
        "latency_ns": latency_ns_json(&latency),
        "wall_ns": r.wall_ns,
        "wall_ns_definition": "nanoseconds from this placement's own clock_zero (read immediately before its first batch's own possible pacing sleep) to the accept instant of its own last batch -- throughput_measurements_per_sec below is measurement_count / (wall_ns / 1e9)",
        "throughput_measurements_per_sec": throughput,
        "declared_rate_measurements_per_sec": declared_rate_per_sec,
        "achieved_vs_declared_rate_ratio": declared_rate_per_sec.map(|d| throughput / d),
        "budget_comparison": {
            "p99_vs_spoore_p99_budget_ratio": p99_ms / SPOORE_P99_BUDGET_MS,
            "throughput_vs_spoore_throughput_budget_ratio": throughput / SPOORE_THROUGHPUT_BUDGET_PER_PLACEMENT,
            "p50_vs_lead_baseline_ratio": p50_ms / LEAD_BASELINE_P50_MS,
            "p99_vs_lead_baseline_ratio": p99_ms / LEAD_BASELINE_P99_MS,
        },
    })
}

fn placements_json(runs: &[PlacementRun], declared_rate_per_sec: Option<f64>) -> Vec<serde_json::Value> {
    runs.iter().map(|r| placement_json(r, declared_rate_per_sec)).collect()
}

/// The aggregate block over every placement: pooled latency (`av_track::harness::
/// pooled_latency_report` -- percentiles over the *union* of every placement's own samples,
/// not an average of each placement's own percentile, stated explicitly in
/// `"latency_definition"` below so a reader never has to guess), summed batch/measurement/
/// accepted counts, and throughput measured over the wall-clock span until the *last*
/// placement finished (since every placement runs concurrently -- `"wall_ns_max_definition"`
/// below states this precisely).
fn aggregate_json(runs: &[PlacementRun], declared_rate_per_sec: Option<f64>) -> serde_json::Value {
    let per_placement_samples: Vec<Vec<av_track::latency::LatencySample>> = runs.iter().map(|r| r.samples.clone()).collect();
    let pooled = harness::pooled_latency_report(&per_placement_samples).expect("at least one batch was submitted, by at least one placement");
    let batch_count: usize = runs.iter().map(|r| r.batch_count).sum();
    let accepted_count: usize = runs.iter().map(|r| r.accepted_count).sum();
    let measurement_count: usize = runs.iter().map(|r| r.measurement_count).sum();
    let wall_ns_max: u64 = runs.iter().map(|r| r.wall_ns).max().unwrap_or(0);
    let throughput = harness::throughput_measurements_per_sec(measurement_count, wall_ns_max);
    let declared_aggregate = declared_rate_per_sec.map(|rate| rate * runs.len() as f64);
    let p50_ms = pooled.p50_ns as f64 / 1_000_000.0;
    let p99_ms = pooled.p99_ns as f64 / 1_000_000.0;
    serde_json::json!({
        "placement_count": runs.len(),
        "batch_count": batch_count,
        "accepted_count": accepted_count,
        "measurement_count": measurement_count,
        "latency_ns": latency_ns_json(&pooled),
        "latency_definition": "min/max/p50/p99 pooled over every placement's own accept-minus-emit latency samples (av_track::harness::pooled_latency_report) -- percentiles of the union, not an average of each placement's own percentile",
        "wall_ns_max": wall_ns_max,
        "wall_ns_max_definition": "the maximum of every placement's own wall_ns -- since placements are driven concurrently, this is the wall-clock span until the last placement finished; throughput_measurements_per_sec below is measurement_count / (wall_ns_max / 1e9)",
        "throughput_measurements_per_sec": throughput,
        "declared_rate_measurements_per_sec": declared_aggregate,
        "achieved_vs_declared_rate_ratio": declared_aggregate.map(|d| throughput / d),
        "budget_comparison": {
            "p99_vs_spoore_p99_budget_ratio": p99_ms / SPOORE_P99_BUDGET_MS,
            "p50_vs_lead_baseline_ratio": p50_ms / LEAD_BASELINE_P50_MS,
            "p99_vs_lead_baseline_ratio": p99_ms / LEAD_BASELINE_P99_MS,
        },
    })
}

/// The declared budgets/baselines themselves (question 41's spoore budget; question 219's
/// lead baseline) -- recorded once, here, rather than repeated as a magic number inside every
/// per-placement/aggregate ratio above.
fn budget_json() -> serde_json::Value {
    serde_json::json!({
        "spoore_p99_budget_ms": SPOORE_P99_BUDGET_MS,
        "spoore_throughput_budget_measurements_per_sec_per_placement": SPOORE_THROUGHPUT_BUDGET_PER_PLACEMENT,
        "lead_baseline_p50_ms": LEAD_BASELINE_P50_MS,
        "lead_baseline_p99_ms": LEAD_BASELINE_P99_MS,
        "lead_baseline_source": LEAD_BASELINE_SOURCE,
    })
}

fn host_state_json() -> serde_json::Value {
    serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "quiescence_verified_by_this_binary": false,
        "load_average_1_5_15": harness::load_average(),
    })
}

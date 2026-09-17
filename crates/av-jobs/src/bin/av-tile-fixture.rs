//! H5b-1 (`docs/heavy-plan.md` H5, round 3): the committed tile-set generator binary. Runs a
//! REAL job through the REAL `av-jobs` queue ([`av_jobs::queue::JobQueue`]) + `Runner`
//! ([`av_jobs::runner::Runner`]) + `TilerExecutor` ([`av_jobs::tiler::TilerExecutor`]),
//! storing every output -- the source raster, every tile, and the manifest -- in a REAL
//! `av-store`/MinIO object store by content hash. Prints exactly ONE JSON object to stdout
//! naming the resulting tile set's own identity; stdout carries nothing else, ever (a caller
//! pipes this straight into `jq`/`json.loads`, never scrapes progress text out of it first).
//! Every other line of output goes to stderr.
//!
//! This is `crates/av-jobs`'s own `store-fixture`-gated `[[bin]]` -- see `Cargo.toml`'s own
//! comment on that feature for why: `cargo build -p av-jobs --bin av-tile-fixture --features
//! store-fixture`.
//!
//! # Every knob is a command-line argument (question 199)
//!
//! Mirrors `crates/av-tiles/src/bin/av-tiles.rs`'s own convention exactly -- that binary's own
//! module doc: "no test may mutate the process environment, and an env-var-configured binary
//! invites exactly that" -- never an environment variable. See [`USAGE`] for the full flag
//! list.
//!
//! # `--dry-run`: the manifest with no store, no container, no network
//!
//! With `--dry-run`, the source raster and every tile/manifest this run produces are held in
//! this crate's own [`av_jobs::runner::MemoryObjectSource`]/[`av_jobs::runner::
//! MemoryObjectSink`] -- the exact same in-memory test doubles `crates/av-jobs/tests/tiler.rs`
//! uses -- so this binary (and its own `mod tests` below) is fully exercised with nothing
//! running and nothing on the network. Every `--store-*` flag is refused as irrelevant in this
//! mode (a caller that passes both is almost certainly confused about which run they are
//! configuring, so this is refused rather than one silently ignored).
//!
//! # The synthetic source raster (`--synthetic-source WxH`)
//!
//! Deterministic RGB8, whole-globe bounds (`-180,-90,180,90`), built with **no randomness and
//! no clock read at all** -- the entire point is that the same `--synthetic-source` value (and
//! the same other flags) hashes to the same source, and therefore the same manifest, on every
//! run and every host. For pixel `(col, row)`, 0-indexed, `col` west-to-east, `row`
//! north-to-south, `0 <= col < width`, `0 <= row < height`:
//!
//! ```text
//! r = round(col * 255 / max(width - 1, 1))
//! g = round(row * 255 / max(height - 1, 1))
//! b = (col + row) mod 256
//! ```
//!
//! encoded into this crate's own `AVRASTER` byte layout (`av_jobs::raster`'s own module doc has
//! the exact 56-byte header this binary's own [`encode_raster`] below reproduces byte for byte
//! -- duplicated here rather than called, because `av_jobs::raster::encode` is `#[cfg(test)]`
//! only in that module, compiled for `cargo test`, never for this binary's own `cargo build`).
//!
//! # Byte accounting is real, and levels/tile size are real tuning knobs
//!
//! `total_stored_bytes` in this binary's own JSON output ([`FixtureResult::total_stored_bytes`]) is the sum of
//! `JobCompletion.outputs[].size_bytes` -- the REAL sizes the configured `ObjectSink` (a real
//! `av-store`/MinIO `put`, or this crate's own `MemoryObjectSink`) reports for what it actually
//! stored, never a value this binary computes independently and could get out of sync with
//! what was written. `--min-level`/`--max-level`/`--tile-size` are ordinary, uncapped (beyond
//! `TilerExecutor`'s own documented ceilings) command-line arguments specifically so an
//! operator can choose a level range and tile size that together produce many gigabytes.
//!
//! **What this binary does NOT achieve, and why, recorded rather than silently claimed:** the
//! current `Runner::run_one`/`Executor::execute` pipeline is not internally streaming --
//! `TilerExecutor::run_imagery` renders and PNG-encodes every tile across every requested
//! level into one `Vec<JobOutput>` before returning it, and `Runner::execute_spec` only begins
//! storing outputs (calling `ObjectSink::put`) once that whole `Vec` has come back. A single
//! `av-tile-fixture` run therefore holds one job's whole raw, uncompressed tile set in memory
//! at its peak, regardless of `--tile-size`/level range -- this binary adds no *additional*
//! buffering of its own on top of that (it never copies a tile's bytes a second time before
//! handing them to the `Runner`), but it cannot make the underlying pipeline hold less than
//! `Runner`/`TilerExecutor` already do. Making `Executor::execute` itself incremental (an
//! output callback instead of one returned `Vec`) would touch that trait's signature and every
//! existing `Executor`/test in this crate (`ProcessExecutor`, `tests/runner.rs`, `tests/
//! tiler.rs`, `tests/store_tiler.rs`) -- out of this task's own scope, and left as an open item
//! for whoever drives the next task's real ten-gigabyte run: a level range and tile size whose
//! *rendered* tile set fits in the host's available memory is required until that redesign
//! happens, not merely recommended.
//!
//! # Progress on stderr ([`av_jobs::tiler::TilerExecutor::with_progress`])
//!
//! This binary is the one caller in this workspace that constructs a `TilerExecutor` with a
//! progress callback (`crate::tiler`'s own module doc on that field): one stderr line per
//! rendered tile is too much for a large run, so this binary prints one line per completed
//! zoom level plus one line for every 5% of the run's total tile count, whichever comes first
//! for a given tile -- enough to watch a multi-gigabyte run advance without flooding the
//! terminal with one line per (possibly tiny) tile.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use av_cdm::pb;
use av_jobs::clock::{Clock, SystemClock};
use av_jobs::hash::hex_encode;
use av_jobs::queue::JobQueue;
use av_jobs::runner::{MemoryObjectSink, MemoryObjectSource, ObjectSink, ObjectSource, Runner};
use av_jobs::tiler::TilerExecutor;
use av_label::ClearanceLadder;
use av_store::{StoreClient, StoreConfig};
use bytes::Bytes;
use openssl::sha::sha256;

const RASTER_MEDIA_TYPE: &str = "application/vnd.altavista.raster+raw";
const MEMORY_RASTER_URI: &str = "memory://av-tile-fixture-source-raster";

const USAGE: &str = "usage: av-tile-fixture --key-prefix PREFIX --ladder MARKING[,MARKING...] \
                      --label-marking MARKING --job-id ID --min-level N --max-level N \
                      [--tile-size N] (--source-path PATH | --synthetic-source WxH) \
                      [--queue-dir PATH] [--dry-run] \
                      [--store-endpoint URL --store-region REGION --store-access-key-id ID \
                       --store-secret-access-key KEY --store-bucket BUCKET \
                       [--store-path-style] [--store-ca-file PATH]]";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Path(PathBuf),
    Synthetic { width: u32, height: u32 },
}

#[derive(Debug)]
struct CliArgs {
    key_prefix: Option<String>,
    ladder: Option<Vec<String>>,
    label_marking: Option<String>,
    job_id: Option<String>,
    min_level: Option<u32>,
    max_level: Option<u32>,
    tile_size: u32,
    source: Option<Source>,
    dry_run: bool,
    queue_dir: Option<PathBuf>,
    store_endpoint: Option<String>,
    store_region: Option<String>,
    store_access_key_id: Option<String>,
    store_secret_access_key: Option<String>,
    store_bucket: Option<String>,
    store_path_style: bool,
    store_ca_file: Option<PathBuf>,
}

/// Parses `"WIDTHxHEIGHT"` (both positive decimal `u32`s) -- the exact shape `--synthetic-
/// source` takes.
fn parse_wxh(raw: &str) -> Result<(u32, u32), String> {
    let (w, h) = raw.split_once('x').ok_or_else(|| format!("--synthetic-source {raw:?} must be WIDTHxHEIGHT (e.g. 64x32). {USAGE}"))?;
    let width = w.parse::<u32>().map_err(|e| format!("--synthetic-source {raw:?}: width {w:?} does not parse as u32: {e}"))?;
    let height = h.parse::<u32>().map_err(|e| format!("--synthetic-source {raw:?}: height {h:?} does not parse as u32: {e}"))?;
    if width == 0 || height == 0 {
        return Err(format!("--synthetic-source {raw:?}: width and height must both be nonzero"));
    }
    Ok((width, height))
}

fn parse_cli_args(args: impl Iterator<Item = String>) -> Result<CliArgs, String> {
    let mut out = CliArgs {
        key_prefix: None,
        ladder: None,
        label_marking: None,
        job_id: None,
        min_level: None,
        max_level: None,
        tile_size: av_jobs::tiler::DEFAULT_TILE_SIZE,
        source: None,
        dry_run: false,
        queue_dir: None,
        store_endpoint: None,
        store_region: None,
        store_access_key_id: None,
        store_secret_access_key: None,
        store_bucket: None,
        store_path_style: false,
        store_ca_file: None,
    };

    let mut args = args.skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value. {USAGE}"));
        match flag.as_str() {
            "--key-prefix" => out.key_prefix = Some(value()?),
            "--ladder" => out.ladder = Some(value()?.split(',').map(str::to_string).collect()),
            "--label-marking" => out.label_marking = Some(value()?),
            "--job-id" => out.job_id = Some(value()?),
            "--min-level" => out.min_level = Some(value()?.parse().map_err(|e| format!("--min-level: {e}"))?),
            "--max-level" => out.max_level = Some(value()?.parse().map_err(|e| format!("--max-level: {e}"))?),
            "--tile-size" => out.tile_size = value()?.parse().map_err(|e| format!("--tile-size: {e}"))?,
            "--source-path" => {
                if out.source.is_some() {
                    return Err(format!("--source-path and --synthetic-source are mutually exclusive. {USAGE}"));
                }
                out.source = Some(Source::Path(PathBuf::from(value()?)));
            }
            "--synthetic-source" => {
                if out.source.is_some() {
                    return Err(format!("--source-path and --synthetic-source are mutually exclusive. {USAGE}"));
                }
                let (width, height) = parse_wxh(&value()?)?;
                out.source = Some(Source::Synthetic { width, height });
            }
            "--queue-dir" => out.queue_dir = Some(PathBuf::from(value()?)),
            "--dry-run" => out.dry_run = true,
            "--store-endpoint" => out.store_endpoint = Some(value()?),
            "--store-region" => out.store_region = Some(value()?),
            "--store-access-key-id" => out.store_access_key_id = Some(value()?),
            "--store-secret-access-key" => out.store_secret_access_key = Some(value()?),
            "--store-bucket" => out.store_bucket = Some(value()?),
            "--store-path-style" => out.store_path_style = true,
            "--store-ca-file" => out.store_ca_file = Some(PathBuf::from(value()?)),
            other => return Err(format!("unrecognised argument {other:?}. {USAGE}")),
        }
    }

    if out.key_prefix.is_none() || out.ladder.is_none() || out.label_marking.is_none() || out.job_id.is_none() {
        return Err(format!("--key-prefix, --ladder, --label-marking and --job-id are all required. {USAGE}"));
    }
    if out.min_level.is_none() || out.max_level.is_none() {
        return Err(format!("--min-level and --max-level are both required. {USAGE}"));
    }
    if out.source.is_none() {
        return Err(format!("exactly one of --source-path or --synthetic-source is required. {USAGE}"));
    }
    let store_flags_given = out.store_endpoint.is_some()
        || out.store_region.is_some()
        || out.store_access_key_id.is_some()
        || out.store_secret_access_key.is_some()
        || out.store_bucket.is_some();
    if out.dry_run {
        if store_flags_given {
            return Err(format!("--dry-run and any --store-* flag are mutually exclusive -- a dry run never touches a store. {USAGE}"));
        }
    } else if out.store_endpoint.is_none() || out.store_region.is_none() || out.store_access_key_id.is_none() || out.store_secret_access_key.is_none() || out.store_bucket.is_none() {
        return Err(format!(
            "--store-endpoint, --store-region, --store-access-key-id, --store-secret-access-key and --store-bucket are all required unless --dry-run is given. {USAGE}"
        ));
    }
    Ok(out)
}

/// The `AVRASTER` byte layout `av_jobs::raster`'s own module doc specifies, byte for byte --
/// duplicated here (that module's own `encode` is `#[cfg(test)]`-only, not reachable from this
/// binary's `cargo build`) rather than made `pub` for a production binary to call: a raster a
/// real producer writes is data this crate only ever reads, never something the tiler itself
/// should be able to construct outside a test -- this binary is the one exception, and it
/// pays for that exception with its own small, self-contained copy instead of widening
/// `av_jobs::raster`'s own production surface for it.
fn encode_raster(width: u32, height: u32, west: f64, south: f64, east: f64, north: f64, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(56 + pixels.len());
    out.extend_from_slice(b"AVRASTER");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&west.to_le_bytes());
    out.extend_from_slice(&south.to_le_bytes());
    out.extend_from_slice(&east.to_le_bytes());
    out.extend_from_slice(&north.to_le_bytes());
    out.extend_from_slice(pixels);
    out
}

/// The deterministic synthetic pixel formula -- see this binary's own module doc for the
/// exact `r`/`g`/`b` arithmetic. `width`/`height` are the only inputs; no clock, no random
/// source, nothing ambient.
fn synthetic_pixels(width: u32, height: u32) -> Vec<u8> {
    let w_denom = width.saturating_sub(1).max(1) as f64;
    let h_denom = height.saturating_sub(1).max(1) as f64;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for row in 0..height {
        for col in 0..width {
            let r = ((col as f64 / w_denom) * 255.0).round() as u8;
            let g = ((row as f64 / h_denom) * 255.0).round() as u8;
            let b = ((col + row) % 256) as u8;
            pixels.push(r);
            pixels.push(g);
            pixels.push(b);
        }
    }
    pixels
}

/// Whole-globe bounds -- fixed, always, for a synthetic source: the only inputs that can
/// change a synthetic source's own bytes (and therefore its SHA-256) are `width`/`height`.
fn synthetic_raster_bytes(width: u32, height: u32) -> Vec<u8> {
    encode_raster(width, height, -180.0, -90.0, 180.0, 90.0, &synthetic_pixels(width, height))
}

/// Reads the source raster's bytes -- either verbatim off disk, or freshly generated by
/// [`synthetic_raster_bytes`]. Never any other source.
fn source_bytes(source: &Source) -> Result<Vec<u8>, String> {
    match source {
        Source::Path(path) => std::fs::read(path).map_err(|e| format!("--source-path {path:?}: {e}")),
        Source::Synthetic { width, height } => Ok(synthetic_raster_bytes(*width, *height)),
    }
}

fn now_unix_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock before 1970").as_secs() as i64
}

/// Guarantees a fresh, never-reused default `--queue-dir` even for two calls to [`run`]
/// inside the SAME process within the SAME wall-clock nanosecond (this binary's own `mod
/// tests` does exactly that, back to back, with no sleep between them -- a real defect this
/// counter fixes: a second-precision default made
/// `two_dry_runs_with_identical_flags_produce_the_identical_manifest_hash` fail with
/// `JobError::DuplicateJobId`, because both calls landed on the identical directory and
/// therefore the identical already-submitted `job_id`). Combined with a nanosecond
/// timestamp AND the process id, this also guards the far less likely cross-process
/// collision a bare counter alone could not.
static QUEUE_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Bridges `av-jobs`'s synchronous `ObjectSource`/`ObjectSink` seam to `av-store`'s async
/// `StoreClient` -- byte for byte `crates/av-jobs/tests/store_tiler.rs::StoreBridge`'s own
/// shape (that file's own doc comment has the full "why synchronous, why one Runtime, why
/// block_on is never nested" reasoning; restated briefly here rather than duplicated at
/// length a second time).
struct StoreBridge {
    runtime: tokio::runtime::Runtime,
    client: StoreClient,
    ladder: ClearanceLadder,
    caller_clearance: String,
}

impl std::fmt::Debug for StoreBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreBridge").finish_non_exhaustive()
    }
}

impl StoreBridge {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure> {
        let bytes = self
            .runtime
            .block_on(self.client.get(asset, &self.caller_clearance, &self.ladder, now_unix_s()))
            .map_err(|e| pb::JobFailure { kind: pb::JobFailureKind::InputMissing as i32, detail: format!("store get {:?}: {e}", asset.uri), exit_code: 0 })?;
        Ok(bytes.to_vec())
    }

    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.runtime
            .block_on(self.client.put(Bytes::copy_from_slice(bytes), media_type, label.clone(), pb::Provenance::default(), now_unix_s()))
            .map_err(|e| pb::JobFailure { kind: pb::JobFailureKind::OutputRejected as i32, detail: format!("store put: {e}"), exit_code: 0 })
    }
}

#[derive(Debug, Clone)]
struct StoreSource(Arc<StoreBridge>);
impl ObjectSource for StoreSource {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure> {
        self.0.fetch(asset)
    }
}

#[derive(Debug, Clone)]
struct StoreSink(Arc<StoreBridge>);
impl ObjectSink for StoreSink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

/// This run's own JSON stdout shape -- see this binary's own module doc, "Byte accounting is
/// real". Field declaration order here is also [`FixtureResult::to_json`]'s own output order
/// (hand-rolled, see that method's own doc for why no JSON library is pulled in for it) --
/// not load-bearing for correctness, only for a human `less`-ing the output.
#[derive(Debug)]
struct FixtureResult {
    manifest_sha256: String,
    tile_count: usize,
    total_stored_bytes: u64,
    source_sha256: String,
    object_key_prefix: String,
    min_level: u32,
    max_level: u32,
    tile_size: u32,
    job_id: String,
    dry_run: bool,
}

impl FixtureResult {
    /// Hand-rolled JSON (this crate has no `serde_json` dependency at all -- `Cargo.toml`'s own
    /// "Deliberately NOT a dependency" note on `serde_json`, unaffected by this task: this
    /// binary's every field is a string, an unsigned integer, or a bool, none of which need an
    /// escaping library to emit correctly. Every string field here is either this binary's own
    /// hex SHA-256 output or a caller-supplied CLI argument printed back verbatim -- job ids
    /// and key prefixes are not free-form untrusted text in this workspace's own convention
    /// (`heavy.proto`'s own doc comments), but they are escaped anyway, defensively, rather
    /// than assumed safe.
    fn to_json(&self) -> String {
        fn esc(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out
        }
        format!(
            "{{\"manifest_sha256\":\"{}\",\"tile_count\":{},\"total_stored_bytes\":{},\"source_sha256\":\"{}\",\"object_key_prefix\":\"{}\",\"min_level\":{},\"max_level\":{},\"tile_size\":{},\"job_id\":\"{}\",\"dry_run\":{}}}",
            esc(&self.manifest_sha256),
            self.tile_count,
            self.total_stored_bytes,
            esc(&self.source_sha256),
            esc(&self.object_key_prefix),
            self.min_level,
            self.max_level,
            self.tile_size,
            esc(&self.job_id),
            self.dry_run,
        )
    }
}

fn run(cli: CliArgs) -> Result<FixtureResult, String> {
    let key_prefix = cli.key_prefix.clone().expect("parse_cli_args checked this");
    let ladder = ClearanceLadder::new(cli.ladder.clone().expect("parse_cli_args checked this"));
    let label_marking = cli.label_marking.clone().expect("parse_cli_args checked this");
    let job_id = cli.job_id.clone().expect("parse_cli_args checked this");
    let min_level = cli.min_level.expect("parse_cli_args checked this");
    let max_level = cli.max_level.expect("parse_cli_args checked this");
    let source = cli.source.clone().expect("parse_cli_args checked this");
    let label = pb::Label { marking: label_marking.clone(), caveats: vec![] };

    if ladder.rank(&label_marking).is_none() {
        return Err(format!("--label-marking {label_marking:?} is not on the --ladder {:?}", cli.ladder));
    }

    eprintln!("av-tile-fixture: reading source raster ({:?}) ...", source);
    let raster_bytes = source_bytes(&source)?;
    let source_sha256 = hex_encode(&sha256(&raster_bytes));
    eprintln!("av-tile-fixture: source raster is {} byte(s), sha256={}", raster_bytes.len(), source_sha256);

    let queue_dir = cli.queue_dir.clone().unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock before 1970").as_nanos();
        let n = QUEUE_DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("av-tile-fixture-queue-{}-{nanos}-{n}", std::process::id()))
    });
    eprintln!("av-tile-fixture: job queue at {queue_dir:?}");
    let (queue, recovery) = JobQueue::open(&queue_dir, "av-tile-fixture").map_err(|e| format!("JobQueue::open({queue_dir:?}): {e}"))?;
    if let Some(report) = recovery {
        eprintln!("av-tile-fixture: NOTE queue recovery discarded {} byte(s): {:?}", report.discarded_bytes, report.reason);
    }

    let (source_obj, sink_obj, raster_asset): (Box<dyn ObjectSource>, Box<dyn ObjectSink>, pb::AssetRef) = if cli.dry_run {
        eprintln!("av-tile-fixture: --dry-run -- using the in-memory ObjectSource/ObjectSink, no store, no container, no network");
        let mem_source = MemoryObjectSource::new();
        mem_source.insert(MEMORY_RASTER_URI, raster_bytes.clone());
        let asset = pb::AssetRef {
            uri: MEMORY_RASTER_URI.to_string(),
            sha256: source_sha256.clone(),
            size_bytes: raster_bytes.len() as u64,
            media_type: RASTER_MEDIA_TYPE.to_string(),
            label: Some(label.clone()),
            ..Default::default()
        };
        (Box::new(mem_source), Box::new(MemoryObjectSink::new(key_prefix.clone())), asset)
    } else {
        let endpoint = cli.store_endpoint.clone().expect("parse_cli_args checked this");
        eprintln!("av-tile-fixture: connecting to the store at {endpoint} (bucket {:?}) ...", cli.store_bucket);
        let store_config = StoreConfig {
            endpoint: endpoint.parse().map_err(|e| format!("--store-endpoint {endpoint:?}: {e}"))?,
            region: cli.store_region.clone().expect("parse_cli_args checked this"),
            access_key_id: cli.store_access_key_id.clone().expect("parse_cli_args checked this"),
            secret_access_key: cli.store_secret_access_key.clone().expect("parse_cli_args checked this"),
            bucket: cli.store_bucket.clone().expect("parse_cli_args checked this"),
            force_path_style: cli.store_path_style,
            ca_file: cli.store_ca_file.clone(),
            key_prefix: key_prefix.clone(),
        };
        let client = StoreClient::new(store_config).map_err(|e| format!("building the store client: {e}"))?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("building the tokio Runtime: {e}"))?;
        runtime.block_on(client.ensure_bucket(now_unix_s())).map_err(|e| format!("ensure_bucket: {e}"))?;

        eprintln!("av-tile-fixture: storing the source raster ({} byte(s)) ...", raster_bytes.len());
        let raster_asset = runtime
            .block_on(client.put(Bytes::from(raster_bytes.clone()), RASTER_MEDIA_TYPE, label.clone(), pb::Provenance::default(), now_unix_s()))
            .map_err(|e| format!("put(source raster): {e}"))?;
        eprintln!("av-tile-fixture: source raster stored at {}", raster_asset.uri);

        // The ladder's own highest rung: guarantees this bridge's own later `fetch` calls
        // (the Runner re-reading back every input/output it just wrote, to hash-verify it --
        // see `Runner::run_one`'s own doc) are never refused by the store's OWN label check,
        // regardless of `--label-marking`'s own rank. This binary is offline tooling with one
        // caller (itself, in this one process); it is not standing in for a deployed reader's
        // real clearance.
        let caller_clearance = cli.ladder.clone().expect("parse_cli_args checked this").last().cloned().unwrap_or_else(|| label_marking.clone());
        let bridge = Arc::new(StoreBridge { runtime, client, ladder: ladder.clone(), caller_clearance });
        (Box::new(StoreSource(bridge.clone())), Box::new(StoreSink(bridge.clone())), raster_asset)
    };

    let tile_size = cli.tile_size;
    eprintln!("av-tile-fixture: running the tiler job (job_id={job_id:?}, levels {min_level}..={max_level}, tile_size={tile_size}) ...");

    let mut parameters: BTreeMap<String, String> = BTreeMap::new();
    parameters.insert("min_level".to_string(), min_level.to_string());
    parameters.insert("max_level".to_string(), max_level.to_string());
    parameters.insert("tile_size".to_string(), tile_size.to_string());
    parameters.insert("output".to_string(), "imagery".to_string());

    let clock = SystemClock;
    let spec = pb::JobSpec {
        job_id: job_id.clone(),
        kind: "tiler".to_string(),
        inputs: vec![raster_asset],
        parameters,
        label: Some(label),
        requested_tai_ns: clock.now_tai_ns(),
        executor: pb::JobExecutorKind::Process as i32,
        command: vec![],
        container_image: String::new(),
        container_image_digest: String::new(),
    };

    let mut runner = Runner::new(queue, source_obj, sink_obj, ladder, &clock);
    // See this binary's own module doc, "Progress on stderr": one line per completed level,
    // plus one line for every 5% of this run's own total tile count, so a large run's own
    // progress is genuinely observable without one line per (possibly tiny) tile.
    // `AtomicI64`, not `Cell`, because the progress callback's own type
    // (`Box<dyn Fn(..) + Send + Sync>`, `crate::tiler::TilerExecutor`'s own field type) must
    // be `Sync` regardless of the fact that `Runner::execute_spec` only ever calls it from
    // one thread -- a `Cell` capture would make the whole closure (and therefore the
    // `TilerExecutor` holding it) not `Sync`, refusing to compile. -1 means "no level
    // reported yet" (a real level is always `>= 0` as a `u32`).
    let last_level_reported = AtomicI64::new(-1);
    let executor = TilerExecutor::with_progress(key_prefix.clone(), move |level, tiles_done, tiles_total| {
        let five_percent_step = (tiles_total / 20).max(1);
        let is_new_level = last_level_reported.load(Ordering::Relaxed) != level as i64;
        if is_new_level || tiles_done.is_multiple_of(five_percent_step) || tiles_done == tiles_total {
            eprintln!("av-tile-fixture: rendering level {level}: tile {tiles_done} of {tiles_total}");
            last_level_reported.store(level as i64, Ordering::Relaxed);
        }
    });
    runner.register_executor("tiler", Box::new(executor));

    runner.queue().submit(&spec).map_err(|e| format!("submit: {e}"))?;
    let completion = runner.run_one(&spec).map_err(|e| format!("run_one (queue append failed -- infrastructure, not a job failure): {e}"))?;

    if !completion.ok {
        let failure = completion.failure.unwrap_or_default();
        return Err(format!("the tiler job itself failed: kind={} detail={:?} exit_code={}", failure.kind, failure.detail, failure.exit_code));
    }
    if completion.manifest_sha256.is_empty() {
        return Err("the job completed ok but produced no manifest_sha256 -- this should be impossible for a successful \"tiler\" job".to_string());
    }

    let tile_count = completion.outputs.iter().filter(|a| a.media_type == av_jobs::tiler::IMAGERY_TILE_MEDIA_TYPE).count();
    let total_stored_bytes: u64 = completion.outputs.iter().map(|a| a.size_bytes).sum();
    eprintln!("av-tile-fixture: job complete: {tile_count} tile(s), {total_stored_bytes} byte(s) stored, manifest_sha256={}", completion.manifest_sha256);

    Ok(FixtureResult {
        manifest_sha256: completion.manifest_sha256,
        tile_count,
        total_stored_bytes,
        source_sha256,
        object_key_prefix: key_prefix,
        min_level,
        max_level,
        tile_size,
        job_id,
        dry_run: cli.dry_run,
    })
}

fn main() {
    let cli = parse_cli_args(std::env::args()).unwrap_or_else(|e| {
        eprintln!("av-tile-fixture: {e}");
        std::process::exit(1);
    });
    match run(cli) {
        Ok(result) => println!("{}", result.to_json()),
        Err(e) => {
            eprintln!("av-tile-fixture: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_ARGS: &[&str] = &[
        "av-tile-fixture",
        "--key-prefix",
        "tiles",
        "--ladder",
        "UNCLASSIFIED,CUI,SECRET",
        "--label-marking",
        "CUI",
        "--job-id",
        "job-1",
        "--min-level",
        "0",
        "--max-level",
        "1",
        "--synthetic-source",
        "4x2",
        "--dry-run",
    ];

    fn args(extra: &[&str]) -> impl Iterator<Item = String> {
        REQUIRED_ARGS.iter().chain(extra.iter()).map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    // -- argument parsing -------------------------------------------------------------------

    #[test]
    fn every_required_flag_missing_is_refused() {
        let err = parse_cli_args(["av-tile-fixture".to_string()].into_iter()).unwrap_err();
        assert!(err.contains("--key-prefix"), "{err}");
    }

    #[test]
    fn a_fully_specified_dry_run_command_line_parses() {
        let cli = parse_cli_args(args(&[])).unwrap();
        assert_eq!(cli.key_prefix, Some("tiles".to_string()));
        assert_eq!(cli.ladder, Some(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]));
        assert_eq!(cli.label_marking, Some("CUI".to_string()));
        assert_eq!(cli.job_id, Some("job-1".to_string()));
        assert_eq!(cli.min_level, Some(0));
        assert_eq!(cli.max_level, Some(1));
        assert_eq!(cli.tile_size, av_jobs::tiler::DEFAULT_TILE_SIZE);
        assert_eq!(cli.source, Some(Source::Synthetic { width: 4, height: 2 }));
        assert!(cli.dry_run);
    }

    #[test]
    fn source_path_and_synthetic_source_are_mutually_exclusive() {
        let err = parse_cli_args(args(&["--source-path", "/dev/null"])).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn dry_run_and_any_store_flag_are_mutually_exclusive() {
        let err = parse_cli_args(args(&["--store-endpoint", "http://127.0.0.1:9000"])).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn missing_min_or_max_level_is_refused() {
        let base: Vec<&str> = vec!["av-tile-fixture", "--key-prefix", "tiles", "--ladder", "CUI", "--label-marking", "CUI", "--job-id", "job-1", "--synthetic-source", "2x2", "--dry-run"];
        let err = parse_cli_args(base.into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--min-level"), "{err}");
    }

    #[test]
    fn neither_source_flag_at_all_is_refused() {
        let base: Vec<&str> = vec!["av-tile-fixture", "--key-prefix", "tiles", "--ladder", "CUI", "--label-marking", "CUI", "--job-id", "job-1", "--min-level", "0", "--max-level", "0", "--dry-run"];
        let err = parse_cli_args(base.into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--source-path or --synthetic-source"), "{err}");
    }

    #[test]
    fn a_non_dry_run_command_line_without_store_flags_is_refused() {
        let base: Vec<&str> = vec![
            "av-tile-fixture",
            "--key-prefix",
            "tiles",
            "--ladder",
            "CUI",
            "--label-marking",
            "CUI",
            "--job-id",
            "job-1",
            "--min-level",
            "0",
            "--max-level",
            "0",
            "--synthetic-source",
            "2x2",
        ];
        let err = parse_cli_args(base.into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--store-endpoint"), "{err}");
    }

    #[test]
    fn an_explicit_tile_size_overrides_the_default() {
        let cli = parse_cli_args(args(&["--tile-size", "16"])).unwrap();
        assert_eq!(cli.tile_size, 16);
    }

    #[test]
    fn an_unrecognised_flag_is_refused() {
        let err = parse_cli_args(args(&["--not-a-real-flag"])).unwrap_err();
        assert!(err.contains("unrecognised"), "{err}");
    }

    #[test]
    fn synthetic_source_rejects_a_malformed_wxh() {
        assert!(parse_wxh("garbage").is_err());
        assert!(parse_wxh("4x").is_err());
        assert!(parse_wxh("x4").is_err());
        assert!(parse_wxh("0x4").is_err(), "zero width must be refused");
        assert!(parse_wxh("4x0").is_err(), "zero height must be refused");
        assert_eq!(parse_wxh("64x32").unwrap(), (64, 32));
    }

    // -- synthetic-source determinism (the whole point of a synthetic source) ---------------

    #[test]
    fn the_same_synthetic_source_flags_produce_byte_identical_bytes_every_time() {
        let a = synthetic_raster_bytes(37, 19);
        let b = synthetic_raster_bytes(37, 19);
        assert_eq!(a, b, "the same --synthetic-source WxH must produce byte-identical raster bytes every run");
        let a_hash = hex_encode(&sha256(&a));
        let b_hash = hex_encode(&sha256(&b));
        assert_eq!(a_hash, b_hash);
    }

    #[test]
    fn different_synthetic_source_dimensions_produce_different_source_hashes() {
        let a = hex_encode(&sha256(&synthetic_raster_bytes(4, 2)));
        let b = hex_encode(&sha256(&synthetic_raster_bytes(4, 3)));
        assert_ne!(a, b, "a different height must change the source raster's own bytes, and therefore its hash");
    }

    #[test]
    fn synthetic_raster_bytes_decode_as_a_well_formed_raster() {
        // Round-trips through this crate's own real decoder -- proves encode_raster's hand-
        // written header really matches av_jobs::raster's documented layout, not merely this
        // file's own belief that it does.
        let bytes = synthetic_raster_bytes(8, 4);
        let raster = av_jobs::raster::decode(&bytes).expect("encode_raster must produce a raster av_jobs::raster::decode accepts");
        assert_eq!(raster.width, 8);
        assert_eq!(raster.height, 4);
        assert_eq!(raster.west, -180.0);
        assert_eq!(raster.south, -90.0);
        assert_eq!(raster.east, 180.0);
        assert_eq!(raster.north, 90.0);
    }

    #[test]
    fn synthetic_pixels_formula_matches_the_documented_corners() {
        // 3x3: width-1 = 2, height-1 = 2 -- exact corner values per this file's own doc
        // formula, computed here independently of synthetic_pixels itself.
        let pixels = synthetic_pixels(3, 3);
        let px = |col: u32, row: u32| -> [u8; 3] {
            let idx = (row as usize * 3 + col as usize) * 3;
            [pixels[idx], pixels[idx + 1], pixels[idx + 2]]
        };
        assert_eq!(px(0, 0), [0, 0, 0]); // r=0/2*255=0, g=0/2*255=0, b=(0+0)%256=0
        assert_eq!(px(2, 0), [255, 0, 2]); // r=2/2*255=255, g=0, b=(2+0)%256=2
        assert_eq!(px(0, 2), [0, 255, 2]); // r=0, g=2/2*255=255, b=(0+2)%256=2
        assert_eq!(px(2, 2), [255, 255, 4]); // r=255, g=255, b=(2+2)%256=4
    }

    // -- the end-to-end dry-run itself (no store, no container) -----------------------------

    #[test]
    fn a_dry_run_produces_a_real_manifest_hash_with_a_positive_tile_count_and_byte_total() {
        let cli = parse_cli_args(args(&[])).unwrap();
        let result = run(cli).expect("a fully-specified --dry-run must succeed");
        assert_eq!(result.manifest_sha256.len(), 64, "{}", result.manifest_sha256);
        assert!(result.tile_count > 0, "a 4x2 synthetic source at levels 0..=1 must produce at least one tile");
        assert!(result.total_stored_bytes > 0);
        assert_eq!(result.source_sha256.len(), 64);
        assert_eq!(result.object_key_prefix, "tiles");
        assert_eq!(result.job_id, "job-1");
        assert!(result.dry_run);
    }

    #[test]
    fn two_dry_runs_with_identical_flags_produce_the_identical_manifest_hash() {
        let result_a = run(parse_cli_args(args(&[])).unwrap()).unwrap();
        let result_b = run(parse_cli_args(args(&[])).unwrap()).unwrap();
        assert_eq!(result_a.manifest_sha256, result_b.manifest_sha256, "identical flags must produce the identical tile-set identity");
        assert_eq!(result_a.source_sha256, result_b.source_sha256);
    }

    #[test]
    fn a_label_marking_off_the_ladder_is_refused_before_any_job_runs() {
        // REQUIRED_ARGS' own "--label-marking CUI" is followed by this override --
        // parse_cli_args' own match arms simply overwrite on a repeated flag, so the LAST
        // occurrence wins, exactly like `crates/av-tiles/src/bin/av-tiles.rs`'s identical
        // repeated-flag convention.
        let cli = parse_cli_args(args(&["--label-marking", "TOP-SECRET"])).unwrap();
        assert_eq!(cli.label_marking, Some("TOP-SECRET".to_string()));
        let err = run(cli).unwrap_err();
        assert!(err.contains("not on the"), "{err}");
    }
}

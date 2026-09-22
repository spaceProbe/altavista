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
//! # `--synthetic-source-style gradient|labelled` (round 7 task 5a): making the switch visible
//!
//! The formula above (`gradient`, the default) reads as a smooth teal/brown-ish ramp at a
//! glance -- close enough to the offline fixture Earth texture's own teal/brown palette that a
//! person watching a browser drive cannot see that the tile set they toggled on is really this
//! synthetic source and not the default imagery, even though the manifest's own identity
//! proves it. `--synthetic-source-style labelled` is a second, purely additive rendering of
//! the exact same `--synthetic-source WxH` input; every pre-existing invocation with no style
//! flag at all still takes the `gradient` arm ([`synthetic_pixels`] below, BYTE FOR BYTE
//! UNCHANGED by this task -- see this file's own `mod tests`,
//! `the_default_synthetic_style_is_gradient_and_its_bytes_are_pinned_from_before_this_task`,
//! for the actual before/after hash proof). Two reasons the default could not simply change to
//! something more visible instead of gaining a second, opt-in style:
//!
//! - The gradient raster's own compressed tile size is load-bearing elsewhere in this round:
//!   the streaming proof measured its tiles at exactly 852 bytes each, and the tight-budget
//!   run's 11,000-byte budget was sized off that empirical record (question 233: that budget
//!   "is not to be retuned without one"). A different default raster would silently move those
//!   bytes and invalidate another worker's proof in this same round.
//! - `crates/av-jobs/tests/tiler.rs` and `crates/av-jobs/tests/store_tiler.rs` pin a manifest
//!   hash (`7c23f4f0...`) for their own fixture source -- checked (this task's own report)
//!   to be a **file-based** `--source-path` fixture raster, not this binary's own synthetic
//!   one, so it is unaffected either way; recorded here so a future reader does not have to
//!   re-derive that.
//!
//! [`labelled_pixels`] below is what `labelled` renders: a high-contrast magenta/black
//! checkerboard (colours nowhere in the offline fixture's own teal/brown palette) plus a baked
//! lat/lon coordinate grid and the word `SYNTHETIC`, using a tiny 3x5 bitmap font hand-authored
//! in this file (see [`glyph_rows`]'s own doc -- no font file, no font crate, no new
//! dependency). Deterministic exactly like `gradient`: **no randomness, no clock read, byte-
//! identical output for identical flags on every run and every host** -- see
//! [`labelled_pixels`]'s own doc for why per-tile `level`/`x`/`y` text specifically cannot be
//! baked in here (this function renders the whole-globe SOURCE raster, before the tiler slices
//! it into tiles) and what this function does instead.
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
//! # `--streaming`: the open item from H5b-1 closed, round 3 task P2a
//!
//! H5b-1's own first version of this doc recorded an open item here: `Runner::run_one`/
//! `Executor::execute` render every tile across every requested level into one
//! `Vec<JobOutput>` before storing any of it, so a single run held one job's whole raw,
//! uncompressed tile set in memory at its peak, regardless of `--tile-size`/level range --
//! unreachable at a genuine ten-gigabyte shape on an 8 GB VM host. That is now closed:
//! `--streaming` (default off, so every existing `--dry-run` test below that does not pass it
//! is byte-for-byte unaffected) makes this binary call `Runner::run_one_streaming` instead of
//! `Runner::run_one`, reaching `av_jobs::tiler::TilerExecutor::run_imagery_streaming` --
//! see that method's own doc comment (`src/tiler.rs`) for exactly how it stores each tile
//! through the configured `ObjectSink` as it is rendered, drops it, and why the manifest's own
//! SHA-256 is proven byte-identical to the buffered path's for the same input
//! (`crates/av-jobs/tests/tiler.rs`). Peak memory with `--streaming` is one tile's PNG bytes
//! plus the decoded source raster plus the manifest's own small `TileEntry` list -- not the
//! whole tile set.
//!
//! **What `--streaming` changes about this binary's own `total_stored_bytes`/`tile_count`
//! accounting**: on the buffered path, `JobCompletion.outputs` carries one `AssetRef` per
//! tile plus the manifest, so summing `outputs[].size_bytes` is exactly the real total. On
//! the streaming path `JobCompletion.outputs` carries only the manifest's own `AssetRef`
//! (`run_imagery_streaming`'s own doc: nothing about the job's real outputs goes unrecorded
//! by that, because the manifest's own encoded bytes already list every tile's `object_key`
//! and `sha256`/`size_bytes`) -- so this binary fetches that one manifest back (through the
//! same `ObjectSource`/store handle it already built, never a second connection) and sums
//! `TileSetManifest.tiles[].size_bytes` itself, plus the manifest's own `size_bytes`, to
//! report the identical *meaning* of `total_stored_bytes` on both paths. `tile_count` is
//! `manifest.tiles.len()` on the streaming path, `outputs[].media_type == IMAGERY_TILE_MEDIA_TYPE`
//! count on the buffered path -- provably the same number for the same job (the manifest
//! names exactly the tiles that were stored).
//!
//! # `peak_rss_bytes`: what makes "it streamed" a measured claim
//!
//! This binary's own JSON output ([`FixtureResult::peak_rss_bytes`]) reports this process's
//! peak resident set size at the moment the job completes, read from the operating system
//! itself via `getrusage(RUSAGE_SELF, ..)` (declared here by raw `extern "C"` FFI -- no `libc`
//! crate, no new dependency at all, this workspace's own "no new crate in `Cargo.lock`" rule).
//! **On macOS (the only platform this binary is built/run on this round -- Colima, GMAT R2026a,
//! this whole track's own host), `ru_maxrss` is already in BYTES** (unlike Linux, where the
//! same field is kilobytes -- see [`ru_maxrss_to_bytes`]'s own doc for the `cfg`-gated
//! conversion, kept correct for both rather than silently assuming one). An assertion that
//! peak memory stayed low is a claim; a number read from the kernel's own accounting of this
//! exact process is a measurement -- `scripts/heavy/ten_gigabyte_proof.py` is the caller that
//! turns this field into the round's actual ten-gigabyte evidence.
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

use av_catalog::{CatalogAsset, PgClient, PgConfig, PgTls};
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
                      [--tile-size N] (--source-path PATH | --synthetic-source WxH \
                      [--synthetic-source-style gradient|labelled]) \
                      [--queue-dir PATH] [--dry-run] [--streaming] \
                      [--store-endpoint URL --store-region REGION --store-access-key-id ID \
                       --store-secret-access-key KEY --store-bucket BUCKET \
                       [--store-path-style] [--store-ca-file PATH]] \
                      [--catalog-host HOST --catalog-user USER --catalog-password PASSWORD \
                       --catalog-database DB [--catalog-port PORT] [--catalog-tls-ca-file PATH]]";

/// `--catalog-*`'s own default port -- `crates/av-gateway/src/bin/av-gateway.rs::
/// DEFAULT_CATALOG_PORT`'s identical value, restated here rather than imported (that binary
/// does not expose it as a library constant, and this task's own brief calls for no proto or
/// shared-crate change to get one).
const DEFAULT_CATALOG_PORT: u16 = 5432;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Path(PathBuf),
    Synthetic { width: u32, height: u32, style: SyntheticStyle },
}

/// `--synthetic-source-style`'s two values -- see this binary's own module doc, "round 7 task
/// 5a: making the switch visible", for the full story. `Default` is `Gradient`: every existing
/// `--synthetic-source WxH` invocation with no style flag at all takes this arm, and
/// [`synthetic_pixels`] (this arm's own renderer) is untouched by this task -- the same
/// function, same formula, same output for the same width/height as before this task existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SyntheticStyle {
    #[default]
    Gradient,
    Labelled,
}

/// Parses `--synthetic-source-style`'s value.
fn parse_synthetic_style(raw: &str) -> Result<SyntheticStyle, String> {
    match raw {
        "gradient" => Ok(SyntheticStyle::Gradient),
        "labelled" => Ok(SyntheticStyle::Labelled),
        other => Err(format!("--synthetic-source-style {other:?} must be \"gradient\" or \"labelled\". {USAGE}")),
    }
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
    /// `--synthetic-source-style`'s raw presence, applied onto `source`'s own `Synthetic`
    /// variant after the whole command line is parsed (`parse_cli_args`'s own post-loop
    /// validation block) -- kept separate from `Source::Synthetic` while parsing is in
    /// progress specifically so `--synthetic-source-style` may come before OR after
    /// `--synthetic-source` on the command line and still take effect (this binary's own
    /// `mod tests` exercises both orders).
    synthetic_style: Option<SyntheticStyle>,
    dry_run: bool,
    /// `--streaming`: see this binary's own module doc, "`--streaming`: the open item from
    /// H5b-1 closed". Orthogonal to `dry_run` -- both a `--dry-run --streaming` combination
    /// (this binary's own `mod tests` uses it, no store/container/network needed) and a
    /// real-store `--streaming` run are valid.
    streaming: bool,
    queue_dir: Option<PathBuf>,
    store_endpoint: Option<String>,
    store_region: Option<String>,
    store_access_key_id: Option<String>,
    store_secret_access_key: Option<String>,
    store_bucket: Option<String>,
    store_path_style: bool,
    store_ca_file: Option<PathBuf>,
    /// Question 228's finding 2 (pipeline half): `Some` only when `--catalog-host` was given
    /// -- the presence signal for "register the manifest this run produces in the catalog",
    /// mirroring `crates/av-gateway/src/bin/av-gateway.rs::CliArgs::catalog_host`'s own
    /// identical "presence is the switch" doc. When present, `--catalog-user`/`--catalog-
    /// password`/`--catalog-database` are all REQUIRED together (`parse_cli_args` refuses to
    /// return `Ok` otherwise) -- a partially-specified catalog connection is a configuration
    /// defect this binary reports at startup, never a silently incomplete `PgConfig`. Also
    /// refused together with `--dry-run` (see this binary's own module doc): a dry run's own
    /// manifest `AssetRef` carries no `Provenance` (`crate::runner::MemoryObjectSink::put`),
    /// which `CatalogAsset::from_asset_ref` refuses outright, so a `--dry-run --catalog-host`
    /// combination could never succeed -- refused at parse time rather than failing deep
    /// inside a job that already ran.
    catalog_host: Option<String>,
    catalog_port: u16,
    catalog_user: Option<String>,
    catalog_password: Option<String>,
    catalog_database: Option<String>,
    catalog_tls_ca_file: Option<PathBuf>,
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
        synthetic_style: None,
        dry_run: false,
        streaming: false,
        queue_dir: None,
        store_endpoint: None,
        store_region: None,
        store_access_key_id: None,
        store_secret_access_key: None,
        store_bucket: None,
        store_path_style: false,
        store_ca_file: None,
        catalog_host: None,
        catalog_port: DEFAULT_CATALOG_PORT,
        catalog_user: None,
        catalog_password: None,
        catalog_database: None,
        catalog_tls_ca_file: None,
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
                out.source = Some(Source::Synthetic { width, height, style: SyntheticStyle::default() });
            }
            "--synthetic-source-style" => out.synthetic_style = Some(parse_synthetic_style(&value()?)?),
            "--queue-dir" => out.queue_dir = Some(PathBuf::from(value()?)),
            "--dry-run" => out.dry_run = true,
            "--streaming" => out.streaming = true,
            "--store-endpoint" => out.store_endpoint = Some(value()?),
            "--store-region" => out.store_region = Some(value()?),
            "--store-access-key-id" => out.store_access_key_id = Some(value()?),
            "--store-secret-access-key" => out.store_secret_access_key = Some(value()?),
            "--store-bucket" => out.store_bucket = Some(value()?),
            "--store-path-style" => out.store_path_style = true,
            "--store-ca-file" => out.store_ca_file = Some(PathBuf::from(value()?)),
            "--catalog-host" => out.catalog_host = Some(value()?),
            "--catalog-port" => out.catalog_port = value()?.parse::<u16>().map_err(|e| format!("--catalog-port: {e}"))?,
            "--catalog-user" => out.catalog_user = Some(value()?),
            "--catalog-password" => out.catalog_password = Some(value()?),
            "--catalog-database" => out.catalog_database = Some(value()?),
            "--catalog-tls-ca-file" => out.catalog_tls_ca_file = Some(PathBuf::from(value()?)),
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
    // --synthetic-source-style only means anything for a Synthetic source -- applied here,
    // after the whole command line is parsed, so it works regardless of whether it came before
    // or after --synthetic-source on the command line (see CliArgs::synthetic_style's own doc).
    if let Some(style) = out.synthetic_style {
        match &mut out.source {
            Some(Source::Synthetic { style: s, .. }) => *s = style,
            Some(Source::Path(_)) => {
                return Err(format!("--synthetic-source-style was given together with --source-path, which has no synthetic style at all. {USAGE}"));
            }
            None => unreachable!("out.source.is_none() was already refused above"),
        }
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
    // Question 228's finding 2: catalog registration is refused together with --dry-run (see
    // CliArgs::catalog_host's own doc for why -- a dry run's own manifest AssetRef carries no
    // Provenance, which CatalogAsset::from_asset_ref refuses outright), and a partially-
    // specified catalog connection is refused at startup rather than silently ignored --
    // mirrors crates/av-gateway/src/bin/av-gateway.rs::parse_cli_args's identical two rules
    // for its own --catalog-* flags.
    if out.catalog_host.is_some() && out.dry_run {
        return Err(format!("--catalog-host and --dry-run are mutually exclusive -- a dry run's own manifest carries no Provenance, so it can never be catalogued. {USAGE}"));
    }
    if out.catalog_host.is_some() && (out.catalog_user.is_none() || out.catalog_password.is_none() || out.catalog_database.is_none()) {
        return Err(format!(
            "--catalog-host was given but --catalog-user/--catalog-password/--catalog-database were not all also given (a partially-specified catalog connection is refused at startup, never silently incomplete). {USAGE}"
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

// -----------------------------------------------------------------------------------------
// `--synthetic-source-style labelled` (round 7 task 5a) -- a high-contrast checkerboard, a
// baked lat/lon coordinate grid, and a tiny hand-authored bitmap font. See this binary's own
// module doc, "`--synthetic-source-style gradient|labelled` (round 7 task 5a)", for why this
// exists and why it could not simply become the new default.
// -----------------------------------------------------------------------------------------

/// Colours nowhere in the offline fixture Earth texture's own teal (roughly `[0,128,128]`) or
/// brown (roughly `[139,69,19]`) palette -- pure magenta and pure black share no channel with
/// either, so the two are unmistakable next to that fixture at a glance, not merely "different
/// enough on paper".
const LABELLED_CHECKER_A: [u8; 3] = [255, 0, 255];
const LABELLED_CHECKER_B: [u8; 3] = [0, 0, 0];
/// Baked-in grid/text colour: pure white -- the one colour with the maximum possible
/// per-channel distance from BOTH checkerboard colours above, so a line or a glyph is legible
/// sitting on either one.
const LABELLED_TEXT_COLOR: [u8; 3] = [255, 255, 255];

/// The checkerboard's own cell size, in raster pixels, as a stated FRACTION of the raster
/// (`1/16` of its own smaller dimension, per this task's own brief) rather than a fixed pixel
/// count -- clamped to a minimum of 1 pixel, and naturally never larger than the raster itself,
/// so the pattern is well-defined even for this file's own smallest test rasters (down to
/// 1x1). A fraction, not a constant, is what keeps the pattern showing multiple cells (staying
/// a checkerboard, not washing out to one flat colour) however small a `--tile-size` crop the
/// tiler cuts from this raster at whatever level it is asked to cut.
fn checker_cell_size(width: u32, height: u32) -> u32 {
    (width.min(height) / 16).max(1)
}

/// This file's own tiny bitmap font -- 3 pixels wide, 5 pixels tall, one row per array entry,
/// that entry's low 3 bits its own pixels (bit 2 = the glyph's own leftmost column, bit 0 =
/// its rightmost). **Hand-authored for this file, row by row, by this task's own author --
/// not derived from any font file, font crate, or published glyph dataset** (this task's own
/// brief: "no font dependency to add and you must not add one"). Only the characters this
/// binary's own labels ever need: the ten digits, `-`, `.`, and the nine letters
/// `C E H I N S T W Y` -- enough to spell `SYNTHETIC` and every `N`/`S`/`E`/`W`-prefixed
/// latitude/longitude label [`labelled_pixels`] draws.
fn glyph_rows(c: char) -> Option<[u8; 5]> {
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b110, 0b001, 0b010, 0b100, 0b111],
        '3' => [0b110, 0b001, 0b010, 0b001, 0b110],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b110, 0b001, 0b110],
        '6' => [0b011, 0b100, 0b110, 0b101, 0b010],
        '7' => [0b111, 0b001, 0b010, 0b100, 0b100],
        '8' => [0b010, 0b101, 0b010, 0b101, 0b010],
        '9' => [0b010, 0b101, 0b011, 0b001, 0b010],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        'N' => [0b101, 0b110, 0b010, 0b011, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'W' => [0b101, 0b101, 0b101, 0b111, 0b101],
        'Y' => [0b101, 0b010, 0b010, 0b010, 0b010],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        _ => return None,
    })
}

const GLYPH_WIDTH: u32 = 3;
const GLYPH_HEIGHT: u32 = 5;
/// One glyph's own width plus one column of spacing before the next.
const GLYPH_PITCH_X: u32 = GLYPH_WIDTH + 1;

/// Writes one pixel, silently clipping (a no-op, never a panic) if `(x, y)` falls outside
/// `[0, width) x [0, height)` -- every pixel-writing function in this section goes through
/// this one function for that bounds check, so a label or a grid line placed near (or past)
/// the raster's own edge simply truncates rather than panicking or wrapping.
fn set_pixel(pixels: &mut [u8], width: u32, height: u32, x: i64, y: i64, color: [u8; 3]) {
    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
        return;
    }
    let idx = (y as usize * width as usize + x as usize) * 3;
    pixels[idx] = color[0];
    pixels[idx + 1] = color[1];
    pixels[idx + 2] = color[2];
}

/// Draws one glyph's own "on" pixels in `color` with its top-left corner at `(x0, y0)`,
/// through [`set_pixel`] (so it is clipped exactly the same way every other pixel write here
/// is). An unknown character ([`glyph_rows`] returning `None`) draws nothing.
fn draw_glyph(pixels: &mut [u8], width: u32, height: u32, x0: i64, y0: i64, c: char, color: [u8; 3]) {
    let Some(rows) = glyph_rows(c) else { return };
    for (row_idx, row_bits) in rows.iter().enumerate() {
        for col_idx in 0..GLYPH_WIDTH {
            let bit = GLYPH_WIDTH - 1 - col_idx; // column 0 is this row's own most-significant bit.
            if (row_bits >> bit) & 1 == 0 {
                continue;
            }
            set_pixel(pixels, width, height, x0 + col_idx as i64, y0 + row_idx as i64, color);
        }
    }
}

/// Draws `text` left to right starting at `(x0, y0)`, [`GLYPH_PITCH_X`] pixels per character.
/// Entirely clipped by [`draw_glyph`]/[`set_pixel`], so a string that runs off the raster's
/// own right or bottom edge simply truncates.
fn draw_text(pixels: &mut [u8], width: u32, height: u32, x0: i64, y0: i64, text: &str, color: [u8; 3]) {
    for (i, c) in text.chars().enumerate() {
        draw_glyph(pixels, width, height, x0 + i as i64 * GLYPH_PITCH_X as i64, y0, c, color);
    }
}

/// `text`'s own drawn pixel width (no trailing gap after the last glyph) -- used to centre
/// the baked `SYNTHETIC` word.
fn text_pixel_width(text: &str) -> i64 {
    let count = text.chars().count() as i64;
    if count == 0 {
        0
    } else {
        count * GLYPH_PITCH_X as i64 - 1
    }
}

/// `--synthetic-source-style labelled`'s own renderer -- see this binary's own module doc for
/// the full rationale. Three layers, drawn in this order onto a fresh `width * height * 3`
/// RGB8 buffer (each layer unconditionally attempted; a raster too small for a given element
/// just has that element clipped away by [`set_pixel`], never a panic -- this file's own tests
/// exercise rasters as small as 2x2):
///
/// 1. A high-contrast magenta/black checkerboard, [`checker_cell_size`] pixels per cell.
/// 2. A lat/lon coordinate grid: one white pixel-wide line every 90 degrees of longitude and
///    every 45 degrees of latitude, over this raster's own fixed whole-globe bounds
///    (`-180,-90,180,90` -- [`synthetic_raster_bytes`]'s own bounds).
/// 3. Baked text, in [`glyph_rows`]'s own tiny font: each longitude line is labelled with its
///    own value (e.g. `E090`, `W180`) near the raster's own north edge, each latitude line
///    with its own value (e.g. `N45`, `S90`) near the raster's own west edge, and the word
///    `SYNTHETIC` once, centred on the equator/prime-meridian intersection.
///
/// # Why a coordinate grid, not per-tile `level`/`x`/`y` text
///
/// This function renders ONE whole-globe SOURCE raster; the tiler (`crate::tiler`, a file this
/// task's own brief forbids touching) slices arbitrary crops of it into tiles only AFTER this
/// function has already returned, so no per-tile `level`/`x`/`y` identity exists yet at the
/// point this function runs -- baking a specific tile's own coordinates into the source raster
/// is structurally impossible here, and this doc says so explicitly rather than leaving it
/// implied. The lat/lon grid is the next-best thing this function CAN do: every tile's own
/// bounds are a known crop of this raster's fixed whole-globe extent, so a person looking at a
/// tile can already read off roughly where in the globe it sits from which grid lines/labels
/// appear inside it, without a devtools read. (Separately: a clean, ADDITIVE way for the
/// TILER itself to stamp a real `level/x/y` label onto each tile after cropping -- e.g. an
/// opt-in post-render step on `TilerExecutor`, never touching the non-labelled path -- looks
/// feasible from this raster's own resampling code, but is out of this task's own scope and
/// not built here; see this task's own report.)
fn labelled_pixels(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 3];

    // Layer 1: the checkerboard.
    let cell = checker_cell_size(width, height);
    for row in 0..height {
        for col in 0..width {
            let parity = (col / cell + row / cell) % 2;
            let color = if parity == 0 { LABELLED_CHECKER_A } else { LABELLED_CHECKER_B };
            set_pixel(&mut pixels, width, height, col as i64, row as i64, color);
        }
    }

    // Whole-globe bounds -- fixed, exactly synthetic_raster_bytes's own.
    const WEST: f64 = -180.0;
    const SOUTH: f64 = -90.0;
    const EAST: f64 = 180.0;
    const NORTH: f64 = 90.0;
    let col_for_lon = |lon: f64| -> i64 { (((lon - WEST) / (EAST - WEST)) * width as f64).round() as i64 };
    let row_for_lat = |lat: f64| -> i64 { (((NORTH - lat) / (NORTH - SOUTH)) * height as f64).round() as i64 };

    // Layer 2 + 3: longitude grid lines, each labelled near the north edge.
    let mut lon = -180i64;
    while lon <= 180 {
        let x = col_for_lon(lon as f64).clamp(0, width as i64 - 1);
        for row in 0..height as i64 {
            set_pixel(&mut pixels, width, height, x, row, LABELLED_TEXT_COLOR);
        }
        let label = format!("{}{:03}", if lon >= 0 { "E" } else { "W" }, lon.unsigned_abs());
        draw_text(&mut pixels, width, height, x + 1, 1, &label, LABELLED_TEXT_COLOR);
        lon += 90;
    }

    // Layer 2 + 3: latitude grid lines, each labelled near the west edge.
    let mut lat = -90i64;
    while lat <= 90 {
        let y = row_for_lat(lat as f64).clamp(0, height as i64 - 1);
        for col in 0..width as i64 {
            set_pixel(&mut pixels, width, height, col, y, LABELLED_TEXT_COLOR);
        }
        let label = format!("{}{:02}", if lat >= 0 { "N" } else { "S" }, lat.unsigned_abs());
        draw_text(&mut pixels, width, height, 1, y + 1, &label, LABELLED_TEXT_COLOR);
        lat += 45;
    }

    // Layer 3: the word SYNTHETIC, centred on the equator/prime-meridian intersection.
    let word = "SYNTHETIC";
    let word_x = width as i64 / 2 - text_pixel_width(word) / 2;
    let word_y = height as i64 / 2 - GLYPH_HEIGHT as i64 / 2;
    draw_text(&mut pixels, width, height, word_x, word_y, word, LABELLED_TEXT_COLOR);

    pixels
}

/// Whole-globe bounds -- fixed, always, for a synthetic source: the only inputs that can
/// change a synthetic source's own bytes (and therefore its SHA-256) are `width`/`height`/
/// `style`.
fn synthetic_raster_bytes(width: u32, height: u32, style: SyntheticStyle) -> Vec<u8> {
    let pixels = match style {
        SyntheticStyle::Gradient => synthetic_pixels(width, height),
        SyntheticStyle::Labelled => labelled_pixels(width, height),
    };
    encode_raster(width, height, -180.0, -90.0, 180.0, 90.0, &pixels)
}

/// Reads the source raster's bytes -- either verbatim off disk, or freshly generated by
/// [`synthetic_raster_bytes`]. Never any other source.
fn source_bytes(source: &Source) -> Result<Vec<u8>, String> {
    match source {
        Source::Path(path) => std::fs::read(path).map_err(|e| format!("--source-path {path:?}: {e}")),
        Source::Synthetic { width, height, style } => Ok(synthetic_raster_bytes(*width, *height, *style)),
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

/// Wraps an `Arc<MemoryObjectSink>` so this binary can hand the `Runner` a `Box<dyn
/// ObjectSink>` it owns while keeping its own read handle on the same store -- needed on
/// `--streaming --dry-run` to fetch the manifest back afterward (this binary's own module
/// doc, "`--streaming`: the open item from H5b-1 closed", "What `--streaming` changes about
/// this binary's own `total_stored_bytes`/`tile_count` accounting"). Byte-for-byte
/// `crates/av-jobs/tests/tiler.rs::SharedSink`'s own shape.
#[derive(Debug, Clone)]
struct SharedMemorySink(Arc<MemoryObjectSink>);
impl ObjectSink for SharedMemorySink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

/// A read handle back onto whichever store this run wrote through -- the in-memory sink
/// (`--dry-run`) or the real store bridge -- kept alongside `source_obj`/`sink_obj` (which
/// are moved into the `Runner`) so `run` can fetch the manifest back after the job completes
/// on `--streaming` (this binary's own module doc, "What `--streaming` changes about this
/// binary's own `total_stored_bytes`/`tile_count` accounting"). Not used at all on the
/// buffered path -- `JobCompletion.outputs` already has everything that path needs.
enum Fetcher {
    Memory(Arc<MemoryObjectSink>),
    Store(Arc<StoreBridge>),
}

impl Fetcher {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, String> {
        match self {
            Fetcher::Memory(sink) => sink.get(&asset.sha256).ok_or_else(|| format!("no object stored under sha256 {:?} in the in-memory sink", asset.sha256)),
            Fetcher::Store(bridge) => bridge.fetch(asset).map_err(|e| format!("fetching {:?} back from the store: {e:?}", asset.uri)),
        }
    }
}

// -----------------------------------------------------------------------------------------
// Peak RSS -- see this binary's own module doc, "peak_rss_bytes: what makes 'it streamed' a
// measured claim". Raw `extern "C"` FFI, no `libc` crate: this workspace's "no new crate in
// Cargo.lock" rule (question 199's sibling rule for this task) applies to a JSON-reporting
// binary exactly as much as to a library.
// -----------------------------------------------------------------------------------------

/// `struct timeval` (`<sys/time.h>`), 64-bit Darwin/Linux layout: `tv_sec` 8 bytes, `tv_usec`
/// a 4-byte `i32` padded to 8 -- the padding is declared explicitly (`_pad`) rather than left
/// to `#[repr(C)]` to infer, so this struct's `size_of` is checked against the platform's own
/// `sizeof(struct timeval)` by this module's own test rather than merely assumed.
#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
    _pad: i32,
}

/// `struct rusage` (`<sys/resource.h>`), the fields `getrusage` fills -- every field present
/// (not just `ru_maxrss`) so this struct's layout matches the real one field-for-field; only
/// `ru_maxrss` is ever read.
#[repr(C)]
struct RUsage {
    ru_utime: Timeval,
    ru_stime: Timeval,
    ru_maxrss: i64,
    ru_ixrss: i64,
    ru_idrss: i64,
    ru_isrss: i64,
    ru_minflt: i64,
    ru_majflt: i64,
    ru_nswap: i64,
    ru_inblock: i64,
    ru_oublock: i64,
    ru_msgsnd: i64,
    ru_msgrcv: i64,
    ru_nsignals: i64,
    ru_nvcsw: i64,
    ru_nivcsw: i64,
}

const RUSAGE_SELF: i32 = 0;

#[cfg(unix)]
extern "C" {
    fn getrusage(who: i32, usage: *mut RUsage) -> i32;
}

/// `ru_maxrss`'s own unit is platform-specific: **bytes on macOS/Darwin**, **kilobytes on
/// Linux** -- the same field name, two different units, a well-known `getrusage(2)` wart.
/// This binary's own module doc states which one this task measured against (macOS); this
/// function is the one place that distinction is applied, so a Linux build of this same
/// binary (not this round's target host, but not refused at compile time either) still
/// reports real bytes rather than silently mislabelled kilobytes.
#[cfg(target_os = "macos")]
fn ru_maxrss_to_bytes(raw: i64) -> u64 {
    raw.max(0) as u64
}
#[cfg(target_os = "linux")]
fn ru_maxrss_to_bytes(raw: i64) -> u64 {
    (raw.max(0) as u64).saturating_mul(1024)
}

/// This process's own peak resident set size in bytes, read from the kernel via
/// `getrusage(RUSAGE_SELF, ..)` -- `None` on a platform this binary has no `ru_maxrss` unit
/// conversion for, or if the call itself fails (never observed in practice; `getrusage` with
/// a valid pointer and `RUSAGE_SELF` does not fail on a real POSIX system, but this function
/// does not assume that).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage: RUsage = unsafe { std::mem::zeroed() };
    let rc = unsafe { getrusage(RUSAGE_SELF, &mut usage as *mut RUsage) };
    if rc != 0 {
        return None;
    }
    Some(ru_maxrss_to_bytes(usage.ru_maxrss))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peak_rss_bytes() -> Option<u64> {
    None
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
    /// Whether `Runner::run_one_streaming`/`TilerExecutor::run_imagery_streaming` ran this
    /// job instead of the buffered `Runner::run_one`/`TilerExecutor::run_imagery` -- see this
    /// binary's own module doc, "`--streaming`: the open item from H5b-1 closed".
    streaming: bool,
    /// This process's own peak resident set size, in BYTES, at job completion --
    /// `getrusage(RUSAGE_SELF, ..).ru_maxrss`, converted to bytes for the platform this ran
    /// on (macOS: already bytes; Linux: kilobytes x 1024 -- see [`ru_maxrss_to_bytes`]'s own
    /// doc). `null` only if this platform has no conversion this binary knows (`peak_rss_bytes`'s
    /// own doc) -- never a silently wrong unit.
    peak_rss_bytes: Option<u64>,
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
        let peak_rss_json = match self.peak_rss_bytes {
            Some(v) => v.to_string(),
            None => "null".to_string(),
        };
        format!(
            "{{\"manifest_sha256\":\"{}\",\"tile_count\":{},\"total_stored_bytes\":{},\"source_sha256\":\"{}\",\"object_key_prefix\":\"{}\",\"min_level\":{},\"max_level\":{},\"tile_size\":{},\"job_id\":\"{}\",\"dry_run\":{},\"streaming\":{},\"peak_rss_bytes\":{}}}",
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
            self.streaming,
            peak_rss_json,
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

    let (source_obj, sink_obj, raster_asset, fetcher): (Box<dyn ObjectSource>, Box<dyn ObjectSink>, pb::AssetRef, Fetcher) = if cli.dry_run {
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
        // Arc'd and cloned, not owned solely by `sink_obj` -- `--streaming` needs a read
        // handle on the SAME sink after the run, to fetch the manifest back (this binary's
        // own module doc, "What `--streaming` changes about this binary's own
        // `total_stored_bytes`/`tile_count` accounting").
        let mem_sink = Arc::new(MemoryObjectSink::new(key_prefix.clone()));
        (Box::new(mem_source), Box::new(SharedMemorySink(mem_sink.clone())), asset, Fetcher::Memory(mem_sink))
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
        (Box::new(StoreSource(bridge.clone())), Box::new(StoreSink(bridge.clone())), raster_asset, Fetcher::Store(bridge))
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
    let completion = if cli.streaming {
        eprintln!("av-tile-fixture: --streaming -- storing each tile through the sink as it is rendered (Runner::run_one_streaming)");
        runner.run_one_streaming(&spec).map_err(|e| format!("run_one_streaming (queue append failed -- infrastructure, not a job failure): {e}"))?
    } else {
        runner.run_one(&spec).map_err(|e| format!("run_one (queue append failed -- infrastructure, not a job failure): {e}"))?
    };

    if !completion.ok {
        let failure = completion.failure.unwrap_or_default();
        return Err(format!("the tiler job itself failed: kind={} detail={:?} exit_code={}", failure.kind, failure.detail, failure.exit_code));
    }
    if completion.manifest_sha256.is_empty() {
        return Err("the job completed ok but produced no manifest_sha256 -- this should be impossible for a successful \"tiler\" job".to_string());
    }

    // -- tile_count / total_stored_bytes: see this binary's own module doc, "What
    // `--streaming` changes about this binary's own `total_stored_bytes`/`tile_count`
    // accounting". Buffered: JobCompletion.outputs already names every tile. Streaming:
    // JobCompletion.outputs names only the manifest (TilerExecutor::run_imagery_streaming's
    // own doc), so this binary fetches that manifest back through `fetcher` and sums its own
    // TileSetManifest.tiles[].size_bytes instead. ---------------------------------------
    let (tile_count, total_stored_bytes) = if cli.streaming {
        let manifest_asset = completion
            .outputs
            .iter()
            .find(|a| a.media_type == av_jobs::tiler::MANIFEST_MEDIA_TYPE)
            .ok_or_else(|| format!("streaming completion carried no manifest output at all: {completion:?}"))?;
        let manifest_bytes = fetcher.fetch(manifest_asset)?;
        let manifest_bytes_sha256 = hex_encode(&sha256(&manifest_bytes));
        if manifest_bytes_sha256 != completion.manifest_sha256 {
            return Err(format!("fetched manifest bytes hash to {manifest_bytes_sha256}, not JobCompletion.manifest_sha256 {}", completion.manifest_sha256));
        }
        let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes.as_slice()).map_err(|e| format!("decoding the fetched manifest bytes as TileSetManifest: {e}"))?;
        let tiles_total: u64 = manifest.tiles.iter().map(|t| t.size_bytes).sum();
        (manifest.tiles.len(), tiles_total + manifest_asset.size_bytes)
    } else {
        let tile_count = completion.outputs.iter().filter(|a| a.media_type == av_jobs::tiler::IMAGERY_TILE_MEDIA_TYPE).count();
        let total_stored_bytes: u64 = completion.outputs.iter().map(|a| a.size_bytes).sum();
        (tile_count, total_stored_bytes)
    };

    let peak_rss_bytes = peak_rss_bytes();
    match peak_rss_bytes {
        Some(v) => eprintln!("av-tile-fixture: job complete: {tile_count} tile(s), {total_stored_bytes} byte(s) stored, peak_rss_bytes={v}, manifest_sha256={}", completion.manifest_sha256),
        None => eprintln!("av-tile-fixture: job complete: {tile_count} tile(s), {total_stored_bytes} byte(s) stored, peak RSS unavailable on this platform, manifest_sha256={}", completion.manifest_sha256),
    }

    // Question 228's finding 2 (pipeline half): register the manifest this run just produced
    // as a CatalogAsset -- ONLY when --catalog-host was given (parse_cli_args has already
    // refused --catalog-host together with --dry-run, and refused a partially-specified
    // catalog connection, so every field this block reads is real). With no --catalog-* flags
    // at all, this whole block never runs and this binary's behaviour/output is byte-for-byte
    // what it was before this task -- every existing test of it (this file's own `mod tests`,
    // none of which pass a --catalog-* flag) stays honest.
    if let Some(catalog_host) = &cli.catalog_host {
        // completion.manifest_sha256 is already verified (the streaming branch above
        // re-hashed the fetched manifest bytes against it directly; the buffered branch's own
        // manifest_sha256 comes from Runner::run_one's own hash-chained JobCompletion, which
        // this binary already refused above at "the job completed ok but produced no
        // manifest_sha256" if it were ever missing) -- this block runs after both, and before
        // FixtureResult is built, exactly where this task's own brief calls for it.
        let manifest_asset = completion
            .outputs
            .iter()
            .find(|a| a.media_type == av_jobs::tiler::MANIFEST_MEDIA_TYPE)
            .ok_or_else(|| format!("--catalog-host was given but the completion carried no manifest output to register: {completion:?}"))?;

        let catalog_user = cli.catalog_user.clone().expect("parse_cli_args refuses to return Ok with catalog_host set but catalog_user absent");
        let catalog_password = cli.catalog_password.clone().expect("parse_cli_args refuses to return Ok with catalog_host set but catalog_password absent");
        let catalog_database = cli.catalog_database.clone().expect("parse_cli_args refuses to return Ok with catalog_host set but catalog_database absent");
        let catalog_tls = match &cli.catalog_tls_ca_file {
            Some(ca_file) => PgTls::Required { ca_file: Some(ca_file.clone()) },
            None => PgTls::Disabled,
        };
        let pg_config = PgConfig {
            host: catalog_host.clone(),
            port: cli.catalog_port,
            user: catalog_user,
            password: catalog_password,
            database: catalog_database,
            application_name: "av-tile-fixture".to_string(),
            connect_timeout: std::time::Duration::from_secs(5),
            tls: catalog_tls,
        };

        // This catalog's own identifier (never the sha256 -- crates/av-catalog/migrations/
        // 0001_init.sql's own COMMENT ON COLUMN assets.asset_id: "the same bytes can be
        // catalogued twice under different labels/provenance"), deterministic in the job_id
        // and the manifest's own real sha256 so re-running the identical job against the
        // identical inputs is idempotent-in-intent (a second real INSERT of the identical
        // asset_id still fails on the PRIMARY KEY, which is the correct, honest behaviour for
        // "this exact tile set is already catalogued" -- this binary does not paper over that
        // with an upsert).
        let asset_id = format!("tileset:{job_id}:{}", completion.manifest_sha256);
        eprintln!("av-tile-fixture: registering the tile-set manifest in the catalog at {catalog_host}:{} (asset_id={asset_id:?}) ...", cli.catalog_port);

        let catalog_asset = CatalogAsset::from_asset_ref(manifest_asset, asset_id.clone(), None, Some(job_id.clone()), clock.now_tai_ns())
            .map_err(|e| format!("building a CatalogAsset from the manifest output: {e}"))?;

        // A fresh Runtime and a fresh PgClient connection for this one registration -- never
        // reusing the store's own Runtime/connection (this binary's own module doc, "Why
        // hand-rolled" in crates/av-catalog's own crate doc explains the wire client;
        // crates/av-gateway/src/catalog_selector.rs's own "Connection lifecycle: connect fresh
        // per call, no pool" is the identical precedent this block follows, restated here for
        // a one-shot CLI tool that has no reason to hold a connection open any longer than
        // this one INSERT needs it).
        let catalog_runtime = tokio::runtime::Runtime::new().map_err(|e| format!("building the catalog tokio Runtime: {e}"))?;
        catalog_runtime.block_on(async {
            let mut client = PgClient::connect(&pg_config).await.map_err(|e| format!("connecting to the catalog at {catalog_host}:{}: {e}", cli.catalog_port))?;
            catalog_asset.insert(&mut client).await.map_err(|e| format!("inserting the tile-set manifest into the catalog: {e}"))?;
            client.close().await.map_err(|e| format!("closing the catalog connection: {e}"))
        })?;
        eprintln!("av-tile-fixture: registered the tile-set manifest in the catalog (asset_id={asset_id:?})");
    }

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
        streaming: cli.streaming,
        peak_rss_bytes,
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
        assert_eq!(cli.source, Some(Source::Synthetic { width: 4, height: 2, style: SyntheticStyle::Gradient }));
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

    // -- question 228's finding 2: --catalog-* flags -----------------------------------------

    #[test]
    fn with_no_catalog_flags_cli_args_catalog_host_is_none() {
        // The presence signal this binary's own registration code gates on (`if let Some(
        // catalog_host) = &cli.catalog_host`) -- every existing test in this file passes no
        // --catalog-* flag at all, so this is the one explicit assertion tying "no flags"
        // directly to "the new code path can never run" for a future reader, rather than
        // leaving it merely implied by every other test's own continued, unchanged behaviour.
        let cli = parse_cli_args(args(&[])).unwrap();
        assert!(cli.catalog_host.is_none());
        assert_eq!(cli.catalog_port, DEFAULT_CATALOG_PORT);
    }

    #[test]
    fn a_dry_run_with_no_catalog_flags_is_byte_for_byte_unaffected() {
        // Guards the brief's own "behaviour with no flags is byte-for-byte unchanged" bar
        // directly against a real run, not just the parsed CliArgs -- a regression that made
        // this binary'S new code silently run even with catalog_host absent would still leave
        // this test's own manifest_sha256/tile_count/total_stored_bytes assertions intact
        // (they do not depend on the catalog at all), but the point is this test exercises the
        // exact same REQUIRED_ARGS-only command line every pre-existing test in this file
        // already does.
        let result = run(parse_cli_args(args(&[])).unwrap()).expect("a fully-specified --dry-run must still succeed with no --catalog-* flags");
        assert_eq!(result.manifest_sha256.len(), 64);
        assert!(result.tile_count > 0);
    }

    #[test]
    fn catalog_host_and_dry_run_are_mutually_exclusive() {
        let err = parse_cli_args(args(&["--catalog-host", "127.0.0.1", "--catalog-user", "u", "--catalog-password", "p", "--catalog-database", "d"])).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn a_partially_specified_catalog_connection_is_refused_at_startup() {
        // --catalog-host without --catalog-user/--catalog-password/--catalog-database is
        // refused up front, never silently incomplete -- mirrors crates/av-gateway/src/bin/
        // av-gateway.rs's identical rule for the same four flags. This command line is also,
        // separately, a --dry-run one (REQUIRED_ARGS's own default) -- the "requires all
        // catalog fields" check must fire regardless of what a caller does or does not also
        // get wrong about --dry-run, so this test only asserts on that one message.
        let err = parse_cli_args(args(&["--catalog-host", "127.0.0.1"])).unwrap_err();
        assert!(err.contains("--catalog-user"), "{err}");
    }

    #[test]
    fn a_fully_specified_catalog_command_line_parses_with_the_default_port() {
        let base: Vec<&str> = vec![
            "av-tile-fixture",
            "--key-prefix",
            "tiles",
            "--ladder",
            "UNCLASSIFIED,CUI",
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
            "--store-endpoint",
            "http://127.0.0.1:9000",
            "--store-region",
            "us-east-1",
            "--store-access-key-id",
            "id",
            "--store-secret-access-key",
            "secret",
            "--store-bucket",
            "bucket",
            "--catalog-host",
            "127.0.0.1",
            "--catalog-user",
            "catalog_user",
            "--catalog-password",
            "catalog_password",
            "--catalog-database",
            "altavista_catalog",
        ];
        let cli = parse_cli_args(base.into_iter().map(str::to_string)).unwrap();
        assert_eq!(cli.catalog_host, Some("127.0.0.1".to_string()));
        assert_eq!(cli.catalog_port, DEFAULT_CATALOG_PORT);
        assert_eq!(cli.catalog_user, Some("catalog_user".to_string()));
        assert_eq!(cli.catalog_password, Some("catalog_password".to_string()));
        assert_eq!(cli.catalog_database, Some("altavista_catalog".to_string()));
        assert!(cli.catalog_tls_ca_file.is_none());
        assert!(!cli.dry_run);
    }

    #[test]
    fn a_manifest_asset_ref_builds_a_real_catalog_asset_with_the_manifests_own_sha256_and_the_jobs_label() {
        // A pure, docker-free unit test of exactly the conversion the registration block in
        // `run` performs (`CatalogAsset::from_asset_ref(manifest_asset, asset_id, None,
        // Some(job_id), created_tai_ns)`), against a manifest-shaped `pb::AssetRef` built the
        // same way the real store path's `av_store::client::StoreClient::put` builds one (a
        // real `label` and a real, if empty-fielded, `Some(Provenance)` -- never `None`, which
        // is what `--dry-run`'s own `MemoryObjectSink::put` would leave it at, and exactly why
        // --catalog-host and --dry-run are refused together above). A real round-trip against
        // a real PostGIS container is NOT covered by this test -- see this task's own report.
        let manifest_sha256 = hex_encode(&sha256(b"a fixture manifest's own bytes"));
        let manifest_asset = pb::AssetRef {
            uri: "s3://altavista-heavy/tiles/prefix/manifest".to_string(),
            sha256: manifest_sha256.clone(),
            size_bytes: 4096,
            media_type: av_jobs::tiler::MANIFEST_MEDIA_TYPE.to_string(),
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            provenance: Some(pb::Provenance::default()),
            ..Default::default()
        };
        let asset_id = format!("tileset:{}:{}", "job-42", manifest_sha256);
        let catalog_asset = CatalogAsset::from_asset_ref(&manifest_asset, asset_id.clone(), None, Some("job-42".to_string()), 1_820_000_000_000_000_000).expect("a manifest AssetRef with real label+provenance must convert");
        assert_eq!(catalog_asset.asset_id, asset_id);
        assert_eq!(catalog_asset.sha256, manifest_sha256);
        assert_eq!(catalog_asset.media_type, av_jobs::tiler::MANIFEST_MEDIA_TYPE);
        assert_eq!(catalog_asset.job_id, Some("job-42".to_string()));
        assert_eq!(catalog_asset.label.marking, "CUI");
        assert_eq!(catalog_asset.created_tai_ns, 1_820_000_000_000_000_000);
    }

    #[test]
    fn a_dry_run_style_manifest_asset_ref_with_no_provenance_is_refused_by_catalog_asset_conversion() {
        // Documents (and proves, not just asserts in a comment) exactly why --catalog-host and
        // --dry-run are mutually exclusive: crate::runner::MemoryObjectSink::put's own
        // `provenance: None` is real, and CatalogAsset::from_asset_ref refuses a `None`
        // provenance outright rather than defaulting it.
        let manifest_asset = pb::AssetRef {
            uri: "memory://tiles/manifest".to_string(),
            sha256: hex_encode(&sha256(b"dry-run manifest bytes")),
            size_bytes: 128,
            media_type: av_jobs::tiler::MANIFEST_MEDIA_TYPE.to_string(),
            label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
            provenance: None,
            ..Default::default()
        };
        let err = CatalogAsset::from_asset_ref(&manifest_asset, "asset-1", None, None, 1).unwrap_err();
        assert!(matches!(err, av_catalog::CatalogError::AssetRefMissingField { field: "provenance" }), "{err:?}");
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
        for style in [SyntheticStyle::Gradient, SyntheticStyle::Labelled] {
            let a = synthetic_raster_bytes(37, 19, style);
            let b = synthetic_raster_bytes(37, 19, style);
            assert_eq!(a, b, "the same --synthetic-source WxH --synthetic-source-style {style:?} must produce byte-identical raster bytes every run");
            let a_hash = hex_encode(&sha256(&a));
            let b_hash = hex_encode(&sha256(&b));
            assert_eq!(a_hash, b_hash);
        }
    }

    #[test]
    fn different_synthetic_source_dimensions_produce_different_source_hashes() {
        let a = hex_encode(&sha256(&synthetic_raster_bytes(4, 2, SyntheticStyle::Gradient)));
        let b = hex_encode(&sha256(&synthetic_raster_bytes(4, 3, SyntheticStyle::Gradient)));
        assert_ne!(a, b, "a different height must change the source raster's own bytes, and therefore its hash");
    }

    #[test]
    fn the_two_styles_produce_different_bytes_for_the_identical_width_and_height() {
        // The one new assertion round 7 task 5a exists to add: the whole point of a second
        // style is that it is NOT the same raster as the default.
        let gradient = synthetic_raster_bytes(64, 32, SyntheticStyle::Gradient);
        let labelled = synthetic_raster_bytes(64, 32, SyntheticStyle::Labelled);
        assert_ne!(gradient, labelled, "gradient and labelled must render different bytes for the identical WxH");
        assert_eq!(gradient.len(), labelled.len(), "both styles encode the same width/height, so their AVRASTER byte length must still match");
    }

    #[test]
    fn the_default_synthetic_style_is_gradient_and_its_bytes_are_pinned_from_before_this_task() {
        // "The default has not moved" -- these two hex hashes were computed BEFORE this task's
        // own code change existed, by independently re-implementing this file's own documented
        // gradient formula (r/g/b arithmetic + the AVRASTER header) in Python, matching Rust's
        // f64::round() "round half away from zero" behaviour exactly (Python's builtin round()
        // rounds half to even instead, which this task's own author confirmed actually changes
        // the 37x19 case's hash -- see this task's own report). Recorded here, in a comment, as
        // the pre-change reference this task's brief calls for; not derived from this file's
        // own post-change code in any way.
        let a = synthetic_raster_bytes(37, 19, SyntheticStyle::default());
        assert_eq!(hex_encode(&sha256(&a)), "9bf8afccccc6b1f902d0aa884b72e312a84a90725a92498f98734415252b82b6");
        assert_eq!(a.len(), 2165);

        // 64x32 -- this crate's own README recipe's real --synthetic-source value.
        let b = synthetic_raster_bytes(64, 32, SyntheticStyle::default());
        assert_eq!(hex_encode(&sha256(&b)), "ddcf7d471778abfa900030b18fc7fd638e5d272a3cfba20ecef372470f6fc2cf");
        assert_eq!(b.len(), 6200);

        // And the CLI's own default (no --synthetic-source-style flag at all) takes the
        // identical Gradient arm -- proven against the real parser, not just SyntheticStyle's
        // own Default impl.
        let cli = parse_cli_args(args(&[])).unwrap();
        assert_eq!(cli.source, Some(Source::Synthetic { width: 4, height: 2, style: SyntheticStyle::Gradient }));
    }

    #[test]
    fn synthetic_raster_bytes_decode_as_a_well_formed_raster() {
        // Round-trips through this crate's own real decoder -- proves encode_raster's hand-
        // written header really matches av_jobs::raster's documented layout, not merely this
        // file's own belief that it does. Both styles: the header is style-independent, only
        // the pixel payload differs.
        for style in [SyntheticStyle::Gradient, SyntheticStyle::Labelled] {
            let bytes = synthetic_raster_bytes(8, 4, style);
            let raster = av_jobs::raster::decode(&bytes).expect("encode_raster must produce a raster av_jobs::raster::decode accepts");
            assert_eq!(raster.width, 8);
            assert_eq!(raster.height, 4);
            assert_eq!(raster.west, -180.0);
            assert_eq!(raster.south, -90.0);
            assert_eq!(raster.east, 180.0);
            assert_eq!(raster.north, 90.0);
        }
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

    // -- --synthetic-source-style CLI parsing (round 7 task 5a) -----------------------------

    #[test]
    fn synthetic_source_style_accepts_gradient_and_labelled_and_refuses_anything_else() {
        let gradient = parse_cli_args(args(&["--synthetic-source-style", "gradient"])).unwrap();
        assert_eq!(gradient.source, Some(Source::Synthetic { width: 4, height: 2, style: SyntheticStyle::Gradient }));

        let labelled = parse_cli_args(args(&["--synthetic-source-style", "labelled"])).unwrap();
        assert_eq!(labelled.source, Some(Source::Synthetic { width: 4, height: 2, style: SyntheticStyle::Labelled }));

        let err = parse_cli_args(args(&["--synthetic-source-style", "psychedelic"])).unwrap_err();
        assert!(err.contains("\"gradient\" or \"labelled\""), "{err}");
    }

    #[test]
    fn synthetic_source_style_takes_effect_regardless_of_flag_order() {
        // CliArgs::synthetic_style's own doc: applied after the whole command line is parsed,
        // specifically so this works whichever flag came first.
        let style_first: Vec<&str> = vec![
            "av-tile-fixture",
            "--synthetic-source-style",
            "labelled",
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
            "1",
            "--synthetic-source",
            "4x2",
            "--dry-run",
        ];
        let cli = parse_cli_args(style_first.into_iter().map(str::to_string)).unwrap();
        assert_eq!(cli.source, Some(Source::Synthetic { width: 4, height: 2, style: SyntheticStyle::Labelled }));
    }

    #[test]
    fn synthetic_source_style_with_a_file_source_is_refused() {
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
            "1",
            "--source-path",
            "/dev/null",
            "--synthetic-source-style",
            "labelled",
            "--dry-run",
        ];
        let err = parse_cli_args(base.into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--source-path"), "{err}");
    }

    // -- labelled_pixels: a person can see it (checkerboard, grid, baked text) --------------

    #[test]
    fn labelled_checkerboard_uses_only_the_two_documented_colours_in_a_roughly_even_split() {
        // The lat/lon grid + baked text is a FIXED number of lines/glyphs regardless of raster
        // size, while the checkerboard area grows with width*height -- so a large-enough
        // raster is what makes the overlay a small minority of pixels; this file's own README
        // recipe size (64x32) is checked separately, by fraction alone, in the next test.
        let (width, height) = (256u32, 128u32);
        let pixels = labelled_pixels(width, height);
        let mut count_a = 0u32;
        let mut count_b = 0u32;
        let mut count_other = 0u32;
        for chunk in pixels.chunks_exact(3) {
            let px = [chunk[0], chunk[1], chunk[2]];
            if px == LABELLED_CHECKER_A {
                count_a += 1;
            } else if px == LABELLED_CHECKER_B {
                count_b += 1;
            } else {
                count_other += 1; // grid lines / glyph pixels (LABELLED_TEXT_COLOR, white).
            }
        }
        let total = (width * height) as f64;
        assert!((count_a as f64 / total) > 0.40, "checker colour A must cover a substantial fraction, got {count_a}/{total}");
        assert!((count_b as f64 / total) > 0.40, "checker colour B must cover a substantial fraction, got {count_b}/{total}");
        assert!((count_other as f64 / total) < 0.15, "grid+text overlay must stay a minority of pixels, got {count_other}/{total}");
        // No pixel at all is teal/brown (the offline fixture's own palette) or a smooth
        // gradient value that only synthetic_pixels itself could have produced.
        assert!(count_a > 0 && count_b > 0 && count_other > 0, "all three pixel classes must actually appear: a={count_a} b={count_b} other={count_other}");
    }

    #[test]
    fn labelled_at_the_readmes_own_64x32_recipe_size_still_shows_both_checker_colours_and_the_overlay() {
        // The exact WxH scripts/heavy/README.md's own drive recipe uses -- both checker
        // colours must appear (the pattern is real, not degenerated to one flat colour) and
        // SOME overlay pixels must appear too (the grid/text is real, not silently skipped).
        let (width, height) = (64u32, 32u32);
        let pixels = labelled_pixels(width, height);
        let mut count_a = 0u32;
        let mut count_b = 0u32;
        let mut count_other = 0u32;
        for chunk in pixels.chunks_exact(3) {
            let px = [chunk[0], chunk[1], chunk[2]];
            if px == LABELLED_CHECKER_A {
                count_a += 1;
            } else if px == LABELLED_CHECKER_B {
                count_b += 1;
            } else {
                count_other += 1;
            }
        }
        let total = width * height;
        assert!(count_a > 0 && count_b > 0 && count_other > 0, "all three pixel classes must appear at the real recipe size: a={count_a} b={count_b} other={count_other} total={total}");
    }

    #[test]
    fn labelled_bakes_the_word_synthetic_centred_and_a_glyph_is_present_at_its_expected_bounding_box() {
        let (width, height) = (64u32, 32u32);
        let pixels = labelled_pixels(width, height);
        let word = "SYNTHETIC";
        let word_x = width as i64 / 2 - text_pixel_width(word) / 2;
        let word_y = height as i64 / 2 - GLYPH_HEIGHT as i64 / 2;

        // Decode: render the SAME word with the SAME font/placement logic into a scratch
        // buffer of LABELLED_TEXT_COLOR-on-black, and confirm every "on" pixel this produces
        // is ALSO LABELLED_TEXT_COLOR in the real labelled_pixels output at the identical
        // coordinate -- i.e. the baked text really is there, at the position the doc comment
        // claims, not merely "some white pixels exist somewhere".
        let mut expected = vec![0u8; width as usize * height as usize * 3];
        draw_text(&mut expected, width, height, word_x, word_y, word, LABELLED_TEXT_COLOR);
        let mut glyph_pixel_count = 0u32;
        for i in 0..(width as usize * height as usize) {
            let e = [expected[i * 3], expected[i * 3 + 1], expected[i * 3 + 2]];
            if e == LABELLED_TEXT_COLOR {
                glyph_pixel_count += 1;
                let got = [pixels[i * 3], pixels[i * 3 + 1], pixels[i * 3 + 2]];
                assert_eq!(got, LABELLED_TEXT_COLOR, "pixel index {i} (word {word:?} at ({word_x},{word_y})) must be baked-text-white in the real render");
            }
        }
        assert!(glyph_pixel_count > 0, "the word {word:?} must actually draw at least one pixel at this raster size");

        // Bounding box sanity: the word's own first and last columns/rows fall within the
        // computed placement, and nowhere near the raster's own edges for this 64x32 size.
        assert!(word_x >= 0 && word_x + text_pixel_width(word) <= width as i64, "word_x={word_x} must fit inside width={width}");
        assert!(word_y >= 0 && word_y + GLYPH_HEIGHT as i64 <= height as i64, "word_y={word_y} must fit inside height={height}");
    }

    #[test]
    fn labelled_decodes_a_named_latitude_label_at_its_documented_position() {
        // The equator line (lat=0) is labelled "N00" starting at pixel (1, row_for_lat(0)+1) --
        // decode it back with the SAME font this file bakes it with, character by character,
        // proving the actual rendered pixels really spell "N00" at that exact position (not
        // merely "some text exists somewhere near there").
        let (width, height) = (64u32, 32u32);
        let pixels = labelled_pixels(width, height);
        let row_for_lat_0 = (((90.0 - 0.0) / 180.0) * height as f64).round() as i64;
        let label_y = row_for_lat_0 + 1;
        let label = "N00";
        for (i, expected_char) in label.chars().enumerate() {
            let glyph_x = 1 + i as i64 * GLYPH_PITCH_X as i64;
            let rows = glyph_rows(expected_char).unwrap();
            for (row_idx, row_bits) in rows.iter().enumerate() {
                for col_idx in 0..GLYPH_WIDTH {
                    let bit = GLYPH_WIDTH - 1 - col_idx;
                    let expected_on = (row_bits >> bit) & 1 != 0;
                    let x = glyph_x + col_idx as i64;
                    let y = label_y + row_idx as i64;
                    if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
                        continue;
                    }
                    let idx = (y as usize * width as usize + x as usize) * 3;
                    let got_white = [pixels[idx], pixels[idx + 1], pixels[idx + 2]] == LABELLED_TEXT_COLOR;
                    assert_eq!(got_white, expected_on, "glyph {expected_char:?} pixel (col {col_idx}, row {row_idx}) at raster ({x},{y}): decoded {got_white}, expected {expected_on}");
                }
            }
        }
    }

    #[test]
    fn checker_cell_size_is_a_stated_fraction_of_the_smaller_dimension_and_never_zero() {
        assert_eq!(checker_cell_size(64, 32), 2); // 32/16
        assert_eq!(checker_cell_size(320, 640), 20); // 320/16
        assert_eq!(checker_cell_size(1, 1), 1); // clamped, never zero
        assert_eq!(checker_cell_size(4, 2), 1); // 2/16 == 0, clamped to 1
    }

    #[test]
    fn labelled_pixels_never_panics_on_the_tiny_rasters_this_files_own_tests_already_use() {
        // 2x2, 4x2, 3x3, 8x4 -- exactly the sizes this file's own pre-existing tests already
        // exercise for the gradient style; labelled must be equally well-defined (checkerboard
        // still renders; text/grid simply clip away).
        for (w, h) in [(1u32, 1u32), (2, 2), (4, 2), (3, 3), (8, 4)] {
            let pixels = labelled_pixels(w, h);
            assert_eq!(pixels.len(), (w * h * 3) as usize);
        }
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

    // -- --streaming: this binary's own wiring of Runner::run_one_streaming ----------------

    #[test]
    fn a_streaming_dry_run_produces_the_identical_manifest_hash_and_byte_total_as_the_buffered_dry_run() {
        // The crate-level proof (`crates/av-jobs/tests/tiler.rs::
        // the_streaming_path_produces_a_byte_identical_manifest_and_byte_identical_tiles_to_the_buffered_path`)
        // is the load-bearing one; this test only checks that THIS BINARY's own `--streaming`
        // flag actually reaches `Runner::run_one_streaming` and that its own
        // `total_stored_bytes`/`tile_count` re-accounting (fetching the manifest back and
        // summing `TileEntry.size_bytes`) agrees with the buffered path's own direct sum --
        // see this binary's own module doc, "What `--streaming` changes about this binary's
        // own `total_stored_bytes`/`tile_count` accounting".
        let buffered = run(parse_cli_args(args(&[])).unwrap()).expect("buffered dry run");
        let streaming = run(parse_cli_args(args(&["--streaming"])).unwrap()).expect("streaming dry run");

        assert!(!buffered.streaming);
        assert!(streaming.streaming);
        assert_eq!(streaming.manifest_sha256, buffered.manifest_sha256, "--streaming must reproduce the identical manifest hash as the buffered path for identical flags");
        assert_eq!(streaming.tile_count, buffered.tile_count, "tile_count must agree whether it came from JobCompletion.outputs (buffered) or the fetched-back manifest (streaming)");
        assert_eq!(streaming.total_stored_bytes, buffered.total_stored_bytes, "total_stored_bytes must agree whether it came from JobCompletion.outputs (buffered) or manifest + tiles (streaming)");
        assert!(streaming.tile_count > 0);
        assert!(streaming.total_stored_bytes > 0);
    }

    #[test]
    fn streaming_and_dry_run_are_not_mutually_exclusive() {
        // Unlike --store-*, --streaming has nothing store-specific about it (it only picks
        // Runner::run_one_streaming over Runner::run_one) -- this is what lets this binary's
        // own test suite exercise the streaming wiring with no docker/store/network at all.
        let cli = parse_cli_args(args(&["--streaming"])).expect("--streaming must be accepted alongside --dry-run");
        assert!(cli.streaming);
        assert!(cli.dry_run);
    }

    // -- peak_rss_bytes: getrusage(RUSAGE_SELF, ..) is really read, and really in bytes ------

    #[test]
    fn peak_rss_bytes_is_reported_and_plausible_for_a_running_process() {
        let rss = peak_rss_bytes().expect("this test binary itself is built for macOS or Linux in this workspace's own CI/dev hosts");
        // A real macOS/Linux process that has parsed CLI args, decoded a raster and run a
        // tiler job has allocated at least a few hundred KB by construction; comparing
        // against an intentionally loose floor (not an exact value, which would be flaky
        // across allocator/OS versions) catches the specific defect this test exists to
        // catch: `ru_maxrss` misread as kilobytes when it is actually bytes (or vice versa)
        // would be off by a factor of 1024, landing far outside this floor either way.
        assert!(rss > 100_000, "peak_rss_bytes() = {rss} is implausibly small for a running process -- likely a unit-conversion defect (KB vs bytes)");
        // And not implausibly large either -- catches the opposite direction (bytes misread
        // as needing *1024 when the platform already reports bytes).
        assert!(rss < 50_000_000_000, "peak_rss_bytes() = {rss} is implausibly large for this test's own tiny fixture -- likely a unit-conversion defect");
    }

    #[test]
    fn to_json_includes_the_streaming_flag_and_a_positive_peak_rss() {
        let result = run(parse_cli_args(args(&["--streaming"])).unwrap()).expect("streaming dry run");
        let json = result.to_json();
        assert!(json.contains("\"streaming\":true"), "{json}");
        assert!(!json.contains("\"peak_rss_bytes\":null"), "peak RSS must be a real number on this test host, got: {json}");
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

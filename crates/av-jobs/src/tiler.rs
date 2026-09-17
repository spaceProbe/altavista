//! H3b (task 3b, `docs/heavy-plan.md` H3 second half): the tiler [`crate::runner::Executor`]
//! for `JobSpec.kind == "tiler"`.
//!
//! # Resampling: nearest-neighbour, exactly specified
//!
//! **Nearest-neighbour is chosen because it is exactly reproducible in integer/`f64`
//! arithmetic with no filter-kernel ambiguity** -- a bilinear or higher-order resampler
//! would need this module to pin down a kernel, an edge-handling rule, and (for anything
//! beyond bilinear) a support radius, any of which a later change could subtly perturb
//! without the change being obviously visible in a diff. Nearest-neighbour has exactly one
//! moving part (`floor`), documented below. A better resampler is a later decision,
//! recorded here rather than silently chosen this round.
//!
//! For output tile-local pixel `(px, py)` (0-indexed, `px` west-to-east, `py` north-to-
//! south, `0 <= px, py < tile_size`), the sampled geodetic point is the pixel's own centre:
//!
//! ```text
//! lon = tile.west + (px + 0.5) / tile_size * (tile.east - tile.west)
//! lat = tile.north - (py + 0.5) / tile_size * (tile.north - tile.south)
//! ```
//!
//! and the sampled raster column/row is:
//!
//! ```text
//! col = floor((lon - raster.west) / (raster.east - raster.west) * raster.width)
//! row = floor((raster.north - lat) / (raster.north - raster.south) * raster.height)
//! ```
//!
//! clamped into `[0, raster.width - 1]` / `[0, raster.height - 1]` (a tile that only
//! partially overlaps the raster can otherwise compute a `col`/`row` fractionally outside
//! the raster for its edge pixels; clamping to the nearest valid raster pixel is this
//! module's documented edge rule, rather than sampling out of bounds or leaving a gap).
//!
//! # A tile entirely outside the raster's bounds is never emitted
//!
//! [`crate::scheme::tiles_covering`] is called with the *raster's own* bounds as the query
//! bounds, so it already returns only tiles that overlap the raster -- a tile with zero
//! overlap is structurally never in that list, so this module needs no separate "is this
//! tile blank" check. This is a deliberate choice over emitting a blank/placeholder tile
//! for out-of-bounds coverage: a tile set for a raster that only covers part of the globe
//! should have gaps where there is no data, not a manifest full of uniform filler tiles a
//! consumer has no way to distinguish from real (if uniformly-coloured) imagery.
//!
//! # The manifest-vs-sink ordering constraint
//!
//! The manifest must name where each tile can be found, but this executor runs *before* the
//! [`crate::runner::ObjectSink`] that stores its outputs -- so it cannot read a location back
//! off an `AssetRef` the sink has not produced yet. It resolves that by computing each tile's
//! `sha256` itself and deriving its key with [`crate::runner::content_addressed_key`], the
//! exact function every `ObjectSink` independently calls for the same bytes, rather than
//! inventing a second key scheme. `crates/av-jobs/tests/store_tiler.rs` asserts that helper
//! and `av_store::keys::object_key` agree for a real hash, so the mirror cannot drift
//! silently. The coupling this leaves is honest and visible: a caller wiring up a `Runner`
//! must construct the `ObjectSink` and this executor with the identical `prefix` string.
//!
//! **What a `TileEntry` deliberately does NOT carry is a URI.** The first implementation
//! filled `TileEntry.uri` with `format!("memory://{key}")` unconditionally, which
//! `tests/store_tiler.rs` measured to be plainly wrong against a real object store -- every
//! tile claimed `memory://tiles/...` while the object lived at `s3://<bucket>/tiles/...`.
//! Filling it *correctly* would have been worse: a fully-qualified URI names a bucket and an
//! endpoint, so the manifest's encoded bytes -- and hence the tile set's identity, which is
//! the SHA-256 of exactly those bytes -- would change with the store that happened to hold
//! the tiles, and a tile set copied between buckets would acquire a second identity. So
//! `uri` is left empty and reserved, and [`pb::TileEntry::object_key`] carries the
//! backend-independent half instead, with `TileSetManifest.object_key_prefix` naming the
//! prefix it was built under. A reader joins that key with whatever store it is reading
//! from. (Manager's review finding, task 3c.)

use std::collections::BTreeMap;

use av_cdm::pb;

use crate::runner::{content_addressed_key, Executor, JobInput, JobOutput};

/// The media type a `TileSetManifest`'s own encoded (protobuf) bytes are stored under.
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.altavista.tileset-manifest+pb";
/// The media type an imagery tile's own PNG bytes are stored under.
pub const IMAGERY_TILE_MEDIA_TYPE: &str = "image/png";

/// `tile_size` must be a power of two in `[MIN_TILE_SIZE, MAX_TILE_SIZE]`.
pub const MIN_TILE_SIZE: u32 = 16;
pub const MAX_TILE_SIZE: u32 = 4096;
/// The documented ceiling on `max_level` -- `crate::scheme::tile_count_x` at this level is
/// `2^17` columns, already far more tiles than any fixture this round's tests build; a job
/// requesting more is refused rather than silently accepted and left to run for an
/// unbounded amount of time.
pub const MAX_LEVEL_CEILING: u32 = 16;
pub const DEFAULT_TILE_SIZE: u32 = 256;

fn invalid_parameters(detail: impl Into<String>) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::InvalidParameters as i32, detail: detail.into(), exit_code: 0 }
}

fn invalid_input(detail: impl Into<String>) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::InvalidInput as i32, detail: detail.into(), exit_code: 0 }
}

fn executor_unavailable(detail: impl Into<String>) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::ExecutorUnavailable as i32, detail: detail.into(), exit_code: 0 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputKind {
    Imagery,
}

#[derive(Debug, Clone)]
struct TilerParams {
    min_level: u32,
    max_level: u32,
    tile_size: u32,
    output: OutputKind,
}

fn parse_u32_param(parameters: &BTreeMap<String, String>, key: &str) -> Result<u32, pb::JobFailure> {
    let raw = parameters.get(key).ok_or_else(|| invalid_parameters(format!("missing required parameter {key:?}")))?;
    raw.parse::<u32>().map_err(|e| invalid_parameters(format!("parameter {key:?} = {raw:?} does not parse as u32: {e}")))
}

fn parse_params(spec: &pb::JobSpec) -> Result<TilerParams, pb::JobFailure> {
    let parameters = &spec.parameters;

    let min_level = parse_u32_param(parameters, "min_level")?;
    let max_level = parse_u32_param(parameters, "max_level")?;
    if min_level > max_level {
        return Err(invalid_parameters(format!("min_level ({min_level}) must not exceed max_level ({max_level})")));
    }
    if max_level > MAX_LEVEL_CEILING {
        return Err(invalid_parameters(format!("max_level ({max_level}) exceeds this executor's documented ceiling ({MAX_LEVEL_CEILING})")));
    }

    let tile_size = match parameters.get("tile_size") {
        None => DEFAULT_TILE_SIZE,
        Some(raw) => raw.parse::<u32>().map_err(|e| invalid_parameters(format!("parameter \"tile_size\" = {raw:?} does not parse as u32: {e}")))?,
    };
    if !(MIN_TILE_SIZE..=MAX_TILE_SIZE).contains(&tile_size) || !tile_size.is_power_of_two() {
        return Err(invalid_parameters(format!("tile_size ({tile_size}) must be a power of two in [{MIN_TILE_SIZE}, {MAX_TILE_SIZE}]")));
    }

    let output_raw = parameters.get("output").map(String::as_str).unwrap_or("imagery");
    let output = match output_raw {
        "imagery" => OutputKind::Imagery,
        "terrain" | "tiles3d" => {
            return Err(executor_unavailable(format!("output kind {output_raw:?} is a recognised future kind (P2/P3) not implemented by this round's TilerExecutor")));
        }
        other => return Err(invalid_parameters(format!("unrecognised \"output\" value {other:?}: expected \"imagery\", \"terrain\", or \"tiles3d\""))),
    };

    Ok(TilerParams { min_level, max_level, tile_size, output })
}

/// Nearest-neighbour sample of `raster` at tile-local pixel `(px, py)` within `tile_bounds`
/// -- see this module's own doc for the exact formula.
fn sample_nearest(raster: &crate::raster::Raster, tile_bounds: &crate::scheme::BoundsDeg, tile_size: u32, px: u32, py: u32) -> [u8; 3] {
    let lon = tile_bounds.west + (px as f64 + 0.5) / tile_size as f64 * (tile_bounds.east - tile_bounds.west);
    let lat = tile_bounds.north - (py as f64 + 0.5) / tile_size as f64 * (tile_bounds.north - tile_bounds.south);

    let col_f = (lon - raster.west) / (raster.east - raster.west) * raster.width as f64;
    let row_f = (raster.north - lat) / (raster.north - raster.south) * raster.height as f64;

    let col = (col_f.floor() as i64).clamp(0, raster.width as i64 - 1) as u32;
    let row = (row_f.floor() as i64).clamp(0, raster.height as i64 - 1) as u32;

    raster.pixel_rgb(col, row).expect("col/row clamped into range")
}

/// Renders one `tile_size x tile_size` RGB8 tile of `raster` at `tile`, PNG-encoded.
fn render_imagery_tile(raster: &crate::raster::Raster, tile: crate::scheme::Tile, tile_size: u32) -> Vec<u8> {
    let bounds = crate::scheme::tile_bounds_deg(tile);
    let mut pixels = Vec::with_capacity(tile_size as usize * tile_size as usize * 3);
    for py in 0..tile_size {
        for px in 0..tile_size {
            let rgb = sample_nearest(raster, &bounds, tile_size, px, py);
            pixels.extend_from_slice(&rgb);
        }
    }
    crate::png::encode_rgb8(tile_size, tile_size, &pixels).expect("pixels length matches width*height*3 by construction")
}

/// `(level, tiles_done_so_far_in_this_run, tiles_total_in_this_run)` -- [`TilerExecutor`]'s
/// own progress-callback shape, factored into a named alias (clippy's `type_complexity` lint,
/// `-D warnings`, refuses the bare `Option<Box<dyn Fn(..) + Send + Sync>>` field type inline;
/// naming it is the fix the lint itself suggests, never `#[allow(...)]`, which this
/// workspace's own binding rule forbids).
type TileProgressCallback = Box<dyn Fn(u32, usize, usize) + Send + Sync>;

/// The tiler `Executor`, `kind == "tiler"`. `key_prefix` must equal the `prefix` the
/// `Runner`'s own configured `ObjectSink` (this round: always a `MemoryObjectSink`) is
/// constructed with -- see this module's own doc, "The manifest-vs-sink ordering
/// constraint".
pub struct TilerExecutor {
    key_prefix: String,
    /// H5b-1 (`docs/heavy-plan.md` H5, round 3): an optional per-tile progress callback --
    /// see [`TileProgressCallback`]'s own doc for its exact signature. Called immediately
    /// after each tile is rendered and PNG-encoded, before ANY tile in this run is handed to
    /// the `ObjectSink` (this executor runs entirely before the sink -- this module's own
    /// doc, "The manifest-vs-sink ordering constraint"; a progress callback cannot change
    /// that, so it reports rendering progress, not storing progress). `None` (what
    /// [`TilerExecutor::new`] sets) costs nothing beyond one `Option` check per tile --
    /// `crates/av-jobs/src/bin/av-tile-fixture.rs` is the one caller that sets it (via
    /// [`TilerExecutor::with_progress`]), to print stderr progress for a large run (that
    /// task's own brief: "must print, on stderr, enough progress that a ten-gigabyte run is
    /// observable").
    progress: Option<TileProgressCallback>,
}

impl std::fmt::Debug for TilerExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A `Box<dyn Fn(..)>` has no `Debug` impl of its own (no closure does) -- named and
        // acknowledged rather than silently omitted, mirroring `crates/av-jobs/tests/
        // store_tiler.rs::StoreBridge`'s identical `finish_non_exhaustive` shape for the
        // identical reason (a field this crate cannot meaningfully print).
        f.debug_struct("TilerExecutor").field("key_prefix", &self.key_prefix).finish_non_exhaustive()
    }
}

impl TilerExecutor {
    pub fn new(key_prefix: impl Into<String>) -> Self {
        Self { key_prefix: key_prefix.into(), progress: None }
    }

    /// Like [`TilerExecutor::new`], but `progress` is called after every tile this run
    /// renders -- see [`TilerExecutor`]'s own field doc for the exact contract and why it
    /// exists.
    pub fn with_progress(key_prefix: impl Into<String>, progress: impl Fn(u32, usize, usize) + Send + Sync + 'static) -> Self {
        Self { key_prefix: key_prefix.into(), progress: Some(Box::new(progress)) }
    }
}

/// Renders tile `tile`, PNG-encodes it, and returns the finished bytes alongside the
/// `pb::TileEntry` describing them -- the one piece of per-tile logic both the buffered
/// (`TilerExecutor::run_imagery`) and streaming (`TilerExecutor::run_imagery_streaming`)
/// code paths call, so the two paths can never disagree about a tile's bytes, hash, or
/// manifest entry: this function, not two independently-written copies of it, is why the
/// load-bearing manifest-equality test (`crates/av-jobs/tests/tiler.rs`) is expected to pass
/// rather than merely hoped to.
fn render_and_describe_tile(raster: &crate::raster::Raster, tile: crate::scheme::Tile, tile_size: u32, key_prefix: &str) -> (Vec<u8>, pb::TileEntry) {
    let png_bytes = render_imagery_tile(raster, tile, tile_size);
    let sha256_hex = crate::hash::hex_encode(&openssl::sha::sha256(&png_bytes));
    let object_key = content_addressed_key(key_prefix, &sha256_hex);
    let entry = pb::TileEntry {
        level: tile.level,
        x: tile.x,
        y: tile.y,
        sha256: sha256_hex,
        size_bytes: png_bytes.len() as u64,
        // Deliberately empty -- see `TileEntry.uri`'s own doc comment in `heavy.proto`. This
        // executor runs before the `ObjectSink` and cannot know its URI scheme; the first
        // version hard-coded `"memory://"` and was measurably wrong against a real store. A
        // fully-qualified URI would also make this manifest's hash -- the tile set's
        // identity -- depend on which bucket happened to hold the tiles.
        uri: String::new(),
        media_type: IMAGERY_TILE_MEDIA_TYPE.to_string(),
        object_key,
    };
    (png_bytes, entry)
}

/// Builds the `TileSetManifest` both `run_imagery` and `run_imagery_streaming` produce --
/// factored out for the identical reason as [`render_and_describe_tile`]: one place that
/// decides the manifest's encoded bytes, so the streaming and buffered paths cannot drift
/// apart on field order, defaulting, or a forgotten field.
fn build_tileset_manifest(params: &TilerParams, raster: &crate::raster::Raster, spec: &pb::JobSpec, input: &JobInput, tile_entries: Vec<pb::TileEntry>, key_prefix: &str) -> pb::TileSetManifest {
    pb::TileSetManifest {
        kind: pb::TileSetKind::Imagery as i32,
        scheme: crate::scheme::SCHEME_ID.to_string(),
        min_level: params.min_level,
        max_level: params.max_level,
        tile_size: params.tile_size,
        bounds: Some(pb::GeoBbox { min_lon: raster.west, min_lat: raster.south, max_lon: raster.east, max_lat: raster.north }),
        tiles: tile_entries,
        source_sha256: vec![input.asset.sha256.clone()],
        parameters: spec.parameters.clone(),
        root_uri: String::new(),
        job_id: spec.job_id.clone(),
        object_key_prefix: key_prefix.to_string(),
    }
}

impl Executor for TilerExecutor {
    fn execute(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let params = parse_params(spec)?;

        if inputs.len() != 1 {
            return Err(invalid_input(format!("tiler requires exactly one input (the raster); got {}", inputs.len())));
        }
        let raster = crate::raster::decode(&inputs[0].bytes).map_err(|e| invalid_input(format!("input raster does not decode: {e}")))?;

        match params.output {
            OutputKind::Imagery => self.run_imagery(spec, &raster, &inputs[0], &params),
        }
    }

    fn declares_manifest(&self) -> bool {
        true
    }

    /// See [`TilerExecutor::run_imagery_streaming`]'s own doc for what this changes about
    /// the job's recorded outputs and why peak memory drops from "the whole tile set" to
    /// "one tile". Parameter parsing, input-count checking and raster decoding are identical
    /// to [`Executor::execute`]'s own -- only which per-output-kind method runs differs.
    fn execute_streaming(&self, spec: &pb::JobSpec, inputs: &[JobInput], sink: &dyn crate::runner::ObjectSink, label: &pb::Label) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let params = parse_params(spec)?;

        if inputs.len() != 1 {
            return Err(invalid_input(format!("tiler requires exactly one input (the raster); got {}", inputs.len())));
        }
        let raster = crate::raster::decode(&inputs[0].bytes).map_err(|e| invalid_input(format!("input raster does not decode: {e}")))?;

        match params.output {
            OutputKind::Imagery => self.run_imagery_streaming(spec, &raster, &inputs[0], &params, sink, label),
        }
    }
}

impl TilerExecutor {
    fn run_imagery(&self, spec: &pb::JobSpec, raster: &crate::raster::Raster, input: &JobInput, params: &TilerParams) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let mut outputs: Vec<JobOutput> = Vec::new();
        let mut tile_entries: Vec<pb::TileEntry> = Vec::new();

        // Precomputed once, purely so a progress callback can report "N of TOTAL" -- cheap
        // (`tiles_covering` is pure arithmetic over `raster.bounds()`, no I/O), and computed
        // the identical way the real loop below computes it level by level, never a separate
        // formula that could disagree with what actually gets rendered.
        let total_tiles: usize = (params.min_level..=params.max_level).map(|level| crate::scheme::tiles_covering(&raster.bounds(), level).len()).sum();
        let mut tiles_done: usize = 0;

        for level in params.min_level..=params.max_level {
            let tiles = crate::scheme::tiles_covering(&raster.bounds(), level);
            for tile in tiles {
                let (png_bytes, entry) = render_and_describe_tile(raster, tile, params.tile_size, &self.key_prefix);
                tiles_done += 1;
                if let Some(cb) = &self.progress {
                    cb(level, tiles_done, total_tiles);
                }
                tile_entries.push(entry);
                outputs.push(JobOutput { bytes: png_bytes, media_type: IMAGERY_TILE_MEDIA_TYPE.to_string(), manifest: false });
            }
        }

        let manifest = build_tileset_manifest(params, raster, spec, input, tile_entries, &self.key_prefix);
        let manifest_bytes: Vec<u8> = prost::Message::encode_to_vec(&manifest);
        outputs.push(JobOutput { bytes: manifest_bytes, media_type: MANIFEST_MEDIA_TYPE.to_string(), manifest: true });

        Ok(outputs)
    }

    /// **The fix for the finding this round opened with**: `run_imagery` (above) renders
    /// every tile across every requested level into one `Vec<JobOutput>` before returning
    /// it, so a ten-gigabyte tile set is ten gigabytes of resident memory at that `Vec`'s
    /// peak -- unreachable on an 8 GB VM host, and the exact gap `crates/av-jobs/src/bin/
    /// av-tile-fixture.rs`'s own module doc recorded as an open item. This method closes it:
    /// each tile is rendered, hashed, stored through `sink` (`ObjectSink::put`, the same
    /// trait [`crate::runner::Runner`] itself stores every output through), and then
    /// **dropped** before the next tile is rendered. Peak memory for this method is one
    /// tile's PNG bytes plus the already-decoded source `raster` plus the manifest's own
    /// small, bytes-free `TileEntry` list (`level`/`x`/`y`/`sha256`/`size_bytes`/
    /// `media_type`/`object_key` -- at most a few hundred bytes per tile, not the tile's own
    /// pixels) -- never the whole tile set at once, regardless of `--tile-size`/level range.
    ///
    /// **What this changes about the job's recorded outputs**: this method returns exactly
    /// ONE [`JobOutput`] -- the manifest, `manifest: true` -- because every tile was already
    /// stored, directly, above; there is nothing left for [`crate::runner::Runner::
    /// run_one_streaming`]'s own step 5 (`ObjectSink::put` over whatever the executor
    /// returned) to do except store that one manifest. So `JobCompletion.outputs` on this
    /// path carries one entry, not one entry per tile the way `run_imagery`'s
    /// `JobCompletion.outputs` does. **Nothing about the job's real outputs becomes
    /// unrecorded by that**: the manifest's own encoded bytes list every tile's
    /// `object_key` and `sha256` (`pb::TileEntry`, `heavy.proto`) under
    /// `TileSetManifest.object_key_prefix` -- a reader that has the manifest already has
    /// everything a per-tile `JobCompletion.outputs` entry would have named, because the
    /// manifest is not a summary of the job's outputs, it already IS their durable index
    /// (this module's own doc, "The manifest-vs-sink ordering constraint").
    ///
    /// **Byte-identical to `run_imagery` for the same input** -- both call
    /// [`render_and_describe_tile`] for every tile, in the identical `(level, tile)` order,
    /// and [`build_tileset_manifest`] with the identical arguments; the manifest's own
    /// SHA-256 (the tile set's identity) does not depend on which of the two produced it.
    /// `crates/av-jobs/tests/tiler.rs` proves this by running both paths over the same
    /// fixture and asserting `manifest_sha256` equality AND that every stored tile's bytes
    /// (not just its hash) are the same across both paths' own sinks.
    fn run_imagery_streaming(
        &self,
        spec: &pb::JobSpec,
        raster: &crate::raster::Raster,
        input: &JobInput,
        params: &TilerParams,
        sink: &dyn crate::runner::ObjectSink,
        label: &pb::Label,
    ) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let mut tile_entries: Vec<pb::TileEntry> = Vec::new();

        let total_tiles: usize = (params.min_level..=params.max_level).map(|level| crate::scheme::tiles_covering(&raster.bounds(), level).len()).sum();
        let mut tiles_done: usize = 0;

        for level in params.min_level..=params.max_level {
            let tiles = crate::scheme::tiles_covering(&raster.bounds(), level);
            for tile in tiles {
                let (png_bytes, entry) = render_and_describe_tile(raster, tile, params.tile_size, &self.key_prefix);
                tiles_done += 1;
                if let Some(cb) = &self.progress {
                    cb(level, tiles_done, total_tiles);
                }
                // Stored immediately, then `png_bytes` goes out of scope at the end of this
                // block and is freed -- this loop never holds more than one tile's bytes.
                let stored = sink.put(&png_bytes, IMAGERY_TILE_MEDIA_TYPE, label)?;
                debug_assert_eq!(stored.sha256, entry.sha256, "an ObjectSink must content-address a tile's bytes to the same sha256 this executor independently computed");
                tile_entries.push(entry);
            }
        }

        let manifest = build_tileset_manifest(params, raster, spec, input, tile_entries, &self.key_prefix);
        let manifest_bytes: Vec<u8> = prost::Message::encode_to_vec(&manifest);
        Ok(vec![JobOutput { bytes: manifest_bytes, media_type: MANIFEST_MEDIA_TYPE.to_string(), manifest: true }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with_params(params: &[(&str, &str)]) -> pb::JobSpec {
        let mut parameters = BTreeMap::new();
        for (k, v) in params {
            parameters.insert(k.to_string(), v.to_string());
        }
        pb::JobSpec { job_id: "job-1".to_string(), kind: "tiler".to_string(), parameters, ..Default::default() }
    }

    // -- parameter validation -------------------------------------------------------------

    #[test]
    fn parse_params_accepts_defaults_and_required_fields() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "2")]);
        let p = parse_params(&spec).unwrap();
        assert_eq!(p.min_level, 0);
        assert_eq!(p.max_level, 2);
        assert_eq!(p.tile_size, DEFAULT_TILE_SIZE);
        assert_eq!(p.output, OutputKind::Imagery);
    }

    #[test]
    fn parse_params_refuses_missing_min_level() {
        let spec = spec_with_params(&[("max_level", "2")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_unparseable_level() {
        let spec = spec_with_params(&[("min_level", "not-a-number"), ("max_level", "2")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_min_greater_than_max() {
        let spec = spec_with_params(&[("min_level", "3"), ("max_level", "1")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_max_level_above_ceiling() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "999")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_a_tile_size_that_is_not_a_power_of_two() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("tile_size", "100")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_an_out_of_bounds_tile_size() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("tile_size", "8")]);
        assert_eq!(parse_params(&spec).unwrap_err().kind, pb::JobFailureKind::InvalidParameters as i32);
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("tile_size", "8192")]);
        assert_eq!(parse_params(&spec).unwrap_err().kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_an_unknown_output_value() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("output", "bogus")]);
        let err = parse_params(&spec).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::InvalidParameters as i32);
    }

    #[test]
    fn parse_params_refuses_terrain_and_tiles3d_as_not_implemented_this_round() {
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("output", "terrain")]);
        assert_eq!(parse_params(&spec).unwrap_err().kind, pb::JobFailureKind::ExecutorUnavailable as i32);
        let spec = spec_with_params(&[("min_level", "0"), ("max_level", "0"), ("output", "tiles3d")]);
        assert_eq!(parse_params(&spec).unwrap_err().kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    }

    // -- sample_nearest / render_imagery_tile ---------------------------------------------

    #[test]
    fn sample_nearest_picks_the_pixel_whose_centre_is_closest() {
        // A 2x2 raster spanning the whole globe: NW=red, NE=green, SW=blue, SE=white.
        let pixels = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let raster = crate::raster::decode(&crate::raster::encode(2, 2, -180.0, -90.0, 180.0, 90.0, &pixels)).unwrap();
        let bounds = crate::scheme::BoundsDeg { west: -180.0, south: -90.0, east: 180.0, north: 90.0 };
        // Tile-local pixel (0,0) of a 2x2-sample tile centres at lon=-90,lat=45 -> NW quadrant -> red.
        assert_eq!(sample_nearest(&raster, &bounds, 2, 0, 0), [255, 0, 0]);
        // (1,0) centres at lon=90,lat=45 -> NE quadrant -> green.
        assert_eq!(sample_nearest(&raster, &bounds, 2, 1, 0), [0, 255, 0]);
        // (0,1) centres at lon=-90,lat=-45 -> SW quadrant -> blue.
        assert_eq!(sample_nearest(&raster, &bounds, 2, 0, 1), [0, 0, 255]);
        // (1,1) centres at lon=90,lat=-45 -> SE quadrant -> white.
        assert_eq!(sample_nearest(&raster, &bounds, 2, 1, 1), [255, 255, 255]);
    }
}

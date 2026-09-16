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
//! See `TileSetManifest`'s own doc comment (`heavy.proto`) for the full reasoning: this
//! executor computes each tile's `sha256` itself and predicts its `uri` via
//! [`crate::runner::content_addressed_key`] -- the exact function
//! [`crate::runner::MemoryObjectSink::put`] independently calls for the same bytes -- rather
//! than inventing a second key scheme. **This round's only `ObjectSink` is
//! `MemoryObjectSink`, which always returns `"memory://{key}"`**; [`TilerExecutor`]
//! hard-codes that same `"memory://"` scheme prefix, so a caller wiring up a `Runner` must
//! construct the `ObjectSink` and this executor with the identical `prefix` string for the
//! manifest's `uri`s to actually match what the sink assigns -- an honest, visible coupling
//! this module does not attempt to hide. A real, store-backed `ObjectSink` (task 3b's own
//! deferred P4) would need its own URI scheme, at which point this hard-coded
//! `"memory://"` assumption becomes something that implementation must revisit explicitly.

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

/// The tiler `Executor`, `kind == "tiler"`. `key_prefix` must equal the `prefix` the
/// `Runner`'s own configured `ObjectSink` (this round: always a `MemoryObjectSink`) is
/// constructed with -- see this module's own doc, "The manifest-vs-sink ordering
/// constraint".
#[derive(Debug)]
pub struct TilerExecutor {
    key_prefix: String,
}

impl TilerExecutor {
    pub fn new(key_prefix: impl Into<String>) -> Self {
        Self { key_prefix: key_prefix.into() }
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
}

impl TilerExecutor {
    fn run_imagery(&self, spec: &pb::JobSpec, raster: &crate::raster::Raster, input: &JobInput, params: &TilerParams) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let mut outputs: Vec<JobOutput> = Vec::new();
        let mut tile_entries: Vec<pb::TileEntry> = Vec::new();

        for level in params.min_level..=params.max_level {
            let tiles = crate::scheme::tiles_covering(&raster.bounds(), level);
            for tile in tiles {
                let png_bytes = render_imagery_tile(raster, tile, params.tile_size);
                let sha256_hex = crate::hash::hex_encode(&openssl::sha::sha256(&png_bytes));
                let key = content_addressed_key(&self.key_prefix, &sha256_hex);
                let uri = format!("memory://{key}");

                tile_entries.push(pb::TileEntry {
                    level: tile.level,
                    x: tile.x,
                    y: tile.y,
                    sha256: sha256_hex,
                    size_bytes: png_bytes.len() as u64,
                    uri,
                    media_type: IMAGERY_TILE_MEDIA_TYPE.to_string(),
                });
                outputs.push(JobOutput { bytes: png_bytes, media_type: IMAGERY_TILE_MEDIA_TYPE.to_string(), manifest: false });
            }
        }

        let manifest = pb::TileSetManifest {
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
        };
        let manifest_bytes: Vec<u8> = prost::Message::encode_to_vec(&manifest);
        outputs.push(JobOutput { bytes: manifest_bytes, media_type: MANIFEST_MEDIA_TYPE.to_string(), manifest: true });

        Ok(outputs)
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

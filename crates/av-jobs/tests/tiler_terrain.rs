//! P3a acceptance evidence for `output == "terrain"` (`crates/av-jobs::tiler::TilerExecutor`,
//! `crates/av-jobs::terrain`): the streaming/buffered determinism this crate's own imagery
//! path already proves (`crates/av-jobs/tests/tiler.rs`), extended to terrain, PLUS an
//! anchored golden -- independently re-derived by a Python script (this file's own
//! `PYTHON_VERIFY_SCRIPT`) using nothing but `hashlib`/`struct` for the payload and the
//! generated Python protobuf runtime for the manifest, never this crate's own code -- proving
//! this is a real proof, not a self-referential pin. Mirrors the shape (and reuses the exact
//! fixture raster) of `crates/av-jobs/tests/tiler.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb;
use av_jobs::clock::TestClock;
use av_jobs::queue::JobQueue;
use av_jobs::runner::{MemoryObjectSink, MemoryObjectSource, ObjectSink, Runner};
use av_jobs::tiler::TilerExecutor;
use av_label::ClearanceLadder;

#[derive(Debug, Clone)]
struct SharedSink(std::sync::Arc<MemoryObjectSink>);

impl ObjectSink for SharedSink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        self.0.put(bytes, media_type, label)
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("av-jobs-tiler-terrain-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ladder() -> ClearanceLadder {
    ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
}

fn valid_label() -> pb::Label {
    pb::Label { marking: "CUI".to_string(), caveats: vec![] }
}

const KEY_PREFIX: &str = "tiles";
const RASTER_URI: &str = "memory://input-raster";

/// The identical fixture raster `crates/av-jobs/tests/tiler.rs::fixture_raster_bytes`
/// documents byte for byte -- reused here (not imported: each integration test binary is its
/// own crate) so both files' anchored derivations start from the same known-good input.
fn fixture_raster_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(80);
    out.extend_from_slice(b"AVRASTER");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(-180.0f64).to_le_bytes());
    out.extend_from_slice(&(-90.0f64).to_le_bytes());
    out.extend_from_slice(&(180.0f64).to_le_bytes());
    out.extend_from_slice(&(90.0f64).to_le_bytes());
    let pixels: [u8; 24] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240];
    out.extend_from_slice(&pixels);
    out
}

fn fixture_raster_sha256_hex() -> String {
    av_jobs::hash::hex_encode(&openssl::sha::sha256(&fixture_raster_bytes()))
}

fn fixture_params() -> BTreeMap<String, String> {
    let mut p = BTreeMap::new();
    p.insert("min_level".to_string(), "0".to_string());
    p.insert("max_level".to_string(), "1".to_string());
    p.insert("tile_size".to_string(), "16".to_string());
    p.insert("output".to_string(), "terrain".to_string());
    p
}

fn tiler_spec(job_id: &str) -> pb::JobSpec {
    pb::JobSpec {
        job_id: job_id.to_string(),
        kind: "tiler".to_string(),
        inputs: vec![pb::AssetRef {
            uri: RASTER_URI.to_string(),
            sha256: fixture_raster_sha256_hex(),
            size_bytes: fixture_raster_bytes().len() as u64,
            media_type: "application/vnd.altavista.raster+raw".to_string(),
            ..Default::default()
        }],
        parameters: fixture_params(),
        label: Some(valid_label()),
        requested_tai_ns: 1,
        executor: pb::JobExecutorKind::Process as i32,
        ..Default::default()
    }
}

fn fresh_runner_with_shared_sink<'a>(dir: &Path, clock: &'a TestClock, raster_bytes: Vec<u8>) -> (Runner<'a>, std::sync::Arc<MemoryObjectSink>) {
    let (queue, _) = JobQueue::open(dir, "q").unwrap();
    let source = Box::new(MemoryObjectSource::new());
    source.insert(RASTER_URI, raster_bytes);
    let sink = std::sync::Arc::new(MemoryObjectSink::new(KEY_PREFIX));
    let mut runner = Runner::new(queue, source, Box::new(SharedSink(sink.clone())), ladder(), clock);
    runner.register_executor("tiler", Box::new(TilerExecutor::new(KEY_PREFIX)));
    (runner, sink)
}

// -- item 1: streaming and buffered paths agree, byte for byte -------------------------

#[test]
fn terrain_streaming_and_buffered_paths_produce_byte_identical_manifests_and_tiles() {
    const JOB_ID: &str = "job-terrain-stream-equality";

    let dir_buffered = TempDir::new("buffered");
    let clock_buffered = TestClock::new(5_000);
    let (runner_buffered, sink_buffered) = fresh_runner_with_shared_sink(dir_buffered.path(), &clock_buffered, fixture_raster_bytes());
    let spec = tiler_spec(JOB_ID);
    runner_buffered.queue().submit(&spec).unwrap();
    let completion_buffered = runner_buffered.run_one(&spec).unwrap();
    assert!(completion_buffered.ok, "{completion_buffered:?}");

    let dir_streaming = TempDir::new("streaming");
    let clock_streaming = TestClock::new(5_000);
    let (runner_streaming, sink_streaming) = fresh_runner_with_shared_sink(dir_streaming.path(), &clock_streaming, fixture_raster_bytes());
    runner_streaming.queue().submit(&spec).unwrap();
    let completion_streaming = runner_streaming.run_one_streaming(&spec).unwrap();
    assert!(completion_streaming.ok, "{completion_streaming:?}");

    assert_eq!(completion_streaming.manifest_sha256, completion_buffered.manifest_sha256, "a terrain tile set's identity must not depend on how it was written");
    assert_eq!(completion_streaming.outputs.len(), 1, "streaming: only the manifest is a recorded output");
    let buffered_tile_outputs = completion_buffered.outputs.iter().filter(|a| a.media_type == av_jobs::tiler::TERRAIN_TILE_MEDIA_TYPE).count();
    assert_eq!(buffered_tile_outputs, 10, "sanity: same fixture as the imagery tests, same tile count");
    assert_eq!(completion_buffered.outputs.len(), buffered_tile_outputs + 1);

    let manifest_bytes_buffered = sink_buffered.get(&completion_buffered.manifest_sha256).unwrap();
    let manifest_bytes_streaming = sink_streaming.get(&completion_streaming.manifest_sha256).unwrap();
    assert_eq!(manifest_bytes_buffered, manifest_bytes_streaming);

    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes_streaming.as_slice()).unwrap();
    assert_eq!(manifest.kind, pb::TileSetKind::Terrain as i32);
    assert_eq!(manifest.tiles.len(), 10);
    for tile in &manifest.tiles {
        assert_eq!(tile.media_type, av_jobs::tiler::TERRAIN_TILE_MEDIA_TYPE);
        let b1 = sink_buffered.get(&tile.sha256).unwrap_or_else(|| panic!("tile ({},{},{}) missing from buffered sink", tile.level, tile.x, tile.y));
        let b2 = sink_streaming.get(&tile.sha256).unwrap_or_else(|| panic!("tile ({},{},{}) missing from streaming sink", tile.level, tile.x, tile.y));
        assert_eq!(b1, b2);
        assert_eq!(av_jobs::hash::hex_encode(&openssl::sha::sha256(&b2)), tile.sha256);
    }
}

// -- item 2: every stored terrain tile round-trips through crate::terrain::decode -------

#[test]
fn every_terrain_tile_round_trips_through_crate_terrain_decode() {
    let dir = TempDir::new("roundtrip");
    let clock = TestClock::new(1_000);
    let (runner, sink) = fresh_runner_with_shared_sink(dir.path(), &clock, fixture_raster_bytes());
    let spec = tiler_spec("job-terrain-roundtrip");
    runner.queue().submit(&spec).unwrap();
    let completion = runner.run_one(&spec).unwrap();
    assert!(completion.ok, "{completion:?}");

    let manifest_bytes = sink.get(&completion.manifest_sha256).unwrap();
    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes.as_slice()).unwrap();
    assert_eq!(manifest.tiles.len(), 10);
    for tile in &manifest.tiles {
        let bytes = sink.get(&tile.sha256).unwrap();
        let decoded = av_jobs::terrain::decode(&bytes).unwrap_or_else(|e| panic!("tile ({},{},{}) failed to decode: {e}", tile.level, tile.x, tile.y));
        assert_eq!(decoded.samples_per_side, 16);
        assert_eq!(decoded.samples.len(), 256);
    }
}

// -- item 3: anchored golden -- independently re-derived by Python, not this crate ------

/// Reads the raw AVRASTER fixture (argv[1]), the encoded `TileSetManifest` (argv[2]), and the
/// stored bytes of tile `(level=0, x=0, y=0)` (argv[3]) -- then:
///   1. decodes the manifest with the generated Python protobuf runtime (`altavista.pb`,
///      argv[4] is the repo root to import it from) and checks every field this test cares
///      about, entirely independent of `prost`;
///   2. independently re-implements, from nothing but this crate's own documented formulas
///      (`crate::tiler`'s "Resampling" section and `crate::terrain`'s own module doc -- NOT
///      by calling this crate's code), the exact bytes of tile (0,0,0) from the raw raster,
///      using only `struct`/`hashlib`, and compares them byte-for-byte against argv[3].
///
/// Prints "OK <sha256 of tile (0,0,0)>" on success; any mismatch raises an `AssertionError`
/// (non-zero exit, caught by the Rust test).
const PYTHON_VERIFY_SCRIPT: &str = r#"
import hashlib
import struct
import sys

raster_path, manifest_path, tile000_path, repo_root = sys.argv[1:5]
sys.path.insert(0, repo_root)
from altavista.pb.altavista.v1 import heavy_pb2  # noqa: E402

# -- 1. manifest, decoded by the Python protobuf runtime (not prost) ------------------
manifest_bytes = open(manifest_path, "rb").read()
m = heavy_pb2.TileSetManifest()
m.ParseFromString(manifest_bytes)

assert m.kind == heavy_pb2.TILE_SET_KIND_TERRAIN, m.kind
assert m.scheme == "geographic-plate-carree-2x1", m.scheme
assert m.min_level == 0 and m.max_level == 1 and m.tile_size == 16
assert (m.bounds.min_lon, m.bounds.min_lat, m.bounds.max_lon, m.bounds.max_lat) == (-180.0, -90.0, 180.0, 90.0)
expected_addrs = [(0, 0, 0), (0, 1, 0), (1, 0, 0), (1, 0, 1), (1, 1, 0), (1, 1, 1), (1, 2, 0), (1, 2, 1), (1, 3, 0), (1, 3, 1)]
got_addrs = [(t.level, t.x, t.y) for t in m.tiles]
assert got_addrs == expected_addrs, got_addrs
for t in m.tiles:
    assert t.media_type == "application/vnd.altavista.terrain-tile+raw", t.media_type
    assert t.uri == "", "TileEntry.uri must stay empty (ordering-constraint doc)"
    assert t.object_key.startswith("tiles/"), t.object_key
raster_bytes = open(raster_path, "rb").read()
assert list(m.source_sha256) == [hashlib.sha256(raster_bytes).hexdigest()]
assert m.object_key_prefix == "tiles"
assert dict(m.parameters) == {"min_level": "0", "max_level": "1", "tile_size": "16", "output": "terrain"}

# Canonical-encoding check: re-serialising the decoded message reproduces the identical
# bytes -- proves this is a real, unambiguous decode, not merely a partial field read.
reserialized = m.SerializeToString(deterministic=True)
assert reserialized == manifest_bytes, "re-serialising the Python-decoded manifest did not reproduce the original bytes"

# -- 2. tile (0,0,0)'s own bytes, rebuilt from raw formulas only (hashlib/struct) ------
MAGIC, VERSION, WIDTH, HEIGHT, CHANNELS = struct.unpack_from("<8sIII I", raster_bytes, 0)
assert MAGIC == b"AVRASTER"
west, south, east, north = struct.unpack_from("<dddd", raster_bytes, 24)
pixels = raster_bytes[56:]

def pixel_rgb(col, row):
    idx = (row * WIDTH + col) * 3
    return pixels[idx], pixels[idx + 1], pixels[idx + 2]

# tile (level=0, x=0, y=0): nx=2**(0+1)=2, ny=2**0=1, dLon=360/2=180, dLat=180/1=180 ->
# west=-180, south=-90, east=0, north=90 (crate::scheme's own documented formula).
TW, TS, TE, TN = -180.0, -90.0, 0.0, 90.0
TILE_SIZE = 16
samples = []
for py in range(TILE_SIZE):
    for px in range(TILE_SIZE):
        lon = TW + (px + 0.5) / TILE_SIZE * (TE - TW)
        lat = TN - (py + 0.5) / TILE_SIZE * (TN - TS)
        col = min(max(int((lon - west) / (east - west) * WIDTH), 0), WIDTH - 1)
        row = min(max(int((north - lat) / (north - south) * HEIGHT), 0), HEIGHT - 1)
        r, g, _b = pixel_rgb(col, row)
        h = (r << 8) | g
        if h >= 32768:
            h -= 65536
        samples.append(h)

header = b"AVTERRHM" + struct.pack("<IIII", 1, TILE_SIZE, 1, 1)
body = b"".join(struct.pack("<h", s) for s in samples)
expected_tile_bytes = header + body

actual_tile_bytes = open(tile000_path, "rb").read()
assert actual_tile_bytes == expected_tile_bytes, (
    f"tile (0,0,0) mismatch: expected {len(expected_tile_bytes)} bytes "
    f"(sha256 {hashlib.sha256(expected_tile_bytes).hexdigest()}), got {len(actual_tile_bytes)} bytes "
    f"(sha256 {hashlib.sha256(actual_tile_bytes).hexdigest()})"
)

print(f"OK {hashlib.sha256(actual_tile_bytes).hexdigest()}")
"#;

#[test]
fn terrain_output_is_anchored_against_an_independent_python_derivation() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().and_then(|p| p.parent()).expect("crates/av-jobs has two ancestors: crates/, then the repo root");
    let python_path = repo_root.join(".venv/bin/python");
    if !python_path.exists() {
        eprintln!("SKIPPED: terrain_output_is_anchored_against_an_independent_python_derivation -- {python_path:?} not found on this host");
        return;
    }

    let dir = TempDir::new("anchor");
    let clock = TestClock::new(9_000);
    let (runner, sink) = fresh_runner_with_shared_sink(dir.path(), &clock, fixture_raster_bytes());
    let spec = tiler_spec("job-terrain-anchor");
    runner.queue().submit(&spec).unwrap();
    let completion = runner.run_one(&spec).unwrap();
    assert!(completion.ok, "{completion:?}");

    let manifest_bytes = sink.get(&completion.manifest_sha256).unwrap();
    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes.as_slice()).unwrap();
    let tile000 = manifest.tiles.iter().find(|t| (t.level, t.x, t.y) == (0, 0, 0)).expect("tile (0,0,0) present");
    let tile000_bytes = sink.get(&tile000.sha256).unwrap();

    let work_dir = dir.path();
    let raster_path = work_dir.join("raster.bin");
    let manifest_path = work_dir.join("manifest.pb");
    let tile000_path = work_dir.join("tile000.bin");
    std::fs::write(&raster_path, fixture_raster_bytes()).unwrap();
    std::fs::write(&manifest_path, &manifest_bytes).unwrap();
    std::fs::write(&tile000_path, &tile000_bytes).unwrap();
    let script_path = work_dir.join("verify.py");
    std::fs::write(&script_path, PYTHON_VERIFY_SCRIPT).unwrap();

    let output = std::process::Command::new(&python_path)
        .arg(&script_path)
        .arg(&raster_path)
        .arg(&manifest_path)
        .arg(&tile000_path)
        .arg(repo_root)
        .output()
        .expect("failed to spawn python");
    assert!(output.status.success(), "python anchoring script failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().starts_with("OK "), "unexpected stdout: {stdout}");
}

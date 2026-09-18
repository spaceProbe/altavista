//! P3a acceptance evidence for `output == "tiles3d"` (`crates/av-jobs::tiler::TilerExecutor`,
//! `crates/av-jobs::tiles3d`): streaming/buffered determinism (mirroring
//! `crates/av-jobs/tests/tiler.rs`'s own imagery proof, extended to cover the extra
//! `tileset.json` output this kind has), PLUS an anchored golden -- independently re-derived
//! by a Python script (`PYTHON_VERIFY_SCRIPT` below) using nothing but `hashlib`/`struct`/
//! `math` for the geo-referencing and `.pnts` payload, and the generated Python protobuf
//! runtime for the manifest, never this crate's own code.

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
        let dir = std::env::temp_dir().join(format!("av-jobs-tiler-tiles3d-test-{tag}-{}-{n}", std::process::id()));
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
/// documents byte for byte.
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
    p.insert("output".to_string(), "tiles3d".to_string());
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

// -- item 1: streaming and buffered paths agree, byte for byte, INCLUDING tileset.json --

#[test]
fn tiles3d_streaming_and_buffered_paths_produce_byte_identical_manifests_tileset_json_and_tiles() {
    const JOB_ID: &str = "job-tiles3d-stream-equality";

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

    assert_eq!(completion_streaming.manifest_sha256, completion_buffered.manifest_sha256, "a tiles3d tile set's identity must not depend on how it was written");
    assert_eq!(completion_streaming.outputs.len(), 1, "streaming: only the manifest is a recorded output");

    let buffered_tile_outputs = completion_buffered.outputs.iter().filter(|a| a.media_type == av_jobs::tiles3d::PNTS_TILE_MEDIA_TYPE).count();
    assert_eq!(buffered_tile_outputs, 10, "sanity: same fixture as the imagery/terrain tests, same tile count");
    let buffered_tileset_json_outputs = completion_buffered.outputs.iter().filter(|a| a.media_type == av_jobs::tiles3d::TILESET_JSON_MEDIA_TYPE).count();
    assert_eq!(buffered_tileset_json_outputs, 1, "exactly one tileset.json output");
    assert_eq!(completion_buffered.outputs.len(), buffered_tile_outputs + 1 /* tileset.json */ + 1 /* manifest */);

    let manifest_bytes_buffered = sink_buffered.get(&completion_buffered.manifest_sha256).unwrap();
    let manifest_bytes_streaming = sink_streaming.get(&completion_streaming.manifest_sha256).unwrap();
    assert_eq!(manifest_bytes_buffered, manifest_bytes_streaming);

    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes_streaming.as_slice()).unwrap();
    assert_eq!(manifest.kind, pb::TileSetKind::Tiles3d as i32);
    assert_eq!(manifest.tiles.len(), 10);
    assert!(!manifest.root_object_key.is_empty(), "TILES3D must set root_object_key");
    assert!(manifest.root_uri.is_empty(), "root_uri must stay empty -- heavy.proto's own doc, ordering constraint");

    // tileset.json itself, byte for byte, from both sinks. MemoryObjectSink::get is keyed by
    // sha256_hex, not the full object_key -- recover the hex suffix (the object_key's own
    // last path segment) the same way a real reader would.
    let root_sha256_hex = manifest.root_object_key.rsplit('/').next().unwrap();
    let tileset_json_buffered = sink_buffered.get(root_sha256_hex).expect("tileset.json present in the buffered sink");
    let tileset_json_streaming = sink_streaming.get(root_sha256_hex).expect("tileset.json present in the streaming sink");
    assert_eq!(tileset_json_buffered, tileset_json_streaming);
    assert_eq!(av_jobs::hash::hex_encode(&openssl::sha::sha256(&tileset_json_streaming)), root_sha256_hex);

    for tile in &manifest.tiles {
        assert_eq!(tile.media_type, av_jobs::tiles3d::PNTS_TILE_MEDIA_TYPE);
        let b1 = sink_buffered.get(&tile.sha256).unwrap_or_else(|| panic!("tile ({},{},{}) missing from buffered sink", tile.level, tile.x, tile.y));
        let b2 = sink_streaming.get(&tile.sha256).unwrap_or_else(|| panic!("tile ({},{},{}) missing from streaming sink", tile.level, tile.x, tile.y));
        assert_eq!(b1, b2);
        assert_eq!(av_jobs::hash::hex_encode(&openssl::sha::sha256(&b2)), tile.sha256);
        assert_eq!(&b1[0..4], b"pnts", "every content tile must be a real .pnts file (magic bytes)");
    }
}

// -- item 2: anchored golden -- independently re-derived by Python, not this crate ------

/// Reads the raw AVRASTER fixture (argv[1]), the encoded `TileSetManifest` (argv[2]), the
/// `tileset.json` bytes (argv[3]), the stored bytes of the `.pnts` content tile for
/// `(level=0, x=0, y=0)` (argv[4]), and the repo root (argv[5], to import `altavista.pb`) --
/// then:
///   1. decodes the manifest with the Python protobuf runtime and checks every field this
///      test cares about, including that `root_object_key`'s own hash matches a freshly
///      computed `hashlib.sha256` of the `tileset.json` bytes;
///   2. parses `tileset.json` (plain `json.loads`, no library), independently recomputes the
///      anchor's ECEF position and East/North/Up basis with a from-scratch WGS84
///      implementation (`math` only), and checks `root.transform` against it -- the same
///      cross-check `web/js/tiles3d_geo_check.mjs` performs against this codebase's *other*
///      3D Tiles fixture, done here in Python against THIS module's own output;
///   3. walks the full node tree and checks every `content.uri` is one of the manifest's own
///      `TileEntry.object_key`s, with none missing and none extra;
///   4. independently rebuilds tile (0,0,0)'s own `.pnts` bytes from nothing but the raw
///      raster and the recomputed anchor/basis (`struct`/`hashlib` only) and compares them
///      byte-for-byte against argv[4].
const PYTHON_VERIFY_SCRIPT: &str = r#"
import hashlib
import json
import math
import struct
import sys

raster_path, manifest_path, tileset_json_path, tile000_path, repo_root = sys.argv[1:6]
sys.path.insert(0, repo_root)
from altavista.pb.altavista.v1 import heavy_pb2  # noqa: E402

WGS84_A_M = 6378137.0
WGS84_B_M = 6356752.314245
WGS84_E2 = 1 - (WGS84_B_M * WGS84_B_M) / (WGS84_A_M * WGS84_A_M)


def geodetic_to_ecef(lon_deg, lat_deg, height_m):
    lon, lat = math.radians(lon_deg), math.radians(lat_deg)
    sin_lat, cos_lat = math.sin(lat), math.cos(lat)
    n = WGS84_A_M / math.sqrt(1 - WGS84_E2 * sin_lat * sin_lat)
    x = (n + height_m) * cos_lat * math.cos(lon)
    y = (n + height_m) * cos_lat * math.sin(lon)
    z = (n * (1 - WGS84_E2) + height_m) * sin_lat
    return x, y, z


def enu_basis(lon_deg, lat_deg):
    lon, lat = math.radians(lon_deg), math.radians(lat_deg)
    sin_lat, cos_lat = math.sin(lat), math.cos(lat)
    sin_lon, cos_lon = math.sin(lon), math.cos(lon)
    east = (-sin_lon, cos_lon, 0.0)
    north = (-sin_lat * cos_lon, -sin_lat * sin_lon, cos_lat)
    up = (cos_lat * cos_lon, cos_lat * sin_lon, sin_lat)
    return east, north, up


# -- 1. manifest, decoded by the Python protobuf runtime (not prost) ------------------
manifest_bytes = open(manifest_path, "rb").read()
m = heavy_pb2.TileSetManifest()
m.ParseFromString(manifest_bytes)

assert m.kind == heavy_pb2.TILE_SET_KIND_TILES3D, m.kind
assert m.scheme == "geographic-plate-carree-2x1"
assert m.min_level == 0 and m.max_level == 1 and m.tile_size == 16
expected_addrs = [(0, 0, 0), (0, 1, 0), (1, 0, 0), (1, 0, 1), (1, 1, 0), (1, 1, 1), (1, 2, 0), (1, 2, 1), (1, 3, 0), (1, 3, 1)]
got_addrs = [(t.level, t.x, t.y) for t in m.tiles]
assert got_addrs == expected_addrs, got_addrs
object_keys = set()
for t in m.tiles:
    assert t.media_type == "application/vnd.altavista.tiles3d-pnts+bin", t.media_type
    assert t.uri == ""
    object_keys.add(t.object_key)
assert m.root_uri == "", "root_uri must stay empty -- heavy.proto's own doc"
assert m.root_object_key != ""

tileset_json_bytes = open(tileset_json_path, "rb").read()
root_sha256_hex = m.root_object_key.rsplit("/", 1)[-1]
assert root_sha256_hex == hashlib.sha256(tileset_json_bytes).hexdigest(), "root_object_key's own hash must match tileset.json's real sha256"
assert m.root_object_key == f"tiles/{root_sha256_hex[0:2]}/{root_sha256_hex[2:4]}/{root_sha256_hex}"

reserialized = m.SerializeToString(deterministic=True)
assert reserialized == manifest_bytes, "re-serialising the Python-decoded manifest did not reproduce the original bytes"

# -- 2. tileset.json's root.transform, checked against an independent WGS84 recompute -
tileset = json.loads(tileset_json_bytes)
assert tileset["asset"]["version"] == "1.0"
geo = tileset["extras"]["geoReference"]
anchor_lon, anchor_lat, anchor_h = geo["lonDeg"], geo["latDeg"], geo["heightM"]
# This fixture's own anchor is the raster bounds centre (crate::tiles3d's own module doc):
# raster spans the whole globe (-180..180, -90..90), so the centre is (0, 0, 0).
assert (anchor_lon, anchor_lat, anchor_h) == (0.0, 0.0, 0.0), (anchor_lon, anchor_lat, anchor_h)

transform = tileset["root"]["transform"]
assert len(transform) == 16
ecef = geodetic_to_ecef(anchor_lon, anchor_lat, anchor_h)
east, north, up = enu_basis(anchor_lon, anchor_lat)


def close(a, b, tol):
    return abs(a - b) <= tol


assert close(transform[12], ecef[0], 1e-6) and close(transform[13], ecef[1], 1e-6) and close(transform[14], ecef[2], 1e-6)
assert transform[15] == 1.0
for col, expected in ((0, east), (4, north), (8, up)):
    got = (transform[col], transform[col + 1], transform[col + 2])
    assert all(close(g, e, 1e-9) for g, e in zip(got, expected)), (col, got, expected)


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def norm(a):
    return math.sqrt(dot(a, a))


assert close(norm(east), 1.0, 1e-12) and close(norm(north), 1.0, 1e-12) and close(norm(up), 1.0, 1e-12)
assert close(dot(east, north), 0.0, 1e-12) and close(dot(east, up), 0.0, 1e-12) and close(dot(north, up), 0.0, 1e-12)

root = tileset["root"]
assert "content" not in root, "the synthetic grouping root must carry no content"
assert root["refine"] == "REPLACE"
assert len(root["boundingVolume"]["region"]) == 6

# -- 3. every content.uri in the tree is exactly one of the manifest's own object_keys --
seen_uris = set()


def walk(node):
    if "content" in node:
        seen_uris.add(node["content"]["uri"])
        assert node["refine"] == "REPLACE"
        assert len(node["boundingVolume"]["region"]) == 6
    for child in node.get("children", []):
        walk(child)


for child in root["children"]:
    walk(child)
assert seen_uris == object_keys, (seen_uris ^ object_keys)

# -- 4. tile (0,0,0)'s own .pnts bytes, rebuilt from raw formulas only ----------------
raster_bytes = open(raster_path, "rb").read()
MAGIC, VERSION, WIDTH, HEIGHT, CHANNELS = struct.unpack_from("<8sIII I", raster_bytes, 0)
assert MAGIC == b"AVRASTER"
rwest, rsouth, reast, rnorth = struct.unpack_from("<dddd", raster_bytes, 24)
pixels = raster_bytes[56:]


def pixel_rgb(col, row):
    idx = (row * WIDTH + col) * 3
    return pixels[idx], pixels[idx + 1], pixels[idx + 2]


TW, TS, TE, TN = -180.0, -90.0, 0.0, 90.0  # tile (level=0,x=0,y=0)'s own bounds
TILE_SIZE = 16
anchor_ecef = geodetic_to_ecef(anchor_lon, anchor_lat, anchor_h)

positions = []
colors = []
for py in range(TILE_SIZE):
    for px in range(TILE_SIZE):
        lon = TW + (px + 0.5) / TILE_SIZE * (TE - TW)
        lat = TN - (py + 0.5) / TILE_SIZE * (TN - TS)
        col = min(max(int((lon - rwest) / (reast - rwest) * WIDTH), 0), WIDTH - 1)
        row = min(max(int((rnorth - lat) / (rnorth - rsouth) * HEIGHT), 0), HEIGHT - 1)
        r, g, b = pixel_rgb(col, row)
        colors.append((r, g, b))
        p_ecef = geodetic_to_ecef(lon, lat, 0.0)
        d = (p_ecef[0] - anchor_ecef[0], p_ecef[1] - anchor_ecef[1], p_ecef[2] - anchor_ecef[2])
        lx, ly, lz = dot(d, east), dot(d, north), dot(d, up)
        positions.append((lx, ly, lz))

points_length = len(positions)
ft_json = json.dumps({
    "POINTS_LENGTH": points_length,
    "POSITION": {"byteOffset": 0},
    "RGB": {"byteOffset": points_length * 12},
}, separators=(",", ":")).encode("utf-8")
while len(ft_json) % 4 != 0:
    ft_json += b" "
ft_bin = b"".join(struct.pack("<fff", *p) for p in positions) + b"".join(struct.pack("<BBB", *c) for c in colors)
byte_length = 28 + len(ft_json) + len(ft_bin)
header = b"pnts" + struct.pack("<IIIIII", 1, byte_length, len(ft_json), len(ft_bin), 0, 0)
expected_tile_bytes = header + ft_json + ft_bin

actual_tile_bytes = open(tile000_path, "rb").read()
assert actual_tile_bytes == expected_tile_bytes, (
    f"tile (0,0,0) mismatch: expected {len(expected_tile_bytes)} bytes "
    f"(sha256 {hashlib.sha256(expected_tile_bytes).hexdigest()}), got {len(actual_tile_bytes)} bytes "
    f"(sha256 {hashlib.sha256(actual_tile_bytes).hexdigest()})"
)

print(f"OK {hashlib.sha256(actual_tile_bytes).hexdigest()} root={root_sha256_hex}")
"#;

#[test]
fn tiles3d_output_is_anchored_against_an_independent_python_derivation() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().and_then(|p| p.parent()).expect("crates/av-jobs has two ancestors: crates/, then the repo root");
    let python_path = repo_root.join(".venv/bin/python");
    if !python_path.exists() {
        eprintln!("SKIPPED: tiles3d_output_is_anchored_against_an_independent_python_derivation -- {python_path:?} not found on this host");
        return;
    }

    let dir = TempDir::new("anchor");
    let clock = TestClock::new(9_000);
    let (runner, sink) = fresh_runner_with_shared_sink(dir.path(), &clock, fixture_raster_bytes());
    let spec = tiler_spec("job-tiles3d-anchor");
    runner.queue().submit(&spec).unwrap();
    let completion = runner.run_one(&spec).unwrap();
    assert!(completion.ok, "{completion:?}");

    let manifest_bytes = sink.get(&completion.manifest_sha256).unwrap();
    let manifest: pb::TileSetManifest = prost::Message::decode(manifest_bytes.as_slice()).unwrap();
    let root_sha256_hex = manifest.root_object_key.rsplit('/').next().unwrap().to_string();
    let tileset_json_bytes = sink.get(&root_sha256_hex).expect("tileset.json present in the sink");
    let tile000 = manifest.tiles.iter().find(|t| (t.level, t.x, t.y) == (0, 0, 0)).expect("tile (0,0,0) present");
    let tile000_bytes = sink.get(&tile000.sha256).unwrap();

    let work_dir = dir.path();
    let raster_path = work_dir.join("raster.bin");
    let manifest_path = work_dir.join("manifest.pb");
    let tileset_json_path = work_dir.join("tileset.json");
    let tile000_path = work_dir.join("tile000.pnts");
    std::fs::write(&raster_path, fixture_raster_bytes()).unwrap();
    std::fs::write(&manifest_path, &manifest_bytes).unwrap();
    std::fs::write(&tileset_json_path, &tileset_json_bytes).unwrap();
    std::fs::write(&tile000_path, &tile000_bytes).unwrap();
    let script_path = work_dir.join("verify.py");
    std::fs::write(&script_path, PYTHON_VERIFY_SCRIPT).unwrap();

    let output = std::process::Command::new(&python_path)
        .arg(&script_path)
        .arg(&raster_path)
        .arg(&manifest_path)
        .arg(&tileset_json_path)
        .arg(&tile000_path)
        .arg(repo_root)
        .output()
        .expect("failed to spawn python");
    assert!(output.status.success(), "python anchoring script failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().starts_with("OK "), "unexpected stdout: {stdout}");
}

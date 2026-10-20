//! Question 256: a store failure is not a 404. Every test here runs a genuine container of the S3
//! server `services/store/IMAGE_DIGEST.md` records and drives this crate's real, store-backed
//! [`av_tiles::source::StoreObjectSource`] (over [`av_store::StoreClient`]) and the real
//! [`av_tiles::core::handle`] against it -- nothing mocks the S3 wire protocol.
//!
//! # The case
//!
//! The store ANSWERS, but an object's `x-amz-meta-av-*` headers are missing (here: an object put
//! by a raw SigV4-signed `PUT` that carries none, so the server has nothing to return). The server
//! is up and the key exists, so this is not "absent"; `av_store::StoreClient::head` cannot rebuild
//! the object's label and refuses with `StoreError::MetadataMissing`. The first trial server
//! (SeaweedFS 4.48) produced exactly this for an object whose `av-provenance` header was empty,
//! and the gateway used to answer `404` and count `tiles_manifest_not_found`. The ruling: whatever
//! the server, a backend error is a `502` counted as `tiles_store_backend_error`, never a `404`.
//!
//! # Record-driven, gate/lock/prune/label discipline
//!
//! Mirrors `crates/av-store/tests/s3_store.rs` (its module doc has the full reasoning): the server
//! is started from `services/store/IMAGE_DIGEST.md` through
//! [`av_lockstep::docker::parse_recorded_launch`] and
//! [`av_lockstep::docker::ManagedContainer::run_local`] (never `docker pull`); each test calls
//! the digest gate first (a visible `SKIPPED` line on a gate failure, never a silent pass), takes
//! the docker-test lock inside its own body, prunes stale labelled containers, labels its own
//! container, binds no volume, and relies on `ManagedContainer`'s `Drop` for teardown.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use av_cdm::pb::{Label, Provenance, TileEntry, TileSetKind, TileSetManifest};
use av_command::clock::SystemClock;
use av_command::counters::Counters;
use av_command::oidc::IssuerConfig;
use av_command::test_support::{valid_claims, TestIssuer};
use av_label::{ClearanceLadder, GroupClearanceMap};
use av_lockstep::docker::{
    announce_gate_skip, lock_docker_tests, parse_recorded_launch, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, ManagedContainer, RecordedLaunch,
};
use av_store::sigv4;
use av_store::{object_key, StoreClient, StoreConfig};
use av_tiles::config::TilesConfig;
use av_tiles::core::{handle, RequestHeaders};
use av_tiles::source::{ObjectSource, ObjectSourceError, StoreObjectSource};
use bytes::Bytes;
use http::Uri;
use openssl::sha::sha256;
use prost::Message as _;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const REGION: &str = "us-east-1";
const BUCKET: &str = "av-tiles-integration-test";
const KEY_PREFIX: &str = "imagery-it";

const ISSUER: &str = "https://sso.test.example/";
const AUDIENCE: &str = "av-tiles";
/// The token's own validity clock (injected into `handle`, as the unit tests do); unrelated to the
/// real clock the store adapter signs its S3 requests with.
const NOW_UNIX_S: i64 = 1_760_000_000;
const TTL_S: i64 = 3_600;

// -----------------------------------------------------------------------------------------
// Record, gate, container -- mirrored from crates/av-store/tests/s3_store.rs.
// -----------------------------------------------------------------------------------------

fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/store/IMAGE_DIGEST.md")
}

fn load_launch() -> RecordedLaunch {
    let path = image_digest_md_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    parse_recorded_launch(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

macro_rules! gate_or_skip {
    ($test_name:expr) => {{
        let launch = load_launch();
        if let Err(reason) = recorded_digest_gate(&launch.image_ref, &launch.recorded_id) {
            let line = announce_gate_skip($test_name, &reason);
            assert!(line.starts_with("SKIPPED "), "{line:?}");
            return;
        }
        launch
    }};
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock before 1970").as_secs() as i64
}

fn docker_logs(container_id: &str) -> String {
    match Command::new("docker").args(["logs", container_id]).output() {
        Ok(o) => format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        Err(e) => format!("(could not run docker logs: {e})"),
    }
}

fn probe_health_once(host_port: u16, health_path: &str) -> Result<(), String> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", host_port)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let request = format!("GET {health_path} HTTP/1.1\r\nHost: 127.0.0.1:{host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("").to_string();
    if status_line.contains(" 200") {
        Ok(())
    } else {
        Err(format!("status line: {status_line:?}"))
    }
}

fn wait_for_health(host_port: u16, health_path: &str, container_id: &str, test_name: &str) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_err;
    loop {
        match probe_health_once(host_port, health_path) {
            Ok(()) => return,
            Err(e) => last_err = e,
        }
        if Instant::now() > deadline {
            let logs = docker_logs(container_id);
            panic!("{test_name}: the S3 server at 127.0.0.1:{host_port} (container {container_id}) did not answer GET {health_path} with 200 within {READY_TIMEOUT:?}; last error: {last_err}; docker logs:\n{logs}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn labels_from_test_label_args(run_id: &str) -> BTreeMap<String, String> {
    let args = test_label_args(run_id);
    let mut map = BTreeMap::new();
    for kv in args.iter().skip(1).step_by(2) {
        let (key, value) = kv.split_once('=').unwrap_or_else(|| panic!("test_label_args produced a non-KEY=VALUE entry: {kv:?}"));
        map.insert(key.to_string(), value.to_string());
    }
    map
}

fn store_config(endpoint: &Uri, access_key: &str, secret_key: &str) -> StoreConfig {
    StoreConfig {
        endpoint: endpoint.clone(),
        region: REGION.to_string(),
        access_key_id: access_key.to_string(),
        secret_access_key: secret_key.to_string(),
        bucket: BUCKET.to_string(),
        force_path_style: true,
        ca_file: None,
        key_prefix: KEY_PREFIX.to_string(),
    }
}

/// One S3-server container, its endpoint and credentials, and a [`StoreClient`] on it. The
/// container is kept for the fixture's lifetime so `Drop` removes it, panic or not.
struct S3Fixture {
    _container: ManagedContainer,
    endpoint: Uri,
    access_key: String,
    secret_key: String,
    client: Arc<StoreClient>,
}

fn start_fixture(launch: &RecordedLaunch, test_name: &str) -> S3Fixture {
    let run_id = test_run_id();
    let labels = labels_from_test_label_args(&run_id);
    let sanitized_run_id: String = run_id.chars().filter(char::is_ascii_alphanumeric).collect();
    let access_key = format!("avtest{sanitized_run_id}");
    let secret_key = format!("avtestsecret{sanitized_run_id}");
    let env = launch.render_credential_env(&access_key, &secret_key).unwrap_or_else(|e| panic!("{test_name}: rendering the recorded credential environment: {e}"));

    let image_ref = &launch.image_ref;
    let (container, host_port) = ManagedContainer::run_local(image_ref, &launch.args, launch.container_port, &BTreeMap::new(), &env, &BTreeMap::new(), &labels)
        .unwrap_or_else(|e| panic!("{test_name}: ManagedContainer::run_local({image_ref:?}) failed: {e}"));
    wait_for_health(host_port, &launch.health_path, &container.container_id, test_name);

    let endpoint: Uri = format!("http://127.0.0.1:{host_port}").parse().unwrap_or_else(|e| panic!("{test_name}: building the endpoint URI: {e}"));
    let client = Arc::new(StoreClient::new(store_config(&endpoint, &access_key, &secret_key)).unwrap_or_else(|e| panic!("{test_name}: StoreClient::new: {e}")));
    S3Fixture { _container: container, endpoint, access_key, secret_key, client }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A raw SigV4-signed `PUT` of `body` to `/{BUCKET}/{key}` carrying **no** `x-amz-meta-*` header,
/// built from [`av_store::sigv4`]'s public functions over a bare `std::net::TcpStream` (HTTP/1.1,
/// `Connection: close`) -- `StoreClient::put` always writes the `av-*` metadata, so this is how a
/// test makes an object the server stores without it. Returns the status code.
fn raw_signed_put_without_metadata(endpoint: &Uri, key: &str, access_key: &str, secret_key: &str, body: &[u8]) -> u16 {
    let payload_hash = hex(&sha256(body));
    let host = endpoint.authority().expect("endpoint has an authority").as_str().to_string();
    let path = format!("/{BUCKET}/{key}");
    let (amz_date, date_stamp) = sigv4::amz_date_from_unix_seconds(now());

    let headers = [("host", host.as_str()), ("x-amz-date", amz_date.as_str()), ("x-amz-content-sha256", payload_hash.as_str())];
    let creq = sigv4::canonical_request("PUT", &path, &[], &headers, &payload_hash);
    let sts = sigv4::string_to_sign(&amz_date, &date_stamp, REGION, "s3", &creq.text);
    let signing_key = sigv4::signing_key(secret_key, &date_stamp, REGION, "s3").expect("signing_key");
    let signature = sigv4::sign(&signing_key, &sts).expect("sign");
    let authorization = sigv4::authorization_header(access_key, &date_stamp, REGION, "s3", &creq.signed_headers, &signature);

    let head = format!(
        "PUT {path} HTTP/1.1\r\nHost: {host}\r\nx-amz-date: {amz_date}\r\nx-amz-content-sha256: {payload_hash}\r\nauthorization: {authorization}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect(host.as_str()).unwrap_or_else(|e| panic!("raw PUT: connect {host}: {e}"));
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.write_all(head.as_bytes()).expect("raw PUT: write head");
    stream.write_all(body).expect("raw PUT: write body");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("raw PUT: read response");
    let text = String::from_utf8_lossy(&buf).to_string();
    let status_line = text.lines().next().unwrap_or("").to_string();
    status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("raw PUT: unparseable status line {status_line:?}"))
}

// -----------------------------------------------------------------------------------------
// The gateway side: a config, a token, a manifest.
// -----------------------------------------------------------------------------------------

fn tiles_config(issuer: &TestIssuer) -> TilesConfig {
    let issuer_config = Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
    let ladder = Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]));
    let mut m = BTreeMap::new();
    m.insert("operators".to_string(), "UNCLASSIFIED".to_string());
    TilesConfig::new(KEY_PREFIX, issuer_config, ladder, Arc::new(GroupClearanceMap::new(m)))
}

fn mint(issuer: &TestIssuer) -> String {
    let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", NOW_UNIX_S, TTL_S);
    claims["groups"] = serde_json::json!(["operators"]);
    issuer.mint(&claims)
}

fn now_tai_ns() -> i64 {
    av_cdm::time::Tai::from_utc_nanos(NOW_UNIX_S * 1_000_000_000).as_nanos()
}

fn tile_entry(level: u32, x: u32, y: u32, tile_bytes: &[u8]) -> TileEntry {
    let sha = hex(&sha256(tile_bytes));
    TileEntry { level, x, y, sha256: sha.clone(), size_bytes: tile_bytes.len() as u64, uri: String::new(), media_type: "image/png".to_string(), object_key: object_key(KEY_PREFIX, &sha).unwrap() }
}

fn manifest_of(tiles: Vec<TileEntry>) -> TileSetManifest {
    TileSetManifest {
        kind: TileSetKind::Imagery as i32,
        scheme: "geographic-plate-carree-2x1".to_string(),
        min_level: 0,
        max_level: 1,
        tile_size: 256,
        bounds: None,
        tiles,
        source_sha256: vec![],
        parameters: Default::default(),
        root_uri: String::new(),
        job_id: "job-it".to_string(),
        object_key_prefix: KEY_PREFIX.to_string(),
        root_object_key: String::new(),
    }
}

fn source_for(fixture: &S3Fixture) -> StoreObjectSource {
    let ladder = Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]));
    StoreObjectSource::new(fixture.client.clone(), ladder, Arc::new(SystemClock))
}

fn get(config: &TilesConfig, source: &StoreObjectSource, counters: &Counters, token: &str, path: &str) -> av_tiles::core::TileResponse {
    handle(config, source, counters, path, RequestHeaders::auth_only(Some(&format!("Bearer {token}"))), now_tai_ns())
}

// -----------------------------------------------------------------------------------------
// The tests. `StoreObjectSource::get` bridges sync to async with `block_in_place`, which needs a
// multi-thread runtime -- the flavor the shipped binary runs.
// -----------------------------------------------------------------------------------------

/// 1. The manifest object exists but carries no `av-*` metadata: the real `StoreObjectSource`
///    answers `Backend` (not `NotFound`), and `handle` answers `502` counted as
///    `tiles_store_backend_error`, with `tiles_manifest_not_found` still 0. Controls: a manifest
///    hash that was never stored is a real `404` / `tiles_manifest_not_found` / backend count 0
///    (the server's own 404 is `NotFound`, and still maps to the not-found refusal).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_object_missing_its_av_metadata_is_502_store_backend_error_not_404() {
    const TEST_NAME: &str = "a_manifest_object_missing_its_av_metadata_is_502_store_backend_error_not_404";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    let tile_bytes = vec![1u8, 2, 3, 4, 5];
    let manifest_bytes = manifest_of(vec![tile_entry(1, 0, 0, &tile_bytes)]).encode_to_vec();
    let manifest_sha256 = hex(&sha256(&manifest_bytes));
    let manifest_key = object_key(KEY_PREFIX, &manifest_sha256).unwrap();

    let status = raw_signed_put_without_metadata(&fixture.endpoint, &manifest_key, &fixture.access_key, &fixture.secret_key, &manifest_bytes);
    assert!((200..300).contains(&status), "{TEST_NAME}: the raw metadata-less PUT must itself succeed, got {status}");

    let source = source_for(&fixture);

    // The ObjectSource half, against the real server.
    let err = source.get(&manifest_key).expect_err("an object with no av-* metadata cannot yield a StoredObject");
    println!("{TEST_NAME}: StoreObjectSource::get on the metadata-less manifest -> {err}");
    assert!(matches!(&err, ObjectSourceError::Backend { key, .. } if key == &manifest_key), "{TEST_NAME}: expected ObjectSourceError::Backend, got {err:?}");

    // The handler half, end to end over the same real source.
    let issuer = TestIssuer::new();
    let config = tiles_config(&issuer);
    let token = mint(&issuer);
    let counters = Counters::new();
    let response = get(&config, &source, &counters, &token, &format!("/v1/tilesets/{manifest_sha256}/manifest"));
    println!(
        "{TEST_NAME}: GET manifest -> {} (tiles_store_backend_error={}, tiles_manifest_not_found={})",
        response.status,
        counters.get("tiles_store_backend_error"),
        counters.get("tiles_manifest_not_found")
    );
    assert_eq!(response.status, 502, "{TEST_NAME}: a store backend error must not be a 404");
    assert!(response.body.is_empty());
    assert_eq!(counters.get("tiles_store_backend_error"), 1);
    assert_eq!(counters.get("tiles_manifest_not_found"), 0);

    // Control: a hash nothing was ever stored under is a genuine 404.
    let never_stored = "b".repeat(64);
    let control = get(&config, &source, &counters, &token, &format!("/v1/tilesets/{never_stored}/manifest"));
    assert_eq!(control.status, 404, "{TEST_NAME}: an absent manifest is still a 404");
    assert_eq!(counters.get("tiles_manifest_not_found"), 1);
    assert_eq!(counters.get("tiles_store_backend_error"), 1, "{TEST_NAME}: the absent key must not count as a backend error");
}

/// 2. The manifest is well formed (put through `StoreClient`, labels intact) and lists two tiles:
///    one put properly, one whose object carries no `av-*` metadata. The good tile is a `200`
///    with its exact bytes (the control: the path works); the metadata-less tile is `502` /
///    `tiles_store_backend_error`, with `tiles_tile_object_not_found` and
///    `tiles_manifest_not_found` still 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tile_object_missing_its_av_metadata_is_502_store_backend_error_not_404() {
    const TEST_NAME: &str = "a_tile_object_missing_its_av_metadata_is_502_store_backend_error_not_404";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    let label = Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] };
    let good_bytes = vec![10u8, 20, 30, 40];
    let bare_bytes = vec![50u8, 60, 70, 80, 90];
    let good = tile_entry(1, 0, 0, &good_bytes);
    let bare = tile_entry(1, 1, 0, &bare_bytes);

    fixture.client.put(Bytes::from(good_bytes.clone()), "image/png", label.clone(), Provenance::default(), now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: put good tile: {e}"));
    let status = raw_signed_put_without_metadata(&fixture.endpoint, &bare.object_key, &fixture.access_key, &fixture.secret_key, &bare_bytes);
    assert!((200..300).contains(&status), "{TEST_NAME}: the raw metadata-less PUT must itself succeed, got {status}");

    let manifest_bytes = manifest_of(vec![good.clone(), bare.clone()]).encode_to_vec();
    let manifest_asset = fixture
        .client
        .put(Bytes::from(manifest_bytes), "application/vnd.altavista.tileset-manifest+pb", label, Provenance::default(), now())
        .await
        .unwrap_or_else(|e| panic!("{TEST_NAME}: put manifest: {e}"));
    let manifest_sha256 = manifest_asset.sha256;

    let source = source_for(&fixture);
    let err = source.get(&bare.object_key).expect_err("an object with no av-* metadata cannot yield a StoredObject");
    println!("{TEST_NAME}: StoreObjectSource::get on the metadata-less tile -> {err}");
    assert!(matches!(&err, ObjectSourceError::Backend { key, .. } if key == &bare.object_key), "{TEST_NAME}: expected ObjectSourceError::Backend, got {err:?}");

    let issuer = TestIssuer::new();
    let config = tiles_config(&issuer);
    let token = mint(&issuer);
    let counters = Counters::new();

    let ok = get(&config, &source, &counters, &token, &format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", good.level, good.x, good.y));
    assert_eq!(ok.status, 200, "{TEST_NAME}: control: a tile put through StoreClient is served");
    assert_eq!(ok.body, good_bytes);

    let response = get(&config, &source, &counters, &token, &format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", bare.level, bare.x, bare.y));
    println!(
        "{TEST_NAME}: GET metadata-less tile -> {} (tiles_store_backend_error={}, tiles_tile_object_not_found={})",
        response.status,
        counters.get("tiles_store_backend_error"),
        counters.get("tiles_tile_object_not_found")
    );
    assert_eq!(response.status, 502, "{TEST_NAME}: a store backend error on a tile object must not be a 404");
    assert!(response.body.is_empty(), "{TEST_NAME}: a refusal must never carry tile bytes");
    assert_eq!(counters.get("tiles_store_backend_error"), 1);
    assert_eq!(counters.get("tiles_tile_object_not_found"), 0);
    assert_eq!(counters.get("tiles_manifest_not_found"), 0);
}

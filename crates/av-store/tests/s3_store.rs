//! H1b (`docs/heavy-plan.md`): the real S3-server integration proof for `av-store`. Every test
//! here runs a genuine container of the S3 server `services/store/IMAGE_DIGEST.md` records
//! (question 256: an Apache-2.0 S3 server chosen by trial, replacing MinIO) and drives
//! [`av_store::client::StoreClient`] against it over real HTTP -- nothing in this file mocks or
//! stubs the S3 wire protocol. `crates/av-store/src/**`'s own `#[cfg(test)]` unit tests already
//! cover every pure function (SigV4 vectors, the content-addressed key layout, the clearance
//! ladder, metadata encode/decode); this file exists specifically to prove the parts a unit test
//! cannot: that a real S3-API server accepts this crate's signed requests, stores and returns the
//! exact bytes and metadata this crate sends, and refuses exactly the things `StoreError`'s
//! variants promise (a wrong secret, an unknown key id, a skewed clock, a missing object).
//!
//! # Record-driven and server-agnostic
//!
//! Nothing here names a server. `services/store/IMAGE_DIGEST.md` carries the image reference, its
//! recorded local id, and the launch record (container port, health path, credential environment
//! templates, server arguments); [`av_lockstep::docker::parse_recorded_launch`] reads it at test
//! time (path from `CARGO_MANIFEST_DIR`) and this file starts exactly what it says. The contract
//! a server must meet to pass: path-style S3 over plain HTTP, SigV4 with the real payload hash,
//! `CreateBucket` (idempotent), `PutObject` with `x-amz-meta-*`, `GetObject` and `HeadObject`
//! (metadata and `Content-Length` returned), a 403 for a bad signature, an unknown key id or a
//! clock skewed by an hour, a 404 for an unknown key, and an unauthenticated 200 on the health
//! path. A server that cannot meet one of these is reported as a gap, never papered over here.
//!
//! # Never pulls -- question 154, question 212(a)
//!
//! The image was pulled once, at setup, and its digest recorded. A test in this workspace must
//! never run `docker pull`: [`gate_or_skip`] calls [`av_lockstep::docker::recorded_digest_gate`],
//! which only ever runs `docker image inspect`, and every container starts through
//! [`av_lockstep::docker::ManagedContainer::run_local`], never `pull_and_run`.
//!
//! # Gate/skip/label/lock discipline -- mirrored from `crates/av-lockstep`'s own Docker tests
//!
//! Every `#[tokio::test]` below:
//! 1. Calls [`gate_or_skip`] first. A gate failure calls [`av_lockstep::docker::announce_gate_skip`]
//!    (a raw stderr write, visible in a plain `cargo test` run) and returns; it never asserts a
//!    pass and never runs the real assertions on a skip.
//! 2. Takes [`av_lockstep::docker::lock_docker_tests`] **inside its own body**, so a skipping
//!    test (no Docker, or a digest mismatch) serialises nothing.
//! 3. Calls [`av_lockstep::docker::prune_stale_test_resources`] before creating anything, so a
//!    previous run's `SIGKILL`-orphaned container (whose `Drop` guard never ran) does not
//!    accumulate (question 156).
//! 4. Starts its container labelled with [`av_lockstep::docker::test_label_args`], no bind mount
//!    (Colima mounts only `$HOME`, so a bind mount from elsewhere would silently be empty); the
//!    container's own filesystem holds the data and dies with it.
//! 5. Relies on [`av_lockstep::docker::ManagedContainer`]'s `Drop` for teardown, even on a panic.
//!    `stop_and_remove` uses `docker rm -v`, so an image that declares a `VOLUME` leaves no
//!    anonymous volume behind (question 249).
//!
//! # Bucket/prefix/credentials
//!
//! Every test shares one bucket name ([`BUCKET`]) and key prefix ([`KEY_PREFIX`]) -- harmless,
//! since [`StoreClient::ensure_bucket`] is idempotent and every object key is content-addressed --
//! but each test starts its OWN container with per-run generated credentials substituted into the
//! record's credential environment, so no two tests share credentials or a server process.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use av_cdm::pb::{AssetRef, Label, Provenance};
use av_lockstep::docker::{
    announce_gate_skip, lock_docker_tests, parse_recorded_launch, prune_stale_test_resources, recorded_digest_gate, test_label_args, test_run_id, ManagedContainer, RecordedLaunch,
};
use av_store::sigv4;
use av_store::{object_key, ClearanceLadder, StoreClient, StoreConfig, StoreError};
use bytes::Bytes;
use http::{Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as LegacyClient;
use hyper_util::rt::TokioExecutor;
use openssl::sha::sha256;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const REGION: &str = "us-east-1";
const BUCKET: &str = "av-store-integration-test";
const KEY_PREFIX: &str = "imagery-it";

// -----------------------------------------------------------------------------------------
// services/store/IMAGE_DIGEST.md -- the recorded image and launch record's one home.
// -----------------------------------------------------------------------------------------

/// `services/store/IMAGE_DIGEST.md`'s path, derived from `CARGO_MANIFEST_DIR` rather than any
/// absolute path baked into this file.
fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/store/IMAGE_DIGEST.md")
}

/// Reads and parses the record at test time; a malformed record panics naming the file and field.
fn load_launch() -> RecordedLaunch {
    let path = image_digest_md_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    parse_recorded_launch(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// Every test's own first line (question 212(a)): load the record, then require the docker daemon
/// up AND the local image's id equal to the recorded one. On a gate failure, announce the visible
/// skip (never a silent pass -- question 194) and return from the calling test function;
/// otherwise evaluates to the [`RecordedLaunch`].
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

// -----------------------------------------------------------------------------------------
// Fixture: one S3-server container + one StoreClient pointed at it.
// -----------------------------------------------------------------------------------------

/// Real Unix-seconds -- the ONE place in this file that reads a clock. `av_store` itself never
/// does (`src/client.rs`'s module doc: "the clock is a parameter, never a read"); this test file
/// is the caller injecting a real reading, because an S3 server (like real S3) rejects a SigV4
/// request whose `x-amz-date` is too far from its own wall clock.
fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock before 1970").as_secs() as i64
}

/// `docker <args>`, panicking with the real stderr on failure -- this test file's own fixture
/// setup, not the code under test.
fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// One readiness probe: connect, send a hand-written `GET <health_path>` HTTP/1.1 request (the
/// record's unauthenticated health path), read the response, and judge the status line -- a real,
/// bare TCP connect, not `StoreClient` (this poll runs BEFORE the server is trusted enough to sign
/// anything against it).
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

/// Retries [`probe_health_once`] within [`READY_TIMEOUT`] -- polling a container for readiness with
/// a bounded retry loop is the established pattern here. Panics with the last observed error AND
/// the container's own `docker logs` if the budget runs out.
fn wait_for_health(host_port: u16, health_path: &str, container_id: &str, test_name: &str) {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_err;
    loop {
        match probe_health_once(host_port, health_path) {
            Ok(()) => return,
            Err(e) => last_err = e,
        }
        if Instant::now() > deadline {
            let logs = docker(&["logs", container_id]);
            panic!("{test_name}: the S3 server at 127.0.0.1:{host_port} (container {container_id}) did not answer GET {health_path} with 200 within {READY_TIMEOUT:?}; last error: {last_err}; docker logs:\n{logs}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// One S3-server container plus one [`StoreClient`] configured to talk to it. `container` is kept
/// alive for the fixture's whole lifetime so its `Drop` removes the container when the fixture
/// (and therefore the test) ends, panic or not.
struct S3Fixture {
    container: ManagedContainer,
    container_id: String,
    endpoint: Uri,
    access_key: String,
    secret_key: String,
    client: StoreClient,
}

/// Converts [`test_label_args`]'s `["--label", "KEY=VALUE", ...]` CLI-argv shape into the
/// `BTreeMap` [`ManagedContainer::run_local`]'s `extra_labels` parameter wants, so the exact label
/// set every container here carries is defined in the one place [`prune_stale_test_resources`]
/// also reads it from.
fn labels_from_test_label_args(run_id: &str) -> BTreeMap<String, String> {
    let args = test_label_args(run_id);
    let mut map = BTreeMap::new();
    for kv in args.iter().skip(1).step_by(2) {
        let (key, value) = kv.split_once('=').unwrap_or_else(|| panic!("test_label_args produced a non-KEY=VALUE entry: {kv:?}"));
        map.insert(key.to_string(), value.to_string());
    }
    map
}

/// A [`StoreConfig`] for the fixture's bucket at `endpoint` with the given credentials.
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

/// Starts a fresh, labelled, loopback-only container from the record (via
/// [`ManagedContainer::run_local`] -- no `docker pull`) with per-run generated credentials
/// substituted into the record's credential environment, waits for its health path to answer 200,
/// and builds a [`StoreClient`] against it.
fn start_fixture(launch: &RecordedLaunch, test_name: &str) -> S3Fixture {
    let run_id = test_run_id();
    let labels = labels_from_test_label_args(&run_id);

    // Alphanumeric-only, comfortably over every S3 server's minimum key lengths: this fixture's
    // focus is av-store, not a server's credential parsing.
    let sanitized_run_id: String = run_id.chars().filter(char::is_ascii_alphanumeric).collect();
    let access_key = format!("avtest{sanitized_run_id}");
    let secret_key = format!("avtestsecret{sanitized_run_id}");
    let env = launch.render_credential_env(&access_key, &secret_key).unwrap_or_else(|e| panic!("{test_name}: rendering the recorded credential environment: {e}"));

    let image_ref = &launch.image_ref;
    let (container, host_port) = ManagedContainer::run_local(image_ref, &launch.args, launch.container_port, &BTreeMap::new(), &env, &BTreeMap::new(), &labels)
        .unwrap_or_else(|e| panic!("{test_name}: ManagedContainer::run_local({image_ref:?}) failed: {e}"));
    let container_id = container.container_id.clone();

    wait_for_health(host_port, &launch.health_path, &container_id, test_name);

    let endpoint: Uri = format!("http://127.0.0.1:{host_port}").parse().unwrap_or_else(|e| panic!("{test_name}: building the endpoint URI: {e}"));
    let client = StoreClient::new(store_config(&endpoint, &access_key, &secret_key)).unwrap_or_else(|e| panic!("{test_name}: StoreClient::new: {e}"));

    S3Fixture { container, container_id, endpoint, access_key, secret_key, client }
}

/// A multi-kilobyte, deterministic (not random -- ADR-004's determinism preference) payload for the
/// round-trip test: 6144 bytes, built from a repeating pattern that is NOT all-zero (a corruption
/// that happened to also produce all-zero bytes would be a weak test of a byte-identical round trip).
fn multi_kb_payload() -> Vec<u8> {
    (0..6144u32).map(|i| (i % 251) as u8).collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A second, raw SigV4-signed request built directly from [`av_store::sigv4`]'s own public
/// functions (never through [`StoreClient`], which has no method that lets a caller send bytes
/// that do not match their own hash) -- the corruption test's way of corrupting an object SERVER
/// SIDE: a genuinely different signed PUT to the same key, not a locally tampered buffer. Uses a
/// bare `hyper_util` client over plain HTTP, so this file needs no new dependency.
async fn raw_signed_put(endpoint: &Uri, bucket: &str, key: &str, access_key: &str, secret_key: &str, body: &[u8], now_unix_secs: i64) -> (StatusCode, Bytes) {
    let payload_hash = hex(&sha256(body));
    let host = endpoint.authority().expect("endpoint has an authority").as_str().to_string();
    let path = format!("/{bucket}/{key}");
    let (amz_date, date_stamp) = sigv4::amz_date_from_unix_seconds(now_unix_secs);

    let headers = [("host", host.as_str()), ("x-amz-date", amz_date.as_str()), ("x-amz-content-sha256", payload_hash.as_str())];
    let creq = sigv4::canonical_request("PUT", &path, &[], &headers, &payload_hash);
    let sts = sigv4::string_to_sign(&amz_date, &date_stamp, REGION, "s3", &creq.text);
    let signing_key = sigv4::signing_key(secret_key, &date_stamp, REGION, "s3").expect("signing_key");
    let signature = sigv4::sign(&signing_key, &sts).expect("sign");
    let authorization = sigv4::authorization_header(access_key, &date_stamp, REGION, "s3", &creq.signed_headers, &signature);

    let uri: Uri = format!("http://{host}{path}").parse().expect("well-formed uri: hex key + ascii prefix need no percent-encoding");
    let request = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header("host", &host)
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", &payload_hash)
        .header("authorization", authorization)
        .body(Full::new(Bytes::copy_from_slice(body)))
        .expect("well-formed request");

    let client: LegacyClient<HttpConnector, Full<Bytes>> = LegacyClient::builder(TokioExecutor::new()).build(HttpConnector::new());
    let response = client.request(request).await.unwrap_or_else(|e| panic!("raw_signed_put: sending the request: {e}"));
    let status = response.status();
    let body_bytes = response.into_body().collect().await.unwrap_or_else(|e| panic!("raw_signed_put: reading the response body: {e}")).to_bytes();
    (status, body_bytes)
}

// -----------------------------------------------------------------------------------------
// The tests.
// -----------------------------------------------------------------------------------------

/// 1. Round trip by hash: `ensure_bucket`, `put` a multi-kilobyte payload with a real `Label`
///    and a fully-populated `Provenance`, check the returned `AssetRef` against an
///    independently computed `openssl::sha::sha256` and the content-addressed key layout, then
///    `get` it back with a DIFFERENT (cleared) caller than any that touched it during `put` and
///    check the bytes are identical, then `head` it and check `Label`/`Provenance` came back
///    field-for-field equal, caveats and attributes included, and that the server reports the
///    same `sha256`, `size_bytes` (the payload length) and `media_type` as the put.
#[tokio::test]
async fn round_trip_by_hash_put_get_head() {
    const TEST_NAME: &str = "round_trip_by_hash_put_get_head";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);

    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    let payload = multi_kb_payload();
    let expected_sha256 = hex(&sha256(&payload));

    let label = Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string(), "REL-TO//FVEY".to_string()] };
    let provenance = Provenance {
        author_kind: 3,
        principal: "svc-tiler".to_string(),
        tool: "av-store-s3-it".to_string(),
        attributes: BTreeMap::from([("mission".to_string(), "av-store-h1b".to_string())]).into_iter().collect(),
        ..Default::default()
    };

    let asset = fixture
        .client
        .put(Bytes::from(payload.clone()), "application/octet-stream", label.clone(), provenance.clone(), now())
        .await
        .unwrap_or_else(|e| panic!("{TEST_NAME}: put: {e}"));

    assert_eq!(asset.sha256, expected_sha256, "AssetRef.sha256 must equal an independently computed openssl::sha::sha256 of the payload");
    let expected_key = object_key(KEY_PREFIX, &expected_sha256).unwrap_or_else(|e| panic!("{TEST_NAME}: object_key: {e}"));
    assert_eq!(asset.uri, format!("s3://{BUCKET}/{expected_key}"), "the stored object's key must be the content-addressed layout");

    // A caller cleared to the ladder's TOP marking (not the same caller that put it, and no
    // caller identity is threaded through put/get at all -- the point here is only that a
    // cleared caller can read what was written).
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    let fetched = fixture.client.get(&asset, "SECRET", &ladder, now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: get: {e}"));
    assert_eq!(fetched.as_ref(), payload.as_slice(), "get must return byte-identical payload bytes");

    let head = fixture.client.head(&asset, now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: head: {e}"));
    assert_eq!(head.sha256, asset.sha256, "head must return the sha256 that was put");
    assert_eq!(head.size_bytes, payload.len() as u64, "head must report the payload length as size_bytes (the server's Content-Length)");
    assert_eq!(head.media_type, "application/octet-stream", "head must return the media type that was put");
    assert_eq!(head.label, Some(label), "head must return the exact Label that was put, caveats included");
    assert_eq!(head.provenance, Some(provenance), "head must return the exact Provenance that was put");

    println!(
        "{TEST_NAME}: put/get/head all agreed on sha256={expected_sha256}, key={expected_key}, {} payload bytes; head answered size_bytes={}, media_type={:?}",
        payload.len(),
        head.size_bytes,
        head.media_type
    );
}

/// 2. A corrupted object is refused on read: `put` the payload, then PUT DIFFERENT bytes of the
///    SAME length directly to the SAME key through a second, raw signed request (so the
///    content-addressed invariant is violated on the server itself, not in a local buffer this
///    crate never even touches), then `get` with the original `AssetRef` and check
///    `StoreError::HashMismatch` names the expected and actual hashes. Same length as the
///    original, deliberately: `verify_payload` checks size before hash (`src/claim_check.rs`'s
///    own doc), so a length-changing corruption would prove `SizeMismatch`, not this test's own
///    target.
#[tokio::test]
async fn corrupted_object_is_refused_on_read_with_hash_mismatch() {
    const TEST_NAME: &str = "corrupted_object_is_refused_on_read_with_hash_mismatch";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    let payload = multi_kb_payload();
    let label = Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] };
    let asset = fixture.client.put(Bytes::from(payload.clone()), "application/octet-stream", label, Provenance::default(), now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: put: {e}"));

    let key = asset.uri.strip_prefix(&format!("s3://{BUCKET}/")).unwrap_or_else(|| panic!("{TEST_NAME}: unexpected AssetRef.uri shape {:?}", asset.uri));

    // Same length as `payload`, genuinely different bytes -- flip roughly a third of the bytes
    // rather than just the first one, so this is unambiguously a real corruption, not an
    // off-by-one edge case.
    let mut tampered = payload.clone();
    for b in tampered.iter_mut().step_by(3) {
        *b ^= 0xFF;
    }
    assert_ne!(tampered, payload, "the tampering must actually change the bytes");
    assert_eq!(tampered.len(), payload.len(), "the tampering must preserve length -- this test targets HashMismatch, not SizeMismatch");

    let (status, body) = raw_signed_put(&fixture.endpoint, BUCKET, key, &fixture.access_key, &fixture.secret_key, &tampered, now()).await;
    assert!(status.is_success(), "{TEST_NAME}: the raw corrupting PUT must itself succeed: {status} {}", String::from_utf8_lossy(&body));

    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string()]);
    let err = fixture.client.get(&asset, "UNCLASSIFIED", &ladder, now()).await.unwrap_err();
    println!("{TEST_NAME}: real StoreError::HashMismatch Display: {err}");
    match &err {
        StoreError::HashMismatch { expected, actual, size_bytes } => {
            assert_eq!(expected, &asset.sha256, "expected hash must be the AssetRef's own sha256");
            assert_ne!(actual, expected, "actual (recomputed) hash must differ from expected -- that is the whole point of this test");
            assert_eq!(*size_bytes, tampered.len() as u64);
        }
        other => panic!("{TEST_NAME}: expected StoreError::HashMismatch, got {other:?}"),
    }
}

/// 3. A label above the caller's clearance is refused, and the refusal happens before any
///    network call is possible. This file proves it empirically, not merely by reading
///    `client.rs`'s own source: it stops and removes the server container FIRST, then calls `get`
///    with an over-clearance caller. If `get` had made any network call before its label check,
///    it would fail with `StoreError::Connect` against a container that no longer exists --
///    getting the typed `StoreError::OverClearance` instead is direct evidence no request
///    reached (or even could have reached) the server, not an assumption about code structure.
#[tokio::test]
async fn label_above_clearance_is_refused_before_any_network_call() {
    const TEST_NAME: &str = "label_above_clearance_is_refused_before_any_network_call";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let mut fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    let label = Label { marking: "SECRET".to_string(), caveats: vec![] }; // the ladder's TOP marking
    let asset = fixture.client.put(Bytes::from(b"top of the ladder".to_vec()), "application/octet-stream", label, Provenance::default(), now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: put: {e}"));

    // Stop and remove the container BEFORE the over-clearance get -- see this test's own doc
    // comment for why this is the empirical proof, not merely a structural claim.
    fixture.container.stop_and_remove().unwrap_or_else(|e| panic!("{TEST_NAME}: stop_and_remove: {e}"));
    let ps = Command::new("docker").args(["ps", "-a", "-q", "--filter", &format!("id={}", fixture.container_id)]).output().expect("docker ps");
    assert!(String::from_utf8_lossy(&ps.stdout).trim().is_empty(), "{TEST_NAME}: the container must actually be gone before the over-clearance get is attempted, or this test proves nothing");

    let err = fixture.client.get(&asset, "UNCLASSIFIED", &ladder, now()).await.unwrap_err();
    println!("{TEST_NAME}: got {err} with the S3 server container already stopped and removed -- a Connect/Body error here would mean a network call was attempted; OverClearance means none was");
    match &err {
        StoreError::OverClearance { object_marking, caller_clearance } => {
            assert_eq!(object_marking, "SECRET");
            assert_eq!(caller_clearance, "UNCLASSIFIED");
        }
        other => panic!("{TEST_NAME}: expected StoreError::OverClearance (proving no network call was attempted against the already-removed container), got {other:?}"),
    }
}

/// 4. An unknown key is a typed `NotFound`, never a hash mismatch and never a panic -- proves
///    the review-finding fix (`StoreError::NotFound`, `src/error.rs`/`src/client.rs`) for real,
///    against a real server 404, for both `get` and `head`.
#[tokio::test]
async fn unknown_key_is_a_typed_not_found() {
    const TEST_NAME: &str = "unknown_key_is_a_typed_not_found";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));

    // A well-formed (64 lowercase hex) hash that was never put -- so the key is well-formed
    // but genuinely absent, distinct from a malformed AssetRef this crate would refuse before
    // ever reaching the network at all.
    let bogus_sha256 = "0".repeat(64);
    let key = object_key(KEY_PREFIX, &bogus_sha256).unwrap_or_else(|e| panic!("{TEST_NAME}: object_key: {e}"));
    let asset = AssetRef {
        uri: format!("s3://{BUCKET}/{key}"),
        sha256: bogus_sha256,
        size_bytes: 0,
        media_type: "application/octet-stream".to_string(),
        label: Some(Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] }),
        ..Default::default()
    };
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string()]);

    let get_err = fixture.client.get(&asset, "UNCLASSIFIED", &ladder, now()).await.unwrap_err();
    println!("{TEST_NAME}: get on an unknown key returned {get_err}");
    assert!(matches!(get_err, StoreError::NotFound { .. }), "{TEST_NAME}: expected StoreError::NotFound from get, got {get_err:?}");

    let head_err = fixture.client.head(&asset, now()).await.unwrap_err();
    println!("{TEST_NAME}: head on an unknown key returned {head_err}");
    assert!(matches!(head_err, StoreError::NotFound { .. }), "{TEST_NAME}: expected StoreError::NotFound from head, got {head_err:?}");
}

/// 5. The running container's image id equals the recorded digest -- an EXPLICIT assertion
///    (question 212(a)), not merely trusting [`gate_or_skip`]'s own check. The gate compares the
///    local image (by reference) against the recorded id BEFORE any container is created; this
///    test additionally inspects the ACTUAL RUNNING CONTAINER's own `.Image` field afterward, so a
///    hypothetical race (something retagging/replacing the local image between the gate check
///    and `docker run`) would still be caught here, not just assumed away.
#[tokio::test]
async fn running_container_image_id_equals_the_recorded_digest() {
    const TEST_NAME: &str = "running_container_image_id_equals_the_recorded_digest";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);

    let running_image_id = docker(&["inspect", "--format", "{{.Image}}", &fixture.container_id]);
    println!("{TEST_NAME}: running container image id = {running_image_id:?}, recorded id (services/store/IMAGE_DIGEST.md) = {:?}", launch.recorded_id);
    assert_eq!(running_image_id, launch.recorded_id, "the RUNNING container's own image id must equal the digest recorded in services/store/IMAGE_DIGEST.md");

    // ensure_bucket is a cheap, real proof this fixture's client can actually talk to the
    // container whose image id was just checked -- not load-bearing for the assertion above,
    // but confirms this is a live, addressable container, not a lucky id match on something
    // that never actually started serving.
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: ensure_bucket: {e}"));
}

/// 6. The server really checks signatures: with the right access key and a DIFFERENT secret, and
///    again with an unknown access key, `ensure_bucket` must fail with `StoreError::S3 { status:
///    403, .. }`. A control call with the right credentials first proves the 403 is the
///    credentials' doing, not the bucket's or the endpoint's. (A server that accepts any key until
///    credentials are configured would pass the control and fail this test -- which is the point.)
#[tokio::test]
async fn a_wrong_secret_is_refused_by_the_server() {
    const TEST_NAME: &str = "a_wrong_secret_is_refused_by_the_server";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: control ensure_bucket with the right credentials: {e}"));

    let wrong_secret = StoreClient::new(store_config(&fixture.endpoint, &fixture.access_key, &format!("{}-wrong", fixture.secret_key))).unwrap_or_else(|e| panic!("{TEST_NAME}: StoreClient::new: {e}"));
    let err = wrong_secret.ensure_bucket(now()).await.unwrap_err();
    println!("{TEST_NAME}: right access key, wrong secret -> the S3 server answered: {err}");
    match &err {
        StoreError::S3 { status, code, .. } => {
            assert_eq!(*status, 403, "a wrong secret must be refused with 403, got {status} {code}");
            println!("{TEST_NAME}: wrong secret: status={status} code={code}");
        }
        other => panic!("{TEST_NAME}: expected StoreError::S3 {{ status: 403, .. }} for a wrong secret, got {other:?}"),
    }

    let unknown_key = StoreClient::new(store_config(&fixture.endpoint, &format!("{}unknown", fixture.access_key), &fixture.secret_key)).unwrap_or_else(|e| panic!("{TEST_NAME}: StoreClient::new: {e}"));
    let err = unknown_key.ensure_bucket(now()).await.unwrap_err();
    println!("{TEST_NAME}: unknown access key -> the S3 server answered: {err}");
    match &err {
        StoreError::S3 { status, code, .. } => {
            assert_eq!(*status, 403, "an unknown access key must be refused with 403, got {status} {code}");
            println!("{TEST_NAME}: unknown access key: status={status} code={code}");
        }
        other => panic!("{TEST_NAME}: expected StoreError::S3 {{ status: 403, .. }} for an unknown access key, got {other:?}"),
    }
}

/// 7. The server enforces the signature's time window: a correct client whose signing time is an
///    hour behind the server's clock must be refused with `StoreError::S3 { status: 403, .. }`
///    (S3's `RequestTimeTooSkewed`). The control call with the current time on the same client
///    proves the credentials are right, so the refusal is the clock's doing.
#[tokio::test]
async fn a_request_signed_an_hour_from_the_server_clock_is_refused() {
    const TEST_NAME: &str = "a_request_signed_an_hour_from_the_server_clock_is_refused";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);
    fixture.client.ensure_bucket(now()).await.unwrap_or_else(|e| panic!("{TEST_NAME}: control ensure_bucket with the current time: {e}"));

    let err = fixture.client.ensure_bucket(now() - 3600).await.unwrap_err();
    println!("{TEST_NAME}: signed an hour in the past -> the S3 server answered: {err}");
    match &err {
        StoreError::S3 { status, code, .. } => {
            assert_eq!(*status, 403, "a request signed an hour from the server clock must be refused with 403, got {status} {code}");
            println!("{TEST_NAME}: skewed clock: status={status} code={code}");
        }
        other => panic!("{TEST_NAME}: expected StoreError::S3 {{ status: 403, .. }} for a skewed signing time, got {other:?}"),
    }
}

/// 8. `ensure_bucket` is idempotent against the real server: the second call returns `Ok(())`
///    (the server's "already owned by you" answer is tolerated, not surfaced as an error).
#[tokio::test]
async fn ensure_bucket_twice_is_idempotent() {
    const TEST_NAME: &str = "ensure_bucket_twice_is_idempotent";
    let launch = gate_or_skip!(TEST_NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let fixture = start_fixture(&launch, TEST_NAME);

    let first = fixture.client.ensure_bucket(now()).await;
    let second = fixture.client.ensure_bucket(now()).await;
    println!("{TEST_NAME}: first ensure_bucket -> {first:?}; second ensure_bucket -> {second:?}");
    first.unwrap_or_else(|e| panic!("{TEST_NAME}: first ensure_bucket: {e}"));
    second.unwrap_or_else(|e| panic!("{TEST_NAME}: second ensure_bucket: {e}"));
}

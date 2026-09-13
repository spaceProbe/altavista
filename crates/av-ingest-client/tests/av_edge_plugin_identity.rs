//! Question 207's headline requirement: the **built** `av-edge-plugin` binary
//! (`src/bin/av-edge-plugin.rs`) now verifies and presents a real seccert-shaped
//! identity, end to end, against a REAL `av-ingest` server on loopback -- not just the
//! library (`crates/av-edge/tests/identity.rs`) or an in-process `Ingest` (`crates/
//! av-ingest/tests/identity_refusal.rs`) or a raw wire client (`crates/av-ingest/tests/
//! wire_identity.rs`). Every test here spawns the REAL built binary
//! (`env!("CARGO_BIN_EXE_av-edge-plugin")`, exactly `tests/av_edge_plugin_binary.rs`'s
//! own pattern) against a REAL, in-process `EdgeIngestService` on an ephemeral loopback
//! port.
//!
//! Lives in `crates/av-ingest-client/tests/`, not `crates/av-ingest/tests/`, because
//! `env!("CARGO_BIN_EXE_av-edge-plugin")` is only set for integration tests of the crate
//! that actually declares that `[[bin]]` target (`av-ingest-client`'s own `Cargo.toml`) --
//! `crates/av-ingest`'s own tests have no way to name it.
//!
//! # The hermetic two-tier CA
//!
//! `Hierarchy` below is the same builder `crates/av-ingest/tests/wire_identity.rs`
//! (itself mirroring `crates/av-ingest/tests/identity_refusal.rs`) already uses,
//! duplicated here rather than shared -- this workspace's own established convention (no
//! `tests/common/mod.rs` exists anywhere to extend instead, and every sibling `wire_*.rs`/
//! `identity_refusal.rs` test file already keeps its own copy). No network, no `lego`, no
//! `seccert` process anywhere (question 154): every certificate is built in-process with
//! the `openssl` crate directly.
//!
//! # Reading test (b)'s "wrong CA... reaches the wire"
//!
//! `av-edge-plugin`'s own new `--client-cert`/`--trust-anchor` pairing rule (`parse_args`:
//! either both or neither) means the *only* way this binary ever reaches `Announce` while
//! presenting a certificate is for its own local check to succeed first -- a rejection
//! exits before ever dialing (test (c) below). So "a leaf from a different CA reaches the
//! wire" (test (b)) is built by giving the plugin's own `--trust-anchor` the *same* CA
//! that issued its own `--client-cert` (its local check is self-consistent and passes),
//! while the **ingest** is configured to trust a *different* Root -- from the ingest's own
//! point of view this is exactly "a leaf from an unrelated CA", the identical shape
//! `wire_identity.rs::a_leaf_from_a_different_ca_is_refused_and_issuer_not_trusted_increments`
//! already proves at the library-client level; this file proves the same defect surfaces
//! correctly all the way through the real plugin binary's own new local-verification gate.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use av_edge::identity::TrustAnchors;
use av_edge::pb;
use av_edge::plugin::{BatchingRule, Pacing, PluginConfig};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::server::bind_loopback;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::extension::{BasicConstraints, KeyUsage};
use openssl::x509::{X509Name, X509NameBuilder, X509};
use tonic::transport::Server;

// -----------------------------------------------------------------------------------------
// Hermetic two-tier CA builder (mirrors crates/av-ingest/tests/wire_identity.rs's own).
// -----------------------------------------------------------------------------------------

fn p384_key() -> EcKey<Private> {
    EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap()
}

fn subject(cn: &str) -> X509Name {
    let mut b = X509NameBuilder::new().unwrap();
    b.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    b.build()
}

fn serial(n: u32) -> Asn1Integer {
    BigNum::from_u32(n).unwrap().to_asn1_integer().unwrap()
}

const WINDOW_START: i64 = 1_700_000_000;
const WINDOW_END: i64 = 1_900_000_000;
const NOW_UNIX: i64 = 1_800_000_000;

fn tai_ns_for_unix(unix_seconds: i64) -> i64 {
    av_cdm::time::Tai::from_utc_nanos(unix_seconds.checked_mul(1_000_000_000).unwrap()).as_nanos()
}

struct Hierarchy {
    root_cert: X509,
    intermediate_cert: X509,
    intermediate_key: EcKey<Private>,
}

impl Hierarchy {
    fn new(label: &str, not_before: i64, not_after: i64) -> Self {
        let root_key = p384_key();
        let root_pkey = PKey::from_ec_key(root_key.clone()).unwrap();
        let root_name = subject(&format!("{label} Root"));
        let mut rb = X509::builder().unwrap();
        rb.set_version(2).unwrap();
        rb.set_subject_name(&root_name).unwrap();
        rb.set_issuer_name(&root_name).unwrap();
        rb.set_pubkey(&root_pkey).unwrap();
        rb.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
        rb.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
        rb.set_serial_number(&serial(1)).unwrap();
        rb.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
        rb.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
        rb.sign(&root_pkey, MessageDigest::sha256()).unwrap();
        let root_cert = rb.build();

        let intermediate_key = p384_key();
        let intermediate_pkey = PKey::from_ec_key(intermediate_key.clone()).unwrap();
        let mut ib = X509::builder().unwrap();
        ib.set_version(2).unwrap();
        ib.set_subject_name(&subject(&format!("{label} Intermediate"))).unwrap();
        ib.set_issuer_name(root_cert.subject_name()).unwrap();
        ib.set_pubkey(&intermediate_pkey).unwrap();
        ib.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
        ib.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
        ib.set_serial_number(&serial(2)).unwrap();
        ib.append_extension(BasicConstraints::new().critical().ca().pathlen(0).build().unwrap()).unwrap();
        ib.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
        ib.sign(&root_pkey, MessageDigest::sha256()).unwrap();
        let intermediate_cert = ib.build();

        Self { root_cert, intermediate_cert, intermediate_key }
    }

    fn root_pem(&self) -> Vec<u8> {
        self.root_cert.to_pem().unwrap()
    }

    fn chain_pem(&self) -> Vec<u8> {
        self.intermediate_cert.to_pem().unwrap()
    }

    fn issue_leaf(&self, cn: &str, serial_n: u32, not_before: i64, not_after: i64) -> (X509, EcKey<Private>) {
        let leaf_key = p384_key();
        let leaf_pkey = PKey::from_ec_key(leaf_key.clone()).unwrap();
        let mut lb = X509::builder().unwrap();
        lb.set_version(2).unwrap();
        lb.set_subject_name(&subject(cn)).unwrap();
        lb.set_issuer_name(self.intermediate_cert.subject_name()).unwrap();
        lb.set_pubkey(&leaf_pkey).unwrap();
        lb.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
        lb.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
        lb.set_serial_number(&serial(serial_n)).unwrap();
        lb.append_extension(BasicConstraints::new().critical().build().unwrap()).unwrap();
        lb.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
        let intermediate_pkey = PKey::from_ec_key(self.intermediate_key.clone()).unwrap();
        lb.sign(&intermediate_pkey, MessageDigest::sha256()).unwrap();
        (lb.build(), leaf_key)
    }

    /// The leaf's own certificate PEM immediately followed by this hierarchy's own
    /// Intermediate PEM -- a "fullchain.pem", the bundle shape `av-edge-plugin`'s own
    /// `split_leaf_and_chain` expects for `--client-cert` (this file's own module doc).
    fn fullchain_pem(&self, leaf: &X509) -> Vec<u8> {
        let mut bundle = leaf.to_pem().unwrap();
        bundle.extend(self.chain_pem());
        bundle
    }
}

// -----------------------------------------------------------------------------------------
// Server harness (mirrors crates/av-ingest-client/tests/av_edge_plugin_binary.rs's own
// start_server, but wired for the certificate path: require_client_certificate = true,
// real TrustAnchors, no config-side verify_keys at all).
// -----------------------------------------------------------------------------------------

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-edge-plugin-identity-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

// Deliberately astronomical: this file's own hermetic CA windows (unix seconds,
// ~2023-2030-ish for tests a/b/c; a narrow 2033-ish minute for test d) are chosen purely
// to exercise `av_edge::identity`'s own checks and have no relationship to the committed
// `ground_segment` fixture's own real batch epochs (`run_products.pb`'s own recorded
// `created_tai_ns`) -- `max_age_ns` this large (~15,850 years) makes every batch in every
// test in this file never stale regardless of that gap, so this file's own tests can pick
// CA windows for identity reasons alone, exactly like `crates/av-ingest-client/tests/
// av_edge_plugin_binary.rs`'s own `SERVER_CLOCK_TAI_NS = 0` trick does for the same
// underlying reason (a batch from the "past" relative to this generous a window is still
// never rejected as stale).
const MAX_AGE_NS: i64 = 500_000_000_000_000_000;

async fn start_server(dir: &std::path::Path, anchors: TrustAnchors, intermediate_chain_pem: Vec<u8>, clock_tai_ns: i64) -> (SocketAddr, Arc<EdgeIngestService>) {
    let mut cfg = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], MAX_AGE_NS);
    cfg.require_client_certificate = true;
    cfg.intermediate_chain_pem = Some(intermediate_chain_pem);
    let service = Arc::new(EdgeIngestService::new(dir, Some(anchors), cfg, Arc::new(move || clock_tai_ns)));

    let listener = bind_loopback("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    (addr, service)
}

// -----------------------------------------------------------------------------------------
// Fixture / plugin-config (identical to av_edge_plugin_binary.rs's own -- same committed
// ground_segment fixture, same codec, same 900-measurement/900-batch shape).
// -----------------------------------------------------------------------------------------

fn av_edge_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../av-edge/tests/fixtures")
}

fn plugin_config() -> PluginConfig {
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
    let codec = pb::PacketCodec {
        id: "flight_tm_out_codec".to_string(),
        apid: 500,
        is_command: false,
        secondary_header_bytes: 0,
        user_data_bytes: 24,
        description: "own Earth-fixed Cartesian position telemetry, encoded (M25.1)".to_string(),
        fields: vec![f("x", 0), f("y", 64), f("z", 128)],
    };
    let label = pb::Label { marking: "CUI".to_string(), caveats: vec![] };
    PluginConfig {
        producer_id: "demo-ground-segment-flight-plugin".to_string(),
        plugin_version: "0.1.0".to_string(),
        instance: "flight".to_string(),
        port: "tm_out".to_string(),
        direction: pb::PortDirection::Out as i32,
        codec_bytes: PluginConfig::encode_codec(&codec),
        component_fields: vec!["x".to_string(), "y".to_string(), "z".to_string()],
        frame_id: "earth_fixed_demo_frame".to_string(),
        sensor_id: "ground-segment-flight".to_string(),
        measurement_id: "flight_position".to_string(),
        shard_key: "ground-segment-demo".to_string(),
        noise_r: vec![100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 100.0],
        label_bytes: PluginConfig::encode_label(&label),
        clearance: "CUI".to_string(),
        leaf_fingerprint_sha256: String::new(),
        batching: BatchingRule::PerEpoch,
        pacing: Pacing::AsFastAsPossible,
    }
}

fn write_plugin_config(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec_pretty(&plugin_config()).unwrap()).unwrap();
    path
}

// -----------------------------------------------------------------------------------------
// Running the real built binary.
// -----------------------------------------------------------------------------------------

struct PluginArgs {
    plugin_config: PathBuf,
    signing_key: PathBuf,
    endpoint: String,
    /// Also passed as `--client-key` whenever set: the existing (unchanged,
    /// pre-question-207) `--client-cert`/`--client-key` pairing rule
    /// (`parse_args`) still applies unconditionally -- this binary's plaintext path
    /// never actually reads `--client-key` (only the `https://` mTLS path does,
    /// `Client::connect`), but a real deployment's `--client-key` and `--signing-key`
    /// name the identical leaf private key anyway (the same key both proves this
    /// process's own identity and signs every batch), so every test in this file just
    /// points both flags at the one key file it already wrote for `--signing-key`.
    client_cert: Option<PathBuf>,
    trust_anchor: Option<PathBuf>,
    now_tai_ns: Option<i64>,
}

fn run_plugin(a: &PluginArgs) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_av-edge-plugin");
    let mut cmd = Command::new(bin);
    cmd.arg("--run-products")
        .arg(av_edge_fixtures_dir().join("ground_segment/run_products.pb"))
        .arg("--port-traffic")
        .arg(av_edge_fixtures_dir().join("ground_segment/port_traffic.pb"))
        .arg("--plugin-config")
        .arg(&a.plugin_config)
        .arg("--signing-key")
        .arg(&a.signing_key)
        .arg("--endpoint")
        .arg(&a.endpoint);
    if let Some(c) = &a.client_cert {
        cmd.arg("--client-cert").arg(c);
        cmd.arg("--client-key").arg(&a.signing_key);
    }
    if let Some(t) = &a.trust_anchor {
        cmd.arg("--trust-anchor").arg(t);
    }
    if let Some(n) = a.now_tai_ns {
        cmd.arg("--now-tai-ns").arg(n.to_string());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().expect("running the built av-edge-plugin binary")
}

/// The one line of JSON this binary's own `verify_own_identity` prints to stderr on a
/// local rejection (this file's own module doc references it; `src/bin/
/// av-edge-plugin.rs`'s own module doc, "Presenting and verifying this plugin's own
/// identity", names the exact shape: `identity_verified`/`rejection`/`detail`/
/// `counters`).
fn find_identity_rejection_json(stderr: &str) -> serde_json::Value {
    let line = stderr.lines().find(|l| l.trim_start().starts_with('{')).unwrap_or_else(|| panic!("no JSON line found on stderr:\n{stderr}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("stderr's JSON line did not parse: {e}\nline={line}"))
}

// -----------------------------------------------------------------------------------------
// (a) Good leaf, end to end.
// -----------------------------------------------------------------------------------------

// `flavor = "multi_thread"`: this test both runs the in-process `EdgeIngestServer` on a
// `tokio::spawn`ed task AND blocks synchronously in `std::process::Command::output()` --
// `crates/av-ingest-client/tests/av_edge_plugin_binary.rs`'s own identical doc comment
// explains the deadlock this attribute avoids; every test in this file carries it for the
// same reason.
#[tokio::test(flavor = "multi_thread")]
async fn a_good_leaf_is_verified_locally_and_accepted_end_to_end() {
    let trusted = Hierarchy::new("Plugin-A-Trusted", WINDOW_START, WINDOW_END);
    let (leaf_cert, leaf_key) = trusted.issue_leaf("edge-plugin-a", 10, WINDOW_START, WINDOW_END);

    let dir = tmp_dir("a");
    let anchors = TrustAnchors::from_root_pem(&trusted.root_pem()).unwrap();
    let (addr, service) = start_server(&dir.join("ingest"), anchors, trusted.chain_pem(), tai_ns_for_unix(NOW_UNIX)).await;

    let client_cert_path = dir.join("leaf-fullchain.pem");
    std::fs::write(&client_cert_path, trusted.fullchain_pem(&leaf_cert)).unwrap();
    let trust_anchor_path = dir.join("root.pem");
    std::fs::write(&trust_anchor_path, trusted.root_pem()).unwrap();
    let signing_key_path = dir.join("leaf-key.pem");
    std::fs::write(&signing_key_path, leaf_key.private_key_to_pem().unwrap()).unwrap();
    let config_path = write_plugin_config(&dir, "plugin_config.json");

    let output = run_plugin(&PluginArgs {
        plugin_config: config_path,
        signing_key: signing_key_path,
        endpoint: addr.to_string(),
        client_cert: Some(client_cert_path),
        trust_anchor: Some(trust_anchor_path),
        now_tai_ns: Some(tai_ns_for_unix(NOW_UNIX)),
    });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "av-edge-plugin exited non-zero: status={:?}\nstdout={stdout}\nstderr={stderr}", output.status);

    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("stdout was not valid JSON: {e}\nstdout={stdout}"));
    assert_eq!(summary["batch_count"], 900);
    assert_eq!(summary["measurement_count"], 900);
    assert_eq!(summary["any_rejected"], false);
    assert_eq!(summary["identity_verified"], true);
    assert_eq!(summary["subject_cn"], "edge-plugin-a");
    let fingerprint = summary["fingerprint_sha256"].as_str().expect("fingerprint_sha256 must be a string").to_string();
    assert_eq!(fingerprint.len(), 64, "a SHA-256 hex fingerprint is 64 hex characters: {fingerprint:?}");
    let verdicts = summary["verdicts"].as_array().expect("verdicts array");
    assert_eq!(verdicts.len(), 900);
    assert!(verdicts.iter().all(|v| v["accepted"] == true), "every verdict must be accepted");

    // Note: `chain_head_hex` here is NOT `av_edge_plugin_binary.rs`'s own pinned
    // `d1d80d0b...` constant -- that pin is for the E1 no-certificate path, where
    // `leaf_fingerprint_sha256` is the empty string. `hash::canonical_body_bytes`
    // deliberately does NOT clear `signer_cert_sha256` (`crates/av-edge/src/sign.rs`'s own
    // doc comment on `sign_batch_with_signer`), so overriding it with a real, verified
    // fingerprint (this binary's own new, intentional behaviour -- see `src/bin/
    // av-edge-plugin.rs`'s module doc) necessarily changes every batch's own `batch_hash`
    // and therefore the final chain head. This is the correct, security-relevant
    // consequence of "the manifest, every batch's signer_cert_sha256 and the certificate
    // cannot drift apart" -- not a defect in this test.
    let chain_head_hex = summary["chain_head_hex"].as_str().unwrap().to_string();
    assert_eq!(chain_head_hex.len(), 64);

    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.accepted, 1);
    assert_eq!(identity.issuer_not_trusted, 0);
    assert_eq!(identity.expired, 0);
    assert_eq!(identity.not_yet_valid, 0);
    assert_eq!(identity.malformed_pem, 0);
    assert_eq!(identity.not_p384, 0);
    assert_eq!(identity.openssl_error, 0);
    assert_eq!(evidence.accepted_total, 900);

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r4/t3a");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_a_stdout.json"), stdout.as_bytes()).unwrap();
    std::fs::write(out_dir.join("test_a_fingerprint.txt"), fingerprint.as_bytes()).unwrap();
}

// -----------------------------------------------------------------------------------------
// (b) Wrong CA, refused and counted end to end -- the headline requirement.
// -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_leaf_from_a_wrong_ca_reaches_the_wire_and_is_refused_and_counted_end_to_end() {
    // The ingest trusts only "Trusted".
    let trusted = Hierarchy::new("Plugin-B-Trusted", WINDOW_START, WINDOW_END);
    // The leaf actually presented was issued by an entirely different CA.
    let foreign = Hierarchy::new("Plugin-B-Foreign", WINDOW_START, WINDOW_END);
    let (foreign_leaf, foreign_key) = foreign.issue_leaf("edge-plugin-b", 10, WINDOW_START, WINDOW_END);

    let dir = tmp_dir("b");
    let anchors = TrustAnchors::from_root_pem(&trusted.root_pem()).unwrap();
    // The server's own chain hint is the FOREIGN hierarchy's intermediate -- set purely so
    // the ingest's own chain-build step reaches the issuer-mismatch decision instead of
    // failing for an unrelated reason, mirroring `wire_identity.rs`'s own identical test
    // and its own identical comment.
    let (addr, service) = start_server(&dir.join("ingest"), anchors, foreign.chain_pem(), tai_ns_for_unix(NOW_UNIX)).await;

    // The plugin's OWN --trust-anchor is the FOREIGN root -- the same CA that issued its
    // own --client-cert, so its local check is self-consistent and it reaches the wire
    // (this file's own module doc, "Reading test (b)'s 'wrong CA... reaches the wire'").
    let client_cert_path = dir.join("leaf-fullchain.pem");
    std::fs::write(&client_cert_path, foreign.fullchain_pem(&foreign_leaf)).unwrap();
    let trust_anchor_path = dir.join("foreign-root.pem");
    std::fs::write(&trust_anchor_path, foreign.root_pem()).unwrap();
    let signing_key_path = dir.join("leaf-key.pem");
    std::fs::write(&signing_key_path, foreign_key.private_key_to_pem().unwrap()).unwrap();
    let config_path = write_plugin_config(&dir, "plugin_config.json");

    let output = run_plugin(&PluginArgs {
        plugin_config: config_path,
        signing_key: signing_key_path,
        endpoint: addr.to_string(),
        client_cert: Some(client_cert_path),
        trust_anchor: Some(trust_anchor_path),
        now_tai_ns: Some(tai_ns_for_unix(NOW_UNIX)),
    });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "av-edge-plugin must exit non-zero when Announce is refused: stdout={stdout}\nstderr={stderr}");
    assert!(stderr.contains("Announce was refused"), "stderr must print the typed Announce refusal: {stderr}");
    assert!(stderr.contains("MANIFEST_REFUSAL_IDENTITY_REFUSED"), "stderr must name the refusal kind: {stderr}");

    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.issuer_not_trusted, 1, "issuer_not_trusted must increment by exactly one, read back through the ingest's own evidence surface");
    assert_eq!(identity.accepted, 0);
    assert_eq!(identity.expired, 0);
    assert_eq!(identity.not_yet_valid, 0);
    assert_eq!(identity.malformed_pem, 0);
    assert_eq!(identity.not_p384, 0);
    assert_eq!(identity.openssl_error, 0);
    assert_eq!(evidence.accepted_total, 0, "zero accepted batches");
    assert_eq!(evidence.rejected_total, 0, "Announce was refused before any Submit was ever attempted");
    assert!(evidence.partitions.is_empty(), "no batch must ever reach a partition log");

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r4/t3a");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_b_stderr.txt"), stderr.as_bytes()).unwrap();
    std::fs::write(out_dir.join("test_b_identity_counters.txt"), format!("{identity:#?}\naccepted_total={}\nrejected_total={}\npartitions_empty={}\n", evidence.accepted_total, evidence.rejected_total, evidence.partitions.is_empty())).unwrap();
}

// -----------------------------------------------------------------------------------------
// (c) The plugin refuses its own untrusted leaf before connecting.
// -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_plugin_refuses_its_own_untrusted_leaf_before_connecting() {
    let trusted = Hierarchy::new("Plugin-C-Trusted", WINDOW_START, WINDOW_END);
    let foreign = Hierarchy::new("Plugin-C-Foreign", WINDOW_START, WINDOW_END);
    let (foreign_leaf, foreign_key) = foreign.issue_leaf("edge-plugin-c", 10, WINDOW_START, WINDOW_END);

    let dir = tmp_dir("c");
    let anchors = TrustAnchors::from_root_pem(&trusted.root_pem()).unwrap();
    let (addr, service) = start_server(&dir.join("ingest"), anchors, trusted.chain_pem(), tai_ns_for_unix(NOW_UNIX)).await;

    // The plugin's own --trust-anchor is the REAL Root (what the ingest also trusts), but
    // --client-cert is a leaf from an entirely unrelated CA -- local verification must
    // fail before this binary ever dials `addr`.
    let client_cert_path = dir.join("leaf-fullchain.pem");
    std::fs::write(&client_cert_path, foreign.fullchain_pem(&foreign_leaf)).unwrap();
    let trust_anchor_path = dir.join("real-root.pem");
    std::fs::write(&trust_anchor_path, trusted.root_pem()).unwrap();
    let signing_key_path = dir.join("leaf-key.pem");
    std::fs::write(&signing_key_path, foreign_key.private_key_to_pem().unwrap()).unwrap();
    let config_path = write_plugin_config(&dir, "plugin_config.json");

    let output = run_plugin(&PluginArgs {
        plugin_config: config_path,
        signing_key: signing_key_path,
        endpoint: addr.to_string(),
        client_cert: Some(client_cert_path),
        trust_anchor: Some(trust_anchor_path),
        now_tai_ns: Some(tai_ns_for_unix(NOW_UNIX)),
    });

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "the plugin must exit non-zero refusing its own untrusted leaf: stdout={stdout}\nstderr={stderr}");
    assert!(stderr.contains("identity verification refused before connecting"), "{stderr}");

    let report = find_identity_rejection_json(&stderr);
    assert_eq!(report["identity_verified"], false);
    assert_eq!(report["rejection"], "ISSUER_NOT_TRUSTED", "{report}");
    let counters = &report["counters"];
    assert_eq!(counters["accepted"], 0);
    assert_eq!(counters["malformed_pem"], 0);
    assert_eq!(counters["not_p384"], 0);
    assert_eq!(counters["issuer_not_trusted"], 1);
    assert_eq!(counters["expired"], 0);
    assert_eq!(counters["not_yet_valid"], 0);
    assert_eq!(counters["openssl_error"], 0);

    // This is what actually proves E2 is wired into the binary, not just the library: the
    // ingest -- which has been running the whole time -- never received anything at all.
    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.accepted, 0);
    assert_eq!(identity.malformed_pem, 0);
    assert_eq!(identity.not_p384, 0);
    assert_eq!(identity.issuer_not_trusted, 0, "the ingest's own counters must be untouched -- Announce was never called");
    assert_eq!(identity.expired, 0);
    assert_eq!(identity.not_yet_valid, 0);
    assert_eq!(identity.openssl_error, 0);
    assert_eq!(evidence.accepted_total, 0);
    assert_eq!(evidence.rejected_total, 0);
    assert!(evidence.partitions.is_empty());

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r4/t3a");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_c_stderr.txt"), stderr.as_bytes()).unwrap();
}

// -----------------------------------------------------------------------------------------
// (d) Certificate expiry with the clock injected, never slept.
// -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn certificate_expiry_is_checked_against_the_injected_clock_never_slept() {
    let leaf_not_before = 2_000_000_000;
    let leaf_not_after = leaf_not_before + 60; // one-minute-wide leaf, matching crates/av-edge/tests/identity.rs's own convention.
    let trusted = Hierarchy::new("Plugin-D-Trusted", WINDOW_START, leaf_not_after + 1_000_000);
    let (leaf_cert, leaf_key) = trusted.issue_leaf("edge-plugin-d", 10, leaf_not_before, leaf_not_after);

    let dir = tmp_dir("d");
    let anchors = TrustAnchors::from_root_pem(&trusted.root_pem()).unwrap();
    // The server's own clock sits comfortably inside the leaf's window -- only the
    // PLUGIN's own local --now-tai-ns moves across the expiry boundary in this test; the
    // server's own clock is fixed for its whole lifetime (an entirely separate process).
    let server_clock = tai_ns_for_unix(leaf_not_before + 30);
    let (addr, service) = start_server(&dir.join("ingest"), anchors, trusted.chain_pem(), server_clock).await;

    let client_cert_path = dir.join("leaf-fullchain.pem");
    std::fs::write(&client_cert_path, trusted.fullchain_pem(&leaf_cert)).unwrap();
    let trust_anchor_path = dir.join("root.pem");
    std::fs::write(&trust_anchor_path, trusted.root_pem()).unwrap();
    let signing_key_path = dir.join("leaf-key.pem");
    std::fs::write(&signing_key_path, leaf_key.private_key_to_pem().unwrap()).unwrap();
    let config_path = write_plugin_config(&dir, "plugin_config.json");

    // Run 1: --now-tai-ns PAST notAfter -- refused locally, never connects. No `sleep`
    // anywhere -- the clock is a command-line argument, not a live read (question 199).
    let past = run_plugin(&PluginArgs {
        plugin_config: config_path.clone(),
        signing_key: signing_key_path.clone(),
        endpoint: addr.to_string(),
        client_cert: Some(client_cert_path.clone()),
        trust_anchor: Some(trust_anchor_path.clone()),
        now_tai_ns: Some(tai_ns_for_unix(leaf_not_after + 30)),
    });
    let past_stdout = String::from_utf8_lossy(&past.stdout);
    let past_stderr = String::from_utf8_lossy(&past.stderr);
    assert!(!past.status.success(), "an expired leaf must be refused: stdout={past_stdout}\nstderr={past_stderr}");
    let past_report = find_identity_rejection_json(&past_stderr);
    assert_eq!(past_report["identity_verified"], false);
    assert_eq!(past_report["rejection"], "EXPIRED", "{past_report}");
    assert_eq!(past_report["counters"]["expired"], 1);
    assert_eq!(past_report["counters"]["accepted"], 0);
    assert_eq!(past_report["counters"]["issuer_not_trusted"], 0);

    // Run 2: --now-tai-ns INSIDE the window -- accepted end to end, against the SAME
    // already-running server.
    let inside = run_plugin(&PluginArgs {
        plugin_config: config_path,
        signing_key: signing_key_path,
        endpoint: addr.to_string(),
        client_cert: Some(client_cert_path),
        trust_anchor: Some(trust_anchor_path),
        now_tai_ns: Some(tai_ns_for_unix(leaf_not_before + 30)),
    });
    let inside_stdout = String::from_utf8_lossy(&inside.stdout);
    let inside_stderr = String::from_utf8_lossy(&inside.stderr);
    assert!(inside.status.success(), "the same leaf, inside its own validity window, must be accepted: stdout={inside_stdout}\nstderr={inside_stderr}");
    let summary: serde_json::Value = serde_json::from_str(inside_stdout.trim()).unwrap();
    assert_eq!(summary["identity_verified"], true);
    assert_eq!(summary["any_rejected"], false);
    assert_eq!(summary["batch_count"], 900);

    // The SERVER's own counters only ever reflect what actually reached it -- the
    // expired run above never dialed at all, so `expired` here stays 0; only the
    // accepted, inside-window run ever touched the wire.
    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.accepted, 1);
    assert_eq!(identity.expired, 0, "the expired-clock run never reached the ingest at all -- it refused locally");
    assert_eq!(identity.issuer_not_trusted, 0);
    assert_eq!(identity.not_yet_valid, 0);

    let out_dir = std::path::Path::new("/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r4/t3a");
    let _ = std::fs::create_dir_all(out_dir);
    std::fs::write(out_dir.join("test_d_past_stderr.txt"), past_stderr.as_bytes()).unwrap();
    std::fs::write(out_dir.join("test_d_inside_stdout.json"), inside_stdout.as_bytes()).unwrap();
}

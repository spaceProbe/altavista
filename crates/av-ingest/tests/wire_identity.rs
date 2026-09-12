//! Requirement 3 (question 202's charter): the forwarded-certificate header path, through
//! the wire. A good leaf is accepted; a leaf from a different CA is refused AND
//! `IdentityCounters.issuer_not_trusted` increments and shows in `GetEvidence`; a lapsed
//! leaf (the injected clock moved past `notAfter`, never slept) is refused AND
//! `IdentityCounters.expired` increments.
//!
//! Reuses the same hermetic, in-process two-tier ECDSA P-384 CA builder `crates/av-ingest/
//! tests/identity_refusal.rs` already built (itself mirroring `crates/av-edge/tests/
//! identity.rs`'s own, cheaper than the real seccert+lego loop
//! `tests/test_edge_identity_seccert.py` runs) -- duplicated here rather than shared,
//! matching this crate's own established "each test file is self-contained" convention
//! (no `tests/common/mod.rs` exists anywhere in this workspace to extend instead).
//!
//! The clock is injected via a `Clock` closure over an `AtomicI64` this test advances by
//! hand between calls -- **never slept** (question 199's sibling rule for this track).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use av_edge::{hash, pb, sign};
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};
use av_ingest_client::{forwarded_cert, EdgeIngestClient};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::extension::{BasicConstraints, KeyUsage};
use openssl::x509::{X509Name, X509NameBuilder, X509};
use tonic::transport::Server;

// ---------------------------------------------------------------------------------------
// Hermetic two-tier CA builder (mirrors crates/av-ingest/tests/identity_refusal.rs's own).
// ---------------------------------------------------------------------------------------

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
}

// ---------------------------------------------------------------------------------------
// Server/client harness.
// ---------------------------------------------------------------------------------------

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-wire-identity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn poll_until_ready(addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("server at {addr} never became ready within the deadline");
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

fn manifest(producer_id: &str) -> pb::PluginManifest {
    pb::PluginManifest {
        producer_id: producer_id.to_string(),
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        clearance: "CUI".to_string(),
        shard_keys: vec!["shard-a".to_string()],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_good_leaf_is_accepted_over_the_wire() {
    let trusted = Hierarchy::new("WI-good", WINDOW_START, WINDOW_END);
    let (leaf_cert, leaf_key) = trusted.issue_leaf("edge-plugin-1", 10, WINDOW_START, WINDOW_END);

    let dir = tmp_dir("good-leaf");
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 5_000_000_000);
    config.intermediate_chain_pem = Some(trusted.chain_pem());
    let clock = Arc::new(AtomicI64::new(tai_ns_for_unix(NOW_UNIX)));
    let (mut client, service) = start_server_with_anchors(&dir, config, &trusted.root_pem(), clock.clone()).await;

    let escaped = forwarded_cert::percent_encode_pem_like_nginx(&leaf_cert.to_pem().unwrap());
    client.set_forwarded_client_cert(escaped);

    let ack = client.announce(manifest("edge-plugin-1")).await.expect("Announce must succeed over the wire");
    assert!(ack.accepted, "{ack:?}");

    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.accepted, 1);
    assert_eq!(identity.issuer_not_trusted, 0);
    assert_eq!(identity.expired, 0);

    // A batch signed with the leaf's own key must also be accepted -- proving the cached
    // key really is the certificate's own public key, not merely "identity accepted".
    let mut batch = pb::MeasurementBatch {
        producer_id: "edge-plugin-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        batch_tai_ns: tai_ns_for_unix(NOW_UNIX),
        shard_key: "shard-a".to_string(),
        ..Default::default()
    };
    sign::sign_batch(&mut batch, hash::GENESIS, &leaf_key).unwrap();
    let verdicts = client.submit_batches(vec![batch]).await.unwrap();
    assert!(verdicts[0].accepted, "{:?}", verdicts[0]);
}

#[tokio::test]
async fn a_leaf_from_a_different_ca_is_refused_and_issuer_not_trusted_increments() {
    let trusted = Hierarchy::new("WI-trusted", WINDOW_START, WINDOW_END);
    let foreign = Hierarchy::new("WI-foreign", WINDOW_START, WINDOW_END);
    let (foreign_leaf, _foreign_key) = foreign.issue_leaf("edge-plugin-1", 10, WINDOW_START, WINDOW_END);

    let dir = tmp_dir("different-ca");
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 5_000_000_000);
    // The service is configured with the FOREIGN hierarchy's own intermediate as its
    // chain hint (a deployment only ever configures the chain for its own PKI in
    // practice; this is set here purely so the chain-build step reaches the
    // issuer-mismatch decision instead of failing earlier for an unrelated reason).
    config.intermediate_chain_pem = Some(foreign.chain_pem());
    let clock = Arc::new(AtomicI64::new(tai_ns_for_unix(NOW_UNIX)));
    let (mut client, service) = start_server_with_anchors(&dir, config, &trusted.root_pem(), clock).await;

    let escaped = forwarded_cert::percent_encode_pem_like_nginx(&foreign_leaf.to_pem().unwrap());
    client.set_forwarded_client_cert(escaped);

    let ack = client.announce(manifest("edge-plugin-1")).await.expect("Announce (the RPC itself) must not fail -- refusal is in ManifestAck, not a transport error");
    assert!(!ack.accepted, "{ack:?}");
    assert_eq!(ack.refusal, pb::ManifestRefusal::IdentityRefused as i32, "{ack:?}");

    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.issuer_not_trusted, 1, "issuer_not_trusted must increment by exactly one, over the wire, read back via GetEvidence's own field");
    assert_eq!(identity.accepted, 0);
    assert_eq!(identity.expired, 0);
}

#[tokio::test]
async fn a_lapsed_leaf_is_refused_and_expired_increments_with_the_clock_injected_never_slept() {
    let leaf_not_before = 2_000_000_000;
    let leaf_not_after = leaf_not_before + 60; // one-minute-wide leaf, matching crates/av-edge/tests/identity.rs's own convention
    // The Root/Intermediate's own window must comfortably CONTAIN the leaf's -- otherwise
    // the injected clock would find the *issuer* outside its own window at the very
    // instant this test means to isolate the *leaf's* own expiry, which is IssuerNotTrusted
    // (a different, already-covered defect -- crates/av-edge/tests/identity.rs::
    // a_lapsed_issuer_under_a_still_valid_leaf_is_issuer_not_trusted_not_expired proves
    // that combination directly), not Expired. An earlier draft of this test used the
    // shared WINDOW_START/WINDOW_END constants (which end at 1_900_000_000, before this
    // leaf's own 2_000_000_000 start) for the hierarchy and failed with exactly that
    // IssuerNotTrusted detail on its very first, supposedly-inside-window Announce --
    // root-caused to this window mismatch, not a service defect.
    let trusted = Hierarchy::new("WI-lapsed", WINDOW_START, leaf_not_after + 1_000_000);
    let (leaf_cert, _leaf_key) = trusted.issue_leaf("edge-plugin-1", 10, leaf_not_before, leaf_not_after);

    let dir = tmp_dir("lapsed-leaf");
    let mut config = EdgeIngestConfig::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()], 5_000_000_000);
    config.intermediate_chain_pem = Some(trusted.chain_pem());
    // Start the injected clock comfortably INSIDE the leaf's one-minute window.
    let clock = Arc::new(AtomicI64::new(tai_ns_for_unix(leaf_not_before + 30)));
    let (mut client, service) = start_server_with_anchors(&dir, config, &trusted.root_pem(), clock.clone()).await;

    let escaped = forwarded_cert::percent_encode_pem_like_nginx(&leaf_cert.to_pem().unwrap());
    client.set_forwarded_client_cert(escaped);

    let ack_inside_window = client.announce(manifest("edge-plugin-1")).await.unwrap();
    assert!(ack_inside_window.accepted, "a leaf must be accepted while the injected clock is inside its validity window: {ack_inside_window:?}");

    // Move the clock PAST notAfter -- by hand, never by sleeping (question 199's sibling
    // rule for this track) -- and announce again (a fresh producer_id, since the first
    // one already has an accepted manifest and MANIFEST_MISMATCH would otherwise mask
    // the identity refusal this test means to isolate).
    clock.store(tai_ns_for_unix(leaf_not_after + 30), Ordering::SeqCst);
    let ack_after_expiry = client.announce(manifest("edge-plugin-2")).await.unwrap();
    assert!(!ack_after_expiry.accepted, "{ack_after_expiry:?}");
    assert_eq!(ack_after_expiry.refusal, pb::ManifestRefusal::IdentityRefused as i32, "{ack_after_expiry:?}");

    let evidence = service.evidence_snapshot();
    let identity = evidence.identity.expect("EvidenceResponse.identity must be present");
    assert_eq!(identity.expired, 1, "expired must increment by exactly one, read back via GetEvidence's own field");
    assert_eq!(identity.accepted, 1, "exactly the first, inside-window Announce must have been counted as accepted");
    assert_eq!(identity.issuer_not_trusted, 0);
}

/// Starts an `EdgeIngestService` (with `require_client_certificate` forced `true` --
/// every test in this file is specifically about that path) trusting `root_pem` as its
/// only anchor, and a `Clock` reading `clock`'s current value -- the test moves `clock`
/// by hand between calls, never sleeping (question 199's sibling rule).
async fn start_server_with_anchors(dir: &std::path::Path, mut config: EdgeIngestConfig, root_pem: &[u8], clock: Arc<AtomicI64>) -> (EdgeIngestClient, Arc<EdgeIngestService>) {
    config.require_client_certificate = true;
    let anchors = av_edge::identity::TrustAnchors::from_root_pem(root_pem).unwrap();
    let service = Arc::new(EdgeIngestService::new(dir, Some(anchors), config, Arc::new(move || clock.load(Ordering::SeqCst))));
    let listener = av_ingest::server::bind_loopback("127.0.0.1:0").await.expect("127.0.0.1:0 must bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let served = service.clone();
    tokio::spawn(async move {
        let _ = Server::builder().add_service(EdgeIngestServer::from_arc(served)).serve_with_incoming(incoming).await;
    });
    poll_until_ready(addr).await;
    let client = EdgeIngestClient::connect_plaintext_addr(addr).await.unwrap();
    (client, service)
}

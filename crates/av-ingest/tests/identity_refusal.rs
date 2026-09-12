//! Requirement 2: a leaf from an unrelated CA (a hermetic, in-process two-tier CA, built
//! directly with the `openssl` crate the way `crates/av-edge/tests/identity.rs` builds
//! its own -- no fixture files, no network) is counted as an identity refusal and never
//! appended, through `av_ingest::ingest::Ingest::submit`'s own public API (never a wire --
//! see `av_ingest`'s `lib.rs` module doc).

use std::path::PathBuf;

use av_edge::identity::TrustAnchors;
use av_edge::policy::ProducerPolicy;
use av_edge::{hash, pb, sign};
use av_ingest::ingest::{Ingest, IngestOutcome, Signer};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::x509::extension::{BasicConstraints, KeyUsage};
use openssl::x509::{X509Name, X509NameBuilder, X509};

// ---------------------------------------------------------------------------------------
// Minimal hermetic two-tier CA builder (mirrors crates/av-edge/tests/identity.rs's own,
// trimmed to only what this one test needs: a root+intermediate hierarchy and a leaf).
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

fn now_tai_ns() -> i64 {
    av_cdm::time::Tai::from_utc_nanos(NOW_UNIX * 1_000_000_000).as_nanos()
}

struct Hierarchy {
    root_cert: X509,
    intermediate_cert: X509,
    intermediate_key: EcKey<Private>,
}

impl Hierarchy {
    fn new(label: &str) -> Self {
        let root_key = p384_key();
        let root_pkey = PKey::from_ec_key(root_key.clone()).unwrap();
        let mut rb = X509::builder().unwrap();
        rb.set_version(2).unwrap();
        let root_name = subject(&format!("{label} Root"));
        rb.set_subject_name(&root_name).unwrap();
        rb.set_issuer_name(&root_name).unwrap();
        rb.set_pubkey(&root_pkey).unwrap();
        rb.set_not_before(&Asn1Time::from_unix(WINDOW_START).unwrap()).unwrap();
        rb.set_not_after(&Asn1Time::from_unix(WINDOW_END).unwrap()).unwrap();
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
        ib.set_not_before(&Asn1Time::from_unix(WINDOW_START).unwrap()).unwrap();
        ib.set_not_after(&Asn1Time::from_unix(WINDOW_END).unwrap()).unwrap();
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

    fn issue_leaf(&self, cn: &str) -> (X509, EcKey<Private>) {
        let leaf_key = p384_key();
        let leaf_pkey = PKey::from_ec_key(leaf_key.clone()).unwrap();
        let mut lb = X509::builder().unwrap();
        lb.set_version(2).unwrap();
        lb.set_subject_name(&subject(cn)).unwrap();
        lb.set_issuer_name(self.intermediate_cert.subject_name()).unwrap();
        lb.set_pubkey(&leaf_pkey).unwrap();
        lb.set_not_before(&Asn1Time::from_unix(WINDOW_START).unwrap()).unwrap();
        lb.set_not_after(&Asn1Time::from_unix(WINDOW_END).unwrap()).unwrap();
        lb.set_serial_number(&serial(10)).unwrap();
        lb.append_extension(BasicConstraints::new().critical().build().unwrap()).unwrap();
        lb.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
        let intermediate_pkey = PKey::from_ec_key(self.intermediate_key.clone()).unwrap();
        lb.sign(&intermediate_pkey, MessageDigest::sha256()).unwrap();
        (lb.build(), leaf_key)
    }
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-ingest-test-identity-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn policy() -> ProducerPolicy {
    ProducerPolicy::new(
        "sim-asset-1",
        "CUI",
        vec![],
        vec!["UNCLASSIFIED".to_string(), "CUI".to_string()],
        "CUI",
        10_000_000_000,
    )
    .unwrap()
}

#[test]
fn a_leaf_from_an_unrelated_ca_is_counted_as_an_identity_refusal_and_never_appended() {
    let trusted = Hierarchy::new("Trusted");
    let foreign = Hierarchy::new("Foreign");

    let dir = tmp_dir("unrelated-ca");
    let anchors = TrustAnchors::from_root_pem(&trusted.root_pem()).unwrap();
    let mut ingest = Ingest::new(&dir, Some(anchors));
    ingest.register_producer(policy());

    let (foreign_leaf, foreign_key) = foreign.issue_leaf("edge.localhost");
    let now = now_tai_ns();

    let mut batch = pb::MeasurementBatch {
        producer_id: "sim-asset-1".to_string(),
        sequence: 1,
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        batch_tai_ns: now,
        shard_key: "shard-a".to_string(),
        ..Default::default()
    };
    sign::sign_batch(&mut batch, hash::GENESIS, &foreign_key).unwrap();

    let identity_before = ingest.identity_counters();
    let leaf_pem = foreign_leaf.to_pem().unwrap();
    let chain_pem = foreign.chain_pem();

    let outcome = ingest.submit(&batch, Signer::Certificate { leaf_pem: &leaf_pem, chain_pem: Some(&chain_pem) }, now).unwrap();

    match &outcome {
        IngestOutcome::IdentityRejected(e) => {
            assert!(matches!(e, av_edge::identity::IdentityRejection::IssuerNotTrusted(_)), "{e:?}");
        }
        other => panic!("expected IdentityRejected, got {other:?}"),
    }

    let identity_after = ingest.identity_counters();
    assert_eq!(identity_after.issuer_not_trusted, identity_before.issuer_not_trusted + 1, "issuer_not_trusted must increment by exactly one");
    assert_eq!(identity_after.accepted, identity_before.accepted, "accepted must not change");

    // Never appended: the producer's own chain counters must be entirely untouched (this
    // batch never reached ChainVerifier at all), and no partition file was ever created.
    let counters = ingest.producer_counters("sim-asset-1");
    assert_eq!(counters.accepted, 0);
    assert_eq!(counters, pb::RejectionCounters { producer_id: "sim-asset-1".to_string(), chain_head: hash::GENESIS.to_vec(), ..Default::default() });
    assert!(!dir.join("shard-a.avlog").exists(), "an identity-refused batch must never create or grow a partition file");
}

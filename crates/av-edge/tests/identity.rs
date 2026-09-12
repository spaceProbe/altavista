//! `docs/edge-plan.md` milestone E2, Rust test requirements 1-7: a hermetic, in-process
//! two-tier CA (root, intermediate, leaves -- including a second, unrelated root, and a
//! one-minute-lifetime leaf), built directly with the `openssl` crate so these tests are
//! fast and never skip, unlike `tests/test_edge_identity_seccert.py`'s real seccert+lego
//! loop.
//!
//! Every helper below builds certificates by hand (`X509Builder`, `EcKey::generate` on
//! `Nid::SECP384R1`, `X509NameBuilder`, `Asn1Time::from_unix`, `BasicConstraints`/
//! `KeyUsage` extensions) -- no fixture files, no network, no shelling out to the
//! `openssl` CLI.

use av_cdm::time::Tai;
use av_edge::identity::{self, EdgeIdentity, IdentityCounters, IdentityRejection, TrustAnchors};
use av_edge::{hash, sign};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{HasPublic, PKey, Private};
use openssl::sha::sha256;
use openssl::x509::extension::{BasicConstraints, KeyUsage};
use openssl::x509::{X509Name, X509NameBuilder, X509};

// ---------------------------------------------------------------------------------------
// Hermetic two-tier CA builder.
// ---------------------------------------------------------------------------------------

fn p384_key() -> EcKey<Private> {
    EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap()
}

fn p256_key() -> EcKey<Private> {
    EcKey::generate(&EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap()).unwrap()
}

fn subject(cn: &str) -> X509Name {
    let mut b = X509NameBuilder::new().unwrap();
    b.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    b.build()
}

fn serial(n: u32) -> Asn1Integer {
    BigNum::from_u32(n).unwrap().to_asn1_integer().unwrap()
}

/// A self-signed Root: `CA:true`, `keyCertSign`/`cRLSign`, signed with its own key.
fn make_root(cn: &str, key: &EcKey<Private>, not_before: i64, not_after: i64) -> X509 {
    let pkey = PKey::from_ec_key(key.clone()).unwrap();
    let name = subject(cn);
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_subject_name(&name).unwrap();
    b.set_issuer_name(&name).unwrap();
    b.set_pubkey(&pkey).unwrap();
    b.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
    b.set_serial_number(&serial(1)).unwrap();
    b.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
    b.sign(&pkey, MessageDigest::sha256()).unwrap();
    b.build()
}

/// An Intermediate: `CA:true, pathlen:0`, signed by `issuer_cert`/`issuer_key`.
fn make_intermediate(cn: &str, key: &EcKey<Private>, issuer_cert: &X509, issuer_key: &EcKey<Private>, not_before: i64, not_after: i64) -> X509 {
    let pkey = PKey::from_ec_key(key.clone()).unwrap();
    let issuer_pkey = PKey::from_ec_key(issuer_key.clone()).unwrap();
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_subject_name(&subject(cn)).unwrap();
    b.set_issuer_name(issuer_cert.subject_name()).unwrap();
    b.set_pubkey(&pkey).unwrap();
    b.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
    b.set_serial_number(&serial(2)).unwrap();
    b.append_extension(BasicConstraints::new().critical().ca().pathlen(0).build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
    b.sign(&issuer_pkey, MessageDigest::sha256()).unwrap();
    b.build()
}

/// A leaf, signed by `issuer_cert`/`issuer_key`, carrying `leaf_pubkey` (deliberately a
/// generic `HasPublic` key, not necessarily EC P-384 -- test group 5 issues a P-256 leaf
/// this way to prove the wrong-curve check actually runs against a leaf that otherwise
/// chains perfectly).
fn make_leaf<T: HasPublic>(cn: &str, leaf_pubkey: &PKey<T>, issuer_cert: &X509, issuer_key: &EcKey<Private>, serial_n: u32, not_before: i64, not_after: i64) -> X509 {
    let issuer_pkey = PKey::from_ec_key(issuer_key.clone()).unwrap();
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_subject_name(&subject(cn)).unwrap();
    b.set_issuer_name(issuer_cert.subject_name()).unwrap();
    b.set_pubkey(leaf_pubkey).unwrap();
    b.set_not_before(&Asn1Time::from_unix(not_before).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::from_unix(not_after).unwrap()).unwrap();
    b.set_serial_number(&serial(serial_n)).unwrap();
    b.append_extension(BasicConstraints::new().critical().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
    b.sign(&issuer_pkey, MessageDigest::sha256()).unwrap();
    b.build()
}

/// One full root+intermediate hierarchy, everything an ordinary (non-adversarial) test
/// needs to issue leaves under.
struct Hierarchy {
    root_cert: X509,
    intermediate_cert: X509,
    intermediate_key: EcKey<Private>,
}

impl Hierarchy {
    fn new(label: &str, not_before: i64, not_after: i64) -> Self {
        let root_key = p384_key();
        let root_cert = make_root(&format!("{label} Root"), &root_key, not_before, not_after);
        let intermediate_key = p384_key();
        let intermediate_cert = make_intermediate(&format!("{label} Intermediate"), &intermediate_key, &root_cert, &root_key, not_before, not_after);
        Self { root_cert, intermediate_cert, intermediate_key }
    }

    fn root_pem(&self) -> Vec<u8> {
        self.root_cert.to_pem().unwrap()
    }

    fn chain_pem(&self) -> Vec<u8> {
        self.intermediate_cert.to_pem().unwrap()
    }

    fn anchors(&self) -> TrustAnchors {
        TrustAnchors::from_root_pem(&self.root_pem()).unwrap()
    }

    /// Issues a P-384 leaf valid for the same window as this hierarchy's own certs.
    fn issue_leaf(&self, cn: &str, serial_n: u32) -> (X509, EcKey<Private>) {
        self.issue_leaf_with_window(cn, serial_n, unix(WINDOW_START), unix(WINDOW_END))
    }

    fn issue_leaf_with_window(&self, cn: &str, serial_n: u32, not_before: i64, not_after: i64) -> (X509, EcKey<Private>) {
        let leaf_key = p384_key();
        let leaf_pkey = PKey::from_ec_key(leaf_key.clone()).unwrap();
        let leaf_cert = make_leaf(cn, &leaf_pkey, &self.intermediate_cert, &self.intermediate_key, serial_n, not_before, not_after);
        (leaf_cert, leaf_key)
    }
}

/// A wide, comfortably-valid window every "ordinary" cert in these tests uses, so tests
/// that are not specifically about time never have to think about it. Chosen well inside
/// `av_cdm::time`'s post-1972 leap-second table.
const WINDOW_START: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z
const WINDOW_END: i64 = 1_900_000_000; // 2030-03-11T05:53:20Z
const NOW_UNIX: i64 = 1_800_000_000; // comfortably inside [WINDOW_START, WINDOW_END]

fn unix(u: i64) -> i64 {
    u
}

fn tai_ns_for_unix(unix_seconds: i64) -> i64 {
    Tai::from_utc_nanos(unix_seconds.checked_mul(1_000_000_000).unwrap()).as_nanos()
}

fn now_tai_ns() -> i64 {
    tai_ns_for_unix(NOW_UNIX)
}

/// SHA-256 of `cert`'s DER encoding, lowercase hex, computed independently of
/// `av_edge::identity`'s own fingerprinting (`openssl::sha::sha256` directly over
/// `to_der()`, plus this file's own hex formatting) -- requirement 1's "assert against a
/// fingerprint you compute independently in the test... not from your own helper calling
/// itself".
fn independent_fingerprint_hex(cert: &X509) -> String {
    let der = cert.to_der().unwrap();
    let digest = sha256(&der);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn assert_accepted(result: &Result<EdgeIdentity, IdentityRejection>) -> &EdgeIdentity {
    match result {
        Ok(identity) => identity,
        Err(e) => panic!("expected acceptance, got {e:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// Requirement 1: a leaf issued by the (in-process) root verifies, and its fingerprint
// matches an independently-computed one.
// ---------------------------------------------------------------------------------------

#[test]
fn leaf_issued_by_the_root_verifies_and_fingerprint_matches_independent_computation() {
    let hierarchy = Hierarchy::new("R1", WINDOW_START, WINDOW_END);
    let (leaf_cert, _leaf_key) = hierarchy.issue_leaf("edge.localhost", 10);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, now_tai_ns(), &mut counters);
    let verified = assert_accepted(&result);

    assert_eq!(verified.fingerprint_sha256, independent_fingerprint_hex(&leaf_cert));
    assert_eq!(verified.subject_cn, "edge.localhost");
    assert_eq!(counters.accepted, 1);
    assert_eq!(counters, IdentityCounters { accepted: 1, ..IdentityCounters::default() });
}

// ---------------------------------------------------------------------------------------
// Requirement 2: a batch signed with the issued leaf's key verifies with the public key
// extracted from the certificate, proven against E1's `verify::verify_batch`.
// ---------------------------------------------------------------------------------------

#[test]
fn batch_signed_with_the_issued_leaf_key_verifies_with_the_certificates_public_key() {
    let hierarchy = Hierarchy::new("R2", WINDOW_START, WINDOW_END);
    let (leaf_cert, leaf_key) = hierarchy.issue_leaf("edge.localhost", 10);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, now_tai_ns(), &mut counters);
    let verified = assert_accepted(&result);

    let mut batch = av_edge::pb::MeasurementBatch { producer_id: "sim-asset-1".to_string(), sequence: 1, batch_tai_ns: now_tai_ns(), ..Default::default() };
    sign::sign_batch_with_signer(&mut batch, hash::GENESIS, &leaf_key, &verified.fingerprint_sha256).expect("signing with the leaf's own key must succeed");

    // Direct E1 check: verify_batch against the public key av_edge::identity extracted.
    av_edge::verify::verify_batch(&batch, &verified.public_key).expect("a batch signed with the leaf's key must verify under the certificate's own public key");

    // The E1/E2 bridge: fingerprint match plus signature verification together.
    identity::verify_batch_signed_by(verified, &batch).expect("verify_batch_signed_by must accept a correctly signed, correctly fingerprinted batch");
}

// ---------------------------------------------------------------------------------------
// Requirement 3: a leaf from a second, unrelated root is refused as issuer-not-trusted,
// and only that counter moves.
// ---------------------------------------------------------------------------------------

#[test]
fn leaf_from_an_unrelated_second_root_is_refused_and_only_that_counter_moves() {
    let trusted = Hierarchy::new("R3-trusted", WINDOW_START, WINDOW_END);
    let foreign = Hierarchy::new("R3-foreign", WINDOW_START, WINDOW_END);
    let (foreign_leaf, _foreign_key) = foreign.issue_leaf("foreign.localhost", 10);

    let anchors = trusted.anchors(); // only the first hierarchy's Root is trusted
    let mut counters = IdentityCounters::new();

    let result = identity::verify_identity(&foreign_leaf.to_pem().unwrap(), Some(&foreign.chain_pem()), &anchors, now_tai_ns(), &mut counters);

    assert!(matches!(result, Err(IdentityRejection::IssuerNotTrusted(_))), "{result:?}");
    assert_eq!(counters, IdentityCounters { issuer_not_trusted: 1, ..IdentityCounters::default() }, "exactly one counter (issuer_not_trusted) must have moved");
}

// ---------------------------------------------------------------------------------------
// Requirement 4: a one-minute-wide validity window is accepted inside and refused as
// expired after, with the clock injected (never slept).
// ---------------------------------------------------------------------------------------

const SHORT_LEAF_NOT_BEFORE: i64 = 2_000_000_000;
const SHORT_LEAF_NOT_AFTER: i64 = SHORT_LEAF_NOT_BEFORE + 60; // one minute wide

#[test]
fn one_minute_leaf_is_accepted_at_an_injected_clock_inside_its_window() {
    let hierarchy = Hierarchy::new("R4", WINDOW_START, SHORT_LEAF_NOT_AFTER + 3600);
    let (leaf_cert, _leaf_key) = hierarchy.issue_leaf_with_window("short.localhost", 10, SHORT_LEAF_NOT_BEFORE, SHORT_LEAF_NOT_AFTER);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let inside = tai_ns_for_unix(SHORT_LEAF_NOT_BEFORE + 30); // 30s into the 60s window
    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, inside, &mut counters);
    assert_accepted(&result);
    assert_eq!(counters.accepted, 1);
}

#[test]
fn one_minute_leaf_is_refused_as_expired_at_an_injected_clock_after_its_window() {
    let hierarchy = Hierarchy::new("R4b", WINDOW_START, SHORT_LEAF_NOT_AFTER + 3600);
    let (leaf_cert, _leaf_key) = hierarchy.issue_leaf_with_window("short.localhost", 10, SHORT_LEAF_NOT_BEFORE, SHORT_LEAF_NOT_AFTER);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let after = tai_ns_for_unix(SHORT_LEAF_NOT_AFTER + 5); // 5s past notAfter
    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, after, &mut counters);
    assert!(matches!(result, Err(IdentityRejection::Expired { .. })), "{result:?}");
    assert_eq!(counters, IdentityCounters { expired: 1, ..IdentityCounters::default() });
}

#[test]
fn leaf_is_refused_as_not_yet_valid_at_an_injected_clock_before_its_window() {
    let hierarchy = Hierarchy::new("R4c", WINDOW_START, SHORT_LEAF_NOT_AFTER + 3600);
    let (leaf_cert, _leaf_key) = hierarchy.issue_leaf_with_window("short.localhost", 10, SHORT_LEAF_NOT_BEFORE, SHORT_LEAF_NOT_AFTER);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let before = tai_ns_for_unix(SHORT_LEAF_NOT_BEFORE - 10);
    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, before, &mut counters);
    assert!(matches!(result, Err(IdentityRejection::NotYetValid { .. })), "{result:?}");
    assert_eq!(counters, IdentityCounters { not_yet_valid: 1, ..IdentityCounters::default() });
}

// ---------------------------------------------------------------------------------------
// Requirement 5: a leaf that is not P-384 is refused with the P-384 rejection.
// ---------------------------------------------------------------------------------------

#[test]
fn non_p384_leaf_is_refused_with_the_not_p384_rejection() {
    let hierarchy = Hierarchy::new("R5", WINDOW_START, WINDOW_END);
    let p256 = p256_key();
    let p256_pkey = PKey::from_ec_key(p256).unwrap();
    // Otherwise perfectly valid: signed by the trusted Intermediate, well inside its
    // window -- only the leaf's own key's curve is wrong.
    let leaf_cert = make_leaf("wrong-curve.localhost", &p256_pkey, &hierarchy.intermediate_cert, &hierarchy.intermediate_key, 10, WINDOW_START, WINDOW_END);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&hierarchy.chain_pem()), &anchors, now_tai_ns(), &mut counters);
    assert!(matches!(result, Err(IdentityRejection::NotP384 { actual: Some(Nid::X9_62_PRIME256V1) })), "{result:?}");
    assert_eq!(counters, IdentityCounters { not_p384: 1, ..IdentityCounters::default() });
}

// ---------------------------------------------------------------------------------------
// Requirement 6: malformed PEM bytes are refused, not panicked on.
// ---------------------------------------------------------------------------------------

#[test]
fn malformed_pem_is_refused_not_panicked_on() {
    let hierarchy = Hierarchy::new("R6", WINDOW_START, WINDOW_END);
    let anchors = hierarchy.anchors();
    let mut counters = IdentityCounters::new();

    let result = identity::verify_identity(b"this is not a certificate", None, &anchors, now_tai_ns(), &mut counters);
    assert!(matches!(result, Err(IdentityRejection::MalformedPem(_))), "{result:?}");
    assert_eq!(counters, IdentityCounters { malformed_pem: 1, ..IdentityCounters::default() });

    // Also: TrustAnchors itself must refuse malformed PEM rather than panicking.
    let bad_anchors = TrustAnchors::from_root_pem(b"also not a certificate");
    match bad_anchors {
        Err(IdentityRejection::MalformedPem(_)) => {}
        Err(other) => panic!("expected MalformedPem, got {other:?}"),
        Ok(_) => panic!("expected TrustAnchors::from_root_pem to refuse garbage PEM, but it succeeded"),
    }
}

// ---------------------------------------------------------------------------------------
// Requirement 7: the documented check order, asserted on leaves carrying two defects
// each at once (`src/identity.rs`'s module doc: MalformedPem -> NotP384 ->
// IssuerNotTrusted -> Expired/NotYetValid).
// ---------------------------------------------------------------------------------------

#[test]
fn check_order_not_p384_wins_over_issuer_not_trusted() {
    // A leaf that is BOTH the wrong curve AND issued under a root that is not trusted at
    // all -- the module doc says the curve check (step 2) runs before any chain walk
    // (step 3), so NotP384 must win.
    let foreign = Hierarchy::new("R7a-foreign", WINDOW_START, WINDOW_END);
    let unrelated_trusted = Hierarchy::new("R7a-trusted", WINDOW_START, WINDOW_END);
    let p256 = p256_key();
    let p256_pkey = PKey::from_ec_key(p256).unwrap();
    let leaf_cert = make_leaf("double-defect.localhost", &p256_pkey, &foreign.intermediate_cert, &foreign.intermediate_key, 10, WINDOW_START, WINDOW_END);

    let anchors = unrelated_trusted.anchors(); // does not trust `foreign` at all
    let mut counters = IdentityCounters::new();
    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&foreign.chain_pem()), &anchors, now_tai_ns(), &mut counters);

    assert!(matches!(result, Err(IdentityRejection::NotP384 { .. })), "NotP384 must be checked (and win) before IssuerNotTrusted: {result:?}");
    assert_eq!(counters, IdentityCounters { not_p384: 1, ..IdentityCounters::default() });
}

#[test]
fn check_order_issuer_not_trusted_wins_over_expired() {
    // A leaf that is BOTH from an unrelated root AND already expired -- the module doc
    // says the chain-trust check (step 3) runs, and must fail, before the leaf's own
    // notBefore/notAfter are ever consulted (step 4), so IssuerNotTrusted must win.
    let trusted = Hierarchy::new("R7b-trusted", WINDOW_START, WINDOW_END);
    let foreign = Hierarchy::new("R7b-foreign", WINDOW_START, SHORT_LEAF_NOT_AFTER + 3600);
    let (expired_foreign_leaf, _key) = foreign.issue_leaf_with_window("double-defect.localhost", 10, SHORT_LEAF_NOT_BEFORE, SHORT_LEAF_NOT_AFTER);

    let anchors = trusted.anchors(); // does not trust `foreign` at all
    let mut counters = IdentityCounters::new();
    let after_expiry = tai_ns_for_unix(SHORT_LEAF_NOT_AFTER + 5);
    let result = identity::verify_identity(&expired_foreign_leaf.to_pem().unwrap(), Some(&foreign.chain_pem()), &anchors, after_expiry, &mut counters);

    assert!(matches!(result, Err(IdentityRejection::IssuerNotTrusted(_))), "IssuerNotTrusted must be checked (and win) before Expired: {result:?}");
    assert_eq!(counters, IdentityCounters { issuer_not_trusted: 1, ..IdentityCounters::default() });
}

// ---------------------------------------------------------------------------------------
// Edge manager's E2 review: a lapsed *issuer* under a still-valid leaf is
// `IssuerNotTrusted`, not `Expired`.
//
// An earlier draft of `verify_identity_inner` reached this case by falling through to
// `Expired` on the reasoning that "the chain builds and the leaf is not yet-to-be-valid,
// so it must be expired" -- which is untrue when it is the Intermediate (or the Root)
// whose window has lapsed while the leaf's has not. The refusal itself was never in
// doubt (nothing is accepted unless the time-injected chain walk passes), but the
// reported reason named a leaf that was perfectly valid and moved the `expired` counter,
// and a counter that attributes a defect to the wrong certificate is the kind of silent
// misreport ADR-004's "everything rejected is counted, never silent" exists to prevent.
// ---------------------------------------------------------------------------------------

#[test]
fn a_lapsed_issuer_under_a_still_valid_leaf_is_issuer_not_trusted_not_expired() {
    // Root and Intermediate lapse well before NOW_UNIX; the leaf's own window contains it.
    let lapsed_issuers = Hierarchy::new("R8", WINDOW_START, NOW_UNIX - 10_000);
    let (leaf_cert, _leaf_key) = lapsed_issuers.issue_leaf_with_window("still-valid.localhost", 10, unix(WINDOW_START), unix(WINDOW_END));

    // The leaf really is inside its own window at the verification time, so nothing about
    // the leaf itself is wrong -- this test would be vacuous otherwise. Asserted against
    // the certificate's own encoded notBefore/notAfter rather than against the constants
    // passed in, so it still holds if leaf issuance ever changes what it does with them.
    // This is also precisely the condition under which the pre-fix fall-through produced a
    // wrong `Expired`: the chain builds, the leaf is not yet-to-be-valid, and the leaf has
    // not expired either, so "it must be expired" was the one conclusion that did not
    // follow.
    let at_now = Asn1Time::from_unix(NOW_UNIX).unwrap();
    assert!(leaf_cert.not_before() < at_now, "the leaf's notBefore must precede the verification time");
    assert!(leaf_cert.not_after() > at_now, "the leaf's notAfter must follow the verification time -- otherwise Expired would be the right answer and this test would prove nothing");

    let anchors = lapsed_issuers.anchors();
    let mut counters = IdentityCounters::new();
    let result = identity::verify_identity(&leaf_cert.to_pem().unwrap(), Some(&lapsed_issuers.chain_pem()), &anchors, now_tai_ns(), &mut counters);

    match &result {
        Err(IdentityRejection::IssuerNotTrusted(detail)) => {
            assert!(detail.contains("issuing certificate"), "the detail must point at the issuer, not the leaf: {detail:?}");
        }
        other => panic!("a lapsed issuer under a valid leaf must be IssuerNotTrusted, not {other:?}"),
    }
    assert_eq!(counters, IdentityCounters { issuer_not_trusted: 1, ..IdentityCounters::default() }, "the expired counter must NOT move: the leaf did not expire, an issuer did");
}

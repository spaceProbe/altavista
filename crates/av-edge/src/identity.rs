//! Identity verification for seccert-issued leaves (`docs/edge-plan.md` milestone E2;
//! `docs/adr/004-security-boundary-and-evidence.md`'s "Identity and trust": "machine
//! identity from `seccert`... the two-tier Root is the enclave trust anchor").
//!
//! This module answers exactly one question for one leaf certificate: does it chain to
//! the configured Root, and is it valid at a caller-injected instant? It does **not**
//! open a socket, does **not** perform an mTLS handshake, and does **not** read any
//! clock -- see `docs/edge-plan.md`'s "Decisions already taken by the manager" for why
//! E2 stops here and leaves a live transport to E3. What it hands back on success --
//! [`EdgeIdentity`]'s EC P-384 public key and fingerprint -- is exactly what
//! [`crate::verify::verify_batch`] and `MeasurementBatch.signer_cert_sha256` need, so E3
//! can wire "verify this connection's leaf" and "verify this batch's signature" together
//! with [`verify_batch_signed_by`] below.
//!
//! # The Root is the only trust anchor
//!
//! [`TrustAnchors`] is built **only** from the PEM(s) a caller explicitly supplies. It
//! never calls `X509StoreBuilder::set_default_paths` (which would pull in the host's
//! `/etc/ssl/certs`-style system trust store) and never consults `SSL_CERT_FILE`/
//! `SSL_CERT_DIR`. This is deliberate, not an oversight: ADR-004 names the two-tier Root
//! as *the* enclave trust anchor, and a plugin whose leaf happens to chain to some
//! unrelated CA the host trusts for an unrelated reason (a corporate proxy CA, a stale
//! LetsEncrypt root, whatever else `/etc/ssl/certs` accumulates) must be refused exactly
//! as hard as one presenting no certificate at all.
//!
//! [`TrustAnchors::from_pems`] accepts more than one PEM blob purely as a loading
//! mechanism (e.g. an old-Root/new-Root rotation window where both must validate) --
//! **not** so an Intermediate can be trusted directly instead of being checked through
//! the untrusted chain parameter below. In this platform's two-tier PKI the Intermediate
//! is never itself a trust anchor: it is presented alongside the leaf (`--chain
//! <issuer.pem>` on the CLI, lego's own naming for the file it writes) and its own
//! signature is checked against the Root during verification, exactly like the leaf's.
//!
//! # Check order
//!
//! [`verify_identity`] checks each leaf in this fixed order, stopping at the first
//! defect (mirrors `crate::chain`'s "one rejection reason per rejected batch" -- see that
//! module's doc for the general principle this restates for identities):
//!
//! 1. **`MalformedPem`** -- `leaf_pem` does not parse as an X.509 certificate at all, or
//!    has no usable public key.
//! 2. **`NotP384`** -- the leaf's public key parses, but is not an EC P-384 key. Checked
//!    before any chain walk: a wrong-curve key is refused on its own terms, without
//!    spending a signature-chain verification on it first.
//! 3. **`IssuerNotTrusted`** -- checked with the caller's clock *disabled*
//!    (`X509VerifyFlags::NO_CHECK_TIME`) so this step answers exactly one question, "does
//!    this leaf's issuer chain (leaf -> `chain_pem`'s Intermediate(s), if any -> Root)
//!    verify", independent of whether the leaf happens to also be expired. This keeps
//!    "wrong CA" and "right CA, wrong time" as two independently diagnosable defects
//!    instead of one combined OpenSSL error code that would need to be decoded after the
//!    fact.
//! 4. **`Expired`** / **`NotYetValid`** -- only once the chain itself is trusted: the
//!    leaf's own `notBefore`/`notAfter` (read directly off the certificate, converted to
//!    TAI nanoseconds -- see [`asn1_time_to_tai_ns`]) are compared against the caller's
//!    `now_tai_ns`. These two are mutually exclusive by construction (a certificate
//!    cannot be simultaneously not-yet-valid and expired), so there is no ordering
//!    question between them. If the chain builds, the time-injected walk still failed, and
//!    the *leaf's* own window does contain `now_tai_ns`, then what lapsed is an issuing
//!    certificate rather than the leaf: that case is reported as `IssuerNotTrusted` with a
//!    detail saying so, not as `Expired` against a leaf that is perfectly valid (edge
//!    manager's review of E2 -- see `verify_identity_inner`'s own comment).
//!
//! `tests/identity.rs`'s `check_order_*` tests construct a leaf carrying two defects at
//! once (e.g. a non-P-384 key that is also expired) and assert the earlier one above
//! wins, the same pattern `crate::chain`'s own `check_order_*` tests use.
//!
//! # Injecting a clock: TAI nanoseconds -> `time_t`
//!
//! OpenSSL's certificate-time checks work in Unix `time_t` seconds
//! (`X509VerifyParam::set_time`); this platform's clocks are TAI nanoseconds
//! (`av_cdm::time::Tai`, ADR-001). [`tai_ns_to_unix_time_t`] does the one conversion this
//! module needs, in the same direction `Tai::to_utc_nanos` already documents: TAI
//! nanoseconds -> this crate's proleptic-UTC nanoseconds since the Unix epoch (no
//! leap-second table lookup beyond what `Tai::to_utc_nanos` already does) -> whole
//! seconds, truncated towards negative infinity (`div_euclid`) rather than toward zero,
//! so a caller-supplied instant one nanosecond into a given second is never rounded up
//! into the next one. [`asn1_time_to_tai_ns`] is the inverse direction, used to read a
//! certificate's own `notBefore`/`notAfter` back into TAI nanoseconds for the `Expired`/
//! `NotYetValid` comparison in step 4 above; it goes through `Asn1TimeRef::diff` against
//! the Unix epoch rather than any string parsing, since `Asn1Time` exposes no direct
//! "as `time_t`" accessor in this crate's pinned `openssl` version (0.10.81).
//!
//! Nothing in this module reads a live clock (question 199): `now_tai_ns` always comes
//! from the caller, exactly like `crate::chain::ChainVerifier::submit`'s `now_tai_ns`.

use openssl::asn1::{Asn1Time, Asn1TimeRef};
use openssl::ec::EcKey;
use openssl::error::ErrorStack;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::Public;
use openssl::stack::Stack;
use openssl::x509::store::{X509Store, X509StoreBuilder};
use openssl::x509::verify::X509VerifyFlags;
use openssl::x509::{X509StoreContext, X509};

use av_cdm::time::Tai;

use crate::pb;
use crate::verify::VerifyError;

/// What can go wrong verifying a leaf against a [`TrustAnchors`]. Every variant is a
/// refusal a caller must count via [`IdentityCounters`], never silently swallow
/// (ADR-004: "everything rejected is counted") -- see the module doc for the fixed check
/// order these are produced in.
#[derive(Debug, thiserror::Error)]
pub enum IdentityRejection {
    /// `leaf_pem` (or a PEM this function also had to parse -- `chain_pem`, or a
    /// [`TrustAnchors`] input) did not parse as an X.509 certificate, or the certificate
    /// has no usable public key.
    #[error("could not parse a PEM certificate: {0}")]
    MalformedPem(String),
    /// The leaf's public key parsed, but is not an EC P-384 (secp384r1) key.
    #[error("leaf's public key is on curve {actual:?}, not P-384 (secp384r1) -- refusing")]
    NotP384 { actual: Option<Nid> },
    /// The leaf's issuer chain (through `chain_pem`'s Intermediate(s), if any) does not
    /// verify against the configured [`TrustAnchors`], independent of validity time
    /// (checked with `X509VerifyFlags::NO_CHECK_TIME` -- see the module doc).
    #[error("leaf's issuer chain does not verify against the configured Root: {0}")]
    IssuerNotTrusted(String),
    /// The chain verifies, but `now_tai_ns` is after the leaf's own `notAfter`.
    #[error("leaf expired at notAfter={not_after_tai_ns} TAI ns; verification time was {now_tai_ns} TAI ns")]
    Expired { not_after_tai_ns: i64, now_tai_ns: i64 },
    /// The chain verifies, but `now_tai_ns` is before the leaf's own `notBefore`.
    #[error("leaf is not yet valid: notBefore={not_before_tai_ns} TAI ns; verification time was {now_tai_ns} TAI ns")]
    NotYetValid { not_before_tai_ns: i64, now_tai_ns: i64 },
    /// An OpenSSL operation that is not itself a verification failure -- an actual
    /// `ErrorStack` (e.g. out of memory) -- distinguished from every check above the way
    /// `crate::sign::SigningError::Openssl`/`crate::verify::VerifyError::Openssl` already
    /// distinguish an operational failure from a refusal.
    #[error("OpenSSL operation failed: {0}")]
    Openssl(String),
}

impl From<ErrorStack> for IdentityRejection {
    fn from(e: ErrorStack) -> Self {
        IdentityRejection::Openssl(e.to_string())
    }
}

/// One counter per [`IdentityRejection`] kind plus `accepted`, incremented by exactly
/// one per call to [`verify_identity`], either way (ADR-004: "everything rejected is
/// counted, never silent"). E3 will decide how this is exposed on the wire (`docs/edge-
/// plan.md`'s "decisions already taken" -- no new proto message this round); this is a
/// plain Rust struct on purpose.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IdentityCounters {
    pub accepted: u64,
    pub malformed_pem: u64,
    pub not_p384: u64,
    pub issuer_not_trusted: u64,
    pub expired: u64,
    pub not_yet_valid: u64,
    pub openssl_error: u64,
}

impl IdentityCounters {
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&mut self, rejection: &IdentityRejection) {
        match rejection {
            IdentityRejection::MalformedPem(_) => self.malformed_pem += 1,
            IdentityRejection::NotP384 { .. } => self.not_p384 += 1,
            IdentityRejection::IssuerNotTrusted(_) => self.issuer_not_trusted += 1,
            IdentityRejection::Expired { .. } => self.expired += 1,
            IdentityRejection::NotYetValid { .. } => self.not_yet_valid += 1,
            IdentityRejection::Openssl(_) => self.openssl_error += 1,
        }
    }
}

/// The Root (and, in principle, any other explicitly-configured trust anchors -- see the
/// module doc for why that is a loading mechanism, not an invitation to trust an
/// Intermediate directly) as an `openssl` [`X509Store`]. Deliberately never falls back
/// to the system/OS trust store.
pub struct TrustAnchors {
    store: X509Store,
}

impl TrustAnchors {
    /// Builds trust anchors from one or more PEM blobs, each of which may itself contain
    /// more than one certificate (`X509::stack_from_pem`); every certificate found in
    /// every blob is added to the store as trusted. Refuses (as [`IdentityRejection::
    /// MalformedPem`]) a PEM that does not parse, or a call with zero certificates found
    /// across every blob given.
    pub fn from_pems(pems: &[&[u8]]) -> Result<Self, IdentityRejection> {
        let mut builder = X509StoreBuilder::new()?;
        let mut any = false;
        for pem in pems {
            let certs = X509::stack_from_pem(pem).map_err(|e| IdentityRejection::MalformedPem(e.to_string()))?;
            for cert in certs {
                any = true;
                builder.add_cert(cert)?;
            }
        }
        if !any {
            return Err(IdentityRejection::MalformedPem("no certificates found in the given PEM(s)".to_string()));
        }
        Ok(Self { store: builder.build() })
    }

    /// The common case: a single Root PEM (which may itself bundle more than one
    /// certificate, e.g. an old and a new Root during a rotation window). This crate
    /// never reads a file itself (this module's only side effect anywhere is ordinary
    /// heap allocation and the OpenSSL calls that need it, matching this crate's own
    /// module-doc promise) -- reading `--ca`'s PEM off disk is the CLI's/caller's job;
    /// see `src/bin/av-edge-identity.rs`.
    pub fn from_root_pem(root_pem: &[u8]) -> Result<Self, IdentityRejection> {
        Self::from_pems(&[root_pem])
    }

    /// One chain-verification call against this store, with `unix_time_t` (when given)
    /// injected via `X509VerifyParam::set_time` and `flags` applied to the same scratch
    /// param -- the manager's own probed recipe (`X509VerifyParam::set_time` +
    /// `X509StoreBuilder::set_param` ahead of `X509StoreContext::verify_cert`). A fresh
    /// `X509Store` is rebuilt from `self.store`'s own certificates for every call (rather
    /// than mutating `self.store`'s parameters in place) purely because this crate's
    /// pinned `openssl` version has no "verify with a per-call override" entry point
    /// other than parameters baked into the store itself -- concurrent callers, and the
    /// two calls [`verify_identity`] itself makes, therefore never interfere with each
    /// other. `unix_time_t = None` together with `flags` empty means "use whatever
    /// OpenSSL's own default time source is", which this module never actually relies on
    /// -- every call site below passes either an explicit time or
    /// `X509VerifyFlags::NO_CHECK_TIME` (which makes the time source moot), so no live
    /// clock read ever affects a result this module returns (question 199).
    fn verify_chain_with(&self, leaf: &X509, chain: &Stack<X509>, unix_time_t: Option<i64>, flags: X509VerifyFlags) -> Result<bool, ErrorStack> {
        let mut param = openssl::x509::verify::X509VerifyParam::new()?;
        param.set_flags(flags)?;
        if let Some(t) = unix_time_t {
            param.set_time(t);
        }
        let mut store_builder = X509StoreBuilder::new()?;
        for cert in self.store.all_certificates() {
            store_builder.add_cert(cert)?;
        }
        store_builder.set_param(&param)?;
        let store = store_builder.build();

        let mut ctx = X509StoreContext::new()?;
        ctx.init(&store, leaf, chain, |c| c.verify_cert())
    }
}

/// A verified leaf: the parts of it [`crate::verify::verify_batch`] and
/// `MeasurementBatch.signer_cert_sha256` need.
#[derive(Debug, Clone)]
pub struct EdgeIdentity {
    /// The leaf's EC P-384 public key -- pass `&identity.public_key` directly to
    /// [`crate::verify::verify_batch`].
    pub public_key: EcKey<Public>,
    /// SHA-256 of the leaf's **DER** encoding, lowercase hex (`openssl::x509::X509::
    /// digest`, i.e. `X509_digest`) -- what `openssl x509 -in leaf.pem -fingerprint
    /// -sha256` prints (colons and uppercase stripped/lowercased), so an operator can
    /// reproduce this value by hand from the same PEM file. This is exactly what
    /// `MeasurementBatch.signer_cert_sha256` carries.
    pub fingerprint_sha256: String,
    /// The leaf's subject Common Name, or the empty string if it has none. Informational
    /// only -- never itself a basis for accepting or refusing a leaf in this module.
    pub subject_cn: String,
    /// The leaf's own `notBefore`, TAI nanoseconds.
    pub not_before_tai_ns: i64,
    /// The leaf's own `notAfter`, TAI nanoseconds.
    pub not_after_tai_ns: i64,
}

/// TAI nanoseconds -> Unix `time_t` seconds, truncating towards negative infinity. See
/// the module doc's "Injecting a clock" section for exactly what this assumes.
fn tai_ns_to_unix_time_t(tai_ns: i64) -> i64 {
    let unix_ns = Tai::from_nanos(tai_ns).to_utc_nanos();
    unix_ns.div_euclid(1_000_000_000)
}

/// The inverse direction: an `Asn1TimeRef` (a certificate's `notBefore`/`notAfter`) ->
/// TAI nanoseconds, via [`Asn1TimeRef::diff`] against the Unix epoch rather than string
/// parsing (see the module doc for why). `openssl`'s `ASN1_TIME_diff(days, secs, from,
/// to)` reports `to - from`; passing the epoch as `from` and `t` as `to` therefore yields
/// exactly `t`'s own Unix-seconds offset.
fn asn1_time_to_tai_ns(t: &Asn1TimeRef) -> Result<i64, IdentityRejection> {
    let epoch = Asn1Time::from_unix(0)?;
    let diff = epoch.diff(t)?;
    let unix_secs = i64::from(diff.days) * 86_400 + i64::from(diff.secs);
    let unix_ns = unix_secs
        .checked_mul(1_000_000_000)
        .ok_or_else(|| IdentityRejection::Openssl(format!("certificate time {unix_secs} unix seconds overflows i64 nanoseconds")))?;
    Ok(Tai::from_utc_nanos(unix_ns).as_nanos())
}

fn subject_cn(cert: &X509) -> String {
    cert.subject_name()
        .entries_by_nid(Nid::COMMONNAME)
        .next()
        .and_then(|entry| entry.data().to_string().ok())
        .unwrap_or_default()
}

fn parse_chain_pem(chain_pem: Option<&[u8]>) -> Result<Stack<X509>, IdentityRejection> {
    let mut stack = Stack::new()?;
    if let Some(pem) = chain_pem {
        let certs = X509::stack_from_pem(pem).map_err(|e| IdentityRejection::MalformedPem(e.to_string()))?;
        for cert in certs {
            stack.push(cert)?;
        }
    }
    Ok(stack)
}

/// The actual check, run by [`verify_identity`] before it updates `counters`. See the
/// module doc for the fixed check order.
fn verify_identity_inner(leaf_pem: &[u8], chain_pem: Option<&[u8]>, anchors: &TrustAnchors, now_tai_ns: i64) -> Result<EdgeIdentity, IdentityRejection> {
    // 1. MalformedPem.
    let leaf = X509::from_pem(leaf_pem).map_err(|e| IdentityRejection::MalformedPem(e.to_string()))?;
    let leaf_pkey = leaf.public_key().map_err(|e| IdentityRejection::MalformedPem(format!("leaf has no usable public key: {e}")))?;

    // 2. NotP384.
    let ec_key = leaf_pkey.ec_key().map_err(|_| IdentityRejection::NotP384 { actual: None })?;
    let curve = ec_key.group().curve_name();
    if curve != Some(Nid::SECP384R1) {
        return Err(IdentityRejection::NotP384 { actual: curve });
    }

    // 3 & 4 together: the primary decision is one combined chain-and-time verification,
    // with `now_tai_ns` injected via `X509VerifyParam::set_time` (the manager's probed
    // recipe) -- this is what actually decides accept vs. refuse. If it refuses, a
    // second, time-*disabled* call (`X509VerifyFlags::NO_CHECK_TIME`) tells us whether
    // the chain itself was the problem (IssuerNotTrusted, step 3) or whether the chain is
    // fine and only the leaf's own validity window is (step 4, disambiguated into
    // Expired/NotYetValid by reading `notBefore`/`notAfter` directly).
    let chain = parse_chain_pem(chain_pem)?;
    let now_time_t = tai_ns_to_unix_time_t(now_tai_ns);
    let trusted_now = anchors.verify_chain_with(&leaf, &chain, Some(now_time_t), X509VerifyFlags::empty())?;

    if !trusted_now {
        let chain_ok_ignoring_time = anchors.verify_chain_with(&leaf, &chain, None, X509VerifyFlags::NO_CHECK_TIME)?;
        if !chain_ok_ignoring_time {
            return Err(IdentityRejection::IssuerNotTrusted(
                "the leaf's issuer chain does not verify against the configured Root, independent of validity time".to_string(),
            ));
        }
        let not_before_tai_ns = asn1_time_to_tai_ns(leaf.not_before())?;
        let not_after_tai_ns = asn1_time_to_tai_ns(leaf.not_after())?;
        if now_tai_ns < not_before_tai_ns {
            return Err(IdentityRejection::NotYetValid { not_before_tai_ns, now_tai_ns });
        }
        if now_tai_ns > not_after_tai_ns {
            return Err(IdentityRejection::Expired { not_after_tai_ns, now_tai_ns });
        }
        // The chain builds, and the *leaf's* own window contains `now_tai_ns` -- so the
        // certificate that was outside its validity window at `now_tai_ns` is one of the
        // issuers, not the leaf. An earlier draft of this function fell through to
        // `Expired` here, which named the leaf's own `notAfter` in the message and moved
        // the `expired` counter; both are wrong when the leaf is fine and it is the
        // Intermediate (or the Root) that has lapsed, and a counter that attributes a
        // defect to the wrong thing is exactly what ADR-004's "never silent" rule exists to
        // prevent. An issuer that is not valid at the verification time is not a trusted
        // issuer at that time, so this is `IssuerNotTrusted`, with a detail that says which
        // of the two untrusted-issuer shapes it is rather than sending an operator to look
        // at a leaf that is not the problem.
        return Err(IdentityRejection::IssuerNotTrusted(
            "the leaf's own validity window contains the verification time, so an issuing certificate in its chain is outside its own validity window at that time".to_string(),
        ));
    }

    let not_before_tai_ns = asn1_time_to_tai_ns(leaf.not_before())?;
    let not_after_tai_ns = asn1_time_to_tai_ns(leaf.not_after())?;
    let fingerprint_sha256 = crate::hash::hex_encode(leaf.digest(MessageDigest::sha256())?.as_ref());
    Ok(EdgeIdentity { public_key: ec_key, fingerprint_sha256, subject_cn: subject_cn(&leaf), not_before_tai_ns, not_after_tai_ns })
}

/// Verifies `leaf_pem` (optionally with `chain_pem`'s Intermediate(s) for path-building)
/// against `anchors`, at the caller-injected `now_tai_ns`, and bumps `counters` by
/// exactly one either way. See the module doc for the fixed check order.
pub fn verify_identity(leaf_pem: &[u8], chain_pem: Option<&[u8]>, anchors: &TrustAnchors, now_tai_ns: i64, counters: &mut IdentityCounters) -> Result<EdgeIdentity, IdentityRejection> {
    let result = verify_identity_inner(leaf_pem, chain_pem, anchors, now_tai_ns);
    match &result {
        Ok(_) => counters.accepted += 1,
        Err(e) => counters.bump(e),
    }
    result
}

/// What can go wrong tying a verified [`EdgeIdentity`] to a `MeasurementBatch`
/// ([`verify_batch_signed_by`]): the identity is fine on its own, but this specific batch
/// does not check out against it.
#[derive(Debug, thiserror::Error)]
pub enum BatchIdentityError {
    /// `batch.signer_cert_sha256` names a different fingerprint than the identity's own
    /// -- this batch was not signed under the certificate that was verified.
    #[error("batch.signer_cert_sha256 ({batch:?}) does not match the verified identity's own fingerprint ({identity:?})")]
    FingerprintMismatch { batch: String, identity: String },
    /// The fingerprint matched, but the batch's signature does not verify under the
    /// identity's public key (`crate::verify::verify_batch`).
    #[error(transparent)]
    Verify(#[from] VerifyError),
}

/// The E1/E2 bridge: given a verified `identity` and one `batch`, checks that the batch
/// declares this exact identity as its signer (`batch.signer_cert_sha256 ==
/// identity.fingerprint_sha256`) *before* spending a signature verification on it -- a
/// batch naming the wrong fingerprint is refused without ever calling
/// [`crate::verify::verify_batch`], since a fingerprint mismatch already means "this is
/// not the certificate whose key should have produced this signature" regardless of
/// whether the signature happens to verify under the identity's key anyway (e.g. a
/// replayed batch from a different, also-P-384, producer). On success, returns the same
/// recomputed 32-byte `batch_hash` [`crate::verify::verify_batch`] does.
pub fn verify_batch_signed_by(identity: &EdgeIdentity, batch: &pb::MeasurementBatch) -> Result<[u8; 32], BatchIdentityError> {
    if batch.signer_cert_sha256 != identity.fingerprint_sha256 {
        return Err(BatchIdentityError::FingerprintMismatch { batch: batch.signer_cert_sha256.clone(), identity: identity.fingerprint_sha256.clone() });
    }
    crate::verify::verify_batch(batch, &identity.public_key).map_err(BatchIdentityError::from)
}

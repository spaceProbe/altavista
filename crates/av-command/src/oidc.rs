//! OIDC token verification (`docs/aiplane-plan.md` milestone A2, the A2.1 half; the secsso
//! claims contract, `docs/open-questions.md` question 34 and question 201(b)).
//!
//! [`verify`] is the one entry point: given a compact-serialization JWS (`header.payload.
//! signature`), an [`IssuerConfig`] (the issuer, the audience, the issuer's already-loaded
//! public key, and the allow-listed `alg` values) and a clock *reading* (a plain
//! `now_tai_ns: i64`, not a `Clock` trait object -- see "Purity" below), returns a verified
//! [`Principal`] ([`av_cdm::pb::Principal`], `authority.proto`) or a named [`TokenError`].
//!
//! # Purity -- a pure function of (token, issuer configuration, clock reading)
//!
//! [`verify`] does no I/O of any kind: it is handed the issuer's public key already parsed
//! (inside [`IssuerConfig`], built once by [`IssuerConfig::from_public_key_pem`]) and a
//! clock reading already taken (`now_tai_ns: i64`, not a `&dyn Clock`) rather than reading
//! any clock itself. This is deliberate, not an accident of API shape: a function that took
//! a `&dyn Clock` could, in principle, be handed an implementation that does I/O (reads a
//! file, blocks on a lock); a plain `i64` cannot. Two calls with equal arguments always
//! return equal results.
//!
//! # Algorithms: RS256 only, and why ES256/ES384 are a named gap, not a half-built one
//!
//! `alg` allow-listing is configuration ([`IssuerConfig::allowed_algs`], a `Vec<`[`Alg`]`>`),
//! not a hardcoded single value -- but [`Alg`] has exactly one variant, [`Alg::Rs256`], and
//! [`IssuerConfig::from_public_key_pem`]'s default allow-list contains only it, because RS256
//! is the only algorithm this module actually implements. **ES256/ES384 are a named,
//! documented gap, not attempted**: a JWS ECDSA signature (RFC 7518 section 3.4) is the raw
//! concatenation `r || s`, each a fixed-width big-endian integer (32 bytes for P-256, 48 for
//! P-384) -- not the DER `SEQUENCE { r, s }` `openssl::sign::Verifier` expects for an EC key.
//! Doing this correctly needs, on top of the RS256 path already here: (a) a raw-to-DER
//! conversion for verification (`openssl::ecdsa::EcdsaSig::from_private_components` then
//! `to_der`), (b) the *inverse* DER-to-raw conversion in the test issuer for *signing* (since
//! `openssl::sign::Signer` over an EC key emits DER, never raw `r || s`), with fixed-width
//! zero-padding on both ends that a leading-zero byte in `r` or `s` can silently get wrong,
//! and (c) a second digest (SHA-384) and a second coordinate width for ES384 specifically.
//! That is not "a few clear lines" once signing, verification and the padding edge case are
//! all accounted for -- it is a second, differently-shaped crypto path with its own failure
//! modes, and this task's brief is explicit that in that situation RS256-only with a
//! documented gap is the right call over a half-tested ECDSA path. A later task may add
//! [`Alg::Es256`]/[`Alg::Es384`] additively; nothing here needs to change shape when it does.
//!
//! # Claim order, not sorted order, for `groups`/`amr`
//!
//! [`Principal::groups`] and [`Principal::amr`] are filled in the exact order the token's own
//! JSON array carried them, never re-sorted. Two reasons, both practical: (1) a JSON array's
//! element order is already well-defined and already deterministic (unlike, say, a
//! `HashMap`'s iteration order) -- there is nothing non-deterministic to fix by sorting; and
//! (2) A2.2's audit line and the ledger both want to record *what the token asserted*, in the
//! order it asserted it (an IdP that lists a `groups` claim in a meaningful order -- primary
//! role first, say -- has that meaning preserved), not a lexicographic reordering that would
//! make the recorded claim harder to compare against what an operator's IdP console shows.
//! Sorting would be *a* deterministic choice, but claim order is already deterministic and
//! more faithful, so this module makes no changes to either list's order.
//!
//! # The refusal vocabulary
//!
//! Every [`TokenError`] variant names exactly one check; none maps to a generic "invalid
//! token", and no `Result` in this module's verification path is ever collapsed to a bare
//! `bool` (`.ok()`, `unwrap_or(false)`, or otherwise) -- see [`verify_rs256_signature`]'s own
//! doc for the one place a boolean legitimately appears (`openssl::sign::Verifier::verify`'s
//! own return type), matched explicitly on all three of `Ok(true)`/`Ok(false)`/`Err(_)` so a
//! backend error is never silently read as "signature invalid". None of `Display`'s text for
//! any variant includes the raw token string or the raw signature bytes -- only claim values
//! (`iss`, `aud`, `exp`/`nbf` as TAI nanoseconds) and the `openssl` backend's own error text
//! (which itself never contains key or signature material, only OpenSSL's internal error
//! codes/library names) -- see `crate::service`'s module doc for where this is asserted by a
//! test at the gRPC boundary.
//!
//! # TAI conversion
//!
//! A JWT's `exp`/`iat`/`nbf` claims are Unix (UTC) seconds (RFC 7519's `NumericDate`). The one
//! conversion site is [`unix_seconds_to_tai_ns`] below, which multiplies by `1_000_000_000`
//! (`saturating_mul`, so an adversarial huge claim value cannot overflow-panic this function
//! -- a panic is not a typed refusal) and calls [`av_cdm::time::Tai::from_utc_nanos`], the
//! identical UTC->TAI boundary conversion `crate::clock::SystemClock` itself uses. There is no
//! second, ad hoc UTC/TAI conversion anywhere in this crate.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Public};
use openssl::sign::Verifier;
use serde::Deserialize;
use thiserror::Error;

use av_cdm::pb::Principal;
use av_cdm::time::Tai;

/// Allow-listed JWS `alg` header values [`verify`] accepts. Exactly one variant today --
/// see the module doc's "Algorithms" section for why ES256/ES384 are a named gap rather than
/// a second variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Rs256,
}

impl Alg {
    /// The exact JWS header string this variant matches (case-sensitive, per RFC 7515 --
    /// `"rs256"`/`"Rs256"` are not `"RS256"`, and neither is refused any differently from any
    /// other unrecognized `alg`: both simply fail to match any entry in the allow-list).
    fn header_value(self) -> &'static str {
        match self {
            Alg::Rs256 => "RS256",
        }
    }
}

/// The issuer's public key, the issuer/audience strings to check claims against, and the
/// allow-listed algorithms -- everything [`verify`] needs, with no I/O of its own (the key is
/// already parsed; see the module doc's "Purity" section).
#[derive(Clone)]
pub struct IssuerConfig {
    issuer: String,
    audience: String,
    public_key: PKey<Public>,
    allowed_algs: Vec<Alg>,
}

/// Everything [`IssuerConfig::from_public_key_pem`] can fail with -- just the one way parsing
/// a PEM-encoded public key can fail, named rather than left as a bare `openssl::error::
/// ErrorStack` at this crate's own construction boundary.
#[derive(Debug, Error)]
pub enum IssuerConfigError {
    #[error("issuer public key PEM did not parse as a public key usable for verification: {0}")]
    InvalidPublicKeyPem(#[source] openssl::error::ErrorStack),
}

impl IssuerConfig {
    /// Parses `public_key_pem` once (the only I/O in this type's construction -- [`verify`]
    /// itself does none) and defaults the allow-list to `[`[`Alg::Rs256`]`]`, the only
    /// algorithm this module implements. Use [`Self::with_allowed_algs`] to narrow (never to
    /// widen beyond what this module can actually verify -- there is nothing to widen to yet).
    pub fn from_public_key_pem(issuer: impl Into<String>, audience: impl Into<String>, public_key_pem: &[u8]) -> Result<Self, IssuerConfigError> {
        let public_key = PKey::public_key_from_pem(public_key_pem).map_err(IssuerConfigError::InvalidPublicKeyPem)?;
        Ok(Self { issuer: issuer.into(), audience: audience.into(), public_key, allowed_algs: vec![Alg::Rs256] })
    }

    /// Replaces the allow-list `verify` checks a token's `alg` header against.
    pub fn with_allowed_algs(mut self, allowed_algs: Vec<Alg>) -> Self {
        self.allowed_algs = allowed_algs;
        self
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn audience(&self) -> &str {
        &self.audience
    }
}

/// Every way [`verify`] can refuse a token, each naming exactly the one check that failed --
/// see the module doc's "The refusal vocabulary" section.
#[derive(Debug, Error)]
pub enum TokenError {
    /// Not exactly three `.`-separated segments (`header.payload.signature`).
    #[error("malformed token: expected 3 dot-separated segments (header.payload.signature), found {0}")]
    WrongSegmentCount(usize),
    #[error("malformed token: header segment is not valid base64url: {0}")]
    HeaderBase64Invalid(base64::DecodeError),
    #[error("malformed token: payload segment is not valid base64url: {0}")]
    PayloadBase64Invalid(base64::DecodeError),
    #[error("malformed token: signature segment is not valid base64url: {0}")]
    SignatureBase64Invalid(base64::DecodeError),
    #[error("malformed token: header segment is not valid JSON: {0}")]
    HeaderJsonInvalid(serde_json::Error),
    #[error("malformed token: payload segment is not valid JSON: {0}")]
    PayloadJsonInvalid(serde_json::Error),
    /// The header's `alg` is not one of [`IssuerConfig::allowed_algs`] -- **including
    /// `"none"`**, which is not special-cased: it simply never matches any entry in a
    /// non-empty allow-list (see the module doc; tested explicitly at
    /// [`tests::alg_none_is_refused_and_never_reaches_signature_verification`]).
    #[error("alg {alg:?} is not in the configured allow-list {allowed:?}")]
    AlgorithmNotAllowed { alg: String, allowed: Vec<Alg> },
    /// The signature was well-formed but did not verify against the configured issuer key.
    #[error("signature does not verify against the configured issuer key")]
    SignatureInvalid,
    /// The `openssl` backend itself failed (never the signature simply not matching -- see
    /// [`Self::SignatureInvalid`] for that case). This variant's `Display` is safe to surface
    /// to a caller: OpenSSL's `ErrorStack` text names internal error codes/library/function
    /// names, never key or signature material.
    #[error("openssl backend error while verifying the signature: {0}")]
    CryptoBackend(#[source] openssl::error::ErrorStack),
    #[error("iss {actual:?} does not match the configured issuer {expected:?}")]
    IssuerMismatch { actual: String, expected: String },
    #[error("aud {actual:?} does not contain the configured audience {expected:?}")]
    AudienceMismatch { actual: Vec<String>, expected: String },
    /// The token carries no `exp` claim at all -- refused rather than treated as "never
    /// expires" (a missing mandatory claim is not the same thing as a distant one).
    #[error("token has no exp claim")]
    MissingExpiry,
    /// `now_tai_ns >= exp_tai_ns` -- refused **at the boundary second**: equal counts as
    /// expired (see the module doc's TAI-conversion section and `crate::service`'s own
    /// boundary test).
    #[error("token expired: exp_tai_ns={exp_tai_ns} is at or before now_tai_ns={now_tai_ns}")]
    Expired { exp_tai_ns: i64, now_tai_ns: i64 },
    /// `now_tai_ns < nbf_tai_ns` -- an absent `nbf` is never refused (RFC 7519 makes it
    /// optional); only a *present* `nbf` in the future is.
    #[error("token not yet valid: nbf_tai_ns={nbf_tai_ns} is after now_tai_ns={now_tai_ns}")]
    NotYetValid { nbf_tai_ns: i64, now_tai_ns: i64 },
    #[error("sub claim is missing or empty")]
    MissingSubject,
    /// The token carries no `iat` claim at all -- refused rather than defaulted to `0` (a
    /// silent, wrong "issued at the Unix epoch" would be worse than a named refusal).
    #[error("token has no iat claim")]
    MissingIssuedAt,
}

/// The JWS header this module reads. Only `alg` matters to [`verify`]; any other header
/// member (`typ`, `kid`, ...) is accepted and ignored by `serde_json`'s default "unknown
/// fields are ignored" behaviour -- there is deliberately no `dead_code`-inviting field for
/// them here (this crate's own rule bans lint-suppressing attributes on hand-written items).
#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
}

/// `aud` may be a single string or an array of strings (RFC 7519, `StringOrURI` vs. an array
/// of same) -- this untagged enum accepts either shape directly from `serde_json`, no manual
/// `serde_json::Value` inspection needed.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    One(String),
    Many(Vec<String>),
}

impl AudienceClaim {
    fn into_vec(self) -> Vec<String> {
        match self {
            AudienceClaim::One(s) => vec![s],
            AudienceClaim::Many(v) => v,
        }
    }
}

/// The secsso claims contract this module parses (question 34, question 201(b)) plus the
/// standard registered claims [`verify`] checks. Every field defaults when absent (`serde`'s
/// `#[serde(default)]`) rather than making the whole payload fail to parse over one missing
/// optional claim -- [`verify`] itself decides which absences are refusals (`sub`, `exp`,
/// `iat`) and which are not (`groups`, `amr`, `acr`, `jti`, `nbf`).
#[derive(Debug, Deserialize)]
struct RawClaims {
    #[serde(default)]
    sub: String,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    amr: Vec<String>,
    #[serde(default)]
    acr: Option<String>,
    #[serde(default)]
    iss: String,
    #[serde(default)]
    aud: Option<AudienceClaim>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    nbf: Option<i64>,
}

/// Converts a JWT `NumericDate` (Unix/UTC seconds) to TAI nanoseconds -- see the module doc's
/// "TAI conversion" section. `saturating_mul` so an adversarial or malformed huge claim value
/// cannot overflow-panic this function; a panic is not a typed refusal.
fn unix_seconds_to_tai_ns(unix_seconds: i64) -> i64 {
    Tai::from_utc_nanos(unix_seconds.saturating_mul(1_000_000_000)).as_nanos()
}

/// Verifies `signing_input` (`"<header_b64>.<payload_b64>"`, exactly the bytes RFC 7515
/// signs) against `signature` under `key` with RS256 (SHA-256 digest). The one place this
/// module's own doc warns about: `openssl::sign::Verifier::verify` returns `Result<bool,
/// ErrorStack>`, and this function matches all three of `Ok(true)`/`Ok(false)`/`Err(_)`
/// explicitly -- never `.unwrap_or(false)` or `.ok()`, which would silently read a backend
/// error as "signature invalid" rather than surfacing [`TokenError::CryptoBackend`].
fn verify_rs256_signature(signing_input: &[u8], signature: &[u8], key: &PKey<Public>) -> Result<(), TokenError> {
    let mut verifier = Verifier::new(MessageDigest::sha256(), key).map_err(TokenError::CryptoBackend)?;
    verifier.update(signing_input).map_err(TokenError::CryptoBackend)?;
    match verifier.verify(signature) {
        Ok(true) => Ok(()),
        Ok(false) => Err(TokenError::SignatureInvalid),
        Err(e) => Err(TokenError::CryptoBackend(e)),
    }
}

/// Verifies `token` (a compact-serialization JWS) against `config`, evaluating `exp`/`nbf`
/// against `now_tai_ns` (a clock *reading*, not a `Clock` -- see the module doc's "Purity"
/// section), and returns the [`Principal`] the claims describe on success. See the module doc
/// for the full refusal vocabulary and every design decision behind this function.
///
/// Order of checks, deliberately: segment count -> header decode/parse -> `alg` allow-list ->
/// payload decode/parse -> signature decode -> **signature verification** -> `iss` -> `aud` ->
/// `exp` -> `nbf` -> `sub`. The `alg` allow-list is checked *before* any cryptographic
/// operation, so an `alg: "none"` token (or any other disallowed `alg`) is refused
/// deterministically regardless of what its "signature" segment even looks like -- it never
/// reaches [`verify_rs256_signature`] at all. Every claim check after signature verification
/// trusts the claims only because the signature already verified; nothing here ever inspects
/// a claim's value before the signature covering it has been checked, except the header's own
/// `alg` (which is not itself a claim `verify` trusts for anything beyond selecting how to
/// verify the signature that follows it).
pub fn verify(token: &str, config: &IssuerConfig, now_tai_ns: i64) -> Result<Principal, TokenError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(TokenError::WrongSegmentCount(parts.len()));
    }
    let (header_b64, payload_b64, signature_b64) = (parts[0], parts[1], parts[2]);

    let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).map_err(TokenError::HeaderBase64Invalid)?;
    let header: Header = serde_json::from_slice(&header_bytes).map_err(TokenError::HeaderJsonInvalid)?;

    if !config.allowed_algs.iter().any(|a| a.header_value() == header.alg) {
        return Err(TokenError::AlgorithmNotAllowed { alg: header.alg, allowed: config.allowed_algs.clone() });
    }

    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).map_err(TokenError::PayloadBase64Invalid)?;
    let claims: RawClaims = serde_json::from_slice(&payload_bytes).map_err(TokenError::PayloadJsonInvalid)?;

    let signature = URL_SAFE_NO_PAD.decode(signature_b64).map_err(TokenError::SignatureBase64Invalid)?;

    let signing_input = format!("{header_b64}.{payload_b64}");
    verify_rs256_signature(signing_input.as_bytes(), &signature, &config.public_key)?;

    if claims.iss != config.issuer {
        return Err(TokenError::IssuerMismatch { actual: claims.iss, expected: config.issuer.clone() });
    }

    let aud_list = claims.aud.map(AudienceClaim::into_vec).unwrap_or_default();
    if !aud_list.iter().any(|a| a == &config.audience) {
        return Err(TokenError::AudienceMismatch { actual: aud_list, expected: config.audience.clone() });
    }

    let exp_tai_ns = match claims.exp {
        Some(exp) => unix_seconds_to_tai_ns(exp),
        None => return Err(TokenError::MissingExpiry),
    };
    if now_tai_ns >= exp_tai_ns {
        return Err(TokenError::Expired { exp_tai_ns, now_tai_ns });
    }

    if let Some(nbf) = claims.nbf {
        let nbf_tai_ns = unix_seconds_to_tai_ns(nbf);
        if now_tai_ns < nbf_tai_ns {
            return Err(TokenError::NotYetValid { nbf_tai_ns, now_tai_ns });
        }
    }

    if claims.sub.is_empty() {
        return Err(TokenError::MissingSubject);
    }

    let issued_at_tai_ns = match claims.iat {
        Some(iat) => unix_seconds_to_tai_ns(iat),
        None => return Err(TokenError::MissingIssuedAt),
    };

    Ok(Principal {
        sub: claims.sub,
        groups: claims.groups,
        amr: claims.amr,
        acr: claims.acr.unwrap_or_default(),
        issuer: claims.iss,
        audience: config.audience.clone(),
        jti: claims.jti.unwrap_or_default(),
        issued_at_tai_ns,
        expiry_tai_ns: exp_tai_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{valid_claims, TestIssuer};
    use serde_json::json;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-command";
    const SUBJECT: &str = "operator-1";
    /// A fixed, arbitrary "now" (2025-10-09T14:13:20Z-ish) so every test's claims are
    /// deterministic and independent of the real wall clock -- this module never reads it.
    const NOW_UNIX_S: i64 = 1_760_000_000;
    const TTL_S: i64 = 3_600;

    fn config(issuer: &TestIssuer) -> IssuerConfig {
        IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap()
    }

    /// A real, fully-valid token, matching `now_tai_ns` set to the same `NOW_UNIX_S` base --
    /// every other test in this module starts from this success case and changes exactly one
    /// thing.
    #[test]
    fn a_valid_token_verifies_and_fills_every_principal_field_in_claim_order() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims["groups"] = json!(["b-group", "a-group"]); // deliberately not alphabetical
        claims["amr"] = json!(["otp", "pwd"]); // deliberately not alphabetical
        let token = issuer.mint(&claims);

        let now_tai_ns = unix_seconds_to_tai_ns(NOW_UNIX_S);
        let principal = verify(&token, &cfg, now_tai_ns).expect("a fresh, correctly-signed token must verify");

        assert_eq!(principal.sub, SUBJECT);
        assert_eq!(principal.groups, vec!["b-group".to_string(), "a-group".to_string()], "claim order, not sorted order");
        assert_eq!(principal.amr, vec!["otp".to_string(), "pwd".to_string()], "claim order, not sorted order");
        assert_eq!(principal.acr, "urn:mfa:otp");
        assert_eq!(principal.issuer, ISSUER);
        assert_eq!(principal.audience, AUDIENCE);
        assert_eq!(principal.jti, "test-jti-1");
        assert_eq!(principal.issued_at_tai_ns, unix_seconds_to_tai_ns(NOW_UNIX_S));
        assert_eq!(principal.expiry_tai_ns, unix_seconds_to_tai_ns(NOW_UNIX_S + TTL_S));
    }

    #[test]
    fn wrong_segment_count_is_refused_malformed() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        for bad in ["not-a-jws-at-all", "one.two", "one.two.three.four"] {
            let err = verify(bad, &cfg, 0).unwrap_err();
            assert!(matches!(err, TokenError::WrongSegmentCount(_)), "{bad:?} -> {err:?}");
        }
    }

    #[test]
    fn bad_base64_in_any_segment_is_refused_malformed() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));
        let parts: Vec<&str> = token.split('.').collect();

        let bad_header = format!("not-valid-base64!!.{}.{}", parts[1], parts[2]);
        assert!(matches!(verify(&bad_header, &cfg, 0).unwrap_err(), TokenError::HeaderBase64Invalid(_)));

        let bad_payload = format!("{}.not-valid-base64!!.{}", parts[0], parts[2]);
        assert!(matches!(verify(&bad_payload, &cfg, 0).unwrap_err(), TokenError::PayloadBase64Invalid(_)));

        let bad_sig = format!("{}.{}.not-valid-base64!!", parts[0], parts[1]);
        assert!(matches!(verify(&bad_sig, &cfg, 0).unwrap_err(), TokenError::SignatureBase64Invalid(_)));
    }

    #[test]
    fn bad_json_in_header_or_payload_is_refused_malformed() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));
        let parts: Vec<&str> = token.split('.').collect();

        // A hand-built malformed token: valid base64url, but the decoded bytes are not JSON
        // at all -- the only way to provoke this case, since a real issuer never mints one.
        let not_json_b64 = URL_SAFE_NO_PAD.encode(b"not json");
        let bad_header = format!("{not_json_b64}.{}.{}", parts[1], parts[2]);
        assert!(matches!(verify(&bad_header, &cfg, 0).unwrap_err(), TokenError::HeaderJsonInvalid(_)));

        let bad_payload = format!("{}.{not_json_b64}.{}", parts[0], parts[2]);
        assert!(matches!(verify(&bad_payload, &cfg, 0).unwrap_err(), TokenError::PayloadJsonInvalid(_)));
    }

    /// **The `alg: "none"` case, tested explicitly.** A hand-built malformed token: no real
    /// issuer signs `alg: "none"` (there is nothing to sign it *with*), so this is
    /// necessarily built by hand -- header/payload only, an empty signature segment (the
    /// classic `alg: none` shape), and it must be refused by the allow-list check, never by
    /// accident reaching (and failing) signature verification.
    #[test]
    fn alg_none_is_refused_and_never_reaches_signature_verification() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"alg": "none"})).unwrap());
        let claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{header_b64}.{payload_b64}."); // empty signature segment

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        match err {
            TokenError::AlgorithmNotAllowed { alg, allowed } => {
                assert_eq!(alg, "none");
                assert_eq!(allowed, vec![Alg::Rs256]);
            }
            other => panic!("expected AlgorithmNotAllowed, got {other:?}"),
        }
    }

    /// A disallowed but *plausible-looking* `alg` (not `"none"`) is refused the same way --
    /// proves the allow-list check is a real allow-list, not a single hardcoded string
    /// compare against `"none"` alone.
    #[test]
    fn an_alg_outside_the_allow_list_other_than_none_is_also_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"alg": "HS256"})).unwrap());
        let claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{header_b64}.{payload_b64}.c2lnbmF0dXJl");

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::AlgorithmNotAllowed { .. }), "{err:?}");
    }

    #[test]
    fn a_signature_from_the_wrong_key_is_refused_signature_invalid() {
        let issuer = TestIssuer::new();
        let wrong_issuer = TestIssuer::new(); // a different key entirely
        let cfg = config(&issuer);
        let token = wrong_issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::SignatureInvalid), "{err:?}");
    }

    /// A real token, correctly signed, with exactly one byte of its signature flipped --
    /// proves this is a genuine cryptographic check, not merely "the two keys differ".
    #[test]
    fn a_tampered_signature_is_refused_signature_invalid() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));
        let parts: Vec<&str> = token.split('.').collect();
        let mut sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        sig[0] ^= 0xFF;
        let tampered = format!("{}.{}.{}", parts[0], parts[1], URL_SAFE_NO_PAD.encode(sig));

        let err = verify(&tampered, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::SignatureInvalid), "{err:?}");
    }

    #[test]
    fn wrong_issuer_is_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims("https://not-the-configured-issuer/", AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        match err {
            TokenError::IssuerMismatch { actual, expected } => {
                assert_eq!(actual, "https://not-the-configured-issuer/");
                assert_eq!(expected, ISSUER);
            }
            other => panic!("expected IssuerMismatch, got {other:?}"),
        }
    }

    #[test]
    fn wrong_audience_is_refused_for_a_string_aud() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, "some-other-audience", SUBJECT, NOW_UNIX_S, TTL_S));

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::AudienceMismatch { .. }), "{err:?}");
    }

    /// `aud` as a JSON array not containing the configured audience -- proves the
    /// array-shaped case is handled, not only the single-string case.
    #[test]
    fn wrong_audience_is_refused_for_an_array_aud_not_containing_it() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims["aud"] = json!(["some-other-audience", "yet-another"]);
        let token = issuer.mint(&claims);

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        match err {
            TokenError::AudienceMismatch { actual, expected } => {
                assert_eq!(actual, vec!["some-other-audience".to_string(), "yet-another".to_string()]);
                assert_eq!(expected, AUDIENCE);
            }
            other => panic!("expected AudienceMismatch, got {other:?}"),
        }
    }

    /// `aud` as a JSON array that *does* contain the configured audience, among others --
    /// proves the array case is a real "contains" check, not "equals the first element".
    #[test]
    fn an_array_aud_containing_the_configured_audience_among_others_is_accepted() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims["aud"] = json!(["some-other-audience", AUDIENCE]);
        let token = issuer.mint(&claims);

        verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).expect("aud array containing the configured audience must be accepted");
    }

    /// **The exact `exp` boundary this task pins.** `exp_tai_ns` is the TAI-nanosecond
    /// conversion of `NOW_UNIX_S + TTL_S`. At `now_tai_ns == exp_tai_ns` exactly, the token is
    /// expired (refused); at `now_tai_ns == exp_tai_ns - 1`, one nanosecond earlier, it is not.
    #[test]
    fn exp_boundary_second_is_refused_one_nanosecond_earlier_is_not() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));
        let exp_tai_ns = unix_seconds_to_tai_ns(NOW_UNIX_S + TTL_S);

        let err = verify(&token, &cfg, exp_tai_ns).unwrap_err();
        match err {
            TokenError::Expired { exp_tai_ns: got_exp, now_tai_ns: got_now } => {
                assert_eq!(got_exp, exp_tai_ns);
                assert_eq!(got_now, exp_tai_ns);
            }
            other => panic!("expected Expired at the boundary, got {other:?}"),
        }

        verify(&token, &cfg, exp_tai_ns - 1).expect("one nanosecond before exp must not be refused");
    }

    /// The mirror boundary for `nbf`: at `now_tai_ns == nbf_tai_ns` exactly the token is
    /// already valid (not refused); one nanosecond earlier it is not yet valid (refused).
    #[test]
    fn nbf_boundary_second_is_valid_one_nanosecond_earlier_is_not() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims["nbf"] = json!(NOW_UNIX_S);
        let token = issuer.mint(&claims);
        let nbf_tai_ns = unix_seconds_to_tai_ns(NOW_UNIX_S);

        verify(&token, &cfg, nbf_tai_ns).expect("at nbf exactly, the token must already be valid");

        let err = verify(&token, &cfg, nbf_tai_ns - 1).unwrap_err();
        match err {
            TokenError::NotYetValid { nbf_tai_ns: got_nbf, now_tai_ns: got_now } => {
                assert_eq!(got_nbf, nbf_tai_ns);
                assert_eq!(got_now, nbf_tai_ns - 1);
            }
            other => panic!("expected NotYetValid one nanosecond before nbf, got {other:?}"),
        }
    }

    #[test]
    fn a_token_with_no_nbf_claim_is_never_refused_for_it() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims.as_object_mut().unwrap().remove("nbf");
        let token = issuer.mint(&claims);

        verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).expect("an absent nbf must never be refused");
    }

    #[test]
    fn missing_exp_is_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims.as_object_mut().unwrap().remove("exp");
        let token = issuer.mint(&claims);

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::MissingExpiry), "{err:?}");
    }

    #[test]
    fn missing_iat_is_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims.as_object_mut().unwrap().remove("iat");
        let token = issuer.mint(&claims);

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::MissingIssuedAt), "{err:?}");
    }

    #[test]
    fn missing_sub_is_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims.as_object_mut().unwrap().remove("sub");
        let token = issuer.mint(&claims);

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::MissingSubject), "{err:?}");
    }

    #[test]
    fn empty_sub_is_refused() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let mut claims = valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S);
        claims["sub"] = json!("");
        let token = issuer.mint(&claims);

        let err = verify(&token, &cfg, unix_seconds_to_tai_ns(NOW_UNIX_S)).unwrap_err();
        assert!(matches!(err, TokenError::MissingSubject), "{err:?}");
    }

    /// Two verifications of the same token against the same config and clock reading return
    /// equal results -- the module doc's "Purity" contract, checked directly.
    #[test]
    fn verify_is_a_pure_function_of_its_three_arguments() {
        let issuer = TestIssuer::new();
        let cfg = config(&issuer);
        let token = issuer.mint(&valid_claims(ISSUER, AUDIENCE, SUBJECT, NOW_UNIX_S, TTL_S));
        let now_tai_ns = unix_seconds_to_tai_ns(NOW_UNIX_S);

        let first = verify(&token, &cfg, now_tai_ns).unwrap();
        let second = verify(&token, &cfg, now_tai_ns).unwrap();
        assert_eq!(first, second);
    }
}

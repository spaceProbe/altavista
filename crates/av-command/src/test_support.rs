//! **Test-fixture-only utilities.** A local OpenSSL-backed OIDC issuer ([`TestIssuer`]) that
//! mints real, signed RS256 tokens with caller-chosen claims -- including deliberately bad
//! ones (wrong `iss`, wrong `aud`, expired, missing `sub`, ...) -- so `crate::oidc`'s refusal
//! paths are exercised against a *real* token wherever possible, not a hand-edited string
//! (question 201(b): "verified against a local test issuer's OpenSSL key").
//!
//! # Why this lives in `src/`, gated by a Cargo feature rather than plain `#[cfg(test)]`
//!
//! Two different kinds of test need this: this crate's own unit tests (`#[cfg(test)]` blocks
//! inside `src/oidc.rs`) and this crate's integration tests (`tests/*.rs`, a separate
//! compilation unit that links against this crate as an ordinary external dependency). A
//! `tests/common/mod.rs` cannot be reached from `src/` at all -- it is not part of the library
//! crate. The reverse is also true: code gated `#[cfg(test)]` inside `src/` is compiled only
//! when *this crate itself* is being tested (`cargo test -p av-command`'s unit-test binary),
//! and is invisible to `tests/*.rs`, which link against the ordinary, non-`--cfg test` build
//! of the library.
//!
//! An earlier revision of this module made it a plain `pub mod`, compiled unconditionally, on
//! the reasoning that both call sites need *some* shape that is not `#[cfg(test)]`-only and
//! that nothing in this crate's production code referenced it. **That was wrong, on review**:
//! a crate whose whole job is *verifying* tokens should not also ship a way to *mint* them as
//! part of its default-feature public API, regardless of whether anything calls it --
//! "unreachable from production code" stops being true the moment someone, elsewhere, does
//! reach it. The fix keeps the code in one place (no duplicated signing logic) while making
//! the module genuinely absent from an ordinary build: this module is declared in `src/lib.rs`
//! as `#[cfg(any(test, feature = "test-support"))] pub mod test_support;`. `cfg(test)` covers
//! this crate's own unit tests; the `test-support` feature (`Cargo.toml`'s `[features]`) is
//! what `tests/*.rs` needs, turned on via this crate's own `[dev-dependencies]` self-reference
//! (`av-command = { path = ".", features = ["test-support"] }`) -- Cargo unifies that feature
//! into the test-profile build of this crate that `tests/*.rs` links against, while
//! `cargo build -p av-command` (no `--tests`, default features) never activates it at all: the
//! module is not compiled into that artifact, not merely unreferenced by it.
//!
//! Key generation is genuinely random ([`TestIssuer::new`], `openssl::rsa::Rsa::generate`) --
//! fine for test-fixture material that never reaches the ledger, but exactly why no test in
//! this crate may depend on a particular key's bytes (every test that needs a key calls
//! `TestIssuer::new` itself and reads the key back out through `public_key_pem`, never a
//! literal PEM checked into a test file), and exactly why this module is the *only* place in
//! this crate that generates a key of any kind -- `crate::oidc` never does.

use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private};
use openssl::rsa::Rsa;
use openssl::sign::Signer;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::json;

/// A local RSA-2048 key pair and RS256 signer, generated fresh per instance. Never constructed
/// or referenced by this crate's non-test code path -- see the module doc.
pub struct TestIssuer {
    private_key: PKey<Private>,
    public_key_pem: Vec<u8>,
}

impl TestIssuer {
    /// Generates a fresh RSA-2048 key pair via the `openssl` crate (ADR-004's crypto rule:
    /// the system OpenSSL only, no bundled crypto -- test-fixture key generation is no
    /// exception). Random, and deliberately so: no test in this crate depends on a
    /// particular key's bytes, only on what a token signed by *some* key does or does not
    /// verify against a given [`crate::oidc::IssuerConfig`].
    pub fn new() -> Self {
        let rsa = Rsa::generate(2048).expect("test issuer: RSA key generation failed");
        let private_key = PKey::from_rsa(rsa).expect("test issuer: PKey::from_rsa failed");
        let public_key_pem = private_key.public_key_to_pem().expect("test issuer: public_key_to_pem failed");
        Self { private_key, public_key_pem }
    }

    /// The public key, PEM-encoded -- feed this straight to
    /// [`crate::oidc::IssuerConfig::from_public_key_pem`].
    pub fn public_key_pem(&self) -> &[u8] {
        &self.public_key_pem
    }

    /// Mints a real RS256-signed compact JWS over `claims` (an arbitrary caller-built JSON
    /// object) with the ordinary `{"alg": "RS256", "typ": "JWT"}` header. Callers build
    /// deliberately-bad `claims` (wrong `iss`, wrong `aud`, past `exp`, missing `sub`, ...) to
    /// exercise `crate::oidc::verify`'s refusal paths against a real, correctly-signed token
    /// -- only the checks that occur *before* signature verification even runs (segment
    /// count, base64, JSON-ness, `alg`) need a hand-built token instead, since those are, by
    /// construction, not something a real issuer would ever produce.
    pub fn mint(&self, claims: &serde_json::Value) -> String {
        self.mint_with_header(&json!({"alg": "RS256", "typ": "JWT"}), claims)
    }

    /// As [`Self::mint`], but with a caller-chosen header. Exists for callers that need a
    /// *real, correctly-signed* token whose header still differs from the ordinary case (e.g.
    /// a real signature over an unexpected `alg` string this issuer's key can still produce,
    /// even though `crate::oidc::verify` refuses it on the allow-list before ever checking the
    /// signature) -- `crate::oidc`'s test for a disallowed-but-plausible `alg` uses this
    /// instead of a hand-built token for exactly that reason.
    pub fn mint_with_header(&self, header: &serde_json::Value, claims: &serde_json::Value) -> String {
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).expect("test issuer: header serializes"));
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("test issuer: claims serialize"));
        let signing_input = format!("{header_b64}.{payload_b64}");

        let mut signer = Signer::new(MessageDigest::sha256(), &self.private_key).expect("test issuer: Signer::new failed");
        signer.update(signing_input.as_bytes()).expect("test issuer: Signer::update failed");
        let signature = signer.sign_to_vec().expect("test issuer: sign_to_vec failed");
        let signature_b64 = URL_SAFE_NO_PAD.encode(signature);

        format!("{signing_input}.{signature_b64}")
    }
}

impl Default for TestIssuer {
    fn default() -> Self {
        Self::new()
    }
}

/// A baseline set of claims satisfying every check in [`crate::oidc::verify`] -- callers get
/// a real, fully-valid token by passing this straight to [`TestIssuer::mint`], or start from
/// it and mutate/remove exactly one field (`serde_json::Value` indexing) to provoke exactly
/// one refusal, e.g. `claims["iss"] = json!("wrong-issuer")` or
/// `claims.as_object_mut().unwrap().remove("exp")`.
///
/// Deliberately carries **no `nbf`** by default: `nbf` is optional per RFC 7519, and this
/// crate's own integration tests (`tests/grpc_service.rs`) run against a
/// [`crate::clock::TestClock`] seeded at small, non-epoch values (e.g. `1_000`) that have no
/// relationship to `now_unix_s` -- a default `nbf` would make every such test's "current
/// clock reading" look like it is before the token's own issue time, refused
/// [`crate::oidc::TokenError::NotYetValid`] for a reason that has nothing to do with what
/// that test is actually checking. Tests that specifically exercise `nbf` add it explicitly.
pub fn valid_claims(issuer: &str, audience: &str, subject: &str, now_unix_s: i64, ttl_s: i64) -> serde_json::Value {
    json!({
        "iss": issuer,
        "aud": audience,
        "sub": subject,
        "iat": now_unix_s,
        "exp": now_unix_s + ttl_s,
        "groups": ["operators", "burn-authorizers"],
        "amr": ["pwd", "otp"],
        "acr": "urn:mfa:otp",
        "jti": "test-jti-1",
    })
}

/// The `groups`/`amr`/`acr` override [`claims_with_roles_and_mfa`] takes -- grouped into one
/// struct (rather than three more bare parameters) so that function's own signature stays at
/// six parameters, at clippy's default `too_many_arguments` threshold rather than over it;
/// this crate's rule against a lint-suppressing attribute on hand-written items means the fix is grouping the
/// arguments, never silencing the lint in place.
pub struct RoleAndMfaClaims<'a> {
    pub groups: &'a [&'a str],
    pub amr: &'a [&'a str],
    pub acr: &'a str,
}

/// As [`valid_claims`], but with caller-chosen `groups`/`amr`/`acr` -- for a test that needs a
/// specific role or a specific (or absent) MFA claim (A2.2). Lives here, not in an integration
/// test file, so a caller needs no direct `serde_json` dependency of its own to build one of
/// these -- `tests/grpc_service.rs`'s own `TestServer::mint_with_claims` is exactly such a
/// caller.
pub fn claims_with_roles_and_mfa(issuer: &str, audience: &str, subject: &str, now_unix_s: i64, ttl_s: i64, overrides: RoleAndMfaClaims<'_>) -> serde_json::Value {
    let mut claims = valid_claims(issuer, audience, subject, now_unix_s, ttl_s);
    claims["groups"] = json!(overrides.groups);
    claims["amr"] = json!(overrides.amr);
    claims["acr"] = json!(overrides.acr);
    claims
}

//! SCRAM-SHA-256 (RFC 5802, specialised to SHA-256 by RFC 7677), on the system OpenSSL only.
//!
//! Every primitive this module needs comes from the `openssl` crate (ADR-004): `openssl::
//! pkcs5::pbkdf2_hmac` for `Hi()` (`SaltedPassword`), `openssl::sign::Signer` + `openssl::
//! pkey::PKey::hmac` with `MessageDigest::sha256()` for every `HMAC(key, "Client Key" |
//! "Server Key" | AuthMessage)` step, `openssl::sha::sha256` for `StoredKey = H(ClientKey)`,
//! `openssl::memcmp::eq` for the constant-time `ServerSignature` comparison
//! ([`verify_server_final`]), and `openssl::rand::rand_bytes` for the client nonce. No `hmac`
//! crate, no `sha2` crate, no `ring` -- SCRAM-SHA-256 needs nothing else.
//!
//! # Channel binding: always `n,,`, never `-PLUS`
//!
//! [`GS2_HEADER`] is always the literal three bytes `n,,` -- GS2's "client does not support
//! channel binding" header, no authzid. `p=tls-server-end-point,,` (`-PLUS`) is deliberately
//! never sent: proving the channel-binding data matches means binding the SCRAM exchange to
//! the TLS session's own `tls-server-end-point` channel-binding data (RFC 5929), which means
//! this code would have to own the TLS layer's finished-message/certificate hash at the point
//! it builds `client-final-message` -- but `src/client.rs`'s `PgTls::Required` wraps a
//! `tokio::net::TcpStream` in an `openssl::ssl::SslStream` as a black box (see that module's
//! own doc), and this crate's actual deployment shape is either a loopback connection (no MITM
//! surface between this process and the catalog container) or a connection behind the
//! platform's own TLS-terminating front (ADR-003) -- neither needs `-PLUS`'s extra guarantee
//! badly enough to justify plumbing channel-binding data across that boundary. [`GS2_HEADER`]
//! is a typed constant precisely so this choice is made in exactly one place, not a string
//! literal `"n,,"` re-typed at every call site a future editor could get subtly wrong (e.g. by
//! typing `"n,"`  -- one comma short -- which GS2's grammar also parses, into a different and
//! wrong header).
//!
//! # SASLprep is not implemented
//!
//! RFC 5802 requires the password to be prepared with SASLprep (RFC 4013) before it is used as
//! `Hi()`'s input. This module does not implement SASLprep (a full Unicode normalization +
//! bidi + prohibited-character profile is a large amount of code this crate's own catalog
//! passwords -- generated, ASCII, by this platform's own deployment tooling -- have no real
//! need for). Instead, [`ScramClient::new`]/[`ScramClient::with_client_nonce`] refuse any
//! non-ASCII password outright ([`CatalogError::NonAsciiPassword`]) rather than hashing an
//! un-normalized byte sequence that might authenticate against the wrong `SaltedPassword`
//! today and a DIFFERENT wrong one tomorrow (Unicode normalization is not idempotent across
//! encodings a password might be retyped in). An ASCII password is its own SASLprep normal
//! form (SASLprep is the identity function on ASCII), so this restriction costs nothing for
//! every password this module actually needs to support, and turns a silent correctness gap
//! into a refusal a deployment can act on.
//!
//! # Known-answer test
//!
//! This module's own `#[cfg(test)]` block asserts every stage of the exchange against RFC
//! 7677 section 3's own published vector (user `user`, password `pencil`, client nonce
//! `rOprNGfwEbeRWgbNEkqO`, server nonce continuation
//! `%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0`, salt `W22ZaJ0SNY7soEsUEjb6gQ==`, iteration count 4096)
//! character-for-character -- see that test module's own doc for exactly which strings are
//! quoted directly from the RFC.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use openssl::hash::MessageDigest;
use openssl::memcmp;
use openssl::pkcs5::pbkdf2_hmac;
use openssl::pkey::PKey;
use openssl::rand::rand_bytes;
use openssl::sha::sha256;
use openssl::sign::Signer;

use crate::error::CatalogError;

/// The GS2 header this client always sends -- see this module's own doc, "Channel binding",
/// for why it is always this value and never `-PLUS`.
pub const GS2_HEADER: &str = "n,,";

/// The SASL mechanism name this crate ever offers or accepts (`AuthenticationSASL`'s
/// mechanism list must contain this exact string, and `SASLInitialResponse` always names it).
pub const MECHANISM: &str = "SCRAM-SHA-256";

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<[u8; 32], CatalogError> {
    let pkey = PKey::hmac(key)?;
    let mut signer = Signer::new(MessageDigest::sha256(), &pkey)?;
    signer.update(data)?;
    let digest = signer.sign_to_vec()?;
    // `PKey::hmac` + `Signer` with `MessageDigest::sha256()` always produces exactly 32 bytes;
    // this is not a fallible cast, just a container-shape conversion (mirrors
    // `crates/av-store/src/sigv4.rs::hmac_sha256`'s identical precedent).
    Ok(digest.try_into().unwrap_or_else(|v: Vec<u8>| panic!("HMAC-SHA256 produced {} bytes, not 32", v.len())))
}

fn xor32(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// RFC 5802 section 5.1's `saslname` escaping: `=` becomes `=3D`, THEN (as a second, separate
/// pass over the already-`=`-escaped string, per the RFC's own ordering) `,` becomes `=2C`.
/// Doing the `=`-escape first matters: if the passes ran in the other order, a username
/// containing a literal `,` would become `=2C`, and the immediately following `=`-escape pass
/// would then mangle that synthesized `=` into `=3D2C` -- wrong. Running `=` first means the
/// second pass's own `=` (inside `=2C`) is never revisited, because each pass runs exactly
/// once.
fn escape_username(username: &str) -> String {
    username.replace('=', "=3D").replace(',', "=2C")
}

/// One attribute (`key=value`) from a SCRAM server message, split at the first `=` only --
/// `value` may itself contain further `=` characters (e.g. base64 padding), which is why this
/// is not a naive `split('=')`.
fn parse_attributes(s: &str) -> Vec<(char, &str)> {
    s.split(',')
        .filter_map(|part| {
            let key = part.chars().next()?;
            part[key.len_utf8()..].strip_prefix('=').map(|value| (key, value))
        })
        .collect()
}

fn find_attr<'a>(attrs: &[(char, &'a str)], key: char) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The result of [`ScramClient::client_final_message`]: the message to send as
/// `SASLResponse`'s data, and the `ServerSignature` this client independently computed and
/// expects the server's own `v=` (inside `AuthenticationSASLFinal`) to match --
/// [`verify_server_final`] is the function that checks it.
#[derive(Debug)]
pub struct ScramFinal {
    pub client_final_message: String,
    pub expected_server_signature: [u8; 32],
}

/// One client-side SCRAM-SHA-256 exchange: constructed once per connection attempt, carries
/// the username/password/client-nonce, and walks the two-message client side of RFC 5802's
/// exchange ([`Self::client_first_message`], then [`Self::client_final_message`] once the
/// server's first message is known). [`verify_server_final`] (a free function -- it needs no
/// state this struct owns beyond the `expected_server_signature` [`ScramFinal`] already
/// returned) checks the server's proof.
pub struct ScramClient {
    username: String,
    password: String,
    client_nonce: String,
}

/// Hand-written, not derived: a derived `Debug` would print `password` in plain text, which
/// this task's own tests (`Result::unwrap_err` requires `T: Debug` on the `Ok` type even when
/// only the `Err` branch is ever reached) would otherwise force into existence as a standing
/// footgun for any future caller who logs a `ScramClient` by accident.
impl std::fmt::Debug for ScramClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScramClient").field("username", &self.username).field("password", &"<redacted>").field("client_nonce", &self.client_nonce).finish()
    }
}

impl ScramClient {
    /// Builds a `ScramClient` with a fresh, randomly generated client nonce
    /// (`openssl::rand::rand_bytes`, 24 random bytes standard-base64-encoded -- every byte of
    /// that alphabet is RFC 5802 `printable` and excludes `,`, so no further escaping is
    /// needed). This is what `src/client.rs::PgClient::connect` calls; [`Self::
    /// with_client_nonce`] is the deterministic form this module's own tests use instead.
    pub fn new(username: &str, password: &str) -> Result<Self, CatalogError> {
        let mut nonce_bytes = [0u8; 24];
        rand_bytes(&mut nonce_bytes)?;
        Self::with_client_nonce(username, password, &BASE64.encode(nonce_bytes))
    }

    /// Builds a `ScramClient` with an explicitly given client nonce, so a test can reproduce a
    /// known exchange byte-for-byte (this task's own RFC 7677 known-answer test, below, and
    /// `tests/wire_protocol.rs`'s fake-server SCRAM handshake test both use this rather than
    /// [`Self::new`]). Refuses a non-ASCII `password` here, at construction -- before any
    /// network I/O -- rather than deep inside the PBKDF2 call: see this module's own doc,
    /// "SASLprep is not implemented".
    pub fn with_client_nonce(username: &str, password: &str, client_nonce: &str) -> Result<Self, CatalogError> {
        if !password.is_ascii() {
            return Err(CatalogError::NonAsciiPassword);
        }
        Ok(Self { username: username.to_string(), password: password.to_string(), client_nonce: client_nonce.to_string() })
    }

    /// `client-first-message-bare` (RFC 5802): `n=<escaped username>,r=<client nonce>`. Kept
    /// separate from [`Self::client_first_message`] because [`Self::client_final_message`]
    /// needs this exact substring (not the GS2-header-prefixed full message) as the first
    /// component of `AuthMessage`.
    pub fn client_first_bare(&self) -> String {
        format!("n={},r={}", escape_username(&self.username), self.client_nonce)
    }

    /// `client-first-message` (RFC 5802): [`GS2_HEADER`] immediately followed by
    /// [`Self::client_first_bare`] (no separator -- the GS2 header's own trailing `,,` already
    /// serves as one). This is the data `SASLInitialResponse` carries.
    pub fn client_first_message(&self) -> String {
        format!("{GS2_HEADER}{}", self.client_first_bare())
    }

    /// Processes the server's `server-first-message` (`AuthenticationSASLContinue`'s data,
    /// decoded as UTF-8 by the caller) and computes `client-final-message` (RFC 5802's
    /// `client-final-message-with-proof`) plus the `ServerSignature` this client expects back.
    ///
    /// Verifies the actual SCRAM MITM defence RFC 5802 section 5 requires: the server's nonce
    /// (`r=`) MUST start with the client's own nonce (a server, or an attacker relaying a
    /// stale/foreign exchange, that does not echo it back is refused --
    /// [`CatalogError::ScramServerNonceMismatch`] -- never silently accepted).
    pub fn client_final_message(&self, server_first_message: &str) -> Result<ScramFinal, CatalogError> {
        let attrs = parse_attributes(server_first_message);
        let server_nonce = find_attr(&attrs, 'r').ok_or(CatalogError::ScramServerFirstMalformed { reason: "missing r= (nonce)" })?;
        let salt_b64 = find_attr(&attrs, 's').ok_or(CatalogError::ScramServerFirstMalformed { reason: "missing s= (salt)" })?;
        let iteration_str = find_attr(&attrs, 'i').ok_or(CatalogError::ScramServerFirstMalformed { reason: "missing i= (iteration count)" })?;

        if !server_nonce.starts_with(self.client_nonce.as_str()) {
            return Err(CatalogError::ScramServerNonceMismatch);
        }
        let salt = BASE64.decode(salt_b64).map_err(|e| CatalogError::ScramInvalidSalt { reason: e.to_string() })?;
        let iteration_count: u32 = iteration_str.parse().ok().filter(|&n: &u32| n > 0).ok_or_else(|| CatalogError::ScramInvalidIterationCount { value: iteration_str.to_string() })?;

        let mut salted_password = [0u8; 32];
        pbkdf2_hmac(self.password.as_bytes(), &salt, iteration_count as usize, MessageDigest::sha256(), &mut salted_password)?;

        let client_key = hmac_sha256(&salted_password, b"Client Key")?;
        let stored_key = sha256(&client_key);

        // `channel-binding` = base64(cbind-input), where cbind-input is just the GS2 header
        // itself (no channel-binding data appended -- see this module's own doc, "Channel
        // binding"). Computed here, at runtime, from `GS2_HEADER` rather than hardcoding its
        // base64 form: this IS the RFC 7677 vector's well-known `c=biws`, produced by
        // construction instead of by copying a magic string.
        let client_final_message_without_proof = format!("c={},r={}", BASE64.encode(GS2_HEADER.as_bytes()), server_nonce);

        let auth_message = format!("{},{},{}", self.client_first_bare(), server_first_message, client_final_message_without_proof);

        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes())?;
        let client_proof = xor32(client_key, client_signature);
        let client_final_message = format!("{client_final_message_without_proof},p={}", BASE64.encode(client_proof));

        let server_key = hmac_sha256(&salted_password, b"Server Key")?;
        let expected_server_signature = hmac_sha256(&server_key, auth_message.as_bytes())?;

        Ok(ScramFinal { client_final_message, expected_server_signature })
    }
}

/// Verifies the server's `server-final-message` (`AuthenticationSASLFinal`'s data, decoded as
/// UTF-8 by the caller) against `expected_server_signature` (from [`ScramClient::
/// client_final_message`]'s own [`ScramFinal`]). This is SCRAM's mutual-authentication half:
/// proof that the party on the other end of the socket actually knows this user's
/// `SaltedPassword` (equivalently, `ServerKey`) -- not merely a copy of `StoredKey`, which is
/// all a server whose own password-verifier table leaked would have. Compared in constant time
/// (`openssl::memcmp::eq`): `ServerSignature` is a MAC, and MAC comparisons are exactly the
/// case constant-time comparison exists for.
///
/// A server that sends `e=<text>` instead of `v=<signature>` (RFC 5802's own explicit
/// server-side SCRAM failure report) is surfaced as [`CatalogError::ScramServerFinalMalformed`]
/// with that text included, not folded into the generic "signature mismatch" case -- a real
/// mismatch and a server-reported failure are different diagnoses.
pub fn verify_server_final(expected_server_signature: &[u8; 32], server_final_message: &str) -> Result<(), CatalogError> {
    let attrs = parse_attributes(server_final_message);
    if let Some(err_text) = find_attr(&attrs, 'e') {
        return Err(CatalogError::ScramServerFinalMalformed { reason: format!("server reported a SCRAM error: {err_text}") });
    }
    let sig_b64 = find_attr(&attrs, 'v').ok_or_else(|| CatalogError::ScramServerFinalMalformed { reason: "missing v= (server signature) and no e= error".to_string() })?;
    let actual = BASE64.decode(sig_b64).map_err(|e| CatalogError::ScramServerFinalMalformed { reason: format!("v= is not valid base64: {e}") })?;
    if actual.len() != 32 || !memcmp::eq(&actual, expected_server_signature) {
        return Err(CatalogError::ScramServerSignatureMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7677 section 3's own published SCRAM-SHA-256 example. Every `const` below whose name
    // starts with `RFC_` is quoted VERBATIM from the RFC text (username "user", password
    // "pencil", client nonce "rOprNGfwEbeRWgbNEkqO", full server nonce
    // "rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0", salt "W22ZaJ0SNY7soEsUEjb6gQ==",
    // iteration count 4096). This task's own binding rule: "If your implementation does not
    // reproduce the RFC's strings character for character, it is wrong -- fix it, never the
    // expectation" -- so every assertion below is `assert_eq!` against one of these constants,
    // never a value this test module invents.
    const RFC_USERNAME: &str = "user";
    const RFC_PASSWORD: &str = "pencil";
    const RFC_CLIENT_NONCE: &str = "rOprNGfwEbeRWgbNEkqO";
    const RFC_CLIENT_FIRST_MESSAGE: &str = "n,,n=user,r=rOprNGfwEbeRWgbNEkqO";
    const RFC_SERVER_FIRST_MESSAGE: &str = "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const RFC_CLIENT_FINAL_MESSAGE: &str = "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=";
    const RFC_SERVER_FINAL_MESSAGE: &str = "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";

    #[test]
    fn rfc7677_client_first_message_matches_the_published_vector() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        assert_eq!(client.client_first_message(), RFC_CLIENT_FIRST_MESSAGE);
    }

    #[test]
    fn rfc7677_client_final_message_matches_the_published_vector() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        let scram_final = client.client_final_message(RFC_SERVER_FIRST_MESSAGE).unwrap();
        assert_eq!(scram_final.client_final_message, RFC_CLIENT_FINAL_MESSAGE);
    }

    #[test]
    fn rfc7677_server_signature_verifies_against_the_published_vector() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        let scram_final = client.client_final_message(RFC_SERVER_FIRST_MESSAGE).unwrap();
        verify_server_final(&scram_final.expected_server_signature, RFC_SERVER_FINAL_MESSAGE).unwrap();
    }

    #[test]
    fn wrong_password_makes_the_server_signature_check_fail() {
        // Same salt/iteration/nonces as the RFC vector, but a different password: this
        // client's own expected server signature is now computed from a different
        // SaltedPassword than the RFC's, so it must NOT match the RFC's own published,
        // correct-password server-final-message.
        let client = ScramClient::with_client_nonce(RFC_USERNAME, "not-the-right-password", RFC_CLIENT_NONCE).unwrap();
        let scram_final = client.client_final_message(RFC_SERVER_FIRST_MESSAGE).unwrap();
        let err = verify_server_final(&scram_final.expected_server_signature, RFC_SERVER_FINAL_MESSAGE).unwrap_err();
        assert!(matches!(err, CatalogError::ScramServerSignatureMismatch));
        // And, for good measure, the client-final-message it produced (built from the wrong
        // password's ClientKey/StoredKey) is not the RFC's own published one either.
        assert_ne!(scram_final.client_final_message, RFC_CLIENT_FINAL_MESSAGE);
    }

    #[test]
    fn server_nonce_not_prefixed_by_client_nonce_is_a_typed_refusal() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        let forged_server_first = "r=totally-different-nonce,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let err = client.client_final_message(forged_server_first).unwrap_err();
        assert!(matches!(err, CatalogError::ScramServerNonceMismatch));
    }

    #[test]
    fn non_ascii_password_is_refused_at_construction() {
        let err = ScramClient::with_client_nonce(RFC_USERNAME, "p\u{e9}ncil", RFC_CLIENT_NONCE).unwrap_err();
        assert!(matches!(err, CatalogError::NonAsciiPassword));
        let err = ScramClient::new(RFC_USERNAME, "p\u{e9}ncil").unwrap_err();
        assert!(matches!(err, CatalogError::NonAsciiPassword));
    }

    #[test]
    fn missing_server_first_fields_are_typed_errors() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        for malformed in ["s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096", "r=rOprNGfwEbeRWgbNEkqOxyz,i=4096", "r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ=="] {
            let err = client.client_final_message(malformed).unwrap_err();
            assert!(matches!(err, CatalogError::ScramServerFirstMalformed { .. }), "{malformed:?} -> {err:?}");
        }
    }

    #[test]
    fn invalid_salt_and_iteration_count_are_typed_errors() {
        let client = ScramClient::with_client_nonce(RFC_USERNAME, RFC_PASSWORD, RFC_CLIENT_NONCE).unwrap();
        let bad_salt = "r=rOprNGfwEbeRWgbNEkqOxyz,s=not-valid-base64!!!,i=4096";
        assert!(matches!(client.client_final_message(bad_salt).unwrap_err(), CatalogError::ScramInvalidSalt { .. }));
        let bad_iter = "r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=not-a-number";
        assert!(matches!(client.client_final_message(bad_iter).unwrap_err(), CatalogError::ScramInvalidIterationCount { .. }));
        let zero_iter = "r=rOprNGfwEbeRWgbNEkqOxyz,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=0";
        assert!(matches!(client.client_final_message(zero_iter).unwrap_err(), CatalogError::ScramInvalidIterationCount { .. }));
    }

    #[test]
    fn server_reported_scram_error_is_distinguished_from_a_signature_mismatch() {
        let err = verify_server_final(&[0u8; 32], "e=invalid-proof").unwrap_err();
        assert!(matches!(err, CatalogError::ScramServerFinalMalformed { .. }));
    }

    #[test]
    fn escape_username_handles_equals_and_comma_in_the_rfc_5802_order() {
        assert_eq!(escape_username("plain"), "plain");
        assert_eq!(escape_username("a=b"), "a=3Db");
        assert_eq!(escape_username("a,b"), "a=2Cb");
        assert_eq!(escape_username("a=b,c"), "a=3Db=2Cc");
    }

    #[test]
    fn new_generates_a_nonce_and_two_calls_never_collide() {
        let a = ScramClient::new(RFC_USERNAME, RFC_PASSWORD).unwrap();
        let b = ScramClient::new(RFC_USERNAME, RFC_PASSWORD).unwrap();
        assert_ne!(a.client_first_message(), b.client_first_message());
    }
}

//! AWS Signature Version 4, on the system OpenSSL only.
//!
//! Every cryptographic primitive this module needs is SHA-256 or HMAC-SHA256, and both come
//! from the `openssl` crate alone (ADR-004): [`openssl::sha::sha256`] for hashing the
//! canonical request, and `openssl::pkey::PKey::hmac` + `openssl::sign::Signer` with
//! `MessageDigest::sha256()` for every step of the four-step signing-key derivation and for
//! the final signature. No `hmac` crate, no `sha2` crate, no `ring` -- SigV4 needs nothing
//! else, so this crate carries nothing else.
//!
//! The date/time stamp is always a **parameter**, never read from a clock in this module
//! (this task's rule 7, "clocks injected, never slept" -- restated here because a signing
//! module is exactly where a maintainer would otherwise reach for `SystemTime::now()`).
//! [`amz_date_from_unix_seconds`] converts the caller's own injected Unix-seconds clock
//! reading into SigV4's two date/time string forms by hand: the civil-from-days algorithm
//! below (Howard Hinnant's `civil_from_days`, a closed-form Gregorian calendar calculation
//! with no lookup table and no leap-second table -- SigV4 dates are plain UTC calendar
//! dates) is the whole implementation, so this crate never needs `chrono` or `time` (rule 2:
//! neither is in this workspace's pre-approved list, and a calendar conversion this small
//! does not justify adding either).
//!
//! # Known-answer test
//!
//! This module's own `#[cfg(test)]` block asserts every stage of the pipeline (canonical
//! request, string to sign, final signature, and the assembled `Authorization` header)
//! against AWS's own published SigV4 test suite vector `get-vanilla-query-order-key-case`
//! (access key `AKIDEXAMPLE`, secret `wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY`, region
//! `us-east-1`, service `service`, date `20150830T123600Z`, request `GET
//! /?Param2=value2&Param1=value1 HTTP/1.1`, host `example.amazonaws.com`) -- see that test's
//! own doc comment for exactly which values are AWS's own published artifacts versus this
//! crate's own cross-checked derivation, and for where the vector was fetched from.

use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sha::sha256;
use openssl::sign::Signer;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};

use crate::error::StoreError;
use crate::keys::hex_encode;

/// SigV4's `UriEncode` for a query-string key or value: RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`) pass through unencoded; every other byte -- `/` included -- is
/// percent-encoded (SigV4: "`/` encoded in query values"). Built from `NON_ALPHANUMERIC`
/// (which already encodes everything except ASCII alphanumerics, `/` included) with the four
/// non-alphanumeric unreserved characters removed from its encode set, rather than
/// enumerating the dozens of punctuation bytes that DO need encoding by hand.
const QUERY_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// [`QUERY_ENCODE_SET`] with `/` additionally left unencoded, for the canonical URI (SigV4:
/// "`/` NOT encoded in the path" -- the path's own segment separators must survive encoding
/// as literal `/` bytes, unlike a `/` that appears inside one query value).
const PATH_ENCODE_SET: &AsciiSet = &QUERY_ENCODE_SET.remove(b'/');

/// Percent-encodes `component` per SigV4's `UriEncode` rules for one query-string key or
/// value. `/` is encoded here (SigV4: "`/` encoded in query values").
fn uri_encode_query_component(component: &str) -> String {
    percent_encode(component.as_bytes(), QUERY_ENCODE_SET).to_string()
}

/// Percent-encodes `path` per SigV4's `UriEncode` rules for the canonical URI. `/` is left
/// unencoded (SigV4: "`/` NOT encoded in the path") -- [`PATH_ENCODE_SET`] is exactly
/// [`QUERY_ENCODE_SET`] with `/` additionally excluded, so applying it to the whole path
/// string encodes every byte that needs it while leaving every literal `/` (the segment
/// separators) alone.
fn uri_encode_path(path: &str) -> String {
    percent_encode(path.as_bytes(), PATH_ENCODE_SET).to_string()
}

/// The canonical URI for `path` (SigV4: `/` if `path` is empty, else `path` percent-encoded
/// per [`uri_encode_path`]). `pub(crate)` -- also used by `crate::client` to build the
/// *actual* request target, so the bytes a real request is sent to are provably the same
/// bytes [`canonical_request`] signed, never two independent encodings that could drift.
pub(crate) fn encoded_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        uri_encode_path(path)
    }
}

/// The canonical query string for `query_pairs`: each key and value percent-encoded, then
/// sorted by `(encoded key, encoded value)` (SigV4's own tie-break for a repeated key).
/// `pub(crate)` for the same reason as [`encoded_path`] -- `crate::client` builds the real
/// request's query string with this exact function, not a second copy of the sort/encode
/// logic.
pub(crate) fn encoded_sorted_query(query_pairs: &[(&str, &str)]) -> String {
    let mut encoded: Vec<(String, String)> = query_pairs.iter().map(|(k, v)| (uri_encode_query_component(k), uri_encode_query_component(v))).collect();
    encoded.sort();
    encoded.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

/// The four HMAC-SHA256 hex digests AWS's own SigV4 test suite publishes are quoted directly
/// in this file's test module; nothing above needs a doc comment repeating them.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<[u8; 32], StoreError> {
    let pkey = PKey::hmac(key)?;
    let mut signer = Signer::new(MessageDigest::sha256(), &pkey)?;
    signer.update(data)?;
    let digest = signer.sign_to_vec()?;
    // `PKey::hmac` + `Signer` with `MessageDigest::sha256()` always produces exactly 32
    // bytes; this is not a fallible cast, just a container-shape conversion.
    Ok(digest.try_into().unwrap_or_else(|v: Vec<u8>| panic!("HMAC-SHA256 produced {} bytes, not 32", v.len())))
}

/// The civil-from-days algorithm (Howard Hinnant's `civil_from_days`, public domain, widely
/// reproduced -- e.g. `https://howardhinnant.github.io/date_algorithms.html`): converts a day
/// count relative to the Unix epoch (1970-01-01 = day 0) into a proleptic-Gregorian
/// `(year, month, day)` triple, correct over the algorithm's documented range (years
/// -100000000..=100000000) with no lookup table, so it also gets 29 February and every
/// century/non-century leap-year rule right without a special case in this module for either.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Converts an injected Unix-seconds clock reading into SigV4's two date/time string forms:
/// the full timestamp (`20130524T000000Z`) used as the `x-amz-date` header value and in the
/// string to sign, and the bare date stamp (`20130524`) used in the credential scope. Never
/// reads a clock itself -- `secs` is always the caller's own value (rule 7).
pub fn amz_date_from_unix_seconds(secs: i64) -> (String, String) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let date_stamp = format!("{year:04}{month:02}{day:02}");
    let amz_date = format!("{date_stamp}T{hour:02}{minute:02}{second:02}Z");
    (amz_date, date_stamp)
}

/// One canonical request, plus the exact `SignedHeaders` string it implies (the two must
/// always travel together: signing a request with a `SignedHeaders` value that does not
/// match the headers actually canonicalized would silently produce a signature over the
/// wrong header set).
pub struct CanonicalRequest {
    pub text: String,
    pub signed_headers: String,
}

/// Builds a SigV4 canonical request (`docs/heavy-plan.md`/AWS's own "Task 1: Create a
/// canonical request").
///
/// - `path` is the raw (unencoded) absolute path, e.g. `"/"` or `"/bucket/key with spaces"`;
///   this function encodes it (`/` left unencoded).
/// - `query_pairs` is the raw (unencoded, caller-order) query parameters; this function
///   encodes each key and value, then sorts by `(encoded key, encoded value)` -- which is
///   also the tie-break AWS specifies for a repeated key.
/// - `headers` must already be exactly the set of headers this request will sign: lowercase
///   names, values already trimmed of leading/trailing whitespace. This function sorts them
///   by name and derives `SignedHeaders` from that same sorted list -- there is exactly one
///   place in this module that decides what "signed headers" means, never a second list a
///   caller could let drift from the first.
/// - `payload_hash_hex` is the lowercase hex SHA-256 of the request body (or of the empty
///   string for a bodyless request) -- always the real hash in this crate (rule: never
///   `UNSIGNED-PAYLOAD`, see `crate::client`'s module doc).
pub fn canonical_request(method: &str, path: &str, query_pairs: &[(&str, &str)], headers: &[(&str, &str)], payload_hash_hex: &str) -> CanonicalRequest {
    let canonical_uri = encoded_path(path);
    let canonical_query_string = encoded_sorted_query(query_pairs);

    let mut sorted_headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())).collect();
    sorted_headers.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = sorted_headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = sorted_headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

    let text = format!("{method}\n{canonical_uri}\n{canonical_query_string}\n{canonical_headers}\n{signed_headers}\n{payload_hash_hex}");
    CanonicalRequest { text, signed_headers }
}

/// Builds the SigV4 string to sign (AWS's own "Task 2"): `AWS4-HMAC-SHA256`, `amz_date`, the
/// credential scope `<date_stamp>/<region>/<service>/aws4_request`, and the hex SHA-256 of
/// `canonical_request_text` -- joined by `\n`, with no trailing newline.
pub fn string_to_sign(amz_date: &str, date_stamp: &str, region: &str, service: &str, canonical_request_text: &str) -> String {
    let hashed_canonical_request = hex_encode(&sha256(canonical_request_text.as_bytes()));
    format!("AWS4-HMAC-SHA256\n{amz_date}\n{date_stamp}/{region}/{service}/aws4_request\n{hashed_canonical_request}")
}

/// The four-step HMAC-SHA256 chain (AWS's own "Task 3: Derive a signing key"):
/// `HMAC(HMAC(HMAC(HMAC("AWS4" + secret, date_stamp), region), service), "aws4_request")`.
pub fn signing_key(secret_access_key: &str, date_stamp: &str, region: &str, service: &str) -> Result<[u8; 32], StoreError> {
    let k_date = hmac_sha256(format!("AWS4{secret_access_key}").as_bytes(), date_stamp.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, service.as_bytes())?;
    hmac_sha256(&k_service, b"aws4_request")
}

/// The final signature (AWS's own "Task 4", step 1-2): `HMAC(signing_key, string_to_sign)`,
/// hex-encoded lowercase.
pub fn sign(signing_key: &[u8; 32], string_to_sign_text: &str) -> Result<String, StoreError> {
    Ok(hex_encode(&hmac_sha256(signing_key, string_to_sign_text.as_bytes())?))
}

/// Assembles the `Authorization` header value AWS's own docs show
/// (`AWS4-HMAC-SHA256 Credential=.../SignedHeaders=...,Signature=...`).
pub fn authorization_header(access_key_id: &str, date_stamp: &str, region: &str, service: &str, signed_headers: &str, signature_hex: &str) -> String {
    format!("AWS4-HMAC-SHA256 Credential={access_key_id}/{date_stamp}/{region}/{service}/aws4_request, SignedHeaders={signed_headers}, Signature={signature_hex}")
}

/// The SHA-256 hex digest of the empty string -- every bodyless request (GET, HEAD, and a
/// PUT this crate never issues without a body) signs `x-amz-content-sha256` against this
/// exact value. Also, incidentally, the payload hash AWS's own published canonical request
/// for `get-vanilla-query-order-key-case` names literally (see the test module below).
pub fn empty_payload_hash_hex() -> String {
    hex_encode(&sha256(b""))
}

#[cfg(test)]
mod tests {
    use super::*;

    // AWS's own published SigV4 test suite vector `get-vanilla-query-order-key-case`
    // (access key AKIDEXAMPLE, secret wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY, region
    // us-east-1, service "service", date 20150830T123600Z, request
    // "GET /?Param2=value2&Param1=value1 HTTP/1.1", host example.amazonaws.com). Fetched for
    // this task from the public GitHub mirror `rhymu8354/aws-sig-v4-test-suite` (the original
    // AWS-published `aws4_testsuite.zip` test fixtures), files `get-vanilla-query-order-key-
    // case.{creq,sts,authz}` under `aws-sig-v4-test-suite/get-vanilla-query-order-key-case/`.
    //
    // What is directly an AWS-published artifact, quoted verbatim below: EXPECTED_CREQ (the
    // .creq file), EXPECTED_STS (the .sts file), and EXPECTED_SIGNATURE / the Signature=...
    // value inside EXPECTED_AUTHZ (the .authz file). EXPECTED_SIGNING_KEY_HEX is NOT itself a
    // separate published artifact in that test suite (no .signingkey file exists for this
    // vector) -- it is this crate's own `signing_key()` output for the published secret/date/
    // region/service, independently cross-checked (see this task's report) by computing the
    // same HMAC chain a second time in Python's stdlib `hmac`/`hashlib` (a different SigV4
    // implementation than this module's own) and confirming (a) that hashing EXPECTED_CREQ
    // reproduces the hashed-canonical-request line inside EXPECTED_STS exactly, and (b) that
    // HMAC-ing EXPECTED_STS with that derived key reproduces EXPECTED_SIGNATURE exactly. A
    // wrong signing key could not have produced AWS's own published signature, so this value
    // is exactly as trustworthy as EXPECTED_SIGNATURE itself, even though AWS's test suite
    // does not publish it as its own separate file.
    const EXPECTED_CREQ: &str = "GET\n/\nParam1=value1&Param2=value2\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const EXPECTED_STS: &str = "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\n816cd5b414d056048ba4f7c5386d6e0533120fb1fcfa93762cf0fc39e2cf19e0";
    const EXPECTED_SIGNING_KEY_HEX: &str = "938127b5336810ddb6a5d6af445fcac9e371f9ed418ed386b022aed82901be75";
    const EXPECTED_SIGNATURE: &str = "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500";
    const EXPECTED_AUTHZ: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500";

    #[test]
    fn amz_date_from_unix_seconds_matches_the_sigv4_vectors_own_timestamp() {
        // Ties this module's own date/time formatting to the same vector the rest of this
        // test module checks: 2015-08-30T12:36:00Z is Unix time 1440938160.
        assert_eq!(amz_date_from_unix_seconds(1_440_938_160), ("20150830T123600Z".to_string(), "20150830".to_string()));
    }

    /// Six-plus known instants (task requirement: "at least six ... including a leap-year 29
    /// Feb and a year boundary"), each independently computed from Python's stdlib
    /// `datetime` (a different, already-trusted calendar implementation) rather than by hand
    /// -- see this task's report for the exact script run.
    #[test]
    fn amz_date_from_unix_seconds_matches_known_instants() {
        let cases: &[(i64, &str, &str)] = &[
            (0, "19700101T000000Z", "19700101"),                 // Unix epoch
            (946_684_800, "20000101T000000Z", "20000101"),       // year boundary into a leap year
            (951_782_400, "20000229T000000Z", "20000229"),       // leap day, century year (2000 IS a leap year: divisible by 400)
            (-631_152_000, "19500101T000000Z", "19500101"),      // pre-epoch, exercises div_euclid/rem_euclid on a negative input
            (1_577_836_799, "20191231T235959Z", "20191231"),     // one second before a year boundary
            (1_577_836_800, "20200101T000000Z", "20200101"),     // the year boundary itself, into 2020 (an ordinary leap year)
            (-1, "19691231T235959Z", "19691231"),                // the second immediately before the Unix epoch
            (1_709_186_828, "20240229T060708Z", "20240229"),     // an ordinary (non-century) leap day
            (-2_203_891_200, "19000301T000000Z", "19000301"),    // 1900 is NOT a leap year (divisible by 100, not 400): the day after its Feb 28
        ];
        for (secs, expected_amz_date, expected_date_stamp) in cases {
            let (amz_date, date_stamp) = amz_date_from_unix_seconds(*secs);
            assert_eq!(&amz_date, expected_amz_date, "amz_date for {secs}");
            assert_eq!(&date_stamp, expected_date_stamp, "date_stamp for {secs}");
        }
    }

    fn vector_request() -> (String, String) {
        let query_pairs = [("Param2", "value2"), ("Param1", "value1")]; // AWS's own request line's order, unsorted
        let headers = [("host", "example.amazonaws.com"), ("x-amz-date", "20150830T123600Z")];
        let payload_hash = empty_payload_hash_hex();
        let creq = canonical_request("GET", "/", &query_pairs, &headers, &payload_hash);
        (creq.text, creq.signed_headers)
    }

    #[test]
    fn canonical_request_matches_the_published_vector() {
        let (text, signed_headers) = vector_request();
        assert_eq!(text, EXPECTED_CREQ);
        assert_eq!(signed_headers, "host;x-amz-date");
    }

    #[test]
    fn string_to_sign_matches_the_published_vector() {
        let (creq_text, _) = vector_request();
        let sts = string_to_sign("20150830T123600Z", "20150830", "us-east-1", "service", &creq_text);
        assert_eq!(sts, EXPECTED_STS);
    }

    #[test]
    fn signing_key_reproduces_the_derivation_this_task_cross_checked() {
        let key = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20150830", "us-east-1", "service").unwrap();
        assert_eq!(hex_encode(&key), EXPECTED_SIGNING_KEY_HEX);
    }

    #[test]
    fn sign_matches_the_published_final_signature() {
        let key = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20150830", "us-east-1", "service").unwrap();
        let signature = sign(&key, EXPECTED_STS).unwrap();
        assert_eq!(signature, EXPECTED_SIGNATURE);
    }

    #[test]
    fn authorization_header_matches_the_published_vector() {
        let key = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20150830", "us-east-1", "service").unwrap();
        let signature = sign(&key, EXPECTED_STS).unwrap();
        let header = authorization_header("AKIDEXAMPLE", "20150830", "us-east-1", "service", "host;x-amz-date", &signature);
        assert_eq!(header, EXPECTED_AUTHZ);
    }

    #[test]
    fn uri_encode_path_leaves_slashes_unencoded_but_encodes_a_space() {
        assert_eq!(uri_encode_path("/a b/c"), "/a%20b/c");
    }

    #[test]
    fn uri_encode_query_component_encodes_a_slash() {
        assert_eq!(uri_encode_query_component("a/b"), "a%2Fb");
    }
}

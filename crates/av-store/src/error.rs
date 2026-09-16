//! `StoreError`: the one error type for this crate (this task's binding rule 9 -- "no
//! `Box<dyn Error>`, no stringly-typed errors"). Every refusal named anywhere in this
//! crate's other modules -- an invalid object key, an oversized metadata header, a label
//! refusal, a corrupted payload, a non-2xx S3 response -- is a distinct variant here, never
//! a shared "other" catch-all string. `openssl::error::ErrorStack` is not `Clone`/`PartialEq`
//! (mirrors `crates/av-edge/src/sign.rs::SigningError::Openssl`'s own precedent), so every
//! variant that wraps one stringifies it immediately rather than storing the `ErrorStack`
//! itself.

use std::fmt;

/// Which side of a [`crate::labels::ClearanceLadder::authorize_read`] comparison a
/// [`StoreError::MarkingNotOnLadder`] is about. Named identically to
/// `crates/av-gateway/src/labels.rs::Side` (this crate's own module doc, `src/labels.rs`,
/// explains why that is the third, not the first, copy of this convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The reading principal's own claimed clearance ([`crate::labels::ClearanceLadder::
    /// authorize_read`]'s `caller_clearance` argument).
    Caller,
    /// The stored object's own configured [`av_cdm::pb::Label::marking`].
    Object,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Caller => write!(f, "caller"),
            Side::Object => write!(f, "object"),
        }
    }
}

/// Everything that can go wrong anywhere in `av-store`. See each variant's own doc for the
/// exact module/function that raises it.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    // -- src/keys.rs -------------------------------------------------------------------
    /// [`crate::keys::object_key`]'s prefix failed one of its four checks (leading/trailing
    /// `/`, `..`, an empty segment, or a non-ASCII byte). `reason` names which.
    #[error("object-key prefix {prefix:?} is invalid: {reason}")]
    InvalidPrefix { prefix: String, reason: &'static str },
    /// [`crate::keys::object_key`]'s `sha256_hex` argument is not exactly 64 lowercase hex
    /// characters.
    #[error("sha256 hex {hash:?} is invalid: {reason}")]
    InvalidHash { hash: String, reason: &'static str },

    // -- src/metadata.rs -----------------------------------------------------------------
    /// A `Label.marking` contains a byte outside printable ASCII (`crate::metadata`'s module
    /// doc: `x-amz-meta-av-marking` is read by a human running `mc stat`, so it is refused at
    /// encode time rather than mangled into an HTTP header MinIO would itself reject or
    /// silently re-encode).
    #[error("marking {marking:?} contains a non-ASCII or non-printable byte, refused before it reaches x-amz-meta-av-marking")]
    InvalidMarking { marking: String },
    /// One `x-amz-meta-av-*` header's encoded value is, by itself, over S3's per-object
    /// user-metadata budget (2 KiB). Checked first, per header, in [`crate::metadata::encode`]
    /// -- before the summed check below -- because a single oversized value (most likely
    /// `x-amz-meta-av-provenance`, if `Provenance.attributes` grows large) is a more specific
    /// diagnosis than reporting the total.
    #[error("{header} is {size} bytes, over S3's {limit}-byte user-metadata budget")]
    MetadataTooLarge { header: &'static str, size: usize, limit: usize },
    /// Every individual `x-amz-meta-av-*` header was under [`crate::metadata::
    /// USER_METADATA_BUDGET_BYTES`] on its own, but their SUM (name + value bytes, summed
    /// across every header [`crate::metadata::encode`] emits) is not -- S3's real budget is
    /// shared across the whole object's user metadata, not allotted separately per header.
    /// H1a's own brief asked for this check (H1b's review finding, this crate's module doc on
    /// `crate::metadata` used to admit only the narrower per-header reading was implemented).
    #[error("the sum of every x-amz-meta-av-* header's name+value bytes is {total} bytes, over S3's {limit}-byte SHARED user-metadata budget")]
    UserMetadataBudgetExceeded { total: usize, limit: usize },
    /// [`crate::metadata::decode`] found `header` present but could not decode its value
    /// (bad base64, or a `prost` decode failure on the decoded bytes).
    #[error("decoding {header}: {reason}")]
    MetadataDecode { header: &'static str, reason: String },
    /// [`crate::metadata::decode`] needed `header` and it was absent from the response.
    #[error("{header} is missing from the object's stored metadata")]
    MetadataMissing { header: &'static str },

    // -- src/labels.rs -----------------------------------------------------------------
    /// `marking` (either the caller's claimed clearance or the object's own label -- `side`
    /// names which) is not on this deployment's [`crate::labels::ClearanceLadder`]. Never
    /// defaulted to rank 0; see that module's doc for the two-sided rule.
    #[error("{side} marking {marking:?} is not on this deployment's clearance ladder -- refused, never defaulted to a rank")]
    MarkingNotOnLadder { side: Side, marking: String },
    /// Both markings are on the ladder, but the object's own rank outranks the caller's.
    #[error("object label {object_marking:?} outranks caller clearance {caller_clearance:?} on this deployment's clearance ladder")]
    OverClearance { object_marking: String, caller_clearance: String },

    // -- src/claim_check.rs --------------------------------------------------------------
    /// [`crate::claim_check::verify_payload`]: the recomputed SHA-256 does not match
    /// `AssetRef.sha256`, compared in constant time (`openssl::memcmp::eq`).
    #[error("payload hash mismatch: AssetRef says {expected}, recomputed {actual} over {size_bytes} bytes")]
    HashMismatch { expected: String, actual: String, size_bytes: u64 },
    /// [`crate::claim_check::verify_payload`]: the byte count does not match
    /// `AssetRef.size_bytes`, checked before the hash is even recomputed (a truncated or
    /// appended-to body is cheaper to catch this way, and a size mismatch is never a hash
    /// collision worth reporting as one).
    #[error("payload size mismatch: AssetRef says {expected} bytes, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    /// [`crate::claim_check::verify_payload`] (or [`crate::client::StoreClient::get`], which
    /// calls it): `AssetRef.sha256` itself is not well-formed 64-character lowercase hex, so
    /// there is nothing to compare against.
    #[error("AssetRef.sha256 {hash:?} is not well-formed 64-character lowercase hex: {reason}")]
    InvalidAssetHash { hash: String, reason: &'static str },

    // -- src/client.rs ---------------------------------------------------------------------
    /// An `AssetRef.uri` this crate itself never produced: not `s3://<bucket>/<key>`.
    #[error("asset uri {uri:?} is not a well-formed s3://bucket/key reference")]
    InvalidAssetUri { uri: String },
    /// An `AssetRef` with no `label` at all was passed to
    /// [`crate::client::StoreClient::get`], which cannot authorize a read against a label
    /// that does not exist.
    #[error("AssetRef for {uri:?} carries no label -- authorize_read has nothing to check against")]
    MissingLabel { uri: String },
    /// Building or configuring the OpenSSL client context failed (`SslConnector::builder`,
    /// `set_ca_file`, or -- for [`crate::sigv4`] -- constructing an HMAC `PKey`/`Signer`).
    /// Stringified immediately: see this module's own doc for why.
    #[error("OpenSSL error: {0}")]
    Tls(String),
    /// `http::Request::builder()...build()` (or a malformed `StoreConfig::endpoint`/object
    /// key) failed to produce a well-formed HTTP request.
    #[error("building the HTTP request: {0}")]
    Request(String),
    /// The TCP connection or the HTTP exchange itself failed (`hyper_util::client::legacy::
    /// Client::request`'s own error type -- connection refused, reset, a protocol violation).
    #[error("sending the HTTP request: {0}")]
    Connect(String),
    /// Reading the response body failed (`hyper::Error` from `BodyExt::collect`).
    #[error("reading the HTTP response body: {0}")]
    Body(String),
    /// [`crate::client::StoreClient::get`]/[`crate::client::StoreClient::head`] found no object
    /// at `uri`: S3/MinIO answered 404, or its `<Code>` was `NoSuchKey`/`NoSuchBucket` (H1b's
    /// review finding -- H1a's own brief already asked for this typed variant and it was
    /// missed; every 404 became an untyped [`StoreError::S3`] instead, indistinguishable by
    /// type from any other server error). Every OTHER non-2xx response is still `StoreError::
    /// S3`, unchanged.
    #[error("no object at {uri:?}: S3 {status} {code}")]
    NotFound { uri: String, status: u16, code: String },
    /// A non-2xx S3/MinIO response, with its status and the `<Code>`/`<Message>`/
    /// `<RequestId>` [`crate::client::parse_s3_error_xml`] extracted from the body (or the
    /// placeholders that function documents when the body did not parse as S3's error XML
    /// shape at all).
    #[error("S3 {status} {code}: {message} (request id {request_id})")]
    S3 { status: u16, code: String, message: String, request_id: String },
}

impl From<openssl::error::ErrorStack> for StoreError {
    fn from(e: openssl::error::ErrorStack) -> Self {
        StoreError::Tls(e.to_string())
    }
}

impl From<http::Error> for StoreError {
    fn from(e: http::Error) -> Self {
        StoreError::Request(e.to_string())
    }
}

impl From<http::uri::InvalidUri> for StoreError {
    fn from(e: http::uri::InvalidUri) -> Self {
        StoreError::Request(e.to_string())
    }
}

impl From<hyper_util::client::legacy::Error> for StoreError {
    fn from(e: hyper_util::client::legacy::Error) -> Self {
        StoreError::Connect(e.to_string())
    }
}

impl From<hyper::Error> for StoreError {
    fn from(e: hyper::Error) -> Self {
        StoreError::Body(e.to_string())
    }
}

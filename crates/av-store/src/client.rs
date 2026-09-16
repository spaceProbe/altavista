//! The S3 client: `StoreConfig` + `StoreClient`, and the four operations this round needs
//! (`put`, `get`, `head`, `ensure_bucket`).
//!
//! # TLS is the system OpenSSL; `http://` is a first-class endpoint, not a hack
//!
//! `StoreClient::new` always builds one `hyper_openssl::client::legacy::HttpsConnector<
//! HttpConnector>` (configured exactly the way `crates/av-grpc/src/tls.rs::connect` builds
//! its own `SslConnector`: `SslMethod::tls_client()`, the caller's `ca_file` if given,
//! `SslVerifyMode::PEER`) and uses it for **every** request, regardless of
//! `StoreConfig::endpoint`'s scheme. This is not an approximation: `HttpsConnector<S>`'s own
//! `Service<Uri>` implementation (`hyper-openssl` 0.10.2, `src/client/legacy.rs`) inspects
//! each request's URI scheme itself and only performs the TLS handshake when it is
//! `https://` -- an `http://` URI is passed straight through to the inner `HttpConnector`
//! with no TLS at all. So one client type serves both a real `https://` deployment and the
//! next task's `http://127.0.0.1:<ephemeral>` MinIO container with no branch in this crate's
//! own code, which is exactly the seam that task needs (`docs/heavy-plan.md` H1's own
//! "Seams the next task needs": "`StoreClient::new` must accept a plain-HTTP endpoint").
//! Never `rustls`, never `ring` -- ADR-004.
//!
//! # The clock is a parameter, never a read
//!
//! Every public method that signs a request takes `now_unix_secs: i64` and passes it
//! straight to [`crate::sigv4::amz_date_from_unix_seconds`]; nothing in this crate ever calls
//! `SystemTime::now()`, and `StoreClient` itself carries no clock field at all -- this is
//! rule 7 ("clocks injected, never slept"). It is also what makes this crate's own tests
//! reproducible with no wall-clock dependency: a caller (this crate's own `#[tokio::test]`s,
//! and the next task's MinIO integration test) can sign a request as if it were any instant.
//!
//! # `x-amz-content-sha256` is always the real payload hash
//!
//! Every request this crate signs sets `x-amz-content-sha256` to the actual SHA-256 of what
//! is being sent (the real payload for `put`, [`crate::sigv4::empty_payload_hash_hex`] for a
//! bodyless `get`/`head`/`ensure_bucket`) -- never the S3-specific `UNSIGNED-PAYLOAD` literal
//! some S3 clients use to skip hashing a large upload. `docs/heavy-plan.md` H1's own brief
//! states this explicitly ("never `UNSIGNED-PAYLOAD`"): this round's payloads are already
//! fully buffered in memory before `put` is called (no streaming upload yet), so there is no
//! performance reason to skip the hash, and skipping it would also mean the signature no
//! longer commits to the body at all.

use std::path::PathBuf;

use av_cdm::pb::{AssetRef, Label, Provenance};
use bytes::Bytes;
use http::{HeaderMap, Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper_openssl::client::legacy::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as LegacyClient;
use hyper_util::rt::TokioExecutor;
use openssl::sha::sha256;
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};

use crate::claim_check::{asset_ref_for, verify_payload};
use crate::error::StoreError;
use crate::keys::{hex_encode, object_key};
use crate::labels::ClearanceLadder;
use crate::metadata::{self, ObjectMetadata};
use crate::sigv4;

type HttpClient = LegacyClient<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Everything needed to reach one bucket in one S3-API-speaking object store. Every field is
/// plain data (no global state anywhere in this crate) -- constructing two `StoreClient`s
/// from two different `StoreConfig`s talks to two independent stores, sharing nothing.
pub struct StoreConfig {
    /// Scheme + authority the client connects to (`https://minio.example.com:9000`, or the
    /// next task's `http://127.0.0.1:<ephemeral>`). Never includes a path.
    pub endpoint: Uri,
    /// The SigV4 region string (MinIO accepts any value here; a real S3 deployment requires
    /// the bucket's actual region).
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    /// `true` for `<endpoint>/<bucket>/<key>` (path-style -- what MinIO's own docs recommend
    /// and what the next task's integration test uses); `false` for
    /// `<scheme>://<bucket>.<endpoint-host>/<key>` (virtual-hosted-style, real AWS S3's
    /// modern default). Controls both the request's `Host` header and its path -- see
    /// [`StoreClient::host_and_path`].
    pub force_path_style: bool,
    /// PEM CA bundle verifying the endpoint's TLS certificate. `None` uses OpenSSL's own
    /// default trust store. Ignored entirely for an `http://` endpoint (there is no TLS
    /// handshake to verify).
    pub ca_file: Option<PathBuf>,
    /// Prepended to every object's content-addressed key ([`crate::keys::object_key`]'s own
    /// `prefix` argument) -- lets more than one logical collection share one bucket.
    pub key_prefix: String,
}

/// An S3-API client for one [`StoreConfig`]. See this module's own doc for the TLS story and
/// the clock-injection rule.
pub struct StoreClient {
    config: StoreConfig,
    http: HttpClient,
}

impl StoreClient {
    /// Builds the underlying `HttpsConnector`/`hyper_util` client. Does not perform any I/O
    /// itself (no connection is opened until the first request) -- an error here is always a
    /// local OpenSSL configuration failure (`config.ca_file` unreadable or malformed).
    pub fn new(config: StoreConfig) -> Result<Self, StoreError> {
        let mut ssl = SslConnector::builder(SslMethod::tls_client())?;
        if let Some(ca_file) = &config.ca_file {
            ssl.set_ca_file(ca_file)?;
        }
        ssl.set_verify(SslVerifyMode::PEER);

        let mut http_connector = HttpConnector::new();
        http_connector.enforce_http(false); // the connector must accept `https://` URIs too
        let https = HttpsConnector::with_connector(http_connector, ssl)?;

        let http = LegacyClient::builder(TokioExecutor::new()).build(https);
        Ok(Self { config, http })
    }

    /// The `Host` header value and the raw (unencoded) absolute path for `key` (or for the
    /// bucket itself, when `key` is `""` -- [`Self::ensure_bucket`]'s own case), under this
    /// config's [`StoreConfig::force_path_style`] setting.
    fn host_and_path(&self, key: &str) -> (String, String) {
        let authority = self.config.endpoint.authority().map(|a| a.as_str()).unwrap_or("");
        if self.config.force_path_style {
            let path = if key.is_empty() { format!("/{}", self.config.bucket) } else { format!("/{}/{key}", self.config.bucket) };
            (authority.to_string(), path)
        } else {
            let host = format!("{}.{authority}", self.config.bucket);
            let path = if key.is_empty() { "/".to_string() } else { format!("/{key}") };
            (host, path)
        }
    }

    /// Signs and sends one request against `key` (or the bucket root, for
    /// [`Self::ensure_bucket`]): builds the canonical request (no query string -- none of
    /// this round's four operations need one), derives the `Authorization` header, and
    /// issues the request with `body` as its content. `extra_headers` are the headers beyond
    /// `host`/`x-amz-date`/`x-amz-content-sha256` (which this function always adds itself,
    /// and always signs) that this request both sends and signs -- the `x-amz-meta-*`
    /// metadata headers for a `put`, empty for `get`/`head`/`ensure_bucket`. Every `x-amz-*`
    /// header this crate ever sends is in this signed set; S3 requires exactly that (AWS's
    /// own docs: "Any `x-amz-*` headers that you plan to include in your request must also
    /// be added" to the canonical headers), so there is no path here that could send an
    /// unsigned `x-amz-*` header even by omission.
    async fn send(&self, method: Method, key: &str, extra_headers: &[(&str, &str)], body_hash_hex: &str, body: Bytes, now_unix_secs: i64) -> Result<(StatusCode, HeaderMap, Bytes), StoreError> {
        let (host, path) = self.host_and_path(key);
        let (amz_date, date_stamp) = sigv4::amz_date_from_unix_seconds(now_unix_secs);

        let mut sign_headers: Vec<(&str, &str)> = vec![("host", &host), ("x-amz-date", &amz_date), ("x-amz-content-sha256", body_hash_hex)];
        sign_headers.extend_from_slice(extra_headers);

        let creq = sigv4::canonical_request(method.as_str(), &path, &[], &sign_headers, body_hash_hex);
        let sts = sigv4::string_to_sign(&amz_date, &date_stamp, &self.config.region, "s3", &creq.text);
        let signing_key = sigv4::signing_key(&self.config.secret_access_key, &date_stamp, &self.config.region, "s3")?;
        let signature = sigv4::sign(&signing_key, &sts)?;
        let authorization = sigv4::authorization_header(&self.config.access_key_id, &date_stamp, &self.config.region, "s3", &creq.signed_headers, &signature);

        let scheme = self.config.endpoint.scheme_str().unwrap_or("http");
        let uri: Uri = format!("{scheme}://{host}{}", sigv4::encoded_path(&path)).parse()?;

        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", &host)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", body_hash_hex)
            .header("authorization", authorization);
        for (name, value) in extra_headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(Full::new(body))?;

        let response = self.http.request(request).await?;
        let status = response.status();
        let response_headers = response.headers().clone();
        let body_bytes = response.into_body().collect().await?.to_bytes();
        Ok((status, response_headers, body_bytes))
    }

    /// PUT the bucket itself, tolerating `BucketAlreadyOwnedByYou`/`BucketAlreadyExists`
    /// (MinIO's/S3's own "this bucket exists and you already own it" responses to a repeat
    /// `ensure_bucket` call, which is exactly the idempotent "make sure it exists" contract
    /// this method's name promises -- a second call is not a defect to surface as an error).
    pub async fn ensure_bucket(&self, now_unix_secs: i64) -> Result<(), StoreError> {
        let (status, _headers, body) = self.send(Method::PUT, "", &[], &sigv4::empty_payload_hash_hex(), Bytes::new(), now_unix_secs).await?;
        if status.is_success() {
            return Ok(());
        }
        let s3_err = parse_s3_error(status, &body);
        if s3_err.code == "BucketAlreadyOwnedByYou" || s3_err.code == "BucketAlreadyExists" {
            return Ok(());
        }
        Err(s3_err.into_store_error(&format!("s3://{}/", self.config.bucket)))
    }

    /// Hashes `bytes`, derives its content-addressed key, signs and sends a PUT carrying the
    /// label/provenance/media-type metadata headers ([`metadata::encode`]), and returns the
    /// resulting [`AssetRef`] -- the claim check a hot-track message would carry instead of
    /// `bytes` itself.
    pub async fn put(&self, bytes: Bytes, media_type: &str, label: Label, provenance: Provenance, now_unix_secs: i64) -> Result<AssetRef, StoreError> {
        let sha256_hex = hex_encode(&sha256(&bytes));
        let key = object_key(&self.config.key_prefix, &sha256_hex)?;
        let asset = asset_ref_for(&self.config.bucket, &key, &bytes, media_type, label.clone(), provenance.clone());

        let meta_headers = metadata::encode(&ObjectMetadata { sha256_hex: asset.sha256.clone(), label, provenance, media_type: media_type.to_string() })?;
        let headers: Vec<(&str, &str)> = meta_headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        let (status, _headers, body) = self.send(Method::PUT, &key, &headers, &asset.sha256, bytes, now_unix_secs).await?;
        if !status.is_success() {
            return Err(parse_s3_error(status, &body).into_store_error(&asset.uri));
        }
        Ok(asset)
    }

    /// Authorizes the read **before fetching a single byte** (`ladder.authorize_read`
    /// against `asset.label`, refusing with no request sent at all if it fails), then GETs
    /// the object and finally [`verify_payload`]s the bytes that came back. This order --
    /// label check, then fetch, then hash check -- is deliberate on both ends: checking the
    /// label first means a caller without clearance never causes this crate to transfer
    /// bytes it should not have asked for in the first place (no network cost for a refusal
    /// this crate can already see coming from the `AssetRef` alone, with no dependence on
    /// what MinIO itself would have done); checking the hash only *after* the fetch is the
    /// only order possible (there is nothing to hash before the bytes exist), but doing it as
    /// the last step means a caller only ever sees bytes that have already cleared both the
    /// authorization and integrity checks -- there is no window where partially-checked bytes
    /// are handed back.
    pub async fn get(&self, asset: &AssetRef, caller_clearance: &str, ladder: &ClearanceLadder, now_unix_secs: i64) -> Result<Bytes, StoreError> {
        let label = asset.label.as_ref().ok_or_else(|| StoreError::MissingLabel { uri: asset.uri.clone() })?;
        ladder.authorize_read(caller_clearance, label)?;

        let key = key_from_uri(&asset.uri)?;
        let (status, _headers, body) = self.send(Method::GET, &key, &[], &sigv4::empty_payload_hash_hex(), Bytes::new(), now_unix_secs).await?;
        if !status.is_success() {
            return Err(parse_s3_error(status, &body).into_store_error(&asset.uri));
        }
        verify_payload(asset, &body)?;
        Ok(body)
    }

    /// Reconstructs an [`AssetRef`] from a HEAD response's own stored metadata headers
    /// ([`metadata::decode`]) and its `Content-Length` -- no bytes are fetched.
    /// `spatial_extent`/`temporal_extent`/`attributes` are carried over from `asset` as given
    /// (S3 user metadata does not store them in this round -- see
    /// [`crate::claim_check::asset_ref_for`]'s own doc), not reconstructed from the response.
    pub async fn head(&self, asset: &AssetRef, now_unix_secs: i64) -> Result<AssetRef, StoreError> {
        let key = key_from_uri(&asset.uri)?;
        let (status, headers, body) = self.send(Method::HEAD, &key, &[], &sigv4::empty_payload_hash_hex(), Bytes::new(), now_unix_secs).await?;
        if !status.is_success() {
            return Err(parse_s3_error(status, &body).into_store_error(&asset.uri));
        }
        let header_pairs = headers.iter().filter_map(|(name, value)| value.to_str().ok().map(|v| (name.as_str(), v)));
        let meta = metadata::decode(header_pairs)?;
        let size_bytes = headers.get(http::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        Ok(AssetRef {
            uri: asset.uri.clone(),
            sha256: meta.sha256_hex,
            size_bytes,
            media_type: meta.media_type,
            label: Some(meta.label),
            spatial_extent: asset.spatial_extent.clone(),
            temporal_extent: asset.temporal_extent, // TemporalExtent (two `i64`s) is Copy
            provenance: Some(meta.provenance),
            attributes: asset.attributes.clone(),
        })
    }
}

/// Parses `s3://<bucket>/<key>` back into its key (the bucket is this client's own
/// [`StoreConfig::bucket`] -- this crate does not support cross-bucket `AssetRef`s in this
/// round, matching [`crate::claim_check::asset_ref_for`]'s own single-bucket contract).
fn key_from_uri(uri: &str) -> Result<String, StoreError> {
    let rest = uri.strip_prefix("s3://").ok_or_else(|| StoreError::InvalidAssetUri { uri: uri.to_string() })?;
    let (_bucket, key) = rest.split_once('/').ok_or_else(|| StoreError::InvalidAssetUri { uri: uri.to_string() })?;
    if key.is_empty() {
        return Err(StoreError::InvalidAssetUri { uri: uri.to_string() });
    }
    Ok(key.to_string())
}

/// One S3/MinIO error response, decomposed. See [`parse_s3_error_xml`] for how `code`/
/// `message`/`request_id` are extracted from the body.
struct S3Error {
    status: StatusCode,
    code: String,
    message: String,
    request_id: String,
}

impl S3Error {
    /// Maps this parsed error onto the crate's one typed [`StoreError`], given the `uri` the
    /// request was about (`"s3://<bucket>/<key>"`, or the bucket root for
    /// [`StoreClient::ensure_bucket`]). A 404 status, or an S3 `<Code>` of `NoSuchKey` /
    /// `NoSuchBucket` at any status, becomes [`StoreError::NotFound`] (review finding -- H1a's
    /// own brief asked for this and it was missed, see that variant's own doc comment); every
    /// other non-2xx stays [`StoreError::S3`], exactly as before this fix.
    fn into_store_error(self, uri: &str) -> StoreError {
        if self.status == StatusCode::NOT_FOUND || self.code == "NoSuchKey" || self.code == "NoSuchBucket" {
            return StoreError::NotFound { uri: uri.to_string(), status: self.status.as_u16(), code: self.code };
        }
        StoreError::S3 { status: self.status.as_u16(), code: self.code, message: self.message, request_id: self.request_id }
    }
}

fn parse_s3_error(status: StatusCode, body: &[u8]) -> S3Error {
    let text = String::from_utf8_lossy(body);
    let (code, message, request_id) = parse_s3_error_xml(&text);
    S3Error { status, code, message, request_id }
}

/// Hand-rolled extractor for S3's/MinIO's `<Error><Code>...</Code><Message>...</Message>
/// ...<RequestId>...</RequestId></Error>` XML shape -- no XML crate (this task's brief is
/// explicit: "no XML crate"), because this shape is three flat, non-nested, non-attributed
/// tags, and a real XML parser's generality (namespaces, CDATA, entity references beyond the
/// handful S3 actually emits) buys nothing here. Falls back to the literal string
/// `"unparsed"` for any tag not found (rather than failing outright), since a non-2xx
/// response is already going to become a [`StoreError::S3`] either way, and a caller is
/// better served by the real HTTP status plus whatever this extractor could find than by a
/// second-order parse failure obscuring the original error.
pub(crate) fn parse_s3_error_xml(xml: &str) -> (String, String, String) {
    (
        extract_tag(xml, "Code").unwrap_or_else(|| "unparsed".to_string()),
        extract_tag(xml, "Message").unwrap_or_else(|| "unparsed".to_string()),
        extract_tag(xml, "RequestId").unwrap_or_else(|| "unparsed".to_string()),
    )
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end_rel = xml[start..].find(&close)?;
    Some(xml[start..start + end_rel].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative MinIO/S3 `<Error>` response body, in the documented S3 API shape
    /// (`services/store/IMAGE_DIGEST.md`: MinIO speaks the real S3 error XML shape, not a
    /// bespoke one) -- hand-authored from that documented shape rather than captured from a
    /// live container, since this task's tests run docker-free (the next task's MinIO
    /// integration test is where a byte-for-byte captured fixture would come from instead).
    const MINIO_NO_SUCH_KEY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>imagery/ab/cd/abcd...</Key><BucketName>altavista-heavy</BucketName><Resource>/altavista-heavy/imagery/ab/cd/abcd...</Resource><RequestId>176C2A7F3E2B4B11</RequestId><HostId>dd9025bab4ad464b049177c95eb6ebf374d3b3fd1af9251148b658df7ac2e3e8</HostId></Error>"#;

    const MINIO_BUCKET_ALREADY_OWNED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>BucketAlreadyOwnedByYou</Code><Message>Your previous request to create the named bucket succeeded and you already own it.</Message><BucketName>altavista-heavy</BucketName><RequestId>176C2A7F3E2B4B12</RequestId><HostId>dd9025bab4ad464b049177c95eb6ebf374d3b3fd1af9251148b658df7ac2e3e8</HostId></Error>"#;

    #[test]
    fn parse_s3_error_xml_extracts_code_message_and_request_id() {
        let (code, message, request_id) = parse_s3_error_xml(MINIO_NO_SUCH_KEY);
        assert_eq!(code, "NoSuchKey");
        assert_eq!(message, "The specified key does not exist.");
        assert_eq!(request_id, "176C2A7F3E2B4B11");
    }

    #[test]
    fn parse_s3_error_xml_extracts_bucket_already_owned_by_you() {
        let (code, _message, _request_id) = parse_s3_error_xml(MINIO_BUCKET_ALREADY_OWNED);
        assert_eq!(code, "BucketAlreadyOwnedByYou");
    }

    #[test]
    fn parse_s3_error_xml_falls_back_to_unparsed_for_a_body_that_is_not_this_shape_at_all() {
        let (code, message, request_id) = parse_s3_error_xml("not xml at all");
        assert_eq!(code, "unparsed");
        assert_eq!(message, "unparsed");
        assert_eq!(request_id, "unparsed");
    }

    /// Review finding (H1b's brief item 1): a 404 with `<Code>NoSuchKey</Code>` must map onto
    /// the typed [`StoreError::NotFound`], not the untyped [`StoreError::S3`] H1a shipped with.
    #[test]
    fn into_store_error_maps_a_404_no_such_key_to_store_error_not_found() {
        let err = parse_s3_error(StatusCode::NOT_FOUND, MINIO_NO_SUCH_KEY.as_bytes()).into_store_error("s3://altavista-heavy/imagery/ab/cd/abcd...");
        match err {
            StoreError::NotFound { uri, status, code } => {
                assert_eq!(uri, "s3://altavista-heavy/imagery/ab/cd/abcd...");
                assert_eq!(status, 404);
                assert_eq!(code, "NoSuchKey");
            }
            other => panic!("expected StoreError::NotFound, got {other:?}"),
        }
    }

    /// `NoSuchBucket` maps to `NotFound` too, even checked against a status other than 404 --
    /// the code alone is enough (S3/MinIO do not always pair `NoSuchBucket` with a literal 404
    /// in every response shape), never only the numeric status.
    #[test]
    fn into_store_error_maps_no_such_bucket_to_not_found_by_code_alone() {
        let body = r#"<Error><Code>NoSuchBucket</Code><Message>m</Message><RequestId>ID</RequestId></Error>"#;
        let err = parse_s3_error(StatusCode::NOT_FOUND, body.as_bytes()).into_store_error("s3://altavista-heavy/");
        assert!(matches!(err, StoreError::NotFound { .. }), "{err:?}");
    }

    /// Every OTHER non-2xx response is unaffected by this fix -- still the untyped
    /// [`StoreError::S3`], exactly as before (the review finding's own "keeping StoreError::S3
    /// for every other non-2xx" requirement).
    #[test]
    fn into_store_error_leaves_every_other_non_2xx_as_store_error_s3() {
        let body = r#"<Error><Code>AccessDenied</Code><Message>m</Message><RequestId>ID</RequestId></Error>"#;
        let err = parse_s3_error(StatusCode::FORBIDDEN, body.as_bytes()).into_store_error("s3://altavista-heavy/imagery/x");
        assert!(matches!(err, StoreError::S3 { status: 403, .. }), "{err:?}");
    }

    #[test]
    fn key_from_uri_extracts_the_key_after_the_bucket() {
        assert_eq!(key_from_uri("s3://altavista-heavy/imagery/ab/cd/abcd...").unwrap(), "imagery/ab/cd/abcd...");
    }

    #[test]
    fn key_from_uri_refuses_a_non_s3_scheme() {
        let err = key_from_uri("https://example.com/x").unwrap_err();
        assert!(matches!(err, StoreError::InvalidAssetUri { .. }), "{err:?}");
    }

    #[test]
    fn key_from_uri_refuses_a_uri_with_no_key() {
        let err = key_from_uri("s3://bucket-only").unwrap_err();
        assert!(matches!(err, StoreError::InvalidAssetUri { .. }), "{err:?}");
    }

    fn test_config(force_path_style: bool) -> StoreConfig {
        StoreConfig {
            endpoint: "http://127.0.0.1:9000".parse().unwrap(),
            region: "us-east-1".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_access_key: "minioadmin".to_string(),
            bucket: "altavista-heavy".to_string(),
            force_path_style,
            ca_file: None,
            key_prefix: "imagery".to_string(),
        }
    }

    #[test]
    fn store_client_new_accepts_a_plain_http_endpoint() {
        // The seam the next task's MinIO integration test needs: StoreClient::new must not
        // require an https:// endpoint or fail on one that is http://.
        StoreClient::new(test_config(true)).unwrap();
    }

    #[test]
    fn host_and_path_uses_path_style_when_configured() {
        let client = StoreClient::new(test_config(true)).unwrap();
        let (host, path) = client.host_and_path("ab/cd/abcd...");
        assert_eq!(host, "127.0.0.1:9000");
        assert_eq!(path, "/altavista-heavy/ab/cd/abcd...");
    }

    #[test]
    fn host_and_path_uses_virtual_hosted_style_when_configured() {
        let mut config = test_config(false);
        config.endpoint = "https://s3.us-east-1.amazonaws.com".parse().unwrap();
        let client = StoreClient::new(config).unwrap();
        let (host, path) = client.host_and_path("ab/cd/abcd...");
        assert_eq!(host, "altavista-heavy.s3.us-east-1.amazonaws.com");
        assert_eq!(path, "/ab/cd/abcd...");
    }

    /// `get` must refuse an over-clearance read with no HTTP request sent at all (no
    /// listening server is even started in this test -- if `get` tried to connect before
    /// checking the label, this test would fail with a connection error instead of the
    /// typed [`StoreError::OverClearance`] it asserts, since nothing is listening on
    /// 127.0.0.1:9000 in this test process).
    #[tokio::test]
    async fn get_refuses_an_over_clearance_read_before_any_request_is_sent() {
        let client = StoreClient::new(test_config(true)).unwrap();
        let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "SECRET".to_string()]);
        let asset = AssetRef {
            uri: "s3://altavista-heavy/imagery/ab/cd/abcd...".to_string(),
            sha256: "0".repeat(64),
            size_bytes: 0,
            media_type: "application/octet-stream".to_string(),
            label: Some(Label { marking: "SECRET".to_string(), caveats: vec![] }),
            ..Default::default()
        };
        let err = client.get(&asset, "UNCLASSIFIED", &ladder, 0).await.unwrap_err();
        assert!(matches!(err, StoreError::OverClearance { .. }), "{err:?}");
    }
}

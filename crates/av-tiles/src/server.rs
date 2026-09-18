//! The HTTP surface: a plain `tokio::net::TcpListener`, manual `GET`-only request-line/header
//! parsing -- byte for byte the pattern `crates/av-command/src/admin.rs` already establishes
//! for this workspace (that module's own doc: no `axum`/`hyper` direct dependency, "even
//! less reason here than there to pin a heavier HTTP stack for two fixed routes" -- this
//! crate serves exactly two routes too). Unlike `admin.rs`, this module must read specific
//! headers back out (`Authorization`, and P2's `Range`/`If-None-Match`) rather than
//! discarding the whole header block -- see [`read_request_line_and_headers`].

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::config::TilesConfig;
use crate::core::{handle, TileResponse};
use crate::counters::Counters;
use crate::source::ObjectSource;

/// Every header this module reads back out of a request, before parsing them into
/// [`crate::core::RequestHeaders`] proper.
#[derive(Debug, Default)]
struct RawHeaders {
    authorization: Option<String>,
    range: Option<String>,
    if_none_match: Option<String>,
}

/// Reads the request line and every header, returning `(method, path, headers)` -- each of
/// [`RawHeaders`]' three fields is the raw header value (case-insensitive name match, per
/// RFC 9110; leading/trailing whitespace on the value trimmed), or `None` if absent. Every
/// OTHER header is read and discarded (this crate's two routes take no other header input).
/// `None` overall means the peer closed before sending a request line at all.
async fn read_request_line_and_headers(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<(String, String, RawHeaders)>> {
    let mut request_line = String::new();
    if stream.read_line(&mut request_line).await? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers = RawHeaders::default();
    loop {
        let mut line = String::new();
        if stream.read_line(&mut line).await? == 0 {
            break;
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("authorization") {
                headers.authorization = Some(value);
            } else if name.eq_ignore_ascii_case("range") {
                headers.range = Some(value);
            } else if name.eq_ignore_ascii_case("if-none-match") {
                headers.if_none_match = Some(value);
            }
        }
    }
    Ok(Some((method, path, headers)))
}

fn status_line(status: u16) -> &'static str {
    match status {
        200 => "200 OK",
        206 => "206 Partial Content",
        304 => "304 Not Modified",
        400 => "400 Bad Request",
        401 => "401 Unauthorized",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        416 => "416 Range Not Satisfiable",
        502 => "502 Bad Gateway",
        _ => "500 Internal Server Error",
    }
}

fn response_bytes(response: &TileResponse) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {}\r\n", status_line(response.status));
    if let Some(ct) = &response.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(etag) = &response.etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    if let Some(cache_control) = &response.cache_control {
        head.push_str(&format!("Cache-Control: {cache_control}\r\n"));
    }
    if let Some(content_range) = &response.content_range {
        head.push_str(&format!("Content-Range: {content_range}\r\n"));
    }
    // A 304 carries no body (RFC 9110 section 15.4.5); every other response's
    // Content-Length is its real body length -- 0 for a refusal, the (possibly partial)
    // byte count for a 200/206.
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    head.push_str("Connection: close\r\n\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(&response.body);
    out
}

fn method_not_allowed_bytes() -> Vec<u8> {
    let body = b"";
    format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", status_line(405), body.len()).into_bytes()
}

async fn handle_connection(stream: TcpStream, config: Arc<TilesConfig>, source: Arc<dyn ObjectSource>, counters: Arc<Counters>, clock: Arc<dyn av_command::clock::Clock>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let Some((method, path, raw_headers)) = read_request_line_and_headers(&mut reader).await? else {
        return Ok(());
    };

    let response_bytes = if method != "GET" {
        method_not_allowed_bytes()
    } else {
        let headers = crate::core::RequestHeaders {
            authorization: raw_headers.authorization.as_deref(),
            range: raw_headers.range.as_deref(),
            if_none_match: raw_headers.if_none_match.as_deref(),
        };
        let response = handle(&config, source.as_ref(), &counters, &path, headers, clock.now_tai_ns());
        response_bytes(&response)
    };

    let mut stream = reader.into_inner();
    stream.write_all(&response_bytes).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Binds `addr` and serves both routes forever, one `tokio::spawn`ed task per connection
/// (mirroring `crates/av-command/src/admin.rs::serve`'s identical shape). `clock` is read
/// fresh for every request (never cached at bind time) -- `av_command::clock::SystemClock`
/// for a real binary, an injected `TestClock` for a test.
pub async fn serve(addr: std::net::SocketAddr, config: Arc<TilesConfig>, source: Arc<dyn ObjectSource>, counters: Arc<Counters>, clock: Arc<dyn av_command::clock::Clock>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, _peer) = listener.accept().await?;
        let config = config.clone();
        let source = source.clone();
        let counters = counters.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, config, source, counters, clock).await {
                eprintln!("av-tiles: connection error: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{Label, TileEntry, TileSetKind, TileSetManifest};
    use av_command::clock::TestClock;
    use av_command::test_support::{valid_claims, TestIssuer};
    use av_label::{ClearanceLadder, GroupClearanceMap};
    use prost::Message as _;
    use std::collections::BTreeMap;
    use tokio::io::AsyncReadExt as _;

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-tiles";
    const NOW_UNIX_S: i64 = 1_760_000_000;
    const TTL_S: i64 = 3_600;

    fn now_tai_ns() -> i64 {
        av_cdm::time::Tai::from_utc_nanos(NOW_UNIX_S * 1_000_000_000).as_nanos()
    }

    fn ladder() -> Arc<ClearanceLadder> {
        Arc::new(ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]))
    }

    fn config(issuer: &TestIssuer, group_clearance: &[(&str, &str)]) -> Arc<TilesConfig> {
        let issuer_config = Arc::new(av_command::oidc::IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap());
        let mut m = BTreeMap::new();
        for (g, marking) in group_clearance {
            m.insert(g.to_string(), marking.to_string());
        }
        Arc::new(TilesConfig::new("imagery/2026", issuer_config, ladder(), Arc::new(GroupClearanceMap::new(m))))
    }

    fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", NOW_UNIX_S, TTL_S);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    fn manifest_and_tile_source(label_marking: &str, key_prefix: &str) -> (Arc<crate::source::InMemoryObjectSource>, String, TileEntry) {
        let tile_bytes = vec![1u8, 2, 3, 4];
        let tile_sha256 = crate::core::hex_sha256(&tile_bytes);
        let tile_key = av_store::object_key(key_prefix, &tile_sha256).unwrap();
        let entry = TileEntry { level: 0, x: 0, y: 0, sha256: tile_sha256.clone(), size_bytes: tile_bytes.len() as u64, uri: String::new(), media_type: "image/png".to_string(), object_key: tile_key.clone() };
        let manifest = TileSetManifest {
            kind: TileSetKind::Imagery as i32,
            scheme: "geographic-plate-carree-2x1".to_string(),
            min_level: 0,
            max_level: 0,
            tile_size: 256,
            bounds: None,
            tiles: vec![entry.clone()],
            source_sha256: vec![],
            parameters: Default::default(),
            root_uri: String::new(),
            job_id: "job-1".to_string(),
            object_key_prefix: key_prefix.to_string(),
            root_object_key: String::new(),
        };
        let manifest_bytes = manifest.encode_to_vec();
        let manifest_sha256 = crate::core::hex_sha256(&manifest_bytes);
        let manifest_key = av_store::object_key(key_prefix, &manifest_sha256).unwrap();

        let source = Arc::new(crate::source::InMemoryObjectSource::new());
        let label = Label { marking: label_marking.to_string(), caveats: vec![] };
        source.insert(manifest_key, manifest_bytes, label.clone());
        source.insert(tile_key, tile_bytes, label);
        (source, manifest_sha256, entry)
    }

    async fn spawn_test_server(config: Arc<TilesConfig>, source: Arc<dyn ObjectSource>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        let counters = Arc::new(Counters::new());
        let clock: Arc<dyn av_command::clock::Clock> = Arc::new(TestClock::new(now_tai_ns()));
        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let _ = handle_connection(stream, config.clone(), source.clone(), counters.clone(), clock.clone()).await;
            }
        });
        addr
    }

    async fn get(addr: std::net::SocketAddr, path: &str, authorization: Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n");
        if let Some(a) = authorization {
            req.push_str(&format!("Authorization: {a}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let split_at = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&buf[..split_at]).to_string();
        let body = buf[split_at + 4..].to_vec();
        let mut lines = head.lines();
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
        let headers = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        (status, headers, body)
    }

    /// P2: like [`get`], but also sends `Range`/`If-None-Match` when given.
    async fn get_with_headers(addr: std::net::SocketAddr, path: &str, authorization: Option<&str>, range: Option<&str>, if_none_match: Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n");
        if let Some(a) = authorization {
            req.push_str(&format!("Authorization: {a}\r\n"));
        }
        if let Some(r) = range {
            req.push_str(&format!("Range: {r}\r\n"));
        }
        if let Some(inm) = if_none_match {
            req.push_str(&format!("If-None-Match: {inm}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let split_at = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&buf[..split_at]).to_string();
        let body = buf[split_at + 4..].to_vec();
        let mut lines = head.lines();
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
        let headers = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        (status, headers, body)
    }

    #[tokio::test]
    async fn a_range_request_returns_206_with_the_exact_byte_slice_of_the_stored_object() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, entry) = manifest_and_tile_source("CUI", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "CUI")]);
        let addr = spawn_test_server(cfg, source).await;
        let token = mint(&issuer, &["operators"]);
        let path = format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", entry.level, entry.x, entry.y);

        // The fixture's tile bytes are [1, 2, 3, 4] -- bytes=1-2 is [2, 3].
        let (status, headers, body) = get_with_headers(addr, &path, Some(&format!("Bearer {token}")), Some("bytes=1-2"), None).await;
        assert_eq!(status, 206);
        assert_eq!(body, vec![2u8, 3]);
        assert!(headers.iter().any(|(k, v)| k == "Content-Range" && v == "bytes 1-2/4"), "{headers:?}");
    }

    #[tokio::test]
    async fn if_none_match_with_the_current_etag_returns_304_with_no_body() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, entry) = manifest_and_tile_source("CUI", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "CUI")]);
        let addr = spawn_test_server(cfg, source).await;
        let token = mint(&issuer, &["operators"]);
        let path = format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", entry.level, entry.x, entry.y);

        let etag = format!("\"{}\"", entry.sha256);
        let (status, _headers, body) = get_with_headers(addr, &path, Some(&format!("Bearer {token}")), None, Some(&etag)).await;
        assert_eq!(status, 304);
        assert!(body.is_empty(), "a 304 must never carry a body");
    }

    #[tokio::test]
    async fn a_real_tile_is_served_byte_identical_to_the_stored_object() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, entry) = manifest_and_tile_source("CUI", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "CUI")]);
        let addr = spawn_test_server(cfg, source).await;
        let token = mint(&issuer, &["operators"]);

        let path = format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", entry.level, entry.x, entry.y);
        let (status, headers, body) = get(addr, &path, Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, 200);
        assert_eq!(body, vec![1u8, 2, 3, 4]);
        assert!(headers.iter().any(|(k, v)| k == "Content-Type" && v == "image/png"), "{headers:?}");
    }

    #[tokio::test]
    async fn a_caller_below_the_tile_sets_clearance_is_refused_403_with_no_bytes() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, entry) = manifest_and_tile_source("SECRET", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let addr = spawn_test_server(cfg, source).await;
        let token = mint(&issuer, &["operators"]);

        let path = format!("/v1/tilesets/{manifest_sha256}/tiles/{}/{}/{}", entry.level, entry.x, entry.y);
        let (status, _headers, body) = get(addr, &path, Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, 403);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn no_authorization_header_is_401() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _entry) = manifest_and_tile_source("UNCLASSIFIED", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let addr = spawn_test_server(cfg, source).await;

        let (status, _headers, _body) = get(addr, &format!("/v1/tilesets/{manifest_sha256}/manifest"), None).await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn a_tile_address_absent_from_the_manifest_is_404() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _entry) = manifest_and_tile_source("UNCLASSIFIED", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let addr = spawn_test_server(cfg, source).await;
        let token = mint(&issuer, &["operators"]);

        let (status, _headers, _body) = get(addr, &format!("/v1/tilesets/{manifest_sha256}/tiles/9/9/9"), Some(&format!("Bearer {token}"))).await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn a_non_get_method_is_405() {
        let issuer = TestIssuer::new();
        let (source, manifest_sha256, _entry) = manifest_and_tile_source("UNCLASSIFIED", "imagery/2026");
        let cfg = config(&issuer, &[("operators", "UNCLASSIFIED")]);
        let addr = spawn_test_server(cfg, source).await;

        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!("POST /v1/tilesets/{manifest_sha256}/manifest HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 405"), "{text}");
    }
}

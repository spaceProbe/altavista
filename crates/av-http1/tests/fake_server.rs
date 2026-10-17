//! The plaintext fake-server suite: a scripted `std::net::TcpListener` on loopback, byte-exact
//! responses, and assertions on the exact request bytes the client wrote.

mod common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use av_http1::{Client, Endpoint, HttpError, Phase, Request, SizeLimits, Timeouts, TlsConfig};
use common::{client_for, client_with, fake, fake_on, ok, pause, w, Step};

fn assert_protocol(result: Result<av_http1::Response, HttpError>, what: &str) {
    match result {
        Err(HttpError::Protocol(_)) => {}
        other => panic!("{what}: expected Protocol, got {other:?}"),
    }
}

fn head_for(addr: std::net::SocketAddr, line: &str, extra: &str) -> Vec<u8> {
    format!("{line} HTTP/1.1\r\nHost: {addr}\r\n{extra}Connection: close\r\n\r\n").into_bytes()
}

// ---------------------------------------------------------------- request bytes

#[test]
fn get_writes_exact_bytes_and_parses_the_response() {
    let srv = fake(vec![vec![w(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nX-Dup: a\r\nx-dup: b\r\n\r\nhello",
    )]]);
    let addr = srv.addr;
    let resp = srv
        .client()
        .request(Request::get("/v1/ping?a=1&b=%20").header("Accept", "text/plain").header("X-Trace", "t-1"))
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.reason, "OK");
    assert_eq!(resp.body, b"hello");
    assert!(resp.is_success());
    assert_eq!(resp.header("content-type"), Some("text/plain"), "case-insensitive lookup");
    assert_eq!(resp.header("CONTENT-LENGTH"), Some("5"));
    assert_eq!(resp.header("absent"), None);
    // Order kept, repeats kept, the server's own capitalisation kept.
    let names: Vec<&str> = resp.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Content-Type", "Content-Length", "X-Dup", "x-dup"]);
    assert_eq!(resp.header_all("X-DUP").collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(resp.header("x-dup"), Some("a"), "header() is the first match");

    let seen = srv.finish();
    assert_eq!(
        seen,
        vec![head_for(addr, "GET /v1/ping?a=1&b=%20", "Accept: text/plain\r\nX-Trace: t-1\r\n")]
    );
}

#[test]
fn post_writes_content_length_and_the_body_verbatim() {
    let srv = fake(vec![ok(b"{}")]);
    let addr = srv.addr;
    let body = b"line one\r\nline two\r\n\r\nGET /smuggled HTTP/1.1\r\n\r\n\x00\xff".to_vec();
    let resp = srv
        .client()
        .request(Request::post("/insert").header("Content-Type", "application/octet-stream").body(body.clone()))
        .unwrap();
    assert_eq!(resp.status, 200);
    let seen = srv.finish();
    let mut expected = head_for(
        addr,
        "POST /insert",
        &format!("Content-Type: application/octet-stream\r\nContent-Length: {}\r\n", body.len()),
    );
    expected.extend_from_slice(&body);
    assert_eq!(seen, vec![expected]);
}

#[test]
fn content_length_rules_for_empty_bodies() {
    let srv = fake(vec![ok(b""), ok(b""), ok(b"")]);
    let addr = srv.addr;
    let c = srv.client();
    c.request(Request::get("/a")).unwrap();
    c.request(Request::put("/b")).unwrap();
    c.request(Request::delete("/c")).unwrap();
    let seen = srv.finish();
    assert_eq!(seen[0], head_for(addr, "GET /a", ""), "no Content-Length on a bodiless GET");
    assert_eq!(seen[1], head_for(addr, "PUT /b", "Content-Length: 0\r\n"), "PUT always says its length");
    assert_eq!(seen[2], head_for(addr, "DELETE /c", ""));
}

#[test]
fn large_request_and_response_bodies_roundtrip() {
    let big_req = vec![0xa5u8; 2 * 1024 * 1024];
    let big_resp: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let srv = fake(vec![ok(&big_resp)]);
    let resp = srv.client().request(Request::post("/up").body(big_req.clone())).unwrap();
    assert_eq!(resp.body, big_resp);
    let seen = srv.finish();
    assert!(seen[0].ends_with(&big_req));
}

#[test]
fn base_path_is_joined_in_front_of_the_request_path() {
    let srv = fake(vec![ok(b""), ok(b""), ok(b"")]);
    let addr = srv.addr;
    let with_base = client_for(&format!("http://{addr}/api/v1/"));
    with_base.request(Request::get("/jobs?x=1&y=2")).unwrap();
    with_base.request(Request::get("/")).unwrap();
    let root = client_for(&format!("http://{addr}/"));
    root.request(Request::get("/jobs")).unwrap();
    let seen = srv.finish();
    assert_eq!(seen[0], head_for(addr, "GET /api/v1/jobs?x=1&y=2", ""));
    assert_eq!(seen[1], head_for(addr, "GET /api/v1/", ""));
    assert_eq!(seen[2], head_for(addr, "GET /jobs", ""));
}

#[test]
fn an_ipv6_loopback_endpoint_brackets_the_host_header() {
    let Ok(srv) = fake_on("[::1]:0", vec![ok(b"v6")]) else {
        eprintln!("skipped: no IPv6 loopback on this host");
        return;
    };
    let resp = client_for(&srv.url()).request(Request::get("/")).unwrap();
    assert_eq!(resp.body, b"v6");
    let seen = srv.finish();
    let text = String::from_utf8(seen[0].clone()).unwrap();
    assert!(text.contains("\r\nHost: [::1]:"), "{text}");
}

// ---------------------------------------------------------------- response framing

#[test]
fn chunked_with_extensions_uppercase_hex_and_trailers() {
    let srv = fake(vec![vec![w(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
          5;name=value;flag\r\nhello\r\nA\r\n0123456789\r\n0;last-chunk\r\nTrailer-One: 1\r\nTrailer-Two: 2\r\n\r\n",
    )]]);
    let resp = srv.client().request(Request::get("/")).unwrap();
    assert_eq!(resp.body, b"hello0123456789");
    assert!(resp.header("trailer-one").is_none(), "trailers are discarded, not merged");
    srv.finish();
}

#[test]
fn chunked_split_across_many_small_writes() {
    let wire = b"4\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\nX-T: 1\r\n\r\n".to_vec();
    let mut steps = vec![w(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n")];
    for byte in &wire {
        steps.push(Step::Write(vec![*byte]));
        steps.push(pause(1));
    }
    let srv = fake(vec![steps]);
    let resp = srv.client().request(Request::get("/")).unwrap();
    assert_eq!(resp.body, b"Wikipedia in\r\n\r\nchunks.");
    srv.finish();
}

#[test]
fn content_length_split_across_many_small_writes_and_a_split_head() {
    let mut steps = Vec::new();
    for piece in [&b"HTT"[..], b"P/1.1 2", b"00 OK\r", b"\nContent-Le", b"ngth: 6\r\n\r", b"\nabc", b"def"] {
        steps.push(w(piece));
        steps.push(pause(3));
    }
    let srv = fake(vec![steps]);
    let resp = srv.client().request(Request::get("/")).unwrap();
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"abcdef"[..]));
    srv.finish();
}

#[test]
fn a_large_chunked_body_decodes() {
    let payload: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
    let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for chunk in payload.chunks(65536) {
        wire.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        wire.extend_from_slice(chunk);
        wire.extend_from_slice(b"\r\n");
    }
    wire.extend_from_slice(b"0\r\n\r\n");
    let srv = fake(vec![vec![Step::Write(wire)]]);
    assert_eq!(srv.client().request(Request::get("/")).unwrap().body, payload);
    srv.finish();
}

#[test]
fn no_framing_reads_to_close() {
    let srv = fake(vec![vec![w(b"HTTP/1.0 200 OK\r\nServer: x\r\n\r\npart one, "), pause(20), w(b"part two")]]);
    let resp = srv.client().request(Request::get("/")).unwrap();
    assert_eq!(resp.body, b"part one, part two");
    srv.finish();
}

#[test]
fn both_framings_is_a_protocol_error() {
    let srv = fake(vec![vec![w(
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
    )]]);
    assert_protocol(srv.client().request(Request::get("/")), "CL + TE");
    srv.finish();
}

#[test]
fn one_hundred_continue_then_final_response_is_skipped() {
    let srv = fake(vec![
        vec![w(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")],
        vec![
            w(b"HTTP/1.1 100 Continue\r\n\r\n"),
            pause(30),
            w(b"HTTP/1.1 103 Early Hints\r\nLink: </s.css>\r\n\r\n"),
            pause(30),
            w(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok"),
        ],
    ]);
    let c = srv.client();
    let r = c.request(Request::post("/a").body(b"x".to_vec())).unwrap();
    assert_eq!((r.status, r.body.as_slice()), (200, &b"hi"[..]));
    assert!(r.header("link").is_none(), "interim headers are not merged into the final response");
    let r = c.request(Request::post("/b").body(b"x".to_vec())).unwrap();
    assert_eq!((r.status, r.reason.as_str(), r.body.as_slice()), (201, "Created", &b"ok"[..]));
    srv.finish();
}

#[test]
fn switching_protocols_is_a_protocol_error() {
    let srv = fake(vec![vec![w(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: x\r\n\r\n")]]);
    assert_protocol(srv.client().request(Request::get("/")), "101");
    srv.finish();
}

#[test]
fn head_204_and_304_have_no_body_whatever_the_headers_say() {
    let srv = fake(vec![
        vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 1234\r\n\r\n")],
        vec![w(b"HTTP/1.1 204 No Content\r\n\r\n")],
        vec![w(b"HTTP/1.1 304 Not Modified\r\nETag: \"x\"\r\n\r\n")],
    ]);
    let c = srv.client();
    let r = c.request(Request::new("HEAD", "/h")).unwrap();
    assert_eq!((r.status, r.body.len()), (200, 0));
    assert_eq!(r.header("content-length"), Some("1234"));
    assert_eq!(c.request(Request::get("/n")).unwrap().body.len(), 0);
    let r = c.request(Request::get("/m")).unwrap();
    assert_eq!((r.status, r.body.len()), (304, 0));
    srv.finish();
}

#[test]
fn non_2xx_statuses_are_responses_not_errors() {
    let srv = fake(vec![
        vec![w(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found")],
        vec![w(b"HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nbad\r\n0\r\n\r\n")],
        vec![w(b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere/\r\nContent-Length: 0\r\n\r\n")],
        vec![w(b"HTTP/1.1 503\r\nContent-Length: 0\r\n\r\n")],
    ]);
    let c = srv.client();
    let r = c.request(Request::get("/a")).unwrap();
    assert_eq!((r.status, r.body.as_slice(), r.is_success()), (404, &b"not found"[..], false));
    let r = c.request(Request::get("/b")).unwrap();
    assert_eq!((r.status, r.reason.as_str(), r.body.as_slice()), (500, "Internal Server Error", &b"bad"[..]));
    let r = c.request(Request::get("/c")).unwrap();
    assert_eq!(r.status, 302, "redirects are returned, never followed");
    assert_eq!(r.header("location"), Some("http://elsewhere/"));
    let r = c.request(Request::get("/d")).unwrap();
    assert_eq!((r.status, r.reason.as_str()), (503, ""), "a status line without a reason phrase");
    assert_eq!(srv.finish().len(), 4, "one connection per request");
}

#[test]
fn header_values_are_trimmed_and_may_be_empty() {
    let srv = fake(vec![vec![w(b"HTTP/1.1 200 OK\r\nX-Pad:   spaced \t \r\nX-Empty:\r\nContent-Length: 0\r\n\r\n")]]);
    let r = srv.client().request(Request::get("/")).unwrap();
    assert_eq!(r.header("x-pad"), Some("spaced"));
    assert_eq!(r.header("x-empty"), Some(""));
    srv.finish();
}

#[test]
fn identical_repeated_content_lengths_are_accepted() {
    let srv = fake(vec![
        vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 3\r\n\r\nabc")],
        vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 3, 3\r\n\r\nabc")],
    ]);
    let c = srv.client();
    assert_eq!(c.request(Request::get("/")).unwrap().body, b"abc");
    assert_eq!(c.request(Request::get("/")).unwrap().body, b"abc");
    srv.finish();
}

// ---------------------------------------------------------------- protocol errors

#[test]
fn malformed_responses_are_protocol_errors() {
    let cases: &[(&str, &[u8])] = &[
        ("HTTP/2 status line", b"HTTP/2 200 OK\r\n\r\n"),
        ("two-digit status", b"HTTP/1.1 20 OK\r\n\r\n"),
        ("four-digit status", b"HTTP/1.1 2000 OK\r\n\r\n"),
        ("alphabetic status", b"HTTP/1.1 abc OK\r\n\r\n"),
        ("double space", b"HTTP/1.1  200 OK\r\n\r\n"),
        ("no space after code", b"HTTP/1.1 200OK\r\n\r\n"),
        ("status below 100", b"HTTP/1.1 099 Low\r\n\r\n"),
        ("empty status line", b"\r\n\r\n"),
        ("garbage", b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03\r\n\r\n"),
        ("space in header name", b"HTTP/1.1 200 OK\r\nBad Header: x\r\n\r\n"),
        ("space before colon", b"HTTP/1.1 200 OK\r\nX-A : x\r\n\r\n"),
        ("header without colon", b"HTTP/1.1 200 OK\r\nNoColon\r\n\r\n"),
        ("empty header name", b"HTTP/1.1 200 OK\r\n: x\r\n\r\n"),
        ("obsolete folding", b"HTTP/1.1 200 OK\r\nX: a\r\n b\r\n\r\n"),
        ("bare LF in a value", b"HTTP/1.1 200 OK\r\nX: a\nb\r\n\r\n"),
        ("bare CR in a value", b"HTTP/1.1 200 OK\r\nX: a\rb\r\n\r\n"),
        ("NUL in a value", b"HTTP/1.1 200 OK\r\nX: a\0b\r\n\r\n"),
        ("LF-only line endings", b"HTTP/1.1 200 OK\nContent-Length: 0\n\n"),
        ("Content-Length plus-sign", b"HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\nhello"),
        ("Content-Length negative", b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n"),
        ("Content-Length not a number", b"HTTP/1.1 200 OK\r\nContent-Length: abc\r\n\r\n"),
        ("Content-Length empty", b"HTTP/1.1 200 OK\r\nContent-Length:\r\n\r\n"),
        ("Content-Length overflow", b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n"),
        ("conflicting Content-Length", b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!"),
        ("conflicting Content-Length list", b"HTTP/1.1 200 OK\r\nContent-Length: 5, 6\r\n\r\nhello!"),
        ("unsupported transfer coding", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nxx"),
        ("coding before chunked", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n0\r\n\r\n"),
        ("chunked then another coding", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n0\r\n\r\n"),
        ("non-hex chunk size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhi\r\n0\r\n\r\n"),
        ("empty chunk size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\r\nhi\r\n0\r\n\r\n"),
        ("signed chunk size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+5\r\nhello\r\n0\r\n\r\n"),
        ("0x chunk size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0x5\r\nhello\r\n0\r\n\r\n"),
        ("space-led chunk size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n 5\r\nhello\r\n0\r\n\r\n"),
        (
            "18-digit chunk size",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n000000000000000005\r\nhello\r\n0\r\n\r\n",
        ),
        ("chunk not followed by CRLF", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX0\r\n\r\n"),
        ("chunk data longer than its size", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhello\r\n0\r\n\r\n"),
        ("truncated Content-Length body", b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhello"),
        ("Content-Length body, nothing sent", b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n"),
        ("truncated inside a chunk", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\na\r\n12345"),
        ("truncated after a chunk", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n"),
        ("truncated before the last-chunk CRLF", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n"),
        ("truncated inside a chunk-size line", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n1"),
        ("closed inside the header", b"HTTP/1.1 200 OK\r\nContent-Le"),
        ("closed after the status line", b"HTTP/1.1 200 OK\r\n"),
        ("closed without a byte", b""),
    ];
    for (what, bytes) in cases {
        let srv = fake(vec![vec![Step::Write(bytes.to_vec())]]);
        assert_protocol(srv.client().request(Request::get("/")), what);
        srv.finish();
    }
}

// ---------------------------------------------------------------- limits

fn limited(srv: &common::Fake, header: usize, body: usize) -> Client {
    srv.client().with_limits(SizeLimits {
        max_header_bytes: header,
        max_body_bytes: body,
    })
}

/// A response head of exactly `total` bytes including the blank line.
fn head_of_size(total: usize) -> Vec<u8> {
    let fixed = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-P: ".len() + "\r\n\r\n".len();
    let mut v = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-P: ".to_vec();
    v.extend(std::iter::repeat_n(b'a', total - fixed));
    v.extend_from_slice(b"\r\n\r\n");
    assert_eq!(v.len(), total);
    v
}

#[test]
fn header_limit_is_exact() {
    let srv = fake(vec![
        vec![Step::Write(head_of_size(1024))],
        vec![Step::Write(head_of_size(1025))],
        vec![Step::Write(head_of_size(60_000))],
    ]);
    let c = limited(&srv, 1024, 1024);
    assert_eq!(c.request(Request::get("/")).unwrap().status, 200, "exactly at the limit passes");
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::HeaderTooLarge { limit: 1024 })));
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::HeaderTooLarge { limit: 1024 })));
    srv.finish();
}

#[test]
fn an_unterminated_endless_header_is_cut_off() {
    let mut steps = vec![w(b"HTTP/1.1 200 OK\r\n")];
    for _ in 0..40 {
        steps.push(Step::Write(vec![b'a'; 1000]));
    }
    let srv = fake(vec![steps]);
    let started = Instant::now();
    assert!(matches!(
        limited(&srv, 4096, 1024).request(Request::get("/")),
        Err(HttpError::HeaderTooLarge { limit: 4096 })
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    srv.finish();
}

#[test]
fn skipped_interim_heads_count_against_the_header_limit() {
    let mut wire = Vec::new();
    for _ in 0..50 {
        wire.extend_from_slice(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    wire.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let srv = fake(vec![vec![Step::Write(wire)]]);
    assert!(matches!(
        limited(&srv, 512, 1024).request(Request::get("/")),
        Err(HttpError::HeaderTooLarge { limit: 512 })
    ));
    srv.finish();
}

#[test]
fn oversized_trailers_are_header_too_large() {
    let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n".to_vec();
    for _ in 0..100 {
        wire.extend_from_slice(b"Trailer: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    wire.extend_from_slice(b"\r\n");
    let srv = fake(vec![vec![Step::Write(wire)]]);
    assert!(matches!(
        limited(&srv, 1024, 1024).request(Request::get("/")),
        Err(HttpError::HeaderTooLarge { limit: 1024 })
    ));
    srv.finish();
}

#[test]
fn body_limit_applies_to_every_framing() {
    let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    chunked.extend_from_slice(b"3c\r\n");
    chunked.extend_from_slice(&[b'x'; 60]);
    chunked.extend_from_slice(b"\r\n3c\r\n");
    chunked.extend_from_slice(&[b'x'; 60]);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    let mut to_close = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    to_close.extend_from_slice(&[b'x'; 101]);
    let mut exact = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec();
    exact.extend_from_slice(&[b'x'; 100]);
    let srv = fake(vec![
        vec![Step::Write(exact)],
        // The length alone is enough: refused before any body byte is read.
        vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 101\r\n\r\n"), Step::Hold],
        vec![Step::Write(chunked)],
        vec![Step::Write(to_close)],
        vec![w(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n")],
        vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n")],
    ]);
    let c = limited(&srv, 4096, 100);
    assert_eq!(c.request(Request::get("/")).unwrap().body.len(), 100, "exactly at the limit passes");
    let started = Instant::now();
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::BodyTooLarge { limit: 100 })));
    assert!(started.elapsed() < Duration::from_secs(2), "refused without waiting for the body");
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::BodyTooLarge { limit: 100 })));
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::BodyTooLarge { limit: 100 })));
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::BodyTooLarge { limit: 100 })));
    assert!(matches!(c.request(Request::get("/")), Err(HttpError::BodyTooLarge { limit: 100 })));
    srv.finish();
}

// ---------------------------------------------------------------- timeouts and reachability

#[test]
fn a_silent_server_is_a_read_timeout() {
    let srv = fake(vec![vec![Step::Hold]]);
    let c = client_with(
        &srv.url(),
        Timeouts {
            connect: Duration::from_secs(2),
            read: Duration::from_millis(300),
            write: Duration::from_secs(2),
        },
    );
    let started = Instant::now();
    match c.request(Request::get("/")) {
        Err(HttpError::Timeout { phase: Phase::Read, addr }) => assert!(addr.starts_with("127.0.0.1:"), "{addr}"),
        other => panic!("expected Timeout(Read), got {other:?}"),
    }
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(2), "{took:?}");
    srv.finish();
}

#[test]
fn a_stalled_body_is_a_read_timeout_too() {
    let srv = fake(vec![vec![w(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\npartial"), Step::Hold]]);
    let c = client_with(&srv.url(), Timeouts::uniform(Duration::from_millis(300)));
    assert!(matches!(
        c.request(Request::get("/")),
        Err(HttpError::Timeout { phase: Phase::Read, .. })
    ));
    srv.finish();
}

#[test]
fn connection_refused_is_unreachable_and_names_the_address() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let c = client_for(&format!("http://127.0.0.1:{port}"));
    match c.request(Request::get("/")) {
        Err(HttpError::Unreachable { addr, source }) => {
            assert_eq!(addr, format!("127.0.0.1:{port}"));
            assert_eq!(source.kind(), std::io::ErrorKind::ConnectionRefused);
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

#[test]
fn an_unresolvable_name_is_unreachable() {
    // The .invalid TLD never resolves (RFC 6761). TLS, because plaintext is loopback-only.
    let c = Client::new(
        Endpoint::parse("https://av-http1-test.invalid:443").unwrap(),
        None,
        Timeouts::uniform(Duration::from_secs(3)),
    )
    .unwrap();
    match c.request(Request::get("/")) {
        Err(HttpError::Unreachable { addr, .. }) => assert!(addr.contains("av-http1-test.invalid"), "{addr}"),
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

// ---------------------------------------------------------------- refusals before any byte is sent

#[test]
fn injection_attempts_are_refused_before_connecting() {
    let srv = fake(vec![]);
    let c = srv.client();
    let long_value = "x".repeat(10);
    let bad: Vec<(&str, Request)> = vec![
        ("CR in header value", Request::get("/").header("X-A", "a\rb")),
        ("LF in header value", Request::get("/").header("X-A", "a\nb")),
        ("CRLF header injection", Request::get("/").header("X-A", "ok\r\nX-Injected: 1")),
        ("NUL in header value", Request::get("/").header("X-A", "a\0b")),
        ("DEL in header value", Request::get("/").header("X-A", "a\x7fb")),
        ("CR in header name", Request::get("/").header("X-A\r", &long_value)),
        ("LF in header name", Request::get("/").header("X-A\nX-B", &long_value)),
        ("NUL in header name", Request::get("/").header("X-\0A", &long_value)),
        ("space in header name", Request::get("/").header("X A", &long_value)),
        ("colon in header name", Request::get("/").header("X:A", &long_value)),
        ("empty header name", Request::get("/").header("", &long_value)),
        ("non-ASCII header name", Request::get("/").header("X-\u{e9}", &long_value)),
        ("CRLF in method", Request::new("GET\r\nX-A: 1", "/")),
        ("space in method", Request::new("GE T", "/")),
        ("empty method", Request::new("", "/")),
        ("request line in method", Request::new("GET /smuggled HTTP/1.1\r\n\r\nGET", "/")),
        ("CRLF in path", Request::get("/x\r\nX-A: 1")),
        ("request smuggling via path", Request::get("/x HTTP/1.1\r\nHost: evil\r\n\r\nGET /y")),
        ("space in path", Request::get("/x y")),
        ("LF in path", Request::get("/x\ny")),
        ("NUL in path", Request::get("/x\0")),
        ("DEL in path", Request::get("/x\x7f")),
        ("fragment in path", Request::get("/x#frag")),
        ("non-ASCII in path", Request::get("/caf\u{e9}")),
        ("path without a slash", Request::get("x")),
        ("empty path", Request::get("")),
        ("query-only path", Request::get("?a=1")),
        ("caller-supplied Host", Request::get("/").header("Host", "evil.example")),
        ("caller-supplied host (lower case)", Request::get("/").header("host", "evil.example")),
        ("caller-supplied Content-Length", Request::post("/").header("Content-Length", "0")),
        ("caller-supplied content-length", Request::post("/").header("content-length", "999").body(b"x".to_vec())),
        ("caller-supplied Transfer-Encoding", Request::post("/").header("Transfer-Encoding", "chunked")),
        ("caller-supplied Connection", Request::get("/").header("Connection", "keep-alive")),
    ];
    for (what, req) in bad {
        match c.request(req) {
            Err(HttpError::InvalidRequest(_)) => {}
            other => panic!("{what}: expected InvalidRequest, got {other:?}"),
        }
    }
    // Not one connection was made: the fake server saw nothing.
    assert!(srv.finish().is_empty(), "a refused request must not touch the network");
}

#[test]
fn legitimate_unusual_requests_are_not_refused() {
    let srv = fake(vec![ok(b"")]);
    let addr = srv.addr;
    srv.client()
        .request(
            Request::new("PATCH", "/a%20b/~c;d=e,f?q=%C3%A9&x=\"y\"")
                .header("X-Custom_1", "tab\tinside")
                .header("Authorization", "Bearer abc.def-ghi_jkl==")
                .body(b"{}".to_vec()),
        )
        .unwrap();
    let seen = srv.finish();
    let expected = head_for(
        addr,
        "PATCH /a%20b/~c;d=e,f?q=%C3%A9&x=\"y\"",
        "X-Custom_1: tab\tinside\r\nAuthorization: Bearer abc.def-ghi_jkl==\r\nContent-Length: 2\r\n",
    );
    let mut expected = expected;
    expected.extend_from_slice(b"{}");
    assert_eq!(seen, vec![expected]);
}

#[test]
fn a_tls_config_for_a_plaintext_endpoint_and_zero_timeouts_are_refused_at_construction() {
    let ep = Endpoint::parse("http://127.0.0.1:1").unwrap();
    assert!(matches!(
        Client::new(ep.clone(), Some(TlsConfig::new()), Timeouts::default()),
        Err(HttpError::InvalidTlsConfig(_))
    ));
    assert!(matches!(
        Client::new(ep, None, Timeouts::uniform(Duration::ZERO)),
        Err(HttpError::InvalidRequest(_))
    ));
}

#[test]
fn the_client_is_send_sync_and_clone() {
    fn assert_traits<T: Send + Sync + Clone>() {}
    assert_traits::<Client>();
    assert_traits::<Endpoint>();
    assert_traits::<TlsConfig>();
}

#[test]
fn a_client_is_reusable_and_usable_from_several_threads() {
    let scripts: Vec<Vec<Step>> = (0..8).map(|_| ok(b"x")).collect();
    let srv = fake(scripts);
    let c = std::sync::Arc::new(srv.client());
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let c = c.clone();
            std::thread::spawn(move || {
                for _ in 0..2 {
                    assert_eq!(c.request(Request::get("/")).unwrap().body, b"x");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(srv.finish().len(), 8);
}

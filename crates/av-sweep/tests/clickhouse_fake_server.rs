//! The ClickHouse study store against a **fake** HTTP server (question 218 asks for both a real
//! server and a fake one): no Docker, loopback only. The fake lets a test make the "server" do what
//! a healthy one never does on demand -- echo the password back in an exception, report an access
//! denial, truncate a reply, or report a column of the wrong type -- and lets it record every
//! request so the tests can assert what the *client* put on the wire: the credentials as headers,
//! the identifiers as bound parameters, no settings, and the study's header inserted last.
//!
//! The real-server counterparts of these tests are in `tests/clickhouse_store.rs`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use av_cdm::pb;
use av_sweep::clickhouse::schema::{self, TABLES};
use av_sweep::clickhouse::{content_sha256, ChClient, ClickHouseConfig, ClickHouseStudyStore, Encoder};
use av_sweep::store::{StoreError, StudyStore, WriteRefusal};

// ---------------------------------------------------------------------------------------------
// The fake server.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Recorded {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The SQL this request carries: the percent-decoded `query` parameter (inserts) or the body.
    fn sql(&self) -> String {
        if let Some(q) = self.target.split('?').nth(1).and_then(|qs| qs.split('&').find_map(|kv| kv.strip_prefix("query="))) {
            return percent_decode(q);
        }
        self.body_text()
    }

    fn param(&self, name: &str) -> Option<String> {
        let key = format!("param_{name}=");
        self.target.split('?').nth(1)?.split('&').find_map(|kv| kv.strip_prefix(key.as_str())).map(percent_decode)
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

type Handler = Arc<dyn Fn(&Recorded) -> Vec<u8> + Send + Sync>;

struct FakeCh {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<Recorded>>>,
    handle: Option<JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut got = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    };
    let head = String::from_utf8_lossy(&got[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let headers: Vec<(String, String)> = lines.filter(|l| !l.is_empty()).filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_string(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    while got.len() < head_end + len {
        match stream.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    }
    Some(Recorded { method, target, headers, body: got[head_end..].to_vec() })
}

fn fake_ch(handler: impl Fn(&Recorded) -> Vec<u8> + Send + Sync + 'static) -> FakeCh {
    let handler: Handler = Arc::new(handler);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (stop2, log2) = (stop.clone(), log.clone());
    let handle = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(false).ok();
                    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    if let Some(req) = read_request(&mut s) {
                        let reply = handler(&req);
                        log2.lock().unwrap().push(req);
                        let _ = s.write_all(&reply);
                        let _ = s.flush();
                    }
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
    });
    FakeCh { addr, stop, log, handle: Some(handle) }
}

impl FakeCh {
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn config(&self) -> ClickHouseConfig {
        let mut c = ClickHouseConfig::new(self.url(), "studies", "avsweep");
        c.timeouts = av_http1::Timeouts::uniform(Duration::from_secs(3));
        c
    }

    fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for FakeCh {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn http(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
    for (n, v) in headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn ok(body: &[u8]) -> Vec<u8> {
    http(200, &[], body)
}

fn exception(status: u16, code: i64, name: &str, message: &str) -> Vec<u8> {
    let body = format!("Code: {code}. DB::Exception: {message}. ({name}) (version 26.8.29.10 (official build))\n");
    http(status, &[("X-ClickHouse-Exception-Code", &code.to_string())], body.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// Reply builders (RowBinary, written with the crate's own public encoder).
// ---------------------------------------------------------------------------------------------

type Triple = (String, String, String);

fn expected_columns() -> Vec<Triple> {
    TABLES.iter().flat_map(|t| t.columns.iter().map(move |c| (t.name.to_string(), c.name.to_string(), c.ty.to_string()))).collect()
}

fn columns_body(triples: &[Triple]) -> Vec<u8> {
    let mut e = Encoder::new();
    let mut position = 0u64;
    let mut current = "";
    for (t, n, ty) in triples {
        if t != current {
            current = t;
            position = 0;
        }
        position += 1;
        e.string(t);
        e.string(n);
        e.string(ty);
        e.u64(position);
    }
    e.finish()
}

fn version_body(versions: &[u32]) -> Vec<u8> {
    let mut e = Encoder::new();
    for v in versions {
        e.u32(*v);
    }
    e.finish()
}

fn header_body(write_id: &str, sha: &str, samples: u64, aggregates: u64) -> Vec<u8> {
    let mut e = Encoder::new();
    e.string(write_id);
    e.string("sweep-hash");
    e.string("drm-hash");
    e.u64(samples);
    e.u64(aggregates);
    e.string(sha);
    e.bool(false); // has_provenance
    e.i32(0);
    for _ in 0..5 {
        e.string("");
    }
    e.i64(0);
    e.string("");
    e.finish()
}

/// A handler for a healthy server that holds no studies yet: columns and version match, header
/// lookups are empty, every insert and `CREATE` succeeds.
fn healthy(req: &Recorded) -> Vec<u8> {
    let sql = req.sql();
    if sql.contains("system.columns") {
        ok(&columns_body(&expected_columns()))
    } else if sql.contains("FROM `av_sweep_schema_version`") {
        ok(&version_body(&[schema::SCHEMA_VERSION]))
    } else {
        ok(b"")
    }
}

fn one_sample_study(sweep_id: &str) -> pb::SweepResults {
    pb::SweepResults {
        sweep_id: sweep_id.to_string(),
        sweep_hash: "sweep-hash".to_string(),
        drm_hash: "drm-hash".to_string(),
        samples: vec![pb::SweepSample {
            point_index: 1,
            draw_index: 2,
            axis_values: [("a.b".to_string(), 1.5)].into_iter().collect(),
            run_id: "r".to_string(),
            scores: [("m".to_string(), pb::ScoreResult { name: "m".to_string(), value: 2.5, unit: 0, passed: None })].into_iter().collect(),
            seeds: [("s".to_string(), 7)].into_iter().collect(),
            ..Default::default()
        }],
        aggregates: vec![pb::ScoreAggregate { name: "m".to_string(), point_index: 1, draws: 1, mean: 2.5, std_dev: 0.0, min: 2.5, max: 2.5, pass_fraction: None }],
        provenance: Some(pb::Provenance { tool: "t".to_string(), attributes: [("k".to_string(), "v".to_string())].into_iter().collect(), ..Default::default() }),
    }
}

// ---------------------------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------------------------

/// The password travels as `X-ClickHouse-Key`, never in the path; a server that echoes it back in
/// an exception message does not get it into a `StoreError` (Display or Debug); `Debug` of the
/// config and the store never shows it.
#[test]
fn the_password_is_a_header_and_never_appears_in_errors_or_debug_output() {
    const VAR: &str = "AV_SWEEP_FAKE_SERVER_TEST_PASSWORD";
    const SECRET: &str = "s3cr3t-pa55w0rd-do-not-leak";
    std::env::set_var(VAR, SECRET);
    let fake = fake_ch(|_| exception(403, 516, "AUTHENTICATION_FAILED", &format!("avsweep: Authentication failed: password is incorrect ({SECRET}) or there is no user with such name")));
    let config = fake.config().with_password_env(VAR);

    let err = ClickHouseStudyStore::connect(&config).unwrap_err();
    std::env::remove_var(VAR);
    let shown = format!("{err} || {err:?}");
    assert!(!shown.contains(SECRET), "the password leaked into the error: {shown}");
    match &err {
        StoreError::Server { exception, .. } => {
            assert_eq!(exception.code, Some(516));
            assert_eq!(exception.name.as_deref(), Some("AUTHENTICATION_FAILED"));
            assert!(exception.message.contains("<redacted>"), "{exception:?}");
        }
        other => panic!("expected a typed server exception, got {other:?}"),
    }

    let requests = fake.requests();
    assert!(!requests.is_empty());
    for r in &requests {
        assert!(!r.target.contains(SECRET), "password in the request target: {}", r.target);
        assert!(!r.body_text().contains(SECRET), "password in the request body");
        assert_eq!(r.header("X-ClickHouse-Key"), Some(SECRET), "the password goes in X-ClickHouse-Key");
        assert_eq!(r.header("X-ClickHouse-User"), Some("avsweep"));
        assert_eq!(r.header("X-ClickHouse-Database"), Some("studies"));
        assert!(r.header("Authorization").is_none(), "no basic-auth URL or header form is used");
    }

    std::env::set_var(VAR, SECRET);
    let client = ChClient::connect(&config).unwrap();
    std::env::remove_var(VAR);
    let debug = format!("{config:?} {client:?}");
    assert!(!debug.contains(SECRET), "{debug}");
    assert!(debug.contains(VAR), "the variable NAME may be shown: {debug}");
}

/// Exception parsing: the numeric code comes from `X-ClickHouse-Exception-Code` when present and
/// from `Code: N.` in the body otherwise; the symbolic name from the body; a reply that is not a
/// ClickHouse exception at all carries no code.
#[test]
fn server_exceptions_are_parsed_into_a_typed_error_carrying_the_code() {
    type Case<'a> = (Vec<u8>, Option<i64>, Option<&'a str>, u16);
    let cases: Vec<Case<'_>> = vec![
        (exception(404, 60, "UNKNOWN_TABLE", "Table studies.nope does not exist"), Some(60), Some("UNKNOWN_TABLE"), 404),
        (http(500, &[], b"Code: 81. DB::Exception: Database nope does not exist. (UNKNOWN_DATABASE) (version 26.8.29.10)\n"), Some(81), Some("UNKNOWN_DATABASE"), 500),
        (http(401, &[], b"Code: 194. DB::Exception: default: Authentication failed: password is incorrect. (REQUIRED_PASSWORD) (version 26.8.29.10)\n"), Some(194), Some("REQUIRED_PASSWORD"), 401),
        (http(502, &[], b"<html>Bad Gateway</html>"), None, None, 502),
        (http(500, &[("X-ClickHouse-Exception-Code", "164")], b"Code: 164. DB::Exception: x: Cannot execute query in readonly mode. (READONLY) (version 1)"), Some(164), Some("READONLY"), 500),
    ];
    for (reply, code, name, status) in cases {
        let fake = fake_ch(move |_| reply.clone());
        let client = ChClient::connect(&fake.config()).unwrap();
        let err = client.command("SELECT 1", &[], "testing").unwrap_err();
        match err {
            StoreError::Server { operation, exception, endpoint } => {
                assert_eq!(operation, "testing");
                assert_eq!(endpoint, fake.url());
                assert_eq!(exception.code, code, "{exception:?}");
                assert_eq!(exception.name.as_deref(), name, "{exception:?}");
                assert_eq!(exception.http_status, status);
            }
            other => panic!("expected StoreError::Server, got {other:?}"),
        }
    }
}

#[test]
fn a_closed_port_is_a_typed_unreachable_naming_the_endpoint() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut config = ClickHouseConfig::new(format!("http://127.0.0.1:{port}"), "studies", "avsweep");
    config.timeouts = av_http1::Timeouts::uniform(Duration::from_secs(2));
    let err = ClickHouseStudyStore::connect(&config).unwrap_err();
    assert!(matches!(&err, StoreError::Unreachable { endpoint, .. } if *endpoint == config.url), "{err:?}");
    assert!(err.to_string().contains(&config.url), "{err}");

    let store = ClickHouseStudyStore::connect_unverified(&config).unwrap();
    assert!(matches!(store.get_study("x"), Err(StoreError::Unreachable { .. })));
    let mut store = store;
    assert!(matches!(store.put_study(&one_sample_study("x")), Err(StoreError::Unreachable { .. })));
    assert!(matches!(store.list_studies(), Err(StoreError::Unreachable { .. })));
}

/// Question 155 through the store: plaintext to a non-loopback host is refused before any
/// connection (so nothing needs to be listening), loopback plaintext is allowed, and user
/// information in the URL is refused without echoing it.
#[test]
fn plaintext_off_loopback_and_urls_with_userinfo_are_refused_before_connecting() {
    for url in ["http://192.0.2.1:8123", "http://clickhouse.example.com:8123", "http://10.1.2.3"] {
        let err = ClickHouseStudyStore::connect_unverified(&ClickHouseConfig::new(url, "d", "u")).unwrap_err();
        match err {
            StoreError::InvalidEndpoint { detail } => assert!(detail.contains("question 155"), "{url}: {detail}"),
            other => panic!("{url}: expected InvalidEndpoint, got {other:?}"),
        }
    }
    for url in ["http://127.0.0.1:8123", "http://localhost:8123", "http://[::1]:8123", "https://clickhouse.example.com:8443"] {
        ClickHouseStudyStore::connect_unverified(&ClickHouseConfig::new(url, "d", "u")).unwrap_or_else(|e| panic!("{url} must be accepted: {e}"));
    }
    let err = ClickHouseStudyStore::connect_unverified(&ClickHouseConfig::new("https://admin:hunter2@ch.example:8443", "d", "u")).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(matches!(err, StoreError::InvalidEndpoint { .. }), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");
}

/// A live schema that differs from the expected one is a typed mismatch naming table, column,
/// expected and found -- a changed type, a missing column, an extra column, a reordered column
/// and a wrong schema version -- and nothing is written or migrated over it.
#[test]
fn schema_mismatches_are_typed_and_name_table_column_expected_and_found() {
    let run = |triples: Vec<Triple>, versions: Vec<u32>| {
        let fake = fake_ch(move |req| {
            let sql = req.sql();
            if sql.contains("system.columns") {
                ok(&columns_body(&triples))
            } else if sql.contains("FROM `av_sweep_schema_version`") {
                ok(&version_body(&versions))
            } else {
                ok(b"")
            }
        });
        let err = ClickHouseStudyStore::connect(&fake.config()).unwrap_err();
        let sent_ddl = fake.requests().iter().any(|r| r.sql().contains("CREATE TABLE") || r.sql().contains("INSERT"));
        (err, sent_ddl)
    };
    let mismatch = |err: StoreError| match err {
        StoreError::SchemaMismatch { table, column, expected, found } => (table, column, expected, found),
        other => panic!("expected SchemaMismatch, got {other:?}"),
    };

    // A changed type.
    let mut t = expected_columns();
    t.iter_mut().find(|(tb, c, _)| tb == schema::STUDIES && c == "sample_count").unwrap().2 = "UInt32".to_string();
    let (err, sent_ddl) = run(t, vec![1]);
    let (table, column, expected, found) = mismatch(err);
    assert_eq!((table.as_str(), column.as_str()), (schema::STUDIES, "sample_count"));
    assert!(expected.contains("UInt64") && found.contains("UInt32"), "{expected} / {found}");
    assert!(!sent_ddl, "a mismatching schema must not be migrated over");

    // A missing column (the last column of the scores table).
    let mut t = expected_columns();
    let idx = t.iter().rposition(|(tb, _, _)| tb == schema::SCORES).unwrap();
    t.remove(idx);
    let (err, _) = run(t, vec![1]);
    let (table, column, _expected, found) = mismatch(err);
    assert_eq!((table.as_str(), column.as_str()), (schema::SCORES, "passed"));
    assert!(found.contains("no such column"), "{found}");

    // An extra column.
    let mut t = expected_columns();
    let idx = t.iter().rposition(|(tb, _, _)| tb == schema::SEEDS).unwrap();
    t.insert(idx + 1, (schema::SEEDS.to_string(), "surprise".to_string(), "String".to_string()));
    let (err, _) = run(t, vec![1]);
    let (table, column, expected, found) = mismatch(err);
    assert_eq!((table.as_str(), column.as_str()), (schema::SEEDS, "surprise"));
    assert!(expected.contains("no such column") && found.contains("surprise"), "{expected} / {found}");

    // Two columns swapped (same names and types present, wrong positions).
    let mut t = expected_columns();
    let a = t.iter().position(|(tb, c, _)| tb == schema::AGGREGATES && c == "mean").unwrap();
    t.swap(a, a + 1);
    let (err, _) = run(t, vec![1]);
    let (table, column, _, found) = mismatch(err);
    assert_eq!((table.as_str(), column.as_str()), (schema::AGGREGATES, "mean"));
    assert!(found.contains("std_dev") && found.contains("position 7"), "{found}");

    // A schema version from the future.
    let (err, _) = run(expected_columns(), vec![2]);
    let (table, column, expected, found) = mismatch(err);
    assert_eq!((table.as_str(), column.as_str(), expected.as_str(), found.as_str()), (schema::SCHEMA_VERSION_TABLE, "version", "1", "2"));
}

/// A store with no tables at all is migrated (every `CREATE TABLE IF NOT EXISTS`, then the version
/// row), and then verified again.
#[test]
fn an_empty_database_is_migrated_then_verified() {
    let created = Arc::new(Mutex::new(false));
    let created2 = created.clone();
    let fake = fake_ch(move |req| {
        let sql = req.sql();
        if sql.contains("CREATE TABLE") {
            *created2.lock().unwrap() = true;
            ok(b"")
        } else if sql.contains("system.columns") {
            if *created2.lock().unwrap() {
                ok(&columns_body(&expected_columns()))
            } else {
                ok(b"")
            }
        } else if sql.contains("FROM `av_sweep_schema_version`") && sql.contains("FORMAT RowBinary") {
            ok(&version_body(&[1]))
        } else {
            ok(b"")
        }
    });
    ClickHouseStudyStore::connect(&fake.config()).expect("migrate then verify");
    let sqls: Vec<String> = fake.requests().iter().map(Recorded::sql).collect();
    let creates = sqls.iter().filter(|s| s.contains("CREATE TABLE IF NOT EXISTS")).count();
    assert_eq!(creates, TABLES.len(), "one CREATE TABLE IF NOT EXISTS per table");
    assert!(sqls.iter().any(|s| s.contains("INSERT INTO `av_sweep_schema_version`")), "the version row is recorded");
}

/// A server that refuses the insert (a read-only user) is a typed `RefusedWrite` carrying the
/// server's code and name; the header is never inserted after a refused insert.
#[test]
fn a_server_refusing_the_insert_is_a_typed_refused_write_and_no_header_lands() {
    let fake = fake_ch(|req| {
        let sql = req.sql();
        if sql.starts_with("INSERT INTO") {
            exception(500, 497, "ACCESS_DENIED", "avsweep: Not enough privileges. To execute this query, it's necessary to have the grant INSERT ON studies.av_sweep_samples")
        } else {
            healthy(req)
        }
    });
    let mut store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let err = store.put_study(&one_sample_study("s1")).unwrap_err();
    match &err {
        StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::Server { code, name, http_status, message } } => {
            assert_eq!(sweep_id, "s1");
            assert_eq!(*code, Some(497));
            assert_eq!(name.as_deref(), Some("ACCESS_DENIED"));
            assert_eq!(*http_status, 500);
            assert!(message.contains("Not enough privileges"), "{message}");
        }
        other => panic!("expected RefusedWrite/Server, got {other:?}"),
    }
    assert!(err.to_string().contains("s1") && err.to_string().contains("497"), "{err}");
    let inserts: Vec<String> = fake.requests().iter().map(Recorded::sql).filter(|s| s.starts_with("INSERT INTO")).collect();
    assert_eq!(inserts.len(), 1, "the writer stops at the first refused insert: {inserts:?}");
    assert!(!inserts.iter().any(|s| s.contains("av_sweep_studies")), "no header after a refusal");
}

/// The write protocol on the wire: every row of one write carries the same write id, the six
/// row tables go first and the header is inserted LAST, and the client then re-reads the header.
#[test]
fn a_write_inserts_every_table_under_one_write_id_and_the_header_last() {
    let study = one_sample_study("wire");
    let sha = content_sha256(&study);
    let write_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let wid2 = write_id.clone();
    let sha2 = sha.clone();
    let fake = fake_ch(move |req| {
        let sql = req.sql();
        if sql.starts_with("INSERT INTO `av_sweep_studies`") {
            let mut d = av_sweep::clickhouse::Decoder::new(&req.body);
            let _sweep_id = d.string().unwrap();
            *wid2.lock().unwrap() = Some(d.string().unwrap());
            ok(b"")
        } else if sql.contains("FROM `av_sweep_studies` WHERE sweep_id") {
            match wid2.lock().unwrap().as_deref() {
                None => ok(b""),
                Some(w) => ok(&header_body(w, &sha2, 1, 1)),
            }
        } else {
            healthy(req)
        }
    });
    let mut store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let location = store.put_study(&study).expect("put");

    let inserts: Vec<Recorded> = fake.requests().into_iter().filter(|r| r.sql().starts_with("INSERT INTO")).collect();
    let tables: Vec<String> = inserts.iter().map(|r| r.sql().split('`').nth(1).unwrap().to_string()).collect();
    assert_eq!(tables, vec![schema::SAMPLES, schema::AXIS_VALUES, schema::SEEDS, schema::SCORES, schema::AGGREGATES, schema::PROVENANCE_ATTRIBUTES, schema::STUDIES], "header last");
    let wid = write_id.lock().unwrap().clone().expect("header was inserted");
    assert_eq!(wid.len(), 32);
    for r in &inserts {
        let mut d = av_sweep::clickhouse::Decoder::new(&r.body);
        assert_eq!(d.string().unwrap(), "wire", "every row starts with the sweep id");
        assert_eq!(d.string().unwrap(), wid, "every row of one write carries the same write id");
        assert!(r.sql().contains("FORMAT RowBinary"));
    }
    assert!(location.ends_with(&format!("#{wid}")), "{location}");
    assert!(!location.contains("avsweep") || !location.contains('@'), "{location}");
}

/// Identifiers are bound as `param_` values, never interpolated into the SQL; characters that the
/// text format treats specially are escaped.
#[test]
fn study_ids_are_bound_parameters_not_sql_text() {
    let fake = fake_ch(healthy);
    let store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    for id in ["x' OR 1=1 --", "tab\there", "back\\slash", "new\nline", "", "ünï", "a&b=c"] {
        assert!(store.get_study(id).unwrap().is_none());
        let r = fake.requests().into_iter().rev().find(|r| r.sql().contains("FROM `av_sweep_studies` WHERE sweep_id")).unwrap();
        let sent = r.param("sid").unwrap_or_else(|| panic!("no param_sid for {id:?}: {}", r.target));
        let expected = id.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n");
        assert_eq!(sent, expected, "{id:?}");
        if !id.is_empty() {
            assert!(!r.sql().contains(id), "the id must not be in the SQL text: {id:?}");
        }
        assert!(r.sql().contains("{sid:String}"));
    }
}

/// No ClickHouse settings anywhere: a `readonly = 1` user is refused when a request changes a
/// setting, which would make every read by such a user fail.
#[test]
fn no_request_carries_a_clickhouse_setting() {
    // One fake that holds a study (so the MoE query is actually sent), one that holds none (so a
    // put inserts every table).
    let holder = fake_ch(|req| if req.sql().contains("FROM `av_sweep_studies` WHERE sweep_id") { ok(&header_body("w", "sha", 0, 0)) } else { healthy(req) });
    let empty = fake_ch(healthy);
    let store = ClickHouseStudyStore::connect(&holder.config()).unwrap();
    let _ = store.get_study("a");
    let _ = store.list_studies();
    let moe = store.aggregates_in_sql("a");
    assert!(moe.is_ok(), "an empty reply is an empty aggregate list: {moe:?}");
    let mut store = ClickHouseStudyStore::connect(&empty.config()).unwrap();
    let _ = store.put_study(&one_sample_study("b"));
    let mut all = holder.requests();
    all.extend(empty.requests());
    assert!(all.iter().any(|r| r.sql().contains("arrayFold")), "the MoE query must have been sent");
    assert!(all.iter().any(|r| r.sql().starts_with("INSERT INTO")), "the inserts must have been sent");
    for r in all {
        let lower = format!("{} {}", r.target, r.sql()).to_ascii_lowercase();
        for banned in ["settings", "readonly", "wait_end_of_query", "max_threads", "max_execution", "max_memory", "max_insert", "async_insert", "session_id"] {
            assert!(!lower.contains(banned), "request carries {banned:?}: {} {}", r.target, r.sql());
        }
    }
}

/// A reply cut off mid-stream, or one that disagrees with the header's counts, is `Corrupt`:
/// never a shorter study that looks valid.
#[test]
fn a_truncated_or_inconsistent_reply_is_corrupt_not_a_shorter_study() {
    let reply_for = |samples_body: Vec<u8>| {
        move |req: &Recorded| {
            let sql = req.sql();
            if sql.contains("FROM `av_sweep_studies` WHERE sweep_id") {
                ok(&header_body("w1", "deadbeef", 2, 0))
            } else if sql.contains("FROM `av_sweep_samples`") {
                ok(&samples_body)
            } else if sql.contains("system.columns") || sql.contains("av_sweep_schema_version") {
                healthy(req)
            } else {
                ok(b"")
            }
        }
    };
    let sample_row = |ordinal: u64| {
        let mut e = Encoder::new();
        e.u64(ordinal);
        e.u32(0);
        e.u32(0);
        for _ in 0..4 {
            e.string("");
        }
        e.finish()
    };

    // The header promises 2 samples; the server returns 1.
    let fake = fake_ch(reply_for(sample_row(0)));
    let store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let err = store.get_study("s").unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt { detail, .. } if detail.contains("2 sample")), "{err:?}");

    // The second row is cut off mid-value.
    let mut two = sample_row(0);
    let mut second = sample_row(1);
    second.truncate(second.len() - 3);
    two.extend(second);
    let fake = fake_ch(reply_for(two));
    let store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let err = store.get_study("s").unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt { .. }), "{err:?}");

    // Both rows complete but the rows hash to something other than the header's recorded hash.
    let mut two = sample_row(0);
    two.extend(sample_row(1));
    let fake = fake_ch(move |req| {
        let sql = req.sql();
        if sql.contains("FROM `av_sweep_studies` WHERE sweep_id") {
            ok(&header_body("w1", "deadbeef", 2, 0))
        } else if sql.contains("FROM `av_sweep_samples`") {
            ok(&two)
        } else if sql.contains("system.columns") || sql.contains("av_sweep_schema_version") {
            healthy(req)
        } else {
            ok(b"")
        }
    });
    let store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let err = store.get_study("s").unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt { detail, .. } if detail.contains("deadbeef")), "{err:?}");
}

/// Studies are immutable: the same id and content is an idempotent `Ok` (and writes nothing); the
/// same id with different content is a typed `RefusedWrite::DifferentContent`.
#[test]
fn an_existing_study_is_idempotent_for_identical_content_and_refused_for_different_content() {
    let study = one_sample_study("same-id");
    let sha = content_sha256(&study);
    let fake = fake_ch(move |req| {
        let sql = req.sql();
        if sql.contains("FROM `av_sweep_studies` WHERE sweep_id") {
            ok(&header_body("existing-write", &sha, 1, 1))
        } else {
            healthy(req)
        }
    });
    let mut store = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let location = store.put_study(&study).expect("identical content is idempotent");
    assert!(location.ends_with("#existing-write"), "{location}");
    assert!(!fake.requests().iter().any(|r| r.sql().starts_with("INSERT INTO")), "an idempotent put writes nothing");

    let mut different = study.clone();
    different.samples[0].draw_index = 99;
    let err = store.put_study(&different).unwrap_err();
    match err {
        StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::DifferentContent { existing_write_id, existing_sha256, new_sha256 } } => {
            assert_eq!(sweep_id, "same-id");
            assert_eq!(existing_write_id, "existing-write");
            assert_ne!(existing_sha256, new_sha256);
        }
        other => panic!("expected RefusedWrite/DifferentContent, got {other:?}"),
    }
    assert!(!fake.requests().iter().any(|r| r.sql().starts_with("INSERT INTO")), "a refused put writes nothing");
}

/// What a `put_study` answered, in terms both backends share: the outcome class and, for a
/// refusal, the two content hashes it names.
#[derive(Debug, PartialEq)]
enum Outcome {
    Ok,
    RefusedDifferent { sweep_id: String, existing_sha256: String, new_sha256: String },
}

fn outcome_of(result: Result<String, StoreError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Ok,
        Err(StoreError::RefusedWrite { sweep_id, reason: WriteRefusal::DifferentContent { existing_sha256, new_sha256, .. } }) => Outcome::RefusedDifferent { sweep_id, existing_sha256, new_sha256 },
        Err(other) => panic!("expected Ok or RefusedWrite/DifferentContent, got {other:?}"),
    }
}

/// A fake server that remembers headers: a header `INSERT` is stored under its sweep id (first one
/// wins, as the smallest `(created_at, write_id)` would for the real server) and a header lookup
/// answers from what was stored. Everything else is [`healthy`].
fn stateful_fake() -> FakeCh {
    let headers: Arc<Mutex<std::collections::BTreeMap<String, (String, String)>>> = Arc::default();
    fake_ch(move |req| {
        let sql = req.sql();
        if sql.starts_with("INSERT INTO `av_sweep_studies`") {
            let mut d = av_sweep::clickhouse::Decoder::new(&req.body);
            let (sweep_id, write_id) = (d.string().unwrap(), d.string().unwrap());
            let (_sweep_hash, _drm_hash, _samples, _aggregates) = (d.string().unwrap(), d.string().unwrap(), d.u64().unwrap(), d.u64().unwrap());
            let sha = d.string().unwrap();
            headers.lock().unwrap().entry(sweep_id).or_insert((write_id, sha));
            ok(b"")
        } else if sql.contains("FROM `av_sweep_studies` WHERE sweep_id") {
            match headers.lock().unwrap().get(&req.param("sid").unwrap()) {
                None => ok(b""),
                Some((write_id, sha)) => ok(&header_body(write_id, sha, 1, 1)),
            }
        } else {
            healthy(req)
        }
    })
}

/// Question 252: the file store and the ClickHouse store answer the same sequence of puts with the
/// same outcomes, refusals naming the same two hashes. (The real-ClickHouse counterpart of the
/// ClickHouse half is `tests/clickhouse_store.rs`; the file half is `store.rs`'s unit tests.)
#[test]
fn the_file_store_and_the_clickhouse_store_answer_the_same_puts_the_same_way() {
    let a1 = one_sample_study("a");
    let mut a2 = a1.clone();
    a2.samples[0].products_uri = "/another/out-dir/sample_p1_d2".to_string();
    let b1 = one_sample_study("b");
    let mut b2 = b1.clone();
    b2.samples[0].draw_index = 99;
    let sequence = [&a1, &a1, &a2, &b1, &b2, &a1, &a2, &b1];

    let fake = stateful_fake();
    let mut clickhouse = ClickHouseStudyStore::connect(&fake.config()).unwrap();
    let root = std::env::temp_dir().join(format!("av-sweep-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut file = av_sweep::store::FileStudyStore::new(&root);

    let from_clickhouse: Vec<Outcome> = sequence.iter().map(|s| outcome_of(clickhouse.put_study(s))).collect();
    let from_file: Vec<Outcome> = sequence.iter().map(|s| outcome_of(file.put_study(s))).collect();
    assert_eq!(from_file, from_clickhouse, "same puts, same outcomes, same hashes in the refusals");
    let refused = |id: &str, existing: &pb::SweepResults, new: &pb::SweepResults| Outcome::RefusedDifferent { sweep_id: id.to_string(), existing_sha256: content_sha256(existing), new_sha256: content_sha256(new) };
    assert_eq!(
        from_file,
        vec![Outcome::Ok, Outcome::Ok, refused("a", &a1, &a2), Outcome::Ok, refused("b", &b1, &b2), Outcome::Ok, refused("a", &a1, &a2), Outcome::Ok],
        "and they are the expected ones"
    );
    let _ = std::fs::remove_dir_all(&root);
}

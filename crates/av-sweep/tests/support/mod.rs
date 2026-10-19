//! Shared by the suites that talk to a **fake** ClickHouse HTTP server (`clickhouse_fake_server`,
//! `two_phase_stores`, `store_agreement_cli`): no Docker, loopback only.
//!
//! - [`fake_ch`] / [`FakeCh`]: a server that runs a handler per request and records every request.
//! - [`StatefulCh`]: a server that **keeps what is inserted** (every study table, row by row, and
//!   answers the store's reads from it, header first-wins like the real winner rule), so a whole
//!   `put_study` / `prepare` / `commit` / `get_study` runs against it. Faults can be switched on
//!   for a test: an insert into one table fails with a server exception, the next header insert
//!   is stored but its reply is lost, every read fails.
//! - [`Hooked`]: wraps a real store and runs a closure just before its `commit`, to put the world
//!   in the state a race or a failure would between the two phases.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use av_cdm::pb;
use av_sweep::clickhouse::schema::{self, TABLES};
use av_sweep::clickhouse::{ClickHouseConfig, Decoder, Encoder};
use av_sweep::plan::StudyPlan;
use av_sweep::store::{PreparedWrite, StoreError, StoreKind, StudyStore};

// ---------------------------------------------------------------------------------------------
// The fake server.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The SQL this request carries: the percent-decoded `query` parameter (inserts) or the body.
    pub fn sql(&self) -> String {
        if let Some(q) = self.target.split('?').nth(1).and_then(|qs| qs.split('&').find_map(|kv| kv.strip_prefix("query="))) {
            return percent_decode(q);
        }
        self.body_text()
    }

    pub fn param(&self, name: &str) -> Option<String> {
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

pub struct FakeCh {
    pub addr: SocketAddr,
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

/// Starts a server on a loopback port that answers every request with `handler`'s bytes (an empty
/// reply closes the connection without answering: a lost reply).
pub fn fake_ch(handler: impl Fn(&Recorded) -> Vec<u8> + Send + Sync + 'static) -> FakeCh {
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
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn config(&self) -> ClickHouseConfig {
        let mut c = ClickHouseConfig::new(self.url(), "studies", "avsweep");
        c.timeouts = av_http1::Timeouts::uniform(Duration::from_secs(3));
        c
    }

    pub fn requests(&self) -> Vec<Recorded> {
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

pub fn http(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
    for (n, v) in headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

pub fn ok(body: &[u8]) -> Vec<u8> {
    http(200, &[], body)
}

pub fn exception(status: u16, code: i64, name: &str, message: &str) -> Vec<u8> {
    let body = format!("Code: {code}. DB::Exception: {message}. ({name}) (version 26.8.29.10 (official build))\n");
    http(status, &[("X-ClickHouse-Exception-Code", &code.to_string())], body.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// Reply builders (RowBinary, written with the crate's own public encoder).
// ---------------------------------------------------------------------------------------------

pub type Triple = (String, String, String);

pub fn expected_columns() -> Vec<Triple> {
    TABLES.iter().flat_map(|t| t.columns.iter().map(move |c| (t.name.to_string(), c.name.to_string(), c.ty.to_string()))).collect()
}

pub fn columns_body(triples: &[Triple]) -> Vec<u8> {
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

pub fn version_body(versions: &[u32]) -> Vec<u8> {
    let mut e = Encoder::new();
    for v in versions {
        e.u32(*v);
    }
    e.finish()
}

pub fn header_body(write_id: &str, sha: &str, samples: u64, aggregates: u64) -> Vec<u8> {
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
pub fn healthy(req: &Recorded) -> Vec<u8> {
    let sql = req.sql();
    if sql.contains("system.columns") {
        ok(&columns_body(&expected_columns()))
    } else if sql.contains("FROM `av_sweep_schema_version`") {
        ok(&version_body(&[schema::SCHEMA_VERSION]))
    } else {
        ok(b"")
    }
}

pub fn one_sample_study(sweep_id: &str) -> pb::SweepResults {
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
// The stateful fake.
// ---------------------------------------------------------------------------------------------

/// One stored row: the bytes after its `sweep_id` and `write_id` columns, which are what every
/// read of the store selects (the header's read starts with the write id).
struct StoredRow {
    write_id: String,
    rest: Vec<u8>,
}

#[derive(Default)]
struct State {
    /// `(table, sweep_id)` to the rows inserted, in insertion order.
    rows: BTreeMap<(String, String), Vec<StoredRow>>,
    fail_inserts_into: Vec<String>,
    lose_next_header_reply: bool,
    fail_reads: bool,
    /// The next this many reads of the studies table (header lookups) fail, then reads work again.
    fail_next_header_reads: u32,
    /// Once a header insert has been processed, every read fails: the server going away right
    /// after the commit's insert, which is what the unknown-outcome window looks like.
    fail_reads_after_header_insert: bool,
}

/// Walks one value of ClickHouse type `ty` (only the types the study tables use).
fn skip(d: &mut Decoder<'_>, ty: &str) {
    match ty {
        "String" => {
            d.string().unwrap();
        }
        "UInt32" => {
            d.u32().unwrap();
        }
        "Int32" => {
            d.i32().unwrap();
        }
        "UInt64" => {
            d.u64().unwrap();
        }
        "Int64" => {
            d.i64().unwrap();
        }
        "Float64" => {
            d.f64().unwrap();
        }
        "Bool" => {
            d.bool().unwrap();
        }
        "Nullable(Bool)" => {
            d.nullable_bool().unwrap();
        }
        "Nullable(Float64)" => {
            d.nullable_f64().unwrap();
        }
        other => panic!("the stateful fake does not know column type {other:?}"),
    }
}

/// Splits an insert body into rows of `(sweep_id, row)`.
fn split_rows(table: &str, body: &[u8]) -> Vec<(String, StoredRow)> {
    let types = schema::table(table).insert_types();
    let mut d = Decoder::new(body);
    let mut out = Vec::new();
    while !d.is_done() {
        let sweep_id = d.string().unwrap();
        let write_id = d.string().unwrap();
        let start = d.position();
        for ty in &types[2..] {
            skip(&mut d, ty);
        }
        out.push((sweep_id, StoredRow { write_id, rest: body[start..d.position()].to_vec() }));
    }
    out
}

/// A fake ClickHouse that behaves like a healthy server holding what has been inserted into it.
pub struct StatefulCh {
    pub fake: FakeCh,
    state: Arc<Mutex<State>>,
}

impl StatefulCh {
    pub fn new() -> StatefulCh {
        let state: Arc<Mutex<State>> = Arc::default();
        let handler_state = state.clone();
        let fake = fake_ch(move |req| Self::handle(&handler_state, req));
        StatefulCh { fake, state }
    }

    fn handle(state: &Mutex<State>, req: &Recorded) -> Vec<u8> {
        let sql = req.sql();
        let mut state = state.lock().unwrap();
        if let Some(table) = sql.strip_prefix("INSERT INTO `").and_then(|rest| rest.split('`').next()) {
            if table == schema::SCHEMA_VERSION_TABLE {
                return ok(b"");
            }
            if state.fail_inserts_into.iter().any(|t| t == table) {
                return exception(500, 497, "ACCESS_DENIED", &format!("simulated refusal of the insert into {table}"));
            }
            for (sweep_id, row) in split_rows(table, &req.body) {
                state.rows.entry((table.to_string(), sweep_id)).or_default().push(row);
            }
            if table == schema::STUDIES && state.fail_reads_after_header_insert {
                state.fail_reads = true;
            }
            if table == schema::STUDIES && std::mem::take(&mut state.lose_next_header_reply) {
                return Vec::new(); // stored, but no reply reaches the client
            }
            return ok(b"");
        }
        let selected = [schema::STUDIES, schema::SAMPLES, schema::AXIS_VALUES, schema::SEEDS, schema::SCORES, schema::AGGREGATES, schema::PROVENANCE_ATTRIBUTES].into_iter().find(|t| sql.contains(&format!("FROM `{t}` WHERE sweep_id")));
        let Some(table) = selected else { return healthy(req) };
        if state.fail_reads || (table == schema::STUDIES && state.fail_next_header_reads > 0) {
            if table == schema::STUDIES && !state.fail_reads {
                state.fail_next_header_reads -= 1;
            }
            return exception(500, 159, "TIMEOUT_EXCEEDED", "simulated failure of a read");
        }
        let sweep_id = req.param("sid").expect("a study read binds {sid}");
        let rows = state.rows.get(&(table.to_string(), sweep_id)).map(Vec::as_slice).unwrap_or(&[]);
        if table == schema::STUDIES {
            // The winner is the first header inserted (the real rule is the smallest created_at).
            return match rows.first() {
                None => ok(b""),
                Some(h) => {
                    let mut reply = Encoder::new();
                    reply.string(&h.write_id);
                    let mut bytes = reply.finish();
                    bytes.extend_from_slice(&h.rest);
                    ok(&bytes)
                }
            };
        }
        let write_id = req.param("wid").expect("a row read binds {wid}");
        ok(&rows.iter().filter(|r| r.write_id == write_id).flat_map(|r| r.rest.iter().copied()).collect::<Vec<u8>>())
    }

    pub fn config(&self) -> ClickHouseConfig {
        self.fake.config()
    }

    /// Every insert into `table` is answered with a server exception and stores nothing.
    pub fn fail_inserts_into(&self, table: &str) {
        self.state.lock().unwrap().fail_inserts_into.push(table.to_string());
    }

    /// The next header insert is stored, but the connection closes without a reply.
    pub fn lose_next_header_reply(&self) {
        self.state.lock().unwrap().lose_next_header_reply = true;
    }

    /// Every read of a study table is answered with a server exception while `on`.
    pub fn fail_reads(&self, on: bool) {
        self.state.lock().unwrap().fail_reads = on;
    }

    /// The next `n` header lookups fail with a server exception, then lookups work again.
    pub fn fail_next_header_reads(&self, n: u32) {
        self.state.lock().unwrap().fail_next_header_reads = n;
    }

    /// The unknown-outcome window: the next header insert is stored but its reply is lost, and
    /// from then on every read fails (until [`Self::clear_faults`]).
    pub fn lose_the_header_reply_and_then_fail_every_read(&self) {
        let mut state = self.state.lock().unwrap();
        state.lose_next_header_reply = true;
        state.fail_reads_after_header_insert = true;
    }

    /// Header inserts and header lookups the server has received, from its request log.
    pub fn header_requests(&self) -> (usize, usize) {
        let log = self.fake.requests();
        let inserts = log.iter().filter(|r| r.sql().starts_with("INSERT INTO `av_sweep_studies`")).count();
        let reads = log.iter().filter(|r| r.sql().contains("FROM `av_sweep_studies` WHERE sweep_id")).count();
        (inserts, reads)
    }

    /// Switches every fault off (the rows stay).
    pub fn clear_faults(&self) {
        let mut state = self.state.lock().unwrap();
        state.fail_next_header_reads = 0;
        state.fail_reads_after_header_insert = false;
        state.fail_inserts_into.clear();
        state.lose_next_header_reply = false;
        state.fail_reads = false;
    }

    /// Rows stored in `table` for `sweep_id`, whatever their write.
    pub fn row_count(&self, table: &str, sweep_id: &str) -> usize {
        self.state.lock().unwrap().rows.get(&(table.to_string(), sweep_id.to_string())).map_or(0, Vec::len)
    }

    /// Write ids that have rows for `sweep_id` but no header: the invisible orphans.
    pub fn orphan_write_ids(&self, sweep_id: &str) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let headers: Vec<&str> = state.rows.get(&(schema::STUDIES.to_string(), sweep_id.to_string())).map(|h| h.iter().map(|r| r.write_id.as_str()).collect()).unwrap_or_default();
        let mut orphans: Vec<String> = state.rows.iter().filter(|((table, id), _)| table == schema::SAMPLES && id == sweep_id).flat_map(|(_, rows)| rows.iter().map(|r| r.write_id.clone())).filter(|w| !headers.contains(&w.as_str())).collect();
        orphans.sort();
        orphans.dedup();
        orphans
    }
}

// ---------------------------------------------------------------------------------------------
// A store with a hook between the phases.
// ---------------------------------------------------------------------------------------------

/// Wraps a real store; `before_commit` runs (once) just before the wrapped store's `commit`, to
/// do what a racing writer or a failing disk would between prepare and commit.
pub struct Hooked {
    pub inner: Box<dyn StudyStore>,
    pub before_commit: Option<Box<dyn FnOnce() + Send>>,
}

impl Hooked {
    pub fn new(inner: Box<dyn StudyStore>, before_commit: impl FnOnce() + Send + 'static) -> Hooked {
        Hooked { inner, before_commit: Some(Box::new(before_commit)) }
    }
}

impl StudyStore for Hooked {
    fn kind(&self) -> StoreKind {
        self.inner.kind()
    }
    fn check_plan(&self, plan: &StudyPlan) -> Result<(), StoreError> {
        self.inner.check_plan(plan)
    }
    fn prepare(&mut self, results: &pb::SweepResults) -> Result<PreparedWrite, StoreError> {
        self.inner.prepare(results)
    }
    fn commit(&mut self, prepared: PreparedWrite) -> Result<String, StoreError> {
        if let Some(hook) = self.before_commit.take() {
            hook();
        }
        self.inner.commit(prepared)
    }
    fn abort(&mut self, prepared: PreparedWrite) -> Result<(), StoreError> {
        self.inner.abort(prepared)
    }
    fn get_study(&self, sweep_id: &str) -> Result<Option<pb::SweepResults>, StoreError> {
        self.inner.get_study(sweep_id)
    }
    fn list_studies(&self) -> Result<Vec<av_sweep::store::StudySummary>, StoreError> {
        self.inner.list_studies()
    }
}

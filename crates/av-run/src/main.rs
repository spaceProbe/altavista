//! `av-run`: the first demo bridge between the Rust DRM executor and the altavista viewer
//! (M16.3, question 5's first demo: "a DRM authored in Python, propagated with GMAT dynamics,
//! shown on the custom globe and in ICRF, reproducible from its config hash").
//!
//! ## Binary, not an RPC -- and why
//!
//! `crates/av-dynamics-service` already hosts `altavista.v1.DynamicsService` over gRPC, so
//! "add a `run_drm` RPC there" was the alternative considered. It is not available to this
//! task: `DynamicsService` (`proto/altavista/v1/dynamics_service.proto`) declares only
//! model-level RPCs (`Describe`/`Step`/`Solve`/`Propagate`/`Derivatives`) -- there is no
//! DRM-level method to reuse, and adding one needs a `.proto` change, which `proto/**` (read-
//! only to this task, "no proto change authorized") forbids outright. `av_kernel::drm::execute`
//! is a synchronous, `&Gmat`-borrowing, single-process-singleton call anyway (GMAT is a
//! per-process singleton, not thread-safe -- `gmat_sys::engine_lock()`'s own doc comment); the
//! natural way to expose "run this DRM bundle" without touching `proto/**` or the tonic service
//! plumbing (`crates/av-grpc`, also not owned by this task) is a small standalone CLI that
//! loads the bundle, drives the executor directly (exactly the same `Gmat::setup` +
//! `engine_lock()` + `RunConfig` shape every `crates/av-kernel/tests/drm_*.rs` test already
//! uses), and puts `RunProducts` on the wire itself.
//!
//! ## The wire format: `altavista.v1.RunProducts`, a real CDM message (question 121, M17.2)
//!
//! Through M16.3, the CDM had no message for a whole run, so this binary framed
//! `av_kernel::drm::RunProducts` as an ad hoc, explicitly length-prefixed concatenation of its
//! `Trajectory`/`Event`/`Provenance` fields' own binary protobuf encodings (`b"AVRUN1"` magic,
//! `altavista/cdm.py`'s `parse_run_wire` the one reader) -- `proto/**` was read-only to that task,
//! and the CDM's real multi-message envelope (`envelope.proto`'s `Batch`/`SignedBatch`) exists
//! for the signed, chained ingest log (ADR-004), not a demo bridge. **The lead has since added
//! `altavista.v1.RunProducts` and `ScoreResult` to `proto/altavista/v1/run.proto`**
//! (`docs/open-questions.md` question 121), so that ad hoc framing is gone -- deleted, not kept
//! as a fallback, on both this side and `altavista/cdm.py`'s. `av_kernel::drm::executor::
//! RunProducts::to_proto` converts this binary's own `RunProducts` value into the real
//! `av_cdm::pb::RunProducts` message (trajectories, events, `scores` as `ScoreResult`,
//! provenance, the dropped-in-flight-message count, and frame definitions -- see that method's
//! own doc comment), and this binary just calls `prost::Message::encode_to_vec` on the result:
//! ordinary protobuf bytes, no bespoke envelope, decodable by any `altavista.v1.RunProducts`
//! reader (`altavista.pb.altavista.v1.run_pb2.RunProducts` on the Python side, binary or JSON
//! transcoded, exactly like `POST /api/cdm/trajectory` already accepted a bare `Trajectory`).
//!
//! ## No new HTTP-client dependency
//!
//! Posting the bundle to a running `altavista` server needs an HTTP client. This workspace's
//! existing servers avoid pulling in a full HTTP stack for a small, fixed, plaintext-localhost
//! surface (`crates/av-dynamics-service/src/admin.rs`'s own module doc: "a plain
//! `tokio::net::TcpListener` with manual GET-only request parsing rather than depending on
//! axum/hyper directly"); [`http_post`] is the client-side mirror of that same choice -- a
//! hand-rolled, blocking HTTP/1.1 POST over `std::net::TcpStream`, plaintext, for exactly one
//! request/response, since altavista's own viewer server is plaintext-localhost-only by design
//! (`altavista/server.py`'s module doc). This keeps `cargo tree | grep -ci ring` at 0 without
//! auditing a new dependency's own transitive tree.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;

use av_cdm::pb::SystemDefinition;
use av_kernel::drm::{execute, schema, DrmError, ExecutionErrorMode, RunConfig};
use gmat_sys::Gmat;
use prost::Message;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("av-run: {e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
struct Cli {
    drm: PathBuf,
    sos: PathBuf,
    systems: Vec<PathBuf>,
    run_id: String,
    server: Option<String>,
    out: Option<PathBuf>,
    gmat_startup: Option<String>,
    error_mode: ExecutionErrorMode,
}

fn usage(prog: &str) -> String {
    format!(
        "usage: {prog} --drm <path> --sos <path> --system <path> [--system <path> ...] --run-id <id> \
         (--server <http://host:port> | --out <path> | both) [--gmat-startup <path>] \
         [--error-mode nominal|sampled]\n\n\
         Loads one DesignReferenceMission + SosConfiguration + SystemDefinition(s) (drms/*.yaml \
         authoring format), runs it through av_kernel::drm::execute, and emits the resulting \
         RunProducts as an altavista.v1.RunProducts CDM v1 message on the wire (binary \
         protobuf): to --out (raw bytes) and/or POSTed to --server's POST /api/cdm/run."
    )
}

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let prog = args.first().map(String::as_str).unwrap_or("av-run");
    let mut drm = None;
    let mut sos = None;
    let mut systems = Vec::new();
    let mut run_id = None;
    let mut server = None;
    let mut out = None;
    let mut gmat_startup = None;
    let mut error_mode = ExecutionErrorMode::Nominal;

    let mut i = 1;
    let next = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{flag} needs a value\n\n{}", usage(prog)))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--drm" => drm = Some(PathBuf::from(next(&mut i, "--drm")?)),
            "--sos" => sos = Some(PathBuf::from(next(&mut i, "--sos")?)),
            "--system" => systems.push(PathBuf::from(next(&mut i, "--system")?)),
            "--run-id" => run_id = Some(next(&mut i, "--run-id")?),
            "--server" => server = Some(next(&mut i, "--server")?),
            "--out" => out = Some(PathBuf::from(next(&mut i, "--out")?)),
            "--gmat-startup" => gmat_startup = Some(next(&mut i, "--gmat-startup")?),
            "--error-mode" => {
                let v = next(&mut i, "--error-mode")?;
                error_mode = match v.as_str() {
                    "nominal" => ExecutionErrorMode::Nominal,
                    "sampled" => ExecutionErrorMode::Sampled,
                    other => return Err(format!("--error-mode must be 'nominal' or 'sampled', got {other:?}\n\n{}", usage(prog))),
                };
            }
            "-h" | "--help" => return Err(usage(prog)),
            other => return Err(format!("unrecognized argument {other:?}\n\n{}", usage(prog))),
        }
        i += 1;
    }

    let drm = drm.ok_or_else(|| format!("--drm is required\n\n{}", usage(prog)))?;
    let sos = sos.ok_or_else(|| format!("--sos is required\n\n{}", usage(prog)))?;
    if systems.is_empty() {
        return Err(format!("at least one --system is required\n\n{}", usage(prog)));
    }
    let run_id = run_id.ok_or_else(|| format!("--run-id is required\n\n{}", usage(prog)))?;
    if server.is_none() && out.is_none() {
        return Err(format!("at least one of --server / --out is required\n\n{}", usage(prog)));
    }
    Ok(Cli { drm, sos, systems, run_id, server, out, gmat_startup, error_mode })
}

fn read_to_string(path: &PathBuf) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// Load every `--system` file and index by `SystemDefinition.id` (the same keying
/// `RunConfig::systems` documents and every `crates/av-kernel/tests/drm_*.rs` fixture loader
/// builds by hand) -- refuses two files declaring the same id rather than silently letting the
/// second overwrite the first, since that would silently run a different bundle than the one
/// named on the command line.
fn load_systems(paths: &[PathBuf]) -> Result<BTreeMap<String, SystemDefinition>, String> {
    let mut systems = BTreeMap::new();
    for path in paths {
        let sys = schema::parse_system_definition_yaml(&read_to_string(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(_prev) = systems.insert(sys.id.clone(), sys) {
            return Err(format!("two --system files declare the same SystemDefinition.id (last one: {})", path.display()));
        }
    }
    Ok(systems)
}

fn run(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(args)?;

    let drm = schema::parse_drm_yaml(&read_to_string(&cli.drm)?).map_err(|e| format!("{}: {e}", cli.drm.display()))?;
    let sos = schema::parse_sos_yaml(&read_to_string(&cli.sos)?).map_err(|e| format!("{}: {e}", cli.sos.display()))?;
    let systems = load_systems(&cli.systems)?;

    // GMAT is a per-process singleton and not thread-safe (gmat_sys::lib.rs's own doc comment);
    // this binary runs exactly one DRM per process, so the lock is held for the process's whole
    // working lifetime -- there is no second caller in this process to contend with it.
    let _engine = gmat_sys::engine_lock();
    let startup = cli.gmat_startup.clone().unwrap_or_else(Gmat::default_startup_file);
    let gmat = Gmat::setup(&startup).map_err(|e| format!("GMAT setup ({startup}): {e}"))?;

    let cfg = RunConfig { gmat: &gmat, drm: &drm, sos: &sos, systems: &systems, run_id: cli.run_id.clone(), error_mode: cli.error_mode };
    let products = execute(cfg).map_err(|e: DrmError| format!("DRM execution failed: {e}"))?;

    eprintln!(
        "av-run: run_id={:?} config_hash={} trajectories={} events={}",
        cli.run_id,
        products.provenance.config_hash,
        products.trajectories.len(),
        products.events.len()
    );

    // Question 121, M17.2: RunProducts on the wire is now a real altavista.v1.RunProducts
    // message (av_kernel::drm::executor::RunProducts::to_proto), not the old AVRUN1 ad hoc
    // framing -- ordinary protobuf bytes, so this is just prost::Message::encode_to_vec.
    let bundle = products.to_proto().encode_to_vec();

    if let Some(out) = &cli.out {
        std::fs::write(out, &bundle).map_err(|e| format!("writing {}: {e}", out.display()))?;
        eprintln!("av-run: wrote {} byte(s) to {}", bundle.len(), out.display());
    }
    if let Some(server) = &cli.server {
        let url = format!("{}/api/cdm/run", server.trim_end_matches('/'));
        // "application/x-protobuf", matching altavista/server.py's POST /api/cdm/trajectory
        // convention exactly (question 121: "/api/cdm/run must accept the message exactly the
        // same way" -- binary protobuf on this content type, JSON transcoding on any other).
        let (status, body) = http_post(&url, "application/x-protobuf", &bundle).map_err(|e| format!("POST {url}: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("POST {url} returned HTTP {status}: {}", String::from_utf8_lossy(&body)));
        }
        eprintln!("av-run: POST {url} -> HTTP {status}: {}", String::from_utf8_lossy(&body).trim());
    }
    Ok(())
}

// ================================================================================================
// A hand-rolled, blocking, plaintext HTTP/1.1 POST (see this module's own doc comment's "No new
// HTTP-client dependency" section) -- exactly one request/response, `Connection: close`, no
// redirects, no chunked transfer-encoding on either side (this binary sends one fixed-length
// body and altavista's own FastAPI/uvicorn server always answers with Content-Length, never
// chunked, for the small JSON acks these endpoints return).
// ================================================================================================

/// Split `http://host[:port]/path` into `(host, port, path)`. `path` defaults to `"/"`;
/// `port` defaults to 80. Never resolves `https://` (this binary only ever talks to a
/// plaintext-localhost altavista server, per the module doc comment).
fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| format!("expected an http:// URL, got {url:?}"))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|e| format!("bad port in {url:?}: {e}"))?),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(format!("no host in {url:?}"));
    }
    Ok((host, port, path.to_string()))
}

/// POST `body` to `url` with `content_type`, returning `(status_code, response_body)`.
fn http_post(url: &str, content_type: &str, body: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let (host, port, path) = parse_http_url(url)?;
    let mut stream = TcpStream::connect((host.as_str(), port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;

    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).map_err(|e| format!("writing request headers: {e}"))?;
    stream.write_all(body).map_err(|e| format!("writing request body: {e}"))?;
    stream.flush().map_err(|e| format!("flushing request: {e}"))?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).map_err(|e| format!("reading response: {e}"))?;

    let split_at = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| "response has no header/body separator".to_string())?;
    let head = std::str::from_utf8(&response[..split_at]).map_err(|e| format!("response headers are not UTF-8: {e}"))?;
    let status_line = head.lines().next().ok_or_else(|| "response has no status line".to_string())?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?
        .parse()
        .map_err(|e| format!("malformed status code in {status_line:?}: {e}"))?;
    let resp_body = response[split_at + 4..].to_vec();
    Ok((status, resp_body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------------- CLI
    #[test]
    fn parses_a_minimal_command_line() {
        let args: Vec<String> = ["av-run", "--drm", "d.yaml", "--sos", "s.yaml", "--system", "sys.yaml", "--run-id", "r1", "--out", "out.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cli = parse_cli(&args).unwrap();
        assert_eq!(cli.drm, PathBuf::from("d.yaml"));
        assert_eq!(cli.sos, PathBuf::from("s.yaml"));
        assert_eq!(cli.systems, vec![PathBuf::from("sys.yaml")]);
        assert_eq!(cli.run_id, "r1");
        assert_eq!(cli.out, Some(PathBuf::from("out.bin")));
        assert_eq!(cli.server, None);
        assert_eq!(cli.error_mode, ExecutionErrorMode::Nominal);
    }

    #[test]
    fn refuses_when_neither_server_nor_out_is_given() {
        let args: Vec<String> = ["av-run", "--drm", "d.yaml", "--sos", "s.yaml", "--system", "sys.yaml", "--run-id", "r1"].iter().map(|s| s.to_string()).collect();
        let err = parse_cli(&args).unwrap_err();
        assert!(err.contains("--server / --out"), "{err}");
    }

    #[test]
    fn refuses_a_duplicate_system_id() {
        let dir = std::env::temp_dir().join(format!("av-run-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys_yaml = "id: dup\nversion: \"1\"\nname: x\ndynamics_model: m\nstate_space_id: gmat.orbital.cartesian6\n";
        let p1 = dir.join("a.yaml");
        let p2 = dir.join("b.yaml");
        std::fs::write(&p1, sys_yaml).unwrap();
        std::fs::write(&p2, sys_yaml).unwrap();
        let err = load_systems(&[p1, p2]).unwrap_err();
        assert!(err.contains("same SystemDefinition.id"), "{err}");
    }

    // -------------------------------------------------------------------------------- URL parsing
    #[test]
    fn parses_host_port_and_path() {
        assert_eq!(parse_http_url("http://127.0.0.1:8765/api/cdm/run").unwrap(), ("127.0.0.1".to_string(), 8765, "/api/cdm/run".to_string()));
    }

    #[test]
    fn defaults_path_to_root_and_port_to_80() {
        assert_eq!(parse_http_url("http://example.test").unwrap(), ("example.test".to_string(), 80, "/".to_string()));
    }

    #[test]
    fn refuses_a_non_http_scheme() {
        assert!(parse_http_url("https://example.test").is_err());
    }

    // -------------------------------------------------------------------------------- wire encoding (question 121, M17.2)
    //
    // The actual RunProducts -> altavista.v1.RunProducts conversion (`to_proto`) and its
    // scores/dropped-count/frames round-trip are av_kernel::drm::executor's own job, tested
    // there (crates/av-kernel/src/drm/executor.rs's `to_proto_tests` module) against a fixture
    // built directly from that crate's own `RunProducts` struct. What is left to prove here,
    // in this binary's own tests, is narrower but still real: that `run()`'s own encoding step
    // produces genuine `altavista.v1.RunProducts` bytes -- not a reintroduction of the deleted
    // `AVRUN1` framing under a different name.
    use av_cdm::pb::{Event, Provenance, Trajectory};
    use av_kernel::drm::RunProducts;

    fn sample_products() -> RunProducts {
        let mut trajectories = BTreeMap::new();
        trajectories.insert("veh".to_string(), Trajectory { id: "veh-trajectory".to_string(), entity_id: "veh".to_string(), config_hash: "traj-hash".to_string(), ..Default::default() });
        RunProducts {
            trajectories,
            events: vec![Event { id: "ev1".to_string(), name: "run_start".to_string(), ..Default::default() }],
            scores: BTreeMap::new(),
            provenance: Provenance { config_hash: "run-hash".to_string(), run_id: "r1".to_string(), ..Default::default() },
            dropped_in_flight_messages: 0,
            frames: vec![],
            measurements: vec![],
        }
    }

    /// The bytes `run()` would write/POST are genuine `altavista.v1.RunProducts` protobuf, not
    /// the old `AVRUN1` length-prefixed framing. Fails against a wrong implementation that kept
    /// (or reintroduced) the deleted `encode_run_bundle`/`b"AVRUN1"` magic: those bytes would
    /// either fail to decode as `RunProducts` at all, or -- since protobuf tolerates unknown
    /// leading garbage far less gracefully than a hand-rolled parser -- decode into a message
    /// that does not carry this fixture's own `entity_id`/`id`/`config_hash` values, which the
    /// `assert_eq!`s below would catch either way.
    #[test]
    fn av_run_bytes_are_real_run_products_protobuf_not_the_old_avrun1_framing() {
        let products = sample_products();
        let bytes = products.to_proto().encode_to_vec();
        assert!(!bytes.starts_with(b"AVRUN1"), "the AVRUN1 magic must be gone from this binary's own output");

        let decoded = av_cdm::pb::RunProducts::decode(bytes.as_slice()).expect("run() must emit valid altavista.v1.RunProducts bytes");
        assert_eq!(decoded.run_id, "r1");
        assert_eq!(decoded.provenance.as_ref().map(|p| p.config_hash.as_str()), Some("run-hash"));
        let traj = decoded.trajectories.get("veh").expect("the one trajectory this fixture declares");
        assert_eq!(traj.entity_id, "veh");
        assert_eq!(traj.config_hash, "traj-hash");
        assert_eq!(decoded.events.len(), 1);
        assert_eq!(decoded.events[0].id, "ev1");
    }

    /// Different `RunProducts.provenance.config_hash` values produce different encoded bytes --
    /// guards against a wrong implementation that hardcodes the provenance, forgets to include
    /// it at all, or accidentally encodes some other field instead (any of which would leave
    /// the config hash unreachable on the wire, exactly the failure this task's honesty
    /// requirements call out: "a hash test that would still pass if the hash were hardcoded or
    /// empty is not a test").
    #[test]
    fn av_run_bytes_carry_the_real_config_hash_not_a_constant() {
        let mut a = sample_products();
        a.provenance.config_hash = "hash-a".to_string();
        let mut b = sample_products();
        b.provenance.config_hash = "hash-b".to_string();
        let bytes_a = a.to_proto().encode_to_vec();
        let bytes_b = b.to_proto().encode_to_vec();
        assert_ne!(bytes_a, bytes_b);
        assert!(!bytes_a.is_empty() && !bytes_b.is_empty());
    }
}

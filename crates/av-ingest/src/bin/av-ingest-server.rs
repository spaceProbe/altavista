//! `av-ingest-server` -- boots one `altavista.v1.EdgeIngest` `tonic` server plus its
//! `GET`-only `/admin/api/evidence` HTTP surface (question 202, E3b's charter; question
//! 148's own rule that a real proof needs a real running process, not just a passing exit
//! code).
//!
//! This binary did not exist before this task: E3b's first round deliberately stopped at
//! the library (`crate::service::EdgeIngestService`) and left "wire it into a runnable
//! process" to whichever later task actually needed one -- `tests/test_edge_ingest_mtls.py`
//! (question 202's proof-through-real-nginx test) is that task, and a real nginx mTLS front
//! needs a real subprocess to sit behind, not an in-process `tonic::transport::Server`
//! spawned inside a Rust test binary (`crates/av-ingest/tests/wire_identity.rs`'s own
//! pattern, which is exactly right for proving the identity path *without* a front, but
//! cannot itself be reached by nginx across a process boundary).
//!
//! # Every knob is a command-line argument, never an environment variable
//!
//! Question 199 forbids a test mutating the calling process's own environment; the
//! established pattern this repository already uses to keep a Python test hermetic while
//! still driving Rust is a small CLI binary that reads **only** the files and values its
//! own arguments name (`av-edge-identity`'s own module doc states this rule verbatim; this
//! binary follows it for the same reason). A pytest fixture can set a subprocess's `env=`
//! freely (that is the *subprocess's* environment, not the calling pytest process's own --
//! `scripts/edge_local_ca.py`'s module doc draws this exact distinction for seccert/lego),
//! but this binary itself reads no environment variable anywhere (`std::env::args` for the
//! argument vector is not "the environment" in the sense question 199 means): every one of
//! its knobs -- the two bind addresses, the log directory, the trust anchors, the
//! Intermediate chain, the clearance ladder, the staleness budget, whether a client
//! certificate is required, and the clock -- is named on the command line specifically so
//! a test can vary each one, run to run, without ever touching `os.environ` (its own or
//! this process's).
//!
//! # The clock: fixed for deterministic tests, or real for everything else
//!
//! `--clock-tai-ns <i64>` freezes this server's `service::Clock` at exactly that value for
//! its entire lifetime (never advanced from inside this process -- a test that needs a
//! "before" and an "after" reading, e.g. a leaf that is valid then lapsed, starts **two**
//! server processes with two different `--clock-tai-ns` values rather than asking one
//! running process to jump its own clock, since this binary's configuration -- the clock
//! included -- is fixed at startup and never mutated afterward, exactly like every other
//! argument below). `--real-clock` instead reads the actual wall clock
//! (`std::time::SystemTime::now`) converted to TAI nanoseconds via
//! `av_cdm::time::Tai::from_utc_nanos` (the same UTC-to-TAI boundary
//! `crate::service::Clock`'s own doc comment names as "the platform's UTC-to-TAI
//! conversion... never inside this crate" -- this binary's `main` is exactly that one
//! sanctioned call site, not a new one). Exactly one of the two must be given; this
//! binary refuses to start otherwise rather than silently picking a default clock source
//! for a security-relevant server.
//!
//! # Printing the bound addresses
//!
//! Both `--grpc-bind`/`--admin-bind` are handed to `crate::server::bind_loopback` as
//! given (typically `127.0.0.1:0`, an ephemeral port -- never a fixed one, matching every
//! other test in this workspace); the OS-assigned addresses are then printed to **stdout**
//! as `GRPC_LISTENING <addr>` / `ADMIN_LISTENING <addr>`, each its own line, flushed
//! before either server starts accepting, so a test driving this binary as a subprocess
//! can read the two lines back instead of guessing a port or re-implementing this
//! process's own bind logic a second time.
//!
//! # `--verify-key`: the no-certificate-in-the-loop identity path, on the command line
//!
//! `EdgeIngestConfig::verify_keys` (`crate::service`) is only ever consulted when
//! `require_client_certificate` is `false` (E1's own no-certificate-in-the-loop path,
//! already exercised in-process by `tests/plugin_wire.rs` and `tests/wire_evidence.rs`),
//! but until E4b nothing populated it for a real *subprocess* of this binary -- every
//! existing caller either built `EdgeIngestService` directly (those two test files) or ran
//! with `--require-client-cert` (`tests/test_edge_ingest_mtls.py`, through the nginx mTLS
//! front). E4b's own container network-posture proof needs a real `av-edge-plugin`
//! subprocess talking plaintext to a real `av-ingest-server` subprocess with no certificate
//! anywhere in the loop (the identity/mTLS plane is E2/E3b's already-proven concern, not
//! what a container network-isolation test is about) -- so this flag closes that one gap.
//! `--verify-key <producer_id>:<path-to-EC-public-key-PEM>` (repeatable) loads each file
//! with `av_edge::verify::load_verifying_key` (accepts a bare EC public key PEM or an X.509
//! certificate, same as every other caller of that function) and inserts it into
//! `EdgeIngestConfig.verify_keys` keyed by `producer_id`. Meaningless (and refused, not
//! silently ignored) when `--require-client-cert` is in effect, since that path never
//! consults `verify_keys` at all -- see `EdgeIngestConfig::require_client_certificate`'s
//! own doc comment.
use std::collections::HashMap;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use av_edge::identity::TrustAnchors;
use av_edge::verify::load_verifying_key;
use av_ingest::admin::AdminState;
use av_ingest::pb::edge_ingest_server::EdgeIngestServer;
use av_ingest::server::bind_loopback;
use av_ingest::service::{EdgeIngestConfig, EdgeIngestService};

#[derive(Debug)]
enum ClockArg {
    Fixed(i64),
    Real,
}

#[derive(Debug)]
struct Args {
    grpc_bind: String,
    admin_bind: String,
    log_dir: PathBuf,
    trust_anchors: Vec<PathBuf>,
    intermediate_chain: Option<PathBuf>,
    clearance_ladder: Vec<String>,
    max_batch_age_ns: i64,
    require_client_cert: bool,
    verify_keys: Vec<(String, PathBuf)>,
    clock: ClockArg,
}

const USAGE: &str = "usage: av-ingest-server \
    --grpc-bind ADDR --admin-bind ADDR --log-dir PATH \
    [--trust-anchor PATH]... [--intermediate-chain PATH] \
    --clearance-ladder A,B,C --max-batch-age-ns N \
    [--require-client-cert | --no-require-client-cert] \
    [--verify-key PRODUCER_ID:PATH]... \
    (--clock-tai-ns N | --real-clock)";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();

    let mut grpc_bind = None;
    let mut admin_bind = None;
    let mut log_dir = None;
    let mut trust_anchors = Vec::new();
    let mut intermediate_chain = None;
    let mut clearance_ladder = None;
    let mut max_batch_age_ns = None;
    let mut require_client_cert = None;
    let mut verify_keys: Vec<(String, PathBuf)> = Vec::new();
    let mut clock: Option<ClockArg> = None;

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--grpc-bind" => grpc_bind = Some(value()?),
            "--admin-bind" => admin_bind = Some(value()?),
            "--log-dir" => log_dir = Some(PathBuf::from(value()?)),
            "--trust-anchor" => trust_anchors.push(PathBuf::from(value()?)),
            "--intermediate-chain" => intermediate_chain = Some(PathBuf::from(value()?)),
            "--verify-key" => {
                let raw = value()?;
                let (producer_id, path) = raw.split_once(':').ok_or_else(|| format!("--verify-key {raw:?} must have the shape PRODUCER_ID:PATH (a literal ':' separates them)"))?;
                if producer_id.is_empty() {
                    return Err(format!("--verify-key {raw:?}: PRODUCER_ID must not be empty"));
                }
                verify_keys.push((producer_id.to_string(), PathBuf::from(path)));
            }
            "--clearance-ladder" => {
                let raw = value()?;
                clearance_ladder = Some(raw.split(',').map(|s| s.to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>());
            }
            "--max-batch-age-ns" => {
                let raw = value()?;
                max_batch_age_ns = Some(raw.parse::<i64>().map_err(|e| format!("--max-batch-age-ns {raw:?} is not a valid i64: {e}"))?);
            }
            "--require-client-cert" => require_client_cert = Some(true),
            "--no-require-client-cert" => require_client_cert = Some(false),
            "--clock-tai-ns" => {
                if clock.is_some() {
                    return Err("--clock-tai-ns and --real-clock are mutually exclusive".to_string());
                }
                let raw = value()?;
                clock = Some(ClockArg::Fixed(raw.parse::<i64>().map_err(|e| format!("--clock-tai-ns {raw:?} is not a valid i64: {e}"))?));
            }
            "--real-clock" => {
                if clock.is_some() {
                    return Err("--clock-tai-ns and --real-clock are mutually exclusive".to_string());
                }
                clock = Some(ClockArg::Real);
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }

    let require_client_cert = require_client_cert.unwrap_or(true); // EdgeIngestConfig::new's own safe default -- see its doc comment.
    let trust_anchors_final = trust_anchors;
    if require_client_cert && trust_anchors_final.is_empty() {
        return Err("at least one --trust-anchor PATH is required when client certificates are required (pass --no-require-client-cert to run without any -- see EdgeIngestConfig::require_client_certificate's own doc comment on why that is not the default)".to_string());
    }
    if require_client_cert && !verify_keys.is_empty() {
        return Err("--verify-key was given but client certificates are required (--require-client-cert, the default) -- EdgeIngestConfig::verify_keys is never consulted on that path, so this is refused rather than silently ignored; pass --no-require-client-cert for the no-certificate-in-the-loop path --verify-key exists for".to_string());
    }

    Ok(Args {
        grpc_bind: grpc_bind.ok_or("--grpc-bind is required")?,
        admin_bind: admin_bind.ok_or("--admin-bind is required")?,
        log_dir: log_dir.ok_or("--log-dir is required")?,
        trust_anchors: trust_anchors_final,
        intermediate_chain,
        clearance_ladder: clearance_ladder.ok_or("--clearance-ladder is required (comma-separated, lowest rung first)")?,
        max_batch_age_ns: max_batch_age_ns.ok_or("--max-batch-age-ns is required")?,
        require_client_cert,
        verify_keys,
        clock: clock.ok_or_else(|| format!("exactly one of --clock-tai-ns or --real-clock is required\n{USAGE}"))?,
    })
}

/// The one sanctioned live-clock read site named by `crate::service::Clock`'s own doc
/// comment: `SystemTime::now()` -> Unix nanoseconds -> `av_cdm::time::Tai`. Never called
/// from anywhere inside the `av-edge`/`av-ingest` libraries themselves (question 199) --
/// only from this binary's own `main`, and only when `--real-clock` was explicitly asked
/// for.
fn read_real_clock_tai_ns() -> i64 {
    let unix_duration = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock is set before the Unix epoch");
    let unix_ns = i64::try_from(unix_duration.as_nanos()).expect("system clock is implausibly far in the future to fit in an i64 nanosecond count");
    av_cdm::time::Tai::from_utc_nanos(unix_ns).as_nanos()
}

fn run() -> Result<Args, String> {
    parse_args(std::env::args())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match run() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-ingest-server: {e}");
            return ExitCode::FAILURE;
        }
    };

    match main_inner(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("av-ingest-server: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn main_inner(args: Args) -> Result<(), String> {
    let clock: av_ingest::service::Clock = match args.clock {
        ClockArg::Fixed(v) => Arc::new(move || v),
        ClockArg::Real => Arc::new(read_real_clock_tai_ns),
    };

    let mut config = EdgeIngestConfig::new(args.clearance_ladder, args.max_batch_age_ns);
    config.require_client_certificate = args.require_client_cert;
    if let Some(path) = &args.intermediate_chain {
        let bytes = std::fs::read(path).map_err(|e| format!("reading --intermediate-chain {path:?}: {e}"))?;
        config.intermediate_chain_pem = Some(bytes);
    }

    // --verify-key PRODUCER_ID:PATH (repeatable) -- see this binary's own module doc,
    // "The no-certificate-in-the-loop identity path, on the command line". parse_args
    // already refused this combined with --require-client-cert, so every entry here is
    // meaningful.
    let mut verify_keys: HashMap<String, openssl::ec::EcKey<openssl::pkey::Public>> = HashMap::new();
    for (producer_id, path) in &args.verify_keys {
        let pem = std::fs::read(path).map_err(|e| format!("reading --verify-key {producer_id}:{path:?}: {e}"))?;
        let key = load_verifying_key(&pem).map_err(|e| format!("--verify-key {producer_id}:{path:?}: {e}"))?;
        verify_keys.insert(producer_id.clone(), key);
    }
    config.verify_keys = verify_keys;

    // Every trust-anchor PEM file is read up front and kept alive for the rest of this
    // function (`TrustAnchors::from_pems` borrows `&[&[u8]]`) -- this binary itself never
    // re-reads or reloads these files after startup.
    let trust_anchor_bytes: Vec<Vec<u8>> = args
        .trust_anchors
        .iter()
        .map(|path| std::fs::read(path).map_err(|e| format!("reading --trust-anchor {path:?}: {e}")))
        .collect::<Result<_, _>>()?;
    let anchors = if trust_anchor_bytes.is_empty() {
        None
    } else {
        let refs: Vec<&[u8]> = trust_anchor_bytes.iter().map(|b| b.as_slice()).collect();
        Some(TrustAnchors::from_pems(&refs).map_err(|e| format!("loading --trust-anchor PEM(s): {e}"))?)
    };

    let service = Arc::new(EdgeIngestService::new(args.log_dir, anchors, config, clock));

    let grpc_listener = bind_loopback(&args.grpc_bind).await.map_err(|e| format!("--grpc-bind {:?}: {e}", args.grpc_bind))?;
    let grpc_addr: SocketAddr = grpc_listener.local_addr().map_err(|e| format!("reading the bound gRPC listener's local_addr: {e}"))?;

    let admin_listener = bind_loopback(&args.admin_bind).await.map_err(|e| format!("--admin-bind {:?}: {e}", args.admin_bind))?;
    let admin_addr: SocketAddr = admin_listener.local_addr().map_err(|e| format!("reading the bound admin listener's local_addr: {e}"))?;

    // Printed BEFORE either server starts accepting, and flushed explicitly: a test
    // reading this subprocess's stdout line-by-line must never race this binary's own
    // buffering.
    println!("GRPC_LISTENING {grpc_addr}");
    println!("ADMIN_LISTENING {admin_addr}");
    std::io::stdout().flush().map_err(|e| format!("flushing stdout: {e}"))?;

    let admin_state = Arc::new(AdminState { ingest: service.ingest_handle() });
    let admin_task = tokio::spawn(async move {
        if let Err(e) = av_ingest::admin::serve_on(admin_listener, admin_state).await {
            eprintln!("av-ingest-server: admin server failed: {e}");
        }
    });

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
    let grpc_result = tonic::transport::Server::builder().add_service(EdgeIngestServer::from_arc(service)).serve_with_incoming(incoming).await;

    admin_task.abort();
    grpc_result.map_err(|e| format!("gRPC server failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_requires_exactly_one_clock_source() {
        let err = parse_args(
            ["av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x", "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1", "--no-require-client-cert"]
                .into_iter()
                .map(String::from),
        )
        .unwrap_err();
        assert!(err.contains("--clock-tai-ns or --real-clock"), "{err}");
    }

    #[test]
    fn parse_args_refuses_both_clock_sources_at_once() {
        let err = parse_args(
            ["av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x", "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1", "--no-require-client-cert", "--clock-tai-ns", "1", "--real-clock"]
                .into_iter()
                .map(String::from),
        )
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn parse_args_requires_a_trust_anchor_when_client_certs_are_required() {
        let err = parse_args(
            ["av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x", "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1", "--clock-tai-ns", "1"]
                .into_iter()
                .map(String::from),
        )
        .unwrap_err();
        assert!(err.contains("--trust-anchor"), "{err}");
    }

    #[test]
    fn parse_args_accepts_the_full_minimal_shape() {
        let args = parse_args(
            [
                "av-ingest-server",
                "--grpc-bind",
                "127.0.0.1:0",
                "--admin-bind",
                "127.0.0.1:0",
                "--log-dir",
                "/tmp/x",
                "--trust-anchor",
                "/tmp/root.pem",
                "--clearance-ladder",
                "UNCLASSIFIED,CUI",
                "--max-batch-age-ns",
                "5000000000",
                "--clock-tai-ns",
                "1800000037000000000",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(args.clearance_ladder, vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]);
        assert!(args.require_client_cert);
        assert!(matches!(args.clock, ClockArg::Fixed(1_800_000_037_000_000_000)));
    }

    #[test]
    fn parse_args_accepts_repeated_verify_key_with_no_require_client_cert() {
        let args = parse_args(
            [
                "av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x",
                "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1", "--no-require-client-cert",
                "--verify-key", "producer-a:/tmp/a.pub.pem", "--verify-key", "producer-b:/tmp/b.pub.pem",
                "--clock-tai-ns", "1",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap();
        assert_eq!(args.verify_keys, vec![("producer-a".to_string(), PathBuf::from("/tmp/a.pub.pem")), ("producer-b".to_string(), PathBuf::from("/tmp/b.pub.pem"))]);
    }

    #[test]
    fn parse_args_refuses_a_verify_key_with_no_colon() {
        let err = parse_args(
            [
                "av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x",
                "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1", "--no-require-client-cert",
                "--verify-key", "no-colon-here",
                "--clock-tai-ns", "1",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap_err();
        assert!(err.contains("PRODUCER_ID:PATH"), "{err}");
    }

    #[test]
    fn parse_args_refuses_verify_key_combined_with_require_client_cert() {
        let err = parse_args(
            [
                "av-ingest-server", "--grpc-bind", "127.0.0.1:0", "--admin-bind", "127.0.0.1:0", "--log-dir", "/tmp/x",
                "--trust-anchor", "/tmp/root.pem", "--clearance-ladder", "UNCLASSIFIED", "--max-batch-age-ns", "1",
                "--verify-key", "producer-a:/tmp/a.pub.pem",
                "--clock-tai-ns", "1",
            ]
            .into_iter()
            .map(String::from),
        )
        .unwrap_err();
        assert!(err.contains("--verify-key"), "{err}");
        assert!(err.contains("never consulted"), "{err}");
    }
}

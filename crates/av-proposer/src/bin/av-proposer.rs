//! `av-proposer` -- D5's binary. One run = connect to the gateway, issue the queries,
//! evaluate the rule, and either propose exactly one command or print a typed "no proposal,
//! and why" line and exit 0. Every knob is a command-line argument (question 199's rule,
//! `crates/av-ingest-client/src/bin/av-edge-plugin.rs`'s own precedent, copied here for the
//! CLI-parsing shape and the machine-readable stdout-summary convention) -- never an
//! environment variable, never a hardcoded magic number.
//!
//! Two modes, mutually exclusive:
//! - The default (propose-only) mode: everything below `--serve-model-service`'s own
//!   description. Opens NO listening socket at all.
//! - `--serve-model-service [addr]`: serves spoore's `ModelService` (D2) on a real loopback
//!   socket, printing `MODEL_SERVICE_LISTENING <addr>` (flushed) before accepting, mirroring
//!   `crates/av-ingest/src/bin/av-ingest-server.rs`'s own `GRPC_LISTENING` convention -- the
//!   other named precedent this task's brief points at. No propose-run happens in this mode.
//!   `addr` is optional (question 219(a)): omitted, or immediately followed by another `--`
//!   flag, it defaults to `DEFAULT_MODEL_SERVICE_BIND`, `av-proposer`'s own row in
//!   `docs/architecture.md` section 4's "Default ports" table; given, it is used verbatim
//!   (the existing, unchanged behaviour). See `parse_args` for the peek rule.

use std::process::ExitCode;

use av_command::counters::Counters;
use av_proposer::gateway_client::GatewayClient;
use av_proposer::model_service::{ModelIdentity, ModelServiceImpl};
use av_proposer::model_service_pb::model_service_server::ModelServiceServer;
use av_proposer::proposer::{run, ProposerConfig, RunOutcome};
use av_proposer::rule::RuleConfig;

/// Question 208(c)/219(a): `docs/architecture.md` section 4, "Default ports", is the one owned
/// port map for every service's default bind in this workspace -- this constant is
/// `av-proposer`'s own entry, the address `--serve-model-service` binds spoore's `ModelService`
/// (D2) to when no explicit address is given (see `parse_args` below). `av-proposer` has no
/// admin surface, so there is no `+100` admin counterpart, the same shape as
/// `crates/av-lockstep-shim/src/bin/av-lockstep-shim.rs::DEFAULT_GRPC_ADDR`.
const DEFAULT_MODEL_SERVICE_BIND: &str = "127.0.0.1:50063";

struct ProposeArgs {
    gateway_endpoint: String,
    run_id: String,
    config_hash: String,
    caller_clearance: String,
    entity_id: String,
    command_class: String,
    score_name: String,
    reference_radius_m: f64,
    threshold_m: f64,
    gain_per_s: f64,
    max_burn_mps: f64,
    model_node_id: String,
    model_version: String,
    /// R5.1/question 208(b): this proposer's service token (a compact-serialization JWS),
    /// read from `--service-token-file <PATH>` (REQUIRED) -- never `--service-token <VALUE>`:
    /// a flag VALUE is visible in `ps` and in `docker inspect`, and this task's own invariant
    /// G forbids the token from ever reaching either.
    service_token: String,
}

struct ServeModelServiceArgs {
    bind: String,
    model_node_id: String,
    model_version: String,
    process_noise_density: f64,
    sensor_id: String,
}

enum Mode {
    Propose(ProposeArgs),
    ServeModelService(ServeModelServiceArgs),
}

const USAGE: &str = "usage:\n  av-proposer --gateway-endpoint URL --run-id ID --caller-clearance MARKING \\\n    --entity-id ID --command-class CLASS --score-name NAME \\\n    --reference-radius-m N --threshold-m N --gain-per-s N --max-burn-mps N \\\n    --model-node-id ID --model-version VERSION --service-token-file PATH [--config-hash HASH]\n  av-proposer --serve-model-service [HOST:PORT] --model-node-id ID --model-version VERSION \\\n    --process-noise-density N --sensor-id ID\n  (--serve-model-service's value is optional: omitted, or immediately followed by another\n   --flag, it defaults to 127.0.0.1:50063 -- av-proposer's row in docs/architecture.md\n   section 4's \"Default ports\" table; given, that address is used verbatim.)";

fn parse_args(args: impl Iterator<Item = String>) -> Result<Mode, String> {
    // Peekable so `--serve-model-service` can look at, without yet consuming, the next token
    // before deciding whether it is that flag's own (optional) value or the next flag --
    // question 219(a)'s rule, implemented as a minimal change that leaves every other flag's
    // parsing (all of which still call plain `args.next()`) byte for byte the same.
    let mut args = args.peekable();
    let _argv0 = args.next();
    let mut serve_model_service: Option<String> = None;
    let (mut gateway_endpoint, mut run_id, mut config_hash, mut caller_clearance, mut entity_id, mut command_class) = (None, None, None, None::<String>, None, None);
    let (mut score_name, mut reference_radius_m, mut threshold_m, mut gain_per_s, mut max_burn_mps) = (None, None, None, None, None);
    let (mut model_node_id, mut model_version, mut process_noise_density, mut sensor_id) = (None, None, None, None);
    let mut service_token_file: Option<String> = None;

    while let Some(flag) = args.next() {
        if flag == "--serve-model-service" {
            // Question 219(a): the next token is this flag's value ONLY if it is present and
            // does not itself begin with `--` -- otherwise (absent, or the next flag) this
            // flag takes `DEFAULT_MODEL_SERVICE_BIND` and, in the "next flag" case, that token
            // is left in the iterator for the loop's next turn to parse normally (this is what
            // makes the peek load-bearing: a plain `args.next()` here would have swallowed it).
            let bind = match args.peek() {
                Some(next) if !next.starts_with("--") => args.next().expect("peeked Some"),
                _ => DEFAULT_MODEL_SERVICE_BIND.to_string(),
            };
            serve_model_service = Some(bind);
            continue;
        }
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--gateway-endpoint" => gateway_endpoint = Some(value()?),
            "--run-id" => run_id = Some(value()?),
            "--config-hash" => config_hash = Some(value()?),
            "--caller-clearance" => caller_clearance = Some(value()?),
            "--entity-id" => entity_id = Some(value()?),
            "--command-class" => command_class = Some(value()?),
            "--score-name" => score_name = Some(value()?),
            "--reference-radius-m" => reference_radius_m = Some(value()?.parse::<f64>().map_err(|e| format!("--reference-radius-m: {e}"))?),
            "--threshold-m" => threshold_m = Some(value()?.parse::<f64>().map_err(|e| format!("--threshold-m: {e}"))?),
            "--gain-per-s" => gain_per_s = Some(value()?.parse::<f64>().map_err(|e| format!("--gain-per-s: {e}"))?),
            "--max-burn-mps" => max_burn_mps = Some(value()?.parse::<f64>().map_err(|e| format!("--max-burn-mps: {e}"))?),
            "--model-node-id" => model_node_id = Some(value()?),
            "--model-version" => model_version = Some(value()?),
            "--process-noise-density" => process_noise_density = Some(value()?.parse::<f64>().map_err(|e| format!("--process-noise-density: {e}"))?),
            "--sensor-id" => sensor_id = Some(value()?),
            "--service-token-file" => service_token_file = Some(value()?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }

    if let Some(bind) = serve_model_service {
        return Ok(Mode::ServeModelService(ServeModelServiceArgs {
            bind,
            model_node_id: model_node_id.ok_or("--model-node-id is required")?,
            model_version: model_version.ok_or("--model-version is required")?,
            process_noise_density: process_noise_density.ok_or("--process-noise-density is required")?,
            sensor_id: sensor_id.ok_or("--sensor-id is required")?,
        }));
    }

    // R5.1/question 208(b): required -- an absent token file must make this binary fail with a
    // typed error, never proceed unauthenticated (invariant B). Read here, once, at startup --
    // `crate::proposer::run`'s own module doc already states "every knob is a command-line
    // argument, never an environment variable" (question 199); a FILE PATH is still a
    // command-line argument, only the token's own bytes come from disk, exactly like `av-
    // command --oidc-public-key-path` already does for the issuer's public key.
    let service_token_file = service_token_file.ok_or("--service-token-file is required (R5.1: this proposer's service token, read from a file, never a --service-token VALUE)")?;
    let service_token = std::fs::read_to_string(&service_token_file)
        .map_err(|e| format!("--service-token-file {service_token_file:?}: {e}"))?
        .trim()
        .to_string();
    if service_token.is_empty() {
        return Err(format!("--service-token-file {service_token_file:?} is empty -- a proposer with no token must never proceed unauthenticated"));
    }

    Ok(Mode::Propose(ProposeArgs {
        gateway_endpoint: gateway_endpoint.ok_or("--gateway-endpoint is required")?,
        run_id: run_id.ok_or("--run-id is required")?,
        config_hash: config_hash.unwrap_or_default(),
        caller_clearance: caller_clearance.ok_or("--caller-clearance is required")?,
        entity_id: entity_id.ok_or("--entity-id is required")?,
        command_class: command_class.ok_or("--command-class is required")?,
        score_name: score_name.ok_or("--score-name is required")?,
        reference_radius_m: reference_radius_m.ok_or("--reference-radius-m is required")?,
        threshold_m: threshold_m.ok_or("--threshold-m is required")?,
        gain_per_s: gain_per_s.ok_or("--gain-per-s is required")?,
        max_burn_mps: max_burn_mps.ok_or("--max-burn-mps is required")?,
        model_node_id: model_node_id.ok_or("--model-node-id is required")?,
        model_version: model_version.ok_or("--model-version is required")?,
        service_token,
    }))
}

async fn run_propose(args: ProposeArgs) -> Result<serde_json::Value, String> {
    let mut client = GatewayClient::connect(args.gateway_endpoint.clone()).await.map_err(|e| format!("{e}"))?;
    let counters = Counters::new();
    let config = ProposerConfig {
        run_id: args.run_id,
        config_hash: args.config_hash,
        caller_clearance: args.caller_clearance,
        entity_id: args.entity_id,
        command_class: args.command_class,
        rule: RuleConfig { score_name: args.score_name, reference_radius_m: args.reference_radius_m, threshold_m: args.threshold_m, gain_per_s: args.gain_per_s, max_burn_mps: args.max_burn_mps },
        model: ModelIdentity { node_id: args.model_node_id, version: args.model_version },
        rationale_prefix: "av-proposer station-keeping rule".to_string(),
        service_token: args.service_token,
    };

    match run(&mut client, &config, &counters).await {
        Ok(RunOutcome::Proposed { command_id, idempotency_key, drift_m, burn_mps }) => Ok(serde_json::json!({
            "outcome": "PROPOSED",
            "command_id": command_id,
            "idempotency_key": idempotency_key,
            "drift_m": drift_m,
            "burn_mps": burn_mps,
        })),
        Ok(RunOutcome::NoProposalNeeded { drift_m }) => Ok(serde_json::json!({
            "outcome": "NO_PROPOSAL_NEEDED",
            "drift_m": drift_m,
            "threshold_m": config.rule.threshold_m,
        })),
        Err(refusal) => Err(format!("{refusal}")),
    }
}

async fn run_serve_model_service(args: ServeModelServiceArgs) -> Result<(), String> {
    use std::io::Write as _;

    let identity = ModelIdentity { node_id: args.model_node_id, version: args.model_version };
    let service = ModelServiceImpl::new(identity, args.process_noise_density, args.sensor_id).map_err(|e| format!("{e}"))?;

    let listener = tokio::net::TcpListener::bind(&args.bind).await.map_err(|e| format!("--serve-model-service {:?}: {e}", args.bind))?;
    let addr = listener.local_addr().map_err(|e| format!("reading the bound listener's local_addr: {e}"))?;
    println!("MODEL_SERVICE_LISTENING {addr}");
    std::io::stdout().flush().map_err(|e| format!("flushing stdout: {e}"))?;

    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tonic::transport::Server::builder()
        .add_service(ModelServiceServer::new(service))
        .serve_with_incoming(incoming)
        .await
        .map_err(|e| format!("ModelService server exited: {e}"))
}

#[tokio::main]
async fn main() -> ExitCode {
    let mode = match parse_args(std::env::args()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("av-proposer: {e}");
            return ExitCode::FAILURE;
        }
    };

    match mode {
        Mode::Propose(args) => match run_propose(args).await {
            Ok(summary) => {
                println!("{summary}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("av-proposer: {e}");
                ExitCode::FAILURE
            }
        },
        Mode::ServeModelService(args) => match run_serve_model_service(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("av-proposer: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    //! Question 219(a): unit tests for `--serve-model-service`'s optional-value rule
    //! (`parse_args`'s `Peekable` peek, above). A `#[cfg(test)] mod` in a bin target is
    //! compiled into that binary's own test harness, so `cargo test -p av-proposer` runs
    //! these with no separate test binary needed.
    use super::*;

    /// Builds the `args` iterator `parse_args` expects: argv[0] (discarded by `parse_args`
    /// itself, mirroring `std::env::args()`), then the given flags/values.
    fn argv<'a>(tail: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        std::iter::once("av-proposer".to_string()).chain(tail.iter().map(|s| s.to_string()))
    }

    /// A `--service-token-file`-able temp file, written once per instance and removed on
    /// `Drop` (success or panic) so a failing assertion still leaves no litter behind. Plain
    /// file I/O only -- nothing here touches the process environment (question 199 forbids
    /// `std::env::set_var` in tests; this needs it for nothing).
    struct TempTokenFile {
        path: std::path::PathBuf,
    }

    impl TempTokenFile {
        fn new(token: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "av-proposer-test-token-{}-{n}-{nanos}.txt",
                std::process::id(),
            ));
            std::fs::write(&path, token).expect("writing temp service-token file");
            Self { path }
        }

        fn path_str(&self) -> String {
            self.path.to_str().expect("temp path is valid UTF-8").to_string()
        }
    }

    impl Drop for TempTokenFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn propose_flags(token_path: &str) -> Vec<String> {
        [
            "--gateway-endpoint", "http://127.0.0.1:0",
            "--run-id", "run-1",
            "--caller-clearance", "UNCLASSIFIED",
            "--entity-id", "entity-1",
            "--command-class", "station-keeping",
            "--score-name", "drift_m",
            "--reference-radius-m", "1.0",
            "--threshold-m", "1.0",
            "--gain-per-s", "0.1",
            "--max-burn-mps", "0.01",
            "--model-node-id", "node-1",
            "--model-version", "v1",
            "--service-token-file", token_path,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn serve_model_service_with_no_following_token_defaults() {
        let args = argv(&[
            "--model-node-id", "node-1",
            "--model-version", "v1",
            "--process-noise-density", "0.001",
            "--sensor-id", "sensor-1",
            "--serve-model-service",
        ]);
        let mode = parse_args(args).expect("parses");
        match mode {
            Mode::ServeModelService(a) => assert_eq!(a.bind, DEFAULT_MODEL_SERVICE_BIND),
            Mode::Propose(_) => panic!("expected Mode::ServeModelService"),
        }
    }

    #[test]
    fn serve_model_service_with_explicit_address_is_unchanged() {
        let args = argv(&[
            "--serve-model-service", "127.0.0.1:0",
            "--model-node-id", "node-1",
            "--model-version", "v1",
            "--process-noise-density", "0.001",
            "--sensor-id", "sensor-1",
        ]);
        let mode = parse_args(args).expect("parses");
        match mode {
            Mode::ServeModelService(a) => assert_eq!(a.bind, "127.0.0.1:0"),
            Mode::Propose(_) => panic!("expected Mode::ServeModelService"),
        }
    }

    #[test]
    fn serve_model_service_immediately_followed_by_another_flag_defaults_and_still_parses_it() {
        // The load-bearing case: `--serve-model-service` has no value token of its own here --
        // the very next thing on the line is `--model-node-id`, a different flag. A plain
        // `args.next()` (the pre-219(a) behaviour) would have swallowed "--model-node-id" as
        // this flag's bind address, corrupting both. The peek rule must default the bind AND
        // leave "--model-node-id" for the loop's next turn to parse normally.
        let args = argv(&[
            "--serve-model-service",
            "--model-node-id", "node-1",
            "--model-version", "v1",
            "--process-noise-density", "0.001",
            "--sensor-id", "sensor-1",
        ]);
        let mode = parse_args(args).expect("parses");
        match mode {
            Mode::ServeModelService(a) => {
                assert_eq!(a.bind, DEFAULT_MODEL_SERVICE_BIND);
                assert_eq!(a.model_node_id, "node-1");
            }
            Mode::Propose(_) => panic!("expected Mode::ServeModelService"),
        }
    }

    #[test]
    fn propose_mode_without_serve_model_service_is_unaffected() {
        let token_file = TempTokenFile::new("test-service-token");
        let flags = propose_flags(&token_file.path_str());
        let flag_refs: Vec<&str> = flags.iter().map(String::as_str).collect();
        let args = argv(&flag_refs);
        let mode = parse_args(args).expect("parses");
        match mode {
            Mode::Propose(a) => {
                assert_eq!(a.gateway_endpoint, "http://127.0.0.1:0");
                assert_eq!(a.service_token, "test-service-token");
            }
            Mode::ServeModelService(_) => panic!("expected Mode::Propose"),
        }
    }
}

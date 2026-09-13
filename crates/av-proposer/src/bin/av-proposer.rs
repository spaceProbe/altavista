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
//! - `--serve-model-service <addr>`: serves spoore's `ModelService` (D2) on a real loopback
//!   socket, printing `MODEL_SERVICE_LISTENING <addr>` (flushed) before accepting, mirroring
//!   `crates/av-ingest/src/bin/av-ingest-server.rs`'s own `GRPC_LISTENING` convention -- the
//!   other named precedent this task's brief points at. No propose-run happens in this mode.

use std::process::ExitCode;

use av_command::counters::Counters;
use av_proposer::gateway_client::GatewayClient;
use av_proposer::model_service::{ModelIdentity, ModelServiceImpl};
use av_proposer::model_service_pb::model_service_server::ModelServiceServer;
use av_proposer::proposer::{run, ProposerConfig, RunOutcome};
use av_proposer::rule::RuleConfig;

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

const USAGE: &str = "usage:\n  av-proposer --gateway-endpoint URL --run-id ID --caller-clearance MARKING \\\n    --entity-id ID --command-class CLASS --score-name NAME \\\n    --reference-radius-m N --threshold-m N --gain-per-s N --max-burn-mps N \\\n    --model-node-id ID --model-version VERSION [--config-hash HASH]\n  av-proposer --serve-model-service HOST:PORT --model-node-id ID --model-version VERSION \\\n    --process-noise-density N --sensor-id ID";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Mode, String> {
    let _argv0 = args.next();
    let mut serve_model_service: Option<String> = None;
    let (mut gateway_endpoint, mut run_id, mut config_hash, mut caller_clearance, mut entity_id, mut command_class) = (None, None, None, None::<String>, None, None);
    let (mut score_name, mut reference_radius_m, mut threshold_m, mut gain_per_s, mut max_burn_mps) = (None, None, None, None, None);
    let (mut model_node_id, mut model_version, mut process_noise_density, mut sensor_id) = (None, None, None, None);

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--serve-model-service" => serve_model_service = Some(value()?),
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

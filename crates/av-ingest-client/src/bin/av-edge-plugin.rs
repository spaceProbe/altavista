//! `av-edge-plugin` -- E4a's plugin binary: replays a kernel run's simulated-asset
//! telemetry (`av_edge::plugin`) as signed, chained `MeasurementBatch`es against a real
//! `EdgeIngest` server (`docs/edge-plan.md` milestone E4).
//!
//! # Why this binary lives here, not on `crates/av-edge`
//!
//! `crates/av-edge`'s own task brief is explicit: it "must not grow a transport
//! dependency" -- it is a pure library the whole edge track depends on, and this binary
//! needs a real network client (`av-ingest-client`'s `EdgeIngestClient` for the plaintext-
//! loopback path, and `av-grpc::tls::connect` -- a `tonic`/OpenSSL transport stack -- for
//! the mTLS-through-nginx path). Putting it under `crates/av-edge/src/bin/` would pull
//! both into `av-edge`'s own dependency tree the moment that crate's `[[bin]]` target is
//! built, which is exactly the outcome the brief rules out.
//!
//! A brand-new `crates/av-edge-plugin` crate was the other option considered: it would
//! avoid nothing this crate does not already avoid (a fresh crate would *still* need to
//! depend on both `av-edge` and `av-ingest-client` to do its job, so it gains nothing
//! dependency-wise over adding one more `[[bin]]` to an existing crate that already
//! depends on both), while adding a new `Cargo.toml`, a new workspace member, and a new
//! line in every gate command this track's manager already runs by name
//! (`cargo test -p av-edge -p av-ingest -p av-ingest-client`) for no benefit.
//!
//! `crates/av-ingest-client` is therefore the right home: it already depends on `av-edge`
//! (for `pb`/`sign`/`hash`/`plugin`) *and* on `av-grpc` (for the OpenSSL mTLS connector --
//! see that crate's own `Cargo.toml` comment on `src/bin/av-ingest-mtls-client.rs`, this
//! binary's direct sibling and the precedent this file follows for the mTLS path), and it
//! already carries `serde_json` for exactly this kind of machine-readable stdout summary.
//! Adding this binary here is one more `[[bin]]` target on a crate that already has the
//! complete dependency set this task needs, not a new dependency edge anywhere.
//!
//! # Command-line configured, never environment-configured (question 199)
//!
//! Every value this binary's behaviour depends on -- which run to replay, which endpoint
//! to dial, which key to sign with, how to pace -- comes from an explicit `--flag`, never
//! from `std::env::var`. `--plugin-config` names a single JSON file (deserialised
//! directly as `av_edge::plugin::PluginConfig`, which already derives `Deserialize` for
//! exactly this reason) rather than dozens of individual flags for that struct's sixteen
//! fields -- the file's *path* is still a command-line argument, so this is "configured on
//! the command line," not "configured by the environment," in the sense question 199's
//! rule actually cares about (no `AV_*`-style variable is ever read).
//!
//! # What it does
//!
//! 1. Loads `--run-products` (`altavista.v1.RunProducts`) and `--port-traffic` (the
//!    `PortTrafficLog` sidecar), hash-verifying the sidecar against `RunProducts.
//!    port_traffic_hash` via `av_edge::plugin::verify_port_traffic_log` before decoding a
//!    single byte of it.
//! 2. Loads `--plugin-config` (JSON) and `--signing-key` (an EC P-384 private key PEM),
//!    and validates the config (`PluginConfig::validate`).
//! 3. Builds the manifest from that same config (`PluginConfig::manifest`) and one
//!    `PortTrafficSource`/chain of signed `MeasurementBatch`es from it
//!    (`PortTrafficSource::from_log`, `BatchBuilder::build_batches_for_config`) -- the
//!    manifest and the batches are derived from the identical `PluginConfig` value, so
//!    they cannot drift apart.
//! 4. Connects: `--endpoint host:port` dials plaintext loopback
//!    (`EdgeIngestClient::connect_plaintext`); `--endpoint https://host:port` (with
//!    `--server-ca`, and optionally `--client-cert`/`--client-key`) dials mTLS through a
//!    service-owned nginx front via `av_grpc::tls::connect` -- reusing, not rewriting,
//!    `src/bin/av-ingest-mtls-client.rs`'s own connection recipe.
//! 5. Calls `Announce`, then streams every batch, honouring `--pacing` (`as-fast-as-
//!    possible`, the default, or `real-time:<scale>`) -- `av_edge::plugin::Pacing::
//!    due_at` decides *when* each batch is due (a pure function); **this binary is the
//!    only place in this whole path that ever calls `std::thread::sleep`** -- never
//!    `av-edge`, never a test.
//! 6. Prints one line of JSON to stdout: `batch_count`, `measurement_count`,
//!    `chain_head_hex` (the last batch's own `batch_hash`), and one entry per verdict
//!    (`sequence`, `accepted`, `rejection`, `detail`). Exits non-zero iff any batch was
//!    rejected, or a hard failure (bad args, a file that will not read, a connect
//!    failure) occurred -- printed to **stderr** in that case, mirroring `src/bin/
//!    av-ingest-mtls-client.rs`'s own stdout/stderr split.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use av_edge::plugin::{BatchBuilder, PluginConfig, PortTrafficSource};
use av_edge::sign;
use av_edge::{hash, pb};
use av_grpc::tls::{connect, MtlsConfig};
use av_ingest_client::pb_client::edge_ingest_client::EdgeIngestClient as RawEdgeIngestClient;
use av_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::Private;

struct Args {
    run_products: PathBuf,
    port_traffic: PathBuf,
    plugin_config: PathBuf,
    signing_key: PathBuf,
    endpoint: String,
    server_ca: Option<PathBuf>,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
}

const USAGE: &str = "usage: av-edge-plugin --run-products PATH --port-traffic PATH --plugin-config PATH \
    --signing-key PATH --endpoint (HOST:PORT | https://HOST:PORT) \
    [--server-ca PATH] [--client-cert PATH --client-key PATH]";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();
    let (mut run_products, mut port_traffic, mut plugin_config, mut signing_key, mut endpoint, mut server_ca, mut client_cert, mut client_key) = (None, None, None, None, None, None, None, None);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--run-products" => run_products = Some(PathBuf::from(value()?)),
            "--port-traffic" => port_traffic = Some(PathBuf::from(value()?)),
            "--plugin-config" => plugin_config = Some(PathBuf::from(value()?)),
            "--signing-key" => signing_key = Some(PathBuf::from(value()?)),
            "--endpoint" => endpoint = Some(value()?),
            "--server-ca" => server_ca = Some(PathBuf::from(value()?)),
            "--client-cert" => client_cert = Some(PathBuf::from(value()?)),
            "--client-key" => client_key = Some(PathBuf::from(value()?)),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }
    if client_cert.is_some() != client_key.is_some() {
        return Err("--client-cert and --client-key must be given together, or not at all".to_string());
    }
    Ok(Args {
        run_products: run_products.ok_or("--run-products is required")?,
        port_traffic: port_traffic.ok_or("--port-traffic is required")?,
        plugin_config: plugin_config.ok_or("--plugin-config is required")?,
        signing_key: signing_key.ok_or("--signing-key is required")?,
        endpoint: endpoint.ok_or("--endpoint is required")?,
        server_ca,
        client_cert,
        client_key,
    })
}

fn load_run_products(path: &std::path::Path) -> Result<pb::RunProducts, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path:?}: {e}"))?;
    <pb::RunProducts as prost::Message>::decode(bytes.as_slice()).map_err(|e| format!("{path:?} does not decode as altavista.v1.RunProducts: {e}"))
}

fn load_verified_port_traffic_log(path: &std::path::Path, expected_hash: &str) -> Result<pb::PortTrafficLog, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path:?}: {e}"))?;
    av_edge::plugin::verify_port_traffic_log(&bytes, expected_hash).map_err(|e| format!("{path:?}: {e}"))
}

fn load_plugin_config(path: &std::path::Path) -> Result<PluginConfig, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path:?}: {e}"))?;
    let cfg: PluginConfig = serde_json::from_slice(&bytes).map_err(|e| format!("{path:?} does not decode as a PluginConfig JSON document: {e}"))?;
    cfg.validate().map_err(|e| format!("{path:?}: {e}"))?;
    Ok(cfg)
}

fn load_signing_key(path: &std::path::Path) -> Result<EcKey<Private>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("reading {path:?}: {e}"))?;
    sign::load_signing_key(&pem).map_err(|e| format!("{path:?}: {e}"))
}

/// One connected client, over either transport this binary supports -- see this file's own
/// module doc for exactly which flags select which.
enum Client {
    Plaintext(EdgeIngestClient),
    Mtls(RawEdgeIngestClient<tonic::transport::Channel>),
}

impl Client {
    async fn connect(args: &Args) -> Result<Self, String> {
        if args.endpoint.starts_with("https://") {
            let server_ca = args.server_ca.as_deref().ok_or("--server-ca is required for an https:// --endpoint")?;
            let cfg = MtlsConfig { ca_file: server_ca, client_cert: args.client_cert.as_deref(), client_key: args.client_key.as_deref() };
            let channel = connect(&args.endpoint, cfg).await.map_err(|e| format!("connecting to {}: {e}", args.endpoint))?;
            Ok(Client::Mtls(RawEdgeIngestClient::new(channel)))
        } else {
            let client = EdgeIngestClient::connect_plaintext(&args.endpoint).await.map_err(|e| format!("connecting to {}: {e}", args.endpoint))?;
            Ok(Client::Plaintext(client))
        }
    }

    async fn announce(&mut self, manifest: pb::PluginManifest) -> Result<pb::ManifestAck, String> {
        match self {
            Client::Plaintext(c) => c.announce(manifest).await.map_err(|e| format!("Announce RPC failed: {e}")),
            Client::Mtls(c) => c.announce(tonic::Request::new(manifest)).await.map(tonic::Response::into_inner).map_err(|e| format!("Announce RPC failed: {e}")),
        }
    }

    /// Submits exactly the batches given, in order, collecting every returned verdict.
    /// Called once per pacing "tick" (`main`'s own loop) -- see this file's module doc for
    /// why pacing is implemented as repeated small submissions rather than a single
    /// paced stream.
    async fn submit(&mut self, batches: Vec<pb::MeasurementBatch>) -> Result<Vec<pb::BatchVerdict>, String> {
        match self {
            Client::Plaintext(c) => c.submit_batches(batches).await.map_err(|e| format!("Submit RPC failed: {e}")),
            Client::Mtls(c) => {
                let stream = futures_util::stream::iter(batches);
                let response = c.submit(tonic::Request::new(stream)).await.map_err(|e| format!("Submit RPC failed: {e}"))?;
                let mut inbound = response.into_inner();
                let mut verdicts = Vec::new();
                loop {
                    match inbound.message().await {
                        Ok(Some(v)) => verdicts.push(v),
                        Ok(None) => break,
                        Err(status) => return Err(format!("Submit stream failed mid-way: {status}")),
                    }
                }
                Ok(verdicts)
            }
        }
    }
}

async fn run(args: Args) -> Result<serde_json::Value, String> {
    let run_products = load_run_products(&args.run_products)?;
    let log = load_verified_port_traffic_log(&args.port_traffic, &run_products.port_traffic_hash)?;
    let cfg = load_plugin_config(&args.plugin_config)?;
    let signing_key = load_signing_key(&args.signing_key)?;

    let source = PortTrafficSource::from_log(&log, &cfg).map_err(|e| format!("decoding the port traffic log against --plugin-config: {e}"))?;
    let builder = BatchBuilder::new(cfg.batching).map_err(|e| format!("{e}"))?;
    // The batch provenance's own created_tai_ns ties every batch back to the run being
    // replayed, rather than to this process's own wall-clock start -- deterministic
    // across re-runs of the identical run/config/key, exactly like `crates/av-edge/tests/
    // plugin_replay.rs` and `crates/av-ingest/tests/plugin_wire.rs` both rely on.
    let created_tai_ns = run_products.provenance.as_ref().map(|p| p.created_tai_ns).unwrap_or(0);
    let provenance = cfg.batch_provenance(&run_products.run_id, created_tai_ns);
    let batches = builder.build_batches_for_config(&source, &cfg, &provenance, &signing_key).map_err(|e| format!("building/signing batches: {e}"))?;

    let measurement_count: usize = batches.iter().map(|b| b.measurements.len()).sum();

    let mut client = Client::connect(&args).await?;
    let manifest = cfg.manifest().map_err(|e| format!("{e}"))?;
    let ack = client.announce(manifest).await?;
    if !ack.accepted {
        let refusal_name = pb::ManifestRefusal::try_from(ack.refusal).map(|r| r.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown ManifestRefusal {}>", ack.refusal));
        return Err(format!("Announce was refused: {refusal_name}: {}", ack.detail));
    }

    // Pacing: av_edge::plugin::Pacing::due_at is a pure function of epochs; this loop is
    // the one place in this whole path that ever sleeps (this file's own module doc).
    let first_epoch = batches.first().map(|b| b.batch_tai_ns).unwrap_or(0);
    let start = Instant::now();
    let mut verdicts: Vec<pb::BatchVerdict> = Vec::with_capacity(batches.len());
    for (index, batch) in batches.into_iter().enumerate() {
        let due_ns = cfg.pacing.due_at(index, first_epoch, batch.batch_tai_ns);
        if due_ns > 0 {
            let due_at = start + Duration::from_nanos(due_ns as u64);
            let now = Instant::now();
            if due_at > now {
                std::thread::sleep(due_at - now);
            }
        }
        let mut one = client.submit(vec![batch]).await?;
        verdicts.append(&mut one);
    }

    let chain_head_hex = verdicts.last().map(|v| hash::hex_encode(&v.batch_hash)).unwrap_or_default();
    let any_rejected = verdicts.iter().any(|v| !v.accepted);
    let verdict_json: Vec<serde_json::Value> = verdicts
        .iter()
        .map(|v| {
            let rejection_name = pb::BatchRejection::try_from(v.rejection).map(|r| r.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown BatchRejection {}>", v.rejection));
            serde_json::json!({
                "sequence": v.sequence,
                "accepted": v.accepted,
                "rejection": rejection_name,
                "detail": v.detail,
            })
        })
        .collect();

    let summary = serde_json::json!({
        "batch_count": verdicts.len(),
        "measurement_count": measurement_count,
        "chain_head_hex": chain_head_hex,
        "any_rejected": any_rejected,
        "verdicts": verdict_json,
    });
    if any_rejected {
        // Still a valid, complete summary -- printed by main() before it returns a
        // non-zero exit code (this file's own module doc: "this binary's job is only to
        // observe and report exactly what happened").
        println!("{summary}");
        return Err("at least one batch was rejected -- see the printed summary's own verdicts".to_string());
    }
    Ok(summary)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-edge-plugin: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(args).await {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("av-edge-plugin: {e}");
            ExitCode::FAILURE
        }
    }
}

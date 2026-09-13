//! `av-ingest-mtls-client` -- the one piece of scope `crates/av-ingest-client`'s own
//! module doc names but does not itself implement: a real mTLS `tonic` client that dials
//! **through** a service-owned nginx front (question 202's own proof,
//! `tests/test_edge_ingest_mtls.py`), rather than `EdgeIngestClient::connect_plaintext`'s
//! deliberately loopback-only, no-TLS-at-all path.
//!
//! # Why this binary, and why it lives in this crate
//!
//! `crates/av-ingest-client`'s own `lib.rs` module doc says plainly: "this client
//! intentionally offers no `connect_mtls` yet, since nothing in this task's own scope
//! calls one... `crates/av-grpc::tls::connect` is already exactly the right building
//! block for it when that task needs one." This task is that task. Rather than teaching
//! `EdgeIngestClient` itself a second connection mode, this binary reaches straight past
//! that wrapper to the generated client `av_ingest_client::pb_client::edge_ingest_client::
//! EdgeIngestClient<tonic::transport::Channel>` (a plain `pub mod` this crate already
//! exposes) and builds its own `Channel` with [`av_grpc::tls::connect`] -- the same
//! OpenSSL-backed (never `ring`) `hyper-openssl` connector `crates/av-grpc/src/bin/
//! describe_client.rs` already uses to prove mTLS through `services/gmat-service/deploy`'s
//! own nginx front. No new TLS stack, no new crypto-adjacent crate (question 155): both
//! `av-grpc` and `openssl` are already this workspace's own, already-audited dependencies.
//!
//! # What it does, in one process
//!
//! 1. Connects (mTLS if `--client-cert`/`--client-key` are given, otherwise no client
//!    certificate at all) to `--endpoint` (an `https://host:port` URI -- the nginx
//!    front's own listen address), trusting `--server-ca` for the front's own TLS server
//!    certificate.
//! 2. Calls `Announce` with one `PluginManifest` built from its other arguments, and
//!    prints the resulting `ManifestAck` as one line of JSON on stdout.
//! 3. If `--submit-with-key <path>` was given, additionally signs and submits exactly one
//!    `MeasurementBatch` under that EC P-384 private key (`av_edge::sign::sign_batch`,
//!    `prev_hash = av_edge::hash::GENESIS` -- this binary's one caller never submits a
//!    second batch from the same producer, so this is always that producer's first),
//!    printing the resulting `BatchVerdict` as a second line of JSON.
//!
//! Every failure this binary itself does not have a JSON shape for (a malformed argument,
//! a file that will not read, `connect` itself failing) is printed to **stderr** and this
//! process exits non-zero; every RPC OUTCOME this binary understands (`Announce`
//! accepted/refused, `Submit` accepted/rejected) is printed to **stdout** as JSON and this
//! process exits `0` regardless of which outcome it was -- the caller (`tests/
//! test_edge_ingest_mtls.py`) decides what a given outcome means for its own assertions;
//! this binary's job is only to observe and report exactly what happened, not to grade it.
use std::path::PathBuf;
use std::process::ExitCode;

use av_edge::{hash, pb, sign};
use av_grpc::tls::{connect, MtlsConfig};
use av_ingest_client::pb_client::edge_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::Private;

struct Args {
    endpoint: String,
    server_ca: PathBuf,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    producer_id: String,
    clearance: String,
    label_marking: String,
    label_caveats: Vec<String>,
    shard_key: String,
    submit_with_key: Option<PathBuf>,
    batch_tai_ns: Option<i64>,
}

const USAGE: &str = "usage: av-ingest-mtls-client --endpoint https://HOST:PORT --server-ca PATH \
    [--client-cert PATH --client-key PATH] --producer-id ID --clearance STR \
    --label-marking STR [--label-caveat STR]... --shard-key STR \
    [--submit-with-key PATH --batch-tai-ns N]";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();
    let mut endpoint = None;
    let mut server_ca = None;
    let mut client_cert = None;
    let mut client_key = None;
    let mut producer_id = None;
    let mut clearance = None;
    let mut label_marking = None;
    let mut label_caveats = Vec::new();
    let mut shard_key = None;
    let mut submit_with_key = None;
    let mut batch_tai_ns = None;

    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--endpoint" => endpoint = Some(value()?),
            "--server-ca" => server_ca = Some(PathBuf::from(value()?)),
            "--client-cert" => client_cert = Some(PathBuf::from(value()?)),
            "--client-key" => client_key = Some(PathBuf::from(value()?)),
            "--producer-id" => producer_id = Some(value()?),
            "--clearance" => clearance = Some(value()?),
            "--label-marking" => label_marking = Some(value()?),
            "--label-caveat" => label_caveats.push(value()?),
            "--shard-key" => shard_key = Some(value()?),
            "--submit-with-key" => submit_with_key = Some(PathBuf::from(value()?)),
            "--batch-tai-ns" => {
                let raw = value()?;
                batch_tai_ns = Some(raw.parse::<i64>().map_err(|e| format!("--batch-tai-ns {raw:?} is not a valid i64: {e}"))?);
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }

    if client_cert.is_some() != client_key.is_some() {
        return Err("--client-cert and --client-key must be given together, or not at all".to_string());
    }
    if submit_with_key.is_some() && batch_tai_ns.is_none() {
        return Err("--submit-with-key requires --batch-tai-ns".to_string());
    }

    Ok(Args {
        endpoint: endpoint.ok_or("--endpoint is required")?,
        server_ca: server_ca.ok_or("--server-ca is required")?,
        client_cert,
        client_key,
        producer_id: producer_id.ok_or("--producer-id is required")?,
        clearance: clearance.ok_or("--clearance is required")?,
        label_marking: label_marking.ok_or("--label-marking is required")?,
        label_caveats,
        shard_key: shard_key.ok_or("--shard-key is required")?,
        submit_with_key,
        batch_tai_ns,
    })
}

fn load_ec_private_key(path: &std::path::Path) -> Result<EcKey<Private>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("reading {path:?}: {e}"))?;
    EcKey::private_key_from_pem(&pem).map_err(|e| format!("{path:?} is not a valid EC private key PEM: {e}"))
}

async fn run(args: Args) -> Result<serde_json::Value, String> {
    let cfg = MtlsConfig {
        ca_file: &args.server_ca,
        client_cert: args.client_cert.as_deref(),
        client_key: args.client_key.as_deref(),
    };
    let channel = connect(&args.endpoint, cfg).await.map_err(|e| format!("connecting to {}: {e}", args.endpoint))?;
    let mut client = EdgeIngestClient::new(channel);

    let manifest = pb::PluginManifest {
        producer_id: args.producer_id.clone(),
        plugin_version: String::new(),
        output_schemas: Vec::new(),
        frame_ids: Vec::new(),
        label: Some(pb::Label { marking: args.label_marking.clone(), caveats: args.label_caveats.clone() }),
        clearance: args.clearance.clone(),
        shard_keys: vec![args.shard_key.clone()],
        leaf_fingerprint_sha256: String::new(),
    };

    let ack = client.announce(tonic::Request::new(manifest)).await.map_err(|e| format!("Announce RPC failed at the transport/status level: {e}"))?.into_inner();

    let refusal_name = pb::ManifestRefusal::try_from(ack.refusal).map(|r| r.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown ManifestRefusal {}>", ack.refusal));

    let mut out = serde_json::json!({
        "announce": {
            "accepted": ack.accepted,
            "refusal": refusal_name,
            "detail": ack.detail,
            "chain_head_hex": hash::hex_encode(&ack.chain_head),
        }
    });

    if let Some(key_path) = &args.submit_with_key {
        let key = load_ec_private_key(key_path)?;
        let mut batch = pb::MeasurementBatch {
            producer_id: args.producer_id,
            sequence: 1,
            label: Some(pb::Label { marking: args.label_marking, caveats: args.label_caveats }),
            batch_tai_ns: args.batch_tai_ns.expect("validated in parse_args"),
            shard_key: args.shard_key,
            ..Default::default()
        };
        sign::sign_batch(&mut batch, hash::GENESIS, &key).map_err(|e| format!("signing the batch: {e}"))?;

        let stream = futures_util::stream::iter(vec![batch]);
        let response = client.submit(tonic::Request::new(stream)).await.map_err(|e| format!("Submit RPC failed at the transport/status level: {e}"))?;
        let mut inbound = response.into_inner();
        let mut verdicts = Vec::new();
        loop {
            match inbound.message().await {
                Ok(Some(v)) => {
                    let rejection_name = pb::BatchRejection::try_from(v.rejection).map(|r| r.as_str_name().to_string()).unwrap_or_else(|_| format!("<unknown BatchRejection {}>", v.rejection));
                    verdicts.push(serde_json::json!({
                        "accepted": v.accepted,
                        "rejection": rejection_name,
                        "detail": v.detail,
                    }));
                }
                Ok(None) => break,
                Err(status) => return Err(format!("Submit stream failed mid-way: {status}")),
            }
        }
        out["submit_verdicts"] = serde_json::Value::Array(verdicts);
    }

    Ok(out)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-ingest-mtls-client: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(args).await {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("av-ingest-mtls-client: {e}");
            ExitCode::FAILURE
        }
    }
}

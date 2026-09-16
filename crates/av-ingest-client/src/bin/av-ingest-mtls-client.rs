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
//! # `--batch-tai-ns`: a literal i64, or the `now` sentinel (question 223)
//!
//! `--batch-tai-ns` accepts either a literal TAI-nanosecond `i64`, or the literal string
//! `now`, meaning "read the wall clock at the moment this process actually builds the
//! batch (immediately before `av_edge::sign::sign_batch` runs), not whenever the caller
//! happened to compute a value earlier." Question 223's own defect was exactly this: a
//! caller (`tests/test_edge_ingest_mtls.py`) that computed a TAI timestamp in Python
//! *before* spawning this process -- so that stamp's age, by the time the server's
//! `Submit` handler actually reads it (`av_edge::policy::ProducerPolicy::is_stale`),
//! already included this process's own startup, the mTLS handshake through the front,
//! and the `Announce` round trip, none of which the batch's own declared age is supposed
//! to reflect at all. The `now` sentinel closes that gap by moving the read to the last
//! possible moment inside the one process that actually needs it: `run` below resolves
//! it via `real_clock_tai_ns` immediately before constructing `pb::MeasurementBatch`,
//! using the exact same `SystemTime::now()` -> Unix nanoseconds -> `av_cdm::time::Tai::
//! from_utc_nanos` conversion `crates/av-ingest/src/bin/av-ingest-server.rs::
//! read_real_clock_tai_ns` already uses for `--real-clock` -- the one UTC-to-TAI boundary
//! this workspace has, reused here rather than reinvented (there is no second leap-
//! second-aware offset anywhere in this tree, and this binary adds none).
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

/// `--batch-tai-ns`'s resolved argument: either a literal TAI-nanosecond value the caller
/// computed itself, or the `now` sentinel, resolved to an actual wall-clock reading only
/// once, immediately before the batch is built (see this binary's own module doc,
/// "`--batch-tai-ns`: a literal i64, or the `now` sentinel").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchTaiNs {
    Literal(i64),
    Now,
}

impl std::str::FromStr for BatchTaiNs {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw == "now" {
            return Ok(BatchTaiNs::Now);
        }
        raw.parse::<i64>().map(BatchTaiNs::Literal).map_err(|e| format!("--batch-tai-ns {raw:?} is not \"now\" or a valid i64: {e}"))
    }
}

#[derive(Debug)]
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
    batch_tai_ns: Option<BatchTaiNs>,
}

const USAGE: &str = "usage: av-ingest-mtls-client --endpoint https://HOST:PORT --server-ca PATH \
    [--client-cert PATH --client-key PATH] --producer-id ID --clearance STR \
    --label-marking STR [--label-caveat STR]... --shard-key STR \
    [--submit-with-key PATH --batch-tai-ns (N | now)]\n\
    \n\
    --batch-tai-ns accepts either a literal TAI-nanosecond i64, or the literal string \
    \"now\", which reads the wall clock at the moment the batch is actually built (question \
    223: the batch's declared age must not include this process's own startup, the mTLS \
    handshake, or the Announce round trip).";

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
                batch_tai_ns = Some(raw.parse::<BatchTaiNs>()?);
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

/// The `now` sentinel's own clock read: `SystemTime::now()` -> Unix nanoseconds ->
/// `av_cdm::time::Tai::from_utc_nanos` -- the exact same UTC-to-TAI conversion
/// `crates/av-ingest/src/bin/av-ingest-server.rs::read_real_clock_tai_ns` uses for
/// `--real-clock`, reused rather than reimplemented (this workspace has exactly one
/// leap-second-aware UTC-to-TAI boundary, `av_cdm::time::Tai`, and this binary already
/// depends on `av-cdm` transitively through `av-edge`/`av-ingest-client`).
fn real_clock_tai_ns() -> i64 {
    let unix_duration = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock is set before the Unix epoch");
    let unix_ns = i64::try_from(unix_duration.as_nanos()).expect("system clock is implausibly far in the future to fit in an i64 nanosecond count");
    av_cdm::time::Tai::from_utc_nanos(unix_ns).as_nanos()
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
        // Resolved as late as possible -- immediately before the batch is built and
        // signed, never earlier -- so the `now` sentinel's whole point (question 223) is
        // not undone by reading it any sooner than this.
        let batch_tai_ns = match args.batch_tai_ns.expect("validated in parse_args") {
            BatchTaiNs::Literal(ns) => ns,
            BatchTaiNs::Now => real_clock_tai_ns(),
        };
        let mut batch = pb::MeasurementBatch {
            producer_id: args.producer_id,
            sequence: 1,
            label: Some(pb::Label { marking: args.label_marking, caveats: args.label_caveats }),
            batch_tai_ns,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Vec<String> {
        let mut v = vec![
            "av-ingest-mtls-client".to_string(),
            "--endpoint".to_string(),
            "https://127.0.0.1:1".to_string(),
            "--server-ca".to_string(),
            "/dev/null".to_string(),
            "--producer-id".to_string(),
            "p".to_string(),
            "--clearance".to_string(),
            "CUI".to_string(),
            "--label-marking".to_string(),
            "CUI".to_string(),
            "--shard-key".to_string(),
            "shard-a".to_string(),
        ];
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn parse_args_accepts_the_now_sentinel_for_batch_tai_ns() {
        let a = parse_args(args(&["--submit-with-key", "/dev/null", "--batch-tai-ns", "now"]).into_iter()).expect("parse_args should accept the `now` sentinel");
        assert_eq!(a.batch_tai_ns, Some(BatchTaiNs::Now));
    }

    #[test]
    fn parse_args_accepts_a_literal_i64_for_batch_tai_ns() {
        let a = parse_args(args(&["--submit-with-key", "/dev/null", "--batch-tai-ns", "1234567890"]).into_iter()).expect("parse_args should accept a literal i64");
        assert_eq!(a.batch_tai_ns, Some(BatchTaiNs::Literal(1_234_567_890)));
    }

    #[test]
    fn parse_args_rejects_garbage_for_batch_tai_ns() {
        let err = parse_args(args(&["--submit-with-key", "/dev/null", "--batch-tai-ns", "not-a-number"]).into_iter()).expect_err("parse_args should reject garbage");
        assert!(err.contains("not-a-number"), "{err}");
        assert!(err.contains("\"now\""), "{err}");
    }

    #[test]
    fn batch_tai_ns_from_str_is_case_sensitive_about_the_now_sentinel() {
        // "Now"/"NOW" are deliberately NOT accepted -- exactly one spelling, matching this
        // binary's own USAGE string, rather than a case-insensitive guess.
        assert!("Now".parse::<BatchTaiNs>().is_err());
        assert!("NOW".parse::<BatchTaiNs>().is_err());
        assert_eq!("now".parse::<BatchTaiNs>(), Ok(BatchTaiNs::Now));
    }

    #[test]
    fn real_clock_tai_ns_is_in_the_right_ballpark() {
        // Not a golden -- just a sanity bound that this reads an actual current-ish wall
        // clock through av_cdm::time::Tai rather than, say, returning 0 or a UTC value
        // mistaken for TAI. 2020-01-01T00:00:00Z in TAI nanoseconds, and 2100-01-01T00:00:00Z,
        // bound any real reading taken while this test suite runs.
        let ns = real_clock_tai_ns();
        assert!(ns > 1_577_836_800_000_000_000, "real_clock_tai_ns() = {ns} is before 2020");
        assert!(ns < 4_102_444_800_000_000_000, "real_clock_tai_ns() = {ns} is after 2100");
    }
}

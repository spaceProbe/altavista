//! `tests/test_grpc_tls.py`'s Rust half: dial `altavista.v1.DynamicsService.Describe`
//! through the gmat-service nginx mTLS front and report what happened, so the Python test
//! can assert on this process's exit code and output rather than embedding a Rust
//! extension.
//!
//! Usage:
//!
//! ```text
//! describe_client --endpoint https://127.0.0.1:PORT --ca root.pem \
//!     [--client-cert leaf+chain.pem --client-key leaf.key] [--model-id ID]
//! ```
//!
//! `--client-cert`/`--client-key` are optional so the same binary drives both halves of
//! the test: present, this call is expected to succeed through `ssl_verify_client on`;
//! absent, the request is expected to be refused (as measured, nginx completes the TLS
//! handshake but rejects the HTTP request itself with 400 -- see
//! `tests/test_grpc_tls.py`'s module docstring) and this process exits non-zero with the
//! resulting error on stderr.
//!
//! Exit codes: `0` on a successful `Describe` (a one-line `OK ...` summary on stdout);
//! `1` on a bad command line; `2` on a connect or RPC failure (the error goes to stderr).

use std::path::PathBuf;
use std::process::ExitCode;

use av_grpc::pb::DescribeRequest;
use av_grpc::tls::{self, MtlsConfig};
use av_grpc::DynamicsServiceClient;

struct Args {
    endpoint: String,
    ca: PathBuf,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    model_id: String,
}

fn parse_args() -> Result<Args, String> {
    let mut endpoint = None;
    let mut ca = None;
    let mut client_cert = None;
    let mut client_key = None;
    let mut model_id = String::new();

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--endpoint" => endpoint = Some(val()?),
            "--ca" => ca = Some(PathBuf::from(val()?)),
            "--client-cert" => client_cert = Some(PathBuf::from(val()?)),
            "--client-key" => client_key = Some(PathBuf::from(val()?)),
            "--model-id" => model_id = val()?,
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(Args {
        endpoint: endpoint.ok_or("--endpoint is required")?,
        ca: ca.ok_or("--ca is required")?,
        client_cert,
        client_key,
        model_id,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("describe_client: {e}");
            return ExitCode::from(1);
        }
    };

    let cfg = MtlsConfig {
        ca_file: &args.ca,
        client_cert: args.client_cert.as_deref(),
        client_key: args.client_key.as_deref(),
    };

    let channel = match tls::connect(&args.endpoint, cfg).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("describe_client: connect failed: {e}");
            return ExitCode::from(2);
        }
    };

    let mut client = DynamicsServiceClient::new(channel);
    let resp = match client
        .describe(DescribeRequest { model_id: args.model_id })
        .await
    {
        Ok(r) => r.into_inner(),
        Err(status) => {
            eprintln!("describe_client: Describe RPC failed: {status}");
            return ExitCode::from(2);
        }
    };

    println!(
        "OK id={} depth={} settings_hash={} capabilities={}",
        resp.id,
        resp.depth,
        resp.settings_hash,
        resp.capabilities.len()
    );
    ExitCode::SUCCESS
}

//! `av-edge-make-signed-batch` -- the small Rust test helper `tests/
//! test_edge_identity_seccert.py` uses to produce a `MeasurementBatch` signed with a
//! genuinely lego-issued leaf's private key (never a committed fixture key: the key is
//! different on every provisioning run, so a committed fixture is not possible here --
//! `docs/edge-plan.md` milestone E2, requirement "(e)").
//!
//! ```text
//! av-edge-make-signed-batch --key <leaf.key> --fingerprint-sha256 <hex> \
//!     --producer-id <id> --sequence <u64> --batch-tai-ns <i64> --out <batch.pb>
//! ```
//!
//! Writes `prost::Message::encode_to_vec` of the signed `MeasurementBatch` to `--out`,
//! for `av-edge-identity --verify-batch` to consume. `--key` is parsed with
//! `openssl::pkey::PKey::private_key_from_pem`, which (unlike `av_edge::sign::
//! load_signing_key`, which deliberately requires SEC1 "-----BEGIN EC PRIVATE KEY-----"
//! -- see that function's own doc comment) accepts either SEC1 or PKCS8
//! ("-----BEGIN PRIVATE KEY-----") EC key PEM, since lego's own key-file format is not a
//! contract this binary should be fragile against.

use av_edge::{hash, pb, sign};
use openssl::pkey::PKey;

struct Args {
    key: String,
    fingerprint_sha256: String,
    producer_id: String,
    sequence: u64,
    batch_tai_ns: i64,
    out: String,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();
    let mut key = None;
    let mut fingerprint_sha256 = None;
    let mut producer_id = None;
    let mut sequence = None;
    let mut batch_tai_ns = None;
    let mut out = None;

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--key" => key = Some(args.next().ok_or("--key requires a value")?),
            "--fingerprint-sha256" => fingerprint_sha256 = Some(args.next().ok_or("--fingerprint-sha256 requires a value")?),
            "--producer-id" => producer_id = Some(args.next().ok_or("--producer-id requires a value")?),
            "--sequence" => {
                let raw = args.next().ok_or("--sequence requires a value")?;
                sequence = Some(raw.parse::<u64>().map_err(|e| format!("--sequence {raw:?} is not a valid u64: {e}"))?);
            }
            "--batch-tai-ns" => {
                let raw = args.next().ok_or("--batch-tai-ns requires a value")?;
                batch_tai_ns = Some(raw.parse::<i64>().map_err(|e| format!("--batch-tai-ns {raw:?} is not a valid i64: {e}"))?);
            }
            "--out" => out = Some(args.next().ok_or("--out requires a value")?),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(Args {
        key: key.ok_or("--key is required")?,
        fingerprint_sha256: fingerprint_sha256.ok_or("--fingerprint-sha256 is required")?,
        producer_id: producer_id.ok_or("--producer-id is required")?,
        sequence: sequence.ok_or("--sequence is required")?,
        batch_tai_ns: batch_tai_ns.ok_or("--batch-tai-ns is required")?,
        out: out.ok_or("--out is required")?,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args(std::env::args()).map_err(|e| format!("argument error: {e}"))?;

    let key_pem = std::fs::read(&args.key).map_err(|e| format!("reading --key {:?}: {e}", args.key))?;
    let pkey = PKey::private_key_from_pem(&key_pem).map_err(|e| format!("parsing --key {:?} as an EC private key (SEC1 or PKCS8): {e}", args.key))?;
    let ec_key = pkey.ec_key().map_err(|e| format!("--key {:?} is not an EC key: {e}", args.key))?;

    let mut batch = pb::MeasurementBatch {
        producer_id: args.producer_id,
        sequence: args.sequence,
        label: Some(pb::Label { marking: "UNCLASSIFIED".to_string(), caveats: vec![] }),
        batch_tai_ns: args.batch_tai_ns,
        ..Default::default()
    };
    sign::sign_batch_with_signer(&mut batch, hash::GENESIS, &ec_key, &args.fingerprint_sha256).map_err(|e| format!("signing the batch: {e}"))?;

    let bytes = <pb::MeasurementBatch as prost::Message>::encode_to_vec(&batch);
    std::fs::write(&args.out, bytes).map_err(|e| format!("writing --out {:?}: {e}", args.out))?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("av-edge-make-signed-batch: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

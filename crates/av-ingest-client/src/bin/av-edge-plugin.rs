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
//! (for `pb`/`sign`/`hash`/`plugin`/`identity`/`buffer`) *and* on `av-grpc` (for the
//! OpenSSL mTLS connector -- see that crate's own `Cargo.toml` comment on `src/bin/
//! av-ingest-mtls-client.rs`, this binary's direct sibling and the precedent this file
//! follows for the mTLS path), and it already carries `serde_json` for exactly this kind
//! of machine-readable stdout summary. Adding this binary here is one more `[[bin]]`
//! target on a crate that already has the complete dependency set this task needs, not a
//! new dependency edge anywhere.
//!
//! # Command-line configured, never environment-configured (question 199)
//!
//! Every value this binary's behaviour depends on -- which run to replay, which endpoint
//! to dial, which key to sign with, how to pace, which certificate and Root to verify
//! this process's own identity against, which clock to check that certificate's validity
//! at, where to buffer while disconnected -- comes from an explicit `--flag`, never from
//! `std::env::var`. `--plugin-config` names a single JSON file (deserialised directly as
//! `av_edge::plugin::PluginConfig`, which already derives `Deserialize` for exactly this
//! reason) rather than dozens of individual flags for that struct's sixteen fields -- the
//! file's *path* is still a command-line argument, so this is "configured on the command
//! line," not "configured by the environment," in the sense question 199's rule actually
//! cares about (no `AV_*`-style variable is ever read).
//!
//! # Presenting and verifying this plugin's own identity (E2, question 207)
//!
//! `--trust-anchor <root.pem>` and `--client-cert <leaf-or-fullchain.pem>` are optional,
//! but must be given together -- either alone is a plain argument error (`parse_args`),
//! never a silent no-op. When both are given, **before this binary ever dials
//! `--endpoint`, announces anything, or signs a single batch**, `run` builds
//! [`av_edge::identity::TrustAnchors::from_root_pem`] from `--trust-anchor` (the Root, and
//! only the Root -- see that module's own doc for why this is deliberate, never the host's
//! system trust store) and calls [`av_edge::identity::verify_identity`] against
//! `--client-cert`'s own leaf. A refusal prints the typed `IdentityRejection` and the
//! `IdentityCounters` this one call produced as one line of JSON to **stderr** and exits
//! non-zero without ever reaching `Client::connect` -- a plugin whose own identity is not
//! trusted must not reach the wire, must not announce, and must not sign (this file's own
//! `run` puts the identity check ahead of `BatchBuilder::build_batches_for_config`,
//! exactly for the "must not sign" half of that rule). On success, the verified
//! `EdgeIdentity.fingerprint_sha256` becomes this run's signing fingerprint: it
//! **overrides** `PluginConfig.leaf_fingerprint_sha256` (so the manifest
//! [`PluginConfig::manifest`] and every batch's `signer_cert_sha256`
//! [`BatchBuilder::build_batches_for_config`] -- both derived from the identical `cfg`
//! value -- cannot drift apart from the certificate actually verified, exactly what
//! [`av_edge::identity::verify_batch_signed_by`] exists downstream to check); if
//! `--plugin-config` already declared a *different*, non-empty `leaf_fingerprint_sha256`,
//! that disagreement is a hard error naming both values rather than a silent overwrite.
//!
//! **Splitting `--client-cert` into leaf and chain.** `--client-cert` may name a single
//! leaf certificate, or a "fullchain.pem" bundling the leaf followed by one or more
//! Intermediates (a common seccert/lego output shape) -- this binary adds no separate
//! `--chain` flag for it. `split_leaf_and_chain` parses every certificate in the file
//! (`X509::stack_from_pem`); the first is always the leaf, and anything after it is
//! re-encoded as one PEM blob and passed as `verify_identity`'s `chain_pem` for path-
//! building against `--trust-anchor`'s Root -- exactly what this platform's two-tier PKI
//! (Root -> Intermediate -> leaf, `av_edge::identity`'s own module doc) needs to verify a
//! leaf that was never issued directly by the Root.
//!
//! # Injecting a clock for certificate validity (`--now-tai-ns`)
//!
//! `verify_identity` needs a `now_tai_ns` to check `--client-cert`'s own validity window
//! against -- this platform's rule is "clocks injected, never slept": a **binary** may
//! read the real clock (there is no test double-checking its own wall clock), but a
//! **test** must inject one. `--now-tai-ns <i64>` is that injection point: when given, it
//! is used verbatim (`crates/av-edge/tests/identity.rs`-style tests set this to move
//! before/after a leaf's own `not_after` without ever sleeping). When **absent**,
//! `read_real_clock_tai_ns` supplies it: `std::time::SystemTime::now()` -> Unix
//! nanoseconds -> `av_cdm::time::Tai::from_utc_nanos` -- the same conversion
//! `crates/av-ingest/src/bin/av-ingest-server.rs::read_real_clock_tai_ns` already uses for
//! its own `--real-clock` flag, so a real deployment of this binary (no `--now-tai-ns`
//! given) checks its own certificate against the actual wall clock, exactly as an operator
//! would expect. `--now-tai-ns` has no effect on anything else in this binary (in
//! particular, it never feeds `Pacing::due_at`'s pacing decisions or any batch's own
//! `batch_tai_ns` -- those keep coming from the replayed run's own recorded epochs).
//!
//! # Presenting the leaf on the wire -- and why not twice
//!
//! A verified identity is only useful to `av-ingest` if the ingest can re-verify it
//! itself and count the result (E3b, question 202/205(1): nginx runs
//! `ssl_verify_client optional_no_ca`, so `av-ingest`, not the front, is the enforcement
//! point). On the **plaintext-loopback** path (`--endpoint host:port`), this binary
//! therefore calls `EdgeIngestClient::set_forwarded_client_cert` with
//! `av_ingest_client::forwarded_cert::percent_encode_pem_like_nginx(&leaf_pem)` before
//! `Announce` -- the same header (`x-ssl-client-escaped-cert`,
//! `av_ingest::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER`) a service-owned nginx front
//! would set, simulated here because plaintext loopback has no such front in front of it.
//! On the **`https://` mTLS** path (`--endpoint https://host:port`), a real nginx front
//! terminates the TLS handshake and sets that header **itself**, from the certificate it
//! actually saw on the wire -- this binary must not also set it there: doing so would let
//! a plugin process assert a certificate identity nginx never verified, exactly the
//! trust-boundary violation `av_ingest::forwarded_cert`'s own module doc's "the ingest,
//! not nginx and not this plugin, is the enforcement point" rule exists to prevent. So
//! `Client::Mtls`'s own raw generated client is never given a `set_forwarded_client_cert`
//! method at all (unlike `EdgeIngestClient`) -- there is deliberately no way to call it on
//! that path from this file.
//!
//! # `--buffer-dir`: E6's durable edge buffer, opt-in
//!
//! Without `--buffer-dir`, every batch is submitted directly through the connected
//! `Client`, exactly as before this task -- no buffer, no new file, anywhere. With
//! `--buffer-dir <path>`, every batch instead goes through
//! [`av_edge::buffer::UplinkDriver`] over a [`PluginBatchSink`] (a
//! [`av_edge::buffer::BatchSink`] bridging this binary's already-connected, async
//! `Client` to that trait's synchronous `submit` -- copying, not reinventing,
//! `crates/av-ingest/tests/e6_wire_disconnect.rs::GrpcBatchSink`'s own bridge: a
//! dedicated background thread owning its own `tokio::runtime::Runtime`, commands sent
//! over a `std::sync::mpsc` channel. Unlike that test's own sink, this one is handed an
//! already-connected, already-`Announce`d `Client` -- `--buffer-dir` never changes how
//! this binary connects or announces, only how it submits), rooted at
//! `<--buffer-dir>/<producer_id>.buflog` -- so a disconnection mid-run is survived exactly
//! as E6 specifies (buffered while down, replayed in order once the link returns) instead
//! of this binary simply failing outright. A run with nothing ever disconnecting produces
//! an **empty** buffer file (`EdgeBuffer::append` is only ever called from inside
//! `UplinkDriver::step`'s own `SinkOutcome::LinkDown` branch -- see that function's own
//! doc comment) -- `crates/av-ingest-client/tests/av_edge_plugin_buffer.rs` proves the
//! batch count and chain head this binary prints are identical with and without
//! `--buffer-dir`, and that the (real, on-disk, possibly-empty) buffer file this flag
//! produces reads back correctly through `EdgeBuffer::open`/`record_count`.
//!
//! # What it does
//!
//! 1. Loads `--run-products` (`altavista.v1.RunProducts`) and `--port-traffic` (the
//!    `PortTrafficLog` sidecar), hash-verifying the sidecar against `RunProducts.
//!    port_traffic_hash` via `av_edge::plugin::verify_port_traffic_log` before decoding a
//!    single byte of it.
//! 2. Loads `--plugin-config` (JSON) and `--signing-key` (an EC P-384 private key PEM),
//!    and validates the config (`PluginConfig::validate`).
//! 3. If `--client-cert`/`--trust-anchor` were given: verifies this process's own
//!    identity (see "Presenting and verifying this plugin's own identity" above),
//!    refusing before any batch is signed on failure, and overriding
//!    `PluginConfig.leaf_fingerprint_sha256` with the verified fingerprint on success.
//! 4. Builds the manifest from that same config (`PluginConfig::manifest`) and one
//!    `PortTrafficSource`/chain of signed `MeasurementBatch`es from it
//!    (`PortTrafficSource::from_log`, `BatchBuilder::build_batches_for_config`) -- the
//!    manifest and the batches are derived from the identical `PluginConfig` value, so
//!    they cannot drift apart.
//! 5. Connects: `--endpoint host:port` dials plaintext loopback
//!    (`EdgeIngestClient::connect_plaintext`); `--endpoint https://host:port` (with
//!    `--server-ca`, and optionally `--client-cert`/`--client-key`) dials mTLS through a
//!    service-owned nginx front via `av_grpc::tls::connect` -- reusing, not rewriting,
//!    `src/bin/av-ingest-mtls-client.rs`'s own connection recipe. On the plaintext path
//!    only, a verified leaf is also forwarded as `x-ssl-client-escaped-cert` (see above).
//! 6. Calls `Announce`, then streams every batch -- directly, or (with `--buffer-dir`)
//!    through `UplinkDriver` -- honouring `--pacing` (`as-fast-as-possible`, the default,
//!    or `real-time:<scale>`) -- `av_edge::plugin::Pacing::due_at` decides *when* each
//!    batch is due (a pure function); **this binary is the only place in this whole path
//!    that ever calls `std::thread::sleep`** -- never `av-edge`, never a test.
//! 7. Prints one line of JSON to stdout: `batch_count`, `measurement_count`,
//!    `chain_head_hex` (the last batch's own `batch_hash`), `identity_verified`/
//!    `subject_cn`/`fingerprint_sha256` (evidence that step 3 ran and what it found, so
//!    this fact lives in the artifact, not only in the exit code), and one entry per
//!    verdict (`sequence`, `accepted`, `rejection`, `detail`). Exits non-zero iff any
//!    batch was rejected, identity verification refused (step 3), or a hard failure (bad
//!    args, a file that will not read, a connect failure) occurred -- printed to
//!    **stderr** in that case, mirroring `src/bin/av-ingest-mtls-client.rs`'s own
//!    stdout/stderr split.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use av_edge::buffer::{BatchSink, EdgeBuffer, SinkOutcome, StepOutcome, UplinkDriver};
use av_edge::identity::{IdentityCounters, IdentityRejection, TrustAnchors};
use av_edge::plugin::{BatchBuilder, PluginConfig, PortTrafficSource};
use av_edge::sign;
use av_edge::{hash, pb};
use av_grpc::tls::{connect, MtlsConfig};
use av_ingest_client::pb_client::edge_ingest_client::EdgeIngestClient as RawEdgeIngestClient;
use av_ingest_client::EdgeIngestClient;
use openssl::ec::EcKey;
use openssl::pkey::Private;
use openssl::x509::X509;

struct Args {
    run_products: PathBuf,
    port_traffic: PathBuf,
    plugin_config: PathBuf,
    signing_key: PathBuf,
    endpoint: String,
    server_ca: Option<PathBuf>,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    trust_anchor: Option<PathBuf>,
    now_tai_ns: Option<i64>,
    buffer_dir: Option<PathBuf>,
}

const USAGE: &str = "usage: av-edge-plugin --run-products PATH --port-traffic PATH --plugin-config PATH \
    --signing-key PATH --endpoint (HOST:PORT | https://HOST:PORT) \
    [--server-ca PATH] [--client-cert PATH --client-key PATH] \
    [--trust-anchor PATH] [--now-tai-ns I64] [--buffer-dir PATH]";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let _argv0 = args.next();
    let (mut run_products, mut port_traffic, mut plugin_config, mut signing_key, mut endpoint, mut server_ca, mut client_cert, mut client_key) = (None, None, None, None, None, None, None, None);
    let (mut trust_anchor, mut now_tai_ns, mut buffer_dir) = (None, None, None);
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
            "--trust-anchor" => trust_anchor = Some(PathBuf::from(value()?)),
            "--now-tai-ns" => {
                let raw = value()?;
                now_tai_ns = Some(raw.parse::<i64>().map_err(|e| format!("--now-tai-ns {raw:?} is not a valid i64: {e}"))?);
            }
            "--buffer-dir" => buffer_dir = Some(PathBuf::from(value()?)),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }
    if client_cert.is_some() != client_key.is_some() {
        return Err("--client-cert and --client-key must be given together, or not at all".to_string());
    }
    if client_cert.is_some() != trust_anchor.is_some() {
        return Err("--client-cert and --trust-anchor must be given together, or not at all".to_string());
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
        trust_anchor,
        now_tai_ns,
        buffer_dir,
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

/// The one sanctioned live-clock read site this binary's own module doc names ("Injecting
/// a clock for certificate validity"): `SystemTime::now()` -> Unix nanoseconds ->
/// `av_cdm::time::Tai` -- mirrors `crates/av-ingest/src/bin/av-ingest-server.rs::
/// read_real_clock_tai_ns` exactly. Only ever called when `--now-tai-ns` was not given
/// (question 199: a binary may read the real clock; a test must inject one via
/// `--now-tai-ns` instead, never by sleeping).
fn read_real_clock_tai_ns() -> i64 {
    let unix_duration = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock is set before the Unix epoch");
    let unix_ns = i64::try_from(unix_duration.as_nanos()).expect("system clock is implausibly far in the future to fit in an i64 nanosecond count");
    av_cdm::time::Tai::from_utc_nanos(unix_ns).as_nanos()
}

/// Splits `--client-cert`'s own PEM bytes into its leaf (the first certificate) and,
/// when the file bundles more than one, the rest re-encoded as one PEM blob for
/// `verify_identity`'s `chain_pem` -- see this file's own module doc, "Splitting
/// --client-cert into leaf and chain".
fn split_leaf_and_chain(bundle_pem: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let certs = X509::stack_from_pem(bundle_pem).map_err(|e| format!("parsing --client-cert as PEM certificate(s): {e}"))?;
    let mut iter = certs.into_iter();
    let leaf = iter.next().ok_or_else(|| "--client-cert contains no certificates".to_string())?;
    let leaf_pem = leaf.to_pem().map_err(|e| format!("re-encoding --client-cert's leaf certificate: {e}"))?;
    let rest: Vec<X509> = iter.collect();
    let chain_pem = if rest.is_empty() {
        None
    } else {
        let mut buf = Vec::new();
        for cert in &rest {
            buf.extend(cert.to_pem().map_err(|e| format!("re-encoding --client-cert's chain certificate: {e}"))?);
        }
        Some(buf)
    };
    Ok((leaf_pem, chain_pem))
}

/// `IdentityRejection`'s own short, machine-stable tag -- mirrors `crates/av-edge/src/
/// bin/av-edge-identity.rs::rejection_kind` exactly (that binary's own precedent for
/// naming a rejection in a JSON field distinct from its human-readable `Display` text).
fn rejection_kind(r: &IdentityRejection) -> &'static str {
    match r {
        IdentityRejection::MalformedPem(_) => "MALFORMED_PEM",
        IdentityRejection::NotP384 { .. } => "NOT_P384",
        IdentityRejection::IssuerNotTrusted(_) => "ISSUER_NOT_TRUSTED",
        IdentityRejection::Expired { .. } => "EXPIRED",
        IdentityRejection::NotYetValid { .. } => "NOT_YET_VALID",
        IdentityRejection::Openssl(_) => "OPENSSL_ERROR",
    }
}

/// `IdentityCounters` as JSON -- that struct itself derives no `Serialize` (`crates/
/// av-edge/src/identity.rs` deliberately keeps it "a plain Rust struct", per its own doc
/// comment), so this binary builds the object by hand from its public fields.
fn identity_counters_json(c: &IdentityCounters) -> serde_json::Value {
    serde_json::json!({
        "accepted": c.accepted,
        "malformed_pem": c.malformed_pem,
        "not_p384": c.not_p384,
        "issuer_not_trusted": c.issuer_not_trusted,
        "expired": c.expired,
        "not_yet_valid": c.not_yet_valid,
        "openssl_error": c.openssl_error,
    })
}

/// What step 3 of this binary's own module doc ("What it does") found -- `None` when
/// `--client-cert`/`--trust-anchor` were not given at all (existing, unchanged
/// behaviour); `Some` carries exactly what the final JSON summary and the forwarded-
/// certificate header (plaintext path only) need.
struct VerifiedIdentity {
    subject_cn: String,
    fingerprint_sha256: String,
    leaf_pem: Vec<u8>,
}

/// Runs this binary's own step 3 ("What it does"): verifies `args.client_cert` against
/// `args.trust_anchor` at `args.now_tai_ns` (or the real clock -- `read_real_clock_tai_ns`)
/// if both were given, and reconciles the result against `cfg.leaf_fingerprint_sha256`.
/// Returns `Ok(None)` (and leaves `cfg` untouched) when neither flag was given -- the
/// exact byte-for-byte-unchanged path `crates/av-ingest-client/tests/
/// av_edge_plugin_binary.rs` pins. On a refusal, prints the typed rejection and the
/// counters that one call produced as JSON to stderr itself (this file's own module doc:
/// "prints... to stderr") before returning `Err`, so `main`'s own `eprintln!` is
/// additional context, not the only place the machine-readable evidence lives.
fn verify_own_identity(args: &Args, cfg: &mut PluginConfig) -> Result<Option<VerifiedIdentity>, String> {
    let (Some(client_cert_path), Some(trust_anchor_path)) = (&args.client_cert, &args.trust_anchor) else {
        return Ok(None);
    };

    let bundle_pem = std::fs::read(client_cert_path).map_err(|e| format!("reading --client-cert {client_cert_path:?}: {e}"))?;
    let (leaf_pem, chain_pem) = split_leaf_and_chain(&bundle_pem)?;
    let root_pem = std::fs::read(trust_anchor_path).map_err(|e| format!("reading --trust-anchor {trust_anchor_path:?}: {e}"))?;
    let anchors = TrustAnchors::from_root_pem(&root_pem).map_err(|e| format!("loading --trust-anchor {trust_anchor_path:?}: {e}"))?;
    let now_tai_ns = args.now_tai_ns.unwrap_or_else(read_real_clock_tai_ns);

    let mut counters = IdentityCounters::new();
    match av_edge::identity::verify_identity(&leaf_pem, chain_pem.as_deref(), &anchors, now_tai_ns, &mut counters) {
        Err(rejection) => {
            let report = serde_json::json!({
                "identity_verified": false,
                "rejection": rejection_kind(&rejection),
                "detail": rejection.to_string(),
                "counters": identity_counters_json(&counters),
            });
            eprintln!("{report}");
            Err(format!("identity verification refused before connecting: {rejection}"))
        }
        Ok(identity) => {
            if !cfg.leaf_fingerprint_sha256.is_empty() && cfg.leaf_fingerprint_sha256 != identity.fingerprint_sha256 {
                return Err(format!(
                    "--plugin-config declares leaf_fingerprint_sha256={:?} but --client-cert's verified fingerprint is {:?} -- refusing to sign under a mismatched fingerprint",
                    cfg.leaf_fingerprint_sha256, identity.fingerprint_sha256
                ));
            }
            cfg.leaf_fingerprint_sha256 = identity.fingerprint_sha256.clone();
            Ok(Some(VerifiedIdentity { subject_cn: identity.subject_cn, fingerprint_sha256: identity.fingerprint_sha256, leaf_pem }))
        }
    }
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

    /// Sets the forwarded-client-certificate header on the plaintext path only -- see
    /// this file's own module doc, "Presenting the leaf on the wire -- and why not
    /// twice", for why `Client::Mtls` has no equivalent call here at all.
    fn set_forwarded_client_cert_if_plaintext(&mut self, leaf_pem: &[u8]) {
        if let Client::Plaintext(c) = self {
            c.set_forwarded_client_cert(av_ingest_client::forwarded_cert::percent_encode_pem_like_nginx(leaf_pem));
        }
    }

    async fn announce(&mut self, manifest: pb::PluginManifest) -> Result<pb::ManifestAck, String> {
        match self {
            Client::Plaintext(c) => c.announce(manifest).await.map_err(|e| format!("Announce RPC failed: {e}")),
            Client::Mtls(c) => c.announce(tonic::Request::new(manifest)).await.map(tonic::Response::into_inner).map_err(|e| format!("Announce RPC failed: {e}")),
        }
    }

    /// Submits exactly the batches given, in order, collecting every returned verdict.
    /// Called once per pacing "tick" (`main`'s own loop, or [`PluginBatchSink`]'s own
    /// background-thread bridge when `--buffer-dir` is given) -- see this file's module
    /// doc for why pacing is implemented as repeated small submissions rather than a
    /// single paced stream.
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

/// One command [`PluginBatchSink`]'s background thread understands.
enum SinkCmd {
    Submit { batch: pb::MeasurementBatch, reply: std_mpsc::Sender<Result<pb::BatchVerdict, String>> },
}

/// A [`BatchSink`] bridging this binary's already-connected, async [`Client`] to
/// [`UplinkDriver`]'s synchronous `submit` -- see this file's own module doc,
/// "`--buffer-dir`: E6's durable edge buffer, opt-in", for why this copies `crates/
/// av-ingest/tests/e6_wire_disconnect.rs::GrpcBatchSink`'s own thread-plus-channel bridge
/// rather than a different one. Takes ownership of an already-connected,
/// already-`Announce`d `Client` -- its background thread only ever serves `Submit`
/// commands, never reconnects on its own (this binary is a one-shot replay, not a
/// long-running daemon that would retry a dropped link across process invocations; a
/// genuine mid-run transport failure surfaces as an ordinary `Err`, exactly like
/// `GrpcBatchSink::submit`'s own identical distinction between "the link was explicitly
/// torn down" (`LinkDown`) and "the RPC itself failed" (`Err`)).
struct PluginBatchSink {
    cmd_tx: std_mpsc::Sender<SinkCmd>,
}

impl PluginBatchSink {
    fn new(mut client: Client) -> Self {
        let (cmd_tx, cmd_rx) = std_mpsc::channel::<SinkCmd>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("building this sink's own dedicated tokio runtime");
            rt.block_on(async move {
                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        SinkCmd::Submit { batch, reply } => {
                            let outcome = match client.submit(vec![batch]).await {
                                Ok(mut verdicts) if verdicts.len() == 1 => Ok(verdicts.remove(0)),
                                Ok(verdicts) => Err(format!("expected exactly 1 verdict for 1 submitted batch, got {}", verdicts.len())),
                                Err(e) => Err(e),
                            };
                            let _ = reply.send(outcome);
                        }
                    }
                }
                // cmd_rx.recv() returned Err (the sender was dropped -- this binary's own
                // process is exiting): `client` is dropped right here, when this async
                // block ends and `rt` itself is dropped next.
            });
        });
        Self { cmd_tx }
    }
}

impl BatchSink for PluginBatchSink {
    type Verdict = pb::BatchVerdict;
    type Error = String;

    fn submit(&mut self, batch: &pb::MeasurementBatch, _now_tai_ns: i64) -> Result<SinkOutcome<pb::BatchVerdict>, String> {
        // `_now_tai_ns` is unused: staleness is checked against the *server's* own
        // injected clock, not anything this client-side sink could supply per call --
        // mirrors `GrpcBatchSink::submit`'s own identical comment.
        let (reply_tx, reply_rx) = std_mpsc::channel();
        if self.cmd_tx.send(SinkCmd::Submit { batch: batch.clone(), reply: reply_tx }).is_err() {
            return Ok(SinkOutcome::LinkDown);
        }
        match reply_rx.recv() {
            Ok(Ok(verdict)) => Ok(SinkOutcome::Delivered(verdict)),
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(SinkOutcome::LinkDown),
        }
    }
}

/// Sleeps until `due_ns` (per `cfg.pacing.due_at`, a pure function of epochs) is due,
/// relative to `start` -- the one place in this whole path that ever calls
/// `std::thread::sleep` (this file's own module doc). Shared by both the direct and the
/// `--buffer-dir` submission loops in `run` so pacing behaves identically either way.
fn sleep_until_due(due_ns: i64, start: Instant) {
    if due_ns > 0 {
        let due_at = start + Duration::from_nanos(due_ns as u64);
        let now = Instant::now();
        if due_at > now {
            std::thread::sleep(due_at - now);
        }
    }
}

async fn run(args: Args) -> Result<serde_json::Value, String> {
    let run_products = load_run_products(&args.run_products)?;
    let log = load_verified_port_traffic_log(&args.port_traffic, &run_products.port_traffic_hash)?;
    let mut cfg = load_plugin_config(&args.plugin_config)?;
    let signing_key = load_signing_key(&args.signing_key)?;

    // Step 3 ("What it does"): verify this process's own identity, if asked to, BEFORE a
    // single batch is built/signed below -- see `verify_own_identity`'s own doc comment.
    let identity = verify_own_identity(&args, &mut cfg)?;

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
    if let Some(identity) = &identity {
        client.set_forwarded_client_cert_if_plaintext(&identity.leaf_pem);
    }
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

    match &args.buffer_dir {
        None => {
            for (index, batch) in batches.into_iter().enumerate() {
                sleep_until_due(cfg.pacing.due_at(index, first_epoch, batch.batch_tai_ns), start);
                let mut one = client.submit(vec![batch]).await?;
                verdicts.append(&mut one);
            }
        }
        Some(buffer_dir) => {
            let buffer_path = buffer_dir.join(format!("{}.buflog", cfg.producer_id));
            let (edge_buffer, _recovery) = EdgeBuffer::open(&buffer_path).map_err(|e| format!("opening --buffer-dir buffer at {buffer_path:?}: {e}"))?;
            let sink = PluginBatchSink::new(client);
            let mut driver = UplinkDriver::new(sink, edge_buffer);
            for (index, batch) in batches.into_iter().enumerate() {
                sleep_until_due(cfg.pacing.due_at(index, first_epoch, batch.batch_tai_ns), start);
                let now_tai_ns = batch.batch_tai_ns;
                match driver.step(&batch, now_tai_ns).map_err(|e| format!("buffered submit (via --buffer-dir) failed: {e:?}"))? {
                    StepOutcome::Delivered { mut replayed, verdict } => {
                        verdicts.append(&mut replayed);
                        verdicts.push(verdict);
                    }
                    StepOutcome::Buffered { mut replayed } => {
                        verdicts.append(&mut replayed);
                    }
                }
            }
        }
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
        "identity_verified": identity.is_some(),
        "subject_cn": identity.as_ref().map(|i| i.subject_cn.clone()),
        "fingerprint_sha256": identity.as_ref().map(|i| i.fingerprint_sha256.clone()),
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

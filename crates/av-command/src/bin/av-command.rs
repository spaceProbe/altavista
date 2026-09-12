//! Boots the `av-command` service binary: `altavista.v1.CommandAuthorityService` (gRPC) and
//! `/admin/api/evidence`(`/verify`) (HTTP), both plaintext on loopback only (question 155;
//! see `crate::service`'s module doc for the exact refusal). Mirrors
//! `crates/av-dynamics-service/src/bin/server.rs`'s shape: warm up durable state first (here,
//! open the ledger and load the policy bundle), start the admin server, then serve gRPC.
//!
//! # Configuration (question 199: never the process environment)
//!
//! Every setting below comes from a command-line flag (with a fixed default), read once via
//! `std::env::args()` (the process's argument vector -- not an environment *variable*; this
//! binary never calls `std::env::var`/`set_var`/`remove_var` anywhere). There is no
//! environment-variable configuration path to accidentally rely on or accidentally mutate.
//!
//! ```text
//! av-command --oidc-issuer ISS --oidc-audience AUD --oidc-public-key-path PATH
//!            [--bind ADDR] [--admin-bind ADDR] [--ledger-dir PATH] [--policy-dir PATH]
//!            [--rate-window-ns NS] [--run-id ID] [--profile-path PATH]
//! ```
//!
//! - `--profile-path` (default `<repo>/profiles/execution.yaml`, **A2.2, new this task**):
//!   the profile whose `authority:` block (`roles`, `mfa_amr_methods`, `mfa_acr`,
//!   `delegations_path` -- [`av_command::authz::load_profile_authz_config`]) and top-level
//!   `audit:` block (`sink_path` -- [`av_command::audit::load_profile_audit_config`]) this
//!   binary reads once, at startup. `delegations_path`, when non-empty, is resolved relative
//!   to this crate's repo root (`<CARGO_MANIFEST_DIR>/../..`), matching `--policy-dir`'s own
//!   repo-relative convention.
//!
//! - `--bind` (default `127.0.0.1:50070`): the gRPC listen address. Refused at startup
//!   (`crate::service::resolve_loopback_bind_address`, a typed [`av_command::service::
//!   BindAddressError`]) unless it is a recognized loopback spelling -- question 155.
//! - `--admin-bind` (default `127.0.0.1:50170`, `av-dynamics-service`'s own +100-from-gRPC-
//!   port convention): same loopback-only refusal, same reason -- the admin surface is never
//!   fronted by nginx/mTLS either (ADR-004 question 63's own scope, matching
//!   `crates/av-dynamics-service/src/bin/server.rs`'s identical comment).
//! - `--ledger-dir` (default `<CARGO_MANIFEST_DIR>/ledger`, i.e. next to this crate, the same
//!   "next to the crate" convention `av-dynamics-service`'s evidence log default uses):
//!   where [`av_command::ledger::Ledger::open`] persists every partition.
//! - `--policy-dir` (default `<repo>/profiles/policies/authority`, `profiles/execution.yaml`'s
//!   own `authority.policy_dir`): where [`av_command::policy::PolicyBundle::load`] reads
//!   `.rego` files from.
//! - `--rate-window-ns` (default `3_600_000_000_000`, one hour -- `profiles/execution.yaml`'s
//!   own `authority.rate_window_ns`): the trailing window `PolicyInputRate.counts_by_class`
//!   is computed over.
//! - `--run-id` (default: a random 16-byte OpenSSL-RNG hex string, matching
//!   `crates/av-dynamics-service/src/bin/server.rs`'s own `random_run_id`): correlates this
//!   process's own admin responses; never parsed, only compared for equality.
//! - `--oidc-issuer`, `--oidc-audience`, `--oidc-public-key-path` (**A2.1, all three
//!   required -- no default of any kind**, `parse_args` refuses to return `Ok` without every
//!   one of them): the OIDC issuer string, the audience string, and the path to that
//!   issuer's public key (PEM), read once here and handed to
//!   [`av_command::oidc::IssuerConfig::from_public_key_pem`] -- `Authorize` verifies every
//!   `principal_token` against exactly this configuration (`crate::service`'s module doc,
//!   "A2.1: `Authorize` verifies *who*, not *whether*"). There being no default is
//!   deliberate: a default issuer/key would either be a real secret baked into this binary
//!   (never acceptable) or a placeholder that would silently accept tokens signed by a key
//!   nobody controls in production -- refusing to start without an operator-supplied
//!   configuration is the correct failure mode.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_command::audit::AuditWriter;
use av_command::authz::{load_delegations, load_profile_authz_config, RoleTable};
use av_command::clock::SystemClock;
use av_command::evidence::AdminState;
use av_command::ledger::Ledger;
use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
use av_command::policy::PolicyBundle;
use av_command::service::{resolve_loopback_bind_address, AuthzConfig, CommandAuthorityServiceImpl, RecordingDispatchSink};

const DEFAULT_BIND: &str = "127.0.0.1:50070";
/// `av-dynamics-service`'s own +100-from-gRPC-port convention (`crates/av-dynamics-service/
/// src/bin/server.rs`'s `DEFAULT_ADMIN_PORT`).
const DEFAULT_ADMIN_BIND: &str = "127.0.0.1:50170";
const DEFAULT_RATE_WINDOW_NS: i64 = 3_600_000_000_000;

struct Args {
    bind: String,
    admin_bind: String,
    ledger_dir: PathBuf,
    policy_dir: PathBuf,
    rate_window_ns: i64,
    run_id: Option<String>,
    /// A2.1: the OIDC issuer `Authorize` verifies `principal_token` against. All three are
    /// required (question 199: no environment-variable fallback, and no silently-weak
    /// default that would let this service start with authentication effectively
    /// disabled) -- `parse_args` refuses to return `Ok` without all three set.
    oidc_issuer: Option<String>,
    oidc_audience: Option<String>,
    oidc_public_key_path: Option<PathBuf>,
    /// A2.2: the profile this binary reads its `authority:` role/MFA/delegation config and
    /// its top-level `audit:` sink config from.
    profile_path: PathBuf,
}

fn default_ledger_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ledger")
}

fn default_policy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

fn default_profile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../profiles/execution.yaml")
}

/// This crate's repo root (`<CARGO_MANIFEST_DIR>/../..`) -- `--policy-dir`'s own default is
/// already repo-relative the same way; A2.2's `delegations_path` is resolved against this.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const USAGE: &str = "usage: av-command --oidc-issuer ISS --oidc-audience AUD \
                      --oidc-public-key-path PATH [--bind ADDR] [--admin-bind ADDR] \
                      [--ledger-dir PATH] [--policy-dir PATH] [--rate-window-ns NS] \
                      [--run-id ID] [--profile-path PATH]";

fn parse_args() -> Result<Args, String> {
    let mut bind = DEFAULT_BIND.to_string();
    let mut admin_bind = DEFAULT_ADMIN_BIND.to_string();
    let mut ledger_dir = default_ledger_dir();
    let mut policy_dir = default_policy_dir();
    let mut rate_window_ns = DEFAULT_RATE_WINDOW_NS;
    let mut run_id = None;
    let mut oidc_issuer = None;
    let mut oidc_audience = None;
    let mut oidc_public_key_path = None;
    let mut profile_path = default_profile_path();

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--bind" => bind = val()?,
            "--admin-bind" => admin_bind = val()?,
            "--ledger-dir" => ledger_dir = PathBuf::from(val()?),
            "--policy-dir" => policy_dir = PathBuf::from(val()?),
            "--rate-window-ns" => rate_window_ns = val()?.parse::<i64>().map_err(|e| format!("--rate-window-ns: {e}"))?,
            "--run-id" => run_id = Some(val()?),
            "--oidc-issuer" => oidc_issuer = Some(val()?),
            "--oidc-audience" => oidc_audience = Some(val()?),
            "--oidc-public-key-path" => oidc_public_key_path = Some(PathBuf::from(val()?)),
            "--profile-path" => profile_path = PathBuf::from(val()?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    if oidc_issuer.is_none() || oidc_audience.is_none() || oidc_public_key_path.is_none() {
        return Err(format!(
            "--oidc-issuer, --oidc-audience and --oidc-public-key-path are all required \
             (A2.1: Authorize verifies principal_token against a real issuer; there is no \
             default issuer to fall back to). {USAGE}"
        ));
    }
    Ok(Args { bind, admin_bind, ledger_dir, policy_dir, rate_window_ns, run_id, oidc_issuer, oidc_audience, oidc_public_key_path, profile_path })
}

/// 16 bytes from OpenSSL's RNG, hex-encoded -- see `crates/av-dynamics-service/src/bin/
/// server.rs`'s identical `random_run_id` for why this is not a bundled `rand`/`uuid` crate
/// and not a literal RFC 4122 UUID.
fn random_run_id() -> String {
    let mut buf = [0u8; 16];
    openssl::rand::rand_bytes(&mut buf).expect("OpenSSL RNG failed");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-command: {e}");
            std::process::exit(1);
        }
    };
    let run_id = args.run_id.unwrap_or_else(random_run_id);

    // Question 155: refused here, before any socket is ever bound -- never a partially
    // started process listening on a bad address.
    let grpc_addr: SocketAddr = resolve_loopback_bind_address(&args.bind).map_err(|e| {
        eprintln!("av-command: {e}");
        e
    })?;
    let admin_addr: SocketAddr = resolve_loopback_bind_address(&args.admin_bind).map_err(|e| {
        eprintln!("av-command: {e}");
        e
    })?;

    let ledger = Arc::new(Ledger::open(&args.ledger_dir)?);
    let bundle = Arc::new(PolicyBundle::load(&args.policy_dir).map_err(|e| format!("loading policy bundle from {:?}: {e}", args.policy_dir))?);
    eprintln!(
        "av-command: ledger at {} ({} partition(s)); policy bundle {:?} (hash {})",
        args.ledger_dir.display(),
        ledger.partitions()?.len(),
        args.policy_dir,
        bundle.policy_hash()
    );

    // A2.1: parsed once, at startup -- the one place this binary reads the issuer's public
    // key from disk. crate::oidc::verify itself does no I/O of any kind (its own module doc,
    // "Purity"); this is the caller "being handed a key" that doc describes.
    let oidc_issuer = args.oidc_issuer.expect("parse_args refuses to return Ok without --oidc-issuer");
    let oidc_audience = args.oidc_audience.expect("parse_args refuses to return Ok without --oidc-audience");
    let oidc_public_key_path = args.oidc_public_key_path.expect("parse_args refuses to return Ok without --oidc-public-key-path");
    let oidc_public_key_pem = std::fs::read(&oidc_public_key_path).map_err(|e| format!("reading --oidc-public-key-path {oidc_public_key_path:?}: {e}"))?;
    let issuer_config = Arc::new(
        av_command::oidc::IssuerConfig::from_public_key_pem(&oidc_issuer, &oidc_audience, &oidc_public_key_pem)
            .map_err(|e| format!("--oidc-public-key-path {oidc_public_key_path:?}: {e}"))?,
    );
    eprintln!("av-command: OIDC issuer {oidc_issuer:?}, audience {oidc_audience:?} (RS256)");

    // A2.2: the profile's authority.roles/mfa_amr_methods/mfa_acr/delegations_path and
    // top-level audit.sink_path, read once at startup from the same profile file --
    // resolved (never assumed) to give a clear error if it does not parse.
    let profile_text = std::fs::read_to_string(&args.profile_path).map_err(|e| format!("reading --profile-path {:?}: {e}", args.profile_path))?;
    let authz_config = load_profile_authz_config(&profile_text).map_err(|e| format!("parsing authority: block of {:?}: {e}", args.profile_path))?;
    let audit_config = av_command::audit::load_profile_audit_config(&profile_text).map_err(|e| format!("parsing audit: block of {:?}: {e}", args.profile_path))?;

    let role_table = Arc::new(RoleTable::from_config(&authz_config.roles));
    let delegations = Arc::new(
        load_delegations(&repo_root(), &authz_config.delegations_path)
            .map_err(|e| format!("loading delegations_path {:?}: {e}", authz_config.delegations_path))?,
    );
    let audit = Arc::new(AuditWriter::open(&audit_config)?);
    eprintln!(
        "av-command: {} role(s), {} delegation(s), audit sink {:?}",
        authz_config.roles.len(),
        delegations.all().count(),
        audit_config
    );

    let admin_state = Arc::new(AdminState { ledger: ledger.clone(), run_id: run_id.clone(), version: env!("CARGO_PKG_VERSION").to_string() });
    eprintln!("av-command: admin API on {admin_addr} (GET /admin/api/evidence, /admin/api/evidence/verify)");
    tokio::spawn(async move {
        if let Err(e) = av_command::admin::serve(admin_addr, admin_state).await {
            eprintln!("av-command: admin server failed: {e}");
        }
    });

    // A3 (docs/aiplane-plan.md) supplies the real kernel telecommand binding behind
    // DispatchSink; this binary ships only the recording implementation (crate::service's
    // module doc, "DispatchSink -- not A3").
    let dispatch_sink = Arc::new(RecordingDispatchSink::new());
    let authz = AuthzConfig {
        role_table,
        delegations,
        mfa_amr_methods: Arc::new(authz_config.mfa_amr_methods),
        mfa_acr: Arc::new(authz_config.mfa_acr),
        audit,
    };
    // Rebuilds the duplicate-dispatch guard from the ledger before serving a single RPC --
    // see crate::service's module doc, "Idempotency ... a guarantee that survives a restart".
    let servicer = CommandAuthorityServiceImpl::new(ledger, bundle, args.rate_window_ns, Arc::new(SystemClock), dispatch_sink, issuer_config, authz)?;

    eprintln!("av-command: listening on {grpc_addr} (run_id={run_id})");
    tonic::transport::Server::builder()
        .add_service(CommandAuthorityServiceServer::new(servicer))
        .serve(grpc_addr)
        .await?;
    Ok(())
}

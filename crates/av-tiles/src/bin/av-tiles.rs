//! `av-tiles` -- H4's binary. Serves `GET /v1/tilesets/{manifest_sha256}/manifest` and
//! `GET /v1/tilesets/{manifest_sha256}/tiles/{level}/{x}/{y}` over plain HTTP on
//! [`DEFAULT_BIND`] (or `--bind`), against a real `av-store`-backed `StoreObjectSource`.
//!
//! Every knob is a command-line argument (question 199's rule: no test may mutate the
//! process environment, and an env-var-configured binary invites exactly that) -- never an
//! environment variable, mirroring `crates/av-gateway/src/bin/av-gateway.rs`'s identical
//! convention. `--oidc-issuer`/`--oidc-audience`/`--oidc-public-key-path` are all REQUIRED,
//! no default (R5.1/question 208(b), restated for this crate: every caller of this surface
//! is authenticated; there is no code path that reaches `av_tiles::server::serve` with no
//! issuer configured).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use av_command::clock::SystemClock;
use av_command::oidc::IssuerConfig;
use av_command::service::{resolve_internal_network_bind_address, resolve_loopback_bind_address};
use av_label::{ClearanceLadder, GroupClearanceMap};
use av_store::{StoreClient, StoreConfig};
use av_tiles::config::TilesConfig;
use av_tiles::source::StoreObjectSource;

/// Question 208(c)/219(a): `docs/architecture.md` section 4, "Default ports", is the one
/// owned port map for every service's default bind in this workspace -- this is `av-tiles`'
/// own row. Round 2's own status recorded "no admin surface, hence no `+100` counterpart";
/// H5b-1 (round 3) adds one, OPTIONAL and off by default -- see [`av_tiles::admin`]'s own
/// module doc for why, and [`av_tiles::admin::DEFAULT_ADMIN_BIND`] for its own `+100` row
/// (not yet added to `docs/architecture.md` itself -- that file is shared across tracks; an
/// open item for the lead, the identical posture the round-2 status already took for this
/// crate's own main-port row).
const DEFAULT_BIND: &str = "127.0.0.1:50073";

const USAGE: &str = "usage: av-tiles --oidc-issuer ISS --oidc-audience AUD --oidc-public-key-path PATH \
                      --ladder MARKING[,MARKING...] --key-prefix PREFIX \
                      --store-endpoint URL --store-region REGION --store-access-key-id ID \
                      --store-secret-access-key KEY --store-bucket BUCKET \
                      [--store-path-style] [--store-ca-file PATH] \
                      [--group-clearance GROUP=MARKING]... [--bind ADDR] [--admin-bind ADDR] \
                      [--admin-role ROLE]... [--internal-network-bind]";

#[derive(Debug)]
struct CliArgs {
    oidc_issuer: Option<String>,
    oidc_audience: Option<String>,
    oidc_public_key_path: Option<PathBuf>,
    ladder: Option<Vec<String>>,
    key_prefix: Option<String>,
    store_endpoint: Option<String>,
    store_region: Option<String>,
    store_access_key_id: Option<String>,
    store_secret_access_key: Option<String>,
    store_bucket: Option<String>,
    store_path_style: bool,
    store_ca_file: Option<PathBuf>,
    group_clearance: BTreeMap<String, String>,
    bind: String,
    /// H5b-1: the OPTIONAL admin surface's own bind (`crate::admin::serve`, `GET /admin/api/
    /// counters`) -- `None` (this struct's own default) means "no admin surface at all",
    /// never a silently-always-on listener a deployment did not ask for.
    admin_bind: Option<String>,
    /// Question 232 (lead, round 5 gate): the explicit opt-in that lets BOTH binds leave
    /// loopback, resolved through `resolve_internal_network_bind_address` instead of
    /// `resolve_loopback_bind_address` -- the same opt-in `av-gateway` has for its
    /// `--internal-network-bind ADDR` (question 206 decision 8), shaped as a switch here
    /// because this binary's two surfaces sit behind the same token check and a container
    /// reached through Docker port mapping needs both on its own interface. Off (this
    /// struct's own default) means the question 155 loopback rule applies unchanged.
    internal_network_bind: bool,
    /// Round 5, item B: OIDC `groups` entries granted `av_tiles::admin::SURFACE`
    /// (`"admin_counters"`) -- an empty `Vec` (this struct's own default) denies every
    /// caller, `av_command::authz::RoleTable::granting_role`'s own deny-by-default
    /// guarantee, exactly like every other role table in this workspace. Repeatable
    /// (`--admin-role admins --admin-role operators`), mirroring `--group-clearance`'s own
    /// accumulate-by-repetition shape, just below.
    admin_roles: Vec<String>,
}

fn parse_cli_args(args: impl Iterator<Item = String>) -> Result<CliArgs, String> {
    let mut out = CliArgs {
        oidc_issuer: None,
        oidc_audience: None,
        oidc_public_key_path: None,
        ladder: None,
        key_prefix: None,
        store_endpoint: None,
        store_region: None,
        store_access_key_id: None,
        store_secret_access_key: None,
        store_bucket: None,
        store_path_style: false,
        store_ca_file: None,
        group_clearance: BTreeMap::new(),
        bind: DEFAULT_BIND.to_string(),
        admin_bind: None,
        internal_network_bind: false,
        admin_roles: Vec::new(),
    };

    let mut args = args.skip(1).peekable();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value. {USAGE}"));
        match flag.as_str() {
            "--oidc-issuer" => out.oidc_issuer = Some(value()?),
            "--oidc-audience" => out.oidc_audience = Some(value()?),
            "--oidc-public-key-path" => out.oidc_public_key_path = Some(PathBuf::from(value()?)),
            "--ladder" => out.ladder = Some(value()?.split(',').map(str::to_string).collect()),
            "--key-prefix" => out.key_prefix = Some(value()?),
            "--store-endpoint" => out.store_endpoint = Some(value()?),
            "--store-region" => out.store_region = Some(value()?),
            "--store-access-key-id" => out.store_access_key_id = Some(value()?),
            "--store-secret-access-key" => out.store_secret_access_key = Some(value()?),
            "--store-bucket" => out.store_bucket = Some(value()?),
            "--store-path-style" => out.store_path_style = true,
            "--store-ca-file" => out.store_ca_file = Some(PathBuf::from(value()?)),
            "--bind" => out.bind = value()?,
            "--admin-bind" => out.admin_bind = Some(value()?),
            "--admin-role" => out.admin_roles.push(value()?),
            "--internal-network-bind" => out.internal_network_bind = true,
            "--group-clearance" => {
                let raw = value()?;
                let (group, marking) = raw.split_once('=').ok_or_else(|| format!("--group-clearance value {raw:?} must be GROUP=MARKING. {USAGE}"))?;
                out.group_clearance.insert(group.to_string(), marking.to_string());
            }
            other => return Err(format!("unrecognised argument {other:?}. {USAGE}")),
        }
    }

    if out.oidc_issuer.is_none() || out.oidc_audience.is_none() || out.oidc_public_key_path.is_none() {
        return Err(format!(
            "--oidc-issuer, --oidc-audience and --oidc-public-key-path are all required (R5.1/question \
             208(b): every caller of this surface is authenticated; there is no default issuer to fall \
             back to). {USAGE}"
        ));
    }
    if out.ladder.is_none() || out.key_prefix.is_none() {
        return Err(format!("--ladder and --key-prefix are both required. {USAGE}"));
    }
    if out.store_endpoint.is_none() || out.store_region.is_none() || out.store_access_key_id.is_none() || out.store_secret_access_key.is_none() || out.store_bucket.is_none() {
        return Err(format!("--store-endpoint, --store-region, --store-access-key-id, --store-secret-access-key and --store-bucket are all required. {USAGE}"));
    }
    Ok(out)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cli = parse_cli_args(std::env::args()).unwrap_or_else(|e| {
        eprintln!("av-tiles: {e}");
        std::process::exit(1);
    });

    // Round 5, item C (question 229's second open ruling; round 4 defect 6): every other
    // bind address in this workspace (`av-command --bind`/`--admin-bind`, `av-gateway
    // --bind`/`--admin-bind`/`--command-admin-bind`) is resolved through
    // `av_command::service::resolve_loopback_bind_address` (question 155) before any
    // `TcpListener::bind` call is ever reached -- `av-tiles` enforced no such restriction at
    // all, the one sibling that didn't. Both this crate's own binds (`--bind`, the main tile-
    // serving port, and `--admin-bind`, H5b-1's optional counters route) now go through the
    // identical function, with the identical typed `BindAddressError` and the identical
    // "refused before any socket is bound" ordering `av-command`'s own binary already has
    // (see that binary's `main()`, lines ~186-190).
    // Question 232: `--internal-network-bind` is the one explicit way off loopback (see the
    // `CliArgs` field's own comment); without it the question 155 rule stands.
    let resolve_bind = if cli.internal_network_bind { resolve_internal_network_bind_address } else { resolve_loopback_bind_address };
    let bind: SocketAddr = resolve_bind(&cli.bind).unwrap_or_else(|e| {
        eprintln!("av-tiles: --bind {:?}: {e}", cli.bind);
        std::process::exit(1);
    });

    let public_key_path = cli.oidc_public_key_path.clone().expect("parse_cli_args refuses to return Ok without --oidc-public-key-path");
    let public_key_pem = std::fs::read(&public_key_path).unwrap_or_else(|e| {
        eprintln!("av-tiles: reading --oidc-public-key-path {public_key_path:?}: {e}");
        std::process::exit(1);
    });
    let issuer_config = Arc::new(
        IssuerConfig::from_public_key_pem(cli.oidc_issuer.expect("checked above"), cli.oidc_audience.expect("checked above"), &public_key_pem).unwrap_or_else(|e| {
            eprintln!("av-tiles: --oidc-public-key-path {public_key_path:?}: {e}");
            std::process::exit(1);
        }),
    );

    let ladder = Arc::new(ClearanceLadder::new(cli.ladder.expect("checked above")));
    let group_clearance = Arc::new(GroupClearanceMap::new(cli.group_clearance));
    let config = Arc::new(TilesConfig::new(cli.key_prefix.expect("checked above"), issuer_config, ladder.clone(), group_clearance));

    let store_config = StoreConfig {
        endpoint: cli.store_endpoint.expect("checked above").parse().unwrap_or_else(|e| {
            eprintln!("av-tiles: --store-endpoint: {e}");
            std::process::exit(1);
        }),
        region: cli.store_region.expect("checked above"),
        access_key_id: cli.store_access_key_id.expect("checked above"),
        secret_access_key: cli.store_secret_access_key.expect("checked above"),
        bucket: cli.store_bucket.expect("checked above"),
        force_path_style: cli.store_path_style,
        ca_file: cli.store_ca_file,
        key_prefix: config.key_prefix().to_string(),
    };
    let store_client = Arc::new(StoreClient::new(store_config).unwrap_or_else(|e| {
        eprintln!("av-tiles: building the store client: {e}");
        std::process::exit(1);
    }));
    let clock: Arc<dyn av_command::clock::Clock> = Arc::new(SystemClock);
    let source = Arc::new(StoreObjectSource::new(store_client, ladder, clock.clone()));
    let counters = Arc::new(av_tiles::counters::Counters::new());

    // H5b-1: the OPTIONAL admin surface, on its own bind, spawned before the "LISTENING"
    // line so a caller polling for that line never observes a window where the main port is
    // up but the admin one (if configured) is not -- mirrors `crates/av-command/src/bin/
    // av-command.rs`'s own ordering for its own `--admin-bind`.
    if let Some(admin_bind_raw) = &cli.admin_bind {
        let admin_bind: SocketAddr = resolve_bind(admin_bind_raw).unwrap_or_else(|e| {
            eprintln!("av-tiles: --admin-bind {admin_bind_raw:?}: {e}");
            std::process::exit(1);
        });
        // Round 5, item B: the SAME issuer configuration this deployment already loaded for
        // the main port (`config.issuer_config`, `av_tiles::config::TilesConfig`'s own `pub`
        // field) -- never a second, independently-loaded issuer for this one route. Every
        // `--admin-role` flag grants `av_tiles::admin::SURFACE` ("admin_counters") only -- the
        // one surface this route has -- an empty `admin_roles` (no `--admin-role` at all)
        // denies every caller, `RoleTable::granting_role`'s own deny-by-default guarantee.
        let admin_roles: BTreeMap<String, Vec<String>> = cli.admin_roles.iter().map(|role| (role.clone(), vec![av_tiles::admin::SURFACE.to_string()])).collect();
        let admin_auth = Arc::new(av_tiles::admin::AdminAuthContext::new(config.issuer_config.clone(), Arc::new(av_command::authz::RoleTable::from_config(&admin_roles))));
        let admin_state = Arc::new(av_tiles::admin::AdminState { counters: counters.clone(), auth: admin_auth, clock: clock.clone() });
        tokio::spawn(async move {
            if let Err(e) = av_tiles::admin::serve(admin_bind, admin_state).await {
                eprintln!("av-tiles: admin server error: {e}");
            }
        });
    }

    println!("av-tiles: LISTENING {bind}");
    if let Err(e) = av_tiles::server::serve(bind, config, source, counters, clock).await {
        eprintln!("av-tiles: server error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_ARGS: &[&str] = &[
        "av-tiles",
        "--oidc-issuer",
        "https://sso.test.example/",
        "--oidc-audience",
        "av-tiles",
        "--oidc-public-key-path",
        "/dev/null",
        "--ladder",
        "UNCLASSIFIED,CUI,SECRET",
        "--key-prefix",
        "imagery/2026",
        "--store-endpoint",
        "http://127.0.0.1:9000",
        "--store-region",
        "us-east-1",
        "--store-access-key-id",
        "id",
        "--store-secret-access-key",
        "secret",
        "--store-bucket",
        "altavista-heavy",
    ];

    fn args(extra: &[&str]) -> impl Iterator<Item = String> {
        REQUIRED_ARGS.iter().chain(extra.iter()).map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn default_bind_matches_the_owned_port_map_constant() {
        assert_eq!(DEFAULT_BIND, "127.0.0.1:50073");
    }

    #[test]
    fn every_required_flag_missing_is_refused() {
        let err = parse_cli_args(["av-tiles".to_string()].into_iter()).unwrap_err();
        assert!(err.contains("--oidc-issuer"), "{err}");
    }

    #[test]
    fn a_fully_specified_command_line_parses_and_defaults_bind() {
        let cli = parse_cli_args(args(&[])).unwrap();
        assert_eq!(cli.bind, DEFAULT_BIND);
        assert_eq!(cli.ladder, Some(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]));
        assert!(cli.group_clearance.is_empty());
    }

    #[test]
    fn an_explicit_bind_overrides_the_default() {
        let cli = parse_cli_args(args(&["--bind", "127.0.0.1:9999"])).unwrap();
        assert_eq!(cli.bind, "127.0.0.1:9999");
    }

    #[test]
    fn group_clearance_flags_accumulate_and_refuse_a_malformed_entry() {
        let cli = parse_cli_args(args(&["--group-clearance", "operators=CUI", "--group-clearance", "admins=SECRET"])).unwrap();
        assert_eq!(cli.group_clearance.get("operators").map(String::as_str), Some("CUI"));
        assert_eq!(cli.group_clearance.get("admins").map(String::as_str), Some("SECRET"));

        let err = parse_cli_args(args(&["--group-clearance", "no-equals-sign"])).unwrap_err();
        assert!(err.contains("GROUP=MARKING"), "{err}");
    }

    #[test]
    fn admin_role_flags_accumulate_and_default_to_empty() {
        let cli = parse_cli_args(args(&[])).unwrap();
        assert!(cli.admin_roles.is_empty(), "no --admin-role at all must deny every caller by default, never grant one implicitly");

        let cli = parse_cli_args(args(&["--admin-role", "admins", "--admin-role", "operators"])).unwrap();
        assert_eq!(cli.admin_roles, vec!["admins".to_string(), "operators".to_string()]);
    }

    // Round 5, item C: `parse_cli_args` only ever stores `--bind`/`--admin-bind` as raw
    // `String`s (see `CliArgs` above) -- `main()` itself is the only place either one is
    // actually resolved to a `SocketAddr`, and `main()` calls `std::process::exit` on
    // refusal, which is not a seam `#[test]` can call directly (mirrors every other binary
    // on this track: `av-command`'s own acceptance test, `crates/av-command/tests/
    // grpc_service.rs::non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_
    // question_155`, calls `resolve_loopback_bind_address` directly too, never `main()`
    // itself). This test pins the one thing that IS testable without a process boundary:
    // that `resolve_loopback_bind_address` -- the exact function `main()` now calls for
    // BOTH `cli.bind` and `cli.admin_bind` (see `main()`, above) -- refuses every
    // non-loopback raw address this crate's own CLI could otherwise be pointed at, and
    // accepts every loopback spelling `--bind`/`--admin-bind` legitimately take, including
    // this crate's own real defaults. A table of refused/accepted addresses, in the style of
    // `grpc_service.rs`'s own acceptance test 8 -- this is the identical, already-reviewed
    // function under test, not a reimplementation of its logic.
    #[test]
    fn resolve_loopback_bind_address_refuses_non_loopback_and_accepts_the_spellings_this_binarys_own_flags_use() {
        for raw in ["0.0.0.0:50073", "0.0.0.0:50173", "10.1.2.3:50073", "example.com:50073", "[::]:50073"] {
            let err = resolve_loopback_bind_address(raw).expect_err(&format!("{raw:?} must be refused"));
            assert!(err.to_string().contains(raw), "{err}");
        }

        // Every loopback spelling this binary's own `--bind`/`--admin-bind` legitimately
        // take, including this crate's own real DEFAULT_BIND/DEFAULT_ADMIN_BIND constants --
        // a regression that broke either default would be caught here, not just by the CLI
        // parsing tests above (which never call the resolver at all).
        for raw in [DEFAULT_BIND, av_tiles::admin::DEFAULT_ADMIN_BIND, "127.0.0.1:9999", "localhost:50073", "[::1]:50073"] {
            resolve_loopback_bind_address(raw).unwrap_or_else(|e| panic!("{raw:?} must be accepted: {e}"));
        }
    }

    #[test]
    fn an_unrecognised_flag_is_refused() {
        let err = parse_cli_args(args(&["--not-a-real-flag"])).unwrap_err();
        assert!(err.contains("unrecognised"), "{err}");
    }
}

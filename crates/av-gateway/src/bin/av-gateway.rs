//! The `av-gateway` binary: serves `DataGatewayService` over gRPC on loopback (D7 -- reuses
//! `av_command::service::resolve_loopback_bind_address`/`BindAddressError` directly rather
//! than a second copy, since this crate already depends on `av-command`; question 155's
//! refusal of a non-loopback bind is therefore the exact same typed check
//! `crates/av-command/src/bin/av-command.rs` uses) and, on the same process, an MCP
//! JSON-RPC-2.0-over-stdio server (D3) on the real process stdin/stdout -- the ONE call
//! site in this crate that touches them directly; every test drives
//! [`av_gateway::mcp::McpHandler`]/[`av_gateway::mcp::serve`] over injected streams instead
//! (question 199).
//!
//! This binary's own wiring (which run products populate the catalogue, which clearance
//! ladder, which `CommandAuthorityService` endpoint to dial) is deliberately minimal --
//! acceptance for this task is the library crate's behaviour, proven by
//! `cargo test -p av-gateway`, not this binary's own CLI surface. It exists so `cargo build
//! -p av-gateway` produces a runnable artifact, matching every other service crate in this
//! workspace.
//!
//! # R3.3: two additive CLI flags (`docs/aiplane-plan.md` milestone A4;
//! `docs/open-questions.md` question 206's decision 11(a))
//!
//! Both are flags, never environment defaults (question 199's rule, applied at this parsing
//! boundary) -- every pre-existing environment-variable default above (`AV_GATEWAY_BIND`,
//! `AV_GATEWAY_CLEARANCE_LADDER`, `AV_GATEWAY_EVIDENCE_LEDGER_DIR`,
//! `AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT`) is untouched and stays the default when the
//! corresponding flag is absent -- an explicit flag only ever takes precedence, never removes
//! the env-based path this binary already had.
//!
//! - `--internal-network-bind <ADDR>`: when present, THIS address (through
//!   [`av_command::service::resolve_internal_network_bind_address`], never
//!   `resolve_loopback_bind_address`) is what this binary's gRPC surface
//!   (`DataGatewayService` + `ModelProposeService`) binds, and `AV_GATEWAY_BIND` is not
//!   consulted at all. Named for exactly the one deployment shape it exists for: a container
//!   on a `docker network create --internal` segment, where the container's own address is
//!   either not known before it starts (`0.0.0.0`) or is a private literal a deployment
//!   already knows -- see [`av_command::service::resolve_internal_network_bind_address`]'s
//!   own doc for the full "why this is safe here and nowhere else" argument.
//! - `--run-products <PATH>:<MARKING>` (repeatable): loads a real, on-disk `RunProducts` file
//!   (`prost`-encoded, e.g. `RunProducts::encode_to_vec()`'s own output -- exactly the shape
//!   every committed `tests/fixtures/*.runproducts.bin` fixture already is) into this
//!   process's own catalogue ([`av_gateway::catalogue::RunCatalogue`], D1), under the declared
//!   `MARKING` (D2's own `av_cdm::pb::Label::marking`) -- through
//!   [`av_gateway::catalogue::CatalogueEntry::from_run_products`], never a second, ad hoc way
//!   of building an entry. `PATH:MARKING` splits on the LAST `:` (mirroring
//!   `--run-products`'s own repeatable-flag convention: a path is vanishingly unlikely to end
//!   in `:MARKING` for any real marking string, and every marking this workspace actually uses
//!   -- `UNCLASSIFIED`/`CUI`/`SECRET` -- contains no `:` at all). Repeated for more than one
//!   run; the catalogue key is each file's own `RunProducts.run_id` (D1: "a caller names a run
//!   by identity, never a path"), so two `--run-products` flags naming files with the SAME
//!   `run_id` silently keep only the later one -- exactly [`std::collections::BTreeMap::
//!   insert`]'s own documented behaviour, not a special case this binary adds. When this flag
//!   is absent (as it always was before this round), the catalogue is empty, exactly as
//!   before.
//!
//! # R3.6/A6, Part 3: the evidence-bundle admin surface (two environment-variable defaults,
//! not flags -- matching every OTHER address this binary already configures this way, e.g.
//! `AV_GATEWAY_BIND`/`AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT` above)
//!
//! - `AV_GATEWAY_ADMIN_BIND` (default `"127.0.0.1:50171"`): where THIS process's own
//!   `GET /admin/api/evidence/bundle` route listens ([`av_gateway::admin::serve`]), refused at
//!   startup if non-loopback (question 155, [`resolve_loopback_bind_address`]).
//! - `AV_GATEWAY_COMMAND_ADMIN_BIND` (default empty = not configured): the running
//!   `av-command` service's own admin address (its own default is
//!   `crates/av-command/src/bin/av-command.rs::DEFAULT_ADMIN_BIND`, `"127.0.0.1:50170"`) --
//!   when absent, the bundle still serves, with the `av-command` side of the bundle honestly
//!   `{"reachable": false, ...}` rather than this binary refusing to start.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use av_cdm::pb::{Label, RunProducts};
use av_command::clock::SystemClock;
use av_command::ledger::Ledger;
use av_command::service::{resolve_internal_network_bind_address, resolve_loopback_bind_address};
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::gateway::{DataGatewayServiceImpl, DataGatewayServiceServer, GatewayCore};
use av_gateway::labels::ClearanceLadder;
use av_gateway::mcp::{serve, McpContext, McpHandler};
use av_gateway::propose_flow::ModelProposeServiceImpl;
use av_gateway::propose_only::ProposeOnlyAuthority;
use av_gateway::pb::model_propose_service_server::ModelProposeServiceServer;
use av_gateway::unknown_route_counter::UnknownRouteCounterLayer;
use av_gateway::evidence_bundle::BundleState;
use prost::Message as _;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

const USAGE: &str = "usage: av-gateway [--internal-network-bind ADDR] [--run-products PATH:MARKING]...";

/// This binary's own additive CLI surface (see the module doc's "R3.3" section) -- parsed
/// once, from `std::env::args()` (question 199: never an environment variable for either of
/// these two flags). Everything else this binary configures still comes from the
/// pre-existing environment-variable defaults, untouched by this struct.
#[derive(Debug, Default, PartialEq, Eq)]
struct CliArgs {
    internal_network_bind: Option<String>,
    run_products: Vec<String>,
}

fn parse_cli_args(mut args: impl Iterator<Item = String>) -> Result<CliArgs, String> {
    let _argv0 = args.next();
    let mut out = CliArgs::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} requires a value"));
        match flag.as_str() {
            "--internal-network-bind" => out.internal_network_bind = Some(value()?),
            "--run-products" => out.run_products.push(value()?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }
    Ok(out)
}

/// Splits one `--run-products` value on its LAST `:` (see the module doc for why the last,
/// not the first) into `(path, marking)`. Refuses (a `String` message, never a panic) when
/// either half would be empty.
fn parse_run_products_spec(spec: &str) -> Result<(PathBuf, String), String> {
    let (path, marking) = spec
        .rsplit_once(':')
        .ok_or_else(|| format!("--run-products {spec:?} must be PATH:MARKING (a path, a ':', and a non-empty marking)"))?;
    if path.is_empty() {
        return Err(format!("--run-products {spec:?}: the PATH half is empty"));
    }
    if marking.is_empty() {
        return Err(format!("--run-products {spec:?}: the MARKING half is empty"));
    }
    Ok((PathBuf::from(path), marking.to_string()))
}

/// Reads `path` off disk, decodes it as a `prost`-encoded `altavista.v1.RunProducts`, and
/// builds the `(run_id, CatalogueEntry)` pair [`build_catalogue_from_run_products_specs`]
/// inserts into the catalogue -- through [`CatalogueEntry::from_run_products`] (this task's
/// own named requirement), never a second, ad hoc way of building an entry.
fn load_catalogue_entry(path: &std::path::Path, marking: &str) -> Result<(String, CatalogueEntry), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading --run-products {path:?}: {e}"))?;
    let run_products = RunProducts::decode(bytes.as_slice()).map_err(|e| format!("decoding --run-products {path:?} as altavista.v1.RunProducts: {e}"))?;
    if run_products.run_id.is_empty() {
        return Err(format!("--run-products {path:?} decodes to an empty RunProducts.run_id -- the catalogue is keyed on run_id (D1) and cannot key an empty string"));
    }
    let label = Label { marking: marking.to_string(), caveats: vec![] };
    let run_id = run_products.run_id.clone();
    Ok((run_id, CatalogueEntry::from_run_products(label, &run_products)))
}

/// Builds this process's own catalogue from every `--run-products PATH:MARKING` the operator
/// passed, in order -- empty (exactly the pre-R3.3 default) when `specs` is empty.
fn build_catalogue_from_run_products_specs(specs: &[String]) -> Result<BTreeMap<String, CatalogueEntry>, String> {
    let mut entries = BTreeMap::new();
    for spec in specs {
        let (path, marking) = parse_run_products_spec(spec)?;
        let (run_id, entry) = load_catalogue_entry(&path, &marking)?;
        entries.insert(run_id, entry);
    }
    Ok(entries)
}

#[tokio::main]
async fn main() {
    let cli = parse_cli_args(std::env::args()).unwrap_or_else(|e| {
        eprintln!("av-gateway: {e}");
        std::process::exit(1);
    });

    // R3.3: an explicit --internal-network-bind flag takes precedence over AV_GATEWAY_BIND
    // and is resolved through the separately-named, separately-reasoned-about resolver -- see
    // this module's own doc and that function's doc for the full "why this is safe" argument.
    // AV_GATEWAY_BIND (and resolve_loopback_bind_address, question 155) is the untouched
    // default for every deployment that does not pass this flag.
    let bind_addr = if let Some(raw) = &cli.internal_network_bind {
        resolve_internal_network_bind_address(raw).unwrap_or_else(|e| {
            eprintln!("av-gateway: refusing to start: {e}");
            std::process::exit(1);
        })
    } else {
        // R3.6 (manager's review): this default was `127.0.0.1:50170`, which is
        // `crates/av-command/src/bin/av-command.rs::DEFAULT_ADMIN_BIND` -- so two services of
        // this same track, each started with nothing but its own defaults, fought over one
        // port and whichever lost failed to bind. `50071` restores the convention both
        // binaries already follow (`av-command` gRPC `50070`, admin `50070 + 100`), giving one
        // coherent map: av-command 50070/50170, av-gateway 50071/50171.
        let bind_raw = env_or("AV_GATEWAY_BIND", "127.0.0.1:50071");
        resolve_loopback_bind_address(&bind_raw).unwrap_or_else(|e| {
            eprintln!("av-gateway: refusing to start: {e}");
            std::process::exit(1);
        })
    };

    let ladder_raw = env_or("AV_GATEWAY_CLEARANCE_LADDER", "UNCLASSIFIED,CUI,SECRET");
    let ladder = ClearanceLadder::new(ladder_raw.split(',').map(str::to_string).collect());

    // R3.3: an explicit --run-products flag takes precedence over the pre-existing empty
    // default -- see this module's own doc for the exact PATH:MARKING shape.
    let catalogue = RunCatalogue::new(build_catalogue_from_run_products_specs(&cli.run_products).unwrap_or_else(|e| {
        eprintln!("av-gateway: {e}");
        std::process::exit(1);
    }));
    let counters = Arc::new(av_gateway::counters::Counters::new());
    let core = Arc::new(GatewayCore::new(catalogue, ladder, counters.clone()));

    let evidence_dir = env_or("AV_GATEWAY_EVIDENCE_LEDGER_DIR", "/tmp/av-gateway-evidence-ledger");
    let evidence_ledger = Arc::new(Ledger::open(&evidence_dir).unwrap_or_else(|e| {
        eprintln!("av-gateway: cannot open evidence ledger at {evidence_dir:?}: {e}");
        std::process::exit(1);
    }));

    // R3.6 (manager's review): this default was `http://127.0.0.1:50110`, an address
    // `av-command` has never listened on -- its own `DEFAULT_BIND` is `127.0.0.1:50070`. Two
    // services of this track started with nothing but their defaults therefore never found
    // each other, and because the fallback below is a LAZY channel the miss surfaced only
    // later, as a connect error on the first `propose_command`, rather than at startup: a
    // misconfiguration that leaves no trace until someone tries to use it. Pointed at
    // `av-command`'s real default.
    let authority_endpoint = env_or("AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT", "http://127.0.0.1:50070");
    let authority = match ProposeOnlyAuthority::connect(authority_endpoint.clone()).await {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("av-gateway: cannot dial CommandAuthorityService at {authority_endpoint:?}: {e} -- propose_command will be unavailable");
            // Still serve the read-only gRPC surface even when the command authority is
            // unreachable; a lazily-connecting channel keeps propose_command's own
            // refusal typed rather than making the whole process fail to start.
            let channel = tonic::transport::Endpoint::from_shared(authority_endpoint).unwrap().connect_lazy();
            Arc::new(ProposeOnlyAuthority::from_channel(channel))
        }
    };

    let clock: Arc<dyn av_command::clock::Clock> = Arc::new(SystemClock);
    // D1/A4b: ModelProposeService is served from this SAME `tonic::transport::Server` as
    // DataGatewayService below -- one process, one port, two `add_service` calls -- never a
    // second listening socket for the network propose path. Shares the identical `authority`/
    // `evidence_ledger`/`clock`/`counters` the MCP surface's `propose_command` tool uses, since
    // both call through the one shared `av_gateway::propose_flow::propose_command`.
    let model_propose = ModelProposeServiceImpl::new(authority.clone(), evidence_ledger.clone(), clock.clone(), counters.clone());
    // R3.2 acceptance evidence 1(b): a raw gRPC request naming a service this server does not
    // serve (e.g. CommandAuthorityService) must be refused AND counted, but tonic answers
    // Unimplemented for an unmatched path entirely inside its own generated router, before any
    // of this workspace's code runs -- see crate::unknown_route_counter's own module doc.
    let known_prefixes = vec!["/altavista.v1.DataGatewayService/".to_string(), "/altavista.v1.ModelProposeService/".to_string()];
    let unknown_route_layer = UnknownRouteCounterLayer::new(known_prefixes, counters.clone());

    // R3.6/A6, Part 3: the one call that collects both services' evidence into one bundle.
    // `AV_GATEWAY_COMMAND_ADMIN_BIND` is the running `av-command` service's own admin address
    // (its default, `crates/av-command/src/bin/av-command.rs::DEFAULT_ADMIN_BIND`, is
    // `"127.0.0.1:50170"`) -- absent (the empty-string default below) is a real, honest
    // configuration state this process starts in fine; the bundle route just names the
    // `av-command` side unreachable rather than refusing to serve at all.
    let admin_bind_raw = env_or("AV_GATEWAY_ADMIN_BIND", "127.0.0.1:50171");
    let admin_addr = resolve_loopback_bind_address(&admin_bind_raw).unwrap_or_else(|e| {
        eprintln!("av-gateway: refusing to start: {e}");
        std::process::exit(1);
    });
    let command_admin_bind_raw = env_or("AV_GATEWAY_COMMAND_ADMIN_BIND", "");
    let command_admin_addr = if command_admin_bind_raw.is_empty() {
        None
    } else {
        Some(resolve_loopback_bind_address(&command_admin_bind_raw).unwrap_or_else(|e| {
            eprintln!("av-gateway: refusing to start: {e}");
            std::process::exit(1);
        }))
    };
    let bundle_state = Arc::new(BundleState {
        evidence_ledger: evidence_ledger.clone(),
        counters: counters.clone(),
        run_id: "av-gateway".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        command_admin_addr,
    });
    let admin_task = tokio::spawn(async move {
        if let Err(e) = av_gateway::admin::serve(admin_addr, bundle_state).await {
            eprintln!("av-gateway: admin server exited: {e}");
        }
    });

    let mcp_ctx = McpContext { gateway: core.clone(), authority, evidence_ledger, clock, counters };
    let mcp_handler = McpHandler::new(mcp_ctx);

    let grpc = tonic::transport::Server::builder()
        .layer(unknown_route_layer)
        .add_service(DataGatewayServiceServer::new(DataGatewayServiceImpl::new(core)))
        .add_service(ModelProposeServiceServer::new(model_propose))
        .serve(bind_addr);

    let mcp = serve(tokio::io::stdin(), tokio::io::stdout(), mcp_handler);

    eprintln!("av-gateway: DataGatewayService + ModelProposeService on {bind_addr}, admin (evidence bundle) on {admin_addr}, MCP server on stdio");
    // R3.3 (manager's review, a defect measured not guessed): this used to be a
    // `tokio::select!` over BOTH futures, so whichever finished first ended the process. The
    // MCP stdio loop finishes the instant its stdin reports EOF -- and a container started
    // with `docker run -d` and no `-i` gets `/dev/null` on stdin, i.e. EOF immediately. The
    // gRPC port therefore closed a fraction of a second after opening, `docker run -d` itself
    // exited 0, and nothing in the container's logs said anything had gone wrong: a failure
    // that leaves no trace, in production rather than in a test.
    //
    // The two surfaces are independent, so they are now treated as independent: the MCP loop
    // reaching EOF is a normal end of *that* surface (there is no stdio peer any more; there
    // is nothing to serve and nothing to recover) and is reported as such, while the gRPC
    // server keeps serving until it stops on its own. Only the gRPC server's own exit ends
    // this process. A deployment that wants the MCP surface still has to give the process a
    // real stdin -- the difference is that it now finds out from a line that says so, instead
    // of from a port that is not there.
    let mcp_task = tokio::spawn(async move {
        match mcp.await {
            Ok(()) => eprintln!("av-gateway: MCP stdio surface ended at EOF (no stdio peer; the gRPC surface is unaffected and still serving)"),
            Err(e) => eprintln!("av-gateway: MCP server exited: {e}"),
        }
    });
    if let Err(e) = grpc.await {
        eprintln!("av-gateway: gRPC server exited: {e}");
    }
    mcp_task.abort();
    admin_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_cdm::pb::{GatewayQueryRequest, GatewaySelector, RunIdentity};

    /// The real, committed fixture this task names: `run_id`
    /// `demo_two_instance_frozen_fixture`, the same file `crates/av-proposer/tests/common/
    /// mod.rs::fixture_bytes` and `crates/av-gateway/tests/propose_only.rs` already read.
    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/demo_two_instance.runproducts.bin")
    }

    #[test]
    fn parse_cli_args_reads_both_flags_repeatable_run_products_included() {
        let args = ["av-gateway", "--internal-network-bind", "0.0.0.0:50170", "--run-products", "a.bin:CUI", "--run-products", "b.bin:SECRET"]
            .into_iter()
            .map(str::to_string);
        let cli = parse_cli_args(args).unwrap();
        assert_eq!(cli.internal_network_bind, Some("0.0.0.0:50170".to_string()));
        assert_eq!(cli.run_products, vec!["a.bin:CUI".to_string(), "b.bin:SECRET".to_string()]);
    }

    #[test]
    fn parse_cli_args_defaults_to_neither_flag_present() {
        let cli = parse_cli_args(["av-gateway"].into_iter().map(str::to_string)).unwrap();
        assert_eq!(cli, CliArgs::default());
    }

    #[test]
    fn parse_cli_args_refuses_an_unrecognized_flag() {
        let err = parse_cli_args(["av-gateway", "--nonsense"].into_iter().map(str::to_string)).unwrap_err();
        assert!(err.contains("--nonsense"), "{err}");
    }

    #[test]
    fn parse_run_products_spec_splits_on_the_last_colon() {
        let (path, marking) = parse_run_products_spec("/a/b.bin:CUI").unwrap();
        assert_eq!(path, PathBuf::from("/a/b.bin"));
        assert_eq!(marking, "CUI");
    }

    #[test]
    fn parse_run_products_spec_refuses_a_spec_with_no_colon() {
        assert!(parse_run_products_spec("/a/b.bin").is_err());
    }

    #[test]
    fn parse_run_products_spec_refuses_an_empty_marking() {
        assert!(parse_run_products_spec("/a/b.bin:").is_err());
    }

    #[test]
    fn parse_run_products_spec_refuses_an_empty_path() {
        assert!(parse_run_products_spec(":CUI").is_err());
    }

    /// This task's own acceptance line for the catalogue flags: a real committed
    /// `RunProducts` fixture, loaded through the exact `--run-products PATH:MARKING` path
    /// this binary's own CLI parses, ends up in a catalogue a real `RunCatalogue::resolve`
    /// call can actually query -- not merely that the file reads and decodes.
    #[test]
    fn a_real_run_products_fixture_loaded_via_run_products_flag_is_queryable() {
        let spec = format!("{}:CUI", fixture_path().display());
        let entries = build_catalogue_from_run_products_specs(&[spec]).expect("the real fixture loads");
        assert_eq!(entries.len(), 1);
        let catalogue = RunCatalogue::new(entries);

        let resolved = catalogue
            .resolve(&RunIdentity { run_id: "demo_two_instance_frozen_fixture".to_string(), config_hash: String::new() })
            .expect("the real fixture's own run_id resolves");
        assert_eq!(resolved.run_products.run_id, "demo_two_instance_frozen_fixture");
        assert_eq!(resolved.label.marking, "CUI");

        // And through the real GatewayCore query path too -- the same catalogue this
        // binary's own main() builds is queryable exactly the way a real DataGatewayService
        // request queries it, not merely through RunCatalogue::resolve directly.
        let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
        let core = GatewayCore::new(catalogue, ladder, Arc::new(av_gateway::counters::Counters::new()));
        let resp = core
            .query(&GatewayQueryRequest {
                run: Some(RunIdentity { run_id: "demo_two_instance_frozen_fixture".to_string(), config_hash: String::new() }),
                caller_clearance: "CUI".to_string(),
                selector: GatewaySelector::All as i32,
                caller_supplied_products_uri: String::new(),
            })
            .expect("a real query against the real, flag-loaded catalogue");
        assert!(!resp.trajectories.is_empty(), "the real fixture has trajectories");
    }

    #[test]
    fn build_catalogue_from_run_products_specs_is_empty_when_no_flag_was_passed() {
        assert!(build_catalogue_from_run_products_specs(&[]).unwrap().is_empty());
    }
}

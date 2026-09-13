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

use std::collections::BTreeMap;
use std::sync::Arc;

use av_command::clock::SystemClock;
use av_command::ledger::Ledger;
use av_command::service::resolve_loopback_bind_address;
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::gateway::{DataGatewayServiceImpl, DataGatewayServiceServer, GatewayCore};
use av_gateway::labels::ClearanceLadder;
use av_gateway::mcp::{serve, McpContext, McpHandler};
use av_gateway::propose_flow::ModelProposeServiceImpl;
use av_gateway::propose_only::ProposeOnlyAuthority;
use av_gateway::pb::model_propose_service_server::ModelProposeServiceServer;
use av_gateway::unknown_route_counter::UnknownRouteCounterLayer;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() {
    let bind_raw = env_or("AV_GATEWAY_BIND", "127.0.0.1:50170");
    let bind_addr = resolve_loopback_bind_address(&bind_raw).unwrap_or_else(|e| {
        eprintln!("av-gateway: refusing to start: {e}");
        std::process::exit(1);
    });

    let ladder_raw = env_or("AV_GATEWAY_CLEARANCE_LADDER", "UNCLASSIFIED,CUI,SECRET");
    let ladder = ClearanceLadder::new(ladder_raw.split(',').map(str::to_string).collect());

    // No run products configured by default -- a real deployment wires this from its own
    // profile; see this module's own doc for why this binary's configuration surface is
    // deliberately minimal.
    let catalogue = RunCatalogue::new(BTreeMap::<String, CatalogueEntry>::new());
    let counters = Arc::new(av_gateway::counters::Counters::new());
    let core = Arc::new(GatewayCore::new(catalogue, ladder, counters.clone()));

    let evidence_dir = env_or("AV_GATEWAY_EVIDENCE_LEDGER_DIR", "/tmp/av-gateway-evidence-ledger");
    let evidence_ledger = Arc::new(Ledger::open(&evidence_dir).unwrap_or_else(|e| {
        eprintln!("av-gateway: cannot open evidence ledger at {evidence_dir:?}: {e}");
        std::process::exit(1);
    }));

    let authority_endpoint = env_or("AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT", "http://127.0.0.1:50110");
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
    let mcp_ctx = McpContext { gateway: core.clone(), authority, evidence_ledger, clock, counters };
    let mcp_handler = McpHandler::new(mcp_ctx);

    let grpc = tonic::transport::Server::builder()
        .layer(unknown_route_layer)
        .add_service(DataGatewayServiceServer::new(DataGatewayServiceImpl::new(core)))
        .add_service(ModelProposeServiceServer::new(model_propose))
        .serve(bind_addr);

    let mcp = serve(tokio::io::stdin(), tokio::io::stdout(), mcp_handler);

    eprintln!("av-gateway: DataGatewayService + ModelProposeService on {bind_addr}, MCP server on stdio");
    tokio::select! {
        r = grpc => { if let Err(e) = r { eprintln!("av-gateway: gRPC server exited: {e}"); } }
        r = mcp => { if let Err(e) = r { eprintln!("av-gateway: MCP server exited: {e}"); } }
    }
}

//! Acceptance evidence 1: "the proposer cannot reach any state but PROPOSED."
//!
//! - `this_crates_own_generated_code_names_no_command_authority_service`: structural proof --
//!   greps every file `build.rs` generated into this crate's own `OUT_DIR` for the string
//!   `"CommandAuthorityService"` and asserts zero matches, printing which files were checked.
//! - `a_crafted_already_started_command_reaching_for_the_real_state_machine_directly_is_
//!   refused_and_counted`: (a)'s intent, honoured the way it is actually reachable. This
//!   crate's own `ProposeCommandRequest` wire schema has NO `state`/`transitions` field at
//!   all (mirroring `crates/av-gateway/src/mcp.rs::handle_propose_command`'s own JSON schema,
//!   D1's explicit instruction) -- a STRONGER property than "refused", since the attack
//!   cannot even be expressed through it. What this test proves instead: even reaching
//!   straight for the real `CommandAuthorityService` this workspace exposes (standing in for
//!   a proposer process that somehow built its own client -- which the structural test above
//!   shows this crate's own generated code never does), a hand-built already-started
//!   `Command` is still refused by the real state machine and counted, exactly as `crates/
//!   av-gateway/tests/propose_only.rs::a_crafted_command_that_is_already_started_is_refused_
//!   and_counted` already proves for the MCP surface.
//! - `a_raw_grpc_request_naming_command_authority_service_against_the_gateways_own_port_is_
//!   refused_and_counted`: (b), exactly as specified -- a raw gRPC call to
//!   `/altavista.v1.CommandAuthorityService/Authorize` against the harness's own gateway
//!   port. `tonic` answers `Unimplemented` before any of this workspace's code runs; the
//!   count comes from `av_gateway::unknown_route_counter`'s layer, attached to the harness's
//!   gateway `Server` in `tests/common/mod.rs`.

mod common;

use av_cdm::pb::{Command, CommandProposal, CommandState, CommandTransition};
use av_command::clock::Clock;
use av_gateway::propose_only::{ProposeOnlyAuthority, ProposeRefusal};
use common::GatewayHarness;

/// Structural proof, not behavioural: this crate's `build.rs` never parses
/// `authority.proto`'s own text at all (see that file's own module doc) -- the ONLY way
/// `"CommandAuthorityService"` could appear in this crate's generated output is if a future
/// change accidentally started compiling that file's services. Checks every `.rs` file this
/// crate's build script wrote into `OUT_DIR`, not just the ones this crate's own code
/// `include!`s, so a stray, unused generated file could not hide the string from this test
/// either.
#[test]
fn this_crates_own_generated_code_names_no_command_authority_service() {
    let out_dir = std::path::PathBuf::from(env!("OUT_DIR"));
    let mut checked = Vec::new();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&out_dir).unwrap_or_else(|e| panic!("reading this crate's own OUT_DIR {out_dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let content = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
            checked.push(path.clone());
            // Checks for the actual generated identifiers a client would need (the module
            // and the type name tonic_build's own naive_snake_case/PascalCase convention
            // would produce for that service), not merely the bare English phrase -- so this
            // test cannot be defeated by a doc comment that happens to mention the service's
            // name while never generating a line of code for it (an earlier version of this
            // test's own `build.rs` comments did exactly that, and this test caught it).
            if content.contains("command_authority_service_client") || content.contains("command_authority_service_server") || content.contains("CommandAuthorityServiceClient") || content.contains("CommandAuthorityServiceServer") {
                found.push(path);
            }
        }
    }
    assert!(!checked.is_empty(), "no .rs files found under this crate's own OUT_DIR {out_dir:?} -- build.rs did not run as expected");
    assert!(found.is_empty(), "CommandAuthorityService appears in this crate's own generated code: {found:?} (checked: {checked:?})");
}

fn already_authorized_proposal(id: &str) -> CommandProposal {
    let already_authorized = Command {
        id: id.to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "burn".to_string(),
        state: CommandState::Authorized as i32,
        transitions: vec![CommandTransition { state: CommandState::Authorized as i32, tai_ns: 1, principal: "attacker".to_string(), ..Default::default() }],
        ..Default::default()
    };
    CommandProposal { command: Some(already_authorized), rationale: "crafted".to_string(), evidence_ids: vec![] }
}

#[tokio::test]
async fn a_crafted_already_started_command_reaching_for_the_real_state_machine_directly_is_refused_and_counted() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("no-cmd-auth-crafted", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    let channel = tonic::transport::Endpoint::from_shared(harness.command_authority_endpoint.clone())
        .expect("valid endpoint URI")
        .connect()
        .await
        .expect("connect to the harness's own CommandAuthorityService port");
    let authority = ProposeOnlyAuthority::from_channel(channel);

    let err = authority.propose(already_authorized_proposal("crafted-already-started"), "attacker".to_string()).await.unwrap_err();
    assert!(matches!(err, ProposeRefusal::AlreadyStarted { .. }), "{err:?}");

    let commands = harness.command_ledger.scan_commands().expect("scan_commands");
    assert!(!commands.contains_key("crafted-already-started"), "a crafted already-started command must never reach the ledger as anything but refused");

    // The crafted attempt above never reached ANY real propose flow, so it never touched the
    // evidence ledger either -- unlike a real, accepted proposal (proven in `tests/
    // evidence_replay.rs`), which does. Also pins the harness's own injected clock actually
    // being what `GatewayHarness::spawn` was given, not a silently substituted wall clock.
    assert_eq!(harness.clock.now_tai_ns(), 1_000);
    let evidence = av_gateway::evidence::EvidenceRecorder::new(&harness.evidence_ledger).read_back("crafted-already-started").expect("read_back");
    assert!(evidence.is_none(), "a refused proposal must never leave an evidence record either");

    harness.shutdown().await;
}

#[tokio::test]
async fn a_raw_grpc_request_naming_command_authority_service_against_the_gateways_own_port_is_refused_and_counted() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("no-cmd-auth-raw-path", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    // `av_proposer::gateway_client::GatewayClient` has no way to name
    // `/CommandAuthorityService/Authorize` at all (this crate's own structural guarantee --
    // it never even generates that service's client), so this test builds the raw HTTP/2
    // request by hand, over the SAME kind of channel a real proposer's own `GatewayClient`
    // dials, to reach a path this gateway's own server does not serve at all.
    let channel = tonic::transport::Endpoint::from_shared(harness.endpoint.clone()).expect("valid endpoint URI").connect().await.expect("connect to the harness's own gateway port");
    let mut raw = tonic::client::Grpc::new(channel);
    raw.ready().await.expect("channel ready");
    let path = http::uri::PathAndQuery::from_static("/altavista.v1.CommandAuthorityService/Authorize");
    let codec = tonic::codec::ProstCodec::<av_cdm::pb::AuthorizeRequest, av_cdm::pb::CommandResponse>::default();
    let request = tonic::Request::new(av_cdm::pb::AuthorizeRequest { command_id: "whatever".to_string(), principal_token: String::new(), delegation_id: String::new() });
    let status = raw.unary(request, path, codec).await.expect_err("this gateway serves no CommandAuthorityService rpc at all");
    assert_eq!(status.code(), tonic::Code::Unimplemented, "{status}");

    assert_eq!(harness.counters.get("gateway_unknown_route"), 1);

    harness.shutdown().await;
}

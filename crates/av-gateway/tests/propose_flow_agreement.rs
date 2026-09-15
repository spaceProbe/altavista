//! D1's own acceptance line: the MCP `propose_command` tool and the new
//! `ModelProposeService.ProposeCommand` rpc call through ONE shared implementation
//! (`crate::propose_flow::propose_command`) -- this file proves the two surfaces can never
//! disagree, against a REAL `CommandAuthorityServiceImpl` over a real loopback socket
//! (`tests/propose_only.rs` is the exact pattern this file's own harness copies -- this
//! crate's stated convention, per `tests/common/mod.rs`'s own doc, is that a helper used by
//! only one test binary lives in that binary, not in `tests/common/`).
//!
//! Two assertions, mirroring this task's own brief word for word:
//! - `two_propose_surfaces_agree_on_the_same_outcome_for_equivalent_input`: the same
//!   (modulo command id, so the two do not collide on the same ledger) proposal, one through
//!   each surface, ends `PROPOSED` on the real ledger with identical field values, and an
//!   evidence record with identical run/query_ids/model identity/version.
//! - `two_propose_surfaces_agree_on_the_same_refusal_for_the_same_crafted_input`: the same
//!   crafted input (a non-empty `envelope_id`, question 53) is refused by both, as the same
//!   underlying refusal kind, with the same real `av_command::state::propose` message text.

mod harness {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use av_command::audit::{AuditSinkConfig, AuditWriter};
    use av_command::authz::{DelegationTable, RoleTable, ServiceRoleTable};
    use av_command::clock::TestClock;
    use av_command::ledger::Ledger;
    use av_command::oidc::IssuerConfig;
    use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
    use av_command::policy::PolicyBundle;
    use av_command::service::{AuthzConfig, CommandAuthorityServiceImpl, RecordingDispatchSink};
    use av_command::test_support::TestIssuer;
    use av_gateway::propose_only::ProposeOnlyAuthority;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::{Endpoint, Server};

    fn real_policy_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
    }

    /// A real `CommandAuthorityServiceImpl` over a real loopback socket -- see
    /// `crates/av-gateway/tests/propose_only.rs`'s identically-named type for the full
    /// reasoning; copied here rather than shared because this crate's own convention (`tests/
    /// common/mod.rs`'s doc) is one private copy per test binary that needs it.
    pub struct CommandAuthorityHarness {
        pub authority: Arc<ProposeOnlyAuthority>,
        pub ledger: Arc<Ledger>,
        pub clock: Arc<TestClock>,
        shutdown_tx: Option<oneshot::Sender<()>>,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl CommandAuthorityHarness {
        pub async fn spawn(name: &str, start_tai_ns: i64) -> Self {
            let ledger_dir = crate::common::tmp_dir(name);
            let ledger = Arc::new(Ledger::open(&ledger_dir).expect("open ledger"));
            let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
            let clock = Arc::new(TestClock::new(start_tai_ns));
            let dispatch_sink = Arc::new(RecordingDispatchSink::new());
            let issuer = TestIssuer::new();
            let issuer_config = Arc::new(
                IssuerConfig::from_public_key_pem("https://sso.test.example/", "av-gateway-it", issuer.public_key_pem())
                    .expect("a freshly generated test issuer key parses as a valid public key"),
            );
            let audit_path = ledger_dir.join("audit.log");
            let audit = Arc::new(AuditWriter::open(&AuditSinkConfig::File(audit_path)).expect("open the test audit sink file"));
            let authz = AuthzConfig {
                role_table: Arc::new(RoleTable::from_config(&BTreeMap::new())),
                delegations: Arc::new(DelegationTable::from_delegations(vec![])),
                mfa_amr_methods: Arc::new(vec![]),
                mfa_acr: Arc::new(String::new()),
                audit,
                service_role_table: Arc::new(ServiceRoleTable::from_config(&BTreeMap::new()).expect("an empty table always parses")),
                counters: Arc::new(av_command::counters::Counters::new()),
            };

            let servicer = CommandAuthorityServiceImpl::new(
                ledger.clone(),
                bundle,
                3_600_000_000_000,
                clock.clone() as Arc<dyn av_command::clock::Clock>,
                dispatch_sink as Arc<dyn av_command::service::DispatchSink>,
                issuer_config,
                authz,
            )
            .expect("rebuild the duplicate-dispatch guard from the ledger at construction");

            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
            let addr = listener.local_addr().expect("local_addr");
            let incoming = TcpListenerStream::new(listener);

            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let handle = tokio::spawn(async move {
                Server::builder()
                    .add_service(CommandAuthorityServiceServer::new(servicer))
                    .serve_with_incoming_shutdown(incoming, async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("server exits cleanly");
            });

            let channel = Endpoint::from_shared(format!("http://{addr}"))
                .expect("valid endpoint URI")
                .connect()
                .await
                .expect("connect to the just-spawned server over its real loopback socket");
            let authority = Arc::new(ProposeOnlyAuthority::from_channel(channel));

            Self { authority, ledger, clock, shutdown_tx: Some(shutdown_tx), handle: Some(handle) }
        }

        pub async fn shutdown_keep_ledger(mut self) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.await;
            }
        }
    }

    /// Spawns `ModelProposeServiceImpl` over its own real loopback socket, wired to
    /// `authority`/`evidence_ledger`/`clock`/`counters` -- the gRPC half of the agreement
    /// this file proves.
    pub async fn spawn_model_propose_service(
        authority: Arc<ProposeOnlyAuthority>,
        evidence_ledger: Arc<Ledger>,
        clock: Arc<dyn av_command::clock::Clock>,
        counters: Arc<av_command::counters::Counters>,
    ) -> (av_gateway::pb::model_propose_service_client::ModelProposeServiceClient<tonic::transport::Channel>, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        use av_gateway::pb::model_propose_service_client::ModelProposeServiceClient;
        use av_gateway::pb::model_propose_service_server::ModelProposeServiceServer;
        use av_gateway::propose_flow::ModelProposeServiceImpl;

        let servicer = ModelProposeServiceImpl::new(authority, evidence_ledger, clock, counters);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("local_addr");
        let incoming = TcpListenerStream::new(listener);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            Server::builder()
                .add_service(ModelProposeServiceServer::new(servicer))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("server exits cleanly");
        });
        let channel = Endpoint::from_shared(format!("http://{addr}")).expect("valid endpoint URI").connect().await.expect("connect to the just-spawned ModelProposeService");
        (ModelProposeServiceClient::new(channel), shutdown_tx, handle)
    }
}

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use av_cdm::pb::{Label, ProposalEvidence, ProposeCommandRequest, Provenance, RunIdentity, RunProducts};
use av_command::clock::Clock;
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::counters::Counters;
use av_gateway::evidence::EvidenceRecorder;
use av_gateway::gateway::GatewayCore;
use av_gateway::labels::ClearanceLadder;
use av_gateway::mcp::{McpContext, McpHandler};
use harness::CommandAuthorityHarness;
use serde_json::Value;

/// An `McpHandler` wired to the SAME `authority`/`evidence_ledger`/`clock`/`counters` a test
/// hands it -- so its half of the agreement runs against the identical real
/// `CommandAuthorityServiceImpl` and evidence ledger the gRPC half does.
fn mcp_handler(authority: Arc<av_gateway::propose_only::ProposeOnlyAuthority>, evidence_ledger: Arc<av_command::ledger::Ledger>, clock: Arc<dyn Clock>, counters: Arc<Counters>) -> McpHandler {
    let mut entries = BTreeMap::new();
    entries.insert(
        "run-a".to_string(),
        CatalogueEntry::from_run_products(
            Label { marking: "CUI".to_string(), caveats: vec![] },
            &RunProducts { run_id: "run-a".to_string(), provenance: Some(Provenance::default()), ..Default::default() },
        ),
    );
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    let gateway = Arc::new(GatewayCore::new(RunCatalogue::new(entries), ladder, Arc::new(Counters::new())));
    McpHandler::new(McpContext { gateway, authority, evidence_ledger, clock, counters })
}

fn mcp_propose_args(command_id: &str) -> Value {
    serde_json::json!({
        "command_id": command_id,
        "entity_id": "sat-1",
        "command_class": "burn",
        "hazardous": false,
        "principal": "model-x",
        "model_version": "1.0.0",
        "rationale": "agreement test",
        "run_id": "run-fixture",
        "config_hash": "hash-fixture",
        "query_ids": ["q1", "q2"],
    })
}

fn grpc_propose_request(command_id: &str) -> ProposeCommandRequest {
    ProposeCommandRequest {
        command_id: command_id.to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "burn".to_string(),
        hazardous: false,
        envelope_id: String::new(),
        idempotency_key: String::new(),
        rationale: "agreement test".to_string(),
        evidence_ids: vec![],
        principal: "model-x".to_string(),
        model_version: "1.0.0".to_string(),
        run: Some(RunIdentity { run_id: "run-fixture".to_string(), config_hash: "hash-fixture".to_string() }),
        query_ids: vec!["q1".to_string(), "q2".to_string()],
    }
}

#[tokio::test]
async fn two_propose_surfaces_agree_on_the_same_outcome_for_equivalent_input() {
    let cmd_auth = CommandAuthorityHarness::spawn("agreement-outcome", 1_000).await;
    let evidence_dir = common::tmp_dir("agreement-outcome-evidence");
    let evidence_ledger = Arc::new(av_command::ledger::Ledger::open(&evidence_dir).expect("open evidence ledger"));
    let clock: Arc<dyn Clock> = cmd_auth.clock.clone();
    let mcp_counters = Arc::new(Counters::new());
    let grpc_counters = Arc::new(Counters::new());

    // MCP surface.
    let handler = mcp_handler(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), mcp_counters);
    let mcp_args = mcp_propose_args("agree-mcp-1");
    let raw = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"propose_command","arguments": mcp_args}}).to_string();
    let mcp_resp = handler.handle_message(&raw).await.expect("a request with an id always gets a response");
    // Question 209(a): Propose now checks automatically -- "burn" is admitted (well under the
    // rate limit at a single recent submission), so both surfaces land at CHECKED, not
    // PROPOSED.
    assert_eq!(mcp_resp["result"]["state"], "COMMAND_STATE_CHECKED", "{mcp_resp}");
    assert_eq!(mcp_resp["result"]["command_id"], "agree-mcp-1");

    // gRPC surface, over a real loopback socket.
    let (mut grpc_client, shutdown_tx, handle) = harness::spawn_model_propose_service(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), grpc_counters).await;
    let grpc_resp = grpc_client.propose_command(grpc_propose_request("agree-grpc-1")).await.expect("ProposeCommand rpc succeeds").into_inner();
    let grpc_command = grpc_resp.command.expect("a successful ProposeCommandResponse always carries the command");
    assert_eq!(grpc_command.state, av_cdm::pb::CommandState::Checked as i32);
    assert_eq!(grpc_command.id, "agree-grpc-1");
    let _ = shutdown_tx.send(());
    let _ = handle.await;

    // Both landed on the SAME real ledger as CHECKED, with identical field values (aside
    // from the id each surface was given) -- the actual "same outcome" assertion.
    let commands = cmd_auth.ledger.scan_commands().expect("scan_commands");
    let mcp_command = commands.get("agree-mcp-1").expect("mcp-proposed command on the ledger");
    let grpc_command = commands.get("agree-grpc-1").expect("grpc-proposed command on the ledger");
    assert_eq!(mcp_command.state, grpc_command.state);
    assert_eq!(mcp_command.entity_id, grpc_command.entity_id);
    assert_eq!(mcp_command.command_class, grpc_command.command_class);
    assert_eq!(mcp_command.hazardous, grpc_command.hazardous);
    assert_eq!(mcp_command.transitions.len(), grpc_command.transitions.len());
    assert_eq!(mcp_command.transitions[0].state, grpc_command.transitions[0].state);
    assert_eq!(mcp_command.transitions[0].principal, grpc_command.transitions[0].principal);

    // Both evidence records agree too (D5/D6, through the shared implementation).
    let recorder = EvidenceRecorder::new(&evidence_ledger);
    let mcp_evidence = recorder.read_back("agree-mcp-1").expect("read_back").expect("evidence present for the mcp surface");
    let grpc_evidence = recorder.read_back("agree-grpc-1").expect("read_back").expect("evidence present for the grpc surface");
    assert_eq!(mcp_evidence.run, grpc_evidence.run);
    assert_eq!(mcp_evidence.query_ids, grpc_evidence.query_ids);
    assert_eq!(mcp_evidence.model_identity, grpc_evidence.model_identity);
    assert_eq!(mcp_evidence.model_version, grpc_evidence.model_version);
    let _: &ProposalEvidence = &mcp_evidence; // both sides really are ProposalEvidence, not two different shapes.

    let _ = std::fs::remove_dir_all(&evidence_dir);
    cmd_auth.shutdown_keep_ledger().await;
}

/// Question 53's `EnvelopeNotAllowed`, through BOTH surfaces, from the identical crafted
/// input (a non-empty `envelope_id`) -- both must be refused, as the same underlying refusal
/// kind, with the same real `av_command::state::propose` message text neither surface
/// re-derives independently.
#[tokio::test]
async fn two_propose_surfaces_agree_on_the_same_refusal_for_the_same_crafted_input() {
    let cmd_auth = CommandAuthorityHarness::spawn("agreement-refusal", 1_000).await;
    let evidence_dir = common::tmp_dir("agreement-refusal-evidence");
    let evidence_ledger = Arc::new(av_command::ledger::Ledger::open(&evidence_dir).expect("open evidence ledger"));
    let clock: Arc<dyn Clock> = cmd_auth.clock.clone();
    let mcp_counters = Arc::new(Counters::new());
    let grpc_counters = Arc::new(Counters::new());

    let handler = mcp_handler(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), mcp_counters);
    let mut mcp_args = mcp_propose_args("refuse-mcp-1");
    mcp_args["envelope_id"] = Value::String("env-station-keeping".to_string());
    let raw = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"propose_command","arguments": mcp_args}}).to_string();
    let mcp_resp = handler.handle_message(&raw).await.expect("a request with an id always gets a response");
    let mcp_message = mcp_resp["error"]["message"].as_str().expect("a refused proposal carries an error message").to_string();
    assert_eq!(mcp_resp["error"]["code"], -32602);
    assert!(mcp_message.contains("propose refuses a non-empty envelope_id"), "{mcp_message}");

    let (mut grpc_client, shutdown_tx, handle) = harness::spawn_model_propose_service(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), grpc_counters).await;
    let mut grpc_req = grpc_propose_request("refuse-grpc-1");
    grpc_req.envelope_id = "env-station-keeping".to_string();
    let status = grpc_client.propose_command(grpc_req).await.expect_err("a non-empty envelope_id must be refused");
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
    assert!(status.message().contains("propose refuses a non-empty envelope_id"), "{status}");
    let _ = shutdown_tx.send(());
    let _ = handle.await;

    // Neither surface's crafted attempt reached the ledger at all.
    let commands = cmd_auth.ledger.scan_commands().expect("scan_commands");
    assert!(!commands.contains_key("refuse-mcp-1"));
    assert!(!commands.contains_key("refuse-grpc-1"));

    let _ = std::fs::remove_dir_all(&evidence_dir);
    cmd_auth.shutdown_keep_ledger().await;
}

/// **Question 209(a)/D6**: a policy denial, through BOTH surfaces, from the identical
/// `"payload"`-class proposal (unconditionally denied by the shipped policy) -- both must be
/// refused, typed and counted, with the same real `av_command::service::ServiceError::
/// PolicyDenied` message text (naming the decision id and deny reasons) neither surface
/// re-derives independently. The gRPC surface's own refusal is `PERMISSION_DENIED`
/// (`ModelProposeServiceImpl`'s `to_status`, mirroring the real `Propose` RPC's own mapping);
/// the MCP surface reshapes the identical underlying `ProposeFlowError` into its own
/// `InvalidParams` JSON-RPC shape (as it does for every propose refusal), but the message
/// text -- the actual thing this test pins -- is the same.
#[tokio::test]
async fn two_propose_surfaces_agree_on_a_policy_denial() {
    let cmd_auth = CommandAuthorityHarness::spawn("agreement-policy-denial", 1_000).await;
    let evidence_dir = common::tmp_dir("agreement-policy-denial-evidence");
    let evidence_ledger = Arc::new(av_command::ledger::Ledger::open(&evidence_dir).expect("open evidence ledger"));
    let clock: Arc<dyn Clock> = cmd_auth.clock.clone();
    let mcp_counters = Arc::new(Counters::new());
    let grpc_counters = Arc::new(Counters::new());

    let handler = mcp_handler(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), mcp_counters.clone());
    let mut mcp_args = mcp_propose_args("deny-mcp-1");
    mcp_args["command_class"] = Value::String("payload".to_string());
    let raw = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"propose_command","arguments": mcp_args}}).to_string();
    let mcp_resp = handler.handle_message(&raw).await.expect("a request with an id always gets a response");
    let mcp_message = mcp_resp["error"]["message"].as_str().expect("a refused proposal carries an error message").to_string();
    assert!(mcp_message.contains("policy denied"), "{mcp_message}");
    assert!(mcp_message.contains("command_class payload is not admitted by policy"), "{mcp_message}");
    assert_eq!(mcp_counters.get("propose_policy_denied"), 1, "the MCP surface must count the identical refusal code");

    let (mut grpc_client, shutdown_tx, handle) = harness::spawn_model_propose_service(cmd_auth.authority.clone(), evidence_ledger.clone(), clock.clone(), grpc_counters.clone()).await;
    let mut grpc_req = grpc_propose_request("deny-grpc-1");
    grpc_req.command_class = "payload".to_string();
    let status = grpc_client.propose_command(grpc_req).await.expect_err("a payload-class command must be refused by policy");
    assert_eq!(status.code(), tonic::Code::PermissionDenied, "{status}");
    assert!(status.message().contains("policy denied"), "{status}");
    assert!(status.message().contains("command_class payload is not admitted by policy"), "{status}");
    assert_eq!(grpc_counters.get("propose_policy_denied"), 1, "the gRPC surface must count the identical refusal code");
    let _ = shutdown_tx.send(());
    let _ = handle.await;

    // Neither surface's refusal erased the REJECTED record: it is still durable and queryable
    // on the real CommandAuthorityService ledger (D3), proven directly here rather than
    // assumed.
    let commands = cmd_auth.ledger.scan_commands().expect("scan_commands");
    let mcp_command = commands.get("deny-mcp-1").expect("the REJECTED record for the mcp surface's attempt must still be on the ledger");
    let grpc_command = commands.get("deny-grpc-1").expect("the REJECTED record for the grpc surface's attempt must still be on the ledger");
    assert_eq!(mcp_command.state, av_cdm::pb::CommandState::Rejected as i32);
    assert_eq!(grpc_command.state, av_cdm::pb::CommandState::Rejected as i32);

    let _ = std::fs::remove_dir_all(&evidence_dir);
    cmd_auth.shutdown_keep_ledger().await;
}

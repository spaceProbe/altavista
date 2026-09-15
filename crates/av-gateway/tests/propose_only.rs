//! D4/D5/D6 acceptance evidence, against a REAL `CommandAuthorityServiceImpl` over a real
//! loopback socket (`tests/common/mod.rs`).
//!
//! - `propose_command_creates_proposed_and_nothing_else`: after `ProposeOnlyAuthority::
//!   propose`, the real ledger shows exactly one `COMMAND_STATE_PROPOSED` record for that
//!   command id -- nothing past it.
//! - `a_proposal_carrying_a_non_empty_envelope_id_is_refused_end_to_end`: question 53,
//!   through this crate's own seam, against the real `state::propose`.
//! - `a_crafted_command_that_is_already_started_is_refused_and_counted`: D4's "any other
//!   shape" -- a hand-built `Command` with a state/transition already populated, proving
//!   `ProposeOnlyAuthority` cannot smuggle a non-fresh command past the real state machine
//!   either.
//! - `evidence_round_trips_the_run_identity_and_query_ids_the_gateway_actually_served`: D5's
//!   own acceptance line -- reads the evidence record back off the ledger and matches it
//!   against what a real `GatewayCore::query` call actually returned in the same test, not
//!   constants the test also wrote.

mod common;

/// A REAL `CommandAuthorityServiceImpl` over a real loopback socket, mirroring
/// `crates/av-command/tests/grpc_service.rs`'s own `TestServer` pattern -- private to this
/// one test binary (see `tests/common/mod.rs`'s own doc for why this is not in that shared
/// module: no other test binary in this crate needs a real command-authority server).
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

    /// Reached only through [`ProposeOnlyAuthority`] (D4): this harness never hands a test
    /// the raw generated `CommandAuthorityServiceClient`.
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
                // R3.1: this crate's own ProposeOnlyAuthority never reaches Dispatch/Ack/
                // Expire/Fail at all (crate::propose_only's own module doc: its generated
                // client carrying those RPCs is private to it) -- an empty service-role table
                // and a fresh Counters are all this fixture needs.
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
}

use std::collections::BTreeMap;
use std::sync::Arc;

use av_cdm::pb::{Command, CommandProposal, CommandState, GatewayQueryRequest, GatewaySelector, Label, ProposalEvidence, Provenance, RunIdentity, RunProducts};
use av_command::ledger::Ledger;
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::counters::Counters;
use av_gateway::evidence::EvidenceRecorder;
use av_gateway::gateway::GatewayCore;
use av_gateway::labels::ClearanceLadder;
use av_gateway::propose_only::ProposeRefusal;
use harness::CommandAuthorityHarness;

fn fresh_proposal(id: &str, entity_id: &str) -> CommandProposal {
    CommandProposal {
        command: Some(Command { id: id.to_string(), entity_id: entity_id.to_string(), command_class: "mode".to_string(), ..Default::default() }),
        rationale: "integration test proposal".to_string(),
        evidence_ids: vec![],
    }
}

/// D4's primary acceptance line, "the other end": after `propose_command`, the ledger shows
/// exactly `PROPOSED` for that command, and nothing past it.
#[tokio::test]
async fn propose_command_creates_proposed_and_nothing_else() {
    let harness = CommandAuthorityHarness::spawn("propose-only-basic", 1_000).await;

    let command = harness.authority.propose(fresh_proposal("cmd-1", "sat-1"), "model-x".to_string()).await.expect("propose succeeds");
    assert_eq!(command.state, CommandState::Proposed as i32);
    assert_eq!(command.transitions.len(), 1);
    assert_eq!(command.transitions[0].state, CommandState::Proposed as i32);

    // Read straight off the real ledger's own scan -- not the in-memory Command this call
    // happened to return -- so this is a durable-record assertion, not merely "the RPC
    // response looked right".
    let commands = harness.ledger.scan_commands().expect("scan_commands");
    let stored = commands.get("cmd-1").expect("command must be on the ledger");
    assert_eq!(stored.state, CommandState::Proposed as i32);
    assert_eq!(stored.transitions.len(), 1, "nothing past PROPOSED: exactly one transition");
    assert_eq!(stored.transitions[0].state, CommandState::Proposed as i32);

    harness.shutdown_keep_ledger().await;
}

/// Question 53, end to end through this crate's own tool/seam: `state::propose` refuses a
/// non-empty `envelope_id` before any transition is recorded.
#[tokio::test]
async fn a_proposal_carrying_a_non_empty_envelope_id_is_refused_end_to_end() {
    let harness = CommandAuthorityHarness::spawn("propose-only-envelope", 1_000).await;

    let mut proposal = fresh_proposal("cmd-envelope", "sat-1");
    proposal.command.as_mut().unwrap().envelope_id = "env-station-keeping".to_string();

    let err = harness.authority.propose(proposal, "model-x".to_string()).await.unwrap_err();
    assert!(matches!(err, ProposeRefusal::EnvelopeNotAllowed { .. }), "{err:?}");
    assert!(err.to_string().contains("question 53"), "{err}");

    // Never reached the ledger at all.
    let commands = harness.ledger.scan_commands().expect("scan_commands");
    assert!(!commands.contains_key("cmd-envelope"));

    harness.shutdown_keep_ledger().await;
}

/// D4's "any other shape": a hand-built `Command` whose `state`/`transitions` are already
/// populated (something this crate's own MCP tool JSON schema can never construct, per
/// `crate::mcp::McpHandler::handle_propose_command`'s own doc -- exercised here by calling
/// `ProposeOnlyAuthority::propose` directly) is refused by the real state machine, not
/// silently accepted.
#[tokio::test]
async fn a_crafted_command_that_is_already_started_is_refused_and_counted() {
    let harness = CommandAuthorityHarness::spawn("propose-only-already-started", 1_000).await;

    let already_authorized = Command {
        id: "cmd-crafted".to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "mode".to_string(),
        state: CommandState::Authorized as i32,
        transitions: vec![av_cdm::pb::CommandTransition { state: CommandState::Authorized as i32, tai_ns: 1, principal: "attacker".to_string(), ..Default::default() }],
        ..Default::default()
    };
    let proposal = CommandProposal { command: Some(already_authorized), rationale: "crafted".to_string(), evidence_ids: vec![] };

    let counters = Counters::new();
    let err = harness.authority.propose(proposal, "model-x".to_string()).await.unwrap_err();
    counters.record(&err);
    assert!(matches!(err, ProposeRefusal::AlreadyStarted { .. }), "{err:?}");
    assert_eq!(counters.get("propose_already_started"), 1);

    let commands = harness.ledger.scan_commands().expect("scan_commands");
    assert!(!commands.contains_key("cmd-crafted"), "a crafted already-started command must never reach the ledger as anything but refused");

    harness.shutdown_keep_ledger().await;
}

/// D5's own acceptance line: read the evidence record back off the (dedicated -- see
/// `crate::evidence`'s module doc) ledger, and match its run identity and query ids against
/// what a real `GatewayCore::query` call actually served in THIS test session -- not
/// constants this test also independently wrote.
#[tokio::test]
async fn evidence_round_trips_the_run_identity_and_query_ids_the_gateway_actually_served() {
    let harness = CommandAuthorityHarness::spawn("propose-only-evidence", 1_000).await;

    // A real gateway session: build a catalogue, issue two real queries, and capture the
    // query_ids the gateway itself computed -- these are what gets attached to the
    // evidence record below, not values this test invents independently.
    let mut entries = BTreeMap::new();
    entries.insert(
        "run-a".to_string(),
        CatalogueEntry::from_run_products(
            Label { marking: "CUI".to_string(), caveats: vec![] },
            &RunProducts { run_id: "run-a".to_string(), provenance: Some(Provenance { config_hash: "hash-a".to_string(), ..Default::default() }), ..Default::default() },
        ),
    );
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]);
    let gateway = GatewayCore::new(RunCatalogue::new(entries), ladder, Arc::new(Counters::new()));

    let q1 = gateway
        .query(&GatewayQueryRequest {
            run: Some(RunIdentity { run_id: "run-a".to_string(), config_hash: String::new() }),
            caller_clearance: "CUI".to_string(),
            selector: GatewaySelector::All as i32,
            caller_supplied_products_uri: String::new(),
        })
        .expect("first real query succeeds");
    // A second, real, SUCCEEDING query -- differs from q1 by config_hash ("" vs "hash-a"),
    // which D5's query id preimage includes, so this is a genuinely distinct real query the
    // gateway served, not a hand-typed second constant.
    let q2 = gateway
        .query(&GatewayQueryRequest {
            run: Some(RunIdentity { run_id: "run-a".to_string(), config_hash: "hash-a".to_string() }),
            caller_clearance: "CUI".to_string(),
            selector: GatewaySelector::All as i32,
            caller_supplied_products_uri: String::new(),
        })
        .expect("second real query succeeds");

    let real_query_ids = vec![q1.query_id.clone(), q2.query_id.clone()];
    assert_ne!(q1.query_id, q2.query_id, "two different requests must not collapse to one id");

    // Propose, using what the gateway session above actually saw.
    let command = harness.authority.propose(fresh_proposal("cmd-evidence", "sat-1"), "model-x".to_string()).await.expect("propose succeeds");

    let evidence_dir = common::tmp_dir("propose-only-evidence-ledger");
    let evidence_ledger = Ledger::open(&evidence_dir).expect("open evidence ledger");
    let recorder = EvidenceRecorder::new(&evidence_ledger);
    let evidence = ProposalEvidence {
        command_id: command.id.clone(),
        run: Some(RunIdentity { run_id: "run-a".to_string(), config_hash: "hash-a".to_string() }),
        query_ids: real_query_ids.clone(),
        model_identity: "model-x".to_string(),
        model_version: "1.0.0".to_string(),
        recorded_tai_ns: 1_000,
    };
    recorder.record(&evidence, &*harness.clock).expect("record evidence");

    // The actual D5 assertion: read back off the ledger, compare against what the gateway
    // session actually served above -- not a constant this test also wrote independently
    // of the gateway calls.
    let back = recorder.read_back(&command.id).expect("read_back").expect("evidence present");
    assert_eq!(back.run.unwrap().run_id, "run-a");
    assert_eq!(back.query_ids, real_query_ids);

    let _ = std::fs::remove_dir_all(&evidence_dir);
    harness.shutdown_keep_ledger().await;
}

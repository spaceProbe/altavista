//! R3.6/A6, Part 1: "a replayed run reproduces every transition, decision id and proposal
//! from the ledger" (`docs/aiplane-plan.md` milestone A6; ADR-004: "Every transition is an
//! event on the log, so a replay reproduces the decision trail including what the model
//! saw"). Round 2's `crates/av-kernel/tests/replay.rs::t7_a_replayed_command_target_
//! reproduces_the_command_trail_byte_for_byte` already proved the KERNEL half (a replayed
//! `execute()` run reproduces `RunProducts.events`' own command trail). This file proves the
//! other half: `av-command`'s own **ledger** -- transitions, decision ids, policy hashes, and
//! (new this round) the original `CommandProposal` (rationale + evidence ids) -- reproduces
//! from the ledger alone, with the process that wrote it gone, no kernel run involved at all.
//!
//! ## Why this lives in `av-gateway`'s own `tests/`, not `av-command`'s
//!
//! The command must arrive at `CHECKED` (question 209(a): `Propose` now runs the check edge
//! automatically, as a separate logged transition -- so `PROPOSED` never persists on its own
//! past the one `Propose` call) through the **gateway's real propose path**
//! (`av_gateway::propose_flow::propose_command`, the one function both the MCP tool and
//! `ModelProposeService` call -- see that module's own doc) so a real `ProposalEvidence`
//! record genuinely exists on the gateway's own evidence ledger, not a hand-built one. That
//! function needs `av_gateway::propose_only::ProposeOnlyAuthority` and
//! `av_gateway::evidence::EvidenceRecorder`, both `av-gateway` types `av-command`'s own test
//! suite cannot reach (the dependency only runs the other direction). `Authorize`/`Dispatch`/
//! `Ack` are then driven directly against the same real `CommandAuthorityService` over its
//! real loopback socket, using the raw, non-restricted
//! `av_command::pb::command_authority_service_client::CommandAuthorityServiceClient` this
//! crate already depends on (a regular, non-dev dependency -- `av-gateway`'s own
//! `Cargo.toml`) -- never through `ProposeOnlyAuthority`, whose whole point (D4) is that it
//! cannot reach those RPCs at all.
//!
//! ## The tamper-detection technique this file reuses rather than re-derives
//!
//! The exact "decode a partition file's frames, mutate one record's body leaving its own
//! `hash`/`prev_hash` untouched, re-encode and rewrite the file" technique is
//! `crates/av-command/src/ledger.rs::tests::verify_detects_a_tampered_record_body_and_
//! reports_its_sequence_number`'s own machinery, already reused once by
//! `crates/av-command/tests/grpc_service.rs::verify_ledger_reports_a_tampered_partition_as_
//! broken_at_the_right_sequence` (that test's own comment: "the same technique
//! `crates/av-command/src/ledger.rs`'s own tamper-detection test uses"). This file cannot
//! import either -- both are private to a different crate's test/unit-test binary -- so it
//! restates the same small, documented on-disk framing (`ledger_file_path`/
//! `read_ledger_records`/`write_ledger_records` below are copied verbatim from
//! `grpc_service.rs`'s own helpers of the same name, which themselves document that they
//! exercise `av_command`'s *public*, documented on-disk contract, not a private shortcut) --
//! this is restating the documented contract a third time, in a third crate, never a fourth,
//! independently-invented tamper strategy.

mod common;

mod harness {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use av_command::audit::{AuditSinkConfig, AuditWriter};
    use av_command::authz::{DelegationTable, RoleTable, ServiceRoleTable};
    use av_command::clock::{Clock, TestClock};
    use av_command::counters::Counters;
    use av_command::ledger::Ledger;
    use av_command::oidc::IssuerConfig;
    use av_command::pb::command_authority_service_client::CommandAuthorityServiceClient;
    use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
    use av_command::policy::PolicyBundle;
    use av_command::service::{AuthzConfig, CommandAuthorityServiceImpl, DispatchSink, RecordingDispatchSink};
    use av_command::test_support::TestIssuer;
    use av_gateway::propose_only::ProposeOnlyAuthority;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::{Channel, Endpoint, Server};

    pub const TEST_ISSUER: &str = "https://sso.test.example/";
    pub const TEST_AUDIENCE: &str = "av-command";
    /// A role table granting `"operators"` the `"mode"` class -- matches
    /// `av_command::test_support::valid_claims`'s default `groups` (`["operators",
    /// "burn-authorizers"]`), exactly like `crates/av-command/tests/grpc_service.rs::
    /// default_roles` -- redefined here rather than imported because it is `av-command`'s own
    /// private test fixture, not part of its public API.
    pub fn default_roles() -> BTreeMap<String, Vec<String>> {
        let mut roles = BTreeMap::new();
        roles.insert("operators".to_string(), vec!["mode".to_string()]);
        roles
    }

    /// R3.1's service-role table, granting `"dispatchers"` all four service RPCs -- same
    /// reasoning as [`default_roles`].
    pub fn default_service_roles() -> BTreeMap<String, Vec<String>> {
        let mut roles = BTreeMap::new();
        roles.insert("dispatchers".to_string(), vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]);
        roles
    }

    pub fn real_policy_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
    }

    /// A real `CommandAuthorityServiceImpl` over a real loopback socket, reachable BOTH
    /// through the restricted [`ProposeOnlyAuthority`] (the gateway's own propose path) AND
    /// through the raw, unrestricted generated client (for `Check`/`Authorize`/`Dispatch`/
    /// `Ack`, which `ProposeOnlyAuthority` structurally cannot reach) -- plus the gateway's own
    /// dedicated evidence ledger (`av_gateway::evidence`'s module doc: never the command
    /// ledger's own directory).
    pub struct FullTrailHarness {
        pub authority: Arc<ProposeOnlyAuthority>,
        pub raw_client: CommandAuthorityServiceClient<Channel>,
        pub ledger_dir: PathBuf,
        pub evidence_ledger: Arc<Ledger>,
        pub evidence_ledger_dir: PathBuf,
        pub clock: Arc<TestClock>,
        pub issuer: TestIssuer,
        shutdown_tx: Option<oneshot::Sender<()>>,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl FullTrailHarness {
        pub async fn spawn(name: &str, start_tai_ns: i64) -> Self {
            let ledger_dir = crate::common::tmp_dir(name);
            let ledger = Arc::new(Ledger::open(&ledger_dir).expect("open ledger"));
            let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
            let clock = Arc::new(TestClock::new(start_tai_ns));
            let dispatch_sink = Arc::new(RecordingDispatchSink::new());
            let issuer = TestIssuer::new();
            let issuer_config = Arc::new(
                IssuerConfig::from_public_key_pem(TEST_ISSUER, TEST_AUDIENCE, issuer.public_key_pem())
                    .expect("a freshly generated test issuer key parses as a valid public key"),
            );
            let audit_path = ledger_dir.join("audit.log");
            let audit = Arc::new(AuditWriter::open(&AuditSinkConfig::File(audit_path)).expect("open the test audit sink file"));
            let authz = AuthzConfig {
                role_table: Arc::new(RoleTable::from_config(&default_roles())),
                delegations: Arc::new(DelegationTable::from_delegations(vec![])),
                mfa_amr_methods: Arc::new(vec![]),
                mfa_acr: Arc::new(String::new()),
                audit,
                service_role_table: Arc::new(ServiceRoleTable::from_config(&default_service_roles()).expect("this fixture's own service-role table always uses recognized rpc names")),
                counters: Arc::new(Counters::new()),
            };

            let servicer = CommandAuthorityServiceImpl::new(
                ledger.clone(),
                bundle,
                3_600_000_000_000, // matches profiles/execution.yaml's authority.rate_window_ns
                clock.clone() as Arc<dyn Clock>,
                dispatch_sink as Arc<dyn DispatchSink>,
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

            // Two independent handles onto the SAME connection target: the restricted gateway
            // seam (propose only, D4) and the raw generated client (Check/Authorize/Dispatch/
            // Ack) -- `tonic::transport::Channel` is cheaply `Clone` (an HTTP/2 connection
            // handle), so this is two logical clients over what may be one real connection,
            // never two separately-behaving servers.
            let authority = Arc::new(ProposeOnlyAuthority::from_channel(channel.clone()));
            let raw_client = CommandAuthorityServiceClient::new(channel);

            let evidence_ledger_dir = crate::common::tmp_dir(&format!("{name}-evidence"));
            let evidence_ledger = Arc::new(Ledger::open(&evidence_ledger_dir).expect("open evidence ledger"));

            Self {
                authority,
                raw_client,
                ledger_dir,
                evidence_ledger,
                evidence_ledger_dir,
                clock,
                issuer,
                shutdown_tx: Some(shutdown_tx),
                handle: Some(handle),
            }
        }

        /// Shuts the server task down cleanly and **joins** it -- the original process is
        /// genuinely gone, not merely "this test stopped calling it" -- but leaves both ledger
        /// directories on disk, for a second, independent `Ledger::open` to read.
        pub async fn shutdown_keep_ledgers(mut self) -> (PathBuf, PathBuf) {
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle.await.expect("server task joins cleanly at test end");
            }
            (self.ledger_dir, self.evidence_ledger_dir)
        }
    }
}

use std::path::Path;

use av_cdm::pb::{
    query_request::Selector, AckLevel, AckRequest, AuthorizeRequest, CommandState, CommandTransition, DispatchRequest, LedgerRecord, QueryRequest, RunIdentity,
};
use av_command::counters::Counters;
use av_command::ledger::Ledger;
use av_command::policy::{self, PolicyBundle};
use av_command::test_support::{claims_with_roles_and_mfa, valid_claims, RoleAndMfaClaims};
use av_gateway::evidence::EvidenceRecorder;
use av_gateway::propose_flow::{propose_command, ProposeCommandInput};
use harness::{default_service_roles, real_policy_dir, FullTrailHarness, TEST_AUDIENCE, TEST_ISSUER};
use prost::Message as _;

const TOKEN_NOW_UNIX_S: i64 = 1_760_000_000;
const TOKEN_TTL_S: i64 = 3_600;

/// See this file's module doc's "tamper-detection technique" section: restates
/// `av_command::ledger`'s documented, public on-disk contract (SHA-256 hex of the partition
/// name, `.ledger` suffix; length-prefixed `LedgerRecord` frames) rather than reaching into
/// that crate's private functions -- a third, independent restatement of the same contract
/// `crates/av-command/tests/grpc_service.rs`'s own helpers of these same names already give.
fn ledger_file_path(ledger_dir: &Path, partition: &str) -> std::path::PathBuf {
    let digest = openssl::sha::sha256(partition.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    ledger_dir.join(format!("{hex}.ledger"))
}

fn read_ledger_records(ledger_dir: &Path, partition: &str) -> Vec<LedgerRecord> {
    let bytes = std::fs::read(ledger_file_path(ledger_dir, partition)).expect("read partition file");
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        out.push(LedgerRecord::decode(&bytes[offset..offset + len]).expect("decode LedgerRecord frame"));
        offset += len;
    }
    out
}

fn write_ledger_records(ledger_dir: &Path, partition: &str, records: &[LedgerRecord]) {
    let mut bytes = Vec::new();
    for r in records {
        let body = r.encode_to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
    }
    std::fs::write(ledger_file_path(ledger_dir, partition), bytes).expect("write partition file");
}

/// **The A6 acceptance test.** Drives a real command through the gateway's real propose path
/// (a real `ProposalEvidence` record results), then `Check`/`Authorize`/`Dispatch`/`Ack` on a
/// real `CommandAuthorityService` -- five real transitions, a real policy decision, a real
/// proposal with a real rationale and real evidence ids. Shuts that service down (joined, not
/// merely dropped), then opens a SECOND, independent `Ledger` handle over the exact same
/// on-disk directory the first process wrote -- the original process is genuinely gone -- and
/// proves every transition, the decision id, the policy hash, and the proposal's rationale and
/// evidence ids all come back identically, that the decision id itself re-derives (not merely
/// re-reads) from the recorded input and policy hash, that the chain verifies, and that a
/// deliberate tamper is caught and named at its exact sequence number.
#[tokio::test]
async fn ledger_decision_trail_and_proposal_reproduce_after_the_process_is_gone() {
    let harness = FullTrailHarness::spawn("decision-trail", 1_000).await;

    const COMMAND_ID: &str = "trail-cmd-1";
    const ENTITY_ID: &str = "sat-trail";
    let rationale = "burn approach corridor clear per the latest conjunction scan".to_string();
    let evidence_ids = vec!["conjunction-scan-report-7".to_string(), "clearance-check-3".to_string()];

    // ================================================================================
    // Drive the real command: Propose (through the gateway's own real propose_command,
    // never a hand-built ledger record) -> Check -> Authorize -> Dispatch -> Ack.
    // ================================================================================
    let input = ProposeCommandInput {
        command_id: COMMAND_ID.to_string(),
        entity_id: ENTITY_ID.to_string(),
        command_class: "mode".to_string(),
        hazardous: false,
        envelope_id: String::new(),
        idempotency_key: "idem-trail-1".to_string(),
        rationale: rationale.clone(),
        evidence_ids: evidence_ids.clone(),
        principal: "model-x".to_string(),
        model_version: "2.1.0".to_string(),
        run: Some(RunIdentity { run_id: "run-alpha".to_string(), config_hash: "hash-alpha".to_string() }),
        query_ids: vec!["q-1".to_string(), "q-2".to_string()],
    };
    let flow_counters = Counters::new();
    let output = propose_command(&harness.authority, &harness.evidence_ledger, &*harness.clock, &flow_counters, input)
        .await
        .expect("the gateway's own real propose path succeeds -- Propose now checks automatically (question 209(a))");
    assert_eq!(output.command.state, CommandState::Checked as i32);
    assert_eq!(output.command.transitions.len(), 2, "PROPOSED then CHECKED, from the ONE Propose call -- the trail is unchanged, just no longer a second RPC");
    let live_evidence = output.evidence.clone();
    assert_eq!(live_evidence.command_id, COMMAND_ID);
    assert!(!live_evidence.query_ids.is_empty(), "sanity: the evidence is real, not a default");

    let mut raw_client = harness.raw_client.clone();

    // The automatic check's own PolicyDecision -- `ProposeOnlyAuthority` (D4's structural
    // minimalism) returns only the `Command`, never the `PolicyDecision` alongside it, so
    // this reads it back through the raw, unrestricted client's own real `Query` -- the
    // IDENTICAL decision `Propose`'s own automatic check already recorded, never a second one
    // this test provokes by calling `Check` again (which would now be refused: the command is
    // already `CHECKED`).
    let queried = raw_client.query(QueryRequest { selector: Some(Selector::CommandId(COMMAND_ID.to_string())) }).await.expect("Query").into_inner();
    let live_decision = queried.decisions.get(COMMAND_ID).expect("Propose's automatic check always records a decision on success").clone();
    assert!(live_decision.allow, "the shipped policy admits the \"mode\" class: {live_decision:?}");

    let auth_token = harness.issuer.mint(&valid_claims(TEST_ISSUER, TEST_AUDIENCE, "operator-1", TOKEN_NOW_UNIX_S, TOKEN_TTL_S));
    let authorized = raw_client
        .authorize(AuthorizeRequest { command_id: COMMAND_ID.to_string(), principal_token: auth_token, delegation_id: String::new() })
        .await
        .expect("Authorize")
        .into_inner();
    assert_eq!(authorized.command.as_ref().unwrap().state, CommandState::Authorized as i32);

    let dispatch_token = harness.issuer.mint(&claims_with_roles_and_mfa(
        TEST_ISSUER,
        TEST_AUDIENCE,
        "ground-segment-1",
        TOKEN_NOW_UNIX_S,
        TOKEN_TTL_S,
        RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" },
    ));
    assert!(default_service_roles().get("dispatchers").unwrap().contains(&"dispatch".to_string()), "sanity: the fixture role really grants dispatch");
    let dispatched = raw_client
        .dispatch(DispatchRequest { command_id: COMMAND_ID.to_string(), service_token: dispatch_token })
        .await
        .expect("Dispatch")
        .into_inner();
    assert_eq!(dispatched.command.as_ref().unwrap().state, CommandState::Dispatched as i32);

    let ack_token = harness.issuer.mint(&claims_with_roles_and_mfa(
        TEST_ISSUER,
        TEST_AUDIENCE,
        "flight-software",
        TOKEN_NOW_UNIX_S,
        TOKEN_TTL_S,
        RoleAndMfaClaims { groups: &["dispatchers"], amr: &[], acr: "" },
    ));
    let acked = raw_client
        .ack(AckRequest {
            command_id: COMMAND_ID.to_string(),
            ack_level: AckLevel::AssetExecuted as i32,
            principal: "flight-software".to_string(),
            reason: "executed".to_string(),
            service_token: ack_token,
        })
        .await
        .expect("Ack")
        .into_inner();
    let live_command = acked.command.expect("command present");
    assert_eq!(live_command.state, CommandState::Acked as i32);
    // The live, ground-truth transition history -- captured from the real RPC responses,
    // never reconstructed -- this is what Part 2 below must reproduce byte for byte.
    let live_transitions: Vec<CommandTransition> = live_command.transitions.clone();
    assert_eq!(
        live_transitions.iter().map(|t| CommandState::try_from(t.state).unwrap()).collect::<Vec<_>>(),
        vec![CommandState::Proposed, CommandState::Checked, CommandState::Authorized, CommandState::Dispatched, CommandState::Acked],
        "sanity: the live trail really has all five real transitions, in order"
    );

    // ================================================================================
    // The process is gone: shut the server down and JOIN the task (not merely dropped),
    // keeping both ledger directories on disk.
    // ================================================================================
    let (ledger_dir, evidence_ledger_dir) = harness.shutdown_keep_ledgers().await;

    // ================================================================================
    // Part A: the command ledger, reopened by a SECOND, independent `Ledger` handle.
    // ================================================================================
    let ledger2 = Ledger::open(&ledger_dir).expect("reopen the command ledger from a second, independent process's point of view");

    let commands = ledger2.scan_commands().expect("scan_commands");
    let reconstructed = commands.get(COMMAND_ID).expect("the command must be reconstructible from the ledger alone");
    assert_eq!(reconstructed.transitions, live_transitions, "every transition must reproduce identically from the ledger alone");
    assert_eq!(reconstructed.state, CommandState::Acked as i32);

    let decisions = ledger2.scan_decisions().expect("scan_decisions");
    let reconstructed_decision = decisions.get(COMMAND_ID).expect("the policy decision must be reconstructible from the ledger alone");
    assert_eq!(reconstructed_decision.decision_id, live_decision.decision_id);
    assert_eq!(reconstructed_decision.policy_hash, live_decision.policy_hash);
    assert_eq!(reconstructed_decision.allow, live_decision.allow);
    assert_eq!(reconstructed_decision.reasons, live_decision.reasons);
    assert_eq!(reconstructed_decision.evaluated_tai_ns, live_decision.evaluated_tai_ns);
    assert_eq!(reconstructed_decision.input, live_decision.input);

    let proposals = ledger2.scan_proposals().expect("scan_proposals");
    let reconstructed_proposal = proposals.get(COMMAND_ID).expect("the original proposal must be reconstructible from the ledger alone");
    assert_eq!(reconstructed_proposal.rationale, rationale, "the proposal's rationale must reproduce identically");
    assert_eq!(reconstructed_proposal.evidence_ids, evidence_ids, "the proposal's evidence ids must reproduce identically");

    let verification = ledger2.verify(ENTITY_ID).expect("verify");
    assert!(verification.ok, "{verification:?}");
    assert_eq!(verification.checked, 5);

    // ================================================================================
    // Part B: the gateway's own dedicated evidence ledger, likewise reopened independently.
    // ================================================================================
    let ledger3 = Ledger::open(&evidence_ledger_dir).expect("reopen the evidence ledger from a second, independent process's point of view");
    let recorder = EvidenceRecorder::new(&ledger3);
    let reconstructed_evidence = recorder.read_back(COMMAND_ID).expect("read_back").expect("the evidence record must be present");
    assert_eq!(reconstructed_evidence, live_evidence, "the ProposalEvidence record must reproduce identically from the ledger alone");

    // ================================================================================
    // Non-vacuity guard (round 2's own review required this beside every byte-identity
    // assertion above): an EMPTY trail would also "compare equal" to itself. Prove the
    // reconstructed values are genuinely populated, not both sides trivially empty.
    // ================================================================================
    assert_eq!(reconstructed.transitions.len(), 5, "the reconstructed trail must be the real five-transition history, not an empty or partial one");
    assert!(!reconstructed_decision.decision_id.is_empty(), "the reconstructed decision id must be a real, non-empty value");
    assert!(reconstructed_decision.input.is_some(), "the reconstructed decision must carry its own real PolicyInput");
    assert!(!reconstructed_proposal.rationale.is_empty(), "the reconstructed rationale must be the real, non-empty sentence above, not an empty default");
    assert_eq!(reconstructed_proposal.evidence_ids.len(), 2, "the reconstructed evidence ids must be the real two ids, not an empty Vec");
    assert!(!reconstructed_evidence.query_ids.is_empty(), "the reconstructed evidence's query ids must be real, not an empty default");
    // A second, empty ledger directory really does report an empty trail for the same
    // command id -- proves the assertions above are discriminating real content, not
    // vacuously true for any ledger whatsoever.
    let empty_dir = common::tmp_dir("decision-trail-empty-control");
    let empty_ledger = Ledger::open(&empty_dir).expect("open a fresh, never-appended ledger");
    assert!(!empty_ledger.scan_commands().expect("scan_commands").contains_key(COMMAND_ID), "a genuinely empty ledger must not contain this command id");
    let _ = std::fs::remove_dir_all(&empty_dir);

    // ================================================================================
    // The decision id itself RE-DERIVES from the recorded PolicyInput and policy hash --
    // not merely "the same string came back off disk". `av_command::policy::evaluate` is
    // the one public function that computes a decision_id (`compute_decision_id` itself is
    // a private fn of that module); calling it again, over a FRESH `PolicyBundle::load` of
    // the same policy directory (so `policy_hash` is independently recomputed, not read off
    // the recorded decision) and a FRESH `TestClock` seeded at the recorded
    // `evaluated_tai_ns`, with the recorded `PolicyInput` as input, must reproduce the exact
    // same `decision_id` -- proving the id is a genuine, reproducible function of its inputs,
    // not an opaque token this test only ever compares to itself.
    // ================================================================================
    let fresh_bundle = PolicyBundle::load(real_policy_dir()).expect("independently reload the shipped policy bundle");
    let recorded_input = reconstructed_decision.input.clone().expect("checked above: the decision carries its own input");
    let rederive_clock = av_command::clock::TestClock::new(reconstructed_decision.evaluated_tai_ns);
    let rederived = policy::evaluate(&fresh_bundle, &recorded_input, &rederive_clock);
    assert_eq!(rederived.decision_id, reconstructed_decision.decision_id, "the decision id must RE-DERIVE from the recorded input/policy hash/evaluated_tai_ns through the public evaluate() function, not merely match because it is the same on-disk string read twice");
    assert_eq!(rederived.policy_hash, reconstructed_decision.policy_hash, "sanity: the independently-reloaded bundle hashes to the same policy_hash the ledger recorded");
    assert_eq!(rederived.allow, reconstructed_decision.allow);
    assert_eq!(fresh_bundle.policy_hash(), live_decision.policy_hash, "sanity: the freshly reloaded bundle is genuinely the same bundle that produced the live decision");

    // ================================================================================
    // Deliberate on-disk tamper, LAST (it corrupts the file the assertions above already
    // read): reuses the ledger.rs/grpc_service.rs technique -- mutate one record's body,
    // leave its own hash/prev_hash untouched, rewrite the file, and check `verify()` names
    // the exact sequence number the chain broke at.
    // ================================================================================
    let mut records = read_ledger_records(&ledger_dir, ENTITY_ID);
    assert_eq!(records.len(), 5, "sanity: the real five-record trail is on disk before the tamper");
    assert_eq!(records[2].seq, 3, "sanity: the AUTHORIZED record is seq 3");
    records[2].command_id = "tampered-command-id".to_string(); // hash/prev_hash left untouched
    write_ledger_records(&ledger_dir, ENTITY_ID, &records);

    let ledger4 = Ledger::open(&ledger_dir).expect("reopen the now-tampered ledger");
    let tampered_result = ledger4.verify(ENTITY_ID).expect("verify");
    assert!(!tampered_result.ok, "verify must detect the tampered record");
    assert_eq!(tampered_result.broken_at_sequence, 3, "must name the exact sequence number the tamper is at");
    assert_eq!(tampered_result.checked, 2, "the two records before the tamper (seq 1, 2) still verify as good");
    assert!(tampered_result.detail.contains("tampered"), "{}", tampered_result.detail);

    let _ = std::fs::remove_dir_all(&ledger_dir);
    let _ = std::fs::remove_dir_all(&evidence_ledger_dir);
}

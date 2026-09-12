//! Integration tests for `altavista.v1.CommandAuthorityService` (A1.3, `crates/av-command/
//! src/service.rs`): drive the **real service over a real loopback socket with a real tonic
//! client** (`tonic`'s `"channel"` feature, a `[dev-dependencies]` entry -- see `Cargo.toml`'s
//! and `build.rs`'s own comments for why that is the cleanest way to get a usable generated
//! client stub without this crate's production `tonic` dependency ever gaining the
//! `"channel"`/`"transport"` feature). Every test binds `127.0.0.1:0` (an OS-assigned
//! ephemeral port), never a fixed one, and shuts its server task down cleanly before
//! returning ([`TestServer::shutdown`]) -- no test here leaves a listening task running past
//! its own end.
//!
//! # No network at test time (question 154)
//!
//! Binding and connecting to `127.0.0.1` here is **not** "network at test time" -- question
//! 154's rule is about a test reaching *outside* the host (an image pull, a real DNS lookup,
//! a call to an external service); a loopback socket never leaves the host's own kernel
//! network stack, and every server this file spawns is spawned in-process by the very test
//! that connects to it. Recorded explicitly here (mirroring `crates/av-command/src/
//! policy.rs`'s module doc, which makes the identical record for its own, narrower "no
//! network builtin" claim) so a later reader does not have to re-derive it.
//!
//! # No test mutates the process environment (question 199)
//!
//! Every test constructs its own [`av_command::clock::TestClock`], ledger temp directory and
//! [`av_command::policy::PolicyBundle`] directly; none of them calls `std::env::set_var`/
//! `remove_var`, and no test sleeps -- every "later" is a fresh `TestClock` value or an
//! explicit `advance`/`set` (`TestServer::spawn`'s own clock is exposed on the struct for a
//! test that needs one, though none of the tests below need to move it forward: the
//! `CommandTransition.tai_ns` at each edge only needs to be *some* deterministic value, never
//! a wall-clock read).
//!
//! # Reading/writing the raw ledger file
//!
//! [`ledger_file_path`]/[`read_ledger_records`]/[`write_ledger_records`] below recompute the
//! on-disk partition filename and framing exactly as `crates/av-command/src/ledger.rs`'s
//! module doc documents it (SHA-256 hex of the partition name plus `.ledger`; a big-endian
//! `u32` length prefix then that many `LedgerRecord` bytes) rather than calling any private
//! function of that module -- this file is a separate crate (an integration test) and can
//! only see `av_command`'s public API, so it exercises the *documented* on-disk contract the
//! same way an external auditor would, not an internal shortcut.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use av_cdm::pb::{
    query_request::Selector, AckLevel, AckRequest, AuthorizeRequest, CheckRequest, Command, CommandProposal, CommandState,
    DispatchRequest, LedgerRecord, ProposeRequest, QueryByEntity, QueryRequest, VerifyLedgerRequest,
};
use av_command::clock::{Clock, TestClock};
use av_command::ledger::Ledger;
use av_command::pb::command_authority_service_client::CommandAuthorityServiceClient;
use av_command::pb::command_authority_service_server::CommandAuthorityServiceServer;
use av_command::policy::PolicyBundle;
use av_command::service::{resolve_loopback_bind_address, BindAddressError, CommandAuthorityServiceImpl, DispatchSink, RecordingDispatchSink};
use prost::Message as _;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Endpoint, Server};
use tonic::Code;

/// The real policy directory `profiles/execution.yaml`'s `authority.policy_dir` names,
/// resolved relative to this crate's manifest (matches `tests/policy_fixture.rs`'s identical
/// helper) -- these tests run the shipped `command.rego`, not a private fixture copy.
fn real_policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

fn tmp_ledger_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-command-grpc-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// See this file's module doc: recomputes `crate::ledger`'s documented (not private)
/// filename scheme.
fn ledger_file_path(ledger_dir: &Path, partition: &str) -> PathBuf {
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

fn base_command(id: &str, entity_id: &str, command_class: &str, idempotency_key: &str) -> Command {
    Command {
        id: id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        entity_id: entity_id.to_string(),
        command_class: command_class.to_string(),
        ..Command::default()
    }
}

fn propose_request(command: Command, principal: &str) -> ProposeRequest {
    ProposeRequest { proposal: Some(CommandProposal { command: Some(command), rationale: "integration test".to_string(), evidence_ids: vec![] }), principal: principal.to_string() }
}

/// A running `CommandAuthorityServiceImpl` behind a real loopback socket, plus everything a
/// test needs to inspect what happened: the ledger directory (for
/// [`read_ledger_records`]/[`write_ledger_records`]), the [`RecordingDispatchSink`] (A3's
/// seam -- see `crate::service`'s module doc), and the shared clock. [`Self::shutdown`] must
/// be called at the end of every test that constructs one.
struct TestServer {
    client: CommandAuthorityServiceClient<Channel>,
    ledger_dir: PathBuf,
    clock: Arc<TestClock>,
    dispatch_sink: Arc<RecordingDispatchSink>,
    shutdown_tx: oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Spawns over a **fresh** ledger directory, named `name` (wiped first if it somehow
    /// already exists -- see [`tmp_ledger_dir`]). The overwhelming majority of tests want
    /// this.
    async fn spawn(name: &str, start_tai_ns: i64) -> Self {
        Self::spawn_over(tmp_ledger_dir(name), start_tai_ns).await
    }

    /// Spawns over `ledger_dir` **as it already is** -- never wiped, never created fresh --
    /// so a test can build a *second* `TestServer` over the exact directory a *first* one
    /// (already shut down via [`Self::shutdown_keep_ledger`]) wrote to, proving a property
    /// survives a real process restart rather than merely surviving within one process's own
    /// `Ledger`/`CommandAuthorityServiceImpl` handles.
    async fn spawn_over(ledger_dir: PathBuf, start_tai_ns: i64) -> Self {
        let ledger = Arc::new(Ledger::open(&ledger_dir).expect("open ledger"));
        let bundle = Arc::new(PolicyBundle::load(real_policy_dir()).expect("load the shipped policy bundle"));
        let clock = Arc::new(TestClock::new(start_tai_ns));
        let dispatch_sink = Arc::new(RecordingDispatchSink::new());

        let servicer = CommandAuthorityServiceImpl::new(
            ledger,
            bundle,
            3_600_000_000_000, // matches profiles/execution.yaml's authority.rate_window_ns
            clock.clone() as Arc<dyn Clock>,
            dispatch_sink.clone() as Arc<dyn DispatchSink>,
        )
        .expect("rebuild the duplicate-dispatch guard from the ledger at construction");

        // Bind an OS-assigned ephemeral port -- never a fixed one -- and read the real
        // address back, with no bind-then-drop-then-rebind race (the listener stays open,
        // handed straight to the server).
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral loopback port");
        let addr: SocketAddr = listener.local_addr().expect("local_addr");
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
        let client = CommandAuthorityServiceClient::new(channel);

        Self { client, ledger_dir, clock, dispatch_sink, shutdown_tx, handle }
    }

    /// Shuts the server task down cleanly, **joins** it (proves the task actually stopped,
    /// rather than merely asking it to), and removes its ledger directory -- the ordinary
    /// end-of-test teardown.
    async fn shutdown(self) {
        let ledger_dir = self.shutdown_keep_ledger().await;
        let _ = std::fs::remove_dir_all(&ledger_dir);
    }

    /// Shuts the server task down cleanly and joins it, like [`Self::shutdown`], but
    /// deliberately leaves the ledger directory on disk and returns its path -- for the one
    /// test that needs a *second* `TestServer` to reopen exactly what this instance wrote
    /// ([`Self::spawn_over`]). The caller is responsible for eventually removing the
    /// directory once no further instance needs it.
    async fn shutdown_keep_ledger(self) -> PathBuf {
        let _ = self.shutdown_tx.send(());
        self.handle.await.expect("server task joins cleanly at test end");
        self.ledger_dir
    }
}

/// **Acceptance test 1**: the full legal path end to end, asserting every response's state,
/// the ledger's exact on-disk record sequence, and `VerifyLedger`.
#[tokio::test]
async fn full_legal_path_propose_check_authorize_dispatch_ack_end_to_end() {
    let mut server = TestServer::spawn("legal-path", 1_000).await;

    let proposed = server.client.propose(propose_request(base_command("cmd-1", "sat-1", "mode", "idem-1"), "model-x")).await.expect("Propose").into_inner();
    let proposed_command = proposed.command.expect("command present");
    assert_eq!(proposed_command.state, CommandState::Proposed as i32);
    assert!(proposed.decision.is_none(), "Propose never runs a policy decision");
    // The injected clock, not the wall clock: every transition's own tai_ns traces back to
    // the TestClock this server was spawned with.
    assert_eq!(proposed_command.transitions[0].tai_ns, server.clock.now_tai_ns());

    let checked = server.client.check(CheckRequest { command_id: "cmd-1".to_string() }).await.expect("Check").into_inner();
    let checked_command = checked.command.expect("command present");
    assert_eq!(checked_command.state, CommandState::Checked as i32);
    let decision = checked.decision.expect("Check attaches the PolicyDecision");
    assert!(decision.allow, "{decision:?}");

    let authorized = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-1".to_string(), principal_token: "bearer-abc".to_string(), delegation_id: "delegation-1".to_string() })
        .await
        .expect("Authorize")
        .into_inner();
    let authorized_command = authorized.command.expect("command present");
    assert_eq!(authorized_command.state, CommandState::Authorized as i32);
    assert_eq!(authorized_command.transitions.last().unwrap().principal, "bearer-abc");
    assert_eq!(authorized_command.transitions.last().unwrap().delegation_id, "delegation-1");

    let dispatched = server.client.dispatch(DispatchRequest { command_id: "cmd-1".to_string() }).await.expect("Dispatch").into_inner();
    let dispatched_command = dispatched.command.expect("command present");
    assert_eq!(dispatched_command.state, CommandState::Dispatched as i32);
    assert_eq!(server.dispatch_sink.dispatched().len(), 1);
    assert_eq!(server.dispatch_sink.dispatched()[0].id, "cmd-1");

    let acked = server
        .client
        .ack(AckRequest { command_id: "cmd-1".to_string(), ack_level: AckLevel::AssetExecuted as i32, principal: "flight-software".to_string(), reason: "executed".to_string() })
        .await
        .expect("Ack")
        .into_inner();
    let acked_command = acked.command.expect("command present");
    assert_eq!(acked_command.state, CommandState::Acked as i32);
    assert_eq!(acked_command.transitions.last().unwrap().ack_level, AckLevel::AssetExecuted as i32);

    // The ledger on disk holds exactly the expected five records, in order.
    let records = read_ledger_records(&server.ledger_dir, "sat-1");
    let states: Vec<CommandState> = records.iter().map(|r| CommandState::try_from(r.transition.as_ref().unwrap().state).unwrap()).collect();
    assert_eq!(
        states,
        vec![CommandState::Proposed, CommandState::Checked, CommandState::Authorized, CommandState::Dispatched, CommandState::Acked]
    );
    assert_eq!(records[0].seq, 1);
    assert_eq!(records[4].seq, 5);
    assert!(records[1].decision.is_some(), "the CHECKED record carries the PolicyDecision");

    let verify = server.client.verify_ledger(VerifyLedgerRequest { partition: "sat-1".to_string() }).await.expect("VerifyLedger").into_inner();
    assert!(verify.ok);
    assert_eq!(verify.results.len(), 1);
    assert!(verify.results[0].ok);
    assert_eq!(verify.results[0].checked, 5);

    server.shutdown().await;
}

/// **Acceptance test 2**: a `payload`-class command is `REJECTED` at `Check`, with the
/// policy's exact reason text in the response, over the wire.
#[tokio::test]
async fn check_denies_a_payload_class_command_with_the_policys_exact_reason() {
    let mut server = TestServer::spawn("policy-deny", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-2", "sat-1", "payload", ""), "model-x")).await.expect("Propose");
    let checked = server.client.check(CheckRequest { command_id: "cmd-2".to_string() }).await.expect("Check").into_inner();
    let checked_command = checked.command.expect("command present");
    assert_eq!(checked_command.state, CommandState::Rejected as i32);
    let decision = checked.decision.expect("Check attaches the PolicyDecision even for a denial");
    assert!(!decision.allow);
    assert_eq!(decision.reasons, vec!["command_class payload is not admitted by policy".to_string()]);

    server.shutdown().await;
}

/// **Acceptance test 3**: every illegal edge over the wire is refused `FAILED_PRECONDITION`
/// with the typed `state::CommandError::IllegalTransition` message preserved.
#[tokio::test]
async fn illegal_edges_over_the_wire_are_refused_failed_precondition_with_the_typed_message() {
    let mut server = TestServer::spawn("illegal-edges", 1_000).await;

    // Authorize-before-Check.
    server.client.propose(propose_request(base_command("cmd-a", "sat-1", "mode", ""), "model-x")).await.unwrap();
    let err = server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-a".to_string(), principal_token: "t".to_string(), delegation_id: String::new() })
        .await
        .expect_err("Authorize before Check must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    // Dispatch-before-Authorize.
    server.client.propose(propose_request(base_command("cmd-b", "sat-1", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-b".to_string() }).await.unwrap();
    let err = server.client.dispatch(DispatchRequest { command_id: "cmd-b".to_string() }).await.expect_err("Dispatch before Authorize must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    // Ack-before-Dispatch.
    server.client.propose(propose_request(base_command("cmd-c", "sat-1", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-c".to_string() }).await.unwrap();
    server
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-c".to_string(), principal_token: "t".to_string(), delegation_id: String::new() })
        .await
        .unwrap();
    let err = server
        .client
        .ack(AckRequest { command_id: "cmd-c".to_string(), ack_level: AckLevel::AssetExecuted as i32, principal: "p".to_string(), reason: "r".to_string() })
        .await
        .expect_err("Ack before Dispatch must be refused");
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("illegal command transition"), "{}", err.message());

    server.shutdown().await;
}

/// **Acceptance test 4** (question 53): `Propose` with a non-empty `envelope_id` is refused
/// `INVALID_ARGUMENT`, over the wire, with the typed message.
#[tokio::test]
async fn propose_with_a_nonempty_envelope_id_is_refused_over_the_wire() {
    let mut server = TestServer::spawn("envelope-refused", 1_000).await;

    let mut command = base_command("cmd-env", "sat-1", "mode", "");
    command.envelope_id = "env-station-keeping".to_string();
    let err = server.client.propose(propose_request(command, "model-x")).await.expect_err("a non-empty envelope_id must be refused");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");
    assert!(err.message().contains("propose refuses a non-empty envelope_id"), "{}", err.message());
    assert!(err.message().contains("question 53"), "{}", err.message());

    server.shutdown().await;
}

/// **Acceptance test 5**: two commands sharing one `idempotency_key` -- the first dispatches;
/// the second is refused `ALREADY_EXISTS`, and appends no second ledger record.
#[tokio::test]
async fn dispatch_refuses_a_duplicate_idempotency_key_and_appends_no_second_ledger_record() {
    let mut server = TestServer::spawn("idempotency", 1_000).await;
    let key = "idem-shared";

    for id in ["cmd-x", "cmd-y"] {
        server.client.propose(propose_request(base_command(id, "sat-1", "mode", key), "model-x")).await.unwrap();
        server.client.check(CheckRequest { command_id: id.to_string() }).await.unwrap();
        server.client.authorize(AuthorizeRequest { command_id: id.to_string(), principal_token: "t".to_string(), delegation_id: String::new() }).await.unwrap();
    }

    let first = server.client.dispatch(DispatchRequest { command_id: "cmd-x".to_string() }).await.expect("the first dispatch of this key succeeds").into_inner();
    assert_eq!(first.command.unwrap().state, CommandState::Dispatched as i32);

    let records_before = read_ledger_records(&server.ledger_dir, "sat-1").len();

    let err = server.client.dispatch(DispatchRequest { command_id: "cmd-y".to_string() }).await.expect_err("a second command sharing the dispatched key must be refused");
    assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    assert!(err.message().contains(&format!("idempotency_key {key:?}")), "{}", err.message());
    assert!(err.message().contains("twice"), "{}", err.message());

    let records_after = read_ledger_records(&server.ledger_dir, "sat-1").len();
    assert_eq!(records_before, records_after, "the refused dispatch must append no second ledger record");
    assert_eq!(server.dispatch_sink.dispatched().len(), 1, "only the first command ever reached the DispatchSink");

    server.shutdown().await;
}

/// **The cross-restart acceptance test** (defect the manager's review found: the
/// duplicate-dispatch guard was built empty at every construction, so the guarantee
/// evaporated across a process restart). Dispatches a key through one `TestServer` instance,
/// shuts that instance down keeping its ledger directory, then builds a **second**,
/// independent `TestServer` over that same directory (a different `Ledger` handle, a
/// different `CommandAuthorityServiceImpl`, a fresh in-memory `BTreeSet` before `new` rebuilds
/// it) and asserts the second instance refuses the same key with `ALREADY_EXISTS`, appending
/// no record for the refused attempt -- proving the guard is rebuilt from the ledger itself
/// (`Ledger::scan_dispatched_idempotency_keys`), not carried over by any in-process state.
#[tokio::test]
async fn dispatch_refuses_a_key_already_dispatched_by_a_prior_process_lifetime() {
    let key = "idem-across-restart";

    let mut server1 = TestServer::spawn("idempotency-restart", 1_000).await;
    server1.client.propose(propose_request(base_command("cmd-r1", "sat-r", "mode", key), "model-x")).await.unwrap();
    server1.client.check(CheckRequest { command_id: "cmd-r1".to_string() }).await.unwrap();
    server1
        .client
        .authorize(AuthorizeRequest { command_id: "cmd-r1".to_string(), principal_token: "t".to_string(), delegation_id: String::new() })
        .await
        .unwrap();
    let dispatched = server1
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r1".to_string() })
        .await
        .expect("the first process lifetime's dispatch succeeds")
        .into_inner();
    assert_eq!(dispatched.command.unwrap().state, CommandState::Dispatched as i32);
    let ledger_dir = server1.shutdown_keep_ledger().await;

    let records_before = read_ledger_records(&ledger_dir, "sat-r").len();

    // A second, independent process lifetime: a fresh Ledger handle, a fresh
    // CommandAuthorityServiceImpl, over the SAME on-disk ledger directory. Its own
    // in-memory idempotency set starts empty and must be rebuilt by `new` before this
    // assertion can pass.
    let mut server2 = TestServer::spawn_over(ledger_dir.clone(), 2_000).await;
    server2.client.propose(propose_request(base_command("cmd-r2", "sat-r", "mode", key), "model-x")).await.unwrap();
    server2.client.check(CheckRequest { command_id: "cmd-r2".to_string() }).await.unwrap();
    server2.client.authorize(AuthorizeRequest { command_id: "cmd-r2".to_string(), principal_token: "t".to_string(), delegation_id: String::new() }).await.unwrap();

    let err = server2
        .client
        .dispatch(DispatchRequest { command_id: "cmd-r2".to_string() })
        .await
        .expect_err("a second process lifetime must still refuse a key the first already dispatched");
    assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    assert!(err.message().contains(&format!("idempotency_key {key:?}")), "{}", err.message());

    // The refused dispatch appended PROPOSED/CHECKED/AUTHORIZED for cmd-r2 (three more
    // records than server1 left behind) but no fourth, DISPATCHED one.
    let records_after = read_ledger_records(&ledger_dir, "sat-r").len();
    assert_eq!(records_after, records_before + 3, "cmd-r2 reached AUTHORIZED but never DISPATCHED: {records_after} vs {records_before}");
    assert_eq!(server2.dispatch_sink.dispatched().len(), 0, "the second process lifetime's DispatchSink was never reached");

    server2.shutdown().await;
}

/// **Acceptance test 6**: `Query` by `command_id`, and by `entity_id` with an optional
/// `CommandState` filter.
#[tokio::test]
async fn query_by_id_and_by_entity_with_a_state_filter() {
    let mut server = TestServer::spawn("query", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-q1", "sat-q", "mode", ""), "model-x")).await.unwrap();
    server.client.propose(propose_request(base_command("cmd-q2", "sat-q", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-q2".to_string() }).await.unwrap(); // cmd-q2 -> CHECKED; cmd-q1 stays PROPOSED

    let by_id = server.client.query(QueryRequest { selector: Some(Selector::CommandId("cmd-q1".to_string())) }).await.expect("Query by id").into_inner();
    assert_eq!(by_id.commands.len(), 1);
    assert_eq!(by_id.commands[0].id, "cmd-q1");

    let by_entity_unfiltered = server
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: "sat-q".to_string(), state_filter: CommandState::Unspecified as i32 })) })
        .await
        .expect("Query by entity, unfiltered")
        .into_inner();
    let mut ids: Vec<String> = by_entity_unfiltered.commands.iter().map(|c| c.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["cmd-q1".to_string(), "cmd-q2".to_string()]);

    let by_entity_checked = server
        .client
        .query(QueryRequest { selector: Some(Selector::Entity(QueryByEntity { entity_id: "sat-q".to_string(), state_filter: CommandState::Checked as i32 })) })
        .await
        .expect("Query by entity, filtered to CHECKED")
        .into_inner();
    assert_eq!(by_entity_checked.commands.len(), 1);
    assert_eq!(by_entity_checked.commands[0].id, "cmd-q2");

    server.shutdown().await;
}

/// **Acceptance test 7**: `VerifyLedger` reports a tampered partition as broken, at the
/// exact sequence number the tamper is at.
#[tokio::test]
async fn verify_ledger_reports_a_tampered_partition_as_broken_at_the_right_sequence() {
    let mut server = TestServer::spawn("verify-tamper", 1_000).await;

    server.client.propose(propose_request(base_command("cmd-t", "sat-t", "mode", ""), "model-x")).await.unwrap();
    server.client.check(CheckRequest { command_id: "cmd-t".to_string() }).await.unwrap();
    server.client.authorize(AuthorizeRequest { command_id: "cmd-t".to_string(), principal_token: "t".to_string(), delegation_id: String::new() }).await.unwrap();

    let mut records = read_ledger_records(&server.ledger_dir, "sat-t");
    assert_eq!(records.len(), 3, "sanity: PROPOSED, CHECKED, AUTHORIZED");

    // Tamper record 2's body, leaving prev_hash/hash untouched -- the same technique
    // crates/av-command/src/ledger.rs's own tamper-detection test uses.
    records[1].command_id = "tampered-command-id".to_string();
    write_ledger_records(&server.ledger_dir, "sat-t", &records);

    let resp = server.client.verify_ledger(VerifyLedgerRequest { partition: "sat-t".to_string() }).await.expect("VerifyLedger").into_inner();
    assert!(!resp.ok);
    assert_eq!(resp.results.len(), 1);
    assert!(!resp.results[0].ok);
    assert_eq!(resp.results[0].broken_at_sequence, 2, "must name the exact sequence number the chain broke at");
    assert_eq!(resp.results[0].checked, 1, "the record before the tamper (seq 1) still verifies as good");
    assert!(resp.results[0].detail.contains("tampered"), "{}", resp.results[0].detail);

    server.shutdown().await;
}

/// **Acceptance test 8** (question 155): a non-loopback bind address is refused with a typed
/// error, at this crate's own public bind-address boundary -- no server is spawned for this
/// test at all, since the whole point is that a bad bind address must never reach a real
/// `TcpListener::bind` call.
#[test]
fn non_loopback_bind_addresses_are_refused_with_a_typed_error_naming_question_155() {
    for raw in ["0.0.0.0:50070", "10.1.2.3:50070", "example.com:50070", "[::]:50070"] {
        let err = resolve_loopback_bind_address(raw).expect_err(&format!("{raw:?} must be refused"));
        assert!(matches!(err, BindAddressError::NotLoopback { .. }), "{raw:?} -> {err:?}");
        assert!(err.to_string().contains("question 155"), "{err}");
        assert!(err.to_string().contains(raw), "{err}");
    }

    // Sanity: the loopback spellings this same boundary must accept -- proves the refusal
    // above is a real discriminator, not every address being refused unconditionally.
    for raw in ["127.0.0.1:50070", "localhost:50070", "[::1]:50070"] {
        resolve_loopback_bind_address(raw).unwrap_or_else(|e| panic!("{raw:?} must be accepted: {e}"));
    }
}

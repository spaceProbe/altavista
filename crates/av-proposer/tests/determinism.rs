//! Acceptance evidence 3: "the same run always yields the same proposal." Two complete,
//! independent proposer runs over the same real fixture produce a byte-identical
//! `CommandProposal`'s resulting `Command` (encoded with prost and compared as bytes) and
//! identical evidence -- with the non-vacuity guard round 2's review required (two empty
//! proposals would also compare equal).

mod common;

use prost::Message as _;

use av_command::counters::Counters;
use av_proposer::gateway_client::GatewayClient;
use av_proposer::model_service::ModelIdentity;
use av_proposer::proposer::{run, ProposerConfig, RunOutcome};
use av_proposer::rule::RuleConfig;
use common::GatewayHarness;

fn config() -> ProposerConfig {
    ProposerConfig {
        run_id: common::FIXTURE_RUN_ID.to_string(),
        config_hash: String::new(),
        caller_clearance: "CUI".to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "burn".to_string(),
        rule: RuleConfig { score_name: "demo_flt_rmag_at_end".to_string(), reference_radius_m: 6_871_000.0, threshold_m: 100.0, gain_per_s: 0.001, max_burn_mps: 5.0 },
        model: ModelIdentity { node_id: "av-proposer.station-keeping".to_string(), version: "1.0.0".to_string() },
        rationale_prefix: "determinism test".to_string(),
        // R5.1: a placeholder -- each harness below has its OWN issuer key, so the real token
        // is minted per-harness (`harness.mint_service_token()`) and substituted in via struct
        // update syntax, never shared between two independently-keyed gateways.
        service_token: String::new(),
    }
}

/// Two COMPLETELY INDEPENDENT harnesses (separate ledgers, separate ports, separate
/// processes' worth of state -- the closest this test can get to "two separate runs" while
/// staying in one test binary) over the identical real fixture, driven by the identical
/// `ProposerConfig`, must derive the identical `Command.id`/`idempotency_key` (D4: pure
/// functions of the proposal's own inputs) and produce byte-identical `Command`s once
/// decoded back off each harness's own real ledger.
#[tokio::test]
async fn two_independent_runs_over_the_same_real_fixture_yield_the_same_proposal() {
    let (entries_a, ladder_a) = common::catalogue_over_real_fixture("CUI");
    let harness_a = GatewayHarness::spawn("determinism-a", 1_000, entries_a, ladder_a).await;
    common::assert_harness_is_wired(&harness_a);
    let (entries_b, ladder_b) = common::catalogue_over_real_fixture("CUI");
    let harness_b = GatewayHarness::spawn("determinism-b", 1_000, entries_b, ladder_b).await;
    common::assert_harness_is_wired(&harness_b);

    let mut client_a = GatewayClient::connect(harness_a.endpoint.clone()).await.expect("connect a");
    let mut client_b = GatewayClient::connect(harness_b.endpoint.clone()).await.expect("connect b");
    let counters_a = Counters::new();
    let counters_b = Counters::new();
    let cfg_a = ProposerConfig { service_token: harness_a.mint_service_token(), ..config() };
    let cfg_b = ProposerConfig { service_token: harness_b.mint_service_token(), ..config() };

    let outcome_a = run(&mut client_a, &cfg_a, &counters_a).await.expect("run a proposes");
    let outcome_b = run(&mut client_b, &cfg_b, &counters_b).await.expect("run b proposes");

    let (id_a, idem_a, drift_a, burn_a) = match outcome_a {
        RunOutcome::Proposed { command_id, idempotency_key, drift_m, burn_mps } => (command_id, idempotency_key, drift_m, burn_mps),
        other => panic!("expected Proposed, got {other:?}"),
    };
    let (id_b, idem_b, drift_b, burn_b) = match outcome_b {
        RunOutcome::Proposed { command_id, idempotency_key, drift_m, burn_mps } => (command_id, idempotency_key, drift_m, burn_mps),
        other => panic!("expected Proposed, got {other:?}"),
    };

    // D4: the SAME command id/idempotency key, derived purely from the proposal's own
    // inputs -- never a random id, never dependent on which harness happened to answer.
    assert_eq!(id_a, id_b, "the same run must yield the same Command.id");
    assert_eq!(idem_a, idem_b, "the same run must yield the same idempotency_key");
    assert_eq!(drift_a, drift_b);
    assert_eq!(burn_a, burn_b);

    // Byte-identity of the actual `Command`s each harness's own real ledger recorded --
    // decoded from disk, not from the RPC response either run happened to get back.
    let commands_a = harness_a.command_ledger.scan_commands().expect("scan a");
    let commands_b = harness_b.command_ledger.scan_commands().expect("scan b");
    let command_a = commands_a.get(&id_a).expect("command a on ledger a");
    let command_b = commands_b.get(&id_b).expect("command b on ledger b");
    assert_eq!(command_a.encode_to_vec(), command_b.encode_to_vec(), "two independent runs over the same fixture and config must produce byte-identical Commands");

    // Non-vacuity guard (round 2's review required this beside every byte-identity
    // assertion): the two commands are not merely equal because both are empty/default --
    // they carry real, non-trivial content.
    assert!(!command_a.id.is_empty());
    assert!(!command_a.transitions.is_empty());
    assert_ne!(command_a.encode_to_vec(), av_cdm::pb::Command::default().encode_to_vec(), "the compared Command must not be the default/empty value");

    // The evidence records agree too.
    let evidence_a = av_gateway::evidence::EvidenceRecorder::new(&harness_a.evidence_ledger).read_back(&id_a).expect("read_back a").expect("evidence a");
    let evidence_b = av_gateway::evidence::EvidenceRecorder::new(&harness_b.evidence_ledger).read_back(&id_b).expect("read_back b").expect("evidence b");
    assert_eq!(evidence_a.run, evidence_b.run);
    assert_eq!(evidence_a.model_identity, evidence_b.model_identity);
    assert_eq!(evidence_a.model_version, evidence_b.model_version);
    assert!(!evidence_a.query_ids.is_empty());
    // Manager's review: the query ids must agree BETWEEN the two runs, not merely be
    // non-empty in one of them. They are the whole of "what the model saw" that a replay
    // resolves (`ProposalEvidence.query_ids`), so a pair of runs whose commands were
    // byte-identical but whose evidence pointed at different queries would still break A4's
    // own acceptance claim.
    assert_eq!(evidence_a.query_ids, evidence_b.query_ids, "the same run must ground its proposal in the same query ids");

    harness_a.shutdown().await;
    harness_b.shutdown().await;
}

/// D4/D8: re-issuing the identical query against the SAME harness reproduces the identical
/// `query_id` -- the deterministic-by-construction claim `crate::gateway_client`'s own
/// callers rely on when they say "the ids of the queries it issued" are reproducible.
#[tokio::test]
async fn reissuing_the_identical_query_reproduces_the_identical_query_id() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("determinism-query-id", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect");
    let request = av_cdm::pb::GatewayQueryRequest {
        run: Some(av_cdm::pb::RunIdentity { run_id: common::FIXTURE_RUN_ID.to_string(), config_hash: String::new() }),
        caller_clearance: "CUI".to_string(),
        selector: av_cdm::pb::GatewaySelector::Scores as i32,
        caller_supplied_products_uri: String::new(),
        caller_token: harness.mint_service_token(),
        // H2c (crates/av-gateway): this test never asks for GATEWAY_SELECTOR_CATALOG.
        catalog_query: None,
    };
    let first = client.query(request.clone()).await.expect("first query").query_id;
    let second = client.query(request).await.expect("second query").query_id;
    assert_eq!(first, second);
    assert!(!first.is_empty());

    harness.shutdown().await;
}

//! Acceptance evidence 2 ("a proposal carries the evidence ids that replay resolves") and 4
//! ("the rule's refusals"), against the REAL, committed `demo_two_instance.runproducts.bin`
//! fixture, over a real `av-gateway` + real `CommandAuthorityServiceImpl` +
//! real on-disk ledger (`tests/common/mod.rs`).

mod common;

use av_cdm::pb::Unit;
use av_command::counters::Counters;
use av_proposer::gateway_client::GatewayClient;
use av_proposer::model_service::ModelIdentity;
use av_proposer::proposer::{run, ProposerConfig, RunOutcome, RunRefusal};
use av_proposer::rule::{RuleConfig, RuleRefusal};
use common::GatewayHarness;

const FLT_SCORE_NAME: &str = "demo_flt_rmag_at_end";
const FLT_SCORE_VALUE: f64 = 6_870_517.488_675_741_5;

fn base_config(score_name: &str, harness: &GatewayHarness) -> ProposerConfig {
    ProposerConfig {
        run_id: common::FIXTURE_RUN_ID.to_string(),
        config_hash: String::new(),
        caller_clearance: "CUI".to_string(),
        entity_id: "sat-1".to_string(),
        command_class: "burn".to_string(),
        rule: RuleConfig { score_name: score_name.to_string(), reference_radius_m: 6_871_000.0, threshold_m: 100.0, gain_per_s: 0.001, max_burn_mps: 5.0 },
        model: ModelIdentity { node_id: "av-proposer.station-keeping".to_string(), version: "1.0.0".to_string() },
        rationale_prefix: "test".to_string(),
        service_token: harness.mint_service_token(),
    }
}

/// D5/acceptance-2: a real proposal, over a real query, carries an evidence record that
/// names the run identity and the query id the gateway actually served, and
/// `EvidenceRecorder::read_back` resolves it from disk for the proposed command id.
#[tokio::test]
async fn a_proposal_carries_the_evidence_the_gateway_actually_served_and_it_reads_back_off_disk() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("evidence-real-fixture", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect to the real gateway");
    let counters = Counters::new();
    let config = base_config(FLT_SCORE_NAME, &harness);

    let outcome = run(&mut client, &config, &counters).await.expect("a real drift past the threshold proposes");
    let (command_id, drift_m, burn_mps) = match outcome {
        RunOutcome::Proposed { command_id, drift_m, burn_mps, .. } => (command_id, drift_m, burn_mps),
        other => panic!("expected a Proposed outcome against the real fixture, got {other:?}"),
    };
    assert!((drift_m - (FLT_SCORE_VALUE - config.rule.reference_radius_m)).abs() < 1e-6);
    assert!(burn_mps.abs() > 0.0);

    let evidence = av_gateway::evidence::EvidenceRecorder::new(&harness.evidence_ledger).read_back(&command_id).expect("read_back").expect("evidence must be present for a real proposal");
    assert_eq!(evidence.run.as_ref().map(|r| r.run_id.as_str()), Some(common::FIXTURE_RUN_ID));
    assert!(!evidence.query_ids.is_empty(), "the evidence must name at least the one query this run issued");
    // R5.1/invariant D: the recorded model_identity is the VERIFIED service token subject,
    // never the caller-declared `principal` (`av_proposer::proposer::run` now sends that
    // empty) -- see `common::SERVICE_TOKEN_SUBJECT`'s own doc comment.
    assert_eq!(evidence.model_identity, common::SERVICE_TOKEN_SUBJECT);
    assert_eq!(evidence.model_version, config.model.version);

    let commands = harness.command_ledger.scan_commands().expect("scan_commands");
    let stored = commands.get(&command_id).expect("the proposed command must be on the real ledger");
    // Question 209(a): Propose now checks automatically, so the real ledger shows CHECKED,
    // not a bare PROPOSED, the instant this run's own ProposeCommand call returns.
    assert_eq!(stored.state, av_cdm::pb::CommandState::Checked as i32);

    harness.shutdown().await;
}

/// Acceptance-4: an absent score is refused, typed, and counted -- never defaulted to zero.
#[tokio::test]
async fn a_score_absent_from_the_real_run_is_refused_typed_and_counted() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("rule-absent", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);
    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect");
    let counters = Counters::new();
    let config = base_config("no_such_score_in_this_run", &harness);

    let err = run(&mut client, &config, &counters).await.unwrap_err();
    assert!(matches!(err, RunRefusal::Rule(RuleRefusal::ScoreAbsent { .. })), "{err:?}");
    assert_eq!(counters.get("rule_score_absent"), 1);

    harness.shutdown().await;
}

/// Acceptance-4: a score present but in the wrong unit is refused, typed, and counted --
/// never compared across units. `demo_flt_cd_at_end` (the fixture's real drag coefficient
/// score) is `UNIT_DIMENSIONLESS`, not `UNIT_METER` -- a real cross-unit mismatch, not a
/// hand-built one.
#[tokio::test]
async fn a_real_score_in_the_wrong_unit_is_refused_typed_and_counted() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("rule-wrong-unit", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);
    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect");
    let counters = Counters::new();
    let config = base_config("demo_flt_cd_at_end", &harness);

    let err = run(&mut client, &config, &counters).await.unwrap_err();
    match &err {
        RunRefusal::Rule(RuleRefusal::ScoreWrongUnit { expected, actual, .. }) => {
            assert_eq!(*expected, Unit::Meter);
            assert_eq!(*actual, Unit::Dimensionless);
        }
        other => panic!("expected ScoreWrongUnit, got {other:?}"),
    }
    assert_eq!(counters.get("rule_score_wrong_unit"), 1);

    harness.shutdown().await;
}

/// Acceptance-4: a non-finite score is refused, typed, and counted. The real fixture carries
/// no non-finite score, so this test builds a SECOND, separate catalogue over a hand-built
/// `RunProducts` carrying one -- the only one of these three refusals that genuinely needs a
/// synthetic value to be reachable at all (mirrors `crates/av-gateway/src/catalogue.rs`'s own
/// `from_raw_bytes` test-only constructor, used the same way there for its own otherwise-
/// unreachable refusal).
#[tokio::test]
async fn a_non_finite_score_is_refused_typed_and_counted() {
    use std::collections::BTreeMap;

    let mut entries = BTreeMap::new();
    let run_products = av_cdm::pb::RunProducts {
        run_id: "run-with-a-nan-score".to_string(),
        provenance: Some(av_cdm::pb::Provenance::default()),
        scores: BTreeMap::from([("nan_score".to_string(), av_cdm::pb::ScoreResult { name: "nan_score".to_string(), value: f64::NAN, unit: Unit::Meter as i32, passed: None })]),
        ..Default::default()
    };
    entries.insert("run-with-a-nan-score".to_string(), av_gateway::catalogue::CatalogueEntry::from_run_products(av_cdm::pb::Label { marking: "CUI".to_string(), caveats: vec![] }, &run_products));
    let ladder = av_gateway::labels::ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]);
    let harness = GatewayHarness::spawn("rule-non-finite", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);

    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect");
    let counters = Counters::new();
    let mut config = base_config("nan_score", &harness);
    config.run_id = "run-with-a-nan-score".to_string();

    let err = run(&mut client, &config, &counters).await.unwrap_err();
    assert!(matches!(err, RunRefusal::Rule(RuleRefusal::ScoreNotFinite { .. })), "{err:?}");
    assert_eq!(counters.get("rule_score_not_finite"), 1);

    harness.shutdown().await;
}

/// Not one of the three refusals, but the OTHER legitimate typed outcome D5 asks for: a
/// drift within the declared threshold proposes nothing, and is reported as such rather than
/// as a refusal.
#[tokio::test]
async fn a_drift_within_threshold_over_the_real_fixture_reports_no_proposal_needed() {
    let (entries, ladder) = common::catalogue_over_real_fixture("CUI");
    let harness = GatewayHarness::spawn("rule-within-threshold", 1_000, entries, ladder).await;
    common::assert_harness_is_wired(&harness);
    let mut client = GatewayClient::connect(harness.endpoint.clone()).await.expect("connect");
    let counters = Counters::new();
    let mut config = base_config(FLT_SCORE_NAME, &harness);
    // A reference so close to the real value that the drift falls inside a generous
    // threshold -- still the REAL scored value, just configured (D3's own knob) not to
    // trigger.
    config.rule.reference_radius_m = FLT_SCORE_VALUE;
    config.rule.threshold_m = 1_000.0;

    let outcome = run(&mut client, &config, &counters).await.expect("no refusal for a drift within threshold");
    assert!(matches!(outcome, RunOutcome::NoProposalNeeded { .. }), "{outcome:?}");

    harness.shutdown().await;
}

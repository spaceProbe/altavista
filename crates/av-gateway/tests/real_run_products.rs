//! Acceptance evidence 6 (D8 determinism) and 7 (real run products, not a hand-built
//! fixture), together: the catalogue this test builds is loaded from a REAL, git-tracked
//! `RunProducts` fixture (`tests/fixtures/demo_measurements.runproducts.bin`) -- produced by
//! a real `crates/av-kernel` `execute()` run at some point in this repository's history
//! (already used by this same repository's own Python test suite, e.g. the analogous
//! `demo_two_instance.runproducts.bin`/`test_feasibility_join.py` fixtures) -- never a
//! `RunProducts` literal this test assembled by hand.
//!
//! ## Which of the three options this task named, and why
//!
//! The task offered three ways to get real run products into a test: read a committed
//! `.pb` fixture, invoke the `av-run` binary, or add a dev-dependency on `av-kernel` and
//! call `execute()` directly. This file reads the committed fixture
//! (`demo_measurements.runproducts.bin`, chosen over the other two committed fixtures
//! because it is the only one of the three with real trajectories, events AND measurements
//! all non-empty -- `demo_two_instance.runproducts.bin` has no measurements, and
//! `demo_attitude_control.runproducts.bin` is 3.8 MB, unnecessarily large for what this test
//! needs to prove). This is the cheapest option that still proves the real thing: it needs
//! no `av-kernel` dev-dependency (this task's own instructions say not to touch
//! `crates/av-kernel/` at all, and a dev-dependency on it would still mean linking and
//! running kernel code this task was not scoped to touch), it needs no subprocess, and the
//! bytes are exactly what a real `execute()` run wrote to disk, byte for byte, since this
//! test reads the fixture file's raw bytes directly into a catalogue entry rather than
//! decoding and re-encoding them.
//!
//! ## R3.4: the command-trail gap, closed
//!
//! `docs/aiplane-plan.md`'s round-2 declared gap named this fixture's own limit plainly: it is
//! the measurements demo, so its `events` carry no `EVENT_KIND_COMMAND_TRANSITION` at all --
//! the gateway was proven against a real run, but not yet against one whose events include a
//! command trail. `crates/av-gateway/tests/command_trail_run_products.rs` closes that gap: a
//! second real, git-tracked fixture (`tests/fixtures/demo_command_trail.runproducts.bin`, a
//! real `CommandAuthorityService` + `av_kernel::drm::execute` run's own command trail) served
//! through a real `DataGatewayService` over a real loopback socket, with the specific state
//! sequence and command id named and asserted, sorted order checked, and label enforcement
//! proven to apply to it exactly like it does to this file's own fixture.

mod common;

use av_cdm::pb::{GatewayQueryRequest, GatewaySelector, Label, RunIdentity};
use av_gateway::catalogue::{CatalogueEntry, RunCatalogue};
use av_gateway::counters::Counters;
use av_gateway::evidence::EvidenceRecorder;
use av_gateway::gateway::GatewayCore;
use av_gateway::labels::ClearanceLadder;
use av_cdm::pb::ProposalEvidence;
use av_command::clock::TestClock;
use av_command::ledger::Ledger;
use prost::Message as _;
use std::collections::BTreeMap;
use std::sync::Arc;

const FIXTURE_RUN_ID: &str = "demo-measurements-frozen";

fn fixture_bytes() -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/demo_measurements.runproducts.bin");
    std::fs::read(&path).unwrap_or_else(|e| panic!("read real RunProducts fixture at {}: {e}", path.display()))
}

fn gateway_over_fixture() -> GatewayCore {
    let mut entries = BTreeMap::new();
    entries.insert("run-fixture".to_string(), CatalogueEntry::from_raw_bytes(Label { marking: "CUI".to_string(), caveats: vec![] }, fixture_bytes()));
    let ladder = ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    GatewayCore::new(RunCatalogue::new(entries), ladder, Arc::new(Counters::new()))
}

fn all_request() -> GatewayQueryRequest {
    GatewayQueryRequest {
        run: Some(RunIdentity { run_id: "run-fixture".to_string(), config_hash: String::new() }),
        caller_clearance: "CUI".to_string(),
        selector: GatewaySelector::All as i32,
        caller_supplied_products_uri: String::new(),
        caller_token: String::new(), // GatewayCore::query itself is auth-agnostic (crate::gateway::authenticated_query is the authenticated layer).
    }
}

/// Sanity: the fixture is real (its own `run_id`/`provenance.run_id` are the kernel's own,
/// not this test's), and every one of the three non-`scores` selectors this fixture has
/// data for actually resolves through the real gateway logic.
#[test]
fn the_fixture_is_a_real_run_and_every_populated_selector_resolves() {
    let core = gateway_over_fixture();
    let resp = core.query(&all_request()).expect("ALL selector over the real fixture");
    let run_products_run_id = {
        // Decode the raw fixture bytes directly (not through the gateway) to assert the
        // gateway's own response is really carrying this run's real content, not a stub.
        let rp = av_cdm::pb::RunProducts::decode(fixture_bytes().as_slice()).expect("fixture decodes as RunProducts");
        assert_eq!(rp.run_id, FIXTURE_RUN_ID);
        assert!(!rp.trajectories.is_empty());
        assert!(!rp.events.is_empty());
        assert!(!rp.measurements.is_empty());
        rp.run_id
    };
    assert_eq!(run_products_run_id, FIXTURE_RUN_ID);
    assert!(!resp.trajectories.is_empty());
    assert!(!resp.events.is_empty());
    assert!(!resp.measurements.is_empty());

    for selector in [GatewaySelector::Trajectories, GatewaySelector::Events, GatewaySelector::Measurements] {
        let mut req = all_request();
        req.selector = selector as i32;
        core.query(&req).unwrap_or_else(|e| panic!("selector {selector:?} over the real fixture: {e}"));
    }

    // The fixture genuinely has no scores -- proves ProductMissingOnHost is reachable
    // against real data, not only a hand-built empty map.
    let mut scores_req = all_request();
    scores_req.selector = GatewaySelector::Scores as i32;
    let err = core.query(&scores_req).unwrap_err();
    assert!(matches!(err, av_gateway::gateway::RefusalReason::ProductMissingOnHost { .. }), "{err:?}");
}

/// D8: "the same run and the same query always produce byte-identical responses." Two
/// INDEPENDENTLY constructed gateway sessions (separate catalogues, both loaded from the
/// same real fixture bytes; separate `Counters`) over the identical query produce
/// byte-identical `GatewayQueryResponse` wire encodings.
#[test]
fn two_gateway_sessions_over_the_same_real_catalogue_produce_byte_identical_responses() {
    let session_a = gateway_over_fixture();
    let session_b = gateway_over_fixture();

    let resp_a = session_a.query(&all_request()).expect("session a");
    let resp_b = session_b.query(&all_request()).expect("session b");

    assert_eq!(resp_a.encode_to_vec(), resp_b.encode_to_vec(), "two independent sessions over the same real catalogue must encode identically");
    assert_eq!(resp_a.query_id, resp_b.query_id);
}

/// D8's ledger half, mirroring `crates/av-command/src/ledger.rs`'s own
/// `two_fresh_ledgers_given_the_same_input_produce_byte_identical_files` test: two fresh
/// evidence ledgers, given the identical input and the identical injected clock reading,
/// produce byte-identical partition files on disk -- read back as raw bytes, not decoded.
#[test]
fn two_fresh_evidence_ledgers_given_the_same_input_produce_byte_identical_files() {
    let dir_a = common::tmp_dir("determinism-evidence-a");
    let dir_b = common::tmp_dir("determinism-evidence-b");

    let evidence = ProposalEvidence {
        command_id: "cmd-determinism".to_string(),
        run: Some(RunIdentity { run_id: "run-fixture".to_string(), config_hash: "hash-fixture".to_string() }),
        query_ids: vec!["q1".to_string(), "q2".to_string()],
        model_identity: "model-x".to_string(),
        model_version: "1.0.0".to_string(),
        model_node_id: "model-x-node".to_string(),
        recorded_tai_ns: 5_000,
    };

    for dir in [&dir_a, &dir_b] {
        let ledger = Ledger::open(dir).expect("open ledger");
        let clock = TestClock::new(5_000);
        EvidenceRecorder::new(&ledger).record(&evidence, &clock).expect("record");
    }

    let partition = av_gateway::evidence::evidence_partition("cmd-determinism");
    let digest = openssl::sha::sha256(partition.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let bytes_a = std::fs::read(dir_a.join(format!("{hex}.ledger"))).expect("read partition file a");
    let bytes_b = std::fs::read(dir_b.join(format!("{hex}.ledger"))).expect("read partition file b");
    assert_eq!(bytes_a, bytes_b, "two fresh evidence ledgers given the same input must be byte-identical on disk");
    assert!(!bytes_a.is_empty());

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

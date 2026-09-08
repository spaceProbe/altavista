//! M25.4a (`docs/open-questions.md` question 175): the `PortTrafficLog` sidecar
//! `crate::drm::executor::execute` writes beside `RunProducts` when [`RunConfig::products_dir`]
//! is `Some` -- `crate::router::Router` (the single choke point every FRAMED/BYTE_STREAM frame
//! a run carries passes through) records every one of them, `execute()` sorts, serializes,
//! writes, and hashes them (`RunProducts.port_traffic_hash`), and records the file's own
//! location in `provenance.attributes["port_traffic_uri"]`. See `crate::router::Router::
//! deliver`/`begin_step`/`take_port_traffic` and `executor::execute`'s own "Port traffic
//! sidecar" module doc section for the full contract this file exercises end to end.
//!
//! ## Hypothesis, stated BEFORE running anything (`drms/demo_command.*.yaml`)
//!
//! `demo_command` (`crates/av-kernel/tests/drm_command.rs`'s own fixture) is a 100 s, 1 Hz run
//! (`start_tai_ns = 1_700_000_000_000_000_000`, `output_period_ns = 1_000_000_000`) with two
//! FRAMED connections, both `link_model: "latency"`, each summing to 3 s total latency
//! (`ground.cmd_out -> flight.cmd_in`, `flight.ack_out -> ground.ack_in`; `demo_command.sos.yaml`'s
//! own header comment: "1.5 s each"). One `command` `Scenario.event` dispatches at
//! `tai_ns = 1_700_000_050_000_000_000` (t = 50 s).
//!
//! **Predicted records: exactly 4**, derived from reading `crate::drm::executor::
//! run_shared_group`/`crate::kernel::HeteroKernel::run_with_ports`/`crate::router::Router`, not
//! from running anything yet:
//!
//! 1. `(instance="ground", port="cmd_out", direction=OUT, tai_ns=1_700_000_050_000_000_000,
//!    sequence=0)`
//! 2. `(instance="flight", port="cmd_in", direction=IN, tai_ns=1_700_000_050_000_000_000,
//!    sequence=0)`
//! 3. `(instance="flight", port="ack_out", direction=OUT, tai_ns=<applied_tai_ns>, sequence=S)`
//! 4. `(instance="ground", port="ack_in", direction=IN, tai_ns=<applied_tai_ns>, sequence=S)`
//!
//! where `<applied_tai_ns>` is the epoch `flight` actually decodes/applies the command and (the
//! same step, `ConstantAccelModel`'s own "apply, then ack" contract) sends its ack -- 3 s of
//! real router latency after dispatch, so `1_700_000_053_000_000_000` if the fixture's own
//! header comment's arithmetic is exactly right, confirmed independently below rather than
//! assumed -- and `S = (applied_tai_ns - start_tai_ns) / output_period_ns + 1`.
//!
//! **This prediction's `<applied_tai_ns>` was WRONG, measured, not retrofitted -- see the test
//! body's own comment at the point it failed for the full account.** The hypothesis above
//! silently equated "the epoch `RunProducts.events`' own `EVENT_KIND_PORT_COMMAND` reports as
//! `applied_tai_ns`" with "the epoch `Router::deliver` actually records the ack's own OUT/IN
//! frames at." Those are two different numbers, one native step (1 s, in this fixture) apart:
//! `av_dynamics::AppliedCommand.applied_tai_ns` is documented (`crates/av-dynamics/src/lib.rs`)
//! as the consuming step's own *start* epoch, while the ack's `Outbox` is handed to `Router::
//! deliver` with that same step's own *result* (end) epoch
//! (`HeteroScheduler::advance_to_with_ports`'s `router.deliver(id, result.t_tai_ns, outbox)`).
//! Measured: the `EVENT_KIND_PORT_COMMAND` event's `applied_tai_ns` is `1_700_000_052_000_000_000`
//! (dispatch + 2 s, not +3 s); the real ack emission epoch -- `applied_tai_ns + flight's own
//! period_ns` -- is `1_700_000_053_000_000_000`, matching the "+3 s" arithmetic the fixture's own
//! header comment describes for the round trip as a whole, just not the value the `PORT_COMMAND`
//! event field itself carries. The corrected reconstruction (`ack_tai_ns`, in the test body) uses
//! the latter.
//!
//! **The cmd_out/cmd_in pair's own `sequence = 0` is the one non-obvious claim, called out
//! explicitly rather than left to be discovered by a failing assertion.** `run_shared_group`'s
//! own command-dispatch loop calls `Router::deliver` directly, once per declared `command`
//! event, *before* the boundary loop that (for `demo_command`, which declares no faults/
//! maneuvers) runs the whole span through exactly one `run_one_span` ->
//! `HeteroKernel::run_with_ports` call -- and `Router::begin_step` (the only thing that ever
//! advances the sequence counter) is called only inside that loop. So the command dispatch's own
//! `deliver` call happens before this run's very first `begin_step`, at `Router`'s own initial
//! `step = 0`. The ack, in contrast, is emitted from inside a real `step_with_ports` call during
//! `run_with_ports`'s own loop, so it gets a real, `begin_step`-advanced sequence. **This was
//! verified by reading `run_shared_group`/`run_with_ports` line by line, not assumed from the
//! `Router::begin_step` doc comment alone** -- see that method's own doc comment for the
//! identical claim, made from the implementer's side.
//!
//! ## Independence (Test A)
//!
//! Test A reconstructs the expected records from facts NO part of `crate::router::Router`'s own
//! recording logic determines: `demo_command.sos.yaml`'s own declared connections/latencies
//! (read directly from the parsed `SosConfiguration` fixture, the same one `execute()` is handed
//! -- not from `Router::build`'s internal `edges`/`port_kinds` tables), the DRM's own declared
//! command dispatch epoch, and the REAL `EVENT_KIND_COMMAND_TRANSITION`
//! DISPATCHED/`EVENT_KIND_PORT_COMMAND` events `RunProducts.events` carries -- produced by
//! `crate::drm::command`/`crate::drm::events`/`ConstantAccelModel::step_with_ports`'s own
//! applied-command bookkeeping, a code path entirely separate from `Router::deliver`'s port-
//! traffic recording (a bug in one cannot silently produce a matching bug in the other). The one
//! number no event carries -- `sequence` -- is derived from `output_period_ns`/`start_tai_ns`
//! (both plain `Scenario`/`DrmOptions` fixture fields) via the tick-index formula above, applied
//! honestly (including its one documented exception, `sequence = 0` for the command dispatch)
//! rather than by asking `Router` what it thinks the answer is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use av_cdm::pb::{DesignReferenceMission, EventKind, PortDirection, PortTrafficLog, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, schema, RunConfig};
use gmat_sys::Gmat;
use prost::Message;

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(drms_path(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

fn load_system(stem: &str) -> SystemDefinition {
    schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"))
}

fn load_command_bundle() -> (DesignReferenceMission, SosConfiguration, BTreeMap<String, SystemDefinition>) {
    let drm = schema::parse_drm_yaml(&read("demo_command.drm.yaml")).expect("DRM parses");
    let sos = schema::parse_sos_yaml(&read("demo_command.sos.yaml")).expect("SosConfiguration parses");
    let flight = load_system("demo_command_flight");
    let ground = load_system("demo_command_ground");
    let mut systems = BTreeMap::new();
    systems.insert(flight.id.clone(), flight);
    systems.insert(ground.id.clone(), ground);
    (drm, sos, systems)
}

fn run_config<'a>(gmat: &'a Gmat, drm: &'a DesignReferenceMission, sos: &'a SosConfiguration, systems: &'a BTreeMap<String, SystemDefinition>, run_id: &str, products_dir: Option<PathBuf>) -> RunConfig<'a> {
    RunConfig { gmat, drm, sos, systems, run_id: run_id.to_string(), error_mode: Default::default(), products_dir, replay: None }
}

/// A fresh, empty scratch directory under the OS temp dir, unique per call within this process
/// (an `AtomicU64` counter alongside the PID -- `cargo test` runs this file's own tests in
/// parallel threads of one process, so the PID alone, this crate's existing `av-run` test
/// convention, is not enough here).
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("port-traffic-sidecar-test-{}-{label}-{n}", std::process::id()));
    assert!(!dir.exists(), "scratch dir {dir:?} must not already exist");
    dir
}

const START_TAI_NS: i64 = 1_700_000_000_000_000_000;
const OUTPUT_PERIOD_NS: i64 = 1_000_000_000;
const COMMAND_TAI_NS: i64 = 1_700_000_050_000_000_000;

/// `sequence` for a real output-tick-driven record at `tai_ns` -- the tick-index formula
/// derived from reading `HeteroKernel::run_with_ports`'s own loop (`Router::begin_step`'s own
/// doc comment states the identical claim from the implementer's side): tick `k` (`tai_ns =
/// start_tai_ns + k * output_period_ns`) gets `sequence = k + 1`.
fn tick_sequence(tai_ns: i64) -> u64 {
    let k = (tai_ns - START_TAI_NS) / OUTPUT_PERIOD_NS;
    assert!(k >= 0 && (tai_ns - START_TAI_NS) % OUTPUT_PERIOD_NS == 0, "tai_ns {tai_ns} is not on this run's own output tick grid");
    k as u64 + 1
}

fn read_port_traffic_log(path: &Path) -> PortTrafficLog {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    PortTrafficLog::decode(bytes.as_slice()).unwrap_or_else(|e| panic!("{path:?} did not decode as a PortTrafficLog: {e}"))
}

/// Test A (the acceptance test). See the module doc comment's "Independence" section for
/// exactly what makes this reconstruction independent of `Router`'s own recording logic.
#[test]
fn demo_command_sidecar_records_match_an_independent_reconstruction_from_fixtures_and_events() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_command_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("test-a");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-traffic-a", Some(dir.clone()))).expect("demo_command executes end to end");

    // -- Independent reconstruction -----------------------------------------------------
    // Connections/latencies: read straight off the parsed SosConfiguration fixture, never off
    // Router::build's own internal edges/port_kinds tables.
    let cmd_conn = sos.connections.iter().find(|c| c.from_instance == "ground" && c.from_port == "cmd_out").expect("the declared ground.cmd_out -> flight.cmd_in connection");
    assert_eq!((cmd_conn.to_instance.as_str(), cmd_conn.to_port.as_str()), ("flight", "cmd_in"));
    let ack_conn = sos.connections.iter().find(|c| c.from_instance == "flight" && c.from_port == "ack_out").expect("the declared flight.ack_out -> ground.ack_in connection");
    assert_eq!((ack_conn.to_instance.as_str(), ack_conn.to_port.as_str()), ("ground", "ack_in"));

    // DISPATCHED epoch: a real EVENT_KIND_COMMAND_TRANSITION event, produced by crate::drm::
    // command -- not by Router.
    let dispatched = products.events.iter().find(|e| e.kind == EventKind::CommandTransition as i32 && e.reference_id == "cmd1" && e.name == "COMMAND_STATE_DISPATCHED").expect("a DISPATCHED transition for cmd1");
    assert_eq!(dispatched.tai_ns, COMMAND_TAI_NS, "sanity: the fixture's own declared dispatch epoch");

    // applied_tai_ns: the real EVENT_KIND_PORT_COMMAND event on `flight` -- produced by
    // ConstantAccelModel::step_with_ports's own applied-command bookkeeping, not by Router.
    //
    // **This is where the hypothesis stated in the module doc comment was WRONG, measured, not
    // retrofitted.** `av_dynamics::AppliedCommand.applied_tai_ns`'s own doc comment (read only
    // AFTER this test first failed here) states it plainly: "the step's own *start* epoch ...
    // not the message's *delivery* epoch". `crate::schedule::HeteroScheduler::
    // advance_to_with_ports`, in contrast, calls `Router::deliver(id, result.t_tai_ns, outbox)`
    // -- that step's own *result* (end) epoch -- for the ack `Outbox` this same step produces.
    // So `applied.tai_ns` (52 s measured, not the 53 s this test originally, wrongly, asserted
    // here) and the ack's own real port-traffic emission epoch (53 s) are genuinely two
    // different numbers, one native step apart -- `applied_tai_ns + flight's own period_ns`,
    // which happens to equal `applied_tai_ns + OUTPUT_PERIOD_NS` in this fixture specifically
    // because `flight`'s declared `step_rate_hz` (1.0) equals the trajectory's own output rate;
    // that equality is a fact about this fixture, not a general law this test relies on beyond
    // it.
    let applied = products.events.iter().find(|e| e.kind == EventKind::PortCommand as i32 && e.entity_id == "flight" && e.name == "accel_scale").expect("the one applied accel_scale command on flight");
    let applied_tai_ns = applied.tai_ns;
    assert_eq!(applied_tai_ns, COMMAND_TAI_NS + 2_000_000_000, "measured: AppliedCommand.applied_tai_ns is the consuming step's own START epoch (t_ns), one period before the step's result epoch the ack is actually emitted at -- see the comment just above");
    let ack_tai_ns = applied_tai_ns + OUTPUT_PERIOD_NS;
    assert_eq!(ack_tai_ns, COMMAND_TAI_NS + 3_000_000_000, "the ack's own real emission epoch (the step's RESULT epoch) is exactly 3 s of real router latency after dispatch, matching the fixture's own header-comment arithmetic -- applied_tai_ns alone (the event field) does not equal this");

    let ack_sequence = tick_sequence(ack_tai_ns);

    let mut expected = vec![
        (PortDirection::Out as i32, "ground".to_string(), "cmd_out".to_string(), COMMAND_TAI_NS, 0u64),
        (PortDirection::In as i32, "flight".to_string(), "cmd_in".to_string(), COMMAND_TAI_NS, 0u64),
        (PortDirection::Out as i32, "flight".to_string(), "ack_out".to_string(), ack_tai_ns, ack_sequence),
        (PortDirection::In as i32, "ground".to_string(), "ack_in".to_string(), ack_tai_ns, ack_sequence),
    ];
    expected.sort_by(|a, b| (a.4, a.1.as_str(), a.2.as_str()).cmp(&(b.4, b.1.as_str(), b.2.as_str())));

    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));
    assert_eq!(log.records.len(), 4, "{:#?}", log.records);
    let actual: Vec<(i32, String, String, i64, u64)> = log.records.iter().map(|r| (r.direction, r.instance.clone(), r.port.clone(), r.tai_ns, r.sequence)).collect();
    assert_eq!(actual, expected, "sidecar records must equal the independently reconstructed expectation");

    // Payload sanity (not a full CCSDS decode, but a real cross-check): each OUT/IN pair of one
    // logical frame must carry byte-identical payloads (Router::deliver copies the same
    // payload to both), and no payload is empty (a real encoded CCSDS packet, not a placeholder).
    for r in &log.records {
        assert!(!r.payload.is_empty(), "{r:?}");
    }
    let cmd_out_payload = log.records.iter().find(|r| r.port == "cmd_out").unwrap().payload.clone();
    let cmd_in_payload = log.records.iter().find(|r| r.port == "cmd_in").unwrap().payload.clone();
    assert_eq!(cmd_out_payload, cmd_in_payload);
    let ack_out_payload = log.records.iter().find(|r| r.port == "ack_out").unwrap().payload.clone();
    let ack_in_payload = log.records.iter().find(|r| r.port == "ack_in").unwrap().payload.clone();
    assert_eq!(ack_out_payload, ack_in_payload);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Test B: the hash in `RunProducts.port_traffic_hash` equals a SHA-256 this test computes
/// itself over the file's bytes read back from disk, and `provenance.attributes[
/// "port_traffic_uri"]` names that exact file.
#[test]
fn port_traffic_hash_matches_an_independently_computed_sha256_of_the_file_and_uri_names_it() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_command_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("test-b");

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-traffic-b", Some(dir.clone()))).expect("demo_command executes end to end");

    assert!(!products.port_traffic_hash.is_empty());
    assert_eq!(products.port_traffic_hash.len(), 64, "lowercase hex SHA-256 is 64 chars: {:?}", products.port_traffic_hash);
    assert!(products.port_traffic_hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "must be lowercase hex: {:?}", products.port_traffic_hash);

    let uri = products.provenance.attributes.get("port_traffic_uri").expect("port_traffic_uri must be set when products_dir is Some");
    let path = PathBuf::from(uri);
    assert_eq!(path, dir.join("port_traffic.pb"), "port_traffic_uri must name the exact file that was written");
    assert!(!products.provenance.attributes.contains_key("port_traffic"), "\"port_traffic\" (the None-case attribute) must be absent when a sidecar was actually written");

    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
    // This crate's own canonical SHA-256 helper is `hash::sha256_hex` (pub(crate) only) -- this
    // is a separate `tests/*.rs` crate, so it cannot call that private function even though it
    // lives in the same workspace; `sha2` is already a direct, real dependency of `av-kernel`
    // (this crate's own `hash.rs` module doc comment) for exactly this hash, so computing it a
    // second, independent time here (not by calling into `av_kernel::drm::hash` at all) is the
    // actual independent check, not a restatement of the same call.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let independently_computed = format!("{:x}", hasher.finalize());
    assert_eq!(products.port_traffic_hash, independently_computed, "RunProducts.port_traffic_hash must equal a fresh SHA-256 of the exact bytes on disk");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Test C: `products_dir: None` writes no file anywhere under the scratch dir, `port_traffic_hash`
/// is empty, `provenance.attributes["port_traffic"] == "not recorded"`, and `port_traffic_uri`
/// is absent.
#[test]
fn products_dir_none_writes_no_sidecar_and_marks_it_explicitly_not_recorded() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_command_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("test-c");
    // The scratch dir is never even created in this test (nothing writes to it) -- proving
    // "no file anywhere under it" this strongly (a directory that does not exist at all) is
    // stronger than only checking `dir.join("port_traffic.pb")`.
    assert!(!dir.exists());

    let products = execute(run_config(&gmat, &drm, &sos, &systems, "test-port-traffic-c", None)).expect("demo_command executes end to end with no products_dir");

    assert!(!dir.exists(), "products_dir: None must never create any directory, let alone write into one");
    assert_eq!(products.port_traffic_hash, "");
    assert_eq!(products.provenance.attributes.get("port_traffic").map(String::as_str), Some("not recorded"));
    assert!(!products.provenance.attributes.contains_key("port_traffic_uri"), "the two attributes are mutually exclusive: no sidecar means no uri");
}

/// Test D: records are sorted `(sequence, instance, port)`. `demo_command` cannot produce two
/// records sharing a `(sequence, instance)` pair (its own cmd/ack pairs each land on two
/// *different* instances, so `instance` alone already separates them without needing the `port`
/// tie-break) -- see `crate::drm::executor::sort_port_traffic_tests` (a unit test on the sort
/// function itself, in `crates/av-kernel/src/drm/executor.rs`) for the case this integration
/// fixture cannot reach. What IS checked here, honestly, is the weaker fact this fixture COULD
/// disprove: Test A's own `assert_eq!` on `actual` (already in `(sequence, instance, port)`
/// order by construction) already fails if the two real sequence groups (0, then the ack's own
/// tick sequence) were not each internally sorted by instance -- this test restates that
/// specific slice of the claim on its own, directly against the sequence-0 group, so a reader
/// does not have to infer it from Test A's broader equality.
#[test]
fn records_within_one_sequence_are_sorted_by_instance() {
    let _engine = gmat_sys::engine_lock();
    let (drm, sos, systems) = load_command_bundle();
    let gmat = Gmat::setup(&Gmat::default_startup_file()).expect("GMAT setup");
    let dir = scratch_dir("test-d");

    execute(run_config(&gmat, &drm, &sos, &systems, "test-port-traffic-d", Some(dir.clone()))).expect("demo_command executes end to end");
    let log = read_port_traffic_log(&dir.join("port_traffic.pb"));

    let sequence_zero: Vec<&str> = log.records.iter().filter(|r| r.sequence == 0).map(|r| r.instance.as_str()).collect();
    assert_eq!(sequence_zero, vec!["flight", "ground"], "\"flight\" < \"ground\": the sequence-0 group (the command dispatch's own OUT+IN pair) must be sorted by instance");

    let _ = std::fs::remove_dir_all(&dir);
}

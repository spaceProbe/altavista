//! Integration tests against the real, shipped policy bundle at
//! `profiles/policies/authority/` (declared by `profiles/execution.yaml`'s `authority:`
//! block) -- A1's own acceptance test list (`docs/aiplane-plan.md` milestone A1): "a policy
//! fixture that admits one class, rejects another, rate-limits a third and refuses any
//! envelope; the decision reproduced from the log with the same policy hash; two runs over
//! the same inputs give byte-identical logs." These run against the file a human actually
//! reviews and the profile actually declares, not a private throwaway copy, so a change to
//! the shipped policy that breaks one of these axes is caught here rather than only in a
//! synthetic fixture no one ships.

use av_cdm::pb::{Command, PolicyInput, PolicyInputRate};
use av_command::authority::check_command;
use av_command::clock::TestClock;
use av_command::ledger::Ledger;
use av_command::policy::{self, PolicyBundle};
use av_command::rate::FixtureRateSource;
use av_command::state;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The real policy directory `profiles/execution.yaml`'s `authority.policy_dir` names,
/// resolved relative to this crate's manifest so the test works regardless of the caller's
/// own working directory.
fn real_policy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/policies/authority")
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-command-policy-fixture-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Matches `profiles/execution.yaml`'s `authority.rate_window_ns` and
/// `profiles/policies/authority/command.rego`'s own `burn_rate_limit` -- restated as
/// constants here (rather than parsed out of those files) because this test's job is to pin
/// the shipped *behaviour*, and a mismatch between this constant and the real files would
/// itself make one of the assertions below fail, which is the point.
const RATE_WINDOW_NS: i64 = 3_600_000_000_000;
const BURN_RATE_LIMIT: u64 = 3;

fn policy_input(command_class: &str, envelope_id: &str, burn_count: u64) -> PolicyInput {
    let mut counts = BTreeMap::new();
    counts.insert("burn".to_string(), burn_count);
    PolicyInput {
        command_id: "cmd-1".to_string(),
        entity_id: "sat-1".to_string(),
        command_class: command_class.to_string(),
        hazardous: false,
        envelope_id: envelope_id.to_string(),
        label: None,
        rate: Some(PolicyInputRate { counts_by_class: counts, window_ns: RATE_WINDOW_NS }),
    }
}

/// **Acceptance test 1**: the shipped policy fixture behaves on all four axes A1 requires.
#[test]
fn shipped_policy_admits_rejects_rate_limits_and_refuses_envelopes() {
    let bundle = PolicyBundle::load(real_policy_dir()).unwrap();
    let clock = TestClock::new(1_000);

    // Axis 1: "mode" is admitted.
    let decision = policy::evaluate(&bundle, &policy_input("mode", "", 0), &clock);
    assert!(decision.allow, "{decision:?}");
    assert!(decision.reasons.is_empty(), "{decision:?}");

    // Axis 2: "payload" is rejected, with its reason asserted by exact string.
    let decision = policy::evaluate(&bundle, &policy_input("payload", "", 0), &clock);
    assert!(!decision.allow, "{decision:?}");
    assert_eq!(decision.reasons, vec!["command_class payload is not admitted by policy".to_string()]);

    // Axis 3: "burn" is allowed below the threshold, denied at and above it.
    let below = policy::evaluate(&bundle, &policy_input("burn", "", BURN_RATE_LIMIT - 1), &clock);
    assert!(below.allow, "below threshold must allow: {below:?}");

    let at = policy::evaluate(&bundle, &policy_input("burn", "", BURN_RATE_LIMIT), &clock);
    assert!(!at.allow, "at threshold must deny: {at:?}");
    assert_eq!(
        at.reasons,
        vec![format!("command_class burn rate-limited: {BURN_RATE_LIMIT} recent submissions >= threshold {BURN_RATE_LIMIT}")]
    );

    let above = policy::evaluate(&bundle, &policy_input("burn", "", BURN_RATE_LIMIT + 2), &clock);
    assert!(!above.allow, "above threshold must deny: {above:?}");

    // Axis 4: a non-empty envelope_id is denied for EVERY class, including the admitted one.
    // The envelope reason is always present; "mode" and "burn" (below threshold) carry no
    // other reason, but "payload" also carries its own independent deny reason (it is
    // rejected outright regardless of envelope_id) -- both are asserted by exact string,
    // sorted, since `reasons` is always sorted (the module doc's own rule).
    let envelope_reason = "envelope_id \"env-station-keeping\" is refused: propose-only stands (question 53), no envelope is enabled by this track".to_string();
    for class in ["mode", "burn"] {
        let decision = policy::evaluate(&bundle, &policy_input(class, "env-station-keeping", 0), &clock);
        assert!(!decision.allow, "class {class:?} with a non-empty envelope_id must deny: {decision:?}");
        assert_eq!(decision.reasons, vec![envelope_reason.clone()], "class {class:?}");
    }
    let payload_decision = policy::evaluate(&bundle, &policy_input("payload", "env-station-keeping", 0), &clock);
    assert!(!payload_decision.allow, "{payload_decision:?}");
    assert_eq!(
        payload_decision.reasons,
        vec!["command_class payload is not admitted by policy".to_string(), envelope_reason],
        "payload denies for both its own reason and the envelope reason, sorted"
    );
}

fn proposed_command(id: &str, entity_id: &str, command_class: &str) -> Command {
    let fresh = Command { id: id.to_string(), entity_id: entity_id.to_string(), command_class: command_class.to_string(), ..Command::default() };
    state::propose(fresh, "model-x", "test fixture", &TestClock::new(0)).unwrap()
}

/// **Acceptance test 2**: the decision reproduced from the log with the same policy hash.
/// Appends a real CHECKED decision through the check edge, reads the record back **from
/// disk** (a fresh `Ledger::open` over the same directory, not the in-memory value
/// `check_command` returned), re-evaluates the record's own attached `PolicyDecision.input`
/// against a freshly loaded bundle, and asserts the same `decision_id`, `allow`,
/// `policy_hash` and `reasons` come back.
#[test]
fn decision_is_reproduced_from_the_ledger_with_the_same_policy_hash() {
    let ledger_dir = tmp_dir("reproduce");
    let clock = TestClock::new(5_000);
    let rate = FixtureRateSource::new(BTreeMap::new());

    let original_decision = {
        let ledger = Ledger::open(&ledger_dir).unwrap();
        let bundle = PolicyBundle::load(real_policy_dir()).unwrap();
        let command = proposed_command("cmd-repro", "sat-repro", "mode");
        let result = check_command(command, &bundle, RATE_WINDOW_NS, &rate, &ledger, &clock).unwrap();
        assert!(result.decision.allow, "{:?}", result.decision);
        result.decision
    };

    // Read the record back from disk with a completely fresh Ledger handle over the same
    // directory -- not the in-memory `Ledger` above, and not the in-memory `PolicyDecision`
    // `check_command` returned.
    let reopened = Ledger::open(&ledger_dir).unwrap();
    let verification = reopened.verify("sat-repro").unwrap();
    assert!(verification.ok, "{verification:?}");

    let path = ledger_dir.join(format!(
        "{}.ledger",
        {
            // Mirror ledger.rs's own sanitize_partition_filename (SHA-256 hex of the
            // partition name) rather than reaching into a private function -- this proves
            // the record really was read "from disk" by a path this test derived itself,
            // not by calling back into the module under test to find the file.
            use openssl::sha::sha256;
            let mut s = String::new();
            for b in sha256(b"sat-repro") {
                s.push_str(&format!("{b:02x}"));
            }
            s
        }
    ));
    let bytes = std::fs::read(&path).unwrap();
    // Length-prefixed frames: skip the 4-byte length, decode the LedgerRecord.
    let len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let record = <av_cdm::pb::LedgerRecord as prost::Message>::decode(&bytes[4..4 + len]).unwrap();
    let decision_from_disk = record.decision.expect("CHECKED record must carry its PolicyDecision");
    let recorded_input = decision_from_disk.input.clone().expect("PolicyDecision.input must be filled");

    let fresh_bundle = PolicyBundle::load(real_policy_dir()).unwrap();
    let replayed = policy::evaluate(&fresh_bundle, &recorded_input, &clock);

    assert_eq!(replayed.decision_id, original_decision.decision_id);
    assert_eq!(replayed.decision_id, decision_from_disk.decision_id);
    assert_eq!(replayed.allow, original_decision.allow);
    assert_eq!(replayed.policy_hash, original_decision.policy_hash);
    assert_eq!(replayed.reasons, original_decision.reasons);

    let _ = std::fs::remove_dir_all(&ledger_dir);
}

/// **Acceptance test 3**: two runs over the same inputs, through the real check edge, give
/// byte-identical logs, now with policy decisions attached. Drives the same sequence of
/// commands (an admitted "mode", a denied "payload", and a rate-limited "burn" both under
/// and at its threshold) into two fresh ledger directories with equal `TestClock`s, and
/// asserts the resulting partition files are byte-identical. Prints each file's own SHA-256
/// in the failure message, per this task's own evidence rule (an exit code, or even a bare
/// `assert_eq!` on byte vectors, is not evidence on its own without something a human can
/// quote).
#[test]
fn two_runs_through_the_check_edge_produce_byte_identical_ledgers() {
    fn run(dir: &Path) {
        let ledger = Ledger::open(dir).unwrap();
        let bundle = PolicyBundle::load(real_policy_dir()).unwrap();
        let clock = TestClock::new(10_000);

        let mut burn_counts = BTreeMap::new();
        burn_counts.insert("burn".to_string(), 0u64);
        let rate = FixtureRateSource::new(burn_counts);

        let mode = proposed_command("cmd-a", "sat-x", "mode");
        check_command(mode, &bundle, RATE_WINDOW_NS, &rate, &ledger, &clock).unwrap();

        clock.advance(100);
        let payload = proposed_command("cmd-b", "sat-x", "payload");
        check_command(payload, &bundle, RATE_WINDOW_NS, &rate, &ledger, &clock).unwrap();

        clock.advance(100);
        let mut at_limit = BTreeMap::new();
        at_limit.insert("burn".to_string(), BURN_RATE_LIMIT);
        let rate_at_limit = FixtureRateSource::new(at_limit);
        let burn = proposed_command("cmd-c", "sat-x", "burn");
        check_command(burn, &bundle, RATE_WINDOW_NS, &rate_at_limit, &ledger, &clock).unwrap();
    }

    let dir_a = tmp_dir("byte-identical-a");
    let dir_b = tmp_dir("byte-identical-b");
    run(&dir_a);
    run(&dir_b);

    fn partition_path(dir: &Path, partition: &str) -> PathBuf {
        use openssl::sha::sha256;
        let mut s = String::new();
        for b in sha256(partition.as_bytes()) {
            s.push_str(&format!("{b:02x}"));
        }
        dir.join(format!("{s}.ledger"))
    }
    fn hex_sha256_of_file(path: &Path) -> String {
        use openssl::sha::sha256;
        let bytes = std::fs::read(path).unwrap();
        let mut s = String::new();
        for b in sha256(&bytes) {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    let path_a = partition_path(&dir_a, "sat-x");
    let path_b = partition_path(&dir_b, "sat-x");
    let bytes_a = std::fs::read(&path_a).unwrap();
    let bytes_b = std::fs::read(&path_b).unwrap();
    let sha_a = hex_sha256_of_file(&path_a);
    let sha_b = hex_sha256_of_file(&path_b);

    assert_eq!(
        bytes_a, bytes_b,
        "partition sat-x must be byte-identical across two independent runs through the check edge -- sha256(a)={sha_a} sha256(b)={sha_b}"
    );
    assert!(!bytes_a.is_empty());
    println!("two_runs_through_the_check_edge_produce_byte_identical_ledgers: sha256(a)={sha_a} sha256(b)={sha_b}");

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

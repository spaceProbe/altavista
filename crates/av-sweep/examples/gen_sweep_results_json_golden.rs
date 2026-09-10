//! Generates `goldens/sweep_results_json/{sweep_results.pb,sweep_results.json}` -- the
//! independent-oracle fixture `tests/test_sweep_results_json.py` uses (this task's own brief,
//! §4): a small, entirely SYNTHETIC (no GMAT, no real study run) `altavista.v1.SweepResults`,
//! written once as real protobuf bytes (`prost::Message::encode_to_vec`, ground truth, never
//! hand-edited) and once through this crate's own hand-written JSON encoder
//! (`crates/av-sweep/src/json.rs`'s `sweep_results_to_json` -- moved here from
//! `src/bin/av-sweep/json.rs` as part of F2, see that module's own doc comment -- reused here
//! rather than reimplemented a third time). The pytest then parses the `.json` with Python's REAL
//! `google.protobuf.json_format.Parse` and the `.pb` with `ParseFromString`, and asserts they
//! decode to the identical message -- that is the oracle: Python's protobuf library, not this
//! crate's own code, is what actually checks the JSON encoder's correctness.
//!
//! Deliberately exercises more of the message tree than any single real study output typically
//! would in one go: both an errored and a successful `SweepSample`, a `ScoreResult` with `passed`
//! both `Some` and unset, a `ScoreAggregate` with `pass_fraction` set, and (question 192(b)) a
//! `seeds` map with TWO keys on the successful sample (so the encoder's map-ordering behaviour is
//! actually exercised here, not just asserted in `src/json.rs`'s own unit tests) alongside an
//! EMPTY `seeds` map on the failed one (a failed sample never got far enough to derive any) --
//! so the oracle test's coverage is not accidentally narrower than the encoder's own declared
//! behaviour. (Originally written under F1b, before `aggregate.rs` existed to compute a real
//! `ScoreAggregate` -- this synthetic one was hand-authored then and still is now; nothing about
//! the golden needed to change for F2, since it already covered aggregates.)
//!
//! Mirrors `goldens/gen_*.py`'s own `--reason` convention (standing rule 10: "Goldens are
//! regenerated only through a generator that takes an explicit `--reason`"), refusing to run at
//! all without one. `SweepResults.provenance.created_tai_ns` stays `0` -- this crate's own
//! platform-wide convention (`crates/av-kernel/src/drm/executor.rs`'s module doc comment: "this
//! crate never reads the wall clock") -- so the `--reason` (and a human-readable timestamp,
//! useful only for a person skimming the sibling file, never hashed or compared) is instead
//! recorded in a sibling `REGENERATED.txt`, not smuggled into the hashed/compared message itself.
//!
//! ```text
//! cargo run -p av-sweep --example gen_sweep_results_json_golden -- --reason "..."
//! ```

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use av_cdm::pb;
use av_sweep::json;
use prost::Message;

fn synthetic_sweep_results() -> pb::SweepResults {
    let mut scores_p0d0 = BTreeMap::new();
    scores_p0d0.insert(
        "demo_flt_rmag_at_end".to_string(),
        pb::ScoreResult { name: "demo_flt_rmag_at_end".to_string(), value: 6_870_518.06, unit: pb::Unit::Meter as i32, passed: None },
    );
    scores_p0d0.insert(
        "demo_flt_cd_at_end".to_string(),
        pb::ScoreResult { name: "demo_flt_cd_at_end".to_string(), value: 220.0, unit: pb::Unit::Dimensionless as i32, passed: Some(true) },
    );

    let sample_ok = pb::SweepSample {
        point_index: 0,
        draw_index: 0,
        axis_values: BTreeMap::from([("demo_flt.spacecraft.DragArea".to_string(), 5.0)]),
        // Question 192(b): TWO keys, deliberately, so the golden actually exercises
        // SweepSample.seeds' own map ordering (this crate's own task brief) -- not just the
        // single value the removed `seed` field could ever carry.
        seeds: BTreeMap::from([("burn_seed".to_string(), 15_505_883_566_766_354_181), ("fault_seed".to_string(), 42)]),
        run_id: "demo_two_instance_sweep_p0_d0".to_string(),
        config_hash: "b00f732c107f97e801aaa23034e5d7c1a6c9a993eebe82d20dd404e1f42ba23d".to_string(),
        scores: scores_p0d0,
        products_uri: "/tmp/av-sweep-golden/sample_p0_d0".to_string(),
        error: String::new(),
    };

    let sample_failed = pb::SweepSample {
        point_index: 1,
        draw_index: 0,
        axis_values: BTreeMap::from([("demo_flt.spacecraft.DragArea".to_string(), 25.0)]),
        seeds: BTreeMap::new(), // a failed sample never got far enough to derive seeds
        run_id: "demo_two_instance_sweep_p1_d0".to_string(),
        config_hash: String::new(),
        scores: BTreeMap::new(),
        products_uri: String::new(),
        error: "axis names demo_flt.spacecraft.DragArea = 999.0, which is outside its declared bound [0, 30]".to_string(),
    };

    let aggregate = pb::ScoreAggregate {
        name: "demo_flt_rmag_at_end".to_string(),
        point_index: 0,
        draws: 2,
        mean: 6_870_517.767,
        std_dev: 0.416,
        min: 6_870_517.473,
        max: 6_870_518.061,
        pass_fraction: Some(1.0),
    };

    pb::SweepResults {
        sweep_id: "demo_two_instance_sweep".to_string(),
        sweep_hash: "627c202b01644b1fcbabf5702194a18663092d56ee3dacafe181c9ea040abcfd".to_string(),
        drm_hash: "499d426e18272fbb361ede2c6edd906fd9f828ac99da154722d56d4e2914433d".to_string(),
        samples: vec![sample_ok, sample_failed],
        aggregates: vec![aggregate],
        provenance: Some(pb::Provenance {
            author_kind: pb::AuthorKind::Agent as i32,
            tool: "av-sweep".to_string(),
            run_id: "demo_two_instance_sweep".to_string(),
            config_hash: "627c202b01644b1fcbabf5702194a18663092d56ee3dacafe181c9ea040abcfd".to_string(),
            created_tai_ns: 0,
            ..Default::default()
        }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let mut reason = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" => {
                i += 1;
                reason = args.get(i).cloned();
            }
            other => {
                eprintln!("unrecognized argument {other:?}\n\nusage: gen_sweep_results_json_golden --reason \"...\"");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }
    let Some(reason) = reason else {
        eprintln!("--reason is required (standing rule: goldens are regenerated only through a generator that takes an explicit --reason)\n\nusage: gen_sweep_results_json_golden --reason \"...\"");
        return ExitCode::FAILURE;
    };

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = repo_root.join("goldens").join("sweep_results_json");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("creating {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }

    let results = synthetic_sweep_results();

    let pb_path = out_dir.join("sweep_results.pb");
    if let Err(e) = std::fs::write(&pb_path, results.encode_to_vec()) {
        eprintln!("writing {}: {e}", pb_path.display());
        return ExitCode::FAILURE;
    }

    let json_path = out_dir.join("sweep_results.json");
    if let Err(e) = std::fs::write(&json_path, json::sweep_results_to_json(&results)) {
        eprintln!("writing {}: {e}", json_path.display());
        return ExitCode::FAILURE;
    }

    let regenerated_path = out_dir.join("REGENERATED.txt");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let note = format!(
        "goldens/sweep_results_json/{{sweep_results.pb,sweep_results.json}}\n\
         Generated by: cargo run -p av-sweep --example gen_sweep_results_json_golden -- --reason \"{reason}\"\n\
         Reason: {reason}\n\
         Generated at (unix seconds, informational only -- never hashed or compared): {now}\n\
         Source: crates/av-sweep/examples/gen_sweep_results_json_golden.rs (synthetic message, no GMAT, no real study run).\n"
    );
    if let Err(e) = std::fs::write(&regenerated_path, note) {
        eprintln!("writing {}: {e}", regenerated_path.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {}", pb_path.display());
    println!("wrote {}", json_path.display());
    println!("wrote {}", regenerated_path.display());
    ExitCode::SUCCESS
}

//! One REAL sweep (GMAT, `drms/demo_two_instance_sweep.*`, the same fixture as
//! `tests/fixture_study.rs`) written through BOTH study stores in a single `av-sweep` study run
//! (`--store-dir` and `--store-clickhouse` together), read back from each, and compared:
//!
//! - the canonical JSON (`json::sweep_results_to_json`) byte for byte, with the SHA-256 of each,
//! - the canonical protobuf encoding, and every sample / aggregate,
//! - `samples.jsonl` regenerated from the ClickHouse copy against the file store's own,
//! - the aggregates `av-sweep` computed in Rust against ClickHouse's SQL answer
//!   (`ClickHouseStudyStore::aggregates_in_sql`), bit for bit,
//! - the read-out CLI mode (`--read-study`) from both stores.
//!
//! Gated on the recorded ClickHouse image (visible `SKIPPED` otherwise); needs GMAT like
//! `tests/fixture_study.rs` does. The fixture has no failed sample (all eight run), so failed
//! samples are covered by the synthetic studies in `tests/clickhouse_store.rs`.

#[macro_use]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use av_cdm::pb;
use av_lockstep::docker::{lock_docker_tests, prune_stale_test_resources};
use av_sweep::clickhouse::ClickHouseStudyStore;
use av_sweep::json;
use av_sweep::store::{FileStudyStore, StudyStore};
use av_sweep::{aggregate, sha256_hex};
use prost::Message;

use common::{ChServer, StartOpts};

fn drms_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn av_sweep_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_av-sweep"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("av-sweep-ch-real-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn f_same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn store_flags(cmd: &mut Command, server: &ChServer) {
    cmd.arg("--store-clickhouse").arg(server.http_url()).arg("--clickhouse-database").arg(&server.database).arg("--clickhouse-user").arg(&server.user).arg("--clickhouse-password-env").arg(&server.password_var);
}

fn read_out(server: &ChServer, from_clickhouse: bool, store_dir: &Path, sweep_id: &str, out: &Path) -> Vec<u8> {
    let mut cmd = Command::new(av_sweep_bin());
    cmd.arg("--read-study").arg(sweep_id).arg("--out-pb").arg(out.join("study.pb")).arg("--out-json").arg(out.join("study.json"));
    if from_clickhouse {
        store_flags(&mut cmd, server);
    } else {
        cmd.arg("--store-dir").arg(store_dir);
    }
    let output = cmd.output().expect("spawning av-sweep --read-study");
    assert!(output.status.success(), "--read-study failed: {}", String::from_utf8_lossy(&output.stderr));
    let json_text = std::fs::read(out.join("study.json")).unwrap();
    assert_eq!(json_text, json::sweep_results_to_json(&pb::SweepResults::decode(std::fs::read(out.join("study.pb")).unwrap().as_slice()).unwrap()).into_bytes(), "the JSON written beside the pb is the canonical JSON of that pb");
    std::fs::read(out.join("study.pb")).unwrap()
}

#[test]
fn one_real_sweep_written_through_both_stores_reads_back_identically() {
    const NAME: &str = "one_real_sweep_written_through_both_stores_reads_back_identically";
    gate_or_skip!(NAME);
    let _lock = lock_docker_tests();
    prune_stale_test_resources(&_lock);
    let server = ChServer::start(NAME, StartOpts::default());

    let out_dir = scratch("out");
    let store_dir = scratch("store");
    let mut cmd = Command::new(av_sweep_bin());
    cmd.arg("--sweep").arg(drms_path("demo_two_instance_sweep.sweep.yaml"));
    cmd.arg("--drm").arg(drms_path("demo_two_instance_sweep.drm.yaml"));
    cmd.arg("--sos").arg(drms_path("demo_two_instance.sos.yaml"));
    cmd.arg("--system").arg(drms_path("demo_two_instance.system.yaml"));
    cmd.arg("--system").arg(drms_path("demo_two_instance_ctrl.system.yaml"));
    cmd.arg("--out-dir").arg(&out_dir).arg("--workers").arg("2").arg("--store-dir").arg(&store_dir);
    store_flags(&mut cmd, &server);
    let started = std::time::Instant::now();
    let output = cmd.output().expect("spawning av-sweep study mode");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "study mode must exit 0: {stderr}");
    println!("  study ran in {:?}; stderr:\n{stderr}", started.elapsed());
    assert!(!stderr.contains(&server.password), "the password must never be printed");
    assert!(stderr.contains("study written to") && stderr.contains("study store written to"), "both stores report their location: {stderr}");

    let id = "demo_two_instance_sweep";
    let from_file = FileStudyStore::new(&store_dir).get_study(id).unwrap().expect("file store holds the study");
    let ch = ClickHouseStudyStore::connect(&server.config()).unwrap();
    let from_ch = ch.get_study(id).unwrap().expect("ClickHouse holds the study");
    let from_out_dir = pb::SweepResults::decode(std::fs::read(out_dir.join("sweep_results.pb")).unwrap().as_slice()).unwrap();

    assert_eq!(from_file.samples.len(), 8);
    assert_eq!(from_file.aggregates.len(), 12);
    assert!(from_file.samples.iter().all(|s| s.error.is_empty()), "the fixture runs every sample");

    // Canonical JSON, byte for byte, and its hash from each store.
    let json_file = json::sweep_results_to_json(&from_file);
    let json_ch = json::sweep_results_to_json(&from_ch);
    assert_eq!(json_file, json_ch, "canonical JSON must be byte-identical");
    assert_eq!(json_file.as_bytes(), std::fs::read(out_dir.join("sweep_results.json")).unwrap().as_slice());
    let (hash_file, hash_ch) = (sha256_hex(json_file.as_bytes()), sha256_hex(json_ch.as_bytes()));
    assert_eq!(hash_file, hash_ch);
    println!("  canonical JSON sha256 (file store):       {hash_file}");
    println!("  canonical JSON sha256 (ClickHouse store): {hash_ch}");
    println!("  canonical protobuf sha256 (file / ClickHouse / --out-dir): {} / {} / {}", sha256_hex(&from_file.encode_to_vec()), sha256_hex(&from_ch.encode_to_vec()), sha256_hex(&from_out_dir.encode_to_vec()));

    // Every sample and aggregate equal, and the encodings.
    assert_eq!(from_file, from_ch, "every sample and aggregate equal");
    assert_eq!(from_file, from_out_dir);
    assert_eq!(from_file.encode_to_vec(), from_ch.encode_to_vec());
    for (a, b) in from_file.samples.iter().zip(&from_ch.samples) {
        for (k, s) in &a.scores {
            assert_eq!(s.value.to_bits(), b.scores[k].value.to_bits(), "score {k} bits");
        }
    }
    println!("  8 samples, 12 aggregates: equal field by field and bit by bit");

    // samples.jsonl regenerated from the ClickHouse copy equals the file store's own.
    let regen_dir = scratch("regen");
    FileStudyStore::new(&regen_dir).put_study(&from_ch).unwrap();
    let original = std::fs::read(store_dir.join(id).join("samples.jsonl")).unwrap();
    let regenerated = std::fs::read(regen_dir.join(id).join("samples.jsonl")).unwrap();
    assert_eq!(original, regenerated, "samples.jsonl regenerated from the ClickHouse copy");
    assert_eq!(original.iter().filter(|b| **b == b'\n').count(), 8);
    for f in ["sweep_results.pb", "sweep_results.json"] {
        assert_eq!(std::fs::read(store_dir.join(id).join(f)).unwrap(), std::fs::read(regen_dir.join(id).join(f)).unwrap(), "{f} regenerated");
    }
    println!("  samples.jsonl ({} bytes), sweep_results.pb and .json regenerated from ClickHouse: byte-identical", original.len());

    // The MoE query answered by ClickHouse against the aggregates av-sweep computed in Rust.
    let sql = ch.aggregates_in_sql(id).unwrap().expect("study present");
    let rust = aggregate(&from_file.samples).unwrap();
    assert_eq!(sql.len(), 12);
    assert_eq!(rust, from_file.aggregates, "aggregate() reproduces what the study stored");
    let mut compared = 0;
    for (s, r) in sql.iter().zip(&rust) {
        assert_eq!((&s.name, s.point_index, s.draws), (&r.name, r.point_index, r.draws));
        for (field, a, b) in [("mean", s.mean, r.mean), ("std_dev", s.std_dev, r.std_dev), ("min", s.min, r.min), ("max", s.max, r.max)] {
            assert!(f_same(a, b), "{} point {} {field}: SQL {a:?} vs Rust {b:?}", s.name, s.point_index);
            compared += 1;
        }
        assert_eq!(s.pass_fraction, r.pass_fraction);
    }
    println!("  MoE: ClickHouse's SQL aggregates equal aggregate(): 12 rows, {compared} f64 fields, all bit-identical");

    // The read-out CLI mode from each store.
    let read_ch_dir = scratch("read-ch");
    let read_file_dir = scratch("read-file");
    let pb_ch = read_out(&server, true, &store_dir, id, &read_ch_dir);
    let pb_file = read_out(&server, false, &store_dir, id, &read_file_dir);
    assert_eq!(pb_ch, pb_file, "--read-study yields the same SweepResults protobuf from either store");
    assert_eq!(pb_ch, from_file.encode_to_vec());
    println!("  --read-study: {} bytes of SweepResults protobuf, identical from both stores", pb_ch.len());

    // A missing study is a non-zero exit with a clear message.
    let output = Command::new(av_sweep_bin()).arg("--read-study").arg("no_such_study").arg("--out-pb").arg(read_ch_dir.join("none.pb")).arg("--store-dir").arg(&store_dir).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("holds no study"), "{}", String::from_utf8_lossy(&output.stderr));

    for d in [out_dir, store_dir, regen_dir, read_ch_dir, read_file_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

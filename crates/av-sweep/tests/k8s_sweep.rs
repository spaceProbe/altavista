//! Question 245, task heavy-ct-2b: a feasibility sweep fanned out as k3s Jobs, byte-identical to a
//! local run -- the real-cluster proofs for `av-sweep --executor k8s|container`
//! (`crates/av-sweep/src/bin/av-sweep/jobs.rs`), over the GMAT-free fixture
//! `drms/leo_orbital_native_sweep.*` (4 points x 3 draws = 12 samples) and the sample image
//! `services/sweep/` (`av-sweep` built `--no-default-features`).
//!
//! # What runs always, and what is gated
//!
//! * Always (no Docker, no cluster): the fixture's hashes verify and the study runs end to end with
//!   child processes under *either* feature set of this crate -- the fixture names no GMAT model
//!   -- with non-trivial scores and aggregates; the `--no-default-features` binary refuses a
//!   `gmat.` model and `--gmat-startup`, typed (`#[cfg(not(feature = "gmat"))]`); a study over an
//!   image that is not present is refused before any sample is touched.
//! * Gated on the recorded sample image (`services/sweep/IMAGE_DIGEST.md`, compared with the image
//!   Docker really has: a mismatch is a visible `SKIPPED`, never a silent trust), the recorded
//!   `alpine` network-gate image, and the `colima` kubeconfig context answering: the three-way
//!   byte-for-byte comparison, the several-pods-at-once evidence, and the docker-killed sample pod.
//!   A gate failure is a visible `SKIPPED` line (a hard failure under `AV_REQUIRE_DOCKER_TESTS=1`).
//!
//! # The lock is held for the pods' whole life (cross-track hazard; `av_jobs::k8s` module doc)
//!
//! A pod's `k8s_POD_*` sandbox carries the pod's `av.test=1` label, and the native tests'
//! `prune_stale_test_resources` removes every container with it under `lock_docker_tests`. So each
//! gated test takes `lock_docker_tests` FIRST and releases it LAST: `Env` owns the lock as its last
//! field, after the namespace guard, which deletes the namespace and waits until the API and
//! Docker show no pod or container of the run. This file takes the lock itself and is never
//! wrapped by `with_docker_lock.py`. Every pod and container carries `av.test=1` and a run id
//! (`--job-label`). Nothing is pruned that this test did not create.
//!
//! # Proof output
//!
//! With `AV_SWEEP_PROOF_DIR=<dir>` set, the tests keep their study directories under it (each run's
//! result moved aside as `a-k8s`, `b-container`, `c-native`, ... so the three runs use the SAME
//! `--out-dir` path and `products_uri` is identical) and write `<test>.proof.txt` with every
//! SHA-256 and count they assert. Without it they work under `.av-test-tmp/av-sweep-k8s/<run id>/`
//! (under `$HOME`, which Colima mounts into the node) and delete it.
//! `AV_SWEEP_NATIVE_BIN=<path>` names the `av-sweep` binary the native (process) run uses, instead
//! of the one cargo built for this test (the proofs use a release build, like the image's).
//!
//! # What "byte-identical" claims, and what it does not
//!
//! The k8s study equals the container study byte for byte (`sweep_results.pb`, `sweep_results.json`
//! and every sample's `run_products.pb`): the same image, the same glibc. It does **not** equal a
//! study run by this host's macOS-native binary, and the test records that instead of hiding it:
//! the native study is itself reproducible (run twice, identical), the differences are confined to
//! trajectory means and score values (millimetres of position at the end of a day), and the cause is
//! the platform's libm -- Apple's `cos`/`sin` and glibc's give results one ulp apart for the same
//! f64 argument on a few percent of inputs, and `av_orbital::fk5::nutation` is built from 106
//! `sin_cos` calls plus three more; a one-ulp difference there moves the nutation matrix, hence the
//! body-fixed rotation, hence the gravity acceleration, and the integration carries it forward. The
//! heavy-ct-2b report has the probe that shows it quantity by quantity. `AV_SWEEP_REQUIRE_NATIVE_EQUAL=1`
//! turns "recorded" into "asserted" for a platform where the two do agree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use av_cdm::pb;
use av_jobs::k8s::manifest::label_value;
use av_jobs::k8s::{K8sApi, Kubeconfig};
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, recorded_digest_gate, test_run_id, DockerGateReason, DockerTestLock};
use prost::Message;

const SAMPLE_IMAGE_NAME: &str = "av-sweep-sample";
const STUDY_TIMEOUT: Duration = Duration::from_secs(900);
const FIXTURE_SAMPLES: usize = 12;

// ------------------------------------------------------------------------------------------
// The study command line, and running it.
// ------------------------------------------------------------------------------------------

fn drms(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../drms").join(name)
}

fn av_sweep_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_av-sweep"))
}

fn native_bin() -> PathBuf {
    std::env::var_os("AV_SWEEP_NATIVE_BIN").map(PathBuf::from).unwrap_or_else(av_sweep_bin)
}

/// The fixture's study arguments (everything but the executor choice).
fn fixture_args(out_dir: &Path, workers: u32) -> Vec<String> {
    let s = |p: PathBuf| p.display().to_string();
    vec![
        "--sweep".into(),
        s(drms("leo_orbital_native_sweep.sweep.yaml")),
        "--drm".into(),
        s(drms("leo_orbital_native_sweep.drm.yaml")),
        "--sos".into(),
        s(drms("leo_orbital_native_sweep.sos.yaml")),
        "--system".into(),
        s(drms("leo_1day_orbital_native.system.yaml")),
        "--out-dir".into(),
        out_dir.display().to_string(),
        "--workers".into(),
        workers.to_string(),
    ]
}

struct Run {
    success: bool,
    status: String,
    stderr: String,
}

/// Runs `bin args` to completion (or kills it at `STUDY_TIMEOUT`), echoing its stderr as it comes
/// (visible under `--nocapture`) and returning all of it.
fn run_av_sweep(bin: &Path, args: &[String]) -> Run {
    let mut child = Command::new(bin).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap_or_else(|e| panic!("spawning {}: {e}", bin.display()));
    let stderr = child.stderr.take().expect("piped stderr");
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = collected.clone();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[av-sweep] {line}");
            let mut s = sink.lock().unwrap();
            s.push_str(&line);
            s.push('\n');
        }
    });
    let begun = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status.to_string();
        }
        if begun.elapsed() > STUDY_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break format!("killed by the test after {STUDY_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(50));
    };
    reader.join().unwrap();
    let stderr = collected.lock().unwrap().clone();
    Run { success: status == "exit status: 0", status, stderr }
}

// ------------------------------------------------------------------------------------------
// A finished study, moved aside and read.
// ------------------------------------------------------------------------------------------

struct Study {
    label: &'static str,
    dir: PathBuf,
    results: pb::SweepResults,
    /// `file name -> sha256` for `sweep_results.pb`, `sweep_results.json`, and for every sample
    /// `sample_pX_dY/run_products.pb`.
    hashes: BTreeMap<String, String>,
    stderr_txt: BTreeMap<String, String>,
    stderr: String,
}

impl Study {
    fn failed(&self) -> Vec<&pb::SweepSample> {
        self.results.samples.iter().filter(|s| !s.error.is_empty()).collect()
    }
}

fn sha256_file(path: &Path) -> String {
    av_sweep::sha256_hex(&fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())))
}

/// Moves `out_dir` to `aside` (so the next run can use the same `--out-dir` path) and reads it.
fn move_aside(label: &'static str, out_dir: &Path, aside: &Path, stderr: String) -> Study {
    let _ = fs::remove_dir_all(aside);
    fs::rename(out_dir, aside).unwrap_or_else(|e| panic!("moving {} to {}: {e}", out_dir.display(), aside.display()));
    let pb_bytes = fs::read(aside.join("sweep_results.pb")).unwrap_or_else(|e| panic!("{label}: reading sweep_results.pb: {e}"));
    let results = pb::SweepResults::decode(pb_bytes.as_slice()).unwrap_or_else(|e| panic!("{label}: decoding sweep_results.pb: {e}"));
    let mut hashes = BTreeMap::new();
    hashes.insert("sweep_results.pb".to_string(), sha256_file(&aside.join("sweep_results.pb")));
    hashes.insert("sweep_results.json".to_string(), sha256_file(&aside.join("sweep_results.json")));
    let mut stderr_txt = BTreeMap::new();
    for entry in fs::read_dir(aside).unwrap().filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("sample_p") {
            let products = entry.path().join("run_products.pb");
            if products.is_file() {
                hashes.insert(format!("{name}/run_products.pb"), sha256_file(&products));
            }
            stderr_txt.insert(name, fs::read_to_string(entry.path().join("stderr.txt")).unwrap_or_default());
        }
    }
    Study { label, dir: aside.to_path_buf(), results, hashes, stderr_txt, stderr }
}

/// The names of every hash that differs between two studies (or exists in only one).
fn differences(a: &Study, b: &Study) -> Vec<String> {
    let names: BTreeSet<&String> = a.hashes.keys().chain(b.hashes.keys()).collect();
    names.into_iter().filter(|n| a.hashes.get(*n) != b.hashes.get(*n)).cloned().collect()
}

/// `proof.txt` lines for one comparison; returns whether the two are byte-identical.
fn compare(report: &mut String, a: &Study, b: &Study) -> bool {
    let diff = differences(a, b);
    let _ = writeln!(report, "compare {} vs {}: {} hashes each, {} differ", a.label, b.label, a.hashes.len(), diff.len());
    for name in &diff {
        let _ = writeln!(report, "  DIFFERS {name}: {} = {:?}, {} = {:?}", a.label, a.hashes.get(name), b.label, b.hashes.get(name));
    }
    diff.is_empty()
}

fn describe(report: &mut String, s: &Study) {
    let _ = writeln!(report, "study {} ({}): {} sample(s), {} failed, {} aggregate(s)", s.label, s.dir.display(), s.results.samples.len(), s.failed().len(), s.results.aggregates.len());
    for (name, hash) in &s.hashes {
        let _ = writeln!(report, "  sha256 {hash}  {}/{name}", s.label);
    }
}

fn write_proof(test: &str, report: &str) {
    println!("\n--- {test} proof ---\n{report}---");
    if let Ok(dir) = std::env::var("AV_SWEEP_PROOF_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(Path::new(&dir).join(format!("{test}.proof.txt")), report).unwrap();
    }
}

// ------------------------------------------------------------------------------------------
// Always-on tests: the fixture, GMAT-free, with child processes.
// ------------------------------------------------------------------------------------------

/// The directory a test keeps its study directories in: `<base>/<test name>/` for the results it
/// moves aside, and ONE `--out-dir` path, `<base>/study`, that every study of every test in this
/// file uses (the tests are serialised by the docker lock), so `products_uri` -- the canonical
/// sample directory, recorded in `sweep_results.pb` -- is identical across all runs and the bytes
/// of `sweep_results.pb` can be compared between runs of different tests.
struct Workdir {
    base: PathBuf,
    path: PathBuf,
    keep: bool,
}

impl Workdir {
    fn new(name: &str) -> Workdir {
        let (base, keep) = match std::env::var("AV_SWEEP_PROOF_DIR") {
            Ok(dir) => (PathBuf::from(dir), true),
            Err(_) => (PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.av-test-tmp/av-sweep-k8s").join(format!("{name}-{}", test_run_id())), false),
        };
        fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let path = base.join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Workdir { base, path, keep }
    }

    /// The shared `--out-dir`, emptied.
    fn study_dir(&self) -> PathBuf {
        let dir = self.base.join("study");
        let _ = fs::remove_dir_all(&dir);
        dir
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
}

#[test]
fn the_fixture_verifies_and_runs_with_no_gmat_and_has_non_trivial_scores() {
    // Hashes the way study mode checks them.
    let sweep = av_sweep::parse_sweep_yaml(&fs::read_to_string(drms("leo_orbital_native_sweep.sweep.yaml")).unwrap()).unwrap();
    av_sweep::verify_sweep_hash(&sweep).expect("the sweep's committed hash verifies");
    let drm = av_kernel::drm::schema::parse_drm_yaml(&fs::read_to_string(drms("leo_orbital_native_sweep.drm.yaml")).unwrap()).unwrap();
    av_kernel::drm::hash::verify_drm_hash(&drm).expect("the DRM's committed hash verifies");
    assert_eq!(sweep.drm_id, drm.id);
    assert_eq!(av_sweep::expand_grid(&sweep).unwrap().len() as u32 * sweep.monte_carlo_draws, FIXTURE_SAMPLES as u32, "4 points x 3 draws");

    let work = Workdir::new("fixture");
    let out = work.path.join("study");
    let run = run_av_sweep(&native_bin(), &fixture_args(&out, 4));
    assert!(run.success, "the fixture study must complete: {}\n{}", run.status, run.stderr);
    let study = move_aside("process", &out, &work.path.join("process"), run.stderr);
    assert_eq!(study.results.samples.len(), FIXTURE_SAMPLES);
    assert!(study.failed().is_empty(), "{:?}", study.failed().iter().map(|s| &s.error).collect::<Vec<_>>());
    // Declared scores: two measures and one objective, per sample.
    for s in &study.results.samples {
        assert_eq!(s.scores.keys().map(String::as_str).collect::<Vec<_>>(), ["leo_pos_x_at_end", "leo_speed_at_end", "leo_speed_ok"], "p{} d{}", s.point_index, s.draw_index);
        assert!(s.scores["leo_speed_ok"].passed.is_some() && s.scores["leo_speed_at_end"].passed.is_none());
    }
    // Non-trivial: the draws of one grid point differ (Gates dispersion under Sampled mode), the
    // points differ from each other (both axes act), and the objective neither passes nor fails everywhere.
    let speed = |p: u32, d: u32| study.results.samples.iter().find(|s| s.point_index == p && s.draw_index == d).unwrap().scores["leo_speed_at_end"].value;
    assert!(speed(0, 0) != speed(0, 1) && speed(0, 1) != speed(0, 2), "the draws of point 0 must differ");
    let point_mean = |p: u32| (0..3).map(|d| speed(p, d)).sum::<f64>() / 3.0;
    assert!((point_mean(0) - point_mean(1)).abs() > 1.0 && (point_mean(0) - point_mean(2)).abs() > 0.2, "both axes must move the score: {} {} {}", point_mean(0), point_mean(1), point_mean(2));
    let passes = study.results.samples.iter().filter(|s| s.scores["leo_speed_ok"].passed == Some(true)).count();
    assert!(passes > 0 && passes < FIXTURE_SAMPLES, "the objective must pass for some samples and fail for others, passed {passes} of {FIXTURE_SAMPLES}");
    assert_eq!(study.results.aggregates.len(), 4 * 3, "4 points x 3 scores");
    let mut report = String::new();
    describe(&mut report, &study);
    let _ = writeln!(report, "objective leo_speed_ok passed for {passes} of {FIXTURE_SAMPLES} samples");
    write_proof("fixture_process_study", &report);
}

/// The `--no-default-features` binary (the sample image's) refuses what it cannot do, typed: a
/// `gmat.` model (the kernel's `GmatFeatureDisabled`, recorded per sample) and `--gmat-startup`.
#[cfg(not(feature = "gmat"))]
#[test]
fn the_no_gmat_binary_refuses_a_gmat_model_and_a_gmat_startup_file_typed() {
    let work = Workdir::new("no-gmat-refusals");
    // A fixture whose instances name gmat. models, through the same binary: every sample fails
    // with the kernel's own typed message, and the study still completes.
    let out = work.path.join("gmat-study");
    let run = run_av_sweep(
        &av_sweep_bin(),
        &[
            "--sweep".into(),
            drms("demo_two_instance_sweep.sweep.yaml").display().to_string(),
            "--drm".into(),
            drms("demo_two_instance_sweep.drm.yaml").display().to_string(),
            "--sos".into(),
            drms("demo_two_instance.sos.yaml").display().to_string(),
            "--system".into(),
            drms("demo_two_instance.system.yaml").display().to_string(),
            "--system".into(),
            drms("demo_two_instance_ctrl.system.yaml").display().to_string(),
            "--out-dir".into(),
            out.display().to_string(),
            "--workers".into(),
            "4".into(),
        ],
    );
    assert!(run.success, "{}\n{}", run.status, run.stderr);
    let study = move_aside("gmat-model", &out, &work.path.join("gmat-model"), run.stderr);
    assert_eq!(study.results.samples.len(), 8);
    assert_eq!(study.failed().len(), 8, "every sample of a gmat. model DRM must fail in a build without GMAT");
    for s in &study.results.samples {
        assert!(s.error.contains("DRM execution failed") && s.error.to_lowercase().contains("gmat"), "p{} d{}: {}", s.point_index, s.draw_index, s.error);
    }
    println!("a gmat. model without GMAT -> {}", study.results.samples[0].error);

    // --gmat-startup: refused once, before any sample, in study mode ...
    let out2 = work.path.join("startup-study");
    let mut args = fixture_args(&out2, 1);
    args.extend(["--gmat-startup".into(), "/no/such/startup_file.txt".into()]);
    let run = run_av_sweep(&av_sweep_bin(), &args);
    assert!(!run.success && run.stderr.contains("--gmat-startup was given, but av-sweep was built with --no-default-features"), "{}\n{}", run.status, run.stderr);
    assert!(!out2.exists(), "nothing is written before the refusal");
    // ... and in sample mode.
    let run = run_av_sweep(
        &av_sweep_bin(),
        &["--run-sample".into(), "--drm-pb".into(), "d".into(), "--sos-pb".into(), "s".into(), "--system-pb".into(), "y".into(), "--run-id".into(), "r".into(), "--error-mode".into(), "nominal".into(), "--out".into(), "o".into(), "--gmat-startup".into(), "x".into()],
    );
    assert!(!run.success && run.stderr.contains("--gmat-startup was given, but av-sweep was built with --no-default-features"), "{}\n{}", run.status, run.stderr);
}

/// A study over an image that is not there is refused before any sample is touched, for both job
/// executors; nothing is pulled. No pod or container is created, so this needs no lock.
#[test]
fn a_study_over_an_absent_image_is_refused_before_any_sample() {
    let work = Workdir::new("absent-image");
    let absent = format!("{SAMPLE_IMAGE_NAME}-no-such-image@sha256:{}", "0".repeat(64));
    for (label, extra) in [("container", vec!["--executor", "container"]), ("k8s", vec!["--executor", "k8s", "--k8s-namespace", "no-such-namespace"])] {
        let out = work.path.join(label);
        let mut args = fixture_args(&out, 2);
        args.extend(extra.into_iter().map(String::from));
        args.extend(["--sample-image".into(), absent.clone()]);
        let run = run_av_sweep(&av_sweep_bin(), &args);
        assert!(!run.success, "{label}: {}\n{}", run.status, run.stderr);
        assert!(run.stderr.contains("is not present locally") && run.stderr.contains("never pulls"), "{label}: {}", run.stderr);
        let samples: Vec<_> = fs::read_dir(&out).map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("sample_p")).collect()).unwrap_or_default();
        assert!(samples.is_empty(), "{label}: no sample directory may exist: {samples:?}");
        assert!(!out.join("sweep_results.pb").exists());
    }
}

// ------------------------------------------------------------------------------------------
// Gated tests: the recorded image, the cluster, the lock.
// ------------------------------------------------------------------------------------------

fn fenced_block_after(text: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_idx = lines.iter().position(|l| l.contains(marker))?;
    let fence_start = marker_idx + lines[marker_idx..].iter().position(|l| l.trim() == "```")?;
    let fence_end = fence_start + 1 + lines[fence_start + 1..].iter().position(|l| l.trim() == "```")?;
    let content = lines[fence_start + 1..fence_end].join("\n").trim().to_string();
    (!content.is_empty()).then_some(content)
}

fn repo_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

/// The sample image's recorded id (`services/sweep/IMAGE_DIGEST.md`), with a NOTE (never a gate)
/// when `git` says commits touching the image's copied paths landed after the recorded build
/// commit, or the build tree was dirty: the record is compared with the image Docker really has,
/// which is the trust decision; the commit is provenance for a human.
fn recorded_sample_image_id() -> String {
    let path = repo_path("services/sweep/IMAGE_DIGEST.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let id = fenced_block_after(&text, "docker image inspect").unwrap_or_else(|| panic!("no 'docker image inspect' block in {path:?}"));
    assert!(id.starts_with("sha256:") && id.len() == 71, "the recorded id must be a sha256 digest, got {id:?}");
    if let Some(commit_line) = fenced_block_after(&text, "Built from commit") {
        let commit = commit_line.split_whitespace().next().unwrap_or_default().to_string();
        if commit_line.contains("dirty") {
            eprintln!("NOTE: services/sweep/IMAGE_DIGEST.md records a build from a DIRTY tree ({commit_line}); rebuild from a clean tree after the work is committed for a provenance the commit check can use");
        } else if let Ok(paths) = fs::read_to_string(repo_path("services/sweep/IMAGE_COPIED_PATHS.txt")) {
            let paths: Vec<&str> = paths.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
            let out = Command::new("git").current_dir(repo_path(".")).args(["log", "--oneline", &format!("{commit}..HEAD"), "--"]).args(&paths).output();
            if let Ok(out) = out {
                let n = String::from_utf8_lossy(&out.stdout).lines().count();
                if n > 0 {
                    eprintln!("NOTE: {n} commit(s) touching the sample image's copied paths landed after its recorded build commit {commit}; rebuild with services/sweep/build-image.sh");
                }
            }
        }
    }
    id
}

/// `alpine@sha256:...` (reference, recorded id) of the network-gate image, from the record
/// `crates/av-jobs/tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md` (the one home of that digest).
fn recorded_gate_image() -> (String, String) {
    let path = repo_path("crates/av-jobs/tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let image_ref = fenced_block_after(&text, "Registry reference").expect("'Registry reference' block");
    let id = fenced_block_after(&text, "docker image inspect").expect("'docker image inspect' block");
    (image_ref, id)
}

fn docker_containers_with(label: &str) -> Vec<String> {
    let out = Command::new("docker").args(["ps", "-a", "--filter", &format!("label={label}"), "--format", "{{.Names}}"]).output().expect("docker ps");
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
}

fn run_containers(run_id: &str, namespace: &str) -> Vec<String> {
    let mut all = docker_containers_with(&format!("av.test.run_id={run_id}"));
    all.extend(docker_containers_with(&format!("io.kubernetes.pod.namespace={namespace}")));
    all.sort();
    all.dedup();
    all
}

/// Deletes the test's namespace, waits for it to be gone, then for every Docker container of the
/// run to be gone (force-removing a straggler that carries THIS run's id, never anything else).
struct NamespaceGuard {
    api: K8sApi,
    name: String,
    run_id: String,
}

impl Drop for NamespaceGuard {
    fn drop(&mut self) {
        let _ = self.api.delete_namespace(&self.name);
        let end = Instant::now() + Duration::from_secs(90);
        let mut gone = false;
        while Instant::now() < end {
            if matches!(self.api.get_namespace(&self.name), Err(e) if e.is_not_found()) {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
        if !gone {
            eprintln!("WARNING: namespace {} still present 90 s after deletion", self.name);
        }
        let end = Instant::now() + Duration::from_secs(60);
        while Instant::now() < end && !run_containers(&self.run_id, &self.name).is_empty() {
            thread::sleep(Duration::from_millis(500));
        }
        for name in run_containers(&self.run_id, &self.name) {
            eprintln!("WARNING: container {name} of run {} outlived its namespace; removing it", self.run_id);
            let _ = Command::new("docker").args(["rm", "-f", "-v", &name]).output();
        }
    }
}

struct Env {
    run_id: String,
    namespace: String,
    /// `av-sweep-sample@sha256:<id>`.
    sample_image: String,
    /// `alpine@sha256:<id>`, the network gate's image.
    gate_image: String,
    api: K8sApi,
    work: Workdir,
    /// Dropped first (fields drop in declaration order) ...
    _ns: NamespaceGuard,
    /// ... and the lock last: no pod of this run outlives it.
    _lock: DockerTestLock,
}

impl Env {
    /// The executor-selecting arguments of a study, plus the labels every pod/container carries.
    fn job_args(&self, executor: &str) -> Vec<String> {
        let mut a: Vec<String> = vec!["--executor".into(), executor.into(), "--sample-image".into(), self.sample_image.clone(), "--job-label".into(), "av.test=1".into(), "--job-label".into(), format!("av.test.run_id={}", self.run_id)];
        if executor == "k8s" {
            a.extend(["--k8s-namespace".into(), self.namespace.clone(), "--k8s-context".into(), "colima".into(), "--k8s-network-gate-image".into(), self.gate_image.clone(), "--job-deadline-secs".into(), "120".into()]);
        }
        a
    }

    fn pods(&self) -> Vec<serde_json::Value> {
        self.api.list_pods(&self.namespace, "av.job=1").unwrap_or_default()
    }
}

/// `None` (after a visible SKIPPED line) when Docker, the images or the cluster is not available.
/// Takes the docker lock FIRST (see the module doc).
fn setup(test_name: &str) -> Option<Env> {
    let lock = lock_docker_tests();
    let sample_id = recorded_sample_image_id();
    let sample_ref = format!("{SAMPLE_IMAGE_NAME}@{sample_id}");
    if let Err(reason) = recorded_digest_gate(&sample_ref, &sample_id) {
        announce_gate_skip(test_name, &reason);
        return None;
    }
    let (gate_ref, gate_id) = recorded_gate_image();
    if let Err(reason) = recorded_digest_gate(&gate_ref, &gate_id) {
        announce_gate_skip(test_name, &reason);
        return None;
    }
    let kubeconfig = match Kubeconfig::load_default(Some("colima")) {
        Ok(k) => k,
        Err(e) => {
            announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "the `colima` kubeconfig context".into(), hint: e.to_string() });
            return None;
        }
    };
    let api = match K8sApi::new(&kubeconfig, av_http1::Timeouts::uniform(Duration::from_secs(10))) {
        Ok(a) => a,
        Err(e) => {
            announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "a Kubernetes API client".into(), hint: e.to_string() });
            return None;
        }
    };
    if let Err(e) = api.list_nodes() {
        announce_gate_skip(test_name, &DockerGateReason::PrerequisiteUnavailable { what: "the k3s API server".into(), hint: e.to_string() });
        return None;
    }
    let run_id = test_run_id();
    let namespace = format!("av-sweep-ct-{run_id}");
    let labels = BTreeMap::from([("av.test".to_string(), "1".to_string()), ("av.test.run_id".to_string(), run_id.clone())]);
    assert!(docker_containers_with(&format!("av.test.run_id={run_id}")).is_empty());
    assert!(matches!(api.get_namespace(&namespace), Err(e) if e.is_not_found()));
    let stale: Vec<String> = docker_containers_with("av.test").into_iter().filter(|n| n.starts_with("k8s_")).collect();
    if !stale.is_empty() {
        eprintln!("NOTE: {} k8s container(s) labelled av.test already present before this run: {stale:?}", stale.len());
    }
    api.create_namespace(&namespace, &labels).unwrap_or_else(|e| panic!("creating namespace {namespace}: {e}"));
    let guard = NamespaceGuard { api: api.clone(), name: namespace.clone(), run_id: run_id.clone() };
    let work = Workdir::new(test_name);
    Some(Env { run_id, namespace, sample_image: sample_ref, gate_image: gate_ref, api, work, _ns: guard, _lock: lock })
}

macro_rules! env_or_skip {
    ($name:expr) => {
        match setup($name) {
            Some(env) => env,
            None => return,
        }
    };
}

/// Polls the namespace's sample pods every 50 ms from a separate thread: independent evidence of
/// how many pods existed per job and how many containers were running at once, whatever the
/// study reports.
struct PodWatcher {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<WatchResult>,
}

#[derive(Default, Debug)]
struct WatchResult {
    /// `av.job.id` label -> every pod name ever seen for it.
    pods_by_job: BTreeMap<String, BTreeSet<String>>,
    /// Most pods existing at one poll.
    max_pods: usize,
    /// Most pods whose workload container was running at one poll.
    max_running: usize,
    /// Most pods whose workload container was running AND not yet finished, by name.
    running_at_peak: Vec<String>,
    /// Every node any pod was bound to (`spec.nodeName`): one, because this cluster has one node.
    nodes: BTreeSet<String>,
}

impl PodWatcher {
    fn start(api: K8sApi, namespace: String) -> PodWatcher {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = thread::spawn(move || {
            let mut r = WatchResult::default();
            while !stop2.load(Ordering::Relaxed) {
                if let Ok(pods) = api.list_pods(&namespace, "av.job=1") {
                    let mut running = Vec::new();
                    for p in &pods {
                        let name = p["metadata"]["name"].as_str().unwrap_or_default().to_string();
                        if let Some(job) = p["metadata"]["labels"]["av.job.id"].as_str() {
                            r.pods_by_job.entry(job.to_string()).or_default().insert(name.clone());
                        }
                        if let Some(node) = p["spec"]["nodeName"].as_str() {
                            r.nodes.insert(node.to_string());
                        }
                        if p["status"]["containerStatuses"][0]["state"]["running"].is_object() {
                            running.push(name);
                        }
                    }
                    r.max_pods = r.max_pods.max(pods.len());
                    if running.len() > r.max_running {
                        r.max_running = running.len();
                        r.running_at_peak = running;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
            r
        });
        PodWatcher { stop, handle }
    }

    fn finish(self) -> WatchResult {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap()
    }
}

/// No sample pod of the study is left in the namespace (each Job is deleted, Foreground, by the
/// executor, and its pod with it) and no job scratch directory is left under the study directory.
fn assert_cluster_clean(env: &Env, what: &str) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let pods = env.pods();
        if pods.is_empty() {
            return;
        }
        assert!(Instant::now() < end, "{what}: {} pod(s) still in {}", pods.len(), env.namespace);
        thread::sleep(Duration::from_millis(250));
    }
}

fn epoch(ts: &str) -> i64 {
    // RFC 3339 `YYYY-MM-DDTHH:MM:SSZ` to epoch seconds (Howard Hinnant's days-from-civil).
    let n = |r: std::ops::Range<usize>| ts[r].parse::<i64>().unwrap_or_else(|_| panic!("bad timestamp {ts:?}"));
    assert!(ts.len() >= 20 && ts.ends_with('Z'), "unexpected timestamp shape {ts:?}");
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) * 86400 + n(11..13) * 3600 + n(14..16) * 60 + n(17..19)
}

/// One sample pod as the study printed it from the cluster's own timestamps.
#[derive(Debug, Clone)]
struct PodLine {
    run_id: String,
    pod: String,
    pod_start: i64,
    started: i64,
    finished: i64,
}

fn pod_lines(stderr: &str) -> Vec<PodLine> {
    stderr
        .lines()
        .filter(|l| l.starts_with("av-sweep: sample ") && l.contains(" startTime="))
        .map(|l| {
            let run_id = l.split_whitespace().nth(2).unwrap().to_string();
            let field = |key: &str| l.split(key).nth(1).unwrap_or_else(|| panic!("no {key} in {l}")).split_whitespace().next().unwrap().to_string();
            PodLine { run_id, pod: field(" pod "), pod_start: epoch(&field("startTime=")), started: epoch(&field("startedAt=")), finished: epoch(&field("finishedAt=")) }
        })
        .collect()
}

/// Whether two `run_products.pb` files are the same message once every trajectory sample's `mean`
/// and `cov` and every score's `value` are removed.
fn numeric_only_difference(a: &Path, b: &Path) -> bool {
    let strip = |p: &Path| {
        let mut r = pb::RunProducts::decode(fs::read(p).ok()?.as_slice()).ok()?;
        for t in r.trajectories.values_mut() {
            for s in &mut t.samples {
                s.mean.clear();
                s.cov.clear();
            }
        }
        for s in r.scores.values_mut() {
            s.value = 0.0;
        }
        Some(r.encode_to_vec())
    };
    // A file that does not decode is not "the same message but for its numbers".
    matches!((strip(a), strip(b)), (Some(x), Some(y)) if x == y)
}

/// `p0_d0`'s value of a score, as text (full precision), for the report.
fn score_text(s: &Study, score: &str) -> String {
    s.results.samples.iter().find(|x| x.point_index == 0 && x.draw_index == 0).map(|x| x.scores.get(score).map_or("<no such score>".to_string(), |v| format!("{:?}", v.value))).unwrap_or_default()
}

/// The most intervals that share one instant (closed intervals, one-second resolution).
fn max_overlap(intervals: &[(i64, i64)]) -> usize {
    intervals.iter().map(|&(s, _)| intervals.iter().filter(|&&(a, b)| a <= s && s <= b).count()).max().unwrap_or(0)
}

#[test]
fn k8s_container_and_native_studies_are_byte_identical_and_several_pods_run_at_once() {
    let env = env_or_skip!("k8s_container_and_native_studies_are_byte_identical_and_several_pods_run_at_once");
    // The SAME --out-dir path for all three runs; each result is moved aside before the next, so
    // `products_uri` (the canonical sample directory) is identical across them.
    let out = env.work.study_dir();
    let mut report = String::new();
    let _ = writeln!(report, "sample image {}  network gate {}  namespace {}  run {}", env.sample_image, env.gate_image, env.namespace, env.run_id);
    let _ = writeln!(report, "--out-dir for all three runs: {}", out.display());

    // (a) k8s, 4 workers.
    let mut args = fixture_args(&out, 4);
    args.extend(env.job_args("k8s"));
    let watcher = PodWatcher::start(env.api.clone(), env.namespace.clone());
    let begun = Instant::now();
    let run = run_av_sweep(&av_sweep_bin(), &args);
    let wall_a = begun.elapsed();
    let watched = watcher.finish();
    assert!(run.success, "(a) k8s study failed: {}\n{}", run.status, run.stderr);
    let a = move_aside("a-k8s", &out, &env.work.path.join("a-k8s"), run.stderr);
    assert_eq!(a.results.samples.len(), FIXTURE_SAMPLES);
    assert!(a.failed().is_empty(), "(a) failures: {:?}", a.failed().iter().map(|s| (&s.run_id, &s.error)).collect::<Vec<_>>());
    assert_cluster_clean(&env, "after (a)");

    // Several pods at once -- on ONE node, not several hosts.
    let lines = pod_lines(&a.stderr);
    assert_eq!(lines.len(), FIXTURE_SAMPLES, "one placement line per sample pod");
    let pod_names: BTreeSet<&String> = lines.iter().map(|l| &l.pod).collect();
    assert_eq!(pod_names.len(), FIXTURE_SAMPLES, "twelve distinct pods for twelve samples");
    for (job, pods) in &watched.pods_by_job {
        assert_eq!(pods.len(), 1, "job {job} had more than one pod: {pods:?} -- that is a retry");
    }
    assert_eq!(watched.pods_by_job.len(), FIXTURE_SAMPLES);
    let pod_lifetimes: Vec<(i64, i64)> = lines.iter().map(|l| (l.pod_start, l.finished)).collect();
    let container_runs: Vec<(i64, i64)> = lines.iter().map(|l| (l.started, l.finished)).collect();
    let _ = writeln!(report, "(a) wall {:.1}s for {FIXTURE_SAMPLES} samples at --workers 4", wall_a.as_secs_f64());
    for l in &lines {
        let _ = writeln!(report, "  pod {} ({}): startTime +{}s, container started +{}s, finished +{}s", l.pod, l.run_id, l.pod_start - lines.iter().map(|x| x.pod_start).min().unwrap(), l.started - lines.iter().map(|x| x.pod_start).min().unwrap(), l.finished - lines.iter().map(|x| x.pod_start).min().unwrap());
    }
    let _ = writeln!(
        report,
        "(a) independent watcher (50 ms polls of the API): at most {} pods existing and {} workload containers running at one poll; pods per job: all exactly 1. Pods at the peak: {:?}",
        watched.max_pods, watched.max_running, watched.running_at_peak
    );
    let _ = writeln!(report, "(a) from the pods' own timestamps (1 s resolution): at most {} pods alive at one instant (startTime..container finishedAt), at most {} containers running at one instant", max_overlap(&pod_lifetimes), max_overlap(&container_runs));
    let _ = writeln!(report, "(a) every pod was bound to: {:?} -- several pods on ONE node, not several hosts", watched.nodes);
    assert_eq!(watched.nodes.len(), 1, "this cluster has one node: {:?}", watched.nodes);
    assert!(max_overlap(&pod_lifetimes) >= 3, "fewer than 3 sample pods overlapped in time: {pod_lifetimes:?}");
    assert!(watched.max_pods >= 3, "the watcher never saw 3 pods at once: {watched:?}");
    describe(&mut report, &a);

    // (b) the same image under local Docker, 4 workers.
    let mut args = fixture_args(&out, 4);
    args.extend(env.job_args("container"));
    let run = run_av_sweep(&av_sweep_bin(), &args);
    assert!(run.success, "(b) container study failed: {}\n{}", run.status, run.stderr);
    let b = move_aside("b-container", &out, &env.work.path.join("b-container"), run.stderr);
    assert!(b.failed().is_empty(), "(b) failures: {:?}", b.failed().iter().map(|s| (&s.run_id, &s.error)).collect::<Vec<_>>());
    assert!(docker_containers_with(&format!("av.test.run_id={}", env.run_id)).iter().all(|n| n.starts_with("k8s_")), "a (b) container was left behind: {:?}", docker_containers_with(&format!("av.test.run_id={}", env.run_id)));
    describe(&mut report, &b);
    // The container executor returns the job's log (question 252, heavy-prec-5): a successful
    // container sample's stderr.txt is the container's own output, never empty. `stderr.txt` is not
    // one of the study's hashes (`move_aside` hashes sweep_results.pb/.json and the run_products.pb of
    // each sample: 14 for this fixture), so the byte-for-byte claim below does not depend on it.
    assert_eq!(b.stderr_txt.len(), FIXTURE_SAMPLES);
    for (name, text) in &b.stderr_txt {
        assert!(text.ends_with('\n') && text.contains("av-sweep"), "(b) {name}/stderr.txt is not the container's log: {text:?}");
    }
    let _ = writeln!(report, "(b) every container sample's stderr.txt holds the container's log; e.g. {}: {:?}", b.stderr_txt.keys().next().unwrap(), b.stderr_txt.values().next().unwrap());
    assert_eq!(b.hashes.len(), FIXTURE_SAMPLES + 2, "the compared hashes are the two result files and one run_products.pb per sample; no stderr.txt");

    // (c) this host's native av-sweep, built --no-default-features, as child processes.
    let mut args = fixture_args(&out, 4);
    args.extend(["--executor".into(), "process".into()]);
    let native = native_bin();
    let _ = writeln!(report, "(c) native binary {} sha256 {}", native.display(), sha256_file(&native));
    let run = run_av_sweep(&native, &args);
    assert!(run.success, "(c) native study failed: {}\n{}", run.status, run.stderr);
    let c = move_aside("c-native", &out, &env.work.path.join("c-native"), run.stderr);
    assert!(c.failed().is_empty());
    describe(&mut report, &c);

    // (c) again: the native run is deterministic on its own platform (so a difference between it
    // and the image is the platform, not nondeterminism).
    let run = run_av_sweep(&native, &args);
    assert!(run.success, "(c2) native study failed: {}\n{}", run.status, run.stderr);
    let c2 = move_aside("c2-native-again", &out, &env.work.path.join("c2-native-again"), run.stderr);
    let cc = compare(&mut report, &c, &c2);
    assert!(cc, "the native study is not reproducible on its own platform: {:?}", differences(&c, &c2));

    // The claim: (a) and (b) are byte-identical.
    let ab = compare(&mut report, &a, &b);
    // And (c) against them, either way.
    let ac = compare(&mut report, &a, &c);
    let bc = compare(&mut report, &b, &c);
    let _ = writeln!(report, "RESULT: k8s == container byte for byte: {ab}; k8s == native: {ac}; container == native: {bc}; native == native again: {cc}");
    if !ac {
        // Where they differ, how: every differing run_products.pb is compared again with the
        // trajectory means/covariances and the score values removed. The root cause of the
        // differences is the platform's libm (see the report of heavy-ct-2b): the same f64 inputs
        // give sin/cos/... results one ulp apart on Apple's libm and on glibc.
        let names: Vec<String> = differences(&a, &c).into_iter().filter(|n| n.ends_with("run_products.pb")).collect();
        let numeric_only = names.iter().filter(|n| numeric_only_difference(&a.dir.join(n), &c.dir.join(n))).count();
        let _ = writeln!(report, "native vs k8s: {} of {} run_products.pb differ; {numeric_only} of them differ ONLY in trajectory means, covariances and score values (everything else -- ids, events, provenance, hashes, frames -- is byte-identical)", names.len(), a.results.samples.len());
        let _ = writeln!(report, "native vs k8s: sweep_results.json score values of sample p0_d0: k8s {} | native {}", score_text(&a, "leo_speed_at_end"), score_text(&c, "leo_speed_at_end"));
    }
    // stderr.txt of a sample: the k8s pod's log versus the child's stderr (informational).
    let log_equal = a.stderr_txt.iter().filter(|(k, v)| c.stderr_txt.get(*k) == Some(v)).count();
    let _ = writeln!(report, "stderr.txt: {log_equal} of {} sample logs (k8s pod log) equal the native child's stderr byte for byte", a.stderr_txt.len());
    let container_equal = b.stderr_txt.iter().filter(|(k, v)| c.stderr_txt.get(*k) == Some(v)).count();
    let _ = writeln!(report, "stderr.txt: {container_equal} of {} sample logs (container log) equal the native child's stderr byte for byte", b.stderr_txt.len());
    let k8s_equal = b.stderr_txt.iter().filter(|(k, v)| a.stderr_txt.get(*k) == Some(v)).count();
    let _ = writeln!(report, "stderr.txt: {k8s_equal} of {} sample logs (container log) equal the k8s pod log byte for byte", b.stderr_txt.len());
    write_proof("three_way_byte_identity", &report);
    assert!(ab, "the k8s study and the container study differ: {:?}", differences(&a, &b));
    if std::env::var("AV_SWEEP_REQUIRE_NATIVE_EQUAL").as_deref() == Ok("1") {
        assert!(ac, "the native study differs from the k8s study: {:?}", differences(&a, &c));
    }
}

#[test]
fn a_docker_killed_sample_pod_is_one_failed_sample_with_no_second_pod_and_a_clean_rerun_reproduces_the_bytes() {
    let env = env_or_skip!("a_docker_killed_sample_pod_is_one_failed_sample_with_no_second_pod_and_a_clean_rerun_reproduces_the_bytes");
    let out = env.work.study_dir();
    let mut report = String::new();
    let _ = writeln!(report, "sample image {}  namespace {}  run {}  --out-dir for all three runs: {}", env.sample_image, env.namespace, env.run_id, out.display());

    // A clean reference run first (k8s, 4 workers), moved aside so the next run uses the same path.
    let mut args = fixture_args(&out, 4);
    args.extend(env.job_args("k8s"));
    let run = run_av_sweep(&av_sweep_bin(), &args);
    assert!(run.success, "reference run failed: {}\n{}", run.status, run.stderr);
    let reference = move_aside("reference-k8s", &out, &env.work.path.join("reference-k8s"), run.stderr);
    assert!(reference.failed().is_empty());
    assert_cluster_clean(&env, "after the reference run");
    describe(&mut report, &reference);

    // The killed run. `docker events` reports a sample container the instant it starts; the first
    // is `docker kill`ed (the container, not the pod).
    let mut events = Command::new("docker")
        .args(["events", "--format", "{{.Actor.ID}}", "--filter", "type=container", "--filter", "event=start", "--filter", &format!("label=io.kubernetes.pod.namespace={}", env.namespace), "--filter", "label=io.kubernetes.container.name=job"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("docker events");
    let killed: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let killer = {
        let killed = killed.clone();
        let stdout = events.stdout.take().unwrap();
        thread::spawn(move || {
            let mut line = String::new();
            if BufReader::new(stdout).read_line(&mut line).is_ok() {
                let id = line.trim().to_string();
                if id.is_empty() {
                    return;
                }
                // Kill first (the sample runs for a fraction of a second), ask whose it was after.
                let out = Command::new("docker").args(["kill", &id]).output().expect("docker kill");
                assert!(out.status.success(), "docker kill {id}: {}", String::from_utf8_lossy(&out.stderr));
                let pod = Command::new("docker").args(["inspect", &id, "--format", "{{index .Config.Labels \"io.kubernetes.pod.name\"}}"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                *killed.lock().unwrap() = Some((id, pod));
            }
        })
    };
    let watcher = PodWatcher::start(env.api.clone(), env.namespace.clone());
    let run = run_av_sweep(&av_sweep_bin(), &args);
    let watched = watcher.finish();
    let _ = events.kill();
    let _ = events.wait();
    let _ = killer.join();
    let (container_id, pod) = killed.lock().unwrap().clone().expect("a sample container started and was killed");
    assert!(run.success, "a failed sample must not fail the study: {}\n{}", run.status, run.stderr);
    let killed_study = move_aside("killed", &out, &env.work.path.join("killed"), run.stderr);
    assert_cluster_clean(&env, "after the killed run");

    let failed = killed_study.failed();
    let _ = writeln!(report, "docker kill of container {container_id} (pod {pod})");
    assert_eq!(failed.len(), 1, "exactly the killed sample fails: {:?}", failed.iter().map(|s| (&s.run_id, &s.error)).collect::<Vec<_>>());
    let f = failed[0];
    let _ = writeln!(report, "failed sample {} (p{} d{}): {}", f.run_id, f.point_index, f.draw_index, f.error);
    for want in ["JOB_FAILURE_KIND_NONZERO_EXIT", "exit code 137", "killed", &pod] {
        assert!(f.error.contains(want), "the error lacks {want:?}: {}", f.error);
    }
    assert!(f.scores.is_empty() && f.products_uri.is_empty());
    // No second pod for it -- by the study's own account (the pod named in the error) and by the
    // independent watcher's.
    let job_pods = &watched.pods_by_job[&label_value(&f.run_id)];
    assert_eq!(job_pods, &BTreeSet::from([pod.clone()]), "a second pod appeared for the killed sample: that is a retry");
    for (job, pods) in &watched.pods_by_job {
        assert_eq!(pods.len(), 1, "job {job} had pods {pods:?}");
    }
    assert_eq!(watched.pods_by_job.len(), FIXTURE_SAMPLES);
    let _ = writeln!(report, "pods ever seen for {}: {job_pods:?}; every one of the {} jobs had exactly one pod", f.run_id, watched.pods_by_job.len());
    // The rest of the study completed: every other sample ran and has scores.
    assert_eq!(killed_study.results.samples.len(), FIXTURE_SAMPLES);
    assert_eq!(killed_study.results.samples.iter().filter(|s| s.error.is_empty() && !s.scores.is_empty()).count(), FIXTURE_SAMPLES - 1);
    // The aggregates are computed over the surviving draws.
    assert_eq!(killed_study.results.aggregates.len(), 4 * 3);
    let _ = writeln!(report, "the other {} samples completed with scores; aggregates: {}", FIXTURE_SAMPLES - 1, killed_study.results.aggregates.len());
    describe(&mut report, &killed_study);

    // A clean rerun at the same --out-dir path reproduces the reference's bytes exactly.
    let mut args = fixture_args(&out, 4);
    args.extend(env.job_args("k8s"));
    let run = run_av_sweep(&av_sweep_bin(), &args);
    assert!(run.success, "clean rerun failed: {}\n{}", run.status, run.stderr);
    let rerun = move_aside("rerun-k8s", &out, &env.work.path.join("rerun-k8s"), run.stderr);
    assert!(rerun.failed().is_empty());
    assert_cluster_clean(&env, "after the clean rerun");
    describe(&mut report, &rerun);
    let same_as_reference = compare(&mut report, &rerun, &reference);
    // The killed run's surviving samples equal the reference's, sample for sample.
    let survivors: Vec<(&String, &String)> = killed_study.hashes.iter().filter(|(n, _)| n.ends_with("run_products.pb")).collect();
    let survivors_equal = survivors.iter().all(|(name, hash)| reference.hashes.get(*name) == Some(*hash));
    let _ = writeln!(report, "RESULT: the {} surviving samples' run_products.pb equal the reference's: {survivors_equal}; the clean rerun equals the reference byte for byte (sweep_results.pb, sweep_results.json, all 12 run_products.pb): {same_as_reference}", survivors.len());
    write_proof("killed_sample_pod", &report);
    assert!(survivors_equal, "a survivor of the killed run differs from the reference run");
    assert!(same_as_reference, "the clean rerun differs from the reference: {:?}", differences(&rerun, &reference));
    // And the killed study's own record differs from the reference exactly where it should: the
    // killed sample has no products and no score, so the killed study's sweep_results differ.
    assert_ne!(killed_study.hashes["sweep_results.pb"], reference.hashes["sweep_results.pb"]);
}

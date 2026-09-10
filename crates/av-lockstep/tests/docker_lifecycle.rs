//! Docker image lifecycle acceptance test (`docs/open-questions.md` question 118, M15.3): build
//! `services/lockstep-ref`'s own `Dockerfile`, push the built image to a throwaway,
//! loopback-only local `registry:2` container this test starts and stops itself (never a
//! real/external registry -- question 118's own "No image is pushed anywhere" rule), recover the
//! real digest Docker assigned it on push, and drive [`av_lockstep::docker::ManagedContainer`]
//! through the whole lifecycle end to end against that genuinely pulled-by-digest image: pull,
//! run (published on loopback), a real `Bind` RPC, `Reset`/`Shutdown`, then stop and remove.
//!
//! Also proves question 118's "`binding_hash` includes the digest": two containers are run from
//! the identical pulled image with two different `IMAGE_DIGEST` environment values (the value
//! `services/lockstep-ref/lockstep_ref/server.py`'s own `Bind` handler folds into
//! `LockstepBindResponse.binding_hash` -- see that file's own doc comment) and otherwise
//! identical `Bind` parameters; the two resulting hashes must differ. **What this half of the
//! test would fail against:** an implementation that pulls/runs the image correctly but never
//! threads `image_digest` into the container's own environment at all -- `hash_a == hash_b`
//! regardless of what `IMAGE_DIGEST` is set to, since the two `Bind` calls would otherwise be
//! byte-identical.
//!
//! ## Gated on `docker info`, and genuinely visible in a plain `cargo test` run (question 194)
//!
//! Question 118: "tests run only when `docker info` succeeds, and otherwise skip with a
//! recorded reason visible in a plain pytest -q / cargo test run -- never silently, never
//! buried." Question 194 (round 5-6) sharpened this: the pre-R6.3 shape here was
//! `println!("SKIPPED ...")` then `return` -- verified empirically (this crate's own
//! `R6_3_REPORT.md` section 1) to be genuinely INVISIBLE in a plain `cargo test` run, because
//! `cargo test`'s default runner never prints a PASSING test's `println!`/`eprintln!` output
//! without `--nocapture`. Every Docker-gated test in this file now goes through
//! [`av_lockstep::docker::docker_daemon_status`] (a typed `DockerGateReason`, not a bare bool)
//! and [`av_lockstep::docker::announce_gate_skip`], which writes the skip line via a raw
//! `std::io::stderr().write_all(...)` -- measured, in the same report section, to remain visible
//! where `println!`/`eprintln!` do not, because libtest's output-capture hook is wired into the
//! `print!`/`eprintln!` macros' own `io::_print`/`io::_eprint` helper functions, never into
//! `Stdout`/`Stderr`'s own `Write` impl. Each test below asserts on the text the helper actually
//! returned (question 194: "the helper returns the announced text, and the test asserts it
//! printed"). The Python side (`tests/test_lockstep_ref.py::test_docker_image_lifecycle_...`,
//! gated via `pytest.skip`, `pyproject.toml`'s `-rs` addopt) remains independently
//! verified-visible too -- see that test's own doc comment -- but this file no longer needs to
//! defer to it for the Rust side's own visibility claim.
//!
//! Every `docker`/`registry:2` resource this test creates is torn down before it returns, success
//! or failure (`Drop` guards, mirroring `crates/av-kernel/tests/drm_container.rs`'s own
//! `ChildGuard` convention for a subprocess).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use av_cdm::pb::{Port, PortDirection, PortKind};
use av_lockstep::docker::{announce_gate_skip, docker_daemon_status, prune_stale_test_resources, test_label_args, test_run_id, ManagedContainer, TEST_LABEL_KEY};
use av_lockstep::{BlockingLockstepClient, LockstepBindRequest, LockstepShutdownRequest};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run `docker <args>`, panicking with the real stderr on failure -- this test's own fixture
/// setup, not the code under test (that is exercised entirely through [`ManagedContainer`]).
fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).current_dir(repo_root()).output().unwrap_or_else(|e| panic!("could not launch `docker {args:?}`: {e}"));
    if !output.status.success() {
        panic!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Stops and removes a container id on drop -- so a failing assertion partway through this test
/// can never leak the throwaway registry.
struct ContainerGuard(String);
impl Drop for ContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

/// Removes an image reference on drop -- best-effort (an image still referenced by a container
/// this same test already tore down; ignored either way).
struct ImageGuard(String);
impl Drop for ImageGuard {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rmi", "-f", &self.0]).output();
    }
}

const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// `cargo test` runs every `#[test]` in one file concurrently by default (same process,
/// multiple threads) unless told otherwise. That is fine for tests with no shared state, but
/// every test below shares the Docker daemon's own global namespace and, more sharply, the
/// [`av_lockstep::docker::TEST_LABEL_KEY`] prune sweep -- two tests racing would let one's
/// `prune_stale_test_resources()` call rip out a container or image the other is still using
/// mid-run. Held for a whole test's duration (not just around the prune call) so "build this
/// image" in one test can never interleave with "prune everything labeled" in another.
static DOCKER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `docker run -d` returning (and `docker port` resolving a host port) only means the container
/// process has *started*, not that `lockstep_ref`'s own gRPC server inside it is already
/// accepting connections -- poll with a real connect attempt (mirrors
/// `crates/av-kernel/tests/drm_container.rs`'s own `spawn_lockstep_ref` readiness loop for the
/// bare-subprocess path) rather than assume it is instantaneous.
fn connect_when_ready(address: &str) -> BlockingLockstepClient {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match BlockingLockstepClient::connect_plaintext(address) {
            Ok(client) => return client,
            Err(e) => {
                if Instant::now() > deadline {
                    panic!("the Docker-run container at {address} did not become ready within {READY_TIMEOUT:?}: {e}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn good_ports() -> Vec<Port> {
    vec![
        Port { name: "in".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, ..Default::default() },
        Port { name: "out".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, ..Default::default() },
    ]
}

// ------------------------------------------------------------------------------------------
// Question 194's own visibility requirement, pinned as a permanent regression (not just a
// one-time manual probe -- see crates/av-lockstep/R6_3_REPORT.md section 1 for that original
// measurement, which this test automates).
// ------------------------------------------------------------------------------------------

/// Spawns a genuinely SEPARATE `cargo test` invocation (no `--nocapture`) targeting one
/// specific unit test that calls `announce_gate_skip` exactly once
/// (`docker::tests::announce_gate_skip_returns_the_line_it_announced`, `crates/av-lockstep/src/
/// docker.rs`), and inspects THAT subprocess's own real, captured stdout for the literal
/// `SKIPPED ...` line -- proving end to end, through a real nested `cargo test` process (not
/// merely in-process reasoning), that the announcement mechanism is genuinely visible in a
/// plain `cargo test` run.
///
/// **What this fails against.** If `announce_gate_skip` regressed to `println!`/`eprintln!`
/// (the pre-R6.3 shape every gated test in this workspace used to have), the subprocess's own
/// captured stdout would show only `test ... ok` with no `SKIPPED` line at all -- measured
/// directly for this exact regression in this crate's own R6_3_REPORT.md section 1 (the
/// `r63_probe_println`/`r63_probe_eprintln` probes), and the `assert!` below would fail.
#[test]
fn announce_gate_skip_is_actually_visible_in_a_real_cargo_test_subprocess_without_nocapture() {
    let output = Command::new("cargo")
        .args(["test", "-p", "av-lockstep", "--lib", "--", "--exact", "docker::tests::announce_gate_skip_returns_the_line_it_announced"])
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("could not launch `cargo test`: {e}"));
    let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "the targeted subprocess test must itself pass:\n{combined}");
    assert!(
        combined.contains("SKIPPED some_test_name: image \"some/image:tag\" is not built locally -- run the build script"),
        "the real SKIPPED line must be visible in the subprocess's own captured stdout/stderr without --nocapture -- got:\n{combined}"
    );
}

#[test]
fn docker_image_lifecycle_pull_by_digest_run_bind_reset_shutdown_stop_remove() {
    if let Err(reason) = docker_daemon_status() {
        let line = announce_gate_skip("docker_image_lifecycle_pull_by_digest_run_bind_reset_shutdown_stop_remove", &reason);
        assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
        return;
    }
    let _lock = DOCKER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Question 156's amendment: prune whatever a previous, interrupted run of this (or any
    // other) AltaVista test left behind (a `Drop` guard never runs on `SIGKILL`) *before*
    // creating anything new -- see `prune_stale_test_resources`'s own doc comment.
    prune_stale_test_resources();
    let run_id = test_run_id();
    let labels = test_label_args(&run_id);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    // 1. Build the image the same way a human operator would (services/lockstep-ref/Dockerfile's
    //    own doc comment: "Build from the REPOSITORY ROOT"), carrying the test marker label
    //    (question 156) so it is swept by `prune_stale_test_resources` if this run is killed
    //    before its own `ImageGuard` runs.
    let local_tag = "lockstep-ref:av-lockstep-docker-lifecycle-test";
    let mut build_args = vec!["build", "-f", "services/lockstep-ref/Dockerfile", "-t", local_tag];
    build_args.extend(label_refs.iter().copied());
    build_args.push(".");
    docker(&build_args);
    let _local_image_guard = ImageGuard(local_tag.to_string());

    // 2. A throwaway, loopback-only local registry -- question 118's own "no real/external
    //    registry" rule; this container (and everything pushed to it) is removed at the end of
    //    this test, never left running. Labeled for the same reason as the image above.
    let mut registry_args = vec!["run", "-d", "-p", "127.0.0.1::5000"];
    registry_args.extend(label_refs.iter().copied());
    registry_args.push("registry:2");
    let registry_id = docker(&registry_args);
    let _registry_guard = ContainerGuard(registry_id.clone());
    let registry_port_line = docker(&["port", &registry_id, "5000"]);
    let registry_port: u16 = registry_port_line
        .lines()
        .next()
        .and_then(|l| l.rsplit(':').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("could not parse a host port out of `docker port` output {registry_port_line:?}"));
    let pushed_ref = format!("127.0.0.1:{registry_port}/lockstep-ref:test");

    // 3. Tag and push -- a real registry round trip, not a locally-cached no-op: `docker pull`
    //    below resolves this exact `<repo>@<digest>` reference against the registry this test
    //    just stood up, proving the pull is genuine.
    docker(&["tag", local_tag, &pushed_ref]);
    let _pushed_image_guard = ImageGuard(pushed_ref.clone());
    docker(&["push", &pushed_ref]);

    // 4. Recover the real digest Docker assigned on push (never invented/assumed).
    let repo_digests = docker(&["inspect", "--format={{index .RepoDigests 0}}", &pushed_ref]);
    let real_digest = repo_digests.rsplit('@').next().filter(|d| d.starts_with("sha256:")).unwrap_or_else(|| panic!("expected a @sha256:... RepoDigests entry, got {repo_digests:?}")).to_string();
    let image_ref = format!("127.0.0.1:{registry_port}/lockstep-ref");

    // 5. Pull by digest, run (published on loopback), Bind -- twice, with two different
    //    IMAGE_DIGEST env values (the *real* `real_digest` is always what is actually pulled;
    //    only the env value handed to the running process differs) to prove `binding_hash`
    //    actually incorporates it, not just that a Bind succeeds at all.
    let hash_for_digest_env = |digest_env: &str| -> (String, ContainerGuard) {
        let mut env = BTreeMap::new();
        env.insert("IMAGE_DIGEST".to_string(), digest_env.to_string());
        let mut container_labels = BTreeMap::new();
        container_labels.insert(TEST_LABEL_KEY.to_string(), "1".to_string());
        container_labels.insert("av.test.run_id".to_string(), run_id.clone());
        let (mut managed, host_port) =
            ManagedContainer::pull_and_run(&image_ref, &real_digest, &[], 50070, &BTreeMap::new(), &env, &BTreeMap::new(), &container_labels).unwrap_or_else(|e| panic!("pull_and_run({digest_env:?}) failed: {e}"));
        let container_id = managed.container_id.clone();

        let address = format!("127.0.0.1:{host_port}");
        let mut client = connect_when_ready(&address);
        // `connect_when_ready`'s own successful `connect_plaintext` proves the container's TCP
        // listen socket is open, but not necessarily that its gRPC server's own thread pool has
        // finished starting up -- a `Bind` sent in that narrow window can still see a transport
        // error even though a moment later it would succeed. Retry the RPC itself (not just the
        // connect) within the same readiness deadline, exactly the "poll a real call, never a
        // bare sleep" rule this crate's own subprocess tests already follow one layer down.
        let bind_deadline = Instant::now() + READY_TIMEOUT;
        let bind = loop {
            let request = LockstepBindRequest { run_id: "docker-lifecycle-test".to_string(), instance: "sig".to_string(), ports: good_ports(), start_tai_ns: 0, base_period_ns: 1_000_000_000, step_period_ns: 1_000_000_000, seed: 1, parameters: BTreeMap::new() };
            match client.bind(request) {
                Ok(resp) => break resp,
                Err(_) if Instant::now() < bind_deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => panic!("Bind RPC did not succeed within {READY_TIMEOUT:?}: {e}"),
            }
        };
        assert!(bind.lockstep_capable, "Bind refused: {:?}", bind.refusal_reason);
        assert_eq!(bind.binding_hash.len(), 64, "binding_hash must be a hex-encoded SHA-256: {:?}", bind.binding_hash);

        // Question 118's own "Bind over loopback" / "Shutdown then stop and remove on run end":
        // exercise the whole remaining lifecycle on this same managed instance before this
        // closure hands back the guard for the caller to drop explicitly (a `ContainerGuard` is
        // still returned so a panic between here and the caller's own explicit teardown cannot
        // leak the container either).
        client.shutdown(LockstepShutdownRequest { run_id: "docker-lifecycle-test".to_string() }).unwrap_or_else(|e| panic!("Shutdown RPC: {e}"));
        managed.stop_and_remove().unwrap_or_else(|e| panic!("stop_and_remove: {e}"));

        // Prove it is actually gone -- not merely stopped (question 118: "stop and remove").
        let ps = Command::new("docker").args(["ps", "-a", "-q", "--filter", &format!("id={container_id}")]).output().expect("docker ps");
        assert!(String::from_utf8_lossy(&ps.stdout).trim().is_empty(), "container {container_id} must be removed after stop_and_remove, docker ps -a still shows it");

        (bind.binding_hash, ContainerGuard(container_id)) // already removed; guard is a harmless no-op safety net
    };

    let (hash_a, _guard_a) = hash_for_digest_env("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let (hash_b, _guard_b) = hash_for_digest_env("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert_ne!(hash_a, hash_b, "binding_hash must differ when IMAGE_DIGEST differs (question 118: \"binding_hash includes the digest\") -- otherwise the digest was never actually threaded into the running container's own environment");
}

/// Question 156's amendment (`docs/open-questions.md`, 2026-09-06): the guard itself, tested
/// directly rather than only trusted by inspection. Stands up a labeled throwaway registry
/// container and a labeled image tagged and pushed to it -- the exact shape of the real
/// incident (question 156's own original finding: a stale `127.0.0.1:<port>/...`-tagged image;
/// its amendment: a stale `registry:2` container) -- **deliberately with no `ContainerGuard`/
/// `ImageGuard` teardown at all**, simulating exactly the failure mode the amendment describes:
/// a test process killed before any `Drop` impl could run. Then calls
/// `prune_stale_test_resources()` the same way the *next* test run would at its own start, and
/// asserts both halves of question 156: no container carrying the test label remains, and
/// (question 156's own original wording) no image tagged with this test's own throwaway
/// registry's port remains.
///
/// **What this would fail against.** A `prune_stale_test_resources` that only removed
/// containers (not images) would pass the container assertion and fail the image one -- the
/// original question 156 finding was specifically about a leftover *image* tag, not a
/// container. A `prune_stale_test_resources` that filtered on the per-run id instead of the
/// standing marker label would find nothing here (this test's `run_id` is fresh, matching
/// nothing a *previous* run could have left) and every assertion below would fail.
#[test]
fn prune_stale_test_resources_removes_orphaned_labeled_containers_and_images() {
    if let Err(reason) = docker_daemon_status() {
        let line = announce_gate_skip("prune_stale_test_resources_removes_orphaned_labeled_containers_and_images", &reason);
        assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
        return;
    }
    let _lock = DOCKER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    prune_stale_test_resources(); // clean slate, same as every other test in this file

    let run_id = test_run_id();
    let labels = test_label_args(&run_id);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let mut run_args = vec!["run", "-d", "-p", "127.0.0.1::5000"];
    run_args.extend(label_refs.iter().copied());
    run_args.push("registry:2");
    let registry_id = docker(&run_args);
    let registry_port_line = docker(&["port", &registry_id, "5000"]);
    let registry_port: u16 = registry_port_line
        .lines()
        .next()
        .and_then(|l| l.rsplit(':').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("could not parse a host port out of `docker port` output {registry_port_line:?}"));
    let pushed_ref = format!("127.0.0.1:{registry_port}/av-test-orphan-check:test");

    let mut build_args = vec!["build", "-f", "services/lockstep-ref/Dockerfile", "-t", &pushed_ref];
    build_args.extend(label_refs.iter().copied());
    build_args.push(".");
    docker(&build_args);
    docker(&["push", &pushed_ref]);

    // Deliberately no ContainerGuard/ImageGuard, and no explicit teardown before pruning -- see
    // this test's own doc comment for why that is the point, not an oversight.
    assert!(
        !docker(&["ps", "-a", "-q", "--filter", &format!("label={TEST_LABEL_KEY}")]).is_empty(),
        "the labeled registry container must exist before pruning, or this test proves nothing"
    );
    assert!(
        !docker(&["images", "-q", "--filter", &format!("label={TEST_LABEL_KEY}")]).is_empty(),
        "the labeled image must exist before pruning, or this test proves nothing"
    );

    prune_stale_test_resources();

    let remaining_containers = docker(&["ps", "-a", "-q", "--filter", &format!("label={TEST_LABEL_KEY}")]);
    assert!(remaining_containers.is_empty(), "a labeled container survived prune_stale_test_resources: {remaining_containers:?}");
    let remaining_images = docker(&["images", "-q", "--filter", &format!("label={TEST_LABEL_KEY}")]);
    assert!(remaining_images.is_empty(), "a labeled image survived prune_stale_test_resources: {remaining_images:?}");

    // Question 156's own original wording, checked directly against the tag (not only via the
    // label filter above): nothing tagged with this test's own registry port remains.
    let inspect = Command::new("docker").args(["image", "inspect", &pushed_ref]).output().expect("docker image inspect");
    assert!(!inspect.status.success(), "image {pushed_ref:?}, tagged with this test's own registry port, must not exist after prune_stale_test_resources");
}

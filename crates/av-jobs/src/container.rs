//! H3, round 3, task P3b (`docs/heavy-plan.md` H3's last open item): the
//! `JOB_EXECUTOR_KIND_CONTAINER` [`crate::runner::Executor`] -- runs a job's command inside a
//! labelled container from a digest-pinned image, the same posture `services/edge-plugin`
//! already runs under. Registered onto a [`crate::runner::Runner`] via
//! [`crate::runner::Runner::register_container_executor`], never through the ordinary
//! `kind -> Executor` registry (`crate::runner::Runner::register_executor`) -- see that
//! method's own doc, and `crate::runner::Runner::execute_spec`'s own Step 4, for why a
//! CONTAINER job's `spec.kind` plays no role in choosing how it runs.
//!
//! # Why this crate shells out to `docker` itself, rather than depending on `av-lockstep`
//!
//! `crates/av-lockstep/src/docker.rs` already has almost everything this needs
//! (`local_image_id`, `recorded_digest_gate`, labelling, `ManagedContainer`'s teardown
//! discipline) -- and `crates/av-jobs/tests/store_tiler.rs`/this crate's own
//! `tests/container.rs` both depend on it as a **dev**-dependency, exactly for that reason.
//! But `av-lockstep` is, by its own crate doc, a tonic client for `altavista.v1.LockstepService`
//! -- a DRM-binding concern, nothing to do with running a batch job's command. Pulling it into
//! `av-jobs`'s own **production** dependency graph (`[dependencies]`, not `[dev-dependencies]`)
//! would wire this crate to a tonic/gRPC stack it otherwise has no use for, purely to reach a
//! handful of `std::process::Command` calls this module can make directly just as easily. This
//! mirrors `crate::hash`'s own module doc ("`av-edge` is off this heavy track entirely... the
//! two lines stay duplicated, deliberately") and `crate::clock`'s own two-method `Clock` trait:
//! this crate reimplements the small, `docker`-CLI-shaped primitives it actually needs (image
//! inspection by digest, labelling, teardown) rather than depend on a crate shaped for a
//! different domain. Every one of those primitives is a handful of lines around
//! `std::process::Command::new("docker")` -- no new dependency, registry or in-workspace,
//! appears in this crate's own `[dependencies]` because of this module.
//!
//! # How inputs/outputs cross the container boundary: a bind mount, not stdin/stdout
//!
//! `crate::runner::Runner::execute_spec` already fetches and hash-verifies every input before
//! any [`crate::runner::Executor`] ever sees it, and stores every output afterward
//! (`crate::runner::ObjectSource`/`ObjectSink`'s own doc comments) -- this executor never talks
//! to an object store itself, on either side. Getting already-verified bytes INTO the
//! container, and its own output bytes back OUT, had two honest options: stdin/stdout (one
//! input, one output, byte-stream only), or a bind mount (arbitrary file count on both sides,
//! and the one mechanism Colima's Docker actually supports for host<->container data --
//! `crates/av-lockstep/src/docker_test_lock.rs`'s own module doc has the measured account: a
//! bind-mount source outside `$HOME` silently becomes an EMPTY directory inside the container,
//! with no error anywhere). `JobSpec.inputs` is `repeated` and a real job kind can produce
//! several outputs (`crate::tiler::TilerExecutor` does, in-process) -- stdin/stdout has no
//! honest way to carry more than one stream of either without this executor inventing its own
//! framing on top, which is exactly the kind of "a second, independently-drifting way" this
//! codebase's own doc comments elsewhere argue against. So: **a bind mount**. Every input is
//! written, in `JobSpec.inputs`' own order, as `<scratch>/in/000`, `<scratch>/in/001`, ... under
//! a fresh, `$HOME`-relative scratch directory (see [`ContainerExecutor::new`]'s own doc for
//! why the caller supplies that root, not this module), bind-mounted **read-only** at
//! `/av-job/in` -- an executor must never be able to mutate an input this runner already
//! hash-verified. `<scratch>/out` is bind-mounted **read-write** at `/av-job/out`, chmod
//! `0o777` before the container ever starts so the fixed non-root [`CONTAINER_UID_GID`] this
//! executor always runs as can write there regardless of which host user created the scratch
//! directory (mirrors `tests/test_edge_plugin_hardening_alpine.py`'s own `chown` priming step
//! for the identical reason, stated in that file's own doc comment). Every regular file present
//! under `<scratch>/out` once the container's command exits zero becomes one
//! [`crate::runner::JobOutput`], in sorted filename order -- deterministic, and requiring no
//! cooperation from the containerized command beyond "write your output files under
//! `/av-job/out`".
//!
//! # Hardening: reused, not reinvented
//!
//! [`HARDENING_RUN_FLAGS`] is byte-for-byte `altavista/container_hardening.py::
//! HARDENING_RUN_FLAGS` (question 207's ruling on what Docker-on-Colima can express: a
//! non-root user, a read-only root filesystem, `--cap-drop ALL`, `--security-opt
//! no-new-privileges`, and the DEFAULT seccomp profile -- proven by never overriding it, not by
//! naming it). There is no mechanism in this crate's production code to import a Python list at
//! Rust compile time (and adding one would be a new dependency this task's own rules forbid), so
//! the flags are declared a second time here, in Rust -- kept from drifting apart from the
//! Python original by
//! `tests::hardening_run_flags_matches_the_python_declaration_in_container_hardening_py`, below,
//! which reads `altavista/container_hardening.py` at test time, parses its own
//! `HARDENING_RUN_FLAGS` list literal out of the source text, and asserts the two lists are
//! equal element for element. [`CONTAINER_UID_GID`] is the identical fixed uid:gid
//! `tests/test_edge_plugin_hardening_alpine.py::PROBE_UID_GID` already uses for the same reason
//! that file's own module doc gives: chosen for consistency, needing no `/etc/passwd` entry in
//! whatever image is named -- always passed explicitly (unlike `services/edge-plugin`'s own
//! image, which bakes a `USER` in), because this executor cannot assume an arbitrary
//! caller-named image declares a non-root user of its own.
//!
//! `--network none` is added on top of [`HARDENING_RUN_FLAGS`] (not folded into that constant,
//! so the parity test above stays a direct, unqualified comparison against the Python list,
//! which deliberately excludes it too) -- this executor's whole contract is "verified bytes in,
//! produced bytes out, never touching the object store or anything else" (this module's own
//! doc, above), so the container gets no network at all, belt-and-suspenders on top of the
//! object store never being reachable from inside it in the first place.
//!
//! # Labelling and teardown (question 156)
//!
//! Every container this executor creates carries `av.job=1` and `av.job.id=<JobSpec.job_id>`
//! ([`JOB_LABEL_KEY`]/[`JOB_LABEL_VALUE`]) plus whatever `extra_labels`
//! [`ContainerExecutor::new`] was constructed with -- a docker-gated test passes its own
//! `av.test`/`av.test.run_id` labels there (`av_lockstep::docker::test_label_args`, converted to
//! a map -- see `tests/container.rs`'s own `labels_from_test_label_args`) so the SAME
//! `docker ps -a --filter label=av.test` this workspace's every other docker-gated test is swept
//! by also finds this executor's own containers if one is ever left behind. [`Cleanup`]'s own
//! `Drop` guarantees `docker rm -f` runs on every exit path out of
//! [`ContainerExecutor::execute`] -- success, a typed refusal, or an early `?` -- the same
//! unconditional-removal guarantee `av_lockstep::docker::ManagedContainer`'s own `Drop` gives
//! its container, applied here without depending on that crate (see this module's own doc,
//! above). This executor never creates a named `docker volume` at all (bind mounts only), so
//! `docker volume ls --filter label=av.test` is empty by construction, not by cleanup.
//!
//! # Never pulls (question 154); the digest gate (question 212(a))
//!
//! [`digest_gate`] runs `docker image inspect <JobSpec.container_image>@<JobSpec.
//! container_image_digest> --format {{.Id}}` -- a reference by CONTENT ADDRESS, resolving only an
//! image already present on this host (exactly like `av_lockstep::docker::local_image_id`), and
//! compares the id it reports against `JobSpec.container_image_digest`: the job's own request
//! plays the role `av_lockstep::docker::recorded_digest_gate`'s own `recorded_id` parameter plays
//! for a test (question 212(a)'s "a test trusts an image only after comparing the running image
//! to a recorded digest, and refuses rather than trusts on a mismatch" -- applied at RUN time,
//! with the caller's own requested digest as the thing trusted only after comparison). The gate
//! inspects the digest reference, never the bare name, so a floating tag (`alpine:latest`) that
//! moves, or is garbage-collected and re-pulled at a newer digest, cannot make a pinned image
//! look absent or substitute a different one (heavy-ct-4: the recorded `alpine:latest` id went
//! stale that way). Three distinct, typed outcomes, none of them a `docker pull`:
//!
//! 1. the image absent entirely (neither the digest reference nor the bare name resolves);
//! 2. the digest reference does not resolve, or resolves to a different id than requested, while
//!    the bare name may resolve to some OTHER image -- refused, never silently substituted (the
//!    bare name is inspected only afterwards, only to make the refusal message say what IS
//!    present; it is never the trust path);
//! 3. a match -- in which case `docker run` is given the same fully digest-pinned reference
//!    (`"<image>@<digest>"`), never the bare name, so the daemon itself enforces content
//!    addressing for the actual run.
//!
//! On this host's containerd image store `{{.Id}}` of an index-pinned image IS the index digest,
//! so the recorded digest, the `@sha256:` reference and the id all coincide (measured; see
//! `tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md`).
//!
//! # Failure-kind reuse (not a new `JobFailureKind` -- see `heavy.proto`'s own enum)
//!
//! - An absent image, or a present image under the wrong digest: `EXECUTOR_UNAVAILABLE` --
//!   this crate's own pre-existing kind for "the container executor cannot run this job right
//!   now", broadened from "not implemented this round" to also cover "not trusted right now"
//!   (untrusted for the same structural reason: this Runner cannot run the job).
//! - `docker run` itself never gets the container started at all -- Docker's own reserved exit
//!   codes `125`/`126`/`127` (daemon-level failure / command found but not executable / command
//!   not found; measured against this host's own real Docker, and exercised by `tests/
//!   container.rs::container_executor_runs_a_real_job_inside_a_real_container`'s own "Part 3")
//!   -- `EXECUTOR_START_FAILED`, the same kind `crate::runner::ProcessExecutor` already uses for
//!   "the command could not even be spawned".
//! - Any OTHER non-zero `docker run` exit is `docker run` (run in the foreground, never `-d`)
//!   faithfully propagating the CONTAINED command's own real exit status -- `NONZERO_EXIT`,
//!   with that real code, exactly parallel to `ProcessExecutor`'s own non-zero-exit handling.
//!
//! # The job's log (question 252, heavy-prec-5)
//!
//! [`ContainerExecutor::run`] returns a [`ContainerReport`] -- the [`Executor`] result plus the
//! container's log -- the way [`crate::k8s::K8sJobExecutor::run`] returns a `RunReport`;
//! [`Executor::execute`] is `run(..).result`, so no other `Executor` implementation and no
//! `Executor` caller changes (the trait stays as it was; a trait change would have touched
//! `ProcessExecutor`, `TilerExecutor`, `K8sJobExecutor` and every test double for one executor's
//! benefit).
//!
//! - **When.** The container is not `--rm`; `docker run` (foreground) returns once the command has
//!   exited and its streams are drained, and [`Cleanup`] removes the container only when
//!   `run` returns. The log is read in between, with `docker logs --timestamps --tail <max
//!   lines + 1>`, on every path where `docker run` ran -- success, nonzero exit, start failure --
//!   before the container is removed. `docker run`'s own stdout is discarded (the log, not the
//!   run's pipe, is the record) and its stderr is read through a bounded tail
//!   ([`RUN_STDERR_KEEP`]), so a chatty job cannot exhaust this process's memory.
//! - **No silent empty log.** If there is no container (the image gate or scratch setup failed
//!   before `docker run`), or `docker logs` fails (the container is gone: `No such container`; the
//!   daemon's log driver cannot be read; docker could not be launched), `log` is `None` and
//!   `log_error` says why.
//! - **Caps.** Exactly the k8s executor's: [`crate::k8s`]'s `shape_log` (the same code, shared as
//!   `pub(crate)`; the same 64 KiB / 1000-line defaults and the same two marker lines) applied to
//!   the merged text; the failure detail's tail is the k8s `log_tail` (last 15 lines, at most 1500
//!   bytes). The bytes read from each `docker logs` stream are themselves bounded
//!   (cap + 31 bytes per line kept), so an unbounded single line cannot exhaust memory either.
//! - **Interleaving.** The daemon's log has two streams (stdout, stderr) and `docker logs` prints
//!   them to two pipes. The per-line timestamp the daemon stamps (fixed-width RFC 3339, 1 ns
//!   resolution) is the only cross-stream order available, so the two are merged by it (a tie keeps
//!   stdout first) and the timestamp is then dropped, giving one text like a pod log. Order within
//!   a stream is exact; across streams it is the order the daemon read the two pipes in, which
//!   measurably differs from the order the job wrote in by microseconds in a tight loop (the
//!   daemon reads each pipe on its own goroutine), faithful at the granularity of anything a human
//!   reads. A final line the job printed without a newline gets one. Stream identity is not
//!   marked (a pod log has none either).

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use av_cdm::pb;

use crate::k8s::{log_tail, shape_log, LogCapture, DEFAULT_LOG_CAP_BYTES, DEFAULT_LOG_MAX_LINES};
use crate::runner::{Executor, JobInput, JobOutput};

/// Question 156's label convention, for containers THIS EXECUTOR creates -- distinct from
/// `av_lockstep::docker::TEST_LABEL_KEY` (`"av.test"`), which that crate's own doc comment
/// reserves for resources a *test* creates. A production job run is not a test, so it gets its
/// own key; a docker-gated test that runs a real job through this executor adds `av.test`/
/// `av.test.run_id` on top, via `extra_labels` (see [`ContainerExecutor::new`]'s own doc).
pub const JOB_LABEL_KEY: &str = "av.job";
pub const JOB_LABEL_VALUE: &str = "1";

/// Byte-for-byte `altavista/container_hardening.py::HARDENING_RUN_FLAGS` -- see this module's
/// own doc, "Hardening: reused, not reinvented", for why this is declared a second time here
/// rather than imported, and for the test that keeps the two from drifting apart.
pub const HARDENING_RUN_FLAGS: &[&str] = &["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"];

/// The fixed non-root uid:gid this executor always runs a container as -- identical value to
/// `tests/test_edge_plugin_hardening_alpine.py::PROBE_UID_GID`. See this module's own doc,
/// "Hardening: reused, not reinvented", for why a fixed value rather than the host's own uid.
pub const CONTAINER_UID_GID: &str = "10001:10001";

/// The container-side mount points every job's command reads/writes at -- fixed, never derived
/// from `JobSpec`, so a job's own `command` can reference them as plain constants.
pub const CONTAINER_INPUT_DIR: &str = "/av-job/in";
pub const CONTAINER_OUTPUT_DIR: &str = "/av-job/out";

/// The `JOB_EXECUTOR_KIND_CONTAINER` [`Executor`] -- see this module's own doc for the full
/// design (bind-mount data flow, hardening, labelling/teardown, the digest gate, and which
/// `JobFailureKind` each failure mode reuses).
#[derive(Debug)]
pub struct ContainerExecutor {
    scratch_root: PathBuf,
    extra_labels: BTreeMap<String, String>,
}

impl ContainerExecutor {
    /// `scratch_root` is where this executor creates one fresh subdirectory per job run (its
    /// own `in`/`out` bind-mount sources) -- it is removed again once that job's run ends,
    /// success or failure alike (this module's own doc, "Labelling and teardown"). **Must be a
    /// directory under `$HOME`**: Colima (this platform's Docker runtime) only bind-mounts
    /// `$HOME` into its VM (`crates/av-lockstep/src/docker_test_lock.rs`'s own module doc has
    /// the measured account of what happens otherwise -- a silent empty directory, no error).
    /// This constructor does not itself verify that (the caller -- production wiring code, or a
    /// test's own `$HOME`-relative convention, e.g. this crate's own `tests/container.rs`) is in
    /// the better position to know it, the same trust `crate::runner::MemoryObjectSink::new`
    /// already places in its own caller-supplied `prefix`.
    ///
    /// `extra_labels` (question 156) is attached to every container this executor creates, IN
    /// ADDITION to its own fixed [`JOB_LABEL_KEY`]/`av.job.id=<JobSpec.job_id>` labels --
    /// production wiring passes an empty map; a docker-gated test passes its own `av.test`/
    /// `av.test.run_id` labels so this executor's own containers are found and swept by the same
    /// `docker ps -a --filter label=av.test` every other docker-gated test in this workspace
    /// uses.
    pub fn new(scratch_root: impl Into<PathBuf>, extra_labels: BTreeMap<String, String>) -> Self {
        Self { scratch_root: scratch_root.into(), extra_labels }
    }
}

/// RAII guard: on drop, best-effort `docker rm -f`s the named container (if one was ever
/// created -- `container_name` starts `None` and is set the moment `docker run` is about to be
/// invoked, so even a container that started but whose `docker run` invocation itself later
/// errored is still removed) and removes the whole per-run scratch directory. Guarantees
/// cleanup on every exit path out of [`ContainerExecutor::execute`] -- success, a typed
/// [`pb::JobFailure`], or an early `?` -- without every one of that function's own return points
/// needing to remember to call it (the same guarantee `av_lockstep::docker::ManagedContainer`'s
/// own `Drop` gives its container; see this module's own doc, "Why this crate shells out to
/// `docker` itself").
struct Cleanup {
    scratch: PathBuf,
    container_name: Option<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(name) = &self.container_name {
            let _ = Command::new("docker").args(["rm", "-f", name]).output();
        }
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

/// How much of `docker run`'s own stderr is kept (its tail): it matters only when the container
/// never started (the daemon's error text); a started container's output is read from its log.
const RUN_STDERR_KEEP: usize = 16 * 1024;

/// What one job run produced, beyond the [`Executor`] result -- the container executor's
/// counterpart of [`crate::k8s::RunReport`].
#[derive(Debug)]
pub struct ContainerReport {
    pub job_id: String,
    /// The container's name, once a `docker run` was attempted (`None`: the job never got that far).
    pub container: Option<String>,
    /// What [`Executor::execute`] returns. A failure's `detail` ends with the log tail (or why
    /// there is no log).
    pub result: Result<Vec<JobOutput>, pb::JobFailure>,
    /// The container's stdout and stderr, merged and capped (see this module's doc).
    pub log: Option<LogCapture>,
    /// Why there is no `log`; set exactly when `log` is `None`.
    pub log_error: Option<String>,
}

/// What `run_job` learned about the log, written as it goes so a failure can quote it.
#[derive(Default)]
struct LogSlot {
    container: Option<String>,
    log: Option<LogCapture>,
    log_error: Option<String>,
}

impl LogSlot {
    /// The text a failure's detail ends with: the log tail, or why there is no log.
    fn suffix(&self) -> String {
        failure_log_suffix(self.log.as_ref().map(|l| l.text.as_str()), self.log_error.as_deref())
    }
}

impl Executor for ContainerExecutor {
    fn execute(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
        self.run(spec, inputs).result
    }
}

impl ContainerExecutor {
    /// Runs one job like [`Executor::execute`] and also returns the container's log, captured
    /// (capped, truncation marked) before the container is removed -- on success and on failure.
    pub fn run(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> ContainerReport {
        let mut slot = LogSlot::default();
        let result = self.run_job(spec, inputs, &mut slot);
        let log_error = match (&slot.log, slot.log_error) {
            (Some(_), _) => None,
            (None, Some(e)) => Some(e),
            (None, None) => Some("no container was created for this job (it failed before `docker run`)".to_string()),
        };
        ContainerReport { job_id: spec.job_id.clone(), container: slot.container, result, log: slot.log, log_error }
    }

    fn run_job(&self, spec: &pb::JobSpec, inputs: &[JobInput], slot: &mut LogSlot) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let image_ref = digest_gate(&spec.container_image, &spec.container_image_digest)?;

        let run_id = format!("{}-{}", sanitize_for_container_name(&spec.job_id), unique_suffix());
        let scratch = self.scratch_root.join(format!("av-job-{run_id}"));
        let in_dir = scratch.join("in");
        let out_dir = scratch.join("out");
        let mut cleanup = Cleanup { scratch: scratch.clone(), container_name: None };

        fs::create_dir_all(&in_dir).map_err(|e| start_failed(format!("creating input scratch dir {in_dir:?}: {e}")))?;
        fs::create_dir_all(&out_dir).map_err(|e| start_failed(format!("creating output scratch dir {out_dir:?}: {e}")))?;
        // 0o777: the fixed non-root CONTAINER_UID_GID this executor always runs as must be able
        // to write here regardless of which host user created it -- see this module's own doc,
        // "How inputs/outputs cross the container boundary".
        fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o777)).map_err(|e| start_failed(format!("chmod output scratch dir {out_dir:?}: {e}")))?;

        for (i, input) in inputs.iter().enumerate() {
            let path = in_dir.join(format!("{i:03}"));
            fs::write(&path, &input.bytes).map_err(|e| start_failed(format!("writing input file {path:?}: {e}")))?;
        }

        let container_name = format!("av-job-{run_id}");
        // Set BEFORE `docker run` runs -- Cleanup::drop must attempt removal even if the
        // container was actually created but this function never gets to see a clean exit
        // status for it (this module's own doc on Cleanup).
        cleanup.container_name = Some(container_name.clone());

        let mut args: Vec<String> = vec!["run".to_string(), "--name".to_string(), container_name.clone(), "--network".to_string(), "none".to_string()];
        args.extend(HARDENING_RUN_FLAGS.iter().map(|f| (*f).to_string()));
        args.push("--user".to_string());
        args.push(CONTAINER_UID_GID.to_string());
        args.push("-v".to_string());
        args.push(format!("{}:{CONTAINER_INPUT_DIR}:ro", in_dir.display()));
        args.push("-v".to_string());
        args.push(format!("{}:{CONTAINER_OUTPUT_DIR}", out_dir.display()));
        args.push("--label".to_string());
        args.push(format!("{JOB_LABEL_KEY}={JOB_LABEL_VALUE}"));
        args.push("--label".to_string());
        args.push(format!("av.job.id={}", spec.job_id));
        for (k, v) in &self.extra_labels {
            args.push("--label".to_string());
            args.push(format!("{k}={v}"));
        }
        args.push(image_ref);
        args.extend(spec.command.iter().cloned());

        // stdout is discarded and stderr kept only as a bounded tail: the record of what the job
        // printed is `docker logs`, below, capped (this module's doc, "The job's log").
        let mut child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| start_failed(format!("could not launch `docker run`: {e}")))?;
        let run_stderr = match child.stderr.take() {
            Some(pipe) => read_tail(pipe, RUN_STDERR_KEEP),
            None => Vec::new(),
        };
        let status = child.wait().map_err(|e| start_failed(format!("waiting for `docker run`: {e}")))?;

        // Whatever happened, read the log now: the container still exists (Cleanup removes it only
        // when this function returns).
        slot.container = Some(container_name.clone());
        match capture_log(&container_name, DEFAULT_LOG_CAP_BYTES, DEFAULT_LOG_MAX_LINES) {
            Ok(log) => slot.log = Some(log),
            Err(e) => slot.log_error = Some(e),
        }

        if !status.success() {
            let code = status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&run_stderr).trim().to_string();
            // See this module's own doc, "Failure-kind reuse": 125/126/127 are Docker's own
            // reserved exit codes for "the container never really started" (daemon-level
            // failure / command not executable / command not found) -- a genuinely new failure
            // mode this task's own brief names ("a container that could not start"), reusing
            // EXECUTOR_START_FAILED rather than inventing a kind for it. Any other non-zero
            // code is `docker run` (foreground, never `-d`) faithfully propagating the
            // CONTAINED command's own real exit status.
            if matches!(code, 125..=127) {
                return Err(pb::JobFailure {
                    kind: pb::JobFailureKind::ExecutorStartFailed as i32,
                    detail: format!("container for job {:?} could not start (docker run exit {code}): {stderr}{}", spec.job_id, slot.suffix()),
                    exit_code: code,
                });
            }
            // `docker run`'s stderr here is the job's own stderr; it is quoted only when the log
            // could not be read (otherwise the log tail, stdout and stderr both, says it).
            let own = if slot.log.is_none() && !stderr.is_empty() { format!(": {stderr}") } else { String::new() };
            return Err(pb::JobFailure {
                kind: pb::JobFailureKind::NonzeroExit as i32,
                detail: format!("container for job {:?} exited with status {code}{own}{}", spec.job_id, slot.suffix()),
                exit_code: code,
            });
        }

        let rejected = |detail: String| {
            let mut f = output_rejected(detail);
            f.detail.push_str(&slot.suffix());
            f
        };
        let mut out_paths: Vec<PathBuf> = fs::read_dir(&out_dir)
            .map_err(|e| rejected(format!("reading output scratch dir {out_dir:?}: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| p.is_file())
            .collect();
        out_paths.sort();

        let mut outputs = Vec::with_capacity(out_paths.len());
        for path in out_paths {
            let bytes = fs::read(&path).map_err(|e| rejected(format!("reading output file {path:?}: {e}")))?;
            outputs.push(JobOutput { bytes, media_type: "application/octet-stream".to_string(), manifest: false });
        }

        // `cleanup` (and its Cleanup::drop, which removes the container and the scratch
        // directory) runs implicitly at this function's own scope end -- after `outputs` has
        // already been read into memory, as part of returning it below.
        Ok(outputs)
    }
}

/// The end of a failure's `detail`: the k8s executor's `; log tail: "..."` (an empty log adds
/// nothing), or `; no log: <why>` so a missing log is never mistaken for an empty one.
fn failure_log_suffix(log: Option<&str>, log_error: Option<&str>) -> String {
    match (log, log_error) {
        (Some(text), _) if !text.is_empty() => format!("; log tail: {:?}", log_tail(text)),
        (Some(_), _) => "; the job printed nothing".to_string(),
        (None, Some(why)) => format!("; no log: {why}"),
        (None, None) => String::new(),
    }
}

/// Reads `r` to its end, keeping only the last `keep` bytes.
fn read_tail(mut r: impl Read, keep: usize) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.extend_from_slice(&buf[..n]);
                if kept.len() > 2 * keep {
                    kept.drain(..kept.len() - keep);
                }
            }
        }
    }
    if kept.len() > keep {
        kept.drain(..kept.len() - keep);
    }
    kept
}

/// Reads `r` to its end (so the writer never blocks), keeping only the first `keep` bytes.
fn read_head(mut r: impl Read, keep: usize) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = keep.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    kept
}

/// Width of the fixed-format RFC 3339 timestamp `docker logs --timestamps` prefixes each line with
/// (`2026-10-18T02:18:48.310935567Z`), then one space.
const DOCKER_TS_LEN: usize = 30;

/// Reads the container's log (`docker logs --timestamps --tail <max_lines + 1>`) and shapes it with
/// the k8s executor's own capping. `Err` says why there is no log.
fn capture_log(container: &str, cap_bytes: usize, max_lines: usize) -> Result<LogCapture, String> {
    let mut child = Command::new("docker")
        .args(["logs", "--timestamps", "--tail", &(max_lines + 1).to_string(), container])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not launch `docker logs {container}`: {e}"))?;
    // Per stream, enough for cap + 1 bytes of text plus the timestamp on each of the lines kept.
    let keep = cap_bytes + 1 + (max_lines + 1) * (DOCKER_TS_LEN + 1);
    let (out_pipe, err_pipe) = (child.stdout.take(), child.stderr.take());
    // The two pipes are read at once: one full pipe would stall docker while this waits on the other.
    let (stdout, stderr) = std::thread::scope(|s| {
        let out = s.spawn(|| out_pipe.map(|p| read_head(p, keep)).unwrap_or_default());
        let err = s.spawn(|| err_pipe.map(|p| read_head(p, keep)).unwrap_or_default());
        (out.join().unwrap_or_default(), err.join().unwrap_or_default())
    });
    let status = child.wait().map_err(|e| format!("waiting for `docker logs {container}`: {e}"))?;
    if !status.success() {
        // On failure the daemon's error is on stderr (a success puts the container's own stderr there).
        return Err(format!("`docker logs {container}` failed ({status}): {}", String::from_utf8_lossy(&stderr).trim()));
    }
    Ok(shape_log(container, &merge_streams(&stdout, &stderr), cap_bytes, max_lines))
}

/// Merges `docker logs --timestamps` output of the two streams into one text: stably ordered by the
/// daemon's timestamp (stdout first on a tie), the timestamp dropped, every line newline-terminated.
fn merge_streams(stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
    let mut lines: Vec<(&[u8], &[u8])> = Vec::new();
    for stream in [stdout, stderr] {
        let mut last_ts: &[u8] = &[];
        let mut chunks: Vec<&[u8]> = stream.split(|b| *b == b'\n').collect();
        if chunks.last().is_some_and(|c| c.is_empty()) {
            chunks.pop();
        }
        for chunk in chunks {
            if chunk.len() > DOCKER_TS_LEN && chunk[DOCKER_TS_LEN] == b' ' && chunk[4] == b'-' && chunk[DOCKER_TS_LEN - 1] == b'Z' {
                last_ts = &chunk[..DOCKER_TS_LEN];
                lines.push((last_ts, &chunk[DOCKER_TS_LEN + 1..]));
            } else {
                // Only a line cut short by the read bound has no timestamp: it follows its stream's last.
                lines.push((last_ts, chunk));
            }
        }
    }
    lines.sort_by(|a, b| a.0.cmp(b.0));
    let mut merged = Vec::with_capacity(lines.iter().map(|(_, t)| t.len() + 1).sum());
    for (_, text) in lines {
        merged.extend_from_slice(text);
        merged.push(b'\n');
    }
    merged
}

fn start_failed(detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::ExecutorStartFailed as i32, detail: format!("CONTAINER executor: {detail}"), exit_code: 0 }
}

fn output_rejected(detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::OutputRejected as i32, detail: format!("CONTAINER executor: {detail}"), exit_code: 0 }
}

/// Resolves and verifies `image` (a bare reference, e.g. `"alpine"`, never itself carrying a
/// digest) against `want_digest` (`"sha256:..."`, `JobSpec.container_image_digest`) by inspecting
/// `<image>@<want_digest>` -- see this module's own doc, "Never pulls; the digest gate", for the
/// full account. Returns that same digest-pinned reference on success, so the actual `docker run`
/// is always given a content-addressed reference, never the bare name.
pub(crate) fn digest_gate(image: &str, want_digest: &str) -> Result<String, pb::JobFailure> {
    if image.is_empty() || want_digest.is_empty() {
        return Err(start_failed("JobSpec.container_image / container_image_digest is empty -- nothing to run".to_string()));
    }
    let pinned = format!("{image}@{want_digest}");
    match inspect_image_id(&pinned) {
        Ok(actual) if actual == want_digest => Ok(pinned),
        Ok(actual) => Err(unavailable(format!(
            "image {pinned:?} is present locally as {actual}, but JobSpec.container_image_digest names {want_digest} -- \
             refused rather than trusted (question 212(a))"
        ))),
        Err(pinned_err) => {
            // Diagnostics only (never the trust path): say what the bare name resolves to, so
            // "absent entirely" and "present, but not at the requested digest" stay distinct.
            match inspect_image_id(image) {
                Ok(actual) => Err(unavailable(format!(
                    "image {image:?} is present locally as {actual}, but JobSpec.container_image_digest names {want_digest}, \
                     which is not present locally -- refused rather than trusted (question 212(a)); this runner never runs \
                     `docker pull` (question 154)"
                ))),
                Err(_) => Err(unavailable(format!(
                    "image {pinned:?} is not present locally -- this runner never runs `docker pull` (question 154); \
                     pull it once, out of band, by digest, then retry: {pinned_err}"
                ))),
            }
        }
    }
}

/// `docker image inspect <reference> --format {{.Id}}`: the local id, or the failure's text
/// (daemon unreachable, or the reference not present).
fn inspect_image_id(reference: &str) -> Result<String, String> {
    let output = Command::new("docker")
        .args(["image", "inspect", reference, "--format", "{{.Id}}"])
        .output()
        .map_err(|e| format!("could not launch docker to inspect image {reference:?}: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn unavailable(detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::ExecutorUnavailable as i32, detail: format!("CONTAINER executor: {detail}"), exit_code: 0 }
}

/// A per-call unique suffix for a scratch directory / container name -- pid + wall-clock
/// nanoseconds, mirroring `av_lockstep::docker::test_run_id`'s own shape (not depended on --
/// see this module's own doc, "Why this crate shells out to `docker` itself").
fn unique_suffix() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{pid}-{nanos}")
}

/// `docker run --name` accepts only `[a-zA-Z0-9][a-zA-Z0-9_.-]*` -- `JobSpec.job_id` is
/// caller-supplied and may contain anything. Replaces every other byte with `_`; an
/// all-replaced (or empty) `job_id` still yields a valid, if unhelpful, name rather than a
/// `docker run` argument error a caller would have to decode.
fn sanitize_for_container_name(job_id: &str) -> String {
    let cleaned: String = job_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
    if cleaned.is_empty() {
        "job".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // Hardening flags: reused, not reinvented -- kept from drifting apart from
    // altavista/container_hardening.py::HARDENING_RUN_FLAGS by actually parsing that file's own
    // source text at test time (question: "if you cannot reuse it directly from Rust, state in
    // a doc comment where the list came from and add a test that the two agree"). No Docker
    // needed -- this is a plain text comparison, always runs.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn hardening_run_flags_matches_the_python_declaration_in_container_hardening_py() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../altavista/container_hardening.py");
        let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
        let marker = "HARDENING_RUN_FLAGS: list[str] = [";
        let start = text.find(marker).unwrap_or_else(|| panic!("could not find {marker:?} in {path:?}"));
        let after_marker = &text[start + marker.len()..];
        let end = after_marker.find(']').unwrap_or_else(|| panic!("no closing ']' for HARDENING_RUN_FLAGS in {path:?}"));
        let list_body = &after_marker[..end];
        let python_flags: Vec<String> = list_body
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_matches(|c| c == '"' || c == '\'').to_string())
            .collect();
        let rust_flags: Vec<String> = HARDENING_RUN_FLAGS.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(
            rust_flags, python_flags,
            "crate::container::HARDENING_RUN_FLAGS (Rust) must equal altavista.container_hardening.HARDENING_RUN_FLAGS \
             (Python) element for element -- one drifted from the other; there is exactly one hardening posture, declared \
             twice only because no mechanism imports the Python list into this crate's production code (see this module's \
             own doc comment)"
        );
    }

    // ---------------------------------------------------------------------------------------
    // The log: merging, capping and the failure text. No Docker needed.
    // ---------------------------------------------------------------------------------------

    fn ts(n: u32) -> String {
        format!("2026-10-18T02:18:48.{n:09}Z")
    }

    #[test]
    fn the_two_streams_merge_by_timestamp_with_stdout_first_on_a_tie() {
        let out = format!("{} o1\n{} o2\n{} o3 no newline", ts(1), ts(3), ts(3));
        let err = format!("{} e1\n{} e2\n", ts(2), ts(3));
        let merged = String::from_utf8(merge_streams(out.as_bytes(), err.as_bytes())).unwrap();
        // o2 and o3 and e2 share a stamp: stdout's two first, in their own order, then stderr's.
        assert_eq!(merged, "o1\ne1\no2\no3 no newline\ne2\n");
    }

    #[test]
    fn merging_keeps_empty_lines_and_a_line_cut_short_follows_its_own_stream() {
        let out = format!("{} \n{} a\n{}", ts(1), ts(2), "2026-10-18T0");
        let merged = String::from_utf8(merge_streams(out.as_bytes(), b"")).unwrap();
        assert_eq!(merged, "\na\n2026-10-18T0\n");
        assert_eq!(merge_streams(b"", b""), b"");
    }

    #[test]
    fn the_container_log_is_capped_exactly_as_the_k8s_log_is() {
        // The same function, so the same marker lines; here the container's name is the source.
        let long = vec![b'x'; DEFAULT_LOG_CAP_BYTES + 100];
        let big = shape_log("c", &long, DEFAULT_LOG_CAP_BYTES, DEFAULT_LOG_MAX_LINES);
        assert!(big.truncated && big.text.contains(&format!("[av: log truncated at {DEFAULT_LOG_CAP_BYTES} bytes")), "{}", &big.text[big.text.len() - 80..]);
        let many: String = (0..DEFAULT_LOG_MAX_LINES + 5).map(|i| format!("{i}\n")).collect();
        let kept = shape_log("c", many.as_bytes(), DEFAULT_LOG_CAP_BYTES, DEFAULT_LOG_MAX_LINES);
        assert!(kept.truncated && kept.text.starts_with(&format!("[av: log truncated: only the last {DEFAULT_LOG_MAX_LINES} lines kept]\n5\n")), "{}", &kept.text[..80]);
    }

    #[test]
    fn a_failures_detail_carries_the_log_tail_or_says_why_there_is_none() {
        let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let with_log = failure_log_suffix(Some(&text), None);
        assert!(with_log.starts_with("; log tail: \"line 25\\n") && with_log.ends_with("line 39\""), "{with_log}");
        assert_eq!(failure_log_suffix(Some(""), None), "; the job printed nothing");
        assert_eq!(failure_log_suffix(None, Some("No such container")), "; no log: No such container");
        assert_eq!(failure_log_suffix(None, None), "");
    }

    #[test]
    fn a_job_that_never_reached_docker_run_reports_no_log_with_a_reason() {
        let exec = ContainerExecutor::new(std::env::temp_dir(), BTreeMap::new());
        // Empty image: refused by digest_gate before any docker call.
        let report = exec.run(&pb::JobSpec { job_id: "j".into(), ..Default::default() }, &[]);
        assert!(report.result.is_err() && report.container.is_none() && report.log.is_none(), "{report:?}");
        assert!(report.log_error.as_deref().is_some_and(|e| e.contains("no container was created")), "{report:?}");
    }

    #[test]
    fn the_bounded_readers_keep_the_tail_and_the_head() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(read_tail(&data[..], 1000), data[data.len() - 1000..]);
        assert_eq!(read_head(&data[..], 1000), data[..1000]);
        assert_eq!(read_tail(&data[..10], 1000), data[..10]);
    }

    #[test]
    fn sanitize_for_container_name_replaces_unsafe_characters() {
        assert_eq!(sanitize_for_container_name("job-1"), "job-1");
        assert_eq!(sanitize_for_container_name("job/with spaces:and:colons"), "job_with_spaces_and_colons");
        assert_eq!(sanitize_for_container_name(""), "job");
        assert_eq!(sanitize_for_container_name("!!!"), "___");
    }

    /// Question 254 item 1 (audit: unreachable by a hostile `job_id`): the job id reaches the
    /// scratch directory only as `av-job-<sanitized id>-<pid>-<nanos>`, and the sanitized id holds
    /// only `[A-Za-z0-9._-]`, so whatever the id is the name is exactly one `Normal` path
    /// component (never `..`, never a separator, never absolute), directly under the scratch root.
    #[test]
    fn a_hostile_job_id_can_only_ever_be_one_scratch_component() {
        for hostile in ["../x", "..", ".", "/abs/path", "a/b", "a\\b", "a\0b", "%2e%2e%2fx", "..%2fx", "", "x/../../y"] {
            let name = format!("av-job-{}-{}", sanitize_for_container_name(hostile), unique_suffix());
            let scratch = std::path::Path::new("/scratch-root").join(&name);
            assert_eq!(scratch.parent(), Some(std::path::Path::new("/scratch-root")), "{hostile:?} -> {scratch:?}");
            let comps: Vec<_> = std::path::Path::new(&name).components().collect();
            assert!(comps.len() == 1 && matches!(comps[0], std::path::Component::Normal(_)), "{hostile:?} -> {name:?}");
            assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')), "{name:?}");
        }
    }

    // ---------------------------------------------------------------------------------------
    // digest_gate's own empty-field precondition needs no Docker at all -- it returns before
    // ever calling `docker image inspect`.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn digest_gate_refuses_an_empty_image_or_digest_without_touching_docker() {
        let err = digest_gate("", "sha256:aaaa").unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::ExecutorStartFailed as i32);
        assert!(err.detail.contains("is empty"), "{:?}", err.detail);

        let err = digest_gate("alpine:latest", "").unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::ExecutorStartFailed as i32);
        assert!(err.detail.contains("is empty"), "{:?}", err.detail);
    }

    // A digest reference that cannot exist is refused as EXECUTOR_UNAVAILABLE (never a pull, never
    // a run) whether or not a Docker daemon is reachable: with one, `docker image inspect` reports
    // "No such image"; without one, the launch/connection failure takes the same refusal path.
    #[test]
    fn digest_gate_refuses_a_digest_reference_that_cannot_exist_as_executor_unavailable() {
        let wrong = format!("sha256:{}", "0".repeat(64));
        let err = digest_gate("av-jobs-no-such-image-for-digest-gate-test", &wrong).unwrap_err();
        assert_eq!(err.kind, pb::JobFailureKind::ExecutorUnavailable as i32, "{err:?}");
        assert!(err.detail.contains("not present locally"), "{:?}", err.detail);
    }
}

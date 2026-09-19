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
//! [`digest_gate`] runs `docker image inspect <JobSpec.container_image> --format {{.Id}}` --
//! resolving only an image already present on this host, exactly like
//! `av_lockstep::docker::local_image_id` -- and compares the result against
//! `JobSpec.container_image_digest`: the job's own request plays the role
//! `av_lockstep::docker::recorded_digest_gate`'s own `recorded_id` parameter plays for a test
//! (question 212(a)'s "a test trusts an image only after comparing the running image to a
//! recorded digest, and refuses rather than trusts on a mismatch" -- applied at RUN time, with
//! the caller's own requested digest as the thing trusted only after comparison). Three
//! distinct, typed outcomes, none of them a `docker pull`: the image absent entirely, the image
//! present under a DIFFERENT id than requested (refused, never silently substituted), or a
//! match -- in which case `docker run` is given the fully digest-pinned reference
//! (`"<image>@<digest>"`), never the bare name, so the daemon itself enforces content
//! addressing for the actual run.
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

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use av_cdm::pb;

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

impl Executor for ContainerExecutor {
    fn execute(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
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

        let output = Command::new("docker").args(&args).output().map_err(|e| start_failed(format!("could not launch `docker run`: {e}")))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
                    detail: format!("container for job {:?} could not start (docker run exit {code}): {stderr}", spec.job_id),
                    exit_code: code,
                });
            }
            return Err(pb::JobFailure {
                kind: pb::JobFailureKind::NonzeroExit as i32,
                detail: format!("container for job {:?} exited with status {code}: {stderr}", spec.job_id),
                exit_code: code,
            });
        }

        let mut out_paths: Vec<PathBuf> = fs::read_dir(&out_dir)
            .map_err(|e| output_rejected(format!("reading output scratch dir {out_dir:?}: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|p| p.is_file())
            .collect();
        out_paths.sort();

        let mut outputs = Vec::with_capacity(out_paths.len());
        for path in out_paths {
            let bytes = fs::read(&path).map_err(|e| output_rejected(format!("reading output file {path:?}: {e}")))?;
            outputs.push(JobOutput { bytes, media_type: "application/octet-stream".to_string(), manifest: false });
        }

        // `cleanup` (and its Cleanup::drop, which removes the container and the scratch
        // directory) runs implicitly at this function's own scope end -- after `outputs` has
        // already been read into memory, as part of returning it below.
        Ok(outputs)
    }
}

fn start_failed(detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::ExecutorStartFailed as i32, detail: format!("CONTAINER executor: {detail}"), exit_code: 0 }
}

fn output_rejected(detail: String) -> pb::JobFailure {
    pb::JobFailure { kind: pb::JobFailureKind::OutputRejected as i32, detail: format!("CONTAINER executor: {detail}"), exit_code: 0 }
}

/// Resolves and verifies `image` (a bare reference, e.g. `"alpine:latest"`, never itself
/// carrying a digest) against `want_digest` (`"sha256:..."`, `JobSpec.container_image_digest`)
/// -- see this module's own doc, "Never pulls; the digest gate", for the full account. Returns
/// the fully digest-pinned reference (`"<image>@<want_digest>"`) on success, so the actual
/// `docker run` is always given a content-addressed reference, never the bare name.
fn digest_gate(image: &str, want_digest: &str) -> Result<String, pb::JobFailure> {
    if image.is_empty() || want_digest.is_empty() {
        return Err(start_failed("JobSpec.container_image / container_image_digest is empty -- nothing to run".to_string()));
    }
    let output = Command::new("docker")
        .args(["image", "inspect", image, "--format", "{{.Id}}"])
        .output()
        .map_err(|e| unavailable(format!("could not launch docker to inspect image {image:?}: {e}")))?;
    if !output.status.success() {
        return Err(unavailable(format!(
            "image {image:?} is not present locally -- this runner never runs `docker pull` (question 154); \
             pull/build it once, out of band, then retry: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let actual = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if actual != want_digest {
        return Err(unavailable(format!(
            "image {image:?} is present locally as {actual}, but JobSpec.container_image_digest names {want_digest} -- \
             refused rather than trusted (question 212(a))"
        )));
    }
    Ok(format!("{image}@{want_digest}"))
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

    #[test]
    fn sanitize_for_container_name_replaces_unsafe_characters() {
        assert_eq!(sanitize_for_container_name("job-1"), "job-1");
        assert_eq!(sanitize_for_container_name("job/with spaces:and:colons"), "job_with_spaces_and_colons");
        assert_eq!(sanitize_for_container_name(""), "job");
        assert_eq!(sanitize_for_container_name("!!!"), "___");
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
}

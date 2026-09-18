//! The runner: drains a [`crate::queue::JobQueue`], fetching and hash-verifying each job's
//! inputs, dispatching to a registered [`Executor`] (by `JobSpec.kind`, except a
//! `JOB_EXECUTOR_KIND_CONTAINER` job, which always runs through [`Runner::
//! register_container_executor`]'s own single executor instead -- see that method's own doc),
//! and storing outputs through an [`ObjectSink`] -- see [`Runner::run_one`]'s own doc for the
//! fixed order of checks and why it never returns an `Err`.

use std::collections::HashMap;
use std::fmt;

use av_cdm::pb;

use crate::clock::Clock;
use crate::hash;
use crate::queue::JobQueue;

/// One already-fetched, already-hash-verified job input -- what [`Executor::execute`]
/// receives, in `JobSpec.inputs`' own order. An executor never sees an input whose bytes
/// have not already been checked against `asset.sha256` (see [`Runner::run_one`]'s own doc).
#[derive(Debug, Clone)]
pub struct JobInput {
    pub asset: pb::AssetRef,
    pub bytes: Vec<u8>,
}

/// One output an [`Executor`] hands back to [`Runner::run_one`], not yet stored anywhere --
/// [`ObjectSink::put`] is what turns it into an [`pb::AssetRef`].
#[derive(Debug, Clone)]
pub struct JobOutput {
    pub bytes: Vec<u8>,
    pub media_type: String,
    /// Whether this output is the job's tile-set (or other kind-specific) manifest --
    /// `JobCompletion.manifest_sha256` is set from whichever output has this `true`. At most
    /// one output of a run may set this (see [`Runner::run_one`]'s own doc).
    pub manifest: bool,
}

/// Runs one job's kind-specific work, given its already-verified inputs. Registered into a
/// [`Runner`] by `JobSpec.kind` (e.g. `"tiler"`, H3b -- not implemented by this crate).
pub trait Executor: fmt::Debug {
    /// Runs one job. `inputs` are the already-fetched, already-hash-verified input bytes, in
    /// the `JobSpec`'s own input order.
    fn execute(&self, spec: &pb::JobSpec, inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure>;

    /// Whether this executor's job kind ever produces a manifest output
    /// (`JobOutput::manifest == true`). Defaults to `false` -- an executor that never emits
    /// a manifest need not override this. [`Runner::run_one`] refuses
    /// (`JOB_FAILURE_KIND_OUTPUT_REJECTED`) a manifest output from an executor that declares
    /// it produces none, on top of refusing more than one manifest output regardless of this
    /// declaration.
    fn declares_manifest(&self) -> bool {
        false
    }

    /// Like [`Executor::execute`], but given direct access to `sink` (the same
    /// [`ObjectSink`] [`Runner::run_one_streaming`] would otherwise store this call's
    /// returned outputs through) and `label` (the job's own, already-ladder-checked label),
    /// so an executor whose job produces many large outputs can store each one as it is
    /// produced instead of returning every output's bytes in one `Vec` for the caller to
    /// store afterward -- see `crate::tiler::TilerExecutor::run_imagery_streaming`'s own doc
    /// for why that matters at a multi-gigabyte tile set.
    ///
    /// **The default implementation delegates to [`Executor::execute`] and ignores `sink`/
    /// `label` entirely** -- exactly today's buffered behaviour, byte-for-byte unchanged, for
    /// every executor (this crate's own [`ProcessExecutor`] included) that does not override
    /// this method. [`Runner::run_one`] never calls this method at all; only
    /// [`Runner::run_one_streaming`] does.
    fn execute_streaming(&self, spec: &pb::JobSpec, inputs: &[JobInput], sink: &dyn ObjectSink, label: &pb::Label) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let _ = (sink, label);
        self.execute(spec, inputs)
    }
}

/// How the runner gets an input's bytes, by its [`pb::AssetRef`]. `av-jobs` must not depend
/// on `av-store` (see `crate`'s own crate doc and `Cargo.toml`'s "Deliberately NOT a
/// dependency" block) -- task 3b wires a real store-backed implementation from a crate that
/// depends on both `av-jobs` and `av-store`. [`MemoryObjectSource`] is this crate's own test
/// double.
pub trait ObjectSource: fmt::Debug {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure>;
}

/// How the runner stores an [`Executor`]'s output bytes as a new [`pb::AssetRef`]. Same
/// dependency reasoning as [`ObjectSource`] -- [`MemoryObjectSink`] is this crate's own test
/// double, content-addressing exactly the way `av_store::keys::object_key` does (see that
/// type's own doc comment).
pub trait ObjectSink: fmt::Debug {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure>;
}

/// An in-memory [`ObjectSource`] for tests: bytes registered by [`MemoryObjectSource::insert`]
/// under an `AssetRef.uri`, fetched back by the same `uri`. Deliberately does **not** verify
/// a fetched object's hash against anything -- that is [`Runner::run_one`]'s own job (so a
/// test can register bytes under an `AssetRef` whose `sha256` deliberately does not match,
/// to exercise `JOB_FAILURE_KIND_INPUT_HASH_MISMATCH`).
#[derive(Debug, Default)]
pub struct MemoryObjectSource {
    objects: std::sync::Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryObjectSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, uri: impl Into<String>, bytes: Vec<u8>) {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).insert(uri.into(), bytes);
    }
}

impl ObjectSource for MemoryObjectSource {
    fn fetch(&self, asset: &pb::AssetRef) -> Result<Vec<u8>, pb::JobFailure> {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).get(&asset.uri).cloned().ok_or_else(|| pb::JobFailure {
            kind: pb::JobFailureKind::InputMissing as i32,
            detail: format!("no object registered for uri {:?}", asset.uri),
            exit_code: 0,
        })
    }
}

/// The content-addressed key layout `av_store::keys::object_key` implements --
/// `"<prefix>/<hex[0..2]>/<hex[2..4]>/<hex>"` -- reimplemented here rather than depended on
/// (`av-jobs` must not depend on `av-store`), matched byte for byte against that function's
/// own doc comment: sharding by the first two hex-digit pairs so no one prefix accumulates
/// every object under one flat key. `sha256_hex` is always this module's own
/// [`crate::hash::hex_encode`] output (exactly 64 lowercase hex characters), so unlike
/// `object_key` this private helper does not re-validate it.
///
/// `pub` (not `pub(crate)`) so [`crate::tiler::TilerExecutor`] can compute the exact same
/// key [`MemoryObjectSink::put`] below will independently derive for the same bytes --
/// **one shared helper**, never a second, independently-written key scheme. See
/// `TileSetManifest`'s own doc comment (`heavy.proto`) for why a tiler executor needs to
/// predict a sink's own key layout at all (the manifest-vs-sink-ordering constraint), and
/// for the caveat that creates: the caller wiring up a `Runner` must construct the
/// `ObjectSink` and the tiler executor with the same `prefix`. Widened from `pub(crate)` to
/// `pub` by task 3c (`crates/av-jobs/tests/store_tiler.rs`): that test asserts, for a real
/// hash, that this function and [`av_store::keys::object_key`] (a crate this one must not
/// depend on in `[dependencies]` -- `av_store` is a dev-dependency of the test binary only)
/// agree byte for byte, so the mirrored layout this doc comment already claimed is checked
/// mechanically rather than merely asserted in prose. No caller inside this crate is
/// affected by the wider visibility.
pub fn content_addressed_key(prefix: &str, sha256_hex: &str) -> String {
    format!("{prefix}/{}/{}/{sha256_hex}", &sha256_hex[0..2], &sha256_hex[2..4])
}

/// An in-memory [`ObjectSink`] for tests, content-addressed the way [`content_addressed_key`]
/// (mirroring `av_store::keys::object_key`) lays out keys, under a fixed `prefix`. Builds a
/// real [`pb::AssetRef`] with `sha256`/`size_bytes`/`media_type`/`label` filled from what was
/// actually stored -- the same fields `av_store::claim_check::asset_ref_for` fills, never
/// trusting a caller-supplied hash.
#[derive(Debug)]
pub struct MemoryObjectSink {
    prefix: String,
    objects: std::sync::Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryObjectSink {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self { prefix: prefix.into(), objects: std::sync::Mutex::new(HashMap::new()) }
    }

    /// The bytes stored under `sha256_hex`, if any -- a test convenience to assert what a
    /// job actually wrote.
    pub fn get(&self, sha256_hex: &str) -> Option<Vec<u8>> {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).get(sha256_hex).cloned()
    }
}

impl ObjectSink for MemoryObjectSink {
    fn put(&self, bytes: &[u8], media_type: &str, label: &pb::Label) -> Result<pb::AssetRef, pb::JobFailure> {
        let digest = openssl::sha::sha256(bytes);
        let sha256_hex = hash::hex_encode(&digest);
        let key = content_addressed_key(&self.prefix, &sha256_hex);
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).insert(sha256_hex.clone(), bytes.to_vec());
        Ok(pb::AssetRef {
            uri: format!("memory://{key}"),
            sha256: sha256_hex,
            size_bytes: bytes.len() as u64,
            media_type: media_type.to_string(),
            label: Some(label.clone()),
            spatial_extent: None,
            temporal_extent: None,
            provenance: None,
            attributes: Default::default(),
        })
    }
}

/// `JOB_EXECUTOR_KIND_PROCESS`: runs `JobSpec.command` as a local subprocess
/// (`command[0]` the executable, the rest its argv), refusing a spawn failure as
/// `JOB_FAILURE_KIND_EXECUTOR_START_FAILED` and a non-zero exit as
/// `JOB_FAILURE_KIND_NONZERO_EXIT` with the real exit code. Its one output is the process's
/// captured stdout, never a manifest (`declares_manifest` stays the trait's own `false`
/// default) -- H3b's tiler is a different, kind-specific `Executor` that this crate does not
/// implement.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessExecutor;

impl Executor for ProcessExecutor {
    fn execute(&self, spec: &pb::JobSpec, _inputs: &[JobInput]) -> Result<Vec<JobOutput>, pb::JobFailure> {
        let Some((program, args)) = spec.command.split_first() else {
            return Err(pb::JobFailure {
                kind: pb::JobFailureKind::ExecutorStartFailed as i32,
                detail: "PROCESS executor: JobSpec.command is empty -- nothing to spawn".to_string(),
                exit_code: 0,
            });
        };
        let output = std::process::Command::new(program).args(args).output().map_err(|e| pb::JobFailure {
            kind: pb::JobFailureKind::ExecutorStartFailed as i32,
            detail: format!("failed to spawn {program:?}: {e}"),
            exit_code: 0,
        })?;
        if !output.status.success() {
            return Err(pb::JobFailure {
                kind: pb::JobFailureKind::NonzeroExit as i32,
                detail: format!("{program:?} exited with status {}", output.status),
                exit_code: output.status.code().unwrap_or(-1),
            });
        }
        Ok(vec![JobOutput { bytes: output.stdout, media_type: "application/octet-stream".to_string(), manifest: false }])
    }
}

/// Drains a [`JobQueue`], running each pending job through a registered [`Executor`] and
/// appending its [`pb::JobCompletion`] to the log.
#[derive(Debug)]
pub struct Runner<'a> {
    queue: JobQueue,
    executors: HashMap<String, Box<dyn Executor>>,
    /// The single [`Executor`] every `JOB_EXECUTOR_KIND_CONTAINER` job runs through, regardless
    /// of `spec.kind` -- see [`Runner::register_container_executor`]'s own doc and
    /// [`Runner::execute_spec`]'s Step 4 for why this is a separate slot from `executors` above,
    /// not another entry keyed by some reserved `kind` string. `None` until a caller registers
    /// one (production wiring, or a test, constructs a `crate::container::ContainerExecutor`
    /// and calls `register_container_executor`); a `Runner` that never does refuses every
    /// CONTAINER job as `EXECUTOR_UNAVAILABLE`, exactly as every `Runner` always did before this
    /// round.
    container_executor: Option<Box<dyn Executor>>,
    source: Box<dyn ObjectSource>,
    sink: Box<dyn ObjectSink>,
    ladder: av_label::ClearanceLadder,
    clock: &'a dyn Clock,
}

impl<'a> Runner<'a> {
    pub fn new(queue: JobQueue, source: Box<dyn ObjectSource>, sink: Box<dyn ObjectSink>, ladder: av_label::ClearanceLadder, clock: &'a dyn Clock) -> Self {
        Self { queue, executors: HashMap::new(), container_executor: None, source, sink, ladder, clock }
    }

    /// Registers `executor` to run every `JobSpec` whose `kind` equals `kind`. A second
    /// registration under the same `kind` replaces the first (this round has no caller that
    /// needs to detect that as an error; the tiler, H3b, registers exactly once at startup).
    /// **Never consulted for a `JOB_EXECUTOR_KIND_CONTAINER` job** -- see
    /// [`Runner::register_container_executor`]'s own doc.
    pub fn register_executor(&mut self, kind: impl Into<String>, executor: Box<dyn Executor>) {
        self.executors.insert(kind.into(), executor);
    }

    /// Registers the one [`Executor`] every `JOB_EXECUTOR_KIND_CONTAINER` job runs through --
    /// in production, a `crate::container::ContainerExecutor`. Deliberately a single slot, not
    /// keyed by `spec.kind` the way [`Runner::register_executor`]'s registry is: a CONTAINER
    /// job's `kind` still must name a registered [`Executor`] in that registry too
    /// ([`Runner::execute_spec`]'s Step 1 runs unconditionally), but that registered `Executor`
    /// is never the one that actually runs the job -- only this one is, for every CONTAINER job
    /// regardless of `kind`
    /// (`crates/av-jobs/tests/runner.rs::executor_unavailable_for_the_container_kind_is_recorded_on_the_log`
    /// proves exactly this: a real, working `ProcessExecutor` registered under the job's own
    /// `kind` is never reached). A second call replaces the first, same convention as
    /// `register_executor`.
    pub fn register_container_executor(&mut self, executor: Box<dyn Executor>) {
        self.container_executor = Some(executor);
    }

    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    /// Runs `spec` to completion and appends the resulting [`pb::JobCompletion`] to the
    /// queue's log, returning it. **Never returns an `Err`**: every failure path below
    /// produces a `JobCompletion { ok: false, failure: Some(..) }`, which is appended to the
    /// log exactly like a success -- a job that fails must leave a durable record, not an
    /// error a caller might drop (`crate`'s own crate doc explains why). The steps run in
    /// this fixed order, each with its own [`pb::JobFailureKind`]:
    ///
    /// 1. `spec.kind` has no registered [`Executor`] -> `UNSUPPORTED_JOB_KIND`.
    /// 2. `spec.label` is not on the configured [`av_label::ClearanceLadder`] ->
    ///    `LABEL_REFUSED`.
    /// 3. For each input, in order: [`ObjectSource::fetch`] -> `INPUT_MISSING` on failure;
    ///    then its recomputed SHA-256 is compared (`openssl::memcmp::eq`) against
    ///    `asset.sha256` -> `INPUT_HASH_MISMATCH`. An executor is never handed unverified
    ///    bytes.
    /// 4. If `spec.executor` is `JOB_EXECUTOR_KIND_CONTAINER`, the job runs through this
    ///    `Runner`'s own configured [`Runner::register_container_executor`] executor instead of
    ///    the `kind`-registered one from Step 1 -- `EXECUTOR_UNAVAILABLE` if none is configured
    ///    (a visible, typed, logged refusal, never a silent skip; see
    ///    [`Runner::register_container_executor`]'s own doc for why `spec.kind` plays no role
    ///    here). Otherwise the (`kind`-registered, for a non-CONTAINER job) [`Executor::execute`]
    ///    runs, and whatever [`pb::JobFailure`] it returns (a [`ProcessExecutor`] maps a spawn
    ///    failure to `EXECUTOR_START_FAILED` and a non-zero exit to `NONZERO_EXIT` with the
    ///    code; `crate::container::ContainerExecutor` maps the same two kinds to "the container
    ///    could not start" and "the containerized command exited non-zero", plus
    ///    `EXECUTOR_UNAVAILABLE` for an absent or digest-mismatched image -- see that type's own
    ///    module doc) becomes this completion's failure.
    /// 5. Each output is stored through [`ObjectSink::put`] -> `OUTPUT_REJECTED` on failure.
    ///    Exactly one output may have `manifest: true`; more than one, or a manifest output
    ///    from an executor whose [`Executor::declares_manifest`] is `false`, is also
    ///    `OUTPUT_REJECTED`. The manifest output's `sha256` becomes
    ///    `JobCompletion.manifest_sha256`.
    /// 6. `started_tai_ns`/`finished_tai_ns` come from the injected [`Clock`]; `input_sha256`
    ///    is recorded in `spec.inputs`' own order; `spec_sha256` is recomputed here from the
    ///    `spec` actually run, never carried forward from whatever the submitter claimed.
    ///
    /// **No job outcome is ever an `Err`.** The `Ok` always carries a completion, whether
    /// the job succeeded or failed; the only `Err` this can return is
    /// [`crate::queue::JobError`] from appending that completion to the log -- an
    /// infrastructure failure of the queue itself, categorically different from a job that
    /// ran and failed, and the one condition a caller must be able to see and stop on.
    /// Deliberately not a panic: this crate's rule is that everything refused is a typed
    /// refusal a caller handles, and aborting a long-running runner process because one
    /// append hit `ENOSPC` would destroy the very evidence the caller needs to act on.
    pub fn run_one(&self, spec: &pb::JobSpec) -> Result<pb::JobCompletion, crate::queue::JobError> {
        let completion = self.execute_spec(spec, false);
        self.queue.complete(&completion)?;
        Ok(completion)
    }

    /// Runs every job [`crate::queue::JobQueue::pending`] currently reports, in order,
    /// through [`Runner::run_one`], returning each completion.
    pub fn run_pending(&self) -> Result<Vec<pb::JobCompletion>, crate::queue::JobError> {
        let pending = self.queue.pending()?;
        pending.iter().map(|spec| self.run_one(spec)).collect()
    }

    /// Like [`Runner::run_one`], but calls the registered [`Executor::execute_streaming`]
    /// instead of [`Executor::execute`] -- every other step (label check, input fetch/
    /// verify, the manifest-count/`declares_manifest` checks, storing whatever the executor
    /// returns through [`ObjectSink::put`], `started_tai_ns`/`finished_tai_ns`, appending to
    /// the log) is [`Runner::run_one`]'s own fixed order, unchanged: this is
    /// [`Runner::execute_spec`] run with `streaming: true` instead of `false`, sharing every
    /// line of that logic rather than a second, independently-drifting copy of it.
    ///
    /// For [`crate::tiler::TilerExecutor`] specifically, this is what makes tile storage
    /// actually stream -- see that type's own `run_imagery_streaming` doc for what changes
    /// about the returned [`pb::JobCompletion::outputs`] on this path (only the manifest, not
    /// one entry per tile) and why nothing about the job's real outputs goes unrecorded by
    /// that. An executor that does not override `execute_streaming` behaves identically to
    /// [`Runner::run_one`] here -- the default implementation only delegates.
    pub fn run_one_streaming(&self, spec: &pb::JobSpec) -> Result<pb::JobCompletion, crate::queue::JobError> {
        let completion = self.execute_spec(spec, true);
        self.queue.complete(&completion)?;
        Ok(completion)
    }

    /// The actual step-by-step run -- see [`Runner::run_one`]'s own doc for the fixed order.
    /// Split out from `run_one`/`run_one_streaming` only so those two alone are responsible
    /// for appending the result to the log; this function itself never touches the log.
    /// `streaming` selects [`Executor::execute_streaming`] (`true`, [`Runner::
    /// run_one_streaming`]'s own call) over [`Executor::execute`] (`false`, [`Runner::
    /// run_one`]'s own call) at step 4 below -- the one line the two callers differ on.
    fn execute_spec(&self, spec: &pb::JobSpec, streaming: bool) -> pb::JobCompletion {
        let started_tai_ns = self.clock.now_tai_ns();
        let spec_sha256 = hash::hex_encode(&openssl::sha::sha256(&prost::Message::encode_to_vec(spec)));

        let make_failure = |kind: pb::JobFailureKind, detail: String, exit_code: i32, input_sha256: Vec<String>, outputs: Vec<pb::AssetRef>| pb::JobCompletion {
            job_id: spec.job_id.clone(),
            spec_sha256: spec_sha256.clone(),
            input_sha256,
            outputs,
            manifest_sha256: String::new(),
            started_tai_ns,
            finished_tai_ns: self.clock.now_tai_ns(),
            ok: false,
            failure: Some(pb::JobFailure { kind: kind as i32, detail, exit_code }),
        };

        // Step 1: spec.kind has no registered Executor -> UNSUPPORTED_JOB_KIND. Looked up
        // unconditionally, even for a CONTAINER job (see Step 4 below) -- kind_executor is not
        // necessarily the Executor that actually runs this job.
        let Some(kind_executor) = self.executors.get(&spec.kind) else {
            return make_failure(pb::JobFailureKind::UnsupportedJobKind, format!("no executor registered for job kind {:?}", spec.kind), 0, vec![], vec![]);
        };

        // Step 2: spec.label must be on the configured ladder -> LABEL_REFUSED.
        let label = spec.label.clone().unwrap_or_default();
        if self.ladder.rank(&label.marking).is_none() {
            return make_failure(
                pb::JobFailureKind::LabelRefused,
                format!("label marking {:?} is not on the configured clearance ladder", label.marking),
                0,
                vec![],
                vec![],
            );
        }

        // Step 3: fetch and hash-verify every input, in order -- an executor never sees
        // bytes that have not cleared this check.
        let mut inputs: Vec<JobInput> = Vec::with_capacity(spec.inputs.len());
        let mut input_sha256: Vec<String> = Vec::with_capacity(spec.inputs.len());
        for asset in &spec.inputs {
            let bytes = match self.source.fetch(asset) {
                Ok(bytes) => bytes,
                Err(failure) => return make_failure(job_failure_kind(&failure), failure.detail, failure.exit_code, input_sha256, vec![]),
            };
            let recomputed = openssl::sha::sha256(&bytes);
            let expected = hash::hex_decode(&asset.sha256);
            let matches = matches!(&expected, Some(e) if e.len() == 32 && openssl::memcmp::eq(e, &recomputed));
            if !matches {
                return make_failure(
                    pb::JobFailureKind::InputHashMismatch,
                    format!("input {:?}: recorded sha256 {:?} does not match the fetched bytes' own sha256 {:?}", asset.uri, asset.sha256, hash::hex_encode(&recomputed)),
                    0,
                    input_sha256,
                    vec![],
                );
            }
            input_sha256.push(asset.sha256.clone());
            inputs.push(JobInput { asset: asset.clone(), bytes });
        }

        // Step 4: JOB_EXECUTOR_KIND_CONTAINER runs through this Runner's own configured
        // container executor (Runner::register_container_executor), entirely bypassing
        // kind_executor from Step 1 above -- spec.kind plays no role in choosing HOW a
        // CONTAINER job runs (crates/av-jobs/tests/runner.rs::
        // executor_unavailable_for_the_container_kind_is_recorded_on_the_log proves this: a
        // real, working ProcessExecutor registered under the job's own kind is never reached).
        // A Runner with no container executor configured refuses every CONTAINER job as
        // EXECUTOR_UNAVAILABLE -- the same typed, logged refusal this crate always gave
        // CONTAINER jobs before this round, now scoped to "unconfigured" rather than
        // "unimplemented". Otherwise the active executor (kind_executor for a non-CONTAINER
        // job, the configured container executor for a CONTAINER one) runs.
        let is_container = pb::JobExecutorKind::try_from(spec.executor).unwrap_or(pb::JobExecutorKind::Unspecified) == pb::JobExecutorKind::Container;
        let active_executor: &dyn Executor = if is_container {
            match &self.container_executor {
                Some(executor) => executor.as_ref(),
                None => {
                    return make_failure(
                        pb::JobFailureKind::ExecutorUnavailable,
                        "JOB_EXECUTOR_KIND_CONTAINER: no container executor is configured on this Runner -- register one with Runner::register_container_executor before submitting a CONTAINER job".to_string(),
                        0,
                        input_sha256,
                        vec![],
                    );
                }
            }
        } else {
            kind_executor.as_ref()
        };
        let raw_outputs = match if streaming { active_executor.execute_streaming(spec, &inputs, self.sink.as_ref(), &label) } else { active_executor.execute(spec, &inputs) } {
            Ok(outputs) => outputs,
            Err(failure) => return make_failure(job_failure_kind(&failure), failure.detail, failure.exit_code, input_sha256, vec![]),
        };

        // Step 5: at most one manifest output, and only from an executor that declares it
        // produces one; store every output through the sink.
        let manifest_count = raw_outputs.iter().filter(|o| o.manifest).count();
        if manifest_count > 1 {
            return make_failure(pb::JobFailureKind::OutputRejected, format!("job produced {manifest_count} manifest outputs; at most one is allowed"), 0, input_sha256, vec![]);
        }
        if manifest_count == 1 && !active_executor.declares_manifest() {
            return make_failure(
                pb::JobFailureKind::OutputRejected,
                format!("job kind {:?} produced a manifest output, but its executor declares it produces none", spec.kind),
                0,
                input_sha256,
                vec![],
            );
        }

        let mut outputs: Vec<pb::AssetRef> = Vec::with_capacity(raw_outputs.len());
        let mut manifest_sha256 = String::new();
        for out in &raw_outputs {
            let asset = match self.sink.put(&out.bytes, &out.media_type, &label) {
                Ok(asset) => asset,
                Err(failure) => return make_failure(job_failure_kind(&failure), failure.detail, failure.exit_code, input_sha256, outputs),
            };
            if out.manifest {
                manifest_sha256 = asset.sha256.clone();
            }
            outputs.push(asset);
        }

        // Step 6: started_tai_ns/finished_tai_ns from the injected Clock; input_sha256 in
        // spec.inputs' own order; spec_sha256 recomputed above from the spec actually run.
        pb::JobCompletion {
            job_id: spec.job_id.clone(),
            spec_sha256,
            input_sha256,
            outputs,
            manifest_sha256,
            started_tai_ns,
            finished_tai_ns: self.clock.now_tai_ns(),
            ok: true,
            failure: None,
        }
    }
}

/// The `JobFailureKind` an already-built [`pb::JobFailure`] (from an [`ObjectSource`],
/// [`ObjectSink`] or [`Executor`]) carries -- decoded once here rather than at every one of
/// `Runner`'s own call sites. An out-of-range `kind` (which none of this crate's own
/// implementations ever produce) decodes as `UNSPECIFIED` rather than panicking.
fn job_failure_kind(failure: &pb::JobFailure) -> pb::JobFailureKind {
    pb::JobFailureKind::try_from(failure.kind).unwrap_or(pb::JobFailureKind::Unspecified)
}

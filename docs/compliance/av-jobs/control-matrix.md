# av-jobs — NIST SP 800-171 Rev 2 control matrix

`crates/av-jobs` — H3 (`docs/heavy-plan.md`, round 2/3): the durable, hash-chained, file-backed
job queue; the job record types (`JobSpec`/`JobCompletion`/`JobFailure`); the runner that drains
the queue through a registered `Executor`, fetching and hash-verifying every input before an
executor ever sees it and storing every output through an `ObjectSink` afterward; and (round 3,
H3's last open item) the `JOB_EXECUTOR_KIND_CONTAINER` executor that runs a job's command inside
a hardened, digest-pinned, network-isolated container via a bind mount, never touching an object
store itself. Same format as `docs/compliance/av-gateway/control-matrix.md` and
`docs/compliance/av-command/control-matrix.md` (family, requirement ID, requirement,
implementation `file:function`, evidence command), with the Met / Partial / Inherited / Gap
legend those files use.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation — see
`docs/compliance/av-command/control-matrix.md`'s identical admonition for what that means.
**Every evidence command below names a real, committed test.** Because a workspace-wide `cargo
test` held this host's `target/` lock for the whole of this task, no `cargo` command below was
actually executed here — each row's evidence command is instead backed by reading the cited
test's own source and confirming its logic matches the row's claim, recorded honestly in this
task's own report rather than left implied. One row's own real-Docker test
(`crates/av-jobs/tests/container.rs::container_executor_runs_a_real_job_inside_a_real_container`)
is additionally known, directly, to be UNRUNNABLE on this host right now: `docker image inspect
alpine:latest` was checked (no pull) and the probe image this test depends on is currently
absent — consistent with this task's brief, which records that Colima's image GC sweeps any
unheld, freshly-pulled image within roughly three minutes on this host. That row's own gate
would skip visibly, naming the missing image, exactly as this workspace's convention requires;
it was not forced to run.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this crate's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this crate does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), not by this crate. |
| Gap | Not implemented anywhere in this crate today. See [Deficiencies](#deficiencies). |

## Scope

This crate is a library with no network surface of its own (no gRPC/HTTP listener — a caller
embeds it and calls `Runner::run_one`/`run_pending` directly; `src/bin/av-tile-fixture.rs` is a
`store-fixture`-gated dev/demo binary, not a service). Access-control-shaped controls (who may
reach this crate's own API) are therefore largely Inherited — the process boundary is the
caller's job, not this crate's.

- `src/log.rs` — [`log::JobLog`]: one append-only, hash-chained file per named queue
  (`payload_len`/`record_hash`/`payload`, `record_hash = SHA-256(prev_record_hash || payload)`
  via `crate::hash::chain_hash`) — the identical pattern `crates/av-ingest/src/
  log.rs::PartitionLog` already established, applied here to jobs. `JobLog::open` always scans
  the whole file and recovers from exactly two conditions (a torn tail, a corrupt trailing
  record), truncating only those; a corrupt record anywhere else is left untouched and reported
  by `JobLog::verify` as a broken chain at that record's index, never silently repaired.
- `src/queue.rs` — [`queue::JobQueue`]: `submit`/`pending`/`complete` over one `JobLog`; a
  duplicate `job_id` is a typed refusal (`JobError::DuplicateJobId`), checked by replaying the
  whole durable log, never an in-memory cache that could drift.
- `src/runner.rs` — [`runner::Runner`]: `run_one`/`run_one_streaming` never return an `Err` for
  a job that ran and failed — every failure path (unsupported kind, label refused, input
  missing/hash-mismatched, executor unavailable/start-failed/nonzero-exit, output rejected) is
  its own typed `pb::JobFailureKind`, appended to the durable log exactly like a success. The
  one `Err` this can return is the log append itself failing — an infrastructure failure,
  categorically different from a job outcome, and never silently dropped.
- `src/container.rs` (round 3, H3's last open item) — [`container::ContainerExecutor`]: the
  `JOB_EXECUTOR_KIND_CONTAINER` executor. Bind-mount data flow (verified input bytes in
  read-only, produced output bytes out read-write — never stdin/stdout), fixed hardening
  (`HARDENING_RUN_FLAGS`, non-root `CONTAINER_UID_GID`, `--network none`), a digest gate that
  never pulls (`digest_gate`, question 154/212(a)), and unconditional `docker rm -f` +
  scratch-directory cleanup on every exit path (`Cleanup`'s own `Drop`).
- `src/hash.rs` — [`hash::chain_hash`]/[`hash::GENESIS`]: the two-line hash-chain primitive,
  reimplemented rather than depended on (`av-edge` is off this heavy track entirely).
- `src/clock.rs` — this crate's own two-method `Clock` trait (`SystemClock`/`TestClock`),
  mirroring `av-command`'s identical convention rather than depending on a hot-path crate.
- `src/tiler.rs`/`src/raster.rs`/`src/png.rs`/`src/scheme.rs`/`src/terrain.rs`/`src/tiles3d.rs`
  (H3b/P3a) — the tiler `Executor` implementation and its supporting codecs; out of this
  matrix's own detailed scope (the brief for this task names the queue/runner/container
  surfaces specifically) beyond noting `runner::Runner::execute_spec`'s label/hash checks apply
  to every executor equally, tiler included.

**Not built in this crate, stated here rather than left implied**: no authentication or
authorization of a caller (this crate has no caller-facing surface to authenticate — see AC
3.1.1/3.1.2's row), no durable audit-sink file distinct from the job log itself, no FIPS-posture
detection, no correlation/aggregation tooling over the log beyond `JobQueue::pending`/
`read_all`.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Inherited | This crate has no network surface of its own (Scope, above) — whatever process embeds a `Runner` is responsible for authenticating whoever asked it to submit a job, before `JobQueue::submit`/`Runner::run_one` are ever called. Out of this crate's own boundary, not a silent gap | N/A |
| 3.1.3 | Control the flow of CUI | Met | `crates/av-jobs/src/runner.rs::Runner::execute_spec` step 2: `spec.label` must be on the configured `av_label::ClearanceLadder` (`self.ladder.rank(&label.marking)`) or the job is refused `LABEL_REFUSED` before any input is even fetched — a durable, typed `JobCompletion` records the refusal exactly like any other outcome | `cargo test -p av-jobs --test runner label_refused_is_recorded_on_the_log` |
| 3.1.5 | Least privilege | Inherited / Not applicable | No role/privilege table exists in this crate — a `Runner`'s own configured `Executor`s and `container_executor` slot are wiring choices made by the embedding caller, not a runtime-negotiated privilege | N/A |
| 3.1.20 | Control connections to external systems (round 3 decision 10: the container never touches the object store) | Met | `crates/av-jobs/src/container.rs::ContainerExecutor::execute` never imports or calls `av_store`/`av_command` of any kind — structurally impossible for it to, since `crates/av-jobs/Cargo.toml`'s own `[dependencies]` names `av-store` only as `optional = true` behind the unrelated `store-fixture` feature (gating a separate dev binary, `av-tile-fixture`), and `container.rs` is compiled unconditionally as part of this crate's default library build. Every input byte a container sees was already fetched and hash-verified by `Runner::execute_spec` BEFORE the executor ever runs (`Executor::execute`'s own trait doc); every output byte crosses back out through the same bind mount, read by `Runner::execute_spec` AFTER the container exits, and only THEN handed to `ObjectSink::put`. The container itself gets `--network none` on every run (`HARDENING_RUN_FLAGS` plus this one addition), so even a compromised job command inside the container has no network path to reach an object store, real or otherwise | `cargo test -p av-jobs --lib container::tests::digest_gate_refuses_an_empty_image_or_digest_without_touching_docker` (needs no Docker) and `grep -n "av_store\|av_command" crates/av-jobs/src/container.rs` (empty — confirms no import) and `grep -n "^av-store" crates/av-jobs/Cargo.toml` (shows `optional = true`, `store-fixture`-gated only) |
| 3.1.22 | Control publicly-posted content | Inherited | This crate posts nothing publicly | N/A |
| 3.1.4 | Separation of duties (two-person rule) | Inherited / Not built | No command-authorization concept exists in this crate | N/A |
| 3.1.6 / 3.1.7–3.1.11 / 3.1.12–3.1.19 / 3.1.21 | Session lock, remote/mobile/wireless access, encrypted remote access, etc. | Inherited | This crate has no network listener of its own to bind, remotely access, or encrypt | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Met | `crates/av-jobs/src/log.rs::JobLog::append` `write_all`s, `flush`es, and `sync_all`s every record before returning `Ok` — durable by the time a caller sees success. `Runner::run_one`/`run_one_streaming` (`crates/av-jobs/src/runner.rs`) NEVER return an `Err` for a job that ran and failed: every failure path is its own typed `pb::JobFailureKind`, appended to the log exactly like a success, so "a job ran, with this specific outcome, at this specific time" is provable from the log alone, for every outcome | `cargo test -p av-jobs --test runner nonzero_exit_is_recorded_on_the_log_with_the_real_exit_code executor_start_failed_is_recorded_on_the_log input_hash_mismatch_is_recorded_on_the_log input_missing_is_recorded_on_the_log unsupported_job_kind_is_recorded_on_the_log a_successful_completions_outputs_and_manifest_hash_survive_a_fresh_log_reopen` |
| 3.3.1 (retention/durability across a process restart) | Records survive independent of the writing process's own lifetime | Met | `crates/av-jobs/src/queue.rs::JobQueue::pending` never keeps a second, in-memory index of queue state — it replays the log fresh on every call, so a second, independent `JobQueue`/`JobLog::open` over the same directory sees exactly what a prior process lifetime wrote | `cargo test -p av-jobs --lib queue::tests::complete_appends_a_completed_record_readable_from_a_fresh_log` and `cargo test -p av-jobs --test runner a_successful_completions_outputs_and_manifest_hash_survive_a_fresh_log_reopen` |
| 3.3.8 | Protect audit information from unauthorized access/modification | Met | SHA-256 hash chain (`record_hash = SHA-256(prev_record_hash \|\| payload)`, `crate::hash::chain_hash`, identical convention to `av-command`'s `Ledger`) — `crates/av-jobs/src/log.rs::JobLog::verify` re-reads the file from disk independent of any in-memory tip state and detects a tampered record anywhere in the middle of the file (never silently truncated — only a torn TAIL or a corrupt TRAILING record is ever discarded by `JobLog::open`; a middle-record tamper is left exactly as found and reported by `verify` at its own index). `chain_hash` itself is pinned against an independently-computed `openssl(1)` CLI digest, not merely self-consistent | `cargo test -p av-jobs --lib log::tests::a_corrupt_middle_record_is_not_truncated_and_is_reported_by_verify log::tests::append_extends_the_chain_and_verify_reports_it_intact log::tests::tip_hash_is_pinned_to_an_independently_derived_golden` and `cargo test -p av-jobs --lib hash::tests::chain_hash_matches_an_independently_computed_openssl_digest` |
| 3.3.2 | Trace actions to individual users/processes | Gap | No `JobSpec`/`JobCompletion` field carries an authenticated human or service identity — `JobCompletion.job_id`/`spec_sha256`/`input_sha256`/`outputs` trace WHAT ran and its exact data lineage by content hash (a real, strong property), but never WHO submitted it. By design (this crate has no auth surface of its own — AC 3.1.1/3.1.2's row); the gap is real and unclosed, not silently assumed away | N/A |
| 3.3.4 | Alert on audit logging failure | Partial | `Runner::run_one`'s own `Result` surfaces a failed log append as a real, propagated `crate::queue::JobError`, never silently swallowed (`crates/av-jobs/src/runner.rs`'s own doc: "the only `Err` this can return... the one condition a caller must be able to see and stop on") — fail-loud to the CALLER, not an alert to an external operator/SIEM (no such sink exists in this crate) | N/A (verified by code inspection: `Runner::run_one`/`run_one_streaming` propagate `self.queue.complete(&completion)?` rather than `.ok()`/`.unwrap_or_default()`-ing it away) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation/aggregation tooling beyond `JobQueue::pending` (uncompleted jobs) and `JobLog::read_all`/`verify` (the raw, per-queue log) — no cross-queue query layer of any kind | N/A |
| 3.3.9 | Limit audit management to a subset of privileged users | Inherited | No network surface exposes the log at all (AC 3.1.1/3.1.2's row) — filesystem permissions on the queue directory are the whole boundary, set by nothing in this crate | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Inherited | `JobCompletion.started_tai_ns`/`finished_tai_ns` come from the injected `crate::clock::Clock` (`crates/av-jobs/src/clock.rs`), never read from the wall clock directly inside `Runner`; NTP synchronization of `SystemClock` is the environment's responsibility | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 / 3.4.2 | Baseline configuration & enforce security settings | Met | **The container hardening posture is declared in two languages and kept honest by a test that parses the Python source at test time** (round 3 decision 11): `crates/av-jobs/src/container.rs::HARDENING_RUN_FLAGS` (Rust) must equal `altavista/container_hardening.py::HARDENING_RUN_FLAGS` (Python) element for element — verified: both currently read `["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"]`, confirmed directly by reading both files. `Cargo.lock` pins every dependency; the workspace-root `deny.toml` covers this crate's tree | `cargo test -p av-jobs --lib container::tests::hardening_run_flags_matches_the_python_declaration_in_container_hardening_py` (runs no Docker — a plain text comparison) and `grep -n "HARDENING_RUN_FLAGS" altavista/container_hardening.py crates/av-jobs/src/container.rs` (both lines pasted in this task's own report) |
| 3.4.6 | Least functionality | Met | `crates/av-jobs/src/container.rs::ContainerExecutor::execute` runs every container job as the fixed non-root `CONTAINER_UID_GID` ("10001:10001"), a read-only root filesystem, `--cap-drop ALL`, `--security-opt no-new-privileges`, the default (never overridden) seccomp profile, and `--network none` — the job's command can write only to its own bind-mounted `/av-job/out`, read only its own bind-mounted, read-only `/av-job/in`, and reach no network and no other capability at all | Same as CM 3.4.1/3.4.2 above, plus `grep -n "CONTAINER_UID_GID\|network.*none" crates/av-jobs/src/container.rs` |
| 3.4.7 | Restrict nonessential programs/ports | Met / Inherited | This crate binds no network port of its own at all (no service binary — Scope, above); the container executor's own containers get zero ports and zero network (`--network none`, unconditional, every run) | N/A |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Inherited | No caller-facing surface exists in this crate to authenticate against (AC 3.1.1/3.1.2's row) | N/A |
| 3.5.3 | MFA for privileged/remote access | Inherited / N/A | No privileged human-facing action exists in this crate | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Inherited / N/A | This crate holds no passwords or long-lived secrets of its own | N/A |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation (the container executor's own boundary, round 3 decision 10) | Met | See AC 3.1.20's row — `--network none` plus the structural absence of any `av-store`/`av-command` dependency inside `container.rs` together mean a job's own containerized command has no reachable network boundary to cross at all, verified rather than merely configured | Same as AC 3.1.20 |
| 3.13.6 | Deny network traffic by default | Met | Every container this executor runs gets `--network none` unconditionally — there is no flag or code path in `ContainerExecutor::execute` that grants network access to a job's command | `grep -n '"--network".to_string(), "none".to_string()' crates/av-jobs/src/container.rs` |
| 3.13.8 | Encrypt CUI in transit | Inherited / N/A | This crate has no network traffic of its own to encrypt — the queue log is local disk, and the container executor's own containers reach no network at all (SC 3.13.6's row) | N/A |
| 3.13.11 | Use FIPS-validated cryptography | Gap | This crate calls `openssl::sha::sha256` (system OpenSSL, ADR-004: `crates/av-jobs/src/hash.rs::chain_hash`) but has no FIPS-posture detection of its own (unlike `av-command`'s `crates/av-command/src/fips.rs`) | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.6 | Monitor for attacks / validate input | Met | `crates/av-jobs/src/runner.rs::Runner::execute_spec`'s fixed, ordered checks: unknown kind (`UNSUPPORTED_JOB_KIND`), label off the ladder (`LABEL_REFUSED`), a fetched input whose recomputed SHA-256 disagrees with `AssetRef.sha256` via a constant-time `openssl::memcmp::eq` compare (`INPUT_HASH_MISMATCH` — an executor is NEVER handed unverified bytes), more than one manifest output or a manifest from a non-declaring executor (`OUTPUT_REJECTED`). `crates/av-jobs/src/container.rs::digest_gate` independently validates a container job's own image: absent locally is refused (never pulled — question 154), present under a DIFFERENT id than the job requested is refused rather than silently substituted (question 212(a)), and `docker run`'s own reserved exit codes 125–127 are distinguished (`EXECUTOR_START_FAILED`, "never really started") from any other non-zero exit (`NONZERO_EXIT`, the contained command's own real status) | `cargo test -p av-jobs --test runner input_hash_mismatch_is_recorded_on_the_log unsupported_job_kind_is_recorded_on_the_log output_rejected_is_recorded_on_the_log_for_two_manifest_outputs output_rejected_for_a_manifest_output_from_an_executor_that_declares_none` and `cargo test -p av-jobs --lib container::tests::digest_gate_refuses_an_empty_image_or_digest_without_touching_docker` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` (RustSec DB) covers this crate's own dependency tree via the workspace-root `deny.toml`; no container/binary image scan exists for the probe image `tests/container.rs` runs against | `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"; cargo deny check advisories` (not run this task — see this file's own top admonition) |

## Inherited wholesale (not this crate's responsibility)

Matching `docs/compliance/av-gateway/control-matrix.md`'s identical table:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | The queue log's own filesystem permissions are not set by this crate |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **No caller identity is ever recorded** (AU 3.3.2, AC 3.1.1/3.1.2, IA 3.5.1/3.5.2) — this
   crate has no auth surface of its own at all, by design (it is a library, embedded by
   whatever process actually submits jobs); `JobCompletion` traces WHAT ran and its exact data
   lineage by content hash, never WHO submitted it. Closing this is the embedding caller's
   job, not this crate's, but it is recorded here rather than left implied.
2. **No correlation/aggregation tooling over the job log** (AU 3.3.5/3.3.6) — only sequential
   replay via `JobQueue::pending`/`JobLog::read_all`/`verify`.
3. **No FIPS-posture detection of this crate's own** (SC 3.13.11) — unlike `av-command`, this
   crate reports nothing about its own OpenSSL linkage, despite calling into it directly for
   every hash-chain step.
4. **The container executor's real-Docker proof is currently unrunnable on this host** — not a
   code defect, a measured fact this round: `docker image inspect alpine:latest` (no pull)
   confirms the probe image `crates/av-jobs/tests/container.rs` depends on
   (`CONTAINER_EXECUTOR_IMAGE_DIGEST.md`) is absent right now, consistent with this task's own
   brief (Colima's image GC sweeps an unheld, freshly-pulled image within roughly three
   minutes on this host). The test's own `gate()` skips visibly rather than silently standing
   in as passing — this crate's design already accounts for exactly this failure mode (the
   same posture `docs/compliance/av-command/control-matrix.md`'s own Deficiency 13 records for
   the proposer's container image).
5. **No SBOM specific to this crate** (CM 3.4.1's usual note) — `Cargo.lock`/`deny.toml`
   cover it at the workspace level only.

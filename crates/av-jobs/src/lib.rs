//! H3a (`docs/heavy-plan.md` H3, first half; question 216): the durable job queue, the job
//! record types, and the runner. **The tiler itself is task 3b and is not in this crate** --
//! `crate::runner::Executor` is the seam H3b implements against, not something this crate
//! provides an implementation of.
//!
//! # Why a file-backed hash chain, not a broker
//!
//! `docs/heavy-plan.md`'s own H3 milestone text: "a runner that takes jobs from a durable,
//! hash-chained file-backed queue (the pattern of the ingest log; a broker later, ADR-003)".
//! ADR-003 draws this substrate's boundary explicitly: nothing on this platform's hot path
//! depends on an orchestrator, and this track's own substrate decision names Postgres and
//! MinIO but no message broker. [`crate::log::JobLog`] is therefore the same pattern
//! `crates/av-ingest/src/log.rs::PartitionLog` already established for the edge ingest
//! ledger -- an append-only, hash-chained file, durable by `fsync` rather than by a
//! separately-operated service -- applied here to jobs instead of measurement batches. A
//! broker (Kafka, NATS, or similar) is deliberately left for a later round: [`crate::queue::
//! JobQueue`]'s own public API (`submit`/`pending`/`complete`) is already the narrow seam a
//! broker-backed implementation would sit behind, so replacing the file with a broker later
//! is a new implementation of that same trait-shaped surface, not a rewrite of every caller.
//!
//! # Why a failed job is never an `Err`
//!
//! A job that fails must leave a durable record on the log, exactly like a job that
//! succeeds -- `crate::runner::Runner::run_one`'s own doc comment walks the fixed order of
//! checks. If a failed job could come back as `Err(..)`, a caller that mishandles or drops
//! that `Err` (a `?` in the wrong place, an unwrapped `Result` that panics before the
//! completion is appended) would silently lose the one artifact this whole queue exists to
//! produce: proof that a specific job ran, with a specific outcome, at a specific time.
//! Making every outcome -- success or failure -- an ordinary `pb::JobCompletion` value that
//! `run_one` *itself* appends to the log before returning closes that gap structurally,
//! rather than relying on every future caller to get error handling right.
//!
//! `run_one`'s own `Result` exists for exactly one other thing: appending that completion to
//! the log can itself fail (a full disk, a revoked permission), and that is an infrastructure
//! failure of the queue, categorically different from a job that ran and failed. It comes
//! back as a typed `crate::queue::JobError` rather than a panic, because this crate's
//! standing rule is that everything refused is a refusal a caller handles -- and aborting a
//! long-running runner process over one failed append would destroy the very evidence that
//! caller most needs to act on. (Manager's review finding, H3a: the first implementation
//! panicked here.)
//!
//! # Why this crate depends on neither `av-store`, `av-edge`, nor `av-command`
//!
//! - **`av-store`**: `crate::runner::ObjectSource`/`ObjectSink` are the seam a real,
//!   store-backed implementation is wired through; task 3b's own crate depends on both
//!   `av-jobs` and `av-store` to provide one. `av-jobs` staying independent of `av-store`
//!   keeps this crate out of `crates/av-store/tests/claim_check_hot_path.rs`'s own hot-path
//!   dependency assertion's blast radius -- that test enumerates `av-ingest`/`av-track`/
//!   `av-command` today, and a job-runner crate quietly depending on `av-store` is exactly
//!   the kind of dependency creep that assertion exists to catch, not something this crate
//!   should risk by depending on it directly.
//! - **`av-edge`**: off this heavy track entirely (`docs/heavy-plan.md`'s own "Isolation"
//!   section: this track does not edit the kernel's executor, router or fault modules, the
//!   ingest, the edge crates or the command service). `crate::hash` reimplements
//!   `av_edge::hash`'s two-line `GENESIS`/`chain_hash` primitive instead of depending on it
//!   -- see that module's own doc for the reasoning, which is the same boundary
//!   `crates/av-store/src/labels.rs`'s former module doc recorded for the clearance-ladder
//!   convention before question 218 gave that one convention a shared home (`av-label`); the
//!   hash-chain convention has no such shared home to extract into, so the two lines stay
//!   duplicated, deliberately, across the two independently-owned crates.
//! - **`av-command`**: a hot-path crate (`crates/av-command/src/clock.rs`'s own module doc:
//!   "every epoch this crate writes... comes from a `Clock` passed in by the caller, never
//!   from reading the wall clock directly"). `crate::clock` defines its own two-method
//!   `Clock` trait and its own `SystemClock`/`TestClock`, mirroring that convention rather
//!   than pulling a job-queue crate into a hot-path crate's dependency tree for the sake of
//!   one trait and two small structs.

pub mod clock;
pub mod hash;
pub mod log;
pub mod queue;
pub mod runner;

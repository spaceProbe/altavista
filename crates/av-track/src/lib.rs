//! E5, "the engine consumes" (`docs/edge-plan.md` milestone E5; built directly on E4,
//! `83eeb83`).
//!
//! The pieces, in the order data flows through them:
//!
//! - [`consumer`] -- reads accepted `MeasurementBatch`es out of `av_ingest::log::
//!   PartitionLog` in order, by explicit partition and explicit offset, verifying the
//!   partition's own record-hash chain before ever handing out a single [`av_edge::pb::
//!   Measurement`]. Its [`consumer::MeasurementConsumer`] trait is this crate's answer to
//!   `docs/edge-plan.md`'s "spoore-io's consumer trait" (question 200(d)) -- see that
//!   module's own doc for the finding that no such trait exists in `spoore-io` today, what
//!   this crate built instead, and exactly what an upstream PR to `spoore-io` would need.
//! - [`config`] -- [`config::TrackConfig`]: every knob this crate's engine bridge runs
//!   under, declared in one hashable, TOML-shaped value (question 11's rule), mirroring
//!   `spoore_node::config::NodeConfig`'s own construction path
//!   (`build_shard_config`/`build_grid`/`build_associator`/`build_tree_and_initiator`) --
//!   see that module's own doc for exactly which parts are mirrored and which are
//!   deliberately narrower (this crate has no grid/multi-cell fleet; one shard, one
//!   sensor, one target class).
//! - [`bridge`] -- [`bridge::EngineBridge`]: groups a partition's measurements into
//!   `spoore_engine::Scan`s by epoch, converts each through `av_cdm::spoore_v0::
//!   measurement_from_pb` (never a second conversion -- the task's own instruction), and
//!   drives a real `spoore_engine::Shard::process`.
//! - [`compare`] -- the produced tracks against the run's own truth `Trajectory`, printing
//!   a full time series plus max/p50/p99, against [`compare::POSITION_TOLERANCE_M`].
//! - [`latency`] -- pure latency-report arithmetic (min/max/p50/p99) over caller-supplied
//!   `(emit, accept)` instant pairs; never reads a clock itself (question 199) -- see that
//!   module's own doc for exactly which two instants and why, and `src/bin/
//!   av-edge-latency.rs` for the one binary in this crate allowed to read a real clock.
//! - [`harness`] -- D5a: `src/bin/av-edge-latency.rs`'s own driving core, moved into this
//!   library (not rewritten) so a test can drive two or three in-process `EdgeIngestService`s
//!   concurrently through the identical code path the container-based, multi-placement
//!   binary run later uses -- see that module's own doc for the in-process/targets modes,
//!   the pacing discipline, and exactly where the two `Instant` reads still sit.
//!
//! # Why this crate, and not `av-edge`
//!
//! `av-edge` must stay transport- and engine-free (E1's own task brief; `crates/av-edge/
//! src/plugin/mod.rs`'s module doc repeats the same constraint for the plugin library).
//! This crate is the mirror image: it depends on `av_ingest::log` (a library dependency,
//! not a transport one -- see `Cargo.toml`'s own comment) and on the real `spoore-engine`/
//! `spoore-models`/`spoore-assoc`/`spoore-tree` crates, neither of which `av-edge` or
//! `av-ingest` may ever depend on.
//!
//! # What this crate does not depend on
//!
//! `spoore-io` appears nowhere in this crate's dependency tree (`tests/
//! no_object_store.rs` asserts this structurally) -- this crate defines its own consumer
//! trait rather than `spoore_io::kafka::PartitionConsumer` (see [`consumer`]'s module doc
//! for why), so it has no reason to depend on `spoore-io` at all, and therefore no path to
//! `spoore-io::clickhouse_sink` (an object-store/ClickHouse sink) either -- "nothing on
//! this path calls the object store" (E5's own exit test) is true structurally, not by
//! omission that happened not to be exercised.

pub mod bridge;
pub mod compare;
pub mod config;
pub mod consumer;
pub mod harness;
pub mod latency;

pub use av_edge::pb;

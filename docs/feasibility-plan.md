# Feasibility mode: plan

Requested by the user 2026-09-09 as a parallel track while team 2 finishes the fault
runtimes. Decisions in `open-questions.md` question 191. This is P1's missing half in
`architecture.md` ("parameter sweeps on the job runner with MoEs in ClickHouse").

## Goal

A design reference mission fanned out over a declared parameter space and a declared number
of Monte Carlo draws, every sample an ordinary executor run reproducible from its own seed
and hash, the scores aggregated per grid point, and the study viewable in the Alta Vista
viewer beside the runs it came from.

## What exists

- `ParameterSweep` and `SweepAxis` (system.proto): axes as explicit values or a range with
  steps, and a draw count; `Scenario.seeds`; `ExecutionErrorMode::Sampled` and the Gates
  model; seeded PCG64 substreams keyed by id; `RunProducts` with `ScoreResult`s; `av-run`.
- Not yet: any sweep executor, seed derivation per sample, aggregation, a results message
  (added now: `SweepSample`, `ScoreAggregate`, `SweepResults` in run.proto), a results view.

## Isolation

The feasibility team works in the git worktree `/Users/probe/code/AltaVista-feasibility` on
branch `feasibility` (from `develop`), so it never edits the tree team 2 is using. New code
lives in a new crate `crates/av-sweep` and new modules under `altavista/feasibility/` and
`web/js/panels/`; it consumes `av_kernel::drm::execute` as a library and must not edit
`crates/av-kernel/src/drm/executor.rs`, `router.rs` or the fault modules. The lead merges
`develop` into `feasibility` between rounds and `feasibility` back into `develop` after
acceptance.

## Milestones

**F1 Sweep executor (`crates/av-sweep`).** Load a `ParameterSweep` (YAML, typed, hashed like
the other artifacts); expand axes into the grid (explicit values, or `min..max` in `steps`);
derive each sample's seed as SHA-256 over (base seed, sweep hash, point, draw) truncated to
64 bits; for each sample apply the axis values as `SystemInstance.overrides` on a copy of
the DRM, rehash, and run through `execute()` with `ExecutionErrorMode::Sampled` when the
sweep declares draws greater than one; run samples in parallel across processes (GMAT is
process-global, one run per process) with a declared worker count; collect `SweepSample`s;
write `SweepResults` (binary and JSON) and optionally each sample's `RunProducts`. A failed
sample records its typed error and does not abort the study. Tests: grid expansion pinned
by hand; seed derivation byte-identical across runs and different across points and draws;
a two-point, two-draw study over the demo DRM reproduces each sample byte for byte when
re-run alone from its recorded seed and config hash; a deliberately failing point is
recorded, not fatal.

**F2 Aggregation and the study store.** Per-point mean, standard deviation, min, max and
pass fraction per score; pinned against hand-computed values on a fixture; `SweepResults`
written beside the samples. ClickHouse is the architecture's store for MoEs: F2 adds a
writer behind a trait with a file-backed implementation now (the CDM message plus a JSONL
of samples) and the ClickHouse implementation later when a host has it; nothing in the
platform reads the store except through that trait.

**F3 Authoring and the viewer.** `altavista.feasibility`: define a sweep from Python (axes,
draws, seeds), launch `av-sweep`, load results; `POST /api/cdm/sweep` publishes a
`SweepResults`; a feasibility panel in the tiling layout shows the grid with a chosen score
as colour, the per-point distribution across draws, and opens any sample's run in the
existing viewer through its `products_uri`. Headless checks against a real study over the
demo DRM.

**F4 A worked study.** A feasibility question the platform can actually answer with what
exists: drag-sail area versus initial altitude for the two-instance demo, scored on final
radius with the Gates burn dispersion drawn, or attitude-control gains versus star-tracker
noise on pointing error. Documented as a walkthrough with the study's hashes and the
resulting figure.

## Exit

A study declared in YAML, run with one command, reproducible sample by sample from its
hashes and seeds, aggregated, published, and explored in the viewer; every number in the
walkthrough traceable to a run hash.

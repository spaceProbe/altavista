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

## Status (feasibility manager, 2026-09-09)

**F1 and F2 are done and committed on `feasibility`; F3 and F4 are not started.**

Round 1 landed `crates/av-sweep` in three tasks (F1a the pure loader, F1b the executor and
binary, F1c two coverage gaps found in review) and F2 (aggregates and the study store).
Gates at the accepted tree, run in isolation by the manager: `cargo test -p av-sweep` **70
passed** (45 library, 17 binary, 8 GMAT integration), `cargo test --workspace --exclude
av-kernel` **252 passed / 0 failed** (182 baseline plus this crate's 70), clippy clean with
no new `#[allow]`, `cargo deny check` ok, `pytest -q` **436 passed / 3 skipped** (the three
skips are pre-existing opt-in and build-artifact skips under `services/`, unrelated to this
track). `cargo test -p av-kernel` was never run: nothing this track touches is compiled
into it.

### Decisions taken while building, each documented at the code that implements it

- **`SweepSample.config_hash` is the per-sample *configuration* hash, not only the DRM's.**
  `run.proto` calls it "the per-sample DRM hash", but a sweep's axis values land on the
  **SOS** (`SweepAxis.instance` names a `SystemInstance`, whose `parameter_overrides` carry
  the value) and the message has no field for a per-sample SOS hash. It is therefore
  computed over the per-sample DRM, the per-sample SOS and every `SystemDefinition`,
  length-prefixed, which makes it recomputable from the files a sample runs from. Closing
  the gap properly needs a proto field, which this track may not add.
- **The seed key joins the derivation tuple.** The plan says SHA-256 over (base seed, sweep
  hash, point, draw); `Scenario.seeds` is a map, so two keys sharing a base value would
  otherwise derive one substream. The key is folded in, length-prefixed. Two vectors are
  pinned against the system `shasum`, not against our own code.
- **`ExecutionErrorMode::Sampled` iff the sweep declares more than one draw**, `Nominal`
  otherwise. A dispersed single-draw study is therefore not expressible today.
- **A failed sample is a value, not an error**: its record carries the child's exit status
  and captured stderr, its scores stay empty, and the study still writes complete results
  and exits 0. All three failure branches (non-zero exit, no output, undecodable output)
  are tested against a real study driven with a fake child.
- **`ScoreAggregate.std_dev` is the population deviation** (divided by n, not n-1): the
  recorded draws are the whole set of realizations, and one draw gives exactly 0.0 rather
  than a division by zero. `draws` counts the draws that actually contributed, which
  differs from the declared count exactly when a sample failed.
- **The store is a trait from day one.** `FileStudyStore` writes `<root>/<sweep_id>/` with
  the `SweepResults` message, its canonical JSON, and a JSONL of samples; the ClickHouse
  backend is a typed refusal naming the deferral, not a stub that pretends to work. The
  `--out-dir` tree stays the sample workspace; `--store-dir` is the platform's read path.

### The fixture study, measured

`drms/demo_two_instance_sweep.{drm,sweep}.yaml` — new files beside the pinned
`demo_two_instance` family, reusing its SOS and systems unmodified — declare a Gates
execution error on the burn with one seed key and a `demo_flt.spacecraft.DragArea` axis at
5 and 25 m². `DragArea` rather than `Cd` because the controller latches `demo_flt`'s `Cd`
to 220 mid-run, which would erase a `Cd` axis for most of the run. Over the four samples:
the axis moves `demo_flt`'s final radius by **97–101 m**; the burn dispersion moves
`demo_mvr`'s by **1.09–1.18 km**; `demo_flt` feels that dispersion only indirectly, through
the range-latched command, at **0.59 m** and **2.40 m** — small, but not zero, because the
dispersed trajectory crosses the controller's threshold on a different 0.1 s tick. Every
one of the four samples re-runs **byte for byte**, alone, from its own recorded inputs,
with no field exclusions, once its recorded `run_id` is supplied (`run_id` appears in every
nested provenance, not only at the top level).

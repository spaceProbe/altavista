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

### F2b update (feasibility worker, 2026-09-09) — question 192 landed

Question 192's four proto follow-ups ((a) `config_hash` scope, already round 1's own decision
above and unchanged; (b) `SweepSample.seed` → `seeds` map; (c) `SweepAxis` event-value axes; (d)
`ParameterSweep.dispersed`) are implemented in `crates/av-sweep`, on top of round 1's F1/F2.
Full account, per-file changes, the axis-key-collision analysis, break-and-restore evidence, and
measured values are in `crates/av-sweep/REPORT.md`.

**Two round-1 decisions above are now superseded:**
- ~~`SweepSample.seed` is a single projected value~~ — **superseded.** `SweepSample.seed` (field
  4) is `reserved` now; every sample records its full `seeds: map<string, uint64>` (field 10,
  every `Scenario.seeds` key's own derived value), not one projected key. `study.rs`'s
  `projected_seed` helper is deleted, not merely unused.
- ~~`ExecutionErrorMode::Sampled` iff the sweep declares more than one draw, `Nominal`
  otherwise. A dispersed single-draw study is therefore not expressible today.~~ —
  **superseded.** The rule is now `Sampled` when `sweep.dispersed || sweep.monte_carlo_draws >
  1`, else `Nominal` — a dispersed single-draw study is expressible via `dispersed: true`,
  refused with a typed error (`DispersedWithoutSeeds`) when `Scenario.seeds` is empty, the same
  posture as `DrawsAboveOneWithoutSeeds`.

Also new: `SweepAxis` may target a scenario event value (`event_id`/`value_key`, e.g. a
maneuver's `dv_x`) instead of an instance parameter — exactly one target per axis, enforced at
load and in `expand_grid` alike, with the two key spaces (`"{instance}.{parameter}"` vs.
`"event:{event_id}.{value_key}"`) proven disjoint rather than assumed non-colliding (see
`REPORT.md`). The fixture study gained a second axis (event `burn1`'s `dv_x`, bracketing its
20.0 m/s commanded value at 10.0/30.0), widening the grid from 2 to 4 points (8 samples); the
measured effect on `demo_mvr_rmag_at_end` was ~48–50 km between the two `dv_x` extremes, in the
hypothesized direction (larger `dv_x` → larger final radius), of the same order of magnitude as
the order-of-magnitude estimate made before running (~10 km) though larger than that rough
linear-scaling estimate predicted — both the hypothesis and the measured value are recorded in
`REPORT.md`.

Gates at F2b, as accepted (one further defect was found in the manager's own review and fixed
before the commit — `validate_axis_target` tested "both targets" on two *complete* targets, so a
complete parameter target beside a half-declared event target was silently reclassified as an
event axis with an empty `value_key`; the count below includes its regression test): `cargo build
--workspace --all-targets` clean; `cargo test -p av-sweep` **89 passed** (61 library, 18 binary,
10 GMAT integration — up from round 1's 70); `cargo clippy --workspace --all-targets -- -D
warnings` clean, no new `#[allow]`; the golden (`goldens/sweep_results_json/`) regenerated for
the `seeds` map, and its Python oracle test (`tests/test_sweep_results_json.py`) passes.

## Status (feasibility manager, 2026-09-10) — round 2 closed

**F2b, F3 and F4 are done and committed on `feasibility`. Every milestone in this plan has
landed; the Exit criterion above is met.**

Round 2 ran in six tasks: F2b (adopt question 192 in `av-sweep`), F3a and F3b in parallel
(Python authoring plus the server route; the viewer panel), F3c (a defect found in review), and
F4 then F4b (the worked study, and its correction). Gates at the accepted tree, run in isolation
by the manager: `cargo test -p av-sweep` **89 passed**, `cargo test --workspace --exclude
av-kernel` **271 passed / 0 failed** (252 baseline plus this crate's 19 new), `cargo clippy
--workspace --all-targets -- -D warnings` clean with no new `#[allow]`, `cargo deny check` ok,
`pytest -q` **492 passed / 3 skipped** (436 baseline plus this round's 56; the three skips are
the same pre-existing opt-in and build-artifact skips under `services/`). `cargo test -p
av-kernel` was never run: nothing this track touches compiles into it.

### What round 2 changed, beyond the milestones as written

- **Question 192 landed in full.** `SweepSample.seeds` records every derived seed rather than one
  projected value; a `SweepAxis` may target a scenario event value; `ParameterSweep.dispersed`
  makes a single dispersed draw expressible. The two round-1 decisions those supersede are struck
  through in the F2b update above.
- **Two axis-key namespaces, made disjoint rather than assumed disjoint.** Both axis kinds share
  one `SweepSample.axis_values` map, and nothing in `av-kernel` restricts the characters in an
  instance or parameter name — so a parameter axis whose key would begin with the reserved
  `"event:"` prefix is refused, and the disjointness is a proof rather than a convention.
- **Opening a sample is a server route, not a browser fetch.** `products_uri` is a directory on
  the host that ran the study; the panel's first implementation fetched that string from the
  browser and could never have worked. `POST /api/cdm/sweep/sample` takes an identity
  (`sweepId`, `pointIndex`, `drawIndex`) and resolves it against the study the server itself
  published, so no caller-supplied path is ever opened. The `RunProducts`-to-scenario conversion
  is now one shared function, which the unedited run-publish tests prove.
- **The worked study found a modelling assumption before it found a trend.** The manager's own
  design put both of `docs/studies/drag-sail-vs-burn.md`'s axes on `demo_mvr`, which carries no
  drag force at all — `drms/demo_two_instance.sos.yaml`'s own header comment says so. The sweep
  surfaced it as a bit-identical score across the whole axis. The null is kept in the study
  document as its most useful paragraph, and a second grid with the sail on `demo_flt` supplies
  the real numbers.

### The worked study, measured

`docs/studies/drag-sail-vs-burn.md`, two grids of 18 samples each (3 `dv_x` × 2 `DragArea` × 3
draws, `dispersed: true`), 76.5 s and 70.4 s at `--workers 2` — together about a sixth of the
plan's fifteen-minute budget. Burn magnitude moves `demo_mvr`'s final radius by **~2.6 km per
m/s**; drag-sail area moves `demo_flt`'s by **46–140 m** over the 7200 s arc, depending on the
commanded burn. Two of the six axis-to-score pairs are **exactly** zero — bit-identical under a
Nominal check, 0 ULP, not "below a tolerance" — because no coupling path exists at all.

The result that was not predicted: `demo_flt`'s final radius responds to the **burn** axis too,
by up to 117 m, through the controller's range-latched drag-sail command — the burn changes when
`demo_mvr` crosses the latch threshold, which changes how much of the run `demo_flt` spends at
`Cd = 220`. That indirect effect scales with `Cd × DragArea` (a 5.10× ratio measured against a 5×
DragArea ratio), so it is comparable in size to the direct drag effect rather than negligible.
The manager's brief had predicted 0.001–4.2 m for this pair, quoting a round-1 number that
measured a different quantity — draw-to-draw latch jitter at a fixed commanded burn, which this
study also measures separately at 0.04–2.07 m. The two are not the same question, and the study
says so rather than reconciling them.

### Open for the lead

The demo SOS gives drag to `demo_flt` only. A study that genuinely varies drag on the
manoeuvring vehicle needs a new `SosConfiguration` enabling drag on `demo_mvr`; that was outside
this round's authorized file set and is not done.

# Drag-sail area vs. burn magnitude, scored on final radius (F4/F4b)

## What question this study asks, and why the platform can answer it today

Over the two-instance demo mission (`demo_flt` carrying a mid-run gravity fault and (from M19.4
on) a genuine drag force, `demo_mvr` carrying a Gates-dispersed 20 m/s prograde VNB burn, coupled
through a native range-condition controller that latches a drag-sail `Cd` command onto `demo_flt`
once `demo_mvr`'s own range crosses apoapsis), this study asks: **how do the two instances' own
final ranges respond to (a) a drag-sail's deployed area (`spacecraft.DragArea`) and (b) the
magnitude of the commanded burn (`dv_x`), and are those responses of comparable size?**

The platform can answer this today because F1/F2/F2b already give it everything the question
needs: a `ParameterSweep` with a parameter axis and an event axis over one DRM
(`crates/av-sweep`'s grid expansion), a Gates `execution_error` block sampled once per draw
(`ExecutionErrorMode::Sampled`), per-`(point, score)` aggregates (mean/std_dev/min/max/draws),
and an authoring path through `altavista.feasibility` that computes real hashes rather than
hand-typing them. Nothing new was added to the platform to run this study — that is the point of
F4/F4b: exercise the F1–F3 stack on a question chosen for what it *can* answer, not stretch it to
something it cannot.

**This is one study, run as two grids over the same DRM.** Grid 1 (`id: drag_sail_vs_burn`, the
original F4 grid) put both axes — DragArea and dv_x — on `demo_mvr`. It found, and disclosed
rather than hid, that the DragArea axis is physically inert there: `demo_mvr` never declares
`force_model.drag_model`, so no `DragForce` object exists for it to override, and
`demo_mvr_rmag_at_end` came back bit-for-bit identical across DragArea values (see "The
modelling finding" below). Grid 2 (`id: drag_sail_vs_burn_flt`, F4b) reuses the identical DRM,
the identical dv_x axis, and the identical DragArea values, but moves the DragArea axis to
`demo_flt` — the instance that genuinely carries a `DragForce` — so this second grid asks the
question the first grid's own design could not answer: does the platform show a real, resolved
gradient when a physically-connected axis is actually swept, and how does that gradient compare
in size to a burn-magnitude axis?

## Hashes and reproduction

| Artifact | Hash |
|---|---|
| `drms/drag_sail_vs_burn.drm.yaml` (DRM, shared by both grids, unmodified since F4) | `ef8ea78c599e5f529316458e0d1413fcbc925b384da3f8f138edd2fc66be3b93` |
| `drms/drag_sail_vs_burn.sweep.yaml` (ParameterSweep, grid 1 "mvr") | `a67a6bf1be781bda13b9d0c204582cc849ef58d6abeb4b6e3ce26b6bd7689fee` |
| `drms/drag_sail_vs_burn_flt.sweep.yaml` (ParameterSweep, grid 2 "flt") | `2f156ae1063e57f90cd92edd77c60cf509b0a574347cf526474959a8ffcb5b2d` |

All three hashes are read directly off the committed YAML files' own `hash:` fields, and all
three were independently re-verified after this document was written:

```
cargo run -q -p av-kernel --example drm_hash -- drm drms/drag_sail_vs_burn.drm.yaml
  -> ef8ea78c599e5f529316458e0d1413fcbc925b384da3f8f138edd2fc66be3b93
cargo run -q -p av-sweep --example sweep_hash -- drms/drag_sail_vs_burn.sweep.yaml
  -> a67a6bf1be781bda13b9d0c204582cc849ef58d6abeb4b6e3ce26b6bd7689fee
cargo run -q -p av-sweep --example sweep_hash -- drms/drag_sail_vs_burn_flt.sweep.yaml
  -> 2f156ae1063e57f90cd92edd77c60cf509b0a574347cf526474959a8ffcb5b2d
```
(full output at `scratchpad/f4b/` — see the file list at the end of this section).

**Exact commands that reproduce each grid** (authored and launched through
`altavista.feasibility`, not hand-written YAML — `altavista/feasibility/study_drag_sail_vs_burn.py`,
kept in the repository at that path, now authors and can launch either or both grids):

```bash
cd /Users/probe/code/AltaVista-feasibility
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

# Grid 2 ("flt", F4b) -- the script's own default; safe to re-run any time
.venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py --grid flt --out-dir /path/to/out

# Grid 1 ("mvr", F4) -- re-authors and re-runs the ORIGINAL grid; see the note below before using this
.venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py --grid mvr --out-dir /path/to/out

# Both grids in one invocation
.venv/bin/python altavista/feasibility/study_drag_sail_vs_burn.py --grid both --out-dir /path/to/out
```

**Why this document's own reproduction run used `--grid flt` only, not `--grid both`.**
`drms/drag_sail_vs_burn.sweep.yaml` (grid 1) is already committed with a hand-prepended header
comment (see that file's own header, and the note below). `altavista.feasibility.yaml_io.to_yaml`
has no header-comment field to author through (a disclosed gap, not silently patched around), so
re-running grid 1's own `emit_sweep_yaml` call reproduces the identical `hash:` (a YAML comment
is not part of the parsed message) but overwrites the file *without* that header comment. To
avoid stripping a disclosed, already-reviewed artifact, this document's own F4b work ran
`--grid flt` only — grid 1's own file and hash were left untouched (independently re-verified
above) and grid 1's own header comment remains intact. `--grid mvr`/`--grid both` are exercised
by the script and kept working (not dead code), but re-running them will need the header comment
re-prepended by hand afterward, exactly as F4's own worker first noted.

The full stdout of grid 2's actual run is saved at `scratchpad/f4b/study_run_flt.log`; the raw
`sweep_results.pb`/`.json` are under `scratchpad/f4b/study_out/flt/`. Grid 1's own run artifacts
(`study_run.log`, `study_out/`) remain exactly where F4 left them, under `scratchpad/f4/`, and
were not re-produced by this task.

## The `yaml_io` emitter change (F4b)

The previous worker flagged that `altavista/feasibility/yaml_io.py`'s `to_yaml` emitted every
field explicitly, including proto3/Rust zero defaults (`instance: ''`, `min: 0.0`, `steps: 0`, an
all-empty `provenance`), making an emitted sweep far noisier than the hand-written fixtures beside
it — bad enough that F4's own worker had to hand-prepend the header comment as text rather than
route it through the package. This task's brief allowed fixing it if the change is small and
provably hash-neutral.

**Made the change.** `_axis_to_dict`, `_provenance_to_dict`, and `to_yaml_dict`
(`altavista/feasibility/yaml_io.py`) now omit a field when it equals that field's own
proto3/Rust zero default (`""`, `0`, `0.0`, `[]`, `{}`, `false`), instead of always writing it.
This is safe because every one of the three `Raw*` structs on the Rust side
(`crates/av-sweep/src/schema.rs`) is `#[serde(default)]`: an omitted key and an explicit
zero-default value parse to the exact same Rust value either way, and YAML mapping key order
(which the new field ordering also does not preserve verbatim against the hand-written fixture
style) is irrelevant to the parsed struct — so neither omission nor field order can move
`canonical_sweep_hash`, which hashes the *parsed message*, not the YAML text. `provenance:` keeps
its existing, separate treatment (present whenever `sweep.provenance is not None`, even as
`provenance: {}`) — that omission is unrelated to the zero-default one, since `Some(default)` and
`None` are different parsed values.

**Proved, not just argued.** Re-emitted grid 1's own declaration (id `drag_sail_vs_burn`, the
identical `SweepDeclaration` `study_drag_sail_vs_burn.py`'s `build_grid_mvr()` builds) through the
*new* emitter, hashed the result via `compute_sweep_hash`, and compared against the hash already
committed in `drms/drag_sail_vs_burn.sweep.yaml`:

```
NEW EMISSION (hash cleared):
id: drag_sail_vs_burn
drm_id: drag_sail_vs_burn_drm
axes:
- instance: demo_mvr
  parameter: spacecraft.DragArea
  values: [5.0, 25.0]
- values: [10.0, 20.0, 30.0]
  event_id: burn1
  value_key: dv_x
monte_carlo_draws: 3
provenance:
  author_kind: AUTHOR_KIND_AGENT
  tool: F4 drag-sail-vs-burn study authoring
dispersed: true

COMPUTED HASH (new emitter): a67a6bf1be781bda13b9d0c204582cc849ef58d6abeb4b6e3ce26b6bd7689fee
COMMITTED HASH (old emitter): a67a6bf1be781bda13b9d0c204582cc849ef58d6abeb4b6e3ce26b6bd7689fee
MATCH: True
```

(full output at `scratchpad/f4b/emitter_hash_check.log`). The same effect is visible in
`drms/drag_sail_vs_burn_flt.sweep.yaml` itself, emitted through the new code from the start: 21
lines including its own `hash:`, versus grid 1's own 60-odd fully-explicit lines for an axis list
of the same shape. `drms/drag_sail_vs_burn.sweep.yaml` (grid 1) was deliberately **not**
re-emitted with the new code — see "Why this document's own reproduction run used `--grid flt`
only" above — so its committed hash stays exactly what this document already commits to, produced
by the pre-F4b emitter.

## The modelling finding: grid 1's DragArea axis is physically inert on `demo_mvr`

Grid 1 (`drms/drag_sail_vs_burn.sweep.yaml`) deliberately put both axes on `demo_mvr` — the
manager's own original F4 design intent: make `demo_mvr_rmag_at_end` a score that genuinely
responds to both a drag-sail axis and a burn-magnitude axis. `demo_mvr` binds `leo_demo_sys`,
which declares `spacecraft.DragArea` (`drms/demo_two_instance.system.yaml`), so a `DragArea` axis
on `demo_mvr` is a legal override — confirmed against that file before relying on it.

**That legality does not translate into a physical effect, and this was checked, not assumed.**
Reading `crates/av-kernel/src/drm/binding.rs::materialize_gmat` shows a `DragForce` object is
only constructed `if let Some(drag_model) = &spec.drag_model`, and `demo_mvr`'s own
`SystemInstance.parameter_overrides` (`drms/demo_two_instance.sos.yaml`, reused unmodified by
both grids) never set `force_model.drag_model` — only `demo_flt`'s own overrides do. That file's
own header comment says so in as many words: *"`demo_mvr` shares the same base `leo_demo_sys`
SystemDefinition but does NOT declare drag, so its own dynamics are unaffected."* `crate::drm::
binding`'s own pinned test, `no_drag_parameters_at_all_still_classifies_with_drag_model_none`,
names `demo_mvr` explicitly and asserts its classified `drag_model` is `None`. A scratch,
single-draw, **Nominal**-error-mode check (no Monte Carlo noise to hide behind) over this same DRM
with only `demo_mvr.spacecraft.DragArea` swept confirmed this empirically:

| DragArea | `demo_mvr_rmag_at_end` |
|---|---|
| 5.0 m² | 6921427.813187283 m |
| 25.0 m² | 6921427.813187283 m |

Bit-for-bit identical. `spacecraft.DragArea` is stored on the materialized spacecraft object but
never read by anything, because no `DragForce` exists for this instance. **The manager's own
original F4 design intent — "make `demo_mvr_rmag_at_end` a score that genuinely responds to both
axes" — does not hold for the DragArea axis, as the fixture is wired.** This is disclosed here, in
`drms/drag_sail_vs_burn.drm.yaml`'s own header comment, and in
`drms/drag_sail_vs_burn.sweep.yaml`'s own header comment — not silently worked around. Fixing it
*on `demo_mvr` itself* would need a new `SosConfiguration` also declaring
`force_model.drag_model` (and its three weather-source companions) on `demo_mvr`, which was out
of F4's authorized scope. **This is exactly what a sweep is for: it caught a modelling assumption
that "DragArea is a legal parameter on this instance" quietly does not imply "DragArea does
anything on this instance," and it caught it with a null result faithfully aggregated and
rendered, not a crash or a silently-wrong number.** Grid 1's own 18 recorded samples (full
dispersed run, not the scratch check above) confirm the same null result at full scale — see
grid 1's own sensitivity table below. Grid 2 (F4b, this document's own addition) was built
specifically to answer the question this finding leaves open: what does the platform show when
the axis *is* physically connected?

## Grid 1 ("mvr"): grid, draws, and the wall-time budget (written before running)

One sample of this 7200 s scenario measures ~8 s in a debug build (matching
`drms/demo_two_instance_sweep.drm.yaml`'s own measurement of the byte-identical-length scenario).
The existing fixture study (2 axes × 2 values × 2 draws = 8 samples) measured **92.6 s at
`--workers 2`** — an effective ~11.58 s/sample once process-spawn and GMAT-init overhead are
included (2.9× the raw per-sample figure). Grid 1's own grid: 2 (DragArea: 5.0, 25.0 m²) × 3
(dv_x: 10.0, 20.0, 30.0 m/s) = 6 points × 3 draws = **18 samples**. At the fixture's own measured
rate, that predicted **18 × 11.58 s ≈ 208 s (~3.5 min)** — comfortably inside the 15-minute (900 s)
budget. `--workers` was kept at 2, matching the fixture and the contention rule's instruction not
to raise it past what the host can sustain.

**Measured: 76.5 s wall time** (`scratchpad/f4/study_run.log`), well under both the 208 s
prediction and the 900 s budget.

## Grid 1 ("mvr"): the 18 samples — `config_hash` and `seeds`

Every sample's `run_id`, `config_hash` (`av_sweep::sample_config_hash`, recomputable from the
files the sample actually ran from) and `seeds` (the derived `burn_seed` that sample's Gates
dispersion drew from) are below, read directly from `study_out/sweep_results.json`
(`scratchpad/f4/samples_dump.txt`).

| Point | DragArea (m²) | dv_x (m/s) | Draw | `run_id` | `config_hash` | `seeds.burn_seed` |
|---|---|---|---|---|---|---|
| 0 | 5.0 | 10.0 | 0 | `drag_sail_vs_burn_p0_d0` | `a6ba8c6a9d228f72f4abc18f2270a14a1c75e1e9ecd1e43d0c2efc0268fb43f1` | 13721345300552107538 |
| 0 | 5.0 | 10.0 | 1 | `drag_sail_vs_burn_p0_d1` | `cf039bdc76aa370f9b0e9a317e1a236e3de40814bc7fe4784266ceae16d1a262` | 7245439700803818975 |
| 0 | 5.0 | 10.0 | 2 | `drag_sail_vs_burn_p0_d2` | `1943767caf03014093973124268d1bd29bb16781bbafdf795942e8e677f1aa98` | 2129192604356428564 |
| 1 | 5.0 | 20.0 | 0 | `drag_sail_vs_burn_p1_d0` | `6052b88d7ae02b295f5fdb143e9d68f4266f7fc442278b1ac52f7366d348d014` | 18407418108018220619 |
| 1 | 5.0 | 20.0 | 1 | `drag_sail_vs_burn_p1_d1` | `0ae4cb5bbd85d8c5cec29905a8c4a0366b44fa609a7e7e54deca2efbad425e0a` | 2869882895847353058 |
| 1 | 5.0 | 20.0 | 2 | `drag_sail_vs_burn_p1_d2` | `a437e2df8e985d0df7473b5d7885b64883bbcabc5d3868b3acbce7ec2141484d` | 9884417388065724522 |
| 2 | 5.0 | 30.0 | 0 | `drag_sail_vs_burn_p2_d0` | `6ec60ee973a17a7fe06ea4f81b436455d0d889f14100fc1044b3d5b7997d58b6` | 4101482119781638746 |
| 2 | 5.0 | 30.0 | 1 | `drag_sail_vs_burn_p2_d1` | `4e6c6b33daec11777565881250ce482bfb37417c05d8f558003b8939ba0a2ef0` | 8245807236767601188 |
| 2 | 5.0 | 30.0 | 2 | `drag_sail_vs_burn_p2_d2` | `758dc94d1188b5ee7f84c5d248ad7a06f3afbd12fdbf4d4d395821eadc2c71d2` | 737853999526853903 |
| 3 | 25.0 | 10.0 | 0 | `drag_sail_vs_burn_p3_d0` | `87570546e7bfc0a0a3fe9393cce27ef8c6000c218bf1964da495daa941486915` | 3579443801850573144 |
| 3 | 25.0 | 10.0 | 1 | `drag_sail_vs_burn_p3_d1` | `c3814346e86a6cf57408c7c7614c04cff87b463285ee5c5d00b94f282d3e9e30` | 10605511264294573518 |
| 3 | 25.0 | 10.0 | 2 | `drag_sail_vs_burn_p3_d2` | `ab9fd123a5979f293348d999c4bec7a6829ab3b723da8159e1dd5df67f82594f` | 15711151439596470015 |
| 4 | 25.0 | 20.0 | 0 | `drag_sail_vs_burn_p4_d0` | `fb834d77d666baed03532021d1c6df3191bc5c1014ab4ecf62cfb759f24a217d` | 8747275218291753807 |
| 4 | 25.0 | 20.0 | 1 | `drag_sail_vs_burn_p4_d1` | `e23210db3df452d749c33fc6369011edff2d87f557f907f9e6bbc9444c6e38f2` | 17445446468653712708 |
| 4 | 25.0 | 20.0 | 2 | `drag_sail_vs_burn_p4_d2` | `2b3b862cee0a508bf8b2e1485da7fada50e96a6cfbabe5ad461c8b11dc789d9a` | 2904949988003337326 |
| 5 | 25.0 | 30.0 | 0 | `drag_sail_vs_burn_p5_d0` | `58bcf3fc108442997d33dfaa5a97be478d6ad1f502077d36fb49a9680c4f4fd9` | 1361182567822591909 |
| 5 | 25.0 | 30.0 | 1 | `drag_sail_vs_burn_p5_d1` | `5ecffc120d31a271d168f8346157658c194252236c7652bd2e0dfe1b843ee1e7` | 430728635156905114 |
| 5 | 25.0 | 30.0 | 2 | `drag_sail_vs_burn_p5_d2` | `fd21035e79dd5cb62a7891f3344d21ba97999b9e703259ea7533eb2c536cb618` | 10290368603235882033 |

All 18 samples succeeded (`error: ""` on every one).

## Grid 1 ("mvr"): aggregate table

`demo_flt_cd_at_end` is dimensionless; `demo_flt_rmag_at_end` and `demo_mvr_rmag_at_end` are
metres. Read from `study_out/sweep_results.json`'s own `aggregates`.

| Point | DragArea | dv_x | Score | Mean | Std dev | Min | Max | Draws |
|---|---|---|---|---|---|---|---|---|
| 0 | 5.0 | 10.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 0 | 5.0 | 10.0 | demo_flt_rmag_at_end (m) | 6870530.203398 | 0.170236 | 6870530.015565 | 6870530.427735 | 3 |
| 0 | 5.0 | 10.0 | demo_mvr_rmag_at_end (m) | 6895929.895712 | 375.677203 | 6895432.632132 | 6896340.535959 | 3 |
| 1 | 5.0 | 20.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 1 | 5.0 | 20.0 | demo_flt_rmag_at_end (m) | 6870517.150178 | 0.703340 | 6870516.591639 | 6870518.142229 | 3 |
| 1 | 5.0 | 20.0 | demo_mvr_rmag_at_end (m) | 6922144.210902 | 1580.363552 | 6919910.814847 | 6923333.572390 | 3 |
| 2 | 5.0 | 30.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 2 | 5.0 | 30.0 | demo_flt_rmag_at_end (m) | 6870507.026339 | 0.753756 | 6870506.135137 | 6870507.978439 | 3 |
| 2 | 5.0 | 30.0 | demo_mvr_rmag_at_end (m) | 6947572.953604 | 2137.056908 | 6944944.465867 | 6950179.020798 | 3 |
| 3 | 25.0 | 10.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 3 | 25.0 | 10.0 | demo_flt_rmag_at_end (m) | 6870530.086477 | 0.254260 | 6870529.809385 | 6870530.423485 | 3 |
| 3 | 25.0 | 10.0 | demo_mvr_rmag_at_end (m) | 6896178.216809 | 574.722869 | 6895410.166587 | 6896792.532405 | 3 |
| 4 | 25.0 | 20.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 4 | 25.0 | 20.0 | demo_flt_rmag_at_end (m) | 6870517.651218 | 0.163741 | 6870517.420593 | 6870517.784587 | 3 |
| 4 | 25.0 | 20.0 | demo_mvr_rmag_at_end (m) | 6921088.316993 | 299.973467 | 6920876.184992 | 6921512.543538 | 3 |
| 5 | 25.0 | 30.0 | demo_flt_cd_at_end | 220.0 | 0.0 | 220.0 | 220.0 | 3 |
| 5 | 25.0 | 30.0 | demo_flt_rmag_at_end (m) | 6870506.639358 | 0.603153 | 6870505.796041 | 6870507.171937 | 3 |
| 5 | 25.0 | 30.0 | demo_mvr_rmag_at_end (m) | 6948368.263970 | 1707.704484 | 6946890.530520 | 6950761.400865 | 3 |

`demo_flt_cd_at_end` is exactly 220.0 with zero spread everywhere: the controller's latched
command is a fixed value once it fires, so this is a deterministic cross-check that the run
executed as designed, not a measured effect.

## Grid 2 ("flt", F4b): axis choice and physical justification

Grid 2 keeps the DRM, the dv_x axis, and the DragArea *values* identical to grid 1, changing only
which instance the DragArea axis targets — from `demo_mvr` (inert) to `demo_flt` (genuinely
carries a `DragForce`: JacchiaRoberts + the CSSI weather-source trio,
`drms/demo_two_instance.sos.yaml`'s own `parameter_overrides` on `demo_flt`).

- **DragArea axis: `demo_flt.spacecraft.DragArea`, [5.0, 25.0] m²** — identical to grid 1's own
  values, not chosen to make the figure look better. 5.0 m² is `leo_demo_sys`'s own declared
  baseline (`drms/demo_two_instance.system.yaml`); 25.0 m² is a deployed drag sail, a 5× increase
  — the same range `demo_two_instance_sweep.sweep.yaml` (F1b/F2b's own fixture) already uses on
  this same instance. Physically, a ~500 kg smallsat (`demo_two_instance.system.yaml`'s own
  `DryMass`) with a compact undeployed cross-section (5 m²) and a modestly-sized deployed
  drag/deorbit sail (25 m²) sits in the range of flown small-sail deorbit devices (a few m² to a
  few tens of m²), not a fictional number. Keeping the identical range as grid 1 (rather than
  picking a new one) makes the two grids' own DragArea results directly comparable, and matches
  `demo_two_instance.sos.yaml`'s own header comment's warning about the `Cd` latch: `demo_ctrl`
  latches `demo_flt`'s `Cd` to 220.0 partway through the run (edge-triggered, once), which is
  exactly why F1b/F2b and grid 1 both chose a `DragArea` axis over a `Cd` axis in the first place
  — a `Cd` axis's own declared value would be partly overwritten by that command for whatever
  fraction of the run remains after the latch fires, while `DragArea` has no such command path
  anywhere in this SOS.
- **dv_x axis: unchanged** — event `burn1`'s `dv_x`, [10.0, 20.0, 30.0] m/s, identical to grid 1.
- **`dispersed: true`, `monte_carlo_draws=3`** — identical to grid 1, so the two grids are
  directly comparable (same draw count, same Gates sigmas, same DRM).

## Grid 2 ("flt"): grid, draws, and the wall-time budget

**Budget arithmetic, written before running.** Grid 2 is the identical size as grid 1: 2
(DragArea) × 3 (dv_x) = 6 points × 3 draws = **18 samples**. Grid 1 (this same DRM, same host,
same day, same debug binary, `--workers 2`) measured **76.5 s** for its own 18 samples — an
effective ~4.25 s/sample. Grid 2 reuses the identical binary, DRM, SOS, and system files (only
the sweep axes differ) and was run shortly after grid 1's own physics pre-check, so caches were
expected to still be warm: the **primary estimate** was grid 1's own rate, 18 × 4.25 s ≈ 77 s. As
a **conservative upper bound**, the F1b/F2b fixture's own slower cold-cache rate (11.58 s/sample)
gives 18 × 11.58 s ≈ 208 s. `--workers` was kept at 2, matching both prior studies and the
contention rule. Even the conservative estimate leaves >4× headroom inside the 900 s budget for
grid 2 alone; combined with grid 1's own already-measured 76.5 s, the conservative *whole-of-F4*
estimate was 76.5 + 208 = 284.5 s (~4.7 min), comfortably under 900 s.

**Measured: 70.4 s wall time** (`scratchpad/f4b/study_run_flt.log`), matching the primary (~77 s)
estimate closely and well under the conservative one. **Combined F4 wall time (both grids):
76.5 + 70.4 = 146.9 s (~2.45 min)** — under the 15-minute budget with roughly 6.1× headroom.

Before running, the contention rule was honoured strictly: `ps -Ao pid,etime,command | grep -E
"cargo test|pytest|docker build"` was checked immediately before every heavy run. The other
team's `cargo test -p av-kernel` (in the sibling `develop` worktree, confirmed via `lsof -p <pid>
| grep cwd`) was in flight continuously from this task's start until it finally cleared roughly
1 hour 44 minutes later; grid 2's own run did not start until `ps` returned no match. A second,
shorter contention window (`cargo test -p av-lockstep --test docker_lifecycle` plus two
`docker build`s, also in the sibling worktree) was waited out the same way immediately
afterward, before the Nominal-mode isolation checks below.

## Grid 2 ("flt"): the 18 samples — `config_hash` and `seeds`

Read directly from `study_out/flt/sweep_results.json`.

| Point | DragArea (m²) | dv_x (m/s) | Draw | `run_id` | `config_hash` | `seeds.burn_seed` |
|---|---|---|---|---|---|---|
| 0 | 5.0 | 10.0 | 0 | `drag_sail_vs_burn_flt_p0_d0` | `5154e884da6a0a3b27ac7a4c28249ec1327f673c8fd874e1b30ddf40db5cb0cb` | 7534726643450577392 |
| 0 | 5.0 | 10.0 | 1 | `drag_sail_vs_burn_flt_p0_d1` | `5be4ea45030a086d4fc7aa2efcb40db25d5ced862561f2a1098dc95632ce26db` | 1224459692950000738 |
| 0 | 5.0 | 10.0 | 2 | `drag_sail_vs_burn_flt_p0_d2` | `3f39c16834bb420f68d2aee1af77d53d3234c39b822255fb69e0162491bfe9cd` | 1348724119079212845 |
| 1 | 5.0 | 20.0 | 0 | `drag_sail_vs_burn_flt_p1_d0` | `beea6062ca87560f64470d9829ca596f8152412088b2df4c94c6497780825498` | 16408952183205839296 |
| 1 | 5.0 | 20.0 | 1 | `drag_sail_vs_burn_flt_p1_d1` | `8780cad56fcdebfd93b22753720f97885bda567a1021d09ae6bf19095e78a718` | 4087367816510234084 |
| 1 | 5.0 | 20.0 | 2 | `drag_sail_vs_burn_flt_p1_d2` | `2bc9b80e216c2a3688d7dc14da30f0560e27b191c821536f45e4e4d0b4e6215c` | 1360008931580311298 |
| 2 | 5.0 | 30.0 | 0 | `drag_sail_vs_burn_flt_p2_d0` | `9ac9afd6a84754492a0a0f78bee1a711e103003d43a74812a4531cc157009532` | 7931971142697650914 |
| 2 | 5.0 | 30.0 | 1 | `drag_sail_vs_burn_flt_p2_d1` | `877353a3e49416f7d52ee25bf49c6263200b67059a7d6ee1d5201283f00f3a2a` | 12119054785498574052 |
| 2 | 5.0 | 30.0 | 2 | `drag_sail_vs_burn_flt_p2_d2` | `5dacffbb2d72aa09749c7c9a06ed2522405999f7534fab4cd531f0da57c2eae2` | 6231952597820482359 |
| 3 | 25.0 | 10.0 | 0 | `drag_sail_vs_burn_flt_p3_d0` | `53d898bf6011d3cb32f5f60b6e275923295622d533e98bad26cb0230b2608f21` | 10564210977899396122 |
| 3 | 25.0 | 10.0 | 1 | `drag_sail_vs_burn_flt_p3_d1` | `722dc19279ea46c80b937b37345e4bb7b9ec0a3b345c8c5dce1934f42f570c1a` | 8141303163764437782 |
| 3 | 25.0 | 10.0 | 2 | `drag_sail_vs_burn_flt_p3_d2` | `9ea5c438d55727ecfe56198dab90f008f016b72683beb4a9ea4a425d4812a1d7` | 6179307082162677319 |
| 4 | 25.0 | 20.0 | 0 | `drag_sail_vs_burn_flt_p4_d0` | `657331528acc5aad55422386bec8ad43a2b1312b6b21841b77288ef6e4f9cb40` | 1793070021646649482 |
| 4 | 25.0 | 20.0 | 1 | `drag_sail_vs_burn_flt_p4_d1` | `1870f4fc031db2da2d4413f273d5d0c98ea9b3fb124affaea5c59cd7df3dc84f` | 13345712012390990450 |
| 4 | 25.0 | 20.0 | 2 | `drag_sail_vs_burn_flt_p4_d2` | `e881b758026217829720fa097495b9ae09c14b6d4e1b9f1f0c315f72d42df380` | 399115272785753228 |
| 5 | 25.0 | 30.0 | 0 | `drag_sail_vs_burn_flt_p5_d0` | `abe2e6d17c04fdd062ac2801b6a6d9726f2464a6b739cf3b9ac778b08a88894a` | 6811975260980584480 |
| 5 | 25.0 | 30.0 | 1 | `drag_sail_vs_burn_flt_p5_d1` | `3f15b53636aac849e6bebce6aa04cf65b542d1a69c19478c361a7bda68a4143c` | 11820510842677628012 |
| 5 | 25.0 | 30.0 | 2 | `drag_sail_vs_burn_flt_p5_d2` | `84522e70e34580a7422a1c6e3aa2cb457be88be04385a8e86ff1aab0ffbb3271` | 15495018144487334996 |

All 18 samples succeeded (`error: ""` on every one).

## Grid 2 ("flt"): aggregate table

| Point | DragArea | dv_x | Score | Mean | Std dev | Min | Max | Draws |
|---|---|---|---|---|---|---|---|---|
| 0 | 5.0 | 10.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 0 | 5.0 | 10.0 | demo_flt_rmag_at_end (m) | 6870530.096755 | 0.223386 | 6870529.789552 | 6870530.314164 | 3 |
| 0 | 5.0 | 10.0 | demo_mvr_rmag_at_end (m) | 6896155.887461 | 494.861100 | 6895679.770057 | 6896838.148173 | 3 |
| 1 | 5.0 | 20.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 1 | 5.0 | 20.0 | demo_flt_rmag_at_end (m) | 6870517.570394 | 0.038563 | 6870517.535680 | 6870517.624178 | 3 |
| 1 | 5.0 | 20.0 | demo_mvr_rmag_at_end (m) | 6921306.316815 | 69.291483 | 6921239.692985 | 6921401.861466 | 3 |
| 2 | 5.0 | 30.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 2 | 5.0 | 30.0 | demo_flt_rmag_at_end (m) | 6870507.081269 | 0.334170 | 6870506.711408 | 6870507.520967 | 3 |
| 2 | 5.0 | 30.0 | demo_mvr_rmag_at_end (m) | 6947159.756652 | 804.396512 | 6946078.658478 | 6948006.901035 | 3 |
| 3 | 25.0 | 10.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 3 | 25.0 | 10.0 | demo_flt_rmag_at_end (m) | 6870483.829283 | 0.754283 | 6870482.831897 | 6870484.655587 | 3 |
| 3 | 25.0 | 10.0 | demo_mvr_rmag_at_end (m) | 6895595.514060 | 356.100068 | 6895189.757555 | 6896056.717944 | 3 |
| 4 | 25.0 | 20.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 4 | 25.0 | 20.0 | demo_flt_rmag_at_end (m) | 6870417.365577 | 2.067484 | 6870415.456100 | 6870420.237908 | 3 |
| 4 | 25.0 | 20.0 | demo_mvr_rmag_at_end (m) | 6922353.318252 | 948.695888 | 6921050.401715 | 6923281.980783 | 3 |
| 5 | 25.0 | 30.0 | demo_flt_cd_at_end | 220.000000 | 0.000000 | 220.000000 | 220.000000 | 3 |
| 5 | 25.0 | 30.0 | demo_flt_rmag_at_end (m) | 6870366.610903 | 1.889199 | 6870364.430106 | 6870369.037977 | 3 |
| 5 | 25.0 | 30.0 | demo_mvr_rmag_at_end (m) | 6947802.253907 | 1125.627737 | 6946463.551075 | 6949217.569630 | 3 |

(read from `study_out/flt/sweep_results.json`'s own `aggregates`; full recomputation script and
its output at `scratchpad/f4b/compute_sensitivities.py`/`.log`).

## Nominal-mode isolation checks (grid 2)

The dispersed 18-sample grid above confounds each axis's own effect with per-point Gates
dispersion noise (every grid point draws its own independent burn-execution-error samples). To
get clean, deterministic numbers — and to prove the "exactly zero" claim below rigorously rather
than merely "small within noise" — two single-axis, single-draw, **Nominal**-error-mode scratch
checks were run (mirroring grid 1's own DRM-header methodology), varying one axis at a time over
the same DRM with `dispersed: false`, `monte_carlo_draws: 1`:

**Check A — DragArea axis only** (`demo_flt.spacecraft.DragArea` ∈ {5.0, 25.0}, dv_x left at the
DRM's own commanded default, 20.0 m/s):

| DragArea | `demo_mvr_rmag_at_end` | `demo_flt_rmag_at_end` |
|---|---|---|
| 5.0 m² | 6921427.813187283 m | 6870517.4886757415 m |
| 25.0 m² | 6921427.813187283 m | 6870419.508094786 m |

`demo_mvr_rmag_at_end` is bit-for-bit identical between the two DragArea values — the same
6921427.813187283 m grid 1's own Nominal check produced (expected: same Nominal-mode, same
dv_x=20.0 default, and `demo_mvr`'s own trajectory has no channel to demo_flt's DragArea either
way). `demo_flt_rmag_at_end` differs by **−97.980581 m** (25 m² decaying the orbit faster than
5 m²), matching the order of magnitude `crates/av-sweep/tests/fixture_study.rs`'s own prior
measurement on this instance/axis pair (~97–101 m) and landing above this task's own ~47 m
manager-brief figure but within the same order of magnitude.

**Check B — dv_x axis only** (event `burn1`'s `dv_x` ∈ {10.0, 20.0, 30.0}, DragArea left at
`demo_flt`'s own base `leo_demo_sys` default, 5.0 m², unoverridden):

| dv_x | `demo_mvr_rmag_at_end` | `demo_flt_rmag_at_end` |
|---|---|---|
| 10.0 m/s | 6895961.952518236 m | 6870530.186549683 m |
| 20.0 m/s | 6921427.813187283 m | 6870517.4886757415 m |
| 30.0 m/s | 6946880.954451178 m | 6870507.187184866 m |

`demo_mvr_rmag_at_end` moves **50919.00 m (~50.9 km)** across the full bracket (slopes ~2546–2547
m/(m/s)), closely matching grid 1's own measured ~2.6 km/(m/s), ~52 km — as expected, since
`demo_mvr`'s own dynamics do not depend on `demo_flt`'s DragArea. `demo_flt_rmag_at_end` moves
**−22.999365 m** across the same bracket at DragArea=5.0 — this is the *indirect*,
controller-latch-timing-mediated coupling, isolated cleanly from any DragArea confound; see the
sensitivity analysis below for why this number is materially larger, and DragArea-dependent, once
measured on the full dispersed grid.

Both checks' raw output is saved at `scratchpad/f4b/scratch_isolation_checks.log`; the authoring
script is `scratchpad/f4b/scratch_isolation_checks.py` (not a study deliverable — a scratch
check, exactly as grid 1's own DRM header used the identical method, not committed to `drms/`).

## Sensitivity analysis: hypothesis vs. measured, all six axis-to-score pairs

Two instances × two axes gives four axis-to-score pairs where a coupling path could exist, plus
two where an instance's own axis is trivially reflected in its own score (`dv_x`→`demo_mvr`
across both grids is one physical relationship, measured twice). All six are reported, including
the near-zero ones, and each near-zero result states plainly whether it is **exactly zero** (a
bit-for-bit identical value under a Nominal, undispersed re-run — no floating-point residual at
all, 0 ULP difference, not "below some tolerance") or **merely small** (a nonzero, resolved value,
small only by comparison).

**1. `demo_mvr_rmag_at_end` vs. DragArea-on-`demo_mvr` (grid 1) — hypothesis: exactly 0 m, no
physical channel exists.** Confirmed exactly zero: the clean Nominal check above is bit-for-bit
identical (6921427.813187283 m at both 5.0 and 25.0 m²), and `crate::drm::binding::
materialize_gmat` structurally never constructs a `DragForce` for this instance. The full-scale
dispersed grid is *consistent* with this (differences of +248, −1056, +795 m at the three dv_x
points, average −4.09 m, no consistent sign, each within ~1–2 standard errors of its own draw
noise — see grid 1's own sensitivity table above) but, being dispersed, cannot by itself prove
exactly zero the way the Nominal check does.

**2. `demo_mvr_rmag_at_end` vs. dv_x-on-`demo_mvr` (grid 1) — hypothesis: ~2.4–2.5 km/(m/s),
monotonic.** Confirmed real and resolved: measured slopes cluster around ~2.6 km/(m/s) (six
segment slopes, 2491–2728 m/(m/s)), ~51.6–52.2 km total across the bracket — see grid 1's own
sensitivity table for the full breakdown.

**3. `demo_mvr_rmag_at_end` vs. DragArea-on-`demo_flt` (grid 2) — hypothesis: exactly 0 m, an
even stronger structural null than pair 1.** **Confirmed exactly zero, and confirmed for a
different, stronger reason than pair 1.** Pair 1's null holds because DragArea is declared on the
scored instance itself but unread by any force object; this pair's null holds because DragArea is
not even *declared* on the scored instance (`demo_mvr` never carries a `demo_flt`-targeted
override), and the port wiring between the two instances is strictly one-directional
(`demo_mvr --(cd_cmd_out, rmag)--> demo_ctrl --(cd_sail_cmd_out, Cd)--> demo_flt`) — nothing
`demo_flt` does is ever consumed by `demo_mvr` or `demo_ctrl`. Check A above is bit-for-bit
identical (6921427.813187283 m at both DragArea values, and identical to pair 1's own Nominal
value — expected, since neither DragArea override reaches `demo_mvr`'s own force model by any
path). The dispersed grid's own three point-pair differences (−560, +1047, +642 m, average
+376.4 m) are all within 0.8–1.9 standard errors of zero (`scratchpad/f4b/compute_sensitivities.log`)
— noisier than pair 1's own dispersed check simply because this grid's dv_x=20 point happened to
draw higher-variance samples, not because the underlying coupling is any less than exactly zero.

**4. `demo_mvr_rmag_at_end` vs. dv_x-on-`demo_mvr` (grid 2) — hypothesis: reproduces grid 1's own
~2.6 km/(m/s), since `demo_flt`'s DragArea cannot reach `demo_mvr`.** **Confirmed.** Check B
above (Nominal, DragArea fixed) measures 50919.00 m total, slopes 2546–2547 m/(m/s). The full
dispersed grid measures 51003.87 m (DragArea=5 row) and 52206.74 m (DragArea=25 row) — both
within ~2.5% of grid 1's own ~51.6–52.2 km and of each other, confirming DragArea-on-`demo_flt`
does not perturb this relationship.

**5. `demo_flt_rmag_at_end` vs. DragArea-on-`demo_flt` (grid 2, direct) — hypothesis: a real,
monotonic, tens-of-metres effect.** **Confirmed.** Check A above measures −97.980581 m
(dv_x=20 fixed). On the full dispersed grid, the same comparison at each of the three dv_x values
gives −46.27 m (dv_x=10), −100.20 m (dv_x=20, matching Check A closely), −140.47 m (dv_x=30) —
real, monotonic (larger DragArea always gives smaller final rmag), and enormous relative to each
point's own draw-to-draw std_dev (0.04–2.07 m): the smallest of these differences (46.27 m at
dv_x=10) is still ~100× the largest relevant std_dev, so none of the three is remotely explainable
as draw noise.

**6. `demo_flt_rmag_at_end` vs. dv_x-on-`demo_mvr` (grid 2, indirect) — hypothesis (this task's
own manager brief): weak, ~0.001–4.2 m, "small but never zero."** **The measured axis-level
effect is real, resolved, and materially larger than that figure — an honest miss against the
hypothesis's own magnitude, disclosed rather than smoothed over, with a root cause identified.**
Check B (Nominal, DragArea=5.0 fixed) measures −22.999365 m across the full dv_x bracket. On the
dispersed grid, the same comparison gives −23.02 m at DragArea=5.0 (matching Check B almost
exactly) but **−117.22 m at DragArea=25.0** — comparable in size to, and at DragArea=25 actually
*larger* than, pair 5's own direct DragArea effect at the same DragArea row (−140.47 m end to end,
of which −117.22 m of the *change across dv_x* comes from this indirect path alone). **Root
cause, checked against the mechanism, not merely observed:** the coupling path is real (dv_x
changes `demo_mvr`'s own trajectory shape, which changes *when* its rmag output crosses
`demo_ctrl`'s 6,884,300 m threshold, which changes how much of the remaining ~1763 s of the run
`demo_flt` spends at the post-latch `Cd=220` versus the pre-latch `Cd=2.2`), and the resulting
decay differential scales with `Cd × DragArea` — at DragArea=25 the post-latch drag force is 5×
larger than at DragArea=5, so the identical latch-timing shift compounds a ~5× larger rmag
differential (−117.22 m vs. −22.999 m is a 5.10× ratio, matching the 5× DragArea ratio closely).
**This task's own manager-brief figure (0.001–4.2 m) measured a genuinely different quantity: the
`fixture_study.rs` test it traces to (`the_controller_latch_coupling_makes_demo_flt_weakly_but_
not_exactly_draw_sensitive`) compares two Gates-dispersed *draws at the same commanded dv_x*
(latch-timing jitter from burn-execution noise, holding the axis fixed), not two different
*commanded* dv_x values on a deliberately large ±10 m/s axis.** This study's own grid does measure
that same per-draw quantity separately — see below — and it lands in the same small range the
manager brief describes; the two numbers answer different questions and should not be conflated.

**Draw-to-draw (Gates dispersion) spread, for comparison against pair 6's own hypothesis figure.**
Per-point `demo_flt_rmag_at_end` std_dev across the 3 draws: 0.223, 0.039, 0.334, 0.754, 2.067,
1.889 m (points 0–5). This is the *same kind of quantity* the 0.001–4.2 m manager-brief figure
describes (draw-to-draw latch-timing jitter at fixed commanded dv_x) and lands in the same small
range — small, genuinely resolved above float noise, and two to four orders of magnitude below
both direct axis effects (pairs 5 and the axis-level part of 6).

**Summary.** Grid 2 is *not* the cleanly separable grid the manager brief predicted for
`demo_flt_rmag_at_end`: that score responds strongly to DragArea (pair 5, tens to ~140 m) *and*
strongly to dv_x through the indirect latch-timing path (pair 6, up to ~117 m at DragArea=25) —
the two effects are comparable in size, and the indirect one is DragArea-dependent, not a fixed
"small" quantity. `demo_mvr_rmag_at_end`, by contrast, *is* cleanly separable exactly as
predicted: it responds only to dv_x (pairs 2 and 4, tens of km) and not at all to either
instance's DragArea (pairs 1 and 3, both exactly zero under a clean Nominal check). Both
structures — one score genuinely coupled to both axes, one score cleanly separable — are real,
platform-measured findings from the same two grids, reported in full rather than edited to match
the pre-run prediction.

## What the figure shows (viewer panel, browser check)

### Grid 1 (unchanged from F4)

The study was published through the real `POST /api/cdm/sweep` route on a server started from
this worktree (`cd /Users/probe/code/AltaVista-feasibility && .venv/bin/python -m altavista
serve --host 127.0.0.1 --port 8811`; `lsof -p <pid> | grep cwd` confirmed the process's own
working directory was this worktree). Opened in a real browser at `http://127.0.0.1:8811/`,
swapping a pane to the "Feasibility Study" panel and setting "Colour by" to
`demo_mvr_rmag_at_end`: the grid renders as a 2×3 table, DragArea (5, 25) as rows and dv_x
(10, 20, 30) as columns. The colour ramp ran almost entirely left-to-right, across the dv_x
columns, and was visually indistinguishable between the two DragArea rows at each column —
dv_x drives the colour, DragArea does not move it. Clicking a grid cell and then "Open this
sample's run" issued the real `POST /api/cdm/sweep/sample`, loading that sample's own
`RunProducts` into the viewport with matching scores.

### Grid 2 (F4b, new)

The same server (still running from this worktree — re-confirmed via `lsof -p <pid> | grep cwd`
immediately before publishing) was used to `POST /api/cdm/sweep` grid 2's own
`sweep_results.json`. The server's own log recorded `published SweepResults 'drag_sail_vs_burn_flt'
(18 samples, 18 aggregates, sweep_hash 2f156ae106…)`, and the publish response was
`{"ok":true,"name":"sweep:drag_sail_vs_burn_flt","clients":1}` (`scratchpad/f4b/server.log`,
`scratchpad/f4b/publish_response.json`).

Opened in the real Browser MCP tab already pointed at that origin, the "Feasibility Study" panel
showed the same 2×3 grid shape as grid 1. Reading the rendered cells' own text and computed
background colour directly from the DOM (`getComputedStyle(td).backgroundColor`, via the
browser's own JavaScript console — used here only to read out precisely what the panel had
already rendered, not to change anything):

- **Colour by `demo_mvr_rmag_at_end`:** row `DragArea=5` → `rgb(21,71,121)` (dv_x=10, 6.8962e6 m),
  `rgb(108,117,127)` (dv_x=20, 6.9213e6 m), `rgb(121,58,21)` (dv_x=30, 6.9472e6 m); row
  `DragArea=25` → the *identical three colours in the identical order* (`rgb(21,71,121)`,
  `rgb(108,117,127)`, `rgb(121,58,21)`) at 6.8956e6/6.9224e6/6.9478e6 m. The colour runs purely
  left-to-right across dv_x and is pixel-identical between the two DragArea rows — a direct,
  precise (not eyeballed) confirmation of pair 3/4's own finding: this score responds to dv_x and
  not at all to DragArea.
- **Colour by `demo_flt_rmag_at_end`:** row `DragArea=5` (values 6870530.10, 6870517.57,
  6870507.08 m, a 23 m internal spread) rendered as three cells sharing one colour,
  `rgb(121,58,21)` — the panel's colour legend is quantized into a small number of discrete bins
  (each cell carries a `class="av-heat-cell av-heat-N"`, `N` naming the bin), and a 23 m spread
  did not cross a bin boundary against this grid's own full six-cell range (~164 m). Row
  `DragArea=25` (6870483.83, 6870417.37, 6870366.61 m, a 117 m internal spread) rendered as three
  *visibly distinct* colours — `rgb(134,80,39)`, `rgb(66,95,138)`, `rgb(21,71,121)` — because that
  larger internal spread does cross multiple bins. This is an honest, precise readout of a
  quantized colour legend, not a claim that the panel shows a smooth gradient for this score; the
  underlying numbers (pairs 5 and 6 above) are what carries the real finding.

**Selecting a grid point and opening a sample.** The panel's own grid cells and "Open this
sample's run" buttons are plain DOM elements (a `<td>` and a `<button>`, both genuinely
clickable — confirmed via `document.elementFromPoint` landing on exactly the intended element,
no overlay). The Browser MCP tool's own coordinate- and ref-based `left_click` did not trigger
either element's click handler in this session, for reasons not fully diagnosed (the click
report showed a coordinate/element that `elementFromPoint` also confirmed as correct, so this
looks like an automation-tool gap in this session rather than an application bug). **Disclosed
plainly, as this task's brief asks:** the panel was still driven through the real browser and the
real API, but grid-point selection and "Open this sample's run" were triggered by dispatching a
genuine `pointerdown`/`mousedown`/`pointerup`/`mouseup`/`click` `MouseEvent` sequence at the
element's own on-screen centre via the browser's JavaScript console, rather than through the
click-automation tool directly — the same event sequence a real click produces, just injected one
layer closer to the DOM. Selecting point 5 (DragArea=25, dv_x=30) populated "Distribution across
draws" with its three individual draws — `drag_sail_vs_burn_flt_p5_d0` (6.9465e6 m),
`_p5_d1` (6.9477e6 m), `_p5_d2` (6.9492e6 m), matching the aggregate table's own min/mean/max for
point 5 (6946463.55 / 6947802.25 / 6949217.57 m). Opening draw 0's "Open this sample's run"
issued the real `POST /api/cdm/sweep/sample` (`{"sweepId":"drag_sail_vs_burn_flt","pointIndex":5,
"drawIndex":0}`) — the server log recorded `opened feasibility sample sweep='drag_sail_vs_burn_flt'
point=5 draw=0 -> run 'run:drag_sail_vs_burn_flt_p5_d0'`
(`scratchpad/f4b/server.log`, line 78) — and the panel loaded that sample's own `RunProducts`:
`config hash` `64001811aacf0f5d4bd0a37321881ecfafd11275f97457c5745c1e133be4b9c2` (matching
`av-sweep --run-sample`'s own printed `config_hash` from the byte-for-byte re-run below exactly),
`Objectives & measures` showing `demo_flt_cd_at_end` 220.000, `demo_flt_rmag_at_end` 6.8704e6 m,
`demo_mvr_rmag_at_end` 6.9465e6 m — matching point 5 draw 0's own recorded value
(6946463.551074704 m) to the precision the panel displays. `Port commands & faults` showed the
`fault1` event at 25.0% of the run's own timeline and the `Cd` port-command at 84.0%, consistent
with this draw's own dispersed burn shifting the controller's latch-crossing time from the DRM's
own nominal-case ~86.2% (t=6207.4 s of 7200 s).

## Byte-for-byte reproduction of one sample

### Grid 1 (unchanged from F4)

Point 1, draw 0 (`drag_sail_vs_burn_p1_d0`, `config_hash`
`6052b88d7ae02b295f5fdb143e9d68f4266f7fc442278b1ac52f7366d348d014`) was re-run alone via
`av-sweep`'s own sample mode. The re-run's `RunProducts` was byte-for-byte identical to the
study's own recorded one: `cmp` reported no difference, and both files' SHA-256 was
`c9d3a2a113dacffabdf16456aeee4b90d5dad46d8258bd24837f06a3e1ebeecb`
(`scratchpad/f4/rerun_p1_d0.log`).

### Grid 2 (F4b, new — a different sample, re-proved independently)

Point 5, draw 0 (`drag_sail_vs_burn_flt_p5_d0`, `config_hash`
`abe2e6d17c04fdd062ac2801b6a6d9726f2464a6b739cf3b9ac778b08a88894a` — the strongest-effect grid
point, DragArea=25/dv_x=30) was re-run alone, from its own recorded `run_id` and the exact `.pb`
input files `av-sweep`'s study mode wrote for it (`sample_p5_d0/drm.pb`, `sos.pb`,
`sys_leo_demo_sys.pb`, `sys_demo_ctrl_sys.pb`), via `av-sweep`'s own sample mode:

```bash
./target/debug/av-sweep --run-sample \
  --drm-pb sample_p5_d0/drm.pb --sos-pb sample_p5_d0/sos.pb \
  --system-pb sample_p5_d0/sys_leo_demo_sys.pb --system-pb sample_p5_d0/sys_demo_ctrl_sys.pb \
  --run-id drag_sail_vs_burn_flt_p5_d0 --error-mode sampled --out rerun_p5_d0.pb
```

The re-run's `RunProducts` (`rerun_p5_d0.pb`) is **byte-for-byte identical** to the study's own
recorded `sample_p5_d0/run_products.pb`: `cmp` reports no difference, and both files' SHA-256 is
`a3b76c1988f5a317186401ee3dba4b160dab5f187111376144c8df4bfe611e03`
(`scratchpad/f4b/rerun_p5_d0.log`).

The same distinct-hash note grid 1's own document already carries applies here too, with grid 2's
own two values: `av-sweep --run-sample`'s own stderr printed `config_hash=64001811aacf0f5d4bd0a37
321881ecfafd11275f97457c5745c1e133be4b9c2` (`RunProducts.provenance.config_hash`, the
`SosConfiguration`'s own hash), which differs from `SweepSample.config_hash`
(`abe2e6d17c04fdd062ac2801b6a6d9726f2464a6b739cf3b9ac778b08a88894a`, `av_sweep::
sample_config_hash`, over the DRM+SOS+every `SystemDefinition` together) — two different,
deliberately distinct hashes at two different layers, not a reproduction failure. The browser
check above independently cross-validated the *first* of these two (`64001811aa…`) against the
value the viewer panel itself displayed for this exact sample.

## What this study does not establish

- **Grid 1's DragArea axis is not a validated drag-sail effect on `demo_mvr`.** As wired in the
  reused `demo_two_instance.sos.yaml`, DragArea has no physical channel to `demo_mvr`'s own
  trajectory at all. Grid 1 proves the *platform mechanics* — a parameter axis, its legality
  check, its grid expansion, its null result faithfully aggregated and rendered — not any real
  sensitivity of `demo_mvr`'s own final radius to a deployed drag sail's area.
- **Grid 2's DragArea axis IS a validated drag-sail effect on `demo_flt`, within this fixture's
  own scope — but only there.** It shows a real, resolved, monotonic, well-understood (`Cd ×
  DragArea` scaling) sensitivity of `demo_flt`'s own final radius, and a real (if
  larger-than-hypothesized) indirect sensitivity to the burn axis through controller-latch
  timing. It does **not** establish a validated drag-sail sensitivity for `demo_mvr` (see above),
  and it does not establish that this coupling generalizes beyond this specific fixture's own
  controller-threshold geometry.
- **A 7200 s arc, once, for both grids.** Every number above is a two-hour propagation from one
  epoch, one initial state, and three explicit burn magnitudes — not a continuous sweep or a
  multi-orbit arc. The dv_x slopes (~2.5–2.6 km/(m/s) on `demo_mvr`) and the DragArea slopes
  (tens to ~140 m on `demo_flt`) are specific to this burn's own true anomaly and this
  controller's own threshold-crossing epoch, not general constants. `demo_flt_rmag_at_end`'s own
  indirect dv_x sensitivity is, by its own root cause (latch-timing shift), especially specific
  to how close this particular arc's threshold crossing sits to the controller's own 6,884,300 m
  boundary — a different threshold, orbit, or burn epoch could plausibly show a much smaller (or
  larger) indirect coupling.
- **A two-instance demo fixture, not a real mission.** `leo_demo_sys`'s force model (JGM2 8×8 +
  Sun/Moon, plus `demo_flt`'s own JacchiaRoberts drag) and the native range-condition controller
  are a deliberately small fixture built for M17.3–M21.3's own bring-up goals, not a
  flight-representative spacecraft or mission design.
- **This dispersion model, at this draw count, for both grids.** `dispersed: true` is declared
  explicitly on both sweeps, but at `monte_carlo_draws=3` the default rule already selects
  `ExecutionErrorMode::Sampled` on its own — the flag is not load-bearing at this draw count on
  either grid. 3 draws per point is enough to see the Gates dispersion is present and roughly the
  right order of magnitude on both grids, but (see grid 1's own dispersion table, and pair 3's own
  noisier-than-pair-1 spread above) not enough to resolve dispersion *shape* reliably; a claim
  about how spread scales with an axis would need more draws per point than either
  budget-constrained grid used.
- **Drag on exactly one of the two vehicles, in both grids.** Neither grid ever declares drag on
  both `demo_flt` and `demo_mvr` simultaneously — that would need a new `SosConfiguration`, out
  of this task's authorized scope. So no measurement above speaks to how two independently
  drag-affected vehicles' final ranges would jointly respond to a shared or differing DragArea.

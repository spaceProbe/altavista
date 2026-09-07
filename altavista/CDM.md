# The CDM v1 adapter (`altavista/cdm.py`)

M1.3. Converts between altavista's own scenario model (`altavista/model.py`: km, km/s,
A1MJD epochs) and `altavista.v1` CDM v1 messages (`proto/altavista/v1/*.proto`: SI
metres/metres-per-second, TAI nanosecond epochs), per ADR-001. Also adds
`POST /api/cdm/trajectory` to the viewer server (`altavista/server.py`) so a CDM
`Trajectory` from elsewhere on the platform can be rendered in the existing viewer.

Entry points: `altavista.cdm.scenario_to_cdm`, `altavista.cdm.trajectory_to_cdm`,
`altavista.cdm.cdm_trajectory_to_viewer_json`, `altavista.cdm.event_to_cdm`,
`altavista.cdm.entity_for`, `altavista.cdm.frame_definition_for`, and
`Scenario.build_cdm()` (opt-in; `Scenario.build()` itself is unchanged).

```python
from altavista.cdm import scenario_to_cdm, cdm_trajectory_to_viewer_json

bundle = sc.build_cdm()               # or: scenario_to_cdm(sc.build())
bundle.trajectories[0].SerializeToString()

viewer_traj = cdm_trajectory_to_viewer_json(some_altavista_v1_trajectory)
```

## Units: km/km-s only inside this module

Per ADR-001 and `altavista/frames.py`'s own rule ("kilometres exist only inside the GMAT
adapter"), every `altavista.v1` message this module produces is in metres and
metres/second; every altavista object it produces is in kilometres and kilometres/second.
The conversion (`M_PER_KM = 1000.0`) happens in exactly one place on each path
(`trajectory_to_cdm`, `cdm_trajectory_to_viewer_json`).

## Epoch: A1MJD <-> TAI nanoseconds

`a1mjd_to_tai_ns()` / `tai_ns_to_a1mjd()` mirror `crates/av-cdm/src/time.rs`'s
`Tai::from_a1_mjd` / `Tai::to_a1_mjd` **exactly**, including the rounding rule (ties away
from zero, matching Rust's `f64::round()`, not Python's banker's-rounding `round()`):

* `A1 = TAI + 0.0343817 s` *exactly* — a fixed IAU/GMAT constant
  (`A1_MINUS_TAI_NS = 34_381_700`), **not** sourced from the leap-second table. GMAT's A.1
  atomic time scale has no leap-second dependency at all.
* GMAT's "Modified Julian Date" is `MJD = JD - 2430000.0`; the Unix epoch's Julian Date is
  `2440587.5`, so `GMAT_MJD(unix epoch) = 10587.5` (`GMAT_MJD_AT_UNIX_EPOCH`).
* Consequently **neither function touches the leap-second table.** No leap-second lookup
  happens on the A1MJD <-> TAI-ns path at all — the same as the Rust reference.

This module still loads `data/time/leap_seconds.json` once, at module level
(`_leap_table()`, `lru_cache`), and exposes full-parity table-driven `utc_ns_to_tai_ns()`
/ `tai_ns_to_utc_ns()` functions mirroring `Tai::from_utc_nanos` / `Tai::to_utc_nanos`
(same pre-1972 clamp, same post-2017 extrapolation, same post-insertion leap-second
collision resolution as `time.rs`). Nothing in the trajectory/event adapters below calls
them — they exist so this module is never tempted to invent a second leap-second table if
a UTC boundary ever needs one here, and so `tests/test_cdm_adapter.py` can cross-check the
shared table end to end.

**Cross-check with the Rust implementation.** There is no Rust toolchain (`cargo`/`rustc`)
in this environment, so `crates/av-cdm/src/time.rs` could not be compiled or run directly
to compare against. `tests/test_cdm_adapter.py` instead (1) checks this module's formula
against a literal value `time.rs`'s own test suite asserts
(`a1_mjd_of_the_tai_origin_matches_the_documented_constant`, copied verbatim from that
file's source text), and (2) cross-checks the full UTC-Gregorian -> UTC-ns ->
`utc_ns_to_tai_ns` -> `tai_ns_to_a1mjd` chain against GMAT's own `TimeSystemConverter` for
several post-1972 epochs spanning multiple leap-second insertions, within a stated
200-microsecond tolerance. See that test's docstring
(`test_a1_tai_matches_gmat_time_system_converter_post_1972`) for exactly what this does
and does not prove — in particular, it does **not** independently prove bit-for-bit
agreement with the compiled Rust crate.

## The covariance placeholder (question 11)

`trajectory_to_cdm` never fills `TrajectorySample.cov`. Covariance is always optional and
explicitly requested by a DRM — never a profile default — and this adapter has no DRM to
consult (altavista's own `Scenario` does not propagate a covariance today either). A caller
holding real covariance can set `TrajectorySample.cov` on the returned message; this
module will not fabricate zeros or an identity matrix to fill the field.

## Event `values` — only what can be parsed reliably

altavista's `Event.detail` is a free-form string. Only the exact shape
`Scenario.maneuver()` generates — `"dv = 20.00 m/s (VNB)"` — is parsed into structured SI
`values` (`{"dv_mps": 20.0}`). Even then, only the scalar magnitude is recovered: altavista's
own string never carries the individual V/N/B (or inertial) components, so `values` never
invents a `dv_v` / `dv_n` / `dv_b` breakdown, and the frame name (`"VNB"`) is never coerced
into a number. Any other `detail` text — notably the free-form multi-line text
`Scenario._event_from_summary` copies out of a GMAT command summary after a script run —
is left as `detail` only, with `values` empty. `_parse_dv_detail()` returns `None` (never
a best guess) for anything that does not match exactly.

## Frame mapping — no silent default

`frame_definition_for()` maps an altavista `Frame.axes` string to `altavista.v1.AxesKind`:

| altavista `Frame.axes` | `AxesKind`             |
|-----------------------|--------------------------|
| `MJ2000Eq`             | `AXES_KIND_MJ2000_EQ`     |
| `MJ2000Ec`             | `AXES_KIND_MJ2000_EC`     |
| `BodyFixed`            | `AXES_KIND_BODY_FIXED`    |
| `ICRF`                 | `AXES_KIND_ICRF`          |

Anything else altavista's own `parse_frame` accepts (e.g. `"BodyInertial"`, from the
`Inertial` frame-name suffix) has no `AxesKind` counterpart and raises
`UnmappedAxesError` — never approximated with a nearby kind, the same rule
`altavista/frames.py` follows for GMAT realizability. `frame_definition_for` does not run
`altavista.frames.FrameRegistry`; `FrameDefinition.gmat_name` is left unset, per core.proto's
own "set by the frame service after validation" contract.

## `config_hash`

`scenario_to_cdm()` computes one SHA-256 hash for the whole bundle and stamps it onto
every `Trajectory.config_hash`, every `Trajectory`/`Event`/`Entity`
`Provenance.config_hash`. `proto/altavista/v1` has no dedicated "GMAT adapter config"
message (adding one is out of scope for this worker — `proto/**` is owned elsewhere), so
the hash is built from the deterministic protobuf serialization
(`SerializeToString(deterministic=True)`, the same pattern
`tests/test_cdm_v1.py::test_drm_with_ric_frame_and_mixed_bindings_round_trips` uses) of
the pieces that actually define the configuration — the `FrameDefinition` and the sorted
`Entity` list — length-prefixed and concatenated, plus the state-space id and tool name.
Spacecraft and events are sorted explicitly (by name; by `(epoch, name)`) before
conversion, so neither the hash nor the bundle's trajectory/event order depends on
`ScenarioData.spacecraft` / `.events` list order — building the same `ScenarioData` twice
independently yields the same `config_hash` (`tests/test_cdm_adapter.py::test_config_hash_is_stable_across_independent_builds`).

## `POST /api/cdm/trajectory`

Accepts one `altavista.v1.Trajectory`:

* `Content-Type: application/x-protobuf` — raw binary protobuf.
* any other `Content-Type` (typically `application/json`) — JSON transcoding via
  `google.protobuf.json_format.Parse`.

Converts it with `cdm_trajectory_to_viewer_json` and publishes/broadcasts it through the
existing `Hub`, exactly like `POST /api/scenario`. A body that fails to parse, or a
`Trajectory` whose `interpolation` is not `INTERPOLATION_HERMITE_VELOCITY` /
`UNSPECIFIED`, gets a `400` naming what was wrong. `POST /api/scenario` itself is
unchanged.

**Known limitation.** The server keeps no live `FrameRegistry`, so an arbitrary posted
`Trajectory.frame_id` cannot be resolved to a real origin/axes here — it is carried
through only as the viewer `Frame`'s display `name`; `origin`/`axes` fall back to
altavista's own default (`Earth`/`MJ2000Eq`). A posted trajectory in a frame other than
Earth-MJ2000Eq will therefore render mislabeled (the numbers are exactly what was posted;
only the frame label/assumption used for any frame-aware viewer chrome is approximate).
Fixing this needs a frame-registry lookup this endpoint does not have; flagged here rather
than silently pretending the frame is always Earth-MJ2000Eq.

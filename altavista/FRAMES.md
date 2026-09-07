# The frame service (`altavista/frames.py`)

M1.2. Validates CDM `FrameDefinition`s (`proto/altavista/v1/core.proto`) by building the
actual GMAT `CoordinateSystem` (and, where needed, `GroundStation`) each one names, and
converts states between registered frames via GMAT's `CoordinateConverter`. Per
ADR-001 ("Frames"), GMAT is the reference implementation and the validator: a definition
GMAT cannot build is refused, never approximated with a different axes kind.

Entry point: `altavista.frames.FrameRegistry`.

```python
from altavista.frames import FrameRegistry
from altavista.pb import core_pb2

reg = FrameRegistry()
fd = core_pb2.FrameDefinition(id="sat_ric", entity_id="sat1", axes=core_pb2.AXES_KIND_RIC,
                               reference_entity_id="sat1", reference_body="Earth")
reg.register(fd)          # builds the GMAT object, sets fd.gmat_name, raises FrameError on failure
state_out = reg.convert(state_m, epoch_a1mjd, "eq_frame_id", "sat_ric")  # metres, m/s
```

## AxesKind -> GMAT realization

| `AxesKind`         | GMAT realization |
|---------------------|-------------------|
| `ICRF`               | `CoordinateSystem` with `Axes = ICRF`, `Origin = <body>`. |
| `MJ2000_EQ`           | `Axes = MJ2000Eq`, `Origin = <body>`. |
| `MJ2000_EC`           | `Axes = MJ2000Ec`, `Origin = <body>`. |
| `BODY_FIXED`          | `Axes = BodyFixed`, `Origin = <body>`. |
| `ENU`                 | A `GroundStation` at `origin_geodetic` (`StateType = Spherical`, `HorizonReference = Ellipsoid`, `Location1/2/3 = lat_deg/lon_deg/alt_km`) plus a `CoordinateSystem` with `Axes = Topocentric` about it. **Deviation:** GMAT's only local-horizon axes kind is not ENU -- see below. |
| `NED`                 | Same `GroundStation`/`Topocentric` realization as `ENU`, with the complementary fixed rotation applied instead -- see below. |
| `RIC`                 | `Axes = ObjectReferenced`, `Origin = entity_id`, `Primary = reference_body`, `Secondary = reference_entity_id`, `XAxis = R`, `ZAxis = N` (GMAT derives `YAxis = N x R`). Matches core.proto's stated convention (X=R, Z=N). |
| `VNB`                 | Same `ObjectReferenced`/`Primary`/`Secondary`/`Origin` pattern as `RIC`, `XAxis = V`, `YAxis = N` (GMAT derives `ZAxis = V x N`, the binormal). Matches core.proto (X=V, Y=N). |
| `VVLH`                | Same `ObjectReferenced`/`Primary`/`Secondary`/`Origin` pattern, `YAxis = -N`, `ZAxis = -R` (GMAT derives `XAxis = N x R`, in-track). **Deviation:** a deliberate altavista choice, not a GMAT default -- see below. Renamed from `LVLH` by M12.4 (question 106) -- see "GMAT `ImpulsiveBurn` `Axes = LVLH` vs `AXES_KIND_VVLH`" below for why. |
| `PLATFORM_BODY`       | **Declared, not convertible, when `attitude_source` is set** (question 72); raises `FrameNotRealizableError` when it is absent, unconditionally, same as before. See below. |
| `LOCAL_CARTESIAN`     | **No GMAT object.** Frameless by design. `register()` accepts it trivially and sets a fixed sentinel `gmat_name`; `convert()` refuses to convert into or out of it. |

### Deviations and approximations (read this)

**ENU / NED are not GMAT's native local-horizon axes.** GMAT's only local-horizon axes
kind is `Topocentric`, and per its own documentation ("the y-axis points due East and
the z-axis is normal to the local horizon. The x-axis completes the right handed set")
it is the classical geodesy **SEZ** convention (South, East, Zenith/Up) -- confirmed
empirically: `x = East x Up = -North`. It is neither ENU nor NED. altavista realizes both
CDM kinds as this same GMAT `Topocentric` (SEZ) `CoordinateSystem`, and applies a fixed,
explicit 3x3 permutation/sign matrix on top of GMAT's `CoordinateConverter` output to
produce true ENU or NED ordering:

```
ENU (East, North, Up)   from SEZ (South, East, Up): E = SEZ_y, N = -SEZ_x, U = SEZ_z
NED (North, East, Down) from SEZ (South, East, Up): N = -SEZ_x, E = SEZ_y, D = -SEZ_z
```

Both matrices are constant (epoch-independent) proper rotations -- SEZ, ENU and NED are
all right-handed physical bases, just relabeled/resigned -- so the same fixed matrix
applies unchanged to a velocity vector; there is no extra angular-rate term. This is
verified in `tests/test_frames.py::test_enu_ned_are_a_fixed_permutation_of_each_other`.

**VVLH's X/Y/Z convention is an altavista choice, not a GMAT default or a core.proto pin.**
core.proto states an explicit convention for RIC (X=R, Z=N) and VNB (X=V, Y=N), but not
for VVLH beyond "local vertical / local horizontal". GMAT has no literal `VVLH`/`LVLH`
`CoordinateSystem` axes kind either -- like RIC/VNB it is realized as `ObjectReferenced`.
GMAT's `ObjectReferenced` can only pair an axis from `{R, -R}` with one from `{N, -N}`, or
`{V, -V}` with `{N, -N}` (R and V are not generally orthogonal off a circular orbit, so
GMAT rejects an `{R, V}` pairing as non-orthogonal). altavista's `VVLH` therefore uses
`YAxis = -N`, `ZAxis = -R` (X computed = Y x Z = N x R, in-track).

**Ratified 2026-09-02 (lead review, question 73):** this is the standard LVLH, not a
deviation from it. The textbook and CCSDS definition takes Z = -R (nadir) and Y = -N and
*completes* X = Y x Z, which is the in-track direction N x R; it coincides with the velocity
direction only on a circular orbit. An "X = V" pairing is therefore not the standard and is
also what GMAT rejects as non-orthogonal off a circular orbit. `core.proto` pinned this
convention on `AXES_KIND_LVLH` at the time; M12.4 (question 106, see below) renamed that
enum value to `AXES_KIND_VVLH` -- the field number and the pinned convention itself are
unchanged, only the name.

### GMAT `ImpulsiveBurn` `Axes = LVLH` vs `AXES_KIND_VVLH` (M11.3 question 102; renamed by M12.4 question 106)

Everything above is about `FrameRegistry`/`CoordinateSystem` (state *conversion*).
`altavista.scenario.Scenario.maneuver` (M11.3, `frame="RIC"`/`"LVLH"`; `"VVLH"` added by
M12.4, all implemented by `Scenario._fire_impulsive_burn`) can instead fire a real GMAT
`ImpulsiveBurn` object -- and GMAT's `ImpulsiveBurn` resource has its own, separate,
*literal* `Axes` field, independent of `CoordinateSystem`/`ObjectReferenced`, whose allowed
values include `"LVLH"` directly (`docs/help/html/ImpulsiveBurn.html` in the GMAT
distribution: `Axes` allowed values `VNB, LVLH, MJ2000Eq, SpacecraftBody`). This is the
"no literal `LVLH`/`VVLH` `CoordinateSystem` axes kind" gap the paragraph above already
flags -- but `ImpulsiveBurn` genuinely has a literal `Axes = LVLH` value, under the name
this platform used to call its own ratified convention, and **it is a different
convention from that ratified convention (now named `AXES_KIND_VVLH`).** This naming
collision -- not the underlying physics, which stays exactly as measured below -- is what
question 106 resolved by renaming this platform's enum value; GMAT's own `ImpulsiveBurn
Axes = LVLH` field name is GMAT's, untouched, and still spelled `"LVLH"`.

**Measured empirically (M11.3), two independent ways:**

1. A scratch spacecraft at `r = (7000, 0, 0)` km, `v = (0, 7.5, 0)` km/s (so `R = +X`,
   `N = r x v = +Z`, in-track `= N x R = +Y`), a plain `ImpulsiveBurn` with
   `CoordinateSystem = Local`, `Origin = Earth`, `Axes = LVLH`, `Element1/2/3 = (1, 1,
   1)` km/s, fired via the low-level API (`Construct` / `SetField` / `SetSolarSystem` /
   `SetSpacecraftToManeuver` / `Initialize` / `Fire`) and read back with
   `ImpulsiveBurn.GetDeltaVInertial()`: **returns exactly `(1, 1, 1)`** -- i.e.
   `X_lvlh = R`, `Y_lvlh = N x R` (in-track), `Z_lvlh = N`. The GMAT help text confirms
   this in words: "the X-axis points from the center of the [body] to the spacecraft ...
   the Z-axis is along the instantaneous orbit normal ... the Y-axis completes the
   right-handed set" -- the *same* triad `ImpulsiveBurn.html` uses to describe its own
   `RIC`-shaped `ObjectReferenced` construction, not the VVLH-style nadir/anti-normal one.
2. The same dv fired instead through an `ImpulsiveBurn` whose `CoordinateSystem` is an
   ObjectReferenced RIC `CoordinateSystem` (`XAxis = R`, `ZAxis = N`, built through
   `FrameRegistry`, exactly the "RIC" branch above) returns the **identical**
   `GetDeltaVInertial()` for every dv tried -- not just the symmetric `(1,1,1)` case.

So: **GMAT's `ImpulsiveBurn Axes = LVLH` is numerically identical to `AXES_KIND_RIC`
(X=R, Z=N), and different from this platform's ratified `AXES_KIND_VVLH` (Z=-R, Y=-N,
X=N×R, the VVLH convention ratified just above).** For the test dv=(1,1,1) at this
r/v, `AXES_KIND_VVLH`'s formula (`crates/av-kernel/src/drm/maneuver.rs::dv_to_inertial`,
read-only, ratified) gives `(-1, 1, -1)` -- a genuinely different vector, not a rounding
difference.

**This is not reconciled -- it is pinned and reported.** `Scenario.maneuver(frame="LVLH")`
reports whatever GMAT's `ImpulsiveBurn` actually computes; nothing reorients the result to
agree with `AXES_KIND_VVLH`. `goldens/gen_leo_1day_maneuver_gmat_lvlh.py` /
`goldens/leo_1day_maneuver_gmat_lvlh.json` (renamed by M12.4 from `..._lvlh.py`/`.json` so
the filename itself could never be mistaken for this platform's own convention) pin GMAT's
own `Axes=LVLH` burn directly (a 20 m/s "in-track" component on the same LEO orbit
`leo_1day_golden` uses) -- and, as direct proof of the mapping above, its
`state_pre_burn`/`state_post_burn`/`final_state` come out bit-for-bit identical to
`goldens/leo_1day_maneuver_ric.json`'s (same dv values, X=R Z=N triad either way).
`drms/leo_1day_maneuver_vvlh.drm.yaml` declares that same burn the way any caller
targeting the ratified `AXES_KIND_VVLH` would (`frame_id: AXES_KIND_VVLH`);
`crates/av-kernel/tests/drm_maneuver.rs::
drm_maneuver_axes_kind_vvlh_does_not_reproduce_gmats_native_lvlh_burn` runs it through the
executor and **measures, not avoids, the mismatch**: `|dv| = 28.284271` m/s at the burn
epoch itself (`sqrt(20^2 + 20^2)` -- the two conventions send the same `dv_y=20` component
in orthogonal directions, in-track vs. anti-normal) and `|dr| = 276275.3` m /
`|dv| = 271.3` m/s an hour later, after coasting on the wrong post-burn velocity. The
sibling test `drm_maneuver_axes_kind_ric_reproduces_gmats_native_lvlh_burn` retags the
identical `dv` `AXES_KIND_RIC` and confirms it *does* reproduce the golden at the tight
(0.05 m / 5e-5 m/s) tolerance -- independent, Rust-side confirmation of the same mapping,
i.e. the record that GMAT's `Axes=LVLH` equals `AXES_KIND_RIC`.

The retired name is not merely undocumented -- it is `reserved` in `core.proto` and refused
at load: `drms/leo_1day_maneuver_lvlh.drm.yaml` (kept, unchanged, still literally declaring
`frame_id: AXES_KIND_LVLH`) now exists solely to prove
`crates/av-kernel/tests/drm_maneuver.rs::a_drm_naming_axes_kind_lvlh_is_a_typed_load_error`:
a DRM naming the old value fails to load with `DrmError::InvalidEnumValue`, not a silent
mismatch.

**Practical consequence for callers of `crates/av-kernel`'s DRM executor:** if a DRM's
intent is "apply the same delta-v a GMAT `ImpulsiveBurn Axes=LVLH` would apply", tag the
`ScenarioEvent` `AXES_KIND_RIC` -- not `AXES_KIND_VVLH`, which means this platform's own
ratified VVLH convention, a real, different, equally valid frame, just not the one GMAT
calls `"LVLH"`. Before M12.4 this same fact was easy to get backwards precisely because
both conventions shared the name `LVLH`; question 106's rename (`AXES_KIND_LVLH` ->
`AXES_KIND_VVLH`, old name `reserved`) exists so that a DRM author can no longer reach for
`AXES_KIND_LVLH` at all and get either convention by accident -- the physical difference
between the two conventions is unchanged and remains fully pinned on both sides
(`AXES_KIND_RIC` for GMAT's, `AXES_KIND_VVLH` for this platform's own).

**`PLATFORM_BODY` is declared-but-not-convertible when `attitude_source` is set (question
72).** `core.proto` v1 added `FrameDefinition.attitude_source` (an `AttitudeSource`: either
`entity_attitude_stream`, a CDM entity id, or `gmat_attitude_model`, a GMAT attitude type
name), realized by a future attitude service (P2). `register()` now:

* Raises `FrameNotRealizableError` (a `NotImplementedError` subclass), unconditionally and
  unchanged from before, when `attitude_source` is unset -- there is nothing to validate.
* Otherwise validates, in Python before touching GMAT:
  * `attitude_source` sets exactly one of `entity_attitude_stream` / `gmat_attitude_model`
    (`MissingFieldError` on `attitude_source.source` if neither is set).
  * `reference_entity_id` is present (`MissingFieldError`).
  * `attitude_source.reference_frame_id` is present (`MissingFieldError`) -- required
    because it doubles as this frame's `parent_frame_id` (question 76, below).

  then against GMAT:
  * `reference_entity_id` names a `Spacecraft` GMAT actually has (`UnknownEntityError`,
    same check as RIC/VNB/VVLH).
  * For `gmat_attitude_model`: **verified against real GMAT**, not a hardcoded Python list
    -- `_validate_gmat_attitude_model` attempts
    `Spacecraft.SetField("Attitude", model_name)` on a scratch, process-shared GMAT
    `Spacecraft` (`gv_frames_attitude_probe`; never the entity the definition actually
    names, so validating a name never mutates a real spacecraft). GMAT constructs the
    `Attitude` sub-object immediately inside `SetField` and raises its own exception
    (`APIException`, "Cannot create Attitude object of unknown attitude type ...") for a
    name it does not recognize; confirmed empirically against this GMAT build (R2026a) --
    `CoordinateSystemFixed`, `NadirPointing`, `Spinner`, `PrecessingSpinner`, `CCSDS-AEM`
    and `SpiceAttitude` are all accepted, `Kinematic` and any made-up name are rejected
    with that exception, and no `Initialize()` call is needed for the exception to fire.
    **What this does and does not prove:** it proves GMAT recognizes the name as a
    constructible attitude *type*. It does **not** prove the type's other required fields
    are configured (e.g. an actual AEM file for `CCSDS-AEM`), that the type is compatible
    with how `reference_entity_id` is actually propagated, or anything about
    `reference_frame_id`'s physical sense -- those remain the attitude service's job.
  * `entity_attitude_stream` is **not** validated against GMAT -- it names a CDM/state-space
    concept, not a GMAT one, and this module validates against GMAT's configuration only
    (per ADR-001/M1.2's scope); pattern-matching it against some Python-side notion of the
    entity catalog this module does not own would be exactly the kind of check the task
    brief warns against.

A validated `PLATFORM_BODY` frame gets no native GMAT `CoordinateSystem` (`register()` sets
`gmat_name` to a descriptive sentinel, `gv_platform_body_<safe id>`, analogous to
`LOCAL_CARTESIAN`'s), so `convert()` and `rotation_matrix()` raise
`AttitudeServiceUnavailableError` for it -- **deliberately not** `FrameNotRealizableError`:
that error means "this frame cannot even be declared"; this one means the frame *is*
legitimately declared and validated, only conversion is deferred to the attitude service.

## `parent_frame_id` fill (question 76)

`core.proto` v1 also added `FrameDefinition.parent_frame_id`, for the viewer's frame graph.
It is optional on input -- proto3 gives it no field-presence tracking (a plain `string`,
not `optional`), so an omitted field and an explicitly-supplied empty string are the same
wire value, and `register()` treats both as "not supplied". When not supplied, `register()`
fills it deterministically from `axes` and the origin; when supplied (non-empty), it is
checked against the same rule and rejected with `InconsistentParentFrameError` if it
disagrees -- never silently kept, never silently overwritten.

| `AxesKind` | Parent |
|---|---|
| `ICRF` / `MJ2000_EQ` / `MJ2000_EC` / `BODY_FIXED` | `""` (registry root) -- body-centred frames are the top of the tree by construction. |
| `ENU` / `NED` | The origin body's `BODY_FIXED` frame (`origin_geodetic.body`), **auto-registered if not already registered** under a canonical `gv_auto_parent_<body>_BodyFixed` id (any already-registered matching frame, under any id, is reused instead -- see "Auto-registration" below). |
| `RIC` / `VNB` / `VVLH` | The reference body's `MJ2000_EQ` frame (`reference_body`), auto-registered/reused the same way under `gv_auto_parent_<body>_MJ2000Eq`. |
| `PLATFORM_BODY` | `attitude_source.reference_frame_id`, **verbatim** -- the schema already names exactly this ("frame the attitude is expressed relative to"), so it is the most precise parent available and altavista does not derive a different one. Required non-empty (`MissingFieldError` if absent). **Not** auto-registered: unlike ENU/NED and RIC/VNB/VVLH, an attitude reference frame is not derivable from axes alone, so a `parent_frame_id` that names a not-yet-registered (or misspelled) frame is accepted at `register()` time and only surfaces as `FrameParentMissingError` when a tree operation actually walks the chain. |
| `LOCAL_CARTESIAN` | `""` (registry root) -- frameless by design, with no body/entity/geodetic origin to derive a parent from; treating it as anything but root would imply a relationship it does not have. |

**Auto-registration** (ENU/NED, RIC/VNB/VVLH only): `register()` first searches every
already-registered frame for one with matching `AxesKind` and body (picking the
lexicographically smallest id if more than one matches, so this never depends on dict
insertion order) and reuses it; only if none exists does it register a new one under the
canonical id above, going through the normal `_register_body_axes` path (so it gets a real
GMAT `CoordinateSystem`, existence-checked with `gmat.Exists` exactly like every other GMAT
object this module builds).

## Tree operations (question 76)

`FrameRegistry.children(frame_id="")` and `FrameRegistry.path_to_root(frame_id)` expose the
`parent_frame_id` tree for the viewer's `frames.js`, via the scene service's JSON.

* `children(frame_id="")` -- registered frame ids whose `parent_frame_id == frame_id`,
  **sorted lexicographically** (never dict-insertion order). `frame_id=""` (the default)
  lists the registry's top-level frames. Raises `FrameError` if a non-root `frame_id` is
  not itself registered.
* `path_to_root(frame_id)` -- `[frame_id, parent, grandparent, ...]` up to (not including)
  the root sentinel `""`. Raises `FrameCycleError` on a cycle in the `parent_frame_id`
  chain, or `FrameParentMissingError` if some parent in the chain names an unregistered
  frame id -- instead of recursing forever on a malformed chain. In practice a cycle or a
  dangling parent can only arise through `PLATFORM_BODY`'s verbatim
  `attitude_source.reference_frame_id` (every other axes kind's parent is derived or
  auto-registered, so it always resolves).

**Units and epoch (read before calling `convert()`):**
* `Geodetic` is SI per ADR-001 (`latitude_rad`/`longitude_rad` radians, `height_m`
  metres); GMAT's `GroundStation` wants degrees and kilometres. That conversion happens
  in exactly one place, `_geodetic_to_gmat`.
* `FrameRegistry.convert()` takes and returns a 6-vector `[x, y, z, vx, vy, vz]` in
  **metres** and **metres/second** (the CDM's units) even though GMAT itself works in
  km and km/s; that round trip happens in exactly one place, inside `convert()` --
  the GMAT-adapter boundary per ADR-001.
* `convert()`'s epoch parameter is named `epoch_a1mjd` and is a bare GMAT **A1MJD
  float**, deliberately -- **not** the CDM's `epoch_ns` (TAI nanoseconds). ADR-001 says
  GMAT's A1 scale is TAI + 0.034 381 7 s, converted "inside the GMAT adapter only" via a
  shared, versioned leap-second table. That table (worker A's Rust crate plus the shared
  `data/time/leap_seconds.json`) does not have a Rust/Python API yet at the time this was
  written. A TAI-ns-facing wrapper around `convert()` is M1.3's job, once that API lands;
  this module does not invent its own leap-second table to fill the gap in the meantime.

## Measured tolerances

From `tests/test_frames.py`, run against real GMAT (`.venv/bin/python -m pytest
tests/test_frames.py -v -s`), test spacecraft state `[6878.137, 512.4, -188.9, -0.221,
7.4013, 2.0110]` km, km/s at A1MJD 21545.0 (deliberately not axis-aligned):

| Check | Measured |
|---|---|
| J2000 mean obliquity (MJ2000Eq -> MJ2000Ec rotation about +X) | `23.43929111` deg magnitude vs. the IAU-76 constant `84381.448 arcsec = 23.439291111...` deg -- agree to `1.1e-9` deg |
| RIC axis orthonormality, `max\|R R^T - I\|` | `2.22e-16` |
| RIC R-axis . spacecraft position unit vector | `1.0000000000000002` |
| Round trip worst relative error (position and velocity, A -> B -> A), across MJ2000Eq<->MJ2000Ec, MJ2000Eq<->BodyFixed, MJ2000Eq<->RIC, MJ2000Eq<->VNB, MJ2000Eq<->VVLH, MJ2000Eq<->ENU, MJ2000Eq<->NED | `4.31e-15` (the ENU/NED pair) -- all pairs well under the `1e-9` target |

**`gmat_attitude_model` validation, measured against this GMAT build (R2026a)** by attempting
`Spacecraft.SetField("Attitude", name)` on a scratch spacecraft (`_validate_gmat_attitude_model`):

| `name` | Result |
|---|---|
| `CoordinateSystemFixed`, `NadirPointing`, `Spinner`, `PrecessingSpinner`, `CCSDS-AEM`, `SpiceAttitude` | Accepted |
| `Kinematic`, `nadirpointing` (wrong case), `""`, any made-up name | Rejected -- GMAT `APIException`: `` Cannot create Attitude object of unknown attitude type "<name>" `` |

`GetPropertyEnumStrings` on the `Attitude` parameter ID returns an empty tuple on this
build, so there is no enumerated-values API to query instead; `SetField` (which raises
immediately, without needing `Initialize()`) is the mechanism this module uses.

## `fixed_rotation_q` is a display convenience; producer-side conversion is authoritative (M19.2, question 129)

`FrameDefinition.fixed_rotation_q` (`core.proto` field 13) carries the constant rotation from a
frame's parent to that frame, so a consumer such as the viewer can realize an inertial-to-inertial
rotation from the bundle alone. The producer fills it only for frames whose rotation to their
parent is measurably constant (ICRF against MJ2000 equatorial); body-fixed and entity-origin
frames are time-varying and stay empty.

**The authoritative numbers are the trajectory samples themselves.** Since ADR-002's fourth
amendment (question 128), a trajectory is emitted in its declared frame by converting *every
sample* through GMAT's `CoordinateConverter::Convert`. The viewer's fixed rotation is a
convenience for re-viewing an already-converted trajectory in a sibling inertial frame; it is not
a second source of truth, and where the two disagree the converted samples win.

**Disclosed magnitude of that disagreement.** The ICRF/FK5 bias is physically constant, but GMAT
realizes it by Lagrange-interpolating a data table (`AxisSystem.cpp`'s
`RotationMatrixFromICRFToFK5`, `ICRFFile.cpp`), which carries a small linear-in-time residual
measured at about `1.78e-14` rad/s. The producer converts each sample against that time-varying
interpolation while the viewer applies one fixed quaternion for the whole run, so the two drift
apart by roughly `1.5e-9` rad over a day -- of order **1 cm at LEO radius over a day-long run**,
which is above this repository's `1e-4` m golden tolerance class. Sub-centimetre viewing geometry
must therefore come from the converted samples, not from applying `fixed_rotation_q` to a
trajectory expressed in another frame.

This is also why the producer's constancy check measures 10 s apart rather than a day: at a day's
separation that same interpolation residual reaches `1.32e-9`, three orders past the `1e-12` bar
the check requires, and ICRF would be wrongly judged non-constant. The tolerance was not loosened
to accommodate it -- the measurement interval was chosen to sit inside the interpolation window.

## Errors (`altavista.frames`)

All typed, subclasses of `FrameError`:

* `UnknownAxesKindError` -- `axes` unset or not a value core.proto declares.
* `MissingFieldError` -- a required field for the given `AxesKind` is missing
  (`origin_geodetic` for ENU/NED; `reference_entity_id`/`reference_body`/`entity_id` for
  RIC/VNB/VVLH; `body` for ICRF/MJ2000_EQ/MJ2000_EC/BODY_FIXED). Carries `.field`.
* `UnknownEntityError` -- a referenced entity is not a `Spacecraft` in GMAT's current
  configuration. Carries `.entity_id`.
* `FrameNotRealizableError` (also a `NotImplementedError`) -- `PLATFORM_BODY` without
  `attitude_source`.
* `UnknownGmatAttitudeModelError` -- a `PLATFORM_BODY`'s `attitude_source.gmat_attitude_model`
  names a string GMAT would not accept as `Spacecraft.Attitude`. Carries `.model_name`.
* `AttitudeServiceUnavailableError` -- `convert()`/`rotation_matrix()` were asked to convert
  through a *declared* `PLATFORM_BODY` frame. Not a `FrameNotRealizableError` -- see above.
* `InconsistentParentFrameError` -- a supplied `parent_frame_id` disagrees with the
  deterministic fill rule (question 76). Carries `.supplied` and `.computed`.
* `FrameCycleError` -- `path_to_root()` found a cycle in the `parent_frame_id` chain.
* `FrameParentMissingError` -- `path_to_root()`/`children()` followed a `parent_frame_id`
  naming an unregistered frame. Carries `.missing_parent_id`.

## `altavista/pb/` -- committed protobuf bindings

`altavista/pb/generate.py` runs `protoc` (recorded `PROTOC_VERSION = "libprotoc 35.1"`)
over `proto/altavista/v1/*.proto` into `altavista/pb/altavista/v1/*_pb2.py`, which is
committed to the repo (unlike `build/pb`, which `tests/test_cdm_v1.py` still generates
ad hoc and which stays gitignored). Regenerating is idempotent and byte-identical for a
fixed protoc version. `altavista/pb/__init__.py` puts its own directory on `sys.path`
once, at import time, so the generated modules' absolute `from altavista.v1 import
..._pb2` cross-imports resolve without any `sys.path` handling by an importer -- then
re-exports the seven `*_pb2` modules, so `from altavista.pb import core_pb2` works
directly. Regenerate with `.venv/bin/python altavista/pb/generate.py`.

## Attitude sampling and sensor footprints (M6.3)

`altavista.scenario.Scenario` samples a spacecraft's real attitude quaternion (into
`Trajectory.attitude`, additive since M5.2) when the caller has *explicitly* configured
a GMAT attitude model on it -- `sc.spacecraft(..., Attitude="NadirPointing")` or
`sat.set_field("Attitude", ...)` -- and only then: GMAT gives every `Spacecraft` a
default `Attitude` (`"CoordinateSystemFixed"`, verified empirically -- `HasAttitude()`
is `True` even when nothing ever sets the field), so trusting `HasAttitude()` directly
would silently attach an unrequested attitude stream to every existing scenario/example
that never asked for one. `Spacecraft._attitude_explicit` (set only by those two call
sites) is what `Scenario._attitude_quat_in_frame` actually checks.

### Quaternion convention: "axis direction expressed in the parent frame"

The wire quaternion (`[x, y, z, w]`, scalar-last) is defined so that applying it to a
vector given in the spacecraft's **body** frame yields that vector's representation in
`ScenarioData.frame` (the scenario/parent frame) -- exactly the convention already used
by `altavista.bodies.BodySampler.orientation` (body-fixed -> scenario, for
`BodyTrack.quat`) and by `web/js/frames.js`'s RIC/VNB/VVLH `axesForKind`/
`quaternionFromAxes` fallback, both of which feed the result directly into Three.js's
`Object3D.quaternion` with no inversion. This is **not** GMAT's own convention:

* GMAT's `Spacecraft.GetAttitude(t)` (== `Attitude::GetCosineMatrix(t)`, verified against
  `third_party/gmat-src/src/base/attitude/Attitude.hpp`'s member comment "the current
  rotation matrix (**from inertial to body**)") returns a DCM `C` such that `v_body = C
  @ v_attitudeCS` -- the standard CCSDS/aerospace "attitude matrix" convention
  (`AttitudeConversionUtility.cpp`: "we are now using the CCSDS definition of
  quaternions where qc = q4", i.e. scalar-last, same component order altavista uses, but
  the *opposite* rotation direction).
* Three.js's `Object3D.quaternion` (and this codebase's `makeBasis`/`setFromRotationMatrix`
  fallback path) needs the **transpose**, `C^T`: applying it to a local/body-space vector
  must yield that vector's *parent-frame* representation (standard scene-graph
  semantics -- a child's local `+X` axis, transformed by its own `quaternion`, is where
  that axis physically points in the parent's coordinates).

So `_attitude_quat_in_frame` transposes GMAT's raw DCM (equivalently, conjugates GMAT's
own quaternion: `[x,y,z,w] -> [-x,-y,-z,w]`) before composing with the (attitude
coordinate system -> scenario frame) rotation and converting to a quaternion via
`altavista.bodies._mat_to_quat`.

**How this was verified**, not assumed:
* `_mat_to_quat(M)` reconstructs the *same* rotation `M` via the standard
  quaternion-vector-rotation formula (`altavista.scenario._quat_apply`) to float64 machine
  precision (`_quat_apply(_mat_to_quat(M), v) == M @ v` for random `M`/`v`), so the two
  are known to share one consistent convention.
* For a `NadirPointing` spacecraft, applying the *raw* (untransposed) GMAT DCM to the
  body's aligned axis gives the wrong vector (`dcm @ nadir_hat` returns the body axis
  unit vector -- consistent with GMAT's own "inertial-to-body" definition, not directly
  usable to place a body axis in scene coordinates); applying `_quat_apply` to the
  *transposed* result (what this module actually emits) reproduces the true nadir
  direction in scenario-frame coordinates to `~1e-16` (`tests/test_frames.py::
  test_nadir_pointing_plus_x_body_axis_tracks_nadir_to_1e_minus_9`).

### GMAT attitude reference frame per model

* **`NadirPointing`** computes its DCM directly from
  `owningSC->GetMJ2000State(theTime)` (verified against
  `third_party/gmat-src/src/base/attitude/NadirPointing.cpp`) -- it never reads the
  `AttitudeCoordinateSystem` field at all. That default MJ2000 frame is
  `<central_body>MJ2000Eq`, the same frame `Scenario._to_frame` converts *from* for
  position/velocity, so this module special-cases `NadirPointing` to use exactly that
  frame rather than trusting the (unread, class-default) `AttitudeCoordinateSystem`
  value.
* Every other model this round supports (`CoordinateSystemFixed`, `Spinner`, a CCSDS-AEM
  file) reads `AttitudeCoordinateSystem` directly (`Spacecraft.GetStringParameter
  ("AttitudeCoordinateSystem")` delegates to the owned `Attitude` object; default
  `"EarthMJ2000Eq"`), used as-is.
* The rotation from that reference frame into the scenario frame is obtained with the
  same dummy-vector `CoordinateConverter.Convert()` trick
  `altavista.bodies.BodySampler.orientation`/`altavista.frames.FrameRegistry.rotation_matrix`
  already use (`Scenario._rotation_matrix_between`), not a new mechanism.

### A GMAT API quirk this feature ran into: `Propagator.Step()` does not sync the `Spacecraft` object

`Scenario.propagate()`'s step loop reads position/velocity straight from the low-level
integrator buffer (`gator.GetState()`), which `Propagator.Step()` *does* keep current --
but GMAT's attitude models (at least `NadirPointing`) compute their DCM from the
**owning `Spacecraft` object's own state** (`owningSC->GetMJ2000State(...)`), which
`Step()` does **not** push updates into. Calling `Spacecraft.GetAttitude(t)` mid-loop
without an explicit sync silently returns the *same* (stale, pre-loop) matrix for every
sample -- verified directly: without the fix below, the measured NadirPointing
nadir-alignment residual grows monotonically from ~0 to ~2.0 (fully wrong / near-opposite)
over one loop, instead of staying under `1e-9`. Fixed by calling the low-level
`Propagator.UpdateSpaceObject()` (found in the GMAT API alongside `Step()`/`GetState()`)
once per step, but only when at least one propagated spacecraft has an explicit attitude
model (`attitude_sats` in `propagate()`) -- a scenario with no attitude configured pays
nothing extra.

A second, unrelated GMAT-singleton quirk surfaced while adding tests for this: two
different `Scenario` instances in one process, each doing its *first* `propagate()`
call with default settings, previously generated the *identical* auto `ForceModel`/
`Propagator` name (`self._prop_counter`/`len(self.spacecraft_list)` both restart at 0
per instance, but GMAT's object namespace is process-wide) and silently corrupted each
other's `PropSetup`. Fixed by folding `id(self)` into both default names
(`force_model()`, `propagate()`'s `pname`) -- affects only the auto-generated case; an
explicit `name=` is untouched.

### `NadirPointing`'s default body axis is **+X**, not +Z

Verified two ways, not assumed: (1) reading `third_party/gmat-src/src/base/attitude/
Attitude.cpp`'s `bodyAlignmentVector.Set(1.0, 0.0, 0.0)` default, and confirmed the
constructed `NadirPointing` model's referenced-frame math (`xhat = pos/|pos|`
(radial-outward), `referenceVector = -xhat` (nadir) in that local frame) means the
default `BodyAlignmentVector` (+X) is what GMAT aligns to nadir; (2) reading the field
back live off a freshly built `Spacecraft` (`sat.obj.GetRealParameter
("BodyAlignmentVectorX") == 1.0`, `...Y == 0.0`, `...Z == 0.0`) and confirming
numerically that the body's `+X` axis (not `+Z`) tracks the nadir direction to
`~1e-16` across a full propagation.

### Sensor footprint (`Scenario.footprint`, `altavista.model.Footprint`)

A cone of declared half-angle about a declared body axis, intersected with the target
body's ellipsoid (equatorial radius / flattening read live from GMAT's own body model,
the same source `BodySampler` uses for `BodyTrack.radius`/`.flattening`), computed in
double precision (`Scenario._cone_ray_directions`/`_ray_ellipsoid_intersect`, plain
`numpy.float64`/Python `float`, no `float32` anywhere in this path). Requires the
declared spacecraft to already carry a recorded attitude stream (raises `ValueError` at
`build()` time otherwise -- a cone direction with no attitude would have to be
invented). Computed lazily at `build()` time, from the spacecraft's already-recorded
`t`/`pos`/`attitude` samples -- **never** by re-querying GMAT's attitude object for a
past epoch a second time, since a stateful model (`Spinner`) cannot be re-queried out of
order without corrupting its own incremental state (see the `Propagator.Step()` quirk
above for the same class of bug).

Wire shape (`ScenarioData.to_dict()["footprints"]`, additive): one entry per declared
footprint, `{name, spacecraft, halfAngleDeg, axis, color, t, center, ring}` -- `center`
and `ring` are per-`t`-sample, already expressed in the scenario frame (km, same as
`Trajectory.pos`), so the viewer draws them directly with no further frame conversion. A
cone ray that misses the ellipsoid (partly over the horizon) is simply omitted from that
sample's `ring`, never fabricated; a sample whose boresight itself misses gets `center:
null`. No proto change: this is altavista-JSON-only (not part of the CDM path), a plain
additive top-level list, the same pattern `frames` (M4.1) already established.

**Validation against a closed form.** For a nadir-pointing cone of sensor half-angle `a`
at altitude `h` over a *sphere* of radius `R`,
`altavista.scenario.nadir_footprint_half_angle_spherical` computes the ground half-angle
(Earth-central angle) via the standard SMAD/Wertz Earth-coverage-geometry relation:
elevation `= arccos((R+h)/R * sin(a))`, ground half-angle `= 90 deg - a - elevation`
(returns `0.0`, not `NaN`, once `(R+h)/R * sin(a) > 1`, i.e. beyond the horizon -- the
same "no intersection" case `_ray_ellipsoid_intersect` reports as `None`). Forcing the
ellipsoid to a perfect sphere (`a_km == b_km`) and comparing this closed form against
the numeric ray/cone/ellipsoid-intersection code reproduces it to `~6e-13` rad
(`tests/test_frames.py::test_footprint_ray_ellipsoid_matches_closed_form_nadir_radius_on_a_sphere`)
-- this validates the *geometry code*, since the closed form itself has no oblate-ellipsoid
generalization.

**This closed form is spherical, not exact for WGS84.** Earth's real flattening
(`~0.0033527`, matching the standard `1/298.257`-ish value, read live off GMAT's own
`Earth` body) means the true ellipsoid radius varies between the equatorial value used
by the closed form and `R_polar = R_eq * (1 - f)`, a difference of `R_eq * f ~= 21.4 km`
in the absolute worst case (a footprint straddling the pole). Measured directly for this
round's actual scenario (`examples/01_leo_propagate.py`'s ISS, 10 deg half-angle, ~400 km
altitude, 51.6 deg inclination): the spherical closed-form radius and the true numeric
ellipsoid-intersection radius differ by up to ~2.3 km across the sampled orbit -- well
under the 21.4 km loose bound, and reported here rather than silently treated as exact.

**Only the "step propagation" path (`Scenario.propagate`/`Scenario.maneuver`) samples
attitude.** `Scenario.run_script`/`Scenario.from_script` (script runs, parsed from a
`ReportFile`) do not -- adding that would mean parsing attitude out of a GMAT
`ReportFile`, out of scope for this round; a script-run `Trajectory` simply has no
`attitude`, same as before M6.3.

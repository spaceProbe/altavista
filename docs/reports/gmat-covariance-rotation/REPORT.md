# GMAT R2026a: `OrbitErrorCovariance` reported in a rotating coordinate system omits the rotation-rate term

## Summary

When a spacecraft's `OrbitErrorCovariance` is reported through a `CoordinateSystem` other
than the one it is declared in, GMAT converts the 6x6 Cartesian covariance with the
block-diagonal transform `[[R, 0], [0, R]]`, where `R` is the 3x3 rotation between the two
systems. That transform is correct only when the two systems have no relative angular
velocity (for example `EarthMJ2000Eq` to `EarthICRF`). For a rotating target system such as
`EarthFixed`, the velocity transform is `v_fixed = R v + Ṙ r`, so the correct covariance
transform is `[[R, 0], [Ṙ, R]]`. The missing `Ṙ` term leaves the velocity block and the
position-velocity cross block of the reported covariance wrong by amounts linear and
quadratic in the frame's angular rate. GMAT's own **state** conversion in the same run does
include the rotation rate, so the state and the covariance it reports for the same object in
the same system are mutually inconsistent.

## Environment

- GMAT R2026a, macOS 26.6.2 (Apple silicon), driven through the GMAT Python API (`gmatpy`)
  with `LoadScript` and `RunScript`. The script is plain GMAT script and runs unchanged in
  the GMAT GUI or console.
- No third-party code is involved. The reproduction uses only a `Spacecraft`, a
  `ForceModel`, a `Propagator`, two `ReportFile`s and the built-in `EarthMJ2000Eq` and
  `EarthFixed` coordinate systems.

## Where the transform is built

`src/base/parameter/OrbitData.cpp`, `OrbitData::GetCovarianceRmat66()`. After
`mCoordConverter.Convert(...)` succeeds, the 6x6 `transform` is filled from
`mCoordConverter.GetLastRotationMatrix()` alone: the 3x3 rotation is copied into the
position-position and velocity-velocity blocks and the two off-diagonal blocks stay zero
(the loop at roughly lines 2252–2263 in the R2026a source). `CoordinateConverter` already
exposes `GetLastRotationDotMatrix()` (`src/base/coordsystem/CoordinateConverter.hpp`), and
the state conversion path uses it, but `GetCovarianceRmat66()` never calls it.

## Reproduction

Files in this directory:

- `run_repro.py`: writes `repro.script`, runs it through the GMAT Python API, parses the two
  report files and prints the comparison. Usage:

  ```bash
  python run_repro.py "/path/to/GMAT R2026a/bin"
  ```

- `repro.script`: the generated GMAT script, runnable on its own in GMAT (the two
  `ReportFile` paths are absolute and point into this directory).
- `expected_output.txt`: the output observed on the environment above.

The script places a spacecraft in a 500 km circular orbit, declares a covariance in
`EarthMJ2000Eq` with 1 km (1 sigma) position uncertainty per axis and **exactly zero
velocity uncertainty**, propagates one second, and reports:

- the state in `EarthMJ2000Eq` and in `EarthFixed`,
- `Sat.EarthMJ2000Eq.OrbitErrorCovariance` and `Sat.EarthFixed.OrbitErrorCovariance`.

Declaring the covariance inside the mission sequence, after `DisplayStateType = Cartesian`,
avoids the "Coordinate conversions may only be performed on Cartesian Covariance matrices"
error that `GetCovarianceRmat66()` raises otherwise.

## Observed

| Quantity | Value |
|---|---|
| Speed in `EarthMJ2000Eq` | 7.608469 km/s |
| Speed in `EarthFixed` | 7.262489 km/s |
| Difference, order of magnitude of ω_E × r | 0.50 km/s |
| Declared covariance diagonal (`EarthMJ2000Eq`) | 1, 1, 1, 0, 0, 0 |
| Reported covariance diagonal in `EarthFixed` | 1, 1, 1, **0, 0, 0** |
| Reported `EarthFixed` position-velocity cross terms | all exactly 0 |

The state conversion changes the speed by half a kilometre per second, which is the
rotation-rate term at work. The covariance conversion of the same object at the same epoch
reports **zero** velocity uncertainty in the rotating system.

## Expected

With `v_fixed = R v + Ṙ r`, a position uncertainty `σ_r` in a system rotating at `ω`
produces a velocity uncertainty of order `ω σ_r`. For Earth's rotation rate
(7.2921159e-5 rad/s) and `σ_r` = 1 km:

| Quantity | Expected |
|---|---|
| Velocity 1 sigma per in-plane axis | ≈ 7.29e-5 km/s = 0.0729 m/s |
| Velocity variance per in-plane axis | ≈ 5.3e-9 km²/s² |
| Position-velocity cross terms | non-zero, `Ṙ P_rr Rᵀ` |

Zero velocity uncertainty in `EarthFixed` for a spacecraft whose position is uncertain by a
kilometre is not physically possible: the frame's rotation converts position uncertainty
into velocity uncertainty, exactly as it converts the position vector into the extra
half-kilometre-per-second in the state report.

## Independent check on a full covariance

On a separate two-hour LEO arc with a full diagonal covariance (100 to 200 m position,
0.1 to 0.2 m/s velocity), the reported `EarthFixed` covariance was compared with
`[[R, 0], [Ṙ, R]] P [[R, 0], [Ṙ, R]]ᵀ` built from the rotation and rotation-rate
matrices GMAT itself returns for that conversion:

| Block | Relative disagreement |
|---|---|
| Position-position | 8.7e-17 (agrees) |
| Position-velocity cross | 1.3e-6 (linear in ω_E) |
| Velocity-velocity | 9.2e-11 (quadratic in ω_E) |

The pattern of the disagreement, exact agreement in the position block and errors scaling
with the first and second power of the angular rate in the other blocks, is the signature
of the missing `Ṙ` term rather than numerical noise.

## Suggested fix

In `OrbitData::GetCovarianceRmat66()`, after the successful `Convert`, also take
`Rmatrix33 rotDot = mCoordConverter.GetLastRotationDotMatrix();` and fill the lower-left
block: `transform(ii + 3, jj) = rotDot(ii, jj);`. For non-rotating target systems `Ṙ` is
zero and the result is unchanged; for rotating ones the velocity and cross blocks become
consistent with the state conversion. The reproduction above then reports non-zero
`EarthFixed` velocity variances of about 5.3e-9 km²/s² for the 1 km position-only case.

## Impact

Any `ReportFile`, `Subscriber` or downstream consumer of `<Spacecraft>.<CoordinateSystem>.
OrbitErrorCovariance` where the coordinate system rotates relative to the covariance's
declared system (body-fixed systems, `ObjectReferenced` systems with a moving reference)
receives velocity and cross-covariance blocks that understate the uncertainty. Position
blocks are unaffected. Reports in inertial systems related by a constant rotation are
unaffected.

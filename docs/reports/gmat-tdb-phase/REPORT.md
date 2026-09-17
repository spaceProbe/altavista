# GMAT R2026a `TimeSystemConverter`: a suspected TDB periodic-term phase error does not reproduce

> **This bundle is a negative result and is kept as one.** Question 225 commissioned an upstream
> bug report about GMAT's TDB periodic-term phase; the reproduction was written, run, and found
> no defect in GMAT. Nothing here is proposed for submission to the GMAT project. The directory
> stays so the claim, the reproduction that tested it, and the reason it failed remain on the
> record; the repository's own artifacts that produced the false finding were corrected in the
> commit after this one. The lead corrected question 225 accordingly.

## Summary

This repository's own N2 work (`crates/av-orbital/src/tdb.rs`, `goldens/gen_tdb_check.py`,
`docs/native-dynamics-plan.md`'s "A GMAT finding: the TDB periodic term's phase") suspected
that GMAT R2026a's `TimeSystemConverter` computes the TDB-TT periodic term with the
mean-anomaly argument's phase wrong by 288.6879178644 degrees -- attributed there to GMAT
internally forming `T_TT` from its own Modified Julian Date convention
(`GMAT_MJD = JD - 2,430,000.0`) while subtracting the J2000 *Julian* Date constant
`T_TT_OFFSET = 2,451,545.0`.

**This does not reproduce.** `Sat.TDBModJulian - Sat.TTModJulian`, read from a plain GMAT
script at five epochs spanning 2020-2033, matches the CORRECT (undisplaced) published series
-- GMAT's own exposed `M_E_OFFSET` used directly -- to within 6.023e-07 s at every epoch
(`docs/reports/gmat-tdb-phase/expected_output.txt`, this bundle's own reproduction), and never
comes within 1.629e-03 s of the phase-shifted series. The 288.6879178644-degree phase shift is
real and exactly reproducible, but only by calling
`TimeSystemConverter::Convert(origValue, fromType, toType, refJd)` with `refJd=0.0` -- which is
not `Convert()`'s own default (`GmatTimeConstants::JD_JAN_5_1941`, i.e. 2,430,000.0, read from
the R2026a source: `third_party/gmat-src/src/gmatutil/util/TimeSystemConverter.hpp:124`) and
not what any call site in GMAT's own R2026a source passes. `goldens/gen_tdb_check.py` calls
`Convert(a1mjd, A1MJD, TDBMJD, 0.0)` with that explicit `0.0`; that call, not GMAT's
`TimeSystemConverter`, is the source of the disagreement `crate::tdb`'s tests measure against
`goldens/tdb_check.json`.

## Environment

- GMAT R2026a, macOS 26.6.2 (Apple silicon), driven through the GMAT Python API (`gmatpy`)
  with `LoadScript` and `RunScript`. `repro.script` is plain GMAT script and runs unchanged
  in the GMAT GUI or console.
- A source checkout of GMAT R2026a is present on this host at `third_party/gmat-src` (the
  headers `crates/gmat-sys/build.rs` compiles its shim against -- see "Where the transform is
  built" below for how this was confirmed and used).
- No third-party Python dependency: `run_repro.py` imports only `math`, `os`, `sys`,
  `pathlib`, and `gmatpy`. No network access at any point.

## Where the transform is built

A GMAT R2026a source checkout is present at `third_party/gmat-src` in this worktree
(`crates/gmat-sys/build.rs` compiles its C++ shim against `third_party/gmat-src/src/base` and
`third_party/gmat-src/src/gmatutil`, confirmed by reading that file directly), so this section
cites real file, function and line, not a description of observed behaviour.

The periodic term itself: `third_party/gmat-src/src/gmatutil/util/TimeSystemConverter.cpp`,
`TimeSystemConverter::ConvertFromTaiMjd(Integer toType, Real origValue, Real refJd, bool
*insideLeapSec)`, the `case TDBMJD` block at lines 706-727:

```cpp
Real tttOffset = T_TT_OFFSET - refJd;
Real t_TT = (origValue - tttOffset) / T_TT_COEFF1;
Real m_E = (M_E_OFFSET + (M_E_COEFF1 * t_TT)) * GmatMathConstants::RAD_PER_DEG;
Real offset = ((TDB_COEFF1 * Sin(m_E)) + (TDB_COEFF2 * Sin(2 * m_E))) / GmatTimeConstants::SECS_PER_DAY;
Real tdbJd = ttJd + offset;
return tdbJd;
```

`origValue` here is a TAI Modified Julian Date on GMAT's own internal MJD scale (referenced to
`refJd`), and `refJd` is a caller-supplied parameter, not a fixed internal constant. The public
entry point, `TimeSystemConverter::Convert(const Real origValue, const Integer fromType, const
Integer toType, Real refJd, bool *insideLeapSec)`, declares `refJd`'s default in the header
(`third_party/gmat-src/src/gmatutil/util/TimeSystemConverter.hpp:121-125`):

```cpp
Real Convert(const Real origValue, const Integer fromType, const Integer toType,
             Real refJd = GmatTimeConstants::JD_JAN_5_1941, bool *insideLeapSec = NULL);
```

`GmatTimeConstants::JD_JAN_5_1941 = 2430000.0` exactly
(`third_party/gmat-src/src/gmatutil/util/GmatConstants.hpp:151`, `"const Real JD_JAN_5_1941 =
2430000.0; // old name JULIAN_DATE_OF_010541"`) -- the same GMAT-MJD-to-JD offset this
platform's own `av_cdm::time` and `av_orbital::tdb` document and use.

Every TDB conversion actually reachable from a running GMAT that this checkout's source was
searched for passes that same default explicitly, never `0.0`:

- `Sat.TDBModJulian`, the parameter this bundle's `repro.script` reports:
  `TDBModJulian::Evaluate()` (`third_party/gmat-src/src/base/parameter/TimeParameters.cpp:909`)
  calls `TimeData::GetTimeReal(TDB)`
  (`third_party/gmat-src/src/base/parameter/TimeData.cpp:285-289`):
  ```cpp
  case TDB:
     time = theTimeConverter->Convert(a1Mjd, TimeSystemConverter::A1MJD,
                                       TimeSystemConverter::TDBMJD,
                                       GmatTimeConstants::JD_JAN_5_1941);
     break;
  ```
  and `theTimeConverter` (`TimeData.cpp:86,121`) is `TimeSystemConverter::Instance()`, the same
  singleton `gmat.TimeSystemConverter.Instance()` reaches through `gmatpy`.
- GMAT's own DE-ephemeris file reader, `DeFile::GetPosVel()`
  (`third_party/gmat-src/src/base/solarsys/DeFile.cpp:383-385`, and the `GmatTime` overload at
  548-550), which converts the request epoch from A1MJD to TDBMJD before looking up the DE
  file -- i.e. the actual "ephemeris path" question 225 asks about:
  ```cpp
  double mjdTDB = (double) theTimeConverter->Convert(atTime.Get(),
                  TimeSystemConverter::A1MJD, TimeSystemConverter::TDBMJD,
                  GmatTimeConstants::JD_JAN_5_1941);
  ```
- `CelestialBody.cpp`, `SpiceInterface.cpp`, and `GmatCommand.cpp` each convert to `TDBMJD`
  with `GmatTimeConstants::JD_JAN_5_1941` as well (grep of every `TDBMJD` occurrence next to a
  `Convert(` call in the checkout).

`goldens/gen_tdb_check.py` (this repo, not GMAT) is the one call site found anywhere that
passes `0.0`: `tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD,
gmat.TimeSystemConverter.TDBMJD, 0.0)`.

## Reproduction

Files in this directory:

- `run_repro.py`: writes `repro.script`, runs it through the GMAT Python API, reads back
  `Sat.A1ModJulian`, `Sat.TTModJulian` and `Sat.TDBModJulian` at five epochs, evaluates the
  published TDB-TT series in pure Python two ways (GMAT's own `M_E_OFFSET` used directly, and
  that same offset shifted by a phase this script derives arithmetically from GMAT's own live
  constants -- never a pasted literal), and compares all three. It also calls
  `TimeSystemConverter.Instance().Convert()` directly with both `refJd=2430000.0` and
  `refJd=0.0` at each epoch, to confirm which one `goldens/gen_tdb_check.py`'s golden actually
  recorded. Usage:

  ```bash
  python run_repro.py "/path/to/GMAT R2026a/bin"
  ```

- `repro.script`: the generated GMAT script, runnable on its own in GMAT (the `ReportFile`
  path is absolute and points into this directory).
- `expected_output.txt`: the literal output observed on the environment above.

`repro.script` places a spacecraft in a circular LEO orbit under a trivial two-body force
model (`Degree = Order = 0`, no drag, no SRP) and, for each of five epochs (01 Jan 2020, 2023,
2026, 2029, 2033), sets `Sat.Epoch` and propagates one second (`SolverIterations = Current` on
the `ReportFile`, since `Execute` skips `ReportFile`s otherwise), producing two report rows per
epoch a second apart; `run_repro.py` takes the first row of each pair, i.e. the state as of
`Sat.Epoch` itself. Five widely-separated epochs make a constant phase error visible as a
mismatch at every epoch alike, rather than a drift that only shows up over time.

## Observed

From `expected_output.txt` (`GMAT TDB-TT` is `Sat.TDBModJulian - Sat.TTModJulian` in seconds;
`correct series` and `GMAT-phase series` are the two pure-Python evaluations):

| Epoch (UTC) | GMAT TDB-TT (s) | correct series (s) | \|diff\| (s) | GMAT-phase series (s) | \|diff\| (s) |
|---|---|---|---|---|---|
| 01 Jan 2020 | -9.335344657e-05 | -9.275116726e-05 | 6.023e-07 | 1.548542228e-03 | 1.642e-03 |
| 01 Jan 2023 | -8.643837646e-05 | -8.634952912e-05 | 8.885e-08 | 1.550800705e-03 | 1.637e-03 |
| 01 Jan 2026 | -8.046627045e-05 | -7.994658047e-05 | 5.197e-07 | 1.553036337e-03 | 1.634e-03 |
| 01 Jan 2029 | -7.355120033e-05 | -7.354241847e-05 | 8.782e-09 | 1.555249098e-03 | 1.629e-03 |
| 01 Jan 2033 | -7.480848581e-05 | -7.465888333e-05 | 1.496e-07 | 1.554865014e-03 | 1.630e-03 |

Maximum disagreement with the correct series over the five epochs: 6.023e-07 s -- at the
precision floor `ReportFile`'s own `Precision = 16` text output sets for an MJD-magnitude
(~2-3e4) number, roughly 8.6e-7 s per digit. Minimum disagreement with the GMAT-phase series:
1.629e-03 s, three orders of magnitude larger and of consistent sign and magnitude with the
series' own ~1.67 ms amplitude.

The direct `Convert()` calls in `expected_output.txt` confirm the mechanism: `refJd=2430000.0`
reproduces `Sat.TDBModJulian` (e.g. 01 Jan 2026: -7.983762771e-05 s, matching the report's
-8.046627045e-05 s to the same ~1e-6 s ReportFile-precision floor as the series comparison
above), and `refJd=0.0` reproduces `goldens/tdb_check.json`'s recorded value for the same
epoch almost exactly (1.553061884e-03 s here versus 0.0015530618838965893 recorded in that
file -- the two calls are, allowing for `gmatpy` argument formatting, identical).

## Expected

If the suspected defect were real, `Sat.TDBModJulian - Sat.TTModJulian` -- the only TDB value
an actual GMAT run (script, GUI, or console) ever produces -- would match the GMAT-phase
series, not the correct one, since that is the series GMAT's `Convert()` would then be
computing internally regardless of how it is called. It does not: at all five epochs it
matches the correct series to the ReportFile output's own text precision and disagrees with
the GMAT-phase series by nearly two orders of magnitude more than that floor.

## Independent check: closing the loop from symptom to cause

Two additional facts, both printed by `run_repro.py` and captured in `expected_output.txt`,
rule out coincidence:

1. The 288.6879178644-degree phase shift is derived arithmetically in this script from GMAT's
   own live-read constants (`rate = M_E_COEFF1 / T_TT_COEFF1 = 0.985600283094 deg/day`;
   `2,430,000 * rate = 2,395,008.687918 deg`; `mod 360 = 288.6879178644 deg`) -- exactly
   `GmatTimeConstants::JD_JAN_5_1941` days of the mean-anomaly rate, the constant this report's
   "Where the transform is built" section identifies as the one place `refJd` differs between
   `goldens/gen_tdb_check.py`'s call and every call site read in GMAT's own source.
2. Calling `TimeSystemConverter.Instance().Convert(a1mjd, A1MJD, TDBMJD, 0.0)` directly, at
   the same five epochs, reproduces the GMAT-phase series' values (and `goldens/tdb_check.json`
   itself) to the same sub-microsecond floor the correct series matches `Sat.TDBModJulian` at.
   `refJd=0.0` is sufficient, on its own, to fully explain the suspected defect; no other
   difference between `goldens/gen_tdb_check.py`'s call and GMAT's own internal calls remains.

## The further observation, and how this finding fits it

`docs/native-dynamics-plan.md`'s N2 section records that switching this crate's own
`tai_ns_to_tdb_jd` from a fitted `M_E_OFFSET` to GMAT's own exposed `M_E_OFFSET` (the "correct"
series in this report's own terms) *reduced* this platform's native third-body acceleration
disagreement against GMAT's own `ODEModel::GetDerivatives` from 1.206205e-14 to 3.972128e-15
m/s^2, onto the same floor the gravity-only arc sits at -- measured by
`crates/av-orbital/tests/thirdbody_goldens.rs`'s `leo_1day_jgm2_8x8_sunmoon_acceleration_agreement`
test (that crate's own N2 report). Those two numbers are round 1's measurements, not
reproduced by this bundle, which exercises only `TimeSystemConverter` directly; they are cited
here, not re-run, per this round's own instruction.

At the time that observation was recorded, it looked like evidence that "GMAT's ephemeris path
uses a TDB its own exposed `TimeSystemConverter` does not return" -- a live inconsistency
inside GMAT. This report's finding removes the mystery rather than confirming it: GMAT's own
DE-ephemeris reader, `DeFile::GetPosVel()` (cited above), converts to TDB with the same
`refJd=GmatTimeConstants::JD_JAN_5_1941` that `Sat.TDBModJulian` uses -- the correct series, by
this report's own reproduction. The native third-body model agreeing better with GMAT's actual
force derivatives after switching to the correct series is exactly what this report's finding
predicts: GMAT's ephemeris path was already using the correct series the whole time, and
switching this crate's own code to match it, rather than the `goldens/gen_tdb_check.py`
`refJd=0.0` artifact, made native and GMAT agree better. There is no discovered instance,
anywhere the R2026a source was searched, of GMAT's own code calling `Convert(..., TDBMJD,
0.0)`.

## Suggested fix

There is nothing to fix in GMAT. `TimeSystemConverter::Convert()` computes the textbook
Fairhead & Bretagnon / Vallado series correctly with its own default `refJd`, and every call
site inside GMAT's own R2026a source passes that default (or the equivalent explicit
`GmatTimeConstants::JD_JAN_5_1941`). The corrective action belongs to this repository, not
upstream, and is out of scope for this report (this bundle is confined to
`docs/reports/gmat-tdb-phase/` and does not modify `crates/`, `goldens/`, or
`docs/native-dynamics-plan.md`):

- `goldens/gen_tdb_check.py` should call `Convert(a1mjd, A1MJD, TDBMJD)` (its own default) or
  `Convert(a1mjd, A1MJD, TDBMJD, 2430000.0)` explicitly, not `0.0`, and be regenerated.
- `crates/av-orbital/src/tdb.rs`'s "Root cause" narrative, its
  `tai_ns_to_tdb_minus_tt_seconds_gmat_phase` function, `GMAT_M_E_PHASE_SHIFT_DEG`, and the
  `crates/av-orbital/tests/tdb_check.rs` assertions built on `goldens/tdb_check.json` all
  encode this same `refJd=0.0` artifact and should be revisited once the golden is
  regenerated correctly.
- `docs/native-dynamics-plan.md`'s "A GMAT finding: the TDB periodic term's phase" subsection
  describes the same artifact as a GMAT defect and should be corrected.

None of this affects `tai_ns_to_tdb_jd`, the function this crate's third-body code actually
calls in production: it already uses GMAT's own `M_E_OFFSET` directly with a correctly-formed
`T_TT` (full Julian Date, not GMAT's internal MJD, subtracted from `T_TT_OFFSET`) -- the same
computation this report's "correct series" performs, and the same one GMAT's own
`Sat.TDBModJulian` and `DeFile::GetPosVel()` perform. The platform's actual TDB output was not
affected by the `refJd=0.0` artifact; only the test narrative describing *why* it agrees with
GMAT was.

## Impact

None on GMAT. `TimeSystemConverter::Convert()` behaves correctly for every caller inside GMAT,
including the one this report's `repro.script` exercises (`Sat.TDBModJulian`) and the one
question 225 asked about (`DeFile::GetPosVel()`, GMAT's DE-ephemeris reader). A caller of
`Convert(..., TDBMJD, refJd)` supplying its own `refJd` should be aware that only
`GmatTimeConstants::JD_JAN_5_1941` (or omitting the argument, which defaults to it) is
consistent with GMAT's own internal usage for an `origValue` on GMAT's internal MJD scale;
`0.0` is not documented anywhere in the header as meaningful for that input scale and produces
a periodic-term phase error of 288.6879178644 degrees, worth up to ~1.7 ms of `TDB-TT`, exactly
as this report measures.

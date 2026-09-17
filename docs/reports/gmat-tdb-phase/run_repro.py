"""Reproduction attempt: does GMAT R2026a's `TimeSystemConverter` compute the TDB-TT
periodic term with the mean-anomaly argument's phase wrong by ~289 degrees (as suspected in
this repo's own N2 work: `crates/av-orbital/src/tdb.rs`, `goldens/gen_tdb_check.py`,
`docs/native-dynamics-plan.md`'s "A GMAT finding: the TDB periodic term's phase")?

This script writes `repro.script` (a plain GMAT script, runnable on its own in the GMAT
GUI/console), runs it through the GMAT Python API, reads back GMAT's own `Sat.TTModJulian`
and `Sat.TDBModJulian` at five widely-separated epochs, and compares that against two
evaluations of the published TDB-TT series computed in pure Python from GMAT's own exposed
`TimeSystemConverter` constants:

  - the CORRECT (undisplaced) series, using GMAT's own `M_E_OFFSET` directly;
  - the "GMAT-phase" series, `M_E_OFFSET` shifted by a phase this script derives
    arithmetically from the other constants (never a pasted literal).

**Finding: this does not reproduce.** `Sat.TDBModJulian - Sat.TTModJulian` matches the
CORRECT series at every epoch (to the sub-microsecond floor set by `ReportFile`'s own text
precision), and matches the GMAT-phase series nowhere. The previously suspected ~289-degree
phase shift IS exactly reproducible -- but only by calling
`TimeSystemConverter::Convert(..., TDBMJD, refJd)` with `refJd=0.0`, which is not
`TimeSystemConverter::Convert`'s own default (`GmatTimeConstants::JD_JAN_5_1941`, i.e.
2,430,000.0) and not what any call site inside GMAT's own source passes. This script also
performs that `refJd=0.0` call directly (still through `gmatpy`, no separate dependency) to
show it reproduces the suspected phase, closing the loop from symptom to cause.

Usage: python run_repro.py [GMAT_BIN]   (default: the GMAT R2026a bin directory beside this
repo). Requires only GMAT's own Python API (gmatpy) and the standard library (`os`, `sys`,
`math`, `pathlib`). No other dependency, no network.
"""
import math
import os
import sys
from pathlib import Path

GMAT_BIN = sys.argv[1] if len(sys.argv) > 1 else "/Users/probe/code/AltaVista/GMAT R2026a/bin"
sys.path.insert(1, GMAT_BIN)
import gmatpy as gmat  # noqa: E402
gmat.Setup(os.path.join(GMAT_BIN, "api_startup_file.txt"))

here = Path(__file__).parent.resolve()
script = here / "repro.script"
rpt = here / "repro.rpt"

EPOCHS = [
    "01 Jan 2020 00:00:00.000",
    "01 Jan 2023 00:00:00.000",
    "01 Jan 2026 00:00:00.000",
    "01 Jan 2029 00:00:00.000",
    "01 Jan 2033 00:00:00.000",
]

epoch_blocks = "\n".join(
    f"Sat.Epoch = '{e}';\nPropagate Prop(Sat) {{Sat.ElapsedSecs = 1}};\n" for e in EPOCHS
)

SCRIPT = f"""\
% Reproduction attempt for a suspected TDB periodic-term phase defect in GMAT's
% TimeSystemConverter (see this directory's REPORT.md). A trivial two-body force model and
% five widely-separated one-second propagations, one per epoch, so the report gets one clean
% row per epoch spanning 13 years -- enough to tell a constant phase error (which this script
% does NOT find) from anything epoch-dependent.
Create Spacecraft Sat;
Sat.DateFormat = UTCGregorian;
Sat.CoordinateSystem = EarthMJ2000Eq;
Sat.DisplayStateType = Cartesian;
Sat.X = 6878.137;
Sat.Y = 0;
Sat.Z = 0;
Sat.VX = 0;
Sat.VY = 7.6;
Sat.VZ = 0;

Create ForceModel FM;
FM.CentralBody = Earth;
FM.PrimaryBodies = {{Earth}};
FM.GravityField.Earth.Degree = 0;
FM.GravityField.Earth.Order = 0;
FM.Drag = None;
FM.SRP = Off;
Create Propagator Prop;
Prop.FM = FM;
Prop.Type = RungeKutta89;

Create ReportFile Rep;
Rep.Filename = '{rpt}';
Rep.Precision = 16;
Rep.WriteHeaders = false;
Rep.Delimiter = ',';
Rep.SolverIterations = Current;
Rep.Add = {{Sat.A1ModJulian, Sat.TTModJulian, Sat.TDBModJulian}};

BeginMissionSequence;

{epoch_blocks}\
"""
script.write_text(SCRIPT)
if rpt.exists():
    rpt.unlink()

cwd = os.getcwd()
try:
    os.chdir(GMAT_BIN)
    if not gmat.LoadScript(str(script)):
        msg = gmat.GetLastMessage() if hasattr(gmat, "GetLastMessage") else ""
        raise SystemExit(f"GMAT failed to load {script}: {msg}")
    if not gmat.RunScript():
        raise SystemExit(f"GMAT failed to run {script}")
finally:
    os.chdir(cwd)

rows = [ln.strip() for ln in rpt.read_text().splitlines() if ln.strip()]
# Two rows per epoch (the propagate segment's start and end, 1 second apart -- utterly
# negligible for an annual-period effect never larger than 1.7 ms); take the first of each
# pair, i.e. the state as of Sat.Epoch itself.
parsed = [tuple(float(x) for x in ln.replace(",", " ").split()) for ln in rows]
per_epoch = parsed[0::2]
if len(per_epoch) != len(EPOCHS):
    raise SystemExit(f"expected {len(EPOCHS)} epoch rows (2 rows each), got {len(rows)} rows total: {rows}")

# --- GMAT's own live TimeSystemConverter constants (not typed from a textbook) ---
tc = gmat.TimeSystemConverter.Instance()
TDB_COEFF1 = tc.TDB_COEFF1
TDB_COEFF2 = tc.TDB_COEFF2
M_E_OFFSET = tc.M_E_OFFSET
M_E_COEFF1 = tc.M_E_COEFF1
T_TT_OFFSET = tc.T_TT_OFFSET
T_TT_COEFF1 = tc.T_TT_COEFF1
print("Live gmat.TimeSystemConverter.Instance() constants:")
print(f"  TDB_COEFF1  = {TDB_COEFF1!r}")
print(f"  TDB_COEFF2  = {TDB_COEFF2!r}")
print(f"  M_E_OFFSET  = {M_E_OFFSET!r}")
print(f"  M_E_COEFF1  = {M_E_COEFF1!r}")
print(f"  T_TT_OFFSET = {T_TT_OFFSET!r}")
print(f"  T_TT_COEFF1 = {T_TT_COEFF1!r}")
print()

# GMAT's own internal MJD convention: GMAT_MJD = JD - GmatTimeConstants::JD_JAN_5_1941.
# GmatTimeConstants is not exposed through gmatpy (checked: gmat.GmatTimeConstants does not
# exist), so this one constant is read from the source instead of the live instance --
# third_party/gmat-src/src/gmatutil/util/GmatConstants.hpp:151,
# "const Real JD_JAN_5_1941 = 2430000.0;" -- and matches this repo's own
# av_cdm::time / av_orbital::tdb documented GMAT-MJD convention (2_430_000.0) exactly.
GMAT_MJD_TO_JD_OFFSET = 2_430_000.0

# --- Derive the phase shift arithmetically (never a pasted literal) ---
rate_deg_per_day = M_E_COEFF1 / T_TT_COEFF1
phase_shift_deg_raw = GMAT_MJD_TO_JD_OFFSET * rate_deg_per_day
phase_shift_deg = phase_shift_deg_raw % 360.0
gmat_phase_effective_offset_deg = (M_E_OFFSET - phase_shift_deg) % 360.0
print("Derivation of the suspected phase shift (arithmetic, from the constants above):")
print(f"  rate = M_E_COEFF1 / T_TT_COEFF1                    = {rate_deg_per_day:.12f} deg/day")
print(f"  {GMAT_MJD_TO_JD_OFFSET:.0f} days x rate                       = {phase_shift_deg_raw:.6f} deg")
print(f"  mod 360                                             = {phase_shift_deg:.10f} deg")
print(f"  M_E_OFFSET - phase_shift_deg (mod 360)              = {gmat_phase_effective_offset_deg:.10f} deg")
print()


def series_tdb_minus_tt(t_tt_centuries, m_e_offset_deg):
    m_e_deg = m_e_offset_deg + M_E_COEFF1 * t_tt_centuries
    m_e_rad = math.radians(m_e_deg)
    return TDB_COEFF1 * math.sin(m_e_rad) + TDB_COEFF2 * math.sin(2.0 * m_e_rad)


print(f"{'epoch (UTC)':<24}{'GMAT TDB-TT (s)':>18}{'correct series (s)':>20}{'|diff| (s)':>14}{'GMAT-phase series (s)':>24}{'|diff| (s)':>14}")
max_diff_correct = 0.0
min_diff_gmat_phase = None
for label, (a1mjd, tt_mjd, tdb_mjd) in zip(EPOCHS, per_epoch):
    gmat_tdb_minus_tt = (tdb_mjd - tt_mjd) * 86400.0

    # T_TT in Julian centuries of TT since J2000, computed CORRECTLY: GMAT's own
    # Sat.TTModJulian is unaffected by the refJd argument (its formula is a fixed constant
    # offset from TAI, see this report's "Where the transform is built"), so converting it to
    # a full Julian Date via GMAT's own MJD convention and then to the J2000 epoch gives the
    # right T_TT with no dependency on the disputed refJd call at all.
    jd_tt_full = tt_mjd + GMAT_MJD_TO_JD_OFFSET
    t_tt_centuries = (jd_tt_full - T_TT_OFFSET) / T_TT_COEFF1

    correct_tdb_minus_tt = series_tdb_minus_tt(t_tt_centuries, M_E_OFFSET)
    gmat_phase_tdb_minus_tt = series_tdb_minus_tt(t_tt_centuries, gmat_phase_effective_offset_deg)

    diff_correct = abs(gmat_tdb_minus_tt - correct_tdb_minus_tt)
    diff_gmat_phase = abs(gmat_tdb_minus_tt - gmat_phase_tdb_minus_tt)
    max_diff_correct = max(max_diff_correct, diff_correct)
    min_diff_gmat_phase = diff_gmat_phase if min_diff_gmat_phase is None else min(min_diff_gmat_phase, diff_gmat_phase)

    print(f"{label:<24}{gmat_tdb_minus_tt:>18.9e}{correct_tdb_minus_tt:>20.9e}{diff_correct:>14.3e}{gmat_phase_tdb_minus_tt:>24.9e}{diff_gmat_phase:>14.3e}")

print()
print(f"max |GMAT report - correct series|    over 5 epochs = {max_diff_correct:.3e} s")
print(f"min |GMAT report - GMAT-phase series| over 5 epochs = {min_diff_gmat_phase:.3e} s")
print()

# --- Close the loop: does refJd=0.0 reproduce the GMAT-phase series? ---
print("Direct TimeSystemConverter.Instance().Convert() calls, explicit refJd argument:")
print(f"{'epoch (UTC)':<24}{'refJd=2430000.0 (TC default)':>30}{'refJd=0.0 (goldens/gen_tdb_check.py)':>40}")
A1MJD = gmat.TimeSystemConverter.A1MJD
TTMJD = gmat.TimeSystemConverter.TTMJD
TDBMJD = gmat.TimeSystemConverter.TDBMJD
refjd_zero_matches_gmat_phase = True
refjd_default_matches_report = True
for label, (a1mjd, tt_mjd, tdb_mjd) in zip(EPOCHS, per_epoch):
    tt_default = tc.Convert(a1mjd, A1MJD, TTMJD, GMAT_MJD_TO_JD_OFFSET)
    tdb_default = tc.Convert(a1mjd, A1MJD, TDBMJD, GMAT_MJD_TO_JD_OFFSET)
    diff_default_s = (tdb_default - tt_default) * 86400.0

    tt_zero = tc.Convert(a1mjd, A1MJD, TTMJD, 0.0)
    tdb_zero = tc.Convert(a1mjd, A1MJD, TDBMJD, 0.0)
    diff_zero_s = (tdb_zero - tt_zero) * 86400.0

    print(f"{label:<24}{diff_default_s:>30.9e}{diff_zero_s:>40.9e}")

    jd_tt_full = tt_mjd + GMAT_MJD_TO_JD_OFFSET
    t_tt_centuries = (jd_tt_full - T_TT_OFFSET) / T_TT_COEFF1
    gmat_phase_tdb_minus_tt = series_tdb_minus_tt(t_tt_centuries, gmat_phase_effective_offset_deg)
    if abs(diff_zero_s - gmat_phase_tdb_minus_tt) > 1e-6:
        refjd_zero_matches_gmat_phase = False
    report_tdb_minus_tt = (tdb_mjd - tt_mjd) * 86400.0
    if abs(diff_default_s - report_tdb_minus_tt) > 1e-6:
        refjd_default_matches_report = False

print()
print("Where the phase shift comes from: passing refJd=0.0 to TimeSystemConverter::Convert()")
print("(the default is GmatTimeConstants::JD_JAN_5_1941 = 2,430,000.0) leaves the periodic")
print("term's T_TT computed from an origValue still on GMAT's internal MJD scale, short by")
print(f"exactly the {GMAT_MJD_TO_JD_OFFSET:.0f}-day offset derived above -- reproducing the GMAT-phase series")
print("to sub-microsecond precision, and reproducing GMAT's own Sat.TDBModJulian nowhere.")
print()

ok = max_diff_correct < 1e-6 and min_diff_gmat_phase > 1e-4 and refjd_zero_matches_gmat_phase and refjd_default_matches_report
if ok:
    print(
        "NOT A GMAT DEFECT: Sat.TDBModJulian - Sat.TTModJulian matches the correct "
        "(undisplaced) series at every epoch and disagrees with the GMAT-phase series by "
        f"{min_diff_gmat_phase:.3e} s or more; the GMAT-phase series is reproduced only by "
        "passing refJd=0.0 explicitly, which is not TimeSystemConverter::Convert()'s own "
        "default and not what any call site read in GMAT's own R2026a source passes."
    )
else:
    print("REPRODUCED: GMAT's own Sat.TDBModJulian disagrees from the correct series and/or matches the GMAT-phase series -- see the numbers above.")

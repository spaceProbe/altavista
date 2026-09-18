"""Generate `tdb_check.json`: GMAT's own TAI(A.1)->TT->TDB conversion at five epochs spanning
the `leo_1day_jgm2_8x8_sunmoon` golden's arc (docs/native-dynamics-plan.md milestone N2, step
3: "test it against GMAT's own epoch handling ... rather than a number typed from a book").

Run explicitly, never from a test:
    .venv/bin/python goldens/gen_tdb_check.py --reason "..." --tolerance-s <measured value>

This is not a dynamics arc, so it carries no `tolerance_m`/`tolerance_mps` the way the orbit
goldens do; it carries its own `tolerance_s` instead (round 1's decision 3: the tolerance
committed with a golden is the tolerance in force, measured first and recorded here, never a
copy inherited by the test) -- see `--tolerance-s` below and `crates/av-orbital/tests/tdb_check.rs`,
the sole consumer, which reads `tolerance_s` from this file rather than asserting a value of
its own.

`gmat.TimeSystemConverter.Instance()` exposes the exact coefficients GMAT's own truncated
TDB-TT series uses (`TDB_COEFF1`, `TDB_COEFF2`, `M_E_OFFSET`, `M_E_COEFF1`, `T_TT_OFFSET`,
`T_TT_COEFF1`) -- recorded here too, so `crates/av-orbital/src/tdb.rs`'s own module doc can
name its source precisely (these are GMAT's own values, not independently typed from a
textbook) and a future change to GMAT's own constants would be caught by this file changing
under regeneration.

**Corrected here, per `docs/reports/gmat-tdb-phase/REPORT.md`**: every `tc.Convert(...)` call
below now passes `refJd=2_430_000.0` explicitly
(`GmatTimeConstants::JD_JAN_5_1941`, `third_party/gmat-src/src/gmatutil/util/GmatConstants.hpp:151`)
-- `TimeSystemConverter::Convert`'s own default (declared in
`third_party/gmat-src/src/gmatutil/util/TimeSystemConverter.hpp:121-125`) and the value every
call site inside GMAT's own R2026a source passes (`Sat.TDBModJulian` via `TimeData.cpp`,
`DeFile::GetPosVel()`, `CelestialBody.cpp`, `SpiceInterface.cpp`, `GmatCommand.cpp`). It is
passed explicitly, not omitted, so this script does not depend on how `gmatpy` happens to
surface (or fail to surface) a C++ default argument, and so nobody looking at this file has to
go read the C++ header to know what value is in effect.

**The previous revision of this script passed `refJd=0.0` instead.** That is not a GMAT
defect: `docs/reports/gmat-tdb-phase/REPORT.md` reproduces GMAT's `TimeSystemConverter`
directly (via a plain GMAT script, `Sat.TDBModJulian`, and a source read of R2026a's own
`TimeSystemConverter.cpp`) and finds that `refJd=0.0` -- not any default of `Convert()`, and
not anything GMAT's own code ever passes -- leaves the periodic term's mean-anomaly argument
short by exactly 2,430,000 days of the `M_E_COEFF1` rate, a phase error of exactly
288.6879178644 degrees. `Sat.TDBModJulian` (and GMAT's DE-ephemeris reader, `DeFile::GetPosVel()`)
match the CORRECT, undisplaced series to within ~6e-7 s (GMAT's own `ReportFile` text
precision) at every epoch that report checked; only this script's own `refJd=0.0` call, not
GMAT, produced the disagreement the previous golden recorded. See that report for the full
reproduction, source citations, and measurements.
"""
import argparse
import datetime
import hashlib
import json
import sys
from pathlib import Path

GMAT_ROOT = Path("/Users/probe/code/AltaVista/GMAT R2026a")
sys.path.insert(1, str(GMAT_ROOT / "bin"))
import gmatpy as gmat  # noqa: E402

gmat.Setup(str(GMAT_ROOT / "bin" / "api_startup_file.txt"))

# The same epoch and one-day arc as goldens/leo_1day_jgm2_8x8_sunmoon.json (epoch_a1mjd
# 31041.500428638676, duration_s 86400.0), sampled at t0, +1/3, +2/3 and +1 day -- the same
# four fractions tests/gravity_goldens.rs's measure_acceleration_agreement uses, so this
# fixture directly supports the N2 acceleration-agreement test's epoch set too.
EPOCH_A1MJD_0 = 31041.500428638676
DURATION_S = 86400.0
FRACTIONS = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]
GOLDEN_NAME = "tdb_check"
# The empirical-calibration set: one point every 7 days for 10 years, enough to resolve the
# ~1-year periodic term's full cycle many times over and catch a secular (non-periodic) drift
# the short one-day arc above could never see.
YEAR_SCAN_STEP_DAYS = 7
YEAR_SCAN_COUNT = 522  # 0..3650 days inclusive by 7

# `TimeSystemConverter::Convert`'s own default `refJd` (its header's own declared default,
# and the value every GMAT-internal call site passes) -- see this module's doc. Passed
# explicitly below, everywhere, rather than omitted, precisely so this script does not depend
# on `gmatpy`'s own handling of a C++ default argument.
REF_JD_GMAT_DEFAULT = 2_430_000.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this fixture is (re)generated; recorded in the file")
    ap.add_argument(
        "--tolerance-s",
        type=float,
        required=True,
        help=(
            "the tolerance (seconds) crates/av-orbital/tests/tdb_check.rs reads from this "
            "file and asserts against -- measure the actual agreement first (run the test, "
            "read its printed RMS/max), then pass that measured value here; never a value "
            "copied from a paper or left at a script default."
        ),
    )
    args = ap.parse_args()

    tc = gmat.TimeSystemConverter.Instance()
    coeffs = {
        "TDB_COEFF1": tc.TDB_COEFF1,
        "TDB_COEFF2": tc.TDB_COEFF2,
        "M_E_OFFSET": tc.M_E_OFFSET,
        "M_E_COEFF1": tc.M_E_COEFF1,
        "T_TT_OFFSET": tc.T_TT_OFFSET,
        "T_TT_COEFF1": tc.T_TT_COEFF1,
        "L_B": tc.L_B,
    }

    epochs = []
    for frac in FRACTIONS:
        a1mjd = EPOCH_A1MJD_0 + (DURATION_S * frac) / 86400.0
        tai_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TAIMJD, REF_JD_GMAT_DEFAULT)
        tt_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TTMJD, REF_JD_GMAT_DEFAULT)
        tdb_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TDBMJD, REF_JD_GMAT_DEFAULT)
        epochs.append({
            "fraction_of_arc": frac,
            "epoch_a1mjd": a1mjd,
            "tai_mjd": tai_mjd,
            "tt_mjd": tt_mjd,
            "tdb_mjd": tdb_mjd,
            "tdb_minus_tt_s": (tdb_mjd - tt_mjd) * 86400.0,
        })

    year_scan = []
    for i in range(YEAR_SCAN_COUNT):
        days = i * YEAR_SCAN_STEP_DAYS
        a1mjd = EPOCH_A1MJD_0 + days
        tt_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TTMJD, REF_JD_GMAT_DEFAULT)
        tdb_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TDBMJD, REF_JD_GMAT_DEFAULT)
        year_scan.append({
            "days_from_epoch_a1mjd_0": days,
            "epoch_a1mjd": a1mjd,
            "tdb_minus_tt_s": (tdb_mjd - tt_mjd) * 86400.0,
        })

    doc = {
        "name": GOLDEN_NAME,
        "generated": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "reason": args.reason,
        "gmat_version": "R2026a",
        "source": "gmat.TimeSystemConverter.Instance() -- Convert(A1MJD -> TAIMJD/TTMJD/TDBMJD, refJd=2430000.0) at each epoch, and the singleton's own TDB series coefficients",
        "ref_jd": REF_JD_GMAT_DEFAULT,
        "tolerance_s": args.tolerance_s,
        "arc_reference_golden": "leo_1day_jgm2_8x8_sunmoon",
        "epoch_a1mjd_0": EPOCH_A1MJD_0,
        "duration_s": DURATION_S,
        "gmat_tdb_series_coefficients": coeffs,
        "epochs": epochs,
        "year_scan_note": "one point every 7 days for 10 years (522 points) of GMAT's own TT/TDB conversion, tdb_minus_tt_s only, all with refJd=2430000.0 (GmatTimeConstants::JD_JAN_5_1941, Convert()'s own default) -- see docs/reports/gmat-tdb-phase/REPORT.md and this script's own module doc.",
        "year_scan": year_scan,
    }
    body = json.dumps(doc, indent=2, sort_keys=True)
    doc["sha256"] = hashlib.sha256(body.encode()).hexdigest()
    out = Path(__file__).with_name(GOLDEN_NAME + ".json")
    out.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
    print("wrote", out)
    for e in epochs:
        print(e)


if __name__ == "__main__":
    main()

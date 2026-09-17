"""Generate `tdb_check.json`: GMAT's own TAI(A.1)->TT->TDB conversion at five epochs spanning
the `leo_1day_jgm2_8x8_sunmoon` golden's arc (docs/native-dynamics-plan.md milestone N2, step
3: "test it against GMAT's own epoch handling ... rather than a number typed from a book").

Run explicitly, never from a test:  .venv/bin/python goldens/gen_tdb_check.py --reason "..."

This is not a dynamics arc, so it carries no `tolerance_m`/`tolerance_mps` the way the orbit
goldens do; the tolerance for the native TAI->TDB conversion this pins against is measured
and recorded directly in `crates/av-orbital/tests/tdb_check.rs`, the sole consumer.

`gmat.TimeSystemConverter.Instance()` exposes the exact coefficients GMAT's own truncated
TDB-TT series uses (`TDB_COEFF1`, `TDB_COEFF2`, `M_E_OFFSET`, `M_E_COEFF1`, `T_TT_OFFSET`,
`T_TT_COEFF1`) -- recorded here too, so `crates/av-orbital/src/tdb.rs`'s own module doc can
name its source precisely (these are GMAT's own values, not independently typed from a
textbook) and a future change to GMAT's own constants would be caught by this file changing
under regeneration.

**Finding, recorded here because it changes what `tdb.rs` actually does**: applying the
exposed `M_E_OFFSET` directly in the standard formula (`M_E = M_E_OFFSET + M_E_COEFF1 *
T_TT_centuries`, `T_TT_centuries` from `T_TT_OFFSET`/`T_TT_COEFF1`) does NOT reproduce GMAT's
own `Convert(..., TDBMJD, ...)` output -- measured disagreement ~1.6 ms (roughly the full
series amplitude), a ~288-degree phase error, not floating-point noise.

**Root-caused (not merely fit): GMAT computes the periodic term's `T_TT` from its own internal
Modified Julian Date convention (`GMAT_MJD = JD - 2_430_000.0`) while subtracting the J2000
*Julian* Date constant `T_TT_OFFSET = 2451545.0`, leaving its mean-anomaly argument short by
exactly 2,430,000 days of the `M_E_COEFF1` rate -- a pure constant phase error.** This lands at
a derived offset of `68.8398054354` degrees, `4.108e-5` degrees (~1.2 ns of `TDB-TT`) from an
earlier version's least-squares fit against this file's own 522-point, 10-year `year_scan`
(`68.8398465155` degrees, RMS residual 1.535e-07 s = 153.5 ns) -- the fit and the derivation
were measuring the same effect. `crates/av-orbital/src/tdb.rs` now derives GMAT's phase from
its own constants (see that module's doc, "Root cause") rather than carrying a fitted literal,
and deliberately uses the CORRECT (undisplaced) series in production, keeping GMAT's own phase
available under its own name specifically so the disagreement can be measured and asserted
(`crates/av-orbital/tests/tdb_check.rs`) rather than silently matched.
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
# The empirical-calibration set (this module doc's "Measured finding"): one point every 7
# days for 10 years, enough to resolve the ~1-year periodic term's full cycle many times over
# and catch a secular (non-periodic) drift the short one-day arc above could never see.
YEAR_SCAN_STEP_DAYS = 7
YEAR_SCAN_COUNT = 522  # 0..3650 days inclusive by 7 -- matches the fit run in this crate's N2 report


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reason", required=True, help="why this fixture is (re)generated; recorded in the file")
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
        tai_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TAIMJD, 0.0)
        tt_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TTMJD, 0.0)
        tdb_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TDBMJD, 0.0)
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
        tt_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TTMJD, 0.0)
        tdb_mjd = tc.Convert(a1mjd, gmat.TimeSystemConverter.A1MJD, gmat.TimeSystemConverter.TDBMJD, 0.0)
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
        "source": "gmat.TimeSystemConverter.Instance() -- Convert(A1MJD -> TAIMJD/TTMJD/TDBMJD) at each epoch, and the singleton's own TDB series coefficients",
        "arc_reference_golden": "leo_1day_jgm2_8x8_sunmoon",
        "epoch_a1mjd_0": EPOCH_A1MJD_0,
        "duration_s": DURATION_S,
        "gmat_tdb_series_coefficients": coeffs,
        "epochs": epochs,
        "year_scan_note": "one point every 7 days for 10 years (522 points) of GMAT's own TT/TDB conversion, tdb_minus_tt_s only -- this crate's N2 report used this exact set (regenerated identically by this script) both to originally empirically fit, and now to confirm the root-caused, derived replacement for, the M_E_OFFSET phase GMAT's own Convert() actually applies internally (68.8398054354 degrees derived, vs the earlier fit's 68.8398465155 degrees, both because the raw M_E_OFFSET GMAT exposes does not reproduce GMAT's own Convert() output when used directly) -- see crates/av-orbital/src/tdb.rs's own module doc, 'Root cause', and this script's own module doc above.",
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

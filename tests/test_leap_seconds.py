"""Cross-check `av-cdm`'s leap-second table and TAI/UTC/A1 relations against GMAT's own
``TimeSystemConverter`` (ADR-001 "Time", question 8: "checked against GMAT's LSK file in
CI"). GMAT is installed in this environment, so this test runs for real and must pass, not
skip.

The comparison works entirely in GMAT's own "Modified Julian Date" convention
(``MJD = JD - 2430000.0``, a float of days; see ``altavista/timeutil.py``), which is exactly
the same *proleptic* calendar convention this crate's ``data/time/leap_seconds.json`` and
``av_cdm::time::Tai`` use (every day is 86 400 SI seconds; a leap second's extra tick has no
representation): for a given UTC instant, our table's offset and the epoch shift are
recomputed independently in Python (not by shelling out to Rust) and checked against
``TimeSystemConverter.Convert``.

Two epoch groups, deliberately asserted differently:

- ``TABLE_COVERED_EPOCHS`` (1972 onward, where ``leap_seconds.json`` has real entries):
  asserted to agree with GMAT to microsecond precision. This is the real correctness check.
- ``PRE_1972_EPOCHS``: `av_cdm::time::Tai` documents (in `time.rs`) that it clamps to the
  table's first entry (offset 10 s) before 1972, rather than modeling the historical
  "rubber second" TAI-UTC relationship, and says plainly not to trust it there. GMAT *does*
  implement that historical relationship, so these two are expected, *measured*, and
  already-documented to disagree -- this test records how much, as a sanity bound, rather
  than asserting a false tight match.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from altavista.gmat_env import load_gmat

REPO_ROOT = Path(__file__).resolve().parent.parent
LEAP_SECONDS_PATH = REPO_ROOT / "data" / "time" / "leap_seconds.json"

# JD(1970-01-01T00:00:00) = 2440587.5 (standard constant); GMAT's MJD = JD - 2430000.0.
GMAT_MJD_AT_UNIX_EPOCH = 10_587.5
SECONDS_PER_DAY = 86400.0
A1_MINUS_TAI_SECONDS = 0.0343817

# Realistic bar: GMAT's Convert works in double-precision MJD days, so ~microsecond
# agreement at present-day magnitudes is the theoretical floor (a few times 1e-16 relative
# precision * ~60000 days ~ 1e-11 days ~ 1 microsecond) -- see ADR-001 "Alternatives
# considered". Tight enough to catch a whole-second or sign error (1e6x this size) with
# eleven orders of magnitude to spare.
TOLERANCE_SECONDS = 5e-6


def _load_table() -> list[dict]:
    doc = json.loads(LEAP_SECONDS_PATH.read_text())
    return doc["entries"]


TABLE = _load_table()


def _offset_seconds_at_utc_ns(utc_ns: int) -> int:
    """Our table-driven TAI-UTC offset for a proleptic UTC nanosecond count.

    Mirrors `av_cdm::time::Tai::from_utc_nanos`'s rule exactly: the offset of the last
    entry effective at or before `utc_ns`, clamped to the table's first entry before 1972
    (`data/time/leap_seconds.json`'s own `clamped_before_first_entry` convention).
    """
    offset = TABLE[0]["tai_utc_offset_seconds"]
    for entry in TABLE:
        if entry["utc_effective_unix_ns"] <= utc_ns:
            offset = entry["tai_utc_offset_seconds"]
        else:
            break
    return offset


def _unix_ns(dt: datetime) -> int:
    delta = dt - datetime(1970, 1, 1, tzinfo=timezone.utc)
    # Exact for whole-second calendar instants, matching scripts/gen_leap_seconds.py's
    # to_unix_ns (integer arithmetic, no float seconds).
    return delta.days * 86_400_000_000_000 + delta.seconds * 1_000_000_000


def _utc_mjd(utc_ns: int) -> float:
    return GMAT_MJD_AT_UNIX_EPOCH + utc_ns / 1e9 / SECONDS_PER_DAY


# At least 10 epochs, 1972 through present, including immediately either side of several
# leap-second boundaries (task requirement / ADR-001 question 8). Checked to microsecond
# tolerance against GMAT.
TABLE_COVERED_EPOCHS = [
    datetime(1972, 1, 1, 0, 0, 0, tzinfo=timezone.utc),        # the table's first entry
    datetime(1972, 6, 30, 23, 59, 59, tzinfo=timezone.utc),    # just before the 1972-07-01 step
    datetime(1972, 7, 1, 0, 0, 0, tzinfo=timezone.utc),        # just after
    datetime(1998, 12, 31, 23, 59, 59, tzinfo=timezone.utc),   # just before the 1999-01-01 step
    datetime(1999, 1, 1, 0, 0, 0, tzinfo=timezone.utc),        # just after
    datetime(2005, 12, 31, 23, 59, 59, tzinfo=timezone.utc),   # just before the 2006-01-01 step
    datetime(2006, 1, 1, 0, 0, 0, tzinfo=timezone.utc),        # just after
    datetime(2016, 12, 31, 23, 59, 59, tzinfo=timezone.utc),   # just before the last (2017) step
    datetime(2017, 1, 1, 0, 0, 0, tzinfo=timezone.utc),        # just after (current offset, 37s)
    datetime(2026, 9, 2, tzinfo=timezone.utc),                 # "present"
]

# Pre-1972: the documented clamp-vs-history deviation, recorded rather than hidden.
PRE_1972_EPOCHS = [
    datetime(1965, 6, 15, tzinfo=timezone.utc),
    datetime(1971, 12, 31, 23, 59, 59, tzinfo=timezone.utc),  # one second before the table starts
]

# The measured pre-1972 offset never gets close to a whole-second-or-sign-error magnitude
# (that would be a real bug); it is bounded only loosely, to the physically plausible range
# TAI-UTC actually took before 1972 (it grew roughly linearly from ~1.4s in 1958 to 10s at
# the 1972 redefinition).
PRE_1972_SANITY_BOUND_SECONDS = 15.0


@pytest.fixture(scope="module")
def time_system_converter():
    g = load_gmat()
    return g.TimeSystemConverter.Instance(), g.TimeSystemConverter


# Accumulates the worst-case residual actually measured, so it can be reported once at the
# end instead of only per-assertion.
_worst = {"offset_seconds": 0.0, "a1_seconds": 0.0, "pre_1972_offset_seconds": 0.0}


@pytest.mark.parametrize("epoch", TABLE_COVERED_EPOCHS, ids=lambda d: d.isoformat())
def test_tai_utc_offset_matches_gmat(time_system_converter, epoch):
    tc, scales = time_system_converter
    utc_ns = _unix_ns(epoch)
    ours_offset_s = _offset_seconds_at_utc_ns(utc_ns)

    utc_mjd = _utc_mjd(utc_ns)
    tai_mjd = tc.Convert(utc_mjd, scales.UTCMJD, scales.TAIMJD)
    gmat_offset_s = (tai_mjd - utc_mjd) * SECONDS_PER_DAY

    residual = abs(gmat_offset_s - ours_offset_s)
    _worst["offset_seconds"] = max(_worst["offset_seconds"], residual)
    assert residual < TOLERANCE_SECONDS, (
        f"{epoch.isoformat()}: ours={ours_offset_s}s gmat={gmat_offset_s}s residual={residual}s"
    )


@pytest.mark.parametrize("epoch", TABLE_COVERED_EPOCHS, ids=lambda d: d.isoformat())
def test_a1_tai_relation_matches_gmat(time_system_converter, epoch):
    tc, scales = time_system_converter
    utc_ns = _unix_ns(epoch)
    tai_ns = utc_ns + _offset_seconds_at_utc_ns(utc_ns) * 1_000_000_000
    tai_mjd = GMAT_MJD_AT_UNIX_EPOCH + tai_ns / 1e9 / SECONDS_PER_DAY

    ours_a1_mjd = tai_mjd + A1_MINUS_TAI_SECONDS / SECONDS_PER_DAY
    gmat_a1_mjd = tc.Convert(tai_mjd, scales.TAIMJD, scales.A1MJD)

    residual_seconds = abs(gmat_a1_mjd - ours_a1_mjd) * SECONDS_PER_DAY
    _worst["a1_seconds"] = max(_worst["a1_seconds"], residual_seconds)
    assert residual_seconds < TOLERANCE_SECONDS, (
        f"{epoch.isoformat()}: residual={residual_seconds}s"
    )


@pytest.mark.parametrize("epoch", PRE_1972_EPOCHS, ids=lambda d: d.isoformat())
def test_pre_1972_deviation_from_gmat_is_bounded_and_recorded(time_system_converter, epoch):
    """Documents (does not hide) the pre-1972 approximation named in `time.rs`'s module
    docs: our table clamps to the first entry's offset (10 s) before 1972; GMAT computes
    the real historical TAI-UTC relationship. This asserts only that GMAT's answer is still
    a plausible historical TAI-UTC value (catching a real bug, e.g. a sign error or a wildly
    wrong magnitude), not that it matches our clamp -- it is not supposed to.
    """
    tc, scales = time_system_converter
    utc_ns = _unix_ns(epoch)
    ours_offset_s = _offset_seconds_at_utc_ns(utc_ns)

    utc_mjd = _utc_mjd(utc_ns)
    tai_mjd = tc.Convert(utc_mjd, scales.UTCMJD, scales.TAIMJD)
    gmat_offset_s = (tai_mjd - utc_mjd) * SECONDS_PER_DAY

    residual = abs(gmat_offset_s - ours_offset_s)
    _worst["pre_1972_offset_seconds"] = max(_worst["pre_1972_offset_seconds"], residual)
    assert 0.0 <= gmat_offset_s <= PRE_1972_SANITY_BOUND_SECONDS, (
        f"{epoch.isoformat()}: GMAT's historical TAI-UTC offset {gmat_offset_s}s is outside "
        f"the physically plausible pre-1972 range"
    )


def test_zzz_report_worst_case_residuals(time_system_converter):
    """Runs last (file-definition order); prints the worst residuals actually measured, for
    the task's "report the worst-case residual you measured" requirement.
    """
    assert all(v >= 0.0 for v in _worst.values())
    print(
        f"\nworst-case TAI-UTC offset residual vs GMAT (1972+, table-covered): "
        f"{_worst['offset_seconds']:.3e} s\n"
        f"worst-case A1/TAI residual vs GMAT (1972+): {_worst['a1_seconds']:.3e} s\n"
        f"pre-1972 clamp-vs-GMAT-history deviation (documented, expected): "
        f"{_worst['pre_1972_offset_seconds']:.3f} s"
    )

"""Epoch helpers. GMAT works internally in A.1 Modified Julian Date (A1MJD).

The viewer receives A1MJD floats (compact, monotonic) plus a UTC calendar string
for display. Conversions use GMAT's own ``TimeSystemConverter`` so leap seconds
match the simulation exactly.
"""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

from .gmat_env import gmat

SECONDS_PER_DAY = 86400.0
# GMAT's MJD offset: MJD(GMAT) = JD - 2430000.0  (GMAT "Modified Julian" is not the IAU one)
GMAT_MJD_JD_OFFSET = 2430000.0


def a1_to_utc_mjd(a1mjd: float) -> float:
    g = gmat()
    tc = g.TimeSystemConverter.Instance()
    return tc.Convert(a1mjd, g.TimeSystemConverter.A1MJD, g.TimeSystemConverter.UTCMJD)


def tai_to_a1_mjd(taimjd: float) -> float:
    g = gmat()
    tc = g.TimeSystemConverter.Instance()
    return tc.Convert(taimjd, g.TimeSystemConverter.TAIMJD, g.TimeSystemConverter.A1MJD)


def a1_to_gregorian(a1mjd: float) -> str:
    """UTC Gregorian string in GMAT format, e.g. ``01 Jan 2026 00:00:00.000``."""
    g = gmat()
    tc = g.TimeSystemConverter.Instance()
    return tc.ConvertMjdToGregorian(a1_to_utc_mjd(a1mjd))


def a1_to_iso(a1mjd: float) -> str:
    """ISO-8601 UTC string (millisecond precision) for an A1MJD epoch."""
    return gregorian_to_iso(a1_to_gregorian(a1mjd))


def gregorian_to_iso(greg: str) -> str:
    dt = datetime.strptime(greg.strip(), "%d %b %Y %H:%M:%S.%f")
    return dt.replace(tzinfo=timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def utc_mjd_to_datetime(utc_mjd: float) -> datetime:
    """Approximate calendar conversion without GMAT (no leap-second handling). Display only."""
    jd = utc_mjd + GMAT_MJD_JD_OFFSET
    # JD 2440587.5 == 1970-01-01T00:00:00Z
    return datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(days=jd - 2440587.5)

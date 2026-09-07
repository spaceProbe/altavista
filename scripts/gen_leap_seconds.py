#!/usr/bin/env python3
"""Generate ``data/time/leap_seconds.json`` from GMAT's SPICE leap-second kernel.

Parses the ``DELTET/DELTA_AT`` block of a ``.tls`` text kernel (NAIF SPK/SPICE
leap-seconds kernel format) -- pairs of ``<TAI-UTC offset in whole seconds>,
@<UTC calendar date>`` -- and writes a versioned, self-describing, deterministically
ordered JSON document that a consumer (the Rust ``av_cdm::time::Tai`` table, or
anything else) can use to convert between TAI and UTC in either direction without
re-deriving the boundary from the kernel text.

Usage::

    .venv/bin/python scripts/gen_leap_seconds.py \\
        "GMAT R2026a/data/time/SPICELeapSecondKernel.tls" \\
        data/time/leap_seconds.json

Both arguments are optional; the defaults above are used when omitted, resolved
relative to the repository root (this script's grandparent directory).

Design notes
------------
- The table is a step function of UTC: ``DELTA_AT`` holds from the listed date
  (inclusive) until the next entry's date. Before the first entry (1972-01-01,
  the start of the whole-second leap-second era) SPICE's own ``deltet_`` routine
  clamps to the first tabulated value rather than applying the pre-1972 "rubber
  second" formula; this generator records the same convention explicitly (see
  ``clamped_before_first_entry`` in the output) instead of leaving it implicit.
- Each entry carries both the UTC instant the offset takes effect (as an ISO-8601
  string and as integer nanoseconds since the Unix epoch, UTC) and the
  corresponding TAI instant (integer nanoseconds, on the same epoch convention:
  "nanoseconds since 1970-01-01T00:00:00, reckoned on the given scale"). TAI is
  computed as ``utc_effective_unix_ns + offset_seconds * 1e9`` -- the new offset
  is the one in force at and after the boundary, so this is the TAI instant
  simultaneous with the UTC boundary.
- The document is deterministic: entries are emitted in the kernel's own
  chronological order (already ascending), field order is fixed, and no
  wall-clock "generated at" timestamp is embedded -- re-running this script
  against an unchanged kernel produces a byte-identical file.
"""
from __future__ import annotations

import datetime as _dt
import hashlib
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SOURCE = REPO_ROOT / "GMAT R2026a" / "data" / "time" / "SPICELeapSecondKernel.tls"
DEFAULT_OUTPUT = REPO_ROOT / "data" / "time" / "leap_seconds.json"

SCHEMA_VERSION = "1.0.0"

_MONTHS = {
    "JAN": 1, "FEB": 2, "MAR": 3, "APR": 4, "MAY": 5, "JUN": 6,
    "JUL": 7, "AUG": 8, "SEP": 9, "OCT": 10, "NOV": 11, "DEC": 12,
}

# Matches one "<offset>, @<YYYY>-<MON>-<D>" pair inside the DELTET/DELTA_AT block,
# e.g. "10,   @1972-JAN-1" or "20,   @1981-JUL-1".
_ENTRY_RE = re.compile(
    r"(?P<offset>\d+)\s*,\s*@(?P<year>\d{4})-(?P<mon>[A-Z]{3})-(?P<day>\d{1,2})"
)


def parse_delta_at(text: str) -> list[tuple[int, _dt.datetime]]:
    """Parse the ``DELTET/DELTA_AT = ( ... )`` block into ``(offset_s, utc_datetime)`` pairs."""
    marker = "DELTET/DELTA_AT"
    start = text.index(marker)
    open_paren = text.index("(", start)
    close_paren = text.index(")", open_paren)
    block = text[open_paren + 1 : close_paren]

    entries = []
    for m in _ENTRY_RE.finditer(block):
        offset = int(m.group("offset"))
        year = int(m.group("year"))
        mon = _MONTHS[m.group("mon")]
        day = int(m.group("day"))
        when = _dt.datetime(year, mon, day, tzinfo=_dt.timezone.utc)
        entries.append((offset, when))

    if not entries:
        raise ValueError(f"no DELTA_AT entries parsed from {marker} block")

    # The kernel lists entries in ascending chronological order; assert it rather
    # than silently re-sorting, since a re-sort would hide a malformed kernel.
    for (_, a), (_, b) in zip(entries, entries[1:]):
        if not a < b:
            raise ValueError(f"DELTA_AT entries are not strictly ascending: {a} >= {b}")

    return entries


def to_unix_ns(dt: _dt.datetime) -> int:
    """Exact integer nanoseconds since the Unix epoch for a UTC calendar instant.

    Uses integer arithmetic throughout (no float seconds) so whole-second
    calendar dates convert exactly.
    """
    delta = dt - _dt.datetime(1970, 1, 1, tzinfo=_dt.timezone.utc)
    return delta.days * 86_400_000_000_000 + delta.seconds * 1_000_000_000


def build_document(source_path: Path) -> dict:
    text = source_path.read_text()
    sha256 = hashlib.sha256(source_path.read_bytes()).hexdigest()
    parsed = parse_delta_at(text)

    entries = []
    for offset_s, when in parsed:
        utc_ns = to_unix_ns(when)
        tai_ns = utc_ns + offset_s * 1_000_000_000
        entries.append(
            {
                "tai_utc_offset_seconds": offset_s,
                "utc_effective": when.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "utc_effective_unix_ns": utc_ns,
                "tai_effective_ns": tai_ns,
            }
        )

    return {
        "version": SCHEMA_VERSION,
        "source": {
            "file": source_path.name,
            "sha256": sha256,
        },
        "generated_from": (
            "DELTET/DELTA_AT block of the SPICE leap-second kernel, parsed by "
            "scripts/gen_leap_seconds.py"
        ),
        "clamped_before_first_entry": (
            "A UTC instant before the first entry's utc_effective uses that "
            "first entry's offset (SPICE deltet_ convention); the pre-1972 "
            "rubber-second UTC formula (DELTET/DELTA_T_A, K, EB, M) is not "
            "represented in this table."
        ),
        "entries": entries,
    }


def main(argv: list[str]) -> int:
    source_path = Path(argv[1]) if len(argv) > 1 else DEFAULT_SOURCE
    output_path = Path(argv[2]) if len(argv) > 2 else DEFAULT_OUTPUT

    if not source_path.is_file():
        print(f"error: source kernel not found: {source_path}", file=sys.stderr)
        return 1

    doc = build_document(source_path)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w") as f:
        json.dump(doc, f, indent=2, sort_keys=False)
        f.write("\n")

    last = doc["entries"][-1]
    print(f"wrote {output_path} with {len(doc['entries'])} entries")
    print(f"source sha256: {doc['source']['sha256']}")
    print(f"last entry: {last}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

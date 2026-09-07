"""M23.2 (docs/sil-plan.md M23, docs/open-questions.md question 143): mechanically proves
`services/cfs/psp-lockstep` never reads a wall clock, rather than relying on code review alone
to catch a regression. Every symbol here is a real way C code on macOS/Linux can read the wall
clock or fake elapsed time with a sleep; a wrong implementation that "cheats" by sleeping for
the requested interval, or by reading `CLOCK_REALTIME`/`CLOCK_MONOTONIC` to compute elapsed time
instead of taking it from the kernel's own `until_tai_ns`, fails this test even though it might
otherwise look correct in isolation.
"""
from __future__ import annotations

import re
from pathlib import Path

_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
_LINE_COMMENT_RE = re.compile(r"//.*")


def strip_c_comments(text: str) -> str:
    """Strips comments before scanning for forbidden symbols -- this module's own doc comments
    *name* `time()`/`gettimeofday()`/etc. as things the implementation must not do, and a naive
    substring scan over the raw source would flag its own documentation as a violation."""
    return _LINE_COMMENT_RE.sub("", _BLOCK_COMMENT_RE.sub("", text))

FORBIDDEN_SYMBOLS = [
    "time(",
    "gettimeofday",
    "clock_gettime",
    "CLOCK_REALTIME",
    "CLOCK_MONOTONIC",
    "CLOCK_BOOTTIME",
    "sleep(",
    "usleep(",
    "nanosleep",
    "clock()",
    "timer_create",
    "timer_settime",
    "alarm(",
]

CFS_DIR = Path(__file__).resolve().parent.parent
CLOCK_SOURCES = [
    CFS_DIR / "psp-lockstep" / "src" / "psp_lockstep.c",
    CFS_DIR / "psp-lockstep" / "inc" / "psp_lockstep.h",
    CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src" / "lockstep_local_framing.c",
    CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src" / "lockstep_local_io.c",
    CFS_DIR / "apps" / "io_lockstep" / "fsw" / "src" / "io_lockstep_app.c",
    CFS_DIR / "apps" / "sch_lockstep" / "fsw" / "src" / "sch_lockstep_app.c",
]


def test_psp_lockstep_sources_exist() -> None:
    for path in CLOCK_SOURCES:
        assert path.is_file(), f"expected source file missing: {path}"


def test_psp_lockstep_never_reads_a_wall_clock() -> None:
    offenders: list[str] = []
    for path in CLOCK_SOURCES:
        text = strip_c_comments(path.read_text())
        for symbol in FORBIDDEN_SYMBOLS:
            if symbol in text:
                offenders.append(f"{path.name}: contains {symbol!r}")
    assert not offenders, "wall-clock/sleep symbol(s) found in the lockstep clock path:\n" + "\n".join(offenders)

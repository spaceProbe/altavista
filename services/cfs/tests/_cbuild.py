"""Shared C build/run helper for the services/cfs framing tests (question 172).

**Why this exists.** Three of these tests compile a small C program and run it under a 10 s
timeout, and they failed intermittently on the lead's gate run. The cause was measured, not
inferred: on this macOS 26 host under load a freshly compiled hello-world took **0.13 s to
compile, 72.0 s on its first launch, and 9 ms on its second**, with the system policy daemon at
57% CPU assessing the many new binaries this project's workers, cargo and Docker produce. **The C
code is not at fault**; a first-launch assessment stall was being charged to a 10 s run budget.

Question 172's decision, implemented here:

1. **Cache the compiled binary by source hash across runs** -- so a binary that has already been
   assessed is reused instead of being rebuilt (and re-assessed) every session.
2. **Launch each freshly built binary once with a generous, untimed warm-up** before the timed
   run, so the assessment cost lands outside the measured window.
3. **Name question 172 in the timeout reason**, so a future timeout points at this decision
   instead of looking like a C bug.
"""

from __future__ import annotations

import hashlib
import subprocess
from pathlib import Path

# Cache lives beside the tests, keyed by a hash of every input that affects the binary.
CACHE_DIR = Path(__file__).resolve().parent / ".cbuild-cache"

# Generous and untimed by design: the measured worst case for a first launch on this host was
# 72 s. This is the warm-up, not the assertion -- the real behaviour is asserted by the timed run.
WARMUP_TIMEOUT_S = 180.0

# The timed budget the tests actually assert against. Unchanged from before question 172: the
# point of the fix is to stop charging first-launch assessment to this number, not to widen it.
RUN_TIMEOUT_S = 10.0

TIMEOUT_REASON = (
    "timed out after {t}s. See docs/open-questions.md question 172: on this host a freshly "
    "compiled binary can stall for ~72 s on its FIRST launch while the system policy daemon "
    "assesses it (9 ms on the second). This helper pre-warms each newly built binary untimed and "
    "caches it by source hash, so a timeout here is a real failure of the program under test, not "
    "first-launch assessment."
)


def compile_cached(name: str, c_src: str, extra_sources: list[Path], include_dir: Path,
                   cflags: tuple[str, ...] = ("-std=c11", "-Wall", "-Wextra", "-Werror"),
                   link_args: tuple[str, ...] = ()) -> Path:
    """Compile `c_src` (plus `extra_sources`) to a cached binary, warming it if newly built."""
    h = hashlib.sha256()
    h.update(c_src.encode())
    for flag in cflags:
        h.update(flag.encode())
    h.update(str(include_dir).encode())
    for la in link_args:
        h.update(la.encode())
    for src in extra_sources:
        h.update(src.read_bytes())
        h.update(str(src).encode())
    key = h.hexdigest()[:16]

    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    binary_path = CACHE_DIR / f"{name}-{key}"
    if binary_path.exists():
        # Already built AND already warmed in the session that built it; the OS keeps its
        # assessment, which is exactly the 9 ms second-launch case.
        return binary_path

    src_path = CACHE_DIR / f"{name}-{key}.c"
    src_path.write_text(c_src)
    compiled = subprocess.run(
        ["cc", *cflags, f"-I{include_dir}", str(src_path),
         *[str(s) for s in extra_sources], "-o", str(binary_path), *link_args],
        capture_output=True, text=True,
    )
    assert compiled.returncode == 0, f"cc failed:\n{compiled.stdout}\n{compiled.stderr}"

    # Question 172 step 2: one untimed warm-up launch, so the assessment stall is paid here and
    # not inside the timed run below. Its result is deliberately ignored -- a program that fails
    # on its own terms is caught by the timed run, which is what asserts.
    try:
        subprocess.run([str(binary_path)], capture_output=True, text=True,
                       timeout=WARMUP_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        # A warm-up that itself exceeds 180 s is a real problem worth surfacing, not swallowing.
        raise AssertionError(
            f"{name}: warm-up launch exceeded {WARMUP_TIMEOUT_S}s -- far beyond the ~72 s "
            "first-launch assessment question 172 measured; treat this as a real hang."
        ) from None
    return binary_path


def run_compiled(binary_path: Path, timeout: float = RUN_TIMEOUT_S) -> subprocess.CompletedProcess:
    """Run an already-warmed binary under the timed budget, with question 172 in the reason."""
    try:
        return subprocess.run([str(binary_path)], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise AssertionError(f"{binary_path.name} " + TIMEOUT_REASON.format(t=timeout)) from None

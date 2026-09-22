"""Question 229's own proof: `scripts/dev/cargo-slot` serialises a THIRD concurrent invocation
behind the two host-wide `flock` slots it takes, never letting three run at once, and announces
the wait it forces rather than blocking silently -- see `scripts/dev/cargo-slot`'s own module
doc comment for the full mechanism (two slots under `$HOME/.altavista/locks/`, `os.execvp` into
the held slot's file descriptor so the flock survives a kill, and why a plain no-op-handler
`SIGALRM` recipe measurably does not interrupt a blocking `fcntl.flock` on this host).

# A private lock directory, not `$HOME` -- and why that is NOT a silent departure from convention

`crates/av-lockstep/src/docker_test_lock.rs`'s own module doc argues at length for locating its
lock file under `$HOME` specifically, because Colima (Docker's VM) mounts only `$HOME` into the
container runtime a Docker-gated test needs to reach. That argument has nothing to do with
`cargo-slot`: no Docker container is involved anywhere in this file, so there is no VM boundary
for a `tmp_path`-only lock file to fail to cross. `docs/open-questions.md` question 212(b) is
what actually governs here -- a test must never depend on, or contend for, the REAL host-wide
slots every other `cargo` invocation on this host also takes -- so every lock directory this
file uses is `AV_CARGO_SLOT_DIR`, pointed at a fresh directory under pytest's own `tmp_path`,
passed to each child exclusively through `subprocess`'s `env=` argument (never
`os.environ[...] = ...` on this test process itself -- question 199).

# Why the three children are synchronised the way they are (no fixed sleep, no 3-way blind race)

A literal blind race -- launch three `cargo-slot` invocations back to back and hope the OS
schedules the first two ahead of the third -- would occasionally schedule fewer than two of them
before the third's own non-blocking attempts run, making the "third one waits" assertion flaky
for a reason that has nothing to do with `cargo-slot`'s own correctness. Instead: the first two
children (A and B) are launched, and this test blocks on each one's own "HELD" line on its own
stdout -- a real OS event, written by the child only after `cargo-slot` has already `exec`'d it,
which only happens after `cargo-slot`'s own `flock` actually succeeded -- before launching the
third (C). This still genuinely proves the two-slot mechanism: A and B provably hold the lock
CONCURRENTLY (both confirmed HELD, both running, before C is even started), and C, launched only
once both slots are confirmed taken, is guaranteed to hit real contention rather than possibly
racing a slot free -- so its own WAITING announcement is asserted deterministically, not
probabilistically. The actual concurrency ceiling (never 3 at once) is then verified for real
from the three children's own measured `[start, end]` intervals, not assumed from the launch
order.

# What each child actually is

Each child is `python3 scripts/dev/cargo-slot -c "<script>"` with `AV_CARGO_SLOT_EXE` overridden
(test-only, see `cargo-slot`'s own doc comment) to `sys.executable`, so `cargo-slot` `exec`s into
`python3 -c "<script>"` instead of a real `cargo` build -- fast, harmless, and needs no network,
Docker or GMAT, while still holding the real slot for a real, measurable duration (the script
sleeps) the same way a real `cargo test` invocation would. This is exactly the "make the
wrapper's command configurable" option this task's own brief offers, rather than trying to
measure overlap against a real (multi-second, non-deterministic-length) compile.
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from altavista.test_env import drain_after_terminate

REPO_ROOT = Path(__file__).resolve().parents[1]
CARGO_SLOT = REPO_ROOT / "scripts" / "dev" / "cargo-slot"

HOLD_SECONDS = 0.8  # long enough that process-spawn jitter (tens of ms) cannot mask overlap

# Question 194's convention: a visible skip, never a silent one, naming exactly what is missing.
_env_with_rustup = dict(os.environ)
_env_with_rustup["PATH"] = f"/opt/homebrew/opt/rustup/bin:{_env_with_rustup.get('PATH', '')}"
_CARGO_MISSING_REASON = (
    None
    if shutil.which("cargo", path=_env_with_rustup["PATH"]) is not None
    else "cargo is not on PATH (export PATH=\"/opt/homebrew/opt/rustup/bin:$PATH\" first) -- "
    "scripts/dev/cargo-slot exists to gate real cargo invocations, so this test skips when "
    "there is no cargo on this host to gate at all (docs/open-questions.md question 194)"
)

# Each child: prints "HELD <start>" to its own stdout the instant it starts (a real,
# blocking-readable synchronisation event -- this only runs at all once cargo-slot's own exec
# has happened, which only happens once its flock succeeded), records its own [start, end] to a
# private result file, sleeps for HOLD_SECONDS to hold the slot measurably, then prints
# "DONE <end>" before exiting.
_CHILD_SCRIPT_TEMPLATE = """
import time
out_path = {out_path!r}
hold_s = {hold_s!r}
start = time.time()
print(f"HELD {{start!r}}", flush=True)
with open(out_path, "w") as f:
    f.write(repr(start) + "\\n")
time.sleep(hold_s)
end = time.time()
with open(out_path, "a") as f:
    f.write(repr(end) + "\\n")
print(f"DONE {{end!r}}", flush=True)
"""


def _child_env(private_lock_dir: Path) -> dict:
    """A copy of this process's own environment (never assigned back into `os.environ` --
    question 199) with the two `cargo-slot`-specific, test-only overrides set: a private lock
    directory (question 212(b): never the real, shared slots) and `python3` itself as the
    executable `cargo-slot` `exec`s into, in place of a real `cargo` build."""
    env = dict(os.environ)
    env["AV_CARGO_SLOT_DIR"] = str(private_lock_dir)
    env["AV_CARGO_SLOT_EXE"] = sys.executable
    return env


def _spawn_child(out_path: Path, env: dict) -> subprocess.Popen:
    script = _CHILD_SCRIPT_TEMPLATE.format(out_path=str(out_path), hold_s=HOLD_SECONDS)
    return subprocess.Popen(
        [sys.executable, str(CARGO_SLOT), "-c", script],
        cwd=REPO_ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _read_line_containing(pipe, needle: str, *, what: str) -> str:
    """Blocks on real OS events (the pipe's own `readline`) until a line containing `needle`
    appears -- never a sleep. Mirrors `tests/test_docker_test_lock_cross_process.py`'s own
    helper of the same name/shape."""
    while True:
        line = pipe.readline()
        assert line, f"{what}: the pipe closed before ever printing a line containing {needle!r}"
        if needle in line:
            return line.strip()


def _parse_interval(out_path: Path, *, label: str) -> "tuple[float, float]":
    text = out_path.read_text()
    lines = [ln for ln in text.splitlines() if ln.strip()]
    assert len(lines) == 2, f"{label}: expected exactly a start and end line in {out_path}, got {lines!r}"
    return float(lines[0]), float(lines[1])


def _max_concurrent(intervals: "list[tuple[float, float]]") -> int:
    """A real sweep-line count of the maximum number of the given [start, end] intervals that
    are simultaneously open at any instant -- not an assumption, a measurement (question 148)."""
    events = []
    for start, end in intervals:
        events.append((start, 1))
        events.append((end, -1))
    events.sort(key=lambda e: (e[0], e[1]))  # process an end (-1) before a start (+1) at a tie
    current = 0
    peak = 0
    for _, delta in events:
        current += delta
        peak = max(peak, current)
    return peak


@pytest.mark.skipif(_CARGO_MISSING_REASON is not None, reason=_CARGO_MISSING_REASON or "")
def test_three_concurrent_invocations_serialise_to_two(tmp_path):
    private_lock_dir = tmp_path / "locks"
    env = _child_env(private_lock_dir)

    real_home_locks = Path(os.environ["HOME"]) / ".altavista" / "locks"
    before = sorted(real_home_locks.iterdir()) if real_home_locks.is_dir() else []

    out_a, out_b, out_c = tmp_path / "a.out", tmp_path / "b.out", tmp_path / "c.out"

    proc_a = _spawn_child(out_a, env)
    proc_b = _spawn_child(out_b, env)
    try:
        # Block on each child's own real "HELD" event -- both A and B are now provably holding
        # a slot each, concurrently, before C is ever started.
        held_a = _read_line_containing(proc_a.stdout, "HELD", what="child A")
        held_b = _read_line_containing(proc_b.stdout, "HELD", what="child B")

        proc_c = _spawn_child(out_c, env)
        try:
            # C must hit real contention: both slots are confirmed held above, so its own
            # cargo-slot must announce a wait before it can acquire either one.
            waiting_line = _read_line_containing(proc_c.stderr, "WAITING", what="child C")

            done_a = _read_line_containing(proc_a.stdout, "DONE", what="child A")
            done_b = _read_line_containing(proc_b.stdout, "DONE", what="child B")
            rc_a = proc_a.wait(timeout=10)
            rc_b = proc_b.wait(timeout=10)

            acquired_line = _read_line_containing(proc_c.stderr, "ACQUIRED cargo-slot", what="child C")
            held_c = _read_line_containing(proc_c.stdout, "HELD", what="child C")
            done_c = _read_line_containing(proc_c.stdout, "DONE", what="child C")
            rc_c = proc_c.wait(timeout=10)
        finally:
            if proc_c.poll() is None:
                proc_c.kill()
    finally:
        for p in (proc_a, proc_b):
            if p.poll() is None:
                p.kill()

    assert rc_a == 0, f"child A must exit cleanly; stderr: {drain_after_terminate(proc_a)}"
    assert rc_b == 0, f"child B must exit cleanly; stderr: {drain_after_terminate(proc_b)}"
    assert rc_c == 0, f"child C must exit cleanly; stderr: {drain_after_terminate(proc_c)}"

    interval_a = _parse_interval(out_a, label="A")
    interval_b = _parse_interval(out_b, label="B")
    interval_c = _parse_interval(out_c, label="C")

    # Question 148: an exit code is not evidence -- print exactly what was measured.
    print(
        "\n--- cargo-slot three-way serialisation proof, observed ---\n"
        f"A interval: {interval_a}\n"
        f"B interval: {interval_b}\n"
        f"C interval: {interval_c}\n"
        f"WAITING line (C): {waiting_line!r}\n"
        f"ACQUIRED line (C): {acquired_line!r}\n"
    )

    assert "WAITING" in waiting_line and "question 229" in waiting_line, (
        f"expected a WAITING line citing question 229, got {waiting_line!r}"
    )
    # Both slot paths must be named -- the private lock directory's own two slot files.
    assert str(private_lock_dir / "cargo-slot-0.lock") in waiting_line, waiting_line
    assert str(private_lock_dir / "cargo-slot-1.lock") in waiting_line, waiting_line

    match = re.search(r"after waiting ([\d.]+)s", acquired_line)
    assert match, f"expected an ACQUIRED line reporting a measured wait, got {acquired_line!r}"
    measured_wait = float(match.group(1))
    assert measured_wait > 0.0, f"the ACQUIRED line's own measured wait must be nonzero (real contention was forced), got {measured_wait}"

    peak = _max_concurrent([interval_a, interval_b, interval_c])
    assert peak == 2, f"expected the maximum number of simultaneously-running children to be exactly 2, measured {peak} from {[interval_a, interval_b, interval_c]}"

    first_finish = min(interval_a[1], interval_b[1])
    # A small tolerance for the real (sub-millisecond-to-low-millisecond) gap between a child's
    # own measured "end" timestamp (written just before it exits) and the moment the kernel
    # actually tears down its file descriptors and releases the flock -- never large enough to
    # hide a genuine ordering violation given HOLD_SECONDS=0.8.
    tolerance_s = 0.2
    assert interval_c[0] >= first_finish - tolerance_s, (
        f"the third child's start ({interval_c[0]}) must be at or after the first-finishing "
        f"child's end ({first_finish}), within {tolerance_s}s measurement tolerance"
    )

    after = sorted(real_home_locks.iterdir()) if real_home_locks.is_dir() else []
    assert after == before, (
        f"this test must never touch the real, host-wide $HOME/.altavista/locks/ directory -- "
        f"before={before!r} after={after!r}"
    )

"""Question 207's Python-side mirror of `crates/av-lockstep/src/docker_test_lock.rs`'s own
cross-process, cross-language test
(`docker_test_lock::tests::flock_lock_is_visible_across_processes_and_languages`): this file
proves the SAME mutual exclusion holds between two independent PYTHON processes on
`altavista.docker_test_lock`'s own production lock path, not merely between Rust and Python.

Needs no Docker at all -- this test never skips for a Docker reason, and always runs (or fails
outright) on any host with a Python interpreter, which this test itself already is.

# Why two real child processes, not two in-process threads

`altavista.docker_test_lock.lock_docker_tests()` is meant to serialise separate OS PROCESSES
(this repository's own `pytest`/`cargo test` invocations, in this worktree or any other) -- the
in-process case (two threads of ONE process) is already covered, with a direct measurement of
whether `flock` even serialises that case at all, by the Rust side's own
`flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions` test. This
file exists to prove the cross-PROCESS claim directly, the same way that Rust test's own sibling
(`flock_lock_is_visible_across_processes_and_languages`) does across languages.

# No fixed sleep

Every synchronisation point below blocks on a real OS event (a child process's own stdout line,
written only after its own `fcntl.flock` call has actually returned, or its own `stdin.readline`
unblocking) -- never a fixed-duration `time.sleep` guess.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Runs inside a CHILD process: acquires the real, production `lock_docker_tests()` context
# manager, announces that it now holds it (only once `fcntl.flock` has actually returned --
# never before), then blocks on a real OS event (its own stdin) until the parent test tells it
# to release.
_HOLDER_SCRIPT = """
import sys
from altavista.docker_test_lock import lock_docker_tests

with lock_docker_tests():
    print("HELD", flush=True)
    sys.stdin.readline()
print("RELEASED", flush=True)
"""

# Runs inside a SEPARATE child process: a single non-blocking attempt on the identical
# production path (via `lock_path()`, never re-deriving it independently -- this probe is
# testing mutual exclusion between two Python processes, not path agreement, so it reuses the
# module's own path function on purpose).
_PROBE_SCRIPT = """
import fcntl
import os
from altavista.docker_test_lock import lock_path

path = lock_path()
path.parent.mkdir(parents=True, exist_ok=True)
fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    print("ACQUIRED")
    fcntl.flock(fd, fcntl.LOCK_UN)
except BlockingIOError:
    print("BLOCKED")
finally:
    os.close(fd)
"""


def _run_probe_child() -> str:
    result = subprocess.run([sys.executable, "-c", _PROBE_SCRIPT], cwd=REPO_ROOT, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, f"the probe child itself must not error: stdout={result.stdout!r} stderr={result.stderr!r}"
    return result.stdout.strip()


def test_two_python_processes_mutually_exclude_on_the_docker_test_lock():
    holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER_SCRIPT],
        cwd=REPO_ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        # Block on a real OS event: the holder child's own stdout line, written only after its
        # `fcntl.flock(LOCK_EX)` call has actually returned -- never a sleep-and-hope.
        held_line = holder.stdout.readline().strip()
        assert held_line == "HELD", f"holder child did not report holding the lock (got {held_line!r}); stderr: {holder.stderr.read()}"

        observed_while_held = _run_probe_child()

        # Release the holder by satisfying its own blocking `stdin.readline()` -- another real
        # OS event, not a sleep -- then wait for its own confirmation line before probing again.
        holder.stdin.write("release\n")
        holder.stdin.flush()
        released_line = holder.stdout.readline().strip()
        assert released_line == "RELEASED", f"holder child did not confirm release (got {released_line!r}); stderr: {holder.stderr.read()}"

        observed_after_release = _run_probe_child()
    finally:
        holder.stdin.close()
        holder.wait(timeout=10)

    # Question 148: an exit code is not evidence -- print exactly what both children actually
    # observed, so a reader of the test output can see the real proof, not just a green dot.
    print(
        f"\n--- two-python-process flock mutual-exclusion proof, observed ---\n"
        f"while the holder child held lock_docker_tests(), the probe child observed: {observed_while_held!r}\n"
        f"after the holder released, the same probe child observed: {observed_after_release!r}"
    )

    assert observed_while_held == "BLOCKED", (
        f"a second Python process using fcntl.flock(LOCK_EX|LOCK_NB) on the identical production path must fail to acquire it while the first "
        f"process's own lock_docker_tests() context manager still holds it -- got {observed_while_held!r}"
    )
    assert observed_after_release == "ACQUIRED", f"once the holder released, the same probe command must now succeed -- got {observed_after_release!r}"

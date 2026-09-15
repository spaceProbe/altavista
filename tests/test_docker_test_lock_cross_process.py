"""Question 207's Python-side mirror of `crates/av-lockstep/src/docker_test_lock.rs`'s own
cross-process, cross-language test
(`docker_test_lock::tests::flock_lock_is_visible_across_processes_and_languages`): this file
proves the SAME mutual exclusion holds between two independent PYTHON processes on
`altavista.docker_test_lock`'s own production lock CODE PATH, not merely between Rust and Python.

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

# Never asserts the REAL shared lock is free (docs/open-questions.md question 212(b))

The two locking tests below used to run their child processes against the real, host-wide
`$HOME/.altavista/locks/docker-tests.lock` -- the exact path every OTHER Docker-gated test on
this host (Rust or Python, this worktree or any other) also locks. Whenever a different track on
this host genuinely held that lock while this file ran, `observed_after_release == "ACQUIRED"`
was simply false, for a reason that has nothing to do with `altavista.docker_test_lock`'s own
correctness -- the lock isn't the module's to give up. The fix: each locking test gives its own
child processes a PRIVATE `$HOME` (a fresh directory under `tmp_path`, via `subprocess.Popen`'s
`env=` argument -- never `os.environ[...] = ...` on this test process itself, question 199), so
`lock_path()` -- the real, unmodified production function -- resolves to a lock file only this
test's own children ever touch. The production `lock_docker_tests()`/`lock_path()` code is still
exercised for real (this is not a hand-rolled `flock`); only the PATH it resolves to differs from
today's default. The cross-language path-agreement claim the old shared-path test also proved by
construction is kept, but split out into its own non-locking test below
(`test_lock_path_matches_the_documented_home_relative_convention`) that asserts against the real,
inherited `$HOME` and takes no lock at all.

# No fixed sleep

Every synchronisation point below blocks on a real OS event (a child process's own stdout line,
written only after its own `fcntl.flock` call has actually returned, or its own `stdin.readline`
unblocking) -- never a fixed-duration `time.sleep` guess.
"""
from __future__ import annotations

import os
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

# Runs inside a THIRD kind of child process: calls the REAL, blocking `lock_docker_tests()` --
# never the raw LOCK_NB probe below -- so that when contended, THIS process's own call takes
# the module's genuine "announce, then block, then announce again" path (see
# altavista/docker_test_lock.py's own "A blocked wait is announced, never silent" doc section).
_WAITER_SCRIPT = """
from altavista.docker_test_lock import lock_docker_tests

with lock_docker_tests():
    print("WAITER_ACQUIRED", flush=True)
"""

# Runs inside a SEPARATE child process: a single non-blocking attempt on `lock_path()`'s own
# resolved path (never re-deriving it independently -- this probe is testing mutual exclusion
# between two Python processes, not path agreement, so it reuses the module's own path function
# on purpose). Which actual path that is depends entirely on the child's own $HOME, supplied via
# `env=` by whoever spawns this script -- see `_child_env` below.
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


def _child_env(private_home: Path) -> dict[str, str]:
    """A copy of THIS process's own environment (never assigned back into `os.environ` --
    question 199) with `$HOME` overridden to a private, per-test scratch directory. Every child
    spawned with this env computes `lock_path()` (the real, unmodified production function) as
    `private_home / ".altavista" / "locks" / "docker-tests.lock"` -- a path only this test's own
    children ever touch, never the real host-wide lock every other Docker-gated test on this
    host also locks (question 212(b))."""
    env = dict(os.environ)
    env["HOME"] = str(private_home)
    return env


def _run_probe_child(env: dict[str, str]) -> str:
    result = subprocess.run([sys.executable, "-c", _PROBE_SCRIPT], cwd=REPO_ROOT, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, f"the probe child itself must not error: stdout={result.stdout!r} stderr={result.stderr!r}"
    return result.stdout.strip()


def test_two_python_processes_mutually_exclude_on_the_docker_test_lock(tmp_path):
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER_SCRIPT],
        cwd=REPO_ROOT,
        env=env,
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

        observed_while_held = _run_probe_child(env)

        # Release the holder by satisfying its own blocking `stdin.readline()` -- another real
        # OS event, not a sleep -- then wait for its own confirmation line before probing again.
        holder.stdin.write("release\n")
        holder.stdin.flush()
        released_line = holder.stdout.readline().strip()
        assert released_line == "RELEASED", f"holder child did not confirm release (got {released_line!r}); stderr: {holder.stderr.read()}"

        observed_after_release = _run_probe_child(env)
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


def _read_line_containing(pipe, needle: str, *, what: str) -> str:
    """Blocks on real OS events (the pipe's own `readline`) until a line containing `needle`
    appears -- never a sleep. Any other lines (there should be none for plain `python -c`, but
    filtering by content rather than position costs nothing and matches the Rust side's own
    nested-subprocess test, which DOES see extra lines from cargo's own build-status text)."""
    while True:
        line = pipe.readline()
        assert line, f"{what}: the pipe closed before ever printing a line containing {needle!r}"
        if needle in line:
            return line.strip()


def test_the_waiting_process_announces_its_own_wait(tmp_path):
    """Question 207's own follow-up: `lock_docker_tests()` must not block silently when
    contended -- see altavista/docker_test_lock.py's "A blocked wait is announced, never silent"
    doc section. Mirrors the Rust side's own
    `docker_test_lock::tests::lock_docker_tests_announces_a_blocked_wait_never_silently`, between
    two Python processes instead of a nested `cargo test` subprocess. Runs its holder/waiter pair
    on a private `$HOME` (see `_child_env`), never the real shared lock (question 212(b)) --
    this test only needs to prove ITS OWN waiter announces and then acquires, not that the real
    host-wide lock was free."""
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER_SCRIPT],
        cwd=REPO_ROOT,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        held_line = holder.stdout.readline().strip()
        assert held_line == "HELD", f"holder child did not report holding the lock (got {held_line!r}); stderr: {holder.stderr.read()}"

        waiter = subprocess.Popen(
            [sys.executable, "-c", _WAITER_SCRIPT],
            cwd=REPO_ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            # Block on a real OS event: the waiter's own WAITING line, written (and flushed) by
            # `lock_docker_tests()` itself only after its own non-blocking attempt has actually
            # failed with BlockingIOError -- never a sleep-and-hope that the waiter has "probably"
            # reached that point by now.
            waiting_line = _read_line_containing(waiter.stderr, "WAITING", what="waiter")

            # Release the holder -- the waiter's own blocked fcntl.flock(LOCK_EX) can now proceed.
            holder.stdin.write("release\n")
            holder.stdin.flush()
            released_line = holder.stdout.readline().strip()
            assert released_line == "RELEASED", f"holder child did not confirm release (got {released_line!r}); stderr: {holder.stderr.read()}"

            acquired_line = _read_line_containing(waiter.stderr, "ACQUIRED the docker-test lock", what="waiter")

            waiter_stdout, waiter_stderr_rest = waiter.communicate(timeout=10)
        finally:
            if waiter.poll() is None:
                waiter.kill()
    finally:
        holder.stdin.close()
        holder.wait(timeout=10)

    # Question 148: an exit code is not evidence -- print exactly what the waiter process wrote.
    print(
        f"\n--- Python waiting-announcement proof, observed ---\n"
        f"WAITING line: {waiting_line!r}\n"
        f"ACQUIRED line: {acquired_line!r}\n"
        f"waiter stdout: {waiter_stdout!r}"
    )

    assert "WAITING" in waiting_line and "docker-test lock" in waiting_line and "question 207" in waiting_line, (
        f"expected a WAITING line naming the docker-test lock and citing question 207, got {waiting_line!r}"
    )
    assert acquired_line.startswith("ACQUIRED the docker-test lock") and "after waiting" in acquired_line, (
        f"expected an ACQUIRED-after-waiting line reporting the real elapsed wait, got {acquired_line!r}"
    )
    assert "WAITER_ACQUIRED" in waiter_stdout, f"the waiter must have actually acquired the lock and printed its own confirmation -- got stdout {waiter_stdout!r} (stderr tail: {waiter_stderr_rest!r})"


def test_lock_path_matches_the_documented_home_relative_convention():
    """The cross-language path-agreement claim the old shared-path test proved by construction
    (both a bare `python3 -c` probe and this module's own `lock_path()` computing the identical
    `$HOME`-relative path), kept as its own assertion -- but taking NO lock at all, against this
    process's own real, inherited `$HOME`, so it can never be affected by whether some other
    process on this host holds the real lock (question 212(b)). The Rust side's own equivalent,
    non-locking path-only test is
    `docker_test_lock::tests::rust_and_python_compute_the_identical_lock_path`."""
    from altavista.docker_test_lock import lock_path

    assert lock_path() == Path(os.environ["HOME"]) / ".altavista" / "locks" / "docker-tests.lock"

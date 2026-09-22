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
import select
import subprocess
import sys
from pathlib import Path

from altavista.test_env import drain_after_terminate

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
        if held_line != "HELD":
            # `holder.stderr.read()` used to sit in this assert's MESSAGE -- evaluated only
            # on failure, at which point the holder child is still running with its stdin
            # open, so the readall blocked forever and the failure was never reported at
            # all. P5 round 3, measured; see `altavista.test_env.drain_after_terminate`.
            raise AssertionError(
                f"holder child did not report holding the lock (got {held_line!r}); "
                f"stderr: {drain_after_terminate(holder)}")

        observed_while_held = _run_probe_child(env)

        # Release the holder by satisfying its own blocking `stdin.readline()` -- another real
        # OS event, not a sleep -- then wait for its own confirmation line before probing again.
        holder.stdin.write("release\n")
        holder.stdin.flush()
        released_line = holder.stdout.readline().strip()
        if released_line != "RELEASED":
            raise AssertionError(
                f"holder child did not confirm release (got {released_line!r}); "
                f"stderr: {drain_after_terminate(holder)}")

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
        if held_line != "HELD":
            # `holder.stderr.read()` used to sit in this assert's MESSAGE -- evaluated only
            # on failure, at which point the holder child is still running with its stdin
            # open, so the readall blocked forever and the failure was never reported at
            # all. P5 round 3, measured; see `altavista.test_env.drain_after_terminate`.
            raise AssertionError(
                f"holder child did not report holding the lock (got {held_line!r}); "
                f"stderr: {drain_after_terminate(holder)}")

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
            if released_line != "RELEASED":
                raise AssertionError(
                    f"holder child did not confirm release (got {released_line!r}); "
                    f"stderr: {drain_after_terminate(holder)}")

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


# --------------------------------------------------------------- re-entrancy (heavy round 5)
# `flock(2)` attaches its lock to the OPEN FILE DESCRIPTION, so a nested acquisition that
# `os.open`s the path a second time blocks on a lock its OWN process already holds -- forever.
# Measured on this host before the fix: a `tests/test_tiles_container.py` run sat 110 minutes
# with 1.3s of CPU and no children, blocked in `flock` with two descriptors on the lock file,
# while another team's docker-gated test and a workspace `cargo test` queued behind it.
# `tests/heavy_stack.py` wraps two different fixtures in this context manager, and a test
# needing both holds both at once -- ordinary, legitimate nesting.

# Runs inside a CHILD process: two NESTED `lock_docker_tests()` blocks. Before the
# re-entrancy fix this child never reaches "NESTED_OK" -- it blocks in `flock` forever and the
# `timeout=` below fires, which is exactly how this test would have caught the defect.
_NESTED_SCRIPT = """
from altavista.docker_test_lock import lock_docker_tests

with lock_docker_tests():
    with lock_docker_tests():
        print("NESTED_OK", flush=True)
"""

# Runs inside a CHILD process: takes the lock, enters AND LEAVES an inner block, then
# announces and waits. While it waits it is still inside the OUTER block, so the lock must
# still be held against other processes -- a re-entrancy counter that released on the INNER
# exit would hand the lock away early, which is a worse bug than the deadlock it replaced.
_INNER_RELEASE_SCRIPT = """
import sys
from altavista.docker_test_lock import lock_docker_tests

with lock_docker_tests():
    with lock_docker_tests():
        pass
    print("INNER_EXITED", flush=True)
    sys.stdin.readline()
print("OUTER_RELEASED", flush=True)
"""


# Runs inside a CHILD process (private `$HOME`, like every other child here): two THREADS
# each take `lock_docker_tests()` around a short critical section and append enter/exit
# markers to one list. Same-process cross-thread exclusion is a REAL property of `flock` --
# a second, independently-opened descriptor blocks even within one process, which the Rust
# half measures in
# `docker_test_lock::tests::flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions`
# -- and the re-entrancy counter must not quietly take it away. It does not, because the
# counter is thread-local: a process-global one would let thread B see thread A's non-zero
# depth, skip locking entirely, and run its critical section concurrently.
# Thread B is started only AFTER thread A is observed inside its critical section, so B's
# check of the counter is guaranteed to happen while A's depth is non-zero. Ordering this
# explicitly is what gives the test teeth: an earlier draft started both threads at once and
# PASSED against a deliberately process-global counter, because B usually reached the check
# before A had finished acquiring. A race that usually goes the right way proves nothing.
_TWO_THREADS_SCRIPT = """
import threading, time
from altavista.docker_test_lock import lock_docker_tests

events = []
events_lock = threading.Lock()
a_inside = threading.Event()
b_done = threading.Event()

def record(s):
    with events_lock:
        events.append(s)

def worker_a():
    with lock_docker_tests():
        record("enter-a")
        a_inside.set()
        # Hold well past the point where B, if it were not properly excluded, would have
        # entered and left its own section.
        b_done.wait(timeout=2.0)
        record("exit-a")

def worker_b():
    with lock_docker_tests():
        record("enter-b")
        record("exit-b")
    b_done.set()

ta = threading.Thread(target=worker_a)
ta.start()
assert a_inside.wait(timeout=30), "thread A never entered its critical section"
tb = threading.Thread(target=worker_b)
tb.start()
for t in (ta, tb):
    t.join(timeout=30)
assert not ta.is_alive() and not tb.is_alive(), "a thread never finished -- deadlock"

# Serialised iff A's section closes before B's opens.
ok = events == ["enter-a", "exit-a", "enter-b", "exit-b"]
print("SERIALISED" if ok else "INTERLEAVED:" + ",".join(events), flush=True)
"""


def test_two_threads_of_one_process_still_serialise_despite_the_reentrancy_counter(tmp_path):
    private_home = tmp_path / "home"
    private_home.mkdir()

    result = subprocess.run(
        [sys.executable, "-c", _TWO_THREADS_SCRIPT],
        cwd=REPO_ROOT,
        env=_child_env(private_home),
        capture_output=True,
        text=True,
        timeout=60,
    )

    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "SERIALISED", (
        "the re-entrancy counter must be THREAD-local: a process-global one lets a second "
        "thread see the first thread's depth, skip its own flock, and run concurrently "
        f"(stdout={result.stdout!r})"
    )


def test_nested_acquisition_in_one_process_does_not_deadlock(tmp_path):
    private_home = tmp_path / "home"
    private_home.mkdir()

    result = subprocess.run(
        [sys.executable, "-c", _NESTED_SCRIPT],
        cwd=REPO_ROOT,
        env=_child_env(private_home),
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "NESTED_OK", (
        "a nested lock_docker_tests() must be counted, not re-acquired -- re-acquiring "
        "opens a second file description and blocks on this process's own flock forever "
        f"(stdout={result.stdout!r} stderr={result.stderr!r})"
    )


def test_leaving_an_inner_block_does_not_release_the_lock_for_other_processes(tmp_path):
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    holder = subprocess.Popen(
        [sys.executable, "-c", _INNER_RELEASE_SCRIPT],
        cwd=REPO_ROOT,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        assert holder.stdout is not None and holder.stdin is not None
        # Bounded, never a bare readline(): against a NON-re-entrant lock this child
        # deadlocks inside its inner block and never writes a line, and a bare readline()
        # would hang this test instead of failing it. A test that hangs on the defect it
        # exists to catch reports nothing.
        ready, _, _ = select.select([holder.stdout], [], [], 30)
        assert ready, (
            "the holder child never reached its inner-block exit within 30s -- it is "
            "deadlocked on its own flock, which is exactly the non-re-entrant defect"
        )
        first = holder.stdout.readline().strip()
        assert first == "INNER_EXITED", f"child did not reach the inner exit: {first!r}"

        # Still inside the OUTER block: another process must NOT be able to take the lock.
        assert _run_probe_child(env) == "BLOCKED", (
            "leaving an INNER lock_docker_tests() block released the host-wide lock while "
            "the outer block was still open -- only the outermost exit may release it"
        )

        holder.stdin.write("\n")
        holder.stdin.flush()
        assert holder.stdout.readline().strip() == "OUTER_RELEASED"
        holder.wait(timeout=30)

        # And once the OUTER block has exited, it really is free again.
        assert _run_probe_child(env) == "ACQUIRED"
    finally:
        if holder.poll() is None:
            holder.terminate()
            drain_after_terminate(holder)

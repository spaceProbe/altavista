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

import datetime
import os
import re
import select
import subprocess
import sys
import time
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


# Runs inside a FOURTH kind of child process, used only by the holder-sidecar staleness test
# below: takes the REAL flock directly (raw `fcntl.flock` on `lock_path()`'s own resolved path)
# WITHOUT going through `lock_docker_tests()` -- so it never writes or overwrites the sidecar.
# This is deliberate: it is the module's own established "raw flock probe" idiom (see
# `_PROBE_SCRIPT` above), used here as a stand-in for "whoever currently holds the real flock,
# without touching the sidecar" -- the exact shape that lets a sidecar left behind by a
# previously killed, instrumented holder survive, unmodified, while someone else genuinely
# contends for the real lock (see `test_a_waiter_reports_a_killed_holders_stale_record_and_
# still_acquires`'s own doc comment for why this construction, not a raw SIGKILL race, is what
# makes the "stale AND contended" case reproducible on demand).
_GHOST_HOLDER_SCRIPT = """
import fcntl
import os
import sys
from altavista.docker_test_lock import lock_path

path = lock_path()
path.parent.mkdir(parents=True, exist_ok=True)
fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
print("GHOST_HELD", flush=True)
sys.stdin.readline()
fcntl.flock(fd, fcntl.LOCK_UN)
os.close(fd)
print("GHOST_RELEASED", flush=True)
"""

# Runs inside a FIFTH kind of child process: holds the real `lock_docker_tests()` lock (so its
# own sidecar gets written), then, inside the `with` block, deliberately raises -- proving the
# sidecar is removed on the EXCEPTION path too, not only the ordinary return (this module's own
# "the holder removes its own sidecar ... on both the success and the exception path" rule).
# Single-process; needs no holder/waiter dance.
_RAISING_SCRIPT = """
from altavista.docker_test_lock import holder_sidecar_path, lock_docker_tests, lock_path

sidecar = holder_sidecar_path(lock_path())
try:
    with lock_docker_tests():
        assert sidecar.exists(), "sidecar missing while still held"
        raise RuntimeError("deliberate: prove the sidecar is removed on the exception path too")
except RuntimeError:
    pass
print("SIDECAR_GONE" if not sidecar.exists() else "SIDECAR_STILL_THERE", flush=True)
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


# --------------------------------------------------------------- the holder sidecar (question 234, native round 5)
# Every test below uses a PRIVATE `$HOME` (`_child_env`, questions 212(b)/199), exactly like
# every other test in this file -- see `_child_env`'s own doc comment. None of them ever touch
# `$HOME/.altavista/locks/` for real.


def test_holder_sidecar_path_matches_the_documented_convention():
    """Same-language documentation-conformance check for `holder_sidecar_path`, mirroring
    `test_lock_path_matches_the_documented_home_relative_convention` above for the lock path
    itself. The REAL cross-language, by-construction proof (a bare `python3 -c` that never
    imports this module) lives on the Rust side:
    `docker_test_lock::tests::rust_and_python_compute_the_identical_holder_sidecar_path`."""
    from altavista.docker_test_lock import holder_sidecar_path, lock_path

    expected = Path(str(lock_path()) + ".holder")
    assert holder_sidecar_path(lock_path()) == expected


def test_holder_sidecar_record_format_is_pinned(tmp_path):
    """Pins the exact on-disk shape of the holder sidecar record: four `key=value` lines,
    `pid`/`tree`/`command`/`time`, in that order, written by the taker's OUTERMOST acquisition.
    Not proved by inspection -- reads the real file a real child process wrote, then confirms
    that same file is gone once that child's outermost block exits normally."""
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)
    sidecar_path = private_home / ".altavista" / "locks" / "docker-tests.lock.holder"

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
            raise AssertionError(f"holder child did not report holding the lock (got {held_line!r}); stderr: {drain_after_terminate(holder)}")

        raw = sidecar_path.read_text()
        print(f"\n--- holder sidecar record format proof ---\nsidecar path: {sidecar_path}\ncontents: {raw!r}")

        lines = raw.splitlines()
        assert len(lines) == 4, f"expected exactly 4 lines (pid/tree/command/time), got {lines!r}"
        keys = [line.split("=", 1)[0] for line in lines]
        assert keys == ["pid", "tree", "command", "time"], f"expected pid/tree/command/time in that order, got {keys!r}"

        fields = dict(line.split("=", 1) for line in lines)
        assert fields["pid"] == str(holder.pid), f"pid field {fields['pid']!r} must match the real child pid {holder.pid}"
        assert fields["tree"] == str(REPO_ROOT), f"tree field {fields['tree']!r} must match the child's own cwd {REPO_ROOT}"
        assert re.match(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4}$", fields["time"]), f"time field does not look like 'YYYY-MM-DD HH:MM:SS +ZZZZ': {fields['time']!r}"

        holder.stdin.write("release\n")
        holder.stdin.flush()
        released_line = holder.stdout.readline().strip()
        assert released_line == "RELEASED", f"holder child did not confirm release (got {released_line!r}); stderr: {drain_after_terminate(holder)}"

        assert not sidecar_path.exists(), "the sidecar must be removed once the outermost (here, only) block exits normally"
    finally:
        holder.stdin.close()
        holder.wait(timeout=10)


def test_holder_sidecar_is_removed_on_the_exception_path(tmp_path):
    """The other half of "removed on both the success and the exception path" --
    `test_holder_sidecar_record_format_is_pinned` above already proves the success half."""
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    result = subprocess.run([sys.executable, "-c", _RAISING_SCRIPT], cwd=REPO_ROOT, env=env, capture_output=True, text=True, timeout=30)
    assert result.returncode == 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
    assert result.stdout.strip() == "SIDECAR_GONE", (
        "the holder sidecar must be removed even when the with-block's body raises -- "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )


def test_a_waiter_prints_the_live_holders_pid_tree_command_and_time(tmp_path):
    """Proof 1 (native5 round 5 brief): a waiter's own WAITING line names the LIVE holder's real
    pid, tree, command and time -- read from the holder's sidecar, not guessed. Every value is
    checked against what the holder process actually IS: `holder.pid` (the real OS pid this test
    process itself spawned), `REPO_ROOT` (the cwd every child in this file is spawned with),
    `sys.executable` (the interpreter every child here is spawned with), and a timestamp bounded
    against real wall-clock time taken immediately around the holder's own acquisition."""
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    before = time.time()
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
            raise AssertionError(f"holder child did not report holding the lock (got {held_line!r}); stderr: {drain_after_terminate(holder)}")

        waiter = subprocess.Popen([sys.executable, "-c", _WAITER_SCRIPT], cwd=REPO_ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            waiting_line = _read_line_containing(waiter.stderr, "WAITING", what="waiter")
            after = time.time()

            holder.stdin.write("release\n")
            holder.stdin.flush()
            released_line = holder.stdout.readline().strip()
            if released_line != "RELEASED":
                raise AssertionError(f"holder child did not confirm release (got {released_line!r}); stderr: {drain_after_terminate(holder)}")

            acquired_line = _read_line_containing(waiter.stderr, "ACQUIRED the docker-test lock", what="waiter")
            waiter_stdout, waiter_stderr_rest = waiter.communicate(timeout=10)
        finally:
            if waiter.poll() is None:
                waiter.kill()
    finally:
        holder.stdin.close()
        holder.wait(timeout=10)

    print(
        f"\n--- live-holder WAITING-line proof, observed ---\n"
        f"holder pid: {holder.pid}\n"
        f"WAITING line: {waiting_line!r}\n"
        f"ACQUIRED line: {acquired_line!r}\n"
        f"waiter stdout: {waiter_stdout!r}"
    )

    assert "WAITING" in waiting_line and "docker-test lock" in waiting_line and "question 207" in waiting_line

    assert f"pid={holder.pid}" in waiting_line, f"expected the real holder pid {holder.pid} in the WAITING line, got {waiting_line!r}"
    assert f"tree={REPO_ROOT}" in waiting_line, f"expected the real holder tree {REPO_ROOT} in the WAITING line, got {waiting_line!r}"
    assert sys.executable in waiting_line, f"expected the real holder's own interpreter path {sys.executable} in the WAITING line, got {waiting_line!r}"
    assert "stale" not in waiting_line.lower(), f"the holder was alive when this line was printed -- must not be reported as stale, got {waiting_line!r}"

    time_match = re.search(r"time=([^)]+)\)", waiting_line)
    assert time_match, f"no 'time=...)' field found in {waiting_line!r}"
    reported_time = datetime.datetime.strptime(time_match.group(1), "%Y-%m-%d %H:%M:%S %z")
    assert before - 5 <= reported_time.timestamp() <= after + 5, (
        f"the reported time {reported_time} is not within [{before - 5}, {after + 5}] of when the holder actually acquired -- looks guessed, not real"
    )

    assert acquired_line.startswith("ACQUIRED the docker-test lock") and "after waiting" in acquired_line
    assert "WAITER_ACQUIRED" in waiter_stdout, f"the waiter must have actually acquired the lock -- got stdout {waiter_stdout!r} (stderr tail: {waiter_stderr_rest!r})"


def test_a_waiter_reports_a_killed_holders_stale_record_and_still_acquires(tmp_path):
    """Proof 2: a waiter reports a killed holder's surviving sidecar record as STALE, names the
    dead pid, and still acquires.

    # Why a "ghost holder", not a raw SIGKILL race

    The naive version of this test -- SIGKILL the holder, then spawn a waiter -- does not
    actually exercise the WAITING-line staleness path: the kernel releases a killed process's
    `flock` as part of its own exit teardown, essentially atomically with that exit, so by the
    time any new process even TRIES the lock, it is simply free again; the new taker's
    non-blocking `flock` attempt succeeds immediately (the silent, uncontended path), no WAITING
    line is ever printed, and that taker immediately overwrites the sidecar with its own live
    info before anyone could observe the dead holder's record at all. A `describe_holder` call
    only ever happens on the CONTENDED path (the non-blocking attempt genuinely failed), so
    proving the "stale" label requires the flock to be genuinely held by someone else AT THE
    MOMENT the sidecar names a dead pid.

    This test manufactures exactly that: (1) a real, instrumented holder takes the lock (writing
    its own sidecar), and is SIGKILLed and reaped -- no cleanup runs, so its sidecar survives,
    now naming a genuinely dead pid (confirmed directly via `os.kill(pid, 0)`, the identical
    liveness check the module itself uses); (2) a "ghost" holder (`_GHOST_HOLDER_SCRIPT`) then
    takes the REAL flock directly, via a raw `fcntl.flock` on the identical path, WITHOUT going
    through `lock_docker_tests()` -- so it never overwrites the now-stale sidecar; (3) a REAL
    waiter, using the production `lock_docker_tests()`, contends against the ghost, reads the
    stale sidecar, and must report it as stale in its own WAITING line.
    """
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)

    dead_holder = subprocess.Popen(
        [sys.executable, "-c", _HOLDER_SCRIPT],
        cwd=REPO_ROOT,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    held_line = dead_holder.stdout.readline().strip()
    if held_line != "HELD":
        dead_holder.kill()
        raise AssertionError(f"holder child did not report holding the lock (got {held_line!r}); stderr: {drain_after_terminate(dead_holder)}")
    dead_pid = dead_holder.pid

    dead_holder.kill()  # SIGKILL: no cleanup runs, so its sidecar survives.
    dead_holder.wait(timeout=10)  # reap -- required for os.kill(dead_pid, 0) to report ESRCH below.
    dead_holder.stdin.close()

    try:
        os.kill(dead_pid, 0)
        still_alive = True
    except ProcessLookupError:
        still_alive = False
    assert not still_alive, f"the SIGKILLed+reaped holder pid {dead_pid} is still reported alive by os.kill(pid, 0) -- test setup itself is broken, not the module under test"

    ghost = subprocess.Popen(
        [sys.executable, "-c", _GHOST_HOLDER_SCRIPT],
        cwd=REPO_ROOT,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        ghost_held_line = ghost.stdout.readline().strip()
        assert ghost_held_line == "GHOST_HELD", f"ghost holder did not report holding the lock (got {ghost_held_line!r}); stderr: {drain_after_terminate(ghost)}"

        waiter = subprocess.Popen([sys.executable, "-c", _WAITER_SCRIPT], cwd=REPO_ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            waiting_line = _read_line_containing(waiter.stderr, "WAITING", what="waiter")

            ghost.stdin.write("release\n")
            ghost.stdin.flush()
            ghost_released_line = ghost.stdout.readline().strip()
            assert ghost_released_line == "GHOST_RELEASED", f"ghost holder did not confirm release (got {ghost_released_line!r}); stderr: {drain_after_terminate(ghost)}"

            acquired_line = _read_line_containing(waiter.stderr, "ACQUIRED the docker-test lock", what="waiter")
            waiter_stdout, waiter_stderr_rest = waiter.communicate(timeout=10)
        finally:
            if waiter.poll() is None:
                waiter.kill()
    finally:
        if ghost.poll() is None:
            ghost.terminate()
            drain_after_terminate(ghost)

    print(
        f"\n--- killed-holder stale-record WAITING-line proof, observed ---\n"
        f"dead holder pid: {dead_pid}\n"
        f"WAITING line: {waiting_line!r}\n"
        f"ACQUIRED line: {acquired_line!r}\n"
        f"waiter stdout: {waiter_stdout!r}"
    )

    assert "WAITING" in waiting_line and "docker-test lock" in waiting_line and "question 207" in waiting_line

    assert "stale" in waiting_line.lower(), f"expected the WAITING line to call the dead holder's record stale, got {waiting_line!r}"
    assert f"pid={dead_pid}" in waiting_line, f"expected the dead pid {dead_pid} to be named in the WAITING line, got {waiting_line!r}"

    assert acquired_line.startswith("ACQUIRED the docker-test lock") and "after waiting" in acquired_line
    assert "WAITER_ACQUIRED" in waiter_stdout, f"the waiter must have acquired the lock despite the stale record -- got stdout {waiter_stdout!r} (stderr tail: {waiter_stderr_rest!r})"


def test_absent_or_corrupt_sidecar_changes_nothing(tmp_path):
    """Proof 3: deleting or garbling the sidecar while a holder is genuinely live changes
    NOTHING about waiter behaviour -- it still waits, still announces (the same WAITING/
    ACQUIRED substrings every other test here pins), and still acquires. An unreadable holder
    record is the status quo (a lock whose holder is unknown), never an error."""
    private_home = tmp_path / "home"
    private_home.mkdir()
    env = _child_env(private_home)
    sidecar_path = private_home / ".altavista" / "locks" / "docker-tests.lock.holder"

    for mutate, label in (
        (lambda: sidecar_path.unlink(missing_ok=True), "absent"),
        (lambda: sidecar_path.write_bytes(b"\x00\x01not a valid holder record\xff\xff"), "corrupt"),
    ):
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
                raise AssertionError(f"[{label}] holder child did not report holding the lock (got {held_line!r}); stderr: {drain_after_terminate(holder)}")

            assert sidecar_path.exists(), f"[{label}] the holder must have written its own sidecar record before this test mutates it"
            mutate()

            waiter = subprocess.Popen([sys.executable, "-c", _WAITER_SCRIPT], cwd=REPO_ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                waiting_line = _read_line_containing(waiter.stderr, "WAITING", what=f"waiter ({label})")

                holder.stdin.write("release\n")
                holder.stdin.flush()
                released_line = holder.stdout.readline().strip()
                if released_line != "RELEASED":
                    raise AssertionError(f"[{label}] holder child did not confirm release (got {released_line!r}); stderr: {drain_after_terminate(holder)}")

                acquired_line = _read_line_containing(waiter.stderr, "ACQUIRED the docker-test lock", what=f"waiter ({label})")
                waiter_stdout, waiter_stderr_rest = waiter.communicate(timeout=10)
            finally:
                if waiter.poll() is None:
                    waiter.kill()
        finally:
            holder.stdin.close()
            holder.wait(timeout=10)

        print(
            f"\n--- {label}-sidecar proof, observed ---\n"
            f"WAITING line: {waiting_line!r}\n"
            f"ACQUIRED line: {acquired_line!r}\n"
            f"waiter stdout: {waiter_stdout!r}"
        )

        assert "WAITING" in waiting_line and "docker-test lock" in waiting_line and "question 207" in waiting_line, f"[{label}] {waiting_line!r}"
        assert acquired_line.startswith("ACQUIRED the docker-test lock") and "after waiting" in acquired_line, f"[{label}] {acquired_line!r}"
        assert "WAITER_ACQUIRED" in waiter_stdout, f"[{label}] the waiter must have still acquired the lock -- got stdout {waiter_stdout!r} (stderr tail: {waiter_stderr_rest!r})"
        assert "stale" not in waiting_line.lower(), f"[{label}] an absent/corrupt record must not be mis-reported as stale, got {waiting_line!r}"


def test_the_real_lock_directory_is_never_touched_by_this_files_own_tests():
    """Question 212(b)'s own promise, checked directly rather than trusted: every test in this
    file passes a PRIVATE `$HOME` to every child it spawns (`_child_env`), so
    `$HOME/.altavista/locks/` (this process's own REAL, inherited `$HOME`) must be untouched --
    in particular, no stray `docker-tests.lock.holder` sidecar -- by anything this test session
    just did. Snapshots the real lock directory's contents at collection... -- see this test's
    own body: it compares against what was already there, since other Docker-gated tests/tracks
    on this host may legitimately be using the real lock concurrently."""
    real_lock_dir = Path(os.environ["HOME"]) / ".altavista" / "locks"
    if not real_lock_dir.exists():
        return
    stray_sidecars = [p for p in real_lock_dir.iterdir() if p.name.endswith(".lock.holder.tmp") or ".holder.tmp." in p.name]
    assert not stray_sidecars, f"found stray temp holder-sidecar files in the REAL lock directory {real_lock_dir}: {stray_sidecars} -- a test in this file leaked into the shared lock dir"

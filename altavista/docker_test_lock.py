"""A host-wide, cross-process, cross-language lock serialising every Docker-gated test on this
machine (`docs/open-questions.md` question 207) -- the Python half of the fix; the Rust half is
`crates/av-lockstep/src/docker_test_lock.rs`'s `DockerTestLock`/`lock_docker_tests`
(re-exported as `av_lockstep::docker::{DockerTestLock, lock_docker_tests}`).

# The defect this fixes

`av_lockstep::docker::prune_stale_test_resources` (`crates/av-lockstep/src/docker.rs`) deletes
EVERY Docker container and image carrying the label `av.test` -- daemon-wide, regardless of
which worktree, process, or LANGUAGE created it (that function's own doc comment has the full
history). The only lock guarding it used to be a process-local `std::sync::Mutex<()>` in one
Rust test file -- invisible to a Python `pytest` process entirely, let alone one in a different
git worktree. A Python test whose containers carry `av.test` (e.g.
`tests/test_edge_plugin_container.py`'s own `ResourceGuard.label_args()`) could be torn down
mid-run by a completely unrelated Rust `cargo test` process's prune sweep, in this worktree or
any other -- the identical failure mode question 207 found on the Rust side, just reachable from
the Python side too.

# The lock path, and why it is exactly this path

`$HOME/.altavista/locks/docker-tests.lock` -- a plain file, `flock(2)`-locked exclusively via
the stdlib `fcntl` module.

- **Not `/tmp`, not `/private/var`.** This host runs Docker through Colima, whose default
  configuration mounts only `$HOME` into its VM (`~/.colima/default/colima.yaml`'s own comment
  -- also why `tests/test_edge_plugin_container.py`'s own module doc puts every one of its
  bind-mount sources under `$HOME`, never `tempfile`/`tmp_path`). A lock file outside `$HOME` is
  invisible to anything running *inside* that VM, and macOS itself periodically reclaims `/tmp`
  and `/private/var/tmp` -- neither property is acceptable for a lock every Docker-gated test on
  this host must keep agreeing on.
- **Daemon-wide, on purpose.** `prune_stale_test_resources` is itself daemon-wide -- a lock
  scoped any narrower would leave exactly the gap question 207 found.
- **Must name the IDENTICAL path as the Rust implementation, by construction, not by import.**
  `crates/av-lockstep/src/docker_test_lock.rs`'s `LOCK_RELATIVE_PATH` constant is
  `".altavista/locks/docker-tests.lock"`, joined onto `$HOME` -- exactly what `lock_path()`
  below computes. The two languages do not share a build, so this is a documented invariant
  rather than a shared constant: **if you change the path in one implementation, change it in
  the other, in the same commit.** Both are proved to agree by a real cross-process,
  cross-language test
  (`crates/av-lockstep/src/docker_test_lock.rs::tests::flock_lock_is_visible_across_processes_and_languages`):
  a Rust process holds this exact lock, a bare `python3` child (constructing the path inline
  from `$HOME`, never importing this module -- proving agreement BY CONSTRUCTION) tries
  `fcntl.flock(..., LOCK_NB)` and is observed to fail, then to succeed once the Rust side drops
  it. This module's own test mirror
  (`tests/test_docker_test_lock_cross_process.py`) proves the same mutual exclusion between two
  Python processes.

# Why `flock`, not a PID file

A lock file with a PID in it (or any `Drop`/`finally`-based guard with no kernel-owned resource
behind it) cannot survive a `SIGKILL`: a killed process never runs its own cleanup code, Python
or Rust. `flock(2)` has no such gap -- the lock is attached to the *open file description*,
which the kernel itself releases (closing every file descriptor) the instant a killed process's
resources are torn down, with zero userspace code involved. That is the one property this lock
actually needs, and it is exactly what `fcntl.flock` (a thin wrapper over the same `flock(2)`
syscall the Rust side's `libc::flock` calls) gives for free.

# A blocked wait is announced, never silent

A follow-up to question 207: `lock_docker_tests()` tries the lock non-blockingly first. Only
when another process on this host genuinely holds it does blocking on `fcntl.flock(fd, LOCK_EX)`
become a real, possibly long wait -- with no output at all, that wait is indistinguishable, to a
human watching a plain `pytest` run, from a hang. So instead: one line to stderr naming the lock
path and citing question 207 before blocking, and one more reporting the actual elapsed wait
(measured with `time.monotonic()`, never assumed) after acquiring. Proved cross-language by the
Rust side's own `docker_test_lock::tests::lock_docker_tests_announces_a_blocked_wait_never_silently`
test and mirrored between two Python processes by
`tests/test_docker_test_lock_cross_process.py::test_the_waiting_process_announces_its_own_wait`.

# The holder sidecar (question 234, native round 5)

Round 4: an orphaned `python3 -c` probe, spawned detached from a DIFFERENT worktree
(`AltaVista-aiplane`), sat in `sys.stdin.readline()` on a pipe with no writer, PPID 1, holding
the real lock for over two hours -- blocking this track's kernel gate. Nothing about the lock
itself said whose it was; the manager had to reconstruct the answer with `lsof`. Question 234's
ruling: the taker writes pid/tree/command/time into a sidecar, so the next waiter reads it
instead.

- **Path**: `holder_sidecar_path()` below -- `lock_path()`'s own path with `.holder` appended by
  plain STRING concatenation (never `Path.with_suffix`, which would replace `.lock` rather than
  append after it, and never a `Path` join, which would insert a separator). **Not the lock file
  itself**: the lock file's only job is to be `flock`ed; its content is irrelevant to the kernel,
  and truncating/rewriting a file another process holds an `flock` on would be exactly the kind
  of subtlety that surprises the next reader for no gain. Like `_LOCK_RELATIVE_PATH` above, the
  two languages must name the IDENTICAL sidecar path by construction, not by import: the Rust
  side is `crates/av-lockstep/src/docker_test_lock.rs`'s `holder_sidecar_path`/
  `HOLDER_SIDECAR_SUFFIX`. **If you change `_HOLDER_SIDECAR_SUFFIX` here, change the Rust side's
  `HOLDER_SIDECAR_SUFFIX` in the same commit.**
- **Written by the taker, in the outermost acquisition only** (`_write_holder_record`, called
  once real `flock` succeeds -- never on the re-entrant, already-held branch): `pid` (this
  process's own), `tree` (the worktree it runs in -- `_current_tree`'s own doc comment says
  which of the brief's two allowed choices this picks, and why), `command` (this process's own
  command line, truncated -- `_current_command_line`), `time` (human-readable local time with a
  UTC offset, never a bare epoch -- `_current_local_time_string`). One `key=value` line per
  field, in that order; pinned exactly by
  `tests/test_docker_test_lock_cross_process.py::test_holder_sidecar_record_format_is_pinned`.
- **Read by the waiter, printed in the WAITING line** (`_describe_holder`) -- but only after a
  liveness check (`_pid_is_alive`, `os.kill(pid, 0)`, `EPERM` counted as alive): the sidecar is
  **advisory, not authoritative**, and this is the one place a careless implementation would be
  worse than `lsof`. A `SIGKILL`ed holder never runs its own cleanup (same gap this module's own
  "Why `flock`, not a PID file" section describes for the lock itself), so a sidecar can
  genuinely name a pid that is long gone -- the waiter must never report a dead pid as though it
  still held the lock. A dead recorded pid is reported as a **stale** record, in those words,
  naming the pid; a live one is reported as the holder. An absent, empty, truncated, or
  unparsable sidecar (`_read_holder_record` returning `None`) is not an error -- it is exactly
  today's status quo (a lock whose holder is unknown), and the waiter blocks precisely as it
  always has.
- **Removed by the holder** (`_remove_holder_record`, best-effort) when its OUTERMOST block
  exits, success or exception -- the `try`/`finally` around `yield` in `lock_docker_tests`
  covers both paths identically. This is a courtesy for the ORDINARY case; it changes nothing
  about the advisory-only guarantee above, which exists precisely because this cleanup does NOT
  run on `SIGKILL`.

The lock itself is still the `flock`; the sidecar is still only a note beside it.

# Usage

    from altavista.docker_test_lock import lock_docker_tests

    def test_something_docker_gated():
        with lock_docker_tests():
            ...  # the ENTIRE docker-gated body, not just around a prune call

Question 199: this module never mutates the process environment. `lock_path()` only *reads*
`$HOME`.
"""
from __future__ import annotations

import contextlib
import fcntl
import os
import sys
import threading
import time
from pathlib import Path
from typing import Iterator

# Relative to $HOME -- see this module's own doc comment for why under $HOME specifically, and
# why crates/av-lockstep/src/docker_test_lock.rs's LOCK_RELATIVE_PATH must name the identical
# path.
_LOCK_RELATIVE_PATH = Path(".altavista") / "locks" / "docker-tests.lock"


class _HeldState(threading.local):
    """This THREAD's current hold on the lock. See `lock_docker_tests`'s "Re-entrancy".

    `depth` counts nested `with lock_docker_tests():` blocks on THIS thread: 0 when this
    thread holds nothing, 1 while its outermost block is open, more while nested ones are.

    **Thread-local, deliberately, and this is the load-bearing detail.** `flock` on a
    second, independently-opened descriptor in the SAME process genuinely does block against
    the first -- the Rust half measures exactly that in
    `docker_test_lock::tests::flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions`,
    and `crate::docker::prune_stale_test_resources` takes a `&DockerTestLock` precisely so
    that same-process serialization is never accidentally relied on to nest. So
    same-process, cross-THREAD exclusion is a real property that this module must keep. A
    process-global counter would destroy it: a second thread would see a non-zero depth,
    conclude the lock was already held on its behalf, and walk straight into Docker while
    the first thread was still using it. Per-thread, each thread takes its own real `flock`
    and the two serialize exactly as before; only genuine nesting ON ONE THREAD is counted.

    The descriptor itself is deliberately NOT stored here. The outermost `lock_docker_tests`
    frame owns it in a local and closes it in its own `finally`, which is what preserves the
    "released by the kernel on process death, with no userspace code involved" property this
    module's own "Why `flock`, not a PID file" section depends on. A copy here would be a
    second reference that nothing reads and that could only ever disagree with the real one.
    """

    depth: int = 0


_state = _HeldState()


def lock_path() -> Path:
    """`$HOME/.altavista/locks/docker-tests.lock`. Raises if `$HOME` is unset -- a hard error at
    the call site, never a silent no-lock fallback (docs/open-questions.md question 207's
    ruling, restated on the Python side identically to the Rust side's own
    `lock_file_path`/`lock_docker_tests` panic-on-missing-`$HOME` behaviour)."""
    home = os.environ.get("HOME")
    if not home:
        raise RuntimeError(
            "$HOME is not set -- cannot locate the docker-test lock file "
            f"({_LOCK_RELATIVE_PATH}); this is a hard error, not a silent no-lock fallback "
            "(docs/open-questions.md question 207)"
        )
    return Path(home) / _LOCK_RELATIVE_PATH


# Appended to `lock_path()`'s own path by STRING concatenation -- see this module's own "The
# holder sidecar" doc section for why concatenation specifically (never `Path.with_suffix`,
# never a `Path` join) and why the Rust side's `HOLDER_SIDECAR_SUFFIX`
# (`crates/av-lockstep/src/docker_test_lock.rs`) must name the identical string: **if you change
# this, change that, in the same commit.**
_HOLDER_SIDECAR_SUFFIX = ".holder"

# The four fields every holder record has, in the order they are written and expected to be
# read back -- pinned by `tests/test_docker_test_lock_cross_process.py::
# test_holder_sidecar_record_format_is_pinned`.
_HOLDER_FIELDS = ("pid", "tree", "command", "time")

# An unbounded command line in a lock-contention diagnostic is its own footgun (a `pytest`
# invocation naming forty test IDs, say) -- truncate rather than write it whole.
_COMMAND_MAX_LEN = 200


def holder_sidecar_path(lock_file_path: Path) -> Path:
    """The advisory holder-record sidecar for `lock_file_path` -- see this module's own "The
    holder sidecar" doc section for the full shape and the advisory-only guarantee this function
    exists to compute the path for."""
    return Path(str(lock_file_path) + _HOLDER_SIDECAR_SUFFIX)


def _current_tree() -> str:
    """The worktree this process is running in, answered as the current working directory --
    the cheaper of the two honest options this task's own brief allows (the other being
    `git rev-parse --show-toplevel`). Chosen deliberately: the field exists to tell worktrees
    apart (round 4's own incident: `AltaVista-edge` against `AltaVista-aiplane` against
    `AltaVista`), and every cwd a docker-gated test runs with already names its worktree as a
    path prefix. `pytest` runs at the repository root; `cargo test` runs each test binary with
    its cwd at the PACKAGE directory (e.g. `<worktree>/crates/av-lockstep`), not the repository
    root -- a correction made in review to this docstring's first draft, which claimed the root
    in both cases. `git rev-parse --show-toplevel` would normalise that to the root at the cost
    of a subprocess spawn on every acquisition, including the uncontended silent path, for no
    gain in telling worktrees apart."""
    return os.getcwd()


def _current_command_line() -> str:
    """This process's own command line: `sys.executable` followed by `sys.argv` exactly as this
    interpreter received them (not `/proc/self/cmdline` -- this module's own hosts are macOS,
    which has no `/proc`), truncated to `_COMMAND_MAX_LEN` characters and with any embedded
    newlines flattened to spaces (the sidecar's own format is one field per line; a command line
    containing one must not be allowed to forge extra fields)."""
    command = " ".join([sys.executable, *sys.argv]).replace("\n", " ").replace("\r", " ")
    if len(command) > _COMMAND_MAX_LEN:
        command = command[:_COMMAND_MAX_LEN] + "...(truncated)"
    return command


def _current_local_time_string() -> str:
    """The current local time, human-readable without conversion -- includes the UTC offset,
    never a bare epoch. `time.strftime` on POSIX is a thin wrapper over the C library's own
    `strftime(3)`, called here with the IDENTICAL format string
    (`"%Y-%m-%d %H:%M:%S %z"`) the Rust side's `format_current_local_time`
    (`crates/av-lockstep/src/docker_test_lock.rs`) passes to `libc::strftime` directly -- the two
    languages produce the same shape by construction, both ultimately going through the same
    platform C library, not merely by choosing to write similar-looking code."""
    return time.strftime("%Y-%m-%d %H:%M:%S %z", time.localtime())


def _pid_is_alive(pid: int) -> bool:
    """True iff `pid` names a process that still exists on this host, checked the same way the
    Rust side does (`libc::kill(pid, 0)`): `os.kill(pid, 0)` sends no signal, only asks.
    `ProcessLookupError` (`ESRCH`) means the process is genuinely gone. `PermissionError`
    (`EPERM`, e.g. a pid now owned by a different user) means it still exists -- the kernel
    checks existence before permission, so `EPERM` could only be raised for a live pid. Biased
    toward "alive": never report a live holder as stale."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _write_holder_record(sidecar_path: Path) -> None:
    """Writes this process's own holder record: pid, tree, command, time, one `key=value` line
    per field, in that order (see this module's own "The holder sidecar" doc section, and
    `tests/test_docker_test_lock_cross_process.py::test_holder_sidecar_record_format_is_pinned`,
    which pins this exact shape against a real file a real child process wrote). Written to a
    temp file in the SAME directory, then `os.replace`d into place -- atomic on POSIX, so a
    concurrent reader (`_read_holder_record`, e.g. a waiter's own `_describe_holder`) never
    observes a half-written record. That reader already tolerates a missing/corrupt sidecar
    regardless (the whole point of this module's own "the sidecar is advisory" doc section); this
    is cheap extra care, not a correctness requirement either way."""
    record = "".join(
        f"{key}={value}\n"
        for key, value in (
            ("pid", os.getpid()),
            ("tree", _current_tree()),
            ("command", _current_command_line()),
            ("time", _current_local_time_string()),
        )
    )
    tmp_path = sidecar_path.with_name(sidecar_path.name + f".tmp.{os.getpid()}")
    tmp_path.write_text(record)
    os.replace(tmp_path, sidecar_path)


def _remove_holder_record(sidecar_path: Path) -> None:
    """Best-effort only -- the sidecar is advisory (this module's own doc section on that), so a
    failure to remove it must never be treated as a failure to release the LOCK itself, which is
    the real `flock` release immediately after this call runs, unconditionally."""
    try:
        sidecar_path.unlink()
    except OSError:
        pass


def _read_holder_record(sidecar_path: Path) -> dict[str, str] | None:
    """The current holder record, or `None` if the sidecar is absent, empty, truncated, or
    missing any of the four expected fields -- every one of those means "no information
    available", never an error (this module's own "the sidecar is advisory" doc section: an
    absent/corrupt sidecar changes nothing about how a waiter behaves)."""
    try:
        raw = sidecar_path.read_text()
    except (OSError, UnicodeDecodeError):
        # `OSError`: absent, a directory, unreadable permissions, etc. `UnicodeDecodeError`
        # (NOT an `OSError` subclass -- caught explicitly on purpose): a garbled sidecar is not
        # guaranteed to even be valid UTF-8 (measured directly by this module's own
        # `tests/test_docker_test_lock_cross_process.py::test_absent_or_corrupt_sidecar_changes_
        # nothing`, whose "corrupt" case writes raw non-UTF-8 bytes and, before this except
        # clause existed, crashed the WAITING waiter outright instead of reporting "no readable
        # holder record" -- exactly the "worse than lsof" failure mode this module's own "the
        # sidecar is advisory" doc section warns against).
        return None
    fields: dict[str, str] = {}
    for line in raw.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            fields[key] = value
    if not all(key in fields for key in _HOLDER_FIELDS):
        return None
    if not fields["pid"].isdigit():
        return None
    return fields


def _format_holder_fields(record: dict[str, str]) -> str:
    return f"pid={record['pid']} tree={record['tree']} command={record['command']!r} time={record['time']}"


def _describe_holder(sidecar_path: Path) -> str:
    """A human-readable clause describing whoever the sidecar says currently holds the lock, for
    the WAITING line -- see this module's own "the sidecar is advisory" doc section for the
    liveness check performed before anyone is called the holder: a dead recorded pid is reported
    as a STALE record, in those words, naming the pid, never silently as though it were still
    holding the lock."""
    record = _read_holder_record(sidecar_path)
    if record is None:
        return "holder: unknown (no readable holder record)"
    pid = int(record["pid"])
    fields = _format_holder_fields(record)
    if _pid_is_alive(pid):
        return f"holder: {fields}"
    return f"stale holder record ({fields}) -- pid {pid} is no longer running"


@contextlib.contextmanager
def lock_docker_tests() -> Iterator[None]:
    """Exclusive, host-wide, cross-language lock over every Docker-gated test on this machine.
    Blocks until free. Acquire this for a docker-gated test's WHOLE body -- see this module's
    own doc comment for why a narrower scope already reproduced question 207's own failure once
    (on the Rust side; the exposure is identical here).

    Unlike the Rust side's `DockerTestLock` (an RAII guard `prune_stale_test_resources` requires
    proof of via a `&DockerTestLock` parameter), Python has no equivalent-by-construction
    resource this repository's Python code shells out to for the daemon-wide prune -- every
    Python docker-gated test in this workspace calls `docker`/`docker rm`/`docker rmi` directly,
    never a shared Python "prune everything labeled" function. So there is no second function
    here whose signature needs to demand a token; the ONE rule is the one stated above: hold
    this context manager for the test's entire body.

    # Re-entrancy (heavy round 5) -- why this is counted rather than re-acquired

    `flock(2)` attaches its lock to the OPEN FILE DESCRIPTION, which is exactly why this
    module chose it (see "Why `flock`, not a PID file" above). The same property makes a
    naive nested acquisition a guaranteed self-deadlock: an inner call would `os.open` a
    SECOND description of the same path and then block in `LOCK_EX` against a lock THIS
    process already holds on the first one -- forever, because nothing will ever release it.

    That is not hypothetical. It was measured on this host: a run of
    `tests/test_tiles_container.py` sat for 110 minutes with 1.3 seconds of CPU and no
    children, `sample` showing it blocked in `flock`, with TWO descriptors open on this
    lock file -- while a second team's docker-gated test and a third team's workspace
    `cargo test` queued behind it. The nesting is ordinary and legitimate:
    `tests/heavy_stack.py` wraps two different fixtures in this context manager, and a test
    that needs both has both open at once.

    Note precisely what this does and does not change, because the distinction is the whole
    design. The counter is PER THREAD ([`_HeldState`] is a `threading.local`), so only a
    nested acquisition on the SAME thread is counted. Same-process, cross-THREAD exclusion is
    a real property -- `flock` on a second, independently-opened descriptor blocks even
    within one process, which the Rust half measures directly in
    `docker_test_lock::tests::flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions`
    -- and it is preserved untouched: a second thread's depth is its own 0, so it takes its
    own real `flock` and serialises against the first exactly as before. Cross-process,
    cross-language exclusion (the property question 207 actually asks for, and the one
    `crates/av-lockstep/src/docker_test_lock.rs`'s cross-language test pins) is likewise
    untouched: per holding thread, exactly one `flock` on exactly one descriptor, released
    only when that thread's OUTERMOST block exits.

    The Rust half solves the same nesting problem a different way, by types rather than by
    counting: `crate::docker::prune_stale_test_resources` takes a `&DockerTestLock`, so "the
    caller already holds it" is a compile-time fact and nothing ever re-acquires. Python has
    no equivalent-by-construction resource here (this module's own doc says so), which is why
    the Python half counts instead.
    """
    if _state.depth > 0:
        # Already held by THIS THREAD (an outer `with lock_docker_tests():` is still open).
        # Re-acquiring would `os.open` a SECOND file description and block on `flock`
        # forever against our own lock -- see this function's own "Re-entrancy" section.
        # Count the nesting and hand the caller the lock this thread already holds.
        _state.depth += 1
        try:
            yield
        finally:
            _state.depth -= 1
        return

    path = lock_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    sidecar_path = holder_sidecar_path(path)
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
    try:
        # Try non-blocking first: if nothing else holds it, this is the whole acquisition --
        # silent, exactly as before.
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            holder_desc = _describe_holder(sidecar_path)
            sys.stderr.write(
                f"WAITING for the docker-test lock ({path}): another process on this host "
                f"currently holds it ({holder_desc}) -- blocking until it releases "
                "(docs/open-questions.md question 207: this lock is host-wide, not per-worktree, "
                "so waiting here under real contention is expected, not a hang)\n"
            )
            sys.stderr.flush()
            waited_since = time.monotonic()
            fcntl.flock(fd, fcntl.LOCK_EX)
            waited_s = time.monotonic() - waited_since
            sys.stderr.write(f"ACQUIRED the docker-test lock ({path}) after waiting {waited_s:.3f}s\n")
            sys.stderr.flush()
        # This IS the outermost acquisition on this thread (the re-entrant, already-held branch
        # above already returned before reaching here) -- question 234: the taker writes its own
        # holder record exactly once per real `flock`, never on a nested re-acquisition.
        _write_holder_record(sidecar_path)
        _state.depth = 1
        yield
    finally:
        _state.depth = 0
        # Best-effort, and ordering does not matter for correctness (the sidecar is advisory --
        # see this module's own "the sidecar is advisory" doc section): remove it before
        # releasing the real `flock` so a waiter that unblocks immediately after never reads a
        # stale record belonging to THIS process.
        _remove_holder_record(sidecar_path)
        # Explicit unlock for clarity in the ordinary (non-killed) case; the actual SIGKILL-safe
        # guarantee comes from the kernel closing this fd on process death regardless of whether
        # this `finally` block ever runs at all -- see this module's own "Why flock, not a PID
        # file" doc section.
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)

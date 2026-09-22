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
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
    try:
        # Try non-blocking first: if nothing else holds it, this is the whole acquisition --
        # silent, exactly as before.
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            sys.stderr.write(
                f"WAITING for the docker-test lock ({path}): another process on this host "
                "currently holds it -- blocking until it releases (docs/open-questions.md "
                "question 207: this lock is host-wide, not per-worktree, so waiting here under "
                "real contention is expected, not a hang)\n"
            )
            sys.stderr.flush()
            waited_since = time.monotonic()
            fcntl.flock(fd, fcntl.LOCK_EX)
            waited_s = time.monotonic() - waited_since
            sys.stderr.write(f"ACQUIRED the docker-test lock ({path}) after waiting {waited_s:.3f}s\n")
            sys.stderr.flush()
        _state.depth = 1
        yield
    finally:
        _state.depth = 0
        # Explicit unlock for clarity in the ordinary (non-killed) case; the actual SIGKILL-safe
        # guarantee comes from the kernel closing this fd on process death regardless of whether
        # this `finally` block ever runs at all -- see this module's own "Why flock, not a PID
        # file" doc section.
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)

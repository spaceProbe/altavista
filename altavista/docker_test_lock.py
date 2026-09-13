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
from pathlib import Path
from typing import Iterator

# Relative to $HOME -- see this module's own doc comment for why under $HOME specifically, and
# why crates/av-lockstep/src/docker_test_lock.rs's LOCK_RELATIVE_PATH must name the identical
# path.
_LOCK_RELATIVE_PATH = Path(".altavista") / "locks" / "docker-tests.lock"


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
    """
    path = lock_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        # Explicit unlock for clarity in the ordinary (non-killed) case; the actual SIGKILL-safe
        # guarantee comes from the kernel closing this fd on process death regardless of whether
        # this `finally` block ever runs at all -- see this module's own "Why flock, not a PID
        # file" doc section.
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)

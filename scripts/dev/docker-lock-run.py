#!/usr/bin/env python3
"""docker-lock-run.py -- run a command while holding the host-wide docker-test lock.

    .venv/bin/python scripts/dev/docker-lock-run.py -- <command> [args...]

The lock is the repository's own, `altavista.docker_test_lock.lock_docker_tests()` (question 207:
exclusive, host-wide, `flock(2)` on `$HOME/.altavista/locks/docker-tests.lock`, cross-language with
the Rust side), including its holder sidecar (`<lock>.holder`, written with THIS process's pid, the
tree, the command line and the time, read by anyone who has to wait). This script only imports it;
it does not change it. It blocks, announcing the wait, until the lock is free; it runs the command
as a child while it holds the lock; the command's exit status is this script's; SIGINT and SIGTERM
are forwarded to the child. The lock is released when this process exits by any means (the kernel
closes the descriptor), so a SIGKILL cannot leave it held.

It exists so that a committed shell recipe (third_party/zcu104-boot/build-boot-bin.sh) can take the
lock itself instead of relying on a caller's wrapper: the recipe re-executes itself under this
script. It must not be a way to run anything the shell policy refuses; it runs exactly the command
it is given, after the lock.

No self-deadlock: `flock` is per open file description, so a child that took the lock again while
its parent holds it would wait forever. Two things prevent that. The child is started with
`AV_DOCKER_LOCK_HELD=<pid of this process>`, and a nested use of this script that finds the variable
set to a live pid runs its command without taking the lock again. And if the holder sidecar names a
live ancestor of this process (someone else's wrapper around the caller), the lock is likewise taken
to be held already.
"""
from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

from altavista.docker_test_lock import holder_sidecar_path, lock_docker_tests, lock_path  # noqa: E402

ENV_HELD = "AV_DOCKER_LOCK_HELD"


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _ancestors() -> set[int]:
    out: set[int] = set()
    pid = os.getpid()
    while pid > 1:
        r = subprocess.run(["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True)
        try:
            pid = int(r.stdout.strip())
        except ValueError:
            break
        out.add(pid)
    return out


def _sidecar_pid() -> int | None:
    try:
        for line in holder_sidecar_path(lock_path()).read_text().splitlines():
            if line.startswith("pid="):
                return int(line[4:])
    except (OSError, ValueError):
        pass
    return None


def _run(cmd: list[str], env: dict[str, str]) -> int:
    child = subprocess.Popen(cmd, env=env)

    def forward(signum, _frame):  # noqa: ANN001
        child.send_signal(signum)

    signal.signal(signal.SIGINT, forward)
    signal.signal(signal.SIGTERM, forward)
    return child.wait()


def main(argv: list[str]) -> int:
    args = argv[1:]
    if args[:1] == ["--"]:
        args = args[1:]
    if not args:
        sys.stderr.write("usage: docker-lock-run.py -- <command> [args...]\n")
        return 2
    held = os.environ.get(ENV_HELD, "")
    held_by_ancestor = held.isdigit() and _alive(int(held)) and int(held) in _ancestors()
    sidecar_pid = _sidecar_pid()
    if sidecar_pid is not None and sidecar_pid in _ancestors():
        held_by_ancestor = True
    if held_by_ancestor:
        sys.stderr.write("docker-lock-run: the docker-test lock is already held by an ancestor process; not taking it again\n")
        return _run(args, {**os.environ, ENV_HELD: str(sidecar_pid or held)})
    with lock_docker_tests():
        return _run(args, {**os.environ, ENV_HELD: str(os.getpid())})


if __name__ == "__main__":
    sys.exit(main(sys.argv))

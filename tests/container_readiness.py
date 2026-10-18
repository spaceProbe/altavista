"""Readiness waits for containers whose processes share one log stream (question 252 item 4,
question 249).

# The defect this module exists to remove

`tests/test_proposer_container.py` runs `av-command` and `av-gateway` in ONE container. Both
write their startup lines with `eprintln!`, and Rust's `stderr` is unbuffered, so one logical
line reaches the pipe as several `write(2)` calls. Two processes writing to one pipe therefore
interleave *inside* a line, and `docker logs` shows the bytes in the order the pipe delivered
them. The old wait looked for each readiness line as a substring of that combined text; last
round's gate saw `av-gateway`'s line arrive split around `av-command`'s output (see
`REPLAYED_INTERLEAVED_STDERR` in the test module, copied from that gate's log), so the needle
never matched although both processes were listening, and the wait timed out after 30 s.

A substring of a shared stream is not a readiness signal. This module offers one that does not
read the stream at all: it connects, from INSIDE the container's own network namespace, to the
port the process must be listening on.

# Why the connect has to run inside the container

Docker's port publisher accepts a TCP connect at t~0.01 s, before the process inside is
listening (question 233's measured readiness race), so a host-side connect to a published port
proves nothing. A connect made by `docker exec` inside the container has no publisher in
the path: on the loopback or on the container's own address, the kernel answers a SYN with a
connection only when some socket is in the LISTEN state, and with a refusal otherwise. The probe
is bash's built-in `/dev/tcp` redirection run under `timeout`, which exists in every Debian
image (the proposer image and the python slim image both have it, measured), so no probe binary
is added to any image.

A connect is also a stricter signal than the lines it replaces: `av-command` and `av-gateway`
both print their "listening" line BEFORE they call `serve`, so the line is a statement of
intent, not of readiness.
"""
from __future__ import annotations

import re
import subprocess
import time
from dataclasses import dataclass
from typing import Callable, Sequence

# A target host that means "the container's own address on its network" (what a peer container
# dials), resolved inside the container with `hostname`, as opposed to its loopback.
SELF = "@self"

_SAFE = re.compile(r"^[A-Za-z0-9_.:-]+$")


class ReadinessTimeout(Exception):
    """A bounded readiness wait gave up. The message says what was observed."""


@dataclass(frozen=True)
class Listener:
    """A TCP port some process in the container must be listening on."""

    label: str
    host: str  # an IP/hostname valid inside the container, or `SELF`
    port: int

    def __post_init__(self) -> None:
        if not _SAFE.match(self.label) or not (self.host == SELF or _SAFE.match(self.host)):
            raise ValueError(f"unsafe listener {self!r}: label and host become part of a shell command")
        if not (0 < self.port < 65536):
            raise ValueError(f"bad port in {self!r}")


# One probe call: the listeners to check -> {label: None if it accepted the connect, else the
# reason it did not}. Injected so the wait can be driven without Docker.
Probe = Callable[[Sequence[Listener]], "dict[str, str | None]"]


def probe_script(listeners: Sequence[Listener]) -> str:
    """The bash program `exec_probe` runs inside the container: one `UP`/`DOWN` line per
    listener, in order. Each connect is its own bounded `timeout 2 bash -c 'exec 3<>/dev/tcp/H/P'`."""
    lines = ['self=$(hostname 2>/dev/null || echo 127.0.0.1)']
    for lst in listeners:
        host = '"$self"' if lst.host == SELF else lst.host
        lines.append(
            f'if out=$(timeout 2 bash -c "exec 3<>/dev/tcp/{host}/{lst.port}" 2>&1); '
            f'then echo "UP {lst.label}"; else echo "DOWN {lst.label} $(echo "$out" | tr "\\n" " ")"; fi'
        )
    return "\n".join(lines)


def parse_probe_output(listeners: Sequence[Listener], stdout: str) -> "dict[str, str | None]":
    seen: dict[str, "str | None"] = {}
    for line in stdout.splitlines():
        parts = line.split(" ", 2)
        if parts[0] == "UP" and len(parts) >= 2:
            seen[parts[1]] = None
        elif parts[0] == "DOWN" and len(parts) >= 2:
            seen[parts[1]] = parts[2].strip() if len(parts) > 2 and parts[2].strip() else "connect failed"
    # A listener the probe did not report on (the exec itself failed) is not up.
    return {lst.label: seen.get(lst.label, "no answer from the probe (docker exec failed?)") for lst in listeners}


def exec_probe(container: str, *, exec_timeout_s: float = 20.0) -> Probe:
    """A `Probe` that runs `probe_script` in `container` through `docker exec`."""

    def probe(listeners: Sequence[Listener]) -> "dict[str, str | None]":
        result = subprocess.run(
            ["docker", "exec", container, "/bin/bash", "-c", probe_script(listeners)],
            capture_output=True, text=True, timeout=exec_timeout_s,
        )
        outcome = parse_probe_output(listeners, result.stdout or "")
        if result.returncode != 0 and not (result.stdout or "").strip():
            reason = (result.stderr or "").strip() or f"docker exec exited {result.returncode}"
            return {label: f"docker exec failed: {reason}" for label in outcome}
        return outcome

    return probe


def container_state(container: str) -> str:
    result = subprocess.run(
        ["docker", "inspect", container, "--format", "{{.State.Status}} (exit {{.State.ExitCode}})"],
        capture_output=True, text=True, timeout=30,
    )
    return result.stdout.strip() or (result.stderr or "").strip() or "unknown"


def wait_for_listeners(
    probe: Probe,
    listeners: Sequence[Listener],
    timeout_s: float,
    *,
    poll_s: float = 0.2,
    state: "Callable[[], str] | None" = None,
    logs: "Callable[[], str] | None" = None,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> str:
    """Polls `probe` until every listener accepts a connect; bounded by `timeout_s`. Returns a
    one-line-per-listener summary of the last (successful) probe. Raises `ReadinessTimeout`
    naming, for each listener, the reason it was still down, plus the container `state` and the
    tail of its `logs` when those callables are given (the logs are context only; nothing here
    parses them). If `state()` reports the container is no longer running, it gives up at once."""
    deadline = clock() + timeout_s
    last: "dict[str, str | None]" = {}
    attempts = 0
    while True:
        attempts += 1
        last = probe(listeners)
        if all(reason is None for reason in last.values()):
            return "\n".join(f"{lst.label}: accepting connections on {lst.host}:{lst.port} (probe attempt {attempts})" for lst in listeners)
        current_state = state() if state is not None else None
        gone = current_state is not None and not current_state.startswith("running")
        if gone or clock() > deadline:
            observed = "\n".join(
                f"  {lst.label} {lst.host}:{lst.port}: " + ("accepting" if last[lst.label] is None else f"NOT accepting -- {last[lst.label]}")
                for lst in listeners
            )
            tail = ""
            if logs is not None:
                text = logs()
                tail = f"\n--- container logs so far (context only; processes sharing a stream may interleave) ---\n{text[-2000:]}"
            why = f"the container is no longer running ({current_state})" if gone else f"{timeout_s}s elapsed"
            state_line = f" (state: {current_state})" if current_state is not None else ""
            raise ReadinessTimeout(f"listeners not ready{state_line}: {why}, after {attempts} probe attempt(s); last observation:\n{observed}{tail}")
        sleep(poll_s)


def wait_for_log_substrings(
    read_logs: "Callable[[], tuple[str, str]]",
    needles: Sequence[str],
    timeout_s: float,
    *,
    poll_s: float = 0.1,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> str:
    """THE OLD WAIT, kept only as the reference the reproduction tests run against (it is not
    used to decide readiness anywhere). Polls `read_logs() -> (stdout, stderr)` until every
    needle is a SUBSTRING of the combined text. When two processes share the stream and their
    writes interleave inside a line, a needle can be absent for ever though its process is up."""
    deadline = clock() + timeout_s
    while True:
        out, err = read_logs()
        combined = (out or "") + (err or "")
        if all(needle in combined for needle in needles):
            return combined
        if clock() > deadline:
            raise ReadinessTimeout(f"never saw all of {tuple(needles)} within {timeout_s}s -- logs so far:\n{combined}")
        sleep(poll_s)

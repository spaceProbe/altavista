"""Shared, docker-gated assertions for the container-hardening posture ADR-004's "Plugins are
untrusted code" bullet describes for the DEVELOPMENT substrate (docs/open-questions.md question
207's ruling on edge round 3's review defect 2): non-root user, read-only root with exactly one
declared writable volume, `--cap-drop ALL`, `--security-opt no-new-privileges`, the DEFAULT
seccomp profile (never `--security-opt seccomp=unconfined`).

# Why this module exists, shared

`tests/test_edge_plugin_container.py` (gated on the built `av-edge-plugin:local` image -- absent
on this host today, see that file's own module doc for the measured disk-space root cause) and
`tests/test_edge_plugin_hardening_alpine.py` (a self-contained `alpine:latest`-based proof that
runs for real on THIS host TODAY, regardless of whether the plugin image exists) both assert the
IDENTICAL posture, from the IDENTICAL two vantage points, on their own labelled, running
container:

1. `docker inspect` (of a running-or-just-exited-but-not-yet-removed container -- every field
   this checks is fixed at `docker create`/`docker run` time and does not change when the
   container's own process exits, so it is valid either way): `assert_inspect_hardening`.
2. `docker exec` of the RUNNING container -- the kernel's own view, which is what actually
   matters, not merely the absence of a flag: `assert_exec_hardening`.

Both test files call these SAME two functions on their own container. "Same flags, same
assertions, one gated on the plugin image and one not" (this task's own brief) is therefore true
by construction -- one shared implementation, not two similar-looking copies that could drift
apart.

Question 199: nothing here mutates the process environment; every `docker`/subprocess call below
passes no extra environment at all.
"""
from __future__ import annotations

import json
import subprocess
from typing import Any


class VolumeWriteDiskExhausted(RuntimeError):
    """Raised by `assert_exec_hardening` when the write-probe to the declared writable volume
    fails specifically with ENOSPC ("No space left on device") -- the measured host-wide disk
    condition this round's own module docs record (the Colima VM's container filesystem at 0
    bytes free; see `tests/test_edge_plugin_container.py`'s and `tests/
    test_edge_plugin_hardening_alpine.py`'s own module docs for the numbers), not a defect in
    the hardening posture itself. Every OTHER fact `assert_exec_hardening` checks (non-root uid,
    `NoNewPrivs`, `Seccomp`, and the read-only-root write correctly failing) is asserted first
    and is real evidence either way -- `partial_facts` carries whatever was confirmed before the
    volume write was attempted, so a caller that catches this and skips (rather than failing
    outright, per question 194: a real, named environmental blocker is a visible skip, not a
    silent pass and not a false failure that reads as a broken implementation) can still print
    what it did prove (question 148)."""

    def __init__(self, message: str, partial_facts: dict[str, Any]) -> None:
        super().__init__(message)
        self.partial_facts = partial_facts

# `crates/av-lockstep/src/docker.rs`'s own established convention (`TEST_LABEL_KEY`/
# `TEST_LABEL_VALUE`), reused verbatim -- `tests/test_edge_plugin_container.py` already follows
# it for its own containers/networks; both callers of this module use it for volumes too.
TEST_LABEL_KEY = "av.test"
TEST_LABEL_VALUE = "1"

# The `docker run` flags this posture needs, EXCLUDING the writable-volume mount (which names a
# per-run volume, so each caller appends its own `-v <volume>:<dest>`) and EXCLUDING any
# `--user` (the plugin image bakes its own non-root `USER` in; the alpine-based test passes its
# own `--user` explicitly since `alpine:latest` declares no such user). Deliberately no
# `--security-opt seccomp=...` entry at all -- see this module's own doc: the DEFAULT profile
# Docker applies whenever no seccomp override is given is what `assert_exec_hardening` proves
# (`Seccomp: 2`, filter mode), which requires NOT naming a profile, not even the default one by
# name.
HARDENING_RUN_FLAGS: list[str] = ["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"]


def _run(*args: str, timeout: float = 30.0) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)


def prune_stale_labelled_resources() -> None:
    """Question 156: prune by label before creating. Removes every container, volume, and
    network still carrying `TEST_LABEL_KEY` from a PREVIOUS run that crashed before its own
    cleanup ran -- mirrors `av_lockstep::docker::prune_stale_test_resources`'s daemon-wide sweep
    on the Rust side. Safe to call unconditionally here because both callers hold
    `altavista.docker_test_lock.lock_docker_tests()` for their entire test body before calling
    this, so no concurrent docker-gated test on this host can be creating resources of its own
    at the same moment (question 207: that lock is exactly what makes this safe)."""
    label_filter = f"label={TEST_LABEL_KEY}"
    stale_containers = _run("ps", "-a", "--filter", label_filter, "-q").stdout.split()
    for cid in stale_containers:
        _run("rm", "-f", cid)
    stale_networks = _run("network", "ls", "--filter", label_filter, "-q").stdout.split()
    for net in stale_networks:
        _run("network", "rm", net)
    # Volumes last -- a volume still referenced by a container-removal race would fail to
    # remove; containers/networks above are gone by this point on this same host (this whole
    # function runs under the host-wide docker-test lock).
    stale_volumes = _run("volume", "ls", "--filter", label_filter, "-q").stdout.split()
    for vol in stale_volumes:
        _run("volume", "rm", vol)


def label_args(run_id: str) -> list[str]:
    """`--label av.test=1 --label av.test.run_id=<run_id>`, repeated on every container/volume/
    network a caller creates -- the SAME two labels `tests/test_edge_plugin_container.py`'s own
    `ResourceGuard.label_args` already applies to its containers/networks."""
    return ["--label", f"{TEST_LABEL_KEY}={TEST_LABEL_VALUE}", "--label", f"av.test.run_id={run_id}"]


def _docker_inspect_one(name: str) -> dict[str, Any]:
    result = _run("inspect", name)
    assert result.returncode == 0, f"docker inspect {name} failed (rc={result.returncode}): {result.stderr}"
    data = json.loads(result.stdout)
    assert len(data) == 1, f"docker inspect {name} returned {len(data)} objects, expected 1"
    return data[0]


def assert_inspect_hardening(container_name: str, *, writable_mount_dest: str | None = None) -> dict[str, Any]:
    """From `docker inspect` of `container_name`: a non-root `.Config.User`, `.HostConfig.
    ReadonlyRootfs` is true, `.HostConfig.CapDrop` contains `ALL`, `.HostConfig.SecurityOpt`
    contains a `no-new-privileges` entry, and -- iff `writable_mount_dest` is given -- a
    `.Mounts` entry there that is RW. Valid whether the container is still running or has
    already exited (not yet removed) -- every field checked here is fixed at create time.
    Returns the full inspect object (question 148: so the caller can print more of what was
    actually observed, not just what was asserted).

    `writable_mount_dest=None` (round 3, `tests/test_tiles_container.py`) is for an image that
    declares NO writable path at all -- `--read-only` applies to its whole root with no
    exception. This is only correct for an image whose production code is independently known
    (grepped, not assumed) to write nothing anywhere; every existing caller of this function
    (`tests/test_edge_plugin_container.py`, `tests/test_edge_plugin_hardening_alpine.py`) still
    passes an explicit destination and is unaffected by this default."""
    info = _docker_inspect_one(container_name)
    config_user = info["Config"]["User"]
    assert config_user not in ("", "0", "root"), f"expected a non-root .Config.User, got {config_user!r}"

    host_config = info["HostConfig"]
    assert host_config["ReadonlyRootfs"] is True, f"expected .HostConfig.ReadonlyRootfs=true, got {host_config['ReadonlyRootfs']!r}"

    cap_drop = host_config.get("CapDrop") or []
    assert "ALL" in cap_drop, f"expected .HostConfig.CapDrop to contain ALL, got {cap_drop!r}"

    security_opt = host_config.get("SecurityOpt") or []
    assert any("no-new-privileges" in opt for opt in security_opt), f"expected .HostConfig.SecurityOpt to contain a no-new-privileges entry, got {security_opt!r}"

    if writable_mount_dest is not None:
        mounts = info.get("Mounts") or []
        matching = [m for m in mounts if m.get("Destination") == writable_mount_dest]
        assert matching, f"expected a mount at {writable_mount_dest}, found .Mounts={mounts!r}"
        assert matching[0].get("RW") is True, f"expected the mount at {writable_mount_dest} to be RW, got {matching[0]!r}"

    return info


def assert_exec_hardening(container_name: str, *, writable_path: str | None = None) -> dict[str, Any]:
    """From `docker exec` of the RUNNING container `container_name` -- the kernel's own view,
    which is what actually matters, not merely the absence of a `docker run` flag: `id -u` is
    non-zero, `/proc/1/status` shows `NoNewPrivs: 1` and `Seccomp: 2` (filter mode -- this is how
    the DEFAULT seccomp profile is proven really applied, rather than merely asserting the
    absence of a `--security-opt seccomp=...` flag), a write to `/` fails, and -- iff
    `writable_path` is given -- a write there succeeds. Requires the container to actually be
    running (unlike `assert_inspect_hardening`) -- callers must not call this after the
    container has exited. Returns the observed facts as a dict (question 148: the caller prints
    these, not just the pass/fail).

    `writable_path=None` (round 3, `tests/test_tiles_container.py`) skips the volume-write probe
    entirely, for an image with no declared writable path at all -- see
    `assert_inspect_hardening`'s own doc for the same case and why it is only correct when the
    image's own production code is independently known to write nothing. Every existing caller
    still passes an explicit path and is unaffected by this default."""
    id_result = _run("exec", container_name, "id", "-u", timeout=15)
    assert id_result.returncode == 0, f"docker exec {container_name} id -u failed (rc={id_result.returncode}): {id_result.stderr}"
    uid = int(id_result.stdout.strip())
    assert uid != 0, f"expected a non-root uid inside the container, got {uid}"

    status_result = _run("exec", container_name, "cat", "/proc/1/status", timeout=15)
    assert status_result.returncode == 0, f"docker exec {container_name} cat /proc/1/status failed (rc={status_result.returncode}): {status_result.stderr}"
    status_fields = {
        line.split(":", 1)[0].strip(): line.split(":", 1)[1].strip()
        for line in status_result.stdout.splitlines()
        if ":" in line
    }
    no_new_privs = status_fields.get("NoNewPrivs")
    assert no_new_privs == "1", f"expected /proc/1/status NoNewPrivs: 1, got {no_new_privs!r} (full status: {status_result.stdout!r})"
    seccomp = status_fields.get("Seccomp")
    assert seccomp == "2", f"expected /proc/1/status Seccomp: 2 (filter mode, i.e. the default profile is applied), got {seccomp!r} (full status: {status_result.stdout!r})"

    # A redirection that fails to even open its target (EROFS under --read-only) means the
    # shell never runs the command at all -- `$?` still reflects that failure since `;` (not
    # `&&`) separates it from the `echo EXIT:$?` that reports it, so this needs no writable
    # scratch path (e.g. /tmp) of its own to record the failure.
    root_write = _run(
        "exec", container_name, "sh", "-c",
        "echo av-hardening-probe > /av_test_write_probe; echo EXIT:$?",
        timeout=15,
    )
    root_write_failed = "EXIT:0" not in root_write.stdout
    assert root_write_failed, f"expected a write to / to fail under --read-only, got stdout={root_write.stdout!r} stderr={root_write.stderr!r}"

    partial_facts = {
        "uid": uid,
        "no_new_privs": no_new_privs,
        "seccomp": seccomp,
        "root_write_failed": root_write_failed,
    }

    if writable_path is None:
        return {**partial_facts, "volume_write_ok": None}

    volume_write = _run(
        "exec", container_name, "sh", "-c",
        f"echo av-hardening-probe > {writable_path}/av_test_write_probe; echo EXIT:$?",
        timeout=15,
    )
    volume_write_ok = "EXIT:0" in volume_write.stdout
    if not volume_write_ok and "No space left on device" in (volume_write.stderr or ""):
        # See `VolumeWriteDiskExhausted`'s own docstring: this is the measured host-wide disk
        # condition, not a hardening defect -- everything else above already passed.
        raise VolumeWriteDiskExhausted(
            f"write to {writable_path} failed with ENOSPC (host disk exhausted), not a "
            f"hardening defect -- stdout={volume_write.stdout!r} stderr={volume_write.stderr!r}",
            partial_facts,
        )
    assert volume_write_ok, f"expected a write to {writable_path} to succeed, got stdout={volume_write.stdout!r} stderr={volume_write.stderr!r}"

    return {**partial_facts, "volume_write_ok": volume_write_ok}

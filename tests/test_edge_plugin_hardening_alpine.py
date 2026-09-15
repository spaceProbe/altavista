"""A self-contained proof, using `alpine:latest`, that the container-hardening flags and
assertions `tests/test_edge_plugin_container.py` applies to the real `av-edge-plugin:local`
image (docs/open-questions.md question 207, edge round 3's own review defect 2) really produce
the claimed values on THIS Docker/Colima host TODAY -- independent of whether that plugin image
has ever been built here.

# Why this file exists, on this host, today (round 5, question 210)

`av-edge-plugin:local` builds again and `tests/test_edge_plugin_container.py` now runs for
real (see that file's own module doc). Round 5 question 210 therefore asked, in so many words,
whether this file's original reason for existing was gone and it should be retired. The
decision (round 5, question 210) is to KEEP it, deliberately, as the probe for hosts without the
plugin image -- for four reasons:

1. `av-edge-plugin:local` has been garbage-collected off this host five separate times
   (question 196(d)/205: Colima's kubelet image collector). Its presence is not a property
   anyone can rely on. A test gated on it is a test that can silently stop covering anything.
2. The two tests do not have the same subject. This file's subject is
   `altavista/container_hardening.py` -- `HARDENING_RUN_FLAGS`, `assert_inspect_hardening`,
   `assert_exec_hardening` -- the SHARED implementation of "what the hardening posture must look
   like from `docker inspect`/`docker exec`". `tests/test_edge_plugin_container.py`'s subject is
   the plugin image. If the shared module regresses while the plugin image happens to be
   missing, only this file would catch it.
3. Together the two tests separate two failures that otherwise look identical: "the hardening
   posture is broken" and "the plugin image is not on this host".
4. It costs almost nothing: `alpine:latest` is 13.6 MB and this test runs in seconds.

What this file technically proves, independent of reason 1-4 above: nothing about the plugin
image itself guarantees that `--read-only` + `--cap-drop ALL` + `--security-opt
no-new-privileges` + a named volume at a declared mount really produce non-root
`.Config.User`, `.HostConfig.ReadonlyRootfs=true`, `.HostConfig.CapDrop` containing `ALL`,
`.HostConfig.SecurityOpt` containing `no-new-privileges`, `NoNewPrivs: 1`, `Seccomp: 2`, a
failed write to `/`, and a successful write to the volume -- on THIS Docker version, THIS
Colima kernel, THIS host's own default seccomp profile -- as opposed to merely being plausible
Docker documentation. `alpine:latest` (a few MB, unrelated to the plugin image -- see
`_compute_skip_reason` below for what happens if it too has been evicted) lets this run for
real, right now, regardless of the plugin image's own fate.

# History: a round-4 stand-in that the round-5 decision above chose to keep

This file was written in round 4 as a stand-in, at a moment when `av-edge-plugin:local` could
not be built at all: the Colima VM's container filesystem was completely full (0 bytes free)
and `services/edge-plugin/build-image.sh` had no room to allocate the layers a build needs, so
nothing on this host could prove the hardening flags actually worked. The user has since
reclaimed that disk (measured 2026-09-15, a snapshot: `overlay 58.8G 15.0G 40.7G 27% /` inside
the Colima VM -- 40.7 GB available, 27% used) and `av-edge-plugin:local` has been rebuilt
(`services/edge-plugin/IMAGE_DIGEST.md` records the current image id). Round 5 (question 210)
considered retiring this file now that its original reason for existing was gone, and chose
instead to keep it deliberately, for the four reasons above. A reader who finds this file should
understand both why it was born (round 4, disk exhaustion) and why it survived (round 5,
question 210: it proves something `test_edge_plugin_container.py` cannot).

# Same flags, same assertions -- literally, not by resemblance

Both `HARDENING_RUN_FLAGS` and the two assertion functions this test calls
(`assert_inspect_hardening`, `assert_exec_hardening`) live in `altavista/container_hardening.py`
and are imported here UNCHANGED -- `tests/test_edge_plugin_container.py`'s own Part 2 imports
and calls the identical functions on its own container. There is exactly one implementation of
"what the hardening posture must look like from `docker inspect`/`docker exec`"; this test and
that one both call it. That is what makes this file a real probe rather than a duplicate: on any
host where `av-edge-plugin:local` is absent, this test still exercises the whole shared
implementation, so the other test's assertions are known-good the moment its image exists. (In
round 4, when the image could not be built at all, that was the only thing keeping those
assertions honest; in round 5 both tests run here, and the property still holds for the next
host that has only one of the two images.)

The one thing this test does NOT share with `test_edge_plugin_container.py`: `--user`.
`services/edge-plugin/Dockerfile` bakes its own non-root `USER edgeplugin` (uid/gid 10001) into
the image, so that test passes no `--user` flag at all. `alpine:latest` declares no such user,
so this test passes `--user 10001:10001` explicitly on `docker run` -- the SAME uid/gid, chosen
for consistency (not because alpine's own `/etc/passwd` needs to agree; Docker accepts a bare
numeric uid:gid with no matching passwd entry, and `id -u`/kernel-level checks work identically
either way).

# Docker-gating, labelling, and locking -- this file's own obligations under questions 154/156/207

- Question 154 (no network at test time): this test never `docker pull`s. If `alpine:latest` is
  not present locally (the same Colima kubelet image-GC phenomenon question 196(d)/205 already
  documents for larger images could evict even this one), `_compute_skip_reason` names that and
  the test skips visibly rather than pulling it.
- Question 156 (label everything, prune by label before creating): every container and volume
  this test creates carries `av.test`/`av.test.run_id` (`altavista.container_hardening.
  label_args`), `prune_stale_labelled_resources()` runs before creating anything, and cleanup
  removes exactly what this run created, asserted afterwards.
- Question 207 (the host-wide docker-test lock): `lock_docker_tests()` wraps this test's entire
  body, exactly as `test_edge_plugin_container.py` already does.
- Question 199 (no process-environment mutation): every subprocess call below passes no extra
  environment at all.
"""
from __future__ import annotations

import json
import subprocess
import uuid

import pytest

from altavista.container_hardening import (
    HARDENING_RUN_FLAGS,
    TEST_LABEL_KEY,
    VolumeWriteDiskExhausted,
    assert_exec_hardening,
    assert_inspect_hardening,
    label_args,
    prune_stale_labelled_resources,
)
from altavista.docker_test_lock import lock_docker_tests

ALPINE_IMAGE = "alpine:latest"
# Chosen only for consistency with `services/edge-plugin/Dockerfile`'s own `edgeplugin` user --
# see this module's own doc for why alpine needs no matching `/etc/passwd` entry for this to work.
PROBE_UID_GID = "10001:10001"
WRITABLE_MOUNT_DEST = "/var/lib/edge-plugin"
HOLD_OPEN_SECONDS = "30"  # generous headroom for a handful of `docker exec` round trips; killed early via `docker rm -f`, never waited out.


def _docker_unavailable_reason() -> "str | None":
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


def _image_present(tag: str) -> bool:
    result = subprocess.run(["docker", "image", "inspect", tag, "--format", "{{.Id}}"], capture_output=True, timeout=30)
    return result.returncode == 0


def _compute_skip_reason() -> "str | None":
    reason = _docker_unavailable_reason()
    if reason is not None:
        return f"Docker not available: {reason}"
    if not _image_present(ALPINE_IMAGE):
        return (
            f"probe image {ALPINE_IMAGE!r} is not present locally, and this test never pulls an "
            f"image itself (question 154: no network at test time) -- it may have been evicted by "
            f"Colima's own kubelet image garbage collector, which removes every image no "
            f"container uses once the VM disk passes its high threshold (questions 196(d)/205; "
            f"that collector took even this image once, mid-round, in round 4). Run "
            f"`docker pull {ALPINE_IMAGE}` once, on a host with network access, then re-run this "
            f"test."
        )
    return None


_SKIP_REASON = _compute_skip_reason()


def _docker(*args: str, timeout: float = 30.0, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if check and result.returncode != 0:
        pytest.fail(f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}")
    return result


@pytest.mark.skipif(_SKIP_REASON is not None, reason=_SKIP_REASON or "")
def test_hardening_flags_produce_the_expected_posture_on_this_docker_colima_host():
    with lock_docker_tests():
        _run_hardening_flags_produce_the_expected_posture_on_this_docker_colima_host()


def _run_hardening_flags_produce_the_expected_posture_on_this_docker_colima_host():
    run_id = uuid.uuid4().hex[:12]
    container_name = f"av-edge-plugin-hardening-alpine-{run_id}"
    volume_name = f"av-edge-plugin-hardening-alpine-{run_id}-state"
    labels = label_args(run_id)

    # Question 156: prune by label before creating.
    prune_stale_labelled_resources()

    created_container = False
    created_volume = False
    try:
        _docker("volume", "create", *labels, volume_name)
        created_volume = True

        # Prime the fresh volume's ownership to PROBE_UID_GID before the hardened, non-root
        # container ever touches it. A brand-new named volume mounted at a path `alpine:latest`
        # does not already declare comes out root:root -- there is nothing in the image at
        # `WRITABLE_MOUNT_DEST` for Docker to propagate ownership from (the mechanism that
        # makes this a non-issue for the REAL plugin image: `services/edge-plugin/Dockerfile`
        # itself creates and `chown`s `/var/lib/edge-plugin` to `edgeplugin` as root, before
        # `VOLUME`, so a fresh volume mounted there inherits that ownership from the image on
        # first use). This one-shot, `--rm`, default-user (root) container reproduces that same
        # priming step for alpine, which declares no such path -- it is scaffolding for this
        # probe's own setup, not part of `HARDENING_RUN_FLAGS` or either assertion function, and
        # it never runs the actual hardening flags itself.
        _docker(
            "run", "--rm", *labels,
            "-v", f"{volume_name}:{WRITABLE_MOUNT_DEST}",
            ALPINE_IMAGE, "chown", PROBE_UID_GID, WRITABLE_MOUNT_DEST,
        )

        # `--network none`: this probe needs no network at all (it only proves filesystem/
        # user/capability/seccomp posture), so there is no reason to give it one -- belt-and-
        # suspenders, not one of the properties `assert_inspect_hardening`/`assert_exec_hardening`
        # themselves check.
        _docker(
            "run", "-d", "--name", container_name, "--network", "none", *labels,
            *HARDENING_RUN_FLAGS,
            "--user", PROBE_UID_GID,
            "-v", f"{volume_name}:{WRITABLE_MOUNT_DEST}",
            ALPINE_IMAGE, "sh", "-c", f"sleep {HOLD_OPEN_SECONDS}",
        )
        created_container = True

        inspect_info = assert_inspect_hardening(container_name, writable_mount_dest=WRITABLE_MOUNT_DEST)

        # Question 148: an exit code is not evidence -- print the actual observed values, not
        # merely that the assertions above passed.
        print(f"\n--- alpine-based hardening proof, from docker inspect (question 148) ---\n"
              f"Config.User={inspect_info['Config']['User']!r} "
              f"HostConfig.ReadonlyRootfs={inspect_info['HostConfig']['ReadonlyRootfs']!r} "
              f"HostConfig.CapDrop={inspect_info['HostConfig'].get('CapDrop')!r} "
              f"HostConfig.SecurityOpt={inspect_info['HostConfig'].get('SecurityOpt')!r}")

        try:
            exec_facts = assert_exec_hardening(container_name, writable_path=WRITABLE_MOUNT_DEST)
        except VolumeWriteDiskExhausted as e:
            # See `VolumeWriteDiskExhausted`'s own docstring: a real, named, host-wide disk
            # condition (this module's own doc has the numbers), not a hardening defect -- every
            # OTHER fact (non-root uid, NoNewPrivs, Seccomp, the read-only-root write correctly
            # failing) already passed above. Question 194: a real environmental blocker is a
            # visible skip, never a bare failure that would misread as a broken implementation,
            # and never a silent pass.
            print(f"\n--- alpine-based hardening proof, from docker exec of the RUNNING container "
                  f"(question 148) -- PARTIAL, before the disk-exhausted volume write ---\n"
                  f"{json.dumps(e.partial_facts)}")
            pytest.skip(
                f"user/NoNewPrivs/Seccomp/read-only-root posture confirmed ({json.dumps(e.partial_facts)}), "
                f"but the final write-to-volume probe could not complete: {e}"
            )

        print(f"\n--- alpine-based hardening proof, from docker exec of the RUNNING container (question 148) ---\n"
              f"{json.dumps(exec_facts)}")
    finally:
        if created_container:
            subprocess.run(["docker", "rm", "-f", container_name], capture_output=True, timeout=30)
        if created_volume:
            subprocess.run(["docker", "volume", "rm", volume_name], capture_output=True, timeout=30)

    # Question 156: nothing labelled with this run's id remains, checked only after cleanup has
    # actually run.
    label_filter = f"label=av.test.run_id={run_id}"
    remaining_containers = _docker("ps", "-a", "--filter", label_filter, "-q").stdout.strip()
    assert remaining_containers == "", f"container(s) labelled {label_filter} still exist after cleanup: {remaining_containers!r}"
    remaining_volumes = _docker("volume", "ls", "--filter", label_filter, "-q").stdout.strip()
    assert remaining_volumes == "", f"volume(s) labelled {label_filter} still exist after cleanup: {remaining_volumes!r}"
    # This test creates no image and no network of its own.
    remaining_images = _docker("images", "--filter", label_filter, "-q").stdout.strip()
    assert remaining_images == "", f"no image should ever carry {label_filter} (this test creates none), but found: {remaining_images!r}"
    remaining_networks = _docker("network", "ls", "--filter", label_filter, "-q").stdout.strip()
    assert remaining_networks == "", f"no network should ever carry {label_filter} (this test creates none), but found: {remaining_networks!r}"


def test_TEST_LABEL_KEY_matches_the_convention_this_file_depends_on():
    """A cheap, non-docker-gated guard against the two modules' label constants drifting apart --
    `crates/av-lockstep/src/docker.rs`'s own `TEST_LABEL_KEY`/`TEST_LABEL_VALUE` convention,
    `altavista.container_hardening`'s re-export of it, and `tests/
    test_edge_plugin_container.py`'s own `TEST_LABEL_KEY` constant must all agree."""
    assert TEST_LABEL_KEY == "av.test"

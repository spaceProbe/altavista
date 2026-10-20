"""Question 255: `altavista.container_hardening.prune_stale_labelled_resources` removes a stale
`av.test`-labelled container's anonymous volumes along with the container.

Why: a container's anonymous volumes (the kind a Dockerfile `VOLUME` instruction, or a bare
`docker create -v /path`, makes) carry no label, so the label-filtered volume sweep at the end of
the prune never sees them; only `docker rm -f -v` removes them. Question 255 found 755 dangling
anonymous volumes (34 GB) filling the Colima VM's disk, one cause being `docker rm -f` without `-v`.

The proof: `docker create` (never start) a labelled container from the recorded store image with
`-v /av-prune-probe`, confirm the anonymous volume exists, run the prune, and assert both the
container and that volume are gone. Gated like the other docker-gated tests: a visible skip if the
daemon does not answer, or if the store image from `services/store/IMAGE_DIGEST.md` is not present
locally with its recorded id (this test never pulls). Runs under the host-wide docker-test lock.
"""
from __future__ import annotations

import json
import subprocess
import uuid
from typing import Optional

import pytest

from altavista.container_hardening import label_args, prune_stale_labelled_resources
from altavista.docker_test_lock import lock_docker_tests
from altavista.store_record import STORE_IMAGE_DIGEST_MD, parse_store_record

ANONYMOUS_VOLUME_DEST = "/av-prune-probe"


def _docker(*args: str, timeout: float = 60.0) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)


def _compute_skip_reason() -> Optional[str]:
    try:
        info = _docker("info", timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"Docker not available: docker is not installed or could not be launched: {e}"
    if info.returncode != 0:
        detail = info.stderr.strip().splitlines()
        return f"Docker not available: `docker info` exited {info.returncode}: {detail[-1] if detail else '(no stderr)'}"
    try:
        record = parse_store_record(STORE_IMAGE_DIGEST_MD)
    except (ValueError, OSError) as e:
        return f"cannot read the store record: {e}"
    inspected = _docker("image", "inspect", record.image_ref, "--format", "{{.Id}}", timeout=30)
    if inspected.returncode != 0:
        return f"image {record.image_ref!r} is not present locally (this test never pulls; see {STORE_IMAGE_DIGEST_MD})"
    actual_id = inspected.stdout.strip()
    if actual_id != record.recorded_id:
        return f"image {record.image_ref!r} is present locally but its id {actual_id!r} does not match the recorded {record.recorded_id!r}"
    return None


SKIP_REASON = _compute_skip_reason()


def _volume_exists(name: str) -> bool:
    return _docker("volume", "inspect", name, timeout=30).returncode == 0


def _container_exists(container_id: str) -> bool:
    return _docker("container", "inspect", container_id, timeout=30).returncode == 0


def test_prune_removes_a_stale_containers_anonymous_volume():
    if SKIP_REASON is not None:
        pytest.skip(SKIP_REASON)
    record = parse_store_record(STORE_IMAGE_DIGEST_MD)
    run_id = f"prune-anon-vol-{uuid.uuid4()}"

    with lock_docker_tests():
        container_id: Optional[str] = None
        volume_name: Optional[str] = None
        try:
            created = _docker("create", *label_args(run_id), "-v", ANONYMOUS_VOLUME_DEST, record.image_ref)
            assert created.returncode == 0, f"docker create failed (rc={created.returncode}): {created.stderr}"
            container_id = created.stdout.strip()

            mounts_result = _docker("inspect", "--format", "{{json .Mounts}}", container_id, timeout=30)
            assert mounts_result.returncode == 0, f"docker inspect failed: {mounts_result.stderr}"
            mounts = json.loads(mounts_result.stdout)
            matching = [m for m in mounts if m.get("Destination") == ANONYMOUS_VOLUME_DEST]
            assert matching, f"expected a mount at {ANONYMOUS_VOLUME_DEST}, found .Mounts={mounts!r}"
            assert matching[0].get("Type") == "volume", f"expected a volume mount, got {matching[0]!r}"
            volume_name = matching[0]["Name"]
            assert volume_name, f"the anonymous volume has no name: {matching[0]!r}"

            assert _volume_exists(volume_name), f"precondition: volume {volume_name} must exist before the prune"

            prune_stale_labelled_resources()

            assert not _container_exists(container_id), f"the stale labelled container {container_id} must be gone after the prune"
            assert not _volume_exists(volume_name), (
                f"the stale container's anonymous volume {volume_name} must be gone after the prune "
                f"(question 255: `docker rm -f` without `-v` leaves it behind)"
            )
        finally:
            if container_id is not None and _container_exists(container_id):
                _docker("rm", "-f", "-v", container_id, timeout=30)
            if volume_name is not None and _volume_exists(volume_name):
                _docker("volume", "rm", volume_name, timeout=30)

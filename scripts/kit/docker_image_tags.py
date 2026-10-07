"""scripts/kit/docker_image_tags.py -- `docker load` that never leaves a repository tag behind
that its caller did not own (question 234, applied to every caller by question 239).

# The defect this fixes

`docker load -i <tarball>` restores the tarball's OWN `repository:tag` on the host, host-wide and
unconditionally. A kit tarball built on an earlier day therefore moves `av-edge-plugin:local` and
`altavista-cfs-lockstep:local` onto its own, older images, while the images other tracks pinned
later stay present but untagged -- which silently defeats question 212(a)'s digest gates for
those tracks (the plugin tag moved this way three times). Commit 69de194 fixed that in
`tests/test_kit_zero_egress_install.py`; `scripts/kit/live_evidence.py::_load_and_verify_image`
had the same `docker load` and none of the fix. This module is that fix, once, for both.

# Where it lives, and why

`scripts/kit/`, not `tests/` and not `altavista/`. `scripts/kit/live_evidence.py` is production
tooling and must not import a test module, and the kit test already puts this directory on
`sys.path` to import its siblings (`build_kit`, `manifest`, `sbom`). `altavista/` would also work
for imports, but everything under it ships in the `altavista` wheel the kit pins, so a docker
hygiene helper there would change the wheel's contents (and its hash and SBOM) for no runtime
reason.

# The pattern (69de194), as one object

`ImageTagGuard` takes a `docker image ls` snapshot of every real `repository:tag -> id` binding
when it is constructed (before the caller's first load). Each `load_verified` call then:

1. snapshots again immediately before and after the load;
2. verifies the loaded image BY ID (`docker image inspect <id>`), never by re-reading the tag,
   against the digest the caller's manifest records;
3. when the caller still needs the image afterward, re-tags it first under a run-scoped,
   test-only name (`<prefix>/<component>:<run_id>`), which the caller uses instead of the
   `:local` tag;
4. undoes every binding the load created or moved, **in a `finally`, so the failure paths
   (load failed, tag missing, digest mismatch) restore too**: a binding the load created is
   removed with `docker rmi <repository:tag>` (untag only: never a bare id, never `-f`, because
   `docker rmi -f <ID>` strips every tag on that id, measured and recorded in
   `crates/av-kernel/tests/drm_attitude_control_cfs.rs`), and a binding the load moved is put
   back with `docker tag <old_id> <repository:tag>`.

`assert_host_tags_unchanged` is the closing proof, read directly off the host and meant to be
called from the caller's outermost `finally`. It refuses a tag that moved, a test-only tag that
survived `remove_test_only_tags`, and a `repository:tag` that did not exist before and does now.

The caller holds `altavista.docker_test_lock.lock_docker_tests()` for the whole span (the "new
tag appeared" check relies on no other lock-taking docker test running inside it); this module
takes no lock of its own.
"""
from __future__ import annotations

import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional


class ImageHygieneError(AssertionError):
    """A load, a verification or a restore failed, or the host's tags are not as found. It is an
    `AssertionError` so that a pytest caller reports it as a failed assertion."""


def _docker(*args: str, timeout: float = 60.0) -> subprocess.CompletedProcess:
    result = subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise ImageHygieneError(
            f"`docker {' '.join(args)}` failed (rc={result.returncode}):\n"
            f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
        )
    return result


def docker_tag_snapshot() -> dict:
    """`repository:tag -> full image id` (`sha256:...`) for every REAL tag on this host right now:
    one `docker image ls --format` call, never free-text `docker images` parsing. `<none>:...`
    entries (untagged and dangling images) are excluded: they carry no `repository:tag` binding a
    `docker load` could clobber. `docker image ls --format '{{.ID}}'` prints a bare hex id with no
    `sha256:` prefix even with `--no-trunc`; every id here is normalised to the `sha256:...` form
    `docker image inspect --format '{{.Id}}'` returns, so callers can compare the two directly."""
    result = _docker("image", "ls", "--no-trunc", "--format", "{{.Repository}}:{{.Tag}}\t{{.ID}}")
    snapshot: dict = {}
    for line in result.stdout.splitlines():
        if not line.strip():
            continue
        repo_tag, image_id = line.split("\t")
        if repo_tag.startswith("<none>:"):
            continue
        if not image_id.startswith("sha256:"):
            image_id = f"sha256:{image_id}"
        snapshot[repo_tag] = image_id
    return snapshot


@dataclass
class LoadedImage:
    """What `ImageTagGuard.load_verified` returns for one component."""

    #: The four keys the kit test and `live_evidence.py` have always recorded as image
    #: provenance: `tag`, `recorded_digest`, `loaded_digest`, `docker_load_stdout`.
    evidence: dict
    #: The run-scoped test-only tag the caller must run containers from, or `None` when the
    #: caller did not ask to keep the image (`keep_as_test_tag=False`).
    test_tag: Optional[str]
    #: `repository:tag -> (id before the load or None, id after the load)`, for the bindings the
    #: load created or moved. All of them were undone before `load_verified` returned.
    changed: dict = field(default_factory=dict)


class ImageTagGuard:
    def __init__(
        self,
        run_id: str,
        test_tag_prefix: str,
        on_test_tag: Optional[Callable[[str], None]] = None,
    ) -> None:
        """Takes the initial tag snapshot now. Construct it before the first load, outside the
        caller's `try`, so it is defined even when the first load raises. `on_test_tag(tag)` is
        called for each test-only tag created, for a caller (the kit test's `ResourceGuard`) that
        removes its resources itself; otherwise call `remove_test_only_tags()`."""
        self.run_id = run_id
        self.test_tag_prefix = test_tag_prefix
        self.on_test_tag = on_test_tag
        self.initial_snapshot = docker_tag_snapshot()
        self.test_only_tags: list = []

    def test_only_tag(self, component: str) -> str:
        """A name no other track's own image tag can collide with."""
        return f"{self.test_tag_prefix}/{component}:{self.run_id}"

    def load_verified(
        self,
        *,
        component: str,
        tarball: Path,
        tag: str,
        recorded_digest: str,
        keep_as_test_tag: bool = False,
    ) -> LoadedImage:
        """`docker load -i <tarball>`, verify the loaded image BY ID against `recorded_digest`,
        optionally re-tag it under a test-only name, and undo every tag binding the load created or
        moved, whatever happens in between. `tag` is the `repository:tag` the tarball carries
        (the manifest's); it is used only to find the loaded id in the after-load snapshot."""
        before_load = docker_tag_snapshot()
        changed: dict = {}
        test_tag: Optional[str] = None
        try:
            load = subprocess.run(
                ["docker", "load", "-i", str(tarball)], capture_output=True, text=True, timeout=120
            )
            # Whatever the load did to the host's tags, it is read here, even after a failed load.
            after_load = docker_tag_snapshot()
            changed = {
                repo_tag: (before_load.get(repo_tag), new_id)
                for repo_tag, new_id in after_load.items()
                if before_load.get(repo_tag) != new_id
            }
            if load.returncode != 0:
                raise ImageHygieneError(f"docker load -i {tarball} failed (rc={load.returncode}): {load.stderr}")

            loaded_id = after_load.get(tag)
            if loaded_id is None:
                raise ImageHygieneError(
                    f"{component}: docker load -i {tarball} succeeded but {tag!r} is not bound "
                    f"to any image afterward (tags currently on the host: {sorted(after_load)!r})"
                )
            # Verify by id, never by re-querying the tag a second time (question 234: the tag is
            # exactly the thing this module stops trusting) -- a self-inspect by id both confirms
            # the id genuinely exists and gives the value compared to the manifest below.
            verify = subprocess.run(
                ["docker", "image", "inspect", loaded_id, "--format", "{{.Id}}"],
                capture_output=True, text=True, timeout=30,
            )
            if verify.returncode != 0:
                raise ImageHygieneError(
                    f"{component}: docker image inspect {loaded_id} failed right after loading: {verify.stderr}"
                )
            actual_digest = verify.stdout.strip()
            evidence = {
                "tag": tag,
                "recorded_digest": recorded_digest,
                "loaded_digest": actual_digest,
                "docker_load_stdout": load.stdout.strip(),
            }
            if actual_digest != recorded_digest:
                raise ImageHygieneError(
                    f"{component}: KIT_MANIFEST records {recorded_digest}, but the image loaded "
                    f"from {tarball} has id {actual_digest} -- refusing to trust it (question 212)"
                )

            if keep_as_test_tag:
                # Re-tag FIRST -- before any untag/restore below runs -- so the image the caller
                # still needs is never left with only the about-to-be-undone host tag as its
                # sole reference.
                test_tag = self.test_only_tag(component)
                _docker("tag", loaded_id, test_tag)
                self.test_only_tags.append(test_tag)
                if self.on_test_tag is not None:
                    self.on_test_tag(test_tag)
        finally:
            problems = self._undo(component, changed, kept=test_tag is not None)
            if problems:
                raise ImageHygieneError(
                    f"{component}: could not undo the tag binding(s) docker load -i {tarball} "
                    f"created or moved --\n" + "\n".join(problems)
                )
        return LoadedImage(evidence=evidence, test_tag=test_tag, changed=changed)

    def _undo(self, component: str, changed: dict, *, kept: bool) -> list:
        problems: list = []
        for repo_tag, (old_id, new_id) in changed.items():
            try:
                if old_id is None:
                    # The load created this binding from nothing: untag only.
                    _docker("rmi", repo_tag)
                    if kept:
                        # Safe because the test-only re-tag already added a second reference
                        # first; prove the image the caller still needs survived the untag.
                        still_present = subprocess.run(
                            ["docker", "image", "inspect", new_id, "--format", "{{.Id}}"],
                            capture_output=True, text=True, timeout=30,
                        )
                        if still_present.returncode != 0:
                            problems.append(
                                f"{component}: untagging {repo_tag!r} (a binding this load "
                                f"created) left image {new_id} with no reference at all -- it "
                                f"should still exist under this run's own test-only tag"
                            )
                else:
                    # The load MOVED an existing tag off `old_id`: point it back.
                    _docker("tag", old_id, repo_tag)
            except ImageHygieneError as e:
                problems.append(f"{repo_tag}: {e}")
        return problems

    def remove_test_only_tags(self) -> None:
        """Removes every test-only tag this guard created, by name (untag only). For a caller that
        does not register `on_test_tag` and clean up itself."""
        for tag in self.test_only_tags:
            subprocess.run(["docker", "rmi", tag], capture_output=True, timeout=30)

    def assert_host_tags_unchanged(self) -> None:
        """The closing proof, checked directly against the host's real state, never assumed from
        having undone each load along the way. Call it from the outermost `finally` so it runs on
        the failure path too: a run that moved tags and then failed for an unrelated reason is
        exactly the case the lead hit. It refuses three shapes: a tag that moved, a test-only tag
        that survived cleanup, and a `repository:tag` that did not exist before this guard was
        built and does now (what an undo that silently failed to untag would leave behind; it
        sits in neither the initial snapshot nor `test_only_tags`). The caller holds
        `lock_docker_tests()` for the whole span, so no other lock-taking docker test can have
        created a tag inside this window: a new tag here is this run's own leak or a process that
        took no lock, and both are worth seeing."""
        final_snapshot = docker_tag_snapshot()
        problems = []
        for repo_tag, before_id in self.initial_snapshot.items():
            after_id = final_snapshot.get(repo_tag)
            if after_id != before_id:
                problems.append(f"{repo_tag}: was {before_id} before this run, is {after_id!r} now")
        for test_tag in self.test_only_tags:
            if test_tag in final_snapshot:
                problems.append(f"{test_tag}: this run's own test-only tag still exists -- cleanup did not remove it")
        for repo_tag in sorted(set(final_snapshot) - set(self.initial_snapshot) - set(self.test_only_tags)):
            problems.append(
                f"{repo_tag}: did not exist before this run and points at {final_snapshot[repo_tag]} now "
                f"-- a repository tag this run created and did not remove"
            )
        if problems:
            raise ImageHygieneError(
                "docker tag hygiene violation (question 199/234: a run that loads an image must "
                "never leave a repository tag behind that it did not own) --\n" + "\n".join(problems)
            )

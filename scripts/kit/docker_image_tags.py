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

# `docker rmi <repo:tag>` is not "untag only" (heavy-ct-5, measured on this host's store)

This host's engine uses the containerd image store (`docker info`: `driver-type
io.containerd.snapshotter.v1`). There `docker rmi <repo:tag>` deletes the image itself ("Untagged:"
then "Deleted: sha256:...") unless some OTHER reference survives (moby `daemon/containerd/image_delete.go`: `ImageDelete`
untags only when `len(sameRef) != len(all)`). Measured on throwaway images, and re-proved by
`tests/test_docker_image_tags.py`:

- The last name of an image: deleted. Another tag in any repository, or in the same repository
  under another tag: only untagged.
- A reference `R@sha256:...` from a pull by digest keeps the image alive when the tag being
  removed is in ANOTHER repository, but NOT when it is in the SAME repository `R`: the removal
  prints "Untagged: R:t", "Untagged: R@sha256:...", "Deleted: sha256:..." (containerd's
  `getSameReferences` treats the untagged digest reference of the same name as the same reference).
- An image known only by its id (dangling, `<none>:<none>` in `docker image ls`; a tag that
  moved off a pinned image leaves it this way) is invisible to a `repository:tag` snapshot, and the
  dangling placeholder is always bundled into the removal ("One is dangling" in `getSameReferences`),
  so `docker load` of a tarball that tags it and then `docker rmi <that tag>` deletes the pinned
  image. A stopped container using the image makes the removal fail with a conflict instead.

The pre-heavy-ct-5 `_undo` ran `docker rmi <repo:tag>` on every binding a load "created from
nothing" on the assumption that it only untags, and `assert_host_tags_unchanged` compared tags
only, so the deletion went unnoticed (reproduced on throwaway images: the image was gone and the
closing assertion passed). Guarantees now:

1. Before the load, the guard reads the whole image table (`docker image ls`, dangling rows and
   digest-reference rows included, `docker_image_table`). If the tarball's image (read from its
   `index.json` / `manifest.json`, best effort) is already on the host and none of its existing
   references would survive removing the names the load would add (`_survives_untag`), the load is
   SKIPPED: the image is verified by id as before and handed back by id, so nothing is added that
   could later have to be removed. (A tarball that carries several such images is refused.)
2. If the load runs anyway and ends up adding a name to such an image (the tarball could not be
   read), `_undo` REFUSES to `docker rmi` it and raises, naming the image and the tag left in place.
3. A test-only tag is never put on such an image either: its `LoadedImage.test_tag` is the image
   id (a valid image reference for `docker run`), which is never removed.
4. `assert_host_tags_unchanged` also fails when any image id present when the guard was built is
   missing at the end.
"""
from __future__ import annotations

import json
import subprocess
import tarfile
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
    return _tags_of(docker_image_table())


def docker_image_table(run: Optional[Callable[[list], str]] = None) -> dict:
    """`image id -> [(repository or None, tag or None), ...]` for EVERY image on this host, one row
    per reference `docker image ls` prints (one `--format` call). A real tag is `(repo, tag)`; a
    reference by digest only (`repo@sha256:...`, from a pull by digest) prints as `(repo, None)`; an
    image with no name at all (dangling, known only by its id) is `(None, None)`. Unlike the tag
    snapshot, nothing is dropped, so an image no `repository:tag` names is still seen.

    `run(args) -> stdout text` (raising on a non-zero exit) replaces the real `docker` call; the
    kit's bundled installer (`image_build.py`, question 250(a)) passes its own injectable runner so
    its refusals can be tested without Docker."""
    table: dict = {}
    runner = run or (lambda args: _docker(*args).stdout)
    # A plain `docker image ls` hides the nameless images on the containerd store (measured: 0
    # `<none>:<none>` rows against 20 under `--filter dangling=true` on this host), so they are
    # listed separately. `-a` is not used: it adds the manifests and children of every image.
    for extra in ((), ("--filter", "dangling=true")):
        stdout = runner(["image", "ls", "--no-trunc", *extra, "--format", "{{.Repository}}\t{{.Tag}}\t{{.ID}}"])
        for line in stdout.splitlines():
            if not line.strip():
                continue
            repo, tag, image_id = line.split("\t")
            if not image_id.startswith("sha256:"):
                image_id = f"sha256:{image_id}"
            row = (None if repo == "<none>" else repo, None if tag == "<none>" else tag)
            if row not in table.setdefault(image_id, []):
                table[image_id].append(row)
    return table


def _tags_of(table: dict) -> dict:
    """The `repository:tag -> id` view of `docker_image_table()` (`<none>:...` rows excluded)."""
    return {
        f"{repo}:{tag if tag is not None else '<none>'}": image_id
        for image_id, rows in table.items()
        for repo, tag in rows
        if repo is not None
    }


def _repository_of(name: str) -> str:
    """`localhost:5000/a/b:t` -> `localhost:5000/a/b`; `a/b@sha256:..` -> `a/b`; `a/b` -> `a/b`."""
    if "@" in name:
        return name.split("@", 1)[0]
    head, _, tail = name.rpartition(":")
    return name if (not head or "/" in tail) else head


def _survives_untag(rows: list, added_names) -> bool:
    """Would the image whose pre-existing references are `rows` still exist after `docker rmi` of
    every name in `added_names` (names added to it on top of `rows`)? Measured on the containerd
    store: a dangling placeholder is removed together with the name, a digest reference of the SAME
    repository is removed together with the tag, and anything else survives -- any tag (other
    repository, or the same repository under another tag) and a digest reference of another
    repository."""
    added_repositories = {_repository_of(n) for n in added_names}
    for repo, tag in rows:
        if repo is None:
            continue
        if tag is not None or repo not in added_repositories:
            return True
    return False


def _tarball_identity(tarball: Path) -> tuple:
    """`(image ids, repo:tag names)` the tarball carries, read from its `index.json` (containerd
    store: the manifest digests) and `manifest.json` (config digests, `RepoTags`). Best effort: an
    unreadable tarball gives `(set(), set())` and `docker load` itself is then the judge."""
    ids: set = set()
    tags: set = set()
    try:
        with tarfile.open(tarball) as tf:
            members = set(tf.getnames())
            if "index.json" in members:
                for desc in json.load(tf.extractfile("index.json")).get("manifests", []):
                    ids.add(desc["digest"])
            if "manifest.json" in members:
                for entry in json.load(tf.extractfile("manifest.json")):
                    tags.update(entry.get("RepoTags") or [])
                    config = entry.get("Config", "")
                    stem = config.rsplit("/", 1)[-1].removesuffix(".json")
                    if stem:
                        ids.add(f"sha256:{stem}")
    except (OSError, tarfile.TarError, ValueError, KeyError, TypeError, AttributeError):
        return set(), set()
    return ids, tags


@dataclass
class LoadedImage:
    """What `ImageTagGuard.load_verified` returns for one component."""

    #: The four keys the kit test and `live_evidence.py` have always recorded as image
    #: provenance: `tag`, `recorded_digest`, `loaded_digest`, `docker_load_stdout`.
    evidence: dict
    #: The image reference the caller must run containers from, or `None` when the caller did not
    #: ask to keep the image (`keep_as_test_tag=False`): the run-scoped test-only tag, or the
    #: image id (`sha256:...`, no tag, nothing to remove) when the image was on the host before the
    #: load and no other reference would outlive removing a tag from it.
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
        initial_table = docker_image_table()
        self.initial_snapshot = _tags_of(initial_table)
        #: `image id -> references` of every image on the host when the guard was built; the
        #: closing proof fails if any of them is gone at the end.
        self.initial_images = initial_table
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
        before_table = docker_image_table()
        before_load = _tags_of(before_table)
        changed: dict = {}
        unsafe: dict = {}
        test_tag: Optional[str] = None
        try:
            skipped_id = self._fragile_preexisting_image(component, tarball, before_table)
            if skipped_id is not None:
                # The image is on the host and the names the tarball would add are the only thing
                # between it and deletion when they are removed again: do not add them.
                load_stdout = (
                    f"docker load skipped: image {skipped_id} is already on this host and no reference "
                    f"of it would outlive an untag of the tarball's names (heavy-ct-5)"
                )
                loaded_id = skipped_id
                after_load = before_load
            else:
                load = subprocess.run(
                    ["docker", "load", "-i", str(tarball)], capture_output=True, text=True, timeout=120
                )
                load_stdout = load.stdout.strip()
                # Whatever the load did to the host's tags, it is read here, even after a failed load.
                after_table = docker_image_table()
                after_load = _tags_of(after_table)
                changed = {
                    repo_tag: (before_load.get(repo_tag), new_id)
                    for repo_tag, new_id in after_load.items()
                    if before_load.get(repo_tag) != new_id
                }
                unsafe = self._untags_that_would_delete(changed, before_table)
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
                "docker_load_stdout": load_stdout,
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
                wanted = self.test_only_tag(component)
                if loaded_id in before_table and not _survives_untag(before_table[loaded_id], {wanted}):
                    # On the host before this load, with nothing but a dangling placeholder or a
                    # same-repository digest reference: a tag added here could only be removed by
                    # deleting the image. Hand back the id; there is nothing to remove later.
                    test_tag = loaded_id
                else:
                    test_tag = wanted
                    _docker("tag", loaded_id, test_tag)
                    self.test_only_tags.append(test_tag)
                    if self.on_test_tag is not None:
                        self.on_test_tag(test_tag)
        finally:
            problems = self._undo(component, changed, unsafe, kept=test_tag is not None)
            if problems:
                raise ImageHygieneError(
                    f"{component}: could not undo the tag binding(s) docker load -i {tarball} "
                    f"created or moved --\n" + "\n".join(problems)
                )
        return LoadedImage(evidence=evidence, test_tag=test_tag, changed=changed)

    def load_base_verified(self, *, component: str, tarball: Path, expected_id: str) -> dict:
        """Question 250(a): the operator-supplied BASE image from a tarball saved BY DIGEST
        (`scripts/kit/save_base_images.py`: `docker save <name>@sha256:...`, which carries no
        `RepoTags`; measured: `manifest.json` has `"RepoTags": null`, `index.json` names the digest).

        1. The tarball's own `index.json`/`manifest.json` must name `expected_id` (the pin), and
           carry no repository names at all: a name would be restored on the host by the load, and
           removing it again could delete the base (this module's heavy-ct-5 section), so such a
           tarball is refused rather than loaded.
        2. If `expected_id` is already on the host, the load is SKIPPED, not run: measured on this
           host, `docker load` of a tarball whose image is present adds a nameless (dangling) reference
           to that image, a mutation of a pinned image for no gain. The present image is verified by
           id exactly as a loaded one would be.
        3. Otherwise `docker load -i` runs, and the image it produced is verified BY ID.

        Never tags, never removes. Returns the evidence dict (what was read, what was done); raises
        `ImageHygieneError` on any disagreement. The caller holds the docker lock."""
        ids, names = _tarball_identity(tarball)
        if expected_id not in ids:
            raise ImageHygieneError(
                f"{component}: {tarball} names image(s) {sorted(ids)!r}, not the pinned {expected_id} -- "
                f"refusing to load a tarball that is not the pinned base"
            )
        if names:
            raise ImageHygieneError(
                f"{component}: {tarball} carries repository name(s) {sorted(names)!r}; a base tarball must be "
                f"saved by digest (scripts/kit/save_base_images.py) so that loading it names nothing on the host"
            )
        before_table = docker_image_table()
        before_load = _tags_of(before_table)
        if expected_id in before_table:
            action = "skipped: the pinned image is already on this host (a load would only add a nameless reference to it)"
        else:
            load = subprocess.run(["docker", "load", "-i", str(tarball)], capture_output=True, text=True, timeout=300)
            if load.returncode != 0:
                raise ImageHygieneError(f"{component}: docker load -i {tarball} failed (rc={load.returncode}): {load.stderr}")
            action = load.stdout.strip()
        after_load = _tags_of(docker_image_table())
        if after_load != before_load:
            moved = {t: (before_load.get(t), after_load.get(t)) for t in set(before_load) | set(after_load) if before_load.get(t) != after_load.get(t)}
            raise ImageHygieneError(f"{component}: loading {tarball} changed repository tag(s) on this host: {moved!r}")
        verify = subprocess.run(
            ["docker", "image", "inspect", expected_id, "--format", "{{.Id}}"], capture_output=True, text=True, timeout=30,
        )
        actual = verify.stdout.strip()
        if verify.returncode != 0 or actual != expected_id:
            raise ImageHygieneError(
                f"{component}: after the load, `docker image inspect {expected_id}` gave "
                f"{actual or verify.stderr.strip()!r}, not the pinned id"
            )
        return {
            "component": component, "tarball": str(tarball), "tarball_image_ids": sorted(ids),
            "expected_id": expected_id, "host_id": actual, "present_before": expected_id in before_table,
            "docker_load": action,
        }

    def _fragile_preexisting_image(self, component: str, tarball: Path, before_table: dict) -> Optional[str]:
        """The id of the image `tarball` carries when that image is ALREADY on the host and no
        reference of it would survive removing the tarball's names (so loading would add names that
        could only be removed by deleting the image); `None` when the load is safe to run."""
        ids, names = _tarball_identity(tarball)
        fragile = []
        for image_id in sorted(ids & set(before_table)):
            rows = before_table[image_id]
            added = {n for n in names if (_repository_of(n), n.rpartition(":")[2]) not in rows}
            if added and not _survives_untag(rows, added):
                fragile.append(image_id)
        if len(fragile) > 1:
            raise ImageHygieneError(
                f"{component}: {tarball} carries several images already on the host that a "
                f"`docker rmi` of the loaded names would delete ({fragile!r}); refusing to load it"
            )
        return fragile[0] if fragile else None

    def _untags_that_would_delete(self, changed: dict, before_table: dict) -> dict:
        """`repo:tag -> why` for each binding a load created on an image that was on the host before
        it, where removing the loaded names would delete the image."""
        unsafe: dict = {}
        for repo_tag, (old_id, new_id) in changed.items():
            if old_id is not None or new_id not in before_table:
                continue
            added = {rt for rt, (o, n) in changed.items() if o is None and n == new_id}
            if not _survives_untag(before_table[new_id], added):
                unsafe[repo_tag] = (
                    f"image {new_id} was on the host before this load with references "
                    f"{before_table[new_id]!r}; none would outlive removing {sorted(added)!r}"
                )
        return unsafe

    def _undo(self, component: str, changed: dict, unsafe: dict, *, kept: bool) -> list:
        problems: list = []
        for repo_tag, (old_id, new_id) in changed.items():
            try:
                if repo_tag in unsafe:
                    # Never delete an image that existed before the load (heavy-ct-5): leave the
                    # name, say so, and let a person decide.
                    problems.append(
                        f"{repo_tag}: NOT removed -- {unsafe[repo_tag]}. `docker rmi` would delete "
                        f"the image (containerd store). The extra name is left in place."
                    )
                elif old_id is None:
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
        final_table = docker_image_table()
        final_snapshot = _tags_of(final_table)
        problems = []
        for image_id, rows in self.initial_images.items():
            if image_id not in final_table:
                problems.append(
                    f"{image_id}: an image present before this run (references {rows!r}) no longer "
                    f"exists -- this run deleted it, or something concurrent did (a prune, kubelet "
                    f"image GC); check the image-event log before trusting either"
                )
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

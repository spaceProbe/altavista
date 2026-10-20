"""heavy-ct-5: `scripts/kit/docker_image_tags.py::ImageTagGuard` never deletes an image it did not
load (question 245).

# The hazard these tests pin

On this host's containerd image store `docker rmi <repo:tag>` is not "untag only": it deletes the
image when the removed name was the last reference that counts. A dangling placeholder (an image
known only by its id) is bundled into the removal, and so is a digest reference of the SAME
repository (`R@sha256:...` beside `R:t`). The guard used to run `docker rmi` on every binding a
`docker load` created "from nothing", so a pinned image known only by its id, or only by a digest
reference of the tarball's own repository, vanished when a kit tarball naming it was loaded and undone,
and `assert_host_tags_unchanged` (tags only) passed.

# What is proved, on throwaway images only

Every image here is made by `docker import` of a one-file tar under a run-scoped repository
`av-hygiene-t5-<run>/...`, labelled `av.test=1` and `av.test.run_id=<run>`; the digest-only case
runs a registry container from the already-present `registry:2` image (never pulled; the test
skips visibly without it) with a tmpfs and a random loopback port, and pulls BY DIGEST from it.
Nothing that this file did not create is touched. Cleanup removes by name and then by id, only ids
carrying this run's label, and the fixture asserts nothing labelled is left.

The docker tests take `altavista.docker_test_lock.lock_docker_tests()` themselves; never wrap this
file in `with_docker_lock.py` (a parent holding the flock would deadlock it, question 240).
"""
from __future__ import annotations

import io
import json
import subprocess
import sys
import tarfile
import time
import urllib.request
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
if str(KIT_DIR) not in sys.path:
    sys.path.insert(0, str(KIT_DIR))
import docker_image_tags  # noqa: E402
from docker_image_tags import ImageHygieneError, ImageTagGuard, docker_image_table, docker_tag_snapshot  # noqa: E402


def _docker_unavailable_reason() -> "str | None":
    try:
        result = subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    except (FileNotFoundError, OSError) as e:
        return f"docker is not installed or could not be launched: {e}"
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip().splitlines()
        return f"`docker info` exited {result.returncode}: {detail[-1] if detail else '(no stderr)'}"
    return None


_DOCKER_SKIP_REASON = _docker_unavailable_reason()
requires_docker = pytest.mark.skipif(_DOCKER_SKIP_REASON is not None, reason=_DOCKER_SKIP_REASON or "")


def _registry_skip_reason() -> "str | None":
    if _DOCKER_SKIP_REASON is not None:
        return _DOCKER_SKIP_REASON
    done = subprocess.run(["docker", "image", "inspect", "registry:2"], capture_output=True, timeout=30)
    if done.returncode != 0:
        return "the image `registry:2` is not on this host (the digest-only case needs a local registry; this test never pulls)"
    return None


_REGISTRY_SKIP_REASON = _registry_skip_reason()
requires_registry = pytest.mark.skipif(_REGISTRY_SKIP_REASON is not None, reason=_REGISTRY_SKIP_REASON or "")


def _run(*args: str, check: bool = True, timeout: float = 120.0, data: "bytes | None" = None) -> subprocess.CompletedProcess:
    done = subprocess.run(["docker", *args], capture_output=True, input=data, timeout=timeout)
    if check and done.returncode != 0:
        raise AssertionError(f"docker {' '.join(args)} rc={done.returncode}: {done.stderr.decode(errors='replace')}")
    return done


def _out(*args: str, **kw) -> str:
    return _run(*args, **kw).stdout.decode().strip()


class _Scratch:
    """Throwaway images and (for the digest-only case) one registry container, all labelled."""

    def __init__(self) -> None:
        self.run_id = uuid.uuid4().hex[:12]
        self.repo = f"av-hygiene-t5-{self.run_id}"
        self.dir: "Path | None" = None
        self.registry_name: "str | None" = None
        self.registry_endpoint: "str | None" = None

    def make(self, name: str) -> "tuple[str, str]":
        """A fresh, unique image tagged `<repo>/<name>:1`; returns `(ref, full id)`."""
        ref = f"{self.repo}/{name}:1"
        payload = f"{self.run_id}:{name}".encode()
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w") as tf:
            info = tarfile.TarInfo(name="marker")
            info.size = len(payload)
            tf.addfile(info, io.BytesIO(payload))
        _run("import", "--change", "LABEL av.test=1", "--change", f"LABEL av.test.run_id={self.run_id}",
             "-", ref, data=buf.getvalue())
        return ref, self.id_of(ref)

    @staticmethod
    def id_of(ref: str) -> str:
        return _out("image", "inspect", ref, "--format", "{{.Id}}")

    @staticmethod
    def exists(ref: str) -> bool:
        return _run("image", "inspect", ref, check=False).returncode == 0

    @staticmethod
    def names_of(image_id: str) -> list:
        done = _run("image", "inspect", image_id, "--format", "{{json .RepoTags}}", check=False)
        return sorted(json.loads(done.stdout)) if done.returncode == 0 else []

    def save(self, ref: str, name: str) -> Path:
        path = self.dir / f"{name}.tar"
        _run("save", "-o", str(path), ref)
        return path

    def make_dangling(self, name: str) -> "tuple[str, str, Path]":
        """An image A known ONLY by its id, plus a tarball that names it `<repo>/<name>:t`.
        A is tagged `t`, saved, then `t` is moved onto another image B (which keeps its own name),
        which leaves A with no name at all. Returns `(t, A's id, tarball)`."""
        t = f"{self.repo}/{name}:t"
        a_ref, a_id = self.make(f"{name}-a")
        _run("tag", a_id, t)
        tarball = self.save(t, name)
        _run("rmi", a_ref)                                  # A keeps only `t`
        _, b_id = self.make(f"{name}-b")
        _run("tag", b_id, t)                                # `t` moves to B: A is now nameless
        _run("rmi", t)                                      # B keeps its own name; `t` is gone
        assert docker_image_table()[a_id] == [(None, None)], docker_image_table()[a_id]
        return t, a_id, tarball

    def start_registry(self) -> str:
        self.registry_name = f"av-hygiene-t5-registry-{self.run_id}"
        _run("run", "-d", "--rm", "--name", self.registry_name, "--label", "av.test=1",
             "--label", f"av.test.run_id={self.run_id}", "-p", "127.0.0.1::5000", "--tmpfs", "/var/lib/registry", "registry:2")
        mapping = _out("port", self.registry_name, "5000/tcp").splitlines()[0]
        self.registry_endpoint = f"localhost:{mapping.rsplit(':', 1)[1]}"
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                if urllib.request.urlopen(f"http://127.0.0.1:{mapping.rsplit(':', 1)[1]}/v2/", timeout=2).status == 200:
                    return self.registry_endpoint
            except OSError:
                time.sleep(0.5)
        raise AssertionError("the scratch registry did not come up")

    def make_digest_only(self, name: str) -> "tuple[str, str, Path]":
        """An image whose ONLY reference is `R@sha256:...`, pulled by digest from the scratch
        registry, where `R = <registry>/<repo>/<name>`; plus a tarball that names the same image
        `R:t` (the SAME repository). Returns `(R:t, the image id, tarball)`."""
        r = f"{self.start_registry()}/{self.repo}/{name}"
        ref, image_id = self.make(name)
        _run("tag", image_id, f"{r}:t")
        tarball = self.save(f"{r}:t", name)
        pushed = _out("push", f"{r}:t")
        digest = [l for l in pushed.splitlines() if "digest:" in l][-1].split("digest:")[1].split()[0]
        _run("rmi", f"{r}:t")
        _run("rmi", ref)
        assert not self.exists(image_id), "the image must be gone locally before it is pulled back by digest"
        _run("pull", f"{r}@{digest}")
        assert self.id_of(f"{r}@{digest}") == image_id
        assert docker_image_table()[image_id] == [(r, None)], docker_image_table()[image_id]
        return f"{r}:t", image_id, tarball

    def labelled_ids(self) -> list:
        """Every image id carrying this run's label, nameless ones included (a plain `docker image
        ls` hides them on the containerd store)."""
        found: set = set()
        for extra in ((), ("--filter", "dangling=true")):
            out = _out("image", "ls", "--no-trunc", "-q", *extra, "--filter", f"label=av.test.run_id={self.run_id}", check=False)
            found.update(i for i in out.splitlines() if i)
        return sorted(found)

    def cleanup(self) -> None:
        if self.registry_name:
            subprocess.run(["docker", "rm", "-f", "-v", self.registry_name], capture_output=True, timeout=60)
        # By name first (an untag, or the removal of our own last name), then by id for what is left.
        # Only ids that carry THIS run's label are ever removed by id.
        for _ in range(3):
            ids = self.labelled_ids()
            if not ids:
                break
            for image_id in ids:
                for name in self.names_of(image_id):
                    subprocess.run(["docker", "rmi", name], capture_output=True, timeout=60)
                if self.exists(image_id) and self.run_id in _out(
                    "image", "inspect", image_id, "--format", "{{json .Config.Labels}}", check=False
                ):
                    subprocess.run(["docker", "rmi", "-f", image_id], capture_output=True, timeout=60)


@pytest.fixture
def scratch(tmp_path):
    from altavista.docker_test_lock import lock_docker_tests

    s = _Scratch()
    s.dir = tmp_path
    with lock_docker_tests():
        try:
            yield s
        finally:
            s.cleanup()
            leftover = s.labelled_ids()
            names = [t for t in docker_tag_snapshot() if s.repo in t]
            assert leftover == [] and names == [], f"scratch images left behind: {leftover!r} {names!r}"


def _guard(s: _Scratch) -> ImageTagGuard:
    return ImageTagGuard(s.run_id, f"{s.repo}/test-only")


# -------------------------------------------------------------------------------------------------
# Pure logic (no docker)
# -------------------------------------------------------------------------------------------------

def test_survives_untag_encodes_the_measured_containerd_rules():
    survives = docker_image_tags._survives_untag
    assert not survives([(None, None)], {"a/b:t"}), "a dangling placeholder goes with the name"
    assert not survives([("a/b", None)], {"a/b:t"}), "a digest reference of the SAME repository goes with the tag"
    assert survives([("c/d", None)], {"a/b:t"}), "a digest reference of another repository survives"
    assert survives([("a/b", "other")], {"a/b:t"}), "another tag survives, even in the same repository"
    assert survives([(None, None), ("x/y", "1")], {"a/b:t"})
    assert not survives([], {"a/b:t"})
    assert docker_image_tags._repository_of("localhost:5000/a/b:t") == "localhost:5000/a/b"
    assert docker_image_tags._repository_of("localhost:5000/a/b@sha256:ab") == "localhost:5000/a/b"
    assert docker_image_tags._repository_of("localhost:5000/a/b") == "localhost:5000/a/b"


# -------------------------------------------------------------------------------------------------
# A pre-existing image survives a load and undo of a tarball that tags it
# -------------------------------------------------------------------------------------------------

@requires_docker
@pytest.mark.parametrize("keep_as_test_tag", [False, True])
def test_an_image_known_only_by_its_id_survives_a_load_of_a_tarball_that_tags_it(scratch, keep_as_test_tag):
    t, a_id, tarball = scratch.make_dangling("idonly")
    guard = _guard(scratch)
    assert a_id in guard.initial_images, "the guard must SEE an image no repository:tag names"

    loaded = guard.load_verified(component="idonly", tarball=tarball, tag=t, recorded_digest=a_id,
                                 keep_as_test_tag=keep_as_test_tag)

    assert scratch.exists(a_id), "the pre-existing image was deleted"
    assert loaded.evidence["loaded_digest"] == a_id
    assert "skipped" in loaded.evidence["docker_load_stdout"]
    assert loaded.changed == {}, "nothing may have been bound to the pre-existing image"
    assert scratch.names_of(a_id) == [], "the tarball's tag must not have been added to it"
    assert loaded.test_tag == (a_id if keep_as_test_tag else None)
    if keep_as_test_tag:
        # the id is a usable image reference, and there is nothing for cleanup to remove
        assert _out("image", "inspect", loaded.test_tag, "--format", "{{.Id}}") == a_id
    guard.remove_test_only_tags()
    assert guard.test_only_tags == []
    guard.assert_host_tags_unchanged()
    assert scratch.exists(a_id)


@requires_docker
@requires_registry
@pytest.mark.parametrize("keep_as_test_tag", [False, True])
def test_an_image_known_only_by_a_same_repository_digest_survives_a_load_that_tags_it(scratch, keep_as_test_tag):
    t, image_id, tarball = scratch.make_digest_only("digonly")
    guard = _guard(scratch)

    loaded = guard.load_verified(component="digonly", tarball=tarball, tag=t, recorded_digest=image_id,
                                 keep_as_test_tag=keep_as_test_tag)

    assert scratch.exists(image_id), "the image pulled by digest was deleted"
    assert "skipped" in loaded.evidence["docker_load_stdout"]
    guard.remove_test_only_tags()
    guard.assert_host_tags_unchanged()
    assert scratch.exists(image_id)
    assert scratch.names_of(image_id) == [f"{t.rsplit(':', 1)[0]}@{image_id}"], scratch.names_of(image_id)


@requires_docker
def test_when_the_tarball_cannot_be_read_the_load_runs_and_the_undo_refuses_to_delete(scratch, monkeypatch):
    """The backstop: the pre-load check finds nothing (the tarball's identity is unreadable), the
    load adds `t` to the id-only image, and `_undo` must refuse the `docker rmi` that would delete
    it -- loudly, leaving the extra name -- rather than run it."""
    monkeypatch.setattr(docker_image_tags, "_tarball_identity", lambda tarball: (set(), set()))
    t, a_id, tarball = scratch.make_dangling("backstop")
    guard = _guard(scratch)

    with pytest.raises(ImageHygieneError, match="NOT removed"):
        guard.load_verified(component="backstop", tarball=tarball, tag=t, recorded_digest=a_id, keep_as_test_tag=True)

    assert scratch.exists(a_id), "the refusal must leave the image in place"
    assert scratch.names_of(a_id) == [t], "the tag the load added is left for a person to deal with"
    assert guard.test_only_tags == [], "no test-only tag may be put on an image that a tag removal would delete"


@requires_docker
def test_a_pre_existing_image_with_another_tag_is_loaded_normally_and_only_untagged(scratch, tmp_path):
    """The common case still runs the load and the untag: the image keeps its other tag."""
    other_ref, image_id = scratch.make("kept")
    t = f"{scratch.repo}/kept-alias:t"
    _run("tag", image_id, t)
    tarball = scratch.save(t, "kept")
    _run("rmi", t)
    guard = _guard(scratch)

    loaded = guard.load_verified(component="kept", tarball=tarball, tag=t, recorded_digest=image_id, keep_as_test_tag=True)

    assert "skipped" not in loaded.evidence["docker_load_stdout"]
    assert loaded.changed == {t: (None, image_id)}
    assert scratch.exists(image_id) and t not in scratch.names_of(image_id)
    assert other_ref in scratch.names_of(image_id)
    assert loaded.test_tag in scratch.names_of(image_id)
    guard.remove_test_only_tags()
    guard.assert_host_tags_unchanged()
    assert scratch.names_of(image_id) == [other_ref]


# -------------------------------------------------------------------------------------------------
# The behaviours that must not change
# -------------------------------------------------------------------------------------------------

@requires_docker
def test_a_binding_the_load_really_created_from_nothing_is_still_removed(scratch):
    ref, image_id = scratch.make("created")
    tarball = scratch.save(ref, "created")
    _run("rmi", ref)                                        # last name: the image is gone from the host
    assert not scratch.exists(image_id)
    guard = _guard(scratch)

    loaded = guard.load_verified(component="created", tarball=tarball, tag=ref, recorded_digest=image_id, keep_as_test_tag=False)

    assert loaded.changed == {ref: (None, image_id)}
    assert not scratch.exists(ref) and not scratch.exists(image_id), "the load's own image and tag must be gone again"
    guard.assert_host_tags_unchanged()


@requires_docker
def test_a_moved_tag_is_restored(scratch):
    b_ref, b_id = scratch.make("moved")
    tarball = scratch.save(b_ref, "moved")
    _run("rmi", b_ref)
    a_ref, a_id = scratch.make("moved")                     # a different image now holds `moved:1`
    assert a_id != b_id and scratch.id_of(a_ref) == a_id
    guard = _guard(scratch)

    loaded = guard.load_verified(component="moved", tarball=tarball, tag=a_ref, recorded_digest=b_id, keep_as_test_tag=True)

    assert loaded.changed == {a_ref: (a_id, b_id)}
    assert scratch.id_of(a_ref) == a_id, "the moved tag was not put back"
    assert scratch.id_of(loaded.test_tag) == b_id
    guard.remove_test_only_tags()
    guard.assert_host_tags_unchanged()


@requires_docker
def test_the_closing_assertion_refuses_an_image_that_disappeared(scratch):
    ref, image_id = scratch.make("vanish")
    guard = _guard(scratch)
    guard.assert_host_tags_unchanged()
    _run("rmi", ref)
    with pytest.raises(ImageHygieneError, match="no longer exists"):
        guard.assert_host_tags_unchanged()


# -------------------------------------------------------------------------------------------------
# Question 254 item 10: `remove_owned_name`, the rule the test guards use to remove a name they added
# -------------------------------------------------------------------------------------------------

def test_remove_owned_name_helpers_without_docker():
    assert docker_image_tags._name_row("a/b:t") == ("a/b", "t")
    assert docker_image_tags._name_row("127.0.0.1:19031/a/b:test") == ("127.0.0.1:19031/a/b", "test")
    assert docker_image_tags._name_row("127.0.0.1:19031/a/b") == ("127.0.0.1:19031/a/b", "latest")
    assert docker_image_tags._name_row("a/b@sha256:ab") == ("a/b", None)
    assert docker_image_tags._looks_like_image_id("sha256:0123456789abcdef")
    assert docker_image_tags._looks_like_image_id("0123456789ab")
    assert not docker_image_tags._looks_like_image_id("lockstep-ref:pytest-docker-lifecycle")
    with pytest.raises(ImageHygieneError, match="bare image id"):
        docker_image_tags.remove_owned_name("sha256:0123456789abcdef0123", ids_before=set())


@requires_docker
def test_remove_owned_name_untags_a_name_on_an_image_that_keeps_another_tag(scratch):
    pin, pin_id = scratch.make("pin")
    ids_before = docker_image_tags.image_ids_present()
    alias = f"{scratch.repo}/alias:test"
    _run("tag", pin_id, alias)
    assert docker_image_tags.remove_owned_name(alias, ids_before=ids_before, force=True) == "removed"
    assert scratch.exists(pin) and not scratch.exists(alias)
    assert docker_image_tags.remove_owned_name(alias, ids_before=ids_before) == "absent"


@requires_docker
@pytest.mark.parametrize("force", [False, True])
def test_remove_owned_name_refuses_the_last_name_of_an_image_known_only_by_its_id(scratch, force):
    """The pinned-by-id stand-in: its only other reference is a nameless placeholder, which goes
    with the name (measured), so removing the name would delete the image. Refused, loudly, with
    and without `-f`; the image and the name stay."""
    t, a_id, _tarball = scratch.make_dangling("idonly-rm")
    ids_before = docker_image_tags.image_ids_present()
    assert a_id in ids_before
    alias = f"{scratch.repo}/alias-of-idonly:test"
    _run("tag", a_id, alias)
    with pytest.raises(ImageHygieneError, match="REFUSED") as refused:
        docker_image_tags.remove_owned_name(alias, ids_before=ids_before, force=force)
    assert alias in str(refused.value) and a_id in str(refused.value)
    assert scratch.exists(a_id) and scratch.exists(alias)


@requires_docker
@requires_registry
def test_remove_owned_name_refuses_a_tag_in_the_same_repository_as_the_only_digest_reference(scratch):
    """The other measured shape (`cfs_common`'s: a tag pushed under a registry name beside the same
    repository's digest reference): the digest reference goes with the tag."""
    t, image_id, _tarball = scratch.make_digest_only("digonly-rm")
    ids_before = docker_image_tags.image_ids_present()
    alias = f"{t.rsplit(':', 1)[0]}:test"
    _run("tag", image_id, alias)
    with pytest.raises(ImageHygieneError, match="REFUSED"):
        docker_image_tags.remove_owned_name(alias, ids_before=ids_before, force=True)
    assert scratch.exists(image_id) and scratch.exists(alias)


@requires_docker
def test_remove_owned_name_removes_the_last_name_of_an_image_this_run_made(scratch):
    ids_before = docker_image_tags.image_ids_present()
    ref, image_id = scratch.make("made-here")
    assert image_id not in ids_before
    assert docker_image_tags.remove_owned_name(ref, ids_before=ids_before, force=True) == "removed"
    assert not scratch.exists(image_id)

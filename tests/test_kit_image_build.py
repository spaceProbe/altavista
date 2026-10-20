"""tests/test_kit_image_build.py -- question 250(a), second half: the kit's host-side image step
(`scripts/kit/image_build.py`, run as `installer/install.sh --build-images`), WITHOUT Docker.

Every docker call of the step goes through an injectable runner; `FakeDocker` below is a small model
of the few docker subcommands the step uses (`version`, `image inspect`, `image ls`, `build`,
`create`, `cp`, `rm`, `rmi`), holding a fake image store and a fake image filesystem per built tag.
The real thing is proved against Docker by `tests/test_kit_zero_egress_install.py`; what is proved
here is every refusal and the shape of every docker invocation:

- docker missing (the real runner, an empty PATH) and the daemon not answering;
- the base image missing, or present with another digest, refused BEFORE any build is issued;
- a base found by `RepoDigests` (classic store) as well as by `Id` (containerd store);
- a build failure; a runtime-content mismatch (naming both hashes) that removes every tag the run
  created; a mode mismatch; a `.Config` mismatch;
- the guard that never deletes an image that existed before the run;
- the tag rules, the kit rules (verification first, a Dockerfile that does not start FROM the pin);
- the build is `--network none --pull=false`, with the recorded modes in the context, and no
  directory is ever `docker cp`'d out of an image.
"""
from __future__ import annotations

import hashlib
import io
import json
import subprocess
import sys
import tarfile
from pathlib import Path, PurePosixPath

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402
import docker_image_tags  # noqa: E402
import image_build  # noqa: E402
import image_layer  # noqa: E402
import install  # noqa: E402
import manifest as kmanifest  # noqa: E402
import save_base_images  # noqa: E402

CFS, PLUGIN = "cfs-image", "edge-plugin-image"
LOCAL_TAGS = {CFS: "altavista-cfs-lockstep:local", PLUGIN: "av-edge-plugin:local"}


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# =================================================================================================
# A synthetic kit, and the image filesystem a build of it should produce
# =================================================================================================

KIT_SOURCE_FILES = {
    CFS: {
        "/cfs/av-lockstep-shim": (b"shim", 0o755),
        "/cfs/av-lockstep-relay": (b"relay", 0o755),
        "/cfs/container-entrypoint.sh": (b"#!/bin/sh\nexec true\n", 0o755),
        "/cfs/cpu1/core-cpu1": (b"core", 0o755),
        "/cfs/cpu1/container-start": (b"start", 0o755),
        "/cfs/cpu1/cf/cfe_es_startup.scr": (b"CFE_LIB, x\n", 0o644),
    },
    PLUGIN: {
        "/usr/local/bin/av-edge-plugin": (b"plugin binary", 0o755),
        "/opt/edge-plugin/fixtures/run_products.pb": (b"runs", 0o644),
        "/opt/edge-plugin/fixtures/port_traffic.pb": (b"ports", 0o644),
        "/opt/edge-plugin/plugin-config.default.json": (b"{}", 0o644),
    },
}
PACKAGE_FILES = {
    "/usr/lib/aarch64-linux-gnu/libssl.so.3": (b"libssl", 0o644),
    "/usr/share/doc/libssl3/copyright": (b"copyright text", 0o644),
}
PACKAGE_LIST = [
    "/.", "/usr", "/usr/lib", "/usr/lib/aarch64-linux-gnu", "/usr/lib/aarch64-linux-gnu/libssl.so.3",
    "/usr/lib/aarch64-linux-gnu/libssl.so", "/usr/share", "/usr/share/doc", "/usr/share/doc/libssl3",
    "/usr/share/doc/libssl3/copyright", "/usr/share/doc/libssl3/changelog.gz",
]
DEB = b"!<arch>\nnot a real deb, only its hash matters here"
CONFIGS = {
    CFS: {"Entrypoint": ["/cfs/container-entrypoint.sh"], "Cmd": None, "User": "", "WorkingDir": "", "Volumes": None,
          "Labels": {"org.opencontainers.image.version": "22.04"}, "Env": ["PATH=/usr/bin"]},
    PLUGIN: {"Entrypoint": ["/usr/local/bin/av-edge-plugin"], "Cmd": None, "User": "edgeplugin", "WorkingDir": "/opt/edge-plugin",
             "Volumes": {"/var/lib/edge-plugin": {}}, "Labels": {"org.altavista.component": "edge-plugin"}, "Env": ["PATH=/usr/bin"]},
}


def runtime_hash_of(component: str, extra: dict | None = None) -> str:
    files = {p: sha(d) for p, (d, _m) in KIT_SOURCE_FILES[component].items()}
    if component == PLUGIN:
        files.update({p: sha(d) for p, (d, _m) in PACKAGE_FILES.items()})
    files.update(extra or {})
    return image_layer.runtime_content_hash(files)


def image_filesystem(component: str) -> dict:
    """The fake image's filesystem: path -> ("file", bytes, mode) | ("symlink", target)."""
    fs = {p: ("file", d, m) for p, (d, m) in KIT_SOURCE_FILES[component].items()}
    if component == PLUGIN:
        fs.update({p: ("file", d, m) for p, (d, m) in PACKAGE_FILES.items()})
        fs["/usr/lib/aarch64-linux-gnu/libssl.so"] = ("symlink", "libssl.so.3")
        fs["/var/lib/dpkg/info/libssl3:arm64.list"] = ("file", ("\n".join(PACKAGE_LIST) + "\n").encode(), 0o644)
    return fs


def make_kit(tmp_path: Path, *, name: str = "kit") -> Path:
    kit = tmp_path / name
    images: dict = {}
    for component in (CFS, PLUGIN):
        root = kit / "images" / component
        dockerfile = (REPO_ROOT / image_layer.IMAGE_SERVICE_DIRS[component] / "Dockerfile.kit").read_bytes()
        (root).mkdir(parents=True)
        (root / "Dockerfile").write_bytes(dockerfile)
        files = []
        for path, (data, mode) in KIT_SOURCE_FILES[component].items():
            dest = root / "rootfs" / path.lstrip("/")
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
            dest.chmod(0o600)  # the on-disk mode is NOT the recorded one: the context must use the record
            files.append({"image_path": path, "kit_path": f"images/{component}/rootfs{path}", "sha256": sha(data),
                          "mode": f"{mode:04o}", "size": len(data), "source": "kit"})
        packages = []
        if component == PLUGIN:
            for path, (data, mode) in PACKAGE_FILES.items():
                files.append({"image_path": path, "kit_path": None, "sha256": sha(data), "mode": f"{mode:04o}",
                              "size": len(data), "source": "package:libssl3"})
            (root / "libssl3_9.9-1_arm64.deb").write_bytes(DEB)
            (root / "libssl3.copyright").write_bytes(b"copyright text")
            packages.append({"name": "libssl3", "version": "9.9-1", "architecture": "arm64", "filename": "libssl3_9.9-1_arm64.deb",
                             "kit_path": f"images/{component}/libssl3_9.9-1_arm64.deb", "sha256": sha(DEB), "size": len(DEB)})
        files.sort(key=lambda f: f["image_path"])
        images[component] = {
            "tag": LOCAL_TAGS[component], "recorded_digest": "sha256:" + "a" * 64, "collected": True,
            "runtime_content_hash": runtime_hash_of(component), "runtime_set": image_layer.runtime_set_entry(component),
            "base": image_layer.base_entry(image_layer.base_pin_for(component)),
            "config": image_layer.config_subset(CONFIGS[component]),
            "dockerfile": {"kit_path": f"images/{component}/Dockerfile"}, "files": files, "packages": packages,
            "network_used": False,
        }
    # every kit carries the document of what the operator brings (question 250(a), task 6); the installer's
    # files step refuses a kit that does not, so the synthetic kits used with it carry it too
    (kit / kmanifest.OPERATOR_DOC_PATH).write_bytes((REPO_ROOT / kmanifest.OPERATOR_DOC_SOURCE).read_bytes())
    doc = kmanifest.build_manifest(
        kit_root=kit, git_commit="0" * 40, git_dirty=False, git_status=[], images=images, sboms={}, packs={},
        vendor={}, wheels={}, binaries={}, runs={}, gaps=[],
    )
    kmanifest.write_manifest(doc, kit)
    assert kmanifest.verify_manifest(kit) == []
    return kit


def rewrite_manifest(kit: Path, edit) -> None:
    """Applies `edit(doc)` and writes the manifest again with fresh file hashes: an attacker-consistent kit."""
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    edit(doc)
    fresh = kmanifest.build_manifest(
        kit_root=kit, git_commit=doc["git_commit"], git_dirty=doc["git_dirty"], git_status=doc["git_status"], images=doc["images"],
        sboms={}, packs={}, vendor={}, wheels={}, binaries={}, runs={}, gaps=[],
    )
    kmanifest.write_manifest(fresh, kit)


# =================================================================================================
# The fake docker
# =================================================================================================

def _tar_of(fs: dict, path: str) -> bytes | None:
    buf = io.BytesIO()
    top = PurePosixPath(path)
    with tarfile.open(fileobj=buf, mode="w") as tf:
        hit = False
        for p, v in sorted(fs.items()):
            pp = PurePosixPath(p)
            if pp != top and top not in pp.parents:
                continue
            hit = True
            rel = str(PurePosixPath(top.name) / pp.relative_to(top)) if pp != top else top.name
            info = tarfile.TarInfo(rel)
            if v[0] == "file":
                info.size, info.mode = len(v[1]), v[2]
                tf.addfile(info, io.BytesIO(v[1]))
            else:
                info.type, info.linkname, info.mode = tarfile.SYMTYPE, v[1], 0o777
                tf.addfile(info)
    return buf.getvalue() if hit else None


class FakeDocker:
    """The docker CLI as the step uses it. `calls` records every (args, input)."""

    def __init__(self, kit: Path):
        self.kit = kit
        self.calls: list[tuple[list, bytes | None]] = []
        self.version_rc = 0
        base_pins = {c: image_layer.base_pin_for(c) for c in (CFS, PLUGIN)}
        #: reference -> (Id, RepoDigests)
        self.bases = {p["reference"]: (p["digest"], [p["reference"]]) for p in base_pins.values()}
        self.tags: dict[str, str] = {}               # tag -> id
        self.table_rows: list[tuple[str, str, str]] = []   # (repo, tag, id) already on the host
        self.dangling: list[str] = []
        self.fs = {c: image_filesystem(c) for c in (CFS, PLUGIN)}
        self.built_config = {c: dict(CONFIGS[c]) for c in (CFS, PLUGIN)}
        self.fail_build: set[str] = set()
        self.forced_id: dict[str, str] = {}
        self.containers: dict[str, str] = {}
        self.image_fs: dict[str, dict] = {}
        self.image_config: dict[str, dict] = {}
        self.rmi_rc = 0
        self.n = 0
        #: `docker image ls --digests` rows of other repositories on the host ({Repository, Tag, ID, Digest}),
        #: answered per repository (question 260: the optional ClickHouse image's repository listing)
        self.listing_rows: list[dict] = []
        self.listing_rc = 0
        self.listing_stdout: bytes | None = None   # overrides the rows (an unreadable answer)

    # -- helpers
    def called(self, *prefix: str) -> list[list]:
        return [a for a, _i in self.calls if a[: len(prefix)] == list(prefix)]

    def _ok(self, out: bytes | str = b""):
        return subprocess.CompletedProcess([], 0, out.encode() if isinstance(out, str) else out, b"")

    def _err(self, msg: str, rc: int = 1):
        return subprocess.CompletedProcess([], rc, b"", msg.encode())

    def __call__(self, args, input=None, timeout=None):
        args = list(args)
        self.calls.append((args, input))
        head = args[0]
        if head == "version":
            return self._ok("29.0.0\n") if self.version_rc == 0 else self._err("Cannot connect to the Docker daemon", self.version_rc)
        if args[:2] == ["image", "inspect"]:
            return self._inspect(args)
        if args[:2] == ["image", "ls"] and "--digests" in args:
            if self.listing_rc:
                return self._err("Cannot connect to the Docker daemon", self.listing_rc)
            if self.listing_stdout is not None:
                return self._ok(self.listing_stdout)
            rows = [r for r in self.listing_rows if r["Repository"] == args[-1]]
            return self._ok("".join(json.dumps(r) + "\n" for r in rows))
        if args[:2] == ["image", "ls"]:
            if "--filter" in args:
                return self._ok("".join(f"<none>\t<none>\t{i}\n" for i in self.dangling))
            rows = list(self.table_rows) + [(t.rpartition(":")[0], t.rpartition(":")[2], i) for t, i in self.tags.items()]
            return self._ok("".join(f"{r}\t{t}\t{i}\n" for r, t, i in rows))
        if head == "build":
            return self._build(args, input)
        if head == "create":
            return self._create(args)
        if head == "cp":
            return self._cp(args)
        if head == "rm":
            self.containers.pop(args[-1], None)
            return self._ok()
        if head == "rmi":
            if self.rmi_rc:
                return self._err("conflict", self.rmi_rc)
            self.tags.pop(args[1], None)
            return self._ok()
        raise AssertionError(f"the step issued a docker command this fake does not model: {args}")

    def _inspect(self, args):
        ref = args[2]
        if ref in self.bases:
            image_id, digests = self.bases[ref]
            return self._ok(json.dumps([{"Id": image_id, "RepoDigests": digests}]))
        iid = self.tags.get(ref)
        if iid is None:
            return self._err(f"Error response from daemon: No such image: {ref}")
        if "{{.Id}}" in args:
            return self._ok(iid + "\n")
        if "{{json .Config}}" in args:
            return self._ok(json.dumps(self.image_config[iid]))
        raise AssertionError(args)

    def _build(self, args, context):
        labels = {a.split("=", 1)[0]: a.split("=", 1)[1] for i, a in enumerate(args) if i and args[i - 1] == "--label"}
        tag = args[args.index("-t") + 1]
        component = labels.get(image_build.COMPONENT_LABEL, "?")
        if component in self.fail_build:
            return subprocess.CompletedProcess([], 1, b"Step 5/9 : RUN chmod\n", b"The command '/bin/sh -c chmod' returned a non-zero code: 1\n")
        self.n += 1
        iid = self.forced_id.get(component) or "sha256:" + sha(f"{component}-{self.n}".encode())
        self.tags[tag] = iid
        self.image_fs[iid] = self.fs[component]
        config = dict(self.built_config[component])
        config["Labels"] = {**(config.get("Labels") or {}), **{k: v for k, v in labels.items()}}
        self.image_config[iid] = config
        return subprocess.CompletedProcess([], 0, b"Successfully built\n", b"")

    def _create(self, args):
        tag = args[-1]
        cid = f"cid{len(self.containers) + 1}"
        self.containers[cid] = self.tags[tag]
        return self._ok(cid + "\n")

    def _cp(self, args):
        cid, _, path = args[1].partition(":")
        fs = self.image_fs[self.containers[cid]]
        # A directory is never copied out: that would be a whole tree of the image.
        directories = {str(p) for k in fs for p in PurePosixPath(k).parents}
        if path in directories and path not in {"/cfs/cpu1", "/opt/edge-plugin"}:
            raise AssertionError(f"the step asked docker cp for the directory {path}")
        out = _tar_of(fs, path)
        if out is None:
            return self._err(f"Error response from daemon: Could not find the file {path} in container {cid}")
        return self._ok(out)


@pytest.fixture
def kit(tmp_path):
    return make_kit(tmp_path)


def run_step(kit: Path, docker: FakeDocker, tmp_path: Path, **kw):
    lines: list[str] = []
    kw.setdefault("record_path", tmp_path / "IMAGE_BUILD_RECORD")
    record = image_build.build_images(docker, kit, out=lines.append, **kw)
    return record, lines


# =================================================================================================
# The happy path, and the shape of every invocation
# =================================================================================================

def test_the_step_builds_offline_checks_each_image_and_writes_the_record(kit, tmp_path):
    docker = FakeDocker(kit)
    record, lines = run_step(kit, docker, tmp_path)
    mhash = kmanifest.sbom.sha256_file(kit / "KIT_MANIFEST")
    assert record["kit_manifest_sha256"] == mhash and record["step"] == "install.sh --build-images"
    assert json.loads((tmp_path / "IMAGE_BUILD_RECORD").read_text()) == record
    for component in (CFS, PLUGIN):
        r = record["images"][component]
        assert r["tag"] == f"av-kit/{component}:{mhash[:12]}" and not r["tag"].endswith(":local")
        assert r["equal"] is True and r["runtime_content_hash_computed"] == r["runtime_content_hash_recorded"] == runtime_hash_of(component)
        assert r["modes_equal"] and r["config_equal"] and r["image_id_existed_before"] is False
        assert r["base"]["verified_by"] == "Id" and r["base"]["digest"] == image_layer.base_pin_for(component)["digest"]
        assert r["build"]["flags"] == ["--network", "none", "--pull=false", "--no-cache", "--force-rm", "--rm=true"]
        assert r["files_read"] == len(KIT_SOURCE_FILES[component]) + (len(PACKAGE_FILES) if component == PLUGIN else 0)
    # every docker invocation: nothing but the modelled read/build/create/cp/rm calls; no start, no run, no pull, no load
    heads = {a[0] for a, _i in docker.calls}
    assert heads <= {"version", "image", "build", "create", "cp", "rm"}, heads
    builds = docker.called("build")
    assert len(builds) == 2
    for args in builds:
        assert args[1:7] == ["--network", "none", "--pull=false", "--no-cache", "--force-rm", "--rm=true"] and args[-1] == "-"
        assert f"{image_build.MANIFEST_LABEL}={mhash}" in args
    # the throwaway containers are created with no network and removed with -v
    assert all(a[:3] == ["create", "--network", "none"] for a in docker.called("create"))
    assert [a[:2] for a in docker.called("rm")] == [["rm", "-v"]] * 2 and not docker.containers


def test_each_base_is_verified_before_any_build_is_issued(kit, tmp_path):
    docker = FakeDocker(kit)
    run_step(kit, docker, tmp_path)
    kinds = [a[0] for a, _i in docker.calls]
    assert kinds.index("build") > max(i for i, (a, _x) in enumerate(docker.calls) if a[:2] == ["image", "inspect"] and a[2] in docker.bases)


def test_the_context_carries_the_recorded_modes_the_files_and_nothing_else(kit):
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    context = image_build.build_context(kit, PLUGIN, doc["images"][PLUGIN])
    with tarfile.open(fileobj=io.BytesIO(context)) as tf:
        members = {m.name: m for m in tf}
        files = {n: m for n, m in members.items() if m.isreg()}
        assert set(files) == {
            "Dockerfile", "libssl3_9.9-1_arm64.deb", "rootfs/usr/local/bin/av-edge-plugin",
            "rootfs/opt/edge-plugin/fixtures/run_products.pb", "rootfs/opt/edge-plugin/fixtures/port_traffic.pb",
            "rootfs/opt/edge-plugin/plugin-config.default.json",
        }
        assert files["rootfs/usr/local/bin/av-edge-plugin"].mode == 0o755 and files["rootfs/opt/edge-plugin/fixtures/run_products.pb"].mode == 0o644
        assert all(m.uid == 0 and m.gid == 0 and m.mtime == 0 for m in members.values())
        assert tf.extractfile("rootfs/usr/local/bin/av-edge-plugin").read() == b"plugin binary"
        assert {n for n, m in members.items() if m.isdir()} >= {"rootfs", "rootfs/usr/local/bin", "rootfs/opt/edge-plugin/fixtures"}
    assert context == image_build.build_context(kit, PLUGIN, doc["images"][PLUGIN]), "the context is deterministic"


def test_no_directory_of_the_image_is_ever_copied_out(kit, tmp_path):
    docker = FakeDocker(kit)  # raises if `docker cp` is asked for /usr, /usr/lib, ... a package directory
    run_step(kit, docker, tmp_path)
    copied = [a[1].partition(":")[2] for a in docker.called("cp")]
    assert "/usr/lib/aarch64-linux-gnu" not in copied and "/usr/lib/aarch64-linux-gnu/libssl.so.3" in copied
    assert "/usr/lib/aarch64-linux-gnu/libssl.so" in copied, "a leaf is read; the symlink is then skipped, as `[ -f ] && [ ! -L ]` skips it"


def test_the_python_hash_is_the_builders_algorithm_and_the_config_keys_agree():
    sample = {"/a/b": "1" * 64, "/a/a": "2" * 64, "/c": "3" * 64}
    assert image_build.runtime_content_hash(sample) == image_layer.runtime_content_hash(sample)
    assert image_build.CONFIG_KEYS == image_layer.CONFIG_KEYS
    with pytest.raises(image_build.RuntimeContentMismatch):
        image_build.runtime_content_hash({})


# =================================================================================================
# Refusals: docker, bases
# =================================================================================================

def test_docker_missing_is_a_typed_refusal_exit_2(kit, tmp_path):
    result = subprocess.run(
        [sys.executable, "-S", str(KIT_DIR / "image_build.py"), "--build-images", str(kit), "--record", str(tmp_path / "rec")],
        capture_output=True, text=True, timeout=60, env={"PATH": str(tmp_path / "no-such-dir")},
    )
    assert result.returncode == 2, result.stdout + result.stderr
    assert "REFUSED [docker-unavailable]" in result.stderr and "`docker` CLI was not found" in result.stderr
    assert not (tmp_path / "rec").exists()


def test_a_daemon_that_does_not_answer_is_refused_before_anything_else(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.version_rc = 1
    with pytest.raises(image_build.DockerUnavailable, match="daemon is not reachable"):
        run_step(kit, docker, tmp_path)
    assert [a[0] for a, _i in docker.calls] == ["version"]


@pytest.mark.parametrize("missing", [CFS, PLUGIN])
def test_a_missing_base_is_refused_naming_reference_and_digest_before_any_build(kit, tmp_path, missing):
    docker = FakeDocker(kit)
    pin = image_layer.base_pin_for(missing)
    del docker.bases[pin["reference"]]
    with pytest.raises(image_build.BaseImageMissing) as exc:
        run_step(kit, docker, tmp_path)
    assert pin["reference"] in str(exc.value) and pin["digest"] in str(exc.value) and "docker load" in str(exc.value)
    assert docker.called("build") == [] and docker.called("create") == [] and not (tmp_path / "IMAGE_BUILD_RECORD").exists()


def test_a_base_with_another_digest_is_refused_before_any_build(kit, tmp_path):
    docker = FakeDocker(kit)
    pin = image_layer.base_pin_for(PLUGIN)
    docker.bases[pin["reference"]] = ("sha256:" + "9" * 64, [f"debian@sha256:{'8' * 64}"])
    with pytest.raises(image_build.BaseImageDigestDiffers) as exc:
        run_step(kit, docker, tmp_path)
    assert pin["digest"] in str(exc.value) and "9" * 64 in str(exc.value)
    assert docker.called("build") == []


def test_a_base_found_by_repo_digests_the_classic_store_is_accepted(kit, tmp_path):
    docker = FakeDocker(kit)
    for c in (CFS, PLUGIN):
        pin = image_layer.base_pin_for(c)
        docker.bases[pin["reference"]] = ("sha256:" + "7" * 64, [f"x/y@sha256:{'1' * 64}", pin["reference"]])
    record, _ = run_step(kit, docker, tmp_path)
    assert {r["base"]["verified_by"] for r in record["images"].values()} == {"RepoDigests"}


# =================================================================================================
# Refusals: the build and the built image
# =================================================================================================

def test_a_build_failure_is_refused_with_the_builders_output_and_leaves_nothing(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.fail_build = {PLUGIN}
    with pytest.raises(image_build.BuildFailed) as exc:
        run_step(kit, docker, tmp_path)
    assert "returned a non-zero code" in str(exc.value) and PLUGIN in str(exc.value)
    assert docker.tags == {}, "the cfs image built first is removed again"
    assert not (tmp_path / "IMAGE_BUILD_RECORD").exists()
    assert not docker.containers


def test_a_runtime_content_mismatch_names_both_hashes_and_removes_every_tag_this_run_created(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.fs[PLUGIN] = {**image_filesystem(PLUGIN), "/usr/local/bin/av-edge-plugin": ("file", b"TAMPERED", 0o755)}
    with pytest.raises(image_build.RuntimeContentMismatch) as exc:
        run_step(kit, docker, tmp_path)
    got = image_layer.runtime_content_hash({
        **{p: sha(d) for p, (d, _m) in KIT_SOURCE_FILES[PLUGIN].items() if p != "/usr/local/bin/av-edge-plugin"},
        **{p: sha(d) for p, (d, _m) in PACKAGE_FILES.items()}, "/usr/local/bin/av-edge-plugin": sha(b"TAMPERED"),
    })
    assert got in str(exc.value) and runtime_hash_of(PLUGIN) in str(exc.value)
    assert "/usr/local/bin/av-edge-plugin" in str(exc.value)
    assert docker.tags == {}, "both tags (the good cfs one included) are gone"
    assert {a[1] for a in docker.called("rmi")} == {f"av-kit/{c}:{kmanifest.sbom.sha256_file(kit / 'KIT_MANIFEST')[:12]}" for c in (CFS, PLUGIN)}
    assert not (tmp_path / "IMAGE_BUILD_RECORD").exists() and not docker.containers


def test_an_extra_file_under_a_runtime_root_changes_the_hash_and_is_refused(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.fs[CFS] = {**image_filesystem(CFS), "/cfs/cpu1/surprise": ("file", b"x", 0o644)}
    with pytest.raises(image_build.RuntimeContentMismatch, match="not recorded by the kit: .*surprise"):
        run_step(kit, docker, tmp_path)
    assert docker.tags == {}


def test_a_missing_runtime_root_is_refused(kit, tmp_path):
    docker = FakeDocker(kit)
    fs = image_filesystem(CFS)
    del fs["/cfs/av-lockstep-relay"]
    docker.fs[CFS] = fs
    with pytest.raises(image_build.RuntimeContentMismatch, match="av-lockstep-relay is not in the built image"):
        run_step(kit, docker, tmp_path)


def test_a_mode_other_than_the_recorded_one_is_refused(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.fs[CFS] = {**image_filesystem(CFS), "/cfs/av-lockstep-shim": ("file", b"shim", 0o644)}
    with pytest.raises(image_build.RuntimeContentMismatch, match="other modes than recorded.*0644.*0755"):
        run_step(kit, docker, tmp_path)
    assert docker.tags == {}


def test_a_config_that_differs_from_the_recorded_image_is_refused(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.built_config[PLUGIN] = {**CONFIGS[PLUGIN], "User": "root"}
    with pytest.raises(image_build.ConfigMismatch) as exc:
        run_step(kit, docker, tmp_path)
    assert '"User"' in str(exc.value) and "root" in str(exc.value) and "edgeplugin" in str(exc.value)
    assert docker.tags == {}


def test_the_installers_own_labels_are_not_part_of_the_config_comparison(kit, tmp_path):
    docker = FakeDocker(kit)
    record, _ = run_step(kit, docker, tmp_path)   # the fake adds org.altavista.kit-* labels to the built config
    assert record["images"][PLUGIN]["config_equal"] is True
    assert any(image_build.MANIFEST_LABEL in d["Labels"] for d in docker.image_config.values())


def test_an_image_that_existed_before_the_run_is_never_deleted_by_the_cleanup(kit, tmp_path):
    """The build produced an id that was already on the host with no other name (a dangling
    placeholder only): `docker rmi <tag>` would delete it (measured, containerd store), so it is left."""
    docker = FakeDocker(kit)
    preexisting = "sha256:" + "5" * 64
    docker.dangling = [preexisting]
    docker.forced_id[PLUGIN] = preexisting
    docker.fs[PLUGIN] = {**image_filesystem(PLUGIN), "/usr/local/bin/av-edge-plugin": ("file", b"TAMPERED", 0o755)}
    lines: list[str] = []
    with pytest.raises(image_build.RuntimeContentMismatch):
        image_build.build_images(docker, kit, record_path=tmp_path / "rec", out=lines.append)
    mhash12 = kmanifest.sbom.sha256_file(kit / "KIT_MANIFEST")[:12]
    assert [a[1] for a in docker.called("rmi")] == [f"av-kit/{CFS}:{mhash12}"], "the tag on the pre-existing image was not rmi'd"
    assert any("NOT removed" in l and preexisting in l for l in lines), lines


def test_an_image_id_that_existed_with_another_name_is_only_untagged(kit, tmp_path):
    docker = FakeDocker(kit)
    preexisting = "sha256:" + "5" * 64
    docker.table_rows = [("someone/else", "keep", preexisting)]
    docker.forced_id[PLUGIN] = preexisting
    docker.fs[PLUGIN] = {**image_filesystem(PLUGIN), "/usr/local/bin/av-edge-plugin": ("file", b"TAMPERED", 0o755)}
    with pytest.raises(image_build.RuntimeContentMismatch):
        run_step(kit, docker, tmp_path)
    assert len(docker.called("rmi")) == 2


# =================================================================================================
# Tags, the kit, the record
# =================================================================================================

def test_image_tag_overrides_and_the_tag_rules(kit, tmp_path):
    docker = FakeDocker(kit)
    record, _ = run_step(kit, docker, tmp_path, image_tags=[f"{PLUGIN}=my.registry:5000/av/plugin:t1"])
    assert record["images"][PLUGIN]["tag"] == "my.registry:5000/av/plugin:t1"
    for bad, match in (
        (f"{PLUGIN}=av-edge-plugin:local", "recorded tag"),
        (f"{CFS}=altavista-cfs-lockstep:local", "recorded tag"),
        (f"{PLUGIN}=noTagHere", "REF must be"),
        (f"{PLUGIN}=a/b@sha256:" + "1" * 64, "REF must be"),
        (f"{PLUGIN}=UPPER:t", "REF must be"),
        ("nonesuch=a:b", "COMPONENT=REF"),
        ("no-equals-sign", "COMPONENT=REF"),
    ):
        with pytest.raises(image_build.TagRefused, match=match):
            run_step(kit, FakeDocker(kit), tmp_path / f"x{abs(hash(bad))}", image_tags=[bad])
    with pytest.raises(image_build.TagRefused, match="same tag"):
        run_step(kit, FakeDocker(kit), tmp_path / "same", image_tags=[f"{PLUGIN}=a:b", f"{CFS}=a:b"])


def test_a_tag_that_already_exists_is_refused_and_nothing_is_built(kit, tmp_path):
    docker = FakeDocker(kit)
    docker.table_rows = [("av-kit/cfs-image", kmanifest.sbom.sha256_file(kit / "KIT_MANIFEST")[:12], "sha256:" + "4" * 64)]
    with pytest.raises(image_build.TagRefused, match="already exists"):
        run_step(kit, docker, tmp_path)
    assert docker.called("build") == []


def test_an_existing_record_is_never_overwritten(kit, tmp_path):
    (tmp_path / "IMAGE_BUILD_RECORD").write_text("earlier")
    with pytest.raises(image_build.ImageBuildRefused, match="already exists"):
        run_step(kit, FakeDocker(kit), tmp_path)
    assert (tmp_path / "IMAGE_BUILD_RECORD").read_text() == "earlier"


def test_a_kit_that_does_not_verify_is_refused_before_docker_is_touched(kit, tmp_path):
    (kit / "images" / PLUGIN / "rootfs/usr/local/bin/av-edge-plugin").write_bytes(b"changed after the manifest")
    docker = FakeDocker(kit)
    with pytest.raises(image_build.KitRefused, match="does NOT verify"):
        run_step(kit, docker, tmp_path)
    assert docker.calls == []


def test_a_kit_built_without_images_is_refused(kit, tmp_path):
    def edit(doc):
        doc["images"][CFS] = {**doc["images"][CFS], "collected": False, "files": [], "dockerfile": None}
    rewrite_manifest(kit, edit)
    with pytest.raises(image_build.KitRefused, match="built without --with-images"):
        run_step(kit, FakeDocker(kit), tmp_path)


def test_a_dockerfile_that_does_not_start_from_the_pin_is_refused_even_in_a_consistent_kit(kit, tmp_path):
    path = kit / "images" / PLUGIN / "Dockerfile"
    pin = image_layer.base_pin_for(PLUGIN)["reference"]
    path.write_text(path.read_text().replace(pin, "debian:bookworm-slim"))
    rewrite_manifest(kit, lambda doc: None)
    docker = FakeDocker(kit)
    with pytest.raises(image_build.KitRefused, match="not exactly the pinned base"):
        run_step(kit, docker, tmp_path)
    assert docker.calls == []


def test_a_base_entry_that_disagrees_with_the_installers_pin_is_refused(kit, tmp_path):
    def edit(doc):
        doc["images"][CFS]["base"]["digest"] = "sha256:" + "0" * 64
        doc["images"][CFS]["base"]["reference"] = doc["images"][CFS]["base"]["reference"].split("@")[0] + "@sha256:" + "0" * 64
    rewrite_manifest(kit, edit)
    with pytest.raises(image_build.KitRefused, match="installer's own pin"):
        run_step(kit, FakeDocker(kit), tmp_path)


def test_an_application_file_whose_recorded_hash_disagrees_is_refused(kit, tmp_path):
    """A kit whose KIT_MANIFEST.files was re-hashed to match a changed file but whose images.files
    entry was not: caught while assembling the context, before any docker call that builds."""
    (kit / "images" / CFS / "rootfs/cfs/av-lockstep-shim").write_bytes(b"other shim")
    rewrite_manifest(kit, lambda doc: None)
    docker = FakeDocker(kit)
    with pytest.raises(image_build.KitRefused, match="images.cfs-image.files records"):
        run_step(kit, docker, tmp_path)
    assert docker.called("build") == []


def test_an_old_kit_without_the_runtime_set_or_config_is_refused(kit, tmp_path):
    def edit(doc):
        del doc["images"][CFS]["runtime_set"]
    rewrite_manifest(kit, edit)
    with pytest.raises(image_build.KitRefused, match="lacks .*runtime_set"):
        run_step(kit, FakeDocker(kit), tmp_path)


def test_install_py_dispatches_the_image_flags_and_records_that_it_built_nothing(kit, monkeypatch, tmp_path):
    seen = {}
    monkeypatch.setattr(image_build, "main", lambda argv, **kw: seen.setdefault("argv", argv) and 0)
    assert install.main(["--build-images", str(kit), "--image-tag", "a=b:c"]) == 0
    assert seen["argv"] == ["--build-images", str(kit), "--image-tag", "a=b:c"]
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    mhash = kmanifest.sbom.sha256_file(kit / "KIT_MANIFEST")
    rec = install._images_record(doc, mhash)
    assert rec["built"] is False and rec["built_by"] is None and "--build-images" in rec["build_step"]
    assert rec["components"][PLUGIN]["expected_runtime_content_hash"] == runtime_hash_of(PLUGIN)
    assert rec["components"][PLUGIN]["default_tag"] == f"av-kit/{PLUGIN}:{mhash[:12]}"
    assert rec["components"][CFS]["base_reference"] == image_layer.base_pin_for(CFS)["reference"]


def test_the_step_and_its_helpers_ship_in_the_kits_installer():
    assert "image_build.py" in build_kit.INSTALLER_SOURCE_FILES and "docker_image_tags.py" in build_kit.INSTALLER_SOURCE_FILES


# =================================================================================================
# The offline probe
# =================================================================================================

class ProbeDocker(FakeDocker):
    def __init__(self, kit, *, outcome):
        super().__init__(kit)
        self.outcome = outcome   # name -> ("fail", text) | "succeed"

    def _build(self, args, context):
        tag = args[args.index("-t") + 1]
        assert args[1:7] == ["--network", "none", "--pull=false", "--no-cache", "--force-rm", "--rm=true"], "the probe uses the real flags"
        with tarfile.open(fileobj=io.BytesIO(context)) as tf:
            dockerfile = tf.extractfile("Dockerfile").read().decode()
        name = "tcp-connect" if "1.1.1.1" in dockerfile else "dns-lookup"
        out = self.outcome[name]
        if out == "succeed":
            self.tags[tag] = "sha256:" + "e" * 64
            return subprocess.CompletedProcess([], 0, b"Successfully built\n", b"")
        return subprocess.CompletedProcess([], 1, out[1].encode(), b"The command '/bin/sh -c bash' returned a non-zero code: 1\n")


GOOD = {"tcp-connect": ("fail", "bash: connect: Network is unreachable\n"), "dns-lookup": ("fail", "bash: example.com: Temporary failure in name resolution\n")}


def test_the_offline_probe_passes_when_both_builds_fail_with_the_network_error(kit):
    lines: list[str] = []
    results = image_build.probe_offline_build(ProbeDocker(kit, outcome=GOOD), kit, out=lines.append)
    assert {(r["probe"], r["error"]) for r in results} == {("tcp-connect", "Network is unreachable"), ("dns-lookup", "Temporary failure in name resolution")}
    assert len(results) == 4, "each probe on each of the two pinned bases"


def test_the_offline_probe_fails_when_a_build_can_reach_the_network_and_removes_the_image(kit):
    docker = ProbeDocker(kit, outcome={**GOOD, "tcp-connect": "succeed"})
    with pytest.raises(image_build.OfflineProbeFailed, match="can reach the network"):
        image_build.probe_offline_build(docker, kit, out=lambda _l: None)
    assert docker.tags == {}


def test_the_offline_probe_fails_when_the_build_fails_for_another_reason(kit):
    docker = ProbeDocker(kit, outcome={**GOOD, "dns-lookup": ("fail", "something else broke\n")})
    with pytest.raises(image_build.OfflineProbeFailed, match="not with the network error"):
        image_build.probe_offline_build(docker, kit, out=lambda _l: None)


def test_the_probe_needs_the_bases_too(kit):
    docker = ProbeDocker(kit, outcome=GOOD)
    docker.bases.clear()
    with pytest.raises(image_build.BaseImageMissing):
        image_build.probe_offline_build(docker, kit, out=lambda _l: None)
    assert docker.called("build") == []


# =================================================================================================
# docker_image_tags.ImageTagGuard.load_base_verified and scripts/kit/save_base_images.py
# =================================================================================================

def _base_tarball(path: Path, digest: str, names=None) -> Path:
    index = json.dumps({"schemaVersion": 2, "manifests": [{"digest": digest}]}).encode()
    manifest = json.dumps([{"Config": "blobs/sha256/x", "RepoTags": names, "Layers": []}]).encode()
    with tarfile.open(path, "w") as tf:
        for n, d in (("index.json", index), ("manifest.json", manifest)):
            info = tarfile.TarInfo(n)
            info.size = len(d)
            tf.addfile(info, io.BytesIO(d))
    return path


def _guard_without_docker(monkeypatch):
    monkeypatch.setattr(docker_image_tags, "docker_image_table", lambda run=None: {})
    return docker_image_tags.ImageTagGuard("run", "prefix")


def test_load_base_verified_refuses_a_tarball_that_is_not_the_pinned_image(monkeypatch, tmp_path):
    guard = _guard_without_docker(monkeypatch)
    tar = _base_tarball(tmp_path / "b.tar", "sha256:" + "1" * 64)
    with pytest.raises(docker_image_tags.ImageHygieneError, match="not the pinned"):
        guard.load_base_verified(component="b", tarball=tar, expected_id="sha256:" + "2" * 64)


def test_load_base_verified_refuses_a_tarball_that_would_name_something_on_the_host(monkeypatch, tmp_path):
    guard = _guard_without_docker(monkeypatch)
    tar = _base_tarball(tmp_path / "b.tar", "sha256:" + "1" * 64, names=["ubuntu:22.04"])
    with pytest.raises(docker_image_tags.ImageHygieneError, match="saved by digest"):
        guard.load_base_verified(component="b", tarball=tar, expected_id="sha256:" + "1" * 64)


def test_load_base_verified_skips_the_load_when_the_pinned_image_is_present(monkeypatch, tmp_path):
    pin = "sha256:" + "1" * 64
    monkeypatch.setattr(docker_image_tags, "docker_image_table", lambda run=None: {pin: [("ubuntu", "22.04")]})
    ran = []
    monkeypatch.setattr(docker_image_tags.subprocess, "run", lambda args, **kw: (ran.append(args), subprocess.CompletedProcess(args, 0, pin + "\n", ""))[1])
    guard = docker_image_tags.ImageTagGuard("run", "prefix")
    evidence = guard.load_base_verified(component="b", tarball=_base_tarball(tmp_path / "b.tar", pin), expected_id=pin)
    assert evidence["present_before"] is True and evidence["docker_load"].startswith("skipped")
    assert all(a[1] != "load" for a in ran), ran


def test_save_base_images_writes_digest_selected_tarballs_and_sums(tmp_path):
    pins = save_base_images.base_pins()
    assert set(pins) == {"debian-bookworm-slim-base-image", "ubuntu-22.04-base-image"}
    seen = []

    def fake_save(reference, out_path):
        seen.append(reference)
        digest = reference.rpartition("@")[2]
        _base_tarball(out_path, digest)
        return subprocess.CompletedProcess([], 0, "", "")

    results = save_base_images.save_bases(tmp_path / "bases", pins, save=fake_save)
    assert all("@sha256:" in r for r in seen), "selected by digest, never by a floating tag"
    assert save_base_images.verify_sums(tmp_path / "bases").keys() == {"debian-bookworm-slim-base-image.tar", "ubuntu-22.04-base-image.tar"}
    assert {r["digest"] for r in results.values()} == {p["digest"] for p in pins.values()}
    with pytest.raises(save_base_images.SaveRefused, match="already holds"):
        save_base_images.save_bases(tmp_path / "bases", pins, save=fake_save)
    (tmp_path / "bases" / "ubuntu-22.04-base-image.tar").write_bytes(b"changed")
    with pytest.raises(save_base_images.SaveRefused, match="sha256"):
        save_base_images.verify_sums(tmp_path / "bases")


def test_save_base_images_refuses_a_missing_image_and_a_wrong_digest_and_leaves_nothing(tmp_path):
    pins = save_base_images.base_pins()
    missing = lambda ref, out: subprocess.CompletedProcess([], 1, "", "No such image")
    with pytest.raises(save_base_images.SaveRefused, match="never pulls"):
        save_base_images.save_bases(tmp_path / "a", pins, save=missing)
    wrong = lambda ref, out: (_base_tarball(out, "sha256:" + "0" * 64), subprocess.CompletedProcess([], 0, "", ""))[1]
    with pytest.raises(save_base_images.SaveRefused, match="expected exactly"):
        save_base_images.save_bases(tmp_path / "b", pins, save=wrong)
    assert not list((tmp_path / "a").iterdir()) and not list((tmp_path / "b").iterdir())


def test_an_attacker_consistent_kit_is_refused_on_the_aggregate_hash_and_says_its_own_records_disagree(kit, tmp_path):
    """One application file changed AND its images.files entry re-hashed AND KIT_MANIFEST re-hashed: the kit
    verifies, the build (here, the fake) reproduces exactly what the kit now carries, and only the recorded
    aggregate runtime-content hash still says what the original image was. Refused naming both hashes."""
    path = kit / "images" / PLUGIN / "rootfs/opt/edge-plugin/plugin-config.default.json"
    changed = b'{"changed": true}'
    path.write_bytes(changed)

    def edit(doc):
        entry = next(f for f in doc["images"][PLUGIN]["files"] if f["image_path"] == "/opt/edge-plugin/plugin-config.default.json")
        entry["sha256"], entry["size"] = sha(changed), len(changed)
    rewrite_manifest(kit, edit)
    docker = FakeDocker(kit)
    docker.fs[PLUGIN] = {**image_filesystem(PLUGIN), "/opt/edge-plugin/plugin-config.default.json": ("file", changed, 0o644)}
    with pytest.raises(image_build.RuntimeContentMismatch) as exc:
        run_step(kit, docker, tmp_path)
    shas = {p: sha(d) for p, (d, _m) in KIT_SOURCE_FILES[PLUGIN].items()} | {p: sha(d) for p, (d, _m) in PACKAGE_FILES.items()}
    shas["/opt/edge-plugin/plugin-config.default.json"] = sha(changed)
    computed = image_layer.runtime_content_hash(shas)
    assert computed in str(exc.value) and runtime_hash_of(PLUGIN) in str(exc.value) and "internally inconsistent" in str(exc.value)
    assert docker.tags == {}


# =================================================================================================
# install.sh: the interpreter it picks, and that it never writes bytecode into a kit
# =================================================================================================

def _sh_sandbox(tmp_path: Path, python3_body: str) -> dict:
    """A PATH holding only `dirname` (symlinked) and a fake `python3`, so the shell script's own logic is what runs."""
    import os
    import shutil

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    os.symlink(shutil.which("dirname"), bin_dir / "dirname")
    fake = bin_dir / "python3"
    fake.write_text("#!/bin/sh\n" + python3_body)
    fake.chmod(0o755)
    return {"PATH": str(bin_dir)}


def test_install_sh_refuses_a_python_older_than_3_11_with_a_typed_message(tmp_path):
    env = _sh_sandbox(tmp_path, "exit 1\n")   # every `-c 'sys.version_info >= (3, 11)'` probe fails
    result = subprocess.run([str(KIT_DIR / "install.sh"), "--build-images", "x"], capture_output=True, text=True, env=env, timeout=30)
    assert result.returncode == 1 and "no Python >= 3.11 interpreter" in result.stderr and "Traceback" not in result.stderr


def test_install_sh_runs_install_py_with_bytecode_writing_off(tmp_path):
    env = _sh_sandbox(tmp_path, 'case "$1" in -c) exit 0;; *) echo "RAN $PYTHONDONTWRITEBYTECODE $*";; esac\n')
    result = subprocess.run([str(KIT_DIR / "install.sh"), "--probe-offline-build", "k"], capture_output=True, text=True, env=env, timeout=30)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f"RAN 1 {KIT_DIR / 'install.py'} --probe-offline-build k"


def test_running_the_installer_from_a_kit_adds_nothing_to_it(kit, tmp_path):
    """The bytecode of the modules `install.py` imports must not land in `<kit>/installer/__pycache__`: the
    kit would stop verifying against its own KIT_MANIFEST (found on the first real run of the image step)."""
    installer = kit / "installer"
    installer.mkdir()
    for name in build_kit.INSTALLER_SOURCE_FILES:
        (installer / name).write_bytes((KIT_DIR / name).read_bytes())
    before = sorted(p.relative_to(kit).as_posix() for p in kit.rglob("*"))
    result = subprocess.run(
        [sys.executable, "-I", str(installer / "install.py"), "--build-images", str(kit / "no-such-kit")],
        capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 2 and "REFUSED [kit]" in result.stderr, result.stderr
    assert sorted(p.relative_to(kit).as_posix() for p in kit.rglob("*")) == before

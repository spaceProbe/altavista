"""Question 250(a), first half: the kit carries each image's APPLICATION LAYER, not the image.

Everything here needs no Docker: the pins file against both Dockerfiles, the runtime Dockerfiles
against the development ones, the recorded `.deb`, the runtime-content hash over a synthetic tree,
`ensure_deb`'s cache and refusals, and the whole `collect_images` path with the docker calls faked
(an image is a directory; `docker cp` is a copy out of it). The tests that touch a real image are
in `tests/test_kit_manifest.py`, behind `AV_KIT_WITH_IMAGES=1`.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import re
import shutil
import stat
import sys
import tarfile
from contextlib import contextmanager, nullcontext
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402
import image_layer  # noqa: E402
import manifest as kmanifest  # noqa: E402
import sbom  # noqa: E402

DEV_DOCKERFILES = {c: REPO_ROOT / d / "Dockerfile" for c, d in image_layer.IMAGE_SERVICE_DIRS.items()}
KIT_DOCKERFILES = {c: REPO_ROOT / d / "Dockerfile.kit" for c, d in image_layer.IMAGE_SERVICE_DIRS.items()}


# =================================================================================================
# 1. The pins file
# =================================================================================================

def test_the_pins_file_has_the_two_base_images_with_their_doc_item_numbers():
    doc = image_layer.load_operator_supplied()
    assert doc["schema"] == 1 and doc["doc"] == "docs/kit-operator-supplied.md"
    bases = {cid: c for cid, c in doc["components"].items() if c["kind"] == "container-base-image"}
    by_item = {c["item"]: c for c in bases.values()}
    assert set(by_item) == {1, 2}
    # every other component is the operator's GMAT (item 3, test_kit_install.py): a data tree, not a base image
    assert {c["kind"] for cid, c in doc["components"].items() if cid not in bases} == {"data-tree"}
    assert by_item[1]["reference"] == (
        "ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc")
    assert by_item[1]["base_of"] == ["cfs-image"]
    assert by_item[2]["reference"] == (
        "debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171")
    assert by_item[2]["base_of"] == ["edge-plugin-image"]
    for c in bases.values():
        assert re.fullmatch(r"[a-z0-9./:_-]+@sha256:[0-9a-f]{64}", c["reference"])
        assert c["reference"].endswith("@" + c["digest"])
        assert c["why"].strip() and c["licences"].strip()
    # every image component has exactly one base
    for component in sbom.IMAGE_COMPONENTS:
        assert image_layer.base_pin_for(component)["reference"].startswith(("ubuntu:", "debian:"))


@pytest.mark.parametrize("component", sbom.IMAGE_COMPONENTS)
def test_the_pin_equals_every_from_digest_of_both_dockerfiles(component):
    pin = image_layer.base_pin_for(component)["reference"]
    dev_froms = image_layer.from_references(image_layer.dockerfile_instructions(DEV_DOCKERFILES[component]))
    kit_froms = image_layer.from_references(image_layer.dockerfile_instructions(KIT_DOCKERFILES[component]))
    assert dev_froms and set(dev_froms) == {pin}, (dev_froms, pin)   # cFS has two stages, both on the pin
    assert kit_froms == [pin]


def test_the_pins_file_is_carried_in_the_installer(tmp_path):
    assert "operator_supplied.json" in build_kit.INSTALLER_SOURCE_FILES


# =================================================================================================
# 2. Dockerfile parity
# =================================================================================================

@pytest.mark.parametrize("component", sbom.IMAGE_COMPONENTS)
def test_the_runtime_dockerfile_is_as_pinned_and_matches_the_development_final_stage(component):
    assert image_layer.kit_dockerfile_problems(REPO_ROOT, component) == []
    dev = image_layer.final_stage(image_layer.dockerfile_instructions(DEV_DOCKERFILES[component]))
    kit = image_layer.dockerfile_instructions(KIT_DOCKERFILES[component])
    assert image_layer.config_instructions(dev) == image_layer.config_instructions(kit)
    assert any(k == "ENTRYPOINT" for k, _ in image_layer.config_instructions(kit))


def test_the_edge_plugin_configuration_is_what_the_hardening_header_promises():
    kit = image_layer.config_instructions(image_layer.dockerfile_instructions(KIT_DOCKERFILES["edge-plugin-image"]))
    keywords = [k for k, _ in kit]
    assert keywords == ["LABEL", "LABEL", "WORKDIR", "VOLUME", "USER", "ENTRYPOINT"]
    assert ("USER", "edgeplugin") in kit and ("WORKDIR", "/opt/edge-plugin") in kit


def _copy_service_files(dest: Path) -> None:
    for component, svc in image_layer.IMAGE_SERVICE_DIRS.items():
        (dest / svc).mkdir(parents=True, exist_ok=True)
        for name in ("Dockerfile", "Dockerfile.kit", "IMAGE_DIGEST.md", "IMAGE_CONTEXT_MANIFEST.txt", "runtime-packages.json"):
            if (REPO_ROOT / svc / name).is_file():
                shutil.copyfile(REPO_ROOT / svc / name, dest / svc / name)
    (dest / "scripts" / "kit").mkdir(parents=True, exist_ok=True)
    shutil.copyfile(KIT_DIR / "operator_supplied.json", dest / "scripts" / "kit" / "operator_supplied.json")


def test_dockerfile_parity_is_enforced_on_every_difference_that_matters(tmp_path):
    _copy_service_files(tmp_path)
    assert image_layer.kit_dockerfile_problems(tmp_path, "edge-plugin-image") == []
    kit = tmp_path / "services" / "edge-plugin" / "Dockerfile.kit"
    original = kit.read_text()

    def perturbed(old: str, new: str) -> list[str]:
        assert old in original, old
        kit.write_text(original.replace(old, new))
        try:
            return image_layer.kit_dockerfile_problems(tmp_path, "edge-plugin-image")
        finally:
            kit.write_text(original)

    assert any("USER" in p or "configuration" in p for p in perturbed("USER edgeplugin", "USER root"))
    assert any("configuration" in p for p in perturbed('LABEL org.altavista.component="edge-plugin"', 'LABEL org.altavista.component="x"'))
    assert any("FROM" in p for p in perturbed("@sha256:88200866", "@sha256:88200867"))
    assert any("RUN" in p and "apt" in p for p in perturbed("RUN DEBIAN_FRONTEND=noninteractive dpkg -i", "RUN apt-get install -y libssl3 && dpkg -i"))
    assert any("flag or heredoc" in p for p in perturbed("COPY rootfs/usr/local/bin/av-edge-plugin", "COPY --chmod=755 rootfs/usr/local/bin/av-edge-plugin"))


def test_no_runtime_dockerfile_run_line_needs_a_network():
    for component, path in KIT_DOCKERFILES.items():
        for ins in image_layer.dockerfile_instructions(path):
            assert ins.keyword in image_layer.KIT_DOCKERFILE_KEYWORDS, (component, ins)
            if ins.keyword == "RUN":
                assert not image_layer._NETWORK_WORDS.search(ins.args), (component, ins.args)
                assert "<<" not in ins.args


# =================================================================================================
# 3. The recorded .deb
# =================================================================================================

def test_the_libssl3_record_is_complete_and_matches_the_dockerfile():
    doc = json.loads((REPO_ROOT / "services/edge-plugin/runtime-packages.json").read_text())
    assert doc["image"] == "edge-plugin-image" and len(doc["packages"]) == 1
    rec = doc["packages"][0]
    assert (rec["package"], rec["version"], rec["architecture"]) == ("libssl3", "3.0.22-1~deb12u1", "arm64")
    assert rec["filename"] == f"libssl3_{rec['version']}_{rec['architecture']}.deb"
    assert re.fullmatch(r"[0-9a-f]{64}", rec["sha256"]) and isinstance(rec["size"], int) and rec["size"] > 0
    assert rec["snapshot_url"].startswith("https://snapshot.debian.org/archive/")
    assert rec["snapshot_url"].endswith("/" + rec["filename"])
    assert re.fullmatch(r"[0-9a-f]{40}", rec["snapshot_sha1"])
    lic = rec["licence"]
    assert re.fullmatch(r"[0-9a-f]{64}", lic["copyright_sha256"])
    assert lic["binary_package_licence"] == "Apache-2.0"
    assert lic["licence_fields"] and "Apache-2.0" in lic["licence_fields"]
    assert lic["note"].strip()
    kit_text = KIT_DOCKERFILES["edge-plugin-image"].read_text()
    assert f"COPY {rec['filename']} /tmp/libssl3.deb" in kit_text
    assert "dpkg -i /tmp/libssl3.deb" in kit_text
    assert set(image_layer.RUNTIME_SETS["edge-plugin-image"].packages) == {"libssl3"}


def _ar(members: list[tuple[str, bytes]]) -> bytes:
    out = b"!<arch>\n"
    for name, body in members:
        out += f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(body):<10}`\n".encode()
        out += body + (b"\n" if len(body) % 2 else b"")
    return out


def _tar_xz(entries: list[tuple[str, bytes | None, int]]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:xz") as tf:
        for name, data, mode in entries:
            info = tarfile.TarInfo(name)
            info.mode = mode
            if data is None:
                info.type = tarfile.DIRTYPE
                tf.addfile(info)
            else:
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))
    return buf.getvalue()


COPYRIGHT = b"Format: x\n\nFiles: *\nLicense: Apache-2.0\n\nFiles: x/*\nLicense: Artistic or GPL-1+\n\nLicense: Apache-2.0\n text\n"
LIB_BYTES = b"\x7fELF fake libssl"


def fake_deb_bytes(lib: bytes = LIB_BYTES) -> bytes:
    control = b"Package: libssl3\nVersion: 9.9-1\nArchitecture: arm64\nDescription: x\n y\n"
    return _ar([
        ("debian-binary", b"2.0\n"),
        ("control.tar.xz", _tar_xz([("./control", control, 0o644)])),
        ("data.tar.xz", _tar_xz([
            ("./", None, 0o755), ("./usr", None, 0o755), ("./usr/lib", None, 0o755),
            ("./usr/lib/libssl.so.3", lib, 0o644),
            ("./usr/share", None, 0o755), ("./usr/share/doc", None, 0o755), ("./usr/share/doc/libssl3", None, 0o755),
            ("./usr/share/doc/libssl3/changelog.gz", b"log", 0o644),
            ("./usr/share/doc/libssl3/copyright", COPYRIGHT, 0o644),
        ])),
    ])


def fake_record(deb: bytes) -> dict:
    return {
        "package": "libssl3", "version": "9.9-1", "architecture": "arm64", "filename": "libssl3_9.9-1_arm64.deb",
        "snapshot_url": "https://snapshot.invalid/libssl3_9.9-1_arm64.deb", "sha256": hashlib.sha256(deb).hexdigest(),
        "size": len(deb), "licence": {
            "copyright_sha256": hashlib.sha256(COPYRIGHT).hexdigest(),
            "licence_fields": ["Apache-2.0", "Artistic or GPL-1+"],
        },
    }


def test_read_deb_reads_control_files_modes_and_the_copyright(tmp_path):
    path = tmp_path / "x.deb"
    path.write_bytes(fake_deb_bytes())
    deb = image_layer.read_deb(path)
    assert deb.control["Package"] == "libssl3" and deb.control["Version"] == "9.9-1"
    assert deb.files["/usr/lib/libssl.so.3"].kind == "file"
    assert deb.files["/usr/lib/libssl.so.3"].sha256 == hashlib.sha256(LIB_BYTES).hexdigest()
    assert deb.files["/usr/lib"].kind == "dir"
    assert deb.copyright == COPYRIGHT
    assert image_layer.copyright_licences(COPYRIGHT) == ["Apache-2.0", "Artistic or GPL-1+"]
    image_layer.check_deb_against_record(deb, fake_record(path.read_bytes()))


def test_ensure_deb_uses_the_cache_and_fetches_once_and_refuses_a_wrong_hash(tmp_path):
    data = fake_deb_bytes()
    record = fake_record(data)
    calls = []

    def download(url):
        calls.append(url)
        return data

    first = image_layer.ensure_deb(tmp_path, record, download=download)
    assert first == tmp_path / ".av-test-tmp" / "kit-deb-cache" / record["sha256"] / record["filename"]
    assert first.read_bytes() == data and calls == [record["snapshot_url"]]
    # second call: served from the cache, same path, no network
    assert image_layer.ensure_deb(tmp_path, record, download=lambda url: pytest.fail("network used on a cache hit")) == first

    # recorded sha256 changed by one hex digit: the fetched bytes are refused and nothing is cached
    wrong = dict(record, sha256=record["sha256"][:-1] + ("0" if record["sha256"][-1] != "0" else "1"))
    with pytest.raises(image_layer.ImageLayerError, match="refusing it"):
        image_layer.ensure_deb(tmp_path, wrong, download=download)
    assert not (tmp_path / ".av-test-tmp" / "kit-deb-cache" / wrong["sha256"]).exists()

    # a cache entry whose bytes no longer match its name is refused too
    first.write_bytes(data + b"x")
    with pytest.raises(image_layer.ImageLayerError, match="refusing it"):
        image_layer.ensure_deb(tmp_path, record, download=download)

    # a wrong size alone is refused
    first.write_bytes(data)
    with pytest.raises(image_layer.ImageLayerError):
        image_layer.ensure_deb(tmp_path, dict(record, size=record["size"] + 1), download=download)


def test_the_cache_is_ignored_by_git():
    import subprocess
    r = subprocess.run(["git", "check-ignore", "-q", ".av-test-tmp/kit-deb-cache/x/y.deb"], cwd=REPO_ROOT)
    assert r.returncode == 0


def test_the_recorded_deb_in_the_cache_when_present_matches_the_record():
    """Skips visibly on a clean clone (the kit builder fetches it); when a previous kit build left it in
    the cache, the real bytes must equal the committed record."""
    rec = image_layer.load_runtime_packages(REPO_ROOT, "edge-plugin-image")[0]
    cached = REPO_ROOT / image_layer.DEB_CACHE_DIR / rec["sha256"] / rec["filename"]
    if not cached.is_file():
        pytest.skip(f"{cached} is not in the cache (a --with-images kit build fetches it)")
    deb = image_layer.read_deb(cached)
    assert deb.sha256 == rec["sha256"] and deb.size == rec["size"]
    image_layer.check_deb_against_record(deb, rec)
    # the files the .deb installs: libssl.so.3 and libcrypto.so.3 are among them
    assert "/usr/lib/aarch64-linux-gnu/libssl.so.3" in deb.files and "/usr/lib/aarch64-linux-gnu/libcrypto.so.3" in deb.files


# =================================================================================================
# 4. The runtime-content hash
# =================================================================================================

def test_runtime_content_hash_is_the_documented_algorithm_over_a_synthetic_tree(tmp_path):
    tree = {"/b/file": b"two", "/a/file": b"one", "/a/file.x": b"three", "/a/file y": b""}
    shas = {p: hashlib.sha256(d).hexdigest() for p, d in tree.items()}
    # written out by hand: "<path> <sha>" lines, sorted bytewise by the WHOLE line ("/a/file <sha>"
    # sorts before "/a/file.x ..." because ' ' < '.'), each line ending in "\n"
    expected_lines = sorted(f"{p} {s}" for p, s in shas.items())
    assert expected_lines[0].startswith("/a/file ") and expected_lines[1].startswith("/a/file y ")
    expected = "sha256:" + hashlib.sha256(("\n".join(expected_lines) + "\n").encode()).hexdigest()
    assert image_layer.runtime_content_hash(shas) == expected
    # one byte of one file moves it
    changed = dict(shas, **{"/b/file": hashlib.sha256(b"twp").hexdigest()})
    assert image_layer.runtime_content_hash(changed) != expected
    with pytest.raises(image_layer.ImageLayerError):
        image_layer.runtime_content_hash({})


def test_the_recorded_hashes_are_read_from_each_components_own_record():
    cfs = image_layer.read_recorded_runtime_hash(REPO_ROOT, "cfs-image")
    edge = image_layer.read_recorded_runtime_hash(REPO_ROOT, "edge-plugin-image")
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", cfs) and re.fullmatch(r"sha256:[0-9a-f]{64}", edge) and cfs != edge
    assert cfs in (REPO_ROOT / "services/cfs/IMAGE_DIGEST.md").read_text()
    assert edge in (REPO_ROOT / "services/edge-plugin/IMAGE_DIGEST.md").read_text()


def test_the_edge_build_script_defines_the_same_set_as_the_python_implementation():
    text = (REPO_ROOT / "services/edge-plugin/build-image.sh").read_text()
    rset = image_layer.RUNTIME_SETS["edge-plugin-image"]
    for tree in rset.trees:
        assert f"find {tree} -type f" in text
    for path in rset.files:
        assert f"sha256sum {path}" in text
    for package in rset.packages:
        assert f"dpkg -L {package}" in text
    assert "LC_ALL=C sort" in text and "RUNTIME_CONTENT_HASH" in text


def test_the_sbom_parser_still_reads_the_edge_digest_record_after_the_new_block():
    doc = sbom.image_sbom("edge-plugin-image")
    assert doc["metadata"]["component"]["version"] == "av-edge-plugin:local"


# =================================================================================================
# 5. Roles
# =================================================================================================

def test_roles_for_the_new_kit_files():
    c = kmanifest._classify_role
    assert c("images/cfs-image/Dockerfile") == "image-dockerfile"
    assert c("images/cfs-image/rootfs/cfs/cpu1/core-cpu1") == "image-rootfs"
    assert c("images/edge-plugin-image/libssl3_3.0.22-1~deb12u1_arm64.deb") == "image-package"
    assert c("images/edge-plugin-image/libssl3.copyright") == "image-package-licence"
    assert c("installer/operator_supplied.json") == "installer"
    with pytest.raises(ValueError):
        c("images/cfs-image.tar")
    with pytest.raises(ValueError):
        c("images/cfs-image/surprise.bin")


# =================================================================================================
# 6. collect_images with the docker calls faked
# =================================================================================================

def make_fake_image(root: Path, component: str, *, lib: bytes = LIB_BYTES) -> dict[str, bytes]:
    """A directory standing in for an image's filesystem, with exactly `RUNTIME_SETS[component]`'s
    shape. Returns {image_path: bytes} for the runtime-content set's files."""
    files: dict[str, tuple[bytes, int]] = {}
    if component == "cfs-image":
        files = {
            "/cfs/av-lockstep-shim": (b"shim", 0o755), "/cfs/av-lockstep-relay": (b"relay", 0o755),
            "/cfs/container-entrypoint.sh": (b"#!/bin/sh\n", 0o755),
            "/cfs/cpu1/core-cpu1": (b"core", 0o755), "/cfs/cpu1/container-start": (b"go", 0o755),
            "/cfs/cpu1/cf/app.so": (b"so", 0o644),
        }
    else:
        files = {
            "/usr/local/bin/av-edge-plugin": (b"plugin", 0o755),
            "/opt/edge-plugin/fixtures/run_products.pb": (b"rp", 0o644),
            "/opt/edge-plugin/fixtures/port_traffic.pb": (b"pt", 0o644),
            "/opt/edge-plugin/plugin-config.default.json": (b"{}", 0o644),
            "/usr/lib/libssl.so.3": (lib, 0o644),
            "/usr/share/doc/libssl3/copyright": (COPYRIGHT, 0o644),
        }
        info = root / "var/lib/dpkg/info"
        info.mkdir(parents=True)
        (info / "libssl3:arm64.list").write_text(
            "/.\n/usr\n/usr/lib\n/usr/lib/libssl.so.3\n/usr/share\n/usr/share/doc\n/usr/share/doc/libssl3\n"
            "/usr/share/doc/libssl3/changelog.gz\n/usr/share/doc/libssl3/copyright\n"
        )
    for path, (data, mode) in files.items():
        full = root / path.lstrip("/")
        full.parent.mkdir(parents=True, exist_ok=True)
        full.write_bytes(data)
        full.chmod(mode)
    return {p: d for p, (d, _m) in files.items()}


def fake_copier_factory(root: Path):
    @contextmanager
    def factory(tag, run_id):
        def copy_out(image_path: str, host_parent: Path) -> bool:
            src = root / image_path.lstrip("/")
            if not src.exists() and not src.is_symlink():
                return False
            dest = host_parent / src.name
            if src.is_dir() and not src.is_symlink():
                shutil.copytree(src, dest, symlinks=True)
            else:
                shutil.copy2(src, dest, follow_symlinks=False)
            return True
        yield copy_out
    return factory


def synthetic_prepared(component: str, tmp_path: Path, deb_bytes: bytes | None = None, **overrides) -> image_layer.PreparedComponent:
    debs, records = {}, []
    if component == "edge-plugin-image":
        deb_bytes = deb_bytes or fake_deb_bytes()
        path = tmp_path / "libssl3_9.9-1_arm64.deb"
        path.write_bytes(deb_bytes)
        debs = {"libssl3": image_layer.read_deb(path)}
        records = [fake_record(deb_bytes)]
        records[0]["licence"]["licence_fields"] = ["Apache-2.0", "Artistic or GPL-1+"]
    prepared = image_layer.PreparedComponent(
        component=component, recorded_runtime_hash="", base=image_layer.base_pin_for(component),
        packages=records, debs=debs,
    )
    for k, v in overrides.items():
        setattr(prepared, k, v)
    return prepared


def expected_hash(component: str, files: dict[str, bytes], excluded=()) -> str:
    return image_layer.runtime_content_hash({p: hashlib.sha256(d).hexdigest() for p, d in files.items() if p not in excluded})


@pytest.mark.parametrize("component", sbom.IMAGE_COMPONENTS)
def test_collect_component_writes_the_application_layer_and_the_dockerfile_copies_resolve(tmp_path, component):
    image_root = tmp_path / "image"
    image_root.mkdir()
    files = make_fake_image(image_root, component)
    excluded = {"/usr/share/doc/libssl3/changelog.gz"}  # listed by dpkg, not present: the slim image's path-exclude
    prepared = synthetic_prepared(component, tmp_path)
    prepared.recorded_runtime_hash = expected_hash(component, files)
    kit = tmp_path / "kit"
    kit.mkdir()
    recorded = {"tag": f"{component}:fake", "recorded_digest": "sha256:" + "a" * 64}

    entry = image_layer.collect_component(
        REPO_ROOT, kit, component, prepared, recorded, tag=recorded["tag"], run_id="t",
        copier_factory=fake_copier_factory(image_root), config={"Entrypoint": ["/e"], "Hostname": "x"},
    )
    assert entry["runtime_set"] == image_layer.runtime_set_entry(component)
    assert entry["config"] == image_layer.config_subset({"Entrypoint": ["/e"]})

    root = kit / "images" / component
    assert (root / "Dockerfile").read_bytes() == KIT_DOCKERFILES[component].read_bytes()
    assert entry["runtime_content_hash"] == prepared.recorded_runtime_hash and entry["collected"] is True
    kit_files = {f["image_path"]: f for f in entry["files"] if f["source"] == "kit"}
    for image_path, f in kit_files.items():
        on_disk = root / "rootfs" / image_path.lstrip("/")
        assert on_disk.read_bytes() == files[image_path]
        assert f"{on_disk.stat().st_mode & 0o7777:04o}" == f["mode"] and f["kit_path"] == f"images/{component}/rootfs{image_path}"
    if component == "edge-plugin-image":
        pkg_files = [f for f in entry["files"] if f["source"] == "package:libssl3"]
        assert {f["image_path"] for f in pkg_files} == {"/usr/lib/libssl.so.3", "/usr/share/doc/libssl3/copyright"}
        assert all(f["kit_path"] is None for f in pkg_files)
        assert (root / "libssl3_9.9-1_arm64.deb").read_bytes() == prepared.debs["libssl3"].path.read_bytes()
        assert (root / "libssl3.copyright").read_bytes() == COPYRIGHT
        assert entry["packages"][0]["dpkg_excluded_files"] == sorted(excluded) and entry["network_used"] is True
    else:
        assert entry["packages"] == [] and entry["network_used"] is False
    assert not list(root.rglob("*.tar"))

    # Every COPY of the runtime Dockerfile names something the kit has, and every kit file is covered.
    copies = [i.args.split() for i in image_layer.dockerfile_instructions(root / "Dockerfile") if i.keyword == "COPY"]
    covered: set[str] = set()
    for src, dst in copies:
        if src.startswith("rootfs/"):
            assert (root / src).exists(), src
            assert "/" + src[len("rootfs/"):] == dst, (src, dst)
            for image_path in kit_files:
                if image_path == dst or image_path.startswith(dst + "/"):
                    covered.add(image_path)
        else:
            assert src.startswith("libssl3_") and src.endswith(".deb"), src   # the recorded .deb (real name checked above)
    assert covered == set(kit_files), sorted(set(kit_files) - covered)


def test_collect_component_is_deterministic(tmp_path):
    entries, trees = [], []
    for name in ("a", "b"):
        base = tmp_path / name
        base.mkdir()
        image_root = base / "image"
        image_root.mkdir()
        files = make_fake_image(image_root, "edge-plugin-image")
        prepared = synthetic_prepared("edge-plugin-image", base)
        prepared.recorded_runtime_hash = expected_hash("edge-plugin-image", files)
        kit = base / "kit"
        kit.mkdir()
        entries.append(json.dumps(image_layer.collect_component(
            REPO_ROOT, kit, "edge-plugin-image", prepared, {"tag": "t", "recorded_digest": "sha256:" + "a" * 64},
            tag="t", run_id=name, copier_factory=fake_copier_factory(image_root)), sort_keys=True))
        trees.append(sorted((p.relative_to(kit).as_posix(), p.read_bytes(), p.stat().st_mode & 0o7777) for p in kit.rglob("*") if p.is_file()))
    assert entries[0] == entries[1] and trees[0] == trees[1]


def test_a_runtime_hash_one_hex_digit_off_is_refused_naming_both_and_writes_nothing(tmp_path):
    image_root = tmp_path / "image"
    image_root.mkdir()
    files = make_fake_image(image_root, "cfs-image")
    good = expected_hash("cfs-image", files)
    bad = good[:-1] + ("0" if good[-1] != "0" else "1")
    prepared = synthetic_prepared("cfs-image", tmp_path)
    prepared.recorded_runtime_hash = bad
    kit = tmp_path / "kit"
    kit.mkdir()
    with pytest.raises(image_layer.ImageLayerError) as exc:
        image_layer.collect_component(
            REPO_ROOT, kit, "cfs-image", prepared, {"tag": "t", "recorded_digest": "sha256:" + "a" * 64},
            tag="t", run_id="t", copier_factory=fake_copier_factory(image_root))
    assert good in str(exc.value) and bad in str(exc.value)
    assert not (kit / "images").exists()


def test_a_package_file_that_differs_from_the_deb_is_refused(tmp_path):
    image_root = tmp_path / "image"
    image_root.mkdir()
    files = make_fake_image(image_root, "edge-plugin-image", lib=b"a different libssl")
    prepared = synthetic_prepared("edge-plugin-image", tmp_path)
    prepared.recorded_runtime_hash = expected_hash("edge-plugin-image", files)
    kit = tmp_path / "kit"
    kit.mkdir()
    with pytest.raises(image_layer.ImageLayerError, match="differs from the .deb"):
        image_layer.collect_component(
            REPO_ROOT, kit, "edge-plugin-image", prepared, {"tag": "t", "recorded_digest": "sha256:" + "a" * 64},
            tag="t", run_id="t", copier_factory=fake_copier_factory(image_root))


def test_a_symlink_under_a_tree_is_refused_because_the_kit_cannot_recreate_it(tmp_path):
    image_root = tmp_path / "image"
    image_root.mkdir()
    files = make_fake_image(image_root, "cfs-image")
    (image_root / "cfs/cpu1/link").symlink_to("core-cpu1")
    prepared = synthetic_prepared("cfs-image", tmp_path)
    prepared.recorded_runtime_hash = expected_hash("cfs-image", files)
    kit = tmp_path / "kit"
    kit.mkdir()
    with pytest.raises(image_layer.ImageLayerError, match="symlink"):
        image_layer.collect_component(
            REPO_ROOT, kit, "cfs-image", prepared, {"tag": "t", "recorded_digest": "sha256:" + "a" * 64},
            tag="t", run_id="t", copier_factory=fake_copier_factory(image_root))


def test_a_missing_runtime_file_is_refused(tmp_path):
    image_root = tmp_path / "image"
    image_root.mkdir()
    make_fake_image(image_root, "cfs-image")
    (image_root / "cfs/av-lockstep-relay").unlink()
    prepared = synthetic_prepared("cfs-image", tmp_path)
    prepared.recorded_runtime_hash = "sha256:" + "0" * 64
    with pytest.raises(image_layer.ImageLayerError, match="not in the image"):
        image_layer.collect_component(
            REPO_ROOT, tmp_path / "kit", "cfs-image", prepared, {"tag": "t", "recorded_digest": "sha256:" + "a" * 64},
            tag="t", run_id="t", copier_factory=fake_copier_factory(image_root))


def _patch_docker(monkeypatch, tmp_path, ids: dict[str, str] | None = None):
    """collect_images with docker faked: availability, ids, the lock, and the image copier."""
    images = {}
    for component in sbom.IMAGE_COMPONENTS:
        root = tmp_path / f"img-{component}"
        root.mkdir()
        images[component] = (root, make_fake_image(root, component))
    recorded = {c: kmanifest.read_recorded_image(c) for c in sbom.IMAGE_COMPONENTS}
    by_tag = {recorded[c]["tag"]: c for c in sbom.IMAGE_COMPONENTS}
    monkeypatch.setattr(build_kit, "_docker_available", lambda: True)
    monkeypatch.setattr(build_kit, "_docker_image_id", lambda tag: (ids or {}).get(tag, recorded[by_tag[tag]]["recorded_digest"]))
    monkeypatch.setattr(build_kit, "_docker_image_config", lambda tag: image_layer.config_subset(
        {"Entrypoint": [f"/entry-of/{by_tag[tag]}"], "User": "u", "Env": ["PATH=/x"], "Hostname": "ignored"}))
    import altavista.docker_test_lock as lock_module
    monkeypatch.setattr(lock_module, "lock_docker_tests", lambda: nullcontext())
    monkeypatch.setattr(image_layer, "image_copier", lambda tag, run_id: fake_copier_factory(images[by_tag[tag]][0])(tag, run_id))
    prepared = {}
    for component in sbom.IMAGE_COMPONENTS:
        (tmp_path / f"prep-{component}").mkdir()
        prepared[component] = synthetic_prepared(component, tmp_path / f"prep-{component}")
        prepared[component].recorded_runtime_hash = expected_hash(component, images[component][1])
    return prepared, recorded, by_tag


def test_collect_images_refuses_an_image_id_that_is_not_the_recorded_one_before_reading_anything(tmp_path, monkeypatch):
    prepared, recorded, _ = _patch_docker(monkeypatch, tmp_path, ids={})
    tag = recorded["cfs-image"]["tag"]
    monkeypatch.setattr(build_kit, "_docker_image_id", lambda t: "sha256:" + "f" * 64)
    kit = tmp_path / "kit"
    kit.mkdir()
    with pytest.raises(RuntimeError, match="does NOT match its recorded digest"):
        build_kit.collect_images(REPO_ROOT, kit, prepared)
    assert not (kit / "images").exists()


def test_collect_images_end_to_end_with_docker_faked(tmp_path, monkeypatch):
    prepared, recorded, _ = _patch_docker(monkeypatch, tmp_path)
    kit = tmp_path / "kit"
    kit.mkdir()
    result = build_kit.collect_images(REPO_ROOT, kit, prepared)
    assert set(result) == set(sbom.IMAGE_COMPONENTS)
    for component, entry in result.items():
        assert entry["tag"] == recorded[component]["tag"] and entry["recorded_digest"] == recorded[component]["recorded_digest"]
        assert entry["base"]["reference"] == image_layer.base_pin_for(component)["reference"]
        assert entry["dockerfile"] == {"kit_path": f"images/{component}/Dockerfile"}
        assert (kit / "images" / component / "Dockerfile").is_file()
        # The installer's image step (image_build.py) reads the roots to hash and the .Config to compare from here.
        assert entry["runtime_set"] == image_layer.runtime_set_entry(component)
        assert entry["config"] == image_layer.config_subset({"Entrypoint": [f"/entry-of/{component}"], "User": "u", "Env": ["PATH=/x"]})
        assert set(entry["config"]) == set(image_layer.CONFIG_KEYS) and "Hostname" not in entry["config"]
    assert not list(kit.rglob("*.tar"))


def test_build_refuses_a_bad_deb_hash_before_anything_is_written_into_the_kit(tmp_path, monkeypatch):
    """The `.deb`'s recorded sha256 changed: `build()` refuses while preparing, so `--out` is never
    created. The download is faked to return real-looking bytes that cannot match."""
    record = image_layer.load_runtime_packages(REPO_ROOT, "edge-plugin-image")[0]
    wrong = dict(record, sha256=record["sha256"][:-1] + ("0" if record["sha256"][-1] != "0" else "1"))
    monkeypatch.setattr(image_layer, "load_runtime_packages", lambda root, c: [wrong] if c == "edge-plugin-image" else [])
    monkeypatch.setattr(image_layer, "_download", lambda url: fake_deb_bytes())
    out = tmp_path / "kit"
    with pytest.raises(image_layer.ImageLayerError, match="refusing it"):
        build_kit.build(repo_root=REPO_ROOT, out=out, site=REPO_ROOT / build_kit.DEFAULT_SITE, with_images=True)
    assert not out.exists()
    assert not (REPO_ROOT / image_layer.DEB_CACHE_DIR / wrong["sha256"]).exists()


def test_the_runtime_set_the_manifest_records_is_the_one_the_builder_hashes():
    """`image_build.py` (shipped in the kit) reads these roots out of the image it builds; they must be the
    builder's `RUNTIME_SETS`, which the recorded hashes were computed over."""
    for component, rset in image_layer.RUNTIME_SETS.items():
        assert image_layer.runtime_set_entry(component) == {"files": list(rset.files), "trees": list(rset.trees), "packages": list(rset.packages)}

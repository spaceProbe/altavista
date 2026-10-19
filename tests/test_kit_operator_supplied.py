"""Question 250(a), task 3: the installer verifies every operator-supplied component the kit needs and
prints what it found (`scripts/kit/operator_check.py`; `install.py` and `image_build.py` are its two
steps).

What is proved here, without Docker (a fake runner) and over synthetic trees:

1. the pins file covers all six components of `docs/kit-operator-supplied.md`, and the pins file, the
   doc's table and each service's own `IMAGE_DIGEST.md` agree on every digest (so the three records
   cannot drift apart);
2. what a kit needs is derived from `KIT_MANIFEST` in one place (`requirement_for`), and the build flag
   `--declare-operator-supplied` records a declaration there (and refuses ClickHouse, unknown names);
3. each component's verification is one function returning a typed result: a container image by digest,
   a tree by descriptor -- not required, required and verified, required and missing, required and
   different, a declared service image verified, a declared one missing or different;
4. both steps print all six lines, a refusal still prints the lines up to it, and the structured list
   goes into `INSTALL_RECORD` and `IMAGE_BUILD_RECORD`.

The GMAT tree check itself is tested in `tests/test_kit_install.py`, the image build in
`tests/test_kit_image_build.py`.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
KIT_DIR = REPO_ROOT / "scripts" / "kit"
sys.path.insert(0, str(KIT_DIR))
import build_kit  # noqa: E402
import image_build  # noqa: E402
import install  # noqa: E402
import manifest as kmanifest  # noqa: E402
import operator_check  # noqa: E402
import test_kit_image_build as kib  # noqa: E402  (the fake docker and the synthetic kit)
import test_kit_install as tki  # noqa: E402  (the synthetic GMAT tree and pins)

DOC = REPO_ROOT / "docs" / "kit-operator-supplied.md"
PINS = json.loads((KIT_DIR / "operator_supplied.json").read_text(encoding="utf-8"))["components"]
CFS, PLUGIN = kib.CFS, kib.PLUGIN

#: Pin id by doc item number, and the order every list is printed in.
ID_BY_ITEM = {1: "ubuntu-22.04-base-image", 2: "debian-bookworm-slim-base-image", 3: "gmat-r2026a",
              4: "clickhouse", 5: "minio", 6: "postgis", 7: "numpy-wheel-linux-aarch64"}
ORDER = [ID_BY_ITEM[i] for i in range(1, 8)]
SERVICES = ("clickhouse", "minio", "postgis")


# =================================================================================================
# 1. The pins file, the doc and the services' own digest records
# =================================================================================================

def test_the_pins_file_covers_the_six_components_of_the_doc_and_numpys_wheel_and_is_well_formed():
    assert {c["item"]: cid for cid, c in PINS.items()} == ID_BY_ITEM
    assert operator_check.pins_problems(PINS) == []
    for cid, pin in PINS.items():
        assert pin["licences"].strip() and pin["why"].strip()
        assert pin["verification"]["step"] == operator_check.STEP_OF_KIND[pin["kind"]], cid
    assert {PINS[c]["kind"] for c in SERVICES} == {"container-image"}
    assert PINS["gmat-r2026a"]["kind"] == "data-tree"
    assert [PINS[c]["verification"]["step"] for c in ORDER] == ["images", "images", "files", "images", "images", "images", "files"]
    assert PINS["numpy-wheel-linux-aarch64"]["kind"] == "python-wheel"   # item 7: the doc's row is task 6
    # only ClickHouse is held, and why (question 247)
    assert {c for c, p in PINS.items() if p.get("held")} == {"clickhouse"}
    assert PINS["clickhouse"]["held"]["question"] == 247
    assert operator_check.declarable(PINS) == ["minio", "postgis"]


def _doc_table_rows() -> dict[int, str]:
    """`{item number: the Pin cell}` of the doc's components table."""
    rows = {}
    for line in DOC.read_text(encoding="utf-8").splitlines():
        m = re.match(r"\|\s*(\d+)\s*\|", line)
        if m:
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            rows[int(m.group(1))] = cells[-1]
    return rows


def test_the_docs_table_has_the_eight_items():
    """The seven pinned components, and the Python version floor (item 8), which `install.sh` checks itself."""
    assert sorted(_doc_table_rows()) == [1, 2, 3, 4, 5, 6, 7, 8]


def test_the_docs_item_7_names_the_pinned_numpy_wheel_by_filename_and_sha256():
    pin = PINS["numpy-wheel-linux-aarch64"]
    cell = _doc_table_rows()[7]
    assert pin["filename"] in cell and pin["sha256"] in cell


def test_the_docs_item_8_is_the_python_floor_the_install_script_enforces():
    cell = _doc_table_rows()[8]
    assert "Python >= 3.11" in cell
    script = (KIT_DIR / "install.sh").read_text(encoding="utf-8")
    assert "sys.version_info >= (3, 11)" in script, "install.sh no longer enforces the floor the document states"


@pytest.mark.parametrize("item", [1, 2, 4, 5, 6])
def test_the_pins_file_and_the_docs_table_agree_on_every_image_digest(item):
    cid = ID_BY_ITEM[item]
    pin = PINS[cid]
    cell = _doc_table_rows()[item]
    in_doc = set(re.findall(r"sha256:[0-9a-f]{64}", cell))
    in_pins = {pin["digest"]} | ({pin["arm64_manifest_digest"]} if "arm64_manifest_digest" in pin else set())
    assert in_doc == in_pins, f"item {item} ({cid}): docs/kit-operator-supplied.md pins {sorted(in_doc)}, operator_supplied.json pins {sorted(in_pins)}"
    assert pin["reference"] in cell, f"item {item} ({cid}): the doc's pin cell does not contain the reference {pin['reference']}"


def test_the_pins_file_and_the_docs_item_3_agree_on_the_gmat_hash():
    after = DOC.read_text(encoding="utf-8").split("### Item 3: the GMAT pin", 1)[1]
    section = re.split(r"\n##+ ", after, maxsplit=1)[0]   # this heading's own section, up to the next heading
    # every pinned platform (question 254, task 12: macos-arm64 and linux-x86_64), every field of its pin,
    # not the hash alone, and the scope the pins were measured under
    pinned = {k: v for k, v in PINS["gmat-r2026a"]["platforms"].items() if v is not None}
    assert {"macos-arm64", "linux-x86_64"} <= set(pinned)
    for key, pin in pinned.items():
        assert f"`{key}`" in section, f"the document's GMAT section does not name the pinned platform {key}"
        assert pin["content_hash"] in section, key
        assert f"{pin['file_count']:,} files" in section, key
        assert f"{pin['total_bytes']:,} bytes" in section, key
        assert f"{pin['symlink_count']} symlinks" in section and pin["symlink_listing_hash"] in section, key
    scope = PINS["gmat-r2026a"]["scope"]
    for name in (*scope["exclude_paths"], *scope["exclude_dir_names"]):
        assert name in section, f"the document's GMAT section does not name the scope entry {name!r}"
    # the platforms without a pin are said to have none
    for key, value in PINS["gmat-r2026a"]["platforms"].items():
        if value is None:
            assert key in section, f"{key} has no pin and the document does not say so"
    # the whole-tree value of the first audit is explained as superseded, not left standing as the pin
    assert "47095a23" in section and "No pin yet" in section
    # the Linux archive each pin was measured from (question 254, task 12): the pins file records it, the document names it
    archives = PINS["gmat-r2026a"]["archives"]
    linux = archives["linux-x86_64"]
    assert linux["pinned"] is True and linux["file"] in section and linux["sha256"].removeprefix("sha256:") in section
    assert f"{linux['bytes']:,}" in section
    assert archives["linux-aarch64"]["published"] is False
    # question 257: every pinned archive's row in the Linux release table names its pin key in the Pin column,
    # and every variant table is described with its default, its variants and the option that chooses one
    rows = {r.split("|")[1].strip(): r.split("|")[-2].strip() for r in section.splitlines() if r.startswith("| `gmat-")}
    for key, archive in archives.items():
        if archive.get("pinned"):
            row = next(pin for name, pin in rows.items() if f"`{archive['file']}`" in name)
            assert row.startswith(f"`{key}`"), (key, row)
    for platform, table in (PINS["gmat-r2026a"].get("variants") or {}).items():
        assert "--gmat-variant" in section and f"`{table['default']}`" in section
        for name, key in table["pins"].items():
            assert f"`{name}`" in section and f"`{key}`" in section, (platform, name, key)


@pytest.mark.parametrize("cid", SERVICES)
def test_each_service_pin_equals_the_digest_in_the_services_own_record(cid):
    pin = PINS[cid]
    record = (REPO_ROOT / pin["source_record"]).read_text(encoding="utf-8")
    assert pin["reference"] in record, f"{pin['source_record']} does not contain the pinned reference {pin['reference']}"
    digests = set(re.findall(r"sha256:[0-9a-f]{64}", record))
    assert pin["digest"] in digests
    if "arm64_manifest_digest" in pin:
        assert pin["arm64_manifest_digest"] in digests
    # the digest in the reference is the digest field
    assert pin["reference"].endswith("@" + pin["digest"])


@pytest.mark.parametrize("cid", ["ubuntu-22.04-base-image", "debian-bookworm-slim-base-image"])
def test_each_base_pin_equals_the_docs_reference_and_the_from_line_of_its_kit_dockerfile(cid):
    pin = PINS[cid]
    for component in pin["base_of"]:
        kit_dockerfile = REPO_ROOT / kib.image_layer.IMAGE_SERVICE_DIRS[component] / "Dockerfile.kit"
        assert f"FROM {pin['reference']}" in kit_dockerfile.read_text(encoding="utf-8")


def test_the_pins_file_validator_names_what_is_wrong():
    bad = json.loads(json.dumps(PINS))
    bad["minio"]["digest"] = "sha256:short"
    bad["postgis"]["verification"]["step"] = "files"
    bad["gmat-r2026a"]["item"] = 5
    problems = operator_check.pins_problems(bad)
    assert any("minio.digest" in p for p in problems) and any("postgis.verification" in p for p in problems)
    assert any("also the item" in p for p in problems)
    assert operator_check.pins_problems({}) and operator_check.pins_problems(None)


# =================================================================================================
# 2. What a kit needs, derived from KIT_MANIFEST
# =================================================================================================

def _doc(*, images=(), packs=(), declared=()):
    return {
        "images": {c: {"collected": True} for c in images},
        "packs": {p: {} for p in packs},
        "operator_supplied_declared": list(declared),
    }


def _req(cid, doc, **kw):
    return operator_check.requirement_for(cid, PINS[cid], doc, **kw)


def test_a_base_is_required_for_each_image_the_kit_carries():
    doc = _doc(images=[CFS])
    assert _req("ubuntu-22.04-base-image", doc).required and "base of cfs-image" in _req("ubuntu-22.04-base-image", doc).text
    assert not _req("debian-bookworm-slim-base-image", doc).required
    both = _doc(images=[CFS, PLUGIN])
    assert _req("debian-bookworm-slim-base-image", both).required
    # an image the kit lists but did not collect (built without --with-images) needs nothing
    uncollected = {"images": {CFS: {"collected": False}}, "packs": {}, "operator_supplied_declared": []}
    assert not _req("ubuntu-22.04-base-image", uncollected).required
    # the image step's --component leaves one out
    left_out = _req("debian-bookworm-slim-base-image", both, images={CFS})
    assert not left_out.required and "--component leaves out edge-plugin-image" in left_out.text


def test_gmat_is_required_when_the_kit_has_packs_gmat():
    assert _req("gmat-r2026a", _doc(packs=["gmat"])).required
    assert _req("gmat-r2026a", _doc(packs=["gmat"])).text == "required (packs.gmat)"
    assert not _req("gmat-r2026a", _doc(packs=["data-time"])).required


def test_a_service_image_is_required_only_when_the_kit_declares_it():
    for cid in ("minio", "postgis"):
        none = _req(cid, _doc())
        assert not none.required and none.outside_kit and "--declare-operator-supplied" in none.why_not
        declared = _req(cid, _doc(declared=[cid]))
        assert declared.required and not declared.outside_kit
    held = _req("clickhouse", _doc())
    assert held.outside_kit and held.why_not == "not declared; held pending licence review, q247"
    assert _req("minio", _doc(declared=["postgis"])).required is False


def test_the_kit_manifest_records_the_declaration_and_defaults_to_empty(tmp_path):
    kit_root = tmp_path / "kit"
    kmanifest.write_manifest(kmanifest.build_manifest(
        kit_root=kit_root, git_commit="0" * 40, git_dirty=False, git_status=[], images={}, sboms={}, packs={},
        vendor={}, wheels={}, binaries={}, runs={}, gaps=[],
    ), kit_root)
    assert json.loads((kit_root / "KIT_MANIFEST").read_text())["operator_supplied_declared"] == []
    kit_root2 = tmp_path / "kit2"
    kmanifest.write_manifest(kmanifest.build_manifest(
        kit_root=kit_root2, git_commit="0" * 40, git_dirty=False, git_status=[], images={}, sboms={}, packs={},
        vendor={}, wheels={}, binaries={}, runs={}, gaps=[], operator_supplied_declared=["postgis", "minio", "minio"],
    ), kit_root2)
    assert json.loads((kit_root2 / "KIT_MANIFEST").read_text())["operator_supplied_declared"] == ["minio", "postgis"]


def _build(tmp_path: Path, **kwargs):
    return build_kit.build(repo_root=REPO_ROOT, out=tmp_path / "kit", site=REPO_ROOT / build_kit.DEFAULT_SITE, **kwargs)


def test_build_kit_declares_minio_and_postgis_and_a_default_kit_declares_nothing(tmp_path):
    manifest_path, _ = _build(tmp_path, declare_operator_supplied=["postgis", "minio"])
    doc = json.loads(manifest_path.read_text())
    assert doc["operator_supplied_declared"] == ["minio", "postgis"]
    assert kmanifest.verify_manifest(manifest_path.parent) == []
    assert "operator_check.py" in {Path(f["path"]).name for f in doc["files"] if f["role"] == "installer"}
    other = tmp_path / "other"
    other_manifest, _ = build_kit.build(repo_root=REPO_ROOT, out=other, site=REPO_ROOT / build_kit.DEFAULT_SITE)
    assert json.loads(other_manifest.read_text())["operator_supplied_declared"] == []


@pytest.mark.parametrize("name, needle", [
    ("clickhouse", "held out of the kit: held pending licence review (question 247)"),
    ("nonesuch", "not a component of the pins file"),
    ("gmat-r2026a", "derived from what the kit carries"),
    ("ubuntu-22.04-base-image", "derived from what the kit carries"),
])
def test_build_kit_refuses_to_declare_clickhouse_an_unknown_name_or_a_derived_component_before_writing(tmp_path, capsys, name, needle):
    out = tmp_path / "kit"
    with pytest.raises(kmanifest.OperatorSuppliedPackError, match="--declare-operator-supplied") as exc:
        build_kit.build(repo_root=REPO_ROOT, out=out, site=REPO_ROOT / build_kit.DEFAULT_SITE, declare_operator_supplied=[name])
    assert needle in str(exc.value) and not out.exists()
    rc = build_kit.main(["--out", str(out), "--declare-operator-supplied", name])
    err = capsys.readouterr().err
    assert rc == 2 and "REFUSED" in err and needle in err and not out.exists()


# =================================================================================================
# 3. The verification functions: typed results
# =================================================================================================

def _docker_with(images: dict):
    """A runner answering `docker image inspect <ref>` from `{ref: (Id, RepoDigests)}`."""
    calls = []

    def docker(args, input=None, timeout=None):
        args = list(args)
        calls.append(args)
        assert args[:2] == ["image", "inspect"], f"unexpected docker call {args}"
        if args[2] not in images:
            return subprocess.CompletedProcess(args, 1, b"", f"Error response from daemon: No such image: {args[2]}".encode())
        image_id, digests = images[args[2]]
        return subprocess.CompletedProcess(args, 0, json.dumps([{"Id": image_id, "RepoDigests": digests}]).encode(), b"")

    docker.calls = calls
    return docker


def _image_req(cid):
    return operator_check.Requirement(True, "required (test)")


@pytest.mark.parametrize("cid", ["ubuntu-22.04-base-image", "minio", "postgis", "clickhouse"])
def test_an_image_whose_id_is_the_pinned_digest_is_verified(cid):
    pin = PINS[cid]
    v = operator_check.verify_image(_docker_with({pin["reference"]: (pin["digest"], [])}), cid, pin, _image_req(cid))
    assert v.verified and v.outcome == "verified" and v.code is None
    assert v.expected == pin["digest"] and v.found == pin["digest"]
    assert v.detail["verified_by"] == "Id" and v.detail["reference"] == pin["reference"]


def test_an_image_found_by_repo_digests_the_classic_store_is_verified():
    pin = PINS["minio"]
    docker = _docker_with({pin["reference"]: ("sha256:" + "7" * 64, ["x/y@sha256:" + "1" * 64, pin["reference"]])})
    v = operator_check.verify_image(docker, "minio", pin, _image_req("minio"))
    assert v.verified and v.detail["verified_by"] == "RepoDigests" and v.found == "sha256:" + "7" * 64


@pytest.mark.parametrize("cid, code", [("ubuntu-22.04-base-image", "base-image-missing"), ("minio", "image-missing")])
def test_an_image_the_host_does_not_hold_is_a_refused_result_naming_reference_and_digest(cid, code):
    pin = PINS[cid]
    v = operator_check.verify_image(_docker_with({}), cid, pin, _image_req(cid))
    assert not v.verified and v.outcome == "refused" and v.code == code
    assert pin["reference"] in v.reason and pin["digest"] in v.reason and "docker load" in v.reason
    assert "Nothing was" in v.reason and "pulled" in v.reason


@pytest.mark.parametrize("cid, code", [("debian-bookworm-slim-base-image", "base-image-digest-differs"), ("postgis", "image-digest-differs")])
def test_an_image_with_another_digest_is_a_refused_result_naming_both(cid, code):
    pin = PINS[cid]
    docker = _docker_with({pin["reference"]: ("sha256:" + "9" * 64, ["other@sha256:" + "8" * 64])})
    v = operator_check.verify_image(docker, cid, pin, _image_req(cid))
    assert not v.verified and v.code == code
    assert pin["digest"] in v.reason and "9" * 64 in v.reason
    assert v.expected == pin["digest"] and v.found["Id"] == "sha256:" + "9" * 64


def test_the_printed_line_shapes():
    pin = PINS["ubuntu-22.04-base-image"]
    verified = operator_check.verify_image(_docker_with({pin["reference"]: (pin["digest"], [])}), "ubuntu-22.04-base-image", pin,
                                           operator_check.Requirement(True, "required (base of cfs-image)"))
    assert operator_check.render_line(verified) == (
        "operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  verified sha256:2edbbc5dc405… via Id")
    refused = operator_check.verify_image(_docker_with({}), "ubuntu-22.04-base-image", pin,
                                          operator_check.Requirement(True, "required (base of cfs-image)"))
    assert operator_check.render_line(refused) == (
        "operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  NOT VERIFIED [base-image-missing]")


# =================================================================================================
# 4. The files install: all six lines, GMAT in every state
# =================================================================================================

def _lines(out: list[str]) -> dict[str, str]:
    """`{component: the printed line}` of the `operator-supplied:` lines in `out`."""
    found = {}
    for line in out:
        m = re.match(r"operator-supplied: (\S+)  ", line)
        if m:
            found[m.group(1)] = line
    return found


@pytest.fixture()
def gmat_world(tmp_path, monkeypatch):
    """A synthetic GMAT tree with a pin for it (`tki`), `manifest.PINS_FILE` pointing at a pins file that has it."""
    tree = tki._make_tree(tmp_path / "gmat tree")
    pins_path = tmp_path / "pins.json"
    pins_path.write_text(json.dumps(tki._pins_doc(tree)), encoding="utf-8")
    monkeypatch.setattr(kmanifest, "PINS_FILE", pins_path)
    return tree, pins_path


def _kit_doc(*, images=(), gmat=False, declared=()):
    doc = _doc(images=images, declared=declared)
    if gmat:
        doc["packs"]["gmat"] = {"copied": False, "operator_supplied": {"component": "gmat-r2026a"}}
    return doc


def test_the_files_step_prints_every_line_when_nothing_is_needed():
    out: list[str] = []
    results = install.verify_operator_supplied(_kit_doc(), out=out.append)
    assert [r.component for r in results] == ORDER and len(out) == 7
    lines = _lines(out)
    assert list(lines) == ORDER
    assert lines["ubuntu-22.04-base-image"].endswith("not required (the kit carries no application layer for cfs-image), not verified")
    assert lines["gmat-r2026a"] == "operator-supplied: gmat-r2026a  not required (the kit has no packs.gmat), not verified"
    assert lines["clickhouse"] == "operator-supplied: clickhouse  outside the kit, not verified (not declared; held pending licence review, q247)"
    assert lines["minio"].startswith("operator-supplied: minio  outside the kit, not verified (not declared;")
    assert not any(r.verified for r in results)


def test_the_files_step_lists_images_as_the_image_steps_and_verifies_gmat(gmat_world):
    tree, _ = gmat_world
    out: list[str] = []
    results = install.verify_operator_supplied(
        _kit_doc(images=[CFS, PLUGIN], gmat=True, declared=["minio"]), gmat_root=str(tree), gmat_platform=tki.PLATFORM, out=out.append)
    lines = _lines(out)
    assert list(lines) == ORDER
    other = "verified by the image step, not here (record: IMAGE_BUILD_RECORD operator_supplied; this step does not read it)"
    assert lines["ubuntu-22.04-base-image"] == f"operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  {other}"
    assert lines["debian-bookworm-slim-base-image"].endswith(other) and "base of edge-plugin-image" in lines["debian-bookworm-slim-base-image"]
    assert lines["minio"].endswith(other) and "required (declared at build time: --declare-operator-supplied minio)" in lines["minio"]
    assert re.fullmatch(r"operator-supplied: gmat-r2026a  required \(packs\.gmat\)  verified testos-x1 sha256:[0-9a-f]{12}… \d+ files", lines["gmat-r2026a"])
    assert lines["postgis"].startswith("operator-supplied: postgis  outside the kit, not verified")
    verified = [r.component for r in results if r.verified]
    assert verified == ["gmat-r2026a"]
    assert [r.component for r in results if r.outcome == "other-step"] == [
        "ubuntu-22.04-base-image", "debian-bookworm-slim-base-image", "minio"]


def test_a_required_gmat_that_is_missing_is_refused_after_the_lines_up_to_it(gmat_world):
    out: list[str] = []
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_supplied(_kit_doc(images=[CFS], gmat=True), out=out.append)
    assert exc.value.code == "gmat-root-missing"
    assert list(_lines(out)) == ["ubuntu-22.04-base-image", "debian-bookworm-slim-base-image", "gmat-r2026a"]
    assert out[-1] == "operator-supplied: gmat-r2026a  required (packs.gmat)  NOT VERIFIED [gmat-root-missing]"


def test_a_required_gmat_that_differs_is_refused_and_the_list_says_so(gmat_world):
    tree, _ = gmat_world
    (tree / "README.txt").write_bytes(b"readmf\n")
    out: list[str] = []
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_supplied(_kit_doc(gmat=True), gmat_root=str(tree), gmat_platform=tki.PLATFORM, out=out.append)
    assert exc.value.code == "gmat-descriptor-differs" and "content_hash" in str(exc.value)
    assert _lines(out)["gmat-r2026a"].endswith("NOT VERIFIED [gmat-descriptor-differs]")
    assert list(_lines(out))[-1] == "gmat-r2026a"   # the lines after it were not printed


def test_a_kit_that_needs_gmat_with_a_pins_file_that_has_none_is_refused(tmp_path, monkeypatch):
    pins = json.loads((KIT_DIR / "operator_supplied.json").read_text())
    del pins["components"]["gmat-r2026a"]
    path = tmp_path / "pins.json"
    path.write_text(json.dumps(pins))
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_supplied(_kit_doc(gmat=True), pins_file=path)
    assert exc.value.code == "gmat-pin-malformed"


def test_a_malformed_pins_file_is_refused_not_listed(tmp_path):
    path = tmp_path / "pins.json"
    path.write_text(json.dumps({"components": {"minio": {"item": 5}}}))
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_supplied(_kit_doc(), pins_file=path)
    assert exc.value.code == "pins-malformed"
    path.write_text("{nope")
    with pytest.raises(install.OperatorSuppliedRefused) as exc:
        install.verify_operator_supplied(_kit_doc(), pins_file=path)
    assert exc.value.code == "pins-malformed"


def test_a_kit_declaring_clickhouse_or_an_unknown_component_is_refused_by_the_installer_too():
    for name in ("clickhouse", "nonesuch", "gmat-r2026a"):
        with pytest.raises(install.OperatorSuppliedRefused) as exc:
            install.verify_operator_supplied(_kit_doc(declared=[name]))
        assert exc.value.code == "declared-component-refused", name


def _install_kit(tmp_path, monkeypatch, *, gmat=False, declared=()):
    """The synthetic collected-images kit of test_kit_image_build, with `packs.gmat` and a declaration
    written into its KIT_MANIFEST; the wheel steps of `install.install` replaced as in test_kit_install."""
    kit = kib.make_kit(tmp_path)
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    packs = {"gmat": {"copied": False, "operator_supplied": {"component": "gmat-r2026a"}}} if gmat else {}
    kmanifest.write_manifest(kmanifest.build_manifest(
        kit_root=kit, git_commit=doc["git_commit"], git_dirty=doc["git_dirty"], git_status=doc["git_status"], images=doc["images"],
        sboms={}, packs=packs, vendor={"collected": False}, wheels={}, binaries={}, runs={}, gaps=[],
        operator_supplied_declared=list(declared),
    ), kit)
    assert kmanifest.verify_manifest(kit) == []
    monkeypatch.setattr(install, "_check_required_wheels", lambda doc: None)
    monkeypatch.setattr(install, "_check_viewer_assets_in_wheel", lambda kit_dir, doc: None)
    monkeypatch.setattr(install, "_install_wheels_into_venv", lambda kit_dir, target_dir: {"venv": "venv", "installed_packages": []})
    return kit


def test_install_prints_the_list_and_records_it_structured(tmp_path, monkeypatch, gmat_world, capsys):
    tree, _ = gmat_world
    kit = _install_kit(tmp_path, monkeypatch, gmat=True, declared=["minio"])
    target = tmp_path / "target"
    assert install.main([str(kit), str(target), "--gmat-root", str(tree), "--gmat-platform", tki.PLATFORM]) == 0
    out = capsys.readouterr().out
    assert list(_lines(out.splitlines())) == ORDER
    assert f"GMAT at {tree} verified against the {tki.PLATFORM} pin (not copied)" in out
    record = json.loads((target / "INSTALL_RECORD").read_text())
    os_rec = record["installed"]["operator_supplied"]
    assert os_rec["step"] == "files install" and os_rec["declared"] == ["minio"]
    comps = {c["component"]: c for c in os_rec["components"]}
    assert list(comps) == ORDER
    assert {c: comps[c]["outcome"] for c in ORDER} == {
        "ubuntu-22.04-base-image": "other-step", "debian-bookworm-slim-base-image": "other-step", "gmat-r2026a": "verified",
        "clickhouse": "outside-kit", "minio": "other-step", "postgis": "outside-kit",
        "numpy-wheel-linux-aarch64": "not-required"}
    assert comps["gmat-r2026a"]["verified"] and comps["gmat-r2026a"]["found"] == comps["gmat-r2026a"]["expected"]
    assert comps["minio"]["verifies_in"] == "image step" and comps["gmat-r2026a"]["verifies_in"] == "files install"
    assert os_rec["gmat"]["verified"] and os_rec["gmat"]["platform"] == tki.PLATFORM   # the earlier, detailed form is kept


def test_install_refuses_with_the_list_up_to_the_refusal_on_stdout_and_the_code_on_stderr(tmp_path, monkeypatch, gmat_world, capsys):
    kit = _install_kit(tmp_path, monkeypatch, gmat=True)
    target = tmp_path / "target"
    assert install.main([str(kit), str(target)]) == 2
    captured = capsys.readouterr()
    assert list(_lines(captured.out.splitlines())) == ["ubuntu-22.04-base-image", "debian-bookworm-slim-base-image", "gmat-r2026a"]
    assert "NOT VERIFIED [gmat-root-missing]" in captured.out
    assert "REFUSED" in captured.err and "gmat-root-missing" in captured.err and not target.exists()


def test_check_operator_supplied_verifies_and_prints_without_installing(tmp_path, monkeypatch, gmat_world, capsys):
    tree, _ = gmat_world
    kit = _install_kit(tmp_path, monkeypatch, gmat=True)
    assert install.main(["--check-operator-supplied", str(kit), "--gmat-root", str(tree), "--gmat-platform", tki.PLATFORM]) == 0
    out = capsys.readouterr().out
    assert list(_lines(out.splitlines())) == ORDER and "verified testos-x1" in out
    assert install.main(["--check-operator-supplied", str(kit)]) == 2
    assert "gmat-root-missing" in capsys.readouterr().err


def test_an_installer_refuses_a_kit_without_the_declaration_list(tmp_path, monkeypatch):
    kit = _install_kit(tmp_path, monkeypatch)
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    del doc["operator_supplied_declared"]
    (kit / "KIT_MANIFEST").write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
    with pytest.raises(install.InstallRefused, match="operator_supplied_declared"):
        install.install(kit, tmp_path / "target")


# =================================================================================================
# 5. The image step: bases and declared service images, by digest
# =================================================================================================

class ServiceDocker(kib.FakeDocker):
    """`FakeDocker` that also holds the service images in `services` (`{pin id: (Id, RepoDigests)}`)."""

    def __init__(self, kit, *, hold=("clickhouse", "minio", "postgis")):
        super().__init__(kit)
        for cid in hold:
            self.bases[PINS[cid]["reference"]] = (PINS[cid]["digest"], [PINS[cid]["reference"]])

    def inspected(self) -> list[str]:
        return [a[2] for a, _i in self.calls if a[:2] == ["image", "inspect"]]


def _declared_kit(tmp_path, declared, *, gmat=False):
    kit = kib.make_kit(tmp_path)
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    packs = {"gmat": {"copied": False, "operator_supplied": {"component": "gmat-r2026a"}}} if gmat else {}
    kmanifest.write_manifest(kmanifest.build_manifest(
        kit_root=kit, git_commit=doc["git_commit"], git_dirty=doc["git_dirty"], git_status=doc["git_status"], images=doc["images"],
        sboms={}, packs=packs, vendor={}, wheels={}, binaries={}, runs={}, gaps=[], operator_supplied_declared=declared,
    ), kit)
    return kit


def test_the_image_step_prints_every_line_and_verifies_only_the_bases_when_nothing_is_declared(tmp_path):
    kit = kib.make_kit(tmp_path)
    docker = ServiceDocker(kit)
    record, lines = kib.run_step(kit, docker, tmp_path)
    out = _lines(lines)
    assert list(out) == ORDER
    bases = PINS["ubuntu-22.04-base-image"], PINS["debian-bookworm-slim-base-image"]
    assert out["ubuntu-22.04-base-image"] == "operator-supplied: ubuntu-22.04-base-image  required (base of cfs-image)  verified sha256:2edbbc5dc405… via Id"
    assert out["debian-bookworm-slim-base-image"] == (
        "operator-supplied: debian-bookworm-slim-base-image  required (base of edge-plugin-image)  verified sha256:88200866dfff… via Id")
    assert out["gmat-r2026a"] == "operator-supplied: gmat-r2026a  not required (the kit has no packs.gmat), not verified"
    for cid in SERVICES:
        assert "outside the kit, not verified (not declared;" in out[cid]
    # the host is asked about the two bases and nothing else: the services are never touched
    assert sorted(docker.inspected()[:2]) == sorted(b["reference"] for b in bases)
    assert not {PINS[c]["reference"] for c in SERVICES} & set(docker.inspected())
    ops = record["operator_supplied"]
    assert ops["step"] == "image step" and ops["declared"] == [] and [c["component"] for c in ops["components"]] == ORDER
    assert record["images"][CFS]["base"]["verified_by"] == "Id"   # the earlier per-image form is kept
    assert json.loads((tmp_path / "IMAGE_BUILD_RECORD").read_text())["operator_supplied"] == ops


def test_the_image_step_lists_gmat_as_the_files_installs_when_the_kit_needs_it(tmp_path):
    kit = _declared_kit(tmp_path, [], gmat=True)
    _record, lines = kib.run_step(kit, ServiceDocker(kit), tmp_path)
    assert _lines(lines)["gmat-r2026a"] == (
        "operator-supplied: gmat-r2026a  required (packs.gmat)  verified by the files install, not here "
        "(record: INSTALL_RECORD installed.operator_supplied.components; this step does not read it)")


def test_a_declared_service_image_that_the_host_holds_is_verified_by_digest(tmp_path):
    kit = _declared_kit(tmp_path, ["minio"])
    docker = ServiceDocker(kit)
    record, lines = kib.run_step(kit, docker, tmp_path)
    out = _lines(lines)
    assert out["minio"] == (
        "operator-supplied: minio  required (declared at build time: --declare-operator-supplied minio)  verified sha256:a1ea29fa2835… via Id")
    assert PINS["minio"]["reference"] in docker.inspected()
    assert PINS["postgis"]["reference"] not in docker.inspected() and PINS["clickhouse"]["reference"] not in docker.inspected()
    minio = next(c for c in record["operator_supplied"]["components"] if c["component"] == "minio")
    assert minio["verified"] and minio["expected"] == PINS["minio"]["digest"] and minio["found"] == PINS["minio"]["digest"]
    assert record["operator_supplied"]["declared"] == ["minio"]
    # a verified declared image is not built, tagged or removed by this step
    assert [a for a, _i in docker.calls if a[0] in ("rmi", "tag", "pull", "load", "run", "start")] == []


def test_a_declared_service_image_the_host_does_not_hold_is_refused_before_any_build(tmp_path):
    kit = _declared_kit(tmp_path, ["minio"])
    docker = ServiceDocker(kit, hold=("postgis",))
    lines: list[str] = []
    with pytest.raises(image_build.OperatorImageMissing) as exc:
        image_build.build_images(docker, kit, out=lines.append, record_path=tmp_path / "rec")
    assert PINS["minio"]["reference"] in str(exc.value) and PINS["minio"]["digest"] in str(exc.value)
    assert "docker load" in str(exc.value) and exc.value.kind == "operator-image-missing"
    assert docker.called("build") == [] and docker.called("create") == [] and not (tmp_path / "rec").exists()
    # the two bases, GMAT and ClickHouse were printed, then the refused line, and nothing after it
    assert list(_lines(lines)) == ["ubuntu-22.04-base-image", "debian-bookworm-slim-base-image", "gmat-r2026a", "clickhouse", "minio"]
    assert lines[-1] == "operator-supplied: minio  required (declared at build time: --declare-operator-supplied minio)  NOT VERIFIED [image-missing]"


def test_a_declared_service_image_with_another_digest_is_refused(tmp_path):
    kit = _declared_kit(tmp_path, ["minio"])
    docker = ServiceDocker(kit)
    docker.bases[PINS["minio"]["reference"]] = ("sha256:" + "5" * 64, ["quay.io/minio/minio@sha256:" + "4" * 64])
    with pytest.raises(image_build.OperatorImageDigestDiffers) as exc:
        kib.run_step(kit, docker, tmp_path)
    assert PINS["minio"]["digest"] in str(exc.value) and "5" * 64 in str(exc.value)
    assert docker.called("build") == []


def test_the_run_exit_code_and_stderr_for_a_declared_image_that_is_missing(tmp_path, capsys):
    kit = _declared_kit(tmp_path, ["minio"])
    docker = ServiceDocker(kit, hold=())
    assert image_build.main(["--build-images", str(kit), "--record", str(tmp_path / "rec")], docker=docker) == 2
    captured = capsys.readouterr()
    assert "REFUSED [operator-image-missing]" in captured.err and PINS["minio"]["digest"] in captured.err
    assert "NOT VERIFIED [image-missing]" in captured.out


def test_a_base_that_is_missing_still_prints_the_lines_up_to_it_with_its_old_refusal_kind(tmp_path):
    kit = kib.make_kit(tmp_path)
    docker = ServiceDocker(kit)
    del docker.bases[PINS["debian-bookworm-slim-base-image"]["reference"]]
    lines: list[str] = []
    with pytest.raises(image_build.BaseImageMissing):
        image_build.build_images(docker, kit, out=lines.append, record_path=tmp_path / "rec")
    assert list(_lines(lines)) == ["ubuntu-22.04-base-image", "debian-bookworm-slim-base-image"]
    assert lines[-1].endswith("NOT VERIFIED [base-image-missing]")


def test_the_component_option_leaves_out_the_other_images_base(tmp_path):
    kit = kib.make_kit(tmp_path)
    docker = ServiceDocker(kit)
    _record, lines = kib.run_step(kit, docker, tmp_path, components=[CFS])
    out = _lines(lines)
    assert "verified sha256:2edbbc5dc405…" in out["ubuntu-22.04-base-image"]
    assert out["debian-bookworm-slim-base-image"].endswith("not required (--component leaves out edge-plugin-image), not verified")
    assert PINS["debian-bookworm-slim-base-image"]["reference"] not in docker.inspected()


def test_the_probe_prints_the_list_too(tmp_path):
    kit = kib.make_kit(tmp_path)
    lines: list[str] = []
    image_build.probe_offline_build(kib.ProbeDocker(kit, outcome=kib.GOOD), kit, out=lines.append)
    assert list(_lines(lines)) == ORDER


def test_the_image_step_refuses_a_kit_without_the_declaration_list_or_with_a_held_component(tmp_path):
    kit = kib.make_kit(tmp_path)
    doc = json.loads((kit / "KIT_MANIFEST").read_text())
    fresh = kmanifest.build_manifest(
        kit_root=kit, git_commit=doc["git_commit"], git_dirty=doc["git_dirty"], git_status=doc["git_status"], images=doc["images"],
        sboms={}, packs={}, vendor={}, wheels={}, binaries={}, runs={}, gaps=[],
    )
    del fresh["operator_supplied_declared"]   # a kit built before this task
    (kit / "KIT_MANIFEST").write_text(json.dumps(fresh, indent=2, sort_keys=True) + "\n")
    with pytest.raises(image_build.KitRefused, match="operator_supplied_declared"):
        kib.run_step(kit, kib.FakeDocker(kit), tmp_path)

    held = _declared_kit(tmp_path / "held", ["clickhouse"])
    with pytest.raises(image_build.KitRefused, match="declares operator-supplied 'clickhouse' is held out of the kit"):
        kib.run_step(held, ServiceDocker(held), tmp_path)


# =================================================================================================
# 6. Shipping
# =================================================================================================

def test_the_check_ships_in_every_kits_installer_and_imports_only_the_standard_library_and_manifest():
    assert "operator_check.py" in build_kit.INSTALLER_SOURCE_FILES
    imports = set(re.findall(r"^import (\w+)", (KIT_DIR / "operator_check.py").read_text(encoding="utf-8"), re.M))
    local = {"manifest"}
    assert (imports & {p.stem for p in KIT_DIR.glob("*.py")}) == local, imports

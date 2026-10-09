"""tests/test_zcu104_boot_recipe.py (docs/open-questions.md question 242 (d), task hilprep-5):

A fast, docker-free guard on third_party/zcu104-boot, the digest-pinned, byte-reproducible recipe for
the ZCU104 SD-card BOOT.BIN (FSBL + PMU firmware + the RTEMS lockstep cFS ELF on the R5 pair, made
with the open-source bootgen). In the manner of services/cfs/tests/test_build_elf_script.py it reads
the recipe and pins the invariants the two-build proof (recorded in the task report) was made with:

  - the builder image is named by content digest, every `docker run` goes through one function that
    passes `--rm` and the pinned image, and nothing builds, tags, commits, pulls or loads an image;
  - the network exists in one place: phases 2 and 3 are `--network none`, phase 1 is `none` unless
    the mode is `fetch` or BOOT_FETCH is set; the mounts are read-only except the one directory each
    phase writes;
  - every apt package is name=version with a recorded .deb hash, the installed set is hashed;
  - every fetched artifact (embeddedsw, bootgen, six tarballs, two toolchain trees, the RPU ELF) is
    pinned by commit and/or hash in pinned-inputs.sh, with no placeholder left;
  - SOURCE_DATE_EPOCH is pinned and exported to the builds, locale/zone/umask are fixed;
  - the .bif boots the FSBL on a53-0, the PMU firmware as `pmufw_image` and the RPU ELF with
    `destination_cpu = r5-lockstep`;
  - the RPU ELF's hash is checked before anything runs; non-shipping builds cannot land in the
    default output directory; nothing is written into third_party/cfs;
  - the structural checker reads a real `bootgen -read` capture (fixture) and fails on a
    partition that is not r5-lockstep.

It does not run docker and does not prove reproducibility -- that is the builds recorded in the
report. It only keeps the recipe those builds were made with from drifting.
"""
from __future__ import annotations

import importlib.util
import os
import re
import stat
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
RECIPE = REPO_ROOT / "third_party" / "zcu104-boot"
SCRIPT = RECIPE / "build-boot-bin.sh"
PINS = RECIPE / "pinned-inputs.sh"
PHASE1 = RECIPE / "container" / "phase1-fetch.sh"
PHASE2 = RECIPE / "container" / "phase2-toolchains.sh"
PHASE3 = RECIPE / "container" / "phase3-build.sh"
LIB = RECIPE / "container" / "lib.sh"
CHECKER = RECIPE / "container" / "structural-check.py"
FIXTURE = REPO_ROOT / "tests" / "fixtures" / "zcu104_boot" / "bootgen-read.txt"

DIGEST = r"sha256:[0-9a-f]{64}"
RPU_ELF_SHA256 = "de96907ff95fc8854723fbc71cd0c084483332c08984b60ec22ef923e7dacaf3"


def _code_lines(path: Path) -> list[str]:
    return [ln for ln in path.read_text().splitlines() if ln.strip() and not ln.lstrip().startswith("#")]


def _pin(name: str) -> str:
    m = re.search(rf'^{name}="([^"]*)"$', PINS.read_text(), re.MULTILINE)
    assert m, f"pinned-inputs.sh no longer assigns {name}=\"...\" on one line"
    return m.group(1)


def _array(name: str) -> list[str]:
    m = re.search(rf"^{name}=\(\n(.*?)^\)$", PINS.read_text(), re.MULTILINE | re.DOTALL)
    assert m, f"pinned-inputs.sh no longer defines the {name}=( ... ) array"
    return [t.strip('"') for t in m.group(1).split()]


def _run(args: list[str], env_extra: dict[str, str], cwd: Path = REPO_ROOT) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(("BOOT_", "CFS_", "ELF_"))}
    env.update(env_extra)
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=120)


def test_scripts_are_executable_bash_and_the_recipe_takes_the_docker_lock_itself() -> None:
    text = SCRIPT.read_text()
    assert text.startswith("#!/usr/bin/env bash\n")
    assert os.access(SCRIPT, os.X_OK), "build-boot-bin.sh must be executable"
    assert "shopt -qo posix" in text and '-z "${BASH_VERSION:-}"' in text
    assert "set -euo pipefail" in text
    assert "TAKES THE HOST-WIDE\n# DOCKER-TEST LOCK ITSELF" in text
    assert "DOES NOT TAKE" not in text
    # It re-executes under the committed helper, by path, through the repo's .venv python, after its
    # refusals (which need no docker) and before anything creates a directory or runs docker.
    exec_line = 'exec "${LOCK_PY}" "${REPO_ROOT}/scripts/dev/docker-lock-run.py" -- "${BASH_SOURCE[0]}" "$@"'
    assert text.count(exec_line) == 1
    assert '[ -z "${AV_DOCKER_LOCK_HELD:-}" ]' in text
    assert 'LOCK_PY="${REPO_ROOT}/.venv/bin/python"' in text
    pos = text.index(exec_line)
    assert text.index("non-shipping build") < pos < text.index('mkdir -p "${BOOT_CACHE_DIR}"') < text.index("docker_base()")
    assert text.index("BOOT_STAGE_DIR exists and is not empty") < pos
    assert (REPO_ROOT / "scripts" / "dev" / "docker-lock-run.py").stat().st_mode & stat.S_IXUSR
    helper = (REPO_ROOT / "scripts" / "dev" / "docker-lock-run.py").read_text()
    assert "from altavista.docker_test_lock import" in helper and "with lock_docker_tests():" in helper
    for p in (PHASE1, PHASE2, PHASE3):
        assert p.read_text().startswith("#!/bin/bash\n")
        assert "set -euo pipefail" in p.read_text()
        assert p.stat().st_mode & stat.S_IXUSR


def test_builder_image_pinned_by_digest_and_every_docker_run_goes_through_one_function() -> None:
    assert re.fullmatch(rf"debian:bookworm-slim@{DIGEST}", _pin("BASE_IMAGE"))
    code = _code_lines(SCRIPT)
    joined = "\n".join(code)
    runs = [ln for ln in code if re.search(r"\bdocker run\b", ln)]
    assert len(runs) == 1, runs
    assert "--rm" in runs[0] and '--network "${net}"' in runs[0]
    # Every phase passes the pinned image variable, never a literal name.
    assert joined.count('"${BASE_IMAGE}"') == 3
    for ln in code:
        assert not re.search(r"\bdebian(:|@)", ln) or ln.startswith(("BASE_IMAGE=", "echo")), f"literal image: {ln}"
    assert not re.search(r"\bdocker (tag|commit|build|load|pull|image|system|volume|save|import)\b", joined)
    # Containers are only ever removed by name.
    assert re.search(r"docker rm -f ", joined)


def test_network_exists_only_in_the_fetch_window_and_mounts_are_read_only_where_they_can_be() -> None:
    code = "\n".join(_code_lines(SCRIPT))
    # Phase 2 and 3: always `none`.
    assert re.search(r'docker_base "\$\{cname\}" none ', code)
    assert re.search(r'docker_base "\$\{CONTAINER_PREFIX\}-3" none', code)
    # Phase 1: `none` unless fetch mode or BOOT_FETCH.
    assert 'local net="none"' in code
    assert 'if [ "${MODE}" = "fetch" ] || [ -n "${BOOT_FETCH:-}" ]; then net="bridge"; fi' in code
    assert code.count('net="bridge"') == 1
    # The recipe itself is read-only in every container.
    assert '-v "${HERE}":/recipe:ro' in code
    # Phase 2: cache inputs read-only; only the toolchain tree is writable.
    assert '-v "${BOOT_CACHE_DIR}/apt":/cache/apt:ro -v "${BOOT_CACHE_DIR}/tarballs":/cache/tarballs:ro' in code
    assert '-v "${BOOT_CACHE_DIR}/toolchain":/opt/zcu104-tc \\' in code
    # Phase 3: sources, debs, toolchains and the staged inputs are read-only; /out is the only output.
    assert '-v "${BOOT_CACHE_DIR}/src":/cache/src:ro' in code
    assert '-v "${BOOT_CACHE_DIR}/toolchain":/opt/zcu104-tc:ro' in code
    assert '-v "${BOOT_STAGE_DIR}":/stage:ro -v "${BOOT_OUT_DIR}":/out' in code
    # The only network fetches live in phase 1.
    for p in (PHASE2, PHASE3, LIB):
        txt = "\n".join(_code_lines(p))
        assert not re.search(r"\b(curl|wget|git fetch|git clone|apt-get (update|install))\b", txt), p.name
    assert "apt-get install -y --no-install-recommends --download-only" in PHASE1.read_text()
    assert PHASE1.read_text().count("curl -fsSL") == 1


def test_apt_packages_pinned_to_versions_with_deb_hashes_and_the_set_is_checked() -> None:
    pkgs = _array("APT_PACKAGES")
    assert len(pkgs) >= 90, len(pkgs)
    names = {}
    for tok in pkgs:
        m = re.fullmatch(r"([a-z0-9][a-z0-9+.\-]*)=(\d[\w.+~:\-]*)", tok)
        assert m, f"apt entry {tok!r} is not name=version"
        names[m.group(1)] = m.group(2)
    assert len(names) == len(pkgs), "duplicate package in APT_PACKAGES"
    # What the recipe uses is in the closure: compilers, make, git, curl, python3, openssl headers.
    assert {"build-essential", "make", "gcc", "g++", "git", "curl", "python3", "libssl-dev", "xz-utils", "m4", "patch"} <= set(names)
    assert re.fullmatch(r"[0-9a-f]{64}", _pin("DPKG_SET_SHA256"))
    # Every pinned package has a recorded .deb hash and nothing else is in the file.
    sums = (RECIPE / "apt-debs.sha256").read_text().splitlines()
    assert len(sums) == len(pkgs)
    files = {}
    for ln in sums:
        h, f = ln.split()
        assert re.fullmatch(r"[0-9a-f]{64}", h)
        files[f.split("_")[0]] = f
    assert set(files) == set(names), set(files) ^ set(names)
    # Phase 1 downloads exactly the array and checks it against the committed hashes.
    p1 = PHASE1.read_text()
    assert '"${APT_PACKAGES[@]}"' in p1 and "sha256sum -c --quiet /recipe/apt-debs.sha256" in p1
    # Later phases install only those files and then compare the complete dpkg set.
    lib = LIB.read_text()
    assert "dpkg -i /cache/apt/*.deb" in lib and "dpkg-query -W" in lib
    assert re.search(r'if \[ "\$\{got\}" != "\$\{DPKG_SET_SHA256\}" \]; then\n\s+echo [^\n]*\n\s+exit 3', lib)


def test_every_fetched_artifact_is_pinned_by_commit_and_hash_with_no_placeholder() -> None:
    pins = PINS.read_text()
    assert "PIN_" not in pins, "a placeholder pin is still in pinned-inputs.sh"
    for name in ("EMBEDDEDSW", "BOOTGEN"):
        assert re.fullmatch(r"[0-9a-f]{40}", _pin(f"{name}_COMMIT")), name
        assert re.fullmatch(r"[0-9a-f]{64}", _pin(f"{name}_TREE_SHA256")), name
        assert _pin(f"{name}_URL").startswith("https://github.com/Xilinx/")
    # The release the commits belong to is the documented one.
    assert "xilinx_v2024.2" in pins
    tarballs = _array("TOOLCHAIN_TARBALLS")
    assert len(tarballs) == 6
    for entry in tarballs:
        name, url, sha = entry.split("|")
        assert url.startswith("https://") and url.endswith(name), entry
        assert re.fullmatch(r"[0-9a-f]{64}", sha), entry
    assert {e.split("|")[0].split("-")[0] for e in tarballs} == {"binutils", "gcc", "newlib", "gmp", "mpfr", "mpc"}
    for name in ("AARCH64_TOOLCHAIN_MANIFEST_SHA256", "MICROBLAZE_TOOLCHAIN_MANIFEST_SHA256"):
        assert re.fullmatch(r"[0-9a-f]{64}", _pin(name)), name
    assert _pin("RPU_ELF_SHA256") == RPU_ELF_SHA256
    # Phase 1 checks tree and tarball hashes and refuses on mismatch.
    p1 = PHASE1.read_text()
    assert "!= pinned ${want}" in p1 and "exit 4" in p1
    assert 'git -c protocol.version=2 fetch -q --depth 1 origin "${commit}"' in p1
    assert 'rev-parse HEAD)" = "${commit}"' in p1


def test_source_date_epoch_pinned_exported_and_environment_fixed() -> None:
    sde = _pin("PINNED_SOURCE_DATE_EPOCH")
    assert re.fullmatch(r"\d{10}", sde)
    assert sde == "1791331200"  # 2026-10-07 00:00:00 UTC
    for p in (PHASE2, PHASE3):
        txt = p.read_text()
        assert re.search(r"^export SOURCE_DATE_EPOCH", txt, re.MULTILINE), p.name
    assert '-e "SOURCE_DATE_EPOCH=${SDE}"' in SCRIPT.read_text()
    lib = LIB.read_text()
    assert "export LC_ALL=C TZ=UTC" in lib and "umask 022" in lib
    # Builds run at fixed container paths, so host paths cannot reach an artifact.
    assert "cp -a /cache/src/embeddedsw /work/embeddedsw" in PHASE3.read_text()
    assert 'PREFIX="/opt/zcu104-tc/${TC_TARGET}"' in PHASE2.read_text()


def test_bif_boots_fsbl_on_a53_pmufw_and_the_rpu_elf_in_lockstep() -> None:
    p3 = PHASE3.read_text()
    m = re.search(r"cat >/out/boot\.bif <<EOF\n(.*?)\nEOF\n", p3, re.DOTALL)
    assert m, "phase3-build.sh no longer writes /out/boot.bif"
    bif = m.group(1)
    assert "[bootloader, destination_cpu = a53-0] /out/fsbl.elf" in bif
    assert "[pmufw_image] /out/pmufw.elf" in bif
    assert "[destination_cpu = r5-lockstep] /stage/${RPU_ELF_NAME}" in bif
    assert "r5-single" not in bif and "r5-0" not in bif and "r5-1" not in bif
    assert '-arch zynqmp -image /out/boot.bif -o "/out/${BOOT_BIN_NAME}" -w' in p3
    assert "bootgen}" in p3 or "${BOOTGEN}" in p3
    assert "-arch zynqmp -read" in p3
    assert "structural-check.py" in p3 and '[ "${SC}" = PASS ]' in p3


def test_standin_image_cannot_be_mistaken_for_a_zcu104_image() -> None:
    text = SCRIPT.read_text()
    assert 'BOOT_BIN_NAME="BOOT.standin-zcu102-psuinit.bin"' in text
    assert 'BOOT_BIN_NAME="BOOT.BIN"' in text
    # BOOT.BIN is only ever written in supplied mode.
    m = re.search(r"if \[ -n \"\$\{BOOT_PSU_INIT_DIR:-\}\" \].*?PSU_INIT_MODE=supplied\n\s+BOOT_BIN_NAME=\"BOOT\.BIN\"", text, re.DOTALL)
    assert m
    assert "NOT for a ZCU104" in text


def test_rpu_elf_hash_checked_before_any_docker_command_and_refusals_need_no_docker(tmp_path: Path) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    marker = tmp_path / "docker-called"
    fake_docker = fake_bin / "docker"
    fake_docker.write_text(f"#!/bin/sh\necho called >> {marker}\nexit 99\n")
    fake_docker.chmod(0o755)
    home = Path(os.environ["HOME"])
    env = {"PATH": f"{fake_bin}:{os.environ['PATH']}"}
    wrong_elf = tmp_path / "core-cpu1.exe"
    wrong_elf.write_bytes(b"not the pinned elf")

    r = _run([str(SCRIPT), "build"], {**env, "BOOT_RPU_ELF": str(wrong_elf)})
    assert r.returncode == 2 and "RPU ELF sha256" in r.stderr and RPU_ELF_SHA256 in r.stderr, r.stderr
    r = _run([str(SCRIPT), "build"], {**env, "BOOT_RPU_ELF": str(tmp_path / "missing.exe")})
    assert r.returncode == 2 and "no RPU ELF" in r.stderr, r.stderr

    # A non-default build date cannot land in the default output directory.
    r = _run([str(SCRIPT), "fetch"], {**env, "BOOT_SOURCE_DATE_EPOCH": "1"})
    assert r.returncode == 2 and "non-shipping" in r.stderr, r.stderr
    # The staging directory must be under $HOME (Colima mounts only $HOME).
    r = _run([str(SCRIPT), "fetch"], {**env, "BOOT_STAGE_DIR": "/var/tmp/zcu104-stage"})
    assert r.returncode == 2 and "must be under $HOME" in r.stderr, r.stderr
    # A non-empty staging directory is refused.
    stage = home / f".zcu104-pytest-stage-{os.getpid()}"
    try:
        stage.mkdir()
        (stage / "x").write_text("x")
        r = _run([str(SCRIPT), "fetch"], {**env, "BOOT_STAGE_DIR": str(stage)})
        assert r.returncode == 2 and "not empty" in r.stderr, r.stderr
    finally:
        (stage / "x").unlink(missing_ok=True)
        stage.rmdir()
    # The two psu_init variables go together.
    r = _run([str(SCRIPT), "fetch"], {**env, "BOOT_PSU_INIT_DIR": str(tmp_path)})
    assert r.returncode == 2 and "go together" in r.stderr, r.stderr
    # Nothing is ever written into third_party/cfs.
    r = _run([str(SCRIPT), "fetch"], {**env, "BOOT_OUT_DIR": str(REPO_ROOT / "third_party" / "cfs" / "out")})
    assert r.returncode == 2 and "third_party/cfs" in r.stderr, r.stderr
    assert not marker.exists(), "a refusal ran docker"


def test_print_toolchain_hash_is_a_manifest_hash_of_a_tree(tmp_path: Path) -> None:
    (tmp_path / "bin").mkdir()
    (tmp_path / "bin" / "tool").write_text("x")
    (tmp_path / "bin" / "tool").chmod(0o755)
    (tmp_path / "link").symlink_to("bin/tool")
    h1 = _run([str(SCRIPT), "--print-toolchain-hash", str(tmp_path)], {})
    assert h1.returncode == 0 and re.fullmatch(r"[0-9a-f]{64}\n", h1.stdout), h1
    (tmp_path / "bin" / "tool").chmod(0o644)  # the executable bit is part of the manifest
    h2 = _run([str(SCRIPT), "--print-toolchain-hash", str(tmp_path)], {})
    assert h2.stdout != h1.stdout


def test_recipe_writes_nowhere_near_third_party_cfs_or_the_main_tree() -> None:
    code = "\n".join(_code_lines(SCRIPT))
    assert "third_party/cfs" in code  # only in the refusal
    refusal = re.search(r'case "\$\{BOOT_CACHE_DIR\}\$\{BOOT_OUT_DIR\}" in\n(.*?)esac', code, re.DOTALL)
    assert refusal and "third_party/cfs" in refusal.group(1) and "exit 2" in refusal.group(1)
    for p in (SCRIPT, PHASE1, PHASE2, PHASE3, LIB):
        txt = "\n".join(_code_lines(p))
        assert "mirrors" not in txt and "rtems-container/work" not in txt, p.name
    assert (RECIPE / ".gitignore").read_text().split() == ["cache/", "output/"]


def _load_checker():
    spec = importlib.util.spec_from_file_location("zcu104_structural_check", CHECKER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_structural_checker_reads_a_real_bootgen_capture() -> None:
    chk = _load_checker()
    bg = chk.parse_bootgen_read(FIXTURE.read_text())
    assert bg["header"]["fsbl_exec_address"] == 0xFFFC0000
    assert bg["header"]["pmufw_length"] == 0x1FAE0
    parts = bg["partitions"]
    assert [p["name"] for p in parts] == ["fsbl.elf.0", "core-cpu1.exe.0", "core-cpu1.exe.1"]
    assert parts[0]["attrs"]["core"] == "a53-0" and parts[0]["attrs"]["exec_state"] == "aarch-64"
    for p in parts[1:]:
        assert p["attrs"]["core"] == "r5-lockstep" and p["attrs"]["exec_state"] == "aarch-32"
    assert (parts[1]["load_addr_lo"], parts[1]["exec_addr_lo"], parts[1]["unencrypted_length"] * 4) == (0, 0x40, 960)
    assert (parts[2]["load_addr_lo"], parts[2]["unencrypted_length"] * 4) == (0x40000000, 719840)


def test_structural_checker_fails_when_the_rpu_is_not_in_lockstep(tmp_path: Path, monkeypatch) -> None:
    chk = _load_checker()
    good = FIXTURE.read_text()
    bad = good.replace("core [r5-lockstep]", "core [r5-0]       ")
    assert bad != good

    # Stand-in readelf results for the three ELFs the real build produced.
    infos = {
        "fsbl": {"entry": 0xFFFC0000, "machine": "AArch64", "class": "ELF64",
                 "loads": [{"offset": 0x1000, "vaddr": 0xFFFC0000, "paddr": 0xFFFC0000, "filesz": 0x20000, "memsz": 0x20000},
                           {"offset": 0x21000, "vaddr": 0xFFFE0000, "paddr": 0xFFFE0000, "filesz": 0x15C8, "memsz": 0x7BD0}]},
        "pmufw": {"entry": 0xFFDD0C38, "machine": "Xilinx MicroBlaze", "class": "ELF32",
                  "loads": [{"offset": 0, "vaddr": 0xFFDC0000, "paddr": 0xFFDC0000, "filesz": 0x16708, "memsz": 0x1A440},
                            {"offset": 0, "vaddr": 0xFFDDA440, "paddr": 0xFFDDA440, "filesz": 0x91C, "memsz": 0x1920},
                            {"offset": 0, "vaddr": 0xFFDDF6E0, "paddr": 0xFFDDF6E0, "filesz": 0x400, "memsz": 0x400}]},
        "rpu": {"entry": 0x40, "machine": "ARM", "class": "ELF32",
                "loads": [{"offset": 0x1000, "vaddr": 0, "paddr": 0, "filesz": 0x3C0, "memsz": 0x20000},
                          {"offset": 0x2000, "vaddr": 0x40000000, "paddr": 0x40000000, "filesz": 0xAFBE0, "memsz": 0x20000000}]},
    }
    monkeypatch.setattr(
        chk, "elf_info", lambda tool, path: infos["fsbl" if "fsbl" in path else "pmufw" if "pmufw" in path else "rpu"]
    )

    def run(text: str) -> int:
        f = tmp_path / "read.txt"
        f.write_text(text)
        monkeypatch.setattr("sys.argv", ["x", "--bootgen-read", str(f), "--fsbl", "fsbl.elf", "--pmufw", "pmufw.elf",
                                         "--rpu", "core-cpu1.exe", "--readelf", "r", "--mb-readelf", "m"])
        return chk.main()

    assert run(good) == 0
    assert run(bad) == 1


def test_output_pins_exist_and_phase3_enforces_them_for_the_shipping_build_only() -> None:
    for name in ("PMUFW_ELF_SHA256", "STANDIN_FSBL_ELF_SHA256", "STANDIN_BOOT_BIN_SHA256"):
        assert re.fullmatch(r"[0-9a-f]{64}", _pin(name)), name
    p3 = PHASE3.read_text()
    assert '[ "${SOURCE_DATE_EPOCH}" = "${PINNED_SOURCE_DATE_EPOCH}" ] && [ "${PIN_CHECK:-1}" = 1 ]' in p3
    assert 'check_pin pmufw.elf /out/pmufw.elf "${PMUFW_ELF_SHA256}"' in p3
    assert 'check_pin fsbl.elf /out/fsbl.elf "${STANDIN_FSBL_ELF_SHA256}"' in p3
    assert '"${STANDIN_BOOT_BIN_SHA256}"' in p3
    assert 'exit 6' in p3
    # The host passes PIN_CHECK=0 for every non-shipping build.
    assert '-e "PIN_CHECK=$([ -z "${NONSHIPPING}" ] && echo 1 || echo 0)"' in SCRIPT.read_text()


def test_microblaze_manifest_leaves_out_exactly_its_target_libraries(tmp_path: Path) -> None:
    assert _pin("MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE") == "microblazeel-xilinx-elf/lib/"
    tree = tmp_path / "tc"
    (tree / "bin").mkdir(parents=True)
    (tree / "microblazeel-xilinx-elf" / "lib").mkdir(parents=True)
    (tree / "bin" / "gcc").write_text("compiler")
    (tree / "microblazeel-xilinx-elf" / "lib" / "libc.a").write_text("one")
    base = _run([str(SCRIPT), "--print-toolchain-hash", str(tree), "microblazeel-xilinx-elf/lib/"], {}).stdout
    (tree / "microblazeel-xilinx-elf" / "lib" / "libc.a").write_text("two")
    assert _run([str(SCRIPT), "--print-toolchain-hash", str(tree), "microblazeel-xilinx-elf/lib/"], {}).stdout == base
    assert _run([str(SCRIPT), "--print-toolchain-hash", str(tree)], {}).stdout != base
    (tree / "bin" / "gcc").write_text("another compiler")
    assert _run([str(SCRIPT), "--print-toolchain-hash", str(tree), "microblazeel-xilinx-elf/lib/"], {}).stdout != base


def test_a_non_default_fsbl_define_is_a_non_shipping_build(tmp_path: Path) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    (fake_bin / "docker").write_text("#!/bin/sh\nexit 99\n")
    (fake_bin / "docker").chmod(0o755)
    r = _run([str(SCRIPT), "fetch"], {"PATH": f"{fake_bin}:{os.environ['PATH']}", "BOOT_FSBL_DEFINE": "FSBL_DEBUG"})
    assert r.returncode == 2 and "non-shipping" in r.stderr and "BOOT_FSBL_DEFINE" in r.stderr, r.stderr


def test_the_lock_helper_holds_the_repos_docker_lock_with_its_sidecar_and_a_second_holder_blocks(tmp_path: Path) -> None:
    import fcntl
    import time

    helper = REPO_ROOT / "scripts" / "dev" / "docker-lock-run.py"
    home = tmp_path / "home"
    home.mkdir()
    env = {**os.environ, "HOME": str(home)}
    env.pop("AV_DOCKER_LOCK_HELD", None)
    lock_file = home / ".altavista" / "locks" / "docker-tests.lock"
    sidecar = Path(str(lock_file) + ".holder")
    ready = tmp_path / "ready"
    child = (
        "import os, pathlib, time\n"
        f"pathlib.Path({str(ready)!r}).write_text(os.environ.get('AV_DOCKER_LOCK_HELD', ''))\n"
        "time.sleep(4)\n"
    )
    first = subprocess.Popen([os.sys.executable, str(helper), "--", os.sys.executable, "-c", child], env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        for _ in range(100):
            if ready.exists():
                break
            time.sleep(0.1)
        assert ready.exists(), "the helper never ran its command"
        # The sidecar names the helper process, and the child was told who holds the lock.
        record = dict(ln.split("=", 1) for ln in sidecar.read_text().splitlines() if "=" in ln)
        assert int(record["pid"]) == first.pid
        assert ready.read_text() == str(first.pid)
        # The flock is really held: a second, independent holder cannot take it without blocking.
        fd = os.open(lock_file, os.O_RDWR)
        try:
            blocked = False
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                blocked = True
            assert blocked, "the lock was not held while the command ran"
        finally:
            os.close(fd)
        # A second helper waits, announcing the holder, and runs after the first has finished.
        second = subprocess.run([os.sys.executable, str(helper), "--", "/bin/echo", "second-ran"], env=env,
                                capture_output=True, text=True, timeout=60)
        assert second.returncode == 0 and "second-ran" in second.stdout
        assert "WAITING for the docker-test lock" in second.stderr and f"pid={first.pid}" in second.stderr, second.stderr
    finally:
        first.wait(timeout=30)
    assert first.returncode == 0
    assert not sidecar.exists(), "the sidecar outlived the holder"
    # The command's exit status is the helper's.
    r = subprocess.run([os.sys.executable, str(helper), "--", "/bin/sh", "-c", "exit 7"], env=env, capture_output=True, text=True, timeout=60)
    assert r.returncode == 7

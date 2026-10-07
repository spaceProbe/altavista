"""services/cfs/tests/test_build_elf_script.py (docs/open-questions.md questions 171/239):

A fast, docker-free guard on third_party/rtems-container/build-elf.sh, the reproducible cross-build
of the RTEMS 6.1 `zynqmp_rpu_lock_step` cFS ELF (core-cpu1.exe). The script makes two clean builds
from different host staging paths byte-identical by pinning or erasing every input that reaches the
ELF. This test reads the script and pins those invariants, in the manner of
test_build_shim_script.py, so a later edit cannot quietly drop one:

  - the builder image is named by content digest, every `docker run` takes it from that pinned
    variable, the container is `--rm`, and the script builds, tags, commits and loads nothing;
  - every apt package is `name=version`, the pinned closure covers the toolchain-facing packages,
    and the container checks the full `dpkg-query` set against a pinned SHA-256;
  - the toolchain is mounted read-only, its manifest hash is pinned, recomputed before any staging,
    and a mismatch refuses unless the explicit override variable is set (behaviour test, no docker);
  - cFE's BUILDDATE, HOSTNAME and USER (the variables generate_build_env.cmake reads) are pinned
    and passed into the container;
  - the rtems-syms temporary-name fix (`-S <TARGET>-dl-sym.c`) is applied to the STAGED RTEMS.cmake,
    idempotently, with a control-only switch that cannot write the default output;
  - nothing is written into third_party/cfs: the cFS tree is fetched into the staging directory
    only, and the only writable mount is that staging directory.

It does not run docker and it does not prove reproducibility -- that proof is two real builds,
recorded in the question 239 report. It only keeps the recipe the proof was made with from drifting.
"""
from __future__ import annotations

import hashlib
import os
import re
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "third_party" / "rtems-container" / "build-elf.sh"
CROSS = REPO_ROOT / "third_party" / "rtems-container" / "build-cfs-cross.sh"

DIGEST = r"sha256:[0-9a-f]{64}"


def _script_text() -> str:
    return SCRIPT.read_text()


def _code_lines() -> list[str]:
    """The script's non-comment, non-blank lines (comments mention tags and paths freely)."""
    return [
        ln
        for ln in _script_text().splitlines()
        if ln.strip() and not ln.lstrip().startswith("#")
    ]


def _assignment(name: str) -> str:
    m = re.search(rf'^{name}="([^"]*)"$', _script_text(), re.MULTILINE)
    assert m, f"build-elf.sh no longer assigns {name}=\"...\" on one line"
    return m.group(1)


def _apt_packages() -> list[str]:
    m = re.search(r"^APT_PACKAGES=\(\n(.*?)^\)$", _script_text(), re.MULTILINE | re.DOTALL)
    assert m, "build-elf.sh no longer defines the APT_PACKAGES=( ... ) array"
    return m.group(1).split()


def _run(args: list[str], env_extra: dict[str, str], cwd: Path = REPO_ROOT) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(("ELF_", "CFS_"))}
    env.update(env_extra)
    return subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=120)


def test_script_is_executable_bash_with_sh_guard() -> None:
    text = _script_text()
    assert text.startswith("#!/usr/bin/env bash\n")
    assert os.access(SCRIPT, os.X_OK), "build-elf.sh must be executable"
    assert "shopt -qo posix" in text and '-z "${BASH_VERSION:-}"' in text
    assert "set -euo pipefail" in text
    # The script does not take the docker-test lock itself and says so (it is wrapped by the caller).
    assert "DOES NOT TAKE THE DOCKER-TEST LOCK" in text
    assert "docker_test_lock" not in text and "flock" not in text


def test_builder_image_pinned_by_digest_and_used_by_the_only_docker_run() -> None:
    image = _assignment("BASE_IMAGE")
    assert re.fullmatch(rf"debian:bookworm-slim@{DIGEST}", image), image
    code = _code_lines()
    joined = "\n".join(code)
    runs = [ln for ln in code if re.search(r"\bdocker run\b", ln)]
    assert len(runs) == 1, runs
    assert "--rm" in runs[0], "the build container must be removed on exit"
    assert joined.count('"${BASE_IMAGE}"') == 1
    # No literal image name outside the pinned assignment, and no command that creates image state.
    for ln in code:
        if ln.startswith("BASE_IMAGE=") or "echo" in ln:
            continue
        assert not re.search(r"\bdebian(:|@)", ln), f"literal image reference: {ln}"
    assert not re.search(r"\bdocker (tag|commit|build|load|pull|image|system|volume)\b", joined)


def test_apt_packages_pinned_to_explicit_versions_and_set_checked() -> None:
    pkgs = _apt_packages()
    assert len(pkgs) >= 90, len(pkgs)
    names = {}
    for tok in pkgs:
        m = re.fullmatch(r"([a-z0-9][a-z0-9+.\-]*)=(\d[\w.+~:\-]*)", tok)
        assert m, f"apt entry {tok!r} is not name=version"
        names[m.group(1)] = m.group(2)
    assert len(names) == len(pkgs), "duplicate package in APT_PACKAGES"
    # The packages the cross-build actually uses are in the pinned closure.
    assert {"cmake", "cmake-data", "build-essential", "make", "python3", "file", "patch"} <= set(names)
    # Every apt-get install in executable code installs only the pinned set (expanded from the env).
    installs = [ln for ln in _code_lines() if "apt-get install" in ln]
    assert len(installs) == 1, installs
    assert re.search(r"apt-get install -y --no-install-recommends \$\{ELF_APT_PACKAGES\}\s*$", installs[0]), installs
    assert '-e "ELF_APT_PACKAGES=${APT_PACKAGES[*]}"' in _script_text()
    # The full installed set is hashed in the container and compared to a pinned value.
    assert re.fullmatch(r"[0-9a-f]{64}", _assignment("ELF_DPKG_SET_SHA256"))
    text = _script_text()
    assert "dpkg-query -W" in text
    assert re.search(r'if \[ "\$\{got\}" != "\$\{ELF_DPKG_SET_SHA256\}" \]; then\n\s+echo [^\n]*\n\s+exit 3', text)


def test_cfe_build_metadata_is_pinned_and_passed_to_the_container() -> None:
    # generate_build_env.cmake reads exactly these three environment variables.
    assert re.fullmatch(r"\d{12}", _assignment("PINNED_BUILDDATE"))
    assert _assignment("PINNED_BUILDHOST")
    assert _assignment("PINNED_BUILDUSER")
    text = _script_text()
    assert '-e "BUILDDATE=${ELF_BUILDDATE}" -e "HOSTNAME=${ELF_BUILDHOST}" -e "USER=${ELF_BUILDUSER}"' in text
    assert 'ELF_BUILDDATE="${ELF_BUILDDATE:-${PINNED_BUILDDATE}}"' in text
    # ... and cFE really reads those variables (the fetched tree is gitignored: check when present).
    gen = REPO_ROOT / "third_party" / "cfs" / "cfe" / "cmake" / "generate_build_env.cmake"
    if gen.exists():
        body = gen.read_text()
        for var in ("BUILDDATE", "HOSTNAME", "USER"):
            assert f"$ENV{{{var}}}" in body, var


def test_toolchain_mounted_read_only_and_manifest_hash_pinned() -> None:
    text = _script_text()
    assert re.fullmatch(r"[0-9a-f]{64}", _assignment("TOOLCHAIN_MANIFEST_SHA256"))
    assert '-v "${ELF_TOOLCHAIN_DIR}":/output/toolchain:ro' in text
    # The manifest is checked before anything is staged or any docker command runs.
    code = "\n".join(_code_lines())
    check = code.index('if [ "${TOOLCHAIN_ACTUAL}" != "${TOOLCHAIN_MANIFEST_SHA256}" ]')
    assert check < code.index('mkdir -p "${STAGE}"') < code.index("docker run")
    assert "ELF_TOOLCHAIN_ALLOW_MISMATCH" in text


def _reference_manifest_hash(root: Path) -> str:
    """The manifest format of build-elf.sh's header, implemented independently of the script."""
    entries: list[tuple[str, str]] = []
    for path in root.rglob("*"):
        rel = str(path.relative_to(root))
        if path.is_symlink():
            entries.append((rel, f"L {os.readlink(path)}  {rel}"))
        elif path.is_file():
            entries.append((rel, f"F {hashlib.sha256(path.read_bytes()).hexdigest()}  {rel}"))
    entries.sort(key=lambda e: e[0].encode())
    return hashlib.sha256("".join(f"{line}\n" for _, line in entries).encode()).hexdigest()


def _fake_toolchain(root: Path) -> Path:
    (root / "bin").mkdir(parents=True)
    syms = root / "bin" / "rtems-syms"
    syms.write_text("#!/bin/sh\n")
    syms.chmod(0o755)
    (root / "lib").mkdir()
    (root / "lib" / "libx.a").write_bytes(b"archive")
    (root / "lib" / "libx.link").symlink_to("libx.a")
    return root


def test_toolchain_manifest_hash_behaviour(tmp_path: Path) -> None:
    tc = _fake_toolchain(tmp_path / "toolchain")
    env = {"ELF_TOOLCHAIN_DIR": str(tc)}
    first = _run([str(SCRIPT), "--print-toolchain-hash"], env)
    assert first.returncode == 0, first.stderr
    assert first.stdout.strip() == _reference_manifest_hash(tc)
    # A changed byte, a retargeted symlink and an added file each change the hash.
    seen = {first.stdout.strip()}
    (tc / "lib" / "libx.a").write_bytes(b"archivE")
    seen.add(_run([str(SCRIPT), "--print-toolchain-hash"], env).stdout.strip())
    (tc / "lib" / "libx.link").unlink()
    (tc / "lib" / "libx.link").symlink_to("bin")
    seen.add(_run([str(SCRIPT), "--print-toolchain-hash"], env).stdout.strip())
    (tc / "lib" / "extra").write_text("x")
    seen.add(_run([str(SCRIPT), "--print-toolchain-hash"], env).stdout.strip())
    assert len(seen) == 4, seen
    assert _run([str(SCRIPT), "--print-toolchain-hash"], env).stdout.strip() == _reference_manifest_hash(tc)


def test_toolchain_mismatch_refuses_before_staging(tmp_path: Path) -> None:
    tc = _fake_toolchain(tmp_path / "toolchain")
    mirrors = tmp_path / "mirrors"
    (mirrors / "bundle.git").mkdir(parents=True)
    stage = Path.home() / "av-elf-stage" / f"pytest-{os.getpid()}"
    env = {
        "ELF_TOOLCHAIN_DIR": str(tc),
        "CFS_MIRROR_DIR": str(mirrors),
        "ELF_STAGE_DIR": str(stage),
        "ELF_OUT_DIR": str(tmp_path / "out"),
    }
    res = _run([str(SCRIPT)], env)
    assert res.returncode == 2, (res.returncode, res.stdout, res.stderr)
    assert "toolchain manifest sha256" in res.stderr and "refusing" in res.stderr
    assert not stage.exists(), "a refused build must not have staged anything"
    assert not (tmp_path / "out").exists()


def test_non_shipping_builds_refuse_the_default_output_dir() -> None:
    for var, value in (
        ("ELF_NO_SYMS_FIX", "1"),
        ("ELF_BUILDDATE", "199901010000"),
        ("ELF_BUILDHOST", "someone-else"),
        ("ELF_BUILDUSER", "someone"),
        ("ELF_TOOLCHAIN_ALLOW_MISMATCH", "1"),
    ):
        res = _run([str(SCRIPT)], {var: value})
        assert res.returncode == 2, (var, res.returncode, res.stdout, res.stderr)
        assert "non-shipping build" in res.stderr and var in res.stderr, (var, res.stderr)


def test_stage_dir_must_be_under_home_and_empty(tmp_path: Path) -> None:
    res = _run([str(SCRIPT)], {"ELF_STAGE_DIR": "/tmp/av-elf-stage-pytest"})
    assert res.returncode == 2 and "under $HOME" in res.stderr, (res.returncode, res.stderr)
    home_stage = Path.home() / "av-elf-stage" / f"pytest-nonempty-{os.getpid()}"
    home_stage.mkdir(parents=True)
    try:
        (home_stage / "leftover").write_text("x")
        res = _run([str(SCRIPT)], {"ELF_STAGE_DIR": str(home_stage)})
        assert res.returncode == 2 and "not empty" in res.stderr, (res.returncode, res.stderr)
    finally:
        (home_stage / "leftover").unlink()
        home_stage.rmdir()
        try:
            home_stage.parent.rmdir()  # only if this test created it and nothing else is staged there
        except OSError:
            pass


def test_rtems_syms_gets_a_fixed_c_file_name_on_the_staged_cmake_only() -> None:
    text = _script_text()
    assert "SYMS_FROM='-o <TARGET>-dl-sym.o <TARGET>-prelink'" in text
    assert "SYMS_TO='-S <TARGET>-dl-sym.c -o <TARGET>-dl-sym.o <TARGET>-prelink'" in text
    # The file edited is the staged copy, and the edit is idempotent and asserted.
    assert 'RTEMS_CMAKE="${STAGE}/third_party/cfs/psp/cmake/Modules/Platform/RTEMS.cmake"' in text
    assert 'grep -qF -- "${SYMS_TO}" "${RTEMS_CMAKE}"' in text  # already applied: no second edit
    assert 'if [ "${n}" != "1" ]' in text  # exactly one link line, else fail loudly
    edits = [ln for ln in _code_lines() if "perl -pi" in ln]
    assert len(edits) == 1 and '"${RTEMS_CMAKE}"' in edits[0], edits
    # The control-only switch skips the edit and is non-shipping.
    assert re.search(r'if \[ -n "\$\{ELF_NO_SYMS_FIX:-\}" \]; then', text)
    assert 'NONSHIPPING="${NONSHIPPING} ELF_NO_SYMS_FIX"' in text
    # No ELF post-processing: the fix is not a byte patch.
    assert not re.search(r"\b(objcopy|strip|dd|xxd|sed -i)\b", "\n".join(_code_lines()))


def test_the_substitution_matches_upstreams_link_line() -> None:
    from_ = re.search(r"^SYMS_FROM='([^']*)'$", _script_text(), re.MULTILINE)
    to = re.search(r"^SYMS_TO='([^']*)'$", _script_text(), re.MULTILINE)
    assert from_ and to
    upstream = (
        '"${RTEMS_TOOLS_PREFIX}/bin/rtems-syms -v -e -c \\"${RTEMS_BSP_C_FLAGS}\\" '
        '-C <CMAKE_C_COMPILER> -o <TARGET>-dl-sym.o <TARGET>-prelink"'
    )
    assert upstream.count(from_.group(1)) == 1
    fixed = upstream.replace(from_.group(1), to.group(1))
    assert "-S <TARGET>-dl-sym.c -o <TARGET>-dl-sym.o" in fixed
    assert fixed.replace(to.group(1), from_.group(1)) == upstream
    # When the fetched cFS tree is present, the real file has exactly that one line.
    real = REPO_ROOT / "third_party" / "cfs" / "psp" / "cmake" / "Modules" / "Platform" / "RTEMS.cmake"
    if real.exists():
        body = real.read_text()
        assert body.count(from_.group(1)) + body.count(to.group(1)) == 1


def test_nothing_is_written_into_third_party_cfs() -> None:
    code = _code_lines()
    # Every executable mention of third_party/cfs is under the staging directory.
    for ln in code:
        if "echo" in ln:
            continue
        for m in re.finditer(r"third_party/cfs\b", ln):
            assert ln[: m.start()].endswith("${STAGE}/") or ln[: m.start()].endswith("/workspace/"), ln
    text = _script_text()
    # fetch-cfs.sh is aimed at the stage by its own parameter, from the local mirrors, off the network.
    assert 'CFS_FETCH_DEST="${STAGE}/third_party/cfs"' in text
    assert 'CFS_MIRROR_DIR="${CFS_MIRROR_DIR}"' in text
    assert 'GIT_ALLOW_PROTOCOL="${GIT_PROTOCOL_GUARD}"' in text and 'GIT_PROTOCOL_GUARD="file"' in text
    # The only writable bind mount is the staging directory; the repo is never mounted.
    mounts = re.findall(r'-v "([^"]+)"(:\S+)?', text)
    assert mounts == [("${STAGE}", ":/workspace"), ("${ELF_TOOLCHAIN_DIR}", ":/output/toolchain:ro")], mounts
    assert "${REPO_ROOT}" not in " ".join(m[0] for m in mounts)
    # rm -rf only ever names the staging dir or this build's own outputs.
    for ln in code:
        if re.search(r"\brm -rf\b", ln):
            assert re.search(r'\$\{(STAGE|ELF_OUT_DIR)\}', ln), ln


def test_staged_inputs_cover_everything_build_cfs_cross_reads() -> None:
    text = _script_text()
    staged = re.search(
        r"git ls-files -z --cached --others --exclude-standard -- \\\n(.*?)\n\s+\| tar", text, re.DOTALL
    )
    assert staged, "the staging copy of the build inputs moved"
    staged_paths = staged.group(1).replace("\\", " ").split()
    cross = CROSS.read_text()
    # Everything build-cfs-cross.sh reads from the repo (via $SVC_CFS = services/cfs, or its own
    # path) lies under a staged path.
    wanted = {f"services/cfs/{m}" for m in re.findall(r'\$SVC_CFS/([\w./\-]+)', cross)}
    wanted.add("third_party/rtems-container/build-cfs-cross.sh")
    wanted.add("third_party/fetch-cfs.sh")
    for path in sorted(wanted):
        assert any(path == s or path.startswith(s.rstrip("/") + "/") for s in staged_paths), (
            f"{path} is read by build-cfs-cross.sh but not staged by build-elf.sh"
        )


def test_default_output_is_gitignored_and_documented_invocation_points_here() -> None:
    res = subprocess.run(
        ["git", "check-ignore", "-q", "third_party/rtems-container/output/elf/core-cpu1.exe"],
        cwd=REPO_ROOT,
    )
    assert res.returncode == 0, "the default ELF_OUT_DIR must be gitignored"
    assert 'DEFAULT_OUT_DIR="${REPO_ROOT}/third_party/rtems-container/output/elf"' in _script_text()
    header = "\n".join(CROSS.read_text().splitlines()[:40])
    assert "build-elf.sh" in header, "build-cfs-cross.sh's header must point at the reproducible recipe"

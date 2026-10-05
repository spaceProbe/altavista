"""services/cfs/tests/test_build_shim_script.py (docs/open-questions.md question 236):

A fast, docker-free guard on services/cfs/build-shim.sh, the reproducible cross-build of the cFS
image's lockstep shim. The script is the recipe that used to live in services/cfs/Dockerfile's
header comment; it makes two builds of one commit byte-identical (measured: two clean builds from
different repo mount paths, `cmp`-identical, with and without strip) by pinning or erasing every
input that reaches the binary. This test reads the script and pins those invariants, so a later
edit cannot quietly drop one:

  - the builder image is named by content digest, never a bare tag, and every `docker run`/
    `docker create` in the script uses a pinned image variable, not a literal image name;
  - the strip image is the very digest-pinned `ubuntu:22.04` services/cfs/Dockerfile builds on;
  - `cargo build --release --locked -p av-lockstep-shim`;
  - `--remap-path-prefix` for the repo mount, the spoore mount, cargo's target dir, CARGO_HOME
    and RUSTUP_HOME (the paths rustc would otherwise embed);
  - every `apt-get install` takes explicit `name=version` packages (protoc's output feeds the
    binary), and the protobuf set includes the compiler and the dev headers.

It does not run docker and it does not prove reproducibility -- that proof is two real builds,
recorded in services/cfs/IMAGE_DIGEST.md. It only keeps the recipe the proof was made with from
drifting.
"""
from __future__ import annotations

import os
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "services" / "cfs" / "build-shim.sh"
DOCKERFILE = REPO_ROOT / "services" / "cfs" / "Dockerfile"

DIGEST = r"sha256:[0-9a-f]{64}"


def _script_text() -> str:
    return SCRIPT.read_text()


def _code_lines() -> list[str]:
    """The script's non-comment, non-blank lines (comments mention flags and tags freely)."""
    return [
        ln
        for ln in _script_text().splitlines()
        if ln.strip() and not ln.lstrip().startswith("#")
    ]


def _assignment(name: str) -> str:
    m = re.search(rf'^{name}="([^"]*)"$', _script_text(), re.MULTILINE)
    assert m, f"build-shim.sh no longer assigns {name}=\"...\" on one line"
    return m.group(1)


def test_script_is_executable_bash_with_sh_guard() -> None:
    text = _script_text()
    assert text.startswith("#!/usr/bin/env bash\n")
    assert os.access(SCRIPT, os.X_OK), "services/cfs/build-shim.sh must be executable"
    # The same refusal guard build-image.sh has: bash in POSIX mode (how `sh` runs it) is refused.
    assert 'shopt -qo posix' in text and '-z "${BASH_VERSION:-}"' in text
    assert "set -euo pipefail" in text


def test_builder_image_pinned_by_digest() -> None:
    rust = _assignment("RUST_IMAGE")
    assert re.fullmatch(rf"rust:1\.90-bookworm@{DIGEST}", rust), rust
    ubuntu = _assignment("UBUNTU_IMAGE")
    assert re.fullmatch(rf"ubuntu:22\.04@{DIGEST}", ubuntu), ubuntu


def test_strip_image_is_the_dockerfiles_own_base() -> None:
    ubuntu = _assignment("UBUNTU_IMAGE")
    froms = re.findall(r"^FROM (\S+)", DOCKERFILE.read_text(), re.MULTILINE)
    assert froms, "services/cfs/Dockerfile has no FROM lines?"
    assert set(froms) == {ubuntu}, (
        f"build-shim.sh strips on {ubuntu} but the Dockerfile's stages are {froms}"
    )


def test_every_docker_image_reference_is_a_pinned_variable() -> None:
    code = _code_lines()
    creates = [ln for ln in code if re.match(r"\s*docker create\b", ln)]
    assert len(creates) == 2, "expected exactly the build container and the strip container"
    joined = "\n".join(code)
    assert joined.count('"${RUST_IMAGE}"') == 1 and joined.count('"${UBUNTU_IMAGE}"') == 1, (
        "each container must take its image from the digest-pinned variable"
    )
    assert not re.search(r"\bdocker run\b", joined), "use docker create + cp so no mount is writable"
    # No literal image name in executable code outside the two pinned assignments: a bare tag floats.
    for ln in code:
        if ln.startswith(("RUST_IMAGE=", "UBUNTU_IMAGE=")) or "echo" in ln:
            continue
        assert not re.search(r"\b(rust|ubuntu)(:|@)", ln), f"literal image reference: {ln}"


def test_cargo_build_is_locked_release_for_the_shim() -> None:
    joined = " ".join(_code_lines())
    assert re.search(
        r"cargo build --release --locked -p av-lockstep-shim\b", joined
    ), "the shim must be built with `cargo build --release --locked -p av-lockstep-shim`"
    # A plain `cargo build` / `cargo install` without --locked anywhere would defeat the lockfile.
    for m in re.finditer(r"cargo (build|install|run)\b[^\n\"]*", joined):
        assert "--locked" in m.group(0), m.group(0)


def test_remap_path_prefix_covers_every_embedded_build_path() -> None:
    text = _script_text()
    m = re.search(r'^\s*SHIM_RUSTFLAGS="(--remap-path-prefix=[^"]*)"$', text, re.MULTILINE)
    assert m, "build-shim.sh no longer sets SHIM_RUSTFLAGS to --remap-path-prefix flags"
    flags = m.group(1).split()
    assert all(f.startswith("--remap-path-prefix=") for f in flags), flags
    sources = {f.split("=", 2)[1] for f in flags}
    # The repo mount, the spoore mount and cargo's target dir are parameters/variables; the two
    # toolchain homes are fixed in the rust image.
    assert {"${SHIM_MOUNT}", "${SPOORE_MOUNT}", "${TARGET_MOUNT}"} <= sources, sources
    assert {"/usr/local/cargo", "/usr/local/rustup"} <= sources, sources
    # The flags reach rustc, and only the explicit control switch can drop them.
    assert '-e "RUSTFLAGS=${SHIM_RUSTFLAGS}"' in text
    assert "SHIM_NO_REMAP" in text and "non-default path" in text, (
        "the remap-less control build must refuse to overwrite the shipping binary"
    )


def test_mounts_are_read_only_and_repo_sits_under_users_probe_code() -> None:
    text = _script_text()
    assert '"${REPO_ROOT}":"${SHIM_MOUNT}":ro' in text
    assert '"${SPOORE_ROOT}":"${SPOORE_MOUNT}":ro' in text
    assert 'SPOORE_MOUNT="/Users/probe/code/spoore"' in text
    assert '!= "/Users/probe/code"' in text, "the SHIM_MOUNT parent guard is gone"


def _apt_set(var: str, version_var: str) -> dict[str, str]:
    value = _assignment(var)
    version = _assignment(version_var)
    assert re.fullmatch(r"\d[\w.+~:\-]*", version), version
    out: dict[str, str] = {}
    for tok in value.split():
        m = re.fullmatch(r"([a-z0-9][a-z0-9+.\-]*)=\$\{" + version_var + r"\}", tok)
        assert m, f"{var} token {tok!r} is not name=${{{version_var}}}"
        out[m.group(1)] = version
    return out


def test_apt_packages_pinned_to_explicit_versions() -> None:
    proto = _apt_set("APT_PROTOBUF", "PROTOBUF_VERSION")
    assert {"protobuf-compiler", "libprotobuf-dev"} <= set(proto), proto
    binutils = _apt_set("APT_BINUTILS", "BINUTILS_VERSION")
    assert "binutils-aarch64-linux-gnu" in binutils, binutils  # carries `strip`
    # Every apt-get install in executable code installs only those pinned sets.
    installs = [ln for ln in _code_lines() if "apt-get install" in ln]
    assert len(installs) == 2, installs
    for ln in installs:
        assert re.search(r"apt-get install\b[^$]*\$\{APT_(PROTOBUF|BINUTILS)\}", ln), ln
        # nothing is installed outside the variable: the text after the flags is just the variable
        tail = ln.split("--no-install-recommends", 1)[1]
        assert re.fullmatch(r"\s*\$\{APT_(PROTOBUF|BINUTILS)\}\s*\\?", tail), tail

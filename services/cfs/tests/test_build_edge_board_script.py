"""services/cfs/tests/test_build_edge_board_script.py (docs/open-questions.md question 254 item 9):

A fast, docker-free guard on services/cfs/build-edge-board.sh, the Linux/aarch64 build of
`av-edge-board` and `av-edge-board-log` that the board's restart tests run inside the Colima VM.
Question 249 recorded that build as "not byte-reproducible" (libssl-dev and pkg-config installed
unpinned from the live mirror, the build paths left in the binary); the script is now reproducible in
the manner of third_party/rtems-container/build-elf.sh (apt) and services/cfs/build-shim.sh (cargo).
This test reads the script and pins those invariants, so a later edit cannot quietly drop one:

  - the builder image is named by content digest, the SAME value as build-shim.sh, never a tag, and
    the script builds, tags, commits and pulls nothing;
  - every package the build installs is `name=version` (the five protobuf packages; their hash is
    pinned here), and nothing else is installed (the old unpinned `libssl-dev pkg-config` is gone);
  - the apt sources are snapshot.debian.org at one fixed timestamp for debian and debian-security,
    replacing the image's live-mirror file; no live mirror remains in executable code; the only apt
    option is Acquire::Check-Valid-Until=false on `apt-get update` (signatures stay verified); any
    other apt source file refuses;
  - the fetched .deb set is hashed against a pin and refuses before install; the installed dpkg set is
    hashed against a pin and refuses before the build;
  - `cargo build --release --locked -p av-edge-board --bins`, and `--remap-path-prefix` for the repo
    mount, the spoore mount, cargo's target dir, CARGO_HOME and RUSTUP_HOME, passed to rustc, with a
    control-only switch that cannot write the default output.

It does not run docker and it does not prove reproducibility -- that proof is two real builds from
different stage paths, recorded in the script's header. It only keeps the recipe the proof was made
with from drifting.
"""
from __future__ import annotations

import hashlib
import os
import re
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "services" / "cfs" / "build-edge-board.sh"
BUILD_SHIM = REPO_ROOT / "services" / "cfs" / "build-shim.sh"

DIGEST = r"sha256:[0-9a-f]{64}"

# The pins as they were resolved and verified by the question 254 item 9 builds.
PINS_SHA256 = "a917be1fe1c8a52850dc47937b5a6f1d3941f345a7ca8a5aa1083abac12589f3"
SNAPSHOT_TS = "20260711T103542Z"
DEB_SET_SHA256 = "6c2aadcce881bb56eb3c5348f480ec2877ee629f4dd6b15fb5b15bc11397eafd"
DPKG_SET_SHA256 = "e1d35298a2e4baa3ee605def72c888acc804068bd982fb9237435333156dab25"
# The first `debian` snapshot whose bookworm Packages index lists the protobuf pins (Debian 12.15). The
# pool files' first_seen (20260704T145040Z) is only a lower bound and fails at apt (script header).
FIRST_SNAPSHOT_LISTING_THE_PINS = "20260711T103542Z"


def _script_text() -> str:
    return SCRIPT.read_text()


def _code_lines() -> list[str]:
    """The script's non-comment, non-blank lines (comments mention tags, paths and flags freely)."""
    return [ln for ln in _script_text().splitlines() if ln.strip() and not ln.lstrip().startswith("#")]


def _assignment(name: str) -> str:
    m = re.search(rf'^{name}="([^"]*)"$', _script_text(), re.MULTILINE)
    assert m, f"build-edge-board.sh no longer assigns {name}=\"...\" on one line"
    return m.group(1)


def _apt_packages() -> list[str]:
    m = re.search(r"^APT_PACKAGES=\(\n(.*?)^\)$", _script_text(), re.MULTILINE | re.DOTALL)
    assert m, "build-edge-board.sh no longer defines the APT_PACKAGES=( ... ) array"
    return m.group(1).split()


def _fake_spoore(root: Path) -> Path:
    (root / "crates" / "spoore-cdm").mkdir(parents=True)
    return root


def _run(env_extra: dict[str, str]) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith(("EDGE_BOARD_", "SPOORE_"))}
    env.update(env_extra)
    return subprocess.run([str(SCRIPT)], cwd=REPO_ROOT, env=env, capture_output=True, text=True, timeout=120)


def test_script_is_executable_bash_with_sh_guard_and_takes_no_lock() -> None:
    text = _script_text()
    assert text.startswith("#!/usr/bin/env bash\n")
    assert os.access(SCRIPT, os.X_OK), "build-edge-board.sh must be executable"
    assert "shopt -qo posix" in text and '-z "${BASH_VERSION:-}"' in text
    assert "set -euo pipefail" in text
    # Like build-shim.sh and build-elf.sh, it does not take the docker-test lock and says so.
    assert "DOES NOT TAKE THE DOCKER-TEST LOCK" in text
    assert "docker_test_lock" not in text and "flock" not in text


def test_builder_image_is_the_shim_builds_digest_and_the_only_image_reference() -> None:
    shim = BUILD_SHIM.read_text()
    pin = re.search(rf'^RUST_IMAGE="(rust:1\.90-bookworm@{DIGEST})"$', shim, re.MULTILINE)
    assert pin, "build-shim.sh no longer pins RUST_IMAGE on one line"
    assert _assignment("RUST_IMAGE") == pin.group(1)
    code = _code_lines()
    joined = "\n".join(code)
    creates = [ln for ln in code if re.match(r"\s*docker create\b", ln)]
    assert len(creates) == 1, creates
    assert joined.count('"${RUST_IMAGE}"') == 1
    assert not re.search(r"\bdocker run\b", joined), "use docker create + cp so no mount is writable"
    assert not re.search(r"\bdocker (tag|commit|build|load|pull|image|system|volume)\b", joined)
    for ln in code:
        if ln.startswith("RUST_IMAGE=") or "echo" in ln:
            continue
        assert not re.search(r"\b(rust|ubuntu|debian|alpine):", ln), f"literal image reference: {ln}"


def test_the_only_installed_packages_are_the_five_pinned_protobuf_debs() -> None:
    pkgs = _apt_packages()
    assert pkgs == sorted(pkgs), "the pins are kept sorted"
    names = {}
    for tok in pkgs:
        m = re.fullmatch(r"([a-z0-9][a-z0-9+.\-]*)=(\d[\w.+~:\-]*)", tok)
        assert m, f"apt entry {tok!r} is not name=version"
        names[m.group(1)] = m.group(2)
    assert len(names) == len(pkgs) == 5, pkgs
    assert set(names) == {"protobuf-compiler", "libprotobuf-dev", "libprotoc32", "libprotobuf32", "libprotobuf-lite32"}
    assert set(names.values()) == {"3.21.12-3+deb12u1"}
    assert hashlib.sha256(("\n".join(pkgs) + "\n").encode()).hexdigest() == PINS_SHA256
    # Every apt-get install installs only that set (expanded from the environment): one download-only
    # pass whose .deb set is hashed, and the install proper. libssl-dev and pkg-config come from the
    # digest-pinned image; the unpinned live-mirror install of them is gone.
    installs = [ln.strip() for ln in _code_lines() if "apt-get install" in ln]
    assert installs == [
        "apt-get install -y --no-install-recommends --download-only ${EB_APT_PACKAGES}",
        "apt-get install -y --no-install-recommends ${EB_APT_PACKAGES}",
    ], installs
    text = _script_text()
    assert '-e "EB_APT_PACKAGES=${APT_PACKAGES[*]}"' in text
    code = "\n".join(_code_lines())
    assert "libssl-dev pkg-config" not in code and "apt-get upgrade" not in code and "dist-upgrade" not in code


def test_apt_sources_are_the_snapshot_at_one_fixed_timestamp_and_no_live_mirror_is_left() -> None:
    ts = _assignment("SNAPSHOT_TS")
    assert ts == SNAPSHOT_TS and re.fullmatch(r"\d{8}T\d{6}Z", ts), ts
    assert ts >= FIRST_SNAPSHOT_LISTING_THE_PINS, "the timestamp must not precede the snapshot that lists the pins"
    text = _script_text()
    # The override is the only other source of the timestamp, and it is non-shipping.
    assert 'EDGE_BOARD_SNAPSHOT_TS="${EDGE_BOARD_SNAPSHOT_TS:-${SNAPSHOT_TS}}"' in text
    assert 'NONSHIPPING="${NONSHIPPING} EDGE_BOARD_SNAPSHOT_TS"' in text
    assert _assignment("SNAPSHOT_DEBIAN_URL") == "https://snapshot.debian.org/archive/debian/${EDGE_BOARD_SNAPSHOT_TS}/"
    assert (
        _assignment("SNAPSHOT_SECURITY_URL")
        == "https://snapshot.debian.org/archive/debian-security/${EDGE_BOARD_SNAPSHOT_TS}/"
    )
    code = "\n".join(_code_lines())
    # The container replaces the image's sources file with exactly the two snapshot archives ...
    m = re.search(r"cat > /etc/apt/sources\.list\.d/debian\.sources <<EOS\n(.*?)\nEOS\n", text, re.DOTALL)
    assert m, "the container no longer writes /etc/apt/sources.list.d/debian.sources"
    stanzas = [s for s in m.group(1).split("\n\n") if s.strip()]
    assert len(stanzas) == 2, stanzas
    parsed = [dict(ln.split(": ", 1) for ln in s.splitlines()) for s in stanzas]
    assert [p["URIs"] for p in parsed] == ["${EB_SNAPSHOT_DEBIAN_URL}", "${EB_SNAPSHOT_SECURITY_URL}"]
    assert [p["Suites"] for p in parsed] == ["bookworm bookworm-updates", "bookworm-security"]
    for p in parsed:
        assert p["Types"] == "deb" and p["Components"] == "main"
        # Signatures stay verified with the image's own keyring.
        assert p["Signed-By"] == "/usr/share/keyrings/debian-archive-keyring.gpg"
        assert set(p) == {"Types", "URIs", "Suites", "Components", "Signed-By"}, p
    # ... passes the two URLs in from the pinned variables ...
    assert '-e "EB_SNAPSHOT_DEBIAN_URL=${SNAPSHOT_DEBIAN_URL}"' in text
    assert '-e "EB_SNAPSHOT_SECURITY_URL=${SNAPSHOT_SECURITY_URL}"' in text
    # ... and refuses if any other apt source file exists.
    assert re.search(
        r'if \[ -e /etc/apt/sources\.list \] \|\| \[ "\$\(ls /etc/apt/sources\.list\.d\)" != "debian\.sources" \]; then\n\s+echo [^\n]*\n\s+exit 5',
        code,
    )
    # No live mirror and no other network location anywhere in executable code.
    for live in ("deb.debian.org", "security.debian.org", "ftp.debian.org", "httpredir"):
        assert live not in code, live
    urls = set(re.findall(r"https?://[^\s\"'`)]+", code))
    assert urls and all(u.startswith("https://snapshot.debian.org/archive/debian") for u in urls), urls


def test_only_the_valid_until_check_is_relaxed_and_signatures_stay_verified() -> None:
    code = "\n".join(_code_lines())
    updates = [ln.strip() for ln in _code_lines() if re.search(r"\bapt-get\b.*\bupdate\b", ln)]
    assert updates == ["apt-get -o Acquire::Check-Valid-Until=false update"], updates
    assert len(re.findall(r"\bapt-get\b[^\n]*\s-o\s", code)) == 1
    assert re.findall(r"\b(?:Acquire|APT|Dir|Debug)::[\w:\-]+", code) == ["Acquire::Check-Valid-Until"]
    assert "apt.conf" not in code and "/etc/apt/preferences" not in code
    for weaker in (
        "--allow-unauthenticated",
        "--allow-insecure",
        "AllowInsecureRepositories",
        "AllowDowngradeToInsecure",
        "trusted=yes",
        "[trusted",
        "--force-yes",
        "Check-Date",
        "--no-check-valid",
        "--insecure",
        "curl ",
        "wget ",
    ):
        assert weaker not in code, weaker
    assert "apt-key" not in code and "gpg --import" not in code, "the image's own keyring is the trust root"


def test_the_fetched_deb_set_and_the_installed_set_are_pinned_and_checked_in_order() -> None:
    assert _assignment("EDGE_BOARD_DEB_SET_SHA256") == DEB_SET_SHA256
    assert _assignment("EDGE_BOARD_DPKG_SET_SHA256") == DPKG_SET_SHA256
    assert DEB_SET_SHA256 != DPKG_SET_SHA256
    text = _script_text()
    assert '-e "EB_DEB_SET_SHA256=${EDGE_BOARD_DEB_SET_SHA256}"' in text
    assert '-e "EB_DPKG_SET_SHA256=${EDGE_BOARD_DPKG_SET_SHA256}"' in text
    # A mismatch refuses: the .deb set with exit 4 (before anything is installed), the dpkg set with exit 3.
    assert re.search(r'if \[ "\$\{got\}" != "\$\{EB_DEB_SET_SHA256\}" \]; then\n\s+echo [^\n]*\n\s+exit 4', text)
    assert re.search(r'if \[ "\$\{got\}" != "\$\{EB_DPKG_SET_SHA256\}" \]; then\n\s+echo [^\n]*\n\s+exit 3', text)
    code = "\n".join(_code_lines())
    # The listing is printed (the record of the network fetch, question 154).
    assert 'echo "${debs}"' in code
    assert "sha256sum -- *.deb | LC_ALL=C sort -k2" in code
    assert "dpkg-query -W -f='${Package}:${Architecture}=${Version}\\n' | LC_ALL=C sort | sha256sum" in code
    update = code.index("apt-get -o Acquire::Check-Valid-Until=false update")
    download = code.index("--download-only")
    deb_check = code.index('"${got}" != "${EB_DEB_SET_SHA256}"')
    install = code.index("apt-get install -y --no-install-recommends ${EB_APT_PACKAGES}")
    dpkg_check = code.index('"${got}" != "${EB_DPKG_SET_SHA256}"')
    build = code.index("exec cargo build")
    assert update < download < deb_check < install < dpkg_check < build


def test_build_inputs_block_prints_the_snapshot_and_the_pins_before_the_container_exists() -> None:
    text = _script_text()
    assert 'echo "  apt sources:   ${SNAPSHOT_DEBIAN_URL} (bookworm, bookworm-updates)"' in text
    assert "${SNAPSHOT_SECURITY_URL} (bookworm-security)" in text
    assert "apt .deb set:  sha256 ${EDGE_BOARD_DEB_SET_SHA256}" in text
    assert "dpkg set sha256 ${EDGE_BOARD_DPKG_SET_SHA256}" in text
    code = "\n".join(_code_lines())
    assert code.index("apt .deb set:") < code.index("docker create")


def test_cargo_build_is_locked_release_for_the_two_bins() -> None:
    joined = " ".join(_code_lines())
    assert 'exec cargo build --release --locked -p av-edge-board --bins --target-dir "${EB_TARGET}"' in joined
    # A plain `cargo build` / `cargo install` without --locked anywhere would defeat the lockfile.
    for m in re.finditer(r"cargo (build|install|run)\b[^\n]*", "\n".join(_code_lines())):
        assert "--locked" in m.group(0), m.group(0)
    # The two binaries are copied out of the same target dir the build used.
    text = _script_text()
    assert '"${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board"' in text
    assert '"${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board-log"' in text
    assert '-e "EB_TARGET=${TARGET_MOUNT}"' in text


def test_remap_path_prefix_covers_every_embedded_build_path_and_reaches_rustc() -> None:
    text = _script_text()
    m = re.search(r'^\s*EDGE_BOARD_RUSTFLAGS="(--remap-path-prefix=[^"]*)"$', text, re.MULTILINE)
    assert m, "build-edge-board.sh no longer sets EDGE_BOARD_RUSTFLAGS to --remap-path-prefix flags"
    flags = m.group(1).split()
    assert all(f.startswith("--remap-path-prefix=") for f in flags), flags
    sources = {f.split("=", 2)[1] for f in flags}
    # The repo mount, the spoore mount and cargo's target dir are parameters; the two toolchain homes are
    # fixed in the rust image.
    assert {"${EDGE_BOARD_MOUNT}", "${SPOORE_MOUNT}", "${TARGET_MOUNT}"} <= sources, sources
    assert {"/usr/local/cargo", "/usr/local/rustup"} <= sources, sources
    assert len(sources) == len(flags) == 5, flags
    assert '-e "RUSTFLAGS=${EDGE_BOARD_RUSTFLAGS}"' in text
    # The remap-less control build exists, is non-shipping and cannot overwrite the default output.
    assert 'NONSHIPPING="${NONSHIPPING} EDGE_BOARD_NO_REMAP"' in text
    assert 'DEFAULT_OUT_DIR="${REPO_ROOT}/target-docker-linux/edge-board"' in text


def test_mounts_are_read_only_and_the_repo_sits_under_users_probe_code() -> None:
    text = _script_text()
    assert '"${REPO_ROOT}":"${EDGE_BOARD_MOUNT}":ro' in text
    assert '"${SPOORE_ROOT}":"${SPOORE_MOUNT}":ro' in text
    assert 'SPOORE_MOUNT="/Users/probe/code/spoore"' in text
    assert '!= "/Users/probe/code"' in text, "the EDGE_BOARD_MOUNT parent guard is gone"
    mounts = re.findall(r'-v "([^"]+)":"([^"]+)"(:\S+)?', text)
    assert [m[2] for m in mounts] == [":ro", ":ro"], mounts
    assert 'EDGE_BOARD_OUT_DIR="${EDGE_BOARD_OUT_DIR:-${DEFAULT_OUT_DIR}}"' in text


def test_the_snapshot_timestamp_override_is_validated_and_non_shipping(tmp_path: Path) -> None:
    res = _run({"EDGE_BOARD_SNAPSHOT_TS": "yesterday"})
    assert res.returncode == 2 and "EDGE_BOARD_SNAPSHOT_TS must look like" in res.stderr, (res.returncode, res.stderr)
    spoore = _fake_spoore(tmp_path / "spoore")
    res = _run({"EDGE_BOARD_SNAPSHOT_TS": "20260707T000000Z", "SPOORE_ROOT": str(spoore)})
    assert res.returncode == 2 and "non-shipping build" in res.stderr and "EDGE_BOARD_SNAPSHOT_TS" in res.stderr, (
        res.returncode,
        res.stderr,
    )
    assert not (tmp_path / "out").exists()


def test_the_remap_less_control_refuses_the_default_output_dir(tmp_path: Path) -> None:
    spoore = _fake_spoore(tmp_path / "spoore")
    res = _run({"EDGE_BOARD_NO_REMAP": "1", "SPOORE_ROOT": str(spoore)})
    assert res.returncode == 2 and "non-shipping build" in res.stderr and "EDGE_BOARD_NO_REMAP" in res.stderr, (
        res.returncode,
        res.stderr,
    )


def test_the_mount_must_sit_directly_under_users_probe_code(tmp_path: Path) -> None:
    spoore = _fake_spoore(tmp_path / "spoore")
    res = _run({"EDGE_BOARD_MOUNT": "/Users/probe/elsewhere/AltaVista", "SPOORE_ROOT": str(spoore)})
    assert res.returncode == 2 and "must sit directly under /Users/probe/code" in res.stderr, (res.returncode, res.stderr)

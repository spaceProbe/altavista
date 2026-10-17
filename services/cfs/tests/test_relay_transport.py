"""services/cfs/tests/test_relay_transport.py (docs/open-questions.md question 245 (6)):

A fast, docker-free guard on the choice of the process that listens on the guest's Unix socket in the
cFS image: `av-lockstep-shim` by default (the image's behaviour is unchanged), `av-lockstep-relay` (the
frame-aware UDP carrier for the board stand-in) when the container is started with
`LOCKSTEP_TRANSPORT=udp`. It reads the entrypoint, the Dockerfile and the build script and pins:

  - the default is the shim, with exactly the command line the image has always used;
  - the relay is chosen only by `LOCKSTEP_TRANSPORT=udp`, takes its UDP port and optional pinned peer
    from the environment, and an unknown value is refused (status 2) instead of falling back;
  - the Dockerfile copies and marks executable both binaries; `build-shim.sh` installs both from the
    one `cargo build -p av-lockstep-shim`; the relay binary is git-ignored like the shim.

It runs no docker. The entrypoint's behaviour in a real container is measured by the native-ct-2 round
(the report records the commands) and by `crates/av-edge-board/tests/cfs_container_restart.rs`.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
ENTRYPOINT = REPO_ROOT / "services" / "cfs" / "container-entrypoint.sh"
DOCKERFILE = REPO_ROOT / "services" / "cfs" / "Dockerfile"
BUILD_SHIM = REPO_ROOT / "services" / "cfs" / "build-shim.sh"
GITIGNORE = REPO_ROOT / ".gitignore"
SHIM_CARGO = REPO_ROOT / "crates" / "av-lockstep-shim" / "Cargo.toml"


def _entry_code() -> str:
    return "\n".join(ln for ln in ENTRYPOINT.read_text().splitlines() if not ln.lstrip().startswith("#"))


def test_default_transport_is_the_shim_with_the_unchanged_command_line() -> None:
    code = _entry_code()
    assert 'LOCKSTEP_TRANSPORT="${LOCKSTEP_TRANSPORT:-shim}"' in code
    shim_arm = re.search(r"^\s*shim\)\n(.*?)\n\s*;;", code, re.MULTILINE | re.DOTALL)
    assert shim_arm, "the entrypoint has no `shim)` arm"
    assert shim_arm.group(1).split() == ["CARRIER=av-lockstep-shim", "/cfs/av-lockstep-shim", "--socket-path", '"$SOCKET_PATH"', "--grpc-addr", "0.0.0.0:50070", "&"]


def test_relay_is_chosen_only_by_udp_and_an_unknown_value_is_refused() -> None:
    code = _entry_code()
    udp_arm = re.search(r"^\s*udp\)\n(.*?)\n\s*;;", code, re.MULTILINE | re.DOTALL)
    assert udp_arm, "the entrypoint has no `udp)` arm"
    arm = udp_arm.group(1)
    assert "/cfs/av-lockstep-relay" in arm and '--socket-path "$SOCKET_PATH"' in arm
    assert '--udp-listen "0.0.0.0:${LOCKSTEP_UDP_PORT:-50090}"' in arm
    assert '${LOCKSTEP_UDP_PEER:+--peer "$LOCKSTEP_UDP_PEER"}' in arm
    assert "--grpc-addr" not in arm, "the relay serves no gRPC"
    star_arm = re.search(r"^\s*\*\)\n(.*?)\n\s*;;", code, re.MULTILINE | re.DOTALL)
    assert star_arm and "exit 2" in star_arm.group(1) and "is not one of: shim, udp" in star_arm.group(1)


def test_the_entrypoint_is_valid_posix_sh() -> None:
    # `sh -n` parses without running anything.
    result = subprocess.run(["/bin/sh", "-n", str(ENTRYPOINT)], capture_output=True, text=True)
    assert result.returncode == 0, result.stderr


def test_dockerfile_copies_and_marks_executable_both_binaries() -> None:
    text = DOCKERFILE.read_text()
    assert "COPY services/cfs/bin/av-lockstep-shim /cfs/av-lockstep-shim" in text
    assert "COPY services/cfs/bin/av-lockstep-relay /cfs/av-lockstep-relay" in text
    assert "RUN chmod +x /cfs/av-lockstep-shim /cfs/av-lockstep-relay /cfs/container-entrypoint.sh" in text


def test_build_shim_installs_the_relay_from_the_same_cargo_build() -> None:
    text = BUILD_SHIM.read_text()
    code = "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))
    # one cargo build of the package builds both [[bin]]s; no separate relay build, no --bin filter
    assert len(re.findall(r"cargo build\b", code)) == 1 and "--bin" not in code
    assert 'RELAY_OUT="${RELAY_OUT:-$(dirname "${SHIM_OUT}")/av-lockstep-relay}"' in code
    for needle in (
        'docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-lockstep-relay"',
        "strip -o /av-lockstep-relay-stripped /av-lockstep-relay",
        'cp "${SHIM_ARTIFACT_DIR}/av-lockstep-relay-stripped" "${RELAY_OUT}"',
    ):
        assert needle in code, needle


def test_the_relay_is_a_second_bin_of_the_shim_crate_and_git_ignored() -> None:
    manifest = SHIM_CARGO.read_text()
    assert manifest.count("[[bin]]") == 2
    assert 'name = "av-lockstep-relay"\npath = "src/bin/av-lockstep-relay.rs"' in manifest
    assert "services/cfs/bin/av-lockstep-relay" in GITIGNORE.read_text().splitlines()


def test_edge_board_linux_build_uses_the_same_pinned_builder_as_the_shim_build() -> None:
    """services/cfs/build-edge-board.sh (native round decision 15) builds the Linux av-edge-board the
    cFS restart test runs inside the Colima VM. Its builder must be the shim build's digest-pinned
    image, named by digest and never by a floating tag, and it must stay locked and read-only."""
    shim = BUILD_SHIM.read_text()
    script = REPO_ROOT / "services" / "cfs" / "build-edge-board.sh"
    board = script.read_text()
    pin = re.search(r'^RUST_IMAGE="(rust:1\.90-bookworm@sha256:[0-9a-f]{64})"$', shim, re.MULTILINE)
    assert pin, "build-shim.sh no longer pins RUST_IMAGE on one line"
    assert f'RUST_IMAGE="{pin.group(1)}"' in board
    code = "\n".join(ln for ln in board.splitlines() if ln.strip() and not ln.lstrip().startswith("#"))
    assert "cargo build --release --locked -p av-edge-board --bins" in code
    assert '"${REPO_ROOT}":"${EDGE_BOARD_MOUNT}":ro' in code and '"${SPOORE_ROOT}":"${SPOORE_MOUNT}":ro' in code
    for ln in code.splitlines():
        if ln.startswith("RUST_IMAGE="):
            continue
        assert not re.search(r"\b(rust|ubuntu|debian|alpine):", ln), f"literal image reference: {ln}"
    assert "set -euo pipefail" in code and script.stat().st_mode & 0o100

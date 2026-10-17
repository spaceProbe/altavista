#!/usr/bin/env bash
# services/cfs/build-edge-board.sh -- a Linux/aarch64 build of `av-edge-board` and `av-edge-board-log`
# (docs/open-questions.md question 245 (6), native round decision 15), for ONE purpose: running the
# board's edge service inside the Colima Docker VM.
#
# Why: this host's Colima uses `portForwarder: ssh` (~/.colima/default/colima.yaml), which forwards TCP
# only, so a UDP port a container publishes never reaches a host socket (measured: crates/av-edge-board/
# README.md, "Docker on macOS"). The kernel stays on the host and dials the service's gRPC over TCP,
# which Lima forwards (a guest 127.0.0.1:<port> listener appears on the host's 127.0.0.1:<port>); the
# service runs in the VM, where the container's published UDP port works. The VM stands in for the edge
# node the board hangs off. `crates/av-edge-board/tests/cfs_container_restart.rs` launches this binary
# through `colima ssh`. The repository is under $HOME, which the VM mounts at the same path, so the
# binary needs no copy step.
#
# This is NOT the shipping, byte-reproducible build that services/cfs/build-shim.sh is. It reuses its
# digest-pinned builder image and its pinned protoc, but the extra packages `av-edge` needs
# (libssl-dev and pkg-config: its ECDSA signing uses the system OpenSSL, ADR-004) are installed
# unpinned, and their versions are printed. The binary is linked against the builder's libssl.so.3 and
# runs in the VM against the VM's libssl.so.3 (Ubuntu 24.04, glibc 2.39, newer than the builder's 2.36).
#
# Outputs, in EDGE_BOARD_OUT_DIR (default target-docker-linux/edge-board, which is git-ignored and .dockerignored):
# av-edge-board and av-edge-board-log, with their SHA-256. Run it on the host, with bash, wrapped in the
# docker-test lock like any docker work (.venv/bin/python <lead>/bin/with_docker_lock.py -- <this>).
# Needs docker and the network (apt, crates.io), the same one-time window as build-shim.sh.
#
# Parameters (environment variables): EDGE_BOARD_OUT_DIR as above; EDGE_BOARD_MOUNT (container path of the
# repo, its parent must be /Users/probe/code, as build-shim.sh's SHIM_MOUNT; default
# /Users/probe/code/AltaVista-shimbuild); SPOORE_ROOT (host spoore checkout, default <repo>/../spoore).

if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-edge-board.sh: run this with bash (execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# rust:1.90-bookworm, by index digest: the SAME value as services/cfs/build-shim.sh (a test pins that).
RUST_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
PROTOBUF_VERSION="3.21.12-3+deb12u1"
APT_PROTOBUF="protobuf-compiler=${PROTOBUF_VERSION} libprotobuf-dev=${PROTOBUF_VERSION} libprotoc32=${PROTOBUF_VERSION} libprotobuf32=${PROTOBUF_VERSION} libprotobuf-lite32=${PROTOBUF_VERSION}"

EDGE_BOARD_MOUNT="${EDGE_BOARD_MOUNT:-/Users/probe/code/AltaVista-shimbuild}"
EDGE_BOARD_OUT_DIR="${EDGE_BOARD_OUT_DIR:-${REPO_ROOT}/target-docker-linux/edge-board}"
SPOORE_ROOT="${SPOORE_ROOT:-${REPO_ROOT}/../spoore}"
SPOORE_MOUNT="/Users/probe/code/spoore"
TARGET_MOUNT="/av-board-target"

if [ "$(dirname "${EDGE_BOARD_MOUNT}")" != "/Users/probe/code" ]; then
    echo "build-edge-board.sh: EDGE_BOARD_MOUNT must sit directly under /Users/probe/code (got ${EDGE_BOARD_MOUNT}); see build-shim.sh" >&2
    exit 2
fi
if [ ! -d "${SPOORE_ROOT}/crates/spoore-cdm" ]; then
    echo "build-edge-board.sh: no spoore checkout at ${SPOORE_ROOT} (set SPOORE_ROOT)" >&2
    exit 2
fi
SPOORE_ROOT="$(cd "${SPOORE_ROOT}" && pwd)"
mkdir -p "${EDGE_BOARD_OUT_DIR}"
EDGE_BOARD_OUT_DIR="$(cd "${EDGE_BOARD_OUT_DIR}" && pwd)"

echo "build-edge-board.sh: build inputs"
echo "  repo commit:   $(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown) ($(git -C "${REPO_ROOT}" status --porcelain 2>/dev/null | wc -l | tr -d ' ') uncommitted file(s))"
echo "  builder image: ${RUST_IMAGE}"
echo "  repo mount:    ${EDGE_BOARD_MOUNT}  output: ${EDGE_BOARD_OUT_DIR}"

CONTAINERS=()
cleanup() { [ "${#CONTAINERS[@]}" -eq 0 ] || docker rm -f "${CONTAINERS[@]}" >/dev/null 2>&1 || true; }
trap cleanup EXIT

BUILD_CTR="av-edge-board-build-$$"
CONTAINERS+=("${BUILD_CTR}")
# Both mounts are read-only; cargo's target dir is in the container's own filesystem; --locked never
# rewrites Cargo.lock. The binaries are copied out with `docker cp`.
docker create --name "${BUILD_CTR}" \
    -v "${REPO_ROOT}":"${EDGE_BOARD_MOUNT}":ro \
    -v "${SPOORE_ROOT}":"${SPOORE_MOUNT}":ro \
    -w "${EDGE_BOARD_MOUNT}" \
    "${RUST_IMAGE}" \
    sh -c "apt-get update -qq && apt-get install -y -qq --no-install-recommends ${APT_PROTOBUF} libssl-dev pkg-config \
        && dpkg-query -W libssl-dev libssl3 pkg-config | sed 's/^/  package: /' \
        && cargo build --release --locked -p av-edge-board --bins --target-dir ${TARGET_MOUNT}" >/dev/null
docker start -a "${BUILD_CTR}"
rm -f "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"
docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board"
docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board-log" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"
chmod +x "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"

echo "build-edge-board.sh: result"
shasum -a 256 "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"

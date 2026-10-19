#!/usr/bin/env bash
# services/cfs/build-edge-board.sh -- the reproducible Linux/aarch64 build of `av-edge-board` and
# `av-edge-board-log` (docs/open-questions.md question 245 (6), native round decision 15; made
# byte-reproducible by question 254, task t9), for ONE purpose: running the board's edge service inside
# the Colima Docker VM.
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
# The binary is linked against the builder's libssl.so.3 (ADR-004: `av-edge`'s ECDSA signing uses the
# system OpenSSL) and runs in the VM against the VM's libssl.so.3 (Ubuntu 24.04, glibc 2.39, newer than
# the builder's 2.36). It is not stripped (`[profile.release] debug = 1`, like build-shim.sh's unstripped
# binary): this is a test-host binary, not a shipped one.
#
# Outputs, in EDGE_BOARD_OUT_DIR (default target-docker-linux/edge-board, which is git-ignored and
# .dockerignored): av-edge-board and av-edge-board-log, with their SHA-256.
# THIS SCRIPT DOES NOT TAKE THE DOCKER-TEST LOCK: on this host run it directly, wrapped by the caller
# (`.venv/bin/python <lead>/bin/with_docker_lock.py -- services/cfs/build-edge-board.sh`), like
# build-shim.sh. Needs docker and the network (snapshot.debian.org, crates.io), the same one-time window
# as build-shim.sh (question 154).
#
# REPRODUCIBILITY (question 254 item 9). Two builds of one source tree give the same bytes from different
# repo mount paths, cargo target dirs, output dirs and host stage directories (the proof is recorded
# below). Before, the recipe was the build-shim.sh one less its pins: it installed the protobuf set and
# `libssl-dev pkg-config` from the LIVE Debian mirror and left rustc to embed the build paths; two builds
# happened to give the same bytes (071fcf20...) and nothing guaranteed it. Every input that reaches the
# binary is now pinned or erased, with the method of third_party/rtems-container/build-elf.sh and
# services/cfs/build-shim.sh:
#
#   * builder: rust:1.90-bookworm by index digest (the SAME value as build-shim.sh; a test pins that),
#     never a tag. The image already carries libssl-dev 3.0.17-1~deb12u3, libssl3 and pkg-config 1.8.1-1
#     (it is Debian's buildpack-deps), so they are inputs by the digest. The old recipe `apt-get install`ed
#     libssl-dev over the live mirror, which UPGRADED libssl-dev, libssl3 and openssl to whatever the
#     mirror then carried (3.0.22-1~deb12u1 in the 2026-10 runs). That install is dropped: nothing in the
#     build needs it (measured, "Measured" below: the bytes do not change), so the live mirror no longer
#     decides the OpenSSL headers.
#   * the extra packages: protoc (prost-build / tonic-build shell out to it in av-cdm's and spoore-cdm's
#     build.rs, so its output is an input to the binary). The five Debian protobuf packages, pinned
#     name=version in APT_PACKAGES; their closure beyond the image is empty (apt's simulation pulls no
#     other package), so those five are every .deb the build installs.
#   * the apt SOURCES: the container replaces the image's own /etc/apt/sources.list.d/debian.sources
#     (the only apt source file in the image; it names the live deb.debian.org) with snapshot.debian.org
#     at ONE fixed timestamp, SNAPSHOT_TS, for `debian` (bookworm, bookworm-updates) and `debian-security`
#     (bookworm-security), over https (the builder carries ca-certificates). Only
#     `Acquire::Check-Valid-Until=false` is relaxed (a snapshot's Release files expire, by design), on
#     `apt-get update` only; the InRelease signatures stay verified against the image's own keyring
#     (/usr/share/keyrings/debian-archive-keyring.gpg). A guard refuses (exit 5) if any apt source other
#     than that one file exists.
#   * how SNAPSHOT_TS was chosen (the method of build-elf.sh, with one correction): snapshot.debian.org's
#     machine-readable API (/mr/binary/<name>/<version>/binfiles?fileinfo=1) gives the `first_seen` of
#     each pinned arm64 .deb in its archive: all five protobuf 3.21.12-3+deb12u1 debs were first seen at
#     20260704T145040Z (archive `debian`). That is only a LOWER bound: the debs sat in the pool (a point-
#     release candidate) before bookworm's index listed them, and a build at 20260704T145040Z fails at
#     apt ("Version '3.21.12-3+deb12u1' for 'protobuf-compiler' was not found", measured). So the
#     bookworm `Packages.xz` of each later snapshot was bisected for the pin: 20260711T022200Z (Debian
#     12.14, Release dated 2026-05-16) does not list it, and SNAPSHOT_TS = 20260711T103542Z, the first
#     `debian` archive snapshot after it (Debian 12.15, Release dated 2026-07-11 10:16:37 UTC), does.
#     debian-security has no snapshot at that exact second; snapshot.debian.org serves its latest one
#     before it. The InRelease files at SNAPSHOT_TS are content-addressed by snapshot.debian.org (the 302
#     target of .../dists/<suite>/InRelease):
#       bookworm            /file/736a99b8b096598d48c438eb356bd074fecfbf23
#       bookworm-updates    /file/3fc738bfb01251ae7a90a02ea88dc87938b5e0e7
#       bookworm-security   /file/5ef0b074852e8a649d58d73218ab02c3b1682eea
#     A timestamp before 20260711T103542Z fails at apt, loudly: the perturbation proof
#     (EDGE_BOARD_SNAPSHOT_TS, non-shipping).
#   * the container then makes two checks. After the download (`apt-get install --download-only`) it
#     hashes the fetched archive set: `sha256sum` of every .deb in /var/cache/apt/archives, sorted by file
#     name, hashed again = EDGE_BOARD_DEB_SET_SHA256 below; it prints the listing (question 154's record
#     of the network fetch) and refuses (exit 4) on a mismatch before anything is installed. After the
#     install it hashes `dpkg-query -W` (every installed package, version and architecture, the base
#     image included) and refuses to build unless it equals EDGE_BOARD_DPKG_SET_SHA256 (exit 3).
#   * `cargo build --release --locked`: Cargo.lock is authoritative (a stale lock fails the build instead
#     of being rewritten; both source mounts are read-only anyway). The cargo target dir lives in the
#     container's own filesystem, so every build starts from a clean state.
#   * `--remap-path-prefix` (RUSTFLAGS, below) erases the repo mount, the spoore mount, the target dir,
#     CARGO_HOME and RUSTUP_HOME paths that rustc would otherwise embed (panic locations, debug info and
#     DW_AT_comp_dir, and the OUT_DIR paths of build-script-generated code), so the binary does not
#     depend on where it was built. The measurement of what each of these removes is under "Measured".
#
# The repository is mounted at a container path whose PARENT is literally /Users/probe/code (see
# build-shim.sh's header: spoore's relative sibling mount then lands at the fixed absolute
# /Users/probe/code/spoore that crates/av-proposer/Cargo.toml hardcodes, and cargo has to load that
# manifest whatever `-p` names). Both mounts are read-only; the binaries are copied out with `docker cp`.
#
# Parameters (environment variables; the defaults are the shipping build):
#   EDGE_BOARD_OUT_DIR       host dir for the two binaries (default <repo>/target-docker-linux/edge-board).
#   EDGE_BOARD_MOUNT         container path of the repo; its parent must be /Users/probe/code (default
#                            /Users/probe/code/AltaVista-shimbuild). Varying it is how the proof shows the
#                            remap, not coincidence, makes builds equal.
#   EDGE_BOARD_CARGO_TARGET  container path of cargo's target dir (default /av-board-target); remapped like
#                            the repo mount, and the proof varies it too.
#   SPOORE_ROOT              host spoore checkout (default <repo>/../spoore).
#   EDGE_BOARD_NO_REMAP=1    CONTROL ONLY: build without the remap flags, to show what they remove.
#   EDGE_BOARD_SNAPSHOT_TS   override the snapshot.debian.org timestamp (YYYYMMDDTHHMMSSZ). NOT the
#                            shipping build; the perturbation proof sets a timestamp before the pins.
# Non-shipping rule: a build with EDGE_BOARD_NO_REMAP or EDGE_BOARD_SNAPSHOT_TS set refuses to run unless
# EDGE_BOARD_OUT_DIR is set explicitly, so such a build never lands in the default output directory
# (build-shim.sh's SHIM_NO_REMAP rule).
#
# Measured (question 254 item 9; the commands and logs are in the t9 report):
#   * the shipping recipe, from edge worktree copies at two host stage dirs (/Users/probe/t9-edge-stage-1
#     and -2, byte-identical sources), EDGE_BOARD_MOUNT /Users/probe/code/t9-mount-a vs
#     /Users/probe/code/t9-other-mount-b, EDGE_BOARD_CARGO_TARGET /av-board-target vs /some/other/target-b,
#     two different output dirs: both builds give
#       d241d6db36b51461a87d7c69d6e373a13ce45349e1e62f78da6de3efddf38f35  av-edge-board
#       654925ce4f1cb3a4ccdcd221556098630344d4ce1f31cac8c3c7b4cb1b721338  av-edge-board-log
#     (.deb set 6c2aadcc..., dpkg set e1d35298..., both pinned above).
#   * what the remap removes: the same recipe with EDGE_BOARD_NO_REMAP=1 gives e7c87e4a... (mount a)
#     twice, and 5b3774df... (mount b, other target dir): same paths reproduce, different paths do not.
#     So the pre-question-254 recipe was reproducible only for ONE fixed path, by coincidence of its
#     defaults; the remap is what makes the bytes independent of the paths. No other leak showed: with
#     the remap the two builds are identical, so there is no build id, timestamp or host path left.
#   * the old shipping hash 071fcf20... (av-edge-board) / d35125fc... (av-edge-board-log) is reproduced
#     EXACTLY by this recipe with EDGE_BOARD_NO_REMAP=1 at the old default paths, with the image's own
#     libssl-dev 3.0.17-1~deb12u3 (no OpenSSL upgrade) and protobuf from the snapshot; the old recipe, run
#     on the same sources, also gave 071fcf20... with libssl-dev 3.0.22-1~deb12u1. So neither the OpenSSL
#     upgrade nor the live-vs-snapshot protobuf changed a byte, and the shipping hash differs from
#     071fcf20... only by the path remap (debug info and panic locations no longer name /Users/probe/code/
#     AltaVista-shimbuild and /av-board-target).
#   The hashes of a later source tree differ by definition (a different spoore or repo commit is a
#   different input, not a reproducibility failure); the build prints its inputs for that comparison.

if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-edge-board.sh: run this with bash (execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# rust:1.90-bookworm, by index digest: the SAME value as services/cfs/build-shim.sh (a test pins that).
RUST_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
# Every package the build installs beyond RUST_IMAGE: the Debian 12 protobuf set (all five carry protoc
# and the headers; one source package), sorted, name=version. Closure beyond the image: none.
APT_PACKAGES=(
    libprotobuf-dev=3.21.12-3+deb12u1
    libprotobuf-lite32=3.21.12-3+deb12u1
    libprotobuf32=3.21.12-3+deb12u1
    libprotoc32=3.21.12-3+deb12u1
    protobuf-compiler=3.21.12-3+deb12u1
)
# The apt sources: snapshot.debian.org at this one timestamp (header, "how SNAPSHOT_TS was chosen").
SNAPSHOT_TS="20260711T103542Z"
EDGE_BOARD_SNAPSHOT_TS="${EDGE_BOARD_SNAPSHOT_TS:-${SNAPSHOT_TS}}"
case "${EDGE_BOARD_SNAPSHOT_TS}" in
    [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z) ;;
    *) echo "build-edge-board.sh: EDGE_BOARD_SNAPSHOT_TS must look like 20260711T103542Z: ${EDGE_BOARD_SNAPSHOT_TS}" >&2; exit 2 ;;
esac
SNAPSHOT_DEBIAN_URL="https://snapshot.debian.org/archive/debian/${EDGE_BOARD_SNAPSHOT_TS}/"
SNAPSHOT_SECURITY_URL="https://snapshot.debian.org/archive/debian-security/${EDGE_BOARD_SNAPSHOT_TS}/"
# sha256 of the sorted (by file name, C locale) `sha256sum *.deb` listing of /var/cache/apt/archives
# after `apt-get install --download-only` of the pinned set above: the fetched archive set.
EDGE_BOARD_DEB_SET_SHA256="6c2aadcce881bb56eb3c5348f480ec2877ee629f4dd6b15fb5b15bc11397eafd"
# sha256 of the sorted `dpkg-query -W -f '${Package}:${Architecture}=${Version}\n'` listing of the builder
# container after the install above (every package, base image included). Verified inside the container
# before the build starts.
EDGE_BOARD_DPKG_SET_SHA256="e1d35298a2e4baa3ee605def72c888acc804068bd982fb9237435333156dab25"

EDGE_BOARD_MOUNT="${EDGE_BOARD_MOUNT:-/Users/probe/code/AltaVista-shimbuild}"
DEFAULT_OUT_DIR="${REPO_ROOT}/target-docker-linux/edge-board"
EDGE_BOARD_OUT_DIR="${EDGE_BOARD_OUT_DIR:-${DEFAULT_OUT_DIR}}"
SPOORE_ROOT="${SPOORE_ROOT:-${REPO_ROOT}/../spoore}"
SPOORE_MOUNT="/Users/probe/code/spoore"
TARGET_MOUNT="${EDGE_BOARD_CARGO_TARGET:-/av-board-target}"

if [ "$(dirname "${EDGE_BOARD_MOUNT}")" != "/Users/probe/code" ]; then
    echo "build-edge-board.sh: EDGE_BOARD_MOUNT must sit directly under /Users/probe/code (got ${EDGE_BOARD_MOUNT}); see build-shim.sh" >&2
    exit 2
fi
if [ ! -d "${SPOORE_ROOT}/crates/spoore-cdm" ]; then
    echo "build-edge-board.sh: no spoore checkout at ${SPOORE_ROOT} (set SPOORE_ROOT)" >&2
    exit 2
fi
NONSHIPPING=""
[ -z "${EDGE_BOARD_NO_REMAP:-}" ] || NONSHIPPING="${NONSHIPPING} EDGE_BOARD_NO_REMAP"
[ "${EDGE_BOARD_SNAPSHOT_TS}" = "${SNAPSHOT_TS}" ] || NONSHIPPING="${NONSHIPPING} EDGE_BOARD_SNAPSHOT_TS"
if [ -n "${NONSHIPPING}" ] && [ "${EDGE_BOARD_OUT_DIR}" = "${DEFAULT_OUT_DIR}" ]; then
    echo "build-edge-board.sh: non-shipping build (${NONSHIPPING# }); set EDGE_BOARD_OUT_DIR to a non-default path" >&2
    exit 2
fi
SPOORE_ROOT="$(cd "${SPOORE_ROOT}" && pwd)"
mkdir -p "${EDGE_BOARD_OUT_DIR}"
EDGE_BOARD_OUT_DIR="$(cd "${EDGE_BOARD_OUT_DIR}" && pwd)"

# Prefixes rustc must not embed (build-shim.sh's set): the repo and spoore mounts are the container paths
# of the sources, TARGET_MOUNT is cargo's target dir (OUT_DIR paths of generated code live under it), and
# the two toolchain homes are where registry crates and the std source map live in this image.
if [ -n "${EDGE_BOARD_NO_REMAP:-}" ]; then
    EDGE_BOARD_RUSTFLAGS=""
else
    EDGE_BOARD_RUSTFLAGS="--remap-path-prefix=${EDGE_BOARD_MOUNT}=/av/src --remap-path-prefix=${SPOORE_MOUNT}=/av/spoore --remap-path-prefix=${TARGET_MOUNT}=/av/target --remap-path-prefix=/usr/local/cargo=/av/cargo --remap-path-prefix=/usr/local/rustup=/av/rustup"
fi

echo "build-edge-board.sh: build inputs"
echo "  repo commit:   $(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown) ($(git -C "${REPO_ROOT}" status --porcelain 2>/dev/null | wc -l | tr -d ' ') uncommitted file(s))"
echo "  spoore commit: $(git -C "${SPOORE_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown) ($(git -C "${SPOORE_ROOT}" status --porcelain 2>/dev/null | wc -l | tr -d ' ') uncommitted file(s))"
echo "  builder image: ${RUST_IMAGE}"
echo "  apt packages:  ${#APT_PACKAGES[@]} pinned name=version entries; dpkg set sha256 ${EDGE_BOARD_DPKG_SET_SHA256}"
echo "  apt sources:   ${SNAPSHOT_DEBIAN_URL} (bookworm, bookworm-updates)"
echo "                 ${SNAPSHOT_SECURITY_URL} (bookworm-security); Check-Valid-Until=false only"
echo "  apt .deb set:  sha256 ${EDGE_BOARD_DEB_SET_SHA256} (pinned; checked in the container before install)"
echo "  repo mount:    ${EDGE_BOARD_MOUNT}  target dir: ${TARGET_MOUNT}  output: ${EDGE_BOARD_OUT_DIR}"
echo "  RUSTFLAGS:     ${EDGE_BOARD_RUSTFLAGS:-<none: CONTROL BUILD, EDGE_BOARD_NO_REMAP set>}"

# The container's work. The unquoted inner heredoc expands in the container, from the -e variables.
read -r -d '' CONTAINER_SCRIPT <<'SH' || true
set -eu
export DEBIAN_FRONTEND=noninteractive
# The image's own source file (deb.debian.org, live) is replaced by the snapshot at one timestamp.
cat > /etc/apt/sources.list.d/debian.sources <<EOS
Types: deb
URIs: ${EB_SNAPSHOT_DEBIAN_URL}
Suites: bookworm bookworm-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg

Types: deb
URIs: ${EB_SNAPSHOT_SECURITY_URL}
Suites: bookworm-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
EOS
# Nothing but that one file may name an apt source.
if [ -e /etc/apt/sources.list ] || [ "$(ls /etc/apt/sources.list.d)" != "debian.sources" ]; then
    echo "build-edge-board.sh: unexpected apt source files: $(ls /etc/apt/sources.list /etc/apt/sources.list.d 2>&1)" >&2
    exit 5
fi
apt-get -o Acquire::Check-Valid-Until=false update
# shellcheck disable=SC2086
apt-get install -y --no-install-recommends --download-only ${EB_APT_PACKAGES}
debs="$(cd /var/cache/apt/archives && sha256sum -- *.deb | LC_ALL=C sort -k2)"
echo "container .deb set ($(echo "${debs}" | wc -l | tr -d ' ') files):"
echo "${debs}"
got="$(echo "${debs}" | sha256sum | cut -d' ' -f1)"
echo "container .deb set sha256: ${got}"
if [ "${got}" != "${EB_DEB_SET_SHA256}" ]; then
    echo "build-edge-board.sh: container .deb set ${got} != pinned ${EB_DEB_SET_SHA256}" >&2
    exit 4
fi
# shellcheck disable=SC2086
apt-get install -y --no-install-recommends ${EB_APT_PACKAGES}
got="$(dpkg-query -W -f='${Package}:${Architecture}=${Version}\n' | LC_ALL=C sort | sha256sum | cut -d' ' -f1)"
echo "container dpkg set sha256: ${got}"
if [ "${got}" != "${EB_DPKG_SET_SHA256}" ]; then
    echo "build-edge-board.sh: container package set ${got} != pinned ${EB_DPKG_SET_SHA256}" >&2
    exit 3
fi
dpkg-query -W libssl-dev libssl3 pkg-config protobuf-compiler | sed 's/^/  package: /'
exec cargo build --release --locked -p av-edge-board --bins --target-dir "${EB_TARGET}"
SH

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
    -e "RUSTFLAGS=${EDGE_BOARD_RUSTFLAGS}" \
    -e "EB_APT_PACKAGES=${APT_PACKAGES[*]}" -e "EB_DEB_SET_SHA256=${EDGE_BOARD_DEB_SET_SHA256}" -e "EB_DPKG_SET_SHA256=${EDGE_BOARD_DPKG_SET_SHA256}" \
    -e "EB_SNAPSHOT_DEBIAN_URL=${SNAPSHOT_DEBIAN_URL}" -e "EB_SNAPSHOT_SECURITY_URL=${SNAPSHOT_SECURITY_URL}" -e "EB_TARGET=${TARGET_MOUNT}" \
    "${RUST_IMAGE}" \
    sh -c "${CONTAINER_SCRIPT}" >/dev/null
docker start -a "${BUILD_CTR}"
rm -f "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"
docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board"
docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-edge-board-log" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"
chmod +x "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"

echo "build-edge-board.sh: result"
shasum -a 256 "${EDGE_BOARD_OUT_DIR}/av-edge-board" "${EDGE_BOARD_OUT_DIR}/av-edge-board-log"

#!/usr/bin/env bash
# services/cfs/build-shim.sh -- the reproducible cross-build of the cFS image's lockstep shim
# (docs/open-questions.md question 236, handed on from question 232 / round 5's finding recorded in
# services/cfs/IMAGE_DIGEST.md "Re-pinned 2026-09-22"). Produces
# services/cfs/bin/av-lockstep-shim, a stripped linux/aarch64 ELF (gitignored: it is a build
# artifact, COPYed into altavista-cfs-lockstep:local by services/cfs/Dockerfile, which does not
# compile it itself -- see that Dockerfile's header for why).
#
# Run from anywhere, on the host, with bash (executed directly or `bash services/cfs/build-shim.sh`).
# Needs docker and the network (apt packages, crates.io), the same one-time window as
# build-image.sh (question 154). On this host wrap it in the docker-test lock like any docker work.
#
# Why this is a script and not a comment: two builds of the SAME commit used to give different
# bytes (round 5 measured it) because the recipe floated a tag, skipped `--locked`, and let rustc
# embed the build paths. Every input that reaches the binary is now pinned or erased:
#
#   * builder: the rust image is named by content digest. That digest is the multi-arch INDEX
#     digest of `rust:1.90-bookworm` (rustc 1.90.0, Debian 12, resolved 2026-10-05 from Docker
#     Hub's registry API: the index's linux/arm64/v8 manifest is
#     sha256:4c632e493dfa97f0fe014c3910d1690c149bba85ed8678d47d3563ec6f258ead). `docker run`
#     here (containerd image store) accepts the index digest and picks the arm64 manifest; the
#     tag is NOT used, so a re-pushed tag cannot change the compiler.
#   * `cargo build --release --locked`: Cargo.lock is authoritative; a stale lock fails the
#     build instead of being silently rewritten.
#   * protoc: prost-build / tonic-build shell out to it (spoore-cdm's and av-cdm's build.rs), so
#     its output is an input to the binary. Debian's protobuf packages are pinned to explicit
#     versions (all five that carry protoc and the headers); a version the mirror no longer
#     carries fails the build loudly rather than building with another protoc. The rust image
#     ships none of them. (libssl-dev/pkg-config are not needed: this crate pulls no TLS stack,
#     see crates/av-lockstep-shim/Cargo.toml's `tonic` comment.)
#   * `--remap-path-prefix` (below) erases the repo mount, spoore mount, target dir, CARGO_HOME
#     and RUSTUP_HOME paths that rustc would otherwise embed (panic locations, debug info, and
#     the OUT_DIR paths of build-script-generated code), so the binary does not depend on where
#     it was built.
#   * strip: with the same digest-pinned ubuntu:22.04 base as services/cfs/Dockerfile, and a
#     pinned binutils.
#
# The repository is mounted at a container path whose PARENT is literally /Users/probe/code, not
# /workspace (round 4's finding, scripts/kit/build_kit.py's CONTAINER_WORKSPACE comment): spoore's
# relative sibling mount (`spoore-cdm = { path = "../spoore/crates/spoore-cdm" }`, question 219(c))
# then lands at the fixed absolute /Users/probe/code/spoore that crates/av-proposer/Cargo.toml
# still hardcodes. Cargo has to load av-proposer's manifest to resolve the workspace whatever
# `-p` names, and a second mount or an in-container symlink at that absolute path makes cargo see
# spoore-cdm as two packages and refuse to write the lockfile (measured). Both mounts are
# read-only; the only writable mount is the target dir.
#
# Parameters (environment variables; the defaults are the shipping build):
#   SHIM_MOUNT        container path of the repo; its parent must be /Users/probe/code
#                     (default /Users/probe/code/AltaVista-shimbuild). Varying it is how
#                     the reproducibility proof shows the remap, not coincidence, makes builds equal.
#   SHIM_CARGO_TARGET container path of cargo's target dir (default /av-shim-target). It lives in
#                     the container's own filesystem, so every build starts from a clean state; it is
#                     remapped like the repo mount, and the proof varies it too.
#   SHIM_ARTIFACT_DIR host dir the unstripped and stripped binaries are copied to (default
#                     <repo>/target-docker-linux, gitignored and .dockerignored).
#   SPOORE_ROOT       host spoore checkout (default <repo>/../spoore, question 219(b)).
#   SHIM_OUT          where the stripped binary is copied (default services/cfs/bin/av-lockstep-shim).
#   SHIM_NO_REMAP=1   CONTROL ONLY: build without the remap flags, to show what they remove.
#                     Never installs to SHIM_OUT's default location (refuses unless SHIM_OUT is set
#                     explicitly to somewhere else).
#
# Outputs, in SHIM_ARTIFACT_DIR: av-lockstep-shim (unstripped, with line tables:
# `[profile.release] debug = 1`) and av-lockstep-shim-stripped. It prints the build inputs
# (this repo's commit and dirtiness under the shim's source paths, spoore's commit) and the SHA-256
# of both binaries. A different spoore commit is a different input, not a reproducibility failure.

# Refuse `sh`, the same guard as build-image.sh: bash in POSIX mode sets BASH_VERSION too, so POSIX
# mode is detected through `shopt -o posix`, and a non-bash shell fails the first test.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-shim.sh: run this with bash (\`bash services/cfs/build-shim.sh\` or execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# rust:1.90-bookworm, by index digest (see header). Never a tag.
RUST_IMAGE="rust:1.90-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f"
# The same digest-pinned base services/cfs/Dockerfile uses (question 185); the strip step runs on it.
UBUNTU_IMAGE="ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc"
# Debian 12 (bookworm), resolved 2026-10-05; all five come from the one source package.
PROTOBUF_VERSION="3.21.12-3+deb12u1"
APT_PROTOBUF="protobuf-compiler=${PROTOBUF_VERSION} libprotobuf-dev=${PROTOBUF_VERSION} libprotoc32=${PROTOBUF_VERSION} libprotobuf32=${PROTOBUF_VERSION} libprotobuf-lite32=${PROTOBUF_VERSION}"
# Ubuntu 22.04 (jammy), resolved 2026-10-05; `strip` itself is in binutils-aarch64-linux-gnu.
BINUTILS_VERSION="2.38-4ubuntu2.12"
APT_BINUTILS="binutils=${BINUTILS_VERSION} binutils-aarch64-linux-gnu=${BINUTILS_VERSION} binutils-common=${BINUTILS_VERSION} libbinutils=${BINUTILS_VERSION} libctf0=${BINUTILS_VERSION} libctf-nobfd0=${BINUTILS_VERSION}"

SHIM_MOUNT="${SHIM_MOUNT:-/Users/probe/code/AltaVista-shimbuild}"
SHIM_ARTIFACT_DIR="${SHIM_ARTIFACT_DIR:-${REPO_ROOT}/target-docker-linux}"
SPOORE_ROOT="${SPOORE_ROOT:-${REPO_ROOT}/../spoore}"
DEFAULT_OUT="${SCRIPT_DIR}/bin/av-lockstep-shim"
SHIM_OUT="${SHIM_OUT:-${DEFAULT_OUT}}"
SPOORE_MOUNT="/Users/probe/code/spoore"
TARGET_MOUNT="${SHIM_CARGO_TARGET:-/av-shim-target}"

if [ "$(dirname "${SHIM_MOUNT}")" != "/Users/probe/code" ]; then
    echo "build-shim.sh: SHIM_MOUNT must sit directly under /Users/probe/code (got ${SHIM_MOUNT}); see this script's header" >&2
    exit 2
fi
if [ ! -d "${SPOORE_ROOT}/crates/spoore-cdm" ]; then
    echo "build-shim.sh: no spoore checkout at ${SPOORE_ROOT} (set SPOORE_ROOT)" >&2
    exit 2
fi
mkdir -p "${SHIM_ARTIFACT_DIR}"
SPOORE_ROOT="$(cd "${SPOORE_ROOT}" && pwd)"
SHIM_ARTIFACT_DIR="$(cd "${SHIM_ARTIFACT_DIR}" && pwd)"

# Prefixes rustc must not embed. The repo and spoore mounts are the container paths of the sources,
# TARGET_MOUNT is cargo's target dir (OUT_DIR paths of generated code live under it), and the two
# toolchain homes are where registry crates and the std source map live in this image.
if [ -n "${SHIM_NO_REMAP:-}" ]; then
    if [ "${SHIM_OUT}" = "${DEFAULT_OUT}" ]; then
        echo "build-shim.sh: SHIM_NO_REMAP is a control build; set SHIM_OUT to a non-default path" >&2
        exit 2
    fi
    SHIM_RUSTFLAGS=""
else
    SHIM_RUSTFLAGS="--remap-path-prefix=${SHIM_MOUNT}=/av/src --remap-path-prefix=${SPOORE_MOUNT}=/av/spoore --remap-path-prefix=${TARGET_MOUNT}=/av/target --remap-path-prefix=/usr/local/cargo=/av/cargo --remap-path-prefix=/usr/local/rustup=/av/rustup"
fi

echo "build-shim.sh: build inputs"
echo "  repo commit:        $(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown)"
echo "  dirty under shim sources: $(git -C "${REPO_ROOT}" status --porcelain -- crates/av-cdm crates/av-lockstep-shim proto/altavista/v1 Cargo.toml Cargo.lock 2>/dev/null | wc -l | tr -d ' ') file(s)"
echo "  spoore commit:      $(git -C "${SPOORE_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown) ($(git -C "${SPOORE_ROOT}" status --porcelain 2>/dev/null | wc -l | tr -d ' ') uncommitted file(s))"
echo "  builder image:      ${RUST_IMAGE}"
echo "  strip image:        ${UBUNTU_IMAGE}"
echo "  protobuf packages:  ${PROTOBUF_VERSION}; binutils: ${BINUTILS_VERSION}"
echo "  repo mount:         ${SHIM_MOUNT}  target dir: ${SHIM_ARTIFACT_DIR}"
echo "  RUSTFLAGS:          ${SHIM_RUSTFLAGS:-<none: CONTROL BUILD, SHIM_NO_REMAP set>}"

# 1. Compile. Both mounts are read-only and cargo's target dir lives in the container's own
#    filesystem (a fresh container is a clean state by construction, with nothing shared from the
#    host to go stale); the binary is copied out with `docker cp`, which also works for host paths
#    the docker VM does not bind-mount (colima shares only $HOME). `--locked` never rewrites
#    Cargo.lock, so nothing is written to the repo.
CONTAINERS=()
cleanup() { [ "${#CONTAINERS[@]}" -eq 0 ] || docker rm -f "${CONTAINERS[@]}" >/dev/null 2>&1 || true; }
trap cleanup EXIT

BUILD_CTR="av-shim-build-$$"
CONTAINERS+=("${BUILD_CTR}")
docker create --name "${BUILD_CTR}" \
    -v "${REPO_ROOT}":"${SHIM_MOUNT}":ro \
    -v "${SPOORE_ROOT}":"${SPOORE_MOUNT}":ro \
    -w "${SHIM_MOUNT}" \
    -e "RUSTFLAGS=${SHIM_RUSTFLAGS}" \
    "${RUST_IMAGE}" \
    sh -c "apt-get update -qq && apt-get install -y -qq --no-install-recommends ${APT_PROTOBUF} \
        && cargo build --release --locked -p av-lockstep-shim --target-dir ${TARGET_MOUNT}" >/dev/null
docker start -a "${BUILD_CTR}"
rm -f "${SHIM_ARTIFACT_DIR}/av-lockstep-shim" "${SHIM_ARTIFACT_DIR}/av-lockstep-shim-stripped"
docker cp "${BUILD_CTR}:${TARGET_MOUNT}/release/av-lockstep-shim" "${SHIM_ARTIFACT_DIR}/av-lockstep-shim"

# 2. Strip, on the Dockerfile's own digest-pinned ubuntu base.
STRIP_CTR="av-shim-strip-$$"
CONTAINERS+=("${STRIP_CTR}")
docker create --name "${STRIP_CTR}" "${UBUNTU_IMAGE}" \
    sh -c "apt-get update -qq && apt-get install -y -qq --no-install-recommends ${APT_BINUTILS} \
        && strip -o /av-lockstep-shim-stripped /av-lockstep-shim" >/dev/null
docker cp "${SHIM_ARTIFACT_DIR}/av-lockstep-shim" "${STRIP_CTR}:/av-lockstep-shim"
docker start -a "${STRIP_CTR}"
docker cp "${STRIP_CTR}:/av-lockstep-shim-stripped" "${SHIM_ARTIFACT_DIR}/av-lockstep-shim-stripped"

# 3. Install and report.
mkdir -p "$(dirname "${SHIM_OUT}")"
cp "${SHIM_ARTIFACT_DIR}/av-lockstep-shim-stripped" "${SHIM_OUT}"
echo "build-shim.sh: result"
shasum -a 256 "${SHIM_ARTIFACT_DIR}/av-lockstep-shim" "${SHIM_ARTIFACT_DIR}/av-lockstep-shim-stripped" "${SHIM_OUT}"

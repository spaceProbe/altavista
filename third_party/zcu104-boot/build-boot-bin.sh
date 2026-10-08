#!/usr/bin/env bash
# third_party/zcu104-boot/build-boot-bin.sh -- the digest-pinned, byte-reproducible recipe for the
# ZCU104 SD-card boot image BOOT.BIN (docs/open-questions.md question 242 (d), task hilprep-5):
#
#     FSBL (Cortex-A53, AArch64) + PMU firmware (MicroBlaze) + the RTEMS 6.1 zynqmp_rpu_lock_step
#     cFS ELF on the R5 pair in lockstep, assembled by the open-source `bootgen`.
#
# THE BOARD IS NOT ON HAND. Nothing here has been booted; the image is checked STRUCTURALLY only
# (container/structural-check.py reads the finished BOOT.BIN back with `bootgen -read`).
#
# KNOWN GAP (README.md, "psu_init"): the ZCU104's PS configuration file (psu_init.c/.h, which
# Vivado generates from the board preset and which carries the DDR4 timing of the board) is NOT in
# any open source embeddedsw or U-Boot carries, so the default "standin" mode builds the FSBL with
# the ZCU102 files that embeddedsw does ship. That image is named BOOT.standin-zcu102-psuinit.bin
# and must NOT be put on a ZCU104. "supplied" mode (BOOT_PSU_INIT_DIR + its manifest hash) builds
# the same recipe with a psu_init.c/.h exported from a ZCU104 Vivado design and writes BOOT.BIN.
#
# Run from anywhere, on the host, with bash, by path. Needs docker (Colima: only $HOME is mounted
# into containers, so the staging directory sits under $HOME). THIS SCRIPT TAKES THE HOST-WIDE
# DOCKER-TEST LOCK ITSELF (question 207; step 0b below: it re-executes under
# scripts/dev/docker-lock-run.py, which holds altavista.docker_test_lock for the whole run), so do
# NOT wrap it in another lock holder (an ancestor already holding the lock is detected and not
# waited for, but the wrapper is pointless). The refusals and --print-toolchain-hash run before and
# without the lock.
#
# Three phases, each a `docker run --rm` of the same digest-pinned Debian image (no tag is made,
# no image built, committed or loaded):
#
#   1. fetch       (network; the only one)  apt closure, embeddedsw, bootgen, toolchain tarballs
#                  into BOOT_CACHE_DIR, each checked against pinned-inputs.sh / apt-debs.sha256.
#                  Runs with the network only in "fetch" mode, or in "all" mode when BOOT_FETCH=1;
#                  otherwise it runs with `--network none` and merely VERIFIES the cache (and
#                  fails, telling you to fetch, if the cache is incomplete). The one-time window
#                  of question 154 is therefore explicit.
#   2. toolchains  (no network)  binutils 2.42 + gcc 13.3.0 + newlib 4.4.0 built from the pinned
#                  tarballs for aarch64-none-elf (FSBL) and microblazeel-xilinx-elf (PMU firmware)
#                  into BOOT_CACHE_DIR/toolchain/<target>; skipped when the tree's manifest hash
#                  already equals the pin in pinned-inputs.sh.
#   3. build       (no network)  bootgen from source, FSBL and PMU firmware from embeddedsw's own
#                  makefiles (misc/copy_bsp.sh), the .bif, `bootgen -arch zynqmp -image ... -w`,
#                  then the structural check. Also `--network none`: a second build with the cache
#                  present touches no network by construction.
#
# Every input that reaches BOOT.BIN is pinned (pinned-inputs.sh: base image digest, 92 apt
# packages name=version with .deb hashes, embeddedsw and bootgen by commit and tree hash, six
# toolchain tarballs by sha256, the two toolchain trees by manifest hash, the RPU ELF by sha256,
# SOURCE_DATE_EPOCH) or erased: locale, time zone, umask fixed in the container; __DATE__/__TIME__
# in the FSBL and PMU firmware banners come from SOURCE_DATE_EPOCH (honoured by GCC >= 7); every
# build runs at fixed container paths (/work, /opt/zcu104-tc), so the host staging path, the
# output directory and the cache location never reach an artifact.
#
# Modes:   build-boot-bin.sh [all|fetch|toolchains|build]      (default all)
#          build-boot-bin.sh --print-toolchain-hash <dir>      (no docker; manifest hash of a tree)
#
# Parameters (environment variables; the defaults are the shipping build):
#   BOOT_CACHE_DIR    fetched sources, .debs, tarballs and toolchains (default <here>/cache, ignored).
#   BOOT_OUT_DIR      host dir for BOOT.BIN, fsbl.elf, pmufw.elf, boot.bif, bootgen-read.txt,
#                     structural-check.txt, SHA256SUMS, logs/ (default <here>/output, ignored).
#   BOOT_STAGE_DIR    host staging dir under $HOME, new or empty (default $HOME/av-boot-stage/build-<pid>);
#                     removed on success unless BOOT_KEEP_STAGE=1.
#   BOOT_RPU_ELF      the RTEMS cFS ELF (default third_party/rtems-container/output/elf/core-cpu1.exe in
#                     this tree or the main tree). Its SHA-256 must equal RPU_ELF_SHA256.
#   BOOT_FETCH=1      in "all" mode, allow phase 1 the network.
#   BOOT_PSU_INIT_DIR, BOOT_PSU_INIT_MANIFEST_SHA256   "supplied" mode: a directory holding psu_init.c
#                     and psu_init.h exported for the ZCU104 and the manifest hash of that directory
#                     (`--print-toolchain-hash <dir>` prints it). Both or neither.
#   BOOT_SOURCE_DATE_EPOCH  override the pinned build date (NON-SHIPPING: needs BOOT_OUT_DIR set).
#   BOOT_FSBL_DEFINE  one -D macro for the FSBL build (FSBL_DEBUG or FSBL_DEBUG_INFO: more console output;
#                     the FSBL has to fit the 0xFFFC0000 OCM region, which FSBL_DEBUG_INFO overflows with
#                     this GCC); default none.
#   BOOT_JOBS         make -j for the toolchain and image builds (default 4).
#   BOOT_WORK_TMPFS=1 build in a tmpfs at /work instead of the container filesystem (a robustness check:
#                     directory enumeration order, which GNU make 4.3 does not sort for $(wildcard), differs).
#   BOOT_KEEP_STAGE=1 keep the staging directory.
#
# Non-shipping rule: a build with BOOT_SOURCE_DATE_EPOCH set to anything but the pin, or with
# BOOT_FSBL_DEFINE set, refuses to run unless BOOT_OUT_DIR is set explicitly, so it never lands in the
# default output directory; its outputs are not compared with the pinned output hashes.

if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-boot-bin.sh: run this with bash (execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"
MAIN_TREE="$(cd "$(git -C "${REPO_ROOT}" rev-parse --git-common-dir)/.." && pwd)"

# shellcheck source=pinned-inputs.sh
. "${HERE}/pinned-inputs.sh"

# The manifest hash of a directory tree: sorted relative paths; `F <sha256>  <path>` for a regular
# file (`X` if executable), `L <target>  <path>` for a symlink; the same function runs in the
# container (container/lib.sh) for the fetched trees. Extra arguments are path prefixes (relative to
# the tree) left out of the manifest.
tree_manifest_hash() {
    python3 -I - "$@" <<'PY'
import hashlib, os, sys

root = sys.argv[1]
excludes = tuple(sys.argv[2:])
entries = []
for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
    if dirpath == root and ".git" in dirnames:
        dirnames.remove(".git")
    for name in dirnames + filenames:
        full = os.path.join(dirpath, name)
        rel = os.path.relpath(full, root)
        if excludes and rel.startswith(excludes):
            continue
        if os.path.islink(full):
            entries.append((rel, "L " + os.readlink(full) + "  " + rel))
        elif os.path.isfile(full):
            h = hashlib.sha256()
            with open(full, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            tag = "X" if os.access(full, os.X_OK) else "F"
            entries.append((rel, tag + " " + h.hexdigest() + "  " + rel))
        elif not os.path.isdir(full):
            sys.exit("build-boot-bin.sh: unsupported file type in manifest: " + full)
entries.sort(key=lambda e: e[0].encode("utf-8", "surrogateescape"))
text = "".join(line + "\n" for _, line in entries)
print(hashlib.sha256(text.encode("utf-8", "surrogateescape")).hexdigest())
PY
}

if [ "${1:-}" = "--print-toolchain-hash" ]; then
    tree_manifest_hash "${2:?usage: --print-toolchain-hash <dir> [excluded-prefix ...]}" "${@:3}"
    exit 0
fi

MODE="${1:-all}"
case "${MODE}" in all|fetch|toolchains|build) ;; *) echo "build-boot-bin.sh: unknown mode ${MODE}" >&2; exit 2 ;; esac

BOOT_CACHE_DIR="${BOOT_CACHE_DIR:-${HERE}/cache}"
DEFAULT_OUT_DIR="${HERE}/output"
BOOT_OUT_DIR="${BOOT_OUT_DIR:-${DEFAULT_OUT_DIR}}"
BOOT_STAGE_DIR="${BOOT_STAGE_DIR:-${HOME}/av-boot-stage/build-$$}"
BOOT_JOBS="${BOOT_JOBS:-4}"
SDE="${BOOT_SOURCE_DATE_EPOCH:-${PINNED_SOURCE_DATE_EPOCH}}"
if [ -z "${BOOT_RPU_ELF:-}" ]; then
    for cand in "${REPO_ROOT}/third_party/rtems-container/output/elf/core-cpu1.exe" \
                "${MAIN_TREE}/third_party/rtems-container/output/elf/core-cpu1.exe"; do
        [ -f "${cand}" ] && { BOOT_RPU_ELF="${cand}"; break; }
    done
fi

# ---- 0. Refusals, before anything is staged or any docker command runs.
NONSHIPPING=""
[ "${SDE}" = "${PINNED_SOURCE_DATE_EPOCH}" ] || NONSHIPPING="${NONSHIPPING} BOOT_SOURCE_DATE_EPOCH"
[ -z "${BOOT_FSBL_DEFINE:-}" ] || NONSHIPPING="${NONSHIPPING} BOOT_FSBL_DEFINE"
if [ -n "${NONSHIPPING}" ] && [ "${BOOT_OUT_DIR}" = "${DEFAULT_OUT_DIR}" ]; then
    echo "build-boot-bin.sh: non-shipping build (${NONSHIPPING# }); set BOOT_OUT_DIR to a non-default path" >&2
    exit 2
fi
elf_sha=""
if [ "${MODE}" = build ] || [ "${MODE}" = all ]; then
    # The RPU application is an input like any other: pinned by SHA-256, checked before anything runs.
    [ -n "${BOOT_RPU_ELF:-}" ] && [ -f "${BOOT_RPU_ELF}" ] || { echo "build-boot-bin.sh: no RPU ELF (set BOOT_RPU_ELF)" >&2; exit 2; }
    elf_sha="$(shasum -a 256 "${BOOT_RPU_ELF}" | cut -d' ' -f1)"
    if [ "${elf_sha}" != "${RPU_ELF_SHA256}" ]; then
        echo "build-boot-bin.sh: RPU ELF sha256 ${elf_sha} != pinned ${RPU_ELF_SHA256}" >&2
        exit 2
    fi
fi
case "${BOOT_STAGE_DIR}" in
    "${HOME}"/*) ;;
    *) echo "build-boot-bin.sh: BOOT_STAGE_DIR must be under \$HOME (Colima mounts only \$HOME): ${BOOT_STAGE_DIR}" >&2; exit 2 ;;
esac
if [ -e "${BOOT_STAGE_DIR}" ] && [ -n "$(ls -A "${BOOT_STAGE_DIR}" 2>/dev/null)" ]; then
    echo "build-boot-bin.sh: BOOT_STAGE_DIR exists and is not empty: ${BOOT_STAGE_DIR}" >&2
    exit 2
fi
case "${BOOT_CACHE_DIR}${BOOT_OUT_DIR}" in
    *"${REPO_ROOT}/third_party/cfs"*|*"${MAIN_TREE}/third_party/cfs"*)
        echo "build-boot-bin.sh: refusing to write into third_party/cfs" >&2; exit 2 ;;
esac
case "${BOOT_CACHE_DIR}" in "${HOME}"/*) ;; *) echo "build-boot-bin.sh: BOOT_CACHE_DIR must be under \$HOME: ${BOOT_CACHE_DIR}" >&2; exit 2 ;; esac
case "${BOOT_OUT_DIR}" in "${HOME}"/*) ;; *) echo "build-boot-bin.sh: BOOT_OUT_DIR must be under \$HOME: ${BOOT_OUT_DIR}" >&2; exit 2 ;; esac
if [ -n "${BOOT_PSU_INIT_DIR:-}" ] || [ -n "${BOOT_PSU_INIT_MANIFEST_SHA256:-}" ]; then
    if [ -z "${BOOT_PSU_INIT_DIR:-}" ] || [ -z "${BOOT_PSU_INIT_MANIFEST_SHA256:-}" ]; then
        echo "build-boot-bin.sh: BOOT_PSU_INIT_DIR and BOOT_PSU_INIT_MANIFEST_SHA256 go together" >&2; exit 2
    fi
    PSU_INIT_MODE=supplied
    BOOT_BIN_NAME="BOOT.BIN"
else
    PSU_INIT_MODE=standin
    BOOT_BIN_NAME="BOOT.standin-zcu102-psuinit.bin"
fi

# ---- 0b. The host-wide docker-test lock (question 207). The refusals above need no docker and take no
# lock. From here on every phase runs docker, so this script re-executes itself under
# scripts/dev/docker-lock-run.py, which holds altavista.docker_test_lock.lock_docker_tests() (and its
# holder sidecar, naming that helper's pid) for the whole run and exports AV_DOCKER_LOCK_HELD to the
# re-executed script, which therefore does not take it again. The lock is released when the run ends
# by any means. It is held for the whole recipe, a toolchain build included.
if [ -z "${AV_DOCKER_LOCK_HELD:-}" ]; then
    LOCK_PY="${REPO_ROOT}/.venv/bin/python"
    [ -x "${LOCK_PY}" ] || LOCK_PY="${MAIN_TREE}/.venv/bin/python"
    if [ ! -x "${LOCK_PY}" ]; then
        echo "build-boot-bin.sh: no .venv/bin/python in ${REPO_ROOT} or ${MAIN_TREE} to take the docker-test lock with" >&2
        exit 2
    fi
    exec "${LOCK_PY}" "${REPO_ROOT}/scripts/dev/docker-lock-run.py" -- "${BASH_SOURCE[0]}" "$@"
fi

mkdir -p "${BOOT_CACHE_DIR}"
BOOT_CACHE_DIR="$(cd "${BOOT_CACHE_DIR}" && pwd)"

CONTAINER_PREFIX="av-boot-$$"
RUNNING_STAGE_OK=""
cleanup() {
    docker rm -f "${CONTAINER_PREFIX}-1" "${CONTAINER_PREFIX}-2a" "${CONTAINER_PREFIX}-2m" "${CONTAINER_PREFIX}-3" >/dev/null 2>&1 || true
    if [ -n "${RUNNING_STAGE_OK}" ] && [ -z "${BOOT_KEEP_STAGE:-}" ]; then
        rm -rf "${BOOT_STAGE_DIR}" 2>/dev/null || echo "build-boot-bin.sh: could not remove ${BOOT_STAGE_DIR}" >&2
    fi
}
trap cleanup EXIT

docker_base() {  # name network extra-args... -- (the entrypoint script is appended by the caller)
    local name="$1" net="$2"; shift 2
    docker run --rm --name "${name}" --network "${net}" \
        -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
        -v "${HERE}":/recipe:ro "$@"
}

# ---- 1. Fetch (or verify) the cache.
phase1() {
    local net="none"
    if [ "${MODE}" = "fetch" ] || [ -n "${BOOT_FETCH:-}" ]; then net="bridge"; fi
    echo "build-boot-bin.sh: phase 1, fetch/verify the cache (network: ${net}) -> ${BOOT_CACHE_DIR}"
    mkdir -p "${BOOT_CACHE_DIR}"/{apt,src,tarballs}
    docker_base "${CONTAINER_PREFIX}-1" "${net}" -v "${BOOT_CACHE_DIR}":/cache \
        --entrypoint /bin/bash "${BASE_IMAGE}" /recipe/container/phase1-fetch.sh \
        || { echo "build-boot-bin.sh: the cache is incomplete or does not verify; run '$0 fetch' (the one-time network window)" >&2; exit 4; }
}

# ---- 2. Toolchains.
toolchain_dir() { echo "${BOOT_CACHE_DIR}/toolchain/$1"; }
toolchain_pin() {
    case "$1" in
        aarch64-none-elf) echo "${AARCH64_TOOLCHAIN_MANIFEST_SHA256}" ;;
        microblazeel-xilinx-elf) echo "${MICROBLAZE_TOOLCHAIN_MANIFEST_SHA256}" ;;
    esac
}
# The manifest hash of a built toolchain tree. The MicroBlaze tree leaves out its target libraries
# (MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE, pinned-inputs.sh: not bit-reproducible from run to run).
toolchain_hash() {  # target dir
    case "$1" in
        microblazeel-xilinx-elf) tree_manifest_hash "$2" ${MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE} ;;
        *) tree_manifest_hash "$2" ;;
    esac
}
phase2() {
    local t dir pin got cname
    for t in aarch64-none-elf microblazeel-xilinx-elf; do
        dir="$(toolchain_dir "${t}")"; pin="$(toolchain_pin "${t}")"
        if [ -x "${dir}/bin/${t}-gcc" ]; then
            got="$(toolchain_hash "${t}" "${dir}")"
            if [ "${got}" = "${pin}" ]; then
                echo "build-boot-bin.sh: toolchain ${t} present, manifest ${got} (pinned)"
                continue
            fi
            echo "build-boot-bin.sh: toolchain ${t} manifest ${got} != pinned ${pin}; rebuilding" >&2
        fi
        echo "build-boot-bin.sh: phase 2, building the ${t} toolchain from the pinned tarballs (no network)"
        mkdir -p "${BOOT_CACHE_DIR}/toolchain"
        rm -rf "${dir}"
        cname="${CONTAINER_PREFIX}-2$(echo "${t}" | cut -c1)"
        docker_base "${cname}" none -e "TC_TARGET=${t}" -e "TC_JOBS=${BOOT_JOBS}" \
            -v "${BOOT_CACHE_DIR}/apt":/cache/apt:ro -v "${BOOT_CACHE_DIR}/tarballs":/cache/tarballs:ro \
            -v "${BOOT_CACHE_DIR}/toolchain":/opt/zcu104-tc \
            --entrypoint /bin/bash "${BASE_IMAGE}" /recipe/container/phase2-toolchains.sh
        # The tree built into a shared /opt/zcu104-tc; split it per target.
        got="$(toolchain_hash "${t}" "${dir}")"
        echo "build-boot-bin.sh: toolchain ${t} manifest ${got} (pinned ${pin})"
        if [ "${got}" != "${pin}" ]; then
            echo "build-boot-bin.sh: toolchain ${t} does not match its pin" >&2
            exit 4
        fi
    done
}

# ---- 3. Build.
phase3() {
    local t
    for t in aarch64-none-elf microblazeel-xilinx-elf; do
        [ "$(toolchain_hash "${t}" "$(toolchain_dir "${t}")")" = "$(toolchain_pin "${t}")" ] \
            || { echo "build-boot-bin.sh: toolchain ${t} missing or not matching its pin (run phase 2)" >&2; exit 2; }
    done
    local psu_sha=""
    mkdir -p "${BOOT_STAGE_DIR}"
    BOOT_STAGE_DIR="$(cd "${BOOT_STAGE_DIR}" && pwd)"
    cp "${BOOT_RPU_ELF}" "${BOOT_STAGE_DIR}/core-cpu1.exe"
    if [ "${PSU_INIT_MODE}" = supplied ]; then
        psu_sha="$(tree_manifest_hash "${BOOT_PSU_INIT_DIR}")"
        if [ "${psu_sha}" != "${BOOT_PSU_INIT_MANIFEST_SHA256}" ]; then
            echo "build-boot-bin.sh: ${BOOT_PSU_INIT_DIR} manifest ${psu_sha} != BOOT_PSU_INIT_MANIFEST_SHA256" >&2
            exit 2
        fi
        mkdir -p "${BOOT_STAGE_DIR}/psu_init"
        cp "${BOOT_PSU_INIT_DIR}/psu_init.c" "${BOOT_PSU_INIT_DIR}/psu_init.h" "${BOOT_STAGE_DIR}/psu_init/"
    fi
    mkdir -p "${BOOT_OUT_DIR}"
    BOOT_OUT_DIR="$(cd "${BOOT_OUT_DIR}" && pwd)"

    echo "build-boot-bin.sh: build inputs"
    echo "  repo commit:         $(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "  builder image:       ${BASE_IMAGE}"
    echo "  apt packages:        ${#APT_PACKAGES[@]} pinned name=version entries; dpkg set sha256 ${DPKG_SET_SHA256}"
    echo "  embeddedsw:          ${EMBEDDEDSW_COMMIT} (tree ${EMBEDDEDSW_TREE_SHA256})"
    echo "  bootgen:             ${BOOTGEN_COMMIT} (tree ${BOOTGEN_TREE_SHA256})"
    echo "  aarch64 toolchain:   manifest ${AARCH64_TOOLCHAIN_MANIFEST_SHA256}"
    echo "  microblaze toolchain: manifest ${MICROBLAZE_TOOLCHAIN_MANIFEST_SHA256}"
    echo "  RPU ELF:             ${BOOT_RPU_ELF} sha256 ${elf_sha}"
    echo "  psu_init:            ${PSU_INIT_MODE}$([ -n "${psu_sha}" ] && echo " (manifest ${psu_sha})" || echo " -- ZCU102 files from embeddedsw; NOT for a ZCU104")"
    echo "  SOURCE_DATE_EPOCH:   ${SDE}"
    echo "  staging dir:         ${BOOT_STAGE_DIR}"
    echo "  output dir:          ${BOOT_OUT_DIR}"

    echo "build-boot-bin.sh: phase 3, building bootgen, FSBL, PMU firmware and ${BOOT_BIN_NAME} (no network)"
    local tmpfs_args=()
    [ -z "${BOOT_WORK_TMPFS:-}" ] || tmpfs_args=(--tmpfs /work:exec,mode=755,size=6g)
    docker_base "${CONTAINER_PREFIX}-3" none \
        ${tmpfs_args[@]+"${tmpfs_args[@]}"} \
        -e "PSU_INIT_MODE=${PSU_INIT_MODE}" -e "SOURCE_DATE_EPOCH=${SDE}" -e "PIN_CHECK=$([ -z "${NONSHIPPING}" ] && echo 1 || echo 0)" \
        -e "FSBL_DEBUG_DEFINE=${BOOT_FSBL_DEFINE:-}" -e "RPU_ELF_NAME=core-cpu1.exe" -e "BOOT_BIN_NAME=${BOOT_BIN_NAME}" -e "BOOT_JOBS=${BOOT_JOBS}" \
        -v "${BOOT_CACHE_DIR}/apt":/cache/apt:ro -v "${BOOT_CACHE_DIR}/src":/cache/src:ro \
        -v "${BOOT_CACHE_DIR}/toolchain":/opt/zcu104-tc:ro \
        -v "${BOOT_STAGE_DIR}":/stage:ro -v "${BOOT_OUT_DIR}":/out \
        --entrypoint /bin/bash "${BASE_IMAGE}" /recipe/container/phase3-build.sh \
        2>&1 | tee "${BOOT_OUT_DIR}/build-docker.log"
    [ "${PIPESTATUS[0]}" = 0 ] || { echo "build-boot-bin.sh: the build container failed (see ${BOOT_OUT_DIR}/build-docker.log)" >&2; exit 1; }
    RUNNING_STAGE_OK=1

    echo "build-boot-bin.sh: result"
    ( cd "${BOOT_OUT_DIR}" && shasum -a 256 "${BOOT_BIN_NAME}" fsbl.elf pmufw.elf boot.bif )
}

case "${MODE}" in
    fetch) phase1 ;;
    toolchains) phase1; phase2 ;;
    build) phase1; phase2; phase3 ;;
    all) phase1; phase2; phase3 ;;
esac

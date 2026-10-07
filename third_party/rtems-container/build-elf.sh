#!/usr/bin/env bash
# third_party/rtems-container/build-elf.sh -- the reproducible cross-build of the RTEMS 6.1
# `zynqmp_rpu_lock_step` cFS ELF (`core-cpu1.exe`), the artifact the Renode half of
# docs/open-questions.md question 145 runs. Handed on from question 171 / sil-plan's 2026-10-05
# status ("Found, not fixed" 2: two builds of the same inputs differed in 12 bytes, three after
# pinning cFE's build metadata) and carried over by question 239 (task native-co-3).
#
# Produces `core-cpu1.exe` in ELF_OUT_DIR (default third_party/rtems-container/output/elf, which is
# gitignored by the `third_party/rtems-container/output/` rule) and prints its SHA-256. Two clean
# builds from different host staging paths give the identical SHA-256.
#
# Run from anywhere, on the host, with bash, by path (`third_party/rtems-container/build-elf.sh`).
# Needs docker (Colima: only $HOME is mounted into containers, so the staging directory sits
# under $HOME) and the network for the apt packages once per build (question 154's one-time
# window; the cFS sources themselves come from the local bare mirrors and touch no network).
# THIS SCRIPT DOES NOT TAKE THE DOCKER-TEST LOCK: on this host wrap it in
# `.venv/bin/python <lock wrapper> -- third_party/rtems-container/build-elf.sh`, like
# services/cfs/build-shim.sh (question 207's host-wide lock; tests take the lock themselves).
#
# Every input that reaches the ELF is pinned, erased, or measured and printed:
#
#   * cFS sources: NOT the mutable third_party/cfs tree. `third_party/fetch-cfs.sh` is run with
#     CFS_FETCH_DEST pointing into a FRESH staging directory (its own existing parameter, so
#     fetch-cfs.sh is unchanged) against the local bare mirrors (CFS_MIRROR_DIR, default the main
#     tree's third_party/mirrors). It checks out the seven pinned commits (bundle v7.0.1
#     088b2fa8..., cfe, osal, psp, three tools) and applies the carried PSP patch from
#     third_party/renode/M24_4/patches. GIT_ALLOW_PROTOCOL=file makes any attempt to leave the
#     local mirrors (a missing commit would trigger `git fetch origin`) fail loudly instead of
#     silently using the network; set ELF_ALLOW_NETWORK=1 to permit it. Nothing is ever written
#     into third_party/cfs or the mirrors.
#   * the lockstep apps, PSP and mission defs: this worktree's services/cfs/{apps,psp-lockstep,
#     build} (working tree, minus gitignored files), copied with the single file
#     build-cfs-cross.sh and fetch-cfs.sh into the staging directory at the layout
#     build-cfs-cross.sh expects under /workspace. Those are the only paths build-cfs-cross.sh
#     reads (SVC_CFS, its own location, and fetch-cfs.sh); the rest of services/cfs and
#     third_party/rtems-container is not an input.
#   * builder: `debian:bookworm-slim` by content digest (the same image the question 171 builds
#     used), `docker run --rm`, no tag created, no image built. The apt packages are the complete
#     closure the last build installed (93 packages, `name=version`); a version the Debian mirror
#     no longer carries makes `apt-get install` fail, loudly. After the install the container
#     hashes `dpkg-query -W` (every installed package, version and architecture) and refuses to
#     build unless it equals ELF_DPKG_SET_SHA256 below, so a dependency that floated is caught too.
#   * toolchain: the RTEMS 6.1 toolchain tree (compiler, linker, BSP, rtems-syms) is mounted
#     read-only at /output/toolchain (the toolchain cmake hardcodes that path). Its content is
#     PINNED: a manifest of the tree -- one line per entry, sorted by relative path, `F <sha256>
#     <path>` for a regular file and `L <target> <path>` for a symlink -- is hashed, and the value
#     must equal TOOLCHAIN_MANIFEST_SHA256 below. A mismatch refuses to build unless
#     ELF_TOOLCHAIN_ALLOW_MISMATCH=1. `build-elf.sh --print-toolchain-hash` prints the value for
#     ELF_TOOLCHAIN_DIR and exits (no docker).
#   * cFE's build metadata: cfe/cmake/generate_build_env.cmake reads the environment variables
#     BUILDDATE, HOSTNAME and USER (and runs `date`/`hostname`/`whoami` only when they are
#     empty). They are pinned to the fixed values below; they are printed by cFE at boot
#     ("Build 202610050000 by altavista@altavista-elf-build") and sit in the ELF's cfe_build_env
#     table, so an unpinned build differs from the pinned one by exactly these bytes.
#   * the rtems-syms temporary file name (the root cause of the last 3 differing bytes). The cFS
#     PSP's psp/cmake/Modules/Platform/RTEMS.cmake links twice for RTEMS dynamic loading: a
#     `<TARGET>-prelink` executable, then `rtems-syms -v -e -c ... -o <TARGET>-dl-sym.o
#     <TARGET>-prelink`, which writes the embedded symbol table as a C file, compiles it and links
#     the object into the final ELF. Without `-S` rtems-syms names that C file "cc" + its own
#     process id in base 62 (alphabet a-z A-Z 0-9, least significant digit first, padded with `a`
#     to 6 characters; log line "symbol C file: /tmp/ccN8aaaa.c" is pid 3759). Measured in the
#     builder container: pid 8 -> cciaaaaa, 14 -> ccoaaaaa, 27 -> ccBaaaaa, 133 -> ccjcaaaa. So the
#     name depends on how many processes the container had started before rtems-syms ran (apt,
#     the make schedule), and the compiler records it as an STT_FILE symbol (`cc2gbaaa.c`, pid
#     4270, in the question 171 ELF) in the final ELF's symbol table; the name's length also
#     shifts the string table behind it. Identical process histories give identical names by
#     coincidence only.
#     `rtems-syms --help` offers `-S file : symbol's C file (also --symc)`, so the staged
#     RTEMS.cmake is edited, idempotently, to pass `-S <TARGET>-dl-sym.c` (a fixed name next to
#     the other link outputs). The edit is a one-line text substitution on the STAGED copy
#     (asserted to apply exactly once, or to be already applied); no ELF bytes are post-processed.
#
# Paths do not leak into the ELF: the container sees the sources at /workspace and the toolchain
# at /output/toolchain whatever the host staging path is, so varying the staging path (the proof
# does) changes nothing.
#
# Parameters (environment variables; the defaults are the shipping build):
#   ELF_OUT_DIR       host dir for core-cpu1.exe, build-logs/ and build-docker.log (default
#                     <repo>/third_party/rtems-container/output/elf, gitignored).
#   ELF_STAGE_DIR     host staging dir; must be a new or empty directory under $HOME (default
#                     $HOME/av-elf-stage/build-<pid>). Removed on success unless ELF_KEEP_STAGE=1.
#   CFS_MIRROR_DIR    local bare mirrors (default the main tree's third_party/mirrors).
#   ELF_TOOLCHAIN_DIR the toolchain tree (default <repo>/third_party/rtems-container/output/toolchain
#                     if it exists, else the main tree's).
#   ELF_TOOLCHAIN_ALLOW_MISMATCH=1   build with a toolchain whose manifest hash differs (NOT the
#                     shipping build; see the non-shipping rule below).
#   ELF_BUILDDATE / ELF_BUILDHOST / ELF_BUILDUSER   override the pinned cFE metadata (the
#                     perturbation proof sets ELF_BUILDDATE).
#   ELF_NO_SYMS_FIX=1 CONTROL ONLY: leave RTEMS.cmake unedited, so rtems-syms names its C file
#                     after its pid again (the control build shows which bytes the fix removes).
#   ELF_ALLOW_NETWORK=1  lift the GIT_ALLOW_PROTOCOL=file guard around fetch-cfs.sh.
#   ELF_KEEP_STAGE=1  keep the staging directory (it is root-owned in part; remove it with docker
#                     or sudo if the host user cannot).
#
# Non-shipping rule: a build with any of ELF_TOOLCHAIN_ALLOW_MISMATCH, ELF_BUILDDATE,
# ELF_BUILDHOST, ELF_BUILDUSER or ELF_NO_SYMS_FIX set refuses to run unless ELF_OUT_DIR is set
# explicitly, so such a build never lands in the default output directory (build-shim.sh's
# SHIM_NO_REMAP rule).

# Refuse `sh`, the same guard as build-shim.sh: bash in POSIX mode sets BASH_VERSION too, so POSIX
# mode is detected through `shopt -o posix`, and a non-bash shell fails the first test.
if [ -z "${BASH_VERSION:-}" ] || shopt -qo posix 2>/dev/null; then
    echo "build-elf.sh: run this with bash (execute it directly), not sh" >&2
    exit 2
fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# The main working tree (a worktree's .git points at <main>/.git/worktrees/<name>); the gitignored
# artifacts (mirrors, toolchain) live there.
MAIN_TREE="$(cd "$(git -C "${REPO_ROOT}" rev-parse --git-common-dir)/.." && pwd)"

# debian:bookworm-slim, by digest (arm64 manifest; the same image the question 171 builds used).
# Never a tag.
BASE_IMAGE="debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171"

# The complete package closure the question 171 build installed on this image (apt-get install
# build-essential cmake python3 file, --no-install-recommends), resolved 2026-10-05.
APT_PACKAGES=(
    binutils-aarch64-linux-gnu=2.40-2
    binutils-common=2.40-2
    binutils=2.40-2
    build-essential=12.9
    bzip2=1.0.8-5+b1
    cmake-data=3.25.1-1
    cmake=3.25.1-1
    cpp-12=12.2.0-14+deb12u1
    cpp=4:12.2.0-3
    dpkg-dev=1.21.23
    file=1:5.44-3
    g++-12=12.2.0-14+deb12u1
    g++=4:12.2.0-3
    gcc-12=12.2.0-14+deb12u1
    gcc=4:12.2.0-3
    libarchive13=3.6.2-1+deb12u5
    libasan8=12.2.0-14+deb12u1
    libatomic1=12.2.0-14+deb12u1
    libbinutils=2.40-2
    libbrotli1=1.0.9-2+b6
    libc-dev-bin=2.36-9+deb12u14
    libc6-dev=2.36-9+deb12u14
    libcc1-0=12.2.0-14+deb12u1
    libcrypt-dev=1:4.4.33-2
    libctf-nobfd0=2.40-2
    libctf0=2.40-2
    libcurl4=7.88.1-10+deb12u15
    libdpkg-perl=1.21.23
    libexpat1=2.5.0-1+deb12u4
    libgcc-12-dev=12.2.0-14+deb12u1
    libgdbm-compat4=1.23-3
    libgdbm6=1.23-3
    libgomp1=12.2.0-14+deb12u1
    libgprofng0=2.40-2
    libgssapi-krb5-2=1.20.1-2+deb12u5
    libhwasan0=12.2.0-14+deb12u1
    libicu72=72.1-3+deb12u1
    libisl23=0.25-1.1
    libitm1=12.2.0-14+deb12u1
    libjansson4=2.14-2
    libjsoncpp25=1.9.5-4
    libk5crypto3=1.20.1-2+deb12u5
    libkeyutils1=1.6.3-2
    libkrb5-3=1.20.1-2+deb12u5
    libkrb5support0=1.20.1-2+deb12u5
    libldap-2.5-0=2.5.13+dfsg-5
    liblsan0=12.2.0-14+deb12u1
    liblzma5=5.4.1-1+deb12u2
    libmagic-mgc=1:5.44-3
    libmagic1=1:5.44-3
    libmpc3=1.3.1-1
    libmpfr6=4.2.0-1
    libncursesw6=6.4-4
    libnghttp2-14=1.52.0-1+deb12u3
    libnsl-dev=1.3.0-2
    libnsl2=1.3.0-2
    libperl5.36=5.36.0-7+deb12u4
    libproc2-0=2:4.0.2-3
    libpsl5=0.21.2-1
    libpython3-stdlib=3.11.2-1+b1
    libpython3.11-minimal=3.11.2-6+deb12u8
    libpython3.11-stdlib=3.11.2-6+deb12u8
    libreadline8=8.2-1.3
    librhash0=1.4.3-3
    librtmp1=2.4+20151223.gitfa8646d.1-2+b2
    libsasl2-2=2.1.28+dfsg-10
    libsasl2-modules-db=2.1.28+dfsg-10
    libsqlite3-0=3.40.1-2+deb12u2
    libssh2-1=1.10.0-3+deb12u1
    libssl3=3.0.22-1~deb12u1
    libstdc++-12-dev=12.2.0-14+deb12u1
    libtirpc-common=1.3.3+ds-1
    libtirpc-dev=1.3.3+ds-1
    libtirpc3=1.3.3+ds-1
    libtsan2=12.2.0-14+deb12u1
    libubsan1=12.2.0-14+deb12u1
    libuv1=1.44.2-1+deb12u1
    libxml2=2.9.14+dfsg-1.3~deb12u6
    linux-libc-dev=6.1.187-1
    make=4.3-4.1
    media-types=10.0.0
    patch=2.7.6-7
    perl-base=5.36.0-7+deb12u4
    perl-modules-5.36=5.36.0-7+deb12u4
    perl=5.36.0-7+deb12u4
    procps=2:4.0.2-3
    python3-minimal=3.11.2-1+b1
    python3.11-minimal=3.11.2-6+deb12u8
    python3.11=3.11.2-6+deb12u8
    python3=3.11.2-1+b1
    readline-common=8.2-1.3
    rpcsvc-proto=1.4.3-1
    xz-utils=5.4.1-1+deb12u2
)
# sha256 of the sorted `dpkg-query -W -f '${Package}:${Architecture}=${Version}\n'` listing of the
# builder container after the install above (every package, base image included). Verified inside
# the container before the build starts.
ELF_DPKG_SET_SHA256="19ed31e506cc179f2a7e7520985e6f56b9fb674302049dde380ee2102cfb7683"

# sha256 of the toolchain tree's manifest (see header). Measured 2026-10-07 on the toolchain built
# by third_party/rtems-container/build-bsp-fix.sh (M24.2c) in the main tree.
TOOLCHAIN_MANIFEST_SHA256="b56b32d856985fcfe2466c41dea26ffafe60f5de843af1e38c444eade635b366"

# cFE's build metadata, fixed (documented in the header). The date is the day the inputs were
# pinned; the host and user are labels, not the machine that ran the build.
PINNED_BUILDDATE="202610050000"
PINNED_BUILDHOST="altavista-elf-build"
PINNED_BUILDUSER="altavista"

ELF_BUILDDATE="${ELF_BUILDDATE:-${PINNED_BUILDDATE}}"
ELF_BUILDHOST="${ELF_BUILDHOST:-${PINNED_BUILDHOST}}"
ELF_BUILDUSER="${ELF_BUILDUSER:-${PINNED_BUILDUSER}}"

DEFAULT_OUT_DIR="${REPO_ROOT}/third_party/rtems-container/output/elf"
ELF_OUT_DIR="${ELF_OUT_DIR:-${DEFAULT_OUT_DIR}}"
ELF_STAGE_DIR="${ELF_STAGE_DIR:-${HOME}/av-elf-stage/build-$$}"
CFS_MIRROR_DIR="${CFS_MIRROR_DIR:-${MAIN_TREE}/third_party/mirrors}"
if [ -z "${ELF_TOOLCHAIN_DIR:-}" ]; then
    if [ -d "${REPO_ROOT}/third_party/rtems-container/output/toolchain" ]; then
        ELF_TOOLCHAIN_DIR="${REPO_ROOT}/third_party/rtems-container/output/toolchain"
    else
        ELF_TOOLCHAIN_DIR="${MAIN_TREE}/third_party/rtems-container/output/toolchain"
    fi
fi

# The manifest hash of a directory tree (see header): sorted relative paths; `F <sha256>  <path>`
# for regular files, `L <target>  <path>` for symlinks (directories are implied by their entries;
# anything else -- a socket, a fifo -- is an error).
tree_manifest_hash() {
    python3 -I - "$1" <<'PY'
import hashlib, os, sys

root = sys.argv[1]
entries = []
for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
    for name in dirnames + filenames:
        full = os.path.join(dirpath, name)
        rel = os.path.relpath(full, root)
        if os.path.islink(full):
            entries.append((rel, "L " + os.readlink(full) + "  " + rel))
        elif os.path.isfile(full):
            h = hashlib.sha256()
            with open(full, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            entries.append((rel, "F " + h.hexdigest() + "  " + rel))
        elif not os.path.isdir(full):
            sys.exit("build-elf.sh: unsupported file type in manifest: " + full)
entries.sort(key=lambda e: e[0].encode("utf-8", "surrogateescape"))
text = "".join(line + "\n" for _, line in entries)
print(hashlib.sha256(text.encode("utf-8", "surrogateescape")).hexdigest())
PY
}

if [ "${1:-}" = "--print-toolchain-hash" ]; then
    tree_manifest_hash "${ELF_TOOLCHAIN_DIR}"
    exit 0
fi

# ---- 0. Refusals, before anything is staged or any docker command runs.
NONSHIPPING=""
[ -z "${ELF_TOOLCHAIN_ALLOW_MISMATCH:-}" ] || NONSHIPPING="${NONSHIPPING} ELF_TOOLCHAIN_ALLOW_MISMATCH"
[ "${ELF_BUILDDATE}" = "${PINNED_BUILDDATE}" ] || NONSHIPPING="${NONSHIPPING} ELF_BUILDDATE"
[ "${ELF_BUILDHOST}" = "${PINNED_BUILDHOST}" ] || NONSHIPPING="${NONSHIPPING} ELF_BUILDHOST"
[ "${ELF_BUILDUSER}" = "${PINNED_BUILDUSER}" ] || NONSHIPPING="${NONSHIPPING} ELF_BUILDUSER"
[ -z "${ELF_NO_SYMS_FIX:-}" ] || NONSHIPPING="${NONSHIPPING} ELF_NO_SYMS_FIX"
if [ -n "${NONSHIPPING}" ] && [ "${ELF_OUT_DIR}" = "${DEFAULT_OUT_DIR}" ]; then
    echo "build-elf.sh: non-shipping build (${NONSHIPPING# }); set ELF_OUT_DIR to a non-default path" >&2
    exit 2
fi
case "${ELF_STAGE_DIR}" in
    "${HOME}"/*) ;;
    *) echo "build-elf.sh: ELF_STAGE_DIR must be under \$HOME (Colima mounts only \$HOME): ${ELF_STAGE_DIR}" >&2; exit 2 ;;
esac
if [ -e "${ELF_STAGE_DIR}" ] && [ -n "$(ls -A "${ELF_STAGE_DIR}" 2>/dev/null)" ]; then
    echo "build-elf.sh: ELF_STAGE_DIR exists and is not empty: ${ELF_STAGE_DIR}" >&2
    exit 2
fi
if [ ! -x "${ELF_TOOLCHAIN_DIR}/bin/rtems-syms" ]; then
    echo "build-elf.sh: no toolchain at ${ELF_TOOLCHAIN_DIR} (set ELF_TOOLCHAIN_DIR)" >&2
    exit 2
fi
if [ ! -d "${CFS_MIRROR_DIR}/bundle.git" ]; then
    echo "build-elf.sh: no cFS mirrors at ${CFS_MIRROR_DIR} (set CFS_MIRROR_DIR)" >&2
    exit 2
fi

echo "build-elf.sh: hashing the toolchain tree ${ELF_TOOLCHAIN_DIR} ..."
TOOLCHAIN_ACTUAL="$(tree_manifest_hash "${ELF_TOOLCHAIN_DIR}")"
if [ "${TOOLCHAIN_ACTUAL}" != "${TOOLCHAIN_MANIFEST_SHA256}" ]; then
    echo "build-elf.sh: toolchain manifest sha256 ${TOOLCHAIN_ACTUAL} != pinned ${TOOLCHAIN_MANIFEST_SHA256}" >&2
    if [ -z "${ELF_TOOLCHAIN_ALLOW_MISMATCH:-}" ]; then
        echo "build-elf.sh: refusing (set ELF_TOOLCHAIN_ALLOW_MISMATCH=1 for a non-shipping build)" >&2
        exit 2
    fi
    echo "build-elf.sh: ELF_TOOLCHAIN_ALLOW_MISMATCH set -- continuing with an unpinned toolchain" >&2
fi

# ---- 1. Stage: a fresh directory holding exactly the container's /workspace.
STAGE="${ELF_STAGE_DIR}"
STAGE_OK=""
CONTAINER="av-elf-build-$$"
cleanup() {
    docker rm -f "${CONTAINER}" >/dev/null 2>&1 || true
    if [ -n "${STAGE_OK}" ] && [ -z "${ELF_KEEP_STAGE:-}" ]; then
        rm -rf "${STAGE}" 2>/dev/null || echo "build-elf.sh: could not remove ${STAGE} (root-owned files?)" >&2
    elif [ -z "${STAGE_OK}" ]; then
        echo "build-elf.sh: build did not finish; staging kept at ${STAGE}" >&2
    fi
}
trap cleanup EXIT

mkdir -p "${STAGE}"
STAGE="$(cd "${STAGE}" && pwd)"

# 1a. This worktree's build inputs (tracked and untracked-but-not-ignored files).
(
    cd "${REPO_ROOT}"
    git ls-files -z --cached --others --exclude-standard -- \
        services/cfs/apps services/cfs/psp-lockstep services/cfs/build \
        third_party/rtems-container/build-cfs-cross.sh third_party/fetch-cfs.sh \
    | tar -cf - --null -T -
) | tar -xf - -C "${STAGE}"

# 1b. The cFS sources at the pinned commits, from the local mirrors, into the stage only.
GIT_PROTOCOL_GUARD="file"
[ -z "${ELF_ALLOW_NETWORK:-}" ] || GIT_PROTOCOL_GUARD="file:http:https:ssh:git"
GIT_ALLOW_PROTOCOL="${GIT_PROTOCOL_GUARD}" \
CFS_FETCH_DEST="${STAGE}/third_party/cfs" \
CFS_MIRROR_DIR="${CFS_MIRROR_DIR}" \
    sh "${REPO_ROOT}/third_party/fetch-cfs.sh"

# 1c. The rtems-syms fixed-name edit, on the staged RTEMS.cmake only (idempotent, asserted).
RTEMS_CMAKE="${STAGE}/third_party/cfs/psp/cmake/Modules/Platform/RTEMS.cmake"
SYMS_FROM='-o <TARGET>-dl-sym.o <TARGET>-prelink'
SYMS_TO='-S <TARGET>-dl-sym.c -o <TARGET>-dl-sym.o <TARGET>-prelink'
if [ -n "${ELF_NO_SYMS_FIX:-}" ]; then
    echo "build-elf.sh: ELF_NO_SYMS_FIX set -- CONTROL build, rtems-syms keeps its pid-derived C file name"
elif grep -qF -- "${SYMS_TO}" "${RTEMS_CMAKE}"; then
    echo "build-elf.sh: RTEMS.cmake already passes -S"
else
    n="$(grep -cF -- "${SYMS_FROM}" "${RTEMS_CMAKE}" || true)"
    if [ "${n}" != "1" ]; then
        echo "build-elf.sh: expected exactly one rtems-syms link line in ${RTEMS_CMAKE}, found ${n}" >&2
        exit 1
    fi
    FROM="${SYMS_FROM}" TO="${SYMS_TO}" perl -pi -e 's/\Q$ENV{FROM}\E/$ENV{TO}/' "${RTEMS_CMAKE}"
    grep -qF -- "${SYMS_TO}" "${RTEMS_CMAKE}" || { echo "build-elf.sh: the -S edit did not apply" >&2; exit 1; }
fi

echo "build-elf.sh: build inputs"
echo "  repo commit:        $(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown) ($(git -C "${REPO_ROOT}" status --porcelain -- services/cfs/apps services/cfs/psp-lockstep services/cfs/build third_party/rtems-container/build-cfs-cross.sh third_party/fetch-cfs.sh third_party/renode/M24_4/patches 2>/dev/null | wc -l | tr -d ' ') uncommitted file(s) under the inputs)"
echo "  cFS bundle commit:  $(cat "${STAGE}/third_party/cfs/PINNED_COMMIT") (mirrors: ${CFS_MIRROR_DIR})"
echo "  builder image:      ${BASE_IMAGE}"
echo "  apt packages:       ${#APT_PACKAGES[@]} pinned name=version entries; dpkg set sha256 ${ELF_DPKG_SET_SHA256}"
echo "  toolchain:          ${ELF_TOOLCHAIN_DIR} (read-only at /output/toolchain)"
echo "  toolchain manifest: ${TOOLCHAIN_ACTUAL} (pinned ${TOOLCHAIN_MANIFEST_SHA256})"
echo "  cFE metadata:       BUILDDATE=${ELF_BUILDDATE} HOSTNAME=${ELF_BUILDHOST} USER=${ELF_BUILDUSER}"
echo "  rtems-syms -S fix:  $([ -n "${ELF_NO_SYMS_FIX:-}" ] && echo 'OFF (control build)' || echo 'on')"
echo "  staging dir:        ${STAGE}"
echo "  staged workspace:   $(tree_manifest_hash "${STAGE}") (manifest sha256 of the tree mounted at /workspace)"
echo "  output dir:         ${ELF_OUT_DIR}"

# ---- 2. Build, in the digest-pinned container (the entrypoint is build-cfs-cross.sh).
read -r -d '' CONTAINER_SCRIPT <<'SH' || true
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update
# shellcheck disable=SC2086
apt-get install -y --no-install-recommends ${ELF_APT_PACKAGES}
got="$(dpkg-query -W -f='${Package}:${Architecture}=${Version}\n' | LC_ALL=C sort | sha256sum | cut -d' ' -f1)"
echo "container dpkg set sha256: ${got}"
if [ "${got}" != "${ELF_DPKG_SET_SHA256}" ]; then
    echo "build-elf.sh: container package set ${got} != pinned ${ELF_DPKG_SET_SHA256}" >&2
    exit 3
fi
exec /workspace/third_party/rtems-container/build-cfs-cross.sh
SH

mkdir -p "${ELF_OUT_DIR}"
ELF_OUT_DIR="$(cd "${ELF_OUT_DIR}" && pwd)"
rm -rf "${ELF_OUT_DIR}/core-cpu1.exe" "${ELF_OUT_DIR}/build-logs" "${ELF_OUT_DIR}/build-docker.log"

docker run --rm --name "${CONTAINER}" \
    -e "BUILDDATE=${ELF_BUILDDATE}" -e "HOSTNAME=${ELF_BUILDHOST}" -e "USER=${ELF_BUILDUSER}" \
    -e "ELF_APT_PACKAGES=${APT_PACKAGES[*]}" -e "ELF_DPKG_SET_SHA256=${ELF_DPKG_SET_SHA256}" \
    -v "${STAGE}":/workspace \
    -v "${ELF_TOOLCHAIN_DIR}":/output/toolchain:ro \
    --entrypoint /bin/sh \
    "${BASE_IMAGE}" -c "${CONTAINER_SCRIPT}" 2>&1 | tee "${ELF_OUT_DIR}/build-docker.log"

# ---- 3. Collect and report (by content, not by exit code).
BUILT="${STAGE}/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe"
if [ ! -s "${BUILT}" ]; then
    echo "build-elf.sh: no core-cpu1.exe at ${BUILT}" >&2
    exit 1
fi
cp "${BUILT}" "${ELF_OUT_DIR}/core-cpu1.exe"
cp -R "${STAGE}/third_party/rtems-container/output/rtems-cross-build-logs" "${ELF_OUT_DIR}/build-logs"
STAGE_OK=1

echo "build-elf.sh: result"
shasum -a 256 "${ELF_OUT_DIR}/core-cpu1.exe"

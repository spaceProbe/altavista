#!/bin/bash
# container/phase2-toolchains.sh -- PHASE 2, no network (`docker run --network none`). Builds one
# bare-metal cross toolchain from the pinned source tarballs in /cache/tarballs:
#   aarch64-none-elf         FSBL (Cortex-A53, AArch64)     -> /opt/zcu104-tc/aarch64-none-elf
#   microblazeel-xilinx-elf  PMU firmware (MicroBlaze LE)   -> /opt/zcu104-tc/microblazeel-xilinx-elf
# Mounts: /recipe (ro), /cache (ro), /opt/zcu104-tc (rw, the output). TC_TARGET names the target.
# Recipe per target: binutils 2.42, then gcc 13.3.0 (C only) with newlib 4.4.0 merged into its
# source tree ("single tree") and gmp/mpfr/mpc in-tree, LTO on (the Xilinx makefiles use -flto),
# newlib's own syscall stubs off (the Xilinx BSP's libxil supplies them). The prefix and the build
# directory are fixed paths, so the toolchain does not depend on the host; SOURCE_DATE_EPOCH is
# exported and the locale/zone/umask are fixed by lib.sh. binutils is configured with
# --enable-deterministic-archives: without it `ar`/`ranlib` stamp the build time into the static
# libraries (libc.a, libgcc.a, ... differed between two otherwise identical builds in the member
# header timestamps only, found 2026-10-07 by building the toolchain twice).
set -euo pipefail
. /recipe/container/lib.sh
: "${TC_TARGET:?TC_TARGET not set}"
export SOURCE_DATE_EPOCH="${PINNED_SOURCE_DATE_EPOCH}"
install_pinned_debs

PREFIX="/opt/zcu104-tc/${TC_TARGET}"
JOBS="${TC_JOBS:-4}"
rm -rf "${PREFIX:?}" /work-tc
mkdir -p "${PREFIX}" /work-tc/src /work-tc/build-binutils /work-tc/build-gcc /opt/zcu104-tc/logs
cd /work-tc/src

for t in binutils-2.42.tar.xz gcc-13.3.0.tar.xz newlib-4.4.0.20231231.tar.gz gmp-6.3.0.tar.xz mpfr-4.2.1.tar.xz mpc-1.3.1.tar.gz; do
    tar -xf "/cache/tarballs/${t}"
done
ln -s ../gmp-6.3.0 gcc-13.3.0/gmp
ln -s ../mpfr-4.2.1 gcc-13.3.0/mpfr
ln -s ../mpc-1.3.1 gcc-13.3.0/mpc
ln -s ../newlib-4.4.0.20231231/newlib gcc-13.3.0/newlib
ln -s ../newlib-4.4.0.20231231/libgloss gcc-13.3.0/libgloss
# The MicroBlaze target libraries are built without debug information (CFLAGS_FOR_TARGET=-O2, set
# before configure). With the default -g, libgloss's libnosys.a (all twelve multilib copies) differed
# between two otherwise identical builds in 124 bytes: the random temporary file name gcc gives the
# assembler (`/tmp/ccXXXXXX.s`) ends up in the DWARF line-table strings of those objects. (The
# FSBL's AArch64 libraries are unaffected and keep the default flags.) The link of the PMU firmware
# still needs the library (MicroBlaze GCC's default library spec names -lgloss), so it cannot be left out.
case "${TC_TARGET}" in
    microblazeel-xilinx-elf) export CFLAGS_FOR_TARGET="-O2" ;;
esac

export PATH="${PREFIX}/bin:${PATH}"
# makeinfo is not installed and the documentation is not wanted.
export MAKEINFO=true

fail() {
    cp "$2" "/opt/zcu104-tc/logs/${TC_TARGET}-failed-$(basename "$2")" 2>/dev/null || true
    echo "phase2: $1 failed; first errors in $2:" >&2
    grep -n -m8 -i -B2 -A3 "error\\|\*\*\*" "$2" >&2 || true
    tail -20 "$2" >&2
    exit 5
}

echo "phase2: binutils for ${TC_TARGET}"
cd /work-tc/build-binutils
/work-tc/src/binutils-2.42/configure --target="${TC_TARGET}" --prefix="${PREFIX}" \
    --disable-nls --disable-werror --disable-gdb --disable-sim --disable-gprofng --disable-libdecnumber --disable-readline \
    --enable-deterministic-archives \
    >configure.log 2>&1 || fail configure configure.log
make MAKEINFO=true -j"${JOBS}" >make.log 2>&1 || fail make make.log
make MAKEINFO=true install >install.log 2>&1 || fail install install.log
cp configure.log "/opt/zcu104-tc/logs/${TC_TARGET}-binutils-configure.log"
cp make.log "/opt/zcu104-tc/logs/${TC_TARGET}-binutils-make.log"

echo "phase2: gcc + newlib for ${TC_TARGET}"
cd /work-tc/build-gcc
EXTRA=()
case "${TC_TARGET}" in
    aarch64-none-elf) EXTRA+=(--disable-multilib --with-arch=armv8-a) ;;
    microblazeel-xilinx-elf) ;;
    *) echo "unknown TC_TARGET ${TC_TARGET}" >&2; exit 2 ;;
esac
/work-tc/src/gcc-13.3.0/configure --target="${TC_TARGET}" --prefix="${PREFIX}" \
    --enable-languages=c --with-newlib --disable-nls --disable-shared --disable-threads \
    --disable-libssp --disable-libgomp --disable-libquadmath --disable-libatomic --disable-libsanitizer \
    --disable-decimal-float --without-isl --disable-werror --enable-lto \
    --disable-newlib-supplied-syscalls \
    "${EXTRA[@]}" >configure.log 2>&1 || fail configure configure.log
make MAKEINFO=true -j"${JOBS}" all >make.log 2>&1 || fail make make.log
make MAKEINFO=true install >install.log 2>&1 || fail install install.log
cp configure.log "/opt/zcu104-tc/logs/${TC_TARGET}-gcc-configure.log"
cp make.log "/opt/zcu104-tc/logs/${TC_TARGET}-gcc-make.log"

"${PREFIX}/bin/${TC_TARGET}-gcc" --version | head -1
case "${TC_TARGET}" in
    microblazeel-xilinx-elf) echo "phase2: ${TC_TARGET} manifest sha256 $(tree_manifest_hash "${PREFIX}" ${MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE}) (without ${MICROBLAZE_TOOLCHAIN_MANIFEST_EXCLUDE})" ;;
    *) echo "phase2: ${TC_TARGET} manifest sha256 $(tree_manifest_hash "${PREFIX}")" ;;
esac
chown -R "${HOST_UID}:${HOST_GID}" /opt/zcu104-tc
echo "phase2: done"

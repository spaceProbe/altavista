#!/bin/sh
# Build the arm-rtems6 cross toolchain (binutils, gcc+newlib, gdb, rtems-tools) via the RTEMS
# Source Builder pinned under third_party/rtems/rsb (fetch-rsb.sh), following the RTEMS
# project's own documented recipe: `sb-set-builder <arch>/rtems-arm`, prefix outside the RSB
# checkout. docs/open-questions.md question 154: network is used here (source tarball
# downloads, RSB records and verifies a SHA-512 for every one, see rsb/rtems/config/tools/
# *.cfg and the generated .txt/.xml reports under third_party/rtems/toolchain-build-log/) --
# this is the one-time fetch/build exception; nothing after this script needs the network.
#
# Bset resolved (rsb/rtems/config/6/rtems-arm.bset -> 6/rtems-default.bset ->
# 6/rtems-base.bset -> tools/rtems-default-tools.bset), pinned by RSB's own config at tag 6.1:
#   binutils 2.43, gcc 13.3.0 + newlib commit 1b3dcfd (RTEMS/sourceware-mirror-newlib-cygwin),
#   gdb 15.2, rtems-tools commit ca7bcc490ee84e65a173386a4ef5bb55635fc9d6, plus RSB-internal
#   gmp 6.3.0, mpfr 4.2.1, expat 2.5.0, dtc 1.6.1, gsed 4.9, texinfo, isl 0.24, mpc 1.3.1.
# Every one of those is content-hashed (SHA-512) by RSB's own .cfg files before use; RSB writes
# a full build report (with every hash and patch it applied) to
# third_party/rtems/toolchain-build-log/<pkg>-<host-triplet>-1.{txt,xml}.
#
# --keep-going: `devel/dtc-1.6.1-1` (the device tree compiler host tool, part of the default
# tool set for BSPs that consume FDT blobs -- not used by arm/zynqmp_rpu_lock_step, confirmed
# by grepping rtems-src for dtc/.dts/.dtb references under that BSP, none found) failed on a
# first attempt with no diagnostic text beyond "shell cmd failed", then *succeeded* on a bare
# manual re-run of its own do-build script in the same build tree (recorded as an unresolved
# flake in REPORT.md, not silently normalized) -- --keep-going lets binutils/gcc/gdb/rtems-tools
# proceed regardless of dtc's outcome, since this BSP does not need dtc.
#
# Host friction recorded (see third_party/rtems/REPORT.md): this build needs a `python`
# executable on PATH for RSB's `#!/usr/bin/env python` shebangs, which recent macOS does not
# ship; invoked directly via `python3 sb-set-builder` instead of relying on the shebang, so no
# PATH/symlink hack is needed. `autoconf`/`automake`/`texinfo` (`makeinfo`) were not present on
# this host and were installed via Homebrew (network use, recorded in REPORT.md) even though
# RSB also builds its own internal texinfo/gsed for the actual GCC build.
#
# --macros=toolchain-extra-macros.mc: sets `gcc_configure_extra_options: --with-system-zlib`
# (gcc-common-1.cfg's own extensibility hook, `%{?gcc_configure_extra_options:...}` -- no
# vendored file edited for this one). Without it GCC's build hits the *same* bundled-zlib/
# `fdopen` macro collision as binutils/gdb (see REPORT.md's "Patch carried" section) -- GCC
# builds its own zlib copy for LTO bytecode compression, independently of the binutils/gdb
# fix. `--with-system-zlib` is accepted by GCC's top-level configure exactly like binutils
# (both come from the same shared gcc/binutils/gdb top-level `configure.ac`).
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
rsb="$here/rsb"
prefix="$here/toolchain"
topdir="$here/toolchain-build"
logdir="$here/toolchain-build-log"

if [ ! -d "$rsb/rtems/config" ]; then
    echo "build-toolchain.sh: $rsb not found -- run fetch-rsb.sh first" >&2
    exit 1
fi

# texinfo is keg-only on macOS (the system ships an ancient version); RSB also needs a real
# `xz` for source archives it decompresses itself (present via Homebrew on this host).
export PATH="/opt/homebrew/opt/texinfo/bin:/opt/homebrew/bin:$PATH"

mkdir -p "$topdir" "$logdir"

echo "build-toolchain.sh: prefix=$prefix topdir=$topdir log=$logdir/build.log"

cd "$rsb/source-builder"
exec python3 sb-set-builder \
    --keep-going \
    --macros="$here/toolchain-extra-macros.mc" \
    --prefix="$prefix" \
    --topdir="$topdir" \
    --log="$logdir/build.log" \
    6/rtems-arm

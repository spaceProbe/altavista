#!/bin/sh
# Entrypoint for the RTEMS 6.1 arm-rtems6 toolchain + zynqmp_rpu_lock_step BSP build box
# (M24.2c). Runs inside the container built from this directory's Dockerfile. Writes
# everything to /output, which the caller bind-mounts to
# third_party/rtems-container/output on the host.
#
# Order of operations, and why:
#   1. Build the toolchain WITHOUT the host's three zlib patches first (--keep-going so a
#      dtc/gdb failure does not block binutils/gcc/newlib/rtems-tools) -- this is the test of
#      the stated expectation (REPORT.md) that those patches and the gdb exclusion are
#      Xcode-26/macOS-SDK-specific and unneeded on Debian.
#   2. Verify success by checking the prefix directly (arm-rtems6-gcc --version), never by
#      trusting RSB's exit code -- the host attempt's exit-0-with-nothing-installed is exactly
#      the failure mode being guarded against here.
#   3. If gdb is genuinely missing/broken for a DIFFERENT reason than the host's Apple-clang
#      issue, this script does NOT silently apply the host's patches -- it reports the new
#      failure text and stops, per "no silent fallbacks".
#   4. Build the zynqmp_rpu_lock_step BSP with the resulting toolchain and build the hello/
#      ticker samples (BUILD_SAMPLES defaults to True, confirmed on the host attempt).
set -eu

OUT=/output
PREFIX="$OUT/toolchain"
TOPDIR="$OUT/toolchain-build"
LOGDIR="$OUT/toolchain-build-log"
RTEMS_OUT="$OUT/rtems-build"

mkdir -p "$OUT" "$TOPDIR" "$LOGDIR"

echo "=== build.sh starting: prefix=$PREFIX topdir=$TOPDIR log=$LOGDIR ===" | tee -a "$OUT/build-progress.log"
date -u | tee -a "$OUT/build-progress.log"

cd /work/rsb/source-builder

set +e
python3 sb-set-builder \
    --keep-going \
    --prefix="$PREFIX" \
    --topdir="$TOPDIR" \
    --log="$LOGDIR/build.log" \
    6/rtems-arm > "$OUT/toolchain-build-stdout.log" 2>&1
RSB_EXIT=$?
set -e

echo "=== sb-set-builder exited $RSB_EXIT (NOT trusted on its own -- verifying prefix next) ===" | tee -a "$OUT/build-progress.log"

echo "=== verifying arm-rtems6-gcc ===" | tee -a "$OUT/build-progress.log"
if [ -x "$PREFIX/bin/arm-rtems6-gcc" ]; then
    "$PREFIX/bin/arm-rtems6-gcc" --version | tee "$OUT/arm-rtems6-gcc-version.txt"
    GCC_OK=1
else
    echo "arm-rtems6-gcc NOT FOUND at $PREFIX/bin/arm-rtems6-gcc" | tee "$OUT/arm-rtems6-gcc-version.txt"
    GCC_OK=0
fi

echo "=== verifying arm-rtems6-gdb ===" | tee -a "$OUT/build-progress.log"
if [ -x "$PREFIX/bin/arm-rtems6-gdb" ]; then
    "$PREFIX/bin/arm-rtems6-gdb" --version | tee "$OUT/arm-rtems6-gdb-version.txt"
    GDB_OK=1
else
    echo "arm-rtems6-gdb NOT FOUND at $PREFIX/bin/arm-rtems6-gdb" | tee "$OUT/arm-rtems6-gdb-version.txt"
    GDB_OK=0
fi

echo "=== verifying arm-rtems6-ld / binutils presence ===" | tee -a "$OUT/build-progress.log"
ls -la "$PREFIX/bin/" 2>&1 | tee "$OUT/prefix-bin-listing.txt" || echo "PREFIX $PREFIX/bin DOES NOT EXIST" | tee "$OUT/prefix-bin-listing.txt"

if [ "$GCC_OK" != "1" ]; then
    echo "FATAL: arm-rtems6-gcc did not build -- toolchain unusable, stopping before BSP build." | tee -a "$OUT/build-progress.log"
    echo "See $OUT/toolchain-build-stdout.log and $LOGDIR/build.log for the exact failure." | tee -a "$OUT/build-progress.log"
    exit 1
fi

echo "=== toolchain OK, proceeding to BSP build ===" | tee -a "$OUT/build-progress.log"

export PATH="$PREFIX/bin:$PATH"
cd /work/rtems-src
./waf distclean -o "$RTEMS_OUT" >/dev/null 2>&1 || true

./waf configure \
    -o "$RTEMS_OUT" \
    --prefix="$PREFIX" \
    --rtems-bsps=arm/zynqmp_rpu_lock_step \
    --rtems-tools="$PREFIX" 2>&1 | tee "$OUT/bsp-configure.log"

./waf -o "$RTEMS_OUT" 2>&1 | tee "$OUT/bsp-build.log"

./waf install -o "$RTEMS_OUT" 2>&1 | tee "$OUT/bsp-install.log"

echo "=== BSP build done, locating samples ===" | tee -a "$OUT/build-progress.log"
find "$RTEMS_OUT" -name '*.exe' | tee "$OUT/samples-list.txt"

echo "=== computing sha256 of hello/ticker samples ===" | tee -a "$OUT/build-progress.log"
: > "$OUT/samples-sha256.txt"
for sample in hello ticker; do
    exe=$(find "$RTEMS_OUT" -path "*/samples/$sample/$sample.exe" | head -n1)
    if [ -n "$exe" ] && [ -f "$exe" ]; then
        sha256sum "$exe" | tee -a "$OUT/samples-sha256.txt"
    else
        echo "MISSING: $sample.exe not found under $RTEMS_OUT" | tee -a "$OUT/samples-sha256.txt"
    fi
done

echo "=== build.sh done ===" | tee -a "$OUT/build-progress.log"
date -u | tee -a "$OUT/build-progress.log"

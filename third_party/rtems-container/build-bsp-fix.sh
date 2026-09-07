#!/bin/bash
# M24.2b follow-up: build the zynqmp_rpu_lock_step BSP + hello/ticker samples using the
# arm-rtems6 toolchain that build.sh already produced successfully (verified: gcc 13.3.0,
# gdb 15.2, 65 arm-rtems6-* binaries under /output/toolchain -- see REPORT.md). Does NOT
# rebuild the toolchain.
#
# Root cause of the first BSP attempt's failure (bsp-configure.log: "Option file 'config.ini'
# was not readable", ctx.fatal at rtems-src/wscript:1456): RTEMS 6's waf `configure` command
# unconditionally requires a `config.ini` BSP-options file to exist in the cwd (default
# `--rtems-config=config.ini`) -- it is NOT auto-generated. The documented RTEMS 6 workflow
# generates it first with `./waf bspdefaults --rtems-bsps=<bsp> > config.ini`. Neither
# build-bsp.sh copy (host's third_party/rtems/build-bsp.sh, nor this dir's work/build-bsp.sh)
# has this step -- both would hit the same fatal error if run as-is. This script adds the
# missing step; it is a finding about the reused script, not an environment quirk (recorded
# in REPORT.md).
set -euo pipefail

OUT=/output
PREFIX="$OUT/toolchain"
RTEMS_OUT="$OUT/rtems-build"
SRC=/work/rtems-src

if [ ! -x "$PREFIX/bin/arm-rtems6-gcc" ]; then
    echo "build-bsp-fix.sh: $PREFIX/bin/arm-rtems6-gcc not found -- toolchain missing" >&2
    exit 1
fi

export PATH="$PREFIX/bin:$PATH"

echo "=== build-bsp-fix.sh starting $(date -u) ===" | tee -a "$OUT/build-progress.log"

cd "$SRC"
./waf distclean -o "$RTEMS_OUT" >/dev/null 2>&1 || true

echo "=== generating config.ini via bspdefaults (missing step in build-bsp.sh) ===" | tee -a "$OUT/build-progress.log"
./waf bspdefaults --rtems-bsps=arm/zynqmp_rpu_lock_step > "$SRC/config.ini" 2> "$OUT/bsp-defaults-stderr.log"
cp "$SRC/config.ini" "$OUT/config.ini"
echo "config.ini written ($(wc -l < "$SRC/config.ini") lines), copy at $OUT/config.ini" | tee -a "$OUT/build-progress.log"

echo "=== waf configure ===" | tee -a "$OUT/build-progress.log"
./waf configure \
    -o "$RTEMS_OUT" \
    --prefix="$PREFIX" \
    --rtems-bsps=arm/zynqmp_rpu_lock_step \
    --rtems-tools="$PREFIX" 2>&1 | tee "$OUT/bsp-configure.log"

echo "=== waf build ===" | tee -a "$OUT/build-progress.log"
./waf -o "$RTEMS_OUT" 2>&1 | tee "$OUT/bsp-build.log"

echo "=== waf install ===" | tee -a "$OUT/build-progress.log"
./waf install -o "$RTEMS_OUT" 2>&1 | tee "$OUT/bsp-install.log"

echo "=== BSP build done, locating samples ===" | tee -a "$OUT/build-progress.log"
find "$RTEMS_OUT" -name '*.exe' | sort | tee "$OUT/samples-list.txt"

echo "=== computing sha256 of hello/ticker samples ===" | tee -a "$OUT/build-progress.log"
# NOTE (fixed after first run): samples build flat into .../testsuites/samples/<name>.exe,
# NOT .../testsuites/samples/<name>/<name>.exe -- the original glob here (copied from the
# inherited build.sh) was wrong and reported both as MISSING despite both existing; verified
# by listing samples-list.txt above before trusting this loop.
: > "$OUT/samples-sha256.txt"
for sample in hello ticker; do
    exe=$(find "$RTEMS_OUT" -path "*/testsuites/samples/$sample.exe" | head -n1)
    if [ -n "$exe" ] && [ -f "$exe" ]; then
        sha256sum "$exe" | tee -a "$OUT/samples-sha256.txt"
    else
        echo "MISSING: $sample.exe not found under $RTEMS_OUT" | tee -a "$OUT/samples-sha256.txt"
    fi
done

echo "=== build-bsp-fix.sh done $(date -u) ===" | tee -a "$OUT/build-progress.log"

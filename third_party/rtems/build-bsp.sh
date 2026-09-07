#!/bin/sh
# Configure and build the `zynqmp_rpu_lock_step` BSP (docs/open-questions.md question 144)
# from the RTEMS 6.1 kernel source pinned under third_party/rtems/rtems-src, using the
# arm-rtems6 toolchain built by build-toolchain.sh into third_party/rtems/toolchain. Follows
# the standard RTEMS 6 waf recipe (`./waf configure`, `./waf`, `./waf install`), sharing ONE
# prefix between the cross toolchain and the BSP install (the documented RTEMS convention --
# arm-rtems6-gcc looks for its target sysroot relative to its own install prefix, so BSP
# libraries installed under the same prefix are found automatically without extra -B/-specs
# plumbing).
#
# `BUILD_SAMPLES` defaults to True (confirmed via `./waf bspdefaults
# --rtems-bsps=arm/zynqmp_rpu_lock_step` before writing this script), so `hello` and `ticker`
# build without any config.ini override; nothing here is added to relax that or any other
# default.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
rtems_src="$here/rtems-src"
prefix="$here/toolchain"
outdir="$here/rtems-build"

if [ ! -x "$prefix/bin/arm-rtems6-gcc" ]; then
    echo "build-bsp.sh: $prefix/bin/arm-rtems6-gcc not found -- run build-toolchain.sh first" >&2
    exit 1
fi
if [ ! -f "$rtems_src/wscript" ]; then
    echo "build-bsp.sh: $rtems_src not found -- run fetch-rtems-src.sh first" >&2
    exit 1
fi

export PATH="$prefix/bin:$PATH"

cd "$rtems_src"
./waf distclean -o "$outdir" >/dev/null 2>&1 || true

./waf configure \
    -o "$outdir" \
    --prefix="$prefix" \
    --rtems-bsps=arm/zynqmp_rpu_lock_step \
    --rtems-tools="$prefix"

./waf -o "$outdir"

./waf install -o "$outdir"

echo "build-bsp.sh: done -- BSP libraries installed under $prefix, build tree in $outdir"
echo "build-bsp.sh: samples expected at $outdir/arm/zynqmp_rpu_lock_step/testsuites/samples/{hello,ticker}/*.exe"

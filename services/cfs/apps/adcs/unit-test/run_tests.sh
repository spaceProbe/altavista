#!/usr/bin/env bash
# Build and run services/cfs/apps/adcs's host-side unit tests with a plain C compiler -- no
# cFS UT-assert framework is used here (see adcs_test_framework.h's own top comment for why:
# third_party/cfs had not landed when this app was written). Only adcs_control.c/adcs_packets.c
# are compiled -- adcs_app.c depends on real cFE headers that do not exist yet and is
# deliberately excluded from this build.
#
# M23.4: adcs_packets.c no longer implements its own CCSDS bit-packing -- it wraps
# services/cfs/apps/shared/ccsds/src/ccsds_codec.c (the same file services/cfs/apps/io_lockstep
# links), so that file (and its own include dir) is now part of this build too.
#
# -ffp-contract=off: disable fused-multiply-add contraction so the floating-point operation
# order in this C code matches the Rust reference bit-for-bit as closely as a different
# compiler/ISA can guarantee (see test_adcs_control.c's own comment on ADCS_CROSS_LANG_TOL).
set -euo pipefail
cd "$(dirname "$0")"

CC="${CC:-cc}"
SHARED_CCSDS_DIR="../../shared/ccsds"
CFLAGS="-std=c99 -Wall -Wextra -Werror -O2 -ffp-contract=off -I../fsw/inc -I$SHARED_CCSDS_DIR/inc -lm"

BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

echo "== building test_adcs_control =="
$CC $CFLAGS test_adcs_control.c ../fsw/src/adcs_control.c -o "$BUILD_DIR/test_adcs_control"

echo "== building test_adcs_packets =="
$CC $CFLAGS test_adcs_packets.c ../fsw/src/adcs_packets.c "$SHARED_CCSDS_DIR/src/ccsds_codec.c" -o "$BUILD_DIR/test_adcs_packets"

status=0
echo "== running test_adcs_control =="
"$BUILD_DIR/test_adcs_control" || status=1
echo
echo "== running test_adcs_packets =="
"$BUILD_DIR/test_adcs_packets" || status=1

exit $status

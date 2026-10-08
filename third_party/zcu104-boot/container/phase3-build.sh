#!/bin/bash
# container/phase3-build.sh -- PHASE 3, no network (`docker run --network none`). Builds bootgen,
# the ZynqMP FSBL and the PMU firmware from the pinned, cached sources with the pinned toolchains,
# writes the .bif, runs bootgen, and checks the result structurally.
#
# Mounts: /recipe (ro), /cache (ro), /opt/zcu104-tc (ro, the two toolchains), /stage (ro: the RPU
# ELF and, in "supplied" mode, /stage/psu_init/{psu_init.c,psu_init.h}), /out (rw).
# Environment (set by build-boot-bin.sh): HOST_UID, HOST_GID, PSU_INIT_MODE (standin|supplied),
# SOURCE_DATE_EPOCH, RPU_ELF_NAME, BOOT_BIN_NAME, BOOT_JOBS.
set -euo pipefail
. /recipe/container/lib.sh
: "${SOURCE_DATE_EPOCH:?}" "${PSU_INIT_MODE:?}" "${RPU_ELF_NAME:?}" "${BOOT_BIN_NAME:?}"
export SOURCE_DATE_EPOCH
JOBS="${BOOT_JOBS:-4}"
install_pinned_debs

TC_A64=/opt/zcu104-tc/aarch64-none-elf
TC_MB=/opt/zcu104-tc/microblazeel-xilinx-elf
# The Xilinx makefiles call the MicroBlaze tools mb-gcc, mb-gcc-ar, ...: same tools, Xilinx names.
mkdir -p /work/bin
for t in "${TC_MB}"/bin/microblazeel-xilinx-elf-*; do
    n="$(basename "$t")"
    ln -sf "$t" "/work/bin/mb-${n#microblazeel-xilinx-elf-}"
done
export PATH="/work/bin:${TC_A64}/bin:${TC_MB}/bin:${PATH}"

rm -rf /out/* 2>/dev/null || true
mkdir -p /out/logs

echo "phase3: bootgen ${BOOTGEN_COMMIT}"
cp -a /cache/src/bootgen /work/bootgen
( cd /work/bootgen && make -j"${JOBS}" >/out/logs/bootgen-make.log 2>&1 ) || { tail -40 /out/logs/bootgen-make.log >&2; exit 5; }
BOOTGEN=/work/bootgen/bootgen

echo "phase3: embeddedsw ${EMBEDDEDSW_COMMIT}"
cp -a /cache/src/embeddedsw /work/embeddedsw
# (No patch is applied: the sources build as they are, with the two make arguments below.)

FSBL_SRC=/work/embeddedsw/lib/sw_apps/zynqmp_fsbl
PMUFW_SRC=/work/embeddedsw/lib/sw_apps/zynqmp_pmufw
case "${PSU_INIT_MODE}" in
    standin)
        # embeddedsw ships board files for the ZCU102 only (see the README): they are built as-is.
        echo "phase3: PSU_INIT_MODE=standin -- the ZCU102 psu_init from embeddedsw; NOT for a ZCU104"
        ;;
    supplied)
        echo "phase3: PSU_INIT_MODE=supplied -- psu_init.c/h from /stage/psu_init replace the board files"
        cp /stage/psu_init/psu_init.c /stage/psu_init/psu_init.h "${FSBL_SRC}/misc/zcu102/"
        ;;
    *) echo "unknown PSU_INIT_MODE ${PSU_INIT_MODE}" >&2; exit 2 ;;
esac

# DUMP=true: the FSBL makefile writes a disassembly listing (`objdump -xSD`) that nothing uses; the
# binutils 2.42 AArch64 disassembler aborts on an instruction in this ELF (README, "Found").
FSBL_MAKE_EXTRA=()
[ -z "${FSBL_DEBUG_DEFINE:-}" ] || FSBL_MAKE_EXTRA=("CFLAGS+=-D${FSBL_DEBUG_DEFINE}")
echo "phase3: FSBL (A53, AArch64) with $(aarch64-none-elf-gcc --version | head -1)"
( cd "${FSBL_SRC}/src" && make BOARD=zcu102 PROC=a53 A53_STATE=64 DUMP=true ${FSBL_MAKE_EXTRA[@]+"${FSBL_MAKE_EXTRA[@]}"} >/out/logs/fsbl-make.log 2>&1 ) \
    || { tail -60 /out/logs/fsbl-make.log >&2; exit 5; }
cp "${FSBL_SRC}/src/fsbl.elf" /out/fsbl.elf
echo "phase3: PMU firmware (MicroBlaze) with $(mb-gcc --version | head -1)"
( cd "${PMUFW_SRC}/src" && make >/out/logs/pmufw-make.log 2>&1 ) \
    || { tail -60 /out/logs/pmufw-make.log >&2; exit 5; }
cp "${PMUFW_SRC}/src/executable.elf" /out/pmufw.elf

# ---- the .bif and the image
cat >/out/boot.bif <<EOF
the_ROM_image:
{
	[bootloader, destination_cpu = a53-0] /out/fsbl.elf
	[pmufw_image] /out/pmufw.elf
	[destination_cpu = r5-lockstep] /stage/${RPU_ELF_NAME}
}
EOF
( cd /out && "${BOOTGEN}" -arch zynqmp -image /out/boot.bif -o "/out/${BOOT_BIN_NAME}" -w ) >/out/logs/bootgen-run.log 2>&1 \
    || { tail -30 /out/logs/bootgen-run.log >&2; exit 5; }
[ -s "/out/${BOOT_BIN_NAME}" ] || { echo "phase3: no ${BOOT_BIN_NAME} produced" >&2; exit 5; }
"${BOOTGEN}" -arch zynqmp -read "/out/${BOOT_BIN_NAME}" >/out/bootgen-read.txt 2>&1 || true

# ---- structural check, from the artifacts only
python3 -I /recipe/container/structural-check.py \
    --bootgen-read /out/bootgen-read.txt --fsbl /out/fsbl.elf --pmufw /out/pmufw.elf \
    --rpu "/stage/${RPU_ELF_NAME}" --readelf "${TC_A64}/bin/aarch64-none-elf-readelf" \
    --mb-readelf "${TC_MB}/bin/microblazeel-xilinx-elf-readelf" >/out/structural-check.txt 2>&1 \
    && SC=PASS || SC=FAIL
echo "phase3: structural check ${SC}"
cat /out/structural-check.txt

{
    echo "psu_init_mode ${PSU_INIT_MODE}"
    echo "source_date_epoch ${SOURCE_DATE_EPOCH}"
    echo "embeddedsw ${EMBEDDEDSW_COMMIT}"
    echo "bootgen ${BOOTGEN_COMMIT}"
    ( cd /out && sha256sum fsbl.elf pmufw.elf "${BOOT_BIN_NAME}" boot.bif )
} >/out/SHA256SUMS
# ---- the outputs against their pins (pinned-inputs.sh), when the build is the shipping build
PINS_OK=yes
check_pin() {  # name file expected
    local got
    got="$(sha256sum "$2" | cut -d' ' -f1)"
    if [ "${got}" = "$3" ]; then echo "phase3: $1 sha256 ${got} == pin"; else echo "phase3: $1 sha256 ${got} != pin $3" >&2; PINS_OK=no; fi
}
if [ "${SOURCE_DATE_EPOCH}" = "${PINNED_SOURCE_DATE_EPOCH}" ] && [ "${PIN_CHECK:-1}" = 1 ]; then
    check_pin pmufw.elf /out/pmufw.elf "${PMUFW_ELF_SHA256}"
    if [ "${PSU_INIT_MODE}" = standin ]; then
        check_pin fsbl.elf /out/fsbl.elf "${STANDIN_FSBL_ELF_SHA256}"
        check_pin "${BOOT_BIN_NAME}" "/out/${BOOT_BIN_NAME}" "${STANDIN_BOOT_BIN_SHA256}"
    fi
else
    echo "phase3: non-shipping build (SOURCE_DATE_EPOCH or the FSBL define differs); outputs are not checked against their pins"
fi
chown -R "${HOST_UID}:${HOST_GID}" /out
[ "${SC}" = PASS ] || exit 1
[ "${PINS_OK}" = yes ] || { echo "phase3: an output differs from its pin" >&2; exit 6; }
echo "phase3: done"

# third_party/zcu104-boot -- the ZCU104 SD-card boot image

A digest-pinned, byte-reproducible recipe for `BOOT.BIN`: the FSBL and PMU firmware from a pinned
Xilinx `embeddedsw`, the RTEMS 6.1 `zynqmp_rpu_lock_step` cFS ELF on the R5 pair in lockstep, all
assembled by the open-source `bootgen` built from source. The run host needs no Xilinx tool, licence
or account (docs/open-questions.md question 242 (d)).

**The ZCU104 is not on hand. Nothing here has been booted.** The image is checked structurally only
(below). Everything under "first boot" is an instruction for when the board arrives, not a result.

## Status: one input is missing, and the default build says so

The FSBL needs the board's `psu_init.c`/`psu_init.h` (PS clocks, MIO, DDR4 timing, ...), which Vivado
generates from a ZCU104 design. No open source carries a complete ZCU104 copy: `embeddedsw` v2024.2
ships board files for the ZCU102 (`lib/sw_apps/zynqmp_fsbl/misc/zcu102`, `zcu102-es2`) and the Kria
SOM only, and U-Boot-xlnx v2024.2's `board/xilinx/zynqmp/zynqmp-zcu104-revA/psu_init_gpl.c` is a
minimized 870-line file whose `psu_ddr_init_data` has no DDR-controller programming and which lacks
functions the FSBL calls (`psu_protection`, `psu_ps_pl_isolation_removal_data`, `psu_apply_master_tz`, ...;
its file history, 878 lines at its first commit, shows it was never complete). So:

* **Default ("standin") build** compiles the FSBL with the ZCU102 files and writes
  `BOOT.standin-zcu102-psuinit.bin`. It proves the recipe, the structure and the reproducibility. It
  must NOT be put on a ZCU104: the ZCU102 has a different DDR4 arrangement (its `xparameters.h` declares 4 GB,
  the ZCU104's device tree 2 GB at 0x0), so DDR bring-up would fail or misbehave. The file name is deliberately not `BOOT.BIN`.
* **Supplied build** closes the gap: export the hardware from a Vivado design for the ZCU104 (its board
  preset; the exported XSA contains `psu_init.c` and `psu_init.h`; this recipe has not generated one), put
  the two files in a directory, and run

      BOOT_PSU_INIT_DIR=<dir> \
      BOOT_PSU_INIT_MANIFEST_SHA256=$(third_party/zcu104-boot/build-boot-bin.sh --print-toolchain-hash <dir>) \
      third_party/zcu104-boot/build-boot-bin.sh

  which writes `BOOT.BIN`. The manifest hash pins the exported files for the record (add it, and the
  XSA's own hash, to the notes of the run that uses them). This path was exercised with the ZCU102
  files fed through it: it produced byte-identical FSBL and image to the standin build, and a wrong
  hash is refused before any docker command.

## What the image contains

`bootgen -arch zynqmp -read` of the standin build (full text: `tests/fixtures/zcu104_boot/bootgen-read.txt`):

| partition | core | state / EL | load | exec | bytes |
|---|---|---|---|---|---|
| (PMU firmware, in the boot header) | PMU MicroBlaze | | 0xFFDC0000 | reset vector | 129760 (0x1FAE0) |
| `fsbl.elf.0` | a53-0 | aarch-64, el-3 | 0xFFFC0000 | 0xFFFC0000 | 136648 (+ the PMU firmware: 266408) |
| `core-cpu1.exe.0` | **r5-lockstep** | aarch-32 | 0x00000000 (TCM) | 0x40 | 960 |
| `core-cpu1.exe.1` | **r5-lockstep** | aarch-32 | 0x40000000 (DDR) | (none) | 719840 |

The `.bif` (`boot.bif` in the output) is:

    the_ROM_image:
    {
        [bootloader, destination_cpu = a53-0] /out/fsbl.elf
        [pmufw_image] /out/pmufw.elf
        [destination_cpu = r5-lockstep] /stage/core-cpu1.exe
    }

bootgen emits one partition per ELF `PT_LOAD` with file content, so the RPU ELF's TCM vector page and
its DDR image are two partitions, both `r5-lockstep` (attribute word 0x71E, destination CPU field
0x700 = `XIH_PH_ATTRB_DEST_CPU_R5_L` in `xfsbl_image_header.h`). The FSBL (a Cortex-A53 AArch64 image
at the start of OCM) is the only thing the CSU boot ROM runs; it loads the rest and releases the R5
pair. The shipping FSBL is built with no debug define, so it prints little (a banner and errors);
`BOOT_FSBL_DEFINE=FSBL_DEBUG` (a non-shipping build) adds the platform, processor and "Exit from FSBL"
lines and fits OCM; `FSBL_DEBUG_INFO` does not fit, see "Found".

## Building

The script is bash, run by path, on the host with docker (Colima: only `$HOME` is mounted). **It takes
the host-wide docker-test lock itself** (question 207): after its refusals it re-executes under
`scripts/dev/docker-lock-run.py`, which holds `altavista.docker_test_lock.lock_docker_tests()` (and its
holder sidecar, `~/.altavista/locks/docker-tests.lock.holder`, naming that helper's pid) for the whole
run, a toolchain build included, and releases it when the run ends by any means. Do not wrap it in another
lock holder; run it plainly:

    third_party/zcu104-boot/build-boot-bin.sh fetch    # once, network
    third_party/zcu104-boot/build-boot-bin.sh          # no network

(`--print-toolchain-hash` and the argument refusals run without the lock.)

`BOOT_RPU_ELF` names the RPU ELF (default `third_party/rtems-container/output/elf/core-cpu1.exe`, in
this tree or the main tree, built by `third_party/rtems-container/build-elf.sh`); its SHA-256 must be
`a5a5fe7b0d87714478c748cd08ca1888d68c6385657626d2cf42e36bc37a2eb5`. The header of the script
documents every parameter. Outputs (default `third_party/zcu104-boot/output/`, ignored): the image,
`fsbl.elf`, `pmufw.elf`, `boot.bif`, `bootgen-read.txt`, `structural-check.txt`, `SHA256SUMS`,
`logs/`. The fetched cache (`cache/`, ignored, about 2 GB with both toolchains) is reused.

Three phases, three `docker run --rm` of the same image, none leaving a tag or image behind:

1. **fetch** (network; the one-time window of question 154): the 92 apt packages as `.deb` files, the
   two source trees, six tarballs. Everything is checked against `pinned-inputs.sh` and
   `apt-debs.sha256`. In every mode but `fetch` this phase runs with `--network none` and only
   verifies the cache.
2. **toolchains** (no network): `aarch64-none-elf` (FSBL) and `microblazeel-xilinx-elf` (PMU
   firmware) GCC 13.3.0 + binutils 2.42 + newlib 4.4.0, built from the tarballs; skipped when the
   tree's manifest hash equals its pin. About 10 to 25 minutes each on 4 cores.
3. **build** (no network): bootgen, FSBL (`make BOARD=zcu102 PROC=a53 A53_STATE=64`), PMU firmware
   (`make`), the `.bif`, bootgen, the structural check. About 3 minutes.

## Pinned inputs (`pinned-inputs.sh` is the authority; this is a reading of it)

| input | pin |
|---|---|
| builder image | `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171` (also `build-elf.sh`'s) |
| apt | 92 `name=version` packages (build-essential, git, curl, python3, libssl-dev, ...), `.deb` hashes in `apt-debs.sha256`, installed set sha256 `b0dc5c6cbbb91237d29e62f3a78459e6314bf38f6daf2b8a3a8360d1eb5c3d9e` |
| `embeddedsw` | `https://github.com/Xilinx/embeddedsw` commit `6e4d0b89d2958994ab9b3531eb4c6e648a63f201` (`xilinx_v2024.2`), tree manifest `ce09fc9af75d7609f03658d58853074b2ab1e560edc45938b4354c3ee19bb720` |
| `bootgen` | `https://github.com/Xilinx/bootgen` commit `6f448fece5d999985128fd454ae047e065a5e45d` (`xilinx_v2024.2`), tree manifest `743a2822746eb3a738d151d6500b3794e9323a339cf5f62f1cbd9925a500c60a` |
| binutils / gcc / newlib / gmp / mpfr / mpc | 2.42 / 13.3.0 / 4.4.0.20231231 / 6.3.0 / 4.2.1 / 1.3.1, sha256 in `pinned-inputs.sh` |
| toolchain trees | manifest hashes in `pinned-inputs.sh`: aarch64 `cc6a9e38...`, MicroBlaze `9b8b3632...` (the latter without its target libraries, see "Found") |
| RPU ELF | sha256 `a5a5fe7b0d87714478c748cd08ca1888d68c6385657626d2cf42e36bc37a2eb5` |
| `SOURCE_DATE_EPOCH` | `1791331200` (2026-10-07 00:00:00 UTC) |

Why release 2024.2: it is the last release before AMD's 2025 restructuring of `lib/sw_apps`, still
has the stand-alone `zynqmp_fsbl`/`zynqmp_pmufw` makefile flows (`misc/copy_bsp.sh`) this recipe uses,
and its bootgen is the matching one. Why a source-built toolchain: no prebuilt binary is trusted, no
vendor download needs an account, and the MicroBlaze compiler has no Debian package (`apt-cache search
microblaze` finds only QEMU); upstream GCC 13.3 builds `microblazeel-xilinx-elf` and accepts the
`-mcpu=v9.2 -mxl-barrel-shift -mxl-pattern-compare -mxl-soft-mul -mlittle-endian` flags Xilinx's
PMU firmware makefile uses. A "manifest hash" is the sha256 of the sorted list of `F|X <sha256> <path>`
and `L <target> <path>` lines of a tree (build-elf.sh's rule, plus the executable bit).

Tree and tarball hashes marked in the pins file as self-measured were measured by the person who
pinned them at the first fetch; the GNU tarball values were also recognised against the widely
published ones. The apt pins depend on the live Debian mirror (as `build-elf.sh`'s do): a version
Debian has dropped makes the fetch fail loudly; `snapshot.debian.org` would make them permanent.

## Reproducibility

`SOURCE_DATE_EPOCH` is honoured by GCC >= 7 for `__DATE__`/`__TIME__`, which the FSBL
("Release 2024.2   Oct  7 2026  -  00:00:00") and the PMU firmware banner print. Locale, time zone
and umask are fixed in the container; sources, build trees and toolchains sit at fixed container paths
(`/work`, `/opt/zcu104-tc`), so the host staging path, output directory and cache location cannot
reach an artifact; none of the three artifacts contains a host path. Phase 3 compares `pmufw.elf`
(always) and, for the standin build, `fsbl.elf` and the image with the output pins in
`pinned-inputs.sh` and fails (exit 6) on a mismatch. Measured 2026-10-07/08 on this host (4 CPUs):

| build | staging dir | output dir | work fs | toolchains | BOOT image | fsbl.elf | pmufw.elf |
|---|---|---|---|---|---|---|---|
| A | default | default | container | cache 1 | `0711bc86...` | `61f8b945...` | `81701d2b...` |
| B | `~/hp5-stageB/deeper/than/default` | `~/hp5-outB` | tmpfs | cache 1 | same | same | same |
| C | `~/hp5-stageC/deeper/s` | `~/hp5-outC` | container | cache 2 (rebuilt from scratch) | same | same | same |

`cmp` is silent on all of image, FSBL, PMU firmware, `.bif`, `bootgen-read.txt` between A, B and C.
Perturbation (`BOOT_SOURCE_DATE_EPOCH` one day later): the FSBL differs in 1 byte (the day digit of the
banner's `Oct  7`), the PMU firmware in 1 byte (the same digit in its banner), and the image in 2 bytes:
file offsets 81042 (inside the PMU firmware block, which starts at 0x2800) and 237739 (inside the FSBL
block, which starts at 0x2800 + 0x1FAE0). The recipe's invariants are pinned by
`tests/test_zcu104_boot_recipe.py`.

## Structural check

`container/structural-check.py` runs inside the build and fails it unless: the boot header's FSBL
execution address equals the FSBL ELF's entry point and is 0xFFFC0000 (start of OCM, where the CSU
boot ROM loads the FSBL); the PMU firmware's lowest segment is 0xFFDC0000 (PMU RAM, the MicroBlaze
reset vector) with its entry inside PMU RAM and bootgen's PMU length equal to the ELF's span; the FSBL
partition is a53-0, aarch-64, el-3 at the ELF's addresses and its length is the PMU firmware plus FSBL
blocks; and every RPU `PT_LOAD` with content is a `r5-lockstep`, aarch-32 partition with
load address = segment `p_paddr`, length = `p_filesz`, and the first one's exec address = the ELF
entry (0x40). It reads the `bootgen -read` text, so it checks the image bytes, not the `.bif`.

## The RPU ELF and the board (UART, clock, memory map)

Finding: **the ELF does not need to change for the ZCU104**, and was not rebuilt. Evidence:

* **Which PS UARTs reach USB.** The ZCU104's USB-UART is a **Future Technology Devices FT4232HL**
  (not the ZCU102's Silicon Labs CP2108): per UG1267 (read through a manual mirror, not the AMD site), channel **A** is the JTAG chain, channel **B** is
  PS **UART0** (MIO18/19), channel **C** is PS **UART1** (MIO20/21), channel **D** is a PL UART (bank
  28). U-Boot-xlnx's `zynqmp-zcu104-revC.dts` agrees: `serial0 = &uart0` (console, `115200n8`),
  `serial1 = &uart1`, pinctrl groups `uart0_4_grp` (MIO18/19) and `uart1_5_grp` (MIO20/21). Both
  boards' `psu_init` set MIO18-21 to the UART function (`MIO_PIN_18..21` = 0xC0 in the ZCU102 file
  and in U-Boot's ZCU104 file).
* **What the RTEMS BSP uses.** `bsps/arm/xilinx-zynqmp-rpu/console/console-config.c` installs
  `/dev/ttyS0` = Zynq UART 0 at 0xFF000000 and `/dev/ttyS1` = UART 1 at 0xFF010000;
  `ZYNQ_UART_KERNEL_IO_BASE_ADDR` is UART 0 (the generated `bspopts.h`), so the cFE/RTEMS console is
  UART0 = FT4232HL channel B, and `io_lockstep` (`services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`,
  `/dev/ttyS1`) is UART1 = channel C. Whether the XPPU lets the RPU master reach both UARTs on the ZCU104's own
  `psu_init` is not checked (see "Not pinned").
* **UART reference clock.** The BSP assumes `ZYNQ_CLOCK_UART` = 100000000 Hz (default of
  `spec/build/bsps/arm/xilinx-zynqmp-rpu/optclkuart.yml`; the divisors are computed from it by
  `zynq_uart_calculate_baud`, default 115200). `psu_init` programs `UART0_REF_CTRL` and
  `UART1_REF_CTRL` (0xFF5E0074/78) to 0x01010F00 on both the ZCU102 file and U-Boot's ZCU104 file:
  source IOPLL, divisor 15; the IOPLL is 33.333 MHz x 45 = 1.5 GHz (`IOPLL_CTRL` FBDIV 45), so
  1.5 GHz / 15 = 100 MHz, 99,990,005 Hz in the generated `xparameters.h` (0.01 % low). The baud error
  is far inside the BSP's margin. (Renode ignores divisors; on hardware the clock matters, and it matches.)
* **Memory map.** The ELF has two `PT_LOAD`s: 0x00000000 (960 bytes of vectors and start code, memsz
  0x20000: ATCM) and 0x40000000 (719840 bytes, memsz 0x20000000: DDR). The BSP options (`bspopts.h`):
  lockstep mode, ATCM 0x0/0x20000, BTCM 0x20000/0x20000, DDR 0x40000000/0x20000000, PL 0x80000000,
  PS devices 0xC0000000. The ZCU104's DDR is 2 GB at 0x0 to 0x7FFFFFFF (device tree `memory@0`), which
  contains the RPU's 0x40000000..0x5FFFFFFF. The FSBL turns the partition at load address 0 into the
  R5-lockstep TCM address (0xFFE00000 + address, `xfsbl_partition_load.c` `R5_L` branch), powers and
  ECC-initialises the TCM, and loads the DDR partition directly; it needs DDR running, which is what
  `psu_init` provides (the gap above).

## SD card and first boot (instructions for when the board arrives; nothing here has been run)

1. **Card.** One FAT32 partition (the boot ROM reads the first FAT partition), `BOOT.BIN` in its
   root, named exactly that. Build with `BOOT_PSU_INIT_DIR` set (above), never the standin file.
2. **Switches.** SW6 selects the boot mode; SD card boot on the ZCU104 is SD1 (level-shifted),
   mode pins [3:0] = 1110, which is SW6 position 1 ON and positions 2, 3, 4 OFF. Read from secondary
   sources (the PYNQ ZCU104 setup guide, Xilinx's ZCU104 BIST guide, the antmicro rowhammer-tester
   ZCU104 page); the primary document, UG1267 "ZCU104 Evaluation Board User Guide", could not be
   fetched here as a file, so **confirm against the silkscreen and UG1267 before powering**. JTAG is
   all four ON, QSPI32 is ON ON OFF ON.
3. **USB.** The board's on-board USB (one micro-USB cable) gives four serial ports (FT4232HL channels A to D, 115200 8N1).
   On the host the FT4232 enumerates as four ports in channel order; channel A is not a UART. Open
   **B** for the console (UART0) and **C** for the lockstep protocol link (UART1,
   `BoardBinding.port_devices` `/dev/...@115200`). Which host device name maps to which channel is
   to be read off the host when it is plugged in (the interface index / last character of the FTDI
   serial number).
4. **Power on** with the console open on channel B. Expect, in order (from the FSBL and cFE sources):
   `Zynq MP First Stage Boot Loader` and `Release 2024.2   Oct  7 2026  -  00:00:00` from the FSBL
   (nothing further from the FSBL at this build's print level except on errors, e.g. the "PMU-FW is not
   running" warning if the PMU firmware did not start); then the RTEMS/cFE boot banner of the cFS
   image (`CFE_PSP`/`CFE_ES` start-up lines, ending in the cFS apps starting, including
   `io_lockstep`) on the same channel. If the FSBL prints an `XFSBL_ERROR_...` line the boot stopped
   in the FSBL. A silent port after the FSBL banner points at the RPU not running.
5. **Lockstep.** The FSBL puts the R5 pair in lockstep by clearing `SLSPLIT` (bit 3) and setting
   `TCM_COMB` (bit 6) in `RPU_GLBL_CNTL` (0xFF9A0000) before it loads the partitions
   (`xfsbl_handoff.c` and `xfsbl_partition_load.c`, the `R5_L` cases). To confirm it on the board read
   that register with any debugger (JTAG over channel A): bit 3 = 0 and bit 6 = 1 mean lockstep.
   R5-1 stays halted in lockstep; its separate TCM banks join R5-0's (the BSP's 128 KB ATCM + 128 KB
   BTCM contiguous view).
6. **If it does not boot**: the first suspects are the `psu_init` gap (DDR), the SW6 position, and the
   card format. For more FSBL output build with `BOOT_FSBL_DEFINE=FSBL_DEBUG` (and a `BOOT_OUT_DIR`):
   it was built and passed the structural check here; its strings include `Platform: Silicon`,
   `Running on A53-0` and `Exit from FSBL`. Both a debug image and the shipping one have the same
   partition table.

## Found (root causes, not fixed upstream)

* **The toolchain build was not reproducible**: two builds of `aarch64-none-elf` from the same
  tarballs differed in 7 static libraries (`libc.a`, `libg.a`, `libm.a`, `libnosys.a`, `librdimon.a`,
  `libgcc.a`, `libgcov.a`), same sizes, 5 bytes each: the `ar`/`ranlib` member-header timestamp of the
  symbol table. Root cause: binutils 2.42 `ar` stamps the current time unless configured with
  `--enable-deterministic-archives`. Fixed in `container/phase2-toolchains.sh`.
* **The MicroBlaze toolchain's target libraries are not bit-reproducible, and the cause was not
  found.** After the `ar` fix, two from-scratch builds of the whole MicroBlaze toolchain (same
  tarballs, same recipe) differed in exactly the files under `<target>/lib/`: all twelve multilib
  copies of `libnosys.a` (6 bytes per member: libgloss objects carry the random `/tmp/ccXXXXXX.s`
  name gcc gives the assembler, in their DWARF line-table strings; building the target libraries
  with `CFLAGS_FOR_TARGET=-O2` did not remove it) and `bs/le/libc.a` with its copy `libg.a`
  (one member, `hash_func.o`, 1252 against 1420 bytes: one build kept the assembler's `$L12..$L19`
  local labels in the symbol table, the other dropped them; compiling that file 12 times with the
  installed compiler gives one result, so the variation is in the build context, not in cc1). Everything
  else, 1021 of 1035 files including `cc1`, `as`, `ld` and `libgcc.a`, matched. Because of this the MicroBlaze
  manifest leaves out `microblazeel-xilinx-elf/lib/`, and the PMU firmware (which links `libc.a` from
  there) is pinned by its own output hash; builds against both toolchain copies gave the same
  `pmufw.elf` and the same image. The aarch64 toolchain, rebuilt from scratch twice after the `ar` fix,
  matched (equal manifest hashes).
* **`aarch64-none-elf-objdump -xSD` aborts** on the FSBL ELF (`aarch64-dis.c:251
  get_sreg_qualifier_from_value` assertion) in binutils 2.42; the FSBL makefile runs it only to write
  an unused `dump` listing, so the recipe passes `DUMP=true`. The offending instruction was not located;
  it is an assertion inside the disassembler, which no artifact of this recipe depends on.
* **`FSBL_DEBUG_INFO` does not fit.** With GCC 13.3 `-Os -flto` the FSBL plus its debug strings
  overflows OCM region `psu_ocm_ram_0_S_AXI_BASEADDR` (`.dup_data` ends 0xFFFEABCF, region ends
  0xFFFE9DFF). Xilinx builds with GCC 12.2 (Vitis). `FSBL_DEBUG` fits (built and structurally checked).
* GNU make 4.3 (Debian bookworm) does not sort `$(wildcard ...)`, which the BSP archive step uses
  (`ar -r libxil.a $(wildcard .../lib/*.o)`); the object order in `libxil.a` is therefore the
  filesystem's enumeration order. Builds on the container's filesystem and on a tmpfs gave identical
  images, so no patch was needed, but a filesystem that enumerates differently is not excluded.

## Not pinned, and why

* The `psu_init` for the ZCU104 (above): no open source has it.
* The Debian package versions are pinned by name=version and hashed, but the Debian archive can drop a
  version.
* The PMU firmware configuration object (`pm_cfg_obj.c`, board-independent default in `misc/`) and the
  XMPU/XPPU settings in `psu_init` decide which master may touch the UARTs and DDR; they were not
  audited against the lockstep application and are a first-boot suspect.

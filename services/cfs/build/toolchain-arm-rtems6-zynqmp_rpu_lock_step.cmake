# M24.3 (docs/open-questions.md questions 144/148/154; docs/sil-plan.md M24) -- cFE cross-toolchain
# file for the RTEMS 6.1 `zynqmp_rpu_lock_step` BSP built by
# third_party/rtems-container/build-bsp-fix.sh (M24.2b/c).
#
# Modeled directly on the pinned cFS bundle's OWN shipped
# third_party/cfs/sample_defs/toolchain-i686-rtems6.cmake (upstream's documented RTEMS 6 pattern)
# -- CFE_SYSTEM_PSPNAME/OSAL_SYSTEM_BSPTYPE/OSAL_SYSTEM_OSTYPE/RTEMS_DYNAMIC_LOAD are copied
# verbatim from that file; only the BSP name, target processor, and ABI flags change here for
# arm/zynqmp_rpu_lock_step. CFE_SYSTEM_PSPNAME stays "pc-rtems" deliberately: read directly
# (third_party/cfs/psp/fsw/pc-rtems/src/*.c), that PSP has no board-specific code at all (malloc()
# for reserved memory, generic rtems_* calls, no i686/PC addresses) -- "pc-rtems" is a legacy
# name for upstream's only RTEMS 6 PSP implementation, not a board dependency. No PSP patch is
# carried (question 148).
#
# ABI flags below are copied from the BSP's own pkg-config file, produced by the M24.2c build:
#   third_party/rtems-container/output/toolchain/lib/pkgconfig/arm-rtems6-zynqmp_rpu_lock_step.pc
#   ABI_FLAGS=-march=armv7-r -mthumb -mfpu=vfpv3-d16 -mfloat-abi=hard
#   RTEMS_ARCH=arm  RTEMS_BSP=zynqmp_rpu_lock_step  RTEMS_BSP_FAMILY=xilinx-zynqmp-rpu
# (cFE's toolchain files set these directly rather than consuming the .pc file themselves.)

set(CMAKE_SYSTEM_NAME       RTEMS)
set(CMAKE_SYSTEM_PROCESSOR  arm)
set(CMAKE_SYSTEM_VERSION    6)

# The RTEMS BSP that will be used for this build
set(RTEMS_BSP               "zynqmp_rpu_lock_step")

# these settings are specific to cFE/OSAL and determine which abstraction layers are built
SET(CFE_SYSTEM_PSPNAME      pc-rtems)
SET(OSAL_SYSTEM_BSPTYPE     generic-rtems)
SET(OSAL_SYSTEM_OSTYPE      rtems)

# Version-specific RTEMS ifdefs needed by OSAL/PSP (matches toolchain-i686-rtems6.cmake)
ADD_DEFINITIONS(-DOS_RTEMS_6)

# `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c` reads this same definition to select
# the UART-backed `connect_to_shim()` implementation instead of the posix build's AF_UNIX one --
# see that file's own top comment and the M24.3 report's "Transport" section for why (no network
# stack in this RTEMS 6 build, so no AF_UNIX/socket()).
ADD_DEFINITIONS(-DAV_CFS_LOCKSTEP_TRANSPORT_UART)

# M24.3: OSAL's own network/BSD-sockets code must be disabled for this target -- this build's
# toolchain has no `-lnetworking` to link against (question 148's "no network stack", confirmed
# directly: nothing named "networking" anywhere under the toolchain prefix). Passing
# `-DOSAL_CONFIG_INCLUDE_NETWORK=FALSE` on the mission-level `cmake` invocation (this task's
# first attempt) does NOT reach OSAL's actual configure step: found by reading
# `cfe/cmake/mission_build.cmake`'s own `process_arch()`, the arch-specific build tree (where
# OSAL is actually configured -- "Configuring for system arch: ...") is a SEPARATE nested
# `execute_process(COMMAND ${CMAKE_COMMAND} ...)` subprocess that forwards only a fixed, small
# variable whitelist (TARGETSYSTEM/MISSION_BINARY_DIR/CMAKE_BUILD_TYPE/CMAKE_INSTALL_PREFIX/
# CMAKE_PREFIX_PATH/CMAKE_EXPORT_COMPILE_COMMANDS/CFE_EDS_ENABLED) plus
# `-DCMAKE_TOOLCHAIN_FILE=<this file>` -- confirmed directly by comparing the two build tree's own
# CMakeCache.txt files: the outer (mission-level) cache correctly showed
# `OSAL_CONFIG_INCLUDE_NETWORK:BOOL=FALSE` while the inner (arch-level, where OSAL actually lives)
# cache still showed `TRUE`, and `os-impl-network.c`/`os-impl-bsd-sockets.c` kept being compiled.
# THIS toolchain file, in contrast, IS forwarded to that inner subprocess (via
# CMAKE_TOOLCHAIN_FILE) and toolchain files run early enough to set CACHE variables with FORCE --
# the one guaranteed place to actually reach OSAL's configure step for this target.
set(OSAL_CONFIG_INCLUDE_NETWORK FALSE CACHE BOOL "no network stack in this RTEMS 6 build (question 148)" FORCE)

# M24.3: found running this build -- `arm-rtems6-ld: undefined reference to 'bsp_cmdline'`
# (osal/src/bsp/generic-rtems/src/bsp_cmdline.c calls a BSP-provided `bsp_cmdline()` this BSP
# variant does not implement; a compile-time "implicit declaration" warning became this link
# error). generic-rtems/CMakeLists.txt's own RTEMS_NO_CMDLINE option exists for exactly this --
# this lockstep-driven target has no use for RTEMS boot command-line parsing anyway. Same
# variable-forwarding reason as OSAL_CONFIG_INCLUDE_NETWORK above: must be set here (the
# toolchain file), not in PREP_OPTS, to actually reach the arch-level configure subprocess.
set(RTEMS_NO_CMDLINE TRUE CACHE BOOL "this BSP has no bsp_cmdline() implementation" FORCE)

# M24.3: found running this build -- `arm-rtems6-ld: undefined reference to 'Stack_checker_Reporter'`
# from librtemscpu.a(check.c)'s rtems_stack_checker_switch_extension. Read directly
# (cpukit/include/rtems/confdefs/extensions.h): `Stack_checker_Reporter` is only DEFINED when
# CONFIGURE_STACK_CHECKER_ENABLED is set (which this build does not set, deliberately -- this
# target has no use for RTEMS's stack-checker extension), and `RTEMS_STACK_CHECKER_EXTENSION`
# (the only thing that would ever actually CALL rtems_stack_checker_switch_extension) is
# likewise only installed under that same guard -- so the function is genuinely dead code for
# this build, but `check.c.o` still gets pulled whole into the link (GNU ld links at
# object-file, not per-function, granularity) because something else in the same translation
# unit IS referenced. This BSP's own pkg-config file
# (output/toolchain/lib/pkgconfig/arm-rtems6-zynqmp_rpu_lock_step.pc) already documents
# `-Wl,--gc-sections` as part of its normal Ldflags for exactly this reason -- enabling it (with
# the matching `-ffunction-sections -fdata-sections` compile flags it depends on) lets the
# linker drop the genuinely-unreachable function instead of this build needing to satisfy a
# reference to a feature it never enables.
#
# NOT done via RTEMS_BSP_C_FLAGS: found running this build (verified by reading the actual
# generated build.make/link.txt rules under build-rtems_zynqmp/.../CMakeFiles/*/) that
# `string(APPEND RTEMS_BSP_C_FLAGS ...)` placed here has NO effect on the real compile/link
# command lines -- psp/cmake/Modules/Platform/RTEMS.cmake's own `CMAKE_C_COMPILE_OBJECT`/
# `CMAKE_C_LINK_EXECUTABLE` templates reference `${RTEMS_BSP_C_FLAGS}` with `${...}` syntax,
# which cmake expands IMMEDIATELY when THAT `set()` command runs (as literal text baked into the
# rule template), not lazily at each compile/link invocation -- so only the value
# RTEMS_BSP_C_FLAGS held AT THAT MOMENT (empirically, before this toolchain file's own later
# lines execute) ends up in the generated rules; appending to the variable afterward changes the
# variable but not the already-baked-in template string. `CMAKE_C_FLAGS`/`CMAKE_EXE_LINKER_FLAGS`
# are different: the SAME templates reference them via the angle-bracket placeholders `<FLAGS>`/
# `<CMAKE_C_LINK_FLAGS>`/`<LINK_FLAGS>`, which CMake's Makefile generator fills in per-target at
# generate time from the actual (final) value of these standard variables -- robust regardless of
# where in the toolchain file they are set. This BSP's own pkg-config file
# (output/toolchain/lib/pkgconfig/arm-rtems6-zynqmp_rpu_lock_step.pc) already documents
# `-Wl,--gc-sections` in its normal Ldflags for exactly this reason.
set(CMAKE_C_FLAGS "-ffunction-sections -fdata-sections" CACHE STRING "" FORCE)
set(CMAKE_CXX_FLAGS "-ffunction-sections -fdata-sections" CACHE STRING "" FORCE)
set(CMAKE_EXE_LINKER_FLAGS "-Wl,--gc-sections" CACHE STRING "" FORCE)

# This build produces one statically-linked RTEMS executable (no dlopen()-based dynamic loading
# of cFS apps) -- matches the pinned bundle's own toolchain-i686-rtems6.cmake, whose comment notes
# this is "tied to the OSAL-BSP and PSP implementation" and not generally switchable. cpu1 in
# targets-rtems.cmake below therefore uses STATIC_APPLIST, not APPLIST.
set(RTEMS_DYNAMIC_LOAD      FALSE)

set(RTEMS_BSP_C_FLAGS       "-march=armv7-r -mthumb -mfpu=vfpv3-d16 -mfloat-abi=hard")
set(RTEMS_BSP_CXX_FLAGS     ${RTEMS_BSP_C_FLAGS})
set(RTEMS_BSP_SPECS_FLAGS   "")

# Exception handling is very iffy on RTEMS -- disable eh_frame creation (matches toolchain-i686-rtems6.cmake).
set(CMAKE_C_COMPILE_OPTIONS_PIC -fno-exceptions -fno-asynchronous-unwind-tables)

# Link libraries needed for a RTEMS 5+ executable (matches toolchain-i686-rtems6.cmake)
set(LINK_LIBRARIES              "-lrtemsdefaultconfig -lrtemsbsp -lrtemscpu")

# No RTEMS_RELOCADDR: that option is specific to i686 PC boot via GRUB/multiboot
# (toolchain-i686-rtems6.cmake's own comment: "if you'll be using GRUB..."). This BSP is loaded
# directly as an ELF (Renode's `sysbus LoadELF`, confirmed working for hello.exe/ticker.exe in
# M24.2c) -- the BSP's own linker script places sections correctly with no override needed.

#+---------------------------------------------------------------------------+
#| Common RTEMS toolchain statements (matches toolchain-i686-rtems6.cmake)   |
#+---------------------------------------------------------------------------+
# Both TOOLS and BSP live under the same container-build prefix (M24.2c's output/toolchain,
# bind-mounted at /output inside the build container -- see build-cfs-cross.sh).
SET(RTEMS_TOOLS_PREFIX "/output/toolchain" CACHE PATH
    "RTEMS tools install directory")
SET(RTEMS_BSP_PREFIX "${RTEMS_TOOLS_PREFIX}" CACHE PATH
    "RTEMS BSP install directory")

SET(SDKHOSTBINDIR               "${RTEMS_TOOLS_PREFIX}/bin")
set(TARGETPREFIX                "${CMAKE_SYSTEM_PROCESSOR}-rtems${CMAKE_SYSTEM_VERSION}-")

SET(CMAKE_C_COMPILER            "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}gcc")
SET(CMAKE_CXX_COMPILER          "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}g++")
SET(CMAKE_LINKER                "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}ld")
SET(CMAKE_ASM_COMPILER          "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}as")
SET(CMAKE_STRIP                 "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}strip")
SET(CMAKE_NM                    "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}nm")
SET(CMAKE_AR                    "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}ar")
SET(CMAKE_OBJDUMP               "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}objdump")
SET(CMAKE_OBJCOPY               "${RTEMS_TOOLS_PREFIX}/bin/${TARGETPREFIX}objcopy")

SET(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM   NEVER)
SET(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY   ONLY)
SET(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE   ONLY)

SET(CMAKE_PREFIX_PATH                   /)

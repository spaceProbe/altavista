# M24.3's RTEMS counterpart to targets.cmake (the posix build's mission-config override).
# Copied into third_party/cfs/rtems_zynqmp_defs/targets.cmake by build-cfs-cross.sh (a separate
# MISSIONCONFIG/`_defs` directory, NOT sample_defs/ -- see M24_3_REPORT.md's "Scope note", this
# leaves the posix build's own sample_defs/targets.cmake completely untouched).
#
# Same mission name/spacecraft ID and app list as the posix targets.cmake this mirrors, one real
# difference: cpu1_STATIC_APPLIST, not cpu1_APPLIST. The RTEMS toolchain file for this BSP sets
# RTEMS_DYNAMIC_LOAD FALSE (matching the pinned bundle's own toolchain-i686-rtems6.cmake), so
# apps are linked directly into one core-cpu1.exe rather than built as separately-dlopen()ed
# modules -- see that toolchain file's own comment and M24_3_REPORT.md's "Static, not dynamic
# apps" section for why.

SET(MISSION_NAME "AltaVistaCfsLockstep")
SET(SPACECRAFT_ID 0x41)

SET(MISSION_CPUNAMES cpu1)
SET(cpu1_PROCESSORID 1)
SET(cpu1_STATIC_APPLIST io_lockstep sch_lockstep adcs)

# M24.3: found running this build -- the three apps' entry points (IO_LOCKSTEP_AppMain,
# SCH_LS_AppMain, ADCS_AppMain) were entirely ABSENT from the final core-cpu1.exe (confirmed with
# arm-rtems6-nm: zero matches for any of the three, or for any other app-specific symbol like
# ccsds_encode_packet/psp_lockstep_release_tick), even though the build log showed "Built target
# io_lockstep/sch_lockstep/adcs" and the .a files were on the final link command line. Root
# cause, read directly in cfe/docs/README_static_app_linkage.md: cFE's own static-app-linkage
# design does NOT reference an app's entry point from any C code at link time -- normally
# `cfe_static_symbol_list.c` (generated from TGT<x>_STATIC_SYMLIST) is what supplies that
# reference, giving OSAL's OS_SymbolLookup() a real address table entry AND giving the linker a
# real "someone takes this symbol's address" reason to keep it. This task's targets-rtems.cmake
# never set STATIC_SYMLIST, so that generated table was empty, and with attempt 7's
# -ffunction-sections/--gc-sections fix (needed to resolve Stack_checker_Reporter, see
# M24_3_REPORT.md), the now-genuinely-unreferenced app entry points were correctly garbage
# collected by the linker -- not a linker bug, a real, previously-latent gap in this task's own
# mission config that gc-sections simply made visible. Format from that same doc: comma-separated
# "EntryPointSymbol,ModuleName" pairs, ModuleName matching the 4th field of each
# services/cfs/build/generate_startup.cmake startup-script line (already: IO_LOCKSTEP,
# SCH_LOCKSTEP, ADCS).
SET(cpu1_STATIC_SYMLIST IO_LOCKSTEP_AppMain,IO_LOCKSTEP SCH_LS_AppMain,SCH_LOCKSTEP ADCS_AppMain,ADCS)

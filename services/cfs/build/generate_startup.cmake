# M23.2's replacement for the fetched bundle's sample_defs/generate_startup.cmake. Copied over
# it by services/cfs/Dockerfile before `make prep`.
#
# Why this file is patched, and why that is not a cFE/OSAL/PSP patch (docs/open-questions.md
# question 148's "record what had to be patched under cfe/osal/psp" does not apply here): the
# fetched original unconditionally writes `sch_lab`/`ci_lab`/`to_lab`/`sample_app` startup
# entries regardless of `targets.cmake`'s own `cpu1_APPLIST`, and this task did not fetch those
# apps' submodules at all (third_party/fetch-cfs.sh's own module doc comment: only cfe/osal/psp
# plus three build-tool submodules are cloned). `sample_defs/` is the bundle's own named
# customization point for exactly this kind of per-mission override, the same spirit as this
# task's own services/cfs/build/targets.cmake replacing sample_defs/targets.cmake.
#
# M23.4: a third hardcoded line added for `adcs` (`ADCS_AppMain`, 12 characters -- well under
# the 19-character Entry Point truncation this file's own reason 2 below found, so no renaming
# was needed the way `sch_lockstep` required). Still hardcoded, not derived from `ARGN`, for the
# same reason as the original two -- `adcs` has no `cfe_assert`/`cfe_testcase`-shaped surprises
# of its own, but deriving generically from `cpu1_APPLIST` would still pick those two up.
#
# This version hardcodes exactly the apps this task builds, rather than deriving entries
# from `ARGN` (`${cpu1_APPLIST}`) generically -- two reasons found only by actually running the
# built image (`docker run` against this exact commit's build), not by static reading alone:
#
# 1. `${cpu1_APPLIST}` at generate-startup time is not only `targets.cmake`'s own
#    `cpu1_APPLIST` -- `ENABLE_UNIT_TESTS=TRUE` (target-configs.mk's own native_std setting)
#    pulls in `cfe_assert`/`cfe_testcase` as additional "Dynamic Apps" the same list traverses,
#    and a generic `CFE_APP, ..., ${APP_UPPER}_AppMain, ...` line for `cfe_assert` is simply
#    wrong (it is a `CFE_LIB` with entry point `CFE_Assert_LibInit`, confirmed by the original
#    fetched file's own hardcoded line) and produces an `undefined symbol` at load time observed
#    in a real `docker run`. Neither is needed to run this task's own two apps, so both are
#    omitted rather than guessed at.
# 2. cFE's startup-script parser truncates the Entry Point field at a fixed buffer (19
#    characters plus the null terminator, matching `OS_MAX_API_NAME`'s default) --
#    `SCH_LOCKSTEP_AppMain` (20 characters) was silently truncated to `SCH_LOCKSTEP_AppMai` and
#    failed to load as `undefined symbol` in the same real run. `services/cfs/apps/sch_lockstep`'s
#    own exported entry point is therefore named the shorter `SCH_LS_AppMain` (14 characters);
#    `services/cfs/apps/io_lockstep`'s own `IO_LOCKSTEP_AppMain` (19 characters) is exactly at
#    the limit and was confirmed to load correctly in the same run.

function (generate_cfs_startup_script CFS_INSTALL_DIR)
    set (STARTUP_FILE "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/${CFS_INSTALL_DIR}/cfe_es_startup.scr")

    file (WRITE ${STARTUP_FILE}
        "CFE_APP, io_lockstep,  IO_LOCKSTEP_AppMain, IO_LOCKSTEP, 70,  131072, 0x0, 0;\n"
        "CFE_APP, sch_lockstep, SCH_LS_AppMain,       SCH_LOCKSTEP, 70,  131072, 0x0, 0;\n"
        "CFE_APP, adcs,         ADCS_AppMain,         ADCS,        71,  131072, 0x0, 0;\n"
    )

endfunction(generate_cfs_startup_script)

# M23.2's replacement for the fetched bundle's sample_defs/targets.cmake
# (docs/sil-plan.md M23, docs/open-questions.md questions 143/147). Copied into
# third_party/cfs/sample_defs/targets.cmake by services/cfs/Dockerfile before `make prep`.
#
# Trimmed to exactly what this task fetches and builds: no sample_app/cf/hs/md/mm/cs/fm/lc/sc/
# ds/hk/sbn (third_party/fetch-cfs.sh does not clone those app submodules -- this task's own
# scope is cfe/osal/psp plus the two apps it writes) and CI_LAB/TO_LAB/SCH_LAB are replaced by
# io_lockstep/sch_lockstep per M23's own milestone text ("CI_LAB and TO_LAB replaced by a
# lockstep-aware I/O app ... and drives the scheduler from the kernel's ticks").
#
# M23.4: `adcs` (docs/open-questions.md question 146's reference cFS ADCS app,
# services/cfs/apps/adcs) added to the app list -- M23.3 wrote it but this batch is the first to
# actually build it against real cFE headers (see that app's own CMakeLists.txt/adcs_app.h for
# the reconciliation story).
#
# One CPU only (the fetched bundle's own file defines a second, "cpu2", as a heterogeneous
# big-endian demonstration this task has no use for).

SET(MISSION_NAME "AltaVistaCfsLockstep")
SET(SPACECRAFT_ID 0x41)

SET(MISSION_CPUNAMES cpu1)
SET(cpu1_PROCESSORID 1)
SET(cpu1_APPLIST io_lockstep sch_lockstep adcs)

# docs/open-questions.md question 185 (round 5): third_party/cfs/target-configs.mk's own
# `PREP_OPTS_native_std += -DENABLE_UNIT_TESTS=TRUE` (a fetched file this task does not edit)
# builds and installs 86 cFE/OSAL unit-test and coverage-harness binaries under /cfs/cpu1 that
# services/cfs/container-entrypoint.sh never runs, each carrying a non-deterministic GNU-linker
# build-id (question 185's own account, confirmed by R4.3's file-by-file diff,
# services/cfs/R4_3_REPORT.md section 4). third_party/cfs/cfe/cmake/mission_build.cmake's own
# `initialize_globals()` does `set(ENABLE_UNIT_TESTS $ENV{ENABLE_UNIT_TESTS} CACHE BOOL ...)`
# with no FORCE, which cannot override a value already placed in the cache by the `-D` flag on
# the initial cmake command line -- so unsetting/not-setting the ENABLE_UNIT_TESTS environment
# variable has no effect here. This file is include()d by third_party/cfs/cfe/CMakeLists.txt
# (`include(${MISSION_DEFS}/targets.cmake)`, line 113) immediately after `initialize_globals()`
# and before `read_targetconfig()`/`prepare()` and before every `if (ENABLE_UNIT_TESTS)
# add_subdirectory(...)` gate in osal/CMakeLists.txt, psp/CMakeLists.txt and each cfe/modules/*/
# CMakeLists.txt runs -- for BOTH the mission-level build and every per-architecture sub-build
# (cfe/CMakeLists.txt's own lines 93-134 are unconditionally shared by both; the arch sub-build
# imports ENABLE_UNIT_TESTS from the mission build's own mission_vars.cache as a plain variable,
# which a CACHE ... FORCE set here still wins over -- confirmed empirically with a standalone
# CMake reproduction of exactly this normal-variable/cache-variable shadowing pattern before
# relying on it; see services/cfs/R5_3_REPORT.md section 3). A CACHE set with FORCE here
# therefore always wins, in every cmake invocation that processes this mission, regardless of
# how ENABLE_UNIT_TESTS arrived at TRUE upstream.
set(ENABLE_UNIT_TESTS FALSE CACHE BOOL "Enable build of unit tests" FORCE)

# docs/open-questions.md question 185's amendment (2026-09-08, after the manager's review):
# "-Wl,--build-id=none in cFE's toolchain configuration for the image as belt and braces" (on top
# of the unit-tests-off fix above, which is what actually removes the 86 binaries that carried a
# non-deterministic build-id). GNU ld writes a `.note.gnu.build-id` ELF section into every
# executable/shared object it links, containing a build-specific hash that is NOT a function of
# the input bytes alone (services/cfs/R4_3_REPORT.md section 4 isolated exactly this: 34 bytes
# differing inside that one section, nothing else, across two --no-cache builds of otherwise
# byte-identical inputs). `-Wl,--build-id=none` tells the linker to omit that section entirely.
#
# DELIBERATELY placed HERE (this file, already COPYed by services/cfs/Dockerfile) rather than in
# a new services/cfs/build/global_build_options.cmake file wired through cFE's own OPTIONAL
# "global-scope build customization" hook (third_party/cfs/cfe/CMakeLists.txt line 122,
# `include("${MISSION_DEFS}/global_build_options.cmake" OPTIONAL)`) -- that was the first attempt
# and it is the more "textbook" extension point, but it broke the build in a way root-caused
# empirically (services/cfs/R5_3_REPORT.md section 4 has the full account, including 9 real
# `docker build` runs isolating the cause): merely ADDING that new file's own `COPY` instruction
# to the Dockerfile -- regardless of the copied file's content, which was independently proven
# irrelevant by neutralizing it and rebuilding -- makes `es/fsw/src/cfe_es_api.c.o` fail with
# `fatal error: global_core_api_base_msgid_values.h: No such file or directory` (a header
# third_party/cfs/sample_defs/cpu1/cfe_core_api_base_msgid_values.h itself unconditionally
# `#include`s). Deterministic per Dockerfile state (confirmed by an exact retry), and NOT caused
# by the digest-pinned base image, by ENABLE_UNIT_TESTS, or by the copied file's content --
# isolated one variable at a time, each confirmed with a real build: reverting only the extra
# COPY instruction (this file's own targets.cmake COPY line, and every other COPY line,
# unaffected) made the failure disappear; reverting only the FROM-line digest pin (keeping the
# extra COPY instruction) did not. The most likely mechanism (not fully root-caused further, and
# not this task's to fix under third_party/cfs/): a latent, filesystem-enumeration-order-
# dependent defect somewhere in cFE's own fetched build system (a `file(GLOB ...)`-shaped search,
# plausibly for `global_core_api_base_msgid_values.h` itself, though its own generator was not
# located after an extensive read of cfe/cmake/*.cmake and every MISSION_CORE_MODULES module's
# own arch_build.cmake/mission_build.cmake -- see R5_3_REPORT.md), sensitive to incidental
# Docker-layer/container-filesystem state that shifts merely from inserting one additional COPY
# layer into the builder stage, independent of that layer's own content. Putting this content in
# an ALREADY-COPYed file sidesteps the trigger entirely without touching third_party/cfs/ and
# without adding any new COPY instruction to services/cfs/Dockerfile.
foreach(_av_linker_flags_var
    CMAKE_EXE_LINKER_FLAGS
    CMAKE_SHARED_LINKER_FLAGS
    CMAKE_MODULE_LINKER_FLAGS
)
    set(${_av_linker_flags_var} "${${_av_linker_flags_var}} -Wl,--build-id=none"
        CACHE STRING "Flags used by the linker (question 185: deterministic build-id)" FORCE)
endforeach()
unset(_av_linker_flags_var)

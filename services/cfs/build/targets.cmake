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

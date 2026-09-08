# M23.2's replacement for the fetched bundle's sample_defs/cpu1/install_custom.cmake. Copied
# over it by services/cfs/Dockerfile before `make prep`.
#
# The original references build targets (`ci_lab`, `to_lab`, `sample_app`, `hs`, `cf`, `md`,
# `mm`, `cs`, `fm`, `lc`, `sc`, `ds`, `hk`) for apps this task never fetches (see
# third_party/fetch-cfs.sh's own module doc comment: only cfe/osal/psp are cloned; CI_LAB/TO_LAB/
# SCH_LAB are replaced outright per docs/sil-plan.md's M23 milestone text), so the original file
# would fail to configure (`$<TARGET_PROPERTY:ci_lab,...>` against a target that does not
# exist). This version keeps only the native container-start install and the startup-script
# generation call (against `services/cfs/build/generate_startup.cmake`'s own replacement, which
# this task also installs over the fetched original) -- everything else in the original was
# wiring for apps this build does not include.

if (${SIMULATION} MATCHES "^native")
    install(PROGRAMS ${CMAKE_CURRENT_LIST_DIR}/container-start DESTINATION cpu1)
endif()

install(SCRIPT ${MISSION_DEFS}/generate_startup.cmake)
install(CODE "generate_cfs_startup_script(\"${TGTNAME}/${INSTALL_SUBDIR}\" ${${TGTNAME}_APPLIST})")

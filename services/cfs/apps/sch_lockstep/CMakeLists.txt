# M23.2 (docs/sil-plan.md M23): the lockstep scheduler app, replacing SCH_LAB.

# M23.4 FIX -- see services/cfs/apps/io_lockstep/CMakeLists.txt's own copy of this exact comment
# for the full account: `psp_lockstep` must be a SHARED library (not STATIC), installed once,
# so io_lockstep.so and sch_lockstep.so -- two independently dlopen()ed cFE app modules -- link
# against the SAME loaded instance of its mutable tick-queue state, rather than each getting its
# own private copy (which silently, and badly, diverged: io_lockstep's own accepted
# release_tick() calls were invisible to this app's own psp_lockstep_tick_count() reads).
if(NOT TARGET psp_lockstep)
  # M24.3: see services/cfs/apps/io_lockstep/CMakeLists.txt's own copy of this comment for the
  # full account -- RTEMS's static-app build (RTEMS_DYNAMIC_LOAD FALSE) has only one copy of
  # this process, so STATIC already gives psp_lockstep's globals to it exactly once; SHARED
  # remains correct for the posix build's independently-dlopen()ed modules.
  if(RTEMS)
    set(PSP_LOCKSTEP_LIB_TYPE STATIC)
  else()
    set(PSP_LOCKSTEP_LIB_TYPE SHARED)
  endif()
  add_library(psp_lockstep ${PSP_LOCKSTEP_LIB_TYPE} ${CMAKE_CURRENT_LIST_DIR}/../../psp-lockstep/src/psp_lockstep.c)
  target_include_directories(psp_lockstep PUBLIC ${CMAKE_CURRENT_LIST_DIR}/../../psp-lockstep/inc)
  # M24.3: see services/cfs/apps/io_lockstep/CMakeLists.txt's own copy of this comment --
  # RTEMS's toolchain has no separate libpthread.a to link (POSIX threads are part of
  # librtemscpu itself); `-lpthread` is only meaningful on the posix build.
  if(NOT RTEMS)
    target_link_libraries(psp_lockstep PUBLIC pthread)
  endif()
  set_target_properties(psp_lockstep PROPERTIES PREFIX "lib" OUTPUT_NAME "psp_lockstep")
  foreach(PSP_LOCKSTEP_INSTALL_TGT ${APP_DYNAMIC_TARGET_LIST})
    install(TARGETS psp_lockstep DESTINATION ${PSP_LOCKSTEP_INSTALL_TGT}/${INSTALL_SUBDIR})
  endforeach()
endif()

add_cfe_app(sch_lockstep fsw/src/sch_lockstep_app.c)
target_include_directories(sch_lockstep PUBLIC fsw/inc)
# M23.4: the shared LOCKSTEP_WAKEUP_MID this app publishes and services/cfs/apps/adcs
# subscribes to (services/cfs/apps/shared/mission/lockstep_wakeup_mid.h's own doc comment).
target_include_directories(sch_lockstep PRIVATE ${CMAKE_CURRENT_LIST_DIR}/../shared/mission)
target_link_libraries(sch_lockstep psp_lockstep)

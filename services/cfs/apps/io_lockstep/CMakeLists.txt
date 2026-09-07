# M23.2 (docs/sil-plan.md M23): the lockstep I/O app, replacing CI_LAB/TO_LAB.
# Follows the same add_cfe_app pattern every cFS app CMakeLists.txt uses
# (cfe/cmake/arch_build.cmake's own add_cfe_app function).

# M23.4 FIX -- found only by actually running the real, compiled apps against a real kernel
# Step, not by static reading alone: `psp_lockstep` holds MUTABLE state (the pending-tick queue,
# `psp_lockstep.c`'s own file-scope statics) that `services/cfs/apps/io_lockstep` (producer,
# `psp_lockstep_release_tick`) and `services/cfs/apps/sch_lockstep` (consumer,
# `psp_lockstep_external_sync`/`psp_lockstep_tick_count`) MUST share -- but each app is loaded
# by cFE ES as its own independent dynamically-loaded MODULE (`io_lockstep.so`/`sch_lockstep.so`,
# `add_cfe_app`'s own `MODULE` library type). Linking a STATIC library into two separately
# dlopen()ed modules gives EACH module its own private COPY of that static library's object
# code -- including its file-scope globals -- not one shared instance: `io_lockstep`'s own
# `psp_lockstep_release_tick` calls were updating a queue `sch_lockstep`'s own copy of
# `psp_lockstep_external_sync`/`psp_lockstep_tick_count` could never see. Confirmed directly: a
# real `docker run` showed `IO_LOCKSTEP: first release_tick(...) accepted` immediately followed
# by ADCS receiving its sensor packets, while `SCH_LOCKSTEP`'s own heartbeat diagnostic kept
# reporting `psp_lockstep_tick_count()==0` thousands of poll iterations later -- two processes'
# worth of state that never should have been separate, silently diverged, in the SAME OS
# process. The fix: build `psp_lockstep` as a real `SHARED` library instead of `STATIC`, and
# install it alongside the app modules that dlopen it (see the `install(TARGETS ...)` call
# below) -- both `io_lockstep.so` and `sch_lockstep.so` then dynamically link against the SAME
# loaded `libpsp_lockstep.so`, and its globals are genuinely shared process-wide, the way any
# dynamically-linked library's globals are.
if(NOT TARGET psp_lockstep)
  # M24.3: RTEMS_DYNAMIC_LOAD FALSE (services/cfs/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake)
  # means this target's apps are all linked STATICALLY into one core-cpu1.exe, not built as
  # separately-dlopen()ed modules -- there is only ever one copy of this process, so the M23.4
  # "SHARED so two dlopen()ed modules share one copy of psp_lockstep's mutable state" problem
  # this comment block describes below does not exist for RTEMS: a plain STATIC library already
  # gives exactly one copy of its globals to the single executable that links it once. RTEMS's
  # own CMake platform module also treats a CMake "SHARED" library specially (a partially-linked
  # `ld -r` relocatable object, for its OWN dynamic-load mechanism, distinct from ELF .so
  # semantics) -- forcing STATIC here avoids that path entirely for a target that does not use
  # it. `if(RTEMS)` is set by psp/cmake/Modules/Platform/RTEMS.cmake, loaded automatically by
  # CMake once CMAKE_SYSTEM_NAME is RTEMS (see the toolchain file above).
  if(RTEMS)
    set(PSP_LOCKSTEP_LIB_TYPE STATIC)
  else()
    set(PSP_LOCKSTEP_LIB_TYPE SHARED)
  endif()
  add_library(psp_lockstep ${PSP_LOCKSTEP_LIB_TYPE} ${CMAKE_CURRENT_LIST_DIR}/../../psp-lockstep/src/psp_lockstep.c)
  target_include_directories(psp_lockstep PUBLIC ${CMAKE_CURRENT_LIST_DIR}/../../psp-lockstep/inc)
  # M24.3: found running this build -- `arm-rtems6-ld: cannot find -lpthread`. RTEMS's toolchain
  # ships <pthread.h> (POSIX threads compile fine, confirmed: psp_lockstep.c's pthread_mutex_t/
  # pthread_cond_t usage built with no errors) but no separate libpthread.a -- POSIX thread
  # support is part of the BSP's own librtemscpu, not a distinct archive to link. `-lpthread` is
  # only meaningful (and only linked) on the posix build.
  if(NOT RTEMS)
    target_link_libraries(psp_lockstep PUBLIC pthread)
  endif()
  set_target_properties(psp_lockstep PROPERTIES PREFIX "lib" OUTPUT_NAME "psp_lockstep")
  # Installed to the SAME per-CPU "cf/" directory `add_cfe_app`'s own `cfs_app_do_install`
  # (cfe/cmake/arch_build.cmake) installs every dynamically-loaded app module to -- these
  # variables are set by the surrounding cFS build (the same ones `add_cfe_app` itself reads,
  # already in scope here since this file is processed the same way every app's CMakeLists.txt
  # is) -- so the dynamic linker finds it alongside io_lockstep.so/sch_lockstep.so at load time
  # (services/cfs/container-entrypoint.sh's own `LD_LIBRARY_PATH` is the belt-and-suspenders
  # fallback in case this platform's own RPATH handling does not cover a plain, non-add_cfe_app
  # SHARED target). Empty (a no-op foreach) for the RTEMS static-app build, since
  # APP_DYNAMIC_TARGET_LIST is only populated for apps in a target's `_APPLIST`, not
  # `_STATIC_APPLIST` -- psp_lockstep is simply linked directly into io_lockstep/sch_lockstep's
  # own static libraries instead, see above.
  foreach(PSP_LOCKSTEP_INSTALL_TGT ${APP_DYNAMIC_TARGET_LIST})
    install(TARGETS psp_lockstep DESTINATION ${PSP_LOCKSTEP_INSTALL_TGT}/${INSTALL_SUBDIR})
  endforeach()
endif()

# M23.4's reconciliation: the CCSDS Space Packet codec (services/cfs/apps/shared/ccsds) is one
# shared static library, not a copy compiled into this app alone -- see that module's own doc
# comment for why. `if(NOT TARGET ...)` guards it the same way `psp_lockstep` is guarded above,
# since services/cfs/apps/adcs/CMakeLists.txt may be processed first in `cpu1_APPLIST` order.
if(NOT TARGET ccsds_codec)
  add_library(ccsds_codec STATIC ${CMAKE_CURRENT_LIST_DIR}/../shared/ccsds/src/ccsds_codec.c)
  target_include_directories(ccsds_codec PUBLIC ${CMAKE_CURRENT_LIST_DIR}/../shared/ccsds/inc)
endif()

set(IO_LOCKSTEP_SRC
  fsw/src/io_lockstep_app.c
  fsw/src/lockstep_local_framing.c
  fsw/src/lockstep_local_io.c
  fsw/src/pbmini.c
  fsw/src/lockstep_messages.c
  fsw/src/io_lockstep_port_table.c
)

add_cfe_app(io_lockstep ${IO_LOCKSTEP_SRC})
target_include_directories(io_lockstep PUBLIC fsw/inc)
target_link_libraries(io_lockstep psp_lockstep ccsds_codec)

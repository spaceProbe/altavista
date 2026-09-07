# M23.3 (docs/sil-plan.md M23, docs/open-questions.md question 146): the reference cFS ADCS
# application. M23.3 wrote adcs_app.c/adcs_control.c/adcs_packets.c but never built this
# CMakeLists.txt -- adcs_app.c had never been compiled against real cFE headers (M23.4's own
# reconciliation brief: "a file that has never been through a compiler is not evidence of
# anything"). Follows the same add_cfe_app pattern every cFS app CMakeLists.txt uses
# (cfe/cmake/arch_build.cmake's own add_cfe_app function), mirroring
# services/cfs/apps/io_lockstep/CMakeLists.txt's own shape.

# M23.4's reconciliation: the CCSDS Space Packet codec (services/cfs/apps/shared/ccsds) is one
# shared static library used by both this app and io_lockstep -- see that module's own doc
# comment. `if(NOT TARGET ...)` guards it since io_lockstep/CMakeLists.txt may be processed
# first in `cpu1_APPLIST` order (both define the same guarded target identically).
if(NOT TARGET ccsds_codec)
  add_library(ccsds_codec STATIC ${CMAKE_CURRENT_LIST_DIR}/../shared/ccsds/src/ccsds_codec.c)
  target_include_directories(ccsds_codec PUBLIC ${CMAKE_CURRENT_LIST_DIR}/../shared/ccsds/inc)
endif()

set(ADCS_SRC
  fsw/src/adcs_app.c
  fsw/src/adcs_control.c
  fsw/src/adcs_packets.c
)

add_cfe_app(adcs ${ADCS_SRC})
target_include_directories(adcs PUBLIC fsw/inc)
target_include_directories(adcs PRIVATE ${CMAKE_CURRENT_LIST_DIR}/../shared/mission)
target_link_libraries(adcs ccsds_codec)

# M24.3: appended (not substituted) onto the end of the fetched bundle's own
# third_party/cfs/target-configs.mk by build-cfs-cross.sh, adding one new CONFIG_NAME for the
# RTEMS 6.1 zynqmp_rpu_lock_step cross-build. This is cFE's own supported way to add a build
# config (see the existing pc686_rtems5/gr712_rtems5 entries in the same file for the pattern
# this mirrors) -- every existing CONFIG_NAMES entry (native_std included) is untouched.
#
# IMPORTANT (found running this exact append, M24.3): `CFS_CONFIG_NAMES` (target-configs.mk's
# own `CFS_CONFIG_NAMES := $(filter-out $(NONCFS_CONFIG_NAMES),$(CONFIG_NAMES))`, a few lines
# above the point this file gets appended) is a `:=` immediate-expansion assignment -- it is
# computed ONCE, from whatever CONFIG_NAMES held AT THAT LINE, before this appended block ever
# runs. Appending only to CONFIG_NAMES (this comment's first version did only that) leaves
# "rtems_zynqmp" out of CFS_CONFIG_NAMES, which silently drops it from `CFS_TARGETS`
# (`goal-configs.mk`: `CFS_TARGETS := $(foreach CFG,$(CFS_CONFIG_NAMES),...)`) -- and
# `target-rules.mk`'s `$(CFS_TARGETS): PREP_OPTS += -S "$(CURDIR)/cfe"` line, the ONLY place the
# cFE source directory is ever told to cmake, then never applies. The observed failure was
# `CMake Error: The source directory ".../third_party/cfs" does not appear to contain
# CMakeLists.txt` -- cmake fell back to defaulting -S to the current directory (third_party/cfs
# itself; the real CMakeLists.txt lives at third_party/cfs/cfe/CMakeLists.txt). Exit code was 2,
# correctly propagated by build-cfs-cross.sh's own `set -o pipefail` -- caught immediately, not
# discovered later as a false "success". Fix: append to CFS_CONFIG_NAMES too (a plain `+=` on an
# already-`:=`-defined variable just appends text, no re-filtering needed).
CONFIG_NAMES += rtems_zynqmp
CFS_CONFIG_NAMES += rtems_zynqmp

O_rtems_zynqmp = build-rtems_zynqmp
ARCH_rtems_zynqmp = arm-rtems6-zynqmp_rpu_lock_step

PREP_OPTS_rtems_zynqmp += -DSIMULATION=$(ARCH)
PREP_OPTS_rtems_zynqmp += -DMISSIONCONFIG=rtems_zynqmp
PREP_OPTS_rtems_zynqmp += -DCFE_EDS_ENABLED=OFF
PREP_OPTS_rtems_zynqmp += -DCMAKE_BUILD_TYPE=debug
# Found running this build (M24.3): osal/src/os/rtems/CMakeLists.txt unconditionally links
# `-lnetworking` for any RTEMS 6+ target when OSAL_CONFIG_INCLUDE_NETWORK is left at its default
# (ON) -- "In RTEMS 6+ the networking subsystem is not included with the default libs, it needs
# to be explicitly added to the final link" (that file's own comment). This RTEMS 6.1 toolchain
# build (M24.2c) never built a networking package (question 148's own research note: "build
# without a network stack") -- confirmed directly, `find .../toolchain -iname "*networking*"`
# finds nothing to link against. `ld: cannot find -lnetworking` at the FINAL executable link step
# (core cFE + all three lockstep apps had already compiled cleanly) was the exact, reproducible
# failure. Disabling it here also matches this task's own transport decision (question 148's
# "Transport" design note): io_lockstep never uses OSAL's socket API on this target anyway (its
# own connect_to_shim() opens a UART character device directly under
# AV_CFS_LOCKSTEP_TRANSPORT_UART), so OSAL's BSD-sockets code was dead weight even before the
# link failure made it a hard error.
PREP_OPTS_rtems_zynqmp += -DOSAL_CONFIG_INCLUDE_NETWORK=FALSE
PLATFORM_rtems_zynqmp  =  default_cpu1

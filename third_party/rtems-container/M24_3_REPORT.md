# M24.3 -- cross-build cFS (pinned) with the lockstep apps for `zynqmp_rpu_lock_step`

status: DONE. cFS (pinned) with all three lockstep apps cross-built for `zynqmp_rpu_lock_step`,
verified by artifact: `core-cpu1.exe` is a 7,549,648-byte ARM/EABI5/hard-float statically linked
ELF (confirmed independently by both `file` on the macOS host and `arm-rtems6-readelf -h`/`nm`
inside the container -- not by exit code), containing `IO_LOCKSTEP_AppMain`/`SCH_LS_AppMain`/
`ADCS_AppMain`/the codec and framing functions as real symbols (verified with `arm-rtems6-nm`
after an earlier attempt silently linked WITHOUT them -- see "Attempt 8" below, a real finding,
not assumed away). Eight failed build attempts preceded this, each a genuine bug found by reading
actual output rather than trusting an exit code; all recorded below with root cause and fix.
Zero patches carried to cFE/OSAL/PSP core source (question 148) -- every fix is a mission-config
override, a new toolchain file, or this task's own `services/cfs/` app code, using extension
points the pinned tree already documents. UART chosen as the M24.4 bridge transport (design
decision only, not run). Packet comparison against posix-build goldens: strongest available
evidence gathered (the exact codec/framing source embedded unchanged in the RTEMS build already
passes 14 host-native byte-exact tests against the kernel's own encoder) -- explicitly NOT
equivalent to running the RTEMS binary itself, which needs Renode (M24.4, out of scope).
`services/cfs/IMAGE_DIGEST.md` digest moved (this task edited three files the Dockerfile copies)
and has been rebuilt and updated. Full repo test suite: 377 passed, 0 failed. No Docker tags left
behind by this task.

Task: cross-compile the pinned cFS bundle (v7.0.1, `088b2fa828db9ff7e00733f1908e0eeb59f66ce3`) plus
`services/cfs/psp-lockstep`, `apps/sch_lockstep`, `apps/io_lockstep`, `apps/adcs`,
`apps/shared/ccsds` for the `zynqmp_rpu_lock_step` RTEMS 6.1 BSP, inside the
`rtems-m24c:build` container, using the toolchain/BSP M24.2c already built and verified there.
This task does NOT touch Renode and does NOT attempt to run the result -- exit criterion is a
cross-built ELF, verified by artifact (readelf/file), not an exit code.
docs/open-questions.md questions 144, 147, 148, 154, 157; docs/sil-plan.md M24.

## Not done / running list (all closed; kept for the record)

- [x] Toolchain cmake file for cFE (`services/cfs/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake`)
- [x] RTEMS-specific `targets.cmake` (static-linked apps, no dynamic loader, static symbol list)
- [x] `io_lockstep`/`sch_lockstep` CMakeLists change: `psp_lockstep` STATIC (not SHARED) and no
      `pthread` link when building for RTEMS, guarded so the POSIX/dynamic build is untouched
- [x] `io_lockstep_app.c` transport change: RTEMS has no network stack (question 148) -- swap
      `connect_to_shim`'s AF_UNIX connect for opening the BSP's second UART (`/dev/ttyS1`) as a
      plain fd, behind a compile-time guard; `lockstep_local_io.c`/`lockstep_local_framing.c`
      reused completely unchanged (they only ever see an `int fd`)
- [x] Build script (`third_party/rtems-container/build-cfs-cross.sh`) that runs inside the
      container against the bind-mounted `output/` toolchain/BSP
- [x] Actual cross-build run inside the container, artifacts verified (`file`/`arm-rtems6-readelf`/`nm`)
- [x] Patch inventory against upstream (question 148) -- none carried, see below
- [x] Packet comparison against M23.2 posix-build goldens (best-effort; no Renode) -- see below
- [x] Digest/test-impact check -- digest moved, rebuilt, updated, `test_image_digest.py` passes
- [x] Docker housekeeping (no stray tags/containers) -- confirmed clean
- [x] Full repo pytest -- 377 passed, 0 failed
- [x] `third_party/rtems/` confirmed untouched (`find ... -newer build.sh` returns nothing)

## What is NOT proven (explicit limits of this task)

- **The RTEMS binary has never been executed.** No Renode in this task by design (M24.4). The
  static-symbol-list fix (attempt 8) makes the app entry points reachable in principle via
  OSAL's `OS_SymbolLookup`, but whether the full RTEMS static-loader/ES startup sequence actually
  runs `IO_LOCKSTEP_AppMain` correctly at boot is untested and unproven -- M24.4's job.
- **The UART bridge transport is an unverified design choice**, not a working, tested
  implementation. `connect_to_shim`'s RTEMS branch is exactly one `open()` call; no code exists
  yet (in this task's scope) for a UART-speaking peer on the shim/Renode-bridge side -- that is
  M24.4/the Renode bridge's own work.
- **The packet comparison is the strongest evidence obtainable without Renode, not equivalent to
  a real run** -- see its own section below for exactly what it does and does not prove.
- **The `OS_STATIC_LOADER`/dynamic-loader runtime interaction is not fully characterized.** The
  build compiles and links with `OSAL_CONFIG_INCLUDE_DYNAMIC_LOADER` still at its default (TRUE)
  -- the RTEMS "kernel symbols" prelink/relink step ran (`rtems-syms`, visible in the attempt 8
  log) even though `RTEMS_DYNAMIC_LOAD` is FALSE for the executable itself. Whether this
  interacts correctly with the static-symbol-list mechanism at runtime is untested.

## Design decisions made so far (read-only investigation, recorded before any file was written)

- **PSP**: the pinned PSP repo (`third_party/cfs/psp/fsw/pc-rtems`) has NO board-specific code --
  read directly: `cfe_psp_start.c`/`cfe_psp_memory.c` use `malloc()`, generic `rtems_*` calls, no
  i686/PC addresses anywhere. `CFE_SYSTEM_PSPNAME=pc-rtems` is reused unchanged for the ARM/Zynq
  target; this is exactly the upstream-provided RTEMS 6 PSP, "pc-rtems" is a legacy name, not a
  board dependency. No PSP-side patch. `OSAL_SYSTEM_BSPTYPE=generic-rtems` (not `pc-rtems`,
  which is the older RTEMS4.11-era OSAL BSP) is the modern, BSP-agnostic OSAL RTEMS layer -- this
  exact pairing (`CFE_SYSTEM_PSPNAME=pc-rtems` + `OSAL_SYSTEM_BSPTYPE=generic-rtems` +
  `OSAL_SYSTEM_OSTYPE=rtems`) is copied verbatim from the pinned bundle's OWN shipped
  `third_party/cfs/sample_defs/toolchain-i686-rtems6.cmake` -- confirming this is upstream's own
  documented RTEMS 6 pattern, only the BSP name/ABI flags change for our arm target.
- **Toolchain glue**: `third_party/rtems-container/output/toolchain/lib/pkgconfig/arm-rtems6-zynqmp_rpu_lock_step.pc`
  (produced by M24.2c's BSP build) gives the exact ABI flags this BSP variant needs:
  `-march=armv7-r -mthumb -mfpu=vfpv3-d16 -mfloat-abi=hard`, `RTEMS_ARCH=arm`,
  `RTEMS_BSP=zynqmp_rpu_lock_step`. These are hand-copied into the new toolchain cmake file
  (cFE's toolchain files set these directly, they do not consume a .pc file).
- **Static, not dynamic apps**: the pinned bundle's own `toolchain-i686-rtems6.cmake` sets
  `RTEMS_DYNAMIC_LOAD FALSE` with a comment that this is "tied to the OSAL-BSP and PSP
  implementation" -- RTEMS's dynamic loader path is not the well-trodden one upstream ships a
  working sample for. This build therefore uses `cpu1_STATIC_APPLIST` (apps linked directly into
  one `core-cpu1.exe`), not `cpu1_APPLIST` (posix's dynamically-`dlopen()`ed `.so` modules) -- a
  real, load-bearing difference from the posix build, recorded here per the task's "diff your
  build against the posix one" instruction. Consequence: `services/cfs/apps/io_lockstep/CMakeLists.txt`'s
  M23.4 fix (`psp_lockstep` built `SHARED` so two independently-`dlopen()`ed modules share one
  copy of its mutable state) does not apply the same way here -- with one statically-linked
  executable there is only ever one copy of `psp_lockstep`'s globals regardless, so it is built
  `STATIC` for this target instead (guarded, POSIX path unchanged). This is a `services/cfs/`
  change; see the digest note above.
- **Transport**: RTEMS 6 here has **no network stack** (question 148's own research note,
  confirmed independently: RSB did not build `librtemsbsd`/any networking package, and the
  container's BSP `.pc` file/build log show no networking config). `io_lockstep`'s only
  transport-specific code is `connect_to_shim()` in `io_lockstep_app.c` (one function, ~15
  lines) -- `AF_UNIX`/`socket()`/`connect()` calls that do not exist without a network stack.
  Everything else in the frame path (`lockstep_local_io.c`'s `lockstep_write_frame`/
  `lockstep_read_frame`, `lockstep_local_framing.c`, `pbmini.c`, `lockstep_messages.c`,
  `ccsds_codec.c`) operates on a plain `int fd` and needs zero changes -- this is exactly
  question 153's "the shim is written once and reused" at the C level, not just the Rust shim
  level. Chosen bridge: **UART**, specifically the BSP's second UART, `/dev/ttyS1` (read
  directly from `third_party/rtems-container/work/rtems-src/bsps/arm/xilinx-zynqmp-rpu/console/console-config.c`:
  this BSP registers exactly two Zynq UART instances as RTEMS termios devices
  `/dev/ttyS0`/`/dev/ttyS1`, and links whichever one matches `ZYNQ_UART_KERNEL_IO_BASE_ADDR` as
  the debug console -- UART0 in this BSP). UART1 is free for the lockstep-local bridge, opened
  with a plain POSIX `open()`/`read()`/`write()` exactly like the existing Unix-socket fd, so
  `lockstep_local_io.c` needs no changes at all. Ethernet was not chosen: it would need a
  network stack this RTEMS 6 build does not have (the same absence that rules out keeping
  AF_UNIX), and Renode's own M24.1 spike already proved UART output on this exact BSP/platform
  pairing (`hello`/`ticker`'s captured UART logs, M24.2c), so UART is also the transport this
  task has direct, already-collected evidence for. This is a design decision for M24.3/M24.4,
  not something this task can run end-to-end (that needs the M24.4 Renode bridge and a
  UART-speaking peer on the shim side, both out of scope here) -- recorded as an unverified
  design choice, not a tested one.

## Build attempts and real bugs found (updated as the run progresses)

**Attempt 1** (`make rtems_zynqmp.prep`): failed immediately --
`CMake Error: The source directory "/workspace/third_party/cfs" does not appear to contain
CMakeLists.txt`. Root cause, found by reading `target-rules.mk`/`goal-configs.mk`, not guessed:
`CFS_CONFIG_NAMES` is a `:=` (immediate-expansion) variable computed from `CONFIG_NAMES` a few
lines ABOVE where this task's appended block lands in `target-configs.mk` -- appending only to
`CONFIG_NAMES` (this attempt's mistake) leaves the new "rtems_zynqmp" entry out of
`CFS_CONFIG_NAMES`, which silently drops it from `CFS_TARGETS`, which is the only place
`-S "$(CURDIR)/cfe"` (the actual cFE source directory) ever gets passed to cmake. `set -o
pipefail` in `build-cfs-cross.sh` caught this immediately (exit code 2 propagated, not
swallowed) -- fixed by also appending to `CFS_CONFIG_NAMES` in
`services/cfs/build/target-configs-append-rtems.mk` (full account in that file's own comment).

**Attempt 2** (prep succeeded this time -- static apps correctly detected: `-- Building Static
App: io_lockstep targets=cpu1` / `sch_lockstep` / `adcs`, confirming the toolchain file's
`RTEMS_DYNAMIC_LOAD FALSE` and `targets-rtems.cmake`'s `STATIC_APPLIST` took effect as intended;
compile got well into cFE core before failing): `cfe/modules/es/fsw/src/cfe_es_perf.c:644:
error: 'CFE_MISSION_ES_PERF_EXIT_BIT' undeclared`. Root cause, found by reading (not guessing):
`cfe_perfids.h` is generated by `generate_configfile_set` (`cfe/modules/core_api/mission_build.cmake`)
the same way every other mission-config header is, but -- unlike those -- it has **no**
`cfe/modules/*/config/default_*.h` fallback; when the mission's own `_defs/` directory (ours,
`rtems_zynqmp_defs/`, freshly created by this task) does not supply one, `generate_configfile_set`
silently emits an EMPTY header (`#ifndef .../#define .../#endif`, confirmed by reading
`build-rtems_zynqmp/inc/cfe_perfids.h` directly) rather than erroring at configure time -- so the
missing file is invisible until the first translation unit that actually uses a reserved
core perf-ID constant. The posix build never hits this because the fetched bundle's own
`sample_defs/cfe_perfids.h` (MISSION_DEFS for `native_std`) already has it. Fix:
`services/cfs/build/cfe_perfids.h`, an unmodified copy of this exact pinned commit's own
`cfe/cmake/sample_defs/cfe_perfids.h` (diffed byte-for-byte against `sample_defs/cfe_perfids.h`
before copying -- identical apart from a license-header date, confirming this is the intended
upstream template, not something requiring app-specific customization), copied into
`rtems_zynqmp_defs/` by `build-cfs-cross.sh` alongside the other override files. Not a
cFE/OSAL/PSP patch (question 148) -- an unmodified upstream file this task's own new mission-defs
directory was simply missing.

Also noted, not chased further (does not block the build, present on prep's stdout both times):
`CMake Warning ... Mismatched PSP/BSP: pc-rtems implies pc-rtems, but generic-rtems is
configured`. Checked against the pinned bundle's OWN shipped `sample_defs/toolchain-i686-rtems6.cmake`
before dismissing it: that file sets the identical pairing
(`CFE_SYSTEM_PSPNAME=pc-rtems` + `OSAL_SYSTEM_BSPTYPE=generic-rtems`), so this warning is
upstream's own preexisting behavior for its own documented RTEMS 6 sample, not something this
task's toolchain file introduced -- recorded rather than silently ignored, per "investigate any
oddity before reporting it as normal."

**Attempt 3** (compile succeeded completely -- cFE core, OSAL, PSP, and all three lockstep apps
(io_lockstep, sch_lockstep, adcs) all compiled with zero errors; only the FINAL link of
`core-cpu1.exe` failed): two missing libraries at link time --
```
ld: cannot find -lnetworking: No such file or directory
ld: cannot find -lpthread: No such file or directory
```
Both root-caused by reading, not guessed:
1. `-lnetworking`: `third_party/cfs/osal/src/os/rtems/CMakeLists.txt` unconditionally links
   `networking` for any RTEMS 6+ target when `OSAL_CONFIG_INCLUDE_NETWORK` (default ON) is set --
   its own comment: "In RTEMS 6+ the networking subsystem is not included with the default libs,
   it needs to be explicitly added to the final link." This toolchain (M24.2c) never built a
   networking package -- confirmed directly, `find .../toolchain -iname "*networking*"` finds
   nothing. This is exactly question 148's "no network stack" landing in a concrete link error.
   Fix: `-DOSAL_CONFIG_INCLUDE_NETWORK=FALSE` added to `PREP_OPTS_rtems_zynqmp`. Consistent with
   this task's own transport decision -- io_lockstep never uses OSAL sockets on this target
   anyway (UART instead), so this code was dead weight even before the link failure.
2. `-lpthread`: this task's own `services/cfs/apps/io_lockstep/CMakeLists.txt` (and
   `sch_lockstep`'s identical copy) does `target_link_libraries(psp_lockstep PUBLIC pthread)`.
   Confirmed by inspection: the toolchain ships `<pthread.h>` (psp_lockstep.c's
   `pthread_mutex_t`/`pthread_cond_t` usage compiled with zero errors) but there is no separate
   `libpthread.a` anywhere under the toolchain prefix -- RTEMS bundles POSIX thread support
   directly into `librtemscpu`, not a distinct archive. Fixed by guarding that
   `target_link_libraries` call with `if(NOT RTEMS)` in both apps' CMakeLists.txt (`RTEMS` is set
   by `psp/cmake/Modules/Platform/RTEMS.cmake`, loaded automatically once `CMAKE_SYSTEM_NAME` is
   RTEMS).

**Attempt 4** (the `-lpthread` fix worked -- gone from the link error entirely; `-lnetworking`
persisted UNCHANGED, identical error text): investigated rather than assumed the
`OSAL_CONFIG_INCLUDE_NETWORK=FALSE` fix was simply wrong -- the actual `cmake` invocation line
logged by `make rtems_zynqmp.prep` (`00-summary.log`/stdout) still showed the OLD `PREP_OPTS`
list with no `-DOSAL_CONFIG_INCLUDE_NETWORK` flag at all, and the compile log still showed
`os-impl-network.c`/`os-impl-bsd-sockets.c` being built. Root cause: `build-cfs-cross.sh`'s
append step checked only whether the marker text was PRESENT in `target-configs.mk`, not whether
it matched the CURRENT tracked `target-configs-append-rtems.mk` -- since attempt 2 had already
appended the (pre-fix) block, the presence check short-circuited and the edited file (now
carrying the network fix) was never re-applied to the persistent host-side
`third_party/cfs/target-configs.mk`. A real "silent stale config" bug in this task's OWN
tooling, caught by reading the actual invoked command line rather than assuming the fix must
have worked. Fixed by making the append step strip any previous M24.3 block (found by its own
sentinel comment) before re-appending fresh, every run -- idempotent by CONTENT, not by presence.

**Attempt 5** (`-lpthread` gone, `-lnetworking` STILL failed, byte-identical error, despite the
mission-level `cmake` invocation now correctly showing `-DOSAL_CONFIG_INCLUDE_NETWORK=FALSE` in
its own command line): investigated by comparing `CMakeCache.txt` directly rather than assuming
the flag "should have worked" -- `third_party/cfs/build-rtems_zynqmp/CMakeCache.txt` (the
mission-level/outer build tree) correctly showed `OSAL_CONFIG_INCLUDE_NETWORK:BOOL=FALSE`, but
`third_party/cfs/build-rtems_zynqmp/arm-rtems6-zynqmp_rpu_lock_step/default_cpu1/CMakeCache.txt`
(a SEPARATE, nested cache -- this is the actual arch-specific build tree where OSAL gets
configured, per the "Configuring for system arch: ..." log line) still showed `TRUE`. Root cause,
read directly in `cfe/cmake/mission_build.cmake`'s `process_arch()`: the arch-level build tree is
generated by a nested `execute_process(COMMAND ${CMAKE_COMMAND} ...)` subprocess that forwards
only a small fixed variable whitelist (`TARGETSYSTEM`, `MISSION_BINARY_DIR`, `CMAKE_BUILD_TYPE`,
`CMAKE_INSTALL_PREFIX`, `CMAKE_PREFIX_PATH`, `CMAKE_EXPORT_COMPILE_COMMANDS`,
`CFE_EDS_ENABLED`) plus `-DCMAKE_TOOLCHAIN_FILE=<toolchain file>` -- arbitrary extra `-D` flags
from the mission-level `PREP_OPTS` (including ours) are simply never passed through to this
subprocess. Fix: move the override into the toolchain file itself (the one thing this
subprocess IS guaranteed to receive) -- `set(OSAL_CONFIG_INCLUDE_NETWORK FALSE CACHE BOOL "..."
FORCE)` added to `services/cfs/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake` (a
toolchain file runs early enough, and with `FORCE`, to override `osal/default_config.cmake`'s own
`CACHE BOOL` default regardless of processing order).

**Attempt 6** (the `-lnetworking` fix worked -- the executable link finally got past that
symbol; three NEW undefined-reference groups surfaced at the same final link step):
```
undefined reference to `bsp_cmdline'
undefined reference to `IDE_Controller_Table' / `IDE_Controller_Count' (from bsps/shared/dev/ide/{ata,ide_controller}.c)
undefined reference to `Stack_checker_Reporter' (from cpukit/libmisc/stackchk/check.c)
```
All three root-caused by reading, not guessed:
1. `bsp_cmdline`: `osal/src/bsp/generic-rtems/src/bsp_cmdline.c` calls a BSP-supplied
   `bsp_cmdline()` this BSP variant does not implement (the compile step had already warned
   "implicit declaration of function 'bsp_cmdline'" -- a warning this task should have chased
   immediately rather than letting reach link time, noted for next time). Fixed: generic-rtems's
   own `RTEMS_NO_CMDLINE` option (its CMakeLists.txt: `if (RTEMS_NO_CMDLINE) ... bsp_no_cmdline.c
   ... else ... bsp_cmdline.c`), set via the toolchain file for the same variable-forwarding
   reason as `OSAL_CONFIG_INCLUDE_NETWORK` above.
2. `IDE_Controller_Table`/`Count`: `osal/src/bsp/generic-rtems/config/default_bsp_rtems_cfg.h`
   (that file's own header: "may be overridden/superseded by mission-provided definitions")
   unconditionally sets `CONFIGURE_APPLICATION_NEEDS_IDE_DRIVER`/`_ATA_DRIVER`, real RTEMS
   `confdefs.h` options registering a PC-style parallel-ATA disk driver -- this whole
   generic-rtems BSP glue traces back to the "pc686" PC target (`psp/fsw/pc-rtems/README.txt`'s
   own setup instructions). Zynq has no IDE/ATA hardware. Fixed: `services/cfs/build/bsp_rtems_cfg.h`,
   an otherwise-identical copy of the default with only those two macros removed, placed at
   `rtems_zynqmp_defs/bsp_rtems_cfg.h` (found via `cfe_locate_implementation_file`'s own search
   order, read directly in `cfe/cmake/global_functions.cmake` -- a direct filename match in
   `MISSION_DEFS` is always checked, so no per-arch prefix subdirectory was needed).
3. `Stack_checker_Reporter`: read directly in `cpukit/include/rtems/confdefs/extensions.h` --
   this symbol is only DEFINED when `CONFIGURE_STACK_CHECKER_ENABLED` is set (this build does not
   set it, and never installs `RTEMS_STACK_CHECKER_EXTENSION` either, so the referencing function
   is genuinely dead code for this build) -- but GNU `ld` links whole `.o` files, not individual
   functions, so `check.c.o` still got pulled in whole for some other symbol in the same file.
   This BSP's own pkg-config file already documents `-Wl,--gc-sections` in its normal Ldflags for
   exactly this reason. Fixed: `-ffunction-sections -fdata-sections -Wl,--gc-sections` appended
   to `RTEMS_BSP_C_FLAGS`/`RTEMS_BSP_CXX_FLAGS` in the toolchain file (not to
   `RTEMS_SYS_LINKFLAGS`, which `psp/cmake/Modules/Platform/RTEMS.cmake` itself sets to `"-u
   Init"` -- overwriting that variable risks dropping the RTEMS entry-point-preserving flag;
   `RTEMS_BSP_C_FLAGS` is provably on the same link command line without that risk, confirmed by
   reading `RTEMS.cmake`'s own `CMAKE_C_LINK_EXECUTABLE` template).

**Attempt 7** (`bsp_cmdline` and `IDE_Controller_*` both fixed -- gone from the link error;
`Stack_checker_Reporter` alone persisted, byte-identical error text, despite the `--gc-sections`
fix from attempt 6): investigated by reading the ACTUAL generated build rules rather than
assuming the flag change had no effect for some deeper reason -- `grep` against the real
`build.make`/`link.txt` files under `build-rtems_zynqmp/arm-rtems6-zynqmp_rpu_lock_step/default_cpu1/`
showed neither `-ffunction-sections` nor `--gc-sections` anywhere in any actual compile or link
command line, even though the ABI flags (`-march=armv7-r` etc, also carried by
`RTEMS_BSP_C_FLAGS`) WERE present. Root cause: `psp/cmake/Modules/Platform/RTEMS.cmake`'s own
`set(CMAKE_C_COMPILE_OBJECT "... ${RTEMS_BSP_C_FLAGS} ...")` uses `${...}` syntax, which CMake
expands IMMEDIATELY when that line executes -- baking in whatever value `RTEMS_BSP_C_FLAGS` held
at THAT moment as literal text in the rule template. `string(APPEND RTEMS_BSP_C_FLAGS ...)` later
in this toolchain file changes the variable but not the already-expanded template string. Fixed
by using `CMAKE_C_FLAGS`/`CMAKE_CXX_FLAGS`/`CMAKE_EXE_LINKER_FLAGS` instead -- the SAME templates
reference these via angle-bracket placeholders (`<FLAGS>`, `<CMAKE_C_LINK_FLAGS>`,
`<LINK_FLAGS>`), which CMake's generator fills in per-target from the variables' FINAL value at
generate time, immune to the ordering issue that broke the `RTEMS_BSP_C_FLAGS` approach.

**Attempt 8: `core-cpu1.exe` LINKED (exit 0) -- but a real, load-bearing finding was caught by
verifying artifact CONTENT, not the exit code (exactly the discipline this task's brief warns
about).** The ELF exists, is a plausible size (7,382,452 bytes), and `file`/`arm-rtems6-readelf -h`
both independently confirm ARM/EABI5/hard-float. **Investigated further rather than declared done
at that point**: `arm-rtems6-nm core-cpu1.exe | grep -iE 'lockstep|adcs|ccsds'` returned exactly
ONE match (`CFE_MSG_SetDefaultCCSDSPri`, a core cFE function whose name happens to contain
"CCSDS") -- **zero** matches for `IO_LOCKSTEP_AppMain`, `SCH_LS_AppMain`, `ADCS_AppMain`,
`psp_lockstep_release_tick`, `ccsds_encode_packet`, or anything else app-specific, despite the
build log showing "Built target io_lockstep/sch_lockstep/adcs" and their `.a` files being present
on the final link command line. Root cause, read in `cfe/docs/README_static_app_linkage.md`: cFE's
static-app-linkage design never references an app's entry point from any C code directly -- a
generated `cfe_static_symbol_list.c` (built from `TGT<x>_STATIC_SYMLIST`, which
`targets-rtems.cmake` never set) is what normally supplies that reference; with attempt 7's
`-ffunction-sections`/`--gc-sections` fix (needed for the `Stack_checker_Reporter` link error),
these now-genuinely-unreferenced app entry points were legitimately garbage-collected -- not a
linker bug, a real gap in this task's own mission config that `--gc-sections` made visible rather
than caused. Fixed: `cpu1_STATIC_SYMLIST` added to `targets-rtems.cmake`
(`IO_LOCKSTEP_AppMain,IO_LOCKSTEP SCH_LS_AppMain,SCH_LOCKSTEP ADCS_AppMain,ADCS`, matching the
4th-field module names `generate_startup.cmake`'s startup script already uses).

**Attempt 9: SUCCESS, verified by content.** With `cpu1_STATIC_SYMLIST` added, `core-cpu1.exe`
linked (exit 0, as expected -- this task's own standing rule is that this alone is never
evidence) AND, checked independently with `arm-rtems6-nm` immediately afterward rather than
assumed fixed, all three app entry points plus the codec/framing functions are now present as
real symbols. See "Final artifact" below for the full, current verification.

## Final artifact (verified by content, attempt 9 -- the successful one)

`third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe`:

- `file`: `ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, with
  debug_info, not stripped` -- run independently TWICE: once by `build-cfs-cross.sh` inside the
  container, once by this session directly on the macOS host's own `file` (a different binary
  than the container's, same answer).
- `arm-rtems6-readelf -h`: `Machine: ARM`, `Flags: 0x5000400, Version5 EABI, hard-float ABI`,
  `Type: EXEC`.
- Size: 7,549,648 bytes (a plausible size for a full cFE core + OSAL + PSP + three apps + two
  test/assert modules statically linked -- roughly 4x the size of the M24.2c `hello.exe` sample,
  consistent with the much larger amount of linked code).
- SHA-256: `db7f3daaa4eef94dbabd28744f8fd3d6d74009233ca658f2baf87856718a6e18`.
- **App symbols present** (`arm-rtems6-nm`, run independently after the build, not trusted from
  the build's own log): `IO_LOCKSTEP_AppMain`, `SCH_LS_AppMain`, `ADCS_AppMain`,
  `ccsds_encode_packet`, `ccsds_decode_packet`, `lockstep_write_frame`, `lockstep_read_frame`,
  `psp_lockstep_release_tick` all present as global (`T`) text symbols -- this is the check that
  caught attempt 7's silent gc-sections-ate-the-apps failure, so it was re-run here rather than
  assumed fixed.
- Also produced and installed: `cfe_es_startup.scr` (the hardcoded startup script naming all
  three apps), `cfe_test_tbl.tbl`, `cfe_testcase.obj`/`cfe_assert.obj` (the cFE unit-test/assert
  RTEMS dynamic-loadable modules -- built as `.obj` relocatable objects per RTEMS's own dynamic-load
  convention, since `ENABLE_UNIT_TESTS` was not explicitly disabled for this config; harmless,
  not exercised).

## Patch inventory (question 148)

**No patch is carried to any file under `third_party/cfs/{cfe,osal,psp}` (or their `modules/`
subdirectories).** Every fix in this report's "Build attempts" section is one of:

1. A new mission-defs file in a NEW `third_party/cfs/rtems_zynqmp_defs/` directory (`targets.cmake`,
   the toolchain file, `cfe_perfids.h`, `bsp_rtems_cfg.h`, `generate_startup.cmake`,
   `cpu1/install_custom.cmake`) -- all using extension points the pinned tree's own code
   documents and expects missions to use (`cfe_locate_implementation_file`'s mission-defs
   override search, `default_bsp_rtems_cfg.h`'s own "may be overridden" header comment,
   `README_static_app_linkage.md`'s documented `STATIC_SYMLIST` mechanism).
2. Cache-variable overrides set FROM the toolchain file (`OSAL_CONFIG_INCLUDE_NETWORK`,
   `RTEMS_NO_CMDLINE`, `CMAKE_C_FLAGS`/`CMAKE_EXE_LINKER_FLAGS` for `--gc-sections`) -- standard
   CMake toolchain-file mechanics, not edits to any tracked upstream file.
3. One appended `CONFIG_NAMES` entry in `third_party/cfs/target-configs.mk` (the file's own
   documented, supported way to add a build config -- existing `pc686_rtems5`/`gr712_rtems5`
   entries are the same pattern) -- appended, not substituted; every existing entry (including
   `native_std`) is byte-for-byte unchanged.
4. This task's OWN `services/cfs/` app code (`io_lockstep_app.c`'s transport guard,
   `io_lockstep`/`sch_lockstep` CMakeLists' `RTEMS`-guarded library-type/pthread changes) --
   already this task's to own, not upstream.

No upstream GitHub issue was searched for or is cited for any of the RTEMS-6-vs-generic-rtems
config gaps found (the ATA/IDE default, `bsp_cmdline`, the missing `-lnetworking` package, the
`Stack_checker_Reporter`/gc-sections interaction) -- each is a configuration/build-system fact
about this pinned commit's own generic-rtems BSP glue, evidenced directly by reading its source
and confirmed by the actual link errors, not a defect this task judged worth reporting upstream.
Consistent with question 148's own framing ("the team pins the cFS bundle at a commit with
working RTEMS 6 ... builds and reports what had to be patched"): the answer for this pin, this
BSP, is "nothing patched -- five build-system/config gaps closed via documented extension
points."

## Packet comparison against the posix build's goldens

No Renode in this task (by design) means the RTEMS ELF cannot actually be run to capture its own
emitted packets. Per the task's own fallback guidance, the strongest available evidence short of
that is comparing the SAME codec/framing/message source this RTEMS build embeds against M23.2's
existing goldens -- and that comparison already exists and already runs on every commit:
`services/cfs/tests/test_ccsds_golden.py`, `test_lockstep_local_framing.py`,
`test_lockstep_messages.py`, `test_psp_lockstep_no_wallclock.py` each compile
`apps/shared/ccsds/src/ccsds_codec.c` / `apps/io_lockstep/fsw/src/{lockstep_local_framing,pbmini,
lockstep_messages}.c` with the HOST's native `cc` and assert byte-exact equality against fixtures
captured from `crates/av-kernel::codec::encode_packet`/`prost`'s own real output
(`services/cfs/tests/fixtures/*.json`, `services/cfs/tests/golden_gen/`).

Ran now (`.venv/bin/pytest -q services/cfs/tests/test_ccsds_golden.py
services/cfs/tests/test_lockstep_local_framing.py services/cfs/tests/test_lockstep_messages.py
services/cfs/tests/test_psp_lockstep_no_wallclock.py`): **14 passed in 3.24s, zero failures.**

Why this is the right evidence for THIS build specifically, not just a preexisting fact restated:
none of `ccsds_codec.c`, `lockstep_local_framing.c`, `lockstep_local_io.c`, `pbmini.c`, or
`lockstep_messages.c` were changed for the RTEMS target -- confirmed both by this task's own diff
(the only source changes are `io_lockstep_app.c`'s `connect_to_shim` transport swap, entirely
outside these files) and directly in the RTEMS build log itself: `[80%] Building C object
apps/io_lockstep/CMakeFiles/ccsds_codec.dir/workspace/third_party/cfs/apps/shared/ccsds/src/ccsds_codec.c.o`
compiled cleanly with `arm-rtems6-gcc`, the identical, unmodified file these host-native tests
already prove byte-exact. This is portable ISO C (bit-shifts, memcpy, IEEE-754 double bit
patterns -- no platform `#ifdef` branches, no inline asm, no compiler-specific extensions in any
of these five files), so the same C standard's guarantees that make the host-native comparison
meaningful extend to the cross-compiled object built from the identical source. This is NOT the
same strength as actually running the RTEMS binary and capturing its own frames (a
compiler-codegen divergence between host gcc and arm-rtems6-gcc, however unlikely for this kind
of code, cannot be ruled out this way) -- recorded as the honest ceiling of what this task can
prove without Renode, not overstated as equivalent to a real run.

## Scope note on `third_party/cfs`

Not in the do-not-modify list (only `third_party/rtems/`, `third_party/renode/`, `web/`,
`altavista/`, `crates/`, `drms/`, `goldens/`, `proto/`, `docs/adr/` are) and not committed to git
(`third_party/fetch-cfs.sh`'s own header: "Nothing under third_party/cfs is committed"). This
task adds a new mission-defs directory `third_party/cfs/rtems_zynqmp_defs/` (cFE's own supported
customization mechanism, `-DMISSIONCONFIG=rtems_zynqmp` selects it) populated from tracked files
under `services/cfs/build/`, and appends one new `CONFIG_NAMES` entry to
`third_party/cfs/target-configs.mk` -- exactly parallel to how `services/cfs/Dockerfile` already
overwrites `sample_defs/targets.cmake` for the posix build. `sample_defs/` and the existing
`native_std` config are untouched by this addition.

## Image digest (services/cfs/IMAGE_DIGEST.md)

Editing anything under `services/cfs/` moves the recorded posix-image digest and fails
`test_image_digest.py` (per this task's own brief, and per `IMAGE_DIGEST.md`'s own "M26.1" note
about exactly this). Three files this task changed ARE copied by `services/cfs/Dockerfile`:
`apps/io_lockstep/fsw/src/io_lockstep_app.c`, `apps/io_lockstep/CMakeLists.txt`,
`apps/sch_lockstep/CMakeLists.txt`. Confirmed this is exactly what happened (not assumed): built
`docker build -f services/cfs/Dockerfile -t altavista-cfs-lockstep:local .` again -- new digest
`sha256:baac4535533094bbfddced863779714345519e71d9be0d2218212186f4151b82`, different from the
previously-recorded `sha256:3bae6444cbb371a79d2e4b327c21b03764c57778c215ddf4b82614642c1f5c0d`
even though every changed file's posix-build behavior is unchanged (all three changes are guarded
by `#ifdef AV_CFS_LOCKSTEP_TRANSPORT_UART` / `if(RTEMS)`, neither of which this posix Dockerfile
ever defines) -- file CONTENT changed, which is what a content-addressed image ID hashes,
regardless of behavioral equivalence. `services/cfs/IMAGE_DIGEST.md` updated with the new digest
and a full account of why (its own new "Rebuilt 2026-09-06 for M24.3" section).
`services/cfs/tests/test_image_digest.py` now passes against the new digest (see "Final
verification" below).

## Final verification

- `core-cpu1.exe` exists, is ARM/EABI5, contains all three app entry points as real symbols --
  see "Final artifact" above. Verified by content on both the host and inside the container, not
  by any exit code.
- `services/cfs/tests/` (17 tests, includes the now-updated `test_image_digest.py`): **17 passed,
  0 failed** (`.venv/bin/pytest -q services/cfs/tests/`).
- Full repo suite: **377 passed, 5 warnings, 0 failed** in 146.71s (`.venv/bin/pytest -q`) --
  matches M24.2c's own already-reconciled 377-count (not the 355 baseline named in older task
  briefs; see that report's own discrepancy note, attributed to concurrent work elsewhere in the
  repo, out of scope to chase further here since this session's changes are limited to
  `services/cfs/` and `third_party/rtems-container/`).
- `third_party/rtems/` confirmed untouched throughout this task
  (`find third_party/rtems -newer third_party/rtems-container/build.sh` returns nothing).
- Docker housekeeping (question 156): this task's every `docker run` used `--rm`, and all exited
  containers were confirmed gone afterward (`docker ps -a` shows no `rtems-m24c-cfs-cross`
  survivors). `altavista-cfs-lockstep:local` and `rtems-m24c:build` are the same, expected,
  persistent local tags the posix-image and toolchain/BSP workflows already depend on
  (re-tagging `altavista-cfs-lockstep:local` with new content, as `test_image_digest.py`'s own
  workflow does on every run, is not a "stray" tag). **Investigated, not ignored**: the full
  repo pytest run (`.venv/bin/pytest -q`, part of this task's own final verification) left a
  `127.0.0.1:33563/lockstep-ref:test` registry-port-tagged image behind -- exactly the shape of
  leftover question 156's own amendment describes (a `crates/av-kernel` container test's
  throwaway local registry). Not this task's own build/test code, but caused by running the
  suite from this session -- removed with `docker rmi` rather than left for someone else to find,
  and confirmed gone afterward.
- Network use (question 154): one `apt-get install cmake` inside the `rtems-m24c:build` container
  (that image, built for M24.2c's toolchain task, never needed cmake) -- a one-time, recorded
  exception, the same class of use `services/cfs/Dockerfile` itself already documents for its own
  package installs. `third_party/fetch-cfs.sh` ran on every attempt but was always a no-op
  ("already present and pinned") since `third_party/cfs` was already fetched on the host. The
  posix-image rebuild (for the digest) needed no new network either (same reason, cached layers).

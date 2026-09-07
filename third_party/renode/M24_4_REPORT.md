# M24.4 -- Renode bridge and identical-traffic comparison

status: **partial** (items 1, 2, 5 and the GIC paragraph DONE and verified; item 3 substantial
progress with one unresolved blocker; item 4 blocked on item 3, one piece independently
verified -- see "Final status and complete 'not done' list" at the end of this file)

Task: `docs/sil-plan.md` M24 exit criterion (identical port traffic, posix container against
Renode, under lockstep; hardware faults through `FAULT_TARGET_KIND_HARDWARE`), plus
`docs/open-questions.md` questions 144, 145, 153, 155, 156 (and its 2026-09-06 amendment), 157,
164. Builds directly on M24.2d's root cause (`third_party/renode/M24_2d_REPORT.md`: TTC counts at
~1/3 rate, no `frequency:` on `ttc0`-`ttc3` in the shipped `zynqmp.repl`; `BSP_RESET_BOARD_AT_EXIT`
defaults on and busy-spins forever on a `Tag`-stub `CRL_APB` register) and M24.3's cross-built cFS
(`third_party/rtems-container/M24_3_REPORT.md`: `third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe`,
verified by content, UART transport, `/dev/ttyS1`).

Written incrementally, measurements first, per question 157's rule (a missing final chat message
must lose nothing). Sized for ~300 tool uses; long runs are backgrounded, not polled.

## Not done yet (running list, updated as items close)

- [x] 1. Our own platform file with explicit TTC `frequency:`, TTC-rate-vs-virtual-time test
      (must pass before anything else is trusted) -- PASSED, see below
- [x] 2. `BSP_RESET_BOARD_AT_EXIT` off for the lockstep guest; WFI-idle test; wall-time-per-
      virtual-second test proving the busy-spin is gone -- PASSED, see below
- [~] 3. The bridge process -- **partial**: boot blocker root-caused and fixed (a real PSP
      patch, verified live: cFE + all 3 lockstep apps now start under Renode), UART1-to-TCP
      wiring proven live, but the lockstep-local handshake over UART1 itself does not yet
      succeed against a minimal test peer (root cause not found within budget) -- the relay
      process and the `demo_attitude_control` Renode-bound DRM are NOT built (see below)
- [~] 4. Identical port traffic -- **not attempted, blocked on item 3's handshake**; concrete
      plan recorded. HARDWARE fault -- **partial, real evidence**: a Renode `machine Reset`
      genuinely reboots cFE (confirmed by a second complete boot sequence in the UART log);
      wiring a bridge's `Reset` RPC to this command is not yet built (bridge does not exist)
- [x] 5. Question 156 label-and-prune guard (registry-port label, prune-before-start, prune-on-
      interrupt) plus its test -- implemented, `cargo check` clean, tests running (see below)
- [x] GIC `ISENABLER1=0` paragraph -- done, see below

## Item 1 -- our own platform file, TTC frequency, and the gate test

**File:** `third_party/renode/platforms/cpus/zynqmp.repl`, derived from the shipped
`third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/platforms/cpus/zynqmp.repl`.

**Shipped file SHA-256:** `0f94778ad81732a9b2c043b569e5d53ca12cc1314832f8eac088e8fe949cb655`
(`shasum -a 256` against the file on disk under the Renode.app bundle, run directly, not
copied from a prior report).

**Diff** (full `diff -u` output; everything else is byte-identical -- an 18-line header
comment plus one `frequency: 100000000` line added after each of `ttc0`-`ttc3`'s IRQ
fan-out line):

```diff
@@ -1,3 +1,23 @@
+// AltaVista M24.4 platform file, derived from Renode 1.16.1's own shipped
+// ... (full rationale comment, see the file itself) ...
+
 // Clusters definitions
 cluster0: CPU.Cluster @ sysbus
@@ -168,15 +188,19 @@
 ttc0: Timers.Cadence_TTC @ sysbus 0xff110000
     [0-2] -> apuGic@[36-38] | rpuGic@[36-38]
+    frequency: 100000000

 ttc1: Timers.Cadence_TTC @ sysbus 0xff120000
     [0-2] -> apuGic@[39-41] | rpuGic@[39-41]
+    frequency: 100000000

 ttc2: Timers.Cadence_TTC @ sysbus 0xff130000
     [0-2] -> apuGic@[42-44] | rpuGic@[42-44]
+    frequency: 100000000

 ttc3: Timers.Cadence_TTC @ sysbus 0xff140000
     [0-2] -> apuGic@[45-47] | rpuGic@[45-47]
+    frequency: 100000000
```

`100000000` matches the RTEMS BSP's own `XIL_CLOCK_TTC_REFERENCE_CLOCK` (read directly from
`third_party/rtems-container/work/rtems-src/bsps/shared/dev/clock/xil-ttc.c`, the same source
M24.2d read). **`frequency:` is confirmed as a real, working property name for this exact
build's `Timers.Cadence_TTC`, not merely assumed or guessed from the property-introspection
dead end M24.2d hit** (`ttc0 Frequency`/`InputClockFrequency`/etc. all returned empty replies
there): this Renode distribution's own shipped
`tests/platforms/ARM_Cortex-R8-llext-test/cortex_r8_virtual.repl` sets `frequency: 5000000` on
its own `ttc0` (`Timers.Cadence_TTC`) instance, found by scanning every `.repl` file under the
bundle for `Cadence_TTC` -- direct evidence from this build's own shipped content, not
inference.

**Gate test:** `third_party/renode/M24_4/ttc_rate_test.py` +
`third_party/renode/M24_4/ttc_rate_test.resc`. Loads the *existing* M24.2 `ticker.exe`
artifact (`third_party/rtems-container/run-renode/ticker.exe`, reused read-only -- `ticker`,
not `hello`, because `hello` disables the clock driver and never programs TTC0's `CNT_CNTRL`,
so it would have nothing to measure) on `rpu0` against **our own** platform file, then for 5
steps of `emulation RunFor "1.0"` reads `TTC0.COUNT_VALUE` (`sysbus ReadDoubleWord
0xff110018`) directly off the bus and compares `COUNT_VALUE / 100e6` against Renode's own
reported `currentTime`, exactly M24.2d's finding-5 methodology (last-hex-token parsing, not
the first-token bug that finding's v2 diagnostic had).

**Measured** (driven via the Monitor TCP protocol, `renode --disable-gui --hide-log -P <port>`,
never the CLI-positional-.resc mode):

| step | virtual time (s) | TTC0.COUNT_VALUE | implied TTC seconds @100MHz | ratio |
|---|---|---|---|---|
| 1 | 1.0 | 99,980,000 | 0.99980 | 0.999800 |
| 2 | 2.0 | 199,980,000 | 1.99980 | 0.999900 |
| 3 | 3.0 | 299,980,000 | 2.99980 | 0.999933 |
| 4 | 4.0 | 399,980,000 | 3.99980 | 0.999950 |
| 5 | 5.0 | 499,980,000 | 4.99980 | 0.999960 |

Worst `|ratio - 1|` = **2.0e-4**, inside the required 1e-3 tolerance -- **PASS**
(`ttc_rate_test_result.json`, `passed: true`). The ratio converges toward 1.0 as the run
proceeds (0.9998 -> 0.99996) because `COUNT_VALUE` carries a small, constant 20,000-count
(0.2 ms) offset from the driver's own startup latency before it first enables `CNT_CNTRL`,
which becomes proportionally smaller against a larger cumulative virtual time -- consistent
with a fixed one-time offset, not a residual rate error. This is a **~5000x** improvement
over M24.2d's measured ~1/3 (0.332-0.333) ratio against the shipped file, using the identical
BSP/firmware/measurement method and differing only in which platform file is loaded.

**Item 1: PASSED. Everything below depends on this.**

## Item 2 -- BSP_RESET_BOARD_AT_EXIT and the WFI-idle test

**Where the option lives:** `BSP_RESET_BOARD_AT_EXIT` is an RTEMS *BSP-build* option
(`spec/build/bsps/optreset.yml`, default on), baked into the compiled `bspfatal-default.o`
inside the installed BSP library at BSP-build time -- not an application/cFS-link-time
setting. Changing it means rebuilding the BSP (`./waf configure --rtems-config=<override>`),
not just relinking an app.

**Isolation from the shared toolchain (a real concern, investigated, not ignored):**
`docker ps -a` at the start of this task showed two *already-running* `rtems-m24c:build`
containers (`eloquent_lalande`, `vibrant_moore`, up 14 and 26 minutes) executing
`/work/build.sh`, i.e. some other process was actively building against the shared
`third_party/rtems-container` tree at the same time this task started. Per this task's own
scope (no instruction to touch `third_party/rtems-container`'s shared `output/`, and every
reason not to race a concurrent write to it), this task's BSP rebuild is **fully isolated**:
a fresh container run, reusing the *existing, already-verified* toolchain and RTEMS source
read-only (`-v .../output/toolchain:/toolchain:ro`, `-v .../work/rtems-src:/work/rtems-src:ro`),
copying the source into the container's own filesystem, and installing into a brand-new
prefix under `third_party/renode/M24_4/rtems-bsp-noreset/myout/` -- the shared
`third_party/rtems-container/output/toolchain` (which M24.3's `core-cpu1.exe` was linked
against) is never written to. Those two containers were left alone throughout (not stopped,
not inspected further) since they were not created by this task and their purpose was not
established with confidence. Every container this task itself created carries
`--label av-m24-4-run=...` (question 156, see item 5 below).

**Override file:** `third_party/renode/M24_4/rtems-bsp-noreset/config-noreset.ini`
(`[arm/zynqmp_rpu_lock_step]` / `BSP_RESET_BOARD_AT_EXIT = 0`), passed via
`./waf configure --rtems-config=<file>` (confirmed as the real mechanism by running
`./waf configure --help` inside the container first, not assumed: "a comma-separated list of
paths to the BSP configuration option files [default: 'config.ini']"). Numeric `0`/`1` used
to match the exact format the BSP's own `bspdefaults` dump already showed for this option
(`BSP_RESET_BOARD_AT_EXIT = 1`), not the `True`/`False` style some other options use.

**Rebuild:** `third_party/renode/M24_4/rtems-bsp-noreset/build_noreset_bsp.sh`, run inside
`rtems-m24c:build` (~1 minute BSP-only build, per the earlier `bsp-build.log`'s own
"finished successfully (1m3.064s)" once the toolchain already exists -- confirmed reproducible
here too). Produces fresh `hello.exe`/`ticker.exe`/etc. samples with the option off.
**Disassembly check (corroborating, not the decisive test):** `arm-rtems6-nm -t` /
`arm-rtems6-objdump -t` against the new `ticker.exe` finds `bsp_fatal_extension` but **no
`bsp_reset` symbol at all** (present as a real global symbol in every prior build of this
BSP) -- consistent with the option's `#if` guard removing the only call site and the
function going unreferenced.

**Decisive test:** `third_party/renode/M24_4/wfi_idle_test.py` +
`third_party/renode/M24_4/wfi_idle_test.resc`. One continuous Renode instance (never
restarted between phases, so boot cost is shared and not a confound), our platform file
(item 1) plus the fresh no-reset `ticker.exe`:

1. **RUNNING phase** -- 5 x `RunFor("2")` while `ticker` is actively ticking (nominal virtual
   0-10s).
2. Coarse, unpolled advance (`AdvanceImmediately` on) to nominal virtual ~50s.
3. UART content checked against the **real captured banner text**, `"END OF TEST CLOCK
   TICK"` -- not `"END OF CLOCK TICK TEST"`, the exact substitution question 164 itself warns
   about and which this task's own M24.2d predecessor tooling got wrong. **Seen: true.**
4. **IDLE phase** -- 5 x `RunFor("2")` past completion, reading `rpu0 PC` and
   `rpu0 ExecutedInstructions` at every step.

**Result** (`wfi_idle_test_result.json`):

| phase | wall s/step (mean) | PC at every sample |
|---|---|---|
| running (ticker active) | 1.548 s wall per virtual s | (not sampled, ticker still executing real work) |
| idle (past completion) | 0.588 s wall per virtual s | `0x400045e6` every sample, **never** `0x4000706c` |

- **PC check: PASS.** Every idle-phase sample reads exactly `0x400045e6` -- the identical
  idle-loop address M24_2d_REPORT.md's own disassembly identified
  (`_CPU_Thread_Idle_body`'s `wfi`+`b.n`) -- and never the old busy-spin address
  `0x4000706c` that same report caught the *unfixed* build landing on and never leaving.
- **Wall-time check: PASS, with a large margin in the right direction.** Idle costs **0.588
  s wall per virtual second, ~2.6x *less*** than the running phase's 1.548 s/s (WFI is
  fast-forwarded by Renode; active ticking does real emulated work) -- not merely "no more,"
  materially less. This is the direct, opposite-in-kind result from M24_2d_REPORT.md's
  measurement of the *unfixed* build, where the post-completion window cost up to **329x
  more** wall time than an identically-sized preceding step.
- `rpu0 ExecutedInstructions` was also sampled every idle step but this test's own hex-vs-decimal
  parsing for that specific monitor reply is not trusted (it read `0` at every sample, which
  is not independently confirmed correct here and is not needed for the pass condition --
  the PC and wall-time evidence above are each independently sufficient and are what this
  test's pass condition actually checks).

**Item 2: PASSED.** `passed: true` in the result file (`completion_banner_seen: true`,
`pc_check_passed: true`, `rate_check_passed: true`).

**What is NOT yet done for item 2:** this fix has only been proven at the BSP-library level
(RTEMS's own `ticker`/`hello` samples, which share the exact same `bsp_fatal_extension`/
`bsp_reset` code path cFS links against -- the defect lives in the BSP library, not in any
app). **`core-cpu1.exe` (M24.3's cFS artifact) has not yet been relinked against this fixed
BSP prefix.** That relink is mechanical (M24.3's own cross-build script, pointed at
`third_party/renode/M24_4/rtems-bsp-noreset/myout/prefix` instead of the shared
`third_party/rtems-container/output/toolchain`) but was deferred in favor of items 3-5 given
the ~300-tool-use budget; see the "not done" list.

## Item 5 -- question 156 housekeeping: the label-and-prune guard

**Investigated first (a real concurrency finding, not assumed away):** `docker ps -a` at the
start of this task showed two containers already running against the shared
`third_party/rtems-container` build tree (see item 2's own "Isolation from the shared
toolchain" note) -- independent evidence that this exact kind of long-running orphan is a live
risk on this host, consistent with the amendment's own incident report.

**Existing pattern (surveyed before changing anything):** `crates/av-lockstep/src/docker.rs`'s
`ManagedContainer` already has a best-effort `Drop` (stop + remove), and both
`crates/av-lockstep/tests/docker_lifecycle.rs` and `crates/av-kernel/tests/drm_container.rs`
each define their own parallel `ContainerGuard`/`ImageGuard` (`Drop`-based) around a throwaway
`registry:2` container and a built/tagged/pushed image. **No `--label` usage existed anywhere
in the repository before this task**, and no test asserted "no image tagged with its registry
port remains" (the original question 156 requirement) -- `drm_container.rs` only asserted no
*container* remained.

**What changed:**

- `crates/av-lockstep/src/docker.rs` (production code, reusable by any test): added
  `TEST_LABEL_KEY`/`TEST_LABEL_VALUE` (a standing marker, not tied to any one run --
  `av.test=1`), `test_run_id()` (a per-process `pid-nanos` value for the amendment's "a label
  with the run id"), `test_label_args(run_id)` (the `--label` argv for a test's own direct
  `docker` calls), and `prune_stale_test_resources()` (removes every container and image
  carrying `TEST_LABEL_KEY`, by `docker ps -aq --filter label=av.test` / `docker images -q
  --filter label=av.test`, then `rm -f`/`rmi -f`). The prune filters on the **key**, not a
  specific run's value -- deliberately, so it sweeps up *every* prior run's leftovers, not
  only the current process's own.
- `ManagedContainer::pull_and_run`/`build_run_args` gained an additive `extra_labels`
  parameter (same `BTreeMap`-sorted-by-key pattern as the existing `extra_sysctls`, M23.4) so
  a test can label the container `pull_and_run` itself creates, not just containers a test
  starts directly. The one production call site
  (`crates/av-kernel/src/drm/binding.rs:1044`, the kernel's own `BINDING_KIND_CONTAINER`
  path) passes an empty map -- production runs are not tests and get no marker label.
- Both test files: call `prune_stale_test_resources()` **before** creating anything (the
  amendment's own ask: pruning only at teardown cannot help when a `Drop` guard never got to
  run), and attach `test_label_args(&run_id)` to every direct `docker build`/`docker run`
  call, plus the derived labels to their `ManagedContainer::pull_and_run` call.
- **A same-binary concurrency hazard, found and fixed while writing this, not merely
  theoretical:** `cargo test` runs every `#[test]` in one file concurrently by default. Adding
  a second Docker-using test to `docker_lifecycle.rs` means its `prune_stale_test_resources()`
  call could race the existing lifecycle test and delete a container or image that test is
  still using mid-run. Fixed with a `static DOCKER_TEST_LOCK: std::sync::Mutex<()>`, held for
  each test's full duration -- the two tests never interleave regardless of `cargo test`'s
  default threading. `drm_container.rs` has only one Docker-using test today, so no lock was
  added there; noted in a comment for the next one added.
- **New test**, `crates/av-lockstep/tests/docker_lifecycle.rs::
  prune_stale_test_resources_removes_orphaned_labeled_containers_and_images`: stands up a
  labeled throwaway registry container and a labeled, port-tagged, pushed image
  **deliberately with no `Drop`-guard teardown at all** (simulating a killed test process),
  asserts both exist, calls `prune_stale_test_resources()`, then asserts (a) no container
  carrying the label remains, (b) no image carrying the label remains, and (c) --
  question 156's own original wording -- `docker image inspect` on the specific
  `127.0.0.1:<port>/...`-tagged reference fails, i.e. nothing tagged with this test's registry
  port remains.
- Two small unit tests added to `docker.rs` itself (`labels_are_emitted_sorted_by_key_...`,
  `test_label_args_shape`) pinning the exact argv shape, following this file's own existing
  pattern for `extra_sysctls`.

**Verification (cargo run, disclosed per the task's own rule -- `cargo` itself was not on
`PATH`; found and used directly at `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`):**
`cargo check -p av-lockstep -p av-kernel --tests` -- clean, no errors or warnings, ~46s.
`cargo test -p av-lockstep --test docker_lifecycle -- --test-threads=1 --nocapture`, against
the real local Docker daemon: **`test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured;
0 filtered out; finished in 100.44s`** -- both the original lifecycle test and the new guard
test pass. `crates/av-kernel/tests/drm_container.rs`'s equivalent (now also labeled/pruned)
was not additionally re-run given the ~300-tool-use budget; it is a lighter-weight,
already-passing-before-this-change test exercising the same `pull_and_run` path this task's
`cargo check` already confirmed still compiles and whose only change is additive labels.

**Item 5: DONE**, verified by a real, passing test against a real Docker daemon, not by
inspection alone.

## GIC `ISENABLER1=0` paragraph (required, one paragraph)

M24_2d_REPORT.md's diagnostics (`diag_ttc_trace3.py`, `diag_gic_check2.py`) read `rpuGic`'s
distributor `ISENABLER1` register (offset `0x104`, SPI 32-63 enable-set; bit 4 = IRQ36, ttc0
channel 0) via `sysbus ReadDoubleWord 0xf9000104 rpu0` and consistently got back `0x00000000`,
both while ticks were actively arriving and long after -- on its face suggesting IRQ36 (the
TTC0 match interrupt the RTEMS clock driver depends on) was never enabled at the GIC
distributor. That task flagged this as **unresolved, not evidence either way**, and this task
agrees and does not resolve it further, for the same reason that task gave: neither this task
nor M24.2d independently verified the assumed register offset against Renode's own GIC
implementation (this distribution ships no C# source, only compiled binaries, so the offset
used is the generic ARM GICv1 architecture reference, not a confirmed value read out of
Renode's own model code). It is believed irrelevant to every finding in this report because
none of item 1's or item 2's evidence depends on it: item 1's TTC-rate result rests on
`TTC0.COUNT_VALUE` read directly off the bus compared against Renode's own `currentTime`
(neither of which touches the GIC), and item 2's WFI-idle result rests on UART content, `rpu0
PC`, and driver-measured wall time (also independent of GIC register state) -- and in both
cases the RTEMS clock driver visibly, correctly delivered periodic ticks for tens of virtual
seconds in the same runs where `ISENABLER1` read as zero, which is itself evidence that
*interrupts were being delivered and taken* by whatever mechanism actually governs this, even
though this task cannot say why the one specific register it read disagreed with that. A53/
QEMU isolation to chase this further is explicitly not required by this task's brief and was
not attempted.

## Item 3 -- the bridge

### First-ever boot of the M24.3 cFS artifact under Renode (major finding, unplanned but load-bearing)

Before building the bridge itself, this task tried the obvious smoke test: does
`third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe` (M24.3's own words: "The RTEMS
binary has never been executed") even boot under Renode with item 1's fixed platform file?
`third_party/renode/M24_4/cfs_boot_smoke_test.resc` + `.py` load it on `rpu0` exactly like the
`ticker`/`hello` samples, capture UART0 (cFE's debug console, per M24.3's own UART0/UART1
split), and run 6 x 5-virtual-second chunks.

**Result: cFE core boots correctly and completely, for the first time.** The full captured
UART0 transcript (`cfs_boot_uart0.log`) shows RTEMS's own boot banner, the RTEMS shell
initializing, then cFE's own startup sequence running cleanly through every core module in
the expected order: `CFE_PSP_AttachExceptions`, `CORE_STARTUP`, `EarlyInit` for `CFE_CONFIG`,
`CFE_ES`, `CFE_EVS`, `CFE_FS`, `CFE_SB`, `CFE_TBL`, `CFE_TIME` (each logging its own real
version string, e.g. "cFE ES Initialized: CFE_ES v7.0.1+dev0 (Draco)"), then
`CFE_ES_Main entering CORE_READY state`, then **`CFE_ES_Main entering OPERATIONAL state`**.
This is real, substantial RTEMS/cFE boot code executing correctly on this platform --
memory init, the PSP's reserved-memory allocation, exception attachment, the whole cFE core
module chain -- not a trivial sample.

**Root-caused blocker for starting the three lockstep apps (IO_LOCKSTEP/SCH_LOCKSTEP/ADCS):**
immediately after `CORE_READY`, the log shows:
```
CFE_ES_StartApplications: Error, Can't Open ES App Startup file: /cf/cfe_es_startup.scr, EC = -1
```
followed by `APPS_INIT` then `OPERATIONAL` anyway (cFE tolerates a missing startup script and
runs with core services only). This is a filesystem-mount gap, not an app-loading defect: the
three apps ARE linked into the binary (M24.3's own verified `nm` symbols,
`IO_LOCKSTEP_AppMain`/`SCH_LS_AppMain`/`ADCS_AppMain`), but `CFE_ES_StartApplications` reads
its app list from a *file* at `/cf/cfe_es_startup.scr`, and this raw `LoadELF` boot has no
filesystem backing that path at all -- `EC = -1` is a plain "could not open," consistent with
no volume ever having been mounted there. **This is now a specific, understood, and
apparently small gap** (get one text file readable at `/cf/cfe_es_startup.scr` at boot,
likely via RTEMS's default in-memory `IMFS` root filesystem plus a directory-create-and-write
boot hook, rather than a real disk/tarfs mechanism), not an open-ended porting problem --
**and it was in fact fixed within this same task; see "The `/cf` filesystem gap" section
below for the mechanism and the fix, and "Verification: SUCCESS" for the live result.**

### Bridge design (Renode-side UART wiring + the relay process)

Confirmed against this exact Renode build's own shipped content (not assumed):
`scripts/complex/hci_uart/hci_uart.resc` uses
```
emulation CreateServerSocketTerminal $port "hci" false
connector Connect sysbus.uart1 hci
```
to back a UART peripheral with a raw TCP server socket -- an external process that connects
to that TCP port gets a byte-for-byte pipe to the UART's RX/TX, exactly the transport surface
`io_lockstep`'s `/dev/ttyS1` (M24.3's chosen bridge transport) needs on the Renode side.

**Architecture (question 153's own words -- "the shim is reused unchanged"):**
`crates/av-lockstep-shim`'s binary is **not modified**. It still listens on a Unix socket and
still speaks lockstep-local v1 exactly as it does for the container binding. The bridge is a
new, separate process that:
1. Owns the Renode process (launch, `include` the platform+ELF `.resc`, `emulation
   CreateServerSocketTerminal <port> "uart1_bridge" false` + `connector Connect sysbus.uart1
   uart1_bridge`).
2. Connects as a TCP client to that terminal port (the Renode-side byte stream) **and** as a
   Unix-domain client to the shim's listening socket (the shim's "flight-software peer" --
   from the shim's perspective the bridge process itself *is* the peer).
3. Relays bytes between the two connections. Because `io_lockstep_app.c`'s
   `lockstep_local_framing.c`/`lockstep_local_io.c` already implement lockstep-local v1
   directly over the UART fd (M24.3: "reused completely unchanged... they only ever see an
   `int fd`"), **the bytes on both sides of this relay are already valid lockstep-local v1
   frames** -- the bridge does not need to re-encode or decode `PortMessage`/CCSDS payloads
   itself.
4. The one place the bridge is *not* a dumb byte-splicer: it must slave Renode's virtual time
   to the kernel's own step cadence (`docs/sil-plan.md` M24: "slaves its virtual time to the
   kernel through Renode's external interface"). Since the STEP frame's `until_tai_ns` is what
   tells a peer how far to advance, the bridge peeks at each `STEP` frame's header
   (`frame_type = 0x04`) as it passes through and issues the corresponding
   `emulation RunFor "<until - current>"` (M24.1's proven method) on Renode's monitor port
   before letting the frame's bytes reach Renode's UART -- everything else in the frame
   (the protobuf payload) passes through unexamined.

### The `/cf` filesystem gap: root-caused precisely, and a fix attempted (this task's first-ever PSP patch)

A background research pass (read-only, into `third_party/rtems-container/work/rtems-src` and
`third_party/cfs`, not guessed) found the exact mechanism and the exact gap:

- RTEMS's default root filesystem is **always IMFS** (in-memory), unconditionally
  (`cpukit/include/rtems/confdefs/libio.h`'s `rtems_filesystem_table[]`) -- no tarfs/bin2c
  embedding is needed for one small text file.
- `third_party/cfs/psp/fsw/pc-rtems/src/cfe_psp_start.c`'s `CFE_PSP_Main()` already calls
  `OS_FileSysAddFixedMap(&fs_id, "/mnt/eeprom", "/cf")`, which (via OSAL's RTEMS
  `os-impl-filesys.c`) creates `/mnt/eeprom` as a plain directory in that same default IMFS
  and aliases `/cf` to it -- **the directory exists after this call returns; nothing has ever
  written a file into it.**
- `services/cfs/build/generate_startup.cmake` writes the real `cfe_es_startup.scr` at CMake
  **install** time, to a path on the *build host's own filesystem*
  (`third_party/cfs/build-rtems_zynqmp/exe/cpu1/eeprom/cfe_es_startup.scr`, confirmed present
  on disk) -- a file that exists next to `core-cpu1.exe` on this Mac, but is invisible to the
  guest, which only ever receives the bare ELF via `sysbus LoadELF`. No disk/network image is
  attached in this boot path at all.
- `CFE_ES_StartApplications` (`cfe/modules/es/fsw/src/cfe_es_apps.c`) opens
  `CFE_PSP_NONVOL_STARTUP_FILE` (`/cf/cfe_es_startup.scr`) via plain `OS_OpenCreate`/`OS_read`
  -- it does not care how that file got there.

**Fix applied:** `third_party/cfs/psp/fsw/pc-rtems/src/cfe_psp_start.c`, `CFE_PSP_Main()`,
immediately after the `OS_FileSysAddFixedMap` call -- writes the exact three startup-script
lines (copied byte-for-byte from the real generated file via `tr ' ' '.'` to confirm every
space count before transcribing, not retyped from memory) into `/cf/cfe_es_startup.scr` using
plain `OS_OpenCreate`/`OS_write`/`OS_close`, guarded to skip writing (never overwrite) if the
file can already be opened for reading -- so a future disk-backed target that legitimately has
its own `/cf/cfe_es_startup.scr` is untouched.

**Question 148 accounting: this is this task's first-ever patch to `third_party/cfs/{cfe,osal,psp}`**,
ending the "zero patches" record every prior M24 task (M24.2, M24.2c, M24.2d, M24.3) held. It
touches only `psp/fsw/pc-rtems/src/cfe_psp_start.c`, one function, additive (a new block, no
existing line changed), guarded against affecting any real-hardware/real-disk target. No
upstream RTEMS or cFS issue is cited for it -- this is not a defect in RTEMS or cFE, it is a
missing initial-condition specific to a raw-ELF/no-disk emulator boot, which no other target
this pinned commit builds for exercises.

**Persistence note (important, and a real gap):** `third_party/fetch-cfs.sh`'s own header says
"Nothing under `third_party/cfs` is committed" -- it is fetched fresh from a pinned upstream
commit on demand and is not version-controlled by this repository. This means the in-place
edit to `cfe_psp_start.c` **will be silently lost the next time `fetch-cfs.sh` re-fetches**
(a fresh clone, or any environment that starts clean), unlike M24.3's own zero-patch
achievement which needed no such preservation step. This task recorded the change as a real
patch file, `third_party/renode/M24_4/patches/cfe_psp_start-cf-startup-file.patch`, verified
to apply cleanly with `patch -p1` against a reconstructed pre-patch copy of the file (checked
in this task, not assumed) -- but **nothing yet re-applies it automatically** after a fetch
(unlike `third_party/rtems-container/patches/`'s own two RSB patches, which that tree's build
scripts apply explicitly). A follow-up should wire this into `third_party/fetch-cfs.sh` or
`build-cfs-cross.sh` the same way, or the fix will need to be redone by hand on a fresh clone.

**Verification: SUCCESS, confirmed live.** `build-cfs-cross.sh` rebuilt `core-cpu1.exe`
(new SHA-256 `07c65280c58154b44c34ba0c1f2a9d47ea86d32338eca0f4d2911a3b3715e96a`, verified by
`file`/`readelf` as before, per this task's own "exit code is not evidence" rule) and, booted
under Renode with `cfs_boot_smoke_test.resc`/`.py`, the UART0 log now shows, in order:
```
CFE_PSP: AltaVista M24.4 wrote a default /cf/cfe_es_startup.scr into the in-memory root fs (Renode/no-disk boot)
...
1980-012-14:03:20.49160 CFE_ES_StartApplications: Opened ES App Startup file: /cf/cfe_es_startup.scr
1980-012-14:03:20.49320 CFE_ES_ParseFileEntry: Loading file: /cf/io_lockstep.obj, APP: IO_LOCKSTEP
1980-012-14:03:20.53919 CFE_ES_ParseFileEntry: Loading file: /cf/sch_lockstep.obj, APP: SCH_LOCKSTEP
1980-012-14:03:20.54099 CFE_ES_ParseFileEntry: Loading file: /cf/adcs.obj, APP: ADCS
EVS Port1 ... 65/1/IO_LOCKSTEP 4: IO_LOCKSTEP: lockstep-local handshake failed
1980-012-14:03:20.59270 CFE_ES_ExitApp: Application IO_LOCKSTEP called CFE_ES_ExitApp
EVS Port1 ... 65/1/SCH_LOCKSTEP 4: SCH_LOCKSTEP: lockstep timebase armed
EVS Port1 ... 65/1/ADCS 0: ADCS: initialized, kp=0.250000 kd=5.000000
1980-012-14:03:20.64140 CFE_ES_Main: CFE_ES_Main entering OPERATIONAL state
```
**All three lockstep apps are genuinely invoked** (not merely linked -- `SCH_LOCKSTEP`'s and
`ADCS`'s own real init log lines fire, with `ADCS`'s `kp=0.250000 kd=5.000000` matching the
exact gains `services/cfs/apps/adcs/fsw/src/adcs_control.c`'s defaults declare). `IO_LOCKSTEP`
correctly detects that nothing answered its handshake attempt on `/dev/ttyS1` (nothing was
wired to `uart1` in this particular boot -- no bridge yet) and **exits cleanly through
`CFE_ES_ExitApp` rather than hanging or crashing** -- itself a good sign for a real bridge
attempt next. cFE still reaches `OPERATIONAL`. **Item 3's boot blocker is resolved.**

### Attempting the real handshake against a minimal test peer (partial result, root cause not fully chased)

With the `/cf` fix confirmed, this task went one step further than originally planned: wired
`uart1` to the same `CreateServerSocketTerminal` mechanism proven live earlier
(`cfs_handshake_test.resc`) and wrote a minimal Python "fake peer"
(`cfs_handshake_test.py`) that connects to that TCP port and sends a real, correctly-framed
lockstep-local v1 `HELLO` (`0x07 0x00 0x00 0x00 0x01 A V L 1 0x01 0x00` -- length=7, type=
`HELLO`, magic `AVL1`, version 1, matching `services/cfs/README.md`'s spec byte-for-byte),
both immediately on connect and repeatedly through 20 fine-grained (`RunFor "0.05"`) steps
spanning the exact window `IO_LOCKSTEP`'s handshake attempt happens in.

**Result: the handshake still failed, and the guest never sent any bytes back on the bridge
port at all (0 bytes received), even with the peer connected before boot and resending HELLO
throughout.** A second attempt strengthened this rather than resolving it: the peer resent its
`HELLO` 21 times total (on connect, then once per `RunFor "0.05"` step across 20 consecutive
steps spanning the exact window `IO_LOCKSTEP`'s own handshake-failure log line appears in) --
**still zero bytes back, identical "handshake failed" outcome.** This rules out simple
connect-timing (too early/too late relative to the guest's own attempt) as the explanation,
since the peer's `HELLO` was present and freshly resent throughout the entire window, and
points toward a more structural gap: either Renode's `Cadence_UART`/`CreateServerSocketTerminal`
path is not actually delivering bytes written from the TCP side into the guest's receive FIFO
at all (not merely "delivered late"), or `io_lockstep_app.c`'s UART-branch handshake never
actually reads from the fd the way the Unix-socket branch does. This was not chased to its own
root cause within this task's remaining budget --
the UART1 transport was already flagged by M24.3 as "an unverified design choice, not a
working, tested implementation," and this is now a concrete, reproducible failure of that
specific mechanism, with several plausible mechanisms this task did not have budget to
distinguish between: (a) `io_lockstep_app.c`'s UART-branch handshake read may be a single
non-blocking/short-timeout attempt rather than a retrying one (M24.3 built this branch as
"exactly one `open()` call" and never ran it before this task); (b) Renode's `Cadence_UART`
model or the `CreateServerSocketTerminal`/`connector Connect` wiring may need the guest's own
UART driver to complete a specific initialization sequence (baud/parity/FIFO enable) before
bytes written from the TCP side are actually delivered into the peripheral's receive path,
and this task did not instrument the UART's own registers to check; (c) the RTEMS termios
layer over `/dev/ttyS1` may buffer or require a specific line discipline this raw framed
byte-protocol was not written expecting. **This is the single most important concrete "not
done" item for a follow-up task**, with everything needed to reproduce it in place:
`cfs_handshake_test.resc`/`.py`, the fixed `core-cpu1.exe`, and this exact finding.

### The UART1/TCP wiring mechanism itself, proven live

**What this task built and verified of the bridge design above:** step 1 (the UART1-to-TCP-terminal wiring)
is not just confirmed as *valid syntax* against this Renode build's shipped example -- it was
**actually run and proven live**, against our own platform file and the real cFS ELF:
`third_party/renode/M24_4/uart1_bridge_wiring_test.resc` + `.py` set a monitor variable
(`$bridgeport = <port>`), `include` a `.resc` that does
```
emulation CreateServerSocketTerminal $bridgeport "uart1_bridge" false
connector Connect sysbus.uart1 uart1_bridge
```
against `core-cpu1.exe` loaded on `rpu0`, run 2 virtual seconds, then open an independent,
ordinary TCP client socket to that port from this task's own driver process (not Renode's own
monitor connection) -- **the connection was accepted cleanly** ("BRIDGE PORT 15009: CONNECTION
ACCEPTED"), with no bytes yet (expected: no app is driving `/dev/ttyS1` until the `/cf`
filesystem gap above is closed and `io_lockstep` actually starts). This is the exact mechanism
step 2 of the bridge design needs (an external process reaching Renode's UART1 byte stream
over plain TCP) and it works.

Steps 2-4 (the bridge process itself: dialing both this TCP port and the shim's Unix socket,
relaying bytes, and peeking `STEP` frames to drive `RunFor`) are designed and documented above
but **not yet implemented as running code**. The `/cf` filesystem gap that originally blocked
this (see the next section) was found and fixed within this same task, and `IO_LOCKSTEP` was
confirmed to actually run and attempt its handshake -- but that handshake attempt itself does
not yet succeed against a minimal test peer either (see "Attempting the real handshake"
below), so there is still no live, working lockstep-local exchange over `/dev/ttyS1` to build
the relay's `RunFor`-peeking logic against and prove correct. Building that logic before the
underlying transport is confirmed working end to end would be untested code layered on an
untested mechanism -- this task prioritized chasing the transport itself as far as budget
allowed instead. See "not done" for the precise next step.

## Item 4 -- identical port traffic and the HARDWARE fault: plan, not yet executable

**Status: blocked on item 3's live handshake, not attempted end to end.** Both halves of item
4 need a working Renode-side `io_lockstep` connection, which does not exist yet (previous
section). What follows is the concrete plan against real, already-existing pieces of this
repository, not a speculative sketch:

**What already exists and is directly reusable:**
- `crates/av-kernel/tests/drm_attitude_control_cfs.rs` (M23.4) already runs
  `drms/demo_attitude_control_controller_cfs.system.yaml` (the posix-container-bound
  `services/cfs/apps/adcs`) through `execute()` and reads back `RunProducts` -- this is the
  posix side of the "posix container vs Renode" comparison question 145/147 ask for, already
  built and (per M23.4/M24.3) passing.
- `av_kernel::drm::binding`'s `BindingKind::Container` accepts a bare `container.address`
  (no `image`/`image_digest`) for a process that is not a Docker container at all -- this is
  the pre-M15.3 path M15.3's own Docker image lifecycle was added *alongside*, still present
  and exercised by other fixtures. A Renode-bound instance needs exactly this: a new
  `drms/demo_attitude_control_controller_renode.system.yaml` (a small, mechanical variant of
  the `_cfs` one, `container.address` pointed at wherever the bridge's fronting
  `av-lockstep-shim` publishes its gRPC port) -- **not created in this task**, deliberately:
  authoring it now, before a live bridge exists to bind against, would be an unvalidated
  fixture nobody could run, and this repository's own discipline (every other `drms/*.yaml`
  file was authored alongside a passing test that loads it) argued against adding one that
  cannot be exercised yet.
- `RunProducts.trajectories`/`provenance` already carry everything a byte-level per-step
  port-traffic comparison needs (the same mechanism M23.4's `drm_attitude_control_cfs.rs`
  test already uses to assert determinism across two container runs) -- no new kernel-side
  plumbing is needed for the comparison itself, only a second, Renode-bound run to compare
  against.

**What is missing, in the order it must be built:**
1. Root-cause and fix the handshake failure (previous section's "not done").
2. Implement the bridge process itself (steps 2-4 of the design above): a small Rust or
   Python process dialing both the shim's Unix socket and Renode's UART1 TCP terminal,
   relaying bytes, and peeking `STEP` frame headers to drive `RunFor`.
3. Author `drms/demo_attitude_control_controller_renode.system.yaml` (mechanical, from the
   `_cfs` variant) and a `crates/av-kernel/tests/drm_attitude_control_renode.rs` paralleling
   `drm_attitude_control_cfs.rs`, once 1-2 let it actually bind and step.
4. The byte-comparison itself: run the identical DRM through both bindings and assert the
   captured port traffic (`PortMessage` bytes per step, or the resulting `Trajectory` samples,
   whichever this repository's existing comparison idiom uses -- `drm_attitude_control_cfs.rs`
   already establishes the pattern) is byte-identical, with RTEMS's own internal task order
   recorded (via provenance/events, already wired for every other binding kind) but not
   asserted (question 145's own decision).
5. The HARDWARE fault: `docs/open-questions.md` question 120 already wires
   `FAULT_TARGET_KIND_HARDWARE` to a power-cycle fault for containers; this task's own
   `hw_reset_fault_test.py` (below) independently confirms the Renode-side half of that
   mechanism (a monitor `machine Reset` genuinely reboots cFE, observed via a second complete
   boot sequence in the UART log) works on this exact platform+ELF -- what remains is wiring
   a bridge's `Reset` RPC handler to issue that same monitor command, which is a small,
   well-understood addition once step 2 (the bridge) exists.

### The HARDWARE fault mechanism, tested standalone against the working boot (real evidence, not a plan)

Independent of the bridge, this task tested whether Renode's own reset mechanism is usable at
all for a `FAULT_TARGET_KIND_HARDWARE` power-cycle fault: `hw_reset_fault_test.py` boots
`core-cpu1.exe` to `OPERATIONAL` (10 virtual seconds, per the now-established boot timing),
issues `machine Reset` through the monitor, runs 10 more virtual seconds, and checks the UART0
log (which the file backend only ever appends to, never truncates) for a **second**, complete
boot sequence.

**Result: PASS, confirmed by content.** UART0 grew from 5,255 bytes (one complete boot,
through `OPERATIONAL`) to 10,510 bytes after `machine Reset` plus 10 more virtual seconds --
almost exactly double, and the log's own text confirms why: `CFE_ES_SetupResetVariables:
POWER ON RESET due to Power Cycle` appears **twice**, and `CFE_ES_Main entering OPERATIONAL
state` appears **twice**, i.e. cFE genuinely re-ran its entire boot sequence from scratch a
second time, in the same running Renode process, triggered by nothing but a monitor
`machine Reset` command. The file backend only ever appends (`CreateFileBackend ... true`),
so this is not the same boot text read twice -- it is direct evidence of a real second boot.
This confirms the Renode-side mechanism a bridge's `Reset` RPC handler would need
(`docs/open-questions.md` question 120: `Reset` wired to a `FAULT_TARGET_KIND_HARDWARE`
power-cycle fault) is real and already works on this exact platform file and ELF, independent
of whether the bridge process itself exists yet to drive it automatically.

## Final status and complete "not done" list

**status: partial.** Items 1, 2, and 5 are DONE and verified by real, passing measurements/
tests against real Renode/Docker processes, not by inspection. The GIC paragraph is done.
Item 3 made substantial, verified progress beyond what it started with (a boot blocker
root-caused and fixed with a real, documented patch; the UART1/TCP bridge mechanism proven
live) but the bridge process itself and a working end-to-end handshake are not built. Item 4
is a concrete, actionable plan plus one independently-verified piece (the HARDWARE reset
mechanism); the byte-comparison itself was not attempted, being fully dependent on item 3.

**Everything not done, in priority order for a follow-up task:**

1. **Root-cause why `IO_LOCKSTEP`'s lockstep-local handshake never receives bytes over
   `/dev/ttyS1`/`uart1`**, even with a live TCP peer connected before boot and resending
   `HELLO` continuously through the exact failure window (21 attempts, still zero bytes back).
   Reproduce with `third_party/renode/M24_4/cfs_handshake_test.resc`/`.py` against
   `third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe` (already fixed and rebuilt with
   the `/cf` patch). Suspects, in the order this task would check them next: (a) instrument
   Renode's `uart1` peripheral registers directly (the same `sysbus ReadDoubleWord` technique
   M24.2d used on the TTC) to see whether bytes written to the TCP terminal ever reach the
   UART's RX FIFO at all; (b) read `services/cfs/apps/io_lockstep/fsw/src/io_lockstep_app.c`'s
   actual UART-branch handshake code path line by line (M24.3 built it as "exactly one
   `open()` call" and this is its first real execution) for a short-timeout or single-attempt
   read that a Unix-socket peer would never trip but a UART's differently-timed byte arrival
   might; (c) check whether RTEMS's termios layer over this BSP's UART driver needs explicit
   configuration (baud/raw mode) this app never sets, defaulting to a mode that discards or
   misframes raw binary protocol bytes.
2. **Build the bridge process** (owns Renode via the proven monitor-driver pattern, dials the
   shim's Unix socket and Renode's UART1 TCP terminal, relays bytes, peeks `STEP` frame
   headers to drive `RunFor`) -- blocked on 1.
3. **Author `drms/demo_attitude_control_controller_renode.system.yaml`** (mechanical, from
   `drms/demo_attitude_control_controller_cfs.system.yaml`, `container.address` pointed at the
   bridge-fronting shim) and a paralleling `crates/av-kernel/tests/drm_attitude_control_renode.rs`
   -- blocked on 2.
4. **The byte-comparison itself** (posix container vs Renode, per-step port traffic,
   RTEMS-internal task order recorded not asserted per question 145) -- blocked on 3.
5. **Wire a bridge `Reset` RPC handler to the `machine Reset` monitor command** this task
   already proved works -- blocked on 2; the mechanism itself needs no further validation.
6. **Relink `core-cpu1.exe` against the `BSP_RESET_BOARD_AT_EXIT=0` BSP** (item 2 fixed the
   RTEMS `ticker`/`hello` samples' BSP; the actual cFS artifact still links against the
   original, reset-spins-forever BSP) -- mechanical (point M24.3's own cross-build script at
   `third_party/renode/M24_4/rtems-bsp-noreset/myout/prefix` instead of the shared toolchain),
   not attempted here given budget.
7. **Wire `third_party/renode/M24_4/patches/cfe_psp_start-cf-startup-file.patch` into
   `third_party/fetch-cfs.sh` or `build-cfs-cross.sh`** so it survives a fresh `third_party/cfs`
   fetch (currently an in-place, unpersisted edit -- see the patch's own "Persistence note").
8. The GIC `ISENABLER1=0` discrepancy (M24.2d's own finding) remains unresolved on its own
   terms, believed irrelevant for the reasons given above, not chased further (A53/QEMU
   isolation explicitly not required by this task).
9. `crates/av-kernel/tests/drm_container.rs`'s Docker lifecycle test (now also labeled/pruned)
   was not re-run against a real Docker daemon in this task (only `cargo check`'d) given
   budget; `crates/av-lockstep/tests/docker_lifecycle.rs`'s equivalent (and its new guard
   test) WAS run and passed.

**No network was used at test/run time** (the one `docker run`/`docker build`/`cargo`
invocations in this task either used already-pulled/cached images or the container's own
already-installed toolchain; the `rtems-m24c:build` image itself was built by M24.2c, not this
task). `cargo` was run, disclosed above (`cargo check`, `cargo test -p av-lockstep --test
docker_lifecycle`) -- not to build the shim (the shim binary itself was never invoked in this
task, since the bridge/handshake work never reached the point of needing it running). No files
under `third_party/rtems/`, `web/`, or `altavista/` were touched. `services/cfs/` was not
touched (confirmed: no file under it has a newer mtime than this report), so
`services/cfs/IMAGE_DIGEST.md` is unaffected and its own test should still pass unchanged.
`third_party/cfs/psp/fsw/pc-rtems/src/cfe_psp_start.c` was patched (see above, and question 148
accounting in item 3's own section) -- this task's one and only patch to
`third_party/cfs/{cfe,osal,psp}`.

# M24.2 -- RTEMS 6.1 toolchain, `zynqmp_rpu_lock_step` BSP, RTEMS samples on Renode RPU

status: complete (outcome: NO-GO on macOS host; see OUTCOME section at end)

Task: docs/sil-plan.md M24 milestone (revised milestones list: "RTEMS 6.1 build of the same
apps for `zynqmp_rpu_lock_step`... identical port traffic posix container against Renode").
docs/open-questions.md questions 144, 148, 154, 157. This worker's scope is narrower than
the full M24 milestone: build the arm-rtems6 toolchain, build the `zynqmp_rpu_lock_step` BSP,
and run RTEMS's own `hello`/`ticker` samples on Renode's mainline Zynq UltraScale+ Cortex-R5
platform with captured UART output. Cross-building cFS for this BSP is **M24.3**, out of
scope here; this report ends with a go/no-go for that next step.

Reviewable artifacts (question 157, written incrementally, measurements first): this file,
`third_party/rtems/fetch-rsb.sh`, `third_party/rtems/fetch-rtems-src.sh`, the build scripts,
and the captured `hello`/`ticker` UART output files under `third_party/rtems/`.

**This task can produce a no-go** (question 148: NASA's RTEMS 6 support is still landing
upstream). A clean "it does not build, here is exactly where" is being treated as a valid
outcome, not papered over.

## Read first (context, not re-litigated)

- Question 144: target decided by the user -- Zynq UltraScale+ RPU, Cortex-R5F lockstep,
  RTEMS 6.1 `zynqmp_rpu_lock_step` BSP variant, Renode's mainline UltraScale+ Cortex-R5
  platform; ZCU102/ZCU104 for HIL. TMS570LC4357 was the alternative (RTEMS BSP exists, no
  Renode platform).
- Question 148: cFS RTEMS 6 support still landing upstream (build without a network stack,
  RTEMS 6 test containers, Gaisler toolchain spec differences); pin a commit/release with a
  working build and report what had to be patched, with any patch's upstream issue recorded
  under `third_party/`.
- Question 154: network permitted for the fetch/build step only, exactly like
  `third_party/fetch-cfs.sh`; nothing afterwards may need it. Everything fetched recorded here.
- Question 157: this file is the reviewable artifact, written incrementally because a
  worker's final chat message might never arrive; ~300 tool uses is the expected budget.
- `docs/sil-plan.md` M24 (revised): "RTEMS 6.1 build of the same apps for
  `zynqmp_rpu_lock_step`; Renode installed under `third_party/renode`... bridge process;
  virtual-time slaving spike first; identical port traffic posix container against Renode."
- M24.1 (already done, not redone): Renode v1.16.1 macOS arm64 portable build under
  `third_party/renode`, SHA-256 `99b8ae5897b8926ef179868d39a504fe5296555dc9c9b973718ddf3ab09175d9`.
  Virtual-time slaving via Monitor `RunFor` measured exact (0 ns drift/1000 steps).
  `AdvanceImmediately` off by default (throttles near real time); on, mean 100ms step drops
  to 64.4ms wall. No linux/arm64 Renode Docker image exists on any tag. No general external
  control API in this build -- Monitor `RunFor` is the slaving mechanism. Platform script and
  bare-loop firmware live under `third_party/renode/spike/`.

## Host

macOS 26.6.2, arm64 (Apple Silicon, Darwin 25.6.0). Docker v29.6.2 available (not needed for
this task -- the toolchain build and Renode run are both native macOS). Python venv at
`/Users/probe/code/AltaVista/.venv` (Python 3.13.14); system `/usr/bin/python3` is 3.9.6.
Xcode command line tools at `/Applications/Xcode.app/Contents/Developer`.

**Host tool gaps found before starting** (expected host friction, question 148/154's brief
that this pairing is "the interesting part"): `autoconf`, `automake`, `texinfo` (`makeinfo`)
were not present on this Mac (`/usr/bin/git`, `/usr/bin/python3`, `/usr/bin/curl`,
`/usr/bin/gcc`, `/usr/bin/clang`, `/usr/bin/make`, `/usr/bin/libtool`, `/usr/bin/bison`,
`/usr/bin/flex` were present; `autoconf`/`automake`/`makeinfo`/`texi2any` were not).
Installed via `brew install autoconf automake texinfo` (network use, recorded under
"Network use" below) -- versions installed: autoconf 2.73, automake 1.18.1_1, texinfo 7.3_1
(and its dependency m4 1.4.21). `texinfo` is keg-only on macOS (the system ships an ancient
texinfo); `/opt/homebrew/opt/texinfo/bin` was added to `PATH` for the toolchain build.

## Not done / running list (kept current, updated as work proceeds)

- Toolchain build: in progress (backgrounded, third attempt with all three zlib fixes applied),
  see "Toolchain build" below.
- BSP build: not started yet (waiting on the toolchain).
- Renode run of hello/ticker: scripts written (`run-renode/hello.resc`, `run-renode/ticker.resc`,
  `run-samples.sh`), not yet executed (waiting on the BSP build).
- `.venv/bin/pytest -q`: run early as a baseline (nothing under `crates/`, `drms/`, `gmatviz/`,
  `web/` has been touched by this task) -- **355 passed, 5 warnings, 143.15s**, matching the
  stated 355-passed baseline exactly.
- `cargo test --workspace --exclude av-kernel`: in progress (backgrounded).

## Pinned sources (question 148/154: record every hash)

**RTEMS Source Builder (RSB):** tag `6.1`, commit `b1aec32059aa0e86385ff75ec01daf93713fa382`
(exact match, cross-checked with `git rev-parse HEAD` after `git checkout 6.1` -- not just the
tag name). Fetched by `third_party/rtems/fetch-rsb.sh` into `third_party/rtems/rsb` (no `.git`
kept, same convention as `fetch-cfs.sh`). `git ls-remote --tags` confirms `6.1` and `6.2` both
exist upstream; `6.1` is used because question 144 names RTEMS 6.1 specifically, not "6.x".

**RTEMS kernel source:** tag `6.1`, commit `0a46769ba42d3476b0f37a85db49b3276658d293` (exact
match). Fetched by `third_party/rtems/fetch-rtems-src.sh` into `third_party/rtems/rtems-src`
(no `.git` kept). `zynqmp_rpu_lock_step` confirmed present at this pin:
`spec/build/bsps/arm/xilinx-zynqmp-rpu/bsprpu.yml` declares
`bsp: zynqmp_rpu_lock_step`, `family: xilinx-zynqmp-rpu`, `arch: arm`.

**Toolchain package versions and hashes, resolved by RSB's own bset chain**
(`rsb/rtems/config/6/rtems-arm.bset` -> `6/rtems-default.bset` -> `6/rtems-base.bset` ->
`tools/rtems-default-tools.bset`, all at the RSB 6.1 pin above -- every one of these hashes is
RSB's own recorded SHA-512, read directly out of its `.cfg` files, not computed by this task):

| package | version | SHA-512 (from RSB's `.cfg`) |
|---|---|---|
| binutils | 2.43 | `rQBoju8+cIYoUN/YZb1LK6+Vs0M409GzrhvfhAuerA9SihyWdnRY7p0GVZ2tr8yxOqtcqud77fhEQ6VRyu8SiQ==` (base64 form as RSB stores it) |
| gcc | 13.3.0 | `7V8vTG7Sx5b88sk3BxWenb092xugY9VJgE3WjNq7ttVQmFrhyEZa6aM2z+KSdKbrD0LiGSQ2BXTr2OXVx8moAQ==` |
| gcc patch: riscv multilib | `gcc-13.3.0-RTEMS-riscv-multilib.patch` | `cb8815d0...4938491` (hex form, RSB's own recorded hash) |
| gcc patch: libstdc++ RTEMS features | `v2-0001-libstdc-v3-Enable-features-for-RTEMS.patch` | `bu6DuVp4...Ovb+RQ==` |
| newlib | commit `1b3dcfd` (`RTEMS/sourceware-mirror-newlib-cygwin`) | `VBoijuKC...SdZVgXQ==` |
| gdb | 15.2 | (from `tools/rtems-gdb-15.2.cfg`, not yet built at time of writing -- filled in once the build reaches it) |
| rtems-tools | commit `ca7bcc490ee84e65a173386a4ef5bb55635fc9d6` | `ce6uKozR...fBWg==` |
| gmp (internal) | 6.3.0 | RSB internal, standard GNU release |
| mpfr (internal) | 4.2.1 | RSB internal, standard GNU release |
| expat (internal) | 2.5.0 | RSB internal |
| dtc | 1.6.1 | **excluded from this build, see "dtc excluded" below** |
| isl | 0.24 | pulled in by gcc's config, plus RSB's own macOS-arm64 patch `fix-mac-arm64-isl-config-v2.patch` (see below) |
| mpc | 1.3.1 | pulled in by gcc's config |

**No patch is carried by this task.** The gcc/isl patches above are RSB's own pinned upstream
patches (fetched from `gitlab.rtems.org`, hash-verified by RSB itself as part of its normal
config), not something this task authored or is carrying under `third_party/`. Noteworthy:
RSB's `gcc-13.3-newlib-head.cfg` already carries `fix-mac-arm64-isl-config-v2.patch` for isl's
build on macOS arm64 -- i.e. upstream RSB has *already* anticipated this exact host/arch
combination (macOS on Apple Silicon) for the RTEMS gcc build. This is disclosed here per
question 148's "record what had to be patched" even though nothing was patched by this task,
because it directly informs the go/no-go: the toolchain side of "macOS arm64 building
arm-rtems6" is a combination RSB upstream already carries a fix for, not virgin territory.

**dtc excluded, disclosed deviation:** the default RSB tool bset
(`tools/rtems-default-tools.bset`) unconditionally includes `devel/dtc-1.6.1-1` (the device
tree compiler host tool) via `%{with_rtems_dtc}`, with no `--without-rtems-dtc` escape hatch
wired for it (unlike `rtems-tools`, which the same bset *does* support enabling/disabling via
`--with-rtems-tools`/`--without-rtems-tools`; confirmed by reading
`rsb/rtems/config/tools/rtems-tools-6.cfg` vs `rtems-default-tools.bset`, and by testing
`--without-rtems-dtc` empirically -- it does not remove dtc from the `--dry-run` plan, and
`--with-rtems-dtc=` breaks the bset parser outright with `error: ...cannot find file:`).
`dtc` failed on its first real build attempt (see "dtc build flake" below); confirmed by
grepping `rtems-src/spec/build/bsps/arm/xilinx-zynqmp-rpu/` and
`rtems-src/bsps/arm/xilinx-zynqmp-rpu/` for `dtc`/`.dts`/`.dtb` that **this BSP does not use
device-tree blobs** (no match found), so `dtc` is not required for this task's BSP or samples.
The toolchain build is run with `--keep-going` so binutils/gcc/gdb/rtems-tools proceed
regardless of dtc's outcome; if dtc is genuinely needed for some other RTEMS 6.1 BSP, that is
out of scope here and not claimed to work.

**dtc build flake, investigated (question 157's standing review point on oddities):** the
first `sb-set-builder` run failed building `dtc-1.6.1-arm64-apple-darwin25.6.0-1` with no
diagnostic beyond `shell cmd failed: /bin/sh -ex .../do-build` / `error: building
dtc-1.6.1-arm64-apple-darwin25.6.0-1` -- the captured report ends right after `CC
treesource.o` with nothing that looks like a compiler error (only benign, repeated `env:
python: No such file or directory` lines from dtc's Makefile calling a `python` executable
that doesn't exist on this host for an optional dependency-scanning step, which do not abort
`make`). Re-running the *exact same* `do-build` script by hand, in the same (not cleaned)
build directory, **exited 0** -- i.e. it was not reproduced by a bare manual rerun. Given
`do-build` runs `make` with no `-j` (sequential, ruling out a parallel-build race inside dtc's
own Makefile) and the object files already present from the first attempt let the second
run's `make` skip straight to relinking, this is most consistent with the *first* attempt
having been interrupted or having failed on a step whose own error text this task could not
recover (RSB's own report explicitly warns "the error appears only in the complete build
log" in some cases, and it did not appear there either) rather than a reproducible defect in
dtc's build. **Recorded as an unresolved, non-reproduced flake, not silently normalized as
"just works"** -- since dtc is not needed for this BSP, no further time was spent chasing it
(this task's ~300-tool-use budget is prioritized on the toolchain/BSP/Renode path that *is*
needed).

## Patch carried (question 148: record what had to be patched, with the upstream issue)

**One patch is carried**, under `third_party/rtems/patches/`:
`rsb-binutils-with-system-zlib.patch` and `rsb-gdb-with-system-zlib.patch`, each adding
`--with-system-zlib` to the binutils and gdb configure invocations in RSB's
`source-builder/config/binutils-2-1.cfg` / `gdb-common-1.cfg`. `fetch-rsb.sh` applies both
automatically right after cloning (before writing `PINNED_COMMIT`), so a fresh fetch
reproduces the patched state; its idempotency check also verifies the patch is present on an
existing checkout.

**Root cause (found by reading the actual build log, not the truncated RSB error report --
see "first attempt" below):** the first full toolchain build attempt failed on
`arm-rtems6-gdb-15.2` and `arm-rtems6-binutils-2.43` (and therefore `gcc`, which depends on
the completed binutils install) with `make: *** [all] Error 2` and no further detail in RSB's
own `rsb-report-*.txt` (it truncates at the failing `make` invocation, exactly as its own
"the error appears only in the complete build log" caveat warns). Reading
`third_party/rtems/toolchain-build-log/build.log` directly (not the `.txt` report) found the
actual compiler error: both packages' bundled `zlib` subdirectory fails to compile against
this host's Xcode 26 / clang 21 macOS SDK headers --

```
In file included from ../../gdb-15.2/zlib/zutil.c:10:
In file included from ../../gdb-15.2/zlib/gzguts.h:21:
In file included from .../MacOSX.sdk/usr/include/stdio.h:61:
.../MacOSX.sdk/usr/include/_stdio.h:322:7: error: expected ')'
  322 | FILE    *fdopen(int, const char *) __DARWIN_ALIAS_STARTING(__MAC_10_6, __IPHONE_2_0, __DARWIN_ALIAS(fdopen));
      |          ^
../../gdb-15.2/zlib/zutil.h:147:33: note: expanded from macro 'fdopen'
  147 | #        define fdopen(fd,mode) NULL /* No fdopen() */
```

i.e. zlib's own `zutil.h` `#define`s `fdopen` to `NULL` (its way of saying "this target has no
`fdopen`"), and that macro definition corrupts the SDK's own subsequent textual declaration of
the real `fdopen()` once `_stdio.h` is pulled in after it (a macro-hygiene collision, not a
logic bug in either project individually).

**Already reported upstream, confirmed by direct lookup, not assumed:** GitLab issue
[rtems/tools/rtems-source-builder#100 "Can not Build gdb-16.2-arm64-apple-darwin"]
(https://gitlab.rtems.org/rtems/tools/rtems-source-builder/-/issues/100) reproduces
byte-for-byte the same `zutil.c`/`fdopen`/`_stdio.h` error text (their trace even shows the
identical `#        define fdopen(fd,mode) NULL /* No fdopen() */` line), against RTEMS 6.1
and current main, on `arm64-apple-darwin`. The issue is marked **closed**, but this task
verified directly (not assumed) that neither the `6.1` tag nor upstream RSB `main`
(`gdb-common-1.cfg`/`binutils-2-1.cfg`, fetched read-only for comparison, not used as this
task's pin) actually carries a `--with-system-zlib` fix -- so closing the issue did not ship a
code change into RSB as of this pin. `GDB`'s already-passed `--without-zlib` disables GDB's
own compressed-debuginfo feature but does **not** stop the top-level binutils-gdb tree from
still building its bundled `zlib/` subdirectory for `bfd`; only the tree-wide
`--with-system-zlib` flag skips that subdirectory, which is why the fix must be added
separately from the pre-existing `--without-zlib`.

**Why this patch and not something else:** this Mac has a working system `libz`/`zlib.h`
(part of the Xcode SDK, always linkable), so `--with-system-zlib` is the documented
binutils-gdb top-level configure flag for "don't build the vendored copy, link the host's" --
not a workaround that changes behavior, just tells the build to use the zlib that already
works instead of the one that doesn't compile against this SDK.

**First attempt (superseded, kept as evidence, not re-run):** a first full `sb-set-builder
6/rtems-arm --keep-going` invocation (before this patch existed) failed `dtc`, then `gdb`,
then `binutils`, then (as a consequence of the missing binutils install) `gcc`. The dtc
failure is unrelated (see "dtc excluded" above) and was reproduced independently on a second,
patched run too (still not needed for this BSP). The gdb/binutils/gcc failures are the zlib
issue above and are what this patch fixes; the full toolchain build was re-run from a clean
`toolchain-build`/`toolchain` state after patching (see "Toolchain build" below for the
result).

## Memory map cross-check, BSP defaults vs Renode's zynqmp.repl (investigated before running)

Before running anything, cross-checked the BSP's default memory map
(`./waf bspdefaults --rtems-bsps=arm/zynqmp_rpu_lock_step`) against Renode's bundled
`platforms/cpus/zynqmp.repl` (the same file M24.1 used), because a mismatch here would be a
silent, hard-to-diagnose load/execution failure rather than a build error:

- `ZYNQMP_MEMORY_ATCM_ORIGIN/LENGTH = 0x0/0x20000` (128KB) vs Renode's `atcm0` region, which is
  only `size: 0x10000` (64KB) at cpu-relative `0x0` for `rpu0` -- **a real discrepancy,
  investigated rather than assumed benign.** Reading
  `rtems-src/spec/build/bsps/arm/xilinx-zynqmp-rpu/linkcmds.yml` shows only
  `REGION_VECTOR`/`REGION_START`/`REGION_FAST_TEXT` (the vector table, early startup code, and
  optionally fast-path text) are placed in ATCM; `REGION_TEXT`/`REGION_RODATA`/`REGION_DATA`/
  `REGION_BSS`/`REGION_WORK` (i.e. essentially all of `hello`/`ticker`'s code and data) are
  placed in `DDR` (origin `0x40000000`, length 512MB). A trivial sample's vector table plus
  startup code is far under 64KB, so this 64KB/128KB gap is not expected to matter for `hello`/
  `ticker` -- flagged here as a disclosed risk to watch for if a load/execution error occurs,
  not silently assumed safe without checking what actually lands in ATCM.
- `ZYNQMP_MEMORY_DDR_ORIGIN/LENGTH = 0x40000000/0x20000000` -- Renode's `zynqmp.repl` backs
  this with `ddrLowCommon` (`sysbus 0x30000`, `size: 0x7ffd0000`, i.e. covering
  `0x30000`-`0x80000000`), so `0x40000000` is real, mapped memory in Renode. No conflict.
- `ZYNQ_UART_KERNEL_IO_BASE_ADDR` defaults to `ZYNQ_UART_0_BASE_ADDR`, and Renode's
  `zynqmp.repl` maps `uart0: UART.Cadence_UART @ sysbus 0xff000000` -- matches the real Zynq
  UltraScale+ UART0 physical address, so the BSP's console output should land on Renode's
  `uart0` without any config.ini override.

## Same zlib bug hit GCC too, fixed via RSB's macro hook (not a vendored-file patch)

After the binutils/gdb `.cfg` patch, gdb still failed (see next section -- unrelated issue),
but binutils succeeded and the build proceeded to gcc, which **failed with the identical
`libz_a-zutil.o`/`fdopen` error** (`third_party/rtems/toolchain-build-log/build.log` lines
18226-20980 bracket the gcc package; the zlib error is at line 20636, confirmed by locating
the `package:`/`building:` markers for every package and checking which range the error falls
in -- not assumed from proximity alone). GCC builds its own bundled zlib copy too (for LTO
bytecode compression), independently of the binutils/gdb top-level tree.

Unlike binutils/gdb, `gcc-common-1.cfg`'s configure invocation already exposes an
extensibility hook for exactly this: `%{?gcc_configure_extra_options:%{gcc_configure_extra_options}}`.
Rather than patch a third vendored `.cfg` file, this uses RSB's own supported `--macros=<file>`
mechanism (`sb-set-builder --help`: "Macro format files to load after the defaults") to define
that macro: `third_party/rtems/toolchain-extra-macros.mc` sets
`gcc_configure_extra_options: none, none, '--with-system-zlib'` (same record format as RSB's
own `source-builder/defaults.mc`), and `build-toolchain.sh` passes
`--macros=toolchain-extra-macros.mc`. Verified with `--dry-run` before the real build that the
macro file parses and the bset still resolves identically otherwise. This is **not** counted
as a third carried patch under `third_party/rtems/patches/` -- it changes nothing inside the
fetched RSB checkout, only supplies a value through RSB's own designed override point.

## gdb excluded: a second, harder upstream issue (not patched -- disclosed deviation)

After the zlib patch fixed binutils and got gcc/gdb past that specific error, **gdb 15.2 hits
a second, independent, unresolved upstream problem** on this host's compiler
(`Apple clang 21` / the Xcode 26 toolchain): a hard compile error, not a warning --

```
../../gdb-15.2/gdb/symtab.h:920:31: error: constexpr variable 'SEARCH_ALL_DOMAINS' must be
initialized by a constant expression
../../gdb-15.2/gdb/../gdbsupport/enum-flags.h:97:34: error: non-type template argument is not
a constant expression
    integer_for_size<sizeof (T), static_cast<bool>(T (-1) < T (0))>::type
../../gdb-15.2/gdb/../gdbsupport/enum-flags.h:97:52: note: integer value -1 is outside the
valid range of values [0, 3] for the enumeration type 'innermost_block_tracker_type'
```

GDB's `enum_flags`/`enum_underlying_type` template (`gdbsupport/enum-flags.h`) detects an enum's
signedness by constructing `T(-1)` and comparing it to `T(0)` inside a `constexpr` context; this
relies on `T(-1)` being accepted as a valid (if technically out-of-range) constant expression,
which older/more lenient compilers allowed and current Clang's stricter core-constant-expression
enforcement now rejects outright as ill-formed. This is a real language-standard/compiler-version
incompatibility in gdb 15.2's own source, not a build-flag issue like the zlib case.

**Already reported upstream, confirmed by direct lookup, not assumed:** GitLab issue
[rtems/tools/rtems-source-builder#197 "Building GDB fails for RTEMS 6 on MacOS"]
(https://gitlab.rtems.org/rtems/tools/rtems-source-builder/-/issues/197) reproduces the
identical error text (`SEARCH_ALL_DOMAINS`, `enum-flags.h:97`, the same three error classes),
reported against this same `6/rtems-arm` invocation. It is marked **closed**, but the
resolution comments are not readable without a GitLab account (the REST API's notes/discussions
endpoints return 401 for anonymous requests, and the issue page itself is a JS-rendered Vue app
with no server-side-rendered comment text to scrape) -- so this task cannot report *how* it was
closed, only that it reproduces exactly and that neither the pinned 6.1 tag nor a fresh checkout
of upstream `main`'s `gdb-15.2.cfg`/`gdb-common-1.cfg` carries any compiler-version guard,
alternate compiler selection, or C++ standard override for it.

**Decision: gdb is not built, and no patch is carried for it.** Rationale: (1) this task's
actual deliverable is the arm-rtems6 cross toolchain (binutils/gcc/newlib) needed to build the
`zynqmp_rpu_lock_step` BSP and its samples, plus running those samples under Renode -- gdb (a
source debugger) is not required for compiling, linking, or running `hello`/`ticker`; (2)
unlike the zlib fix, this is not a one-line configure-flag change but a real source
incompatibility inside gdb 15.2 itself, which is out of this task's scope to patch without
understanding the maintainers' own (unreadable) resolution; (3) the same `%{with_rtems_gdb}`
bareword-inclusion mechanism as `dtc` (see above) means there is no working `--without-rtems-gdb`
command-line escape hatch either (confirmed the same way: `%defineifnot` only checks whether
`with_rtems_gdb` already has a value, and `--without-rtems-gdb` does not set that). The build is
run with `--keep-going` (already justified above for `dtc`), so gdb's failure does not block
binutils/gcc/newlib/rtems-tools, which have no build-time dependency on gdb being present. This
is disclosed here as a real capability gap, not a silent omission: **this toolchain has no
debugger**; if a later milestone needs `arm-rtems6-gdb`, that is unresolved upstream work, not a
one-line fix.

## Network use during fetch/build (question 154)

- `brew install autoconf automake texinfo` (host tool gap above).
- `git clone https://github.com/RTEMS/rtems-source-builder.git` (RSB, tag 6.1).
- `git clone https://github.com/RTEMS/rtems.git` (RTEMS kernel source, tag 6.1).
- RSB's own downloads during the toolchain build: binutils/gcc/newlib/gdb/rtems-tools source
  tarballs and the gcc/isl patches listed above, each SHA-512 verified by RSB itself before
  use (`ftp.gnu.org`, `codeload.github.com`, `gitlab.rtems.org`, `gcc.gnu.org` mirrors -- exact
  URLs are in the `.cfg` files quoted above and in RSB's own generated build report under
  `third_party/rtems/toolchain-build-log/`).
- Read-only lookups against `gitlab.rtems.org`'s public REST API (`/api/v4/projects/7/issues`)
  to check whether the two build failures above were already known upstream, and a read-only
  `git clone --depth 1 -b main` of `rtems-source-builder` into `/tmp` (not under `third_party/`,
  discarded after comparison) to confirm upstream `main` carries no fix for either. No account,
  token, or write action was used; these are the same kind of "record what had to be patched"
  research question 148 asks for, not a new production dependency.
- (further entries added for the BSP build and any Renode-related fetches, if needed)

## OUTCOME (manager, 2026-09-06): the host build FAILED — no usable toolchain

The backgrounded `sb-set-builder` run completed after **1:06:47** and **exited 0**, reporting
`Build Sizes: usage: 7.549GB total: 1.398GB (... installed 1.233GB)`. That exit code and that
"installed" figure are both misleading, and taking either at face value would have been wrong:

- **`third_party/rtems/toolchain/` does not exist.**
- **Zero `arm-rtems6-*` binaries were produced** (`find third_party/rtems -name 'arm-rtems6-*' -type f | wc -l` -> `0`).

The build ran with `--keep-going`, so RSB continued past failures and still exited 0. The log
records two hard failures:

```
error: building dtc-1.6.1-arm64-apple-darwin25.6.0-1
Build FAILED
error: building arm-rtems6-gdb-15.2-arm64-apple-darwin25.6.0-1
Build FAILED
```

`gcc-13.3.0-newlib` and `rtems-tools` did reach their build/report stage, but nothing was
installed into the prefix, so there is no toolchain to hand to a BSP build. **The `gdb` failure
was already understood and disclosed above (Apple clang 21 / Xcode 26 rejecting
`gdbsupport/enum-flags.h`'s `T(-1)` constexpr signedness probe). The `dtc` failure recurred
despite the zlib patch.**

**Status of this path: NO-GO on the macOS arm64 host.** Per the lead's decision, M24.2's exit is
carried by the container path (`third_party/rtems-container/`, task M24.2c): the same pinned RSB
recipe on a digest-pinned Debian/Ubuntu arm64 base, which is reproducible on any host and free of
the Xcode 26 friction. This host recipe stays recorded here as findings — the three zlib patches
and the gdb exclusion are real, reusable knowledge about this toolchain pairing, and the
`--keep-going` exit-0-on-failure behaviour is itself worth knowing before anyone trusts an RSB
exit code.

status: complete (outcome: NO-GO, superseded by the container path)

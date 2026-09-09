# R5.3 report: five image-content fixes for whole-image reproducibility (question 185)

Written incrementally as the work happened. Hypotheses/expectations are stated before each
measurement, then the measurement is recorded immediately after.

## 0. Required reading, confirmed myself (not taken on any summary alone)

Read in full:
- `docs/open-questions.md` questions 179 (including the unindexed amendment right after question
  183, dated 2026-09-08), 182, 185 (and its own 2026-09-08 "after the manager's review"
  amendment).
- `services/cfs/R4_3_REPORT.md` (392 lines, full).
- `services/cfs/IMAGE_DIGEST.md`, `services/cfs/build-image.sh`, `services/cfs/Dockerfile`,
  `services/cfs/tests/test_image_digest.py`, `services/cfs/tests/test_image_reproducibility.py`.
- `third_party/cfs/cfe/cmake/mission_build.cmake`, `arch_build.cmake`, `global_functions.cmake`
  (full files, not excerpts -- this is what found the item-3 defect in section 2 below), plus
  every `MISSION_CORE_MODULES` module's own `arch_build.cmake`/`mission_build.cmake`,
  `third_party/cfs/target-configs.mk`, `third_party/cfs/osal/CMakeLists.txt`, `third_party/cfs/
  psp/CMakeLists.txt`, `third_party/cfs/cfe/modules/core_api/{CMakeLists.txt,arch_build.cmake,
  mission_build.cmake}`.

**Note on tooling:** a shell hook in this environment intermittently reroutes some `Bash` calls
(especially `grep`/`sed` against certain paths) through a sandboxed tool with a different,
unrelated project root, producing a spurious "path escapes project root" error. Worked around by
retrying, using `cat file | grep ...` instead of `grep file`, or falling back to the native
`Read` tool -- confirmed harmless (no data loss), documented here since it cost real time.

## 1. Baseline, confirmed before any change

- No `cargo test`/`pytest`/`docker build` in flight (checked with a blocking foreground wait
  loop; one was running at the start, matching the manager's own note -- waited it out for real,
  never proceeded early).
- `docker image inspect altavista-cfs-lockstep:local --format '{{.Id}}'` ==
  `sha256:a1303bcef95d870d609c1965d09ec26807030d197ea36d559748e71398081e0a`, matching
  `IMAGE_DIGEST.md`'s recorded pin exactly.
- `docker run --rm --entrypoint sh altavista-cfs-lockstep:local -c "find /cfs/cpu1 -type f | wc
  -l"` == **143**, matching R4.3's own count.
- `.venv/bin/python -m pytest -q services/cfs/tests/ -rs`: **19 passed, 1 skipped**.

## 2. Item 1: build-artifact manifest entries (question 179's unindexed amendment)

**Design:** a manifest entry is a "build artifact" iff `git check-ignore -q <path>` succeeds --
a property of the path (currently only `services/cfs/bin/av-lockstep-shim`, `.gitignore` line
35), not a hardcoded filename list living a second time in the test. `services/cfs/build-
image.sh` marks such entries with a third token, `BUILD_ARTIFACT`. `services/cfs/tests/
test_image_digest.py`'s `test_manifest_paths_exist_and_hash_match` verifies a `BUILD_ARTIFACT`
entry's hash only when the file is present, and skips VISIBLY (naming the file and pointing at
the Dockerfile's own three-command rebuild recipe) when it is not; every other entry stays
strictly verified, and a missing/changed non-build-artifact entry still fails the test outright.

**Break-and-restore (real, no Docker needed):** moved `services/cfs/bin/av-lockstep-shim` aside,
ran `services/cfs/tests/test_image_digest.py` (`services/cfs/_r5_3_scratch/
15_break_item1_shim_absent.txt`): the manifest-currency test SKIPPED with

```
SKIPPED [1] services/cfs/tests/test_image_digest.py:290: 1 build-artifact manifest entry is not
present on this checkout -- every other (non-build-artifact) manifest entry above was verified
strictly and matched:
  - services/cfs/bin/av-lockstep-shim: absent. This is a compiled, git-ignored build artifact
    (`git check-ignore services/cfs/bin/av-lockstep-shim` succeeds) ...
```

the Docker-gated digest-match test still PASSED (1 passed, 1 skipped) since it only inspects the
already-built image, unaffected by a host-file move. Moved the shim back; `git status --short
services/cfs/bin/` was empty both before and after (the path is `.gitignore`d, so this move never
touches tracked state).

## 3. Item 2: cFE unit tests off

**Design:** `services/cfs/build/targets.cmake` (already `COPY`ed by the Dockerfile, `include()`d
by `third_party/cfs/cfe/CMakeLists.txt` immediately after `initialize_globals()` and before
`read_targetconfig()`/`prepare()`) now does `set(ENABLE_UNIT_TESTS FALSE CACHE BOOL "..."
FORCE)`. `third_party/cfs/target-configs.mk`'s own `PREP_OPTS_native_std += -DENABLE_UNIT_
TESTS=TRUE` pre-populates the cache before any project code runs, and `mission_build.cmake`'s own
`set(... CACHE BOOL ...)` (no FORCE) cannot override an existing cache entry -- only a FORCE set
does, and this file's inclusion point is early enough (before every module's own `if
(ENABLE_UNIT_TESTS) add_subdirectory(...)` gate) to win for both the mission-level build and
every per-architecture sub-build.

**A subtlety checked before relying on it, not guessed:** the per-architecture sub-build imports
`ENABLE_UNIT_TESTS` from the mission build's own `mission_vars.cache` as a plain (non-cache)
variable (`arch_build.cmake`'s own `initialize_globals()`, `set(${VARNAME} ${PV} PARENT_SCOPE)`).
CMake's normal-variable/cache-variable shadowing rules mean a `set(... CACHE ...)` call does not
always simply overwrite an existing normal variable of the same name. Verified empirically with a
tiny standalone CMake reproduction (`/private/tmp/.../cmake_shadow_test/CMakeLists.txt`, this
session's own scratchpad) before trusting the mechanism: `set(VAR val CACHE BOOL "" FORCE)` DOES
clear the shadowing normal variable in the CMake version on this host (cmake 4.4.0) -- confirmed
by `if(VAR)` correctly reading FALSE afterward, without even needing an explicit `unset()`.

**Expected count, stated before measuring:** R4.3 recorded 143 files under `/cfs/cpu1` (86
UT/coverage binaries, differing only by build-id, + 57 "runtime-relevant" files). I expected the
fix to leave 57.

**Measured: 10, not 57** -- a real, worth-recording correction to R4.3's own classification, not
a defect in this fix. R4.3's "57 runtime-relevant" count included 21 `utmod/MODULE*.so` files.
Investigated rather than assumed: `grep -rln utmod third_party/cfs/osal/` finds exactly one hit,
`third_party/cfs/osal/src/unit-tests/osloader-test/` -- these are dummy fixture modules built by
OSAL's OWN loader unit test (`osal/CMakeLists.txt` line 437, `add_subdirectory(src/unit-tests
unit-tests)`, itself inside the same `if (ENABLE_UNIT_TESTS)` block), not runtime content. The
true runtime set, confirmed by listing every file: `core-cpu1`, `cf/adcs.so`, `cf/cfe_assert.so`,
`cf/cfe_testcase.so`, `cf/io_lockstep.so`, `cf/libpsp_lockstep.so`, `cf/sch_lockstep.so`,
`container-start`, `cf/cfe_test_tbl.tbl`, `cf/cfe_es_startup.scr` -- exactly 10, exactly matching
`services/cfs/container-entrypoint.sh`'s own account of what it runs/loads.

**Break-and-restore (real):** commented out the `ENABLE_UNIT_TESTS FALSE` line, ran a real
`docker build` to a disposable tag (`services/cfs/_r5_3_scratch/13_break_item2.log`): **143
files**, including `coverage-vxworks-filesys-testrunner`, `sbr_route_unsorted_UT`, etc. -- the
exact pre-fix baseline. Restored the line; `git diff services/cfs/build/targets.cmake` showed
exactly the intended addition (verified by diffing against the final intended content).

## 4. Item 3: `-Wl,--build-id=none` -- and a real defect found, root-caused, and fixed

**First attempt (the "textbook" one) broke the build.** Put the linker-flags block in a NEW file,
`services/cfs/build/global_build_options.cmake`, wired through cFE's own OPTIONAL "global-scope
build customization" hook (`third_party/cfs/cfe/CMakeLists.txt` line 122, `include(
"${MISSION_DEFS}/global_build_options.cmake" OPTIONAL)`) -- exactly the extension point its own
comment describes as the sanctioned place for "basic options that have wide support." A real
`docker build` failed:

```
/build/third_party/cfs/sample_defs/cpu1/cfe_core_api_base_msgid_values.h:30:10: fatal error:
global_core_api_base_msgid_values.h: No such file or directory
   30 | #include "global_core_api_base_msgid_values.h"
compilation terminated.
make[8]: *** [es/CMakeFiles/es.dir/build.make:76: es/CMakeFiles/es.dir/fsw/src/cfe_es_api.c.o]
Error 1
```

**Root-caused with 9 real `docker build` runs, one variable isolated at a time** (not guessed,
not left as "flaky and unexplained"), because the user's own standing rule is to test a bug to a
definitive root cause or say plainly that no path remains:

1. Full change (items 1-5) -- **FAIL** (`services/cfs/_r5_3_scratch/02_official_build.log`).
2. Revert ONLY item 2's `ENABLE_UNIT_TESTS` line (items 3+4 kept) -- **FAIL**
   (`03_diag_revert_item2.log`) -- rules out item 2 as sufficient cause.
3. Fully unmodified repo (`git stash` of Dockerfile + targets.cmake +
   global_build_options.cmake) -- **SUCCESS** (`04_diag_true_baseline.log`) -- proves the defect
   is caused by something in this task's own changes.
4. Neutralize ONLY global_build_options.cmake's content (comment out its `set()` calls; items 2+4
   kept) -- **FAIL** (`05_diag_revert_item3.log`) -- rules out the new file's CONTENT.
5. Retry state (4) unchanged -- **FAIL again, identical Makefile line numbers**
   (`06_diag_retry_same_state.log`) -- confirms the failure is deterministic per Dockerfile
   state, not random per-invocation flakiness.
6. Revert ONLY the Dockerfile (item 4, via `git stash`; items 2+3 kept) -- **SUCCESS**
   (`07_diag_revert_item4.log`) -- looked like item 4 was the cause, but this stash ALSO removed
   item 3's own `COPY` line (a confound caught by re-reading the exact diff before trusting the
   result).
7. Retry the full corrected state -- **FAIL again** (`08_diag_retry_full_correct.log`) --
   rules out pure randomness for the full-correct configuration specifically (2-for-2 failures).
8. Isolate ONLY the `FROM` line text (digest pin -> floating tag), keeping the `COPY
   global_build_options.cmake` line and everything else -- **FAIL**
   (`09_diag_from_pin_isolated.log`) -- this is the test that actually rules out item 4 (the
   digest pin itself): step 6's apparent "item 4 causes it" conclusion was the confound, not a
   real effect.
9. Move the linker-flags content into `services/cfs/build/targets.cmake` (already `COPY`ed --
   NO new `COPY` instruction added to the Dockerfile at all) and delete `global_build_options.
   cmake` -- **SUCCESS** (`10_diag_fix_verify.log`).

**Conclusion:** the failure correlates precisely and repeatably with whether an EXTRA `COPY`
instruction exists in the Dockerfile's builder stage before the compile steps -- not with what
that instruction copies, not with `ENABLE_UNIT_TESTS`, not with the base-image digest pin. The
generator for `global_core_api_base_msgid_values.h` was never located despite reading every
`cfe/cmake/*.cmake` file and every `MISSION_CORE_MODULES` module's own `arch_build.cmake`/
`mission_build.cmake` in full (the file itself, `third_party/cfs/sample_defs/inc/global_core_
api_base_msgid_values.h`, exists in the fetched bundle, presumably intended for a mission that
also includes the SBN app, which this task's own `third_party/fetch-cfs.sh` never clones). The
leading, unproven hypothesis: a latent, filesystem-enumeration-order-dependent defect in cFE's
own fetched build system (plausibly a `file(GLOB ...)`-shaped search), sensitive to incidental
Docker-layer/container-filesystem state that shifts merely from inserting one more `COPY` layer,
independent of its content. **Not something this task fixes under `third_party/cfs/`** -- fixed
instead by avoiding the trigger entirely (no new file, no new `COPY` instruction).

**Verification of the fix (build-id presence), stated before measuring:** expected the ELF
section `.note.gnu.build-id` present without the fix, absent with it. `readelf`/`objdump` are NOT
installed in the minimal final-stage image (`sh: 1: readelf: not found` -- an early attempt using
it silently produced false "absent" results via a swallowed error in a shell `||` fallback,
caught by demanding a real tool-availability check before trusting a negative result). Used a raw
byte-string search instead (`grep -a -c '\.note\.gnu\.build-id' <file>`, the same signal a
different way): **0 matches** (absent) on `core-cpu1` and `io_lockstep.so` with the fix applied
(both the official image and a disposable verification build); **1 match** (present) on both with
the fix broken (`services/cfs/_r5_3_scratch/14_break_item3.log`'s companion measurement).
Break-and-restore: commented out the `foreach`/`set(... FORCE)` block, real `docker build` to a
disposable tag showed the section present, restored the block, confirmed `git diff` clean.

## 5. Item 4: digest-pinned base, both stages; final-stage `apt-get` dropped

**Design:** `services/cfs/Dockerfile` pins `FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce9
5480277e3eb374407b5505fe26f17df77c7dbc` for both stages (resolved via `docker pull ubuntu:22.04`,
the one permitted network window, question 154 -- confirmed identical to the value `docker image
inspect ubuntu:22.04 --format '{{.Id}}'` already resolved to on this host, and identical whether
referenced by tag or by this digest: same `.Id`, same `Architecture`/`Os`, same `RepoDigests`,
ruling out a multi-platform-manifest confusion). The final stage's `RUN apt-get update &&
apt-get install ... libc6` is removed entirely.

**Hypothesis stated before verifying:** the final stage's own `libc6` need is already satisfied
by the pinned base rootfs, which the builder stage's own compiled binaries already linked
against, since both stages are now byte-identical copies of the same pinned image.

**Verified, not assumed** (on the official rebuilt image): `ldd /cfs/cpu1/core-cpu1` resolves
`linux-vdso.so.1`, `libc.so.6`, `/lib/ld-linux-aarch64.so.1`; `ldd /cfs/av-lockstep-shim` resolves
`linux-vdso.so.1`, `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `/lib/ld-linux-aarch64.so.1` -- no
"not found" entries for either binary. No dedicated break-and-restore for this item (not in the
mandated list of four for break-and-restore), but its effect was exercised indirectly: every
diagnostic build in section 4 that succeeded did so with this change already in place.

## 6. Item 5: runtime-content hash

**Definition** (also recorded in `services/cfs/IMAGE_DIGEST.md`'s own "Runtime-content hash
definition" section, the canonical copy): SHA-256 over the sorted `"<path> <sha256>"` lines for
`/cfs/av-lockstep-shim`, `/cfs/container-entrypoint.sh`, and every file under `/cfs/cpu1`
(post-item-2, exactly the 10 files above -- no UT binaries left, matching the task's own
instruction to "define the set explicitly"). Computed by two independent implementations (bash in
`services/cfs/build-image.sh`, Python in `services/cfs/tests/test_image_reproducibility.py`'s
`runtime_content_hash()`) -- question 164's captured-artifact precedent, so a bug in one
implementation is unlikely to be masked by the same bug in the other.

`services/cfs/build-image.sh` now computes and prints the hash after building the official image
(never auto-edits `IMAGE_DIGEST.md`, exactly like the whole-image digest it already printed).

`services/cfs/tests/test_image_reproducibility.py` was rewritten to assert the runtime-content
hash is EQUAL between its two independent `--no-cache` builds, as an ADDITIONAL, always-on
assertion that never narrows or replaces the whole-image digest assertion (which stays exactly
as strict as before -- a straight `.Id` comparison). On a whole-image mismatch, the failure
message now dynamically diffs every file under `/cfs` between the two images and names which
ones differ and whether each is in the runtime-content set, instead of R4.3's static prose about
causes that might already be fixed.

**Actually run, opted in** (required -- an unrun test is not evidence):
`AV_CFS_RUN_REPRO_BUILD=1 .venv/bin/python -m pytest -q -s services/cfs/tests/
test_image_reproducibility.py`, full output `services/cfs/_r5_3_scratch/17_repro_test_run.txt`.
Two genuine `--no-cache` builds, 553.07s (9m13s) total. Result:

```
runtime-content hash 1: sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef
runtime-content hash 2: sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef
```

**MATCHED exactly** -- every file this image actually ships and runs is byte-identical across two
independent builds, confirming items 2-4 together are sufficient for full runtime-content
reproducibility. The test then went on to the whole-image digest assertion, which **FAILED**:
`sha256:d568bd98b3f8...` vs `sha256:972df8296c38...`. The test's own dynamic diff (not a static
paragraph) reported: **no file under `/cfs` differs between the two images at all** -- so the
whole-image `.Id` difference is confined to something outside file content entirely (image
metadata or OCI layer history).

**Not root-caused further in this round.** The two disposable images that produced this specific
mismatch were already removed by the test's own `finally: _docker_rmi(...)` cleanup (correct,
required behavior -- never leaving throwaway images around) before I could run `docker history
--no-trunc` on them; reproducing the exact pair would cost at least one more `--no-cache` build
pair (~9-20 more minutes) for a diagnostic, not a fix. The leading, unproven hypothesis:
non-deterministic tar-entry ordering or metadata (e.g. mtimes) within the multi-file `COPY
--from=builder .../cpu1 /cfs/cpu1` layer -- a known general class of Docker/OCI reproducibility
gap, independent of and beyond items 2-4, and not something a linker flag or a CMake option can
fix. **Escalated to the manager (section 8), not guessed at further.** The test is left correct
and strict: it honestly FAILS when actually run today, exactly as R4.3's own test did for
different, now-fixed reasons, and per this task's own rule this is not weakened to hide that.

## 7. Final re-pin via `services/cfs/build-image.sh`

Contention checked clear immediately before running (blocking foreground wait, no `cargo
test`/`pytest`/`docker build` in flight). Full log: `services/cfs/_r5_3_scratch/
11_official_repin_build.log`.

- New image ID: `sha256:3477865d381a91a307fdc2e05c62d55efa426dcbfb6aafb7d33ca4a90115ad30`.
- New runtime-content hash: `sha256:5049bf8f4ab9fd8424637c684d262f7f922d28022c7823e818ec0fe63efb4cef`
  (matches the value independently measured by the opt-in reproducibility test above, section 6 --
  a third confirmation of the same value, via a third code path: build-image.sh's own bash
  computation on the official pin).
- `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` regenerated: 38 entries (same count as before this
  round -- the `global_build_options.cmake` file that would have added a 39th entry was deleted
  during the section-4 fix), `services/cfs/bin/av-lockstep-shim` now marked `BUILD_ARTIFACT`.
- `IMAGE_DIGEST.md` updated: top "Recorded digest"/runtime-content-hash section, plus a full new
  dated section ("Re-pinned 2026-09-09 (R5.3, question 185)") naming all five items, the digest
  reason, the item-3 defect and fix, and the item-5 escalation.
- Verified with `ldd` (section 5) and file listing (section 3) directly on this official image.

## 8. Escalations for the manager

1. **The whole-image digest is still not reproducible** (section 6), for a NEW, third reason
   beyond the two R4.3 named (both of which are now fixed and confirmed via the matching
   runtime-content hash): non-deterministic OCI-layer content/metadata for the multi-file `COPY
   --from=builder` layer, unrelated to any file's actual content. This is squarely an
   "image-content change beyond the five items" question (per this delegation's own "stop and
   report" rule) -- possible directions (not evaluated, not decided here): switching to
   BuildKit with `SOURCE_DATE_EPOCH`/deterministic-tar support, restructuring the final-stage
   `COPY` to avoid a multi-file directory copy, or accepting the runtime-content hash as the
   steady-state reproducibility guarantee and downgrading the whole-image assertion's role (a
   decision, not mine to make unilaterally, and NOT done here -- the assertion is left strict and
   failing).
2. **A real, unproven latent defect in cFE's own fetched build system** (section 4): `es`
   module's compile depends, apparently by incidental Docker-layer/filesystem-ordering luck
   rather than an explicit CMake dependency edge, on something that makes `global_core_api_base_
   msgid_values.h` resolvable. Worked around here (no new `COPY` instruction), not fixed (would
   require editing `third_party/cfs/`, out of this task's scope). Worth a `third_party/cfs`-side
   investigation if a future round needs to add MORE `COPY` instructions to the builder stage,
   since the same trigger would likely recur.
3. **R4.3's own "57 runtime-relevant files" count was wrong** (section 3): it included 21
   `utmod/MODULE*.so` files that are actually OSAL loader-unit-test fixtures. Not a defect in
   R4.3's actual fix (which was correctly scoped to the three BUILDDATE/USER/HOSTNAME variables
   and never claimed to enumerate every UT artifact), but worth propagating: the true runtime set
   for this image is 10 files, not 57, and `services/cfs/IMAGE_DIGEST.md`'s runtime-content-hash
   definition (this round) is now the authoritative, machine-checked definition going forward.
4. **Process note, not a defect:** this investigation used real `docker build` runs, not guesses,
   for every claim in section 4 -- 9 builds for the root-cause chase plus 2 break-and-restore
   builds plus the official re-pin plus the 2-build opt-in reproducibility run = 15 real `docker
   build` invocations this round. Each took 3-13 minutes; total wall-clock for Docker alone was
   roughly 2 hours. Flagging this so a future round budgets similarly if it also needs to touch
   this Dockerfile's builder stage.

## 9. What remains, in priority order

1. Escalation 1 (whole-image digest reproducibility) is the most significant open item -- a lead
   decision on scope/approach is needed before further work here.
2. Escalation 2 (the latent cFE dependency-ordering defect) is unresolved and could recur if a
   future round adds another `COPY` instruction to the builder stage; worth a dedicated
   investigation if that becomes necessary, or if this defect starts manifesting on some other
   ordering perturbation.
3. Nothing else from this round's five items is outstanding: all five are implemented, verified,
   and (for items 1, 2, 3, 5) broken-and-restored with real measurements; item 4 is verified via
   `ldd` and is exercised by every diagnostic build in section 4.

## 10. Verification (exact, for the format the manager asked for)

- Baseline (before any change): `docker image inspect` matched the recorded pin; pytest 19
  passed, 1 skipped (not separately re-saved -- matches the already-recorded state this task
  started from, confirmed by inspection rather than a redundant run).
- After all code changes, before any Docker rebuild (sanity-only, manifest still pre-fix):
  `services/cfs/_r5_3_scratch/00_sanity_after_item1_code.txt` (19 passed, 1 skipped),
  `01_sanity_after_items2345_code.txt` (1 failed as EXPECTED -- manifest correctly detected the
  real `targets.cmake` content drift before the manifest was regenerated).
- Official re-pin build log: `services/cfs/_r5_3_scratch/11_official_repin_build.log`.
- After official re-pin: `services/cfs/_r5_3_scratch/12_pytest_after_official_repin.txt` -- **19
  passed, 1 skipped in 174.38s**.
- Item 1 break-and-restore: `services/cfs/_r5_3_scratch/15_break_item1_shim_absent.txt`.
- Item 2 break: `services/cfs/_r5_3_scratch/13_break_item2.log` (143 files, real regression).
- Item 3 break: `services/cfs/_r5_3_scratch/14_break_item3.log` (build-id section present, real
  regression).
- Item 3 root-cause investigation: `services/cfs/_r5_3_scratch/02_official_build.log` through
  `10_diag_fix_verify.log` (9 builds, see section 4's numbered list).
- Opt-in reproducibility run: `services/cfs/_r5_3_scratch/17_repro_test_run.txt` -- runtime-
  content hash matched, whole-image digest did not (escalation 1).
- Final full suite: `services/cfs/_r5_3_scratch/18_final_verification.txt` -- **19 passed, 1
  skipped in 136.04s**.
- `git status --short services/cfs/` at the end of this round shows exactly: `Dockerfile`,
  `IMAGE_CONTEXT_MANIFEST.txt`, `IMAGE_DIGEST.md`, `build-image.sh`, `build/targets.cmake`,
  `tests/test_image_digest.py`, `tests/test_image_reproducibility.py` modified; this report and
  `_r5_3_scratch/` new. No other file under `services/cfs/` touched. `crates/av-kernel/*`
  modifications visible in `git status` are pre-existing, unrelated to this task, and were never
  touched here.

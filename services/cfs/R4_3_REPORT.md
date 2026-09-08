# R4.3 report: reproducible cFS container image (question 182)

Written incrementally. Hypotheses/expectations are stated before each measurement, then the
measurement is recorded immediately after.

## 0. Required reading, confirmed myself (not taken on the report's word)

Read in full (not excerpted):
- `services/cfs/IMAGE_DIGEST.md`, especially "Re-pinned 2026-09-08 (M25.4a, question 179): the
  cause, named".
- `services/cfs/build-image.sh` (139 lines).
- `services/cfs/tests/test_image_digest.py` (265 lines).
- `services/cfs/Dockerfile` (93 lines).
- `third_party/cfs/cfe/cmake/generate_build_env.cmake` (48 lines, full file, not just lines
  1-40 as the brief suggested -- see finding below).

**Finding, checked myself, that changes the plan:** `IMAGE_DIGEST.md`'s prose says "set
`BUILDDATE`, `BUILDUSER` and `BUILDHOST` to fixed values in `services/cfs/Dockerfile`" and only
quotes `generate_build_env.cmake` lines 15-23 (the `BUILDDATE` block) as evidence. Reading the
*whole* file (lines 25-43) shows the other two cmake variables do **not** read the environment
variables of the same name:

```
set(BUILDHOST $ENV{HOSTNAME})   # reads env var HOSTNAME, not BUILDHOST
...
set(BUILDUSER $ENV{USER})       # reads env var USER, not BUILDUSER
```

So `ENV BUILDUSER=altavista` / `ENV BUILDHOST=altavista-build` in the Dockerfile would compile
and do **nothing** -- cmake would still fall back to `whoami`/`hostname` inside the container
(root/some container hostname), silently leaving 2 of the 3 non-reproducible variables
unpinned. Confirmed by grepping the whole `third_party/cfs/` tree: `ENV{USER}`, `ENV{HOSTNAME}`,
`ENV{BUILDDATE}` are the only three env-var reads for this purpose; nothing reads
`ENV{BUILDUSER}` or `ENV{BUILDHOST}` anywhere in the fetched tree.

**Plan, revised accordingly:** pin three ENV lines in the Dockerfile's builder stage --
`BUILDDATE`, `USER`, and `HOSTNAME` (env var names), holding the *values* the brief suggested
for build-user/build-host identity (`altavista`, `altavista-build`). This still pins exactly
the three cFE-side variables (`BUILDDATE`/`BUILDUSER`/`BUILDHOST` inside `CONFIGDATA`), just
under the host env-var names cFE's cmake actually reads.

Also confirmed: `third_party/fetch-cfs.sh` and the rest of the fetched `third_party/cfs/` tree
make no other use of `$USER` or `$HOSTNAME`, so setting these two env vars for the builder
stage has no other observable effect on the build (grepped, zero other hits).

**Mechanism hypothesis (per the brief's instruction to state this before changing anything):**
`ARG` is build-time-only and is not automatically exported into the process environment of
later `RUN` layers unless separately consumed; `ENV` sets a real environment variable that
persists into every subsequent `RUN`/`CMD` in that stage (and is what cmake's `$ENV{...}`
reads via `getenv()`). The three variables must be visible to the `RUN make native_std.compile`
step (cFE's `generate_build_env.cmake` comment: "runs at build time (as opposed to prep time)"),
so they must be set with `ENV`, not `ARG`, and set before that RUN line (placed right after
`FROM ubuntu:22.04 AS builder`, ahead of every RUN in that stage, is sufficient and simplest).

## 1. Baseline measurement (before any change)

- No other `cargo test`/`pytest`/`docker build` in flight (`ps -Ao pid,etime,command | grep -E
  "cargo test|pytest|docker build"` empty). `docker info` succeeded.
- `git status --short services/cfs/` clean; branch `develop` at `7797d29`.
- `.venv/bin/python -m pytest -q services/cfs/tests/` (full output:
  `services/cfs/_r4_3_scratch/baseline_pytest.txt`): **19 passed in 43.42s**, exit 0.
- `docker image inspect altavista-cfs-lockstep:local --format '{{.Id}}'` ==
  `sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde`, matching
  `IMAGE_DIGEST.md`'s currently recorded pin exactly. Starting point is clean and consistent.

## 2. Escalation noted before proceeding (not blocking, see section 5 below for full writeup)

`docs/open-questions.md` question 182's own text in the repo also carries an unindexed
amendment paragraph (right after question 183, dated 2026-09-08) assigning
`test_manifest_paths_exist_and_hash_match` build-artifact handling
(`services/cfs/bin/av-lockstep-shim`, untracked, currently present on this host so the test
passes here but would fail on a clean checkout) to "R4.3 owns that test". This delegation's own
"What to build" list (items 1-4) does not mention it. I did **not** implement it -- it is a
distinct, separately-decided feature touching the same test file, outside the four numbered
items I was given, and the brief's own rule is to escalate rather than silently expand scope.
Recorded fully in section 5.

## 3. Dockerfile change made and verified

`services/cfs/Dockerfile`: added three `ENV` lines right after `FROM ubuntu:22.04 AS builder`,
before any `RUN`:

```
ENV BUILDDATE=202609080000
ENV USER=altavista
ENV HOSTNAME=altavista-build
```

(Not `ENV BUILDUSER=...`/`ENV BUILDHOST=...` -- see the finding in section 0: those names are
never read by `generate_build_env.cmake`.)

Re-pinned via `services/cfs/build-image.sh` (never a hand-rolled `docker build`), network window
per question 154. Full log: `services/cfs/_r4_3_scratch/build_repin.log`.

- New image ID: `sha256:9d7757ef4dd9463e6347692b1e35748229e23a9df8f8b3e12875d1509c0861fa`.
- Previous (question 179) image ID:
  `sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde`.
- Verified from the log (not exit code) that the builder stage genuinely re-ran: grepped
  `Step \|Using cache\|Running in` -- every builder-stage step (1-20) shows `Running in ...`,
  none show `Using cache`; only the unrelated final-stage `apt-get libc6` layer was cached.
- `services/cfs/IMAGE_CONTEXT_MANIFEST.txt` regenerated by the same run (38 file entries, same
  set of paths/hashes as before -- the Dockerfile itself is not a COPY source so its own edit
  does not appear there; only the image ID comment line at the top of the manifest changed).
- Confirmed the three pinned values are actually linked into the compiled binary, not just
  present in the Dockerfile text: `docker run --rm --entrypoint sh altavista-cfs-lockstep:local
  -c "grep -a -o '202609080000' /cfs/cpu1/core-cpu1"` (and similarly for `altavista-build` and
  `altavista`) each returned a match.
- `IMAGE_DIGEST.md` updated with a new dated section ("Re-pinned 2026-09-08 (R4.3, question
  182)") naming question 182, the exact ENV lines, and the reasoning above; recorded digest and
  the "Previous digest" line both updated.
- `.venv/bin/python -m pytest -q services/cfs/tests/test_image_digest.py -v` (full output:
  `services/cfs/_r4_3_scratch/after_repin_digest_test.txt`): **2 passed in 0.20s**, exit 0 --
  both the manifest-currency test and the digest-match test pass against the new pin.

## 4. Reproducibility test: written, then actually run -- and a real, deeper finding

**Hypothesis stated before writing the test (per the brief's own instruction):** two full builds
are too expensive (10+ min each measured below) for the default suite or CI to run on every
invocation. Chose **option (a)**: the test really does perform two independent
`docker build --no-cache` runs (disposable tags, never touching `altavista-cfs-lockstep:local`
or the recorded manifest), gated behind BOTH Docker availability (reusing
`test_image_digest.py`'s own `docker_available()`, imported by path, not reimplemented) AND a
new opt-in env var `AV_CFS_RUN_REPRO_BUILD` -- so the default suite skips it visibly, and a
human/CI can turn it on. `--no-cache` (not `--build-arg CACHEBUST=<n>`) was chosen to avoid a
second Dockerfile change beyond the three pinned variables. File:
`services/cfs/tests/test_image_reproducibility.py`.

**Actually ran it** (required -- an unrun test is not evidence):
`AV_CFS_RUN_REPRO_BUILD=1 .venv/bin/python -m pytest -q -s services/cfs/tests/
test_image_reproducibility.py`, full output saved to
`services/cfs/_r4_3_scratch/repro_test_positive_run.txt`. Result: **FAILED in 540.83s (9m01s)**,
two genuinely independent `--no-cache` builds of the current, correctly-pinned Dockerfile:

```
digest 1: sha256:841a01b176171c7ab1ab3402e16bce4142c761a9473e56c1ab887b9025b00a73
digest 2: sha256:08808106bae5c1b56d87b625356cf46eb310cfda252a6b0b9aefd538dabee17f
```

**This is a real, unexpected result, not a test bug -- investigated rather than dismissed.**
Confirmed the builder stage genuinely re-ran both times (`--no-cache` guarantees this; also
directly confirmed via a third throwaway build, log `services/cfs/_r4_3_scratch/
investigate_build.log`, `docker image inspect` -> `sha256:7cfc1d52a5baf60f622f5262c66ed19125fbb
86119f5f8abaaeae9616c6c8374`, distinct from the official pin
`sha256:9d7757ef4dd9463e6347692b1e35748229e23a9df8f8b3e12875d1509c0861fa`).

`docker history --no-trunc` on the official image vs. that third throwaway build (files:
`services/cfs/_r4_3_scratch/history_official.txt` / `history_investigate.txt`) shows the two
static-host-file COPY layers (`av-lockstep-shim`, `container-entrypoint.sh`) have IDENTICAL
`file:` content digests across builds (as expected -- those are pre-built/static, not
recompiled), but:
  - The `COPY --from=builder .../cpu1 /cfs/cpu1` layer's `dir:` digest DIFFERS
    (`dir:54496a1abdcdca...` vs `dir:c8a2d18e2379ac...`).
  - The final stage's own `apt-get install libc6` layer ALSO differs between the two `--no-cache`
    runs (`sha256:06f1f84f97a0...` vs `sha256:b02aa7d901e8...`) -- a second, independent,
    unrelated non-determinism source (unpinned apt package/index state), separate from anything
    the builder stage or `generate_build_env.cmake` does.

**Narrowed the `/cfs/cpu1` diff to specific files** (sha256 of every file under `/cfs/cpu1` in
both images, `services/cfs/_r4_3_scratch/content_official.txt` /
`content_investigate.txt`, diffed): 86 of 143 files differ in content -- and **every single one
of the 86 is a `coverage-*-testrunner` / `*-test` / `*_UT` cFE/OSAL unit-test or coverage-harness
executable** (a byproduct `make native_std.install` installs, never invoked by
`services/cfs/container-entrypoint.sh` at runtime). **Confirmed the other 57 files -- every
runtime-relevant artifact -- are byte-identical across the two independent builds:**
`core-cpu1` itself, all mission `.so` app/PSP modules (`adcs.so`, `io_lockstep.so`,
`sch_lockstep.so`, `libpsp_lockstep.so`, `cfe_testcase.so`, `cfe_assert.so`), `cf/cfe_es_startup.
scr`, `cf/cfe_test_tbl.tbl`, `container-start`, and all 21 `utmod/MODULE*.so` files. **The
question-182 fix (pinning BUILDDATE/USER/HOSTNAME) is confirmed effective for everything the
image actually runs.**

**Isolated the cause for one differing test binary**
(`coverage-config-ALL-testrunner`, 162408 bytes both builds, same size): `cmp -l` found only 34
differing bytes total, in ~7 small clusters, the first cluster starting at byte 613 -- `xxd`
confirms this is inside the ELF `.note.gnu.build-id` section (`GNU\0` magic immediately
precedes the first differing bytes in both files). This is the well-known GNU-linker
`--build-id=sha1` non-determinism class (the note's hash differs across two links of what is,
by every other measure, identical input), unrelated to cFE's `generate_build_env.cmake`
mechanism this task was scoped to fix.

**Conclusion:** the three-variable pin (question 182's actual ask) is necessary and confirmed
sufficient for every artifact the container actually runs. It is NOT sufficient to make the
whole image's content-addressed `.Id` reproducible, because of two additional, independent,
out-of-scope non-determinism sources: (1) linker build-id non-determinism in the bundled
cFE/OSAL unit-test/coverage-harness binaries (never executed at runtime), and (2) the final
stage's `apt-get install libc6` layer. Fixing either changes image content beyond the three
pinned variables (e.g. stripping/excluding the UT harnesses, or passing `-Wl,--build-id=none`
into cFE's toolchain file, or pinning apt package versions/a frozen mirror) -- **escalated to
the manager in section 5, not guessed at here**, per this task's own "image-content change
beyond the three variables -> stop and report" rule. The reproducibility test is left correct
and strict (whole-image digest comparison, as question 182 literally asked for): it currently,
honestly, FAILS when actually run, and must not be weakened to hide that.

## 5. Break-and-restore: the exact named regression (`BUILDDATE` removed)

**Hypothesis stated before running:** since the reproducibility test already fails on the
correctly-pinned Dockerfile (section 4, for the two unrelated reasons found there), a whole-image
digest comparison after removing `BUILDDATE` will *also* fail, but that alone would not cleanly
prove this specific regression is what the test (or the pin) catches -- the signal would be
confounded by the pre-existing build-id/apt noise. So, in addition to the whole-image digest,
isolate a **clean signal**: compare `core-cpu1`'s own sha256 (confirmed in section 4 to be
byte-identical across two good, pinned builds) between two builds with only `BUILDDATE` removed.
If the pin is what's protecting `core-cpu1`'s reproducibility, removing it alone should make
`core-cpu1` itself start differing, even though the `.so` app modules (which don't link
`CONFIGDATA`) should stay identical.

**Break:** edited `services/cfs/Dockerfile`, removed only the `ENV BUILDDATE=202609080000` line
(kept `ENV USER=altavista` / `ENV HOSTNAME=altavista-build`). Ran two independent
`docker build --no-cache` builds to disposable tags (`altavista-cfs-break-1:local`,
`altavista-cfs-break-2:local`; logs `services/cfs/_r4_3_scratch/break_build1.log` /
`break_build2.log`), sequential, ~14 minutes apart (long enough that "same wall-clock minute"
coincidence is not a concern here -- noted per the brief's own warning about that risk).

Results:
```
break-1 image ID: sha256:f98c78069ce91dcd2921bc66b2a1c8b8d60ec3b055b18dd3bd658cfed6e791c7
break-2 image ID: sha256:5ebd22047929786baef1b1a17e53b87107c2564e8c33a1bc33a6aafa456e44eb
  -> whole-image digest: MISMATCH (expected; confounded by section-4 noise too)

core-cpu1 sha256, break-1: 13fb1865e167ce349e05526f1ffb072bb73c5e4cdd1c3e0614888ee29841acd7
core-cpu1 sha256, break-2: 6e14d551d847b29a6a86a461e26fe2c19624fc73718e8d08d23d3c6c22de5a18
  -> DIFFERENT (clean signal: this file was byte-identical across two *pinned* builds in
     section 4; removing only BUILDDATE alone is sufficient to break its reproducibility)

Embedded fallback timestamp found inside core-cpu1 (grep -a -o '202[0-9]{9}'):
  break-1: 202609081555
  break-2: 202609081609
  -> confirms generate_build_env.cmake's `date +%Y%m%d%H%M` fallback fired, with genuinely
     different wall-clock minutes (14 min apart), exactly the mechanism named in IMAGE_DIGEST.md

.so app modules (adcs.so, cfe_assert.so, cfe_testcase.so, io_lockstep.so, libpsp_lockstep.so,
sch_lockstep.so): IDENTICAL between break-1 and break-2 -- consistent with BUILDDATE/BUILDUSER/
BUILDHOST being linked only into cFE's core CONFIGDATA object (core-cpu1), not into each app's
own .so, exactly as generate_build_env.cmake's own comment says ("linked into the final
CONFIGDATA object").
```

This is exact, unconfounded, real panic/mismatch text -- not a guess: `core-cpu1`'s own content
hash provably changes between two builds differing only in whether `BUILDDATE` is pinned, and
the embedded fallback strings show why (two different real timestamps baked in).

**Restore:** re-added `ENV BUILDDATE=202609080000` to `services/cfs/Dockerfile` (`git diff`
confirmed the file returned to exactly the intended three-line addition over the original,
nothing else changed). Both `altavista-cfs-break-1:local` / `-break-2:local` images removed
(`docker rmi -f`).

## 6. Mid-task incident: the official image tag vanished, and restoring it re-confirmed section 4

Between finishing section 4's investigation and starting section 5's break test,
`docker image inspect altavista-cfs-lockstep:local` unexpectedly started returning "No such
image" -- this task never ran `docker rmi` or any prune command against that tag. Several
concurrent `cargo test -p av-kernel ...` processes were observed running on this host at the
time (`ps -Ao pid,etime,command`) -- acceptable under this task's own contention rule, which
only restricts a second concurrent `docker build`. `IMAGE_DIGEST.md`'s own top section already
documents that `crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s `BINDING_KIND_CONTAINER`
tests "separately tag and push this same already-built image to a throwaway local ... registry"
-- the most likely actor, though this was not directly proven (no log captured from whatever
process did it). **Recorded as a process/environment gap in section 8's escalations: this task's
own contention-avoidance rule does not protect against a concurrent test suite mutating an
already-built image this task depends on.**

Restored via `services/cfs/build-image.sh` again (recovery from external interference, not a
second intentional re-pin decision). Full log: `services/cfs/_r4_3_scratch/build_restore.log`.
**Every builder-stage step (1-20) showed `Using cache`** -- direct proof the compiled `/cfs/cpu1`
tree is the exact same bytes as the first R4.3 build, and confirmed explicitly:
`core-cpu1`'s sha256 (`39941a529a802e16327418d699a0dda86374fb64bb56ad81533cb9590d5c8b43`) is
IDENTICAL between the first R4.3 build and this restore build. **The only step that re-executed
was Step 22, the final stage's `RUN apt-get install ... libc6`** -- red-handed confirmation that
this single layer is the sole cause of this particular digest move, exactly the second
non-determinism source named in section 4. New image ID (now the recorded pin):
`sha256:a1303bcef95d870d609c1965d09ec26807030d197ea36d559748e71398081e0a`.
`IMAGE_DIGEST.md` updated accordingly (both the top "Recorded digest" and the R4.3 dated section
now carry a "Mid-task incident" addendum with this full account).

## 7. Final verification (after restore)

- No `docker build` in flight (`ps -Ao pid,etime,command | grep docker build` empty) before
  running.
- `.venv/bin/python -m pytest -q services/cfs/tests/ -rs` (full output:
  `services/cfs/_r4_3_scratch/final_verification.txt`): **19 passed, 1 skipped in 148.14s**,
  exit 0. The 1 skip is `test_image_reproducibility.py`'s own test, visibly naming
  `AV_CFS_RUN_REPRO_BUILD` and `services/cfs/build-image.sh` in its reason -- matches
  `test_image_digest.py`'s existing skip style.
- `docker images | grep -E "repro|break|investigate"` empty -- no throwaway images left behind.
- `git status --short services/cfs/` shows exactly: `Dockerfile` modified,
  `IMAGE_CONTEXT_MANIFEST.txt` modified, `IMAGE_DIGEST.md` modified, plus the two new files
  (`R4_3_REPORT.md`, `tests/test_image_reproducibility.py`) and this task's own
  `_r4_3_scratch/` scratch directory (evidence logs, referenced throughout this report by path;
  left in place for the manager, not committed by me).

## 8. Summary in the required report format

### 1. What was built
- `services/cfs/Dockerfile`: three `ENV` lines pinned in the builder stage (`BUILDDATE`,
  `USER`, `HOSTNAME` -- not `BUILDUSER`/`BUILDHOST`, see section 0's finding).
- `services/cfs/IMAGE_DIGEST.md`: new dated section for question 182 (values, mechanism,
  reasoning, the deeper-finding sub-section, and the mid-task-incident addendum), recorded
  digest and manifest updated.
- `services/cfs/IMAGE_CONTEXT_MANIFEST.txt`: regenerated by `build-image.sh` (38 entries,
  unchanged paths/hashes -- only the header's image-ID comment moved).
- `services/cfs/tests/test_image_reproducibility.py`: new Docker-gated, opt-in-gated
  reproducibility test (option (a): real two-build comparison, reusing
  `test_image_digest.py`'s `docker_available()`).

### 2. Verification (exact)
- Baseline: `services/cfs/_r4_3_scratch/baseline_pytest.txt` -- 19 passed in 43.42s.
- After re-pin, digest test only: `services/cfs/_r4_3_scratch/after_repin_digest_test.txt` --
  2 passed in 0.20s.
- Default full suite after adding the new test file:
  `services/cfs/_r4_3_scratch/after_new_test_default.txt` -- 19 passed, 1 skipped in 48.78s.
- Real two-build run (positive case, pinned Dockerfile):
  `services/cfs/_r4_3_scratch/repro_test_positive_run.txt` -- 1 failed in 540.83s (digests
  `sha256:841a01b1...` vs `sha256:08808106...`; root-caused in section 4, not a fix defect).
- Break test (BUILDDATE removed): `services/cfs/_r4_3_scratch/break_build1.log` /
  `break_build2.log` -- whole-image mismatch, and the clean, isolated `core-cpu1` signal
  (`13fb1865...` vs `6e14d551...`), full account in section 5.
- Final full suite after restore: `services/cfs/_r4_3_scratch/final_verification.txt` -- 19
  passed, 1 skipped in 148.14s.
- Final recorded/verified image ID: `sha256:a1303bcef95d870d609c1965d09ec26807030d197ea36d559748e71398081e0a`.
- Final `core-cpu1` content sha256 (stable across every good build in this task):
  `39941a529a802e16327418d699a0dda86374fb64bb56ad81533cb9590d5c8b43`.

### 3. Measurements worth keeping
- A single `docker build --no-cache` of `services/cfs/Dockerfile` on this host takes roughly
  4.5-9 minutes (540.83s for two builds back-to-back; individual break builds ~14 min apart
  wall-clock but each build itself well under that).
- A cache-hit rebuild (no Dockerfile/content change, warm cache) is fast and reuses every
  builder-stage layer verbatim -- confirmed by the section-6 restore log (`Using cache` for all
  of steps 1-20).
- Of the 143 files `make native_std.install` puts under `/cfs/cpu1`, only 57 are
  runtime-relevant (loaded/executed by this image's own `container-entrypoint.sh` path); the
  other 86 are cFE/OSAL's own bundled unit-test/coverage-harness binaries.

### 4. Defects found, including my own
- **In `IMAGE_DIGEST.md`'s own question-179 write-up (not mine, but I built on it and could
  have silently propagated it):** its recommendation to pin `BUILDUSER`/`BUILDHOST` by those
  exact env-var names is wrong -- `generate_build_env.cmake` reads `$ENV{USER}`/`$ENV{HOSTNAME}`
  for those two, not `$ENV{BUILDUSER}`/`$ENV{BUILDHOST}`. Caught by reading the cmake file's
  full 48 lines instead of trusting the 15-23 excerpt quoted there. Fixed in this task's own
  Dockerfile change (section 0, section 3).
- **Pre-existing, newly discovered, out of this task's scope:** the whole cFS container image is
  not fully reproducible even with the question-182 fix applied -- see section 4's full account
  (linker build-id non-determinism in bundled UT/coverage-harness binaries; independent
  `apt-get install libc6` non-determinism in the final stage). Not something I introduced; not
  something I fixed (would require an image-content change beyond the three variables).
- **My own process gap, not a code defect:** the mid-task image-tag disappearance (section 6)
  cost an extra ~5-minute rebuild and is not fully explained (the likely actor is named but not
  proven from a captured log). No code or test change resulted from this beyond the
  re-recorded digest; flagged as a process gap in section 6/escalations, not silently absorbed.

### 5. Escalations for the manager
1. **Full image-content reproducibility is out of this task's scope but real** (section 4): the
   bundled cFE/OSAL unit-test/coverage-harness binaries (86 files, never executed at runtime)
   carry non-deterministic GNU-linker build-ids, and the final stage's `apt-get install libc6`
   is independently non-deterministic. Both are needed for the whole-image digest itself to be
   reproducible; both are content changes beyond the three pinned variables. Candidate fixes
   (not evaluated in depth, explicitly not decided here): strip/exclude the UT/coverage
   binaries from the shipped image; pass a deterministic build-id flag (e.g.
   `-Wl,--build-id=none` or a fixed value) into cFE's toolchain/CMake configuration; pin apt
   package versions (or vendor a frozen `.deb`) for both `apt-get install` invocations.
2. **`test_image_reproducibility.py` will fail every time it is actually run today**, precisely
   because of escalation #1 -- this is intentional honesty (the test is correct and the image
   genuinely isn't whole-digest-reproducible yet), but the manager should decide whether that's
   an acceptable steady state for an opt-in test, or whether escalation #1 needs to be resolved
   (or the test's scope narrowed to runtime-relevant files only, which is itself a decision I'm
   not making unilaterally) before this test is wired into any CI gate.
3. **`docs/open-questions.md`'s amendment to question 179** (recorded right after question 183,
   dated 2026-09-08) assigns build-artifact manifest marking in
   `test_manifest_paths_exist_and_hash_match` (for `services/cfs/bin/av-lockstep-shim`, untracked
   and currently present on this host, would fail `test_manifest_paths_exist_and_hash_match` on
   a clean checkout) to "R4.3 owns that test" -- not implemented here, since it's outside this
   delegation's own explicit "What to build" list (items 1-4) and touches the same test file for
   a distinct, separately-decided reason. Needs a decision on whether it belongs in this round
   or a follow-up.
4. **Process/environment gap** (section 6): the contention rule ("check for a concurrent
   `docker build` before starting one") does not protect an already-built image tag from being
   retagged/pushed/pruned by a concurrent test suite (most likely
   `crates/av-kernel/tests/drm_attitude_control_cfs.rs`'s `BINDING_KIND_CONTAINER` tests, per
   `IMAGE_DIGEST.md`'s own description of what those tests do to this same image, though not
   proven from a captured log this time). Worth a rule update or a documented "don't run those
   tests while re-pinning this image" convention.

### 6. What remains
- The two out-of-scope non-determinism sources in escalation #1 are unresolved; the officially
  recorded pin (`sha256:a1303bcef95d...`) and `test_image_digest.py` are both internally
  consistent and passing, but a *third* independent rebuild (even without `--no-cache`, if the
  build cache is ever evicted -- exactly what happened mid-task here) would very likely drift
  the pin again for the same, now-understood reasons.
- `test_image_reproducibility.py` is written, gated, and has been run for real in both the good
  and broken cases (this report), but is not part of the default suite and will keep failing
  when opted into until escalation #1 is resolved by a lead decision.
- The question-179-amendment manifest/build-artifact feature (escalation #3) is not implemented.
- Nothing outside `services/cfs/` was touched by this task.

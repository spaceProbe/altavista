# R6.3 report -- visible gating for every Docker- or image-gated test (question 194)

Team 2, round 6, task R6.3. Written incrementally as work proceeds.

## 0. Environment notes

- `docker info` succeeds on this host; `altavista-cfs-lockstep:local` and `ubuntu:22.04` are
  NOT present (host-level prune, per the task brief). `registry:2` and `python:3.13-slim` ARE
  present locally.
- No concurrent `cargo test`/`cargo build`/`docker build` from the other worker observed before
  any run in this task (checked via `ps -Ao pid,etime,command` before every build/test/docker
  invocation below).

## 1. Visibility measurement (done FIRST, before designing the mechanism)

**Hypothesis, stated before measuring:** `println!`/`eprintln!` are captured by libtest and
invisible for a passing test in a plain `cargo test` run (no `--nocapture`) -- already disclosed
in this repo's own `crates/av-lockstep/tests/docker_lifecycle.rs` module doc comment and
`docs/open-questions.md` question 194's own framing. A raw write to the real stderr file
descriptor via `std::io::stderr().write_all(...)` (NOT through the `eprintln!`/`eprint!` macros)
bypasses libtest's output-capture hook, which is wired into the `print!`/`eprintln!` macro
helper functions (`io::_print`/`io::_eprint`), not into `Stdout`/`Stderr`'s own `Write` impl --
so it should remain visible even for a passing test.

**Measurement.** Three temporary probe tests were added to
`crates/av-lockstep/tests/docker_lifecycle.rs` (`r63_probe_println`, `r63_probe_eprintln`,
`r63_probe_raw_stderr_write`), each printing/writing a unique marker and then passing. Run:

```
cargo test -p av-lockstep --test docker_lifecycle -- r63_probe
```

(no `--nocapture`). Full output saved to
`/private/tmp/claude-501/-Users-probe-code-AltaVista/ed238b37-d349-42f1-956b-22807101027f/scratchpad/r6_3/visibility_measurement_plain.txt`.
**Literal captured output:**

```
running 3 tests
R63_PROBE_RAW_STDERR_MARKER
test r63_probe_eprintln ... ok
test r63_probe_println ... ok
test r63_probe_raw_stderr_write ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s
```

**Result:** `R63_PROBE_PRINTLN_MARKER` and `R63_PROBE_EPRINTLN_MARKER` do NOT appear anywhere.
`R63_PROBE_RAW_STDERR_MARKER` DOES appear (printed before libtest's own "running 3 tests" line,
since it is an unbuffered real-fd write, asynchronous with respect to libtest's own captured
output). Hypothesis confirmed exactly. The probe tests were removed after this measurement
(`git diff` on `docker_lifecycle.rs` reflects only the final feature, not the probes).

**Mechanism chosen:** the gating helper's visible-skip announcement is written via
`std::io::stderr().write_all(...)`, never `println!`/`eprintln!`.

## 2. What was built

### 2.1 The typed gate helper (`crates/av-lockstep/src/docker.rs`)

Chosen location: `av_lockstep::docker` is already a normal `[dependencies]` entry of
`av-kernel` and a `[dev-dependencies]` entry of `av-lockstep-shim` (checked before writing any
code -- both crates already call `av_lockstep::docker::{docker_available, prune_stale_test_
resources, test_label_args, test_run_id}` today), so **no `Cargo.toml` edit was needed at all**
to make the new helper reachable from every gated test file in scope. No new workspace member,
no new dependency.

Added to `crates/av-lockstep/src/docker.rs`:

- `DockerGateReason` (item 1): a `#[derive(Debug, Clone, PartialEq, Eq)]` enum, not a bare
  `String` -- `DockerNotInstalled`, `DockerDaemonUnreachable { detail }`, `ImageNotBuilt {
  image_ref, build_hint }`, `RequiredFileMissing { what, path }`. `.message()` /
  `impl Display` render a human-readable line naming the resource and how to obtain it, at
  least as informative as every pre-R6.3 hand-formatted string it replaces.
- `docker_daemon_status() -> Result<(), DockerGateReason>` -- distinguishes "not installed" from
  "installed, daemon unreachable" (the pre-R6.3 `docker_available()` conflated both into one
  `bool`; kept as a thin wrapper over this for every existing call site).
- `IMAGE_OVERRIDE_ENV = "AV_DOCKER_TEST_IMAGE_OVERRIDE"` (item 5) + `resolve_image_ref` +
  `image_gate_status(default_image_ref, build_hint)` -- the image-absence branch is now provable
  deterministically by pointing the override at a name that cannot exist, without ever touching
  a real tag.
- `REQUIRE_DOCKER_TESTS_ENV = "AV_REQUIRE_DOCKER_TESTS"` (item 4) + `require_docker_tests()`.
- `announce_gate_skip(test_name, reason) -> String` (items 2+3): writes `"SKIPPED {test_name}:
  {reason}\n"` via a raw `std::io::stderr().write_all(...)` (never `println!`/`eprintln!` --
  see section 1's measurement), returns the exact line written, and panics instead of returning
  when `AV_REQUIRE_DOCKER_TESTS` is set.

### 2.2 The `docker rmi -f <ID>` hazard fix (item 6)

`prune_stale_test_resources`'s image half now calls `docker images --filter label=... --format
'{{.ID}}\t{{.Repository}}:{{.Tag}}'` (previously `-q`, IDs only) and delegates the
removal-target decision to a new pure function `image_removal_targets(images_tsv) -> Vec<String>`
(unit-tested without Docker, mirroring `build_run_args`'s own existing pattern): one `docker rmi
-f` call per distinct **reference** (`<repository>:<tag>`), never a bare image ID, except for a
truly dangling (`<none>:<none>`) row, which falls back to its own ID (safe there: no other tag
exists to strip).

### 2.3 Real-Docker measurement that motivated the fix (manager's own probe, reproduced)

Hypothesis stated before measuring: `docker rmi -f <IMAGE ID>` removes every repository:tag
reference pointing at that image, not only the one a filtered `docker images` query returned it
for. Reproduced directly on this host (not merely trusted from the brief):

```
docker tag python:3.13-slim av-r63-probe:tag1
docker tag python:3.13-slim av-r63-probe:tag2
docker build -q -t av-r63-probe-labeled:tag1 --label av.r63.probe=1 - <<< 'FROM python:3.13-slim'
docker tag av-r63-probe-labeled:tag1 av-r63-probe-labeled:tag2
docker images -q --filter label=av.r63.probe
  -> 363952b07d42
     363952b07d42          # SAME id, once per matching tag row -- NOT deduplicated
docker images --filter label=av.r63.probe --format '{{.ID}}\t{{.Repository}}:{{.Tag}}'
  -> 363952b07d42  av-r63-probe-labeled:tag1
     363952b07d42  av-r63-probe-labeled:tag2
docker images --filter label=av.r63.probe --format '{{.Repository}}:{{.Tag}}' | grep -c 'av-r63-probe:'
  -> 0   # the UNLABELED python:3.13-slim alias tags never match the label filter at all
```

Confirms: (a) a labelled image's OWN tags both show up under the label filter (expected, both
legitimately swept either way); (b) an unrelated tag pointing at a *different* image ID is
correctly excluded by the label filter regardless of ID-based or reference-based removal.
**Honest disclosure, not overclaimed:** because Docker's label filtering matches by the
underlying image ID (any tag aliasing a labelled ID shows the same label), a same-process
regression test built purely on `docker images --filter label=...` output cannot make the OLD
(ID-based) and NEW (reference-based) implementations disagree on *which tags* end up removed in
every constructible scenario on this Docker version -- both discover the same tag set through
that query. The concrete, executed difference this task proves instead is at the operation-shape
level: `image_removal_targets` (the pure function the fix now goes through) issues one `docker
rmi -f <reference>` per distinct tag, never collapsing to a shared bare ID -- proven by a real
break-and-restore (section 4) -- which is strictly the documented-safer Docker operation (`docker
rmi <tag>` untags exactly that one reference; `docker rmi -f <ID>` is Docker's own documented
"remove this image and every tag pointing at it" operation). This matches the brief's literal
instruction ("make the sweep remove references rather than force-remove IDs") and removes the
latent hazard as specified; it is not a claim that this host's separately-investigated vanished
images (traced to a bulk host-level prune, per question 194's own record) are explained by this
function.

## 3. Verification

- `cargo build -p av-lockstep --lib`: clean (no warnings), log:
  `scratchpad/r6_3/build_av_lockstep_lib.txt`.
- `cargo test -p av-lockstep --lib -- docker::` : **14 passed, 0 failed** (log:
  `scratchpad/r6_3/unit_tests_restored.txt`), covering `image_removal_targets` (4 tests),
  `DockerGateReason`/`announce_gate_skip`/`resolve_image_ref`/`image_gate_status`/
  `require_docker_tests` (6 tests), plus the 4 pre-existing `build_run_args`/`test_label_args`
  tests (unaffected, still green).
- Break-and-restore for `image_removal_targets_removes_by_reference_not_a_shared_bare_id` and
  `image_removal_targets_handles_multiple_distinct_images`: EXECUTED. Reverted
  `image_removal_targets` to the pre-fix bare-ID-collapsing shape, ran
  `cargo test -p av-lockstep --lib -- docker::tests::image_removal_targets`: **2 passed, 2
  FAILED** (real captured panic text: `left: ["363952b07d42"] right:
  ["av-r63-probe-labeled:tag1", "av-r63-probe-labeled:tag2"]`, log:
  `scratchpad/r6_3/break_and_restore_image_removal_targets.txt`). Restored the fix, re-ran: **14
  passed, 0 failed** (log: `scratchpad/r6_3/unit_tests_restored.txt`). `git diff --stat --
  crates/av-lockstep/src/docker.rs` after restore shows only the intended, final diff (checked:
  the break/restore left no residue).
- Visibility probe (section 1): added, measured, removed;
  `git diff --stat -- crates/av-lockstep/tests/docker_lifecycle.rs` is empty (confirmed via
  `git diff` before any of this task's real edits to that file).

## 4. Defects found, including own

- The pre-R6.3 `prune_stale_test_resources` removed images via `docker rmi -f <bare ID>` from a
  `docker images -q --filter label=...` listing -- a latent hazard per question 194 item 6 (see
  section 2.3). Fixed.
- Own: the first draft of the two-tag regression scenario assumed an UNLABELED sibling tag
  pointing at the same image ID as a labelled one could be constructed to discriminate old vs.
  new removal behaviour end to end against a real daemon. Measured directly (section 2.3) that
  Docker's label filtering matches by image ID, so no such sibling can exist while still
  matching the filter query both implementations read from -- disclosed rather than shipping a
  misleading "proves the fix" claim; the actually-discriminating test is the pure-function
  break-and-restore instead (section 3).

## 5. Escalations for the manager

(none yet -- filled in as remaining items are completed)

## 6. What remains, in priority order

1. Route every gated test file in scope through `announce_gate_skip`, asserting on the returned
   text (`drm_attitude_control_cfs.rs`, `drm_attitude_control_renode.rs`, `drm_container.rs`,
   `docker_lifecycle.rs`, `end_to_end_kernel_path.rs`).
2. A dedicated visibility-proof test: spawn `cargo test` as a real subprocess against a gated
   test known to skip, inspect its real captured stdout for the `SKIPPED` line.
3. Verify `services/cfs/tests/test_image_digest.py`/`test_image_reproducibility.py`'s existing
   `pytest.skip`/`-rs` visibility claim with a real run (task explicitly asks this be checked,
   not rewritten unless it fails the standard).
4. `cargo clippy` on touched crates.
5. Full report polish.

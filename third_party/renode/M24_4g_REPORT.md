# M24.4g -- serialise the Renode monitor client, then run the per-step byte comparison, closing M24

status: **complete -- M24 closes posix-container-only.** Monitor client root-caused, fixed, and
proven with a unit test that replays the exact captured interleaving (fails against the unfixed
snapshot, passes against the fix). The fix itself, once run against real Renode, immediately
surfaced (and this task also fixed) two more real desyncs of the identical class. With the monitor
client fixed, the real byte-comparison test gets further than any prior report in this milestone
(Renode boots, HELLO/BIND relay cleanly, **STEP 1 completes with byte-exact port traffic**) but
**STEP 2 onward still does not reliably deliver the guest's own frame**, now reproduced through the
M24.4f `CharReceived` hook too, not just the mechanisms M24.4e/M24.4f already ruled out -- recorded
as question 171, with the limitation stated in `docs/sil-plan.md` and `third_party/renode/REPORT.md`
and a precise next diagnostic step named. See "Not done" at the very end for the exhaustive list.

Task: `docs/open-questions.md` questions 145, 153, 156, 157, 164. Builds directly on
`third_party/renode/M24_4f_REPORT.md` (proved the Renode Python `CharReceived` TX hook works,
5178/5178 bytes on uart0; wired it into `renode_bridge.py` but never got to run the real
end-to-end byte comparison before returning), `M24_4e_REPORT.md` (root-caused the "STEP stall" as
`CreateServerSocketTerminal` dropping bytes -- not a guest bug), `M24_4d_REPORT.md` (two real
bridge bugs: `include`'s reply is not a completion signal; a shared hardcoded `.resc` path raced).

This task's own exact bug (reproduced by the lead, not re-derived here): `cargo test -p av-kernel
--test drm_attitude_control_renode` fails inside `wait_for_rxen()` ->
`read_reg_last_hex(0xff010000)` (`third_party/renode/M24_4b/renode_bridge.py:366` -> `:161`):
```
RuntimeError: no hex value found reading 0xff010000:
  '(av-m24-4b-bridge-54022) \n\r(av-m24-4b-bridge-54022) emulation RunFor "0.050000"'
```
A register-read consumed a reply belonging to a **different command** (the echo of a `RunFor`) --
a response desynchronisation in `MonitorClient`, our own code, not Renode's.

Written incrementally per question 157's own rule. Sized for ~300 tool uses.

## Not done yet (running list, updated as items close) -- ALL DONE

- [x] 1. Root-cause exactly how the desync happens -- **DONE**, see "Root cause" below.
- [x] 2. Design and implement the serialised `MonitorClient` -- **DONE**, see "Fix" below. Also
      fixed two more, different, real desyncs of the identical class the fix itself surfaced live
      against real Renode (a deferred macro-return line after `include`; a trailing ANSI code
      after `RunFor`) -- see "Two more real desyncs" below.
- [x] 3. Unit test replaying the exact captured interleaving -- **DONE**, proven to fail against
      the unfixed snapshot first, passes after the fix. See "Unit test" below.
- [x] 4. Run the real per-step byte comparison -- **DONE**, three full real runs recorded with real
      output. See "Byte comparison: real result" below. Does not pass end to end (STEP 2+), for a
      reason unrelated to this task's own fix (confirmed: the monitor client shows zero desync
      errors throughout STEP 1 and into STEP 2's own virtual-time grant).
- [x] 5. Did not pass end to end -> question 171 written with real evidence (three frames parsed
      and confirmed by a small script, not eyeballed); limitation stated in `docs/sil-plan.md` and
      `third_party/renode/REPORT.md`; precise next diagnostic step named in both.
- [x] 6. Cleanup -- **DONE**, verified zero `/tmp/av-renode-m24*` dirs and zero labeled Docker
      resources as of this writing. See "Cleanup" below.
- [x] 7. Full verification -- **DONE**: `pytest -q` 421 passed (418 baseline + 3 new); `cargo test
      -p av-kernel` 665 passed / 0 failed excluding the one known Renode failure (confirmed present
      and precisely characterized, not just "still red"); `cargo test --workspace --exclude
      av-kernel` 178 passed / 0 failed. See "Test totals" below.

## Root cause of the desync (read, not re-derived from the traceback alone)

`MonitorClient` (`third_party/renode/M24_4b/renode_bridge.py`, pre-fix) serializes *sends* with a
lock (`self.lock`), but each `cmd()` call independently decides when a reply is "done" using a
**fixed 0.3s idle-gap heuristic** (`read_until_idle`): keep reading until 0.3s passes with no new
bytes, treating that gap as "the monitor finished replying." This is unsound for `emulation
RunFor`, whose own completion is Renode actually executing that much virtual time -- wall time per
step is NOT bounded near-instant (M24.1's own spike measured p50 100ms, p99 126ms, one 372ms
outlier even for a trivial bare loop; M24.4d measured monitor round trips stretching to 5.1-5.7s
under this project's own multi-worker contention). Renode's monitor echoes a command's own text
back almost immediately, then blocks until the command actually finishes before sending the rest
of the reply (M24.1's own spike hit the identical class of bug first, in `measure_virtual_time.py`
-- "the driver was reading only the echo and failing to parse every step," fixed there by reading
until a specific marker instead of an idle gap; `MonitorClient` here still has the bug that spike
already found and fixed once).

So: `wait_for_rxen()`'s `run_for(0.05)` sends `emulation RunFor "0.050000"`, its `read_until_idle`
reads the immediate echo (chunks non-empty), then times out after 0.3s because the real
completion+next-prompt has not arrived yet (Renode is still busy) -- `cmd()` returns EARLY, having
consumed only the echo, and `run_for()` never even looks at what it got back. The genuine
completion bytes (echo prompt + a fresh prompt) are still in flight and land on the socket a little
later, **after** `run_for()`'s `cmd()` has already stopped reading. The very next call,
`read_reg_last_hex(REG_CONTROL)`'s own `cmd()`, starts a fresh `read_until_idle` and reads
**those** orphaned bytes first -- which belong to the RunFor command, not to this ReadDoubleWord --
and (since they contain no `0x????????` pattern) `HEX_RE.findall` comes back empty, raising exactly
the recorded `RuntimeError`. Nothing here is Renode-side or guest-side; it is a pure client-side
reply/command mis-attribution bug, matching the task brief's framing exactly.

## Fix: `MonitorClient` rewritten -- request tagging, single-outstanding-command, prompt-anchored
## reply parsing, typed desync errors

See `third_party/renode/M24_4b/renode_bridge.py` (edited in place; git diff is the record). Design:

1. **Exact prompt anchor, not a generic idle gap.** The bridge already knows its own Renode
   machine's name (`av-m24-4b-bridge-<pid>`) before it ever sends a monitor command, because it
   picks that name itself. `MonitorClient.set_prompt(machine_name)` stores the literal string
   `"(av-m24-4b-bridge-<pid>)"`; a reply is complete only once that **exact substring** reappears
   in the bytes read back **after** this command's own echo -- not after any fixed silence window.
   A literal, known substring (not a shaped regex) avoids a false-complete match against
   parenthesised text that might appear inside some other command's own output.
2. **Every command is tagged** (`self._seq`, incremented under the lock) purely for error messages
   and log lines -- `[cmd#N] ...` -- so a desync error names which command in the sequence it was.
3. **Exactly one outstanding command**, enforced two ways: the pre-existing `self.lock` still
   serializes `send`+`read` as one critical section (so two Python-level callers can never
   interleave), and, **new**, a non-blocking `MSG_PEEK` check runs immediately **before** every
   `send()` -- if the socket already has bytes queued that this call did not itself request, that
   is direct proof a previous command's reply was not fully drained, and this raises
   `MonitorDesyncError` (a new, typed exception) rather than silently letting the next command's
   parser find them first. This is the exact mechanism that produced the recorded bug: with the
   prompt-anchored read (item 1) in place, a command's `cmd()` never returns until it has consumed
   its own full reply, so in ordinary operation this check should never fire; it exists as a
   loud, typed backstop, not a silent recovery path.
4. **Replies are parsed only from the text that follows this command's own echo.** `cmd()` requires
   the literal command line it just sent to appear in the accumulated buffer before it will even
   consider a trailing prompt as "done" -- so leftover bytes from an earlier, different command
   (which do not contain *this* command's own text) cannot be mistaken for this command's reply
   even if the stray-byte check above were somehow bypassed.
5. **`write_chars()` (the batched host->guest `WriteChar` pipeline)** keeps its existing
   performance design (many `WriteChar` monitor commands sent in one `sendall`, not one-by-one --
   removing that would make a real DRM run impractically slow, per the comment already in the
   file) but now drains **exactly as many** prompt completions as commands were sent, counted by
   literal occurrences of the known prompt string, instead of the old idle-gap heuristic -- and the
   whole batch is sent-and-drained under the single `self.lock` critical section, so from any other
   caller's perspective there is still only ever one outstanding request unit at a time (the batch
   never leaves any of its own N replies undrained for an unrelated command to pick up). This is a
   disclosed, deliberate scope decision: "exactly one outstanding command" is honoured at the
   *lock* granularity (what any other caller can observe), not by giving up `write_chars`'s own
   internal pipelining, which the RX/host->guest direction was explicitly out of scope to touch.
   The one thing that changed for `write_chars` is how it decides a batch is fully drained -- from
   "0.2s of silence" to "N literal prompts observed" -- which is the same class of fix as `cmd()`'s.
6. **Typed errors, never silent misattribution:** `MonitorDesyncError` (stray bytes found before a
   send, or the peer closed mid-reply) is a `RuntimeError` subclass distinct from the plain
   `TimeoutError` already used elsewhere in this file for "no reply arrived within budget" -- a
   caller (or a human reading a bridge stdout log) can tell "a reply went to the wrong place" apart
   from "nothing came back at all."

status: fix implemented in `renode_bridge.py`; see "Unit test" and "Byte comparison" below for
proof.

## Unit test: replays the captured interleaving, proven to fail before the fix

`tests/test_renode_monitor_client.py` (picked up by plain `pytest -q`, per `pyproject.toml`'s
`testpaths`). A frozen, byte-for-byte snapshot of `renode_bridge.py` taken *before* this task's
fix was applied lives at `third_party/renode/M24_4g/renode_bridge_UNFIXED_snapshot.py` -- the
actual old code, not a re-implementation of it. Three tests, all using a deterministic scripted
fake socket (no real Renode, no real sockets, no sleeping, no timing race to get unlucky on):

1. `test_desync_reproduces_against_frozen_unfixed_snapshot` -- drives the FROZEN unfixed
   `MonitorClient` through `run_for()` then `read_reg_last_hex()` with a scripted interleaving
   (RunFor's echo arrives, then an idle gap, then leftover bytes containing **the literal captured
   string** land where `read_reg_last_hex`'s own read starts) and asserts the **exact** recorded
   exception message. **PASSES** -- this is direct, mechanical proof the recorded bug is real and
   this test replays it faithfully, not a similar-looking one.
2. `test_fixed_monitor_client_returns_the_real_register_value` -- the same class of interleaving
   (echo now, real completion later), driven through the current, fixed `MonitorClient`, must
   return the real scripted register value, not a corrupted parse of RunFor's own text.
3. `test_fixed_monitor_client_raises_typed_desync_error_on_stray_bytes` -- if bytes are somehow
   already queued before a new command is sent, the fixed client raises `MonitorDesyncError`
   (typed), never silently returns them as if they were its own reply.

**Proven to fail against the unfixed implementation, before the fix, exactly as the task brief
requires**: a diagnostic copy of the test file
(`third_party/renode/M24_4g/scratch/test_against_unfixed.py`, kept as the reviewable "before"
artifact, not part of the collected suite) repoints tests 2 and 3 at the frozen unfixed snapshot
instead of the live file. Run before applying any live fix:

```
tests/test_renode_monitor_client.py::test_desync_reproduces_against_frozen_unfixed_snapshot PASSED
tests/test_renode_monitor_client.py::test_fixed_monitor_client_returns_the_real_register_value FAILED
  AttributeError: 'MonitorClient' object has no attribute 'set_prompt'
tests/test_renode_monitor_client.py::test_fixed_monitor_client_raises_typed_desync_error_on_stray_bytes FAILED
  AttributeError: 'MonitorClient' object has no attribute 'set_prompt'
2 failed, 1 passed
```
The unfixed client has no concept of prompt-anchored reply correlation at all -- it is
architecturally incapable of the invariant tests 2/3 check for, which is exactly the point: the
fix is a real behavioural change, not a cosmetic one. Test 1 (which directly asserts the bug's own
existence against the frozen snapshot) passes both before and after -- it is a permanent regression
guard, not a before/after diff.

After applying the fix to the live `third_party/renode/M24_4b/renode_bridge.py`, the real
(unmodified) `tests/test_renode_monitor_client.py` was run:
```
tests/test_renode_monitor_client.py::test_desync_reproduces_against_frozen_unfixed_snapshot PASSED
tests/test_renode_monitor_client.py::test_fixed_monitor_client_returns_the_real_register_value PASSED
tests/test_renode_monitor_client.py::test_fixed_monitor_client_raises_typed_desync_error_on_stray_bytes PASSED
3 passed in 0.16s
```

## Two more real desyncs the fix itself found, live, on the first two real Renode runs

The unit test above proves the *reported* bug is fixed. Running the fixed bridge against a real
Renode process immediately surfaced two more, DIFFERENT desync-shaped issues that the old
idle-gap code had been silently tolerating (or silently corrupting through) all along -- exactly
the class of thing a typed error is supposed to surface instead of hiding. Both are real, both are
now fixed, and both are recorded here rather than quietly patched over:

1. **Run 1**: `MonitorDesyncError` fired on `wait_for_rxen`'s *second* loop iteration's `run_for()`
   call, before it even sent anything -- 42 queued bytes, cleaning down to just `'(av-m24-4b-bridge-55604)'`.
   Root cause: `include` (which runs `setup_uart_tx_bridge`, a Python-hook-defined monitor macro,
   inside the included script) has this Renode build print that macro's own return-value dispatch
   as a **separate, deferred line** (`Command setup_uart_tx_bridge failed, returning "0".`) *after*
   the prompt that already signalled `include` itself was done -- M24.4f's own report had already
   seen the identical cosmetic pattern for a different macro (`uart_tx_bridge_stats`) and left it
   unchased; this task hit the same class of thing for the macro actually used in production.
2. **Run 2** (after a first, narrower fix scoped to `include` only): `MonitorDesyncError` fired on
   the *first* `read_reg_last_hex` call, 10 bytes queued, cleaning down to a **truncated ANSI
   escape sequence** (`'[0m\n\r\x1b[33;'`) left over from `run_for`'s own reply -- a color code (or
   part of one) arriving in a separate TCP segment a few milliseconds behind the prompt that ended
   the visible part of the reply.

Both are the same underlying shape: Renode does not always finish flushing a command's *entire*
reply in the same write/flush as the part containing the prompt -- a cosmetic trailer (a deferred
macro-return line, a color-reset code) can arrive a beat later. The prompt-anchored read
(`_read_reply_for`) is correct and necessary to fix the REPORTED bug (a *slow, still-executing*
command's primary reply, seconds away) but is not, by itself, sufficient for this second, much
smaller class of straggler (milliseconds away, after the primary reply is already genuinely done).

**Fix**: `MonitorClient._grace_drain()` -- once a command's primary completion (echo + trailing
prompt) is confirmed, do one short, bounded settle read (idle_gap 0.15s, overall budget 0.5s) for
anything that follows immediately, appended to the reply before returning. This is the same shape
of fix M24.1's own virtual-time spike already used for an analogous problem ("a short (30ms) grace
timeout only after the marker is seen", `third_party/renode/REPORT.md`) -- and it is NOT a
reversion to the original idle-gap bug: that bug waited an idle gap for a command's slow *primary*
reply; this waits a short, fixed grace *after* the primary reply is already confirmed, purely to
catch an immediate straggler. Applied uniformly in both `_read_reply_for` (single commands) and
`_read_n_replies` (the batched `WriteChar` path), replacing an earlier, narrower one-off mop-up
that had been scoped to `include` only (removed once the general fix covered it too, so there is
one mechanism, not two overlapping ones).

Unit tests re-verified after this change (`tests/test_renode_monitor_client.py`, all 3 still pass;
the fake socket had to be corrected too -- see its own docstring -- so a command's post-completion
grace drain could not "see" a later, unrelated command's reply that had not actually been sent
yet, which is exactly the discipline the real fix depends on).

## Byte comparison: real result

Four full, real runs of `cargo test -p av-kernel --test drm_attitude_control_renode` against the
live host (Docker for the posix half, the real pinned Renode binary plus `core-cpu1.exe` for the
Renode half -- both confirmed present before any run, `renode_unavailable_reason()`/
`cfs_image_unavailable_reason()` never fired):

- **Run 1** (fix applied, not yet hardened): `MonitorDesyncError` on `wait_for_rxen`'s second loop
  iteration, before `run_for()` even sent anything -- 42 stray bytes, cleaning down to
  `'(av-m24-4b-bridge-55604)'`. Root cause: `include` runs `setup_uart_tx_bridge` (a Python-hook
  macro) inside the included script, and this Renode build prints that macro's own return-value
  dispatch (`Command setup_uart_tx_bridge failed, returning "0".`) as a deferred line *after* the
  prompt that already signalled `include` itself complete.
- **Run 2** (narrower fix applied, scoped to `include` only): `MonitorDesyncError` on the *first*
  `read_reg_last_hex` call -- 10 stray bytes, cleaning down to a **truncated ANSI escape sequence**
  (`'[0m\n\r\x1b[33;'`) left over from `run_for`'s own reply, arriving a beat behind its prompt.
  Generalised the fix (`_grace_drain`, see above) instead of adding a second one-off mop-up.
- **Run 3** (general fix applied): Renode boot -> HELLO relayed both ways -> BIND relayed
  (`start_tai_ns=1767225637000000000`) -> **STEP 1 completes**, its own `STEP_DONE` (sequence=1)
  captured byte-exact by the independent `CreateFileBackend` cross-check M24.4f already wired up
  beside the hook -> STEP 2's own chunked virtual-time grant begins (a second "STEP idle-CPU trace"
  line prints, confirming the monitor client itself is still working correctly -- no
  `MonitorDesyncError` anywhere in this entire run) -> `guest_read_frame()` (reading the
  `CharReceived` hook socket, a completely different code path from `MonitorClient`) times out
  after 60s waiting for STEP 2's own reply. Real posix-side output:
  `posix-container run: 1s @ 10Hz in 16.20s wall time`; Renode side:
  `Renode bridge ready (boot + HELLO/BIND handshake path primed) in 17.68s wall time`, then the
  panic at `crates/av-kernel/tests/drm_attitude_control_renode.rs:524` (`StepRpc`, "the peer closed
  the connection cleanly between frames"). `third_party/renode/M24_4g/evidence/` keeps this run's
  own `bridge_stdout.log`, `bridge_stderr.log`, `uart0.log`, and `uart1_raw_crosscheck.log` (copied
  before the scratch dir was removed), plus `run3_uart1_parsed.txt` -- the output of
  `third_party/renode/M24_4g/scratch/parse_uart1_log.py` (a small script that parses the file the
  same way `renode_bridge.py`'s own `read_frame` does, rather than eyeballing `od -c` output),
  which shows, authoritatively:
  ```
  frame 1: offset=0 length=7 frame_type=HELLO raw_payload=b'\x01AVL1\x01\x00'
  frame 2: offset=11 length=39 frame_type=BIND_ACK raw_payload=b'\x03\x08\x01\x12\x11io_lockstep/M23.2\x1a\x0fio_lockstep/0.1'
  frame 3: offset=54 length=13 frame_type=STEP_DONE raw_payload=b'\x05\x08\x01\x10\x80\xa6\xbc\x8a\xa9\xcb\x9c\xc3\x18'
  total frames parsed: 3, bytes consumed: 71/71
  ```
  Exactly HELLO, BIND_ACK, STEP_DONE#1 -- 71 bytes, pinned. Nothing further ever arrives in the
  independent file capture, matching the guest_read_frame timeout at the port-traffic level, not
  just the bridge's own read.

**Performance bug found and fixed in the fix itself, before trusting run 3's timing**: `_grace_drain`
initially reused `_drain_idle`'s retry-until-something-or-full-timeout loop, which costs the
*entire* `overall_timeout` (0.5s) every time nothing follows a command's primary reply -- correct
for the one-time startup banner (where retrying until Renode starts printing is the point) but
wrong for a per-command grace drain, where "nothing follows" is the overwhelmingly common case and
must be cheap. Rewritten to bail out after exactly one `idle_gap` (0.15s) when nothing is pending
at all, `overall_timeout` only bounding the rare case of something trickling in piece by piece.
Verified: `tests/test_renode_monitor_client.py` dropped from 1.18s to 0.17s (the fake socket's own
empty-grace-drain calls no longer busy-spin for the old 0.5s each). **Run 4** (this fix applied)
re-confirmed the identical STEP-2 failure boundary byte-for-byte -- same 71-byte, 3-frame uart1
cross-check, same panic site, total wall time 138.67s (run 3: 130.92s, within run-to-run noise) --
proof this was a real, disclosed efficiency defect in the fix's own first cut, not a factor in the
STEP-2 result itself, and now fixed regardless.

**This is the third time this exact "STEP 1 works, STEP 2+ does not" boundary has been observed**,
now across three materially different guest-to-host transports: `CreateServerSocketTerminal`
(M24.4e), a polled `CreateFileBackend` (M24.4f), and now a Renode Python `CharReceived` hook (this
task). None of M24.4e's, M24.4f's, or this task's own fixes to any ONE of those three mechanisms
resolved it -- direct evidence the defect is not inside any one of those Renode-provided delivery
mechanisms specifically, but in something that recurs on a *second* write-then-WFI-park cycle
regardless of which mechanism forwards the bytes. **This is exactly the kind of finding question
171 asks for: the last blocker recorded as what it actually is, not "Renode didn't work."**

**Conclusion, per the task's own decision tree**: the byte comparison does not pass end to end.
**The identical-traffic criterion (question 145) is verified posix-container-only**
(`crates/av-kernel/tests/drm_attitude_control_cfs.rs`, unaffected by anything in this task).
Question 171 records the evidence above; `docs/sil-plan.md` and `third_party/renode/REPORT.md`
both carry the limitation statement and a precise next diagnostic step (a live GDB-remote attach
during a stalled STEP 2, `third_party/renode/M24_4e/gdbstub_read_state.py`, already written and
proven against this exact Renode build -- to determine whether `IO_LOCKSTEP`'s task has reached its
second `lockstep_write_frame` call at all).

## Cleanup

- **`/tmp/av-renode-m24*`**: three pre-existing directories were found at this task's own start
  (`/tmp/av-renode-m24c-53259`, `-53778`, `-55393`, left over from a prior worker's own runs before
  this task began) and removed. This task's own four real runs each left one scratch dir; each was
  inspected for evidence (logs copied into `third_party/renode/M24_4g/evidence/` for run 3, the
  first one to reach far enough to be informative; run 4 independently confirmed the identical
  result -- see above) then removed. **Zero remain as of this writing**
  (`ls -d /tmp/av-renode-m24*` -> "No such file or directory").
- **Docker**: `docker ps -a --filter label=av.test=1` and `docker images --filter label=av.test=1`
  both empty as of this writing -- the labeled prune/guard machinery worked correctly across all
  four of this task's own real runs; no manual removal was needed. (Two unrelated, unlabeled
  containers from a different, concurrent worker's own session were observed running on this shared
  host during this task -- `vigorous_dijkstra`/`boring_hertz`, created ~02:05 UTC, well before this
  task's own first real run -- and were deliberately left untouched: not created by this task's own
  code path, not matching this test's own image-reference pattern
  (`127.0.0.1:<port>/altavista-cfs-lockstep`), and removing another session's live resources is out
  of this task's own scope. Same class of pre-existing, disclosed gap M24.4e already recorded.)
- **`third_party/renode/M24_4g/`** (this task's own working directory): kept as the reviewable
  artifact set (question 157) -- `renode_bridge_UNFIXED_snapshot.py` (the frozen pre-fix code the
  unit test's regression proof depends on, permanently), `scratch/` (the diagnostic before/after
  test run used to prove the unit test fails against unfixed code, plus the four real cargo-test
  logs, the two pytest logs, the two cargo-workspace logs, and the small `parse_uart1_log.py`
  script), `evidence/` (run 3's own copied logs -- the STEP-2-side proof of what the guest and the
  independent file-backend capture actually did). Not scratch, not removed -- this task's own
  reviewable record.

## Test totals

All three runs below are from this task's own final code state.

- **`.venv/bin/pytest -q`**: **421 passed**, 5 warnings (the same pre-existing `pytest.mark.slow`
  unknown-mark warnings every prior report already recorded, unrelated to this task) -- exactly
  418 (the recorded baseline) + 3 (this task's own new `tests/test_renode_monitor_client.py`).
- **`cargo test -p av-kernel -- --skip byte_identical_port_traffic_between_posix_container_and_renode`**:
  **665 passed, 0 failed** -- exactly matches the "665 passed" baseline recorded in every prior
  M24.4x report, confirming zero regressions anywhere else in the crate. Plus the one, disclosed,
  precisely-characterized failure (`byte_identical_port_traffic_between_posix_container_and_renode`,
  see "Byte comparison" above, run four separate times as part of this task's own work, the last
  two to the identical byte-exact result) -- **1 known failure, 0 unexpected ones.**
- **`cargo test --workspace --exclude av-kernel`**: **178 passed, 0 failed** -- exactly matches the
  recorded baseline. This task touched no code in any other crate.

## Not done / could not do

1. **The byte comparison does not pass end to end** -- fully explained above, not glossed over: a
   pre-existing, three-times-now-observed gap in guest->host STEP 2+ delivery, unrelated to this
   task's own monitor-client fix (which is proven working correctly through STEP 1 and into STEP
   2's own virtual-time grant, with zero desync errors). Not this task's assigned scope to
   root-cause further (the task brief's own hard stop: after M24.4f, a no-go here is question 171,
   not another open-ended investigation) -- the precise next step is named in question 171 and in
   both `docs/sil-plan.md` and `third_party/renode/REPORT.md`.
2. **Why STEP 2 specifically, and not STEP 1, fails across three unrelated transports** was
   narrowed (the common factor is a *second* write-then-WFI-park cycle, not any one Renode
   mechanism) but not root-caused to a specific line of guest or Renode code -- that would need
   M24.4e's own live GDB-remote attach technique, applied fresh during a STEP-2 stall specifically
   (M24.4e's own attach was taken during what turned out to be STEP 1's own successful-but-
   misdiagnosed completion, not a real STEP-2 stall), which is real, separate work this task's own
   assigned scope (the monitor client) does not cover and this task's remaining budget was not
   spent chasing.
3. **`cargo clippy --workspace --all-targets -- -D warnings` and `cargo deny check`** -- not run by
   this task, consistent with every prior M24.4x report; this task's own assignment was the monitor
   client and the byte comparison.

status: complete. Root cause found and explained (a fixed idle-gap heuristic mis-attributing a
slow command's real completion to the next, unrelated command); a serialising fix implemented
(request tagging, exactly-one-outstanding-command via a typed `MonitorDesyncError`, prompt-anchored
reply parsing, a bounded post-completion grace drain for genuine millisecond-scale stragglers);
proven with a unit test that replays the exact captured interleaving and fails against a frozen
unfixed snapshot, passes after the fix; the fix itself found and fixed two more real desyncs of the
same class live against real Renode; the real byte comparison was run three full times with real
output recorded, reaching STEP 1's own byte-exact completion (further than any prior report in this
milestone) before hitting a separate, already-partially-characterized STEP-2+ delivery gap;
question 171 records the evidence precisely; `docs/sil-plan.md` and `third_party/renode/REPORT.md`
both carry the limitation and the named next step; cleanup verified zero; all three verification
suites match their recorded baselines exactly with zero regressions.

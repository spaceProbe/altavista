# M25.3e report: measurements on the viewer payload (question 174) + acknowledgement level (question 177)

State of the tree at start: M25.3d's own worker did real, good work (`web/js/timeline_events.js`,
`web/js/timeline_check.mjs`, `tests/test_viewer_timeline.py`, plus edits to `web/js/app.js`,
`web/js/panels/run_products_panel.js`, `web/style.css`) but was cut off before writing a report.
There is no `web/js/REPORT_M25_3d.md` anywhere in the repo -- the dangling reference to it inside
`timeline_events.js`'s own module doc comment is fixed by this task (now points here and at
`docs/open-questions.md` question 177). This file is the record for both M25.3d's finding and this
task's (M25.3e's) two work items.

## Work item 1: question 177 -- referenceId/attributes reach the viewer

**Hypothesis, stated before measuring:** `altavista/cdm.py`'s `cdm_event_to_viewer_event` drops
`ev.reference_id` and `ev.provenance.attributes` before the viewer ever sees a published scenario
(M25.3d's own finding, read from that function's source, not re-derived). Expected fix: thread both
through additively on `altavista.model.Event`/`ScenarioData`, and have
`web/js/timeline_events.js`'s `groupCommandTransitions` prefer the real `referenceId` over its own
regex-parsed `detail` fallback, with the acknowledgement panel showing the real `ack_level`.

**Measured, confirmed correct:** exactly as hypothesized. `Event.to_dict()`'s wire shape was
`{name, t, type, spacecraft, detail}` before this task (pinned by M25.3d's own
`test_acked_transition_reaches_the_viewer_with_no_trace_of_ack_level`, which asserted that exact
key set and explicitly said finding a 6th/7th key "is exactly the signal to come back and wire up
the client-side display for real" -- that signal fired this task).

### Code changes
- `altavista/model.py`: `Event` gains `reference_id: Optional[str] = None` and
  `attributes: Dict[str, str] = field(default_factory=dict)`; `to_dict()` adds `"referenceId"`/
  `"attributes"`. Both additive, defaulting to falsy/empty.
- `altavista/cdm.py`: `cdm_event_to_viewer_event` now passes `reference_id=ev.reference_id or None,
  attributes=dict(ev.provenance.attributes)`.
- `web/js/timeline_events.js`: `groupCommandTransitions` groups by `ev.referenceId` when present,
  falling back to `parseCommandId(ev.detail)` only when absent; `ackLevel` is read off
  `ev.attributes.ack_level` (present only on a command's ACKED transition), `null` otherwise --
  never fabricated.
- `web/js/panels/run_products_panel.js`: the acknowledgement panel now shows the real ack level
  text instead of "not reported to the viewer".
- Dangling `REPORT_M25_3d.md` references in `timeline_events.js`/`timeline_check.mjs` fixed to
  point at `docs/open-questions.md` question 177 and this file.

### Judgement call: do the detail-parsing helpers stay?

- **`parseCommandId`: KEPT, as a fallback only.** `groupCommandTransitions` now prefers
  `ev.referenceId`; `parseCommandId(ev.detail)` is used only when an event carries no
  `referenceId` at all (a scenario published before this task, or a producer that never sets it).
  Removing it would regress every such scenario from "grouped correctly, just via detail" to
  "silently unparsed". Proven live by `web/js/timeline_check.mjs`'s new "FALLBACK" check (a
  synthetic event with no `referenceId` still groups via `parseCommandId`) and "PRIORITY" check (a
  present `referenceId` groups correctly even when `detail` is deliberately garbled and would fail
  to parse on its own) -- so the fallback is both real and dominated by the primary path, not dead
  code either way.
- **`parseContactCounterpart`: KEPT, not a fallback -- still the ONLY way.** A contact event's own
  `reference_id` is not the counterpart (`events.rs::contact_event` never sets one); the counterpart
  rides only in `detail`'s free text. This was never in question -- the brief's own text says so --
  and nothing in this task's changes touches it.

### Break-and-restore evidence
1. Reverted `cdm.py`'s `cdm_event_to_viewer_event` to the pre-task mapping (no `reference_id`/
   `attributes` passed to `Event(...)`). Result: 5 of 9 `tests/test_viewer_timeline.py` tests failed
   -- `test_acked_transition_now_carries_the_real_reference_id_and_ack_level`,
   `test_every_command_transition_carries_its_real_reference_id`,
   `test_reference_id_and_attributes_reach_the_viewer`,
   `test_command_transitions_grouped_by_command_with_real_state_names` (the ackLevel sub-check),
   `test_timeline_check_report`. Restored -> 9 passed again.
2. Reverted `groupCommandTransitions` to ignore `ev.referenceId` (parse `detail` unconditionally)
   and to never read `ack_level` off `attributes`. Result: `test_command_transitions_grouped_by_
   command_with_real_state_names` failed (cmd1's ackLevel check) and the two new PRIORITY checks
   failed (a present `referenceId` no longer rescues a garbled `detail`). Restored -> 9 passed
   again, 39/39 `timeline_check.mjs` checks green.

## Work item 2: question 174 -- measurements on the viewer payload

**Hypothesis, stated before measuring:** mirroring question 165 (scores) exactly gives the smallest,
most-reviewable diff: an additive `_measurement_to_dict` in `altavista/server.py`, a `measurements`
list on `ScenarioData`, wired into `POST /api/cdm/run` with `meta.measurementsSource`. Expected: no
change needed to `POST /api/cdm/trajectory` at all, since that route builds its own `ScenarioData`
independently and the new field defaults to `[]`.

**Measured, confirmed correct.** `_measurement_to_dict` maps `{measurement_id, epoch_ns, sensor_id,
frame_id, z, r}` -> `{id, epoch, sensorId, frameId, z, r}`, `epoch` through the same
`tai_ns_to_a1mjd` every other epoch on the payload uses. `/api/cdm/trajectory` needed zero code
changes; `test_cdm_trajectory_route_is_unchanged_by_question_174` proves it still defaults
`measurements: []` and never stamps `measurementsSource`.

### Code changes
- `altavista/model.py`: `ScenarioData.measurements: List[dict] = field(default_factory=list)`,
  additive in `to_dict()`.
- `altavista/server.py`: `_measurement_to_dict` (mirrors `_score_result_to_dict`'s hand-built-dict
  convention exactly); `POST /api/cdm/run` computes `viewer_measurements` (no re-sort -- the
  executor already sorts by `(epoch, id)`, question 173) and threads it plus
  `meta.measurementsSource = "RunProducts.measurements"`.
- `web/js/panels/run_products_panel.js`: `measurementRows`, `measurementCountsBySensor`,
  `timelineMeasurementTicks` (pure functions) plus a "Telemetry" panel section.
- `web/js/app.js`: passes `sc.measurements` to the panel and draws one small tick per measurement
  epoch on the timeline via `timelineMeasurementTicks`.
- `web/style.css`: `.tick-measurement`, `.av-timeline-event-measurement`.

### A real bug this task's own tests caught (not staged -- found while writing the panels_check.mjs
coverage)
`measurementRows` originally did `frameId: m.frameId || null`, coercing the wire's own literal
`""` (the NORMAL value for `demo_measurements` -- "frame_id empty for every measurement") to
`null`. `panels_check.mjs`'s `frameId === ''` assertion failed immediately. Fixed to `m.frameId ??
null` (only `null`/`undefined` become `null`; an empty string is carried through honestly). This is
exactly the kind of bug this task's own "nothing is ever synthesized" rule exists to catch -- a
falsy-but-legitimate value must never be treated the same as "absent".

### Break-and-restore evidence
1. Removed `measurements=viewer_measurements` from the `ScenarioData(...)` call in
   `publish_cdm_run`. Result: `test_measurements_published_with_the_approved_wire_shape_ids_and_
   epochs` and `test_measurements_order_is_preserved_never_resorted_by_the_server` failed (both
   correctly -- an implementation that "accepts but never threads" produces `measurements: []`
   regardless of input); `test_measurements_is_additive_and_empty_by_default...` and
   `test_cdm_trajectory_route_is_unchanged...` still passed (unaffected by this specific break, as
   predicted). Restored -> all 4 non-skipped tests passed again.
2. Broke `_measurement_to_dict` itself: wrong wire key (`measurementId` instead of `id`), no epoch
   conversion (`m.epoch_ns` raw), and a fabricated identity covariance whenever `r` was empty.
   Result: `test_measurements_published_with_the_approved_wire_shape_ids_and_epochs` failed
   immediately on the key-set assertion (would also have caught the epoch/`r` bugs had it gotten
   past that). Restored -> passed again.
3. `web/js/panels_check.mjs`'s own `measurementCountsBySensor`/`timelineMeasurementTicks` checks:
   broke the sort (`.sort()` removed) and the per-measurement tick mapping (deduped by epoch).
   Result: 3 checks failed exactly as expected (`measurementCountsBySensor: ... sorted by
   sensorId`, both `timelineMeasurementTicks` checks). Restored -> 47/47 green. (The fixture's
   sensor-insertion order was deliberately chosen to differ from alphabetical order specifically
   so the missing-`.sort()` bug could not hide behind coincidental ordering -- an earlier version
   of this fixture had that exact blind spot and was caught and fixed before landing.)

### The acceptance test: BLOCKED, disclosed, not faked
The lead's own acceptance criterion -- "test with a real av-run bundle that the demo's telemetry
appears with the right ids and epochs" against `demo_measurements` (`crates/av-kernel/tests/
demo_measurements.rs`'s own 36-measurement, 12-epoch proof) -- **cannot be completed in this
environment**: producing a real `RunProducts` bundle from `drms/demo_measurements.{drm,sos}.yaml`
needs `av-run`, which needs `cargo build`, which this task's own ABSOLUTE CONSTRAINT forbids
outright (a heavy Rust gate runs concurrently on this host). No frozen
`tests/fixtures/demo_measurements.runproducts.bin` exists yet (grepped: no `*.runproducts.bin`
anywhere in the repo names it, unlike `demo_two_instance`/`demo_attitude_control`, which do have
frozen fixtures from an earlier real `av-run` invocation).

`tests/test_cdm_run.py::test_demo_measurements_real_bundle_publishes_36_measurements_with_right_
ids_and_epochs` is written as a real, would-pass-if-fed-the-real-bundle test (decodes the frozen
bundle directly for the id/epoch/count facts against `demo_measurements.rs`'s own pinned values,
then re-derives the same facts from the actually-published scenario JSON) -- it is **SKIPPED**,
with a message naming the exact blocker and the exact command to unblock it:

```
cargo build -p av-run --bin av-run
target/debug/av-run --drm drms/demo_measurements.drm.yaml \
    --sos drms/demo_measurements.sos.yaml \
    --system drms/demo_attitude_sensors_truth.system.yaml \
    --system drms/demo_measurements_startracker.system.yaml \
    --system drms/demo_measurements_imu.system.yaml \
    --run-id demo-measurements-frozen \
    --out tests/fixtures/demo_measurements.runproducts.bin
```

This was **not** worked around with a hand-built substitute dressed up as the real DRM's output.
The general wire-format plumbing (item 2 above) is proven correct against small, explicitly-
synthetic `core_pb2.Measurement` messages POSTed through the real, unmodified server route --
that is a genuinely real code path, just not the specific `demo_measurements` acceptance claim,
and the report says so plainly rather than blurring the two.

## Checks run

`node web/js/timeline_check.mjs`: 39 checks, all pass (was fewer before this task's additions --
new: 3 `ackLevel` checks, 4 `referenceId/attributes` checks, 3 fallback/priority checks = 10 new).
`node web/js/panels_check.mjs`: 47 checks, all pass (9 new: 5 `measurementRows`, 2
`measurementCountsBySensor`, 2 `timelineMeasurementTicks`).

`.venv/bin/python -m pytest -q tests/test_viewer_timeline.py`: 9 passed.
`.venv/bin/python -m pytest -q tests/test_cdm_run.py` (deselecting the 10 tests that need a live
`cargo build`/`av-run` invocation -- this task's own ABSOLUTE CONSTRAINT forbids running cargo at
all): 37 passed, 1 skipped (the acceptance test, disclosed above).
`.venv/bin/python -m pytest -q tests/test_viewer_panels.py`: 13 passed.
`.venv/bin/python -m pytest -q tests/test_cdm_adapter.py` (deselecting the one test that shells out
to `cargo run`): 55 passed.
`.venv/bin/python -m pytest -q tests/test_script_prep.py tests/test_cdm_v1.py tests/
test_viewer_globe.py tests/test_viewer_jitter.py tests/test_viewer_layout.py tests/
test_viewer_net.py tests/test_viewer_viewport.py`: 115 passed (no regression from the additive
`Event`/`ScenarioData` field changes).

## What was NOT done, and why

- The `demo_measurements` acceptance test is skipped, blocked on cargo (see above) -- reported
  prominently, not worked around.

## Manager addendum (2026-09-07): the acceptance-test blocker is closed

The blocker above was correctly reported rather than worked around, and the recipe left in the
test's own docstring was exact. Once the heavy kernel gate finished and cargo was free, the
manager ran that recipe verbatim:

```
cargo build -p av-run --bin av-run
target/debug/av-run --drm drms/demo_measurements.drm.yaml \
    --sos drms/demo_measurements.sos.yaml \
    --system drms/demo_attitude_sensors_truth.system.yaml \
    --system drms/demo_measurements_startracker.system.yaml \
    --system drms/demo_measurements_imu.system.yaml \
    --run-id demo-measurements-frozen \
    --out tests/fixtures/demo_measurements.runproducts.bin
```

`av-run` reported `config_hash=e8d184b42a7c9fefa88571bb61b22d5a71232a4b58233df395907f495f10a656`,
matching `drms/demo_measurements.drm.yaml`'s own declared hash, and wrote 9755 bytes. The frozen
bundle is now committed at `tests/fixtures/demo_measurements.runproducts.bin`, alongside the two
that already existed, and
`test_demo_measurements_real_bundle_publishes_36_measurements_with_right_ids_and_epochs` **runs and
passes** -- it no longer skips.

**Independently verified against the artifact, not against the test that consumes it** (the
standing rule that a test tool's pass condition is pinned against a captured real artifact): a
standalone decode of the frozen bytes reports 36 measurements; exactly 12 each of
`altavista.attitude_q4`, `altavista.imu_accel3`, `altavista.imu_gyro3`; 12 distinct epochs equal to
`1767225637000000000 + k*500_000_000` for k=1..=12; `frame_id` empty for all 36; `r` length 0 for
the star tracker and 9 for both IMU ids. The same decode also confirms **question 176 end to end**:
every one of the 36 measurements carries `meta["decoded_at"]` equal to its own `sensor_id`, so the
emitter-side decode survives serialization onto the real wire bundle, not merely in-process.
`port_traffic_hash` is empty, as expected with M25.4 not yet built.

**Break-and-restore (manager's own, since this test had never run):** truncated the server's
publish path to `run_products.measurements[:5]` in `altavista/server.py`; the acceptance test
failed with `AssertionError: assert 5 == 36` at `tests/test_cdm_run.py:1716`. Restored; `diff`
against the pre-break copy is byte-identical and the test is green again.
- `crates/**` was not touched (out of scope; no kernel-side defect was found that would require it).
- The full repo test suite / `cargo` were never run, per this task's own ABSOLUTE CONSTRAINT; counts
  above are for the specific files this task touched or that import the modules this task changed.

# `av-run`

M16.3, question 5's first demo bridge: **"a DRM authored in Python, propagated with GMAT
dynamics, shown on the custom globe and in ICRF, reproducible from its config hash."** Every
piece of that already existed except the link between the Rust DRM executor
(`crates/av-kernel/src/drm`) and the altavista viewer. `av-run` is that link: a small binary
that loads a DRM bundle, runs it end to end through `av_kernel::drm::execute`, and emits the
resulting `RunProducts` (trajectories, events, run provenance) as CDM v1 on the wire.

## Binary, not an RPC -- and why

The alternative considered was a `run_drm` RPC on `crates/av-dynamics-service`'s existing
`altavista.v1.DynamicsService`. It is not available to this task:
`proto/altavista/v1/dynamics_service.proto` declares only model-level RPCs (`Describe`/
`Step`/`Solve`/`Propagate`/`Derivatives`) -- there is no DRM-level method to reuse, and
adding one needs a `.proto` change, which `proto/**` (read-only to this task, "no proto
change authorized") forbids outright.

`av_kernel::drm::execute` is a synchronous call that borrows one `&gmat_sys::Gmat` handle for
its whole duration (GMAT is a per-process singleton, not thread-safe --
`gmat_sys::engine_lock()`'s own doc comment). The natural way to expose "run this DRM bundle"
without touching `proto/**` or the tonic service plumbing (`crates/av-grpc`, also not owned
by this task) is a standalone CLI that drives the executor directly -- exactly the same
`Gmat::setup` + `engine_lock()` + `RunConfig` shape every `crates/av-kernel/tests/drm_*.rs`
test already uses -- and puts `RunProducts` on the wire itself.

## Usage

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -p av-run

# Run the golden maneuver DRM and write the wire bundle to a file:
target/debug/av-run \
  --drm drms/leo_1day_maneuver_vnb.drm.yaml \
  --sos drms/leo_1day_maneuver_vnb.sos.yaml \
  --system drms/leo_1day_golden.system.yaml \
  --run-id demo-1 \
  --out /tmp/run.bin

# ...and/or POST it straight to a running altavista server's POST /api/cdm/run:
target/debug/av-run \
  --drm drms/leo_1day_maneuver_vnb.drm.yaml \
  --sos drms/leo_1day_maneuver_vnb.sos.yaml \
  --system drms/leo_1day_golden.system.yaml \
  --run-id demo-1 \
  --server http://127.0.0.1:8765
```

`--system` may be repeated for a `SosConfiguration` with instances bound to more than one
`SystemDefinition`. `--error-mode nominal|sampled` selects `av_kernel::drm::
ExecutionErrorMode` (default `nominal`); `--gmat-startup <path>` overrides
`Gmat::default_startup_file()`. At least one of `--server`/`--out` is required.

## The wire format: `altavista.v1.RunProducts`, a real CDM message (question 121, M17.2)

Through M16.3, the CDM had no message for a whole run, so this binary framed
`av_kernel::drm::RunProducts` (`{ trajectories: BTreeMap<String, Trajectory>, events:
Vec<Event>, scores: BTreeMap<String, Score>, provenance: Provenance, ... }`) as an ad hoc,
explicitly length-prefixed concatenation of its `Trajectory`/`Event`/`Provenance` fields' own
binary protobuf encodings (`b"AVRUN1"` magic), since `proto/**` was read-only to that task and
the CDM's real multi-message envelope (`envelope.proto`'s `Batch`/`SignedBatch`) exists for
the signed, chained ingest log (ADR-004), not a demo bridge.

**The lead has since added a real message for a whole run** (`docs/open-questions.md` question
121): `proto/altavista/v1/run.proto` declares `RunProducts { run_id, trajectories (map, keyed
by SystemInstance.id), events, scores (map of `ScoreResult`), provenance,
dropped_in_flight_messages, frames }` and `ScoreResult { name, value, unit, optional bool
passed }`. **The `AVRUN1` framing is deleted, not kept as a fallback, on both sides** --
`src/main.rs`'s old `encode_run_bundle` and `altavista/cdm.py`'s old `parse_run_wire`/`RunBundle`
are gone. `av_kernel::drm::executor::RunProducts::to_proto` converts this binary's own
`RunProducts` into the real `av_cdm::pb::RunProducts` message (every `Trajectory`/`Event`/
`Provenance` passes through unchanged; `scores` becomes one `ScoreResult` per entry, `passed`
copying straight across as `Option<bool>` so an `Objective`'s `Some(false)` and a
`MeasureOfEffectiveness`'s `None` stay distinguishable on the wire -- proto3 `optional bool`,
not a plain `bool`; `frames` and `dropped_in_flight_messages` are genuinely populated fields,
not placeholders -- see `crates/av-kernel/README.md`'s "RunProducts on the wire" section for
exactly how), and this binary just calls `prost::Message::encode_to_vec` on the result:
ordinary protobuf bytes, no bespoke envelope. `scores`/`frames` are on the wire now (unlike the
old framing, which dropped `scores` entirely) even though the viewer does not yet surface them
(question 122/M17.1's own job) -- accepted and preserved by `POST /api/cdm/run`, not silently
lost.

## No new HTTP-client dependency

`POST`ing the bundle to a running `altavista` server needs an HTTP client. This workspace's
existing servers avoid pulling in a full HTTP stack for a small, fixed, plaintext-localhost
surface (`crates/av-dynamics-service/src/admin.rs`'s own module doc: "a plain
`tokio::net::TcpListener` with manual GET-only request parsing rather than depending on
axum/hyper directly"). `src/main.rs`'s `http_post` is the client-side mirror of that same
choice: a hand-rolled, blocking HTTP/1.1 POST over `std::net::TcpStream`, plaintext, for
exactly one request/response -- altavista's own viewer server is plaintext-localhost-only by
design (`altavista/server.py`'s module doc). This keeps `cargo tree | grep -ci ring` at 0
without auditing a new dependency's own transitive tree.

## The altavista side

`altavista/server.py`'s `POST /api/cdm/run` (next to the pre-existing `POST
/api/cdm/trajectory`) accepts this message **exactly the same way** `POST /api/cdm/trajectory`
does: binary protobuf (`Content-Type: application/x-protobuf`, `run_pb2.RunProducts.
ParseFromString`) or JSON transcoding (any other content type, `google.protobuf.json_format.
Parse`) -- M17.2's own requirement. Every `Trajectory` (read off `RunProducts.trajectories`, a
proto map keyed by `SystemInstance.id`, iterated in sorted-key order since a proto map has no
wire order of its own) converts with the *existing, unmodified*
`altavista.cdm.cdm_trajectory_to_viewer_json` (M16.3's own design steer, still true: reuse that
path rather than writing a second converter) and every `Event` with
`altavista.cdm.cdm_event_to_viewer_event`, and publishes/broadcasts one scenario carrying all of
them, exactly like `POST /api/scenario` already does. The run's `Provenance.config_hash` (the
DRM's own canonical hash, verified and refused-if-tampered by
`crates/av-kernel/src/drm/hash.rs` *before* the run ever executed) is carried in
`ScenarioData.meta["configHash"]`, additive on the existing `meta` dict, and shown in the
viewer's info line (`web/js/cdm_run.js`'s `formatScenarioInfo`, called from `web/js/app.js`)
-- "reproducible from its config hash" means a viewer showing a run must be able to show
*which* configuration produced it. `RunProducts.scores`/`.frames` are parsed and preserved but
not yet threaded into `ScenarioData` -- the viewer has no objective/MOE display surface yet,
and building the scene's frame list from `RunProducts.frames` through `FrameRegistry` is
question 122's own M17.1 task.

## What this demo does and does not prove

- **Proven end to end** (`tests/test_cdm_run.py`): the golden maneuver DRM
  (`drms/leo_1day_maneuver_vnb.{drm,sos}.yaml` + `drms/leo_1day_golden.system.yaml`) runs
  through `av-run`'s real GMAT propagation, lands in a real altavista server via `POST
  /api/cdm/run`, and the published scenario's config hash, trajectory epoch span, and
  MANEUVER/LIFECYCLE event epochs/kinds are read back and checked against the DRM's own
  declared hash/window and `goldens/leo_1day_maneuver_vnb.json`'s own recorded burn epoch --
  including through the viewer's own headless harness (`web/js/verify_cdm_run.mjs`, running
  the real, shipped `web/js/cdm_run.js` under `node`), not merely the Python conversion
  layer.
- **EVENT_KIND_FAULT** is proven through the identical server/viewer code path fed a
  hand-built, protocol-honest synthetic bundle (`tests/test_cdm_run.py::
  test_fault_event_kind_reaches_the_same_server_and_viewer_path`), since the golden maneuver
  DRM itself declares no `FAULT_TARGET_KIND_DYNAMICS` fault to draw a real one from. This is
  stated plainly rather than silently substituted for a real fault-producing run.
- **Multiple simultaneous trajectories** (a `SosConfiguration` with more than one
  `BINDING_KIND_MODEL` instance): `RunProducts.trajectories` is a proto map keyed by
  `SystemInstance.id`, so this handles an arbitrary count of trajectories by construction, but
  no fixture this task's tests exercise declares more than one instance, so the
  multi-trajectory path is exercised at the conversion-unit level
  (`crates/av-kernel/src/drm/executor.rs`'s `to_proto_tests`) rather than a live
  multi-instance GMAT run.
- **ICRF / the custom globe**: rendering in a chosen frame and on the custom globe is
  existing, unmodified viewer functionality (`web/js/frames.js`, `web/js/globe.js`, not owned
  by or changed for this task) -- `av-run` only has to get the trajectory's own declared
  `frame_id` onto the wire honestly (from `ModelInfo.frame_id`, GMAT's own configured
  coordinate system name), which it does via the unmodified `Trajectory.frame_id` field.
- **`RunProducts.frames`** (question 121/122, M17.2): genuinely populated from the real run's
  own trajectories -- see `crates/av-kernel/README.md`'s "RunProducts on the wire" section --
  and proven end to end for the golden maneuver DRM by
  `tests/test_cdm_run.py::test_run_bundle_bytes_are_a_real_run_products_message_with_frames_and_scores`
  (decodes the real `av-run` output directly and checks the `EarthMJ2000Eq` `FrameDefinition`
  it carries). Threading `frames` into the viewer's own scene (through `FrameRegistry`) is
  question 122's own M17.1 task, not this one -- `altavista/server.py` parses and preserves the
  field but does not yet build anything from it.

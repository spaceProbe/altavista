# Test fixtures -- `demo_ground_segment` run products (E4a, the simulated-asset plugin)

`run_products.pb` and `port_traffic.pb` are the real `altavista.v1.RunProducts` and
`altavista.v1.PortTrafficLog` bytes produced by one execution of
`drms/demo_ground_segment.{drm,sos}.yaml` plus `drms/demo_ground_segment_{flight,ground}.
system.yaml` -- 900 s at 1 Hz, `"flight"` (a native `ConstantAccelModel`) broadcasting its
own Earth-fixed Cartesian position every step on the FRAMED `tm_out` port, received by
`"ground"` (a `GroundStationModel`) over a 100 ms-latency link.

Committed so `crates/av-edge/tests/plugin_replay.rs` and `crates/av-ingest/tests/
plugin_wire.rs` are offline and deterministic (question 154) -- neither test ever runs GMAT
or `av-kernel`'s executor itself.

## Regenerating

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export GMAT_ROOT="/Users/probe/code/AltaVista/GMAT R2026a"
cargo test -p av-kernel --test generate_e4a_ground_segment_fixture -- --ignored --nocapture
```

See `crates/av-kernel/tests/generate_e4a_ground_segment_fixture.rs`'s own module doc for
exactly what that test does. Only re-run this if `drms/demo_ground_segment*.yaml` themselves
change (their own committed `hash:` fields would need repinning too).

## Pinned values

- `RunProducts.port_traffic_hash` (SHA-256 hex of `port_traffic.pb`'s exact bytes):
  `5497083dd47f246803a28ba4cda4c8dcaa4314d60c6129b67674afc2fa2a718b`
- `RunProducts.provenance.run_id`: `e4a-demo-ground-segment-fixture`
- `drms/demo_ground_segment.drm.yaml` hash **at the time this fixture was generated**:
  `c736e887531cded74bb95de2b319011e7285a148bed9e38f768cc19ab369e2e2` -- recorded provenance
  and, since round 5, **asserted** against the DRM's own live, committed `hash:` field by
  `crates/av-kernel/tests/e4a_fixture_provenance.rs` (which also asserts that
  `port_traffic.pb` hashes to the `port_traffic_hash` `run_products.pb` records, so a
  half-regeneration cannot land either). Nothing in the replay path itself reads it -- question
  207 traced every reader of this fixture and only `run_id`/`created_tai_ns` are ever pulled
  back out of `RunProducts.provenance` -- which is exactly why it needed a guard rather than a
  comment. History: round 4 (question 207) registered
  `earth_fixed_demo_frame` in this DRM's own `scenario.frames`, which moved its committed
  `hash:` field to the value above -- but this fixture was not regenerated at the time, so its
  recorded provenance kept naming the DRM's *prior* hash,
  `7a5944b319fa0dd6f5781c616a75beac94d94fa8d986e5f0b8eaaa46890412d0`, a DRM that no longer
  existed anywhere in the tree. Question 210 (2026-09-15) ratified round 4's decision 8 in
  part -- `earth_fixed_demo_frame` stays registered -- and ruled that the fixture be
  regenerated from the current DRM with the committed generator so its provenance is true
  again. It has been: the value above is what the regenerated fixture now records, and it is
  equal to the DRM's own live, committed `hash:` field, verified by decoding
  `run_products.pb` (see `crates/av-kernel/tests/generate_e4a_ground_segment_fixture.rs`'s
  module doc for the regeneration command).
- `drms/demo_ground_segment.sos.yaml` hash: `111ed78f4e7a0a0055bffddc320a142a5ffde3881161649ca9875ee5a8fae4f2`
- `drms/demo_ground_segment_flight.system.yaml` hash: `0f7f191b664f7f28dc09f0505d8e1505f0f1601952dd32d5f3260addeca5f32e`
- `run_products.pb`: 61982 bytes
- `port_traffic.pb`: 116261 bytes, 900 `PortTrafficRecord`s, every one `instance="flight"`,
  `port="tm_out"`, `direction=PORT_DIRECTION_OUT` (one per second of the 900 s run; `"ground"`
  emits nothing on any FRAMED port in this DRM).

Downstream, `crates/av-edge/tests/plugin_replay.rs` pins the derived plugin-replay values
computed from these two files plus `crates/av-edge/tests/fixtures/test_signing_key.pem`
(**not a real identity** -- see that key's own `README.md`):

- Batch count: 900 (one `MeasurementBatch` per recorded epoch, `BatchingRule::PerEpoch`).
- Final chain head (`batch_hash` of the 900th batch), hex:
  `d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698`
- Decoded-position-vs-truth-trajectory maximum deviation: `0.0` m (FLOAT64 CCSDS fields
  round-trip bit-exactly through `scale=1.0`/`offset=0.0` -- see `plugin_replay.rs`'s own
  test for the measured, printed value).

If `crates/av-edge/src/plugin.rs`'s canonical hash/signing logic, `tests/fixtures/
test_signing_key.pem`, or this fixture ever change, these derived values must be
recomputed and repinned together -- `plugin_replay.rs`'s own tests print the actual chain
head and deviation on every run, so repinning is "read the printed value, paste it here."

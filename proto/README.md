# AltaVista CDM v1 (`altavista.v1`)

Source of truth for every message the platform stores, logs or sends. Decided in
[ADR-001](../docs/adr/001-cdm-v1.md); the dynamics service in [ADR-002](../docs/adr/002-dynamics-contract.md).

| File | Contents |
|---|---|
| `core.proto` | time scales, units, frame registry entries, state spaces, Gaussian states, beliefs, measurements, innovations, provenance |
| `envelope.proto` | labels, envelopes, batches, signed and hash-chained batches |
| `entity.proto` | entities with aliases, spatial/temporal extents, asset references (claim-check) |
| `trajectory.proto` | trajectories with interpolation contracts and segments; events |
| `system.proto` | system definitions with bus-neutral ports, bindings (model / container / Renode / board), system-of-systems configurations, design reference missions, faults, sweeps |
| `command.proto` | commands, the authority state machine, ack levels, proposals |
| `dynamics_service.proto` | `DynamicsService`: describe, derivatives, step, propagate, solve |

## Compatibility

`spoore.v0` is a compatible subset: `StateComponent`, `StateSpace`, `GaussianState`,
`MixtureComponent`, `Belief`, `Measurement` and `Innovation` keep spoore's names and field
numbers. Two fields are reserved where spoore used an enum `Frame` (`StateSpace` 3,
`Measurement` 7); v1 references the frame registry by id instead. `epoch_ns` is declared
TAI; the `av-cdm` adapter shifts spoore's Unix-scale epochs on conversion.

Strict compatibility from P0: `buf breaking --against` the previous tag with the `FILE`
rule set runs in CI; a breaking change is a new package (`altavista.v2`).

## Compile

```bash
protoc -I proto -I "$(brew --prefix protobuf)/include" \
  --descriptor_set_out=/dev/null proto/altavista/v1/*.proto
```

`buf lint` and `buf breaking` use `proto/buf.yaml`.

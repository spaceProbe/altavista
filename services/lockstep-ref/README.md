# lockstep-ref

A reference implementation of `altavista.v1.LockstepService` (`proto/altavista/v1/lockstep.proto`,
`docs/open-questions.md` question 107) in Python, over `grpcio` and the already-generated
`altavista.pb.altavista.v1.lockstep_pb2`/`lockstep_pb2_grpc` bindings.

## This is a test fixture, not a deployed service

Contrast with the other two Python/Rust-adjacent services in this repository:

- **`services/gmat-service`** hosts real GMAT (ADR-002 depth 1) and is explicitly
  design-time only ("this service is now a design-time tool only, not part of a deployed
  profile" -- its own README's banner), but it is still a *real* implementation of something
  this platform actually needs: a GMAT-backed `DynamicsService`.
- **`crates/av-dynamics-service`** is the *deployed* Rust runtime for the same
  `DynamicsService` contract (ADR-002 depth 2, `"gmat-ffi"`) -- never `grpcio`, never
  BoringSSL.
- **`lockstep-ref` (this package)** is neither. It implements `LockstepService` correctly
  enough to be driven end to end by `crates/av-kernel`'s `BINDING_KIND_CONTAINER` executor
  path and by `tests/test_lockstep_ref.py`, but it is **only ever spawned as a local
  subprocess inside a test** (never `add_insecure_port` on a non-loopback address, never
  started by a profile, never referenced by `profiles/*.yaml`). `grpcio` bundling BoringSSL
  is accepted here for exactly the reason it is accepted (with the same caveat) in
  `gmat_service`: this process never terminates a real network-facing TLS connection in this
  batch's tests (`--tls-cert`/`--tls-key`/`--tls-ca` exist so a caller *can* exercise
  `av_lockstep::BlockingLockstepClient::connect_mtls` against it, but no required test in
  this batch uses that path -- see "Honesty" below).

A real container-hosted `LockstepService` process (cFS, ROS 2, custom flight software) would
be built and run by whoever owns that binding's image -- **not** this package in general. This
package's own `Dockerfile` (M15.3, `docs/open-questions.md` question 118) is the one exception,
and a narrow one: it packages this exact reference fixture as a real, runnable image *only* so
`av-kernel`'s own Docker image-lifecycle path (pull by digest, run, Bind over loopback, stop and
remove -- see this file's own "Docker image lifecycle" section below) has something real to
pull/run/stop/remove in tests. It is not a template for a real binding's own image, and no image
built from it is ever pushed anywhere but a throwaway, loopback-only local registry a test starts
and stops itself. `lockstep-ref` exists so the *protocol* half of `BINDING_KIND_CONTAINER` has
something real to Bind/Step/Reset/Shutdown against, both in tests and as a worked example of what
a real lockstep-capable process must do -- whether spawned as a bare subprocess (M13.2) or run
from this Docker image (M15.3).

## What it does

One SIGNAL input port, one SIGNAL output port (names configurable, `--in-port`/`--out-port`,
default `"in"`/`"out"`), one named output (`--output-name`, default `"integral"`): every
`Step` sums whatever SIGNAL payloads arrived on the input port, holds that sum constant over
`[cursor, until_tai_ns]` (an explicit-Euler integration -- not claimed to be higher order),
adds `value * dt_s` to a running integral, and reports the integral both as a SIGNAL on the
output port and as `named_outputs["integral"]`. See `lockstep_ref/server.py`'s own module doc
comment for `Bind`'s port-set validation and `Reset`'s exact behaviour.

## Running it

```sh
cd services/lockstep-ref
/path/to/AltaVista/.venv/bin/python -m lockstep_ref --port 50070
```

or, from anywhere, with the package directory on `PYTHONPATH`:

```sh
PYTHONPATH=services/lockstep-ref .venv/bin/python -m lockstep_ref --port 50070
```

Plaintext gRPC on `127.0.0.1` by default (like `gmat_service`); `--tls-cert`/`--tls-key`
(and optionally `--tls-ca` to also require and verify a client certificate) switch to
`grpc.ssl_server_credentials` for the rare test that wants to exercise mTLS end to end.

## Test-only misbehaviour knobs

Read only from environment variables, never from any RPC field, so a well-behaved caller can
never trigger them by accident:

- `LOCKSTEP_REF_REFUSE=1` -- every `Bind` refuses (`lockstep_capable=False`), regardless of
  the request's declared ports. Exercises the plain "not lockstep capable" refusal.
- `LOCKSTEP_REF_LIE_REACHED_AT_STEP=<n>` -- on the `n`-th `Step` call after a successful
  `Bind` (1-indexed), responds with `reached_tai_ns = until_tai_ns + 1` instead of the
  correct value, exactly once.
- `LOCKSTEP_REF_LIE_SEQUENCE_AT_STEP=<n>` -- on the `n`-th `Step` call, responds with
  `sequence = request.sequence + 1` instead of echoing it back correctly, exactly once.

`crates/av-kernel/tests/drm_container.rs` spawns this same process with these same
environment variables to prove the DRM executor's own protocol checks (sequence,
`reached_tai_ns`) stop a run with a typed error rather than silently accepting a bad response.

## Docker image lifecycle (M15.3, `docs/open-questions.md` question 118)

`Dockerfile` (build from the **repository root**, not this directory -- see the file's own
top comment for why) packages this reference process into a real, runnable image:

```sh
docker build -f services/lockstep-ref/Dockerfile -t lockstep-ref:local .
```

`av_lockstep::docker::ManagedContainer` (`crates/av-lockstep/src/docker.rs`) is what actually
pulls an image *by digest* and runs it -- `crates/av-kernel/src/drm/binding.rs::
materialize_container` calls into it when a `BINDING_KIND_CONTAINER` instance's `Binding.config`
declares a `ContainerBinding.image`/`image_digest` (instead of the M13.2 `container.address`
already-running-process path, which the two are mutually exclusive with). The container is
always published and connected to over loopback (`127.0.0.1`, ADR-003), and always stopped and
removed once the run's own `Shutdown` RPC completes. **No image built from this Dockerfile is
ever pushed to a real/external registry**; every test that needs to prove a genuine "pull by
digest" (not a locally-cached tag) starts a throwaway `registry:2` container on loopback,
pushes to it, and hands that address's image reference to `ManagedContainer` -- see
`crates/av-lockstep/tests/docker_lifecycle.rs`, `crates/av-kernel/tests/drm_container.rs
::docker_image_lifecycle_through_execute_...`, and this repository's own
`tests/test_lockstep_ref.py::test_docker_image_lifecycle_...` for the three places that pattern
is exercised (a Rust-only proof of `ManagedContainer` itself, the same proof through the real
`execute()` entry point, and a Rust-independent proof that this image behaves correctly when run
by a plain `docker` CLI). All three are gated on `docker info` succeeding and skip with a
recorded reason otherwise -- see those files' own doc comments for exactly how (and, for the
Rust side, a disclosed limitation: `cargo test` does not print a passing test's own skip-reason
`println!` without `--nocapture`, so the pytest-side test's `pytest.skip` is this repository's
verified-visible location for that requirement).

`IMAGE_DIGEST` (an environment variable `ManagedContainer` sets on the container it runs, never
an RPC field) is what makes `Bind`'s own `binding_hash` actually depend on the pulled image's
digest -- see `lockstep_ref/server.py`'s own `Bind` doc comment for exactly how it is folded in.

## Honesty / what is not exercised end to end

- `seed` (`LockstepBindRequest.seed`) is accepted, recorded into `binding_hash`'s own
  provenance-style digest, and otherwise unused -- this particular reference model (a
  deterministic explicit-Euler integrator) has no random behaviour to seed. A real
  lockstep-capable process that *does* need randomness would seed a PRNG from this field
  exactly the way `av-kernel`'s own DYNAMICS fault realization seeds a PCG64 stream from a
  declared `Scenario.seeds` entry (ADR-004: "seeds are inputs").
- `Reset` is implemented (zeroes the integral, re-anchors the simulated clock), covered
  directly by `tests/test_lockstep_ref.py::test_reset_zeroes_the_running_integral`, **and (M15.3
  question 118, moved from DYNAMICS to `FAULT_TARGET_KIND_HARDWARE` by M16.2 question 120) now
  driven end to end through the executor**: a `FAULT_TARGET_KIND_HARDWARE` fault of `kind ==
  "power_cycle"` naming a container-bound instance calls `Reset` at the fault's own epoch with
  `reason = "fault:<fault id>"`, and the run continues -- `crates/av-kernel/tests/drm_container.rs
  ::a_power_cycle_fault_on_a_container_instance_resets_the_integrator_and_the_run_continues`
  proves it by driving a real nonzero integral through a reset and asserting the post-reset
  value, not merely that the RPC does not error. See `crates/av-kernel/src/drm/fault.rs`'s own
  module doc comment ("Container power-cycle (HARDWARE)") for why HARDWARE is the fault shape
  this uses now -- `FAULT_TARGET_KIND_HARDWARE` already exists in
  `proto/altavista/v1/system.proto` and its own doc comment names "power cycle" directly; M15.3's
  brief wrongly believed only DYNAMICS/PORT/SENSOR were available. The interim DYNAMICS shape is
  now refused as a typed load error
  (`crates/av-kernel/tests/drm_container.rs
  ::a_dynamics_fault_of_kind_power_cycle_is_refused_as_a_typed_load_error`), so no DRM against
  this reference process can keep relying on it.
- mTLS (`--tls-cert`/`--tls-key`/`--tls-ca`, `av_lockstep::BlockingLockstepClient::
  connect_mtls`) is implemented on both ends but not exercised by any required test in this
  batch -- every required test uses plaintext loopback, which the task brief explicitly
  allows ("plaintext loopback allowed only for tests"). `grpcio`'s TLS bundles BoringSSL the
  same way `gmat_service`'s does; see that service's README "FIPS and crypto accounting"
  section for the same accounting applied here (this process's own TLS path, if ever used
  for something real, would need the same nginx-mTLS-front treatment `gmat_service` got, not
  a claim that `grpc.ssl_server_credentials` itself is FIPS-clean).
- This reference model has **no physical dynamics state at all** (position/velocity, or any
  other continuous ODE state) -- it is a pure SIGNAL-port integrator. `crates/av-kernel`'s
  `ContainerModel` (the executor-side binding) therefore declares `state_dim() == 0` for a
  container-bound instance; see that crate's README for exactly what that does and does not
  let a container-bound `Trajectory` express in this batch.

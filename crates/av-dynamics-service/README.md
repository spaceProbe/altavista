# av-dynamics-service

A Rust `tonic` gRPC server implementing `altavista.v1.DynamicsService`
(`proto/altavista/v1/dynamics_service.proto`) over `gmat-sys`/`av-dynamics` -- **ADR-002
depth 2** ("gmat-ffi": GMAT's `ODEModel::GetDerivatives` driven by our own
`av_dynamics::integrate::Dopri5` integrator), pinned against the same golden
`services/gmat-service` is: `goldens/leo_1day_jgm2_8x8_sunmoon.json` (Earth JGM2 8x8
gravity, Luna + Sun point masses, no drag, no SRP).

## Why this exists (ADR-003, 2026-09-02 amendment)

`services/gmat-service` (Python, `grpcio`) bundles BoringSSL and cannot terminate TLS on
the host's FIPS OpenSSL. The amendment decided two things: a cross-host `DynamicsService`
call is fronted by a service-owned nginx (question 84), and **`DynamicsService` is hosted
in Rust** so `grpcio` leaves the deployed runtime entirely (question 85). This crate is
that Rust host. `services/gmat-service` is retained as a **design-time** tool (solvers,
authoring, golden generation) — see that service's own README.

## What is and isn't different from `services/gmat-service`

Same golden, same declared physical configuration, same wire protocol (`proto/altavista/v1/dynamics_service.proto`
is the single source of truth for both). Two differences are deliberate, not bugs:

- **`ModelInfo.depth` / `TrajectorySegment.dynamics_depth` are `"gmat-ffi"`**, not
  `"gmat-api"` — ADR-002 depth 2 (our own integrator over GMAT's derivative function), not
  depth 1 (GMAT's own Python-API propagator).
- **`ModelInfo.settings_hash` differs from `gmat_service.config.settings_hash()`'s value.**
  `crate::config`'s settings map names `av_dynamics::integrate::Dopri5` as the integrator,
  not GMAT's native `PrinceDormand78`; ADR-002 says the hash should capture "what
  determines the physics," and the two services' integrators genuinely differ, so their
  hashes should too.

Because ADR-002 depth 2 never touches GMAT's own stateful `Propagator`/`Step()` API (only
the pure `GetDerivatives(state, dt)` function), the `MaxStepAttempts`-exhaustion failure
mode `services/gmat-service/README.md`'s "A correctness finding" section documents for
depth 1 does not apply here — see `src/propagate.rs`'s module doc for the full argument.
`Propagate`'s sampling loop still chunks at the caller's `sample_interval_s` (for
trajectory cadence, matching the wire contract both services share), just not to survive a
failure mode this integrator does not have.

## Running it

```sh
cd /Users/probe/code/AltaVista
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo run -p av-dynamics-service --bin av-dynamics-service -- --port 50062
```

Flags: `--port` (default 50062 — deliberately different from `gmat_service`'s 50061 so
both can run side by side during the transition), `--admin-port` (default 50162, +100 from
`--port` — see "Admin API" below), `--evidence-path` (default
`crates/av-dynamics-service/evidence.jsonl`), `--run-id` (mainly for tests).

**Plaintext gRPC on `127.0.0.1` only** (ADR-004/ADR-003 amendment) — never bind this to a
non-loopback address. mTLS across a host boundary is terminated by the same
service-owned nginx front `services/gmat-service` uses
(`services/gmat-service/deploy/nginx-gmat-grpc.conf.template`, unmodified — the template is
per-plaintext-backend, not per-language); `tests/test_grpc_tls.py`'s
`test_describe_succeeds_through_proxy_with_client_cert_against_rust_backend` /
`test_refused_without_client_cert_against_rust_backend` prove it against this server
specifically (both cases measured — see that test's `-s` output).

## Threading: the GMAT single-thread contract under an async runtime

GMAT is a process-wide singleton and is not thread-safe. `services/gmat-service` solves
this by sizing its **entire** `grpc.server` thread pool to exactly one worker. `tonic`'s
executor is `tokio`'s multi-threaded runtime, which this crate must never let touch GMAT,
so the equivalent rule here is a **dedicated `std::thread`** (`src/worker.rs`,
"the GMAT worker"): spawned once at startup, it builds this server's two bound GMAT models
(one plain 6-state, one 42-state/STM-augmented) and then loops taking closures off an
`std::sync::mpsc` channel, one at a time, for the life of the process — holding
`gmat_sys::engine_lock()` throughout. Every RPC handler dispatches its GMAT-touching work
through `WorkerHandle::run`, which sends a boxed closure down that channel and `.await`s a
`tokio::sync::oneshot` reply the worker thread fills in; `Describe` is the one exception —
its `ModelInfo` is computed once at warm-up and served directly (`Arc`-shared, `Send`,
plain owned CDM types), no round trip needed.

## `Propagate`'s two paths

- **`covariance = false`**: `src/propagate.rs::run_plain` chains `GmatModel::step`
  (`av_dynamics`'s default `Dopri5`-based stepper) across the requested sample cadence.
- **`covariance = true`**: `run_with_covariance` integrates the STM-augmented state
  (`av_dynamics::stm::StmAugmented`) across the same cadence, **without** reseeding `Phi`
  to the identity at each chunk boundary — the accumulated `Phi(t0, t_i)` is fed forward as
  the next chunk's own initial condition, which is exactly `Phi(t0, t_{i+1})` after
  integrating the same linear, homogeneous `d(Phi)/dt = A(t) Phi` ODE forward (the
  semigroup property, applied by superposition rather than by multiplying two
  separately-integrated factors). `P(t) = Phi(t0,t) P0 Phi(t0,t)^T`
  (`av_dynamics::stm::propagate_covariance`), explicitly symmetrized, checked with
  `av_cdm::covariance::check_spd_row_major` at every sample before being accepted — a
  hygiene failure (question 80) aborts the RPC (`FAILED_PRECONDITION`); there is no
  `PropagateRequest` field to opt into the nearest-SPD projection (same gap
  `gmat_service.model.GmatModel.propagate_covariance`'s own docstring names).

## Evidence log

`src/evidence.rs`: one JSON object per successful RPC response, JSONL, **same nine keys**
as `gmat_service.evidence.EvidenceLog` (the original six -- `epoch`, `method`,
`request_hash`, `response_hash`, `run_id`, `settings_hash` -- plus M7.3's hash-chain fields
`seq`, `prev_hash`, `hash`), keys already sorted in the emitted text (the struct's fields
are declared in alphabetical order, which is what `serde_json`'s struct serializer emits
verbatim — no need to round-trip through a `BTreeMap`-backed `Value` to get the same effect
`json.dumps(..., sort_keys=True)` gives on the Python side). Hashing is via
**`openssl::sha::sha256`** (the `openssl` crate, OpenSSL-backed), not `sha2` and not
`ring` — this is the point: `crates/av-grpc` already proves the OpenSSL-backed path for
TLS, and this is the same proof for hashing.

### Hash chain (M7.3, ADR-004)

Every record's `prev_hash`/`hash` chain exactly the way `proto/altavista/v1/envelope.proto`'s
`SignedBatch` documents: `prev_hash` is the previous record's own `hash`, or the literal
string `"GENESIS"` for the first record; `hash` is `SHA-256(prev_hash_bytes || body_bytes)`.
`EvidenceLog::verify` (`src/evidence.rs`) walks the file straight from disk and recomputes
every record's hash, reporting `{ok, checked, broken_at_seq, detail}` — a record's content
edited without recomputing its `hash`, or a rewritten `prev_hash`/`seq`, is detected and
reported at the exact `seq` it broke at. **Not cross-language byte-identical** with
`gmat_service.evidence`'s Python twin (JSON separator conventions differ between
`serde_json` and `json.dumps`) — each language's own log is independently, fully
self-verifying, which is what a replay or an assessor's chain-of-custody check over one
evidence file needs; see `src/evidence.rs`'s module doc for the full explanation.

```sh
cargo test -p av-dynamics-service --lib evidence::tests::verify_detects_a_tampered_record_and_reports_its_sequence_number -- --nocapture
```

## Admin API (`/admin/api/evidence`, ADR-004 question 63)

`src/admin.rs`: a hand-rolled, `GET`-only HTTP/1.1 server (`tokio::net::TcpListener`, no
`axum`/`hyper` dependency added — see that module's doc for why) on `--admin-port`
(default 50162), **localhost only**, same as the gRPC port:

- `GET /admin/api/evidence` — `version`, `settings_hash`, `run_id`, the evidence log's
  `chain_head`/`entries`, and `fips` (see below). Everything else 404s/405s.
- `GET /admin/api/evidence/verify` — runs `EvidenceLog::verify` and returns its result.

```sh
curl -s http://127.0.0.1:50162/admin/api/evidence | python3 -m json.tool
curl -s http://127.0.0.1:50162/admin/api/evidence/verify | python3 -m json.tool
```

### FIPS posture (`src/fips.rs`) — detected, not claimed

`openssl::fips::{enable,enabled}` (the OpenSSL 1.0.2-era `FIPS_mode` API) **does not exist
for this build at all** — the `openssl` crate only compiles that module for OpenSSL 1.x
(`#[cfg(not(any(libressl, ossl300)))]`), and this crate links OpenSSL 3.x. `fips::detect`
instead attempts `openssl::provider::Provider::try_load(None, "fips", false)` — OpenSSL
3.x's own mechanism for loading a named provider module — and reports the linked OpenSSL's
version alongside whether that load succeeded. **On this build (Homebrew `openssl@3`
3.6.3), the load fails**: `ls $(brew --prefix openssl@3)/lib/ossl-modules` lists only
`legacy.dylib`, no `fips.dylib` — this host/build has no FIPS-validated module available at
all. See `docs/compliance/av-dynamics-service/control-matrix.md`'s SC 3.13.11 row.

## Crypto/dependency accounting (ADR-004, question 86)

```
$ export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
$ cargo tree --workspace | grep -ci ring
0
$ otool -L target/debug/av-dynamics-service
target/debug/av-dynamics-service:
        @rpath/libGmatBase.R2026a.dylib (...)
        @rpath/libGmatUtil.R2026a.dylib (...)
        /usr/lib/libc++.1.dylib (...)
        /opt/homebrew/opt/openssl@3/lib/libssl.3.dylib (...)
        /opt/homebrew/opt/openssl@3/lib/libcrypto.3.dylib (...)
        /usr/lib/libiconv.2.dylib (...)
        /usr/lib/libSystem.B.dylib (...)
```

`tonic`'s dependency here has default features off with only `codegen`/`prost`/`server`
(no `tls`/`tls-native-roots`/`tls-webpki-roots`, which are the only tonic features that
pull `tokio-rustls` -> `ring`) — this crate never terminates TLS itself in the first place
(plaintext on loopback; the nginx front does TLS). `../../deny.toml` (repo root) bans
`ring`/`md-5`/`sha1`/`blake2`/`rustls`/`tokio-rustls` mechanically for the whole workspace,
not just this crate — see that file's own comments and this task's report for the
`cargo deny check` output.

## Golden `Propagate`

Measured (`.venv/bin/python -m pytest -q tests/test_dynamics_service_rs.py::test_propagate_matches_golden_within_tolerance -s`),
against `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s full 86,400 s arc, `sample_interval_s =
600`:

| | Result |
|---|---|
| Samples | 145 |
| Position error at t1 | ~4.06e-3 m (golden tolerance: 0.05 m) |
| Velocity error at t1 | ~3.92e-6 m/s (golden tolerance: 5e-5 m/s) |
| Covariance relative error at t1 (`covariance=true`) | ~3.5e-10 of the golden's own covariance norm |

Both comfortably inside the golden's own declared tolerance — see this task's report for
the exact numbers from the run that produced it, and
`crates/gmat-sys/tests/leo_golden.rs` for the same integration mechanism (Dopri5 over
GMAT's `GetDerivatives`) already proven against this same golden as a single, unchunked
call over the whole arc.

## Not implemented

`Solve` returns `UNIMPLEMENTED` (M6.2 scope, matching `gmat_service.service.Solve`).

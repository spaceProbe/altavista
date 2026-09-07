# gmat-service

> **M6.2 / `docs/adr/003-substrate-and-deployment.md`'s 2026-09-02 amendment: this service
> is now a design-time tool only, not part of a deployed profile.** `grpcio` bundles
> BoringSSL and cannot terminate TLS on the host's FIPS OpenSSL (see "TLS front door"
> below), so the amendment moved the *deployed* `altavista.v1.DynamicsService` host to
> Rust: **`crates/av-dynamics-service`** (ADR-002 depth 2, `"gmat-ffi"`), which never links
> `grpcio`/BoringSSL at all. This service remains exactly what it always was underneath —
> GMAT's own Python-API propagator (ADR-002 depth 1, `"gmat-api"`) — and stays in service
> for solvers, interactive authoring and golden generation (`goldens/gen_leo_1day.py` and
> friends), none of which need to run in a production profile. Its own tests
> (`tests/test_gmat_service.py`) and behaviour are unchanged by this; nothing below this
> note describes new behaviour.
>
> **M7.2:** `profiles/design.yaml` and `profiles/feasibility.yaml` (`docs/architecture.md`
> section 5's "Configurability: four profiles") list this service as a component of those two
> profiles only -- `profiles/analysis.yaml` and `profiles/execution.yaml` do not, and
> `tests/test_profiles.py` checks the execution profile mechanically for exactly that: no
> Python-hosted component anywhere in it. `crates/av-dynamics-service` (the Rust
> `DynamicsService` host this amendment moved the deployed path to) is the one dynamics
> component every profile lists, this one included -- see `profiles/README.md`.

A Python gRPC server implementing `altavista.v1.DynamicsService` (`proto/altavista/v1/dynamics_service.proto`)
by hosting GMAT R2026a through `altavista`'s Python API (`gmatpy`).

This is **ADR-002 depth 1**: "Python API in-process, now." `step` advances the state by
stepping GMAT's own propagator with state write-back; `propagate` does the same in a loop,
sampling the trajectory at a requested interval; `derivatives` evaluates GMAT's force model
directly (`ODEModel.GetDerivatives`). See `docs/adr/002-dynamics-contract.md` for how this
sits beside depth 2 (`crates/gmat-sys`, FFI to the C++ core) and depth 3 (native Rust
models, GMAT as the oracle).

The model this server hosts is pinned against `goldens/leo_1day_jgm2_8x8_sunmoon.json`:
Earth JGM2 8x8 gravity, Luna + Sun point masses, no drag, no SRP, PrinceDormand78 at
Accuracy 1e-13 / MaxStep 600 s. `Describe` reports this exact configuration's SHA-256 as
`settings_hash`.

## Running it

```sh
cd services/gmat-service
/path/to/AltaVista/.venv/bin/python -m gmat_service --port 50061
```

or, from anywhere, with the package directory on `PYTHONPATH`:

```sh
PYTHONPATH=services/gmat-service .venv/bin/python -m gmat_service --port 50061
```

Flags: `--port` (default 50061), `--evidence-path` (default
`services/gmat-service/evidence.jsonl`), `--run-id` (mainly for tests, which want a known
id to assert against).

`gmat_service` is a plain Python package (not published/pip-installed); it needs
`altavista` importable, which it is once this project's own `.venv` has been set up
(`altavista` is installed there in editable mode — see the repo root `pyproject.toml`).

## Security: plaintext on localhost, fronted by nginx mTLS (M5.3)

The server itself is unchanged since M2.2: it binds **plaintext gRPC on `127.0.0.1` only**
(`grpc.server(...).add_insecure_port`), and this is still correct -- see "TLS front door"
below for why the fix is not "add TLS to this process." Do not bind this to a
non-loopback address as it stands.

## TLS front door (M5.3, ADR-004): an nginx `grpc_pass` proxy, not TLS in Python

**Why not just call `grpc.server(...).add_secure_port(...)` with seccert material?**
Because `grpcio` bundles its own BoringSSL build -- there is no way to make it terminate
TLS through the system/host OpenSSL instead, short of a from-source build against a
different SSL backend that upstream `grpcio` does not support. ADR-004's crypto rule is
"the system FIPS OpenSSL only, no bundled crypto"; wiring mTLS into this process directly
would mean every handshake ran through BoringSSL, not OpenSSL. See "FIPS and crypto
accounting" below for exactly how far that gap reaches even with TLS moved elsewhere.

**What this milestone builds instead**: `services/gmat-service/deploy/nginx-gmat-grpc.conf.template`,
an nginx config template -- server certificate, `ssl_client_certificate` +
`ssl_verify_client on` for mandatory mTLS, TLS 1.2/1.3 with ECDSA-only cipher suites,
`grpc_pass` to this service's loopback plaintext port -- built the same way `secproxy`
fronts everything else in the SecRouter suite (nginx linking the host OpenSSL; see that
template's own header comment for the exact `secproxy` reference points, fetched from
`github.com/secrouter/secproxy` while building this). `crates/av-grpc` is the Rust
`tonic` client side: an OpenSSL-backed connector (`hyper-openssl`, never `ring` -- see
that crate's `Cargo.toml`/`tls.rs` doc comments), used both as a real client and, via its
`describe_client` binary, as `tests/test_grpc_tls.py`'s proof that the whole path works:
a local two-tier ECDSA P-384 test CA, a server + client cert, nginx started for real, and
`Describe` proven to succeed with the client certificate and to be refused without one.
Run it with `.venv/bin/python -m pytest -q tests/test_grpc_tls.py -v -s` (nginx must be on
the machine; the test skips with a clear message if it is not -- it does not xfail).

**Escalation: this is a service-owned front, not `secproxy` itself.** ADR-003 says "gRPC
sidecars and Redpanda are not fronted [by secproxy], like `secllm`" -- `secproxy`'s own
generator only ever emits `proxy_pass` blocks for the suite's HTTP/web services. This
milestone's template reuses `secproxy`'s *material* (nginx, host OpenSSL, a seccert-style
CA) as gmat-service's own dedicated front, because the crypto rule forces TLS out of
Python somewhere and there was nowhere else placed to put it that isn't a new component.
Two ways to resolve this are left for the lead: (a) keep it as a per-service template like
this one, one nginx instance per `grpcio`-based service that needs mTLS; or (b) teach
`secdeploy`'s generator a new fronted-component class for plaintext-gRPC-behind-mTLS
sidecars, so this stops being a one-off. Nothing here decides that; the template's
placeholders (`__SSL_CERTIFICATE__`, `__SSL_CLIENT_CERTIFICATE__`, etc.) are written so
either path can fill them.

## FIPS and crypto accounting (M5.3 escalation for the lead)

An honest accounting of this path against ADR-004's crypto rule ("SHA-256 only... the
system FIPS OpenSSL only, no bundled crypto"), not a claim that it is clean end to end:

**Clean:**
- The TLS/mTLS boundary itself (client <-> nginx) now runs entirely through nginx's
  linked OpenSSL -- measured here as Homebrew OpenSSL 3.6.3 (`/opt/homebrew/opt/openssl@3`,
  `nginx -V`'s `--with-http_ssl_module` build), the macOS eval-target equivalent of the
  FIPS-validated provider ADR-003 names for `fedora-fips` production. `ssl_verify_client
  on` makes every caller present a certificate; `ssl_ciphers`/`ssl_conf_command` in the
  template restrict to ECDSA-only, FIPS-approved AEAD suites (see the template's own
  comments for the exact list and why 1.2 is kept alongside 1.3).
- `crates/av-grpc`'s Rust client is confirmed OpenSSL-backed and `ring`-free:
  `cargo tree -p av-grpc | grep -i ring` returns nothing (empty match, see this task's
  report for the full tree), and `otool -L target/debug/describe_client` shows it linked
  against `/opt/homebrew/opt/openssl@3/lib/{libssl,libcrypto}.3.dylib` directly, not a
  bundled/vendored copy. This was the one thing this task existed to avoid shipping, and
  it is not in the tree.
- The evidence log (`gmat_service/evidence.py`) is SHA-256 via `hashlib` only, unaffected
  by any of this.

**Not clean -- real gaps, not points to explain away:**
1. **`grpcio` bundles BoringSSL, full stop.** This milestone moves the *TLS handshake*
   out of Python, but the `grpcio` Python wheel installed in `.venv` still statically
   links a BoringSSL build -- present as compiled object code in the environment whether
   or not this service ever calls `grpc.secure_channel`/`add_secure_port`. ADR-004's rule
   reads "no bundled crypto," not "no bundled crypto that is reachable" -- an SBOM/binary
   audit of this venv would still find BoringSSL symbols. Moving TLS to nginx closes the
   *network-facing* instance of the crypto rule for this service; it does not remove
   BoringSSL from the dependency graph. There is no drop-in fix for this within `grpcio`
   as distributed; it would need either an alternative Python gRPC implementation built
   against system OpenSSL (none evaluated here) or an accepted, documented exception.
2. **The gmat-service <-> nginx hop is plaintext HTTP/2 (h2c) over loopback**, by design
   (this service still refuses to bind non-loopback). That is the correct posture as long
   as both processes stay co-located on one host; it is called out here because it is a
   real plaintext hop that exists, not because it needs fixing today.
3. **The nginx build proving this is Homebrew's on macOS, not the FIPS-mode OpenSSL
   provider on `fedora-fips`.** ADR-003 names macOS as "the evaluation environment," and
   this milestone's proof is exactly that -- an eval-environment proof that the *pattern*
   (nginx on host OpenSSL, mTLS, ECDSA-only suites) works, not a FIPS-mode-operational
   proof (no `openssl.cnf` `fips_mode`/provider self-test coverage here).
4. **The CA and certificates `tests/test_grpc_tls.py` builds are local, throwaway test
   material** (a fresh two-tier ECDSA P-384 CA generated per test run), not seccert-issued.
   `services/gmat-service/deploy/nginx-gmat-grpc.conf.template`'s placeholders are the
   hook a real `secdeploy` wiring would fill with seccert material; that wiring does not
   exist in this repo yet (there is no Rust/Python `secdeploy` here to call).
5. **No repo-wide enforcement of "no `ring`" exists yet.** ADR-004 says "a CI check fails
   the build on a forbidden dependency"; this task verified `av-grpc`'s tree by hand
   (`cargo tree -p av-grpc | grep -i ring`). There is no `deny.toml`/`cargo-deny`
   configuration anywhere in this repo as of M5.3 -- a regression (someone enabling
   `tonic`'s `tls` feature later, say) would not be caught automatically. Flagged, not
   fixed, here -- introducing repo-wide `cargo-deny` config touches every crate's policy,
   which is a call for the lead, not a unilateral addition from this service's `deploy/`
   directory.
6. **No `docs/compliance/control-matrix.md` exists for gmat-service.** ADR-004 requires
   one per component; this was out of this milestone's explicit scope (M5.3's brief is
   the TLS front, not the compliance-evidence surface), but it is a pre-existing gap worth
   naming alongside the others above rather than leaving implicit.

## Threading: one GMAT configuration, one thread, for the life of the process

GMAT holds **one configuration per process** and is **not thread-safe** — every `gmatpy`
handle is effectively `!Send` (`docs/adr/002-dynamics-contract.md`, amendment
2026-09-02: "all handles are !Send; parallelism is by process"). `LoadScript` wipes the
configuration and `SaveScript` segfaults, and — as this service's own build process found —
GMAT objects built via anything other than `Construct()` (e.g. a bare
`PropagationStateManager()`) are kept alive only by their Python reference; letting one go
out of scope while a `ForceModel` still holds a raw pointer to it (via
`SetPropStateManager`) segfaults the *next* time that force model is used, not the call
that dropped the reference. See `gmat_service/model.py`'s module and inline comments for
the specifics this server works around.

Given that, `gmat_service/server.py` sizes the **entire** gRPC thread pool to exactly one
worker:

```python
server = grpc.server(futures.ThreadPoolExecutor(max_workers=1))
```

Every RPC — and therefore every call this process ever makes into `gmatpy`, from the very
first `gmat.Setup()` at warm-up onward — runs on that one thread. Concurrent requests
simply queue behind each other; there is no separate locking layer, because gRPC's own
dispatch already serializes everything. This is the simpler of the two options considered
("a `grpc.server` with a 1-worker thread pool, or a request queue drained by one thread")
and is acceptable because `DynamicsService` at depth 1 is a design-time / low-rate path —
the real-time hot path is depths 2 and 3, not this Python-API host.

`warm_up()` (loading GMAT and building this server's one force-model/propagator/derivative
configuration) is explicitly submitted to that same executor *before* `server.start()`, so
the first request never pays GMAT's model-build cost.

## What each RPC does

- **`Describe`** returns a fixed `ModelInfo`: id `gmat.earth.jgm2_8x8.sun_moon`, `depth =
  "gmat-api"`, `settings_hash`, and `goldens = ["leo_1day_jgm2_8x8_sunmoon"]`. Capabilities
  claimed: `DERIVATIVES`, `STEP`, `PROPAGATE`, `DETERMINISTIC`, `STM` (M3.2: `Propagate`
  propagates covariance, see below). **Never** `SOLVE` (unimplemented).
- **`Step`** advances one state by GMAT propagator stepping with write-back, chunked
  internally at the model's own `MaxStep` (see "A correctness finding" below). A non-empty
  `StepRequest.cov` still returns `FAILED_PRECONDITION` — the STM path (below) is wired into
  `Propagate` only, not `Step`'s per-substep case.
- **`Propagate`** returns a CDM `Trajectory`: SI metres, TAI nanoseconds, one
  `TrajectorySegment` naming the model and `dynamics_depth = "gmat-api"`, `Provenance`
  with `tool = "gmat-service"` and this process's `run_id`. `output_frame_id`, when set to
  anything other than this model's own `EarthMJ2000Eq`, is rejected (`UNIMPLEMENTED`) —
  there is no frame service hosted here to do the conversion. `ControlSegment`s and
  `Impulse`s are rejected too (`UNIMPLEMENTED`) — this RPC does not wrap
  `altavista.scenario.Scenario.maneuver`; that path exists for interactive scenario-building,
  not this service. `PropagateRequest.covariance = true` propagates covariance — see
  "Covariance" below.
- **`Derivatives`** evaluates GMAT's force model directly with
  `ODEModel.GetDerivatives(state, dt)`, matching ADR-002's own measured contract (the
  `dt` offset from a fixed reference epoch is bit-identical to rebuilding the model at the
  shifted epoch — verified while building this service). Implemented, not stubbed: it is
  cheap through the Python API (ADR-002 amendment: ~10.4 µs/call) and reachable without
  disproportionate work.
- **`Solve`** returns `UNIMPLEMENTED`. GMAT's differential corrector is the declared
  depth-1 candidate per ADR-002 but is not wired in here.

## Covariance: `Propagate(covariance=true)` (ADR-002 second amendment, M3.2)

`model.GmatModel.propagate_covariance` requests the STM from a dedicated Spacecraft/Propagator
pair (`_cov_obj`/`_cov_prop`, separate from the plain `Step`/`Propagate` pairs — see the
inline comment in `model.py` for why) via
`prop.GetPropStateManager().SetProperty("STM", sc)` before `PrepareInternals()`, exactly
`GMAT_API_Cookbook`'s "STM and Covariance Propagation" chapter and
`docs/teamlog/adr-002-amendment-draft-stm.md`. This grows every recorded sample's raw state
from 6 to 42 elements (row-major, index `6 + row*6 + col` is STM element `(row, col)`), which
`_run`'s existing chunked `Propagator.Step()` loop (question 77's chunking rule — see "A
correctness finding" below) already returns unmodified; `propagate_covariance` splits each
sample into the physical 6-state (unaffected — becomes `Trajectory.samples[i].mean`) and
`Phi(t0, t)` (36 elements), and computes `P(t) = Phi(t0, t) P0 Phi(t0, t)^T`
(`model._propagate_covariance`, explicitly symmetrized, reporting the pre-symmetrization
asymmetry via a log line) for `service.py` to attach as `Trajectory.samples[i].cov`.

**Depth 1's own mechanism, not a reproduction of the kernel's.** This is GMAT's own
propagator computing the STM directly — reading it from the raw 42-state array `gator.
GetState()` already returns *is* the mechanism at this depth (ADR-002: depth 1 hosts GMAT's
own propagator). This is different from the kernel (ADR-002 depth 2,
`crates/gmat-sys`/`crates/av-kernel`), which is required to integrate its own STM via
`GetDerivatives` rather than ever reading GMAT's back after the fact — the two are pinned
against the *same* golden (`goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `"stm"` block, itself
generated by running GMAT's propagator exactly this way) so they can be compared against one
shared reference, not against each other.

`seed.cov` (`GaussianState.cov`, row-major 6x6 SPD) is the declared P0 — **required** when
`covariance=true`; an empty `seed.cov` is `INVALID_ARGUMENT`, never a silent identity or zero
fallback (question 11: covariance is always explicitly requested).

**Measured** (`.venv/bin/python -m pytest -q tests/test_gmat_service.py::
test_propagate_covariance_true_matches_golden_stm -s`, against the same golden's full
86,400 s arc, `sample_interval_s = 600`, the golden's own declared diagonal SI P0 — (100 m)^2
position variance, (0.1 m/s)^2 velocity variance):

| | Result |
|---|---|
| Samples | 145 |
| `P(t0)` vs the declared P0 | exact (`Phi(t0,t0) = I`) to `< 1e-9` relative |
| Covariance at t1 vs `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.cov_t1_si` | max abs error 1.789e-3, **relative error 1.5e-12** |
| Mean (position/velocity) at t1 | same tolerance as the plain (non-covariance) `Propagate` test, unaffected by requesting covariance |

The relative error is nine orders of magnitude tighter than the kernel's own covariance
comparison (`crates/av-kernel/README.md`, 5.6e-10) because this service and the golden
generator both run *the same GMAT propagator the same way* — this is closer to a
determinism check than an independent-implementation agreement, which is expected and stated
plainly, not presented as if it were the stronger kind of validation the kernel's own
comparison is.

### Covariance hygiene (`docs/open-questions.md` question 80)

Every `P(t)` `propagate_covariance` computes is run through `gmat_service.covariance.
check_spd` — the Python-side mirror of `av_cdm::covariance::check_spd` (Rust,
`crates/av-cdm/src/covariance.rs`; see that module's README section for the full rationale
and the algorithm) — **before** the covariance is returned to `service.py` and, in turn,
before it is attached to `TrajectorySample.cov`. The check is finite -> symmetric
(`SYMMETRY_RTOL = 1e-9`, relative to magnitude, the identical formula both language
implementations use) -> a Cholesky attempt (`numpy.linalg.cholesky`), the same order and bar
`spoore_cdm::GaussianState`'s Rust constructor enforces. A failure raises a typed
`gmat_service.covariance.CovarianceHygieneError` subclass, is re-raised as a `ModelError`
(`FAILED_PRECONDITION`) by `propagate_covariance`, and increments the process-wide
`gmat_service.covariance.spd_check_failures()` counter — never a silent repair.

`propagate_covariance(..., nearest_spd_projection=False)` is the new, opt-in parameter
(default off): when `True`, a failing sample is replaced by `gmat_service.covariance.
nearest_spd`'s eigenvalue-clipping projection (logged at `WARNING`, still counted as a
hygiene failure via `spd_check_failures()`, and separately via `nearest_spd_projections_
applied()`) rather than the RPC failing outright. Not reachable through `PropagateRequest`
today — there is no proto field for it (`DrmOptions` has none, and `proto/**` is read-only
to this task); see `crates/av-cdm/README.md`'s "Covariance hygiene" section for the proposed
additive schema change.

`propagate_covariance` also now logs (`LOG.info`, alongside the existing max-asymmetry line)
the smallest Cholesky-diagonal² proxy seen across the whole run
(`gmat_service.covariance.CholeskyDiagnostics.min_cholesky_diag_sq`) — see that class's
docstring for exactly what it is (an upper bound on the true smallest eigenvalue, not the
eigenvalue itself) and is not, so drift toward singularity across samples or runs is visible
without an extra eigenvalue computation.

`gmat_service.covariance` has no `altavista`/`gmatpy` dependency (pure `numpy`), so its own
tests in `tests/test_gmat_service.py` run without the server subprocess or GMAT: a golden `P`
(this service's own `goldens/leo_1day_jgm2_8x8_sunmoon.json`'s `stm.p0_si` and `stm.
cov_t1_si` — the same two arrays the Rust side's equivalence test uses) passes; a hand-built
asymmetric matrix and a hand-built indefinite matrix each fail with their typed error and
increment the counter; the opt-in projection makes a previously-failing matrix pass
afterward. Equivalence to spoore's bar rests on the Rust side's direct proof against
`spoore_cdm::GaussianState::from_slices` (`crates/av-cdm/src/covariance.rs`'s
`tests::equivalence_*`) plus this module implementing the identical algorithm and being
tested against the same fixed fixtures — `spoore-cdm` is a Rust crate with no Python
binding, so a direct cross-language call is not possible.

## A correctness finding about `altavista.scenario.Scenario.propagate()`

While building `Propagate`'s sampling loop, this worker found that
`Scenario.propagate(step=...)` uses `step` for two different things at once: the size of
each external `Propagator.Step(dt)` call, and the trajectory recording cadence. Measured:
`Propagator.Step(dt)` does **not** sub-step internally to honour the propagator's own
`MaxStep` for one large external call — `gator.Step(3600.0)` in a single call gives a
different, physically wrong result from six chained `gator.Step(600.0)` calls covering the
same 3600 s (checked against `leo_1day_jgm2_8x8_sunmoon`: chaining at 600 s = `MaxStep`
reproduces the golden's final state to 0.0 m / 0.0 m/s; a single 3600 s external step
diverges by roughly 1e7 m over the full arc).

`altavista/scenario.py` is not owned by this worker, so this service does not call
`Scenario.propagate()` for `Step`/`Propagate`; `gmat_service/model.py` drives the raw
`Propagator`/`GetPropagator()` API directly, always chunking internal `Step()` calls at
`min(sample_interval_s, MaxStep)` and recording samples only at the caller's requested
cadence. This is reported as an escalation for `scenario.py`'s owner (a caller passing a
`step` larger than the propagator's `MaxStep` gets silently wrong results today), not
fixed in place.

## Evidence log (spoore ADR-005 pattern)

One JSON object per **successful** RPC response is appended to a local JSONL file (an
aborted/errored RPC has no response to log). Default path:
`services/gmat-service/evidence.jsonl`; override with `--evidence-path`.

```json
{
  "epoch": 1767225636999999868,
  "method": "Propagate",
  "request_hash": "…64 hex chars…",
  "response_hash": "…64 hex chars…",
  "settings_hash": "…64 hex chars…",
  "run_id": "b2c1…"
}
```

- `epoch` — TAI nanoseconds the record was **written** (wall clock, converted via
  `altavista.cdm.utc_ns_to_tai_ns`). This is the evidence record's own creation time, not the
  request's simulated epoch (already inside `request_hash` — e.g. `StateVector.tai_ns` /
  `GaussianState.epoch_ns`).
- `request_hash` / `response_hash` — SHA-256 hex of the request/response protobuf's
  **deterministic** encoding (`SerializeToString(deterministic=True)`), via `hashlib`.
- `settings_hash` — this server's `gmat_service.config.settings_hash()` at record time.
- `run_id` — one id per server process (a `uuid4` hex, unless `--run-id` overrides it).

SHA-256 only, via `hashlib` — no new crypto dependency, nothing bundled (ADR-004's crypto
rule). This is what lets a replay read the recorded answer instead of re-running GMAT.

## Tests

`tests/test_gmat_service.py` runs the server as a **separate process**
(`python -m gmat_service` via `subprocess.Popen`, on an ephemeral port), because GMAT is a
process-wide singleton and several tests in that file also load GMAT directly (through
`altavista.scenario`) in the *test* process, for an independent comparison. Readiness is
awaited with `grpc.channel_ready_future(...).result(timeout=...)` — a real poll, not a
bare sleep — and the subprocess is always terminated in the fixture's `finally` block.

`test_propagate_matches_golden_within_tolerance` is marked `@pytest.mark.slow` (not
actually slow here — the golden arc propagates in well under a second — but marked per the
task convention so a future slower golden doesn't block a quick `pytest -q -m "not slow"`
run). It still runs in a plain `pytest -q`.

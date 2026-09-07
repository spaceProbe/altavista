# AltaVista platform architecture — proposal

Status: proposal for discussion, 2026-09-02. Builds on altavista (this repo) and on
spoore (`~/code/spoore`). The published page version of this document has the diagrams.

## Summary

altavista proved one slice: build a mission in Python with GMAT, watch it in shared browsers.
The target is much larger: one configurable infrastructure for multi-domain mission
**design**, **analysis** and **execution**, with secure ingestion at the edge, a dual-track
processing engine, a unified Earth-and-orbit 3D viewer, and an isolated AI ecosystem
that can propose commands back to edge systems.

Spoore already made most of the hard decisions about *representing dynamic systems*
and about the substrate that carries them. The proposal is to adopt those decisions
unchanged, extend the common data model to cover orbital frames, design-time products,
heavy payloads and commands, and add the three things spoore never needed: an edge
security boundary, a custom Three.js geospatial client, and a command authority path.
GMAT contributes validated dynamics that must be portable into other simulation
engines, not a process the platform depends on.

Open questions and design forks are tracked in [open-questions.md](open-questions.md).

## 1. What spoore decided, and what we keep

| Spoore decision | Why it carries over |
|---|---|
| **Schema-first CDM** (`proto/spoore/v0/cdm.proto`): declared state spaces with labeled components, units and frame; Gaussian state with an event-time epoch; labeled Gaussian-mixture belief; measurement with provenance | It is the one language every domain shares. Nothing exists only as a native type. |
| **One model contract** (`Predictor` ⇄ `ModelService`): `warm_start`, `predict(to epoch)`, `measurement_likelihood`, `update`, `claim_testability` | Analytic filters in-process and learned models behind gRPC are the same thing to the engine. GMAT becomes a model behind the same contract. |
| **Models as declared, refutable claims** in a taxonomy-as-data tree (`trees/*.yaml`, ADR-006) | A domain is a subtree. Space is one more subtree, added by declaring nodes, not by changing the engine. |
| **Determinism** (ADR-004): engine output is a pure function of the ordered input log; event time only; seeds logged; model outputs logged as evidence so replay never re-runs a model (ADR-005) | Replay is how analysis, tests, audits and reproducible figures work. It is also how an after-action review of a command decision works. |
| **Substrate** (ADR-003): Redpanda (Kafka API), gRPC, ClickHouse, protobuf everywhere; single-threaded deterministic shard actors partitioned by spatial cell; ownership enforced at the publish boundary; every approximation counted; air-gap kit and SBOMs | Air-gap first is the binding constraint for defense deployments, and it was designed in from day one. |
| **Measured ADRs**, the assumption ledger, "what would falsify this" | The discipline that keeps a big system honest. We inherit the format. |

What spoore does not cover, because it never had to:

- Frames are Earth-local (ECEF, ENU, NED, body) and time is Unix nanoseconds. No inertial or planet-centred frames, no TAI/TT/TDB.
- No design-time products: no planned trajectory, maneuver, contact window, or solver run.
- No heavy payloads. Its largest object is a covariance matrix.
- Viewers are self-contained replay HTML (canvas), not a live geospatial client.
- No command path. It estimates; it never acts.
- Security is deployment posture (air-gap, SBOM), not a per-message, per-plugin model.

## 2. The representation: one entity from design to execution

A dynamic system is `(entity, frame, epoch, state space, belief, dynamics model, controls, evidence)`.
Design and execution differ only in **where evidence comes from** (a solver's goals versus
sensor measurements) and in **horizon**. Keeping one representation means a spacecraft
designed on Monday is the same entity being tracked on Friday, with its planned
trajectory as the prior.

CDM v1 keeps `spoore.v0` as a compatible subset and adds:

- **Frame** becomes a registry entry, not an enum: `{id, origin (body, platform or entity), axes, time scale}`. Day-one frames: ICRF/J2000 equatorial for Earth, Moon, Mars and Sun; ITRF/ECEF; ENU/NED at an origin; spacecraft and platform body frames; and **entity-relative RIC, VNB and VVLH frames** for RPO and OSAM (GMAT realizes these as `ObjectReferenced` axes). GMAT's `CoordinateSystem` is the reference implementation and the validator; conversion is a service call, never a convention.
- **Epoch**: `int64` nanoseconds on **TAI**, with a time-scale enum for boundary conversions (UTC, TT, TDB, GPS); one versioned leap-second table shipped with the platform and checked against GMAT's. Units are SI metres internally; kilometres exist only inside the GMAT adapter.
- **Entity**: a stable identity across domains and lifecycle (design object → planned → tracked). Track ids map onto entity ids through association and continuity evidence (spoore ADR-010: the claim is carried, the proof is re-earned).
- **Trajectory**: a design-time product. Samples of `GaussianState` (mean, covariance from the state transition matrix when the DRM asks for it), an interpolation contract (Hermite with velocity), segments and events. This is exactly what altavista produces today; it becomes a first-class message and a stored asset.
- **Rule**: profiles select components; they never change model behaviour or numerical settings. Anything that affects a result (covariance on or off, step rates, lateness windows, force-model choices) is declared in the DRM or system definition and hashed, so the same design gives the same numbers in every profile.
- **Event**: maneuver, contact window, eclipse, lifecycle, command acknowledgement. Typed, epoch-stamped, with provenance.
- **Command**: the one message with authority semantics (section 4).
- **Asset**: a reference to a heavy payload: object-store URI, content hash, size, media type, spatial and temporal extent, label. Messages carry references, never payloads (claim-check pattern).
- **Envelope**: every message carries a handling label (customer marking scheme), origin plugin id, and signature.

The model contract stays spoore's `Predictor`, with two design-time verbs added:
`propagate(seed, controls, horizon) → Trajectory` and `solve(problem) → Solution` for
targeting and optimization.

**GMAT as validated dynamics, not as a process.** GMAT's value is its validated force
models, integrators, coordinate systems, ephemerides and time systems. The architecture
must be able to carry those into a different simulation engine, so GMAT enters at three
depths, behind one dynamics contract, and the deployment picks the depth:

1. **Force models driven by our integrator.** GMAT's `ODEModel` exposes
   `GetDerivatives(state, dt, order)` (see `api/Ex_R2020a_BasicForceModel.py`), so the
   kernel's own integrator, or a third-party engine's, can call GMAT's gravity, drag, SRP
   and point-mass models as a derivative function. GMAT is Apache 2.0, so linking its C++
   core (`libGmatBase` and the force-model plugins) through a C shim into Rust is allowed.
2. **Whole propagator in-process.** The GMAT API's `Propagator` stepped from Python or
   C++, as altavista does today (state write-back reproduces a continuous run to 1e-11 km).
   Also carries GMAT's solvers for design-time `solve`.
3. **GMAT as the oracle.** Native Rust dynamics in the kernel, pinned against GMAT in CI
   with spoore-style goldens (position, velocity and STM agreement to declared tolerances
   over declared arcs). This is how the validated performance survives an engine that
   cannot link GMAT at all.

Whichever depth runs, the dynamics library is exported three ways for other engines: a C
ABI, gRPC (spoore's `ModelService` plus a `DynamicsService`), and an FMU. Outputs from any
out-of-process host are logged as evidence (ADR-005), so a replay reads the answer instead
of re-running the model.

The taxonomy gains a `space` subtree, subject to ADR-006's rule that a node is a
constraint its parent does not make and that the sensor can refute:

```
root
├── surface / airborne / ballistic / marine        (spoore, unchanged)
└── orbital — a = μ/r² (+J2), no control           refuted by thrust
    ├── orbital.high_fidelity — GMAT sidecar       full force model; earns depth on long arcs
    ├── orbital.maneuvering — IMM over coast/burn  refuted by nothing; it is the guard for burns
    └── reentry — drag-dominated, a = g − D        refuted by sustained altitude
```

## 3. From design reference mission to software in the loop

The design side starts from **design reference missions** (DRMs), must let us define and
reconfigure a system and a system of systems, and must carry the same dynamics into
real-time software-in-the-loop (SIL) and hardware-in-the-loop (HIL) runs against software
in containers, in Renode, and on dev boards. Three declared artifacts and one contract do
this.

Three artifacts, all data with a hash, so every run records exactly what it used:

- **System definition**: what a system is. Subsystems, typed ports (CDM message schemas, or raw byte streams such as UART and CAN frames for flight software), parameters, variants.
- **System-of-systems configuration**: which systems, connected how, and each system's **binding**: where it runs. Reconfiguration is a diff on this file.
- **Design reference mission**: a configuration plus a scenario (environment, timeline, events, faults) plus objectives and measures of effectiveness.

One dynamics contract, `step(state, controls, dt) → (state, outputs)` with
`derivatives(state, t) → state_dot` underneath it, alongside spoore's estimation contract.
GMAT's force models implement `derivatives` for the space domain and its propagators
implement `step`; design-time propagation and a real-time SIL step are the same call at a
different pace. Other domains
implement the same contract: 6-DoF air, ground vehicle, marine, RF links, 0-D power and
thermal, each as a service or an FMU. Sensor and actuator models sit on the contract's
outputs and produce CDM measurements, so the tracking engine downstream never knows whether
its input came from a field asset or a simulation.

The **simulation kernel** owns simulated time and routes ports between systems. A system's
binding is one of:

| Binding | What runs | How it connects |
|---|---|---|
| model | a dynamics model inside the kernel | in-process |
| container | flight or ground software (cFS, ROS 2, custom) in Docker/Kubernetes | gRPC or socket ports, lockstep-capable |
| Renode | the real binary on an emulated MCU or SoC | UART, CAN and Ethernet bridged to kernel ports; Renode's virtual time slaved to the kernel clock |
| board | the real binary on real hardware | the edge plugin's serial, CAN and Ethernet I/O: the same boundary as a field asset |

The ports do not change when the binding changes. That is the acceptance test: the same
binary in a container and in Renode must produce identical port traffic under lockstep.

Step rates are declared per system (default 10 Hz kernel dynamics, faster loops inside the
bound flight software), so the kernel is a multi-rate scheduler with interpolated sensor
outputs; GMAT-in-the-loop is supported at the default rate and native dynamics cover faster
settings. Sensor emulation is message-level first, signal-level where Renode's peripheral
models provide it. Fault injection covers ports, dynamics and environment, hardware (Renode
peripheral faults, board resets) and sensors, all declared in the DRM scenario as timed
events.

Time authority: the kernel runs **lockstep** for model, container and Renode bindings
(deterministic, replayable, faster or slower than real time) and **real-time pacing** as soon
as a board is bound, because the board's clock is the constraint. A HIL run logs the board's
I/O on the durable log so everything else still replays, the same move ADR-005 makes for
non-deterministic models. Mixed runs are the normal case: GNC on a board, ADCS in Renode,
propulsion as a model, the ground segment in containers.

Standards: FMI 3.0 co-simulation for packaging models where that tooling exists, SSP for
exchanging system-of-systems structure. Our YAML and proto stay the source of truth and
import and export both.

**Natural-language, LLM-integrated design is a platform goal.** Because every design input
is a declared artifact (DRM, system definition, system-of-systems configuration, sweep
specification, GMAT script template, view profile), an assistant can draft and edit them
through MCP tools, and the platform validates every draft the same way it validates a
human's: schema, frame registry, GMAT load, dry run. The assistant runs on secrouter (local
models via secllm by default; classification-gated egress to GovCloud tiers when allowed),
uses secagent's harness pattern, sees platform data only through the read-only MCP gateway,
and never executes a design or command without the review gate the profile declares. Its
authorship is recorded in the artifact's provenance and hash, so an LLM-drafted DRM is as
reproducible and auditable as a hand-written one. Acceptance is a golden set of
natural-language design requests with expected artifacts and measures of effectiveness,
run in CI (questions 66–70).

**Mission feasibility uses the same modes.** A feasibility study is a DRM plus a parameter
space: launch windows, delta-v budgets, sensor placements, link margins, fleet sizes, failure
injections. The job runner fans the DRM out across that space as batch kernel runs (models
bound, lockstep, as fast as the hardware allows), Monte Carlo seeds are logged inputs per
ADR-004, measures of effectiveness land in ClickHouse, and the viewer shows envelopes and
outliers over the same scene. Nothing is re-modeled for the study: the dynamics, sensor
models and configuration hashes are the ones the SIL and HIL runs use, so a feasible design
on paper is the design that later runs in the loop.

## 4. Reference architecture

Five planes. The hot track and the heavy track are the dual-track engine; the other
planes are the edge, the viewer, the AI ecosystem, and control.

### Edge and ingestion plane

- **Plugin** = a rootless podman container (UBI9 FIPS image) with one job: asset protocol → CDM messages. It declares its output schemas, frames and the label it emits under, and ships a signed manifest and SBOM. First plugin: ADS-B (receive-only); then CCSDS TM/TC with cFS, ROS 2, MAVLink, CSV/parquet replay. Board I/O is a driver interface (UART/USB-serial and Ethernet/UDP first; CAN, SpaceWire, 1553, RS-422 and GPIO later, nothing precluding them).
- **Security boundary at the edge node**: plugin sandbox (`--network none` plus its declared asset endpoint and the local edge log), mTLS with seccert-issued certificates, per-batch ECDSA P-384 signatures chained per plugin, label enforcement (a plugin cannot emit above its clearance), egress allow-list, hardware attestation where available. Single handling level per deployment; labels carried on every message anyway. Rejected, dropped and unsigned messages are counted, never silent.
- **Edge log**: a small Redpanda or embedded log holding hours of signed batches; the edge is capture-only while disconnected and replays everything in order on reconnect.
- **Dispatch**: the command return path terminates here. The edge node is the only component that talks to the asset in both directions.

### Processing plane: the dual track

- **Hot track**: Redpanda topics per domain and label; deterministic shard actors (spoore's engine, extended with the `space` subtree and an orbital partitioner: by regime and cell, not a planar grid); in-memory only; publishes tracks, events and residuals. Latency budget inherited from spoore (p99 ≤ 25 ms in-process, ≤ 75 ms through the bus at 1k measurements/s per shard) and re-measured with orbital models.
- **Heavy track**: object store (MinIO on-prem, S3 in cloud) for 3D payloads: imagery, point clouds, meshes, terrain, ephemerides. A job runner (Kubernetes Jobs or Argo Workflows) for tiling into 3D Tiles, ML batch runs, GMAT solves and analysis replays. An asset catalog (Postgres) with extents and labels. ClickHouse for analytics over both tracks.
- **The link**: hot-track messages carry `Asset` references; a heavy job's completion is an event on the hot track. Nothing on the hot path ever calls the object store.
- **Design services**: the GMAT service (solves and propagations as heavy-track jobs, results as Trajectory assets and events), the simulation kernel (section 3; batch runs on the heavy track, SIL and HIL runs on the hot track), and the replay service (ADR-004 replay from log offsets, producing scorecards).

### Presentation plane

- **Scene service**: subscribes to topics, maintains the current scene per mission profile, and streams deltas to browsers over WebSocket: entity states, events, asset manifests, and the shared clock from altavista. Interpolation (Hermite) happens client-side, as it does in altavista today.
- **Viewer**: a custom Three.js infrastructure grown from altavista, built as modules:
  - *Frame graph*: a scene node per declared frame, transforms from the frame service, the camera parented to any frame (Earth-fixed, inertial, body-centred, spacecraft-relative). Switching frames is re-parenting, not re-loading.
  - *Precision*: double-precision state on the CPU, floating-origin (relative-to-eye) rendering on the GPU, logarithmic depth. One scene holds from a dev board on a bench to LEO to interplanetary.
  - *Globe*: WGS84 ellipsoid quadtree with imagery and terrain tiles from our tile gateway, screen-space-error LOD, decoding in worker threads, budgeted loading.
  - *Streaming layers*: overlays arrive as tiles or chunks through a priority queue with a memory budget and cancellation, so a heavy geospatial payload never stalls a frame. 3D Tiles support through NASA AMMOS's `3DTilesRendererJS` is an option to evaluate, not a dependency.
  - *Entities*: instanced markers, trajectories as Hermite-densified wide lines, trails, covariance ellipsoids, sensor footprints, contact lines, labels with occlusion.
  - *Time*: the shared clock and interpolation contracts from altavista.
  - *Views as data*: a view profile (frames, layers, camera rigs) inside the mission profile.
  Overlays stream from the object store through a tile gateway that enforces labels; the browser only loads what is in view.
- **Multi-user**: shared clocks and shared scenes with roles; a command console appears only in the execution profile.

### AI plane

- **Model sidecars** (spoore `ModelService`) as rootless podman containers on UBI9 FIPS images: no egress, read-only volumes, placed on the secllm inference tier for GPUs (CPU-only ONNX by default, CUDA opt-in per model). Registered as tree nodes by YAML.
- **Data access gateway**: read-only, label-aware, exposed as an MCP server behind secrouter's deny-by-default tool allow-list (and as gRPC for numeric sidecars). Models cannot write to the log directly; their outputs enter through the evidence topic, attributed and versioned.
- **Language models** run through secrouter: local models on secllm by default, GovCloud tiers opt-in with classification-gated egress. Agents use secagent's harness with the platform's MCP tools as affordances.
- **Command proposals**: a model or agent may emit `CommandProposal` messages (an MCP tool that can only create the `proposed` state) and nothing else that acts. Propose-only until an envelope policy exists; enabling an envelope is a two-person policy change.
- **Training store**: a separate database for learning, filled only by reviewed export jobs (deterministic dumps with declared holdouts): Parquet datasets in a dedicated, separately labeled bucket with a Postgres catalog. A model version's provenance names its dataset hash.

### Command and authority

The command message moves through a logged state machine:

```
proposed → checked → authorized → dispatched → acked
                 ↘ rejected     ↘ expired     ↘ failed
```

- `checked`: OPA policy (label, asset, envelope, rate limits).
- `authorized`: a human, or delegated automation where the profile allows it.
- `dispatched`: the edge plugin translates to the asset protocol (MAVLink, CCSDS TC, ROS 2).
- `acked`/`failed`/`expired`: the asset's response, or the deadline.

Every transition is an event on the log, so a replay reproduces the decision trail, including what the model saw when it proposed.

### Control plane

A **mission profile** (YAML) declares plugins, trees, services, viewer layers, labels and authority policy. Deployment tooling renders it (see the SecRouter-suite alignment below); the air-gap kit packages it; every component ships an SBOM; OpenTelemetry and ClickHouse carry metrics and counters.

### Alignment with the SecRouter suite (decided, questions 61–65)

The user's SecRouter suite (`github.com/secrouter`) already provides the identity, trust,
naming, front-door, deployment and compliance-evidence tier for closed networks. The
proposal adopts it rather than re-inventing those pieces:

- **Machine identity and mTLS**: certificates from `seccert` (internal ACME CA) through
  standard ACME clients in every service and edge plugin; the seccert Root is the enclave
  trust anchor. Replaces the SPIFFE/SPIRE assumption.
- **Human identity**: OIDC via `secsso` (or the customer's IdP), with `groups` for roles and
  `amr`/`acr` for MFA on the viewer, consoles and command authorization.
- **Naming and front door**: `secdns` for `*.<domain>`, `secproxy` (nginx on the host's FIPS
  OpenSSL) fronting the viewer, scene service and tile gateway on `:443`.
- **Deployment**: AltaVista components as suite tiers in `suite.toml`, placed by
  `secsite.toml`, deployed by `secdeploy` (native hardened systemd on `fedora-fips`, fail-closed
  FIPS preflight, deploy-audit hash chain). Kubernetes/Helm becomes an alternative target, not
  the primary one. Sandboxing of plugins and model sidecars then rests on hardened units and
  rootless containers on FIPS base images rather than gVisor/Kata (question 62).
- **Compliance evidence**: each component ships a control matrix in the `secrouter` format
  (family, NIST 800-171 ID, implementation `file:function`, evidence command), exposes
  `/admin/api/evidence`, keeps hash-chained audit ledgers with a `verify` endpoint, uses SHA-256
  only and the system FIPS OpenSSL only; `secdeploy evidence` collects the bundle.
- **AI plane**: LLM traffic goes through `secrouter` (governance, budgets, classification-gated
  egress); the platform's read-only data access is exposed as MCP tools behind secrouter's
  deny-by-default tool allow-list; `secagent` is the pattern for command-proposing agents.
  Numeric `ModelService` sidecars are not LLM traffic and stay on gRPC.

## 5. Configurability: four profiles

Profiles are files first (a console that edits the same files comes later). They select
components, plugins, trees, viewer layers, labels and authority policy; they never change
numerical behaviour (section 2).

| Plane | Design | Feasibility | Analysis | Execution |
|---|---|---|---|---|
| Ingestion | none | none | replay from logs or files | live edge plugins, HIL boards |
| Hot track | off | off | replay engine | live engine, SIL/HIL kernel |
| Heavy track | GMAT solves, batch DRM runs | DRM sweeps, Monte Carlo, MoE aggregation | scorecards, tiling | tiling, ML batch |
| Viewer | design scene, trajectories, planning tools | envelopes, outliers, trade dashboards | replay with truth overlays | live scene, command console |
| AI | optional planners | surrogates, optimizers over the sweep | model evaluation | sidecars as tree nodes, proposals |
| Command | none | none | none | full path |

A profile is a file. A deployment can run more than one.

## 6. Technology choices

| Concern | Choice | Alternatives considered |
|---|---|---|
| Core engine | Rust; reuse spoore crates as dependencies | Rewrite in Python (rejected: latency, determinism) |
| Design and model authoring | Python (altavista lineage, spoore harness) | — |
| Viewer | Custom Three.js infrastructure (frame graph, floating origin, tiled globe, streaming layers), from altavista | CesiumJS (rejected: user direction; its precision and tiling machinery is rebuilt as modules we own) |
| Space dynamics | GMAT force models and propagators linked in-process (Apache 2.0), exported as C ABI, gRPC and FMU; native Rust models pinned against GMAT goldens | GMAT only as a sidecar process (kept as one depth, not the only one) |
| Log | Redpanda (Kafka API), pending legal review; `rskafka` as the Rust client (pure Rust, partition-bound, TLS via `rustls-openssl` on the host FIPS provider) | Kafka (JVM, air-gap cost); NATS (no ClickHouse ingestion); `rdkafka` (bundles librdkafka and OpenSSL) |
| Analytics | ClickHouse | Postgres-only (loses Kafka ingestion and columnar speed) |
| Object store | MinIO (S3 API), gigabytes to a few TB, single node; customer S3/GovCloud drop-in | filesystem store (no cloud drop-in) |
| Training store | Parquet datasets in a dedicated, separately labeled bucket + Postgres dataset catalog; filled only by reviewed export jobs | second ClickHouse; Postgres/Timescale |
| Catalog | Postgres (+PostGIS) | — |
| Wire | protobuf, gRPC, WebSocket to browsers; per-batch ECDSA P-384 signatures with hash chaining at the edge | per-message signatures (CPU cost); mTLS only (no post-export provenance) |
| Identity | seccert (ACME CA, enclave trust anchor) for every service and plugin; secsso or the customer IdP (OIDC, groups, MFA) for people | SPIFFE/SPIRE (retired: the suite already has an identity tier) |
| Policy | OPA | — |
| Isolation | hardened systemd units for platform services; rootless podman (Quadlet) on UBI9 FIPS images with `--network none` plus allow-lists for plugins and model sidecars | gVisor/Kata (retired: no orchestrator, and gVisor's netstack bypasses the host FIPS OpenSSL) |
| Embedded emulation | Renode (Cortex-M first; virtual time slaved to the kernel) | QEMU (weaker peripheral and multi-node story) |
| Model packaging | FMI 3.0 export in P2, SSP later | bespoke only (rejected: no interchange with existing tooling) |
| 3D Tiles | NASA AMMOS `3DTilesRendererJS`, vendored, behind our layer interface | own loader (months before overlays stream) |
| Deployment | SecRouter-suite tiers in `suite.toml`, placed by `secsite.toml`, deployed by `secdeploy` on `fedora-fips` (native systemd, FIPS fail-closed, deploy-audit chain); macOS eval target | Kubernetes/Helm (secondary target later) |
| Crypto rule | SHA-256 only; system FIPS OpenSSL only; no bundled crypto (rustls `ring` excluded; OpenSSL-backed TLS in Rust) | aws-lc-rs FIPS build only if a deployment accepts a second validated module |

## 7. Repository shape

```
altavista/
  proto/            cdm v1, system definition, sos config, drm, ingest, scene, command,
                    model_service, dynamics_service
  crates/           av-cdm (extends spoore-cdm), av-engine (wraps spoore-engine),
                    av-kernel (simulation kernel, clock, port router),
                    av-dynamics (dynamics contract; native models; gmat-sys FFI to
                    libGmatBase; C ABI and FMU exports),
                    av-ingest, av-scene, av-command, av-node
  services/         gmat-service (Python API host, solvers; from altavista),
                    dynamics (6-DoF, vehicle, links), tiler, replay
  goldens/          GMAT reference arcs that pin native dynamics in CI
  bindings/         container, renode, board (each: port bridge + clock adapter)
  plugins/          mavlink, ccsds, adsb, ros2, replay-csv, serial, can
  viewer/           Three.js infrastructure: frames, precision, globe, layers, entities, time
  drms/             design reference missions and system-of-systems configurations
  viewer/           Cesium app; Three.js inertial mode from web/
  profiles/         design.yaml, feasibility.yaml, analysis.yaml, execution.yaml
  trees/            taxonomies as data, including the space subtree
  deploy/           secdeploy tier definitions, systemd units, airgap data packs
  training/         export jobs and dataset catalog schema for the training store
  docs/adr/         numbered, measured, with "what would falsify this"
```

## 8. Phases

| Phase | Scope | Exit criteria |
|---|---|---|
| P0 | CDM v1 proto (frames, time scales, entity, trajectory, asset, envelope) plus system definition, system-of-systems configuration and DRM messages; the dynamics contract; ADRs 000–004 ratified; GMAT frame service validates frames; altavista Trajectory becomes a CDM message; `gmat-sys` FFI spike calling `GetDerivatives` from Rust | `cargo test` and proto lint green; an altavista scenario round-trips through CDM v1; Rust-driven integration of GMAT's force model matches GMAT's own propagator on a one-day LEO arc to a declared tolerance. **Status 2026-09-02:** ADRs accepted; proto compiles and round-trips (`tests/test_cdm_v1.py`); spike passed at 5.7 mm / 2.6 µs per call (`crates/gmat-sys`, ADR-002 amendment). Remaining: `av-cdm` crate with the spoore adapter, the frame service, the altavista Trajectory adapter. |
| P1 | Design and feasibility profiles: DRM authoring, GMAT service, scene service; viewer modules 1–3 (frame graph, floating origin, tiled globe); batch simulation kernel with GMAT and one non-space dynamics model bound as models; parameter sweeps on the job runner with MoEs in ClickHouse | the four altavista examples render on a tiled globe and in ICRF with no precision jitter at LEO or at Mars distance; a DRM run is reproducible from its configuration hash; a 100-run sweep reproduces its MoE table from logged seeds |
| P2 | SIL: kernel lockstep with container and Renode bindings; edge boundary (seccert certificates, per-batch signatures, labels); spoore engine with the space subtree consuming simulated measurements | the same binary in a container and in Renode produces identical port traffic under lockstep; replay bit-identical; p99 budget measured |
| P3 | HIL and the heavy track: a dev board bound through the edge plugin's serial/CAN I/O with real-time pacing; object store, tiler, streaming layers and entity module in the viewer, catalog, claim-check; native Rust orbital dynamics pinned against GMAT goldens | a board-in-the-loop run replays everything but the board from its logged I/O; a 10 GB overlay streams without frame drops; hot-path latency unchanged under load; native dynamics within tolerance of GMAT on every golden arc |
| P4 | AI plane and command path: sidecar isolation, read-only gateway, evidence log (closes spoore's open ADR-005 gap), command state machine with OPA and human authorization, dispatch to a SIL target | a model proposes, a human authorizes, a simulated asset acts, and the replay reproduces the decision trail |
| P5 | Air-gap kit, SBOMs, accreditation evidence package, multi-node scale | zero-egress install; 3-node throughput target met |

## 9. Decisions taken (2026-09-02)

All 71 forks in [open-questions.md](open-questions.md) are answered; the load-bearing ones:

- **Organization**: AltaVista (internal name); spoore as a git/path dependency with CDM v1 a compatible superset; Rust core, TypeScript viewer, Python authoring; open core (Apache 2.0) with proprietary plugins; first demo is a DRM on the globe.
- **Representation**: TAI nanoseconds, SI metres, Gaussian-mixture rule kept, DRM entity id authoritative with an asset catalog of aliases, strict proto compatibility from P0, RIC/VNB/VVLH entity-relative frames for RPO/OSAM.
- **Dynamics**: Python API in-process now, `gmat-sys` FFI spike in P0, native Rust in P3 pinned against GMAT goldens for gravity, third bodies, SRP, drag, relativity, tides and SPICE; our integrator validated against GMAT's; differential corrector as a platform `solve`; spoore's determinism standard.
- **SIL/HIL**: cFS first then ROS 2; Cortex-M Renode with lockstep required; UART and Ethernet first with nothing precluding CAN, SpaceWire, 1553, RS-422 and GPIO; ground segment is a system with the same bindings; FMI export in P2.
- **Security and deployment**: CMMC Level 2 / NIST 800-171 as the target; seccert + secsso; secdeploy on fedora-fips as suite tiers; hardened systemd plus rootless podman on UBI9; per-batch signatures; single handling level with labels; capture-only edges buffering hours; the suite's evidence conventions verbatim from P0.
- **Substrate**: Redpanda pending legal with `rskafka` as client; ClickHouse; MinIO; Parquet + Postgres training store; spoore's latency budgets re-measured; per-domain lateness windows declared per sensor.
- **Viewer**: custom Three.js; floating origin configurable for every frame; `3DTilesRendererJS` vendored; public and restricted basemaps from the start; WebGL2 now; Chrome/Edge/Firefox on integrated and discrete GPUs; tens of viewers with roles and shared/private clocks; ground tracks, footprints, contact lines and a 2D companion map first; fully offline build.
- **AI**: numeric sidecars on the inference tier's GPUs; LLMs through secrouter with local models by default; the assistant may author every artifact type with a human accepting the diff (two reviewers for profiles and policies); secagent harness with a natural-language golden set in CI; propose-only commands; role-gated authorization with time-limited delegations and SIEM export; deadline plus idempotency plus ack levels on commands.
- **Process**: spoore's ADR format (the user approves, I draft); self-hosted CI runners on x86-64 and arm64 with GMAT; paper-grade ADRs, memos when earned; files first, UI later.

Still open outside this list: the Redpanda legal review itself, the first demo's date, and the names of existing models to reuse for non-space domains (none named yet).

## 10. Risks

- Orbital frames inflate the CDM. Mitigation: conversion is delegated to the GMAT service and validated against it; the CDM only declares.
- Building the globe ourselves is real work: precision handling, tile LOD, terrain skirts, worker decoding. Mitigation: the modules are ordered so precision and frames land first (they are what Cesium cannot give us anyway), the globe second, and every module has a measured frame-time budget in CI.
- GMAT's C++ core was not built as a library for embedding: the API bundles it through SWIG and it holds global state (one configuration per process). Mitigation: the `gmat-sys` spike in P0 answers whether `GetDerivatives` can be driven re-entrantly from Rust; if not, depth 2 (API in-process) and depth 3 (native + oracle) carry the plan.
- Dual-track coupling. Mitigation: claim-check discipline and a CI test that fails if the hot path imports an object-store client.
- "Defense-grade" as a slogan. Mitigation: define it as tests: mTLS everywhere, signed messages, sandboxed plugins, label enforcement, egress checks in CI, all counted, spoore-style.
- GMAT is a per-process singleton. Mitigation: one GMAT per job container; parallelism through jobs.
- GMAT step latency in real-time SIL. altavista measured ~2 ms for 100 propagator steps in-process, but the gRPC hop and GMAT's initialization on state write-back must be measured at the target step rate before promising real-time pacing.
- Renode time coupling. Slaving Renode's virtual time to the kernel clock over its external interfaces is the least proven piece; prototype it in P2 before committing the acceptance test to it.

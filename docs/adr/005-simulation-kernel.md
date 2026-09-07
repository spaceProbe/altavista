# ADR-005: The simulation kernel

- **Status:** Accepted (drafted 2026-09-02 by the lead; accepted by the user 2026-09-02)
- **Date:** 2026-09-02
- **Plan reference:** [architecture.md](../architecture.md) section 3; questions 18–19, 23–29, 74, 87–90
- **Implemented by:** `crates/av-kernel` (clock, scheduler, DRM executor, expression evaluator), `crates/av-dynamics` (model contract), `bindings/` (P2), `proto/altavista/v1/system.proto`
- **Depends on:** [ADR-001](001-cdm-v1.md), [ADR-002](002-dynamics-contract.md); spoore ADR-004 (determinism)

## Context

Team 1 built the kernel's skeleton: an integer TAI clock, a lockstep multi-rate scheduler
over one model type, Hermite interpolation of outputs, CDM trajectory output, covariance
through the state transition matrix, and a DRM executor that loads, hashes, binds and runs
a design reference mission (measured against the golden arc to 0.1 mm). Four things were
deliberately not invented by the team because they are decisions, not code:

1. **Heterogeneous scheduling.** `Scheduler<M>` is generic over one model type. A
   multi-domain run (a spacecraft on GMAT dynamics, an aircraft on a 6-DoF model, a power
   subsystem on a 0-D model, a ground station as a container) needs one scheduler over
   many kinds (question 74).
2. **Scoring.** `Objective` and `MeasureOfEffectiveness` carry an `expression` string that
   parses and hashes but is never evaluated; feasibility sweeps cannot score a mission
   (question 90).
3. **Bindings beyond in-process models.** Container, Renode and board bindings are
   refused with a typed error; the port router and the lockstep protocol they need are
   undefined (questions 23–29).
4. **Interpolation beyond position and velocity.** Attitude now rides in the state vector
   (question 88), and covariance is available only at an instance's own step rate, because
   the interpolation contract stops at six components.

This ADR decides those four and records the rules the skeleton already follows.

## Decision

### 1. Models are trait objects behind one error type

`av_dynamics::DynamicsModel` gains an object-safe form: `Box<dyn DynamicsModel<Error =
ModelError>>`, where `ModelError` is one enum (numerical failure, capability missing,
binding transport failure, GMAT error) carrying the model id. A **model registry** maps a
model id from a `SystemDefinition.dynamics_model` to a constructor: native models, GMAT
models through `gmat-sys`, a remote `DynamicsService` stub (gRPC through the ADR-003
front), and later FMUs. The scheduler is generic no more: it holds `BTreeMap<InstanceName,
Box<dyn DynamicsModel>>`, so instance order, and therefore output order, is the sorted
instance name, never insertion or hash order.

### 2. Time and rates

- The kernel clock is integer TAI nanoseconds (ADR-001). The **base period** is the greatest
  common divisor of every instance's `step_rate_hz` period and the DRM's output period;
  every instance period must be an integer multiple of the base period, checked at load,
  refused otherwise with the offending instance named.
- An instance steps at every multiple of its own period; the kernel ticks at the base
  period and visits due instances in sorted name order.
- **Lockstep** is the default: no instance advances past the kernel clock, and the run is a
  pure function of the ordered inputs (spoore ADR-004). **Real-time pacing** is entered
  only when the configuration binds a board, and then the kernel clock is slaved to wall
  time at 1:1 with the deviation counted; a HIL run is replayable only from the logged
  board I/O (question 18).

### 3. Interpolation contract, by component class

A `StateSpace` declares every component; the kernel and the viewer interpolate by class:

| Component class | Rule |
|---|---|
| position and velocity (first six of a Cartesian space) | cubic Hermite with velocity (`INTERPOLATION_HERMITE_VELOCITY`) |
| unit quaternion (four components labeled `q_x, q_y, q_z, q_w`) | normalized spherical linear interpolation (slerp) between neighbouring samples; unit norm asserted |
| rates, masses, scalars | linear |
| state transition matrix, covariance | **never interpolated**: available only at the instance's own samples |
| discrete modes, counters | zero-order hold |

Every `Trajectory` therefore travels with its `StateSpace` (question 88): a consumer that
cannot classify a component refuses to interpolate it rather than guessing.

### 4. Ports, the router, and bindings

- A **port** is typed (`PORT_KIND_CDM`, `FRAMED`, `BYTE_STREAM`, `SIGNAL`) with declared
  timing. The **router** connects ports per `SosConfiguration.connections`, applies the
  optional link model (latency, loss, bandwidth: a dynamics model like any other), and
  delivers at the receiving instance's next step in deterministic order.
- **Bindings** implement one interface: `Bound { fn step(&mut self, until_tai_ns: i64,
  inputs: &Inbox) -> Result<Outbox, ModelError> }`.
  - `MODEL`: the in-process trait object above.
  - `CONTAINER`: a gRPC **lockstep protocol** (`LockstepService`: `Step(until_tai_ns,
    inputs) -> outputs`, blocking, one outstanding step per instance) over the ADR-003
    transport; a container that cannot honour it declares `lockstep_capable = false` and is
    refused in lockstep runs.
  - `RENODE`: a bridge process that owns the Renode instance, slaves Renode's virtual time
    to the kernel through its external interface, and speaks the same lockstep protocol
    to the kernel; UART/CAN/Ethernet ports map to Renode peripheral bridges.
  - `BOARD`: the edge node's I/O drivers; real-time pacing; every input and output logged
    as a signed batch (ADR-004) so the rest of the run replays.
- A binding the kernel cannot honour is a typed refusal at load, never at step 10,000.

### 5. Faults

Faults are scenario events, hashed with the DRM and injected at their epochs in sorted
`(epoch, id)` order: `DYNAMICS` sets a model parameter through the model's declared
parameter interface; `PORT` acts in the router (drop, delay, corrupt, duplicate);
`SENSOR` acts in sensor models (bias, noise, dropout, misalignment); `HARDWARE` acts
through the binding (Renode peripheral faults, board reset and power cycle). Every random
element draws from a seeded PCG64 keyed by `Scenario.seeds[<fault id>]`, so the same DRM
produces the same fault realization on every run and on every host.

### 6. Scoring: the expression language

`Objective.expression` and `MeasureOfEffectiveness.expression` are written in a small,
**total, deterministic, unit-checked** expression language evaluated after a run over its
products. Grammar (EBNF):

```
expr     := term (('+' | '-') term)*
term     := unary (('*' | '/') unary)*
unary    := '-' unary | primary
primary  := number [unit] | call | ref | '(' expr ')'
call     := name '(' [expr (',' expr)*] ')'
ref      := ident ('.' ident)* ['@' time]
time     := 'start' | 'end' | number 's' | ident        (an event name resolves to its epoch)
number   := decimal literal; unit := one of the CDM Unit names (m, m/s, rad, s, kg, ...)
```

References resolve against run products: `entity.<id>.<component>@time` (a state
component at an epoch, interpolated per section 3), `range(<a>, <b>)@time`,
`event.<name>.t`, `count(event.<kind>)`, `output.<instance>.<name>@time`. Aggregates over
the run: `min(ref)`, `max(ref)`, `mean(ref)`, `final(ref)`, `integral(ref)`,
`duration(condition)`. Comparisons yield 0 or 1 and may be aggregated (`duration(range(a, b)
< 100 m)`). Units propagate through arithmetic and a unit mismatch is a parse-time error,
not a runtime surprise. No loops, no recursion, no assignment, no side effects, no access
outside the run's products: an expression is a pure function of the run and is hashed with
the DRM.

An `Objective` passes when `|value - target| <= tolerance`. A `MeasureOfEffectiveness`
evaluates to a value with a unit, recorded per run in ClickHouse with the run id and the
DRM hash, which is what feasibility sweeps aggregate.

### 7. Provenance and hashing

The canonical hash of a DRM, configuration or system definition is SHA-256 over its
deterministic protobuf encoding with its own `hash` field cleared; the executor refuses an
artifact whose stored hash disagrees. Every output carries the DRM, configuration, system
definition and data-pack hashes and a run id in its `Provenance`; the expression that
produced a score is hashed with it.

## Alternatives considered

**An FMI co-simulation master as the kernel.** Interoperable with Modelica and Simulink
tooling from day one. Rejected as the kernel because FMI's clock and step semantics would
constrain the multi-rate scheduler, the port kinds (byte streams, signals) and the
determinism rules; FMI stays an export and import format (question 23).

**Keeping the scheduler generic over one model type.** Simplest code. Rejected because a
multi-domain run is the platform's purpose, and monomorphizing over an enum of every model
kind would put every domain's dependencies into one type.

**A general scripting language for objectives (Lua, Rhai, Python).** Expressive. Rejected
because scores must be total, deterministic and hashable, and because scripts inside a
CUI-handling kernel are an attack surface the evidence pattern would have to cover. The
expression language can grow (piecewise functions, vector operations) without becoming a
program.

**Interpolating the state transition matrix.** Would give covariance at any output epoch.
Rejected: there is no interpolation of Φ that preserves its meaning between integrator
steps; covariance is sampled where it is computed, and a DRM that needs it more often steps
that instance faster.

**Actor-per-system with message passing as the kernel's core.** Natural for bindings.
Deferred rather than rejected: bindings already look like actors behind the `Bound`
interface; the kernel's tick loop stays a deterministic single thread (spoore ADR-004) and
parallelism is by process.

## Consequences

**A refactor of `av-kernel` and `av-dynamics`**: trait objects and a registry replace the
generic scheduler; existing tests must pass unchanged on the golden arc.

**`Trajectory` messages must carry their `StateSpace`.** The gmatviz adapter and the kernel
emit it; the viewer refuses to interpolate a component it cannot classify.

**The expression evaluator is a new component with its own goldens**: a set of DRMs with
known scores pinned like the dynamics goldens.

**Bindings land in P2 with the protocol defined here**; the container binding's acceptance
test (same binary in a container and in Renode, identical port traffic under lockstep,
question 25) is the protocol's test.

**Real-time pacing is a mode of the same kernel**, not a second kernel; the only difference
is who owns the clock, and that difference is counted.

## What would falsify this

The premise is that one deterministic tick loop over trait-object models, with a
total expression language for scoring, serves design, feasibility, SIL and HIL without a
second engine.

The observables: a domain model whose step cannot be expressed as `step(until, inputs)`
without violating lockstep (a continuous-time co-simulation that needs rollback, for
example, which would force a rollback-capable scheduler); a feasibility study whose
measures cannot be written in the expression language without loops (which would mean the
language needs vector operations or the study needs a batch job, and the ADR must say
which); or a HIL run whose real-time deviation counter shows the single-threaded tick loop
cannot keep up with the board at the declared rate.

## Amendment 2026-09-02: the grammar corrected by its implementation

Team 1's M8.2 implemented section 6's EBNF as written and found two defects the lead's
draft carried: `call` had no `@time` suffix (so the section's own `range(a, b)@time` was
unparseable), and comparison operators did not exist at all, although the section's worked
example is `duration(range(a, b) < 100 m)`. The grammar is corrected as follows; the
semantics in the rest of section 6 are unchanged.

```
expr     := comparison
comparison := sum [('<' | '<=' | '>' | '>=' | '==' | '!=') sum]
sum      := term (('+' | '-') term)*
term     := unary (('*' | '/') unary)*
unary    := '-' unary | postfix
postfix  := primary ['@' time]
primary  := number [unit] | call | ref | '(' expr ')'
call     := name '(' [expr (',' expr)*] ')'
ref      := ident ('.' ident)*
time     := 'start' | 'end' | number 's' | ident        (an event name resolves to its epoch)
```

A comparison yields a dimensionless 0 or 1 and requires both sides to carry the same unit
(or both dimensionless); it is not associative, so `a < b < c` is a parse error. `@time`
applies to any postfix expression, so `range(a, b)@end` and `entity.sat.pos_x@start` are
both well-formed. Correction, same day: when this amendment was first written the evaluator
did *not* yet implement it (its lexer had no comparison tokens and a test pinned
`range(a, b)@end` as unparseable); team 1's M9.1 implemented the corrected grammar and
replaced those tests. As of M9.1 the evaluator in `crates/av-kernel/src/expr` implements it
and `goldens/expr_range_duration.json` pins the worked example
(`duration(range(a, b) < 100 m)` = 5.8 s). An amendment must describe the state at the
time it is written, and this one briefly did not.

Two implementation choices from M8 are ratified here: `Kernel::run_with_covariance` takes
the per-instance physical dimension explicitly (the STM-augmented state does not carry it),
and `BoxedModel` remains `!Send` like every GMAT-backed model, so parallelism stays by
process.

## References

- `crates/av-kernel` (clock, scheduler, DRM executor, question 87), `crates/av-dynamics`
  (`DynamicsModel`, `StmAugmented`, `propagate_covariance`), `drms/leo_1day_golden.*.yaml`.
- spoore ADR-004 (determinism), `crates/spoore-scenarios` (`DeterministicRng`, PCG64).
- Modelica Association, *FMI 3.0* (co-simulation with clocks), *SSP 2.0*.
- Renode documentation: external time control and peripheral bridges (UART, CAN, Ethernet).
- Kleppmann, M. *Designing Data-Intensive Applications*, ch. 11 (deterministic processing over an ordered log).

# spoore upstream proposals

AltaVista depends on spoore's CDM crate by path (question 12: spoore crates by dependency,
CDM v1 a compatible superset, changes spoore needs go upstream as PRs). Question 75 recorded
three changes that spoore should carry itself. They are now implemented as three commits on
the `altavista-upstream` branch of the spoore checkout at `/Users/probe/code/spoore`, one
commit per proposal so each can go up as its own PR.

| Commit | Proposal (question 75) | Files | Wire or behaviour change |
|---|---|---|---|
| `ea19659` | `spoore-cdm/build.rs` generates `BTreeMap` for every proto `map<...>` field (`prost_build::Config` with `.btree_map(["."])`), so `Measurement.meta` encodes in key order instead of `HashMap`'s per-process random order. spoore's ADR-004 already forbids unordered iteration on any output path and names `Measurement::meta` as the example. | `crates/spoore-cdm/build.rs` | Wire bytes become deterministic; no call site changed. |
| `ccf8d23` | `spoore/v0/cdm.proto` states the time scale of `epoch_ns` on `TrackSeed`, `TrackHandoff` and `TrackUpdate` ("Event time, nanoseconds since the Unix epoch."), matching the comment `GaussianState` and `Measurement` already carry and `spoore_cdm::state::Epoch`'s own doc. | `proto/spoore/v0/cdm.proto` | Comment only; no renumbering, no wire change. |
| `9d732cc` | `spoore_cdm::state::SYMMETRY_RTOL`, `validate_covariance` and `check_positive_definite` become `pub` with doc comments and are re-exported from `spoore_cdm`, so a downstream crate can apply spoore's canonical covariance check (same checks, same order, same tolerance) to a matrix that is not yet a `GaussianState`. | `crates/spoore-cdm/src/state.rs`, `crates/spoore-cdm/src/lib.rs` | Visibility only. |

## Verification

Each commit was checked in the spoore workspace: `cargo build --workspace --all-features
--all-targets` and `cargo clippy --workspace --all-features --all-targets` clean, `cargo test
--workspace --all-features --no-fail-fast` 1090 passed / 25 ignored at every step, identical to
the pre-change baseline, and `cargo xtask ledger --check` and `cargo xtask goldens --check`
passing after the third commit. With the branch checked out as AltaVista's path dependency,
`cargo test -p av-cdm` passes 47/47 unchanged.

## What AltaVista does until the PRs land

- `av-cdm` keeps its own covariance check, proven equivalent to spoore's in M4.2 (question 75).
  Replacing it with the re-exported spoore functions is a one-line change per call site once
  the third commit is on spoore's main; doing it earlier would make AltaVista build only
  against this branch.
- The path dependency follows whatever branch the spoore checkout has out. The branch is a
  superset of main at the source level (no call site changed, no field renumbered), so
  AltaVista builds and tests identically against either.
- `Measurement.meta` ordering only matters for spoore's own encoding path. AltaVista's
  `spoore.v0` adapter tests in `crates/av-cdm/tests/spoore_v0.rs` are unaffected either way.

## Submitting

Open the three PRs from `altavista-upstream` in the order above (each is independent; the
order only matches the numbering in question 75). Commit bodies carry the rationale and the
measured gate counts, and reference spoore's ADR-004 where it applies. No AltaVista change is
needed when they merge; record the merge in question 75 and delete the branch.

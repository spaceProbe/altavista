# third_party

Sources fetched for building, not vendored in git.

| Folder | What | How to get it | Used by |
|---|---|---|---|
| `gmat-src/` | GMAT R2026a source, `src/base` and `src/gmatutil` only (headers for the C++ shim) | `sh third_party/fetch-gmat-src.sh` | `crates/gmat-sys/build.rs` (`GMAT_SRC` overrides the path) |
| `cspice/include/` | CSPICE toolkit headers (NAIF, public domain); GMAT's headers include `SpiceUsr.h` under `__USE_SPICE__`, which the shipped binaries were built with | `sh third_party/fetch-cspice.sh` | `crates/gmat-sys/build.rs` (`CSPICE_INCLUDE` overrides) |
| `cfs/` | NASA cFS (Apache 2.0): the bundle build scaffolding plus the `cfe`/`osal`/`psp` submodules only, pinned at the bundle tag `v7.0.1` (`088b2fa828db9ff7e00733f1908e0eeb59f66ce3`; submodule commits recorded in the script's own header) | `sh third_party/fetch-cfs.sh` (also run automatically, inside the permitted network window, by `services/cfs/Dockerfile`) | `services/cfs/` (M23.2, `docs/open-questions.md` questions 147/148/154) |

The GMAT binaries themselves live in the install next to this repo (`GMAT R2026a/`, override
with `GMAT_ROOT`); the shim links `libGmatBase` and `libGmatUtil` from the app bundle's
`Frameworks` folder (`GMAT_LIB` overrides).

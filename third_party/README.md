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

## cFS local mirror (question 196(c))

`fetch-cfs.sh` records a local bare mirror of each of the seven pinned cFS repos (the bundle plus
`cfe`/`osal`/`psp`/`tools/tblCRCTool`/`tools/elf2cfetbl`/`tools/commandline-tools`) under
`third_party/mirrors/<name>.git` (`CFS_MIRROR_DIR` overrides). That directory is gitignored --
fetched, never committed, same convention as `third_party/cfs/` itself. The mirror is only ever
cloned from `github.com` the FIRST time a given host runs `sh third_party/fetch-cfs.sh` and does
not already have it (the one-time network window question 154 permits); every fetch after that,
and `services/cfs/tests/test_clean_fetch_patches.py`, clones from the local mirror path and never
touches the network.

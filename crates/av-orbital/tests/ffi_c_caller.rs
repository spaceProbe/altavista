//! N6 deliverable 1's own acceptance test (`docs/native-dynamics-plan.md`: "a test that a C
//! caller obtains the same derivative bytes"): compiles and links the real, committed C file
//! `tests/c/av_orbital_ffi_test.c` against the real `libav_orbital.a` Cargo has already built
//! (the `staticlib` crate-type this task's own `Cargo.toml` change adds), runs the resulting
//! executable, and compares the raw IEEE-754 bit pattern of every `state_dot` component it
//! prints against the SAME derivative computed through the ordinary Rust `av_dynamics::
//! DynamicsModel::derivatives` path -- bit for bit (`f64::to_bits`), never within a tolerance.
//!
//! No cargo feature gate: this file builds and runs under `cargo test -p av-orbital
//! --no-default-features` too (the model it exercises, `EarthGravityModel<fk5::
//! Fk5BodyFixedRotation>`, has no `gmat-sys` dependency at all -- see `src/ffi.rs`'s own doc
//! comment, "Why `Fk5BodyFixedRotation`, never `GmatBodyFixedRotation`").
//!
//! # How the C file is compiled and linked
//!
//! No `build.rs`/`cc`-crate step compiles it -- per this task's own brief, "the simplest honest
//! way is a Rust integration test that invokes the system `cc`". Three `cc` invocations, all
//! visible in this file's own [`run_cc`] calls:
//!
//! 1. `cc -c -I<include dir> tests/c/av_orbital_ffi_test.c -o <tmp>/av_orbital_ffi_test.o`
//! 2. `cc <tmp>/av_orbital_ffi_test.o <deps>/libav_orbital-<hash>.a <native-static-libs>
//!    -o <tmp>/av_orbital_ffi_test`, where `<deps>/libav_orbital-<hash>.a` is the real,
//!    already-built staticlib archive, LOCATED (never assumed) by [`find_staticlib`] under the
//!    directory `AV_ORBITAL_LIB_DIR` names (a compile-time environment variable `build.rs`
//!    emits -- see that file's own doc comment, "N6's second job") -- see [`find_staticlib`]'s
//!    own doc comment for why this is a full path, never `-L<dir> -lav_orbital`. The
//!    `<native-static-libs>` are [`NATIVE_STATIC_LIBS`] below -- the exact set of system/vendor
//!    libraries linking against a Rust `staticlib` built from this crate's own dependency graph
//!    needs, measured (never guessed) via `cargo rustc -p av-orbital --lib --no-default-features
//!    -- --print=native-static-libs` on this host and recorded with that measurement (see
//!    [`NATIVE_STATIC_LIBS`]'s own doc comment for the exact command and output this crate was
//!    built with when that measurement was taken, and the platform it is therefore scoped to).
//! 3. The resulting executable is run directly (no further `cc` step) as `<tmp>/
//!    av_orbital_ffi_test <gravity_file_path> <gmat_root>`.
//!
//! `av_orbital_ffi_test.c` is compiled from a file committed in this repository (never
//! generated), and no step above fetches anything over the network (question 154) -- only the
//! system `cc` already required to build any Rust binary on this platform (`rustc` itself
//! shells out to it as the platform linker) and the artifacts `cargo test` has already built.
//!
//! # Bit-identical, and why that is possible at all
//!
//! The comparison in [`c_caller_obtains_the_same_derivative_bytes_as_the_rust_path`] is
//! `f64::to_bits(rust_value) == u64::from_str_radix(c_hex_line, 16).unwrap()` for all six
//! `state_dot` components -- the RAW bit pattern, not a value compared within an epsilon. This
//! is possible (not merely hoped for) because both sides run the IDENTICAL compiled code path:
//! the C caller's `av_orbital_model_derivatives` and this test's own `EarthGravityModel::
//! derivatives` call both bottom out in the exact same monomorphised `EarthGravityModel<
//! Fk5BodyFixedRotation>::derivatives` machine code (`src/ffi.rs`'s `av_orbital_model_derivatives`
//! is a thin, non-computational wrapper -- pointer/length checks, then one call into that same
//! method with an empty controls slice, since this model takes none) -- there is no second,
//! independently-implemented "C version" of the physics to agree
//! with only approximately. A future change that genuinely introduced a second code path (e.g.
//! parallel C and Rust implementations of the same force) would need a tolerance here instead;
//! today there is exactly one implementation on both sides of this comparison, so there is no
//! floating-point non-associativity, reordering or precision difference to bound.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use av_dynamics::DynamicsModel;
use av_orbital::fk5::Fk5BodyFixedRotation;
use av_orbital::model::{EarthGravityModel, EarthGravityModelInfo};

/// The exact native libraries `cc` must additionally link against `libav_orbital.a` for the
/// linked executable to resolve every symbol the archive itself does not define -- Rust
/// `staticlib`s bundle only THIS crate's (and its Rust dependencies') own compiled code, never
/// the system/vendor libraries those dependencies call into (unlike a `cdylib`, which resolves
/// and records its own runtime dependencies; see `src/ffi.rs`'s own doc comment for why this
/// crate exports a `staticlib` and not a `cdylib` for this hedge). This exact list was measured,
/// never guessed, by running (on this host, macOS arm64, `rustc 1.90.0`-class toolchain per
/// this repository's own pinned `rust-toolchain`):
///
/// ```text
/// cargo rustc -p av-orbital --lib --no-default-features -- --print=native-static-libs
/// ```
///
/// which prints (to stderr, as a `note:`) the exact set this constant reproduces. The only
/// native dependency in `av-orbital`'s own dependency graph is `openssl-sys` (ADR-004's crypto
/// rule: SHA-256 through the platform OpenSSL, `openssl::sha::sha256`, used by `src/cof.rs`'s
/// and `src/model.rs`'s own gravity/DE-file digests) -- `-lssl`/`-lcrypto` below are that, found
/// at `/opt/homebrew/opt/openssl@3/lib` (this host's Homebrew OpenSSL 3, also passed as an
/// explicit `-L` since it is not a default linker search path); the rest are the ordinary
/// system libraries any linked Rust binary on this platform needs.
///
/// Scoped to macOS (`cfg(target_os = "macos")`) because that is the only platform measured;
/// [`gate`] below skips the whole test VISIBLY (never silently) rather than guess a list for a
/// platform nobody has run this command on.
// Not itself #[cfg(target_os = "macos")]-gated (only gate()'s own OS check is): these values
// would be wrong on another platform, but gate() always returns Some(..) and this test always
// returns before build_c_test_binary() ever reads them there, so leaving the declarations
// unconditional keeps this file compiling everywhere while only ever USING the values on the
// one platform they were measured on.
const NATIVE_STATIC_LIBS: &[&str] = &["-lssl", "-lcrypto", "-liconv", "-lSystem", "-lresolv", "-lc", "-lm"];
const NATIVE_STATIC_LIB_SEARCH_PATHS: &[&str] = &["/opt/homebrew/opt/openssl@3/lib"];

/// The same LEO-shaped state and epoch `tests/c/av_orbital_ffi_test.c` uses -- kept in sync BY
/// HAND (there is no shared fixture format between a `.c` file and a Rust test in this
/// workspace); a mismatch here would make this test compare two DIFFERENT derivative
/// evaluations and either fail loudly (most components) or pass by coincidence on the
/// position-derivative-equals-velocity slice alone, so both files also assert that slice
/// independently as a sanity check of their own inputs.
const STATE: [f64; 6] = [6_878_000.0, 0.0, 0.0, 0.0, 7_500.0, 1_000.0];
// 2026-09-02, crate::fk5's own module test constant (TAI_NS) -- must fall inside
// eopc04_08.62-now's covered date range, since this test builds the model with the real
// Fk5BodyFixedRotation (never IdentityRotation); see that file's own doc comment.
const T_TAI_NS: i64 = 1_788_307_237_000_000_000;
const MAX_DEGREE: usize = 8;
const MAX_ORDER: usize = 8;

/// This repository's own question-194 house style (`crates/av-lockstep/src/docker.rs`'s
/// `announce_gate_skip`/`write_real_stderr`, grepped for per this task's own brief): a gated
/// test that finds its gate absent prints a real, uncaptured `"SKIPPED "`-prefixed line rather
/// than silently returning, and the caller asserts that the line was actually produced. Written
/// locally rather than depending on `av-lockstep` (a new cross-crate dependency this task does
/// not need for one skip line) -- `av-orbital`'s own `Cargo.toml` gains no dependency from this
/// file (see this task's own report for the `cargo deny` implication of that).
fn announce_gate_skip(test_name: &str, reason: &str) -> String {
    let line = format!("SKIPPED {test_name}: {reason}\n");
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.flush();
    line
}

/// `Some(reason)` if this test cannot run (the platform has no measured [`NATIVE_STATIC_LIBS`],
/// or the system `cc` is not on `PATH`); `None` if it can.
fn gate() -> Option<String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Some("no native-static-libs list has been measured for this platform (see NATIVE_STATIC_LIBS's own doc comment); run `cargo rustc -p av-orbital --lib --no-default-features -- --print=native-static-libs` on this platform and add one".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        match Command::new("cc").arg("--version").output() {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(format!("`cc --version` exited with status {} -- no working system C compiler", output.status)),
            Err(e) => Some(format!("the system `cc` compiler was not found on PATH ({e}) -- this test compiles a committed .c file with it")),
        }
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn gmat_root() -> PathBuf {
    av_orbital::cof::locate_gmat_root().expect("GMAT_ROOT set (this task's own environment rule)")
}

fn run_cc(args: &[&str], step: &str) {
    let output = Command::new("cc").args(args).output().unwrap_or_else(|e| panic!("could not launch `cc` for {step}: {e}"));
    assert!(
        output.status.success(),
        "`cc` failed at {step} (args: {args:?}, status: {})\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Finds the real, already-built `libav_orbital-<hash>.a` under `<AV_ORBITAL_LIB_DIR>/deps/`.
/// **Not `-L<dir> -lav_orbital`**: measured directly (this task's own report has the listing),
/// Cargo leaves the `staticlib` artifact for a package built as a `cargo test` dependency ONLY
/// under `deps/` with its usual per-build-config hash suffix (`libav_orbital-<16 hex digits>.a`)
/// -- unlike a `[[bin]]` target's own primary output, which Cargo does uplift to an unhashed
/// name at the profile root, a lib target built merely because tests depend on it is not
/// uplifted, so `-lav_orbital` (which only ever searches for an EXACT `libav_orbital.a`/`.so`
/// name) cannot find it. The full path is passed to `cc` directly as a positional link input
/// instead, which sidesteps `-l` name resolution entirely.
///
/// **More than one match is a real, observed case, not a hypothetical.** The hash covers the
/// feature set (among other things), so a `deps/` directory that has ever seen BOTH a
/// `--no-default-features` and a default-features build of this crate genuinely contains two
/// `libav_orbital-*.a` files at once (measured directly: this task's own report names both
/// hashes from exactly that sequence). The one THIS test run just built is picked by last
/// modification time (`SystemTime`, newest wins) rather than by trying to reconstruct Cargo's
/// own fingerprint hash -- `cargo test` always rebuilds/relinks this crate's lib target
/// immediately before running this test binary (it is a compile-time dependency of it), so that
/// artifact's mtime is always the most recent among any candidates left over from an earlier,
/// differently-configured run in the same target directory. Panics if zero candidates exist at
/// all (nothing built).
fn find_staticlib(lib_dir: &Path) -> PathBuf {
    let deps_dir = lib_dir.join("deps");
    let mut matches: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&deps_dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", deps_dir.display()))
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("libav_orbital-") && name.ends_with(".a") {
                let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or_else(|e| panic!("stat'ing {}: {e}", path.display()));
                Some((modified, path))
            } else {
                None
            }
        })
        .collect();
    if matches.is_empty() {
        panic!("no libav_orbital-*.a found under {} -- expected the staticlib crate-type (this task's own Cargo.toml change) to have been built by `cargo test` before this test ran", deps_dir.display());
    }
    matches.sort_by_key(|(modified, _)| *modified);
    matches.pop().expect("checked non-empty above").1
}

/// Extra link flags needed ONLY when this crate was built with the `gmat-frames` feature (the
/// default) -- measured directly, not assumed, after this test first failed against a
/// default-features build with a page of `Undefined symbols` from `gmatffi.o` (this task's own
/// report has the exact error). The cause: Rust's own codegen-unit partitioning does not
/// isolate `src/ffi.rs`'s object code from `src/frame_gmat.rs`'s in `libav_orbital.a` (this
/// FFI module never calls into `frame_gmat` -- see `src/ffi.rs`'s own doc comment, "Why
/// `Fk5BodyFixedRotation`, never `GmatBodyFixedRotation`" -- but the linker still pulls in
/// whichever `.o` archive member defines the referenced symbols, and that member also defines
/// unrelated GMAT-calling code), so linking `libav_orbital.a` at all requires satisfying
/// `gmat-sys`'s own link requirements too, regardless of which code path this test actually
/// exercises at runtime. Mirrors `crates/gmat-sys/build.rs`'s own `cargo:rustc-link-lib`/
/// `-search` lines exactly (`GmatBase.R2026a`, `GmatUtil.R2026a`, `c++`) and `crates/av-orbital/
/// build.rs`'s own `-rpath` (the dylibs must be found at RUN time too, not just link time,
/// since `cc`, unlike `cargo`/`rustc`, has no other mechanism telling the loader where to find
/// them).
#[cfg(feature = "gmat-frames")]
fn gmat_frames_link_args(gmat_root: &Path) -> Vec<String> {
    let gmat_lib_dir = gmat_root.join("bin").join("GMAT-R2026a_Beta.app").join("Contents").join("Frameworks");
    vec![
        format!("-L{}", gmat_lib_dir.display()),
        format!("-Wl,-rpath,{}", gmat_lib_dir.display()),
        "-lGmatBase.R2026a".to_string(),
        "-lGmatUtil.R2026a".to_string(),
        "-lc++".to_string(),
    ]
}

#[cfg(not(feature = "gmat-frames"))]
fn gmat_frames_link_args(_gmat_root: &Path) -> Vec<String> {
    Vec::new()
}

/// Compiles and links `tests/c/av_orbital_ffi_test.c` against the real `libav_orbital.a`,
/// returning the path to the resulting executable. Panics (via [`run_cc`]) with `cc`'s own
/// stdout/stderr on any compile or link failure -- never a silent empty binary.
fn build_c_test_binary(tmp_dir: &Path, gmat_root: &Path) -> PathBuf {
    let c_file = manifest_dir().join("tests/c/av_orbital_ffi_test.c");
    let include_dir = manifest_dir().join("include");
    let lib_dir = PathBuf::from(env!("AV_ORBITAL_LIB_DIR")); // build.rs, "N6's second job".
    let staticlib = find_staticlib(&lib_dir);
    let object = tmp_dir.join("av_orbital_ffi_test.o");
    let exe = tmp_dir.join("av_orbital_ffi_test");

    run_cc(&["-c", "-I", include_dir.to_str().unwrap(), c_file.to_str().unwrap(), "-o", object.to_str().unwrap()], "compiling tests/c/av_orbital_ffi_test.c");

    let mut link_args: Vec<&str> = vec![object.to_str().unwrap(), staticlib.to_str().unwrap()];
    let search_path_flags: Vec<String> = NATIVE_STATIC_LIB_SEARCH_PATHS.iter().map(|p| format!("-L{p}")).collect();
    for flag in &search_path_flags {
        link_args.push(flag);
    }
    let gmat_flags = gmat_frames_link_args(gmat_root);
    for flag in &gmat_flags {
        link_args.push(flag);
    }
    link_args.extend_from_slice(NATIVE_STATIC_LIBS);
    link_args.push("-o");
    link_args.push(exe.to_str().unwrap());
    run_cc(&link_args, "linking against libav_orbital.a");

    exe
}

/// Runs the built C test binary and returns its six printed `state_dot` bit patterns, parsed
/// from hex, in `state_dot[0..6]` order. Panics with the binary's own stdout/stderr if it
/// exits nonzero or prints anything other than exactly six 16-hex-digit lines.
fn run_c_test_binary(exe: &Path, gravity_file: &Path, gmat_root: &Path) -> [u64; 6] {
    let output = Command::new(exe)
        .arg(gravity_file)
        .arg(gmat_root)
        .output()
        .unwrap_or_else(|e| panic!("could not run the built C test binary {}: {e}", exe.display()));
    assert!(
        output.status.success(),
        "the C test binary exited with status {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "expected exactly six hex lines from the C test binary, got {}: {lines:?}", lines.len());
    let mut bits = [0u64; 6];
    for (i, line) in lines.iter().enumerate() {
        bits[i] = u64::from_str_radix(line.trim(), 16).unwrap_or_else(|e| panic!("line {i} ({line:?}) is not 16 hex digits: {e}"));
    }
    bits
}

/// The same derivative, computed through the ordinary Rust `av_dynamics::DynamicsModel` path --
/// identical gravity file, degree/order, central body and rotation type (`Fk5BodyFixedRotation`,
/// never the GMAT-backed one -- see this file's own module doc) as `av_orbital_model_new`'s own
/// C-side construction.
fn rust_side_derivative(gravity_file: &Path, gmat_root: &Path) -> [f64; 6] {
    let rotation = Fk5BodyFixedRotation::new(gmat_root).expect("Fk5BodyFixedRotation::new");
    let info = EarthGravityModelInfo { id: "native.orbital.c_abi_export_test".to_string(), version: env!("CARGO_PKG_VERSION").to_string(), goldens: Vec::new() };
    let model = EarthGravityModel::new(gravity_file, MAX_DEGREE, MAX_ORDER, "Earth", rotation, info).expect("EarthGravityModel::new");
    let mut state_dot = [0.0; 6];
    model.derivatives(&STATE, T_TAI_NS, &[], &mut state_dot).expect("derivatives");
    state_dot
}

#[test]
fn c_caller_obtains_the_same_derivative_bytes_as_the_rust_path() {
    if let Some(reason) = gate() {
        let line = announce_gate_skip("c_caller_obtains_the_same_derivative_bytes_as_the_rust_path", &reason);
        assert!(line.starts_with("SKIPPED "), "announce_gate_skip must return the line it printed");
        return;
    }

    let tmp_dir = std::env::temp_dir().join(format!("av_orbital_ffi_test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).expect("create scratch dir for the compiled C test binary");

    let gmat_root = gmat_root();
    let gravity_file = gmat_root.join("data/gravity/earth/JGM2.cof");

    let exe = build_c_test_binary(&tmp_dir, &gmat_root);
    let c_bits = run_c_test_binary(&exe, &gravity_file, &gmat_root);
    let rust_state_dot = rust_side_derivative(&gravity_file, &gmat_root);

    eprintln!("[ffi_c_caller] C-path bits:    {c_bits:016x?}");
    eprintln!("[ffi_c_caller] Rust-path bits: {:016x?}", rust_state_dot.map(f64::to_bits));

    for (i, (&c, &rust_value)) in c_bits.iter().zip(rust_state_dot.iter()).enumerate() {
        let rust_bits = rust_value.to_bits();
        assert_eq!(
            c, rust_bits,
            "state_dot[{i}]: C path bits {:016x} (value {}) != Rust path bits {:016x} (value {}) -- expected bit-for-bit identical, see this file's own module doc, \"Bit-identical, and why that is possible at all\"",
            c, f64::from_bits(c), rust_bits, rust_value
        );
    }

    // The C test binary already checked this against its own inputs; re-checked here so a
    // future change to either side's constants that broke this invariant would fail on BOTH
    // files independently, not just the one that happens to run first.
    assert_eq!(&rust_state_dot[0..3], &STATE[3..6], "d(pos)/dt must equal velocity exactly on the Rust side too");

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

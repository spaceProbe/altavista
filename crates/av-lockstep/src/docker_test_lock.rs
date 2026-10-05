//! A host-wide, cross-process, cross-language lock serialising every Docker-gated test on this
//! machine -- not merely within one `cargo test` binary, and not merely within one git
//! worktree. Re-exported as `av_lockstep::docker::{DockerTestLock, lock_docker_tests}` (see
//! `crate::docker`'s own `pub use` of this module) so every caller spells it through the same
//! `docker` module every other Docker-lifecycle name in this crate already lives under.
//!
//! ## The defect this fixes (`docs/open-questions.md` question 207)
//!
//! [`crate::docker::prune_stale_test_resources`] deletes EVERY Docker container and image
//! carrying `av.test` -- daemon-wide, regardless of which worktree or process created it (see
//! that function's own doc comment for the full history). The only lock guarding it used to be
//! `crates/av-lockstep/tests/docker_lifecycle.rs`'s `DOCKER_TEST_LOCK`, a **process-local**
//! `std::sync::Mutex<()>` -- invisible to any other `cargo test`/`pytest` process, in this
//! worktree or any other. Two worktrees running Docker-gated tests concurrently could (and, in
//! round 3's own gate, did) tear out each other's labelled containers mid-run: round 3 measured
//! `prune_stale_test_resources_removes_orphaned_labeled_containers_and_images` failing with
//! `dial tcp 127.0.0.1:34751: connect: connection refused` while a different track's `cargo
//! test -p av-kernel` was running concurrently against the same Docker daemon, and passing when
//! re-run alone. Prune-by-label is daemon-wide; its lock was not. This module is the fix: a
//! single lock file every Docker-gated test process on this HOST -- Rust or Python, this
//! worktree or any other -- opens and `flock`s before touching Docker.
//!
//! ## The lock path, and why it is exactly this path
//!
//! `$HOME/.altavista/locks/docker-tests.lock` -- a plain file, `flock(2)`-locked exclusively.
//!
//! - **Not `/tmp`, not `/private/var`.** This host runs Docker through Colima, whose default
//!   configuration mounts only `$HOME` into its VM (`~/.colima/default/colima.yaml`'s own
//!   comment -- also the exact reason `tests/test_edge_plugin_container.py`'s own module doc
//!   gives for why every one of its bind-mount sources lives under `$HOME`, never
//!   `tempfile`/`tmp_path`). A lock file outside `$HOME` is invisible to anything running
//!   *inside* that VM, and on macOS the OS itself periodically reclaims `/tmp` and
//!   `/private/var/tmp` -- neither property is acceptable for a lock every Docker-gated test on
//!   this host must keep agreeing on for as long as the host exists.
//! - **Daemon-wide, on purpose.** [`crate::docker::prune_stale_test_resources`] is itself
//!   daemon-wide (it matches every container/image carrying `av.test`, full stop, independent of
//!   which worktree/run created it) -- a lock scoped any narrower (per worktree, per crate, per
//!   test binary -- the pre-fix `DOCKER_TEST_LOCK` was scoped to one test *binary*) would leave
//!   exactly the gap question 207 found: two different scopes racing the one daemon-wide
//!   resource they both mutate. The lock's scope must match the blast radius of what it guards.
//! - **Both languages must agree on this exact path, by construction, not by import.** The Rust
//!   side is this module (`lock_file_path` below). The Python side is
//!   `altavista/docker_test_lock.py`'s `lock_path()` -- deliberately not a shared constant (the
//!   two toolchains do not share a build), so this is a documented invariant instead: **if you
//!   change the path in one, change it in the other, in the same commit.** Both are proved to
//!   agree by a real cross-process, cross-language test
//!   (`flock_lock_is_visible_across_processes_and_languages` below): a Rust process holds this
//!   module's own lock, a `python3` child tries `fcntl.flock` on the identical path and is
//!   observed to fail, then to succeed once the Rust side drops it.
//!
//! ## Why `flock`, not a lock-file-with-a-PID scheme, and why this needed a new dependency
//!
//! `libc = "0.2"` is a new `[workspace.dependencies]` entry (see the root `Cargo.toml`'s own
//! comment on that line for the license/provenance case). It exists because
//! [`std::sync::Mutex`]/any other pure-Rust `Drop`-based guard, and a hand-rolled "write my own
//! PID into a file, another process checks whether that PID is still alive" scheme, both share
//! the same fatal gap: **neither survives `SIGKILL`.** A `Drop` impl never runs when a process
//! is killed (this exact gap is *already* documented, for the container-cleanup case, in
//! `crate::docker`'s own module doc comment on `prune_stale_test_resources` -- "an interrupted
//! test leaves a `Drop` guard un-run"), and a PID file left behind by a killed process still
//! names a PID that looks "held" to a checker until something notices the process is gone and
//! cleans the file up (a second, independent failure mode: PID reuse by an unrelated process
//! makes a stale PID file look falsely held forever). `flock(2)` has neither problem: a lock is
//! attached to the *open file description*, which the kernel itself tears down -- and therefore
//! releases the lock -- the instant a killed process's file descriptors are closed, with no
//! userspace code involved at all. That is the one property this lock actually needs (a
//! docker-gated test that gets killed mid-run must not leave the NEXT run's lock acquisition
//! hanging forever), and it is exactly what `flock` gives for free.
//!
//! ## A blocked wait is announced, never silent
//!
//! [`lock_docker_tests`] tries the lock non-blockingly first. When another process on this host
//! genuinely holds it, blocking on `flock(LOCK_EX)` with no output would be indistinguishable,
//! to a human watching a plain `cargo test` run, from a hang -- the same visibility concern
//! questions 194/148 already exist for elsewhere in this crate. So instead: one line announcing
//! the wait (naming the lock path, citing question 207) is printed via
//! [`crate::docker::write_real_stderr`] before blocking, and one more reporting the actual
//! elapsed wait (measured with `Instant`, never assumed) after acquiring. Proved end to end,
//! against a genuinely separate process, by `lock_docker_tests_announces_a_blocked_wait_never_silently`
//! below (a nested `cargo test` subprocess targeting the `probe_process_that_blocks_acquiring_the_docker_test_lock`
//! test -- the same "spawn a real subprocess and inspect its own captured output" idiom
//! `crates/av-lockstep/tests/docker_lifecycle.rs::announce_gate_skip_is_actually_visible_in_a_real_cargo_test_subprocess_without_nocapture`
//! already establishes). The Python side's identical announcement is proved the same way,
//! between two Python processes, by
//! `tests/test_docker_test_lock_cross_process.py::test_the_waiting_process_announces_its_own_wait`.
//!
//! **Round 3 (question 217(f)) revised WHICH lock this proof runs against.** Round 2 held the
//! REAL, host-wide lock for this test deliberately, reasoning that question 212(b) forbids a
//! test from ASSERTING the shared lock is free, not from WAITING on it -- so blocking on the
//! real lock looked safe. What round 2 did not measure is the COST of that choice: the parent
//! test held the real lock from before it even spawned the nested `cargo test` child until that
//! child's own WAITING line appeared, and that window includes the CHILD'S OWN COMPILATION, not
//! merely its run. Measured directly (round 3, `cargo clean -p av-lockstep` then re-running this
//! same proof against a pre-built parent binary, forcing the nested child to compile from
//! scratch inside the window the real lock was held): **~8.0s** of real, host-wide lock hold
//! time for one nested crate's worth of compilation alone -- every other track's Docker-gated
//! test on this host blocked for that whole window, for a reason (this proof's own child
//! compiling) that has nothing to do with what any of them were doing. Round 3 moves this proof
//! onto a PRIVATE lock path instead -- [`lock_docker_tests_at`], this module's own established
//! pattern (`flock_lock_is_visible_across_processes_and_languages` below already uses it, for
//! the identical question-212(b) reason). The announcement code under test
//! (`lock_docker_tests_at`'s own WAITING/ACQUIRED lines) is exactly what `lock_docker_tests`
//! calls in production -- pointing it at a private path changes nothing about what is proved,
//! only removes the real lock from the picture entirely: measured after the fix, this proof
//! holds the real host-wide lock for **0s** -- it is never acquired at all. See
//! `lock_docker_tests_announces_a_blocked_wait_never_silently`'s own doc comment below for the
//! mechanism (`AV_LOCKSTEP_PROBE_LOCK_PATH`, passed to the child's environment, never by
//! mutating this process's own).
//!
//! ## The holder sidecar (question 234, native round 5)
//!
//! Round 4: an orphaned `python3 -c` probe, spawned detached from a DIFFERENT worktree
//! (`AltaVista-aiplane`), sat in `sys.stdin.readline()` on a pipe with no writer, PPID 1,
//! holding the real lock for over two hours -- blocking this track's kernel gate. Nothing about
//! the lock itself said whose it was; the manager had to reconstruct the answer with `lsof`.
//! Question 234's ruling: the taker writes pid/tree/command/time into a sidecar, so the next
//! waiter reads it instead.
//!
//! - **Path**: [`holder_sidecar_path`] below -- [`lock_file_path`]'s own path with
//!   [`HOLDER_SIDECAR_SUFFIX`] appended by plain string concatenation (never
//!   [`PathBuf::with_extension`], which would replace an existing extension rather than append
//!   after it, and never [`PathBuf::join`], which would insert a path separator). **Not the lock
//!   file itself**: the lock file's only job is to be `flock`ed; its content is irrelevant to
//!   the kernel, and truncating/rewriting a file another process holds an `flock` on would be
//!   exactly the kind of subtlety that surprises the next reader for no gain. Like
//!   [`LOCK_RELATIVE_PATH`] above, the two languages must name the IDENTICAL sidecar path by
//!   construction, not by import: the Python side is `altavista/docker_test_lock.py`'s
//!   `holder_sidecar_path`/`_HOLDER_SIDECAR_SUFFIX`. **If you change [`HOLDER_SIDECAR_SUFFIX`]
//!   here, change the Python side's `_HOLDER_SIDECAR_SUFFIX` in the same commit.**
//! - **Written by the taker, in the outermost acquisition only** ([`write_holder_record`],
//!   called once [`lock_docker_tests_at`]'s own `flock` succeeds): `pid` (this process's own,
//!   [`std::process::id`]), `tree` (the worktree it runs in -- [`current_tree`]'s own doc
//!   comment says which of the task brief's two allowed choices this picks, and why), `command`
//!   (this process's own command line, truncated -- [`current_command_line`]), `time`
//!   (human-readable local time with a UTC offset, never a bare epoch --
//!   [`format_current_local_time`]). One `key=value` line per field, in that order; pinned
//!   exactly by `tests::holder_sidecar_record_format_is_pinned` below. Every acquisition through
//!   [`lock_docker_tests_at`] is a genuinely fresh `flock` (unlike the Python side, this type has
//!   no re-entrancy counter to skip: [`crate::docker::prune_stale_test_resources`]'s own
//!   `&DockerTestLock` parameter is what prevents a caller from nesting acquisitions in the first
//!   place -- see this module's own doc section on that), so there is no "already held, skip the
//!   write" branch to reason about here.
//! - **Read by the waiter, printed in the WAITING line** ([`describe_holder`]) -- but only after
//!   a liveness check ([`pid_is_alive`], `libc::kill(pid, 0)`, `EPERM` counted as alive): the
//!   sidecar is **advisory, not authoritative**, and this is the one place a careless
//!   implementation would be worse than `lsof`. A `SIGKILL`ed holder never runs its own cleanup
//!   (the identical gap this module's own "Why `flock`, not a lock-file-with-a-PID scheme"
//!   section describes for the lock itself), so a sidecar can genuinely name a pid that is long
//!   gone -- the waiter must never report a dead pid as though it still held the lock. A dead
//!   recorded pid is reported as a **stale** record, in those words, naming the pid; a live one
//!   is reported as the holder. An absent, empty, truncated, or unparsable sidecar
//!   ([`read_holder_record`] returning [`None`]) is not an error -- it is exactly today's status
//!   quo (a lock whose holder is unknown), and the waiter blocks precisely as it always has.
//! - **Removed by the holder** ([`remove_holder_record`], best-effort) when its guard drops --
//!   [`DockerTestLock`]'s own `Drop` impl covers the ordinary return path and an unwinding panic
//!   alike (Rust runs `Drop` impls during unwind unless the crate is built `panic = "abort"`,
//!   which this workspace is not). This is a courtesy for the ORDINARY case; it changes nothing
//!   about the advisory-only guarantee above, which exists precisely because this cleanup does
//!   NOT run on `SIGKILL`.
//!
//! The lock itself is still the `flock`; the sidecar is still only a note beside it.
//!
//! ## Why [`crate::docker::prune_stale_test_resources`] takes `&DockerTestLock` as a parameter
//!
//! See that function's own doc comment for the full "acquire-inside-the-function would
//! deadlock" argument; in short: `flock` on a second, independently-opened file descriptor in
//! the SAME process blocks against the first (measured directly by
//! `flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions` below),
//! so a `prune_stale_test_resources` that opened and locked the file itself would deadlock any
//! caller that (correctly) already holds the lock across its own test body when it reaches the
//! prune call. Requiring a `&DockerTestLock` argument makes "the caller already holds the lock"
//! a compile-time fact instead of a convention a future caller could forget.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Relative to `$HOME` -- see this module's own doc comment for why under `$HOME` specifically,
/// and why the Python implementation (`altavista/docker_test_lock.py`) must name the identical
/// path.
const LOCK_RELATIVE_PATH: &str = ".altavista/locks/docker-tests.lock";

/// `$HOME/.altavista/locks/docker-tests.lock`. Reads the environment (fine, question 199 -- only
/// *mutating* the process environment is forbidden); never falls back to any other directory if
/// `$HOME` is unset, because a lock a Docker-gated test cannot actually reach is worse than a
/// hard failure at the call site (this crate's "refuse, typed, never silent" convention, applied
/// here as "refuse loudly" since [`lock_docker_tests`]'s own signature has no `Result` to return
/// a typed refusal through -- see that function's own doc comment for why the signature is
/// infallible-looking on purpose).
fn lock_file_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| {
        panic!(
            "$HOME is not set -- cannot locate the docker-test lock file ({LOCK_RELATIVE_PATH}); \
             this is a hard error, not a silent no-lock fallback (docs/open-questions.md question 207)"
        )
    });
    PathBuf::from(home).join(LOCK_RELATIVE_PATH)
}

/// Appended to [`lock_file_path`]'s own path by STRING concatenation -- see this module's own
/// "The holder sidecar" doc section for why concatenation specifically (never
/// [`PathBuf::with_extension`], never [`PathBuf::join`]) and why the Python side's
/// `_HOLDER_SIDECAR_SUFFIX` (`altavista/docker_test_lock.py`) must name the identical string:
/// **if you change this, change that, in the same commit.**
const HOLDER_SIDECAR_SUFFIX: &str = ".holder";

/// An unbounded command line in a lock-contention diagnostic is its own footgun -- truncate
/// rather than write it whole. Same value as the Python side's `_COMMAND_MAX_LEN`, though
/// nothing requires the two to match exactly; both just need to be "a stated length".
const COMMAND_MAX_LEN: usize = 200;

/// The advisory holder-record sidecar for `lock_path` -- see this module's own "The holder
/// sidecar" doc section for the full shape and the advisory-only guarantee this function exists
/// to compute the path for.
fn holder_sidecar_path(lock_path: &Path) -> PathBuf {
    let mut sidecar = lock_path.as_os_str().to_owned();
    sidecar.push(HOLDER_SIDECAR_SUFFIX);
    PathBuf::from(sidecar)
}

/// The worktree this process is running in, answered as the current working directory -- the
/// cheaper of the two honest options this task's own brief allows (the other being
/// `git rev-parse --show-toplevel`). Chosen deliberately: the field exists to tell worktrees
/// apart (round 4's own incident: `AltaVista-edge` against `AltaVista-aiplane` against
/// `AltaVista`), and every cwd a docker-gated test runs with already names its worktree as a
/// path prefix. `pytest` runs at the repository root; `cargo test` runs each test binary with
/// its cwd at the PACKAGE directory (e.g. `<worktree>/crates/av-lockstep`), not the repository
/// root -- a correction made in review to this comment's first draft, which claimed the root in
/// both cases. `git rev-parse --show-toplevel` would normalise that to the root at the cost of a
/// subprocess spawn on every acquisition, including the uncontended silent path, for no gain in
/// telling worktrees apart.
fn current_tree() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown: current_dir() failed>".to_string())
}

/// This process's own command line: every argument [`std::env::args`] received (argv\[0\]
/// included), space-joined, truncated to [`COMMAND_MAX_LEN`] *characters* (not bytes --
/// `.chars().take(..)` so this can never panic by slicing a multi-byte UTF-8 boundary), with any
/// embedded newlines flattened to spaces (the sidecar's own format is one field per line; a
/// command line containing one must not be allowed to forge extra fields).
fn current_command_line() -> String {
    let joined = std::env::args().collect::<Vec<_>>().join(" ").replace(['\n', '\r'], " ");
    if joined.chars().count() > COMMAND_MAX_LEN {
        let mut truncated: String = joined.chars().take(COMMAND_MAX_LEN).collect();
        truncated.push_str("...(truncated)");
        truncated
    } else {
        joined
    }
}

/// The current local time, human-readable without conversion -- includes the UTC offset, never
/// a bare epoch. Formatted via a raw `libc::strftime` call (`"%Y-%m-%d %H:%M:%S %z"`) rather
/// than adding a `chrono`/`time` dependency purely for this: the crate already depends on
/// `libc` for [`libc::flock`] itself (see this crate's own `Cargo.toml` comment on that
/// dependency), and `strftime(3)` is the same C library function CPython's own `time.strftime`
/// wraps on POSIX -- the Python side's `_current_local_time_string`
/// (`altavista/docker_test_lock.py`) passes the IDENTICAL format string, so the two languages
/// produce the same shape by construction, both ultimately going through the same platform C
/// library, not merely by choosing to write similar-looking code.
fn format_current_local_time() -> String {
    // SAFETY: `t` and `tm` are stack-local and fully initialised by `libc::time`/
    // `libc::localtime_r` before any read; `buf` is a fixed-size, zero-initialised stack buffer
    // whose length is passed to `strftime` as its own bound, so it can never write past the end
    // of `buf`. `fmt` is a valid, NUL-terminated C string for the duration of the call (it
    // outlives the `strftime` call itself, never freed early).
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let fmt = CString::new("%Y-%m-%d %H:%M:%S %z").expect("format string has no interior NUL");
        let mut buf = [0u8; 64];
        let len = libc::strftime(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len(), fmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..len]).into_owned()
    }
}

/// True iff `pid` names a process that still exists on this host, checked the same way the
/// Python side does (`os.kill(pid, 0)`): `libc::kill(pid, 0)` sends no signal, only asks. `0`
/// means it exists. `ESRCH` means the process is genuinely gone. `EPERM` (e.g. a pid now owned
/// by a different user) means it still exists -- the kernel checks existence before permission,
/// so `EPERM` could only be raised for a live pid. Biased toward "alive" for anything else this
/// syscall is not documented to return here: never report a live holder as stale.
fn pid_is_alive(pid: libc::pid_t) -> bool {
    // SAFETY: `kill` with signal `0` sends no signal at all; it is always safe to call with any
    // pid value, including ones that do not exist.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Writes this process's own holder record: pid, tree, command, time, one `key=value` line per
/// field, in that order (see this module's own "The holder sidecar" doc section, and
/// `tests::holder_sidecar_record_format_is_pinned` below, which pins this exact shape against a
/// real file this process itself wrote). Written to a temp file in the SAME directory, then
/// [`fs::rename`]d into place -- atomic on POSIX, so a concurrent reader
/// ([`read_holder_record`], e.g. a waiter's own [`describe_holder`]) never observes a
/// half-written record. That reader already tolerates a missing/corrupt sidecar regardless (the
/// whole point of this module's own "the sidecar is advisory" doc section); this is cheap extra
/// care, not a correctness requirement either way.
fn write_holder_record(sidecar_path: &Path) {
    let pid = std::process::id();
    let record = format!(
        "pid={pid}\ntree={}\ncommand={}\ntime={}\n",
        current_tree(),
        current_command_line(),
        format_current_local_time(),
    );
    let mut tmp_os = sidecar_path.as_os_str().to_owned();
    tmp_os.push(format!(".tmp.{pid}"));
    let tmp_path = PathBuf::from(tmp_os);
    fs::write(&tmp_path, record.as_bytes()).unwrap_or_else(|e| panic!("could not write the holder-record temp file {tmp_path:?}: {e}"));
    fs::rename(&tmp_path, sidecar_path).unwrap_or_else(|e| panic!("could not install the holder record at {sidecar_path:?}: {e}"));
}

/// Best-effort only -- the sidecar is advisory (this module's own doc section on that), so a
/// failure to remove it must never be treated as a failure to release the LOCK itself, which is
/// the real `flock` release [`DockerTestLock`]'s own `Drop` impl performs unconditionally right
/// after calling this.
fn remove_holder_record(sidecar_path: &Path) {
    let _ = fs::remove_file(sidecar_path);
}

/// One holder record, as read back from a sidecar file.
struct HolderRecord {
    pid: libc::pid_t,
    tree: String,
    command: String,
    time: String,
}

/// The current holder record, or [`None`] if the sidecar is absent, empty, truncated, or missing
/// any of the four expected fields, or if its `pid` field does not parse as an integer -- every
/// one of those means "no information available", never an error (this module's own "the
/// sidecar is advisory" doc section: an absent/corrupt sidecar changes nothing about how a
/// waiter behaves).
fn read_holder_record(sidecar_path: &Path) -> Option<HolderRecord> {
    let text = fs::read_to_string(sidecar_path).ok()?;
    let mut pid: Option<libc::pid_t> = None;
    let mut tree: Option<String> = None;
    let mut command: Option<String> = None;
    let mut time: Option<String> = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "pid" => pid = value.parse::<libc::pid_t>().ok(),
                "tree" => tree = Some(value.to_string()),
                "command" => command = Some(value.to_string()),
                "time" => time = Some(value.to_string()),
                _ => {}
            }
        }
    }
    Some(HolderRecord { pid: pid?, tree: tree?, command: command?, time: time? })
}

fn format_holder_fields(record: &HolderRecord) -> String {
    format!("pid={} tree={} command={:?} time={}", record.pid, record.tree, record.command, record.time)
}

/// A human-readable clause describing whoever the sidecar says currently holds the lock, for the
/// WAITING line -- see this module's own "the sidecar is advisory" doc section for the liveness
/// check performed before anyone is called the holder: a dead recorded pid is reported as a
/// STALE record, in those words, naming the pid, never silently as though it were still holding
/// the lock.
fn describe_holder(sidecar_path: &Path) -> String {
    let record = match read_holder_record(sidecar_path) {
        Some(r) => r,
        None => return "holder: unknown (no readable holder record)".to_string(),
    };
    let fields = format_holder_fields(&record);
    if pid_is_alive(record.pid) {
        format!("holder: {fields}")
    } else {
        format!("stale holder record ({fields}) -- pid {} is no longer running", record.pid)
    }
}

/// An RAII guard over the host-wide `flock(LOCK_EX)` on [`lock_file_path`]. Holding one is proof
/// (enforced at compile time via [`crate::docker::prune_stale_test_resources`]'s own `&
/// DockerTestLock` parameter) that this process currently has exclusive claim to every
/// Docker-gated test on this host -- see this module's own doc comment for the full account of
/// why the scope is daemon-wide and cross-language.
///
/// Holds the [`File`] (the open file description the lock is attached to) -- `Drop` releases the
/// lock by closing that file descriptor (see [`Drop`]'s own doc comment on this type for why
/// that is also what makes this survive a `SIGKILL`, unlike a `Drop`-based guard with no
/// kernel-owned resource behind it). Also holds `sidecar_path`, this guard's own holder-record
/// sidecar (question 234 -- see this module's own "The holder sidecar" doc section): unlike the
/// lock itself, removing this on drop is a courtesy for the ordinary case, not a correctness
/// requirement -- see that doc section for why a `SIGKILL` leaving it behind is expected and
/// handled by the READER, not avoided by the holder.
#[derive(Debug)]
pub struct DockerTestLock {
    file: File,
    sidecar_path: PathBuf,
}

/// Acquires the host-wide Docker-test lock, blocking until it is free. Every Docker-gated test
/// (Rust or Python, in this worktree or any other on this host) must hold the guard this returns
/// for its ENTIRE body, not just around a `prune_stale_test_resources` call -- see this module's
/// own doc comment, and `crate::docker::prune_stale_test_resources`'s own doc comment, for why a
/// narrower scope already reproduced question 207's own failure once.
///
/// **A blocked wait is announced, never silent.** A follow-up to question 207: blocking on
/// `flock(LOCK_EX)` with no output is, to a human watching a plain `cargo test` run, genuinely
/// indistinguishable from a hang -- the same visibility concern questions 194/148 already exist
/// for elsewhere in this module's own crate. This function therefore tries the lock
/// non-blockingly first; only if that would actually block does it print one line (via
/// [`crate::docker::write_real_stderr`] -- a raw stderr write, not `eprintln!`, so it survives
/// libtest's output capture on an eventually-passing test) naming the lock path and citing
/// question 207, then blocks for real, then prints a second line reporting how long the wait
/// actually took (measured with [`std::time::Instant`] around the blocking call itself -- a real
/// elapsed-time measurement of an OS event, never a sleep).
///
/// Panics (rather than returning a `Result`) if `$HOME` is unset, the lock directory cannot be
/// created, the lock file cannot be opened, or the underlying `flock(2)` call itself fails for a
/// reason other than blocking (e.g. `ENOLCK`) -- a hard error at the acquire site, never a
/// silent no-lock fallback (this task's own binding rule, restated from
/// `docs/open-questions.md` question 207's ruling).
pub fn lock_docker_tests() -> DockerTestLock {
    lock_docker_tests_at(lock_file_path())
}

/// The actual implementation behind [`lock_docker_tests`], taking the lock path as a parameter
/// rather than always computing it from `$HOME` via [`lock_file_path`]. [`lock_docker_tests`]
/// itself is `lock_docker_tests_at(lock_file_path())` and nothing else -- every production
/// caller keeps calling `lock_docker_tests()` with no signature change at all. This split
/// exists solely so this module's own tests can prove real mutual exclusion (blocked while
/// held, acquired after release, the WAITING/ACQUIRED announcement) against a PRIVATE lock file
/// under a test's own scratch directory, instead of the real host-wide path -- see
/// `tests::flock_lock_is_visible_across_processes_and_languages`'s own doc comment
/// (`docs/open-questions.md` question 212(b): a test must never depend on the real, shared lock
/// being free).
pub(crate) fn lock_docker_tests_at(path: PathBuf) -> DockerTestLock {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("could not create {parent:?} for the docker-test lock: {e}"));
    }
    let sidecar_path = holder_sidecar_path(&path);
    // `truncate(false)` explicitly: this is a lock file, never a payload -- its (empty)
    // contents are irrelevant, and truncating it on every acquire would be needless I/O with no
    // benefit, not a correctness requirement either way.
    let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).unwrap_or_else(|e| panic!("could not open/create the docker-test lock file {path:?}: {e}"));
    let fd = file.as_raw_fd();

    // Try non-blocking first. If nothing else holds the lock, this is the entire acquisition --
    // silent, exactly as before.
    // SAFETY: `fd` is a valid, open file descriptor owned by `file` for the duration of this
    // call (not closed until `file` is dropped, no earlier than this function's return).
    let rc_nb = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if rc_nb != 0 {
        let nb_err = io::Error::last_os_error();
        if nb_err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            // Some failure OTHER than "would block" (e.g. ENOLCK) -- a hard error at the
            // acquire site, same as the blocking call's own failure path below.
            panic!("flock(LOCK_EX|LOCK_NB) on {path:?} failed for a reason other than blocking: {nb_err}");
        }
        let holder_desc = describe_holder(&sidecar_path);
        crate::docker::write_real_stderr(&format!(
            "WAITING for the docker-test lock ({}): another process on this host currently holds it ({holder_desc}) -- \
             blocking until it releases (docs/open-questions.md question 207: this lock is host-wide, \
             not per-worktree, so waiting here under real contention is expected, not a hang)\n",
            path.display()
        ));
        let waited_since = std::time::Instant::now();
        // SAFETY: same `fd`, still open, no other code in this function touches it concurrently.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            panic!("flock(LOCK_EX) on {path:?} failed: {err}");
        }
        let waited = waited_since.elapsed();
        crate::docker::write_real_stderr(&format!("ACQUIRED the docker-test lock ({}) after waiting {waited:?}\n", path.display()));
    }
    // Question 234: the taker writes its own holder record exactly once per real `flock` --
    // every call to `lock_docker_tests_at` acquires a genuinely fresh lock (see this module's
    // own "The holder sidecar" doc section for why Rust has no re-entrant "already held, skip
    // the write" branch to reason about here, unlike the Python side).
    write_holder_record(&sidecar_path);
    DockerTestLock { file, sidecar_path }
}

impl Drop for DockerTestLock {
    /// Releases the lock. Three things happen here, deliberately, in this order:
    ///
    /// 1. Best-effort removal of this guard's own holder-record sidecar
    ///    ([`remove_holder_record`], question 234) -- done first so a waiter that unblocks
    ///    immediately after this call never reads a stale record belonging to THIS process.
    ///    Ordering does not matter for CORRECTNESS (the sidecar is advisory -- see this module's
    ///    own "The holder sidecar" doc section), only for tidiness.
    /// 2. An explicit `flock(LOCK_UN)` -- makes the release visible at exactly this point in the
    ///    code, rather than relying entirely on the implicit close below.
    /// 3. `self.file`'s own `Drop` (run automatically right after this method returns) closes
    ///    the file descriptor, which *also* releases the lock -- this is not belt-and-suspenders
    ///    against a bug, it is the actual mechanism that makes this lock survive `SIGKILL`: a
    ///    killed process never runs this `Drop` impl at all (which means step 1 never runs
    ///    either -- exactly why the sidecar is advisory only, and why a waiter must verify
    ///    liveness before trusting it), but the KERNEL still closes every file descriptor a
    ///    killed process held, which releases every `flock` that process held -- with zero
    ///    userspace code (this method included) ever running. The explicit `LOCK_UN` call exists
    ///    only for the ordinary (non-killed) case's own clarity; the guarantee this whole module
    ///    exists for comes from the third, kernel-driven release.
    fn drop(&mut self) {
        remove_holder_record(&self.sidecar_path);
        let fd = self.file.as_raw_fd();
        // SAFETY: `fd` is still open (this is the only place that closes it, via `self.file`'s
        // own Drop immediately after this method returns). Return value ignored: a `Drop` impl
        // cannot propagate a `Result`, and the file's own descriptor is about to be closed
        // regardless, which releases the lock even if this explicit unlock somehow failed.
        let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A fresh, `$HOME`-relative (never `/tmp`/`/private/var` -- this task's own rule that no
    /// new path outside `$HOME` appears anywhere in this change) scratch directory under this
    /// repository's own already-`.gitignore`d `.av-test-tmp/` convention
    /// (`tests/test_edge_plugin_container.py`'s own module doc names the same convention and the
    /// same reason: this whole repository checkout already lives under `$HOME`, so anything
    /// placed inside it is too, without needing its own `$HOME`-reading logic).
    fn repo_scratch_dir(label: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.av-test-tmp").join(format!("av-lockstep-{label}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("could not create scratch dir {dir:?}: {e}"));
        dir
    }

    // -------------------------------------------------------------------------------------
    // "Measure, do not assume" (question 207's own standard): does flock(2) on two SEPARATE
    // open file descriptions in the SAME process actually serialise two threads of that
    // process? This is the exact question that decides whether
    // crates/av-lockstep/tests/docker_lifecycle.rs's pre-existing process-local
    // `DOCKER_TEST_LOCK: Mutex<()>` can be removed in favour of this module's lock alone, or
    // must be kept alongside it. No Docker, no python3 -- always runs.
    // -------------------------------------------------------------------------------------

    #[test]
    fn flock_serializes_two_threads_of_the_same_process_on_separate_open_file_descriptions() {
        let scratch = repo_scratch_dir("flock-thread-probe");
        let path = scratch.join("probe.lock");
        fs::write(&path, b"").unwrap_or_else(|e| panic!("could not create {path:?}: {e}"));

        // Thread A opens its OWN File (its own, independent open file description), takes the
        // exclusive lock, then blocks on a real OS event (a channel recv, never a sleep) until
        // the main thread tells it to release -- this rendezvous, not any timing guess, is what
        // guarantees thread B's own non-blocking attempt below observes the lock still held:
        // the main thread only sends B's own check happens strictly after A's `held_tx.send`
        // (which itself happens strictly after A's flock call returns) and strictly before
        // `release_tx.send` unblocks A's own `release_rx.recv`.
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let path_for_a = path.clone();
        let thread_a = std::thread::spawn(move || {
            let file_a = OpenOptions::new().read(true).write(true).open(&path_for_a).expect("thread A open");
            let fd_a = file_a.as_raw_fd();
            let rc = unsafe { libc::flock(fd_a, libc::LOCK_EX) };
            assert_eq!(rc, 0, "thread A's own blocking LOCK_EX must succeed immediately (nothing else holds it yet)");
            held_tx.send(()).expect("signal B");
            release_rx.recv().expect("wait for the main thread's release signal -- a real event, never a sleep");
            let _ = unsafe { libc::flock(fd_a, libc::LOCK_UN) };
        });

        held_rx.recv().expect("wait for A to signal it holds the lock");

        // Thread B (this thread): a SEPARATE File::open on the identical path -- a second,
        // independent open file description, the same relationship two SEPARATE PROCESSES
        // would have to each other, just without the process boundary. If flock is scoped per
        // open file description (as POSIX/BSD document, and as this module's own doc comment
        // claims), B's own non-blocking attempt must fail while A still holds its lock (proven
        // held by the channel rendezvous above, not by a timing assumption).
        let file_b = OpenOptions::new().read(true).write(true).open(&path).expect("thread B open");
        let fd_b = file_b.as_raw_fd();
        let rc_while_a_holds = unsafe { libc::flock(fd_b, libc::LOCK_EX | libc::LOCK_NB) };
        let errno_while_a_holds = io::Error::last_os_error();

        // Release A (unblocks its own release_rx.recv above) and wait for it to actually finish
        // unlocking and exit -- `join` blocks on that real OS/thread-completion event, never a
        // sleep.
        release_tx.send(()).expect("tell thread A to release");
        thread_a.join().expect("thread A must not panic");

        // A has now released (its own explicit LOCK_UN ran before this join could return). B's
        // fd is still open (never closed, never unlocked) -- try again.
        let rc_after_a_released = unsafe { libc::flock(fd_b, libc::LOCK_EX | libc::LOCK_NB) };
        let _ = unsafe { libc::flock(fd_b, libc::LOCK_UN) };

        println!(
            "flock same-process/two-fd measurement on this host: while thread A held the lock, thread B's own LOCK_EX|LOCK_NB on a separate fd returned {rc_while_a_holds} (errno: {errno_while_a_holds}); \
             after thread A released, the identical call on the SAME (still-open) fd returned {rc_after_a_released}"
        );

        fs::remove_dir_all(&scratch).ok();

        assert_ne!(
            rc_while_a_holds, 0,
            "flock did NOT serialise two threads of one process on this host (a second open file description could LOCK_EX|LOCK_NB while another thread's separate fd already held the exclusive lock) -- \
             if this ever fails on some future host, crates/av-lockstep/tests/docker_lifecycle.rs's own process-local DOCKER_TEST_LOCK mutex must be restored alongside this module's lock, not removed"
        );
        assert_eq!(rc_after_a_released, 0, "once thread A released, thread B's own already-open fd must be able to LOCK_EX|LOCK_NB successfully");
    }

    // -------------------------------------------------------------------------------------
    // Cross-process, cross-language proof (question 207): a python3 child using stdlib
    // `fcntl.flock` on the IDENTICAL path this module computes must observe real mutual
    // exclusion against THIS process's own DockerTestLock guard. No Docker needed -- this
    // needs only python3, and skips VISIBLY (question 194's precedent) if it is absent, never
    // silently.
    // -------------------------------------------------------------------------------------

    /// Takes the lock path as its first (and only) `argv` element, rather than deriving it from
    /// `$HOME` itself -- see `flock_lock_is_visible_across_processes_and_languages`'s own doc
    /// comment for why: that test now runs against a PRIVATE lock file under its own scratch
    /// directory, never the real host-wide path, so the path has to come from the Rust side,
    /// the one place that already knows it.
    const PYTHON_FLOCK_PROBE: &str = r#"
import fcntl, os, sys
path = sys.argv[1]
os.makedirs(os.path.dirname(path), exist_ok=True)
fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    print("ACQUIRED")
    fcntl.flock(fd, fcntl.LOCK_UN)
except BlockingIOError:
    print("BLOCKED")
finally:
    os.close(fd)
"#;

    fn run_python_probe(path: &std::path::Path) -> String {
        let output = Command::new("python3")
            .args(["-c", PYTHON_FLOCK_PROBE])
            .arg(path)
            .output()
            .expect("python3 was already confirmed present by this test's own gate");
        assert!(output.status.success(), "the python3 probe itself must not error: stdout={:?} stderr={:?}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// The same probe, but against a path this test OWNS (passed in `argv`) rather than the
    /// host-wide production lock. See
    /// [`flock_release_is_visible_across_processes_and_languages`] for why the release half of
    /// the proof cannot run against the production path.
    const PYTHON_FLOCK_PROBE_AT_PATH: &str = r#"
import fcntl, os, sys
path = sys.argv[1]
os.makedirs(os.path.dirname(path), exist_ok=True)
fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    print("ACQUIRED")
    fcntl.flock(fd, fcntl.LOCK_UN)
except BlockingIOError:
    print("BLOCKED")
finally:
    os.close(fd)
"#;

    fn run_python_probe_at(path: &std::path::Path) -> String {
        let output = Command::new("python3").args(["-c", PYTHON_FLOCK_PROBE_AT_PATH, &path.to_string_lossy()]).output().expect("python3 was already confirmed present by this test's own gate");
        assert!(output.status.success(), "the python3 probe itself must not error: stdout={:?} stderr={:?}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// `None` iff `python3` is on `PATH`; otherwise the visible-skip line both probe tests
    /// print instead of asserting (question 194's precedent, unchanged from the single test
    /// these two were split out of).
    fn python3_skip_line(test_name: &str) -> Option<String> {
        let present = Command::new("python3").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if present {
            return None;
        }
        let reason = crate::docker::DockerGateReason::PrerequisiteUnavailable { what: "python3".to_string(), hint: "install python3 and put it on PATH".to_string() };
        Some(crate::docker::announce_gate_skip(test_name, &reason))
    }

    /// Half one of the cross-language proof, and the only half that may name the PRODUCTION
    /// lock path: while this process holds the real [`DockerTestLock`], a python3 child that
    /// re-derives that path from `$HOME` by itself must observe the lock as taken. That is
    /// what proves the two independent implementations agree on the path *by construction*.
    ///
    /// This direction is contention-proof: another holder elsewhere on this host cannot make it
    /// pass spuriously, because [`lock_docker_tests`] blocks until this process is the holder.
    #[test]
    fn flock_lock_is_visible_across_processes_and_languages() {
        // This test needs no Docker at all (question 194's own distinction: gate on the ONE
        // real precondition this test has, never a broader "docker available" check that would
        // misreport the actual reason for a skip) -- only python3 on PATH.
        if let Some(line) = python3_skip_line("flock_lock_is_visible_across_processes_and_languages") {
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        let scratch = repo_scratch_dir("flock-cross-language-probe");
        let private_lock_path = scratch.join("docker-tests.lock");

        let guard = lock_docker_tests_at(private_lock_path.clone());
        let observed_while_held = run_python_probe(&private_lock_path);
        drop(guard);

        // Question 148: an exit code is not evidence -- print exactly what was observed
        // (visible with `--nocapture`; also asserted on directly below, not merely eyeballed).
        println!("flock cross-process/cross-language proof: while the Rust guard held the production lock, the python3 child observed {observed_while_held:?}");

        assert_eq!(
            observed_while_held, "BLOCKED",
            "a python3 child using fcntl.flock(LOCK_EX|LOCK_NB) on the IDENTICAL private path this Rust guard just locked must fail to acquire while the guard holds it -- got {observed_while_held:?}"
        );
    }

    /// Half two: **releasing** an `flock` really does let another process's `LOCK_EX|LOCK_NB`
    /// succeed -- proved on a lock file this test creates and owns, never on the production
    /// path.
    ///
    /// # Why this may not use the production path (the defect this split fixes)
    ///
    /// Until 2026-09-15 this assertion lived in
    /// [`flock_lock_is_visible_across_processes_and_languages`] above, as "drop the guard, then
    /// the same probe must print ACQUIRED" against `$HOME/.altavista/locks/docker-tests.lock`.
    /// That asserts **the host-wide lock is free at a particular instant** -- which is exactly
    /// what question 212(b) already ruled a defect for the Python counterpart
    /// (`tests/test_docker_test_lock_cross_process.py` "must not assert that a host-wide lock
    /// is free, only that its own two processes serialise"). The Rust side was missed then, and
    /// it is not a theoretical gap: two sibling tests in THIS very test binary legitimately
    /// hold the same lock concurrently under libtest's default thread pool --
    /// [`probe_process_that_blocks_acquiring_the_docker_test_lock`] takes it directly, and
    /// [`lock_docker_tests_announces_a_blocked_wait_never_silently`] spawns a nested `cargo
    /// test` child that takes it and holds it for however long that child's own build takes.
    /// Measured on this host: the old assertion FAILED (`got "BLOCKED"`) on a cold target
    /// directory, where the nested child's build kept the lock for ~131 s, and PASSED on a warm
    /// one, where the same test binary finished in 0.15 s. A second track's worktree running
    /// its own Docker-gated tests widens the same window further (question 207's contention
    /// rule). Release semantics are a property of `flock` itself, so they are provable on any
    /// file -- no reason to prove them on the one file the whole host contends for.
    #[test]
    fn flock_release_is_visible_across_processes_and_languages() {
        if let Some(line) = python3_skip_line("flock_release_is_visible_across_processes_and_languages") {
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        // A lock file this test alone uses: same directory as the production lock (so it lives
        // under `$HOME`, the one directory Colima mounts and macOS does not reclaim -- see this
        // module's own doc), a name no other code in this workspace ever opens, and this
        // process's own pid so two concurrent `cargo test` runs cannot collide on it either.
        let own_path = lock_file_path().with_file_name(format!("docker-tests.selftest.{}.lock", std::process::id()));
        if let Some(parent) = own_path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|e| panic!("could not create {parent:?} for this test's own lock file: {e}"));
        }
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&own_path).unwrap_or_else(|e| panic!("could not open {own_path:?}: {e}"));
        // SAFETY: `fd` is valid and owned by `file` for the whole of this call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "this test's own, uncontended lock file must be acquirable without blocking: {}", io::Error::last_os_error());

        let observed_while_held = run_python_probe_at(&own_path);
        drop(file); // closing the fd releases the flock -- the mechanism under test
        let observed_after_drop = run_python_probe_at(&own_path);

        println!("flock release proof on this test's own lock file {}: while held, the python3 child observed {observed_while_held:?}; after release, {observed_after_drop:?}", own_path.display());

        fs::remove_file(&own_path).ok();

        assert_eq!(observed_while_held, "BLOCKED", "while this test's own fd held the exclusive lock, a python3 child must not be able to take it -- got {observed_while_held:?}");
        assert_eq!(
            observed_after_drop, "ACQUIRED",
            "after the fd was closed (which releases the flock), the SAME python3 child command on the SAME path must succeed -- got {observed_after_drop:?}"
        );
    }

    /// The cross-language PATH-agreement claim the old version of
    /// `flock_lock_is_visible_across_processes_and_languages` used to prove as a side effect of
    /// locking the real shared path -- kept as its own assertion, but doing NO locking
    /// whatsoever, against the real `lock_file_path()` (this module's actual production path
    /// function): a bare `python3 -c` re-derives the identical path inline from `$HOME` (never
    /// importing `altavista.docker_test_lock` -- proving agreement BY CONSTRUCTION, not by one
    /// side importing the other's constant), and this test asserts the two strings are equal.
    /// Nothing here ever touches the real lock file, so this can never be affected by whether
    /// some other process on this host holds it. Skips visibly (question 194's precedent) if
    /// python3 is absent, exactly as `flock_lock_is_visible_across_processes_and_languages`
    /// does. The Python-side equivalent is
    /// `tests/test_docker_test_lock_cross_process.py::test_lock_path_matches_the_documented_home_relative_convention`.
    #[test]
    fn rust_and_python_compute_the_identical_lock_path() {
        let python3_present = Command::new("python3").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !python3_present {
            let reason = crate::docker::DockerGateReason::PrerequisiteUnavailable { what: "python3".to_string(), hint: "install python3 and put it on PATH".to_string() };
            let line = crate::docker::announce_gate_skip("rust_and_python_compute_the_identical_lock_path", &reason);
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        const PYTHON_PATH_PROBE: &str = r#"
import os
print(os.path.join(os.environ["HOME"], ".altavista", "locks", "docker-tests.lock"))
"#;
        let output = Command::new("python3").args(["-c", PYTHON_PATH_PROBE]).output().expect("python3 was already confirmed present by this test's own gate");
        assert!(output.status.success(), "the python3 path probe itself must not error: stdout={:?} stderr={:?}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let python_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let rust_path = lock_file_path();

        println!("lock path agreement proof: Rust lock_file_path() = {rust_path:?}, python3's own inline computation = {python_path:?}");

        assert_eq!(
            rust_path.to_string_lossy(),
            python_path,
            "Rust's lock_file_path() and a bare python3's own inline $HOME-relative computation must name the IDENTICAL path -- got Rust={rust_path:?}, python3={python_path:?}"
        );
    }

    // -------------------------------------------------------------------------------------
    // A blocked wait is announced, never silent (question 207's own follow-up). Proved against
    // a genuinely separate process (not just in-process reasoning about what the code SHOULD
    // print) via a nested `cargo test` subprocess -- the identical idiom
    // `crates/av-lockstep/tests/docker_lifecycle.rs::
    // announce_gate_skip_is_actually_visible_in_a_real_cargo_test_subprocess_without_nocapture`
    // already establishes for exactly this "does a raw stderr write survive libtest's capture"
    // question. `Command::output()`'s own pipes capture a child's raw fd writes regardless of
    // libtest's internal capture state, so no `--nocapture` is needed on the nested invocation.
    //
    // Round 3 (question 217(f)): this proof now runs against a PRIVATE lock path, never the
    // real host-wide one -- see `lock_docker_tests_announces_a_blocked_wait_never_silently`'s
    // own doc comment below for the measured reason.
    // -------------------------------------------------------------------------------------

    /// Not meaningful on its own -- exists ONLY as the nested-subprocess target for
    /// `lock_docker_tests_announces_a_blocked_wait_never_silently` below. Acquires the lock at
    /// `AV_LOCKSTEP_PROBE_LOCK_PATH` (read from THIS process's own inherited environment --
    /// reading is not the mutation question 199 forbids; the parent test sets it on the CHILD's
    /// environment via `Command::env`, never by mutating its own `std::env`) if that variable is
    /// set, else falls back to the REAL, production lock via `lock_docker_tests()`. The fallback
    /// matters: when run as an ordinary, standalone part of this crate's own test suite (`cargo
    /// test -p av-lockstep --lib`, no parent test involved, no env var set), this must still
    /// pass trivially and silently, the same as any other acquire-then-release of an uncontended
    /// lock -- exactly the case this crate's own FINAL CHECKS command exercises.
    ///
    /// Round 3 (question 217(f)): `lock_docker_tests_announces_a_blocked_wait_never_silently`
    /// used to point this probe at the real lock unconditionally, holding the real, host-wide
    /// lock for this whole process's compilation as a nested `cargo test` child -- see that
    /// test's own updated doc comment, and this module's own top-level doc, "A blocked wait is
    /// announced, never silent", for the measured before/after. Announces success on stdout so
    /// the parent test can tell the child's own `flock` call actually returned (as opposed to
    /// the child having crashed before reaching it).
    #[test]
    fn probe_process_that_blocks_acquiring_the_docker_test_lock() {
        let _guard = match std::env::var_os("AV_LOCKSTEP_PROBE_LOCK_PATH") {
            Some(path) => lock_docker_tests_at(PathBuf::from(path)),
            None => lock_docker_tests(),
        };
        println!("PROBE_ACQUIRED");
    }

    /// Round 3 (question 217(f)): moved off the REAL, host-wide lock onto a PRIVATE path
    /// (`lock_docker_tests_at`, this module's own established pattern -- see
    /// `flock_lock_is_visible_across_processes_and_languages`'s own doc comment for the
    /// identical question-212(b) reasoning). Round 2 had kept this one test on the real lock
    /// deliberately: question 212(b) forbids a test from ASSERTING the shared lock is free, not
    /// from WAITING on it, so blocking on the real lock looked safe. What round 2 did not
    /// measure is the COST: the real lock was held from before this test even spawned the
    /// nested `cargo test` child until that child's own WAITING line appeared -- a window that
    /// includes the child's own COMPILATION, not merely its run. Measured directly (`cargo clean
    /// -p av-lockstep`, then re-running this same proof from a pre-built parent binary so the
    /// nested child had to compile from scratch inside that window): the real, host-wide lock
    /// was held for **~8.0s** -- every other track's Docker-gated test on this host blocked for
    /// that whole window, for a reason that has nothing to do with any of them. Moving to a
    /// private path removes that cost while proving the identical thing: `lock_docker_tests_at`
    /// is the exact code `lock_docker_tests` calls in production, merely pointed at a path this
    /// test owns, so the announcement logic under test is unchanged. Measured after this change:
    /// the real lock is held for **0s** -- this test never acquires it at all.
    #[test]
    fn lock_docker_tests_announces_a_blocked_wait_never_silently() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;

        // A PRIVATE lock path under this crate's own scratch-dir convention (`repo_scratch_dir`,
        // the same one `flock_lock_is_visible_across_processes_and_languages` uses) -- never the
        // real, shared `$HOME/.altavista/locks/docker-tests.lock` (question 212(b): a test must
        // never depend on, or hold, the real lock).
        let scratch = repo_scratch_dir("blocked-wait-announce");
        let private_lock_path = scratch.join("docker-tests.lock");

        // This process acquires the (private) lock FIRST -- nothing else holds it yet, so this
        // is the silent, non-blocking path (no announcement expected here).
        let guard = lock_docker_tests_at(private_lock_path.clone());

        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        // `--nocapture` on the NESTED invocation: without it, libtest would swallow the probe
        // test's own `println!("PROBE_ACQUIRED")` on a passing run (the exact question-194
        // capture behaviour this crate already documents elsewhere) -- this test needs that
        // line visible in the child's own stdout to confirm the child genuinely reached and
        // passed through its own `flock` call, not merely that the subprocess exited 0. The
        // WAITING/ACQUIRED lines themselves need no such flag (`write_real_stderr` already
        // bypasses libtest's capture on its own), but harmless to request together.
        //
        // `AV_LOCKSTEP_PROBE_LOCK_PATH` is set on the CHILD's own environment via `Command::env`
        // -- never by mutating this process's own `std::env` (question 199) -- so
        // `probe_process_that_blocks_acquiring_the_docker_test_lock` locks the SAME private path
        // this test just locked, not the real one.
        let mut child = Command::new("cargo")
            .args(["test", "-p", "av-lockstep", "--lib", "--", "--exact", "docker_test_lock::tests::probe_process_that_blocks_acquiring_the_docker_test_lock", "--nocapture"])
            .current_dir(&repo_root)
            .env("AV_LOCKSTEP_PROBE_LOCK_PATH", &private_lock_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("could not spawn the nested cargo test subprocess: {e}"));

        // Block on a REAL OS event: the child's own stderr line, which `write_real_stderr`
        // flushes immediately after writing -- never a sleep-and-hope that the child has
        // "probably" reached its own flock call by now. `cargo test`'s own build-status text
        // ("Compiling"/"Finished"/"Running...") also lands on this same stderr pipe before the
        // test binary itself even starts, so this reads (and discards) lines until it finds the
        // one actually containing "WAITING" -- filtering on content, never on line position. If
        // the announcement regressed to nothing at all, this loop would instead block forever on
        // eof-that-never-comes until the child's own process exit closes the pipe -- which
        // happens only if the child's own `flock` call unblocks, which (with `guard` still held
        // here) it cannot, so a regression here shows up as this test hanging/timing out, not
        // silently passing.
        let mut child_stderr = BufReader::new(child.stderr.take().expect("piped stderr"));
        let waiting_line = loop {
            let mut line = String::new();
            let n = child_stderr.read_line(&mut line).expect("read a line from the child's real stderr pipe");
            assert_ne!(n, 0, "the child's stderr pipe closed before ever printing a WAITING line -- lines seen so far are lost, but this means the announcement never happened");
            if line.contains("WAITING") {
                break line.trim().to_string();
            }
        };

        // Release -- the child's own blocked flock(LOCK_EX) can now proceed.
        drop(guard);

        let acquired_line = loop {
            let mut line = String::new();
            let n = child_stderr.read_line(&mut line).expect("read a line from the child's real stderr pipe");
            assert_ne!(n, 0, "the child's stderr pipe closed before ever printing an ACQUIRED-after-waiting line");
            if line.contains("ACQUIRED the docker-test lock") {
                break line.trim().to_string();
            }
        };

        let mut remaining_stderr = String::new();
        std::io::Read::read_to_string(&mut child_stderr, &mut remaining_stderr).ok();

        let output_status = child.wait().expect("the nested cargo test subprocess must exit");
        let mut child_stdout_buf = String::new();
        if let Some(mut out) = child.stdout.take() {
            std::io::Read::read_to_string(&mut out, &mut child_stdout_buf).ok();
        }

        // The child has fully exited (and therefore closed its own fd on the private lock file)
        // by this point -- safe to remove the scratch directory now, same as
        // `flock_lock_is_visible_across_processes_and_languages`'s own cleanup below.
        fs::remove_dir_all(&scratch).ok();

        // Question 148: an exit code is not evidence -- print exactly what the child process
        // actually wrote, not a paraphrase.
        std::io::stdout()
            .write_all(format!("lock_docker_tests waiting-announcement proof (nested subprocess): WAITING line = {waiting_line:?}; ACQUIRED line = {acquired_line:?}; child stdout tail = {child_stdout_buf:?}\n").as_bytes())
            .ok();

        assert!(
            output_status.success(),
            "the nested cargo test subprocess (the actual test of lock_docker_tests()'s blocking path) must itself pass -- stderr tail: {remaining_stderr:?}, stdout: {child_stdout_buf:?}"
        );
        assert!(child_stdout_buf.contains("PROBE_ACQUIRED"), "the child must have actually acquired the lock and printed its own confirmation -- got stdout {child_stdout_buf:?}");
        assert!(
            waiting_line.contains("WAITING") && waiting_line.contains("docker-test lock") && waiting_line.contains("question 207"),
            "expected a WAITING line naming the docker-test lock and citing question 207, got {waiting_line:?}"
        );
        assert!(
            acquired_line.starts_with("ACQUIRED the docker-test lock") && acquired_line.contains("after waiting"),
            "expected an ACQUIRED-after-waiting line reporting the real elapsed wait, got {acquired_line:?}"
        );
    }

    // -------------------------------------------------------------------------------------
    // The holder sidecar (question 234, native round 5). No Docker needed for any of these --
    // `holder_sidecar_record_format_is_pinned` and `holder_sidecar_is_removed_when_the_
    // outermost_guard_drops_during_a_panic` run entirely in-process against a private lock path;
    // the two `rust_and_python_*` tests below need only `python3` on PATH, same gate as the
    // other cross-language tests above.
    // -------------------------------------------------------------------------------------

    /// Pins the exact on-disk shape of the holder sidecar record: four `key=value` lines,
    /// `pid`/`tree`/`command`/`time`, in that order, written by the taker's outermost (here,
    /// only) acquisition. Not proved by inspection -- reads the real file this very process's
    /// own [`DockerTestLock`] guard wrote, via a private lock path (question 212(b)). Also
    /// proves the sidecar is removed once the guard's outermost block exits on the ordinary
    /// (non-panicking) path -- see `holder_sidecar_is_removed_when_the_outermost_guard_drops_
    /// during_a_panic` below for the unwind path.
    #[test]
    fn holder_sidecar_record_format_is_pinned() {
        let scratch = repo_scratch_dir("sidecar-format");
        let private_lock_path = scratch.join("docker-tests.lock");
        let sidecar_path = holder_sidecar_path(&private_lock_path);

        let guard = lock_docker_tests_at(private_lock_path.clone());
        let raw = fs::read_to_string(&sidecar_path).unwrap_or_else(|e| panic!("could not read the holder sidecar {sidecar_path:?} this guard just wrote: {e}"));

        // Question 148: print what was actually observed, not a paraphrase.
        println!("holder sidecar record format proof: sidecar path = {sidecar_path:?}, contents = {raw:?}");

        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 4, "expected exactly 4 lines (pid/tree/command/time), got {lines:?}");
        let keys: Vec<&str> = lines.iter().map(|l| l.split('=').next().unwrap_or("")).collect();
        assert_eq!(keys, vec!["pid", "tree", "command", "time"], "expected pid/tree/command/time in that order, got {keys:?}");

        let pid_field = lines[0].split_once('=').expect("pid line has an '='").1;
        let pid_value: u32 = pid_field.parse().unwrap_or_else(|e| panic!("pid field {pid_field:?} did not parse as an integer: {e}"));
        assert_eq!(pid_value, std::process::id(), "the sidecar's own pid field must be this process's real pid");

        let tree_field = lines[1].split_once('=').expect("tree line has an '='").1;
        assert_eq!(tree_field, current_tree(), "the sidecar's own tree field must match this process's own current_tree()");

        let time_field = lines[3].split_once('=').expect("time line has an '='").1;
        // "%Y-%m-%d %H:%M:%S %z" shape, checked without pulling in a regex dependency just for
        // this: three space-separated parts of the expected lengths, the last starting with a
        // sign.
        let parts: Vec<&str> = time_field.split(' ').collect();
        assert_eq!(parts.len(), 3, "expected 'date time tz', got {time_field:?}");
        assert_eq!(parts[0].len(), 10, "date part {:?} of {time_field:?} should be YYYY-MM-DD", parts[0]);
        assert_eq!(parts[1].len(), 8, "time part {:?} of {time_field:?} should be HH:MM:SS", parts[1]);
        assert!(
            parts[2].len() == 5 && (parts[2].starts_with('+') || parts[2].starts_with('-')),
            "tz offset part {:?} of {time_field:?} should look like +ZZZZ/-ZZZZ",
            parts[2]
        );

        drop(guard);
        assert!(!sidecar_path.exists(), "the sidecar must be removed once the outermost (here, only) guard drops on the ordinary path");

        fs::remove_dir_all(&scratch).ok();
    }

    /// The sidecar removal on drop must run on an UNWINDING panic too, not only the ordinary
    /// return path -- `Drop for DockerTestLock` runs during unwind because this workspace is not
    /// built `panic = "abort"` (checked directly here: `catch_unwind` only ever catches an
    /// unwinding panic, never an aborting one, so this test would itself abort the whole test
    /// binary, not merely fail, if that assumption were ever wrong).
    #[test]
    fn holder_sidecar_is_removed_when_the_outermost_guard_drops_during_a_panic() {
        let scratch = repo_scratch_dir("sidecar-panic-cleanup");
        let private_lock_path = scratch.join("docker-tests.lock");
        let sidecar_path = holder_sidecar_path(&private_lock_path);

        let result = std::panic::catch_unwind(|| {
            let _guard = lock_docker_tests_at(private_lock_path.clone());
            assert!(sidecar_path.exists(), "the sidecar must exist while the guard is held, before this deliberate panic");
            panic!("deliberate panic while still holding the DockerTestLock guard, to prove its Drop runs during unwind");
        });

        assert!(result.is_err(), "the inner closure must have actually panicked -- this test proves nothing if it did not");
        assert!(!sidecar_path.exists(), "the sidecar must be removed even when the guard is dropped while unwinding a panic");

        fs::remove_dir_all(&scratch).ok();
    }

    /// The cross-language PATH-agreement claim for the sidecar, the same by-construction proof
    /// `rust_and_python_compute_the_identical_lock_path` above already gives the lock path
    /// itself: a bare `python3 -c` re-derives the sidecar path inline (never importing this
    /// module or `altavista.docker_test_lock` -- proving agreement BY CONSTRUCTION), and this
    /// test asserts it equals [`holder_sidecar_path`]'s own output. No locking at all, so this
    /// can never be affected by contention. The Python-side equivalent is
    /// `tests/test_docker_test_lock_cross_process.py::
    /// test_holder_sidecar_path_matches_the_documented_convention`.
    #[test]
    fn rust_and_python_compute_the_identical_holder_sidecar_path() {
        if let Some(line) = python3_skip_line("rust_and_python_compute_the_identical_holder_sidecar_path") {
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        const PYTHON_SIDECAR_PATH_PROBE: &str = r#"
import os
lock = os.path.join(os.environ["HOME"], ".altavista", "locks", "docker-tests.lock")
print(lock + ".holder")
"#;
        let output = Command::new("python3").args(["-c", PYTHON_SIDECAR_PATH_PROBE]).output().expect("python3 was already confirmed present by this test's own gate");
        assert!(output.status.success(), "the python3 sidecar-path probe itself must not error: stdout={:?} stderr={:?}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let python_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let rust_path = holder_sidecar_path(&lock_file_path());

        println!("holder sidecar path agreement proof: Rust holder_sidecar_path(lock_file_path()) = {rust_path:?}, python3's own inline computation = {python_path:?}");

        assert_eq!(
            rust_path.to_string_lossy(),
            python_path,
            "Rust's holder_sidecar_path(lock_file_path()) and a bare python3's own inline computation must name the IDENTICAL path -- got Rust={rust_path:?}, python3={python_path:?}"
        );
    }

    /// A REAL cross-language proof (not merely path agreement): a Rust [`DockerTestLock`] guard
    /// holds a PRIVATE lock (question 212(b)) and writes its own holder sidecar; a Python child
    /// running this repository's OWN production `altavista.docker_test_lock.lock_docker_tests()`
    /// -- not a hand-rolled probe -- contends on the identical path, blocks, reads that sidecar,
    /// and must report this Rust process as the (live, non-stale) holder in its own WAITING
    /// line before it acquires.
    ///
    /// The Python child cannot simply `import altavista.docker_test_lock`: that triggers
    /// `altavista/__init__.py`, which imports `numpy` (a real dependency this crate's own test
    /// gate does not install for a bare `python3`) -- discovered while building this test,
    /// recorded in this task's report as a defect in the ORIGINAL cross-language test design,
    /// not in the module under test. `docker_test_lock.py` itself has no such dependency (only
    /// stdlib imports), so this loads it directly via `importlib.util.spec_from_file_location`,
    /// bypassing the package `__init__.py` entirely -- still the real, unmodified production
    /// module code, just reached a different way.
    ///
    /// The other direction (a Python holder, a Rust waiter reading a Python-written sidecar) is
    /// proved by `a_rust_waiter_reads_and_reports_a_real_python_holders_sidecar` below.
    #[test]
    fn a_python_waiter_using_the_real_lock_docker_tests_reads_and_reports_this_rust_holders_sidecar() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;

        if let Some(line) = python3_skip_line("a_python_waiter_using_the_real_lock_docker_tests_reads_and_reports_this_rust_holders_sidecar") {
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        let scratch = repo_scratch_dir("cross-lang-sidecar");
        // `scratch` plays the role of a private `$HOME` for the Python child below: its own
        // `lock_path()`/`holder_sidecar_path()` resolve `$HOME/.altavista/locks/docker-tests.lock`
        // (and `...lock.holder`) to exactly this path -- the identical `.altavista/locks/`
        // convention `altavista/docker_test_lock.py`'s own module doc states, and the same
        // private-`$HOME` pattern `tests/test_docker_test_lock_cross_process.py`'s own
        // `_child_env` already uses (question 199/212(b)). Constructed here, independently of
        // any Python code, so the two sides meeting at the SAME file is itself part of the
        // by-construction proof.
        let private_lock_path = scratch.join(".altavista").join("locks").join("docker-tests.lock");
        let sidecar_path = holder_sidecar_path(&private_lock_path);

        let guard = lock_docker_tests_at(private_lock_path.clone());

        const PYTHON_REAL_WAITER: &str = r#"
import importlib.util
import os
import sys

repo_root = sys.argv[1]
module_path = os.path.join(repo_root, "altavista", "docker_test_lock.py")
spec = importlib.util.spec_from_file_location("docker_test_lock", module_path)
docker_test_lock = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docker_test_lock)

with docker_test_lock.lock_docker_tests():
    print("WAITER_ACQUIRED", flush=True)
"#;

        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut child = Command::new("python3")
            .args(["-c", PYTHON_REAL_WAITER, &repo_root.to_string_lossy()])
            .current_dir(&repo_root)
            .env("HOME", &scratch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("could not spawn the python3 waiter child: {e}"));

        let mut child_stderr = BufReader::new(child.stderr.take().expect("piped stderr"));
        let waiting_line = loop {
            let mut line = String::new();
            let n = child_stderr.read_line(&mut line).expect("read a line from the child's real stderr pipe");
            assert_ne!(n, 0, "the python3 waiter's stderr pipe closed before ever printing a WAITING line");
            if line.contains("WAITING") {
                break line.trim().to_string();
            }
        };

        // Release the Rust guard -- the Python child's own blocked fcntl.flock(LOCK_EX) can now
        // proceed.
        drop(guard);

        let acquired_line = loop {
            let mut line = String::new();
            let n = child_stderr.read_line(&mut line).expect("read a line from the child's real stderr pipe");
            assert_ne!(n, 0, "the python3 waiter's stderr pipe closed before ever printing an ACQUIRED line");
            if line.contains("ACQUIRED the docker-test lock") {
                break line.trim().to_string();
            }
        };

        let output_status = child.wait().expect("the python3 waiter child must exit");
        let mut child_stdout_buf = String::new();
        if let Some(mut out) = child.stdout.take() {
            std::io::Read::read_to_string(&mut out, &mut child_stdout_buf).ok();
        }
        let mut remaining_stderr = String::new();
        std::io::Read::read_to_string(&mut child_stderr, &mut remaining_stderr).ok();

        fs::remove_dir_all(&scratch).ok();

        std::io::stdout()
            .write_all(
                format!(
                    "cross-language sidecar proof (Rust holder, Python waiter): sidecar path = {sidecar_path:?}; WAITING line = {waiting_line:?}; ACQUIRED line = {acquired_line:?}; child stdout = {child_stdout_buf:?}\n"
                )
                .as_bytes(),
            )
            .ok();

        assert!(output_status.success(), "the python3 waiter child must itself exit successfully -- stderr tail: {remaining_stderr:?}, stdout: {child_stdout_buf:?}");
        assert!(child_stdout_buf.contains("WAITER_ACQUIRED"), "the python3 waiter must have actually acquired the lock -- got stdout {child_stdout_buf:?}");

        assert!(
            waiting_line.contains("WAITING") && waiting_line.contains("docker-test lock") && waiting_line.contains("question 207"),
            "expected a WAITING line naming the docker-test lock and citing question 207, got {waiting_line:?}"
        );
        assert!(
            waiting_line.contains(&format!("pid={}", std::process::id())),
            "expected the Python waiter's WAITING line to name THIS Rust process's own pid (read from the sidecar this guard wrote), got {waiting_line:?}"
        );
        assert!(
            waiting_line.contains(&current_tree()),
            "expected the Python waiter's WAITING line to name THIS Rust process's own tree (current_tree()), got {waiting_line:?}"
        );
        assert!(
            !waiting_line.to_lowercase().contains("stale"),
            "the Rust holder was alive and well when this WAITING line was printed -- it must not be reported as stale, got {waiting_line:?}"
        );
        assert!(
            acquired_line.starts_with("ACQUIRED the docker-test lock") && acquired_line.contains("after waiting"),
            "expected an ACQUIRED-after-waiting line reporting the real elapsed wait, got {acquired_line:?}"
        );
    }

    // -------------------------------------------------------------------------------------
    // The reverse direction (round-5 "What remains"; questions 207, 212(b), 234, 236, 237): a
    // PYTHON holder, a RUST waiter. Helpers first, then the test.
    // -------------------------------------------------------------------------------------

    /// Wall-clock bound on every single wait in the Python-holder/Rust-waiter proof below. Each
    /// wait blocks on a real event (a child's own output line); this only bounds how long a
    /// regression may hang the test before it fails with the lines seen so far.
    const CHILD_EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    /// Kills and reaps its child on every exit path, including a panic unwinding through the
    /// test -- a leaked Python holder would keep the private lock (and a leaked waiter a blocked
    /// `flock`) alive past the test.
    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Forwards `reader`'s lines over a channel from a helper thread, so a test can wait on the
    /// next line with a wall-clock deadline ([`recv_line_containing`]) instead of an unbounded
    /// blocking `read_line`. The channel disconnects when the stream reaches EOF.
    fn spawn_line_pump<R: std::io::Read + Send + 'static>(reader: R) -> std::sync::mpsc::Receiver<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(reader).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        rx
    }

    /// The next line from `rx` containing `needle` (other lines are skipped: filtered on content,
    /// never on position), or a panic naming `what` and every line seen if `wait` of wall-clock
    /// time passes or the stream ends first.
    fn recv_line_containing(rx: &std::sync::mpsc::Receiver<String>, needle: &str, wait: std::time::Duration, what: &str) -> String {
        use std::sync::mpsc::RecvTimeoutError;
        let deadline = std::time::Instant::now() + wait;
        let mut seen: Vec<String> = Vec::new();
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(line) if line.contains(needle) => return line.trim().to_string(),
                Ok(line) => seen.push(line),
                Err(RecvTimeoutError::Timeout) => panic!("timed out after {wait:?} waiting for {what} (a line containing {needle:?}); lines seen: {seen:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("the stream ended before {what} (a line containing {needle:?}); lines seen: {seen:?}"),
            }
        }
    }

    /// The reverse of `a_python_waiter_using_the_real_lock_docker_tests_reads_and_reports_this_
    /// rust_holders_sidecar` above: a Python process holds a PRIVATE lock (question 212(b)) via
    /// this repository's own production `altavista.docker_test_lock.lock_docker_tests()` -- which
    /// is what writes the sidecar, so there is no hand-written `flock` or record anywhere -- and
    /// a Rust process running the production `lock_docker_tests_at` wait path (the existing
    /// [`probe_process_that_blocks_acquiring_the_docker_test_lock`] probe, pointed at the private
    /// lock through `AV_LOCKSTEP_PROBE_LOCK_PATH`) must print the Python holder's sidecar fields
    /// in its WAITING line, then acquire once the Python holder releases.
    ///
    /// The Rust waiter is this very test binary (`std::env::current_exe`) re-run with `--exact`
    /// on the probe, not the nested `cargo test` the announce test uses: same probe, same
    /// production code, but no build-lock or cargo-slot contention and no compile inside the
    /// window the private lock is held.
    ///
    /// Pins the exact fields rather than "contains HELD": `pid` is the Python child's real pid
    /// (`Child::id()` from the spawn, cross-checked against the pid the child itself printed),
    /// `tree` is the child's cwd as `os.getcwd()` reports it (the canonicalised repo root it is
    /// spawned in), and `command` is `sys.executable -c <repo root>` -- the Python side's
    /// `_current_command_line` over `sys.argv == ["-c", repo_root]`, with the executable path
    /// reported by the child itself. The record's `time` is the one field not pinned beyond being
    /// present. Neither the "unknown" nor the "stale" variant may appear.
    ///
    /// Same loading trick as the Rust-holder test above (`importlib.util.spec_from_file_location`,
    /// because `altavista/__init__.py` imports `numpy`, which a bare `python3` does not have).
    #[test]
    fn a_rust_waiter_reads_and_reports_a_real_python_holders_sidecar() {
        use std::io::Write;
        use std::process::Stdio;

        if let Some(line) = python3_skip_line("a_rust_waiter_reads_and_reports_a_real_python_holders_sidecar") {
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        // `scratch` is the Python child's private `$HOME`; the lock it takes is
        // `$HOME/.altavista/locks/docker-tests.lock`, and the Rust waiter is handed that same
        // path through the probe's environment variable -- so the two sides meet at one file
        // without either importing the other's path logic.
        let scratch = repo_scratch_dir("py-holder-rust-waiter");
        let private_lock_path = scratch.join(".altavista").join("locks").join("docker-tests.lock");
        let sidecar_path = holder_sidecar_path(&private_lock_path);

        // Canonical, so the child's `os.getcwd()` (the physical path) and the `argv` it receives
        // are byte-identical to what this test expects, not merely equivalent through `..`.
        let repo_root = fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")).expect("the repository root exists");
        let repo_root_str = repo_root.to_string_lossy().into_owned();

        const PYTHON_REAL_HOLDER: &str = r#"
import importlib.util
import os
import sys

repo_root = sys.argv[1]
module_path = os.path.join(repo_root, "altavista", "docker_test_lock.py")
spec = importlib.util.spec_from_file_location("docker_test_lock", module_path)
docker_test_lock = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docker_test_lock)

with docker_test_lock.lock_docker_tests():
    print(f"HOLDER_READY {os.getpid()} {sys.executable}", flush=True)
    # Hold until the test closes our stdin (or kills us): a real event, never a sleep.
    sys.stdin.readline()
"#;

        let mut holder = KillOnDrop(
            Command::new("python3")
                .args(["-c", PYTHON_REAL_HOLDER, &repo_root_str])
                .current_dir(&repo_root)
                .env("HOME", &scratch)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("could not spawn the python3 holder child: {e}")),
        );
        let holder_pid = holder.0.id();
        let holder_stdin = holder.0.stdin.take().expect("piped stdin");
        let holder_stdout = spawn_line_pump(holder.0.stdout.take().expect("piped stdout"));
        let holder_stderr = spawn_line_pump(holder.0.stderr.take().expect("piped stderr"));

        // The holder prints this only from inside `lock_docker_tests()`, i.e. after it has taken
        // the real `flock` and written its sidecar.
        let ready_line = recv_line_containing(&holder_stdout, "HOLDER_READY", CHILD_EVENT_TIMEOUT, "the python3 holder to take the lock");
        let (printed_pid, python_executable) = ready_line
            .strip_prefix("HOLDER_READY ")
            .and_then(|rest| rest.split_once(' '))
            .unwrap_or_else(|| panic!("unparsable HOLDER_READY line {ready_line:?}"));
        assert_eq!(printed_pid, holder_pid.to_string(), "the pid the child printed must be the pid of the process this test spawned");
        let raw_sidecar = fs::read_to_string(&sidecar_path).unwrap_or_else(|e| panic!("the python3 holder must have written its sidecar {sidecar_path:?} before reporting ready: {e}"));

        // The Rust waiter: this test binary re-run on the probe, against the same private lock.
        let mut waiter = KillOnDrop(
            Command::new(std::env::current_exe().expect("the path of this test binary"))
                .args(["--exact", "docker_test_lock::tests::probe_process_that_blocks_acquiring_the_docker_test_lock", "--nocapture"])
                .env("AV_LOCKSTEP_PROBE_LOCK_PATH", &private_lock_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("could not spawn the Rust waiter child: {e}")),
        );
        let waiter_stdout = spawn_line_pump(waiter.0.stdout.take().expect("piped stdout"));
        let waiter_stderr = spawn_line_pump(waiter.0.stderr.take().expect("piped stderr"));

        let waiting_line = recv_line_containing(&waiter_stderr, "WAITING", CHILD_EVENT_TIMEOUT, "the Rust waiter's WAITING line");

        // Release: closing the holder's stdin ends its `readline()`, so its `with` block exits
        // (removing the sidecar, then unlocking) -- the waiter's blocked `flock` can now proceed.
        drop(holder_stdin);

        let acquired_line = recv_line_containing(&waiter_stderr, "ACQUIRED the docker-test lock", CHILD_EVENT_TIMEOUT, "the Rust waiter to acquire after the python3 holder released");
        recv_line_containing(&waiter_stdout, "PROBE_ACQUIRED", CHILD_EVENT_TIMEOUT, "the Rust waiter's own acquisition confirmation");
        let waiter_status = waiter.0.wait().expect("the Rust waiter child must exit");
        let holder_status = holder.0.wait().expect("the python3 holder child must exit");

        // Question 148: print what was actually observed, not a paraphrase.
        std::io::stdout()
            .write_all(
                format!(
                    "cross-language sidecar proof (Python holder, Rust waiter): holder pid = {holder_pid}; sidecar path = {sidecar_path:?}; sidecar contents = {raw_sidecar:?}; WAITING line = {waiting_line:?}; ACQUIRED line = {acquired_line:?}\n"
                )
                .as_bytes(),
            )
            .ok();

        // Both children have exited, so nothing still uses the directory.
        fs::remove_dir_all(&scratch).ok();

        assert!(holder_status.success(), "the python3 holder must exit successfully, got {holder_status:?}; its stderr lines: {:?}", holder_stderr.try_iter().collect::<Vec<_>>());
        assert!(waiter_status.success(), "the Rust waiter must exit successfully, got {waiter_status:?}");

        let expected_command = format!("{python_executable} -c {repo_root_str}");
        let expected_holder_clause = format!("holder: pid={holder_pid} tree={repo_root_str} command={expected_command:?} time=");
        assert!(
            waiting_line.contains("docker-test lock") && waiting_line.contains("question 207") && waiting_line.contains(&private_lock_path.display().to_string()),
            "expected a WAITING line naming the docker-test lock, its private path and question 207, got {waiting_line:?}"
        );
        assert!(
            waiting_line.contains(&expected_holder_clause),
            "expected the Rust waiter's WAITING line to carry the Python holder's own sidecar fields {expected_holder_clause:?} (pid = the spawned child's real pid, tree = its cwd, command = its own command line), got {waiting_line:?}"
        );
        assert!(
            !waiting_line.contains("holder: unknown") && !waiting_line.to_lowercase().contains("stale"),
            "the Python holder was alive and its sidecar readable -- the line must be neither the unknown nor the stale variant, got {waiting_line:?}"
        );
        assert!(
            acquired_line.starts_with("ACQUIRED the docker-test lock") && acquired_line.contains("after waiting"),
            "expected an ACQUIRED-after-waiting line reporting the real elapsed wait, got {acquired_line:?}"
        );
    }
}

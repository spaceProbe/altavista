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

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

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

/// An RAII guard over the host-wide `flock(LOCK_EX)` on [`lock_file_path`]. Holding one is proof
/// (enforced at compile time via [`crate::docker::prune_stale_test_resources`]'s own `&
/// DockerTestLock` parameter) that this process currently has exclusive claim to every
/// Docker-gated test on this host -- see this module's own doc comment for the full account of
/// why the scope is daemon-wide and cross-language.
///
/// Deliberately holds only a [`File`] (the open file description the lock is attached to) --
/// nothing else needs to be tracked; `Drop` releases the lock by closing that file descriptor
/// (see [`Drop`]'s own doc comment on this type for why that is also what makes this survive a
/// `SIGKILL`, unlike a `Drop`-based guard with no kernel-owned resource behind it).
#[derive(Debug)]
pub struct DockerTestLock {
    file: File,
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
    let path = lock_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("could not create {parent:?} for the docker-test lock: {e}"));
    }
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
        crate::docker::write_real_stderr(&format!(
            "WAITING for the docker-test lock ({}): another process on this host currently holds it -- \
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
    DockerTestLock { file }
}

impl Drop for DockerTestLock {
    /// Releases the lock. Two redundant-looking things happen here, deliberately:
    ///
    /// 1. An explicit `flock(LOCK_UN)` -- makes the release visible at exactly this point in the
    ///    code, rather than relying entirely on the implicit close below.
    /// 2. `self.file`'s own `Drop` (run automatically right after this method returns) closes
    ///    the file descriptor, which *also* releases the lock -- this is not belt-and-suspenders
    ///    against a bug, it is the actual mechanism that makes this lock survive `SIGKILL`: a
    ///    killed process never runs this `Drop` impl at all, but the KERNEL still closes every
    ///    file descriptor a killed process held, which releases every `flock` that process held
    ///    -- with zero userspace code (this method included) ever running. The explicit
    ///    `LOCK_UN` call exists only for the ordinary (non-killed) case's own clarity; the
    ///    guarantee this whole module exists for comes from the second, kernel-driven release.
    fn drop(&mut self) {
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

    /// Deliberately re-derives the lock path inline from `$HOME`, rather than importing
    /// `altavista.docker_test_lock` -- this is the plain system `python3` a fresh checkout has
    /// on `PATH`, not necessarily this repository's own `.venv`, and the whole point of this
    /// test is to prove the two independent implementations agree on the path BY CONSTRUCTION,
    /// not by one importing the other's constant.
    const PYTHON_FLOCK_PROBE: &str = r#"
import fcntl, os
home = os.environ["HOME"]
path = os.path.join(home, ".altavista", "locks", "docker-tests.lock")
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

    fn run_python_probe() -> String {
        let output = Command::new("python3").args(["-c", PYTHON_FLOCK_PROBE]).output().expect("python3 was already confirmed present by this test's own gate");
        assert!(output.status.success(), "the python3 probe itself must not error: stdout={:?} stderr={:?}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn flock_lock_is_visible_across_processes_and_languages() {
        // This test needs no Docker at all (question 194's own distinction: gate on the ONE
        // real precondition this test has, never a broader "docker available" check that would
        // misreport the actual reason for a skip) -- only python3 on PATH.
        let python3_present = Command::new("python3").arg("--version").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !python3_present {
            let reason = crate::docker::DockerGateReason::PrerequisiteUnavailable { what: "python3".to_string(), hint: "install python3 and put it on PATH".to_string() };
            let line = crate::docker::announce_gate_skip("flock_lock_is_visible_across_processes_and_languages", &reason);
            assert!(line.starts_with("SKIPPED "), "the gate helper must announce a visible skip line: {line:?}");
            return;
        }

        let guard = lock_docker_tests();
        let observed_while_held = run_python_probe();
        drop(guard);
        let observed_after_drop = run_python_probe();

        // Question 148: an exit code is not evidence -- print exactly what was observed
        // (visible with `--nocapture`; also asserted on directly below, not merely eyeballed).
        println!("flock cross-process/cross-language proof: while the Rust guard held the lock, the python3 child observed {observed_while_held:?}; after the guard was dropped, the identical python3 child observed {observed_after_drop:?}");

        assert_eq!(
            observed_while_held, "BLOCKED",
            "a python3 child using fcntl.flock(LOCK_EX|LOCK_NB) on the IDENTICAL path this Rust module computes from $HOME must fail to acquire while this process's own DockerTestLock guard holds it -- got {observed_while_held:?} (either the paths disagree, or the lock is not actually exclusive)"
        );
        assert_eq!(
            observed_after_drop, "ACQUIRED",
            "after the Rust guard is dropped (closing its fd releases the flock), the SAME python3 child command must now succeed -- got {observed_after_drop:?}"
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
    // -------------------------------------------------------------------------------------

    /// Not meaningful on its own -- exists ONLY as the nested-subprocess target for
    /// `lock_docker_tests_announces_a_blocked_wait_never_silently` below. Acquires the REAL,
    /// production lock and announces success on stdout so the parent test can tell the child's
    /// own `flock` call actually returned (as opposed to the child having crashed before
    /// reaching it). When run as an ordinary part of this crate's own test suite (no
    /// contention), this passes trivially and silently, the same as any other acquire-then-
    /// release of an uncontended lock.
    #[test]
    fn probe_process_that_blocks_acquiring_the_docker_test_lock() {
        let _guard = lock_docker_tests();
        println!("PROBE_ACQUIRED");
    }

    #[test]
    fn lock_docker_tests_announces_a_blocked_wait_never_silently() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::Stdio;

        // This process acquires the lock FIRST -- nothing else holds it yet, so this is the
        // silent, non-blocking path (no announcement expected here).
        let guard = lock_docker_tests();

        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        // `--nocapture` on the NESTED invocation: without it, libtest would swallow the probe
        // test's own `println!("PROBE_ACQUIRED")` on a passing run (the exact question-194
        // capture behaviour this crate already documents elsewhere) -- this test needs that
        // line visible in the child's own stdout to confirm the child genuinely reached and
        // passed through its own `flock` call, not merely that the subprocess exited 0. The
        // WAITING/ACQUIRED lines themselves need no such flag (`write_real_stderr` already
        // bypasses libtest's capture on its own), but harmless to request together.
        let mut child = Command::new("cargo")
            .args(["test", "-p", "av-lockstep", "--lib", "--", "--exact", "docker_test_lock::tests::probe_process_that_blocks_acquiring_the_docker_test_lock", "--nocapture"])
            .current_dir(&repo_root)
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
}

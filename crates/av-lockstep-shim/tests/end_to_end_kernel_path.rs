//! M23.1's acceptance evidence: "A Python reference peer as a test fixture, proving the
//! shim end to end against the existing kernel container path before any cFS code exists."
//!
//! This test drives the real `av-lockstep-shim` binary (built by Cargo for this test run,
//! via `env!("CARGO_BIN_EXE_av-lockstep-shim")`) exactly the way `crates/av-kernel`'s
//! `BINDING_KIND_CONTAINER` executor would: through `av_lockstep::BlockingLockstepClient`
//! (unmodified -- the same client crate the kernel's `ContainerModel` uses), plaintext
//! loopback, `Bind` -> several `Step`s -> `Reset` -> `Step` -> `Shutdown`. On the other
//! side of the shim, `tests/lockstep_local_peer.py` (this repository's own new "lockstep-
//! local v1" reference peer, `../../tests/lockstep_local_peer.py` relative to this crate)
//! connects over the Unix socket standing in for a real cFS lockstep I/O app.
//!
//! Nothing here touches `crates/av-kernel` -- it reuses the same public client crate the
//! kernel depends on, from an ordinary test harness, which is what "against the existing
//! kernel container path" means without modifying (or even depending on) that crate.
use std::io::ErrorKind;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// `av_lockstep`'s own `LockstepBindRequest`/`LockstepStepRequest`/`LockstepResetRequest`/
// `LockstepShutdownRequest` (re-exported from `av_grpc::pb`, the module its
// `BlockingLockstepClient` methods actually take) are a *separate*, independently-generated
// copy of these message structs from `av_cdm::pb`'s -- `av-grpc/build.rs` only
// `extern_path`s `Port`/`PortMessage` onto `av_cdm::pb`, not the whole `altavista.v1`
// package (see that build script's own comment) -- so this test must use `av_lockstep`'s
// copies for anything passed to `BlockingLockstepClient`, and only `Port`/`PortMessage`
// (and the `PortKind`/`PortDirection` enums used to build a `Port`) from `av_cdm::pb`.
use av_cdm::pb::{Port, PortDirection, PortKind, PortMessage};
use av_lockstep::{BlockingLockstepClient, LockstepBindRequest, LockstepResetRequest, LockstepShutdownRequest, LockstepStepRequest};

/// Kills and reaps the wrapped child on drop, so a failed assertion (which unwinds past
/// the normal end of the test) never leaves the shim or the Python peer running.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate lives at <repo>/crates/av-lockstep-shim")
        .to_path_buf()
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port").local_addr().unwrap().port()
}

fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut ready: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn signal_payload(value: f64) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn decode_signal(payload: &[u8]) -> f64 {
    f64::from_le_bytes(payload.try_into().expect("SIGNAL payload must be exactly 8 bytes"))
}

#[test]
fn the_shim_drives_a_full_run_through_the_kernels_own_lockstep_client() {
    let repo_root = repo_root();
    let python = repo_root.join(".venv").join("bin").join("python3");
    assert!(python.is_file(), "expected a repo-local venv python at {}", python.display());
    let peer_script = repo_root.join("tests").join("lockstep_local_peer.py");
    assert!(peer_script.is_file(), "expected the reference peer fixture at {}", peer_script.display());

    // Deliberately NOT `std::env::temp_dir()`: on macOS that resolves to a long
    // per-process `$TMPDIR` under `/var/folders/...`, and a `sockaddr_un.sun_path` is
    // capped at 104 bytes on both macOS and Linux -- a path built from it plus any
    // per-run uniqueness routinely overflows that limit, which fails `UnixListener::bind`
    // silently as far as this test's own polling loop is concerned (the shim process exits
    // immediately with an error this test never sees, since it only *waits* for the socket
    // file to appear). `/tmp` (short, and the same directory `mktemp -d` itself defaults
    // to) keeps the whole path well under the limit.
    let run_dir = PathBuf::from(format!("/tmp/av-ls-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&run_dir).expect("create a scratch dir for the socket");
    let socket_path = run_dir.join("lockstep.sock");
    let grpc_addr = format!("127.0.0.1:{}", free_tcp_port());

    // 1. The shim: the same binary `services/cfs` will eventually run beside cFS, built by
    //    Cargo for this test.
    let shim_bin = env!("CARGO_BIN_EXE_av-lockstep-shim");
    let mut shim = ChildGuard(
        Command::new(shim_bin)
            .arg("--socket-path")
            .arg(&socket_path)
            .arg("--grpc-addr")
            .arg(&grpc_addr)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn av-lockstep-shim"),
    );

    if !wait_until(Duration::from_secs(10), || socket_path.exists()) {
        let status = shim.0.try_wait().ok().flatten();
        panic!("av-lockstep-shim never created its Unix socket at {} (process status: {status:?})", socket_path.display());
    }

    // 2. The Python reference peer, standing in for a real cFS lockstep I/O app.
    let mut peer = ChildGuard(
        Command::new(&python)
            .arg(&peer_script)
            .arg("--socket-path")
            .arg(&socket_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the Python reference peer"),
    );

    assert!(
        wait_until(Duration::from_secs(15), || match TcpStream::connect(&grpc_addr) {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::ConnectionRefused => false,
            Err(e) => panic!("unexpected error probing {grpc_addr}: {e}"),
        }),
        "av-lockstep-shim's LockstepService never became ready on {grpc_addr}"
    );

    // 3. Drive it exactly the way `crates/av-kernel`'s `ContainerModel` drives a
    //    BINDING_KIND_CONTAINER process -- the same client crate, plaintext loopback.
    let mut client = BlockingLockstepClient::connect_plaintext(&grpc_addr).expect("connect to the shim's LockstepService");

    let bind_response = client
        .bind(LockstepBindRequest {
            run_id: "e2e-run".to_string(),
            instance: "adcs-sig".to_string(),
            ports: vec![
                Port { name: "in".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, schema: "signal".to_string(), timing: None, interface_class: String::new() },
                Port { name: "out".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, schema: "signal".to_string(), timing: None, interface_class: String::new() },
            ],
            start_tai_ns: 0,
            base_period_ns: 1_000_000_000,
            step_period_ns: 1_000_000_000,
            seed: 7,
            parameters: Default::default(),
        })
        .expect("Bind RPC");
    assert!(bind_response.lockstep_capable, "refusal_reason={:?}", bind_response.refusal_reason);
    assert_eq!(bind_response.binding_hash.len(), 64, "expected a hex-encoded SHA-256");

    // Three steps at constant SIGNAL inputs -- outputs must flow back correctly (this is
    // the acceptance criterion: "a run completes with outputs flowing back").
    let mut t: i64 = 0;
    let mut running = 0.0f64;
    for (sequence, value) in [(1u64, 2.0f64), (2, 3.0), (3, -1.0)] {
        let until = t + 1_000_000_000;
        let step_response = client
            .step(LockstepStepRequest {
                sequence,
                until_tai_ns: until,
                inputs: vec![PortMessage { port: "in".to_string(), tai_ns: t, payload: signal_payload(value) }],
            })
            .unwrap_or_else(|e| panic!("Step RPC (sequence {sequence}): {e}"));
        running += value * 1.0; // dt_s = 1.0 for every step here
        assert_eq!(step_response.sequence, sequence);
        assert_eq!(step_response.reached_tai_ns, until, "lockstep.proto: reached_tai_ns must equal until_tai_ns");
        assert_eq!(step_response.outputs.len(), 1);
        assert_eq!(step_response.outputs[0].port, "out");
        let got = decode_signal(&step_response.outputs[0].payload);
        assert!((got - running).abs() < 1e-9, "step {sequence}: got={got} want={running}");
        assert!((step_response.named_outputs["integral"] - running).abs() < 1e-9);
        t = until;
    }

    // Reset (power_cycle) must zero the peer's running integral -- proves `Reset` reaches
    // the peer through the local protocol, not just that the RPC itself returns.
    let reset_response = client.reset(LockstepResetRequest { sequence: 4, tai_ns: t, reason: "power_cycle".to_string() }).expect("Reset RPC");
    assert_eq!(reset_response.sequence, 4);

    let post_reset = client
        .step(LockstepStepRequest { sequence: 5, until_tai_ns: t + 1_000_000_000, inputs: vec![] })
        .expect("Step RPC after Reset");
    assert!((post_reset.named_outputs["integral"]).abs() < 1e-12, "Reset must zero the integral, not merely re-anchor the clock");

    client.shutdown(LockstepShutdownRequest { run_id: "e2e-run".to_string() }).expect("Shutdown RPC");

    // Both children should exit on their own shortly after Shutdown -- the peer because
    // this test's own `run()` loop stops on FRAME_SHUTDOWN_ACK's own send, and the shim
    // because... (see the crate's own binary: it exits when the gRPC server future
    // returns, which does not happen on Shutdown alone). Explicit cleanup below covers
    // whichever of the two needs it; ChildGuard's Drop covers the rest either way.
    let _ = peer.0.wait();
    let _ = std::fs::remove_dir_all(&run_dir);
    drop(shim);
}

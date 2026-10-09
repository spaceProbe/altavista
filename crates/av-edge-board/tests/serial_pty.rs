//! The serial transport against a pseudo-terminal stand-in (no board): the real
//! `av-edge-board` binary opens the slave side of a pty as `<path>@115200`; a fake
//! lockstep-local guest runs on the master side and decodes frames with
//! `av_lockstep_shim::framing`'s layout; the kernel's own gRPC client drives a full run
//! through the service. The master side measures what the guest's UART would see: no burst
//! above 64 bytes, and the gaps between bursts at least the previous burst's wire time.
mod common;

use std::time::Duration;

use av_edge_board::serial::{wire_time, MAX_CHUNK_BYTES};

/// The real Cadence UART receive FIFO depth, the bound the guest side can take in one burst. Held here
/// independently of the crate's own constant so the measurement does not follow a change to it.
const FIFO_DEPTH: usize = 64;
use common::*;

const BAUD: u32 = 115_200;

#[test]
fn a_full_run_over_a_pty_with_paced_writes() {
    let pty = open_pty();
    let master_fd = {
        use std::os::fd::AsRawFd;
        pty.master.as_raw_fd()
    };
    let guest = SerialGuest::spawn(master_fd);
    let device = format!("{}@{BAUD}", pty.slave_path);
    let mut service = Service::spawn("serial", &["--port-device", &device, "--edge-node-id", "edge-pty"]);
    service.wait_ready(Duration::from_secs(20));

    let t0 = std::time::Instant::now();
    let bind = drive_full_run(&service.grpc_addr, &service.bind_params(&Default::default()));
    let run_time = t0.elapsed();
    assert_eq!(bind.version, "fake-guest-1");

    // The service exits by itself after the Shutdown RPC.
    let status = service.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "av-edge-board exit status {status}; stderr:\n{}", service.stderr());
    let stderr = service.stderr();
    service.save_evidence("serial_pty.stderr.txt");
    for stage in ["link open", "handshake complete", "LockstepService listening", "Shutdown acknowledged", "stopped"] {
        assert!(stderr.contains(stage), "stderr lacks the stage {stage:?}:\n{stderr}");
    }
    let events = guest.events.lock().unwrap().clone();
    let brain_frames = {
        let b = guest.brain.lock().unwrap();
        assert_eq!(b.step_sequences, [1, 2, 3, 4, 6]);
        assert_eq!(b.step_input_bytes, [300, 400, 1000, 40, 700], "the guest received every STEP's inputs whole, over 256 bytes included");
        b.frames.len()
    };
    guest.finish();
    assert_eq!(brain_frames, 1 + 1 + 5 + 1 + 1, "HELLO, BIND, 5 STEPs, RESET, SHUTDOWN");

    // ---- Pacing, measured at the master side (what the guest's UART receives). ----
    let wt64 = wire_time(FIFO_DEPTH, BAUD);
    // One "burst" is one `read` return at the master. Time-clustering reads would merge a short
    // final chunk (wire time well under a millisecond) with the next frame's first chunk, so
    // each read is taken as it came; the reader polls with no delay, and a read that coalesced
    // two chunks would show here as over 64 bytes (an honest failure under extreme host load).
    let b = bursts(&events, Duration::ZERO);
    let sizes: Vec<usize> = b.iter().map(|x| x.1).collect();
    let max_burst = *sizes.iter().max().unwrap();
    let total: usize = sizes.iter().sum();
    println!("PTY-BURSTS reads={} total_bytes={total} max_burst={max_burst} (the guest-side FIFO depth is {FIFO_DEPTH})", sizes.len());
    assert!(max_burst <= FIFO_DEPTH, "a burst of {max_burst} bytes reached the guest side; bursts: {sizes:?}");
    // Reader-wake jitter allowance: a burst can be timestamped late, which shrinks the
    // measured gap to the next one by at most this much.
    let tolerance = Duration::from_micros(2_500);
    let mut gaps_after_full: Vec<Duration> = Vec::new();
    for pair in b.windows(2) {
        let (start, size) = pair[0];
        let gap = pair[1].0.duration_since(start);
        let need = wire_time(size, BAUD);
        assert!(gap + tolerance >= need, "a {size}-byte burst was followed after {gap:?}, less than its wire time {need:?} (tolerance {tolerance:?})");
        if size == FIFO_DEPTH {
            gaps_after_full.push(gap);
        }
    }
    gaps_after_full.sort();
    let median = gaps_after_full[gaps_after_full.len() / 2];
    let min = gaps_after_full[0];
    println!(
        "PTY-PACING bursts={} total_bytes={total} max_burst={max_burst} full_chunk_gaps={} min_gap={min:?} median_gap={median:?} wire_time_64B={wt64:?} tolerance_below={tolerance:?} run_wall_time={run_time:?}",
        b.len(),
        gaps_after_full.len()
    );
    assert!(median >= wt64.saturating_sub(tolerance) && median <= wt64 * 3, "median gap {median:?} vs wire time {wt64:?}");
    // The whole host-to-guest byte count: HELLO 11, BIND, STEPs (> 256 bytes each) ...
    assert!(total > 2_000, "total bytes {total}");
    assert_eq!(MAX_CHUNK_BYTES, FIFO_DEPTH, "the transport's chunk size is the real UART FIFO depth");
}

#[test]
fn the_serial_device_is_taken_exclusively() {
    // TIOCEXCL: a second open of the slave by the same (non-root) user is refused with EBUSY
    // while the service holds it. Linux enforces this on a pty slave; macOS does not (its pty
    // driver ignores the exclusive flag on open; only real serial drivers honour it), so there
    // the test shows the OS ignoring a TIOCEXCL the test itself set, and the property is
    // asserted on Linux only. Root bypasses TIOCEXCL everywhere (skipped visibly).
    let open_slave = |path: &str| {
        let c_path = std::ffi::CString::new(path).unwrap();
        // SAFETY: valid NUL-terminated path.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK) };
        let err = if fd < 0 { std::io::Error::last_os_error().raw_os_error() } else { None };
        (fd, err)
    };
    if unsafe { libc::geteuid() } == 0 {
        println!("SKIPPED the_serial_device_is_taken_exclusively: running as root, TIOCEXCL does not bind root");
        return;
    }
    let pty = open_pty();
    let master_fd = {
        use std::os::fd::AsRawFd;
        pty.master.as_raw_fd()
    };
    let guest = SerialGuest::spawn(master_fd);
    let device = format!("{}@{BAUD}", pty.slave_path);
    let mut service = Service::spawn("serial-excl", &["--port-device", &device, "--edge-node-id", "edge-pty"]);
    service.wait_ready(Duration::from_secs(20));
    let (fd, err) = open_slave(&pty.slave_path);
    if fd >= 0 {
        // SAFETY: just opened.
        unsafe { libc::close(fd) };
    }
    if cfg!(target_os = "linux") {
        assert!(fd < 0 && err == Some(libc::EBUSY), "a second open of the slave must fail with EBUSY, got fd={fd} errno={err:?}");
        println!("EXCLUSIVE second open of {} refused with EBUSY", pty.slave_path);
    } else {
        println!("EXCLUSIVE-NOT-ASSERTED (non-Linux): second open of {} returned fd={fd} errno={err:?}", pty.slave_path);
        // Control: on a fresh pty, set TIOCEXCL ourselves; the OS still lets a second open in.
        let control = open_pty();
        let (a, _) = open_slave(&control.slave_path);
        assert!(a >= 0);
        // SAFETY: `a` is an open tty descriptor.
        assert_eq!(unsafe { libc::ioctl(a, libc::TIOCEXCL as _) }, 0);
        let (b, berr) = open_slave(&control.slave_path);
        println!("EXCLUSIVE-CONTROL (non-Linux): after the test set TIOCEXCL itself, a second open returned fd={b} errno={berr:?}");
        assert!(b >= 0, "expected this OS's pty driver to ignore TIOCEXCL on open (the documented limit of this test)");
        // SAFETY: both are open descriptors.
        unsafe {
            libc::close(a);
            libc::close(b);
        }
    }
    drop(service);
    guest.finish();
}

#[test]
fn a_bad_device_and_an_unsupported_baud_are_startup_errors() {
    let mut s = Service::spawn("serial-bad", &["--port-device", "/dev/null@115200", "--edge-node-id", "e"]);
    let status = s.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(s.stderr().contains("not a terminal device"), "{}", s.stderr());
    let mut s = Service::spawn("serial-spec", &["--port-device", "ttyUSB0@115200", "--edge-node-id", "e"]);
    let status = s.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(s.stderr().contains("relative"), "{}", s.stderr());
    // A handshake that never completes is a typed timeout, not a hang: nobody answers on this pty.
    let pty = open_pty();
    let device = format!("{}@{BAUD}", pty.slave_path);
    let mut s = Service::spawn("serial-timeout", &["--port-device", &device, "--edge-node-id", "e", "--handshake-timeout-ms", "700"]);
    let status = s.wait_exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1));
    assert!(s.stderr().contains("did not complete within 700 ms"), "{}", s.stderr());
    drop(pty);
}

//! Shared stand-ins for the `av-edge-board` integration tests: a fake lockstep-local guest
//! (the "brain"), a pseudo-terminal pair, the real service binary as a child process, and a
//! client driver that talks to it through `av_lockstep::BlockingLockstepClient`, the client
//! the kernel's container path uses. No board is involved anywhere.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use av_cdm::pb::{LockstepBindRequest as CdmBindRequest, LockstepBindResponse, LockstepResetResponse, LockstepShutdownResponse, LockstepStepRequest as CdmStepRequest, LockstepStepResponse, Port, PortDirection, PortKind, PortMessage};
use av_lockstep::{BlockingLockstepClient, LockstepBindResponse as ClientBindResponse, LockstepStepResponse as ClientStepResponse, LockstepBindRequest, LockstepResetRequest, LockstepShutdownRequest, LockstepStepRequest};
use av_lockstep_shim::framing::{encode_frame, encode_hello, FrameType, PROTOCOL_VERSION};
use prost::Message;

// ---------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------

/// Pop one complete frame off the front of `buf`, if there is one.
pub fn take_frame(buf: &mut Vec<u8>) -> Option<(FrameType, Vec<u8>)> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return None;
    }
    let ty = FrameType::from_code(buf[4]).unwrap_or_else(|| panic!("fake guest: unknown frame type byte 0x{:02x}", buf[4]));
    let payload = buf[5..4 + len].to_vec();
    buf.drain(..4 + len);
    Some((ty, payload))
}

pub fn pattern(len: usize, salt: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(7).wrapping_add(salt)).collect()
}

// ---------------------------------------------------------------------------------------
// The fake guest's logic
// ---------------------------------------------------------------------------------------

/// What the fake guest has seen, and the rules it answers by. It is the lockstep-local v1
/// guest's side as `io_lockstep_app.c` plays it, with a trivial model: a STEP's reply carries
/// the byte sum of its inputs as `named_outputs["sum"]` and the inputs, concatenated and
/// reversed, as one `out` message.
#[derive(Default)]
pub struct Brain {
    /// The raw payload of every BIND frame received, in order.
    pub binds: Vec<Vec<u8>>,
    /// Every frame type received, in order.
    pub frames: Vec<FrameType>,
    /// The size of every STEP frame's input payload bytes received.
    pub step_input_bytes: Vec<usize>,
    /// Sequence numbers of STEPs received.
    pub step_sequences: Vec<u64>,
    /// Receive-side framing violations (a transport delivering something that is not one frame).
    pub framing_violations: usize,
    /// Reply with only the first half of the STEP_DONE frame for these STEP sequences (UDP).
    pub half_reply_for_sequences: Vec<u64>,
}

impl Brain {
    /// Handle one received frame; returns the reply frames (already encoded) and whether the
    /// guest is done (after SHUTDOWN).
    pub fn on_frame(&mut self, ty: FrameType, payload: &[u8]) -> (Vec<Vec<u8>>, bool) {
        self.frames.push(ty);
        match ty {
            FrameType::Hello => (vec![encode_frame(FrameType::Hello, &encode_hello(PROTOCOL_VERSION))], false),
            FrameType::Bind => {
                self.binds.push(payload.to_vec());
                let _decoded = CdmBindRequest::decode(payload).expect("fake guest: BIND payload decodes");
                let ack = LockstepBindResponse { lockstep_capable: true, binding_hash: "ab".repeat(32), version: "fake-guest-1".to_string(), refusal_reason: String::new() };
                (vec![encode_frame(FrameType::BindAck, &ack.encode_to_vec())], false)
            }
            FrameType::Step => {
                let req = CdmStepRequest::decode(payload).expect("fake guest: STEP payload decodes");
                self.step_sequences.push(req.sequence);
                let all: Vec<u8> = req.inputs.iter().flat_map(|m| m.payload.iter().copied()).collect();
                self.step_input_bytes.push(all.len());
                let sum: u64 = all.iter().map(|b| *b as u64).sum();
                let mut reversed = all;
                reversed.reverse();
                let resp = LockstepStepResponse {
                    sequence: req.sequence,
                    reached_tai_ns: req.until_tai_ns,
                    outputs: vec![PortMessage { port: "out".to_string(), tai_ns: req.until_tai_ns, payload: reversed }],
                    named_outputs: BTreeMap::from([("sum".to_string(), sum as f64)]),
                };
                let frame = encode_frame(FrameType::StepDone, &resp.encode_to_vec());
                if self.half_reply_for_sequences.contains(&req.sequence) {
                    (vec![frame[..frame.len() / 2].to_vec()], false)
                } else {
                    (vec![frame], false)
                }
            }
            FrameType::Reset => {
                let req = av_cdm::pb::LockstepResetRequest::decode(payload).expect("fake guest: RESET payload decodes");
                (vec![encode_frame(FrameType::ResetAck, &LockstepResetResponse { sequence: req.sequence }.encode_to_vec())], false)
            }
            FrameType::Shutdown => (vec![encode_frame(FrameType::ShutdownAck, &LockstepShutdownResponse {}.encode_to_vec())], true),
            other => panic!("fake guest: unexpected frame {other} from the service"),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Pseudo-terminal
// ---------------------------------------------------------------------------------------

pub struct Pty {
    pub master: OwnedFd,
    pub slave_path: String,
}

pub fn open_pty() -> Pty {
    // `ptsname` returns a pointer into static storage on some platforms.
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(fd >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
        let master = OwnedFd::from_raw_fd(fd);
        assert_eq!(libc::grantpt(fd), 0, "grantpt: {}", std::io::Error::last_os_error());
        assert_eq!(libc::unlockpt(fd), 0, "unlockpt: {}", std::io::Error::last_os_error());
        let name = libc::ptsname(fd);
        assert!(!name.is_null(), "ptsname: {}", std::io::Error::last_os_error());
        Pty { master, slave_path: CStr::from_ptr(name).to_str().unwrap().to_string() }
    }
}

/// Wait up to `timeout_ms` for `fd` to be readable; true if it is (or hung up).
pub fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd.
    let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
    r > 0
}

pub fn write_all_fd(fd: i32, mut data: &[u8]) {
    while !data.is_empty() {
        // SAFETY: valid buffer.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("write to the pty master: {e}");
        }
        data = &data[n as usize..];
    }
}

/// Read events from the master side: each `read` return is `(when, bytes)`.
pub type Events = Vec<(Instant, Vec<u8>)>;

pub struct SerialGuest {
    pub brain: Arc<Mutex<Brain>>,
    pub events: Arc<Mutex<Events>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SerialGuest {
    /// Run the fake guest on the master side of `master_fd`: read, timestamp, parse frames,
    /// answer. Replies are written in one `write` each, as fast as the pty takes them.
    pub fn spawn(master_fd: i32) -> Self {
        let brain = Arc::new(Mutex::new(Brain::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, ev, st) = (brain.clone(), events.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut stream: Vec<u8> = Vec::new();
            let mut buf = vec![0u8; 8192];
            while !st.load(Ordering::Relaxed) {
                if !poll_readable(master_fd, 20) {
                    continue;
                }
                // SAFETY: valid buffer.
                let n = unsafe { libc::read(master_fd, buf.as_mut_ptr().cast(), buf.len()) };
                let at = Instant::now();
                if n <= 0 {
                    break;
                }
                let n = n as usize;
                ev.lock().unwrap().push((at, buf[..n].to_vec()));
                stream.extend_from_slice(&buf[..n]);
                while let Some((ty, payload)) = take_frame(&mut stream) {
                    let (replies, done) = b.lock().unwrap().on_frame(ty, &payload);
                    for r in replies {
                        write_all_fd(master_fd, &r);
                    }
                    if done {
                        return;
                    }
                }
            }
        });
        Self { brain, events, stop, thread: Some(thread) }
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for SerialGuest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One cluster of bytes that arrived together at the master: `(first arrival, total bytes)`.
/// Reads closer together than `gap` belong to one burst.
pub fn bursts(events: &Events, gap: Duration) -> Vec<(Instant, usize)> {
    let mut out: Vec<(Instant, usize)> = Vec::new();
    let mut last: Option<Instant> = None;
    for (at, data) in events {
        match (last, out.last_mut()) {
            (Some(prev), Some(b)) if at.duration_since(prev) < gap => b.1 += data.len(),
            _ => out.push((*at, data.len())),
        }
        last = Some(*at);
    }
    out
}

// ---------------------------------------------------------------------------------------
// The service binary as a child process
// ---------------------------------------------------------------------------------------

pub struct Service {
    pub child: Child,
    pub grpc_addr: String,
    stderr_path: PathBuf,
    /// A per-service scratch directory: the throwaway identity, the I/O log.
    pub dir: PathBuf,
    pub identity: TestIdentity,
    /// Where `spawn` told the service to create its I/O log.
    pub io_log: PathBuf,
}

pub fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A throwaway P-384 key and a self-signed certificate for it, written as PEM files.
pub struct TestIdentity {
    pub key_pem: PathBuf,
    pub cert_pem: PathBuf,
    /// The bare public key (`-----BEGIN PUBLIC KEY-----`).
    pub pub_pem: PathBuf,
}

pub fn make_identity(dir: &std::path::Path, cn: &str) -> TestIdentity {
    use openssl::asn1::Asn1Time;
    use openssl::bn::BigNum;
    use openssl::ec::{EcGroup, EcKey};
    use openssl::hash::MessageDigest;
    use openssl::nid::Nid;
    use openssl::pkey::PKey;
    use openssl::x509::{X509Builder, X509NameBuilder};
    let key = EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap();
    let pkey = PKey::from_ec_key(key.clone()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, cn).unwrap();
    let name = name.build();
    let mut b = X509Builder::new().unwrap();
    b.set_version(2).unwrap();
    b.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap()).unwrap();
    b.set_subject_name(&name).unwrap();
    b.set_issuer_name(&name).unwrap();
    b.set_pubkey(&pkey).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(30).unwrap()).unwrap();
    b.sign(&pkey, MessageDigest::sha384()).unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let id = TestIdentity { key_pem: dir.join(format!("{cn}.key.pem")), cert_pem: dir.join(format!("{cn}.cert.pem")), pub_pem: dir.join(format!("{cn}.pub.pem")) };
    std::fs::write(&id.key_pem, key.private_key_to_pem().unwrap()).unwrap();
    std::fs::write(&id.cert_pem, b.build().to_pem().unwrap()).unwrap();
    std::fs::write(&id.pub_pem, pkey.public_key_to_pem().unwrap()).unwrap();
    id
}

impl Service {
    /// Start `av-edge-board` with `args` plus a fresh loopback `--grpc-addr`, a throwaway
    /// identity and a not-yet-existing I/O log (`self.io_log`). Its stderr goes to a file the
    /// test inspects.
    pub fn spawn(tag: &str, args: &[&str]) -> Self {
        Self::spawn_with(tag, args, true)
    }

    /// As `spawn`, but with the three I/O-log flags only if `with_log_args`.
    pub fn spawn_with(tag: &str, args: &[&str], with_log_args: bool) -> Self {
        let grpc_addr = format!("127.0.0.1:{}", free_tcp_port());
        let dir = std::env::temp_dir().join(format!("av-edge-board-{tag}-{}.d", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let identity = make_identity(&dir, "edge-test");
        let io_log = dir.join("io.log");
        let stderr_path = dir.join("service.stderr");
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_av-edge-board"));
        cmd.args(args).args(["--grpc-addr", &grpc_addr]);
        if with_log_args {
            cmd.arg("--io-log").arg(&io_log).arg("--signing-key").arg(&identity.key_pem).arg("--signing-cert").arg(&identity.cert_pem);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::from(stderr)).spawn().expect("spawn av-edge-board");
        Self { child, grpc_addr, stderr_path, dir, identity, io_log }
    }

    /// Read and verify this service's I/O log right now against its certificate, as an
    /// independent reader would (a fresh read of the file).
    pub fn read_io_log(&self) -> av_edge::board_log::VerifiedLog {
        let verifier = av_edge::board_log::LogVerifier::from_pem(&std::fs::read(&self.identity.cert_pem).unwrap()).unwrap();
        av_edge::board_log::read_log(&self.io_log, &verifier).unwrap_or_else(|e| panic!("the I/O log must verify: {e}"))
    }

    pub fn wait_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("av-edge-board exited early ({status}); stderr:\n{}", self.stderr());
            }
            if TcpStream::connect(&self.grpc_addr).is_ok() && self.stderr().contains("LockstepService listening") {
                return;
            }
            assert!(Instant::now() < deadline, "av-edge-board not ready after {timeout:?}; stderr:\n{}", self.stderr());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn stderr(&self) -> String {
        let mut s = String::new();
        if let Ok(mut f) = std::fs::File::open(&self.stderr_path) {
            let _ = f.read_to_string(&mut s);
        }
        s
    }

    /// Wait for the process to exit by itself; returns its status.
    pub fn wait_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "av-edge-board did not exit within {timeout:?}; stderr:\n{}", self.stderr());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Copy the stderr log to `dest` (evidence), if `AV_EDGE_BOARD_EVIDENCE_DIR` is set.
    pub fn save_evidence(&self, name: &str) {
        if let Ok(dir) = std::env::var("AV_EDGE_BOARD_EVIDENCE_DIR") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::copy(&self.stderr_path, PathBuf::from(&dir).join(name));
            if self.io_log.exists() {
                let _ = std::fs::copy(&self.io_log, PathBuf::from(&dir).join(format!("{name}.io.log")));
                let _ = std::fs::copy(&self.identity.cert_pem, PathBuf::from(&dir).join(format!("{name}.cert.pem")));
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------------------
// The client: the kernel's own LockstepService client
// ---------------------------------------------------------------------------------------

pub fn ports() -> Vec<Port> {
    vec![
        Port { name: "in".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::In as i32, schema: "signal".to_string(), timing: None, interface_class: "uart".to_string() },
        Port { name: "out".to_string(), kind: PortKind::Signal as i32, direction: PortDirection::Out as i32, schema: "signal".to_string(), timing: None, interface_class: "uart".to_string() },
    ]
}

pub const RUN_ID: &str = "hp3b-run";
pub const INSTANCE: &str = "obc";
pub const BASE_PERIOD_NS: i64 = 100_000_000;

/// The BIND request the kernel's container path builds (`materialize_container`), as the
/// `av_cdm::pb` type the guest decodes, for `parameters`.
pub fn container_path_bind(parameters: &BTreeMap<String, String>) -> CdmBindRequest {
    CdmBindRequest {
        run_id: RUN_ID.to_string(),
        instance: INSTANCE.to_string(),
        ports: ports(),
        start_tai_ns: 0,
        base_period_ns: BASE_PERIOD_NS,
        step_period_ns: BASE_PERIOD_NS,
        seed: 42,
        parameters: parameters.clone(),
    }
}

pub fn client_bind_request(parameters: &BTreeMap<String, String>) -> LockstepBindRequest {
    LockstepBindRequest {
        run_id: RUN_ID.to_string(),
        instance: INSTANCE.to_string(),
        ports: ports(),
        start_tai_ns: 0,
        base_period_ns: BASE_PERIOD_NS,
        step_period_ns: BASE_PERIOD_NS,
        seed: 42,
        parameters: parameters.clone().into_iter().collect(),
    }
}

pub fn step_request(sequence: u64, input_len: usize) -> LockstepStepRequest {
    let until = sequence as i64 * BASE_PERIOD_NS;
    LockstepStepRequest { sequence, until_tai_ns: until, inputs: vec![PortMessage { port: "in".to_string(), tai_ns: until - BASE_PERIOD_NS, payload: pattern(input_len, sequence as u8) }] }
}

/// Assert a STEP reply is what the fake guest's model must produce for `input_len` bytes.
pub fn check_step_reply(sequence: u64, input_len: usize, resp: &ClientStepResponse) {
    let input = pattern(input_len, sequence as u8);
    let sum: u64 = input.iter().map(|b| *b as u64).sum();
    let mut reversed = input;
    reversed.reverse();
    assert_eq!(resp.sequence, sequence);
    assert_eq!(resp.reached_tai_ns, sequence as i64 * BASE_PERIOD_NS);
    assert_eq!(resp.named_outputs["sum"], sum as f64, "step {sequence}: the guest's byte sum of the {input_len} input bytes");
    assert_eq!(resp.outputs.len(), 1);
    assert_eq!(resp.outputs[0].payload, reversed, "step {sequence}: the {input_len} input bytes came back reversed, byte for byte");
}

/// The full exchange: Bind, steps with inputs larger than 256 bytes, Reset, a step after it,
/// Shutdown. Returns the Bind response.
pub fn drive_full_run(addr: &str, parameters: &BTreeMap<String, String>) -> ClientBindResponse {
    let mut client = BlockingLockstepClient::connect_plaintext(addr).expect("connect to av-edge-board");
    let bind = client.bind(client_bind_request(parameters)).expect("Bind RPC");
    assert!(bind.lockstep_capable, "Bind refused: {:?}", bind.refusal_reason);
    for (sequence, len) in [(1u64, 300usize), (2, 400), (3, 1000), (4, 40)] {
        let resp = client.step(step_request(sequence, len)).unwrap_or_else(|e| panic!("Step {sequence}: {e}"));
        check_step_reply(sequence, len, &resp);
    }
    let reset = client.reset(LockstepResetRequest { sequence: 5, tai_ns: 4 * BASE_PERIOD_NS, reason: "power_cycle".to_string() }).expect("Reset RPC");
    assert_eq!(reset.sequence, 5);
    let resp = client.step(step_request(6, 700)).expect("Step after Reset");
    check_step_reply(6, 700, &resp);
    client.shutdown(LockstepShutdownRequest { run_id: RUN_ID.to_string() }).expect("Shutdown RPC");
    bind
}

// ---------------------------------------------------------------------------------------
// A fake guest on a UDP socket: one frame per datagram
// ---------------------------------------------------------------------------------------

pub struct UdpGuest {
    pub brain: Arc<Mutex<Brain>>,
    pub addr: std::net::SocketAddr,
    /// The address the service's HELLO came from (set once the handshake began).
    pub service_addr: Arc<Mutex<Option<std::net::SocketAddr>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UdpGuest {
    pub fn spawn(brain: Brain) -> Self {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let addr = socket.local_addr().unwrap();
        let brain = Arc::new(Mutex::new(brain));
        let service_addr = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (b, sa, st) = (brain.clone(), service_addr.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65_536];
            while !st.load(Ordering::Relaxed) {
                let (n, src) = match socket.recv_from(&mut buf) {
                    Ok(x) => x,
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                    Err(e) => panic!("udp guest recv: {e}"),
                };
                *sa.lock().unwrap() = Some(src);
                let mut dg = buf[..n].to_vec();
                let Some((ty, payload)) = take_frame(&mut dg) else {
                    b.lock().unwrap().framing_violations += 1;
                    continue;
                };
                if !dg.is_empty() {
                    b.lock().unwrap().framing_violations += 1;
                }
                let (replies, done) = b.lock().unwrap().on_frame(ty, &payload);
                for r in replies {
                    socket.send_to(&r, src).unwrap();
                }
                if done {
                    return;
                }
            }
        });
        Self { brain, addr, service_addr, stop, thread: Some(thread) }
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for UdpGuest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

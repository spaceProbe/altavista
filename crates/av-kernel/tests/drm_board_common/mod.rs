//! Shared stand-ins for `tests/drm_board_*.rs` (question 242, hilprep-3a): the real `av-edge-board`
//! binary (the board's edge service) as a child process in front of a **fake lockstep-local guest
//! on a loopback UDP socket**, and the demo attitude-control DRM with its `"controller"` instance
//! rebound to `BINDING_KIND_BOARD`. Nothing here involves a board, Renode or Docker: every result
//! built on it is a stand-in result.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Read;
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use av_cdm::pb::{Binding, BindingKind, BoardBinding, DesignReferenceMission, Fault, LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse, SosConfiguration, SystemDefinition};
use av_kernel::drm::{execute, hash, schema, DrmError, RunConfig, RunProducts};
use av_lockstep_shim::framing::{encode_frame, encode_hello, FrameType, PROTOCOL_VERSION};
use prost::Message;

/// Held by every test for its whole body: these tests measure wall time against a real-time
/// schedule, and (with the `gmat` feature) `execute` takes the process-wide engine lock anyway, so
/// letting two runs overlap would only make one wait inside the other's measured interval.
static SERIAL: Mutex<()> = Mutex::new(());
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

pub const CONTROLLER: &str = "controller";
pub const PERIOD_NS: i64 = 100_000_000;
pub const FAKE_BINDING_HASH_BYTE: &str = "ab";

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(repo_root().join("drms").join(name)).unwrap_or_else(|e| panic!("reading drms/{name}: {e}"))
}

pub fn load_system(stem: &str) -> SystemDefinition {
    let mut sys = schema::parse_system_definition_yaml(&read(&format!("{stem}.system.yaml"))).unwrap_or_else(|e| panic!("{stem}.system.yaml: {e}"));
    sys.hash = hash::canonical_system_hash(&sys);
    sys
}

// ------------------------------------------------------------------------------------------
// The fake guest: lockstep-local v1 over UDP, one frame per datagram
// ------------------------------------------------------------------------------------------

/// Pop one complete frame off the front of `buf`.
fn take_frame(buf: &mut Vec<u8>) -> Option<(FrameType, Vec<u8>)> {
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

/// How the fake guest misbehaves. The default answers every STEP at once with an empty
/// STEP_DONE (the controller is then silent and the truth drifts: the stand-in does not run the
/// control law).
#[derive(Clone, Default)]
pub struct GuestPlan {
    /// Sleep this long before answering the STEP with this sequence number.
    pub delay_for_step: BTreeMap<u64, Duration>,
    /// Never answer a STEP whose sequence is at least this.
    pub silent_from_step: Option<u64>,
}

/// What the fake guest saw.
#[derive(Default)]
pub struct GuestSeen {
    pub frames: Vec<FrameType>,
    pub binds: Vec<LockstepBindRequest>,
    pub step_sequences: Vec<u64>,
    pub step_until_tai_ns: Vec<i64>,
    pub resets: Vec<LockstepResetRequest>,
    /// Wall instants at which each STEP arrived.
    pub step_arrivals: Vec<Instant>,
}

pub struct FakeGuest {
    pub addr: std::net::SocketAddr,
    pub seen: Arc<Mutex<GuestSeen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeGuest {
    pub fn spawn(plan: GuestPlan) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let addr = socket.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(GuestSeen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65_536];
            while !st.load(Ordering::Relaxed) {
                let (n, src) = match socket.recv_from(&mut buf) {
                    Ok(x) => x,
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                    Err(e) => panic!("fake guest recv: {e}"),
                };
                let mut dg = buf[..n].to_vec();
                let Some((ty, payload)) = take_frame(&mut dg) else { continue };
                s.lock().unwrap().frames.push(ty);
                let reply: Option<Vec<u8>> = match ty {
                    FrameType::Hello => Some(encode_frame(FrameType::Hello, &encode_hello(PROTOCOL_VERSION))),
                    FrameType::Bind => {
                        s.lock().unwrap().binds.push(LockstepBindRequest::decode(payload.as_slice()).expect("fake guest: BIND decodes"));
                        let ack = LockstepBindResponse { lockstep_capable: true, binding_hash: FAKE_BINDING_HASH_BYTE.repeat(32), version: "fake-guest-1".to_string(), refusal_reason: String::new() };
                        Some(encode_frame(FrameType::BindAck, &ack.encode_to_vec()))
                    }
                    FrameType::Step => {
                        let req = LockstepStepRequest::decode(payload.as_slice()).expect("fake guest: STEP decodes");
                        {
                            let mut g = s.lock().unwrap();
                            g.step_sequences.push(req.sequence);
                            g.step_until_tai_ns.push(req.until_tai_ns);
                            g.step_arrivals.push(Instant::now());
                        }
                        if plan.silent_from_step.is_some_and(|from| req.sequence >= from) {
                            None
                        } else {
                            if let Some(d) = plan.delay_for_step.get(&req.sequence) {
                                std::thread::sleep(*d);
                            }
                            let resp = LockstepStepResponse { sequence: req.sequence, reached_tai_ns: req.until_tai_ns, outputs: vec![], named_outputs: BTreeMap::new() };
                            Some(encode_frame(FrameType::StepDone, &resp.encode_to_vec()))
                        }
                    }
                    FrameType::Reset => {
                        let req = LockstepResetRequest::decode(payload.as_slice()).expect("fake guest: RESET decodes");
                        let ack = LockstepResetResponse { sequence: req.sequence };
                        s.lock().unwrap().resets.push(req);
                        Some(encode_frame(FrameType::ResetAck, &ack.encode_to_vec()))
                    }
                    FrameType::Shutdown => Some(encode_frame(FrameType::ShutdownAck, &LockstepShutdownResponse {}.encode_to_vec())),
                    other => panic!("fake guest: unexpected frame {other}"),
                };
                if let Some(r) = reply {
                    socket.send_to(&r, src).unwrap();
                }
            }
        });
        Self { addr, seen, stop, thread: Some(thread) }
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

impl Drop for FakeGuest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------------------------------------
// The edge service binary
// ------------------------------------------------------------------------------------------

/// `av-edge-board` sits next to `deps/` in the target directory this test was built into; the
/// path dev-dependency in `Cargo.toml` makes Cargo build it (as for `av-lockstep-shim`).
pub fn edge_board_bin() -> PathBuf {
    let mut dir = std::env::current_exe().expect("this test's executable path").parent().expect("a parent directory").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("av-edge-board");
    assert!(candidate.is_file(), "expected the av-edge-board binary at {} (built via this crate's dev-dependency)", candidate.display());
    candidate
}

pub fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A throwaway P-384 key and self-signed certificate (PEM), for the edge service's signed I/O log.
fn write_identity(dir: &Path) -> (PathBuf, PathBuf) {
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
    name.append_entry_by_nid(Nid::COMMONNAME, "av-kernel-board-test").unwrap();
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
    let (key_pem, cert_pem) = (dir.join("edge.key.pem"), dir.join("edge.cert.pem"));
    std::fs::write(&key_pem, key.private_key_to_pem().unwrap()).unwrap();
    std::fs::write(&cert_pem, b.build().to_pem().unwrap()).unwrap();
    (key_pem, cert_pem)
}

/// The command line of the edge service. Every flag the service requires is added here and
/// nowhere else, so a flag a later task makes mandatory is a change to this function only.
fn service_argv(device: &str, edge_node_id: &str, grpc_addr: &str, dir: &Path) -> Vec<String> {
    let (key, cert) = write_identity(dir);
    let mut argv: Vec<String> = ["--port-device", device, "--edge-node-id", edge_node_id, "--grpc-addr", grpc_addr].iter().map(|s| s.to_string()).collect();
    argv.extend(["--io-log".to_string(), dir.join("io.log").display().to_string(), "--signing-key".to_string(), key.display().to_string(), "--signing-cert".to_string(), cert.display().to_string()]);
    argv
}

pub struct Service {
    child: Child,
    pub grpc_addr: String,
    pub dir: PathBuf,
    stderr_path: PathBuf,
}

impl Service {
    /// Start `av-edge-board` for `device` (it handshakes with the guest at `device` first, so
    /// the guest must already be running) and wait until it serves.
    pub fn start(tag: &str, device: &str, edge_node_id: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let grpc_addr = format!("127.0.0.1:{}", free_tcp_port());
        let stderr_path = dir.join("service.stderr");
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let child = Command::new(edge_board_bin()).args(service_argv(device, edge_node_id, &grpc_addr, &dir)).stdout(Stdio::null()).stderr(Stdio::from(stderr)).spawn().expect("spawn av-edge-board");
        let mut svc = Self { child, grpc_addr, dir, stderr_path };
        svc.wait_ready(Duration::from_secs(30));
        svc
    }

    fn wait_ready(&mut self, timeout: Duration) {
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
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn udp_device(guest: &FakeGuest) -> String {
    format!("udp://127.0.0.1:{}", guest.addr.port())
}

// ------------------------------------------------------------------------------------------
// The DRM: demo_attitude_control with "controller" rebound to BINDING_KIND_BOARD
// ------------------------------------------------------------------------------------------

pub struct Scene {
    pub drm: DesignReferenceMission,
    pub sos: SosConfiguration,
    pub systems: BTreeMap<String, SystemDefinition>,
}

impl Scene {
    /// `device` is the one device every port of the controller maps to; `edge_address` the
    /// `host:port` of the edge service; `extra_params` more `board.*` parameters on the
    /// controller's system.
    pub fn new(device: &str, edge_node_id: &str, edge_address: &str, extra_params: &[(&str, f64)], duration_s: i64, faults: Vec<Fault>) -> Self {
        let truth = load_system("demo_attitude_control_truth");
        let star = load_system("demo_attitude_control_startracker");
        let imu = load_system("demo_attitude_control_imu");
        let mut controller = load_system("demo_attitude_control_controller_board");
        for p in controller.parameters.iter_mut() {
            if p.name == "board.edge_address" {
                p.string_value = edge_address.to_string();
            }
        }
        for (name, value) in extra_params {
            controller.parameters.push(av_cdm::pb::Parameter { name: name.to_string(), value: *value, ..Default::default() });
        }
        controller.hash = hash::canonical_system_hash(&controller);

        let mut sos = schema::parse_sos_yaml(&read("demo_attitude_control.sos.yaml")).expect("the native SosConfiguration parses");
        sos.id = "attitude_control_board_sos".to_string();
        let port_devices: BTreeMap<String, String> = controller.ports.iter().map(|p| (p.name.clone(), device.to_string())).collect();
        let mut found = false;
        for inst in sos.instances.iter_mut() {
            if inst.name == CONTROLLER {
                found = true;
                inst.system_id = controller.id.clone();
                inst.step_rate_hz = 10.0;
                inst.binding = Some(Binding {
                    kind: BindingKind::Board as i32,
                    config: Some(av_cdm::pb::binding::Config::Board(BoardBinding { edge_node_id: edge_node_id.to_string(), port_devices: port_devices.clone().into_iter().collect(), power_control: String::new() })),
                });
            }
        }
        assert!(found, "demo_attitude_control.sos.yaml must declare a \"controller\" instance");
        sos.hash = hash::canonical_sos_hash(&sos);

        let mut drm = schema::parse_drm_yaml(&read("demo_attitude_control.drm.yaml")).expect("the native DRM parses");
        drm.id = "attitude_control_board_drm".to_string();
        drm.sos_configuration_id = sos.id.clone();
        drm.objectives.clear();
        drm.measures.clear();
        {
            let scenario = drm.scenario.as_mut().expect("a scenario");
            scenario.end_tai_ns = scenario.start_tai_ns + duration_s * 1_000_000_000;
            scenario.faults = faults;
            scenario.seeds.insert(CONTROLLER.to_string(), 42);
        }
        drm.hash = hash::canonical_drm_hash(&drm);

        let systems = [truth, star, imu, controller].into_iter().map(|s| (s.id.clone(), s)).collect();
        Self { drm, sos, systems }
    }

    pub fn start_tai_ns(&self) -> i64 {
        self.drm.scenario.as_ref().unwrap().start_tai_ns
    }

    pub fn run(&self) -> Result<RunProducts, DrmError> {
        #[cfg(feature = "gmat")]
        let _engine = gmat_sys::engine_lock();
        #[cfg(feature = "gmat")]
        let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
        execute(RunConfig {
            #[cfg(feature = "gmat")]
            gmat: &gmat,
            drm: &self.drm,
            sos: &self.sos,
            systems: &self.systems,
            run_id: "test-drm-board".to_string(),
            error_mode: Default::default(),
            products_dir: None,
            replay: None,
            command_source: None,
        })
    }
}

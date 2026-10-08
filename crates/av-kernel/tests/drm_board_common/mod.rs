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

use av_cdm::pb::{Binding, BindingKind, BoardBinding, DesignReferenceMission, Fault, LockstepBindRequest, LockstepBindResponse, LockstepResetRequest, LockstepResetResponse, LockstepShutdownResponse, LockstepStepRequest, LockstepStepResponse, PortMessage, SosConfiguration, SystemDefinition};
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

/// How a fake guest answers a STEP: the response's `outputs` and `named_outputs` as a function of
/// the request (hilprep-2b: non-empty, step-dependent answers, so a replay from the log is not
/// vacuous).
pub type StepAnswer = Arc<dyn Fn(&LockstepStepRequest) -> (Vec<PortMessage>, BTreeMap<String, f64>) + Send + Sync>;

/// How the fake guest misbehaves. The default answers every STEP at once with an empty
/// STEP_DONE (the controller is then silent and the truth drifts: the stand-in does not run the
/// control law).
#[derive(Clone, Default)]
pub struct GuestPlan {
    /// Sleep this long before answering the STEP with this sequence number.
    pub delay_for_step: BTreeMap<u64, Duration>,
    /// Never answer a STEP whose sequence is at least this.
    pub silent_from_step: Option<u64>,
    /// Answer every STEP with these outputs instead of empty ones (the default `None`).
    pub answer: Option<StepAnswer>,
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
                            let (outputs, named_outputs) = plan.answer.as_ref().map(|f| f(&req)).unwrap_or_default();
                            let resp = LockstepStepResponse { sequence: req.sequence, reached_tai_ns: req.until_tai_ns, outputs, named_outputs };
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

/// The command that rebuilds the binary these tests spawn.
pub const REBUILD_EDGE_BOARD: &str = "scripts/dev/cargo-slot build -p av-edge-board --bins";

/// The local crates the `av-edge-board` binary is compiled from: itself and its whole path-dependency
/// closure (`av-edge`, the pure half; `av-cdm`, the generated proto types; `av-codec` and
/// `av-dynamics`, which `av-edge` links; `av-lockstep-shim`, `PeerLink` and `ShimService`).
pub const EDGE_BOARD_SOURCE_CRATES: [&str; 6] = ["crates/av-edge-board", "crates/av-edge", "crates/av-cdm", "crates/av-codec", "crates/av-dynamics", "crates/av-lockstep-shim"];

/// The proto directory every one of those crates' `build.rs` compiles (`board.proto`, `edge.proto`,
/// `lockstep.proto`, ...): a proto change rebuilds the binary too.
pub const EDGE_BOARD_SOURCE_PROTO_DIR: &str = "proto/altavista/v1";

/// Every file whose change makes the `av-edge-board` binary out of date, for [`edge_board_bin`]'s
/// staleness check: for each crate in [`EDGE_BOARD_SOURCE_CRATES`], its `Cargo.toml`, its
/// `build.rs` if it has one, and every file under its `src/` (recursively); and every file under
/// [`EDGE_BOARD_SOURCE_PROTO_DIR`].
fn edge_board_source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for krate in EDGE_BOARD_SOURCE_CRATES {
        let root = repo_root().join(krate);
        for name in ["Cargo.toml", "build.rs"] {
            if root.join(name).is_file() {
                files.push(root.join(name));
            }
        }
        walk(&root.join("src"), &mut files);
    }
    walk(&repo_root().join(EDGE_BOARD_SOURCE_PROTO_DIR), &mut files);
    files
}

/// Panics, naming the fix, unless `binary` exists and is at least as new (mtime) as the newest of
/// `sources`. Takes the paths as arguments so the check itself is testable.
pub fn assert_binary_is_fresh(binary: &Path, sources: &[PathBuf]) {
    assert!(binary.is_file(), "the av-edge-board binary {} does not exist: build it with `{REBUILD_EDGE_BOARD}` (`cargo test -p av-kernel` does not build another package's binary)", binary.display());
    let built = std::fs::metadata(binary).and_then(|m| m.modified()).unwrap_or_else(|e| panic!("mtime of {}: {e}", binary.display()));
    let newest = sources.iter().map(|p| (std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or_else(|e| panic!("mtime of {}: {e}", p.display())), p)).max_by_key(|(t, _)| *t);
    if let Some((changed, path)) = newest {
        assert!(
            built >= changed,
            "the av-edge-board binary {} is older than {} (a source it is built from): the board tests would spawn a STALE service. Rebuild it with `{REBUILD_EDGE_BOARD}` and rerun. (Scanned: Cargo.toml, build.rs and src/** of {} and every file under {EDGE_BOARD_SOURCE_PROTO_DIR}.)",
            binary.display(),
            path.display(),
            EDGE_BOARD_SOURCE_CRATES.join(", ")
        );
    }
}

/// `av-edge-board` sits next to `deps/` in the target directory this test was built into. Cargo
/// does NOT build it for `cargo test -p av-kernel` (a dev-dependency's binary is not built for
/// another package's tests), so a stale or missing one is a visible panic, not a silent run
/// against old code: see [`assert_binary_is_fresh`] and [`REBUILD_EDGE_BOARD`].
pub fn edge_board_bin() -> PathBuf {
    let mut dir = std::env::current_exe().expect("this test's executable path").parent().expect("a parent directory").to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let candidate = dir.join("av-edge-board");
    assert_binary_is_fresh(&candidate, &edge_board_source_files());
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
fn service_argv(device: &str, edge_node_id: &str, grpc_addr: &str, dir: &Path, extra: &[String]) -> Vec<String> {
    let (key, cert) = write_identity(dir);
    let mut argv: Vec<String> = ["--port-device", device, "--edge-node-id", edge_node_id, "--grpc-addr", grpc_addr].iter().map(|s| s.to_string()).collect();
    argv.extend(["--io-log".to_string(), dir.join("io.log").display().to_string(), "--signing-key".to_string(), key.display().to_string(), "--signing-cert".to_string(), cert.display().to_string()]);
    argv.extend(extra.iter().cloned());
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
        Self::start_with(tag, device, edge_node_id, &[])
    }

    /// As [`Service::start`], with `extra` more `av-edge-board` arguments (`--power-control ...`).
    pub fn start_with(tag: &str, device: &str, edge_node_id: &str, extra: &[String]) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let grpc_addr = format!("127.0.0.1:{}", free_tcp_port());
        let stderr_path = dir.join("service.stderr");
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let child = Command::new(edge_board_bin()).args(service_argv(device, edge_node_id, &grpc_addr, &dir, extra)).stdout(Stdio::null()).stderr(Stdio::from(stderr)).spawn().expect("spawn av-edge-board");
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

    /// The edge service's process id (the parent of anything it runs).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Read and verify the edge service's board I/O log now, as an independent reader would.
    pub fn read_io_log(&self) -> av_edge::board_log::VerifiedLog {
        let verifier = av_edge::board_log::LogVerifier::from_pem(&std::fs::read(self.dir.join("edge.cert.pem")).unwrap()).unwrap();
        av_edge::board_log::read_log(&self.dir.join("io.log"), &verifier).unwrap_or_else(|e| panic!("the I/O log must verify: {e}"))
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

    /// Declare `BoardBinding.power_control` on the controller (re-hashing the configuration).
    pub fn with_power_control(mut self, power_control: &str) -> Self {
        for inst in self.sos.instances.iter_mut().filter(|i| i.name == CONTROLLER) {
            if let Some(Binding { config: Some(av_cdm::pb::binding::Config::Board(b)), .. }) = inst.binding.as_mut() {
                b.power_control = power_control.to_string();
            }
        }
        self.sos.hash = hash::canonical_sos_hash(&self.sos);
        self
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

// ------------------------------------------------------------------------------------------
// The recording power-control fake (question 242 (c), hilprep-4)
// ------------------------------------------------------------------------------------------

/// A tiny executable the test writes into its own scratch directory, for `av-edge-board
/// --power-control cmd:<path>`. Every call appends one JSON line (argv, its own pid, its parent
/// pid, the Unix time in seconds, its working directory) to `calls.jsonl` beside itself, then
/// does what `mode` says: `ok` (the default; exit 0), `fail` (stderr "relay stuck", exit 3), or
/// `sleep` (writes its pid to `sleep.pid`, sleeps 30 s). `/bin/sh` is the interpreter: the shell
/// allowlist on this host governs commands typed into a shell, not processes a binary spawns.
/// hilprep-6 reuses it.
pub struct FakeChannel {
    pub dir: PathBuf,
    pub path: PathBuf,
}

impl FakeChannel {
    pub fn create(tag: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("drm-board-channel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("power-cycle");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             d=$(dirname \"$0\")\n\
             args=\"\"\n\
             for a in \"$@\"; do args=\"$args\\\"$a\\\",\"; done\n\
             args=${args%,}\n\
             printf '{\"argv\":[%s],\"pid\":%s,\"ppid\":%s,\"time_unix\":%s,\"cwd\":\"%s\"}\\n' \"$args\" \"$$\" \"$PPID\" \"$(date +%s)\" \"$(pwd)\" >> \"$d/calls.jsonl\"\n\
             mode=ok\n\
             [ -f \"$d/mode\" ] && mode=$(cat \"$d/mode\")\n\
             case \"$mode\" in\n\
               fail) echo 'relay stuck' >&2; exit 3;;\n\
               sleep) echo $$ > \"$d/sleep.pid\"; sleep 30;;\n\
             esac\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, path }
    }

    /// `cmd:<path>`, the `--power-control` / `BoardBinding.power_control` spelling.
    pub fn uri(&self) -> String {
        format!("cmd:{}", self.path.display())
    }

    pub fn set_mode(&self, mode: &str) {
        std::fs::write(self.dir.join("mode"), mode).unwrap();
    }

    /// Every call recorded so far, parsed.
    pub fn calls(&self) -> Vec<serde_json::Value> {
        match std::fs::read_to_string(self.dir.join("calls.jsonl")) {
            Ok(text) => text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("calls.jsonl line {l:?}: {e}"))).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The argv strings of a recorded call.
    pub fn argv(call: &serde_json::Value) -> Vec<String> {
        call["argv"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
    }
}

impl Drop for FakeChannel {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

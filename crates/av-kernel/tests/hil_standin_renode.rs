//! Hilprep-6 (question 242 (b)): the **stand-in HIL run**, end to end.
//!
//! **STAND-IN: Renode 1.16.1 emulating the ZynqMP RPU, not the ZCU104. No board is involved and
//! nothing here is a board result.** What is real is the code path a board run takes: the real
//! reproducible RTEMS ELF (`a5a5fe7b...2eb5`, question 240) free-running in Renode (`start`, not
//! `RunFor`; `third_party/renode/hil_standin/renode_realtime_pty.py`), its UART1 on a host
//! pseudo-terminal, the real `av-edge-board` edge service speaking lockstep-local over that
//! pseudo-terminal at 115200 baud, the kernel binding the `"controller"` of
//! `drms/demo_attitude_control` as a `BINDING_KIND_BOARD`, real-time pacing, the signed I/O log,
//! the `power_cycle` fault through the edge service's power control (a recording fake), and the
//! replay of each run from its signed log with the board replaced.
//!
//! The run: 10 s at 10 Hz (100 scheduler ticks), twice, each against a freshly booted guest:
//!
//! - **(a) no faults.** The decoded port-traffic records are compared with the lockstep reference,
//!   the records-only hash `8e518964...8fd2` of `drm_attitude_control_renode.rs` (the full value is
//!   recorded in the round's gate logs, not in the repository, which abbreviates it: the constant
//!   below is that value, printed by that test as `port traffic records-only hash: posix=... renode=...`).
//!   Real time against lockstep is exactly the question; the test prints where the records differ if
//!   they do.
//! - **(b) a `power_cycle` `HARDWARE` fault at t = 5 s.** The recording fake is called once with the
//!   documented arguments by the edge service (not the kernel); the guest's `RESET` is in the edge
//!   log right after the `POWER_CYCLE` record; the run completes.
//!
//! Both runs print and assert their `PacingReport` (ticks, overruns and their events, the worst
//! overrun and its epoch, the histogram, work times, final lateness) and the measured Renode
//! virtual-to-wall ratio between two marks the helper takes when the run starts and ends. Overruns
//! are a measurement here, never a failure: the assertions are that the report is present and
//! self-consistent, not that it shows none.
//!
//! Then each live run is replayed from its edge log (lockstep, no Renode, no edge service) with the
//! named-exclusion helper (`board_replay::strip_replay_exclusions`); every excluded field is printed
//! with both values and everything else is compared with `==`.
//!
//! Perturbations (in the test): a STEP record's output corrupted in the edge log and re-signed with
//! the genuine key is refused under the live run's pin and, with the pin its forger recomputed,
//! replays to products that differ; a binding whose `port_devices` names another device than the
//! edge service's is a typed Bind refusal with nothing forwarded to the guest; an edge service
//! started on a device that does not exist is a typed startup failure.
//!
//! What the stand-in showed (`third_party/renode/hil_standin/README.md` has the evidence): Renode has
//! no real-time cap on virtual time, so the helper holds it at or below 1:1 (`--pacing pause`) and
//! the test asserts the measured ratio over the run; the host-to-guest bytes must reach UART1 at
//! 115200 baud in *virtual* time (`uart_rx_paced_hook.py`), because at the wall-clock rate a guest
//! running at 0.1 to 0.6 of real time dropped a STEP and stalled (3 of 3 runs with
//! `AV_HIL_STANDIN_RX_PATH=pty`); the first STEP takes the guest's whole 2000 ms output wait, which
//! at that speed makes nearly every later tick late (the pacing report says so, and is asserted to
//! be consistent, not small); and after the in-place `RESET` the controller is silent for as many
//! ticks as had passed before it (`psp_lockstep_init` zeroes the tick count, `sch_lockstep` keeps its
//! own last-seen count), so run (b)'s remaining 50 steps are answered empty, each after the 2000 ms
//! wait, which is why `board.step_timeout_ms` is 180 s here. The records-only hash comparison of
//! run (a) is unaffected by any of it.
//!
//! Opt-in: `AV_HIL_STANDIN_TESTS=1` (a visible `SKIPPED` line otherwise). `AV_RENODE_BIN` and
//! `AV_RENODE_CORE_CPU1_EXE` override the Renode binary and the ELF as in the Renode test (the ELF's
//! SHA-256 is checked either way); `AV_HIL_STANDIN_KEEP=1` keeps the scratch directory on a pass;
//! `AV_HIL_STANDIN_RX_PATH` and `AV_HIL_STANDIN_STEP_TIMEOUT_MS` are experiment knobs (below).
//! Needs `scripts/dev/cargo-slot build -p av-edge-board --bins` first (the harness fails visibly on
//! a stale binary). No Docker, no GMAT engine beyond what `execute` takes itself. Wall time,
//! measured on a loaded host (load average 11 to 18): 153 s and 184 s for the whole test (two boots
//! of 14 to 25 s, run (a) 10 to 22 s, run (b) 110 to 117 s, the replays 0.05 to 0.1 s each).
mod drm_board_common;

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use av_cdm::pb::{self, Fault, FaultTargetKind, PortDirection, PortTrafficLog};
use av_edge::board_log::{encode_frame, seal, verify_bytes, BoardIoKind, BoardIoRecord, LogSigner, LogVerifier};
use av_kernel::drm::board_replay::{strip_replay_exclusions, ReplaySource};
use av_kernel::drm::{execute_with_board_replay, BoardLogPin, BoardLogReplay, BoardReplayRefusal, DrmError, RunConfig, RunProducts};
use av_kernel::pacing::is_overrun_event;
use av_lockstep::docker::{announce_gate_skip_multi, DockerGateReason};
use drm_board_common::*;
use prost::Message;

const TEST_NAME: &str = "a_board_bound_run_against_the_real_elf_in_renode_in_real_time_replays_from_its_signed_log";
const OPT_IN_ENV: &str = "AV_HIL_STANDIN_TESTS";
const KEEP_ENV: &str = "AV_HIL_STANDIN_KEEP";
/// Experiment knobs (README.md of `third_party/renode/hil_standin`): `AV_HIL_STANDIN_RX_PATH=pty`
/// delivers host-to-guest bytes in wall-clock time through Renode's own pty terminal instead of at
/// the baud rate in virtual time (the perturbation that shows why the virtual-time path exists);
/// `AV_HIL_STANDIN_STEP_TIMEOUT_MS` overrides `board.step_timeout_ms`.
const RX_PATH_ENV: &str = "AV_HIL_STANDIN_RX_PATH";
const STEP_TIMEOUT_ENV: &str = "AV_HIL_STANDIN_STEP_TIMEOUT_MS";
const STAND_IN_LINE: &str = "STAND-IN: Renode 1.16.1 emulating the ZynqMP RPU, not the ZCU104";

/// SHA-256 of the reproducible `core-cpu1.exe` (question 240: four builds from four paths).
const ELF_SHA256: &str = "a5a5fe7b0d87714478c748cd08ca1888d68c6385657626d2cf42e36bc37a2eb5";
/// The lockstep reference: the records-only hash of the 998 decoded port-traffic records of the
/// posix container and of the Renode lockstep run (`drm_attitude_control_renode.rs`), 10 s at 10 Hz.
const LOCKSTEP_RECORDS_ONLY_HASH: &str = "8e518964f6253559d3ac23868ffdeba43fb3dde67c6af2ccfd94c425c4948fd2";
const LOCKSTEP_RECORD_COUNT: usize = 998;

const EDGE_NODE: &str = "standin-renode";
const BAUD: u32 = 115_200;
const ARC_S: i64 = 10;
const TICKS: u64 = 100;
const FAULT_AT_S: i64 = 5;
/// A real-time ratio above this over the run's window fails: the helper holds virtual time to 1:1
/// plus a 100 ms backlog allowance, which is 1.01 over 10 s; the rest is the marks' polling jitter.
const MAX_RATIO: f64 = 1.05;
/// `board.step_timeout_ms`: after the in-place RESET the guest answers every STEP with an empty
/// STEP_DONE for a while (README.md), each after the guest's own 2000 ms (virtual) wait for an
/// output, which at Renode's 0.1 to 0.6 virtual-to-wall ratio is 3 to 20 s of wall time per step.
const STEP_TIMEOUT_MS: f64 = 180_000.0;

// ------------------------------------------------------------------------------------------
// Files, gates
// ------------------------------------------------------------------------------------------

fn renode_bin() -> PathBuf {
    std::env::var_os("AV_RENODE_BIN").map(PathBuf::from).unwrap_or_else(|| repo_root().join("third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"))
}
fn renode_elf() -> PathBuf {
    std::env::var_os("AV_RENODE_CORE_CPU1_EXE").map(PathBuf::from).unwrap_or_else(|| repo_root().join("third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe"))
}
fn platform() -> PathBuf {
    repo_root().join("third_party/renode/platforms/cpus/zynqmp.repl")
}
fn helper_script() -> PathBuf {
    repo_root().join("third_party/renode/hil_standin/renode_realtime_pty.py")
}
fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python3")
}

fn gate_reasons() -> Vec<DockerGateReason> {
    let mut reasons = Vec::new();
    if std::env::var(OPT_IN_ENV).as_deref() != Ok("1") {
        reasons.push(DockerGateReason::PrerequisiteUnavailable {
            what: format!("the opt-in {OPT_IN_ENV}=1 (not set)"),
            hint: format!("set {OPT_IN_ENV}=1 to run it; two boots of Renode and two {ARC_S} s real-time runs take a few minutes of wall time"),
        });
    }
    for (path, what) in [
        (renode_bin(), "the Renode binary (fetch-renode.sh, or set AV_RENODE_BIN)"),
        (platform(), "this repository's zynqmp.repl"),
        (renode_elf(), "the reproducible RTEMS core-cpu1.exe (set AV_RENODE_CORE_CPU1_EXE)"),
        (helper_script(), "third_party/renode/hil_standin/renode_realtime_pty.py"),
        (venv_python(), "the repo-local .venv python3 (renode_bridge.py imports altavista.pb)"),
    ] {
        if !path.is_file() {
            reasons.push(DockerGateReason::RequiredFileMissing { what: what.to_string(), path: path.display().to_string() });
        }
    }
    reasons
}

fn sha256_hex(bytes: &[u8]) -> String {
    openssl::sha::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------------------------------------
// The guest: Renode (the helper) in its own process group
// ------------------------------------------------------------------------------------------

/// Signals the helper's whole process group on drop (the helper and Renode, a child of it), so no
/// exit path -- a failed assertion included -- leaves a Renode behind.
struct GroupGuard(Child);
impl Drop for GroupGuard {
    fn drop(&mut self) {
        let pgid = self.0.id();
        let _ = Command::new("kill").args(["-TERM", &format!("-{pgid}")]).output();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = Command::new("kill").args(["-KILL", &format!("-{pgid}")]).output();
        let _ = self.0.wait();
    }
}

struct Guest {
    dir: PathBuf,
    /// The slave device Renode's pty terminal created (`/dev/ttysNNN`): the edge service accepts
    /// only paths under `/dev/`, so the symlink Renode also makes is not what it is given.
    slave: String,
    uart0_log: PathBuf,
    stats_path: PathBuf,
    boot_wall: Duration,
    guard: GroupGuard,
}

impl Guest {
    fn boot(dir: &Path, monitor_port: u16) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let (pty, uart0_log, ready_path, stats_path) = (dir.join("uart1.pty"), dir.join("uart0.log"), dir.join("ready.json"), dir.join("stats.json"));
        let t0 = Instant::now();
        let child = Command::new(venv_python())
            .arg(helper_script())
            .arg("--renode-bin")
            .arg(renode_bin())
            .arg("--platform")
            .arg(platform())
            .arg("--elf")
            .arg(renode_elf())
            .arg("--workdir")
            .arg(dir)
            .arg("--uart0-log")
            .arg(&uart0_log)
            .arg("--pty-path")
            .arg(&pty)
            .args(["--monitor-port", &monitor_port.to_string()])
            .arg("--ready-file")
            .arg(&ready_path)
            .arg("--stats-file")
            .arg(&stats_path)
            .arg("--tap-file")
            .arg(dir.join("uart1_tap.jsonl"))
            .arg("--stop-file")
            .arg(dir.join("stop"))
            .args(["--pacing", "pause", "--quantum-us", "1000"])
            .args(["--rx-path", &std::env::var(RX_PATH_ENV).unwrap_or_else(|_| "paced".to_string())])
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(dir.join("helper.stdout")).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(dir.join("helper.stderr")).unwrap()))
            .process_group(0)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn the Renode helper: {e}"));
        let mut guard = GroupGuard(child);
        let deadline = Instant::now() + Duration::from_secs(180);
        while !ready_path.is_file() {
            if let Some(status) = guard.0.try_wait().unwrap() {
                panic!("the Renode helper exited early ({status}); stderr:\n{}\nstdout:\n{}", read(&dir.join("helper.stderr")), read(&dir.join("helper.stdout")));
            }
            assert!(Instant::now() < deadline, "the guest was not ready within 180 s; helper stdout:\n{}\nstderr:\n{}", read(&dir.join("helper.stdout")), read(&dir.join("helper.stderr")));
            std::thread::sleep(Duration::from_millis(100));
        }
        let slave = serde_json::from_str::<serde_json::Value>(&read(&ready_path)).unwrap()["pty_slave"].as_str().unwrap().to_string();
        assert!(slave.starts_with("/dev/"), "Renode's pty terminal is a /dev/ device: {slave}");
        let boot_wall = t0.elapsed();
        println!("GUEST READY after {boot_wall:.2?}: {}", read(&dir.join("helper.stdout")).trim());
        Self { dir: dir.to_path_buf(), slave, uart0_log, stats_path, boot_wall, guard }
    }

    /// The device spec the binding and the edge service both name: the pty and the baud.
    fn device(&self) -> String {
        format!("{}@{BAUD}", self.slave)
    }

    /// Drop a mark file; the helper records the virtual and wall time at which it saw it.
    fn mark(&self, name: &str) {
        std::fs::write(self.dir.join(format!("mark.{name}")), b"").unwrap();
    }

    /// Stop the helper (it writes its final statistics and quits Renode) and read them.
    fn stop(self) -> serde_json::Value {
        std::fs::write(self.dir.join("stop"), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut guard = self.guard;
        while guard.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the helper did not stop");
            std::thread::sleep(Duration::from_millis(100));
        }
        serde_json::from_str(&read(&self.stats_path)).unwrap()
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// The measured virtual-to-wall ratio of Renode between the two marks, from the helper's own
/// clock and Renode's own `Elapsed Virtual Time`.
struct Ratio {
    wall_s: f64,
    virtual_s: f64,
    ratio: f64,
    stats: serde_json::Value,
}

fn ratio_between_marks(stats: serde_json::Value) -> Ratio {
    let m = |name: &str, key: &str| stats["marks"][name][key].as_f64().unwrap_or_else(|| panic!("mark {name} missing in the helper's statistics: {stats}"));
    let (wall_s, virtual_s) = (m("run_end", "wall_s") - m("run_start", "wall_s"), m("run_end", "virtual_s") - m("run_start", "virtual_s"));
    Ratio { wall_s, virtual_s, ratio: virtual_s / wall_s, stats }
}

// ------------------------------------------------------------------------------------------
// Running and replaying a scene
// ------------------------------------------------------------------------------------------

fn run_scene(scene: &Scene, run_id: &str, products_dir: Option<PathBuf>, boards: &[BoardLogReplay]) -> Result<RunProducts, DrmError> {
    #[cfg(feature = "gmat")]
    let _engine = gmat_sys::engine_lock();
    #[cfg(feature = "gmat")]
    let gmat = gmat_sys::Gmat::setup(&gmat_sys::Gmat::default_startup_file()).expect("GMAT setup (default-feature build only)");
    execute_with_board_replay(
        RunConfig {
            #[cfg(feature = "gmat")]
            gmat: &gmat,
            drm: &scene.drm,
            sos: &scene.sos,
            systems: &scene.systems,
            run_id: run_id.to_string(),
            error_mode: Default::default(),
            products_dir,
            replay: None,
            command_source: None,
        },
        boards,
    )
}

struct Live {
    name: &'static str,
    dir: PathBuf,
    scene: Scene,
    run_id: String,
    products: RunProducts,
    wire: pb::RunProducts,
    products_dir: PathBuf,
    sidecar_copy: PathBuf,
    log_path: PathBuf,
    cert_pem: Vec<u8>,
    key_pem: Vec<u8>,
    channel_calls: Vec<serde_json::Value>,
    service_pid: u32,
    service_stderr: String,
    ratio: Ratio,
    run_wall: Duration,
    boot_wall: Duration,
}

impl Live {
    fn pin(&self) -> BoardLogPin {
        BoardLogPin::from_trajectories(&self.products.trajectories, CONTROLLER).expect("the live run pinned its board log")
    }
    fn replay_of(&self, log: &Path, expected: BoardLogPin) -> BoardLogReplay {
        BoardLogReplay { instance: CONTROLLER.to_string(), log_path: log.to_path_buf(), certificate_pem: self.cert_pem.clone(), expected }
    }
    fn replay(&self, log: &Path, expected: BoardLogPin) -> Result<RunProducts, DrmError> {
        run_scene(&self.scene, &self.run_id, Some(self.products_dir.clone()), &[self.replay_of(log, expected)])
    }
}

fn live_run(root: &Path, name: &'static str, monitor_port: u16, fault: bool) -> Live {
    println!("---- LIVE RUN ({name}) ----");
    let dir = root.join(name);
    let guest = Guest::boot(&dir.join("guest"), monitor_port);
    let channel = FakeChannel::create(name);
    let device = guest.device();
    let service = Service::start_with(name, &device, EDGE_NODE, &["--power-control".to_string(), channel.uri()]);
    println!("EDGE SERVICE up on {} for {device} (handshake with the guest done)", service.grpc_addr);

    let faults = if fault {
        vec![Fault { id: "pc1".to_string(), instance: CONTROLLER.to_string(), target_kind: FaultTargetKind::Hardware as i32, kind: "power_cycle".to_string(), tai_ns: 0, ..Default::default() }]
    } else {
        vec![]
    };
    let mut scene = Scene::new(&device, EDGE_NODE, &service.grpc_addr, &[("board.step_timeout_ms", std::env::var(STEP_TIMEOUT_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(STEP_TIMEOUT_MS))], ARC_S, faults).with_power_control(&channel.uri());
    if fault {
        let at = scene.start_tai_ns() + FAULT_AT_S * 1_000_000_000;
        scene.drm.scenario.as_mut().unwrap().faults[0].tai_ns = at;
        scene.drm.hash = av_kernel::drm::hash::canonical_drm_hash(&scene.drm);
    }

    // The perturbation on the Bind side, on the live service before the real Bind: a binding whose
    // port_devices names another device is refused by the edge service, nothing is forwarded to the
    // guest, and the service stays up for the real Bind.
    if !fault {
        let other = format!("{}.other@{BAUD}", guest.slave);
        let wrong = Scene::new(&other, EDGE_NODE, &service.grpc_addr, &[], 1, vec![]);
        let err = wrong.run().expect_err("a mismatched port_devices is refused at Bind");
        println!("PERTURBATION (binding names {other}, the service has {device}): {err}");
        match &err {
            DrmError::BoardRefused { instance, reason } => {
                assert_eq!(instance, CONTROLLER);
                assert!(reason.contains("port device differs"), "the service's own reason: {reason}");
            }
            other => panic!("expected BoardRefused, got {other:?}"),
        }
        assert!(service.stderr().contains("Bind refused, nothing forwarded to the board"), "{}", service.stderr());
    }

    let run_id = format!("hil-standin-{name}");
    let products_dir = dir.join("products");
    guest.mark("run_start");
    let t0 = Instant::now();
    let result = run_scene(&scene, &run_id, Some(products_dir.clone()), &[]);
    let run_wall = t0.elapsed();
    guest.mark("run_end");
    let products = match result {
        Ok(p) => p,
        Err(e) => {
            // Evidence first: the service's log and the helper's relay counters survive the panic.
            let _ = std::fs::copy(service.dir.join("io.log"), dir.join("failed.io.log"));
            let _ = std::fs::write(dir.join("failed.service.stderr"), service.stderr());
            let _ = std::fs::copy(service.dir.join("edge.cert.pem"), dir.join("failed.cert.pem"));
            guest.mark("run_end");
            std::fs::write(guest.dir.join("probe.request"), b"").unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            let probe = read(&guest.dir.join("probe.0.json"));
            let relay = serde_json::from_str::<serde_json::Value>(&read(&guest.stats_path)).map(|v| v["relay"].to_string()).unwrap_or_default();
            panic!("the live run ({name}): {e}\nedge service stderr:\n{}\nhelper relay counters: {relay}\nUART1 probe: {probe}\nguest console tail:\n{}", service.stderr(), tail(&read(&guest.uart0_log), 3000))
        }
    };
    let boot_wall = guest.boot_wall;
    // The marks are polled by the helper every few tens of ms; give it a moment to see the second.
    std::thread::sleep(Duration::from_millis(400));
    std::fs::copy(&guest.uart0_log, dir.join("uart0.log")).unwrap();
    let ratio = ratio_between_marks(guest.stop());

    let log_path = dir.join("io.log");
    std::fs::copy(service.dir.join("io.log"), &log_path).unwrap();
    let cert_pem = std::fs::read(service.dir.join("edge.cert.pem")).unwrap();
    let key_pem = std::fs::read(service.dir.join("edge.key.pem")).unwrap();
    std::fs::write(dir.join("edge.cert.pem"), &cert_pem).unwrap();
    let sidecar_copy = dir.join("live_port_traffic.pb");
    std::fs::copy(products_dir.join("port_traffic.pb"), &sidecar_copy).unwrap();
    let wire = products.to_proto();
    Live {
        name,
        dir,
        scene,
        run_id,
        products,
        wire,
        products_dir,
        sidecar_copy,
        log_path,
        cert_pem,
        key_pem,
        channel_calls: channel.calls(),
        service_pid: service.pid(),
        service_stderr: service.stderr(),
        ratio,
        run_wall,
        boot_wall,
    }
}

fn tail(s: &str, n: usize) -> String {
    s[s.len().saturating_sub(n)..].to_string()
}

// ------------------------------------------------------------------------------------------
// What a run measured
// ------------------------------------------------------------------------------------------

fn report_and_check_pacing(live: &Live) {
    let p = live.products.pacing.as_ref().expect("a board-bound run carries a PacingReport");
    let events: Vec<&pb::Event> = live.products.events.iter().filter(|e| is_overrun_event(e)).collect();
    println!("PACING REPORT ({}): {p:#?}", live.name);
    println!(
        "PACING SUMMARY ({}): ticks {} | overruns {} | worst {:.3} ms at epoch {} (t+{:.1} s) | total {:.3} ms | work mean {:.3} ms worst {:.3} ms | final lateness {:.3} ms | histogram (upper edges ns {:?}) {:?} | run wall {:.3} s",
        live.name,
        p.ticks_paced,
        p.overrun_count,
        p.worst_overrun_ns as f64 / 1e6,
        p.worst_overrun_tai_ns,
        if p.worst_overrun_tai_ns > 0 { (p.worst_overrun_tai_ns - p.sim_start_tai_ns) as f64 / 1e9 } else { 0.0 },
        p.total_overrun_ns as f64 / 1e6,
        p.mean_work_ns as f64 / 1e6,
        p.worst_work_ns as f64 / 1e6,
        p.final_lateness_ns as f64 / 1e6,
        p.overrun_histogram_upper_edges_ns,
        p.overrun_histogram,
        live.run_wall.as_secs_f64()
    );
    for e in events.iter().take(8) {
        println!("  overrun event at epoch {}: {:?}", e.tai_ns, e.values);
    }
    // Self-consistency: a measurement is present and adds up, whatever it shows.
    assert_eq!(p.mode, pb::PacingMode::RealTime as i32);
    assert_eq!(p.forcing_instances, vec![CONTROLLER.to_string()]);
    assert_eq!(p.base_period_ns, PERIOD_NS);
    assert_eq!(p.ticks_paced, TICKS, "a {ARC_S} s arc at 10 Hz is {TICKS} scheduler ticks");
    assert_eq!(p.overrun_count as usize, events.len(), "every overrun is one event");
    assert_eq!(p.overrun_histogram.len(), p.overrun_histogram_upper_edges_ns.len() + 1);
    assert_eq!(p.overrun_histogram.iter().sum::<u64>(), p.overrun_count, "every overrun is in one histogram bucket");
    assert!(p.worst_overrun_ns >= 0 && p.total_overrun_ns >= p.worst_overrun_ns as u64, "worst <= total");
    if p.overrun_count == 0 {
        assert_eq!((p.worst_overrun_ns, p.worst_overrun_tai_ns, p.total_overrun_ns), (0, 0, 0));
    } else {
        assert!(p.worst_overrun_ns > 0 && p.total_overrun_ns >= p.overrun_count, "overruns are strictly positive");
        assert!(events.iter().any(|e| e.tai_ns == p.worst_overrun_tai_ns), "the worst overrun's epoch is one of the overrun events'");
        assert!(p.worst_overrun_tai_ns > p.sim_start_tai_ns && p.worst_overrun_tai_ns <= p.sim_start_tai_ns + ARC_S * 1_000_000_000);
    }
    assert!(p.worst_work_ns >= p.mean_work_ns && p.mean_work_ns > 0 && p.total_work_ns as i64 >= p.worst_work_ns, "work times are consistent");
    assert!(p.final_lateness_ns >= 0);
    let r = &live.ratio;
    println!(
        "RENODE VIRTUAL-TO-WALL RATIO ({}): {:.4} = {:.3} virtual s / {:.3} wall s between the run's start and end marks | helper pacing {} (pauses {}, paused {:.2} s, max virtual ahead of the 1:1 reference line {:.1} ms, of the wall clock since start {:.1} ms) | since the guest was ready: {:.4}",
        live.name,
        r.ratio,
        r.virtual_s,
        r.wall_s,
        r.stats["pacing"],
        r.stats["pauses"],
        r.stats["paused_wall_s"].as_f64().unwrap_or(0.0),
        r.stats["max_virtual_ahead_of_reference_ms"].as_f64().unwrap_or(f64::NAN),
        r.stats["max_virtual_ahead_of_wall_ms"].as_f64().unwrap_or(f64::NAN),
        r.stats["since_ready"]["ratio"].as_f64().unwrap_or(f64::NAN)
    );
    assert!(r.wall_s > 5.0 && r.virtual_s > 0.0, "the marks bracket the run");
    assert!(r.ratio > 0.0 && r.ratio <= MAX_RATIO, "Renode's virtual time stayed at or below 1:1 over the run: {}", r.ratio);
}

fn sidecar_records(path: &Path) -> (PortTrafficLog, Vec<u8>) {
    let bytes = std::fs::read(path).unwrap();
    (PortTrafficLog::decode(bytes.as_slice()).unwrap(), bytes)
}

fn records_only_hash(log: &PortTrafficLog) -> String {
    sha256_hex(&PortTrafficLog { records: log.records.clone(), ..Default::default() }.encode_to_vec())
}

fn per_port(log: &PortTrafficLog) -> BTreeMap<(String, String, i32), (usize, usize)> {
    let mut m: BTreeMap<(String, String, i32), (usize, usize)> = BTreeMap::new();
    for r in &log.records {
        let e = m.entry((r.instance.clone(), r.port.clone(), r.direction)).or_default();
        e.0 += 1;
        e.1 += r.payload.len();
    }
    m
}

// ------------------------------------------------------------------------------------------
// Replay and comparison (the 2b helpers, as drm_board_replay.rs uses them)
// ------------------------------------------------------------------------------------------

fn assert_same_products(what: &str, live: &pb::RunProducts, replay: &pb::RunProducts) {
    assert_eq!(live.trajectories, replay.trajectories, "{what}: trajectories");
    assert_eq!(live.events, replay.events, "{what}: events");
    assert_eq!(live.measurements, replay.measurements, "{what}: measurements");
    assert_eq!(live.scores, replay.scores, "{what}: scores");
    assert_eq!(live.provenance, replay.provenance, "{what}: provenance");
    assert_eq!(live.frames, replay.frames, "{what}: frames");
    assert_eq!(live.dropped_in_flight_messages, replay.dropped_in_flight_messages, "{what}: dropped");
    assert_eq!(live.port_traffic_hash, replay.port_traffic_hash, "{what}: port_traffic_hash");
    assert_eq!(live, replay, "{what}: the whole RunProducts");
}

/// Print every excluded field with both values.
fn print_excluded(what: &str, live: &pb::RunProducts, replay: &pb::RunProducts) {
    println!("EXCLUDED FIELDS ({what}):");
    let pacing = |p: &pb::RunProducts| p.pacing.as_ref().map(|p| (p.mode, p.ticks_paced, p.overrun_count, p.worst_overrun_ns));
    println!("  RunProducts.pacing (mode, ticks_paced, overrun_count, worst_overrun_ns): live {:?} | replay {:?}", pacing(live), pacing(replay));
    let overruns = |p: &pb::RunProducts| p.events.iter().filter(|e| e.id.starts_with("marker:pacing:overrun:")).count();
    println!("  overrun events (count): live {} | replay {}", overruns(live), overruns(replay));
    let outcome = |p: &pb::RunProducts| p.events.iter().find(|e| e.id.starts_with("marker:power_cycle:")).map(|e| (e.id.clone(), e.detail.clone(), e.values.get("duration_ns").copied()));
    println!("  power-cycle outcome event (id, detail, values[duration_ns]): live {:?} | replay {:?}", outcome(live), outcome(replay));
    println!("  (a replay from the edge log reproduces the board segment's dynamics_* and the binding-hash provenance exactly: not excluded)");
}

fn compare_replay(live: &Live, what: &str) -> RunProducts {
    let pin = live.pin();
    let guard_before = std::fs::metadata(&live.log_path).unwrap().len();
    let t0 = Instant::now();
    let replayed = live.replay(&live.log_path, pin).unwrap_or_else(|e| panic!("the replay of run {} from its edge log: {e}", live.name));
    println!("REPLAY ({}) from the edge log: wall {:.3} s (live run {:.3} s); the log is {guard_before} bytes and untouched", what, t0.elapsed().as_secs_f64(), live.run_wall.as_secs_f64());
    assert!(replayed.pacing.is_none() && !replayed.events.iter().any(is_overrun_event), "a replay is lockstep: no PacingReport, no overrun events");
    assert_eq!(std::fs::metadata(&live.log_path).unwrap().len(), guard_before);
    let replay_wire = replayed.to_proto();
    print_excluded(what, &live.wire, &replay_wire);
    let mut a = live.wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    let mut b = replay_wire.clone();
    strip_replay_exclusions(&mut b, CONTROLLER, ReplaySource::EdgeLog);
    assert_same_products(what, &a, &b);
    assert_eq!(live.wire.trajectories[CONTROLLER], replay_wire.trajectories[CONTROLLER], "{what}: the board trajectory, segments and provenance (binding hashes and the pin) are identical");
    assert_eq!(live.wire.events.iter().find(|e| e.id == "fault:pc1"), replay_wire.events.iter().find(|e| e.id == "fault:pc1"));
    // The replay's own sidecar (written over the live one at the same path) is byte-identical.
    assert_eq!(std::fs::read(live.products_dir.join("port_traffic.pb")).unwrap(), std::fs::read(&live.sidecar_copy).unwrap(), "{what}: the replay's port_traffic.pb is byte-identical to the live run's");
    println!("REPLAY ({what}) == LIVE after the named exclusions: trajectories, events, scores, provenance, frames, port_traffic_hash {} and the whole RunProducts", live.products.port_traffic_hash);
    replayed
}

// ------------------------------------------------------------------------------------------
// Log surgery (the test holds the signing key, as the edge node does)
// ------------------------------------------------------------------------------------------

fn reseal(live: &Live, mut records: Vec<BoardIoRecord>, from: usize) -> Vec<u8> {
    let signer = LogSigner::from_pem(&live.key_pem, &live.cert_pem).unwrap();
    for i in from..records.len() {
        records[i].prev_hash = if i == 0 { av_edge::hash::GENESIS.to_vec() } else { records[i - 1].record_hash.clone() };
        seal(&mut records[i], &signer).unwrap();
    }
    records.iter().flat_map(|r| encode_frame(r).unwrap()).collect()
}

fn corrupt_one_step_output_and_replay(live: &Live) {
    println!("---- PERTURBATION: one STEP record's output corrupted in the edge log, re-signed ({}) ----", live.name);
    let verifier = LogVerifier::from_pem(&live.cert_pem).unwrap();
    let log = verify_bytes(&std::fs::read(&live.log_path).unwrap(), &verifier).expect("the live log verifies");
    let k = log.records.iter().enumerate().filter(|(_, r)| r.kind == BoardIoKind::Step as i32 && !r.outputs.is_empty()).nth(20).map(|(i, _)| i).expect("a STEP record with an output");
    let mut forged = log.records.clone();
    forged[k].outputs[0].payload[8] ^= 0x40;
    let forged_bytes = reseal(live, forged, k);
    let forged_path = live.dir.join("forged.log");
    std::fs::write(&forged_path, &forged_bytes).unwrap();
    let forged_log = verify_bytes(&forged_bytes, &verifier).expect("the forged log verifies: genuinely re-signed");
    // Under the live run's pin the forged log is refused before anything binds.
    let err = live.replay(&forged_path, live.pin()).expect_err("the forged log is not the one the run pinned");
    println!("REFUSED (forged log, the live run's pin): {err}");
    assert!(matches!(&err, DrmError::BoardReplay { refusal, .. } if matches!(**refusal, BoardReplayRefusal::ChainHeadMismatch { .. })), "{err:?}");
    // With the pin its forger recomputed it replays, and the comparison with the live run fails.
    let forged_pin = BoardLogPin::of_log(&forged_log).unwrap();
    assert_ne!(forged_pin, live.pin());
    let replayed = live.replay(&forged_path, forged_pin).unwrap_or_else(|e| panic!("the forged log replays with its own pin: {e}"));
    let mut a = live.wire.clone();
    strip_replay_exclusions(&mut a, CONTROLLER, ReplaySource::EdgeLog);
    let mut b = replayed.to_proto();
    strip_replay_exclusions(&mut b, CONTROLLER, ReplaySource::EdgeLog);
    let mut differing = Vec::new();
    for (name, same) in [("trajectories", a.trajectories == b.trajectories), ("events", a.events == b.events), ("scores", a.scores == b.scores), ("provenance", a.provenance == b.provenance), ("port_traffic_hash", a.port_traffic_hash == b.port_traffic_hash)] {
        if !same {
            differing.push(name);
        }
    }
    println!("PERTURBED REPLAY (STEP record {} of the log, output byte 8 xor 0x40, re-signed, its own pin): comparison with the live run FAILS in {differing:?}; port_traffic_hash live {} | replay {}", k + 1, a.port_traffic_hash, b.port_traffic_hash);
    assert!(a != b, "the perturbed replay's products must differ from the live run's");
    assert!(!differing.is_empty());
    assert_ne!(live.wire.trajectories["attitude"], replayed.to_proto().trajectories["attitude"], "the plant's trajectory depends on the logged output");
    // The genuine log still replays to the live products afterwards.
    let again = live.replay(&live.log_path, live.pin()).expect("the genuine log replays");
    let mut c = again.to_proto();
    strip_replay_exclusions(&mut c, CONTROLLER, ReplaySource::EdgeLog);
    assert_same_products("the genuine log after the perturbation", &a, &c);
}

// ------------------------------------------------------------------------------------------
// The test
// ------------------------------------------------------------------------------------------

fn check_elf() {
    let elf = renode_elf();
    let sha = sha256_hex(&std::fs::read(&elf).unwrap());
    println!("ELF {} SHA-256 {sha} (expected {ELF_SHA256})", elf.display());
    assert_eq!(sha, ELF_SHA256, "the guest must be the reproducible ELF of question 240 (set AV_RENODE_CORE_CPU1_EXE to a copy of it)");
}

#[test]
fn a_board_bound_run_against_the_real_elf_in_renode_in_real_time_replays_from_its_signed_log() {
    let reasons = gate_reasons();
    if !reasons.is_empty() {
        let line = announce_gate_skip_multi(TEST_NAME, &reasons);
        assert!(line.starts_with("SKIPPED ") && line.contains(TEST_NAME), "the gate helper must announce a visible skip line naming this test: {line:?}");
        return;
    }
    let _serial = serial();
    let total = Instant::now();
    println!("{STAND_IN_LINE}");
    check_elf();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("hil-standin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    println!("SCRATCH {}", root.display());

    // ---- The wrong device, a startup refusal: no such tty.
    {
        let missing = format!("/dev/ttys9999-no-such-tty@{BAUD}");
        // The harness reports a service that exits early by panicking with its stderr; catch that
        // here (and keep the default hook from printing it as if the test had failed).
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(|| Service::start("wrong-pty", &missing, EDGE_NODE));
        std::panic::set_hook(hook);
        let msg = match caught {
            Ok(_) => panic!("an edge service on a device that does not exist must not start"),
            Err(p) => p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default(),
        };
        println!("PERTURBATION (edge service started on a device that does not exist): {}", msg.lines().take(6).collect::<Vec<_>>().join(" | "));
        assert!(msg.contains("exited early") && msg.to_lowercase().contains("no-such-tty"), "a typed startup failure naming the device: {msg}");
    }

    // ---- (a) without faults: the lockstep reference.
    let a = live_run(&root, "a", 15_431, false);
    println!("STAND-IN (a): {} ticks, {} scheduler steps in the edge log, boot {:.2?}, run {:.2?}", TICKS, a.products.pacing.as_ref().unwrap().ticks_paced, a.boot_wall, a.run_wall);
    report_and_check_pacing(&a);
    assert!(a.channel_calls.is_empty(), "no fault, no power cycle");
    let verifier = LogVerifier::from_pem(&a.cert_pem).unwrap();
    let log_a = verify_bytes(&std::fs::read(&a.log_path).unwrap(), &verifier).expect("(a) the edge log verifies");
    let kinds_a: Vec<BoardIoKind> = log_a.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    assert_eq!((kinds_a.first(), kinds_a.last()), (Some(&BoardIoKind::Bind), Some(&BoardIoKind::Shutdown)));
    assert_eq!(kinds_a.iter().filter(|k| **k == BoardIoKind::Step).count() as u64, TICKS);
    assert!(!kinds_a.contains(&BoardIoKind::PowerCycle) && !kinds_a.contains(&BoardIoKind::Reset));
    let outputs: usize = log_a.records.iter().map(|r| r.outputs.len()).sum();
    println!("EDGE LOG (a): {} records ({} STEP), {outputs} output frames from the real ELF, chain head {}", log_a.records.len(), TICKS, a.pin().chain_head);
    assert!(outputs > 50, "the guest's control law answered with wheel torques: {outputs}");

    let (sidecar_a, bytes_a) = sidecar_records(&a.products_dir.join("port_traffic.pb"));
    assert_eq!(a.products.port_traffic_hash, sha256_hex(&bytes_a));
    let hash_a = records_only_hash(&sidecar_a);
    println!("PORT TRAFFIC (a): {} records, records-only hash {hash_a}; lockstep reference {LOCKSTEP_RECORDS_ONLY_HASH} ({LOCKSTEP_RECORD_COUNT} records)", sidecar_a.records.len());
    for ((instance, port, direction), (n, bytes)) in per_port(&sidecar_a) {
        println!("  port traffic {instance}.{port} dir={direction}: {n} records, {bytes} payload bytes");
    }
    if hash_a != LOCKSTEP_RECORDS_ONLY_HASH {
        std::fs::write(root.join("a_records.debug"), format!("{:#?}", sidecar_a.records)).ok();
        let controller_out = sidecar_a.records.iter().filter(|r| r.instance == CONTROLLER && r.direction == PortDirection::Out as i32).count();
        println!("RECORDS DIFFER from the lockstep reference: {} records against {LOCKSTEP_RECORD_COUNT}; controller OUT records {controller_out}; dump at {}", sidecar_a.records.len(), root.join("a_records.debug").display());
    }
    assert_eq!(sidecar_a.records.len(), LOCKSTEP_RECORD_COUNT, "record count against the lockstep reference");
    assert_eq!(hash_a, LOCKSTEP_RECORDS_ONLY_HASH, "real time against lockstep: the decoded port-traffic records must hash to the lockstep reference");
    println!("RECORDS-ONLY HASH (a) == the Renode lockstep reference {LOCKSTEP_RECORDS_ONLY_HASH}");

    compare_replay(&a, "replay of (a) from its edge log");
    corrupt_one_step_output_and_replay(&a);

    // ---- (b) a power_cycle HARDWARE fault at t = 5 s.
    let b = live_run(&root, "b", 15_432, true);
    println!("STAND-IN (b): boot {:.2?}, run {:.2?}", b.boot_wall, b.run_wall);
    report_and_check_pacing(&b);
    assert_eq!(b.channel_calls.len(), 1, "the power control was called exactly once: {:?}", b.channel_calls);
    let fault_tai = b.scene.start_tai_ns() + FAULT_AT_S * 1_000_000_000;
    let argv = FakeChannel::argv(&b.channel_calls[0]);
    println!("POWER CONTROL CALL {}", b.channel_calls[0]);
    assert_eq!(argv, ["power-cycle", "--edge-node-id", EDGE_NODE, "--instance", CONTROLLER, "--fault-id", "pc1", "--tai-ns", &fault_tai.to_string()]);
    assert_eq!(b.channel_calls[0]["ppid"].as_u64().unwrap() as u32, b.service_pid, "the channel's parent is the edge service, not the kernel");
    assert_ne!(b.service_pid, std::process::id());
    assert!(b.service_stderr.contains("PowerCycle Performed"), "{}", b.service_stderr);
    let verifier_b = LogVerifier::from_pem(&b.cert_pem).unwrap();
    let log_b = verify_bytes(&std::fs::read(&b.log_path).unwrap(), &verifier_b).expect("(b) the edge log verifies");
    let kinds_b: Vec<BoardIoKind> = log_b.records.iter().map(|r| BoardIoKind::try_from(r.kind).unwrap()).collect();
    let at = kinds_b.iter().position(|k| *k == BoardIoKind::PowerCycle).expect("a POWER_CYCLE record in the edge log");
    assert_eq!(kinds_b.iter().filter(|k| **k == BoardIoKind::PowerCycle).count(), 1);
    assert_eq!(kinds_b[at + 1], BoardIoKind::Reset, "the guest's RESET follows the power cycle in the edge log");
    assert_eq!(kinds_b[at - 1], BoardIoKind::Step);
    assert_eq!(log_b.records[at + 1].reset_reason, "fault:pc1");
    assert_eq!(log_b.records[at + 1].reset_tai_ns, fault_tai);
    assert_eq!(kinds_b.iter().filter(|k| **k == BoardIoKind::Step).count() as u64, TICKS, "the run completed all {TICKS} steps around the RESET");
    assert_eq!(kinds_b.last(), Some(&BoardIoKind::Shutdown));
    println!("EDGE LOG (b): records around the fault {:?}; RESET reason {:?} at epoch {}; the run completed {TICKS} steps", &kinds_b[at - 1..=at + 2], log_b.records[at + 1].reset_reason, log_b.records[at + 1].reset_tai_ns);
    let outcome = b.products.events.iter().find(|e| av_kernel::drm::power::is_power_cycle_event(e)).expect("the power-cycle outcome event");
    assert_eq!((outcome.id.as_str(), outcome.tai_ns), ("marker:power_cycle:pc1", fault_tai));
    assert_eq!(outcome.values["performed"], 1.0);
    compare_replay(&b, "replay of (b) from its edge log");
    assert_eq!(b.channel_calls.len(), 1);

    println!("TOTAL wall time {:.1} s", total.elapsed().as_secs_f64());
    println!("PASS: the stand-in HIL run (Renode, not the ZCU104): real ELF, real time, pty, board binding, pacing, signed log, replay");
    if std::env::var(KEEP_ENV).as_deref() == Ok("1") {
        println!("{KEEP_ENV}=1: scratch kept at {}", root.display());
    } else {
        let _ = std::fs::remove_dir_all(&root);
    }
}

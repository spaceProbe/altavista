//! The failure modes of a `BINDING_KIND_BOARD` instance at run time (question 242, hilprep-3a),
//! **against stand-ins only** (the real `av-edge-board` in front of a fake UDP guest, or a bare
//! TCP listener): a Bind-time mismatch refused with the edge service's reason and nothing
//! forwarded; a guest that stops answering and a service that never replies are typed timeouts,
//! not hangs; an unreachable edge service, an unknown seed key and a HARDWARE fault are typed
//! refusals. No board anywhere.
mod drm_board_common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use av_cdm::pb::{Fault, FaultTargetKind};
use av_kernel::drm::binding::ContainerError;
use av_kernel::drm::DrmError;
use drm_board_common::*;

/// A `port_devices` entry naming a different UDP port than the service was started with is
/// refused at Bind, with the edge service's reason, before anything is forwarded to the guest.
#[test]
fn a_port_devices_mismatch_is_refused_at_bind_with_the_services_reason_and_nothing_reaches_the_guest() {
    let _serial = serial();
    let guest = FakeGuest::spawn(GuestPlan::default());
    let service = Service::start("mismatch-device", &udp_device(&guest), "zcu104-test");
    let other_port = guest.addr.port().wrapping_add(1).max(1025);
    let scene = Scene::new(&format!("udp://127.0.0.1:{other_port}"), "zcu104-test", &service.grpc_addr, &[], 1, vec![]);
    let err = scene.run().expect_err("a mismatched device is refused");
    println!("REFUSED: {err}");
    match &err {
        DrmError::BoardRefused { instance, reason } => {
            assert_eq!(instance, CONTROLLER);
            assert!(reason.contains("port device differs"), "the service's own reason: {reason}");
        }
        other => panic!("expected BoardRefused, got {other:?}"),
    }
    assert!(guest.seen.lock().unwrap().binds.is_empty(), "the refused BIND was not forwarded to the guest");
    assert!(service.stderr().contains("Bind refused, nothing forwarded to the board"), "{}", service.stderr());
    guest.finish();
}

#[test]
fn an_edge_node_mismatch_is_refused_at_bind_with_the_services_reason() {
    let _serial = serial();
    let guest = FakeGuest::spawn(GuestPlan::default());
    let service = Service::start("mismatch-node", &udp_device(&guest), "zcu104-test");
    let scene = Scene::new(&udp_device(&guest), "some-other-node", &service.grpc_addr, &[], 1, vec![]);
    let err = scene.run().expect_err("a mismatched edge node is refused");
    assert!(matches!(&err, DrmError::BoardRefused { reason, .. } if reason.contains("edge node id differs")), "{err:?}");
    assert!(guest.seen.lock().unwrap().binds.is_empty());
    guest.finish();
}

/// A guest that stops answering at step 4 is the typed step-timeout error after
/// `board.step_timeout_ms`, in bounded wall time, not a hang.
#[test]
fn a_guest_that_stops_answering_is_a_step_timeout_error_not_a_hang() {
    let _serial = serial();
    let guest = FakeGuest::spawn(GuestPlan { silent_from_step: Some(4), ..GuestPlan::default() });
    let device = udp_device(&guest);
    let service = Service::start("silent-guest", &device, "zcu104-test");
    let scene = Scene::new(&device, "zcu104-test", &service.grpc_addr, &[("board.step_timeout_ms", 400.0)], 3, vec![]);
    let t0 = Instant::now();
    let err = scene.run().expect_err("a silent guest ends the run");
    let wall = t0.elapsed();
    println!("TIMEOUT after {wall:?}: {err}");
    match &err {
        DrmError::ContainerProtocol { instance, source: ContainerError::StepTimeout { timeout_ms } } => {
            assert_eq!(instance, CONTROLLER);
            assert_eq!(*timeout_ms, 400);
        }
        other => panic!("expected ContainerProtocol(StepTimeout), got {other:?}"),
    }
    // Three steps paced at 100 ms each, then the 400 ms wait: well under 3 s, far from a hang.
    assert!(wall >= Duration::from_millis(400) && wall < Duration::from_secs(3), "{wall:?}");
    assert_eq!(guest.seen.lock().unwrap().step_sequences, vec![1, 2, 3, 4], "the guest saw steps 1..=4 and the kernel gave up on 4");
}

/// An edge service that accepts the connection and never speaks HTTP/2 is a typed Bind timeout
/// after `board.bind_timeout_ms` (tonic connects lazily, so the wait lands in Bind), not a hang.
#[test]
fn an_edge_service_that_never_answers_is_a_bounded_typed_error() {
    let _serial = serial();
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = silent.local_addr().unwrap().to_string();
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &addr, &[("board.bind_timeout_ms", 500.0)], 1, vec![]);
    let t0 = Instant::now();
    let err = scene.run().expect_err("a silent edge service is an error");
    let wall = t0.elapsed();
    println!("SILENT SERVICE after {wall:?}: {err}");
    assert!(matches!(&err, DrmError::BoardBind { detail, .. } if detail.contains("no reply within 500 ms")), "{err:?}");
    assert!(wall >= Duration::from_millis(500) && wall < Duration::from_secs(5), "{wall:?}");
}

#[test]
fn nothing_listening_at_the_edge_address_is_a_typed_connect_error() {
    let _serial = serial();
    let port = free_tcp_port();
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], 1, vec![]);
    let err = scene.run().expect_err("nothing is listening");
    assert!(matches!(&err, DrmError::BoardConnect { instance, address, .. } if instance == CONTROLLER && address.ends_with(&port.to_string())), "{err:?}");
}

#[test]
fn a_board_seed_key_missing_from_the_scenario_is_a_typed_error() {
    let _serial = serial();
    let guest = FakeGuest::spawn(GuestPlan::default());
    let service = Service::start("seed", &udp_device(&guest), "zcu104-test");
    let mut scene = Scene::new(&udp_device(&guest), "zcu104-test", &service.grpc_addr, &[], 1, vec![]);
    scene.drm.scenario.as_mut().unwrap().seeds.remove(CONTROLLER);
    scene.drm.hash = av_kernel::drm::hash::canonical_drm_hash(&scene.drm);
    let err = scene.run().expect_err("no seed for board.seed_key");
    assert!(matches!(&err, DrmError::UnknownBoardSeed { seed_key, .. } if seed_key == CONTROLLER), "{err:?}");
    guest.finish();
}

/// `BoardBinding.power_control` is read but not acted on, so a HARDWARE fault (a power cycle
/// included) naming a board instance is refused at load, before any connection is made.
#[test]
fn a_hardware_power_cycle_fault_on_a_board_instance_is_refused_at_load() {
    let _serial = serial();
    let fault = Fault {
        id: "pc1".to_string(),
        instance: CONTROLLER.to_string(),
        target_kind: FaultTargetKind::Hardware as i32,
        kind: "power_cycle".to_string(),
        tai_ns: 1_767_225_638_000_000_000,
        ..Default::default()
    };
    // Nothing listens at the edge address: reaching a connection attempt would be BoardConnect.
    let port = free_tcp_port();
    let scene = Scene::new("udp://127.0.0.1:9", "zcu104-test", &format!("127.0.0.1:{port}"), &[], 3, vec![fault]);
    let err = scene.run().expect_err("a HARDWARE fault on a board is refused");
    assert!(matches!(&err, DrmError::HardwareFaultNotSupportedOnInstance { fault_id, instance } if fault_id == "pc1" && instance == CONTROLLER), "{err:?}");
}

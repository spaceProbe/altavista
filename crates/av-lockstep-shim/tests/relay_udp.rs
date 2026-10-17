//! `av-lockstep-relay` (question 245 (6)) against a fake guest on its Unix socket and plain UDP
//! sockets standing in for `av-edge-board`: the real binary, run natively. The container it runs
//! in is `crates/av-edge-board/tests/cfs_container_restart.rs`.
//!
//! - **One whole frame per datagram, both ways, byte for byte**: a frame the guest writes in
//!   pieces is one datagram; two frames in one write are two datagrams in order; a frame near the
//!   datagram limit goes through; a frame over it is dropped and counted.
//! - **Datagrams that are not exactly one frame** (short, half a frame, trailing bytes, length 0,
//!   unknown type) are dropped and counted, never forwarded; the next valid datagram is.
//! - **While no guest is connected, datagrams are dropped, not queued**: a HELLO sent before the
//!   guest connects is never delivered afterwards.
//! - **The peer**: learned from the last valid datagram (replies go to it), or pinned with `--peer`
//!   (datagrams from any other source are dropped and counted).
//! - **A guest that goes away** (closes, or violates the framing) ends its session; the relay
//!   waits for the next guest.
use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use av_lockstep_shim::framing::{encode_frame, encode_hello, FrameType, PROTOCOL_VERSION};

struct Relay {
    child: Child,
    socket: PathBuf,
    udp: SocketAddr,
    stderr: PathBuf,
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Relay {
    fn spawn(tag: &str, extra: &[String]) -> Self {
        let dir = std::env::temp_dir();
        let socket = dir.join(format!("av-relay-{}-{tag}.sock", std::process::id()));
        let stderr = dir.join(format!("av-relay-{}-{tag}.stderr", std::process::id()));
        let udp: SocketAddr = format!("127.0.0.1:{}", free_udp_port()).parse().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_av-lockstep-relay"))
            .args(["--socket-path", socket.to_str().unwrap(), "--udp-listen", &udp.to_string()])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .expect("spawn av-lockstep-relay");
        let relay = Self { child, socket, udp, stderr };
        wait_until("the relay to listen", || relay.socket.exists() && relay.log().contains("listening for the guest"));
        relay
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn guest(&self) -> UnixStream {
        let s = UnixStream::connect(&self.socket).expect("connect to the relay's socket");
        s.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        s
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(&self.stderr);
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn peer_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    s
}

fn hello() -> Vec<u8> {
    encode_frame(FrameType::Hello, &encode_hello(PROTOCOL_VERSION))
}

/// Read exactly `len` bytes from the guest's socket.
fn read_guest(guest: &mut UnixStream, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    guest.read_exact(&mut buf).expect("the guest's socket delivers the frame");
    buf
}

/// The guest's socket has nothing to read (within its short timeout).
fn guest_is_silent(guest: &mut UnixStream) -> bool {
    let mut b = [0u8; 1];
    matches!(guest.read(&mut b), Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

fn recv(peer: &UdpSocket) -> Option<(Vec<u8>, SocketAddr)> {
    let mut buf = vec![0u8; 70_000];
    peer.recv_from(&mut buf).ok().map(|(n, src)| (buf[..n].to_vec(), src))
}

#[test]
fn frames_cross_both_ways_one_per_datagram_byte_for_byte() {
    let relay = Relay::spawn("frames", &[]);
    let peer = peer_socket();
    let mut guest = relay.guest();
    wait_until("the guest to be accepted", || relay.log().contains("guest connected (session 1)"));

    // peer -> guest: the HELLO arrives as the same 11 bytes, and teaches the relay who the peer is.
    peer.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());

    // guest -> peer: one frame written in three pieces is one datagram.
    let step = encode_frame(FrameType::StepDone, &[7u8; 300]);
    guest.write_all(&step[..3]).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    guest.write_all(&step[3..100]).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    guest.write_all(&step[100..]).unwrap();
    let (dg, src) = recv(&peer).expect("one datagram");
    assert_eq!((dg, src), (step.clone(), relay.udp));
    assert!(recv(&peer).is_none(), "and nothing more");

    // two frames in one write are two datagrams, in order.
    let (a, b) = (encode_frame(FrameType::ResetAck, &[1]), encode_frame(FrameType::BindAck, &[2, 3]));
    let both = [a.clone(), b.clone()].concat();
    guest.write_all(&both).unwrap();
    assert_eq!(recv(&peer).unwrap().0, a);
    assert_eq!(recv(&peer).unwrap().0, b);

    // a large frame goes through, in both directions (8000 bytes: macOS refuses to send a UDP
    // datagram over `net.inet.udp.maxdgram`, 9216 by default, a limit of this host's stack and not
    // of the relay; the relay's own limit is the 65 507-byte IPv4 maximum, exercised below).
    let big = encode_frame(FrameType::StepDone, &vec![0xabu8; 8_000]);
    guest.write_all(&big).unwrap();
    assert_eq!(recv(&peer).unwrap().0, big);
    peer.send_to(&big, relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, big.len()), big);

    // a frame over the limit cannot be one datagram: dropped and counted, and the session goes on.
    let too_big = encode_frame(FrameType::StepDone, &vec![0u8; 70_000]);
    guest.write_all(&too_big).unwrap();
    wait_until("the oversize drop to be logged", || relay.log().contains("cannot be sent as one datagram"));
    assert!(recv(&peer).is_none());
    guest.write_all(&a).unwrap();
    assert_eq!(recv(&peer).unwrap().0, a);
}

#[test]
fn a_datagram_that_is_not_exactly_one_frame_is_dropped_and_counted_never_forwarded() {
    let relay = Relay::spawn("malformed", &[]);
    let peer = peer_socket();
    let mut guest = relay.guest();
    wait_until("the guest to be accepted", || relay.log().contains("guest connected (session 1)"));
    let frame = encode_frame(FrameType::Step, &[1, 2, 3, 4]);
    let mut trailing = frame.clone();
    trailing.push(0);
    let mut unknown_type = frame.clone();
    unknown_type[4] = 0xee;
    let bad: Vec<Vec<u8>> = vec![vec![], vec![1, 2, 3], frame[..frame.len() - 2].to_vec(), trailing, vec![0, 0, 0, 0, 5], unknown_type, vec![0xff, 0xff, 0xff, 0xff, 5]];
    for b in &bad {
        peer.send_to(b, relay.udp).unwrap();
    }
    peer.send_to(&frame, relay.udp).unwrap();
    // Only the valid frame arrives, and it arrives intact after everything before it was dropped.
    assert_eq!(read_guest(&mut guest, frame.len()), frame);
    assert!(guest_is_silent(&mut guest));
    // Close the guest to get the totals line.
    drop(guest);
    wait_until("the session to end", || relay.log().contains("guest session 1 ended"));
    let log = relay.log();
    println!("{log}");
    assert!(log.contains("7 malformed"), "{log}");
}

#[test]
fn datagrams_are_dropped_not_queued_while_no_guest_is_connected() {
    let relay = Relay::spawn("noguest", &[]);
    let peer = peer_socket();
    // Three HELLOs with nobody connected.
    for _ in 0..3 {
        peer.send_to(&hello(), relay.udp).unwrap();
    }
    wait_until("the drops to be logged", || relay.log().contains("(drop #3)"));
    let mut guest = relay.guest();
    wait_until("the guest to be accepted", || relay.log().contains("guest connected (session 1)"));
    assert!(relay.log().contains("3 datagram(s) were dropped while no guest was connected"), "{}", relay.log());
    assert!(guest_is_silent(&mut guest), "a HELLO sent before the guest connected is not delivered afterwards");
    // A HELLO sent now is delivered, and the guest's answer goes back to the peer that sent it
    // (learned even from the datagrams that were dropped).
    peer.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    guest.write_all(&hello()).unwrap();
    assert_eq!(recv(&peer).unwrap().0, hello());
}

#[test]
fn the_peer_is_learned_from_the_last_valid_datagram_or_pinned() {
    // Learned: replies go to whoever sent the last valid datagram.
    let relay = Relay::spawn("learn", &[]);
    let (a, b) = (peer_socket(), peer_socket());
    let mut guest = relay.guest();
    wait_until("the guest to be accepted", || relay.log().contains("guest connected (session 1)"));
    a.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    b.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    guest.write_all(&hello()).unwrap();
    assert_eq!(recv(&b).unwrap().0, hello());
    assert!(recv(&a).is_none(), "the earlier peer gets nothing");
    drop((guest, relay));

    // Pinned: only that address is accepted and sent to; a foreign source is dropped and counted.
    let pinned = peer_socket();
    let stranger = peer_socket();
    let relay = Relay::spawn("pin", &["--peer".to_string(), pinned.local_addr().unwrap().to_string()]);
    let mut guest = relay.guest();
    wait_until("the guest to be accepted", || relay.log().contains("guest connected (session 1)"));
    stranger.send_to(&hello(), relay.udp).unwrap();
    assert!(guest_is_silent(&mut guest));
    wait_until("the foreign drop to be logged", || relay.log().contains("not the configured peer"));
    pinned.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    guest.write_all(&hello()).unwrap();
    assert_eq!(recv(&pinned).unwrap().0, hello());
    assert!(recv(&stranger).is_none());
}

#[test]
fn a_guest_that_goes_away_or_breaks_the_framing_ends_its_session_and_the_next_one_is_served() {
    let relay = Relay::spawn("sessions", &[]);
    let peer = peer_socket();

    // Session 1: the guest closes. A frame sent while nobody is connected is dropped.
    let mut guest = relay.guest();
    wait_until("session 1", || relay.log().contains("guest connected (session 1)"));
    peer.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    drop(guest);
    wait_until("session 1 to end", || relay.log().contains("guest session 1 ended: the guest closed the connection"));
    peer.send_to(&hello(), relay.udp).unwrap();
    wait_until("the drop", || relay.log().contains("no guest is connected"));

    // Session 2: the guest writes a frame with an unknown type: a violation, the session ends.
    let mut guest = relay.guest();
    wait_until("session 2", || relay.log().contains("guest connected (session 2)"));
    assert!(guest_is_silent(&mut guest), "the HELLO sent while nobody was connected is gone");
    guest.write_all(&[2, 0, 0, 0, 0xee, 0]).unwrap();
    wait_until("session 2 to end", || relay.log().contains("guest session 2 ended: protocol violation from the guest"));
    assert!(recv(&peer).is_none(), "the violating frame was not forwarded");

    // Session 3: normal again.
    let mut guest = relay.guest();
    wait_until("session 3", || relay.log().contains("guest connected (session 3)"));
    peer.send_to(&hello(), relay.udp).unwrap();
    assert_eq!(read_guest(&mut guest, 11), hello());
    guest.write_all(&hello()).unwrap();
    assert_eq!(recv(&peer).unwrap().0, hello());
}

#[test]
fn usage_errors_exit_with_status_2_before_binding_anything() {
    for args in [vec![], vec!["--socket-path", "/tmp/x"], vec!["--udp-listen", "127.0.0.1:1"], vec!["--socket-path", "/tmp/x", "--udp-listen", "nonsense"], vec!["--socket-path", "/tmp/x", "--udp-listen", "127.0.0.1:1", "--peer", "nonsense"], vec!["--bogus"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_av-lockstep-relay")).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

//! `av-lockstep-relay` (question 245 (6)): the in-container UDP carrier for lockstep-local v1.
//!
//! The cFS image's guest app (`io_lockstep`, unchanged) connects to a Unix socket and speaks
//! lockstep-local v1 over it. In the ordinary image `av-lockstep-shim` listens there and serves
//! `LockstepService` to the kernel. In the board stand-in the kernel's edge service
//! (`av-edge-board`) runs on the host and reaches the guest over UDP, one whole frame per
//! datagram, so this relay listens on the same Unix socket **in place of the shim** and carries
//! frames between the guest and one UDP peer. It is a wire, not a service: it speaks no HELLO, no
//! BIND, keeps no protocol state, and never answers for either end.
//!
//! ```text
//! av-lockstep-relay --socket-path PATH --udp-listen ADDR:PORT [--peer ADDR:PORT]
//! ```
//!
//! - **Guest to peer.** Each complete frame the guest writes (read with `framing::read_frame`, so a
//!   frame with an unknown type byte or an impossible length is a protocol violation, not
//!   forwarded) is sent as exactly one datagram, `[len u32 LE][type u8][payload]`, the length
//!   prefix kept, byte for byte what the Unix socket carried. A frame that cannot fit one
//!   datagram is dropped and counted. A violation ends the guest's session (counted); the relay
//!   goes back to waiting for a guest to connect.
//! - **Peer to guest.** A datagram that is exactly one complete frame is written to the guest's
//!   socket. Anything else (shorter than a header, a declared length of zero or over the frame
//!   limit, half a frame, trailing bytes) is dropped and counted, never forwarded.
//! - **No guest connected: datagrams are dropped, and counted.** Like a UART whose receiver is not
//!   open, the relay does not queue: a HELLO that arrives before the guest has connected is lost,
//!   not delivered late. This is what makes it safe for the edge service to re-send HELLO until
//!   the guest answers (`av-edge-board --power-cycle-reboots-peer`): a copy can only reach a guest
//!   that is listening, and a guest that has already answered is told apart by the service
//!   stopping at the first answer.
//! - **The peer.** With `--peer ADDR:PORT` only datagrams from that address are accepted and
//!   frames go there. Without it the peer is learned: the source of the most recent valid
//!   datagram. That is what the container needs when the host reaches it through a published
//!   port, where the source address the container sees (a NAT address) cannot be known in
//!   advance and replies must go back to it to be translated onto the published port.
//!   Frames the guest writes before any peer is known are dropped and counted.
//! - **One guest at a time.** A second connection waits in the listen backlog until the first
//!   ends. The socket file is replaced if a stale one exists, as the shim does.
//!
//! Only this crate's existing library code (`framing`) is used; the shim binary is not touched.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use av_lockstep_shim::framing::{self, encode_frame, FrameError, FrameType, MAX_FRAME_LEN};
use tokio::io::AsyncWriteExt;
use tokio::net::{UdpSocket, UnixListener};
use tokio::sync::mpsc;

/// The largest UDP payload over IPv4 (65535 less the 8-byte UDP and 20-byte IP headers).
const MAX_DATAGRAM_PAYLOAD: usize = 65_507;
/// How many drops of one kind are logged individually before only the counters grow.
const LOG_LIMIT: u64 = 5;

const USAGE: &str = "usage: av-lockstep-relay --socket-path PATH --udp-listen ADDR:PORT [--peer ADDR:PORT]";

struct Args {
    socket_path: PathBuf,
    udp_listen: SocketAddr,
    peer: Option<SocketAddr>,
}

fn parse_args() -> Result<Args, String> {
    let (mut socket_path, mut udp_listen, mut peer) = (None, None, None);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--socket-path" => socket_path = Some(PathBuf::from(val()?)),
            "--udp-listen" => {
                let v = val()?;
                udp_listen = Some(v.parse::<SocketAddr>().map_err(|e| format!("--udp-listen {v:?} is not a valid ADDR:PORT: {e}"))?);
            }
            "--peer" => {
                let v = val()?;
                peer = Some(v.parse::<SocketAddr>().map_err(|e| format!("--peer {v:?} is not a valid ADDR:PORT: {e}"))?);
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unrecognized argument: {other}\n{USAGE}")),
        }
    }
    Ok(Args {
        socket_path: socket_path.ok_or_else(|| format!("--socket-path is required\n{USAGE}"))?,
        udp_listen: udp_listen.ok_or_else(|| format!("--udp-listen is required\n{USAGE}"))?,
        peer,
    })
}

/// `Ok(())` iff `datagram` is exactly one complete frame whose type byte is a known frame type.
fn validate_datagram(datagram: &[u8]) -> Result<(), String> {
    if datagram.len() < 5 {
        return Err(format!("{} bytes is shorter than the 5-byte minimum frame", datagram.len()));
    }
    let declared = u32::from_le_bytes([datagram[0], datagram[1], datagram[2], datagram[3]]);
    if declared == 0 {
        return Err("declared frame length 0".to_string());
    }
    if declared > MAX_FRAME_LEN {
        return Err(format!("declared frame length {declared} is over the {MAX_FRAME_LEN} byte limit"));
    }
    let need = 4 + declared as usize;
    if datagram.len() < need {
        return Err(format!("{} bytes but the frame needs {need}: half a frame is not forwarded", datagram.len()));
    }
    if datagram.len() > need {
        return Err(format!("{} bytes but the frame is {need}: {} trailing byte(s)", datagram.len(), datagram.len() - need));
    }
    if FrameType::from_code(datagram[4]).is_none() {
        return Err(format!("unknown frame type byte 0x{:02x}", datagram[4]));
    }
    Ok(())
}

#[derive(Default)]
struct Counters {
    guest_sessions: AtomicU64,
    to_peer: AtomicU64,
    to_guest: AtomicU64,
    dropped_no_guest: AtomicU64,
    dropped_malformed: AtomicU64,
    dropped_foreign: AtomicU64,
    dropped_no_peer: AtomicU64,
    dropped_oversize: AtomicU64,
}

impl Counters {
    fn line(&self) -> String {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        format!(
            "guest sessions {}, frames to peer {}, datagrams to guest {}, dropped: {} while no guest was connected, {} malformed, {} from a foreign source, {} guest frames with no peer known, {} guest frames too large for a datagram",
            g(&self.guest_sessions),
            g(&self.to_peer),
            g(&self.to_guest),
            g(&self.dropped_no_guest),
            g(&self.dropped_malformed),
            g(&self.dropped_foreign),
            g(&self.dropped_no_peer),
            g(&self.dropped_oversize),
        )
    }
}

/// What the guest-reader task reports.
enum FromGuest {
    Frame(Vec<u8>),
    /// The session ended: a clean close, or a protocol violation (the reason says which).
    Ended(String),
}

/// Where frames go and which datagrams are accepted.
struct PeerTracker {
    pinned: Option<SocketAddr>,
    learned: Option<SocketAddr>,
}

impl PeerTracker {
    fn target(&self) -> Option<SocketAddr> {
        self.pinned.or(self.learned)
    }

    /// Whether a datagram from `src` is acceptable.
    fn accepts(&self, src: SocketAddr) -> bool {
        self.pinned.is_none_or(|p| p == src)
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("av-lockstep-relay: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args).await {
        eprintln!("av-lockstep-relay: {e}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    if args.socket_path.exists() {
        std::fs::remove_file(&args.socket_path).map_err(|e| format!("removing stale socket {}: {e}", args.socket_path.display()))?;
    }
    let listener = UnixListener::bind(&args.socket_path).map_err(|e| format!("binding Unix socket {}: {e}", args.socket_path.display()))?;
    let udp = UdpSocket::bind(args.udp_listen).await.map_err(|e| format!("binding UDP {}: {e}", args.udp_listen))?;
    let udp_addr = udp.local_addr().map_err(|e| e.to_string())?;
    eprintln!(
        "av-lockstep-relay: listening for the guest on {} and for the peer on UDP {udp_addr} ({})",
        args.socket_path.display(),
        match args.peer {
            Some(p) => format!("peer pinned to {p}"),
            None => "peer learned from the first valid datagram".to_string(),
        }
    );

    let counters = Counters::default();
    let mut peer = PeerTracker { pinned: args.peer, learned: None };
    let mut buf = vec![0u8; 65_536];
    let mut logged_no_guest = 0u64;

    loop {
        // Wait for a guest. Datagrams that arrive meanwhile are dropped, not queued.
        let stream = loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted.map_err(|e| format!("accepting the guest: {e}"))?;
                    break stream;
                }
                received = udp.recv_from(&mut buf) => {
                    let (len, src) = match received {
                        Ok(x) => x,
                        // An ICMP "port unreachable" for an earlier send is not a receive failure.
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset) => continue,
                        Err(e) => return Err(format!("UDP receive: {e}")),
                    };
                    if !peer.accepts(src) {
                        counters.dropped_foreign.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if validate_datagram(&buf[..len]).is_ok() {
                        // Remember who is asking so the guest's first frame has somewhere to go.
                        peer.learned = Some(src);
                    }
                    let n = counters.dropped_no_guest.fetch_add(1, Ordering::Relaxed) + 1;
                    if logged_no_guest < LOG_LIMIT {
                        logged_no_guest += 1;
                        eprintln!("av-lockstep-relay: dropped a {len}-byte datagram from {src}: no guest is connected (drop #{n})");
                    }
                }
            }
        };
        let session = counters.guest_sessions.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!(
            "av-lockstep-relay: guest connected (session {session}); {} datagram(s) were dropped while no guest was connected; peer {}",
            counters.dropped_no_guest.load(Ordering::Relaxed),
            peer.target().map(|p| p.to_string()).unwrap_or_else(|| "not yet known".to_string())
        );

        let (mut read_half, mut write_half) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<FromGuest>(64);
        let reader = tokio::spawn(async move {
            loop {
                match framing::read_frame(&mut read_half).await {
                    Ok((ty, payload)) => {
                        if tx.send(FromGuest::Frame(encode_frame(ty, &payload))).await.is_err() {
                            return;
                        }
                    }
                    Err(FrameError::PeerClosed) => {
                        let _ = tx.send(FromGuest::Ended("the guest closed the connection".to_string())).await;
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(FromGuest::Ended(format!("protocol violation from the guest: {e}"))).await;
                        return;
                    }
                }
            }
        });

        let ended = loop {
            tokio::select! {
                from_guest = rx.recv() => match from_guest {
                    Some(FromGuest::Frame(frame)) => {
                        let Some(target) = peer.target() else {
                            let n = counters.dropped_no_peer.fetch_add(1, Ordering::Relaxed) + 1;
                            if n <= LOG_LIMIT {
                                eprintln!("av-lockstep-relay: dropped a {}-byte guest frame: no peer address is known yet (drop #{n})", frame.len());
                            }
                            continue;
                        };
                        if frame.len() > MAX_DATAGRAM_PAYLOAD {
                            counters.dropped_oversize.fetch_add(1, Ordering::Relaxed);
                            eprintln!("av-lockstep-relay: dropped a {}-byte guest frame: it cannot be sent as one datagram (limit {MAX_DATAGRAM_PAYLOAD})", frame.len());
                            continue;
                        }
                        match udp.send_to(&frame, target).await {
                            Ok(_) => { counters.to_peer.fetch_add(1, Ordering::Relaxed); }
                            Err(e) => eprintln!("av-lockstep-relay: sending a {}-byte frame to {target} failed: {e}", frame.len()),
                        }
                    }
                    Some(FromGuest::Ended(why)) => break why,
                    None => break "the guest reader stopped".to_string(),
                },
                received = udp.recv_from(&mut buf) => {
                    let (len, src) = match received {
                        Ok(x) => x,
                        // An ICMP "port unreachable" for an earlier send is not a receive failure.
                        Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset) => continue,
                        Err(e) => return Err(format!("UDP receive: {e}")),
                    };
                    if !peer.accepts(src) {
                        let n = counters.dropped_foreign.fetch_add(1, Ordering::Relaxed) + 1;
                        if n <= LOG_LIMIT {
                            eprintln!("av-lockstep-relay: dropped a {len}-byte datagram from {src}, which is not the configured peer (drop #{n})");
                        }
                        continue;
                    }
                    if let Err(why) = validate_datagram(&buf[..len]) {
                        let n = counters.dropped_malformed.fetch_add(1, Ordering::Relaxed) + 1;
                        if n <= LOG_LIMIT {
                            eprintln!("av-lockstep-relay: dropped a malformed datagram from {src}: {why} (drop #{n})");
                        }
                        continue;
                    }
                    peer.learned = Some(src);
                    if let Err(e) = write_half.write_all(&buf[..len]).await {
                        break format!("writing to the guest failed: {e}");
                    }
                    if let Err(e) = write_half.flush().await {
                        break format!("writing to the guest failed: {e}");
                    }
                    counters.to_guest.fetch_add(1, Ordering::Relaxed);
                }
            }
        };
        reader.abort();
        eprintln!("av-lockstep-relay: guest session {session} ended: {ended}; totals: {}", counters.line());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_frame_with_a_known_type_is_valid() {
        let frame = encode_frame(FrameType::Step, &[1, 2, 3]);
        assert!(validate_datagram(&frame).is_ok());
        assert!(validate_datagram(&[]).is_err());
        assert!(validate_datagram(&frame[..4]).is_err());
        assert!(validate_datagram(&[0, 0, 0, 0, 9]).is_err());
        assert!(validate_datagram(&[0xff, 0xff, 0xff, 0xff, 9]).is_err());
        assert!(validate_datagram(&frame[..frame.len() - 2]).is_err());
        let mut two = frame.clone();
        two.extend_from_slice(&frame[..3]);
        assert!(validate_datagram(&two).is_err());
        let mut unknown = frame;
        unknown[4] = 0xee;
        assert!(validate_datagram(&unknown).is_err());
    }

    #[test]
    fn a_pinned_peer_accepts_only_its_own_address() {
        let a: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let b: SocketAddr = "10.0.0.2:5000".parse().unwrap();
        let pinned = PeerTracker { pinned: Some(a), learned: Some(b) };
        assert!(pinned.accepts(a) && !pinned.accepts(b));
        assert_eq!(pinned.target(), Some(a));
        let learning = PeerTracker { pinned: None, learned: Some(b) };
        assert!(learning.accepts(a) && learning.accepts(b));
        assert_eq!(learning.target(), Some(b));
    }
}

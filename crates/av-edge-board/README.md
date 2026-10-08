# av-edge-board

The board's edge service (`docs/open-questions.md` question 242 (b)). It opens the board's link
(`BoardBinding.port_devices`: a serial device or a UDP endpoint), speaks **lockstep-local v1**
(`services/cfs/README.md`) over it to the flight software exactly as `av-lockstep-shim` does over a
Unix socket, and serves `altavista.v1.LockstepService` to the kernel on loopback. It reuses
`av_lockstep_shim::{PeerLink, framing, service::ShimService}` unchanged; the shim is not modified.

The pure half (spec parsing, typed errors, one-link-per-board validation, the canonical form and
SHA-256 config hash) is `av_edge::board` in `crates/av-edge/src/board.rs`, the way the edge
plugin's pure half is `av_edge::plugin`.

**Everything here is proven against stand-ins only**: a host pseudo-terminal, loopback UDP, fake
guests that speak the frame protocol. No board has been involved.

## Command line

```text
av-edge-board --port-device <spec> --edge-node-id <id> [--grpc-addr 127.0.0.1:<port>]
              [--handshake-timeout-ms <n>] [--udp-local <addr:port>]
```

- `--port-device`: `/dev/<name>@<baud>` (serial, framing fixed at 8N1, no flow control) or
  `udp://<host>:<port>` (IPv4, bracketed IPv6, or hostname). Anything else is a typed error that
  names what is wrong (`av_edge::board::PortDeviceSpecError`); there is no default.
- `--edge-node-id`: the edge node this service stands for (non-empty, no whitespace).
- `--grpc-addr`: where `LockstepService` listens. **Loopback only**; a non-loopback address is
  refused at start. Default `127.0.0.1:50081` (not yet in `docs/architecture.md`'s port map).
- `--handshake-timeout-ms`: how long to wait for the guest's HELLO (default 60000). Expiry is a
  typed startup error and exit status 1.
- `--udp-local`: UDP only; the local address to bind (default: an ephemeral port on the wildcard
  address). A board whose target is statically configured needs a fixed port here.

Every stage is logged on stderr: the parsed device and the link's config hash, link open,
handshake, listening, each Bind decision, Shutdown, and (UDP) the datagram counters at exit. One
link per process. The process exits with status 0 after a successful Shutdown RPC (once the reply
is sent) or on SIGINT/SIGTERM; status 1 for any startup failure, 2 for a usage error.

## The handshake: HELLO is sent once

The service speaks first (as the shim does). The guest, `io_lockstep_app.c` on the RTEMS build,
reads that HELLO **once**, answers, and then expects BIND; a second HELLO where BIND is expected
fails the guest. So HELLO is never retried, on any transport. A HELLO sent before the guest's end
of the link is open is simply lost (UART) or unanswered (UDP) and the handshake times out with a
typed error: start the service after the board's end of the link is up.

## Serial transport (`src/serial.rs`)

- Opened `O_RDWR | O_NOCTTY | O_NONBLOCK | O_CLOEXEC`, then raw 8N1 through `libc` termios
  (`cfmakeraw`, `CS8`, no parity, one stop bit, no flow control, `VMIN=1`), the baud on both
  directions, and a read-back that fails if the driver did not apply the baud or the line format.
  Input already queued is flushed. Taken exclusively with `TIOCEXCL` (a second open by another
  non-root process fails with `EBUSY`; **Linux enforces this on a pty slave, macOS's pty driver
  does not**, so the test asserts it on Linux only, where it passes).
- Baud: on Linux only the standard `Bxxx` rates exist (50 to 4 000 000); any other rate is a typed
  `UnsupportedBaud` at open, never a silent nearest rate. On macOS `speed_t` is the rate itself and
  the driver decides.
- No new registry dependency (`libc` only; no `serialport`, `nix`, `tokio-serial`).
- Hang-up (`EIO` on Linux) reads as end of file.

### Paced writes, always

Writes go out **in chunks of at most 64 bytes, with each chunk's 8N1 wire time (10 bits per byte at
the baud rate) between a chunk and the next**. Why: question 238 found that a frame injected into
the RTEMS UART in one burst overflowed termios' 256-byte raw input ring (the Renode UART model has
an unbounded receive FIFO) and bytes were dropped, so the guest blocked forever mid-frame; bursts
of 64 bytes (the Cadence UART's real receive FIFO depth) paced at the baud rate always delivered.
A host pseudo-terminal feeding Renode has no pacing of its own, and on real hardware the UART
paces at the baud rate but the host's tty driver queues far more than 64 bytes ahead of it, so the
pacing is unconditional and there is no switch to turn it off. A 305-byte STEP takes about 26 ms at
115 200 baud, the line's own wire time. The next chunk is released a wire time after the previous
`write` returned, never earlier, so the real line rate is slightly below the nominal one (the
measured median gap between full chunks over a pty was 6.1 to 7.3 ms against a 5.56 ms wire time:
tokio's timers round up to the millisecond).

## UDP transport (`src/udp.rs`)

Lockstep-local over UDP, **one whole frame per datagram**, the length prefix kept, so the bytes are
the same frames the other transports carry. It is an `AsyncRead + AsyncWrite` adapter, so `PeerLink`
runs unchanged: reads serve the bytes of one validated datagram, writes buffer until a whole frame
is present and then send one datagram. (A frame-level seam in `PeerLink` would have meant a
second trait in the shim, which is compiled into the cFS image, for no behavioural gain.)

- A received datagram that is not exactly one complete frame (shorter than the 5-byte minimum,
  declared length 0 or over the 16 MiB frame limit, shorter than its declared frame, or with bytes
  after the frame) is a typed `DatagramError`. It is counted and fails the read that was waiting for
  it (a request-response exchange cannot continue past a lost reply); the stream stays usable. The
  error reaches `PeerLink` inside `FrameError::Io`; `udp::datagram_error` recovers it.
- Datagrams from any address other than the configured peer (every address the host resolves to,
  at the configured port) are dropped, counted and logged (the first five). The socket is not
  `connect`ed on purpose: the kernel would filter silently and nothing could be counted.
- A frame that cannot fit one datagram (65 507 bytes) is a typed write error. No retransmission,
  no reordering protection: a lost datagram is a stalled exchange, surfaced by the caller's timeout.

## The Bind-time check (`src/service.rs`)

When the kernel binds a `BINDING_KIND_BOARD` instance it will set `board.edge_node_id` and
`board.port_device` (`av_edge::board::BIND_PARAM_EDGE_NODE_ID` / `BIND_PARAM_PORT_DEVICE`) in
`LockstepBindRequest.parameters`; the kernel side is the next task. The service compares them with
its own configuration (the device in canonical form, so `udp://HOST:5000` equals `udp://host:5000`):

- **Both present and equal**: both are removed, then the BIND is forwarded. The guest's BIND bytes
  are what the container path sends today (and the guest's `payload[512]` BIND buffer sees nothing
  extra).
- **Different, only one present, or the device unparseable**: refused with a
  `LockstepBindResponse { lockstep_capable: false, refusal_reason }` naming the requested and the
  configured values, and **nothing is forwarded to the board**. This is the refusal shape the
  kernel's container client already surfaces as a typed `ContainerRefused`.
- **Both absent**: forwarded unchanged, with a `WARNING` line on stderr.

## Tests

```text
scripts/dev/cargo-slot test -p av-edge --lib board
scripts/dev/cargo-slot test -p av-edge-board
```

- `tests/serial_pty.rs`: the real binary on the slave side of a pty, a fake guest on the master side,
  the kernel's `BlockingLockstepClient` driving Bind, STEPs with 300 to 1000 input bytes, Reset,
  Shutdown; measures at the master that no read exceeds 64 bytes and that each burst is followed no
  sooner than its wire time (tolerance stated in the test). Also the `TIOCEXCL` check and the
  startup errors.
- `tests/udp_loopback.rs`: the same run over loopback UDP; a half-frame reply and a forged datagram
  from a foreign address; a lost HELLO.
- `tests/bind_check.rs`: matching parameters forwarded stripped (the guest's BIND bytes equal the
  container path's), mismatches refused with nothing forwarded.

The Linux build is checked (not built by the host's cargo) in the digest-pinned Rust builder image
`services/cfs/build-shim.sh` uses.

## Not done

- No board has been involved; no real UART, USB-UART bridge or Ethernet MAC.
- No TLS: the kernel-facing gRPC link is plaintext on loopback, as the shim's (question 155).
- No log of the board's I/O (the next tasks), no `power_control` (`BoardBinding.power_control` is
  not read), no reconnect: one link per process, HELLO once.
- Only one link kind per board instance; a mixed `port_devices` map is refused by
  `av_edge::board::BoardLink` and this service takes a single device.

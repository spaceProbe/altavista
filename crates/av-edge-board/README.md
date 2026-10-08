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
              --io-log <path> --signing-key <pem> --signing-cert <pem>
```

- `--io-log`, `--signing-key`, `--signing-cert`: **required** (usage error, status 2, naming the
  missing flag): a board run without a durable, signed I/O log is refused. The key is the edge
  node's EC P-384 private key, the certificate the one carrying its public key (a key on another
  curve, or not matching the certificate, is refused with status 1 before any file is created).
  The log must not exist (see "The board I/O log" below).

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

## The board I/O log (question 242 (a))

Every exchange that crosses the link is recorded on the edge side as a signed, hash-chained
`altavista.v1.BoardIoRecord` (`proto/altavista/v1/edge.proto`; the writer, reader and verifier
are `av_edge::board_log` in `crates/av-edge/src/board_log.rs`, whose module doc has the byte-exact
format). ADR-005 section 4: the board's I/O is logged "as a signed batch (ADR-004) so the rest of
the run replays".

- **What is logged:** BIND (the request as forwarded, board parameters stripped, and the board's
  response), each STEP (`lockstep_sequence`, `until_tai_ns`, the `inputs` sent and the `outputs` and
  `named_outputs` returned, the bytes as they crossed the link), RESET, SHUTDOWN. A failed exchange
  is logged too, with its `error` (inputs recorded, outputs empty). Not logged: a Bind refused by
  the board-parameter check (it never reaches the board) and the HELLO handshake (link setup). Each
  record carries the run and instance of the kernel's Bind, the signer's certificate fingerprint,
  the link's config hash, and two wall-clock instants: when the last byte of the request had been
  handed to the link and when the last byte of the response had been read from it
  (`timed::TimedStream`; informational, signed, not used by a replay). `BOARD_IO_KIND_POWER_CYCLE`
  is reserved for the later power-control task; nothing writes it.
- **Chain and signature:** `record_hash = SHA-256(prev_hash || canonical body)`, the first
  `prev_hash` the ASCII `GENESIS`, `signature` = ECDSA P-384 over `record_hash` (the
  `MeasurementBatch` definition). File framing `[payload_len u32 LE][record_hash 32][payload]`, as
  `av-ingest`'s partition log, but with one chain, the signed one (no second unsigned hash).
- **Durability:** the record is written with one `write` and `fsync`ed (`File::sync_all`) **before
  the reply is returned to the kernel** (`BoardIoLog::record` is awaited between the exchange and the
  return). On macOS `sync_all` is `F_FULLFSYNC`, a few milliseconds per exchange on this host; the
  kernel's per-step budget must include it. One exchange at a time; records are in exchange order.
- **When the log cannot be written:** the kernel gets `DATA_LOSS` instead of the response, the log
  is poisoned, and every later exchange is refused with `FAILED_PRECONDITION` before anything is
  sent to the board. A run does not continue unlogged.
- **One log per service run:** the file is created exclusively; an existing path is refused
  (appending would splice a second run into the first run's chain and put a crashed run's torn
  tail mid-file). The file's directory entry is `fsync`ed on creation. A startup that fails after
  the log was created (a handshake timeout, say) removes the still-empty file, so the next start is
  not refused; a log holding any record is never removed.
- **What the chain cannot show:** records removed from the *end* of a log. Pin the chain head and
  record count (printed at exit and by the tool) in the run's manifest.

### Reading a log: `av-edge-board-log`

```text
av-edge-board-log <log> --cert <pem> [--records]
```

`--cert` is the signer's certificate (every record must name its fingerprint and verify against its
key) or a bare P-384 public key PEM (signatures only). It prints `key: value` lines (`verification:
OK | OK, TORN TAIL | FAILED: <typed error with the record index>`, `records`, `binds`, `steps`,
`resets`, `shutdowns`, `failed_exchanges`, `first_epoch_tai_ns` / `last_epoch_tai_ns` (STEP
`until_tai_ns`), the wall-clock bounds, `producer`, `signer_cert_sha256`, `link_config_sha256`,
`run_ids`, `chain_head`, `bytes_verified`, and `torn_tail:` if the last frame is physically
incomplete); `--records` adds a line per record. Exit status 0 verified, 3 verified with a torn tail
(the records before it are intact), 1 verification failed, 2 usage. The file is never modified. The
library entry points a replay uses are `av_edge::board_log::{LogVerifier::from_pem, read_log,
verify_bytes}`; a torn tail is `VerifiedLog::recovery`, a typed outcome, never an error.

## Tests

```text
scripts/dev/cargo-slot test -p av-edge --lib board
scripts/dev/cargo-slot test -p av-edge-board
```

- `tests/io_log.rs`: the I/O log through the real binary, the pty and UDP fake guests and the
  kernel's gRPC client. Every exchange of a session is a record; the log verifies; STEP outputs equal
  what the client received; the BIND record equals the BIND bytes the guest received; **immediately
  after each Step returns, a fresh independent read of the file already holds that step's record**;
  the startup refusals; a failed exchange; the tool's exit codes for tampering, a wrong certificate
  and a torn tail.
- `tests/io_log_inprocess.rs`: `BoardService` over an in-memory stream with a sink recording write
  and sync events: the order guest-reply, write, sync-start, sync-end, response-returned with an
  80 ms sync; and `DATA_LOSS` / `FAILED_PRECONDITION` after an injected sync failure with the
  guest's STEP count unmoved. (The real binary's `fsync` syscall itself was not traced; the ordering
  of write and sync before the reply is established here, in the sink, and in
  `av_edge::board_log`'s unit tests.)
- `tests/serial_pty.rs`: the real binary on the slave side of a pty, a fake guest on the master side,
  the kernel's `BlockingLockstepClient` driving Bind, STEPs with 300 to 1000 input bytes, Reset,
  Shutdown; measures at the master that no read exceeds 64 bytes and that each burst is followed no
  sooner than its wire time (tolerance stated in the test). Also the `TIOCEXCL` check and the
  startup errors.
- `tests/udp_loopback.rs`: the same run over loopback UDP; a half-frame reply and a forged datagram
  from a foreign address; a lost HELLO.
- `tests/bind_check.rs`: matching parameters forwarded stripped (the guest's BIND bytes equal the
  container path's), mismatches refused with nothing forwarded.

The Linux build of hilprep-3b was checked (not built by the host's cargo) in the digest-pinned Rust
builder image `services/cfs/build-shim.sh` uses; the I/O log additions have not been built on Linux.

## Not done

- No board has been involved; no real UART, USB-UART bridge or Ethernet MAC.
- No TLS: the kernel-facing gRPC link is plaintext on loopback, as the shim's (question 155).
- No `power_control` (`BoardBinding.power_control` is
  not read), no reconnect: one link per process, HELLO once.
- Only one link kind per board instance; a mixed `port_devices` map is refused by
  `av_edge::board::BoardLink` and this service takes a single device.

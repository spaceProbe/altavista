# av-edge-board

The board's edge service (`docs/open-questions.md` question 242 (b)). It opens the board's link
(`BoardBinding.port_devices`: a serial device or a UDP endpoint), speaks **lockstep-local v1**
(`services/cfs/README.md`) over it to the flight software exactly as `av-lockstep-shim` does over a
Unix socket, and serves `altavista.v1.LockstepService` to the kernel on loopback, with
`altavista.v1.BoardEdgeService` (the edge node's power control) on the same address. It reuses
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
              [--power-control cmd:/abs/path] [--power-timeout-ms <n>]
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
- `--power-control`: this edge node's power control channel (see "Power control" below):
  `cmd:<absolute path>`, or absent for none. `gpio://...` is reserved and refused at start, as is
  anything malformed. `--power-timeout-ms`: how long one run of the channel may take before it is
  killed (default 30000, 10 to 600000).
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

When the kernel binds a `BINDING_KIND_BOARD` instance it sets `board.edge_node_id` and
`board.port_device` (`av_edge::board::BIND_PARAM_EDGE_NODE_ID` / `BIND_PARAM_PORT_DEVICE`) in
`LockstepBindRequest.parameters`. The service compares them with its own configuration (the
device in canonical form, so `udp://HOST:5000` equals `udp://host:5000`). **A board link is always
checked**: a Bind that lacks the parameters is refused, so the check protects every client, not
only a kernel that sends them.

- **Both present and equal**: both are removed, then the BIND is forwarded. The guest's BIND bytes
  are what the container path sends today (and the guest's `payload[512]` BIND buffer sees nothing
  extra).
- **Different, only one present, neither present, or the device unparseable**: refused with a
  `LockstepBindResponse { lockstep_capable: false, refusal_reason }` naming the requested and the
  configured values, and **nothing is forwarded to the board**. This is the refusal shape the
  kernel's container client already surfaces as a typed `ContainerRefused`.
  The refusal for a Bind with neither parameter says so and names both sides. There is no opt-out
  flag (until the power-control task the service forwarded such a Bind unchecked with a warning;
  that was the lead's ruling to close, question 242).

## Power control (question 242 (c)): `altavista.v1.BoardEdgeService`

`BoardBinding.power_control` is the **edge node's** power control channel, and the board may hang
off this host or off a separate Linux edge node, so the kernel never runs it: a power-cycle fault
asks this service, at the address the kernel already dials for the board, over
`BoardEdgeService.PowerCycle` (`proto/altavista/v1/board.proto`, a separate service so nothing
that implements `LockstepService` changes). `src/power.rs` has the full contract; in short:

- **The channel** (`av_edge::board::parse_power_control`): `cmd:<absolute path>` runs that
  executable here as `<path> power-cycle --edge-node-id <id> --instance <name> --fault-id <id>
  --tai-ns <n>`: no shell, working directory `/`, stdin and stdout `/dev/null`, stderr captured
  (the last 4096 bytes are returned), its own process group, killed (the whole group, `SIGKILL`) at
  `--power-timeout-ms`. Exit 0 is success. `gpio://...` is reserved: not implemented this round.
- **Refusals** (an ordinary response, outcome `REFUSED`, nothing run, nothing logged): the service
  has no channel, the request's `power_control` differs from its own (canonical form), or its
  `edge_node_id` differs. A malformed `instance` / `fault_id` is `INVALID_ARGUMENT`.
- **Failures** (outcome `FAILED`, typed): the executable could not be started, exited non-zero,
  was ended by a signal, or timed out, each with a detail and the captured stderr tail.
- **The I/O log:** a channel that was started is one `BOARD_IO_KIND_POWER_CYCLE` record, durable
  before the reply returns, between the STEP records either side of it: `run_id`, `instance`,
  `reset_tai_ns` (the fault's epoch), `reset_reason` (`fault:<id>`), `error` for a failure, and
  the instants the channel was started and finished. It shares the log's exchange gate, so no
  STEP interleaves; a failed log refuses it before anything runs.
- **Not done (HIL-day item):** this service performs the power cycle and nothing more. A board that
  really reboots drops the lockstep-local link; the service must then re-handshake (HELLO once) and
  the kernel must re-`Bind` before its `RESET`. Neither exists, and the stand-in cannot exercise
  it: the fake guest and the Renode binding stay up through the "power cycle".
- **The fake for tests** (also what the stand-in run uses): `--power-control cmd:<a tiny
  executable>` that appends one JSON line per call beside itself and exits 0, or fails or sleeps on
  demand; `tests/common/mod.rs` `FakeChannel` (and the kernel's `tests/drm_board_common`).

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
  (`timed::TimedStream`; informational, signed, not used by a replay). A power cycle the service
  ran is a `BOARD_IO_KIND_POWER_CYCLE` record (see "Power control").
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
- **What the chain cannot show:** records removed from the *end* of a log. So the kernel pins the
  chain head and record count in the run's products (hilprep-2b): `BoardEdgeService.BoardIoLogHead`
  (`proto/altavista/v1/board.proto`, served on the same address as `LockstepService`) reports
  `records`, `chain_head` (32 raw bytes), `signer_cert_sha256`, `link_config_sha256` and
  `producer_id` of the log as it stands. It is **read only** (it appends nothing), takes the
  exchange gate (never answered between a STEP's exchange and its record), and is
  `FAILED_PRECONDITION` when the log has failed, when no Bind has named a run yet, or when its
  `run_id` / `instance` are not the Bind's. The kernel asks once, after the last STEP and **before**
  the SHUTDOWN (the service records the SHUTDOWN and exits, so it cannot answer after it); a replay
  requires the pinned count and head and then exactly one SHUTDOWN record
  (`crates/av-kernel/README.md`, "Replaying a board-bound run"). The count and head are also printed
  at exit and by the tool.

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
  container path's), mismatches and a Bind lacking the parameters refused with nothing forwarded.
- `tests/power_cycle.rs`: `PowerCycle` through the real binary and the kernel's client
  (`av_lockstep::board_edge`): the channel runs once, as the service's child, with the documented
  argv, and the log holds `STEP, POWER_CYCLE, RESET, STEP` in order; every refusal runs nothing and
  logs nothing; a non-zero exit, a spawn failure and a timeout (the child really dead) are typed
  failures; reserved and malformed channels are startup errors.
- `tests/power_inprocess.rs`: the `POWER_CYCLE` record is written and synced before the reply
  returns; a failed sync is `DATA_LOSS`, and the failed log refuses the next power cycle before the
  channel runs.
- `tests/log_head_inprocess.rs` (hilprep-2b): `BoardIoLogHead` equals the end of the verified log
  (count, last record's hash, signer, link, producer), writes nothing, moves with the log, and is
  refused before a Bind, for another run or instance, and from a failed log. The kernel's end-to-end
  proof is `crates/av-kernel/tests/drm_board_replay.rs`.

The Linux build of hilprep-3b was checked (not built by the host's cargo) in the digest-pinned Rust
builder image `services/cfs/build-shim.sh` uses; the I/O log additions have not been built on Linux.

## Not done

- No board has been involved; no real UART, USB-UART bridge or Ethernet MAC.
- No TLS: the kernel-facing gRPC link is plaintext on loopback, as the shim's (question 155).
- No reconnect: one link per process, HELLO once; so no re-handshake after a power cycle that
  really reboots the board (see "Power control"). `gpio://` power control is reserved.
- Only one link kind per board instance; a mixed `port_devices` map is refused by
  `av_edge::board::BoardLink` and this service takes a single device.

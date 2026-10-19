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
              [--power-cycle-reboots-peer [--rehandshake-interval-ms <n>] [--rehandshake-timeout-ms <n>]]
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
- `--power-cycle-reboots-peer`: **declares** that the power channel reboots the flight software (a real
  board always would; the existing stand-ins' power cycles, the fake channel and Renode, do not and
  must not set it). After a power cycle that performed, the service re-handshakes with the rebooted
  peer (see "Re-handshake after a reboot" below). It needs `--power-control`.
  `--rehandshake-interval-ms` (default 1000, 100 to 60000, not above the timeout) is the time between
  re-sent HELLOs; `--rehandshake-timeout-ms` (default 60000, 200 to 600000) is the deadline, counted
  from the first re-sent HELLO. Both are usage errors without the declaration.
- `--udp-local`: UDP only; the local address to bind (default: an ephemeral port on the wildcard
  address). A board whose target is statically configured needs a fixed port here.

Every stage is logged on stderr: the parsed device and the link's config hash, link open,
handshake, listening, each Bind decision, Shutdown, and (UDP) the datagram counters at exit. One
link per process. The process exits with status 0 after a successful Shutdown RPC (once the reply
is sent) or on SIGINT/SIGTERM; status 1 for any startup failure, 2 for a usage error.

## The handshake: HELLO is sent once (at startup)

The service speaks first (as the shim does). The guest, `io_lockstep_app.c` on the RTEMS build,
reads that HELLO **once**, answers, and then expects BIND; a second HELLO where BIND is expected
fails the guest. So the startup HELLO is never retried, on any transport. A HELLO sent before the
guest's end of the link is open is simply lost (UART) or unanswered (UDP) and the handshake times out
with a typed error: start the service after the board's end of the link is up. The one place HELLO
is sent again is the re-handshake after a declared reboot, below.

## Re-handshake after a reboot (question 245 (6))

A peer that really reboots loses its lockstep-local session: it needs a fresh HELLO, then a fresh BIND
from the kernel, before RESET and STEP. **A `docker restart` of the cFS container stands in for a board
reboot in every proof here; no board has been involved.**

- **When:** only when the operator declared `--power-cycle-reboots-peer`, and only after a power cycle
  whose channel **performed**. Without the declaration nothing changes (their tests are unchanged and
  green: `tests/power_cycle.rs`, the kernel's `drm_board_*`).
- **What:** inside the same exchange gate as the power cycle (no STEP, RESET or BIND interleaves), the
  service discards what the link has queued (UDP: every waiting datagram; serial: `tcflush`), builds a
  fresh `PeerLink` on the same link (`relink::SharedStream` is a cloneable handle on the one stream;
  `av-lockstep-shim` is unchanged) and sends HELLO; if the peer has not answered within the interval,
  it discards and sends again, until an answer or the deadline. A frame that is not a HELLO where the
  answer should be is noise: the cadence is kept, not a HELLO per datagram. A HELLO answer of another
  protocol version or magic is not retried: `REHANDSHAKE_REFUSED`, with the typed protocol error.
- **The result is in the `PowerCycle` response** (`board.proto`, additive): `peer_rehandshaken`,
  `rehandshake_hellos_sent`, `rehandshake_duration_ns`, `rehandshake_timeout_ms`; a peer that never
  answers is outcome `FAILED` with failure `REHANDSHAKE_TIMEOUT` (`detail` names the deadline). The
  kernel then `Bind`s again with the run's same Bind request before its RESET.
- **A lost link stays lost:** after a failed re-handshake every Bind, Step and Reset is refused with
  `UNAVAILABLE` ("the link to the board is lost") before anything is written to the board; a later
  power cycle that re-handshakes successfully clears it. A Shutdown on a lost link is logged with the
  reason, answered `UNAVAILABLE`, and still ends the process (a failed run must not leave this service
  running).
- **Logged:** one `BOARD_IO_KIND_LINK_REHANDSHAKE` record (`edge.proto`, additive: the kind and
  `rehandshake_hellos_sent`, `rehandshake_timeout_ms`), written after the `POWER_CYCLE` record and
  durable before the reply, with the edge-node instants the first HELLO was sent and the re-handshake
  ended, and `error` set for a failure. The kernel's second BIND is an ordinary `BIND` record. One
  chain: `BIND, STEP.., POWER_CYCLE, LINK_REHANDSHAKE, BIND, RESET, STEP.., SHUTDOWN`.
  `av-edge-board-log` prints `link_rehandshakes:` and the record. The HELLO count and the durations are
  wall-clock dependent (how long the peer took to boot); a replay consumes the records and compares
  none of them.
- **Why re-sending HELLO is safe, and the numbers:** (1) nothing reaches a guest before it is
  listening: the cFS stand-in's relay (`av-lockstep-relay`, `services/cfs/README.md`) drops datagrams
  while no guest is connected, as a UART whose receiver is not open would; (2) the interval is far
  longer than the guest's HELLO reply time, so the answer to one HELLO is read before the next is
  sent. **Interval: 1000 ms by default. Measured reply time of the real cFS guest (a HELLO datagram to
  the relay in its container, the answer back): 0.32 ms before the restart and 0.36 ms after it**
  (container started to guest connected: 1.73 s; after `docker restart`, three HELLOs were sent a
  second apart, the relay dropped the first two because the guest was not yet connected, the third
  was answered 2.29 s after the restart began; the guest bound twice and logged no failure). Those
  numbers were taken inside the Docker VM, see "Docker on macOS" below.
- **Residual risks, stated plainly.** (a) A lost *answer* (a dropped datagram, a corrupted byte) makes
  the service send another HELLO to a guest that has already answered and is waiting for BIND, which
  fails it. On loopback or the container network that does not happen; on a real serial board a
  corrupted reply is possible and unhandled. (b) A real serial board whose UART receiver buffers bytes
  while its firmware is still booting (a hardware FIFO of 16 to 64 bytes holds up to five 11-byte
  HELLOs) delivers several HELLOs to the booted guest, which reads one, answers, and then reads the
  next where it expects BIND. The relay avoids this for the container because it drops; a serial board
  has no such guard, and the interval cannot know the board's boot time. The mitigation on HELLO day is
  a `--rehandshake-interval-ms` longer than the board's boot time, giving up that responsiveness, or a
  board-side drain of its UART before it reads HELLO. Neither is decided or measured here. (c) The
  discard before each HELLO removes frames of the dead session that arrived before it, not ones that
  arrive during the attempt (noise is handled as above).

### Docker on macOS: why the edge service runs inside the Colima VM (measured)

**A `docker restart` of the container stands in for a board reboot, and no board is involved.** The
real-container proof also needs the edge service to exchange UDP datagrams with the container's
published port, and **on this host that cannot be done from the macOS host**: Colima 0.10.3 / Lima 2.1.4
(`vmType: vz`) is configured with `portForwarder: ssh` (`~/.colima/default/colima.yaml`), and Colima's
own comment on that setting says "ssh is more stable but supports only TCP". Measured with a throwaway
echo container: a datagram sent from the host to `127.0.0.1:<published UDP port>` is never delivered, a
container's datagram to the host gateway never reaches a host socket either, and the Lima host agent
(`~/.colima/_lima/colima/ha.stderr.log`) logs `Forwarding TCP from 127.0.0.1:N` for every published TCP
port and nothing for the UDP port its guest agent reported. The same exchange run **inside the VM**
works (replies from `127.0.0.1:<port>`, the container seeing the NAT address 172.17.0.1). Changing the
forwarder (`grpc` carries UDP, marked experimental) restarts the shared VM, so it was not done (native
round decision 15).

So the test launches the **Linux build of `av-edge-board` inside the Colima VM**, which stands in for the
edge node the board hangs off, and keeps the kernel (the test's `BlockingLockstepClient`) on the host:

- `services/cfs/build-edge-board.sh` builds `av-edge-board` and `av-edge-board-log` for linux/aarch64
  in the digest-pinned Rust builder `build-shim.sh` uses (`rust:1.90-bookworm@sha256:3914072c...`; a
  test pins that the two scripts name the same digest). Output: `target-docker-linux/edge-board/`
  (git-ignored). **The build is byte-reproducible** (question 254 item 9; it was not before, question
  249): the protobuf packages are pinned by version and installed from `snapshot.debian.org` at one
  timestamp (`20260711T103542Z`, the first snapshot whose bookworm index lists them), signatures still
  verified, the fetched `.deb` set (`6c2aadcc...`) and the installed package set (`e1d35298...`) checked
  against pins before the build; nothing is installed from the live mirror (libssl-dev 3.0.17 and
  pkg-config come with the digest-pinned image; the old unpinned `libssl-dev pkg-config` install only
  upgraded OpenSSL and changed no byte); `--locked`; and `--remap-path-prefix` erases the repo, spoore,
  cargo-home and target-dir paths. Two builds of one source tree from different host stage dirs, repo
  mount paths, cargo target dirs and output dirs gave byte-identical binaries: `av-edge-board`
  `d241d6db36b51461a87d7c69d6e373a13ce45349e1e62f78da6de3efddf38f35`, `av-edge-board-log`
  `654925ce4f1cb3a4ccdcd221556098630344d4ce1f31cac8c3c7b4cb1b721338` (the script's header records the
  controls). These differ from the earlier recorded `071fcf20...` (`av-edge-board`) only by the path
  remap: the same recipe with `EDGE_BOARD_NO_REMAP=1` at the old default paths reproduces `071fcf20...`
  exactly, and without the remap a different mount path gives different bytes (so the earlier build was
  reproducible only for one fixed path). A different source commit gives different hashes by definition;
  the build prints its inputs. The binary runs against the VM's own `libssl.so.3`.
- The test starts it with `colima ssh -- <launch script>`, the I/O log, the signing identity, the
  power-cycle fixture and the service's stderr under `$HOME/av-edge-board-cfs-restart-stage-<pid>/` (the
  VM mounts `$HOME` at the same path). The launch script records its pid and `exec`s the service under
  `timeout 1500`, so the VM-side process cannot outlive its cap. The fixture is the VM's own
  `/usr/bin/docker restart -t 2 <container>`.
- The host reaches the service's gRPC (`--grpc-addr 127.0.0.1:<port>`, loopback in the VM) over TCP: Lima
  forwards a guest `127.0.0.1:<port>` listener to the host's `127.0.0.1:<port>` (observed: the test
  connects from the host and runs the whole session through it).
- At the end of the test, and when it panics (a perturbation that panics right after the service came
  up was run), a guard stops the VM-side process; the test then checks with `pgrep -f` inside the VM
  that none remains (the `pgrep` pattern was shown to find a live process and to find none after it
  ended), that no container of the run's label remains, and the stage directories are removed.
- The kernel's end-to-end test of a board that reboots mid-run, `crates/av-kernel/tests/drm_board_cfs_restart.rs`,
  uses this same path (the kernel on the host, `board.edge_address` the Lima-forwarded
  `127.0.0.1:<port>`, the service in the VM with `--port-device udp://127.0.0.1:<published port>`).
- A host where UDP reaches the published port (Linux Docker, or a UDP-capable forwarder) is detected by
  sending the relay one datagram from the host; the test then runs the host build of the service
  instead. A host where neither path works is a visible SKIPPED naming both reasons (a panic under
  `AV_REQUIRE_DOCKER_TESTS=1`).

**Measured, real cFS container, through that path** (`scratch`: `cfs_container_restart-1.log`): the
startup handshake completed in under 1 ms (HELLO sent to answer read); `PowerCycle` (a real
`docker restart -t 2`, which returned in 0.28 s) took 2.3 s in all: HELLO #1 at +0 ms, #2 at +1001 ms
and #3 at +2007 ms, answered 1 ms after it was sent (the first two were dropped by the relay, the guest
not being connected yet); the guest bound twice and logged no failure; the signed log holds
`BIND, STEP x3, POWER_CYCLE, LINK_REHANDSHAKE, BIND, RESET, STEP x5, SHUTDOWN`. Reply times measured
more finely by a driver run inside the VM: HELLO answer 0.32 ms before and 0.36 ms after the restart.

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
- **A peer that reboots:** with `--power-cycle-reboots-peer` the service re-handshakes after the
  power cycle (see "Re-handshake after a reboot"); without it this service performs the power cycle
  and nothing more, which is right for the stand-ins whose guest stays up (the fake guest, Renode).
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
  ran is a `BOARD_IO_KIND_POWER_CYCLE` record (see "Power control"), followed by a
  `BOARD_IO_KIND_LINK_REHANDSHAKE` record when it re-handshook (see "Re-handshake after a reboot").
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
- `tests/rehandshake.rs` (question 245 (6)): the real binary, the kernel's clients, a power channel
  that signals a **fake guest that really loses its session** (down for a time, then waiting for a
  HELLO, a second HELLO where BIND is expected is a recorded violation) over loopback UDP: the
  reboot is re-handshaken, re-Bound and the run continues, the log is one chain with the
  `LINK_REHANDSHAKE` record and the second BIND, and the reader tool counts it; a guest that never
  returns fails the power cycle at the deadline within a bounded time and the lost link refuses
  everything (nothing more reaches the guest) while Shutdown still ends the process; another protocol
  version is refused at once; noise in place of an answer is not an answer; a stale frame queued before
  the re-handshake is discarded; the control (no declaration) leaves the rebooted guest unanswered; the
  flags' usage errors; and that without the declaration a power cycle re-sends nothing and adds no
  record.
- `tests/cfs_container_restart.rs` (question 245 (6)): **the real cFS container** (`io_lockstep`
  unchanged, the relay in place of the shim) behind the real service over UDP, through a real
  `docker restart` standing in for a board reboot, driven by the kernel's `BlockingLockstepClient`:
  HELLO, BIND, STEPs, `PowerCycle`, re-handshake, BIND again, RESET, STEPs. Docker-gated, takes the
  docker lock itself (never wrap it). On this host the service runs inside the Colima VM (see "Docker
  on macOS"); it needs `AV_CFS_TEST_IMAGE` to name an image that has the relay and, for the VM path,
  the Linux binary (`services/cfs/build-edge-board.sh`, or `AV_EDGE_BOARD_LINUX`).
- `tests/log_head_inprocess.rs` (hilprep-2b): `BoardIoLogHead` equals the end of the verified log
  (count, last record's hash, signer, link, producer), writes nothing, moves with the log, and is
  refused before a Bind, for another run or instance, and from a failed log. The kernel's end-to-end
  proof is `crates/av-kernel/tests/drm_board_replay.rs`.

The Linux build of hilprep-3b was checked (not built by the host's cargo) in the digest-pinned Rust
builder image `services/cfs/build-shim.sh` uses; the I/O log additions have not been built on Linux.

## Not done

- No board has been involved; no real UART, USB-UART bridge or Ethernet MAC.
- No TLS: the kernel-facing gRPC link is plaintext on loopback, as the shim's (question 155).
- No reconnect on a dropped link: one link per process. The only re-handshake is after a power cycle
  declared to reboot the peer (see "Re-handshake after a reboot", with its residual risks for a real
  serial board). `gpio://` power control is reserved.
- Only one link kind per board instance; a mixed `port_devices` map is refused by
  `av_edge::board::BoardLink` and this service takes a single device.

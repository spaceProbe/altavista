# The lockstep shim and "lockstep-local v1"

M23.1 (`docs/open-questions.md` question 153, `docs/sil-plan.md` M23). cFS is C, and a gRPC
implementation inside it would be heavy and hard to keep deterministic. `crates/av-lockstep-shim`
is a small Rust process that runs beside the flight software in the same container: it speaks
ordinary `altavista.v1.LockstepService` gRPC to the kernel (the same contract, and the same
`crates/av-lockstep` client, `services/lockstep-ref` already proves end to end) and translates
every call into **"lockstep-local v1"**, a length-prefixed frame protocol, over a Unix socket to
the flight software. The same shim fronts the Renode bridge in M24, so nothing about this
protocol assumes cFS or a container — it is a plain framed byte protocol over any
`AsyncRead + AsyncWrite` stream (`crates/av-lockstep-shim/src/peer_link.rs`'s `PeerLink<S>` is
generic over `S`; every test in that crate drives it over an in-process
`tokio::net::UnixStream::pair()`, never a real socket file).

M23.1 landed this README as the byte-exact specification the shim (`crates/av-lockstep-shim`)
already implements and is pinned against (`crates/av-lockstep-shim/tests/frame_bytes.rs`). M23.2
is what speaks it from the cFS side:

- `psp-lockstep/` -- the lockstep PSP timebase (question 143): a small library exposing
  `psp_lockstep_release_tick`/`psp_lockstep_external_sync`/`psp_lockstep_current_tai_ns`, the
  latter registered as OSAL's `OS_TimerSync_t` in place of NULL so cFS's own scheduler timebase
  blocks on the kernel's ticks instead of a wall-clock POSIX timer. No OSAL/PSP source under
  `third_party/cfs` is patched -- `OS_TimeBaseCreate`'s `external_sync` parameter is exactly this
  extension point, already public and unmodified upstream.
- `apps/sch_lockstep/` -- replaces SCH_LAB: creates the lockstep timebase and attaches a tick
  callback via `OS_TimerAdd`.
- `apps/io_lockstep/` -- replaces CI_LAB and TO_LAB with one app (not two -- see its own module
  doc comment for why): owns the Unix domain socket connection to the shim (implementing this
  README's frame layer independently in C -- `lockstep_local_framing.c`/`lockstep_local_io.c`),
  decodes/encodes CCSDS Space Packets per the M22.3 `PacketCodec` convention
  (`ccsds_codec.c`, a from-scratch C port of `crates/av-kernel/src/codec.rs` checked byte-for-byte
  against it, not merely against itself -- see `services/cfs/tests/test_ccsds_golden.py`), and
  encodes/decodes the `altavista.v1` lockstep messages with a minimal hand-written protobuf codec
  (`pbmini.c`/`lockstep_messages.c`, checked the same way against real `prost`-encoded bytes --
  see `services/cfs/tests/test_lockstep_messages.py`).
- `build/` -- mission-config overrides (`targets.cmake`, `generate_startup.cmake`,
  `cpu1_install_custom.cmake`) that `Dockerfile` copies over the fetched bundle's own
  `sample_defs/` equivalents; each file's own header comment explains why it replaces the
  original rather than patching cFE/OSAL/PSP.
- `Dockerfile` -- builds `third_party/fetch-cfs.sh`'s pinned cFS for OSAL-posix
  (`SIMULATION=native`) with these two apps; digest recorded in `IMAGE_DIGEST.md`.
- `tests/` -- host-buildable C tests (byte-exact against real Rust/prost output, no Docker
  needed) plus the Docker-gated image/digest test; see `IMAGE_DIGEST.md` for the runtime smoke
  check and its host caveats.

## Who listens, who connects, who speaks first

The **shim listens** on the Unix socket; the **flight-software peer connects** to it. This
mirrors the real deployment order: the container/orchestrator starts the shim first (it is what
proves `lockstep_capable` to the kernel), and the flight software's I/O app connects to a path
it is handed at boot — exactly the same "already listening" role the M24 Renode bridge process
will play for its own peer. Given that, the **shim also speaks first** once a connection is
accepted: it sends its own `HELLO` immediately, rather than two processes each waiting on the
other. See `crates/av-lockstep-shim/src/peer_link.rs`'s module doc comment for the same
rationale in code.

One shim process serves exactly one peer connection for its lifetime — a fresh shim (and a fresh
bound flight-software process) starts per container/run, matching `Bind`'s own one-shot-per-process
contract; `Reset` exists precisely so a *bound* process can be power-cycled without a fresh `Bind`.

## Endianness: little-endian, deliberately, and why that differs from CCSDS elsewhere

**Every multi-byte integer in this protocol is little-endian.** This is a deliberate choice, not
an oversight, and it differs from this platform's CCSDS framing elsewhere (which is big-endian on
the wire — CCSDS is designed to cross a real RF/network link between independently-built
spacecraft/ground systems, where a fixed network byte order matters). "lockstep-local v1" never
leaves one host: it is a Unix socket between two processes in the same container (or, in M24,
between the Renode bridge process and its peer on the same host). There is no second party with
its own byte-order convention to interoperate with, and matching CCSDS's big-endian choice here
would buy nothing but an unconditional byte-swap on every frame on both aarch64 and x86_64 hosts
(both little-endian natively). Little-endian is also already this platform's own precedent for
exactly this kind of same-process/same-host payload: `lockstep.proto`'s own doc comment says
"SIGNAL payloads are little-endian f64", matched byte-for-byte by
`av_dynamics::encode_signal`/`decode_signal` and `lockstep_ref.server.encode_signal`/
`decode_signal` — this protocol's header fields simply follow the same convention its own
payloads already use.

## Frame layout

Every frame on the wire:

```
+----------------------+------------------+------------------------------+
| length : u32, LE      | frame_type : u8 | payload : (length - 1) bytes |
+----------------------+------------------+------------------------------+
     4 bytes                 1 byte              length - 1 bytes
```

- **`length`** is a little-endian `u32`. It is the number of bytes that follow it —
  `1 (frame_type) + payload.len()` — and does **not** include itself. A reader that has just
  read the 4-byte `length` field reads exactly `length` more bytes to have the complete frame.
- **`frame_type`** is one byte (see the table below). Endianness does not apply to a
  single-byte field.
- **`payload`** is `length - 1` bytes, interpreted per `frame_type` (see below).

A frame's total on-wire size is always `4 + length` bytes.

An implementation must enforce an upper bound on `length` before allocating a buffer for the
payload (this shim's own bound, `MAX_FRAME_LEN` in `crates/av-lockstep-shim/src/framing.rs`, is
16 MiB) — a garbled or hostile `length` field must fail as a typed "frame too large" error, never
drive an unbounded allocation.

### Frame types

| `frame_type` | name           | direction     | payload                                    |
|--------------|----------------|---------------|---------------------------------------------|
| `0x01`       | `HELLO`        | either (see "Who speaks first") | this document's own layout, below |
| `0x02`       | `BIND`         | shim -> peer  | `LockstepBindRequest` (protobuf)             |
| `0x03`       | `BIND_ACK`     | peer -> shim  | `LockstepBindResponse` (protobuf)            |
| `0x04`       | `STEP`         | shim -> peer  | `LockstepStepRequest` (protobuf)             |
| `0x05`       | `STEP_DONE`    | peer -> shim  | `LockstepStepResponse` (protobuf)            |
| `0x06`       | `RESET`        | shim -> peer  | `LockstepResetRequest` (protobuf)            |
| `0x07`       | `RESET_ACK`    | peer -> shim  | `LockstepResetResponse` (protobuf)           |
| `0x08`       | `SHUTDOWN`     | shim -> peer  | `LockstepShutdownRequest` (protobuf)         |
| `0x09`       | `SHUTDOWN_ACK` | peer -> shim  | `LockstepShutdownResponse` (protobuf)        |
| `0xFF`       | `ERROR`        | either        | this document's own layout, below            |

Every non-`HELLO`, non-`ERROR` payload is the unmodified
`prost::Message::encode_to_vec()` bytes of the matching message from
`proto/altavista/v1/lockstep.proto` (via `av_cdm::pb`, the one place those types are generated —
see `crates/av-lockstep-shim/build.rs`). `HELLO` and `ERROR` are this protocol's own small,
hand-rolled binary layouts: `lockstep.proto` is read-only for this task and has no messages for
either (there is nothing in the gRPC-facing contract that corresponds to a local transport
handshake or a local framing-layer error).

### How a `PortMessage` is carried

**Unchanged, embedded exactly as the proto already carries it.** `PortMessage` is never framed
or encoded on its own — it only ever appears as an element of `LockstepStepRequest.inputs` or
`LockstepStepResponse.outputs`, both `repeated PortMessage` fields, so it is carried as part of
its parent message's own protobuf encoding (a nested length-delimited field), exactly the way
`lockstep.proto` already defines it for the gRPC wire. This shim adds no second, local-only
encoding of `PortMessage` — the byte-pinned test below demonstrates this directly by hand-decoding
a `PortMessage` nested inside a `STEP` frame's `LockstepStepRequest` payload.

### `HELLO` payload (6 bytes)

```
+--------------------+----------------------+
| magic : 4 bytes    | version : u16, LE    |
| "AVL1"             | (currently 1)        |
+--------------------+----------------------+
```

- `magic` is the four raw ASCII bytes `A`, `V`, `L`, `1` (`0x41 0x56 0x4C 0x31`) — chosen to be
  human-legible in a hex dump (`hexdump -C` prints `41 56 4c 31  |AVL1|`), not because the value
  itself carries meaning beyond "this is a lockstep-local HELLO".
- `version` is the protocol version this side speaks, currently `1` ("lockstep-local v1").

**Handshake sequencing:** the shim sends its own `HELLO` immediately after accepting the
connection, then reads the peer's `HELLO`. If the peer's frame is not `HELLO`, or its `magic`
does not match, or its `version` does not equal the shim's own, the shim sends the peer an
`ERROR` frame (see below) naming exactly what disagreed and returns a typed error to its own
caller — it never proceeds to `BIND` with a peer it cannot trust to parse its frames correctly.
This is what "a version-handshake mismatch fails loudly" means concretely.

### `ERROR` payload (`19 + message_len` bytes)

```
+----------+-----------------+-----------------+--------------------+---------------------+
| code:u8  | expected:i64,LE | actual:i64,LE   | message_len:u16,LE | message: UTF-8 bytes |
+----------+-----------------+-----------------+--------------------+---------------------+
   1 byte      8 bytes            8 bytes            2 bytes           message_len bytes
```

`code` values:

| `code` | meaning                       |
|--------|--------------------------------|
| `1`    | version mismatch               |
| `2`    | sequence mismatch               |
| `3`    | a Step was already outstanding |
| `4`    | `reached_tai_ns` mismatch      |
| `5`    | unexpected frame type           |
| `6`    | malformed frame                |
| `7`    | bad `HELLO` magic               |
| other  | other (no specific code)        |

`expected`/`actual` are `0` when a variant has no natural pair of values to name (e.g. bad
`HELLO` magic, whose two byte strings are described in `message` instead of forced into two
`i64`s). `message` is always present and human-readable regardless of `code` — a receiver should
never need to interpret `code` to know *something specific* went wrong, only to match on it
programmatically if it wants to.

## Protocol semantics the shim enforces (`crates/av-lockstep-shim/src/peer_link.rs`)

- **Exactly one outstanding `STEP`.** A second `step()` call before the first has received its
  `STEP_DONE` is rejected *immediately*, before it ever touches the socket, with a typed
  "a Step is already outstanding" error naming both the outstanding and the attempted sequence
  number. This is a hard rejection, not a queue — a caller that ignores `lockstep.proto`'s own
  "one outstanding" contract at the gRPC layer gets the same enforcement here, one layer down.
- **Sequence numbers are checked wherever this proto defines an echo.** `Step` and `Reset`
  responses both carry back the `sequence` their request carried
  (`LockstepStepResponse.sequence`, `LockstepResetResponse.sequence`); a mismatch is a typed
  error naming both the sent and the echoed value.
- **`reached_tai_ns` must equal `until_tai_ns`.** `lockstep.proto`'s own doc comment: "anything
  else is a protocol error." Checked here, so a caller (the kernel) never has to re-derive this
  rule itself against the shim.
- **A dropped or mis-ordered frame is a typed error, never a silent retry.** An unexpected
  `frame_type` where a specific response was expected, a truncated frame, or the peer closing
  the connection mid-exchange all surface as distinct typed errors (`UnexpectedFrameType`,
  `FrameError::Truncated`, `FrameError::PeerClosed`) — this shim never guesses or reparses.

## Transport security (question 155)

The kernel-facing gRPC link is **plaintext only on loopback within one host** -- a container on
the same host counts as that host, which is exactly the M23 binding. Whenever the bound process
is on another host, the shim is fronted by the service-owned nginx mTLS template (question 84),
exactly as the dynamics services are, and the kernel **refuses a non-loopback plaintext endpoint
at load with a typed error**. System OpenSSL through nginx remains the only TLS in the platform;
no crypto-adjacent crate is added to this workspace. The same rule governs the Renode bridge in
M24.

This settles what M23.1 first recorded as an open gap: the vendored `hyper-openssl` 0.10.2
provides only a client-side connector (`hyper_openssl::client::legacy::HttpsConnector`, which is
what `av_grpc::tls` uses) and has no server/acceptor module at all, so a server-side OpenSSL
listener here would have meant either hand-rolling an async acceptor or adding a new dependency.
Neither was done, deliberately. `av_dynamics_service`, this workspace's other Rust-hosted gRPC
server, resolves the identical question the same way.

## M23.4: closing the loop for real -- `apps/adcs`, `apps/shared/ccsds`, `container-entrypoint.sh`

M23.3 wrote the reference ADCS app (`apps/adcs`) but never compiled it against real cFE headers.
M23.4 does: `apps/adcs/CMakeLists.txt` builds `adcs_app.c`/`adcs_control.c`/`adcs_packets.c` into
this image's own `cf/adcs.so`, and `apps/shared/ccsds` is the ONE CCSDS Space Packet codec both
`io_lockstep` and `adcs` now link (M23.2 and M23.3 had each written an independent copy --
`apps/shared/ccsds/inc/ccsds_codec.h`'s own doc comment has the full account). This image's
`ENTRYPOINT` (`container-entrypoint.sh`) now runs both `crates/av-lockstep-shim` and cFS in the
same container, in the right order, so a `BINDING_KIND_CONTAINER` instance dialing this image's
published port reaches a real `LockstepService` server with a real, compiled flight app behind
it -- this is what `crates/av-kernel/tests/drm_attitude_control_cfs.rs` exercises end to end.

Actually compiling and running the loop for the first time surfaced six real defects (a
nonexistent cFE API call, a software-bus message envelope mismatch, a shared static library
silently duplicated per dynamically-loaded app module, an `OS_TimerAdd` interval-counting bug
that fired a callback ~100,000 times per kernel step, a command-type CCSDS packet's MsgId not
being the bare APID, and an unconditional blocking receive that deadlocked before the FSW's own
warm-up completed) -- every one found only by actually running the compiled apps against a real
kernel `Step`, not by static reading of any of them alone. `IMAGE_DIGEST.md`'s own "M23.4
changes" section has the full account of each, with a pointer to its own fix site.

## Example: hand-derived bytes for a `SHUTDOWN` frame

As a worked example matching `crates/av-lockstep-shim/tests/frame_bytes.rs`'s pinned test: a
`SHUTDOWN` frame carrying `LockstepShutdownRequest { run_id: "run-1" }`.

`lockstep.proto`'s `LockstepShutdownRequest.run_id` is field `1`, type `string`:

1. Protobuf tag byte for field 1, wire type 2 (length-delimited): `(1 << 3) | 2 = 0x0A`.
2. Varint length of `"run-1"` (5 ASCII bytes): `0x05`.
3. The bytes of `"run-1"`: `0x72 0x75 0x6E 0x2D 0x31`.
4. Protobuf payload: `0A 05 72 75 6E 2D 31` (7 bytes).
5. `frame_type` for `SHUTDOWN`: `0x08`.
6. `length = 1 (frame_type) + 7 (payload) = 8` → little-endian `u32`: `08 00 00 00`.

Full frame (12 bytes): `08 00 00 00 08 0A 05 72 75 6E 2D 31`.

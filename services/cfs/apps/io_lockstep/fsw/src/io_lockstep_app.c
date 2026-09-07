/* See io_lockstep_app.h for the module doc comment.
 *
 * Per-STEP sequencing, and why it closes the port-boundary determinism question (145) at this
 * app's own boundary: on every `STEP` frame this app (1) decodes each input `PortMessage`'s
 * payload as a CCSDS packet with `ccsds_decode_packet` and transmits it onto the cFE software
 * bus (`CFE_SB_TransmitMsg`) under the port table's declared `msg_id`; (2) calls
 * `psp_lockstep_release_tick(request.until_tai_ns)`, which is the *only* thing that lets
 * `services/cfs/apps/sch_lockstep`'s blocked scheduler tick proceed -- so every input this app
 * places on the bus is visible to every subscriber before the tick that processes it ever
 * fires; (3) waits, once per `LOCKSTEP_PORT_FROM_BUS` table entry, on
 * `CFE_SB_ReceiveBuffer(..., IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS)` against that entry's own pipe,
 * for whatever this tick's own processing produces on it.
 *
 * M23.4 FIX -- found only by actually closing the real loop, not by static reading alone: this
 * used to be `CFE_SB_PEND_FOREVER` unconditionally, on the documented assumption that "every
 * declared output has genuinely been produced by this tick's processing." That assumption is
 * false for a FROM_BUS port whose publisher legitimately has nothing to say yet -- the
 * reference ADCS app's own `ADCS_RunControlLawAndPublish` (mirroring the native `crate::drm::
 * controller::AttitudeControllerModel`'s own "emits nothing before the first measurement
 * arrives" rule) publishes nothing at all until it has received at least one star tracker AND
 * one IMU measurement, which -- in the real closed-loop demo's own shared multi-rate kernel
 * run -- is not yet true on this app's very first `STEP` or two. `CFE_SB_PEND_FOREVER` there
 * deadlocked the whole run on the very first tick that had nothing to report, forever (a real
 * `docker run` against the real demo topology hung indefinitely; `crates/av-kernel/tests/
 * drm_attitude_control_cfs.rs`'s own comparison test is what this a regression here would fail
 * to ever complete). A bounded wait is the fix, not a design compromise: `LockstepStepResponse.
 * outputs` (`lockstep.proto`) is an ordinary `repeated` field with no "exactly one per declared
 * port" cardinality requirement, and the native (Rust) binding already produces exactly this
 * same "sometimes empty this tick" shape for the identical reason (an empty `Outbox` simply
 * routes nothing downstream that step) -- a timed-out FROM_BUS port here is reported the same
 * way: absent from `outputs`, not a protocol error, and (this file's own one-shot pattern) an
 * `EVS` event logs the first occurrence so a run where a port legitimately never produces
 * anything (a genuinely broken FSW) is still visible, never silent.
 *
 * `IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS` (2000 ms) is chosen generously above any real processing
 * latency this app has ever measured end to end (single-digit milliseconds, confirmed against
 * the real image) -- large enough that the timeout is never mistaken for the steady-state
 * (post-warm-up) case, where `ADCS_RunControlLawAndPublish` publishes synchronously in the same
 * dispatch that received the wakeup and is available to this app's own receive within
 * microseconds, so the "which ticks time out" outcome is a deterministic property of this
 * demo's own topology and scheduling (question 145's port-boundary determinism), not a host-
 * scheduling race this bound is wide enough to never trip in practice.
 *
 * This one bounded wait is the only wall-clock-shaped thing in this file, and it does not read
 * a wall clock directly (no `time()`/`gettimeofday()`/`clock_gettime()`/`sleep()`/etc. appear
 * here -- this platform's own `test_psp_lockstep_no_wallclock.py` mechanically scans this exact
 * file for those symbols) -- the bound is enforced entirely inside OSAL's own
 * `CFE_SB_ReceiveBuffer` implementation, the same integer-millisecond timeout parameter shape
 * `cfe_sb.h`'s own public API already documents (`CFE_SB_POLL`/`CFE_SB_PEND_FOREVER` are just
 * the two named extremes of that same parameter).
 */
#include "io_lockstep_app.h"

#include <fcntl.h>
#include <string.h>
#include <unistd.h>

#ifndef AV_CFS_LOCKSTEP_TRANSPORT_UART
#include <sys/socket.h>
#include <sys/un.h>
#else
#include <termios.h>
#endif

#include "cfe.h"

#include "ccsds_codec.h"
#include "io_lockstep_port_table.h"
#include "lockstep_local_framing.h"
#include "lockstep_local_io.h"
#include "lockstep_messages.h"
#include "psp_lockstep.h"

/* M24.3 (docs/open-questions.md question 148): this RTEMS 6 build has no network stack -- RSB
 * did not build librtemsbsd or any networking package for zynqmp_rpu_lock_step, so AF_UNIX/
 * socket() do not exist here. AV_CFS_LOCKSTEP_TRANSPORT_UART (set by
 * services/cfs/build/toolchain-arm-rtems6-zynqmp_rpu_lock_step.cmake) switches `connect_to_shim`
 * to open the BSP's second UART as a plain character device instead -- this is the ONLY
 * transport-specific code in this app (everything below `connect_to_shim` operates on a plain
 * `int fd` via lockstep_local_io.c/lockstep_local_framing.c, unchanged for either transport;
 * question 153's "the shim is written once and reused" holds at the C level too). See
 * M24_3_REPORT.md's "Transport" section for the full rationale (UART over Ethernet: no network
 * stack either way, and this exact BSP's UART already has captured, verified output from
 * M24.2c's hello/ticker runs). This is an unverified design choice -- it has not been run, only
 * cross-compiled; running it is M24.4 (needs the Renode bridge and a UART-speaking shim peer,
 * both out of scope here). */
#ifdef AV_CFS_LOCKSTEP_TRANSPORT_UART
/* `third_party/rtems-container/work/rtems-src/bsps/arm/xilinx-zynqmp-rpu/console/console-config.c`
 * registers exactly two Zynq UART instances as RTEMS termios devices, `/dev/ttyS0`/`/dev/ttyS1`,
 * and links whichever one matches the BSP's debug-console base address (UART0 in this BSP) as
 * the console. UART1 (`/dev/ttyS1`) is therefore free for this bridge. Overridable at build time
 * for a different board revision. */
#ifndef IO_LOCKSTEP_UART_PATH
#define IO_LOCKSTEP_UART_PATH "/dev/ttyS1"
#endif
#else
/* Overridable at build time (-D) for a real deployment; the default matches the path
 * `crates/av-lockstep-shim`'s own Bind-time contract documents for a container binding. */
#ifndef IO_LOCKSTEP_SOCKET_PATH
#define IO_LOCKSTEP_SOCKET_PATH "/var/run/lockstep/lockstep-local.sock"
#endif
#endif

#define IO_LOCKSTEP_PIPE_DEPTH 8

/* M23.4 FIX -- see this file's own top comment for the full account of why this is bounded, not
 * CFE_SB_PEND_FOREVER, and why the bound is wide enough to never fire in the steady-state case. */
#define IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS 2000

typedef struct
{
    CFE_SB_PipeId_t pipe_id;
    bool has_pipe;
} io_lockstep_from_bus_state_t;

static io_lockstep_from_bus_state_t g_from_bus_state[16];

static int connect_to_shim(const char *socket_path);
static bool do_handshake(int fd);
static bool handle_bind(int fd);
static bool handle_step(int fd, const uint8_t *payload, size_t payload_len);
static bool handle_reset(int fd, const uint8_t *payload, size_t payload_len);

void IO_LOCKSTEP_AppMain(void)
{
    uint32 run_status = CFE_ES_RunStatus_APP_RUN;

    CFE_ES_PerfLogEntry(0);

    if (CFE_EVS_Register(NULL, 0, CFE_EVS_EventFilter_BINARY) != CFE_SUCCESS)
    {
        CFE_ES_WriteToSysLog("IO_LOCKSTEP: CFE_EVS_Register failed\n");
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }

    size_t port_count = 0;
    const io_lockstep_port_entry_t *ports = io_lockstep_port_table(&port_count);
    size_t from_bus_count = 0;
    for (size_t i = 0; i < port_count && from_bus_count < (sizeof(g_from_bus_state) / sizeof(g_from_bus_state[0])); ++i)
    {
        if (ports[i].direction != LOCKSTEP_PORT_FROM_BUS) continue;
        CFE_Status_t status = CFE_SB_CreatePipe(&g_from_bus_state[from_bus_count].pipe_id, IO_LOCKSTEP_PIPE_DEPTH, ports[i].port_name);
        if (status != CFE_SUCCESS)
        {
            CFE_EVS_SendEvent(1, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: CFE_SB_CreatePipe(%s) failed: %ld", ports[i].port_name, (long)status);
            CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
            return;
        }
        status = CFE_SB_Subscribe(CFE_SB_ValueToMsgId(ports[i].msg_id), g_from_bus_state[from_bus_count].pipe_id);
        if (status != CFE_SUCCESS)
        {
            CFE_EVS_SendEvent(2, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: CFE_SB_Subscribe(%s) failed: %ld", ports[i].port_name, (long)status);
            CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
            return;
        }
        g_from_bus_state[from_bus_count].has_pipe = true;
        from_bus_count += 1;
    }

#ifdef AV_CFS_LOCKSTEP_TRANSPORT_UART
    int fd = connect_to_shim(IO_LOCKSTEP_UART_PATH);
    if (fd < 0)
    {
        CFE_EVS_SendEvent(3, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: could not open %s", IO_LOCKSTEP_UART_PATH);
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }
#else
    int fd = connect_to_shim(IO_LOCKSTEP_SOCKET_PATH);
    if (fd < 0)
    {
        CFE_EVS_SendEvent(3, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: could not connect to %s", IO_LOCKSTEP_SOCKET_PATH);
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }
#endif

    if (!do_handshake(fd))
    {
        CFE_EVS_SendEvent(4, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: lockstep-local handshake failed");
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }

    if (!handle_bind(fd))
    {
        CFE_EVS_SendEvent(5, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: BIND failed");
        CFE_ES_ExitApp(CFE_ES_RunStatus_APP_ERROR);
        return;
    }

    CFE_EVS_SendEvent(6, CFE_EVS_EventType_INFORMATION, "IO_LOCKSTEP: bound, entering step loop");

    while (CFE_ES_RunLoop(&run_status))
    {
        uint8_t frame_type;
        uint8_t payload[LOCKSTEP_LOCAL_MAX_FRAME_LEN < 4096 ? LOCKSTEP_LOCAL_MAX_FRAME_LEN : 4096];
        size_t payload_len;
        lockstep_io_status_t io_st = lockstep_read_frame(fd, &frame_type, payload, sizeof(payload), &payload_len);
        if (io_st != LOCKSTEP_IO_OK)
        {
            CFE_EVS_SendEvent(7, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: read_frame failed: %d", (int)io_st);
            run_status = CFE_ES_RunStatus_APP_ERROR;
            break;
        }

        if (frame_type == LOCKSTEP_FRAME_STEP)
        {
            if (!handle_step(fd, payload, payload_len))
            {
                run_status = CFE_ES_RunStatus_APP_ERROR;
                break;
            }
        }
        else if (frame_type == LOCKSTEP_FRAME_RESET)
        {
            if (!handle_reset(fd, payload, payload_len))
            {
                run_status = CFE_ES_RunStatus_APP_ERROR;
                break;
            }
        }
        else if (frame_type == LOCKSTEP_FRAME_SHUTDOWN)
        {
            uint8_t resp[8];
            size_t resp_len;
            lockstep_encode_shutdown_response(resp, sizeof(resp), &resp_len);
            lockstep_write_frame(fd, LOCKSTEP_FRAME_SHUTDOWN_ACK, resp, resp_len);
            run_status = CFE_ES_RunStatus_APP_EXIT;
            break;
        }
        else
        {
            CFE_EVS_SendEvent(8, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: unexpected frame_type 0x%02X", frame_type);
            run_status = CFE_ES_RunStatus_APP_ERROR;
            break;
        }
    }

    CFE_ES_ExitApp(run_status);
}

#ifdef AV_CFS_LOCKSTEP_TRANSPORT_UART
/* Opens the bridge UART as a plain character device (see this file's own top comment for why:
 * no network stack in this RTEMS 6 build, so no AF_UNIX). The parameter is a device path, not a
 * socket path, despite the shared name with the posix build's `connect_to_shim` -- kept the same
 * function name/signature deliberately so the caller in IO_LOCKSTEP_AppMain needs no `#ifdef` of
 * its own beyond picking which path constant to pass.
 *
 * M24.4b ROOT-CAUSE FIX (docs/open-questions.md question 148/153): a bare `open()` was never
 * exercised against a real UART before this task -- M24.4 cross-compiled it but never ran it.
 * Running it under Renode surfaced a real defect, root-caused by reading
 * third_party/rtems-container/work/rtems-src/cpukit/libcsupport/src/termios.c directly (not
 * assumed): RTEMS's termios `tty` structure is `calloc`'d
 * (`rtems_termios_open_tty`, that file's own line ~362), so its `rawInBufSemaphoreWait` field --
 * which selects a *blocking* wait (`rtems_binary_semaphore_wait_timed_ticks`) versus a
 * *non-blocking* poll (`rtems_binary_semaphore_try_wait`) in `fillBufferQueue`'s read path -- is
 * zero-initialized to `false` (non-blocking) at `open()` time and is ONLY ever switched to `true`
 * by an explicit `tcsetattr`/`TIOCSETA` call (same file, the "Set default parameters" comment's
 * surrounding `TIOCSETA` case, `rawInBufSemaphoreWait = true` for the `c_cc[VMIN] != 0` branch).
 * A bare `open()` with no `tcsetattr` therefore makes every `read()` on this fd return `0`
 * immediately whenever the raw input queue happens to be empty at that exact instant (which is
 * always true the first time `do_handshake()` reads, since it runs immediately after `open()`,
 * before any peer byte could possibly have arrived) -- and `lockstep_local_io.c`'s `read_all()`
 * (correct for its only previously-tested transport, a connected stream socket, where `read()==0`
 * unambiguously means the peer closed the connection) treats that `0` as
 * `LOCKSTEP_IO_ERR_PEER_CLOSED` and gives up immediately, without ever retrying. This exactly
 * explains M24.4's own finding that the handshake fails identically, byte-for-byte, whether or
 * not a live peer is resending `HELLO` throughout the entire failure window (`M24_4b_REPORT.md`
 * has the full account, including two independent Renode-side probes ruling out an RX-FIFO/wiring
 * explanation first): the guest gives up before any external byte could possibly matter, every
 * single time, regardless of what is on the wire.
 *
 * The fix is to put the port into raw, blocking mode explicitly, exactly as any real UART-backed
 * binary protocol must: disable canonical line editing/echo/signal generation (this protocol has
 * no line terminator and must never have `ISIG`'s control characters intercepted), disable
 * `IXON`/`IXOFF` software flow control and `ICRNL` (this is an arbitrary-binary framed protocol;
 * 0x11/0x13/0x0D are ordinary payload bytes, not flow-control or newline characters, and must
 * never be interpreted or dropped), and set `VMIN=1, VTIME=0` so `read()` blocks for at least one
 * real byte -- which is also precisely the branch of the `TIOCSETA` handler above that sets
 * `rawInBufSemaphoreWait = true`, fixing the non-blocking-read defect at its actual source. A
 * `tcgetattr`/`tcsetattr` failure is treated as a hard connect failure (returns -1, matching this
 * function's existing contract) rather than silently continuing in the broken default mode. Not
 * yet exercised against a real UART *before this task*; this task both root-caused and fixed that
 * gap, and `third_party/renode/M24_4b/handshake_fix_test.py` proves it live (see
 * `M24_4b_REPORT.md`). */
static int connect_to_shim(const char *device_path)
{
    int fd = open(device_path, O_RDWR);
    if (fd < 0) return -1;

    struct termios tio;
    if (tcgetattr(fd, &tio) != 0)
    {
        close(fd);
        return -1;
    }

    /* Raw mode: no line editing, no signal generation, no echo, no flow control or
     * newline/carriage-return translation -- every byte is opaque protocol data. */
    tio.c_iflag &= (tcflag_t) ~(BRKINT | ICRNL | INLCR | IGNCR | ISTRIP | IXON | IXOFF | IMAXBEL);
    tio.c_oflag &= (tcflag_t) ~OPOST;
    tio.c_lflag &= (tcflag_t) ~(ICANON | ECHO | ECHOE | ECHOK | ECHONL | ISIG | IEXTEN);
    tio.c_cflag |= (CS8 | CREAD | CLOCAL);
    tio.c_cc[VMIN] = 1;
    tio.c_cc[VTIME] = 0;

    if (tcsetattr(fd, TCSANOW, &tio) != 0)
    {
        close(fd);
        return -1;
    }

    return fd;
}
#else
/* Connects (never listens -- services/cfs/README.md's own "Who listens, who connects" rule) to
 * the shim's Unix domain socket. Returns a connected fd, or -1 on failure. */
static int connect_to_shim(const char *socket_path)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, socket_path, sizeof(addr.sun_path) - 1);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0)
    {
        close(fd);
        return -1;
    }
    return fd;
}
#endif

static bool do_handshake(int fd)
{
    uint8_t frame_type;
    uint8_t payload[LOCKSTEP_HELLO_PAYLOAD_LEN];
    size_t payload_len;
    /* services/cfs/README.md: "the shim also speaks first" -- read the shim's HELLO before
     * sending ours. */
    if (lockstep_read_frame(fd, &frame_type, payload, sizeof(payload), &payload_len) != LOCKSTEP_IO_OK) return false;
    if (frame_type != LOCKSTEP_FRAME_HELLO) return false;
    char magic[4];
    uint16_t version;
    if (lockstep_decode_hello(payload, payload_len, magic, &version) != LOCKSTEP_FRAMING_OK) return false;
    if (memcmp(magic, LOCKSTEP_HELLO_MAGIC, 4) != 0 || version != LOCKSTEP_PROTOCOL_VERSION) return false;

    uint8_t our_hello[LOCKSTEP_HELLO_PAYLOAD_LEN];
    lockstep_encode_hello(LOCKSTEP_PROTOCOL_VERSION, our_hello);
    return lockstep_write_frame(fd, LOCKSTEP_FRAME_HELLO, our_hello, sizeof(our_hello)) == LOCKSTEP_IO_OK;
}

static bool handle_bind(int fd)
{
    uint8_t frame_type;
    uint8_t payload[512];
    size_t payload_len;
    if (lockstep_read_frame(fd, &frame_type, payload, sizeof(payload), &payload_len) != LOCKSTEP_IO_OK) return false;
    if (frame_type != LOCKSTEP_FRAME_BIND) return false;

    lockstep_bind_request_t req;
    if (lockstep_decode_bind_request(payload, payload_len, &req) != PBMINI_OK) return false;

    psp_lockstep_init(req.start_tai_ns);

    uint8_t resp[256];
    size_t resp_len;
    lockstep_encode_bind_response(true, "io_lockstep/M23.2", "io_lockstep/0.1", "", resp, sizeof(resp), &resp_len);
    return lockstep_write_frame(fd, LOCKSTEP_FRAME_BIND_ACK, resp, resp_len) == LOCKSTEP_IO_OK;
}

static bool handle_step(int fd, const uint8_t *payload, size_t payload_len)
{
    lockstep_step_request_t req;
    if (lockstep_decode_step_request(payload, payload_len, &req) != PBMINI_OK) return false;

    size_t port_count = 0;
    const io_lockstep_port_entry_t *ports = io_lockstep_port_table(&port_count);

    /* Step 1: every declared TO_BUS input this STEP carries is decoded and transmitted before
     * the tick is released -- see this file's own module doc comment. */
    for (size_t i = 0; i < req.input_count; ++i)
    {
        const io_lockstep_port_entry_t *entry = NULL;
        for (size_t p = 0; p < port_count; ++p)
        {
            if (ports[p].direction == LOCKSTEP_PORT_TO_BUS && strcmp(ports[p].port_name, req.inputs[i].port) == 0)
            {
                entry = &ports[p];
                break;
            }
        }
        if (entry == NULL) continue; /* an undeclared port name -- typed refusal is the
                                         router's own job upstream of this app; this app simply
                                         does not forward what it was not configured for */

        double values[CCSDS_MAX_FIELDS];
        ccsds_status_t cst = ccsds_decode_packet(&entry->codec, req.inputs[i].payload, req.inputs[i].payload_len, values);
        if (cst != CCSDS_OK)
        {
            CFE_EVS_SendEvent(10, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: decode(%s) failed: %s", entry->port_name, ccsds_status_str(cst));
            return false;
        }

        /* The packet bytes this app puts on the bus are req.inputs[i].payload verbatim -- the
         * exact bytes the kernel's own codec produced, decoded here only to validate them
         * (never re-encoded before transmission), so "the packets on the software bus are the
         * same bytes the kernel encodes" holds by construction, not by luck. */
        union
        {
            CFE_MSG_Message_t msg;
            uint8_t bytes[LOCKSTEP_MSG_MAX_PAYLOAD];
        } buffer;
        memset(&buffer, 0, sizeof(buffer));
        memcpy(buffer.bytes, req.inputs[i].payload, req.inputs[i].payload_len);
        CFE_Status_t status = CFE_SB_TransmitMsg(&buffer.msg, true);
        if (status != CFE_SUCCESS)
        {
            CFE_EVS_SendEvent(11, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: TransmitMsg(%s) failed: %ld", entry->port_name, (long)status);
            return false;
        }
    }

    /* Step 2: release the tick -- only now does the scheduler (services/cfs/apps/sch_lockstep)
     * unblock and dispatch this tick's schedule table entries. */
    if (psp_lockstep_release_tick(req.until_tai_ns) != 0)
    {
        CFE_EVS_SendEvent(12, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: release_tick(%lld) rejected (non-increasing)", (long long)req.until_tai_ns);
        return false;
    }
    {
        static bool s_logged_first_release = false;
        if (!s_logged_first_release)
        {
            s_logged_first_release = true;
            CFE_EVS_SendEvent(14, CFE_EVS_EventType_INFORMATION, "IO_LOCKSTEP: first release_tick(%lld) accepted", (long long)req.until_tai_ns);
        }
    }

    /* Step 3: wait, once per declared FROM_BUS port, up to IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS for
     * this tick to have produced that output -- see this file's own top comment for why this is
     * bounded rather than CFE_SB_PEND_FOREVER, and why a timeout is a legitimate "nothing to
     * report this tick" outcome (absent from `outputs`), not a protocol error. */
    lockstep_port_message_t outputs[16];
    size_t output_count = 0;
    size_t from_bus_index = 0;
    for (size_t p = 0; p < port_count; ++p)
    {
        if (ports[p].direction != LOCKSTEP_PORT_FROM_BUS) continue;
        CFE_SB_Buffer_t *buf_ptr = NULL;
        CFE_Status_t status = CFE_SB_ReceiveBuffer(&buf_ptr, g_from_bus_state[from_bus_index].pipe_id, IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS);
        from_bus_index += 1;
        if (status == CFE_SB_TIME_OUT)
        {
            /* One-shot visible confirmation this legitimate ("nothing to report yet") path was
             * actually taken, not silently -- see this file's own top comment. Every occurrence
             * still costs nothing downstream: `output_count` simply is not incremented for this
             * port this tick, exactly mirroring the native binding's own "an empty Outbox routes
             * nothing" behavior. */
            static bool s_logged_first_timeout = false;
            if (!s_logged_first_timeout)
            {
                s_logged_first_timeout = true;
                CFE_EVS_SendEvent(17, CFE_EVS_EventType_INFORMATION, "IO_LOCKSTEP: %s produced no output within %d ms this tick (expected before the FSW's own warm-up completes)", ports[p].port_name, IO_LOCKSTEP_FROM_BUS_TIMEOUT_MS);
            }
            continue;
        }
        if (status != CFE_SUCCESS || buf_ptr == NULL)
        {
            CFE_EVS_SendEvent(13, CFE_EVS_EventType_ERROR, "IO_LOCKSTEP: ReceiveBuffer(%s) failed: %ld", ports[p].port_name, (long)status);
            return false;
        }
        {
            /* One-shot visible confirmation that a real FROM_BUS output (e.g. ADCS's own
             * wheel-torque command) reached this app at least once -- this exact path was
             * silently deadlocked before the M23.4 fixes this file's own top comment and
             * io_lockstep_port_table.c's own CCSDS_V1_MSGID both describe. */
            static bool s_logged_first_recv = false;
            if (!s_logged_first_recv)
            {
                s_logged_first_recv = true;
                CFE_EVS_SendEvent(15, CFE_EVS_EventType_INFORMATION, "IO_LOCKSTEP: first FROM_BUS output received on %s", ports[p].port_name);
            }
        }
        CFE_MSG_Size_t msg_size = 0;
        CFE_MSG_GetSize(&buf_ptr->Msg, &msg_size);
        if (output_count < (sizeof(outputs) / sizeof(outputs[0])) && (size_t)msg_size <= LOCKSTEP_MSG_MAX_PAYLOAD)
        {
            strncpy(outputs[output_count].port, ports[p].port_name, LOCKSTEP_MSG_MAX_STRING - 1);
            outputs[output_count].port[LOCKSTEP_MSG_MAX_STRING - 1] = '\0';
            outputs[output_count].tai_ns = req.until_tai_ns;
            memcpy(outputs[output_count].payload, (const uint8_t *)&buf_ptr->Msg, (size_t)msg_size);
            outputs[output_count].payload_len = (size_t)msg_size;
            output_count += 1;
        }
    }

    uint8_t resp[LOCKSTEP_MSG_MAX_PAYLOAD * 4];
    size_t resp_len;
    if (lockstep_encode_step_response(req.sequence, req.until_tai_ns, outputs, output_count, resp, sizeof(resp), &resp_len) != PBMINI_OK) return false;
    return lockstep_write_frame(fd, LOCKSTEP_FRAME_STEP_DONE, resp, resp_len) == LOCKSTEP_IO_OK;
}

static bool handle_reset(int fd, const uint8_t *payload, size_t payload_len)
{
    lockstep_reset_request_t req;
    if (lockstep_decode_reset_request(payload, payload_len, &req) != PBMINI_OK) return false;
    psp_lockstep_init(req.tai_ns);
    uint8_t resp[16];
    size_t resp_len;
    lockstep_encode_reset_response(req.sequence, resp, sizeof(resp), &resp_len);
    return lockstep_write_frame(fd, LOCKSTEP_FRAME_RESET_ACK, resp, resp_len) == LOCKSTEP_IO_OK;
}

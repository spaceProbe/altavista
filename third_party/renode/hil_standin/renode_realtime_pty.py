#!/usr/bin/env python3
"""hilprep-6: the stand-in HIL guest -- the real reproducible RTEMS ELF in Renode, free-running
(`start`, never `RunFor`), with UART1 on a host pseudo-terminal.

**This is an emulator stand-in: Renode 1.16.1 emulating the ZynqMP RPU, not the ZCU104.** Nothing
this script reports is a board result. README.md (this directory) has the measurements and the
reasoning behind each choice below.

What it does:

1. Starts Renode with the repository's `zynqmp.repl`, the ELF on `rpu0` and UART0 into a log file.
   UART1 is bridged to a host pseudo-terminal made here with `openpty`: its slave (`/dev/ttysNNN`,
   reported in `--ready-file`, symlinked at `--pty-path`) is what `av-edge-board --port-device
   /dev/ttysNNN@115200` opens. The relay between that pty and UART1:
   - guest to host (`--tx-path hook`, default): a Renode Python hook on UART1's `CharReceived`
     event (`third_party/renode/M24_4f/uart_tx_bridge_hook.py`) forwards every transmitted byte to
     a socket this script owns.
   - host to guest (`--rx-path paced`, default): `uart_rx_paced_hook.py` writes the bytes into the
     UART at `--baud` in VIRTUAL time (a recurring `Machine.ScheduleAction`), because bytes
     arriving at the wall-clock rate overran the guest's 256-byte termios ring about half the time
     when Renode ran below real time. `--rx-path pty` instead uses Renode's own pty terminal
     (`emulation CreateUartPtyTerminal`, `connector Connect`), as it arrives.
   Renode's pty terminal is attached in both modes: the guest's transmissions through it are
   drained and counted (they agree with the hook's, `stats.relay`), and in `pty` mode it carries
   the host-to-guest bytes.
2. `start`s the machine and lets it run. Renode 1.16.1 has no setting that makes virtual time
   follow the wall clock: with `AdvanceImmediately` false (the default) an IDLE guest (WFI) is
   held to the quantum's wall time, so it tracks real time when the host keeps up, and a BUSY
   guest runs as fast or as slow as the host executes it; it can run ahead (measured up to 2x
   while catching up) or far behind (0.01 to 0.6 on a loaded host). `--pacing pause` (default)
   adds the controller that keeps it at or below 1:1: it polls `emulation GetTimeSourceInfo`,
   tracks a reference line that advances with the wall clock but never more than `--credit-ms`
   ahead of the guest (no banked backlog to catch up at more than 1:1), and when the guest is
   within `--lead-ms` of the line it pauses the emulation (`emulation PauseAll`) until it is
   `--resume-ms` behind, then `StartAll`. `--pacing none` leaves Renode alone.
3. Waits until the guest can take the edge service's one HELLO: UART1's receiver enabled (the
   `Control` register's RXEN, the signal `renode_bridge.py` waits for) AND the cFE console on UART0
   reporting OPERATIONAL. Then writes `--ready-file` and prints `READY`.
4. Keeps sampling the virtual-to-wall ratio until SIGTERM / SIGINT / `--stop-file`, then writes
   `--stats-file` (also every ~2 s) and quits Renode. A file `mark.<name>` created in `--workdir`
   makes it record the wall and virtual time at which it saw it (`stats.marks`), so a caller can
   measure the ratio over exactly its own window; `probe.request` makes it read UART1's registers,
   the PC and the hook counters into `probe.<n>.json` (diagnosis of a stalled step).

It reuses `third_party/renode/M24_4b/renode_bridge.py`'s `MonitorClient` (tagged commands, the
prompt-after-echo completion rule, LF-terminated commands: question 240) for everything off the hot
path, and `fast_cmd` below (the same rule without the client's 0.15 s settle read, ~165 ms per
command) for the pacer's polls.
"""
import argparse
import glob
import json
import os
import pty
import re
import select
import signal
import socket
import sys
import threading
import time
import tty

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK_SCRIPT = os.path.join(HERE, "..", "M24_4f", "uart_tx_bridge_hook.py")
RX_HOOK_SCRIPT = os.path.join(HERE, "uart_rx_paced_hook.py")
sys.path.insert(0, os.path.join(HERE, "..", "M24_4b"))
sys.path.insert(0, os.path.join(HERE, "..", "..", ".."))  # the repo root, for altavista.pb (imported by renode_bridge)
import renode_bridge as rb  # noqa: E402

UART1_CONTROL = 0xFF010000
CONTROL_RXEN = 1 << 2
VT_RE = re.compile(r"Elapsed Virtual Time:\s*(\d+):(\d+):(\d+)\.(\d+)")

_stop = False


def _on_signal(_sig, _frm):
    global _stop
    _stop = True


def parse_virtual_s(info: str) -> float:
    m = VT_RE.search(info)
    if not m:
        raise RuntimeError(f"no 'Elapsed Virtual Time' in {info!r}")
    h, mi, s, frac = m.groups()
    return int(h) * 3600 + int(mi) * 60 + int(s) + int(frac) / 10 ** len(frac)


def fast_cmd(mon, line: str, timeout: float = 15.0) -> str:
    """One monitor command with no idle-gap grace drain (`MonitorClient.cmd` adds a 0.15 s settle
    read after every reply, ~165 ms per command measured, far too slow for a pacer that must react
    within milliseconds). Same completion rule as `MonitorClient._read_reply_for` -- this command's
    own echo, then the monitor's prompt after it -- but the prompt is accepted only once COMPLETE
    (`(<name>) ` plus its colour reset, `MonitorClient.PROMPT_TAIL`), so nothing is left on the
    socket for the next command's stray-byte check (`MonitorDesyncError`). Holds the client's lock,
    so it serialises with every other caller exactly as `cmd` does."""
    with mon.lock:
        mon._seq += 1
        tag = mon._seq
        mon._check_no_stray_bytes(tag)
        mon.send(line)
        deadline = time.monotonic() + timeout
        end = (mon.prompt + " ").encode() + b"\x1b[0m"
        buf = b""
        while True:
            if line in rb.clean(buf):
                after = rb.clean(buf).split(line, 1)[1]
                if mon.prompt in after and buf.endswith(end):
                    return rb.clean(buf)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"[cmd#{tag}] {line!r}: no complete prompt within {timeout} s (have {rb.clean(buf)!r})")
            mon.sock.settimeout(max(0.005, min(0.5, remaining)))
            try:
                data = mon.sock.recv(65536)
            except socket.timeout:
                continue
            if not data:
                raise rb.MonitorDesyncError(f"[cmd#{tag}] {line!r}: monitor closed mid-reply")
            buf += data


class Relay:
    """The host pty (the one the edge service opens) <-> UART1.

    host -> guest: bytes the edge service writes arrive on the pty master and are written to the
    slave of Renode's own pty terminal on UART1 (`renode_uart1.pty`), which Renode feeds into the
    UART's receive path while the machine runs.
    guest -> host: in `hook` mode, the bytes of the CharReceived hook socket are written to the pty
    master; Renode's pty terminal also writes the guest's transmissions to its own pty, and a
    drain thread reads those so Renode never blocks on them (counted, and in `pty` mode they ARE the
    guest-to-host path). The counters let a run show whether the two paths agree."""

    def __init__(self, master_fd, renode_pty_path, tx_path, tap_file=None):
        self.tap = open(tap_file, "a", buffering=1) if tap_file else None
        self.tap_lock = threading.Lock()
        self.t0 = time.monotonic()
        self.master_fd = master_fd
        self.tx_path = tx_path
        self.hook_conn = None
        self.rx_conn = None
        self.rx_path = "pty"
        self.q_fd = os.open(renode_pty_path, os.O_RDWR | os.O_NOCTTY)
        tty.setraw(self.q_fd)
        self.counters = {"host_to_guest_bytes": 0, "guest_to_host_hook_bytes": 0, "guest_to_host_renode_pty_bytes": 0, "errors": []}
        self._stop = threading.Event()
        self._threads = []

    def _tap(self, direction, data):
        if self.tap:
            with self.tap_lock:
                self.tap.write(json.dumps({"t": round(time.monotonic() - self.t0, 4), "dir": direction, "len": len(data), "hex": data.hex()}) + "\n")

    def _write_all(self, fd, data):
        view = memoryview(data)
        while view and not self._stop.is_set():
            try:
                n = os.write(fd, view)
            except BlockingIOError:
                time.sleep(0.001)
                continue
            view = view[n:]

    def _rx(self):
        try:
            while not self._stop.is_set():
                r, _, _ = select.select([self.master_fd], [], [], 0.1)
                if r:
                    data = os.read(self.master_fd, 4096)
                    if data:
                        self.counters["host_to_guest_bytes"] += len(data)
                        self._tap("host->guest", data)
                        if self.rx_conn is not None:
                            self.rx_conn.sendall(data)
                        else:
                            self._write_all(self.q_fd, data)
        except OSError as e:
            if not self._stop.is_set():
                self.counters["errors"].append(f"rx: {e!r}")

    def _renode_pty_drain(self):
        try:
            while not self._stop.is_set():
                r, _, _ = select.select([self.q_fd], [], [], 0.1)
                if r:
                    data = os.read(self.q_fd, 4096)
                    if data:
                        self.counters["guest_to_host_renode_pty_bytes"] += len(data)
                        if self.tx_path == "pty":
                            self._write_all(self.master_fd, data)
        except OSError as e:
            if not self._stop.is_set():
                self.counters["errors"].append(f"renode pty: {e!r}")

    def _hook(self):
        try:
            while not self._stop.is_set():
                r, _, _ = select.select([self.hook_conn], [], [], 0.1)
                if r:
                    data = self.hook_conn.recv(4096)
                    if not data:
                        return
                    self.counters["guest_to_host_hook_bytes"] += len(data)
                    self._tap("guest->host", data)
                    self._write_all(self.master_fd, data)
        except OSError as e:
            if not self._stop.is_set():
                self.counters["errors"].append(f"hook: {e!r}")

    def start(self):
        targets = [self._rx, self._renode_pty_drain] + ([self._hook] if self.hook_conn is not None else [])
        for t in targets:
            th = threading.Thread(target=t, daemon=True)
            th.start()
            self._threads.append(th)

    def stop(self):
        self._stop.set()
        for th in self._threads:
            th.join(timeout=2.0)


def write_json_atomic(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(obj, f, indent=1)
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--renode-bin", required=True)
    ap.add_argument("--platform", required=True)
    ap.add_argument("--elf", required=True)
    ap.add_argument("--workdir", required=True, help="scratch directory (resc, logs, pty link)")
    ap.add_argument("--uart0-log", required=True)
    ap.add_argument("--pty-path", required=True, help="a symlink to the pty slave the edge service opens (the real device is /dev/ttysNNN, in the ready file)")
    ap.add_argument("--tap-file", default=None, help="append one JSON line per chunk the relay carries (time, direction, length, hex): the evidence of what crossed UART1, with timing")
    ap.add_argument("--rx-path", choices=["paced", "pty"], default="paced", help="host-to-guest bytes: delivered at --baud in VIRTUAL time by uart_rx_paced_hook.py (default), or as they arrive through Renode's pty terminal (wall-clock; stalls the guest: README.md)")
    ap.add_argument("--baud", type=int, default=115200, help="the line rate the paced receive path emulates, in virtual time")
    ap.add_argument("--tx-path", choices=["hook", "pty"], default="hook", help="guest-to-host bytes: a CharReceived hook on UART1 (default) or Renode's own pty terminal (loses bytes: README.md)")
    ap.add_argument("--monitor-port", type=int, required=True)
    ap.add_argument("--ready-file", required=True)
    ap.add_argument("--stats-file", required=True)
    ap.add_argument("--stop-file", default=None)
    ap.add_argument("--run-s", type=float, default=0.0, help="experiments: stop by itself this long after READY (0: run until stopped)")
    ap.add_argument("--boot-timeout-s", type=float, default=120.0)
    ap.add_argument("--pacing", choices=["pause", "none"], default="pause")
    ap.add_argument("--lead-ms", type=float, default=10.0, help="pause when virtual time is within this of the 1:1 reference line")
    ap.add_argument("--resume-ms", type=float, default=40.0, help="resume when the guest is this far behind the reference line")
    ap.add_argument("--credit-ms", type=float, default=100.0, help="the most the reference line may lead the guest (the backlog a slow guest may catch up at once)")
    ap.add_argument("--poll-ms", type=float, default=10.0)
    ap.add_argument("--advance-immediately", choices=["default", "true", "false"], default="default")
    ap.add_argument("--quantum-us", type=int, default=0, help="0 keeps Renode's default (100 us)")
    args = ap.parse_args()

    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGINT, _on_signal)

    os.makedirs(args.workdir, exist_ok=True)
    for p in (args.pty_path, args.ready_file, args.stats_file):
        try:
            os.unlink(p)
        except FileNotFoundError:
            pass
    machine = f"av-hil-standin-{os.getpid()}"
    resc = os.path.join(args.workdir, f"hil_standin.{os.getpid()}.resc")
    renode_log = os.path.join(args.workdir, "renode_monitor.log")
    # The same boot script shape as renode_bridge.py (platform, UART0 file backend, the ELF on rpu0
    # with the other cores halted). UART1 is on a Renode pty terminal (`true` is its force-create
    # flag) that this script's relay uses for host-to-guest bytes, and, in `--tx-path hook` mode, the
    # CharReceived hook of renode_bridge.py (M24.4f) carries guest-to-host bytes to a socket this
    # script owns. The pty the EDGE SERVICE opens is a second pair, made here with openpty.
    renode_pty = os.path.join(args.workdir, "renode_uart1.pty")
    hook_srv = None
    hook_lines = ""
    if args.tx_path == "hook":
        hook_srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        hook_srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        hook_srv.bind(("127.0.0.1", 0))
        hook_srv.listen(1)
        hook_lines = f"include @{HOOK_SCRIPT}\nsetup_uart_tx_bridge sysbus.uart1 {hook_srv.getsockname()[1]}"
    rx_srv = None
    if args.rx_path == "paced":
        rx_srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        rx_srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        rx_srv.bind(("127.0.0.1", 0))
        rx_srv.listen(1)
        hook_lines += f"\ninclude @{RX_HOOK_SCRIPT}\nsetup_uart_rx_paced sysbus.uart1 {rx_srv.getsockname()[1]} {args.baud}"
    master_fd, slave_fd = pty.openpty()
    tty.setraw(slave_fd)  # raw both ways; the edge service sets 8N1 and the baud itself
    slave_path = os.ttyname(slave_fd)
    os.symlink(slave_path, args.pty_path)
    with open(resc, "w") as f:
        f.write(f'''# generated by renode_realtime_pty.py (pid {os.getpid()})
:name: {machine}
using sysbus
using sysbus.cluster0
using sysbus.cluster1
mach create "{machine}"
machine LoadPlatformDescription @{args.platform}
uart0 CreateFileBackend @{args.uart0_log} true
emulation CreateUartPtyTerminal "uart1pty" "{renode_pty}" true
connector Connect sysbus.uart1 uart1pty
{hook_lines}
macro reset
"""
    cluster0 ForEach IsHalted true
    cluster1 ForEach IsHalted true
    rpu0 IsHalted false
    sysbus LoadELF @{args.elf} cpu=rpu0
"""
runMacro $reset
''')

    import subprocess
    logf = open(renode_log, "wb")
    proc = subprocess.Popen([args.renode_bin, "--disable-gui", "--hide-log", "-P", str(args.monitor_port)], stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT)
    mon = None
    relay = None
    stats = {
        "stand_in": "Renode 1.16.1 emulating the ZynqMP RPU, not the ZCU104",
        "pacing": args.pacing, "lead_ms": args.lead_ms, "resume_ms": args.resume_ms, "credit_ms": args.credit_ms, "poll_ms": args.poll_ms,
        "rx_path": args.rx_path, "tx_path": args.tx_path, "baud": args.baud, "advance_immediately": args.advance_immediately, "quantum_us": args.quantum_us,
    }
    try:
        sock = rb.wait_for_port("127.0.0.1", args.monitor_port, time.monotonic() + 30.0)
        mon = rb.MonitorClient(sock)
        mon.drain_startup_banner()
        mon.set_prompt(machine)
        mon.cmd(f"include @{resc}", timeout=60.0)
        if not os.path.lexists(renode_pty):
            raise RuntimeError(f"Renode did not create the pty link {renode_pty}")
        relay = Relay(master_fd, os.path.realpath(renode_pty), args.tx_path, args.tap_file)
        relay.rx_path = args.rx_path
        if hook_srv is not None:
            hook_srv.settimeout(60.0)
            conn, _addr = hook_srv.accept()
            relay.hook_conn = conn
        if rx_srv is not None:
            rx_srv.settimeout(60.0)
            relay.rx_conn, _addr = rx_srv.accept()
        relay.start()
        stats["relay"] = relay.counters
        if args.advance_immediately != "default":
            mon.cmd(f"emulation SetAdvanceImmediately {args.advance_immediately}")
        if args.quantum_us:
            mon.cmd(f'emulation SetGlobalQuantum "{args.quantum_us / 1e6:.9f}"')
        stats["time_source_before_start"] = rb.clean(mon.cmd("emulation GetTimeSourceInfo"))

        def virtual_s():
            return parse_virtual_s(fast_cmd(mon, "emulation GetTimeSourceInfo"))

        mon.cmd("start", timeout=30.0)
        t0 = time.monotonic()
        samples = []  # (wall_s, virtual_s) about once a second
        marks = {}  # name -> {"wall_s", "virtual_s"}: the first loop pass that saw <workdir>/mark.<name>
        max_ahead_wall_s = float("-inf")  # max over samples of (virtual elapsed - wall elapsed since start)
        max_ahead_ref_s = float("-inf")  # max over samples of (virtual elapsed - the 1:1 reference line)
        pauses = 0
        paused_wall_s = 0.0
        polls = 0
        ready_info = None
        last_sample = -1.0
        last_stats = 0.0
        poll = args.poll_ms / 1000.0
        lead = args.lead_ms / 1000.0
        resume = args.resume_ms / 1000.0
        credit = args.credit_ms / 1000.0
        # The 1:1 reference line R: the virtual time the guest may have reached. It advances with the
        # wall clock, but never more than `credit` ahead of where the guest actually is, so a guest
        # that was slow (the boot runs at ~0.1-0.7 x on this host) cannot bank a backlog and then
        # run faster than 1:1 for seconds to catch up: over any window longer than `credit`, virtual
        # time advances at most 1:1 (plus credit / window).
        ref = 0.0
        last_ref_t = t0
        v = 0.0

        def snapshot(final=False):
            wall = time.monotonic() - t0
            out = dict(stats)
            out.update({
                "final": final, "wall_s": wall, "virtual_s": v, "ratio_total": (v / wall) if wall > 0 else None,
                "max_virtual_ahead_of_wall_ms": None if max_ahead_wall_s == float("-inf") else max_ahead_wall_s * 1000.0,
                "max_virtual_ahead_of_reference_ms": None if max_ahead_ref_s == float("-inf") else max_ahead_ref_s * 1000.0,
                "pauses": pauses, "paused_wall_s": paused_wall_s, "polls": polls, "ready": ready_info,
                "marks": marks, "samples_1s": samples[-4000:],
            })
            if ready_info:
                dw = wall - ready_info["wall_s"]
                dv = v - ready_info["virtual_s"]
                out["since_ready"] = {"wall_s": dw, "virtual_s": dv, "ratio": (dv / dw) if dw > 0 else None}
            return out

        def advance_ref(v_now):
            nonlocal ref, last_ref_t
            t = time.monotonic()
            ref += t - last_ref_t
            last_ref_t = t
            ref = min(ref, v_now + credit)

        while not _stop:
            if args.stop_file and os.path.exists(args.stop_file):
                break
            wall = time.monotonic() - t0
            if args.run_s and ready_info is not None and wall - ready_info["wall_s"] >= args.run_s:
                break
            if proc.poll() is not None:
                raise RuntimeError(f"Renode exited early ({proc.returncode}); see {renode_log}")
            v = virtual_s()
            polls += 1
            advance_ref(v)
            wall = time.monotonic() - t0
            max_ahead_wall_s = max(max_ahead_wall_s, v - wall)
            max_ahead_ref_s = max(max_ahead_ref_s, v - ref)
            if wall - last_sample >= 1.0:
                samples.append((round(wall, 3), round(v, 4)))
                last_sample = wall
            for name in os.listdir(args.workdir):
                if name.startswith("mark.") and name[5:] not in marks:
                    marks[name[5:]] = {"wall_s": wall, "virtual_s": v}
                elif name == "probe.request":
                    # Diagnosis on demand (a stalled step): UART1's registers and the RPU's PC, read
                    # through the monitor while the machine runs.
                    os.unlink(os.path.join(args.workdir, name))
                    probe = {"wall_s": wall, "virtual_s": v, "relay": dict(relay.counters)}
                    for reg, off in (("CR", 0x00), ("MR", 0x04), ("IMR", 0x10), ("ISR", 0x14), ("RXTOUT", 0x1C), ("RXWM", 0x20), ("SR", 0x2C)):
                        probe[reg] = hex(int(rb.HEX_RE.findall(fast_cmd(mon, f"sysbus ReadDoubleWord {hex(UART1_CONTROL + off)}"))[-1], 16))
                    probe["PC"] = [rb.HEX_RE.findall(fast_cmd(mon, "rpu0 PC"))[-1] for _ in range(5)]
                    probe["uart1_history_buffer"] = fast_cmd(mon, "sysbus.uart1 DumpHistoryBuffer")[-3000:]
                    probe["uart1_buffer_state"] = fast_cmd(mon, "sysbus.uart1 BufferState")[:200]
                    if args.rx_path == "paced":
                        probe["rx_paced_stats"] = fast_cmd(mon, "uart_rx_paced_stats")[-400:]
                    if args.tx_path == "hook":
                        probe["hook_stats"] = fast_cmd(mon, "uart_tx_bridge_stats")[-400:]
                    n = len(glob.glob(os.path.join(args.workdir, "probe.*.json")))
                    write_json_atomic(os.path.join(args.workdir, f"probe.{n}.json"), probe)
            if ready_info is None:
                if wall > args.boot_timeout_s:
                    raise TimeoutError(f"the guest was not ready within {args.boot_timeout_s} s (see {args.uart0_log})")
                ctrl = int(rb.HEX_RE.findall(fast_cmd(mon, f"sysbus ReadDoubleWord {hex(UART1_CONTROL)}"))[-1], 16)
                try:
                    operational = b"entering OPERATIONAL state" in open(args.uart0_log, "rb").read()
                except FileNotFoundError:
                    operational = False
                if ctrl & CONTROL_RXEN and operational:
                    ready_info = {"wall_s": wall, "virtual_s": v, "uart1_control": hex(ctrl)}
                    write_json_atomic(args.ready_file, {
                        "pty_path": args.pty_path, "pty_slave": slave_path, "tx_path": args.tx_path, "rx_path": args.rx_path, "baud": args.baud, "helper_pid": os.getpid(),
                        "renode_pid": proc.pid, "monitor_port": args.monitor_port, "uart0_log": args.uart0_log,
                        "ready": ready_info, "pacing": args.pacing, "stand_in": stats["stand_in"],
                    })
                    print(f"READY pty={args.pty_path} slave={slave_path} tx_path={args.tx_path} after {wall:.2f} s wall / {v:.2f} s virtual", flush=True)
            if args.pacing == "pause" and v > ref - lead:
                fast_cmd(mon, "emulation PauseAll")
                pauses += 1
                tp = time.monotonic()
                # Paused, virtual time stands still while the reference line moves on: resume once the
                # guest is `resume` behind it.
                v = virtual_s()
                while not _stop:
                    advance_ref(v)
                    if ref - v >= resume:
                        break
                    time.sleep(0.002)
                fast_cmd(mon, "emulation StartAll")
                paused_wall_s += time.monotonic() - tp
            else:
                time.sleep(poll)
            if wall - last_stats >= 2.0:
                write_json_atomic(args.stats_file, snapshot())
                last_stats = wall
        v = virtual_s() if mon else v
        write_json_atomic(args.stats_file, snapshot(final=True))
        s = snapshot(final=True)
        print(f"STATS ratio_total={s['ratio_total']:.4f} since_ready={s.get('since_ready', {}).get('ratio')} max_ahead_of_wall_ms={s['max_virtual_ahead_of_wall_ms']:.1f} max_ahead_of_reference_ms={s['max_virtual_ahead_of_reference_ms']:.1f} pauses={pauses} polls={polls}", flush=True)
    finally:
        if relay is not None:
            relay.stop()
        if mon is not None:
            try:
                mon.send("quit")
            except OSError:
                pass
        try:
            proc.wait(timeout=15.0)
        except Exception:
            proc.kill()
            proc.wait(timeout=15.0)


if __name__ == "__main__":
    main()

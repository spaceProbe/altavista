#!/usr/bin/env python3
"""M24.4d -- direct evidence for why `CreateServerSocketTerminal` reports success (a clean
`include` reply, no error anywhere in the monitor log) without a listener ever accepting a
connection.

Drives the *exact* monitor sequence `renode_bridge.py::RenodeBridge.start()` uses (same commands,
same `.resc` template, same idle-gap-based `read_until_idle` framing) but instruments it two ways
the production bridge does not:

1. After the `include` command's own `read_until_idle`-framed reply comes back "clean", keeps
   reading the SAME monitor socket for a much longer window (30s) to see whether any further
   bytes -- e.g. a delayed error the 0.3s idle-gap cutoff already walked away from -- ever arrive.
2. Runs a background poller that repeatedly asks the OS itself (`lsof -nP -p <renode_pid>`, not
   the monitor protocol and not `wait_for_port`'s own TCP-connect probe) whether a LISTEN socket
   for the target uart port exists yet, and logs the real wall-clock offset (from the moment the
   `include` command was sent) at which that first becomes true -- or never becomes true within
   the probe's own 60s budget.

Run N iterations back to back (one Renode process per iteration, freshly booted, ports freed
between iterations) to characterise whether this is a hard failure or a race.
"""
import json
import re
import socket
import struct
import subprocess
import sys
import threading
import time

REPO = "/Users/probe/code/AltaVista"
RENODE_BIN = f"{REPO}/third_party/renode/renode-1.16.1-osx-arm64/Renode.app/Contents/MacOS/renode"
PLATFORM = f"{REPO}/third_party/renode/platforms/cpus/zynqmp.repl"
ELF = f"{REPO}/third_party/cfs/build-rtems_zynqmp/exe/cpu1/core-cpu1.exe"
SCRATCH = f"{REPO}/third_party/renode/M24_4d"

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


def clean(raw: bytes) -> str:
    return ANSI_RE.sub(b"", raw).decode(errors="replace").strip()


def free_tcp_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class MonitorClient:
    def __init__(self, sock):
        self.sock = sock
        self.lock = threading.Lock()

    def send(self, line):
        self.sock.sendall((line + "\r\n").encode())

    def read_until_idle(self, idle_gap=0.3, overall_timeout=30.0):
        chunks = []
        deadline = time.monotonic() + overall_timeout
        self.sock.settimeout(idle_gap)
        while time.monotonic() < deadline:
            try:
                data = self.sock.recv(65536)
                if not data:
                    break
                chunks.append((time.monotonic(), data))
            except socket.timeout:
                if chunks:
                    break
                continue
        return chunks

    def cmd(self, line, idle_gap=0.3, timeout=30.0):
        with self.lock:
            t_sent = time.monotonic()
            self.send(line)
            chunks = self.read_until_idle(idle_gap=idle_gap, overall_timeout=timeout)
            return t_sent, chunks


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            s = socket.create_connection((host, port), timeout=1.0)
            return s
        except OSError:
            time.sleep(0.1)
    return None


def lsof_listen_ports(pid):
    """Returns the set of (proto, local_addr) strings for LISTEN sockets held by `pid`, via a
    real `lsof` invocation against the live process -- not the monitor protocol, not inference."""
    try:
        out = subprocess.run(
            ["lsof", "-nP", "-p", str(pid), "-a", "-iTCP"],
            capture_output=True, text=True, timeout=5.0,
        ).stdout
    except Exception as e:
        return None, str(e)
    lines = [l for l in out.splitlines() if "LISTEN" in l]
    return lines, out


def poll_lsof_for_port(pid, port, stop_event, samples, budget_s=60.0):
    """Background thread: every 0.1s, asks lsof for this pid's LISTEN sockets and records
    whether `port` appears, with a wall-clock timestamp, until `stop_event` is set or the
    budget elapses."""
    t_start = time.monotonic()
    seen_first_listen_at = None
    while not stop_event.is_set() and (time.monotonic() - t_start) < budget_s:
        lines, raw = lsof_listen_ports(pid)
        now = time.monotonic()
        hit = bool(lines) and any(f":{port} " in l or f":{port}\n" in l or l.rstrip().endswith(f":{port} (LISTEN)") for l in lines)
        # more robust: just check the port number appears anywhere in a LISTEN line
        hit = hit or (lines and any(re.search(rf"[:\.]{port}\b", l) for l in lines))
        samples.append({"t": now - t_start, "any_listen_lines": lines, "hit": hit})
        if hit and seen_first_listen_at is None:
            seen_first_listen_at = now - t_start
        time.sleep(0.1)
    return seen_first_listen_at


def run_one_iteration(idx, results):
    monitor_port = free_tcp_port()
    uart_port = free_tcp_port()
    renode_log_path = f"{SCRATCH}/race_probe_{idx}_renode_stdout.log"
    resc_path = f"{SCRATCH}/race_probe_{idx}_generated.resc"
    uart0_log = f"{SCRATCH}/race_probe_{idx}_uart0.log"

    logf = open(renode_log_path, "wb")
    proc = subprocess.Popen(
        [RENODE_BIN, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
        stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
    )
    record = {"idx": idx, "monitor_port": monitor_port, "uart_port": uart_port, "pid": proc.pid}
    try:
        t0 = time.monotonic()
        sock = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
        if sock is None:
            record["error"] = "monitor port never came up"
            results.append(record)
            return
        mon = MonitorClient(sock)
        mon.sock.settimeout(10.0)
        try:
            mon.sock.recv(65536)
        except OSError:
            pass

        with open(resc_path, "w") as f:
            f.write(f'''# generated by listener_race_probe.py (iteration {idx})
:name: av-m24-4d-race-{idx}
using sysbus
using sysbus.cluster0
using sysbus.cluster1
mach create "av-m24-4d-race-{idx}"
machine LoadPlatformDescription @{PLATFORM}
uart0 CreateFileBackend @{uart0_log} true
emulation CreateServerSocketTerminal $uartport "uart1_bridge_{idx}" false
connector Connect sysbus.uart1 uart1_bridge_{idx}
macro reset
"""
    cluster0 ForEach IsHalted true
    cluster1 ForEach IsHalted true
    rpu0 IsHalted false
    sysbus LoadELF @{ELF} cpu=rpu0
"""
runMacro $reset
''')

        mon.cmd(f"$uartport = {uart_port}")

        # Start the lsof poller BEFORE sending `include`, so we capture the true t=0 baseline
        # (no listener yet) and the exact moment (if any) a LISTEN socket for uart_port appears,
        # relative to when the `include` command was actually sent on the wire.
        stop_event = threading.Event()
        samples = []
        poll_result = {}
        def poller():
            poll_result["first_listen_offset"] = poll_lsof_for_port(proc.pid, uart_port, stop_event, samples, budget_s=45.0)
        pt = threading.Thread(target=poller)
        pt.start()

        t_include_sent, include_chunks = mon.cmd(f"include @{resc_path}", idle_gap=0.3, timeout=20.0)
        t_include_reply_done = time.monotonic()
        include_text = clean(b"".join(c for _, c in include_chunks))
        record["include_reply_first_pass"] = include_text[:300]
        record["include_reply_first_pass_elapsed_s"] = t_include_reply_done - t_include_sent

        # Hypothesis: the 0.3s idle-gap cutoff walked away from more output still coming.
        # Keep listening on the SAME monitor socket for up to 30 more seconds.
        more_chunks = mon.read_until_idle(idle_gap=1.0, overall_timeout=30.0)
        t_after_extra_wait = time.monotonic()
        extra_text = clean(b"".join(c for _, c in more_chunks))
        record["extra_monitor_output_after_first_reply"] = extra_text[:1000]
        record["extra_wait_elapsed_s"] = t_after_extra_wait - t_include_reply_done

        # Now do the real TCP-connect probe (what wait_for_port/the production bridge does).
        conn_deadline = time.monotonic() + 15.0
        client = wait_for_port("127.0.0.1", uart_port, conn_deadline)
        t_connect_result = time.monotonic()
        record["tcp_connect_succeeded_within_15s"] = client is not None
        record["tcp_connect_elapsed_from_include_sent_s"] = t_connect_result - t_include_sent
        if client is not None:
            client.close()

        # Let the lsof poller run a little longer to see if a late bind ever happens even past
        # the production bridge's own 15s budget, then stop it.
        time.sleep(5.0)
        stop_event.set()
        pt.join(timeout=10.0)
        record["lsof_first_listen_offset_from_poll_start_s"] = poll_result.get("first_listen_offset")
        record["lsof_sample_count"] = len(samples)
        # Keep only a compact trace: state transitions (hit flips from False->True), plus first/last.
        transitions = []
        prev_hit = None
        for s in samples:
            if s["hit"] != prev_hit:
                transitions.append({"t": round(s["t"], 3), "hit": s["hit"]})
                prev_hit = s["hit"]
        record["lsof_hit_transitions"] = transitions
        if samples:
            record["lsof_final_sample_lines"] = samples[-1]["any_listen_lines"]

        # Full lsof snapshot of the Renode process's sockets at the very end, for direct
        # inspection of address family / actual bound address (hypothesis b).
        final_lines, final_raw = lsof_listen_ports(proc.pid)
        record["final_lsof_listen_lines"] = final_lines
    except Exception as e:
        record["exception"] = repr(e)
    finally:
        try:
            mon.send("quit")
        except Exception:
            pass
        try:
            proc.wait(timeout=10.0)
        except Exception:
            proc.kill()
            try:
                proc.wait(timeout=10.0)
            except Exception:
                pass
        results.append(record)


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 5
    results = []
    for i in range(n):
        print(f"=== iteration {i} ===", flush=True)
        run_one_iteration(i, results)
        print(json.dumps(results[-1], indent=2, default=str), flush=True)
    with open(f"{SCRATCH}/listener_race_probe_results.json", "w") as f:
        json.dump(results, f, indent=2, default=str)
    print("wrote", f"{SCRATCH}/listener_race_probe_results.json")


if __name__ == "__main__":
    main()

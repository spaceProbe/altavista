#!/usr/bin/env python3
"""M24.4b -- control experiment, independent of io_lockstep entirely: does Renode's
CreateServerSocketTerminal + connector Connect wiring deliver TCP-side bytes into ANY UART's RX
path on this exact platform/ELF at all? Uses uart0 (the console), which already has a live
RTEMS shell reading stdin (confirmed in M24.4's own boot transcript: "RTEMS Shell on
/dev/console. Use 'help' to list commands. SHLL [/] #"), instead of uart1/io_lockstep -- if
typing a real shell command over this exact same TCP-terminal mechanism never produces a shell
response, that is direct, application-independent proof the RX side of this wiring is broken
for this Renode build/platform, not something specific to io_lockstep's own code or to uart1.
"""
import re
import socket
import subprocess
import sys
import time

ANSI_RE = re.compile(rb"\x1b\[[0-9;]*m")


def wait_for_port(host, port, deadline):
    while time.monotonic() < deadline:
        try:
            return socket.create_connection((host, port), timeout=1.0)
        except OSError:
            time.sleep(0.2)
    raise TimeoutError(f"port {port} never accepted a connection")


def main():
    renode = ("/Users/probe/code/AltaVista/third_party/renode/renode-1.16.1-osx-arm64/"
              "Renode.app/Contents/MacOS/renode")
    resc = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart_tcp_rx_control_test.resc"
    monitor_port = 15031
    console_port = 15032
    log_path = "/Users/probe/code/AltaVista/third_party/renode/M24_4b/uart_tcp_rx_control_test.renode_log.txt"

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            [renode, "--disable-gui", "--hide-log", "-P", str(monitor_port)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
        )
        try:
            t0 = time.monotonic()
            sock = wait_for_port("127.0.0.1", monitor_port, t0 + 30.0)
            sock.settimeout(2.0)
            try:
                sock.recv(65536)
            except OSError:
                pass

            def send(line):
                sock.sendall((line + "\r\n").encode())

            def drain(timeout=5.0):
                sock.settimeout(timeout)
                chunks = []
                try:
                    while True:
                        data = sock.recv(65536)
                        if not data:
                            break
                        chunks.append(data)
                except socket.timeout:
                    pass
                return b"".join(chunks)

            send(f"$consoleport = {console_port}")
            drain()
            send(f"include @{resc}")
            print("include reply:", drain(timeout=15.0)[:500])

            # Let boot proceed far enough for the RTEMS shell prompt to appear (per M24.4's own
            # transcript this happens quickly, well under 5 virtual seconds).
            send('emulation RunFor "6"')
            print("boot RunFor reply:", drain(timeout=30.0)[:300])

            console_client = socket.create_connection(("127.0.0.1", console_port), timeout=5.0)
            print("console TCP client connected")

            # Drain whatever the shell has already printed (banner/prompt) so we can see the
            # delta caused by our own input specifically.
            console_client.settimeout(2.0)
            pre = b""
            try:
                while True:
                    chunk = console_client.recv(4096)
                    if not chunk:
                        break
                    pre += chunk
            except socket.timeout:
                pass
            print(f"pre-input console bytes: {len(pre)} : {pre[-300:]!r}")

            # Type a real, distinctive shell command, resending it across several RunFor steps
            # exactly like the HELLO-resend design elsewhere in this task, in case of a similar
            # timing sensitivity.
            cmd = b"echo AV_M24_4B_PROBE\r\n"
            for i in range(10):
                send('emulation RunFor "0.2"')
                drain(timeout=10.0)
                try:
                    console_client.sendall(cmd)
                except OSError as e:
                    print(f"console send failed on attempt {i+1}: {e}")

            send('emulation RunFor "2"')
            drain(timeout=15.0)

            console_client.settimeout(3.0)
            post = b""
            try:
                while True:
                    chunk = console_client.recv(4096)
                    if not chunk:
                        break
                    post += chunk
            except socket.timeout:
                pass
            print(f"post-input console bytes: {len(post)} : {post!r}")

            cleaned = ANSI_RE.sub(b"", post)
            got_echo = b"AV_M24_4B_PROBE" in cleaned
            print(f"shell echoed our distinctive string back: {got_echo}")

            console_client.close()
            send("quit")
            try:
                proc.wait(timeout=15.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=15.0)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait(timeout=15.0)

    print("PASS (RX wiring delivers bytes)" if got_echo else "FAIL (RX wiring does not deliver bytes even to the shell)")
    return 0 if got_echo else 1


if __name__ == "__main__":
    sys.exit(main())

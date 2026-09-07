#!/usr/bin/env python3
"""One-off probe (not a spike measurement) to see the exact byte protocol Renode's
-P/--port Monitor TCP server speaks, so measure_virtual_time.py knows what to read for
and can detect "command finished" reliably. Connects, sends a couple of commands, and
dumps the raw bytes received (with a short read timeout) so the prompt framing is visible.
"""
import socket
import sys
import time

HOST = "127.0.0.1"
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 9999

s = socket.create_connection((HOST, PORT), timeout=5)
s.settimeout(2)


def read_all(label):
    time.sleep(0.3)
    chunks = []
    try:
        while True:
            data = s.recv(65536)
            if not data:
                break
            chunks.append(data)
    except socket.timeout:
        pass
    blob = b"".join(chunks)
    print(f"--- {label} ({len(blob)} bytes) ---")
    print(repr(blob))


read_all("initial banner")
s.sendall(b"help\r\n")
read_all("after help")
s.sendall(b"machine ElapsedVirtualTime\r\n")
read_all("after ElapsedVirtualTime (no machine yet)")
s.close()

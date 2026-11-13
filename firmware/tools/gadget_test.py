#!/usr/bin/env python3
"""Service client for the ESP32 gadget (TCP 7790). The keyboard and mouse
come from the hub (light link), and so do firmware updates; this is for
diagnostics.

Frames are [kind u8][len u8][payload]. Usage:
  gadget_test.py HOST ping [N]       round-trip time over N pings
  gadget_test.py HOST log [SECONDS]  prints the board's log (kept lines, then live)
"""
import socket
import statistics
import struct
import sys
import time

PORT = 7790


def connect(host):
    s = socket.create_connection((host, PORT), timeout=5)
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    kind, payload = recv(s)
    print("board:", payload.decode(errors="replace"))
    return s


def send(s, kind, payload=b""):
    s.sendall(bytes([kind, len(payload)]) + payload)


def recv_exact(s, n):
    b = b""
    while len(b) < n:
        part = s.recv(n - len(b))
        if not part:
            raise EOFError("connection closed")
        b += part
    return b


def recv(s):
    kind, n = recv_exact(s, 2)
    return kind, recv_exact(s, n)


def main():
    host, cmd, *rest = sys.argv[1:]
    s = connect(host)
    if cmd == "ping":
        n = int(rest[0]) if rest else 200
        times = []
        for i in range(n):
            t = time.perf_counter()
            send(s, 3, struct.pack("<I", i))
            while True:
                kind, payload = recv(s)
                if kind == 3:
                    break
            times.append((time.perf_counter() - t) * 1000)
            time.sleep(0.01)
        times.sort()
        print(f"rtt ms: min {times[0]:.1f}  median {statistics.median(times):.1f}  "
              f"p95 {times[int(n * 0.95) - 1]:.1f}  max {times[-1]:.1f}")
    elif cmd == "log":
        s.settimeout(float(rest[0]) if rest else 5)
        try:
            while True:
                kind, payload = recv(s)
                if kind == 5:
                    print(payload.decode(errors="replace"))
        except socket.timeout:
            pass
    else:
        sys.exit(__doc__)
    s.close()


if __name__ == "__main__":
    main()

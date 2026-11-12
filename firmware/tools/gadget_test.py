#!/usr/bin/env python3
"""Test client for the ESP32 gadget's phase-1 protocol (TCP 7790).

Frames are [kind u8][len u8][payload]. Usage:
  gadget_test.py HOST ping [N]       round-trip time over N pings
  gadget_test.py HOST type TEXT      types TEXT (US layout, a-z 0-9 space . , - and Enter)
  gadget_test.py HOST circle         moves the pointer in a circle
  gadget_test.py HOST download       restarts the board into the ROM download mode
  gadget_test.py HOST leds [SECONDS] prints keyboard LED changes from the PC
  gadget_test.py HOST log [SECONDS]  prints the board's log (kept lines, then live)
  gadget_test.py HOST ota IMAGE.bin  updates the firmware (espflash save-image output)
"""
import math
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


def keycode(ch):
    """(modifier, usage) for a character, US layout."""
    if "a" <= ch <= "z":
        return 0, 0x04 + ord(ch) - ord("a")
    if "A" <= ch <= "Z":
        return 0x02, 0x04 + ord(ch) - ord("A")
    if "1" <= ch <= "9":
        return 0, 0x1E + ord(ch) - ord("1")
    table = {"0": 0x27, "\n": 0x28, " ": 0x2C, "-": 0x2D, ".": 0x37, ",": 0x36}
    if ch in table:
        return 0, table[ch]
    raise ValueError(f"no key for {ch!r}")


def kb(s, mod=0, key=0):
    send(s, 1, bytes([mod, 0, key, 0, 0, 0, 0, 0]))


def mouse(s, dx, dy, buttons=0, wheel=0, pan=0):
    send(s, 2, struct.pack("<BhhbB", buttons, dx, dy, wheel, pan & 0xFF))


def wait_line(s, words, timeout=20):
    """The first log line from the board containing one of `words`."""
    s.settimeout(timeout)
    while True:
        kind, payload = recv(s)
        if kind == 5:
            line = payload.decode(errors="replace")
            if any(w in line for w in words):
                return line


def reconnect(host, timeout=40):
    deadline = time.time() + timeout
    time.sleep(2)
    while True:
        try:
            return connect(host)
        except OSError:
            if time.time() > deadline:
                raise
            time.sleep(1)


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
    elif cmd == "type":
        for ch in " ".join(rest):
            mod, key = keycode(ch)
            kb(s, mod, key)
            time.sleep(0.01)
            kb(s)
            time.sleep(0.01)
    elif cmd == "circle":
        steps = 120
        r = 150
        for i in range(steps):
            a1, a0 = 2 * math.pi * (i + 1) / steps, 2 * math.pi * i / steps
            mouse(s, round(r * (math.cos(a1) - math.cos(a0))), round(r * (math.sin(a1) - math.sin(a0))))
            time.sleep(0.008)
    elif cmd == "download":
        send(s, 0xF0)
        time.sleep(0.5)
    elif cmd == "ota":
        image = open(rest[0], "rb").read()
        for attempt in range(2):
            send(s, 0xF1, struct.pack("<I", len(image)))
            line = wait_line(s, ("INFO  ota:", "WARN  ota:"))
            print(line)
            if "update mode" in line:
                # The board restarts into the update mode (WiFi only).
                s.close()
                s = reconnect(host)
                continue
            if "WARN  ota:" in line:
                sys.exit(1)
            break
        else:
            sys.exit("the board did not enter the update mode")
        for i in range(0, len(image), 252):
            send(s, 0xF2, image[i:i + 252])
        send(s, 0xF3)
        print(wait_line(s, ("INFO  ota: ", "WARN  ota:"), timeout=60))
    elif cmd == "log":
        s.settimeout(float(rest[0]) if rest else 5)
        try:
            while True:
                kind, payload = recv(s)
                if kind == 5:
                    print(payload.decode(errors="replace"))
        except socket.timeout:
            pass
    elif cmd == "leds":
        s.settimeout(float(rest[0]) if rest else 30)
        try:
            while True:
                kind, payload = recv(s)
                if kind == 4:
                    print(f"leds 0x{payload[0]:02x}")
        except socket.timeout:
            pass
    else:
        sys.exit(__doc__)
    s.close()


if __name__ == "__main__":
    main()

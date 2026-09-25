#!/usr/bin/env python3
"""Real-launch the pi-rust TUI in a PTY and drive it through realistic input.

This script is the post-unit-test sanity check: it boots the binary, then
performs a scripted sequence (typing, Enter, Ctrl+C, /exit, paste, mouse
clicks, ...) and prints the rendered frame after each step. Anything the
unit tests miss — initial render, prompt label, status bar, redraw on
exit — should surface here.
"""

import os
import pty
import select
import signal
import sys
import time
import termios
import fcntl
import struct


BIN = "/Users/louloulin/appx/pi/pi-rust/target/release/pi"
COLS = 100
ROWS = 30
STEP_DELAY = 0.25


def set_winsize(fd, rows, cols):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def drain(fd, timeout=0.15):
    """Read everything available on `fd`, returning decoded text."""
    out = []
    end = time.time() + timeout
    while True:
        remaining = end - time.time()
        if remaining <= 0:
            break
        r, _, _ = select.select([fd], [], [], remaining)
        if not r:
            break
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        out.append(chunk.decode("utf-8", errors="replace"))
    return "".join(out)


def send(fd, data, settle=STEP_DELAY):
    if isinstance(data, str):
        data = data.encode("utf-8")
    os.write(fd, data)
    return drain(fd, timeout=settle)


def step(label, fd, payload):
    print(f"\n===== STEP: {label} =====")
    out = send(fd, payload)
    print(out.replace("\x1b", "ESC"))
    return out


def main():
    pid, fd = pty.fork()
    if pid == 0:
        # child
        env = os.environ.copy()
        env["TERM"] = "xterm-256color"
        env["PI_NO_COLOR"] = "1"
        os.execvpe(BIN, [BIN, "interactive"], env)

    set_winsize(fd, ROWS, COLS)
    # give the binary a beat to draw the first frame
    initial = drain(fd, timeout=1.2)
    print(f"===== INITIAL FRAME ({COLS}x{ROWS}) =====")
    print(initial.replace("\x1b", "ESC"))

    # ---- scripted interaction ----
    step("type 'hello'", fd, "hello")
    step("Backspace twice", fd, "\x7f\x7f")          # erase "lo"
    step("Type more", fd, " world")
    step("Enter (submit)", fd, "\r")
    step("Wait for response", fd, "")
    time.sleep(2.5)
    drained = drain(fd, timeout=0.4)
    print(drained.replace("\x1b", "ESC"))

    step("Ctrl+L (clear/redraw)", fd, "\x0c")
    step("Slash menu: /", fd, "/")
    step("Slash: 'mo'", fd, "mo")
    step("Escape to dismiss", fd, "\x1b")
    step("Ctrl+C (interrupt)", fd, "\x03")
    step("Type '/exit'", fd, "/exit")
    step("Enter to confirm exit", fd, "\r")
    final = drain(fd, timeout=1.0)
    print("===== FINAL FRAME =====")
    print(final.replace("\x1b", "ESC"))

    # cleanup
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    os.close(fd)
    os.waitpid(pid, 0)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n[!] interrupted", file=sys.stderr)
        sys.exit(1)
#!/usr/bin/env python3
"""Raw-PTY probe for the Ctrl+C exit/clear semantics of the Rust `pi` TUI.

`pty_capture.py` renders frames; it cannot answer "did the process exit?".
This probe drives the same binary in the same kind of PTY, but measures
process liveness with `waitpid(WNOHANG)` and reports the observed
sequence of *screen states* per keystroke.

It exists because audit §12.4 item 1 ("a single Ctrl+C on an empty prompt
exits, and a draft is lost") has to be re-checked on every new tip: the
exit path is a process-level fact, not a rendering fact.

Usage
-----
    python3 pi-rust/scripts/pty_probe_ctrl_c.py --bin pi-rust/target/debug/pi

Exit code 0 = probe ran, 1 = probe could not run (bad binary/deps).
"""

from __future__ import annotations

import argparse
import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time

try:
    import pyte
except ImportError:  # pragma: no cover
    sys.exit("python module 'pyte' is required: pip install pyte")


def spawn(binary: str, args: list[str], cwd: str, env: dict, cols: int, rows: int):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    pid = os.fork()
    if pid == 0:  # child
        try:
            os.close(master)
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            os.dup2(slave, 0)
            os.dup2(slave, 1)
            os.dup2(slave, 2)
            if slave > 2:
                os.close(slave)
            os.chdir(cwd)
            os.execve(binary, [binary, *args], env)
        except BaseException as exc:  # pragma: no cover
            os.write(2, f"exec failed: {exc}\n".encode())
            os._exit(127)
    os.close(slave)
    return master, pid


def pump(master, stream, seconds: float) -> None:
    deadline = time.time() + seconds
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            return
        ready, _, _ = select.select([master], [], [], min(remaining, 0.05))
        if not ready:
            continue
        try:
            data = os.read(master, 1 << 16)
        except OSError:
            return
        if not data:
            return
        stream.feed(data.decode("utf-8", "replace"))


def alive(pid: int) -> bool:
    """True while the child is still running; reaps it exactly once."""
    try:
        return os.waitpid(pid, os.WNOHANG) == (0, 0)
    except ChildProcessError:
        # Already reaped by an earlier call (or never ours) -> not running.
        return False


def compose_line(screen) -> str:
    """The composer row is the last non-empty row above the footer."""
    rows = [line.rstrip() for line in screen.display]
    for line in reversed(rows):
        if line.startswith("> "):
            return line
    return "(no composer row found)"


class Session:
    def __init__(self, binary: str, args: list[str], cols: int = 120, rows: int = 34):
        self.home = tempfile.mkdtemp(prefix="pi-probe-home-")
        self.cwd = tempfile.mkdtemp(prefix="pi-probe-cwd-")
        env = dict(os.environ)
        env.update(
            {
                "HOME": self.home,
                "TERM": "xterm-256color",
                "COLORTERM": "truecolor",
                "LANG": "en_US.UTF-8",
                "COLUMNS": str(cols),
                "LINES": str(rows),
            }
        )
        env.pop("PI_HOME", None)
        self.screen = pyte.Screen(cols, rows)
        self.screen.set_mode(pyte.modes.LNM)
        self.stream = pyte.Stream(self.screen)
        self.master, self.pid = spawn(binary, args, self.cwd, env, cols, rows)
        pump(self.master, self.stream, 2.5)

    def send(self, data: bytes, wait: float = 0.4) -> None:
        try:
            os.write(self.master, data)
        except OSError:
            pass
        pump(self.master, self.stream, wait)

    def alive(self) -> bool:
        return alive(self.pid)

    def close(self) -> None:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.kill(self.pid, sig)
            except ProcessLookupError:
                break
            time.sleep(0.2)
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.master)
        shutil.rmtree(self.home, ignore_errors=True)
        shutil.rmtree(self.cwd, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--model", default="faux/faux-model")
    ap.add_argument("--args", default="")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        print(f"binary not found: {args.bin}", file=sys.stderr)
        return 1
    binary = os.path.abspath(args.bin)
    extra = [a for a in args.args.split(" ") if a]
    argv = ["--model", args.model, *extra]

    print("== case A: draft in composer, one Ctrl+C ==")
    s = Session(binary, argv)
    print(f"   startup: alive={s.alive()} composer={compose_line(s.screen)!r}")
    s.send(b"this draft must not be lost", 0.8)
    print(f"   after typing: alive={s.alive()} composer={compose_line(s.screen)!r}")
    s.send(b"\x03", 1.2)
    a_alive = s.alive()
    print(f"   after ONE Ctrl+C: alive={a_alive} composer={compose_line(s.screen)!r}")
    s.close()

    print("== case B: draft in composer, two Ctrl+C inside the double-press window ==")
    s = Session(binary, argv)
    s.send(b"draft two", 0.8)
    s.send(b"\x03", 0.2)
    mid_alive = s.alive()
    mid_composer = compose_line(s.screen)
    s.send(b"\x03", 1.2)
    b_alive = s.alive()
    print(
        f"   after Ctrl+C #1: alive={mid_alive} composer={mid_composer!r} | "
        f"after Ctrl+C #2: alive={b_alive}"
    )
    s.close()

    print("== case C: empty composer, one Ctrl+C ==")
    s = Session(binary, argv)
    s.send(b"\x03", 1.2)
    c_alive = s.alive()
    print(f"   after ONE Ctrl+C: alive={c_alive} composer={compose_line(s.screen)!r}")
    s.close()

    print("== case D: empty composer, two Ctrl+C ==")
    s = Session(binary, argv)
    s.send(b"\x03", 0.2)
    s.send(b"\x03", 1.2)
    d_alive = s.alive()
    print(f"   after two Ctrl+C: alive={d_alive}")
    s.close()

    print("== case E: empty composer, Ctrl+D ==")
    s = Session(binary, argv)
    s.send(b"\x04", 1.2)
    e_alive = s.alive()
    print(f"   after Ctrl+D: alive={e_alive}")
    s.close()

    print()
    print("summary (alive=True means the process was still running):")
    print(f"  A draft  + 1x Ctrl+C  -> alive={a_alive} (draft loss if exited)")
    print(f"  B draft  + 2x Ctrl+C  -> alive={b_alive}")
    print(f"  C empty  + 1x Ctrl+C  -> alive={c_alive}")
    print(f"  D empty  + 2x Ctrl+C  -> alive={d_alive}")
    print(f"  E empty  + 1x Ctrl+D  -> alive={e_alive}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Raw-PTY probe for `app.suspend` (Ctrl+Z) — the one chord whose effect is a
*process state*, not a frame.

`pty_capture.py` renders frames, and the other probes answer liveness with
`waitpid`. Suspend needs a third measurement: the kernel's stop state. After
`Ctrl+Z` the child must be `T` in `/proc/<pid>/stat`, and after `SIGCONT`/`fg`
it must be back in `R`/`S` with a repainted frame.

This probe must run under **job control**, because `SIGTSTP` aimed at an
*orphaned* process group is discarded by the kernel (POSIX: a stopped group
with no parent in the same session would be unrecoverable). Two cases:

* case A — `pi` spawned directly, `os.setsid()` in the child: the child's group
  is orphaned, so `Ctrl+Z` is a repaint and nothing stops. This is the case
  every other harness in this directory creates, and it is why the frame
  captures cannot assert suspend (LUM-1308 §limitations).
* case B — `pi` started from an interactive `bash` in the same PTY: bash is the
  session leader, puts the job in its own process group, and is itself the
  parent in the same session, so the group is *not* orphaned and the stop
  really happens. The probe then waits for `fg` and checks the repaint.

Usage
-----
    python3 pi-rust/scripts/pty_probe_ctrl_z.py --bin pi-rust/target/debug/pi

Exit code 0 = probe ran, 1 = probe could not run (bad binary/deps).
Case results are printed; case B is the one that gates the chord.
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


def pump_until(master, stream, predicate, timeout: float) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        pump(master, stream, 0.1)
    return predicate()


# ------------------------------------------------------------ /proc helpers


def proc_table() -> list[tuple[int, int, str, str]]:
    """`(pid, ppid, state, comm)` for every process we may look at."""
    rows = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat", encoding="utf-8") as fh:
                stat = fh.read()
        except OSError:
            continue
        # `comm` is parenthesised and may contain spaces or ')' — split it off
        # first, then the remaining fields are whitespace-separated with
        # `state` first and `ppid` second after the closing paren.
        try:
            comm = stat[stat.index("(") + 1 : stat.rindex(")")]
            rest = stat[stat.rindex(")") + 2 :].split()
            rows.append((int(entry), int(rest[1]), rest[0], comm))
        except (ValueError, IndexError):
            continue
    return rows


def state_of(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as fh:
            stat = fh.read()
        return stat[stat.rindex(")") + 2 :].split()[0]
    except (OSError, ValueError, IndexError):
        return "?"


def find_child(ppid: int, comm_contains: str) -> int | None:
    for pid, parent, _state, comm in proc_table():
        if parent == ppid and comm_contains in comm:
            return pid
    return None


def alive(pid: int) -> bool:
    """True while the child is still running; reaps it exactly once.

    Note `WNOHANG` without `WUNTRACED`: a *stopped* child still counts as
    running here, which is exactly what suspend needs.

    Only valid for a process this probe forked itself — for case B's `pi`
    (bash's child) use [`proc_alive`].
    """
    try:
        return os.waitpid(pid, os.WNOHANG) == (0, 0)
    except ChildProcessError:
        return False


def proc_alive(pid: int | None) -> bool:
    """Liveness for a process that is *not* our child.

    `waitpid` raises `ChildProcessError` there, so signal 0 (or the state we can
    already read from `/proc`) is the only usable check.
    """
    if not pid:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass  # it exists, it is just not ours to signal
    return True


def screen_text(screen) -> str:
    return "\n".join(line.rstrip() for line in screen.display)


class Session:
    def __init__(self, binary: str, args: list[str], cols: int = 120, rows: int = 34):
        self.home = tempfile.mkdtemp(prefix="pi-probe-z-home-")
        self.cwd = tempfile.mkdtemp(prefix="pi-probe-z-cwd-")
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

    def send(self, data: bytes, wait: float = 0.4) -> None:
        try:
            os.write(self.master, data)
        except OSError:
            pass
        pump(self.master, self.stream, wait)

    def text(self) -> str:
        return screen_text(self.screen)

    def close(self) -> None:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.kill(self.pid, sig)
                os.killpg(self.pid, sig)
            except (ProcessLookupError, PermissionError):
                break
            time.sleep(0.2)
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.master)
        shutil.rmtree(self.home, ignore_errors=True)
        shutil.rmtree(self.cwd, ignore_errors=True)


def session_env(home: str, cols: int, rows: int) -> dict:
    env = dict(os.environ)
    env.update(
        {
            "HOME": home,
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8",
            "COLUMNS": str(cols),
            "LINES": str(rows),
            "PS1": "probe$ ",
        }
    )
    env.pop("PI_HOME", None)
    return env


def case_a(binary: str, argv: list[str]) -> dict:
    """Direct spawn: the child is its own session, so its group is orphaned."""
    session = Session(binary, argv)
    pump(session.master, session.stream, 2.5)
    started = session.text()
    session.send(b"draft before Ctrl+Z", 0.8)
    before = session.text()
    session.send(b"\x1a", 2.0)  # Ctrl+Z
    after_state = state_of(session.pid)
    after = session.text()
    running = alive(session.pid)
    session.close()
    return {
        "started": bool(started.strip()),
        "draft_visible": "draft before Ctrl+Z" in before,
        "alive_after_ctrl_z": running,
        "state_after_ctrl_z": after_state,
        "repainted": after != before,
    }


def case_b(binary: str, argv: list[str], cols: int = 120, rows: int = 34) -> dict:
    """Interactive bash + job control: the stop really happens."""
    home = tempfile.mkdtemp(prefix="pi-probe-zj-home-")
    cwd = tempfile.mkdtemp(prefix="pi-probe-zj-cwd-")
    env = session_env(home, cols, rows)
    screen = pyte.Screen(cols, rows)
    screen.set_mode(pyte.modes.LNM)
    stream = pyte.Stream(screen)

    # `-i` for job control (a non-interactive bash never stops jobs), and
    # `--noprofile --norc` so the user's rc files cannot change the result.
    # Order matters: bash parses GNU long options *before* single-letter ones, so
    # `-i --noprofile` fails with `--: invalid option`.
    master, shell_pid = spawn(
        "/bin/bash", ["--noprofile", "--norc", "-i"], cwd, env, cols, rows
    )
    result: dict = {"shell_pid": shell_pid}
    try:
        prompt = pump_until(
            master, stream, lambda: "probe$" in screen_text(screen), timeout=8.0
        )
        result["prompt"] = prompt

        # `set -m` is implied by `-i`; assert it from the shell itself.
        os.write(master, b"set -o | grep monitor\n")
        pump(master, stream, 1.0)
        result["monitor_on"] = "on" in screen_text(screen).split("monitor")[-1][:12]

        command = " ".join([binary, *argv])
        os.write(master, command.encode() + b"\n")
        found = pump_until(
            master,
            stream,
            lambda: "to hide this header" in screen_text(screen)
            or "for commands" in screen_text(screen),
            timeout=15.0,
        )
        result["tui_started"] = found
        pi_pid = find_child(shell_pid, "pi")
        result["pi_pid"] = pi_pid
        result["state_before"] = state_of(pi_pid) if pi_pid else "?"

        os.write(master, b"\x1a")  # Ctrl+Z
        stopped = pump_until(
            master,
            stream,
            lambda: pi_pid is not None and state_of(pi_pid) == "T",
            timeout=6.0,
        )
        result["stopped_state"] = state_of(pi_pid) if pi_pid else "?"
        result["stopped"] = stopped
        # bash reports the stop on its own line once it has the terminal back.
        result["shell_reported_stop"] = pump_until(
            master, stream, lambda: "Stopped" in screen_text(screen), timeout=6.0
        )

        os.write(master, b"fg\n")
        resumed = pump_until(
            master,
            stream,
            lambda: pi_pid is not None and state_of(pi_pid) in ("R", "S"),
            timeout=8.0,
        )
        result["resumed"] = resumed
        result["state_after_fg"] = state_of(pi_pid) if pi_pid else "?"
        result["repainted"] = pump_until(
            master,
            stream,
            lambda: "for commands" in screen_text(screen),
            timeout=8.0,
        )
        result["alive_after_fg"] = proc_alive(pi_pid)
    finally:
        try:
            os.killpg(shell_pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            os.waitpid(shell_pid, 0)
        except ChildProcessError:
            pass
        try:
            os.close(master)
        except OSError:
            pass
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(cwd, ignore_errors=True)
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--model", default="faux/faux-model")
    ap.add_argument("--skip-a", action="store_true", help="run only the job-control case")
    args = ap.parse_args()

    if not os.path.exists(args.bin):
        print(f"binary not found: {args.bin}", file=sys.stderr)
        return 1
    binary = os.path.abspath(args.bin)
    argv = ["--model", args.model]

    if not args.skip_a:
        print("== case A: pi spawned directly (own session -> orphaned group) ==")
        a = case_a(binary, argv)
        print(f"   startup frame         : {a['started']}")
        print(f"   Ctrl+Z: state={a['state_after_ctrl_z']} alive={a['alive_after_ctrl_z']} "
              f"repainted={a['repainted']}")
        print("   (expected: SIGTSTP discarded — orphaned group, see module docstring)")

    print("== case B: pi started from an interactive bash (job control) ==")
    b = case_b(binary, argv)
    print(f"   shell prompt          : {b['prompt']} (pid {b['shell_pid']})")
    print(f"   monitor mode on       : {b['monitor_on']}")
    print(f"   TUI started           : {b['tui_started']}")
    print(f"   pi pid / state        : {b['pi_pid']} / {b['state_before']}")
    print(f"   after Ctrl+Z          : state={b['stopped_state']} stopped={b['stopped']} "
          f"bash_said_stopped={b['shell_reported_stop']}")
    print(f"   after fg              : state={b['state_after_fg']} resumed={b['resumed']} "
          f"repainted={b['repainted']} alive={b['alive_after_fg']}")

    print()
    verdict = (
        b["tui_started"]
        and b["stopped"]
        and b["shell_reported_stop"]
        and b["resumed"]
        and b["repainted"]
        and b["alive_after_fg"]
    )
    print(f"verdict: case B {'PASS' if verdict else 'FAIL'} "
          f"(Ctrl+Z stopped the TUI, fg resumed and repainted it)")
    return 0 if verdict else 1


if __name__ == "__main__":
    raise SystemExit(main())

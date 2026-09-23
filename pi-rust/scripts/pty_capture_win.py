#!/usr/bin/env python3
"""Windows/ConPTY backend for the `pi` TUI capture harness.

Why this exists
---------------
`pty_capture.py` is POSIX-only (`pty.openpty` + `fcntl`): on Windows it cannot
spawn the binary at all, so every Windows round so far produced screenshots
from a *frame buffer* — the app's own renderer frozen by a Rust test — and had
to say so in the report ("证明几何与高亮，不证明按键时序"). That is a real
evidence gap: a frame buffer cannot show that a keystroke reached the app, that
an overlay opened, or that a repaint happened in the right order.

Windows 10 1809+ ships ConPTY, and `pywinpty` exposes it to Python. This
module is the missing backend: it spawns the real `pi.exe` inside a real
pseudo console of an exact size and feeds it the same key token language as the
POSIX harness, so a Windows round can finally publish panels captured from a
live session.

What is shared and what is not
------------------------------
Everything that is platform neutral is *imported* from `pty_capture.py`:
`encode_keys` (the `<Enter>` / `<C-a>` token language), the pyte `Renderer`,
`snapshot`, `evaluate_panel` / `summarize_assertions` (the panel assertion
schema), and the palette. Only the three OS-coupled primitives are
re-implemented here:

| POSIX (`pty_capture.py`)      | Windows (this file)                    |
|-------------------------------|----------------------------------------|
| `spawn` (`os.fork` + `openpty`)| `ConPty.spawn` (`winpty.PtyProcess`)   |
| `pump` / `drain` (`select` on the master fd) | `ConPty.drain` (reader thread + queue) |
| `child_alive` / `reap` (`waitpid`) | `ConPty.is_alive` / `close`        |

The scenario JSON schema, the assertion schema, the caption format, the frame
hashes and the text dump are byte-for-byte the POSIX harness's, so a scenario
file written for Linux runs here unchanged (and vice versa) apart from the
path-valued fields `fixtures` / `extensions`, which are resolved relative to
the repository root in both.

Requirements
------------
    pip install pywinpty pyte wcwidth pillow

Usage
-----
    python pi-rust/scripts/pty_capture_win.py \
        --bin pi-rust/target/debug/pi.exe \
        --steps pi-rust/scripts/pty_scenarios/lum1457-tui-interaction.json \
        --out pi-rust/docs/screenshots/lum1457-tui-interaction.png

Caveat, stated up front (and printed at the end of every run): ConPTY is not a
Unix PTY. It re-renders the console for the attached client, so what the child
*sees* about its own terminal size is exact, but the byte stream carries
ConPTY's own cursor/erase traffic. Panels are therefore trustworthy about
layout, text, colour, cursor position and "a keystroke changed the screen";
they are not a byte-exact recording of what the app wrote.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import pty_capture as pc  # noqa: E402  (shared renderer + assertions + keys)

try:
    import winpty
except ImportError:  # pragma: no cover
    sys.exit("python module 'pywinpty' is required: pip install pywinpty")

try:
    import pyte
except ImportError:  # pragma: no cover - pty_capture imports it too
    sys.exit("python module 'pyte' is required: pip install pyte")


class ConPty:
    """A ConPTY-hosted child with a non-blocking byte stream.

    `winpty.PtyProcess.read` blocks, so the reads happen on one daemon thread
    that pushes chunks into a queue; the harness's own timing helpers
    (`drain` / `pump`) then work exactly like the POSIX ones, including the
    "quiet for N seconds" idle rule.
    """

    def __init__(self, argv, cwd, env, cols, rows):
        self.proc = winpty.PtyProcess.spawn(
            argv, cwd=cwd, env=env, dimensions=(rows, cols)
        )
        self._chunks: queue.Queue[bytes | None] = queue.Queue()
        self._closed = False
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    def _read_loop(self):
        while True:
            try:
                data = self.proc.read(1 << 16)
            except EOFError:
                break
            except Exception:  # pragma: no cover - ConPTY teardown races
                break
            if not data:
                continue
            if isinstance(data, str):
                data = data.encode("utf-8", "replace")
            self._chunks.put(data)
        self._chunks.put(None)

    def is_alive(self) -> bool:
        try:
            return bool(self.proc.isalive())
        except Exception:  # pragma: no cover
            return False

    def write(self, data: bytes) -> None:
        self.proc.write(data.decode("utf-8", "replace"))

    def drain(self, stream, idle=0.5, hard_timeout=5.0) -> bytes:
        """Feed output into `stream` until the child has been quiet `idle` s."""
        last = time.time()
        start = last
        got = bytearray()
        while time.time() - last < idle and time.time() - start < hard_timeout:
            try:
                data = self._chunks.get(timeout=0.05)
            except queue.Empty:
                continue
            if data is None:
                self._closed = True
                break
            last = time.time()
            got += data
            pc.note_raw(data)
            stream.feed(data.decode("utf-8", "replace"))
        return bytes(got)

    def close(self, hard=True) -> None:
        try:
            self.proc.terminate(force=True if hard else False)
        except Exception:  # pragma: no cover - already gone
            pass


def prepare_child(scenario, root, home, cwd, cols, rows):
    """Materialise fixtures / extensions and build the child environment.

    Mirrors `pty_capture.main`'s setup so a scenario behaves the same on both
    backends: fixture files (an object maps a name to contents, a list names
    files whose contents come from the repo), extensions under the *global*
    search path (`$HOME/.pi/agent/extensions`, the project-local path is gated
    by project trust that a fresh temp cwd has not granted), then the terminal
    environment plus the scenario's own `env` overrides.
    """
    declared_fixtures = scenario.get("fixtures", [])
    if isinstance(declared_fixtures, dict):
        fixture_items = declared_fixtures.items()
    else:
        fixture_items = (
            (rel, "// fixture for pty_capture_win.py\n") for rel in declared_fixtures
        )
    for rel, contents in fixture_items:
        path = os.path.join(cwd, rel)
        parent = os.path.dirname(path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(contents)

    declared = scenario.get("extensions") or {}
    if isinstance(declared, dict):
        sources = dict(declared)
    else:
        sources = {}
        for rel in declared:
            with open(os.path.join(root, rel), encoding="utf-8") as fh:
                sources[os.path.basename(rel)] = fh.read()
    for name, source in sources.items():
        path = os.path.join(home, ".pi", "agent", "extensions", name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(source)

    # `git_init` (LUM-1490): a scenario that needs a repository starts one in
    # the child's cwd, with `git` writing `.git/HEAD` itself — so the branch the
    # app reads (and a custom footer queries through `footerData`) is a real
    # one, not a synthesised file. `--initial-branch` keeps the name stable
    # across the git versions a developer may have (>= 2.28); the scenario can
    # pass `{"branch": "…"}` for the same thing. One empty commit is made so
    # `HEAD` is *born*: without it `git checkout --detach HEAD` (the detached-
    # HEAD panel) fails on an unborn branch.
    git_init = scenario.get("git_init")
    if git_init:
        branch = git_init if isinstance(git_init, str) else git_init.get("branch")
        command = ["git", "init", "--quiet"]
        if branch:
            command += ["--initial-branch", str(branch)]
        run_git = {
            "cwd": cwd,
            "check": True,
            "stdin": subprocess.DEVNULL,
            "stdout": subprocess.PIPE,
            "stderr": subprocess.PIPE,
        }
        subprocess.run(command, **run_git)
        subprocess.run(
            [
                "git",
                "-c",
                "user.email=pi-pty@example.invalid",
                "-c",
                "user.name=pi pty harness",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "pty-scenario init",
            ],
            **run_git,
        )

    env = dict(os.environ)
    env.update(
        {
            # `dirs::home_dir()` picks USERPROFILE on Windows and HOME
            # elsewhere; set both so the agent dir (extensions, sessions,
            # settings) lands in the scenario's temp home on either backend.
            # HOMEDRIVE / HOMEPATH are deliberately left alone: the temp home
            # may sit on a different drive than the caller's, and a split
            # HOMEDRIVE/HOMEPATH pair that disagrees with USERPROFILE is a
            # path the app never has to consider in a real session.
            "HOME": home,
            "USERPROFILE": home,
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "COLUMNS": str(cols),
            "LINES": str(rows),
            "NO_COLOR": "",
        }
    )
    for key, value in (scenario.get("env") or {}).items():
        if value == "":
            env.pop(key, None)
        else:
            env[key] = value
    env.pop("PI_HOME", None)
    return env


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True, help="path to the built `pi.exe`")
    ap.add_argument("--steps", required=True, help="scenario JSON file")
    ap.add_argument("--out", required=True, help="PNG output path")
    ap.add_argument("--text-out", default=None, help="text dump (default <out>.txt)")
    ap.add_argument("--home", default=None, help="HOME for the child (fresh temp by default)")
    ap.add_argument("--cwd", default=None, help="cwd for the child (fresh temp by default)")
    ap.add_argument("--keep-temp", action="store_true")
    ap.add_argument("--font-size", type=int, default=15)
    ap.add_argument("--scale", type=int, default=2)
    ap.add_argument("--allow-xpass", action="store_true")
    ap.add_argument(
        "--sheet",
        type=int,
        default=0,
        help="tile the panels into N columns instead of one tall column",
    )
    ap.add_argument("--sheet-width", type=int, default=2400)
    args = ap.parse_args()

    with open(args.steps, encoding="utf-8") as fh:
        scenario = json.load(fh)

    cols = scenario.get("cols", 120)
    rows = scenario.get("rows", 34)
    panels = scenario["panels"]

    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    base = os.environ.get("TEMP") or os.environ.get("TMP") or "."
    home = args.home or os.path.join(base, f"pi-pty-home-{os.getpid()}")
    cwd = args.cwd or os.path.join(base, f"pi-pty-cwd-{os.getpid()}")
    # Only the scratch dirs this harness invented are its to delete; an
    # explicit `--home` / `--cwd` is the caller's fixture.
    if args.home is None and os.path.isdir(home):
        shutil.rmtree(home, ignore_errors=True)
    if args.cwd is None and os.path.isdir(cwd):
        shutil.rmtree(cwd, ignore_errors=True)
    os.makedirs(home, exist_ok=True)
    os.makedirs(cwd, exist_ok=True)

    env = prepare_child(scenario, root, home, cwd, cols, rows)

    screen = pyte.Screen(cols, rows)
    screen.set_mode(pyte.modes.LNM)
    # `AltScreenFeeder` clears the grid on `[?1049h` — pyte has no
    # alternate-screen buffer, so without it the `pi: <cwd> is not trusted`
    # banner printed to the *main* screen stays on the grid and the app's
    # first alt-screen paint (which writes only the cells it owns) comes out
    # interleaved with it. The feeder feeds the same `screen` object, so the
    # panel freezing below is unchanged.
    stream = pc.AltScreenFeeder(screen)
    binary = os.path.abspath(args.bin)
    child = ConPty([binary, *scenario.get("args", [])], cwd, env, cols, rows)

    cards = []
    assertion_rows: list[tuple[str, list[dict]]] = []
    # Byte offset the current panel's raw window starts at. `raw_expect` /
    # `raw_reject` assertions (OSC 0 terminal titles, OSC 8 links) are matched
    # against this window, because those sequences are not cell content. See
    # `pty_capture.RAW_TRACE` for what ConPTY does and does not preserve.
    raw_mark = 0
    try:
        child.drain(stream, idle=1.0, hard_timeout=10.0)
        deadline = time.time() + 25.0
        while time.time() < deadline and not pc.snapshot(screen).strip():
            child.drain(stream, idle=0.4, hard_timeout=2.0)
        if not pc.snapshot(screen).strip():
            print(
                "WARN: the app painted nothing within 25s of startup — the first "
                "panel may be a blank frame",
                file=sys.stderr,
            )
        for index, panel in enumerate(panels, start=1):
            for keys, pause in pc.send_steps(panel):
                if keys:
                    child.write(pc.encode_keys(keys))
                if pause > 0:
                    child.drain(stream, idle=min(0.4, max(0.1, pause / 2)), hard_timeout=pause + 1.0)
            wait = float(panel.get("wait", 0.7))
            if wait > 0:
                child.drain(stream, idle=min(0.5, max(0.15, wait / 2)), hard_timeout=wait + 1.0)
            wait_for = panel.get("wait_for")
            if wait_for and wait_for not in pc.snapshot(screen):
                timeout = float(panel.get("wait_timeout", max(wait, 8.0)))
                limit = time.time() + timeout
                while time.time() < limit and wait_for not in pc.snapshot(screen):
                    child.drain(stream, idle=0.3, hard_timeout=1.5)
                if wait_for not in pc.snapshot(screen):
                    print(
                        f"WARN: panel {index} waited {timeout:.1f}s for "
                        f"{wait_for!r} and it never appeared",
                        file=sys.stderr,
                    )
            if panel.get("skip_capture"):
                continue
            label = panel.get("label") or f"panel {index}"
            caption = scenario.get("caption", "") or ""
            head = f"[{index}] {label}" if not caption else f"{caption}  |  [{index}] {label}"
            frame = copy.deepcopy(screen)
            body = pc.snapshot(frame)
            panel_raw = pc.raw_since(raw_mark)
            raw_mark = len(pc.RAW_TRACE)
            cards.append(
                (head, body, frame, frame.cursor.x, frame.cursor.y, child.is_alive())
            )
            assertion_rows.append((head, pc.evaluate_panel(panel, body, panel_raw)))
    finally:
        send = scenario.get("final_send")
        if send:
            try:
                child.write(pc.encode_keys(send))
                child.drain(stream, idle=0.8, hard_timeout=6.0)
            except Exception:
                pass
        child.close()
        if not args.keep_temp:
            if args.home is None:
                shutil.rmtree(home, ignore_errors=True)
            if args.cwd is None:
                shutil.rmtree(cwd, ignore_errors=True)

    if not cards:
        print("no panels captured", file=sys.stderr)
        return 1

    renderer = pc.Renderer(
        font_size=args.font_size,
        scale=args.scale,
        text="".join(body for _, body, *_ in cards),
    )
    images = [renderer.render_panel(frame, head) for head, _, frame, _, _, _ in cards]
    out_dir = os.path.dirname(os.path.abspath(args.out))
    os.makedirs(out_dir, exist_ok=True)
    composite = (
        renderer.sheet(images, args.sheet, args.sheet_width)
        if args.sheet
        else renderer.stack(images)
    )
    composite.save(args.out)

    hashes = [hashlib.sha256(body.encode("utf-8")).hexdigest()[:12] for _, body, *_ in cards]
    pix = [hashlib.sha256(img.tobytes()).hexdigest()[:12] for img in images]
    duplicates = [
        i for i in range(1, len(hashes)) if hashes[i] == hashes[i - 1] or pix[i] == pix[i - 1]
    ]

    text_out = args.text_out or (args.out + ".txt")
    with open(text_out, "w", encoding="utf-8") as fh:
        fh.write(
            f"# {os.path.basename(args.out)} — chars, {cols}x{rows} cells "
            f"(Windows ConPTY backend)\n"
        )
        for i, (head, body, _, cx, cy, alive) in enumerate(cards):
            fh.write(
                f"\n===== {head}  (cursor {cx},{cy}, frame {hashes[i]}, px {pix[i]}, "
                f"alive={alive}) =====\n{body}\n"
            )
            for row in assertion_rows[i][1]:
                fh.write(f"  assert {row['status']:<6} {pc.describe_assertion(row)}\n")

    print(f"wrote {args.out}")
    print(f"wrote {text_out}")
    for i, (head, body, _, cx, cy, alive) in enumerate(cards, start=1):
        print(
            f"frame {i:2d}  {hashes[i - 1]}  px {pix[i - 1]}  "
            f"alive={str(alive):5s} {head}"
        )

    assert_failed, assert_report = pc.summarize_assertions(assertion_rows, args.allow_xpass)
    if any(row for _, rows_ in assertion_rows for row in rows_):
        for head, rows_ in assertion_rows:
            if not rows_:
                continue
            worst = {"FAIL": 0, "XPASS": 1, "XFAIL": 2, "PASS": 3}
            print(f"--- {head} assertions ---")
            for row in sorted(rows_, key=lambda r: worst[r["status"]]):
                print(f"  {row['status']:<6} {pc.describe_assertion(row)}")
        print(assert_report[0])
        for line in assert_report[1:]:
            print(line, file=sys.stderr)

    if duplicates:
        panels_txt = ", ".join(str(i + 1) for i in duplicates)
        message = (
            f"{'FAIL' if scenario.get('distinct_panels') else 'WARN'}: panel(s) "
            f"[{panels_txt}] are byte-identical to the previous panel — the caption "
            f"claims a state change the frame does not show"
        )
        print(message, file=sys.stderr)
        if scenario.get("distinct_panels"):
            return 1
    if assert_failed:
        print(
            "wrote the PNG and the text dump; refusing to report success because "
            "panel assertions failed (see the FAIL/XPASS rows above)",
            file=sys.stderr,
        )
        return 1

    print(
        "note: captured through ConPTY (pywinpty), not a Unix pty — layout, text, "
        "colour, cursor and repaint order are real, the byte stream is ConPTY's "
        "own rendering of them.",
        file=sys.stderr,
    )
    for head, body, _, cx, cy, alive in cards:
        print(f"--- {head} (cursor {cx},{cy}, alive={alive}) ---")
        print(body)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

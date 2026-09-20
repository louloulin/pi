#!/usr/bin/env python3
"""Headless PTY capture harness for the Rust `pi` TUI.

Why this exists
---------------
Earlier TUI rounds produced screenshots with a throw-away harness (PTY +
hand-written VT parser + PIL) that was never committed, so every round
re-invented it and no reviewer could reproduce the images. This script is
that harness, committed, with a declarative scenario file.

It runs the real binary in a real PTY of an exact size, feeds a scripted
key sequence, parses the byte stream with `pyte` (a VT emulator, not a
hand-rolled ANSI stripper), and renders the resulting character grid to a
PNG *and* to a plain-text dump. The text dump is the load-bearing
evidence: it is greppable and does not depend on colors.

Requirements: python3 with `pyte`, `wcwidth` and `Pillow`
(`pip install pyte wcwidth pillow`).

Usage
-----
    python3 pi-rust/scripts/pty_capture.py \
        --bin ./artifacts/pi-bin \
        --steps pi-rust/scripts/pty_scenarios/merged_tip.json \
        --out pi-rust/docs/screenshots/lum1241-merged-tip.png

Scenario file schema (JSON):
    {
      "cols": 120, "rows": 34,
      "args": ["--model", "faux/faux-model"],
      "fixtures": ["src/main.rs", "README.md"],
      "extensions": ["scripts/pty_scenarios/lifecycle.js"],
      "panels": [
        {"label": "idle", "send": "",     "wait": 1.5},
        {"label": "slash", "send": "/",   "wait": 0.8},
        {"label": "narrow", "send": "mo", "wait": 0.6, "skip_capture": true}
      ]
    }

`extensions` lists JS sources to load as global extensions. Each entry
is either an object mapping a file name to inline source, or a path
(relative to the repo root) whose file is used. They are written into
the child's `$HOME/.pi/agent/extensions/` so the run exercises the real
global-extension search path (project-local `.pi/extensions` is gated by
project trust, which a fresh temp cwd has not granted).

`final_send` (optional) is a key sequence sent after the last panel and
before the harness falls back to SIGTERM/SIGKILL. Use it to let a
scenario exit *gracefully* — `<C-d>` quits the TUI, which is the only way
the process runs its shutdown path (extension `session_shutdown`).

`send` is literal text plus key tokens: `<Enter> <Esc> <Tab> <BS> <Up>
<Down> <Left> <Right> <PgUp> <PgDn> <Home> <End> <C-a>..<C-z> <Del>`.
Every panel is fed into the *same* process, so panels are cumulative
frames of one interactive session. `skip_capture` drives the UI without
emitting a panel (useful for intermediate keystrokes).
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import sys
import termios
import time

try:
    import pyte
except ImportError:  # pragma: no cover
    sys.exit("python module 'pyte' is required: pip install pyte")

try:
    import wcwidth  # noqa: F401  (pyte uses it internally; fail early)
except ImportError:  # pragma: no cover
    sys.exit("python module 'wcwidth' is required: pip install wcwidth")

from PIL import Image, ImageDraw, ImageFont

# ---------------------------------------------------------------- keys

_KEY_TOKENS = {
    "<Enter>": "\r",
    "<Esc>": "\x1b",
    "<Tab>": "\t",
    "<BS>": "\x7f",
    "<Space>": " ",
    "<Up>": "\x1b[A",
    "<Down>": "\x1b[B",
    "<Right>": "\x1b[C",
    "<Left>": "\x1b[D",
    "<Home>": "\x1b[H",
    "<End>": "\x1b[F",
    "<PgUp>": "\x1b[5~",
    "<PgDn>": "\x1b[6~",
    "<Del>": "\x1b[3~",
    "<F1>": "\x1bOP",
    "<F2>": "\x1bOQ",
    "<F3>": "\x1bOR",
    "<BackTab>": "\x1b[Z",
}
for _c in "abcdefghijklmnopqrstuvwxyz":
    _KEY_TOKENS[f"<C-{_c}>"] = chr(ord(_c) - ord("a") + 1)
_KEY_TOKENS["<C-[>"] = "\x1b"


def encode_keys(text: str) -> bytes:
    """Expand `<Enter>`-style tokens in `text` into raw terminal bytes."""
    out = []
    i = 0
    while i < len(text):
        if text[i] == "<":
            end = text.find(">", i)
            if end != -1:
                token = text[i : end + 1]
                if token in _KEY_TOKENS:
                    out.append(_KEY_TOKENS[token])
                    i = end + 1
                    continue
        out.append(text[i])
        i += 1
    return "".join(out).encode("utf-8")


# ------------------------------------------------------------ palette

# pi-tui's built-in dark theme is close to this; the harness only needs
# *legible* colors, not byte-identical ones (the PNG is for humans).
DEFAULT_BG = (24, 24, 28)
DEFAULT_FG = (212, 212, 212)

_NAMED = {
    "black": (0, 0, 0),
    "red": (205, 49, 49),
    "green": (13, 188, 121),
    "brown": (229, 229, 16),
    "yellow": (229, 229, 16),
    "blue": (36, 114, 200),
    "magenta": (188, 63, 188),
    "cyan": (17, 168, 205),
    "white": (229, 229, 229),
    "brightblack": (102, 102, 102),
    "brightred": (241, 76, 76),
    "brightgreen": (35, 209, 139),
    "brightbrown": (245, 245, 67),
    "brightyellow": (245, 245, 67),
    "brightblue": (59, 142, 234),
    "brightmagenta": (214, 112, 214),
    "brightcyan": (41, 184, 219),
    "brightwhite": (255, 255, 255),
    "default": DEFAULT_FG,
}


def parse_color(value, fallback):
    if value is None:
        return fallback
    if isinstance(value, tuple):
        return value
    if value.startswith("#") and len(value) == 7:
        return tuple(int(value[i : i + 2], 16) for i in (1, 3, 5))
    # pyte reports 24-bit SGR (38;2;r;g;b / 48;2;r;g;b) as a *bare* six-digit
    # hex string, so a theme that uses hex colors (pi's dark theme does) would
    # otherwise fall through to the default and every panel would render
    # monochrome.
    if len(value) == 6 and all(ch in "0123456789abcdefABCDEF" for ch in value):
        return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))
    key = value.replace("_", "").replace(" ", "").lower()
    if key in _NAMED:
        return _NAMED[key]
    if key.startswith("bright"):
        base = _NAMED.get(key[6:])
        if base:
            return tuple(min(255, c + 60) for c in base)
    return fallback


# ------------------------------------------------------------- render


def load_font(size: int):
    for name in (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ):
        if os.path.exists(name):
            return ImageFont.truetype(name, size)
    return ImageFont.load_default()


def load_bold_font(size: int):
    for name in (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
    ):
        if os.path.exists(name):
            return ImageFont.truetype(name, size)
    return load_font(size)


class Renderer:
    def __init__(self, font_size: int = 16, scale: int = 2):
        self.font = load_font(font_size * scale)
        self.bold = load_bold_font(font_size * scale)
        self.scale = scale
        adv = self.font.getlength("M")
        self.cell_w = int(round(adv))
        self.cell_h = int(round(self.font.getmetrics()[0] + self.font.getmetrics()[1]))
        self.caption_h = 26 * 1
        self.caption_font = load_font(13 * 1)

    def render_panel(self, screen, caption: str) -> Image.Image:
        cols = screen.columns
        rows = screen.lines
        width = cols * self.cell_w + 2
        height = rows * self.cell_h + self.caption_h + 2
        img = Image.new("RGB", (width, height), (10, 10, 12))
        draw = ImageDraw.Draw(img)

        # caption bar
        draw.rectangle([0, 0, width, self.caption_h], fill=(42, 44, 54))
        draw.text((8, 6), caption, font=self.caption_font, fill=(238, 238, 240))

        top = self.caption_h + 1
        for y in range(rows):
            row = screen.buffer[y]
            for x in range(cols):
                cell = row[x]
                data = cell.data
                # pyte reports untouched attributes as the literal string
                # "default"; resolve those against the theme *per channel*
                # (mapping "default" through the named table would paint the
                # background with the foreground color).
                fg = (
                    DEFAULT_FG
                    if cell.fg in (None, "default")
                    else parse_color(cell.fg, DEFAULT_FG)
                )
                bg = (
                    DEFAULT_BG
                    if cell.bg in (None, "default")
                    else parse_color(cell.bg, DEFAULT_BG)
                )
                # pyte's `Char` is a namedtuple of flags (0.8.x), not an
                # attribute set — read the flags directly.
                if getattr(cell, "reverse", False):
                    fg, bg = (bg if bg != DEFAULT_BG else DEFAULT_FG), (
                        fg if fg != DEFAULT_FG else DEFAULT_BG
                    )
                px = 1 + x * self.cell_w
                py = top + y * self.cell_h
                if bg != DEFAULT_BG:
                    draw.rectangle(
                        [px, py, px + self.cell_w, py + self.cell_h], fill=bg
                    )
                if not data or data == " ":
                    continue
                font = self.bold if getattr(cell, "bold", False) else self.font
                draw.text((px, py), data, font=font, fill=fg)

        # cursor marker (hollow box) — helps justify "keystrokes landed"
        if not screen.cursor.hidden:
            px = 1 + screen.cursor.x * self.cell_w
            py = top + screen.cursor.y * self.cell_h
            draw.rectangle(
                [px, py, px + self.cell_w - 1, py + self.cell_h - 1],
                outline=(140, 140, 160),
            )
        return img

    def stack(self, panels) -> Image.Image:
        if not panels:
            raise SystemExit("no panels captured")
        gap = 10
        width = max(p.width for p in panels)
        height = sum(p.height for p in panels) + gap * (len(panels) - 1)
        img = Image.new("RGB", (width, height), (10, 10, 12))
        y = 0
        for panel in panels:
            img.paste(panel, (0, y))
            y += panel.height + gap
        return img


# --------------------------------------------------------------- pty


def spawn(binary, args, cwd, env, cols, rows):
    master, slave = pty.openpty()
    fcntl.ioctl(
        slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0)
    )
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


def pump(master, stream, seconds, chunk=1 << 16):
    """Feed PTY output into the VT emulator for `seconds`."""
    deadline = time.time() + seconds
    got = bytearray()
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        ready, _, _ = select.select([master], [], [], min(remaining, 0.05))
        if not ready:
            continue
        try:
            data = os.read(master, chunk)
        except OSError:
            break
        if not data:
            break
        got += data
        stream.feed(data.decode("utf-8", "replace"))
    return bytes(got)


def drain(master, stream, idle=0.35, hard_timeout=3.0):
    """Read until the child has been quiet for `idle` seconds."""
    last = time.time()
    start = last
    while time.time() - last < idle and time.time() - start < hard_timeout:
        ready, _, _ = select.select([master], [], [], 0.05)
        if not ready:
            continue
        try:
            data = os.read(master, 1 << 16)
        except OSError:
            break
        if not data:
            break
        last = time.time()
        stream.feed(data.decode("utf-8", "replace"))


def snapshot(screen) -> str:
    lines = [line.rstrip() for line in screen.display]
    while lines and not lines[-1]:
        lines.pop()
    body = "\n".join(lines)
    return body


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True, help="path to the built `pi` binary")
    ap.add_argument("--steps", required=True, help="scenario JSON file")
    ap.add_argument("--out", required=True, help="PNG output path")
    ap.add_argument("--text-out", default=None, help="text dump path (default: <out>.txt)")
    ap.add_argument("--home", default=None, help="HOME for the child (default: fresh temp dir)")
    ap.add_argument("--cwd", default=None, help="cwd for the child (default: temp fixture dir)")
    ap.add_argument("--keep-temp", action="store_true")
    ap.add_argument("--font-size", type=int, default=15)
    ap.add_argument("--scale", type=int, default=2)
    args = ap.parse_args()

    with open(args.steps, encoding="utf-8") as fh:
        scenario = json.load(fh)

    cols = scenario.get("cols", 120)
    rows = scenario.get("rows", 34)
    panels = scenario["panels"]

    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    home = args.home or os.path.join("/tmp", f"pi-pty-home-{os.getpid()}")
    cwd = args.cwd or os.path.join("/tmp", f"pi-pty-cwd-{os.getpid()}")
    if args.home is None and os.path.isdir(home):
        shutil.rmtree(home)
    if args.cwd is None and os.path.isdir(cwd):
        shutil.rmtree(cwd)
    os.makedirs(home, exist_ok=True)
    os.makedirs(cwd, exist_ok=True)
    # Fixture files so `@`-completion has stable, reviewable candidates.
    for rel in scenario.get("fixtures", []):
        path = os.path.join(cwd, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("// fixture for pty_capture.py\n")
    # Real extension sources, under the global search path so the run
    # covers extension loading + the event fan-out end to end.
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

    env = dict(os.environ)
    env.update(
        {
            "HOME": home,
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8",
            "COLUMNS": str(cols),
            "LINES": str(rows),
            "NO_COLOR": "",
        }
    )
    env.pop("PI_HOME", None)

    screen = pyte.Screen(cols, rows)
    screen.set_mode(pyte.modes.LNM)  # CRLF semantics like a real terminal
    stream = pyte.Stream(screen)
    master, pid = spawn(os.path.abspath(args.bin), scenario.get("args", []), cwd, env, cols, rows)

    cards = []
    text_sections = []
    try:
        drain(master, stream, idle=0.6, hard_timeout=8.0)
        for index, panel in enumerate(panels, start=1):
            send = panel.get("send", "")
            if send:
                os.write(master, encode_keys(send))
            wait = float(panel.get("wait", 0.7))
            if wait > 0:
                pump(master, stream, wait)
            if panel.get("skip_capture"):
                continue
            label = panel.get("label") or f"panel {index}"
            caption = scenario.get("caption", "") or ""
            head = f"[{index}] {label}" if not caption else f"{caption}  |  [{index}] {label}"
            cards.append((head, snapshot(screen), screen.cursor.x, screen.cursor.y))
    finally:
        send = scenario.get("final_send")
        exited = False
        status = 0
        if send:
            try:
                os.write(master, encode_keys(send))
                drain(master, stream, idle=0.6, hard_timeout=10.0)
            except OSError:
                pass  # the child may already be gone; the kill path below decides
            # Give the graceful path a moment to finish (shutdown hooks,
            # session flush) before the kill fallback below.
            deadline = time.time() + 5.0
            while time.time() < deadline:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    exited = True
                    break
                time.sleep(0.1)
        if exited:
            if os.WIFEXITED(status):
                print(f"child exited on {send!r}: code {os.WEXITSTATUS(status)}")
            else:
                print(f"child exited on {send!r}: signal {os.WTERMSIG(status)}")
        else:
            print(f"child did not exit on {send!r}; sending SIGTERM/SIGKILL")
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(0.2)
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        os.close(master)
        if not args.keep_temp:
            shutil.rmtree(home, ignore_errors=True)
            shutil.rmtree(cwd, ignore_errors=True)

    renderer = Renderer(font_size=args.font_size, scale=args.scale)
    images = [renderer.render_panel(screen, head) for head, _, _, _ in cards]
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    renderer.stack(images).save(args.out)

    text_out = args.text_out or (args.out + ".txt")
    with open(text_out, "w", encoding="utf-8") as fh:
        fh.write(f"# {os.path.basename(args.out)} — chars, {cols}x{rows} cells\n")
        for head, body, cx, cy in cards:
            fh.write(f"\n===== {head}  (cursor {cx},{cy}) =====\n{body}\n")

    print(f"wrote {args.out}")
    print(f"wrote {text_out}")
    for head, body, cx, cy in cards:
        print(f"--- {head} (cursor {cx},{cy}) ---")
        print(body)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

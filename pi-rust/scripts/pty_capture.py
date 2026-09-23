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
<Down> <Left> <Right> <PgUp> <PgDn> <Home> <End> <C-a>..<C-z> <Del>
<M-a>..<M-z> <M-Up> <M-Down> <M-Left> <M-Right>`.
Every panel is fed into the *same* process, so panels are cumulative
frames of one interactive session. `skip_capture` drives the UI without
emitting a panel (useful for intermediate keystrokes). `wait_for` (with
`wait_timeout`, default 8s) keeps pumping until that text is on the grid
before the frame is frozen, which is how an async panel (a model reply, a
completed tool call) is captured *after* the thing it claims happened.

Panels can also carry `expect` / `reject` / `probe` / `xfail` /
`xfail_reject` assertions over their frozen grid — see the "panel assertions"
section below. A failing `expect`/`reject`, or an `xfail` that unexpectedly
passes, makes the harness exit non-zero with the failing needle named. Use
`probe` when a behaviour only a *fixed* binary can show: an unmet probe is
reported as XFAIL, so one scenario documents both sides of an A/B (`probe`
rows that PASS name the binary's post-fix behaviour).

Every emitted panel is rendered from a **frozen copy of the terminal at
that panel's moment**, not from the emulator's final state, and the
harness prints a per-panel frame hash. Before the first panel the harness
pumps until the app has painted *something* — a startup that outran the
first sleep used to produce an empty first panel that still looked like
evidence — and `wait_for` does the same per panel. Set
`"distinct_panels": true` to turn "two adjacent panels are byte-identical"
into a hard failure; the harness then exits non-zero instead of shipping a
collage whose captions claim interaction the images do not show.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import time

try:
    import fcntl
    import pty
    import termios
except ImportError:  # pragma: no cover - Windows has none of the three
    # The POSIX backend below (`spawn` / `pump` / `drain`) needs them, but
    # everything else in this module — key encoding, the pyte renderer, the
    # panel assertions — is platform neutral and shared with
    # `pty_capture_win.py`, the ConPTY backend. Keeping the import soft is
    # what lets that module `import pty_capture` on Windows instead of
    # forking a second copy of the renderer. On POSIX nothing changes.
    fcntl = None
    pty = None
    termios = None

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
    # Alt+arrow in the xterm CSI-modifier encoding (`ESC [ 1 ; 3 A` = "cursor up
    # with modifier 3 = Alt"), which is what a real terminal sends and what
    # crossterm decodes into Alt+Arrow. Needed by the `/scoped-models` reorder
    # chords (`app.models.reorderUp` / `reorderDown` default to
    # `alt+up` / `alt+down`).
    #
    # The deprecated `ESC ESC [ A` form must not be used here: the first `ESC`
    # is delivered on its own and closes whatever overlay is open before the
    # arrow ever arrives (LUM-1274 measured exactly that).
    "<M-Up>": "\x1b[1;3A",
    "<M-Down>": "\x1b[1;3B",
    "<M-Left>": "\x1b[1;3D",
    "<M-Right>": "\x1b[1;3C",
}
for _c in "abcdefghijklmnopqrstuvwxyz":
    _KEY_TOKENS[f"<C-{_c}>"] = chr(ord(_c) - ord("a") + 1)
    # Alt+letter arrives as `ESC` followed by the letter (the `metaSendsEscape`
    # terminal convention), which crossterm decodes as Alt+Char.
    _KEY_TOKENS[f"<M-{_c}>"] = "\x1b" + _c
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


# Monospace candidates, first match wins. The Windows entries keep the
# harness usable from the ConPTY backend (`pty_capture_win.py`), where the
# Linux paths do not exist and `ImageFont.load_default()` would render the
# frames in a proportional bitmap face that breaks the cell grid.
_MONO_FONTS = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "C:/Windows/Fonts/consola.ttf",
    "C:/Windows/Fonts/lucon.ttf",
)
_MONO_BOLD_FONTS = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
    "C:/Windows/Fonts/consolab.ttf",
)
# CJK-capable faces, preferred whenever the frames contain wide glyphs — a
# monospace face without CJK draws every `你好` cell as a `.notdef` box, and
# a screenshot that shows boxes is not evidence about a Chinese draft.
_CJK_FONTS = (
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/simsun.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
)
_CJK_BOLD_FONTS = ("C:/Windows/Fonts/msyhbd.ttc",) + _CJK_FONTS


def _needs_cjk(text: str) -> bool:
    return any(ord(ch) > 0x2E7F for ch in text)


def _first_font(names, size):
    for name in names:
        if os.path.exists(name):
            try:
                return ImageFont.truetype(name, size)
            except OSError:  # a .ttc whose face index is unusable — try the next
                continue
    return None


def load_font(size: int, text: str = ""):
    names = (*_CJK_FONTS, *_MONO_FONTS) if _needs_cjk(text) else _MONO_FONTS
    font = _first_font(names, size)
    return font if font is not None else ImageFont.load_default()


def load_bold_font(size: int, text: str = ""):
    names = (*_CJK_BOLD_FONTS, *_MONO_BOLD_FONTS) if _needs_cjk(text) else _MONO_BOLD_FONTS
    font = _first_font(names, size)
    return font if font is not None else load_font(size, text)


class Renderer:
    def __init__(self, font_size: int = 16, scale: int = 2, text: str = ""):
        # `text` is the concatenated frame content of the run; it only picks
        # the font family (monospace vs CJK-capable), the grid stays driven by
        # the cell metrics of whichever face won.
        self.font = load_font(font_size * scale, text)
        self.bold = load_bold_font(font_size * scale, text)
        self.scale = scale
        adv = self.font.getlength("M")
        self.cell_w = int(round(adv))
        self.cell_h = int(round(self.font.getmetrics()[0] + self.font.getmetrics()[1]))
        self.caption_h = 26 * 1
        self.caption_font = load_font(13 * 1)

    def sheet(
        self, images: list[Image.Image], columns: int = 2, max_width: int = 2400
    ) -> Image.Image:
        """Tile equal-sized panels into a grid, downscaled to a readable width.

        A 17-panel vertical collage is ~11k px tall and awkward to view; the
        sheet keeps the same panels in a grid at a width a human can actually
        scan. Downscale is integer, so glyphs stay crisp.
        """
        cols = max(1, columns)
        width, height = images[0].size
        factor = 1
        while (cols * width) // factor > max_width and factor < 8:
            factor += 1
        if factor > 1:
            width, height = width // factor, height // factor
            images = [img.resize((width, height), Image.LANCZOS) for img in images]
        rows = (len(images) + cols - 1) // cols
        out = Image.new("RGB", (width * cols, height * rows), (10, 10, 12))
        for index, img in enumerate(images):
            out.paste(img, ((index % cols) * width, (index // cols) * height))
        return out

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


# Raw bytes every backend has read from the pty, in order.
#
# The pyte grid is the right target for everything a user can *see*, but a few
# sequences are deliberately invisible: OSC 0 (terminal title) and OSC 8
# (hyperlinks) change the terminal's state, not its cells. `raw_expect` /
# `raw_reject` assert against this trace instead, bounded per panel by
# `raw_since()` so one panel's assertion cannot be satisfied by an earlier
# panel's output. A backend that reads bytes is responsible for calling
# `note_raw()` — `pump` / `drain` do it here, and the Windows backend does it
# in `ConPty.drain`.
RAW_TRACE = bytearray()


def note_raw(data: bytes) -> None:
    """Record bytes a backend just read from the pty."""
    RAW_TRACE.extend(data)


def raw_since(mark: int) -> str:
    """The raw trace from byte `mark` on, decoded for substring assertions."""
    return bytes(RAW_TRACE[mark:]).decode("utf-8", "replace")


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
        note_raw(data)
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
        note_raw(data)
        stream.feed(data.decode("utf-8", "replace"))


def send_steps(panel: dict) -> list[tuple[str, float]]:
    """Normalize a panel's `send` into `[(key_tokens, pause_after_seconds), …]`.

    A plain string is one step with no pause. A list lets a scenario separate
    keys in time — see the schema note above `evaluate_panel` for why a
    `text` + `Enter` burst needs it (LUM-1461's paste-burst classifier treats
    an instantaneous run as a paste, including its trailing newline).
    """
    send = panel.get("send", "")
    if not send:
        return []
    if isinstance(send, str):
        return [(send, 0.0)]
    steps: list[tuple[str, float]] = []
    for step in send:
        if isinstance(step, str):
            steps.append((step, 0.0))
        else:
            steps.append((step.get("keys", ""), float(step.get("pause", 0.0))))
    return steps


def snapshot(screen) -> str:
    lines = [line.rstrip() for line in screen.display]
    while lines and not lines[-1]:
        lines.pop()
    body = "\n".join(lines)
    return body


# The VT sequence a full-screen app sends to switch to the alternate screen.
_ALT_SCREEN_ENTER = "\x1b[?1049h"


class AltScreenFeeder:
    r"""Feed raw terminal bytes into a `pyte` screen, one alt-screen switch aware.

    `pyte` 0.8.x has no alternate-screen buffer: mode 1049 is an unknown
    private mode, so a `\\x1b[?1049h` is *ignored* and everything the app
    printed to the main screen stays on the grid. A full-screen app then
    paints only the cells it owns — its renderer is differential, it assumes
    the terminal shows its previous frame — so the two screens end up
    interleaved. Measured, not theorised: on Windows the main-screen
    `Use “pi --approve” for this run…` banner showed through the alt-screen
    header, and a frame read

    ```text
    pi v0.1.0ers\ADMINI~1\AppData\Local\Temp\…\pi-pty-cwd-1343
    ```

    i.e. the banner's path tail left over in columns the app never wrote.
    Panels captured like that are wrong about what a user sees, which is the
    one thing a screenshot must not be.

    The feeder therefore clears the emulated grid when the app enters the
    alternate screen. `Screen.reset()` is used in place, so the `screen`
    object keeps its identity and every caller can keep holding it. Leaving
    the alternate screen (`1049l`) is left alone: a capture ends inside the
    alt screen, and re-showing the main screen would be a different session.
    """

    def __init__(self, screen):
        self.screen = screen
        self.stream = pyte.Stream(screen)

    def feed(self, text: str) -> None:
        while _ALT_SCREEN_ENTER in text:
            head, _, text = text.partition(_ALT_SCREEN_ENTER)
            self.stream.feed(head)
            self.screen.reset()
            self.screen.set_mode(pyte.modes.LNM)
        self.stream.feed(text)


# ------------------------------------------------------- panel assertions
#
# A screenshot plus a caption is a claim; these turn the claim into a check
# the harness evaluates. Each panel may carry:
#
#   "expect":        ["text"]   must be present           -> FAIL when missing
#   "reject":        ["text"]   must be absent            -> FAIL when present
#   "probe":         ["text"]   A/B evidence             -> XFAIL when missing
#   "xfail":         ["text"]   a KNOWN defect: present today -> FAIL when missing
#   "xfail_reject":  ["text"]   a known defect: present today -> FAIL when absent
#   "raw_expect":    ["\u001b]0;pi"]  present in the pty BYTE stream -> FAIL when missing
#   "raw_reject":    ["\u001b]0;pi"]  absent from the byte stream     -> FAIL when present
#
# `raw_*` exists for state-changing-but-invisible sequences: a terminal title
# (OSC 0) and a hyperlink (OSC 8) are not cell content, so the pyte grid can
# never show them. They are evaluated against the bytes this panel's own window
# produced (`RAW_TRACE`), not the whole session.
#
# `probe` is for a behaviour that only a *fixed* binary can show: it records
# "this binary does not satisfy it yet" as XFAIL instead of failing the run, so
# one scenario documents both sides of an A/B. `xfail` is a stronger claim — a
# defect that must be fixed — so a marker that unexpectedly starts passing is a
# hard failure here and has to be moved to `expect`.
#
# An entry is either a plain substring, or an object:
#
#   {"text": "/mo", "count": 2}      {"text": "x", "count": ">=2"}
#   {"text": "x", "count": "2..4"}   {"count": ">=2", "regex": "^> "}
#   {"text": "...", "why": "LUM-1262"}
#
# `count` accepts an int, "N..M", ">=N" or "<=N"; without it the entry asserts
# presence (expect/xfail_reject) or absence (reject/xfail). `regex` switches the
# needle from a literal substring to a pattern (counted per line).
#
# A panel's `send` is normally one string of key tokens, written in one go.
# When the *timing* of the keys matters it can instead be a list of steps, each
# a token string or `{"keys": ..., "pause": seconds}`:
#
#   "send": ["/retitle", {"pause": 0.3}, "<Enter>"]
#
# That form exists because of the paste-burst classifier (LUM-1461): a run of
# three or more plain characters delivered within `PASTE_BURST_CHAR_INTERVAL`
# is treated as a paste, and an `Enter` inside the burst window is inserted as
# a newline instead of submitting. A harness write is instantaneous, so
# `"/retitle<Enter>"` is *exactly* that burst; a scenario that means "type a
# command and press Enter" has to space the two apart, the way a human does.

_STATUS_FOR = {
    # kind -> (status when the needle is found, status when it is not)
    "expect": ("PASS", "FAIL"),
    "reject": ("FAIL", "PASS"),
    # A/B evidence: describes post-fix behaviour that the binary under test may
    # not have yet. Unmet is recorded as XFAIL (with its reason) instead of a
    # failure, so the same scenario documents "before" and "after" runs; it is
    # not a gate.
    "probe": ("PASS", "XFAIL"),
    # A known defect that is supposed to disappear: it must keep failing until
    # the fix lands, and *passing* is a hard failure here, so the marker cannot
    # outlive the bug.
    "xfail": ("XPASS", "XFAIL"),
    "xfail_reject": ("XFAIL", "XPASS"),
    # Raw-stream assertions (see the schema note above). `raw_expect` is a hard
    # requirement like `expect`: a title either reached the terminal or it did
    # not, and there is no "not implemented yet" reading of it.
    "raw_expect": ("PASS", "FAIL"),
    "raw_reject": ("FAIL", "PASS"),
}

# Assertion kinds whose needles are matched against the raw byte stream rather
# than the frozen grid.
RAW_KINDS = ("raw_expect", "raw_reject")


def parse_count(spec):
    """Normalize a `count` field into an inclusive `(low, high)` range."""
    if spec is None:
        return None
    if isinstance(spec, int):
        return (spec, spec)
    text = str(spec).strip()
    if text.startswith(">="):
        return (int(text[2:]), None)
    if text.startswith("<="):
        return (None, int(text[2:]))
    if ".." in text:
        low, _, high = text.partition("..")
        return (int(low), int(high))
    return (int(text), int(text))


def count_needle(body: str, needle: str, pattern: bool, count) -> tuple[int, bool]:
    """Return `(occurrences, satisfied)` for one needle against a panel grid."""
    if pattern:
        found = len(re.findall(needle, body, flags=re.MULTILINE))
    else:
        found = body.count(needle)
    bounds = parse_count(count)
    if bounds is None:
        return found, found > 0
    low, high = bounds
    ok = (low is None or found >= low) and (high is None or found <= high)
    return found, ok


def evaluate_panel(panel: dict, body: str, raw: str = "") -> list[dict]:
    """Evaluate one frozen panel's assertions. One row per assertion.

    `body` is the panel's grid, `raw` the byte window this panel produced —
    `raw_expect` / `raw_reject` match against the latter.
    """
    rows: list[dict] = []
    for kind, found_status, absent_status in (
        ("expect", *_STATUS_FOR["expect"]),
        ("reject", *_STATUS_FOR["reject"]),
        ("probe", *_STATUS_FOR["probe"]),
        ("xfail", *_STATUS_FOR["xfail"]),
        ("xfail_reject", *_STATUS_FOR["xfail_reject"]),
        ("raw_expect", *_STATUS_FOR["raw_expect"]),
        ("raw_reject", *_STATUS_FOR["raw_reject"]),
    ):
        target = raw if kind in RAW_KINDS else body
        for entry in panel.get(kind) or []:
            if isinstance(entry, str):
                spec = {"text": entry}
            else:
                spec = dict(entry)
            needle = spec.get("text", "")
            is_pattern = "regex" in spec or spec.get("pattern") is True
            if is_pattern and not needle:
                needle = spec["regex"]
            found, satisfied = count_needle(target, needle, is_pattern, spec.get("count"))
            rows.append(
                {
                    "kind": kind,
                    "needle": needle,
                    "pattern": is_pattern,
                    "count": spec.get("count"),
                    "found": found,
                    "status": found_status if satisfied else absent_status,
                    "why": spec.get("why", ""),
                }
            )
    return rows


def describe_assertion(row: dict) -> str:
    counts = f" (x{row['found']})" if row["count"] is not None else ""
    why = f"  — {row['why']}" if row["why"] else ""
    return f"{row['kind']:<12} {row['needle']!r}{counts}{why}"


def summarize_assertions(rows: list[dict], allow_xpass: bool) -> tuple[bool, list[str]]:
    """Return `(failed, report lines)` for every panel's assertions."""
    tally = {"PASS": 0, "FAIL": 0, "XFAIL": 0, "XPASS": 0}
    failures: list[str] = []
    populated = [(head, panel_rows) for head, panel_rows in rows if panel_rows]
    for head, panel_rows in populated:
        for row in panel_rows:
            status = row["status"]
            if status == "XPASS" and allow_xpass:
                status = "XFAIL"
            tally[status] += 1
            if status in ("FAIL", "XPASS"):
                name = head.split("  |  ")[-1]
                failures.append(f"{name}: {status} {describe_assertion(row)}")
    checks = sum(tally.values())
    report = [
        f"assertions: {checks} checks over {len(populated)} panels — "
        f"{tally['PASS']} PASS, {tally['FAIL']} FAIL, "
        f"{tally['XFAIL']} XFAIL, {tally['XPASS']} XPASS"
    ]
    return bool(failures), report + failures


def pump_until(master, stream, predicate, timeout: float, slice_s: float = 0.25) -> bool:
    """Pump the PTY until `predicate()` holds or `timeout` elapses.

    Startup timing is not constant: the app loads models and extensions before
    its first paint, and a scenario that outruns that paints a blank panel that
    still *looks* like evidence. Waiting on a condition instead of a fixed
    sleep is what makes a first panel trustworthy.
    """
    deadline = time.time() + timeout
    while True:
        if predicate():
            return True
        if time.time() >= deadline:
            return False
        pump(master, stream, slice_s)


def child_alive(pid) -> bool:
    """Has the app process already exited? Reaps it if so.

    A panel that claims "the UI is still up" is only worth something if the
    process is provably still up, so every frame records this.
    """
    if pid in _REAPED:
        return False
    try:
        wpid, status = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        _REAPED[pid] = 0
        return False
    if wpid == 0:
        return True
    _REAPED[pid] = status
    return False


def reap(pid) -> None:
    """Best-effort final reap that tolerates an already-reaped child."""
    if pid in _REAPED or child_alive(pid) is False:
        return
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


_REAPED = {}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True, help="path to the built `pi` binary")
    ap.add_argument("--steps", required=True, help="scenario JSON file")
    ap.add_argument("--out", required=True, help="PNG output path")
    ap.add_argument("--text-out", default=None, help="text dump path (default: <out>.txt)")
    ap.add_argument(
        "--home",
        default=None,
        help="HOME for the child (default: fresh temp dir; an explicit --home is kept)",
    )
    ap.add_argument(
        "--cwd",
        default=None,
        help="cwd for the child (default: fresh temp dir; an explicit --cwd is kept)",
    )
    ap.add_argument("--keep-temp", action="store_true")
    ap.add_argument("--font-size", type=int, default=15)
    ap.add_argument("--scale", type=int, default=2)
    ap.add_argument(
        "--allow-xpass",
        action="store_true",
        help="treat a fixed `xfail` marker as a warning instead of a failure",
    )
    ap.add_argument(
        "--sheet",
        type=int,
        default=0,
        help="tile the panels into N columns instead of one tall column",
    )
    ap.add_argument(
        "--sheet-width",
        type=int,
        default=2400,
        help="target width in px for --sheet (panels are downscaled to fit)",
    )
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
    #
    # A scenario can also name `"fixtures": {"name": "contents"}` to give a
    # fixture real contents — the `app.editor.external` scenarios use it to
    # install a fake `$EDITOR` (LUM-1308). A fixture whose name ends in `.sh`
    # is made executable: it is about to be run as a command, and the child
    # resolves `./editor.sh` relative to its own cwd.
    declared_fixtures = scenario.get("fixtures", [])
    if isinstance(declared_fixtures, dict):
        fixture_items = declared_fixtures.items()
    else:
        fixture_items = ((rel, "// fixture for pty_capture.py\n") for rel in declared_fixtures)
    for rel, contents in fixture_items:
        path = os.path.join(cwd, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(contents)
        if rel.endswith(".sh"):
            os.chmod(path, 0o755)
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
            "HOME": home,
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8",
            "COLUMNS": str(cols),
            "LINES": str(rows),
            "NO_COLOR": "",
        }
    )
    # Scenario-level overrides last, so a scenario can pin `EDITOR`/`VISUAL`
    # (the `app.editor.external` scenarios) without the harness guessing. An
    # explicit empty value removes the variable instead of exporting "".
    for key, value in (scenario.get("env") or {}).items():
        if value == "":
            env.pop(key, None)
        else:
            env[key] = value
    env.pop("PI_HOME", None)

    screen = pyte.Screen(cols, rows)
    screen.set_mode(pyte.modes.LNM)  # CRLF semantics like a real terminal
    # `AltScreenFeeder` clears the grid on `\x1b[?1049h` (pyte has no
    # alternate-screen buffer); it feeds the same `screen` object, so every
    # `snapshot` / `deepcopy` below is unchanged.
    stream = AltScreenFeeder(screen)
    master, pid = spawn(os.path.abspath(args.bin), scenario.get("args", []), cwd, env, cols, rows)

    cards = []
    assertion_rows: list[tuple[str, list[dict]]] = []
    text_sections = []
    # Byte offset the current panel's raw window starts at; panels advance it,
    # so a `raw_expect` can only be satisfied by this panel's own output.
    raw_mark = 0
    try:
        drain(master, stream, idle=0.6, hard_timeout=8.0)
        if not pump_until(
            master, stream, lambda: bool(snapshot(screen).strip()), timeout=12.0
        ):
            print(
                "WARN: the app painted nothing within 12s of startup — the first "
                "panel may be a blank frame",
                file=sys.stderr,
            )
        for index, panel in enumerate(panels, start=1):
            panel_raw_mark = raw_mark
            for keys, pause in send_steps(panel):
                if keys:
                    os.write(master, encode_keys(keys))
                if pause > 0:
                    pump(master, stream, pause)
            raw_mark = len(RAW_TRACE)
            wait = float(panel.get("wait", 0.7))
            if wait > 0:
                pump(master, stream, wait)
            # Optional synchronization: `"wait_for": "text"` keeps pumping
            # until that text is on the grid (or `wait_timeout` expires), so a
            # panel that claims "the reply landed" cannot silently capture the
            # frame from before the reply.
            wait_for = panel.get("wait_for")
            if wait_for and wait_for not in snapshot(screen):
                timeout = float(panel.get("wait_timeout", max(wait, 8.0)))
                if not pump_until(
                    master, stream, lambda: wait_for in snapshot(screen), timeout
                ):
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
            # Freeze this panel's terminal state. The emulator keeps mutating
            # as later keys arrive, so rendering `screen` after the loop would
            # paint every panel with the *last* frame (the bug this fixes).
            frame = copy.deepcopy(screen)
            body = snapshot(frame)
            panel_raw = raw_since(panel_raw_mark)
            cards.append(
                (
                    head,
                    body,
                    frame,
                    frame.cursor.x,
                    frame.cursor.y,
                    child_alive(pid),
                )
            )
            assertion_rows.append((head, evaluate_panel(panel, body, panel_raw)))
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
        reap(pid)
        os.close(master)
        if not args.keep_temp:
            # Only the scratch dirs the harness created itself are its to
            # remove. An explicit `--home` / `--cwd` is the caller's fixture
            # — deleting it silently emptied the prepared agent dir that a
            # `/reload`-style scenario depends on, and the run then reported
            # "no file" for a fixture that was there at spawn time.
            if args.home is None:
                shutil.rmtree(home, ignore_errors=True)
            if args.cwd is None:
                shutil.rmtree(cwd, ignore_errors=True)

    renderer = Renderer(
        font_size=args.font_size,
        scale=args.scale,
        # Wide glyphs need a CJK-capable face; the frames know whether any
        # appeared, so the font choice is per-run rather than per-platform.
        text="".join(body for _, body, *_ in cards),
    )
    images = [renderer.render_panel(frame, head) for head, _, frame, _, _, _ in cards]
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    composite = (
        renderer.sheet(images, args.sheet, args.sheet_width) if args.sheet else renderer.stack(images)
    )
    composite.save(args.out)

    # Frame provenance: hash each panel's frozen grid and its rendered pixels,
    # so a reader can tell a real multi-frame collage from one frame repeated.
    hashes = [hashlib.sha256(body.encode("utf-8")).hexdigest()[:12] for _, body, _, _, _, _ in cards]
    pix = [hashlib.sha256(img.tobytes()).hexdigest()[:12] for img in images]
    duplicates = [
        i
        for i in range(1, len(hashes))
        if hashes[i] == hashes[i - 1] or pix[i] == pix[i - 1]
    ]

    text_out = args.text_out or (args.out + ".txt")
    with open(text_out, "w", encoding="utf-8") as fh:
        fh.write(f"# {os.path.basename(args.out)} — chars, {cols}x{rows} cells\n")
        for i, (head, body, _, cx, cy, alive) in enumerate(cards):
            fh.write(
                f"\n===== {head}  (cursor {cx},{cy}, frame {hashes[i]}, px {pix[i]}, "
                f"alive={alive}) =====\n{body}\n"
            )
            for row in assertion_rows[i][1]:
                fh.write(f"  assert {row['status']:<6} {describe_assertion(row)}\n")

    print(f"wrote {args.out}")
    print(f"wrote {text_out}")
    for i, (head, body, _, cx, cy, alive) in enumerate(cards, start=1):
        print(
            f"frame {i:2d}  {hashes[i - 1]}  px {pix[i - 1]}  "
            f"alive={str(alive):5s} {head}"
        )
    # A repeat that is not adjacent is usually legitimate (two panels can end
    # in the same terminal state), but it is worth naming so a reader can see
    # exactly which captions share a grid instead of assuming every caption
    # implies a distinct frame.
    seen: dict[str, int] = {}
    for idx, digest in enumerate(hashes, start=1):
        if digest in seen:
            print(
                f"WARN: panel {idx} renders the same grid as panel {seen[digest]} "
                "(not adjacent; check whether the captions really claim two states)"
            )
        else:
            seen[digest] = idx

    assert_failed, assert_report = summarize_assertions(assertion_rows, args.allow_xpass)
    if any(row for _, rows in assertion_rows for row in rows):
        for head, rows in assertion_rows:
            if not rows:
                continue
            worst = {
                "FAIL": 0,
                "XPASS": 1,
                "XFAIL": 2,
                "PASS": 3,
            }
            print(f"--- {head} assertions ---")
            for row in sorted(rows, key=lambda r: worst[r["status"]]):
                print(f"  {row['status']:<6} {describe_assertion(row)}")
        for line in assert_report[:1]:
            print(line)
        for line in assert_report[1:]:
            print(line, file=sys.stderr)

    if duplicates:
        panels = ", ".join(str(i + 1) for i in duplicates)
        message = (
            f"{'FAIL' if scenario.get('distinct_panels') else 'WARN'}: panel(s) "
            f"[{panels}] are byte-identical to the previous panel — the caption "
            f"claims a state change the frame does not show"
        )
        print(message, file=sys.stderr)
        if scenario.get("distinct_panels"):
            print("wrote the PNG anyway; refusing to report success", file=sys.stderr)
            for i, (head, body, _, cx, cy, alive) in enumerate(cards, start=1):
                print(f"--- {head} (cursor {cx},{cy}, alive={alive}) ---")
                print(body)
            return 1
    if assert_failed:
        print(
            "wrote the PNG and the text dump; refusing to report success because "
            "panel assertions failed (see the FAIL/XPASS rows above)",
            file=sys.stderr,
        )
        return 1
    for head, body, _, cx, cy, alive in cards:
        print(f"--- {head} (cursor {cx},{cy}, alive={alive}) ---")
        print(body)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

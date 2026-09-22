#!/usr/bin/env python3
"""Render a saved frame dump to a PNG — the no-PTY screenshot path.

Why this exists (LUM-1412): `scripts/pty_capture.py` is the repo's canonical
screenshot harness, but it needs a real PTY (`import pty`, `fcntl`, `termios`)
and `pyte`. A Windows runner has neither, and the pi-rust rounds are sometimes
dispatched to one (see `docs/FEATURE_PI_RS_STATUS.md` §"`--assignee pi` 会模糊
命中另一台机器的 agent"). Those rounds can still produce a *frame-buffer*
screenshot: a test prints the cell grid `App::render_snapshot` built (the same
grid the driver hands to `ratatui`), and this script paints that grid.

It is deliberately **not** a substitute for a PTY capture: it renders one
already-frozen frame, so it cannot show keystroke *interaction*, and it carries
no colour information (a dump is text). Screenshots made this way must say so
in their caption; `pty_capture.py` remains the evidence of record for anything
that claims a real terminal did something.

Dump format (what `tests/lum1412_chrome_clip.rs` prints):

    FRAME DUMP cols=44 rows=14
    |pi v0.1.0                                   |
    |hints hidden on a short terminal — Alt+H…   |
    ...
    END FRAME DUMP

A dump may also carry SGR codes (`7` reverse, `1` bold, `4` underline) inside
the cells — `tests/pointer_columns.rs` emits them for a text selection and a
search match. Lines with codes are drawn run by run (reverse = filled cell,
underline = a rule under the run); lines without keep the plain path. Codes
are ignored when measuring columns.

Usage:
    python3 pi-rust/scripts/frame_to_png.py \
        --text pi-rust/docs/screenshots/lum1412-chrome-clip-44x14.txt \
        --out  pi-rust/docs/screenshots/lum1412-chrome-clip-44x14.png \
        --caption "LUM-1412 44x14 frame-buffer render"
"""

from __future__ import annotations

import argparse
import os
import re
import sys

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover
    sys.exit("python module 'Pillow' is required: pip install pillow")

try:
    from wcwidth import wcwidth
except ImportError:  # pragma: no cover
    def wcwidth(ch: str) -> int:  # type: ignore[misc]
        """Fallback for installs without `wcwidth` (see `pty_capture.py`)."""
        return 0 if ord(ch) < 32 else 1

# Same palette as `pty_capture.py`, so a sheet mixing both kinds of panel does
# not look like two different products.
BG = (10, 10, 12)
CAPTION_BG = (42, 44, 54)
FG = (238, 238, 240)
# Reverse-video cells (a text selection, the current search match) and the
# underline of a non-current search match; kept readable on the dark frame.
REVERSE_BG = (206, 212, 222)
REVERSE_FG = (10, 10, 12)
UNDERLINE_FG = (140, 148, 162)

_FONT_CANDIDATES = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "C:/Windows/Fonts/consola.ttf",
    "C:/Windows/Fonts/lucon.ttf",
)

# Used instead when a dump contains CJK / fullwidth / emoji code points, which
# the monospace faces above do not cover (they render as blank boxes). Every
# CJK face here is designed so one ideograph advance equals two half-width
# advances, which is what the dump's column padding assumes.
_CJK_FONT_CANDIDATES = (
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/simsun.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/System/Library/Fonts/PingFang.ttc",
)


def _needs_cjk_font(text: str) -> bool:
    """True when `text` contains a code point a Latin-only face cannot draw."""
    return any(ord(ch) > 0x2E7F for ch in text)


def load_font(size: int, text: str = ""):
    candidates = _FONT_CANDIDATES
    if _needs_cjk_font(text):
        candidates = _CJK_FONT_CANDIDATES + _FONT_CANDIDATES
    for name in candidates:
        if os.path.exists(name):
            return ImageFont.truetype(name, size)
    return ImageFont.load_default()


_DUMP_HEADER = re.compile(r"FRAME DUMP\s+cols=(\d+)\s+rows=(\d+)")


def read_dump(path: str) -> tuple[int, int, list[str]]:
    """Return `(cols, rows, lines)` from a frame dump file."""
    with open(path, encoding="utf-8") as handle:
        raw = handle.read()
    lines: list[str] = []
    cols = rows = 0
    inside = False
    for line in raw.splitlines():
        match = _DUMP_HEADER.search(line)
        if match:
            cols, rows = int(match.group(1)), int(match.group(2))
            inside = True
            continue
        if not inside:
            continue
        if line.strip() == "END FRAME DUMP":
            break
        lines.append(line[1:-1] if line.startswith("|") and line.endswith("|") else line)
    if not lines:
        sys.exit(f"no frame dump found in {path}: expected a `FRAME DUMP …` marker")
    if not cols:
        cols = max(len(line) for line in lines)
    if not rows:
        rows = len(lines)
    return cols, rows, lines


def _pad_to_columns(line: str, cols: int) -> str:
    """Pad `line` to `cols` **terminal columns**, not characters.

    The frame dump is logical text (a wide glyph appears once), so a CJK row
    has fewer characters than columns and `str.ljust` would leave the right
    edge ragged.
    """
    width = sum(max(wcwidth(ch), 0) for ch in line)
    return line + " " * max(0, cols - width)


# SGR codes the frame dump may carry: `0` reset, `1` bold, `4` underline,
# `7` reverse (and the `2x` codes that turn each off). A dump that carries
# them is drawn run by run so a selection or a search match is visible in the
# PNG; a plain-text dump keeps the original single-pass path.
_SGR = re.compile(r"\x1b\[([0-9;]*)m")


def split_runs(line: str) -> list[tuple[str, frozenset[int]]]:
    """Split a dump line into `(text, active SGR codes)` runs."""
    runs: list[tuple[str, frozenset[int]]] = []
    style: set[int] = set()
    pos = 0
    for match in _SGR.finditer(line):
        if match.start() > pos:
            runs.append((line[pos : match.start()], frozenset(style)))
        for code in filter(None, match.group(1).split(";")):
            value = int(code)
            if value == 0:
                style = set()
            elif value in (1, 4, 7):
                style.add(value)
            elif value == 22:
                style.discard(1)
            elif value == 24:
                style.discard(4)
            elif value == 27:
                style.discard(7)
        pos = match.end()
    if pos < len(line):
        runs.append((line[pos:], frozenset(style)))
    return runs


def render(cols: int, rows: int, lines: list[str], caption: str) -> Image.Image:
    joined = "".join(_SGR.sub("", line) for line in lines)
    font = load_font(32, joined + caption)
    caption_font = load_font(13)
    adv = font.getlength("M")
    cell_w = int(round(adv))
    ascent, descent = font.getmetrics()
    cell_h = int(round(ascent + descent))
    caption_h = 26
    width = cols * cell_w + 2
    height = rows * cell_h + caption_h + 2
    img = Image.new("RGB", (width, height), BG)
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 0, width, caption_h], fill=CAPTION_BG)
    draw.text((8, 6), caption, font=caption_font, fill=FG)
    for y, line in enumerate(lines[:rows]):
        top = caption_h + 1 + y * cell_h
        if "\x1b[" not in line:
            draw.text((1, top), _pad_to_columns(line, cols), font=font, fill=FG)
            continue
        x = 1
        for text, style in split_runs(line):
            if not text:
                continue
            cells = sum(max(wcwidth(ch), 0) for ch in text)
            run_w = cells * cell_w
            fill = FG
            if 7 in style:
                draw.rectangle([x, top, x + run_w - 1, top + cell_h - 1], fill=REVERSE_BG)
                fill = REVERSE_FG
            draw.text((x, top), text, font=font, fill=fill)
            if 4 in style:
                underline_y = top + ascent + 2
                draw.rectangle([x, underline_y, x + run_w - 1, underline_y], fill=UNDERLINE_FG)
            x += run_w
    return img


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--text", required=True, help="frame dump file")
    parser.add_argument("--out", required=True, help="PNG to write")
    parser.add_argument("--caption", default="", help="caption bar text")
    args = parser.parse_args()

    cols, rows, lines = read_dump(args.text)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    image = render(cols, rows, lines, args.caption)
    image.save(args.out)
    print(f"{args.out}: {cols}x{rows} cells → {image.size[0]}x{image.size[1]} px")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

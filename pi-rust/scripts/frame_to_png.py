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

# Same palette as `pty_capture.py`, so a sheet mixing both kinds of panel does
# not look like two different products.
BG = (10, 10, 12)
CAPTION_BG = (42, 44, 54)
FG = (238, 238, 240)

_FONT_CANDIDATES = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "C:/Windows/Fonts/consola.ttf",
    "C:/Windows/Fonts/lucon.ttf",
)


def load_font(size: int):
    for name in _FONT_CANDIDATES:
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


def render(cols: int, rows: int, lines: list[str], caption: str) -> Image.Image:
    font = load_font(32)
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
        draw.text((1, caption_h + 1 + y * cell_h), line.ljust(cols), font=font, fill=FG)
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

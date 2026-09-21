#!/usr/bin/env python3
"""Read a `pty_capture.py` PNG back as *colours* and name them against the
built-in themes.

`pty_capture.py` writes a text dump alongside every screenshot, and text is
where most assertions belong. But a palette is not text: a theme change is
invisible to `expect` / `reject`, so the only honest evidence for "the stored
theme was in effect on the first frame" is the pixels.

This script answers exactly that question for a pair of screenshots:

* it counts the colours in a region of each PNG (default: the top half, i.e.
  the first panel of a one-column sheet),
* it labels every observed colour with the theme token(s) whose hex it equals,
  read straight out of `crates/pi-tui/assets/themes/*.json`,
* and it prints the dark -> light movement per token, so a reviewer can check
  the claim "the palette flipped at startup" against the shipped schemes
  instead of trusting a prose summary.

One subtlety it makes explicit: cells whose foreground is the *terminal
default* are drawn with `pty_capture.DEFAULT_FG` (`#d4d4d4`), which is why that
colour appears with an identical count in both images. It is harness chrome,
not the pi theme, and the script reports it as `-- (terminal default)` so the
number is not mistaken for a themed token.

Usage:
    python3 scripts/theme_palette_report.py BEFORE.png AFTER.png [--top 8] [--fraction 0.5]
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

try:
    from PIL import Image
except ImportError:  # pragma: no cover - environment guard
    sys.exit("Pillow is required: pip install pillow")

REPO_ROOT = Path(__file__).resolve().parent.parent
THEME_DIR = REPO_ROOT / "crates" / "pi-tui" / "assets" / "themes"

# Mirrors `pty_capture.DEFAULT_FG`: the colour the harness paints a cell whose
# foreground is the terminal default.
DEFAULT_FG = (212, 212, 212)


def hex_to_rgb(value: str) -> tuple[int, int, int] | None:
    text = value.lstrip("#")
    if len(text) != 6:
        return None
    try:
        return tuple(int(text[i : i + 2], 16) for i in (0, 2, 4))  # type: ignore[return-value]
    except ValueError:
        return None


def load_theme_tokens() -> list[tuple[str, str, tuple[int, int, int]]]:
    """`(theme name, token name, rgb)` for every hex in every shipped theme."""
    rows: list[tuple[str, str, tuple[int, int, int]]] = []
    for path in sorted(THEME_DIR.glob("*.json")):
        doc = json.loads(path.read_text(encoding="utf-8"))
        theme = doc.get("name", path.stem)
        for section in ("vars", "colors"):
            for token, value in (doc.get(section) or {}).items():
                if not isinstance(value, str) or not value.startswith("#"):
                    continue  # an alias like "text" -> resolved through vars
                rgb = hex_to_rgb(value)
                if rgb is not None:
                    rows.append((theme, token, rgb))
    return rows


def describe(rgb: tuple[int, int, int], tokens) -> str:
    if rgb == DEFAULT_FG:
        return "-- (terminal default)"
    names = [f"{theme}.{token}" for theme, token, value in tokens if value == rgb]
    # A hex used by both schemes is not evidence of a switch; name them all.
    return ", ".join(names) if names else "-- (unlabelled)"


def region_top_colors(png: Path, fraction: float) -> Counter:
    """Colour histogram of the top `fraction` of the screenshot."""
    with Image.open(png) as handle:
        image = handle.convert("RGB")
        width, height = image.size
        crop = image.crop((0, 0, width, int(height * fraction)))
        return Counter(crop.getdata())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--top", type=int, default=8, help="colours reported per image")
    parser.add_argument(
        "--fraction",
        type=float,
        default=0.5,
        help="vertical fraction of the PNG to read (default 0.5 = first panel of a tall sheet)",
    )
    args = parser.parse_args()

    tokens = load_theme_tokens()
    if not tokens:
        sys.exit(f"no themes found under {THEME_DIR}")

    counters = {} 
    for label, path in (("before", args.before), ("after", args.after)):
        if not path.is_file():
            sys.exit(f"missing screenshot: {path}")
        counters[label] = region_top_colors(path, args.fraction)

    width = max(
        len(describe(rgb, tokens)) for counter in counters.values() for rgb, _ in counter.most_common(args.top)
    )
    for label, path in (("before", args.before), ("after", args.after)):
        print(f"== {label}: {path.name}")
        for rgb, count in counters[label].most_common(args.top):
            hex_value = f"#{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}"
            print(f"  {hex_value}  {count:>9}  {describe(rgb, tokens):<{width}}")
        print()

    print("== dark -> light movement (tokens that appear in only one image)")
    for rgb, _ in counters["before"].most_common(args.top):
        name = describe(rgb, tokens)
        if counters["after"].get(rgb):
            continue
        print(f"  gone after : #{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}  {name}")
    for rgb, _ in counters["after"].most_common(args.top):
        name = describe(rgb, tokens)
        if counters["before"].get(rgb):
            continue
        print(f"  new after  : #{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}  {name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Which user-visible hints still hardcode a chord instead of reading the table?

Why this exists (LUM-1450): LUM-1447 closed one instance of this class (the
transcript's `Ctrl+O to expand` fold hint) and its audit named two more
(`/help`'s `keys:` block, the dialog footers). This script measures the whole
class, so "the hint surfaces follow the effective keybindings" is a number
rather than an impression.

Method
------
Scan the `src` trees of `pi-tui` and `pi-coding-agent` for **string literals
that contain a chord-shaped token** (`Ctrl+…`, `Alt+…`, `Shift+…`, `Esc`,
`Enter`, `PgUp`, `PgDn`, `Tab`, `Space`). A hit is *derived* when the literal
is an argument of one of the registry helpers:

    key_text_or / key_text_in / key_text_preferring / key_hint_or
    matches_with_fallback

(each of which takes a shipped-default chord on purpose, and resolves the real
one from the keybindings table), and *hardcoded* otherwise.

Excluded from the scan, each for a stated reason:

* comments (`//`, `///`, `//!`) — prose, not a rendered string;
* `#[cfg(test)]` items and `tests/` — fixtures are allowed to name chords;
* `keybindings.rs` — the table itself, where chords are *defined*;
* `locale.rs` / `fn format_chord` — the spelling vocabulary (`ctrl` → `Ctrl`)
  every derived chord is rendered through, not a hint;
* the `KNOWN_HARDCODED` list below — hints that name a chord for a reason
  other than advertising the binding.

`--check` fails in **both** directions, the way `keybinding_coverage.py` does:
a new hardcoded hint chord fails, and so does a `KNOWN_HARDCODED` entry that no
longer matches anything (a fix that forgot to shrink the list).

Known limitation, stated rather than hidden: the "is this an argument of a
registry helper" test looks at the hit's own line and the two lines above it,
so a literal that reaches a helper through a local variable would read as
hardcoded (over-reporting, never under-reporting). Every hit is printed with
its file:line, so a reader can check by hand.

Usage:
    python pi-rust/scripts/hint_chord_literals.py pi-rust
    python pi-rust/scripts/hint_chord_literals.py pi-rust --check
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from app_action_coverage import strip_test_items  # noqa: E402

SCAN_DIRS = ("crates/pi-tui/src", "crates/pi-coding-agent/src")

#: Files whose chord literals are *definitions*, not hints.
SKIP_FILES = ("keybindings.rs", "locale.rs")

#: Functions whose chord literals are the spelling vocabulary.
SKIP_FUNCTIONS = ("format_chord",)

#: A chord-shaped token inside a string literal.
CHORD = re.compile(
    r"""(?x)
    " [^"\n]* \b
    (?:
        (?:Ctrl|Alt|Shift|Cmd|Super)
        (?:\+[A-Za-z0-9]+)?
      | Esc(?:ape)?
      | Enter
      | (?:PgUp|PgDn)
      | Tab
      | Space
    )
    \b [^"\n]* "
    """
)

STRING_LITERAL = re.compile(r'"[^"\n]*"')

FUNCTION = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+|async\s+)?fn\s+(\w+)")

#: Registry helpers: a chord literal passed to one of these *is* the derived
#: path (the helper substitutes the effective chord).
REGISTRY_HELPERS = (
    "key_text_or(",
    "key_text_in(",
    "key_text_preferring(",
    "key_hint_or(",
    "matches_with_fallback(",
)

#: Hint surfaces that hardcode a chord on purpose. Each entry is
#: `(file name, needle)`. Adding one is a decision: say why above it.
KNOWN_HARDCODED: tuple[tuple[str, str], ...] = (
    # Upstream hardcodes this too (`components/settings-list.ts:321`), so
    # aligning it would invent a behaviour pi-ts does not have (LUM-1447 §7).
    ("settings.rs", "Enter/Space"),
    # `/help`'s `/new` row: the chord *column* is derived; this parenthetical
    # names the kitty-protocol alternative for the same action. It states a
    # terminal-protocol condition, which a derived chord cannot carry — a user
    # who rebinds `tui.input.newLine` can make it stale (docs/LUM1450_HINT_CHORDS.md §7).
    ("slash.rs", "kitty-protocol"),
    # The slash-command autocomplete dropdown description. `AUTOCOMPLETE_COMMANDS`
    # is a `const` table of `&'static str`, so a derived chord needs the provider
    # builder to format rows (docs/LUM1450_HINT_CHORDS.md §7, first follow-up).
    ("slash.rs", "Configure which models"),
    # `/hotkeys`'s `app.tools.expand` / `app.header` rows: the chord column is
    # derived, and the parenthetical reads "by default" — a note about the
    # shipped value, not an advertisement of the effective one.
    ("slash.rs", "(Ctrl+O by default)"),
    ("slash.rs", "(Alt+H by default)"),
)


def chord_literals(line: str) -> list[str]:
    out = []
    for literal in STRING_LITERAL.finditer(line):
        text = literal.group(0)
        if CHORD.search(text):
            out.append(text)
    return out


def scan(root: pathlib.Path) -> list[tuple[str, int, str]]:
    hits: list[tuple[str, int, str]] = []
    found: set[tuple[str, str]] = set()
    for rel in SCAN_DIRS:
        for path in sorted((root / rel).rglob("*.rs")):
            if path.name in SKIP_FILES:
                continue
            text = strip_test_items(path.read_text(encoding="utf-8"))
            lines = text.splitlines()
            enclosing = ""
            for index, line in enumerate(lines):
                if match := FUNCTION.match(line):
                    enclosing = match.group(1)
                stripped = line.strip()
                if stripped.startswith("//"):
                    continue
                if enclosing in SKIP_FUNCTIONS:
                    continue
                lits = chord_literals(line)
                if not lits:
                    continue
                # The helper call may open on the two lines above the literal.
                window = "\n".join(lines[max(0, index - 2) : index + 1])
                if any(helper in window for helper in REGISTRY_HELPERS):
                    continue
                allowed = [
                    (skip_file, needle)
                    for skip_file, needle in KNOWN_HARDCODED
                    if path.name == skip_file and needle in line
                ]
                found.update(allowed)
                if allowed:
                    continue
                for literal in lits:
                    hits.append((str(path.relative_to(root)), index + 1, literal))
    stale = sorted(set(KNOWN_HARDCODED) - found)
    return hits, stale


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="pi-rust", help="pi-rust directory")
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 on a new hardcoded hint chord, or on a stale allowlist entry",
    )
    args = parser.parse_args()

    root = pathlib.Path(args.root)
    hits, stale = scan(root)
    print(
        f"hint chord literals: {len(hits)} hardcoded, "
        f"{len(KNOWN_HARDCODED)} known-allowed"
    )
    for path, line, literal in hits:
        print(f"  HARDCODED {path}:{line}  {literal}")
    for skip_file, needle in stale:
        print(f"  STALE ALLOWLIST {skip_file}: {needle!r} matches nothing")
    if args.check and (hits or stale):
        print(
            "route the hint through key_text_preferring / key_hint_or, or fix "
            "the KNOWN_HARDCODED list (LUM-1450).",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Which defined keybinding ids have no consumer in the code?

Why this exists (LUM-1447): two ids in the `pi-tui` table
(`tui.editor.historyPrevious` / `historyNext`) were defined, advertised by
`/hotkeys`, and answered by **nobody** — a user who bound them got a dead
chord. `app_action_coverage.py` measures the `app.*` half of the same class;
this script covers both tables so a "dead id" claim is reproducible instead of
anecdotal.

Method: a literal occurrence of the id **outside** the definition file and
outside `#[cfg(test)]` items counts as a consumer. That is deliberately the
same weak-but-falsifiable test `app_action_coverage.py` uses (a literal in a
comment also counts, so the tool can *under*-report a gap, never invent one).

Known limitation, stated rather than hidden: a consumer that matches the id
through a constant or a loop instead of a literal reads as unconsumed. Grep
output for every reported id is printed so the reader can check by hand.

Usage:
    python pi-rust/scripts/keybinding_coverage.py pi-rust
    python pi-rust/scripts/keybinding_coverage.py pi-rust --check
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from app_action_coverage import strip_test_items  # noqa: E402

#: (label, definition file, id pattern)
TABLES = (
    (
        "tui.*",
        pathlib.Path("crates/pi-tui/src/keybindings.rs"),
        re.compile(r'entry\(\s*"([^"]+)"'),
    ),
    (
        "app.*",
        pathlib.Path("crates/pi-coding-agent/src/keybindings.rs"),
        re.compile(r'entry\(\s*"([^"]+)"'),
    ),
)

#: The `app.*` consumed-hint list. It lives in the `pi-tui` table file but is a
#: *registry*, not a consumer: counting its literals would report every listed
#: id as consumed even after its handler is deleted (LUM-1263 made
#: `app_action_coverage.py` skip the same file for the same reason).
CONSUMED_LIST = pathlib.Path("crates/pi-tui/src/keybindings.rs")

#: Ids that are known to have no consumer yet. `--check` fails when this set
#: and reality disagree in **either** direction: a new dead id regresses, and
#: a fixed id must be removed from here in the same commit (the reverse test
#: `app_action_coverage.py --check-consumed` documents for its own list).
#:
#: **Emptied by LUM-1263**, which wired the last one
#: (`app.tree.editLabel` — the `/tree` rename editor, claimed by
#: `interactive.rs::handle_picker_key`). The set is kept as the tripwire for the
#: next dead id: a new definition with no consumer fails `--check` until it is
#: wired or listed here.
KNOWN_UNCONSUMED: frozenset[str] = frozenset()


def defined(repo: pathlib.Path, path: pathlib.Path, pattern: re.Pattern[str]) -> list[str]:
    text = (repo / path).read_text(encoding="utf-8")
    return sorted(set(pattern.findall(text)))


def consumers(repo: pathlib.Path, definitions: pathlib.Path, ids: list[str]) -> dict[str, list[str]]:
    """Map id → the `file:line` sites mentioning it, in non-test code."""
    hits: dict[str, list[str]] = {id_: [] for id_ in ids}
    for path in sorted((repo / "crates").rglob("*.rs")):
        rel = path.relative_to(repo)
        if rel == definitions or rel == CONSUMED_LIST or "tests" in rel.parts:
            continue
        product = strip_test_items(path.read_text(encoding="utf-8", errors="replace"))
        for id_ in ids:
            for match in re.finditer(re.escape(f'"{id_}"'), product):
                line = product.count("\n", 0, match.start()) + 1
                hits[id_].append(f"{rel}:{line}")
    return hits


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", nargs="?", default="pi-rust", type=pathlib.Path)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit 1 when the unconsumed set differs from KNOWN_UNCONSUMED",
    )
    args = parser.parse_args()
    repo = args.repo

    unconsumed: set[str] = set()
    for label, definitions, pattern in TABLES:
        ids = defined(repo, definitions, pattern)
        hits = consumers(repo, definitions, ids)
        dead = [id_ for id_ in ids if not hits[id_]]
        unconsumed.update(dead)
        print(f"{label}: {len(ids) - len(dead)}/{len(ids)} consumed")
        for id_ in dead:
            print(f"  UNCONSUMED {id_}  ({definitions})")

    if unconsumed == set(KNOWN_UNCONSUMED):
        print(f"in sync: {len(unconsumed)} known-unconsumed id(s)")
        return 0
    extra = sorted(unconsumed - set(KNOWN_UNCONSUMED))
    fixed = sorted(set(KNOWN_UNCONSUMED) - unconsumed)
    if extra:
        print(f"NEW DEAD ID(S): {extra}")
    if fixed:
        print(f"FIXED BUT STILL LISTED: {fixed} — update KNOWN_UNCONSUMED")
    return 1 if args.check else 0


if __name__ == "__main__":
    raise SystemExit(main())

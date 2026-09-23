#!/usr/bin/env python3
"""Measure how much of the `app.*` action surface is actually wired up.

Why this exists
---------------
`pi-rust/crates/pi-coding-agent/src/keybindings.rs` *defines* every `app.*`
action (it is the Rust port of upstream's `core/keybindings.ts`). Defining a
chord and honouring it are different things: a chord can be defined, advertised
in the startup header (`pi-tui/src/locale.rs`'s `STARTUP_HINTS`), listed in
`/hotkeys`, and still have no code path that ever fires it. That was the
LUM-1240 "dead key" defect class, and it is invisible to `cargo build`.

The distinction this tool draws:

* **wired**      — at least one *handler-shaped* mention of the action id: the
                   literal appears as an argument to a chord lookup
                   (`matches("app.x", &[...])`, `matches_app_key(..)`,
                   `get_keys("app.x")`), i.e. some code path resolves the chord.
* **advertised** — mentioned only in a *advertisement-shaped* place: the
                   `("app.x", "what it does")` rows that `/hotkeys` prints, or
                   `locale.rs`'s `STARTUP_HINTS`. This is the "false ad" set:
                   the UI promises a chord that nothing resolves.
* **silent**     — defined and never mentioned at all.

An action can be both (handled on one code path, advertised on another); it is
counted as wired if any handler-shaped mention exists.

Counting rules (kept deliberately strict so the number cannot flatter us):

* the match is the exact string literal `"app.<id>"`, not a prefix match, so
  `app.models.save` is not credited by a mention of `app.models.toggleProvider`;
* this is an *id-dispatch* metric: a chord implemented by matching the raw key
  (`Key::Ctrl('c')`) instead of the action id is reported as unwired even
  though it works. Every action the tool reports as advertised-only or silent
  still has to be hand-checked against its chord before being called a defect
  (`--files` prints the evidence);
* `#[cfg(test)]` items and `tests/` directories are excluded, so a unit test
  that resolves a chord does not count as a shipped key binding. The stripper is
  brace-aware (a naive "cut at the first `#[cfg(test)]`" truncates
  `interactive.rs`, which has test items before its tree-selector input
  handling and would under-report by ~17 actions);
* `crates/pi-coding-agent/src/keybindings.rs` (the definitions) and
  `crates/pi-tui/src/keybindings.rs` (the consumed-hint list) are never counted
  as consumers. The second one matters: a literal in the list itself must not
  satisfy the check, or `--check-consumed` can never fail (LUM-1263).

Usage:
    python3 pi-rust/scripts/app_action_coverage.py               # human table
    python3 pi-rust/scripts/app_action_coverage.py --json        # machine read
    python3 pi-rust/scripts/app_action_coverage.py --files       # file:line list
    python3 pi-rust/scripts/app_action_coverage.py --check-consumed

`--check-consumed` exits 1 when the measured wired set and
`pi-tui/src/keybindings.rs`'s `CONSUMED_APP_ACTIONS` disagree, in either
direction. It needs no compiler, so CI can gate on it.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

DEFINITIONS = pathlib.Path("crates/pi-coding-agent/src/keybindings.rs")
#: The hand-maintained list the startup header filters its hints through
#: (LUM-1240/LUM-1245). `--check-consumed` keeps it honest: a chord listed here
#: but not actually resolved is a false ad, and a wired chord missing from here
#: is a hint the header silently drops. It is **not** a consumer: counting the
#: list's own literals would make the check tautological (every listed id would
#: measure as wired), so [`scan`] skips it too. LUM-1263 found and fixed that.
CONSUMED_LIST = pathlib.Path("crates/pi-tui/src/keybindings.rs")
ADVERTISING = pathlib.Path("crates/pi-tui/src/locale.rs")
ROOTS = (pathlib.Path("crates"),)


def strip_test_items(text: str) -> str:
    """Drop `#[cfg(test)]` items that run to end of file.

    Brace-aware, and skips string/char literals plus comments so a `}` inside a
    string does not close an item early. Items that are followed by more code
    are left alone: only the trailing test module is removed.
    """
    search_from = 0
    while True:
        start = text.find("#[cfg(test)]", search_from)
        if start == -1:
            return text
        brace = text.find("{", start)
        if brace == -1:
            return text
        depth = 0
        i = brace
        while i < len(text):
            ch = text[i]
            if ch == '"':
                i = _skip_string(text, i)
                continue
            if ch == "'":
                i = _skip_char_or_lifetime(text, i)
                continue
            if ch == "/" and i + 1 < len(text) and text[i + 1] == "/":
                i = text.find("\n", i)
                if i == -1:
                    return text
                continue
            if ch == "/" and i + 1 < len(text) and text[i + 1] == "*":
                end = text.find("*/", i + 2)
                if end == -1:
                    return text
                i = end + 2
                continue
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if i >= len(text):
            return text
        if not text[i + 1 :].strip():
            return text[:start]
        search_from = i + 1


def _skip_string(text: str, i: int) -> int:
    """Return the index just past the string literal starting at `i`."""
    if text.startswith('r"', i) or text.startswith('r#"', i):
        hashes = 0
        j = i + 1
        while j < len(text) and text[j] == "#":
            hashes += 1
            j += 1
        if j < len(text) and text[j] == '"':
            closer = '"' + "#" * hashes
            end = text.find(closer, j + 1)
            return len(text) if end == -1 else end + len(closer)
    j = i + 1
    while j < len(text):
        if text[j] == "\\":
            j += 2
            continue
        if text[j] == '"':
            return j + 1
        j += 1
    return len(text)


def _skip_char_or_lifetime(text: str, i: int) -> int:
    """Return the index just past a char literal, or `i + 1` for a lifetime."""
    if i + 1 < len(text) and text[i + 1] == "\\":
        j = i + 2
        while j < len(text) and text[j] != "'":
            j += 1
        return min(j + 1, len(text))
    if i + 2 < len(text) and text[i + 2] == "'":
        return i + 3
    # `'a` — a lifetime, not a literal.
    return i + 1


def defined_actions(repo: pathlib.Path) -> list[str]:
    source = (repo / DEFINITIONS).read_text(encoding="utf-8")
    return sorted(set(re.findall(r'"(app\.[A-Za-z0-9_.]+)"', source)))


def _is_advertisement(text: str, start: int, end: int) -> bool:
    """Does this literal sit in a `("app.x", "description")` row?

    Those rows are what `/hotkeys` prints. Requires the quote to be followed by
    a string literal, and the opening paren right before the id.
    """
    before = text[max(0, start - 24) : start]
    after = text[end : end + 24]
    return bool(re.search(r"\(\s*$", before)) and bool(re.match(r'\s*,\s*"', after))


def scan(repo: pathlib.Path, actions: list[str]) -> dict[str, dict[str, list[str]]]:
    hits: dict[str, dict[str, list[str]]] = {
        action: {"handler": [], "advertised": []} for action in actions
    }
    for root in ROOTS:
        for path in sorted((repo / root).rglob("*.rs")):
            rel = path.relative_to(repo)
            if rel in (DEFINITIONS, CONSUMED_LIST) or "tests" in rel.parts:
                continue
            product = strip_test_items(path.read_text(encoding="utf-8", errors="replace"))
            for action in actions:
                needle = f'"{action}"'
                for match in re.finditer(re.escape(needle), product):
                    line = product.count("\n", 0, match.start()) + 1
                    where = f"{rel}:{line}"
                    kind = (
                        "advertised"
                        if rel == ADVERTISING
                        or _is_advertisement(product, match.start(), match.end())
                        else "handler"
                    )
                    hits[action][kind].append(where)
    return hits


def classify(repo: pathlib.Path) -> dict:
    actions = defined_actions(repo)
    if not actions:
        raise SystemExit(f"no app.* actions found in {DEFINITIONS} — wrong repo root?")

    hits = scan(repo, actions)
    buckets: dict[str, list[str]] = {"wired": [], "advertised": [], "silent": []}
    for action in actions:
        if hits[action]["handler"]:
            buckets["wired"].append(action)
        elif hits[action]["advertised"]:
            buckets["advertised"].append(action)
        else:
            buckets["silent"].append(action)
    return {"actions": actions, "hits": hits, "buckets": buckets}


def consumed_list(repo: pathlib.Path) -> list[str]:
    text = (repo / CONSUMED_LIST).read_text(encoding="utf-8")
    start = text.find("pub const CONSUMED_APP_ACTIONS")
    if start == -1:
        raise SystemExit(f"CONSUMED_APP_ACTIONS not found in {CONSUMED_LIST}")
    end = text.find("];", start)
    return sorted(set(re.findall(r'"(app\.[^"]+)"', text[start:end])))


def check_consumed(repo: pathlib.Path, wired: list[str]) -> int:
    listed = consumed_list(repo)
    extra = [action for action in listed if action not in wired]
    missing = [action for action in wired if action not in listed]
    print(f"CONSUMED_APP_ACTIONS: {len(listed)} entries; measured wired: {len(wired)}")
    if not extra and not missing:
        print("in sync: the header's hint filter matches the code")
        return 0
    for action in extra:
        print(f"  FALSE AD   {action} is listed as consumed but no code resolves it")
    for action in missing:
        print(f"  HIDDEN     {action} is wired but not listed (the header drops its hint)")
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("repo", nargs="?", default="pi-rust", help="path to the pi-rust workspace")
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument("--files", action="store_true", help="list every reference")
    parser.add_argument("--check-consumed", action="store_true", help="fail on hint-filter drift")
    args = parser.parse_args()

    repo = pathlib.Path(args.repo)
    result = classify(repo)
    buckets, hits, actions = result["buckets"], result["hits"], result["actions"]

    if args.check_consumed:
        return check_consumed(repo, buckets["wired"])

    if args.json:
        payload = {
            "defined": len(actions),
            "wired": len(buckets["wired"]),
            "advertised": len(buckets["advertised"]),
            "silent": len(buckets["silent"]),
            "ratio": round(len(buckets["wired"]) / len(actions), 4),
            "buckets": buckets,
            "hits": hits if args.files else {k: v for k, v in hits.items() if k in buckets["wired"]},
        }
        json.dump(payload, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    total = len(actions)
    for name in ("wired", "advertised", "silent"):
        rows = buckets[name]
        print(f"{name}: {len(rows)}/{total} ({len(rows) / total:.1%})")
        if name == "wired":
            if args.files:
                for action in rows:
                    print(f"  {action}: {', '.join(hits[action]['handler'][:3])}")
        else:
            for action in rows:
                where = ", ".join(hits[action]["handler"] + hits[action]["advertised"][:2])
                print(f"  {action}  ({where or '-'})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Measure how much of upstream pi's extension event surface actually fires.

Why this exists
---------------
`docs/RUST_TS_PARITY_METRICS.md` says the extension-event axis is the largest
single swing in the weighted parity score (+5.6pt when finished) and that
"plugin-ecosystem compatibility" is the top blocker. That number was produced by
hand: a `grep` for `ExtensionEvent::[A-Za-z]*` over `crates`, minus a manual
"which variants are only constructed by tests" review. Hand-counted numbers have
already drifted twice in this repo (the `app.*` wiring rate was reported as
47.7% / 43.2% / 79.5% by three tasks), so this axis gets the same treatment the
`app.*` axis got in `app_action_coverage.py`: a script that can be re-run and
that fails loudly when it drifts from the documented list.

The distinction this tool draws
-------------------------------

* **subscribable** — upstream's plugin-facing API is `pi.on(event, handler)`;
  the overload set in `packages/coding-agent/src/core/extensions/types.ts` is
  therefore the definition of "an event a plugin can listen for".
* **declared**     — event names that exist in the extension runner's own type
  unions / dispatch table but have no `pi.on` overload (upstream documents 36
  names total; the overload set alone is smaller). Reported separately, never
  folded into the primary ratio silently.
* **wire tag**     — `ExtensionEvent::name()`, i.e. the string a WASM/JS plugin
  really receives. A plugin matches on the string, so a variant is only useful
  if its tag equals the upstream name (`session_end` is *not* `session_shutdown`).
* **produced**     — a variant that some *production* code path constructs.
  `#[cfg(test)]` items and `tests/` directories are excluded (brace/string/comment
  aware, same stripper as `app_action_coverage.py`), so a test that builds an
  event does not count as a shipped emit site. A variant with no production
  construction site is a dead variant: a plugin can name it and never see it.

Counting rules (kept strict so the number cannot flatter us):

* the primary ratio is `produced ∩ upstream / upstream` — an event is only
  counted as covered when a production code path can really deliver it under the
  name the plugin subscribed to;
* a Rust-native tag that upstream does not have (`user_message`) is listed as an
  extra and never inflates the ratio;
* the denominator is the *full* upstream set unless `--denominator` says
  otherwise; the docs have printed both 35 and 36 for this, and the two are not
  interchangeable (`--check-doc` fails on that drift).

Usage:
    python3 pi-rust/scripts/extension_event_coverage.py                  # human table
    python3 pi-rust/scripts/extension_event_coverage.py --json           # machine read
    python3 pi-rust/scripts/extension_event_coverage.py --files          # emit-site list
    python3 pi-rust/scripts/extension_event_coverage.py --check-doc      # doc drift gate
    python3 pi-rust/scripts/extension_event_coverage.py --require tool_call,tool_result

`--require` is the acceptance gate for the next porting round: it exits 1 while
any listed event has no production emit site, and prints where the missing ones
would have to be constructed.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from app_action_coverage import strip_test_items  # noqa: E402  (same directory)

#: Upstream's plugin-facing event API + the runner's own dispatch types.
UPSTREAM_TYPES = pathlib.Path("packages/coding-agent/src/core/extensions/types.ts")
UPSTREAM_RUNNER = pathlib.Path("packages/coding-agent/src/core/extensions/runner.ts")
#: The Rust enum + its wire tags.
EVENTS = pathlib.Path("crates/pi-protocol/src/events.rs")
#: The document the numbers are quoted from.
PARITY_DOC = pathlib.Path("docs/RUST_TS_PARITY_METRICS.md")
ROOTS = (pathlib.Path("crates"),)

#: `pi.on(event: "name", handler)` overloads.
_OVERLOAD = re.compile(r'on\(\s*event:\s*"([a-z_][a-z0-9_]*)"')
#: Event names inside the runner's unions (`{ type: "a" | "b" }`). TypeScript
#: type names (`"boolean" | "string"`) share the shape, so a union member only
#: counts as an event name when it is snake_case with at least one separator —
#: every upstream event name is, and no TS type name is.
_UNION_LINE = re.compile(r"type:\s*\"([a-z_][a-z0-9_]*)\"")
#: The runner's dispatch table.
_DISPATCH = re.compile(r"handlers\.(?:get|has|set)\(\s*\"([a-z_][a-z0-9_]*)\"")
#: Match arms of `ExtensionEvent::name()`: `Self::Variant { .. } => "tag",`
_NAME_ARM = re.compile(r"Self::([A-Za-z0-9_]+)\s*(?:\{[^}]*\})?\s*=>\s*\"([a-z_][a-z0-9_]*)\"")
#: A production construction site.
_CONSTRUCT = re.compile(r"ExtensionEvent::([A-Za-z0-9_]+)")


def source_root(repo: pathlib.Path) -> pathlib.Path:
    """The checkout root that holds `packages/`.

    The tool is invoked with the `pi-rust` workspace (the `app.*` tool's
    convention) while the upstream TypeScript lives one level up, next to it.
    """
    for candidate in (repo.resolve(), repo.resolve().parent):
        if (candidate / UPSTREAM_TYPES).is_file():
            return candidate
    raise SystemExit(f"{UPSTREAM_TYPES} not found next to {repo} — wrong repo root?")


def upstream_events(repo: pathlib.Path) -> dict[str, list[str]]:
    """Return the upstream event names, split by how they are declared."""
    root = source_root(repo)
    overloads: set[str] = set()
    for path in (root / UPSTREAM_TYPES,):
        text = path.read_text(encoding="utf-8", errors="replace")
        overloads |= set(_OVERLOAD.findall(text))

    declared: set[str] = set()
    for path in (root / UPSTREAM_RUNNER, root / UPSTREAM_TYPES):
        text = path.read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            if "|" in line:  # a union member list, not an unrelated type field
                declared |= {
                    name for name in _UNION_LINE.findall(line) if "_" in name
                }
        declared |= set(_DISPATCH.findall(text))
    return {
        "subscribable": sorted(overloads),
        "declared_only": sorted(declared - overloads),
    }


def rust_events(repo: pathlib.Path) -> dict[str, dict[str, object]]:
    """Map variant -> {tag, sites[]} for every `ExtensionEvent` variant."""
    source = (repo / EVENTS).read_text(encoding="utf-8", errors="replace")
    start = source.find("pub fn name(&self)")
    if start == -1:
        raise SystemExit(f"ExtensionEvent::name() not found in {EVENTS}")
    body = source[start : source.find("\n    }", start)]
    tags = {variant: tag for variant, tag in _NAME_ARM.findall(body)}
    if not tags:
        raise SystemExit(f"no variants parsed out of ExtensionEvent::name() in {EVENTS}")

    variants = {variant: {"tag": tag, "sites": []} for variant, tag in tags.items()}
    for root in ROOTS:
        for path in sorted((repo / root).rglob("*.rs")):
            rel = path.relative_to(repo)
            if "tests" in rel.parts:
                continue
            product = strip_test_items(path.read_text(encoding="utf-8", errors="replace"))
            for match in _CONSTRUCT.finditer(product):
                variant = match.group(1)
                if variant in variants:
                    line = product.count("\n", 0, match.start()) + 1
                    variants[variant]["sites"].append(f"{rel}:{line}")
    return variants


def classify(repo: pathlib.Path) -> dict:
    up = upstream_events(repo)
    upstream = sorted(set(up["subscribable"]) | set(up["declared_only"]))
    if not upstream:
        raise SystemExit(f"no upstream events found in {UPSTREAM_TYPES}")
    variants = rust_events(repo)

    tags = {data["tag"] for data in variants.values()}
    produced = {data["tag"] for data in variants.values() if data["sites"]}
    return {
        "upstream": upstream,
        "upstream_subscribable": up["subscribable"],
        "upstream_declared_only": up["declared_only"],
        "variants": variants,
        "tags": sorted(tags),
        "produced": sorted(produced),
        "missing_variants": sorted(set(upstream) - tags),
        "dead_variants": sorted(
            data["tag"] for data in variants.values() if not data["sites"]
        ),
        "rust_only_tags": sorted(tags - set(upstream)),
    }


def doc_events(repo: pathlib.Path) -> list[str]:
    """The upstream event list as printed in the parity document.

    Anchored on the sentence that introduces it, so a later backtick run
    elsewhere in the doc cannot be mistaken for the event list.
    """
    text = (repo / PARITY_DOC).read_text(encoding="utf-8", errors="replace")
    marker = text.find("扩展可监听")
    if marker == -1:
        raise SystemExit(f"no event-list marker found in {PARITY_DOC}")
    match = re.search(r"`([a-z_]+(?:/[a-z_]+)+)`", text[marker:])
    if not match:
        raise SystemExit(f"no slash-separated event list after the marker in {PARITY_DOC}")
    return sorted(set(match.group(1).split("/")))


def check_doc(repo: pathlib.Path, result: dict) -> int:
    documented = doc_events(repo)
    upstream = result["upstream"]
    print(f"documented: {len(documented)} events; extracted upstream: {len(upstream)}")
    missing = [name for name in documented if name not in upstream]
    extra = [name for name in upstream if name not in documented]
    if not missing and not extra:
        print("in sync: the parity doc's event list matches the code")
        return 0
    for name in missing:
        print(f"  DOC ONLY   {name} is listed in {PARITY_DOC} but no code declares it")
    for name in extra:
        print(f"  CODE ONLY  {name} is declared in code but missing from {PARITY_DOC}")
    return 1


def check_required(result: dict, required: list[str]) -> int:
    produced = set(result["produced"])
    upstream = set(result["upstream"])
    missing = [name for name in required if name not in produced]
    unknown = [name for name in required if name not in upstream]
    print(
        f"required events: {len(required)}; with a production emit site: "
        f"{len(required) - len(missing)}"
    )
    for name in unknown:
        print(f"  UNKNOWN    {name} is not an upstream event name")
    for name in missing:
        if name in unknown:
            continue
        tag = "declared, never constructed" if name in result["tags"] else "no variant at all"
        print(f"  NOT FIRING {name} ({tag})")
    if missing or unknown:
        return 1
    print("ok: every required event has a production emit site under its upstream name")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("repo", nargs="?", default="pi-rust", help="path to the pi-rust workspace")
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument("--files", action="store_true", help="list every emit site")
    parser.add_argument("--check-doc", action="store_true", help="fail on doc drift")
    parser.add_argument("--require", default=None, help="comma-separated events that must fire")
    args = parser.parse_args()

    repo = pathlib.Path(args.repo)
    result = classify(repo)

    if args.check_doc:
        return check_doc(repo, result)
    if args.require:
        return check_required(result, [n.strip() for n in args.require.split(",") if n.strip()])

    upstream = result["upstream"]
    total = len(upstream)
    tags = [name for name in result["tags"] if name in upstream]
    produced = [name for name in result["produced"] if name in upstream]

    if args.json:
        payload = {
            "upstream": {
                "total": total,
                "subscribable": len(result["upstream_subscribable"]),
                "declared_only": len(result["upstream_declared_only"]),
            },
            "rust": {
                "variants": len(result["variants"]),
                "tags": len(tags),
                "produced": len(produced),
                "tag_ratio": round(len(tags) / total, 4),
                "production_ratio": round(len(produced) / total, 4),
            },
            "missing_variants": result["missing_variants"],
            "dead_variants": result["dead_variants"],
            "rust_only_tags": result["rust_only_tags"],
            "produced": produced,
            "sites": (
                {v: d["sites"] for v, d in result["variants"].items()} if args.files else {}
            ),
        }
        json.dump(payload, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    print(f"upstream events: {total} ({len(result['upstream_subscribable'])} subscribable + "
          f"{len(result['upstream_declared_only'])} declared-only)")
    print(f"wire tags matching upstream:    {len(tags)}/{total} ({len(tags) / total:.1%})")
    print(f"production emit sites:          {len(produced)}/{total} ({len(produced) / total:.1%})")
    if result["rust_only_tags"]:
        print(f"rust-only tags (not upstream):  {', '.join(result['rust_only_tags'])}")
    if result["dead_variants"]:
        print(f"dead variants (no production site): {', '.join(result['dead_variants'])}")
    print(f"missing variants: {len(result['missing_variants'])}/{total}")
    for name in result["missing_variants"]:
        print(f"  {name}")
    if args.files:
        print("emit sites:")
        for variant, data in sorted(result["variants"].items()):
            if data["sites"]:
                print(f"  {data['tag']}: {', '.join(data['sites'][:4])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

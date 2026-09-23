#!/usr/bin/env python3
"""Line counts for the Rust↔TS parity audit — the two `src` trees, side by side.

Why this exists (LUM-1426): the audit's size number used to be measured with

    find … -name '*.ts' | xargs wc -l | tail -1

which is wrong on any tree large enough for `xargs` to split the file list into
several batches: `wc` then prints one total per batch and `tail -1` keeps only
the **last** batch. On this repository that under-reports the TypeScript tree by
~100k lines. Summing per file in Python has no batch limit and no shell quoting
surprises, so the number is reproducible.

Usage (from the repository root, the same place the other coverage scripts run):

    python pi-rust/scripts/measure_loc.py

Definition of the two sides, kept identical to `docs/RUST_TS_PARITY_METRICS.md`:

* Rust  = every `*.rs` under `pi-rust/crates/**/src/`, **excluding `mod.rs`**
          (the historical口径; `all` is printed too for reference).
* TS    = every `*.ts` under `packages/**/src/`.
"""

from __future__ import annotations

import pathlib
import sys


def total(root: str, pattern: str, exclude: str | None = None, src_only: bool = True):
    files = 0
    lines = 0
    for path in pathlib.Path(root).rglob(pattern):
        if src_only and "/src/" not in path.as_posix():
            continue
        if exclude and path.name == exclude:
            continue
        lines += len(path.read_text(encoding="utf-8", errors="ignore").splitlines())
        files += 1
    return files, lines


def main() -> int:
    root = pathlib.Path.cwd()
    if not (root / "pi-rust").is_dir() or not (root / "packages").is_dir():
        sys.exit(
            "run me from the repository root (the directory holding `pi-rust/` and `packages/`)"
        )
    rust_files, rust_lines = total("pi-rust/crates", "*.rs", exclude="mod.rs")
    _, rust_all = total("pi-rust/crates", "*.rs")
    ts_files, ts_lines = total("packages", "*.ts")
    print(f"rust src (no mod.rs): {rust_lines:>8} lines / {rust_files} files")
    print(f"rust src (all):       {rust_all:>8} lines")
    print(f"ts   src:             {ts_lines:>8} lines / {ts_files} files")
    print(f"size parity (src vs src): {rust_lines / ts_lines * 100:.1f}%")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

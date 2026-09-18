#!/usr/bin/env bash
# CI entry point for the Pi Rust port.
# Usage:  ./scripts/ci.sh
#
# Exits non-zero on any failure. Stage 0 has no tests yet so this is a
# best-effort check that the workspace resolves.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not on PATH; skipping (install rustup first)" >&2
  exit 0
fi

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test"
cargo test --workspace

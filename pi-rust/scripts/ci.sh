#!/usr/bin/env bash
# CI entry point for the Pi Rust port.
# Usage:  ./scripts/ci.sh
#
# Exits non-zero on any failure. Stage 0 has no tests yet so this is a
# best-effort check that the workspace resolves.

set -euo pipefail

cd "$(dirname "$0")/.."

# LUM-1308: resolve a *working* toolchain before deciding anything. On a machine
# whose `cargo` is a rustup shim without a default toolchain, this script used to
# report `fmt --check` / `clippy` failures that had nothing to do with the code
# (and a different toolchain gives a different answer for `needless_lifetimes`).
# toolchain.sh never touches the network. Set PI_RUST_TOOLCHAIN / PI_RUST_BIN to
# override, or SKIP_TOOLCHAIN_HINT=1 to keep whatever is on PATH (e.g. inside
# rustup's own CI image).
if [ "${SKIP_TOOLCHAIN_HINT:-0}" != "1" ]; then
  # shellcheck source=scripts/toolchain.sh
  . "$(dirname "$0")/toolchain.sh"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not on PATH; skipping (install rustup first)" >&2
  exit 0
fi

echo "==> toolchain: $(rustc --version) / $(cargo --version)"

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> cargo test"
cargo test --workspace --locked

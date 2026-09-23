#!/usr/bin/env bash
# Resolve a *usable* Rust toolchain for this workspace's gates.
#
# Why this exists (LUM-1308): `cargo fmt --check`, `cargo clippy` and
# `cargo test` reported failures on this machine that had nothing to do with
# the code. `~/.local/bin/cargo` is a rustup shim whose default toolchain is
# not configured, so every gate died with
#
#     error: rustup could not choose a version of rustc to run, because one
#     wasn't specified explicitly, and no default is configured.
#
# and the previous round had to report an unpinned red gate (LUM-1306 §"gates",
# LUM-1307 §1). Worse, running `cargo +1.85.0` makes rustup try to *sync* the
# channel over the network first, which can hang for minutes in a sandbox.
#
# So: locate an installed toolchain directory directly and put its `bin` first
# on `PATH`. That never touches the network and never needs a rustup default.
#
# Usage (from bash):   . scripts/toolchain.sh
# Usage (from sh):     . scripts/toolchain.sh   # requires bash; use bash
#
# Overrides:
#   PI_RUST_TOOLCHAIN=1.88.0  the version to look for (default below)
#   PI_RUST_BIN=/path/to/bin  skip discovery and use this directory

# shellcheck disable=SC2034  # read by the caller and by `ci.sh`
PI_RUST_TOOLCHAIN_VERSION="${PI_RUST_TOOLCHAIN:-1.85.0}"

pi_toolchain_bin_dir=""

pi_find_toolchain() {
  if [ -n "${PI_RUST_BIN:-}" ]; then
    pi_toolchain_bin_dir="$PI_RUST_BIN"
    return 0
  fi
  # `RUSTUP_HOME` first (a container may keep toolchains outside $HOME), then
  # the default location. The glob matches the installed directory name, which
  # has the host triple appended (`1.85.0-x86_64-unknown-linux-gnu`).
  for root in "${RUSTUP_HOME:-}" "$HOME/.rustup" /usr/local/rustup; do
    [ -n "$root" ] || continue
    [ -d "$root/toolchains" ] || continue
    for candidate in "$root"/toolchains/"$PI_RUST_TOOLCHAIN_VERSION"-*/bin; do
      [ -x "$candidate/cargo" ] || continue
      [ -x "$candidate/rustc" ] || continue
      pi_toolchain_bin_dir="$candidate"
      return 0
    done
  done
  return 1
}

if ! pi_find_toolchain; then
  # Last resort: whatever `PATH` already resolves, but only if it actually
  # works — the broken shim above answers `command -v cargo` too.
  if rustc -vV >/dev/null 2>&1; then
    echo "warning: no ${PI_RUST_TOOLCHAIN_VERSION} toolchain did not resolve; using the ambient $(rustc --version)" >&2
    return 0 2>/dev/null || exit 0
  fi
  echo "error: no usable Rust toolchain found." >&2
  echo "  looked for: <RUSTUP_HOME|\$HOME/.rustup>/toolchains/${PI_RUST_TOOLCHAIN_VERSION}-*/bin" >&2
  echo "  ambient rustc: $(rustc --version 2>&1 | head -1)" >&2
  echo "  set PI_RUST_BIN=<toolchain>/bin to point at an installed one." >&2
  return 1 2>/dev/null || exit 1
fi

PATH="$pi_toolchain_bin_dir:$PATH"
export PATH

if ! rustc -vV >/dev/null 2>&1; then
  echo "error: ${pi_toolchain_bin_dir}/rustc is not runnable" >&2
  return 1 2>/dev/null || exit 1
fi

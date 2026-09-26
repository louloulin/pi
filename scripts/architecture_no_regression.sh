#!/usr/bin/env bash
# Architecture no-regression guard for M1 refactor.
#
# Asserts:
#   1. interactive.rs pub API surface unchanged (9 items)
#   2. coding_agent lib.rs pub API surface unchanged (48 items)
#   3. interactive.rs + app/mod.rs LOC count non-increasing vs. M1-start
#      (captured baselines: interactive 9457, app/mod.rs 7492)
#
# This guard is intentionally a shell script + grep, not a Rust
# integration test, because pi-ai's broken baseline blocks all
# downstream crates from compiling in this branch. Re-enable as a
# `cargo test` integration test once pi-ai is restored.
#
# Usage:
#   ./scripts/architecture_no_regression.sh
#
# Exit codes:
#   0 — pass
#   1 — public API drift
#   2 — file grew beyond M1 ceiling
#   3 — baseline file missing

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PI_RUST="$REPO_ROOT/pi-rust"
INTERACTIVE="$PI_RUST/crates/pi-coding-agent/src/interactive/mod.rs"
APP_MOD="$PI_RUST/crates/pi-tui/src/app/mod.rs"
LIB_RS="$PI_RUST/crates/pi-coding-agent/src/lib.rs"
BASELINE_DIR="$PI_RUST/tests"

INTERACTIVE_BASELINE="$BASELINE_DIR/interactive_pub_api_baseline.txt"
LIB_BASELINE="$BASELINE_DIR/coding_agent_pub_api_baseline.txt"

# M1-start LOC ceilings (taken 2026-09-26).
INTERACTIVE_CEILING=10000   # +5% headroom over 9457 to allow micro-patches
APP_MOD_CEILING=7900        # +5% headroom over 7492

failed=0

# --- Guard 1: interactive.rs pub API surface unchanged -------------------
if [[ ! -f "$INTERACTIVE_BASELINE" ]]; then
    echo "FAIL: baseline $INTERACTIVE_BASELINE missing" >&2
    exit 3
fi

current=$(grep -nE "^pub (fn|struct|enum|use|mod|const|static)" "$INTERACTIVE" | sort || true)
baseline=$(cat "$INTERACTIVE_BASELINE")

if [[ "$current" != "$baseline" ]]; then
    echo "FAIL: interactive.rs pub API drift" >&2
    echo "--- baseline ---" >&2
    echo "$baseline" >&2
    echo "--- current ---" >&2
    echo "$current" >&2
    echo "--- diff ---" >&2
    diff <(echo "$baseline") <(echo "$current") >&2 || true
    failed=1
fi

# --- Guard 2: coding_agent lib.rs pub API surface unchanged --------------
if [[ ! -f "$LIB_BASELINE" ]]; then
    echo "FAIL: baseline $LIB_BASELINE missing" >&2
    exit 3
fi

current_lib=$(grep -nE "^pub (fn|struct|enum|use|mod|const|static)" "$LIB_RS" | sort || true)
baseline_lib=$(cat "$LIB_BASELINE")

if [[ "$current_lib" != "$baseline_lib" ]]; then
    echo "FAIL: coding_agent lib.rs pub API drift" >&2
    diff <(echo "$baseline_lib") <(echo "$current_lib") >&2 || true
    failed=1
fi

# --- Guard 3: LOC non-increasing -----------------------------------------
interactive_loc=$(wc -l < "$INTERACTIVE" | tr -d ' ')
app_mod_loc=$(wc -l < "$APP_MOD" | tr -d ' ')

if (( interactive_loc > INTERACTIVE_CEILING )); then
    echo "FAIL: interactive.rs grew to $interactive_loc (ceiling $INTERACTIVE_CEILING)" >&2
    failed=2
fi

if (( app_mod_loc > APP_MOD_CEILING )); then
    echo "FAIL: app/mod.rs grew to $app_mod_loc (ceiling $APP_MOD_CEILING)" >&2
    failed=2
fi

if [[ $failed -eq 0 ]]; then
    echo "OK: architecture no-regression"
    echo "  interactive/mod.rs: $interactive_loc lines (ceiling $INTERACTIVE_CEILING)"
    echo "  app/mod.rs:         $app_mod_loc lines (ceiling $APP_MOD_CEILING)"
    exit 0
fi

exit $failed
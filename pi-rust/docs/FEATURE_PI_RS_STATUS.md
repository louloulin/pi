# `feature/pi.rs` Branch — Consolidated Status

This branch (created from LUM-1016) consolidates the in-review Rust port work
from the LUM-981 follow-up tree into a single commit chain on top of `origin/main`.

## Branch composition

Six merge commits on top of `origin/main` (`71dca871b`):

| Order | Source | Title |
|-------|--------|-------|
| 1 | `33adc5b83` | Stage 0 — scaffold pi-rust workspace |
| 2 | `9e61b0fa3` | LUM-995 — wire `should_stop_after_turn` / `prepare_next_turn` hooks |
| 3 | `3fac21a2b` | Stage 4 — pi-tui interactive CLI + `session_log` (LUM-988) |
| 4 | `9d7e49582` | Stage 6 — `wasm32-unknown-unknown` + `wasm-bindgen` browser host (LUM-990) |
| 5 | `7517da24a` | LUM-997 — bash / write / edit / read tools for `pi-coding-agent` |
| 6 | `523af6782` (cherry-pick) | LUM-996 — OpenAI Chat Completions provider (provider module + fixtures + example) |

The diff against `origin/main` is **83 files / +14 594 lines**, all under
`pi-rust/`.

## What works after the merge

```
$ cargo check  --workspace --all-targets    # clean
$ cargo build  --workspace --all-targets    # clean
$ cargo test   --workspace                  # 87 tests pass
```

## What was *deliberately* skipped

Three follow-up task branches are NOT merged into `feature/pi.rs` because each
implements the same surface area in a way that conflicts with the chain above:

- **`agent/devbox1/a5e8bd115db9` (LUM-984, Stage 1 pi-ai)** — defines a more
  elaborate `AssistantMessageEvent` (TextStart/TextEnd/ThinkingStart/ThinkingEnd
  /ToolCallStart/ToolCallEnd, plus response_id / response_model / raw_stop_reason /
  error_message / timestamp on `AssistantMessage`) and an `OpenAiTransport`
  trait abstraction. Conflicts with `pi-protocol/src/events.rs` and
  `pi-ai/src/providers/openai.rs` from LUM-995 / Stage 4 / Stage 6. The Stage 1
  providers, fixtures, and SSE parser live on the branch for future reference;
  merging requires either collapsing two protocol designs or rewriting the
  TUI / WASM consumers, which exceeds the LUM-1016 scope.
- **`agent/devbox1/e331c7847` (LUM-985, Stage 2 pi-agent-core)** — introduces
  its own `AgentLoop` event loop with `AgentEvent` defined in `agent_loop::*`
  and uses a `tool.rs` module that is incompatible with Stage 4's `events.rs`
  separation. The Stage 2 test suite (`tests/agent_loop.rs`, 5 e2e scenarios)
  is preserved on the branch.
- **`agent/devbox1/b662db4686e7` (LUM-996, full merge)** — full commit merges
  on top of the alternate Stage 0 base (`0ab09d1cd`) and changes every crate's
  Cargo.toml + lib.rs in ways that don't reconcile with the `feature/pi.rs`
  chain. The OpenAI Chat Completions provider, fixtures, and example were
  cherry-picked individually instead.

## Why this ordering instead of the LUM-981 follow-up chain

The original LUM-981 plan (and PLAN.md / PLAN-lum-981.md) sequenced Stages 0 →
1 → 2 → 3 → 4 → 5 → 6. Stage 1 and Stage 2 each delivered agent_core / pi-ai
implementations that did not anticipate the events.rs / module reorganization
Stage 4 introduced. The worktrees for Stage 4 / Stage 6 were built directly on
Stage 0 + LUM-995 and never depended on Stage 1 / Stage 2 — the Stage 4 branch
ships its own minimal `AgentLoop` plus `events::AgentEvent`.

The pragmatic sequence is the one that the actual git history of completed
tasks supports: take what compiles and what each branch already validated in
isolation, drop what conflicts at the protocol/event level, and surface the
deferred work as a follow-up.

## Recommended follow-up

If a future agent wants Stage 1 / Stage 2 work folded into `feature/pi.rs`:

1. Pick the protocol — either keep Stage 4/6's `AssistantMessageEvent` enum
   (Start / TextDelta / ThinkingDelta / ToolCallDelta / Done / Aborted / Error)
   and expand it with the Start/End variants Stage 1 needs, OR drop Stage 4's
   `events` module in favor of Stage 2's `agent_loop` placement.
2. Reconcile `pi-ai` provider designs — Stage 1's `OpenAiTransport` trait +
   `FixtureTransport` test mode is more testable; LUM-996's `reqwest` direct
   adapter is simpler. A future PR could port LUM-996's
   `OpenAiProvider::stream_simple` body onto Stage 1's `OpenAiTransport`.
3. Port Stage 2's e2e test suite (`tests/agent_loop.rs`) onto whichever
   agent_core lands in `feature/pi.rs` — the scenarios are valuable regression
   coverage independent of the implementation choice.

## Verification commands

```bash
# all on feature/pi.rs
cd pi-rust
cargo check  --workspace --all-targets     # 0 errors
cargo build  --workspace --all-targets     # 0 errors
cargo test   --workspace                   # 87 / 87 pass
```
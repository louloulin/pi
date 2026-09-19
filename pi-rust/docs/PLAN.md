# Pi Rust — staged delivery plan

This plan tracks the Rust port of Pi. It is staged so the highest-value pieces
(agent loop + multi-provider LLM + extension host) land first and unblock the
rest. Each stage has a clear exit criterion that can be verified independently.

## Goals

1. **Functional parity** with the TypeScript implementation for the core agent,
   the AI provider layer, the interactive CLI, and the TUI.
2. **Plugin ecosystem compatibility** — the existing pi extension model (TS
   modules loaded from `.pi/extensions/` and `~/.pi/agent/extensions/`) must
   keep working. The Rust host runs JS extensions in a QuickJS/wasm runtime
   that exposes the same `ExtensionAPI` surface as the TS host.
3. **Native + WASM targets** — the agent core compiles for `wasm32-unknown-unknown`
   so it can be embedded in browser/edge runtimes; the full binary targets native
   (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, …).

## Stages

### Stage 0 — Scaffold (this commit)
- Workspace manifest, crate skeletons, stubbed public APIs.
- Cross-crate architecture notes (`ARCHITECTURE.md`).
- CI scripts: `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
  `cargo test --workspace` (no-op until Stage 1).

**Exit criterion:** `cargo build --workspace` succeeds with stub crates.

### Stage 1 — `pi-ai` provider layer (parallel task A)
Scope: port `packages/ai/src/types.ts`, the `streamSimple` contract, the model
catalog loader, and the OpenAI + Anthropic provider implementations.

Concrete deliverables:
- `pi-ai::types`: `Message`, `ContentBlock`, `Tool`, `Model`, `Api`, `Context`,
  `AssistantMessageEvent`, `AssistantMessageEventStream`, `Usage`.
- `pi-ai::models`: catalog loader + typed model registry with serde round-trip.
- `pi-ai::providers::openai`: chat-completions + Responses streaming.
- `pi-ai::providers::anthropic`: messages streaming (SSE) + prompt caching.
- `pi-ai::stream`: unified `stream_simple` returning the event-stream enum.
- Unit tests against recorded provider fixtures (no live network in CI).

Exit criterion: a `pi-ai` example streams a recorded OpenAI fixture end-to-end
and produces an `AssistantMessage` matching the schema used by `pi-agent-core`.

### Stage 2 — `pi-agent-core` (parallel task B)
Scope: port the event loop from `packages/agent/src/agent-loop.ts`, including
tool execution modes, queue draining, message transformation, and event emission.

Concrete deliverables:
- `pi_agent_core::agent_loop::AgentLoop` mirroring the TS loop 1:1.
- `pi_agent_core::agent::Agent` facade matching the public surface of
  `packages/agent/src/agent.ts`.
- Sequential + parallel tool execution with the same `BeforeToolCall` /
  `AfterToolCall` hooks the TS version exposes to extensions.
- Event-stream enum that matches `packages/agent/src/types.ts`.
- Integration tests using the faux provider from the TS test harness.

Exit criterion: `pi-agent-core` can drive the faux provider through a 5-turn
session with a single tool call, emitting the same event sequence the TS harness
asserts against in `packages/agent/test/`.

### Stage 3 — `pi-extensions` WASM host (parallel task C)
Scope: a Rust host that loads pi extensions written against the existing
`ExtensionAPI` TypeScript shape and runs them inside a sandboxed WASM runtime.

Concrete deliverables:
- IDL for the extension host: `ExtensionAPI`, `ExtensionContext`, the events
  (`session_start`, `tool_call`, …), `registerTool`, `registerCommand`,
  `appendEntry`, `ui.notify`, `ui.confirm`, `ui.input`, `ui.select`,
  `ui.custom`.
- A JS shim (`extensions/runtime/pi-ext-shim.mjs`) that implements the host
  imports using a narrow WASM ABI, plus the QuickJS/wasmtime embedder.
- An example extension (the `notify-on-start` example from
  `packages/coding-agent/examples/extensions`) running unmodified under the host.
- A `pi-extensions::loader` that resolves extensions from
  `~/.pi/agent/extensions/`, `.pi/extensions/`, and `-e ./path.ts` (TS bundled
  to a temp module at load time, matching `coding-agent`'s resolution rules).

Exit criterion: an end-to-end smoke test loads an extension from disk,
delivers a `session_start` event, the extension calls `pi.registerTool`, the
agent invokes the registered tool, and the tool result is appended to the
session. The extension source is the same `.ts` file the TS host ships with.

### Stage 4 — `pi-tui` + interactive CLI
Scope: `packages/tui` primitives + the `coding-agent` interactive mode.

Exit criterion: `pi-coding-agent --interactive` renders the same prompt, the
editor component, and the message stream that the TS version does.

### Stage 5 — Session backend + persistence
Scope: port `packages/session-backends/sqlite-node` to `rusqlite`.

Exit criterion: a session recorded by the TS CLI round-trips through the
Rust session backend without diff.

### Stage 6 — WASM target for `pi-agent-core` + `pi-ai`
Scope: compile `pi-agent-core` + `pi-ai` for `wasm32-unknown-unknown` with the
`wasm-bindgen` ABI and ship a minimal JS host that drives an agent turn
end-to-end through the faux provider.

Concrete deliverables:
- Conditional `wasm` feature on `pi-ai` and `pi-agent-core` (no
  `mio` / `reqwest` on the wasm target — the workspace dep overrides
  strip `net`, `process`, `rt-multi-thread`, and `signal` from tokio).
- `pi_ai::wasm::register_faux_provider` — seeds the in-memory model
  catalog with a faux provider and a script of canned replies.
- `pi_agent_core::wasm::AgentHandle` — `wasm-bindgen` wrapper that
  exposes `new`, `prompt`, `subscribe`, `unsubscribe`; events fan out
  to JS callbacks via `serde-wasm-bindgen` (`u64` round-tripped as
  `BigInt`).
- `examples/wasm-host/` — minimal Vite + ESM JS host (`index.html`,
  `main.mjs`, `test.mjs`) that loads the wasm-pack-built package
  and runs four `node --test` smoke tests.
- `.github/workflows/rust-wasm.yml` — CI workflow that builds
  `wasm-pack build -p pi-agent-core --target web`, runs the JS
  smoke tests, asserts `pi_agent_core_bg.wasm` is under the 500 KB
  budget.
- Size budget: `pi_agent_core_bg.wasm` < 500 KB (currently ~130 KB
  after `wasm-opt -Oz`).

Exit criterion: a JS host page embeds the WASM build and drives an agent
loop with the faux provider; `wasm-pack build -p pi-agent-core --target web`
produces a `.wasm` < 500 KB.

## Concurrent execution

Stages 1–3 are independent and run as three parallel sub-tasks:

| Task | Crate | Reason it can run in parallel |
|---|---|---|
| A | `pi-ai` | No dependency on agent core or extension host; only depends on `pi-protocol` types. |
| B | `pi-agent-core` | Depends on `pi-ai` via the `streamSimple` trait. We define a trait stub up front so B can compile against a fake stream. |
| C | `pi-extensions` | Depends on `pi-protocol` only; can integrate with the real `pi-agent-core` once Stage 2 lands. |

## Verification matrix

| Concern | Tool |
|---|---|
| Formatting | `cargo fmt --check` |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` |
| Unit tests | `cargo test --workspace` |
| Provider conformance | Recorded SSE fixtures in `crates/pi-ai/fixtures/` |
| Extension conformance | The existing TS extension examples loaded via `pi-extensions` |
| WASM target | `cargo build -p pi-agent-core --target wasm32-unknown-unknown` |

## Delivered after Stage 6

### Stage 7 — `pi-ai` Anthropic Messages provider
`AnthropicProvider` streaming adapter (`POST {base}/v1/messages`), SSE
parsing for text / tool_use / thinking blocks, usage + cache-read/write
mapping, stop-reason mapping, Claude 4.5 model catalog, SSE fixtures and
an `anthropic_faux` agent-loop integration test. Merged in LUM-1048.

### Stage 8 — `pi-coding-agent` print mode
`print_mode.rs` (`text` / `json` / `json-events`) and `file_processor.rs`
(`@file` expansion, stdin pipe, 1 MiB cap), sysexits exit codes,
`SIGINT` / `SIGTERM` handling. The `print mode is a stub` placeholder is
gone. Merged in LUM-1048.

### Stage 9 — navigation tools
`find` / `grep` / `ls` alongside `bash` / `read` / `write` / `edit`,
with a shared ignore list (`mod_ignore.rs`) and sandbox checks. Merged in
LUM-1048.

## Planned — Stage 10–12

| Stage | Scope | Exit criterion |
|-------|-------|----------------|
| 10 | `--rpc` mode: stdio JSON-RPC over the `Agent` facade, mirroring `packages/server` | a JSON-RPC client can start a session, send a prompt and receive the streamed event vocabulary |
| 11 | Google Gemini provider (`generativelanguage` streaming) + catalog | fixture-driven tests equivalent to the Stage 7 Anthropic suite |
| 12 | print mode on `pi-session` (SQLite) instead of JSONL | `--continue` / `--session` read/write the same store as `/resume` |

Stages 10–12 touch disjoint files and are dispatched at most three at a
time; `feature/pi.rs` is the integration branch for each.

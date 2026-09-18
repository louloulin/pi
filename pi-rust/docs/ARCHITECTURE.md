# Pi Rust — architecture notes

The Rust port mirrors the TypeScript monorepo's package boundaries, but two
differences are intentional:

1. **`pi-extensions` is a new crate** — the TS port leans on Node's `vm` to run
   extension sources. The Rust port needs an explicit host crate so we can
   sandbox extensions in WASM and reuse the same load surface for both native
   and `wasm32` builds.

2. **`pi-protocol` exists in Rust from the start** — in the TS monorepo
   `packages/protocol` was carved out only after the wire types stabilised. In
   Rust we hoist those types up so the WASM boundary between `pi-extensions`
   and `pi-agent-core` has a single, serde-friendly schema instead of a
   sprawling bag of trait objects.

## Crate dependency graph

```
                     pi-coding-agent
                     /      |      \
                  pi-tui  pi-extensions
                            |
                            v
                  pi-agent-core  ──>  pi-ai
                       \              /
                        \            /
                         v          v
                          pi-protocol
```

The arrows are `Cargo.toml` dependencies. All crates depend on
`pi-protocol` for shared wire types.

## Cross-crate type flow

### `pi-protocol`

- `Message`, `ContentBlock`, `Tool`, `ToolResult`
- `Context` (system prompt + message log + tools)
- `AssistantMessage` + `AssistantMessageEvent` enum
- `SessionEntry` (the session-format rows from
  `packages/coding-agent/docs/session-format.md`)
- `ExtensionEvent` enum (`session_start`, `tool_call`, `tool_result`,
  `agent_end`, `message_start`, `message_update`, `message_end`, …)
- `ExtensionCommand` and `ToolDefinition` descriptors
- `UiRequest` / `UiResponse` (for `ctx.ui.notify`, `ctx.ui.confirm`, etc.)

All types derive `Serialize` / `Deserialize`. They are the source of truth
shared between native Rust code, the JS extension shim, and the WASM ABI.

### `pi-ai`

Exposes:

```rust
#[async_trait]
pub trait StreamFn: Send + Sync {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream>;
}
```

`Models::stream_simple` is the default implementation. The agent loop holds an
`Arc<dyn StreamFn>` so tests can swap in the faux provider without touching
the agent code.

### `pi-agent-core`

Owns the event loop, tool execution, and queue draining. Mirrors the TS
structure 1:1:

```
AgentLoop
  ├─ turn driver (per assistant turn)
  │    ├─ pre-tool hooks (BeforeToolCall)
  │    ├─ tool execution (sequential | parallel)
  │    └─ post-tool hooks (AfterToolCall)
  ├─ queue draining (one-at-a-time | all)
  └─ event emitter (typed enum over an mpsc channel)
```

### `pi-extensions`

Loads extensions from disk (matching the TS resolution rules), bundles `.ts`
sources if needed, then runs them inside `wasmtime` with a JS shim that
implements the host-import ABI. The shim translates `ExtensionEvent`s to
JS calls and vice versa.

### `pi-coding-agent`

The interactive binary. Wires `pi-agent-core` to `pi-extensions`, the TUI,
and the SQLite session backend.

## Why a workspace at all

The TS monorepo uses npm workspaces; the Rust port uses Cargo workspaces for
the same reason — fast incremental compilation across packages, a single
`cargo test` for everything, and one set of pinned dependency versions.

## Why a separate protocol crate

The TS port has `packages/protocol` but `packages/agent` and `packages/coding-agent`
both re-import those types. In Rust we make this an explicit dependency so we
never accidentally fork a wire type and break round-trip with the TS session
format.

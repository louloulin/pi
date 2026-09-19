# `pi-extensions` — JS extension host for the Pi Rust port

> Stage 3 of [R2] in the LUM-981 / LUM-982 roadmap. This document is
> the Rust port's counterpart to
> [`packages/coding-agent/docs/extensions.md`](../../../../packages/coding-agent/docs/extensions.md);
> every event, UI request, and registration shape below maps to the
> TypeScript `ExtensionAPI` so existing pi extensions can run with
> almost no source change.

The host embeds a QuickJS runtime via [`rquickjs-core`] so JS / TS
extensions live **inside the agent's Rust process** rather than as
out-of-process WASM modules. Loading a `.js` file installs a single
CommonJS-shaped module whose default export is called with the host's
`pi` object.

```text
            ┌────────────────────────────────────────────┐
            │  pi-coding-agent (Rust)                    │
            │   ├── extensions::js_loader (Stage 3)      │
            │   └── pi_extensions::JsExtensionHost       │
            │          ├── AsyncRuntime + AsyncContext   │
            │          ├── host imports (Rust → JS)      │
            │          └── runtime/pi-ext-shim.mjs (JS)  │
            └─────────────┬──────────────────────────────┘
                          │
                          ▼
            ┌────────────────────────────────────────────┐
            │  user extension source                    │
            │   (any .js / .mjs / .ts file on disk)     │
            └────────────────────────────────────────────┘
```

## Table of Contents

- [Quick Start](#quick-start)
- [Wire shapes](#wire-shapes)
  - [`pi` global (TS-side API mirror)](#pi-global-ts-side-api-mirror)
  - [Host imports (Rust → JS)](#host-imports-rust--js)
  - [Events](#events)
  - [UI requests](#ui-requests)
- [Loading extensions](#loading-extensions)
- [Timeouts & isolation](#timeouts--isolation)
- [Compatibility notes](#compatibility-notes)
- [Examples](#examples)

## Quick Start

```rust
use pi_extensions::{
    DispatchOutcome, JsExtensionHost, ScriptedUiAnswers, ScriptedUiHandler,
};
use pi_protocol::ExtensionEvent;
use std::sync::Arc;

let ui = Arc::new(ScriptedUiHandler::new(ScriptedUiAnswers::default()));
let host = JsExtensionHost::with_handler(ui).await?;

host.load(
    pi_extensions::ExtensionEntry {
        source: std::path::PathBuf::from("./my-ext.js"),
        id: "my-ext".into(),
        label: None,
    },
    include_str!("./my-ext.js"),
).await?;

host.emit_event_with(&ExtensionEvent::SessionStart, Some("tui"), true, "/tmp")
    .await?;
```

A minimal `my-ext.js`:

```javascript
module.exports = function (pi) {
  pi.registerTool({
    name: "echo",
    label: "Echo",
    description: "Echo a string",
    parameters: { type: "object" },
    execute: function (args) {
      return { content: [{ type: "text", text: String(args.text) }] };
    },
  });
  pi.on("session_start", function (_event, ctx) {
    ctx.ui.notify("extension loaded", "info");
  });
};
```

## Wire shapes

### `pi` global (TS-side API mirror)

The shim installs a `globalThis.pi` whose shape mirrors the TS
`ExtensionAPI` at minimum surface. Every registration call that talks
back to the host invokes a host import (see below).

| Method                          | Maps to host import             | Notes                                                                |
|---------------------------------|---------------------------------|----------------------------------------------------------------------|
| `pi.on(event, handler)`         | —                               | Registers a handler for `event` (string).                            |
| `pi.registerTool(definition)`   | `host_register_tool(json)`      | `definition.execute(args, ctx)` runs in the JS context.              |
| `pi.registerCommand(name, opt)` | `host_register_command(json)`   | `opt.handler(args, ctx)` runs when the user invokes `/name`.         |
| `pi.appendEntry(type, data)`    | `host_append_entry(type, json)` | Custom session entries (state persistence).                          |
| `pi.sendMessage(message)`       | `host_send_message(json)`       | Custom messages routed back to the agent.                            |
| `pi.sendUserMessage(text)`      | `host_send_user_message(json)`  | User messages enqueued as turn input.                                |
| `pi.setSessionName(name)`       | `host_set_session_name(name)`   | Sets the session display name.                                       |
| `ctx.ui.notify(msg, level)`     | `host_ui_notify(msg, level)`    | Fire-and-forget notification. `level` ∈ `info`/`warning`/`success`/`error`. |
| `ctx.ui.confirm(title, body)`   | `host_ui_confirm(title, body)`  | Returns `Promise<boolean>`. Resolves via `UiHandler::confirm`.       |
| `ctx.ui.input(title, ph)`       | `host_ui_input(title, ph)`      | Returns `Promise<string | null>`. Resolves via `UiHandler::input`.   |
| `ctx.ui.select(title, options)` | `host_ui_select(title, json)`   | Returns `Promise<string | null>`. Resolves via `UiHandler::select`.  |

Event handlers may be `async` — the host awaits the returned promise
inside `JsExtensionHost::emit_event_with`. Synchronous results are
serialised as JSON; `null` becomes `null`.

### Host imports (Rust → JS)

Every host import takes JSON strings as arguments (or a plain string
for the simpler ones) so the ABI is language-agnostic and can be
swapped for a `wasm32` binding later without touching the shim.

| Import                              | Signature                            | Purpose                                                                                       |
|-------------------------------------|--------------------------------------|------------------------------------------------------------------------------------------------|
| `host_register_tool(json)`          | `(json: string) => void`             | `json` is a serialised `ToolDefinition` (`name`, `label`, `description`, `parameters`).        |
| `host_register_command(json)`       | `(json: string) => void`             | `json` is `{ name, description }`.                                                              |
| `host_append_entry(type, dataJson)` | `(type: string, dataJson: string) => void` | Custom session entry. `dataJson` round-trips through `serde_json`.                            |
| `host_send_message(json)`           | `(json: string) => void`             | Push a `CustomMessage` payload back to the agent.                                              |
| `host_send_user_message(json)`      | `(json: string) => void`             | Push a user message (string or content array) into the queue.                                  |
| `host_set_session_name(name)`       | `(name: string) => void`             | Set the session display name.                                                                  |
| `host_ui_notify(message, level)`    | `(message: string, level: string) => void` | Fire-and-forget notify. `level` ∈ `info`/`success`/`warning`/`error`.                          |
| `host_ui_confirm(title, body)`      | `(title: string, body: string) => Promise<boolean>` | Async — resolves via `UiHandler::confirm`.                                          |
| `host_ui_input(title, placeholder)` | `(title: string, placeholder: string) => Promise<string | null>` | Async — resolves via `UiHandler::input`.                                          |
| `host_ui_select(title, options)`    | `(title: string, optionsJson: string) => Promise<string | null>` | Async — `optionsJson` is the JSON-encoded `string[]`. Resolves via `UiHandler::select`.  |
| `host_log(level, message)`          | `(level: string, message: string) => void` | Surface an extension-side log line on the `pi_extension` tracing target.            |

### Events

Stage 3 ships the minimum subset of `ExtensionEvent` the upstream
hooks reference; the wire types live in `pi_protocol::ExtensionEvent`
and the JS shim dispatches whatever event names the extension
subscribes to. The following event tags are emitted by the agent
runtime today:

| Event          | Wire variant                       | Notes                                                  |
|----------------|-------------------------------------|--------------------------------------------------------|
| `session_start`| `ExtensionEvent::SessionStart`      | Host passes `mode`, `hasUI`, `cwd` via `_ctx_*` fields. |
| `session_end`  | `ExtensionEvent::SessionEnd`        | Mirror of upstream `session_shutdown` (alias for now).  |
| `user_message` | `ExtensionEvent::UserMessage { … }` | Carries the full `Message` payload.                     |
| `tool_call`    | `ExtensionEvent::ToolCall { … }`    | Carries the `ToolCall`.                                 |
| `tool_result`  | `ExtensionEvent::ToolResult { … }`  | Carries the `ToolResult`.                               |
| `agent_end`    | `ExtensionEvent::AgentEnd { … }`    | Carries the final `AssistantMessage`.                   |

Event payloads use `serde_json` tagged representation; the shim
inserts three underscore-prefixed context fields before serialising
(`_ctx_mode`, `_ctx_hasUI`, `_ctx_cwd`). Extensions read them off
the event object directly.

### UI requests

UI requests are issued by JS via the `ctx.ui.*` methods. They are
**not** dispatched events — they round-trip through a dedicated
`mpsc` channel that the host drains in a background task. The host
calls into the user-supplied [`UiHandler`] and writes the answer
back to a `oneshot::channel`. JS awaits the resolved promise.

```text
JS ctx.ui.confirm("Are you sure?", "rm -rf foo") ─►
   host_ui_confirm (Async Func) ─► ui_tx ─►
      ui_worker ─► UiHandler::confirm ─► oneshot::reply ─►
   Promise<bool> resolves back in JS
```

The shim short-circuits `ctx.ui.*` when `hasUI` is `false` on the
event context, returning `false` / `null` without contacting the
host — useful for batch / RPC modes.

## Loading extensions

```rust
use pi_coding_agent::extensions::js_loader;
use pi_extensions::{JsExtensionHost, ExtensionSearchPaths};

let host = JsExtensionHost::new().await?;
let paths = js_loader::search_paths(home_dir.as_deref(), &cwd);
let outcome = js_loader::load_extensions(host, &paths, "tui", true, &cwd.display().to_string()).await?;
println!("loaded {} extensions, {} errors", outcome.entries.len(), outcome.errors.len());
```

`js_loader::search_paths` mirrors the upstream resolution rules
(`~/.pi/agent/extensions/` + `.pi/extensions/`). Both the JSON
descriptor path (Stage 0) and the JS path (Stage 3) coexist; the
file extension decides which loader runs.

## Timeouts & isolation

- Each host import and event dispatch is bounded by
  [`DEFAULT_TIMEOUT`] (5 seconds).
- The host installs an **interrupt handler** on the QuickJS runtime
  so an infinite `while(true)` aborts at the next bytecode boundary
  once the deadline is exceeded. The interrupt fires within a few
  thousand opcodes of the deadline.
- Timeouts surface as `ExtensionError::Timeout`; never panic.
- Each `JsExtensionHost` owns one `AsyncRuntime` / `AsyncContext`
  pair. Cloning the host shares the pair. Drives one `runtime.drive()`
  task that pumps JS promises.
- Per-call mutex discipline: rquickjs-core serialises context
  access internally; concurrent JS calls from multiple threads are
  safe by construction.

## Compatibility notes

Stage 3 keeps the JS surface intentionally tiny so the upstream pi
extensions can run with minimal source change. The following upstream
TS constructs are **not** supported in Stage 3 and require an adapter
or a Stage 4+ follow-up:

| Upstream feature                        | Status          | Notes                                                                  |
|-----------------------------------------|-----------------|------------------------------------------------------------------------|
| `module.exports = function (pi) { … }`  | ✅ Supported    | The shim wraps source in a CJS-shaped loader.                          |
| `pi.on(eventName, handler)`             | ✅ Supported    | Event tags are free-form strings; the host dispatches anything.        |
| `pi.registerTool(...)` + `execute(...)` | ✅ Supported    | JSON Schema `parameters` round-trip; result shape matches TS.          |
| `pi.registerCommand(...)`               | ✅ Supported    | Commands are surfaced through `host.log()` for the agent to wire up.   |
| `ctx.ui.confirm / input / select`       | ✅ Supported    | Async; the host awaits the user's `UiHandler` reply.                   |
| `ctx.ui.notify(...)`                    | ✅ Supported    | Fire-and-forget; logged on the `pi_extension` tracing target.          |
| `pi.sendMessage / sendUserMessage`      | ✅ Supported    | Logged; the agent wires them into the runtime in a later stage.        |
| `pi.appendEntry(type, data)`            | ✅ Supported    | Logged; persistence lands in the session backend (Stage 5).            |
| ESM `import` statements                 | ⚠️ Partial     | `import type { … }` lines are stripped by `js_loader`; the rest fails. |
| TypeBox parameter schemas               | ✅ Wire-only    | The JSON Schema `parameters` field is preserved verbatim.               |
| Custom renderers (`registerMessageRenderer`, …) | ❌ Out of scope | Land in Stage 4 alongside the TUI.                          |
| Custom editor / footer / header         | ❌ Out of scope | TUI concern (Stage 4).                                                 |
| Provider registration                   | ❌ Out of scope | Stage 1 + later `pi-ai` work.                                          |

When a feature is not yet wired, the corresponding host import can be
added in a follow-up commit without changing the JS-side shape.

## Examples

The `examples/` directory carries verbatim copies of the upstream TS
examples as a compatibility reference. The end-to-end tests in
`tests/e2e.rs` translate each to a small JS source so the test
binary does not need a TypeScript compiler:

- `hello.ts` → JS equivalent registers a `hello` tool that greets a
  name; the e2e test loads it and calls `host.execute_tool("hello", …)`.
- `notify.ts` → JS equivalent subscribes to `session_start` /
  `agent_settled` and calls `ctx.ui.notify`.
- `custom-commands.ts` → JS equivalent registers an `/echo` command.

To run the examples against a real shell:

```bash
cargo test -p pi-extensions --test e2e -- --nocapture --test-threads=1
```
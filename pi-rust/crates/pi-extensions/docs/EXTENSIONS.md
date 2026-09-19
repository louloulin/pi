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
host. It also raises a `warning` notification naming the denied
request, so a plugin author sees *why* the prompt never appeared
instead of guessing. `hasUI` is therefore the honest answer to "can
this process show a dialog?", not a mode name.

#### Interactive mode (TUI)

In the TUI the answer is a key press: `pi-coding-agent` installs a
`TuiUiHandler` that turns every request into a `pi-tui` `Dialog` modal
and awaits the `oneshot` the App resolves.

| Request                        | Modal                                     | Answer                        |
|--------------------------------|-------------------------------------------|-------------------------------|
| `ctx.ui.confirm(title, body)`  | yes — Enter/`y` accept, `n`/Esc deny       | `true` / `false`              |
| `ctx.ui.input(title, ph)`      | yes — text editor, Enter submits           | `string` / `null` on Esc      |
| `ctx.ui.select(title, options)`| yes — arrow / `j` `k` / `g` `G` + Enter    | `string` / `null` on Esc      |
| `ctx.ui.notify(msg, level)`    | no — appended to the transcript            | `NotifyAck` (fire-and-forget) |

While a modal is open it owns the keyboard: the prompt underneath
freezes and `Ctrl+C` / `Esc` cancel the *dialog* (deny / `null`)
rather than the turn or the app. The bridge has a `ready` gate that
is closed until the render loop starts, so a `session_start` handler
that prompts during extension loading gets the deny default instead
of stalling the host on a modal nobody can answer; the gate closes
again when the loop exits. A second request arriving while a modal is
already open is likewise denied rather than queued — an extension
never waits on a prompt the user cannot see.

#### Non-interactive modes (print / rpc / no TTY)

`pi --print`, `pi rpc`, and any run whose stdin or stdout is not a
terminal keep the stderr-backed handler: `confirm` → `false`,
`input` / `select` → `null`, `notify` → stderr. These are deliberate,
documented denials, never a hang — an RPC client that does not
implement a UI protocol is not left waiting for a reply it cannot
produce.

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

### CLI wiring

The `pi` binary drives the loader itself — extensions load before the
mode starts and their tools are advertised to the model:

```text
pi                                   # discovers ~/.pi/agent/extensions + .pi/extensions
pi -e ./my-ext.js                    # one extra file (or directory)
pi --extensions-dir ./ext-registry   # extra directory, repeatable
pi --no-extensions                   # built-in tool bundle only
```

`pi-coding-agent`'s `extensions::wiring::load` runs one load pass, fires
`session_start` at every loaded extension, and wraps the host in an
`ExtensionToolExecutor` that merges `BuiltinToolExecutor` definitions
with `JsExtensionHost::registered_tools()`. Built-in names win on
collision, so an extension can never silently shadow `bash` / `read` /
`write` / `edit` / `find` / `grep` / `ls`.

Both the load pass and the agent loop must run on the same tokio
runtime: the host spawns its promise driver and UI worker on the runtime
it is created in, so `wiring::load` takes the runtime the mode is about
to use instead of building its own.

### Extension commands & side effects

`load` also returns an `ExtensionRuntime` (in `pi-coding-agent`'s
extensions wiring) alongside the executor. It carries the commands every
extension registered and exposes the host for two jobs:

```rust
runtime.has_command("greet");        // is `/greet` an extension command?
runtime.execute_command("greet", args).await;  // run the JS handler
runtime.drain_side_effects();        // take appendEntry/sendMessage/…
```

* **Commands.** In the TUI a `/name` that no built-in owns is matched
  against `runtime.commands()` and, on a hit, the JS handler runs with
  the raw argument text; the return value is shown in the transcript and
  `/help` lists every registered command. `pi --print "/name args"`
  does the same thing but never contacts the model — the handler runs
  before the agent is built and the result is emitted as
  `text` / `json` / `json-events`.
* **Side effects.** `pi.appendEntry`, `pi.sendMessage`,
  `pi.sendUserMessage` and `pi.setSessionName` accumulate in the host and
  are drained by the mode, which appends them to the session store
  (`SessionEntry::Extension` in SQLite for print mode, JSONL for the
  TUI). Draining takes them, so nothing is written twice.

Unknown `/name` prompts still fall through to the previous behaviour
(model turn in print mode, "unknown command" notice in the TUI), so the
runtime can never swallow a prompt that was meant for the agent.

## Timeouts & isolation

- Each host import and event dispatch is bounded by
  [`DEFAULT_TIMEOUT`] (5 seconds). Interactive mode raises the ceiling
to 300 s (`wiring::INTERACTIVE_UI_TIMEOUT`): the extension is awaiting
a *human*, and 5 s is not enough to read a confirmation. The
interactive `UiHandler` still answers instantly when the TUI is not
pumping dialogs, so the wider budget only applies to a visible modal.
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
| `pi.registerCommand(...)`               | ✅ Supported    | Dispatched by the TUI (`/name args`) and by `pi --print "/name args"`; the JS handler runs in the host. |
| `ctx.ui.confirm / input / select`       | ✅ Supported    | Async; the host awaits the user's `UiHandler` reply. The TUI renders a real modal dialog; print / rpc / non-TTY runs deny (`false` / `null`) and report the denial through `ctx.ui.notify`. |
| `ctx.ui.notify(...)`                    | ✅ Supported    | Fire-and-forget; logged on the `pi_extension` tracing target.          |
| `pi.sendMessage / sendUserMessage`      | ✅ Supported    | Persisted as session entries by the TUI and print modes; `sendUserMessage` is not re-injected as a new turn yet. |
| `pi.appendEntry(type, data)`            | ✅ Supported    | Persisted to the session backend as `SessionEntry::Extension` (TUI JSONL + print-mode SQLite). |
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
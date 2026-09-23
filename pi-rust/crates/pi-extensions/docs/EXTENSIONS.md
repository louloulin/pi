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
  - [`fetch` (global)](#fetch-global)
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
| `pi.exec(command, args, opts)`  | `host_exec(command, json)`      | Runs a child process; resolves with `{ stdout, stderr, code, killed }`. `opts.signal` cancels via `host_exec_cancel(id)`. |
| `ctx.ui.notify(msg, level)`     | `host_ui_notify(msg, level)`    | Fire-and-forget notification. `level` ∈ `info`/`warning`/`success`/`error`. |
| `ctx.ui.theme`                  | —                               | Identity styling object (`fg` / `bg` / `bold` / … return their text); the host has no palette. |
| `ctx.ui.custom(factory, options?)` | `host_ui_region("customOpen"/"customClose"/"customSetVisible", json)` | Returns a thenable `CustomHandle` (`resolve` / `close` / `done` / `isVisible` / `setVisible`) whose component renders in the TUI's overlay (or, without `overlay`, in the editor region) until it is resolved. Without a region host it resolves `undefined` and warns, like `confirm` in a non-interactive run. |
| `ctx.ui.setWidget` / `setHeader` / `setFooter` / `setEditorComponent` | `host_ui_region("setWidget"/"setHeader"/"setFooter"/"setEditorComponent", json)` | Installs the region component, or clears it for `null` / `undefined`. |
| `ctx.ui.setStatus(key, text)` | `host_ui_region("setStatus", json)` | Sets (`text`) or clears (`undefined`) one footer status line. Upstream draws every status as one extra footer row, sorted by key (`packages/coding-agent/src/modes/interactive/components/footer.ts:243-251`); the TUI renders the same row. The shim also keeps the map, so a custom footer receives it through `footerData.getExtensionStatuses()`. |
| `ctx.ui.setTitle(title)` | `host_ui_region("setTitle", json)` | Sets the terminal window/tab title. Upstream writes OSC 0 + BEL (`packages/tui/src/terminal.ts:520-523`); the TUI queues the title and the driver writes the sequence on the tty, so it never enters the frame buffer. Control characters in the title are replaced by spaces before the sequence is built. |
| `ctx.ui.setEditorText` / `setTheme` / … | — | The remaining display methods stay inert no-ops with a one-time warning notification. See [`docs/SDK_MODULES.md`](SDK_MODULES.md). |
| `ctx.ui.addAutocompleteProvider(factory)` | `host_ui_autocomplete("register"/"baseGetSuggestions"/"baseApplyCompletion"/"baseShouldTriggerFileCompletion", json)` | Stacks the factory's provider on the composer. The wrapper chain lives in the shim (each factory receives the provider it wraps as `current`), the host supplies the built-in `CombinedAutocompleteProvider` the chain delegates to, and `_pi_autocomplete_call(op, json)` is the Rust→JS entry point. `triggerCharacters` replaces the editor's table (defaults `@`, `#`). Only **synchronous** callbacks are served: a Promise-returning `getSuggestions` / `applyCompletion` / `shouldTriggerFileCompletion` is not awaited, that one call falls back to the built-in provider and a warning is reported once. See [`docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`](../../../docs/LUM1448_AUTOCOMPLETE_PROVIDER.md). |
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
| `host_register_provider(json)`      | `(json: string) => void` (throws)    | `json` is `{ name, displayName?, baseUrl?, apiKey?, api?, models? }`. Validates the name / `api` family and throws a JS `Error` on a bad registration; a repeated `name` overwrites in place. |
| `host_unregister_provider(name)`    | `(name: string) => void`             | Drops a provider registration. A no-op for an unknown name.                                     |
| `host_append_entry(type, dataJson)` | `(type: string, dataJson: string) => void` | Custom session entry. `dataJson` round-trips through `serde_json`.                            |
| `host_send_message(json)`           | `(json: string) => void`             | Push a `CustomMessage` payload back to the agent.                                              |
| `host_send_user_message(json)`      | `(json: string) => void`             | Push a user message (string or content array) into the queue.                                  |
| `host_set_session_name(name)`       | `(name: string) => void`             | Set the session display name.                                                                  |
| `host_exec(command, argsJson)`      | `(command: string, argsJson: string) => Promise<string>` | Runs `command` with `argsJson` = `{ id?, args, cwd?, timeout? }` and returns the JSON `ExecResult` `{stdout, stderr, code, killed}`. Never rejects. `id` is cancellation/deadline bookkeeping and is optional. |
| `host_exec_cancel(id)`              | `(id: number) => void`               | Cancel the `host_exec` call whose `argsJson.id` is `id`: kills the child and makes `host_exec` resolve with `{ code: -1, killed: true }`. A no-op for an unknown or finished `id`. The shim calls it when `options.signal` fires. |
| `host_fetch(requestJson)`           | `(requestJson: string) => Promise<string>` | The `fetch` global's transport. `requestJson` = `{ id?, url, method?, headers?, body?(base64), timeout? }`; resolves with the JSON `{ ok, status, statusText, url, redirected, headers, body(base64) }` envelope, or `{ ok: false, name, message }`. Never rejects. |
| `host_fetch_cancel(id)`             | `(id: number) => void`               | Cancel the `host_fetch` call whose `requestJson.id` is `id`: drops the in-flight request and makes `host_fetch` resolve with `{ ok: false, name: "AbortError", … }`. A no-op for an unknown or finished `id`. The shim calls it when the request's `signal` fires. |
| `host_ui_notify(message, level)`    | `(message: string, level: string) => void` | Fire-and-forget notify. `level` ∈ `info`/`success`/`warning`/`error`.                          |
| `host_ui_confirm(title, body)`      | `(title: string, body: string) => Promise<boolean>` | Async — resolves via `UiHandler::confirm`.                                          |
| `host_ui_input(title, placeholder)` | `(title: string, placeholder: string) => Promise<string | null>` | Async — resolves via `UiHandler::input`.                                          |
| `host_ui_select(title, options)`    | `(title: string, optionsJson: string) => Promise<string | null>` | Async — `optionsJson` is the JSON-encoded `string[]`. Resolves via `UiHandler::select`.  |
| `host_ui_region(op, payloadJson)`   | `(op: string, payloadJson: string) => string` | The synchronous region bridge behind `ctx.ui.setWidget` / `setHeader` / `setFooter` / `setEditorComponent` / `custom`. Returns `{"ok":true}` (plus `session` for `customOpen`) or `{"ok":false,"error":"…"}`. The mutation is forwarded to the injected `UiRegionHost` on a worker task; `op: "wake"` only asks the host to poll its async driver so a component factory scheduled on a microtask runs. |
| `host_ui_autocomplete(op, payloadJson)` | `(op: string, payloadJson: string) => string` | The synchronous bridge behind `ctx.ui.addAutocompleteProvider`. `op: "register"` records a (re-)registration (bumping the generation the interactive loop watches); `baseGetSuggestions` / `baseApplyCompletion` / `baseShouldTriggerFileCompletion` delegate to the injected `AutocompleteBaseProvider` (the built-in `CombinedAutocompleteProvider`) and return `{"ok":true,"suggestions"|"completion"|"value":…}` or `{"ok":false,"error":"…"}`. Synchronous because a wrapper calls `current.getSuggestions(...)` from inside a plain function call. |
| `host_log(level, message)`          | `(level: string, message: string) => void` | Surface an extension-side log line on the `pi_extension` tracing target.            |
| `host_child_read(handle, stream)`   | `(handle: number, stream: string) => Promise<string>` | One chunk of a `node:child_process` pipe: `{data,done}`. See [`docs/NODE_BUILTINS.md`](NODE_BUILTINS.md). |
| `host_child_wait(handle)`           | `(handle: number) => Promise<string>` | Wait for a `node:child_process` child: `{code,signal,…}`.                            |
| `host_pi_ai_stream_start(requestJson)` | `(requestJson: string) => Promise<string>` | Starts one built-in pi-ai provider stream (LUM-1180). `requestJson` = `{api, model, context, options}`; resolves with `{ok:true,id}` or `{ok:false,error}`. The shim strips `options.signal` — the host owns cancellation. |
| `host_pi_ai_stream_next(id)`        | `(id: number) => Promise<string>`    | One upstream-shaped `AssistantMessageEvent` at a time: `{ok:true,done:false,event}`, `{ok:true,done:true}`, or `{ok:false,error}`. Never rejects. |
| `host_pi_ai_stream_cancel(id)`      | `(id: number) => void`               | Aborts the request and drops the host-side stream (and its connection). A no-op for an unknown or finished `id`. The shim calls it when the stream's `AbortSignal` fires and again when the pump exits. |
| `host_node_call(op, argsJson)`      | `(op: string, argsJson: string) => string` | The single bridge behind the `node:*` virtual modules (fs / os / buffer / crypto / process). Returns a JSON envelope (`{"ok":true,"value":…}` or `{"ok":false,"code","message","syscall","path"}`); see [`docs/NODE_BUILTINS.md`](NODE_BUILTINS.md). |

`host_node_call` is the only host import that is *not* part of the
upstream extension API: the shim uses it internally to back the Node
builtin virtual modules, so extension code keeps importing `node:fs`
etc. exactly as it does on Node.

`host_exec` is what backs `pi.exec` — the documented shell-out API every
git-driven example extension uses (`packages/coding-agent/examples/
extensions/{auto-commit-on-exit,dirty-repo-guard,git-checkpoint,
git-merge-and-resolve}.ts`). The child is spawned directly (no shell) with
`cwd` defaulting to the session working directory, so extensions that used
`pi.exec` upstream run unmodified. Divergences from upstream `execCommand`:

* `options.signal` works, out of band: a signal is not JSON, so the shim
  allocates a call `id`, passes it in `argsJson`, and calls
  `host_exec_cancel(id)` when the signal fires. The host kills the child and
  the promise resolves with `{ code: -1, killed: true }` (never rejects) —
  the same contract as upstream. An already-aborted signal resolves
  immediately without spawning anything.
* `options.timeout` (ms) is a promise the host keeps: while such a call is
  in flight the host per-call deadline is raised to `options.timeout + 1 s`
  ([`EXEC_TIMEOUT_GRACE`](../src/host.rs)), so the child's own timeout is
  what ends the call. Without an explicit `options.timeout` the host per-call
  timeout still applies unchanged — [`DEFAULT_TIMEOUT`](../src/host.rs)
  (5 s) in print / RPC, 300 s in the interactive TUI — so a long `git fetch`
  needs `options.timeout` or an interactive session. `options.timeout` is
  clamped to 24 h.
* killing a child kills only that process, not its descendants: a
  `sh -c '…'` wrapper leaves grandchildren alive, and because they inherit
  the stdio pipes the call still waits for them to exit (Node behaves the
  same way).
* a timeout kills with `SIGKILL` (upstream sends `SIGTERM`, then `SIGKILL`
  after 5 s) and reports `code: -1`; Node collapses the missing exit code
  to `0`, which would make a killed command look successful.
* a spawn failure (`ENOENT`) reports the OS error in `stderr` with
  `code: 1` instead of dropping the message.

QuickJS ships no `AbortController`, so the shim installs a DOM-shaped
polyfill (`aborted`, `reason`, `throwIfAborted()`, `addEventListener` /
`removeEventListener` / `onabort`, plus the `AbortSignal.abort()` and
`AbortSignal.any()` statics). `AbortSignal.timeout(ms)` is **not**
implemented — it needs a timer and the host exposes neither `setTimeout`
nor `node:timers`; extensions should pass `options.timeout` instead.

### `fetch` (global)

Upstream extensions run in Node/Bun and therefore use the platform `fetch`;
the repo's own `.pi/extensions/import-repro.ts` reads gists and issue comments
with it. QuickJS ships no `fetch` and a JS-only polyfill cannot reach the
network, so `fetch` is a host bridge: the shim serialises the request, the
Rust side performs it with the same `reqwest` stack the `pi-ai` providers use
(rustls TLS and the default proxy-env handling), and the shim rebuilds a
`Response` from the returned bytes.

Implemented surface:

* `fetch(input, init)` where `input` is a URL string or a `Request`, and
  `init.method` / `init.headers` / `init.body` / `init.signal` / `init.timeout`.
* `Headers` (`append` / `set` / `get` / `has` / `delete` / `forEach` /
  `keys` / `values` / `entries` / iterator), `Request`, and `Response`
  (`ok` / `status` / `statusText` / `url` / `redirected` / `headers` /
  `bodyUsed` / `text()` / `json()` / `arrayBuffer()` / `clone()`).
* Bodies: `string`, `ArrayBuffer`, `TypedArray` / `DataView`,
  `URLSearchParams`.
* `init.signal` cancels out of band exactly like `pi.exec(options.signal)`: the
  shim allocates an `id` from the same counter `pi.exec` uses, passes it in the
  request JSON, and calls `host_fetch_cancel(id)` when the signal fires; the
  promise then rejects with an `AbortError` (`name: "AbortError"`, DOM-shaped).
  A non-standard `init.timeout` (ms) rejects with a `TimeoutError` and, like
  `pi.exec`, raises the host per-call deadline to `timeout + 1 s` while in
  flight. Both are clamped to the same 24 h ceiling.

Divergences from upstream `fetch`, all deliberate:

* no streaming: `Response.body` is `null` and there is no `ReadableStream`, so
  the whole body is buffered host-side before the promise resolves;
* no `FormData` / `Blob` bodies;
* `credentials`, `mode`, `cache`, `redirect`, `keepalive` and
  `referrer` are ignored (no browser origin, no cookie jar);
* a network failure rejects with a `TypeError` whose message is the transport
  error (`reqwest`'s text) rather than a `TypeError: fetch failed` plus a
  `cause`;
* `redirected` is derived as `finalUrl !== requestUrl` rather than reported as
  a redirect count;
* automatic content decompression is off (the workspace `reqwest` build has no
  `gzip` feature), so no `Accept-Encoding` is sent and the caller sees the raw
  bytes — decompress with `node:zlib` if a server ignores that and compresses
  anyway.

HTTP already-covered behaviour worth knowing: a 4xx/5xx response **resolves**
with `ok: false` (never rejects), exactly like upstream, and the response
headers are lower-cased by `reqwest` as HTTP requires.

### Node builtin virtual modules

Extensions run on Node upstream, so `node:fs`, `node:fs/promises`,
`node:os`, `node:buffer`, `node:crypto`, `node:process` and `node:util`
(plus the `Buffer` / `process` globals, and `TextEncoder` / `TextDecoder`)
are provided as virtual modules in both module formats —
`import { readFileSync } from "node:fs"` and
`require("node:fs")`. `node:util` is pure JS and needs no host op; the
rest ride the single `host_node_call` bridge. The supported subset, the
error shape and every deliberate divergence from Node are documented in
[`docs/NODE_BUILTINS.md`](NODE_BUILTINS.md). `node:child_process` is
bridged too (`spawn` / `execFile` / `exec`, streaming stdio via
`host_child_read` / `host_child_wait`); a live child outlives the call
that created it, so it uses two extra async host imports.

The web-platform names extensions treat as ambient are polyfilled by the
shim when the engine lacks them: `atob` / `btoa`, `crypto` (`getRandomValues`
/ `randomUUID` / `subtle.digest`, backed by the host `crypto.digest` op over
SHA-1 / SHA-256), `URLSearchParams`, and `URL` (LUM-1177). `npx`-style legacy
OAuth extensions such as `custom-provider-anthropic/index.ts` need exactly this
set to build a PKCE challenge and an authorize URL, and the sibling
`custom-provider-gitlab-duo/index.ts` additionally parses the OAuth callback
with `new URL(callbackUrl).searchParams.get("code")`, which is what motivated
the `URL` global. Its divergences from Node are listed in the
[globals table](NODE_BUILTINS.md#globals).

### SDK virtual modules (`@earendil-works/*`)

Upstream extensions also import pi's own SDK packages. The shim ships
them as virtual modules registered under three spellings each —
`@earendil-works/<pkg>`, the historical `@mariozechner/<pkg>`, and the
bare package name:

```js
import { Text, Markdown, SelectList } from "@earendil-works/pi-tui";
import { defineTool, getAgentDir, parseFrontmatter } from "@earendil-works/pi-coding-agent";
import { Type, StringEnum, uuidv7 } from "@earendil-works/pi-ai";
```

The components and helpers are pure JS in the shim (only `getAgentDir`
and `uuidv7` reach the host, through `host_node_call`). Modules are
proxies with an explicit inventory: a **value** import of a real
upstream export the shim does not implement throws
`ERR_PI_SDK_UNIMPLEMENTED` (naming the specifier, the export and the
doc), and any other unknown name throws `ERR_PI_SDK_UNKNOWN_EXPORT` — a
missing name is never silently `undefined`. `import type { … }` is
erased by the loader, so type-only imports need no binding.

The per-specifier inventory, the documented gaps (the streaming /
provider entries, the third-party `gondolin` sandbox) and every
divergence are in
[`docs/SDK_MODULES.md`](SDK_MODULES.md). `globalThis.__pi_sdk_manifest()`
exposes the same inventory as JSON, and `tests/sdk_modules.rs` checks it
against the upstream examples so a new import cannot outrun the bridge.

### Events

Stage 3 ships the minimum subset of `ExtensionEvent` the upstream
hooks reference; the wire types live in `pi_protocol::ExtensionEvent`
and the JS shim dispatches whatever event names the extension
subscribes to. The following event tags are emitted by the agent
runtime today:

| Event                | Wire variant                             | Notes                                                  |
|----------------------|------------------------------------------|--------------------------------------------------------|
| `session_start`      | `ExtensionEvent::SessionStart`            | Host passes `mode`, `hasUI`, `cwd` via `_ctx_*` fields. |
| `session_shutdown`   | `ExtensionEvent::SessionShutdown`         | Upstream name. The shim still accepts `session_end` as an alias. |
| `session_info_changed`| `ExtensionEvent::SessionInfoChanged`     | `/name` and `ctx.setSessionName`.                      |
| `session_compact`    | `ExtensionEvent::SessionCompact`          | Manual and automatic compaction.                        |
| `resources_discover` | `ExtensionEvent::ResourcesDiscover`       | Fired while the extension set loads.                    |
| `agent_start`        | `ExtensionEvent::AgentStart`              | Brackets one agent run.                                 |
| `agent_end`          | `ExtensionEvent::AgentEnd { messages }`   | Carries the whole message log, as upstream does.        |
| `turn_start`         | `ExtensionEvent::TurnStart`               | `turnIndex` counts from 0 within the run.               |
| `turn_end`           | `ExtensionEvent::TurnEnd`                 | `message` + `toolResults`.                              |
| `message_start`      | `ExtensionEvent::MessageStart`            | Empty assistant message; the loop only streams deltas.  |
| `message_update`     | `ExtensionEvent::MessageUpdate`           | `assistantMessageEvent` (the `type`-tagged delta).      |
| `message_end`        | `ExtensionEvent::MessageEnd`              |                                                         |
| `tool_execution_start`| `ExtensionEvent::ToolExecutionStart`     | Sent after the legacy `tool_call`.                      |
| `tool_execution_update`| `ExtensionEvent::ToolExecutionUpdate`   | Sent after the legacy `tool_call`.                      |
| `tool_execution_end` | `ExtensionEvent::ToolExecutionEnd`        | Sent after the legacy `tool_result`.                    |
| `thinking_level_select`| `ExtensionEvent::ThinkingLevelSelect`   | Shift+Tab cycle, `/thinking`, the selector.             |
| `user_bash`          | `ExtensionEvent::UserBash`                | Local `!` / `!!` commands.                              |
| `input`              | `ExtensionEvent::Input`                   | Every submitted user message.                           |
| `user_message`       | `ExtensionEvent::UserMessage { … }`       | Port-only extra; carries the full `Message` payload.    |
| `tool_call`          | `ExtensionEvent::ToolCall { … }`          | Port-only extra; carries the `ToolCall`.                |
| `tool_result`        | `ExtensionEvent::ToolResult { … }`        | Port-only extra; carries the `ToolResult`.              |

LUM-1432 closed the rest of the upstream surface — the port now declares and emits
**all 36 upstream event names** (`python pi-rust/scripts/extension_event_coverage.py pi-rust`
prints `production emit sites: 36/36`). The events added by that round:

| Event | Wire variant | Semantics |
|-------|--------------|-----------|
| `project_trust` | `ExtensionEvent::ProjectTrust` | Asked between the two extension-load passes; the first `yes`/`no` wins, `undecided` falls through. `remember: true` persists to the trust store. |
| `session_before_switch` | `ExtensionEvent::SessionBeforeSwitch` | Before `/new` (`reason: new`) and `/resume` (`reason: resume`); `{cancel:true}` vetoes. |
| `session_before_fork` | `ExtensionEvent::SessionBeforeFork` | Before `/fork`; `{cancel:true}` vetoes. |
| `session_before_compact` | `ExtensionEvent::SessionBeforeCompact` | Before `/compact` and auto-compaction; `{cancel:true}` vetoes. |
| `session_compact_failed` | `ExtensionEvent::SessionCompactFailed` | Compaction error path. |
| `session_before_tree` | `ExtensionEvent::SessionBeforeTree` | Before `/tree` navigation; `{cancel:true}` vetoes. |
| `session_tree` | `ExtensionEvent::SessionTree` | After the leaf moved. |
| `context` | `ExtensionEvent::Context` | Before each provider call; `{messages}` replaces the list the request carries. |
| `before_provider_request` | `ExtensionEvent::BeforeProviderRequest` | Request boundary. ⚠️ Handler return values are not applied yet (no wire-payload seam in `pi-ai`). |
| `before_provider_headers` | `ExtensionEvent::BeforeProviderHeaders` | Header boundary. ⚠️ Same gap. |
| `after_provider_response` | `ExtensionEvent::AfterProviderResponse` | `status` is derived (200 = stream established, 0 = call failed); `headers` empty. |
| `before_agent_start` | `ExtensionEvent::BeforeAgentStart` | Before the run; `{systemPrompt}` replaces the system prompt for that run. |
| `agent_settled` | `ExtensionEvent::AgentSettled` | After the run, including the error path. |
| `ui_prompt_start` / `ui_prompt_end` | `ExtensionEvent::UiPrompt{Start,End}` | Brackets a blocking `ctx.ui.confirm/input/select` in the TUI. |
| `model_select` | `ExtensionEvent::ModelSelect` | Now emitted from `/model` (`apply_selector_choice`), with `source: set`. |

The name table (upstream names, the Rust-only tags, and the `session_end` →
`session_shutdown` alias) lives in `crates/pi-extensions/src/events.rs` and is
mirrored in `runtime/pi-ext-shim.mjs`; a unit test compares the two lists so they
cannot drift. Details, emit sites and the documented fidelity gaps are in
`pi-rust/docs/LUM1432_EXTENSION_EVENTS.md`.

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
- Timeouts surface as `ExtensionError::Timeout`; never panic. One
  exception: while a `pi.exec` call that passed an explicit
  `options.timeout` is in flight, the host deadline is raised to that
  timeout plus 1 s so the child's own timeout and the extension's cancel
  handling get to decide the outcome — the host never cuts the call short
  before the deadline the extension asked for (`drive_call` in
  `src/host.rs`). A cancellation is not a timeout: it resolves the
  `pi.exec` promise with `{ code: -1, killed: true }`.
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
| `pi.exec(command, args, options)`        | ✅ Supported    | Spawns directly (no shell); `cwd` defaults to the session cwd; resolves with `{ stdout, stderr, code, killed }`. `options.signal` cancels the child, `options.timeout` replaces the host per-call timeout (see [Host imports](#host-imports-rust--js)). |
| `ctx.ui.confirm / input / select`       | ✅ Supported    | Async; the host awaits the user's `UiHandler` reply. The TUI renders a real modal dialog; print / rpc / non-TTY runs deny (`false` / `null`) and report the denial through `ctx.ui.notify`. |
| `ctx.ui.notify(...)`                    | ✅ Supported    | Fire-and-forget; logged on the `pi_extension` tracing target.          |
| `pi.sendMessage / sendUserMessage`      | ✅ Supported    | Persisted as session entries by the TUI and print modes; `sendUserMessage` is not re-injected as a new turn yet. |
| `pi.appendEntry(type, data)`            | ✅ Supported    | Persisted to the session backend as `SessionEntry::Extension` (TUI JSONL + print-mode SQLite). |
| ESM `import` statements                 | ✅ Supported   | `import type { … }` lines are erased; value imports resolve through the virtual module map (`node:*`, `node:path`, `node:url`, `typebox`, `@earendil-works/*`, …); anything else fails with a readable error naming the specifier. |
| `require("node:fs")` (CJS)             | ✅ Supported   | `require` resolves through the same virtual module map as the ESM rewrite. |
| `node:fs` / `node:fs/promises`          | ✅ Subset      | Sync + promise + callback forms; see [`docs/NODE_BUILTINS.md`](NODE_BUILTINS.md) for the op list and divergences. |
| `node:os` / `node:buffer` / `node:crypto` / `node:process` / `node:util` | ✅ Subset | Idem. `Buffer` and `process` are also installed as globals; `node:util` is pure JS (`promisify` / `inspect` / `format` / `types` / `TextEncoder` / …) and installs `TextEncoder` / `TextDecoder` globally when the engine lacks them. `node:crypto` covers entropy (`randomBytes` / `randomUUID` / `randomInt` / `getRandomValues`) plus SHA-1 / SHA-256 digests (`createHash`, `subtle.digest`); `createHmac` and key-based WebCrypto still throw. |
| `node:child_process`                    | ✅ Subset      | `spawn` / `execFile` / `exec` with streaming stdio; a live child outlives the creating call via `host_child_read` / `host_child_wait`. Extensions that only shell out should still prefer the documented `pi.exec` API. |
| `fetch` / `Headers` / `Request` / `Response` | ✅ Subset | Backed by the `host_fetch` import over the same `reqwest` stack the providers use; `body` is buffered (no streams), `signal` and a non-standard `timeout` are honoured. See the [`fetch` section](#fetch-global). |
| `@earendil-works/pi-tui`                | ✅ Subset      | Components (`Text` / `Box` / `Container` / `Markdown` / `SelectList` / `SettingsList` / `Editor` / `Input` / …) and the ANSI geometry helpers, as free-standing renderables — no live terminal. See [`docs/SDK_MODULES.md`](SDK_MODULES.md). |
| `@earendil-works/pi-coding-agent`       | ✅ Subset      | `defineTool`, `getAgentDir`, `parseFrontmatter`, `truncateHead`/`truncateLine`, `formatSize`, `convertToLlm`, `serializeConversation`, `withFileMutationQueue`, `VERSION`, the theme getters, the loader/border components, and the seven `create*Tool` built-in tool factories (`createReadTool` / `createWriteTool` / `createEditTool` / `createBashTool` / `createFindTool` / `createGrepTool` / `createLsTool`), which delegate to the host's own tool bundle through the `host_builtin_tool` bridge. See [`docs/SDK_MODULES.md`](SDK_MODULES.md) for the `cwd` rebasing divergence and the ignored `spawnHook`. |
| `@earendil-works/pi-ai`                 | ✅ Subset      | `Type`, `StringEnum`, `uuidv7`, `calculateCost`, `contentText`, and the pure-JS event-stream trio (`EventStream` / `AssistantMessageEventStream` / `createAssistantMessageEventStream`). |
| `@earendil-works/pi-ai/compat`          | ✅ Subset      | The pure-JS provider registry (`registerApiProvider` / `unregisterApiProviders` / `getApiProvider(s)` / `stream(Simple)` / `complete(Simple)`) and the event-stream factory, plus the built-in `anthropicMessagesApi` / `openAIResponsesApi` factories (LUM-1180) and `openAICompletionsApi` / `googleGenerativeAIApi` / `azureOpenAIResponsesApi` (LUM-1204) with `registerBuiltInApiProviders` / `resetApiProviders`, which stream from the host's own `AnthropicProvider` / `OpenAiResponsesProvider` / `OpenAiProvider` / `GoogleProvider` / `AzureOpenAiResponsesProvider` across the `host_pi_ai_stream_*` bridge. All five are dispatched by the bundled `pi-coding-agent` runner (LUM-1211): `ext_bridge::model_from_js` accepts the five api ids and `BuiltinPiAiStreamRunner` picks each adapter through the same table as the CLI's `ProviderRouter`, so every factory has a real loopback-SSE integration test. An extension that reaches for a non-bridged api gets one shared error naming the bridged set and each remaining gap. The remaining five upstream lazy apis (`bedrockConverseStreamApi`, `googleVertexApi`, `mistralConversationsApi`, `openAICodexResponsesApi`, `piMessagesApi`) stay `ERR_PI_SDK_UNIMPLEMENTED`, each with a `reason` naming the missing adapter — except `mistralConversationsApi`, whose adapter exists but is not routed yet. See [`docs/SDK_MODULES.md`](SDK_MODULES.md) for that gap table plus the custom-header / thinking-block / cost divergences. |
| `@earendil-works/pi-agent-core`         | ✅ Supported   | Resolves; upstream imports here are type-only and erased. |
| `@earendil-works/gondolin`              | ❌ Not bridged | Third-party sandbox VM (`VM`, `RealFSProvider`); throws a clear `ERR_PI_SDK_UNIMPLEMENTED`. Use the upstream Node runtime for it. |
| Old `@mariozechner/*` scope / bare package names | ✅ Supported | Every SDK specifier is registered under `@earendil-works/<pkg>`, `@mariozechner/<pkg>` and `<pkg>`. |
| TypeBox parameter schemas               | ✅ Wire-only    | The JSON Schema `parameters` field is preserved verbatim.               |
| Custom renderers (`registerMessageRenderer`, …) | ❌ Out of scope | No host bridge: the renderer registry is a separate upstream surface. `ctx.ui.custom()` does render its component in the TUI overlay. |
| Custom editor / footer / header / widgets | ✅ Supported   | `ctx.ui.custom` / `setWidget` / `setHeader` / `setFooter` / `setEditorComponent` render through the `UiRegionHost` bridge (`pi-coding-agent` injects the TUI adapter; LUM-1190). A custom footer receives upstream's full `footerData` provider — `getGitBranch()` / `getAvailableProviderCount()` / `getExtensionStatuses()` / `onBranchChange(cb)` (LUM-1490) — read from a session-scoped snapshot the interactive driver pushes every frame, so a `git checkout` in another terminal is visible without a restart. The remaining status-line `set*` methods stay inert no-ops so extensions that configure them at load time still load. |
| `ctx.ui.addAutocompleteProvider(factory)` | ✅ Subset      | The upstream wrapper-chain API: the factory receives `current` (the built-in provider, or the previous wrapper) and returns `{ triggerCharacters, getSuggestions, applyCompletion, shouldTriggerFileCompletion }`; the chain lives in the shim and its `triggerCharacters` are deduplicated into the editor's table (LUM-1448). ⚠️ Divergences: upstream's `getSuggestions` is `async` (it may `await pi.exec`); this port serves only synchronous callbacks — a Promise-returning callback is not awaited, that call falls back to the built-in provider and warns once. `options.signal` is a never-aborted signal. `ctx.ui.addAutocompleteProvider` is inert in print / RPC runs (no editor). See [`docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`](../../../docs/LUM1448_AUTOCOMPLETE_PROVIDER.md). |
| Provider registration                   | ✅ Subset      | `pi.registerProvider(name, config)` registers the declarative form (`baseUrl` / `apiKey` / `api` / `models`) **and** — since LUM-1199 — the native `pi.registerProvider(provider)` object overload (`id` / `name` / `baseUrl` / `apiKey` / `api` / `getModels()`), plus the `oauth` block and a `streamSimple(model, context, options)` handler; `pi.unregisterProvider(name)` removes either form. The application layer turns each entry into a `ProviderRouter` adapter plus model-catalog entries, so the agent can stream from the extension's provider and the `/model` selector lists its models. Supported `api` values for a **handler-less** registration: `anthropic-messages`, `openai-responses`, `openai-completions`, `google-generative-ai` (anything else throws with the supported set); a registration that brings `streamSimple` may name any family, because the extension speaks that wire protocol itself. Credentials resolve `stored credential → declared apiKey → (empty)`: a stored api-key credential or the access token of a stored OAuth credential wins over the declaration (the `oauth.getApiKey` equivalent the port can honour today). ⚠️ Divergences: `cost` on a model has no `pi_protocol::Model` field and is dropped; the `streamSimple` bridge drains the handler before returning, so events arrive in order but not incrementally and mid-stream abort is not observed (the pre-start `signal` is honoured); the `oauth` block's `login` / `refreshToken` callbacks are recorded but not driven host-side, so an expired stored token is not refreshed; `headers` / `authHeader` / `refreshModels` and the `!command` apiKey form (never executed) remain unsupported. The extension-side `@earendil-works/pi-ai/compat` registry is unchanged (LUM-1180). |

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
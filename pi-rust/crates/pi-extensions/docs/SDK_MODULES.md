# `@earendil-works/*` SDK virtual modules

Upstream pi extensions do not only import Node builtins — they import
pi's own SDK packages, which hold the TUI components and the
coding-agent helpers:

```
@earendil-works/pi-tui            Text / Box / Container / SelectList / …
@earendil-works/pi-coding-agent   defineTool / getAgentDir / parseFrontmatter / …
@earendil-works/pi-ai             Type / StringEnum / uuidv7 / calculateCost / …
@earendil-works/pi-agent-core     (type-only in the upstream examples)
@earendil-works/pi-ai/compat      provider registry + event stream + builtin apis
@earendil-works/gondolin          third-party sandbox VM (unbridged)
```

The Rust port embeds QuickJS and has no `node_modules`; the shim ships
these packages as **virtual modules** so the same specifiers resolve.
This document is the port's counterpart to
[`docs/NODE_BUILTINS.md`](NODE_BUILTINS.md): it lists what is
implemented, what throws, and every deliberate divergence.

## How the bridge works

```
extension ESM import                    shim                           host (Rust)
─────────────────────────────────────────────────────────────────────────────────────
import { Text } from            __pi_import("@earendil-works/pi-tui")        —
  "@earendil-works/pi-tui";  ──▶ returns the module's Proxy            —
new Text("hi").render(10)       Text is a real class in the shim      —
getAgentDir()                   __pi_os_module.homedir() ──────────▶ host_node_call("os.homedir")
```

Unlike `node:*`, most SDK modules are **pure JS**: the components and
helpers live in the shim itself and need no host import. Three families
cross into Rust: `getAgentDir()` (reads `os.homedir()` through
`host_node_call`), the built-in tool factories (`createReadTool` …),
which run a built-in tool through the `host_builtin_tool_definition` /
`host_builtin_tool` bridge described under
[`@earendil-works/pi-coding-agent`](#earendil-workspi-coding-agent--helpers-bridged-tool-factories-bridged),
and the two built-in model adapters behind `@earendil-works/pi-ai/compat`,
which stream provider events through `host_pi_ai_stream_start` /
`host_pi_ai_stream_next` / `host_pi_ai_stream_cancel` (see
[`@earendil-works/pi-ai/compat`](#earendil-workspi-ai-compat--provider-registry--builtin-apis)).

### Every specifier is registered under three names

The packages were once published as `@mariozechner/*`, and some
extensions import the bare package name. All three spellings resolve to
the same module object and the same manifest entry:

```js
"@earendil-works/pi-tui" === "@mariozechner/pi-tui" === "pi-tui"  // same object
```

### A missing name never evaluates to `undefined`

Every module is a `Proxy` with an explicit inventory. This is a hard
rule: silently returning `undefined` turns a missing bridge into a
confusing `TypeError` deep inside extension code, minutes later.

| Access                        | Result                                                        |
|-------------------------------|---------------------------------------------------------------|
| Implemented name              | the real value                                                |
| Documented upstream gap       | throws `ERR_PI_SDK_UNIMPLEMENTED`, message names the specifier, the export and this doc |
| Any other name                | throws `ERR_PI_SDK_UNKNOWN_EXPORT` ("has no export")          |
| `default`                     | the module itself (ESM/CJS interop)                           |
| `then` / `toJSON` / `valueOf` / `__esModule` / symbols | `undefined` (JS-internal protocol names, so `await import()` and `JSON.stringify` keep working) |
| `Object.keys(mod)` / `"name" in mod` | the implemented names (`default` included), matching the underlying object; a documented gap is readable and throws, but is not reported as an export |

`import type { … }` lines are erased by the loader, so type-only names
(`ExtensionAPI`, `Component`, `Theme`, `SettingItem`, …) need no runtime
binding at all. A *value* import of a real upstream export the shim does
not implement is listed as a documented gap below, not as "unknown".

### The inventory is self-describing

The shim exposes the machine-readable inventory it is built from:

```js
globalThis.__pi_sdk_manifest() // JSON: { "<specifier>": { implemented: [...], unimplemented: [...] } }
```

`tests/sdk_modules.rs` reads that JSON at runtime (instead of restating
the lists) and scans `packages/coding-agent/examples/extensions` for
value imports from these packages: every imported name must be either
implemented or a documented gap, so a new upstream import fails the
test instead of silently outrunning the bridge.

## Specifier inventory

### `@earendil-works/pi-tui` — fully bridged

The component classes implement the constructor surface upstream code
uses and `render(width)` returns terminal strings. The host does not run
its own render loop: a component handed to `ctx.ui.custom()` /
`setWidget` / `setHeader` / `setFooter` / `setEditorComponent` is rendered
by the TUI instead, so a component built and used purely inside an
extension still has to be driven by one of those calls — see
[`ctx.ui`](#ctxiu-divergences).

| Export | Notes |
|---|---|
| `Text`, `Spacer`, `Box`, `Container` | Real layout: `paddingX`/`paddingY`, `render(width)`, `addChild` / `removeChild` / `clear` / `invalidate`. Output lines are padded to the requested width. |
| `Markdown` | Line-oriented subset: headings, lists, quotes, fenced code, inline `**bold**` / `*italic*` / `` `code` `` / links. |
| `Input`, `Editor`, `CustomEditor` | Value + `handleInput` state machines (`Enter` submit, backspace, printable text). `CustomEditor extends Editor` and exposes `actionHandlers` (a `Map`) plus `setWorkingStatusIndicator`. |
| `Loader`, `CancellableLoader` (via `BorderedLoader`), `BorderedLoader` | `BorderedLoader extends Container`, honours `{ cancellable }` and exposes an `AbortSignal`. |
| `SelectList`, `SettingsList` | Filtering, selection, `Up`/`Down`/`j`/`k`, `SettingItem` cycling through `values`. |
| `DynamicBorder` | `render(width)` returns a `-` rule of the requested width. |
| `visibleWidth`, `truncateToWidth`, `wrapTextWithAnsi`, `stripTerminalSequences`, `hyperlink`, `CURSOR_MARKER` | ANSI-aware geometry helpers. |
| `Key`, `parseKey`, `matchesKey`, `isKeyRelease`, `isKeyRepeat` | Legacy + modified-key parsing, Kitty release/repeat detection. |
| `fuzzyMatch`, `fuzzyFilter` | Subsequence scoring with word-boundary and consecutive-run bonuses. |
| `getMarkdownTheme`, `getSelectListTheme`, `getSettingsListTheme` | **Shim extras** (upstream keeps these in `pi-coding-agent`); identity themes. |

Divergences from upstream `pi-tui`:

* **No live terminal.** Components render to strings but are never
  mounted; `render(width)` is pure. There is no `TUI`, no `Terminal`, no
  focus/overlay manager.
* **No colour.** The theme getters return identity functions (and
  `"> "` for the settings cursor), because the host has no palette or
  TTY. Passing a real theme object still works — components only call
  the documented functions. For the same reason `ctx.ui.theme` is an
  identity object (`fg`/`bg`/`bold`/`italic`/`underline`/`inverse`/
  `strikethrough` return their text; `getFgAnsi`/`getBgAnsi` return `""`).
* **Width is a range-table approximation.** `visibleWidth` uses a
  wide-character range table, not full East-Asian-Width + grapheme
  segmentation: combining marks and emoji ZWJ sequences are
  approximated.
* **`wrapTextWithAnsi` does not re-open ANSI styles** across wrapped
  lines, and `truncateToWidth` may cut in the middle of an escape
  sequence. Both keep plain (unstyled) text exact.
* **`truncateToWidth`'s default ellipsis is `"..."`**, matching upstream,
  not the single-character `"…"`.
* **`matchesKey` does not parse the Kitty CSI-u form**
  (`\x1b[97;1:3u`): it compares `parseKey(data)` against the key id, with
  the `escape`↔`esc` and `return`↔`enter` aliases. `isKeyRelease` /
  `isKeyRepeat` *do* understand the `:3`/`:2` Kitty suffixes.

### `@earendil-works/pi-coding-agent` — helpers bridged, tool factories bridged

| Export | Notes |
|---|---|
| `defineTool` | Identity, exactly like upstream: it exists to carry the generic type in TS. |
| `CONFIG_DIR_NAME` | `".pi"`. |
| `DEFAULT_MAX_LINES` / `DEFAULT_MAX_BYTES` | `2000` / `51200` (50 KB). |
| `VERSION` | `"0.85.1-pi-rust"` — the upstream package version this port tracks, with a port suffix. |
| `getAgentDir` | `$PI_CODING_AGENT_DIR` when set (tilde-expanded), else `~/.pi/agent`. |
| `formatSize`, `truncateHead`, `truncateLine` | Byte/line truncation primitives, including the full `TruncationResult` shape (`truncatedBy`, `firstLineExceedsLimit`, `outputLines`, …). |
| `parseFrontmatter`, `stripFrontmatter` | `{ frontmatter, body }` with upstream's delimiter and newline/BOM normalisation. |
| `withFileMutationQueue` | Serialises `fn()` per resolved file path. |
| `convertToLlm`, `serializeConversation` | Message conversion and the summarisation text format, including the compaction / branch summary prefixes. |
| `createReadTool`, `createWriteTool`, `createEditTool`, `createBashTool`, `createFindTool`, `createGrepTool`, `createLsTool` | Factories for the host's built-in tools (`createXTool(cwd?)`). `parameters` is the exact schema the Rust executor coerces arguments against, and `execute` runs the *same* built-in the model calls, so an extension can re-register a built-in with custom rendering but the original behaviour (upstream's `built-in-tool-renderer.ts`). |
| `BorderedLoader`, `CustomEditor`, `DynamicBorder`, `getEditorTheme`, `getMarkdownTheme`, `getSelectListTheme`, `getSettingsListTheme` | Re-exported from the shim's component set. |

Documented gaps (throw `ERR_PI_SDK_UNIMPLEMENTED`): none — every export
the upstream examples import from this package is bridged.

Tool factories:

* `createXTool(cwd?)` takes the working directory the tool should resolve
  relative paths against. The host rebases `path` to an absolute path for
  `read` / `write` / `edit` and sets `bash`'s `cwd`, so a relative path in
  the tool call resolves against `cwd` as it does upstream.
* The navigation tools (`find` / `grep` / `ls`) take an optional `path`
  and reject absolute paths in the Rust port, so they stay anchored at the
  pi process cwd; `cwd` is not applied to them.
* `execute` resolves to `{ content, details, isError }` — the same shape
  `pi.registerTool` returns — with `isError` set for a tool that failed.
  It *rejects* only when the bridge is unusable (no runner installed,
  unknown tool name, malformed host reply), and then with
  `ERR_PI_BUILTIN_TOOL`.
* A factory still returns a usable object when no runner is installed
  (permissive `{ "type": "object" }` schema); only `execute` rejects. This
  keeps an extension that merely *builds* a tool loadable in a host that
  has no built-in bundle.
* Upstream's `createBashTool(cwd, { spawnHook })` second argument is
  accepted and ignored: the Rust `BashTool` has no spawn hook.
* The tool-call id the extension passes to `execute(toolCallId, params, …)`
  is not forwarded across the bridge (the built-in result gets a synthetic
  id); only the re-registering extension can observe it.

Divergences:

* **`parseFrontmatter` parses a YAML subset.** Upstream uses the `yaml`
  package; the shim handles flat `key: value`, one level of indentation,
  scalars, quotes and inline `[a, b]` / `{a: b}` collections. Anchors,
  block scalars (`|`/`>`), multi-line flow collections and trailing
  comments are left as strings.
* **`withFileMutationQueue` keys on `path.resolve(expandHome(path))`**
  rather than upstream's inode-aware key; the observable contract
  (mutations to the same path run in order) is unchanged.

### `@earendil-works/pi-ai` — partial

Implemented: `Type` (the same TypeBox `Type` object as the `typebox`
virtual module), `StringEnum(values, options)`, `uuidv7(timestampMs?)`,
`calculateCost(model, usage)`, `contentText(content, separator?)`, and the
event-stream trio `EventStream`, `AssistantMessageEventStream`,
`createAssistantMessageEventStream()`.

Divergences:

* **`uuidv7` uses `host_node_call("crypto.randomBytes")`** instead of
  `globalThis.crypto.getRandomValues` (QuickJS has no WebCrypto). The
  layout, monotonic clock and in-millisecond sequence match RFC 9562
  Appendix A.4 / upstream.
* **`Type` is re-exported, not a second TypeBox build** — `Type` and the
  `typebox` / `@sinclair/typebox` virtual modules are the same object, so
  schemas built from either compare equal.
* **The event stream is a verbatim port of
  `packages/ai/src/utils/event-stream.ts`.** It is pure JS (a FIFO queue
  behind an `AsyncIterable` plus the promise the terminal event resolves),
  so it needs no host bridge; the async iterator is hand-rolled rather
  than an `async function*`, matching the rest of the shim.

### `@earendil-works/pi-ai/compat` — provider registry + builtin apis

Upstream `compat.ts` keeps a module-level `Map<Api, RegisteredApiProvider>`
so an extension can plug its own streaming implementation in; that is
exactly what the `custom-provider-*` examples do. The registry, the
`AssistantMessageEventStream` factory and the two built-in provider
factories the Rust port has adapters for are **implemented**:

| Export | Notes |
|---|---|
| `registerApiProvider(provider, sourceId?)` | Stores `{api, stream, streamSimple}` keyed by `api`, wrapping both functions with upstream's `Mismatched api` guard. |
| `unregisterApiProviders(sourceId)` / `getApiProvider(api)` / `getApiProviders()` | Registry maintenance, same shapes as upstream. |
| `stream` / `streamSimple` / `complete` / `completeSimple` | Resolve the registered provider for `model.api`, stream, and (for `complete*`) await `result()`. |
| `createAssistantMessageEventStream()` | Same factory as `@earendil-works/pi-ai`. |
| `anthropicMessagesApi()` / `openAIResponsesApi()` | Return a `ProviderStreams` whose `stream` / `streamSimple` run the host's `AnthropicProvider` / `OpenAiResponsesProvider` (LUM-1180). |
| `registerBuiltInApiProviders()` | Called at module init: registers the two bridged apis **without clobbering** an entry an extension already registered for the same api id. |
| `resetApiProviders()` | Clears the whole registry (extension overrides included) and re-registers the builtins. |

#### The built-in provider streaming bridge

The two factories normally `import()` a TypeScript provider module that
talks to the network. The shim cannot, and `pi-extensions` cannot depend
on `pi-ai`, so the embedding crate injects a **stream runner** into the
host and the traffic crosses three host imports:

| Host import | Contract |
|---|---|
| `host_pi_ai_stream_start(requestJson)` | `{api, model, context, options}` → `Promise<{ok:true, id}>` or `Promise<{ok:false, error}>`. The `options.signal` is stripped before serialising; the host owns cancellation. |
| `host_pi_ai_stream_next(id)` | `Promise<{ok:true, done:false, event}>` for one upstream-shaped `AssistantMessageEvent`, `{ok:true, done:true}` when the stream ended, `{ok:false, error}` for an unknown id. Never rejects. |
| `host_pi_ai_stream_cancel(id)` | Aborts the request and drops the host-side stream (and its connection). A no-op for an unknown or finished `id`. |

`anthropicMessagesApi().streamSimple(model, context, options)` is
**synchronous** — it hands back an `AssistantMessageEventStream` and drives
the host behind it, mirroring upstream's `lazyStream`. The events land in
the same pure-JS queue as `@earendil-works/pi-ai`, so `for await` and
`await stream.result()` behave exactly like upstream, and `complete()` /
`completeSimple()` fold the same stream into an `AssistantMessage`. A
stream that fails to start, a transport error, and an `AbortSignal` abort
all terminate the stream with an `error` event (`reason` `error` /
`aborted`) carrying a whole `AssistantMessage` — never a thrown exception.

A live stream raises the per-call host deadline to 10 minutes (the default
is 5 seconds), because a single extension tool call can drive a whole model
turn; `AbortSignal` remains the normal way to stop earlier.

Without a runner (a host that is not `pi-coding-agent`) the factories
still import and return a stream that terminates with an error event
naming the missing runner.

Documented gaps still need a host adapter for that api family:
`azureOpenAIResponsesApi`, `bedrockConverseStreamApi`,
`googleGenerativeAIApi`, `googleVertexApi`, `mistralConversationsApi`,
`openAICodexResponsesApi`, `openAICompletionsApi`, `piMessagesApi`. The
module resolves, so an extension that imports a gap only fails when it
actually reaches for that factory.

Divergences:

* **No builtin provider catalogue.** Upstream `stream`/`streamSimple` first
  look the model up in the builtin catalogue and route cloudflare models
  through `Models`; the shim has neither, so they go straight to the
  registry and throw `No API provider registered for api: …` when the
  extension has not registered one. The two bridged apis are the
  exception: `registerBuiltInApiProviders()` runs at module init, so
  `stream` / `complete` reach them even when the extension imports nothing
  but those.
* **No env API-key injection.** Upstream's `withEnvApiKey` fills in
  `options.apiKey` from the provider environment; the shim has no
  `getEnvApiKey` bridge, so the caller must pass `apiKey` explicitly. The
  *host* runner still falls back to the provider's env vars
  (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …) when `apiKey` is absent.
* **Custom `options.headers` are not forwarded.** The Rust Anthropic /
  OpenAI-Responses adapters take only a credential and a base URL, so a
  header-based credential (the upstream `custom-provider-gitlab-duo`
  example passes `apiKey: "gitlab-duo"` plus an `Authorization` header)
  does not reach the request yet. `apiKey`, `baseUrl`,
  `temperature` and `maxTokens` are forwarded.
* **`Done` carries no thinking blocks.** The Rust `Done` event's
  `content` has no thinking variant, so streamed
  `thinking_delta` events reach the extension but the final
  `AssistantMessage` only keeps text and tool calls.
* **Cost is not carried.** The host's `Done` usage has no `cost` field;
  the shim leaves the zeroed `cost` object in place. Use
  `calculateCost(model, usage)` from `@earendil-works/pi-ai` if the
  extension needs it.

#### `pi.registerProvider` — ExtensionAPI bridge (slices 1–2)

The extension-facing `pi.registerProvider(name, config)` /
`pi.registerProvider(providerObject)` / `pi.unregisterProvider(name)`
methods are **implemented** for both overloads. They are a distinct path
from the `compat` registry above: the registration lands in the host
(`host_register_provider`, validated), and `pi-coding-agent` turns it into
a `ProviderRouter` adapter plus model-catalog entries, so the agent itself
can stream from the extension's provider.

Supported fields (string overload): `name`, `baseUrl`, `apiKey` (literal
or `$VAR` / `${VAR}`), `api`, `models`, `streamSimple`, `oauth`. The object
overload takes upstream's `Provider` shape: `id` (or `name`), `name`,
`baseUrl`, `apiKey`, `api` (defaulting to `getModels()[0].api`),
`getModels()` / `models`, `streamSimple`, `oauth` (or
`auth.oauth`). `api` must be one of `anthropic-messages`,
`openai-responses`, `openai-completions`, `google-generative-ai` for a
registration **without** `streamSimple`; with a handler any family string
is accepted, because the extension speaks that wire protocol itself. A
`models` array without `api` and an unknown `api` both throw with the
supported set in the message. A `baseUrl`-only call is accepted as an
override of a built-in provider.

`streamSimple(model, context, options)` is answered by the host import
`_pi_provider_stream_simple(name, modelJson, contextJson, optionsJson)`,
which `pi-coding-agent`'s `ExtensionStreamFn` adapter calls on every
request for that provider. The Rust `Model` / `Context` /
`SimpleStreamOptions` triple is serialised into upstream's shapes
(`baseUrl` / `contextWindow` / `maxTokens`; `systemPrompt` / `messages` /
`tools`; `temperature` / `maxTokens` / `apiKey` / `baseUrl`), the
handler's `AssistantMessageEventStream` is drained, and the events are
decoded back into `pi_protocol::AssistantMessageEvent`. `options.apiKey`
is filled in by the host, because the handler owns the HTTP request and
upstream's own `streamSimple` implementations read exactly that field.

Credential resolution for an extension provider is **stored credential →
declared `apiKey` → (empty)**, which is
`pi_ai::resolve_provider_auth`'s "a stored credential owns the provider"
rule applied to a provider whose only host-side handler is api-key: a
stored api-key credential, or the access token of a stored OAuth
credential (`oauth.getApiKey`'s equivalent), wins over the declaration.

Documented divergences from upstream `ProviderConfig` / `Provider`:

* **`streamSimple` delivery is buffered.** The host drains the handler's
  async iterator in one QuickJS call before returning, so events keep
  their order but arrive in a batch rather than as they are produced; a
  mid-stream `AbortSignal` is not observed. A `signal` already cancelled
  when the request starts short-circuits to `aborted` +
  `done{aborted}`, and the host's per-call deadline (10 minutes for a
  provider stream) bounds a handler that never returns.
* **A failing handler fails the stream in-band.** The host call's
  rejection, a `{ok:false}` envelope and a handler that never emits a
  terminal event all become an `error` event followed by
  `done{reason:"error"}`, matching the `StreamFn` contract that request
  failures are never returned as `Err`.
* **`cost` is not carried.** `pi_protocol::Model` has no cost field, so a
  model entry's `cost` is dropped from the catalog; usage keeps a zeroed
  `cost` object, as in the `compat` divergence above.
* **Thinking blocks are dropped from `done`.** `pi_protocol::Content` has
  no thinking variant, so `thinking_delta` events reach the client but the
  terminal `Done` keeps only text and tool calls.
* **The `oauth` block is declared, not driven.** `login` / `refreshToken`
  / `getApiKey` / `modifyModels` stay in the shim and the host records
  their presence as flags; nothing calls them yet, so an expired stored
  token is not refreshed and `/login` does not exist for extension
  providers.
* **No `headers` / `authHeader` / `refreshModels`.** Same reason as the
  compat divergence above: the Rust adapters take only a credential and
  a base URL, and a handler-owned provider gets `apiKey` / `baseUrl`
  rather than a header map.
* **`!command` apiKey is never executed.** The host stores the string but
  the application layer rejects it, logs a warning and skips the
  provider; use a literal or `$VAR`.
* **A key that does not resolve skips the provider.** Matching the
  built-in router's "unconfigured provider is absent, not broken" rule,
  a `$VAR` that is unset warns and leaves the provider unregistered.
* **`streamSimple` needs a host.** A `RegisteredProviders` snapshot with
  no host attached (embedding code that only reads declarations) skips
  such a provider with a warning instead of registering a broken adapter.
* **`unregisterProvider` is a host-registry no-op after load.** It
  removes the registration from the host; a live router adapter / catalog
  entry is removed through `ProviderRouter::unregister_provider`, which
  the CLI does not yet call from an event handler.

### `@earendil-works/pi-agent-core` — resolves, no runtime exports

Every value import from this package in the upstream examples is
type-only (`import type { AgentMessage, … }`), which the loader erases.
The module intentionally has no runtime exports; it exists so the
specifier resolves if an extension imports it for side effects.

### `@earendil-works/gondolin` — known unbridged third party

`gondolin` is a third-party sandbox VM (`RealFSProvider`, `VM`), not part
of the pi SDK. It is registered as a documented gap so importing it fails
with a clear message instead of "unsupported import". Extensions that
actually need the sandbox cannot run in the Rust port yet; use the
upstream Node runtime for them.

## `ctx.ui` divergences

Loading is unaffected by these — they only matter when an extension
*calls* the method:

| Method | Behaviour |
|---|---|
| `notify`, `confirm`, `input`, `select` | As before (see [`EXTENSIONS.md`](EXTENSIONS.md#ui-requests)). |
| `theme` | Identity styling object (no colour). |
| `custom(factory, options?)` | Returns a thenable `CustomHandle` (`resolve` / `close` / `done` / `isVisible` / `setVisible`). The factory's component is rendered by the TUI overlay (`options.overlay`) or replaces the editor region, until the handle is resolved. The factory may be `async`; a handle resolved before the factory's promise settles never opens a session. Without a region host the handle resolves `undefined` on a microtask and the denial is reported once, exactly like `confirm` in print mode. |
| `setWidget`, `setHeader`, `setFooter`, `setEditorComponent` | Forwarded to the injected `UiRegionHost` as one synchronous `host_ui_region(op, json)` call each (`clear` for `null` / `undefined`). Each component is registered in the shim and re-rendered by the host with the live terminal width; `handleInput(data)` receives raw key data. Without a region host the call is reported as a denial and nothing is queued. |
| `editor(title, initial?)` | Returns `null` and reports the denial, like `input` in a non-interactive run. |
| `setStatus`, `setTitle`, `setEditorText`, `setHiddenThinkingLabel`, `setWorkingIndicator`, `setWorkingVisible`, `setWorkingMessage`, `addAutocompleteProvider`, `setTheme` | No-op with a one-time warning notification. Accepting the call lets extensions that configure the status line at `session_start` load; the value is inert because there is no channel for it. |

## Adding a new SDK export

1. Implement it in the SDK section of
   `runtime/pi-ext-shim.mjs` (before the `__pi_virtual_modules` map) and
   add its name to the right `__pi_sdk_module(...)` call — implemented
   names in the second argument, documented gaps in the third.
2. Extend `tests/sdk_modules.rs` if the new export is reachable from an
   example.
3. Update the tables above. A name the shim does not implement must never
   be silently omitted: add it as a gap with a reason, or leave it
   unimplemented so the "has no export" error fires.

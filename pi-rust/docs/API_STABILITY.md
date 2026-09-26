# pi-rust extension API stability

This document is the source of truth for the extension API surface that
pi-rust ships. Every name listed here is implemented and exercised by at
least one test. Anything not on the list should be treated as
"experimental, may move", and an extension that depends on it must
declare the dependency in `required_api` so the loader can fail fast
when the implementation moves.

## Stability tiers

| Tier | Meaning | Manifest contract |
| --- | --- | --- |
| **Stable** | Backed by tests; behaviour matches the TS reference. | Available without flag. |
| **Experimental** | Backed by tests, but the upstream TS API is itself `@experimental`. | Available without flag, but listed under `experimental` in this doc. |
| **Planned** | On the parity roadmap; not yet implemented in this build. | Must be declared in `required_api`. The loader warns but does not refuse. |
| **Missing** | Tracked upstream but deliberately deferred. | Same as *Planned*. |

The list below is the **Stable** tier; *Experimental* items are marked
with `(experimental)`. *Planned* / *Missing* items are not listed here
because they are not part of the surface an extension can rely on.

The authoritative enum is
[`STABLE_API`](../../crates/pi-coding-agent/src/extensions/api_surface.rs)
in the source tree. When a new method lands, add its string identifier
there **and** mirror it on this page; the CI diff check fails if the
two diverge.

## Lifecycle events (ExtensionEvent)

Extensions subscribe via `pi.on(name, handler)`. Names are the
`ExtensionEvent::name()` wire tags — the same strings the mapper uses
when translating from `AgentEvent`.

| Name | Since | Notes |
| --- | --- | --- |
| `agent_start` | v0.1 | Fires once at the start of a session. |
| `agent_end` | v0.1 | Fires once at session end with the message log. |
| `turn_start` | v0.1 | Counter-based; first turn is `turn_index: 0`. |
| `turn_end` | v0.1 | Carries the assistant message + every tool result. |
| `message_start` | v0.1 | Empty assistant message shell. |
| `message_update` | v0.1 | Streaming deltas, one per `text_delta` / `tool_call_delta`. |
| `message_end` | v0.1 | Final assistant message. |
| `tool_execution_start` | v0.1 | Delivered *after* the `tool_call` hook returns its decision. |
| `tool_execution_update` | v0.1 | Per-delta progress from `ToolExecutor::on_update`. |
| `tool_execution_end` | v0.1 | Final result, with `is_error` post-hook. |
| `user_message` | v0.1 | Also delivered as `input` (upstream parity). |
| `input` | v0.1 | Source = `Interactive` today. |
| `session_start` | v0.1 | First event the JS host sees. |
| `session_shutdown` | v0.1 | Triggered before the runtime goes away. |
| `session_before_switch` | v0.1 | Cancelable. |
| `session_before_fork` | v0.1 | Cancelable. |
| `session_before_compact` | v0.1 | Cancelable. |
| `session_compact` | v0.1 | Post-compaction summary. |
| `session_compact_failed` | v0.1 | Compaction refused by an extension. |
| `session_before_tree` | v0.1 | Cancelable; can rewrite summary / instructions. |
| `session_tree` | v0.1 | Tree built. |
| `session_info_changed` | v0.1 | Name / cwd / leaf updated. |
| `tool_call` | v0.1 | Hookable; precedes `tool_execution_start`. |
| `tool_result` | v0.1 | Hookable; follows `tool_execution_end`. |
| `session_fork` | v0.1 | Post-fork observability (new + parent session id). |
| `resources_discover` | v0.1 | Extension asked to advertise extra skill / prompt / theme paths. |
| `model_select` | v0.1 | Model switch (set / cycle / restore). |
| `thinking_level_select` | v0.1 | Thinking-level change. |
| `user_bash` | v0.1 | `!`-prefixed shell invocation. |
| `project_trust` | v0.1 | Trust tri-state decision surfaced to extensions. |
| `ui_prompt_start` | v0.1 | Blocking UI prompt opened (kind: `select / confirm / input / editor / custom`). |
| `ui_prompt_end` | v0.1 | Blocking UI prompt closed. |
| `provider_stream` (experimental) | v0.85.x | Opaque provider event payload; no AgentEvent source yet. |
| `cache_warming_decision` (experimental) | v0.85.x | Cache-warmer asks the extension to warm the prompt cache. |

## UI methods (ExtensionUIContext)

The Rust `TuiUiHandler` implements the same surface upstream's
`ExtensionUIContext` exposes. Strings are the JS shim's `pi.ui.*` keys.

| Method | Since | Notes |
| --- | --- | --- |
| `ui.select` | v0.1 | Dialog. |
| `ui.confirm` | v0.1 | Dialog. |
| `ui.input` | v0.1 | Dialog. |
| `ui.notify` | v0.1 | Dialog. |
| `ui.setWidget` | v0.1 | Component-slot replacement. |
| `ui.setFooter` | v0.1 | Component-slot replacement. |
| `ui.setHeader` | v0.1 | Component-slot replacement. |
| `ui.setStatus` | v0.1 | Status bar text. |
| `ui.setWorkingMessage` | v0.1 | Streaming message. |
| `ui.setWorkingVisible` | v0.1 | Stream loader row. |
| `ui.setWorkingIndicator` | v0.1 | Spinner frames. |
| `ui.setHiddenThinkingLabel` | v0.1 | Hidden-thinking block label. |
| `ui.setEditorComponent` | v0.1 | Editor factory. |
| `ui.setEditorText` | v0.1 | Editor content. |
| `ui.pasteToEditor` | v0.1 | Paste into editor. |
| `ui.editor` | v0.1 | Multi-line editor dialog. |
| `ui.addAutocompleteProvider` | v0.1 | Provider factory. |
| `ui.registerShortcut` | v0.1 | Reserved-key conflict detection (19 reserved). |
| `ui.registerMessageRenderer` | v0.1 | Per-role renderer. |
| `ui.registerEntryRenderer` | v0.1 | Per-`customType` renderer. |
| `ui.registerMarkdownTransformer` | v0.1 | AST transformer. |
| `ui.registerFlag` | v0.1 | CLI flag registration. |
| `ui.theme` / `ui.setTheme` | v0.1 | Active theme. |
| `ui.getTheme` / `ui.getAllThemes` | v0.1 | Theme query. |
| `ui.getEditorText` | v0.1 | Editor text read. |
| `ui.getEditorComponent` | v0.1 | Editor factory query. |
| `ui.setToolsExpanded` / `ui.getToolsExpanded` | v0.1 | Tool block fold. |
| `ui.onTerminalInput` | v0.1 | Terminal input interception. |
| `ui.setTitle` | v0.1 | TUI title. |
| `ui.custom` | v0.1 | Custom overlay. |

## Top-level API methods (ExtensionAPI)

Methods that live directly on the extension context (not under `ui.`).
Strings are the snake_case manifest names; the Rust `TuiUiHandler` /
`ExtensionAPI` impls surface them under those names.

| Method | Since | Notes |
| --- | --- | --- |
| `abort` | v0.85.x | Stop the active turn / tool batch. |
| `appendEntry` / `append_entry` | v0.85.x | Append a custom-message entry without going through the model. |
| `compact` | v0.85.x | Trigger a manual context compaction. |
| `fork` | v0.85.x | Fork the session at the given entry. |
| `navigateTree` / `navigate_tree` | v0.85.x | Jump to a leaf in the session tree. |
| `newSession` / `new_session` | v0.85.x | Start a brand-new session. |
| `switchSession` / `switch_session` | v0.85.x | Switch to a stored session. |
| `sendMessage` / `send_message` | v0.85.x | Inject a message as if from the user. |
| `sendUserMessage` / `send_user_message` | v0.85.x | Like `sendMessage`, but routes through the user-prompt path. |
| `followUp` / `follow_up` | v0.85.x | Queue a follow-up prompt for after the current turn. |
| `getActiveTools` / `get_active_tools` | v0.85.x | Active tool allowlist. |
| `getAllTools` / `get_all_tools` | v0.85.x | All registered tools (built-in + extension). |
| `setActiveTools` / `set_active_tools` | v0.85.x | Replace the active tool allowlist. |
| `getModel` / `get_model` | v0.85.x | Current model id. |
| `setModel` / `set_model` | v0.85.x | Switch model. |
| `getThinkingLevel` / `get_thinking_level` | v0.85.x | Current thinking level. |
| `setThinkingLevel` / `set_thinking_level` | v0.85.x | Change thinking level. |
| `getSessionName` / `get_session_name` | v0.85.x | Current session name. |
| `setSessionName` / `set_session_name` | v0.85.x | Set the session name. |
| `getFlag` / `get_flag` | v0.85.x | Read an experimental flag. |
| `isStreaming` / `is_streaming` | v0.85.x | True while a turn is streaming. |
| `waitForIdle` / `wait_for_idle` | v0.85.x | Resolves when no turn / queue is in flight. |
| `registerTool` / `register_tool` | v0.85.x | Register a custom `ToolDefinition`. |

## Lifecycle hooks (ExtensionLifecycleHooks)

Async hooks extensions install on the agent loop.

| Hook | Since | Notes |
| --- | --- | --- |
| `lifecycle.beforeAgentStart` | v0.1 | Chainable `systemPrompt` append. |
| `lifecycle.context` | v0.1 | Replace message list. |
| `lifecycle.beforeProviderRequest` | v0.1 | Mutate outbound payload. |
| `lifecycle.beforeProviderHeaders` | v0.1 | Mutate outbound headers. |
| `lifecycle.afterProviderResponse` | v0.1 | Observe post-response state. |
| `lifecycle.agentSettled` | v0.1 | Fires after every successful turn. |

## Component slots (P0-1)

| Slot | Since | Notes |
| --- | --- | --- |
| `componentSlot.register` | v0.1 | Add a custom `ComponentSlot` for the dialog / footer / header. |

## Tool renderers (P2-3)

| API | Since | Notes |
| --- | --- | --- |
| `toolRenderer.register` | v0.1 | Extension-supplied `renderCall` / `renderResult` shadows the built-in default. |

## Manifest contract

```json
{
    "name": "my-extension",
    "required_api": ["ui.setWidget", "registerShortcut", "turn_start"]
}
```

`required_api` is **optional**: an extension without it loads against
whatever surface the current build provides. With it, the loader
fails fast on the first unknown name (see
[`validate_required_api`](../../crates/pi-coding-agent/src/extensions/required_api.rs))
and surfaces the missing entries in a clear error so the plugin
author can adjust.

The loader treats `required_api` as a **hard** constraint — extensions
that depend on missing APIs do not silently degrade.

## CI monthly diff

`.github/workflows/api_stability_diff.yml` runs on the first of every
month, diffs this document + `STABLE_API` against the upstream TS
source, and opens an issue when entries move. The workflow is
deliberately best-effort: a missing entry is informational, a
*removed* entry is a release-blocker and requires a release note.
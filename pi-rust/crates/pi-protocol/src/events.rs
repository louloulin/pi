//! Agent + extension event surface.
//!
//! Mirrors the typed event union in `packages/agent/src/types.ts` and the
//! extension event surface in `packages/coding-agent/src/extensions/types.ts`.

use serde::{Deserialize, Serialize};

use crate::{Content, Message, ToolResult, Usage};

/// Final stop reason of an assistant turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model emitted text/tool calls and stopped naturally.
    Stop,
    /// Model requested a tool invocation; the loop will execute it.
    ToolUse,
    /// Max output tokens hit before a natural stop.
    MaxTokens,
    /// Generation aborted by the user.
    Aborted,
    /// Generation failed with an error message.
    Error,
    /// Model finished its turn with an empty content array.
    Empty,
}

/// Per-token event stream emitted during assistant generation.
///
/// This is the Rust analogue of `AssistantMessageEvent` in
/// `packages/ai/src/types.ts`. The full set of variants will be filled in
/// during Stage 1 alongside the provider ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    /// Generation started.
    Start {
        /// Model identifier.
        model: String,
    },
    /// A text delta arrived.
    TextDelta {
        /// The new text.
        delta: String,
    },
    /// A reasoning/thinking delta arrived.
    ThinkingDelta {
        /// The new thinking text.
        delta: String,
    },
    /// A tool call arrived (possibly partial).
    ToolCallDelta {
        /// Tool call index.
        index: u32,
        /// Provider-issued identifier (when first seen).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name (when first seen).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Argument fragment.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments_delta: Option<String>,
    },
    /// Generation finished.
    Done {
        /// Final message content.
        content: Vec<Content>,
        /// Stop reason.
        stop_reason: StopReason,
        /// Token usage.
        usage: Usage,
    },
    /// Generation aborted.
    Aborted,
    /// Generation failed.
    Error {
        /// Provider-specific error message.
        message: String,
    },
}

/// Full assistant message returned at the end of a stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// Model identifier.
    pub model: String,
    /// Final content blocks.
    pub content: Vec<Content>,
    /// Stop reason.
    pub stop_reason: StopReason,
    /// Token usage.
    pub usage: Usage,
    /// Provider error text, present when [`AssistantMessage::stop_reason`] is
    /// [`StopReason::Error`] or [`StopReason::Aborted`].
    ///
    /// Mirrors upstream `AssistantMessage.errorMessage`
    /// (`packages/ai/src/types.ts`). Overflow / retry classification reads it
    /// before falling back to scanned content text (see
    /// `pi_ai::is_context_overflow`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Lifecycle event delivered to extensions.
///
/// Two families live in this enum:
///
/// * **Rust-native** variants from Stage 3 (`session_start`,
///   `user_message`, `resources_discover`). Their payloads are Rust
///   shapes and do **not** match upstream's field names.
/// * **Upstream-parity** variants (LUM-1246, extended by LUM-1330 for the
///   tool hooks). Tag *and* payload field names mirror
///   `packages/coding-agent/src/core/extensions/types.ts` — a snake_case
///   `type` tag with camelCase fields — so a TypeScript plugin that reads
///   `event.toolCallId` / `event.assistantMessageEvent` works without an
///   adapter. Field names are pinned with explicit `rename`s rather than a
///   container attribute so the parity is visible at the definition site.
///
/// `ExtensionEvent::name()` returns the wire tag; it is the string a plugin
/// passes to `pi.on(...)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEvent {
    // ------------------------------------------------------------------
    // Upstream parity — tool hooks (LUM-1330) and the Rust-native extras.
    // ------------------------------------------------------------------
    /// Session started.
    SessionStart,
    /// Agent received a user message (Rust-native; upstream has no
    /// `user_message` — see [`ExtensionEvent::Input`]).
    UserMessage {
        /// The user message.
        message: Message,
    },
    /// Upstream `tool_call`: the agent is about to run a tool, and a
    /// handler may block it, ask the loop to stop after this batch, or
    /// patch the arguments in place.
    ///
    /// Payload parity (LUM-1330): `toolCallId` / `toolName` / `input` are
    /// upstream's field names, so a TypeScript plugin that reads
    /// `event.input` sees the same object it sees under pi-ts. Before
    /// LUM-1330 this variant carried the Rust-native `{ call }` shape,
    /// which no upstream plugin could read.
    ToolCall {
        /// Provider-issued tool call id.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Registered tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Parsed tool arguments. A handler mutating `event.input` in
        /// place patches the arguments the executor receives.
        #[serde(rename = "input")]
        input: serde_json::Value,
    },
    /// Upstream `tool_result`: a tool finished, and a handler may replace
    /// the content / `isError` flag / structured details.
    ToolResult {
        /// Provider-issued tool call id.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Registered tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Arguments the call ran with (post-`tool_call` patching).
        #[serde(rename = "input")]
        input: serde_json::Value,
        /// Result blocks. Upstream types this as `(TextContent |
        /// ImageContent)[]`; the port reuses [`Content`], which is a
        /// superset — a plugin that reads `item.text` off a text block
        /// works unchanged.
        content: Vec<Content>,
        /// Whether the tool failed.
        #[serde(rename = "isError")]
        is_error: bool,
        /// Structured details, when the tool produced any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    /// An extension is asked to advertise extra skill / prompt / theme
    /// paths. Mirrors upstream `resources_discover`.
    ResourcesDiscover {
        /// Working directory the session opened in.
        cwd: String,
        /// Why discovery runs.
        reason: ResourcesDiscoverReason,
    },

    // ------------------------------------------------------------------
    // Upstream parity — session lifecycle.
    // ------------------------------------------------------------------
    /// Upstream `session_shutdown`: fired when the extension runtime is
    /// torn down by quit, reload, or session replacement.
    ///
    /// Stage 3 shipped this as `SessionEnd` (tag `session_end`); the shim
    /// still accepts `session_end` as an alias (see
    /// the `EVENT_ALIASES` table in `pi-ext-shim.mjs`).
    SessionShutdown {
        /// Why the runtime is going away.
        reason: SessionShutdownReason,
        /// Destination session file, when shutdown is a session swap.
        #[serde(
            rename = "targetSessionFile",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        target_session_file: Option<String>,
    },
    /// Upstream `session_info_changed`: the session's metadata changed.
    SessionInfoChanged {
        /// Current normalized session name; `None` when it was cleared.
        #[serde(rename = "name")]
        name: Option<String>,
    },
    /// Upstream `session_compact`: context compaction succeeded.
    SessionCompact {
        /// What triggered the compaction.
        reason: CompactReason,
        /// Estimated input tokens before the rewrite.
        #[serde(rename = "tokensBefore")]
        tokens_before: u32,
        /// Estimated input tokens after the rewrite.
        #[serde(rename = "tokensAfter")]
        tokens_after: u32,
        /// How many trailing messages were retained verbatim.
        #[serde(rename = "retained")]
        retained: usize,
    },
    /// Upstream `session_before_switch`: a session swap is about to happen.
    /// A handler may return `{ cancel: true }` to stop it.
    SessionBeforeSwitch {
        /// Why the session is being replaced.
        reason: SessionBeforeSwitchReason,
        /// Destination session file, when the swap names one.
        #[serde(
            rename = "targetSessionFile",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        target_session_file: Option<String>,
    },
    /// Upstream `session_before_fork`: a fork is about to happen. A handler
    /// may return `{ cancel: true }` to stop it.
    SessionBeforeFork {
        /// Entry the fork is anchored on.
        #[serde(rename = "entryId")]
        entry_id: String,
        /// Whether the anchor entry itself is included.
        position: ForkPosition,
    },
    /// `session_fork`: a fork was just created. Carries the new
    /// session id and the parent session id so the extension can
    /// match them. Mirrors
    /// `pi_agent_rust`'s `session_before_fork` veto path: the
    /// upstream `session_before_fork` event lets a plugin refuse a
    /// fork; this `SessionFork` event lets it observe the success
    /// path. Subscribed via `pi.on("session_fork", handler)`.
    SessionFork {
        /// New session id created by the fork.
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Parent session id the fork was anchored on.
        #[serde(rename = "parentId")]
        parent_id: String,
        /// Entry the fork was anchored on (same as `entry_id`
        /// upstream emits on `session_before_fork`).
        #[serde(rename = "entryId")]
        entry_id: String,
    },
    /// Upstream `session_before_compact`: context compaction is about to run.
    /// A handler may return `{ cancel: true }` to stop it.
    SessionBeforeCompact {
        /// What triggered the compaction.
        reason: CompactReason,
        /// True when the aborted turn is retried after this compaction.
        #[serde(rename = "willRetry")]
        will_retry: bool,
        /// Extra instructions the user attached to `/compact`.
        #[serde(
            rename = "customInstructions",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        custom_instructions: Option<String>,
    },
    /// Upstream `session_compact_failed`: compaction failed or was aborted.
    SessionCompactFailed {
        /// What triggered the compaction attempt.
        reason: CompactReason,
        /// Error text when it failed for a non-abort reason.
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        /// True when the attempt was cancelled or aborted.
        aborted: bool,
        /// True when the aborted turn would have been retried.
        #[serde(rename = "willRetry")]
        will_retry: bool,
        /// True when the failing compaction content came from an extension.
        #[serde(rename = "fromExtension")]
        from_extension: bool,
    },
    /// Upstream `session_before_tree`: tree navigation is about to happen.
    /// A handler may return `{ cancel: true }` to stop it.
    SessionBeforeTree {
        /// Entry the navigation targets.
        #[serde(rename = "targetId")]
        target_id: String,
        /// Entry that was the leaf before the navigation.
        #[serde(rename = "oldLeafId", default, skip_serializing_if = "Option::is_none")]
        old_leaf_id: Option<String>,
        /// Whether the user asked for a branch summary.
        #[serde(rename = "userWantsSummary")]
        user_wants_summary: bool,
        /// Custom summarization instructions, when any.
        #[serde(
            rename = "customInstructions",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        custom_instructions: Option<String>,
    },
    /// Upstream `session_tree`: navigation in the session tree finished.
    SessionTree {
        /// Leaf the session now points at.
        #[serde(rename = "newLeafId")]
        new_leaf_id: Option<String>,
        /// Leaf the session pointed at before.
        #[serde(rename = "oldLeafId")]
        old_leaf_id: Option<String>,
        /// True when an extension supplied the branch summary.
        #[serde(rename = "fromExtension")]
        from_extension: bool,
    },

    // ------------------------------------------------------------------
    // Upstream parity — agent / turn / message.
    // ------------------------------------------------------------------
    /// Upstream `agent_start`: an agent loop started.
    AgentStart,
    /// Upstream `agent_end`: an agent loop ended.
    AgentEnd {
        /// Message log the run produced.
        messages: Vec<Message>,
    },
    /// Upstream `turn_start`.
    TurnStart {
        /// Zero-based index of this turn inside the session.
        #[serde(rename = "turnIndex")]
        turn_index: usize,
        /// Unix milliseconds when the turn began.
        #[serde(rename = "timestamp")]
        timestamp: i64,
    },
    /// Upstream `turn_end`.
    TurnEnd {
        /// Zero-based index of this turn inside the session.
        #[serde(rename = "turnIndex")]
        turn_index: usize,
        /// The assistant message the turn finished with.
        message: Message,
        /// Tool results the turn produced.
        #[serde(rename = "toolResults")]
        tool_results: Vec<Message>,
    },
    /// Upstream `message_start`.
    MessageStart {
        /// The message that started streaming.
        message: Message,
    },
    /// Upstream `message_update`.
    ///
    /// Divergence: upstream also carries the in-progress `message`; the Rust
    /// loop only exposes the delta (`assistantMessageEvent`), so that field is
    /// omitted rather than faked.
    MessageUpdate {
        /// The delta, shaped like upstream's `AssistantMessageEvent`.
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: serde_json::Value,
    },
    /// Upstream `message_end`.
    MessageEnd {
        /// The finished message.
        message: Message,
    },
    /// Upstream `context`: the message list is about to be handed to the
    /// model. A handler may return `{ messages }` to replace it.
    Context {
        /// Messages the request is about to carry.
        messages: Vec<Message>,
    },
    /// Upstream `before_provider_request`: the request payload is about to be
    /// sent. A handler's return value replaces the payload upstream; the Rust
    /// port has no wire-payload seam yet, so the value is advisory (see
    /// `docs/LUM1432_EXTENSION_EVENTS.md`).
    BeforeProviderRequest {
        /// Request descriptor the port is about to hand to the provider
        /// adapter.
        payload: serde_json::Value,
    },
    /// Upstream `before_provider_headers`: request headers were assembled.
    /// Handlers mutate `headers` in place upstream; the Rust adapters take
    /// only a credential + base URL, so the map is advisory today.
    BeforeProviderHeaders {
        /// Headers the port knows about at this seam.
        headers: serde_json::Map<String, serde_json::Value>,
    },
    /// Upstream `after_provider_response`: a provider response was received.
    AfterProviderResponse {
        /// HTTP status. `0` means the Rust adapters did not surface one
        /// (documented gap); `200` means the stream was established.
        status: u16,
        /// Response headers, empty when the adapters did not surface them.
        headers: serde_json::Map<String, serde_json::Value>,
    },
    /// Upstream `before_agent_start`: a user prompt was submitted, the agent
    /// run has not started yet. A handler may return `{ systemPrompt }` to
    /// replace the system prompt for the run.
    BeforeAgentStart {
        /// Raw user prompt text.
        prompt: String,
        /// Assembled system prompt.
        #[serde(rename = "systemPrompt")]
        system_prompt: String,
    },
    /// Upstream `agent_settled`: the run finished and no retry, compaction or
    /// queued continuation will follow.
    AgentSettled,

    // ------------------------------------------------------------------
    // Upstream parity — tool execution.
    // ------------------------------------------------------------------
    /// Upstream `tool_execution_start`.
    ToolExecutionStart {
        /// Provider-issued tool call id.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Parsed tool arguments.
        #[serde(rename = "args")]
        args: serde_json::Value,
    },
    /// Upstream `tool_execution_update`.
    ToolExecutionUpdate {
        /// Provider-issued tool call id.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Parsed tool arguments.
        #[serde(rename = "args")]
        args: serde_json::Value,
        /// Partial output produced so far (Rust emits text, not upstream's
        /// typed `partialResult` object).
        #[serde(rename = "partialResult")]
        partial_result: String,
    },
    /// Upstream `tool_execution_end`.
    ToolExecutionEnd {
        /// Provider-issued tool call id.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool name.
        #[serde(rename = "toolName")]
        tool_name: String,
        /// Final tool result (Rust shape; upstream sends the raw result).
        #[serde(rename = "result")]
        result: ToolResult,
        /// Whether the tool failed.
        #[serde(rename = "isError")]
        is_error: bool,
    },

    // ------------------------------------------------------------------
    // Upstream parity — model / thinking / bash / input.
    // ------------------------------------------------------------------
    /// Upstream `model_select`.
    ModelSelect {
        /// Model id that became active.
        #[serde(rename = "model")]
        model: String,
        /// Model id that was active before, when there was one.
        #[serde(
            rename = "previousModel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        previous_model: Option<String>,
        /// How the switch happened.
        #[serde(rename = "source")]
        source: ModelSelectSource,
    },
    /// Upstream `thinking_level_select`.
    ThinkingLevelSelect {
        /// Level that became active.
        level: String,
        /// Level that was active before.
        #[serde(rename = "previousLevel")]
        previous_level: String,
    },
    /// Upstream `user_bash`.
    UserBash {
        /// Command line the user typed (without the `!` / `!!` prefix).
        command: String,
        /// True for `!!`, i.e. excluded from the model's context.
        #[serde(rename = "excludeFromContext")]
        exclude_from_context: bool,
        /// Working directory the command runs in.
        cwd: String,
    },
    /// Upstream `input`: user input was received before agent processing.
    Input {
        /// The input text.
        text: String,
        /// Where the input came from.
        source: InputSource,
    },

    // ------------------------------------------------------------------
    // Upstream parity — trust and blocking UI prompts.
    // ------------------------------------------------------------------
    /// Upstream `project_trust`: the project needs a trust decision and a
    /// loaded extension may answer it.
    ProjectTrust {
        /// Working directory whose trust is undecided.
        cwd: String,
    },
    /// Upstream `ui_prompt_start`: Pi started waiting on a blocking
    /// extension UI prompt.
    UiPromptStart {
        /// Which dialog kind is blocking.
        kind: UiPromptKind,
        /// Dialog title, when the caller supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Upstream `ui_prompt_end`: Pi is no longer waiting on the prompt.
    UiPromptEnd {
        /// Which dialog kind was blocking.
        kind: UiPromptKind,
        /// Dialog title, when the caller supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Upstream `provider_stream`: a raw provider event arrived (LLM
    /// streaming JSON, retry hints, rate-limit signals). The Rust port
    /// currently surfaces the event as an opaque payload — the agent loop
    /// is the source. Plugin authors can subscribe, but should not rely
    /// on the payload shape (no upstream `agent.subscribe` source yet).
    ProviderStream {
        /// Opaque provider event payload.
        #[serde(rename = "providerEvent")]
        provider_event: serde_json::Value,
    },
    /// Upstream `cache_warming_decision`: cache-warming logic asked the
    /// extension whether to warm the prompt cache. The Rust port does
    /// not yet run a cache warmer; the variant is wired so the manifest
    /// contract can advertise the event today.
    CacheWarmingDecision {
        /// Estimated tokens the prompt would consume.
        #[serde(rename = "estimatedTokens")]
        estimated_tokens: u32,
        /// Cache TTL in milliseconds, when the warmer advertised one.
        #[serde(
            rename = "cacheTtlMs",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        cache_ttl_ms: Option<u32>,
    },
}

impl ExtensionEvent {
    /// The wire tag of this event — the string a plugin hands to `pi.on`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::UserMessage { .. } => "user_message",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::ResourcesDiscover { .. } => "resources_discover",
            Self::SessionShutdown { .. } => "session_shutdown",
            Self::SessionInfoChanged { .. } => "session_info_changed",
            Self::SessionCompact { .. } => "session_compact",
            Self::SessionBeforeSwitch { .. } => "session_before_switch",
            Self::SessionBeforeFork { .. } => "session_before_fork",
            Self::SessionFork { .. } => "session_fork",
            Self::SessionBeforeCompact { .. } => "session_before_compact",
            Self::SessionCompactFailed { .. } => "session_compact_failed",
            Self::SessionBeforeTree { .. } => "session_before_tree",
            Self::SessionTree { .. } => "session_tree",
            Self::AgentStart => "agent_start",
            Self::AgentEnd { .. } => "agent_end",
            Self::TurnStart { .. } => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::Context { .. } => "context",
            Self::BeforeProviderRequest { .. } => "before_provider_request",
            Self::BeforeProviderHeaders { .. } => "before_provider_headers",
            Self::AfterProviderResponse { .. } => "after_provider_response",
            Self::BeforeAgentStart { .. } => "before_agent_start",
            Self::AgentSettled => "agent_settled",
            Self::ToolExecutionStart { .. } => "tool_execution_start",
            Self::ToolExecutionUpdate { .. } => "tool_execution_update",
            Self::ToolExecutionEnd { .. } => "tool_execution_end",
            Self::ModelSelect { .. } => "model_select",
            Self::ThinkingLevelSelect { .. } => "thinking_level_select",
            Self::UserBash { .. } => "user_bash",
            Self::Input { .. } => "input",
            Self::ProjectTrust { .. } => "project_trust",
            Self::UiPromptStart { .. } => "ui_prompt_start",
            Self::UiPromptEnd { .. } => "ui_prompt_end",
            Self::ProviderStream { .. } => "provider_stream",
            Self::CacheWarmingDecision { .. } => "cache_warming_decision",
        }
    }
}

/// Why a session is being replaced (`session_before_switch.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionBeforeSwitchReason {
    /// A brand-new session is starting.
    New,
    /// A stored session is being resumed.
    Resume,
}

/// Where a fork is anchored (`session_before_fork.position`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPosition {
    /// Fork before the anchor entry (the entry is not carried over).
    Before,
    /// Fork at the anchor entry (it is the new tip).
    At,
}

/// Which blocking extension UI prompt is in flight
/// (`ui_prompt_start.kind` / `ui_prompt_end.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiPromptKind {
    /// Single-select dialog.
    Select,
    /// Yes/no confirmation.
    Confirm,
    /// Free-form text input.
    Input,
    /// External editor.
    Editor,
    /// Extension-provided custom component.
    Custom,
}

/// Why the extension runtime was torn down (`session_shutdown.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShutdownReason {
    /// The process is quitting.
    Quit,
    /// Configuration / extensions are being reloaded.
    Reload,
    /// A new session started.
    New,
    /// Another session was resumed.
    Resume,
    /// Another session was forked from this one.
    Fork,
}

/// What triggered a context compaction (`session_compact.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactReason {
    /// The user ran `/compact`.
    Manual,
    /// The context threshold was crossed.
    Threshold,
    /// Recovering from a context overflow error.
    Overflow,
}

/// How a model switch happened (`model_select.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelectSource {
    /// The model was set explicitly (`/model`, `--model`).
    Set,
    /// The user cycled models with a keybinding.
    Cycle,
    /// A stored session restored its model.
    Restore,
}

/// Where user input came from (`input.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    /// The interactive TUI.
    Interactive,
    /// A JSON-RPC client.
    Rpc,
    /// Another extension.
    Extension,
}

/// Why an extension is asked to advertise extra resource paths.
///
/// Mirrors the `reason` field of upstream's `ResourcesDiscoverEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcesDiscoverReason {
    /// The session is starting.
    Startup,
    /// Resources are being re-read after a config change.
    Reload,
}

/// UI request delivered to extensions (notify, confirm, input, select, custom).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiRequest {
    /// Non-blocking notification.
    Notify {
        /// Message body.
        message: String,
        /// Severity.
        #[serde(default)]
        level: UiLevel,
    },
    /// Yes/no confirmation.
    Confirm {
        /// Title of the dialog.
        title: String,
        /// Body text.
        body: String,
    },
    /// Free-form text input.
    Input {
        /// Title of the dialog.
        title: String,
        /// Placeholder text.
        #[serde(default)]
        placeholder: Option<String>,
    },
    /// Single-select from a list.
    Select {
        /// Title of the dialog.
        title: String,
        /// Options to choose from.
        options: Vec<String>,
    },
}

/// UI severity — drives colour and icon in the TUI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLevel {
    /// Informational.
    #[default]
    Info,
    /// Success.
    Success,
    /// Warning.
    Warning,
    /// Error.
    Error,
}

/// Response to a [`UiRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiResponse {
    /// Notify acknowledgment (no payload).
    NotifyAck,
    /// Confirm answer.
    Confirm {
        /// Whether the user accepted.
        accepted: bool,
    },
    /// Input answer.
    Input {
        /// Text the user entered.
        value: String,
    },
    /// Select answer.
    Select {
        /// Selected option (matches one of `UiRequest::Select::options`).
        value: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each new event variant added to the `ExtensionEvent` union must
    /// (a) round-trip through serde with its wire tag, and (b) report
    /// the same tag from `name()`. Pinned so a rename in either place
    /// fails the contract test instead of silently drifting.
    #[test]
    fn provider_stream_event_round_trips() {
        let event = ExtensionEvent::ProviderStream {
            provider_event: serde_json::json!({"chunk": "abc"}),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "provider_stream");
        assert_eq!(event.name(), "provider_stream");
        let back: ExtensionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn cache_warming_decision_round_trips() {
        let event = ExtensionEvent::CacheWarmingDecision {
            estimated_tokens: 4096,
            cache_ttl_ms: Some(60_000),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "cache_warming_decision");
        assert_eq!(json["estimatedTokens"], 4096);
        assert_eq!(json["cacheTtlMs"], 60_000);
        assert_eq!(event.name(), "cache_warming_decision");
        let back: ExtensionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn cache_warming_decision_omits_optional_ttl() {
        let event = ExtensionEvent::CacheWarmingDecision {
            estimated_tokens: 256,
            cache_ttl_ms: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(
            json.get("cacheTtlMs").is_none(),
            "the optional TTL must be skipped when None"
        );
        let back: ExtensionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
    }
}

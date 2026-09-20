//! Agent + extension event surface.
//!
//! Mirrors the typed event union in `packages/agent/src/types.ts` and the
//! extension event surface in `packages/coding-agent/src/extensions/types.ts`.

use serde::{Deserialize, Serialize};

use crate::{Content, Message, ToolCall, ToolResult, Usage};

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
/// * **Rust-native** variants from Stage 3 (`session_start`, `user_message`,
///   `tool_call`, `tool_result`, `resources_discover`). Their payloads are
///   Rust shapes and do **not** match upstream's field names.
/// * **Upstream-parity** variants (LUM-1246). Tag *and* payload field names
///   mirror `packages/coding-agent/src/core/extensions/types.ts` — a
///   snake_case `type` tag with camelCase fields — so a TypeScript plugin
///   that reads `event.toolCallId` / `event.assistantMessageEvent` works
///   without an adapter. Field names are pinned with explicit `rename`s
///   rather than a container attribute so the parity is visible at the
///   definition site.
///
/// `ExtensionEvent::name()` returns the wire tag; it is the string a plugin
/// passes to `pi.on(...)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEvent {
    // ------------------------------------------------------------------
    // Rust-native (Stage 3) — kept for wire compatibility.
    // ------------------------------------------------------------------
    /// Session started.
    SessionStart,
    /// Agent received a user message (Rust-native; upstream has no
    /// `user_message` — see [`ExtensionEvent::Input`]).
    UserMessage {
        /// The user message.
        message: Message,
    },
    /// Agent is about to call a tool (Rust-native; upstream's typed
    /// `tool_call` carries `toolCallId` / `toolName` / `input`).
    ToolCall {
        /// Tool call about to be executed.
        call: ToolCall,
    },
    /// Tool finished executing (Rust-native; upstream's `tool_result`
    /// carries `toolCallId` / `content` / `isError`).
    ToolResult {
        /// Tool result.
        result: ToolResult,
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
            Self::AgentStart => "agent_start",
            Self::AgentEnd { .. } => "agent_end",
            Self::TurnStart { .. } => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::ToolExecutionStart { .. } => "tool_execution_start",
            Self::ToolExecutionUpdate { .. } => "tool_execution_update",
            Self::ToolExecutionEnd { .. } => "tool_execution_end",
            Self::ModelSelect { .. } => "model_select",
            Self::ThinkingLevelSelect { .. } => "thinking_level_select",
            Self::UserBash { .. } => "user_bash",
            Self::Input { .. } => "input",
        }
    }
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

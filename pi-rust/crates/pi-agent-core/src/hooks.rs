//! Hook trait surfaces — `BeforeToolCall` / `AfterToolCall` / `BeforeTurn` /
//! `AfterTurn` — that match the extension-event hook points in
//! `packages/agent/src/types.ts` and the extension hook points in
//! `packages/coding-agent/src/extensions/types.ts`.
//!
//! `ShouldStopAfterTurnContext` / `PrepareNextTurnContext` /
//! `AgentLoopTurnUpdate` / `AgentHookAdapter` and the matching
//! `ShouldStopHookFn` / `PrepareHookFn` aliases mirror the same hook points
//! in `packages/agent/src/agent-loop.ts`. The aliases use an owned
//! `BoxFuture<'static, _>` (no HRTB) so user callbacks can stash borrowed
//! state by cloning into the owned context before calling the closure —
//! matching the `for<'a>`-broken prototype the original `AgentHookAdapter`
//! silently swallowed.

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use pi_protocol::{
    AssistantMessage, Context as AgentContext, Message, Model, ToolCall, ToolResult,
};

/// Async lifecycle hooks for the extension events that fire *inside* a run
/// and can rewrite what happens next.
///
/// The loop's [`EventObserver`](crate::agent_loop::EventObserver) is a plain
/// synchronous `Fn`: it fans an event out to every subscriber but cannot
/// await a plugin's answer. The upstream extension events below are
/// request/response — `context` replaces the message list,
/// `before_agent_start` replaces the system prompt — so they need an async
/// seam instead. This trait *is* that seam.
///
/// `pi-agent-core` deliberately does not construct
/// [`ExtensionEvent`](pi_protocol::ExtensionEvent) values here: the JS
/// runtime lives in `pi-coding-agent`, which implements this trait and owns
/// the construction + dispatch (`crate::extensions::lifecycle`). Every
/// default implementation is a no-op, so a host that implements only one
/// method does not affect the others.
#[async_trait]
pub trait LifecycleHooks: Send + Sync {
    /// Upstream `before_agent_start`: called once per submitted prompt, before
    /// the run's `agent_start`. Returning `Some(prompt)` replaces the system
    /// prompt for this run (upstream chains multiple handlers).
    async fn before_agent_start(&self, _prompt: &str, _system_prompt: &str) -> Option<String> {
        None
    }

    /// Upstream `context`: called before each provider call. Returning a
    /// different list replaces the messages that request carries.
    async fn context(&self, messages: Vec<Message>) -> Vec<Message> {
        messages
    }

    /// Upstream `agent_settled`: the run really finished — no retry,
    /// compaction or queued continuation follows.
    async fn agent_settled(&self) {}

    /// Upstream `before_provider_request`: the request is about to be handed
    /// to the provider adapter. The returned payload is advisory until the
    /// Rust port grows a wire-payload seam (see
    /// `docs/LUM1432_EXTENSION_EVENTS.md`).
    async fn before_provider_request(&self, payload: serde_json::Value) -> serde_json::Value {
        payload
    }

    /// Upstream `before_provider_headers`: request headers were assembled.
    /// The returned map is advisory for the same reason as
    /// [`Self::before_provider_request`].
    async fn before_provider_headers(
        &self,
        headers: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        headers
    }

    /// Upstream `after_provider_response`: the provider call returned.
    /// `ok` is `true` when a stream was established; the Rust adapters do not
    /// surface the HTTP status yet, so the status the event carries is
    /// derived from this flag and documented as a gap.
    async fn after_provider_response(&self, _ok: bool) {}
}

/// Hook called before a tool executes.
///
/// Returning `BeforeToolCallDecision::Block` prevents the tool from running;
/// the loop emits an error tool result instead.
#[async_trait]
pub trait BeforeToolCall: Send + Sync {
    /// Inspect / block a tool call.
    async fn before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision;
}

/// Hook called after a tool executes. May rewrite the result before the
/// loop emits it.
#[async_trait]
pub trait AfterToolCall: Send + Sync {
    /// Mutate (or replace) a tool result.
    async fn after_tool_call(&self, result: &mut ToolResult);
}

/// Hook called before each assistant turn starts.
#[async_trait]
pub trait BeforeTurn: Send + Sync {
    /// Observe (or rewrite) the messages handed to the model.
    async fn before_turn(&self, messages: &mut Vec<Message>);
}

/// Hook called after each assistant turn finishes.
#[async_trait]
pub trait AfterTurn: Send + Sync {
    /// Observe the final assistant message.
    async fn after_turn(&self, message: &AssistantMessage);
}

/// Decision returned from [`BeforeToolCall::before_tool_call`].
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallDecision {
    /// True if the tool should be blocked.
    pub block: bool,
    /// Human-readable reason shown in the error tool result.
    pub reason: Option<String>,
    /// Hint that the loop should stop after this tool batch.
    pub terminate: bool,
}

impl BeforeToolCallDecision {
    /// Allow the tool to execute.
    pub fn allow() -> Self {
        Self::default()
    }

    /// Block the tool from executing with the given reason.
    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            block: true,
            reason: Some(reason.into()),
            terminate: false,
        }
    }
}

/// Thinking / reasoning level requested for the next provider call.
///
/// Mirrors `ThinkingLevel` in `packages/agent/src/types.ts`. Stage 1 of
/// the port streams the raw level to provider adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThinkingLevel {
    /// No reasoning tokens requested.
    Off,
    /// Minimal effort.
    Minimal,
    /// Low effort.
    Low,
    /// Medium effort.
    Medium,
    /// High effort.
    High,
    /// Extra-high effort (provider-specific support).
    Xhigh,
    /// Maximum effort (provider-specific support).
    Max,
}

impl ThinkingLevel {
    /// Every level in upstream declaration order (off → max). Mirrors
    /// `EXTENDED_THINKING_LEVELS` in `packages/ai/src/models.ts:913` and
    /// `THINKING_LEVEL_OPTIONS` in
    /// `packages/coding-agent/src/core/defaults.ts`.
    pub const ALL: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];

    /// The wire / settings spelling of this level (upstream's
    /// `ModelThinkingLevel` string union).
    pub const fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    /// True when the level would request reasoning tokens from the model.
    pub fn is_reasoning(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl std::fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string is not one of the seven thinking levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseThinkingLevelError {
    /// The offending input, kept for the message.
    pub input: String,
}

impl std::fmt::Display for ParseThinkingLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown thinking level {:?}", self.input)
    }
}

impl std::error::Error for ParseThinkingLevelError {}

impl std::str::FromStr for ThinkingLevel {
    type Err = ParseThinkingLevelError;

    /// Parse the upstream spelling, case-insensitively
    /// (`packages/ai/src/models.ts` compares `toLowerCase()`).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(ThinkingLevel::Off),
            "minimal" => Ok(ThinkingLevel::Minimal),
            "low" => Ok(ThinkingLevel::Low),
            "medium" => Ok(ThinkingLevel::Medium),
            "high" => Ok(ThinkingLevel::High),
            "xhigh" | "x-high" | "extra-high" => Ok(ThinkingLevel::Xhigh),
            "max" => Ok(ThinkingLevel::Max),
            _ => Err(ParseThinkingLevelError {
                input: text.trim().to_string(),
            }),
        }
    }
}

/// Owned context passed to [`ShouldStopHookFn`].
///
/// The agent loop clones the borrowed state into this struct before calling
/// the user callback, so the callback never has to outlive a borrow of the
/// loop's internal state. This is what keeps the signature
/// `Fn(ShouldStopAfterTurnContext) -> BoxFuture<'static, _>` honest.
///
/// `tool_results` is a list of `Message` rows with `Role::Tool` carrying the
/// `ToolResult` content block — i.e. the same wire shape as the TS
/// `ToolResultMessage` interface, expressed through Rust's existing
/// `Message`/`Content` enums.
#[derive(Debug, Clone)]
pub struct ShouldStopAfterTurnContext {
    /// The assistant message that completed the turn.
    pub message: AssistantMessage,
    /// Tool result messages passed to the preceding `turn_end` event.
    pub tool_results: Vec<Message>,
    /// Current agent context after the turn's assistant message and tool
    /// results have been appended.
    pub context: AgentContext,
    /// Messages this loop invocation will return if it exits at this
    /// point. Prompt runs include the initial prompt messages;
    /// continuation runs do not include pre-existing context messages.
    pub new_messages: Vec<Message>,
}

/// Owned context passed to [`PrepareHookFn`].
///
/// Same shape as [`ShouldStopAfterTurnContext`]; the TS side
/// `extends` the former. In Rust we keep them as separate concrete types
/// to avoid a hidden HRTB when the alias widens the future bound.
#[derive(Debug, Clone)]
pub struct PrepareNextTurnContext {
    /// The assistant message that completed the turn.
    pub message: AssistantMessage,
    /// Tool result messages passed to the preceding `turn_end` event.
    pub tool_results: Vec<Message>,
    /// Current agent context after the turn's assistant message and tool
    /// results have been appended.
    pub context: AgentContext,
    /// Messages this loop invocation will return if it exits at this
    /// point.
    pub new_messages: Vec<Message>,
}

impl From<ShouldStopAfterTurnContext> for PrepareNextTurnContext {
    fn from(value: ShouldStopAfterTurnContext) -> Self {
        Self {
            message: value.message,
            tool_results: value.tool_results,
            context: value.context,
            new_messages: value.new_messages,
        }
    }
}

/// Replacement runtime state applied before the next provider request.
///
/// All fields are optional — `None` keeps the current value. Mirrors
/// `AgentLoopTurnUpdate` in `packages/agent/src/types.ts`.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    /// Context for the next provider request.
    pub context: Option<AgentContext>,
    /// Model for the next provider request.
    pub model: Option<Model>,
    /// Thinking level for the next provider request.
    pub thinking_level: Option<ThinkingLevel>,
}

/// User-facing callback deciding whether the agent loop exits after the
/// most recent turn.
///
/// The `BoxFuture<'static, _>` return type is owned (no HRTB) — the loop
/// clones borrowed state into a [`ShouldStopAfterTurnContext`] before
/// calling this closure, so the returned future does not need to borrow
/// from the loop.
pub type ShouldStopHookFn =
    Arc<dyn Fn(ShouldStopAfterTurnContext) -> BoxFuture<'static, bool> + Send + Sync>;

/// User-facing callback mutating the loop state before the next turn.
///
/// Same lifetime story as [`ShouldStopHookFn`]: the loop hands the
/// callback an owned [`PrepareNextTurnContext`], so the returned future
/// never borrows from the loop.
pub type PrepareHookFn = Arc<
    dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;

/// Adapter that stores optional `should_stop_after_turn` and
/// `prepare_next_turn` callbacks and forwards to them when invoked.
///
/// `AgentHookAdapter` is the seam between [`AgentOptions`] and
/// [`AgentLoop`](crate::AgentLoop): the `Agent` constructs one from
/// `AgentOptions`, then hands it to the loop. Each `invoke_*` method
/// returns the hook's decision when a hook is registered, otherwise the
/// loop's default — `false` for `should_stop_after_turn` and `None` for
/// `prepare_next_turn`.
#[derive(Default, Clone)]
pub struct AgentHookAdapter {
    /// Optional `should_stop_after_turn` callback.
    pub should_stop_after_turn: Option<ShouldStopHookFn>,
    /// Optional `prepare_next_turn` callback.
    pub prepare_next_turn: Option<PrepareHookFn>,
    /// Optional `before_tool_call` hook. When unset the loop allows every
    /// tool call.
    pub before_tool_call: Option<Arc<dyn BeforeToolCall>>,
    /// Optional `after_tool_call` hook. When unset the loop emits the
    /// executor's result unchanged.
    pub after_tool_call: Option<Arc<dyn AfterToolCall>>,
    /// Optional async lifecycle hooks for the extension events that fire
    /// inside a run (`context`, `before_agent_start`, …). When unset every
    /// point is a no-op.
    pub lifecycle: Option<Arc<dyn LifecycleHooks>>,
}

impl std::fmt::Debug for AgentHookAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHookAdapter")
            .field(
                "should_stop_after_turn",
                &self.should_stop_after_turn.as_ref().map(|_| "…"),
            )
            .field(
                "prepare_next_turn",
                &self.prepare_next_turn.as_ref().map(|_| "…"),
            )
            .field(
                "before_tool_call",
                &self.before_tool_call.as_ref().map(|_| "…"),
            )
            .field(
                "after_tool_call",
                &self.after_tool_call.as_ref().map(|_| "…"),
            )
            .field("lifecycle", &self.lifecycle.as_ref().map(|_| "…"))
            .finish()
    }
}

impl AgentHookAdapter {
    /// Construct an empty adapter with no hooks registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a `should_stop_after_turn` callback.
    pub fn with_should_stop_after_turn(mut self, hook: ShouldStopHookFn) -> Self {
        self.should_stop_after_turn = Some(hook);
        self
    }

    /// Register a `prepare_next_turn` callback.
    pub fn with_prepare_next_turn(mut self, hook: PrepareHookFn) -> Self {
        self.prepare_next_turn = Some(hook);
        self
    }

    /// Register a `before_tool_call` hook.
    pub fn with_before_tool_call(mut self, hook: Arc<dyn BeforeToolCall>) -> Self {
        self.before_tool_call = Some(hook);
        self
    }

    /// Register an `after_tool_call` hook.
    pub fn with_after_tool_call(mut self, hook: Arc<dyn AfterToolCall>) -> Self {
        self.after_tool_call = Some(hook);
        self
    }

    /// Register the async lifecycle hooks.
    pub fn with_lifecycle(mut self, hook: Arc<dyn LifecycleHooks>) -> Self {
        self.lifecycle = Some(hook);
        self
    }

    /// Invoke the registered `should_stop_after_turn` hook, or return
    /// `false` when no hook is registered.
    pub async fn invoke_should_stop(&self, ctx: ShouldStopAfterTurnContext) -> bool {
        match &self.should_stop_after_turn {
            Some(hook) => hook(ctx).await,
            None => false,
        }
    }

    /// Invoke the registered `prepare_next_turn` hook, or return `None`
    /// when no hook is registered.
    pub async fn invoke_prepare_next_turn(
        &self,
        ctx: PrepareNextTurnContext,
    ) -> Option<AgentLoopTurnUpdate> {
        match &self.prepare_next_turn {
            Some(hook) => hook(ctx).await,
            None => None,
        }
    }

    /// Invoke the registered `before_tool_call` hook, or allow the call
    /// when no hook is registered.
    pub async fn invoke_before_tool_call(&self, call: &ToolCall) -> BeforeToolCallDecision {
        match &self.before_tool_call {
            Some(hook) => hook.before_tool_call(call).await,
            None => BeforeToolCallDecision::allow(),
        }
    }

    /// Invoke the registered `after_tool_call` hook. When no hook is
    /// registered the result is left untouched.
    pub async fn invoke_after_tool_call(&self, result: &mut ToolResult) {
        if let Some(hook) = &self.after_tool_call {
            hook.after_tool_call(result).await;
        }
    }

    /// Invoke the registered [`LifecycleHooks::before_agent_start`], or
    /// return `None` when no lifecycle hook is registered.
    pub async fn invoke_before_agent_start(
        &self,
        prompt: &str,
        system_prompt: &str,
    ) -> Option<String> {
        match &self.lifecycle {
            Some(hook) => hook.before_agent_start(prompt, system_prompt).await,
            None => None,
        }
    }

    /// Invoke the registered [`LifecycleHooks::context`], or return the
    /// messages unchanged when no lifecycle hook is registered.
    pub async fn invoke_context(&self, messages: Vec<Message>) -> Vec<Message> {
        match &self.lifecycle {
            Some(hook) => hook.context(messages).await,
            None => messages,
        }
    }

    /// Invoke the registered [`LifecycleHooks::agent_settled`].
    pub async fn invoke_agent_settled(&self) {
        if let Some(hook) = &self.lifecycle {
            hook.agent_settled().await;
        }
    }

    /// Invoke the registered [`LifecycleHooks::before_provider_request`], or
    /// return the payload unchanged when no lifecycle hook is registered.
    pub async fn invoke_before_provider_request(
        &self,
        payload: serde_json::Value,
    ) -> serde_json::Value {
        match &self.lifecycle {
            Some(hook) => hook.before_provider_request(payload).await,
            None => payload,
        }
    }

    /// Invoke the registered [`LifecycleHooks::before_provider_headers`], or
    /// return the map unchanged when no lifecycle hook is registered.
    pub async fn invoke_before_provider_headers(
        &self,
        headers: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        match &self.lifecycle {
            Some(hook) => hook.before_provider_headers(headers).await,
            None => headers,
        }
    }

    /// Invoke the registered [`LifecycleHooks::after_provider_response`].
    pub async fn invoke_after_provider_response(&self, ok: bool) {
        if let Some(hook) = &self.lifecycle {
            hook.after_provider_response(ok).await;
        }
    }
}

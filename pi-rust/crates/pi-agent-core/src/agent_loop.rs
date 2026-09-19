//! Agent loop — Stage 2 driver. Mirrors the loop in
//! `packages/agent/src/agent-loop.ts`, including the `should_stop_after_turn`
//! and `prepare_next_turn` hook points.
//!
//! Stage 2 keeps the surface tight: the loop streams a single assistant
//! turn per iteration, executes inline tool calls through the registered
//! [`ToolExecutor`](crate::tools::ToolExecutor) (falling back to a stub when
//! none is configured), and forwards `should_stop_after_turn` /
//! `prepare_next_turn` decisions to the user-registered hooks. Stage 4 fills
//! in queue draining and steering / follow-up message sources.

use futures::StreamExt;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{
    AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Role,
    ToolCall, ToolResult,
};
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::hooks::{
    AgentHookAdapter, AgentLoopTurnUpdate, PrepareNextTurnContext, ShouldStopAfterTurnContext,
};
use crate::state::{AgentConfig, AgentState};
use crate::tools::ToolExecutor;

/// Errors the agent loop can surface to its caller.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Streaming layer returned an error.
    #[error("stream error: {0}")]
    Stream(String),
    /// Tool execution failed.
    #[error("tool error in {tool}: {message}")]
    Tool {
        /// Tool name.
        tool: String,
        /// Error message.
        message: String,
    },
    /// Provider produced an `AssistantMessageEvent::Error`.
    #[error("provider error: {0}")]
    Provider(String),
}

/// Outcome of a single [`AgentLoop::run`] invocation.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    /// Final assistant message.
    pub message: AssistantMessage,
    /// Tool results produced by this invocation (one per tool call).
    pub tool_results: Vec<Message>,
    /// True if the loop asked the model to call a tool and that tool ran.
    pub tool_executed: bool,
}

/// Snapshot of the live loop configuration — `model` plus the optional
/// `thinking_level` override a `prepare_next_turn` hook can install.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Active model. Mutated in place by `prepare_next_turn`.
    pub model: pi_protocol::Model,
    /// Optional thinking level override.
    pub thinking_level: Option<crate::hooks::ThinkingLevel>,
}

impl From<&AgentConfig> for LoopConfig {
    fn from(config: &AgentConfig) -> Self {
        Self {
            model: config.model.clone(),
            thinking_level: None,
        }
    }
}

/// Single agent turn entry point.
#[derive(Clone)]
pub struct AgentLoop {
    config: AgentConfig,
    state: AgentState,
    hooks: AgentHookAdapter,
    follow_up: Vec<Message>,
    signal: CancellationToken,
}

impl AgentLoop {
    /// Construct a loop from a configuration, initial state, and hook
    /// adapter.
    pub fn new(config: AgentConfig, state: AgentState, hooks: AgentHookAdapter) -> Self {
        Self {
            config,
            state,
            hooks,
            follow_up: Vec::new(),
            signal: CancellationToken::new(),
        }
    }

    /// Borrow the current state.
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Mutable borrow of the state — used by [`Agent`] for
    /// per-turn overrides and by the session layer to inspect /
    /// mutate the message log.
    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    /// Borrow the configuration.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Mutable borrow of the configuration — used by [`Agent::set_model`]
    /// to swap the active model between turns.
    pub fn config_mut(&mut self) -> &mut AgentConfig {
        &mut self.config
    }

    /// Borrow the hook adapter driving this loop.
    pub fn hooks(&self) -> &AgentHookAdapter {
        &self.hooks
    }

    /// Mutable borrow of the hook adapter — tests use this to swap hooks
    /// in mid-run.
    pub fn hooks_mut(&mut self) -> &mut AgentHookAdapter {
        &mut self.hooks
    }

    /// Cooperative cancellation handle handed to tool executors.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.signal.clone()
    }

    /// Replace the cancellation handle used for tool execution. A caller can
    /// cancel the returned token to ask the active executor to stop.
    pub fn set_cancellation_token(&mut self, signal: CancellationToken) {
        self.signal = signal;
    }

    /// Add a user message to the log. Real draining happens during
    /// [`AgentLoop::run`].
    pub fn push_user(&mut self, message: Message) {
        self.state.messages.push(message);
    }

    /// Push a follow-up message that the next [`AgentLoop::run`] will
    /// process after the loop's initial prompt messages.
    pub fn push_follow_up(&mut self, message: Message) {
        self.follow_up.push(message);
    }

    /// Number of queued follow-up messages waiting to be processed.
    pub fn follow_up_len(&self) -> usize {
        self.follow_up.len()
    }

    /// Run the loop until the agent would stop, calling
    /// `should_stop_after_turn` and `prepare_next_turn` at the right
    /// points and forwarding their decisions to the live configuration.
    ///
    /// Stage 2 returns the outcome of the *final* turn — multiple turns
    /// are driven internally so a `prepare_next_turn` hook can swap
    /// `context` / `model` between turns. The per-turn outcomes are
    /// emitted via the `on_turn` callback (Stage 4 wires the public
    /// event stream).
    pub async fn run(
        &mut self,
        prompts: Vec<Message>,
        mut on_turn: impl FnMut(&TurnOutcome) + Send,
    ) -> Result<TurnOutcome, AgentError> {
        // Step 1 — clone initial prompts into the state and the new-message list.
        let mut new_messages: Vec<Message> = prompts.clone();
        for prompt in &prompts {
            self.state.messages.push(prompt.clone());
        }

        let mut current_context: AgentContext = AgentContext {
            system_prompt: self.state.system_prompt.clone(),
            messages: self.state.messages.clone(),
            tools: self.config.tool_definitions(),
        };
        let mut loop_config: LoopConfig = (&self.config).into();

        let mut last_completed_turn: Option<ShouldStopAfterTurnContext> = None;
        let mut has_more_tool_calls = false;
        // Pending messages injected at the start of the next turn
        // (analogous to the TS `pendingMessages` buffer). The first
        // turn is always allowed because we have prompts to process.
        let mut pending_messages: Vec<Message> = std::mem::take(&mut self.follow_up);
        if !prompts.is_empty() {
            has_more_tool_calls = true;
        }

        loop {
            if has_more_tool_calls || !pending_messages.is_empty() {
                // Step 2 — call `prepare_next_turn` before the next turn
                // if we have a previously completed turn to hand it.
                if let Some(turn) = last_completed_turn.take() {
                    let prepare_ctx = PrepareNextTurnContext::from(turn);
                    if let Some(update) = self.hooks.invoke_prepare_next_turn(prepare_ctx).await {
                        apply_turn_update(&update, &mut current_context, &mut loop_config);
                    }
                }

                // Step 3 — drain pending messages into the live context.
                for message in pending_messages.drain(..) {
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }

                // Step 4 — stream a single assistant response.
                // `prepare_next_turn` may have replaced the context
                // wholesale; re-advertise the registered tools unless the
                // hook supplied its own list.
                if current_context.tools.is_empty() {
                    current_context.tools = self.config.tool_definitions();
                }
                let assistant_message = stream_assistant_response(
                    &self.config.stream_fn,
                    &current_context,
                    &loop_config,
                )
                .await?;
                let assistant_log_message = Message {
                    role: Role::Assistant,
                    content: assistant_message.content.clone(),
                    model: Some(assistant_message.model.clone()),
                };
                current_context.messages.push(assistant_log_message.clone());
                new_messages.push(assistant_log_message);

                // Step 5 — execute any tool calls emitted by the model.
                // `BeforeToolCall` may block a call, the executor may fail,
                // and `AfterToolCall` may rewrite the result — none of which
                // aborts the turn.
                let (tool_results, continue_loop) = execute_tool_calls(
                    self.config.tool_executor.as_ref(),
                    &self.hooks,
                    &assistant_message,
                    &self.signal,
                )
                .await;
                let mut tool_result_messages: Vec<Message> = Vec::with_capacity(tool_results.len());
                for result in tool_results {
                    let msg = Message {
                        role: Role::Tool,
                        content: vec![pi_protocol::Content::ToolResult(result.clone())],
                        model: None,
                    };
                    current_context.messages.push(msg.clone());
                    new_messages.push(msg.clone());
                    tool_result_messages.push(msg);
                }
                has_more_tool_calls = continue_loop;

                let outcome = TurnOutcome {
                    message: assistant_message.clone(),
                    tool_results: tool_result_messages.clone(),
                    tool_executed: !tool_result_messages.is_empty(),
                };
                on_turn(&outcome);

                // Step 6 — build the completed-turn context the next
                // hook will consume.
                let turn = ShouldStopAfterTurnContext {
                    message: assistant_message,
                    tool_results: tool_result_messages,
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };

                // Step 7 — ask the user hook whether the agent should
                // stop here.
                if self.hooks.invoke_should_stop(turn.clone()).await {
                    // Sync our state with the live context before
                    // exiting so subsequent `state.messages` reads
                    // reflect the run.
                    self.state.messages = current_context.messages.clone();
                    return Ok(TurnOutcome {
                        message: turn.message,
                        tool_results: turn.tool_results,
                        tool_executed: outcome.tool_executed,
                    });
                }

                last_completed_turn = Some(turn);

                // Step 8 — check for newly-arrived follow-up messages.
                if !has_more_tool_calls {
                    let follow_up = std::mem::take(&mut self.follow_up);
                    if follow_up.is_empty() {
                        self.state.messages = current_context.messages.clone();
                        return Ok(TurnOutcome {
                            message: last_completed_turn
                                .as_ref()
                                .map(|t| t.message.clone())
                                .unwrap(),
                            tool_results: last_completed_turn
                                .as_ref()
                                .map(|t| t.tool_results.clone())
                                .unwrap_or_default(),
                            tool_executed: outcome.tool_executed,
                        });
                    }
                    pending_messages = follow_up;
                    has_more_tool_calls = true;
                }
            } else {
                break;
            }
        }

        // Sync state and return.
        self.state.messages = current_context.messages.clone();
        Ok(TurnOutcome {
            message: last_completed_turn
                .as_ref()
                .map(|t| t.message.clone())
                .unwrap_or_else(|| AssistantMessage {
                    model: loop_config.model.id.clone(),
                    content: Vec::new(),
                    stop_reason: pi_protocol::StopReason::Empty,
                    usage: pi_protocol::Usage::default(),
                }),
            tool_results: last_completed_turn
                .as_ref()
                .map(|t| t.tool_results.clone())
                .unwrap_or_default(),
            tool_executed: false,
        })
    }
}

/// Apply an [`AgentLoopTurnUpdate`] to the live loop configuration. Each
/// `Some(_)` field overrides the previous value; `None` leaves the field
/// untouched.
fn apply_turn_update(
    update: &AgentLoopTurnUpdate,
    context: &mut AgentContext,
    config: &mut LoopConfig,
) {
    if let Some(replacement) = &update.context {
        *context = replacement.clone();
    }
    if let Some(model) = &update.model {
        config.model = model.clone();
    }
    if let Some(level) = update.thinking_level {
        config.thinking_level = Some(level);
    }
}

/// Stream a single assistant response from the provider and fold the
/// event stream into the final [`AssistantMessage`].
async fn stream_assistant_response(
    stream_fn: &SharedStreamFn,
    context: &AgentContext,
    config: &LoopConfig,
) -> Result<AssistantMessage, AgentError> {
    let options = pi_ai::SimpleStreamOptions::default();
    let mut stream = stream_fn
        .stream_simple(&config.model, context, &options)
        .await
        .map_err(|err| AgentError::Stream(err.to_string()))?;

    let mut final_message: Option<AssistantMessage> = None;
    while let Some(event) = stream.next().await {
        match event.map_err(|err| AgentError::Stream(err.to_string()))? {
            AssistantMessageEvent::Start { model } => {
                final_message = Some(AssistantMessage {
                    model,
                    content: Vec::new(),
                    stop_reason: pi_protocol::StopReason::Empty,
                    usage: pi_protocol::Usage::default(),
                });
            }
            AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => {
                if let Some(message) = final_message.as_mut() {
                    message.content = content;
                    message.stop_reason = stop_reason;
                    message.usage = usage;
                } else {
                    final_message = Some(AssistantMessage {
                        model: config.model.id.clone(),
                        content,
                        stop_reason,
                        usage,
                    });
                }
            }
            AssistantMessageEvent::Error { message } => {
                return Err(AgentError::Provider(message));
            }
            AssistantMessageEvent::Aborted => {
                if let Some(message) = final_message.as_mut() {
                    message.stop_reason = pi_protocol::StopReason::Aborted;
                }
            }
            // Text / thinking / toolcall deltas are ignored — Stage 2
            // collapses the stream into the final message only.
            AssistantMessageEvent::TextDelta { .. }
            | AssistantMessageEvent::ThinkingDelta { .. }
            | AssistantMessageEvent::ToolCallDelta { .. } => {}
        }
    }

    final_message.ok_or_else(|| {
        AgentError::Stream("provider stream closed without a Start or Done event".into())
    })
}

/// Execute the tool calls emitted by an assistant message.
///
/// Each call runs in source order. For every call the loop:
///
/// 1. asks the `BeforeToolCall` hook for a decision — a `Block` decision
///    turns into an `is_error: true` result without executing anything;
/// 2. dispatches to the registered [`ToolExecutor`](crate::tools::ToolExecutor)
///    — or, when none is registered, keeps the Stage 2 stub behaviour so
///    callers that predate tool execution still work;
/// 3. asks the `AfterToolCall` hook to rewrite the result.
///
/// Tool failures never abort the turn: they surface as `is_error: true`
/// [`ToolResult`]s so the model can react to them. `continue_loop` mirrors
/// the TypeScript loop's `!terminate` — it is `true` unless *every* tool in
/// the batch requested termination through `BeforeToolCallDecision`.
async fn execute_tool_calls(
    executor: Option<&Arc<dyn ToolExecutor>>,
    hooks: &AgentHookAdapter,
    assistant_message: &AssistantMessage,
    signal: &CancellationToken,
) -> (Vec<ToolResult>, bool) {
    let tool_calls: Vec<&ToolCall> = assistant_message
        .content
        .iter()
        .filter_map(|c| match c {
            Content::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();

    if tool_calls.is_empty() {
        return (Vec::new(), false);
    }

    let mut all_terminate = true;
    let mut results = Vec::with_capacity(tool_calls.len());
    for call in tool_calls {
        let decision = hooks.invoke_before_tool_call(call).await;
        all_terminate &= decision.terminate;

        if decision.block {
            let reason = decision
                .reason
                .unwrap_or_else(|| "blocked by before_tool_call".to_string());
            results.push(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(format!("tool call blocked: {reason}"))),
                is_error: true,
                details: None,
            });
            continue;
        }

        let mut result = match executor {
            Some(executor) => match executor.execute(call, signal.clone()).await {
                Ok(result) => result,
                // Executor-level failures become error results; the turn
                // continues so the model can correct course.
                Err(err) => ToolResult {
                    tool_call_id: call.id.clone(),
                    content: Box::new(Content::text(err.to_string())),
                    is_error: true,
                    details: None,
                },
            },
            // Backward compatibility: no executor registered, so keep the
            // Stage 2 stub result the existing loop tests assert on.
            None => ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(format!("(stub) executed {}", call.name))),
                is_error: false,
                details: None,
            },
        };
        hooks.invoke_after_tool_call(&mut result).await;
        results.push(result);
    }

    (results, !all_terminate)
}

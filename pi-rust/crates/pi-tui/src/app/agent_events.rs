//! Agent event ingestion.
//!
//! [`App::drain_agent_events`] funnels every queued [`AgentEvent`] from
//! the agent's broadcast channel through [`App::apply_event`], which is
//! the single sink that updates the message view, the status bar, the
//! turn-usage cache, and the pending-error slot.
//!
//! The event handler lives here so [`super::App`] stays focused on the
//! per-frame rendering surface; the message view + tool-block renderer
//! already do most of the heavy lifting, so the body is mostly a long
//! `match` over `AgentEvent`.
//!
//! # Extension subscription surface
//!
//! [`AgentEventHandler`] is the public trait extensions and overlays
//! implement to receive typed callbacks for each stage of a turn.
//! [`AgentEventRouter`] owns the fan-out list and dispatches events to
//! every subscriber; [`default_router`] returns an empty router that
//! callers can `.push(...)` handlers onto. These three types are
//! additive — the existing [`App::apply_event`] pipeline still owns the
//! message view + status bar; the router only forwards to extra
//! subscribers.

use std::sync::atomic::Ordering;

use pi_agent_core::{AgentEvent, AssistantMessageUpdate};
use pi_protocol::{Content, Message, StopReason, ToolCall, ToolResult, Usage};
use tokio::sync::mpsc;

use super::{App, TurnUsage};

impl App {
    /// Drain any pending agent events into the message view. The
    /// caller calls this on every render tick — the App drains
    /// synchronously, so the TUI never blocks on the agent.
    pub fn drain_agent_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let event = match self.event_rx.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.cancel_token = None;
                        self.event_rx = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            self.apply_event(event);
        }
        changed
    }

    /// Apply one [`AgentEvent`] to the message view and status bar.
    ///
    /// [`App::drain_agent_events`] funnels every queued event through this;
    /// it is public so a driver (or a test) can inject a synthetic event —
    /// the faux provider only streams text, so a thinking or tool event has
    /// no other way in (the render loop itself goes through
    /// [`App::drain_agent_events`]).
    pub fn apply_agent_event(&mut self, event: AgentEvent) {
        self.apply_event(event);
    }

    /// Apply a single [`AgentEvent`] to the message view + status bar.
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {}
            AgentEvent::MessageStart { model } => {
                self.tool_call_ids.clear();
                self.messages.begin_assistant_stream(&model);
            }
            AgentEvent::MessageUpdate(update) => match update {
                AssistantMessageUpdate::TextDelta { delta } => {
                    self.messages.append_assistant_delta(&delta);
                }
                AssistantMessageUpdate::ThinkingDelta { delta } => {
                    self.messages.append_thinking_delta(&delta);
                }
                AssistantMessageUpdate::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => {
                    // The first delta for a call carries the provider id;
                    // the rest only carry argument fragments. Remember the
                    // id by index so every fragment lands in one block.
                    let call_id = match id {
                        Some(id) => {
                            self.tool_call_ids.insert(index, id.clone());
                            id
                        }
                        None => self
                            .tool_call_ids
                            .get(&index)
                            .cloned()
                            .unwrap_or_else(|| format!("tool-{index}")),
                    };
                    self.messages.begin_tool_stream(&call_id, name.as_deref());
                    if let Some(delta) = arguments_delta {
                        self.messages.append_tool_stream_args(&call_id, &delta);
                    }
                }
            },
            AgentEvent::MessageEnd { message } => {
                self.messages.end_assistant_stream();
                // Tool execution events that follow carry their own call id,
                // so the index map has done its job and can be dropped.
                self.tool_call_ids.clear();
                // Cumulative session totals plus the latest turn's context
                // size, which drives the footer's context gauge.
                let usage = message.usage;
                self.status_data.add_usage(&usage);
                let context_used = if usage.total > 0 {
                    usage.total
                } else {
                    usage.input + usage.output + usage.cache_read + usage.cache_write
                };
                self.status_data.set_context_used(context_used);
            }
            AgentEvent::ToolExecutionStart { call } => {
                if let Some(renderer) = self.tool_block_renderer.as_mut() {
                    renderer.begin_tool(&call);
                }
                let args = if call.arguments.is_null() {
                    String::new()
                } else {
                    call.arguments.to_string()
                };
                self.messages
                    .start_tool_execution(&call.id, &call.name, &args);
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: _,
                delta,
            } => {
                self.messages.append_assistant_delta(&delta);
            }
            AgentEvent::ToolExecutionEnd {
                result,
                duration_ms,
            } => {
                let is_error = result.is_error;
                let body = match result.content.as_ref() {
                    Content::Text(t) => t.text.clone(),
                    _ => "(binary result)".to_string(),
                };
                // Let the driver's renderer style the block against the
                // viewport we last painted at. Before the first render there
                // is no width, so fall back to a sane 80 columns.
                let width = {
                    let w = self.viewport.viewport_width.load(Ordering::Relaxed);
                    if w == 0 {
                        80
                    } else {
                        w
                    }
                };
                let styled = self
                    .tool_block_renderer
                    .as_mut()
                    .and_then(|renderer| renderer.finish_tool(&result, width));
                self.messages.finish_tool_execution_with_lines(
                    &result.tool_call_id,
                    duration_ms,
                    &body,
                    is_error,
                    styled,
                );
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                if message.stop_reason == pi_protocol::StopReason::Error {
                    self.pending_error = Some("provider returned an error".into());
                }
                self.last_turn_usage = Some(TurnUsage {
                    usage: message.usage,
                    stop_reason: message.stop_reason,
                    trailing: tool_results,
                });
            }
            AgentEvent::UserMessage(_) => {}
            // Run brackets. The TUI renders turn/message state, not the run
            // boundary itself, so these need no UI work — but the extension
            // fan-out hook (if installed) still sees them.
            AgentEvent::AgentStart | AgentEvent::AgentEnd { .. } => {}
            AgentEvent::Error(message) => {
                self.pending_error = Some(message);
            }
        }
    }
}

// =====================================================================
// Public extension subscription surface (additive — does not touch the
// existing App::apply_event pipeline). See module-level docs above.
// =====================================================================

/// Typed subscription hooks for [`AgentEvent`] callbacks.
///
/// Extensions and overlays implement this trait to receive callbacks for
/// each stage of a turn without holding an `&mut App`. Every method has
/// a no-op default so consumers override only what they need.
///
/// The trait is **object-safe** — handlers are stored as
/// `Box<dyn AgentEventHandler>` inside [`AgentEventRouter`].
pub trait AgentEventHandler: Send {
    /// A user message was enqueued at the start of a turn.
    fn on_user_message(&mut self, _msg: &Message) {}
    /// A new assistant message started streaming. `model` is the
    /// provider-issued identifier.
    fn on_assistant_start(&mut self, _model: &str) {}
    /// Incremental assistant text delta.
    fn on_assistant_delta(&mut self, _delta: &str) {}
    /// Incremental assistant thinking delta.
    fn on_thinking_delta(&mut self, _delta: &str) {}
    /// A tool call started executing.
    fn on_tool_call(&mut self, _call: &ToolCall) {}
    /// Incremental tool execution update (partial stdout/stderr).
    fn on_tool_update(&mut self, _tool_call_id: &str, _delta: &str) {}
    /// A tool call finished. `duration_ms` is the wall-clock duration.
    fn on_tool_result(&mut self, _result: &ToolResult, _duration_ms: u64) {}
    /// The assistant message finished streaming.
    fn on_assistant_end(&mut self, _usage: &Usage) {}
    /// A turn finished. `tool_results` is the list of tool-result
    /// messages produced during the turn.
    fn on_turn_end(
        &mut self,
        _usage: &Usage,
        _stop_reason: &StopReason,
        _tool_results: &[Message],
    ) {
    }
    /// A new turn began.
    fn on_turn_start(&mut self) {}
    /// An agent run started (one per `Agent::prompt` call).
    fn on_agent_start(&mut self) {}
    /// An agent run finished.
    fn on_agent_end(&mut self, _messages: &[Message]) {}
    /// The session was compacted. Carries nothing extra today; this
    /// hook is a placeholder so extensions can subscribe before the
    /// richer `SessionCompact` payload lands.
    fn on_session_compacted(&mut self) {}
    /// Agent surface error.
    fn on_error(&mut self, _message: &str) {}
}

/// Fans [`AgentEvent`]s out to a list of [`AgentEventHandler`]
/// subscribers.
///
/// The router owns no state of its own — every `dispatch` call iterates
/// the handler list and forwards the relevant sub-event. The existing
/// [`App::apply_event`] pipeline is untouched; consumers who want the
/// router's events call `router.dispatch(&event)` alongside the drain.
pub struct AgentEventRouter {
    handlers: Vec<Box<dyn AgentEventHandler>>,
}

impl Default for AgentEventRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventRouter {
    /// Construct an empty router.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Append a handler to the fan-out list.
    pub fn push(&mut self, handler: Box<dyn AgentEventHandler>) {
        self.handlers.push(handler);
    }

    /// Number of registered handlers.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// True when no handlers are registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Forward one [`AgentEvent`] to every registered handler.
    ///
    /// Handlers run sequentially in registration order. A handler panics
    /// propagate (we do not swallow them) — extension authors should
    /// keep callbacks total.
    pub fn dispatch(&mut self, event: &AgentEvent) {
        for handler in &mut self.handlers {
            Self::forward(event, handler.as_mut());
        }
    }

    fn forward(event: &AgentEvent, h: &mut dyn AgentEventHandler) {
        match event {
            AgentEvent::UserMessage(msg) => h.on_user_message(msg),
            AgentEvent::MessageStart { model } => h.on_assistant_start(model),
            AgentEvent::MessageUpdate(AssistantMessageUpdate::TextDelta { delta }) => {
                h.on_assistant_delta(delta)
            }
            AgentEvent::MessageUpdate(AssistantMessageUpdate::ThinkingDelta { delta }) => {
                h.on_thinking_delta(delta)
            }
            AgentEvent::MessageUpdate(AssistantMessageUpdate::ToolCallDelta { .. }) => {
                // Tool-call deltas carry argument fragments; the router
                // does not forward them — extensions receive the
                // structured ToolCall via `on_tool_call` instead.
            }
            AgentEvent::MessageEnd { message } => h.on_assistant_end(&message.usage),
            AgentEvent::ToolExecutionStart { call } => h.on_tool_call(call),
            AgentEvent::ToolExecutionUpdate { tool_call_id, delta } => {
                h.on_tool_update(tool_call_id, delta)
            }
            AgentEvent::ToolExecutionEnd {
                result,
                duration_ms,
            } => h.on_tool_result(result, *duration_ms),
            AgentEvent::TurnStart => h.on_turn_start(),
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => h.on_turn_end(&message.usage, &message.stop_reason, tool_results),
            AgentEvent::AgentStart => h.on_agent_start(),
            AgentEvent::AgentEnd { messages } => h.on_agent_end(messages),
            AgentEvent::Error(msg) => h.on_error(msg),
        }
    }
}

/// Factory helper — returns an empty router.
///
/// Equivalent to `AgentEventRouter::new()` but kept as a named
/// constructor so future presets (e.g. a "logger" handler preinstalled)
/// can be added without breaking callers.
pub fn default_router() -> AgentEventRouter {
    AgentEventRouter::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records every callback it sees for later assertion.
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
    }
    impl Recorder {
        fn new(log: Arc<Mutex<Vec<String>>>) -> Self {
            Self { log }
        }
        fn push(&self, line: String) {
            self.log.lock().unwrap().push(line);
        }
    }
    impl AgentEventHandler for Recorder {
        fn on_assistant_start(&mut self, model: &str) {
            self.push(format!("assistant_start:{model}"));
        }
        fn on_assistant_delta(&mut self, delta: &str) {
            self.push(format!("assistant_delta:{delta}"));
        }
        fn on_thinking_delta(&mut self, delta: &str) {
            self.push(format!("thinking_delta:{delta}"));
        }
        fn on_tool_call(&mut self, call: &ToolCall) {
            self.push(format!("tool_call:{}", call.name));
        }
        fn on_tool_result(&mut self, result: &ToolResult, duration_ms: u64) {
            self.push(format!("tool_result:{}:{duration_ms}", result.tool_call_id));
        }
        fn on_assistant_end(&mut self, usage: &Usage) {
            self.push(format!("assistant_end:{}", usage.input + usage.output));
        }
        fn on_turn_end(
            &mut self,
            usage: &Usage,
            stop_reason: &StopReason,
            _tool_results: &[Message],
        ) {
            self.push(format!(
                "turn_end:{}:{:?}",
                usage.input + usage.output,
                stop_reason
            ));
        }
        fn on_agent_start(&mut self) {
            self.push("agent_start".into());
        }
        fn on_agent_end(&mut self, _messages: &[Message]) {
            self.push("agent_end".into());
        }
        fn on_error(&mut self, msg: &str) {
            self.push(format!("error:{msg}"));
        }
    }

    fn text_delta(delta: &str) -> AssistantMessageUpdate {
        AssistantMessageUpdate::TextDelta {
            delta: delta.into(),
        }
    }

    fn thinking_delta(delta: &str) -> AssistantMessageUpdate {
        AssistantMessageUpdate::ThinkingDelta {
            delta: delta.into(),
        }
    }

    #[test]
    fn empty_router_drops_silently() {
        let mut router = default_router();
        assert!(router.is_empty());
        router.dispatch(&AgentEvent::TurnStart);
        router.dispatch(&AgentEvent::Error("boom".into()));
        assert_eq!(router.len(), 0);
    }

    #[test]
    fn single_handler_records_each_event() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut router = default_router();
        router.push(Box::new(Recorder::new(log.clone())));

        router.dispatch(&AgentEvent::AgentStart);
        router.dispatch(&AgentEvent::MessageStart {
            model: "test-model".into(),
        });
        router.dispatch(&AgentEvent::MessageUpdate(text_delta("hi")));
        router.dispatch(&AgentEvent::MessageUpdate(thinking_delta("plan?")));
        router.dispatch(&AgentEvent::ToolExecutionEnd {
            result: ToolResult {
                tool_call_id: "c1".into(),
                content: Box::new(Content::text("done")),
                is_error: false,
                details: None,
                added_tool_names: None,
            },
            duration_ms: 42,
        });
        router.dispatch(&AgentEvent::Error("oops".into()));

        let captured = log.lock().unwrap().clone();
        assert_eq!(
            captured,
            vec![
                "agent_start".to_string(),
                "assistant_start:test-model".to_string(),
                "assistant_delta:hi".to_string(),
                "thinking_delta:plan?".to_string(),
                "tool_result:c1:42".to_string(),
                "error:oops".to_string(),
            ]
        );
    }

    #[test]
    fn multiple_handlers_each_receive_event() {
        let log_a = Arc::new(Mutex::new(Vec::<String>::new()));
        let log_b = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut router = default_router();
        router.push(Box::new(Recorder::new(log_a.clone())));
        router.push(Box::new(Recorder::new(log_b.clone())));

        router.dispatch(&AgentEvent::MessageStart {
            model: "shared".into(),
        });

        assert_eq!(
            log_a.lock().unwrap().clone(),
            vec!["assistant_start:shared".to_string()]
        );
        assert_eq!(
            log_b.lock().unwrap().clone(),
            vec!["assistant_start:shared".to_string()]
        );
        assert_eq!(router.len(), 2);
    }

    #[test]
    fn tool_call_dispatch_forwards_structured_call() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut router = default_router();
        router.push(Box::new(Recorder::new(log.clone())));

        router.dispatch(&AgentEvent::ToolExecutionStart {
            call: ToolCall {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            },
        });

        assert_eq!(
            log.lock().unwrap().clone(),
            vec!["tool_call:bash".to_string()]
        );
    }

    #[test]
    fn default_noop_handlers_are_zero_cost_to_skip() {
        // A handler that only overrides on_error must not receive any
        // other callback.
        struct OnlyError {
            log: Arc<Mutex<Vec<String>>>,
        }
        impl AgentEventHandler for OnlyError {
            fn on_error(&mut self, msg: &str) {
                self.log.lock().unwrap().push(format!("error:{msg}"));
            }
        }

        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut router = default_router();
        router.push(Box::new(OnlyError { log: log.clone() }));

        router.dispatch(&AgentEvent::TurnStart);
        router.dispatch(&AgentEvent::MessageUpdate(text_delta("ignored")));
        router.dispatch(&AgentEvent::Error("captured".into()));

        assert_eq!(
            log.lock().unwrap().clone(),
            vec!["error:captured".to_string()]
        );
    }
}

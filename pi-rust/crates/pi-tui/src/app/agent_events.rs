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

use std::sync::atomic::Ordering;

use pi_agent_core::{AgentEvent, AssistantMessageUpdate};
use pi_protocol::Content;
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

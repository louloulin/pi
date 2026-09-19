//! Top-level [`App`] — owns the editor / message view / status bar and
//! drives the render loop.
//!
//! The App is intentionally framework-agnostic: it operates on the
//! [`InputEvent`] enum and a [`ratatui::buffer::Buffer`] view, so unit
//! tests can drive the full loop with synthetic input and assert on
//! the rendered buffer without ever touching a real terminal.
//!
//! Concrete binaries embed the App in a `crossterm`-backed
//! [`ratatui::Terminal`] and feed it events from the terminal input.
//! See `crates/pi-coding-agent/src/interactive.rs` for the wiring.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::Duration;

use crossterm::event::Event as CtEvent;
use parking_lot::Mutex;
use pi_agent_core::{Agent, AgentEvent, AssistantMessageUpdate};
use pi_protocol::{Content, Message, Usage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::dialog::{Dialog, DialogAction, DialogKind};
use crate::editor::EditorAction;
use crate::input::{InputEvent, Key, KeyCode, KeyModifiers};
use crate::message::{MessageItem, MessageView};
use crate::prompt::{Prompt, PromptAction};
use crate::selector::{Selector, SelectorAction, SelectorItem};
use crate::status::{StatusBar, StatusData};
use crate::styled::write_styled_line;
use crate::theme::{builtin_theme, load_theme, ColorMode, Theme, ThemeError};

/// Configuration knobs for the App.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Placeholder shown in the prompt when it is empty.
    pub prompt_placeholder: String,
    /// Session identifier shown in the status bar.
    pub session_id: String,
    /// Polling interval for `crossterm::event::poll` (microseconds). The
    /// TUI uses this to throttle the render loop when there is no
    /// input.
    pub event_poll_interval: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            prompt_placeholder: "type a prompt — /help for commands".to_string(),
            session_id: "local".to_string(),
            event_poll_interval: Duration::from_millis(50),
        }
    }
}

/// Provider usage reported by the most recently finished agent turn.
///
/// Rust [`Message`] rows carry no provider usage, and `pi-tui` cannot
/// depend on `pi-coding-agent` (the dependency runs the other way), so
/// the driver cannot recover the last turn's usage from the agent's
/// message log. [`App::drain_agent_events`] records it here instead and
/// drivers consume it with [`App::take_turn_usage`] — `pi-coding-agent`
/// uses it to decide whether automatic compaction should run.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsage {
    /// Usage reported by the turn's final assistant message.
    pub usage: Usage,
    /// Messages that followed that assistant message in the turn (the
    /// tool results it produced), for `context_tokens_with_trailing`.
    pub trailing: Vec<Message>,
}

/// Outcome returned by [`App::step`] after each key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Step made no meaningful state change.
    Idle,
    /// Step mutated the rendered state.
    Redraw,
    /// User submitted a prompt. The caller is responsible for handing
    /// the text to the agent.
    Submitted(String),
    /// User pressed Ctrl+C / Ctrl+D — the caller should shut the App
    /// down and (optionally) fall back to print mode.
    Exit,
}

/// Snapshot of the rendered App for tests.
#[derive(Debug, Clone)]
pub struct RenderSnapshot {
    /// Width the snapshot was rendered at.
    pub width: u16,
    /// Height the snapshot was rendered at.
    pub height: u16,
    /// Pre-rendered content lines, top to bottom.
    pub lines: Vec<String>,
    /// Currently displayed placeholder text (empty if the prompt has
    /// a buffer).
    pub prompt_placeholder: String,
    /// Buffer text the editor currently shows.
    pub prompt_buffer: String,
    /// Whether the selector modal is currently visible.
    pub selector_open: bool,
    /// Selector title (when open).
    pub selector_title: Option<String>,
    /// Selector items (when open).
    pub selector_items: Vec<SelectorItem>,
    /// Selector cursor index (when open).
    pub selector_cursor: Option<usize>,
    /// Whether a modal extension dialog is currently visible.
    pub dialog_open: bool,
    /// Flavour of the open dialog (when open).
    pub dialog_kind: Option<DialogKind>,
    /// Title of the open dialog (when open).
    pub dialog_title: Option<String>,
    /// Rendered dialog lines (when open).
    pub dialog_lines: Vec<String>,
    /// Status bar snapshot.
    pub status: StatusData,
}

impl RenderSnapshot {
    /// Convert the lines into a `Buffer` so callers can compare against
    /// `Buffer::with_lines` results.
    pub fn to_buffer(&self) -> Buffer {
        Buffer::with_lines(self.lines.iter().map(String::as_str))
    }
}

/// Top-level App.
pub struct App {
    config: AppConfig,
    prompt: Prompt,
    messages: MessageView,
    status_bar: StatusBar,
    status_data: StatusData,
    /// The active palette the buffer render path consumes. Swapping it with
    /// [`App::set_theme`] takes effect on the next [`App::render_to_buffer`]
    /// call — no rebuild, no restart.
    theme: Theme,
    selector: Option<Selector>,
    /// Modal requested by a JS extension (`ctx.ui.confirm` / `input` /
    /// `select`) that is waiting for a key press.
    dialog: Option<Dialog>,
    /// Channel carrying dialogs from the extension host. Only set in
    /// interactive mode; see `pi-coding-agent`'s extension UI bridge.
    ui_dialogs: Option<mpsc::UnboundedReceiver<Dialog>>,
    /// Live subscription to agent events. Constructed in
    /// [`App::new`] from `agent.subscribe()` so the App receives every
    /// event the agent emits across all turns.
    event_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Cancellation token for the in-flight prompt, if any.
    cancel_token: Option<CancellationToken>,
    /// Last user-facing error surfaced by the agent loop. Rendered
    /// into the message view on the next step.
    pending_error: Option<String>,
    /// Usage of the most recently finished turn, recorded by
    /// [`App::drain_agent_events`] and consumed by
    /// [`App::take_turn_usage`].
    last_turn_usage: Option<TurnUsage>,
    /// Shared liveness flag for the in-flight `submit` task. The task
    /// clears it when `Agent::prompt` returns, so [`App::is_busy`]
    /// answers "a turn is running" instead of "a turn was ever
    /// started".
    turn_busy: Arc<AtomicBool>,
    /// Set when the App should exit at the next opportunity. The TUI
    /// exit path checks this between key events.
    exit_requested: bool,
    /// Width of the message viewport as of the last render. Scroll keys use
    /// it to wrap the log exactly like the renderer does, so a "page" is a
    /// real screenful.
    viewport_width: AtomicU16,
    /// Height of the message viewport as of the last render; the page size
    /// for `PageUp` / `PageDown`.
    viewport_height: AtomicU16,
}

impl App {
    /// Construct an App over an [`Agent`] handle. The App subscribes
    /// to the agent's event stream immediately and drains events
    /// during [`App::drain_agent_events`] (called by the render
    /// loop).
    pub fn new(agent: &Agent, config: AppConfig) -> Self {
        let mut status_data = StatusData::new(
            agent
                .model()
                .label
                .clone()
                .unwrap_or_else(|| agent.model().id.clone()),
            config.session_id.clone(),
        );
        status_data.hint = Some("? for help".to_string());
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder(config.prompt_placeholder.clone());
        let event_rx = agent.subscribe();
        Self {
            config,
            prompt,
            messages: MessageView::new(),
            status_bar: StatusBar::new(),
            status_data,
            theme: builtin_theme("dark", ColorMode::TrueColor)
                .expect("built-in dark theme is valid"),
            selector: None,
            dialog: None,
            ui_dialogs: None,
            event_rx: Some(event_rx),
            cancel_token: None,
            pending_error: None,
            last_turn_usage: None,
            turn_busy: Arc::new(AtomicBool::new(false)),
            exit_requested: false,
            viewport_width: AtomicU16::new(0),
            viewport_height: AtomicU16::new(0),
        }
    }

    /// Borrow the message view (for tests and snapshots).
    pub fn messages(&self) -> &MessageView {
        &self.messages
    }

    /// Mutable borrow of the message view.
    pub fn messages_mut(&mut self) -> &mut MessageView {
        &mut self.messages
    }

    /// Borrow the prompt.
    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Mutable borrow of the prompt.
    pub fn prompt_mut(&mut self) -> &mut Prompt {
        &mut self.prompt
    }

    /// Borrow the status data.
    pub fn status_data(&self) -> &StatusData {
        &self.status_data
    }

    /// Mutable borrow of the status data.
    pub fn status_data_mut(&mut self) -> &mut StatusData {
        &mut self.status_data
    }

    /// The palette the buffer render path currently consumes.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Install a theme. The next [`App::render_to_buffer`] (or
    /// [`App::render_snapshot`]) reflects it immediately — hot-swapping does
    /// not require rebuilding the App.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Load and install a theme by name, keeping the current colour mode.
    ///
    /// Built-in names (`dark`, `light`) always resolve; custom themes are not
    /// searched because the App does not own a custom-themes directory. On
    /// failure the previous theme is left untouched.
    pub fn set_theme_by_name(&mut self, name: &str) -> Result<(), ThemeError> {
        let theme = load_theme(name, self.theme.color_mode(), None)?;
        self.theme = theme;
        Ok(())
    }

    /// Whether the user has requested an exit.
    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Whether a background agent turn is currently in flight.
    pub fn is_busy(&self) -> bool {
        self.turn_busy.load(Ordering::SeqCst)
    }

    /// Usage of the most recently finished turn.
    ///
    /// Read-only peek at what [`App::take_turn_usage`] would return;
    /// it does not consume the value, so a driver that peeks during a
    /// render pass can still take it later.
    pub fn last_turn_usage(&self) -> Option<&TurnUsage> {
        self.last_turn_usage.as_ref()
    }

    /// Take the usage of the most recently finished turn.
    ///
    /// Returns `None` when no turn has finished since the last call.
    /// The value is cleared so a driver reacts once per turn; a later
    /// `TurnEnd` replaces it, so an intermediate turn is never acted on
    /// while the prompt that produced it is still running.
    pub fn take_turn_usage(&mut self) -> Option<TurnUsage> {
        self.last_turn_usage.take()
    }

    /// Set a model override that takes effect on the next
    /// `Agent::prompt` call.
    pub fn queue_model_switch(&mut self, agent: &mut Agent, model: pi_protocol::Model) {
        self.status_data.model = model.label.clone().unwrap_or_else(|| model.id.clone());
        agent.set_model(model);
    }

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

    /// Apply a single [`AgentEvent`] to the message view + status bar.
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {}
            AgentEvent::MessageStart { model } => {
                self.messages.begin_assistant_stream(&model);
            }
            AgentEvent::MessageUpdate(update) => match update {
                AssistantMessageUpdate::TextDelta { delta } => {
                    self.messages.append_assistant_delta(&delta);
                }
                AssistantMessageUpdate::ThinkingDelta { .. } => {
                    // Collapsed thinking — not rendered by Stage 4.
                }
                AssistantMessageUpdate::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta: _,
                } => {
                    let label = name.unwrap_or_else(|| format!("tool-{index}"));
                    let _ = id; // placeholder — Stage 4 collapses into a single block
                    self.messages.push_tool(&label, "(streaming)", "", false);
                }
            },
            AgentEvent::MessageEnd { message } => {
                self.messages.end_assistant_stream();
                self.status_data
                    .add_tokens(message.usage.input, message.usage.output);
            }
            AgentEvent::ToolExecutionStart { call } => {
                self.messages.push_tool(&call.name, "(running)", "", false);
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
                let name = String::new();
                let is_error = result.is_error;
                let body = match result.content.as_ref() {
                    Content::Text(t) => t.text.clone(),
                    _ => "(binary result)".to_string(),
                };
                self.messages
                    .push_tool(&name, &format!("{duration_ms}ms"), &body, is_error);
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
                    trailing: tool_results,
                });
            }
            AgentEvent::UserMessage(_) => {}
            AgentEvent::Error(message) => {
                self.pending_error = Some(message);
            }
        }
    }

    /// Pop the last pending error (if any). The TUI prints this on
    /// the next render so the user sees the agent surface error.
    pub fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    /// Begin an agent turn asynchronously. The App spawns a tokio
    /// task that calls `Agent::prompt`; events flow through the
    /// subscriber channel established in [`App::new`] and are drained
    /// by [`App::drain_agent_events`].
    pub fn submit(&mut self, agent: Arc<AsyncMutex<Agent>>, text: String) {
        if self.turn_busy.load(Ordering::SeqCst) {
            return; // already busy
        }
        if self.event_rx.is_none() {
            // App constructed without a subscription — re-establish one.
            if let Ok(guard) = agent.try_lock() {
                self.event_rx = Some(guard.subscribe());
            }
        }
        self.messages.push(MessageItem::user(&text));
        self.prompt.push_history(&text);
        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());
        self.turn_busy.store(true, Ordering::SeqCst);

        let busy = self.turn_busy.clone();
        let cancel_for_task = cancel.clone();
        let agent_clone = agent.clone();
        let text_clone = text.clone();
        tokio::spawn(async move {
            let mut guard = agent_clone.lock().await;
            let result = guard.prompt(&text_clone).await;
            drop(guard);
            if let Err(err) = result {
                let _ = cancel_for_task; // keep the cancellation alive until drop
                let guard = agent_clone.lock().await;
                guard.emit(AgentEvent::Error(err.to_string()));
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Cancel the in-flight turn (if any).
    pub fn cancel(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }

    /// Open the model selector. The selector lists the supplied
    /// candidates; the App does not interpret `value` — the caller
    /// closes the selector with [`App::close_selector`] and applies
    /// the picked value.
    pub fn open_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Close the selector and return the picked value if any.
    pub fn close_selector(&mut self) -> Option<Selector> {
        self.selector.take()
    }

    /// Replace the active selector — used by the binary entry point
    /// when it wants to mutate the selector state in response to a key
    /// event without taking it out of the App.
    pub fn replace_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Whether the selector modal is currently visible.
    pub fn selector_open(&self) -> bool {
        self.selector.is_some()
    }

    /// Attach the channel that carries extension dialogs.
    ///
    /// The interactive entry point calls this with the receiver half of
    /// the extension UI bridge; [`App::poll_ui_dialogs`] then turns each
    /// envelope into a modal.
    pub fn attach_ui_dialogs(&mut self, rx: mpsc::UnboundedReceiver<Dialog>) {
        self.ui_dialogs = Some(rx);
    }

    /// Show `dialog` and wait for the user.
    ///
    /// Returns `false` without showing it when another modal is already
    /// on screen — the dialog is answered with its cancel default so the
    /// extension never waits on a prompt nobody can see.
    pub fn open_dialog(&mut self, mut dialog: Dialog) -> bool {
        if self.dialog.is_some() {
            dialog.cancel();
            return false;
        }
        self.dialog = Some(dialog);
        true
    }

    /// Whether a modal extension dialog is on screen.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Borrow the open dialog.
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    /// Take the open dialog out of the App — used by the entry point
    /// after it resolved the answer itself.
    pub fn take_dialog(&mut self) -> Option<Dialog> {
        self.dialog.take()
    }

    /// Drain queued extension UI requests.
    ///
    /// `notify` requests become message-view lines (they are
    /// fire-and-forget), everything else becomes a modal. A modal whose
    /// host stopped waiting is closed. Returns whether the App changed
    /// and the caller should redraw.
    pub fn poll_ui_dialogs(&mut self) -> bool {
        let mut changed = false;
        if let Some(dialog) = &self.dialog {
            if dialog.is_abandoned() {
                self.dialog = None;
                changed = true;
            }
        }
        loop {
            let dialog = match self.ui_dialogs.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(dialog) => dialog,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.ui_dialogs = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            if dialog.kind() == DialogKind::Notify {
                let mut dialog = dialog;
                if let Some((message, level)) = dialog.notify_text() {
                    self.messages.push_info(format!("[{level:?}] {message}"));
                }
                dialog.resolve(Some(pi_protocol::UiResponse::NotifyAck));
                continue;
            }
            // A rejected dialog is answered with its cancel default by
            // `open_dialog`; the sender dropping would do the same, but
            // sending is explicit.
            let _ = self.open_dialog(dialog);
        }
        changed
    }

    /// Borrow the active selector (if any).
    pub fn selector(&self) -> Option<&Selector> {
        self.selector.as_ref()
    }

    /// Process a single [`InputEvent`]. Returns the outcome so the
    /// caller can decide whether to redraw.
    pub fn step(&mut self, event: InputEvent) -> StepOutcome {
        if self.exit_requested {
            return StepOutcome::Exit;
        }
        // An open modal owns the keyboard: the selector and prompt stay
        // frozen underneath it.
        if self.dialog.is_some() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            return self.step_dialog(key);
        }
        // Selector gets first dibs on keys when it is open.
        if let Some(selector) = self.selector.as_mut() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            match selector.handle_key(key) {
                SelectorAction::None => return StepOutcome::Idle,
                SelectorAction::Changed => return StepOutcome::Redraw,
                SelectorAction::Selected(_) | SelectorAction::Cancelled => {
                    return StepOutcome::Redraw;
                }
            }
        }
        let InputEvent::Key(key) = event else {
            return StepOutcome::Idle;
        };
        self.step_key(key)
    }

    /// Process a single [`Key`]. Public so tests can step the App
    /// with explicit keys.
    pub fn step_key(&mut self, key: Key) -> StepOutcome {
        // A modal dialog swallows every key — including Ctrl+C / Esc,
        // which cancel the dialog instead of the turn or the App.
        if self.dialog.is_some() {
            return self.step_dialog(key);
        }
        // Global keys first.
        match key {
            // Esc cancels the in-flight turn, otherwise dismisses
            // selectors (handled above) or is a no-op.
            Key {
                code: KeyCode::Esc,
                modifiers,
            } if modifiers.is_empty() && self.is_busy() => {
                self.cancel();
                return StepOutcome::Redraw;
            }
            // Global Ctrl+C.
            Key {
                code: KeyCode::Char('c'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => {
                if self.is_busy() {
                    self.cancel();
                    return StepOutcome::Redraw;
                }
                self.exit_requested = true;
                return StepOutcome::Exit;
            }
            // Global Ctrl+L clears the screen.
            Key {
                code: KeyCode::Char('l'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => {
                self.messages.clear();
                return StepOutcome::Redraw;
            }
            // Fullscreen chat-log scrolling. Upstream deliberately shadows
            // the bare editor bindings for these chords in fullscreen mode
            // (`packages/tui/src/keybindings.ts:159-165,208-209`: "These
            // intentionally shadow the unmodified editor bindings in
            // fullscreen mode"); `Ctrl+A` / `Ctrl+E` still reach the editor
            // for start / end of line.
            Key {
                code: KeyCode::PageUp,
                modifiers,
            } if modifiers.is_empty() => {
                let page = self.message_page();
                return if self.scroll_viewport_up(page) {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            Key {
                code: KeyCode::PageDown,
                modifiers,
            } if modifiers.is_empty() => {
                let page = self.message_page();
                return if self.scroll_viewport_down(page) {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            // `tui.altScreen.top` / `tui.altScreen.bottom`.
            Key {
                code: KeyCode::Home,
                modifiers,
            } if modifiers.is_empty() => {
                return if self.scroll_viewport_to_top() {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            Key {
                code: KeyCode::End,
                modifiers,
            } if modifiers.is_empty() => {
                return if self.scroll_viewport_to_bottom() {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            _ => {}
        }

        match self.prompt.handle_key(key) {
            PromptAction::None => StepOutcome::Idle,
            PromptAction::Changed => StepOutcome::Redraw,
            PromptAction::Submit(text) => {
                // Caller is responsible for invoking `submit` with an
                // `Arc<AsyncMutex<Agent>>` — we just announce the
                // submitted text and clear the buffer.
                let submitted = text.clone();
                self.prompt.clear();
                StepOutcome::Submitted(submitted)
            }
            PromptAction::Interrupt => {
                self.cancel();
                StepOutcome::Redraw
            }
            PromptAction::Eof => {
                self.exit_requested = true;
                StepOutcome::Exit
            }
        }
    }

    /// Route a key to the open dialog.
    fn step_dialog(&mut self, key: Key) -> StepOutcome {
        let Some(dialog) = self.dialog.as_mut() else {
            return StepOutcome::Idle;
        };
        match dialog.handle_key(key) {
            DialogAction::None => StepOutcome::Idle,
            DialogAction::Changed => StepOutcome::Redraw,
            // The answer already travelled to the host over the
            // dialog's reply channel; the modal just closes.
            DialogAction::Resolved(_) => {
                self.dialog = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Mark the App for exit (e.g. after `/exit`).
    pub fn request_exit(&mut self) {
        self.exit_requested = true;
    }

    /// Append a free-form info line to the message view (used by
    /// slash commands to print help / errors).
    pub fn info(&mut self, text: impl Into<String>) {
        self.messages.push_info(text);
    }

    /// Size of the message viewport as of the last render — `(0, 0)`
    /// before the first one.
    pub fn viewport(&self) -> (u16, u16) {
        (
            self.viewport_width.load(Ordering::Relaxed),
            self.viewport_height.load(Ordering::Relaxed),
        )
    }

    /// Lines per page — one message-viewport height, but never zero so a
    /// key press before the first render is still a no-op instead of a
    /// panic.
    fn message_page(&self) -> usize {
        let (_, height) = self.viewport();
        height.max(1) as usize
    }

    /// Largest valid scroll offset for the viewport the App last rendered.
    fn max_scroll(&self) -> usize {
        let (width, height) = self.viewport();
        if height == 0 {
            return 0;
        }
        self.messages
            .line_count(width)
            .saturating_sub(height as usize)
    }

    /// Current scroll offset, resolving `MessageView::scroll_to_top`'s
    /// `usize::MAX` sentinel against the last rendered viewport so it can
    /// be scrolled back down from.
    fn resolved_scroll(&self) -> usize {
        let max = self.max_scroll();
        match self.messages.scroll_offset() {
            usize::MAX => max,
            offset => offset.min(max),
        }
    }

    /// Scroll the chat log up (towards older output) by `lines`. Returns
    /// true when the viewport actually moved, so the caller can skip a
    /// redraw.
    ///
    /// Mirrors `tui.altScreen.pageUp` / `lineUp`
    /// (`packages/tui/src/keybindings.ts:160-176`) — the fullscreen TUI
    /// owns the scrollback because the alternate screen hides the
    /// terminal's own.
    pub fn scroll_viewport_up(&mut self, lines: usize) -> bool {
        let next = self
            .resolved_scroll()
            .saturating_add(lines.max(1))
            .min(self.max_scroll());
        if next == self.resolved_scroll() {
            return false;
        }
        self.messages.set_scroll_from_bottom(next);
        true
    }

    /// Scroll the chat log down (towards the tail) by `lines`. Reaching
    /// the tail re-attaches the viewport to new output. Returns true when
    /// the viewport actually moved.
    pub fn scroll_viewport_down(&mut self, lines: usize) -> bool {
        let current = self.resolved_scroll();
        if current == 0 {
            return false;
        }
        // `set_scroll_from_bottom(0)` also re-attaches to the tail.
        self.messages
            .set_scroll_from_bottom(current.saturating_sub(lines.max(1)));
        true
    }

    /// Jump the chat log to the oldest line — `tui.altScreen.top`.
    pub fn scroll_viewport_to_top(&mut self) -> bool {
        let max = self.max_scroll();
        if max == 0 || (self.resolved_scroll() == max && !self.messages.is_following()) {
            return false;
        }
        self.messages.set_scroll_from_bottom(max);
        true
    }

    /// Jump the chat log back to the tail — `tui.altScreen.bottom`. New
    /// output pins the viewport again.
    pub fn scroll_viewport_to_bottom(&mut self) -> bool {
        if self.messages.scroll_offset() == 0 {
            return false;
        }
        self.messages.scroll_to_bottom();
        true
    }

    /// Render the App into a `Buffer` at the given area.
    pub fn render_to_buffer(&self, area: Rect, buf: &mut Buffer) {
        // Layout: message view fills the top, prompt the bottom row,
        // status bar the row above the prompt.
        let status_height = 1u16;
        let prompt_height = 1u16;
        let message_height = area.height.saturating_sub(status_height + prompt_height);
        let message_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: message_height,
        };
        let status_area = Rect {
            x: area.x,
            y: area.y + message_height,
            width: area.width,
            height: status_height,
        };
        let prompt_area = Rect {
            x: area.x,
            y: area.y + message_height + status_height,
            width: area.width,
            height: prompt_height,
        };

        // Record the viewport the scroll keys clamp against. Keys arrive
        // between renders, so the previous render's geometry is what they
        // see — exactly what the reader was looking at.
        self.viewport_width.store(area.width, Ordering::Relaxed);
        self.viewport_height
            .store(message_height, Ordering::Relaxed);

        self.messages
            .render_to_buffer_themed(message_area, buf, &self.theme);
        self.status_bar
            .render_to_buffer_themed(&self.status_data, status_area, buf, &self.theme);

        // Prompt line.
        let line = self.prompt.render_line(area.width);
        for (col, ch) in line.chars().enumerate() {
            let x = prompt_area.x + col as u16;
            if x >= prompt_area.x + prompt_area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, prompt_area.y)) {
                cell.set_char(ch);
            }
        }

        // Selector overlay — when open, draw on top of everything
        // except the prompt and status.
        if let Some(selector) = &self.selector {
            let lines = selector.render_styled_lines(area.width);
            let start_row = area.y + 1;
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                write_styled_line(buf, area.x, y, area.width, line, &self.theme);
            }
        }

        // Extension dialog overlay — topmost, so it wins over the
        // selector if both are somehow open.
        if let Some(dialog) = &self.dialog {
            let lines = dialog.render_lines(area.width);
            let start_row = area.y;
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                // Blank the row first: a modal must be readable even
                // when the message view underneath is full of text.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char(' ');
                    }
                }
                for (col, ch) in line.chars().enumerate() {
                    let x = area.x + col as u16;
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                    }
                }
            }
        }
    }

    /// Render the App into a flat snapshot (used by the snapshot tests
    /// in `tests/snapshot.rs`).
    pub fn render_snapshot(&self, width: u16, height: u16) -> RenderSnapshot {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        self.render_to_buffer(area, &mut buf);
        let lines = buf
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>();
        RenderSnapshot {
            width,
            height,
            lines,
            prompt_placeholder: self.config.prompt_placeholder.clone(),
            prompt_buffer: self.prompt.text().to_string(),
            selector_open: self.selector_open(),
            selector_title: self.selector.as_ref().map(|s| s.title().to_string()),
            selector_items: self
                .selector
                .as_ref()
                // The rows that pass the selector's search filter (the
                // scroll window is a rendering detail and is applied by
                // `Selector::render_lines`).
                .map(|s| s.visible_items().cloned().collect())
                .unwrap_or_default(),
            selector_cursor: self.selector.as_ref().map(|s| s.cursor()),
            dialog_open: self.dialog.is_some(),
            dialog_kind: self.dialog.as_ref().map(|d| d.kind()),
            dialog_title: self.dialog.as_ref().map(|d| d.title().to_string()),
            dialog_lines: self
                .dialog
                .as_ref()
                .map(|d| d.render_lines(width))
                .unwrap_or_default(),
            status: self.status_data.clone(),
        }
    }

    /// Convert a raw `crossterm` event into an [`InputEvent`]. Used by
    /// the binary entry point.
    pub fn translate_event(event: CtEvent) -> InputEvent {
        match event {
            CtEvent::Key(key) => InputEvent::from(key),
            CtEvent::Resize(w, h) => InputEvent::Resize {
                width: w,
                height: h,
            },
            _ => InputEvent::Ignored,
        }
    }

    /// Translate a slice of `crossterm` events.
    pub fn translate_events<I: IntoIterator<Item = CtEvent>>(events: I) -> Vec<InputEvent> {
        events.into_iter().map(Self::translate_event).collect()
    }
}

// Quiet unused-import warning for the parking_lot::Mutex, which we
// keep available for downstream code that wants to wrap state behind
// the App handle.
#[allow(dead_code)]
fn _keep_mutex_path() -> Arc<Mutex<()>> {
    Arc::new(Mutex::new(()))
}

// Quiet unused-editor-action warning — the import is there for the
// public method signature contract.
#[allow(dead_code)]
const _: EditorAction = EditorAction::None;

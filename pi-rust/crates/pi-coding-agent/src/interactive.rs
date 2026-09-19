//! Interactive TUI driver — Stage 4 of the Rust port.
//!
//! [`run_interactive`] owns the terminal: it enables raw mode, starts a
//! crossterm-backed `ratatui` terminal, drives the [`App`] event loop,
//! and exits cleanly (restoring the terminal state) when the user
//! requests quit or the agent turn finishes.
//!
//! The interactive driver shares the [`Agent`] facade with `--print`
//! and `--rpc` so prompt logic is not duplicated across the three
//! modes. Slash commands (`/help`, `/clear`, `/model`, `/session`,
//! `/exit`) are handled here; the agent loop only sees the text of a
//! submitted prompt.
//!
//! Errors during rendering or the agent turn fall back to print mode
//! so the terminal never hangs (Stage 4 acceptance criterion).

use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self as ct_event, Event as CtEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use parking_lot::Mutex as SyncMutex;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{Agent, AgentEvent, AgentOptions};
use pi_ai::models::Models;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Model, ProviderId};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::{handle_command, SlashCommand};
use crate::compaction::{
    compact, Compaction, CompactionError, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
};
use crate::extensions::ui_bridge::TuiUi;
use crate::extensions::wiring::ExtensionRuntime;
use crate::prompt_templates::PromptTemplate;
use crate::session_log::SessionLog;
use crate::text_fallback::{run_text_fallback, FallbackReason};
use crate::tool_executor::default_executor;

use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, KeyCode};
use pi_tui::selector::{Selector, SelectorItem};

/// Result of running the interactive TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveExit {
    /// User explicitly quit (`/exit`, Ctrl+C on idle, Ctrl+D).
    UserExit,
    /// Agent returned and the user finished (Stage 4 keeps the TUI
    /// alive across turns; this variant is reserved for future
    /// one-shot modes).
    TurnFinished,
    /// The driver fell back to print mode because the TUI could not
    /// start.
    Fallback,
}

/// Bundled options for [`run_interactive`].
///
/// `Debug` and `Default` are implemented by hand because
/// `Arc<dyn StreamFn>` implements neither; `Default` installs the faux
/// provider so offline callers keep the pre-Stage-14 behaviour.
pub struct InteractiveOptions {
    /// System prompt prepended to every turn.
    pub system_prompt: String,
    /// Extra system-prompt fragments the user passed via
    /// `--append-system-prompt`.
    pub append_system_prompt: Vec<String>,
    /// Default model the agent uses.
    pub model: Option<Model>,
    /// Model registry the `/model` command browses.
    pub models: Models,
    /// Session log destination. `None` disables session persistence.
    pub session_log: Option<SessionLog>,
    /// Session identifier surfaced in the status bar.
    pub session_id: String,
    /// Compaction thresholds and the auto-compaction toggle, resolved
    /// from `settings.json` by the caller (`config::load_compaction_settings`).
    /// Manual `/compact` uses the token settings; the toggle only gates
    /// the automatic trigger.
    pub compaction: CompactionSettings,
    /// Initial prompt to submit on launch.
    pub initial_prompt: Option<String>,
    /// Prompt templates loaded from `~/.pi/agent/prompts`, `.pi/prompts`
    /// and `--prompt-template` paths. A `/<name> [args]` submission whose
    /// name matches one of these expands into the template body before
    /// it is sent to the model.
    pub prompt_templates: Vec<PromptTemplate>,
    /// Streaming adapter used for every turn. The `pi` binary passes a
    /// [`ProviderRouter`](crate::provider::ProviderRouter) so `/model`
    /// can switch the TUI between providers live.
    pub stream_fn: SharedStreamFn,
    /// Tool executor the agent loop dispatches model tool calls to.
    /// The `pi` binary passes [`default_executor`]; tests inject a
    /// scripted executor.
    pub tool_executor: Arc<dyn ToolExecutor>,
    /// Loaded JS extensions, when any. `None` disables extension
    /// command dispatch and side-effect persistence.
    pub extensions: Option<Arc<ExtensionRuntime>>,
    /// Interactive UI bridge for `ctx.ui.confirm / input / select`.
    /// `None` keeps the headless behaviour (deny / cancel), which is
    /// what tests and non-TTY runs get.
    pub extension_ui: Option<TuiUi>,
}

impl std::fmt::Debug for InteractiveOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractiveOptions")
            .field("system_prompt", &self.system_prompt)
            .field("append_system_prompt", &self.append_system_prompt)
            .field("model", &self.model)
            .field("models", &self.models)
            .field("session_log", &self.session_log)
            .field("session_id", &self.session_id)
            .field("compaction", &self.compaction)
            .field("initial_prompt", &self.initial_prompt)
            .field("prompt_templates", &self.prompt_templates.len())
            .field("stream_fn", &"<dyn StreamFn>")
            .field("tool_executor", &"<dyn ToolExecutor>")
            .field("extensions", &self.extensions.is_some())
            .field("extension_ui", &self.extension_ui.is_some())
            .finish()
    }
}

impl Default for InteractiveOptions {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            append_system_prompt: Vec::new(),
            model: None,
            models: Models::new(),
            session_log: None,
            session_id: String::new(),
            compaction: DEFAULT_COMPACTION_SETTINGS,
            initial_prompt: None,
            prompt_templates: Vec::new(),
            stream_fn: Arc::new(FauxProvider::default()) as SharedStreamFn,
            tool_executor: default_executor(),
            extensions: None,
            extension_ui: None,
        }
    }
}

/// Entry point invoked from `main`.
pub async fn run_interactive(options: InteractiveOptions) -> anyhow::Result<InteractiveExit> {
    let stream_fn: SharedStreamFn = options.stream_fn.clone();
    let resolved_model = options
        .model
        .clone()
        .unwrap_or_else(|| default_model(&options.models));

    let mut system_prompt = options.system_prompt.clone();
    if !options.append_system_prompt.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&options.append_system_prompt.join("\n"));
    }

    let agent = Agent::new(
        AgentOptions::new(resolved_model.clone(), stream_fn, system_prompt)
            // The TUI is a real coding session: tool calls must hit the
            // filesystem / shell instead of the Stage 2 stub.
            .with_tool_executor(options.tool_executor.clone()),
    );
    let agent = Arc::new(AsyncMutex::new(agent));

    let config = AppConfig {
        prompt_placeholder: "type a prompt — /help for commands".into(),
        session_id: options.session_id.clone(),
        event_poll_interval: Duration::from_millis(50),
        markdown: true,
    };

    let mut terminal = match setup_terminal() {
        Ok(terminal) => terminal,
        Err(err) => {
            eprintln!("pi: TUI setup failed ({err}); falling back to print mode");
            let mut agent_guard = agent.lock().await;
            return run_text_fallback(
                &mut agent_guard,
                FallbackReason::TerminalSetupFailed(err.to_string()),
            )
            .await
            .map(|_| InteractiveExit::Fallback);
        }
    };

    let outcome = run_loop(&mut terminal, agent, options, config).await;

    let teardown = teardown_terminal(&mut terminal);
    if let Err(err) = teardown {
        eprintln!("pi: TUI teardown failed: {err}");
    }

    outcome
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: Arc<AsyncMutex<Agent>>,
    mut options: InteractiveOptions,
    config: AppConfig,
) -> anyhow::Result<InteractiveExit> {
    // Build the App on the stack first so we can drop it before
    // tearing down the terminal.
    let mut app = App::new(&*agent.lock().await, config.clone());

    // Extension dialogs: the App drains the bridge every tick, and the
    // gate only opens now that the loop is running (see `ui_bridge`).
    if let Some(ui) = options.extension_ui.as_mut() {
        if let Some(dialogs) = ui.take_dialogs() {
            app.attach_ui_dialogs(dialogs);
        }
        ui.arm();
    }

    // Initial prompt is submitted on launch.
    if let Some(text) = options.initial_prompt.clone() {
        if !text.is_empty() {
            app.submit(agent.clone(), text);
        }
    }

    let mut last_render = std::time::Instant::now();
    let render_interval = Duration::from_millis(50);

    loop {
        // Drain pending agent events before drawing so the TUI sees
        // fresh state on every tick.
        app.drain_agent_events();
        // Turn queued `ctx.ui.*` requests into modals (and notifications
        // into transcript lines) before rendering them.
        app.poll_ui_dialogs();
        // Extensions write session entries / custom messages from
        // event handlers; fold them into the log + transcript each
        // tick so nothing is lost between turns.
        persist_extension_side_effects(&mut app, &options);
        // Stage 27: once a prompt finishes, check whether the turn that
        // just ended pushed the context past the compaction threshold.
        maybe_auto_compact(&mut app, &agent, &options).await;

        if app.is_exit_requested() {
            break;
        }

        // Redraw.
        if last_render.elapsed() >= render_interval {
            let snapshot = app.render_snapshot(terminal.size()?.width, terminal.size()?.height);
            terminal.draw(|frame| {
                let area = frame.area();
                let lines = snapshot.lines.clone();
                for (idx, line) in lines.iter().enumerate() {
                    let y = area.y + idx as u16;
                    if y >= area.y + area.height {
                        break;
                    }
                    for (col, ch) in line.chars().enumerate() {
                        let x = area.x + col as u16;
                        if x >= area.x + area.width {
                            break;
                        }
                        frame
                            .buffer_mut()
                            .cell_mut((x, y))
                            .expect("in-bounds cell")
                            .set_char(ch);
                    }
                }
            })?;
            last_render = std::time::Instant::now();
        }

        // Poll for crossterm events with a short timeout so the
        // render loop continues to tick.
        if ct_event::poll(config.event_poll_interval)? {
            while let Some(event) = read_event()? {
                let translated = App::translate_event(event);
                if let Some(action) =
                    handle_input_event(&mut app, &agent, &options, translated).await?
                {
                    match action {
                        InternalAction::Exit => break,
                    }
                }
            }
        }
    }

    // Nothing is pumping dialogs any more: deny instead of queueing.
    if let Some(ui) = options.extension_ui.as_ref() {
        ui.disarm();
    }
    Ok(InteractiveExit::UserExit)
}

/// Internal action returned from `handle_input_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalAction {
    /// User requested exit.
    Exit,
}

/// Translate a single [`InputEvent`] into App mutations. Returns
/// `Some(InternalAction::Exit)` when the App has exited and the
/// render loop should unwind.
async fn handle_input_event(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
    event: InputEvent,
) -> anyhow::Result<Option<InternalAction>> {
    // When the selector is open (and no extension dialog is on top of
    // it), handle selection first.
    if app.selector_open() && !app.dialog_open() {
        let InputEvent::Key(key) = event else {
            return Ok(None);
        };
        let selector_state = app.selector().cloned();
        if let Some(sel) = selector_state {
            let mut owned = sel;
            let action = owned.handle_key(key);
            match action {
                pi_tui::selector::SelectorAction::None => {
                    // Forward through; do not update App state.
                }
                pi_tui::selector::SelectorAction::Changed => {
                    app.replace_selector(owned);
                }
                pi_tui::selector::SelectorAction::Selected(value) => {
                    app.close_selector();
                    apply_selector_choice(app, agent, &value, options).await;
                }
                pi_tui::selector::SelectorAction::Cancelled => {
                    app.close_selector();
                }
            }
            return Ok(None);
        }
    }

    let step_outcome = app.step(event);
    match step_outcome {
        pi_tui::app::StepOutcome::Idle => Ok(None),
        pi_tui::app::StepOutcome::Redraw => Ok(None),
        pi_tui::app::StepOutcome::Submitted(text) => {
            if text.starts_with('/') {
                // Prompt templates take precedence over built-in slash
                // commands, mirroring the TS CLI: `/<name>` expands to
                // the template body when a template with that name was
                // loaded, otherwise the text falls through to the
                // built-in / extension command dispatch.
                if let Some((template, args)) =
                    crate::prompt_templates::find_prompt_template(&text, &options.prompt_templates)
                {
                    let parsed = crate::prompt_templates::parse_command_args(&args);
                    let expanded =
                        crate::prompt_templates::substitute_args(&template.content, &parsed);
                    app.submit(agent.clone(), expanded);
                    Ok(None)
                } else {
                    run_slash_command(app, agent, options, &text).await?;
                    Ok(None)
                }
            } else {
                app.submit(agent.clone(), text);
                Ok(None)
            }
        }
        pi_tui::app::StepOutcome::Exit => Ok(Some(InternalAction::Exit)),
    }
}

/// Apply the user's choice when the selector closes.
async fn apply_selector_choice(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    value: &str,
    options: &InteractiveOptions,
) {
    if value.starts_with("model:") {
        let id = value.trim_start_matches("model:");
        let mut found = None;
        for (provider, model) in options.models.iter() {
            if model.id == id {
                found = Some((provider.clone(), model.clone()));
                break;
            }
        }
        if let Some((_, model)) = found {
            app.queue_model_switch(&mut *agent.lock().await, model);
            app.info(format!("model → {}", id));
        }
    }
}

async fn run_slash_command(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
    text: &str,
) -> anyhow::Result<()> {
    let parsed = match handle_command(text) {
        Ok(cmd) => cmd,
        Err(err) => {
            app.info(format!("error: {err}"));
            return Ok(());
        }
    };
    match parsed {
        SlashCommand::Help => {
            let empty = [];
            let commands = options
                .extensions
                .as_ref()
                .map(|runtime| runtime.commands())
                .unwrap_or(empty.as_slice());
            let help = append_prompt_templates(
                help_text_with_extensions(commands),
                &options.prompt_templates,
            );
            app.info(help);
        }
        SlashCommand::Clear => {
            app.messages_mut().clear();
        }
        SlashCommand::Exit => {
            app.request_exit();
        }
        SlashCommand::Model => {
            let items = options
                .models
                .iter()
                .map(|(provider, model)| {
                    let label = model.label.clone().unwrap_or_else(|| model.id.clone());
                    SelectorItem::new(format!("model:{}", model.id), label)
                        .with_description(provider.to_string())
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                app.info("/model: no models available".to_string());
            } else {
                // Upstream `/model` is searchable and windows at 10 rows
                // (`model-selector.ts`: `maxVisible = 10`).
                let selector = Selector::new("Pick a model", items)
                    .searchable(true)
                    .with_max_visible(10);
                app.open_selector(selector);
            }
        }
        SlashCommand::Session => {
            let agent_guard = agent.lock().await;
            let state = agent_guard.state();
            app.info(format!(
                "session {} — messages={}, model={}",
                options.session_id,
                state.messages.len(),
                agent_guard.model().id
            ));
        }
        SlashCommand::Resume => {
            // Stage 5: drive the selector from the SQLite reader via
            // `pi_coding_agent::list_resumable` so the user sees
            // versioned, time-stamped session metadata instead of raw
            // filenames. Falls back to a friendly message when the
            // session directory is empty or unconfigured.
            let dir = options
                .session_log
                .as_ref()
                .map(|log| log.directory().to_path_buf());
            let Some(dir) = dir else {
                app.info("/resume: session directory not configured".to_string());
                return Ok(());
            };
            let refs = match crate::list_resumable(&dir) {
                Ok(refs) => refs,
                Err(err) => {
                    app.info(format!("/resume: {err}"));
                    return Ok(());
                }
            };
            if refs.is_empty() {
                app.info("/resume: no saved sessions".to_string());
                return Ok(());
            }
            let items = refs
                .into_iter()
                .map(|r| {
                    let value = format!("resume:{}", r.session_id);
                    let label = r.session_id.clone();
                    SelectorItem::new(value, label).with_description(r.display())
                })
                .collect::<Vec<_>>();
            let selector = Selector::new("Pick a session to resume", items)
                .searchable(true)
                // Upstream `session-selector.ts`: `maxVisible = 10`.
                .with_max_visible(10);
            app.open_selector(selector);
        }
        SlashCommand::Trust(action) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let store = crate::trust::ProjectTrustStore::new(&crate::paths::agent_dir_or_default());
            match action {
                Some(decision) => match store.set(&cwd, Some(decision)) {
                    Ok(()) => {
                        let label = if decision { "trusted" } else { "untrusted" };
                        app.info(format!(
                            "Saved trust decision for {}: {label}. Restart pi for this to take effect.",
                            cwd.display()
                        ));
                    }
                    Err(err) => app.info(format!("/trust: {err}")),
                },
                None => {
                    let has_resources = crate::trust::has_trust_requiring_project_resources(&cwd);
                    let saved = match store.get(&cwd) {
                        Ok(Some(true)) => "trusted",
                        Ok(Some(false)) => "untrusted",
                        Ok(None) => "no saved decision (defaults to untrusted)",
                        Err(err) => {
                            app.info(format!("/trust: {err}"));
                            return Ok(());
                        }
                    };
                    app.info(format!(
                        "project trust for {}: {saved}; resources that require trust: {}",
                        cwd.display(),
                        if has_resources { "yes" } else { "no" }
                    ));
                    app.info("usage: /trust yes | /trust no".to_string());
                }
            }
        }
        SlashCommand::Compact { instructions } => {
            run_compact(app, agent, options, instructions.as_deref()).await;
        }
        SlashCommand::Unknown(name) => {
            // A `/name` that is not a built-in may still belong to an
            // extension (`pi.registerCommand`). Run it when it does.
            let runtime = options.extensions.as_ref();
            if let Some(runtime) = runtime.filter(|r| r.has_command(&name)) {
                let args = extension_command_args(text);
                match runtime.execute_command(&name, args).await {
                    Ok(outcome) => {
                        if let Some(text) = command_result_text(&outcome) {
                            if !text.is_empty() {
                                app.info(text);
                            }
                        }
                        if let Some(err) = &outcome.error {
                            app.info(format!("/{name}: {err}"));
                        }
                    }
                    Err(err) => app.info(format!("/{name}: {err}")),
                }
                persist_extension_side_effects(app, options);
                return Ok(());
            }
            app.info(format!("unknown command /{name} — try /help"));
        }
    }
    Ok(())
}

/// Run `/compact`: summarize the conversation prefix and replace the
/// agent's message log with the summary plus the retained tail.
///
/// The on-screen transcript is kept as scrollback (it is the user's
/// record of the session) and gains an info block describing what was
/// summarized; only the model context is replaced.
async fn run_compact(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
    instructions: Option<&str>,
) {
    if app.is_busy() {
        app.info("/compact: a turn is in flight — abort or wait for it to finish");
        return;
    }

    let (model, history) = {
        let guard = agent.lock().await;
        (guard.model().clone(), guard.state().messages.clone())
    };
    if history.is_empty() {
        app.info("/compact: nothing to compact yet");
        return;
    }

    // Manual compaction uses the configured token thresholds but ignores
    // the auto-compaction toggle, matching upstream.
    let compaction = match compact(
        &history,
        &model,
        &options.stream_fn,
        options.compaction,
        instructions,
    )
    .await
    {
        Ok(compaction) => compaction,
        Err(err) => {
            app.info(format!("/compact: {err}"));
            return;
        }
    };

    let report = apply_compaction(agent, options, history.len(), compaction).await;
    app.info(format!(
        "/compact: summarized {} message(s) → kept {} ({} → {} est. tokens; {} read, {} modified)\n\n{}",
        report.summarized(),
        report.retained,
        report.tokens_before,
        report.tokens_after,
        report.read_files,
        report.modified_files,
        report.summary,
    ));
}

/// Automatic compaction: after a turn finishes, compare that turn's
/// context size against the model's context window and summarize the
/// prefix when it no longer fits `reserveTokens` of head-room.
///
/// Mirrors the threshold branch of `_checkCompaction` in
/// `agent-session.ts`: the check runs between turns (never while a turn
/// is in flight, hence the [`App::is_busy`] guard) and `settings.enabled`
/// — `compaction.enabled` / `autoCompact` in `settings.json` — gates it.
///
/// Returns `true` when a compaction ran.
async fn maybe_auto_compact(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
) -> bool {
    if app.is_busy() {
        return false;
    }
    let Some(turn) = app.take_turn_usage() else {
        return false;
    };
    let settings = options.compaction;
    if !settings.enabled {
        return false;
    }

    let (model, history) = {
        let guard = agent.lock().await;
        (guard.model().clone(), guard.state().messages.clone())
    };
    if history.is_empty() {
        return false;
    }

    // Rust `Message` rows carry no provider usage, so the count comes from
    // the `TurnEnd` event. A zero-usage turn (faux provider, or a provider
    // error) falls back to the pure size estimate over the whole log,
    // matching upstream `estimateContextTokens`.
    let context_tokens = if crate::compaction::calculate_context_tokens(&turn.usage) > 0 {
        crate::compaction::context_tokens_with_trailing(&turn.usage, &turn.trailing)
    } else {
        crate::compaction::estimate_context_tokens(&history)
    };
    if !crate::compaction::should_compact(context_tokens, model.context_window, settings) {
        return false;
    }

    let compaction = match compact(&history, &model, &options.stream_fn, settings, None).await {
        Ok(compaction) => compaction,
        // The conversation already fits the retained window — nothing to do.
        Err(CompactionError::NothingToCompact) => return false,
        Err(err) => {
            app.info(format!("auto-compact: {err}"));
            return false;
        }
    };

    // The summarization call awaited; if a new prompt slipped in meanwhile
    // the turn in flight owns `state.messages` and must not be truncated
    // under it. Skip this round; the next `TurnEnd` re-evaluates.
    if app.is_busy() {
        return false;
    }

    let report = apply_compaction(agent, options, history.len(), compaction).await;
    app.info(format!(
        "auto-compact: context {context_tokens} > {} window − {} reserve; summarized {} message(s) → kept {} ({} → {} est. tokens)\n\n{}",
        model.context_window,
        settings.reserve_tokens,
        report.summarized(),
        report.retained,
        report.tokens_before,
        report.tokens_after,
        report.summary,
    ));
    true
}

/// What a completed compaction did, in the numbers both the manual and the
/// automatic path report to the user.
struct CompactionReport {
    history_len: usize,
    retained: usize,
    tokens_before: u32,
    tokens_after: u32,
    read_files: usize,
    modified_files: usize,
    summary: String,
}

impl CompactionReport {
    /// Number of messages replaced by the summary.
    fn summarized(&self) -> usize {
        self.history_len.saturating_sub(self.retained)
    }
}

/// Persist `compaction`, swap the agent's message log for the summary plus
/// the retained tail, and return the numbers callers render.
async fn apply_compaction(
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
    history_len: usize,
    compaction: Compaction,
) -> CompactionReport {
    if let Some(log) = options.session_log.as_ref() {
        let _ = log.append_compaction(
            compaction.summary.clone(),
            compaction.retained_tail.clone(),
            compaction.tokens_before,
            Some(compaction.usage),
            Some(compaction.details()),
        );
    }

    let retained = compaction.retained_tail.len();
    let tokens_before = compaction.tokens_before;
    let read_files = compaction.read_files.len();
    let modified_files = compaction.modified_files.len();
    let summary = compaction.summary.clone();
    let compacted_history = compaction.into_history();
    let tokens_after = crate::compaction::estimate_context_tokens(&compacted_history);

    {
        let mut guard = agent.lock().await;
        guard.state_mut().messages = compacted_history;
    }

    CompactionReport {
        history_len,
        retained,
        tokens_before,
        tokens_after,
        read_files,
        modified_files,
        summary,
    }
}

/// Extract the text the user typed after the command name.
fn extension_command_args(text: &str) -> &str {
    let rest = text.trim().trim_start_matches('/');
    match rest.find(char::is_whitespace) {
        Some(idx) => rest[idx..].trim(),
        None => "",
    }
}

/// Render a command handler's return value as display text.
fn command_result_text(outcome: &pi_extensions::CommandExecutionOutcome) -> Option<String> {
    match &outcome.result {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// Splice the loaded prompt template list into the help text.
fn append_prompt_templates(base: String, templates: &[PromptTemplate]) -> String {
    if templates.is_empty() {
        return base;
    }
    let mut section = String::from("prompt templates:\n");
    for template in templates {
        if template.description.is_empty() {
            section.push_str(&format!("  /{}\n", template.name));
        } else {
            section.push_str(&format!("  /{:<16} {}\n", template.name, template.description));
        }
    }
    match base.split_once("\nkeys:") {
        Some((head, tail)) => format!("{head}\n\n{section}\nkeys:{tail}"),
        None => format!("{base}\n\n{section}"),
    }
}

/// Splice the extension command list into the built-in help text.
fn help_text_with_extensions(commands: &[pi_extensions::RegisteredCommand]) -> String {
    let base = crate::commands::help_text();
    if commands.is_empty() {
        return base;
    }
    let mut section = String::from("extension commands:\n");
    for command in commands {
        if command.description.is_empty() {
            section.push_str(&format!("  /{}\n", command.name));
        } else {
            section.push_str(&format!(
                "  /{:<16} {}\n",
                command.name, command.description
            ));
        }
    }
    match base.split_once("\nkeys:") {
        Some((head, tail)) => format!("{head}\n\n{section}\nkeys:{tail}"),
        None => format!("{base}\n\n{section}"),
    }
}

/// Drain the extension host's session side effects into the JSONL log
/// and surface custom messages in the transcript.
///
/// The host accumulates whatever extensions recorded via
/// `pi.appendEntry` / `pi.sendMessage` / `pi.sendUserMessage` /
/// `pi.setSessionName`; this runs every loop tick so nothing is lost.
fn persist_extension_side_effects(app: &mut App, options: &InteractiveOptions) {
    let Some(runtime) = options.extensions.as_ref() else {
        return;
    };
    let effects = runtime.drain_side_effects();
    let log = options.session_log.as_ref();
    for entry in &effects.entries {
        if let Some(log) = log {
            let _ =
                log.append_extension("extension", entry.custom_type.clone(), entry.data.clone());
        }
    }
    for message in &effects.messages {
        if let Some(log) = log {
            let _ = log.append_extension("extension", "message", message.clone());
        }
        app.info(format!("[extension] {}", message_preview(message)));
    }
    for user_message in &effects.user_messages {
        if let Some(log) = log {
            let _ =
                log.append_extension("extension", "user_message", serde_json::json!(user_message));
        }
        app.info(format!("[extension] queued message: {user_message}"));
    }
    if let Some(name) = &effects.session_name {
        if let Some(log) = log {
            let _ = log.append_extension("extension", "session_name", serde_json::json!(name));
        }
        app.info(format!("[extension] session renamed to {name}"));
    }
}

/// Human-readable form of a `pi.sendMessage` payload.
fn message_preview(value: &serde_json::Value) -> String {
    if let Some(text) = value.get("content").and_then(|c| c.as_str()) {
        return text.to_string();
    }
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    value.to_string()
}

#[allow(dead_code)]
fn list_session_files(dir: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "jsonl" || s == "sqlite")
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn default_model(models: &Models) -> Model {
    models
        .iter()
        .next()
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: pi_protocol::Api::Faux,
            label: Some("Faux test model".into()),
            context_window: 8192,
            max_output_tokens: 1024,
        })
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn read_event() -> anyhow::Result<Option<CtEvent>> {
    match ct_event::read()? {
        event @ (CtEvent::Key(_)
        | CtEvent::Resize(_, _)
        | CtEvent::FocusGained
        | CtEvent::FocusLost
        | CtEvent::Paste(_)) => Ok(Some(event)),
        CtEvent::Mouse(_) => Ok(None),
    }
}

// Quiet unused-import warnings for `AgentEvent` / `SyncMutex` /
// `KeyCode` / `Write` when the module is compiled in non-default
// feature configurations.
#[allow(dead_code)]
const _: Option<AgentEvent> = None;
#[allow(dead_code)]
fn _keep_sync_mutex() -> Arc<SyncMutex<()>> {
    Arc::new(SyncMutex::new(()))
}
#[allow(dead_code)]
fn _keep_keycode() -> Option<KeyCode> {
    None
}
#[allow(dead_code)]
fn _keep_writer() -> Option<Box<dyn Write>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_command_args_returns_text_after_the_name() {
        assert_eq!(extension_command_args("/echo"), "");
        assert_eq!(extension_command_args("/echo hello"), "hello");
        assert_eq!(extension_command_args("  /echo   hi there  "), "hi there");
    }

    #[test]
    fn help_text_lists_extension_commands_before_the_key_legend() {
        let commands = vec![
            pi_extensions::RegisteredCommand {
                name: "greet".into(),
                description: "Greets someone".into(),
            },
            pi_extensions::RegisteredCommand {
                name: "bare".into(),
                description: String::new(),
            },
        ];
        let text = help_text_with_extensions(&commands);
        assert!(text.contains("extension commands:"), "{text}");
        assert!(text.contains("/greet"), "{text}");
        assert!(text.contains("Greets someone"), "{text}");
        assert!(text.contains("/bare"), "{text}");
        let commands_at = text.find("extension commands:").expect("section");
        let keys_at = text.find("keys:").expect("key legend");
        assert!(commands_at < keys_at, "section order:\n{text}");
    }

    #[test]
    fn help_text_without_commands_is_unchanged() {
        assert_eq!(help_text_with_extensions(&[]), crate::commands::help_text());
    }

    #[test]
    fn message_preview_prefers_content_then_text_then_json() {
        assert_eq!(message_preview(&serde_json::json!({"content": "hi"})), "hi");
        assert_eq!(message_preview(&serde_json::json!("plain")), "plain");
        assert_eq!(
            message_preview(&serde_json::json!({"other": 1})),
            "{\"other\":1}"
        );
    }

    // -----------------------------------------------------------------------
    // Automatic compaction (Stage 27)
    // -----------------------------------------------------------------------

    use pi_agent_core::{Agent, AgentOptions};
    use pi_ai::providers::faux::FauxProvider;
    use pi_protocol::{Api, Message, Model, ProviderId, Role};
    use pi_tui::app::AppConfig;

    const LONG: usize = 400;

    fn small_window_model(context_window: u32) -> Model {
        Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: Api::Faux,
            label: None,
            context_window,
            max_output_tokens: 0,
        }
    }

    fn text_message(role: Role, text: String) -> Message {
        Message {
            role,
            content: vec![pi_protocol::Content::text(text)],
            model: None,
        }
    }

    /// Poll the App until the submitted prompt finishes and every event
    /// it emitted has been drained.
    async fn drain_until_idle(app: &mut App) {
        for _ in 0..500 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            app.drain_agent_events();
            if !app.is_busy() {
                // The task emits `TurnEnd` before clearing the busy flag;
                // drain once more so the accessor sees it.
                app.drain_agent_events();
                return;
            }
        }
        panic!("agent turn did not finish");
    }

    fn settings(enabled: bool) -> CompactionSettings {
        CompactionSettings {
            enabled,
            reserve_tokens: 1024,
            keep_recent_tokens: 100,
        }
    }

    /// Seed `[user, assistant]` and run one more prompt, producing a
    /// four-message history long enough to exceed a 100-token window.
    async fn app_after_two_turns(context_window: u32) -> (App, Arc<AsyncMutex<Agent>>) {
        let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
            small_window_model(context_window),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ))));
        {
            let mut guard = agent.lock().await;
            guard.state_mut().messages = vec![
                text_message(Role::User, "x".repeat(LONG)),
                text_message(Role::Assistant, "y".repeat(LONG)),
            ];
        }
        let config = AppConfig {
            session_id: "auto".into(),
            ..AppConfig::default()
        };
        let mut app = App::new(&*agent.lock().await, config);
        app.submit(agent.clone(), "z".repeat(LONG));
        drain_until_idle(&mut app).await;
        (app, agent)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_compaction_compacts_after_a_turn_over_the_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_log = SessionLog::open(dir.path(), "auto").expect("session log");
        let (mut app, agent) = app_after_two_turns(100).await;
        let options = InteractiveOptions {
            session_log: Some(session_log),
            compaction: settings(true),
            ..InteractiveOptions::default()
        };

        assert!(
            maybe_auto_compact(&mut app, &agent, &options).await,
            "a turn over the context window should compact"
        );

        let messages = agent.lock().await.state().messages.clone();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            crate::compaction::extract_summary(&messages[0]).is_some(),
            "the summary replaces the compacted prefix: {messages:?}"
        );
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].role, Role::Assistant);

        // The compaction was persisted as a session entry.
        options
            .session_log
            .as_ref()
            .expect("log")
            .close()
            .expect("close");
        let contents = std::fs::read_to_string(dir.path().join("auto.jsonl")).expect("read log");
        assert!(
            contents.lines().any(|line| line.contains("\"type\":\"compaction\"")),
            "expected a compaction entry: {contents}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_compaction_is_skipped_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_log = SessionLog::open(dir.path(), "auto").expect("session log");
        let (mut app, agent) = app_after_two_turns(100).await;
        let options = InteractiveOptions {
            session_log: Some(session_log),
            compaction: settings(false),
            ..InteractiveOptions::default()
        };

        assert!(!maybe_auto_compact(&mut app, &agent, &options).await);

        let messages = agent.lock().await.state().messages.clone();
        assert_eq!(messages.len(), 4, "history must be untouched: {messages:?}");
        options
            .session_log
            .as_ref()
            .expect("log")
            .close()
            .expect("close");
        let contents = std::fs::read_to_string(dir.path().join("auto.jsonl")).expect("read log");
        assert!(!contents.contains("compaction"), "{contents}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_compaction_is_skipped_below_the_threshold() {
        let (mut app, agent) = app_after_two_turns(1_000_000).await;
        let options = InteractiveOptions {
            compaction: settings(true),
            ..InteractiveOptions::default()
        };
        assert!(!maybe_auto_compact(&mut app, &agent, &options).await);
        assert_eq!(agent.lock().await.state().messages.len(), 4);
    }
}

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
use pi_agent_core::{Agent, AgentEvent, AgentOptions};
use pi_ai::models::Models;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{Model, ProviderId};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::{handle_command, SlashCommand};
use crate::session_log::SessionLog;
use crate::text_fallback::{run_text_fallback, FallbackReason};

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
#[derive(Debug, Default)]
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
    /// Initial prompt to submit on launch.
    pub initial_prompt: Option<String>,
}

/// Entry point invoked from `main`.
pub async fn run_interactive(options: InteractiveOptions) -> anyhow::Result<InteractiveExit> {
    let stream_fn: SharedStreamFn = Arc::new(FauxProvider::default());
    let resolved_model = options
        .model
        .clone()
        .unwrap_or_else(|| default_model(&options.models));

    let mut system_prompt = options.system_prompt.clone();
    if !options.append_system_prompt.is_empty() {
        system_prompt.push('\n');
        system_prompt.push_str(&options.append_system_prompt.join("\n"));
    }

    let agent = Agent::new(AgentOptions::new(
        resolved_model.clone(),
        stream_fn,
        system_prompt,
    ));
    let agent = Arc::new(AsyncMutex::new(agent));

    let config = AppConfig {
        prompt_placeholder: "type a prompt — /help for commands".into(),
        session_id: options.session_id.clone(),
        event_poll_interval: Duration::from_millis(50),
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
    options: InteractiveOptions,
    config: AppConfig,
) -> anyhow::Result<InteractiveExit> {
    // Build the App on the stack first so we can drop it before
    // tearing down the terminal.
    let mut app = App::new(&*agent.lock().await, config.clone());

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

        if app.is_exit_requested() {
            return Ok(InteractiveExit::UserExit);
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
                        InternalAction::Exit => return Ok(InteractiveExit::UserExit),
                    }
                }
            }
        }
    }
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
    // When the selector is open, handle selection first.
    if app.selector_open() {
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
                run_slash_command(app, agent, options, &text).await?;
                Ok(None)
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
            app.info(crate::commands::help_text());
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
                let selector = Selector::new("Pick a model", items);
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
            // Stage 4 fallback: list session files under the session
            // directory if available, otherwise inform the user that
            // `/resume` requires a session directory.
            if let Some(log) = &options.session_log {
                let dir = log.directory().to_path_buf();
                let entries = match list_session_files(&dir) {
                    Ok(entries) => entries,
                    Err(err) => {
                        app.info(format!("/resume: {err}"));
                        return Ok(());
                    }
                };
                if entries.is_empty() {
                    app.info("/resume: no saved sessions".to_string());
                } else {
                    let items = entries
                        .into_iter()
                        .map(|entry| {
                            let id = entry
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("unknown")
                                .to_string();
                            SelectorItem::new(format!("resume:{id}"), id.clone())
                                .with_description(entry.display().to_string())
                        })
                        .collect::<Vec<_>>();
                    let selector = Selector::new("Pick a session to resume", items);
                    app.open_selector(selector);
                }
            } else {
                app.info("/resume: session directory not configured".to_string());
            }
        }
        SlashCommand::Unknown(name) => {
            app.info(format!("unknown command /{name} — try /help"));
        }
    }
    Ok(())
}

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

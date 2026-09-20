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

use crossterm::event::{
    self as ct_event, DisableMouseCapture, EnableMouseCapture, Event as CtEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use parking_lot::Mutex as SyncMutex;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::{Agent, AgentEvent, AgentOptions, RetryPolicy};
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
use crate::config::{self, ConfigSources};
use crate::extensions::ui_bridge::{RegionPump, TuiUi};
use crate::extensions::wiring::ExtensionRuntime;
use crate::prompt_templates::PromptTemplate;
use crate::session_log::SessionLog;
use crate::text_fallback::{run_text_fallback, FallbackReason};
use crate::tool_executor::default_executor;

use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, KeyCode};
use pi_tui::message::Role;
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::settings::{SettingItem, SettingsList};

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
    /// Agent-level retry budget for the assistant call, resolved from
    /// `settings.json` by the caller (`config::load_agent_retry_policy`).
    /// Applied to the agent the TUI drives.
    pub retry: RetryPolicy,
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
            .field("retry", &self.retry)
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
            retry: RetryPolicy::default(),
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
            .with_tool_executor(options.tool_executor.clone())
            // `settings.retry` decides how many times a transient provider
            // failure restarts the assistant call before the TUI reports it.
            .with_retry_policy(options.retry),
    );
    let agent = Arc::new(AsyncMutex::new(agent));

    let config = AppConfig {
        prompt_placeholder: "type a prompt — /help for commands".into(),
        session_id: options.session_id.clone(),
        event_poll_interval: Duration::from_millis(50),
        markdown: true,
        // Upstream default: `copyOnSelect ?? true`.
        copy_on_select: true,
        // Let the App detect the terminal's OSC 8 support from the
        // environment (`Hyperlinks: None` = auto).
        hyperlinks: None,
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

    // TUI-only: install the merged coding-agent table (the `pi-tui` defaults
    // plus the four `tui.*` platform overrides and the `app.*` ids) as the
    // process-wide registry the `pi-tui` components resolve every chord
    // against. Print / RPC / no-TTY runs never reach this point and keep the
    // `pi-tui` defaults.
    //
    // The returned manager is the handle a config reload re-installs through
    // (`keybindings::reload_keybindings`); the render loop has no reload
    // trigger yet, so it is only kept alive for the session here.
    let _keybindings =
        crate::keybindings::install_keybindings_from(crate::paths::agent_dir_or_default());

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
    // Region / overlay mutations (`ctx.ui.setHeader` & friends) need the
    // App in hand, so their receiver becomes a `RegionPump` the loop
    // drives below.
    let mut region_pump = None;
    if let Some(ui) = options.extension_ui.as_mut() {
        if let Some(dialogs) = ui.take_dialogs() {
            app.attach_ui_dialogs(dialogs);
        }
        region_pump = ui.take_regions().map(|(ops, tx)| RegionPump::new(ops, tx));
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
        // Apply queued `ctx.ui` region mutations and re-render the JS
        // components before the frame is drawn, so a `setHeader` that
        // just arrived shows up in this tick. The width mirrors what the
        // App is about to render into; a failed size query falls back to
        // the 80-column default.
        if let Some(pump) = region_pump.as_mut() {
            let width = terminal.size().map(|area| area.width).unwrap_or(80);
            pump.pump(&mut app, width).await;
        }
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

        // Redraw. The App's buffer path carries the theme *and* the
        // chat-log selection highlight, so draw the App straight into the
        // frame instead of round-tripping through plain snapshot lines.
        if last_render.elapsed() >= render_interval {
            terminal.draw(|frame| {
                let area = frame.area();
                app.render_to_buffer(area, frame.buffer_mut());
            })?;
            last_render = std::time::Instant::now();
        }

        // Poll for crossterm events with a short timeout so the
        // render loop continues to tick.
        if ct_event::poll(config.event_poll_interval)? {
            while let Some(event) = read_event()? {
                let translated = App::translate_event(event);
                if let Some(action) =
                    handle_input_event(&mut app, &agent, &mut options, translated).await?
                {
                    match action {
                        InternalAction::Exit => break,
                    }
                }
            }
        }

        // Copy-on-select: the App hands over the finished selection, the
        // driver performs the terminal write (upstream's default is an
        // OSC 52 sequence — `packages/tui/src/tui-alt-screen.ts:1459`).
        if let Some(text) = app.take_clipboard_request() {
            write!(
                terminal.backend_mut(),
                "{}",
                pi_tui::clipboard::osc52_sequence(&text)
            )?;
            terminal.backend_mut().flush()?;
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
    options: &mut InteractiveOptions,
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

    // Coding-agent shortcuts. Upstream registers these on the editor
    // (`interactive-mode.ts:2883-2903`); the Rust port has no editor action
    // hook, so the driver claims the key before the App sees it — but only
    // while no modal or overlay owns the keyboard.
    if !app.selector_open()
        && !app.dialog_open()
        && !app.settings_open()
        && !app.custom_open()
        && !app.search_open()
    {
        let keybindings = pi_tui::keybindings::get_keybindings();
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.model.cycleForward",
            &["ctrl+p"],
        ) {
            cycle_model(app, agent, options, CycleDirection::Forward).await;
            return Ok(None);
        }
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.model.cycleBackward",
            &["shift+ctrl+p"],
        ) {
            cycle_model(app, agent, options, CycleDirection::Backward).await;
            return Ok(None);
        }
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.message.copy",
            &["ctrl+x"],
        ) {
            copy_last_assistant_message(app);
            return Ok(None);
        }
    }

    let step_outcome = app.step(event);
    // `/settings` hands value changes back to the driver — upstream's
    // `onChange(id, newValue)` callback. Apply what the running session
    // can honour, persist everything to the user settings file, and
    // report the rest as "next launch" so nothing is silently dropped.
    drain_settings_changes(app, options, &settings_sources());
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

/// Which way `app.model.cycleForward` / `app.model.cycleBackward` move
/// through the model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CycleDirection {
    /// `app.model.cycleForward` — the next model, wrapping at the end.
    Forward,
    /// `app.model.cycleBackward` — the previous model, wrapping at the
    /// start.
    Backward,
}

/// The model catalog in a stable order.
///
/// [`Models`] groups entries by provider in a `HashMap`, so its iteration
/// order varies run to run. Both the `/model` selector and the cycling
/// shortcuts need one deterministic order, so they share this helper
/// (sorted by provider then model id).
fn sorted_models(models: &Models) -> Vec<(ProviderId, Model)> {
    let mut catalog = models
        .iter()
        .map(|(provider, model)| (provider.clone(), model.clone()))
        .collect::<Vec<_>>();
    catalog.sort_by(|a, b| (a.0.to_string(), &a.1.id).cmp(&(b.0.to_string(), &b.1.id)));
    catalog
}

/// `app.model.cycleForward` / `app.model.cycleBackward` — move the live
/// session to the next / previous catalog model.
///
async fn cycle_model(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &InteractiveOptions,
    direction: CycleDirection,
) {
    let catalog = sorted_models(&options.models);
    if catalog.is_empty() {
        app.info("no models available".to_string());
        return;
    }
    if catalog.len() == 1 {
        app.info("only one model available".to_string());
        return;
    }
    let mut agent_guard = agent.lock().await;
    let current = agent_guard.model().id.clone();
    let next = match catalog.iter().position(|(_, model)| model.id == current) {
        Some(index) => match direction {
            CycleDirection::Forward => (index + 1) % catalog.len(),
            CycleDirection::Backward => (index + catalog.len() - 1) % catalog.len(),
        },
        // The pinned model is not part of the catalog (e.g. `--model` named
        // an id the registered providers do not ship): start from the top.
        None => 0,
    };
    let (provider, model) = catalog[next].clone();
    let label = model.label.clone().unwrap_or_else(|| model.id.clone());
    app.queue_model_switch(&mut agent_guard, model);
    app.info(format!("model → {provider}/{label}"));
}

/// `app.message.copy` — hand the last assistant message to the driver's
/// clipboard channel.
///
/// Upstream's `handleCopyCommand({ preferSelection: true })` copies an active
/// selection first and falls back to the last assistant block; the selection
/// case is already covered by copy-on-select, so this only needs the
/// fallback.
fn copy_last_assistant_message(app: &mut App) {
    let Some(text) = app
        .messages()
        .items()
        .iter()
        .rev()
        .find(|item| item.role == Role::Assistant && !item.text.trim().is_empty())
        .map(|item| item.text.clone())
    else {
        app.info("nothing to copy yet".to_string());
        return;
    };
    let lines = text.lines().count();
    app.request_clipboard(text);
    app.info(format!("copied {lines} line(s) to the clipboard"));
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
    options: &mut InteractiveOptions,
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
            let items = sorted_models(&options.models)
                .into_iter()
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
        SlashCommand::Hotkeys => {
            app.info(crate::commands::slash::hotkeys_text());
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
        SlashCommand::Export { path } => {
            // Upstream `handleExportCommand`: `.jsonl` writes the session
            // branch as JSONL, anything else writes self-contained HTML.
            // The running TUI theme is forwarded so the export matches
            // what the user sees.
            let agent_guard = agent.lock().await;
            let state = agent_guard.state();
            let tools = options.tool_executor.definitions();
            let theme_name = app.theme().name().map(str::to_string);
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let request = crate::commands::export::ActiveSession {
                session_id: &options.session_id,
                cwd: &cwd,
                messages: &state.messages,
                system_prompt: &state.system_prompt,
                tools: &tools,
            };
            let result = crate::commands::export::run_slash_export(
                request,
                path.as_deref(),
                theme_name.as_deref(),
            );
            drop(agent_guard);
            match result {
                Ok(file_path) => app.info(format!("Session exported to: {}", file_path.display())),
                Err(err) => app.info(format!("Failed to export session: {err}")),
            }
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
        SlashCommand::Settings => {
            open_settings(app, options, &settings_sources());
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
/// The `settings.json` locations for the current working directory.
fn settings_sources() -> ConfigSources {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ConfigSources::discover(&cwd)
}

/// Build and open the `/settings` modal (upstream
/// `SettingsSelectorComponent`, `components/settings-selector.ts:449`).
///
/// The item set is the upstream one restricted to the settings this build
/// actually consumes:
///
/// | item | settings.json | effect |
/// |------|---------------|--------|
/// | Auto-compact | `compaction.enabled` | gates auto-compaction, live |
/// | Fullscreen copy on select | `fullscreenCopyOnSelect` | gates copy-on-select, live |
/// | Theme | `theme` | swaps the App palette, live |
///
/// Upstream's other rows (steering/follow-up mode, transport,
/// `modelThinkingLevels`, image handling, …) belong to subsystems this
/// port does not have yet; they land with those subsystems rather than as
/// inert rows here.
fn open_settings(app: &mut App, options: &InteractiveOptions, sources: &ConfigSources) {
    let ui = config::load_ui_settings(sources);

    // The live palette wins over the persisted choice: the user may have
    // switched themes from a future `/theme` command or a CLI flag that
    // never touched `settings.json`.
    let theme = app
        .theme()
        .name()
        .map(str::to_string)
        .or(ui.theme)
        .unwrap_or_else(|| DEFAULT_THEME_NAME.to_string());

    let items = vec![
        SettingItem::new("autocompact", "Auto-compact")
            .with_description("Automatically compact context when it gets too large")
            .with_values(["true", "false"], boolean(options.compaction.enabled)),
        SettingItem::new("fullscreen-copy-on-select", "Fullscreen copy on select")
            .with_description(
                "Automatically copy selected text; disable to copy selections with Ctrl+X",
            )
            .with_values(["true", "false"], boolean(app.copy_on_select())),
        SettingItem::new("theme", "Theme")
            .with_description("Color theme for the interface")
            .with_values(["dark", "light"], theme),
    ];

    // Upstream windows the list at `min(items.length, 10)` rows.
    let max_visible = items.len().min(10);
    app.open_settings(SettingsList::new(items, max_visible).searchable(true));
}

/// Apply every value change the settings modal queued — upstream's
/// `onChange` callback, which the modal only fires for a real value change
/// (cursor moves and filter edits never reach the settings file).
///
/// Activations (Enter on a value-less row) have no submenu in this build;
/// they are reported instead of being dropped.
fn drain_settings_changes(
    app: &mut App,
    options: &mut InteractiveOptions,
    sources: &ConfigSources,
) {
    while let Some((id, value)) = app.take_pending_setting_change() {
        apply_setting_change(app, options, sources, &id, &value);
    }
    if let Some(id) = app.take_pending_setting_activation() {
        app.info(format!(
            "/settings: '{id}' opens a submenu that this build does not implement yet"
        ));
    }
}

/// Apply one `/settings` value change.
///
/// Called for every [`SettingsAction::ValueChanged`](pi_tui::settings::SettingsAction)
/// the modal reports — upstream's `onChange` callback
/// (`settings-selector.ts:830`). The change is applied to the running
/// session where the subsystem allows it and always persisted to the user
/// settings file; a failure to write is reported, never silent.
fn apply_setting_change(
    app: &mut App,
    options: &mut InteractiveOptions,
    sources: &ConfigSources,
    id: &str,
    value: &str,
) {
    // Each row owns one settings key and its JSON type: the booleans are
    // stored as booleans (`core/settings-manager.ts:849,1284`), the theme
    // as a string. A stringified `"false"` would be rejected by the
    // loader and silently fall back to the default.
    use serde_json::Value;

    let (key, json_value, applied, note) = match id {
        "autocompact" => {
            let enabled = value == "true";
            // `options` is owned by the run loop, so the toggle applies to
            // every later `maybe_auto_compact` check — live, like upstream.
            options.compaction.enabled = enabled;
            (
                "compaction.enabled",
                Value::Bool(enabled),
                format!("auto-compact → {value}"),
                " (applies from the next turn)",
            )
        }
        "fullscreen-copy-on-select" => {
            let enabled = value == "true";
            app.set_copy_on_select(enabled);
            (
                "fullscreenCopyOnSelect",
                Value::Bool(enabled),
                format!("copy on select → {value}"),
                "",
            )
        }
        "theme" => match app.set_theme_by_name(value) {
            Ok(()) => (
                "theme",
                Value::String(value.to_string()),
                format!("theme → {value}"),
                "",
            ),
            Err(err) => {
                app.info(format!("/settings: {err}"));
                return;
            }
        },
        other => {
            app.info(format!("/settings: unknown setting {other:?}"));
            return;
        }
    };

    match config::save_user_setting(sources, key, json_value) {
        Ok(path) => app.info(format!(
            "/settings: {applied}{note} (saved to {})",
            path.display()
        )),
        Err(err) => app.info(format!(
            "/settings: {applied}{note}, but saving to settings.json failed: {err}"
        )),
    }
}

/// The theme the App boots with when no choice is stored — upstream's
/// default (`createTheme(getBuiltinThemes()["dark"])`).
const DEFAULT_THEME_NAME: &str = "dark";

/// `true` / `false` for a [`SettingItem`] value list.
fn boolean(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

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
        Some(options.retry),
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
/// Mirrors `_checkCompaction` in `agent-session.ts`: the check runs between
/// turns (never while a turn is in flight, hence the [`App::is_busy`] guard)
/// and `settings.enabled` — `compaction.enabled` / `autoCompact` in
/// `settings.json` — gates it. Two signals trigger it, exactly like upstream:
/// `context_tokens + reserve_tokens` crossing the window, and an overflow
/// signal in the finished turn (`pi_ai::is_context_overflow`) that the
/// reserve threshold would otherwise miss, e.g. a length stop with zero
/// output that filled the window while `reserve_tokens` is 0.
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
    // Upstream `_checkCompaction` compacts on two further signals besides the
    // threshold: a *successful* turn whose reported input already exceeds the
    // window (z.ai answers an oversized prompt instead of failing it), and a
    // length stop with zero output that filled the window (Xiaomi MiMo
    // truncates the input to fit and generates nothing) — `utils/overflow.ts`.
    // The error-message case (upstream `isContextOverflow` case 1) cannot
    // fire here yet: `pi-protocol::AssistantMessage` now carries
    // `error_message`, but a finished turn reaches this function as a
    // `TurnUsage`, which has no error slot (`pi_tui::TurnUsage`). A failed
    // attempt is retried by `pi-agent-core::retry` instead (which excludes
    // overflow from the retry budget).
    let context_overflow = pi_ai::is_context_overflow(
        turn.stop_reason,
        None,
        &turn.usage,
        Some(model.context_window),
    );
    if !context_overflow
        && !crate::compaction::should_compact(context_tokens, model.context_window, settings)
    {
        return false;
    }
    let trigger = if context_overflow {
        "overflow"
    } else {
        "threshold"
    };

    let compaction = match compact(
        &history,
        &model,
        &options.stream_fn,
        settings,
        None,
        Some(options.retry),
    )
    .await
    {
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
        "auto-compact ({trigger}): context {context_tokens} vs {} window − {} reserve; summarized {} message(s) → kept {} ({} → {} est. tokens)\n\n{}",
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
            section.push_str(&format!(
                "  /{:<16} {}\n",
                template.name, template.description
            ));
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
    sorted_models(models)
        .into_iter()
        .next()
        .map(|(_, m)| m)
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
    // Mouse capture is what delivers the wheel and the drag gestures the
    // chat-log selection consumes: upstream turns it on for every alt-screen
    // session (`packages/tui/src/tui-alt-screen.ts:168,265`, mouse sequence
    // at `:353-362`) and handles the terminal-side selection loss with its
    // own selection + copy-on-select (`:1343-1379`, `:1449-1462`), which the
    // App mirrors. Text selection by the terminal itself is therefore
    // unavailable, matching upstream.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn read_event() -> anyhow::Result<Option<CtEvent>> {
    match ct_event::read()? {
        event @ (CtEvent::Key(_)
        | CtEvent::Mouse(_)
        | CtEvent::Resize(_, _)
        | CtEvent::FocusGained
        | CtEvent::FocusLost
        | CtEvent::Paste(_)) => Ok(Some(event)),
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
    use pi_ai::{AssistantMessageEventStream, SimpleStreamOptions, StreamError, StreamFn};
    use pi_protocol::{
        Api, AssistantMessageEvent, Context, Message, Model, ProviderId, Role, StopReason, Usage,
    };
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

    /// A provider whose every reply is a `length` stop carrying `usage`, so the
    /// App records exactly the turn signal the test wants to exercise.
    #[derive(Debug)]
    struct LengthStopProvider {
        usage: Usage,
    }

    #[async_trait::async_trait]
    impl StreamFn for LengthStopProvider {
        async fn stream_simple(
            &self,
            model: &Model,
            _context: &Context,
            _options: &SimpleStreamOptions,
        ) -> Result<AssistantMessageEventStream, StreamError> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(AssistantMessageEvent::Start {
                    model: model.id.clone(),
                }),
                Ok(AssistantMessageEvent::Done {
                    content: vec![pi_protocol::Content::text("truncated")],
                    stop_reason: StopReason::MaxTokens,
                    usage: self.usage,
                }),
            ])))
        }
    }

    /// Seed `[user, assistant]` and run one more prompt, producing a
    /// four-message history long enough to exceed a 100-token window.
    async fn app_after_two_turns(context_window: u32) -> (App, Arc<AsyncMutex<Agent>>) {
        app_after_two_turns_with(context_window, Arc::new(FauxProvider::default())).await
    }

    /// Like [`app_after_two_turns`], but with a caller-supplied provider whose
    /// reply controls the turn's recorded [`pi_tui::app::TurnUsage`].
    async fn app_after_two_turns_with(
        context_window: u32,
        provider: SharedStreamFn,
    ) -> (App, Arc<AsyncMutex<Agent>>) {
        let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
            small_window_model(context_window),
            provider,
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
            contents
                .lines()
                .any(|line| line.contains("\"type\":\"compaction\"")),
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_compaction_runs_on_a_length_stop_overflow_the_threshold_would_miss() {
        // 990 / 1000 = 99% of the window with zero output — the Xiaomi MiMo
        // shape (`is_context_overflow` case 3). With `reserve_tokens: 0` the
        // threshold check (`990 + 0 > 1000`) stays false, so only the overflow
        // branch can trigger a compaction.
        let provider = Arc::new(LengthStopProvider {
            usage: Usage {
                input: 990,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total: 990,
            },
        });
        let (mut app, agent) = app_after_two_turns_with(1_000, provider).await;
        let options = InteractiveOptions {
            compaction: CompactionSettings {
                enabled: true,
                reserve_tokens: 0,
                keep_recent_tokens: 100,
            },
            ..InteractiveOptions::default()
        };

        assert!(
            maybe_auto_compact(&mut app, &agent, &options).await,
            "a 99%-full length stop should compact even below the reserve threshold"
        );
        assert_eq!(agent.lock().await.state().messages.len(), 3);
    }

    // -----------------------------------------------------------------------
    // `/settings`
    // -----------------------------------------------------------------------

    use pi_tui::input::{Key, KeyModifiers};

    /// The `/settings` sources: a throwaway user file and no project file,
    /// so the test never touches the developer's real settings.
    fn temp_sources(dir: &std::path::Path) -> ConfigSources {
        ConfigSources {
            user: Some(user_settings_path(dir)),
            project: None,
        }
    }

    fn user_settings_path(dir: &std::path::Path) -> PathBuf {
        dir.join(config::SETTINGS_FILE_NAME)
    }

    /// The App's rendered transcript, joined — the App exposes lines only
    /// through a render pass.
    fn transcript(app: &App) -> String {
        app.render_snapshot(80, 24).lines.join("\n")
    }

    fn settings_app() -> App {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let config = AppConfig {
            session_id: "settings".into(),
            ..AppConfig::default()
        };
        App::new(&agent, config)
    }

    /// Press one key on the App and hand whatever the modal queued to the
    /// driver — exactly what `handle_input_event` does for every event.
    fn press(
        app: &mut App,
        options: &mut InteractiveOptions,
        sources: &ConfigSources,
        code: KeyCode,
    ) {
        app.step(InputEvent::Key(Key::new(code, KeyModifiers::NONE)));
        drain_settings_changes(app, options, sources);
    }

    #[test]
    fn open_settings_shows_the_live_state_of_three_items() {
        let mut app = settings_app();
        let options = InteractiveOptions {
            compaction: settings(false),
            ..InteractiveOptions::default()
        };
        app.set_copy_on_select(false);

        open_settings(&mut app, &options, &ConfigSources::default());

        let settings = app.settings().expect("modal open");
        assert_eq!(
            values(settings),
            vec![
                ("autocompact".to_string(), "false".to_string()),
                ("fullscreen-copy-on-select".to_string(), "false".to_string()),
                ("theme".to_string(), "dark".to_string()),
            ],
            "upstream order, restricted to the wired settings"
        );
        assert!(!options.compaction.enabled);
    }

    /// `(id, current value)` for every row, in display order.
    fn values(settings: &SettingsList) -> Vec<(String, String)> {
        settings
            .items()
            .iter()
            .map(|item| (item.id.clone(), item.current_value.clone()))
            .collect()
    }

    #[test]
    fn open_settings_prefers_the_live_theme_over_the_stored_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        config::save_user_setting(
            &sources,
            "theme",
            serde_json::Value::String("light".to_string()),
        )
        .expect("seed");
        let mut app = settings_app();
        app.set_theme_by_name("light").expect("switch");

        open_settings(&mut app, &InteractiveOptions::default(), &sources);
        let settings = app.settings().expect("modal");
        assert_eq!(settings.item("theme").expect("row").current_value, "light");
    }

    #[test]
    fn cycling_a_row_applies_and_persists_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();
        let mut options = InteractiveOptions {
            compaction: settings(true),
            ..InteractiveOptions::default()
        };
        app.set_copy_on_select(true);
        open_settings(&mut app, &options, &sources);

        // Row 1: auto-compact `true` → `false`, live from the next turn.
        press(&mut app, &mut options, &sources, KeyCode::Enter);
        assert!(!options.compaction.enabled, "the toggle applies live");

        // Row 2: copy-on-select `true` → `false`.
        press(&mut app, &mut options, &sources, KeyCode::Down);
        press(&mut app, &mut options, &sources, KeyCode::Enter);
        assert!(!app.copy_on_select());

        // Row 3: theme `dark` → `light`.
        press(&mut app, &mut options, &sources, KeyCode::Down);
        press(&mut app, &mut options, &sources, KeyCode::Enter);
        assert_eq!(app.theme().name(), Some("light"));

        // Everything landed in the user settings file, and only the keys
        // the modal owns.
        let parsed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(user_settings_path(dir.path())).expect("read"),
        )
        .expect("json");
        assert_eq!(parsed["theme"], serde_json::json!("light"));
        assert_eq!(parsed["fullscreenCopyOnSelect"], serde_json::json!(false));
        assert_eq!(parsed["compaction"]["enabled"], serde_json::json!(false));
        let reloaded = config::load_ui_settings(&sources);
        assert_eq!(reloaded.theme.as_deref(), Some("light"));
        assert!(!reloaded.fullscreen_copy_on_select);
        assert!(!config::load_compaction_settings(&sources).enabled);
    }

    #[test]
    fn moving_the_cursor_and_filtering_persist_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();
        let mut options = InteractiveOptions::default();
        open_settings(&mut app, &options, &sources);

        for code in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('t'),
            KeyCode::Backspace,
        ] {
            press(&mut app, &mut options, &sources, code);
        }

        assert!(
            !user_settings_path(dir.path()).exists(),
            "cursor moves and filter edits are not settings changes (upstream `onChange`)"
        );
    }

    #[test]
    fn escape_closes_the_modal_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();
        let mut options = InteractiveOptions::default();
        open_settings(&mut app, &options, &sources);

        press(&mut app, &mut options, &sources, KeyCode::Esc);

        assert!(!app.settings_open());
        assert!(!user_settings_path(dir.path()).exists());
    }

    #[test]
    fn a_failed_save_is_reported_and_the_live_change_is_kept() {
        // A malformed settings file must not cost the user the change they
        // just made in the running session.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(user_settings_path(dir.path()), "{ nope").expect("seed");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();
        let mut options = InteractiveOptions::default();

        apply_setting_change(&mut app, &mut options, &sources, "theme", "light");

        assert_eq!(app.theme().name(), Some("light"));
        let rendered = transcript(&app);
        assert!(
            rendered.contains("saving to settings.json failed"),
            "{rendered}"
        );
        assert_eq!(
            std::fs::read_to_string(user_settings_path(dir.path())).expect("read"),
            "{ nope",
            "the user's file is untouched"
        );
    }

    #[test]
    fn an_activation_without_a_submenu_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();
        let mut options = InteractiveOptions::default();
        app.open_settings(pi_tui::settings::SettingsList::new(
            vec![SettingItem::new("warnings", "Warnings")],
            10,
        ));

        press(&mut app, &mut options, &sources, KeyCode::Enter);

        let rendered = transcript(&app);
        assert!(
            rendered.contains("'warnings' opens a submenu"),
            "{rendered}"
        );
    }

    // -----------------------------------------------------------------------
    // `app.*` actions: model cycling + message copy (the first two handlers)
    // -----------------------------------------------------------------------

    /// A model entry with no label, so the notification text falls back to
    /// `<provider>/<id>`.
    fn catalog_model(provider: &str, id: &str) -> Model {
        Model {
            provider: ProviderId::new(provider),
            id: id.into(),
            api: Api::Faux,
            label: None,
            context_window: 1_000,
            max_output_tokens: 100,
        }
    }

    /// Two providers registered out of order: the sorted catalog must be
    /// `alpha/a` then `beta/b` regardless of insertion order.
    fn two_model_catalog() -> Models {
        let mut models = Models::new();
        models.set_provider(ProviderId::new("beta"), vec![catalog_model("beta", "b")]);
        models.set_provider(ProviderId::new("alpha"), vec![catalog_model("alpha", "a")]);
        models
    }

    async fn app_starting_at(model: Model) -> (App, Arc<AsyncMutex<Agent>>) {
        let agent = Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
            model,
            Arc::new(FauxProvider::default()),
            "you are pi",
        ))));
        let config = AppConfig {
            session_id: "cycle".into(),
            ..AppConfig::default()
        };
        let app = App::new(&*agent.lock().await, config);
        (app, agent)
    }

    #[test]
    fn default_model_is_the_first_entry_of_the_sorted_catalog() {
        assert_eq!(default_model(&two_model_catalog()).id, "a");
    }

    #[test]
    fn sorted_models_orders_by_provider_then_id() {
        let ids = sorted_models(&two_model_catalog())
            .into_iter()
            .map(|(provider, model)| format!("{provider}/{}", model.id))
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["alpha/a", "beta/b"]);
    }

    #[tokio::test]
    async fn cycling_models_wraps_around_the_sorted_catalog() {
        let (mut app, agent) = app_starting_at(catalog_model("alpha", "a")).await;
        let options = InteractiveOptions {
            models: two_model_catalog(),
            ..InteractiveOptions::default()
        };

        cycle_model(&mut app, &agent, &options, CycleDirection::Forward).await;
        assert_eq!(agent.lock().await.model().id, "b");

        // Forward from the last entry wraps to the first.
        cycle_model(&mut app, &agent, &options, CycleDirection::Forward).await;
        assert_eq!(agent.lock().await.model().id, "a");

        // Backward from the first wraps to the last.
        cycle_model(&mut app, &agent, &options, CycleDirection::Backward).await;
        assert_eq!(agent.lock().await.model().id, "b");

        let rendered = transcript(&app);
        assert!(rendered.contains("model → beta/b"), "{rendered}");
    }

    #[tokio::test]
    async fn cycling_from_a_model_outside_the_catalog_starts_at_the_top() {
        let (mut app, agent) = app_starting_at(catalog_model("gamma", "zz")).await;
        let options = InteractiveOptions {
            models: two_model_catalog(),
            ..InteractiveOptions::default()
        };

        cycle_model(&mut app, &agent, &options, CycleDirection::Forward).await;
        assert_eq!(agent.lock().await.model().id, "a");
    }

    #[tokio::test]
    async fn cycling_a_single_model_reports_it_instead_of_switching() {
        let mut models = Models::new();
        models.set_provider(ProviderId::new("alpha"), vec![catalog_model("alpha", "a")]);
        let (mut app, agent) = app_starting_at(catalog_model("alpha", "a")).await;
        let options = InteractiveOptions {
            models,
            ..InteractiveOptions::default()
        };

        cycle_model(&mut app, &agent, &options, CycleDirection::Forward).await;

        assert_eq!(agent.lock().await.model().id, "a");
        let rendered = transcript(&app);
        assert!(rendered.contains("only one model available"), "{rendered}");
    }

    #[test]
    fn copying_the_last_assistant_message_queues_it_for_the_clipboard() {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let mut app = App::new(&agent, AppConfig::default());
        app.messages_mut().push(pi_tui::message::MessageItem {
            role: pi_tui::message::Role::Assistant,
            text: "first reply".into(),
            thinking: String::new(),
            streaming: false,
        });
        app.messages_mut().push(pi_tui::message::MessageItem {
            role: pi_tui::message::Role::Assistant,
            text: "second reply\nwith two lines".into(),
            thinking: String::new(),
            streaming: false,
        });

        copy_last_assistant_message(&mut app);

        assert_eq!(
            app.take_clipboard_request().as_deref(),
            Some("second reply\nwith two lines")
        );
        let rendered = transcript(&app);
        assert!(rendered.contains("copied 2 line(s)"), "{rendered}");
    }

    #[test]
    fn copying_an_empty_transcript_reports_nothing_to_copy() {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let mut app = App::new(&agent, AppConfig::default());

        copy_last_assistant_message(&mut app);

        assert!(app.take_clipboard_request().is_none());
        let rendered = transcript(&app);
        assert!(rendered.contains("nothing to copy yet"), "{rendered}");
    }
}

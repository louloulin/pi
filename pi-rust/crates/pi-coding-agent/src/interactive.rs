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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use pi_agent_core::{Agent, AgentEvent, AgentOptions, RetryPolicy, ThinkingLevel};
use pi_ai::models::Models;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::{
    CompactReason, Content, ExtensionEvent, Message, Model, ProviderId, SessionEntry,
    SessionShutdownReason, ToolCall, ToolResult,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex as AsyncMutex;

use pi_session::{SessionReader, SessionWriter};

use crate::commands::resume::{
    delete_session, rename_session, SessionFilter, SessionRef, SessionSort,
};
use crate::commands::session::new_session_id;
use crate::commands::tree::{
    clone_session, fork_selector, fork_session, session_tip, tree_selector_with, CreatedSession,
    TreeFilter, TreeView,
};
use crate::commands::{handle_command, SlashCommand};
use crate::compaction::{
    compact, Compaction, CompactionError, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
};
use crate::config::{self, ConfigSources};
use crate::extensions::events::ExtensionEventMapper;
use crate::extensions::ui_bridge::{RegionPump, TuiUi};
use crate::extensions::wiring::{ExtensionReport, ExtensionRuntime};
use crate::prompt_templates::PromptTemplate;
use crate::session_log::SessionLog;
use crate::text_fallback::{run_text_fallback, FallbackReason};
use crate::tool_executor::default_executor;
use crate::tools::AgentTool;

use pi_tui::app::{App, AppConfig, ExtensionHeader, FollowUpOutcome, Submission};
use pi_tui::dialog::{Dialog, DialogAction};
use pi_tui::input::{InputEvent, KeyCode};
use pi_tui::message::Role;
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::settings::{SettingItem, SettingsList};
use pi_tui::ToolBlockRenderer;

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
    /// Session display name (`/name`), shown in the status bar and
    /// `/session`, and persisted to the session file. `None` until the
    /// user names the session.
    pub session_name: Option<String>,
    /// Path of the upstream-v4 SQLite file backing this session, when
    /// there is one. `/new` creates it; `/resume` attaches it. `/name`
    /// writes the display name into its `sessions.metadata`.
    pub session_database: Option<PathBuf>,
    /// Entry id the session's cursor sits on, when it was moved by
    /// `/tree`. `None` means the tip of the stored transcript. The next
    /// append attaches to this entry (`parent_entry_id`).
    pub session_leaf: Option<String>,
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
    /// UI-facing projection of the load pass (startup header summary +
    /// `/extensions`). Always populated by `main.rs`, including the
    /// `--no-extensions` case; empty means "nothing to show".
    pub extension_report: ExtensionReport,
    /// Interactive UI bridge for `ctx.ui.confirm / input / select`.
    /// `None` keeps the headless behaviour (deny / cancel), which is
    /// what tests and non-TTY runs get.
    pub extension_ui: Option<TuiUi>,
    /// Agent-level retry budget for the assistant call, resolved from
    /// `settings.json` by the caller (`config::load_agent_retry_policy`).
    /// Applied to the agent the TUI drives.
    pub retry: RetryPolicy,
    /// Reader answering `app.clipboard.pasteImage` (`Alt+V`). `None` uses
    /// the real [`SystemClipboard`](crate::clipboard::SystemClipboard);
    /// tests inject a fake so the image / text / empty paths can be driven
    /// without a system clipboard.
    pub clipboard: Option<Arc<dyn crate::clipboard::ClipboardReader>>,
    /// View state of the `/resume` and `/tree` pickers.
    ///
    /// The Rust port has one shared [`Selector`] for every picker and
    /// rebuilds it on each open, so the view state upstream keeps inside
    /// `SessionSelector` / `TreeSelector` (`sortMode`, `showPath`,
    /// `nameFilter`, `filterMode`, `foldedNodes`, `showLabelTimestamps`)
    /// lives here instead. Driver-internal: it is not part of the launch
    /// surface, but it must outlive a selector open.
    #[doc(hidden)]
    pub pickers: PickerState,
    /// Suppress the built-in startup header (the key-hint screen above the
    /// transcript).
    ///
    /// Upstream's switch is the `quietStartup` setting, plus its
    /// `options.verbose` override (`interactive-mode.ts:910`); the Rust CLI
    /// exposes the same thing as `--no-header`. The App still honours the
    /// runtime `app.header` chord either way.
    pub quiet_startup: bool,
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
            .field("session_name", &self.session_name)
            .field("session_database", &self.session_database)
            .field("session_leaf", &self.session_leaf)
            .field("compaction", &self.compaction)
            .field("initial_prompt", &self.initial_prompt)
            .field("prompt_templates", &self.prompt_templates.len())
            .field("stream_fn", &"<dyn StreamFn>")
            .field("tool_executor", &"<dyn ToolExecutor>")
            .field("extensions", &self.extensions.is_some())
            .field("extension_report", &self.extension_report)
            .field("extension_ui", &self.extension_ui.is_some())
            .field("retry", &self.retry)
            .field("pickers", &self.pickers)
            .field("quiet_startup", &self.quiet_startup)
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
            session_name: None,
            session_database: None,
            session_leaf: None,
            compaction: DEFAULT_COMPACTION_SETTINGS,
            initial_prompt: None,
            prompt_templates: Vec::new(),
            stream_fn: Arc::new(FauxProvider::default()) as SharedStreamFn,
            tool_executor: default_executor(),
            extensions: None,
            extension_report: ExtensionReport::default(),
            extension_ui: None,
            retry: RetryPolicy::default(),
            clipboard: None,
            pickers: PickerState::default(),
            quiet_startup: false,
        }
    }
}

/// The App configuration the interactive driver runs with.
///
/// Split out of [`run_interactive`] so the launch surface (header on or off,
/// locale, poll interval) is a pure function of the options and can be
/// asserted without a terminal — `tests/startup_header.rs` drives it against
/// the installed keybinding table.
pub fn interactive_app_config(options: &InteractiveOptions) -> AppConfig {
    AppConfig {
        prompt_placeholder: "type a prompt — /help for commands".into(),
        session_id: options.session_id.clone(),
        event_poll_interval: Duration::from_millis(50),
        markdown: true,
        // Upstream default: `copyOnSelect ?? true`.
        copy_on_select: true,
        // Let the App detect the terminal's OSC 8 support from the
        // environment (`Hyperlinks: None` = auto).
        hyperlinks: None,
        // Collapsed tool blocks preview this many tail lines before the
        // `… (+M lines, Ctrl+O to expand)` hint. `pi_tui::TOOL_PREVIEW_LINES`
        // is the 4-line default; a user setting can override it here.
        tool_preview_lines: pi_tui::TOOL_PREVIEW_LINES,
        // Upstream shows its header unless the user asked for a quiet
        // startup (`quietStartup`; the CLI spelling here is `--no-header`).
        startup_header: !options.quiet_startup,
        // First run teaches the chords it just shipped; `app.header` folds it
        // and a folded header costs no rows.
        startup_header_expanded: true,
        locale: locale_from_env(std::env::var("PI_LANG").ok().as_deref()),
        extension_header: extension_header_for(options),
        // Multi-line composer (LUM-1282): the App passes the prompt's
        // natural row count to `plan_chrome` so a wrapping buffer grows
        // the editor region rather than disappearing into the chrome
        // budget. `8` matches Martty's `min(h/2, 12)` cap on tall
        // terminals (`src/ui.rs:25-54`).
        composer_max_rows: 8,
    }
}

/// Project the extension report onto the startup header's extension row.
///
/// `--no-extensions` is its own state (the header says so); an empty report
/// hides the row, which keeps a no-extension run byte-identical to the
/// pre-Stage-71 header.
fn extension_header_for(options: &InteractiveOptions) -> ExtensionHeader {
    let report = &options.extension_report;
    if report.disabled {
        return ExtensionHeader::Disabled;
    }
    if report.loaded.is_empty() {
        return ExtensionHeader::Hidden;
    }
    let home = crate::paths::home_dir();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ExtensionHeader::Loaded {
        count: report.loaded.len(),
        names: report
            .loaded
            .iter()
            .map(|path| crate::commands::display_path(path, home.as_deref(), &cwd))
            .collect(),
    }
}

/// The startup header's copy table: `PI_LANG` (`en` / `zh`, a region suffix is
/// tolerated) wins, anything else keeps the default (English).
pub(crate) fn locale_from_env(value: Option<&str>) -> pi_tui::Locale {
    value.and_then(pi_tui::Locale::parse).unwrap_or_default()
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

    let config = interactive_app_config(&options);

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

/// Install the composer's command / file completion provider, plus the
/// commands the loaded extensions registered (`pi.registerCommand`).
///
/// Split out of [`run_loop`] so the wiring itself is testable without a
/// terminal: the LUM-1236 defect was exactly that this call did not exist,
/// and a provider-only test could not have caught it. Extension commands
/// complete through the same dropdown because they are real commands —
/// `/extensions` lists them and `handle_command` dispatch does not care who
/// registered them.
fn install_composer_autocomplete(
    app: &mut App,
    base_path: PathBuf,
    extra: Vec<pi_tui::autocomplete::SlashCommand>,
) {
    let mut commands = crate::commands::slash::autocomplete_commands();
    commands.extend(extra);
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(
            pi_tui::autocomplete::CombinedAutocompleteProvider::new(commands, base_path),
        ));
}

/// The extension-registered commands as dropdown rows.
fn extension_autocomplete_commands(
    options: &InteractiveOptions,
) -> Vec<pi_tui::autocomplete::SlashCommand> {
    options
        .extensions
        .as_ref()
        .map(|runtime| {
            runtime
                .commands()
                .iter()
                .map(|command| {
                    let entry = pi_tui::autocomplete::SlashCommand::new(command.name.clone());
                    if command.description.is_empty() {
                        entry
                    } else {
                        entry.with_description(command.description.clone())
                    }
                })
                .collect()
        })
        .unwrap_or_default()
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
    // `--resume <id>` seeds the stored session's `/name` so the status bar
    // and `/session` show it before the first command runs.
    app.set_session_name(options.session_name.clone());
    // Wire the existing rich tool renderers (`tools/render.rs`) into the
    // interactive transcript. The App cannot name that type (no
    // `pi-tui` → `pi-coding-agent` dependency), so the driver installs the
    // adapter and the App owns the folding (collapsed preview, Ctrl+O,
    // click-to-toggle). Print mode keeps its own session in `text_fallback`.
    let tool_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    app.set_tool_block_renderer(Box::new(crate::tools::InteractiveToolRenderer::new(
        tool_cwd.clone(),
    )));
    // Command / path completion for the composer. The engine (`pi-tui`
    // `autocomplete`) and the keyboard map (`tui.input.tab`) have existed
    // since LUM-1122, but nothing ever installed a provider in the binary:
    // typing `/` showed no candidates (LUM-1236). Upstream installs the same
    // `CombinedAutocompleteProvider` on the editor at startup; `tool_cwd` is
    // the base the `@` file completion walks, and the extension-registered
    // commands ride in the same table (Stage 70 / LUM-1238).
    install_composer_autocomplete(
        &mut app,
        tool_cwd,
        extension_autocomplete_commands(&options),
    );

    // Stage 67 — seed the session thinking level from the persisted
    // `defaultThinkingLevel`, clamp it to what the active model can honour,
    // and publish it to both the agent (the next provider call) and the App
    // (status bar + editor chrome).
    {
        let mut agent_guard = agent.lock().await;
        let supports = crate::thinking::model_supports_thinking(agent_guard.model());
        let level = crate::thinking::clamp_thinking_level(
            supports,
            config::load_default_thinking_level(&settings_sources()),
        );
        agent_guard.set_thinking_level(level);
        app.set_thinking_supported(supports);
        app.set_thinking_level(level);
    }

    // Extension lifecycle fan-out (LUM-1246). Extensions subscribe to the
    // *upstream* event names (`turn_start`, `tool_execution_start`, …) that
    // the JS shim exposes, but until now nothing fed the agent's own event
    // stream into the host — `pi.on(...)` only ever fired for
    // `session_start` / `resources_discover`. A second subscriber to the
    // agent's fan-out (the first belongs to the App) drives every mapped
    // event into the host on its own task, so a slow plugin cannot stall
    // rendering. Skipped entirely when no loaded extension subscribed.
    let extension_pump = start_extension_event_pump(&agent, options.extensions.as_ref()).await;

    // Local `!` / `!!` commands: one at a time, run off the render loop so
    // `Esc` can cancel them.
    let mut bash = BashRunner::default();

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

    // `None` until the first frame is on screen: the loop must draw one
    // frame before it starts honouring `render_interval`, otherwise a
    // keystroke already waiting in the tty makes the very first `poll`
    // return instantly and the alternate screen stays blank until a full
    // render interval has elapsed (LUM-1233 measured an entirely empty
    // screen when input was queued at launch).
    let mut last_render: Option<std::time::Instant> = None;
    let render_interval = Duration::from_millis(50);

    // `app.clipboard.pasteImage` reader: the driver owns the terminal, so it
    // owns the clipboard too (injectable for tests).
    let clipboard: Arc<dyn crate::clipboard::ClipboardReader> = options
        .clipboard
        .clone()
        .unwrap_or_else(|| Arc::new(crate::clipboard::SystemClipboard::new()));

    loop {
        // Drain pending agent events before drawing so the TUI sees
        // fresh state on every tick.
        app.drain_agent_events();
        // A local `!` command may have finished since the last tick; fold it
        // into the transcript (the task owns the process, the loop owns the
        // App). A no-op when no command is running.
        if bash.is_running() {
            let width = terminal.size().map(|area| area.width).unwrap_or(80);
            bash.poll(&mut app, width);
        }
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
        // A finished turn releases anything the user queued while it ran;
        // each queued prompt becomes its own turn so none is dropped.
        deliver_pending(&mut app, &agent, &mut options, &mut bash).await?;

        if app.is_exit_requested() {
            break;
        }

        // Redraw. The App's buffer path carries the theme *and* the
        // chat-log selection highlight, so draw the App straight into the
        // frame instead of round-tripping through plain snapshot lines.
        let render_due = last_render
            .map(|at| at.elapsed() >= render_interval)
            .unwrap_or(true);
        if render_due {
            terminal.draw(|frame| {
                let area = frame.area();
                app.render_to_buffer(area, frame.buffer_mut());
            })?;
            last_render = Some(std::time::Instant::now());
        }

        // Poll for crossterm events with a short timeout so the render
        // loop continues to tick, then drain whatever is already
        // buffered without blocking. `Event::read` blocks until the next
        // event, so it may only ever be called after `poll` reported one
        // (see `drain_ready_events`).
        if ct_event::poll(config.event_poll_interval)? {
            for event in drain_ready_events(ct_event::poll, ct_event::read)? {
                let translated = App::translate_event(event);
                if let Some(action) =
                    handle_input_event(&mut app, &agent, &mut options, &mut bash, translated)
                        .await?
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

        // `app.clipboard.pasteImage` (`Alt+V`): the App recorded the chord;
        // read the system clipboard off the render loop (the backends are
        // blocking) and hand the result back — image chip, or the plain-text
        // paste fallback.
        if app.take_image_paste_request() {
            let reader = clipboard.clone();
            let paste = tokio::task::spawn_blocking(move || reader.read())
                .await
                .unwrap_or(crate::clipboard::ClipboardPaste::Empty);
            apply_clipboard_paste(&mut app, paste);
        }
    }

    // Nothing is pumping dialogs any more: deny instead of queueing.
    if let Some(ui) = options.extension_ui.as_ref() {
        ui.disarm();
    }
    // Stop the fan-out, then let extensions observe the teardown. Upstream
    // emits `session_shutdown` on quit / reload / session replacement; a
    // plugin that flushes state there must not be able to hold the exit open,
    // so the delivery is bounded independently of the host's (much longer)
    // interactive timeout.
    if let Some(pump) = extension_pump {
        pump.abort();
    }
    if let Some(runtime) = options.extensions.as_ref() {
        let _ = tokio::time::timeout(
            EXTENSION_SHUTDOWN_TIMEOUT,
            runtime.deliver_shutdown(SessionShutdownReason::Quit),
        )
        .await;
    }
    Ok(InteractiveExit::UserExit)
}

/// Apply one `app.clipboard.pasteImage` clipboard read to the composer.
///
/// An image becomes a chip; otherwise the paste falls back to plain text
/// (upstream `handleClipboardPaste`, `interactive-mode.ts:2933`: it saves the
/// image to a temp file and pastes the path, and inserts the text otherwise).
fn apply_clipboard_paste(app: &mut App, paste: crate::clipboard::ClipboardPaste) {
    match paste {
        crate::clipboard::ClipboardPaste::Image(image) => {
            app.paste_image(image);
        }
        crate::clipboard::ClipboardPaste::Text(text) => app.paste_text(&text),
        // Nothing usable on the clipboard: leave the draft and the status
        // hint alone rather than flashing a spurious failure.
        crate::clipboard::ClipboardPaste::Empty => {}
    }
}

/// Internal action returned from `handle_input_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalAction {
    /// User requested exit.
    Exit,
}

// ---------------------------------------------------------------------------
// Local `!` / `!!` bash commands (LUM-1223, upstream `handleBashCommand`)
// ---------------------------------------------------------------------------

/// Monotonic id for a synthetic local-bash tool block, so two commands in one
/// transcript never collide in the App's streamed-tool map.
fn next_bash_call_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("local-bash-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// State for the local `!` / `!!` command that is currently running.
///
/// The command runs on a background task (the `bash` tool's process wait is
/// already bridged onto `spawn_blocking`) so the render loop keeps reading
/// keys while it runs; `Esc` sets the shared abort flag and the tool kills the
/// child. One command at a time, mirroring upstream's `session.isBashRunning`.
#[derive(Default)]
struct BashRunner {
    run: Option<BashRun>,
}

struct BashRun {
    /// Set to `true` by `Esc`; the tool polls it and kills the child.
    abort: Arc<AtomicBool>,
    /// Delivers the finished command's outcome back to the render loop.
    rx: tokio::sync::mpsc::UnboundedReceiver<BashOutcome>,
    /// The command text, echoed in the transcript header.
    command: String,
    /// `!!` — the result is kept out of the agent message log.
    excluded: bool,
}

struct BashOutcome {
    result: Result<crate::tools::ToolOutput, crate::tools::ToolError>,
    elapsed_ms: u64,
}

impl BashRunner {
    /// Whether a command is still in flight (finished-but-unpolled counts as
    /// running, so a second submission is refused until the block is shown).
    fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Start `command` on a background task. Returns immediately; the outcome
    /// arrives on [`BashRunner::poll`] / [`BashRunner::wait`].
    fn start(&mut self, command: String, excluded: bool) {
        let abort = Arc::new(AtomicBool::new(false));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let abort_for_task = abort.clone();
        let command_for_task = command.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let args = serde_json::json!({ "command": command_for_task });
            let result = crate::tools::BashTool
                .execute(args, crate::tools::AbortLike::from_flag(abort_for_task))
                .await;
            let _ = tx.send(BashOutcome {
                result,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        });
        self.run = Some(BashRun {
            abort,
            rx,
            command,
            excluded,
        });
    }

    /// Request cancellation of the running command (upstream
    /// `session.abortBash()`, `interactive-mode.ts:2858`).
    fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.abort.store(true, Ordering::SeqCst);
        }
    }

    /// Fold a finished command into the transcript. Returns `true` once the run
    /// was consumed, so the render loop knows there is nothing left to poll.
    fn poll(&mut self, app: &mut App, width: u16) -> bool {
        let Some(run) = self.run.as_mut() else {
            return true;
        };
        match run.rx.try_recv() {
            Ok(outcome) => {
                let run = self.run.take().expect("checked above");
                push_bash_block(app, &run.command, run.excluded, &outcome, width);
                true
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => false,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                self.run = None;
                true
            }
        }
    }

    /// Await the running command and fold it into the transcript. Used by
    /// tests and any one-shot caller that has nothing else to poll.
    #[cfg(test)]
    async fn wait(&mut self, app: &mut App, width: u16) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let outcome = run.rx.recv().await;
        let Some(run) = self.run.take() else {
            return;
        };
        if let Some(outcome) = outcome {
            push_bash_block(app, &run.command, run.excluded, &outcome, width);
        }
    }
}

/// Render a finished `!` / `!!` command into the transcript.
///
/// The block goes through the same rich renderer the model's tool calls use
/// ([`crate::tools::InteractiveToolRenderer`]) and the same App-side
/// finalizer (`MessageView::finish_tool_execution_with_lines`), so the
/// collapsed preview, `Ctrl+O` and click-to-expand all apply for free.
///
/// Nothing is written to the agent's message log: the command is local, and
/// `!!` must never reach the model. `excluded` is recorded on the block's
/// details so the two forms stay distinguishable without changing the
/// transcript text.
fn push_bash_block(
    app: &mut App,
    command: &str,
    excluded: bool,
    outcome: &BashOutcome,
    width: u16,
) {
    let (text, is_error, details) = match &outcome.result {
        Ok(output) => (
            crate::tools::get_text_output(output, false),
            false,
            output.details.clone(),
        ),
        Err(crate::tools::ToolError::Aborted) => ("command cancelled".to_string(), true, None),
        Err(err) => (err.to_string(), true, None),
    };

    let exclude_flag = serde_json::json!(excluded);
    let details = Some(match details {
        Some(mut details) => {
            if let Some(map) = details.as_object_mut() {
                map.insert("exclude_from_context".into(), exclude_flag);
            }
            details
        }
        None => serde_json::json!({ "exclude_from_context": exclude_flag }),
    });

    let call = ToolCall {
        id: next_bash_call_id(),
        name: "bash".to_string(),
        arguments: serde_json::json!({ "command": command }),
    };
    let result = ToolResult {
        tool_call_id: call.id.clone(),
        content: Box::new(Content::text(text.clone())),
        is_error,
        details,
        added_tool_names: None,
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut renderer = crate::tools::InteractiveToolRenderer::new(cwd);
    renderer.begin_tool(&call);
    let block = renderer.finish_tool(&result, width);

    let messages = app.messages_mut();
    messages.start_tool_execution(&call.id, "bash", command);
    messages.finish_tool_execution_with_lines(&call.id, outcome.elapsed_ms, &text, is_error, block);
}

/// Translate a single [`InputEvent`] into App mutations. Returns
/// `Some(InternalAction::Exit)` when the App has exited and the
/// render loop should unwind.
async fn handle_input_event(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
    event: InputEvent,
) -> anyhow::Result<Option<InternalAction>> {
    // A rename modal the driver opened itself owns the keyboard until it
    // is answered: the App would drop the resolved dialog before the new
    // name could be read.
    if app.dialog_open() && options.pickers.session.pending_rename.is_some() {
        let InputEvent::Key(key) = event else {
            return Ok(None);
        };
        handle_rename_dialog_key(app, options, key);
        return Ok(None);
    }

    // When the selector is open (and no extension dialog is on top of
    // it), handle selection first.
    if app.selector_open() && !app.dialog_open() {
        let InputEvent::Key(key) = event else {
            return Ok(None);
        };
        // The picker-scoped `app.session.*` / `app.tree.*` chords go to the
        // driver: the shared `Selector` does not know them
        // (`session-selector.ts:537-601`, `tree-selector.ts:996-1091`).
        let kind = app.selector().map(picker_kind).unwrap_or(PickerKind::Other);
        if kind != PickerKind::Other && handle_picker_key(app, options, kind, key) {
            return Ok(None);
        }
        // `app.thinking.save` (Ctrl+S) inside the thinking selector persists
        // the highlighted level as the default — upstream consumes the chord
        // in `ThinkingSelectorComponent.handleInput`, a behaviour no other
        // selector has. Intercepted here because the shared `Selector`
        // ignores it; the model selector's own Ctrl+S is a separate gap.
        let selected = app
            .selector()
            .and_then(|selector| selector.selected_value())
            .map(str::to_string);
        if let Some(value) = selected {
            let keybindings = pi_tui::keybindings::get_keybindings();
            if value.starts_with("thinking:")
                && pi_tui::keybindings::matches_with_fallback(
                    &keybindings,
                    &InputEvent::Key(key),
                    "app.thinking.save",
                    &["ctrl+s"],
                )
            {
                app.close_selector();
                apply_thinking_selector_value(
                    app,
                    agent,
                    &value,
                    true,
                    options.extensions.as_ref(),
                )
                .await;
                return Ok(None);
            }
        }
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
        // `app.model.select` (`Ctrl+L`) — upstream's chord for "Open model
        // selector" (`packages/coding-agent/src/core/keybindings.ts:116`).
        // This port used to hardcode `Ctrl+L` in `App::step_key` to clear the
        // transcript, which contradicted the header hint; `/clear` still does
        // the clearing (LUM-1245).
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.model.select",
            &["ctrl+l"],
        ) {
            open_model_selector(app, options);
            return Ok(None);
        }
        // `app.thinking.cycle` (Shift+Tab) — the same switching path
        // `/thinking` and the selector take.
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.thinking.cycle",
            &["shift+tab"],
        ) {
            handle_thinking_cycle(app, agent, options.extensions.as_ref()).await;
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
        // `app.message.followUp`: queue the editor buffer behind the
        // in-flight turn (idle: behaves like Enter).
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.message.followUp",
            &["alt+enter"],
        ) {
            handle_follow_up(app, agent, options, bash).await?;
            return Ok(None);
        }
        // `app.message.dequeue`: pull the queued prompts back into the
        // editor so they can be edited instead of waiting for the turn.
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.message.dequeue",
            &["alt+up"],
        ) {
            handle_dequeue(app);
            return Ok(None);
        }
        // `app.session.new` — the same action `/new` runs. Bound to
        // `alt+n` by the merged table (upstream leaves it unbound).
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.session.new",
            &["alt+n"],
        ) {
            start_new_session(app, agent, options).await;
            return Ok(None);
        }
        // `app.session.tree` — the same overlay `/tree` opens.
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.session.tree",
            &["alt+t"],
        ) {
            open_tree_selector(app, options);
            return Ok(None);
        }
        // `app.session.fork` — the same picker `/fork` opens.
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.session.fork",
            &["alt+f"],
        ) {
            open_fork_selector(app, options);
            return Ok(None);
        }
        // `app.session.resume` — deliberately the *same* code path as
        // `/resume` (`open_resume_selector`), never a second copy.
        if pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            &event,
            "app.session.resume",
            &["alt+r"],
        ) {
            open_resume_selector(app, options);
            return Ok(None);
        }
    }

    // `Esc` cancels a running local `!` command (upstream
    // `session.abortBash()`, `interactive-mode.ts:2857`). The App's own `Esc`
    // path only fires while an *agent turn* is in flight, and a local command
    // does not set `turn_busy`, so claim the key here before an open overlay
    // gets it.
    if bash.is_running()
        && !app.selector_open()
        && !app.dialog_open()
        && !app.settings_open()
        && !app.custom_open()
        && !app.search_open()
        && matches!(&event, InputEvent::Key(key) if key.code == KeyCode::Esc)
    {
        bash.cancel();
        app.flash_status("Cancelling bash command…");
        return Ok(None);
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
        pi_tui::app::StepOutcome::Submitted(submission) => {
            handle_submitted(app, agent, options, bash, submission).await?;
            Ok(None)
        }
        pi_tui::app::StepOutcome::Exit => Ok(Some(InternalAction::Exit)),
    }
}

/// Route submitted editor text the same way Enter does: prompt templates
/// and slash commands short-circuit before the agent, everything else
/// becomes a prompt. Shared by Enter and by an idle `app.message.followUp`
/// (upstream's `handleFollowUp` calls `editor.onSubmit` when no turn is
/// running, so both chords converge on the same handler).
async fn handle_submitted(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
    submission: Submission,
) -> anyhow::Result<()> {
    let text = submission.text.clone();
    // Local `!cmd` / `!!cmd` commands never reach the model. Upstream parses
    // them before the slash and queue branches
    // (`interactive-mode.ts:3106-3118`); a command with nothing after the
    // prefix falls through to the normal prompt path.
    if let Some(command) = pi_tui::editor::parse_bash_command(&text) {
        if !submission.images.is_empty() {
            // A local command has no image part; keep the typed text in the
            // editor and drop the chips rather than sending them nowhere.
            app.set_editor_text(&text);
            app.flash_status("Local ! commands cannot carry image attachments");
            return Ok(());
        }
        if app.is_busy() || bash.is_running() {
            // Deliberately *not* Stage 61's pending queue: a bash command
            // cannot start while something else runs, so the text goes back
            // to the editor and the user is told (upstream `editor.setText`
            // + `showWarning`).
            app.set_editor_text(&text);
            app.flash_status("A bash command is already running. Press Esc to cancel it first.");
            return Ok(());
        }
        // Upstream memoises the raw line including its prefix.
        app.prompt_mut().push_history(text);
        // Upstream `user_bash`: plugins see local commands too (a shell
        // history / audit extension is the usual consumer).
        deliver_extension_event(
            options.extensions.as_ref(),
            ExtensionEvent::UserBash {
                command: command.command.clone(),
                exclude_from_context: command.excluded,
                cwd: std::env::current_dir()
                    .map(|cwd| cwd.display().to_string())
                    .unwrap_or_default(),
            },
        )
        .await;
        bash.start(command.command, command.excluded);
        return Ok(());
    }
    if text.starts_with('/') {
        if !submission.images.is_empty() {
            // Slash commands take a string, not a message; there is nowhere
            // to hand the attachments, so say so instead of dropping them
            // silently.
            app.flash_status("Images are ignored for slash commands");
        }
        // Prompt templates take precedence over built-in slash
        // commands, mirroring the TS CLI: `/<name>` expands to
        // the template body when a template with that name was
        // loaded, otherwise the text falls through to the
        // built-in / extension command dispatch.
        if let Some((template, args)) =
            crate::prompt_templates::find_prompt_template(&text, &options.prompt_templates)
        {
            let parsed = crate::prompt_templates::parse_command_args(&args);
            let expanded = crate::prompt_templates::substitute_args(&template.content, &parsed);
            app.submit(agent.clone(), expanded);
        } else {
            run_slash_command(app, agent, options, &text).await?;
        }
    } else {
        // The plain prompt path: the draft keeps its image chips, which
        // `App::submit` turns into the `UserMessage`'s image blocks.
        app.submit(agent.clone(), submission);
    }
    Ok(())
}

/// Handle `app.message.followUp` (upstream `handleFollowUp`). While a turn
/// is in flight the editor buffer is queued; when the App is idle the chord
/// is exactly Enter, so the text goes through [`handle_submitted`].
async fn handle_follow_up(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
) -> anyhow::Result<()> {
    match app.follow_up_from_editor() {
        FollowUpOutcome::Empty | FollowUpOutcome::Queued => Ok(()),
        FollowUpOutcome::RefusedImages => {
            app.flash_status("Cannot attach images while a turn is running");
            Ok(())
        }
        FollowUpOutcome::Submitted(submission) => {
            handle_submitted(app, agent, options, bash, submission).await
        }
    }
}

/// Handle `app.message.dequeue` (upstream `restoreQueuedMessagesToEditor`).
/// The restored prompts land in the editor, in delivery order, with any
/// text already there kept at the end. The status line matches upstream's
/// wording verbatim.
fn handle_dequeue(app: &mut App) {
    let restored = app.restore_pending_to_editor();
    if restored == 0 {
        app.flash_status("No queued messages to restore");
    } else {
        app.flash_status(format!(
            "Restored {restored} queued message{} to editor",
            if restored > 1 { "s" } else { "" }
        ));
    }
}

/// Deliver one queued prompt once the in-flight turn has finished
/// (upstream's `getSteeringMessages` / `getFollowUpMessages` drain at the
/// inner-loop boundary). Delivery is serial: each queued prompt becomes its
/// own turn, so the order the user typed them in is preserved and nothing
/// is dropped.
async fn deliver_pending(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
) -> anyhow::Result<()> {
    if app.is_busy() || app.pending_len() == 0 {
        return Ok(());
    }
    let Some(text) = app.take_next_pending() else {
        return Ok(());
    };
    // The pending queue is text-only; a drained prompt therefore has no
    // image chips to carry (`Submission::from`).
    handle_submitted(app, agent, options, bash, text.into()).await
}

/// The status line upstream prints when `app.thinking.cycle` has nowhere to
/// go (`interactive-mode.ts:4179`).
const UNSUPPORTED_THINKING_STATUS: &str = "Current model does not support thinking";

/// Re-derive the thinking level after a model switch: recompute the support
/// flag, then clamp the session level to what the new model can honour.
///
/// `app` and `agent` must already reflect the new model (call this right after
/// [`App::queue_model_switch`]). Upstream keeps a per-model level in
/// `modelThinkingLevels`; this port has no such memory yet, so the session
/// level carries over and is clamped.
fn sync_thinking_for_model(app: &mut App, agent: &mut Agent) {
    let supports = crate::thinking::model_supports_thinking(agent.model());
    let level = crate::thinking::clamp_thinking_level(supports, app.thinking_level());
    agent.set_thinking_level(level);
    app.set_thinking_supported(supports);
    app.set_thinking_level(level);
}

/// `app.thinking.cycle` — the Shift+Tab cycle.
///
/// Upstream `cycleThinkingLevel` (`interactive-mode.ts:4177`): a model that
/// cannot reason has no next level, so the request is reported instead of
/// silently ignored. Everything else funnels through
/// [`apply_thinking_level`], the same path `/thinking` and the selector use.
async fn handle_thinking_cycle(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    extensions: Option<&Arc<ExtensionRuntime>>,
) {
    let supports = {
        let agent_guard = agent.lock().await;
        crate::thinking::model_supports_thinking(agent_guard.model())
    };
    // `cycle_thinking_level` returns `None` exactly when the model cannot
    // reason. `Max` then routes through the shared path, which reports the
    // limitation (`UNSUPPORTED_THINKING_STATUS`) instead of echoing a level
    // the model would ignore.
    let next = crate::thinking::cycle_thinking_level(supports, app.thinking_level())
        .unwrap_or(ThinkingLevel::Max);
    apply_thinking_level(app, agent, next, false, extensions).await;
}

/// The single switching code path behind every thinking-level entry point.
///
/// Mirrors upstream `AgentSession::setThinkingLevel`
/// (`agent-session.ts:1814`) plus the status lines `selectThinkingLevel`
/// prints (`interactive-mode.ts:4806`): the requested level is clamped to what
/// the model supports, published to the agent (the next provider call) and to
/// the App (status bar + editor chrome), and — when `persist` is set — written
/// to `defaultThinkingLevel`.
async fn apply_thinking_level(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    level: ThinkingLevel,
    persist: bool,
    extensions: Option<&Arc<ExtensionRuntime>>,
) {
    let previous = app.thinking_level();
    let mut agent_guard = agent.lock().await;
    let supports = crate::thinking::model_supports_thinking(agent_guard.model());
    let clamped = crate::thinking::clamp_thinking_level(supports, level);
    agent_guard.set_thinking_level(clamped);
    drop(agent_guard);
    app.set_thinking_supported(supports);
    app.set_thinking_level(clamped);

    // Upstream `thinking_level_select`: plugins track the reasoning budget
    // (e.g. to annotate transcripts). Report the level that is actually in
    // force, not the one that was requested.
    if clamped != previous {
        deliver_extension_event(
            extensions,
            ExtensionEvent::ThinkingLevelSelect {
                level: clamped.as_str().to_string(),
                previous_level: previous.as_str().to_string(),
            },
        )
        .await;
    }

    if persist && !persist_default_thinking_level(app, &settings_sources(), level) {
        return;
    }

    if !supports && level.is_reasoning() {
        // An attempt to raise the level on a model that cannot reason reports
        // the limitation rather than echoing a level that will be ignored.
        app.flash_status(UNSUPPORTED_THINKING_STATUS);
    } else if persist {
        app.flash_status(format!(
            "Default thinking level: {}",
            crate::thinking::level_label(level)
        ));
    } else {
        app.flash_status(format!(
            "Thinking level: {}",
            crate::thinking::level_label(clamped)
        ));
    }
}

/// Apply a `/thinking` selector value (`thinking:<level>`).
///
/// `persist` distinguishes Enter (in-session) from `app.thinking.save`
/// (Ctrl+S), matching upstream's `onSelect` / `onSelectAsDefault` callbacks.
async fn apply_thinking_selector_value(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    value: &str,
    persist: bool,
    extensions: Option<&Arc<ExtensionRuntime>>,
) {
    let Some(level) = value
        .strip_prefix("thinking:")
        .and_then(crate::thinking::parse_thinking_level)
    else {
        return;
    };
    apply_thinking_level(app, agent, level, persist, extensions).await;
}

/// Write `level` to `defaultThinkingLevel` in the discovered settings
/// locations, reporting a failure through the message view.
///
/// Returns `false` when the write failed (the caller then skips the status
/// flash). Split out of [`apply_thinking_level`] so the persistence rule is
/// testable against a throwaway settings file; upstream persists the
/// *requested* level, not the clamped one (`setThinkingLevel(level, {
/// persist: true })`, `agent-session.ts:1814`), so a level the current model
/// cannot honour still becomes the default for the models that can.
fn persist_default_thinking_level(
    app: &mut App,
    sources: &ConfigSources,
    level: ThinkingLevel,
) -> bool {
    match config::save_default_thinking_level(sources, level) {
        Ok(_) => true,
        Err(err) => {
            app.info(format!("/thinking: {err}"));
            false
        }
    }
}

/// Open the `/thinking` selector — upstream `showThinkingSelector`
/// (`interactive-mode.ts:4817`), reusing the shared [`Selector`] the model
/// picker uses.
fn open_thinking_selector(app: &mut App, supports: bool) {
    let current = app.thinking_level();
    let default = config::load_default_thinking_level(&settings_sources());
    let available = crate::thinking::available_thinking_levels(supports);
    let items = available
        .iter()
        .map(|level| {
            let marker = if *level == current { '✓' } else { ' ' };
            let description = if *level == default {
                format!("{} · default", crate::thinking::level_description(*level))
            } else {
                crate::thinking::level_description(*level).to_string()
            };
            SelectorItem::new(
                format!("thinking:{}", level.as_str()),
                format!("{marker} {}", level.as_str()),
            )
            .with_description(description)
        })
        .collect::<Vec<_>>();
    let mut selector = Selector::new("Thinking Level", items).searchable(true);
    // Upstream opens with the current level highlighted, so Enter on an
    // untouched list keeps the level (`setSelectedIndex`).
    if let Some(index) = available.iter().position(|level| *level == current) {
        for _ in 0..index {
            selector.next();
        }
    }
    app.open_selector(selector);
}

/// Open the `/model` selector — upstream `showModelSelector`
/// (`interactive-mode.ts`), reusing the shared [`Selector`] the thinking
/// picker also uses.
///
/// Shared by the `/model` command and the `app.model.select` chord: upstream
/// binds that action to `Ctrl+L`
/// (`packages/coding-agent/src/core/keybindings.ts:116`, "Open model
/// selector"), so both entry points must land on one implementation rather
/// than a second copy (LUM-1245).
fn open_model_selector(app: &mut App, options: &InteractiveOptions) {
    let items = sorted_models(&options.models)
        .into_iter()
        .map(|(provider, model)| {
            let label = model.label.clone().unwrap_or_else(|| model.id.clone());
            SelectorItem::new(format!("model:{}", model.id), label)
                .with_description(provider.to_string())
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        app.info("no models available".to_string());
        return;
    }
    // Upstream `/model` is searchable and windows at 10 rows
    // (`model-selector.ts`: `maxVisible = 10`).
    let selector = Selector::new("Pick a model", items)
        .searchable(true)
        .with_max_visible(10);
    app.open_selector(selector);
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
    sync_thinking_for_model(app, &mut agent_guard);
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
    options: &mut InteractiveOptions,
) {
    if let Some(id) = value.strip_prefix("model:") {
        let mut found = None;
        for (provider, model) in options.models.iter() {
            if model.id == id {
                found = Some((provider.clone(), model.clone()));
                break;
            }
        }
        if let Some((_, model)) = found {
            let mut agent_guard = agent.lock().await;
            app.queue_model_switch(&mut agent_guard, model);
            sync_thinking_for_model(app, &mut agent_guard);
            drop(agent_guard);
            app.info(format!("model → {}", id));
        }
    } else if value.starts_with("thinking:") {
        // The `/thinking` selector reuses the shared [`Selector`]; its values
        // carry the `thinking:` prefix.
        apply_thinking_selector_value(app, agent, value, false, options.extensions.as_ref()).await;
    } else if let Some(session_id) = value.strip_prefix("resume:") {
        resume_session(app, agent, options, session_id).await;
    } else if let Some(entry_id) = value.strip_prefix("tree:") {
        handle_tree_selection(app, agent, options, entry_id).await;
    } else if let Some(entry_id) = value.strip_prefix("fork:") {
        handle_fork_selection(app, agent, options, entry_id).await;
    }
}

/// Attach the TUI to a stored session picked from `/resume`.
///
/// Restores the transcript (the rendered view *and* the agent's model
/// context), the session identity and its `/name`, so a named session
/// keeps its name across a resume.
async fn resume_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    session_id: &str,
) {
    let Some(directory) = session_directory(options) else {
        app.info("/resume: session directory not configured".to_string());
        return;
    };
    let reference = match crate::list_resumable(&directory) {
        Ok(refs) => refs.into_iter().find(|r| r.session_id == session_id),
        Err(err) => {
            app.info(format!("/resume: {err}"));
            return;
        }
    };
    let Some(reference) = reference else {
        app.info(format!("/resume: session {session_id} not found"));
        return;
    };
    let reader = match SessionReader::open(&reference.database) {
        Ok(reader) => reader,
        Err(err) => {
            app.info(format!("/resume: {err}"));
            return;
        }
    };
    match attach_session(app, agent, options, reference.database, session_id, &reader).await {
        Ok(Some(name)) => app.info(format!("Resumed session {session_id} ({name})")),
        Ok(None) => app.info(format!("Resumed session {session_id}")),
        Err(err) => app.info(format!("/resume: {err}")),
    }
}

/// The directory session files live in, when one is configured.
fn session_directory(options: &InteractiveOptions) -> Option<PathBuf> {
    options
        .session_log
        .as_ref()
        .map(|log| log.directory().to_path_buf())
}

/// Point the TUI and the agent at a stored session: transcript, model
/// context, identity, `/name` and the JSONL trail. `/resume`, `/fork`
/// and `/clone` all funnel through here so a resumed and a freshly
/// branched session behave identically.
///
/// Returns the session's display name when it has one.
async fn attach_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    database: PathBuf,
    session_id: &str,
    reader: &SessionReader,
) -> anyhow::Result<Option<String>> {
    let entries = reader
        .iter_entries(session_id)?
        .into_iter()
        .map(|decoded| decoded.entry)
        .collect::<Vec<SessionEntry>>();
    let name = reader.session_name(session_id).ok().flatten();
    let messages = entries_to_messages(&entries);

    app.messages_mut().clear();
    for entry in &entries {
        if let Some(item) = entry_to_item(entry) {
            app.messages_mut().push(item);
        }
    }
    agent.lock().await.state_mut().messages = messages;

    options.session_id = session_id.to_string();
    options.session_name = name.clone();
    options.session_database = Some(database);
    options.session_leaf = None;
    // Keep the JSONL trail aimed at the session we just attached to.
    if let Some(directory) = session_directory(options) {
        if let Ok(log) = SessionLog::open(&directory, session_id) {
            options.session_log = Some(log);
        }
    }
    app.set_session_id(session_id);
    app.set_session_name(name.clone());
    Ok(name)
}

/// Open the session database the driver is currently attached to.
fn open_current_session(
    app: &mut App,
    options: &InteractiveOptions,
    verb: &str,
) -> Option<(PathBuf, SessionReader)> {
    let Some(database) = options.session_database.clone() else {
        app.info(format!("{verb}: no session database"));
        return None;
    };
    match SessionReader::open(&database) {
        Ok(reader) => Some((database, reader)),
        Err(err) => {
            app.info(format!("{verb}: {err}"));
            None
        }
    }
}

/// View state of the `/resume` session picker.
///
/// Upstream keeps this inside `SessionSelector`
/// (`session-selector.ts`: `sortMode`, `showPath`, `nameFilter`,
/// `confirmingDeletePath`). The Rust port has one shared [`Selector`] for
/// every picker, so the state lives in the driver and the selector is
/// rebuilt in place whenever it changes.
#[derive(Debug, Default)]
struct SessionPickerState {
    /// Sort mode (`app.session.toggleSort`).
    sort: SessionSort,
    /// Show the session file path in the description
    /// (`app.session.togglePath`).
    show_path: bool,
    /// All sessions, or only the named ones
    /// (`app.session.toggleNamedFilter`).
    filter: SessionFilter,
    /// Session waiting for its second delete chord
    /// (`app.session.delete` / `app.session.deleteNoninvasive`).
    pending_delete: Option<String>,
    /// Session being renamed through the input dialog, plus the reply
    /// channel that keeps the App from treating the dialog as abandoned
    /// (`App::poll_ui_dialogs` closes dialogs whose host stopped
    /// listening).
    pending_rename: Option<PendingRename>,
}

/// A rename awaiting the input dialog's answer.
struct PendingRename {
    /// The session the typed name belongs to.
    session: SessionRef,
    /// Kept alive (never awaited) so the dialog is not reaped.
    _reply: tokio::sync::oneshot::Receiver<Option<pi_protocol::UiResponse>>,
}

impl std::fmt::Debug for PendingRename {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRename")
            .field("session", &self.session.session_id)
            .finish_non_exhaustive()
    }
}

/// Everything the driver remembers about the pickers it clips chords off.
///
/// One struct rather than free variables so the loop owns a single value
/// that outlives every selector open — upstream's selector components
/// live for the whole session; the port's selectors are rebuilt on every
/// open.
#[derive(Debug, Default)]
#[doc(hidden)]
pub struct PickerState {
    /// `/resume` picker view state.
    session: SessionPickerState,
    /// `/tree` picker view state (filter, folds, label timestamps).
    tree: TreeView,
}

/// Which picker is open, decided from the item values rather than the
/// title so a search filter never changes the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerKind {
    /// The `/resume` picker (`resume:*` values).
    Session,
    /// The `/tree` overlay (`tree:*` values).
    Tree,
    /// Any other selector (`/model`, `/thinking`, `/fork`, extension
    /// dialogs): no picker chords apply.
    Other,
}

/// Classify the open selector, or `Other` when none is open.
fn picker_kind(selector: &Selector) -> PickerKind {
    if let Some(value) = selector.items().first().map(|item| item.value.as_str()) {
        if value.starts_with("resume:") {
            return PickerKind::Session;
        }
        if value.starts_with("tree:") {
            return PickerKind::Tree;
        }
        // A populated picker with other values (`model:`, `thinking:`, …)
        // is never one of ours, whatever its title says.
        return PickerKind::Other;
    }
    // An empty picker is still ours: a filter that hid every session, or a
    // session with no entries for `/tree`. The chord that undoes the filter
    // must keep working, so fall back to the title.
    if selector.title().starts_with("Pick a session to resume") {
        PickerKind::Session
    } else if selector.title() == "Session tree" {
        PickerKind::Tree
    } else {
        PickerKind::Other
    }
}

/// Handle the picker-scoped chord `key` for the open selector.
///
/// Returns `true` when the key was consumed. These are the `app.session.*`
/// and `app.tree.*` ids upstream consumes inside its selector components
/// (`session-selector.ts:537-601`, `tree-selector.ts:996-1091`); the
/// shared [`Selector`] knows nothing about them, so the driver claims them
/// before it forwards the key.
fn handle_picker_key(
    app: &mut App,
    options: &mut InteractiveOptions,
    kind: PickerKind,
    key: pi_tui::input::Key,
) -> bool {
    let keybindings = pi_tui::keybindings::get_keybindings();
    let event = InputEvent::Key(key);
    // The built-in chords stand in when the installed table does not
    // define the id (a bare `pi-tui` registry in tests), and the
    // coding-agent table wins when it does.
    let matches = |id: &str, builtin: &[&str]| {
        pi_tui::keybindings::matches_with_fallback(&keybindings, &event, id, builtin)
    };

    match kind {
        PickerKind::Session => {
            // An open delete confirmation swallows every other key
            // (upstream `session-selector.ts:537-547`).
            if let Some(pending) = options.pickers.session.pending_delete.clone() {
                if matches("tui.select.confirm", &["enter"]) {
                    options.pickers.session.pending_delete = None;
                    confirm_session_delete(app, options, &pending);
                } else if matches("tui.select.cancel", &["escape"]) {
                    options.pickers.session.pending_delete = None;
                    refresh_session_selector(app, options);
                }
                return true;
            }
            if matches("app.session.toggleSort", &["ctrl+s"]) {
                options.pickers.session.sort = options.pickers.session.sort.next();
                refresh_session_selector(app, options);
                return true;
            }
            if matches("app.session.toggleNamedFilter", &["ctrl+n"]) {
                options.pickers.session.filter = options.pickers.session.filter.toggled();
                refresh_session_selector(app, options);
                return true;
            }
            if matches("app.session.togglePath", &["ctrl+p"]) {
                options.pickers.session.show_path = !options.pickers.session.show_path;
                refresh_session_selector(app, options);
                return true;
            }
            if matches("app.session.rename", &["ctrl+r"]) {
                if let Some(session) = selected_session(app, options) {
                    options.pickers.session.pending_rename = Some(open_rename_dialog(app, session));
                }
                return true;
            }
            if matches("app.session.delete", &["ctrl+d"]) {
                begin_session_delete(app, options);
                return true;
            }
            if matches("app.session.deleteNoninvasive", &["ctrl+backspace"]) {
                // Upstream forwards the chord to the search input while a
                // query is typed, and only treats it as "delete" when the
                // query is empty (`session-selector.ts:592-601`).
                let query = app
                    .selector()
                    .map(|selector| selector.filter().to_string())
                    .unwrap_or_default();
                if query.is_empty() {
                    begin_session_delete(app, options);
                    return true;
                }
                return false;
            }
            false
        }
        PickerKind::Tree => {
            if matches("app.tree.foldOrUp", &["ctrl+left", "alt+left"]) {
                return tree_fold_or_up(app, options);
            }
            if matches("app.tree.unfoldOrDown", &["ctrl+right", "alt+right"]) {
                return tree_unfold_or_down(app, options);
            }
            if matches("app.tree.toggleLabelTimestamp", &["shift+t"]) {
                options.pickers.tree.show_label_timestamps =
                    !options.pickers.tree.show_label_timestamps;
                refresh_tree_selector(app, options);
                return true;
            }
            let direct: [(&str, &[&str], TreeFilter); 5] = [
                ("app.tree.filter.default", &["ctrl+d"], TreeFilter::Default),
                ("app.tree.filter.noTools", &["ctrl+t"], TreeFilter::NoTools),
                (
                    "app.tree.filter.userOnly",
                    &["ctrl+u"],
                    TreeFilter::UserOnly,
                ),
                (
                    "app.tree.filter.labeledOnly",
                    &["ctrl+l"],
                    TreeFilter::LabeledOnly,
                ),
                ("app.tree.filter.all", &["ctrl+a"], TreeFilter::All),
            ];
            for (id, builtin, mode) in direct {
                if matches(id, builtin) {
                    // Upstream's direct chords are toggles: pressing the
                    // active mode falls back to `default`
                    // (`tree-selector.ts:1044-1062`), except the explicit
                    // `default` chord which always resets.
                    options.pickers.tree.filter =
                        if mode != TreeFilter::Default && options.pickers.tree.filter == mode {
                            TreeFilter::Default
                        } else {
                            mode
                        };
                    options.pickers.tree.folded.clear();
                    refresh_tree_selector(app, options);
                    return true;
                }
            }
            if matches("app.tree.filter.cycleForward", &["ctrl+o"]) {
                options.pickers.tree.filter = options.pickers.tree.filter.next();
                options.pickers.tree.folded.clear();
                refresh_tree_selector(app, options);
                return true;
            }
            if matches("app.tree.filter.cycleBackward", &["shift+ctrl+o"]) {
                options.pickers.tree.filter = options.pickers.tree.filter.prev();
                options.pickers.tree.folded.clear();
                refresh_tree_selector(app, options);
                return true;
            }
            false
        }
        PickerKind::Other => false,
    }
}

/// The [`SessionRef`] the session picker highlights, resolved by session
/// id against the current listing.
fn selected_session(app: &App, options: &InteractiveOptions) -> Option<SessionRef> {
    let id = app
        .selector()
        .and_then(|selector| selector.selected_value())
        .and_then(|value| value.strip_prefix("resume:"))
        .map(str::to_string)?;
    let directory = session_directory(options)?;
    crate::list_resumable(&directory)
        .ok()?
        .into_iter()
        .find(|session| session.session_id == id)
}

/// First chord of the two-step delete: refuse the live session outright
/// (upstream `Cannot delete the currently active session`,
/// `session-selector.ts:398-402`) and otherwise arm the confirmation.
fn begin_session_delete(app: &mut App, options: &mut InteractiveOptions) {
    let Some(session) = selected_session(app, options) else {
        return;
    };
    if is_live_session(options, &session) {
        app.info("Cannot delete the currently active session".to_string());
        return;
    }
    options.pickers.session.pending_delete = Some(session.session_id);
    refresh_session_selector(app, options);
}

/// Second chord of the two-step delete.
fn confirm_session_delete(app: &mut App, options: &mut InteractiveOptions, session_id: &str) {
    let Some(directory) = session_directory(options) else {
        return;
    };
    let Some(session) = crate::list_resumable(&directory)
        .ok()
        .and_then(|refs| refs.into_iter().find(|s| s.session_id == session_id))
    else {
        app.info(format!("/resume: session {session_id} disappeared"));
        refresh_session_selector(app, options);
        return;
    };
    let keep_file = options.session_database.clone();
    match delete_session(&session, keep_file.as_deref()) {
        Ok((rows, file_removed)) => {
            app.info(format!(
                "Deleted session {session_id} ({rows} rows{})",
                if file_removed { ", file removed" } else { "" }
            ));
        }
        Err(err) => app.info(format!("/resume: could not delete {session_id}: {err}")),
    }
    refresh_session_selector(app, options);
}

/// Whether `session` is the one the running TUI is attached to.
fn is_live_session(options: &InteractiveOptions, session: &SessionRef) -> bool {
    options.session_id == session.session_id
        || options
            .session_database
            .as_deref()
            .is_some_and(|path| path == session.database)
}

/// Open the rename input modal for `session` and return the pending
/// rename bookkeeping.
fn open_rename_dialog(app: &mut App, session: SessionRef) -> PendingRename {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let request = pi_protocol::UiRequest::Input {
        title: format!("Rename session {}", session.session_id),
        placeholder: session.name.clone(),
    };
    if !app.open_dialog(Dialog::new(request, tx)) {
        app.info("Rename: another dialog is already open".to_string());
    }
    PendingRename {
        session,
        _reply: rx,
    }
}

/// Drive the rename modal for one key and apply the answer.
///
/// The dialog is taken out of the App so its answer can be read: the App
/// drops a resolved dialog (`App::step_dialog`), and the rename must not
/// be lost with it. An unresolved key puts the dialog straight back.
fn handle_rename_dialog_key(
    app: &mut App,
    options: &mut InteractiveOptions,
    key: pi_tui::input::Key,
) -> bool {
    let Some(pending) = options.pickers.session.pending_rename.take() else {
        return false;
    };
    let Some(mut dialog) = app.take_dialog() else {
        options.pickers.session.pending_rename = Some(pending);
        return false;
    };
    match dialog.handle_key(key) {
        DialogAction::Resolved(response) => {
            let value = match response {
                Some(pi_protocol::UiResponse::Input { value }) => value,
                _ => String::new(),
            };
            let name = crate::interactive::normalize_session_name(&value);
            if name.is_empty() {
                app.info("Rename cancelled".to_string());
            } else {
                match rename_session(&pending.session, &name) {
                    Ok(()) => app.info(format!(
                        "Renamed session {} to {name:?}",
                        pending.session.session_id
                    )),
                    Err(err) => app.info(format!("Rename failed: {err}")),
                }
            }
            refresh_session_selector(app, options);
            true
        }
        _ => {
            let _ = app.open_dialog(dialog);
            options.pickers.session.pending_rename = Some(pending);
            true
        }
    }
}

/// `app.tree.foldOrUp`: fold the highlighted branch, else jump to the
/// previous branch segment start (`tree-selector.ts:1002-1009`).
fn tree_fold_or_up(app: &mut App, options: &mut InteractiveOptions) -> bool {
    let Some(entry_id) = selected_tree_entry(app) else {
        return true;
    };
    let rows = current_tree_rows(options);
    let Some(index) = rows.iter().position(|row| row.value == entry_id) else {
        return true;
    };
    if rows[index].foldable && !options.pickers.tree.folded.contains(&entry_id) {
        options.pickers.tree.folded.insert(entry_id);
        refresh_tree_selector(app, options);
        return true;
    }
    if let Some(target) = branch_segment_start(&rows, index, TreeSegment::Up) {
        set_tree_cursor(app, &rows, target);
    }
    true
}

/// `app.tree.unfoldOrDown`: unfold the highlighted branch, else jump to
/// the next branch segment start (`tree-selector.ts:1010-1017`).
fn tree_unfold_or_down(app: &mut App, options: &mut InteractiveOptions) -> bool {
    let Some(entry_id) = selected_tree_entry(app) else {
        return true;
    };
    if options.pickers.tree.folded.remove(&entry_id) {
        refresh_tree_selector(app, options);
        return true;
    }
    let rows = current_tree_rows(options);
    let Some(index) = rows.iter().position(|row| row.value == entry_id) else {
        return true;
    };
    if let Some(target) = branch_segment_start(&rows, index, TreeSegment::Down) {
        set_tree_cursor(app, &rows, target);
    }
    true
}

/// Direction for the branch-segment jump the fold chords fall back to.
#[derive(Debug, Clone, Copy)]
enum TreeSegment {
    /// Previous segment start (the fold chord when nothing folds).
    Up,
    /// Next segment start (the unfold chord when nothing unfolds).
    Down,
}

/// The visible rows of the current tree view, or none when the session
/// database cannot be read.
fn current_tree_rows(options: &InteractiveOptions) -> Vec<pi_tui::tree::TreeRow> {
    let Some(database) = options.session_database.clone() else {
        return Vec::new();
    };
    let Ok(reader) = SessionReader::open(&database) else {
        return Vec::new();
    };
    let active_leaf = options
        .session_leaf
        .clone()
        .or_else(|| session_tip(&reader, &options.session_id).ok().flatten());
    crate::commands::tree::tree_rows(
        &reader,
        &options.session_id,
        active_leaf.as_deref(),
        &options.pickers.tree,
    )
    .unwrap_or_default()
}

/// The entry id the tree overlay highlights (`tree:<entry_id>`).
fn selected_tree_entry(app: &App) -> Option<String> {
    app.selector()
        .and_then(|selector| selector.selected_value())
        .and_then(|value| value.strip_prefix("tree:"))
        .map(str::to_string)
}

/// Index of the nearest visible parent of `index`.
///
/// The port draws the tree with indent prefixes instead of the parent
/// maps upstream keeps, so the nearest preceding row one indent shallower
/// *is* the parent.
fn visible_parent(rows: &[pi_tui::tree::TreeRow], index: usize) -> Option<usize> {
    if rows[index].indent == 0 {
        return None;
    }
    let depth = rows[index].indent - 1;
    (0..index).rev().find(|&idx| rows[idx].indent == depth)
}

/// Number of visible children of `index` (the rows below it one indent
/// deeper, up to the first row at its own depth or shallower).
fn visible_children_count(rows: &[pi_tui::tree::TreeRow], index: usize) -> usize {
    let depth = rows[index].indent + 1;
    let mut count = 0;
    for row in &rows[index + 1..] {
        if row.indent < depth {
            break;
        }
        if row.indent == depth {
            count += 1;
        }
    }
    count
}

/// Index of the first visible child of `index`.
fn first_visible_child(rows: &[pi_tui::tree::TreeRow], index: usize) -> Option<usize> {
    let depth = rows[index].indent + 1;
    for (offset, row) in rows[index + 1..].iter().enumerate() {
        if row.indent < depth {
            return None;
        }
        if row.indent == depth {
            return Some(index + 1 + offset);
        }
    }
    None
}

/// Upstream `findBranchSegmentStart` (`tree-selector.ts:1127-1158`): a
/// segment start is the first child of a branch point. `down` descends
/// through single-child chains to the first leaf or branch point; `up`
/// climbs the visible parents to the first segment start above the
/// cursor, falling back to the root.
fn branch_segment_start(
    rows: &[pi_tui::tree::TreeRow],
    index: usize,
    direction: TreeSegment,
) -> Option<usize> {
    if rows.is_empty() || index >= rows.len() {
        return None;
    }
    match direction {
        TreeSegment::Down => {
            let mut current = index;
            loop {
                let children = visible_children_count(rows, current);
                if children == 0 {
                    return Some(current);
                }
                if children > 1 {
                    return first_visible_child(rows, current);
                }
                current = first_visible_child(rows, current)?;
            }
        }
        TreeSegment::Up => {
            let mut current = index;
            loop {
                let Some(parent) = visible_parent(rows, current) else {
                    return Some(current);
                };
                if visible_children_count(rows, parent) > 1 && current < index {
                    return Some(current);
                }
                current = parent;
            }
        }
    }
}

/// Move the tree cursor onto `target`, i.e. the row whose value is
/// `rows[target].value`.
///
/// The selector's own items are the same rows in the same order (the
/// plain `/tree` overlay has no typed search filter when the fold chords
/// run, because a typed query would have reordered the fuzzy hits), so
/// the row index maps onto the item index directly.
fn set_tree_cursor(app: &mut App, rows: &[pi_tui::tree::TreeRow], target: usize) {
    let Some(value) = rows.get(target).map(|row| format!("tree:{}", row.value)) else {
        return;
    };
    let Some(selector) = app.selector_mut() else {
        return;
    };
    if let Some(index) = selector.items().iter().position(|item| item.value == value) {
        selector.set_cursor(index);
    }
}
/// `/resume` and `app.session.resume` share this one code path: it lists
/// the stored sessions through `list_resumable` and opens the picker.
/// Values are `resume:<session_id>`.
fn open_resume_selector(app: &mut App, options: &mut InteractiveOptions) {
    let Some(dir) = session_directory(options) else {
        app.info("/resume: session directory not configured".to_string());
        return;
    };
    // A pending delete confirmation belongs to the previous visit.
    options.pickers.session.pending_delete = None;
    match build_session_selector(&dir, &options.pickers.session) {
        Ok(selector) => app.open_selector(selector),
        Err(err) => app.info(format!("/resume: {err}")),
    }
}

/// Build the `/resume` picker for the current [`SessionPickerState`].
///
/// Returns `Err` when the session directory cannot be listed. An empty
/// list is *not* an error: the selector is still built (with no items and
/// a footer explaining which filter hid everything), so the user can
/// clear the named filter with the chord the footer names instead of
/// being thrown back to the prompt with no way back.
fn build_session_selector(
    directory: &Path,
    state: &SessionPickerState,
) -> anyhow::Result<Selector> {
    let mut refs = crate::list_resumable(directory)?;
    refs.retain(|session| state.filter.accepts(session));
    state.sort.apply(&mut refs);
    let items = refs
        .iter()
        .map(|reference| {
            let confirming = state.pending_delete.as_deref() == Some(reference.session_id.as_str());
            let label = if confirming {
                format!("⚠ {}", reference.session_id)
            } else {
                reference.session_id.clone()
            };
            SelectorItem::new(format!("resume:{}", reference.session_id), label)
                .with_description(reference.display_with(state.show_path))
        })
        .collect::<Vec<_>>();
    Ok(Selector::new(session_picker_title(state), items)
        .searchable(true)
        // Upstream `session-selector.ts`: `maxVisible = 10`.
        .with_max_visible(10)
        .with_footer(session_picker_footer(state)))
}

/// Upstream renders the active sort / name-filter mode into the picker
/// title (`session-selector.ts:131-136`); the port has no separate header
/// row, so the modes ride along here.
fn session_picker_title(state: &SessionPickerState) -> String {
    format!(
        "Pick a session to resume · sort: {} · name: {}",
        state.sort.name(),
        state.filter.name()
    )
}

/// The `/resume` key hints, mirroring upstream's two hint lines
/// (`session-selector.ts:168-182`) with the live mode values folded in.
fn session_picker_footer(state: &SessionPickerState) -> Vec<String> {
    if let Some(pending) = &state.pending_delete {
        return vec![
            format!("  Delete session {pending}?"),
            "  enter confirm · esc cancel".to_string(),
        ];
    }
    let mut hints = vec![
        format!(
            "  ctrl+s sort ({}) · ctrl+n named ({}) · ctrl+p path ({}) · ctrl+r rename",
            state.sort.name(),
            state.filter.name(),
            if state.show_path { "on" } else { "off" }
        ),
        "  ctrl+d delete · ctrl+backspace delete · enter resume · esc cancel".to_string(),
    ];
    if state.filter == SessionFilter::NamedOnly {
        hints.push("  no sessions listed? ctrl+n shows every session".to_string());
    }
    hints
}

/// Rebuild the open session picker in place after a view change.
///
/// The filter text and the cursor survive the rebuild, so `Ctrl+S` does
/// not lose the user's place in a long list.
fn refresh_session_selector(app: &mut App, options: &mut InteractiveOptions) {
    let Some(dir) = session_directory(options) else {
        return;
    };
    let (cursor, filter) = app
        .selector()
        .map(|selector| (selector.cursor(), selector.filter().to_string()))
        .unwrap_or((0, String::new()));
    match build_session_selector(&dir, &options.pickers.session) {
        Ok(mut selector) => {
            if !filter.is_empty() {
                selector.set_filter(filter);
            }
            selector.set_cursor(cursor);
            app.replace_selector(selector);
        }
        Err(err) => app.info(format!("/resume: {err}")),
    }
}

/// Open the `/tree` overlay for the current session.
///
/// The tree is built from the stored `entry_id`/`parent_entry_id` links
/// (`pi_session::SessionReader::session_tree`) and flattened with
/// upstream `tree-selector.ts`'s pre-order / active-branch-first / gutter
/// semantics. Values are `tree:<entry_id>`.
fn open_tree_selector(app: &mut App, options: &InteractiveOptions) {
    let Some((_, reader)) = open_current_session(app, options, "/tree") else {
        return;
    };
    let active_leaf = options
        .session_leaf
        .clone()
        .or_else(|| session_tip(&reader, &options.session_id).ok().flatten());
    match tree_selector_with(
        &reader,
        &options.session_id,
        active_leaf.as_deref(),
        &options.pickers.tree,
    ) {
        Ok(selector) => app.open_selector(selector),
        Err(err) => app.info(format!("/tree: {err}")),
    }
}

/// Rebuild the open `/tree` overlay after a filter / fold / label change,
/// keeping the cursor and the search text.
fn refresh_tree_selector(app: &mut App, options: &InteractiveOptions) {
    let Some((_, reader)) = open_current_session(app, options, "/tree") else {
        return;
    };
    let active_leaf = options
        .session_leaf
        .clone()
        .or_else(|| session_tip(&reader, &options.session_id).ok().flatten());
    let (cursor, filter) = app
        .selector()
        .map(|selector| (selector.cursor(), selector.filter().to_string()))
        .unwrap_or((0, String::new()));
    match tree_selector_with(
        &reader,
        &options.session_id,
        active_leaf.as_deref(),
        &options.pickers.tree,
    ) {
        Ok(mut selector) => {
            if !filter.is_empty() {
                selector.set_filter(filter);
            }
            selector.set_cursor(cursor);
            app.replace_selector(selector);
        }
        Err(err) => app.info(format!("/tree: {err}")),
    }
}

/// Open the `/fork` user-message picker for the current session. Values
/// are `fork:<entry_id>`; an empty transcript reports the upstream
/// `No messages to fork from`.
fn open_fork_selector(app: &mut App, options: &InteractiveOptions) {
    let Some((_, reader)) = open_current_session(app, options, "/fork") else {
        return;
    };
    match fork_selector(&reader, &options.session_id) {
        Ok(selector) if selector.is_empty() => {
            app.info("No messages to fork from".to_string());
        }
        Ok(selector) => app.open_selector(selector),
        Err(err) => app.info(format!("/fork: {err}")),
    }
}

/// `/tree` commit: move the stored leaf onto the selected node so later
/// appends attach under it, then re-render the conversation up to that
/// node (upstream `navigateTree` + `renderInitialMessages`).
async fn handle_tree_selection(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    entry_id: &str,
) {
    let Some(database) = options.session_database.clone() else {
        app.info("/tree: no session database".to_string());
        return;
    };
    let session_id = options.session_id.clone();
    let writer = match SessionWriter::open(&database) {
        Ok(writer) => writer,
        Err(err) => {
            app.info(format!("/tree: {err}"));
            return;
        }
    };
    if let Err(err) = writer.set_leaf(&session_id, entry_id) {
        app.info(format!("/tree: {err}"));
        return;
    }
    let reader = match SessionReader::open(&database) {
        Ok(reader) => reader,
        Err(err) => {
            app.info(format!("/tree: {err}"));
            return;
        }
    };
    let path = match reader.entry_ancestry(&session_id, entry_id) {
        Ok(path) => path,
        Err(err) => {
            app.info(format!("/tree: {err}"));
            return;
        }
    };
    let entries = path
        .into_iter()
        .map(|decoded| decoded.entry)
        .collect::<Vec<_>>();
    let messages = entries_to_messages(&entries);
    app.messages_mut().clear();
    for entry in &entries {
        if let Some(item) = entry_to_item(entry) {
            app.messages_mut().push(item);
        }
    }
    agent.lock().await.state_mut().messages = messages;
    options.session_leaf = Some(entry_id.to_string());
    app.info("Navigated to selected point".to_string());
}

/// `/fork` commit: create a new session file holding the root-to-`entry_id`
/// path and continue there.
async fn handle_fork_selection(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    entry_id: &str,
) {
    let Some(directory) = session_directory(options) else {
        app.info("/fork: session directory not configured".to_string());
        return;
    };
    let Some((_, reader)) = open_current_session(app, options, "/fork") else {
        return;
    };
    match fork_session(&directory, &reader, &options.session_id, entry_id) {
        Ok(created) => {
            clone_into_new_session(app, agent, options, created).await;
        }
        Err(err) => app.info(format!("/fork: {err}")),
    }
}

/// Attach the TUI to a freshly created `/fork` or `/clone` session so the
/// user keeps going in the new branch (upstream replaces the runtime).
async fn clone_into_new_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    created: CreatedSession,
) {
    let reader = match SessionReader::open(&created.path) {
        Ok(reader) => reader,
        Err(err) => {
            app.info(format!("session {}: {err}", created.session_id));
            return;
        }
    };
    match attach_session(
        app,
        agent,
        options,
        created.path,
        &created.session_id,
        &reader,
    )
    .await
    {
        Ok(_) => app.info(format!(
            "Session branched into {} ({} entries)",
            created.session_id, created.entries
        )),
        Err(err) => app.info(format!("session {}: {err}", created.session_id)),
    }
}

/// Start a fresh session: open a new upstream-v4 session file, reset the
/// agent + view state and point the driver at the new identity.
///
/// This is what both `/new` and `app.session.new` run. It differs from
/// `/clear`, which only clears the message view and keeps the session.
async fn start_new_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
) {
    let Some(directory) = options
        .session_log
        .as_ref()
        .map(|log| log.directory().to_path_buf())
    else {
        app.info("/new: session directory not configured".to_string());
        return;
    };
    let id = new_session_id();
    // Open the JSONL log first so a failure leaves no half-created
    // session behind; the previous session's files are never touched.
    let log = match SessionLog::open(&directory, &id) {
        Ok(log) => log,
        Err(err) => {
            app.info(format!("/new: could not open session log: {err}"));
            return;
        }
    };
    let path = directory.join(format!("{id}.sqlite"));
    if let Err(err) = write_new_session_file(&path, &id) {
        app.info(format!("/new: could not create session file: {err}"));
        return;
    }

    app.messages_mut().clear();
    // A new session starts with an empty composer: the previous draft's text
    // and any image chips belong to the old session.
    app.clear_composer();
    agent.lock().await.state_mut().messages.clear();

    options.session_log = Some(log);
    options.session_id = id.clone();
    options.session_name = None;
    options.session_database = Some(path);
    app.set_session_id(id.clone());
    app.set_session_name(None);
    app.info(format!("Started new session {id}"));
}

/// Create an upstream-v4 session database and stamp the header, using
/// [`SessionWriter`] (never hand-rolled SQL).
fn write_new_session_file(path: &Path, session_id: &str) -> anyhow::Result<()> {
    let writer = SessionWriter::open(path)?;
    writer.write_header(SessionEntry::Header {
        id: session_id.to_string(),
        created_at: chrono::Utc::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })?;
    writer.checkpoint()?;
    Ok(())
}

/// Persist the display name to the session's files.
///
/// `/resume` reads the name back from the SQLite `sessions.metadata`, so
/// naming a session that has no database yet also materialises one
/// (header + name) — otherwise the name could not survive a resume.
fn persist_session_name(options: &mut InteractiveOptions, name: &str) -> anyhow::Result<()> {
    if options.session_database.is_none() {
        if let Some(directory) = options
            .session_log
            .as_ref()
            .map(|log| log.directory().to_path_buf())
        {
            let path = directory.join(format!("{}.sqlite", options.session_id));
            write_new_session_file(&path, &options.session_id)?;
            options.session_database = Some(path);
        }
    }
    if let Some(path) = options.session_database.clone() {
        let writer = SessionWriter::open(&path)?;
        writer.resume(&options.session_id)?;
        writer.set_session_name(name)?;
        writer.checkpoint()?;
    }
    // The interactive session also has a JSONL trail; record the name
    // there (same `session_name` kind the extension path uses) so the
    // legacy file is self-describing.
    if let Some(log) = options.session_log.as_ref() {
        let _ = log.append_extension("extension", "session_name", serde_json::json!(name));
    }
    Ok(())
}

/// Set `/name <text>`: normalize, persist, then reflect the name in the
/// status bar. Mirrors upstream `handleNameCommand`
/// (`interactive-mode.ts:6193`).
async fn set_session_name(app: &mut App, options: &mut InteractiveOptions, raw: &str) {
    let name = normalize_session_name(raw);
    if name.is_empty() {
        app.info("usage: /name <name>".to_string());
        return;
    }
    if options.session_database.is_none() && options.session_log.is_none() {
        app.info("/name: session persistence is disabled — the name was not saved".to_string());
        return;
    }
    match persist_session_name(options, &name) {
        Ok(()) => {
            if name != raw.trim() {
                app.info(format!(
                    "Session name was normalized from {raw:?} to {name:?}"
                ));
            }
            options.session_name = Some(name.clone());
            app.set_session_name(Some(name.clone()));
            // Upstream `session_info_changed`.
            deliver_extension_event(
                options.extensions.as_ref(),
                ExtensionEvent::SessionInfoChanged {
                    name: Some(name.clone()),
                },
            )
            .await;
            app.info(format!("Session name set: {name}"));
        }
        Err(err) => app.info(format!("/name: could not save the session name: {err}")),
    }
}

/// Collapse runs of CR/LF into a single space and trim, matching
/// upstream `appendSessionInfo` (`session-manager.ts:1151`).
fn normalize_session_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_newline_run = false;
    for ch in raw.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_newline_run {
                out.push(' ');
                in_newline_run = true;
            }
        } else {
            out.push(ch);
            in_newline_run = false;
        }
    }
    out.trim().to_string()
}

/// Flatten the text blocks of a content list into one string.
fn content_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a stored entry as a message-view item, skipping entries that
/// have no place in the transcript (headers, extension bookkeeping,
/// compaction checkpoints).
fn entry_to_item(entry: &SessionEntry) -> Option<pi_tui::message::MessageItem> {
    match entry {
        SessionEntry::UserMessage(message) => Some(pi_tui::message::MessageItem::user(
            content_text(&message.content),
        )),
        SessionEntry::AssistantMessage(message) => Some(pi_tui::message::MessageItem::assistant(
            content_text(&message.content),
        )),
        SessionEntry::ToolResult(result) => Some(pi_tui::message::MessageItem::tool(content_text(
            std::slice::from_ref(&*result.content),
        ))),
        _ => None,
    }
}

/// Rebuild the agent's model context from stored entries. A compaction
/// checkpoint resets the log to its summary plus the retained tail,
/// exactly as replaying the session would.
fn entries_to_messages(entries: &[SessionEntry]) -> Vec<Message> {
    let mut messages = Vec::new();
    for entry in entries {
        match entry {
            SessionEntry::UserMessage(message) => messages.push(message.clone()),
            SessionEntry::AssistantMessage(message) => messages.push(Message {
                role: pi_protocol::Role::Assistant,
                content: message.content.clone(),
                model: Some(message.model.clone()).filter(|model| !model.is_empty()),
            }),
            SessionEntry::ToolCall(call) => messages.push(Message {
                role: pi_protocol::Role::Assistant,
                content: vec![Content::ToolCall(call.clone())],
                model: None,
            }),
            SessionEntry::ToolResult(result) => messages.push(Message {
                role: pi_protocol::Role::Tool,
                content: vec![Content::ToolResult(result.clone())],
                model: None,
            }),
            SessionEntry::Compaction {
                summary,
                retained_tail,
                ..
            } => {
                messages.clear();
                messages.push(Message {
                    role: pi_protocol::Role::System,
                    content: vec![Content::text(summary.clone())],
                    model: None,
                });
                messages.extend(retained_tail.iter().cloned());
            }
            SessionEntry::Header { .. } | SessionEntry::Extension { .. } => {}
        }
    }
    messages
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
            // Command-reference output, not user input: the info prefix keeps
            // `/help`'s body from reading as something the user typed
            // (LUM-1238 §15.4).
            app.info_block(help);
        }
        SlashCommand::Clear => {
            app.messages_mut().clear();
        }
        SlashCommand::New => {
            start_new_session(app, agent, options).await;
        }
        SlashCommand::Copy => {
            copy_last_assistant_message(app);
        }
        SlashCommand::Name { name } => match name {
            Some(name) => set_session_name(app, options, &name).await,
            None => match options.session_name.as_deref() {
                Some(name) => app.info(format!("Session name: {name}")),
                None => app.info("usage: /name <name>".to_string()),
            },
        },
        SlashCommand::Exit => {
            app.request_exit();
        }
        SlashCommand::Model => {
            open_model_selector(app, options);
        }
        SlashCommand::Hotkeys => {
            app.info_block(crate::commands::slash::hotkeys_text());
        }
        SlashCommand::Extensions => {
            let home = crate::paths::home_dir();
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            app.info(crate::commands::extensions_text(
                &options.extension_report,
                home.as_deref(),
                &cwd,
            ));
        }
        SlashCommand::Session => {
            let agent_guard = agent.lock().await;
            let state = agent_guard.state();
            match options.session_name.as_deref() {
                Some(name) => app.info(format!(
                    "session {} — name: {name}, messages={}, model={}",
                    options.session_id,
                    state.messages.len(),
                    agent_guard.model().id
                )),
                None => app.info(format!(
                    "session {} — messages={}, model={}",
                    options.session_id,
                    state.messages.len(),
                    agent_guard.model().id
                )),
            }
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
            // filenames. Stage 65 moved the body into
            // `open_resume_selector` so `app.session.resume` runs the
            // exact same code instead of a second copy.
            open_resume_selector(app, options);
        }
        SlashCommand::Tree => {
            open_tree_selector(app, options);
        }
        SlashCommand::Fork => {
            open_fork_selector(app, options);
        }
        SlashCommand::Clone => {
            let Some(directory) = session_directory(options) else {
                app.info("/clone: session directory not configured".to_string());
                return Ok(());
            };
            let Some((_, reader)) = open_current_session(app, options, "/clone") else {
                return Ok(());
            };
            match clone_session(&directory, &reader, &options.session_id) {
                Ok(created) => {
                    clone_into_new_session(app, agent, options, created).await;
                }
                Err(err) => app.info(format!("/clone: {err}")),
            }
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
        SlashCommand::Thinking { level } => {
            let (supports, available) = {
                let agent_guard = agent.lock().await;
                let supports = crate::thinking::model_supports_thinking(agent_guard.model());
                (
                    supports,
                    crate::thinking::available_thinking_levels(supports),
                )
            };
            match level {
                Some(raw) => {
                    // Upstream matches the argument case-insensitively against
                    // the levels the *model* offers (`handleThinkingCommand`,
                    // `interactive-mode.ts:4789`), so an unsupported level is
                    // reported rather than applied.
                    let requested = crate::thinking::parse_thinking_level(&raw)
                        .filter(|candidate| available.contains(candidate));
                    match requested {
                        Some(level) => {
                            apply_thinking_level(
                                app,
                                agent,
                                level,
                                false,
                                options.extensions.as_ref(),
                            )
                            .await
                        }
                        None => app.info(format!(
                            "Unknown thinking level \"{raw}\". Available levels: {}.",
                            available
                                .iter()
                                .map(|level| level.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                    }
                }
                None => open_thinking_selector(app, supports),
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

    let report = apply_compaction(
        agent,
        options,
        history.len(),
        compaction,
        CompactReason::Manual,
    )
    .await;
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
    let compact_reason = if context_overflow {
        CompactReason::Overflow
    } else {
        CompactReason::Threshold
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

    let report = apply_compaction(agent, options, history.len(), compaction, compact_reason).await;
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
    reason: CompactReason,
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

    // Upstream `session_compact`, emitted from the one place that knows the
    // compaction actually landed; the manual and automatic paths share it.
    deliver_extension_event(
        options.extensions.as_ref(),
        ExtensionEvent::SessionCompact {
            reason,
            tokens_before,
            tokens_after,
            retained,
        },
    )
    .await;

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
/// How long extension `session_shutdown` handlers may run before the process
/// exits without them.
///
/// The host's interactive timeout is five minutes (a human may be reading a
/// dialog), which is the wrong ceiling for teardown: shutdown is not
/// interactive, so a plugin that blocks there is simply abandoned.
const EXTENSION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Subscribe to the agent's event fan-out and drive extension lifecycle
/// events.
///
/// Returns `None` when no runtime is attached or none of the loaded
/// extensions subscribed to an event — without subscribers the pump would
/// only cost a task and a channel.
async fn start_extension_event_pump(
    agent: &Arc<AsyncMutex<Agent>>,
    extensions: Option<&Arc<ExtensionRuntime>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let runtime = extensions?;
    if !runtime.has_any_subscriber() {
        return None;
    }
    let events = agent.lock().await.subscribe();
    let runtime = runtime.clone();
    Some(tokio::spawn(async move {
        run_extension_event_pump(runtime, events).await;
    }))
}

/// The pump body: translate every agent event and deliver the ones the
/// runtime subscribed to. Ends when the agent drops its fan-out senders.
async fn run_extension_event_pump(
    runtime: Arc<ExtensionRuntime>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) {
    let mut mapper = ExtensionEventMapper::new();
    while let Some(event) = events.recv().await {
        for extension_event in mapper.map(&event, now_millis()) {
            runtime.deliver_event(&extension_event).await;
        }
    }
}

/// Unix milliseconds — the timestamp upstream's `turn_start` carries.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Best-effort delivery of one lifecycle event from a call site outside the
/// agent fan-out (`user_bash`, `thinking_level_select`, …).
async fn deliver_extension_event(
    extensions: Option<&Arc<ExtensionRuntime>>,
    event: ExtensionEvent,
) {
    if let Some(runtime) = extensions {
        runtime.deliver_event(&event).await;
    }
}

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

/// Drain the crossterm events that are **already buffered**, and return as
/// soon as the queue is empty.
///
/// `crossterm::event::read` is a blocking call — it waits for the *next*
/// event when nothing is pending — so it must only be used after
/// `Event::poll(Duration::ZERO)` reported `true`. The previous version of the
/// interactive loop was `while let Some(event) = read_event()? { .. }`, where
/// `read_event` always returned `Ok(Some(_))` (the `Event` enum has no `None`
/// variant). The first ordinary key press therefore parked the render loop
/// inside a blocking `read()` forever: no more frames, no more agent events,
/// no `Esc` feedback (LUM-1233).
///
/// `poll` / `read` are injected so the contract — *never read an empty
/// queue* — is assertable without a real terminal.
fn drain_ready_events<P, R>(mut poll: P, mut read: R) -> anyhow::Result<Vec<CtEvent>>
where
    P: FnMut(Duration) -> io::Result<bool>,
    R: FnMut() -> io::Result<CtEvent>,
{
    let mut events = Vec::new();
    while poll(Duration::ZERO)? {
        events.push(read()?);
    }
    Ok(events)
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
    use pi_extensions::JsExtensionHost;

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
    // Startup header (Stage 66)
    // -----------------------------------------------------------------------

    #[test]
    fn the_launch_surface_shows_the_header_unless_the_startup_is_quiet() {
        let options = InteractiveOptions::default();
        let config = interactive_app_config(&options);
        assert!(config.startup_header, "interactive mode shows the header");
        assert!(config.startup_header_expanded, "and expands it on entry");

        let quiet = InteractiveOptions {
            quiet_startup: true,
            ..InteractiveOptions::default()
        };
        assert!(!interactive_app_config(&quiet).startup_header);
    }

    #[test]
    fn the_locale_comes_from_the_environment_and_defaults_to_english() {
        assert_eq!(locale_from_env(Some("en")), pi_tui::Locale::En);
        assert_eq!(locale_from_env(Some("zh-CN")), pi_tui::Locale::Zh);
        assert_eq!(locale_from_env(Some("fr")), pi_tui::Locale::En);
        assert_eq!(locale_from_env(None), pi_tui::Locale::En);
    }

    // -----------------------------------------------------------------------
    // Interactive input loop — LUM-1235 P0 regression
    // -----------------------------------------------------------------------

    #[test]
    fn draining_events_reads_every_buffered_event() {
        let queued = std::cell::Cell::new(3usize);
        let events = drain_ready_events(
            |_| Ok(queued.get() > 0),
            || {
                assert!(queued.get() > 0, "read() must not run on an empty queue");
                queued.set(queued.get() - 1);
                Ok(CtEvent::FocusGained)
            },
        )
        .expect("drain");
        assert_eq!(events.len(), 3);
        assert_eq!(queued.get(), 0);
    }

    #[test]
    fn draining_events_returns_immediately_when_nothing_is_buffered() {
        let mut reads = 0usize;
        let events = drain_ready_events(
            |_| Ok(false),
            || {
                reads += 1;
                Ok(CtEvent::FocusLost)
            },
        )
        .expect("drain");
        assert!(events.is_empty());
        assert_eq!(reads, 0, "an empty queue must never be read");
    }

    #[test]
    fn draining_events_propagates_a_poll_error() {
        let err = drain_ready_events(
            |_| Err(io::Error::other("boom")),
            || Ok(CtEvent::FocusGained),
        )
        .expect_err("poll error");
        assert!(err.to_string().contains("boom"), "{err}");
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
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
        });
        app.messages_mut().push(pi_tui::message::MessageItem {
            role: pi_tui::message::Role::Assistant,
            text: "second reply\nwith two lines".into(),
            thinking: String::new(),
            streaming: false,
            tool_header: None,
            tool_lines: None,
            tool_expanded: None,
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

    // -----------------------------------------------------------------------
    // `app.model.select` (`Ctrl+L`) — LUM-1240 P0-1
    // -----------------------------------------------------------------------

    /// `Ctrl+L` is `app.model.select` upstream
    /// (`packages/coding-agent/src/core/keybindings.ts:116`, "Open model
    /// selector"). The App used to hardcode the chord as "clear the
    /// transcript", which shadowed the advertised action; the driver owns the
    /// chord now and opens the same picker `/model` does.
    #[tokio::test]
    async fn ctrl_l_opens_the_model_selector() {
        let (mut app, agent) = app_starting_at(catalog_model("alpha", "a")).await;
        let mut options = InteractiveOptions {
            models: two_model_catalog(),
            ..InteractiveOptions::default()
        };
        let mut bash = BashRunner::default();
        app.messages_mut()
            .push(pi_tui::message::MessageItem::user("seed"));

        let event = InputEvent::Key(Key::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        handle_input_event(&mut app, &agent, &mut options, &mut bash, event)
            .await
            .expect("Ctrl+L");

        assert!(app.selector_open(), "Ctrl+L must open the selector");
        let selector = app.selector().expect("selector");
        assert_eq!(selector.title(), "Pick a model");
        assert!(selector.is_searchable(), "the model picker is searchable");
        assert_eq!(
            selector
                .items()
                .iter()
                .map(|item| item.value.as_str())
                .collect::<Vec<_>>(),
            vec!["model:a", "model:b"],
            "sorted catalog order"
        );
        // P0-1: it is *not* a clear chord any more.
        assert!(!app.messages().is_empty(), "Ctrl+L must not clear the log");
    }

    /// Committing from the `Ctrl+L` picker switches the agent's model — the
    /// path is shared with `/model`, so this pins that sharing.
    #[tokio::test]
    async fn committing_the_ctrl_l_picker_switches_the_model() {
        let (mut app, agent) = app_starting_at(catalog_model("alpha", "a")).await;
        let mut options = InteractiveOptions {
            models: two_model_catalog(),
            ..InteractiveOptions::default()
        };
        let mut bash = BashRunner::default();

        let ctrl_l = InputEvent::Key(Key::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        handle_input_event(&mut app, &agent, &mut options, &mut bash, ctrl_l)
            .await
            .expect("Ctrl+L");
        assert!(app.selector_open());

        // Move to `beta/b` and commit.
        handle_input_event(
            &mut app,
            &agent,
            &mut options,
            &mut bash,
            key(KeyCode::Down),
        )
        .await
        .expect("Down");
        handle_input_event(
            &mut app,
            &agent,
            &mut options,
            &mut bash,
            key(KeyCode::Enter),
        )
        .await
        .expect("Enter");

        assert!(!app.selector_open(), "committing closes the picker");
        assert_eq!(agent.lock().await.model().id, "b");
    }

    /// The chord is claimed only while no overlay owns the keyboard — the
    /// same guard every `app.*` driver intercept uses. With the search
    /// overlay open `Ctrl+L` stays inert instead of opening a second modal.
    #[tokio::test]
    async fn ctrl_l_is_inert_while_an_overlay_owns_the_keyboard() {
        let (mut app, agent) = app_starting_at(catalog_model("alpha", "a")).await;
        let mut options = InteractiveOptions {
            models: two_model_catalog(),
            ..InteractiveOptions::default()
        };
        let mut bash = BashRunner::default();
        app.open_search();
        assert!(app.search_open());

        let event = InputEvent::Key(Key::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        handle_input_event(&mut app, &agent, &mut options, &mut bash, event)
            .await
            .expect("Ctrl+L");

        assert!(!app.selector_open(), "the overlay keeps the keyboard");
        assert!(app.search_open(), "the overlay is still open");
    }

    // -----------------------------------------------------------------------
    // Queued input while streaming (LUM-1216)
    // -----------------------------------------------------------------------

    fn alt_enter() -> InputEvent {
        InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::ALT))
    }

    fn alt_up() -> InputEvent {
        InputEvent::Key(Key::new(KeyCode::Up, KeyModifiers::ALT))
    }

    #[tokio::test]
    async fn follow_up_queues_while_busy_and_dequeue_restores_it() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        // A turn is in flight; `submit` marks the App busy synchronously and
        // this single-threaded runtime does not run the spawned task yet.
        app.submit(agent.clone(), "first".to_string());
        assert!(app.is_busy());

        app.set_editor_text("queued follow-up");
        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_enter())
            .await
            .expect("follow-up chord");
        assert_eq!(app.pending_len(), 1);
        assert_eq!(app.editor_text(), "", "the queue took the buffer");

        // Dequeue pulls it back into the editor and reports upstream's tally.
        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_up())
            .await
            .expect("dequeue chord");
        assert_eq!(app.pending_len(), 0);
        assert_eq!(app.editor_text(), "queued follow-up");
        assert_eq!(
            app.status_flash(),
            Some("Restored 1 queued message to editor")
        );
    }

    #[tokio::test]
    async fn dequeue_status_matches_upstream_for_every_count() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        app.submit(agent.clone(), "first".to_string());
        app.submit(agent.clone(), "second".to_string());
        app.submit(agent.clone(), "third".to_string());
        assert_eq!(app.pending_len(), 2);

        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_up())
            .await
            .expect("dequeue");
        assert_eq!(
            app.status_flash(),
            Some("Restored 2 queued messages to editor")
        );

        // Nothing left to restore: the editor is left alone.
        app.set_editor_text("");
        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_up())
            .await
            .expect("dequeue empty");
        assert_eq!(app.status_flash(), Some("No queued messages to restore"));
        assert_eq!(app.editor_text(), "");
    }

    #[tokio::test]
    async fn queued_chords_are_not_stolen_while_an_overlay_is_open() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        app.submit(agent.clone(), "first".to_string());
        app.set_editor_text("queued");
        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_enter())
            .await
            .expect("follow-up");
        assert_eq!(app.pending_len(), 1);

        // With the search overlay focused the chords belong to the overlay:
        // alt+enter must not queue a second message and alt+up must not
        // dequeue the first.
        assert!(app.open_search());
        app.set_editor_text("must not queue");
        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_enter())
            .await
            .expect("overlay alt+enter");
        assert_eq!(app.pending_len(), 1, "overlay owns app.message.followUp");

        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt_up())
            .await
            .expect("overlay alt+up");
        assert_eq!(app.pending_len(), 1, "overlay owns app.message.dequeue");
        assert_eq!(app.status_flash(), None, "dequeue did not run");
    }

    #[tokio::test]
    async fn queued_prompts_are_delivered_in_order_after_the_turn_ends() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        app.submit(agent.clone(), "first".to_string());
        assert!(app.is_busy());
        // Type the next three prompts while the turn streams; each Enter goes
        // through the driver and is queued instead of being dropped.
        for text in ["second", "third", "fourth"] {
            for ch in text.chars() {
                app.step(InputEvent::Key(Key::new(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE,
                )));
            }
            handle_input_event(
                &mut app,
                &agent,
                &mut options,
                &mut bash,
                InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            )
            .await
            .expect("enter while busy");
        }
        assert_eq!(app.pending_len(), 3);

        // Let the in-flight turn finish, then drain the queue one message at
        // a time, the way the render loop does.
        drain_until_idle(&mut app).await;
        for _ in 0..3 {
            deliver_pending(&mut app, &agent, &mut options, &mut bash)
                .await
                .expect("deliver pending");
            drain_until_idle(&mut app).await;
        }
        assert_eq!(app.pending_len(), 0, "the queue drained completely");

        let user_texts: Vec<String> = app
            .messages()
            .items()
            .iter()
            .filter(|item| item.role == pi_tui::message::Role::User)
            .map(|item| item.text.clone())
            .collect();
        assert_eq!(user_texts, vec!["first", "second", "third", "fourth"]);
    }

    // -----------------------------------------------------------------------
    // `/new`, `/copy`, `/name` + `app.session.new` (Stage 60)
    // -----------------------------------------------------------------------

    fn session_options(dir: &std::path::Path, session_id: &str) -> InteractiveOptions {
        InteractiveOptions {
            session_log: Some(SessionLog::open(dir, session_id).expect("session log")),
            session_id: session_id.to_string(),
            ..InteractiveOptions::default()
        }
    }

    #[tokio::test]
    async fn slash_copy_uses_the_clipboard_channel() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();

        // Empty transcript: a friendly message, never a silent drop.
        run_slash_command(&mut app, &agent, &mut options, "/copy")
            .await
            .expect("copy");
        assert!(app.take_clipboard_request().is_none());
        assert!(transcript(&app).contains("nothing to copy yet"));

        app.messages_mut()
            .push(pi_tui::message::MessageItem::assistant("hello there"));
        run_slash_command(&mut app, &agent, &mut options, "/copy")
            .await
            .expect("copy");
        assert_eq!(app.take_clipboard_request().as_deref(), Some("hello there"));
    }

    #[tokio::test]
    async fn slash_name_sets_reports_and_persists_the_session_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = session_options(dir.path(), "named-session");

        run_slash_command(&mut app, &agent, &mut options, "/name foo")
            .await
            .expect("name");
        assert_eq!(options.session_name.as_deref(), Some("foo"));
        assert_eq!(app.status_data().session_name.as_deref(), Some("foo"));
        assert!(transcript(&app).contains("Session name set: foo"));

        // No argument reports the current name.
        run_slash_command(&mut app, &agent, &mut options, "/name")
            .await
            .expect("name");
        assert!(transcript(&app).contains("Session name: foo"));

        // `/session` surfaces it too.
        run_slash_command(&mut app, &agent, &mut options, "/session")
            .await
            .expect("session");
        assert!(transcript(&app).contains("name: foo"));

        // Persisted to the session file and readable through `pi-session`.
        let database = options.session_database.clone().expect("session database");
        let reader = SessionReader::open(&database).expect("reader");
        assert_eq!(
            reader
                .session_name("named-session")
                .expect("read name")
                .as_deref(),
            Some("foo")
        );
    }

    #[test]
    fn session_names_collapse_newline_runs_like_upstream() {
        assert_eq!(normalize_session_name("  hello  "), "hello");
        assert_eq!(normalize_session_name("a\r\nb"), "a b");
        assert_eq!(normalize_session_name("a\r\r\n\nb"), "a b");
        assert_eq!(normalize_session_name("\n\n"), "");
    }

    #[tokio::test]
    async fn new_session_clears_state_and_keeps_the_old_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = session_options(dir.path(), "old-session");
        let old_path = dir.path().join("old-session.sqlite");
        write_new_session_file(&old_path, "old-session").expect("old session file");

        app.messages_mut()
            .push(pi_tui::message::MessageItem::user("hello"));
        agent
            .lock()
            .await
            .state_mut()
            .messages
            .push(text_message(Role::User, "hello".into()));

        run_slash_command(&mut app, &agent, &mut options, "/new")
            .await
            .expect("new");

        assert_ne!(options.session_id, "old-session");
        assert!(agent.lock().await.state().messages.is_empty());
        assert_eq!(app.status_data().session_id, options.session_id);
        let rendered = transcript(&app);
        assert!(!rendered.contains("hello"), "{rendered}");
        assert!(rendered.contains("Started new session"), "{rendered}");

        // The new file is a valid session and the old one is untouched.
        let new_database = options.session_database.clone().expect("session database");
        let new_reader = SessionReader::open(&new_database).expect("new reader");
        assert!(new_reader
            .session_row(&options.session_id)
            .expect("row")
            .is_some());
        let old_reader = SessionReader::open(&old_path).expect("old reader");
        assert!(old_reader
            .session_row("old-session")
            .expect("row")
            .is_some());

        // `pi session list` / `/resume` see both.
        let ids = crate::list_resumable(dir.path())
            .expect("list")
            .into_iter()
            .map(|r| r.session_id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&"old-session".to_string()), "{ids:?}");
        assert!(ids.contains(&options.session_id), "{ids:?}");
    }

    #[tokio::test]
    async fn app_session_new_keybinding_runs_the_new_session_action() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = session_options(dir.path(), "kb-old");
        let mut bash = BashRunner::default();

        let event = InputEvent::Key(pi_tui::input::Key::new(
            KeyCode::Char('n'),
            pi_tui::input::KeyModifiers {
                alt: true,
                ..Default::default()
            },
        ));
        handle_input_event(&mut app, &agent, &mut options, &mut bash, event)
            .await
            .expect("handle event");

        assert_ne!(options.session_id, "kb-old");
        assert!(dir
            .path()
            .join(format!("{}.sqlite", options.session_id))
            .is_file());
    }

    // -----------------------------------------------------------------------
    // `/tree`, `/fork`, `/clone` + the session keybindings (Stage 65)
    // -----------------------------------------------------------------------

    fn session_user(text: &str) -> SessionEntry {
        SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::text(text)],
            model: None,
        })
    }

    fn session_assistant(text: &str) -> SessionEntry {
        SessionEntry::AssistantMessage(pi_protocol::AssistantMessage {
            model: "test-model".into(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
            error_message: None,
        })
    }

    /// A two-branch upstream-v4 session (`e1..e6`), built with the same
    /// `SessionWriter` the commands use:
    ///
    /// ```text
    /// e1(u1) ─ e2(a1) ─ e3(u2) ─ e4(a2)
    ///                  └─ e5(u2b) ─ e6(a2b)
    /// ```
    fn build_branch_session(dir: &Path, session_id: &str) -> PathBuf {
        let path = dir.join(format!("{session_id}.sqlite"));
        let writer = SessionWriter::open(&path).expect("open writer");
        writer
            .write_header(SessionEntry::Header {
                id: session_id.to_string(),
                created_at: chrono::Utc::now(),
                version: "0.1.0".into(),
            })
            .expect("header");
        for entry in [
            session_user("u1"),
            session_assistant("a1"),
            session_user("u2"),
            session_assistant("a2"),
        ] {
            writer.append(entry).expect("append");
        }
        // Branch B re-roots at the first assistant reply.
        writer.set_leaf(session_id, "e2").expect("set leaf");
        writer.append(session_user("u2b")).expect("append");
        writer.append(session_assistant("a2b")).expect("append");
        writer.checkpoint().expect("checkpoint");
        drop(writer);
        path
    }

    /// Driver options attached to a session file already on disk.
    fn stored_options(dir: &Path, session_id: &str) -> InteractiveOptions {
        let mut options = session_options(dir, session_id);
        options.session_database = Some(dir.join(format!("{session_id}.sqlite")));
        options
    }

    /// `(entry_id, parent_entry_id, seq)` per entry — enough to prove a
    /// copy is entry-for-entry identical without a raw SQL dependency.
    fn entry_signature(
        reader: &SessionReader,
        session_id: &str,
    ) -> Vec<(Option<String>, Option<String>, i64)> {
        reader
            .iter_entries(session_id)
            .expect("entries")
            .into_iter()
            .map(|entry| (entry.entry_id, entry.parent_entry_id, entry.seq))
            .collect()
    }

    #[tokio::test]
    async fn slash_clone_copies_every_entry_into_a_new_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let source_path = build_branch_session(dir.path(), "clone-source");
        let mut options = stored_options(dir.path(), "clone-source");
        let before = std::fs::read(&source_path).expect("source bytes");

        run_slash_command(&mut app, &agent, &mut options, "/clone")
            .await
            .expect("clone");

        assert_ne!(options.session_id, "clone-source");
        let new_path = options.session_database.clone().expect("new database");
        assert_eq!(
            new_path,
            dir.path().join(format!("{}.sqlite", options.session_id))
        );
        assert!(new_path.is_file());
        let source = SessionReader::open(&source_path).expect("source reader");
        let cloned = SessionReader::open(&new_path).expect("clone reader");
        assert_eq!(
            entry_signature(&cloned, &options.session_id),
            entry_signature(&source, "clone-source"),
            "the clone is entry-for-entry identical"
        );
        assert!(
            cloned
                .verify_stats(&options.session_id)
                .expect("verify stats")
                .expect("stats row")
                .is_consistent(),
            "the clone's cached stats agree with a recompute"
        );
        assert_eq!(
            std::fs::read(&source_path).expect("source bytes"),
            before,
            "the source file must not change"
        );
        assert!(transcript(&app).contains("Session branched into"));
        assert!(transcript(&app).contains("6 entries"));
    }

    #[tokio::test]
    async fn slash_fork_stops_at_the_selected_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let source_path = build_branch_session(dir.path(), "fork-source");
        let mut options = stored_options(dir.path(), "fork-source");

        run_slash_command(&mut app, &agent, &mut options, "/fork")
            .await
            .expect("fork");
        let selector = app.selector().cloned().expect("fork selector open");
        let values = selector
            .items()
            .iter()
            .map(|item| item.value.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec!["fork:e1", "fork:e3", "fork:e5"],
            "only non-empty user messages are offered, in entry order"
        );
        let labels = selector
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["u1", "u2", "u2b"]);
        app.close_selector();

        apply_selector_choice(&mut app, &agent, "fork:e3", &mut options).await;

        assert_ne!(options.session_id, "fork-source");
        let new_path = options.session_database.clone().expect("new database");
        let reader = SessionReader::open(&new_path).expect("reader");
        assert!(
            reader
                .session_row(&options.session_id)
                .expect("row")
                .is_some(),
            "the new session has its header row first"
        );
        let ids = reader
            .iter_entries(&options.session_id)
            .expect("entries")
            .into_iter()
            .map(|entry| entry.entry_id.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["e1", "e2", "e3"],
            "the fork stops at the selected user message"
        );
        assert!(
            reader
                .verify_stats(&options.session_id)
                .expect("verify stats")
                .expect("stats row")
                .is_consistent(),
            "the fork's cached stats agree with a recompute"
        );
        // The source keeps all six entries.
        let source = SessionReader::open(&source_path).expect("source reader");
        assert_eq!(entry_signature(&source, "fork-source").len(), 6);
    }

    #[tokio::test]
    async fn slash_fork_on_an_empty_transcript_creates_no_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let path = dir.path().join("empty.sqlite");
        write_new_session_file(&path, "empty").expect("session file");
        let mut options = stored_options(dir.path(), "empty");
        let sqlite_files = |dir: &Path| {
            std::fs::read_dir(dir)
                .expect("dir")
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sqlite"))
                .count()
        };
        let before = sqlite_files(dir.path());

        run_slash_command(&mut app, &agent, &mut options, "/fork")
            .await
            .expect("fork");

        assert!(
            app.selector().is_none(),
            "no picker for an empty transcript"
        );
        assert!(transcript(&app).contains("No messages to fork from"));
        assert_eq!(
            sqlite_files(dir.path()),
            before,
            "no session file is created"
        );
        assert_eq!(options.session_id, "empty");
    }

    #[tokio::test]
    async fn slash_tree_switches_the_leaf_and_the_next_append_hangs_below_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let path = build_branch_session(dir.path(), "tree-source");
        let mut options = stored_options(dir.path(), "tree-source");
        let before = std::fs::read(&path).expect("source bytes");

        run_slash_command(&mut app, &agent, &mut options, "/tree")
            .await
            .expect("tree");
        // Opening the overlay is read-only; Esc would leave it that way.
        assert_eq!(std::fs::read(&path).expect("source bytes"), before);
        let selector = app.selector().cloned().expect("tree selector open");
        let values = selector
            .items()
            .iter()
            .map(|item| item.value.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec!["tree:e1", "tree:e2", "tree:e5", "tree:e6", "tree:e3", "tree:e4"],
            "pre-order, with the active branch emitted first"
        );
        app.close_selector();

        apply_selector_choice(&mut app, &agent, "tree:e2", &mut options).await;

        assert_eq!(options.session_leaf.as_deref(), Some("e2"));
        assert!(transcript(&app).contains("Navigated to selected point"));
        // The view is reloaded to the selected path (e1 `u1`, e2 `a1`).
        let rendered = transcript(&app);
        assert!(rendered.contains("u1"), "{rendered}");
        assert!(rendered.contains("a1"), "{rendered}");
        assert!(!rendered.contains("u2"), "{rendered}");

        // A later append (a fresh writer, as after a restart) attaches to
        // the selected node.
        let writer = SessionWriter::open(&path).expect("writer");
        writer.resume("tree-source").expect("resume");
        writer.append(session_user("after-tree")).expect("append");
        writer.checkpoint().expect("checkpoint");
        drop(writer);
        let reader = SessionReader::open(&path).expect("reader");
        let appended = reader
            .iter_entries("tree-source")
            .expect("entries")
            .into_iter()
            .find(|entry| entry.seq == 7)
            .expect("appended entry");
        assert_eq!(
            appended.parent_entry_id.as_deref(),
            Some("e2"),
            "the append hangs off the tree-selected leaf"
        );
    }

    #[tokio::test]
    async fn session_tree_fork_and_resume_keybindings_open_their_selectors() {
        let dir = tempfile::tempdir().expect("tempdir");
        build_branch_session(dir.path(), "kb-session");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = stored_options(dir.path(), "kb-session");
        let mut bash = BashRunner::default();

        let alt = |ch: char| {
            InputEvent::Key(pi_tui::input::Key::new(
                KeyCode::Char(ch),
                pi_tui::input::KeyModifiers {
                    alt: true,
                    ..Default::default()
                },
            ))
        };

        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt('t'))
            .await
            .expect("tree key");
        assert_eq!(app.selector().map(Selector::title), Some("Session tree"));
        app.close_selector();

        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt('f'))
            .await
            .expect("fork key");
        assert_eq!(
            app.selector().map(Selector::title),
            Some("Fork from a user message")
        );
        app.close_selector();

        handle_input_event(&mut app, &agent, &mut options, &mut bash, alt('r'))
            .await
            .expect("resume key");
        let values = app
            .selector()
            .expect("resume selector")
            .items()
            .iter()
            .map(|item| item.value.clone())
            .collect::<Vec<_>>();
        assert!(
            values.contains(&"resume:kb-session".to_string()),
            "the resume key shares the `/resume` picker: {values:?}"
        );
    }

    #[tokio::test]
    async fn session_name_survives_resume() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;

        let mut first = session_options(dir.path(), "session-a");
        run_slash_command(&mut app, &agent, &mut first, "/name foo")
            .await
            .expect("name");
        assert_eq!(first.session_name.as_deref(), Some("foo"));

        // A different session, then resume `session-a` from the selector.
        let mut second = session_options(dir.path(), "session-b");
        apply_selector_choice(&mut app, &agent, "resume:session-a", &mut second).await;

        assert_eq!(second.session_id, "session-a");
        assert_eq!(second.session_name.as_deref(), Some("foo"));
        assert_eq!(app.status_data().session_name.as_deref(), Some("foo"));
        assert!(transcript(&app).contains("Resumed session session-a (foo)"));
    }

    // -----------------------------------------------------------------------
    // Local `!` / `!!` bash commands (LUM-1223)
    // -----------------------------------------------------------------------

    fn key(code: KeyCode) -> InputEvent {
        InputEvent::Key(Key::new(code, KeyModifiers::NONE))
    }

    /// Type `text` into the editor and press Enter, the way the render loop
    /// does with real key events.
    async fn submit_line(
        app: &mut App,
        agent: &Arc<AsyncMutex<Agent>>,
        options: &mut InteractiveOptions,
        bash: &mut BashRunner,
        text: &str,
    ) {
        for ch in text.chars() {
            app.step(key(KeyCode::Char(ch)));
        }
        handle_input_event(app, agent, options, bash, key(KeyCode::Enter))
            .await
            .expect("submit");
    }

    /// The tool block the transcript shows for a local command.
    fn local_bash_block(app: &App) -> pi_tui::message::MessageItem {
        let items = app.messages().items();
        let blocks: Vec<_> = items
            .iter()
            .filter(|item| item.role == pi_tui::message::Role::Tool)
            .cloned()
            .collect();
        assert_eq!(blocks.len(), 1, "expected exactly one tool block");
        blocks.into_iter().next().expect("checked above")
    }

    #[tokio::test]
    async fn bang_echo_renders_a_tool_block_without_a_user_message() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        submit_line(&mut app, &agent, &mut options, &mut bash, "!echo hi").await;
        assert!(bash.is_running(), "the command runs off the render loop");
        bash.wait(&mut app, 80).await;
        assert!(!bash.is_running(), "the block consumed the run");

        let block = local_bash_block(&app);
        assert!(
            block.text.contains("[tool:bash] echo hi"),
            "header: {}",
            block.text
        );
        assert!(block.text.contains("hi"), "output: {}", block.text);
        assert!(
            block.tool_header.is_some(),
            "the Stage 58 renderer owns the block"
        );
        // Local: nothing user-authored is rendered and no agent turn started.
        assert!(
            !app.messages()
                .items()
                .iter()
                .any(|item| item.role == pi_tui::message::Role::User),
            "a local command must not render a user message"
        );
        assert!(!app.is_busy());
        assert_eq!(agent.lock().await.state().messages.len(), 0);
    }

    #[tokio::test]
    async fn double_bang_output_is_visible_but_never_enters_the_agent_log() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        // Seed the log so "unchanged" is a meaningful comparison.
        agent
            .lock()
            .await
            .state_mut()
            .messages
            .push(text_message(Role::User, "seed".into()));
        let before = agent.lock().await.state().messages.clone();

        submit_line(&mut app, &agent, &mut options, &mut bash, "!!echo hi").await;
        bash.wait(&mut app, 80).await;

        let block = local_bash_block(&app);
        assert!(block.text.contains("hi"), "output: {}", block.text);

        let after = agent.lock().await.state().messages.clone();
        assert_eq!(
            after.len(),
            before.len(),
            "excluded command touched the log"
        );
        assert_eq!(after[0].content, before[0].content);
    }

    #[tokio::test]
    async fn a_busy_app_refuses_bash_and_restores_the_editor() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        // A turn is in flight: the driver must refuse the local command
        // rather than queue it behind the turn (that is Stage 61's pending
        // queue, a different path).
        app.submit(agent.clone(), "first".to_string());
        assert!(app.is_busy());

        submit_line(&mut app, &agent, &mut options, &mut bash, "!sleep 5").await;

        assert_eq!(
            app.editor_text(),
            "!sleep 5",
            "text goes back to the editor"
        );
        assert_eq!(
            app.status_flash(),
            Some("A bash command is already running. Press Esc to cancel it first.")
        );
        assert_eq!(
            app.pending_len(),
            0,
            "bash must not use the follow-up queue"
        );
        assert!(!bash.is_running(), "nothing was started");
    }

    #[tokio::test]
    async fn empty_bang_commands_fall_back_to_the_normal_prompt() {
        for line in ["!", "!!", "!   "] {
            let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
            let mut options = InteractiveOptions::default();
            let mut bash = BashRunner::default();

            submit_line(&mut app, &agent, &mut options, &mut bash, line).await;

            assert!(!bash.is_running(), "{line:?} must not start bash");
            assert!(app.is_busy(), "{line:?} must be a plain prompt");
            assert!(
                app.messages()
                    .items()
                    .iter()
                    .any(|item| item.role == pi_tui::message::Role::User && item.text == line),
                "{line:?} must render as a user message"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn esc_cancels_a_running_bash_command() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        submit_line(&mut app, &agent, &mut options, &mut bash, "!sleep 30").await;
        assert!(bash.is_running());

        handle_input_event(&mut app, &agent, &mut options, &mut bash, key(KeyCode::Esc))
            .await
            .expect("esc");
        assert_eq!(app.status_flash(), Some("Cancelling bash command…"));

        bash.wait(&mut app, 80).await;
        let block = local_bash_block(&app);
        assert!(
            block.text.contains("command cancelled"),
            "output: {}",
            block.text
        );
    }

    // -----------------------------------------------------------------------
    // Composer autocomplete wiring (LUM-1236)
    // -----------------------------------------------------------------------

    /// `run_loop` must install the provider on the live composer. LUM-1236
    /// was a *missing call*, so this drives [`install_composer_autocomplete`]
    /// and then asserts the App paints candidates: the whole path from
    /// install, through a keystroke, to the screen.
    #[test]
    fn the_composer_completes_slash_commands_from_a_keystroke() {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let mut app = App::new(&agent, AppConfig::default());
        install_composer_autocomplete(&mut app, std::env::temp_dir(), Vec::new());

        for ch in ['/', 'c', 'o', 'm'] {
            app.step(key(KeyCode::Char(ch)));
        }

        let snapshot = app.render_snapshot(72, 14);
        let rendered = snapshot.lines.join("\n");
        let selected = snapshot
            .lines
            .iter()
            .find(|line| line.contains('❯'))
            .unwrap_or_else(|| panic!("no dropdown row in:\n{rendered}"));
        // Whatever the fuzzy ranker puts first, the dropdown is live and
        // the `/com` prefix keeps its own matching candidates on screen.
        assert!(
            selected.contains("compact") || selected.contains("copy"),
            "{rendered}"
        );
        assert!(rendered.contains("compact"), "{rendered}");
        // The prompt keeps the typed prefix while the list is up.
        assert!(rendered.contains("/com"), "{rendered}");
    }

    /// The dropdown must not survive `Esc`: the editor closes it and the
    /// next frame has to clear the rows it borrowed from the transcript.
    #[test]
    fn escaping_the_dropdown_repaints_the_transcript_rows() {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let mut app = App::new(&agent, AppConfig::default());
        app.messages_mut()
            .push(pi_tui::message::MessageItem::assistant("existing output"));
        install_composer_autocomplete(&mut app, std::env::temp_dir(), Vec::new());

        for ch in ['/', 'm'] {
            app.step(key(KeyCode::Char(ch)));
        }
        assert!(app
            .render_snapshot(72, 14)
            .lines
            .iter()
            .any(|l| l.contains('❯')));

        app.step(key(KeyCode::Esc));
        let rendered = app.render_snapshot(72, 14).lines.join("\n");
        assert!(!rendered.contains('❯'), "{rendered}");
        assert!(rendered.contains("existing output"), "{rendered}");
        // `Esc` closed the dropdown without rewriting the input.
        assert!(rendered.contains("/m"), "{rendered}");
    }

    /// Stage 70 (LUM-1238) folded the extension-registered commands into the
    /// same dropdown: `pi.registerCommand("ext-echo")` has to complete like a
    /// built-in, or the user cannot discover it.
    #[test]
    fn extension_commands_complete_through_the_same_dropdown() {
        let agent = Agent::new(AgentOptions::new(
            small_window_model(1_000_000),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        let mut app = App::new(&agent, AppConfig::default());
        let extra = vec![pi_tui::autocomplete::SlashCommand::new("ext-echo")
            .with_description("echo through an extension")];
        install_composer_autocomplete(&mut app, std::env::temp_dir(), extra);

        for ch in ['/', 'e', 'x', 't'] {
            app.step(key(KeyCode::Char(ch)));
        }

        let rendered = app.render_snapshot(72, 14).lines.join("\n");
        assert!(rendered.contains("ext-echo"), "{rendered}");
    }

    // -----------------------------------------------------------------------
    // `app.clipboard.pasteImage` (Stage 63 / LUM-1224)
    // -----------------------------------------------------------------------

    /// The production reader shells out to `wl-paste` / `xclip` / `pngpaste` /
    /// PowerShell, none of which are reachable from the test sandbox; the
    /// trait exists so the driver can swap in a stub.
    struct FakeClipboard(crate::clipboard::ClipboardPaste);

    impl crate::clipboard::ClipboardReader for FakeClipboard {
        fn read(&self) -> crate::clipboard::ClipboardPaste {
            self.0.clone()
        }
    }

    fn clipboard_image() -> pi_protocol::ImageContent {
        pi_protocol::ImageContent {
            mime_type: "image/png".into(),
            data: "QUJD".into(),
        }
    }

    #[tokio::test]
    async fn image_paste_attaches_a_chip_and_non_images_fall_back_to_text() {
        let (mut app, _agent) = app_starting_at(small_window_model(1_000_000)).await;

        // Clipboard holds an image: one chip, no raw bytes in the buffer.
        apply_clipboard_paste(
            &mut app,
            crate::clipboard::ClipboardPaste::Image(clipboard_image()),
        );
        assert_eq!(app.image_count(), 1);
        assert_eq!(app.editor_text(), "[Image #1]");

        // Clipboard holds text: pasted at the cursor like a normal paste.
        apply_clipboard_paste(
            &mut app,
            crate::clipboard::ClipboardPaste::Text(" hello".into()),
        );
        assert_eq!(app.editor_text(), "[Image #1] hello");
        assert_eq!(app.image_count(), 1);

        // Clipboard holds nothing usable: the draft is left untouched.
        apply_clipboard_paste(&mut app, crate::clipboard::ClipboardPaste::Empty);
        assert_eq!(app.editor_text(), "[Image #1] hello");
    }

    #[tokio::test]
    async fn alt_v_records_the_read_request_for_the_driver() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();
        let mut bash = BashRunner::default();

        let event = InputEvent::Key(Key::new(
            KeyCode::Char('v'),
            pi_tui::input::KeyModifiers {
                alt: true,
                ..Default::default()
            },
        ));
        handle_input_event(&mut app, &agent, &mut options, &mut bash, event)
            .await
            .expect("alt+v");

        // The App only records the chord; the render loop owns the blocking
        // clipboard read, so the flag must still be pending here.
        assert!(app.take_image_paste_request());
        assert!(!app.take_image_paste_request());
    }

    #[tokio::test]
    async fn clipboard_reader_is_injectable_through_the_options() {
        let options = InteractiveOptions {
            clipboard: Some(Arc::new(FakeClipboard(
                crate::clipboard::ClipboardPaste::Image(clipboard_image()),
            ))),
            ..InteractiveOptions::default()
        };

        let reader = options.clipboard.clone().expect("injected reader");
        match reader.read() {
            crate::clipboard::ClipboardPaste::Image(image) => {
                assert_eq!(image.mime_type, "image/png");
                assert_eq!(image.data, "QUJD");
            }
            other => panic!("unexpected paste: {other:?}"),
        }
    }

    #[tokio::test]
    async fn new_session_clears_the_composer_chips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = session_options(dir.path(), "chips-old");

        app.set_editor_text("draft");
        apply_clipboard_paste(
            &mut app,
            crate::clipboard::ClipboardPaste::Image(clipboard_image()),
        );
        assert_eq!(app.image_count(), 1);

        run_slash_command(&mut app, &agent, &mut options, "/new")
            .await
            .expect("new");

        assert_eq!(app.image_count(), 0, "/new must drop the old draft's chips");
        assert_eq!(app.editor_text(), "");
    }

    // -----------------------------------------------------------------------
    // Thinking level (Stage 67 / LUM-1230)
    // -----------------------------------------------------------------------

    /// A model whose API family plus id mark it reasoning-capable — the port's
    /// equivalent of upstream's `Model.reasoning === true`.
    fn reasoning_model() -> Model {
        Model {
            provider: ProviderId::new("openai"),
            id: "o3-mini".into(),
            api: Api::OpenAiChatCompletions,
            label: None,
            context_window: 1_000_000,
            max_output_tokens: 0,
        }
    }

    #[tokio::test]
    async fn the_thinking_cycle_moves_the_level_and_the_agent() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        app.set_thinking_supported(true);
        app.set_thinking_level(ThinkingLevel::Medium);

        handle_thinking_cycle(&mut app, &agent, None).await;

        assert_eq!(app.thinking_level(), ThinkingLevel::High);
        assert_eq!(
            agent.lock().await.thinking_level(),
            Some(ThinkingLevel::High)
        );
        assert_eq!(app.status_flash(), Some("Thinking level: high"));
    }

    #[tokio::test]
    async fn the_thinking_cycle_wraps_from_max_to_off() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        app.set_thinking_supported(true);
        app.set_thinking_level(ThinkingLevel::Max);

        handle_thinking_cycle(&mut app, &agent, None).await;

        assert_eq!(app.thinking_level(), ThinkingLevel::Off);
        assert_eq!(app.status_flash(), Some("Thinking level: off"));
    }

    #[tokio::test]
    async fn the_thinking_cycle_reports_a_model_that_cannot_reason() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        app.set_thinking_level(ThinkingLevel::Medium);

        handle_thinking_cycle(&mut app, &agent, None).await;

        assert!(!app.thinking_supported());
        assert_eq!(app.thinking_level(), ThinkingLevel::Off, "clamped to off");
        assert_eq!(
            app.status_flash(),
            Some("Current model does not support thinking"),
            "raising the level on a non-reasoning model must not be silent"
        );
    }

    #[tokio::test]
    async fn slash_thinking_sets_the_level_directly() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        let mut options = InteractiveOptions::default();

        run_slash_command(&mut app, &agent, &mut options, "/thinking high")
            .await
            .expect("thinking");

        assert_eq!(app.thinking_level(), ThinkingLevel::High);
        assert_eq!(
            agent.lock().await.thinking_level(),
            Some(ThinkingLevel::High)
        );
        assert_eq!(app.status_flash(), Some("Thinking level: high"));
        assert!(
            !app.selector_open(),
            "an explicit level never opens the selector"
        );
    }

    #[tokio::test]
    async fn slash_thinking_rejects_a_level_the_model_cannot_honour() {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = InteractiveOptions::default();

        run_slash_command(&mut app, &agent, &mut options, "/thinking high")
            .await
            .expect("thinking");

        // Upstream `handleThinkingCommand` only reports; it never calls
        // `setThinkingLevel`, so the level is left exactly as it was.
        assert_eq!(app.thinking_level(), ThinkingLevel::Medium, "untouched");
        assert_eq!(agent.lock().await.thinking_level(), None);
        assert!(app.status_flash().is_none());
        let text = transcript(&app);
        assert!(text.contains("Unknown thinking level \"high\""), "{text}");
        assert!(text.contains("Available levels: off"), "{text}");
    }

    #[tokio::test]
    async fn slash_thinking_without_an_argument_opens_the_selector() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        let mut options = InteractiveOptions::default();
        app.set_thinking_level(ThinkingLevel::Medium);

        run_slash_command(&mut app, &agent, &mut options, "/thinking")
            .await
            .expect("thinking");

        assert!(app.selector_open());
        // Upstream highlights the current level when the list opens.
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some("thinking:medium")
        );
    }

    #[tokio::test]
    async fn choosing_a_thinking_level_from_the_selector_applies_it() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        let mut options = InteractiveOptions::default();
        run_slash_command(&mut app, &agent, &mut options, "/thinking")
            .await
            .expect("thinking");

        apply_thinking_selector_value(&mut app, &agent, "thinking:high", false, None).await;

        assert_eq!(app.thinking_level(), ThinkingLevel::High);
        assert_eq!(
            agent.lock().await.thinking_level(),
            Some(ThinkingLevel::High)
        );
        assert_eq!(app.status_flash(), Some("Thinking level: high"));
    }

    #[tokio::test]
    async fn thinking_selector_values_ignore_anything_but_a_level() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;

        apply_thinking_selector_value(&mut app, &agent, "model:gpt-4o", false, None).await;
        assert_eq!(app.thinking_level(), ThinkingLevel::Medium, "untouched");
        apply_thinking_selector_value(&mut app, &agent, "thinking:bogus", false, None).await;
        assert_eq!(app.thinking_level(), ThinkingLevel::Medium, "untouched");
    }

    #[test]
    fn persisting_the_default_thinking_level_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources = temp_sources(dir.path());
        let mut app = settings_app();

        assert!(persist_default_thinking_level(
            &mut app,
            &sources,
            ThinkingLevel::High
        ));
        let raw = std::fs::read_to_string(user_settings_path(dir.path())).expect("settings");
        assert!(raw.contains("\"defaultThinkingLevel\": \"high\""), "{raw}");
        assert_eq!(
            config::load_default_thinking_level(&sources),
            ThinkingLevel::High
        );

        // A failed write is reported, never a silent drop.
        std::fs::write(dir.path().join("blocked"), b"not a directory").expect("blocker");
        let unwritable = ConfigSources {
            user: Some(dir.path().join("blocked").join(config::SETTINGS_FILE_NAME)),
            project: None,
        };
        let mut app = settings_app();
        assert!(!persist_default_thinking_level(
            &mut app,
            &unwritable,
            ThinkingLevel::Low
        ));
        assert!(transcript(&app).contains("/thinking:"));
    }

    // -----------------------------------------------------------------------
    // `/resume` + `/tree` picker chords (Stage 68 / LUM-1255)
    // -----------------------------------------------------------------------

    /// Two stored sessions in `dir`, the second one named. The names are
    /// what `app.session.toggleNamedFilter` filters on.
    fn two_sessions(dir: &Path) -> (SessionRef, SessionRef) {
        let mut first = None;
        let mut second = None;
        for (index, name) in [None, Some("named one")].into_iter().enumerate() {
            let id = format!("picker-{index}");
            let path = dir.join(format!("{id}.sqlite"));
            let writer = SessionWriter::open(&path).expect("writer");
            writer
                .write_header(SessionEntry::Header {
                    id: id.clone(),
                    created_at: chrono::Utc::now(),
                    version: "0.1.0".into(),
                })
                .expect("header");
            writer.append(session_user("hello")).expect("append");
            if let Some(name) = name {
                writer.set_session_name(name).expect("name");
            }
            writer.checkpoint().expect("checkpoint");
            drop(writer);
            let reference = crate::list_resumable(dir)
                .expect("list")
                .into_iter()
                .find(|reference| reference.session_id == id)
                .expect("stored session");
            if index == 0 {
                first = Some(reference);
            } else {
                second = Some(reference);
            }
        }
        (first.expect("first"), second.expect("second"))
    }

    /// An App with the `/resume` picker open over `dir`, and the two
    /// stored sessions.
    async fn resume_picker(dir: &Path) -> (App, Arc<AsyncMutex<Agent>>, InteractiveOptions) {
        // The sessions must be on disk before the picker reads them.
        let (_first, _second) = two_sessions(dir);
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = stored_options(dir, "picker-0");
        open_resume_selector(&mut app, &mut options);
        assert!(app.selector_open(), "the picker is open");
        (app, agent, options)
    }

    /// Put the session picker's cursor on `resume:<session_id>`.
    ///
    /// Never "row 0" or "one row down": the listing order depends on the
    /// sort mode and on how close together the two fixtures were created.
    fn move_resume_cursor_to(app: &mut App, session_id: &str) {
        let index = app
            .selector()
            .expect("selector")
            .items()
            .iter()
            .position(|item| item.value == format!("resume:{session_id}"))
            .unwrap_or_else(|| panic!("{session_id} is not listed"));
        app.selector_mut().expect("selector").set_cursor(index);
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some(format!("resume:{session_id}").as_str())
        );
    }

    /// Every visible row's description, in order.
    fn selector_descriptions(app: &App) -> Vec<String> {
        app.selector()
            .expect("selector")
            .items()
            .iter()
            .map(|item| item.description.clone().unwrap_or_default())
            .collect()
    }

    fn chord(code: KeyCode, modifiers: pi_tui::input::KeyModifiers) -> pi_tui::input::Key {
        pi_tui::input::Key::new(code, modifiers)
    }

    fn ctrl(c: char) -> pi_tui::input::Key {
        chord(
            KeyCode::Char(c),
            pi_tui::input::KeyModifiers {
                control: true,
                ..Default::default()
            },
        )
    }

    /// Drive one chord through `handle_input_event` — the same path a
    /// real keypress takes, so the interception is what is under test.
    async fn send(
        app: &mut App,
        agent: &Arc<AsyncMutex<Agent>>,
        options: &mut InteractiveOptions,
        key: pi_tui::input::Key,
    ) {
        let mut bash = BashRunner::default();
        handle_input_event(app, agent, options, &mut bash, InputEvent::Key(key))
            .await
            .expect("handle event");
    }

    #[tokio::test]
    async fn the_resume_picker_blocks_a_rename_from_deleting_the_live_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        // The live session is `picker-0` (the App is attached to it).
        move_resume_cursor_to(&mut app, "picker-0");
        let mut bash = BashRunner::default();
        handle_input_event(
            &mut app,
            &agent,
            &mut options,
            &mut bash,
            InputEvent::Key(ctrl('d')),
        )
        .await
        .expect("ctrl+d");

        // Upstream `cannot delete the currently active session`: no
        // confirmation is armed and the file survives.
        assert!(options.pickers.session.pending_delete.is_none());
        assert!(transcript(&app).contains("Cannot delete the currently active session"));
        assert!(dir.path().join("picker-0.sqlite").is_file());
    }

    #[tokio::test]
    async fn ctrl_d_arms_and_enter_confirms_a_session_delete() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        // Move onto the second session, which is not the live one.
        move_resume_cursor_to(&mut app, "picker-1");

        send(&mut app, &agent, &mut options, ctrl('d')).await;
        assert_eq!(
            options.pickers.session.pending_delete.as_deref(),
            Some("picker-1"),
            "the first chord only arms the confirmation"
        );
        assert!(
            dir.path().join("picker-1.sqlite").is_file(),
            "nothing is deleted before the second chord"
        );
        // While confirming, every other key is swallowed.
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Down, Default::default()),
        )
        .await;
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some("resume:picker-1"),
            "the cursor does not move while confirming"
        );
        assert_eq!(
            options.pickers.session.pending_delete.as_deref(),
            Some("picker-1")
        );

        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Enter, Default::default()),
        )
        .await;
        assert!(options.pickers.session.pending_delete.is_none());
        assert!(
            !dir.path().join("picker-1.sqlite").is_file(),
            "file removed"
        );
        assert!(
            crate::list_resumable(dir.path())
                .expect("list")
                .iter()
                .all(|reference| reference.session_id != "picker-1"),
            "the deleted session is gone from the listing"
        );
        assert!(dir.path().join("picker-0.sqlite").is_file(), "live kept");
    }

    #[tokio::test]
    async fn escape_cancels_the_delete_confirmation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        move_resume_cursor_to(&mut app, "picker-1");
        send(&mut app, &agent, &mut options, ctrl('d')).await;
        assert_eq!(
            options.pickers.session.pending_delete.as_deref(),
            Some("picker-1")
        );
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Esc, Default::default()),
        )
        .await;

        assert!(options.pickers.session.pending_delete.is_none());
        assert!(app.selector_open(), "the picker survives the cancel");
        assert!(dir.path().join("picker-1.sqlite").is_file());
    }

    #[tokio::test]
    async fn the_resume_picker_cycles_sort_name_filter_and_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        let title = |app: &App| app.selector().expect("selector").title().to_string();
        let description = |app: &App| selector_descriptions(app).join(" | ");

        assert!(
            title(&app).contains("sort: newest · name: all"),
            "{}",
            title(&app)
        );
        send(&mut app, &agent, &mut options, ctrl('s')).await;
        assert!(title(&app).contains("sort: oldest"), "{}", title(&app));
        send(&mut app, &agent, &mut options, ctrl('s')).await;
        assert!(title(&app).contains("sort: name"), "{}", title(&app));
        send(&mut app, &agent, &mut options, ctrl('s')).await;
        assert!(title(&app).contains("sort: newest"), "{}", title(&app));

        // `Ctrl+N` keeps only the named sessions.
        send(&mut app, &agent, &mut options, ctrl('n')).await;
        assert!(title(&app).contains("name: named"), "{}", title(&app));
        let values = app
            .selector()
            .expect("selector")
            .items()
            .iter()
            .map(|item| item.value.clone())
            .collect::<Vec<_>>();
        assert_eq!(values, vec!["resume:picker-1".to_string()], "{values:?}");
        send(&mut app, &agent, &mut options, ctrl('n')).await;
        assert_eq!(app.selector().expect("selector").items().len(), 2);

        // `Ctrl+P` adds the file path to every row.
        assert!(
            !description(&app).contains(".sqlite"),
            "{}",
            description(&app)
        );
        send(&mut app, &agent, &mut options, ctrl('p')).await;
        assert!(
            description(&app).contains("picker-0.sqlite"),
            "{}",
            description(&app)
        );
        send(&mut app, &agent, &mut options, ctrl('p')).await;
        assert!(
            !description(&app).contains(".sqlite"),
            "{}",
            description(&app)
        );
    }

    #[tokio::test]
    async fn an_empty_resume_picker_still_answers_its_chords() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let mut options = session_options(dir.path(), "empty-picker");
        options.pickers.session.filter = SessionFilter::NamedOnly;
        open_resume_selector(&mut app, &mut options);

        let selector = app.selector().expect("selector");
        assert!(selector.items().is_empty(), "nothing to resume");
        assert!(
            selector.footer().iter().any(|line| line.contains("ctrl+n")),
            "the footer names the way out: {:?}",
            selector.footer()
        );
        assert_eq!(picker_kind(selector), PickerKind::Session, "classified");

        // The chord that undoes the filter works on an empty list.
        send(&mut app, &agent, &mut options, ctrl('n')).await;
        assert_eq!(options.pickers.session.filter, SessionFilter::All);
    }

    #[tokio::test]
    async fn ctrl_r_opens_the_rename_dialog_and_enter_applies_the_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        move_resume_cursor_to(&mut app, "picker-0");
        send(&mut app, &agent, &mut options, ctrl('r')).await;
        assert!(options.pickers.session.pending_rename.is_some());
        assert!(app.dialog_open(), "the input modal is up");

        // Type a name and confirm it.
        for c in "renamed".chars() {
            send(
                &mut app,
                &agent,
                &mut options,
                chord(KeyCode::Char(c), Default::default()),
            )
            .await;
        }
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Enter, Default::default()),
        )
        .await;

        assert!(options.pickers.session.pending_rename.is_none());
        let stored = SessionReader::open(dir.path().join("picker-0.sqlite"))
            .expect("reader")
            .session_name("picker-0")
            .expect("name")
            .expect("renamed");
        assert_eq!(stored, "renamed");
        let named = crate::list_resumable(dir.path())
            .expect("list")
            .into_iter()
            .find(|reference| reference.session_id == "picker-0")
            .expect("session");
        assert_eq!(named.name.as_deref(), Some("renamed"));
    }

    #[tokio::test]
    async fn escape_abandons_the_rename_dialog_and_keeps_the_picker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        move_resume_cursor_to(&mut app, "picker-0");
        send(&mut app, &agent, &mut options, ctrl('r')).await;
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Esc, Default::default()),
        )
        .await;

        assert!(options.pickers.session.pending_rename.is_none());
        assert!(!app.dialog_open());
        assert!(app.selector_open(), "the picker is still there");
        let stored = SessionReader::open(dir.path().join("picker-0.sqlite"))
            .expect("reader")
            .session_name("picker-0")
            .expect("name");
        assert!(stored.is_none(), "nothing was renamed: {stored:?}");
    }

    #[tokio::test]
    async fn ctrl_backspace_only_deletes_with_an_empty_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = resume_picker(dir.path()).await;

        move_resume_cursor_to(&mut app, "picker-1");
        // A typed query forwards `Ctrl+Backspace` to the search input.
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Char('p'), Default::default()),
        )
        .await;
        let ctrl_backspace = chord(
            KeyCode::Backspace,
            pi_tui::input::KeyModifiers {
                control: true,
                ..Default::default()
            },
        );
        send(&mut app, &agent, &mut options, ctrl_backspace).await;
        assert!(
            options.pickers.session.pending_delete.is_none(),
            "the chord belongs to the search input while a query is typed"
        );

        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Esc, Default::default()),
        )
        .await;
        assert!(!app.selector_open(), "Esc clears the query first");
        open_resume_selector(&mut app, &mut options);
        move_resume_cursor_to(&mut app, "picker-1");
        send(&mut app, &agent, &mut options, ctrl_backspace).await;
        assert_eq!(
            options.pickers.session.pending_delete.as_deref(),
            Some("picker-1"),
            "with no query it arms the confirmation"
        );
    }

    /// A two-branch session in a `/tree` overlay, plus the driver state.
    async fn tree_picker(dir: &Path) -> (App, Arc<AsyncMutex<Agent>>, InteractiveOptions) {
        let (mut app, agent) = app_starting_at(small_window_model(1_000_000)).await;
        let path = build_branch_session(dir, "tree-picker");
        let mut options = InteractiveOptions {
            session_id: "tree-picker".into(),
            session_database: Some(path),
            session_log: Some(SessionLog::open(dir, "tree-picker").expect("log")),
            ..InteractiveOptions::default()
        };
        run_slash_command(&mut app, &agent, &mut options, "/tree")
            .await
            .expect("/tree");
        assert!(app.selector_open(), "the overlay is open");
        (app, agent, options)
    }

    fn tree_values(app: &App) -> Vec<String> {
        app.selector()
            .expect("selector")
            .items()
            .iter()
            .filter_map(|item| item.value.strip_prefix("tree:").map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn the_tree_filter_chords_narrow_and_widen_the_overlay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = tree_picker(dir.path()).await;

        let all = tree_values(&app);
        assert!(all.len() >= 4, "the branch session renders: {all:?}");
        assert_eq!(tree_footer_filter(&app), "default");

        // `Ctrl+U` keeps user messages only.
        send(&mut app, &agent, &mut options, ctrl('u')).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::UserOnly);
        assert_eq!(tree_footer_filter(&app), "user-only");
        let user = tree_values(&app);
        assert!(user.len() < all.len(), "narrowed: {user:?} vs {all:?}");
        assert!(user.contains(&"e1".to_string()), "{user:?}");

        // Pressing the active mode again falls back to `default`.
        send(&mut app, &agent, &mut options, ctrl('u')).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::Default);

        // The explicit `default` chord always resets, even from a mode the
        // toggle would have left alone.
        send(&mut app, &agent, &mut options, ctrl('t')).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::NoTools);
        send(&mut app, &agent, &mut options, ctrl('d')).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::Default);

        // Cycling walks the whole ring in both directions.
        send(&mut app, &agent, &mut options, ctrl('o')).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::NoTools);
        let backward = chord(
            KeyCode::Char('o'),
            pi_tui::input::KeyModifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
        );
        send(&mut app, &agent, &mut options, backward).await;
        assert_eq!(options.pickers.tree.filter, TreeFilter::Default);
    }

    /// The `filter: …` field of the overlay footer.
    fn tree_footer_filter(app: &App) -> String {
        app.selector()
            .expect("selector")
            .footer()
            .iter()
            .filter(|line| line.contains("filter: "))
            .find_map(|line| line.split("filter: ").nth(1))
            .map(|tail| tail.split(" ·").next().unwrap_or(tail).trim().to_string())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn ctrl_left_folds_a_branch_and_ctrl_left_again_jumps_to_its_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = tree_picker(dir.path()).await;

        // `e2` is the branch point (children `e3` and `e5`), so its
        // children — not `e2` itself — are foldable, exactly like
        // upstream's `isFoldable`: the only child of a single-child chain
        // is not foldable, because folding it would hide the rest of the
        // chain.
        move_cursor_to(&mut app, "e2");
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Left, mods_ctrl()),
        )
        .await;
        assert!(
            options.pickers.tree.folded.is_empty(),
            "the branch point is not foldable"
        );
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some("tree:e2"),
            "so the chord climbs to the segment start"
        );

        // Put the cursor on `e3`, which does start a segment: `⌃←` folds
        // it and hides `e4`.
        move_cursor_to(&mut app, "e3");
        let before = tree_values(&app);
        assert!(before.contains(&"e4".to_string()), "{before:?}");
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Left, mods_ctrl()),
        )
        .await;
        assert!(
            options.pickers.tree.folded.contains("e3"),
            "the segment start folds: {:?}",
            options.pickers.tree.folded
        );
        let folded = tree_values(&app);
        assert!(folded.len() < before.len(), "{folded:?} vs {before:?}");
        assert!(!folded.contains(&"e4".to_string()), "{folded:?}");
        assert!(folded.contains(&"e3".to_string()), "{folded:?}");

        // Already folded: the chord climbs the visible parents to the
        // first segment start above the cursor (`e2`).
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Left, mods_ctrl()),
        )
        .await;
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some("tree:e2"),
            "the fold chord falls back to moving up"
        );
    }

    /// Put the picker cursor on the row whose value is `tree:<entry_id>`.
    fn move_cursor_to(app: &mut App, entry_id: &str) {
        let index = app
            .selector()
            .expect("selector")
            .items()
            .iter()
            .position(|item| item.value == format!("tree:{entry_id}"))
            .unwrap_or_else(|| panic!("{entry_id} is not visible"));
        app.selector_mut().expect("selector").set_cursor(index);
        assert_eq!(
            app.selector().and_then(|s| s.selected_value()),
            Some(format!("tree:{entry_id}").as_str())
        );
    }

    #[tokio::test]
    async fn ctrl_right_unfolds_what_ctrl_left_folded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = tree_picker(dir.path()).await;

        move_cursor_to(&mut app, "e3");
        let all = tree_values(&app);

        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Left, mods_ctrl()),
        )
        .await;
        send(
            &mut app,
            &agent,
            &mut options,
            chord(KeyCode::Right, mods_ctrl()),
        )
        .await;
        assert!(options.pickers.tree.folded.is_empty());
        assert_eq!(tree_values(&app), all, "the whole tree is back");
    }

    fn mods_ctrl() -> pi_tui::input::KeyModifiers {
        pi_tui::input::KeyModifiers {
            control: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn shift_t_toggles_the_entry_timestamp_in_the_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut app, agent, mut options) = tree_picker(dir.path()).await;

        let descriptions = selector_descriptions;
        assert!(!options.pickers.tree.show_label_timestamps);
        let before = descriptions(&app);

        let shift_t = chord(
            KeyCode::Char('t'),
            pi_tui::input::KeyModifiers {
                shift: true,
                ..Default::default()
            },
        );
        send(&mut app, &agent, &mut options, shift_t).await;
        assert!(options.pickers.tree.show_label_timestamps);
        assert_ne!(descriptions(&app), before, "the column switched");

        send(&mut app, &agent, &mut options, shift_t).await;
        assert!(!options.pickers.tree.show_label_timestamps);
        assert_eq!(descriptions(&app), before);
    }

    #[tokio::test]
    async fn another_selector_keeps_its_own_keys() {
        let (mut app, agent) = app_starting_at(reasoning_model()).await;
        let mut options = InteractiveOptions::default();
        run_slash_command(&mut app, &agent, &mut options, "/thinking")
            .await
            .expect("/thinking");
        assert_eq!(
            picker_kind(app.selector().expect("selector")),
            PickerKind::Other,
            "the thinking picker is not a session/tree picker"
        );

        // `Ctrl+D` in the thinking picker is nobody's chord here: the
        // picker stays open and nothing is armed.
        send(&mut app, &agent, &mut options, ctrl('d')).await;
        assert!(app.selector_open());
        assert!(options.pickers.session.pending_delete.is_none());
    }

    // -----------------------------------------------------------------------
    // Extension lifecycle fan-out (LUM-1246)
    // -----------------------------------------------------------------------

    /// Load one extension source into a fresh host and wrap it in a runtime
    /// that reports exactly `subscribed`.
    async fn extension_runtime(
        tag: &str,
        source: &str,
        subscribed: &[&str],
    ) -> (ExtensionRuntime, JsExtensionHost) {
        let dir = std::env::temp_dir().join(format!("pi-ext-events-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("extension.js");
        std::fs::write(&file, source).expect("write extension");
        let host = JsExtensionHost::new().await.expect("host");
        host.load(
            pi_extensions::ExtensionEntry {
                source: file.clone(),
                id: tag.to_string(),
                label: None,
            },
            source,
        )
        .await
        .expect("load");
        let _ = std::fs::remove_dir_all(&dir);
        (ExtensionRuntime::for_test(host.clone(), subscribed), host)
    }

    /// Poll until the extension has recorded an entry named `name` and
    /// return every recorded entry type in order.
    async fn wait_for_entry(host: &JsExtensionHost, name: &str) -> Vec<String> {
        for _ in 0..300 {
            let kinds: Vec<String> = host
                .log()
                .entries
                .iter()
                .map(|entry| entry.custom_type.clone())
                .collect();
            if kinds.iter().any(|kind| kind == name) {
                return kinds;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "extension never saw {name:?}; recorded {:?}",
            host.log().entries.len()
        );
    }

    /// The whole point of the fan-out: an extension that subscribes to the
    /// upstream lifecycle names sees a real turn, today only `session_start`
    /// and `resources_discover` ever fired.
    #[tokio::test]
    async fn a_js_extension_sees_a_live_turn_through_the_pump() {
        let source = r#"
            module.exports = function (pi) {
                pi.on("agent_start", () => pi.appendEntry("agent_start", {}));
                pi.on("turn_start", (event) => pi.appendEntry("turn_start", {
                    index: event.turnIndex,
                }));
                pi.on("message_update", (event) => pi.appendEntry("message_update", {
                    kind: event.assistantMessageEvent.type,
                }));
                pi.on("message_end", () => pi.appendEntry("message_end", {}));
                pi.on("turn_end", () => pi.appendEntry("turn_end", {}));
                pi.on("agent_end", (event) => pi.appendEntry("agent_end", {
                    messages: event.messages.length,
                }));
                pi.on("session_shutdown", () => pi.appendEntry("session_shutdown", {}));
                // Subscribed by the runtime but intentionally absent from the
                // handler table above: `message_start` is dropped by the shim.
                pi.on("message_start", () => pi.appendEntry("message_start", {}));
            };
        "#;
        let (runtime, host) = extension_runtime(
            "fanout",
            source,
            &[
                "agent_start",
                "turn_start",
                "message_update",
                "message_end",
                "turn_end",
                "agent_end",
                "session_shutdown",
            ],
        )
        .await;
        let runtime = Arc::new(runtime);
        let (_, agent) = app_starting_at(catalog_model("faux", "faux-model")).await;

        let pump = start_extension_event_pump(&agent, Some(&runtime))
            .await
            .expect("a subscribing extension installs the pump");
        agent.lock().await.prompt("hi").await.expect("turn");

        let kinds = wait_for_entry(&host, "agent_end").await;
        pump.abort();

        assert_eq!(kinds.first().map(String::as_str), Some("agent_start"));
        for expected in ["turn_start", "message_update", "message_end", "turn_end"] {
            assert!(kinds.iter().any(|kind| kind == expected), "{kinds:?}");
        }
        assert_eq!(kinds.last().map(String::as_str), Some("agent_end"));
        // `message_start` was subscribed by the runtime but has no handler,
        // and the gating test below covers the other direction: an event
        // nobody subscribed to never reaches the shim.
        let agent_end = host
            .log()
            .entries
            .iter()
            .find(|entry| entry.custom_type == "agent_end")
            .cloned()
            .expect("agent_end entry");
        assert!(agent_end.data["messages"].as_u64().unwrap_or(0) >= 2);

        // Teardown event: on the exit path, not through the fan-out.
        assert!(runtime.deliver_shutdown(SessionShutdownReason::Quit).await);
        let kinds = wait_for_entry(&host, "session_shutdown").await;
        assert_eq!(kinds.last().map(String::as_str), Some("session_shutdown"));
    }

    /// No subscribers → no task, no channel, no serialisation.
    #[tokio::test]
    async fn the_pump_is_skipped_when_nothing_subscribed() {
        let (_, agent) = app_starting_at(catalog_model("faux", "faux-model")).await;
        assert!(start_extension_event_pump(&agent, None).await.is_none());

        let (runtime, _host) =
            extension_runtime("silent", "module.exports = function (pi) {};", &[]).await;
        assert!(!runtime.has_any_subscriber());
        let runtime = Arc::new(runtime);
        assert!(start_extension_event_pump(&agent, Some(&runtime))
            .await
            .is_none());
    }

    /// A runtime only forwards the names its extensions subscribed to.
    #[tokio::test]
    async fn unsubscribed_events_never_cross_into_js() {
        let source = r#"
            module.exports = function (pi) {
                pi.on("turn_start", (event) => pi.appendEntry("turn_start", {
                    index: event.turnIndex,
                }));
            };
        "#;
        let (runtime, host) = extension_runtime("gated", source, &["turn_start"]).await;

        assert!(!runtime.deliver_event(&ExtensionEvent::AgentStart).await);
        assert!(
            runtime
                .deliver_event(&ExtensionEvent::TurnStart {
                    turn_index: 0,
                    timestamp: 0,
                })
                .await
        );
        assert_eq!(host.log().entries.len(), 1);
        assert_eq!(host.log().entries[0].custom_type, "turn_start");
    }
}

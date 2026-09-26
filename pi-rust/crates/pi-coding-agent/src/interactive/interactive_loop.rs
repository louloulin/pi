//! Main interactive loop + the outcome type it returns.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR8 of the M1 single-file
//! split; see `scripts/architecture_no_regression.sh`).
//!
//! Two pieces live here:
//!
//! - [`RunOutcome`] — the value the inner loop hands back to
//!   `run_interactive` so the outer driver can compose the resume hint,
//!   restore the transcript on exit, and report the user-visible exit
//!   reason.
//! - [`run_loop`] — the inner driver loop. One tick per UI frame: drain
//!   the agent event queue, draw the frame, read the next input, dispatch
//!   it to [`super::handle_input_event`] (still in `mod.rs` until the
//!   keymap PR). The loop exits when `app.is_exit_requested()` flips or
//!   `Ctrl+C` arms the shutdown signal.
//!
//! Because this loop is the bridge between every other module
//! (extensions, bash runner, picker, completion, settings, model switcher,
//! session lifecycle), almost every free function it calls has to be
//! re-imported here via `use super::{...}`. The split is intentionally
//! one-way: the loop calls `super::*`; no module calls back into the
//! loop.

use std::io::{Stdout, Write};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self as ct_event, Event as CtEvent};
use pi_agent_core::Agent;
use pi_protocol::SessionShutdownReason;
use pi_tui::app::{App, AppConfig};
use pi_tui::input::InputEvent;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex as AsyncMutex;

use super::{
    apply_clipboard_paste, apply_startup_ui_settings, available_provider_count, deliver_pending,
    drain_ready_events, exit_screen_output, extension_autocomplete_commands,
    flush_terminal_title, handle_input_event, install_composer_autocomplete,
    install_extension_autocomplete, maybe_auto_compact, persist_extension_side_effects,
    resume_command, run_external_editor, seed_model_scope, settings_sources,
    start_extension_event_pump, suspend_to_background, sync_status_metrics, write_exit_output,
    ExtensionLifecycleHooks, FullscreenExitOutput, InternalAction, InteractiveExit,
    InteractiveOptions, UiPromptObserver, EXTENSION_SHUTDOWN_TIMEOUT,
};
use super::bash_runner::BashRunner;
use crate::config;
use crate::extensions::ui_bridge::RegionPump;

/// Decide whether a `set_theme_by_name` failure leaves the user with a
/// usable fallback palette or whether they genuinely need a status-bar
/// flash.
///
/// The App is constructed with `dark` as its initial palette, so a
/// failure to load the user-requested theme simply keeps that
/// default in place. The only failures worth surfacing in the status
/// bar are the ones that mean even the default cannot be relied on —
/// missing colour tokens, unparseable hex, broken `vars`, …
///
/// "Theme not found" / "invalid theme name" land here in practice:
/// users frequently port a `theme` field from a TS pi config (e.g.
/// `"upup-dark"`) into a Rust pi `settings.json` and the request fails
/// to resolve because the port does not ship that palette. Showing a
/// panic-y "Theme not applied: …" banner for what is effectively a
/// cosmetic mismatch crowded the first TUI frame; routing the
/// notice to the persistent startup log keeps the audit trail
/// without polluting the screen.
fn is_fallback_sufficient_theme_error(err: &str) -> bool {
    err.starts_with("Theme not found:") || err.starts_with("Invalid theme name:")
}

/// What one interactive run hands back to [`run_interactive`].
pub(super) struct RunOutcome {
    /// Why the loop stopped.
    pub(super) exit: InteractiveExit,
    /// Bytes the driver writes to stdout *after* leaving the alternate screen
    /// — empty when the session asked to leave nothing behind.
    ///
    /// Composed inside the loop because that is the only place holding both the
    /// live `App` (the transcript) and the resolved settings (the mode).
    pub(super) exit_output: String,
}

pub(super) async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: Arc<AsyncMutex<Agent>>,
    mut options: InteractiveOptions,
    config: AppConfig,
) -> anyhow::Result<RunOutcome> {
    // Build the App on the stack first so we can drop it before
    // tearing down the terminal.
    let mut app = App::new(&*agent.lock().await, config.clone());
    // `--resume <id>` seeds the stored session's `/name` so the status bar
    // and `/session` show it before the first command runs.
    app.set_session_name(options.session_name.clone());
    // LUM-1466 — upstream's footer is two rows: `pwd (branch) • name` above
    // the stats row (`components/footer.ts:119-127,230-231`). Only the driver
    // knows the process cwd and the repository it sits in, so it hands both
    // to the App. A session outside a repo — or on a detached HEAD — still
    // gets the `pwd` row, just without the `(branch)` suffix, which is
    // upstream's own `getGitBranch()` contract.
    //
    // LUM-1490 — the branch is no longer resolved once: `HEAD` moves outside
    // this process (`git checkout` in another terminal), so the driver keeps a
    // [`BranchTracker`] and re-reads it every frame. That single tracker feeds
    // both readers of the fact: the built-in footer and — through the
    // `footerData` snapshot pushed below — a custom footer.
    let mut branch_tracker = crate::footer::BranchTracker::new();
    if let Ok(cwd) = std::env::current_dir() {
        branch_tracker.poll(&cwd);
        app.set_status_git_branch(branch_tracker.branch().map(str::to_string));
        app.set_status_cwd(Some(cwd.display().to_string()));
    }
    // LUM-1467 — the stats row's provider prefix, ` (sub)` suffix, ` (auto)`
    // suffix and `$cost` rates are facts only the driver owns.
    let mut footer_facts = pi_extensions::FooterData {
        git_branch: branch_tracker.branch().map(str::to_string),
        available_provider_count: available_provider_count(&options),
    };
    {
        let agent_guard = agent.lock().await;
        sync_status_metrics(&mut app, &options, agent_guard.model());
    }
    // Wire the existing rich tool renderers (`tools/render.rs`) into the
    // interactive transcript. The App cannot name that type (no
    // `pi-tui` → `pi-coding-agent` dependency), so the driver installs the
    // adapter and the App owns the folding (collapsed preview, Ctrl+O,
    // click-to-toggle). Print mode keeps its own session in `text_fallback`.
    let tool_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // The same directory the tool renderer and the `@` completion walk: the
    // per-frame `HEAD` re-read needs one path the loop can keep borrowing after
    // `tool_cwd` is moved into the autocomplete provider below.
    let branch_cwd = tool_cwd.clone();

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
    let composer_provider = install_composer_autocomplete(
        &mut app,
        tool_cwd,
        extension_autocomplete_commands(&options),
    );
    // LUM-1448 — stack the extension-registered autocomplete wrappers on top
    // of that provider. `ctx.ui.addAutocompleteProvider` is dispatched during
    // the load pass, so by the time the App exists the shim already holds the
    // chain; this publishes the built-in provider it delegates to and installs
    // the composed provider. A wrapper registered *later* bumps the host's
    // generation counter and the loop below re-installs the chain.
    let mut autocomplete_generation = options
        .extensions
        .as_ref()
        .and_then(|runtime| runtime.host().map(|host| host.autocomplete_generation()))
        .unwrap_or(0);
    if let Some(runtime) = options.extensions.as_ref() {
        let base = composer_provider.clone();
        if install_extension_autocomplete(&mut app, runtime, base).await {
            autocomplete_generation = runtime
                .host()
                .map(|host| host.autocomplete_generation())
                .unwrap_or(autocomplete_generation);
        }
    }

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

    // Startup settings (LUM-1307) — the persisted `theme` /
    // `fullscreenCopyOnSelect` are applied to the App that is about to draw
    // its first frame. Upstream reads `settings.json` at session start; this
    // port only read it from `/settings` and `/reload`, so a hand-written
    // `theme: "light"` was invisible until one of those ran. A theme the
    // resolver rejects is reported instead of silently keeping the default.
    {
        let startup_ui = apply_startup_ui_settings(&mut app, &settings_sources());
        // `settings.json`'s `enabledModels` bounds the `Ctrl+P` cycle from the
        // first key press, the way upstream resolves it into
        // `session.scopedModels` before the loop starts (`main.ts:448`).
        seed_model_scope(&mut options, &settings_sources());
        if let Some(err) = &startup_ui.theme_error {
            // `set_theme_by_name` leaves the App's *previous* palette in
            // place on failure. The App is built with `dark` as its
            // initial palette (`App::new` in `pi-tui/app/mod.rs`), so a
            // simple "theme not found" — the case that hits users who
            // copied a TS pi `theme: "upup-dark"` into `settings.json`
            // — already has a usable fallback painted; flashing a
            // status bar warning only crowds the first frame.
            //
            // Anything else (missing colour tokens, invalid hex,
            // unresolvable variable, …) means even the fallback path is
            // unhappy, and the user genuinely needs to know.
            if is_fallback_sufficient_theme_error(err) {
                crate::startup_log::log_message(&format!(
                    "pi: theme fallback: {err} (kept built-in palette)"
                ));
            } else {
                app.flash_status(format!("Theme not applied: {err}"));
            }
        }
    }

    // Extension lifecycle fan-out (LUM-1246). Extensions subscribe to the
    // *upstream* event names (`turn_start`, `tool_execution_start`, …) that
    // the JS shim exposes, but until now nothing fed the agent's own event
    // stream into the host — `pi.on(...)` only ever fired for
    // `session_start` / `resources_discover`. A second subscriber to the
    // agent's fan-out (the first belongs to the App) drives every mapped
    // event into the host on its own task, so a slow plugin cannot stall
    // rendering. Skipped entirely when no loaded extension subscribed.
    let extension_event_bus = std::sync::Arc::new(crate::extensions::event_bus::EventBus::new());
    let extension_pump = start_extension_event_pump(
        &agent,
        options.extensions.as_ref(),
        std::sync::Arc::clone(&extension_event_bus),
    )
    .await;
    // The events that fire *inside* a run (`context`, `before_agent_start`, …)
    // need an async answer, so they ride [`LifecycleHooks`] instead of the
    // synchronous fan-out above. Installed before the first prompt, and only
    // when extensions actually loaded.
    if let Some(runtime) = options.extensions.as_ref() {
        let hooks = Arc::new(ExtensionLifecycleHooks::new(runtime.clone()));
        agent.lock().await.hooks_mut().lifecycle = Some(hooks.clone());
        // Upstream `ui_prompt_start` / `ui_prompt_end` bracket every blocking
        // `ctx.ui.*` dialog. The TUI dialog handler forwards those edges; the
        // `Weak` keeps the runtime from being pinned by the handler.
        if let Some(ui) = options.extension_ui.as_ref() {
            let observer: Arc<dyn UiPromptObserver> = hooks.clone();
            ui.bridge().set_prompt_observer(Arc::downgrade(&observer));
        }
    }

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

    // Seed the `footerData` snapshot before the first frame. A custom footer
    // installed from `session_start` reads `getGitBranch()` during its very
    // first render, and [`ExtensionRuntime::sync_footer_data`] does not report
    // the initial value as a branch transition (upstream's `onBranchChange`
    // fires on a change, never on the initial value).
    if let Some(runtime) = options.extensions.as_ref() {
        runtime.sync_footer_data(footer_facts.clone()).await;
    }

    loop {
        // Drain pending agent events before drawing so the TUI sees
        // fresh state on every tick.
        app.drain_agent_events();
        // Surface a provider / agent error that the App captured during the
        // last drain — without this the user sees nothing when the model
        // call fails, which looks like the prompt was never sent. A flash
        // (not a permanent info block) so the message disappears once the
        // reader has had a chance to see it, matching the rest of the
        // transient-status policy (`App::flash_status`).
        if let Some(err) = app.take_error() {
            app.flash_status(format!("Agent error: {err}"));
        }
        // A local `!` command may have finished since the last tick; fold it
        // into the transcript (the task owns the process, the loop owns the
        // App). A no-op when no command is running.
        if bash.is_running() {
            let width = terminal.size().map(|area| area.width).unwrap_or(80);
            bash.poll(&mut app, width);
        }
        // LUM-1490 — `HEAD` moves outside this process (`git checkout` in
        // another terminal), so the branch both footers read is re-read every
        // tick; upstream watches the file and repaints on change
        // (`footer-data-provider.ts:139-196`). The tracker reports a
        // *transition* only, so an idle frame costs one small read, and a real
        // move updates both readers: the built-in footer through the App and a
        // custom footer through the `footerData` query channel (whose
        // `onBranchChange` subscribers are told from inside
        // `sync_footer_data`).
        if branch_tracker.poll(&branch_cwd) {
            let branch = branch_tracker.branch().map(str::to_string);
            app.set_status_git_branch(branch.clone());
            footer_facts.git_branch = branch;
            if let Some(runtime) = options.extensions.as_ref() {
                runtime.sync_footer_data(footer_facts.clone()).await;
            }
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
        // LUM-1485 — the terminal title (`ctx.ui.setTitle` or the automatic
        // `pi - <session> - <cwd>`). The App only queues it: the OSC 0
        // sequence has to go to the tty *outside* the frame buffer, or the
        // cell grid would count it as text. Written before the draw so a
        // `setTitle` that just arrived lands in this tick.
        flush_terminal_title(&mut app, terminal.backend_mut());
        // A late `ctx.ui.addAutocompleteProvider` (from a command handler or
        // any event after startup) bumps the host's generation counter; pick
        // the chain up again so its trigger characters reach the editor.
        if let Some(runtime) = options.extensions.as_ref() {
            let generation = runtime
                .host()
                .map(|host| host.autocomplete_generation())
                .unwrap_or(autocomplete_generation);
            if generation != autocomplete_generation {
                autocomplete_generation = generation;
                let base = composer_provider.clone();
                install_extension_autocomplete(&mut app, runtime, base).await;
            }
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
        //
        // While a paste burst is accumulating (LUM-1461) the timeout is
        // shortened to land just after it goes quiet: the classifier flushes
        // on a time boundary, and a 50 ms poll would leave the pasted text
        // invisible for a beat after the terminal stopped sending it.
        let poll_interval = app
            .paste_burst_deadline()
            .map(|deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .max(Duration::from_millis(1))
                    .min(config.event_poll_interval)
            })
            .unwrap_or(config.event_poll_interval);
        if ct_event::poll(poll_interval)? {
            for event in drain_ready_events(ct_event::poll, ct_event::read)? {
                // Bracketed paste is not expressible as an `InputEvent` (that
                // enum is `Copy` and the payload is owned), so the driver
                // routes it straight to the composer. Without this branch the
                // payload was translated to `Ignored` and the paste was
                // dropped entirely; with bracketed paste disabled, the same
                // bytes used to arrive as keystrokes and every newline in a
                // pasted block submitted the draft.
                if let CtEvent::Paste(text) = &event {
                    app.step_paste(text);
                    continue;
                }
                // `translate_event` drops Windows key *release* events, which
                // carry no input but would otherwise replay every keystroke
                // (see its doc comment).
                let Some(translated) = App::translate_event(event) else {
                    continue;
                };
                if let Some(action) =
                    handle_input_event(&mut app, &agent, &mut options, &mut bash, translated)
                        .await?
                {
                    match action {
                        InternalAction::Exit => break,
                        InternalAction::ExternalEditor { command } => {
                            run_external_editor(terminal, &mut app, &command);
                        }
                        InternalAction::Suspend => {
                            suspend_to_background(terminal, &mut app);
                        }
                        // A global chord consumed the event without
                        // producing a side-effect the driver has to run
                        // (`keymap::dispatch_global_chord_table` returns
                        // this when `app.model.cycleForward`,
                        // `app.thinking.cycle`, etc. fire); the inner
                        // loop's redraw handles the visual update.
                        InternalAction::None => {}
                    }
                }
            }
        }
        // A burst that has gone quiet is handed to the composer here, on
        // every beat, whether or not an event arrived.
        app.tick_paste_burst(std::time::Instant::now());

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

    // Exit output (`fullscreenExitOutput`): the driver prints this after it has
    // left the alternate screen, so the transcript stays in the terminal's
    // scrollback. Composed here because the App (the transcript) and the
    // settings (the mode) are both still in scope.
    //
    // The width mirrors the last frame's; a failed size query falls back to the
    // 80-column default the rest of the loop uses.
    let width = terminal.size().map(|area| area.width).unwrap_or(80);
    let transcript = match options.exit_output {
        FullscreenExitOutput::Transcript => app.transcript_text(width),
        FullscreenExitOutput::ResumeHint => String::new(),
    };
    let resume = resume_command(&options);
    let exit_output = exit_screen_output(
        options.exit_output,
        (!transcript.is_empty()).then_some(transcript.as_str()),
        resume.as_deref(),
    );

    Ok(RunOutcome {
        exit: InteractiveExit::UserExit,
        exit_output,
    })
}

#[cfg(test)]
mod tests {
    use super::is_fallback_sufficient_theme_error;

    #[test]
    fn theme_not_found_is_silent() {
        assert!(is_fallback_sufficient_theme_error(
            "Theme not found: upup-dark"
        ));
    }

    #[test]
    fn invalid_theme_name_is_silent() {
        assert!(is_fallback_sufficient_theme_error(
            "Invalid theme name: \"/etc/passwd\""
        ));
    }

    #[test]
    fn structural_theme_errors_still_flash() {
        // Missing colour tokens mean even the fallback palette is at risk;
        // the user genuinely needs to know.
        assert!(!is_fallback_sufficient_theme_error(
            "Invalid theme \"dark\": missing color token \"toolPendingBg\""
        ));
        assert!(!is_fallback_sufficient_theme_error(
            "Invalid hex color: #xyz"
        ));
        assert!(!is_fallback_sufficient_theme_error(
            "Variable reference not found: undefined-token"
        ));
    }
}

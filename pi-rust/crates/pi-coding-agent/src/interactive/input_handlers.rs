//! Input-event dispatchers + the helpers they lean on.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR3 of the M1 single-file
//! split; see `docs/API_STABILITY.md` and
//! `scripts/architecture_no_regression.sh`).
//!
//! Six functions cluster here because they share one job: take the next
//! thing the user did — a chord, a paste, an editor submission — and turn
//! it into either a driver-level action (`InternalAction::*`) or an
//! in-flight mutation of the [`App`]. They never touch the terminal
//! directly; the render loop owns that.
//!
//! `quote_if_needed` and `write_exit_output` are pure helpers used by the
//! command-dispatch layer (`run_slash_command`, `apply_clipboard_paste`
//! is the image/text paste chip path for `app.clipboard.pasteImage`).
//! `handle_submitted` / `handle_follow_up` / `handle_dequeue` route
//! prompt submissions — the three paths Enter, `app.message.followUp`
//! and `app.message.dequeue` collapse into one (`handle_submitted`).

use std::io::Write;
use std::sync::Arc;

use anyhow::Result as AnyhowResult;
use pi_agent_core::Agent;
use pi_protocol::ExtensionEvent;
use pi_tui::app::{App, FollowUpOutcome, Submission};
use tokio::sync::Mutex as AsyncMutex;

use super::{deliver_extension_event, run_slash_command, BashRunner, InteractiveOptions};

/// Upstream `quoteIfNeeded` (`interactive-mode.ts:264-269`): bare when every
/// character is safe for a POSIX shell, otherwise single-quoted with the
/// embedded-quote escape.
pub(super) fn quote_if_needed(value: &str) -> String {
    let safe = !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '~' | ':' | '@')
        });
    if safe {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Write the composed exit output, best effort.
///
/// Upstream writes it with `process.stdout.write` and does not let an I/O error
/// fail the exit (`interactive-mode.ts:3988`); a closed pipe (`pi | head`) must
/// not turn a quit into a crash.
pub(super) fn write_exit_output(text: &str) {
    if text.is_empty() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Apply one `app.clipboard.pasteImage` clipboard read to the composer.
///
/// An image becomes a chip; otherwise the paste falls back to plain text
/// (upstream `handleClipboardPaste`, `interactive-mode.ts:2933`: it saves the
/// image to a temp file and pastes the path, and inserts the text otherwise).
pub(super) fn apply_clipboard_paste(app: &mut App, paste: crate::clipboard::ClipboardPaste) {
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

/// Route submitted editor text the same way Enter does: prompt templates
/// and slash commands short-circuit before the agent, everything else
/// becomes a prompt. Shared by Enter and by an idle `app.message.followUp`
/// (upstream's `handleFollowUp` calls `editor.onSubmit` when no turn is
/// running, so both chords converge on the same handler).
pub(super) async fn handle_submitted(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
    submission: Submission,
) -> AnyhowResult<()> {
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
            app.flash_status(format!(
                "A bash command is already running. Press {} to cancel it first.",
                pi_tui::keybindings::key_text_or("app.interrupt", "Esc")
            ));
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
pub(super) async fn handle_follow_up(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
) -> AnyhowResult<()> {
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
pub(super) fn handle_dequeue(app: &mut App) {
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
//! Global keymap dispatcher.
//!
//! Migrated from `interactive/mod.rs` on 2026-09-26 (M1-9 of the single-file
//! split; see `docs/API_STABILITY.md` and
//! `scripts/architecture_no_regression.sh`).
//!
//! [`dispatch_global_chord_table`] is the central chord table the driver
//! consults after the App has had a chance to claim a key but before it
//! falls through to the App's own step path. It mirrors upstream's
//! `dispatchChord` (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2883-2903`)
//! — every entry binds one `app.*` id (with a default fallback chord) to
//! the helper that runs it, and returns `Some(InternalAction)` when it
//! consumed the event.
//!
//! Extension-installed shortcuts (P0-3, plan §5) are checked first via
//! [`dispatch_extension_shortcut`]; a registered shortcut always wins
//! over the built-in table because it lives on a layer the user can
//! manage (`ctx.ui.registerShortcut`), and the built-in `app.*` / `tui.*`
//! chords are reserved against extensions by the registry itself (see
//! `RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS`).
//!
//! The dispatcher is intentionally a pure dispatcher: each branch is a
//! one-liner that defers to the matching helper in `mod.rs`. That keeps
//! the chord table scannable in one file and lets the implementation
//! land in whichever module owns the state (the tree picker in
//! `session_picker.rs`, the bash runner in `bash_runner.rs`, and so on).

use pi_tui::app::App;
use pi_tui::input::InputEvent;

use super::{InternalAction, InteractiveOptions};
use crate::extensions::extension_shortcuts::ExtensionShortcutRegistry;
use crate::interactive::bash_runner::BashRunner;
use crate::interactive::CycleDirection;
use pi_agent_core::Agent;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// Look up an extension-installed shortcut for `event`.
///
/// The interactive driver calls this from
/// [`dispatch_global_chord_table`] *before* any built-in `app.*` / `tui.*`
/// chord, mirroring the precedence TS gives to extension shortcuts
/// (`runner.ts:71-90`). The returned callback is opaque to the TUI: it is
/// a JSON-encoded reference the JS shim knows how to resolve, so the
/// driver simply emits it via stdout in debug builds or hands it back
/// through the bridge on the production path. Returning `Some(_)` is
/// enough to claim the event; the dispatcher does not need to know what
/// the callback does.
pub(super) fn dispatch_extension_shortcut(
    registry: Option<&ExtensionShortcutRegistry>,
    event: &InputEvent,
) -> Option<()> {
    let registry = registry?;
    registry.lookup(event).map(|_| ())
}

/// Walk every global chord registered on the app keymap and run the
/// matching helper when one matches.
///
/// Returns `Some(InternalAction)` when a chord claimed the event (the
/// caller should NOT fall through to the App); returns `None` when no
/// chord matched (the caller proceeds to its normal App step).
///
/// The function exists so the chord table has one home even though the
/// helpers it dispatches to live in `mod.rs`; moving the table into its
/// own module is what made the M1 single-file split visible — the
/// previous 9442-line file had the table buried inside a 309-line
/// `handle_input_event`.
pub(super) async fn dispatch_global_chord_table(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    bash: &mut BashRunner,
    registry: Option<&ExtensionShortcutRegistry>,
    event: &InputEvent,
) -> Option<InternalAction> {
    // Mirror the same gating upstream applies to its global chord
    // table: an open modal/overlay owns the keyboard first, otherwise
    // the first letter of a search query or a settings field would be
    // read as a shortcut.
    if app.selector_open()
        || app.dialog_open()
        || app.settings_open()
        || app.custom_open()
        || app.search_open()
        // A reverse history search (`Ctrl+R`) owns the composer: codex keeps
        // its facade chords out for the whole session, otherwise the first
        // letter of a query could be read as a shortcut. The search's own
        // keys are routed by `App::step_key_at` before any of this.
        || app.history_search_active()
    {
        return None;
    }

    // Extension shortcuts (P0-3) take precedence over the built-in
    // chord table. The collision check at registration time ensures
    // none of these chords collide with a reserved key, so claiming
    // the event here cannot stomp a built-in.
    if dispatch_extension_shortcut(registry, event).is_some() {
        return Some(InternalAction::None);
    }

    let keybindings = pi_tui::keybindings::get_keybindings();
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.model.cycleForward",
        &["ctrl+p"],
    ) {
        super::cycle_model(app, agent, options, CycleDirection::Forward).await;
        return Some(InternalAction::None);
    }
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.model.cycleBackward",
        &["shift+ctrl+p"],
    ) {
        super::cycle_model(app, agent, options, CycleDirection::Backward).await;
        return Some(InternalAction::None);
    }
    // `app.model.select` (`Ctrl+L`) — upstream's chord for "Open model
    // selector" (`packages/coding-agent/src/core/keybindings.ts:116`).
    // This port used to hardcode `Ctrl+L` in `App::step_key` to clear the
    // transcript, which contradicted the header hint; `/clear` still does
    // the clearing (LUM-1245).
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.model.select",
        &["ctrl+l"],
    ) {
        super::open_model_selector(app, options);
        return Some(InternalAction::None);
    }
    // `app.thinking.cycle` (Shift+Tab) — the same switching path
    // `/thinking` and the selector take.
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.thinking.cycle",
        &["shift+tab"],
    ) {
        super::handle_thinking_cycle(app, agent, options.extensions.as_ref()).await;
        return Some(InternalAction::None);
    }
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.message.copy",
        &["ctrl+x"],
    ) {
        super::copy_last_assistant_message(app);
        return Some(InternalAction::None);
    }
    // `app.message.followUp`: queue the editor buffer behind the
    // in-flight turn (idle: behaves like Enter).
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.message.followUp",
        &["alt+enter"],
    ) {
        super::handle_follow_up(app, agent, options, bash).await.ok()?;
        return Some(InternalAction::None);
    }
    // `app.message.dequeue`: pull the queued prompts back into the
    // editor so they can be edited instead of waiting for the turn.
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.message.dequeue",
        &["alt+up"],
    ) {
        super::handle_dequeue(app);
        return Some(InternalAction::None);
    }
    // `app.session.new` — the same action `/new` runs. Bound to
    // `alt+n` by the merged table (upstream leaves it unbound).
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.session.new",
        &["alt+n"],
    ) {
        super::start_new_session(app, agent, options).await;
        return Some(InternalAction::None);
    }
    // `app.session.tree` — the same overlay `/tree` opens.
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.session.tree",
        &["alt+t"],
    ) {
        super::open_tree_selector(app, options);
        return Some(InternalAction::None);
    }
    // `app.session.fork` — the same picker `/fork` opens.
    //
    // No builtin chord: `alt+f` belongs to `tui.editor.cursorWordRight`
    // (upstream, codex `move_word_right`, Martty `WordRight`) and this
    // global path runs before the composer, so binding it here silently
    // removed forward-word from the TUI (LUM-1360). Upstream leaves
    // `app.session.fork` unbound; `/fork` is the documented entry point,
    // and a user binding in `keybindings.json` still takes effect through
    // `matches_with_fallback`.
    if pi_tui::keybindings::matches_with_fallback(&keybindings, event, "app.session.fork", &[]) {
        super::open_fork_selector(app, options);
        return Some(InternalAction::None);
    }
    // `app.session.resume` — deliberately the *same* code path as
    // `/resume` (`open_resume_selector`), never a second copy.
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.session.resume",
        &["alt+r"],
    ) {
        super::open_resume_selector(app, options);
        return Some(InternalAction::None);
    }
    // `app.editor.external` (`Ctrl+G`) — upstream hands the draft to
    // `$EDITOR` (`interactive-mode.ts:4246-4262`). The driver runs the
    // editor because only it owns the terminal; nothing is claimed here
    // beyond the resolution of which command to run.
    if pi_tui::keybindings::matches_with_fallback(
        &keybindings,
        event,
        "app.editor.external",
        &["ctrl+g"],
    ) {
        return Some(InternalAction::ExternalEditor {
            command: crate::config::load_external_editor_command(&super::settings_sources()),
        });
    }
    // `app.suspend` (`Ctrl+Z`) — upstream `process.kill(0, "SIGTSTP")`
    // after stopping the TUI (`interactive-mode.ts:3550-3600`). Unbound on
    // Windows by the merged table, so that platform can never reach this
    // branch; the guard keeps the meaning explicit if a user binds it.
    if !cfg!(windows)
        && pi_tui::keybindings::matches_with_fallback(
            &keybindings,
            event,
            "app.suspend",
            &["ctrl+z"],
        )
    {
        return Some(InternalAction::Suspend);
    }

    None
}

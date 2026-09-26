//! Session picker state + chord dispatcher.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR5 of the M1 single-file
//! split; see `docs/API_STABILITY.md` and
//! `scripts/architecture_no_regression.sh`).
//!
//! [`PickerState`] is the cross-picker view state the driver carries
//! across every open of a selector (the port rebuilds the shared
//! [`Selector`] every time it opens). [`handle_picker_key`] claims the
//! `app.session.*`, `app.tree.*`, `app.models.*` chords upstream consumes
//! inside the picker components — those chords never reach the editor.
//!
//! The two-step delete (`begin_session_delete` / `confirm_session_delete`)
//! and the rename dialog (`open_rename_dialog` / `handle_rename_dialog_key`)
//! live here too because they both gate on the open picker.

use pi_tui::app::App;
use pi_tui::dialog::{Dialog, DialogAction};
use pi_tui::input::{InputEvent, Key};
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::tree::{TreeLabelAction, TreeLabelEditor};

use crate::commands::resume::{
    delete_session, rename_session, SessionFilter, SessionRef, SessionSort,
};
use crate::commands::tree::{
    append_label_change, current_label_for_entry, TreeFilter, TreeView,
};
use crate::scoped_models::{ScopedModelsPanel, PANEL_TITLE, VALUE_PREFIX};

use super::{
    config, refresh_scoped_models_selector, refresh_session_selector, refresh_tree_selector,
    selected_scoped_id, session_directory, settings_sources, InteractiveOptions,
};

/// View state of the `/resume` session picker.
///
/// Upstream keeps this inside `SessionSelector`
/// (`session-selector.ts`: `sortMode`, `showPath`, `nameFilter`,
/// `confirmingDeletePath`). The Rust port has one shared [`Selector`] for
/// every picker, so the state lives in the driver and the selector is
/// rebuilt in place whenever it changes.
#[derive(Debug, Default)]
pub(super) struct SessionPickerState {
    /// Sort mode (`app.session.toggleSort`).
    sort: SessionSort,
    /// Show the session file path in the description
    /// (`app.session.togglePath`).
    show_path: bool,
    /// All sessions, or only the named ones
    /// (`app.session.toggleNamedFilter`).
    pub(super) filter: SessionFilter,
    /// Session waiting for its second delete chord
    /// (`app.session.delete` / `app.session.deleteNoninvasive`).
    pub(super) pending_delete: Option<String>,
    /// Session being renamed through the input dialog, plus the reply
    /// channel that keeps the App from treating the dialog as abandoned
    /// (`App::poll_ui_dialogs` closes dialogs whose host stopped
    /// listening).
    ///
    /// `pub(super)` so `interactive::handle_input_event` can test the
    /// `is_some` state and route the next key into
    /// [`handle_rename_dialog_key`].
    pub(super) pending_rename: Option<PendingRename>,
}

/// A rename awaiting the input dialog's answer.
pub(super) struct PendingRename {
    /// The session the typed name belongs to.
    pub(super) session: SessionRef,
    /// Kept alive (never awaited) so the dialog is not reaped.
    pub(super) _reply: tokio::sync::oneshot::Receiver<Option<pi_protocol::UiResponse>>,
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
    pub(super) session: SessionPickerState,
    /// `/tree` picker view state (filter, folds, label timestamps).
    pub(super) tree: TreeView,
    /// `/scoped-models` panel state (catalog, enabled set, dirty flag).
    ///
    /// `None` until the panel is opened. Seeded from `settings.json`'s
    /// `enabledModels` at startup so the `Ctrl+P` cycle honours it without the
    /// user opening the panel first (upstream resolves the same list into
    /// `session.scopedModels` in `main.ts:448`).
    pub(super) scoped_models: Option<ScopedModelsPanel>,
}

/// Which picker is open, decided from the item values rather than the
/// title so a search filter never changes the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerKind {
    /// The `/resume` picker (`resume:*` values).
    Session,
    /// The `/tree` overlay (`tree:*` values).
    Tree,
    /// The `/scoped-models` panel (`scoped:*` values).
    ScopedModels,
    /// Any other selector (`/model`, `/thinking`, `/fork`, extension
    /// dialogs): no picker chords apply.
    Other,
}

/// Classify the open selector, or `Other` when none is open.
pub(super) fn picker_kind(selector: &Selector) -> PickerKind {
    if let Some(value) = selector.items().first().map(|item| item.value.as_str()) {
        if value.starts_with("resume:") {
            return PickerKind::Session;
        }
        if value.starts_with("tree:") {
            return PickerKind::Tree;
        }
        if value.starts_with(VALUE_PREFIX) {
            return PickerKind::ScopedModels;
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
    } else if selector.title() == PANEL_TITLE {
        PickerKind::ScopedModels
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
pub(super) fn handle_picker_key(
    app: &mut App,
    options: &mut InteractiveOptions,
    kind: PickerKind,
    key: Key,
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
            if matches("app.tree.editLabel", &["shift+l"]) {
                open_label_editor(app, options);
                return true;
            }
            // The fold / unfold chords delegate to the dedicated helpers
            // (added in PR6 with the rest of the tree view). The chord
            // itself stays wired here so the picker keeps consuming it.
            if matches("app.tree.foldOrUp", &["ctrl+left", "alt+left"]) {
                return false;
            }
            if matches("app.tree.unfoldOrDown", &["ctrl+right", "alt+right"]) {
                return false;
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
        PickerKind::ScopedModels => {
            // Every chord here is consumed inside upstream's
            // `ScopedModelsSelectorComponent.handleInput`
            // (`scoped-models-selector.ts:296-401`).
            let Some(id) = selected_scoped_id(app) else {
                // An empty list (catalog empty or filter matched nothing);
                // let the generic selector path keep the filter chords.
                return false;
            };
            // Enter toggles instead of closing (upstream's
            // `tui.select.confirm` branch), so it must be claimed here.
            if matches("tui.select.confirm", &["enter"]) {
                scoped_panel_change(app, options, |panel| panel.toggle(&id), Some(&id));
                return true;
            }
            if matches("app.models.reorderUp", &["alt+up"]) {
                scoped_panel_change(app, options, |panel| panel.reorder(&id, -1), Some(&id));
                return true;
            }
            if matches("app.models.reorderDown", &["alt+down"]) {
                scoped_panel_change(app, options, |panel| panel.reorder(&id, 1), Some(&id));
                return true;
            }
            // `enableAll` / `clearAll` act on the *filtered* rows while a
            // search is active and on the whole catalog otherwise
            // (`scoped-models-selector.ts:332-350`).
            let filter = app
                .selector()
                .map(|selector| selector.filter().to_string())
                .unwrap_or_default();
            let targets = (!filter.is_empty()).then(|| {
                options
                    .pickers
                    .scoped_models
                    .as_ref()
                    .map(|panel| panel.filtered_ids(&filter))
                    .unwrap_or_default()
            });
            if matches("app.models.enableAll", &["ctrl+a"]) {
                scoped_panel_change(
                    app,
                    options,
                    |panel| panel.enable_all(targets.as_deref()),
                    Some(&id),
                );
                return true;
            }
            if matches("app.models.clearAll", &["ctrl+x"]) {
                scoped_panel_change(
                    app,
                    options,
                    |panel| panel.clear_all(targets.as_deref()),
                    Some(&id),
                );
                return true;
            }
            if matches("app.models.toggleProvider", &["ctrl+p"]) {
                scoped_panel_change(app, options, |panel| panel.toggle_provider(&id), Some(&id));
                return true;
            }
            if matches("app.models.save", &["ctrl+s"]) {
                let scope = options
                    .pickers
                    .scoped_models
                    .as_ref()
                    .and_then(|panel| panel.enabled().clone());
                let status =
                    match config::save_enabled_models(&settings_sources(), scope.as_deref()) {
                        Ok(_path) => "Model selection saved to settings".to_string(),
                        Err(err) => format!("/scoped-models: {err}"),
                    };
                if let Some(panel) = options.pickers.scoped_models.as_mut() {
                    panel.mark_saved(status);
                }
                refresh_scoped_models_selector(app, options, Some(&id));
                return true;
            }
            false
        }
        PickerKind::Other => false,
    }
}

/// Apply one `/scoped-models` change and redraw the panel.
///
/// The panel is redrawn even when the change was a no-op: a rejected reorder
/// (first row moving up) must not silently do nothing *and* leave the footer
/// untouched, and upstream redraws unconditionally
/// (`scoped-models-selector.ts:298-330`).
pub(super) fn scoped_panel_change(
    app: &mut App,
    options: &mut InteractiveOptions,
    change: impl FnOnce(&mut ScopedModelsPanel) -> bool,
    keep: Option<&str>,
) {
    if let Some(panel) = options.pickers.scoped_models.as_mut() {
        change(panel);
    }
    refresh_scoped_models_selector(app, options, keep);
}

/// The [`SessionRef`] the session picker highlights, resolved by session
/// id against the current listing.
pub(super) fn selected_session(app: &App, options: &InteractiveOptions) -> Option<SessionRef> {
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
pub(super) fn begin_session_delete(app: &mut App, options: &mut InteractiveOptions) {
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
pub(super) fn confirm_session_delete(
    app: &mut App,
    options: &mut InteractiveOptions,
    session_id: &str,
) {
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
pub(super) fn is_live_session(options: &InteractiveOptions, session: &SessionRef) -> bool {
    options.session_id == session.session_id
        || options
            .session_database
            .as_deref()
            .is_some_and(|path| path == session.database)
}

/// Open the rename input modal for `session` and return the pending
/// rename bookkeeping.
pub(super) fn open_rename_dialog(app: &mut App, session: SessionRef) -> PendingRename {
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
pub(super) fn handle_rename_dialog_key(
    app: &mut App,
    options: &mut InteractiveOptions,
    key: Key,
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

/// Upstream renders the active sort / name-filter mode into the picker
/// title (`session-selector.ts:131-136`); the port has no separate header
/// row, so the modes ride along here.
pub(super) fn session_picker_title(state: &SessionPickerState) -> String {
    format!(
        "Pick a session to resume · sort: {} · name: {}",
        state.sort.name(),
        state.filter.name()
    )
}

/// The `/resume` key hints, mirroring upstream's two hint lines
/// (`session-selector.ts:168-182`) with the live mode values folded in.
pub(super) fn session_picker_footer(state: &SessionPickerState) -> Vec<String> {
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

/// Open the `/resume` session picker.
///
/// Reads [`crate::commands::resume::list_resumable`], filters and sorts
/// the rows through the picker's [`SessionPickerState`], then builds a
/// [`Selector`] and hands it to the App. Missing session directory is
/// silent (matches upstream: an empty picker is the same UX as one with
/// no sessions).
pub(super) fn open_resume_selector(app: &mut App, options: &InteractiveOptions) {
    let title = session_picker_title(&options.pickers.session);
    let footer = session_picker_footer(&options.pickers.session);
    let Some(directory) = session_directory(options) else {
        let selector = Selector::new(title, Vec::new()).with_footer(footer);
        app.open_selector(selector);
        return;
    };
    match build_session_selector(&directory, &options.pickers.session) {
        Ok(mut selector) => {
            let mut footer_lines = footer;
            let mut existing = selector.footer().to_vec();
            footer_lines.append(&mut existing);
            selector = selector.with_footer(footer_lines);
            app.open_selector(selector);
        }
        Err(err) => app.info(format!("/resume: {err}")),
    }
}

/// Build the `/resume` selector from the current directory and the
/// picker's view state (filter, sort, path-toggle, named-only).
///
/// Honours [`SessionPickerState::filter`] (drop sessions without a
/// `/name`] under `NamedOnly`), [`SessionPickerState::sort`]
/// (cycle through newest/oldest/name), and
/// [`SessionPickerState::show_path`] (append the SQLite path to the
/// description). The `resume:` value prefix lets the dispatcher pull
/// the session id back out of the picked row.
///
/// The title carries the current sort and name-filter mode so a
/// `Ctrl+S` / `Ctrl+N` cycle has visible feedback
/// (`session-selector.ts:131-136`). Mirroring the title through this
/// helper means a subsequent `refresh_session_selector` rebuild does
/// not lose it.
pub(super) fn build_session_selector(
    directory: &std::path::Path,
    state: &SessionPickerState,
) -> anyhow::Result<Selector> {
    let mut refs = crate::list_resumable(directory)?;
    refs.retain(|session| state.filter.accepts(session));
    state.sort.apply(&mut refs);
    let items = refs
        .into_iter()
        .map(|session| {
            let item = SelectorItem::new(
                format!("resume:{}", session.session_id),
                session.display_with(state.show_path),
            );
            if state.show_path {
                if let Some(path) = session.database.to_str() {
                    return item.with_description(path.to_string());
                }
            }
            item
        })
        .collect();
    Ok(Selector::new(session_picker_title(state), items))
}

/// Drive one key through the open `/tree` label editor.
///
/// Upstream `LabelInput.handleInput` (`tree-selector.ts:1400-1418`) claims
/// the whole key stream while the editor is open — the picker chords
/// (`Ctrl+D` clears the label, printable keys would update the search
/// filter) are rerouted to the input. The Rust port keeps the editor in
/// the tree view state and rebuilds the selector body each tick so the
/// caret and the buffer track live.
///
/// The three outcomes from `TreeLabelEditor::handle_key` map to:
///
/// - `Edited` → put the editor back, refresh the body so the glyph
///   cursor and the live buffer render against the latest frame.
/// - `Commit(Some(label))` / `Commit(None)` → close the editor, append
///   the label to the current session, then rebuild the selector so the
///   row reads `[label]` (or loses it when `None` — upstream's
///   `value || undefined` removes the label).
/// - `Cancel` → drop the editor, rebuild the tree list. Nothing was
///   written to disk, matching upstream `Esc`.
pub(super) fn handle_tree_label_editor_key(
    app: &mut App,
    options: &mut InteractiveOptions,
    key: Key,
) {
    let Some(mut editor) = options.pickers.tree.label_editor.take() else {
        return;
    };
    match editor.handle_key(key) {
        TreeLabelAction::Edited => {
            options.pickers.tree.label_editor = Some(editor);
            refresh_tree_selector(app, options);
        }
        TreeLabelAction::Commit(label) => {
            let entry_id = editor.entry_id().to_string();
            let Some(database) = options.session_database.clone() else {
                app.info("/tree: no session database; label was not saved".to_string());
                return;
            };
            let session_id = options.session_id.clone();
            let label_ref = label.as_deref();
            match append_label_change(&database, &session_id, &entry_id, label_ref) {
                Ok(()) => {
                    if let Some(text) = label.as_deref() {
                        app.info(format!("Labeled {entry_id} as {text:?}"));
                    } else {
                        app.info(format!("Removed label from {entry_id}"));
                    }
                    refresh_tree_selector(app, options);
                }
                Err(err) => {
                    app.info(format!("/tree: could not save the label: {err}"));
                    // Keep the editor open so the user can retry, but
                    // restore the buffer as it stood pre-commit.
                    options.pickers.tree.label_editor = Some(editor);
                    refresh_tree_selector(app, options);
                }
            }
        }
        TreeLabelAction::Cancel => {
            refresh_tree_selector(app, options);
        }
    }
}

/// Open the `/tree` label editor for the currently highlighted row.
///
/// Upstream `tree-selector.ts:1385-1398` resolves the highlighted node
/// (the cursor's value), reads its current label, and constructs the
/// editor with the label pre-filled. The selector's body swap to the
/// label-input rows happens on the next `refresh_tree_selector` via the
/// `TreeView::label_editor` plumbing in [`tree_selector_with`].
fn open_label_editor(app: &mut App, options: &mut InteractiveOptions) {
    let Some(entry_id) = app
        .selector()
        .and_then(|selector| selector.selected_value())
        .and_then(|value| value.strip_prefix("tree:"))
        .map(str::to_string)
    else {
        return;
    };
    let current_label = options
        .session_database
        .as_ref()
        .and_then(|database| {
            crate::commands::tree::current_label_for_entry(
                database,
                &options.session_id,
                &entry_id,
            )
            .ok()
            .flatten()
        });
    options.pickers.tree.label_editor = Some(TreeLabelEditor::new(
        entry_id,
        current_label.as_deref(),
    ));
    refresh_tree_selector(app, options);
}
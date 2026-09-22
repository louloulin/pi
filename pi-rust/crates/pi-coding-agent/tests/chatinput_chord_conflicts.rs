//! Chord-conflict guard for the composer (`LUM-1360`).
//!
//! The `app.*` table and the `tui.editor.*` / `tui.input.*` table are merged
//! into one registry, and the coding-agent's global input path
//! (`interactive.rs::handle_input_event`) consumes its `app.*` actions
//! *before* the composer ever sees the key. So an `app.*` default chord that
//! the editor table also claims does not merely "also do something" — it
//! silently removes the editor action.
//!
//! That is exactly what happened with `alt+f`: `tui.editor.cursorWordRight`
//! has been bound to `alt+f` since the Stage 10 editor port (it is also codex's
//! `move_word_right` and Martty's `KeyCode::Char('f') if alt => WordRight`),
//! but Stage 65 handed `alt+f` to `app.session.fork` on the stated premise
//! that the port's invented `alt+*` session chords "are free across all
//! platform tables". Measured in a real PTY, that premise was false: `Alt+F`
//! forked the session instead of moving the caret one word right, so the
//! forward-word chord every reference TUI binds was dead in the composer.
//!
//! These tests keep the invariant that made that possible from regressing:
//! every `app.*`/editor chord overlap must be one of the *deliberate,
//! documented* cases in [`ALLOWED_OVERLAPS`], each of which is separated at
//! runtime by an explicit condition (an open overlay, a selection, or an empty
//! draft). A new overlap fails the test instead of shipping.

use pi_coding_agent::keybindings::{merged_definitions, Env, Platform};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{KeybindingsConfig, KeybindingsManager};

/// The chords a definition claims, as key ids.
fn chords(
    definitions: &[(String, pi_tui::keybindings::KeybindingDefinition)],
    id: &str,
) -> Vec<String> {
    definitions
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, definition)| definition.default_keys.clone())
        .unwrap_or_else(|| panic!("{id} is part of the merged table"))
}

/// Overlaps that are deliberate. Each entry is `(app id, chord, why)`.
///
/// The `why` is load-bearing: an entry is only legitimate when something in
/// the app *disambiguates* the two meanings. `overlay-scoped` means the action
/// is only consumed while its own picker/overlay has focus
/// (`handle_picker_key` / the overlay guards), so the composer still gets the
/// chord in the state a user types in. `dual-use` means one handler inspects
/// the editor state (a selection, an empty draft) before choosing.
const ALLOWED_OVERLAPS: &[(&str, &str, &str)] = &[
    // dual-use: `tui.input.copy` copies when a selection exists, `app.clear`
    // clears the draft (twice to exit) when it does not.
    (
        "app.clear",
        "ctrl+c",
        "dual-use: copy with a selection, else clear",
    ),
    // dual-use: `app.exit` only fires when the draft is empty; otherwise the
    // chord reaches `tui.editor.deleteCharForward`.
    (
        "app.exit",
        "ctrl+d",
        "dual-use: exit on an empty draft, else delete forward",
    ),
    // overlay-scoped: only consumed while the matching picker is open.
    (
        "app.models.enableAll",
        "ctrl+a",
        "overlay-scoped: /scoped-models",
    ),
    (
        "app.models.save",
        "ctrl+s",
        "overlay-scoped: /scoped-models",
    ),
    (
        "app.session.delete",
        "ctrl+d",
        "overlay-scoped: /resume picker",
    ),
    (
        "app.session.rename",
        "ctrl+r",
        "overlay-scoped: /resume picker",
    ),
    (
        "app.session.toggleSort",
        "ctrl+s",
        "overlay-scoped: /resume picker",
    ),
    (
        "app.thinking.save",
        "ctrl+s",
        "overlay-scoped: /thinking picker",
    ),
    (
        "app.tree.filter.all",
        "ctrl+a",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.filter.default",
        "ctrl+d",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.filter.userOnly",
        "ctrl+u",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.foldOrUp",
        "alt+left",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.foldOrUp",
        "ctrl+left",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.unfoldOrDown",
        "alt+right",
        "overlay-scoped: /tree picker",
    ),
    (
        "app.tree.unfoldOrDown",
        "ctrl+right",
        "overlay-scoped: /tree picker",
    ),
];

/// A key event for `chord`-style ids used by the resolution assertions.
fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::Key(Key::new(code, modifiers))
}

#[test]
fn no_app_default_shadows_an_editor_chord_unless_allowlisted() {
    let definitions = merged_definitions(&Platform::Linux, &Env::new());
    let editor_chords: Vec<(String, String)> = definitions
        .iter()
        .filter(|(id, _)| id.starts_with("tui.editor.") || id.starts_with("tui.input."))
        .flat_map(|(id, definition)| {
            definition
                .default_keys
                .iter()
                .map(move |chord| (id.clone(), chord.clone()))
        })
        .collect();

    let mut unexpected = Vec::new();
    for (id, definition) in definitions.iter().filter(|(id, _)| id.starts_with("app.")) {
        for chord in &definition.default_keys {
            let shadowed: Vec<&str> = editor_chords
                .iter()
                .filter(|(_, editor_chord)| editor_chord == chord)
                .map(|(editor_id, _)| editor_id.as_str())
                .collect();
            if shadowed.is_empty() {
                continue;
            }
            let allowed = ALLOWED_OVERLAPS
                .iter()
                .any(|(allowed_id, allowed_chord, _)| allowed_id == id && allowed_chord == chord);
            if !allowed {
                unexpected.push(format!(
                    "{id} ({chord}) shadows {shadowed:?} — bind a free chord, or add a documented \
                     ALLOWED_OVERLAPS entry explaining how the two meanings are separated"
                ));
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "app.* default chords silently remove composer actions:\n  {}",
        unexpected.join("\n  ")
    );
}

#[test]
fn the_allowlist_names_ids_that_exist_and_actually_overlap() {
    // A stale allowlist is worse than none: it would pre-authorise an overlap
    // that no longer (or never did) exist, hiding a future regression behind a
    // comment about code that has moved on.
    let definitions = merged_definitions(&Platform::Linux, &Env::new());
    let editor_chords: Vec<String> = definitions
        .iter()
        .filter(|(id, _)| id.starts_with("tui.editor.") || id.starts_with("tui.input."))
        .flat_map(|(_, definition)| definition.default_keys.clone())
        .collect();

    for (id, chord, why) in ALLOWED_OVERLAPS {
        assert!(
            !why.trim().is_empty(),
            "ALLOWED_OVERLAPS entry {id}/{chord} must carry a justification"
        );
        assert!(
            editor_chords
                .iter()
                .any(|editor_chord| editor_chord == chord),
            "ALLOWED_OVERLAPS entry {id}/{chord} no longer overlaps anything"
        );
    }

    // macOS is the other side of the two platform forks that touch these
    // tables (`app.tree.*`, `app.session.fork`); the guard must hold there too.
    let darwin = merged_definitions(&Platform::Darwin, &Env::new());
    let darwin_editor_chords: Vec<String> = darwin
        .iter()
        .filter(|(id, _)| id.starts_with("tui.editor.") || id.starts_with("tui.input."))
        .flat_map(|(_, definition)| definition.default_keys.clone())
        .collect();
    for (id, definition) in darwin.iter().filter(|(id, _)| id.starts_with("app.")) {
        for chord in &definition.default_keys {
            if !darwin_editor_chords
                .iter()
                .any(|editor_chord| editor_chord == chord)
            {
                continue;
            }
            assert!(
                ALLOWED_OVERLAPS.iter().any(
                    |(allowed_id, allowed_chord, _)| allowed_id == id && allowed_chord == chord
                ),
                "{id} ({chord}) shadows a darwin editor chord without an allowlist entry"
            );
        }
    }
}

#[test]
fn alt_f_reaches_the_composer_as_forward_word() {
    // The concrete regression: `app.session.fork` is unbound by default
    // (upstream `coding-agent/src/core/keybindings.ts` declares
    // `defaultKeys: []`), so `Alt+F` keeps its editor meaning.
    let definitions = merged_definitions(&Platform::Linux, &Env::new());
    assert!(
        chords(&definitions, "app.session.fork").is_empty(),
        "app.session.fork must stay unbound so it cannot shadow a composer chord; \
         `/fork` is the documented path"
    );

    let manager = KeybindingsManager::new(definitions, KeybindingsConfig::new());
    let alt_f = key(KeyCode::Char('f'), KeyModifiers::ALT);
    assert!(
        manager.matches(&alt_f, "tui.editor.cursorWordRight"),
        "Alt+F must be the editor's forward-word chord (codex move_word_right, \
         Martty WordRight, upstream cursorWordRight)"
    );
    assert!(
        !manager.matches(&alt_f, "app.session.fork"),
        "Alt+F must not fork the session"
    );

    // The backward twin was never contested; assert it stays that way so a
    // future `alt+b` shortcut cannot repeat the `alt+f` mistake silently.
    let alt_b = key(KeyCode::Char('b'), KeyModifiers::ALT);
    assert!(manager.matches(&alt_b, "tui.editor.cursorWordLeft"));
}

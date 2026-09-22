//! LUM-1464 — the `?` shortcut overlay.
//!
//! The App's status bar has advertised `? for help` since the port's first
//! footer, but nothing in the port ever handled `?` — the hint was a false
//! advertisement of exactly the class LUM-1240/LUM-1245 retired for `app.*`
//! ids. codex has the real thing (`ChatComposer::handle_shortcut_overlay_key`,
//! `bottom_pane/chat_composer.rs:3149`, rendered by
//! `bottom_pane/footer.rs::shortcut_overlay_lines`), and Martty binds its
//! equivalent to `Ctrl+K` on an empty prompt (`src/input/keymap.rs:97`).
//!
//! This file pins the *behaviour*: the toggle gate, the "any other key closes
//! and is still handled" rule, and the four keyboard layers the overlay must
//! not steal from. The painted geometry is pinned by
//! `tests/lum1464_shortcut_overlay_frames.rs`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::component::TextComponent;
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::reset_keybindings;
use pi_tui::selector::{Selector, SelectorItem};

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 4096,
        max_output_tokens: 512,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "lum1464-shortcut-overlay".into(),
            ..AppConfig::default()
        },
    )
}

fn key(code: KeyCode) -> InputEvent {
    InputEvent::Key(Key::new(code, KeyModifiers::NONE))
}

fn chord(c: char) -> InputEvent {
    InputEvent::Key(Key::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn question() -> InputEvent {
    key(KeyCode::Char('?'))
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.step(key(KeyCode::Char(c)));
    }
}

#[test]
fn question_mark_on_an_empty_composer_opens_the_overlay_without_typing() {
    reset_keybindings();
    let mut app = app();
    let outcome = app.step(question());
    assert!(
        matches!(outcome, StepOutcome::Redraw),
        "opening the overlay repaints, got {outcome:?}"
    );
    assert!(app.shortcut_overlay_open());
    assert!(
        app.prompt().is_empty(),
        "`?` is a chord here, not a character the user typed"
    );
}

#[test]
fn question_mark_after_a_draft_is_literal_text() {
    reset_keybindings();
    let mut app = app();
    type_text(&mut app, "why");
    app.step(question());
    assert!(!app.shortcut_overlay_open(), "a draft means `?` is text");
    assert_eq!(app.prompt().text(), "why?");
}

#[test]
fn question_mark_again_closes_the_overlay() {
    reset_keybindings();
    let mut app = app();
    app.step(question());
    assert!(app.shortcut_overlay_open());
    app.step(question());
    assert!(!app.shortcut_overlay_open());
    assert!(app.prompt().is_empty(), "toggling never typed a `?`");
}

#[test]
fn escape_closes_the_overlay_without_clearing_the_composer() {
    reset_keybindings();
    let mut app = app();
    app.step(question());
    let outcome = app.step(key(KeyCode::Esc));
    assert!(matches!(outcome, StepOutcome::Redraw));
    assert!(!app.shortcut_overlay_open());
    assert!(app.prompt().is_empty());
    // Esc while the overlay is open is the overlay's close key — it must not
    // reach `app.clear` / `app.interrupt`.
    assert!(!app.is_exit_requested());
}

#[test]
fn another_key_closes_the_overlay_and_is_still_handled() {
    reset_keybindings();
    let mut app = app();
    app.step(question());
    app.step(key(KeyCode::Char('x')));
    assert!(
        !app.shortcut_overlay_open(),
        "typing closes help (codex's reset_mode_after_activity)"
    );
    assert_eq!(
        app.prompt().text(),
        "x",
        "the key that closed the overlay was not swallowed"
    );
}

#[test]
fn a_global_chord_closes_the_overlay_and_still_fires() {
    reset_keybindings();
    let mut app = app();
    app.step(question());
    let before = app.tools_expanded();
    app.step(chord('o')); // `app.tools.expand`
    assert!(!app.shortcut_overlay_open());
    assert_eq!(
        app.tools_expanded(),
        !before,
        "Ctrl+O had to reach the tool-expansion handler"
    );
}

#[test]
fn question_mark_does_not_open_over_a_modal() {
    reset_keybindings();
    let mut app = app();
    app.open_selector(Selector::new(
        "Pick a model",
        vec![SelectorItem::new("model:faux-a", "faux-a")],
    ));
    app.step(question());
    assert!(
        !app.shortcut_overlay_open(),
        "the modal owns the keyboard, including `?`"
    );
    assert!(app.selector_open(), "and the modal is still open");
}

#[test]
fn question_mark_goes_to_the_history_search_query() {
    reset_keybindings();
    let mut app = app();
    app.step(chord('r')); // `tui.editor.historySearch`
    assert!(app.history_search_active());
    app.step(question());
    assert!(
        !app.shortcut_overlay_open(),
        "the reverse search owns the keyboard"
    );
    assert_eq!(
        app.prompt().editor().history_search_query(),
        Some("?"),
        "`?` is a search query character, not a chord, mid-search"
    );
}

#[test]
fn question_mark_does_not_open_over_the_transcript_search_overlay() {
    reset_keybindings();
    let mut app = app();
    assert!(app.open_search(), "the transcript search bar is open");
    app.step(question());
    assert!(
        !app.shortcut_overlay_open(),
        "the search overlay owns the keyboard"
    );
}

#[test]
fn an_attachment_also_makes_question_mark_text() {
    reset_keybindings();
    let mut app = app();
    app.paste_image(pi_protocol::ImageContent {
        data: "aGk=".into(),
        mime_type: "image/png".into(),
    });
    app.step(question());
    assert!(
        !app.shortcut_overlay_open(),
        "an attached image is content, so the empty-composer gate must fail"
    );
}

#[test]
fn a_custom_editor_component_keeps_the_keyboard() {
    reset_keybindings();
    let mut app = app();
    // `ctx.ui.setEditorComponent` replaces the composer; the App's own help
    // must not open over an extension-owned editor region.
    app.set_editor_component(Some(Box::new(TextComponent::new(["extension editor"]))));
    assert!(app.has_editor_component());
    app.step(question());
    assert!(
        !app.shortcut_overlay_open(),
        "the extension editor owns the input surface"
    );
}

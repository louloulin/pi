//! Integration coverage for editor undo (`tui.editor.undo`, `Ctrl+-`).
//!
//! These tests drive the public component surface — [`Editor`] through
//! [`InputEvent`]s, plus the [`Prompt`] submit lifecycle — rather than
//! reaching into the snapshot type, so they cover the same path the
//! interactive mode uses:
//!
//! 1. A word typed character-by-character coalesces into a single undo
//!    unit, while each space remains separately undoable.
//! 2. A bulk insert (the path a bracketed paste takes) undoes atomically.
//! 3. Kill / yank / yank-pop are all undoable, in reverse order.
//! 4. Entering history browsing is undoable, restoring the draft.
//! 5. Submitting clears the stack, so undo cannot resurrect a sent prompt.
//! 6. `Ctrl+-` survives the `crossterm` → [`InputEvent`] conversion.

use pi_tui::backend::event::{
    KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyModifiers as CtModifiers,
};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, Key, Prompt, PromptAction};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn ctrl(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn alt(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::ALT)
}

fn plain(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

/// Feed a string one character at a time, like a terminal would.
fn type_text(ed: &mut Editor, text: &str) {
    for c in text.chars() {
        assert_eq!(ed.handle_event(plain(c)), EditorAction::Changed);
    }
}

#[test]
fn typed_words_coalesce_and_spaces_undo_one_at_a_time() {
    let mut ed = Editor::new();
    type_text(&mut ed, "hello world");
    assert_eq!(ed.text(), "hello world");

    // Undo #1: the space plus the word after it.
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed,);
    assert_eq!(ed.text(), "hello");

    // Undo #2: the first word.
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed,);
    assert_eq!(ed.text(), "");

    // Nothing left to undo.
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::None);
}

#[test]
fn bulk_insert_undoes_atomically() {
    let mut ed = Editor::new();
    ed.insert_str("hello world");
    ed.move_home();
    for _ in 0..5 {
        ed.move_right();
    }

    // This is the "bracketed paste" shape: one insert of many
    // characters in the middle of the buffer.
    assert_eq!(ed.insert_str("beep boop"), EditorAction::Changed);
    assert_eq!(ed.text(), "hellobeep boop world");

    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "hello world");
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn kill_yank_and_yank_pop_undo_in_reverse_order() {
    let mut ed = Editor::new();
    ed.insert_str("first");
    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    ed.insert_str("second");
    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    assert_eq!(ed.kill_ring_len(), 2);

    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "second");
    assert_eq!(ed.handle_event(alt('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "first");

    // yank-pop itself was undoable …
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "second");
    // … then the yank …
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "");
    // … then the kill that produced the ring entry.
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "second");
}

#[test]
fn history_browsing_undoes_back_to_the_draft() {
    let mut ed = Editor::new();
    ed.push_history("older");
    type_text(&mut ed, "draft");
    assert_eq!(ed.text(), "draft");

    assert_eq!(
        ed.handle_event(key(KeyCode::Up, KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "older");

    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "draft");
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn submitting_clears_the_undo_stack() {
    let mut prompt = Prompt::new("> ");
    prompt.editor_mut().insert_str("sent prompt");
    assert_eq!(
        prompt.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
        PromptAction::Submit("sent prompt".to_string()),
    );

    // The interactive mode pushes to history and clears the buffer after
    // a submit; undo must not bring the prompt back.
    prompt.push_history("sent prompt");
    prompt.clear();
    assert_eq!(prompt.editor_mut().undo_len(), 0);
    assert_eq!(
        prompt.handle_key(Key::new(KeyCode::Char('-'), KeyModifiers::CONTROL)),
        PromptAction::None,
    );
    assert!(prompt.is_empty());
}

#[test]
fn ctrl_minus_survives_the_crossterm_conversion() {
    let mut ed = Editor::new();
    type_text(&mut ed, "ab");
    assert_eq!(ed.text(), "ab");

    // crossterm reports kitty-protocol `Ctrl+-` as Char('-') + CONTROL;
    // the `From` conversion must keep both halves intact.
    let event = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char('-'), CtModifiers::CONTROL));
    let InputEvent::Key(k) = event else {
        panic!("expected a key event");
    };
    assert_eq!(k, Key::new(KeyCode::Char('-'), KeyModifiers::CONTROL));

    assert_eq!(ed.handle_event(event), EditorAction::Changed);
    assert_eq!(ed.text(), "");
}

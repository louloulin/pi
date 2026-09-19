//! Integration coverage for word navigation and word kill
//! (`tui.editor.cursorWordLeft` / `cursorWordRight` /
//! `deleteWordBackward` / `deleteWordForward`).
//!
//! These tests drive the public component surface — [`Editor`] and
//! [`Prompt`] through [`InputEvent`]s — so they cover the same path the
//! interactive mode uses, plus the `crossterm` → [`InputEvent`]
//! conversion for the chords a terminal actually sends:
//!
//! 1. `Alt+B` / `Alt+F` (and `Alt+Left` / `Alt+Right`,
//!    `Ctrl+Left` / `Ctrl+Right`) move the cursor by whole words.
//! 2. `Ctrl+W` / `Alt+Backspace` and `Alt+D` / `Alt+Delete` kill words
//!    on either side of the cursor without corrupting UTF-8.
//! 3. Word kills share the kill ring with the line kills, so `Ctrl+Y`
//!    restores them and consecutive kills accumulate.
//! 4. `Ctrl+-` undoes a word kill.
//! 5. The edited text is what `Enter` submits.

use pi_tui::backend::event::{
    KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyModifiers as CtModifiers,
};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, Prompt, PromptAction};

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

/// `git commit -m message`, with the offsets every assertion below uses:
///
/// ```text
/// git commit -m message
/// 0   4      11 14    21
/// ```
const CMD: &str = "git commit -m message";

#[test]
fn word_moves_stop_at_word_boundaries() {
    let mut ed = Editor::new();
    type_text(&mut ed, CMD);
    assert_eq!(ed.cursor(), 21);

    // `Alt+B` jumps the whole last word.
    assert_eq!(ed.handle_event(alt('b')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 14); // start of "message"
                                 // `Alt+Left` treats "-m" as punctuation + word, so it lands on "m"…
    assert_eq!(
        ed.handle_event(key(KeyCode::Left, KeyModifiers::ALT)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 12);
    // …and `Ctrl+Left` finishes the "-m" piece, stopping on the space.
    assert_eq!(
        ed.handle_event(key(KeyCode::Left, KeyModifiers::CONTROL)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 11);
    assert_eq!(
        ed.handle_event(key(KeyCode::Left, KeyModifiers::ALT)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 4); // start of "commit"

    // Walking forward covers the same boundaries in reverse.
    assert_eq!(
        ed.handle_event(key(KeyCode::Right, KeyModifiers::ALT)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 10); // end of "commit"
    assert_eq!(ed.handle_event(alt('f')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 12); // past the "-"
    assert_eq!(
        ed.handle_event(key(KeyCode::Right, KeyModifiers::CONTROL)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 13); // end of "-m"
    assert_eq!(
        ed.handle_event(key(KeyCode::Right, KeyModifiers::ALT)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 21);
    // At the end of the buffer the move is a no-op.
    assert_eq!(ed.handle_event(alt('f')), EditorAction::None);
    assert_eq!(ed.cursor(), 21);
}

#[test]
fn word_kills_delete_whole_words_and_yank_restores_them() {
    let mut ed = Editor::new();
    type_text(&mut ed, CMD);

    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.text(), "git commit -m ");
    assert_eq!(ed.cursor(), 14);

    // `Alt+D` kills forward, so move to the start first.
    ed.move_home();
    assert_eq!(ed.handle_event(alt('d')), EditorAction::Changed);
    assert_eq!(ed.text(), " commit -m ");
    assert_eq!(ed.cursor(), 0);

    // Both kills landed on the ring; `Ctrl+Y` restores the most recent
    // one ("git") first.
    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "git commit -m ");
    assert_eq!(ed.cursor(), 3);
}

#[test]
fn consecutive_word_kills_accumulate_like_line_kills() {
    let mut ed = Editor::new();
    type_text(&mut ed, "one two three");

    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.text(), "one ");
    assert_eq!(ed.kill_ring_len(), 1);
    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "one two three");

    // A line kill starts its own entry (the yank ended the chain), so
    // `Ctrl+U` + `Ctrl+Y` restores the whole buffer.
    ed.move_end();
    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    assert_eq!(ed.text(), "");
    assert_eq!(ed.kill_ring_len(), 2);
    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "one two three");
}

#[test]
fn word_kills_are_undoable_and_utf8_safe() {
    let mut ed = Editor::new();
    type_text(&mut ed, "héllo wörld");

    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.text(), "héllo ");
    assert_eq!(ed.cursor(), "héllo ".len());

    let undo = key(KeyCode::Char('-'), KeyModifiers::CONTROL);
    assert_eq!(ed.handle_event(undo), EditorAction::Changed);
    assert_eq!(ed.text(), "héllo wörld");
    assert_eq!(ed.cursor(), "héllo wörld".len());
}

#[test]
fn word_chords_survive_the_crossterm_conversion() {
    let mut ed = Editor::new();
    type_text(&mut ed, "git commit");

    // Terminals report `Alt+Backspace` as Backspace + ALT and
    // `Ctrl+Left` as Left + CONTROL.
    let alt_backspace = InputEvent::from(CtKeyEvent::new(CtKeyCode::Backspace, CtModifiers::ALT));
    assert_eq!(alt_backspace, key(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(ed.handle_event(alt_backspace), EditorAction::Changed);
    assert_eq!(ed.text(), "git ");
    assert_eq!(ed.cursor(), 4);

    let ctrl_left = InputEvent::from(CtKeyEvent::new(CtKeyCode::Left, CtModifiers::CONTROL));
    assert_eq!(ctrl_left, key(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(ed.handle_event(ctrl_left), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);

    let alt_delete = InputEvent::from(CtKeyEvent::new(CtKeyCode::Delete, CtModifiers::ALT));
    assert_eq!(alt_delete, key(KeyCode::Delete, KeyModifiers::ALT));
    assert_eq!(ed.handle_event(alt_delete), EditorAction::Changed);
    assert_eq!(ed.text(), " ");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn prompt_submits_the_word_navigated_text() {
    let mut prompt = Prompt::new("› ");

    for c in "hello world".chars() {
        assert_eq!(prompt.handle_event(plain(c)), PromptAction::Changed);
    }
    // Delete "world" and retype a different one.
    assert_eq!(prompt.handle_event(ctrl('w')), PromptAction::Changed);
    for c in "there".chars() {
        assert_eq!(prompt.handle_event(plain(c)), PromptAction::Changed);
    }
    assert_eq!(prompt.text(), "hello there");
    assert_eq!(
        prompt.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        PromptAction::Submit("hello there".to_string())
    );
    // The prompt itself does not clear on submit — the caller does
    // (`App::step_key` clears before announcing the submission).
    assert_eq!(prompt.text(), "hello there");
    prompt.clear();
    assert!(prompt.is_empty());
}

#[test]
fn prompt_word_move_does_not_submit_or_interrupt() {
    let mut prompt = Prompt::new("› ");
    for c in "hello world".chars() {
        prompt.handle_event(plain(c));
    }
    assert_eq!(prompt.handle_event(alt('b')), PromptAction::Changed);
    assert_eq!(prompt.editor().cursor(), 6);
    assert_eq!(prompt.text(), "hello world");
}

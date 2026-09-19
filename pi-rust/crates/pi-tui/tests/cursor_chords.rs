//! Integration coverage for the emacs cursor aliases `Ctrl+B` / `Ctrl+F`
//! (`tui.editor.cursorLeft` / `cursorRight`).
//!
//! Upstream binds `tui.editor.cursorLeft` to `["left", "ctrl+b"]` and
//! `tui.editor.cursorRight` to `["right", "ctrl+f"]`
//! (`packages/tui/src/keybindings.ts:82`), so the chords must move the
//! cursor by exactly one character — not one word, which is what the
//! neighbouring `Alt+B` / `Alt+F` pairs do — and must survive the
//! `crossterm` → [`InputEvent`] conversion a real terminal goes through.
//!
//! These tests drive the public component surface ([`Editor`] and
//! [`Prompt`] via [`InputEvent`]s), matching `tests/word_navigation.rs`.

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

fn plain(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn type_text(ed: &mut Editor, text: &str) {
    for c in text.chars() {
        assert_eq!(ed.handle_event(plain(c)), EditorAction::Changed);
    }
}

#[test]
fn ctrl_b_and_ctrl_f_move_one_char() {
    let mut ed = Editor::new();
    type_text(&mut ed, "hello");
    assert_eq!(ed.cursor(), 5);

    assert_eq!(ed.handle_event(ctrl('b')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 4);
    assert_eq!(ed.handle_event(ctrl('b')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 3);
    assert_eq!(ed.handle_event(ctrl('f')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 4);
    assert_eq!(ed.text(), "hello");
}

#[test]
fn ctrl_b_and_ctrl_f_are_noops_at_the_buffer_edges() {
    let mut ed = Editor::new();
    type_text(&mut ed, "ab");

    // Cursor starts at the end: `Ctrl+F` cannot move further.
    assert_eq!(ed.handle_event(ctrl('f')), EditorAction::None);
    assert_eq!(ed.cursor(), 2);

    ed.move_home();
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(ctrl('b')), EditorAction::None);
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn ctrl_b_and_ctrl_f_step_over_multibyte_chars() {
    let mut ed = Editor::new();
    type_text(&mut ed, "héllo");
    assert_eq!(ed.cursor(), "héllo".len());

    // Bytes: h=0, é=1..3 (two bytes), l=3, l=4, o=5 — one chord steps
    // over the whole character, the same offsets `move_left` uses.
    for expected in [5, 4, 3, 1, 0] {
        assert_eq!(ed.handle_event(ctrl('b')), EditorAction::Changed);
        assert_eq!(ed.cursor(), expected);
    }
    assert_eq!(ed.handle_event(ctrl('b')), EditorAction::None);

    assert_eq!(ed.handle_event(ctrl('f')), EditorAction::Changed);
    assert_eq!(ed.cursor(), "h".len());
    assert_eq!(ed.handle_event(ctrl('f')), EditorAction::Changed);
    assert_eq!(ed.cursor(), "hé".len());
}

#[test]
fn cursor_chords_survive_the_crossterm_conversion() {
    let mut ed = Editor::new();
    type_text(&mut ed, "git commit");

    // A terminal sends `0x02` / `0x06`; crossterm decodes both as
    // `Char(..)` + CONTROL, and `InputEvent` must keep that shape.
    let ctrl_b = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char('b'), CtModifiers::CONTROL));
    assert_eq!(ctrl_b, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(ed.handle_event(ctrl_b), EditorAction::Changed);
    assert_eq!(ed.cursor(), 9);

    let ctrl_f = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char('f'), CtModifiers::CONTROL));
    assert_eq!(ctrl_f, key(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(ed.handle_event(ctrl_f), EditorAction::Changed);
    assert_eq!(ed.cursor(), 10);
    // Neither chord edits the buffer.
    assert_eq!(ed.text(), "git commit");
}

#[test]
fn prompt_cursor_chords_change_but_never_submit() {
    let mut prompt = Prompt::new("› ");
    for c in "hello world".chars() {
        prompt.handle_event(plain(c));
    }

    assert_eq!(prompt.handle_event(ctrl('b')), PromptAction::Changed);
    assert_eq!(prompt.editor().cursor(), 10);
    assert_eq!(prompt.handle_event(ctrl('f')), PromptAction::Changed);
    assert_eq!(prompt.editor().cursor(), 11);
    assert_eq!(prompt.text(), "hello world");
}

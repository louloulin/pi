//! Integration coverage for `Ctrl+D` (`tui.editor.deleteCharForward`).
//!
//! Upstream binds `tui.editor.deleteCharForward` to `["delete", "ctrl+d"]`
//! (`packages/tui/src/keybindings.ts:121`), and the coding agent's custom
//! editor only treats `Ctrl+D` as an exit chord while the buffer is empty —
//! otherwise it falls through to the editor's delete-char-forward handler
//! (`packages/coding-agent/src/modes/interactive/components/custom-editor.ts:117`).
//!
//! These tests drive the public component surface ([`Editor`] and
//! [`Prompt`] via [`InputEvent`]s), matching `tests/cursor_chords.rs`.

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
fn ctrl_d_on_an_empty_buffer_is_eof() {
    let mut ed = Editor::new();
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Eof);
    // The exit chord never touches the buffer.
    assert_eq!(ed.text(), "");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn ctrl_d_on_a_non_empty_buffer_deletes_forward() {
    let mut ed = Editor::new();
    type_text(&mut ed, "xy");
    ed.move_home();
    assert_eq!(ed.cursor(), 0);

    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Changed);
    assert_eq!(ed.text(), "y");
    assert_eq!(ed.cursor(), 0);

    // Deleting the last character is still a `Changed`, not an `Eof`:
    // the exit chord only fires on an already-empty buffer.
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Changed);
    assert_eq!(ed.text(), "");
    assert_eq!(ed.cursor(), 0);

    // Now the buffer is empty, so the next `Ctrl+D` exits.
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Eof);
}

#[test]
fn ctrl_d_at_the_end_of_a_non_empty_buffer_is_a_noop_not_eof() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abc");
    assert_eq!(ed.cursor(), 3);

    // `delete()` cannot advance past the end, so this is `None` — and
    // crucially not `Eof`, because the buffer still holds text.
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::None);
    assert_eq!(ed.text(), "abc");
    assert_eq!(ed.cursor(), 3);
}

#[test]
fn ctrl_d_steps_over_multibyte_chars() {
    let mut ed = Editor::new();
    type_text(&mut ed, "héllo");
    ed.move_home();

    // Bytes: h=0, é=1..3 (two bytes), l=3, l=4, o=5 — one chord removes
    // the whole character, the same offsets `delete()` uses.
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Changed);
    assert_eq!(ed.text(), "éllo");
    assert_eq!(ed.cursor(), 0);

    // The two-byte `é` is removed in one step, leaving the cursor on the
    // next character boundary.
    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Changed);
    assert_eq!(ed.text(), "llo");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn ctrl_d_push_is_undoable() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abc");
    ed.move_home();

    assert_eq!(ed.handle_event(ctrl('d')), EditorAction::Changed);
    assert_eq!(ed.text(), "bc");

    // `Ctrl+-` (`tui.editor.undo`) restores the deleted character.
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "abc");
}

#[test]
fn ctrl_d_survives_the_crossterm_conversion() {
    // A terminal sends `0x04`; crossterm decodes it as `Char('d')` +
    // CONTROL, and `InputEvent` must keep that shape.
    let event = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char('d'), CtModifiers::CONTROL));
    assert_eq!(event, key(KeyCode::Char('d'), KeyModifiers::CONTROL));

    let mut ed = Editor::new();
    assert_eq!(ed.handle_event(event), EditorAction::Eof);
}

#[test]
fn prompt_ctrl_d_exits_only_when_empty() {
    let mut prompt = Prompt::new("› ");
    assert_eq!(prompt.handle_event(ctrl('d')), PromptAction::Eof);

    for c in "hi".chars() {
        prompt.handle_event(plain(c));
    }
    prompt.editor_mut().move_home();
    assert_eq!(prompt.handle_event(ctrl('d')), PromptAction::Changed);
    assert_eq!(prompt.text(), "i");

    assert_eq!(prompt.handle_event(ctrl('d')), PromptAction::Changed);
    assert_eq!(prompt.text(), "");
    assert_eq!(prompt.handle_event(ctrl('d')), PromptAction::Eof);
}

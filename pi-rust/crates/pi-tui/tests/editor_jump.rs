//! Integration coverage for the editor jump mode
//! (`tui.editor.jumpForward` / `tui.editor.jumpBackward`,
//! `Ctrl+]` / `Ctrl+Alt+]`).
//!
//! Upstream `packages/tui/components/editor.ts:687-705,2126-2155` arms a
//! one-shot "jump" target: the hotkey arms the mode, the next printable
//! character moves the cursor to its next (forward) or previous
//! (backward) occurrence instead of inserting, and the character under
//! the cursor is never a match. These tests drive the same public
//! surface the interactive mode uses:
//!
//! 1. `Ctrl+]` arms a forward jump; the target key moves the cursor and
//!    is not inserted.
//! 2. `Ctrl+Alt+]` arms a backward jump.
//! 3. Repeated presses of the hotkey walk occurrence by occurrence.
//! 4. Matching is case-sensitive.
//! 5. A character with no match leaves the cursor alone and still exits
//!    the mode.
//! 6. Pressing either hotkey again cancels without moving.
//! 7. Any non-printable key cancels and still performs its own action
//!    (`Enter` submits).
//! 8. A jump is a pure cursor move: it neither edits the buffer, pushes
//!    an undo snapshot, nor survives `clear`.
//! 9. The chords survive the `crossterm` → [`InputEvent`] conversion,
//!    including the legacy `Ctrl+5` / `Ctrl+Alt+5` spellings crossterm
//!    derives from the `0x1D` / `ESC 0x1D` byte sequences.
//! 10. [`Prompt`] forwards the chords and only reports `Changed` when
//!     the cursor actually moved (arming a jump never submits).

use pi_tui::backend::event::{
    KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyModifiers as CtModifiers,
};
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, JumpDirection, Key, Prompt, PromptAction};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

/// `Ctrl+]`, the kitty-protocol spelling of `tui.editor.jumpForward`.
fn jump_forward() -> InputEvent {
    key(KeyCode::Char(']'), KeyModifiers::CONTROL)
}

/// `Ctrl+Alt+]`, the kitty-protocol spelling of `tui.editor.jumpBackward`.
fn jump_backward() -> InputEvent {
    key(
        KeyCode::Char(']'),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::CONTROL
        },
    )
}

/// Type a string one character at a time, the way a terminal delivers
/// keystrokes.
fn type_text(ed: &mut Editor, text: &str) {
    for c in text.chars() {
        ed.insert_char(c);
    }
}

#[test]
fn ctrl_bracket_jumps_forward_to_the_next_occurrence() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abcabc");
    ed.move_home();
    assert_eq!(ed.cursor(), 0);

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(ed.jump_mode(), Some(JumpDirection::Forward));

    // The target character is consumed by the jump, not inserted.
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('c'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "abcabc");
    assert_eq!(ed.cursor(), 2);
    assert_eq!(ed.jump_mode(), None);
}

#[test]
fn ctrl_alt_bracket_jumps_backward_to_the_previous_occurrence() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abcabc");
    assert_eq!(ed.cursor(), 6);

    assert_eq!(ed.handle_event(jump_backward()), EditorAction::None);
    assert_eq!(ed.jump_mode(), Some(JumpDirection::Backward));

    assert_eq!(
        ed.handle_event(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "abcabc");
    assert_eq!(ed.cursor(), 3);
    assert_eq!(ed.jump_mode(), None);
}

#[test]
fn repeated_jumps_walk_each_occurrence() {
    let mut ed = Editor::new();
    type_text(&mut ed, "a-b-c-d");
    ed.move_home();

    for expected in [1usize, 3, 5] {
        assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
        assert_eq!(
            ed.handle_event(key(KeyCode::Char('-'), KeyModifiers::NONE)),
            EditorAction::Changed
        );
        assert_eq!(ed.cursor(), expected);
    }

    // No further `-` ahead of the last occurrence.
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('-'), KeyModifiers::NONE)),
        EditorAction::None
    );
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn the_character_under_the_cursor_is_not_a_match() {
    let mut ed = Editor::new();
    type_text(&mut ed, "aa");
    ed.move_home();
    assert_eq!(ed.cursor(), 0);

    // Forward from position 0 must skip the `a` it is sitting on.
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 1);

    // Backward from position 1 finds the `a` before it.
    assert_eq!(ed.handle_event(jump_backward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn matching_is_case_sensitive() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abcABC");
    ed.move_home();

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('A'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 3);

    // A shifted target keeps its shift modifier and still jumps.
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('C'), KeyModifiers::SHIFT)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn multi_byte_targets_land_on_a_char_boundary() {
    let mut ed = Editor::new();
    type_text(&mut ed, "ab→c");
    ed.move_home();

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('→'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert!(ed.text().is_char_boundary(ed.cursor()));
    assert_eq!(ed.cursor(), "ab".len());
}

#[test]
fn a_missing_occurrence_leaves_the_cursor_but_exits_the_mode() {
    let mut ed = Editor::new();
    type_text(&mut ed, "xyz");
    ed.move_home();

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('q'), KeyModifiers::NONE)),
        EditorAction::None
    );
    assert_eq!(ed.text(), "xyz");
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.jump_mode(), None);

    // The mode is gone, so the next character types normally.
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('q'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "qxyz");
}

#[test]
fn pressing_a_jump_hotkey_again_cancels() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abc");
    ed.move_home();

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(ed.jump_mode(), None);
    assert_eq!(ed.cursor(), 0);

    // Crossing directions also cancels rather than re-arming.
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(ed.handle_event(jump_backward()), EditorAction::None);
    assert_eq!(ed.jump_mode(), None);
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn a_non_printable_key_cancels_and_still_runs() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abc");

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    match ed.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)) {
        EditorAction::Submit(text) => assert_eq!(text, "abc"),
        other => panic!("unexpected action: {:?}", other),
    }
    assert_eq!(ed.jump_mode(), None);

    // A control chord behaves the same way: it cancels and runs.
    ed.move_home();
    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 3);
    assert_eq!(ed.jump_mode(), None);
}

#[test]
fn a_jump_is_not_undoable_and_does_not_edit() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abcabc");
    ed.move_home();
    let undo_before = ed.undo_len();

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('c'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 2);
    assert_eq!(ed.undo_len(), undo_before, "a jump must not push undo");
    assert_eq!(ed.text(), "abcabc");
}

#[test]
fn clear_drops_a_pending_jump() {
    let mut ed = Editor::new();
    type_text(&mut ed, "abc");

    assert_eq!(ed.handle_event(jump_forward()), EditorAction::None);
    ed.clear();
    assert_eq!(ed.jump_mode(), None);

    // The next character types into the freshly cleared buffer.
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('z'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "z");
}

#[test]
fn jump_chords_survive_the_crossterm_conversion() {
    // Kitty-protocol spelling.
    let event = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char(']'), CtModifiers::CONTROL));
    let InputEvent::Key(k) = event else {
        panic!("expected a key event");
    };
    assert_eq!(k, Key::new(KeyCode::Char(']'), KeyModifiers::CONTROL));

    let mut ed = Editor::new();
    type_text(&mut ed, "abcabc");
    ed.move_home();
    assert_eq!(ed.handle_event(event), EditorAction::None);
    assert_eq!(ed.jump_mode(), Some(JumpDirection::Forward));
}

#[test]
fn legacy_ctrl_five_event_also_arms_a_jump() {
    // Without the Kitty keyboard protocol, `Ctrl+]` arrives as the `0x1D`
    // control byte; crossterm's parser maps `0x1C..=0x1F` onto
    // `Ctrl+4..=Ctrl+7` (`event/sys/unix/parse.rs`), so the editor sees
    // `Ctrl+5` and must treat it as `tui.editor.jumpForward`.
    let mut ed = Editor::new();
    type_text(&mut ed, "abcabc");
    ed.move_home();

    let forward = InputEvent::from(CtKeyEvent::new(CtKeyCode::Char('5'), CtModifiers::CONTROL));
    assert_eq!(ed.handle_event(forward), EditorAction::None);
    assert_eq!(ed.jump_mode(), Some(JumpDirection::Forward));
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('c'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 2);

    // `ESC 0x1D` is `Ctrl+Alt+]` — crossterm adds ALT to the decoded
    // `Ctrl+5` event, which must arm a backward jump.
    let backward = InputEvent::from(CtKeyEvent::new(
        CtKeyCode::Char('5'),
        CtModifiers::CONTROL | CtModifiers::ALT,
    ));
    assert_eq!(ed.handle_event(backward), EditorAction::None);
    assert_eq!(ed.jump_mode(), Some(JumpDirection::Backward));
    assert_eq!(
        ed.handle_event(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn prompt_forwards_jump_chords_without_submitting() {
    let mut prompt = Prompt::new("› ");
    for c in "abcabc".chars() {
        prompt.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    prompt.editor_mut().move_home();

    // Arming the jump changes nothing visible, so it is not `Changed`.
    assert_eq!(prompt.handle_event(jump_forward()), PromptAction::None);
    assert_eq!(prompt.editor().jump_mode(), Some(JumpDirection::Forward));

    // The target key moves the cursor and never submits or edits.
    assert_eq!(
        prompt.handle_event(key(KeyCode::Char('c'), KeyModifiers::NONE)),
        PromptAction::Changed
    );
    assert_eq!(prompt.editor().cursor(), 2);
    assert_eq!(prompt.text(), "abcabc");
}

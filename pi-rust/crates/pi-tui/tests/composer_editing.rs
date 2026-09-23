//! Composer editing parity: what a multi-line draft does when a key
//! arrives.
//!
//! LUM-1312 gave the composer a real visual layout ([`pi_tui::visual`] via
//! `Prompt`) but the *editor* still behaved like a one-line text field:
//! `Shift+Enter` submitted instead of adding a line, `Up` / `Down` always
//! reached into the prompt history, `Home` / `End` jumped to the ends of
//! the whole draft, and `Ctrl+U` / `Ctrl+K` / `Ctrl+W` were scoped to the
//! buffer instead of the line. Every case below is the upstream
//! behaviour, with the line references this port mirrors.
//!
//! The width matters: the editor measures the draft with the same wrap the
//! renderer paints with (`Editor::set_visual_width`), which is what the
//! `App` publishes every frame (`AppConfig::composer_max_rows` /
//! `Prompt::body_width`). A test that forgets it is testing the
//! single-row fallback, which is why `visual_width` is set explicitly
//! wherever a case depends on wrapping.

use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn shift_enter() -> InputEvent {
    key(KeyCode::Enter, KeyModifiers::SHIFT)
}

fn enter() -> InputEvent {
    key(KeyCode::Enter, KeyModifiers::NONE)
}

fn up() -> InputEvent {
    key(KeyCode::Up, KeyModifiers::NONE)
}

fn down() -> InputEvent {
    key(KeyCode::Down, KeyModifiers::NONE)
}

fn home() -> InputEvent {
    key(KeyCode::Home, KeyModifiers::NONE)
}

fn end() -> InputEvent {
    key(KeyCode::End, KeyModifiers::NONE)
}

fn ctrl(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn alt(code: KeyCode) -> InputEvent {
    key(code, KeyModifiers::ALT)
}

fn plain(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

/// An editor that wraps at `width` body columns, with `text` in the buffer
/// and the cursor at the end of it.
fn editor_at(width: usize, text: &str) -> Editor {
    let mut ed = Editor::new();
    ed.set_visual_width(width);
    ed.insert_str(text);
    ed
}

fn type_text(ed: &mut Editor, text: &str) {
    for c in text.chars() {
        assert_eq!(ed.handle_event(plain(c)), EditorAction::Changed);
    }
}

// ---------------------------------------------------------------------------
// Newlines: the composer grows instead of sending the draft.
// ---------------------------------------------------------------------------

#[test]
fn shift_enter_inserts_a_newline_and_enter_still_submits() {
    let mut ed = Editor::new();
    type_text(&mut ed, "first line");

    // Upstream `tui.input.newLine` (`packages/tui/src/keybindings.ts:143`).
    assert_eq!(ed.handle_event(shift_enter()), EditorAction::Changed);
    assert_eq!(ed.text(), "first line\n");
    assert_eq!(ed.cursor(), 11);

    type_text(&mut ed, "second");
    assert_eq!(
        ed.visual_row_count(),
        2,
        "the newline is a row, even with no width recorded"
    );

    // `Enter` is `tui.input.submit` and must still hand the draft over.
    match ed.handle_event(enter()) {
        EditorAction::Submit(text) => assert_eq!(text, "first line\nsecond"),
        other => panic!("expected a submit, got {other:?}"),
    }
}

#[test]
fn ctrl_j_is_the_shift_enter_alias() {
    let mut ed = Editor::new();
    type_text(&mut ed, "a");
    assert_eq!(ed.handle_event(ctrl('j')), EditorAction::Changed);
    assert_eq!(ed.text(), "a\n");
}

#[test]
fn a_trailing_backslash_turns_enter_into_a_newline() {
    // Upstream `shouldSubmitOnBackslashEnter`: the fallback for terminals
    // that cannot report `Shift+Enter`. The backslash is consumed, so the
    // newline is not left with a stray `\` in front of it.
    let mut ed = Editor::new();
    type_text(&mut ed, "line\\");
    assert_eq!(ed.handle_event(enter()), EditorAction::Changed);
    assert_eq!(ed.text(), "line\n");

    // Enter again submits, because there is no backslash in front of it.
    assert!(matches!(ed.handle_event(enter()), EditorAction::Submit(_)));
}

#[test]
fn a_newline_mid_draft_splits_the_line_at_the_cursor() {
    let mut ed = Editor::new();
    type_text(&mut ed, "helloworld");
    for _ in 0..5 {
        assert_eq!(ed.handle_event(ctrl('b')), EditorAction::Changed);
    }
    assert_eq!(ed.handle_event(shift_enter()), EditorAction::Changed);
    assert_eq!(ed.text(), "hello\nworld");
    assert_eq!(ed.cursor(), 6, "the cursor opens the new line");
}

// ---------------------------------------------------------------------------
// Vertical motion: visual rows inside the draft, history at the edges.
// ---------------------------------------------------------------------------

#[test]
fn up_and_down_walk_the_logical_lines_of_a_draft() {
    // "alpha" 0-4, '\n' 5, "beta" 6-9, '\n' 10, "gamma" 11-15.
    let mut ed = editor_at(80, "alpha\nbeta\ngamma");
    assert_eq!(ed.cursor(), 16);

    // Up from the end of "gamma" (column 5) lands at the end of "beta",
    // and the run keeps column 5, so the second Up reaches the end of
    // "alpha" too (the sticky column, upstream's `preferredVisualCol`).
    for expected in [10, 5] {
        assert_eq!(ed.handle_event(up()), EditorAction::Changed);
        assert_eq!(ed.cursor(), expected, "Up moves one line at a time");
    }
    // On the first row but past the line start, Up snaps to the line start
    // first; only from column 0 does it reach for the history (which is
    // empty here, so nothing changes).
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(up()), EditorAction::None);
    assert_eq!(ed.cursor(), 0);

    // Down walks the rows back, keeping column 0, and ends the last line
    // when it is asked to go past it.
    for expected in [6, 11, 16] {
        assert_eq!(ed.handle_event(down()), EditorAction::Changed);
        assert_eq!(ed.cursor(), expected);
    }
}

#[test]
fn up_walks_soft_wrapped_rows_before_it_reaches_history() {
    // Body width 10: "hello world foo bar" is three rows
    // ("hello ", "world foo ", "bar").
    let mut ed = editor_at(10, "hello world foo bar");
    assert_eq!(ed.visual_row_count(), 3);
    assert_eq!(ed.cursor(), 19);

    // Up: row 2 -> row 1, keeping the display column where it can.
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.visual_caret().0, 1);
    // Up again: row 1 -> row 0, still holding the column the run started
    // at (3), which is what makes Up/Down feel like a text box.
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.visual_caret(), (0, 3));

    // A draft that occupies one row keeps the historical meaning: Up
    // browses the prompt history instead.
    let mut ed = editor_at(10, "");
    ed.push_history("previous prompt");
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.text(), "previous prompt");
}

#[test]
fn vertical_motion_keeps_the_column_across_a_short_row() {
    // Rows: "one" / "x" / "three" — moving down from column 2 of row 0
    // lands at the end of the short row and then returns to column 2 when
    // the wider row can hold it again (upstream's sticky column).
    let mut ed = editor_at(80, "one\nx\nthree");
    ed.move_home();
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0, "start of the first row");
    for _ in 0..2 {
        assert_eq!(ed.handle_event(ctrl('f')), EditorAction::Changed);
    }
    assert_eq!(ed.visual_caret(), (0, 2));

    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.visual_caret(), (1, 1), "clamped to the short row");
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.visual_caret(), (2, 2), "the column is restored");

    // A horizontal move ends the vertical run, so the next Down takes its
    // column from where the cursor actually is.
    assert_eq!(ed.handle_event(ctrl('b')), EditorAction::Changed);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.visual_caret(), (1, 1));
}

#[test]
fn down_from_the_last_row_returns_to_the_history_draft() {
    let mut ed = editor_at(80, "typed");
    ed.push_history("older prompt");

    // First Up moves to the start of the line (upstream's rule: a
    // one-row draft whose cursor is past column 0 snaps to the line
    // start first), the second browses the history.
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.text(), "older prompt");

    // Down from the history gives the draft back, at the end of it: a
    // recalled entry is replaced wholesale, and so is the draft that
    // replaces it (upstream `navigateHistory` + `setTextInternal`).
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.text(), "typed");
    assert_eq!(ed.cursor(), 5);
    // The draft is one row and the history is behind us: a further Down has
    // nothing to reach.
    assert_eq!(ed.handle_event(down()), EditorAction::None);
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn history_entries_can_be_carried_into_the_draft() {
    let mut ed = editor_at(80, "");
    ed.push_history("hello world");
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    // The recalled entry is real buffer text: it can be edited, and a
    // newline can be added to it.
    assert_eq!(ed.handle_event(end()), EditorAction::None);
    assert_eq!(ed.handle_event(shift_enter()), EditorAction::Changed);
    assert_eq!(ed.text(), "hello world\n");
}

// ---------------------------------------------------------------------------
// Home / End are line-scoped, like upstream moveToLineStart / moveToLineEnd.
// ---------------------------------------------------------------------------

#[test]
fn home_and_end_are_scoped_to_the_logical_line() {
    let mut ed = editor_at(80, "alpha\nbeta\ngamma");
    // Home on the last line stops after its newline, not at the draft start.
    assert_eq!(ed.handle_event(home()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 11, "start of 'gamma', not start of the draft");

    // Second line: Home stops after its newline, End stops before the next.
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 6);
    assert_eq!(ed.handle_event(end()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 10, "end of 'beta', not end of the draft");
    assert_eq!(ed.handle_event(home()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 6, "start of 'beta', not start of the draft");

    // Back down to the last line.
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 11);
    assert_eq!(ed.handle_event(end()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 16);
}

#[test]
fn ctrl_a_and_ctrl_e_match_home_and_end() {
    let mut ed = editor_at(80, "abc\ndef");
    assert_eq!(ed.cursor(), 7);
    assert_eq!(ed.handle_event(ctrl('a')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 4, "start of 'def'");
    assert_eq!(ed.handle_event(ctrl('e')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 7);
    // On the first line, Up lands at column 3 of "abc", which is already
    // that line's end — so Ctrl+E has nothing left to do...
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 3);
    assert_eq!(ed.handle_event(ctrl('e')), EditorAction::None);
    // ...while from the line start it moves to the line end, stopping
    // before the newline rather than at the end of the draft.
    assert_eq!(ed.handle_event(ctrl('a')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(ctrl('e')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 3);
}

#[test]
fn a_single_line_draft_keeps_the_whole_buffer_as_its_line() {
    // The pre-LUM-1312 contract for the common case: no newline means
    // Home / End are the buffer corners.
    let mut ed = editor_at(80, "hello world");
    assert_eq!(ed.handle_event(home()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(end()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 11);
}

// ---------------------------------------------------------------------------
// Line-scoped kills, with the newline as the line-boundary target.
// ---------------------------------------------------------------------------

#[test]
fn ctrl_u_kills_to_the_start_of_the_line_not_the_draft() {
    let mut ed = editor_at(80, "alpha\nbeta\ngamma");
    assert_eq!(ed.cursor(), 16);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 10, "end of 'beta'");

    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    assert_eq!(ed.text(), "alpha\n\ngamma", "only 'beta' went");
    assert_eq!(ed.cursor(), 6);
}

#[test]
fn ctrl_u_at_a_line_start_joins_it_to_the_previous_line() {
    let mut ed = editor_at(80, "alpha\nbeta");
    ed.handle_event(home());
    assert_eq!(ed.cursor(), 6);
    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    assert_eq!(ed.text(), "alphabeta");
    assert_eq!(ed.cursor(), 5);
    // The newline itself is what the kill ring holds.
    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "alpha\nbeta");
    assert_eq!(ed.cursor(), 6);
}

#[test]
fn ctrl_k_kills_to_the_end_of_the_line_and_joins_the_next_one() {
    let mut ed = editor_at(80, "alpha\nbeta\ngamma");
    ed.handle_event(home());
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(ctrl('k')), EditorAction::Changed);
    assert_eq!(ed.text(), "\nbeta\ngamma", "only 'alpha' went");
    assert_eq!(ed.cursor(), 0);

    // At the end of the (now empty) first line, Ctrl+K joins the next one.
    assert_eq!(ed.handle_event(ctrl('k')), EditorAction::Changed);
    assert_eq!(ed.text(), "beta\ngamma");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn consecutive_line_kills_accumulate_into_one_ring_entry() {
    let mut ed = editor_at(80, "one\ntwo\nthree");
    ed.handle_event(home());
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(ctrl('k')), EditorAction::Changed);
    assert_eq!(ed.handle_event(ctrl('k')), EditorAction::Changed);
    assert_eq!(ed.text(), "two\nthree", "the newline came along");
    // `Ctrl+Y` yanks the accumulated "one\n" back in one go.
    assert_eq!(ed.handle_event(ctrl('y')), EditorAction::Changed);
    assert_eq!(ed.text(), "one\ntwo\nthree");
}

#[test]
fn ctrl_w_at_a_line_start_joins_lines_like_backspace() {
    let mut ed = editor_at(80, "alpha\nbeta");
    ed.handle_event(home());
    assert_eq!(ed.cursor(), 6);
    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.text(), "alphabeta", "the newline went, not the word");
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn ctrl_w_inside_a_line_kills_only_that_line_s_word() {
    let mut ed = editor_at(80, "alpha\nbeta gamma");
    assert_eq!(ed.cursor(), 16);
    assert_eq!(ed.handle_event(ctrl('w')), EditorAction::Changed);
    assert_eq!(ed.text(), "alpha\nbeta ");
    assert_eq!(ed.cursor(), 11);
}

#[test]
fn alt_d_at_the_end_of_a_line_joins_the_next_line() {
    let mut ed = editor_at(80, "alpha\nbeta\ngamma");
    ed.handle_event(home());
    ed.handle_event(up());
    ed.handle_event(up());
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(ctrl('e')), EditorAction::Changed);
    assert_eq!(ed.cursor(), 5, "end of 'alpha'");
    assert_eq!(
        ed.handle_event(alt(KeyCode::Char('d'))),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "alphabeta\ngamma", "the newline went");
    assert_eq!(ed.cursor(), 5, "the join point");
}

#[test]
fn alt_d_inside_a_line_kills_only_that_line_s_next_word() {
    let mut ed = editor_at(80, "alpha beta\ngamma");
    ed.handle_event(home());
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(
        ed.handle_event(alt(KeyCode::Char('d'))),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), " beta\ngamma", "the newline survived");
    assert_eq!(ed.cursor(), 0);
}

// ---------------------------------------------------------------------------
// Word motions cross logical lines.
// ---------------------------------------------------------------------------

#[test]
fn word_motions_step_across_logical_lines() {
    let mut ed = editor_at(80, "alpha\nbeta");
    ed.handle_event(home());
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);

    // Forward to the end of the word / line, then across the newline onto
    // the start of the next line.
    assert_eq!(ed.handle_event(alt(KeyCode::Right)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 5, "end of 'alpha'");
    assert_eq!(ed.handle_event(alt(KeyCode::Right)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 6, "start of 'beta'");
    // Backward off the start of line 1 lands at the end of line 0.
    assert_eq!(ed.handle_event(alt(KeyCode::Left)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 5);
    assert_eq!(ed.handle_event(alt(KeyCode::Left)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn word_motions_stay_inside_their_line_mid_line() {
    let mut ed = editor_at(80, "one two\nthree four");
    ed.handle_event(home());
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
    assert_eq!(ed.handle_event(alt(KeyCode::Right)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 3, "end of 'one'");
    assert_eq!(ed.handle_event(alt(KeyCode::Right)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 7, "end of 'two', which is the line end");
    assert_eq!(ed.handle_event(alt(KeyCode::Left)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 4, "'two' starts here, still on line 0");
    assert_eq!(ed.handle_event(alt(KeyCode::Left)), EditorAction::Changed);
    assert_eq!(ed.cursor(), 0);
}

// ---------------------------------------------------------------------------
// Deletion around newlines, undo, and chip alignment.
// ---------------------------------------------------------------------------

#[test]
fn backspace_at_a_line_start_joins_the_previous_line() {
    let mut ed = editor_at(80, "alpha\nbeta");
    ed.handle_event(home());
    assert_eq!(ed.cursor(), 6);
    assert_eq!(
        ed.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE)),
        EditorAction::Changed
    );
    assert_eq!(ed.text(), "alphabeta");
    assert_eq!(ed.cursor(), 5);
}

#[test]
fn undo_restores_a_multi_line_edit() {
    let mut ed = editor_at(80, "alpha");
    ed.handle_event(shift_enter());
    type_text(&mut ed, "beta");
    assert_eq!(ed.text(), "alpha\nbeta");
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "alpha\n", "the typed word went");
    assert_eq!(ed.handle_event(ctrl('-')), EditorAction::Changed);
    assert_eq!(ed.text(), "alpha", "the newline went too");
}

#[test]
fn a_draft_that_needs_more_rows_than_the_cap_scrolls_with_the_cursor() {
    // The renderer keeps the cursor row on screen; `Prompt::render_lines`
    // is what enforces it, and the editor only has to report the row
    // correctly. Body width 4, six lines, cap 3 → rows 3..5 are shown
    // while the cursor is on the last one.
    let mut ed = editor_at(4, "a\nb\nc\nd\ne\nf");
    assert_eq!(ed.visual_row_count(), 6);
    assert_eq!(ed.visual_caret().0, 5);
    for _ in 0..5 {
        assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    }
    assert_eq!(ed.visual_caret().0, 0);
}

#[test]
fn image_chips_stay_aligned_across_newlines() {
    // A chip is one sentinel in the buffer; a newline next to it must not
    // shift the attachment pairing.
    use pi_protocol::ImageContent;
    let mut ed = Editor::new();
    type_text(&mut ed, "look");
    assert_eq!(
        ed.insert_image(ImageContent {
            mime_type: "image/png".into(),
            data: "AAAA".into(),
        }),
        pi_tui::editor::ImageInsertOutcome::Inserted
    );
    ed.handle_event(shift_enter());
    type_text(&mut ed, "done");
    assert_eq!(ed.image_count(), 1);
    assert_eq!(ed.display_text(), "look[Image #1]\ndone");

    // Home / End are line-scoped, and the cursor lands on the chip
    // boundary rather than inside its rendered label.
    ed.handle_event(home());
    ed.handle_event(up());
    assert_eq!(ed.cursor(), 0);
    ed.handle_event(end());
    assert_eq!(
        ed.cursor(),
        ed.text().find('\n').expect("the draft has two lines"),
        "the line ends on the newline, right after the chip sentinel"
    );
    assert_eq!(ed.image_count(), 1);
}

//! Bracketed paste in the composer (LUM-1328).
//!
//! Upstream pi turns bracketed paste on (`packages/tui/src/terminal.ts:184`)
//! and hands the payload to `Editor.handlePaste`
//! (`packages/tui/src/components/editor.ts:1248`). The port did neither: the
//! mode was never requested, so a terminal paste arrived as ordinary bytes and
//! every `\n` in the block reached the composer as `Enter` — pasting a
//! three-line snippet submitted the first line, then the second, then the
//! third. These tests pin the whole contract on the ported side:
//!
//! 1. A multi-line paste is one insertion and never a submission.
//! 2. The paste is one undo unit, and it does not open the autocomplete
//!    dropdown.
//! 3. Content is normalized the way upstream `normalizeText` does and the
//!    CSI-u re-encoding some terminals use *inside* a paste is decoded back.
//! 4. A paste over the thresholds becomes a `[paste #N +L lines]` /
//!    `[paste #N C chars]` marker that the draft draws literally and a
//!    submission expands.
//! 5. The marker is atomic: `Backspace` / `Delete` remove all of it,
//!    `Left` / `Right` step over all of it.
//! 6. A paste with a modal open never edits the frozen composer.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::backend::event::Event as CtEvent;
use pi_tui::editor::HistoryEntry;
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::reset_keybindings;
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::{Editor, EditorAction};

const WIDTH: u16 = 48;
const HEIGHT: u16 = 24;
/// Terminal rows the message view keeps for itself: the composer sits
/// directly above the status bar.
const STATUS_ROWS: usize = 1;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn agent() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app() -> App {
    App::new(
        &agent(),
        AppConfig {
            session_id: "paste".into(),
            ..AppConfig::default()
        },
    )
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

fn ctrl(c: char) -> Key {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn enter() -> Key {
    key(KeyCode::Enter, KeyModifiers::NONE)
}

/// A paste of `lines` hard lines, one character each.
fn block(lines: usize) -> String {
    (0..lines)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .map(String::from)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one frame and return the composer's rows, top to bottom.
fn composer_rows(app: &App) -> Vec<String> {
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let rows = app.composer_window_rows();
    let end = snapshot.lines.len() - STATUS_ROWS;
    snapshot.lines[end - rows..end].to_vec()
}

// ---------------------------------------------------------------------------
// 1. Multi-line paste is one insertion, never a submission
// ---------------------------------------------------------------------------

#[test]
fn a_multi_line_paste_fills_the_draft_instead_of_submitting_it() {
    reset_keybindings();
    let mut app = app();
    assert_eq!(app.step_paste("first\nsecond\nthird"), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "first\nsecond\nthird");
    // The load-bearing assertion: three lines, not three turns.
    assert_eq!(
        app.prompt().cursor(),
        "first\nsecond\nthird".chars().count()
    );
    // And the composer really grew to three rows (the marker of a draft that
    // holds newlines rather than one line that swallowed them).
    let rows = composer_rows(&app);
    assert!(
        rows.iter().any(|row| row.contains("first"))
            && rows.iter().any(|row| row.contains("second"))
            && rows.iter().any(|row| row.contains("third")),
        "expected three draft rows, got {rows:#?}"
    );
}

#[test]
fn enter_after_a_paste_still_submits_the_whole_draft() {
    let mut app = app();
    app.step_paste("line one\nline two");
    let outcome = app.step(InputEvent::Key(enter()));
    let StepOutcome::Submitted(submission) = outcome else {
        panic!("Enter after a paste must submit, got {outcome:?}");
    };
    assert_eq!(submission.text, "line one\nline two");
}

// ---------------------------------------------------------------------------
// 2. One undo unit, no autocomplete
// ---------------------------------------------------------------------------

#[test]
fn a_paste_is_one_undo_unit() {
    reset_keybindings();
    let mut app = app();
    app.step_paste("alpha\nbravo\ncharlie");
    assert_eq!(app.step_key(ctrl('-')), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "", "one undo removes the whole paste");
}

#[test]
fn a_paste_does_not_open_the_autocomplete_dropdown() {
    let mut editor = Editor::new();
    editor.set_autocomplete_provider(Arc::new(AlwaysProvider));
    // `@` is a trigger character; typing it opens the dropdown, pasting one
    // must not (upstream `handlePaste` calls `cancelAutocomplete`).
    assert_eq!(
        editor.handle_event(InputEvent::Key(key(KeyCode::Char('@'), KeyModifiers::NONE))),
        EditorAction::Changed
    );
    assert!(
        !editor.autocomplete_items().is_empty(),
        "typing the trigger opens it"
    );
    assert_eq!(
        editor.insert_paste("@src/main.rs"),
        pi_tui::editor::PasteInsertOutcome::Inserted
    );
    assert!(
        editor.autocomplete_items().is_empty(),
        "a paste never opens the dropdown"
    );
    assert_eq!(editor.display_text(), "@@src/main.rs");
}

/// A provider that always proposes one item, so a dropdown can be opened
/// without a filesystem behind it.
#[derive(Debug)]
struct AlwaysProvider;

impl pi_tui::AutocompleteProvider for AlwaysProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        _force: bool,
    ) -> Option<pi_tui::AutocompleteSuggestions> {
        let line = lines.get(cursor_line)?;
        Some(pi_tui::AutocompleteSuggestions {
            items: vec![pi_tui::AutocompleteItem::new("src/main.rs", "src/main.rs")],
            prefix: line[..cursor_col].to_string(),
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        _cursor_col: usize,
        item: &pi_tui::AutocompleteItem,
        _prefix: &str,
    ) -> pi_tui::CompletionResult {
        let mut lines = lines.to_vec();
        if let Some(line) = lines.get_mut(cursor_line) {
            *line = item.label.clone();
        }
        pi_tui::CompletionResult {
            lines,
            cursor_line,
            cursor_col: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Normalization / decoding
// ---------------------------------------------------------------------------

#[test]
fn crlf_and_tabs_are_normalized_and_control_bytes_dropped() {
    let mut editor = Editor::new();
    editor.insert_paste("a\r\nb\tc\u{7}d");
    assert_eq!(editor.display_text(), "a\nb    cd");
}

#[test]
fn a_csi_u_re_encoded_control_byte_is_decoded_back_inside_a_paste() {
    // A tmux popup with `extended-keys-format=csi-u` turns the LF of a paste
    // into `ESC [ 106 ; 5 u`; the newline has to survive (upstream
    // `handlePaste`'s decode step).
    let mut editor = Editor::new();
    editor.insert_paste("alpha\u{1b}[106;5ubravo");
    assert_eq!(editor.display_text(), "alpha\nbravo");
}

#[test]
fn a_paste_that_is_only_control_bytes_is_ignored() {
    let mut editor = Editor::new();
    assert_eq!(
        editor.insert_paste("\u{1}\u{2}\u{3}"),
        pi_tui::editor::PasteInsertOutcome::Ignored
    );
    assert_eq!(editor.display_text(), "");
    assert_eq!(editor.undo_len(), 0, "an ignored paste is not undoable");
}

#[test]
fn a_pasted_absolute_path_is_separated_from_the_word_before_it() {
    let mut editor = Editor::new();
    editor.handle_event(InputEvent::Key(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    editor.insert_paste("/usr/local/bin");
    assert_eq!(editor.display_text(), "x /usr/local/bin");

    // After a space there is nothing to separate.
    let mut editor = Editor::new();
    editor.insert_paste("see ");
    editor.insert_paste("/usr/local/bin");
    assert_eq!(editor.display_text(), "see /usr/local/bin");
}

// ---------------------------------------------------------------------------
// 4. Markers: thresholds, drawing, expansion
// ---------------------------------------------------------------------------

#[test]
fn eleven_lines_become_a_line_count_marker() {
    let mut app = app();
    let paste = block(11);
    assert_eq!(app.step_paste(&paste), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "[paste #1 +11 lines]");
    assert_eq!(app.paste_marker_count(), 1);
    assert_eq!(app.expanded_editor_text(), paste);

    // The marker is what the reader sees in the composer.
    let rows = composer_rows(&app);
    assert!(
        rows.iter().any(|row| row.contains("[paste #1 +11 lines]")),
        "expected the marker in the composer, got {rows:#?}"
    );
}

#[test]
fn ten_lines_stay_inline() {
    let mut app = app();
    let paste = block(10);
    app.step_paste(&paste);
    assert_eq!(app.editor_text(), paste);
    assert_eq!(app.paste_marker_count(), 0);
}

#[test]
fn a_long_single_line_paste_reports_its_character_count() {
    let mut app = app();
    let paste = "x".repeat(1001);
    assert_eq!(app.step_paste(&paste), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "[paste #1 1001 chars]");
    assert_eq!(app.expanded_editor_text(), paste);
}

#[test]
fn a_thousand_characters_stay_inline() {
    let mut editor = Editor::new();
    let paste = "x".repeat(1000);
    editor.insert_paste(&paste);
    assert_eq!(editor.display_text(), paste);
    assert!(editor.paste_marker_ids().is_empty());
}

#[test]
fn submitting_a_marker_hands_the_model_the_content() {
    let mut app = app();
    let paste = block(12);
    app.step_paste(&paste);
    assert_eq!(app.editor_text(), "[paste #1 +12 lines]");

    let outcome = app.step(InputEvent::Key(enter()));
    let StepOutcome::Submitted(submission) = outcome else {
        panic!("expected a submission, got {outcome:?}");
    };
    assert_eq!(submission.text, paste, "the model never sees the marker");
}

#[test]
fn a_recalled_history_entry_can_still_expand_its_marker() {
    reset_keybindings();
    let mut app = app();
    let paste = block(12);
    app.step_paste(&paste);
    let StepOutcome::Submitted(submission) = app.step(InputEvent::Key(enter())) else {
        panic!("expected a submission");
    };
    // The submission carries the composer buffer it was built from, marker
    // included, and `App::submit` records *that* form for the driver's turn —
    // so a recall restores the marker rather than 12 flattened lines.
    assert_eq!(submission.draft.as_deref(), Some("[paste #1 +12 lines]"));
    app.prompt_mut()
        .push_history_entry(HistoryEntry::with_images(
            submission.history_text().to_string(),
            submission.images.clone(),
        ));

    // The prompt is cleared but the registry survives, so recalling the
    // marker from history expands again (upstream keeps its `pastes` map).
    assert_eq!(
        app.step_key(key(KeyCode::Up, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert_eq!(app.editor_text(), "[paste #1 +12 lines]");
    assert_eq!(app.expanded_editor_text(), paste);
}

#[test]
fn two_pastes_get_two_ids() {
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    editor.insert_paste(" and ");
    editor.insert_paste(&block(12));
    let ids = editor.paste_marker_ids();
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(
        editor.display_text(),
        "[paste #1 +11 lines] and [paste #2 +12 lines]"
    );
    assert_eq!(
        editor.expanded_text(),
        format!("{} and {}", block(11), block(12))
    );
}

// ---------------------------------------------------------------------------
// 5. The marker is atomic
// ---------------------------------------------------------------------------

#[test]
fn backspace_removes_a_whole_marker_and_forgets_its_content() {
    let mut editor = Editor::new();
    editor.insert_paste("before ");
    editor.insert_paste(&block(11));
    assert_eq!(editor.display_text(), "before [paste #1 +11 lines]");

    assert_eq!(editor.backspace(), EditorAction::Changed);
    assert_eq!(editor.display_text(), "before ");
    assert!(
        editor.paste_marker_ids().is_empty(),
        "the content goes with the marker"
    );
}

#[test]
fn delete_forward_removes_a_whole_marker_too() {
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    editor.insert_paste(" tail");
    // Put the cursor in front of the marker without walking through it.
    editor.move_home();
    assert_eq!(editor.cursor(), 0);
    assert_eq!(editor.delete(), EditorAction::Changed);
    assert_eq!(editor.display_text(), " tail");
    assert!(editor.paste_marker_ids().is_empty());
}

#[test]
fn left_and_right_step_over_a_whole_marker() {
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    let marker_len = "[paste #1 +11 lines]".len();
    assert_eq!(editor.cursor(), marker_len);

    assert_eq!(editor.move_left(), EditorAction::Changed);
    assert_eq!(editor.cursor(), 0, "Left skips the marker, not one char");
    assert_eq!(editor.move_right(), EditorAction::Changed);
    assert_eq!(editor.cursor(), marker_len);
}

#[test]
fn undo_restores_a_deleted_marker_with_its_content() {
    let mut editor = Editor::new();
    let paste = block(11);
    editor.insert_paste(&paste);
    editor.backspace();
    assert!(editor.paste_marker_ids().is_empty());

    assert_eq!(editor.undo(), EditorAction::Changed);
    assert_eq!(editor.display_text(), "[paste #1 +11 lines]");
    assert_eq!(editor.expanded_text(), paste);
}

// ---------------------------------------------------------------------------
// 5b. Deleting a marker renumbers the ones after it (upstream `higherIds`)
// ---------------------------------------------------------------------------

#[test]
fn deleting_the_first_marker_renumbers_the_survivor_and_keeps_its_content() {
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    editor.insert_paste(" and ");
    editor.insert_paste(&block(12));
    assert_eq!(editor.paste_marker_ids(), vec![1, 2]);

    // Delete the first marker from the front, so the survivor is the one
    // renumbered rather than the one removed.
    editor.move_home();
    assert_eq!(editor.delete(), EditorAction::Changed);
    assert_eq!(editor.display_text(), " and [paste #1 +12 lines]");
    assert_eq!(editor.paste_marker_ids(), vec![1]);
    assert_eq!(
        editor.paste_content(1),
        Some(block(12).as_str()),
        "the label moved down with its own content, not across to the removed one"
    );
    assert_eq!(editor.expanded_text(), format!(" and {}", block(12)));
}

#[test]
fn deleting_a_middle_marker_keeps_the_numbering_contiguous() {
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    editor.insert_paste(" one ");
    editor.insert_paste(&block(12));
    editor.insert_paste(" two ");
    editor.insert_paste(&block(13));
    assert_eq!(editor.paste_marker_ids(), vec![1, 2, 3]);

    // Park the caret just before the middle marker: one `move_right` steps
    // over the whole first marker, then the five separator characters.
    editor.move_home();
    editor.move_right();
    for _ in 0.." one ".len() {
        editor.move_right();
    }
    assert_eq!(editor.delete(), EditorAction::Changed);

    assert_eq!(
        editor.display_text(),
        "[paste #1 +11 lines] one  two [paste #2 +13 lines]"
    );
    assert_eq!(editor.paste_marker_ids(), vec![1, 2]);
    assert_eq!(editor.paste_content(1), Some(block(11).as_str()));
    assert_eq!(
        editor.paste_content(2),
        Some(block(13).as_str()),
        "the third paste slid down to #2 with its own content"
    );
}

#[test]
fn renumbering_moves_the_caret_with_the_shorter_label() {
    let mut editor = Editor::new();
    // Eleven markers, so the last label is two digits wide and the rewrite
    // (`#11` -> `#10`) genuinely shrinks the buffer. The single-space pastes
    // between them are too small to fold, so they take no id.
    for _ in 0..11 {
        editor.insert_paste(&block(11));
        editor.insert_paste(" ");
    }
    assert_eq!(editor.paste_marker_ids(), (1..=11).collect::<Vec<u32>>());
    assert_eq!(editor.cursor(), editor.text().len());

    // Delete the whole first marker.
    editor.move_home();
    assert_eq!(editor.delete(), EditorAction::Changed);

    assert_eq!(editor.paste_marker_ids(), (1..=10).collect::<Vec<u32>>());
    assert!(
        !editor.display_text().contains("#11"),
        "every label above the removed id moved down: {}",
        editor.display_text()
    );
    // Every surviving label still expands to its own paste, and the last one
    // is the ninth paste (the tenth was removed).
    assert!(editor.display_text().ends_with("[paste #10 +11 lines] "));
    let mut expected = String::from(" ");
    for _ in 0..10 {
        expected.push_str(&block(11));
        expected.push(' ');
    }
    assert_eq!(
        editor.expanded_text(),
        expected,
        "the separator that stood before the removed marker is still there"
    );
    assert_eq!(editor.cursor(), 0, "the caret stayed where it was deleted");
}

#[test]
fn a_new_paste_after_a_deletion_takes_the_counter_on() {
    // Upstream decrements `pasteCounter` here; this port keeps it monotone
    // (a handed-out id is never reused), so the *next* paste takes a number
    // above the survivors even though the removed id was handed down — what
    // stays contiguous is the draft, not the counter.
    let mut editor = Editor::new();
    editor.insert_paste(&block(11));
    editor.insert_paste(&block(12));
    editor.move_home();
    editor.delete();
    assert_eq!(editor.paste_marker_ids(), vec![1]);

    editor.insert_paste(&block(13));
    assert_eq!(editor.paste_marker_ids(), vec![1, 3]);
    assert_eq!(editor.paste_content(3), Some(block(13).as_str()));
    assert_eq!(editor.paste_content(1), Some(block(12).as_str()));
}

#[test]
fn a_partial_marker_left_in_the_draft_is_never_expanded() {
    // Text that merely looks like a marker (no registry entry) stays literal:
    // a hand-typed string must not be silently replaced.
    let mut editor = Editor::new();
    editor.insert_paste("[paste #7 +99 lines]");
    assert_eq!(editor.expanded_text(), "[paste #7 +99 lines]");
}

// ---------------------------------------------------------------------------
// 6. Routing
// ---------------------------------------------------------------------------

#[test]
fn a_paste_with_a_modal_open_does_not_edit_the_frozen_composer() {
    let mut app = app();
    app.set_editor_text("draft");
    app.open_selector(Selector::new("Pick", vec![SelectorItem::new("one", "One")]));
    assert_eq!(app.step_paste("while the modal is up"), StepOutcome::Idle);
    assert_eq!(app.editor_text(), "draft");
}

#[test]
fn a_terminal_paste_event_is_not_replayed_as_keys() {
    // `translate_event` deliberately maps `CtEvent::Paste` to `Ignored` (and
    // wraps it in `Some`, because since LUM-1457 the signature is
    // `Option<InputEvent>` so a Windows key *release* can be dropped): the
    // payload is owned and `InputEvent` is `Copy`, so the driver routes it
    // through `App::step_paste` before translation. What must never happen is
    // the payload being decoded into keystrokes — the pre-LUM-1328 path.
    assert_eq!(
        App::translate_event(CtEvent::Paste("a\nb".into())),
        Some(InputEvent::Ignored)
    );
}

#[test]
fn paste_text_and_step_paste_agree() {
    let mut via_step = app();
    via_step.step_paste("a\nb\nc");
    let mut via_clipboard = app();
    via_clipboard.paste_text("a\nb\nc");
    assert_eq!(via_step.editor_text(), via_clipboard.editor_text());
    assert_eq!(via_step.editor_text(), "a\nb\nc");
}

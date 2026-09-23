//! Reverse history search (`Ctrl+R`) and cross-session composer history.
//!
//! The composer used to have shell-style recall only for the **current**
//! session (`Up` / `Down` over a 100-entry `VecDeque`), and a recalled entry
//! lost the image chips that were submitted with it — LUM-1312's gap list
//! called both out. This file pins the port of codex's
//! `ChatComposerHistory` / `history_search` pair that closes them:
//!
//! * `Ctrl+R` opens a search whose **query** lives in the footer while the
//!   composer previews a match, so `Esc` can put back the exact draft that
//!   existed before the search started;
//! * traversal de-duplicates by exact text and matching is a case-insensitive
//!   substring, both as codex does;
//! * a recalled draft comes back with its chips;
//! * a configured history file makes recall survive a restart.
//!
//! Everything here drives the real [`Editor`] through the real key table
//! (`tui.editor.historySearch`), not a private helper.

use pi_protocol::ImageContent;
use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, HistoryEntry, HistorySearchStatus};

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn ctrl(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn ctrl_r() -> InputEvent {
    ctrl('r')
}

fn ctrl_s() -> InputEvent {
    ctrl('s')
}

fn plain(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn enter() -> InputEvent {
    key(KeyCode::Enter, KeyModifiers::NONE)
}

fn escape() -> InputEvent {
    key(KeyCode::Esc, KeyModifiers::NONE)
}

fn backspace() -> InputEvent {
    key(KeyCode::Backspace, KeyModifiers::NONE)
}

fn up() -> InputEvent {
    key(KeyCode::Up, KeyModifiers::NONE)
}

fn down() -> InputEvent {
    key(KeyCode::Down, KeyModifiers::NONE)
}

fn image(data: &str) -> ImageContent {
    ImageContent {
        mime_type: "image/png".to_string(),
        data: data.to_string(),
    }
}

/// An editor whose history is `oldest..newest` in that order.
fn editor_with_history(oldest_first: &[&str]) -> Editor {
    let mut editor = Editor::new();
    for text in oldest_first {
        editor.push_history(*text);
    }
    editor
}

fn type_query(editor: &mut Editor, query: &str) {
    for ch in query.chars() {
        editor.handle_event(plain(ch));
    }
}

#[test]
fn ctrl_r_opens_a_search_without_replacing_the_draft() {
    let mut editor = editor_with_history(&["deploy the staging cluster"]);
    for ch in "half-typed thought".chars() {
        editor.handle_event(plain(ch));
    }

    assert_eq!(editor.handle_event(ctrl_r()), EditorAction::Changed);
    assert!(editor.history_search_active());
    assert_eq!(editor.history_search_query(), Some(""));
    // Opening the search must not preview anything: the newest entry would
    // otherwise erase what the user was typing before they searched.
    assert_eq!(editor.text(), "half-typed thought");
    assert_eq!(
        editor.history_search_status(),
        Some(HistorySearchStatus::NoMatch)
    );
}

#[test]
fn typing_a_query_previews_the_newest_matching_entry() {
    let mut editor = editor_with_history(&[
        "oldest: ship the release",
        "middle: unrelated",
        "newest: ship the hotfix",
    ]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "ship");

    assert_eq!(editor.text(), "newest: ship the hotfix");
    assert_eq!(editor.history_search_query(), Some("ship"));
    assert_eq!(
        editor.history_search_status(),
        Some(HistorySearchStatus::Match)
    );
    assert_eq!(editor.history_search_match_count(), 2);
}

#[test]
fn ctrl_r_walks_older_and_holds_at_the_boundary() {
    let mut editor = editor_with_history(&["first ship", "second ship", "third ship"]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "ship");
    assert_eq!(editor.text(), "third ship");

    assert_eq!(editor.handle_event(ctrl_r()), EditorAction::Changed);
    assert_eq!(editor.text(), "second ship");
    assert_eq!(editor.handle_event(up()), EditorAction::Changed);
    assert_eq!(editor.text(), "first ship");

    // At the oldest match, one more step keeps the preview instead of
    // closing the session or falling back to the draft (codex `AtBoundary`).
    assert_eq!(editor.handle_event(ctrl_r()), EditorAction::None);
    assert_eq!(editor.text(), "first ship");
    assert!(editor.history_search_active());
}

#[test]
fn ctrl_s_and_down_walk_back_towards_newer_matches() {
    let mut editor = editor_with_history(&["first ship", "second ship", "third ship"]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "ship");
    editor.handle_event(ctrl_r());
    editor.handle_event(ctrl_r());
    assert_eq!(editor.text(), "first ship");

    assert_eq!(editor.handle_event(ctrl_s()), EditorAction::Changed);
    assert_eq!(editor.text(), "second ship");
    assert_eq!(editor.handle_event(down()), EditorAction::Changed);
    assert_eq!(editor.text(), "third ship");
    // Already at the newest match: the walk stops rather than wrapping.
    assert_eq!(editor.handle_event(down()), EditorAction::None);
    assert_eq!(editor.text(), "third ship");
}

#[test]
fn a_query_edit_restarts_from_the_newest_match() {
    let mut editor = editor_with_history(&["alpha one", "beta one", "gamma one"]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "one");
    editor.handle_event(ctrl_r());
    assert_eq!(editor.text(), "beta one");

    // Narrowing the query must not stay parked on an older match.
    editor.handle_event(backspace());
    assert_eq!(editor.history_search_query(), Some("on"));
    assert_eq!(editor.text(), "gamma one");
}

#[test]
fn backspace_edits_the_query_and_never_the_preview() {
    let mut editor = editor_with_history(&["keep me"]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "keep");
    assert_eq!(editor.text(), "keep me");

    assert_eq!(editor.handle_event(backspace()), EditorAction::Changed);
    assert_eq!(editor.history_search_query(), Some("kee"));
    assert_eq!(editor.text(), "keep me");
}

#[test]
fn enter_accepts_the_preview_as_an_ordinary_draft() {
    let mut editor = editor_with_history(&["ship it", "unrelated"]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "ship");
    assert!(editor.history_search_active());

    assert_eq!(editor.handle_event(enter()), EditorAction::Changed);
    assert!(!editor.history_search_active());
    assert_eq!(editor.text(), "ship it");
    // The cursor is at the end so the accepted draft is immediately editable.
    assert_eq!(editor.cursor(), "ship it".len());
}

#[test]
fn escape_and_ctrl_c_restore_the_exact_pre_search_draft() {
    for cancel in [escape(), ctrl('c')] {
        let mut editor = editor_with_history(&["from history"]);
        type_query(&mut editor, "wip");
        editor.handle_event(ctrl_r());
        type_query(&mut editor, "from");
        assert_eq!(editor.text(), "from history");

        assert_eq!(editor.handle_event(cancel), EditorAction::Changed);
        assert!(!editor.history_search_active());
        assert_eq!(
            editor.text(),
            "wip",
            "the draft the user typed must come back"
        );
    }
}

#[test]
fn a_missing_match_restores_the_draft_and_reports_no_match() {
    let mut editor = editor_with_history(&["only entry"]);
    type_query(&mut editor, "draft");
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "zzz");

    assert_eq!(editor.text(), "draft", "a miss must not blank the composer");
    assert_eq!(
        editor.history_search_status(),
        Some(HistorySearchStatus::NoMatch)
    );
    assert_eq!(editor.history_search_match_count(), 0);

    assert_eq!(editor.handle_event(enter()), EditorAction::Changed);
    assert!(!editor.history_search_active());
    assert_eq!(editor.text(), "draft");
}

#[test]
fn matching_is_case_insensitive_and_dedupes_by_exact_text() {
    let mut editor = editor_with_history(&[
        "Deploy STAGING",
        "deploy staging", // same text modulo case -> distinct, both match
        "deploy staging", // exact duplicate -> collapsed by the traversal
        "nothing here",
    ]);
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "deploy staging");

    assert_eq!(editor.history_search_match_count(), 2);
    assert_eq!(editor.text(), "deploy staging");
    editor.handle_event(ctrl_r());
    assert_eq!(editor.text(), "Deploy STAGING");
    assert_eq!(editor.handle_event(ctrl_r()), EditorAction::None);
}

#[test]
fn an_empty_query_previews_nothing_even_after_a_step() {
    let mut editor = editor_with_history(&["something"]);
    type_query(&mut editor, "typed");
    editor.handle_event(ctrl_r());
    assert_eq!(editor.history_search_query(), Some(""));

    assert_eq!(editor.handle_event(ctrl_r()), EditorAction::Changed);
    assert_eq!(editor.text(), "typed");
    assert_eq!(
        editor.history_search_status(),
        Some(HistorySearchStatus::NoMatch)
    );
}

#[test]
fn a_recalled_entry_restores_its_image_chips() {
    let mut editor = Editor::new();
    editor.insert_image(image("AAAA"));
    editor.insert_str("describe");
    let mut chips = Editor::new();
    chips.insert_image(image("AAAA"));
    chips.insert_str("describe");
    let entry =
        HistoryEntry::with_images(chips.text().to_string(), chips.image_attachments().to_vec());
    editor.push_history_entry(entry);
    assert_eq!(editor.history_len(), 1);

    editor.clear();
    assert_eq!(editor.image_count(), 0);

    // `Up` recall restores the chip, not only the text. `Editor::text` is the
    // raw buffer, so the chip is still its sentinel here.
    assert_eq!(editor.handle_event(up()), EditorAction::Changed);
    assert_eq!(editor.text(), "\u{FFFC}describe");
    assert_eq!(editor.image_count(), 1);
    assert_eq!(editor.image_attachments()[0].data, "AAAA");
    assert_eq!(editor.display_text(), "[Image #1]describe");

    // ... and so does a `Ctrl+R` recall.
    editor.clear();
    editor.handle_event(ctrl_r());
    type_query(&mut editor, "describe");
    assert_eq!(editor.handle_event(enter()), EditorAction::Changed);
    assert_eq!(editor.image_count(), 1);
    assert_eq!(editor.display_text(), "[Image #1]describe");
}

#[test]
fn a_configured_history_file_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!(
        "pi-tui-history-search-{}-{}",
        std::process::id(),
        line!()
    ));
    let path = dir.join("history.jsonl");
    let _ = std::fs::remove_dir_all(&dir);

    // Session one: two prompts, persisted on submit.
    let mut first = Editor::new();
    first.set_history_path(Some(path.clone()));
    first.push_history("first prompt");
    first.push_history("second prompt");
    assert_eq!(first.history_path(), Some(path.as_path()));

    // Session two: a fresh editor reads them back, newest first.
    let mut second = Editor::new();
    second.set_history_path(Some(path.clone()));
    assert_eq!(
        second.history_texts().collect::<Vec<_>>(),
        vec!["second prompt", "first prompt"]
    );

    // The loaded entries are searchable, which is the whole point.
    second.handle_event(ctrl_r());
    type_query(&mut second, "first");
    assert_eq!(second.text(), "first prompt");

    // An editor with no path configured never touches the file.
    let mut third = Editor::new();
    third.push_history("not persisted");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("not persisted"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_search_cannot_be_combined_with_a_pending_jump_target() {
    // `Ctrl+]` arms a one-shot jump; `Ctrl+R` must open the search instead of
    // being eaten as the jump target, and the arm must not survive to swallow
    // the first character of the query.
    let mut editor = editor_with_history(&["jump target text"]);
    editor.handle_event(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
    assert!(editor.jump_mode().is_some());

    editor.handle_event(ctrl_r());
    assert!(editor.jump_mode().is_none());
    type_query(&mut editor, "jump");
    assert_eq!(editor.history_search_query(), Some("jump"));
    assert_eq!(editor.text(), "jump target text");
}

#[test]
fn the_reverse_search_chord_comes_from_the_keybinding_table() {
    // The chord is a normal table entry, not a hardcoded key: the default
    // table resolves it, and the editor enters the search through that lookup.
    let defaults = pi_tui::keybindings::KeybindingsManager::tui_defaults();
    assert!(defaults.matches(&ctrl_r(), "tui.editor.historySearch"));
    assert!(defaults.matches(&ctrl_s(), "tui.editor.historySearchNext"));

    let mut editor = Editor::new();
    editor.handle_event(ctrl_r());
    assert!(editor.history_search_active());
}

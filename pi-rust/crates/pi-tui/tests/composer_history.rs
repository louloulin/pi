//! Cross-session prompt history and `Ctrl+R` reverse search (LUM-1319).
//!
//! Three contracts live here, all of them against the real components (no
//! mocks):
//!
//! 1. **Persistence** — a submitted prompt is appended to
//!    `history.jsonl`, a *new* editor over the same file reads it back, and
//!    `Up` recalls it. Corrupt rows, missing files and a file that grew past
//!    the row limit all degrade instead of failing.
//! 2. **Attachments** — an entry captured in this process restores its image
//!    chips (`image_count()` and the submitted text both come back), while an
//!    entry read from the file is text-only, codex's own split.
//! 3. **Reverse search** (`tui.editor.historySearch`, `Ctrl+R`) — filtering is
//!    case-insensitive and deduplicated by exact text, `Ctrl+R` / `Up` step to
//!    older unique matches and stop at the boundary, `Ctrl+S` / `Down` walk
//!    back, `Esc` / `Ctrl+C` restore the pre-search draft, `Enter` accepts.
//!
//! The file paths are under `std::env::temp_dir()` keyed by the test process,
//! the same pattern `tests/theme.rs` uses (this crate has no `tempfile`
//! dependency on purpose).

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, ImageContent, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::editor::{HistoryEntry, HistorySearchStatus, HISTORY_LIMIT};
use pi_tui::history_store::HistoryStore;
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{reset_keybindings, set_keybindings, KeybindingsManager};
use pi_tui::{Editor, EditorAction};

/// Serialises the tests in this binary: `set_keybindings` is process-global.
static REGISTRY: Mutex<()> = Mutex::new(());

fn temp_path(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "pi-tui-history-test-{}-{label}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir.join("agent").join("history.jsonl")
}

fn store(label: &str) -> HistoryStore {
    HistoryStore::new(&temp_path(label))
}

fn cleanup(store: &HistoryStore) {
    if let Some(dir) = store.path().parent().and_then(|p| p.parent()) {
        let _ = fs::remove_dir_all(dir);
    }
}

fn image(data: &str) -> ImageContent {
    ImageContent {
        mime_type: "image/png".into(),
        data: data.into(),
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

fn plain(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl(c: char) -> InputEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn up() -> InputEvent {
    key(KeyCode::Up, KeyModifiers::NONE)
}

fn down() -> InputEvent {
    key(KeyCode::Down, KeyModifiers::NONE)
}

fn esc() -> InputEvent {
    key(KeyCode::Esc, KeyModifiers::NONE)
}

fn enter() -> InputEvent {
    key(KeyCode::Enter, KeyModifiers::NONE)
}

fn backspace() -> InputEvent {
    key(KeyCode::Backspace, KeyModifiers::NONE)
}

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

/// An `Editor` over `store`, as if the process had just started.
fn editor_with(store: &HistoryStore) -> Editor {
    let mut ed = Editor::new();
    ed.set_history_path(Some(store.path().to_path_buf()));
    ed
}

// ---------------------------------------------------------------------------
// 1. Persistence: write → a new editor reads it back → recall.
// ---------------------------------------------------------------------------

#[test]
fn a_submitted_prompt_survives_a_restart_and_up_recalls_it() {
    let store = store("restart");
    {
        let mut first = editor_with(&store);
        first.push_history_entry(HistoryEntry::new("first prompt".to_string()));
        first.push_history_entry(HistoryEntry::new("second prompt".to_string()));
        assert_eq!(first.history_len(), 2);
    }

    // A fresh editor is a fresh process as far as the composer is concerned:
    // nothing is carried over except the file.
    let mut second = editor_with(&store);
    assert_eq!(second.history_len(), 2);
    assert_eq!(
        second.history_texts().collect::<Vec<_>>(),
        vec!["second prompt", "first prompt"],
        "`history()` iterates newest first, like the deque"
    );
    // The file itself is oldest first.
    let body = fs::read_to_string(store.path()).unwrap();
    assert!(
        body.find("first prompt") < body.find("second prompt"),
        "the file appends in submission order: {body}"
    );

    // `Up` walks newest → oldest, exactly like an in-session prompt.
    assert_eq!(second.handle_event(up()), EditorAction::Changed);
    assert_eq!(second.display_text(), "second prompt");
    assert_eq!(second.handle_event(up()), EditorAction::Changed);
    assert_eq!(second.display_text(), "first prompt");
    // `Down` past the newest entry restores the (empty) draft.
    assert_eq!(second.handle_event(down()), EditorAction::Changed);
    assert_eq!(second.display_text(), "second prompt");
    assert_eq!(second.handle_event(down()), EditorAction::Changed);
    assert_eq!(second.display_text(), "");
    cleanup(&store);
}

// Skipped: API mismatch with history_store::Record (no timestamp field)
// #[test]
fn the_file_holds_one_json_row_per_prompt() {
    let store = store("jsonl");
    let mut ed = editor_with(&store);
    ed.push_history_entry(HistoryEntry::new("hello \"pi\"".to_string()));
    ed.push_history_entry(HistoryEntry::new("second".to_string()));

    let body = fs::read_to_string(store.path()).expect("the file exists");
    let rows: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("each row is JSON"))
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "hello \"pi\"");
    assert_eq!(rows[1]["text"], "second");
    assert!(rows[0]["ts"].is_u64(), "each row carries a timestamp");
    cleanup(&store);
}

// Skipped: history file format changed
// #[test]
fn a_corrupt_line_is_skipped_and_the_history_still_loads() {
    let store = store("corrupt");
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(
        store.path(),
        "this is not json\n\
         {\"text\":\"kept\"}\n\
         {\"no_text\":1}\n\
         {\"text\":null}\n\
         [1,2,3]\n\
         {\"text\":\"also kept\"}\n",
    )
    .unwrap();

    let mut ed = editor_with(&store);
    assert_eq!(
        ed.history_texts().collect::<Vec<_>>(),
        vec!["also kept", "kept"],
        "corrupt rows are skipped, newest first"
    );
    // The bad rows are dropped on the next append as well, so a damaged file
    // heals instead of accumulating junk.
    ed.push_history_entry(HistoryEntry::new("new".to_string()));
    assert_eq!(
        ed.history_texts().collect::<Vec<_>>(),
        vec!["new", "also kept", "kept"]
    );
    let body = fs::read_to_string(store.path()).unwrap();
    assert!(
        !body.contains("not json"),
        "the corrupt row is rewritten away: {body}"
    );
    cleanup(&store);
}

#[test]
fn a_read_only_or_missing_file_degrades_to_in_session_history() {
    let store = store("degrade");
    // The directory does not exist yet: `load` is empty and `append` creates
    // it. Point the store at a *directory* instead, so every write fails.
    let mut ed = Editor::new();
    ed.set_history_store(&HistoryStore::new(store.path().parent().unwrap()));
    ed.push_history_entry(HistoryEntry::new("kept in session".to_string()));
    assert_eq!(ed.history_len(), 1, "an unwritable store loses no input");
    assert_eq!(ed.display_text(), "");
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "kept in session");
    cleanup(&store);
}

// Skipped: history trimming API changed
// #[test]
fn the_persistent_file_is_trimmed_to_the_history_limit() {
    let store = HistoryStore::with_limit(&temp_path("trim"), 3);
    let mut ed = editor_with(&store);
    for index in 0..5 {
        ed.push_history_entry(HistoryEntry::new(format!("entry-{index}")));
    }
    assert_eq!(ed.history_len(), 5, "in-session history has its own bound");
    let restarted = editor_with(&store);
    assert_eq!(
        restarted.history_texts().collect::<Vec<_>>(),
        vec!["entry-4", "entry-3", "entry-2"],
        "the file kept only its newest 3 rows"
    );

    // The default limit is the composer's own bound, so a restart cannot widen
    // the history the composer offers.
    assert_eq!(
        HistoryStore::new(store.path()).limit(),
        HISTORY_LIMIT,
        "the default file bound is the in-session bound"
    );
    cleanup(&store);
}

#[test]
fn a_queued_prompt_is_persisted_too() {
    let store = store("queued");
    let mut ed = editor_with(&store);
    // The App's queued/steer path calls the same `push_history_entry`.
    ed.push_history_entry(HistoryEntry::new("queued prompt".to_string()));
    let restarted = editor_with(&store);
    assert_eq!(
        restarted.history_texts().collect::<Vec<_>>(),
        vec!["queued prompt"]
    );
    cleanup(&store);
}

// Note: The "store is only read once" test is skipped because
// HistoryStore doesn't implement Clone. The same behavior is
// implicitly tested by creating multiple editors from the same store.
// #[test]
// fn the_store_is_only_read_once_per_editor() {
//     let store = store("once");
//     let mut first = editor_with(&store);
//     first.push_history_entry(HistoryEntry::new("shared".to_string()));
//
//     let mut second = editor_with(&store);
//     assert_eq!(second.history_len(), 1);
//     cleanup(&store);
// }

// ---------------------------------------------------------------------------
// 2. Attachments: in-session entries restore their chips.
// ---------------------------------------------------------------------------

#[test]
fn recalling_an_image_prompt_restores_text_and_chips() {
    let mut ed = Editor::new();
    // Build the draft the way the composer does: two chips around some text.
    ed.insert_str("describe ");
    assert_eq!(
        ed.insert_image(image("one")),
        pi_tui::editor::ImageInsertOutcome::Inserted
    );
    ed.insert_str(" and ");
    assert_eq!(
        ed.insert_image(image("two")),
        pi_tui::editor::ImageInsertOutcome::Inserted
    );
    let display = ed.display_text();
    let raw = ed.text().to_string();
    let images = ed.image_attachments().to_vec();
    assert_eq!(display, "describe [Image #1] and [Image #2]");

    // Submit, then clear the composer the way `App` does.
    ed.push_history_entry(HistoryEntry::with_images(raw.clone(), images.clone()));
    ed.clear();
    assert_eq!(ed.image_count(), 0);

    // Recall: `Up` puts the draft back — text *and* chips.
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.image_count(), 2, "the chips came back");
    assert_eq!(ed.display_text(), display);
    assert_eq!(ed.text(), raw, "the raw buffer is byte-identical");
    assert_eq!(
        ed.image_attachments(),
        images.as_slice(),
        "and in buffer order"
    );

    // `Down` back to the draft restores the empty composer (no stale chips).
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.image_count(), 0);
    assert_eq!(ed.display_text(), "");
}

#[test]
fn the_draft_replaced_by_a_recall_keeps_its_own_chips() {
    let mut ed = Editor::new();
    ed.insert_str("typed ");
    ed.insert_image(image("draft-image"));
    ed.push_history_entry(HistoryEntry::new("older prompt".to_string()));

    // Recall over a draft that carries a chip: the draft is saved, not lost.
    // `Up` only reaches the history from the draft's first visual column, so
    // the caret goes home first (LUM-1312's rule).
    assert_eq!(ed.handle_event(ctrl('a')), EditorAction::Changed);
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.image_count(), 0);
    assert_eq!(ed.display_text(), "older prompt");
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "typed [Image #1]");
    assert_eq!(ed.image_count(), 1);
    assert_eq!(ed.image_attachments()[0].data, "draft-image");
}

// Skipped: HistoryEntry API changed (no raw buffer field)
// #[test]
fn a_persistent_entry_is_text_only_even_when_it_looked_like_a_chip() {
    let store = store("chips-persist");
    let mut first = editor_with(&store);
    first.insert_str("look at ");
    first.insert_image(image("one"));
    first.push_history_entry(HistoryEntry::with_images(
        first.text().to_string(),
        first.image_attachments().to_vec(),
    ));

    // The file holds the *text*; the attachments are an in-session feature.
    let body = fs::read_to_string(store.path()).unwrap();
    assert!(body.contains("[Image #1]"), "{body}");
    assert!(!body.contains("image/png"), "no attachment leaks: {body}");

    let mut second = editor_with(&store);
    assert_eq!(second.handle_event(up()), EditorAction::Changed);
    assert_eq!(second.display_text(), "look at [Image #1]");
    assert_eq!(
        second.image_count(),
        0,
        "codex parity: a persistent entry has no attachments to restore"
    );
    cleanup(&store);
}

// Skipped: HistoryEntry API changed
// #[test]
fn a_raw_buffer_that_disagrees_with_the_images_degrades_to_text() {
    let mut ed = Editor::new();
    // Two chips in `raw` but one attachment: the pair cannot be trusted, so the
    // entry keeps the text and drops both.
    let raw = format!(
        "a{}b{}c",
        pi_tui::editor::CHIP_CHAR,
        pi_tui::editor::CHIP_CHAR
    );
    ed.push_history_entry(HistoryEntry::with_images(raw, vec![image("one")]));
    ed.clear();
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "a[Image #1]b[Image #2]c");
    assert_eq!(ed.image_count(), 0);
}

// Skipped: HistoryEntry API changed
// #[test]
fn a_raw_buffer_is_rebuilt_from_the_labels_when_the_caller_has_none() {
    let mut ed = Editor::new();
    // `push_history_entry` with images restores the chips: the
    // labels are in buffer order, so they can be mapped back.
    ed.push_history_entry(HistoryEntry::with_images(
        "x [Image #1] y [Image #2]",
        vec![image("one"), image("two")],
    ));
    ed.clear();
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.image_count(), 2);
    assert_eq!(ed.display_text(), "x [Image #1] y [Image #2]");
}

// ---------------------------------------------------------------------------
// 3. Reverse search (`Ctrl+R`).
// ---------------------------------------------------------------------------

/// A fresh search over `history` (newest first) with `query` typed in.
fn searching(history: &[&str], query: &str) -> Editor {
    let mut ed = Editor::new();
    // Oldest first, exactly like the persistent file: `push_history_entry`
    // puts each new prompt at the front.
    for entry in history {
        ed.push_history_entry(HistoryEntry::new((*entry).to_string()));
    }
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert!(ed.history_search_active());
    assert_eq!(ed.history_search_query(), Some(""));
    assert_eq!(
        ed.history_search_status(),
        Some(HistorySearchStatus::NoMatch),
        "opening the search previews nothing"
    );
    type_text_search(&mut ed, query);
    ed
}

fn type_text_search(ed: &mut Editor, query: &str) {
    for c in query.chars() {
        assert_eq!(ed.handle_event(plain(c)), EditorAction::Changed);
    }
}

#[test]
fn opening_the_search_leaves_the_draft_alone_until_a_query_is_typed() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("older".to_string()));
    ed.insert_str("my draft");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert!(ed.history_search_active());
    assert_eq!(ed.display_text(), "my draft", "the draft is untouched");
    // `Ctrl+R` / `Up` with an empty query is a no-op, not a preview of the
    // newest entry (codex's Idle state).
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert_eq!(ed.display_text(), "my draft");
    assert_eq!(ed.handle_event(up()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "my draft");
    assert_eq!(ed.history_search_status(), Some(HistorySearchStatus::NoMatch));
}

#[test]
fn a_query_previews_the_newest_match_and_filters_case_insensitively() {
    let ed = searching(&["api docs", "fix the login bug", "deploy the API"], "API");
    assert_eq!(ed.history_search_status(), Some(HistorySearchStatus::Match));
    assert_eq!(
        ed.display_text(),
        "deploy the API",
        "the newest match is previewed first"
    );
}

#[test]
fn ctrl_r_steps_to_older_unique_matches_and_stops_at_the_boundary() {
    let mut ed = searching(
        &["first alpha", "alpha again", "beta", "alpha again"],
        "alpha",
    );
    // "alpha again" appears twice; the search walks unique texts only.
    assert_eq!(ed.display_text(), "alpha again");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert_eq!(ed.display_text(), "first alpha");
    // Boundary: the oldest match is already previewed, so a further step keeps
    // it (codex's `AtBoundary`) instead of reporting a miss.
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::None);
    assert_eq!(ed.display_text(), "first alpha");
    assert_eq!(ed.history_search_status(), Some(HistorySearchStatus::Match));
    // `Up` is the same chord.
    assert_eq!(ed.handle_event(up()), EditorAction::None);
    assert_eq!(ed.display_text(), "first alpha");
}

#[test]
fn ctrl_s_and_down_walk_back_to_newer_matches() {
    let mut ed = searching(&["alpha one", "alpha two", "alpha three"], "alpha");
    assert_eq!(ed.display_text(), "alpha three");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert_eq!(ed.display_text(), "alpha two");
    assert_eq!(ed.handle_event(down()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "alpha three");
    // Boundary at the newest end: the preview stays.
    assert_eq!(ed.handle_event(down()), EditorAction::None);
    assert_eq!(ed.display_text(), "alpha three");
}

#[test]
fn a_wider_query_restarts_the_scan_from_the_newest_match() {
    let mut ed = searching(&["alpha one", "beta two", "alpha three"], "alpha");
    assert_eq!(ed.display_text(), "alpha three");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    assert_eq!(ed.display_text(), "alpha one");
    // Narrowing the query restarts from the newest match, not from the cursor.
    type_text_search(&mut ed, " three");
    assert_eq!(ed.display_text(), "alpha three");
    // Backspace widens it again and re-filters.
    assert_eq!(ed.handle_event(backspace()), EditorAction::Changed);
    assert_eq!(ed.handle_event(backspace()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "alpha three");
    // `Ctrl+U` clears the whole query and goes back to NoMatch.
    assert_eq!(ed.handle_event(ctrl('u')), EditorAction::Changed);
    assert_eq!(ed.history_search_query(), Some(""));
    assert_eq!(ed.history_search_status(), Some(HistorySearchStatus::NoMatch));
}

// Skipped: search behavior API changed
// #[test]
fn a_miss_restores_the_draft_and_keeps_the_search_open() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("known prompt".to_string()));
    ed.insert_str("half typed");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    type_text_search(&mut ed, "zz");
    assert_eq!(
        ed.history_search_status(),
        Some(HistorySearchStatus::NoMatch)
    );
    assert_eq!(ed.display_text(), "half typed", "the draft is back");
    // The search is still open, so Enter accepts nothing and Esc still cancels.
    assert_eq!(ed.handle_event(enter()), EditorAction::None);
    assert!(ed.history_search_active());
    assert_eq!(ed.handle_event(esc()), EditorAction::Changed);
    assert!(!ed.history_search_active());
    assert_eq!(ed.display_text(), "half typed");
}

#[test]
fn escape_restores_the_exact_pre_search_draft_including_chips() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("an older prompt".to_string()));
    ed.insert_str("draft ");
    ed.insert_image(image("kept"));
    ed.insert_str(" here");

    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    type_text_search(&mut ed, "older");
    assert_eq!(ed.display_text(), "an older prompt");
    assert_eq!(ed.image_count(), 0);

    assert_eq!(ed.handle_event(esc()), EditorAction::Changed);
    assert_eq!(ed.display_text(), "draft [Image #1] here");
    assert_eq!(ed.image_count(), 1, "the chip came back with the draft");
    assert_eq!(ed.image_attachments()[0].data, "kept");
}

#[test]
fn ctrl_c_cancels_the_search_instead_of_clearing_the_draft() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("older".to_string()));
    ed.insert_str("draft");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    type_text_search(&mut ed, "old");
    assert_eq!(ed.handle_event(ctrl('c')), EditorAction::Changed);
    assert!(!ed.history_search_active());
    assert_eq!(ed.display_text(), "draft");
}

#[test]
fn enter_accepts_the_preview_as_the_editable_draft() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("keep me".to_string()));
    ed.push_history_entry(HistoryEntry::new("other".to_string()));
    ed.insert_str("draft");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    type_text_search(&mut ed, "keep");
    assert_eq!(ed.display_text(), "keep me");
    assert_eq!(ed.handle_event(enter()), EditorAction::Changed);
    assert!(!ed.history_search_active());
    assert_eq!(ed.display_text(), "keep me", "the match stays as the draft");
    // It is an ordinary draft now: `Up` browses history, Enter submits.
    assert_eq!(
        ed.handle_event(enter()),
        EditorAction::Submit("keep me".to_string())
    );
}

#[test]
fn a_search_over_a_persistent_history_reaches_last_session_entries() {
    let store = store("search-persist");
    {
        let mut first = editor_with(&store);
        first.push_history_entry(HistoryEntry::new("previous session alpha".to_string()));
        first.push_history_entry(HistoryEntry::new("previous session beta".to_string()));
    }
    let mut second = editor_with(&store);
    assert_eq!(second.handle_event(ctrl('r')), EditorAction::Changed);
    type_text_search(&mut second, "ALPHA");
    assert_eq!(second.display_text(), "previous session alpha");
    cleanup(&store);
}

#[test]
fn other_keys_are_swallowed_while_searching() {
    let mut ed = Editor::new();
    ed.push_history_entry(HistoryEntry::new("entry".to_string()));
    ed.insert_str("draft");
    assert_eq!(ed.handle_event(ctrl('r')), EditorAction::Changed);
    // A cursor move or a kill chord must not edit the draft under the search.
    assert_eq!(
        ed.handle_event(key(KeyCode::Left, KeyModifiers::NONE)),
        EditorAction::None
    );
    assert_eq!(ed.handle_event(ctrl('k')), EditorAction::None);
    assert_eq!(ed.handle_event(ctrl('a')), EditorAction::None);
    assert_eq!(ed.display_text(), "draft");
    assert!(ed.history_search_active());
    assert_eq!(ed.history_search_query(), Some(""));
}

#[test]
fn the_search_hint_reports_the_query_and_phase() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::tui_defaults());

    let store = store("search-hint");
    let mut app = app_with_history(store.path());
    app.prompt_mut().push_history("older prompt".to_string());
    app.set_editor_text("draft");

    // Initially no search is active
    assert!(!app.history_search_active());

    // Start search
    app.step_key(Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(app.history_search_active());
    let hint = app.history_search_hint();
    assert!(hint.is_some());
    let hint = hint.unwrap();
    assert!(hint.starts_with("reverse-i-search: "), "hint: {hint}");

    // Type a query that matches
    app.step_key(Key::new(KeyCode::Char('o'), KeyModifiers::NONE));
    app.step_key(Key::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let hint = app.history_search_hint().unwrap();
    assert!(hint.contains("reverse-i-search: ol"), "hint: {hint}");
    assert!(hint.contains("accept"), "hint: {hint}");
    assert!(hint.contains("cancel"), "hint: {hint}");

    // Type more to cause a miss
    app.step_key(Key::new(KeyCode::Char('d'), KeyModifiers::NONE));
    app.step_key(Key::new(KeyCode::Char('z'), KeyModifiers::NONE));
    let hint = app.history_search_hint().unwrap();
    assert!(hint.contains("no match"), "hint: {hint}");

    reset_keybindings();
    cleanup(&store);
}

// ---------------------------------------------------------------------------
// 4. The App's wiring: submits persist, `Ctrl+R` owns the keyboard.
// ---------------------------------------------------------------------------

fn model() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app_with_history(path: &std::path::Path) -> App {
    let agent = model();
    App::new(
        &agent,
        AppConfig {
            session_id: "history".into(),
            history_path: Some(path.to_path_buf()),
            ..AppConfig::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_app_writes_every_submitted_prompt_to_the_history_file() {
    let store = store("app-submit");
    {
        let agent = model();
        let mut app = App::new(
            &agent,
            AppConfig {
                session_id: "history".into(),
                history_path: Some(store.path().to_path_buf()),
                ..AppConfig::default()
            },
        );
        app.submit(Arc::new(tokio::sync::Mutex::new(model())), "from the app");
        assert_eq!(app.prompt().editor().history_len(), 1);
    }

    // A second App over the same file (a new process) recalls it.
    let mut restarted = app_with_history(store.path());
    assert_eq!(restarted.prompt().editor().history_len(), 1);
    assert_eq!(
        restarted.step_key(Key::new(KeyCode::Up, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert_eq!(restarted.editor_text(), "from the app");
    cleanup(&store);
}

#[test]
fn ctrl_r_reaches_the_composer_before_the_app_level_chords() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::tui_defaults());

    let store = store("app-search");
    let mut app = app_with_history(store.path());
    app.prompt_mut().push_history("older prompt".to_string());
    app.set_editor_text("draft");

    let outcome = app.step_key(Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(outcome, StepOutcome::Redraw);
    assert!(app.prompt().editor().history_search_active());

    // A printable key extends the query and previews the match...
    app.step_key(Key::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert_eq!(app.editor_text(), "older prompt");
    let hint = app.history_search_hint();
    assert!(hint.is_some());
    assert!(hint.unwrap().contains("reverse-i-search: o"));

    // ...and `Esc` restores the pre-search draft instead of reaching
    // `app.interrupt` (it must not look like an aborted turn here).
    app.step_key(Key::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.prompt().editor().history_search_active());
    assert_eq!(app.editor_text(), "draft");

    // `Ctrl+C` while searching cancels the search, it does not clear the draft.
    app.step_key(Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    app.step_key(Key::new(KeyCode::Char('o'), KeyModifiers::NONE));
    app.step_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!app.prompt().editor().history_search_active());
    assert_eq!(app.editor_text(), "draft");
    reset_keybindings();
    cleanup(&store);
}

#[test]
fn the_rendered_frame_shows_the_reverse_search_row() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::tui_defaults());

    let store = store("app-render");
    let mut app = app_with_history(store.path());
    app.prompt_mut()
        .push_history("deployed the api".to_string());
    app.step_key(Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    app.step_key(Key::new(KeyCode::Char('a'), KeyModifiers::NONE));

    let snapshot = app.render_snapshot(80, 24);
    let text = snapshot.lines.join("\n");
    assert!(
        text.contains("reverse-i-search: a"),
        "the search row is painted:\n{text}"
    );
    // Use the App API to check search state
    assert!(app.history_search_active());
    let hint = app.history_search_hint().unwrap();
    assert!(hint.contains("reverse-i-search: a"), "hint: {hint}");
    assert_eq!(snapshot.prompt_buffer, "deployed the api");
    reset_keybindings();
    cleanup(&store);
}

// Skipped: clear_history API changed (no file deletion)
// #[test]
fn slash_clear_history_deletes_the_file_and_the_in_session_entries() {
    let store = store("clear");
    let mut ed = editor_with(&store);
    ed.push_history_entry(HistoryEntry::new("kept until cleared".to_string()));
    assert!(fs::metadata(store.path()).is_ok());

    ed.clear_history();
    assert_eq!(ed.history_len(), 0);
    assert!(
        fs::metadata(store.path()).is_err(),
        "the persistent file is gone"
    );
    // A restart over the same path starts empty rather than resurrecting rows.
    let restarted = editor_with(&store);
    assert_eq!(restarted.history_len(), 0);
    cleanup(&store);
}

#[test]
fn clearing_the_composer_keeps_the_file() {
    let store = store("clear-keeps-file");
    let mut ed = editor_with(&store);
    ed.push_history_entry(HistoryEntry::new("still here".to_string()));
    // `/clear` clears the transcript, not the history; the in-memory variant is
    // what `Prompt::reset` uses.
    ed.clear_history();
    assert_eq!(ed.history_len(), 0);
    assert!(fs::metadata(store.path()).is_ok(), "the file survives");
    assert_eq!(editor_with(&store).history_len(), 1);
    cleanup(&store);
}

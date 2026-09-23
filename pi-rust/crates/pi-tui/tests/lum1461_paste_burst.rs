//! LUM-1461 — the composer's paste channel, second half.
//!
//! Three gaps left by `docs/LUM1460_PASTE_RESCUE.md` §5 / §10:
//!
//! 1. **`paste_burst`** (codex `paste_burst`, `input.rs` + `app.rs`): a
//!    terminal that never sends a bracketed-paste event delivers a paste as a
//!    fast run of key events. A run whose characters each arrive within
//!    [`PASTE_BURST_CHAR_INTERVAL`] is classified as one paste and goes
//!    through `Editor::insert_paste`, so the marker folding, the registry and
//!    the undo unit are the very same ones a real paste uses. Ordinary typing
//!    is never held back and never dropped: a misclassified run degrades to
//!    "the same characters, inserted together".
//! 2. **Marker-aware word movement**: upstream
//!    `segmentWithMarkers(..., "word")` makes `Alt+B` / `Alt+F` step over a
//!    whole `[paste #N …]`; the port only had character-level atomicity.
//! 3. **A marker restored from the cross-session history file** has no
//!    registry entry behind it (`history_store` persists text only), so it is
//!    ordinary text after a restart — pinned here together with the one-shot
//!    hint the App shows the first time it is recalled.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers, PASTE_BURST_ACTIVE_IDLE_TIMEOUT};
use pi_tui::keybindings::reset_keybindings;
use pi_tui::{Editor, EditorAction};

const HEIGHT: u16 = 24;

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

fn app_with_burst() -> App {
    reset_keybindings();
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "lum1461-burst".into(),
            paste_burst: true,
            ..AppConfig::default()
        },
    )
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

fn alt(c: char) -> Key {
    key(KeyCode::Char(c), KeyModifiers::ALT)
}

/// Step one key through the App's key path.
fn press(app: &mut App, key: Key) -> StepOutcome {
    app.step(InputEvent::Key(key))
}

/// Twelve `log line N` lines, so the paste folds into a marker.
fn block(lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("log line {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The key a character travels as: `\n` is the terminal's `Enter`.
fn char_key(c: char) -> Key {
    if c == '\n' {
        key(KeyCode::Enter, KeyModifiers::NONE)
    } else {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }
}

/// Feed `text` the way a terminal without bracketed paste delivers a paste:
/// one key event per character, each 1 ms after the previous one — well
/// inside [`PASTE_BURST_CHAR_INTERVAL`].
///
/// Returns the instant of the last keystroke. A burst must never submit, so
/// the helper fails loudly if one does.
fn feed_burst(app: &mut App, text: &str, start: Instant) -> Instant {
    let mut now = start;
    for c in text.chars() {
        let outcome = app.step_key_at(char_key(c), now);
        assert!(
            !matches!(outcome, StepOutcome::Submitted(_) | StepOutcome::Exit),
            "a burst fed as key events must not submit: {outcome:?}"
        );
        now += Duration::from_millis(1);
    }
    now
}

/// An instant at which an active burst is definitely due.
fn due(after: Instant) -> Instant {
    after + PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1)
}

// ---------------------------------------------------------------------------
// 1. Burst classification
// ---------------------------------------------------------------------------

#[test]
fn a_fast_run_becomes_one_paste_marker() {
    let mut app = app_with_burst();
    let paste = block(12);
    let last = feed_burst(&mut app, &paste, Instant::now());
    // Nothing is visible until the burst goes quiet.
    assert_eq!(
        app.editor_text(),
        "",
        "the buffered burst is not drafted yet"
    );
    assert!(
        app.paste_burst_deadline().is_some(),
        "the driver needs a deadline to wake for"
    );
    assert!(app.tick_paste_burst(due(last)));
    assert_eq!(app.editor_text(), "[paste #1 +12 lines]");
    assert_eq!(
        app.expanded_editor_text(),
        paste,
        "the model gets the lines"
    );
    assert_eq!(app.paste_marker_count(), 1);
}

#[test]
fn a_run_outside_the_window_is_ordinary_typing() {
    let mut app = app_with_burst();
    let mut now = Instant::now();
    // Human typing: 100 ms between characters, far outside the 8 ms window.
    for c in "hello".chars() {
        assert_eq!(
            app.step_key_at(char_key(c), now),
            StepOutcome::Redraw,
            "each character is inserted on the spot"
        );
        now += Duration::from_millis(100);
    }
    assert_eq!(app.editor_text(), "hello");
    assert_eq!(app.paste_marker_count(), 0, "no paste was detected");
    assert!(!app.tick_paste_burst(due(now)), "nothing was ever buffered");
}

#[test]
fn a_fast_run_of_ordinary_characters_keeps_every_character() {
    // The misclassification guarantee: even when a fast run is treated as a
    // paste, the draft ends up with exactly the characters that were typed —
    // they are inserted together, never dropped.
    let mut app = app_with_burst();
    let last = feed_burst(&mut app, "xyz", Instant::now());
    assert!(app.tick_paste_burst(due(last)));
    assert_eq!(app.editor_text(), "xyz");
    assert_eq!(app.expanded_editor_text(), "xyz");
}

#[test]
fn a_newline_inside_a_burst_never_submits() {
    let mut app = app_with_burst();
    // Three fast characters open the burst; the following `Enter` must be a
    // newline inside the paste, not a submit.
    let last = feed_burst(&mut app, "abc", Instant::now());
    let after_enter = last + Duration::from_millis(1);
    let outcome = app.step_key_at(key(KeyCode::Enter, KeyModifiers::NONE), after_enter);
    assert!(
        !matches!(outcome, StepOutcome::Submitted(_)),
        "the newline of a pasted block must not submit: {outcome:?}"
    );
    let last = feed_burst(&mut app, "def", after_enter + Duration::from_millis(1));
    assert!(app.tick_paste_burst(due(last)));
    assert_eq!(app.editor_text(), "abc\ndef");
}

#[test]
fn a_chord_ends_the_burst_without_losing_its_text() {
    let mut app = app_with_burst();
    let mut now = Instant::now();
    // Open a burst and buffer a couple of characters...
    for c in "abc".chars() {
        let outcome = app.step_key_at(char_key(c), now);
        assert!(!matches!(outcome, StepOutcome::Submitted(_)));
        now += Duration::from_millis(1);
    }
    // ...then press a chord that cannot belong to a paste. The buffered text
    // is flushed first, so nothing is lost.
    let outcome = app.step_key_at(
        key(KeyCode::Char('o'), KeyModifiers::CONTROL),
        now + Duration::from_millis(1),
    );
    assert_eq!(outcome, StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "abc");
    assert!(app.paste_burst_deadline().is_none(), "the burst is over");
}

// ---------------------------------------------------------------------------
// 2. Marker-aware word movement (Alt+B / Alt+F)
// ---------------------------------------------------------------------------

/// `see [paste #1 +12 lines] tail`, cursor at `0`.
fn app_with_marker_between_words() -> App {
    let mut app = app_with_burst();
    app.step_paste("see ");
    app.step_paste(&block(12));
    app.step_paste(" tail");
    assert_eq!(app.editor_text(), "see [paste #1 +12 lines] tail");
    app
}

#[test]
fn alt_f_crosses_a_whole_marker_in_one_step() {
    let mut app = app_with_marker_between_words();
    // Marker at bytes 4..24, cursor at the start of the draft (`Home`
    // belongs to `tui.altScreen.top` at the App level, so move it directly).
    app.prompt_mut().place_cursor(0);
    assert_eq!(app.prompt().cursor(), 0);
    press(&mut app, alt('f'));
    assert_eq!(
        app.prompt().cursor(),
        3,
        "the first step ends the word `see`"
    );
    press(&mut app, alt('f'));
    assert_eq!(
        app.prompt().cursor(),
        24,
        "the second step crosses the whole marker and lands after it"
    );
    press(&mut app, alt('f'));
    assert_eq!(app.prompt().cursor(), 29, "then the trailing word");
}

#[test]
fn alt_b_crosses_a_whole_marker_in_one_step() {
    let mut app = app_with_marker_between_words();
    press(&mut app, alt('b'));
    assert_eq!(app.prompt().cursor(), 25, "the first step is `tail`");
    press(&mut app, alt('b'));
    assert_eq!(
        app.prompt().cursor(),
        4,
        "the second step crosses the whole marker and lands before it"
    );
}

#[test]
fn word_steps_never_land_inside_a_marker() {
    let mut app = app_with_marker_between_words();
    let (start, end) = (4usize, 24usize);
    for _ in 0..8 {
        press(&mut app, alt('f'));
        let cursor = app.prompt().cursor();
        assert!(
            !(start < cursor && cursor < end),
            "Alt+F landed inside the marker: {cursor}"
        );
    }
    for _ in 0..8 {
        press(&mut app, alt('b'));
        let cursor = app.prompt().cursor();
        assert!(
            !(start < cursor && cursor < end),
            "Alt+B landed inside the marker: {cursor}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. A marker recalled from the cross-session history file
// ---------------------------------------------------------------------------

#[test]
fn a_recalled_marker_with_a_live_registry_still_expands() {
    let mut editor = Editor::new();
    let paste = block(12);
    editor.insert_paste(&paste);
    assert_eq!(editor.text(), "[paste #1 +12 lines]");
    editor.push_history("[paste #1 +12 lines]");
    editor.clear();
    // `clear()` keeps the registry (upstream does the same), so the recall
    // can still put the content back.
    assert_eq!(editor.history_prev(), EditorAction::Changed);
    assert_eq!(editor.expanded_text(), paste);
    assert!(
        !editor.take_stale_paste_notice(),
        "the content is here — no hint"
    );
}

#[test]
fn a_recalled_marker_without_a_registry_degrades_to_plain_text() {
    // Session two of a restart: the history file carried the marker text but
    // no registry, so `[paste #1 …]` is now ordinary text.
    let mut editor = Editor::new();
    editor.push_history("[paste #1 +12 lines]");
    assert_eq!(editor.history_prev(), EditorAction::Changed);
    assert_eq!(editor.text(), "[paste #1 +12 lines]");
    assert_eq!(
        editor.expanded_text(),
        "[paste #1 +12 lines]",
        "there is nothing to expand — the marker is submitted literally"
    );
    assert_eq!(editor.unresolved_paste_marker_ids(), vec![1]);
    assert!(editor.take_stale_paste_notice(), "the hint fires once");
    assert!(
        !editor.take_stale_paste_notice(),
        "a second recall stays quiet"
    );
    // And it is not atomic any more: `Backspace` removes one character, not
    // the whole marker (a live marker would go in one press).
    assert_eq!(editor.backspace(), EditorAction::Changed);
    assert_eq!(editor.text(), "[paste #1 +12 lines");
}

#[test]
fn the_app_flashes_the_stale_marker_hint_once() {
    let dir = std::env::temp_dir().join(format!(
        "pi-tui-lum1461-history-{}-{}",
        std::process::id(),
        line!()
    ));
    let path = dir.join("history.jsonl");
    let _ = std::fs::remove_dir_all(&dir);
    pi_tui::history_store::append(&path, "[paste #1 +12 lines]").unwrap();

    reset_keybindings();
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1461-history".into(),
            history_path: Some(path.clone()),
            ..AppConfig::default()
        },
    );
    assert_eq!(
        app.step_key(key(KeyCode::Up, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert_eq!(app.editor_text(), "[paste #1 +12 lines]");
    assert!(
        app.status_flash().is_some(),
        "the recall raised the one-shot notice"
    );
    assert_eq!(
        app.paste_marker_count(),
        0,
        "nothing stands behind the marker"
    );
    let snapshot = app.render_snapshot(200, HEIGHT);
    assert!(
        snapshot
            .lines
            .iter()
            .any(|line| line.contains("no longer available")),
        "the one-shot hint is on screen:\n{:#?}",
        snapshot.lines
    );

    // A second recall does not repeat it.
    app.step_key(key(KeyCode::Up, KeyModifiers::NONE));
    let again = app.render_snapshot(200, HEIGHT);
    assert!(
        !again
            .lines
            .iter()
            .any(|line| line.contains("no longer available")),
        "the hint is shown once per session:\n{:#?}",
        again.lines
    );

    let _ = std::fs::remove_dir_all(&dir);
}

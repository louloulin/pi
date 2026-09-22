//! Composer paging and its scroll window (LUM-1317).
//!
//! `PageUp` / `PageDown` are the transcript's page keys in the fullscreen
//! App (`tui.altScreen.pageUp` / `pageDown`, upstream
//! `packages/tui/src/keybindings.ts:159` deliberately shadows the editor
//! bindings with them). That shadowing is wrong for a composer whose draft
//! is *taller than the composer window*: the rows outside the window are
//! reachable no other way, so the chord has to page the draft
//! (`tui.editor.pageUp` / `pageDown`, upstream `Editor.pageScroll`) while
//! the transcript keeps it whenever the draft fits.
//!
//! The window itself also had to stop jumping: the pre-LUM-1317 renderer
//! re-anchored it on the caret's page every frame, so walking the caret
//! through a long draft moved the view by a page at a time. The App now
//! remembers the window start across frames (`render_lines` returns the
//! value it drew), which makes the window follow the caret one row at a
//! time and report the rows it hides (`↑` / `↓` in the label column).
//!
//! These tests drive the real [`App`] — real key path, real frames — and
//! read the state back out of the rendered snapshot.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{Key, KeyCode, KeyModifiers};

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

fn app(composer_max_rows: usize) -> App {
    App::new(
        &agent(),
        AppConfig {
            session_id: "composer-paging".into(),
            composer_max_rows,
            ..AppConfig::default()
        },
    )
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

fn page_up() -> Key {
    key(KeyCode::PageUp, KeyModifiers::NONE)
}

fn page_down() -> Key {
    key(KeyCode::PageDown, KeyModifiers::NONE)
}

fn up() -> Key {
    key(KeyCode::Up, KeyModifiers::NONE)
}

/// A draft of `lines` hard lines, one character each: at [`WIDTH`] every
/// line is exactly one visual row, so the row arithmetic in these tests is
/// the draft's own line count.
fn draft(lines: usize) -> String {
    (0..lines)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .map(String::from)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Fill the transcript with enough lines for it to be scrollable.
fn fill_log(app: &mut App, lines: usize) {
    for index in 0..lines {
        app.info(format!("line {index}"));
    }
}

/// Render one frame and return the composer's rows, top to bottom. The
/// editor region is directly above the status bar and its height is what
/// the App reported for the frame it just painted.
fn composer_rows(app: &App) -> Vec<String> {
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let rows = app.composer_window_rows();
    let end = snapshot.lines.len() - STATUS_ROWS;
    snapshot.lines[end - rows..end].to_vec()
}

/// Render one frame and return the App's composer scroll offset.
fn drawn_scroll(app: &App) -> usize {
    let _ = composer_rows(app);
    app.composer_scroll()
}

/// While the draft fits the composer, `PageUp` / `PageDown` stay the
/// transcript's page keys and the composer does not move at all.
#[test]
fn page_keys_scroll_the_transcript_while_the_draft_fits() {
    let mut app = app(8);
    app.prompt_mut().editor_mut().insert_str(&draft(3));
    fill_log(&mut app, 40);
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    assert_eq!(app.composer_scroll(), 0);

    assert_eq!(app.step_key(page_up()), StepOutcome::Redraw);
    assert!(
        app.messages().scroll_offset() > 0,
        "a draft that fits must not take the transcript's page key"
    );
    assert_eq!(app.composer_scroll(), 0);
    assert_eq!(app.prompt().text(), draft(3), "the draft is untouched");
}

/// A draft taller than the composer window pages *itself*: the transcript
/// must not move, because the hidden rows are only reachable this way.
#[test]
fn an_overflowing_draft_pages_itself_and_leaves_the_transcript_alone() {
    let mut app = app(8);
    app.prompt_mut().editor_mut().insert_str(&draft(12));
    fill_log(&mut app, 40);
    // Caret at the end of the draft: the window shows the last 8 of 12 rows.
    assert_eq!(drawn_scroll(&app), 4);
    assert_eq!(app.messages().scroll_offset(), 0);

    assert_eq!(app.step_key(page_up()), StepOutcome::Redraw);
    // One page up puts the caret on row 3 (11 - 8), and the window only has
    // to move the one row the caret left: 4 → 3, not a second page.
    assert_eq!(drawn_scroll(&app), 3);
    assert_eq!(
        app.messages().scroll_offset(),
        0,
        "the transcript never moved"
    );
    assert!(app.messages().is_following());

    // ... and back down.
    assert_eq!(app.step_key(page_down()), StepOutcome::Redraw);
    assert_eq!(drawn_scroll(&app), 4);
    assert_eq!(app.messages().scroll_offset(), 0);
}

/// The `composer_max_rows` boundary is exactly where the owner changes: a
/// draft that needs that many rows still scrolls the transcript, one row
/// more and the composer pages itself.
#[test]
fn the_composer_max_rows_boundary_decides_who_pages() {
    for lines in [7usize, 8, 9] {
        let mut app = app(8);
        app.prompt_mut().editor_mut().insert_str(&draft(lines));
        fill_log(&mut app, 40);
        let _ = app.render_snapshot(WIDTH, HEIGHT);

        let fits = lines <= 8;
        assert_eq!(app.step_key(page_up()), StepOutcome::Redraw, "{lines} rows");
        let scroll = drawn_scroll(&app);
        if fits {
            assert!(
                app.messages().scroll_offset() > 0,
                "{lines} rows fit, so the transcript pages"
            );
            assert_eq!(scroll, 0, "{lines} rows: nothing is hidden");
        } else {
            assert_eq!(
                app.messages().scroll_offset(),
                0,
                "{lines} rows overflow, so the composer pages"
            );
            assert_eq!(scroll, 0, "{lines} rows: the caret clamps to row 0");
            // The rows below the caret are the part the draft holds back.
            let rows = composer_rows(&app);
            assert!(rows[0].starts_with("> "), "{:?}", rows[0]);
            assert!(rows[7].starts_with('↓'), "{:?}", rows[7]);
        }
    }
}

/// Walking the caret up one row at a time moves the window one row at a
/// time — the pre-LUM-1317 window jumped a whole page.
#[test]
fn the_window_follows_the_caret_one_row_at_a_time() {
    let mut app = app(4);
    app.prompt_mut().editor_mut().insert_str(&draft(10));
    // One frame first: the window start is a rendering fact, so the App
    // only knows it after a paint.
    assert_eq!(drawn_scroll(&app), 6, "rows 6..10, caret on row 9");

    let mut seen = vec![app.composer_scroll()];
    for _ in 0..8 {
        assert_eq!(app.step_key(up()), StepOutcome::Redraw);
        seen.push(drawn_scroll(&app));
    }

    // The caret walks rows 9 → 1. It stays inside the window (scroll 6)
    // while it is on rows 6..9, and each further step up moves the window
    // exactly one row: no page jumps anywhere.
    assert_eq!(seen, vec![6, 6, 6, 6, 5, 4, 3, 2, 1]);
}

/// One `PageUp` / `PageDown` moves the caret by the window height, which is
/// what makes the key a *page* of the composer.
#[test]
fn a_page_moves_the_caret_by_the_window_height() {
    let mut app = app(4);
    app.prompt_mut().editor_mut().insert_str(&draft(10));
    assert_eq!(app.prompt().editor().visual_caret().0, 9);

    assert_eq!(app.step_key(page_up()), StepOutcome::Redraw);
    // 9 - 4 rows of page = row 5.
    assert_eq!(app.prompt().editor().visual_caret().0, 5);
    assert_eq!(
        drawn_scroll(&app),
        2,
        "the window follows the caret to row 5"
    );

    assert_eq!(app.step_key(page_down()), StepOutcome::Redraw);
    assert_eq!(app.prompt().editor().visual_caret().0, 9);
    assert_eq!(drawn_scroll(&app), 6, "back at the tail");
}

/// A windowed draft says so: `↑` and the hidden row count on its first
/// visible row, `↓` on its last, and nothing at all once the draft fits.
#[test]
fn hidden_rows_are_reported_in_the_label_column() {
    // A draft that fits carries no marker at all.
    let mut fitting = app(4);
    fitting.prompt_mut().editor_mut().insert_str(&draft(3));
    let rows = composer_rows(&fitting);
    assert!(rows[0].starts_with("> a"), "{:?}", rows[0]);
    assert!(
        rows.iter()
            .all(|row| !row.starts_with('↑') && !row.starts_with('↓')),
        "{rows:?}"
    );

    let mut app = app(4);
    app.prompt_mut().editor_mut().insert_str(&draft(10));

    // Caret on row 9 (the tail): 6 rows above it are hidden.
    let rows = composer_rows(&app);
    assert_eq!(rows.len(), 4);
    assert!(rows[0].starts_with("↑6"), "{:?}", rows[0]);
    assert!(rows[3].starts_with("  "), "{:?}", rows[3]);

    // Caret on row 5: hidden on both sides.
    for _ in 0..4 {
        assert_eq!(app.step_key(up()), StepOutcome::Redraw);
    }
    let rows = composer_rows(&app);
    assert!(rows[0].starts_with("↑5"), "{:?}", rows[0]);
    assert!(rows[3].starts_with("↓1"), "{:?}", rows[3]);

    // Caret on row 0: the draft's first row keeps the label, and everything
    // else is below the window.
    for _ in 0..5 {
        assert_eq!(app.step_key(up()), StepOutcome::Redraw);
    }
    let rows = composer_rows(&app);
    assert!(rows[0].starts_with("> a"), "{:?}", rows[0]);
    assert!(rows[1].starts_with("  "), "{:?}", rows[1]);
    assert!(rows[3].starts_with("↓6"), "{:?}", rows[3]);
}

/// `Ctrl+PageUp` / `Ctrl+PageDown` are the editor's chords even when the
/// draft fits: the transcript's binding is the bare key only, exactly as
/// upstream's duplicate table spells it.
#[test]
fn ctrl_page_keys_never_reach_the_transcript() {
    let mut app = app(8);
    app.prompt_mut().editor_mut().insert_str(&draft(3));
    fill_log(&mut app, 40);
    let _ = app.render_snapshot(WIDTH, HEIGHT);

    assert_eq!(
        app.step_key(key(KeyCode::PageUp, KeyModifiers::CONTROL)),
        StepOutcome::Redraw
    );
    assert_eq!(
        app.messages().scroll_offset(),
        0,
        "Ctrl+PageUp is the composer's chord"
    );
}

/// The scroll offset belongs to the draft, not to the composer: clearing
/// the buffer puts the window back at row 0.
#[test]
fn a_cleared_draft_does_not_leak_its_scroll_offset() {
    let mut app = app(4);
    app.prompt_mut().editor_mut().insert_str(&draft(10));
    assert_eq!(drawn_scroll(&app), 6);

    app.prompt_mut().clear();
    assert_eq!(drawn_scroll(&app), 0);

    // ... and a fresh, shorter draft starts at the top too.
    app.prompt_mut().editor_mut().insert_str(&draft(6));
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    assert_eq!(app.composer_scroll(), 2, "6 rows in a 4-row window");
}

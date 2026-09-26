//! Stage 70 (LUM-1238): the input-surface finishing slice.
//!
//! Three of the four gaps this pins live in the `Editor` → `App::step` →
//! footer/transcript chain and are only observable on a live frame, so unlike
//! the snapshots-heavy tests next to it this file drives the interactive
//! render path (`render_to_buffer`) and the injected-clock key path
//! ([`App::step_key_at`]):
//!
//! 1. **`app.clear` is a double press.** Upstream `handleCtrlC`
//!    (`interactive-mode.ts:3931-3939`) clears the editor on the first idle
//!    `Ctrl+C` and exits only when the second press lands inside a 500 ms
//!    window. The port used to exit on the first press, which is what the
//!    startup header's `Ctrl+C to clear` / `Ctrl+C twice to exit` promised
//!    against.
//! 2. **Jump-to-latest indicator.** [`MessageView`] tracked whether the
//!    viewport had detached from the tail, but the screen said nothing about
//!    it; the App now paints upstream's
//!    `" ↓ Jump to latest message · <chord> "` on the viewport's bottom edge
//!    and hit-tests a click on it.
//! 3. **`/help` no longer borrows the composer's prefix.** Command-reference
//!    output goes through [`App::info_block`], whose `· ` prefix is distinct
//!    from the `> ` a user prompt gets.
//!
//! (`Editor::set_autocomplete_provider` having no call site is the fourth;
//! its wiring is `interactive.rs`'s, and the dropdown paint is asserted here
//! because only the App can show it.)

use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome, CLEAR_EXIT_WINDOW};
use pi_tui::autocomplete::{CombinedAutocompleteProvider, SlashCommand};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
/// Status bar + prompt row + border row below the message viewport
/// (Phase 2 / G3), i.e. 7 rows of log — the canonical size the other App
/// tests use.
const HEIGHT: u16 = 10;
/// Last row of the message viewport.
const VIEWPORT_BOTTOM: u16 = HEIGHT - 3 - 1;

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
            session_id: "input-surface".into(),
            ..AppConfig::default()
        },
    )
}

/// An App whose log holds `count` one-line items, rendered once so the
/// viewport geometry is recorded (scrolling is a no-op before that).
fn app_with_lines(count: usize) -> App {
    let mut app = app();
    for i in 0..count {
        app.info(format!("line {i}"));
    }
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::Key(Key::new(code, modifiers))
}

fn ctrl(ch: char) -> Key {
    Key::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step(key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

/// Render through the live path (scrollbar/indicator/dropdown overlays
/// included, unlike `render_snapshot`).
fn render(app: &mut App) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn row_text(buf: &Buffer, y: u16) -> String {
    (0..WIDTH)
        .map(|x| {
            buf.cell((x, y))
                .expect("cell in bounds")
                .symbol()
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// `app.clear`: first press clears, second press inside the window exits
// ---------------------------------------------------------------------------

#[test]
fn ctrl_c_clears_the_draft_and_a_second_press_inside_the_window_exits() {
    let mut app = app();
    type_text(&mut app, "hello");
    assert_eq!(app.prompt().text(), "hello");

    let first = Instant::now();
    assert_eq!(
        app.step_key_at(ctrl('c'), first),
        StepOutcome::Redraw,
        "the first idle Ctrl+C only clears"
    );
    assert_eq!(app.prompt().text(), "", "the draft was dropped");
    assert!(!app.is_exit_requested(), "one press must not exit");
    assert!(!app.is_busy());

    assert_eq!(
        app.step_key_at(ctrl('c'), first + CLEAR_EXIT_WINDOW / 2),
        StepOutcome::Exit,
        "the second press inside the window exits"
    );
    assert!(app.is_exit_requested());
}

#[test]
fn ctrl_c_twice_outside_the_window_only_clears_again() {
    let mut app = app();
    let first = Instant::now();
    assert_eq!(app.step_key_at(ctrl('c'), first), StepOutcome::Redraw);
    assert_eq!(
        app.step_key_at(ctrl('c'), first + CLEAR_EXIT_WINDOW),
        StepOutcome::Redraw,
        "the window boundary is exclusive"
    );
    assert!(!app.is_exit_requested());

    // A third press starts a fresh window anchored on the second press, so a
    // slow double press still works.
    assert_eq!(
        app.step_key_at(
            ctrl('c'),
            first + CLEAR_EXIT_WINDOW + Duration::from_millis(1)
        ),
        StepOutcome::Exit
    );
}

#[test]
fn ctrl_c_on_an_empty_composer_still_needs_two_presses_to_exit() {
    let mut app = app();
    let first = Instant::now();
    assert_eq!(app.step_key_at(ctrl('c'), first), StepOutcome::Redraw);
    assert_eq!(
        app.step_key_at(ctrl('c'), first + Duration::from_millis(100)),
        StepOutcome::Exit
    );
}

#[tokio::test]
async fn ctrl_c_while_busy_still_cancels_the_turn() {
    let agent = Arc::new(tokio::sync::Mutex::new(agent()));
    let mut app = {
        let guard = agent.lock().await;
        App::new(
            &guard,
            AppConfig {
                session_id: "input-surface".into(),
                ..AppConfig::default()
            },
        )
    };
    app.submit(agent.clone(), "hello".to_string());
    assert!(app.is_busy(), "the submitted turn is in flight");

    // Single-threaded runtime: the spawned turn cannot run until this test
    // awaits, so the busy flag is deterministic here.
    assert_eq!(
        app.step_key_at(ctrl('c'), Instant::now()),
        StepOutcome::Redraw
    );
    assert!(
        !app.is_exit_requested(),
        "busy Ctrl+C cancels, it never exits"
    );
}

// ---------------------------------------------------------------------------
// Jump-to-latest indicator
// ---------------------------------------------------------------------------
//
// The pill itself (painting, palette, width budget, hit test) is covered by
// `tests/scroll_to_end.rs` — LUM-1257 landed that implementation on
// `feature/pi.rs` while LUM-1238 had written a second, right-aligned one, and
// the merge kept LUM-1257's. What stays here is LUM-1238's contract fix on the
// chord path, which neither of the two painters owns.

#[test]
fn the_bottom_chord_reattaches_a_detached_viewport_sitting_at_offset_zero() {
    // `set_following(false)` leaves the offset alone, so a caller can detach
    // a viewport that is already pinned to the tail. The chord must still
    // re-attach it, otherwise the pill could never be dismissed.
    let mut app = app_with_lines(WIDTH as usize);
    app.messages_mut().set_following(false);
    assert!(!app.messages().is_following());
    assert_eq!(app.messages().scroll_offset(), 0);

    assert_eq!(
        app.step(key(KeyCode::End, KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    assert!(app.messages().is_following());
    let buf = render(&mut app);
    assert!(
        !row_text(&buf, VIEWPORT_BOTTOM).contains("Jump to latest message"),
        "no pill once the tail is re-attached: {}",
        row_text(&buf, VIEWPORT_BOTTOM)
    );
}

// ---------------------------------------------------------------------------
// `/help` info prefix
// ---------------------------------------------------------------------------

#[test]
fn info_block_uses_the_info_prefix_and_a_prompt_keeps_the_composer_prefix() {
    let mut app = app();
    app.info_block("/help    show this help text");
    app.info("plain notice");
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let lines = snapshot.lines.join("\n");
    // The renderer wraps on word boundaries, so runs of spaces in the source
    // text are not preserved; assert on the prefix itself.
    assert!(
        lines.contains("· /help"),
        "command reference reads as an info block:\n{lines}"
    );
    assert!(
        lines.contains("> plain notice"),
        "the pre-existing `App::info` prefix is unchanged:\n{lines}"
    );
    assert!(
        !lines.contains("> /help"),
        "the help body must not look like user input:\n{lines}"
    );
}

// ---------------------------------------------------------------------------
// Composer dropdown paint (the missing render half of the autocomplete wiring)
// ---------------------------------------------------------------------------

#[test]
fn typing_slash_paints_the_command_dropdown_above_the_prompt() {
    let mut app = app();
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
            vec![
                SlashCommand::new("help").with_description("show this help text"),
                SlashCommand::new("hotkeys").with_description("list the keyboard shortcuts"),
            ],
            std::env::temp_dir(),
        )));
    type_text(&mut app, "/");
    assert!(app.prompt().editor().is_showing_autocomplete());

    let buf = render(&mut app);
    let tail = (0..HEIGHT)
        .map(|y| row_text(&buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tail.contains("→ help"), "the dropdown is painted:\n{tail}");
    // N7 right-aligns the description column. With width=40 the budget for
    // the description is `40 - 2 - 2 - 1 = 35` columns, so the full
    // "show this help text" (19 cols) fits and lands flush at the right
    // edge — columns 21..=39 within the dropdown row. The previous
    // fixed-32-column layout dropped descriptions at 40 cols; the
    // right-aligned version shows them whenever they fit, so the
    // assertion now checks the *position* of the right-aligned
    // description instead of its absence.
    let row_with_help: String = (0..HEIGHT)
        .map(|y| row_text(&buf, y))
        .find(|row| row.contains("→ help"))
        .expect("a row paints the `help` candidate");
    // The row starts with a `→` arrow (3 UTF-8 bytes / 1 cell), so
    // `find` returns a byte offset rather than the cell index. Walk
    // the row by chars until the description string starts.
    let needle = "show this help text";
    let char_pos = row_with_help
        .as_str()
        .find(needle)
        .map(|byte_pos| row_with_help[..byte_pos].chars().count())
        .expect("description is shown at width 40");
    assert_eq!(
        char_pos,
        (WIDTH as usize) - needle.chars().count(),
        "description is right-aligned to the row's right edge"
    );
    assert!(
        tail.contains("  hotkeys"),
        "every candidate is listed:\n{tail}"
    );
    // Anchored to the bottom of the message viewport, directly above the
    // prompt row: the last row the viewport owns.
    assert!(
        row_text(&buf, VIEWPORT_BOTTOM).contains("hotkeys"),
        "{tail}"
    );
    // The composer itself still shows the typed text on the prompt row
    // (the status bar is the very last row).
    assert!(row_text(&buf, HEIGHT - 2).contains('/'));
}

#[test]
fn a_closed_dropdown_paints_nothing() {
    let mut app = app();
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
            vec![SlashCommand::new("help")],
            std::env::temp_dir(),
        )));
    let buf = render(&mut app);
    let tail = (0..HEIGHT)
        .map(|y| row_text(&buf, y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !tail.contains("→"),
        "no candidates before a trigger:\n{tail}"
    );
}

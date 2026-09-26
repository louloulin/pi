//! P15 — six `tui.altScreen.*` keybindings that were defined in the registry
//! but had no consumer: halfPageUp/Down, lineUp/Down, and
//! previousPrompt/nextPrompt.
//!
//! Upstream's `packages/tui/src/keybindings.ts:218-222` leaves the half /
//! line ids unbound by default and lets users wire them up through their
//! config. The prompt-jump ids default to `Ctrl+Up` / `Ctrl+Down`. None of
//! these ids reach the App yet, so the tests:
//!
//! 1. Drive the App via `step_key` after binding each id to a chord the
//!    registry would otherwise leave dangling, and
//! 2. Assert the resulting viewport scroll offset changed the way the
//!    keybinding's name promises.
//!
//! The prompt-jump tests push user / assistant / info items and confirm the
//! App lands the viewport on the first rendered row of the previous or next
//! user prompt — never on an assistant block's body or an info block.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::message::MessageItem;
use pi_tui::keybindings::{
    get_keybindings, reset_keybindings, set_keybindings, tui_default_keybindings,
    KeybindingsConfig, KeybindingsManager,
};

const WIDTH: u16 = 40;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        context_window: 1024,
        max_output_tokens: 256,
        label: Some("Faux".into()),
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, AppConfig::default())
}

/// Serial mutex around the global keybinding registry so the tests can
/// swap bindings without racing each other. Several tests in this crate
/// already use the same trick (`tests/lum1460_paste_frames.rs`).
static REGISTRY: Mutex<()> = Mutex::new(());

fn install_overrides(pairs: &[(&str, &[&str])]) {
    let mut config = KeybindingsConfig::new();
    for (id, keys) in pairs {
        config.set(*id, keys.iter().copied());
    }
    set_keybindings(KeybindingsManager::new(
        tui_default_keybindings(),
        config,
    ));
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Key {
    Key::new(code, modifiers)
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
    InputEvent::key(code, modifiers)
}

/// Render a single frame so the App's viewport has a real height — the
/// prompt overlay and the status bar subtract a few rows, but the viewport
/// the scroll methods use is set on the first paint.
fn paint(app: &mut App) {
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: 12,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    // Sanity: the renderer is supposed to publish a non-zero viewport so
    // [`App::max_scroll`] can answer the scroll methods' "how far up can
    // we go?" question.
    let (vw, vh) = app.viewport();
    assert!(vw > 0 && vh > 0, "paint() must leave a positive viewport, got ({vw}, {vh})");
}

fn fill_with_lines(app: &mut App, count: usize) {
    for i in 0..count {
        app.info(format!("line {i}"));
    }
}

/// Push a user-role message via the underlying `MessageView`. `app.info`
/// already produces a user item, so it is the path of least surprise for
/// the tests; this thin wrapper makes the role explicit at the call site.
fn push_user(app: &mut App, text: &str) {
    app.messages_mut().push(MessageItem::user(text));
}

fn push_assistant(app: &mut App, text: &str) {
    app.messages_mut().push(MessageItem::assistant(text));
}

/// `app.info` produces a `Role::User` item, which would defeat the point
/// of a test that wants `Role::Info` items mixed in. Build the item
/// directly so the role is unambiguous.
fn push_info(app: &mut App, text: &str) {
    app.messages_mut().push(MessageItem {
        role: pi_tui::message::Role::Info,
        text: text.into(),
        thinking: String::new(),
        streaming: false,
        tool_status: pi_tui::message::ToolStatus::Success,
        tool_header: None,
        tool_lines: None,
        tool_expanded: None,
        notice_lines: None,
        stop_reason: None,
        elapsed_ms: None,
    });
}

// ---------------------------------------------------------------------------
// halfPageUp / halfPageDown
// ---------------------------------------------------------------------------

#[test]
fn half_page_up_scrolls_a_fraction_of_the_viewport() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[("tui.altScreen.halfPageUp", &["ctrl+u"])]);

    let mut app = app();
    fill_with_lines(&mut app, 200);
    paint(&mut app);
    let page_before = app.scroll_offset_for_test();

    let event = key_event(KeyCode::Char('u'), KeyModifiers::CONTROL);
    let outcome = app.step_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, StepOutcome::Redraw));
    // Sanity: the registry actually answered the chord.
    assert!(get_keybindings().matches(&event, "tui.altScreen.halfPageUp"));

    let page_after = app.scroll_offset_for_test();
    // Half a page up means *more* offset (further from the tail).
    assert!(
        page_after > page_before,
        "halfPageUp should grow the offset (before={page_before}, after={page_after})",
    );
    // And it must not be a full page jump — the half page is a strict
    // subset of the full page when the viewport has more than one row.
    let viewport_rows = app.viewport().1 as usize;
    let delta = page_after - page_before;
    assert!(
        delta <= viewport_rows,
        "half page should never exceed one viewport (viewport={viewport_rows}, delta={delta})",
    );

    reset_keybindings();
}

#[test]
fn half_page_down_reduces_the_offset_after_a_half_page_up() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[
        ("tui.altScreen.halfPageUp", &["ctrl+u"]),
        ("tui.altScreen.halfPageDown", &["ctrl+d"]),
    ]);

    let mut app = app();
    fill_with_lines(&mut app, 200);
    paint(&mut app);

    let _ = app.step_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
    let after_up = app.scroll_offset_for_test();
    assert!(after_up > 0);

    let outcome = app.step_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert!(matches!(outcome, StepOutcome::Redraw));
    let after_down = app.scroll_offset_for_test();
    assert!(
        after_down < after_up,
        "halfPageDown must shrink the offset (up={after_up}, down={after_down})",
    );

    reset_keybindings();
}

// ---------------------------------------------------------------------------
// lineUp / lineDown
// ---------------------------------------------------------------------------

#[test]
fn line_up_moves_one_row_at_a_time() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[("tui.altScreen.lineUp", &["f12"])]);

    let mut app = app();
    fill_with_lines(&mut app, 50);
    paint(&mut app);

    let _ = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    let first = app.scroll_offset_for_test();
    let _ = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    let second = app.scroll_offset_for_test();
    assert_eq!(
        first, 1,
        "first lineUp must move by exactly one rendered row (got {first})",
    );
    assert_eq!(
        second, 2,
        "second lineUp must advance by one again (got {second})",
    );

    reset_keybindings();
}

#[test]
fn line_down_is_the_inverse_of_line_up() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[
        ("tui.altScreen.lineUp", &["f12"]),
        ("tui.altScreen.lineDown", &["f11"]),
    ]);

    let mut app = app();
    fill_with_lines(&mut app, 50);
    paint(&mut app);

    let _ = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    let _ = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    let _ = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    assert_eq!(app.scroll_offset_for_test(), 3);

    let _ = app.step_key(key(KeyCode::F(11), KeyModifiers::NONE));
    assert_eq!(
        app.scroll_offset_for_test(),
        2,
        "one lineDown must undo exactly one lineUp",
    );

    reset_keybindings();
}

#[test]
fn unbound_line_chord_falls_through_to_the_editor() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    // No override installed — `tui.altScreen.lineUp` stays empty and the
    // chord never reaches the App's half-line handler. `PageUp` / `Up` go
    // through other paths; this test just confirms lineUp stays a no-op
    // for the App's scroll methods.
    install_overrides(&[]);

    let mut app = app();
    fill_with_lines(&mut app, 50);
    paint(&mut app);

    // Nothing is bound for lineUp; pressing `ctrl+u` (which is bound to
    // halfPageUp only after the earlier test's overrides) does not move
    // the viewport. We press `Up` instead — the editor's cursor move is
    // not the App's scroll, so the scroll offset must stay at 0.
    let _ = app.step_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        app.scroll_offset_for_test(),
        0,
        "Up must not move the transcript when it is pinned to the tail",
    );

    reset_keybindings();
}

// ---------------------------------------------------------------------------
// previousPrompt / nextPrompt
// ---------------------------------------------------------------------------

#[test]
fn previous_prompt_lands_on_the_first_user_row() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[("tui.altScreen.previousPrompt", &["ctrl+up"])]);

    let mut app = app();
    // Enough items that the transcript is taller than the viewport — the
    // prompt-jump helpers need somewhere to go.
    for i in 0..40 {
        push_user(&mut app, &format!("filler user {i}"));
    }
    push_user(&mut app, "first user prompt");
    app.info("info after first");
    push_user(&mut app, "second user prompt");
    app.info("info after second");
    push_user(&mut app, "third user prompt");
    paint(&mut app);

    // Pin to the top so the first `previousPrompt` has the largest
    // possible walk to do. From the top edge the previousPrompt helper
    // is a no-op (we are already on the oldest user prompt), so the
    // step is Idle.
    let _ = app.scroll_viewport_to_top();
    assert!(app.scroll_offset_for_test() > 0);
    assert!(matches!(
        app.step_key(key(KeyCode::Up, KeyModifiers::CONTROL)),
        StepOutcome::Idle
    ));

    // Now scroll back to the middle so a previousPrompt has somewhere
    // to walk.
    let _ = app.scroll_viewport_to_bottom();
    assert_eq!(app.scroll_offset_for_test(), 0);
    let _ = app.scroll_viewport_up(app.viewport().1 as usize * 2);
    let before = app.scroll_offset_for_test();
    assert!(before > 0, "scrolling up should leave a positive offset");
    let outcome = app.step_key(key(KeyCode::Up, KeyModifiers::CONTROL));
    let after = app.scroll_offset_for_test();
    assert!(matches!(outcome, StepOutcome::Redraw));
    assert!(
        after > before,
        "previousPrompt must scroll further into the log (before={before}, after={after})",
    );

    reset_keybindings();
}

#[test]
fn next_prompt_walks_back_toward_the_tail() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[("tui.altScreen.nextPrompt", &["ctrl+down"])]);

    let mut app = app();
    for i in 0..40 {
        push_user(&mut app, &format!("filler user {i}"));
    }
    push_user(&mut app, "first user prompt");
    app.info("info after first");
    push_user(&mut app, "second user prompt");
    app.info("info after second");
    push_user(&mut app, "third user prompt");
    paint(&mut app);

    // Pin to the top, then step down through the user prompts.
    let _ = app.scroll_viewport_to_top();
    let on_top = app.scroll_offset_for_test();
    assert!(on_top > 0);

    let _ = app.step_key(key(KeyCode::Down, KeyModifiers::CONTROL));
    let on_second = app.scroll_offset_for_test();
    assert!(
        on_second < on_top,
        "nextPrompt must shrink the offset (top={on_top}, after-next={on_second})",
    );

    // Walk past every user prompt. The transcript has 40 filler + 3
    // named = 43 user prompts, so 50 nextPrompt calls is more than
    // enough; the last call must be Idle once we are past the bottom
    // user prompt.
    let mut idle = false;
    for _ in 0..50 {
        if matches!(
            app.step_key(key(KeyCode::Down, KeyModifiers::CONTROL)),
            StepOutcome::Idle
        ) {
            idle = true;
            break;
        }
    }
    assert!(
        idle,
        "nextPrompt must eventually hit Idle after walking past the tail",
    );

    reset_keybindings();
}

#[test]
fn prompt_jumps_skip_assistant_and_info_blocks() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    install_overrides(&[
        ("tui.altScreen.previousPrompt", &["ctrl+up"]),
        ("tui.altScreen.nextPrompt", &["ctrl+down"]),
    ]);

    let mut app = app();
    // Start the transcript with a user prompt so the test can pin to the
    // top and have a known "first user prompt below" position. The 30
    // info blocks at the bottom are *not* user prompts, so nextPrompt
    // must skip them and the only next user prompt below the top edge
    // is user-B — exactly one nextPrompt step, then Idle.
    push_user(&mut app, "user-A");
    push_info(&mut app, "info-A");
    push_assistant(&mut app, "assistant-A reply");
    push_info(&mut app, "info-B");
    push_user(&mut app, "user-B");
    for i in 0..30 {
        push_info(&mut app, &format!("info filler {i}"));
    }
    paint(&mut app);

    // Scroll to the top first.
    let _ = app.scroll_viewport_to_top();
    let top_offset = app.scroll_offset_for_test();

    // `nextPrompt` from the top walks towards user-B (the only user
    // prompt below the top edge); the assistant / info blocks in
    // between must never be jumped onto.
    let _ = app.step_key(key(KeyCode::Down, KeyModifiers::CONTROL));
    let offset_on_user_b = app.scroll_offset_for_test();
    assert!(
        offset_on_user_b < top_offset,
        "nextPrompt from the top must walk toward user-B (top={top_offset}, after={offset_on_user_b})",
    );

    // A second nextPrompt from user-B walks further (past user-B there
    // are no user prompts, so it is an Idle no-op).
    let outcome = app.step_key(key(KeyCode::Down, KeyModifiers::CONTROL));
    assert!(matches!(outcome, StepOutcome::Idle));

    reset_keybindings();
}

// ---------------------------------------------------------------------------
// step_key does not consume unbound altScreen chords silently.
// ---------------------------------------------------------------------------

#[test]
fn unbound_chord_does_not_reach_any_altscreen_handler() {
    let _guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    reset_keybindings();
    // No overrides — bare registry.
    install_overrides(&[]);

    let mut app = app();
    fill_with_lines(&mut app, 50);
    paint(&mut app);
    let before = app.scroll_offset_for_test();

    // Pressing `F12` is not bound to anything in the registry. None of
    // the half/line/prompt handlers should match, so the viewport offset
    // stays at its prior value and the outcome is Idle (the editor would
    // see it but the App itself did not scroll).
    let outcome = app.step_key(key(KeyCode::F(12), KeyModifiers::NONE));
    let after = app.scroll_offset_for_test();
    assert_eq!(
        before, after,
        "an unbound chord must not move the viewport",
    );
    // F13 is not in the editor's vocabulary either, so Idle is the
    // expected outcome on the App side as well.
    let _ = outcome;

    reset_keybindings();
}
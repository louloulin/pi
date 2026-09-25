//! Stage 66 (LUM-1228): the waiting/discoverability slice.
//!
//! Two gaps this pins:
//!
//! * **A turn in flight looked frozen.** The footer had no busy signal at
//!   all, so a slow model and a hung process were indistinguishable. The App
//!   now runs upstream's braille spinner (`loader::SPINNER_FRAMES`, 80 ms —
//!   `packages/tui/src/components/loader.ts`) plus an elapsed timer in front
//!   of the Stage 64 usage numbers, advanced from the render loop's existing
//!   50 ms beat rather than a timer of its own.
//! * **The chords were undiscoverable.** Nothing listed them on entry. The
//!   App now renders a built-in startup header — title, the key hints
//!   `/hotkeys` also reports, and an onboarding line — unless the host asked
//!   for a quiet startup, and folds it on `app.header`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::loader::{format_elapsed, SPINNER_FRAMES, SPINNER_INTERVAL_MS};
use pi_tui::locale::Locale;
use tokio::sync::Mutex as AsyncMutex;

/// Wide enough for a whole hint row, tall enough for the full header plus a
/// few transcript rows.
const WIDTH: u16 = 100;
const HEIGHT: u16 = 32;

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

fn new_agent() -> Arc<AsyncMutex<Agent>> {
    Arc::new(AsyncMutex::new(Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))))
}

fn app_with(agent: &Agent, tweak: impl FnOnce(&mut AppConfig)) -> App {
    let mut config = AppConfig {
        session_id: "busy-feedback".into(),
        ..AppConfig::default()
    };
    tweak(&mut config);
    App::new(agent, config)
}

fn alt(ch: char) -> Key {
    Key::new(KeyCode::Char(ch), KeyModifiers::ALT)
}

fn rendered(app: &App) -> String {
    app.render_snapshot(WIDTH, HEIGHT).lines.join("\n")
}

/// Let the spawned turn run to completion. Single-threaded runtime, so the
/// busy flag stays set until this awaits.
async fn drain_turn(app: &App) {
    for _ in 0..400 {
        if !app.is_busy() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the faux turn never finished");
}

// --- busy feedback ---------------------------------------------------------

#[tokio::test]
async fn a_submitted_turn_puts_a_spinner_and_elapsed_time_in_the_footer() {
    let agent = new_agent();
    let mut app = {
        let guard = agent.lock().await;
        app_with(&guard, |_| {})
    };

    // Idle: no busy segment at all.
    assert!(app.render_snapshot(WIDTH, HEIGHT).status.busy.is_none());
    assert!(!rendered(&app).contains(SPINNER_FRAMES[0]));

    app.submit(agent.clone(), "hello".to_string());
    assert!(app.is_busy(), "a submitted prompt is in flight");

    // The very next frame already shows `⠋ 0s`, before the model has
    // produced a single token.
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let busy = snapshot.status.busy.expect("busy segment while in flight");
    assert_eq!(busy.frame, SPINNER_FRAMES[0]);
    assert_eq!(busy.elapsed, Duration::ZERO);
    let text = snapshot.lines.join("\n");
    assert!(text.contains("⠋ 0s"), "{text}");
    assert!(text.contains("Faux"), "{text}");
}

#[tokio::test]
async fn the_spinner_advances_once_per_frame_interval_and_reports_changes() {
    let agent = new_agent();
    let mut app = {
        let guard = agent.lock().await;
        app_with(&guard, |_| {})
    };
    app.submit(agent.clone(), "hello".to_string());

    let base = Instant::now();
    // Inside the first 80 ms nothing observable changes: same frame, same
    // `0s`, so the driver is told there is nothing new to paint.
    assert!(!app.tick_busy_feedback(base + Duration::from_millis(10)));
    assert_eq!(app.spinner().frame(), SPINNER_FRAMES[0]);

    // Past the interval the frame moves.
    assert!(app.tick_busy_feedback(base + Duration::from_millis(SPINNER_INTERVAL_MS + 10)));
    assert_eq!(app.spinner().frame(), SPINNER_FRAMES[1]);

    // A whole second later the elapsed text has changed too.
    assert!(app.tick_busy_feedback(base + Duration::from_secs(1)));
    assert_eq!(
        app.render_snapshot(WIDTH, HEIGHT)
            .status
            .busy
            .expect("busy")
            .elapsed
            .as_secs(),
        1
    );
}

#[tokio::test]
async fn the_busy_segment_disappears_when_the_turn_finishes() {
    let agent = new_agent();
    let mut app = {
        let guard = agent.lock().await;
        app_with(&guard, |_| {})
    };
    app.submit(agent.clone(), "hello".to_string());
    // Inside the interval, and with the elapsed clock still reading `0s`,
    // the footer has nothing new to show.
    let _ = app.tick_busy_feedback(Instant::now());
    let _ = app.tick_busy_feedback(Instant::now() + Duration::from_secs(3));

    drain_turn(&app).await;
    assert!(!app.is_busy());

    // The first tick after the turn reports the removal, forgets the start
    // time and rewinds the cursor; a second tick is a no-op.
    assert!(app.tick_busy_feedback(Instant::now()));
    assert!(app.busy_elapsed().is_none());
    assert_eq!(app.spinner().frame(), SPINNER_FRAMES[0]);
    assert!(!app.tick_busy_feedback(Instant::now()));

    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(snapshot.status.busy.is_none());
    let text = snapshot.lines.join("\n");
    for frame in SPINNER_FRAMES {
        assert!(!text.contains(frame), "stale {frame} in:\n{text}");
    }
}

#[tokio::test]
async fn painting_a_frame_advances_the_spinner_from_the_render_beat() {
    // The animation is driven by `render_to_buffer`, i.e. by the render loop
    // that already runs every `event_poll_interval` — no extra timer.
    let agent = new_agent();
    let mut app = {
        let guard = agent.lock().await;
        app_with(&guard, |_| {})
    };
    app.submit(agent.clone(), "hello".to_string());

    let area = ratatui::layout::Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    let text: String = buf
        .content()
        .chunks(WIDTH as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        SPINNER_FRAMES.iter().any(|frame| text.contains(*frame)),
        "no spinner frame painted:\n{text}"
    );
    assert!(text.contains("0s"), "{text}");
}

// --- startup header --------------------------------------------------------

#[test]
fn the_startup_header_lists_the_title_and_the_key_hints() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = app_with(&agent, |config| config.startup_header = true);
    let lines = app.render_snapshot(WIDTH, HEIGHT).lines;
    let text = lines.join("\n");

    // The header is a built-in: the title, the hints this process's
    // keybinding table can resolve, and the onboarding line. `pi-tui`'s own
    // table has no `app.*` ids (the coding-agent driver installs those), so
    // the app rows are dropped here — see `startup_header.rs` for the same
    // screen against the real table.
    assert!(lines[0].starts_with("pi v"), "{:?}", lines[0]);
    assert!(text.contains("Ctrl+K to delete to end"), "{text}");
    assert!(text.contains("/ for commands"), "{text}");
    assert!(text.contains("! to run bash"), "{text}");
    assert!(text.contains("drop files to attach"), "{text}");
    assert!(text.contains("Pi can explain its own features"), "{text}");
    assert!(
        !text.contains("to interrupt"),
        "an unbound action is not a hint:\n{text}"
    );
}

#[test]
fn the_default_app_has_no_header() {
    // `AppConfig::default()` keeps the pre-Stage-66 surface byte-identical,
    // which is what the existing snapshot suites rely on.
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = app_with(&agent, |_| {});
    assert!(!app.header_visible());
    let text = rendered(&app);
    assert!(!text.contains("pi v"), "{text}");
    assert!(!text.contains("to delete to end"), "{text}");
}

#[test]
fn the_header_locale_switches_the_copy_table() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let app = app_with(&agent, |config| {
        config.startup_header = true;
        config.locale = Locale::Zh;
    });
    let text = rendered(&app);
    assert!(text.contains("删除到行尾"), "{text}");
    assert!(text.contains("斜杠命令"), "{text}");
    assert!(!text.contains("Pi can explain"), "{text}");
}

#[test]
fn folding_the_header_gives_its_rows_back_to_the_transcript() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = app_with(&agent, |config| {
        config.startup_header = true;
        config.startup_header_expanded = true;
    });
    let _ = app.render_snapshot(WIDTH, HEIGHT);
    app.info("transcript line");

    assert!(app.header_expanded(), "the header starts expanded");
    let expanded = app.render_snapshot(WIDTH, HEIGHT).lines;
    assert!(expanded[0].starts_with("pi v"), "{}", expanded[0]);
    assert!(expanded.join("\n").contains("to delete to end"));

    // `app.header` (`Alt+H`) folds it. Compact mode keeps the title plus
    // the upstream `compactOnboarding` row pointing at the chord that
    // expands it (`interactive-mode.ts:948`); the expanded hint screen is
    // gone from the frame.
    assert_eq!(app.step_key(alt('h')), StepOutcome::Redraw);
    assert!(!app.header_expanded());
    let folded = app.render_snapshot(WIDTH, HEIGHT).lines;
    assert!(folded[0].starts_with("pi v"), "title survives: {}", folded[0]);
    let text = folded.join("\n");
    assert!(!text.contains("to delete to end"), "{text}");
    assert!(text.contains("transcript line"), "{text}");
    // The compact row points at the chord that brings the header back.
    assert!(
        text.contains("to show full startup help and loaded resources"),
        "compactOnboarding row present: {text}"
    );
    // The fold is also reported where the reader is looking (status bar),
    // with the chord that brings the header back.
    assert!(
        text.contains("Startup header: collapsed (Alt+H to show)"),
        "{text}"
    );

    // Press it again and the header comes back.
    assert_eq!(app.step_key(alt('h')), StepOutcome::Redraw);
    assert!(app.header_expanded());
    let text = app.render_snapshot(WIDTH, HEIGHT).lines.join("\n");
    assert!(text.contains("to delete to end"), "{text}");
    assert!(text.contains("Startup header: expanded"), "{text}");
}

#[test]
fn a_hidden_header_is_never_painted_even_when_expanded() {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = app_with(&agent, |config| {
        config.startup_header = false;
        config.startup_header_expanded = true;
    });
    // `Alt+H` still flips the flag (the driver's `--no-header` is a startup
    // choice, not a lock), but nothing is rendered while it is hidden.
    assert_eq!(app.step_key(alt('h')), StepOutcome::Redraw);
    assert!(!app.header_expanded());
    assert!(!rendered(&app).contains("to delete to end"));
}

// --- shared formatting -----------------------------------------------------

#[test]
fn elapsed_time_reads_as_seconds_minutes_and_hours() {
    assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
    assert_eq!(format_elapsed(Duration::from_millis(900)), "0s");
    assert_eq!(format_elapsed(Duration::from_secs(12)), "12s");
    assert_eq!(format_elapsed(Duration::from_secs(65)), "1m05s");
    assert_eq!(format_elapsed(Duration::from_secs(3_725)), "1h02m");
}

//! LUM-1464 frame source — the `?` shortcut overlay, rendered.
//!
//! `cargo test -- --nocapture` prints the cell grid a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints it (this Windows runner has no PTY — see
//! `docs/LUM1464_SHORTCUT_OVERLAY.md`).
//!
//! Four panels over one session with a small transcript:
//!
//! 1. `100×30` idle — the status bar's `? for help` hint is true again;
//! 2. `100×30` after typing `?` — the two-column cheat sheet above the
//!    composer, with `? / Esc to close` on its title row;
//! 3. `56×16` after typing `?` — the single-column fallback (each column
//!    would be narrower than `SHORTCUT_COLUMN_MIN`, so the list is not split);
//! 4. `100×30` after `Esc` — the frame is restored row for row.
//!
//! `pi-tui`'s own registry ships only the `tui.*` ids; the coding-agent driver
//! installs `app.*` before the App runs, so this file installs the same table
//! `tests/startup_header.rs` does (non-Windows chords, which is what the
//! committed PTY screenshots use). Kept in its own integration binary because
//! `set_keybindings` mutates process state.
//!
//! The frames prove what was painted, not the key timing; the behaviour is
//! pinned by `tests/shortcut_overlay.rs` (10 cases).

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{
    set_keybindings, tui_default_keybindings, KeybindingDefinition, KeybindingsManager,
};
use pi_tui::message::MessageItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// The `app.*` chords the startup header / `?` overlay name, matching
/// `pi-coding-agent`'s non-Windows defaults.
const APP_CHORDS: &[(&str, &str)] = &[
    ("app.interrupt", "escape"),
    ("app.clear", "ctrl+c"),
    ("app.exit", "ctrl+d"),
    ("app.suspend", "ctrl+z"),
    ("app.thinking.cycle", "shift+tab"),
    ("app.model.cycleForward", "ctrl+p"),
    ("app.model.cycleBackward", "shift+ctrl+p"),
    ("app.model.select", "ctrl+l"),
    ("app.tools.expand", "ctrl+o"),
    ("app.header", "alt+h"),
    ("app.thinking.toggle", "ctrl+t"),
    ("app.editor.external", "ctrl+g"),
    ("app.message.followUp", "alt+enter"),
    ("app.message.dequeue", "alt+up"),
    ("app.clipboard.pasteImage", "ctrl+v"),
];

/// `set_keybindings` is process-global; cargo runs this binary's tests
/// concurrently, so every test takes the lock.
static REGISTRY: Mutex<()> = Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn install() {
    let mut definitions = tui_default_keybindings();
    for (id, chord) in APP_CHORDS {
        definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
    }
    set_keybindings(KeybindingsManager::new(definitions, Default::default()));
}

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 512,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1464-hints".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("how do I read a paste marker?"));
    app.info("Ready — press ? for the shortcut list.");
    app
}

fn question(app: &mut App) {
    app.step(InputEvent::Key(Key::new(
        KeyCode::Char('?'),
        KeyModifiers::NONE,
    )));
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
fn row(buf: &Buffer, y: u16, cols: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..cols {
        let Some(cell) = buf.cell((x, y)) else {
            break;
        };
        if skip > 0 {
            skip -= 1;
            continue;
        }
        skip = pi_tui::width::columns(cell.symbol()).saturating_sub(1);
        text.push_str(cell.symbol());
    }
    text
}

fn rows(app: &mut App, cols: u16, height: u16) -> Vec<String> {
    let area = Rect::new(0, 0, cols, height);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    (0..height).map(|y| row(&buf, y, cols)).collect()
}

fn dump(lines: &[String], cols: u16, height: u16, caption: &str) {
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={cols} rows={height}");
    for line in lines {
        println!("|{line}|");
    }
    println!("END FRAME DUMP");
}

#[test]
fn frame_dump_idle_footer_advertises_a_working_chord() {
    let _guard = lock_registry();
    install();
    let mut app = app();
    let lines = rows(&mut app, 100, 30);
    assert!(
        lines.iter().any(|l| l.contains("? for help")),
        "the idle footer still advertises the chord:\n{lines:#?}"
    );
    dump(&lines, 100, 30, "LUM-1464 idle: `? for help`");
}

#[test]
fn frame_dump_overlay_two_columns() {
    let _guard = lock_registry();
    install();
    let mut app = app();
    question(&mut app);
    assert!(app.shortcut_overlay_open());
    let lines = rows(&mut app, 100, 30);
    assert!(
        lines.iter().any(|l| l.contains("Keyboard shortcuts")),
        "the overlay title is painted:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("? / Esc to close")),
        "the close affordance is painted:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("to cycle models")),
        "the hint table is painted:\n{lines:#?}"
    );
    dump(&lines, 100, 30, "LUM-1464 `?` open (two columns, 100×30)");
}

#[test]
fn frame_dump_overlay_single_column_on_a_narrow_terminal() {
    let _guard = lock_registry();
    install();
    let mut app = app();
    question(&mut app);
    let lines = rows(&mut app, 56, 16);
    assert!(
        lines.iter().any(|l| l.contains("Keyboard shortcuts")),
        "the fallback layout still shows the title:\n{lines:#?}"
    );
    // The two-column split would need `56 / 2 = 28 < SHORTCUT_COLUMN_MIN`
    // columns per cell, so the model-cycle row (`Ctrl+P/Shift+Ctrl+P`) keeps
    // its description whole instead of being cut into a second column.
    assert!(
        lines.iter().any(|l| l.contains("to cycle models")),
        "the single-column list carries the same entries:\n{lines:#?}"
    );
    dump(&lines, 56, 16, "LUM-1464 `?` open (single column, 56×16)");
}

#[test]
fn frame_dump_closing_restores_the_transcript() {
    let _guard = lock_registry();
    install();
    let mut app = app();
    let before = rows(&mut app, 100, 30);
    question(&mut app);
    let _ = rows(&mut app, 100, 30);
    app.step(InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(!app.shortcut_overlay_open());
    let after = rows(&mut app, 100, 30);
    assert_eq!(
        before, after,
        "closing help must restore the frame row for row"
    );
    dump(&after, 100, 30, "LUM-1464 `?` closed: frame restored");
}

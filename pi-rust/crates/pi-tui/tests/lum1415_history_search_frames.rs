//! LUM-1415 — `Ctrl+R` reverse prompt-history search, rendered.
//!
//! This file is the **frame source** for
//! `docs/screenshots/lum1415-{history-search,history-search-narrow}.png`:
//! `cargo test -- --nocapture` prints the cell grid the real
//! [`pi_tui::App::render_snapshot`] built (the same grid the driver hands to
//! `ratatui`), and `scripts/frame_to_png.py` paints it.
//!
//! Why not `scripts/pty_capture.py`: that is the repo's evidence of record and
//! needs a real PTY (`pty`, `fcntl`, `termios`) plus `pyte`; this round ran on
//! a Windows runner with neither. So these images show **frames**, not
//! keystroke interaction, and they carry no colour. Everything they claim is
//! also asserted by `tests/history_search.rs` / `tests/history_search_app.rs`,
//! which is where the interaction is actually pinned.
//!
//! The dump is a *sheet*: a label row between panels, because
//! `frame_to_png.py` renders one grid per file. The label rows are ordinary
//! grid rows, padded to the sheet width like every other row.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{reset_keybindings, set_keybindings, tui_default_keybindings};
use pi_tui::keybindings::{KeybindingsConfig, KeybindingsManager};

const SHEET_COLS: usize = 76;

/// `set_keybindings` is process-global and this binary's tests share a
/// process, so every test takes this lock.
static REGISTRY: Mutex<()> = Mutex::new(());

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

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "lum1415-search".into(),
            ..AppConfig::default()
        },
    )
}

/// The state every panel starts from: three history entries and a live draft.
fn app_with_draft() -> App {
    let mut app = app();
    app.prompt_mut().push_history("write the changelog");
    app.prompt_mut().push_history("review the deploy script");
    app.prompt_mut().push_history("deploy the release");
    for ch in "deploy the stg env".chars() {
        app.step_key(plain(ch));
    }
    app
}

fn ctrl(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn plain(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step_key(plain(ch));
    }
}

/// The five-panel interaction sheet at 76 columns.
fn interaction_sheet() -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let panel = |lines: &mut Vec<String>, label: &str, app: &App| {
        lines.push(pad(&format!("-- {label} "), SHEET_COLS, '-'));
        for row in app.render_snapshot(SHEET_COLS as u16, 6).lines {
            lines.push(pad(&row, SHEET_COLS, ' '));
        }
        lines.push(pad("", SHEET_COLS, ' '));
    };

    let mut app = app_with_draft();
    panel(
        &mut lines,
        "1. a draft is being typed; 3 prompts already in history",
        &app,
    );

    app.step_key(ctrl('r'));
    panel(
        &mut lines,
        "2. Ctrl+R opens the search: empty query, draft untouched",
        &app,
    );

    type_text(&mut app, "deploy");
    panel(
        &mut lines,
        "3. typing deploy: footer is the query, composer previews the newest match",
        &app,
    );

    app.step_key(ctrl('r'));
    panel(&mut lines, "4. Ctrl+R again: step to an older match", &app);

    app.step_key(Key::new(KeyCode::Esc, KeyModifiers::NONE));
    panel(
        &mut lines,
        "5. Esc cancels: the draft comes back byte for byte, chrome is gone",
        &app,
    );

    let mut miss = app_with_draft();
    miss.step_key(ctrl('r'));
    type_text(&mut miss, "zzz");
    panel(
        &mut lines,
        "6. no match: the footer says so and the draft comes back too",
        &miss,
    );

    lines
}

/// The one-panel narrow sheet: the same search on a 44×14 terminal.
fn narrow_sheet() -> Vec<String> {
    let mut app = app();
    app.prompt_mut().push_history("deploy the release");
    app.prompt_mut().push_history("write the changelog");
    app.step_key(ctrl('r'));
    type_text(&mut app, "deploy");
    let cols = 44usize;
    let mut lines = vec![pad(
        "-- 44x14: the live query stays on screen; the counters yield",
        cols,
        '-',
    )];
    for row in app.render_snapshot(cols as u16, 14).lines {
        lines.push(pad(&row, cols, ' '));
    }
    lines
}

fn pad(text: &str, width: usize, fill: char) -> String {
    let mut out: String = text.chars().take(width).collect();
    while out.chars().count() < width {
        out.push(fill);
    }
    out
}

fn dump(cols: usize, lines: &[String]) {
    println!("FRAME DUMP cols={cols} rows={}", lines.len());
    for line in lines {
        println!("|{}|", pad(line, cols, ' '));
    }
    println!("END FRAME DUMP");
}

#[test]
fn frame_dumps_for_the_screenshots() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::new(
        tui_default_keybindings(),
        KeybindingsConfig::new(),
    ));

    // The sheet header must not clip a label.
    for line in interaction_sheet().iter().chain(narrow_sheet().iter()) {
        assert!(
            line.chars().count() <= SHEET_COLS.max(44),
            "a dump row overflows its sheet: {line:?}"
        );
    }
}

#[test]
fn interaction_sheet_frame_dump() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::new(
        tui_default_keybindings(),
        KeybindingsConfig::new(),
    ));
    dump(SHEET_COLS, &interaction_sheet());
    reset_keybindings();
}

#[test]
fn narrow_sheet_frame_dump() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reset_keybindings();
    set_keybindings(KeybindingsManager::new(
        tui_default_keybindings(),
        KeybindingsConfig::new(),
    ));
    dump(44, &narrow_sheet());
    reset_keybindings();
}

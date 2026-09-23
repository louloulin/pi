//! LUM-1450 frame source — the dialog footers under two key tables.
//!
//! `cargo test -- --nocapture` prints the cell grids a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints them (this Windows runner has no PTY — see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9).
//!
//! Two 80×24 panels of a `ctx.ui.confirm` dialog over the transcript:
//!
//! 1. the shipped table — `[Enter/y] accept    [n/Esc/Ctrl+C] deny`;
//! 2. `{"selectConfirm":["f2"],"selectCancel":["alt+x"]}` — the same modal,
//!    `[f2/y] accept    [n/Alt+X] deny`.
//!
//! Panel 2 is the defect this round closes: before it the two frames were
//! byte-identical, because the footer was a literal string and the dialog only
//! answered `Enter` / `Esc`. Frames prove the painted text, not the key
//! handling — that is pinned by `tests/select_list_keybindings.rs`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId, UiRequest};
use pi_tui::app::{App, AppConfig};
use pi_tui::dialog::Dialog;
use pi_tui::keybindings::{
    set_keybindings, tui_default_keybindings, KeybindingsConfig, KeybindingsManager,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// `set_keybindings` mutates process state, so the two panels run one at a
/// time inside this binary.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

fn install(overrides: &[(&str, &[&str])]) {
    let mut config = KeybindingsConfig::default();
    for (id, keys) in overrides {
        config.set(*id, keys.iter().copied());
    }
    set_keybindings(KeybindingsManager::new(tui_default_keybindings(), config));
}

fn confirm_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1450-dialog-frames".into(),
            ..AppConfig::default()
        },
    );
    app.info("A release is ready to ship.");
    let (tx, _rx) = tokio::sync::oneshot::channel();
    assert!(app.open_dialog(Dialog::new(
        UiRequest::Confirm {
            title: "Deploy".into(),
            body: "Ship the release?".into(),
        },
        tx,
    )));
    app
}

/// One buffer row as plain text (wide glyphs collapse to one cell).
fn row(buf: &Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..COLS {
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

fn rows(app: &mut App) -> Vec<String> {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    (0..ROWS).map(|y| row(&buf, y)).collect()
}

fn dump(lines: &[String], caption: &str) {
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for line in lines {
        println!("|{}|", line);
    }
    println!("END FRAME DUMP");
}

#[test]
fn frame_dump_dialog_footer_default() {
    let _guard = lock_registry();
    install(&[]);
    let mut app = confirm_app();
    let lines = rows(&mut app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[Enter/y] accept    [n/Esc/Ctrl+C] deny")),
        "the default footer:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1450 ctx.ui.confirm under the shipped table: `[Enter/y] accept`",
    );
}

#[test]
fn frame_dump_dialog_footer_rebound() {
    let _guard = lock_registry();
    install(&[
        ("tui.select.confirm", &["f2"]),
        ("tui.select.cancel", &["alt+x"]),
    ]);
    let mut app = confirm_app();
    let lines = rows(&mut app);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[f2/y] accept    [n/Alt+X] deny")),
        "the rebound footer:\n{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("[Enter/y]")),
        "the shipped chord is gone from the frame:\n{lines:#?}"
    );
    dump(
        &lines,
        "LUM-1450 the same modal after selectConfirm/selectCancel move: `[f2/y] accept`",
    );
}

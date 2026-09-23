//! LUM-1450 frame source — the `/help` key legend under two key tables.
//!
//! `cargo test -- --nocapture` prints the cell grids a real
//! [`pi_tui::App::render_to_buffer`] produced and `scripts/frame_to_png.py`
//! paints them (this Windows runner has no PTY — see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9).
//!
//! The frames are rendered from the **real merged coding-agent table**
//! (`merged_definitions`, the same one the TTY path installs) and the **real**
//! `/help` text (`commands::help_text_with`), pushed through the same
//! `App::info_block` the driver uses. Two 80×24 panels:
//!
//! 1. the shipped table — `Ctrl+R` reverse search, `Ctrl+L` model selector;
//! 2. `{"historySearch":["ctrl+p"],"selectConfirm":["f2"]}` plus a moved
//!    `app.model.select` — the same legend, new chords.
//!
//! Panel 2 is the defect this round closes: before it the legend was a literal
//! string, so it was byte-identical to panel 1 no matter what the user bound.
//! Frames prove the painted text; the resolution rules are pinned by
//! `commands::slash::tests::the_help_legend_tracks_the_effective_chords`.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::commands::slash::help_text_with;
use pi_coding_agent::keybindings::{merged_definitions, process_env, Platform};
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{set_keybindings, KeybindingsConfig, KeybindingsManager};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 100;
const ROWS: u16 = 30;

/// `set_keybindings` mutates process state, and cargo runs this binary's tests
/// concurrently: every frame takes this lock.
static REGISTRY: Mutex<()> = Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn manager(overrides: &[(&str, &[&str])]) -> KeybindingsManager {
    let mut config = KeybindingsConfig::default();
    for (id, keys) in overrides {
        config.set(*id, keys.iter().copied());
    }
    KeybindingsManager::new(merged_definitions(&Platform::Linux, &process_env()), config)
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

fn help_app(keybindings: &KeybindingsManager) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1450-help-frames".into(),
            ..AppConfig::default()
        },
    );
    // Exactly the driver's path: `/help` → `App::info_block(help_text())`.
    app.info_block(help_text_with(keybindings));
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
fn frame_dump_help_legend_default() {
    let _guard = lock_registry();
    let keybindings = manager(&[]);
    set_keybindings(keybindings.clone());
    let mut app = help_app(&keybindings);
    let lines = rows(&mut app);
    for needle in ["Ctrl+A / Ctrl+E", "Ctrl+R", "Ctrl+L", "Ctrl+C", "Esc"] {
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "the default legend must name {needle}:\n{lines:#?}"
        );
    }
    dump(
        &lines,
        "LUM-1450 /help under the merged table: the legend names the shipped chords",
    );
}

#[test]
fn frame_dump_help_legend_rebound() {
    let _guard = lock_registry();
    let keybindings = manager(&[
        ("tui.editor.historySearch", &["ctrl+p"]),
        ("app.model.select", &["ctrl+m"]),
        ("app.interrupt", &["ctrl+g"]),
    ]);
    set_keybindings(keybindings.clone());
    let mut app = help_app(&keybindings);
    let lines = rows(&mut app);
    for needle in ["Ctrl+P", "Ctrl+M", "Ctrl+G"] {
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "the rebound legend must name {needle}:\n{lines:#?}"
        );
    }
    for gone in ["Ctrl+R", "Ctrl+L"] {
        assert!(
            !lines.iter().any(|l| l.contains(gone)),
            "the replaced chord {gone} must be gone:\n{lines:#?}"
        );
    }
    dump(
        &lines,
        "LUM-1450 the same legend after keybindings.json moves three chords",
    );
}

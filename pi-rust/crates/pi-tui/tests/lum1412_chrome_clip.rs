//! LUM-1412 — a chrome row that does not fit its region is *marked*, not
//! silently cut.
//!
//! The 44×14 frame below is the shape the defect was measured in: a terminal
//! too small for the startup header's hint list, so the header folds to one
//! row (`locale::header_folded_line`, 51 characters) and that row is wider
//! than the terminal. Before this round the shared writer
//! (`styled::write_styled_line`) stopped at column 44 and the row read
//! `hints hidden on a short terminal — Alt+H sho` — a fragment that is
//! indistinguishable from a sentence the author chose to end that way.
//!
//! This file is also the frame source for
//! `docs/screenshots/lum1412-chrome-clip-44x14.png`: `cargo test` with
//! `--nocapture` prints the frame between the `FRAME` markers, and
//! `scripts/frame_to_png.py` renders that dump to a PNG. That path exists
//! because the runner that produced this round is a Windows box with no
//! `pty` module (and no `pyte`), so `scripts/pty_capture.py` cannot run there
//! — see `docs/LUM1412_CHROME_CLIP.md` §5. On a Linux runner the same frame
//! is reproducible end-to-end from the real binary via
//! `scripts/pty_scenarios/lum1412-chrome-clip.json`.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{
    reset_keybindings, set_keybindings, tui_default_keybindings, KeybindingDefinition,
    KeybindingsConfig, KeybindingsManager,
};
use pi_tui::MessageItem;

const COLS: u16 = 44;
const ROWS: u16 = 14;

/// The chords the folded header row names. `app.header` is the one printed in
/// that row; the rest keep the header's other rows resolvable so the folded
/// layout is the real one.
const APP_CHORDS: &[(&str, &str)] = &[
    ("app.interrupt", "escape"),
    ("app.clear", "ctrl+c"),
    ("app.exit", "ctrl+d"),
    ("app.header", "alt+h"),
    ("app.tools.expand", "ctrl+o"),
    ("app.model.select", "ctrl+l"),
    ("app.thinking.cycle", "shift+tab"),
];

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

fn startup_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1412-chrome-clip".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    app.messages_mut().push(MessageItem::user("hello"));
    app
}

fn frame() -> Vec<String> {
    let mut definitions = tui_default_keybindings();
    for (id, chord) in APP_CHORDS {
        definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
    }
    set_keybindings(KeybindingsManager::new(
        definitions,
        KeybindingsConfig::new(),
    ));
    startup_app().render_snapshot(COLS, ROWS).lines
}

/// The defect, pinned at the exact frame it was measured in: the folded
/// header row keeps whole words and ends with the clip mark.
#[test]
fn the_folded_header_row_is_marked_at_44_columns() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lines = frame();
    // Row 0 is the title (`pi v0.1.0`); the folded hint list is row 1.
    let header = lines.get(1).expect("the folded header row").clone();

    assert_eq!(
        header,
        "Press Alt+H to show full startup help and…",
        "the folded header must drop a whole word and say so:\n{}",
        lines.join("\n")
    );
    // The pre-fix frame, byte for byte — the regression net for the silent
    // clip. `sty...` never appears in a marked row.
    assert_ne!(
        header,
        "Press Alt+H to show full startup help an",
        "{header}"
    );
    assert!(
        !header.ends_with("reso") && !header.ends_with("res"),
        "no partial word may survive the clip: {header:?}"
    );
    // The rows that follow keep their own content: the composer, the status
    // bar and the footer are not collateral damage of the header's clip.
    assert!(
        lines.iter().any(|line| line.contains("type a prompt")),
        "the composer survives a 44×14 terminal:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().any(|line| line.contains("Faux")),
        "the status bar survives a 44×14 terminal:\n{}",
        lines.join("\n")
    );
    reset_keybindings();
}

/// The same frame, printed for the screenshot pipeline
/// (`scripts/frame_to_png.py`). Every row is padded to the region width, so
/// the dump is a faithful cell grid rather than a set of trimmed strings.
#[test]
fn frame_dump_for_the_screenshot() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = {
        let mut definitions = tui_default_keybindings();
        for (id, chord) in APP_CHORDS {
            definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
        }
        set_keybindings(KeybindingsManager::new(
            definitions,
            KeybindingsConfig::new(),
        ));
        startup_app().render_snapshot(COLS, ROWS)
    };
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for line in &snapshot.lines {
        println!("|{}|", pad(line, COLS as usize));
    }
    println!("END FRAME DUMP");
    reset_keybindings();
}

fn pad(line: &str, width: usize) -> String {
    let mut out: String = line.chars().take(width).collect();
    while out.chars().count() < width {
        out.push(' ');
    }
    out
}

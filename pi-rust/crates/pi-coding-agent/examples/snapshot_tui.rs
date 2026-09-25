//! End-to-end visual parity snapshot harness.
//!
//! Build with: `cargo run -p pi-coding-agent --example snapshot_tui`
//!
//! For each scenario in `SCENARIOS`:
//!   1. Boot a fresh `App` against the `faux` provider + dark theme.
//!   2. Drive the scenario-specific setters (status, selector, message list).
//!   3. Render the App through [`App::render_snapshot`].
//!   4. Emit a file under `target/snapshot/<scenario>.txt` — trimmed
//!      per-row plain text, suitable for `diff` against a TS pi-tui
//!      reference snapshot.
//!
//! The harness is the make-or-break gate of Phase 8: it surfaces any
//! remaining per-cell drift between the Rust port and upstream pi-tui.
//! Differences should be triaged one of three ways:
//!   * **Rust bug** — the Rust frame diverges from what upstream would
//!     produce; fix it in the relevant `pi-tui` module.
//!   * **TS reference drift** — upstream has since changed; regenerate the
//!     reference snapshot and move on.
//!   * **Acceptable diff** — spinner frame numbers, async-driven timing,
//!     anything whose value depends on capture instant.
//!
//! See `docs/VISUAL_PARITY_DIFF.md` for the running list of captured
//! scenarios and their deltas.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, RenderSnapshot};
use pi_tui::components::selector::{Selector, SelectorItem};
use pi_tui::keybindings::{
    set_keybindings, tui_default_keybindings, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager,
};
use pi_tui::message::{MessageItem, ToolStatus};
use pi_tui::theme::{builtin_theme, ColorMode};
use pi_tui::utils::styled::{SpanStyle, StyledLine, StyledSpan};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// Keybindings the live TUI install before render — without this the
/// `app.header` row resolves to the hardcoded `Alt+H` fallback and the
/// fold row that lives next to the title never appears, even on a
/// short-terminal capture.
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
    ("app.editor.external", "ctrl+e"),
    ("app.message.followUp", "alt+enter"),
    ("app.message.dequeue", "alt+up"),
    ("app.clipboard.pasteImage", "ctrl+v"),
];

fn install_keybindings() {
    let mut definitions = tui_default_keybindings();
    for (id, chord) in APP_CHORDS {
        definitions.push(((*id).to_string(), KeybindingDefinition::new([*chord])));
    }
    set_keybindings(KeybindingsManager::new(
        definitions,
        KeybindingsConfig::new(),
    ));
}

const WIDTH: u16 = 100;
const HEIGHT: u16 = 32;
const SHORT_HEIGHT: u16 = 23;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        api: Api::Faux,
        id: "faux-model".into(),
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn base_app(agent: &Agent, session: &str) -> App {
    App::new(
        agent,
        AppConfig {
            session_id: session.into(),
            startup_header: true,
            ..AppConfig::default()
        },
    )
}

fn render(app: &App) -> Vec<String> {
    let snap: RenderSnapshot = app.render_snapshot(WIDTH, HEIGHT);
    snap.lines
}

fn write_snapshot(out_dir: &std::path::Path, name: &str, lines: &[String]) {
    fs::create_dir_all(out_dir).expect("mkdir target/snapshot");
    let path = out_dir.join(format!("{name}.txt"));
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(&path, body).expect("write snapshot");
    eprintln!("wrote {} ({} lines)", path.display(), lines.len());
}

/// 1. Empty editor + empty transcript — the cold-boot view.
fn scenario_empty(agent: &Agent, out: &std::path::Path) {
    let app = base_app(agent, "snapshot-empty");
    write_snapshot(out, "01-empty", &render(&app));
}

/// 2. One user message + one assistant message.
fn scenario_messages(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-messages");
    app.messages_mut().push(MessageItem::user("hello pi"));
    app.messages_mut().push(MessageItem::assistant("hi back"));
    write_snapshot(out, "02-messages", &render(&app));
}

/// 3. Multi-line composer draft — exercises the G3 top border (only painted
/// when `composer_max_rows > 1`).
fn scenario_multiline_composer(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-composer");
    app.prompt_mut().editor_mut().set_text(
        "alpha bravo charlie\ndelta echo foxtrot\ngolf hotel india",
    );
    write_snapshot(out, "03-composer-multiline", &render(&app));
}

/// 4. Selector overlay open with five items — exercises the G5 ─ separator
/// line between header and list.
fn scenario_selector(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-selector");
    let items = (1..=5)
        .map(|i| SelectorItem::new(format!("item-{i}"), format!("Item {i}")))
        .collect();
    app.open_selector(Selector::new("Pick one", items));
    write_snapshot(out, "04-selector", &render(&app));
}

/// 5. Tool block with folded output — exercises the G1 ToolSuccessBg slot.
fn scenario_tool_block(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-tool");
    let mut tool = MessageItem::tool("echo hi");
    tool.tool_status = ToolStatus::Success;
    tool.tool_expanded = Some(false);
    let output_lines: Vec<StyledLine> = (1..=200)
        .map(|i| vec![StyledSpan::new(format!("output line {i}"), SpanStyle::PLAIN)])
        .collect();
    tool.tool_lines = Some(output_lines);
    app.messages_mut().push(tool);
    write_snapshot(out, "05-tool-block", &render(&app));
}

/// 6. Footer with `thinking_level = High` + `experimental = true` — exercises
/// G2 (`• xp`) and the thinking-level segment colour.
fn scenario_footer_xp(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-footer-xp");
    app.set_thinking_level(pi_agent_core::ThinkingLevel::High);
    app.set_experimental(true);
    app.set_status_cwd(Some("/srv/repo".into()));
    app.set_status_git_branch(Some("main".into()));
    app.set_status_provider(3, Some("anthropic".into()));
    write_snapshot(out, "06-footer-xp", &render(&app));
}

/// 7. Short terminal that folds the header — exercises the G4
/// compactOnboarding row and the fold path that hands rows back to the
/// transcript.
fn scenario_short_terminal(agent: &Agent, out: &std::path::Path) {
    let mut app = base_app(agent, "snapshot-short");
    app.messages_mut().push(MessageItem::user("hi"));
    let snap = app.render_snapshot(WIDTH, SHORT_HEIGHT);
    write_snapshot(out, "07-short-terminal", &snap.lines);
}

/// Drive every scenario and dump per-cell cell-symbol traces alongside the
/// trimmed text, for parity work that needs to see the exact ratatui cell
/// stream (e.g. width-1 character indexing).
fn dump_cell_traces(agent: &Agent, out: &std::path::Path) {
    fs::create_dir_all(out).expect("mkdir target/snapshot");
    for (name, app) in [
        ("01-empty", base_app(agent, "snapshot-empty")),
        (
            "02-messages",
            {
                let mut a = base_app(agent, "snapshot-messages");
                a.messages_mut().push(MessageItem::user("hello pi"));
                a.messages_mut().push(MessageItem::assistant("hi back"));
                a
            }
        ),
    ] {
        let area = Rect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        };
        let mut buf = Buffer::empty(area);
        let mut a = app;
        a.render_to_buffer(area, &mut buf);
        let mut body = String::with_capacity((WIDTH as usize + 1) * HEIGHT as usize);
        for y in 0..area.height {
            let row: String = (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect();
            body.push_str(row.trim_end());
            body.push('\n');
        }
        let path = out.join(format!("{name}.cells.txt"));
        fs::write(&path, &body).expect("write cell trace");
        eprintln!("wrote {}", path.display());
    }
}

fn main() {
    let target = env::var("SNAPSHOT_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("target/snapshot"));

    // Smoke-test that the dark theme resolves — the scenarios all use it,
    // so a missing theme JSON is a fast failure surface.
    let _theme = builtin_theme("dark", ColorMode::TrueColor)
        .expect("dark theme must be available; assets/themes/dark.json is wired through Theme::from_json");

    // Install the `app.*` keybinding table so the fold row can resolve
    // `Alt+H` exactly the way `startup_header.rs` does for the unit tests.
    install_keybindings();

    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));

    scenario_empty(&agent, &target);
    scenario_messages(&agent, &target);
    scenario_multiline_composer(&agent, &target);
    scenario_selector(&agent, &target);
    scenario_tool_block(&agent, &target);
    scenario_footer_xp(&agent, &target);
    scenario_short_terminal(&agent, &target);
    dump_cell_traces(&agent, &target);
}
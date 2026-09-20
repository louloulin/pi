//! Integration coverage for the App's themed buffer pipeline.
//!
//! Unlike `tests/styles.rs` (which asserts on the `*_themed` ANSI strings),
//! these tests drive the real interactive render path — `App::render_to_buffer`
//! — and assert the [`ratatui::style::Style`] carried by the rendered cells.
//! They also cover the theme hot-swap requirement: replacing the App's theme
//! must be reflected by the very next render.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions, ThinkingLevel};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::theme::{builtin_theme, ColorMode};
use pi_tui::{MessageItem, Selector, SelectorItem};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Dark palette values (`assets/themes/dark.json`).
const ACCENT: Color = Color::Rgb(138, 190, 183);
const MUTED: Color = Color::Rgb(128, 128, 128);
const DIM: Color = Color::Rgb(102, 102, 102);
const TEXT: Color = Color::Rgb(212, 212, 212);
const BORDER_MUTED: Color = Color::Rgb(80, 80, 80);
const SELECTED_BG: Color = Color::Rgb(58, 58, 74);

/// Light palette accent (`assets/themes/light.json`).
const LIGHT_ACCENT: Color = Color::Rgb(90, 128, 128);

/// Dark palette `bashMode` (`vars.green` in `assets/themes/dark.json`).
const BASH_MODE: Color = Color::Rgb(181, 189, 104);

/// Dark palette thinking-border slots (`assets/themes/dark.json`).
const THINKING_MEDIUM: Color = Color::Rgb(129, 162, 190);
const THINKING_HIGH: Color = Color::Rgb(178, 148, 187);
const THINKING_MAX: Color = Color::Rgb(255, 95, 255);

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
    let config = AppConfig {
        session_id: "test".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn style_at(buf: &Buffer, x: u16, y: u16) -> Style {
    buf.cell((x, y)).expect("cell in bounds").style()
}

/// A cell that was never themed keeps `ratatui`'s `Color::Reset` default and
/// no modifiers.
fn is_unstyled(style: Style) -> bool {
    style.fg == Some(Color::Reset)
        && style.bg == Some(Color::Reset)
        && style.add_modifier.is_empty()
}

fn symbol_at(buf: &Buffer, x: u16, y: u16) -> String {
    buf.cell((x, y))
        .expect("cell in bounds")
        .symbol()
        .to_string()
}

/// The text of one rendered row, used to assert on the status bar's
/// composed content rather than on individual cells.
fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .filter_map(|x| buf.cell((x, y)))
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn app_buffer_cells_carry_the_theme_colours() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.messages_mut().push(MessageItem::assistant("hi"));
    let buf = render(&mut app, 40, 4);

    // Message rows: `> hello` (row 0) and `  hi` (row 1).
    assert_eq!(symbol_at(&buf, 0, 0), ">");
    assert_eq!(style_at(&buf, 0, 0).fg, Some(ACCENT));
    // The user body takes `userMessageText`. The assistant body renders as
    // markdown by default (`AppConfig::markdown`), and a markdown paragraph
    // carries no theme slot — upstream's `Markdown` inherits the terminal's
    // default text style — so those cells stay unstyled.
    assert_eq!(style_at(&buf, 2, 0).fg, Some(TEXT));
    assert!(is_unstyled(style_at(&buf, 2, 1)));

    // Status row (y = height - 1, the last row: the status bar sits below
    // the editor region, upstream's footer position): model accent, session
    // muted, stats dim.
    assert_eq!(symbol_at(&buf, 0, 3), "F");
    assert_eq!(style_at(&buf, 0, 3).fg, Some(ACCENT));
    assert_eq!(symbol_at(&buf, 6, 3), "t"); // "  test  " starts at column 4
    assert_eq!(style_at(&buf, 6, 3).fg, Some(MUTED));
    assert_eq!(symbol_at(&buf, 12, 3), "i"); // "in 0 out 0 …" starts at column 12
    assert_eq!(style_at(&buf, 12, 3).fg, Some(DIM));

    // The editor region (y = height - 2) paints the prompt label in the
    // current thinking level's border colour (upstream
    // `updateEditorBorderColor`); the buffer itself stays plain.
    assert_eq!(style_at(&buf, 0, 2).fg, Some(THINKING_MEDIUM));
    assert!(is_unstyled(style_at(&buf, 2, 2)));
}

#[test]
fn set_theme_hot_swaps_the_next_render() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));

    let dark = render(&mut app, 40, 4);
    assert_eq!(style_at(&dark, 0, 0).fg, Some(ACCENT));

    app.set_theme(builtin_theme("light", ColorMode::TrueColor).expect("light theme"));
    let light = render(&mut app, 40, 4);
    assert_eq!(style_at(&light, 0, 0).fg, Some(LIGHT_ACCENT));
    // The visible text is unchanged by the palette.
    assert_eq!(symbol_at(&light, 0, 0), ">");
    assert_eq!(symbol_at(&light, 2, 0), "h");

    // Switching back restores the dark accent on the same App instance.
    app.set_theme_by_name("dark").expect("built-in dark");
    let back = render(&mut app, 40, 4);
    assert_eq!(style_at(&back, 0, 0).fg, Some(ACCENT));
}

#[test]
fn bash_mode_colours_the_prompt_label() {
    let mut app = app();
    // The editor region is the row above the status bar (`height - 2`); the
    // prompt label occupies its first two cells.
    app.set_editor_text("!ls");
    let buf = render(&mut app, 40, 4);
    assert_eq!(symbol_at(&buf, 0, 2), ">");
    assert_eq!(style_at(&buf, 0, 2).fg, Some(BASH_MODE));
    assert_eq!(style_at(&buf, 1, 2).fg, Some(BASH_MODE));
    // Only the label carries the colour; the buffer itself stays plain.
    assert!(is_unstyled(style_at(&buf, 2, 2)));

    // A normal buffer paints the label in the thinking level's border colour
    // instead (default level: medium).
    app.set_editor_text("ls");
    let buf = render(&mut app, 40, 4);
    assert_eq!(style_at(&buf, 0, 2).fg, Some(THINKING_MEDIUM));
}

#[test]
fn the_prompt_label_follows_the_thinking_level() {
    let mut app = app();

    app.set_thinking_level(ThinkingLevel::High);
    let buf = render(&mut app, 40, 4);
    assert_eq!(style_at(&buf, 0, 2).fg, Some(THINKING_HIGH));

    app.set_thinking_level(ThinkingLevel::Max);
    let buf = render(&mut app, 40, 4);
    assert_eq!(style_at(&buf, 0, 2).fg, Some(THINKING_MAX));

    // Bash mode still wins over the thinking colour.
    app.set_editor_text("!ls");
    let buf = render(&mut app, 40, 4);
    assert_eq!(style_at(&buf, 0, 2).fg, Some(BASH_MODE));
}

#[test]
fn the_status_bar_shows_the_level_only_for_reasoning_models() {
    let mut app = app();

    app.set_thinking_supported(true);
    app.set_thinking_level(ThinkingLevel::Off);
    let buf = render(&mut app, 60, 4);
    let status = row_text(&buf, 3, 60);
    assert!(status.contains("• thinking off"), "got {status:?}");

    app.set_thinking_level(ThinkingLevel::High);
    let buf = render(&mut app, 60, 4);
    let status = row_text(&buf, 3, 60);
    assert!(status.contains("• high"), "got {status:?}");

    // Upstream's footer only shows the segment for reasoning models.
    app.set_thinking_supported(false);
    app.set_thinking_level(ThinkingLevel::Medium);
    let buf = render(&mut app, 60, 4);
    let status = row_text(&buf, 3, 60);
    assert!(!status.contains('•'), "got {status:?}");
}

#[test]
fn a_plain_theme_renders_unstyled_cells() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.set_theme(builtin_theme("dark", ColorMode::None).expect("plain theme"));
    let buf = render(&mut app, 40, 4);

    assert_eq!(symbol_at(&buf, 0, 0), ">");
    assert!(is_unstyled(style_at(&buf, 0, 0)));
    assert!(is_unstyled(style_at(&buf, 2, 0)));
    assert!(is_unstyled(style_at(&buf, 0, 2)));
}

#[test]
fn selector_overlay_uses_the_title_border_and_selected_row_slots() {
    let mut app = app();
    let items = vec![
        SelectorItem::new("alpha", "Alpha").with_description("one"),
        SelectorItem::new("beta", "Beta").with_description("two"),
    ];
    app.open_selector(Selector::new("Pick", items));
    let buf = render(&mut app, 60, 12);

    // The selector overlays starting one row below the message area's top.
    let title = style_at(&buf, 0, 1);
    assert_eq!(symbol_at(&buf, 0, 1), "P");
    assert_eq!(title.fg, Some(ACCENT));
    assert!(title.add_modifier.contains(Modifier::BOLD));

    assert_eq!(symbol_at(&buf, 0, 2), "─");
    assert_eq!(style_at(&buf, 0, 2).fg, Some(BORDER_MUTED));

    // Row 3 is the selected first item: accent fg over the selectedBg.
    let selected = style_at(&buf, 0, 3);
    assert_eq!(symbol_at(&buf, 0, 3), "❯");
    assert_eq!(selected.fg, Some(ACCENT));
    assert_eq!(selected.bg, Some(SELECTED_BG));

    // The non-selected item's description column is muted.
    assert!(is_unstyled(style_at(&buf, 2, 4)));
    assert_eq!(style_at(&buf, 34, 4).fg, Some(MUTED));
    assert_eq!(symbol_at(&buf, 34, 4), "t"); // "two" starts at column 34
}

#[test]
fn selector_ansi_strings_match_the_styled_buffer_text() {
    // The span-based buffer path and the legacy ANSI path must agree on the
    // visible layout, so a caller migrating between them sees no jump.
    let mut app = app();
    app.open_selector(Selector::new(
        "Pick",
        vec![SelectorItem::new("alpha", "Alpha").with_description("one")],
    ));
    let snapshot = app.render_snapshot(60, 12);
    let buf = render(&mut app, 60, 12);

    let title_row: String = (0..60)
        .map(|x| symbol_at(&buf, x, 1))
        .collect::<String>()
        .trim_end()
        .to_string();
    assert_eq!(title_row, "Pick");
    assert_eq!(snapshot.lines[1], "Pick");
}

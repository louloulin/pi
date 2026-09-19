//! Integration coverage for the App's themed buffer pipeline.
//!
//! Unlike `tests/styles.rs` (which asserts on the `*_themed` ANSI strings),
//! these tests drive the real interactive render path — `App::render_to_buffer`
//! — and assert the [`ratatui::style::Style`] carried by the rendered cells.
//! They also cover the theme hot-swap requirement: replacing the App's theme
//! must be reflected by the very next render.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
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

fn render(app: &App, width: u16, height: u16) -> Buffer {
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

#[test]
fn app_buffer_cells_carry_the_theme_colours() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.messages_mut().push(MessageItem::assistant("hi"));
    let buf = render(&app, 40, 4);

    // Message rows: `> hello` (row 0) and `  hi` (row 1).
    assert_eq!(symbol_at(&buf, 0, 0), ">");
    assert_eq!(style_at(&buf, 0, 0).fg, Some(ACCENT));
    assert_eq!(style_at(&buf, 2, 0).fg, Some(TEXT)); // userMessageText
    assert_eq!(style_at(&buf, 2, 1).fg, Some(TEXT)); // assistant body

    // Status row (y = height - 2): model accent, session muted, stats dim.
    assert_eq!(symbol_at(&buf, 0, 2), "F");
    assert_eq!(style_at(&buf, 0, 2).fg, Some(ACCENT));
    assert_eq!(symbol_at(&buf, 6, 2), "t"); // "  test  " starts at column 4
    assert_eq!(style_at(&buf, 6, 2).fg, Some(MUTED));
    assert_eq!(symbol_at(&buf, 18, 2), "i"); // "in 0 out 0 …" starts at column 18
    assert_eq!(style_at(&buf, 18, 2).fg, Some(DIM));

    // The prompt row keeps its plain style (out of scope for this slice).
    assert!(is_unstyled(style_at(&buf, 0, 3)));
}

#[test]
fn set_theme_hot_swaps_the_next_render() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));

    let dark = render(&app, 40, 4);
    assert_eq!(style_at(&dark, 0, 0).fg, Some(ACCENT));

    app.set_theme(builtin_theme("light", ColorMode::TrueColor).expect("light theme"));
    let light = render(&app, 40, 4);
    assert_eq!(style_at(&light, 0, 0).fg, Some(LIGHT_ACCENT));
    // The visible text is unchanged by the palette.
    assert_eq!(symbol_at(&light, 0, 0), ">");
    assert_eq!(symbol_at(&light, 2, 0), "h");

    // Switching back restores the dark accent on the same App instance.
    app.set_theme_by_name("dark").expect("built-in dark");
    let back = render(&app, 40, 4);
    assert_eq!(style_at(&back, 0, 0).fg, Some(ACCENT));
}

#[test]
fn a_plain_theme_renders_unstyled_cells() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.set_theme(builtin_theme("dark", ColorMode::None).expect("plain theme"));
    let buf = render(&app, 40, 4);

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
    let buf = render(&app, 60, 12);

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
    let buf = render(&app, 60, 12);

    let title_row: String = (0..60)
        .map(|x| symbol_at(&buf, x, 1))
        .collect::<String>()
        .trim_end()
        .to_string();
    assert_eq!(title_row, "Pick");
    assert_eq!(snapshot.lines[1], "Pick");
}

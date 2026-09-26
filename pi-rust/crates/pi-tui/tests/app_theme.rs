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
use pi_tui::autocomplete::{CombinedAutocompleteProvider, SlashCommand};
use pi_tui::input::Key;
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

/// One rendered row as a quoted `String`, for inclusion in assertion
/// failure messages.
fn rows_text(buf: &Buffer, y: u16) -> String {
    let width = buf.area().width;
    row_text(buf, y, width)
}

#[test]
fn app_buffer_cells_carry_the_theme_colours() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.messages_mut().push(MessageItem::assistant("hi"));
    // Phase 2 / G3 reserves one extra chrome row for the editor border, so
    // height 4 leaves exactly one message row visible. Use 5 to keep both
    // messages in view.
    let buf = render(&mut app, 40, 5);

    // Message rows: `hello` (row 0) and `  hi` (row 1). P9 removed the
    // `> ` prefix from user messages — `userMessageText` now owns the
    // first column, and the body takes the `text` colour from the theme.
    assert_eq!(symbol_at(&buf, 0, 0), "h");
    assert_eq!(style_at(&buf, 0, 0).fg, Some(TEXT));
    // The assistant body renders as markdown by default (`AppConfig::markdown`),
    // and a markdown paragraph carries no theme slot — upstream's `Markdown`
    // inherits the terminal's default text style — so those cells stay
    // unstyled. The assistant still keeps its two-cell indent.
    assert_eq!(symbol_at(&buf, 0, 1), " ");
    assert_eq!(symbol_at(&buf, 2, 1), "h");
    assert!(is_unstyled(style_at(&buf, 2, 1)));

    // Status row (y = height - 1, the last row: the status bar sits below
    // the editor region, upstream's footer position): the stats cluster is
    // dim and left-aligned, the session id muted in the middle, and the model
    // accent at the right edge — upstream's `statsLeft + padding + rightSide`
    // with LUM-1467's field alignment (`footer.ts:205-240`).
    assert_eq!(symbol_at(&buf, 0, 4), "?"); // "?/1.0k  ? for help" starts here
    assert_eq!(style_at(&buf, 0, 4).fg, Some(DIM));
    assert_eq!(symbol_at(&buf, 20, 4), "t"); // "  test  " starts at column 18
    assert_eq!(style_at(&buf, 20, 4).fg, Some(MUTED));
    assert_eq!(symbol_at(&buf, 36, 4), "F"); // "Faux" is flush right, at column 36
    assert_eq!(style_at(&buf, 36, 4).fg, Some(ACCENT));

    // The editor body sits at y = height - 2 (the prompt label colour is the
    // current thinking level's border colour, see upstream
    // `updateEditorBorderColor`). The row above (y = height - 3) is the new
    // border row (Phase 2 / G3) — `─` glyphs in the same border colour.
    // The border only paints when the composer is multi-row; at the default
    // height of 5 with two short messages, the composer has the room to
    // wrap, so the border row is visible.
    assert_eq!(symbol_at(&buf, 0, 2), "─");
    assert_eq!(style_at(&buf, 2, 3).bg, Some(SELECTED_BG));
}

#[test]
fn set_theme_hot_swaps_the_next_render() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));

    let dark = render(&mut app, 40, 4);
    assert_eq!(style_at(&dark, 0, 0).fg, Some(TEXT));

    app.set_theme(builtin_theme("light", ColorMode::TrueColor).expect("light theme"));
    let light = render(&mut app, 40, 4);
    assert_eq!(style_at(&light, 0, 0).fg, Some(Color::Rgb(31, 35, 40)));
    // The visible text is unchanged by the palette.
    assert_eq!(symbol_at(&light, 0, 0), "h");
    assert_eq!(symbol_at(&light, 1, 0), "e");

    // Switching back restores the dark text colour on the same App instance.
    app.set_theme_by_name("dark").expect("built-in dark");
    let back = render(&mut app, 40, 4);
    assert_eq!(style_at(&back, 0, 0).fg, Some(TEXT));
}

#[test]
fn bash_mode_colours_the_prompt_label() {
    let mut app = app();
    // The editor region is the row above the status bar (`height - 2`); the
    // prompt label occupies its first two cells.
    app.set_editor_text("!ls");
    let buf = render(&mut app, 40, 4);
    // P9 removed the `> ` chevron prefix; the bash-mode signal now lives
    // on the leading `!`/`!!` indicator. The composer paints that indicator
    // in `BashMode` so the user can still tell the buffer is a shell
    // submission rather than a model prompt.
    assert_eq!(symbol_at(&buf, 0, 2), "!");
    assert_eq!(style_at(&buf, 0, 2).fg, Some(BASH_MODE));
    // Only the indicator carries bash-mode colour; the rest of the row
    // keeps its default body colour and the `selectedBg` slot the composer
    // paints on every row.
    assert_eq!(symbol_at(&buf, 1, 2), "l");
    assert_ne!(style_at(&buf, 1, 2).fg, Some(BASH_MODE));
    assert_eq!(style_at(&buf, 2, 2).bg, Some(SELECTED_BG));

    // A normal buffer paints nothing in the bash-mode colour — the body
    // just inherits the default body colour.
    app.set_editor_text("ls");
    let buf = render(&mut app, 40, 4);
    assert_ne!(style_at(&buf, 0, 2).fg, Some(BASH_MODE));
}

#[test]
fn the_prompt_label_follows_the_thinking_level() {
    // P9 removed the `> ` chevron label that used to take the thinking-level
    // border colour on the composer's first row. The thinking level still
    // drives the border row painted above a multi-row composer (the
    // `─` separator from Phase 2 / G3), so we exercise that path here by
    // filling the buffer with enough newlines to force `max_rows > 1`.
    let mut app = app();

    app.set_thinking_level(ThinkingLevel::High);
    app.set_editor_text("alpha\nbeta\ngamma\ndelta\nepsilon\nzeta");
    let buf = render(&mut app, 40, 6);
    // Border row sits immediately above the composer's rectangle. With
    // height=6 the layout reserves row 5 for the status bar and one
    // message row at the top, leaving 4 rows for the editor — one
    // border row plus three composer rows, anchored at y = 1.
    assert_eq!(style_at(&buf, 0, 1).fg, Some(THINKING_HIGH));

    app.set_thinking_level(ThinkingLevel::Max);
    let buf = render(&mut app, 40, 6);
    assert_eq!(style_at(&buf, 0, 1).fg, Some(THINKING_MAX));

    // Bash mode still wins over the thinking colour on the border row.
    app.set_editor_text("!ls\nalpha\nbeta\ngamma\ndelta\nepsilon");
    let buf = render(&mut app, 40, 6);
    assert_eq!(style_at(&buf, 0, 1).fg, Some(BASH_MODE));
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

    // P9 removed the `> ` prefix from user messages — the user body now
    // starts at column 0 and inherits the default text colour.
    assert_eq!(symbol_at(&buf, 0, 0), "h");
    assert!(is_unstyled(style_at(&buf, 0, 0)));
    assert!(is_unstyled(style_at(&buf, 1, 0)));
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
    assert_eq!(symbol_at(&buf, 0, 3), "→");
    assert_eq!(selected.fg, Some(ACCENT));
    assert_eq!(selected.bg, Some(SELECTED_BG));

    // The non-selected item's description column is muted. N7
    // right-aligns descriptions to the row's right edge (column 59 for
    // width=60), so the description "two" sits flush at columns 57–59.
    assert!(is_unstyled(style_at(&buf, 2, 4)));
    assert_eq!(style_at(&buf, 57, 4).fg, Some(MUTED));
    assert_eq!(symbol_at(&buf, 57, 4), "t"); // "two" ends at column 59
}

#[test]
fn the_composer_dropdown_paints_the_select_list_theme_roles() {
    // LUM-1305: the dropdown is laid out by the shared `SelectList`
    // implementation, so its cells carry the same roles the modal pickers
    // do — `accent` over `selectedBg` for the highlighted row, `muted` for
    // the description column, and an unstyled label. The App used to derive
    // the style from the row text itself and painted every candidate row
    // `muted`, so a label and its description came out the same colour.
    let mut app = app();
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
            vec![
                SlashCommand::new("help").with_description("show this help text"),
                SlashCommand::new("clear").with_description("wipe the message view"),
            ],
            std::env::temp_dir(),
        )));
    app.step_key(Key::char('/'));
    let buf = render(&mut app, 80, 12);

    let selected_row = row_containing(&buf, 80, 12, "→ help");
    let plain_row = row_containing(&buf, 80, 12, "  clear");
    assert_eq!(plain_row, selected_row + 1, "the list is one row per item");

    // N7 right-aligns the description column. With width=80:
    //  - "show this help text" (19 cols) is right-aligned → columns 61–79
    //  - "wipe the message view" (21 cols) is right-aligned → columns 59–79
    let selected_desc_start: u16 = (80 - "show this help text".chars().count()) as u16;
    let plain_desc_start: u16 = (80 - "wipe the message view".chars().count()) as u16;
    assert_eq!(
        symbol_at(&buf, selected_desc_start, selected_row),
        "s",
        "selected row {selected_row} col {selected_desc_start}: row was {:?}",
        rows_text(&buf, selected_row)
    );
    assert_eq!(
        symbol_at(&buf, plain_desc_start, plain_row),
        "w",
        "plain row {plain_row} col {plain_desc_start}: row was {:?}",
        rows_text(&buf, plain_row)
    );

    // The selected row is wrapped whole: accent fg over selectedBg, label and
    // description alike.
    let selected_label = style_at(&buf, 2, selected_row);
    assert_eq!(selected_label.fg, Some(ACCENT));
    assert_eq!(selected_label.bg, Some(SELECTED_BG));
    let selected_description = style_at(&buf, selected_desc_start, selected_row);
    assert_eq!(selected_description.fg, Some(ACCENT));
    assert_eq!(selected_description.bg, Some(SELECTED_BG));

    // A plain row keeps its label at the terminal default and only the
    // description column takes `muted`.
    assert!(is_unstyled(style_at(&buf, 2, plain_row)));
    assert_eq!(style_at(&buf, plain_desc_start, plain_row).fg, Some(MUTED));
    // The right-aligned description ends flush with the row's right edge:
    // "wipe the message view" (21 cols) is right-aligned into columns
    // 59..=79, so the trailing "w" lives in the last column.
    assert_eq!(symbol_at(&buf, 79, plain_row), "w");
}

/// The `y` of the first row whose text contains `needle`.
fn row_containing(buf: &Buffer, width: u16, height: u16, needle: &str) -> u16 {
    (0..height)
        .find(|y| row_text(buf, *y, width).contains(needle))
        .unwrap_or_else(|| panic!("no row contains {needle:?}"))
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

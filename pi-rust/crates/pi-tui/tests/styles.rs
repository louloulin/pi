//! Integration coverage for the theme consumption layer: the
//! [`SelectListStyles`] adapter, the themed render paths of `Selector`,
//! `StatusBar` and `MessageView`, and the plain-text regressions.

use pi_tui::styles::SelectListStyles;
use pi_tui::theme::{builtin_theme, ColorMode, Theme};
use pi_tui::{MessageItem, MessageView, Selector, SelectorItem, StatusBar, StatusData};

// Dark palette (crates/pi-tui/assets/themes/dark.json):
// accent #8abeb7, muted #808080, dim #666666, text/userMessageText #d4d4d4,
// selectedBg #3a3a4a.
const ACCENT: &str = "\x1b[38;2;138;190;183m";
const MUTED: &str = "\x1b[38;2;128;128;128m";
const DIM: &str = "\x1b[38;2;102;102;102m";
const TEXT: &str = "\x1b[38;2;212;212;212m";
const SELECTED_BG: &str = "\x1b[48;2;58;58;74m";
const ERROR: &str = "\x1b[38;2;204;102;102m";
const WARNING: &str = "\x1b[38;2;255;255;0m";

fn dark() -> Theme {
    builtin_theme("dark", ColorMode::TrueColor).expect("built-in dark theme")
}

/// Remove every `\x1b[`-introduced escape sequence so a themed render can be
/// compared against the plain render.
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn strip_ansi_lines(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| strip_ansi(line)).collect()
}

fn items() -> Vec<SelectorItem> {
    vec![
        SelectorItem::new("gpt", "gpt-4o").with_description("OpenAI"),
        SelectorItem::new("claude", "claude-3.5-sonnet").with_description("Anthropic"),
        SelectorItem::new("faux", "faux-model").with_description("Test"),
    ]
}

#[test]
fn select_list_methods_emit_the_exact_dark_ansi_sequences() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);

    assert_eq!(
        styles.selected_text("row"),
        format!("{SELECTED_BG}{ACCENT}row\x1b[39m\x1b[49m")
    );
    assert_eq!(styles.description("desc"), format!("{MUTED}desc\x1b[39m"));
    assert_eq!(
        styles.no_match("  No matching items"),
        format!("{MUTED}  No matching items\x1b[39m")
    );
    assert_eq!(
        styles.scroll_info("  (1/12)"),
        format!("{MUTED}  (1/12)\x1b[39m")
    );
    // Upstream `selectedPrefix` (`theme.ts:1211`).
    assert_eq!(styles.selected_prefix("❯ "), format!("{ACCENT}❯ \x1b[39m"));
}

#[test]
fn selector_styles_the_selected_row_and_the_description_column() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let sel = Selector::new(
        "Pick",
        vec![SelectorItem::new("gpt", "gpt-4o").with_description("OpenAI")],
    );
    let themed = sel.render_lines_themed(80, &styles);

    // The selected row is wrapped whole: accent fg over the selectedBg
    // background (default 32-column primary column => 26 spaces of padding).
    let spacing = " ".repeat(26);
    assert_eq!(
        themed[2],
        format!("{SELECTED_BG}{ACCENT}❯ gpt-4o{spacing}OpenAI\x1b[39m\x1b[49m")
    );
    assert_eq!(strip_ansi_lines(&themed), sel.render_lines(80));
}

#[test]
fn selector_styles_a_non_selected_description_on_its_own() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let sel = Selector::new(
        "Pick",
        vec![
            SelectorItem::new("a", "Alpha").with_description("one"),
            SelectorItem::new("b", "Beta").with_description("two"),
        ],
    );
    let themed = sel.render_lines_themed(80, &styles);

    // Row 3 is not selected, so only the gap + description carry the muted fg.
    let spacing = " ".repeat(28); // 32-column primary column - "Beta" (4)
    assert_eq!(themed[3], format!("  Beta{MUTED}{spacing}two\x1b[39m"));
    assert_eq!(strip_ansi_lines(&themed), sel.render_lines(80));
}

#[test]
fn selector_styles_the_no_match_line() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let mut sel = Selector::new("Pick", items());
    sel.set_filter("zzz");
    let themed = sel.render_lines_themed(40, &styles);

    assert_eq!(themed[2], format!("{MUTED}  No matching items\x1b[39m"));
    assert_eq!(strip_ansi_lines(&themed), sel.render_lines(40));
}

#[test]
fn selector_styles_the_scroll_indicator() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let items = (0..12)
        .map(|i| SelectorItem::new(format!("item-{i}"), format!("Item {i}")))
        .collect();
    let sel = Selector::new("Pick", items).with_max_visible(5);
    let themed = sel.render_lines_themed(40, &styles);

    assert_eq!(themed[7], format!("{MUTED}  (1/12)\x1b[39m"));
    assert_eq!(strip_ansi_lines(&themed), sel.render_lines(40));
}

#[test]
fn status_bar_themed_layout_matches_the_plain_render() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");

    let plain = bar.render(&data, 60);
    let themed = bar.render_themed(&data, 60, &styles);

    assert_eq!(strip_ansi(&themed), plain);
    assert!(themed.contains(&format!("{ACCENT}gpt-4o\x1b[39m")));
    assert!(themed.contains(&format!("{MUTED}  abc-123  \x1b[39m")));
    // LUM-1467 — with no window and no usage the stats cluster is just the
    // trailing hint, and the model is the accent right side.
    assert!(themed.contains(&format!("{DIM}? for help\x1b[39m")));
    assert!(plain.ends_with("gpt-4o"));
}

#[test]
fn status_bar_colours_the_context_gauge_by_severity() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let bar = StatusBar::new();

    let mut hot = StatusData::new("gpt-4o", "abc-123").with_context_window(100_000);
    hot.context_used = 96_000;
    let themed = bar.render_themed(&hot, 60, &styles);
    assert!(
        themed.contains(&format!("{ERROR}96.0%/100k\x1b[39m")),
        "{themed:?}"
    );

    let mut warm = StatusData::new("gpt-4o", "abc-123").with_context_window(100_000);
    warm.context_used = 80_000;
    let themed = bar.render_themed(&warm, 60, &styles);
    assert!(
        themed.contains(&format!("{WARNING}80.0%/100k\x1b[39m")),
        "{themed:?}"
    );
}

#[test]
fn message_view_themed_layout_matches_the_plain_render() {
    let theme = dark();
    let styles = SelectListStyles::new(&theme);
    let mut view = MessageView::new();
    view.push(MessageItem::user("hello"));
    view.push(MessageItem::assistant("hi back"));
    view.push(MessageItem::tool("[tool:read] ok"));

    let themed = view.render_lines_themed(40, &styles);
    assert_eq!(strip_ansi_lines(&themed), view.render_lines(40));
    assert_eq!(themed[0], format!("{ACCENT}> \x1b[39m{TEXT}hello\x1b[39m"));
    assert_eq!(
        themed[2],
        format!("{MUTED}* \x1b[39m{MUTED}[tool:read] ok\x1b[39m")
    );
}

#[test]
fn a_plain_color_mode_emits_no_escape_sequences_from_any_component() {
    let theme = builtin_theme("dark", ColorMode::None).expect("plain dark theme");
    let styles = SelectListStyles::new(&theme);

    let mut view = MessageView::new();
    view.push(MessageItem::user("hello"));
    view.push(MessageItem::assistant("hi back"));
    view.begin_assistant_stream("gpt");

    let mut sel = Selector::new("Pick", items()).searchable(true);
    sel.set_filter("zzz");

    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");

    let mut lines = view.render_lines_themed(20, &styles);
    lines.extend(sel.render_lines_themed(40, &styles));
    lines.push(bar.render_themed(&data, 60, &styles));

    for line in lines {
        assert!(
            !line.contains('\x1b'),
            "ANSI leaked under ColorMode::None: {line:?}"
        );
    }
}

#[test]
fn the_plain_render_paths_are_unchanged() {
    // Guards the pre-existing `render_lines` / `render` contract the snapshot
    // tests rely on: adding the themed variants must not perturb plain output.
    let mut view = MessageView::new();
    view.push(MessageItem::user("hi"));
    view.push(MessageItem::assistant("hello"));
    assert_eq!(
        view.render_lines(40),
        vec!["> hi".to_string(), "  hello".to_string()]
    );

    let mut sel = Selector::new("Pick", items());
    sel.set_filter("zzz");
    assert_eq!(
        sel.render_lines(40),
        vec![
            "Pick".to_string(),
            "─".repeat(40),
            "  No matching items".to_string()
        ]
    );

    let bar = StatusBar::new();
    let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");
    let plain = bar.render(&data, 60);
    assert!(!plain.contains('\x1b'));
    // The model is the right side now (`footer.ts:230-240`).
    assert!(plain.ends_with("gpt-4o"), "{plain}");
}

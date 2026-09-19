//! Markdown rendering tests.
//!
//! These assert on the `ThemeColor::Md*` slots carried by the returned
//! [`StyledLine`]/[`StyledSpan`]s rather than on ANSI strings, so a palette
//! change cannot make them pass or fail for the wrong reason.

use pi_tui::markdown::{render_markdown, render_markdown_with_theme};
use pi_tui::message::{MessageItem, MessageView, Role};
use pi_tui::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeColor};

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

fn find<'a>(line: &'a StyledLine, text: &str) -> &'a StyledSpan {
    line.iter()
        .find(|span| span.text == text)
        .unwrap_or_else(|| panic!("no span {text:?} in {line:?}"))
}

fn slot(line: &StyledLine, text: &str) -> Option<ThemeColor> {
    find(line, text).style.fg
}

// ---------------------------------------------------------------------------
// Block level
// ---------------------------------------------------------------------------

#[test]
fn h1_is_heading_bold_underline() {
    let lines = render_markdown("# Title", 40);
    assert_eq!(texts(&lines), vec!["Title"]);
    let span = find(&lines[0], "Title");
    assert_eq!(span.style.fg, Some(ThemeColor::MdHeading));
    assert!(span.style.bold);
    assert!(span.style.underline);
}

#[test]
fn h2_is_heading_bold_only_and_h3_keeps_its_prefix() {
    let lines = render_markdown("## Sub", 40);
    let span = find(&lines[0], "Sub");
    assert_eq!(span.style.fg, Some(ThemeColor::MdHeading));
    assert!(span.style.bold);
    assert!(!span.style.underline);

    let lines = render_markdown("### Third", 40);
    assert_eq!(texts(&lines), vec!["### Third"]);
    let span = find(&lines[0], "### Third");
    assert_eq!(span.style.fg, Some(ThemeColor::MdHeading));
    assert!(span.style.bold);
}

#[test]
fn headings_up_to_level_six_and_closing_hashes() {
    let lines = render_markdown("###### Six\n\n## Closed ##", 40);
    assert_eq!(texts(&lines), vec!["###### Six", "", "Closed"]);
    assert_eq!(
        find(&lines[0], "###### Six").style.fg,
        Some(ThemeColor::MdHeading)
    );
    assert_eq!(
        find(&lines[2], "Closed").style.fg,
        Some(ThemeColor::MdHeading)
    );
}

#[test]
fn paragraphs_keep_line_breaks_and_are_separated() {
    let lines = render_markdown("line one\nline two\n\nsecond para", 40);
    assert_eq!(
        texts(&lines),
        vec!["line one", "line two", "", "second para"]
    );
}

#[test]
fn paragraph_wraps_at_the_requested_width() {
    let lines = render_markdown("alpha beta gamma delta", 11);
    assert_eq!(texts(&lines), vec!["alpha beta", "gamma delta"]);
}

#[test]
fn fenced_code_block_uses_language_on_the_border() {
    let lines = render_markdown("```rust\nfn main() {}\n```", 40);
    assert_eq!(texts(&lines), vec!["```rust", "  fn main() {}", "```"]);
    assert_eq!(
        slot(&lines[0], "```rust"),
        Some(ThemeColor::MdCodeBlockBorder)
    );
    assert_eq!(
        slot(&lines[1], "fn main() {}"),
        Some(ThemeColor::MdCodeBlock)
    );
    assert_eq!(slot(&lines[2], "```"), Some(ThemeColor::MdCodeBlockBorder));
}

#[test]
fn tilde_fence_renders_backtick_border_like_upstream() {
    let lines = render_markdown("~~~py\nx = 1\n~~~", 40);
    assert_eq!(texts(&lines), vec!["```py", "  x = 1", "```"]);
}

#[test]
fn unclosed_fence_runs_to_end_without_panicking() {
    // The closing border is still emitted, matching `renderCodeBlock`
    // upstream (`markdown.ts:535`).
    let lines = render_markdown("```rust\nfn main() {\n    todo!()", 40);
    assert_eq!(
        texts(&lines),
        vec!["```rust", "  fn main() {", "      todo!()", "```"]
    );
    assert_eq!(
        slot(&lines[1], "fn main() {"),
        Some(ThemeColor::MdCodeBlock)
    );
}

#[test]
fn unordered_and_nested_lists() {
    let lines = render_markdown("- a\n  - b\n- c", 40);
    assert_eq!(texts(&lines), vec!["- a", "  - b", "- c"]);
    for line in &lines {
        let bullet = line
            .iter()
            .find(|span| span.style.fg == Some(ThemeColor::MdListBullet))
            .expect("list bullet span");
        assert!(bullet.text.starts_with('-'));
    }
    // The nested bullet is indented by the parent marker width.
    assert_eq!(lines[1][0].text, "  ");
    assert_eq!(lines[1][1].text, "- ");
}

#[test]
fn ordered_list_renumbers_from_its_start() {
    let lines = render_markdown("3. a\n4. b", 40);
    assert_eq!(texts(&lines), vec!["3. a", "4. b"]);
}

#[test]
fn list_item_wraps_under_a_continuation_indent() {
    let lines = render_markdown("- alpha beta gamma", 10);
    assert_eq!(texts(&lines), vec!["- alpha", "  beta", "  gamma"]);
}

#[test]
fn blockquote_uses_quote_slots_and_border() {
    let lines = render_markdown("> quoted **bold**", 40);
    assert_eq!(texts(&lines), vec!["│ quoted bold"]);
    assert_eq!(lines[0][0].text, "│ ");
    assert_eq!(lines[0][0].style.fg, Some(ThemeColor::MdQuoteBorder));
    let quoted = find(&lines[0], "quoted ");
    assert_eq!(quoted.style.fg, Some(ThemeColor::MdQuote));
    assert!(quoted.style.italic);
    let bold = find(&lines[0], "bold");
    assert_eq!(bold.style.fg, Some(ThemeColor::MdQuote));
    assert!(bold.style.italic);
    assert!(bold.style.bold);
}

#[test]
fn horizontal_rule_uses_hr_slot_and_caps_at_80() {
    let lines = render_markdown("---", 40);
    assert_eq!(plain_text(&lines[0]), "─".repeat(40));
    assert_eq!(lines[0][0].style.fg, Some(ThemeColor::MdHr));

    let lines = render_markdown("---", 200);
    assert_eq!(plain_text(&lines[0]).chars().count(), 80);
}

// ---------------------------------------------------------------------------
// Inline level
// ---------------------------------------------------------------------------

#[test]
fn inline_emphasis_code_and_strikethrough() {
    let lines = render_markdown("plain **bold** *it* _it2_ `code` ~~del~~", 80);
    assert_eq!(texts(&lines), vec!["plain bold it it2 code del"]);

    assert!(find(&lines[0], "bold").style.bold);
    assert!(find(&lines[0], "it").style.italic);
    assert!(find(&lines[0], "it2").style.italic);
    let code = find(&lines[0], "code");
    assert_eq!(code.style.fg, Some(ThemeColor::MdCode));
    assert!(!code.style.bold);
    assert!(find(&lines[0], "del").style.strikethrough);
}

#[test]
fn underscore_does_not_split_a_word() {
    let lines = render_markdown("snake_case_name", 40);
    assert_eq!(texts(&lines), vec!["snake_case_name"]);
    assert!(lines[0].iter().all(|span| !span.style.italic));
}

#[test]
fn nested_emphasis_accumulates_slots() {
    let lines = render_markdown("**bold *and italic* here**", 40);
    assert_eq!(texts(&lines), vec!["bold and italic here"]);
    assert!(find(&lines[0], "bold ").style.bold);
    assert!(!find(&lines[0], "bold ").style.italic);
    let both = find(&lines[0], "and italic");
    assert!(both.style.bold && both.style.italic);
}

#[test]
fn unclosed_emphasis_is_literal() {
    let lines = render_markdown("**bold", 40);
    assert_eq!(texts(&lines), vec!["**bold"]);
    assert!(lines[0].iter().all(|span| !span.style.bold));
}

#[test]
fn backslash_escapes_are_literal() {
    let lines = render_markdown(r"\*not italic\*", 40);
    assert_eq!(texts(&lines), vec!["*not italic*"]);
    assert!(lines[0].iter().all(|span| !span.style.italic));
}

#[test]
fn link_renders_label_and_url_slots() {
    let lines = render_markdown("[pi](https://example.com)", 80);
    assert_eq!(texts(&lines), vec!["pi (https://example.com)"]);
    let label = find(&lines[0], "pi");
    assert_eq!(label.style.fg, Some(ThemeColor::MdLink));
    assert!(label.style.underline);
    assert_eq!(
        slot(&lines[0], " (https://example.com)"),
        Some(ThemeColor::MdLinkUrl)
    );
}

#[test]
fn link_with_matching_label_and_href_omits_the_url() {
    let lines = render_markdown("[https://x.dev](https://x.dev)", 80);
    assert_eq!(texts(&lines), vec!["https://x.dev"]);
    assert!(lines[0]
        .iter()
        .all(|span| span.style.fg != Some(ThemeColor::MdLinkUrl)));
}

#[test]
fn malformed_link_degrades_to_text() {
    let lines = render_markdown("[not a link", 40);
    assert_eq!(texts(&lines), vec!["[not a link"]);
}

// ---------------------------------------------------------------------------
// Width handling
// ---------------------------------------------------------------------------

#[test]
fn wide_glyphs_count_one_column_like_display_width() {
    let lines = render_markdown("你好世界", 2);
    assert_eq!(texts(&lines), vec!["你好", "世界"]);

    let lines = render_markdown("ab你c", 3);
    assert_eq!(texts(&lines), vec!["ab你", "c"]);
}

#[test]
fn mixed_cjk_and_ascii_wrap_without_panicking() {
    let lines = render_markdown("中文 mixed 中文字符", 6);
    assert_eq!(texts(&lines), vec!["中文", "mixed", "中文字符"]);
    assert!(lines
        .iter()
        .all(|line| plain_text(line).chars().count() <= 6));
}

#[test]
fn whitespace_only_input_renders_nothing() {
    assert!(render_markdown("   \n\t  ", 40).is_empty());
}

#[test]
fn special_character_soup_never_panics() {
    let alphabet = [
        '*', '_', '`', '~', '[', ']', '(', ')', '#', '>', '-', ' ', '\\', 'a', '\n',
    ];
    let mut cases: Vec<String> = Vec::new();
    for a in alphabet {
        cases.push(a.to_string());
        for b in alphabet {
            cases.push(format!("{a}{b}"));
            for c in alphabet {
                cases.push(format!("{a}{b}{c}"));
            }
        }
    }

    for case in &cases {
        for width in [1usize, 3, 8] {
            let lines = render_markdown(case, width);
            assert!(
                lines.iter().all(|line| !plain_text(line).contains('\n')),
                "input {case:?} at width {width} leaked a newline"
            );
        }
    }
}

#[test]
fn malformed_and_streaming_input_never_panics() {
    let inputs = [
        "",
        "\n\n\n",
        "#",
        "# ",
        "###### ",
        "####### too many",
        "-",
        "- ",
        "1.",
        "1) ",
        ">",
        ">>>",
        "> > x",
        "```",
        "~~~",
        "```` ```",
        "[",
        "]",
        "[]",
        "[](",
        "[a]()",
        "[a](b",
        "**",
        "***",
        "****",
        "*",
        "_",
        "__",
        "~~",
        "~",
        "`",
        "``",
        "\\",
        "a\\",
        "\\*",
        "|a|b|",
        "$$x$$",
        "![img](url)",
        "<div>x</div>",
        "  - a\n\n- b",
        "- a\n\n  b",
        "1. a\n1) b",
        "> - x\n>   - y",
        "# h\n```\ncode",
        "text\n---",
        "text\n===",
        "\u{4e2d}\u{6587}**\u{52a0}\u{7c97}**\u{6d4b}\u{8bd5}",
        "\u{1f389} **x** \u{1f389}",
        "\t- tab",
    ];

    for input in inputs {
        for width in [0usize, 1, 2, 5, 80] {
            let lines = render_markdown(input, width);
            // Hard breaks are always emitted as separate lines, so no rendered
            // line may still contain a raw newline.
            assert!(
                lines.iter().all(|line| !plain_text(line).contains('\n')),
                "input {input:?} at width {width} leaked a newline"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Theme plumbing
// ---------------------------------------------------------------------------

#[test]
fn md_heading_resolves_through_the_theme_to_a_ratatui_style() {
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    let lines = render_markdown("# Title", 40);
    let style = find(&lines[0], "Title").style.to_style(&theme);
    assert_eq!(style.fg, Some(ratatui::style::Color::Rgb(240, 198, 116)));
    assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert!(style
        .add_modifier
        .contains(ratatui::style::Modifier::UNDERLINED));
}

#[test]
fn plain_theme_collapses_every_span_to_plain() {
    let theme = builtin_theme("dark", ColorMode::None).expect("plain theme");
    let lines = render_markdown_with_theme("# hi **bold** `code`", &theme, 40);
    assert!(!lines.is_empty());
    for span in lines.iter().flatten() {
        assert_eq!(span.style, SpanStyle::PLAIN);
    }
    assert_eq!(texts(&lines), vec!["hi bold code"]);
}

// ---------------------------------------------------------------------------
// MessageView wiring
// ---------------------------------------------------------------------------

#[test]
fn message_view_defaults_to_plain_text() {
    let mut view = MessageView::new();
    view.push(MessageItem::assistant("# Head"));
    assert!(!view.markdown());
    let lines = view.render_styled_lines(40);
    assert_eq!(texts(&lines), vec!["  # Head"]);
    assert!(lines
        .iter()
        .flatten()
        .all(|span| span.style.fg != Some(ThemeColor::MdHeading)));
}

#[test]
fn message_view_markdown_opt_in_themes_assistant_bodies() {
    let mut view = MessageView::new();
    view.set_markdown(true);
    view.push(MessageItem::assistant("# Head"));
    let lines = view.render_styled_lines(40);
    assert_eq!(texts(&lines), vec!["  Head"]);
    let heading = find(&lines[0], "Head");
    assert_eq!(heading.style.fg, Some(ThemeColor::MdHeading));
    assert!(heading.style.bold);
}

#[test]
fn message_view_markdown_leaves_user_and_tool_bodies_alone() {
    let mut view = MessageView::new().with_markdown(true);
    view.push(MessageItem::user("# Head"));
    view.push(MessageItem::tool("[tool:x] # Head"));
    let lines = view.render_styled_lines(60);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        find(&lines[0], "# Head").style.fg,
        Some(ThemeColor::UserMessageText)
    );
    assert_eq!(lines[1][0].text, "* ");
    assert!(find(&lines[1], "[tool:x] # Head").style.fg == Some(ThemeColor::ToolOutput));
}

#[test]
fn streaming_caret_sits_on_the_last_markdown_line() {
    let mut view = MessageView::new().with_markdown(true);
    view.push(MessageItem {
        role: Role::Assistant,
        text: "first\nsecond".to_string(),
        streaming: true,
    });
    let lines = view.render_styled_lines(40);
    assert_eq!(lines.len(), 2);
    assert!(plain_text(&lines[1]).ends_with('▍'));
    assert!(!plain_text(&lines[0]).contains('▍'));
    assert_eq!(
        lines[1].last().expect("caret span").style.fg,
        Some(ThemeColor::Dim)
    );
}

#[test]
fn markdown_message_view_renders_into_a_buffer() {
    let mut view = MessageView::new();
    view.set_markdown(true);
    view.push(MessageItem::assistant("# Title\n\nbody **bold**"));
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    view.render_to_buffer(area, &mut buf);
    let row0: String = (0..30)
        .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    assert!(row0.starts_with("  Title"));
}

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
    // Upstream funnels fenced code through `theme.highlightCode`, so the
    // language's tokens land on the `Syntax*` slots (see `highlight.rs`).
    assert_eq!(slot(&lines[1], "fn"), Some(ThemeColor::SyntaxKeyword));
    assert_eq!(slot(&lines[1], "main"), Some(ThemeColor::SyntaxFunction));
    // Text the highlighter does not classify keeps the code-block color.
    assert_eq!(slot(&lines[1], " "), Some(ThemeColor::MdCodeBlock));
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
    assert_eq!(slot(&lines[1], "fn"), Some(ThemeColor::SyntaxKeyword));
    assert_eq!(slot(&lines[1], " "), Some(ThemeColor::MdCodeBlock));
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
        '*', '_', '`', '~', '[', ']', '(', ')', '#', '>', '-', ' ', '\\', 'a', '\n', '|', ':',
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
        "$$",
        "$",
        "\\(",
        "$\\frac{1}{2}$",
        "$$\n\\frac{1}{2}",
        "\\(x",
        "\\[x\\]",
        "a $$\\sum_{i=0}^n$$ b",
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
// LaTeX
// ---------------------------------------------------------------------------

#[test]
fn latex_block_renders_in_display_mode() {
    let lines = render_markdown("$$\n\\frac{x^2+1}{x-1}\n$$", 40);
    assert_eq!(texts(&lines), vec!["x²+1", "────", "x-1"]);
}

#[test]
fn bracket_latex_block_renders_in_display_mode() {
    let lines = render_markdown("\\[\n\\sum_{i=0}^n x_i\n\\]", 40);
    assert_eq!(texts(&lines), vec![" n", " ∑  xᵢ", "i=0"]);
}

#[test]
fn single_line_dollar_block_is_a_display_block() {
    let lines = render_markdown("$$x^2+1$$", 40);
    assert_eq!(texts(&lines), vec!["x²+1"]);
}

#[test]
fn unsupported_latex_block_falls_back_to_its_source() {
    let lines = render_markdown("$$\n\\unknown{y}\n$$", 40);
    assert_eq!(texts(&lines), vec!["$$", "\\unknown{y}", "$$"]);
}

#[test]
fn unterminated_latex_blocks_stay_literal() {
    // An unterminated `$$` only stays a block when its body looks like math.
    let lines = render_markdown("$$\n\\frac{1}{2}", 40);
    assert_eq!(texts(&lines), vec!["$$", "\\frac{1}{2}"]);
    // `\[` is always pending.
    let lines = render_markdown("\\[\nx+1", 40);
    assert_eq!(texts(&lines), vec!["\\[", "x+1"]);
    // `$$` with no body at all is plain text.
    assert_eq!(texts(&render_markdown("$$", 40)), vec!["$$"]);
}

#[test]
fn inline_dollar_latex_renders() {
    let lines = render_markdown("value $x^2+1$ here", 40);
    assert_eq!(texts(&lines), vec!["value x²+1 here"]);
}

#[test]
fn inline_double_dollar_latex_renders() {
    let lines = render_markdown("a $$x^2$$ b", 40);
    assert_eq!(texts(&lines), vec!["a x² b"]);
}

#[test]
fn inline_backslash_delimited_latex_renders() {
    assert_eq!(
        texts(&render_markdown("f \\(x\\) = 1", 40)),
        vec!["f x = 1"]
    );
    assert_eq!(texts(&render_markdown("x \\[y\\] z", 40)), vec!["x y z"]);
}

#[test]
fn dollar_latex_guards_keep_plain_dollar_text_literal() {
    assert_eq!(
        texts(&render_markdown("Price $5 and $6", 40)),
        vec!["Price $5 and $6"]
    );
    assert_eq!(texts(&render_markdown("$HOME$foo", 40)), vec!["$HOME$foo"]);
    assert_eq!(texts(&render_markdown("$ e $", 40)), vec!["$ e $"]);
}

#[test]
fn latex_inside_headings_and_list_items_renders() {
    assert_eq!(texts(&render_markdown("# $x^2$", 40)), vec!["x²"]);
    assert_eq!(texts(&render_markdown("- $a_i$", 40)), vec!["- aᵢ"]);
}

#[test]
fn unsupported_inline_latex_falls_back_to_its_source() {
    let lines = render_markdown("a $\\unknown{y}$ b", 40);
    assert_eq!(texts(&lines), vec!["a $\\unknown{y}$ b"]);
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
        thinking: String::new(),
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

// ---------------------------------------------------------------------------
// Tables (GFM) — upstream `renderTable` in
// `packages/tui/src/components/markdown.ts:839-1015`.
// ---------------------------------------------------------------------------

/// The simplest table, byte-for-byte: natural column widths (the longest cell
/// per column), bold headers, a divider between the header and every body row.
#[test]
fn table_renders_a_box_grid_with_bold_headers() {
    let lines = render_markdown(
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
        80,
    );
    assert_eq!(
        texts(&lines),
        vec![
            "┌───────┬─────┐",
            "│ Name  │ Age │",
            "├───────┼─────┤",
            "│ Alice │ 30  │",
            "├───────┼─────┤",
            "│ Bob   │ 25  │",
            "└───────┴─────┘",
        ]
    );

    // Header cells are bold (`theme.bold(padded)`), body cells are not, and no
    // border carries a colour slot.
    let header = find(&lines[1], "Name ");
    assert!(header.style.bold);
    assert_eq!(header.style.fg, None);
    let body = &lines[3];
    assert!(plain_text(body).contains("Alice"));
    assert!(body.iter().all(|span| !span.style.bold));
    // The whole border is one plain span (`push_span` merges equal slots).
    assert_eq!(slot(&lines[0], "┌───────┬─────┐"), None);
}

/// One divider between the header and the first body row, one between each
/// pair of body rows, none after the last row (upstream "row dividers" case).
#[test]
fn table_dividers_separate_rows_but_not_the_bottom_border() {
    let lines = texts(&render_markdown(
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
        80,
    ));
    assert_eq!(lines.iter().filter(|line| line.contains('┼')).count(), 2);
    assert_eq!(lines.iter().filter(|line| line.starts_with('┌')).count(), 1);
    assert_eq!(lines.iter().filter(|line| line.starts_with('└')).count(), 1);
    assert!(lines[1].starts_with('│'));
    assert!(lines[2].starts_with('├'));
}

/// Port of upstream's "should keep column width at least the longest word":
/// at width 32 the long word drives the first column's floor.
#[test]
fn table_column_never_goes_below_its_longest_word() {
    let lines = render_markdown(
        "| Column One | Column Two |\n| --- | --- |\n| superlongword short | otherword |\n| small | tiny |",
        32,
    );
    let plain = texts(&lines);
    let data_line = plain
        .iter()
        .find(|line| line.contains("superlongword"))
        .expect("data row with the longest word");
    let segment = data_line.split('│').nth(1).expect("first column segment");
    let column_width = segment.chars().count() - 2;
    assert!(
        column_width >= "superlongword".chars().count(),
        "first column width {column_width} < 13 in {data_line:?}"
    );
    assert!(plain[0].chars().count() <= 32);
}

/// Port of upstream's "should wrap table cells when table exceeds available
/// width": nothing overflows the width, content survives the wrap, and the
/// grid stays aligned across the wrapped rows.
#[test]
fn table_wraps_cells_and_keeps_the_grid_aligned() {
    let lines = render_markdown(
        "| Command | Description | Example |\n| --- | --- | --- |\n| npm install | Install all dependencies | npm install |\n| npm run build | Build the project | npm run build |",
        50,
    );
    let plain = texts(&lines);
    for line in &plain {
        assert!(
            line.chars().count() <= 50,
            "line exceeds width 50: {line:?}"
        );
    }
    // Every grid line has the same width as the top border.
    let grid_width = plain[0].chars().count();
    for line in plain.iter().filter(|line| line.starts_with('│')) {
        assert_eq!(
            line.chars().count(),
            grid_width,
            "uneven grid line {line:?}"
        );
    }
    let joined = plain.join(" ");
    for needle in [
        "Command",
        "Description",
        "npm install",
        "Install",
        "Build the project",
    ] {
        assert!(joined.contains(needle), "missing {needle:?}");
    }
    // "Install all dependencies" is wrapped, not dropped.
    assert!(plain.iter().any(|line| line.contains("Install all")));
    assert!(plain.iter().any(|line| line.contains("dependencies")));
}

/// Too narrow for borders plus one column per cell: upstream replays
/// `token.raw`; this port wraps each raw source line instead of producing a
/// mangled grid.
#[test]
fn table_too_narrow_replays_the_raw_markdown() {
    let source = "| Command | Description | Example |\n| --- | --- | --- |\n| npm install | Install | npm install |";
    let lines = texts(&render_markdown(source, 12));
    assert!(!lines.iter().any(|line| line.contains('┌')));
    assert!(!lines.iter().any(|line| line.contains('│')));
    for line in &lines {
        assert!(line.chars().count() <= 12, "{line:?}");
    }
    // The raw source is present word by word.
    let joined = lines.join(" ");
    assert!(joined.contains("Command"));
    assert!(joined.contains("Description"));
    assert!(joined.contains("Example"));
    assert!(joined.contains("---"));
}

/// Alignment markers are accepted (and validated) but not rendered: upstream's
/// `renderTable` never reads `token.align`, so every column is left-aligned.
#[test]
fn table_alignment_markers_are_accepted_but_not_rendered() {
    let lines = texts(&render_markdown(
        "| Left | Center | Right |\n| :--- | :---: | ---: |\n| A | B | C |\n| Long text | Middle | End |",
        80,
    ));
    assert_eq!(
        lines,
        vec![
            "┌───────────┬────────┬───────┐",
            "│ Left      │ Center │ Right │",
            "├───────────┼────────┼───────┤",
            "│ A         │ B      │ C     │",
            "├───────────┼────────┼───────┤",
            "│ Long text │ Middle │ End   │",
            "└───────────┴────────┴───────┘",
        ]
    );
}

/// A body row is normalised to the header's column count (marked does the
/// same), and an optional outer pipe is not a cell.
#[test]
fn table_rows_are_normalised_to_the_header_width() {
    let lines = texts(&render_markdown(
        "| a | b |\n| --- | --- |\n| 1 | 2 | 3 |\n| 4 |",
        40,
    ));
    assert_eq!(
        lines,
        vec![
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ 1 │ 2 │",
            "├───┼───┤",
            "│ 4 │   │",
            "└───┴───┘",
        ]
    );
}

/// A delimiter row whose column count disagrees with the header does not start
/// a table — the lines stay literal text.
#[test]
fn table_requires_a_matching_delimiter_row() {
    let lines = texts(&render_markdown("| a | b |\n| --- |\n| 1 | 2 |", 40));
    assert_eq!(lines, vec!["| a | b |", "| --- |", "| 1 | 2 |"]);

    // A bare `---` is still a horizontal rule, not a delimiter row.
    let lines = texts(&render_markdown("text\n\n---", 40));
    assert_eq!(lines[2], "─".repeat(40));
}

/// Inside a block quote the table inherits the quote style: the quote border
/// stays, and cells carry `mdQuote`.
#[test]
fn table_inside_a_quote_keeps_the_quote_style() {
    let lines = render_markdown("> | a | b |\n> | --- | --- |\n> | 1 | 2 |", 40);
    assert_eq!(
        texts(&lines),
        vec![
            "│ ┌───┬───┐",
            "│ │ a │ b │",
            "│ ├───┼───┤",
            "│ │ 1 │ 2 │",
            "│ └───┴───┘",
        ]
    );
    for line in &lines {
        assert_eq!(
            line.first().expect("quote border span").style.fg,
            Some(ThemeColor::MdQuoteBorder)
        );
    }
    let cell = find(&lines[1], "a");
    assert_eq!(cell.style.fg, Some(ThemeColor::MdQuote));
    assert!(cell.style.italic);
    assert!(cell.style.bold);
}

/// A `\|` inside a cell is a literal pipe, and inline markup inside a cell
/// keeps its own slots while the column width still fits the whole cell.
#[test]
fn table_cells_keep_escaped_pipes_and_inline_slots() {
    let lines = render_markdown("| a \\| b | `c` |\n| --- | --- |\n| 1 | 2 |", 40);
    assert_eq!(
        texts(&lines),
        vec![
            "┌───────┬───┐",
            "│ a | b │ c │",
            "├───────┼───┤",
            "│ 1     │ 2 │",
            "└───────┴───┘",
        ]
    );
    // The header is bold, but the code cell keeps its `mdCode` slot.
    let code = find(&lines[1], "c");
    assert_eq!(code.style.fg, Some(ThemeColor::MdCode));
    assert!(code.style.bold);

    // A link renders its label and its URL, and the width accounts for both.
    let lines = render_markdown(
        "| `c` | [l](https://e.com/p) |\n| --- | --- |\n| 1 | 2 |",
        40,
    );
    let header = &lines[1];
    assert_eq!(find(header, "l").style.fg, Some(ThemeColor::MdLink));
    assert_eq!(
        find(header, " (https://e.com/p)").style.fg,
        Some(ThemeColor::MdLinkUrl)
    );
    let width = plain_text(&lines[0]).chars().count();
    assert_eq!(plain_text(header).chars().count(), width);
}

/// Upstream only adds a trailing blank line when another block follows, so a
/// table at the end of a document ends at its bottom border.
#[test]
fn table_ends_the_document_without_a_trailing_blank() {
    let lines = render_markdown("| a |\n| --- |\n| 1 |", 40);
    assert_eq!(texts(&lines).last().expect("last line"), "└───┘");

    let lines = render_markdown("| a |\n| --- |\n| 1 |\n\nafter", 40);
    assert_eq!(texts(&lines)[5], "");
    assert_eq!(texts(&lines)[6], "after");
}

/// Wide glyphs count one column (this crate's single width convention), so a
/// CJK table never overflows or panics.
#[test]
fn table_with_wide_glyphs_stays_within_the_width() {
    let lines = texts(&render_markdown(
        "| 名前 | 年齢 |\n| --- | --- |\n| あいうえお | 30 |",
        20,
    ));
    for line in &lines {
        assert!(line.chars().count() <= 20, "{line:?}");
    }
    assert!(lines[1].contains("名前"));
    assert!(lines[3].contains("あいうえお"));
    let grid_width = lines[0].chars().count();
    for line in lines.iter().filter(|line| line.starts_with('│')) {
        assert_eq!(
            line.chars().count(),
            grid_width,
            "uneven grid line {line:?}"
        );
    }
}

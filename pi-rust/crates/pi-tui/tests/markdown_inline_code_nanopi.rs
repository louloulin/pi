//! P19 — borrow nanopi's inline-`code` "fill + colour" pattern.
//!
//! nanopi renders inline `code` with `bg = Indexed(236)` and
//! `fg = Indexed(228)` — a sunburst yellow on near-black. The bg fill is
//! the load-bearing piece: without it, an inline `code` span is just
//! coloured prose and reads as ordinary text. With the fill, the eye
//! catches the run as a syntactic token, which is what every other
//! renderer does (md renderers, IDE tooltips, terminal emulators).
//!
//! The TS pi-tui applies only the foreground colour via `theme.code(text)`,
//! and pi-rust's prior `MdCode` slot did the same. The borrow here adds a
//! `MdCodeBg` ThemeBg slot that the inline-code path uses together with
//! the existing `MdCode` foreground, mirroring nanopi's two-slot pattern.
//!
//! Tests pin every leg of the borrow:
//!   1. A plain inline `code` span carries both `MdCode` fg and `MdCodeBg` bg.
//!   2. The bg fill does not leak into fenced code blocks (those have
//!      their own dedicated slot, `MdCodeBlock`).
//!   3. The bg fill does not leak into ordinary text (a paragraph without
//!      backticks has no `MdCodeBg`).
//!   4. Inside a heading, inline `code` still gets the bg fill — the fill
//!      is a property of the code span, not of its container.
//!   5. The `MdCodeBg` slot is wired through both themes (dark + light).
//!   6. A non-empty fallback exists for themes that omit `mdCodeBg`, so
//!      users who haven't customised still get a visible fill.

use pi_tui::markdown::render_markdown;
use pi_tui::styled::{plain_text, StyledLine};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeBg};

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(|line| plain_text(line)).collect()
}

fn style_at<'a>(line: &'a StyledLine, text: &str) -> &'a pi_tui::styled::SpanStyle {
    &line
        .iter()
        .find(|span| span.text == text)
        .unwrap_or_else(|| panic!("no span {text:?} in {line:?}"))
        .style
}

/// 1 — inline `code` carries `MdCode` fg + `MdCodeBg` bg. The fg was the
/// pre-borrow behaviour; the bg is the new piece.
#[test]
fn inline_code_carries_md_code_fg_and_md_code_bg() {
    let lines = render_markdown("`code`", 40);
    assert_eq!(texts(&lines), vec!["code"]);
    let style = style_at(&lines[0], "code");
    assert_eq!(style.fg, Some(pi_tui::theme::ThemeColor::MdCode));
    assert_eq!(style.bg, Some(ThemeBg::MdCodeBg));
}

/// 2 — fenced code blocks stay on `MdCodeBlock` and never pick up the
/// inline-code bg. The bg fill is scoped to backtick spans, not to "any
/// code-shaped thing".
#[test]
fn fenced_code_block_does_not_pick_up_the_inline_bg() {
    let lines = render_markdown("```\ncode\n```", 40);
    // The fence delimiters use `MdCodeBlockBorder`; the inner code uses
    // `MdCodeBlock`. Neither should ever carry `MdCodeBg`.
    for line in &lines {
        for span in line {
            assert_ne!(
                span.style.bg,
                Some(ThemeBg::MdCodeBg),
                "fenced block must not inherit the inline-code bg (line text={:?})",
                texts(&[line.clone()]),
            );
        }
    }
}

/// 3 — a paragraph with no backticks contains no `MdCodeBg` bg fill.
/// The borrow is targeted: the bg must not leak into ordinary prose.
#[test]
fn plain_prose_does_not_carry_the_inline_code_bg() {
    let lines = render_markdown("just some plain text", 40);
    assert_eq!(texts(&lines), vec!["just some plain text"]);
    for span in &lines[0] {
        assert_ne!(span.style.bg, Some(ThemeBg::MdCodeBg));
    }
}

/// 4 — heading containers do not suppress the inline-code bg. The fill
/// travels with the code span, so an `H1` like `# Use `cargo`` still
/// renders its backticked word with the bg.
#[test]
fn inline_code_inside_a_heading_keeps_the_bg() {
    let lines = render_markdown("# Use `cargo`", 40);
    // Find the rendered line that contains `cargo` and assert its bg.
    let code_line = lines
        .iter()
        .find(|line| plain_text(line).contains("cargo"))
        .expect("heading render must contain the inline `cargo` span");
    let style = style_at(code_line, "cargo");
    assert_eq!(style.bg, Some(ThemeBg::MdCodeBg));
}

/// 5 — the `MdCodeBg` slot is reachable through the theme of both built-in
/// themes. This is the wiring test: it proves the slot isn't just declared
/// in Rust, it is reachable from a parsed theme.json.
#[test]
fn md_code_bg_is_resolved_by_dark_and_light_themes() {
    for name in ["dark", "light"] {
        let theme = builtin_theme(name, ColorMode::TrueColor)
            .unwrap_or_else(|e| panic!("theme {name} failed to load: {e:?}"));
        // The slot must resolve to *some* color value — hex string,
        // indexed, or rgb. We only assert the lookup doesn't panic and
        // returns a usable color, not that two themes produce the same
        // value (each can pick its own fill).
        let value = theme
            .bg_value(ThemeBg::MdCodeBg)
            .unwrap_or_else(|| panic!("{name}: MdCodeBg must resolve to a ColorValue"));
        let _ = format!("{value:?}");
    }
}

/// 6 — the `MdCodeBg` slot declares `ToolPendingBg` as its fallback. We
/// can't easily test the *resolved* fallback value from a built-in theme
/// (both themes now set `mdCodeBg` explicitly), so we verify the *slot
/// machinery* directly: the slot is optional and its fallback points at
/// the right neighbour. Without this wiring, themes that omit
/// `mdCodeBg` would render inline code with no fill.
#[test]
fn md_code_bg_declares_tool_pending_bg_as_its_fallback() {
    assert!(
        ThemeBg::MdCodeBg.is_optional(),
        "MdCodeBg must be optional so themes can omit it without failing validation",
    );
    assert_eq!(
        ThemeBg::MdCodeBg.fallback(),
        Some(ThemeBg::ToolPendingBg),
        "MdCodeBg's fallback must be ToolPendingBg so the borrow keeps working even for themes that don't override it",
    );
}
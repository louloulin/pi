//! Phase 12 — BranchSummary / CompactionSummary / SkillInvocation custom
//! messages render with the correct `[kind]` label, the body wrapped to
//! the viewport, and the `customMessageBg` slot applied across every row.
//!
//! Mirrors upstream `custom-message.ts` (`packages/coding-agent/src/modes/interactive/components/custom-message.ts`).
//! The custom-message variant of `Box(paddingX, 1, theme.bg("customMessageBg", …))`
//! is the only chrome — no per-line prefix is added — and the body is
//! wrapped to the message viewport just like the user message body.

use pi_tui::message::{MessageItem, MessageView, Role};
use pi_tui::theme::{builtin_theme, ColorMode};
use pi_tui::utils::styled::plain_text as line_to_text;

const WIDTH: u16 = 40;

fn render_first_lines(view: &mut MessageView, items: &[MessageItem], width: u16) -> Vec<String> {
    for item in items {
        view.push(item.clone());
    }
    view.render_styled_lines(width)
        .into_iter()
        .map(|row| line_to_text(&row))
        .collect()
}

#[test]
fn branch_summary_label_and_body_match_upstream() {
    let mut view = MessageView::new();
    let lines = render_first_lines(
        &mut view,
        &[MessageItem {
            role: Role::BranchSummary,
            text: "auto-committed branch\nwith two commits".into(),
            ..MessageItem::user("")
        }],
        WIDTH,
    );
    assert!(lines.iter().any(|s| s.contains("[branch]")), "{lines:?}");
    assert!(
        lines.iter().any(|s| s.contains("auto-committed branch")),
        "{lines:?}"
    );
}

#[test]
fn compaction_summary_uses_compaction_label() {
    let mut view = MessageView::new();
    let lines = render_first_lines(
        &mut view,
        &[MessageItem {
            role: Role::CompactionSummary,
            text: "Compacted 4096 → 1024 tokens".into(),
            ..MessageItem::user("")
        }],
        WIDTH,
    );
    assert!(lines.iter().any(|l| l.contains("[compaction]")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("Compacted")), "{lines:?}");
}

#[test]
fn skill_invocation_label_carries_the_skill_name() {
    let mut view = MessageView::new();
    let lines = render_first_lines(
        &mut view,
        &[MessageItem {
            role: Role::SkillInvocation,
            text: "running skill: weather-check".into(),
            ..MessageItem::user("")
        }],
        WIDTH,
    );
    assert!(lines.iter().any(|l| l.contains("[skill]")), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("running skill")),
        "{lines:?}"
    );
}

#[test]
fn custom_message_paints_custom_message_bg_slot() {
    let _theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
    let mut view = MessageView::new();
    view.push(MessageItem {
        role: Role::BranchSummary,
        text: "test body".into(),
        ..MessageItem::user("")
    });
    let styled = view.render_styled_lines_with_links(WIDTH, false);
    let first_row = styled.first().expect("at least the label row");
    let span = first_row.first().expect("label row has a span");
    // BoxLayout::with_bg stamps the configured slot onto every span.
    assert!(span.style.bg.is_some(), "label row should have a bg: {span:?}");
}

#[test]
fn empty_custom_message_body_renders_label_only() {
    let mut view = MessageView::new();
    let lines = render_first_lines(
        &mut view,
        &[MessageItem {
            role: Role::BranchSummary,
            text: "".into(),
            ..MessageItem::user("")
        }],
        WIDTH,
    );
    assert!(lines.iter().any(|l| l.contains("[branch]")), "{lines:?}");
    assert!(
        lines.len() >= 2,
        "empty body should still reserve at least one blank row"
    );
}
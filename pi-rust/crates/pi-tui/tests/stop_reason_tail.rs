//! P21 (C3.6) — Assistant stop-reason tail line.
//!
//! Upstream paints a one-line status row under a finalized assistant
//! block when the provider reported an abnormal `StopReason`
//! (`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`,
//! the trailing `stop_reason` glyph). The Rust port mirrors the same
//! six arms through [`stop_reason_tail_line`]:
//!
//!   - `Stop` and `ToolUse` are the "happy" arms — no tail is painted,
//!     so the helper returns an empty `Vec<StyledSpan>` and the call
//!     site skips appending a row.
//!   - `MaxTokens` paints a red `⚠ max_tokens` warning with a bold
//!     modifier so the cut-off is obvious.
//!   - `Aborted` paints a dim `⏹ aborted` — visible without looking
//!     like an error.
//!   - `Error` paints a red `✗ error` — generation failure.
//!   - `Empty` paints a dim `(empty response)` — usually a routing /
//!     configuration issue.
//!
//! The tests below pin every arm at the helper level (the glyph /
//! colour) and at the integration level (the wire point in
//! `MessageView::item_lines` — the tail only appears on finalized
//! assistant items, with the right prefix, and never leaks onto user /
//! tool / streaming blocks).

use pi_protocol::StopReason;
use pi_tui::message::{MessageItem, MessageView, ToolStatus};
use pi_tui::styled::{plain_text, SpanStyle};
use pi_tui::ThemeColor;

/// Helper — render the tail through the public [`stop_reason_tail_line`]
/// then collapse it to plain text. Avoids dragging in the full
/// `MessageView` for the helper-only checks below.
fn tail_text(reason: StopReason) -> String {
    pi_tui::message::stop_reason_tail_line(reason)
        .iter()
        .map(|s| s.text.clone())
        .collect::<Vec<_>>()
        .join("")
}

fn tail_styles(reason: StopReason) -> Vec<SpanStyle> {
    pi_tui::message::stop_reason_tail_line(reason)
        .into_iter()
        .map(|s| s.style)
        .collect()
}

/// 1 — `Stop` is the happy arm: the helper must produce an empty
/// `Vec` so the call site does not append a row.
#[test]
fn stop_reason_tail_stop_is_empty() {
    assert!(pi_tui::message::stop_reason_tail_line(StopReason::Stop).is_empty());
}

/// 2 — `ToolUse` is also the happy arm — the helper must not produce
/// any spans.
#[test]
fn stop_reason_tail_tool_use_is_empty() {
    assert!(
        pi_tui::message::stop_reason_tail_line(StopReason::ToolUse).is_empty(),
        "ToolUse must not paint a tail — the body already shows the call"
    );
}

/// 3 — `MaxTokens` is the canonical "model hit the cap" arm: a red
/// `⚠ max_tokens` row with a bold modifier.
#[test]
fn stop_reason_tail_max_tokens_is_a_red_bold_warning() {
    let spans = pi_tui::message::stop_reason_tail_line(StopReason::MaxTokens);
    assert_eq!(spans.len(), 1, "MaxTokens must render exactly one span");
    assert_eq!(spans[0].text, "⚠ max_tokens");
    assert_eq!(spans[0].style.fg, Some(ThemeColor::Error));
    assert!(
        spans[0].style.bold,
        "MaxTokens must be bold so the warning stands out"
    );
}

/// 4 — `Aborted` is the user-cancellation arm: a dim `⏹ aborted` row
/// (visible, but not loud).
#[test]
fn stop_reason_tail_aborted_is_dim() {
    assert_eq!(tail_text(StopReason::Aborted), "⏹ aborted");
    let styles = tail_styles(StopReason::Aborted);
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].fg, Some(ThemeColor::Dim));
    assert!(!styles[0].bold, "Aborted must not be bold — only an FYI");
}

/// 5 — `Error` is the provider-reported failure arm: a red `✗ error`
/// row (no bold, so it reads as a status row rather than a warning).
#[test]
fn stop_reason_tail_error_is_red() {
    assert_eq!(tail_text(StopReason::Error), "✗ error");
    let styles = tail_styles(StopReason::Error);
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].fg, Some(ThemeColor::Error));
    assert!(
        !styles[0].bold,
        "Error must not be bold — MaxTokens owns the bold modifier"
    );
}

/// 6 — `Empty` is the "model finished with no content" arm: a dim
/// `(empty response)` row.
#[test]
fn stop_reason_tail_empty_is_dim() {
    assert_eq!(tail_text(StopReason::Empty), "(empty response)");
    let styles = tail_styles(StopReason::Empty);
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].fg, Some(ThemeColor::Dim));
    assert!(!styles[0].bold);
}

/// 7 — The end-to-end integration check: a finalized assistant item
/// with `MaxTokens` must surface the warning row at the tail of the
/// rendered output, after the body and with the assistant's `"  "`
/// prefix.
#[test]
fn finalized_assistant_with_max_tokens_paints_the_warning_row() {
    let mut view = MessageView::new();
    view.push(
        MessageItem::assistant("here is the answer").with_stop_reason(StopReason::MaxTokens),
    );
    let lines = view.render_styled_lines(40);
    assert!(
        !lines.is_empty(),
        "the rendered output must contain at least one row"
    );
    let last = lines.last().expect("rendered lines");
    let last_text = plain_text(last);
    assert!(
        last_text.contains("⚠ max_tokens"),
        "MaxTokens tail must end the rendered output; got {last_text:?}"
    );
    // The assistant prefix is two spaces — same column the body uses.
    let first_span = &last[0];
    assert_eq!(first_span.text, "  ");
}

/// 8 — A finalized assistant item with `Stop` (the happy arm) must
/// **not** append a tail row — the body alone is the entire block.
#[test]
fn finalized_assistant_with_stop_does_not_paint_a_tail() {
    let mut view = MessageView::new();
    view.push(
        MessageItem::assistant("the model said hello").with_stop_reason(StopReason::Stop),
    );
    let lines = view.render_styled_lines(40);
    let combined: String = lines.iter().map(|l| plain_text(l.as_slice())).collect::<Vec<_>>().join("\n");
    assert!(
        !combined.contains("⚠") && !combined.contains("⏹") && !combined.contains("✗"),
        "Stop must not paint any tail; got {combined:?}"
    );
    assert!(
        !combined.contains("(empty response)"),
        "Stop must not paint any tail; got {combined:?}"
    );
}

/// 9 — A streaming assistant item must not paint a tail even if a
/// stop reason was carried over (the working header already conveys
/// "in flight"). The driver only sets a stop reason on
/// `MessageEnd`; this test guards the call site from prematurely
/// rendering a tail.
#[test]
fn streaming_assistant_with_max_tokens_does_not_paint_a_tail() {
    let mut view = MessageView::new();
    let mut item = MessageItem::assistant_streaming();
    item.text = "partial reply so".into();
    item.streaming = true;
    item.set_stop_reason(StopReason::MaxTokens);
    view.push(item);
    let lines = view.render_styled_lines(40);
    let combined: String = lines.iter().map(|l| plain_text(l.as_slice())).collect::<Vec<_>>().join("\n");
    assert!(
        !combined.contains("⚠"),
        "streaming blocks must not paint a tail even when a stop reason is set; got {combined:?}"
    );
    // The streaming caret indicator is the affordance instead.
    assert!(
        combined.contains('▍'),
        "streaming caret must still land on the body; got {combined:?}"
    );
}

/// 10 — A non-assistant role must not paint a tail — the helper is
/// Assistant-only. Build a tool block with a stop reason set (even
/// though the field is ignored) and confirm the rendered output does
/// not contain any of the tail glyphs.
#[test]
fn non_assistant_role_does_not_paint_a_tail() {
    let mut view = MessageView::new();
    let mut tool = MessageItem::tool("[tool:bash] done");
    tool.tool_status = ToolStatus::Success;
    // Even with a stop reason set, the call site only paints the tail
    // for finalized assistant items.
    tool.set_stop_reason(StopReason::MaxTokens);
    view.push(tool);
    let lines = view.render_styled_lines(40);
    let combined: String = lines.iter().map(|l| plain_text(l.as_slice())).collect::<Vec<_>>().join("\n");
    assert!(
        !combined.contains("⚠"),
        "tool blocks must not paint a stop-reason tail; got {combined:?}"
    );
}

/// 11 — `with_stop_reason` builder chains like the other `with_*`
/// builders on `MessageItem`.
#[test]
fn with_stop_reason_sets_the_field() {
    let item = MessageItem::assistant("body")
        .with_stop_reason(StopReason::Error);
    assert_eq!(item.stop_reason, Some(StopReason::Error));
}

/// 12 — `set_stop_reason` mutates the field in place.
#[test]
fn set_stop_reason_updates_the_field() {
    let mut item = MessageItem::assistant("body");
    assert_eq!(item.stop_reason, None);
    item.set_stop_reason(StopReason::Empty);
    assert_eq!(item.stop_reason, Some(StopReason::Empty));
}

/// 13 — Default constructors initialize the field to `None`, so
/// existing call sites that pre-date the field keep their byte
/// identical rendering.
#[test]
fn default_constructors_leave_stop_reason_unset() {
    assert_eq!(MessageItem::user("u").stop_reason, None);
    assert_eq!(MessageItem::assistant("a").stop_reason, None);
    assert_eq!(MessageItem::assistant_streaming().stop_reason, None);
    assert_eq!(MessageItem::tool("t").stop_reason, None);
    assert_eq!(MessageItem::tool_pending("p").stop_reason, None);
    assert_eq!(MessageItem::tool_error("e").stop_reason, None);
    let notice = MessageItem::notice(Vec::new());
    assert_eq!(notice.stop_reason, None);
}

/// 14 — `Role::User` items must not paint a stop-reason tail even
/// when the field is set — the body has its own status row upstream.
/// Pin the role short-circuit at the integration boundary.
#[test]
fn user_role_does_not_paint_a_tail() {
    let mut view = MessageView::new();
    let item = MessageItem::user("hello").with_stop_reason(StopReason::Error);
    view.push(item);
    let lines = view.render_styled_lines(40);
    let combined: String = lines.iter().map(|l| plain_text(l.as_slice())).collect::<Vec<_>>().join("\n");
    assert!(
        !combined.contains("⚠")
            && !combined.contains("⏹")
            && !combined.contains("✗"),
        "user blocks must not paint a stop-reason tail; got {combined:?}"
    );
}

/// 15 — `Role::Tool` (already covered by test 10) and other roles
/// must skip the tail; the helper is Assistant-only. Confirm by
/// rendering a tool block with `StopReason::MaxTokens` set: the
/// rendered output must not contain the `⚠` glyph.
#[test]
fn tool_role_does_not_paint_a_tail_integration() {
    let mut view = MessageView::new();
    let mut tool = MessageItem::tool("[tool:bash] done");
    tool.tool_status = ToolStatus::Success;
    tool.set_stop_reason(StopReason::MaxTokens);
    view.push(tool);
    let lines = view.render_styled_lines(40);
    let combined: String = lines.iter().map(|l| plain_text(l.as_slice())).collect::<Vec<_>>().join("\n");
    assert!(
        !combined.contains("⚠"),
        "tool blocks must not paint a stop-reason tail; got {combined:?}"
    );
}
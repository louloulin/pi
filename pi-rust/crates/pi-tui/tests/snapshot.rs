//! Snapshot tests for `MessageView` and the `App` rendering pipeline.
//!
//! These tests exercise the rendering path against a `Buffer` so a
//! future Stage 5+ change that perturbs the layout surfaces a diff in
//! CI. They intentionally avoid `insta` so the workspace has zero
//! non-workspace dependencies added by Stage 4 — the snapshot is
//! inlined as an expected `Vec<String>` and the comparison happens
//! inside the test body.

use pi_tui::message::{MessageItem, MessageView};

fn strip_trailing(line: &str) -> String {
    line.trim_end().to_string()
}

#[test]
fn empty_view_renders_one_blank_line() {
    let view = MessageView::new();
    let lines: Vec<String> = view
        .render_lines(20)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines, vec![""]);
}

#[test]
fn user_message_prefix_and_wrap() {
    let mut view = MessageView::new();
    view.push(MessageItem::user("hello world"));
    let lines: Vec<String> = view
        .render_lines(20)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines, vec!["> hello world"]);
}

#[test]
fn assistant_message_no_user_prefix() {
    let mut view = MessageView::new();
    view.push(MessageItem::assistant("hi back"));
    let lines: Vec<String> = view
        .render_lines(20)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines, vec!["  hi back"]);
}

#[test]
fn tool_block_uses_asterisk() {
    let mut view = MessageView::new();
    view.push_tool("read", "{\"path\":\"/tmp/x\"}", "ok", false);
    // Wide enough to keep the whole body on one line.
    let lines: Vec<String> = view
        .render_lines(60)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("* [tool:read]"));
    assert!(lines[0].contains("{\"path\":\"/tmp/x\"}"));
    assert!(lines[0].contains("→ ok"));
}

#[test]
fn streaming_assistant_gets_caret_indicator() {
    let mut view = MessageView::new();
    view.begin_assistant_stream("gpt");
    view.append_assistant_delta("hello");
    let lines: Vec<String> = view
        .render_lines(40)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    // The streaming header ("~ Working ") sits above the body to mirror
    // upstream `assistant-message.ts`, so two lines are now expected
    // instead of one. The caret indicator `▍` lands on the body row.
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("Working"));
    assert!(lines[1].contains("[gpt]hello"));
    assert!(lines[1].ends_with('▍'));
    // Finalizing removes the caret and the streaming header.
    view.end_assistant_stream();
    let lines: Vec<String> = view
        .render_lines(40)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("[gpt]hello"));
    assert!(!lines[0].ends_with('▍'));
}

#[test]
fn long_text_wraps_within_width() {
    let mut view = MessageView::new();
    view.push(MessageItem::user("a".repeat(40)));
    let lines: Vec<String> = view
        .render_lines(10)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    // Width 10, prefix ">" + space eats 2 columns, so the body wraps
    // at 8 columns. 40 chars across 8 cols = 5 wrapped lines.
    assert_eq!(lines.len(), 5);
    for line in &lines {
        assert!(line.starts_with("> "));
        // Body length (after the prefix) must be <= 8.
        assert!(line.chars().count() <= 10);
    }
}

#[test]
fn render_to_buffer_matches_lines_layout() {
    let mut view = MessageView::new();
    view.push(MessageItem::user("alpha"));
    view.push(MessageItem::assistant("beta"));

    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    view.render_to_buffer(area, &mut buf);

    let row0: String = (0..area.width)
        .map(|x| {
            buf.cell((x, 0))
                .unwrap()
                .symbol()
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect();
    let row1: String = (0..area.width)
        .map(|x| {
            buf.cell((x, 1))
                .unwrap()
                .symbol()
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect();
    assert_eq!(row0.trim_end(), "> alpha");
    assert_eq!(row1.trim_end(), "  beta");
}

#[test]
fn scroll_offset_skips_lines_from_tail() {
    let mut view = MessageView::new();
    for i in 0..5 {
        view.push(MessageItem::user(format!("msg {i}")));
    }
    view.scroll_up();
    view.scroll_up();
    // The view holds the lines; the App slices them by height. We
    // only assert the scroll_offset counter moved.
    assert_eq!(view.scroll_offset(), 2);
    view.scroll_to_bottom();
    assert_eq!(view.scroll_offset(), 0);
}

#[test]
fn info_message_uses_user_prefix() {
    let mut view = MessageView::new();
    view.push_info("session ready");
    let lines: Vec<String> = view
        .render_lines(20)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert_eq!(lines, vec!["> session ready"]);
}

#[test]
fn update_available_notice_builds_a_bordered_block() {
    let mut view = MessageView::new();
    view.push_update_notice("1.2.3", None, "https://pi.dev/changelog", "pi", 60);
    let lines: Vec<String> = view
        .render_lines(60)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    // TS pi-tui's `showNewVersionNotification` — top border, bold header,
    // instruction line with the action, changelog link, bottom border.
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines[0].chars().all(|c| c == '─') && !lines[0].is_empty());
    assert!(lines[1].contains("Update Available"));
    assert!(lines[2].contains("1.2.3") && lines[2].contains("pi update"));
    assert!(lines[3].contains("Changelog") && lines[3].contains("https://pi.dev/changelog"));
    assert_eq!(lines[4].chars().all(|c| c == '─') && !lines[4].is_empty(), true);
}

#[test]
fn update_available_notice_with_note_renders_the_body_block() {
    let mut view = MessageView::new();
    view.push_update_notice(
        "2.0.0",
        Some("Highlights:\n- faster startup\n- new tui"),
        "https://pi.dev/changelog",
        "pi",
        40,
    );
    let lines: Vec<String> = view
        .render_lines(40)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    // divider, header, instruction, blank, body×3, blank, link, divider.
    assert!(lines[0].chars().all(|c| c == '─'));
    assert!(lines[1].contains("Update Available"));
    assert!(lines.iter().any(|l| l.contains("Highlights")));
    assert!(lines.iter().any(|l| l.contains("faster startup")));
    assert!(lines.iter().any(|l| l.contains("new tui")));
    // Borders bracket the block. `strip_trailing` keeps every divider row
    // (─ is not trailing whitespace), so the exact count is the divider
    // count from the builder.
    let divider_count = lines
        .iter()
        .filter(|l| !l.is_empty() && l.chars().all(|c| c == '─'))
        .count();
    assert_eq!(divider_count, 2, "{lines:?}");
}

#[test]
fn package_update_notice_lists_packages_with_dash_prefix() {
    let mut view = MessageView::new();
    view.push_package_update_notice(
        &[
            "@pi/foo".to_string(),
            "@pi/bar".to_string(),
        ],
        "pi",
        40,
    );
    let lines: Vec<String> = view
        .render_lines(40)
        .into_iter()
        .map(|l| strip_trailing(&l))
        .collect();
    assert!(lines.iter().any(|l| l.contains("Package Updates Available")));
    assert!(lines.iter().any(|l| l.contains("pi update --extensions")));
    assert!(lines.iter().any(|l| l.starts_with("- @pi/foo")));
    assert!(lines.iter().any(|l| l.starts_with("- @pi/bar")));
    assert_eq!(
        lines.iter().filter(|l| l.chars().all(|c| c == '─')).count(),
        2
    );
}

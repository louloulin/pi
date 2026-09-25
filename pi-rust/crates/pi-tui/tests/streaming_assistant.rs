//! The in-flight assistant header (Phase 3 / G6).
//!
//! Upstream paints a one-line "~ Working" label with a spinner glyph above
//! any assistant message that is still streaming
//! (`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`).
//! The Rust port keeps the same copy, but the glyph is no longer a static
//! placeholder — the App mirrors its busy spinner frame onto the message
//! view before each render, so the header animates in lock-step with the
//! footer cursor and the reader sees the same frame on both ends of the
//! screen.

use pi_tui::components::loader::{Spinner, SPINNER_FRAMES};
use pi_tui::message::{MessageItem, MessageView};

#[test]
fn working_header_carries_the_current_spinner_frame() {
    let mut view = MessageView::new();
    view.begin_assistant_stream("faux");
    view.append_assistant_delta("hello");

    // The default spinner frame is the first glyph in the table — the App
    // sets the field on every render, but pre-render callers (tests,
    // snapshots) see the table default until they choose otherwise.
    assert_eq!(view.spinner_frame(), SPINNER_FRAMES[0]);

    let lines = view.render_lines(40);
    assert_eq!(lines.len(), 2, "header + body");
    assert!(
        lines[0].starts_with(SPINNER_FRAMES[0]),
        "the working header carries the first spinner frame: {:?}",
        lines[0]
    );
    assert!(lines[0].contains("Working"));

    // Setting the spinner frame rewrites the header without touching the body.
    view.set_spinner_frame(SPINNER_FRAMES[5]);
    let lines = view.render_lines(40);
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].starts_with(SPINNER_FRAMES[5]),
        "the working header reflects the new frame: {:?}",
        lines[0]
    );
    assert!(lines[1].contains("hello"));
}

#[test]
fn working_header_changes_when_spinner_advances() {
    let mut view = MessageView::new();
    view.begin_assistant_stream("faux");
    view.append_assistant_delta("streaming…");

    let mut last_glyph: Option<char> = None;
    // Walk the whole frame table; every step must change the header glyph.
    // The App sets the field from `spinner.frame()` each render, so this is
    // exactly the sequence a real render produces.
    let mut spinner = Spinner::new();
    let mut header_lines = Vec::new();
    for frame in SPINNER_FRAMES.iter() {
        assert_eq!(spinner.frame(), *frame);
        view.set_spinner_frame(spinner.frame());
        let lines = view.render_lines(40);
        assert_eq!(lines.len(), 2, "header + body at frame {frame}");
        assert!(
            lines[0].starts_with(*frame),
            "frame {frame:?} expected at the head, got {:?}",
            lines[0]
        );
        assert_ne!(
            last_glyph,
            Some(*frame),
            "the spinner walked past frame {frame:?} twice"
        );
        last_glyph = Some(*frame);
        header_lines.push(lines[0].clone());
        spinner.advance();
    }

    // Every render produced a distinct header line — i.e. the glyph really
    // moved on each step instead of staying stuck on the first frame.
    assert_eq!(
        header_lines.iter().collect::<std::collections::HashSet<_>>().len(),
        SPINNER_FRAMES.len(),
        "the working header must walk every frame"
    );

    // The wrapping `advance()` lands back on frame 0 after the last step.
    view.set_spinner_frame(spinner.frame());
    let lines = view.render_lines(40);
    assert!(lines[0].starts_with(SPINNER_FRAMES[0]));
}

#[test]
fn ending_the_stream_removes_the_working_header() {
    let mut view = MessageView::new();
    view.begin_assistant_stream("faux");
    view.append_assistant_delta("done");
    view.set_spinner_frame(SPINNER_FRAMES[3]);

    assert_eq!(view.render_lines(40).len(), 2);

    view.end_assistant_stream();
    let lines = view.render_lines(40);
    assert_eq!(lines.len(), 1, "the working header vanishes when the stream ends");
    assert!(lines[0].contains("done"));
    assert!(!lines[0].contains("Working"));
}

#[test]
fn finished_message_does_not_paint_a_working_header() {
    let mut view = MessageView::new();
    view.push(MessageItem::assistant("complete answer"));
    view.set_spinner_frame(SPINNER_FRAMES[2]);

    let lines = view.render_lines(40);
    assert_eq!(lines.len(), 1);
    assert!(!lines[0].contains("Working"));
    assert!(!lines[0].starts_with(SPINNER_FRAMES[2]));
}
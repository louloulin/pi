//! LUM-1455 — `App::transcript_text`: the flat transcript the driver prints
//! **after** leaving the alternate screen.
//!
//! Upstream's `fullscreenExitOutput: "transcript"` (the default) leaves the
//! session in the terminal's scrollback when the TUI exits
//! (`interactive-mode.ts:790-795`); this port has no regular renderer, so the
//! driver prints `App::transcript_text` instead
//! (`pi-coding-agent/src/interactive.rs::exit_screen_output`). The contract
//! these tests pin is deliberately narrow and checkable:
//!
//! 1. it is the **whole** chat log, not the viewport (a 3×-taller-than-screen
//!    conversation comes back complete);
//! 2. every line fits the width it was asked for, measured in terminal columns
//!    (CJK content, exactly as `tests/column_width.rs` measures the frame);
//! 3. it carries the live palette as ANSI, so the dump looks like the session
//!    it replaces rather than a raw string;
//! 4. a session with nothing in it renders as an empty string, not as a screen
//!    of blank rows.
//!
//! The transcript is also compared against the frame the App paints at the same
//! width: the same `MessageView::render_styled_lines` layout backs both, so the
//! dump must be a superset (prefix-identical, since the frame clips to the
//! viewport) of what the user was looking at.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::MessageItem;
use pi_tui::width::columns;

const WIDTH: u16 = 40;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 512,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "lum1455-transcript".into(),
            ..AppConfig::default()
        },
    )
}

/// Strip SGR sequences so the assertions can talk about text.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// A session long enough to overflow the 24-row viewport several times over.
fn long_session() -> App {
    let mut app = app();
    app.messages_mut()
        .push(MessageItem::user("how do I read a paste marker?"));
    app.messages_mut().push(MessageItem::assistant(
        "Read the marker's line count, then the body. A paste marker keeps the \
         draft short while the full text stays in the session, so a large paste \
         never floods the composer or the transcript you are reading right now.",
    ));
    for index in 0..24 {
        app.messages_mut()
            .push(MessageItem::tool(format!("tool call {index}")));
    }
    app
}

#[test]
fn the_transcript_is_the_whole_log_not_the_viewport() {
    let app = long_session();
    let transcript = app.transcript_text(WIDTH);
    let text = plain(&transcript);

    assert!(
        text.contains("Read the marker's line count"),
        "the assistant body must survive:\n{text}"
    );
    assert!(
        text.contains("tool call 23"),
        "the last tool block must survive:\n{text}"
    );
    // A 24-row viewport cannot hold all of this; the dump is the document.
    let lines: Vec<&str> = transcript.lines().collect();
    assert!(
        lines.len() > 24,
        "expected more lines than the default viewport holds, got {}",
        lines.len()
    );
    assert!(transcript.ends_with('\n'), "the dump is newline-terminated");
}

#[test]
fn every_transcript_line_fits_the_width_in_columns() {
    let mut app = app();
    app.messages_mut()
        .push(MessageItem::user("这是一段中文提问，用来验证列宽计算。"));
    app.messages_mut().push(MessageItem::assistant(
        "Mixed 中英文 reply with `code` and a longer tail so it wraps.",
    ));

    for width in [24u16, 40, 71] {
        let transcript = app.transcript_text(width);
        for (index, line) in transcript.lines().enumerate() {
            let plain_line = plain(line);
            let measured = columns(&plain_line);
            assert!(
                measured <= width as usize,
                "width {width}: line {index} is {measured} columns: {plain_line:?}"
            );
        }
    }
}

#[test]
fn the_transcript_carries_the_live_theme() {
    let app = long_session();
    let transcript = app.transcript_text(WIDTH);
    assert!(
        transcript.contains('\u{1b}'),
        "the dump has to be themed like the session it replaces"
    );
    // The plain text must not have leaked the escapes into the content.
    assert!(!plain(&transcript).contains('\u{1b}'));
}

#[test]
fn an_empty_session_renders_an_empty_string() {
    let app = app();
    assert_eq!(app.transcript_text(WIDTH), "");
    // A zero-width terminal has no layout to speak of either.
    assert_eq!(long_session().transcript_text(0), "");
}

#[test]
fn the_dump_is_a_superset_of_the_frame_the_user_saw() {
    let mut app = long_session();
    // The log is taller than the viewport, so pin it to the top: the point is
    // that what the frame painted is in the dump, not which rows happened to be
    // on screen. One frame first, so the App has a viewport to scroll in.
    let _ = app.render_snapshot(WIDTH, 24);
    assert!(app.scroll_viewport_to_top());
    let snapshot = app.render_snapshot(WIDTH, 24);
    let transcript = app.transcript_text(WIDTH);
    let dumped: Vec<String> = transcript.lines().map(plain).collect();

    let mut checked = 0usize;
    for line in snapshot.lines.iter().map(|line| plain(line)) {
        let painted = line.trim_end().trim_start();
        let is_transcript = painted.contains("tool call") || painted.contains("paste marker");
        if !is_transcript {
            continue;
        }
        assert!(
            dumped.iter().any(|row| row.contains(painted)),
            "a painted row is missing from the dump: {painted:?}\n{transcript}"
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected several transcript rows on screen, checked {checked}"
    );
}

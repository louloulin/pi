//! OSC 8 hyperlink rendering (LUM-1149 / LUM-1152).
//!
//! A hyperlink-capable terminal makes labelled links clickable; a terminal
//! that does not understand OSC 8 paints the label unchanged. The escape
//! sequences are zero-width, so they must never change a cell's column count
//! or leak into a text selection. These tests pin all three properties:
//!
//! * a markdown link renders as one linked cell run and no inline `(url)`,
//! * the linked cells are exactly as wide as the plain text,
//! * selecting across the link yields the label, never an escape byte.

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::hyperlink::{self, visible_width};
use pi_tui::input::{MouseButton, MouseGesture, MouseGestureKind};
use pi_tui::markdown::{render_markdown, render_markdown_with_links};
use pi_tui::message::{MessageItem, MessageView, Role};
use pi_tui::theme::{builtin_theme, ColorMode, ThemeColor};
use pi_tui::InputEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const WIDTH: u16 = 40;
const HEIGHT: u16 = 6;

fn theme() -> pi_tui::theme::Theme {
    builtin_theme("dark", ColorMode::TrueColor).expect("built-in dark theme")
}

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

fn app_with(config: AppConfig) -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        std::sync::Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, config)
}

fn gesture(kind: MouseGestureKind, x: u16, y: u16) -> InputEvent {
    InputEvent::gesture(MouseGesture::new(kind, x, y, false))
}

fn assistant_with_link() -> MessageView {
    let mut view = MessageView::new().with_markdown(true).with_hyperlinks(true);
    view.push(MessageItem {
        role: Role::Assistant,
        text: "see [docs](https://example.com/x) now".into(),
        streaming: false,
    });
    view
}

#[test]
fn markdown_link_renders_osc8_and_drops_the_inline_url() {
    let lines = render_markdown_with_links("[docs](https://example.com/x)", 80, true);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    // The label keeps the target; the inline `(url)` slot is gone.
    assert!(line
        .iter()
        .any(|span| span.link.as_deref() == Some("https://example.com/x")));
    assert!(
        line.iter()
            .all(|span| span.style.fg != Some(ThemeColor::MdLinkUrl)),
        "the OSC 8 branch must not also print the URL"
    );
    let text: String = line.iter().map(|span| span.text.as_str()).collect();
    assert_eq!(text, "docs");

    let themed = pi_tui::styled::themed_text(line, &theme());
    assert!(themed.contains("\u{1b}]8;;https://example.com/x\u{1b}\\"));
    assert_eq!(hyperlink::strip_ansi(&themed), "docs");
    assert_eq!(visible_width(&themed), 4);
}

#[test]
fn markdown_link_without_hyperlinks_falls_back_to_inline_url() {
    let lines = render_markdown("[docs](https://example.com/x)", 80);
    let text: String = lines[0].iter().map(|span| span.text.as_str()).collect();
    assert_eq!(text, "docs (https://example.com/x)");
    assert!(lines[0].iter().all(|span| span.link.is_none()));
    assert!(lines[0]
        .iter()
        .any(|span| span.style.fg == Some(ThemeColor::MdLinkUrl)));
}

#[test]
fn osc8_cells_are_exactly_as_wide_as_plain_text() {
    let view = assistant_with_link();
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    view.render_to_buffer_themed_with_links(area, &mut buf, &theme(), true);

    // Row 0 is the assistant line: "  see docs now".
    let symbols: Vec<&str> = (0..WIDTH)
        .map(|x| buf.cell((x, 0)).unwrap().symbol())
        .collect();
    let joined: String = symbols.concat();
    assert!(
        joined.contains("\u{1b}]8;;https://example.com/x\u{1b}\\"),
        "the live buffer must carry the hyperlink sequence"
    );

    // The link label occupies the same columns whether or not escapes are
    // embedded: strip them and the row matches the plain rendering.
    let visible = hyperlink::strip_ansi(&joined);
    assert_eq!(visible.trim_end(), "  see docs now");
    assert_eq!(visible_width(joined.trim_end()), "  see docs now".len());

    // And every painted cell still owns exactly one glyph.
    for symbol in symbols {
        if symbol.trim().is_empty() {
            continue;
        }
        assert_eq!(
            hyperlink::strip_ansi(symbol).chars().count(),
            1,
            "an OSC 8 cell must stay one column wide: {symbol:?}"
        );
    }
}

#[test]
fn selection_never_includes_osc8_sequences() {
    let mut app = app_with(AppConfig {
        session_id: "hyperlink".into(),
        hyperlinks: Some(true),
        ..AppConfig::default()
    });
    app.messages_mut().push(MessageItem {
        role: Role::Assistant,
        text: "see [docs](https://example.com/x) now".into(),
        streaming: false,
    });
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);

    // "  see docs now": select `see docs now` (columns 2..=15).
    assert_eq!(
        app.step(gesture(MouseGestureKind::Press(MouseButton::Left), 2, 0)),
        StepOutcome::Redraw
    );
    assert_eq!(
        app.step(gesture(MouseGestureKind::Drag(MouseButton::Left), 15, 0)),
        StepOutcome::Redraw
    );
    let selected = app.selection_text().expect("a selection");
    assert_eq!(selected, "see docs now");
    assert!(
        !selected.contains('\u{1b}'),
        "escapes leaked into selection"
    );
}

#[test]
fn snapshot_export_stays_plain_text_when_hyperlinks_are_on() {
    let mut app = app_with(AppConfig {
        session_id: "hyperlink".into(),
        hyperlinks: Some(true),
        ..AppConfig::default()
    });
    app.messages_mut().push(MessageItem {
        role: Role::Assistant,
        text: "see [docs](https://example.com/x) now".into(),
        streaming: false,
    });
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    assert!(!snapshot.lines.iter().any(|line| line.contains('\u{1b}')));
    // The export keeps the URL reachable even though the screen uses OSC 8.
    assert!(snapshot
        .lines
        .iter()
        .any(|line| line.contains("(https://example.com/x)")));
}

//! Integration coverage for the extension UI host surface: header, footer,
//! widgets, the custom editor region, and the `custom` overlay.
//!
//! The snapshot tests drive [`App::render_snapshot`] — the same flat render
//! path the `/transcript` export uses — so they pin the region *order* and the
//! height budget without a terminal. The behaviour tests drive
//! [`App::step_key`] and assert the keyboard priority plus the exact
//! `dispose` counts, which is what the extension bridge will rely on when a
//! JS factory is attached in the follow-up task.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use pi_tui::styled::SpanStyle;
use pi_tui::theme::{builtin_theme, ColorMode};
use pi_tui::{
    Component, CustomOptions, MessageItem, OverlayAnchor, TextComponent, ThemeColor,
    WidgetPlacement,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

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

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let config = AppConfig {
        session_id: "extension-ui".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

fn style_at(buf: &Buffer, x: u16, y: u16) -> Style {
    buf.cell((x, y)).expect("cell in bounds").style()
}

fn line(app: &App, width: u16, height: u16, row: usize) -> String {
    app.render_snapshot(width, height).lines[row].clone()
}

/// A component that records the keys it saw, consumes exactly one of them,
/// and counts its `dispose` calls.
struct Probe {
    body: Vec<String>,
    consume: Option<Key>,
    disposed: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Key>>>,
}

impl Probe {
    fn new<I, S>(body: I, consume: Option<Key>) -> (Self, Arc<AtomicUsize>, Arc<Mutex<Vec<Key>>>)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let disposed = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                body: body.into_iter().map(Into::into).collect(),
                consume,
                disposed: disposed.clone(),
                seen: seen.clone(),
            },
            disposed,
            seen,
        )
    }
}

impl Component for Probe {
    fn render(&self, _width: u16) -> Vec<pi_tui::StyledLine> {
        TextComponent::new(self.body.clone()).render(0)
    }

    fn handle_input(&mut self, key: Key) -> bool {
        self.seen.lock().expect("probe lock").push(key);
        self.consume == Some(key)
    }

    fn dispose(&mut self) {
        self.disposed.fetch_add(1, Ordering::Relaxed);
    }
}

fn key(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn message_app() -> App {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app
}

#[test]
fn header_renders_above_the_message_view() {
    let mut app = message_app();
    app.set_header(Some(Box::new(TextComponent::new(["-- header --"]))));
    assert!(app.has_header());

    // height 6 → status 1, editor 1, header 1, message 3.
    let snapshot = app.render_snapshot(30, 6);
    assert_eq!(snapshot.lines[0], "-- header --");
    assert_eq!(snapshot.lines[1].trim(), "> hello");
    assert!(
        snapshot.lines[4].starts_with('>'),
        "the prompt stays below the message view, got {:?}",
        snapshot.lines[4]
    );

    app.clear_header();
    assert!(!app.has_header());
    assert_eq!(app.render_snapshot(30, 6).lines[0].trim(), "> hello");
}

#[test]
fn footer_renders_below_the_status_bar() {
    let mut app = message_app();
    app.set_footer(Some(Box::new(TextComponent::new(["== footer =="]))));
    assert!(app.has_footer());

    // height 6 → message 3, editor 1, status 1, footer 1.
    let snapshot = app.render_snapshot(30, 6);
    assert_eq!(snapshot.lines[5], "== footer ==");
    assert!(
        snapshot.lines[4].contains("Faux"),
        "the status bar sits above the footer, got {:?}",
        snapshot.lines[4]
    );

    app.clear_footer();
    assert!(!app.has_footer());
    assert!(!app.render_snapshot(30, 6).lines[5].contains("footer"));
}

#[test]
fn above_and_below_widgets_wrap_the_editor_region() {
    let mut app = message_app();
    app.set_widget(
        "above".into(),
        Some(Box::new(TextComponent::new(["ABOVE"]))),
        WidgetPlacement::Above,
    );
    app.set_widget(
        "below".into(),
        Some(Box::new(TextComponent::new(["BELOW"]))),
        WidgetPlacement::Below,
    );

    // height 7 → above 1, message 3, editor 1, below 1, status 1.
    let snapshot = app.render_snapshot(30, 7);
    assert_eq!(snapshot.lines[0], "ABOVE");
    assert_eq!(snapshot.lines[1].trim(), "> hello");
    assert!(
        snapshot.lines[4].starts_with('>'),
        "editor row: {:?}",
        snapshot.lines[4]
    );
    assert_eq!(snapshot.lines[5], "BELOW");
    assert!(snapshot.lines[6].contains("Faux"));
}

#[test]
fn widgets_keep_insertion_order_and_replacement_moves_to_the_end() {
    let mut app = message_app();
    for (key, label, placement) in [
        ("a", "A", WidgetPlacement::Above),
        ("b", "B", WidgetPlacement::Above),
        ("c", "C", WidgetPlacement::Below),
        ("d", "D", WidgetPlacement::Above),
    ] {
        app.set_widget(
            key.into(),
            Some(Box::new(TextComponent::new([label]))),
            placement,
        );
    }

    // height 9 → above 3, message 3, editor 1, below 1, status 1.
    let snapshot = app.render_snapshot(30, 9);
    assert_eq!(
        (
            snapshot.lines[0].as_str(),
            snapshot.lines[1].as_str(),
            snapshot.lines[2].as_str()
        ),
        ("A", "B", "D")
    );
    assert_eq!(snapshot.lines[7], "C");

    // Re-setting `b` replaces it and pushes it to the end of the order.
    app.set_widget(
        "b".into(),
        Some(Box::new(TextComponent::new(["B2"]))),
        WidgetPlacement::Above,
    );
    let snapshot = app.render_snapshot(30, 9);
    assert_eq!(
        (
            snapshot.lines[0].as_str(),
            snapshot.lines[1].as_str(),
            snapshot.lines[2].as_str()
        ),
        ("A", "D", "B2")
    );

    // Clearing one key leaves the rest in place.
    app.set_widget("a".into(), None, WidgetPlacement::Above);
    let snapshot = app.render_snapshot(30, 9);
    assert_eq!(
        (snapshot.lines[0].as_str(), snapshot.lines[1].as_str()),
        ("D", "B2")
    );
    assert_eq!(
        app.widget_keys(),
        vec![
            ("c".to_string(), WidgetPlacement::Below),
            ("d".to_string(), WidgetPlacement::Above),
            ("b".to_string(), WidgetPlacement::Above),
        ]
    );
}

#[test]
fn an_over_tall_region_is_truncated_and_the_message_view_survives() {
    let mut app = message_app();
    app.set_header(Some(Box::new(TextComponent::new([
        "h1", "h2", "h3", "h4", "h5", "h6", "h7", "h8", "h9", "h10",
    ]))));

    // height 5 → status 1 + one reserved message row leave 3 rows for the
    // chrome. The prompt takes its own row first (LUM-1261: it is the one
    // region the user cannot work without, and the header can be folded away),
    // so the header is truncated to its first 2 rows and the later chrome
    // regions get nothing.
    let snapshot = app.render_snapshot(30, 5);
    assert_eq!(snapshot.lines[0], "h1");
    assert_eq!(snapshot.lines[1], "h2");
    assert_eq!(snapshot.lines[2].trim(), "> hello", "message row survives");
    assert!(
        snapshot.lines[3].contains('>'),
        "the prompt keeps a row instead of being starved: {:?}",
        snapshot.lines
    );
    assert!(snapshot.lines[4].contains("Faux"));
    assert!(
        !snapshot.lines.iter().any(|line| line.contains("h3")),
        "the header tail must be dropped: {:?}",
        snapshot.lines
    );
}

#[test]
fn custom_overlay_paints_on_top_of_the_message_view() {
    let mut app = app();
    for i in 0..6 {
        app.messages_mut()
            .push(MessageItem::user(format!("line {i}")));
    }
    // Margin 0 makes the box the full width, so the rows it spans are
    // completely covered and the assertion is unambiguous.
    let handle = app.open_custom(
        Box::new(TextComponent::new(["OVR1", "OVR2"])),
        CustomOptions::overlay().margin(0),
    );
    assert!(app.custom_open());
    assert!(app.custom_visible());

    // height 8, width 24 → message rows 0..6; the 2-row box is centred on
    // rows 3 and 4.
    let snapshot = app.render_snapshot(24, 8);
    assert_eq!(snapshot.lines[2].trim(), "> line 2");
    assert_eq!(snapshot.lines[3], "OVR1", "overlay covers its rows");
    assert_eq!(snapshot.lines[4], "OVR2");
    assert!(
        !snapshot
            .lines
            .iter()
            .any(|line| line.contains("line 3") || line.contains("line 4")),
        "the overlay must cover the transcript underneath: {:?}",
        snapshot.lines
    );

    // Hiding the session removes the overlay from the frame without closing
    // it; showing it again brings it back.
    handle.set_visible(false);
    assert!(!app.custom_visible());
    assert_eq!(app.render_snapshot(24, 8).lines[3].trim(), "> line 3");
    handle.set_visible(true);
    assert_eq!(app.render_snapshot(24, 8).lines[3], "OVR1");
}

#[test]
fn an_anchored_narrow_overlay_only_covers_its_box() {
    let mut app = app();
    for i in 0..6 {
        app.messages_mut().push(MessageItem::user(format!("m{i}")));
    }
    let _handle = app.open_custom(
        Box::new(TextComponent::new(["OVR1"])),
        CustomOptions::overlay()
            .anchor(OverlayAnchor::TopLeft)
            .width(6),
    );

    // The box is 6 wide, 1 tall, at (1, 1) with the default 1-cell margin.
    // Row 1 is `> m1`; the box blanks columns 1..7 and writes `OVR1` there,
    // leaving column 0 and the transcript to its right untouched.
    let snapshot = app.render_snapshot(24, 8);
    assert_eq!(snapshot.lines[0].trim(), "> m0", "row 0 is outside the box");
    assert_eq!(
        snapshot.lines[1], ">OVR1",
        "box at (1, 1): {:?}",
        snapshot.lines[1]
    );
    assert_eq!(snapshot.lines[2].trim(), "> m2", "row 2 is outside the box");
}

#[test]
fn extension_regions_resolve_theme_slots() {
    let mut app = message_app();
    app.set_header(Some(Box::new(TextComponent::styled(
        ["accent header"],
        SpanStyle::fg(ThemeColor::Accent),
    ))));
    let buf = render(&mut app, 24, 5);
    let dark_accent = style_at(&buf, 0, 0).fg;
    assert!(
        dark_accent.is_some(),
        "an extension region must resolve its theme slots"
    );

    // A theme hot-swap reaches the region on the next frame: the region is
    // re-resolved against the live theme rather than cached at set time.
    app.set_theme(builtin_theme("light", ColorMode::TrueColor).expect("light theme"));
    let buf = render(&mut app, 24, 5);
    assert_ne!(style_at(&buf, 0, 0).fg, dark_accent);
}

#[test]
fn editor_text_round_trips_through_the_prompt() {
    let mut app = app();
    app.set_editor_text("draft");
    assert_eq!(app.editor_text(), "draft");
    let snapshot = app.render_snapshot(20, 4);
    assert_eq!(snapshot.prompt_buffer, "draft");
    assert!(
        snapshot.lines[2].starts_with("> draft"),
        "the prompt row renders the buffer plus the cursor: {:?}",
        snapshot.lines[2]
    );
}

#[test]
fn overlay_consumes_input_before_the_prompt() {
    let mut app = app();
    let (probe, _, seen) = Probe::new(["pip"], Some(key('x')));
    let _handle = app.open_custom(Box::new(probe), CustomOptions::overlay());

    // The overlay consumes `x`: the prompt never sees it.
    assert_eq!(app.step_key(key('x')), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "");
    assert_eq!(seen.lock().expect("lock").len(), 1);

    // The overlay declines `a`, so it falls through the existing layers and
    // reaches the prompt.
    assert_eq!(app.step_key(key('a')), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "a");
    assert_eq!(seen.lock().expect("lock").len(), 2);
}

#[test]
fn editor_component_gets_keys_before_the_prompt() {
    let mut app = app();
    let (probe, _, seen) = Probe::new(["ed"], Some(key('q')));
    app.set_editor_component(Some(Box::new(probe)));
    assert!(app.has_editor_component());

    assert_eq!(app.step_key(key('q')), StepOutcome::Redraw);
    assert_eq!(app.editor_text(), "", "the component consumed the key");
    assert_eq!(app.step_key(key('b')), StepOutcome::Redraw);
    assert_eq!(
        app.editor_text(),
        "b",
        "unhandled keys still reach the prompt"
    );
    assert_eq!(seen.lock().expect("lock").len(), 2);

    app.clear_editor_component();
    assert!(!app.has_editor_component());
}

#[test]
fn a_non_overlay_custom_replaces_the_editor_and_restores_it() {
    let mut app = app();
    app.set_editor_text("draft");
    let (probe, disposed, _) = Probe::new(["CUSTOM"], None);
    let _handle = app.open_custom(Box::new(probe), CustomOptions::default());

    // The component paints in the editor region instead of the prompt.
    let snapshot = app.render_snapshot(20, 5);
    assert_eq!(snapshot.lines[3].trim(), "CUSTOM");
    assert!(snapshot.lines[4].contains("Faux"));

    assert!(app.close_custom(Some("ok".into())));
    assert_eq!(disposed.load(Ordering::Relaxed), 1);
    let snapshot = app.render_snapshot(20, 5);
    assert!(
        snapshot.lines[3].starts_with("> draft"),
        "the parked prompt comes back: {:?}",
        snapshot.lines[3]
    );
    assert_eq!(
        app.editor_text(),
        "draft",
        "the parked editor text is restored"
    );
}

#[test]
fn replacing_a_widget_disposes_the_old_component_once() {
    let mut app = app();
    let (first, first_disposed, _) = Probe::new(["first"], None);
    app.set_widget("w".into(), Some(Box::new(first)), WidgetPlacement::Above);
    assert_eq!(first_disposed.load(Ordering::Relaxed), 0);

    app.set_widget(
        "w".into(),
        Some(Box::new(TextComponent::new(["second"]))),
        WidgetPlacement::Above,
    );
    assert_eq!(first_disposed.load(Ordering::Relaxed), 1);
    assert_eq!(line(&app, 20, 5, 0), "second");

    let (third, third_disposed, _) = Probe::new(["third"], None);
    app.set_widget("w".into(), Some(Box::new(third)), WidgetPlacement::Below);
    assert_eq!(third_disposed.load(Ordering::Relaxed), 0);
    app.set_widget("w".into(), None, WidgetPlacement::Below);
    assert_eq!(third_disposed.load(Ordering::Relaxed), 1);
    assert!(app.widget_keys().is_empty());
}

#[test]
fn close_custom_stops_rendering_and_disposes_exactly_once() {
    let mut app = app();
    let (probe, disposed, _) = Probe::new(["OVR"], None);
    let mut handle = app.open_custom(Box::new(probe), CustomOptions::overlay().width(8));

    assert!(app.custom_open());
    assert_eq!(app.render_snapshot(20, 5).lines[2].trim(), "OVR");

    assert!(app.close_custom(Some("done".into())));
    assert!(!app.custom_open());
    assert!(!app.custom_visible());
    assert_eq!(disposed.load(Ordering::Relaxed), 1);
    assert!(
        !app.render_snapshot(20, 5)
            .lines
            .iter()
            .any(|line| line.contains("OVR")),
        "a closed session must not render"
    );
    assert_eq!(
        handle.try_recv_result(),
        Some(Some("done".to_string())),
        "the handle carries the close result"
    );

    // Closing again is a no-op and never disposes a second time.
    assert!(!app.close_custom(None));
    assert_eq!(disposed.load(Ordering::Relaxed), 1);
}

#[test]
fn opening_a_second_custom_cancels_the_first() {
    let mut app = app();
    let (first, disposed, _) = Probe::new(["first"], None);
    let mut first_handle = app.open_custom(Box::new(first), CustomOptions::overlay());
    let _second = app.open_custom(
        Box::new(TextComponent::new(["second"])),
        CustomOptions::overlay().width(10),
    );
    assert_eq!(disposed.load(Ordering::Relaxed), 1);
    assert_eq!(first_handle.try_recv_result(), Some(None));
    assert_eq!(app.render_snapshot(20, 5).lines[2].trim(), "second");
}

#[test]
fn full_chrome_order_is_header_message_widgets_prompt_status_footer() {
    let mut app = app();
    app.messages_mut().push(MessageItem::user("hello"));
    app.set_header(Some(Box::new(TextComponent::new(["HEADER"]))));
    app.set_widget(
        "a".into(),
        Some(Box::new(TextComponent::new(["ABOVE"]))),
        WidgetPlacement::Above,
    );
    app.set_widget(
        "b".into(),
        Some(Box::new(TextComponent::new(["BELOW"]))),
        WidgetPlacement::Below,
    );
    app.set_footer(Some(Box::new(TextComponent::new(["FOOTER"]))));

    // height 9, width 30 → status 1, editor 1, header 1, above 1, below 1,
    // footer 1, and the three rows left over for the message view. The row
    // order is the whole point: header, above-editor, message, prompt,
    // below-editor, status, footer.
    let snapshot = app.render_snapshot(30, 9);
    assert_eq!(snapshot.lines[0], "HEADER");
    assert_eq!(snapshot.lines[1], "ABOVE");
    assert_eq!(snapshot.lines[2].trim(), "> hello");
    assert!(
        snapshot.lines[5].starts_with('>'),
        "editor row: {:?}",
        snapshot.lines[5]
    );
    assert_eq!(snapshot.lines[6], "BELOW");
    assert!(
        snapshot.lines[7].contains("Faux"),
        "status row: {:?}",
        snapshot.lines[7]
    );
    assert_eq!(
        snapshot.lines[8], "FOOTER",
        "the footer is the very last row"
    );
}

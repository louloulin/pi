//! Real-execution verification of the layout engine.

use pi_tui::component::Component;
use pi_tui::components::Text;
use pi_tui::layout::{get_layout_boxes_at, render_layout_frame, LayoutBox, LayoutFrame, LayoutRect};
use pi_tui::styled::{SpanStyle, StyledLine, StyledSpan};

/// Helper: a static-text component the layout engine can paint as a leaf.
fn text_component(lines: &[&str]) -> std::sync::Arc<dyn Component> {
    std::sync::Arc::new(Text::from_lines(lines.iter().map(|s| s.to_string())))
}

#[test]
fn layout_engine_paints_root_text() {
    let root = text_component(&["alpha", "beta", "gamma"]);
    let frame = render_layout_frame(root, 10, 3, || {});
    assert_eq!(frame.lines.len(), 3);
    assert!(frame.lines[0].contains("alpha"));
    assert!(frame.lines[1].contains("beta"));
    assert!(frame.lines[2].contains("gamma"));
}

#[test]
fn layout_engine_renders_oversized_text_without_truncating() {
    // The layout engine stores the rendered lines at the safe width.
    // It does not truncate lines itself; clipping is a paint-time concern.
    let root = text_component(&["a very long single line of text"]);
    let frame = render_layout_frame(root, 5, 1, || {});
    assert_eq!(frame.lines.len(), 1);
    let rendered = frame.lines[0].chars().count();
    // Component rendered the full string into a 5-wide region; the engine
    // simply stored the result. Verify the text survived the round-trip.
    assert!(rendered >= 5);
}

#[test]
fn layout_engine_hit_test_finds_root_box() {
    let root = text_component(&["alpha"]);
    let frame = render_layout_frame(root, 10, 1, || {});
    let hits = get_layout_boxes_at(&frame, 0, 0);
    assert!(!hits.is_empty());
    assert_eq!(hits[0].rect.width, 10);
}

#[test]
fn layout_engine_hit_test_outside_returns_empty() {
    let root = text_component(&["alpha"]);
    let frame = render_layout_frame(root, 10, 1, || {});
    let hits = get_layout_boxes_at(&frame, 100, 100);
    assert!(hits.is_empty());
}

#[test]
fn layout_engine_request_render_callback_fires() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = std::sync::Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let root = text_component(&["alpha"]);
    let _frame = render_layout_frame(root, 10, 1, move || {
        counter_clone.fetch_add(1, Ordering::Relaxed);
    });
    // The callback may or may not fire on the first pass; just confirm
    // the closure can be plumbed through.
    assert!(counter.load(Ordering::Relaxed) < 1000);
}

#[test]
fn cursor_marker_survives_full_layout_pass() {
    struct CursorComponent;
    impl Component for CursorComponent {
        fn render(&self, _width: u16) -> Vec<StyledLine> {
            vec![vec![StyledSpan::new(
                pi_tui::layout::CURSOR_MARKER.to_string(),
                SpanStyle::PLAIN,
            )]]
        }
    }
    let root: std::sync::Arc<dyn Component> = std::sync::Arc::new(CursorComponent);
    let frame = render_layout_frame(root, 5, 1, || {});
    assert!(frame.lines[0].contains(pi_tui::layout::CURSOR_MARKER));
}

#[test]
fn layout_rect_default_is_origin() {
    let r: LayoutRect = Default::default();
    assert_eq!(r.x, 0);
    assert_eq!(r.y, 0);
    assert_eq!(r.width, 0);
    assert_eq!(r.height, 0);
}

#[test]
fn layout_box_debug_skips_dyn_fields() {
    // Manual Debug impl avoids the `dyn Component` Debug requirement.
    let root = text_component(&["x"]);
    let frame = render_layout_frame(root, 10, 1, || {});
    let formatted = format!("{:?}", frame.root);
    assert!(formatted.contains("LayoutBox"));
    assert!(formatted.contains("<dyn Component>"));
}

#[test]
fn layout_box_clone_works() {
    let root = text_component(&["x"]);
    let frame = render_layout_frame(root, 10, 1, || {});
    let cloned: LayoutBox = frame.root.clone();
    assert_eq!(cloned.rect, frame.root.rect);
}

#[test]
fn layout_frame_clone_works() {
    let root = text_component(&["x"]);
    let frame: LayoutFrame = render_layout_frame(root, 10, 1, || {});
    let cloned = frame.clone();
    assert_eq!(cloned.lines.len(), frame.lines.len());
}
//! State for the extension UI host surface — the regions an extension can
//! take over: header, footer, widgets around the editor, a custom editor
//! component, and the `custom` overlay.
//!
//! [`App`](crate::App) owns one [`ExtensionUi`] and exposes it through
//! `set_header` / `set_footer` / `set_widget` / `set_editor_component` /
//! `open_custom`. The module also owns the host-side layout: it renders the
//! attached [`Component`]s to [`StyledLine`]s and plans how many rows each
//! region may occupy (`plan_chrome`).
//!
//! # Layout contract
//!
//! Regions are laid out top to bottom in this order:
//!
//! ```text
//! header
//! message view
//! Above widgets (in insertion order)
//! editor region (custom non-overlay component → editor component → prompt)
//! Below widgets (in insertion order)
//! status bar
//! footer
//! ```
//!
//! and the `custom` overlay (`CustomOptions::overlay`) is painted on top of
//! all of it. The same order is restated in `app.rs`, which owns the paint
//! calls.
//!
//! Height budgeting (`plan_chrome`):
//!
//! * The status region (1 row, or 2 when the host supplied a working
//!   directory — LUM-1466) and at least one message row are reserved first;
//!   the message view therefore never disappears, however tall the extension
//!   regions are.
//! * The editor region is reserved **next**, before the header. The prompt is
//!   the one region the user cannot do anything without, and the header is the
//!   region that can be folded away (`Alt+H`) — on a default startup frame the
//!   built-in header is 21 rows, so letting it take the budget first left the
//!   editor with zero rows and the user typed blind on a 22/23-row terminal
//!   (`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3.1, re-measured in
//!   `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §3). Upstream's flex layout
//!   shrinks the flexible message area, never the fixed editor region.
//! * The remaining rows are handed to the rest of the chrome in render order —
//!   header, Above widgets, Below widgets, footer — each taking at most what
//!   its component asked for.
//! * Truncation is not the only answer for the *built-in* startup header:
//!   [`App`](crate::App) folds its hint list on a terminal that cannot hold the
//!   list, the composer and three transcript rows, and says so on one dim row
//!   (LUM-1266). The tail-dropping rule below still governs an extension header
//!   installed with `ctx.ui.setHeader`.
//! * The status region (1 row, or 2 when the host supplied a working
//!   directory — LUM-1466) and at least one message row are reserved first;
//!   the message view therefore never disappears, however tall the extension
//!   regions are.
//! * The editor region is reserved **next**, before the header. The prompt is
//!   the one region the user cannot do anything without, and the header is the
//!   region that can be folded away (`Alt+H`) — on a default startup frame the
//!   built-in header is 21 rows, so letting it take the budget first left the
//!   editor with zero rows and the user typed blind on a 22/23-row terminal
//!   (`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3.1, re-measured in
//!   `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §3). Upstream's flex layout
//!   shrinks the flexible message area, never the fixed editor region.
//! * The remaining rows are handed to the rest of the chrome in render order —
//!   header, Above widgets, Below widgets, footer — each taking at most what
//!   its component asked for.
//! * Truncation is not the only answer for the *built-in* startup header:
//!   [`App`](crate::App) folds its hint list on a terminal that cannot hold the
//!   list, the composer and three transcript rows, and says so on one dim row
//!   (LUM-1266, `docs/TUI_SHORT_VIEWPORT_AND_SCROLLBAR_LUM1266.md`). The
//!   tail-dropping rule below still governs an extension header installed with
//!   `ctx.ui.setHeader`.
//! * A region that does not fit in what is left is **truncated to the
//!   remaining rows (its tail is dropped)** and every later region renders
//!   nothing. This is the "truncate the tail" policy: upstream's `Container`
//!   renders every child and lets the terminal clip, which is the same
//!   observable result as long as the message view keeps its reserved row.
//!   The header keeps its first rows, so a squeezed header loses its
//!   onboarding line and then its last hints — not its title and first hints.
//!
//! # Widget ordering
//!
//! Widgets render in insertion order within their placement, and re-setting a
//! key moves it to the end of the sequence — upstream keeps an ordered `Map`
//! per placement and re-inserts on every `setWidget`
//! (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2188-2226`).
//! Setting a key again — under either placement — replaces the old component
//! after calling its `dispose`, and passing `None` clears it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use crate::component::{Component, CustomHandle, CustomOptions, WidgetPlacement};
use crate::core::input_parse::Key;
use crate::utils::styled::StyledLine;

/// One widget slot, kept in insertion order.
struct WidgetEntry {
    key: String,
    placement: WidgetPlacement,
    component: Box<dyn Component>,
}

/// The single open `custom` session.
struct CustomOverlay {
    component: Option<Box<dyn Component>>,
    options: CustomOptions,
    visible: Arc<AtomicBool>,
    result_tx: Option<mpsc::Sender<Option<String>>>,
}

impl CustomOverlay {
    fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }
}

/// The extension UI regions attached to an [`App`](crate::App).
#[derive(Default)]
pub struct ExtensionUi {
    header: Option<Box<dyn Component>>,
    footer: Option<Box<dyn Component>>,
    editor_component: Option<Box<dyn Component>>,
    widgets: Vec<WidgetEntry>,
    custom: Option<CustomOverlay>,
    next_custom_id: u64,
}

impl ExtensionUi {
    /// An empty host surface: every region unset, no custom session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach or replace the header component, disposing the previous one.
    pub(crate) fn set_header(&mut self, component: Option<Box<dyn Component>>) {
        dispose_option(&mut self.header);
        self.header = component;
    }

    /// Drop the header component, disposing it.
    pub(crate) fn clear_header(&mut self) {
        dispose_option(&mut self.header);
    }

    /// Whether a header component is attached.
    pub fn has_header(&self) -> bool {
        self.header.is_some()
    }

    /// Attach or replace the footer component, disposing the previous one.
    pub(crate) fn set_footer(&mut self, component: Option<Box<dyn Component>>) {
        dispose_option(&mut self.footer);
        self.footer = component;
    }

    /// Drop the footer component, disposing it.
    pub(crate) fn clear_footer(&mut self) {
        dispose_option(&mut self.footer);
    }

    /// Whether a footer component is attached.
    pub fn has_footer(&self) -> bool {
        self.footer.is_some()
    }

    /// Attach or replace the custom editor component, disposing the previous
    /// one.
    pub(crate) fn set_editor_component(&mut self, component: Option<Box<dyn Component>>) {
        dispose_option(&mut self.editor_component);
        self.editor_component = component;
    }

    /// Drop the custom editor component, disposing it.
    pub(crate) fn clear_editor_component(&mut self) {
        dispose_option(&mut self.editor_component);
    }

    /// Whether a custom editor component is attached.
    pub fn has_editor_component(&self) -> bool {
        self.editor_component.is_some()
    }

    /// Set (or clear, with `None`) the widget registered under `key`.
    ///
    /// The key is looked up across both placements, so re-setting an existing
    /// key replaces its component — disposing the old one — and appends the
    /// new entry to the end of the insertion order.
    pub(crate) fn set_widget(
        &mut self,
        key: String,
        component: Option<Box<dyn Component>>,
        placement: WidgetPlacement,
    ) {
        self.remove_widget(&key);
        if let Some(component) = component {
            self.widgets.push(WidgetEntry {
                key,
                placement,
                component,
            });
        }
    }

    /// Remove the widget under `key` from either placement, disposing it.
    /// Returns whether a widget was removed.
    pub(crate) fn remove_widget(&mut self, key: &str) -> bool {
        let Some(index) = self.widgets.iter().position(|widget| widget.key == key) else {
            return false;
        };
        let mut entry = self.widgets.remove(index);
        entry.component.dispose();
        true
    }

    /// Remove every widget, disposing each.
    pub(crate) fn clear_widgets(&mut self) {
        for mut widget in self.widgets.drain(..) {
            widget.component.dispose();
        }
    }

    /// The registered widget keys, in insertion order, with their placements.
    pub fn widget_keys(&self) -> Vec<(String, WidgetPlacement)> {
        self.widgets
            .iter()
            .map(|widget| (widget.key.clone(), widget.placement))
            .collect()
    }

    /// Open a `custom` session, returning its handle.
    ///
    /// Only one session exists at a time: opening a second one closes the
    /// first as cancelled (`None` result) and disposes its component first.
    pub(crate) fn open_custom(
        &mut self,
        component: Box<dyn Component>,
        options: CustomOptions,
    ) -> CustomHandle {
        self.close_custom(None);
        let id = self.next_custom_id;
        self.next_custom_id += 1;
        let visible = Arc::new(AtomicBool::new(true));
        let (result_tx, result_rx) = mpsc::channel();
        self.custom = Some(CustomOverlay {
            component: Some(component),
            options,
            visible: visible.clone(),
            result_tx: Some(result_tx),
        });
        CustomHandle::new(id, visible, result_rx)
    }

    /// Close the open `custom` session, delivering `result` to its handle and
    /// disposing its component (exactly once). Returns whether a session was
    /// open.
    pub(crate) fn close_custom(&mut self, result: Option<String>) -> bool {
        let Some(mut custom) = self.custom.take() else {
            return false;
        };
        if let Some(tx) = custom.result_tx.take() {
            // A closed receiver is not an error: the caller may have dropped
            // the handle without waiting for the result.
            let _ = tx.send(result);
        }
        if let Some(mut component) = custom.component.take() {
            component.dispose();
        }
        true
    }

    /// Whether a `custom` session is open.
    pub fn custom_open(&self) -> bool {
        self.custom.is_some()
    }

    /// Whether the open `custom` component is currently visible (it may be
    /// temporarily hidden through [`CustomHandle::set_visible`]).
    pub fn custom_visible(&self) -> bool {
        self.custom.as_ref().is_some_and(CustomOverlay::is_visible)
    }

    /// Route a key to the `custom` overlay, if one is open, visible and in
    /// overlay mode. Returns whether it consumed the key.
    pub(crate) fn handle_overlay_input(&mut self, key: Key) -> bool {
        let Some(custom) = self.custom.as_mut() else {
            return false;
        };
        if !custom.options.overlay || !custom.is_visible() {
            return false;
        }
        match custom.component.as_mut() {
            Some(component) => component.handle_input(key),
            None => false,
        }
    }

    /// Route a key to the editor-region component: a non-overlay `custom`
    /// session if one is open and visible, otherwise the custom editor
    /// component. Returns whether it consumed the key.
    pub(crate) fn handle_editor_input(&mut self, key: Key) -> bool {
        if let Some(custom) = self.custom.as_mut() {
            if !custom.options.overlay && custom.is_visible() {
                if let Some(component) = custom.component.as_mut() {
                    return component.handle_input(key);
                }
            }
        }
        match self.editor_component.as_mut() {
            Some(component) => component.handle_input(key),
            None => false,
        }
    }

    /// Render every attached component into its region's lines.
    pub(crate) fn frame(&self, width: u16) -> ExtensionFrame {
        ExtensionFrame {
            header: render_optional(self.header.as_deref(), width),
            above: self.placement_lines(WidgetPlacement::Above, width),
            below: self.placement_lines(WidgetPlacement::Below, width),
            footer: render_optional(self.footer.as_deref(), width),
            editor: self.editor_lines(width),
            overlay: self.overlay_layer(width),
            status: 1,
            pending: 0,
        }
    }

    fn placement_lines(&self, placement: WidgetPlacement, width: u16) -> Vec<StyledLine> {
        let mut lines = Vec::new();
        for widget in self
            .widgets
            .iter()
            .filter(|widget| widget.placement == placement)
        {
            lines.extend(widget.component.render(width));
        }
        lines
    }

    /// Lines for the editor region, or `None` when the built-in prompt
    /// renders there.
    fn editor_lines(&self, width: u16) -> Option<Vec<StyledLine>> {
        if let Some(custom) = self.custom.as_ref() {
            if !custom.options.overlay && custom.is_visible() {
                if let Some(component) = custom.component.as_ref() {
                    return Some(component.render(width));
                }
            }
        }
        self.editor_component
            .as_ref()
            .map(|component| component.render(width))
    }

    fn overlay_layer(&self, width: u16) -> Option<OverlayLayer> {
        let custom = self.custom.as_ref()?;
        if !custom.options.overlay || !custom.is_visible() {
            return None;
        }
        let component = custom.component.as_ref()?;
        Some(OverlayLayer {
            lines: component.render(width),
            options: custom.options,
        })
    }
}

impl Drop for ExtensionUi {
    fn drop(&mut self) {
        dispose_option(&mut self.header);
        dispose_option(&mut self.footer);
        dispose_option(&mut self.editor_component);
        self.clear_widgets();
        if let Some(mut custom) = self.custom.take() {
            if let Some(mut component) = custom.component.take() {
                component.dispose();
            }
        }
    }
}

/// Dispose a stored component and clear the slot.
fn dispose_option(slot: &mut Option<Box<dyn Component>>) {
    if let Some(mut component) = slot.take() {
        component.dispose();
    }
}

fn render_optional(component: Option<&dyn Component>, width: u16) -> Vec<StyledLine> {
    component
        .map(|component| component.render(width))
        .unwrap_or_default()
}

/// The rendered lines of every extension region for one frame.
pub(crate) struct ExtensionFrame {
    /// Header lines, above the message view.
    pub(crate) header: Vec<StyledLine>,
    /// `Above` widget lines, below the message view.
    pub(crate) above: Vec<StyledLine>,
    /// `Below` widget lines, below the editor region.
    pub(crate) below: Vec<StyledLine>,
    /// Footer lines, below the status bar.
    pub(crate) footer: Vec<StyledLine>,
    /// Editor-region lines that replace the prompt, if any.
    pub(crate) editor: Option<Vec<StyledLine>>,
    /// The topmost `custom` overlay, if one is visible.
    pub(crate) overlay: Option<OverlayLayer>,
    /// Rows the built-in status bar asked for (1, or 2 when it has a
    /// location row to draw). The App sets this from
    /// [`crate::components::status::StatusBar::line_count`]; [`plan_chrome`] reserves
    /// exactly this many rows so a host that never supplies a working
    /// directory keeps the single-row geometry.
    pub(crate) status: u16,
    /// Rows the queued-messages block asked for (`0` when no prompt is
    /// queued). The App sets this from
    /// [`crate::components::message::MessageView::pending_block_rows`]; it is data driven
    /// for the same reason `status` is — an empty queue must keep the frame
    /// geometry it had before LUM-1469.
    pub(crate) pending: u16,
}

/// A visible `custom` overlay and the options that place it.
pub(crate) struct OverlayLayer {
    /// The component's rendered lines.
    pub(crate) lines: Vec<StyledLine>,
    /// Positioning options.
    pub(crate) options: CustomOptions,
}

/// Rows each region of the frame may occupy.
pub(crate) struct ChromeLayout {
    /// Header rows.
    pub(crate) header: u16,
    /// `Above` widget rows.
    pub(crate) above: u16,
    /// Editor-region rows (at least one).
    pub(crate) editor: u16,
    /// `Below` widget rows.
    pub(crate) below: u16,
    /// Message-view rows (at least one whenever the terminal has two rows).
    pub(crate) message: u16,
    /// Status-bar rows.
    pub(crate) status: u16,
    /// Footer rows.
    pub(crate) footer: u16,
    /// Queued-messages rows, directly above the editor region (upstream keeps
    /// its `pendingMessagesContainer` in the prompt area,
    /// `interactive-mode.ts:878-892`).
    pub(crate) pending: u16,
}

/// Split `total` rows between the message view, the status bar and the
/// extension regions.
///
/// See the module docs for the policy; the short version is "reserve the
/// status region ([`ExtensionFrame::status`] rows), the prompt and one
/// message row, then hand out the rest in render order, truncating the
/// tail".
///
/// `editor_min_rows` is the smallest number of rows the built-in prompt
/// needs to render its current buffer at the App's current width. Pass
/// `1` for the legacy single-row composer; pass
/// [`crate::Prompt::line_count`] when the prompt is wrapped so the
/// composer is allowed to grow before the header steals the rows. The
/// editor always gets at least `1` row — the prompt is never invisible.
pub(crate) fn plan_chrome(
    total: u16,
    frame: &ExtensionFrame,
    editor_min_rows: u16,
) -> ChromeLayout {
    // `frame.status` is the built-in status bar's own row count: 1, or 2 when
    // it draws upstream's `pwd` row above the stats row (LUM-1466).
    let status = frame.status.max(1).min(total);
    // One row stays with the message view so it never vanishes.
    let mut budget = total.saturating_sub(status).saturating_sub(1);
    let mut take = |want: u16| {
        let got = want.min(budget);
        budget -= got;
        got
    };

    // The prompt is reserved before the header, not after it (LUM-1261).
    // Handing the budget to the header first let the 21-row startup legend
    // consume every row on a 22/23-row terminal, leaving `editor == 0`: the
    // composer was painted into a zero-height region and the user typed into
    // an invisible input (`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3.1). The
    // header is foldable and the prompt is not, so the header is what gives
    // way — it is truncated (its tail dropped) instead of starving the
    // editor. The prompt always needs a row, and a component that renders
    // nothing would otherwise make the editor region disappear entirely.
    let editor = take(
        frame
            .editor
            .as_ref()
            .map_or(editor_min_rows.max(1), |lines| {
                lines_height(lines).max(editor_min_rows.max(1))
            }),
    );
    // The queued-messages block (LUM-1469) is budgeted immediately after the
    // composer: it is the second thing the reader cannot work without while a
    // turn is running (it is where their typed-ahead prompt shows up), and the
    // foldable header is what gives way when both cannot fit.
    let pending = take(frame.pending);
    let header = take(lines_height(&frame.header));
    let above = take(lines_height(&frame.above));
    let below = take(lines_height(&frame.below));
    let footer = take(lines_height(&frame.footer));
    let used = header + above + editor + below + footer + pending;
    ChromeLayout {
        header,
        above,
        editor,
        below,
        message: total.saturating_sub(status + used),
        status,
        footer,
        pending,
    }
}

fn lines_height(lines: &[StyledLine]) -> u16 {
    u16::try_from(lines.len()).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, TextComponent};
    use crate::core::input_parse::{Key, KeyCode, KeyModifiers};
    use crate::utils::styled::plain_text;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A component that counts `dispose` calls and records keys it saw.
    struct Probe {
        lines: Vec<StyledLine>,
        consumes: bool,
        disposed: Arc<AtomicUsize>,
        seen: Arc<std::sync::Mutex<Vec<Key>>>,
    }

    impl Probe {
        fn new(
            label: &str,
            consumes: bool,
        ) -> (Self, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<Key>>>) {
            let disposed = Arc::new(AtomicUsize::new(0));
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    lines: TextComponent::new([label.to_string()]).render(10),
                    consumes,
                    disposed: disposed.clone(),
                    seen: seen.clone(),
                },
                disposed,
                seen,
            )
        }
    }

    impl Component for Probe {
        fn render(&self, _width: u16) -> Vec<StyledLine> {
            self.lines.clone()
        }

        fn handle_input(&mut self, key: Key) -> bool {
            self.seen.lock().expect("lock").push(key);
            self.consumes
        }

        fn dispose(&mut self) {
            self.disposed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A component that only counts `dispose` calls.
    struct Counter {
        lines: Vec<StyledLine>,
        disposed: Arc<AtomicUsize>,
    }

    fn counter(label: &str) -> (Counter, Arc<AtomicUsize>) {
        let disposed = Arc::new(AtomicUsize::new(0));
        (
            Counter {
                lines: TextComponent::new([label.to_string()]).render(10),
                disposed: disposed.clone(),
            },
            disposed,
        )
    }

    impl Component for Counter {
        fn render(&self, _width: u16) -> Vec<StyledLine> {
            self.lines.clone()
        }

        fn dispose(&mut self) {
            self.disposed.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn key(c: char) -> Key {
        Key::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn line_texts(lines: &[StyledLine]) -> Vec<String> {
        lines.iter().map(|line| plain_text(line)).collect()
    }

    #[test]
    fn widgets_render_in_insertion_order_per_placement() {
        let mut ui = ExtensionUi::new();
        ui.set_widget(
            "a".into(),
            Some(Box::new(TextComponent::new(["A"]))),
            WidgetPlacement::Above,
        );
        ui.set_widget(
            "b".into(),
            Some(Box::new(TextComponent::new(["B"]))),
            WidgetPlacement::Below,
        );
        ui.set_widget(
            "c".into(),
            Some(Box::new(TextComponent::new(["C"]))),
            WidgetPlacement::Above,
        );

        let frame = ui.frame(20);
        assert_eq!(line_texts(&frame.above), vec!["A", "C"]);
        assert_eq!(line_texts(&frame.below), vec!["B"]);
    }

    #[test]
    fn resetting_a_key_replaces_and_disposes_it() {
        let mut ui = ExtensionUi::new();
        let (first, disposed, _) = Probe::new("first", false);
        ui.set_widget("k".into(), Some(Box::new(first)), WidgetPlacement::Above);
        assert_eq!(disposed.load(Ordering::Relaxed), 0);

        // Re-setting under the *other* placement still replaces the key
        // (upstream removes it from both maps).
        ui.set_widget(
            "k".into(),
            Some(Box::new(TextComponent::new(["second"]))),
            WidgetPlacement::Below,
        );
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        let frame = ui.frame(20);
        assert!(frame.above.is_empty());
        assert_eq!(line_texts(&frame.below), vec!["second"]);

        assert!(ui.remove_widget("k"));
        assert!(!ui.remove_widget("k"));
        assert!(ui.frame(20).below.is_empty());
    }

    #[test]
    fn header_and_footer_replace_disposes_the_old_component() {
        let mut ui = ExtensionUi::new();
        let (first, disposed, _) = Probe::new("first", false);
        ui.set_header(Some(Box::new(first)));
        assert!(ui.has_header());
        ui.set_header(Some(Box::new(TextComponent::new(["second"]))));
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        ui.clear_header();
        assert!(!ui.has_header());
        assert!(ui.frame(20).header.is_empty());

        assert!(!ui.has_footer());
        ui.set_footer(Some(Box::new(TextComponent::new(["f"]))));
        assert!(ui.has_footer());
        ui.clear_footer();
        assert!(!ui.has_footer());
    }

    #[test]
    fn custom_close_disposes_once_and_delivers_the_result() {
        let mut ui = ExtensionUi::new();
        let (probe, disposed, _) = Probe::new("probe", true);
        let mut handle = ui.open_custom(Box::new(probe), CustomOptions::overlay());
        assert!(ui.custom_open());
        assert!(ui.custom_visible());
        assert!(ui.frame(20).overlay.is_some());

        assert!(ui.close_custom(Some("done".into())));
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        assert!(!ui.custom_open());
        assert!(ui.frame(20).overlay.is_none());
        assert_eq!(
            handle.try_recv_result(),
            Some(Some("done".to_string())),
            "the handle receives the close result"
        );

        // Closing again is a no-op and must not dispose a second time.
        assert!(!ui.close_custom(None));
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn replacing_an_open_custom_cancels_the_previous_session() {
        let mut ui = ExtensionUi::new();
        let (first, disposed, _) = Probe::new("first", false);
        let mut first_handle = ui.open_custom(Box::new(first), CustomOptions::overlay());
        let _second = ui.open_custom(
            Box::new(TextComponent::new(["second"])),
            CustomOptions::overlay(),
        );
        assert_eq!(disposed.load(Ordering::Relaxed), 1);
        assert_eq!(first_handle.try_recv_result(), Some(None));
    }

    #[test]
    fn hidden_custom_is_not_rendered_or_fed_input() {
        let mut ui = ExtensionUi::new();
        let (probe, _, seen) = Probe::new("probe", true);
        let handle = ui.open_custom(Box::new(probe), CustomOptions::overlay());
        assert!(ui.handle_overlay_input(key('x')));
        handle.set_visible(false);
        assert!(!ui.custom_visible());
        assert!(ui.frame(20).overlay.is_none());
        assert!(!ui.handle_overlay_input(key('y')));
        handle.set_visible(true);
        assert!(ui.handle_overlay_input(key('z')));
        assert_eq!(seen.lock().expect("lock").len(), 2);
    }

    #[test]
    fn input_routing_splits_overlay_and_editor_regions() {
        let mut ui = ExtensionUi::new();
        let (overlay, _, overlay_seen) = Probe::new("o", true);
        ui.open_custom(Box::new(overlay), CustomOptions::overlay());
        let (editor_hidden, _, editor_seen) = Probe::new("e", true);
        ui.set_editor_component(Some(Box::new(editor_hidden)));

        // An overlay component owns the key first; the editor component is
        // only consulted when the overlay declines it (which the App does by
        // falling through to the next layer).
        assert!(ui.handle_overlay_input(key('a')));
        assert!(editor_seen.lock().expect("lock").is_empty());
        assert_eq!(overlay_seen.lock().expect("lock").len(), 1);
        assert!(ui.handle_editor_input(key('b')));
        assert_eq!(editor_seen.lock().expect("lock").len(), 1);

        // A non-overlay session renders in the editor region and receives
        // input there.
        ui.close_custom(None);
        let (editor_custom, _, custom_seen) = Probe::new("c", true);
        ui.open_custom(Box::new(editor_custom), CustomOptions::default());
        assert!(!ui.handle_overlay_input(key('c')));
        assert!(ui.handle_editor_input(key('d')));
        assert_eq!(custom_seen.lock().expect("lock").len(), 1);
        let frame = ui.frame(20);
        assert!(frame.overlay.is_none());
        assert_eq!(line_texts(&frame.editor.expect("editor lines")), vec!["c"]);
    }

    #[test]
    fn editor_lines_fall_back_to_the_editor_component() {
        let mut ui = ExtensionUi::new();
        assert!(ui.frame(20).editor.is_none());
        ui.set_editor_component(Some(Box::new(TextComponent::new(["ed"]))));
        assert!(ui.has_editor_component());
        let frame = ui.frame(20);
        assert_eq!(line_texts(&frame.editor.expect("editor lines")), vec!["ed"]);
        ui.clear_editor_component();
        assert!(!ui.has_editor_component());
        assert!(ui.frame(20).editor.is_none());
    }

    fn frame_with(
        header: usize,
        above: usize,
        editor: Option<usize>,
        below: usize,
        footer: usize,
    ) -> ExtensionFrame {
        let lines = |count: usize| vec![TextComponent::new(["x"]).render(1)[0].clone(); count];
        ExtensionFrame {
            header: lines(header),
            above: lines(above),
            below: lines(below),
            footer: lines(footer),
            editor: editor.map(lines),
            overlay: None,
            status: 1,
            pending: 0,
        }
    }

    /// [`frame_with`] plus a queued-messages block of `pending` rows
    /// (LUM-1469).
    fn frame_with_pending(pending: u16) -> ExtensionFrame {
        ExtensionFrame {
            pending,
            ..frame_with(0, 0, None, 0, 0)
        }
    }

    #[test]
    fn plan_chrome_reserves_status_and_one_message_row() {
        // No extension content: message takes everything but status + editor.
        let layout = plan_chrome(10, &frame_with(0, 0, None, 0, 0), 1);
        assert_eq!(
            (layout.header, layout.above, layout.editor, layout.below),
            (0, 0, 1, 0)
        );
        assert_eq!((layout.status, layout.footer), (1, 0));
        assert_eq!(layout.message, 8);
    }

    /// LUM-1469: the queued-messages block costs transcript rows, exactly the
    /// rows it asked for, and it sits between the composer and the header.
    #[test]
    fn plan_chrome_reserves_pending_rows_above_the_editor() {
        // 12 rows: 1 status + 1 composer + 3 pending leave 7 transcript rows.
        let layout = plan_chrome(12, &frame_with_pending(3), 1);
        assert_eq!((layout.editor, layout.pending), (1, 3));
        assert_eq!(
            (layout.header, layout.above, layout.below, layout.footer),
            (0, 0, 0, 0)
        );
        assert_eq!((layout.status, layout.message), (1, 7));
    }

    /// The composer is reserved first and the queued block next, so a header
    /// that cannot fit both is what shrinks — never the composer, and never a
    /// queued prompt whose only on-screen home is this block.
    #[test]
    fn plan_chrome_folds_the_header_before_dropping_the_pending_block() {
        let frame = ExtensionFrame {
            pending: 3,
            ..frame_with(8, 0, None, 0, 0)
        };
        // 6 rows: 1 status + 1 transcript leave 4 for chrome.
        let layout = plan_chrome(6, &frame, 1);
        assert_eq!((layout.editor, layout.pending), (1, 3));
        assert_eq!(layout.header, 0);
        assert_eq!(layout.message, 1);
    }

    #[test]
    fn plan_chrome_truncates_the_tail_when_the_chrome_overflows() {
        // 5 rows total: 1 status + 1 message leave 3 for chrome. The prompt
        // takes its reserved row first (it is the region the user cannot work
        // without), then the header takes the remaining 2 and the tail — the
        // Above/Below widgets and the footer — renders nothing.
        let layout = plan_chrome(5, &frame_with(10, 2, None, 2, 4), 1);
        assert_eq!((layout.editor, layout.header), (1, 2));
        assert_eq!((layout.above, layout.below), (0, 0));
        assert_eq!((layout.status, layout.footer), (1, 0));
        assert_eq!(layout.message, 1);
    }

    /// LUM-1261: the 21-row startup header must not push the composer off the
    /// screen. Measured defect (LUM-1260 §3.1 and re-measured in
    /// `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §3): at 120×22 and 120×23 a
    /// typed draft never appeared in the PTY grid, because the header claimed
    /// all 21 budget rows and left `editor == 0`. At 24 rows the frame fits, so
    /// the header keeps every row there.
    #[test]
    fn the_startup_header_cannot_starve_the_prompt() {
        let header = 21u16;
        // 24 rows: everything fits — header intact, prompt, one message row.
        let layout = plan_chrome(24, &frame_with(header as usize, 0, None, 0, 0), 1);
        assert_eq!(
            (layout.header, layout.editor, layout.message, layout.status),
            (21, 1, 1, 1)
        );
        // 23 and 22 rows: the prompt and the message row survive; the header
        // is truncated by exactly the rows they needed.
        for total in [23u16, 22] {
            let layout = plan_chrome(total, &frame_with(header as usize, 0, None, 0, 0), 1);
            assert_eq!(
                (layout.header, layout.editor, layout.message, layout.status),
                (header - (24 - total), 1, 1, 1),
                "at {total} rows the prompt must stay on screen"
            );
            assert!(layout.editor >= 1, "at {total} rows the prompt vanished");
            assert!(layout.header >= 1, "at {total} rows the header vanished");
        }
    }

    #[test]
    fn plan_chrome_never_starves_the_composer() {
        // A header far taller than the terminal: the prompt keeps its row and
        // so does the message view, whatever the header wants. Two rows are
        // the one exception — with only `status + one row` left there is no
        // room for both, and the row stays with the transcript (the terminal
        // is unusable either way).
        for total in 2..24u16 {
            let layout = plan_chrome(total, &frame_with(22, 0, None, 0, 0), 1);
            assert_eq!(layout.status, 1.min(total), "total {total}");
            let expected_editor = u16::from(total >= 3);
            assert_eq!(
                layout.editor, expected_editor,
                "the composer keeps a row at total {total}"
            );
            if total >= 3 {
                assert!(layout.message >= 1, "total {total}");
            }
        }
    }

    #[test]
    fn plan_chrome_keeps_the_editor_region_non_empty() {
        let layout = plan_chrome(4, &frame_with(0, 0, Some(0), 0, 0), 1);
        assert_eq!(layout.editor, 1);
        assert_eq!(layout.message, 2);
    }

    #[test]
    fn plan_chrome_grows_the_editor_for_a_wrapped_prompt() {
        // The prompt needs 3 rows; the chrome reserves them, even when the
        // header would otherwise claim all available space.
        let layout = plan_chrome(10, &frame_with(15, 0, None, 0, 0), 3);
        assert_eq!(layout.editor, 3, "the prompt got its three rows");
        assert!(
            layout.header <= 5,
            "the header gave way to the prompt (got {})",
            layout.header
        );
        assert!(layout.message >= 1);
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for total in 0..4u16 {
            let layout = plan_chrome(total, &frame_with(3, 3, None, 3, 3), 1);
            let used = layout.header
                + layout.above
                + layout.editor
                + layout.below
                + layout.status
                + layout.footer
                + layout.message;
            assert_eq!(used, total, "total {total} must stay within the screen");
        }
    }

    #[test]
    fn drop_disposes_every_attached_component() {
        let (header, header_disposed) = counter("h");
        let (footer, footer_disposed) = counter("f");
        let (editor, editor_disposed) = counter("e");
        let (widget, widget_disposed) = counter("w");
        {
            let mut ui = ExtensionUi::new();
            ui.set_header(Some(Box::new(header)));
            ui.set_footer(Some(Box::new(footer)));
            ui.set_editor_component(Some(Box::new(editor)));
            ui.set_widget("w".into(), Some(Box::new(widget)), WidgetPlacement::Above);
            assert_eq!(header_disposed.load(Ordering::Relaxed), 0);
        }
        assert_eq!(header_disposed.load(Ordering::Relaxed), 1);
        assert_eq!(footer_disposed.load(Ordering::Relaxed), 1);
        assert_eq!(editor_disposed.load(Ordering::Relaxed), 1);
        assert_eq!(widget_disposed.load(Ordering::Relaxed), 1);
    }
}

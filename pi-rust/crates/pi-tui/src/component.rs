//! The [`Component`] abstraction behind the extension UI host surface.
//!
//! Upstream's TUI is built from components: every region of the alt-screen
//! is a `Component` that renders itself to lines
//! (`packages/tui/src/tui.ts:111-134`), and extensions receive the same type
//! from `ctx.ui.setHeader` / `setFooter` / `setWidget` / `custom`
//! (`packages/coding-agent/src/core/extensions/types.ts:150-262`). This
//! crate's components (message view, prompt, selector, settings list, dialog)
//! each draw themselves straight into a `ratatui::buffer::Buffer`, so until
//! now there was no object an extension component could be attached to.
//!
//! This module adds the missing half — a minimal, object-safe trait that the
//! [`App`](crate::App) can host as `Box<dyn Component>` in its extension
//! regions — without re-fitting the existing components, which keep their
//! `render_to_buffer` inherent methods.
//!
//! # Why `render(&self, width) -> Vec<StyledLine>`
//!
//! The issue left the choice between this signature and an equivalent
//! `render_to_buffer(area, buf, theme)` open; this port picks the line
//! buffer, for three reasons:
//!
//! * **It is upstream's shape.** `Component.render(width: number): string[]`
//!   (`packages/tui/src/tui.ts:117`) hands back lines for a width and nothing
//!   else. A component that also reached for the screen buffer would have to
//!   know the region's origin, which is exactly the layout knowledge the host
//!   is supposed to own.
//! * **Theme stays a host concern.** A [`StyledLine`] carries
//!   [`SpanStyle`] *slots*, not resolved colours, so the host resolves them
//!   through the live [`Theme`](crate::Theme) with the same
//!   [`write_styled_line`](crate::utils::styled::write_styled_line) path every
//!   built-in component uses. A component can therefore never hard-code a
//!   colour, and a theme hot-swap takes effect on the next frame without the
//!   component knowing.
//! * **One render path for every surface.** The live frame
//!   ([`App::render_to_buffer`](crate::App::render_to_buffer)) and the flat
//!   snapshot ([`App::render_snapshot`](crate::App::render_snapshot)) both
//!   drive the same host layout, so quick offline snapshot tests exercise
//!   exactly the pixels the terminal gets.
//!
//! # Cross-bridge note
//!
//! Wiring a JavaScript factory (`ctx.ui.setWidget(key, factory)`) to a Rust
//! `Box<dyn Component>` is a **later** task: this module only defines the
//! Rust-side host surface. Nothing here executes JS or touches
//! `pi-extensions`.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use crate::core::input_parse::Key;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// A hostable UI region.
///
/// Implemented by extensions (and by [`TextComponent`] for the common case of
/// a block of plain or uniformly styled lines) and attached to the
/// [`App`](crate::App) through its extension UI host surface:
/// `set_header` / `set_footer` / `set_widget` / `set_editor_component` /
/// `open_custom`.
///
/// The trait mirrors upstream's `Component`
/// (`packages/tui/src/tui.ts:111-134`) minus the mouse and invalidation
/// hooks, which this host surface does not route yet (see
/// [`App::open_custom`](crate::App::open_custom)).
pub trait Component: Send + Sync {
    /// Render the component for a viewport `width` in columns.
    ///
    /// Returns one [`StyledLine`] per screen row. The host clips each line to
    /// the region width and drops rows that do not fit the region height, so
    /// a component may return more lines than there is room for.
    fn render(&self, width: u16) -> Vec<StyledLine>;

    /// Handle a key while the component holds focus.
    ///
    /// Returns `true` when the key was consumed. A `false` return lets the
    /// host fall through to the next layer (the existing modals, the
    /// transcript search overlay, the viewport chords, and finally the
    /// prompt), which is this port's stand-in for upstream components calling
    /// `super.handleInput(data)` for keys they do not handle.
    ///
    /// The default is "consume nothing", so a render-only component can
    /// ignore input entirely.
    fn handle_input(&mut self, key: Key) -> bool {
        let _ = key;
        false
    }

    /// Release resources held by the component.
    ///
    /// The host calls this exactly once, when the component is replaced,
    /// cleared, or its custom session closes — the Rust shape of upstream's
    /// `Component & { dispose?(): void }`
    /// (`packages/coding-agent/src/core/extensions/types.ts:175`). The
    /// default is a no-op.
    fn dispose(&mut self) {}

    /// Return the layout node describing how to lay this component out,
    /// or `None` if the component has no children to layout. Mirrors
    /// upstream's `Symbol.for("@earendil-works/pi-tui/layout-node")`
    /// lookup (`packages/tui/src/layout-node.ts:3,48-51`). The default
    /// returns `None`; layout-aware components override it.
    fn layout_node(&self) -> Option<crate::app::layout_node::LayoutNode> {
        None
    }
}

/// A [`Component`] slot the App can host in a UI region.
///
/// Mirrors upstream's `Component` (`packages/tui/src/tui.ts:111-134`)
/// extended with the mouse, focus and bounding-rect hooks this port
/// routes through its layout host. The plan calls it the foundational
/// trait for P0: every region (`setHeader` / `setFooter` / `setWidget` /
/// `setEditorComponent` / `openCustom`) accepts a `Arc<dyn ComponentSlot>`
/// and dispatches input / mouse / focus to whichever slot owns the
/// pointer.
///
/// Most existing extension components already implement [`Component`];
/// the blanket impl at the bottom of this module gives them
/// mouse/focus/bounds defaults so they keep compiling while new
/// extensions opt into the fuller contract.
pub trait ComponentSlot: Component {
    /// Process a mouse event while this slot owns the cursor.
    ///
    /// Returns `true` when the event was consumed (the host stops
    /// dispatching it). Mirrors upstream's `handleMouse?(event)`
    /// (`packages/tui/src/components/component-base.ts:30-37`).
    ///
    /// The default declines every event so a render-only slot is safe.
    fn handle_mouse(&mut self, _event: MouseEvent) -> bool {
        false
    }

    /// Whether this slot currently holds input focus.
    ///
    /// Mirrors upstream's `focusable?: boolean` property
    /// (`packages/tui/src/components/component-base.ts:13`). The default
    /// is `false` — render-only components stay out of the focus chain.
    fn focusable(&self) -> bool {
        false
    }

    /// The bounding rect the host renders this slot into.
    ///
    /// Mirrors upstream's `getBoundingRect?(): { x, y, width, height }`
    /// (`packages/tui/src/components/component-base.ts:18-23`); a slot
    /// returns `None` when it has not been laid out yet, which lets the
    /// host skip mouse dispatch instead of inventing a stale rectangle.
    fn bounding_rect(&self) -> Option<Rect> {
        None
    }
}

/// The mouse events the host surfaces to a [`ComponentSlot`].
///
/// Upstream's `MouseEvent` (`packages/tui/src/tui.ts:95-105`) carries
/// `event`, `button`, `col`, `row`, plus modifiers; the Rust port
/// flattens that to a struct whose fields are the ones the slot needs
/// for hit-testing. The host fills in the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Which button changed state.
    pub button: MouseButton,
    /// Press / release / motion / scroll.
    pub kind: MouseKind,
    /// Column (1-based) the event landed on.
    pub col: u16,
    /// Row (1-based) the event landed on.
    pub row: u16,
}

/// Mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Scroll wheel — paired with [`MouseKind::Scroll`].
    Wheel,
}

/// What happened with [`MouseEvent::button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Move,
    /// Wheel scroll; [`MouseEvent::col`] is the delta, positive = right,
    /// negative = left.
    ScrollH,
    /// Wheel scroll; [`MouseEvent::row`] is the delta, positive = down,
    /// negative = up.
    ScrollV,
}

/// Top-left origin rectangle in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl ComponentSlot for TextComponent {}

/// Where a widget renders relative to the editor region.
///
/// Upstream `WidgetPlacement` (`packages/coding-agent/src/core/extensions/types.ts:106`)
/// is `"aboveEditor" | "belowEditor"`; [`WidgetPlacement::Above`] is the
/// default, matching `options?.placement ?? "aboveEditor"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WidgetPlacement {
    /// Between the message view and the editor region (upstream
    /// `aboveEditor`). The default.
    #[default]
    Above,
    /// Between the editor region and the status bar (upstream
    /// `belowEditor`).
    Below,
}

/// Anchor point of a `custom` overlay inside the terminal.
///
/// The subset of upstream's `OverlayAnchor`
/// (`packages/tui/src/tui.ts:204-212`) that this host surface positions;
/// percentage offsets and the `row` / `col` overrides are not ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OverlayAnchor {
    /// Centred in the terminal. Upstream's default.
    #[default]
    Center,
    /// Margin-relative top-left corner.
    TopLeft,
    /// Margin-relative top-right corner.
    TopRight,
    /// Margin-relative bottom-left corner.
    BottomLeft,
    /// Margin-relative bottom-right corner.
    BottomRight,
}

/// Options for [`App::open_custom`](crate::App::open_custom).
///
/// The Rust counterpart of the `{ overlay, overlayOptions, onHandle }`
/// argument of upstream's `ctx.ui.custom`
/// (`packages/coding-agent/src/core/extensions/types.ts:200-212`):
/// `overlay` is [`CustomOptions::overlay`], the `overlayOptions` subset this
/// port positions is the remaining fields, and `onHandle` is the
/// [`CustomHandle`] returned by `open_custom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomOptions {
    /// Render as an overlay on top of the whole frame (`true`) or swap the
    /// component into the editor region (`false`).
    ///
    /// Defaults to `false`, matching upstream's `options?.overlay ?? false`.
    /// [`CustomOptions::overlay`] builds the common topmost variant.
    pub overlay: bool,
    /// Overlay width in columns. `None` spans the terminal width.
    pub width: Option<u16>,
    /// Maximum overlay height in rows. `None` grows to the component's line
    /// count (bounded by the terminal height).
    pub max_height: Option<u16>,
    /// Where the overlay box is anchored. Ignored when `overlay` is `false`.
    pub anchor: OverlayAnchor,
    /// Inset from the anchored corner / edge, in cells. Ignored when
    /// `overlay` is `false`.
    pub margin: u16,
}

impl Default for CustomOptions {
    fn default() -> Self {
        Self {
            overlay: false,
            width: None,
            max_height: None,
            anchor: OverlayAnchor::Center,
            margin: 0,
        }
    }
}

impl CustomOptions {
    /// Topmost overlay options with a one-cell margin — the common
    /// `ctx.ui.custom(factory, { overlay: true })` shape.
    pub fn overlay() -> Self {
        Self {
            overlay: true,
            margin: 1,
            ..Self::default()
        }
    }

    /// Set the overlay width in columns.
    pub fn width(mut self, width: u16) -> Self {
        self.width = Some(width);
        self
    }

    /// Cap the overlay height in rows.
    pub fn max_height(mut self, max_height: u16) -> Self {
        self.max_height = Some(max_height);
        self
    }

    /// Set the overlay anchor.
    pub fn anchor(mut self, anchor: OverlayAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Set the inset from the anchored edge.
    pub fn margin(mut self, margin: u16) -> Self {
        self.margin = margin;
        self
    }
}

/// Handle returned by [`App::open_custom`](crate::App::open_custom).
///
/// The Rust counterpart of upstream's `OverlayHandle`
/// (`packages/tui/src/tui.ts:270-284`) reduced to what the host surface can
/// honour: visibility control plus the result channel the custom session
/// answers on. The remaining `OverlayHandle` operations (`focus`,
/// `unfocus`, `isFocused`, `getBounds`) have no equivalent because this host
/// keeps a single custom session with a fixed focus priority.
///
/// The handle stays useful after the session closes: `set_visible` then only
/// flips a detached flag, and the result channel reports the close.
pub struct CustomHandle {
    id: u64,
    visible: Arc<AtomicBool>,
    result: Option<Receiver<Option<String>>>,
}

impl CustomHandle {
    /// Build the handle from the pieces the host owns. Kept crate-private:
    /// callers get handles from `open_custom`.
    pub(crate) fn new(id: u64, visible: Arc<AtomicBool>, result: Receiver<Option<String>>) -> Self {
        Self {
            id,
            visible,
            result: Some(result),
        }
    }

    /// Session identifier, unique per App and increasing with every
    /// `open_custom`.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Temporarily show or hide the component (upstream `setHidden`).
    ///
    /// A hidden component is neither rendered nor given input; its session
    /// stays open until [`App::close_custom`](crate::App::close_custom).
    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
    }

    /// Whether the component is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// Take the channel that delivers the result of
    /// [`App::close_custom`](crate::App::close_custom).
    ///
    /// Sending the value from the app side never blocks (the channel is
    /// unbounded), so the host can close a session from the render loop. The
    /// receiver is a `std::sync::mpsc` handle: async callers should poll it
    /// with [`Receiver::try_recv`] from a render/tick loop or move it to a
    /// blocking task rather than block an executor.
    ///
    /// Returns `None` on the second call — the channel can only be handed out
    /// once.
    pub fn take_result_receiver(&mut self) -> Option<Receiver<Option<String>>> {
        self.result.take()
    }

    /// Non-blocking peek at the session result.
    ///
    /// `None` means the session is still open; `Some(result)` means it
    /// closed, with `result` being the value passed to
    /// [`App::close_custom`](crate::App::close_custom) (`None` when it closed
    /// without one). Once a result has been read, later calls return `None`
    /// again.
    pub fn try_recv_result(&mut self) -> Option<Option<String>> {
        let receiver = self.result.as_ref()?;
        match receiver.try_recv() {
            Ok(result) => {
                self.result = None;
                Some(result)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.result = None;
                Some(None)
            }
        }
    }
}

impl fmt::Debug for CustomHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomHandle")
            .field("id", &self.id)
            .field("visible", &self.is_visible())
            .field("result_channel_open", &self.result.is_some())
            .finish()
    }
}

/// A [`Component`] that renders a fixed block of lines.
///
/// Covers the other half of upstream's `setWidget` overload — the
/// `string[]` content, which upstream wraps in `Container` + `Text` rows
/// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2210-2221`).
/// Lines are rendered verbatim: the host clips them to the region width, so
/// no wrapping happens here (upstream's `Text` wraps; this helper does not,
/// see the module docs on truncation).
#[derive(Debug, Clone, Default)]
pub struct TextComponent {
    lines: Vec<StyledLine>,
}

impl TextComponent {
    /// Build a plain-text block from any iterator of string-ish lines.
    pub fn new<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::styled(lines, SpanStyle::PLAIN)
    }

    /// Build a block whose every span carries `style`.
    ///
    /// The extension-facing way to stay theme-agnostic: pass a
    /// [`SpanStyle`] slot such as `SpanStyle::fg(ThemeColor::Muted)` and the
    /// host resolves it against the live theme.
    pub fn styled<I, S>(lines: I, style: SpanStyle) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            lines: lines
                .into_iter()
                .map(|line| vec![StyledSpan::new(line, style)])
                .collect(),
        }
    }

    /// Build a block from already-styled lines.
    pub fn from_lines(lines: Vec<StyledLine>) -> Self {
        Self { lines }
    }

    /// Replace the block's content.
    pub fn set_lines<I, S>(&mut self, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        *self = Self::new(lines);
    }

    /// The block's current lines.
    pub fn lines(&self) -> &[StyledLine] {
        &self.lines
    }
}

impl Component for TextComponent {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let _ = width;
        self.lines.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeColor;

    #[test]
    fn text_component_renders_plain_lines() {
        let component = TextComponent::new(["one", "two"]);
        let lines = component.render(40);
        assert_eq!(lines.len(), 2);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "one");
        assert_eq!(crate::utils::styled::plain_text(&lines[1]), "two");
        assert_eq!(lines[0][0].style, SpanStyle::PLAIN);
    }

    #[test]
    fn text_component_styles_every_span_with_the_slot() {
        let component = TextComponent::styled(["muted"], SpanStyle::fg(ThemeColor::Muted));
        let lines = component.render(10);
        assert_eq!(lines[0][0].style, SpanStyle::fg(ThemeColor::Muted));
    }

    #[test]
    fn default_placement_and_anchor_match_upstream() {
        assert_eq!(WidgetPlacement::default(), WidgetPlacement::Above);
        assert_eq!(OverlayAnchor::default(), OverlayAnchor::Center);
        assert!(!CustomOptions::default().overlay);
        assert!(CustomOptions::overlay().overlay);
        assert_eq!(CustomOptions::overlay().margin, 1);
    }

    #[test]
    fn custom_handle_visibility_and_result_channel() {
        let visible = Arc::new(AtomicBool::new(true));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut handle = CustomHandle::new(7, visible.clone(), rx);
        assert_eq!(handle.id(), 7);
        assert!(handle.is_visible());

        handle.set_visible(false);
        assert!(!handle.is_visible());
        assert!(!visible.load(Ordering::Relaxed));

        assert_eq!(handle.try_recv_result(), None);
        tx.send(Some("picked".to_string())).expect("open channel");
        assert_eq!(handle.try_recv_result(), Some(Some("picked".to_string())));
        assert_eq!(handle.try_recv_result(), None);

        assert!(handle.take_result_receiver().is_none());
    }
}

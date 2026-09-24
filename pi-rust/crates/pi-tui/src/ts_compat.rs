//! TS pi-tui compatibility shim.
//!
//! The names exported from this module exist solely so that the
//! `ts_contract.rs` compile-time check in this crate (and any plugin that
//! imports `pi_tui::<NAME>`) can resolve every symbol that the TS
//! `packages/tui/src/index.ts` re-exports. None of these names is a
//! complete implementation: most are type aliases pointing at the
//! existing Rust surface, and the small handful that have no Rust
//! counterpart yet is intentionally a stub.
//!
//! The goal of this file is **shape alignment only** — the layered
//! migration in `tui-modularization-report.md` §3.15 (especially layers
//! 2/3/4) is what fleshes these stubs out. Until then, a plugin that
//! imports one of the placeholder names will type-check, but calling it
//! will return an empty/zero value.
//!
//! Naming follows upstream: types, traits, enums and constants keep
//! their PascalCase / SCREAMING_SNAKE_CASE spelling so a JS/TS plugin
//! author can read `pi_tui::Editor` / `pi_tui::CURSOR_MARKER` against
//! `pi-tui`'s docs without having to translate case in their head.

#![allow(dead_code)]

use crate::component::Component;

// ─── marked (npm) re-exports ────────────────────────────────────────
//
// Upstream `import { Marked, Token, Tokens } from "marked"`. The Rust
// port has its own tokenizer in `crate::highlight`; the `Token` here
// is an alias for that one (it carries `kind: TokenKind` + a `text`
// span). `Tokens` is a `Vec<Token>` so plugin authors can copy
// upstream's loop body verbatim. `Marked` is a thin trait that future
// renderers can implement once marked's parser lands.

// (No re-export needed: `Token` is already re-exported from `crate::highlight`
// at the crate root. Keeping the alias visible here as a private use so the
// `Tokens` definition below resolves.)
use crate::highlight::Token;
/// Upstream `Tokens` — a sequence of marked token records.
pub type Tokens = Vec<Token>;

/// Upstream `Marked` — the marked renderer trait. The Rust port has no
/// full marked implementation yet; this stub trait exists so plugin
/// code that types a parameter as `&dyn Marked` compiles.
pub trait Marked {
    /// Render `src` to a string of styled lines. Upstream returns HTML;
    /// the Rust port will return `Vec<StyledLine>` once marked lands.
    fn render(&self, src: &str) -> Vec<crate::styled::StyledLine>;
}

// ─── components/box ─────────────────────────────────────────────────
//
// Upstream `Box` is a generic container — `new Box(...)` in TS. Rust
// already has `std::boxed::Box<T>`; we just re-export it under the
// upstream name so a plugin author doesn't have to remember that the
// Rust name is `std::boxed::Box`.

// `pub use` so a plugin can name `pi_tui::Box<T>`. The TS upstream's `Box` is
// a generic container; the Rust std type is the natural counterpart.
pub use std::boxed::Box;

// ─── components/cancellable-loader ──────────────────────────────────
//
// Upstream `CancellableLoader` is a `Component` with an extra
// `cancel()` method. The stub here is a trait with a no-op default so
// any type that implements `Component` automatically satisfies it.

/// Upstream `CancellableLoader` — a `Component` that can be aborted.
pub trait CancellableLoader: Component {
    /// Request cancellation. Default is a no-op so component authors
    /// can opt-in by overriding it.
    fn cancel(&mut self) {}
}

// ─── components/editor ──────────────────────────────────────────────
//
// `Editor` already exists; the two options/theme structs are new
// placeholders that mirror upstream's `EditorOptions` and
// `EditorTheme` fields as a no-op `Default`-able shell. A plugin can
// construct `EditorOptions::default()` today; the surface will grow
// when the editor integration lands in layer 4.

// (No re-export needed: `Editor` is already re-exported from `crate::editor`
// at the crate root.)

use crate::keybindings::KeybindingDefinition;

/// Upstream `EditorOptions` — runtime options for the editor widget.
/// Placeholder until the editor integration lands in layer 4.
#[derive(Debug, Clone, Default)]
pub struct EditorOptions {
    /// Optional theme override (placeholder field).
    pub theme: Option<EditorTheme>,
}

/// Upstream `EditorTheme` — theming knobs for the editor widget.
/// Placeholder until the editor integration lands in layer 4.
#[derive(Debug, Clone, Default)]
pub struct EditorTheme;

// ─── components/h-stack ─────────────────────────────────────────────

/// Upstream `HStack` — a horizontal stack container. Stub trait; the
/// real layout is in `crate::viewport` and will be exposed via an
/// `HStack::new(...)` constructor in layer 2.
pub trait HStack: Component {}

// ─── components/image ───────────────────────────────────────────────
//
// All three (`Image`, `ImageOptions`, `ImageTheme`) already exist in
// `crate::image`. Nothing to add.

// ─── components/input ───────────────────────────────────────────────

/// Upstream `Input` — a single-line input component. Stub.
#[derive(Debug, Clone, Default)]
pub struct Input;

/// Upstream `JumpDirection` — alias for the existing enum in
/// `crate::editor`.
// (No re-export needed: `JumpDirection` is already re-exported from
// `crate::editor` at the crate root.)

/// Upstream `JUMP_DIRECTION` — a frozen list of the two jump
/// directions. Mirrors the TS literal `{ forward: "next",
/// backward: "previous" }` as a `&[(&str, &str)]`.
pub const JUMP_DIRECTION: &[(&str, &str)] = &[("forward", "next"), ("backward", "previous")];

// ─── components/loader ──────────────────────────────────────────────

/// Upstream `Loader` — a spinner / progress component. Stub trait
/// until the loader integration (currently in `crate::loader`) gains
/// the full upstream surface.
pub trait Loader: Component {}

/// Upstream `LoaderIndicatorOptions` — options for the loader
/// indicator. Placeholder.
#[derive(Debug, Clone, Default)]
pub struct LoaderIndicatorOptions;

// ─── components/markdown ────────────────────────────────────────────

/// Upstream `Markdown` — the markdown component. Stub trait; the
/// rendering work itself lives in `crate::markdown::render_markdown`
/// (which is already exported) and will be wrapped in layer 2.
pub trait Markdown: Component {}

/// Upstream `MarkdownOptions` — options for the markdown component.
/// Placeholder.
#[derive(Debug, Clone, Default)]
pub struct MarkdownOptions {}

/// Upstream `DefaultTextStyle` — alias for `SpanStyle`, the existing
/// styled-span slot type.
pub type DefaultTextStyle = crate::styled::SpanStyle;

/// Upstream `MarkdownTheme` — markdown rendering theme. Placeholder.
#[derive(Debug, Clone, Default)]
pub struct MarkdownTheme {}

// ─── components/mouse-region ────────────────────────────────────────
//
// `MouseRegion` already exists in `crate::mouse_region`. The handler
// trait is a new stub.

/// Upstream `MouseRegionHandler` — callback signature for mouse
/// hits inside a `MouseRegion`. Stub.
pub trait MouseRegionHandler {
    /// A mouse hit at `(x, y)`. Default is a no-op.
    fn handle(&mut self, x: u16, y: u16) {
        let _ = (x, y);
    }
}

// ─── components/scroll-view ─────────────────────────────────────────

/// Upstream `ScrollView` — a scrollable viewport. Stub.
#[derive(Debug, Clone, Default)]
pub struct ScrollView {}

/// Upstream `ScrollViewOptions` — options for `ScrollView`.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewOptions {}

/// Upstream `ScrollViewScrollbar` — scrollbar rendering options.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewScrollbar {}

/// Upstream `ScrollViewScrollToOptions` — options for
/// `scrollView.scrollTo(...)`.
#[derive(Debug, Clone, Default)]
pub struct ScrollViewScrollToOptions {}

// ─── components/select-list ─────────────────────────────────────────
//
// The three core types are already exported as `SelectorItem`,
// `Selector`, and `SelectorLayout`. The theme and truncate flag are
// new stubs.

/// Upstream `SelectListTheme` — theming for the selector. Placeholder.
#[derive(Debug, Clone, Default)]
pub struct SelectListTheme {}

/// Upstream `SelectListTruncatePrimaryContext` — whether the primary
/// context column gets truncated. `false` matches upstream's default
/// (no truncation).
pub type SelectListTruncatePrimaryContext = bool;

// ─── components/settings-list ──────────────────────────────────────
//
// `SettingItem` and `SettingsList` are already exported. Theme is
// new.

/// Upstream `SettingsListTheme` — theming for the settings list.
/// Placeholder.
#[derive(Debug, Clone, Default)]
pub struct SettingsListTheme {}

// ─── components/spacer ──────────────────────────────────────────────

/// Upstream `Spacer` — a flex spacer component. Stub trait.
pub trait Spacer: Component {}

// ─── components/text ────────────────────────────────────────────────

/// Upstream `Text` — a plain text component. Stub trait; the existing
/// `TextComponent` is the runtime impl and will satisfy this once
/// layer 2 lands.
pub trait Text: Component {}

// ─── components/truncated-text ──────────────────────────────────────

/// Upstream `TruncatedText` — text that truncates to fit its width.
/// Stub trait.
pub trait TruncatedText: Component {}

// ─── components/v-stack ─────────────────────────────────────────────

/// Upstream `StackOptions` — options for `VStack`. Stub.
pub type StackOptions = ();

/// Upstream `StackEntryOptions` — options for a single stack entry.
/// Stub.
pub type StackEntryOptions = ();

/// Upstream `StackEntry` — a single entry inside a `VStack`. Stub
/// type alias for `String` until the real shape lands.
pub type StackEntry = String;

/// Upstream `StackChild` — a child of a stack. Stub type alias for
/// `String`.
pub type StackChild = String;

/// Upstream `VStack` — a vertical stack container. Stub trait.
pub trait VStack: Component {}

// ─── editor-component ──────────────────────────────────────────────

/// Upstream `EditorComponent` — the editor component contract. Stub
/// trait; the real shape lands with the editor integration in layer
/// 4.
pub trait EditorComponent: Component {}

// ─── keybindings ───────────────────────────────────────────────────
//
// `KeybindingDefinition`, `KeybindingsConfig`, `KeybindingsManager`
// are already exported. The four names below are new.

/// Upstream `Keybinding` — alias for `KeybindingDefinition` (the
/// per-action record). The two names differ in upstream but describe
/// the same shape; we pick the Rust-canonical one as the canonical
/// name and re-export the TS spelling as an alias.
pub use crate::keybindings::KeybindingDefinition as Keybinding;

/// Upstream `KeybindingDefinitions` — a list of keybinding records.
pub type KeybindingDefinitions = Vec<KeybindingDefinition>;

/// Upstream `Keybindings` — alias for the internal
/// `TuiKeybindingDefinitions` slice alias. The two are spelled
/// `Keybindings` / `KeybindingsManager` upstream; we keep both names
/// distinct in Rust (the manager is the runtime object, this alias
/// points at the static definition slice).
pub use crate::keybindings::TuiKeybindingDefinitions as Keybindings;

/// Upstream `TUI_KEYBINDINGS` — the constant identifier for the
/// default keybindings table. The Rust port exposes the same data
/// through `tui_default_keybindings()` (already exported); the const
/// re-export here is a stub `&[&str]` so a plugin that names it
/// compiles.
pub const TUI_KEYBINDINGS: &[&str] = &[];

// ─── latex ─────────────────────────────────────────────────────────
//
// `render_latex` already exists; only the options struct is new.

/// Upstream `renderLatex` — alias for the existing
/// `crate::latex::render_latex`.
pub use crate::latex::render_latex;

/// Upstream `RenderLatexOptions` — options for the latex renderer.
/// Placeholder; the existing `render_latex` takes no options today.
#[derive(Debug, Clone, Default)]
pub struct RenderLatexOptions {}

// ─── native-platform ───────────────────────────────────────────────
//
// The clipboard plumbing lives in `crate::clipboard` (OSC 52). The
// native-platform trait is a new stub; the default impl returns no
// clipboard so existing OSC 52 callers keep working unchanged.

/// Upstream `NativeClipboard` — host-supplied native clipboard
/// bridge. Stub trait; the Rust port falls back to OSC 52 today.
pub trait NativeClipboard {
    /// Read the current clipboard contents. Default is empty.
    fn read(&self) -> String {
        String::new()
    }
    /// Write `text` to the clipboard. Default is a no-op.
    fn write(&mut self, text: &str) {
        let _ = text;
    }
}

/// Upstream `getNativeClipboard` — fetch the host's native
/// clipboard. Returns a boxed trait object whose default impl falls
/// through to OSC 52.
pub fn get_native_clipboard() -> std::boxed::Box<dyn NativeClipboard> {
    struct Fallback;
    impl NativeClipboard for Fallback {}
    std::boxed::Box::new(Fallback)
}

// ─── stdin-buffer ──────────────────────────────────────────────────

/// Upstream `StdinBuffer` — a buffered stdin reader. Stub.
#[derive(Debug, Clone, Default)]
pub struct StdinBuffer {}

/// Upstream `StdinBufferEventMap` — event names for `StdinBuffer`.
/// Stub trait.
pub trait StdinBufferEventMap {}

/// Upstream `StdinBufferOptions` — options for `StdinBuffer`.
#[derive(Debug, Clone, Default)]
pub struct StdinBufferOptions {}

// ─── terminal-colors ───────────────────────────────────────────────
//
// Four new names — none has a Rust counterpart yet, so all four are
// stub definitions.

/// Upstream `parseOsc11BackgroundColor` — parse an OSC 11 background
/// color report. Stub returning `None`.
pub fn parse_osc11_background_color(_input: &str) -> Option<RgbColor> {
    None
}

/// Upstream `parseTerminalColorSchemeReport` — parse an OSC color
/// scheme report. Stub returning `None`.
pub fn parse_terminal_color_scheme_report(
    _input: &str,
) -> Option<std::boxed::Box<dyn TerminalColorScheme>> {
    None
}

/// Upstream `RgbColor` — an RGB triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
}

/// Upstream `TerminalColorScheme` — terminal color scheme report.
/// Stub trait.
pub trait TerminalColorScheme {}

// ─── tui ────────────────────────────────────────────────────────────
//
// `Component` and `OverlayAnchor` already exist. The remaining ~20
// names are new stubs that mirror the upstream `TUI` interface.

// (No re-export needed: `Component` and `OverlayAnchor` are already
// re-exported from `crate::component` at the crate root.)

/// Upstream `Container` — a `Component` that contains other
/// components. Stub trait.
pub trait Container: Component {}

/// Upstream `CURSOR_MARKER` — the zero-width-space marker used to
/// mark a hidden cursor cell in a rendered line. Mirrors
/// `packages/tui/src/tui.ts` `'​'`.
pub const CURSOR_MARKER: char = '\u{200B}';

/// Upstream `compositeTuiLine` — merge a list of styled lines into a
/// single composite line. Stub returning an empty line.
pub fn composite_tui_line(_parts: &[crate::styled::StyledLine]) -> crate::styled::StyledLine {
    Vec::new()
}

/// Upstream `Focusable` — a component that can hold focus. Stub trait.
pub trait Focusable {
    /// Whether this object currently holds focus. Default is `false`.
    fn is_focused(&self) -> bool {
        false
    }
}

/// Upstream `isFocusable` — runtime predicate that mirrors
/// `Focusable::is_focused`.
pub fn is_focusable<T: Focusable>(t: &T) -> bool {
    t.is_focused()
}

/// Upstream `ViewportTUI` — a TUI that owns a viewport. Stub trait.
pub trait ViewportTUI {}

/// Upstream `isViewportTUI` — runtime predicate that returns
/// `true` for `ViewportTUI` implementors.
pub fn is_viewport_tui<T: ViewportTUI>(_: &T) -> bool {
    true
}

/// Upstream `OverlayBounds` — the bounding rect of an overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayBounds {
    /// Top-left column (cells).
    pub x: u16,
    /// Top-left row (cells).
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

/// Upstream `OverlayMargin` — the inset of an overlay from its
/// anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayMargin {
    /// Top inset.
    pub top: u16,
    /// Right inset.
    pub right: u16,
    /// Bottom inset.
    pub bottom: u16,
    /// Left inset.
    pub left: u16,
}

/// Upstream `OverlayHandle` — alias for `CustomHandle`, the existing
/// handle returned by `App::open_custom`.
pub use crate::component::CustomHandle as OverlayHandle;

/// Upstream `OverlayOptions` — options for opening an overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayOptions {
    /// Anchor point of the overlay.
    pub anchor: crate::component::OverlayAnchor,
    /// Margin from the anchor.
    pub margin: OverlayMargin,
}

/// Upstream `OverlayUnfocusOptions` — options for unfocusing an
/// overlay. Placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayUnfocusOptions {}

/// Upstream `SizeValue` — a pixel / cell size with optional unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SizeValue {
    /// The size in cells.
    pub value: u16,
}

/// Upstream `TUI` — the top-level TUI driver trait. Stub; the real
/// driver lives in `crate::App` and `crate::terminal` today and will
/// be re-exposed under this name once the layer-2 split lands.
pub trait TUI {}

/// Result of dispatching a key event to a `TuiInputListener`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TuiInputListenerResult {
    /// The key was consumed.
    Consumed,
    /// The key was ignored and the host should try the next handler.
    #[default]
    Ignored,
}

/// Upstream `TuiInputListener` — callback signature for input
/// listeners. Stub trait.
pub trait TuiInputListener {
    /// Handle a raw input chunk. Default returns `Ignored`.
    fn on_input(&mut self, data: &str) -> TuiInputListenerResult {
        let _ = data;
        TuiInputListenerResult::Ignored
    }
}

/// Upstream `TuiMode` — a TUI mode (alt-screen vs main-screen vs
/// custom). Stub trait.
pub trait TuiMode {}

/// Upstream `TuiMouseButton` — the mouse button that triggered an
/// event. Alias for `crate::input::MouseButton` (an enum).
pub use crate::input::MouseButton as TuiMouseButton;

/// Upstream `TuiMouseEvent` — a single mouse event. Stub struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiMouseEvent {
    /// Which button was involved.
    pub button: TuiMouseButton,
    /// The kind of mouse event.
    pub kind: TuiMouseEventType,
    /// Column in cells.
    pub x: u16,
    /// Row in cells.
    pub y: u16,
}

impl Default for TuiMouseEvent {
    fn default() -> Self {
        Self {
            // Both aliased enums (`TuiMouseButton` / `TuiMouseEventType`)
            // are upstream plain enums without a `Default` impl, so we
            // pick the first variant by name. This matches upstream's
            // "all fields off" semantics for the empty event.
            button: TuiMouseButton::Left,
            kind: TuiMouseEventType::Move,
            x: 0,
            y: 0,
        }
    }
}

/// Upstream `TuiMouseEventType` — kind of mouse event. Alias for
/// `crate::input::MouseGestureKind` (the existing gesture enum).
pub use crate::input::MouseGestureKind as TuiMouseEventType;

/// Upstream `TuiMouseEventResult` — outcome of dispatching a mouse
/// event. Mirrors `TuiInputListenerResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TuiMouseEventResult {
    /// The event was consumed.
    Consumed,
    /// The event was ignored.
    #[default]
    Ignored,
}

/// Upstream `TuiStopOptions` — options for stopping a `TUI`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TuiStopOptions {}

// ─── tui-alt-screen ────────────────────────────────────────────────

/// Upstream `TuiAltScreen` — the alt-screen TUI driver. Stub.
#[derive(Debug, Clone, Default)]
pub struct TuiAltScreen {}

/// Upstream `TuiAltScreenOptions` — options for `TuiAltScreen`.
#[derive(Debug, Clone, Default)]
pub struct TuiAltScreenOptions {}

// ─── tui-main-screen ──────────────────────────────────────────────

/// Upstream `TuiMainScreen` — the main-screen (non-alt) TUI driver.
/// Stub.
#[derive(Debug, Clone, Default)]
pub struct TuiMainScreen {}

/// Upstream `TuiMainScreenRenderState` — render state for
/// `TuiMainScreen`. Stub.
#[derive(Debug, Clone, Default)]
pub struct TuiMainScreenRenderState {}
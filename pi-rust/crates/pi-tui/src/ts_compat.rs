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

// (Component + CustomHandle/OverlayAnchor are re-exported via the
// `crate::component::*` block below; no separate `use` is needed.)

// ─── Component (re-exported under its canonical name) ───────────────
//
// Upstream exports `Component` from `./tui.ts`. The Rust port already
// defines it in `crate::component`; surface it here so a plugin
// authoring against `pi_tui::ts_compat::*` does not have to depend on
// the private path layout.

pub use crate::component::{
    Component, CustomHandle as OverlayHandle, OverlayAnchor, TextComponent,
    WidgetPlacement as WidgetPlacementTs,
};

// ─── image ──────────────────────────────────────────────────────────
//
// Upstream exports `Image`, `ImageOptions`, `ImageTheme` from
// `./components/image.ts`. The Rust port defines them in
// `crate::components::image`; surface them under their TS names here.

pub use crate::components::image::{Image, ImageOptions, ImageTheme};

// ─── input (MouseRegion + JumpDirection live here) ──────────────────
//
// Upstream exports `MouseRegion` and `SettingItem` directly; the Rust
// port already has these in `crate::components::mouse_region` and
// `crate::components::settings`. Re-export so plugin code can name
// them through `ts_compat::*` and stay aligned with the upstream file
// layout.

pub use crate::components::mouse_region::MouseRegion;
pub use crate::components::settings::SettingItem;

// ─── fuzzy ──────────────────────────────────────────────────────────
//
// Upstream exports `FuzzyMatch`, `fuzzyFilter`, `fuzzyMatch` from
// `./fuzzy.ts`. The Rust port defines them in
// `crate::utils::fuzzy`; surface them here.

pub use crate::utils::fuzzy::{fuzzy_filter, fuzzy_match, FuzzyMatch};

// ─── keybindings ───────────────────────────────────────────────────
//
// Upstream exports `KeybindingConflict`, `KeybindingsConfig`,
// `KeybindingsManager`, `getKeybindings`, `setKeybindings` from
// `./keybindings.ts`. Surface them under their TS spellings.

pub use crate::components::keybindings::{
    get_keybindings, set_keybindings, KeybindingConflict, KeybindingDefinition,
    KeybindingsConfig, KeybindingsManager,
};

// ─── keys ──────────────────────────────────────────────────────────
//
// Upstream exports `decodeKittyPrintable`, `isKeyRelease`, etc. from
// `./keys.ts`. The Rust port has them in `crate::core::keys`.

pub use crate::core::keys::{
    decode_kitty_printable, is_key_release, is_key_repeat, is_kitty_protocol_active,
    matches_key, parse_key, set_kitty_protocol_active, KeyEventType, KeyId,
};

// ─── terminal ──────────────────────────────────────────────────────
//
// Upstream exports `ProcessTerminal`, `Terminal` from `./terminal.ts`.
// Surface them under their TS spellings.

pub use crate::terminal::process::{ProcessTerminal, Terminal};

// ─── terminal-image ────────────────────────────────────────────────
//
// Upstream exports the kitty/iTerm2 image helpers from
// `./terminal-image.ts`. The Rust port implements them in
// `crate::terminal::image`; surface the public surface here.

pub use crate::terminal::image::{
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities_with as detect_capabilities, encode_iterm2, encode_kitty,
    get_capabilities, get_cell_dimensions, get_gif_dimensions, get_image_dimensions,
    get_jpeg_dimensions, get_png_dimensions, get_webp_dimensions,
    CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions, image_fallback,
    render_image, reset_capabilities_cache, set_capabilities, set_capability_overrides,
    set_cell_dimensions, TerminalCapabilities,
};

// ─── JumpDirection ─────────────────────────────────────────────────
//
// Upstream exports `JumpDirection` as a type-only export from
// `./components/input.ts`. The Rust port has it in
// `crate::components::editor::JumpDirection`.

pub use crate::components::editor::JumpDirection;

// ─── utils ─────────────────────────────────────────────────────────
//
// Upstream exports `getOsc8LinkAtColumn`, `sliceByColumn`,
// `stripTerminalSequences`, `truncateToWidth`, `visibleWidth`,
// `wrapTextWithAnsi` from `./utils.ts`. The Rust port has them in
// `crate::utils` (alongside the legacy flat re-exports in
// `crate::utils::util`).

pub use crate::utils::util::{
    get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, truncate_to_width,
    wrap_text_with_ansi,
};
pub use crate::utils::hyperlink::{hyperlink, visible_width};

// ─── marked (npm) re-exports ────────────────────────────────────────
//
// Upstream `import { Marked, Token, Tokens } from "marked"`. The Rust
// port has its own tokenizer in `crate::highlight`; the `Token` here
// is an alias for that one (it carries `kind: TokenKind` + a `text`
// span). `Tokens` is a `Vec<Token>` so plugin authors can copy
// upstream's loop body verbatim. `Marked` is a thin trait that future
// renderers can implement once marked's parser lands.

pub use crate::utils::highlight::Token;
/// Upstream `Tokens` — a sequence of marked token records.
pub type Tokens = Vec<Token>;

/// Upstream `Marked` — the marked renderer trait. The Rust port has no
/// full marked implementation yet; this stub trait exists so plugin
/// code that types a parameter as `&dyn Marked` compiles.
pub trait Marked {
    /// Render `src` to a string of styled lines. Upstream returns HTML;
    /// the Rust port will return `Vec<StyledLine>` once marked lands.
    fn render(&self, src: &str) -> Vec<crate::utils::styled::StyledLine>;
}

// ─── components/box ─────────────────────────────────────────────────
//
// Upstream `Box` is a layout container — `packages/tui/src/components/box.ts`.
// The Rust port re-exports the impl from `crate::components::box_layout`
// where it is named `BoxLayout` to avoid shadowing `std::boxed::Box`.

pub use crate::components::BoxLayout as Box;

// ─── components/dynamic-border ──────────────────────────────────────
//
// Upstream `DynamicBorder` is the divider used by the "Update Available"
// and "Package Updates Available" notices
// (`packages/coding-agent/src/modes/interactive/components/dynamic-border.ts`).
// It renders `─`.repeat(width)` with a configurable colour.

pub use crate::components::DynamicBorder;

// ─── components/cancellable-loader ──────────────────────────────────

/// Upstream `CancellableLoader` — a `Component` that can be aborted.
pub trait CancellableLoader: Component {
    /// Request cancellation. Default is a no-op so component authors
    /// can opt-in by overriding it.
    fn cancel(&mut self) {}
}

/// Re-export of the cancellable loader component.
pub use crate::components::CancellableLoader as CancellableLoaderComponent;

/// Re-export of the alt-screen flash container.
pub use crate::components::AltScreenFlashContainer;
pub use crate::components::DEFAULT_DURATION_MS as ALT_SCREEN_FLASH_DEFAULT_DURATION_MS;

// ─── components/editor ──────────────────────────────────────────────
//
// (KeybindingDefinition is re-exported at the top of this module.)

/// Upstream `EditorOptions` — runtime options for the editor widget.
#[derive(Debug, Clone, Default)]
pub struct EditorOptions {
    /// Optional theme override (placeholder field).
    pub theme: Option<EditorTheme>,
}

/// Upstream `EditorTheme` — theming knobs for the editor widget.
#[derive(Debug, Clone, Default)]
pub struct EditorTheme;

// ─── components/h-stack ─────────────────────────────────────────────

pub use crate::components::HStack;

// ─── components/image ───────────────────────────────────────────────

pub use crate::components::{Image as ImageComponent, ImageOptions as ImageComponentOptions, ImageTheme as ImageComponentTheme};

// ─── components/input ───────────────────────────────────────────────

pub use crate::components::InputComponent as Input;

/// Upstream `JUMP_DIRECTION` — a frozen list of the two jump
/// directions. Mirrors the TS literal `{ forward: "next",
/// backward: "previous" }` as a `&[(&str, &str)]`.
pub const JUMP_DIRECTION: &[(&str, &str)] = &[("forward", "next"), ("backward", "previous")];

// ─── components/loader ──────────────────────────────────────────────

// ─── components/loader ──────────────────────────────────────────────
// `Loader` is a trait stub declared in this module — the upstream
// Loader is a spinner component; the Rust port renders spinners via
// `crate::components::loader::Spinner` instead.

/// Upstream `Loader` — a spinner component. Stub trait; the Rust
/// port uses `crate::components::loader::Spinner` directly.
pub trait Loader: Component {}

/// Upstream `LoaderIndicatorOptions` — options for a `Loader`
/// indicator. Stub struct.
#[derive(Debug, Clone, Default)]
pub struct LoaderIndicatorOptions {}

/// The Braille-pattern frame table that [`Spinner`] walks each tick
/// (`packages/tui/src/components/loader.ts:38-41`). Re-exported under its
/// TS name so plugin authors can inspect the animation without depending
/// on the internal module path.
pub use crate::components::loader::SPINNER_FRAMES as SpinnerFrames;

/// Animation cadence for [`Spinner`] in milliseconds (`loader.ts:42`).
pub use crate::components::loader::SPINNER_INTERVAL_MS as SpinnerIntervalMs;

/// The cursor that walks [`SpinnerFrames`]. Mirrors the upstream
/// `CancellableLoader` state machine enough that callers can drive a
/// spinner from a render tick (`loader.ts:67-91`).
pub use crate::components::loader::Spinner as Spinner;

// ─── components/markdown ────────────────────────────────────────────

/// Upstream `Markdown` — re-exported from `crate::components`. The
/// Rust port surfaces `crate::components::markdown::render_markdown` as
/// the renderer; the component struct lives in the same module but the
/// `MarkdownComponent` alias is currently a downstream concern.
pub use crate::components::markdown::render_markdown as Markdown;
pub use crate::components::markdown::render_markdown_with_links as MarkdownOptions;

/// Upstream `DefaultTextStyle` — alias for `SpanStyle`, the existing
/// styled-span slot type.
pub type DefaultTextStyle = crate::utils::styled::SpanStyle;

/// Upstream `MarkdownTheme` — markdown rendering theme. Placeholder.
#[derive(Debug, Clone, Default)]
pub struct MarkdownTheme {}

// ─── components/mouse-region ────────────────────────────────────────

/// Upstream `MouseRegionHandler` — callback signature for mouse
/// hits inside a `MouseRegion`. Stub.
pub trait MouseRegionHandler {
    /// A mouse hit at `(x, y)`. Default is a no-op.
    fn handle(&mut self, x: u16, y: u16) {
        let _ = (x, y);
    }
}

// ─── components/scroll-view ─────────────────────────────────────────

pub use crate::components::{
    ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions,
};

// ─── components/select-list ─────────────────────────────────────────

pub use crate::components::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext, DEFAULT_PRIMARY_COLUMN_WIDTH, MIN_DESCRIPTION_WIDTH,
    PRIMARY_COLUMN_GAP,
};

// ─── components/settings-list ──────────────────────────────────────

pub use crate::components::SettingsListComponent as SettingsList;

/// Upstream `SettingsListTheme` — theming for the settings list.
#[derive(Debug, Clone, Default)]
pub struct SettingsListTheme {}

// ─── components/spacer ──────────────────────────────────────────────

pub use crate::components::Spacer;

// ─── components/text ────────────────────────────────────────────────

pub use crate::components::Text;

// ─── components/truncated-text ──────────────────────────────────────

/// Upstream `TruncatedText` — text that truncates to fit its width.
pub use crate::components::TruncatedText;

// ─── components/v-stack ─────────────────────────────────────────────

/// Upstream `StackOptions` — options for `VStack` / `HStack`.
pub use crate::components::StackOptions;

/// Upstream `StackEntryOptions` — per-entry layout hints.
pub use crate::components::StackEntryOptions;

/// Upstream `StackEntry` — a single entry inside a stack.
pub use crate::components::StackLayoutEntry as StackEntry;

/// Upstream `StackChild` — a child of a stack.
pub use crate::components::StackLayoutEntry as StackChild;

/// Layout algorithm primitives (`allocateStackSizes`,
/// `visibleStackEntries`).
pub use crate::components::{
    allocate_stack_sizes as allocateStackSizes, visible_stack_entries as visibleStackEntries,
    StackAlign, StackBasis, StackLayoutViewport,
};

pub use crate::components::VStack;

// ─── editor-component ──────────────────────────────────────────────

/// Upstream `EditorComponent` — the editor component contract (see
/// `packages/tui/src/editor-component.ts:11-74`). Lets extensions provide
/// their own editor implementation (vim mode, emacs mode, custom
/// keybindings) while remaining compatible with the core application.
///
/// The trait mirrors upstream's interface: core text accessors and input
/// handling are required; the history / advanced / autocomplete / appearance
/// methods are optional with no-op defaults, matching upstream's
/// `?`-suffixed members.
pub trait EditorComponent: Component {
    /// Current text content. Required — `getText(): string` upstream.
    fn get_text(&self) -> String;

    /// Replace the text content. Required — `setText(text: string): void`.
    fn set_text(&mut self, text: String);

    /// Raw terminal input: key presses, paste sequences, etc. Required —
    /// `handleInput(data: string): void`.
    fn handle_input_data(&mut self, data: &str);

    /// Add text to the prompt history so `Up` / `Down` (or dedicated
    /// `historyPrevious` / `historyNext` chords) recall it.
    /// Optional — upstream `addToHistory?(text)`.
    fn add_to_history(&mut self, text: &str) {
        let _ = text;
    }

    /// Insert text at the current cursor position. Optional — upstream
    /// `insertTextAtCursor?(text)`.
    fn insert_text_at_cursor(&mut self, text: &str) {
        let _ = text;
    }

    /// Get the text with any markers (paste, image chips) expanded to their
    /// concrete payload. Optional — upstream `getExpandedText?()`. The
    /// default returns whatever `get_text()` returns.
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    /// Install an autocomplete provider. Optional — upstream
    /// `setAutocompleteProvider?(provider)`.
    fn set_autocomplete_provider(
        &mut self,
        provider: Option<std::sync::Arc<dyn crate::components::autocomplete::AutocompleteProvider>>,
    ) {
        let _ = provider;
    }

    /// Style the border string. Optional — upstream `borderColor?`. The
    /// default is the identity function: return the text unchanged.
    fn border_color(&self, text: &str) -> String {
        text.to_string()
    }

    /// Horizontal padding inside the editor's frame. Optional — upstream
    /// `setPaddingX?(padding)`.
    fn set_padding_x(&mut self, padding: u16) {
        let _ = padding;
    }

    /// Maximum number of autocomplete items visible in the dropdown.
    /// Optional — upstream `setAutocompleteMaxVisible?(maxVisible)`.
    fn set_autocomplete_max_visible(&mut self, max_visible: u16) {
        let _ = max_visible;
    }
}

// ─── layout / layout-node ──────────────────────────────────────────
//
// Upstream's `packages/tui/src/layout.ts` and `packages/tui/src/layout-node.ts`
// aren't re-exported from `index.ts`, but plugins implementing
// `LayoutComponent` need the symbol and the data shapes. The Rust port
// exposes them through `crate::layout_node` directly.

pub use crate::app::layout::{
    get_layout_boxes_at as getLayoutBoxesAt, get_scroll_view_box as getScrollViewBox,
    get_scroll_views_at as getScrollViewsAt, get_scrollbar_geometry as getScrollbarGeometry,
    render_layout_frame as renderLayoutFrame, LayoutBox as LayoutBoxTs, LayoutFrame as LayoutFrameTs,
    LayoutRect as LayoutRectTs,
};
pub use crate::app::layout_node::{
    get_layout_node as getLayoutNode, LayoutComponent, LayoutNode, LayoutNode as LayoutNodeTs,
    LayoutViewport, ScrollLayoutNode, ScrollLayoutState, ScrollOverscroll, StackKind, StackLayoutNode,
    LAYOUT_NODE,
};

// ─── keybindings ───────────────────────────────────────────────────
//
// `KeybindingDefinition`, `KeybindingsConfig`, `KeybindingsManager`
// are already exported. The four names below are new.

/// Upstream `Keybinding` — alias for `KeybindingDefinition` (the
/// per-action record). The two names differ in upstream but describe
/// the same shape; we pick the Rust-canonical one as the canonical
/// name and re-export the TS spelling as an alias.
pub use crate::components::keybindings::KeybindingDefinition as Keybinding;

/// Upstream `KeybindingDefinitions` — a list of keybinding records.
pub type KeybindingDefinitions = Vec<KeybindingDefinition>;

/// Upstream `Keybindings` — alias for the internal
/// `TuiKeybindingDefinitions` slice alias. The two are spelled
/// `Keybindings` / `KeybindingsManager` upstream; we keep both names
/// distinct in Rust (the manager is the runtime object, this alias
/// points at the static definition slice).
pub use crate::components::keybindings::TuiKeybindingDefinitions as Keybindings;

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
/// `crate::components::latex::render_latex`.
pub use crate::components::latex::render_latex;

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

/// Upstream `StdinBuffer` — a buffered stdin reader.
pub use crate::terminal::stdin_buffer::StdinBuffer;

/// Upstream `StdinBufferEventMap` — event names for `StdinBuffer`.
pub use crate::terminal::stdin_buffer::StdinBufferEventMap;

/// Upstream `StdinBufferOptions` — options for `StdinBuffer`.
pub use crate::terminal::stdin_buffer::StdinBufferOptions;

// ─── terminal-colors ───────────────────────────────────────────────

pub use crate::terminal::colors::{
    is_osc11_background_color_response, parse_osc11_background_color, parse_terminal_color_scheme_report,
    RgbColor, TerminalColorSchemeReport as TerminalColorScheme,
};

// ─── tui ────────────────────────────────────────────────────────────
//
// `Component` and `OverlayAnchor` already exist. The remaining ~20
// names are new stubs that mirror the upstream `TUI` interface.

// (No re-export needed: `Component` and `OverlayAnchor` are already
// re-exported from `crate::component` at the crate root.)

/// Upstream `Container` — a `Component` that contains other
/// components. Stub trait (defined later in this module).

/// Upstream `CURSOR_MARKER` — the zero-width-space marker used to
/// mark a hidden cursor cell in a rendered line. Mirrors
/// `packages/tui/src/tui.ts` `'​'`.
pub const CURSOR_MARKER: char = '\u{200B}';

/// Upstream `compositeTuiLine` — merge a list of styled lines into a
/// single composite line. Stub returning an empty line.
pub fn composite_tui_line(_parts: &[crate::utils::styled::StyledLine]) -> crate::utils::styled::StyledLine {
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

// (Component, CustomHandle → OverlayHandle, and OverlayAnchor are
// already re-exported at the top of this module.)

/// Upstream `Container` — a `Component` that contains other
/// components. Stub trait.
pub trait Container: Component {}

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
/// event. Alias for `crate::core::input_parse::MouseButton` (an enum).
pub use crate::core::input_parse::MouseButton as TuiMouseButton;

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
/// `crate::core::input_parse::MouseGestureKind` (the existing gesture enum).
pub use crate::core::input_parse::MouseGestureKind as TuiMouseEventType;

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

/// Upstream `TuiAltScreen` — the alt-screen TUI driver.
pub use crate::core::tui_drivers::TuiAltScreen;

/// Upstream `TuiAltScreenOptions` — options for `TuiAltScreen`.
#[derive(Debug, Clone, Default)]
pub struct TuiAltScreenOptions {}

// ─── tui-main-screen ──────────────────────────────────────────────

/// Upstream `TuiMainScreen` — the main-screen (non-alt) TUI driver.
pub use crate::core::tui_drivers::TuiMainScreen;

/// Upstream `TuiMainScreenRenderState` — render state for
/// `TuiMainScreen`.
#[derive(Debug, Clone, Default)]
pub struct TuiMainScreenRenderState {
    /// Number of lines in the last rendered frame.
    pub last_frame_lines: usize,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_8bit_rgb_parses() {
        let c = parse_osc11_background_color("rgb:ff/80/00").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn osc11_16bit_rgb_parses() {
        let c = parse_osc11_background_color("rgb:ffff/8080/0000").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn osc11_strips_framing() {
        let c = parse_osc11_background_color("\x1b]11;rgb:12/34/56\x07").unwrap();
        assert_eq!(c, RgbColor { r: 0x12, g: 0x34, b: 0x56 });
    }

    #[test]
    fn osc11_rejects_garbage() {
        assert!(parse_osc11_background_color("nope").is_none());
        assert!(parse_osc11_background_color("rgb:notanumber").is_none());
    }

    #[test]
    fn osc10_dark_scheme() {
        let s = parse_terminal_color_scheme_report("\x1b]10;?1\x07").unwrap();
        assert_eq!(s, TerminalColorScheme::Dark);
    }

    #[test]
    fn osc12_light_scheme() {
        let s = parse_terminal_color_scheme_report("12;?2").unwrap();
        assert_eq!(s, TerminalColorScheme::Light);
    }

    #[test]
    fn osc_unknown_scheme_carries_payload() {
        let s = parse_terminal_color_scheme_report("\x1b]12;?5\x07").unwrap();
        assert_eq!(s, TerminalColorScheme::Unknown("?5".to_string()));
    }
}

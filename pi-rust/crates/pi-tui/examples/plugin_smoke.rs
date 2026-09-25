//! Plugin authoring smoke test for `pi-tui`.
//!
//! This example exists to prove that every name exported from
//! `packages/tui/src/index.ts` is reachable through `pi_tui::*`. If the
//! pi-rust TUI is missing an export the build fails here, which makes
//! the parity gap immediately visible.
//!
//! It is a compile-only check (`cargo check -p pi-tui --example
//! plugin_smoke`). It does not run anything interactive.
//!
//! The file deliberately uses every symbol so any rename / removal in
//! `pi_tui::ts_compat` shows up here.

#![allow(unused_imports)]
#![allow(dead_code)]

use pi_tui::ts_compat::{
    // marked re-exports (token alias + trait)
    Marked as TuiMarked, Tokens, Token,
    // components/box
    Box as TuiBox,
    // components/cancellable-loader
    CancellableLoader as TuiCancellableLoader,
    // components/editor
    EditorOptions, EditorTheme,
    // components/h-stack
    HStack as TuiHStack,
    // components/image
    Image as TuiImage, ImageOptions as TuiImageOptions, ImageTheme as TuiImageTheme,
    // components/input
    Input as TuiInput, JUMP_DIRECTION, JumpDirection as TuiJumpDirection,
    // components/loader
    Loader as TuiLoader, LoaderIndicatorOptions as TuiLoaderIndicatorOptions,
    // components/markdown
    DefaultTextStyle, Markdown as TuiMarkdown, MarkdownOptions as TuiMarkdownOptions,
    MarkdownTheme as TuiMarkdownTheme,
    // components/mouse-region
    MouseRegion as TuiMouseRegion, MouseRegionHandler as TuiMouseRegionHandler,
    // components/scroll-view
    ScrollView as TuiScrollView, ScrollViewOptions as TuiScrollViewOptions,
    ScrollViewScrollbar as TuiScrollViewScrollbar,
    ScrollViewScrollToOptions as TuiScrollViewScrollToOptions,
    // components/select-list
    SelectItem as TuiSelectItem, SelectList as TuiSelectList,
    SelectListLayoutOptions as TuiSelectListLayoutOptions,
    SelectListTheme as TuiSelectListTheme,
    SelectListTruncatePrimaryContext as TuiSelectListTruncatePrimaryContext,
    // components/settings-list
    SettingItem as TuiSettingItem, SettingsList as TuiSettingsList,
    SettingsListTheme as TuiSettingsListTheme,
    // components/spacer
    Spacer as TuiSpacer,
    // components/text
    Text as TuiText,
    // components/truncated-text
    TruncatedText as TuiTruncatedText,
    // components/v-stack
    StackChild as TuiStackChild, StackEntry as TuiStackEntry,
    StackEntryOptions as TuiStackEntryOptions, StackOptions as TuiStackOptions,
    VStack as TuiVStack,
    // editor-component
    EditorComponent as TuiEditorComponent,
    // fuzzy
    FuzzyMatch as TuiFuzzyMatch, fuzzy_filter as TuiFuzzyFilter,
    fuzzy_match as TuiFuzzyMatchFn,
    // keybindings
    Keybinding as TuiKeybinding, KeybindingConflict as TuiKeybindingConflict,
    KeybindingDefinition as TuiKeybindingDefinition,
    KeybindingDefinitions as TuiKeybindingDefinitions,
    Keybindings as TuiKeybindings, KeybindingsConfig as TuiKeybindingsConfig,
    KeybindingsManager as TuiKeybindingsManager,
    get_keybindings as TuiGetKeybindings,
    set_keybindings as TuiSetKeybindings,
    TUI_KEYBINDINGS as TuiTuiKeybindings,
    // keys
    decode_kitty_printable as TuiDecodeKittyPrintable,
    is_key_release as TuiIsKeyRelease, is_key_repeat as TuiIsKeyRepeat,
    is_kitty_protocol_active as TuiIsKittyProtocolActive,
    matches_key as TuiMatchesKey, parse_key as TuiParseKey,
    set_kitty_protocol_active as TuiSetKittyProtocolActive,
    KeyEventType as TuiKeyEventType, KeyId as TuiKeyId,
    // latex
    render_latex as TuiRenderLatex, RenderLatexOptions as TuiRenderLatexOptions,
    // native-platform
    NativeClipboard as TuiNativeClipboard,
    get_native_clipboard as TuiGetNativeClipboard,
    // stdin-buffer
    StdinBuffer as TuiStdinBuffer, StdinBufferEventMap as TuiStdinBufferEventMap,
    StdinBufferOptions as TuiStdinBufferOptions,
    // terminal
    ProcessTerminal as TuiProcessTerminal, Terminal as TuiTerminal,
    // terminal-colors
    parse_osc11_background_color as TuiParseOsc11BackgroundColor,
    parse_terminal_color_scheme_report as TuiParseTerminalColorSchemeReport,
    RgbColor as TuiRgbColor, TerminalColorScheme as TuiTerminalColorScheme,
    // terminal-image
    allocate_image_id as TuiAllocateImageId,
    calculate_image_rows as TuiCalculateImageRows,
    delete_all_kitty_images as TuiDeleteAllKittyImages,
    delete_kitty_image as TuiDeleteKittyImage,
    detect_capabilities as TuiDetectCapabilities,
    encode_iterm2 as TuiEncodeITerm2,
    encode_kitty as TuiEncodeKitty,
    get_capabilities as TuiGetCapabilities,
    get_cell_dimensions as TuiGetCellDimensions,
    get_gif_dimensions as TuiGetGifDimensions,
    get_image_dimensions as TuiGetImageDimensions,
    get_jpeg_dimensions as TuiGetJpegDimensions,
    get_png_dimensions as TuiGetPngDimensions,
    get_webp_dimensions as TuiGetWebpDimensions,
    hyperlink as TuiHyperlink,
    CellDimensions as TuiCellDimensions,
    ImageDimensions as TuiImageDimensions,
    ImageProtocol as TuiImageProtocol,
    ImageRenderOptions as TuiImageRenderOptions,
    image_fallback as TuiImageFallback,
    render_image as TuiRenderImage,
    reset_capabilities_cache as TuiResetCapabilitiesCache,
    set_capabilities as TuiSetCapabilities,
    set_capability_overrides as TuiSetCapabilityOverrides,
    set_cell_dimensions as TuiSetCellDimensions,
    TerminalCapabilities as TuiTerminalCapabilities,
    // tui
    Component as TuiComponent, Container as TuiContainer,
    CURSOR_MARKER as TuiCursorMarker,
    composite_tui_line as TuiCompositeTuiLine,
    Focusable as TuiFocusable,
    is_focusable as TuiIsFocusable,
    is_viewport_tui as TuiIsViewportTui,
    OverlayAnchor as TuiOverlayAnchor,
    OverlayBounds as TuiOverlayBounds,
    OverlayHandle as TuiOverlayHandle,
    OverlayMargin as TuiOverlayMargin,
    OverlayOptions as TuiOverlayOptions,
    OverlayUnfocusOptions as TuiOverlayUnfocusOptions,
    SizeValue as TuiSizeValue, TUI as TuiTUI,
    TuiInputListener as TuiTuiInputListener,
    TuiInputListenerResult as TuiTuiInputListenerResult,
    TuiMode as TuiTuiMode,
    TuiMouseButton as TuiTuiMouseButton,
    TuiMouseEvent as TuiTuiMouseEvent,
    TuiMouseEventResult as TuiTuiMouseEventResult,
    TuiMouseEventType as TuiTuiMouseEventType,
    TuiStopOptions as TuiTuiStopOptions,
    ViewportTUI as TuiViewportTUI,
    // tui-alt-screen / tui-main-screen
    TuiAltScreen as TuiTuiAltScreen,
    TuiAltScreenOptions as TuiTuiAltScreenOptions,
    TuiMainScreen as TuiTuiMainScreen,
    TuiMainScreenRenderState as TuiTuiMainScreenRenderState,
    // utils
    get_osc8_link_at_column as TuiGetOsc8LinkAtColumn,
    slice_by_column as TuiSliceByColumn,
    strip_terminal_sequences as TuiStripTerminalSequences,
    truncate_to_width as TuiTruncateToWidth,
    visible_width as TuiVisibleWidth,
    wrap_text_with_ansi as TuiWrapTextWithAnsi,
};

// ---------------------------------------------------------------------------
// Functional smoke: a no-op trait impl so we exercise every trait stub.
// ---------------------------------------------------------------------------

struct SampleComponent;

impl TuiComponent for SampleComponent {
    fn render(&self, _width: u16) -> Vec<pi_tui::StyledLine> {
        Vec::new()
    }
}

impl TuiContainer for SampleComponent {}
impl TuiCancellableLoader for SampleComponent {}
impl TuiMouseRegionHandler for SampleComponent {}
impl TuiFocusable for SampleComponent {}
impl TuiMarked for SampleComponent {
    fn render(&self, _src: &str) -> Vec<pi_tui::StyledLine> { Vec::new() }
}
impl TuiNativeClipboard for SampleComponent {}
impl TuiTuiInputListener for SampleComponent {}
impl TuiTuiMode for SampleComponent {}
impl TuiTUI for SampleComponent {}
impl TuiViewportTUI for SampleComponent {}

/// Show that an EditorComponent can be implemented for a Rust type.
struct SampleEditor;

impl TuiComponent for SampleEditor {
    fn render(&self, _width: u16) -> Vec<pi_tui::StyledLine> { Vec::new() }
}

impl TuiEditorComponent for SampleEditor {
    fn get_text(&self) -> String { String::new() }
    fn set_text(&mut self, _text: String) {}
    fn handle_input_data(&mut self, _data: &str) {}
}

/// Show that the fuzzy API has the expected shape.
fn _fuzzy_smoke() {
    let items: [&str; 3] = ["alpha", "beta", "gamma"];
    let matches = TuiFuzzyFilter(&items, "ab", |s| *s);
    let _: Vec<&&str> = matches;
    let _single = TuiFuzzyMatchFn("hl", "hello");
    let _: TuiFuzzyMatch = _single;
}

/// `Marked` returns `Vec<StyledLine>` (the existing renderer surface).
fn _marked_smoke(m: &dyn TuiMarked) -> Vec<pi_tui::StyledLine> {
    m.render("# hi")
}

/// Verify the autocomplete provider factory surface.
fn _autocomplete_smoke() -> pi_tui::AutocompleteSuggestions {
    let item = pi_tui::AutocompleteItem::new("hello", "world");
    pi_tui::AutocompleteSuggestions {
        items: vec![item],
        prefix: "h".to_string(),
    }
}

/// Verify keybindings can be queried by id.
fn _keybindings_smoke() -> TuiKeybindingDefinitions {
    let _ = TuiGetKeybindings();
    Vec::new()
}

fn main() {
    // Trivial: ensure all type aliases actually resolve at link time.
    let _: TuiProcessTerminal = TuiProcessTerminal::new().expect("terminal");
    let _: TuiStdinBuffer = TuiStdinBuffer::new(TuiStdinBufferOptions::tty());

    // Verify trait predicates work.
    let _ = TuiIsFocusable(&SampleComponent);
    let _ = TuiIsViewportTui(&SampleComponent);
    let _ = TuiCompositeTuiLine(&[]);
    let _ = TuiGetOsc8LinkAtColumn("", 0);
    let _ = TuiSliceByColumn("", 0, 1);
    let _ = TuiStripTerminalSequences("");
    let _ = TuiTruncateToWidth("hello", 3);
    let _ = TuiVisibleWidth("hello");
    let _ = TuiWrapTextWithAnsi("hello", 80);

    println!("pi-tui plugin authoring surface: every TS export compiles.");
}
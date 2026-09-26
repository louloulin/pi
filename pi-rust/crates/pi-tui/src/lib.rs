//! `pi-tui` — Terminal UI primitives for the Pi Rust port.
//!
//! Stage 4 ports `packages/tui` (editor, message view, prompt, selector,
//! status bar) onto `ratatui` + `crossterm`, plus an [`App`] struct that
//! drives the interactive event loop and pipes [`AgentEvent`]s from
//! `pi-agent-core` into the rendered message view.
//!
//! See `docs/ARCHITECTURE.md` for how the crate fits into the workspace.
//!
//! # Module map
//!
//! The crate is split into five top-level sub-modules, each owning one
//! layer of the framework. See the module docs for what each layer owns.
//!
//! | Layer | What lives here |
//! |-------|-----------------|
//! | [`core`] | Framework primitives — `Component` trait, focus, overlay, the TUI driver, input abstraction |
//! | [`components`] | Domain widgets — `Editor`, `Prompt`, `MessageView`, `Selector`, `Dialog`, `Markdown`, `Loader`, … |
//! | [`utils`] | Pure helpers — display width, styled spans, fuzzy match, syntax highlight, OSC 8 hyperlinks |
//! | [`terminal`] | Backend — `ProcessTerminal`, capabilities, image protocol, title, raw TTY, stdin buffer |
//! | [`app`] | Orchestration — `App` event loop, step dispatchers, viewport state, layout |

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod clipboard;
pub mod component;
pub mod components;
pub mod core;
pub mod locale;
pub mod narrow_terminal;
pub mod styles;
pub mod terminal;
pub mod theme;
pub mod tree;
pub mod ts_compat;
pub mod utils;

// Legacy module aliases — downstream code imports `pi_tui::keybindings`,
// `pi_tui::input`, etc. These re-export the modules under their old flat
// names so existing imports continue to resolve while the new modular
// paths (`pi_tui::components::keybindings`) remain the canonical home.
pub mod autocomplete {
    pub use crate::components::autocomplete::*;
}
pub mod dialog {
    pub use crate::components::dialog::*;
}
pub mod editor {
    pub use crate::components::editor::*;
}
pub mod extension_ui {
    pub use crate::components::extension_ui::*;
}
pub mod fuzzy {
    pub use crate::utils::fuzzy::*;
}
pub mod highlight {
    pub use crate::utils::highlight::*;
}
pub mod history_store {
    pub use crate::app::history_store::*;
}
pub mod hyperlink {
    pub use crate::utils::hyperlink::*;
}
pub mod image {
    pub use crate::components::image::*;
}
pub mod input {
    pub use crate::core::input_parse::*;
}
pub mod keybindings {
    pub use crate::components::keybindings::*;
}
pub mod kill_ring {
    pub use crate::components::kill_ring::*;
}
pub mod latex {
    pub use crate::components::latex::*;
}
pub mod loader {
    pub use crate::components::loader::*;
}
pub mod markdown {
    pub use crate::components::markdown::*;
}
pub mod message {
    pub use crate::components::message::*;
}
pub mod mouse_region {
    pub use crate::components::mouse_region::*;
}
pub mod prompt {
    pub use crate::components::prompt::*;
}
pub mod search {
    pub use crate::components::search::*;
}
pub mod selector {
    pub use crate::components::selector::*;
}
pub mod settings {
    pub use crate::components::settings::*;
}
pub mod slash_menu {
    pub use crate::components::slash_menu::*;
}
pub mod status {
    pub use crate::components::status::*;
}
pub mod styled {
    pub use crate::utils::styled::*;
}
pub mod terminal_image {
    pub use crate::terminal::image::*;
}
pub mod terminal_title {
    pub use crate::terminal::title::*;
}
pub mod undo_stack {
    pub use crate::components::undo_stack::*;
}
pub mod viewport {
    pub use crate::app::viewport::*;
}
pub mod width {
    pub use crate::utils::width::*;
}
pub mod word_navigation {
    pub use crate::utils::word_navigation::*;
}
pub mod keys {
    pub use crate::core::keys::*;
}
pub mod frame_pacer {
    pub use crate::terminal::frame_pacer::*;
}
pub mod high_dpi {
    pub use crate::terminal::high_dpi::*;
}
pub mod stdin_buffer {
    pub use crate::terminal::stdin_buffer::*;
}
pub mod layout {
    pub use crate::app::layout::*;
}
pub mod layout_node {
    pub use crate::app::layout_node::*;
}


// -----------------------------------------------------------------------------
// Legacy flat re-exports
// -----------------------------------------------------------------------------
// The crate used to expose everything at the top level. Downstream crates
// (notably `pi-coding-agent`) still import from the flat surface, so each
// public item below is re-exported from its new modular home. New code should
// prefer the modular paths; these re-exports exist purely for migration.

pub use crate::app::{App, AppConfig, FollowUpOutcome, RenderSnapshot};
pub use crate::app::viewport::ScrollbarGeometry;
pub use crate::app::history_store::{append, default_path, load, rewrite, HistoryStore};
pub use crate::components::autocomplete::{
    compose_autocomplete_providers, ArgumentCompletions, AutocompleteItem, AutocompleteProvider,
    AutocompleteProviderFactory, AutocompleteSuggestions, CombinedAutocompleteProvider,
    CompletionResult, SlashCommand, TriggeredAutocompleteProvider,
};
pub use crate::clipboard::{base64_encode, osc52_sequence};
pub use crate::component::{
    Component, CustomHandle, CustomOptions, OverlayAnchor, TextComponent, WidgetPlacement,
};
pub use crate::components::dialog::{Dialog, DialogAction, DialogKind};
pub use crate::components::editor::{
    is_bash_mode, parse_bash_command, BashCommand, Editor, EditorAction, HistoryEntry,
    HistorySearch, HistorySearchDirection, HistorySearchStatus, JumpDirection,
};
pub use crate::components::extension_ui::ExtensionUi;
pub use crate::utils::fuzzy::{fuzzy_filter, fuzzy_match, fuzzy_match_all, fuzzy_rank, FuzzyMatch};
pub use crate::utils::highlight::{
    get_language_from_path, highlight_code, supports_language, tokenize, Token, TokenKind,
};
pub use crate::utils::hyperlink::{
    close_hyperlink, hyperlink, open_hyperlink, visible_width,
};
pub use crate::components::image::{truncate_to_width, Image, ImageOptions, ImageTheme};
pub use crate::core::input_parse::{
    is_mouse_sequence, parse_mouse_sequence, InputEvent, Key, KeyModifiers, MouseButton,
    MouseGesture, MouseGestureKind,
};
pub use crate::components::keybindings::{
    get_keybindings, key_matches, parse_key_id, reset_keybindings, set_keybindings,
    tui_default_keybindings, KeybindingConflict, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager,
};
pub use crate::components::kill_ring::{KillDirection, KillRing};
pub use crate::components::loader::{
    format_elapsed, indicator_line, Spinner, SPINNER_FRAMES, SPINNER_INTERVAL_MS,
};
pub use crate::locale::{Locale, STARTUP_HINTS};
pub use crate::narrow_terminal::{
    is_extreme_narrow, is_narrow, narrow_options, NarrowOptions, EXTREME_NARROW_WIDTH,
    HEADER_HINT_COLLAPSE_THRESHOLD, NARROW_TERMINAL_WIDTH,
};
pub use crate::components::markdown::{
    render_markdown, render_markdown_with_links, render_markdown_with_theme,
    render_markdown_with_transform, MarkdownTransform, TransformError,
};
pub use crate::components::message::{
    stop_reason_tail_line, tool_fold_hint, MessageItem, MessageView, PendingMessageKind, Role,
    ToolBlock, ToolBlockRenderer, TOOL_PREVIEW_LINES,
};
pub use crate::components::mouse_region::{MouseRegion, MouseRegionPoint};
pub use crate::components::prompt::{Prompt, PromptAction};
pub use crate::components::search::{
    apply_query_key, find_matches, normalize_query, render_search_bar, search_bar_rect,
    search_bar_text, SearchBar, SearchBarLayout, SearchIndex, SearchMatch, SearchResult,
    SearchSegment, SearchSelectionMode,
};
pub use crate::components::selector::{Selector, SelectorAction, SelectorItem, SelectorLayout};
pub use crate::components::settings::{SettingItem, SettingsAction, SettingsList};
pub use crate::components::slash_menu::{SlashMenu, SlashMenuEntry, SlashMenuWidget};
pub use crate::components::status::{format_cost, format_tokens, BusyIndicator, StatusBar, StatusData, StatusPricing};
pub use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};
pub use crate::styles::SelectListStyles;
pub use crate::terminal::image::{
    allocate_image_id, apply_env_overrides, calculate_image_cell_size, calculate_image_rows,
    capability_inputs_from_env, crop_kitty_image_line, decoded_base64_len, delete_all_kitty_images,
    delete_all_kitty_placements, delete_kitty_image, detect_capabilities_from_env,
    detect_capabilities_with, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_kitty_image_metadata,
    get_kitty_image_placement, get_png_dimensions, get_webp_dimensions, image_fallback,
    is_image_line, register_kitty_image_metadata, render_image, reset_capabilities_cache,
    set_capabilities, set_capability_overrides, set_cell_dimensions, shorten_image_path,
    CapabilityInputs, CapabilityOverrides, CellDimensions, ImageCellSize, ImageDimensions,
    ImageProtocol, ImageRenderOptions, Iterm2EncodeOptions, KittyEncodeOptions, KittyImageMetadata,
    KittyImagePlacement, Override, RenderImageResult, TerminalCapabilities, ITERM2_PREFIX,
    KITTY_CHUNK_SIZE, KITTY_PREFIX,
};
pub use crate::terminal::high_dpi::{
    detect_device_pixel_ratio_from_env, detect_device_pixel_ratio_with, device_pixel_ratio,
    refresh_device_pixel_ratio, set_device_pixel_ratio, DevicePixelRatio, DEFAULT_DPI_RATIO,
    HIGH_DPI_RATIO,
};
pub use crate::terminal::title::{
    auto_title, path_basename, sanitize_title, title_sequence, TITLE_CLOSE, TITLE_OPEN,
};
pub use crate::terminal::process::{ProcessTerminal, Terminal, TerminalError};
pub use crate::theme::{
    available_themes, builtin_theme, builtin_theme_names, default_custom_themes_dir,
    default_theme_name, is_light_theme, load_theme, load_theme_from_path, parse_auto_theme_setting,
    resolve_theme_setting, ColorMode, ColorValue, TerminalTheme, Theme, ThemeBg, ThemeColor,
    ThemeController, ThemeError, ThemeJson,
};
pub use crate::tree::{flatten_tree, tree_selector_items, TreeItem, TreeRow};
pub use crate::ts_compat::{
    composite_tui_line, get_native_clipboard, is_focusable, is_viewport_tui, parse_osc11_background_color,
    parse_terminal_color_scheme_report, render_latex, Box, CancellableLoader, Container, CURSOR_MARKER,
    DefaultTextStyle, EditorComponent, EditorOptions, EditorTheme, Focusable, HStack, Input,
    JUMP_DIRECTION, Keybinding, KeybindingDefinitions, Keybindings, Loader,
    LoaderIndicatorOptions, Markdown, MarkdownOptions, MarkdownTheme, Marked, MouseRegionHandler,
    NativeClipboard, OverlayBounds, OverlayHandle, OverlayMargin, OverlayOptions, OverlayUnfocusOptions,
    RenderLatexOptions, RgbColor,
    ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions,
    SelectListTheme, SelectListTruncatePrimaryContext, SettingsListTheme, SizeValue, Spacer, StackChild,
    StackEntry, StackEntryOptions, StackOptions, StdinBuffer, StdinBufferEventMap, StdinBufferOptions,
    TerminalColorScheme, Text, Tokens, TruncatedText, TUI, TUI_KEYBINDINGS, TuiAltScreen,
    TuiAltScreenOptions, TuiInputListener, TuiInputListenerResult, TuiMainScreen, TuiMainScreenRenderState,
    TuiMode, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, TuiStopOptions,
    ViewportTUI, VStack,
};
pub use crate::components::undo_stack::UndoStack;
pub use crate::utils::width::{char_columns, columns, prefix_columns, truncate_columns};
pub use crate::utils::word_navigation::{find_word_backward, find_word_forward};
pub use crate::core::keys::{
    decode_kitty_printable, is_key_release, is_key_repeat, is_kitty_protocol_active, matches_key, parse_key,
    set_kitty_protocol_active, KeyEventType, KeyId,
};
pub use crate::utils::{
    get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, wrap_text_with_ansi,
};

/// Re-export of the underlying terminal backend so binaries can pin a
/// single version of `crossterm`.
pub use crossterm as backend;
/// Re-export of the underlying widget library.
pub use ratatui as widgets;

/// Build info that the binary prints at startup.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
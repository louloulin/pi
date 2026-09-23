//! `pi-tui` — Terminal UI primitives for the Pi Rust port.
//!
//! Stage 4 ports `packages/tui` (editor, message view, prompt, selector,
//! status bar) onto `ratatui` + `crossterm`, plus an [`App`] struct that
//! drives the interactive event loop and pipes [`AgentEvent`]s from
//! `pi-agent-core` into the rendered message view.
//!
//! See `docs/ARCHITECTURE.md` for how the crate fits into the workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod autocomplete;
pub mod clipboard;
pub mod component;
pub mod dialog;
pub mod editor;
pub mod extension_ui;
pub mod fuzzy;
pub mod highlight;
pub mod history_store;
pub mod hyperlink;
pub mod image;
pub mod input;
pub mod keybindings;
pub mod kill_ring;
pub mod latex;
pub mod loader;
pub mod locale;
pub mod markdown;
pub mod message;
pub mod mouse_region;
pub mod prompt;
pub mod search;
pub mod selector;
pub mod settings;
pub mod status;
pub mod styled;
pub mod styles;
pub mod terminal_image;
pub mod terminal_title;
pub mod theme;
pub mod tree;
pub mod undo_stack;
pub(crate) mod visual_text;
pub mod width;
pub mod word_navigation;

pub use app::{App, AppConfig, FollowUpOutcome, RenderSnapshot, ScrollbarGeometry};
pub use autocomplete::{
    compose_autocomplete_providers, ArgumentCompletions, AutocompleteItem, AutocompleteProvider,
    AutocompleteProviderFactory, AutocompleteSuggestions, CombinedAutocompleteProvider,
    CompletionResult, SlashCommand, TriggeredAutocompleteProvider,
};
pub use clipboard::{base64_encode, osc52_sequence};
pub use component::{
    Component, CustomHandle, CustomOptions, OverlayAnchor, TextComponent, WidgetPlacement,
};
pub use dialog::{Dialog, DialogAction, DialogKind};
pub use editor::{
    is_bash_mode, parse_bash_command, BashCommand, Editor, EditorAction, HistoryEntry,
    HistorySearchDirection, HistorySearchStatus, JumpDirection,
};
pub use extension_ui::ExtensionUi;
pub use fuzzy::{fuzzy_filter, fuzzy_match, fuzzy_match_all, fuzzy_rank, FuzzyMatch};
pub use highlight::{
    get_language_from_path, highlight_code, supports_language, tokenize, Token, TokenKind,
};
pub use hyperlink::{close_hyperlink, hyperlink, open_hyperlink, visible_width};
pub use image::{truncate_to_width, Image, ImageOptions, ImageTheme};
pub use input::{
    is_mouse_sequence, parse_mouse_sequence, InputEvent, Key, KeyModifiers, MouseButton,
    MouseGesture, MouseGestureKind,
};
pub use keybindings::{
    get_keybindings, key_matches, parse_key_id, reset_keybindings, set_keybindings,
    tui_default_keybindings, KeybindingConflict, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager,
};
pub use kill_ring::{KillDirection, KillRing};
pub use loader::{format_elapsed, indicator_line, Spinner, SPINNER_FRAMES, SPINNER_INTERVAL_MS};
pub use locale::{Locale, STARTUP_HINTS};
pub use markdown::{render_markdown, render_markdown_with_links, render_markdown_with_theme};
pub use message::{
    tool_fold_hint, MessageItem, MessageView, PendingMessageKind, Role, ToolBlock,
    ToolBlockRenderer, TOOL_PREVIEW_LINES,
};
pub use mouse_region::{MouseRegion, MouseRegionPoint};
pub use prompt::{Prompt, PromptAction};
pub use search::{
    apply_query_key, find_matches, normalize_query, render_search_bar, search_bar_rect,
    search_bar_text, SearchBar, SearchBarLayout, SearchIndex, SearchMatch, SearchResult,
    SearchSegment, SearchSelectionMode,
};
pub use selector::{Selector, SelectorAction, SelectorItem, SelectorLayout};
pub use settings::{SettingItem, SettingsAction, SettingsList};
pub use status::{format_cost, format_tokens, BusyIndicator, StatusBar, StatusData, StatusPricing};
pub use styled::{SpanStyle, StyledLine, StyledSpan};
pub use styles::SelectListStyles;
pub use terminal_image::{
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
pub use terminal_title::{
    auto_title, path_basename, sanitize_title, title_sequence, TITLE_CLOSE, TITLE_OPEN,
};
pub use theme::{
    available_themes, builtin_theme, builtin_theme_names, default_custom_themes_dir,
    default_theme_name, is_light_theme, load_theme, load_theme_from_path, parse_auto_theme_setting,
    resolve_theme_setting, ColorMode, ColorValue, TerminalTheme, Theme, ThemeBg, ThemeColor,
    ThemeController, ThemeError, ThemeJson,
};
pub use tree::{flatten_tree, tree_selector_items, TreeItem, TreeRow};
pub use undo_stack::UndoStack;
pub use width::{char_columns, columns, prefix_columns, truncate_columns};
pub use word_navigation::{find_word_backward, find_word_forward};

/// Re-export of the underlying terminal backend so binaries can pin a
/// single version of `crossterm`.
pub use crossterm as backend;
/// Re-export of the underlying widget library.
pub use ratatui as widgets;

/// Build info that the binary prints at startup.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

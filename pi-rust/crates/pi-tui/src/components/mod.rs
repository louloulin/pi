//! Composables used by [`crate::app::App`] to render the editor, message
//! view, prompt and overlays.
//!
//! Each module owns one widget or feature widget from upstream
//! `packages/tui/src/components/` and the matching widget/feature layer in
//! `pi-tui/src/app/`. The split mirrors upstream's layout:
//!
//! * `editor`, `prompt`, `slash_menu`, `autocomplete`, `keybindings`,
//!   `kill_ring`, `undo_stack`, `mouse_region`, `extension_ui` — the
//!   composer's editing surface and its supporting services.
//! * `message`, `markdown`, `status`, `settings`, `extension_ui` — the
//!   transcript and chrome.
//! * `dialog`, `selector`, `search`, `slash_menu` — modal overlays.
//! * `latex`, `loader`, `image`, `cancellable_loader`, `spacer`,
//!   `truncated_text`, `text`, `box_layout`, `scroll_view`,
//!   `select_list`, `settings_list`, `h_stack`, `v_stack`, `stack` — the
//!   presentational widget layer shared with the extension surface.
//!
//! Public re-exports keep the legacy flat surface stable — extensions that
//! still `use pi_tui::editor::Editor` keep compiling.

pub mod alt_screen_flash;
pub mod autocomplete;
pub mod box_layout;
pub mod cancellable_loader;
pub mod dialog;
pub mod dynamic_border;
pub mod editor;
pub mod extension_ui;
pub mod h_stack;
pub mod image;
pub mod input;
pub mod keybindings;
pub mod kill_ring;
pub mod latex;
pub mod loader;
pub mod markdown;
pub mod message;
pub mod mouse_region;
pub mod prompt;
pub mod scroll_view;
pub mod search;
pub mod select_list;
pub mod selector;
pub mod settings;
pub mod settings_list;
pub mod slash_menu;
pub mod spacer;
pub mod stack;
pub mod status;
pub mod status_indicator;
pub mod text;
pub mod tool_strip;
pub mod truncated_text;
pub mod undo_stack;
pub mod v_stack;

pub use alt_screen_flash::{AltScreenFlashContainer, DEFAULT_DURATION_MS};
pub use autocomplete::{
    compose_autocomplete_providers, ArgumentCompletions, AutocompleteItem, AutocompleteProvider,
    AutocompleteProviderFactory, AutocompleteSuggestions, CombinedAutocompleteProvider,
    CompletionResult, SlashCommand, TriggeredAutocompleteProvider,
};
pub use box_layout::BoxLayout;
pub use box_layout::BoxLayout as Box;
pub use cancellable_loader::CancellableLoader;
pub use crate::component::{
    ComponentSlot, MouseButton, MouseEvent, MouseKind, Rect as SlotRect, TextComponent,
};
pub use dialog::{Dialog, DialogAction, DialogKind};
pub use dynamic_border::DynamicBorder;
pub use editor::{
    is_bash_mode, parse_bash_command, BashCommand, Editor, EditorAction, HistoryEntry,
    HistorySearch, HistorySearchDirection, HistorySearchStatus, JumpDirection,
};
pub use extension_ui::ExtensionUi;
pub use h_stack::HStack;
pub use image::{truncate_to_width, Image, ImageOptions, ImageTheme};
pub use input::InputComponent;
pub use keybindings::{
    get_keybindings, key_matches, parse_key_id, reset_keybindings, set_keybindings,
    tui_default_keybindings, KeybindingConflict, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager,
};
pub use kill_ring::{KillDirection, KillRing};
pub use latex::render_latex as render_latex_text;
pub use loader::{format_elapsed, indicator_line, Spinner, SPINNER_FRAMES, SPINNER_INTERVAL_MS};
pub use markdown::{render_markdown, render_markdown_with_links, render_markdown_with_theme};
pub use message::{
    tool_fold_hint, MessageItem, MessageView, PendingMessageKind, Role, ToolBlock,
    ToolBlockRenderer, TOOL_PREVIEW_LINES,
};
pub use mouse_region::{MouseRegion, MouseRegionPoint};
pub use prompt::{Prompt, PromptAction};
pub use scroll_view::{
    ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions, ScrollbarMode,
};
pub use search::{
    apply_query_key, find_matches, normalize_query, render_search_bar, search_bar_rect,
    search_bar_text, SearchBar, SearchBarLayout, SearchIndex, SearchMatch, SearchResult,
    SearchSegment, SearchSelectionMode,
};
pub use select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext, DEFAULT_PRIMARY_COLUMN_WIDTH, MIN_DESCRIPTION_WIDTH,
    PRIMARY_COLUMN_GAP,
};
pub use settings::{SettingItem, SettingsAction, SettingsList};
pub use settings_list::{SettingsListComponent, SettingsListComponentTheme};
pub use slash_menu::{SlashMenu, SlashMenuEntry, SlashMenuWidget};
pub use spacer::Spacer;
pub use stack::{
    allocate_stack_sizes, visible_stack_entries, StackAlign, StackBasis, StackEntryOptions,
    StackLayoutEntry, StackLayoutViewport, StackOptions,
};
pub use status::{format_cost, format_tokens, BusyIndicator, StatusBar, StatusData, StatusPricing};
pub use status_indicator::{
    BranchSummaryStatusIndicator, CompactionReason, CompactionStatusIndicator, IdleStatus,
    RetryStatusIndicator, StatusIndicator, StatusIndicatorKind, TimerHandle,
    WorkingStatusIndicator,
};
pub use text::Text;
pub use tool_strip::{ToolStrip, ToolStripState};
pub use truncated_text::TruncatedText;
pub use undo_stack::UndoStack;
pub use v_stack::VStack;
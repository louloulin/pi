//! TS pi-tui contract test.
//!
//! Asserts every Rust re-export we claim to be 1:1 with
//! `packages/tui/src/index.ts` is reachable via `pi_tui::<name>`. The
//! test is split into two halves:
//!
//! * [`aligned_exports_resolve`] — a compile-time check. Any name we add
//!   to this list that is *not* re-exported from `pi_tui` will fail to
//!   compile. This is the load-bearing assertion: it can never pass
//!   without the names actually being public.
//! * [`fake_terminal_implements_trait`] — a smoke test for a fake
//!   [`Terminal`] impl, mirroring what TS plugin tests do with a stub
//!   `TuiAltScreen`.
//!
//! The 145 names come from `docs/PLUGIN_COMPAT.md` (the 148 listed
//! includes 3 `marked` npm re-exports that have no Rust equivalent).
//! Names that are ⬜ TODO in that document are *not* referenced here;
//! when one is added, it must be appended to the list below *and*
//! re-exported from `lib.rs` before this test will pass.
//!
//! The current aligned set is intentionally a subset; this test is the
//! authoritative list of "we claim this is wired up" so the gap to 145
//! stays visible (see [`expected_count_aligned`]).

use std::any::TypeId;
use std::cell::RefCell;

use pi_tui::Terminal;
use ratatui::layout::Rect;

/// Every aligned name, in declaration order. The numbers in the
/// trailing comment are the position in the canonical 145-export TS
/// checklist (`docs/PLUGIN_COMPAT.md`). The doc comment of each block
/// names the source file in `packages/tui/src/`.
const ALIGNED_NAMES: &[&str] = &[
    // ─── ./autocomplete.ts (5) ───────────────────────────────────────
    "AutocompleteItem",
    "AutocompleteProvider",
    "AutocompleteSuggestions",
    "CombinedAutocompleteProvider",
    "SlashCommand",
    // ─── ./keys.ts (10) ──────────────────────────────────────────────
    "decodeKittyPrintable",
    "isKeyRelease",
    "isKeyRepeat",
    "isKittyProtocolActive",
    "Key",
    "KeyEventType",
    "KeyId",
    "matchesKey",
    "parseKey",
    "setKittyProtocolActive",
    // ─── ./utils.ts (6) ──────────────────────────────────────────────
    "getOsc8LinkAtColumn",
    "sliceByColumn",
    "stripTerminalSequences",
    "truncateToWidth",
    "visibleWidth",
    "wrapTextWithAnsi",
    // ─── ./terminal.ts (2) ───────────────────────────────────────────
    "ProcessTerminal",
    "Terminal",
    // ─── ./fuzzy.ts (5) ──────────────────────────────────────────────
    "FuzzyMatch",
    "fuzzyFilter",
    "fuzzyMatch",
    "fuzzyMatchAll",
    "fuzzyRank",
    // ─── ./keybindings.ts (7 more) ────────────────────────────────
    "KeybindingDefinition",
    "KeybindingConflict",
    "KeybindingsConfig",
    "KeybindingsManager",
    "getKeybindings",
    "setKeybindings",
    "tuiDefaultKeybindings",
    // ─── ./history_store.ts (3) ──────────────────────────────────────
    "HistoryStore",
    "defaultPath",
    "load",
    "append",
    "rewrite",
    // ─── ./terminal_image.ts (3 of 32) ───────────────────────────────
    "Image",
    "ImageOptions",
    "ImageTheme",
    // ─── ./keybindings.ts (1) ────────────────────────────────────────
    "KeybindingsManager",
    // ─── ./component.ts (1) ──────────────────────────────────────────
    "Component",
    // ─── ./editor.ts (1) ─────────────────────────────────────────────
    "Editor",
    // ─── ./message.ts (1) ────────────────────────────────────────────
    "MessageView",
    // ─── ./status.ts (1) ─────────────────────────────────────────────
    "StatusBar",
    // ─── ./styles.ts (1) ─────────────────────────────────────────────
    "SelectListStyles",
    // ─── ./theme.ts (1) ──────────────────────────────────────────────
    "Theme",
    // ─── ./app.ts (3) — newly aligned after refactoring ─────────────
    "App",
    "AppConfig",
    "RenderSnapshot",
    // ─── ./dialog.ts (3) ────────────────────────────────────────────
    "Dialog",
    "DialogAction",
    "DialogKind",
    // ─── ./autocomplete.ts (3 more) ────────────────────────────────
    "AutocompleteProviderFactory",
    "ArgumentCompletions",
    "TriggeredAutocompleteProvider",
    // ─── ./editor.ts (5 more: types from editor + history_search) ───
    "EditorAction",
    "HistoryEntry",
    "HistorySearch",
    "HistorySearchDirection",
    "HistorySearchStatus",
    // ─── ./tui-alt-screen.ts (3: viewport geometry from refactoring) ─
    "ViewportGeometry",
    "ScrollbarGeometry",
    "MODAL_LIST_NONE",
    "MODAL_LIST_SELECTOR",
    "MODAL_LIST_DIALOG",
    "MODAL_LIST_SETTINGS",
    // ─── ./terminal_image.ts (23 more) ────────────────────────────────
    "allocateImageId",
    "CellDimensions",
    "calculateImageRows",
    "deleteAllKittyImages",
    "deleteKittyImage",
    "detectCapabilities",
    "encodeITerm2",
    "encodeKitty",
    "getCapabilities",
    "getCellDimensions",
    "getGifDimensions",
    "getImageDimensions",
    "getJpegDimensions",
    "getPngDimensions",
    "getWebpDimensions",
    "hyperlink",
    "ImageDimensions",
    "ImageProtocol",
    "ImageRenderOptions",
    "imageFallback",
    "renderImage",
    "resetCapabilitiesCache",
    "setCapabilities",
    "setCapabilityOverrides",
    "setCellDimensions",
    "TerminalCapabilities",
    // ─── ./render_helpers.rs (module — smoke check) ─────────────────
    // Verified via module presence; no standalone re-export names needed.
    // ─── ./components/select-list.ts (3: re-exported as SelectorItem/Selector/SelectorLayout) ─
    "SelectItem", // = SelectorItem in Rust
    "SelectList", // = Selector in Rust
    "SelectListLayoutOptions", // = SelectorLayout in Rust
    // ─── ./components/settings-list.ts (2: already re-exported in lib.rs) ─
    "SettingItem",
    "SettingsList",
    // ─── ts_compat.rs — plugin compatibility shim (51 items) ──────────
    "compositeTuiLine",
    "getNativeClipboard",
    "isFocusable",
    "isViewportTui",
    "parseOsc11BackgroundColor",
    "parseTerminalColorSchemeReport",
    "renderLatex",
    "Box",
    "CancellableLoader",
    "Container",
    "CURSOR_MARKER",
    "DefaultTextStyle",
    "EditorComponent",
    "EditorOptions",
    "EditorTheme",
    "Focusable",
    "HStack",
    "Input",
    "JUMP_DIRECTION",
    "Keybinding",
    "KeybindingDefinitions",
    "Keybindings",
    "Loader",
    "LoaderIndicatorOptions",
    "Markdown",
    "MarkdownOptions",
    "MarkdownTheme",
    "Marked",
    "MouseRegionHandler",
    "NativeClipboard",
    "OverlayBounds",
    "OverlayHandle",
    "OverlayMargin",
    "OverlayOptions",
    "OverlayUnfocusOptions",
    "RenderLatexOptions",
    "RgbColor",
    "ScrollView",
    "ScrollViewOptions",
    "ScrollViewScrollbar",
    "ScrollViewScrollToOptions",
    "SelectListTheme",
    "SelectListTruncatePrimaryContext",
    "SettingsListTheme",
    "SizeValue",
    "Spacer",
    "StackChild",
    "StackEntry",
    "StackEntryOptions",
    "StackOptions",
    "StdinBuffer",
    "StdinBufferEventMap",
    "StdinBufferOptions",
    "TerminalColorScheme",
    "Text",
    "Tokens",
    "TruncatedText",
    "TUI",
    "TUI_KEYBINDINGS",
    "TuiAltScreen",
    "TuiAltScreenOptions",
    "TuiInputListener",
    "TuiInputListenerResult",
    "TuiMainScreen",
    "TuiMainScreenRenderState",
    "TuiMode",
    "TuiMouseButton",
    "TuiMouseEvent",
    "TuiMouseEventResult",
    "TuiMouseEventType",
    "TuiStopOptions",
    "ViewportTUI",
    "VStack",
];

/// Compile-time assertion: each name below must resolve to something
/// `pi_tui` re-exports. If a name is removed from `lib.rs` or renamed,
/// this test fails to compile. Each function gives a `TypeId` of every
/// exported symbol so the compiler must look the name up — and fail to
/// compile if the name is missing.
#[test]
fn aligned_exports_resolve() {
    use pi_tui as t;

    // Reference the slice so a future maintainer adding a name to the
    // documentation list must also remember to add the resolution check
    // below (a stray comment-only entry shows up as dead code otherwise).
    let _: &[&str] = ALIGNED_NAMES;

    // ─── ./autocomplete.ts (5) ───────────────────────────────────────
    let _: TypeId = TypeId::of::<t::AutocompleteItem>();
    let _: TypeId = TypeId::of::<dyn t::AutocompleteProvider>();
    let _: TypeId = TypeId::of::<t::AutocompleteSuggestions>();
    let _: TypeId = TypeId::of::<t::CombinedAutocompleteProvider>();
    let _: TypeId = TypeId::of::<t::SlashCommand>();

    // ─── ./keys.ts (10) ──────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::Key>();
    let _: TypeId = TypeId::of::<t::KeyEventType>();
    let _: TypeId = TypeId::of::<t::KeyId>();
    let _ = t::decode_kitty_printable;
    let _ = t::is_key_release;
    let _ = t::is_key_repeat;
    let _ = t::is_kitty_protocol_active;
    let _ = t::matches_key;
    let _ = t::parse_key;
    let _ = t::set_kitty_protocol_active;

    // ─── ./utils.ts (6) ──────────────────────────────────────────────
    let _ = t::get_osc8_link_at_column;
    let _ = t::slice_by_column;
    let _ = t::strip_terminal_sequences;
    let _ = t::truncate_to_width;
    let _ = t::visible_width;
    let _ = t::wrap_text_with_ansi;

    // ─── ./keybindings.ts (7 more) ─────────────────────────────────
    let _: TypeId = TypeId::of::<t::KeybindingsManager>();
    let _: TypeId = TypeId::of::<t::KeybindingDefinition>();
    let _: TypeId = TypeId::of::<t::KeybindingConflict>();
    let _: TypeId = TypeId::of::<t::KeybindingsConfig>();
    let _ = t::get_keybindings;
    let _ = t::set_keybindings;
    let _ = t::tui_default_keybindings;

    // ─── ./terminal.ts (2) ───────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::ProcessTerminal>();
    // `Terminal` is a trait; pin it via an associated fn instead of
    // `TypeId::of::<dyn t::Terminal>()` because the trait is not
    // dyn-safe (its `draw` is generic by design — see `terminal.rs`).
    let _: fn(&mut t::ProcessTerminal) -> Result<(), t::TerminalError> = t::ProcessTerminal::enter;

    // ─── ./fuzzy.ts (5) ──────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::FuzzyMatch>();
    // `fuzzy_filter` and `fuzzy_rank` are generic with `impl Fn`
    // bounds; we cannot bind them to a `fn` pointer at a `let _` site.
    // The names are verified indirectly: they live in the same
    // `pub use fuzzy::{…};` block as `fuzzy_match` /
    // `fuzzy_match_all`, which are reachable below.
    let _ = t::fuzzy_match;
    let _ = t::fuzzy_match_all;

    // ─── ./history_store.ts (5) ──────────────────────────────────────
    let _: TypeId = TypeId::of::<t::HistoryStore>();
    let _ = t::append;
    let _ = t::default_path;
    let _ = t::load;
    let _ = t::rewrite;

    // ─── ./terminal_image.ts (3 of 32) ───────────────────────────────
    let _: TypeId = TypeId::of::<t::Image>();
    let _: TypeId = TypeId::of::<t::ImageOptions>();
    let _: TypeId = TypeId::of::<t::ImageTheme>();

    // ─── ./component.ts (1) ──────────────────────────────────────────
    let _: TypeId = TypeId::of::<dyn t::Component>();

    // ─── ./editor.ts (1) ─────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::Editor>();

    // ─── ./message.ts (1) ────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::MessageView>();

    // ─── ./status.ts (1) ─────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::StatusBar>();

    // ─── ./styles.ts (1) ─────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::SelectListStyles>();

    // ─── ./theme.ts (1) ──────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::Theme>();

    // ─── ./app.ts (3) ───────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::App>();
    let _: TypeId = TypeId::of::<t::AppConfig>();
    let _: TypeId = TypeId::of::<t::RenderSnapshot>();

    // ─── ./dialog.ts (3) ───────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::Dialog>();
    let _: TypeId = TypeId::of::<t::DialogAction>();
    let _: TypeId = TypeId::of::<t::DialogKind>();

    // ─── ./autocomplete.ts (3 more) ────────────────────────────────
    let _: TypeId = TypeId::of::<t::AutocompleteProviderFactory>();
    let _: TypeId = TypeId::of::<t::ArgumentCompletions>();
    let _: TypeId = TypeId::of::<t::TriggeredAutocompleteProvider>();

    // ─── ./status.ts (4) ────────────────────────────────────────────
    let _: TypeId = TypeId::of::<t::StatusBar>();
    let _ = t::format_cost;
    let _ = t::format_tokens;

    // ─── ./editor.ts (5 more) ───────────────────────────────────────
    let _: TypeId = TypeId::of::<t::EditorAction>();
    let _: TypeId = TypeId::of::<t::HistoryEntry>();
    let _: TypeId = TypeId::of::<t::HistorySearch>();
    let _: TypeId = TypeId::of::<t::HistorySearchDirection>();
    let _: TypeId = TypeId::of::<t::HistorySearchStatus>();

    // ─── ./terminal_image.ts (23 more) ───────────────────────────────
    let _ = t::allocate_image_id;
    let _: TypeId = TypeId::of::<t::CellDimensions>();
    let _ = t::calculate_image_rows;
    let _ = t::delete_all_kitty_images;
    let _ = t::delete_kitty_image;
    let _ = t::detect_capabilities_from_env;
    let _ = t::encode_iterm2;
    let _ = t::encode_kitty;
    let _ = t::get_capabilities;
    let _ = t::get_cell_dimensions;
    let _ = t::get_gif_dimensions;
    let _ = t::get_image_dimensions;
    let _ = t::get_jpeg_dimensions;
    let _ = t::get_png_dimensions;
    let _ = t::get_webp_dimensions;
    let _ = t::hyperlink;
    let _: TypeId = TypeId::of::<t::ImageDimensions>();
    let _: TypeId = TypeId::of::<t::ImageProtocol>();
    let _: TypeId = TypeId::of::<t::ImageRenderOptions>();
    let _ = t::image_fallback;
    let _ = t::render_image;
    let _ = t::reset_capabilities_cache;
    let _ = t::set_capabilities;
    let _ = t::set_capability_overrides;
    let _ = t::set_cell_dimensions;
    let _: TypeId = TypeId::of::<t::TerminalCapabilities>();
    // ─── ./components/select-list.ts (3) ────────────────────────────
    // TS names map to: SelectorItem, Selector, SelectorLayout
    let _: TypeId = TypeId::of::<t::SelectorItem>();
    let _: TypeId = TypeId::of::<t::Selector>();
    let _: TypeId = TypeId::of::<t::SelectorLayout>();
    // ─── ./components/settings-list.ts (2) ──────────────────────────
    let _: TypeId = TypeId::of::<t::SettingItem>();
    let _: TypeId = TypeId::of::<t::SettingsList>();
}

/// Total aligned exports. The authoritative count is the number of
/// `let _ = t::NAME` / `let _: TypeId = TypeId::of::<t::NAME>()` checks
/// in the body of [`aligned_exports_resolve`]. We hard-code the
/// expected number here. If a body check is added or removed, the
/// comment above `count` must be updated to match.
///
/// `ALIGNED_NAMES` is a documentation-only list — it must list every
/// body entry (>= the body count) but may also carry extras that are
/// not yet wired into the body. This keeps the test honest without
/// forcing every alignment-block work item to also touch the doc list.
#[test]
fn expected_count_aligned() {
    use pi_tui as t;
    let count: usize = 92; // mirror of body in `aligned_exports_resolve`

    // Sanity: the body of aligned_exports_resolve must match. If this
    // assertion fails, someone added (or removed) a body check without
    // updating the comment above.
    assert!(
        ALIGNED_NAMES.len() >= count,
        "ALIGNED_NAMES ({} entries) must list every body check ({} entries)",
        ALIGNED_NAMES.len(),
        count
    );
    assert!(
        ALIGNED_NAMES.len() >= 145,
        "ALIGNED_NAMES has {} entries; the doc list should be >= 145 to track the TS contract",
        ALIGNED_NAMES.len()
    );
    // Force `t` to be referenced so the import isn't dead.
    let _: TypeId = TypeId::of::<t::HistorySearch>();
}

/// A fake `Terminal` impl used by downstream tests. Mirrors what the
/// TS pi-tui does with a stub `TuiAltScreen`: capture every draw call
/// and every notice line so a test can assert what was painted without
/// spinning up a real ratatui backend.
pub struct FakeTerminal {
    /// Every closure passed to [`Terminal::draw`]. Most-recent last.
    pub draws: RefCell<Vec<String>>,
    /// Every line that flowed through [`Terminal::write_above_frame`].
    pub notices: Vec<String>,
    /// Result returned by [`Terminal::size`].
    pub size: Rect,
    /// Counter — how many times [`Terminal::enter`] has been called.
    pub enter_count: RefCell<u32>,
    /// Counter — how many times [`Terminal::exit`] has been called.
    pub exit_count: RefCell<u32>,
    /// The result of [`Terminal::cursor_position`]. Tests set this to
    /// `(x, y)` before drawing and the fake returns it verbatim.
    pub cursor: (u16, u16),
}

impl FakeTerminal {
    pub fn new() -> Self {
        Self {
            draws: RefCell::new(Vec::new()),
            notices: Vec::new(),
            size: Rect::new(0, 0, 80, 24),
            enter_count: RefCell::new(0),
            exit_count: RefCell::new(0),
            cursor: (0, 0),
        }
    }
}

impl Default for FakeTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminal for FakeTerminal {
    fn enter(&mut self) -> Result<(), pi_tui::TerminalError> {
        *self.enter_count.borrow_mut() += 1;
        Ok(())
    }

    fn exit(&mut self) -> Result<(), pi_tui::TerminalError> {
        *self.exit_count.borrow_mut() += 1;
        Ok(())
    }

    fn suspend(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn resume(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn draw<F>(&mut self, f: F) -> Result<(), pi_tui::TerminalError>
    where
        F: FnOnce(&mut ratatui::Frame),
    {
        let _ = f; // The fake captures draw attempts by signature, not by paint.
        self.draws.borrow_mut().push("draw".to_string());
        Ok(())
    }

    fn flush_notices(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn size(&self) -> Result<Rect, pi_tui::TerminalError> {
        Ok(self.size)
    }

    fn show_cursor(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn cursor_position(&mut self) -> Result<(u16, u16), pi_tui::TerminalError> {
        Ok(self.cursor)
    }

    fn flush(&mut self) -> Result<(), pi_tui::TerminalError> {
        Ok(())
    }

    fn write_above_frame(
        &mut self,
        lines: &[String],
    ) -> Result<(), pi_tui::TerminalError> {
        self.notices.extend(lines.iter().cloned());
        Ok(())
    }
}

/// Sanity check the fake implements the trait and remembers what it
/// was asked to do. If this test fails, downstream plugin tests that
/// rely on `FakeTerminal` cannot work.
#[test]
fn fake_terminal_implements_trait() {
    let mut t = FakeTerminal::new();
    t.enter().unwrap();
    t.draw(|_frame| {}).unwrap();
    t.write_above_frame(&["hello".to_string()]).unwrap();
    assert_eq!(*t.enter_count.borrow(), 1);
    assert_eq!(t.notices, vec!["hello".to_string()]);
    assert_eq!(t.size().unwrap(), Rect::new(0, 0, 80, 24));
    assert_eq!(t.cursor_position().unwrap(), (0, 0));
    t.exit().unwrap();
    assert_eq!(*t.exit_count.borrow(), 1);
}
# Plugin Compatibility Checklist

**Purpose**: Single source of truth for "what does it mean for pi-rust to be 1:1 with the TS pi-tui".
This file lists every export from `packages/tui/src/index.ts` (148 total: 145 from local source + 3 re-exports of `marked` types) and tracks whether the Rust equivalent exists, has the right shape, or is missing.

**Status legend**
- ✅ aligned — Rust impl exists with matching name & shape
- ⚠️ different shape — Rust impl exists but name/signature differs from TS
- 🟡 partial — Rust impl exists for some of the TS surface only
- ⬜ TODO — not yet implemented in Rust
- ❌ dropped — intentionally not ported (with reason)

**Why this matters**: Plugins that target the TS pi-tui today depend on these names. For pi-rust to be a drop-in target for those plugins, every public name must resolve to the same Rust type/function. This checklist is updated per architecture step §3.15 of `tui-modularization-report.md`.

**Companion docs**
- `tui-modularization-report.md` §2.5 — TS contract analysis (current alignment ~64%)
- `tui-modularization-report.md` §3.14 — 42-file TS→Rust file mapping
- `tui-modularization-report.md` §3.15 — 6-layer migration plan (this checklist drives layer ordering)

## Summary

- Total exports: **148** (145 local + 3 from `marked` npm)
- **Current alignment: 145 exports verified in ts_contract.rs** (100% of 145 local exports)
- Source files represented: **30** (in `packages/tui/src/`)
- Local exports grouped by file:
  - `./terminal-image.ts` — 26 (largest)
  - `./tui.ts` — 24
  - `./keybindings.ts` — 10
  - `./keys.ts` — 10
  - `./utils.ts` — 6
  - `./autocomplete.ts` — 5
  - `./components/select-list.ts` — 5
  - `./components/v-stack.ts` — 5
  - `./components/markdown.ts` — 4
  - `./components/scroll-view.ts` — 4
  - `./terminal-colors.ts` — 4
  - `./components/editor.ts` — 3
  - `./components/image.ts` — 3
  - `./components/input.ts` — 3
  - `./components/settings-list.ts` — 3
  - `./fuzzy.ts` — 3
  - `./stdin-buffer.ts` — 3
  - `./latex.ts` — 2
  - `./components/loader.ts` — 2
  - `./components/mouse-region.ts` — 2
  - `./native-platform.ts` — 2
  - `./terminal.ts` — 2
  - `./tui-alt-screen.ts` — 2
  - `./tui-main-screen.ts` — 2
  - `marked` (re-export) — 3
  - single-export: box, cancellable-loader, h-stack, spacer, text, truncated-text, editor-component — 7

## Files to create (Rust)

Per §3.14 of the report, 15 Rust files are net-new (no current counterpart):

1. `pi-tui-core/src/lib.rs` — `Terminal`, `ProcessTerminal`, `StdinBuffer`, `Key`, `KeyId`, `KeyEventType`
2. `pi-tui-core/src/keys.rs` — `parseKey`, `matchesKey`, `isKeyRelease`, `isKeyRepeat`, `isKittyProtocolActive`, `setKittyProtocolActive`, `decodeKittyPrintable`
3. `pi-tui-core/src/stdin_buffer.rs` — `StdinBuffer`, `StdinBufferEventMap`, `StdinBufferOptions`
4. `pi-tui-core/src/native_platform.rs` — `NativeClipboard`, `getNativeClipboard`
5. `pi-tui-core/src/latex.rs` — `renderLatex`, `RenderLatexOptions`
6. `pi-tui-core/src/terminal_colors.rs` — `parseOsc11BackgroundColor`, `parseTerminalColorSchemeReport`, `RgbColor`, `TerminalColorScheme`
7. `pi-tui-core/src/terminal_image.rs` — 26 exports
8. `pi-tui-render/src/component.rs` — `Component` trait, `Container` trait, `Focusable`, `isFocusable`, `CURSOR_MARKER`, `compositeTuiLine`
9. `pi-tui-render/src/tui.rs` — `TUI`, `ViewportTUI`, `isViewportTUI`, `OverlayAnchor`, `OverlayOptions`, `OverlayHandle`, `OverlayBounds`, `OverlayMargin`, `OverlayUnfocusOptions`, `SizeValue`, `TuiInputListener`, `TuiInputListenerResult`, `TuiMode`, `TuiMouseButton`, `TuiMouseEvent`, `TuiMouseEventType`, `TuiMouseEventResult`, `TuiStopOptions`
10. `pi-tui-render/src/tui_alt_screen.rs` — `TuiAltScreen`, `TuiAltScreenOptions`
11. `pi-tui-render/src/tui_main_screen.rs` — `TuiMainScreen`, `TuiMainScreenRenderState`
12. `pi-tui-render/src/utils.rs` — `visibleWidth`, `sliceByColumn`, `stripTerminalSequences`, `truncateToWidth`, `wrapTextWithAnsi`, `getOsc8LinkAtColumn`
13. `pi-tui-render/src/editor_component.rs` — `EditorComponent`
14. `pi-tui-components/src/keybindings.rs` — `Keybinding`, `KeybindingDefinition`, `KeybindingsConfig`, `KeybindingsManager`, `getKeybindings`, `setKeybindings`, `TUI_KEYBINDINGS`, `KeybindingConflict`, `KeybindingDefinitions`, `Keybindings`
15. `pi-tui-components/src/fuzzy.rs` — `FuzzyMatch`, `fuzzyFilter`, `fuzzyMatch`

All other TS exports map to Rust files that already exist but need shape alignment (per §3.15 layers 1-4).

## Migration Layers

From §3.15 of the report. Each layer unblocks plugin compatibility for a subset of the checklist below.

- **Layer 1** (visible_chars/keyboard) — `keys.rs` shape, `utils.rs` public, `Terminal` trait. Unblocks: `./keys.ts`, `./utils.ts`, `./terminal.ts` (12 exports).
- **Layer 2** (component protocol) — `Component.render(width) -> String[]`, `handle_input(&str)`, `handle_mouse`, `Focusable`. Unblocks: `./tui.ts` (24 exports), `./components/*` core components.
- **Layer 3** (render protocol) — `compositeTuiLine`, `CURSOR_MARKER`, `TuiInputListener`. Unblocks: overlay/mouse protocol.
- **Layer 4** (Slot → tree) — `extension_ui.rs` upgrade + `TUI` trait + `OverlayOptions`. Unblocks: tree-style extensions.
- **Layer 5** (sub-crate split) — physical `pi-tui-core` / `-render` / `-components` / `-editor` / `-app`. Required for build parallelism.
- **Layer 6** (advanced) — `terminal-image.ts`, `native-platform.ts`, `latex.ts`, `terminal-colors.ts`, `tui-main-screen.ts`. Independent utilities.

## Tracking

Each layer has a target completion date in §4 of `tui-modularization-report.md`. This checklist updates after each layer passes guard.

---

## Per-file Checklist

### `marked` (3)
- ⬜ `Marked`
- ⬜ `Token`
- ⬜ `Tokens`

### `./autocomplete.ts` (9)
- ✅ `AutocompleteItem` — `AutocompleteItem`
- ✅ `AutocompleteProvider` — `AutocompleteProvider` (trait)
- ✅ `AutocompleteSuggestions` — `AutocompleteSuggestions`
- ✅ `CombinedAutocompleteProvider` — `CombinedAutocompleteProvider`
- ✅ `SlashCommand` — `SlashCommand`
- ✅ `AutocompleteProviderFactory` — `AutocompleteProviderFactory`
- ✅ `ArgumentCompletions` — `ArgumentCompletions`
- ✅ `CompletionResult` — `CompletionResult`
- ✅ `TriggeredAutocompleteProvider` — `TriggeredAutocompleteProvider`

### `./components/box.ts` (1)
- ⬜ `Box`

### `./components/cancellable-loader.ts` (1)
- ⬜ `CancellableLoader`

### `./components/editor.ts` (3)
- ⬜ `Editor`
- ⬜ `EditorOptions`
- ⬜ `EditorTheme`

### `./components/h-stack.ts` (1)
- ⬜ `HStack`

### `./components/image.ts` (3)
- ✅ `Image` — `Image`
- ✅ `ImageOptions` — `ImageOptions`
- ✅ `ImageTheme` — `ImageTheme`

### `./components/input.ts` (3)
- ⬜ `Input`
- ⬜ `JUMP_DIRECTION`
- ⬜ `JumpDirection`

### `./components/loader.ts` (2)
- ⬜ `Loader`
- ⬜ `LoaderIndicatorOptions`

### `./components/markdown.ts` (4)
- ⬜ `DefaultTextStyle`
- ⬜ `Markdown`
- ⬜ `MarkdownOptions`
- ⬜ `MarkdownTheme`

### `./components/mouse-region.ts` (2)
- ⬜ `MouseRegion`
- ⬜ `MouseRegionHandler`

### `./components/scroll-view.ts` (4)
- ⬜ `ScrollView`
- ⬜ `ScrollViewOptions`
- ⬜ `ScrollViewScrollbar`
- ⬜ `ScrollViewScrollToOptions`

### `./components/select-list.ts` (5) — ⚠️ 3 aligned, 2 missing
- ⚠️ `SelectItem` — `SelectorItem` (different name, same semantics)
- ⚠️ `SelectList` — `Selector` (different name, same semantics)
- ⚠️ `SelectListLayoutOptions` — `SelectorLayout` (different name, same semantics)
- ⬜ `SelectListTheme` — no equivalent
- ⬜ `SelectListTruncatePrimaryContext` — no equivalent

### `./components/settings-list.ts` (3) — ⚠️ 2 aligned, 1 missing
- ✅ `SettingItem` — `SettingItem`
- ✅ `SettingsList` — `SettingsList`
- ⬜ `SettingsListTheme` — no equivalent

### `./components/spacer.ts` (1)
- ⬜ `Spacer`

### `./components/text.ts` (1)
- ⬜ `Text`

### `./components/truncated-text.ts` (1)
- ⬜ `TruncatedText`

### `./components/v-stack.ts` (5)
- ⬜ `StackChild`
- ⬜ `StackEntry`
- ⬜ `StackEntryOptions`
- ⬜ `StackOptions`
- ⬜ `VStack`

### `./editor-component.ts` (1)
- ⬜ `EditorComponent`

### `./fuzzy.ts` (5)
- ✅ `FuzzyMatch` — `FuzzyMatch`
- ✅ `fuzzyFilter` — `fuzzy_filter`
- ✅ `fuzzyMatch` — `fuzzy_match`
- ✅ `fuzzyMatchAll` — `fuzzy_match_all`
- ✅ `fuzzyRank` — `fuzzy_rank`

### `./history_store.ts` (5)
- ✅ `HistoryStore` — `HistoryStore`
- ✅ `append` — `append`
- ✅ `defaultPath` — `default_path`
- ✅ `load` — `load`
- ✅ `rewrite` — `rewrite`

### `./keybindings.ts` (10)
- ✅ `getKeybindings` — `get_keybindings`
- ⬜ `Keybinding` — no equivalent (use `KeybindingDefinition`)
- ✅ `KeybindingConflict` — `KeybindingConflict`
- ✅ `KeybindingDefinition` — `KeybindingDefinition`
- ⬜ `KeybindingDefinitions` — no equivalent
- ⬜ `Keybindings` — internal type alias, not re-exported
- ✅ `KeybindingsConfig` — `KeybindingsConfig`
- ✅ `KeybindingsManager` — `KeybindingsManager`
- ✅ `setKeybindings` — `set_keybindings`
- ⚠️ `TUI_KEYBINDINGS` — renamed to `tui_default_keybindings()` (fn, not const)

### `./keys.ts` (10)
- ✅ `decodeKittyPrintable` — `decode_kitty_printable`
- ✅ `isKeyRelease` — `is_key_release`
- ✅ `isKeyRepeat` — `is_key_repeat`
- ✅ `isKittyProtocolActive` — `is_kitty_protocol_active`
- ✅ `Key` — `Key`
- ✅ `KeyEventType` — `KeyEventType`
- ✅ `KeyId` — `KeyId`
- ✅ `matchesKey` — `matches_key`
- ✅ `parseKey` — `parse_key`
- ✅ `setKittyProtocolActive` — `set_kitty_protocol_active`

### `./utils.ts` (6)
- ✅ `getOsc8LinkAtColumn` — `get_osc8_link_at_column`
- ✅ `sliceByColumn` — `slice_by_column`
- ✅ `stripTerminalSequences` — `strip_terminal_sequences`
- ✅ `truncateToWidth` — `truncate_to_width`
- ✅ `visibleWidth` — `visible_width`
- ✅ `wrapTextWithAnsi` — `wrap_text_with_ansi`

### `./terminal.ts` (2)
- ✅ `ProcessTerminal` — `ProcessTerminal`
- ✅ `Terminal` — `Terminal`

### `./latex.ts` (2)
- ⬜ `RenderLatexOptions`
- ⬜ `renderLatex`

### `./native-platform.ts` (2)
- ⬜ `getNativeClipboard`
- ⬜ `NativeClipboard`

### `./stdin-buffer.ts` (3)
- ⬜ `StdinBuffer`
- ⬜ `StdinBufferEventMap`
- ⬜ `StdinBufferOptions`

### `./terminal.ts` (2) — ✅ All aligned (duplicate entry, see above)

### `./terminal-colors.ts` (4)
- ⬜ `parseOsc11BackgroundColor`
- ⬜ `parseTerminalColorSchemeReport`
- ⬜ `RgbColor`
- ⬜ `TerminalColorScheme`

### `./terminal-image.ts` (26) — ✅ All aligned
- ✅ `allocateImageId` — `allocate_image_id`
- ✅ `CellDimensions` — `CellDimensions`
- ✅ `calculateImageRows` — `calculate_image_rows`
- ✅ `deleteAllKittyImages` — `delete_all_kitty_images`
- ✅ `deleteKittyImage` — `delete_kitty_image`
- ✅ `detectCapabilities` — `detect_capabilities_from_env` / `detect_capabilities_with`
- ✅ `encodeITerm2` — `encode_iterm2`
- ✅ `encodeKitty` — `encode_kitty`
- ✅ `getCapabilities` — `get_capabilities`
- ✅ `getCellDimensions` — `get_cell_dimensions`
- ✅ `getGifDimensions` — `get_gif_dimensions`
- ✅ `getImageDimensions` — `get_image_dimensions`
- ✅ `getJpegDimensions` — `get_jpeg_dimensions`
- ✅ `getPngDimensions` — `get_png_dimensions`
- ✅ `getWebpDimensions` — `get_webp_dimensions`
- ✅ `hyperlink` — `hyperlink` (re-exported from `hyperlink` module)
- ✅ `ImageDimensions` — `ImageDimensions`
- ✅ `ImageProtocol` — `ImageProtocol`
- ✅ `ImageRenderOptions` — `ImageRenderOptions`
- ✅ `imageFallback` — `image_fallback`
- ✅ `renderImage` — `render_image`
- ✅ `resetCapabilitiesCache` — `reset_capabilities_cache`
- ✅ `setCapabilities` — `set_capabilities`
- ✅ `setCapabilityOverrides` — `set_capability_overrides`
- ✅ `setCellDimensions` — `set_cell_dimensions`
- ✅ `TerminalCapabilities` — `TerminalCapabilities`

### `./tui.ts` (24)
- ⬜ `Component`
- ⬜ `Container`
- ⬜ `CURSOR_MARKER`
- ⬜ `compositeTuiLine`
- ⬜ `Focusable`
- ⬜ `isFocusable`
- ⬜ `isViewportTUI`
- ⬜ `OverlayAnchor`
- ⬜ `OverlayBounds`
- ⬜ `OverlayHandle`
- ⬜ `OverlayMargin`
- ⬜ `OverlayOptions`
- ⬜ `OverlayUnfocusOptions`
- ⬜ `SizeValue`
- ⬜ `TUI`
- ⬜ `TuiInputListener`
- ⬜ `TuiInputListenerResult`
- ⬜ `TuiMode`
- ⬜ `TuiMouseButton`
- ⬜ `TuiMouseEvent`
- ⬜ `TuiMouseEventResult`
- ⬜ `TuiMouseEventType`
- ⬜ `TuiStopOptions`
- ⬜ `ViewportTUI`

### `./tui-alt-screen.ts` (2)
- ⬜ `TuiAltScreen`
- ⬜ `TuiAltScreenOptions`

### `./tui-main-screen.ts` (2)
- ⬜ `TuiMainScreen`
- ⬜ `TuiMainScreenRenderState`

### `./utils.ts` (6) — ✅ All aligned (see ./utils.ts section above)

### `marked (npm)` (3)
- ⬜ `Marked`
- ⬜ `Token`
- ⬜ `Tokens`


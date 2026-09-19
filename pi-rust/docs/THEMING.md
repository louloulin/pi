# Theming — the `pi-tui` theme consumption layer

`pi-tui` has two theme-related layers:

1. **`theme.rs`** (LUM-1101) parses the theme documents and resolves every
   slot to an ANSI prefix: `Theme::fg` / `Theme::bg` / `get_fg_ansi` /
   `get_bg_ansi`, `builtin_theme("dark" | "light", mode)`, `ThemeController`.
2. **`styles.rs`** (LUM-1112, this slice) adapts that palette for individual
   components, mirroring the per-component adapters upstream builds in
   `packages/coding-agent/src/modes/interactive/theme/theme.ts`.

The adapter is `SelectListStyles`, a `&Theme` borrow:

```rust
use pi_tui::styles::SelectListStyles;
use pi_tui::theme::{builtin_theme, ColorMode};

let theme = builtin_theme("dark", ColorMode::TrueColor).unwrap();
let styles = SelectListStyles::new(&theme);
println!("{}", styles.selected_text("row"));
```

The components do not change their existing plain-text API. Instead each
render method has a themed twin:

| plain | themed |
|-------|--------|
| `Selector::render_lines` | `Selector::render_lines_themed(width, &styles)` |
| `StatusBar::render` | `StatusBar::render_themed(data, width, &styles)` |
| `MessageView::render_lines` | `MessageView::render_lines_themed(width, &styles)` |

The visible text is byte-for-byte identical between the two — the themed
variant only inserts escape sequences. A `ColorMode::None` theme makes the
themed variant equal to the plain one, which is what the regression test
`a_plain_color_mode_emits_no_escape_sequences_from_any_component` pins.

## Slot mapping (upstream evidence)

| method | slot(s) | upstream |
|--------|---------|----------|
| `selected_prefix` | `accent` | `theme.ts:1211` |
| `selected_text` | `accent` fg + `selectedBg` bg | `theme.ts:1212` + `session-selector.ts:507` |
| `description` | `muted` | `theme.ts:1213` |
| `scroll_info` | `muted` | `theme.ts:1214` |
| `no_match` | `muted` | `theme.ts:1215` |
| `accent` / `muted` / `dim` / `text` | matching slot | `footer.ts:236-240` (dim stats), `user-message.ts:48` |
| `user_message_text` | `userMessageText` | `user-message.ts:48` |
| `tool_title` / `tool_output` | `toolTitle` / `toolOutput` | `tool-execution.ts:153,165` |
| `error` | `error` | context-percentage colour in `footer.ts` |

The select-list consumers are
`packages/tui/src/components/select-list.ts:80,103,205,208,216`.

## Deliberate differences from upstream

* **Selected row carries a background.** Upstream `getSelectListTheme`
  (`theme.ts:1212`) maps `selectedText` to `theme.fg("accent", text)` only, but
  the slice asks for a highlighted whole row and other upstream lists already
  do that (`session-selector.ts:507`, `tree-selector.ts:751-752`,
  `tui-renderer.ts:32`). `selected_text` is therefore the union: accent
  foreground over `selectedBg`.
* **One adapter, not three.** Upstream also has `MarkdownTheme` and
  `SettingsListTheme`; those components are not ported to `pi-tui` yet
  (markdown rendering is explicitly out of scope). The generic helpers cover
  the status bar and message view without inventing adapters for components
  that do not exist.
* **`ColorMode::None` is a Rust addition.** Upstream only has `"truecolor" |
  "256color"` (`theme.ts:104`). `ColorMode::None` resolves every slot to the
  empty string and makes every `Theme` markup method a no-op, so a caller can
  guarantee plain output (`NO_COLOR`, piped output, tests).
* **The status bar keeps the model readable.** Upstream's two-line footer dims
  the whole stats line (`footer.ts:236-240`); the single-line `StatusBar` puts
  the model in `accent`, the session id in `muted` and the token/usage segment
  in `dim`.

## Not in this slice

Markdown rendering and its `MarkdownTheme` adapter, the file watcher live
reload, the shiki/CLI highlight adapters, and wiring the App's `ratatui::Buffer`
renderer to convert these ANSI strings into `ratatui` cell styles. The App
continues to render plain text into the buffer; the themed string methods are
for callers that emit text (and are the seam a future buffer-styling layer can
reuse).

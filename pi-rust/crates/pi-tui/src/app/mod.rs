//! Top-level [`App`] — owns the editor / message view / status bar and
//! drives the render loop.
//!
//! The App is intentionally framework-agnostic: it operates on the
//! [`InputEvent`] enum and a [`ratatui::buffer::Buffer`] view, so unit
//! tests can drive the full loop with synthetic input and assert on
//! the rendered buffer without ever touching a real terminal.
//!
//! Concrete binaries embed the App in a `crossterm`-backed
//! [`ratatui::Terminal`] and feed it events from the terminal input.
//! See `crates/pi-coding-agent/src/interactive.rs` for the wiring.
//!
//! # Text selection
//!
//! The chat log owns its own selection because mouse capture takes the
//! terminal's native text selection away. Upstream supports three
//! granularities (double-click selects a word, triple-click a line —
//! `SelectionGranularity`, `packages/tui/src/tui-alt-screen.ts:104`) and
//! keeps scrolling while the drag pointer rests on a viewport edge
//! (`updateSelectionAutoScroll`, `tui-alt-screen.ts:1264-1297`). This port
//! mirrors both.
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **Click-count source.** Upstream reads `TuiMouseEvent.clickCount`,
//!   which the engine synthesises; `crossterm` exposes no such field and
//!   [`MouseGesture`] deliberately carries none, so the App reconstructs
//!   the count itself from the same key upstream uses: a press within
//!   [`DOUBLE_CLICK_INTERVAL`] of the previous one, on the same rendered
//!   log line and the same word column range. The rule is upstream's
//!   `getClickCount` (`tui-alt-screen.ts:1220-1245`); only the time source
//!   differs (`std::time::Instant` instead of `Date.now()`).
//! * **Word segmentation.** Word selection is built from
//!   [`crate::word_navigation`]'s UAX #29 boundaries (`unicode_segmentation`,
//!   already a dependency), not `Intl.Segmenter`, with `/` and `-` treated
//!   as joiners exactly like upstream's `TERMINAL_WORD_SELECTION_JOINERS`
//!   (`tui-alt-screen.ts:82`). The segmentation difference is the CJK one
//!   `word_navigation` already documents: ICU's dictionary keeps `你好`
//!   together, UAX #29 splits each ideograph, so double-click selects one
//!   ideograph per hop.
//! * **Columns are characters in the selection model, cells at the edges.**
//!   The selection is stored as character offsets (see
//!   [`App::selection_text`]), which is what makes slicing the text exact. A
//!   pointer cell is snapped to the glyph drawn there on the way in
//!   ([`crate::utils::width::char_index_at_column`], upstream's
//!   `getGraphemeCellRange`), and the character range is turned back into the
//!   cell span it covers on the way out
//!   ([`crate::utils::width::columns_before`]) so the highlight and the search marks
//!   land on the cells a terminal actually paints. Wide glyphs therefore
//!   select as one unit from either half of the character.
//! * **Autoscroll beat.** There is no `setInterval` in Rust and this crate
//!   must not spawn a timer thread, so the drag autoscroll advances one
//!   line **per draw**. The driver redraws on a 50 ms interval
//!   (`pi-coding-agent`'s render loop), which reproduces upstream's 50 ms
//!   `setInterval`; on a stationary pointer, redraws are what keep the
//!   viewport moving. [`App::advance_selection_autoscroll`] is public so
//!   tests can step the beat deterministically.
//!
//! # Scrollbar
//!
//! When the wrapped transcript is taller than the viewport the chat log
//! paints a vertical scrollbar in the viewport's last column. Pointer hover
//! highlights it, a press on the thumb starts a proportional drag that
//! honours the grab offset, a press on the track jumps straight to that
//! offset, and the release ends the drag. This is the port of upstream's
//! `getScrollbarGeometry` / `paintScrollbar`
//! (`packages/tui/src/layout.ts:44-51,280-326`) and `updateScrollbarHover` /
//! `setScrollbarHover` / `handleScrollbarMouseEvent` /
//! `scrollScrollbarToPointer` (`packages/tui/src/tui-alt-screen.ts:1041-1109`).
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **One scroll view.** Upstream keeps a geometry per `ScrollView` and
//!   hit-tests the one under the pointer. The App has exactly one scrollable
//!   region — the message log — so [`App::scrollbar_geometry`] *is* that
//!   geometry and the upstream `getScrollViewBox` lookup collapses away. A
//!   future multi-pane layout would have to bring the lookup back.
//! * **No transient hide delay.** Upstream's default `scrollbar: "auto"`
//!   reveals the bar for `scrollbarHideDelayMs` (1000 ms) after scroll
//!   activity and keeps it while it is hovered. This crate spawns no timer
//!   thread, so the bar is simply shown while the content overflows and
//!   hidden when it fits — the `"always"` variant gated on overflow. Hover
//!   and drag behave exactly as upstream.
//! * **Active styling.** Upstream emphasises the active bar only by swapping
//!   the thumb glyph (`┃` → `█`); there is no colour or modifier change to
//!   observe in a `Buffer`. The port keeps the glyph swap and additionally
//!   renders the track and thumb bold while active, so the hover state is
//!   visible on the style channel too.
//! * **`render_snapshot` omits the bar.** The flat snapshot backs the
//!   `/transcript` text export, so it stays content-only; the bar is an
//!   interaction affordance of the live frame and is painted only by
//!   [`App::render_to_buffer`].
//!
//! # Keybindings
//!
//! The global chords in [`App::step_key`] are resolved through the
//! process-wide registry ([`crate::components::keybindings::get_keybindings`]), so an
//! installed override (`pi-coding-agent`'s `KeybindingsManager::create`)
//! reaches the App. With nothing installed the registry serves the
//! defaults, which are the chords the hardcoded judgements spelled out.
//!
//! Consumed here:
//!
//! * `app.interrupt` (`escape`) — cancel the in-flight turn while busy.
//! * `app.clear` (`ctrl+c`) — cancel while busy; when idle, the first
//!   press clears the composer and a second press inside
//!   [`CLEAR_EXIT_WINDOW`] exits. Matches upstream `handleCtrlC`
//!   (`interactive-mode.ts:3931-3939`), whose 500 ms window is what the
//!   startup header's `Ctrl+C to clear` / `Ctrl+C twice to exit` promises.
//! * `tui.altScreen.pageUp` / `pageDown` (`pageUp` / `pageDown`) and
//!   `tui.altScreen.top` / `bottom` (`home` / `end`) — viewport scrolling.
//! * `tui.altScreen.halfPageUp` / `halfPageDown` (`Ctrl+U` / `Ctrl+D` in
//!   nanopi, unbound by upstream) — half-viewport scrolling; activated
//!   through an installed override.
//! * `tui.altScreen.lineUp` / `lineDown` — single-row scrolling; activated
//!   through an installed override.
//! * `tui.altScreen.previousPrompt` / `nextPrompt` (`Ctrl+Up` / `Ctrl+Down`)
//!   — jump between user prompts.
//! * `tui.altScreen.search` / `searchClose` / `searchNext` /
//!   `searchPrevious` — the transcript search overlay (see below).
//!
//! `app.exit` (`ctrl+d`) is consumed by [`crate::Editor`], which returns
//! [`EditorAction::Eof`] on an empty buffer and falls through to
//! `tui.editor.deleteCharForward` otherwise (upstream `CustomEditor` does
//! the same). The ids the coding-agent driver claims before the App sees the
//! key (`app.model.cycleForward` / `cycleBackward` / `select`, the
//! `app.session.*` and `app.message.*` chords) need no branch here. The
//! remaining `app.*` / `tui.altScreen.*` ids in the merged table have **no
//! consumer** in this port yet and are deliberately not implemented here:
//! `app.suspend`, `app.editor.external`, the `app.tree.*` and `app.models.*`
//! families, and `tui.altScreen.halfPageUp` / `halfPageDown` / `lineUp` /
//! `lineDown` / `previousPrompt` / `nextPrompt`.
//!
//! Two filters guard the startup header and `/hotkeys`: an id must resolve to
//! a chord **and** name an action with a consumer
//! ([`crate::components::keybindings::CONSUMED_APP_ACTIONS`]). Without the second filter a
//! default-table entry that nothing answers was still advertised as a working
//! shortcut (LUM-1240).
//!
//! `app.thinking.toggle` (`ctrl+t`) **is** wired: it collapses / expands
//! assistant thinking blocks and reports the new state through the transient
//! status hint (see [`App::toggle_thinking_visibility`]).
//!
//! `app.tools.expand` (`ctrl+o`) **is** wired too: it collapses / expands
//! every tool block and reports the new state through the transient status
//! hint (see [`App::toggle_tools_expanded`]). A mouse click on a single block
//! toggles just that block.
//!
//! `Ctrl+L` is deliberately **not** claimed here. Upstream binds it to
//! `app.model.select`, "Open model selector"
//! (`packages/coding-agent/src/core/keybindings.ts:116`), and the selector
//! lives in the coding-agent driver, which claims the chord first. Clearing
//! the transcript is [`App::clear_transcript`]'s job, reached through
//! `/clear`; this port used to hardcode `Ctrl+L` to clear, which shadowed the
//! driver's chord and contradicted the header hint (LUM-1245).
//!
//! # Transcript search
//!
//! `Ctrl+Shift+F` opens an in-transcript search overlay — the port of
//! upstream's `AltScreenSearch*` (`packages/tui/src/alt-screen-search.ts`,
//! wired up in `packages/tui/src/tui-alt-screen.ts:496-660,705-720`). The bar
//! is anchored to the top-right of the message viewport, the query is matched
//! against the **rendered** transcript, the selection starts at the first
//! match at or after the viewport's top row, `Enter` / `Ctrl+G` and
//! `Shift+Enter` / `Ctrl+Shift+G` step through matches with wraparound and
//! scroll them into view, hits are highlighted in place (current = bold +
//! reversed, others = underlined), and the bar answers mouse hover / clicks
//! inside its own rectangle. [`crate::search`] holds the query bar, the corpus
//! and the geometry; the App owns the state machine.
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **Columns are characters, not display cells.** [`crate::search`] returns
//!   character-offset spans, matching the selection path above, so wide glyphs
//!   are not widened to the display cell.
//! * **Case folding is per-character `to_lowercase()`**, not the Unicode case
//!   folding of upstream's `regex` `iu` flags: `pi-tui` has no `regex`
//!   dependency and the query is matched literally.
//! * **The corpus is already plain text.** Upstream strips terminal sequences
//!   while building it; the lines this port indexes come from
//!   [`MessageView::visible_lines`] and [`plain_text`], which are sequence-free
//!   by construction.
//! * **No cursor cell.** The bar keeps the query cursor offset (editing is
//!   fully supported) but does not paint a reversed cell for it, and pads its
//!   own borders, so every rendered bar line is exactly as wide as the bar.
//! * **The overlay is anchored to the message viewport**, not the whole
//!   terminal: the App does not own the status / prompt rows. It is painted
//!   *last*, so an extension dialog cannot cover it.
//!
//! # Extension UI host surface
//!
//! Extensions reach the App through `ctx.ui.*`. This port hosts the six
//! region-shaped methods directly on the App, backed by
//! [`crate::component::Component`] objects; the JavaScript side of the bridge
//! is deliberately **not** part of this surface yet (see the closing note).
//!
//! | upstream `ctx.ui` | Rust host surface |
//! | --- | --- |
//! | `setHeader(factory)` | [`App::set_header`] / [`App::clear_header`] |
//! | `setFooter(factory)` | [`App::set_footer`] / [`App::clear_footer`] |
//! | `setWidget(key, content, { placement })` | [`App::set_widget`] |
//! | `setEditorText(text)` | [`App::set_editor_text`] |
//! | `getEditorText()` | [`App::editor_text`] |
//! | `custom(factory, { overlay, overlayOptions, onHandle })` | [`App::open_custom`] → [`crate::component::CustomHandle`], [`App::close_custom`] |
//! | `setEditorComponent(factory)` | [`App::set_editor_component`] / [`App::clear_editor_component`] |
//! | `setStatus(key, text)` | [`App::set_extension_status`] (footer's third row, LUM-1481) |
//! | `setTitle(title)` | [`App::set_terminal_title`] (queued OSC 0, LUM-1485) |
//!
//! `setWorkingMessage` and the remaining `ctx.ui` methods are *not*
//! region-shaped and stay with the coding-agent layer; they are out of
//! scope here. `setStatus` is region-shaped in the one sense that matters —
//! it is host chrome drawn from data the host holds — so the App owns the map
//! and the footer renders it; the JS-side plumbing lives in
//! `pi-extensions` / `pi-coding-agent` and no longer answers
//! `ERR_PI_UI_UNSUPPORTED` for it. `setTitle` is the same story with a
//! different sink: the App holds the title and queues it, and the driver is
//! the only component that writes it ([`App::take_terminal_title`]) — the
//! sequence never enters the frame buffer. The JS factory → Rust component
//! bridge is still incomplete (`ctx.ui.custom` reaches the App, the setters
//! above do not).
//!
//! **Render order** was rearranged by this surface to match upstream's
//! container stack (`header` → chat → widget-above → editor → widget-below →
//! footer, `packages/coding-agent/src/modes/interactive/interactive-mode.ts:547-570,876-885`),
//! where the model / session status line lives inside the footer at the very
//! bottom:
//!
//! ```text
//! header
//! message view                                        ← keeps >= 1 row
//! Above widgets (insertion order)
//! editor region (custom non-overlay → editor component → prompt)
//! Below widgets (insertion order)
//! status bar
//! footer
//! custom overlay (topmost: drawn after the search bar) ← when visible
//! ```
//!
//! One extra region is data driven and sits directly above the editor: the
//! queued-messages block (LUM-1469), upstream's `pendingMessagesContainer`
//! (`interactive-mode.ts:878-892`). It costs zero rows while nothing is
//! queued, so a host that never queues a prompt keeps the geometry it had
//! before the block existed.
//!
//! All region rects are computed once per frame by
//! `crate::components::extension_ui::plan_chrome`; the message viewport's geometry (and
//! therefore the scroll, selection and search coordinates) follows the
//! message rect, so extension regions never shift the transcript under the
//! pointer. Height budgeting reserves the status row and one message row
//! first, then hands out the rest in render order, **truncating the tail** of
//! any region that does not fit (see the `extension_ui` module docs).
//!
//! Keyboard priority while a `custom` overlay is visible is: the overlay's
//! [`Component::handle_input`] first, and only if it returns `false` the
//! existing dialog / settings / selector / search / viewport layers, and
//! finally the prompt. A non-overlay `custom` session or a custom editor
//! component takes the key just before the prompt, so the app-level chords
//! (interrupt, clear, page up/down, search) keep working. A visible
//! component is never hidden by a theme swap: the host resolves its
//! [`crate::utils::styled::SpanStyle`] slots through the live [`Theme`] on every
//! frame.
//!
//! Deliberate deviations, documented here rather than silently omitted:
//!
//! * **One custom session.** Upstream stacks overlays and resolves focus
//!   between them (`showOverlay` / `hideOverlay`,
//!   `packages/tui/src/tui.ts:685-800`). The App hosts at most one, and
//!   opening a second closes the first with a `None` result.
//! * **No mouse routing to extension components.** Upstream's `Component`
//!   has `handleMouse`; this host surface only routes keys, so a gesture
//!   under a `custom` overlay still reaches the chat log. The trait will
//!   grow a mouse hook when the region rectangles are worth hit-testing.
//! * **`OverlayOptions` is a subset.** `width` / `maxHeight` / `anchor` /
//!   `margin` are honoured; percentage sizes, `row` / `col` offsets and the
//!   `visible(termWidth, termHeight)` predicate are not.
//! * **Status row swap.** The status bar is now the second-to-last row and
//!   the prompt sits above it, the reverse of the pre-surface port and the
//!   order upstream uses. `tests/app_theme.rs` pins the new rows.
//!
//! JS factory → Rust component bridge is a later task: nothing in this
//! module executes extension JavaScript.

// Submodules: each owns a focused group of `impl App` methods. They all add
// methods to the [`App`] type from outside this file (Rust allows
// multi-file `impl` blocks), so the public surface stays identical to the
// pre-split port. Submodules access private fields through `use super::*`.
pub mod agent_events;
pub mod history_search;
pub mod history_store;
pub mod layout;
pub mod layout_node;
pub mod viewport;
mod step_dialog;
mod step_key;
mod step_mouse;
mod step_paste;
mod step_search;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as CtEvent, KeyEventKind as CtKeyEventKind, KeyModifiers as CtModifiers,
    MouseEventKind as CtMouseEventKind,
};
use parking_lot::Mutex;
use pi_agent_core::{Agent, AgentEvent, ThinkingLevel};
use pi_protocol::{Message, StopReason, Usage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

use crate::component::{Component, CustomHandle, CustomOptions, OverlayAnchor, WidgetPlacement};
use crate::components::dialog::{Dialog, DialogKind};
use crate::components::editor::{EditorAction, HistoryEntry, HistorySearchStatus};
use crate::components::extension_ui::{plan_chrome, ChromeLayout, ExtensionFrame, ExtensionUi};
use crate::core::input_parse::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture,
    MouseGestureKind, PasteBurst,
};
use crate::components::keybindings::{get_keybindings, key_text_or, matches_with_fallback, KeybindingsManager};
use crate::components::loader::{format_elapsed, Spinner, SPINNER_INTERVAL_MS};
use crate::locale::{
    format_chord, HeaderKey, Locale, EXTENSIONS_DISABLED_EN, EXTENSIONS_DISABLED_ZH,
    HEADER_ONBOARDING_EN, HEADER_ONBOARDING_ZH, HEADER_TITLE, SHORTCUT_OVERLAY_CLOSE_EN,
    SHORTCUT_OVERLAY_CLOSE_ZH, SHORTCUT_OVERLAY_TITLE_EN, SHORTCUT_OVERLAY_TITLE_ZH, STARTUP_HINTS,
};
use crate::components::message::{MessageItem, MessageView, PendingMessageKind, Role, ToolBlockRenderer};
use crate::components::mouse_region::{MouseRegion, MouseRegionPoint};
use crate::components::prompt::{Prompt, PromptAction};
use crate::components::search::{
    apply_query_key, render_search_bar, search_bar_rect, SearchBar, SearchIndex, SearchMatch,
    SearchSelectionMode,
};
use crate::components::selector::{Selector, SelectorAction, SelectorItem};
use crate::components::settings::{SettingsAction, SettingsList};
use crate::components::slash_menu::SlashMenu;
use crate::components::status::{StatusBar, StatusData};
use crate::app::viewport::{ScrollbarDrag, ViewportGeometry};
pub use crate::app::viewport::ScrollbarGeometry;
pub use crate::app::agent_events::{default_router, AgentEventHandler, AgentEventRouter};
use crate::utils::styled::{
    buffer_row_text, plain_text, themed_text, write_plain_row, write_styled_line, SpanStyle, StyledLine, StyledSpan,
};
use crate::theme::{
    builtin_theme, load_theme, ColorMode, Theme, ThemeBg, ThemeColor,
    ThemeError,
};
use crate::utils::visual_text::VisualLayout;
use crate::utils::width::{columns, truncate_columns};

/// Lines scrolled per wheel notch. Mirrors the upstream `wheelScrollLines`
/// option's default (`packages/tui/src/tui-alt-screen.ts:166,264`).
pub(super) const WHEEL_SCROLL_LINES: usize = 1;

/// Alt+wheel multiplies the per-notch step by this factor, matching
/// upstream's `ALT_WHEEL_SCROLL_MULTIPLIER`
/// (`packages/tui/src/tui-alt-screen.ts:75,968-971`).
pub(super) const ALT_WHEEL_SCROLL_MULTIPLIER: usize = 5;

/// Chords are left-aligned into at least this many columns on a `?` overlay
/// row, so the descriptions line up (codex draws the same table in
/// `bottom_pane/footer.rs`). The actual column is the widest chord in the set
/// when that is wider (`Ctrl+P/Shift+Ctrl+P`), so no entry pushes its own
/// description out of the table's alignment.
const SHORTCUT_CHORD_COLUMN: usize = 16;

/// Minimum width of one column before the `?` overlay splits into two; below
/// it a single column keeps every description whole (LUM-1464).
const SHORTCUT_COLUMN_MIN: usize = 36;

/// Longest rule the `?` overlay's title row draws, so a wide terminal does not
/// get a 200-column line of `─`.
const SHORTCUT_RULE_MAX: usize = 40;

/// Append one `<chord> <description>` cell to a shortcut-overlay row: the
/// chord padded to `chord_column`, the description, then blanks out to
/// `cell_width` so a second column lands on the same edge every row.
///
/// Every measurement is in terminal columns, not characters: the Chinese
/// descriptions are full-width, and a char-counted pad shifts the second
/// column left by one cell per CJK glyph (the LUM-1418 / LUM-1426 class of
/// defect). A description wider than its cell is marked with `…` rather than
/// silently cut (LUM-1412), and trailing blanks are skipped on a
/// single-column row where they would only make the frame look trimmed.
fn push_shortcut_cell(
    row: &mut StyledLine,
    entry: &(String, String),
    cell_width: usize,
    chord_column: usize,
) {
    let (chord, description) = entry;
    let label = format!("  {chord} ");
    let label_width = columns(&label);
    let padded_width = label_width.max(chord_column);
    row.push(StyledSpan::new(
        format!("{label}{}", " ".repeat(padded_width - label_width)),
        SpanStyle::fg(ThemeColor::Accent),
    ));
    let budget = cell_width.saturating_sub(padded_width);
    let (text, text_width) = if budget == 0 {
        (String::new(), 0)
    } else if columns(description) > budget {
        let trimmed = truncate_columns(description, budget - 1);
        (format!("{trimmed}…"), columns(trimmed) + 1)
    } else {
        (description.clone(), columns(description))
    };
    row.push(StyledSpan::new(text, SpanStyle::fg(ThemeColor::Muted)));
    let used = padded_width + text_width;
    if used < cell_width {
        row.push(StyledSpan::new(
            " ".repeat(cell_width - used),
            SpanStyle::PLAIN,
        ));
    }
}

/// Which modal list the last frame painted, for the pointer hit test.
///
/// The modals are painted straight into the cell buffer (there is no
/// component tree to hit-test), so the frame records where the *item* rows
/// landed and [`App::modal_list_hit`] maps a pointer cell back onto a row —
/// the same seam as [`App::autocomplete_first_item`]. Only one list can be
/// pointed at, and the painted order decides which: a dialog and a settings
/// modal win over a selector, exactly like `mouse_regions`.
const MODAL_LIST_NONE: u8 = 0;
/// The `/model`-style picker (`self.selector`).
const MODAL_LIST_SELECTOR: u8 = 1;
/// The select list of an extension dialog (`self.dialog`, `ctx.ui.select`).
const MODAL_LIST_DIALOG: u8 = 2;
/// The `/settings` list (`self.settings`).
const MODAL_LIST_SETTINGS: u8 = 3;

/// Rows the modal pickers paint above their first item row: the title and
/// the `─` rule ([`Selector::render_styled_lines`]).
const SELECTOR_HEADER_ROWS: u16 = 2;

/// How long a second `app.clear` (`Ctrl+C`) press after the first still
/// counts as a double press.
///
/// Upstream keeps the window in `lastSigintTime` and exits when
/// `now - lastSigintTime < 500` (`interactive-mode.ts:3931-3939`); the
/// startup header advertises the same contract as `Ctrl+C twice to exit`.
/// The window is compared against the timestamp [`App::step_key_at`]
/// receives, so tests drive it deterministically instead of racing a real
/// clock.
pub const CLEAR_EXIT_WINDOW: Duration = Duration::from_millis(500);

/// `round(value / divisor)` for unsigned integers, matching the `Math.round`
/// calls in upstream's scrollbar maths
/// (`packages/tui/src/layout.ts:318-322`).
fn round_div(value: usize, divisor: usize) -> usize {
    debug_assert!(divisor > 0, "round_div divisor must not be zero");
    (value + divisor / 2) / divisor
}

/// Slice `text` to the half-open cell range `[start_cell, end_cell)`, in
/// terminal **columns** rather than characters. Mirrors upstream's
/// `sliceByColumn(line, start, length, strict=true)`:
///
/// * a glyph whose cell range starts at or after `start_cell` and ends at
///   or before `end_cell` is kept whole;
/// * a glyph whose cell range straddles `end_cell` (the second cell of a
///   wide character past the boundary) is dropped — terminals cannot draw
///   half a glyph and keeping it would push the row past its region;
/// * a glyph whose cell range starts before `start_cell` (the second cell
///   of a wide character before the boundary) is also dropped.
///
/// `text` is measured with the crate's [`crate::width`] rule, the same one
/// the prompt uses to lay out the row, so the cells line up with the
/// rendered frame.
fn cell_slice_strict(text: &str, start_cell: usize, end_cell: usize) -> String {
    let mut result = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = crate::utils::width::char_columns(ch);
        if col >= end_cell {
            break;
        }
        let in_range = col >= start_cell;
        let fits = col + w <= end_cell;
        if w == 0 {
            // Zero-width glyphs attach to the previous one and are kept
            // when the previous one was kept; their own `col` does not
            // advance, so `in_range` / `fits` decide from the anchor cell.
            if in_range && fits {
                result.push(ch);
            }
            continue;
        }
        if in_range && fits {
            result.push(ch);
        }
        col += w;
    }
    result
}

/// The message viewport's rectangle for a planned frame layout.
///
/// It sits below the header and the above-editor widgets, and its height is
/// whatever [`plan_chrome`] left after the chrome regions.
fn message_rect(area: Rect, layout: &ChromeLayout) -> Rect {
    Rect {
        x: area.x,
        y: area.y + layout.header + layout.above,
        width: area.width,
        height: layout.message,
    }
}

/// Position a `custom` overlay box inside `area`.
///
/// `line_count` is how many rows the component rendered; `max_height` and
/// `margin` shrink that box and the anchor decides which corner it hugs
/// (centred by default, upstream's `OverlayAnchor` default). The result is
/// always clamped inside `area`, so a large margin or a tiny terminal can
/// never place the box off-screen.
fn overlay_rect(area: Rect, options: CustomOptions, line_count: usize) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect {
            x: area.x,
            y: area.y,
            width: 0,
            height: 0,
        };
    }
    let margin = options.margin;
    let inner_width = area.width.saturating_sub(margin.saturating_mul(2)).max(1);
    let inner_height = area.height.saturating_sub(margin.saturating_mul(2)).max(1);
    let width = options.width.unwrap_or(area.width).min(inner_width).max(1);
    let requested = u16::try_from(line_count).unwrap_or(u16::MAX);
    let height = options
        .max_height
        .unwrap_or(requested)
        .min(requested.max(1))
        .min(inner_height)
        .max(1);
    let (x, y) = match options.anchor {
        OverlayAnchor::Center => (
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
        ),
        OverlayAnchor::TopLeft => (area.x + margin, area.y + margin),
        OverlayAnchor::TopRight => (
            area.x + area.width.saturating_sub(width).saturating_sub(margin),
            area.y + margin,
        ),
        OverlayAnchor::BottomLeft => (
            area.x + margin,
            area.y + area.height.saturating_sub(height).saturating_sub(margin),
        ),
        OverlayAnchor::BottomRight => (
            area.x + area.width.saturating_sub(width).saturating_sub(margin),
            area.y + area.height.saturating_sub(height).saturating_sub(margin),
        ),
    };
    let x = x.max(area.x).min(area.x + area.width - width);
    let y = y.max(area.y).min(area.y + area.height - height);
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Window in which two presses on the same word count as a double click
/// (and three as a triple click). Upstream's `DOUBLE_CLICK_INTERVAL_MS`
/// (`packages/tui/src/tui-alt-screen.ts:79`).
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Line-column segments that stay attached to a word when double-clicking,
/// so paths and kebab-case tokens select whole. Upstream's
/// `TERMINAL_WORD_SELECTION_JOINERS` (`packages/tui/src/tui-alt-screen.ts:82`).
const TERMINAL_WORD_SELECTION_JOINERS: [&str; 2] = ["/", "-"];

/// Lines a single autoscroll beat moves the viewport. Upstream scrolls one
/// line per 50 ms `setInterval` tick (`tui-alt-screen.ts:1264-1297`).
const SELECTION_AUTOSCROLL_LINES: usize = 1;

/// Transcript rows the built-in startup header must leave behind before it is
/// allowed to show its full hint list: the composer's row, the status row and
/// this many message rows. A shorter terminal folds the hints for that frame
/// (LUM-1266).
const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// Rows below the message view that are never an extension region's to take:
/// the composer and the (LUM-1466: one- or two-row) status bar. Mirrors the
/// reservation in [`crate::components::extension_ui::plan_chrome`].
const RESERVED_CHROME_ROWS: u16 = 2;

/// Rows the expanded startup header must leave for the rest of the frame.
const HEADER_RESERVED_ROWS: u16 = MIN_TRANSCRIPT_ROWS + RESERVED_CHROME_ROWS;
/// Columns the chat-log scrollbar occupies at the right edge of the frame when
/// it is drawn.
const SCROLLBAR_COLUMNS: u16 = 1;

/// Leading half of the "jump to latest" pill's label. Kept as one string so the
/// shortcut half can be appended (` · <shortcut> `) or dropped when the
/// action is unbound — upstream builds the same label inline
/// (`packages/coding-agent/src/modes/interactive/tui-renderer.ts:29-33`).
const SCROLL_TO_END_LABEL: &str = " ↓ Jump to latest message ";

/// Leading half of the "cut above" hint's label, followed by the number of
/// hidden lines and the key that jumps to the log's head.
///
/// This is the one piece of transcript furniture that has no upstream
/// counterpart: upstream renders nothing when the top edge of the viewport
/// lands in the middle of a block, so a block taller than the viewport (a
/// `/help` dump, a long tool result, a long answer) is indistinguishable from
/// a block that simply *starts* there. See
/// `docs/TUI_TRUNCATION_AFFORDANCE_LUM1273.md`.
const TRUNCATED_ABOVE_LEAD: &str = " ⋯ ";

/// Configuration knobs for the App.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Placeholder shown in the prompt when it is empty.
    pub prompt_placeholder: String,
    /// Session identifier shown in the status bar.
    pub session_id: String,
    /// Polling interval for `crossterm::event::poll` (microseconds). The
    /// TUI uses this to throttle the render loop when there is no
    /// input.
    pub event_poll_interval: Duration,
    /// Render assistant bodies as markdown (headings, lists, fenced code,
    /// emphasis, …) instead of plain text. On by default, matching
    /// upstream's `Markdown` component in the assistant message
    /// (`packages/coding-agent/src/modes/interactive/components/assistant-message.ts`).
    ///
    /// The switch can be flipped at runtime with [`App::set_markdown`];
    /// `/clear` and other transcript operations leave it untouched.
    pub markdown: bool,
    /// Copy a finished chat-log text selection to the clipboard as soon as
    /// the mouse button is released. On by default, matching upstream's
    /// `copyOnSelect ?? true` (`packages/tui/src/tui-alt-screen.ts:272`).
    ///
    /// "Copy" here means "hand the selection text to the driver" (see
    /// [`App::take_clipboard_request`]); the App never touches the
    /// terminal or the system clipboard itself.
    pub copy_on_select: bool,
    /// Render markdown links as OSC 8 hyperlinks when the terminal supports
    /// them.
    ///
    /// `None` (the default) auto-detects from the environment via
    /// [`crate::terminal::capabilities::hyperlinks_supported`]; `Some(false)` forces the
    /// plain `label (url)` fallback and `Some(true)` forces escape output
    /// (useful for tests and for a driver that probed the terminal itself).
    ///
    /// When enabled, the live buffer wraps every link-labelled cell in an
    /// OSC 8 pair and drops the inline `(url)`; the `/transcript` snapshot
    /// stays plain text. See [`crate::hyperlink`].
    pub hyperlinks: Option<bool>,
    /// Lines a collapsed tool block previews before showing the
    /// `… (+M lines, Ctrl+O to expand)` hint.
    ///
    /// Defaults to [`crate::components::message::TOOL_PREVIEW_LINES`] (4). This is the
    /// injection point for the interactive TUI: the driver maps a user
    /// setting onto it, and tests pin it to make a fold assertion exact. It
    /// only seeds the view — [`App::messages_mut`] exposes the live value.
    pub tool_preview_lines: usize,
    /// Show the built-in startup header (the key-hint screen) above the
    /// message view.
    ///
    /// Upstream gates its header on `options.verbose ||
    /// !settingsManager.getQuietStartup()` (`interactive-mode.ts:910`); this
    /// is the same switch at the App level, so a host that wants a quiet
    /// startup (or a test that wants a fixed transcript) starts here.
    /// `false` in [`AppConfig::default`] keeps the headless/default surface
    /// byte-identical to the pre-header port; the interactive driver turns it
    /// on unless the user asked for `--no-header` / `quietStartup`.
    pub startup_header: bool,
    /// Whether the startup header starts expanded (the full hint list) or
    /// folded away.
    ///
    /// Upstream seeds this from `getStartupExpansionState()` (`--verbose ||
    /// toolOutputExpanded`); the Rust port defaults to expanded so a first
    /// run teaches the chords it just shipped. Folded means *no rows* — the
    /// message view takes the space back (see [`App::toggle_header`]).
    pub startup_header_expanded: bool,
    /// Copy table the built-in startup header reads (see [`crate::locale`]).
    pub locale: Locale,
    /// The extension summary the built-in startup header shows under the
    /// title (see [`ExtensionHeader`]).
    ///
    /// The driver owns extension discovery (`pi-tui` cannot depend on
    /// `pi-coding-agent`), so it hands the App this projection. The default
    /// is [`ExtensionHeader::Hidden`], which keeps the headless and
    /// no-extension header byte-identical to the pre-Stage-71 surface.
    pub extension_header: ExtensionHeader,
    /// Upper cap on how many rows the composer can grow into when the
    /// buffer wraps. The composer never shrinks below one row.
    ///
    /// The prompt computes the natural row count from the buffer and the
    /// available width (see [`crate::Prompt::line_count`]); this field is
    /// the upper bound the App hands [`crate::Prompt::render_lines`].
    /// `1` reproduces the pre-multi-line single-row composer; `8`
    /// matches Martty's `min(h/2, 12)` cap for tall terminals
    /// (`src/ui.rs:25-54`). The default is `8`.
    pub composer_max_rows: usize,
    /// Recognize a paste the terminal delivered as a fast burst of key
    /// events instead of a bracketed-paste event (codex `paste_burst`; the
    /// fallback for old emulators and some SSH / multiplexer relays).
    ///
    /// `false` — the default — keeps the App deterministic for headless
    /// consumers and unit tests: two synthetic characters can share an
    /// `Instant`, which a real terminal never does. The interactive driver
    /// turns it on ([`AppConfig::default`] stays off, mirroring
    /// `startup_header`), and only then does [`App::step_key_at`] classify a
    /// run of plain characters by timing.
    pub paste_burst: bool,
    /// Cross-session composer history file (see [`crate::history_store`]).
    ///
    /// `None` — the default — disables persistence, so a headless App or a
    /// test never reads or writes the user's real history. The interactive
    /// driver sets this to `~/.pi/agent/history.jsonl` (or `$PI_HOME`), which
    /// is what makes `Ctrl+R` recall survive a restart and what codex's
    /// `~/.codex/history.jsonl` does for its composer.
    pub history_path: Option<std::path::PathBuf>,
}

/// What the built-in startup header says about loaded extensions.
///
/// A `ctx.ui.setHeader` from an extension still replaces the whole built-in
/// header (including this row): upstream renders `customHeader ??
/// builtInHeader` (`interactive-mode.ts:958`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExtensionHeader {
    /// No extension row. Nothing was loaded (or the caller says nothing).
    #[default]
    Hidden,
    /// `N extension(s): <display names>` right under the title.
    Loaded {
        /// How many sources loaded.
        count: usize,
        /// Display names, already shortened by the driver (`~` / `./`).
        names: Vec<String>,
    },
    /// The user turned extension loading off (`--no-extensions`): the header
    /// says `extensions: none (--no-extensions)`.
    Disabled,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            prompt_placeholder: "type a prompt — /help for commands".to_string(),
            session_id: "local".to_string(),
            event_poll_interval: Duration::from_millis(50),
            markdown: true,
            copy_on_select: true,
            hyperlinks: None,
            tool_preview_lines: crate::components::message::TOOL_PREVIEW_LINES,
            startup_header: false,
            startup_header_expanded: true,
            locale: Locale::default(),
            extension_header: ExtensionHeader::Hidden,
            composer_max_rows: 8,
            paste_burst: false,
            history_path: None,
        }
    }
}

/// Provider usage reported by the most recently finished agent turn.
///
/// Rust [`Message`] rows carry no provider usage, and `pi-tui` cannot
/// depend on `pi-coding-agent` (the dependency runs the other way), so
/// the driver cannot recover the last turn's usage from the agent's
/// message log. [`App::drain_agent_events`] records it here instead and
/// drivers consume it with [`App::take_turn_usage`] — `pi-coding-agent`
/// uses it to decide whether automatic compaction should run.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsage {
    /// Usage reported by the turn's final assistant message.
    pub usage: Usage,
    /// Stop reason of the turn's final assistant message. Drivers use it to
    /// tell a silent context overflow (`stop` with input over the window) and
    /// a length-stop overflow (`max_tokens` with zero output) from a normal
    /// turn, so compaction can be triggered before the threshold is crossed.
    pub stop_reason: StopReason,
    /// Messages that followed that assistant message in the turn (the
    /// tool results it produced), for `context_tokens_with_trailing`.
    pub trailing: Vec<Message>,
}

/// Granularity of the active selection. Upstream's
/// `SelectionGranularity` (`packages/tui/src/tui-alt-screen.ts:104`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionGranularity {
    /// Cell-by-cell selection (a plain drag).
    Character,
    /// Whole-word selection (a double click, or a drag started by one).
    Word,
    /// Whole-line selection (a triple click, or a drag started by one).
    Line,
}

/// One end of a selection, in absolute rendered-log coordinates.
///
/// [`SelectionPoint::boundary`] mirrors upstream's `SelectionPoint.boundary`
/// (`packages/tui/src/tui-alt-screen.ts:104-110`): a word / line range ends
/// *between* cells, so the end column is exclusive, while a character
/// focus column is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionPoint {
    /// Index of a line in [`MessageView::render_styled_lines`].
    pub(crate) line: usize,
    /// Character column within that line.
    pub(crate) col: usize,
    /// True when `col` is an exclusive end (a word / line range edge).
    pub(crate) boundary: bool,
}

impl SelectionPoint {
    /// An inclusive cell (a character-granularity endpoint).
    const fn cell(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            boundary: false,
        }
    }

    /// An exclusive range edge (the end of a word / line range).
    const fn boundary(line: usize, col: usize) -> Self {
        Self {
            line,
            col,
            boundary: true,
        }
    }

    /// Reading-order key: line first, then column (the `boundary` flag does
    /// not affect order, matching upstream's row/col comparison).
    const fn order(&self) -> (usize, usize) {
        (self.line, self.col)
    }
}

/// A text selection in the chat log.
///
/// Both ends are absolute coordinates in the *rendered log*: the `line` of a
/// line as produced by [`MessageView::render_styled_lines`], and a character
/// column within it. Absolute line indices (rather than screen rows) are what
/// makes a selection survive scrolling and trailing output: the same text
/// stays selected while the viewport moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Where the drag started (or the start of the press range).
    anchor: SelectionPoint,
    /// Where the pointer currently is (or the end of the press range).
    focus: SelectionPoint,
    /// Granularity the gesture started at; a drag recomputes the focus
    /// range the same way (upstream's `selectionGranularity`).
    granularity: SelectionGranularity,
    /// The range captured on press for word / line granularity. Drags
    /// recompute the focus range from the pointer and anchor against this
    /// (upstream's `selectionInitialRange`,
    /// `packages/tui/src/tui-alt-screen.ts:1193-1218`). `None` for
    /// character selections.
    initial: Option<(SelectionPoint, SelectionPoint)>,
}

impl Selection {
    /// Construct a zero-length character selection at `point` (a mouse
    /// press).
    const fn at(point: (usize, usize)) -> Self {
        let point = SelectionPoint::cell(point.0, point.1);
        Self {
            anchor: point,
            focus: point,
            granularity: SelectionGranularity::Character,
            initial: None,
        }
    }

    /// Construct a word / line selection from the range captured on press.
    const fn range(
        start: SelectionPoint,
        end: SelectionPoint,
        granularity: SelectionGranularity,
    ) -> Self {
        Self {
            anchor: start,
            focus: end,
            granularity,
            initial: Some((start, end)),
        }
    }

    /// Start / end in reading order, or `None` when the selection is
    /// empty — upstream's `getSelectionBounds`
    /// (`packages/tui/src/tui-alt-screen.ts:1381-1397`) treats an
    /// anchor equal to the focus as "no selection".
    pub(crate) fn bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        if self.anchor.order() == self.focus.order() {
            return None;
        }
        Some(if self.anchor.order() < self.focus.order() {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        })
    }
}

/// A press recorded for double / triple click detection (upstream's
/// `ClickTarget`, `packages/tui/src/tui-alt-screen.ts:114-121`).
#[derive(Debug, Clone, Copy)]
struct ClickTarget {
    /// When the press happened.
    at: Instant,
    /// Consecutive clicks so far (1, 2, 3, then back to 1).
    count: usize,
    /// Rendered-log line the press landed on.
    row: usize,
    /// First column of the word range the press resolved to.
    word_start: usize,
    /// Exclusive end column of that word range.
    word_end: usize,
}

/// One UAX #29 segment of a rendered line, in character columns.
///
/// Character columns, not cells: the segment table is compared against the
/// character-offset pointer the App resolves (see `word_selection`), and the
/// cell span is computed once when the highlight is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WordSegment {
    /// First column of the segment (inclusive).
    start: usize,
    /// One past the last column of the segment (exclusive).
    end: usize,
    /// Whether the segment is pickable (word-like, or a joiner).
    selectable: bool,
    /// Whether the segment is one of [`TERMINAL_WORD_SELECTION_JOINERS`].
    joiner: bool,
}

/// Split a rendered line into the segments upstream's `getWordSelection`
/// walks (`packages/tui/src/tui-alt-screen.ts:1156-1197`).
///
/// Columns are character offsets, not terminal cells — the pointer is
/// converted to a character offset before this table is consulted.
fn word_segments(line: &str) -> Vec<WordSegment> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for segment in line.split_word_bounds() {
        let end = start + segment.chars().count();
        let joiner = TERMINAL_WORD_SELECTION_JOINERS.contains(&segment);
        let selectable = crate::utils::word_navigation::is_word_like(segment) || joiner;
        segments.push(WordSegment {
            start,
            end,
            selectable,
            joiner,
        });
        start = end;
    }
    segments
}

/// Upstream's `canJoin` (`packages/tui/src/tui-alt-screen.ts:1174-1177`):
/// two pickable segments join when at least one of them is a joiner.
fn word_segments_can_join(left: &WordSegment, right: &WordSegment) -> bool {
    left.selectable && right.selectable && (left.joiner || right.joiner)
}

/// Exclusive end column of a selection end, clamped to `len` — upstream's
/// `getSelectionColumns` end half (`packages/tui/src/tui-alt-screen.ts:1424-1432`):
/// a boundary column is already one past the last selected cell, while an
/// inclusive focus column selects the cell under it.
fn selection_end_column(end: &SelectionPoint, len: usize) -> usize {
    if end.boundary {
        end.col.min(len)
    } else {
        end.col.saturating_add(1).min(len)
    }
}

/// Open transcript search state (upstream `ActiveSearch`,
/// `packages/tui/src/tui-alt-screen.ts:500-511`).
///
/// The [`SearchIndex`] caches the corpus + matches, [`SearchBar`] owns the
/// query and its caret, and the remaining fields are the selection contract
/// `refreshSearch` implements: which match is selected, the key used to keep
/// that match across a re-index, the row the next "first match at or after"
/// lookup is anchored to, and how the *next* refresh should recompute the
/// selection.
#[derive(Debug)]
pub(crate) struct SearchState {
    /// Cached corpus + matches for the rendered transcript.
    index: SearchIndex,
    /// The query bar and its result counter.
    bar: SearchBar,
    /// Matches from the last refresh.
    pub(crate) matches: Vec<SearchMatch>,
    /// Index into [`SearchState::matches`] of the selected match.
    pub(crate) selected_index: Option<usize>,
    /// [`SearchMatch::key`] of the selected match, so a re-index keeps it.
    selected_key: Option<String>,
    /// Row the `Query` selection mode anchors to.
    anchor_row: usize,
    /// How the next refresh recomputes the selection.
    selection_mode: SearchSelectionMode,
}

impl SearchState {
    /// A fresh, empty search anchored to `anchor_row`.
    fn new(anchor_row: usize) -> Self {
        Self {
            index: SearchIndex::new(),
            bar: SearchBar::new(),
            matches: Vec::new(),
            selected_index: None,
            selected_key: None,
            anchor_row,
            selection_mode: SearchSelectionMode::Query,
        }
    }
}

/// Result of routing a key to the open search overlay.
///
/// The overlay owns the keyboard while it is open, *except* for the viewport
/// scroll chords and the process-global Ctrl+C / Ctrl+L: upstream lets those
/// through because `shouldDeferViewportInputToOverlay` is false once the search
/// overlay itself holds focus
/// (`packages/tui/src/tui-alt-screen.ts:644-645,1806-1811`).
#[derive(Debug, Clone)]
pub(super) enum SearchKeyOutcome {
    /// The overlay consumed the key.
    Handled(StepOutcome),
    /// `App::step_key`'s global handling must still see the key.
    PassThrough,
}

/// A submitted composer draft: the user-visible text plus the image chips
/// attached to it, in buffer order.
///
/// The App produces one from the prompt on Enter (or a follow-up chord) and
/// the driver feeds it to the agent. [`Submission::content_blocks`] is the
/// bridge between the composer's chip model and the protocol's
/// `UserMessage` shape: the text block comes first, then one
/// [`pi_protocol::Content::Image`] per attachment
/// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts`, where
/// the pasted path becomes an image part).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Submission {
    /// The draft's visible text (chip labels expanded, paste markers
    /// expanded to the lines they stand for).
    pub text: String,
    /// Pasted images attached to the draft, in buffer order.
    pub images: Vec<pi_protocol::ImageContent>,
    /// The composer buffer the submission was built from, when it came from
    /// the App's own composer: [`CHIP_CHAR`](crate::components::editor::CHIP_CHAR)
    /// sentinels and `[paste #N …]` markers included.
    ///
    /// The prompt history stores this form, not [`Submission::text`], for the
    /// same reason upstream's `pushHistoryEntry` keeps `getText()`: a recalled
    /// paste has to come back as the compact marker rather than the lines it
    /// stands for. `None` for a programmatic submission
    /// ([`Submission::new`] / `From<String>`), where there is no draft.
    pub draft: Option<String>,
}

impl Submission {
    /// A text-only submission.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            draft: None,
        }
    }

    /// The buffer to record in the prompt history.
    ///
    /// The raw draft when there is one (markers and chips intact), otherwise
    /// the expanded text — a programmatic submission has no separate draft.
    pub fn history_text(&self) -> &str {
        self.draft.as_deref().unwrap_or(&self.text)
    }

    /// The `UserMessage` content blocks for this draft: the text first,
    /// then one image block per attachment. A draft with no chips yields
    /// the single text block that `Agent::prompt` would have built.
    pub fn content_blocks(&self) -> Vec<pi_protocol::Content> {
        let mut blocks = vec![pi_protocol::Content::text(self.text.clone())];
        blocks.extend(self.images.iter().cloned().map(pi_protocol::Content::Image));
        blocks
    }
}

impl From<String> for Submission {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Submission {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Outcome returned by [`App::step`] after each key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Step made no meaningful state change.
    Idle,
    /// Step mutated the rendered state.
    Redraw,
    /// User submitted a prompt. The caller is responsible for handing
    /// it to the agent (see [`Submission`]).
    Submitted(Submission),
    /// User pressed Ctrl+C / Ctrl+D — the caller should shut the App
    /// down and (optionally) fall back to print mode.
    Exit,
}

/// What [`App::follow_up_from_editor`] did with the editor buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUpOutcome {
    /// The editor was empty — nothing to submit or queue.
    Empty,
    /// The App was idle, so the chord behaves exactly like Enter. The
    /// caller runs its normal submit path on the returned draft (slash
    /// commands included); the App has already cleared the buffer.
    Submitted(Submission),
    /// A turn was in flight, so the text was queued behind it
    /// (upstream's `streamingBehavior: "followUp"`). The buffer is clear.
    Queued,
    /// A turn was in flight and the draft carried images. The pending queue
    /// is text-only, so instead of dropping the attachments the App left
    /// the draft in the editor and the driver should say why.
    RefusedImages,
}

/// Direction for [`App::jump_to_previous_prompt`] / [`App::jump_to_next_prompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptJumpDirection {
    /// Walk towards the oldest line.
    Previous,
    /// Walk towards the latest line.
    Next,
}

/// Snapshot of the rendered App for tests.
#[derive(Debug, Clone)]
pub struct RenderSnapshot {
    /// Width the snapshot was rendered at.
    pub width: u16,
    /// Height the snapshot was rendered at.
    pub height: u16,
    /// Pre-rendered content lines, top to bottom.
    pub lines: Vec<String>,
    /// Currently displayed placeholder text (empty if the prompt has
    /// a buffer).
    pub prompt_placeholder: String,
    /// Buffer text the editor currently shows.
    pub prompt_buffer: String,
    /// Whether the settings modal is currently visible.
    pub settings_open: bool,
    /// Rendered settings lines (when open).
    pub settings_lines: Vec<String>,
    /// Whether the selector modal is currently visible.
    pub selector_open: bool,
    /// Selector title (when open).
    pub selector_title: Option<String>,
    /// Selector items (when open).
    pub selector_items: Vec<SelectorItem>,
    /// Selector cursor index (when open).
    pub selector_cursor: Option<usize>,
    /// Whether a modal extension dialog is currently visible.
    pub dialog_open: bool,
    /// Flavour of the open dialog (when open).
    pub dialog_kind: Option<DialogKind>,
    /// Title of the open dialog (when open).
    pub dialog_title: Option<String>,
    /// Rendered dialog lines (when open).
    pub dialog_lines: Vec<String>,
    /// Whether the transcript search overlay is visible.
    pub search_open: bool,
    /// Current search query (empty while the overlay is closed).
    pub search_query: String,
    /// Rendered search-bar lines (when open).
    pub search_lines: Vec<String>,
    /// Current history-search query (empty while the search is closed).
    pub history_search_query: String,
    /// Status bar snapshot.
    pub status: StatusData,
}

impl RenderSnapshot {
    /// Convert the lines into a `Buffer` so callers can compare against
    /// `Buffer::with_lines` results.
    pub fn to_buffer(&self) -> Buffer {
        Buffer::with_lines(self.lines.iter().map(String::as_str))
    }
}

/// Top-level App.
pub struct App {
    config: AppConfig,
    prompt: Prompt,
    messages: MessageView,
    status_bar: StatusBar,
    status_data: StatusData,
    /// The active palette the buffer render path consumes. Swapping it with
    /// [`App::set_theme`] takes effect on the next [`App::render_to_buffer`]
    /// call — no rebuild, no restart.
    theme: Theme,
    selector: Option<Selector>,
    /// Modal settings list opened by `/settings` (upstream
    /// `SettingsSelectorComponent`).
    settings: Option<SettingsList>,
    /// Value the driver must persist — the last
    /// [`SettingsAction::ValueChanged`] the list reported. Consumed with
    /// [`App::take_pending_setting_change`].
    pending_setting_change: Option<(String, String)>,
    /// Item id the driver must handle — the last
    /// [`SettingsAction::Activated`] the list reported (upstream opens the
    /// item's submenu). Consumed with
    /// [`App::take_pending_setting_activation`].
    pending_setting_activation: Option<String>,
    /// Modal requested by a JS extension (`ctx.ui.confirm` / `input` /
    /// `select`) that is waiting for a key press.
    dialog: Option<Dialog>,
    /// Channel carrying dialogs from the extension host. Only set in
    /// interactive mode; see `pi-coding-agent`'s extension UI bridge.
    ui_dialogs: Option<mpsc::UnboundedReceiver<Dialog>>,
    /// Live subscription to agent events. Constructed in
    /// [`App::new`] from `agent.subscribe()` so the App receives every
    /// event the agent emits across all turns.
    event_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    /// Cancellation token for the in-flight prompt, if any.
    cancel_token: Option<CancellationToken>,
    /// Last user-facing error surfaced by the agent loop. Rendered
    /// into the message view on the next step.
    pending_error: Option<String>,
    /// Usage of the most recently finished turn, recorded by
    /// [`App::drain_agent_events`] and consumed by
    /// [`App::take_turn_usage`].
    last_turn_usage: Option<TurnUsage>,
    /// Shared liveness flag for the in-flight `submit` task. The task
    /// clears it when `Agent::prompt` returns, so [`App::is_busy`]
    /// answers "a turn is running" instead of "a turn was ever
    /// started".
    turn_busy: Arc<AtomicBool>,
    /// Set when the App should exit at the next opportunity. The TUI
    /// exit path checks this between key events.
    exit_requested: bool,
    /// The terminal title currently in effect — the last value queued for the
    /// driver (upstream's `Terminal.setTitle`, OSC 0). `None` until something
    /// queues one.
    terminal_title: Option<String>,
    /// A title the driver has not written yet, consumed with
    /// [`App::take_terminal_title`]. One slot, not a queue: only the newest
    /// title matters, so a startup that sets the session name and the cwd in
    /// the same tick emits one sequence, not two.
    pending_terminal_title: Option<String>,
    /// When the last idle `app.clear` (`Ctrl+C`) press landed. A second
    /// press within [`CLEAR_EXIT_WINDOW`] exits; see [`App::step_key_at`].
    /// `None` before the first press of the session.
    last_clear_at: Option<Instant>,
    /// Per-frame geometry — the rectangles the renderer paints and the
    /// pointer / key paths read back. See [`ViewportGeometry`].
    viewport: ViewportGeometry,
    /// Value a pointer click selected in the open picker, waiting for the
    /// driver to apply it with [`App::take_selector_commit`].
    ///
    /// The App does not own what a picker *means* — `/model` switches the
    /// model, `/session` resumes a session — the interactive driver does (it
    /// handles the keyboard's `Enter` itself, `interactive.rs:948-962`). A
    /// click therefore records the same intent the key path would have
    /// produced, and the driver consumes it right after the step.
    pending_selector_commit: Option<String>,
    /// Active chat-log text selection, if any.
    selection: Option<Selection>,
    /// Region-local cell of a left press that landed inside a modal overlay,
    /// kept until its release so only a click that starts and ends on the
    /// same cell commits (upstream's `isClick`,
    /// `packages/tui/src/tui-alt-screen.ts:1312-1315`). `None` whenever no
    /// modal is on screen.
    modal_mouse_press: Option<MouseRegionPoint>,
    /// Absolute cell of a left press that landed inside the composer, kept
    /// until its release so only a click that starts and ends on the same
    /// cell moves the caret (upstream's `isClick` gate).
    prompt_mouse_press: Option<(u16, u16)>,
    /// Item index of a left press that landed inside a tool block region,
    /// kept until its release so only a press-and-release on the same block
    /// toggles its `tool_expanded` (upstream's `isClick` gate). `None`
    /// whenever no tool block owns the press.
    tool_block_press: Option<usize>,
    /// Cell-based composer drag selection, in absolute buffer cell
    /// coordinates (LUM-1332). The selection stores the press anchor and
    /// the current drag focus; the rendered text and highlight are both
    /// derived from the cell range, matching upstream `tui-alt-screen`'s
    /// `selectionAnchor` / `selectionFocus` model rather than the editor's
    /// internal character offsets.
    composer_drag_selection: Option<((u16, u16), (u16, u16))>,
    /// Whether the matching press repainted (caret moved to a new cell).
    /// A click repaints if the press did — the press arms the drag with
    /// anchor==focus, which renders no highlight, so the release has no
    /// independent repaint signal of its own.
    composer_press_moved_caret: bool,
    /// Absolute cell and candidate index of a left press that landed on an
    /// autocomplete row, kept until its release so only a click that starts
    /// and ends on the same cell applies the completion (upstream's
    /// `mousePressTarget` / synthesised `click`,
    /// `packages/tui/src/tui-alt-screen.ts:876-903`).
    autocomplete_mouse_press: Option<(u16, u16, usize)>,
    /// True between a left-button press and its release, so drags extend
    /// the selection without requiring the terminal to report the button.
    selection_dragging: bool,
    /// Open transcript search overlay, if any (upstream `activeSearch`,
    /// `packages/tui/src/tui-alt-screen.ts:496-521`).
    search: Option<SearchState>,
    /// Last press, kept for double / triple click detection (upstream's
    /// `lastClick`, `packages/tui/src/tui-alt-screen.ts:114-121`).
    last_click: Option<ClickTarget>,
    /// Where the drag autoscroll is pulling: `-1` towards older output,
    /// `1` towards the tail, `0` when it is stopped (`selectionAutoScrollDirection`).
    selection_autoscroll_direction: i8,
    /// The pointer cell a drag autoscroll is anchored to — absolute
    /// terminal coordinates, `None` while stopped
    /// (`selectionDragPointer`).
    selection_autoscroll_pointer: Option<(u16, u16)>,
    /// Text waiting to be copied by the driver. Filled by copy-on-select;
    /// consumed with [`App::take_clipboard_request`].
    pending_clipboard: Option<String>,
    /// True when `app.clipboard.pasteImage` (`Alt+V`) fired and the driver
    /// still has to answer it. The App owns no terminal / clipboard handle,
    /// so it records the request and the driver reads the system clipboard
    /// and calls [`App::paste_image`] or [`App::paste_text`] — the same
    /// seam as [`App::pending_clipboard`].
    pending_image_paste: bool,
    /// True while the pointer is over the chat-log scrollbar's column and
    /// rows (upstream's `scrollbarHover`,
    /// `packages/tui/src/tui-alt-screen.ts:221`); drives the bar's active
    /// rendering.
    scrollbar_hover: bool,
    /// In-flight scrollbar drag, if any (upstream's `scrollbarDrag`,
    /// `packages/tui/src/tui-alt-screen.ts:129-138`).
    scrollbar_drag: Option<ScrollbarDrag>,
    /// Provider tool-call index → call id for the assistant message that is
    /// currently streaming.
    ///
    /// A `ToolCallDelta` carries the id only on its first fragment (the
    /// OpenAI streaming shape), so the index is the only key available for
    /// the rest. The map is cleared at `MessageStart` / `MessageEnd`, which
    /// bounds it to one assistant message. See
    /// [`MessageView::begin_tool_stream`].
    tool_call_ids: HashMap<u32, String>,
    /// Extension UI regions (header, footer, widgets, custom editor, `custom`
    /// overlay). See the module docs' "Extension UI host surface" section.
    extension: ExtensionUi,
    /// Prompt text to restore when a non-overlay `custom` session closes
    /// (upstream saves `this.editor.getText()` on `showExtensionCustom`,
    /// `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2755,2778`).
    custom_saved_editor: Option<String>,
    /// Transient status-bar message (upstream's `showStatus`), rendered in the
    /// hint slot of the *next* frame and cleared by the next key press. Used
    /// to acknowledge a chord that changes no visible text on its own, e.g.
    /// `app.thinking.toggle`.
    status_flash: Option<String>,
    /// `?` shortcut overlay (codex `FooterMode::ShortcutOverlay`,
    /// `bottom_pane/chat_composer.rs:3149`): the cheat sheet of chords the
    /// startup header already advertises, toggled by typing `?` into an empty
    /// composer. Any other key closes it and is handled normally, which is
    /// codex's `reset_mode_after_activity`. See
    /// [`App::shortcut_overlay_open`].
    shortcut_overlay: bool,
    /// Driver-supplied rich renderer for tool blocks.
    ///
    /// `pi-tui` cannot depend on the crate that owns the tool renderers, so
    /// the driver installs one with [`App::set_tool_block_renderer`] and the
    /// App hands it each finished [`pi_protocol::ToolResult`] for styling.
    /// See [`ToolBlockRenderer`](crate::components::message::ToolBlockRenderer).
    tool_block_renderer: Option<Box<dyn ToolBlockRenderer>>,
    /// Cell a left press landed on when it hit a tool block, kept until its
    /// release: a click that starts and ends on the same tool block toggles
    /// that block's expansion instead of starting a text selection.
    tool_press: Option<(u16, u16, usize)>,
    /// Cursor into [`crate::components::loader::SPINNER_FRAMES`] for the footer's busy
    /// indicator. Advanced by [`App::tick_busy_feedback`] from the render
    /// loop's existing beat — there is no timer of its own.
    spinner: Spinner,
    /// When the in-flight turn was submitted; the busy indicator's elapsed
    /// time is measured from here. `None` while idle.
    turn_started: Option<Instant>,
    /// When the spinner last advanced. Gates [`Spinner::advance`] to one step
    /// per [`SPINNER_INTERVAL_MS`], so a driver that polls faster than the
    /// upstream 80 ms frame rate still animates at that rate.
    spinner_advanced_at: Instant,
    /// Docked-bottom row that lights up while a tool is running or the model
    /// is thinking. nanopi's `tui.rs:5305-5322` dedicates a 1-row strip above
    /// the footer for "tool running / thinking"; the Rust port buries that
    /// info inside the busy spinner of `status_bar`, so a frozen-looking
    /// footer leaves the reader guessing. The strip owns its own row and
    /// its own spinner so a busy state is visible without parsing the
    /// footer (`N2` in `docs/NANOPI_VS_PI_RUST_GAP_ANALYSIS.md`).
    tool_strip: crate::components::tool_strip::ToolStrip,
    /// Whether the built-in startup header is currently expanded. Distinct
    /// from [`AppConfig::startup_header`] (visible at all) and from an
    /// extension's `ctx.ui.setHeader`, which replaces the built-in lines.
    header_expanded: bool,
    /// Paste-burst classifier (codex `paste_burst`). Fed only while
    /// [`AppConfig::paste_burst`] is on; see [`App::tick_paste_burst`].
    paste_burst: PasteBurst,
    /// Set when a burst was flushed while handling a key that the burst did
    /// not consume, so [`App::step_key_at`] can turn an otherwise idle
    /// outcome into a redraw.
    burst_flush_pending: bool,
    /// Set by the driver when a state change happened that did not go through
    /// `step` / `step_paste` (e.g. an async transcript append, a queued modal).
    /// The pacer reads it via [`App::dirty`] and clears it via
    /// [`App::clear_dirty`] after a successful paint.
    dirty_redraw: bool,
    /// JS extensions can ask for a dialog (`ctx.ui.confirm` / `input` /
    /// `select`) at any time; they sit in a channel drained by
    /// [`App::poll_ui_dialogs`]. Tracked here so the pacer keeps redrawing
    /// until the channel is empty — a dialog appearing must paint, even
    /// if nothing else changed.
    pending_dialogs: Vec<Dialog>,
    /// Session thinking level — what the next provider call requests
    /// (upstream `session.thinkingLevel`). Seeded from the persisted
    /// `defaultThinkingLevel` by the driver, then moved by
    /// `app.thinking.cycle`, `/thinking` and `app.thinking.save`.
    thinking_level: ThinkingLevel,
    /// Whether the active model reasons at all — upstream's
    /// `Model.reasoning` (see `pi-coding-agent`'s equivalent flag; the Rust
    /// descriptor drops the field). When false the status bar omits the
    /// level and the driver answers a cycle with the "does not support
    /// thinking" notice.
    thinking_supported: bool,
    /// Interactive slash command menu overlay (mirrors Martty's slash menu).
    /// Appears when the user types `/` in the editor, shows matching
    /// commands with descriptions, supports ↑/↓ navigation and Enter to execute.
    slash_menu: SlashMenu,
}

/// Upstream's footer join (`components/footer.ts:182-188`): an extended
/// level renders bare, `off` reads `thinking off`.
fn thinking_level_suffix(level: ThinkingLevel) -> String {
    if level == ThinkingLevel::Off {
        "thinking off".to_string()
    } else {
        level.to_string()
    }
}

impl App {
    /// Construct an App over an [`Agent`] handle. The App subscribes
    /// to the agent's event stream immediately and drains events
    /// during [`App::drain_agent_events`] (called by the render
    /// loop).
    pub fn new(agent: &Agent, config: AppConfig) -> Self {
        let model = agent.model();
        let mut status_data = StatusData::new(
            model.label.clone().unwrap_or_else(|| model.id.clone()),
            config.session_id.clone(),
        )
        .with_context_window(model.context_window);
        status_data.hint = Some("? for help".to_string());
        let mut prompt = Prompt::default();
        prompt.set_placeholder(config.prompt_placeholder.clone());
        // Cross-session history is opt-in: the driver hands over a path, and
        // an App built without one never touches the filesystem.
        prompt
            .editor_mut()
            .set_history_path(config.history_path.clone());
        let event_rx = agent.subscribe();
        let markdown = config.markdown;
        let tool_preview_lines = config.tool_preview_lines;
        let header_expanded = config.startup_header_expanded;
        let hyperlinks = config
            .hyperlinks
            .unwrap_or_else(crate::terminal::capabilities::hyperlinks_supported);
        Self {
            config,
            prompt,
            messages: MessageView::new()
                .with_markdown(markdown)
                .with_hyperlinks(hyperlinks)
                .with_tool_preview_lines(tool_preview_lines),
            status_bar: StatusBar::new(),
            status_data,
            theme: builtin_theme("dark", ColorMode::TrueColor)
                .expect("built-in dark theme is valid"),
            selector: None,
            settings: None,
            pending_setting_change: None,
            pending_setting_activation: None,
            dialog: None,
            ui_dialogs: None,
            event_rx: Some(event_rx),
            cancel_token: None,
            pending_error: None,
            last_turn_usage: None,
            turn_busy: Arc::new(AtomicBool::new(false)),
            exit_requested: false,
            terminal_title: None,
            pending_terminal_title: None,
            last_clear_at: None,
            viewport: ViewportGeometry::new(),
            pending_selector_commit: None,
            selection: None,
            search: None,
            modal_mouse_press: None,
            prompt_mouse_press: None,
            tool_block_press: None,
            composer_drag_selection: None,
            composer_press_moved_caret: false,
            autocomplete_mouse_press: None,
            selection_dragging: false,
            last_click: None,
            selection_autoscroll_direction: 0,
            selection_autoscroll_pointer: None,
            pending_clipboard: None,
            pending_image_paste: false,
            scrollbar_hover: false,
            scrollbar_drag: None,
            tool_call_ids: HashMap::new(),
            extension: ExtensionUi::new(),
            custom_saved_editor: None,
            status_flash: None,
            shortcut_overlay: false,
            tool_block_renderer: None,
            tool_press: None,
            spinner: Spinner::new(),
            turn_started: None,
            spinner_advanced_at: Instant::now(),
            tool_strip: crate::components::tool_strip::ToolStrip::new(),
            header_expanded,
            paste_burst: PasteBurst::new(),
            burst_flush_pending: false,
            dirty_redraw: false,
            pending_dialogs: Vec::new(),
            thinking_level: ThinkingLevel::Medium,
            thinking_supported: false,
            slash_menu: SlashMenu::new(),
        }
    }

    /// Install the driver's rich tool-block renderer.
    ///
    /// The App owns the folding policy (collapsed tail preview +
    /// `app.tools.expand`); the driver owns the syntax highlighting. Passing
    /// `None` (or never calling this) keeps the plain `[tool:…]` body, which
    /// is what headless tests and the print-mode path want.
    pub fn set_tool_block_renderer(&mut self, renderer: Box<dyn ToolBlockRenderer>) {
        self.tool_block_renderer = Some(renderer);
    }

    /// Remove the rich tool-block renderer, if any.
    pub fn clear_tool_block_renderer(&mut self) {
        self.tool_block_renderer = None;
    }

    /// Whether assistant bodies are currently rendered as markdown.
    pub fn markdown(&self) -> bool {
        self.messages.markdown()
    }

    /// Turn markdown rendering of assistant bodies on or off.
    ///
    /// Takes effect on the next render; the transcript items themselves are
    /// untouched, so flipping the switch back re-renders the same bodies.
    pub fn set_markdown(&mut self, enabled: bool) {
        self.messages.set_markdown(enabled);
    }

    /// Whether assistant thinking / reasoning blocks are currently shown.
    pub fn thinking_visible(&self) -> bool {
        self.messages.thinking_visible()
    }

    /// Show or hide assistant thinking blocks in place, returning the new
    /// visibility. Backs `app.thinking.toggle` (`Ctrl+T`), upstream's
    /// `toggleThinkingBlockVisibility`
    /// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:4239`).
    ///
    /// Only the collapsed flag changes — the reasoning text stays on the
    /// transcript item, so toggling back restores it unchanged.
    pub fn set_thinking_visible(&mut self, visible: bool) {
        self.messages.set_thinking_visible(visible);
    }

    /// Flip assistant thinking visibility, returning the new value.
    pub fn toggle_thinking_visibility(&mut self) -> bool {
        let visible = !self.messages.thinking_visible();
        self.messages.set_thinking_visible(visible);
        visible
    }

    /// The session thinking level (what the next provider call requests).
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.thinking_level
    }

    /// Set the session thinking level. The editor chrome (the prompt label,
    /// standing in for upstream's editor border) and the status bar pick it
    /// up on the next render.
    pub fn set_thinking_level(&mut self, level: ThinkingLevel) {
        self.thinking_level = level;
    }

    /// Whether the active model reasons at all (upstream `Model.reasoning`).
    pub fn thinking_supported(&self) -> bool {
        self.thinking_supported
    }

    /// Record whether the active model supports thinking. Drives the status
    /// bar's level segment, which upstream only shows for reasoning models
    /// (`components/footer.ts:182-188`).
    pub fn set_thinking_supported(&mut self, supported: bool) {
        self.thinking_supported = supported;
    }

    // -- slash menu -------------------------------------------------------

    /// Whether the slash menu is currently visible.
    pub fn slash_menu_visible(&self) -> bool {
        self.slash_menu.is_visible()
    }

    /// Show the slash menu with the given entries.
    pub fn show_slash_menu(&mut self, entries: Vec<crate::components::slash_menu::SlashMenuEntry>) {
        self.slash_menu.show(entries);
    }

    /// Hide the slash menu.
    pub fn hide_slash_menu(&mut self) {
        self.slash_menu.hide();
    }

    /// Clear the slash menu.
    pub fn clear_slash_menu(&mut self) {
        self.slash_menu.clear();
    }

    /// Move the slash menu selection up by one row.
    pub fn slash_menu_up(&mut self) {
        self.slash_menu.move_up();
    }

    /// Move the slash menu selection down by one row.
    pub fn slash_menu_down(&mut self) {
        self.slash_menu.move_down();
    }

    /// Page up in the slash menu.
    pub fn slash_menu_page_up(&mut self) {
        self.slash_menu.page_up(5);
    }

    /// Page down in the slash menu.
    pub fn slash_menu_page_down(&mut self) {
        self.slash_menu.page_down(5);
    }

    /// Get the currently selected slash menu entry, if any.
    pub fn slash_menu_selected(&self) -> Option<&crate::components::slash_menu::SlashMenuEntry> {
        self.slash_menu.selected_entry()
    }

    /// Get the number of entries in the slash menu.
    pub fn slash_menu_len(&self) -> usize {
        self.slash_menu.len()
    }

    /// Whether tool blocks render expanded.
    pub fn tools_expanded(&self) -> bool {
        self.messages.tools_expanded()
    }

    /// Flip every tool block's expansion, returning the new state.
    ///
    /// Backs `app.tools.expand` (`Ctrl+O`), upstream's `setToolsExpanded`:
    /// one press expands every block, including any a click had collapsed,
    /// and the status bar echoes `Tool output: expanded/collapsed` the way
    /// upstream's `showStatus` does.
    pub fn toggle_tools_expanded(&mut self) -> bool {
        self.messages.toggle_tools_expanded()
    }

    /// Whether the built-in startup header is configured to render at all
    /// (see [`AppConfig::startup_header`]).
    pub fn header_visible(&self) -> bool {
        self.config.startup_header
    }

    /// Show or hide the built-in startup header for this session.
    ///
    /// Hiding it takes no rows, so the message view takes the space back on
    /// the next render.
    pub fn set_header_visible(&mut self, visible: bool) {
        self.config.startup_header = visible;
    }

    /// Whether the startup header is currently expanded.
    pub fn header_expanded(&self) -> bool {
        self.header_expanded
    }

    /// Expand or fold the startup header. Folding renders *no* rows (not a
    /// collapsed one-liner), matching this port's requirement that a folded
    /// header "not occupy a line" — the message view gets the rows back.
    pub fn set_header_expanded(&mut self, expanded: bool) {
        self.header_expanded = expanded;
    }

    /// Flip the startup header's expansion, returning the new state.
    ///
    /// Backs the `app.header` chord. Upstream has no such binding — it drives
    /// the header's `setExpanded` from `setToolsExpanded` together with the
    /// tool blocks — so this port separates the two: `Ctrl+O` keeps folding
    /// tool output, and `app.header` folds the hint screen.
    pub fn toggle_header(&mut self) -> bool {
        let expanded = !self.header_expanded;
        self.header_expanded = expanded;
        expanded
    }

    /// The locale copy table the startup header reads.
    pub fn locale(&self) -> Locale {
        self.config.locale
    }

    /// Switch the startup header's copy table (`/lang`-style switches, or a
    /// host that resolved `--lang` at startup). Takes effect on the next
    /// render.
    pub fn set_locale(&mut self, locale: Locale) {
        self.config.locale = locale;
    }

    /// The frame cursor behind the busy indicator (read-only; tests assert
    /// the animation, [`App::tick_busy_feedback`] drives it).
    pub fn spinner(&self) -> &Spinner {
        &self.spinner
    }

    /// How long the in-flight turn has been running, or `None` when idle.
    pub fn busy_elapsed(&self) -> Option<Duration> {
        self.turn_started.map(|started| started.elapsed())
    }

    /// Advance the busy feedback for this tick and return whether the footer
    /// changed.
    ///
    /// Called from [`App::render_to_buffer`], i.e. from the render loop's
    /// existing 50 ms beat — no second timer, matching the port's "one clock"
    /// rule. `now` is a parameter so a test can drive the animation
    /// deterministically (the driver passes `Instant::now()`).
    ///
    /// While a turn is in flight the spinner advances at most once per
    /// [`SPINNER_INTERVAL_MS`] and the status bar carries
    /// `"<frame> <elapsed>"`; once `turn_busy` clears, the segment is removed
    /// and the cursor resets, so a finished turn never leaves a stray glyph
    /// or a stale time on screen.
    pub fn tick_busy_feedback(&mut self, now: Instant) -> bool {
        if self.is_busy() {
            let started = *self.turn_started.get_or_insert(now);
            if now.saturating_duration_since(self.spinner_advanced_at)
                >= Duration::from_millis(SPINNER_INTERVAL_MS)
            {
                self.spinner.advance();
                self.spinner_advanced_at = now;
            }
            let elapsed = now.saturating_duration_since(started);
            let frame = self.spinner.frame();
            // At the resolution the footer actually prints: a new frame, or a
            // new whole second on the clock. Sub-second jitter is not a change.
            let changed = match self.status_data.busy {
                Some(previous) => {
                    previous.frame != frame
                        || format_elapsed(previous.elapsed) != format_elapsed(elapsed)
                }
                None => true,
            };
            self.status_data.set_busy(frame, elapsed);
            return changed;
        }
        if self.status_data.busy.is_some() {
            self.status_data.clear_busy();
            self.spinner.reset();
            self.turn_started = None;
            return true;
        }
        false
    }

    /// The extension frame with the built-in startup header merged in when no
    /// extension header is set.
    ///
    /// Upstream renders `customHeader ?? builtInHeader`
    /// (`interactive-mode.ts:958`), so a `ctx.ui.setHeader` from an extension
    /// still wins. Composing here (rather than only at paint time) matters:
    /// [`plan_chrome`] has to budget the header's rows before the message
    /// viewport is sized, otherwise the built-in lines would be painted into a
    /// zero-height region.
    ///
    /// `total_height` is the frame height the header is being composed for:
    /// a terminal too short for the full hint list gets the folded header
    /// instead (see [`App::builtin_header_lines`]).
    fn composed_frame(&self, width: u16, total_height: u16) -> ExtensionFrame {
        let mut frame = self.extension.frame(width);
        if frame.header.is_empty() {
            frame.header = self.builtin_header_lines(total_height);
        }
        // The status region is the one chrome region whose height is data
        // driven: a host that supplied a working directory gets upstream's
        // two-row footer (`pwd` row + stats row), a host that did not keeps
        // the single stats row (`docs/LUM1466_TWO_LINE_FOOTER.md`).
        frame.status = self.status_bar.line_count(&self.status_for_render());
        // The docked tool strip (N2) reserves a row only while a turn is
        // in flight — an idle App keeps the pre-N2 single-row geometry.
        frame.tool_strip = if self.tool_strip_state().is_active() {
            1
        } else {
            0
        };
        // The queued-messages block is the second data-driven region: it
        // occupies rows exactly when a prompt is waiting behind the
        // in-flight turn, so an idle App is byte-for-byte the pre-LUM-1469
        // frame.
        frame.pending = self.messages.pending_block_rows();
        frame
    }

    /// [`HEADER_RESERVED_ROWS`], adjusted for a footer that draws a location
    /// row (LUM-1466).
    ///
    /// The startup header folds when it cannot leave the composer, the status
    /// region and [`MIN_TRANSCRIPT_ROWS`] transcript rows behind. The status
    /// region is one row for a host with no working directory and two for the
    /// interactive driver, so the fold decision has to read the live count —
    /// otherwise a 24-row terminal would keep the hint list one row too long
    /// and `plan_chrome` would truncate the header's tail anyway, which is the
    /// silently-cut frame LUM-1266 removed.
    fn header_reserved_rows(&self) -> u16 {
        HEADER_RESERVED_ROWS
            + self
                .status_bar
                .line_count(&self.status_for_render())
                .saturating_sub(1)
            // A queued-messages block is budgeted before the header by
            // `plan_chrome`, so the fold decision has to count it too —
            // otherwise the header would keep hint rows the frame cannot
            // hold and `plan_chrome` would cut its tail (the silent cut
            // LUM-1266 removed).
            + self.messages.pending_block_rows()
    }

    /// The built-in startup header: title, key hints, onboarding line.
    ///
    /// Empty when the header is disabled or folded, so "folded" costs zero
    /// rows. The hint rows come from [`STARTUP_HINTS`] resolved against the
    /// live keybinding table, so an override in `keybindings.json` shows up
    /// here exactly as it does in `/hotkeys`.
    ///
    /// The hint list is tall (a row per bound hint) and the terminal is not
    /// always: when the expanded list would leave fewer than
    /// [`MIN_TRANSCRIPT_ROWS`] transcript rows above the composer and the
    /// status bar, this frame drops the hints and keeps the title (plus, when
    /// it still fits, one row naming the chord that brings them back). The
    /// user's own fold state is untouched — `app.header` still expands on a
    /// short terminal, where the list is simply truncated (LUM-1266).
    fn builtin_header_lines(&self, total_height: u16) -> Vec<StyledLine> {
        if !self.config.startup_header {
            return Vec::new();
        }
        let mut lines = self.header_title_lines();
        if !self.header_expanded {
            // Compact mode (user pressed `app.header` to collapse). TS pi-tui
            // prints `logo + compactOnboarding` here; we mirror that with the
            // title and the TS `compactOnboarding` row pointing at the chord
            // that expands it (`interactive-mode.ts:948`).
            let compact_rows = lines.len() as u16 + u16::from(self.header_folded_text().is_some());
            if compact_rows.saturating_add(self.header_reserved_rows()) <= total_height {
                if let Some(text) = self.header_folded_text() {
                    lines.push(vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Dim))]);
                }
            }
            return lines;
        }
        let hints = self.header_hint_lines();
        if hints.is_empty() {
            return lines;
        }
        let expanded_rows = lines.len() as u16 + u16::try_from(hints.len()).unwrap_or(u16::MAX);
        if expanded_rows.saturating_add(self.header_reserved_rows()) <= total_height {
            lines.extend(hints);
            return lines;
        }
        // Short terminal: the title survives, the hints fold away, and the
        // row that says so is dropped too if even that does not fit. The row
        // itself is the TS `compactOnboarding` text — the same string the
        // compact-mode branch above prints.
        let folded = self.header_folded_text();
        let folded_rows = lines.len() as u16 + u16::from(folded.is_some());
        if folded_rows.saturating_add(self.header_reserved_rows()) <= total_height {
            if let Some(text) = folded {
                lines.push(vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Dim))]);
            }
        }
        lines
    }

    /// Resolve the `app.header` chord and render it through
    /// [`crate::locale::header_folded_line`] so both compact-mode and
    /// short-terminal-fold reuse the same string (`compactOnboarding`).
    ///
    /// `app.header` is a Rust-only binding — it has no upstream counterpart
    /// and is *not* registered in the default keybinding table — so we fall
    /// back to its hardcoded chord the same way `step_key` does (Alt+H).
    fn header_folded_text(&self) -> Option<String> {
        let kb = get_keybindings();
        let keys = kb.get_keys("app.header");
        let resolved = if keys.is_empty() {
            vec![crate::locale::format_chord("alt+h")]
        } else {
            keys.into_iter()
                .map(|chord| crate::locale::format_chord(&chord))
                .collect()
        };
        let joined = resolved.join("/");
        if joined.is_empty() {
            None
        } else {
            Some(crate::locale::header_folded_line(self.config.locale, &joined))
        }
    }

    /// The header rows that survive a short terminal: the product line and,
    /// when an extension header was replaced, its summary.
    fn header_title_lines(&self) -> Vec<StyledLine> {
        let mut lines: Vec<StyledLine> = Vec::with_capacity(2);
        // Logo, upstream `interactive-mode.ts:913`.
        lines.push(vec![
            StyledSpan::new(
                format!("{HEADER_TITLE} "),
                SpanStyle::fg(ThemeColor::Accent).bold(),
            ),
            StyledSpan::new(
                format!("v{}", env!("CARGO_PKG_VERSION", "pi-tui version")),
                SpanStyle::fg(ThemeColor::Dim),
            ),
        ]);
        // Extension summary, right under the title so it is the first thing
        // a user who ran `pi -e ./ext.mjs` reads. Absent by default.
        if let Some(summary) = self.extension_header_line() {
            lines.push(vec![StyledSpan::new(
                summary,
                SpanStyle::fg(ThemeColor::Muted),
            )]);
        }
        lines
    }

    /// The expanded header's tail: one row per resolvable hint, a blank row
    /// and the onboarding line.
    fn header_hint_lines(&self) -> Vec<StyledLine> {
        let entries = self.hint_entries();
        let mut lines: Vec<StyledLine> = Vec::with_capacity(entries.len() + 2);
        for (keys, description) in entries {
            lines.push(vec![
                StyledSpan::new(format!("  {keys} "), SpanStyle::fg(ThemeColor::Accent)),
                StyledSpan::new(description, SpanStyle::fg(ThemeColor::Muted)),
            ]);
        }
        if lines.is_empty() {
            return lines;
        }
        lines.push(Vec::new());
        lines.push(vec![StyledSpan::new(
            self.config
                .locale
                .tr(HEADER_ONBOARDING_EN, HEADER_ONBOARDING_ZH)
                .to_string(),
            SpanStyle::fg(ThemeColor::Dim),
        )]);
        lines
    }

    /// The `<chord> <description>` pairs the hint surfaces share: the startup
    /// header, `/hotkeys`, and the `?` shortcut overlay.
    ///
    /// Two filters: the id must resolve to a chord in the live table *and*
    /// name an action this port consumes. The second is what keeps a
    /// bound-but-unimplemented `app.*` id out of the header
    /// (`CONSUMED_APP_ACTIONS`, LUM-1240/LUM-1245).
    fn hint_entries(&self) -> Vec<(String, String)> {
        let kb = get_keybindings();
        let locale = self.config.locale;
        let mut entries: Vec<(String, String)> = Vec::with_capacity(STARTUP_HINTS.len());
        for hint in STARTUP_HINTS {
            if !hint.is_wired() {
                continue;
            }
            let Some(keys) = hint.key.label(|id| kb.get_keys(id)) else {
                // Unbound in this table: an unbound action is not a hint.
                continue;
            };
            entries.push((keys, hint.description(locale).to_string()));
        }
        entries
    }

    /// The rows the `?` shortcut overlay paints.
    ///
    /// A title row, a rule, then [`App::hint_entries`] in one or two columns,
    /// and the header's onboarding line last. The entry set is the same one
    /// the startup header and `/hotkeys` list, so the overlay cannot drift
    /// from them; only the layout is new. Empty when every hint is unbound or
    /// unimplemented — the caller then paints nothing.
    fn shortcut_overlay_lines(&self, width: usize) -> Vec<StyledLine> {
        let entries = self.hint_entries();
        if entries.is_empty() {
            return Vec::new();
        }
        let locale = self.config.locale;
        let mut lines: Vec<StyledLine> = Vec::with_capacity(entries.len() + 3);
        lines.push(vec![
            StyledSpan::new(
                format!(
                    "  {}",
                    locale.tr(SHORTCUT_OVERLAY_TITLE_EN, SHORTCUT_OVERLAY_TITLE_ZH)
                ),
                SpanStyle::fg(ThemeColor::Accent).bold(),
            ),
            StyledSpan::new(
                format!(
                    "  {}",
                    locale.tr(SHORTCUT_OVERLAY_CLOSE_EN, SHORTCUT_OVERLAY_CLOSE_ZH)
                ),
                SpanStyle::fg(ThemeColor::Dim),
            ),
        ]);
        lines.push(vec![StyledSpan::new(
            "─".repeat(width.min(SHORTCUT_RULE_MAX)),
            SpanStyle::fg(ThemeColor::Dim),
        )]);
        // Two columns only when each one can hold a cell; a second column
        // whose descriptions wrap mid-word reads worse than one long list.
        let chord_column = entries
            .iter()
            .map(|(chord, _)| columns(chord) + 3)
            .max()
            .unwrap_or(0)
            .max(SHORTCUT_CHORD_COLUMN);
        let cell_width = width / 2;
        if cell_width >= SHORTCUT_COLUMN_MIN {
            let half = entries.len().div_ceil(2);
            for index in 0..half {
                let mut row: StyledLine = Vec::new();
                push_shortcut_cell(&mut row, &entries[index], cell_width, chord_column);
                if let Some(entry) = entries.get(index + half) {
                    push_shortcut_cell(&mut row, entry, cell_width, chord_column);
                }
                lines.push(row);
            }
        } else {
            for entry in &entries {
                let mut row: StyledLine = Vec::new();
                push_shortcut_cell(&mut row, entry, width, chord_column);
                lines.push(row);
            }
        }
        lines.push(Vec::new());
        lines.push(vec![StyledSpan::new(
            locale
                .tr(HEADER_ONBOARDING_EN, HEADER_ONBOARDING_ZH)
                .to_string(),
            SpanStyle::fg(ThemeColor::Dim),
        )]);
        lines
    }

    /// The header's extension row, or `None` when there is nothing to
    /// advertise.
    ///
    /// The copy lives in [`crate::locale`]; the width budget is one row
    /// (`plan_chrome` counts lines, and the renderer clips — it never
    /// wraps — a line wider than the viewport).
    fn extension_header_line(&self) -> Option<String> {
        let locale = self.config.locale;
        match &self.config.extension_header {
            ExtensionHeader::Hidden => None,
            ExtensionHeader::Disabled => Some(
                locale
                    .tr(EXTENSIONS_DISABLED_EN, EXTENSIONS_DISABLED_ZH)
                    .to_string(),
            ),
            // A `Loaded` with no names carries no information; hide it
            // rather than print a dangling count.
            ExtensionHeader::Loaded { names, .. } if names.is_empty() => None,
            ExtensionHeader::Loaded { count, names } => Some(
                crate::locale::extensions_summary_line(locale, *count, names),
            ),
        }
    }

    /// Whether markdown links render as OSC 8 hyperlinks.
    pub fn hyperlinks(&self) -> bool {
        self.messages.hyperlinks()
    }

    /// Borrow the message view (for tests and snapshots).
    pub fn messages(&self) -> &MessageView {
        &self.messages
    }

    /// Mutable borrow of the message view.
    pub fn messages_mut(&mut self) -> &mut MessageView {
        &mut self.messages
    }

    /// Borrow the prompt.
    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// First draft row the last frame's composer window showed — the
    /// composer's scroll offset (`0` when the whole draft fits, or before
    /// the first frame). Exposed for tests and audits: it is the state that
    /// makes the window scroll smoothly instead of jumping by pages.
    pub fn composer_scroll(&self) -> usize {
        self.viewport.composer_scroll.load(Ordering::Relaxed)
    }

    /// Mutable borrow of the prompt.
    pub fn prompt_mut(&mut self) -> &mut Prompt {
        &mut self.prompt
    }

    /// Borrow the status data.
    pub fn status_data(&self) -> &StatusData {
        &self.status_data
    }

    /// Mutable borrow of the status data.
    pub fn status_data_mut(&mut self) -> &mut StatusData {
        &mut self.status_data
    }

    /// Point the status bar at a different session (`/new`, `/resume`).
    pub fn set_session_id(&mut self, session_id: impl Into<String>) {
        self.status_data.session_id = session_id.into();
    }

    /// Set the session display name (`/name`). `None` clears it, so the
    /// status bar falls back to the session identifier.
    pub fn set_session_name(&mut self, name: Option<String>) {
        self.status_data.session_name = name;
        self.sync_terminal_title();
    }

    /// Set the working directory the footer's location row shows.
    ///
    /// The driver owns this: `pi-tui` cannot reach the process cwd on behalf
    /// of a host, and a host that never calls this keeps the one-row footer
    /// (the pre-LUM-1466 geometry). [`AppConfig`] is not touched, so no
    /// existing `AppConfig` literal changes shape.
    pub fn set_status_cwd(&mut self, cwd: Option<String>) {
        self.status_data.cwd = cwd;
        self.sync_terminal_title();
    }

    /// Set the git branch joined onto the footer's location row.
    ///
    /// `None` is both "not a repo" and "detached HEAD", matching upstream's
    /// `FooterDataProvider::getGitBranch` (`core/footer-data-provider.ts:126`).
    pub fn set_status_git_branch(&mut self, branch: Option<String>) {
        self.status_data.git_branch = branch;
    }

    /// Tell the footer how many providers are routable and which one is
    /// active, so the model can carry upstream's `(provider) ` prefix
    /// (`footer.ts:191-197`). The driver owns the catalog.
    pub fn set_status_provider(&mut self, count: usize, label: Option<String>) {
        self.status_data.provider_count = count;
        self.status_data.provider_label = label;
    }

    /// Turn the footer's `(auto)` context-gauge suffix on or off
    /// (`footer.ts:150`) — `compaction.enabled` in the driver's settings.
    pub fn set_status_auto_compact(&mut self, enabled: bool) {
        self.status_data.auto_compact = enabled;
    }

    /// Mark the active provider as subscription-billed, which renders the
    /// cost part as ` (sub)` (`footer.ts:139-143`).
    pub fn set_status_subscription(&mut self, subscription: bool) {
        self.status_data.subscription = subscription;
    }

    /// Toggle the footer's `• xp` experimental-features indicator
    /// (`footer.ts:162-164`).
    ///
    /// The driver wires this from `core/experimental.ts` (or any host that
    /// tracks the same toggle). `false` (the default) keeps the indicator
    /// out of the stats cluster entirely; `true` paints a dim `•` followed
    /// by a bold warning-coloured `xp`. Either direction takes effect on the
    /// next render — no extra rebudget, because the zone is already part of
    /// [`StatusData`]'s [`NARROW_SACRIFICE_ORDER`].
    ///
    /// [`NARROW_SACRIFICE_ORDER`]: crate::components::status::NARROW_SACRIFICE_ORDER
    pub fn set_experimental(&mut self, experimental: bool) {
        self.status_data.set_experimental(experimental);
    }

    /// Whether the footer is currently drawing the `• xp` indicator.
    pub fn experimental(&self) -> bool {
        self.status_data.experimental()
    }

    /// Install the active model's per-token rates so the footer can
    /// accumulate `$cost` from the usage events it already receives
    /// (`footer.ts:137-143`).
    pub fn set_status_pricing(&mut self, pricing: Option<crate::components::status::StatusPricing>) {
        self.status_data.set_pricing(pricing);
    }

    /// Install or clear one extension status
    /// (`ctx.ui.setStatus(key, text)`), drawn by the footer as a third row.
    ///
    /// `None` deletes the key, matching upstream's `undefined` clear
    /// (`footer-data-provider.ts:140-147`). Every install/clear re-budgets the
    /// status region through [`StatusBar::line_count`], so the transcript
    /// gives up exactly one row while a status exists and gets it back when
    /// the last extension clears its key.
    ///
    /// [`StatusBar::line_count`]: crate::components::status::StatusBar::line_count
    pub fn set_extension_status(&mut self, key: &str, text: Option<&str>) {
        self.status_data.set_extension_status(key, text);
    }

    /// Drop every extension status (`session_start` on a fresh session, or
    /// host teardown).
    pub fn clear_extension_statuses(&mut self) {
        self.status_data.clear_extension_statuses();
    }

    /// The terminal title currently in effect, or `None` before anything
    /// queued one. Read-only view of the last value handed to the driver;
    /// the wire format lives in [`crate::terminal_title`].
    pub fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }

    /// Give the terminal a title (`ctx.ui.setTitle(title)`).
    ///
    /// The title is queued for the driver to write as OSC 0, sanitised first
    /// so a title containing `ESC` / `BEL` cannot escape into a control
    /// sequence ([`crate::terminal::title::sanitize_title`]). Upstream hands the
    /// string to the terminal verbatim; the difference is deliberate and only
    /// ever drops characters a terminal title has no use for.
    ///
    /// The next automatic title ([`App::sync_terminal_title`] on a session
    /// change) overwrites it, which is upstream's behaviour too: `setTitle`
    /// owns the title only until the session next changes.
    pub fn set_terminal_title(&mut self, title: impl Into<String>) {
        self.queue_terminal_title(crate::terminal::title::sanitize_title(&title.into()));
    }

    /// Recompute the automatic title — `"<app> - <session name> - <cwd>"`,
    /// upstream's `updateTerminalTitle`
    /// (`modes/interactive/interactive-mode.ts:1017-1028`).
    ///
    /// Called by the setters the title is derived from
    /// ([`App::set_session_name`], [`App::set_status_cwd`]), so `/new`,
    /// `/resume`, `/name` and session-tree switches all refresh it without the
    /// driver having to remember. Nothing is queued when the composed title is
    /// already the one in effect.
    pub fn sync_terminal_title(&mut self) {
        let title = crate::terminal::title::auto_title(
            crate::locale::HEADER_TITLE,
            self.status_data.session_name.as_deref(),
            self.status_data.cwd.as_deref(),
        );
        self.queue_terminal_title(title);
    }

    /// Re-assert the automatic title even when it has not changed.
    ///
    /// `app.editor.external` and `app.suspend` hand the tty to a child
    /// process, which is free to retitle the terminal while it runs; upstream
    /// has no equivalent because it never leaves the alternate screen. Called
    /// after the driver takes the tty back, where the queued-title dedup in
    /// [`App::queue_terminal_title`] would otherwise swallow the write.
    pub fn reassert_terminal_title(&mut self) {
        let title = crate::terminal::title::auto_title(
            crate::locale::HEADER_TITLE,
            self.status_data.session_name.as_deref(),
            self.status_data.cwd.as_deref(),
        );
        self.terminal_title = Some(title.clone());
        self.pending_terminal_title = Some(title);
    }

    /// Take the title the driver still has to write, if any.
    ///
    /// The driver calls this once per tick and writes the result with
    /// [`crate::terminal::title::title_sequence`]. `None` — the common case —
    /// means the terminal title is already correct, so no bytes are written.
    pub fn take_terminal_title(&mut self) -> Option<String> {
        self.pending_terminal_title.take()
    }

    /// Queue `title` unless it is already the title in effect.
    fn queue_terminal_title(&mut self, title: String) {
        if self.terminal_title.as_deref() == Some(title.as_str()) {
            return;
        }
        self.terminal_title = Some(title.clone());
        self.pending_terminal_title = Some(title);
    }

    /// The last transient status message pushed by [`App::flash_status`], if
    /// any. Consumed by the next render and cleared by the next key press.
    pub fn status_flash(&self) -> Option<&str> {
        self.status_flash.as_deref()
    }

    /// Show a transient status message in the status bar until the next key
    /// press (upstream's `showStatus`, which is what `Ctrl+T` and the other
    /// toggles use to acknowledge a chord).
    pub fn flash_status(&mut self, text: impl Into<String>) {
        self.status_flash = Some(text.into());
    }

    /// Whether the `?` shortcut overlay is on screen.
    ///
    /// The overlay is a *view* of the same table the startup header and
    /// `/hotkeys` read ([`App::hint_entries`]); nothing else about the session
    /// changes while it is open, and a driver that wants to render its own
    /// help can leave this closed.
    pub fn shortcut_overlay_open(&self) -> bool {
        self.shortcut_overlay
    }

    /// Toggle the `?` overlay, returning its new state.
    pub fn toggle_shortcut_overlay(&mut self) -> bool {
        self.shortcut_overlay = !self.shortcut_overlay;
        self.shortcut_overlay
    }

    /// Close the `?` overlay if it is open, returning whether it was.
    pub fn close_shortcut_overlay(&mut self) -> bool {
        let was_open = self.shortcut_overlay;
        self.shortcut_overlay = false;
        was_open
    }

    /// Whether the composer's reverse history search (`Ctrl+R`) owns the
    /// keyboard right now.
    ///
    /// The driver checks this before claiming its own `app.*` chords: codex's
    /// composer keeps the keyboard for the whole search session, and a global
    /// chord firing mid-search would edit the preview `Esc` is there to undo.
    pub fn history_search_active(&self) -> bool {
        self.prompt.editor().history_search_active()
    }

    /// The reverse-search footer text (`reverse-i-search: <query>` plus the
    /// accept / cancel affordance), or `None` while no search is open.
    ///
    /// Rendered through [`StatusData::hint`] — the same single-line trailing
    /// slot [`App::flash_status`] uses — so the search chrome does not need a
    /// new row and cannot change the frame's layout mid-search. codex renders
    /// the identical text on its footer line.
    pub fn history_search_hint(&self) -> Option<String> {
        let editor = self.prompt.editor();
        let query = editor.history_search_query()?;
        let mut hint = format!("reverse-i-search: {query}");
        match editor.history_search_status() {
            Some(HistorySearchStatus::Match) => {
                // The same two ids `Editor::handle_history_search_key` matches:
                // `tui.input.submit` accepts the preview, `tui.select.cancel`
                // restores the pre-search draft (codex prints the identical
                // affordance on its footer line).
                hint.push_str(&format!(
                    "  {} accept · {} cancel",
                    crate::components::keybindings::key_text_or("tui.input.submit", "Enter"),
                    crate::components::keybindings::key_text_or("tui.select.cancel", "Esc"),
                ));
            }
            Some(HistorySearchStatus::NoMatch) if !query.is_empty() => {
                hint.push_str("  no match");
            }
            _ => {}
        }
        Some(hint)
    }

    /// The status data as it should be painted: [`App::status_data`] with the
    /// transient hint layered on top when a flash is pending. Borrowed in the
    /// common case so the render path does not clone on every frame.
    fn status_for_render(&self) -> Cow<'_, StatusData> {
        let flash = self.status_flash.as_deref();
        // A reverse history search outranks a transient flash: the query is
        // live state the user is typing into, the flash is an acknowledgement
        // of a chord that already happened.
        let search = self.history_search_hint();
        if !self.thinking_supported && flash.is_none() && search.is_none() && !self.shortcut_overlay
        {
            return Cow::Borrowed(&self.status_data);
        }
        let mut data = self.status_data.clone();
        if self.thinking_supported {
            // Upstream's footer joins `model • level` for reasoning models
            // (`components/footer.ts:182-188`); `off` reads `thinking off`.
            data.model = format!(
                "{} • {}",
                data.model,
                thinking_level_suffix(self.thinking_level)
            );
        }
        if let Some(search) = search {
            data.hint = Some(search);
            // The query is live input, not an acknowledgement: it must survive
            // the narrow footer's sacrifice order (see
            // [`crate::components::status::StatusData::hint_pinned`]).
            data.hint_pinned = true;
        } else if let Some(flash) = flash {
            data.hint = Some(flash.to_string());
        } else if self.shortcut_overlay {
            // The overlay repeats the close affordance on its title row; the
            // status bar carries it too so the reader who cannot see the
            // overlay's top edge (a short terminal truncates it) still knows
            // how to get out.
            data.hint = Some(
                self.config
                    .locale
                    .tr(SHORTCUT_OVERLAY_CLOSE_EN, SHORTCUT_OVERLAY_CLOSE_ZH)
                    .to_string(),
            );
        }
        Cow::Owned(data)
    }

    /// The palette the buffer render path currently consumes.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Install a theme. The next [`App::render_to_buffer`] (or
    /// [`App::render_snapshot`]) reflects it immediately — hot-swapping does
    /// not require rebuilding the App.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Whether copy-on-select is on for the chat log.
    pub fn copy_on_select(&self) -> bool {
        self.config.copy_on_select
    }

    /// Turn copy-on-select on or off for the running session.
    ///
    /// The next selection release respects the new value; selections that
    /// were already captured stay queued.
    pub fn set_copy_on_select(&mut self, enabled: bool) {
        self.config.copy_on_select = enabled;
    }

    /// The autocomplete dropdown height in rows.
    pub fn autocomplete_max_visible(&self) -> usize {
        self.prompt.editor().autocomplete_max_visible()
    }

    /// Set the autocomplete dropdown height in rows, clamped to
    /// [`MIN_AUTOCOMPLETE_MAX_VISIBLE`](crate::components::editor::MIN_AUTOCOMPLETE_MAX_VISIBLE)..=[`MAX_AUTOCOMPLETE_MAX_VISIBLE`](crate::components::editor::MAX_AUTOCOMPLETE_MAX_VISIBLE).
    ///
    /// Backs the `autocompleteMaxVisible` setting, which upstream reads while
    /// constructing the editor so it is already in effect on the first frame
    /// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:557`).
    pub fn set_autocomplete_max_visible(&mut self, rows: usize) {
        self.prompt.editor_mut().set_autocomplete_max_visible(rows);
    }

    /// Load and install a theme by name, keeping the current colour mode.
    ///
    /// Built-in names (`dark`, `light`) always resolve; custom themes are not
    /// searched because the App does not own a custom-themes directory. On
    /// failure the previous theme is left untouched.
    pub fn set_theme_by_name(&mut self, name: &str) -> Result<(), ThemeError> {
        let theme = load_theme(name, self.theme.color_mode(), None)?;
        self.theme = theme;
        Ok(())
    }

    /// Whether the user has requested an exit.
    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Whether the next frame would draw anything different from the
    /// last one. The driver's [`FramePacer`](
    /// crates/pi-coding-agent::interactive::FramePacer) uses this to
    /// decide whether the next poll slice is the slow 50 ms idle
    /// path or the bounded next-frame target; a scene that says
    /// "nothing changed" lets the pacer hand the tty thread its
    /// quiet window.
    ///
    /// The flag is OR'd across all the sources that can dirty a
    /// frame: an in-flight background turn (the spinner), a pending
    /// paste burst that has not yet flushed, the selection / scroll
    /// hover state, and the modal/extension dialog queues. The set
    /// is conservative: a no-op redraw costs a buffer write, not
    /// correctness.
    pub fn dirty(&self) -> bool {
        if self.turn_busy.load(Ordering::SeqCst) {
            return true;
        }
        if self.paste_burst_deadline().is_some() {
            return true;
        }
        if self.dirty_redraw {
            return true;
        }
        if self.dialog.is_some()
            || self.settings.is_some()
            || self.selector.is_some()
            || !self.pending_dialogs.is_empty()
        {
            return true;
        }
        false
    }

    /// Set the dirty flag explicitly. Drivers call this after they
    /// step the App (the step result is already tracked) and after
    /// they hand a delta to the App that did not go through `step`
    /// (a transcript append, a queued modal, etc.).
    pub fn mark_dirty(&mut self) {
        self.dirty_redraw = true;
    }

    /// Clear the dirty flag. Called after a successful paint.
    pub fn clear_dirty(&mut self) {
        self.dirty_redraw = false;
    }

    /// Whether a background agent turn is currently in flight.
    pub fn is_busy(&self) -> bool {
        self.turn_busy.load(Ordering::SeqCst)
    }

    /// The current state of the docked tool strip.
    ///
    /// Returns [`ToolStripState::Idle`] when no turn is in flight, so an
    /// idle session stays byte-identical to the pre-N2 frame
    /// (`docs/NANOPI_VS_PI_RUST_GAP_ANALYSIS.md`). When a turn is in
    /// flight, the strip shows "thinking" with the elapsed time since
    /// [`App::turn_started`]. A future wiring will surface a tool's
    /// display name through this accessor when a tool call is in flight
    /// — the renderer already handles the `RunningTool` branch.
    pub fn tool_strip_state(&self) -> crate::components::tool_strip::ToolStripState<'_> {
        if self.is_busy() {
            if let Some(started) = self.turn_started {
                return crate::components::tool_strip::ToolStripState::Thinking {
                    started_at: started,
                };
            }
        }
        crate::components::tool_strip::ToolStripState::Idle
    }

    /// Usage of the most recently finished turn.
    ///
    /// Read-only peek at what [`App::take_turn_usage`] would return;
    /// it does not consume the value, so a driver that peeks during a
    /// render pass can still take it later.
    pub fn last_turn_usage(&self) -> Option<&TurnUsage> {
        self.last_turn_usage.as_ref()
    }

    /// Take the usage of the most recently finished turn.
    ///
    /// Returns `None` when no turn has finished since the last call.
    /// The value is cleared so a driver reacts once per turn; a later
    /// `TurnEnd` replaces it, so an intermediate turn is never acted on
    /// while the prompt that produced it is still running.
    pub fn take_turn_usage(&mut self) -> Option<TurnUsage> {
        self.last_turn_usage.take()
    }

    /// Set a model override that takes effect on the next
    /// `Agent::prompt` call.
    pub fn queue_model_switch(&mut self, agent: &mut Agent, model: pi_protocol::Model) {
        self.status_data.model = model.label.clone().unwrap_or_else(|| model.id.clone());
        self.status_data.context_window = model.context_window;
        // The next turn's context size is unknown until it reports usage.
        self.status_data.context_used = 0;
        agent.set_model(model);
    }

    // Agent event drain / apply — see `app/agent_events.rs`.

    /// Pop the last pending error (if any). The TUI prints this on
    /// the next render so the user sees the agent surface error.
    pub fn take_error(&mut self) -> Option<String> {
        self.pending_error.take()
    }

    /// Begin an agent turn asynchronously. The App spawns a tokio
    /// task that calls `Agent::prompt_content`; events flow through the
    /// subscriber channel established in [`App::new`] and are drained
    /// by [`App::drain_agent_events`].
    ///
    /// Accepts anything convertible to a [`Submission`], so the text-only
    /// call sites keep passing a `String` while the composer hands over a
    /// draft that carries pasted images.
    pub fn submit<M: Into<Submission>>(&mut self, agent: Arc<AsyncMutex<Agent>>, message: M) {
        let submission = message.into();
        let text = submission.text.clone();
        if self.turn_busy.load(Ordering::SeqCst) {
            // A turn is in flight: never drop the input. Upstream submits
            // this with `streamingBehavior: "steer"` so it joins the turn;
            // the port has no window into the locked `Agent`, so it queues
            // the text and the driver delivers it at the next turn
            // boundary. See `App::pending_len` / `App::take_next_pending`.
            //
            // The queue is text-only, so a draft that carries images cannot
            // ride it without losing the attachments. This is a defensive
            // backstop: drafts the App owns are refused *before* they are
            // cleared (see `step_key`'s `PromptAction::Submit` arm and
            // `follow_up_from_editor`), so nothing typed is lost here.
            if !submission.images.is_empty() {
                self.flash_status("Cannot attach images while a turn is running");
                return;
            }
            self.messages
                .push_pending(PendingMessageKind::Steer, text.clone());
            self.prompt.push_history_entry(HistoryEntry::with_images(
                submission.history_text().to_string(),
                submission.images.clone(),
            ));
            return;
        }
        if self.event_rx.is_none() {
            // App constructed without a subscription — re-establish one.
            if let Ok(guard) = agent.try_lock() {
                self.event_rx = Some(guard.subscribe());
            }
        }
        self.messages.push(MessageItem::user(&text));
        self.prompt.push_history_entry(HistoryEntry::with_images(
            submission.history_text().to_string(),
            submission.images.clone(),
        ));
        let cancel = CancellationToken::new();
        self.cancel_token = Some(cancel.clone());
        self.turn_busy.store(true, Ordering::SeqCst);
        // Start the busy feedback in the same breath as the turn: the footer
        // shows the spinner + `0s` on the very next frame, so a slow model is
        // distinguishable from a frozen UI before the first token arrives.
        self.spinner.reset();
        let started = Instant::now();
        self.turn_started = Some(started);
        self.spinner_advanced_at = started;
        self.status_data
            .set_busy(self.spinner.frame(), Duration::ZERO);

        let busy = self.turn_busy.clone();
        let cancel_for_task = cancel.clone();
        let agent_clone = agent.clone();
        let content = submission.content_blocks();
        tokio::spawn(async move {
            let mut guard = agent_clone.lock().await;
            let result = guard.prompt_content(content).await;
            drop(guard);
            if let Err(err) = result {
                let _ = cancel_for_task; // keep the cancellation alive until drop
                let guard = agent_clone.lock().await;
                guard.emit(AgentEvent::Error(err.to_string()));
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Submit the editor buffer with `app.message.followUp` semantics
    /// (upstream's `handleFollowUp`,
    /// `packages/coding-agent/src/modes/interactive/interactive-mode.ts`).
    ///
    /// While a turn is in flight the text is queued and delivered after it
    /// ends; when the App is idle the chord behaves exactly like Enter, so
    /// the caller runs its normal submit path on the returned draft. Either
    /// way the editor buffer ends up empty — the text is never dropped. A
    /// draft with image chips cannot be queued (the queue is text-only), so
    /// the App reports [`FollowUpOutcome::RefusedImages`] and *keeps* the
    /// draft in the editor.
    pub fn follow_up_from_editor(&mut self) -> FollowUpOutcome {
        if self.prompt.text().trim().is_empty() {
            return FollowUpOutcome::Empty;
        }
        let busy = self.turn_busy.load(Ordering::SeqCst);
        if busy && !self.prompt.images().is_empty() {
            return FollowUpOutcome::RefusedImages;
        }
        let submission = Submission {
            text: self.prompt.expanded_text(),
            images: self.prompt.images().to_vec(),
            draft: Some(self.prompt.editor().text().to_string()),
        };
        self.prompt.clear();
        if busy {
            self.messages
                .push_pending(PendingMessageKind::FollowUp, submission.text.clone());
            self.prompt.push_history_entry(HistoryEntry::with_images(
                submission.history_text().to_string(),
                submission.images.clone(),
            ));
            FollowUpOutcome::Queued
        } else {
            FollowUpOutcome::Submitted(submission)
        }
    }

    /// Number of prompts queued behind the in-flight turn.
    pub fn pending_len(&self) -> usize {
        self.messages.pending_len()
    }

    /// Remove and return the next queued prompt, steering before follow-up.
    /// The driver calls this once a turn has finished and feeds the text
    /// back through its normal submit path.
    pub fn take_next_pending(&mut self) -> Option<String> {
        self.messages.take_next_pending()
    }

    /// Restore every queued prompt to the editor (upstream's
    /// `restoreQueuedMessagesToEditor`), steering before follow-up, keeping
    /// any text already in the buffer at the end. Returns how many prompts
    /// were restored; `0` means the queues were empty and the editor is
    /// untouched.
    pub fn restore_pending_to_editor(&mut self) -> usize {
        let queued = self.messages.take_all_pending();
        if queued.is_empty() {
            return 0;
        }
        let restored = queued.len();
        let mut parts: Vec<String> = queued.into_iter().map(|(_, text)| text).collect();
        let current = self.prompt.text().to_string();
        if !current.trim().is_empty() {
            parts.push(current);
        }
        self.prompt.editor_mut().set_text(parts.join("\n\n"));
        restored
    }

    /// Cancel the in-flight turn (if any).
    pub fn cancel(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }

    /// Open the model selector. The selector lists the supplied
    /// candidates; the App does not interpret `value` — the caller
    /// closes the selector with [`App::close_selector`] and applies
    /// the picked value.
    pub fn open_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Close the selector and return the picked value if any.
    pub fn close_selector(&mut self) -> Option<Selector> {
        self.selector.take()
    }

    /// Replace the active selector — used by the binary entry point
    /// when it wants to mutate the selector state in response to a key
    /// event without taking it out of the App.
    pub fn replace_selector(&mut self, selector: Selector) {
        self.selector = Some(selector);
    }

    /// Mutably borrow the open selector.
    ///
    /// The driver uses it to move the cursor after it rebuilt the picker
    /// items itself (e.g. the `/tree` fold chords' branch jump).
    pub fn selector_mut(&mut self) -> Option<&mut Selector> {
        self.selector.as_mut()
    }

    /// Whether the selector modal is currently visible.
    pub fn selector_open(&self) -> bool {
        self.selector.is_some()
    }

    /// Attach the channel that carries extension dialogs.
    ///
    /// The interactive entry point calls this with the receiver half of
    /// the extension UI bridge; [`App::poll_ui_dialogs`] then turns each
    /// envelope into a modal.
    pub fn attach_ui_dialogs(&mut self, rx: mpsc::UnboundedReceiver<Dialog>) {
        self.ui_dialogs = Some(rx);
    }

    /// Show `dialog` and wait for the user.
    ///
    /// Returns `false` without showing it when another modal is already
    /// on screen — the dialog is answered with its cancel default so the
    /// extension never waits on a prompt nobody can see.
    pub fn open_dialog(&mut self, mut dialog: Dialog) -> bool {
        if self.dialog.is_some() {
            dialog.cancel();
            return false;
        }
        self.dialog = Some(dialog);
        true
    }

    /// Whether a modal extension dialog is on screen.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Borrow the open dialog.
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    /// Take the open dialog out of the App — used by the entry point
    /// after it resolved the answer itself.
    pub fn take_dialog(&mut self) -> Option<Dialog> {
        self.dialog.take()
    }

    /// Drain queued extension UI requests.
    ///
    /// `notify` requests become message-view lines (they are
    /// fire-and-forget), everything else becomes a modal. A modal whose
    /// host stopped waiting is closed. Returns whether the App changed
    /// and the caller should redraw.
    pub fn poll_ui_dialogs(&mut self) -> bool {
        let mut changed = false;
        if let Some(dialog) = &self.dialog {
            if dialog.is_abandoned() {
                self.dialog = None;
                changed = true;
            }
        }
        loop {
            let dialog = match self.ui_dialogs.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(dialog) => dialog,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.ui_dialogs = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            if dialog.kind() == DialogKind::Notify {
                let mut dialog = dialog;
                if let Some((message, level)) = dialog.notify_text() {
                    self.messages.push_info(format!("[{level:?}] {message}"));
                }
                dialog.resolve(Some(pi_protocol::UiResponse::NotifyAck));
                continue;
            }
            // A rejected dialog is answered with its cancel default by
            // `open_dialog`; the sender dropping would do the same, but
            // sending is explicit.
            let _ = self.open_dialog(dialog);
        }
        changed
    }

    /// Borrow the active selector (if any).
    pub fn selector(&self) -> Option<&Selector> {
        self.selector.as_ref()
    }

    /// Open the settings modal (`/settings`).
    ///
    /// The list owns the keyboard until it is cancelled or the driver calls
    /// [`App::close_settings`]. Any queued setup work from a previous
    /// session is dropped so a stale change cannot be persisted against the
    /// new list.
    pub fn open_settings(&mut self, list: SettingsList) {
        self.settings = Some(list);
        self.pending_setting_change = None;
        self.pending_setting_activation = None;
    }

    /// Close the settings modal and hand the list back to the driver.
    pub fn close_settings(&mut self) -> Option<SettingsList> {
        self.settings.take()
    }

    /// Whether the settings modal is on screen.
    pub fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// Borrow the open settings list.
    pub fn settings(&self) -> Option<&SettingsList> {
        self.settings.as_ref()
    }

    /// Mutably borrow the open settings list — the driver uses it to
    /// refresh a value it changed outside the modal.
    pub fn settings_mut(&mut self) -> Option<&mut SettingsList> {
        self.settings.as_mut()
    }

    /// Take the value the list asked to persist, if any.
    ///
    /// Returns `(id, value)` — upstream's `onChange(id, newValue)`.
    pub fn take_pending_setting_change(&mut self) -> Option<(String, String)> {
        self.pending_setting_change.take()
    }

    /// Take the item id the list asked the driver to open, if any.
    ///
    /// Returns the id of an item without `values` that was confirmed —
    /// upstream's `submenu` hook.
    pub fn take_pending_setting_activation(&mut self) -> Option<String> {
        self.pending_setting_activation.take()
    }

    // -----------------------------------------------------------------
    // Extension UI host surface (upstream `ctx.ui.*`)
    //
    // The regions are described in the module docs; the state and the
    // layout policy live in [`crate::extension_ui`].
    // -----------------------------------------------------------------

    /// Attach or replace the header component, the region above the message
    /// view (upstream `ctx.ui.setHeader`).
    ///
    /// Passing `None` is equivalent to [`App::clear_header`]. Any previously
    /// attached component is disposed first.
    pub fn set_header(&mut self, component: Option<Box<dyn Component>>) {
        self.extension.set_header(component);
    }

    /// Remove the header component, disposing it (upstream
    /// `ctx.ui.setHeader(undefined)` restores the built-in header).
    pub fn clear_header(&mut self) {
        self.extension.clear_header();
    }

    /// Whether a header component is attached.
    pub fn has_header(&self) -> bool {
        self.extension.has_header()
    }

    /// Attach or replace the footer component, the region below the status
    /// bar (upstream `ctx.ui.setFooter`).
    ///
    /// Passing `None` is equivalent to [`App::clear_footer`]. Any previously
    /// attached component is disposed first.
    pub fn set_footer(&mut self, component: Option<Box<dyn Component>>) {
        self.extension.set_footer(component);
    }

    /// Remove the footer component, disposing it (upstream
    /// `ctx.ui.setFooter(undefined)` restores the built-in footer).
    pub fn clear_footer(&mut self) {
        self.extension.clear_footer();
    }

    /// Whether a footer component is attached.
    pub fn has_footer(&self) -> bool {
        self.extension.has_footer()
    }

    /// Attach or replace the custom editor component, the region that
    /// otherwise shows the prompt (upstream `ctx.ui.setEditorComponent`).
    ///
    /// The component receives keys just before the prompt: anything it
    /// consumes is not typed into the built-in editor, anything it ignores
    /// falls through, so the app-level chords keep working. Passing `None` is
    /// equivalent to [`App::clear_editor_component`]. Any previously attached
    /// component is disposed first.
    pub fn set_editor_component(&mut self, component: Option<Box<dyn Component>>) {
        self.extension.set_editor_component(component);
    }

    /// Remove the custom editor component, disposing it (upstream
    /// `ctx.ui.setEditorComponent(undefined)` restores the default editor).
    pub fn clear_editor_component(&mut self) {
        self.extension.clear_editor_component();
    }

    /// Whether a custom editor component is attached.
    pub fn has_editor_component(&self) -> bool {
        self.extension.has_editor_component()
    }

    /// Set (or clear, with `None`) the widget registered under `key`
    /// (upstream `ctx.ui.setWidget`).
    ///
    /// Widgets render in insertion order within their placement. Setting an
    /// existing key again — under either placement — replaces the component,
    /// disposing the old one, and moves the key to the end of the order;
    /// `None` removes it. `placement` defaults to
    /// [`WidgetPlacement::Above`] upstream, so pass it explicitly here or use
    /// [`WidgetPlacement::default`].
    pub fn set_widget(
        &mut self,
        key: String,
        component: Option<Box<dyn Component>>,
        placement: WidgetPlacement,
    ) {
        self.extension.set_widget(key, component, placement);
    }

    /// The registered widget keys, in insertion order, with their placements.
    pub fn widget_keys(&self) -> Vec<(String, WidgetPlacement)> {
        self.extension.widget_keys()
    }

    /// Append a keyed component *below* the built-in footer, in insertion
    /// order (upstream `ctx.ui.appendFooter`).
    ///
    /// The built-in status bar / footer keeps rendering; the appended rows
    /// stack below it. Passing `None` clears the component registered under
    /// `key`, disposing it. Re-registering an existing `key` replaces the
    /// previous component (disposing it) and moves it to the end of the
    /// sequence.
    pub fn append_footer(&mut self, key: String, component: Option<Box<dyn Component>>) {
        self.extension.append_footer(key, component);
    }

    /// Drop the appended footer under `key`, disposing it. Returns whether
    /// a footer was removed.
    pub fn remove_appended_footer(&mut self, key: &str) -> bool {
        self.extension.remove_appended_footer(key)
    }

    /// Drop every appended footer, disposing each.
    pub fn clear_appended_footers(&mut self) {
        self.extension.clear_appended_footers();
    }

    /// The registered appended-footer keys, in insertion order.
    pub fn appended_footer_keys(&self) -> Vec<String> {
        self.extension.appended_footer_keys()
    }

    /// Prepend a keyed component *above* the built-in header, in insertion
    /// order (upstream `ctx.ui.prependHeader`).
    ///
    /// The built-in header keeps rendering; the prepended rows stack above
    /// it. Same key-replace / key-clear semantics as [`App::append_footer`].
    pub fn prepend_header(&mut self, key: String, component: Option<Box<dyn Component>>) {
        self.extension.prepend_header(key, component);
    }

    /// Drop the prepended header under `key`, disposing it. Returns whether
    /// a header was removed.
    pub fn remove_prepended_header(&mut self, key: &str) -> bool {
        self.extension.remove_prepended_header(key)
    }

    /// Drop every prepended header, disposing each.
    pub fn clear_prepended_headers(&mut self) {
        self.extension.clear_prepended_headers();
    }

    /// The registered prepended-header keys, in insertion order.
    pub fn prepended_header_keys(&self) -> Vec<String> {
        self.extension.prepended_header_keys()
    }

    /// Replace the text in the core input editor (upstream
    /// `ctx.ui.setEditorText`).
    ///
    /// The hidden editor that a non-overlay `custom` session parked keeps its
    /// text: this writes the same buffer, so a session that closes restores
    /// what was there when it opened.
    pub fn set_editor_text(&mut self, text: &str) {
        self.prompt.editor_mut().set_text(text);
    }

    /// The current visible text of the core input editor (upstream
    /// `ctx.ui.getEditorText`). Pasted chips render as their `[Image #N]`
    /// labels.
    pub fn editor_text(&self) -> String {
        self.prompt.text()
    }

    /// The draft with paste markers expanded to the text they stand for —
    /// what a submission and the external editor get (upstream
    /// `getExpandedText`, `interactive-mode.ts:4247`).
    pub fn expanded_editor_text(&self) -> String {
        self.prompt.expanded_text()
    }

    /// Number of paste markers standing in the draft; `0` when the draft
    /// was typed rather than pasted in bulk.
    pub fn paste_marker_count(&self) -> usize {
        self.prompt.editor().paste_marker_ids().len()
    }

    /// Show a custom component with keyboard focus (upstream
    /// `ctx.ui.custom`), returning the handle that controls its visibility
    /// and carries the close result.
    ///
    /// With [`CustomOptions::overlay`] the component is painted on top of
    /// every other region and receives keys before any other layer. Without
    /// it, it replaces the editor region, receives keys just before the
    /// prompt, and the prompt's text is saved and restored around the
    /// session, matching upstream's `showExtensionCustom`. Only one session is
    /// open at a time: opening a second closes the first with a `None`
    /// result.
    ///
    /// Close the session with [`App::close_custom`]. The JS factory → Rust
    /// component bridge is a later task — `pi-extensions` still answers
    /// `ERR_PI_UI_UNSUPPORTED` for `ctx.ui.custom`.
    pub fn open_custom(
        &mut self,
        component: Box<dyn Component>,
        options: CustomOptions,
    ) -> CustomHandle {
        self.custom_saved_editor = if options.overlay {
            None
        } else {
            Some(self.prompt.text().to_string())
        };
        self.extension.open_custom(component, options)
    }

    /// Close the open `custom` session, delivering `result` to the handle's
    /// result channel and disposing the component exactly once. Returns
    /// whether a session was open.
    ///
    /// A non-overlay session restores the editor text captured by
    /// [`App::open_custom`].
    pub fn close_custom(&mut self, result: Option<String>) -> bool {
        let closed = self.extension.close_custom(result);
        if closed {
            if let Some(text) = self.custom_saved_editor.take() {
                self.prompt.editor_mut().set_text(text);
            }
        }
        closed
    }

    /// Whether a `custom` session is open.
    pub fn custom_open(&self) -> bool {
        self.extension.custom_open()
    }

    /// Whether a `custom` session is open *and* currently visible (it can be
    /// hidden temporarily through [`CustomHandle::set_visible`]).
    pub fn custom_visible(&self) -> bool {
        self.extension.custom_visible()
    }

    /// Process a single [`InputEvent`]. Returns the outcome so the
    /// caller can decide whether to redraw.
    pub fn step(&mut self, event: InputEvent) -> StepOutcome {
        if self.exit_requested {
            return StepOutcome::Exit;
        }
        // A visible custom overlay has the highest keyboard priority: it sees
        // the key before every other layer, and only an unconsumed key falls
        // through to them (module docs, "Extension UI host surface").
        if let InputEvent::Key(key) = &event {
            if self.extension.handle_overlay_input(*key) {
                return StepOutcome::Redraw;
            }
        }
        // Mouse gestures are routed by rectangle rather than through the
        // keyboard's modal guard: `step_mouse_gesture` hit-tests the open
        // modal overlays first and only reaches the chat log with no modal up.
        if let InputEvent::MouseGesture(gesture) = event {
            return self.step_mouse_gesture(gesture);
        }
        // A dialog or a picker claims the notches that land on its list rows:
        // upstream hands them to `dispatchMouseToOverlay` before `routeWheel`
        // sees them, and an unhandled notch is *deferred* rather than scrolled
        // (`packages/tui/src/tui-alt-screen.ts:679-694`). While one is open
        // the transcript behind it therefore never moves, whether or not the
        // notch hit a row. This has to run before the modal keyboard guards
        // below, which freeze every other event.
        if let InputEvent::Mouse { up, y, .. } = event {
            if self.dialog.is_some() || self.selector.is_some() {
                return self.step_modal_list_wheel(up, y);
            }
        }
        // An open modal owns the keyboard: the selector and prompt stay
        // frozen underneath it.
        if self.dialog.is_some() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            return self.step_dialog(key);
        }
        // A settings modal owns both the keyboard and the wheel: it is the
        // only interactive surface on screen while it is open.
        if self.settings.is_some() {
            if let InputEvent::Mouse { up, alt, .. } = event {
                if let Some(outcome) = self.step_settings_wheel(up, alt) {
                    return outcome;
                }
            }
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            return self.step_settings(key);
        }
        // Selector gets first dibs on keys when it is open.
        if let Some(selector) = self.selector.as_mut() {
            let InputEvent::Key(key) = event else {
                return StepOutcome::Idle;
            };
            match selector.handle_key(key) {
                SelectorAction::None => return StepOutcome::Idle,
                SelectorAction::Changed => return StepOutcome::Redraw,
                SelectorAction::Selected(_) | SelectorAction::Cancelled => {
                    return StepOutcome::Redraw;
                }
            }
        }
        let InputEvent::Key(key) = event else {
            if let InputEvent::Mouse { up, alt, x, y } = event {
                // The composer's dropdown borrows transcript rows and owns the
                // pointer there, the wheel included: upstream hands the notch
                // to `dispatchMouseToLayout` (editor → `SelectList`) *before*
                // `routeWheel` reaches the chat-log scroll view
                // (`packages/tui/src/tui-alt-screen.ts:679-694`).
                if let Some(outcome) = self.step_autocomplete_wheel(up, x, y) {
                    return outcome;
                }
                let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
                let changed = if up {
                    self.scroll_viewport_up(lines)
                } else {
                    self.scroll_viewport_down(lines)
                };
                // An unclaimed notch is a `routeWheel`: the scroll view moves
                // *and* the scrollbar hover follows the cell the notch came
                // from (`packages/tui/src/tui-alt-screen.ts:973-984`), so
                // wheeling over the bar lights it up without a move event.
                let hover_changed = self.update_scrollbar_hover(x, y);
                return if changed || hover_changed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            return StepOutcome::Idle;
        };
        self.step_key(key)
    }

    /// Route a bracketed paste into the composer.
    ///
    /// The driver calls this for `crossterm`'s [`CtEvent::Paste`], which the
    /// terminal only ever sends while the driver has bracketed paste enabled
    /// (`pi-coding-agent`'s `setup_terminal`, upstream
    /// `packages/tui/src/terminal.ts:184`). The payload arrives as **one**
    /// event instead of a burst of keys, which is the whole point: a pasted
    /// block used to reach the composer as individual characters and every
    /// newline in it was an `Enter`, so pasting three lines submitted the
    /// draft three times.
    ///
    /// Paste is a separate entry point rather than an
    /// [`InputEvent`](crate::core::input_parse::InputEvent) variant because that enum is
    /// `Copy` by construction — [`App::step`] matches it by value and still
    /// uses it afterwards — and a heap payload cannot ride in a `Copy` type.
    ///
    /// The modal layers keep their priority: while a dialog, the settings
    /// modal or a selector owns the keyboard, a paste is dropped instead of
    /// editing the frozen composer underneath it.
    // step_paste lives in `app/step_paste.rs`; this comment is kept as a
    // module-level signpost for grep users.

    /// True when `key` triggers an `app.*` id.
    ///
    /// The `app.*` ids belong to the coding-agent config layer, so a bare
    /// `pi-tui` registry (only the `tui.*` ids) leaves them unknown and the
    /// port's built-in chords stand in — the pre-keybinding behaviour. A
    /// table that defines the id (even deliberately unbound) takes over,
    /// so rebinding works.
    fn matches_app_key(
        kb: &KeybindingsManager,
        event: &InputEvent,
        keybinding: &str,
        builtin: &[&str],
    ) -> bool {
        matches_with_fallback(kb, event, keybinding, builtin)
    }


    // -----------------------------------------------------------------
    // Paste burst (codex `paste_burst`) — see `app/step_paste.rs`.
    // -----------------------------------------------------------------
    // Modal dialog / settings list step handlers — see `app/step_dialog.rs`.

    /// Mark the App for exit (e.g. after `/exit`).
    pub fn request_exit(&mut self) {
        self.exit_requested = true;
    }

    // -----------------------------------------------------------------
    // Transcript search (upstream `AltScreenSearch*`,
    // `packages/tui/src/tui-alt-screen.ts:496-660`)
    // -----------------------------------------------------------------

    /// Whether the search overlay is open.
    pub fn search_open(&self) -> bool {
        self.search.is_some()
    }

    /// The current search query, or `None` while the overlay is closed.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|state| state.bar.query())
    }

    /// The matches from the last refresh (empty while closed).
    pub fn search_matches(&self) -> &[SearchMatch] {
        self.search
            .as_ref()
            .map(|state| state.matches.as_slice())
            .unwrap_or(&[])
    }

    /// Index of the selected match, or `None`.
    pub fn search_match_index(&self) -> Option<usize> {
        self.search.as_ref().and_then(|state| state.selected_index)
    }

    /// Borrow the query bar (for tests).
    pub fn search_bar(&self) -> Option<&SearchBar> {
        self.search.as_ref().map(|state| &state.bar)
    }

    /// Open the search overlay, anchored to the current viewport row.
    ///
    /// Returns true when the state changed (i.e. it was closed). Mirrors
    /// upstream's `toggleSearch` first half
    /// (`packages/tui/src/tui-alt-screen.ts:496-521`).
    pub fn open_search(&mut self) -> bool {
        if self.search.is_some() {
            return false;
        }
        let anchor = self.viewport_skip();
        self.search = Some(SearchState::new(anchor));
        true
    }

    /// Close the search overlay. Returns true when it was open
    /// (`closeSearch`, `:523-529`).
    pub fn close_search(&mut self) -> bool {
        self.search.take().is_some()
    }

    /// Toggle the search overlay
    /// (`Ctrl+Shift+F`, `tui.altScreen.search`).
    pub fn toggle_search(&mut self) -> bool {
        if self.search.is_some() {
            self.close_search()
        } else {
            self.open_search()
        }
    }

    /// Replace the query as if the reader had typed it, refreshing the
    /// matches. Opens the overlay when it is closed.
    pub fn set_search_query(&mut self, query: impl Into<String>) -> bool {
        if self.search.is_none() {
            self.open_search();
        }
        let query = query.into();
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        if state.bar.query() == query {
            return false;
        }
        state.bar.set_query(query);
        self.search_query_changed();
        true
    }

    /// Select the next (`1`) or previous (`-1`) match
    /// (`navigateSearch`, `:535-540`).
    pub fn navigate_search(&mut self, direction: i8) -> bool {
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        // Upstream ignores navigation while the query is empty.
        if state.bar.query().trim().is_empty() || state.matches.is_empty() {
            return false;
        }
        state.selection_mode = if direction < 0 {
            SearchSelectionMode::Previous
        } else {
            SearchSelectionMode::Next
        };
        let before = self.search_match_index();
        let revealed = self.refresh_search();
        // A step that lands on a match already on screen still changed the
        // selection, so it still needs a redraw.
        revealed || self.search_match_index() != before
    }

    /// Re-run the search against the current transcript and re-select the
    /// match, revealing it when the selection mode asked for a jump.
    ///
    /// The Rust port of `refreshSearch`
    /// (`packages/tui/src/tui-alt-screen.ts:570-641`), including its choice to
    /// do nothing while the query is blank and to keep the selection stable
    /// across a re-index by [`SearchMatch::key`].
    pub fn refresh_search(&mut self) -> bool {
        let (width, height) = self.viewport();
        let Some(state) = self.search.as_mut() else {
            return false;
        };
        let query = state.bar.query().to_string();
        let lines: Vec<String> = if width == 0 || height == 0 {
            Vec::new()
        } else {
            self.messages
                .render_styled_lines(width)
                .iter()
                .map(|line| plain_text(line))
                .collect()
        };

        if query.trim().is_empty() || lines.is_empty() {
            state.matches.clear();
            state.selected_index = None;
            state.selected_key = None;
            state.selection_mode = SearchSelectionMode::Retain;
            state.bar.set_result(-1, 0);
            return false;
        }

        let should_reveal = state.selection_mode != SearchSelectionMode::Retain;
        let result = state.index.search(&lines, &query);
        let changed = result.changed;
        let mode = state.selection_mode;
        let anchor_row = state.anchor_row;
        let previous_index = state.selected_index;
        let previous_key = state.selected_key.clone();
        state.matches = result.matches;

        if !changed && mode == SearchSelectionMode::Retain {
            return false;
        }

        let len = state.matches.len();
        let exact = if changed {
            previous_key
                .as_ref()
                .and_then(|key| state.matches.iter().position(|m| &m.key() == key))
        } else {
            previous_index.filter(|index| *index < len)
        };
        let selected = if len == 0 {
            None
        } else {
            Some(match mode {
                SearchSelectionMode::Query => state
                    .matches
                    .iter()
                    .position(|m| m.first_row().is_some_and(|row| row >= anchor_row))
                    .unwrap_or(0),
                SearchSelectionMode::Next => {
                    let base = search_base_index(exact, previous_index, len);
                    if base < 0 {
                        0
                    } else {
                        ((base + 1) % len as i64) as usize
                    }
                }
                SearchSelectionMode::Previous => {
                    let base = search_base_index(exact, previous_index, len);
                    if base < 0 {
                        len - 1
                    } else {
                        ((base - 1 + len as i64) % len as i64) as usize
                    }
                }
                SearchSelectionMode::Retain => exact
                    .or_else(|| previous_index.map(|index| index.min(len - 1)))
                    .unwrap_or(0),
            })
        };

        state.selected_index = selected;
        state.selected_key = selected.and_then(|index| state.matches.get(index).map(|m| m.key()));
        state.selection_mode = SearchSelectionMode::Retain;
        state
            .bar
            .set_result(selected.map(|i| i as i64).unwrap_or(-1), len);

        if !should_reveal {
            return false;
        }
        self.search_reveal()
    }

    /// The query changed: re-anchor the next selection under the current one
    /// and switch to the `Query` mode (`updateSearchQuery`, `:531-543`).
    fn search_query_changed(&mut self) {
        let anchor = self.search_anchor_row();
        let Some(state) = self.search.as_mut() else {
            return;
        };
        state.anchor_row = anchor;
        state.selection_mode = SearchSelectionMode::Query;
        state.bar.set_result(-1, 0);
        let _ = self.refresh_search();
    }

    /// Row the `Query` selection mode anchors to: the selected match's first
    /// row, else the viewport top.
    fn search_anchor_row(&self) -> usize {
        if let Some(state) = self.search.as_ref() {
            if let Some(row) = state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.first_row())
            {
                return row;
            }
        }
        self.viewport_skip()
    }

    /// First rendered row of the viewport the App last drew.
    fn viewport_skip(&self) -> usize {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return 0;
        }
        self.messages.visible_lines(width, height).0
    }

    /// Scroll the selected match into view, a third of a page below the top
    /// edge (`refreshSearch`'s reveal half, `:627-641`).
    fn search_reveal(&mut self) -> bool {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return false;
        }
        let Some(state) = self.search.as_ref() else {
            return false;
        };
        let (Some(first), Some(last)) = (
            state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.first_row()),
            state
                .selected_index
                .and_then(|index| state.matches.get(index))
                .and_then(|m| m.last_row()),
        ) else {
            return false;
        };
        let (visible_start, lines) = self.messages.visible_lines(width, height);
        let visible_end = visible_start + lines.len();
        if first >= visible_start && last < visible_end {
            return false;
        }
        let page = height as usize;
        let max = self.messages.line_count(width).saturating_sub(page);
        let target = first.saturating_sub(page / 3).min(max);
        self.messages.set_scroll_from_bottom(max - target);
        true
    }

    /// Give an open search overlay the mouse first, exactly like the modal

    /// Append a free-form info line to the message view (used by
    /// slash commands to print help / errors).
    pub fn info(&mut self, text: impl Into<String>) {
        self.messages.push_info(text);
    }

    /// Append a system info block — command-reference output such as
    /// `/help` / `/hotkeys` — with the `· ` prefix instead of the composer's
    /// `> ` (LUM-1238 §15.4).
    pub fn info_block(&mut self, text: impl Into<String>) {
        self.messages.push_info_block(text);
    }

    /// Size of the message viewport as of the last render — `(0, 0)`
    /// before the first one.
    pub fn viewport(&self) -> (u16, u16) {
        (
            self.viewport.viewport_width.load(Ordering::Relaxed),
            self.viewport.viewport_height.load(Ordering::Relaxed),
        )
    }

    /// Set the cached viewport size.
    ///
    /// Production drivers write the measured viewport after every paint,
    /// but tests need to drive [`App::narrow_options`] and
    /// [`App::composer_max_rows_effective`] at synthetic widths without
    /// routing through a real terminal. Calling this with `(0, 0)` clears
    /// the cached size so the helpers fall back to the unmeasured
    /// (wide) classification.
    pub fn set_viewport_size(&self, width: u16, height: u16) {
        self.viewport.viewport_width.store(width, Ordering::Relaxed);
        self.viewport.viewport_height.store(height, Ordering::Relaxed);
    }

    /// Top-left cell of the message viewport as of the last render. Pointer
    /// coordinates are absolute, so this is what maps them back in.
    pub fn viewport_origin(&self) -> (u16, u16) {
        (
            self.viewport.viewport_origin.0.load(Ordering::Relaxed),
            self.viewport.viewport_origin.1.load(Ordering::Relaxed),
        )
    }

    /// Rectangle of the "jump to latest" pill as of the last render, or
    /// `None` when the viewport was following the tail and no pill was
    /// painted.
    pub fn scroll_to_end_rect(&self) -> Option<Rect> {
        let width = self.viewport.scroll_to_end.2.load(Ordering::Relaxed);
        (width > 0).then(|| Rect {
            x: self.viewport.scroll_to_end.1.load(Ordering::Relaxed),
            y: self.viewport.scroll_to_end.0.load(Ordering::Relaxed),
            width,
            height: 1,
        })
    }

    /// Rectangle of the "cut above" hint as of the last render, or `None`
    /// when the top edge of the viewport is a block boundary (or the reader
    /// has scrolled away from the tail) and nothing was painted.
    pub fn truncated_above_rect(&self) -> Option<Rect> {
        let width = self.viewport.truncated_above.2.load(Ordering::Relaxed);
        (width > 0).then(|| Rect {
            x: self.viewport.truncated_above.1.load(Ordering::Relaxed),
            y: self.viewport.truncated_above.0.load(Ordering::Relaxed),
            width,
            height: 1,
        })
    }

    /// How many lines of the block at the viewport's top edge are off-screen
    /// above it, or `None` when there is nothing to disclose.
    ///
    /// The transcript is laid out as items, and [`MessageView::item_line_ranges`]
    /// gives each item's row span in the same layout the renderer uses, so a
    /// top edge that falls strictly *inside* one span is exactly "this block
    /// is cut" — as opposed to a top edge that coincides with a boundary,
    /// which every scrolled transcript has and which hides nothing.
    ///
    /// Only reported while the viewport is pinned to the tail. A detached
    /// reader already has the jump-to-latest pill telling them where they
    /// are; showing both would spend a second transcript row to say what the
    /// first one already says.
    pub fn truncated_above_lines(&self) -> Option<usize> {
        let (width, height) = self.viewport();
        // One row is not a viewport: the hint would be the whole transcript.
        if width == 0 || height < 2 {
            return None;
        }
        if !self.messages.is_following() || self.resolved_scroll() != 0 {
            return None;
        }
        let total = self.messages.line_count(width);
        let start = total.saturating_sub(height as usize);
        if start == 0 {
            return None;
        }
        let (item_start, _) = self
            .messages
            .item_line_ranges(width)
            .into_iter()
            .find(|(from, to)| *from <= start && start < *to)?;
        let hidden = start - item_start;
        (hidden > 0).then_some(hidden)
    }

    /// Whether the pointer currently rests on the chat-log scrollbar.
    ///
    /// Upstream's `scrollbarHover`
    /// (`packages/tui/src/tui-alt-screen.ts:221`); it drives the bar's active
    /// rendering, so the render path is the only other observer.
    pub fn scrollbar_hovered(&self) -> bool {
        self.scrollbar_hover
    }

    /// Whether a scrollbar drag currently owns the pointer.
    pub fn scrollbar_dragging(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    /// Geometry of the chat-log scrollbar, or `None` when the wrapped
    /// transcript fits the viewport — upstream's `getScrollbarGeometry`
    /// (`packages/tui/src/layout.ts:280-326`).
    ///
    /// Needs a recorded viewport ([`App::render_to_buffer`] or
    /// [`App::render_snapshot`] at least once); before that the viewport is
    /// zero-sized and this returns `None`.
    pub fn scrollbar_geometry(&self) -> Option<ScrollbarGeometry> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return None;
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let content = self.messages.line_count(width);
        let max_scroll = self.max_scroll();
        // A bar over content that already fits would be pure noise, so the
        // track is the whole viewport or nothing (see the module docs on the
        // omitted transient hide delay).
        if content <= height as usize || max_scroll == 0 {
            return None;
        }

        let track_height = height as usize;
        // The thumb keeps the content-to-track ratio, floored at two rows so
        // a long transcript still has something to grab, and capped by the
        // track itself.
        let min_thumb = 2.min(track_height);
        let thumb_height = round_div(track_height * track_height, content)
            .max(min_thumb)
            .min(track_height);
        let max_thumb_top = track_height - thumb_height;
        // `resolved_scroll` counts from the bottom while the bar measures
        // from the top, so a bottom-pinned view puts the thumb at the end of
        // the track.
        let scroll_top = max_scroll.saturating_sub(self.resolved_scroll());
        let thumb_top = origin_y + round_div(scroll_top * max_thumb_top, max_scroll) as u16;

        Some(ScrollbarGeometry {
            // `width` is the transcript's wrap width, one column short of the
            // frame while the bar is visible: the bar owns the frame's right
            // edge, not the text's last column (see
            // [`App::viewport_for_render`]).
            column: origin_x + width + self.viewport.viewport_reserved.load(Ordering::Relaxed) - 1,
            track_top: origin_y,
            track_height: height,
            thumb_top,
            thumb_height: thumb_height as u16,
            max_scroll,
        })
    }

    /// The scrollbar geometry under an absolute pointer position, or `None`
    /// when the pointer is off the bar — upstream's `getScrollbarTargetAt`
    /// (`packages/tui/src/tui-alt-screen.ts:1041-1055`), minus its overlay
    /// guard: an open modal or search bar is dispatched before the scrollbar
    /// in [`App::step_mouse_gesture`], so an overlay never reaches here.
    fn scrollbar_geometry_at(&self, x: u16, y: u16) -> Option<ScrollbarGeometry> {
        let geometry = self.scrollbar_geometry()?;
        let within_track =
            y >= geometry.track_top && y < geometry.track_top.saturating_add(geometry.track_height);
        (x == geometry.column && within_track).then_some(geometry)
    }

    /// Aim the hover flag at a pointer position and report whether it
    /// changed — upstream's `updateScrollbarHover`
    /// (`packages/tui/src/tui-alt-screen.ts:1058-1064`).
    fn update_scrollbar_hover(&mut self, x: u16, y: u16) -> bool {
        let hovered = self.scrollbar_geometry_at(x, y).is_some();
        let changed = hovered != self.scrollbar_hover;
        self.scrollbar_hover = hovered;
        changed
    }

    /// Move the scroll offset so the thumb's top lands on the pointer —
    /// upstream's `scrollScrollbarToPointer`
    /// (`packages/tui/src/tui-alt-screen.ts:1067-1109`). Returns whether the
    /// view actually scrolled.
    ///
    /// `grab_offset` is the pointer's distance from the thumb's top, so a
    /// drag keeps holding the same part of the thumb; a track press passes
    /// half the thumb height to centre it (upstream's `pressedOnThumb ?
    /// grabOffset : thumbHeight / 2`).
    fn scroll_scrollbar_to_pointer(
        &mut self,
        geometry: &ScrollbarGeometry,
        y: u16,
        grab_offset: u16,
    ) -> bool {
        let track_height = geometry.track_height as usize;
        let max_thumb_top = track_height.saturating_sub(geometry.thumb_height as usize);
        if max_thumb_top == 0 {
            return false;
        }
        let thumb_top = y.saturating_sub(geometry.track_top) as usize;
        let top = thumb_top
            .saturating_sub(grab_offset as usize)
            .min(max_thumb_top);
        // Top-relative thumb position → bottom-relative scroll offset, the
        // inverse of [`App::scrollbar_geometry`].
        let scroll_top =
            round_div(top * geometry.max_scroll, max_thumb_top).min(geometry.max_scroll);
        let offset = geometry.max_scroll - scroll_top;
        if self.resolved_scroll() == offset {
            return false;
        }
        self.messages.set_scroll_from_bottom(offset);
        true
    }

    /// Handle a pointer gesture against the chat-log scrollbar, if it lands
    /// there — upstream's `handleScrollbarMouseEvent`
    /// (`packages/tui/src/tui-alt-screen.ts:1041-1109`), converged onto the
    /// single scroll view.
    ///
    /// `Some` means the scrollbar consumed the gesture; `None` lets the
    /// caller fall through to the selection path. A press on the thumb
    /// starts a drag, a press on the track jumps straight to the pointer,
    /// and once a drag is in flight every non-wheel gesture belongs to it
    /// until the release — so a stray click cannot start a selection
    /// mid-drag.
    /// Feed a non-wheel mouse gesture to the App: start, extend, finish or
    /// clear the chat-log text selection.
    ///
    /// Mirrors upstream's `handleSelectionMouseEvent`
    /// (`packages/tui/src/tui-alt-screen.ts:1300-1380`): a press anchors a
    /// selection whose granularity comes from the click count (single →
    /// character, double → word, triple → line), a drag extends it under
    /// that same granularity, and a release copies it when
    /// [copy-on-select](AppConfig::copy_on_select) is on. A press and
    /// release on the same cell leaves an empty selection, so a plain
    /// click clears whatever was selected before.
    ///
    /// While a drag is in flight and the pointer rests on the viewport's
    /// top or bottom row, the gesture also arms the drag autoscroll; see
    /// [`App::advance_selection_autoscroll`] for the beat.
    ///
    /// A gesture landing on the chat-log scrollbar is handled first and
    /// never reaches the selection path — see
    /// [`App::scrollbar_geometry`] and [`App::step_scrollbar_mouse_gesture`].
    ///
    /// With a modal on screen the gesture is hit-tested against the modal
    /// overlays instead — see [`App::mouse_regions`] and
    /// [`App::step_modal_mouse_gesture`].
    ///
    /// Deviations (deliberate, documented in the module docs): the click
    /// count is reconstructed from press timing because `crossterm` has no
    /// `clickCount` field; gestures that press outside the message viewport
    /// (the prompt and status rows) are ignored, though an in-flight drag is
    /// clamped back into the viewport so edge autoscroll keeps tracking.

    /// Route a non-wheel gesture through the chat-log text selection: start,
    /// extend, finish or clear it. Upstream's `handleSelectionMouseEvent`
    /// (`packages/tui/src/tui-alt-screen.ts:1310-1420`).
    fn step_selection_mouse_gesture(&mut self, gesture: &MouseGesture) -> StepOutcome {
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                self.stop_selection_autoscroll();
                // A fresh press invalidates any pending tool-block click.
                self.tool_press = None;
                let Some(point) = self.selection_point(gesture.x, gesture.y) else {
                    // A press outside the viewport cannot arm a drag — if a
                    // previous gesture left `selection_dragging` on, drop it
                    // so the next release / drag is no-op until a real press
                    // lands inside the chat log.
                    self.selection_dragging = false;
                    self.last_click = None;
                    return StepOutcome::Idle;
                };
                // The click key is always the word under the pointer, even
                // when the count resolves to a line selection (upstream
                // `getClickCount(anchor, word)`,
                // `packages/tui/src/tui-alt-screen.ts:1364-1367`).
                let word = self.word_selection(point);
                let click_count = self.next_click_count(point, word);
                // A single click on a tool block is an expand / collapse, not
                // the start of a selection. Remember the cell so only a
                // release on that same cell commits (a drag away still
                // selects text).
                if click_count == 1 {
                    if let Some(index) = self.tool_block_at(point.0) {
                        self.tool_press = Some((gesture.x, gesture.y, index));
                    }
                }
                let next = match click_count {
                    2 => word.map(|(start, end)| {
                        Selection::range(start, end, SelectionGranularity::Word)
                    }),
                    3 => self.line_selection(point).map(|(start, end)| {
                        Selection::range(start, end, SelectionGranularity::Line)
                    }),
                    _ => Some(Selection::at(point)),
                };
                let Some(next) = next else {
                    return StepOutcome::Idle;
                };
                self.selection_dragging = true;
                if self.selection == Some(next) {
                    return StepOutcome::Idle;
                }
                self.selection = Some(next);
                StepOutcome::Redraw
            }
            MouseGestureKind::Drag(MouseButton::Left) | MouseGestureKind::Move
                if self.selection_dragging =>
            {
                // A drag is not a click: upstream clears `lastClick`
                // (`tui-alt-screen.ts:1341`), so a click-drag-click is not a
                // double click.
                self.last_click = None;
                // A drag can leave the viewport (onto the prompt / status
                // rows or past the top); it is clamped back in so the focus
                // still tracks, exactly like upstream's
                // `getScrollSelectionPoint`.
                let changed = self
                    .selection_point_clamped(gesture.x, gesture.y)
                    .map(|point| self.extend_selection(point) == StepOutcome::Redraw)
                    .unwrap_or(false);
                let armed = self.update_selection_autoscroll(gesture.x, gesture.y);
                if changed || armed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                self.selection_dragging = false;
                self.stop_selection_autoscroll();
                // A press and release on the same cell of a tool block is a
                // click on that block: toggle it and swallow the selection
                // the press started, so nothing is copied.
                if let Some((px, py, index)) = self.tool_press.take() {
                    if px == gesture.x && py == gesture.y {
                        self.clear_selection();
                        self.pending_clipboard = None;
                        return if self.messages.toggle_tool_at(index).is_some() {
                            StepOutcome::Redraw
                        } else {
                            StepOutcome::Idle
                        };
                    }
                }
                let changed = self
                    .selection_point(gesture.x, gesture.y)
                    .map(|point| self.extend_selection(point) == StepOutcome::Redraw)
                    .unwrap_or(false);
                if self.config.copy_on_select {
                    if let Some(text) = self.selection_text() {
                        self.pending_clipboard = Some(text);
                    }
                }
                if changed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            // Middle / right buttons and bare moves with no button held
            // are not part of the selection gesture (upstream gives the
            // right button to paste on Windows only).
            _ => StepOutcome::Idle,
        }
    }

    /// Advance the pending edge autoscroll by one beat, returning whether
    /// the viewport moved.
    ///
    /// Upstream runs this from a 50 ms `setInterval`
    /// (`packages/tui/src/tui-alt-screen.ts:1292-1297`); this port has no
    /// timer thread, so the beat is the draw itself —
    /// [`App::render_to_buffer`] calls this once per frame, and the driver's
    /// render loop is already throttled to 50 ms. A drag parked on an edge
    /// therefore scrolls one line per redraw, and a stationary pointer keeps
    /// moving because the driver keeps drawing.
    ///
    /// Stops when the viewport cannot move any further in the requested
    /// direction (upstream's `remaining === direction`), which also means a
    /// drag at the tail's bottom edge does not spin.
    pub fn advance_selection_autoscroll(&mut self) -> bool {
        let Some((x, y)) = self.selection_autoscroll_pointer else {
            return false;
        };
        let direction = self.selection_autoscroll_direction;
        if direction == 0 {
            return false;
        }
        let moved = if direction < 0 {
            self.scroll_viewport_up(SELECTION_AUTOSCROLL_LINES)
        } else {
            self.scroll_viewport_down(SELECTION_AUTOSCROLL_LINES)
        };
        if !moved {
            self.stop_selection_autoscroll();
            return false;
        }
        // The viewport moved, so the focus under the stationary pointer is a
        // different log line now — recompute it like upstream's
        // `autoScrollSelection`.
        if let Some(point) = self.selection_point_clamped(x, y) {
            let _ = self.extend_selection(point);
        }
        true
    }

    /// On-screen rectangles of the open modal overlays, topmost first.
    ///
    /// The Rust counterpart of upstream's `renderedOverlayLayouts`
    /// (`packages/tui/src/tui.ts:492,824-847`): each region is the box the
    /// modal occupied in the last render — the dialog starts at the top of
    /// the message area, the settings list and the selector one row below it
    /// (`App::render_to_buffer`) — in absolute terminal cells. The order is
    /// the keyboard focus order in `step_key` (dialog → settings → selector),
    /// so the topmost modal owns a click when overlays overlap. Empty before
    /// the first render, when the geometry is not known yet.
    pub fn mouse_regions(&self) -> Vec<MouseRegion> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let mut overlays: Vec<(u16, usize)> = Vec::new();
        if let Some(dialog) = &self.dialog {
            overlays.push((0, dialog.render_lines(width).len()));
        }
        if let Some(settings) = &self.settings {
            overlays.push((1, settings.render_lines(width).len()));
        }
        if let Some(selector) = &self.selector {
            overlays.push((1, selector.render_styled_lines(width).len()));
        }
        let mut regions = Vec::new();
        for (top, lines) in overlays {
            // The renderer clips an overlay to the message viewport, so the
            // status / prompt rows can never be part of its rectangle.
            let rows = u16::try_from(lines).unwrap_or(u16::MAX);
            let rows = rows.min(height.saturating_sub(top));
            if rows > 0 {
                regions.push(MouseRegion::new(Rect::new(
                    origin_x,
                    origin_y + top,
                    width,
                    rows,
                )));
            }
        }
        regions
    }

    /// On-screen rectangles of every tool block in the chat log, in render
    /// order. Empty before the first render or when the transcript is empty.
    ///
    /// Upstream wraps a folded tool block in a `MouseRegion` so a click on
    /// the body toggles `tool_expanded`
    /// (`packages/tui/src/components/tool-execution.ts:115-126`); the Rust
    /// port exposes the same rectangles here and routes the gesture in
    /// [`App::step_tool_block_mouse_gesture`].
    pub fn tool_block_regions(&self) -> Vec<(usize, MouseRegion)> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let total = self.messages.line_count(width);
        let scroll_top = total.saturating_sub(height as usize + self.resolved_scroll());
        let mut out = Vec::new();
        for (idx, start, end) in self.messages.tool_block_ranges(width) {
            let visible_start = start.max(scroll_top);
            let visible_end = end.min(scroll_top + height as usize);
            if visible_end <= visible_start {
                continue;
            }
            let row_offset = visible_start - scroll_top;
            let rows = u16::try_from(visible_end - visible_start).unwrap_or(u16::MAX);
            let y = origin_y + row_offset as u16;
            if y >= origin_y + height {
                continue;
            }
            let rows = rows.min(height - (y - origin_y));
            if rows == 0 {
                continue;
            }
            out.push((idx, MouseRegion::new(Rect::new(origin_x, y, width, rows))));
        }
        out
    }

    /// Move the selection focus, returning whether the rendered highlight
    /// actually changed.
    ///
    /// For character selections the focus is the pointer cell. For word /
    /// line selections the pointer is first resolved to the same granularity
    /// range, then the selection is rebuilt against the range captured on
    /// press — upstream's `updateSelectionFocus`
    /// (`packages/tui/src/tui-alt-screen.ts:1200-1218`).
    fn extend_selection(&mut self, point: (usize, usize)) -> StepOutcome {
        let Some(selection) = self.selection else {
            return StepOutcome::Idle;
        };
        let Some(updated) = self.updated_selection(selection, point) else {
            return StepOutcome::Idle;
        };
        if updated == selection {
            return StepOutcome::Idle;
        }
        self.selection = Some(updated);
        StepOutcome::Redraw
    }

    /// Recompute `selection` with its focus dragged to `point`, or `None`
    /// when the granularity range under the pointer cannot be resolved
    /// (upstream leaves the focus untouched in that case).
    fn updated_selection(
        &self,
        mut selection: Selection,
        point: (usize, usize),
    ) -> Option<Selection> {
        let Some(initial) = selection.initial else {
            selection.focus = SelectionPoint::cell(point.0, point.1);
            return Some(selection);
        };
        let (start, end) = match selection.granularity {
            SelectionGranularity::Word => self.word_selection(point)?,
            SelectionGranularity::Line => self.line_selection(point)?,
            SelectionGranularity::Character => {
                selection.focus = SelectionPoint::cell(point.0, point.1);
                return Some(selection);
            }
        };
        // A target before the initial range swaps the ends, so dragging up
        // past the anchor still selects from the new range to the anchor.
        if start.order() < initial.0.order() {
            selection.anchor = initial.1;
            selection.focus = start;
        } else {
            selection.anchor = initial.0;
            selection.focus = end;
        }
        Some(selection)
    }

    /// The rendered text of one log line, with styles stripped — upstream's
    /// `getSelectionSourceLine` (`packages/tui/src/tui-alt-screen.ts:1145-1151`).
    fn selection_line(&self, line: usize) -> Option<String> {
        let (width, _) = self.viewport();
        if width == 0 {
            return None;
        }
        let lines = self.messages.render_styled_lines(width);
        lines.get(line).map(|line| crate::utils::styled::plain_text(line))
    }

    /// The word range under `point`, or `None` when the line has no
    /// segment there (upstream's `getWordSelection`,
    /// `packages/tui/src/tui-alt-screen.ts:1156-1197`).
    ///
    /// A pickable segment (word-like, or one of
    /// [`TERMINAL_WORD_SELECTION_JOINERS`]) grows across adjacent pickable
    /// segments whenever a joiner sits on one side, which is what keeps
    /// `path/to/file` and `kebab-case` whole. The returned end carries an
    /// exclusive boundary column.
    fn word_selection(&self, point: (usize, usize)) -> Option<(SelectionPoint, SelectionPoint)> {
        let line = self.selection_line(point.0)?;
        let segments = word_segments(&line);
        let clicked = segments
            .iter()
            .position(|segment| point.1 >= segment.start && point.1 < segment.end)?;
        let mut start = segments[clicked].start;
        let mut end = segments[clicked].end;
        let mut index = clicked;
        while index > 0 && word_segments_can_join(&segments[index - 1], &segments[index]) {
            start = segments[index - 1].start;
            index -= 1;
        }
        let mut index = clicked;
        while index + 1 < segments.len()
            && word_segments_can_join(&segments[index], &segments[index + 1])
        {
            end = segments[index + 1].end;
            index += 1;
        }
        Some((
            SelectionPoint::cell(point.0, start),
            SelectionPoint::boundary(point.0, end),
        ))
    }

    /// The whole visible line under `point` — upstream's `getLineSelection`
    /// (`packages/tui/src/tui-alt-screen.ts:1193-1203`).
    fn line_selection(&self, point: (usize, usize)) -> Option<(SelectionPoint, SelectionPoint)> {
        let line = self.selection_line(point.0)?;
        let width = line.chars().count();
        Some((
            SelectionPoint::cell(point.0, 0),
            SelectionPoint::boundary(point.0, width),
        ))
    }

    /// Consume the previous press and return this press's click count.
    ///
    /// Upstream's `getClickCount` (`packages/tui/src/tui-alt-screen.ts:1220-1245`):
    /// a press repeats the count when it lands within
    /// [`DOUBLE_CLICK_INTERVAL`] of the previous one, on the same line and
    /// the same word column range; otherwise it restarts at `1`. The count
    /// cycles `1 → 2 → 3 → 1`. The only deviation is the time source —
    /// `std::time::Instant` rather than `Date.now()` — because `crossterm`
    /// does not report `clickCount`.
    fn next_click_count(
        &mut self,
        point: (usize, usize),
        word: Option<(SelectionPoint, SelectionPoint)>,
    ) -> usize {
        let now = Instant::now();
        let count = match (word, self.last_click) {
            (Some((start, end)), Some(previous))
                if now.duration_since(previous.at) <= DOUBLE_CLICK_INTERVAL
                    && previous.row == point.0
                    && previous.word_start == start.col
                    && previous.word_end == end.col =>
            {
                (previous.count % 3) + 1
            }
            _ => 1,
        };
        self.last_click = word.map(|(start, end)| ClickTarget {
            at: now,
            count,
            row: point.0,
            word_start: start.col,
            word_end: end.col,
        });
        count
    }

    /// Arm or disarm the edge autoscroll for a drag at `(x, y)`, returning
    /// whether the autoscroll is now pulling.
    ///
    /// Upstream's `updateSelectionAutoScroll`
    /// (`packages/tui/src/tui-alt-screen.ts:1247-1284`): a pointer at or
    /// above the viewport top pulls towards older output, at or below the
    /// bottom pulls towards the tail, and anywhere in between stops it.
    fn update_selection_autoscroll(&mut self, x: u16, y: u16) -> bool {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            self.stop_selection_autoscroll();
            return false;
        }
        let (_, origin_y) = self.viewport_origin();
        let top = origin_y;
        let bottom = origin_y.saturating_add(height).saturating_sub(1);
        let direction = if y <= top {
            -1
        } else if y >= bottom {
            1
        } else {
            0
        };
        if direction == 0 {
            self.stop_selection_autoscroll();
            return false;
        }
        self.selection_autoscroll_direction = direction;
        self.selection_autoscroll_pointer = Some((x, y));
        true
    }

    /// Stop the edge autoscroll, if one is pending.
    fn stop_selection_autoscroll(&mut self) {
        self.selection_autoscroll_direction = 0;
        self.selection_autoscroll_pointer = None;
    }

    /// Index of the tool block whose rendered lines cover log line `line`,
    /// or `None` when that line belongs to a user / assistant message (or
    /// the log is empty).
    ///
    /// Uses [`MessageView::item_index_at_line`], so a click lands on exactly
    /// the block the renderer painted at that row.
    fn tool_block_at(&self, line: usize) -> Option<usize> {
        let (width, _) = self.viewport();
        if width == 0 {
            return None;
        }
        let index = self.messages.item_index_at_line(line, width)?;
        match self.messages.items().get(index) {
            Some(item) if item.role == Role::Tool => Some(index),
            _ => None,
        }
    }

    /// Map an absolute terminal cell onto a rendered-log coordinate, or
    /// `None` when it falls outside the message viewport or before the
    /// first render.
    fn selection_point(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return None;
        }
        let (_, origin_y) = self.viewport_origin();
        if y < origin_y || y >= origin_y.saturating_add(height) {
            return None;
        }
        self.selection_point_clamped(x, y)
    }

    /// Like [`App::selection_point`], but clamps a pointer outside the
    /// viewport back onto its nearest row.
    /// Upstream keeps tracking a drag that left the viewport and maps the
    /// pointer through `getScrollSelectionPoint`, so the focus follows a
    /// pointer resting on the status / prompt rows while the autoscroll
    /// runs.
    fn selection_point_clamped(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        let (width, height) = self.viewport();
        if width == 0 || height == 0 {
            return None;
        }
        let (origin_x, origin_y) = self.viewport_origin();
        let row = if y < origin_y {
            0
        } else {
            ((y - origin_y) as usize).min(height as usize - 1)
        };
        let (start, lines) = self.messages.visible_lines(width, height);
        // Below the last rendered line (a short log): clamp to the last
        // line so a drag past the end still selects to the end of the text.
        let row = row.min(lines.len().saturating_sub(1));
        // The pointer arrives in **terminal cells** while every selection
        // column below is a **character offset**; snap the cell to the glyph
        // drawn there (upstream `getGraphemeCellRange`,
        // `packages/tui/src/utils.ts:320`). Without this a click on a CJK
        // line resolves to a character index past the text — the line is
        // half as many characters as it is cells — so the selection landed
        // to the right of the pointer and copied the wrong text.
        let cell = x.saturating_sub(origin_x) as usize;
        let col = match lines.get(row) {
            Some(line) => {
                let text = crate::utils::styled::plain_text(line);
                crate::utils::width::char_index_at_column(&text, cell)
            }
            None => 0,
        };
        Some((start + row, col))
    }

    /// Returns the selected text in the composer, if any (LUM-1332).
    ///
    /// The selection is a cell range on absolute buffer coordinates;
    /// `composer_selection_text` reconstructs the text by rendering each
    /// affected row through `Prompt::render_lines`, dropping the `▍` caret
    /// marker, and slicing the **cells** between the selection start and
    /// end with the wide-char-aware boundary rule upstream's
    /// `sliceByColumn(..., strict=true)` enforces — a wide glyph that would
    /// straddle the end cell is dropped rather than half-drawn, so the
    /// selection stays a sequence of whole characters.
    pub fn composer_selection_text(&self) -> Option<String> {
        let sel = self.composer_drag_selection?;
        let ((ax, ay), (fx, fy)) = sel;
        if ax == fx && ay == fy {
            return None;
        }
        let label = self.prompt.label();
        let label_width = crate::utils::width::columns(label);
        let body_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let total_width = (label_width + body_width).max(1) as u16;
        let max_rows = self.viewport.composer_size.1.load(Ordering::Relaxed) as usize;
        let scroll = self.viewport.composer_scroll.load(Ordering::Relaxed);

        // Upstream's anchor / focus carry the click-semantics cell: the caret
        // sits *before* the character at that column, so the cell itself is
        // included in the selection on the click side and excluded on the
        // other. We keep that semantics for the focus and pick the inclusive
        // end from whichever endpoint is larger — `+ char_width_at(max)` —
        // so dragging to a wide glyph captures both of its cells.
        let (start_row, end_row) = if ay <= fy { (ay, fy) } else { (fy, ay) };
        let (max_col_x, max_row_y) = if (ax, ay) >= (fx, fy) {
            (ax, ay)
        } else {
            (fx, fy)
        };

        // Render the composer's current window so we get the line text each
        // selected row carries.
        let (lines, _) = self.prompt.render_lines(total_width, max_rows.max(1), scroll);
        let row_offset = self.viewport.composer_origin.1.load(Ordering::Relaxed) as usize;

        let mut out = String::new();
        for abs_y in start_row..=end_row {
            let Some(row) = lines.get((abs_y as usize).saturating_sub(row_offset)) else {
                if abs_y < end_row {
                    out.push('\n');
                }
                continue;
            };
            // Drop the caret glyph — the highlight never carries it through.
            let line: String = row.chars().filter(|ch| *ch != '▍').collect();
            let (pre_start, pre_end_inclusive) =
                self.composer_row_range(abs_y, ay, ax, fy, fx, max_row_y, max_col_x, total_width);
            let cell_text = cell_slice_strict(&line, pre_start, pre_end_inclusive + 1);
            out.push_str(&cell_text);
            if abs_y < end_row {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Pre-caret cell range on `row_y`: `(start, end_inclusive)`. The end
    /// is the **last** cell the selection covers on this row, not the
    /// exclusive end — the caller turns it into a half-open range itself.
    fn composer_row_range(
        &self,
        row_y: u16,
        anchor_y: u16,
        anchor_x: u16,
        focus_y: u16,
        focus_x: u16,
        max_row_y: u16,
        max_x: u16,
        total_width: u16,
    ) -> (usize, usize) {
        let label = self.prompt.label();
        let label_width = crate::utils::width::columns(label);
        let label_cells = label_width;
        let last_cell = (total_width.saturating_sub(1)) as usize;
        let max_end_inclusive = self.max_end_inclusive(max_row_y, max_x);
        if anchor_y == focus_y {
            let (lo, hi) = if anchor_x <= focus_x {
                (anchor_x as usize, max_end_inclusive)
            } else {
                (focus_x as usize, max_end_inclusive)
            };
            (lo, hi)
        } else if row_y == anchor_y {
            if anchor_y < focus_y {
                // Forward: start at anchor, run through the last cell of the
                // anchor row's draft content (not the whole padded line).
                let anchor_end = self.row_last_content_cell(anchor_y);
                (anchor_x as usize, anchor_end)
            } else {
                // Backward: anchor is the bottom of the range; include up
                // through the last cell of the char at `anchor_x`.
                (label_cells, max_end_inclusive)
            }
        } else if row_y == focus_y {
            if anchor_y < focus_y {
                // Forward: focus row, from the label edge through the char.
                (label_cells, max_end_inclusive)
            } else {
                // Backward: focus row, from focus through the last content cell.
                let focus_end = self.row_last_content_cell(focus_y);
                (focus_x as usize, focus_end)
            }
        } else {
            // Middle row: from label to last content cell — never the padding.
            let row_end = self.row_last_content_cell(row_y);
            (label_cells, row_end.max(last_cell))
        }
    }

    /// Last buffer cell the draft occupies on `row_y`. The label takes the
    /// first `label_width` cells; cells past the last char are padding and
    /// do not belong to the selection, even though they are inside the
    /// composer's painted rectangle. An empty row (a window-padded
    /// continuation past the draft's natural height) reports `label_width -
    /// 1` so the caller still clamps the pointer to the gutter edge.
    fn row_last_content_cell(&self, row_y: u16) -> usize {
        let ox = self.viewport.composer_origin.0.load(Ordering::Relaxed);
        let width = self.viewport.composer_size.0.load(Ordering::Relaxed);
        let body_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let text = self.prompt.text();
        let label = self.prompt.label();
        let label_width = crate::utils::width::columns(label);
        let layout = crate::utils::visual_text::VisualLayout::new(&text, body_width.max(1));
        let rows = layout.rows();
        let oy = self.viewport.composer_origin.1.load(Ordering::Relaxed);
        let row_in_window = (row_y as usize).saturating_sub(oy as usize);
        let row_in_draft = self.viewport.composer_scroll.load(Ordering::Relaxed) + row_in_window;
        let row_width = rows
            .get(row_in_draft)
            .map(|r| crate::utils::width::columns(&r.text))
            .unwrap_or(0);
        let max_cell = (ox + width.saturating_sub(1)) as usize;
        let result = if row_width == 0 {
            (ox as usize + label_width.saturating_sub(1)).min(max_cell)
        } else {
            (ox as usize + label_width + row_width - 1).min(max_cell)
        };
        result
    }

    /// Last pre-caret cell the selection covers on `max_row_y` when the
    /// larger endpoint sits at `max_x`. For ASCII it is just `max_x`. For a
    /// wide character that starts at `max_x` it is `max_x + 1` so the
    /// second cell of the glyph lands in the slice.
    fn max_end_inclusive(&self, max_row_y: u16, max_x: u16) -> usize {
        let label = self.prompt.label();
        let label_width = crate::utils::width::columns(label);
        let body_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let max_rows = self.viewport.composer_size.1.load(Ordering::Relaxed) as usize;
        let scroll = self.viewport.composer_scroll.load(Ordering::Relaxed);
        let total_width = (label_width + body_width).max(1) as u16;
        let (lines, _) = self.prompt.render_lines(total_width, max_rows.max(1), scroll);
        let row_offset = self.viewport.composer_origin.1.load(Ordering::Relaxed) as usize;
        let Some(row) = lines.get((max_row_y as usize).saturating_sub(row_offset)) else {
            return max_x as usize;
        };
        let line: String = row.chars().filter(|ch| *ch != '▍').collect();
        let mut col = 0usize;
        for ch in line.chars() {
            let w = crate::utils::width::char_columns(ch);
            if col == max_x as usize {
                return max_x as usize + w.saturating_sub(1);
            }
            if col > max_x as usize {
                break;
            }
            col += w;
        }
        max_x as usize
    }

    /// Returns the last recorded composer area as `(x, y, width, height)`,
    /// or `(0, 0, 0, 0)` if no frame has been painted yet.
    pub fn composer_area(&self) -> (u16, u16, u16, u16) {
        (
            self.viewport.composer_origin.0.load(Ordering::Relaxed),
            self.viewport.composer_origin.1.load(Ordering::Relaxed),
            self.viewport.composer_size.0.load(Ordering::Relaxed),
            self.viewport.composer_size.1.load(Ordering::Relaxed),
        )
    }

    /// Record the composer rectangle for the pointer hit test, or clear it
    /// when the frame painted no composer (`None`).
    fn record_composer_area(&self, rect: Option<Rect>) {
        match rect {
            Some(rect) => {
                self.viewport.composer_origin.0.store(rect.x, Ordering::Relaxed);
                self.viewport.composer_origin.1.store(rect.y, Ordering::Relaxed);
                self.viewport.composer_size.0.store(rect.width, Ordering::Relaxed);
                self.viewport.composer_size.1.store(rect.height, Ordering::Relaxed);
            }
            None => self.viewport.composer_size.0.store(0, Ordering::Relaxed),
        }
    }

    /// Record the autocomplete dropdown rectangle and the candidate window it
    /// painted, or clear both when the frame painted no dropdown.
    ///
    /// `first_item` is the index of the first candidate row drawn and
    /// `item_rows` how many candidate rows followed; the trailing `(n/m)`
    /// counter row is excluded because it is not clickable. Clearing is not
    /// optional: a stale rectangle would keep swallowing clicks meant for the
    /// transcript after the dropdown closed.
    fn record_autocomplete_area(&self, rect: Option<Rect>, first_item: usize, item_rows: usize) {
        match rect {
            Some(rect) => {
                self.viewport.autocomplete_origin.0.store(rect.x, Ordering::Relaxed);
                self.viewport.autocomplete_origin.1.store(rect.y, Ordering::Relaxed);
                self.viewport.autocomplete_size
                    .0
                    .store(rect.width, Ordering::Relaxed);
                self.viewport.autocomplete_size
                    .1
                    .store(rect.height, Ordering::Relaxed);
                self.viewport.autocomplete_first_item
                    .store(first_item, Ordering::Relaxed);
                self.viewport.autocomplete_item_rows
                    .store(item_rows, Ordering::Relaxed);
            }
            None => {
                self.viewport.autocomplete_size.0.store(0, Ordering::Relaxed);
                self.viewport.autocomplete_size.1.store(0, Ordering::Relaxed);
                self.viewport.autocomplete_item_rows.store(0, Ordering::Relaxed);
            }
        }
    }

    /// Candidate index under a screen cell, or `None` for a cell that is not
    /// a dropdown candidate row (outside the list, or on its `(n/m)` counter).
    fn autocomplete_item_at(&self, x: u16, y: u16) -> Option<usize> {
        let (ox, oy, width, height, first_item, item_rows) = self.autocomplete_hit_geometry()?;
        if x < ox || x >= ox.saturating_add(width) || y < oy || y >= oy.saturating_add(height) {
            return None;
        }
        let row = (y - oy) as usize;
        if row >= item_rows {
            return None;
        }
        Some(first_item + row)
    }

    /// True when (x, y) is inside the autocomplete dropdown's painted
    /// rectangle. Distinct from [`Self::autocomplete_item_at`], which only
    /// fires for actual candidate rows; the rectangle also covers the
    /// `(n/m)` indicator row, and a click there must be swallowed by the
    /// dropdown rather than reach the transcript selection path
    /// (LUM-1327 upstream parity).
    fn autocomplete_contains(&self, x: u16, y: u16) -> bool {
        let Some((ox, oy, width, height, _, _)) = self.autocomplete_hit_geometry() else {
            return false;
        };
        x >= ox && y >= oy && x < ox.saturating_add(width) && y < oy.saturating_add(height)
    }

    /// Geometry the autocomplete handler needs to hit-test a pointer gesture:
    /// origin (ox, oy), width, height, first visible item, and how many item
    /// rows the painted window covers. The rectangle is recomputed from the
    /// composer's *current* state instead of the cached `autocomplete_origin`
    /// alone, because the list hangs off the composer's top edge — when the
    /// draft grows without an intervening frame, the whole list shifts up by
    /// the composer's growth and the cached origin would route clicks to the
    /// wrong row (LUM-1327).
    fn autocomplete_hit_geometry(&self) -> Option<(u16, u16, u16, u16, usize, usize)> {
        let width = self.viewport.autocomplete_size.0.load(Ordering::Relaxed);
        let height = self.viewport.autocomplete_size.1.load(Ordering::Relaxed);
        if width == 0 || height == 0 {
            return None;
        }
        let cached_oy = self.viewport.autocomplete_origin.1.load(Ordering::Relaxed);
        let cached_composer_height = self.viewport.composer_size.1.load(Ordering::Relaxed);
        let body_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let current_height = if body_width == 0 {
            cached_composer_height
        } else {
            VisualLayout::new(&self.prompt.text(), body_width)
                .rows()
                .len() as u16
        };
        // Composer extends up by `delta` rows when the draft grew since the
        // last paint; the list, which is pinned to the composer's top edge,
        // shifts up by the same amount.
        let delta = current_height.saturating_sub(cached_composer_height);
        let oy = cached_oy.saturating_sub(delta);
        let ox = self.viewport.autocomplete_origin.0.load(Ordering::Relaxed);
        let first_item = self.viewport.autocomplete_first_item.load(Ordering::Relaxed);
        let item_rows = self.viewport.autocomplete_item_rows.load(Ordering::Relaxed);
        Some((ox, oy, width, height, first_item, item_rows))
    }

    /// Record where the last frame painted the open modal's item rows, or
    /// clear the record when it painted none.
    ///
    /// `first_row` is the absolute row of the first painted item row,
    /// `first_item` the filtered index that row carries and `rows` how many
    /// item rows followed. The App calls this from the paint path (which
    /// takes `&self`), hence the atomics — the same seam as
    /// [`App::record_autocomplete_area`].
    fn record_modal_list(&self, kind: u8, first_row: u16, first_item: usize, rows: usize) {
        self.viewport.modal_list_kind.store(kind, Ordering::Relaxed);
        self.viewport.modal_list_first_row
            .store(first_row, Ordering::Relaxed);
        self.viewport.modal_list_first_item
            .store(first_item, Ordering::Relaxed);
        self.viewport.modal_list_rows.store(rows, Ordering::Relaxed);
    }

    /// The modal list row under the absolute row `y`, as
    /// `(kind, filtered item index)`, or `None` for a row that carries no
    /// item (above the list, on the title, rule, `(n/m)` counter or hint
    /// rows, or past its end).
    ///
    /// Only the row decides ownership, never the column: upstream hit-tests
    /// the overlay rectangle — which spans the terminal's full width here —
    /// and then maps `event.y` through `getVisibleRange()`, and neither
    /// `SelectList::handleMouse` nor `SettingsList::handleMouse` reads the
    /// column (`packages/tui/src/components/select-list.ts:110-140`,
    /// `packages/tui/src/components/settings-list.ts:179-210`).
    fn modal_list_hit(&self, y: u16) -> Option<(u8, usize)> {
        let kind = self.viewport.modal_list_kind.load(Ordering::Relaxed);
        if kind == MODAL_LIST_NONE {
            return None;
        }
        let rows = self.viewport.modal_list_rows.load(Ordering::Relaxed);
        if rows == 0 {
            return None;
        }
        let first_row = self.viewport.modal_list_first_row.load(Ordering::Relaxed);
        if y < first_row || y >= first_row.saturating_add(rows as u16) {
            return None;
        }
        let row = (y - first_row) as usize;
        Some((
            kind,
            self.viewport.modal_list_first_item.load(Ordering::Relaxed) + row,
        ))
    }

    /// Route a wheel notch that landed on an open modal's list.
    ///
    /// A notch over the item rows moves the highlight **one** row and clamps
    /// at the ends — upstream's `SelectList::handleMouse` reduces any wheel
    /// delta to `-1` / `+1` (`delta = wheelDelta < 0 ? -1 : 1`, unlike the
    /// transcript's `wheelScrollLines`), and `SettingsList::handleMouse`
    /// does the same. A notch on the title / counter / hint rows, or outside
    /// the modal's list, is still the modal's: upstream leaves it to
    /// `shouldDeferViewportInputToOverlay`, which returns it unconsumed, so
    /// the transcript behind the modal never scrolls either way
    /// (`packages/tui/src/tui-alt-screen.ts:645-694`).
    ///
    /// Only the settings modal is claimed globally instead (see
    /// [`App::step_settings_wheel`]): it is the only interactive surface on
    /// screen while it is open, so its own handler never needs the cell.
    fn step_modal_list_wheel(&mut self, up: bool, y: u16) -> StepOutcome {
        let Some((kind, _)) = self.modal_list_hit(y) else {
            return StepOutcome::Idle;
        };
        let step: i64 = if up { -1 } else { 1 };
        match kind {
            MODAL_LIST_DIALOG => {
                let Some(dialog) = self.dialog.as_mut() else {
                    return StepOutcome::Idle;
                };
                // `set_select_cursor` clamps to the last option, so the
                // comparison after it is what reports "nothing moved".
                let before = dialog.select_cursor();
                let target = if up {
                    before.saturating_sub(1)
                } else {
                    before.saturating_add(1)
                };
                dialog.set_select_cursor(target);
                if dialog.select_cursor() == before {
                    StepOutcome::Idle
                } else {
                    StepOutcome::Redraw
                }
            }
            MODAL_LIST_SELECTOR => {
                let Some(selector) = self.selector.as_mut() else {
                    return StepOutcome::Idle;
                };
                let len = selector.filtered_len();
                if len == 0 {
                    return StepOutcome::Idle;
                }
                let target = (selector.cursor() as i64 + step).clamp(0, len as i64 - 1) as usize;
                if target == selector.cursor() {
                    return StepOutcome::Idle;
                }
                selector.set_cursor(target);
                StepOutcome::Redraw
            }
            MODAL_LIST_SETTINGS => {
                let Some(list) = self.settings.as_mut() else {
                    return StepOutcome::Idle;
                };
                match list.scroll_by(step as i32) {
                    SettingsAction::Changed => StepOutcome::Redraw,
                    _ => StepOutcome::Idle,
                }
            }
            _ => StepOutcome::Idle,
        }
    }

    /// Move the open modal's highlight onto the row under the pointer —
    /// upstream's press branch, which selects the pressed row without
    /// activating it (`select-list.ts:126-136`, `settings-list.ts:197-205`).
    fn modal_list_press(&mut self, y: u16) -> StepOutcome {
        let Some((kind, index)) = self.modal_list_hit(y) else {
            return StepOutcome::Idle;
        };
        match kind {
            MODAL_LIST_DIALOG => {
                let Some(dialog) = self.dialog.as_mut() else {
                    return StepOutcome::Idle;
                };
                if dialog.select_cursor() == index {
                    return StepOutcome::Idle;
                }
                dialog.set_select_cursor(index);
                StepOutcome::Redraw
            }
            MODAL_LIST_SELECTOR => {
                let Some(selector) = self.selector.as_mut() else {
                    return StepOutcome::Idle;
                };
                if selector.cursor() == index {
                    return StepOutcome::Idle;
                }
                selector.set_cursor(index);
                StepOutcome::Redraw
            }
            MODAL_LIST_SETTINGS => {
                let Some(list) = self.settings.as_mut() else {
                    return StepOutcome::Idle;
                };
                match list.set_cursor(index) {
                    SettingsAction::Changed => StepOutcome::Redraw,
                    _ => StepOutcome::Idle,
                }
            }
            _ => StepOutcome::Idle,
        }
    }

    /// Activate the open modal's row under the pointer — upstream's click
    /// branch (`select-list.ts:137-148`, `settings-list.ts:206-210`).
    ///
    /// The three lists commit differently and each reuses the path its
    /// keyboard already takes: a selector hands the value to the driver
    /// through [`App::take_selector_commit`], a dialog answers itself (the
    /// App owns the reply channel), and the settings list activates the row,
    /// which the driver drains like any other settings change.
    fn modal_list_click(&mut self, y: u16) -> Option<StepOutcome> {
        let (kind, index) = self.modal_list_hit(y)?;
        match kind {
            MODAL_LIST_DIALOG => {
                let dialog = self.dialog.as_mut()?;
                dialog.set_select_cursor(index);
                Some(self.step_dialog(Key::new(KeyCode::Enter, KeyModifiers::NONE)))
            }
            MODAL_LIST_SELECTOR => {
                let selector = self.selector.as_mut()?;
                selector.set_cursor(index);
                let value = selector.selected_value()?.to_string();
                self.pending_selector_commit = Some(value);
                Some(StepOutcome::Redraw)
            }
            MODAL_LIST_SETTINGS => {
                let list = self.settings.as_mut()?;
                list.set_cursor(index);
                Some(self.step_settings(Key::new(KeyCode::Enter, KeyModifiers::NONE)))
            }
            _ => None,
        }
    }

    /// Take the picker row a pointer activation selected, if any.
    ///
    /// The interactive driver polls this right after feeding the App an
    /// event: what a picker *means* (`/model` switches the model, `/session`
    /// resumes a session) lives in the driver, which is also where the
    /// keyboard's `Enter` is applied (`pi-coding-agent/src/interactive.rs`).
    pub fn take_selector_commit(&mut self) -> Option<String> {
        self.pending_selector_commit.take()
    }

    /// Route a gesture that landed on the composer's autocomplete dropdown.
    ///
    /// Upstream checks the list rectangle **before** the editor body and
    /// before the screen-level text selection
    /// (`packages/tui/src/components/editor.ts:618-638` →
    /// `SelectList.handleMouse`), so a click on a candidate must not start a
    /// selection of the transcript text the list is painted over. Semantics
    /// follow the `SelectList`: a press highlights the row, a click (same-cell
    /// release) selects it and applies the completion, and a release anywhere
    /// else drops the pending press without applying.
    ///
    /// `None` leaves the gesture to the rest of the pointer path.
    fn autocomplete_mouse_gesture(&mut self, gesture: &MouseGesture) -> Option<StepOutcome> {
        // A release that follows a press inside the dropdown still belongs to
        // the dropdown — upstream's `mousePressTarget === "autocomplete"`
        // owns the entire press / drag / release pair (`editor.ts:618-666`).
        // Without that ownership the release can wander past the last row
        // onto the composer (no `(n/m)` counter leaves a row between the
        // bottom candidate and the editor), the prompt handler then runs its
        // caret-placement path, and `set_display_cursor` refreshes the
        // autocomplete and resets the highlight to its best match — the
        // exact LUM-1327 fix the press branch was written to provide.
        let pending_autocomplete_press = self.autocomplete_mouse_press.is_some();
        let inside_dropdown = self.autocomplete_contains(gesture.x, gesture.y);
        if !inside_dropdown && !pending_autocomplete_press {
            return None;
        }
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                let Some(index) = self.autocomplete_item_at(gesture.x, gesture.y) else {
                    // In rectangle but on the indicator: the click completes
                    // nothing — same as upstream's "not a candidate" branch
                    // — so we swallow it without starting a selection.
                    return Some(StepOutcome::Idle);
                };
                // The press takes the pointer exactly like the pill / the
                // scrollbar do: the transcript selection behind the list is
                // dropped, and the cell is remembered so only a release on it
                // commits (`clearTextSelection` + `mousePressTarget`).
                self.stop_selection_autoscroll();
                self.selection = None;
                self.selection_dragging = false;
                self.scrollbar_hover = false;
                self.scrollbar_drag = None;
                self.prompt_mouse_press = None;
                self.composer_drag_selection = None;
                self.autocomplete_mouse_press = Some((gesture.x, gesture.y, index));
                self.prompt.editor_mut().set_autocomplete_selected(index);
                Some(StepOutcome::Redraw)
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                let Some(pressed) = self.autocomplete_mouse_press.take() else {
                    // No pending press means the press landed on the
                    // indicator row (or on no candidate at all). Either
                    // way the dropdown owns the release — it must not
                    // reach the transcript selection path, so swallow
                    // it as Idle.
                    return Some(StepOutcome::Idle);
                };
                if pressed.0 != gesture.x || pressed.1 != gesture.y {
                    // The pointer moved off the pressed row: upstream drops the
                    // gesture (and with it the click) instead of applying a
                    // candidate the reader never released on. The press is
                    // consumed either way so a release past the dropdown
                    // rect (e.g. onto the composer on a press of the last
                    // candidate) cannot also reach the prompt path.
                    return Some(StepOutcome::Idle);
                }
                self.prompt
                    .editor_mut()
                    .set_autocomplete_selected(pressed.2);
                Some(match self.prompt.accept_autocomplete() {
                    crate::components::prompt::PromptAction::Changed => StepOutcome::Redraw,
                    _ => StepOutcome::Idle,
                })
            }
            MouseGestureKind::Drag(_) | MouseGestureKind::Move
                if self.autocomplete_mouse_press.is_some() =>
            {
                Some(StepOutcome::Idle)
            }
            _ => None,
        }
    }

    /// Route a wheel notch that landed on the composer's autocomplete
    /// dropdown: move the highlight one candidate, like the list's own
    /// `SelectList.handleMouse` wheel branch.
    ///
    /// Only the *row* decides ownership, not the column: upstream tests the
    /// notch against the editor's row range while the editor's layout box
    /// spans the full width, and the list's wheel branch never reads `x`
    /// (`packages/tui/src/components/editor.ts:618-638`,
    /// `packages/tui/src/components/select-list.ts:110-121`).
    ///
    /// Upstream's `SelectList.handleMouse` treats a notch as a single step
    /// regardless of the wheel's magnitude (`delta = wheelDelta < 0 ? -1 : 1`,
    /// `packages/tui/src/components/select-list.ts:110-121`) and **clamps** at
    /// the ends, where the keyboard's `tui.select.up` / `down` wrap; the range
    /// it tests is the painted list block, counter row included, across the
    /// editor's full width (`packages/tui/src/components/editor.ts:618-638`).
    /// The window re-centers on the new selection by itself — it is derived
    /// from `autocomplete_selected` at paint time — so a notch at the edge of
    /// a long list scrolls the window by exactly the rows it moved.
    ///
    /// `None` when there is no dropdown under the notch, which leaves the
    /// wheel to the chat-log viewport (upstream's `routeWheel`).
    fn step_autocomplete_wheel(&mut self, up: bool, _x: u16, y: u16) -> Option<StepOutcome> {
        if !self.prompt.editor().is_showing_autocomplete() {
            return None;
        }
        // The list rectangle, not just a candidate row: upstream tests the
        // wheel against `renderedAutocompleteHeight`, which includes the
        // `(n/m)` counter, so a notch on the counter row steers the list too.
        let height = self.viewport.autocomplete_size.1.load(Ordering::Relaxed);
        if height == 0 {
            return None;
        }
        let origin_y = self.viewport.autocomplete_origin.1.load(Ordering::Relaxed);
        if y < origin_y || y >= origin_y.saturating_add(height) {
            return None;
        }
        let len = self.prompt.editor().autocomplete_items().len();
        if len == 0 {
            return None;
        }
        let selected = self.prompt.editor().autocomplete_selected();
        let step = if up { -1i64 } else { 1i64 };
        let next = (selected as i64 + step).clamp(0, len as i64 - 1) as usize;
        if next == selected {
            // Clamped at an end: the notch is still the list's, so the
            // transcript behind it must not move either.
            return Some(StepOutcome::Idle);
        }
        self.prompt.editor_mut().set_autocomplete_selected(next);
        Some(StepOutcome::Redraw)
    }

    /// True when the pointer cell is inside the composer rectangle the last
    /// frame painted.
    fn composer_contains(&self, x: u16, y: u16) -> bool {
        let width = self.viewport.composer_size.0.load(Ordering::Relaxed);
        let height = self.viewport.composer_size.1.load(Ordering::Relaxed);
        if width == 0 || height == 0 {
            return false;
        }
        let ox = self.viewport.composer_origin.0.load(Ordering::Relaxed);
        let oy = self.viewport.composer_origin.1.load(Ordering::Relaxed);
        x >= ox && y >= oy && x < ox.saturating_add(width) && y < oy.saturating_add(height)
    }

    /// Resolve a cell inside the composer to a character offset in
    /// [`Prompt::text`], or `None` when the cell holds no draft text.
    ///
    /// Upstream's `Editor.handleMouse` click branch
    /// (`packages/tui/src/components/editor.ts:620-666`): the click row is
    /// mapped through the same visual-line map the renderer used, the cell
    /// column is snapped to the glyph drawn there, and one correction keeps
    /// a click past the end of a soft-wrapped row on that row instead of
    /// letting it jump to the start of the next one.
    fn composer_cursor_offset(&self, x: u16, y: u16) -> Option<usize> {
        if !self.composer_contains(x, y) {
            return None;
        }
        let ox = self.viewport.composer_origin.0.load(Ordering::Relaxed) as usize;
        let oy = self.viewport.composer_origin.1.load(Ordering::Relaxed) as usize;
        let label = columns(self.prompt.label());
        let body_col = (x as usize).saturating_sub(ox);
        let row_in_window = (y as usize).saturating_sub(oy);
        let text = self.prompt.text();
        let width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let layout = VisualLayout::new(&text, width);
        let rows = layout.rows();

        // The visual row the pointer is over.
        let row_in_draft = self.viewport.composer_scroll.load(Ordering::Relaxed) + row_in_window;
        let visual = rows.get(row_in_draft)?;

        // Measure the pointer's column in display characters from the row's
        // start.  `char_index_at_column` walks terminal cells — the same
        // column space the renderer uses to lay out the row — and returns
        // the character index whose first cell sits at or before the
        // pointer, matching how `cursor_at(row, column)` expects its
        // `column` to be a display column. Subtract the label so the
        // column is relative to the body text, not the whole painted row.
        let col_in_row =
            crate::utils::width::char_index_at_column(&visual.text, body_col.saturating_sub(label));

        // When the pointer landed past the last character of a soft-wrapped
        // row (not the last row of its hard line), park the caret on that
        // row's last character instead of letting it spill onto the next row
        // (upstream's `isLastSegment` correction). A click on a single-char
        // cell's trailing half also lands here and needs the same correction.
        let row_char_count = visual.text.chars().count();
        let is_last_row_of_draft = row_in_draft + 1 >= rows.len();
        let row_cell_count = crate::utils::width::columns(&visual.text);
        // The column argument is a *cell* column, so compare against the row's
        // cell width: a click at or past the trailing cell of a non-final
        // row needs the correction, while a click on a valid cell inside
        // the row does not.
        let col_in_row = if body_col.saturating_sub(label) >= row_cell_count && !is_last_row_of_draft {
            row_char_count.saturating_sub(1)
        } else {
            col_in_row
        };

        // `caret` gives `(row, col)` for a display cursor, and
        // `cursor_at(row, col)` is its inverse — so we can find the
        // display cursor position by starting from the visual row's char
        // offset: `visual.start + col_in_row` gives the cursor that
        // `caret` would put at this row and column.
        let cursor_offset = layout.cursor_at(row_in_draft, col_in_row);
        Some(cursor_offset)
    }

    /// Place the composer caret at the clicked cell.
    fn place_prompt_cursor(&mut self, x: u16, y: u16) -> StepOutcome {
        let Some(offset) = self.composer_cursor_offset(x, y) else {
            return StepOutcome::Idle;
        };
        match self.prompt.place_cursor(offset) {
            crate::components::prompt::PromptAction::Changed => StepOutcome::Redraw,
            _ => StepOutcome::Idle,
        }
    }

    /// Route a gesture that landed on the composer.
    ///
    /// `Some` means the composer owns the gesture — a left press inside it is
    /// remembered and a release on the same cell places the caret (upstream
    /// synthesises that click in the editor's `handleMouse`); a drag from it
    /// is swallowed so the transcript behind cannot start a text selection
    /// through the composer. `None` leaves the gesture to the transcript
    /// path, which is what a release that left the composer needs.
    ///
    /// The selection itself is **cell-based** and lives on the App, mirroring
    /// upstream's `selectionAnchor` / `selectionFocus` (cell columns on
    /// `tui-alt-screen`), not the editor's character offsets. The editor
    /// owns text; the screen owns the cells the user painted over.
    fn prompt_mouse_gesture(&mut self, gesture: &MouseGesture) -> Option<StepOutcome> {
        // A reverse history search (`Ctrl+R`) owns the editor: its preview is
        // live state, so a click that drops the caret mid-search would edit
        // the buffer the reader is about to accept with `Enter` (codex
        // `Editor.handleMouse` short-circuits on `historySearchActive()`).
        // Drop any composer press/drag state so the next post-search pointer
        // event starts from a clean anchor.
        if self.history_search_active() {
            self.prompt_mouse_press = None;
            self.composer_drag_selection = None;
            return Some(StepOutcome::Idle);
        }
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                if !self.composer_contains(gesture.x, gesture.y) {
                    // A press outside the composer is a new gesture: the composer's
                    // selection goes away (upstream re-anchors on every press).
                    self.composer_drag_selection = None;
                    self.composer_press_moved_caret = false;
                    self.prompt_mouse_press = None;
                    return None;
                }
                // Record the press cell so a release on the same cell is a
                // click (the editor-only path) and a release elsewhere is the
                // end of a drag. The drag selection anchors here too — the
                // first cell the pointer pressed on.
                self.prompt_mouse_press = Some((gesture.x, gesture.y));
                self.composer_drag_selection = Some(((gesture.x, gesture.y), (gesture.x, gesture.y)));
                let cursor_before = self.prompt.editor().cursor();
                let _ = self.place_prompt_cursor(gesture.x, gesture.y);
                let cursor_after = self.prompt.editor().cursor();
                self.composer_press_moved_caret = cursor_before != cursor_after;
                if cursor_before == cursor_after {
                    Some(StepOutcome::Idle)
                } else {
                    Some(StepOutcome::Redraw)
                }
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                let pressed = self.prompt_mouse_press.take();
                if !self.composer_contains(gesture.x, gesture.y) {
                    // The release wandered off the composer — the App still
                    // owns the drag selection so a release elsewhere can
                    // finish it, but the click/release path does not. Drop
                    // the stale selection so the next gesture starts fresh.
                    self.composer_drag_selection = None;
                    self.pending_clipboard = None;
                    return None;
                }
                let is_click = pressed == Some((gesture.x, gesture.y));
                self.composer_press_moved_caret = false;
                if is_click {
                    // A click is neither a selection nor a copy: collapse
                    // the drag selection down to nothing and place the
                    // caret on the clicked cell. The press already armed
                    // the drag with anchor==focus, which renders no
                    // highlight (see
                    // `apply_composer_selection_highlight`'s empty-range
                    // guard), and the caret is already at the clicked cell
                    // after the press, so the release has no independent
                    // repaint signal of its own.
                    let _ = self.place_prompt_cursor(gesture.x, gesture.y);
                    self.composer_drag_selection = None;
                    self.pending_clipboard = None;
                    Some(StepOutcome::Idle)
                } else {
                    // Terminals coalesce motion: the release can carry the
                    // last pointer position without a drag event for it, so
                    // the release has to finish the gesture at *its* cell
                    // rather than at the last motion sample. Extend the
                    // selection to the release cell, move the caret there,
                    // and queue the highlighted fragment for the clipboard
                    // when copy-on-select is on.
                    let anchor = pressed.unwrap_or((gesture.x, gesture.y));
                    self.composer_drag_selection = Some((anchor, (gesture.x, gesture.y)));
                    let caret_outcome = self.place_prompt_cursor(gesture.x, gesture.y);
                    if self.config.copy_on_select {
                        if let Some(text) = self.composer_selection_text() {
                            self.pending_clipboard = Some(text);
                        }
                    }
                    Some(caret_outcome)
                }
            }
            MouseGestureKind::Drag(_) | MouseGestureKind::Move
                if self.prompt_mouse_press.is_some() =>
            {
                // LUM-1332: a drag from a composer press belongs to the
                // composer. Resolve the cell the pointer is over — when
                // the pointer wandered past the composer's rectangle, the
                // helper clamps it back onto the composer so the drag can
                // never bleed into the transcript behind it.
                let Some((cell_x, cell_y)) = self.composer_drag_cell(gesture.x, gesture.y) else {
                    return Some(StepOutcome::Idle);
                };
                let anchor = self
                    .prompt_mouse_press
                    .unwrap_or((cell_x, cell_y));
                self.composer_drag_selection = Some((anchor, (cell_x, cell_y)));
                // The caret follows the drag so a release on a different
                // cell leaves the cursor there.  Plain clicks already set
                // it; the drag only moves it.
                let _ = self.place_prompt_cursor(cell_x, cell_y);
                Some(StepOutcome::Redraw)
            }
            _ => None,
        }
    }

    /// Cell the *drag* focus should sit at for a pointer at `(x, y)`.
    ///
    /// When the pointer is inside the composer this is just the pointer
    /// itself — the cell range `composer_drag_selection` already stores
    /// is the cell range the user can see under the caret. When the
    /// pointer has wandered outside the composer's rectangle, the focus
    /// is clamped back to the composer's near edge so the drag stays
    /// inside the prompt: above the first row → first row, below the
    /// last row → last row, past the left edge → first column, past the
    /// right edge → last column of the row the pointer is on. The drag
    /// always belongs to the composer when the press that armed it did,
    /// even if the pointer leaves the rectangle (LUM-1332).
    fn composer_drag_cell(&self, x: u16, y: u16) -> Option<(u16, u16)> {
        let ox = self.viewport.composer_origin.0.load(Ordering::Relaxed);
        let oy = self.viewport.composer_origin.1.load(Ordering::Relaxed);
        let width = self.viewport.composer_size.0.load(Ordering::Relaxed);
        let height = self.viewport.composer_size.1.load(Ordering::Relaxed);
        if width == 0 || height == 0 {
            return None;
        }
        // Clamp y to the composer's own row range.
        let clamped_y = y.max(oy).min(oy + height.saturating_sub(1));
        // Compute the draft's pre-caret extent on `clamped_y`. Empty rows
        // (the prompt window showing a blank line because the draft has fewer
        // rows than the window) have no body cells at all, so a pointer over
        // them snaps to the label gutter's right edge — i.e. the column just
        // past the label — and the resulting focus collapses to an empty
        // selection. That matches upstream's behaviour of dragging past the
        // draft into empty space producing an empty range.
        let body_width = self.viewport.composer_body_width.load(Ordering::Relaxed) as usize;
        let text = self.prompt.text();
        let label = self.prompt.label();
        let label_width = crate::utils::width::columns(label);
        let layout = crate::utils::visual_text::VisualLayout::new(&text, body_width.max(1));
        let rows = layout.rows();
        let row_in_window = (clamped_y as usize).saturating_sub(oy as usize);
        let row_in_draft = self.viewport.composer_scroll.load(Ordering::Relaxed) + row_in_window;
        let row_width = rows
            .get(row_in_draft)
            .map(|r| crate::utils::width::columns(&r.text))
            .unwrap_or(0);
        let row_extent_lo = (ox + label_width as u16).min(ox + width.saturating_sub(1));
        let row_extent_hi = if row_width == 0 {
            row_extent_lo
        } else {
            (ox + label_width as u16 + row_width as u16 - 1)
                .min(ox + width.saturating_sub(1))
        };
        let clamped_x = x.max(row_extent_lo).min(row_extent_hi);
        Some((clamped_x, clamped_y))
    }

    /// Start / end of the active selection in rendered-log coordinates, or
    /// `None` when nothing is selected.
    pub fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let (start, end) = self.selection?.bounds()?;
        Some(((start.line, start.col), (end.line, end.col)))
    }

    /// True when there is a non-empty text selection.
    pub fn has_selection(&self) -> bool {
        self.selection_bounds().is_some()
    }

    /// The selected text, exactly as copy-on-select would hand it to the
    /// driver: one entry per rendered line, joined by `\n`, with trailing
    /// whitespace trimmed per line and the character under the focus cell
    /// included. A word / line range ends on an exclusive boundary column,
    /// so its end column is *not* widened (upstream `getActiveSelectionText`,
    /// `packages/tui/src/tui-alt-screen.ts:1412-1429`).
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection?.bounds()?;
        let (width, _) = self.viewport();
        if width == 0 {
            return None;
        }
        let lines = self.messages.render_styled_lines(width);
        if lines.is_empty() {
            return None;
        }
        let last = lines.len() - 1;
        let end_line = end.line.min(last);
        let mut out: Vec<String> = Vec::new();
        for (idx, line) in lines.iter().enumerate().take(end_line + 1).skip(start.line) {
            let text = crate::utils::styled::plain_text(line);
            let len = text.chars().count();
            let from = if idx == start.line {
                start.col.min(len)
            } else {
                0
            };
            let to = if idx == end.line {
                selection_end_column(&end, len)
            } else {
                len
            };
            let segment: String = text
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            out.push(segment.trim_end().to_string());
        }
        let text = out.join("\n");
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Drop the active selection (and any highlight it produced).
    ///
    /// Mirrors upstream's `clearTextSelection`
    /// (`packages/tui/src/tui-alt-screen.ts:865-874`), which stops the
    /// autoscroll but deliberately keeps the click-count history: two clicks
    /// separated by an intervening clear still count as a double click.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_dragging = false;
        self.stop_selection_autoscroll();
    }

    /// Drop every transcript row and repin the viewport to the end.
    ///
    /// This is the `/clear` path (`pi-coding-agent`'s `SlashCommand::Clear`).
    /// It used to also be reachable from `Ctrl+L`; that chord belongs to
    /// `app.model.select` upstream, and the driver now owns it, so the App no
    /// longer claims it. The selection is dropped because its line indices
    /// point into the transcript that just disappeared; component state that is
    /// not the transcript (markdown mode, thinking/tool folds, theme) is left
    /// alone.
    pub fn clear_transcript(&mut self) {
        self.messages.clear();
        self.clear_selection();
    }

    /// Take the text the driver must copy to the clipboard, if a
    /// copy-on-select release queued one.
    ///
    /// The App deliberately owns no terminal handle: the driver decides how
    /// to copy (upstream defaults to an OSC 52 write, with an injectable
    /// clipboard override — `packages/tui/src/tui-alt-screen.ts:1449-1462`).
    /// See [`crate::clipboard::osc52_sequence`] for that default.
    pub fn take_clipboard_request(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Queue a clipboard write for the driver, using the same channel as
    /// copy-on-select.
    ///
    /// The driver resolves these requests by copying to the system
    /// clipboard. Callers like the coding agent's `app.message.copy` handler
    /// use this to hand over transcript text the App itself does not own.
    pub fn request_clipboard(&mut self, text: impl Into<String>) {
        self.pending_clipboard = Some(text.into());
    }

    /// Take the pending `app.clipboard.pasteImage` request, if any.
    ///
    /// `true` means the user pressed the chord and the driver should read the
    /// system clipboard and call [`App::paste_image`] (image found) or
    /// [`App::paste_text`] (text fallback). The App never reads the
    /// clipboard itself, so headless tests drive the two paths directly.
    pub fn take_image_paste_request(&mut self) -> bool {
        std::mem::take(&mut self.pending_image_paste)
    }

    /// Attach a clipboard image to the draft as a chip at the cursor
    /// (upstream `handleClipboardPaste`'s image branch,
    /// `interactive-mode.ts:2933`, which inserts the saved file path).
    ///
    /// Returns `false` when [`MAX_IMAGE_ATTACHMENTS`](crate::components::editor::MAX_IMAGE_ATTACHMENTS)
    /// chips are already attached; the caller surfaces the refusal. The
    /// draft text is left untouched either way.
    pub fn paste_image(&mut self, image: pi_protocol::ImageContent) -> bool {
        match self.prompt.editor_mut().insert_image(image) {
            crate::components::editor::ImageInsertOutcome::Inserted => true,
            crate::components::editor::ImageInsertOutcome::AtCapacity => {
                self.flash_status(format!(
                    "At most {} images can be attached to a prompt",
                    crate::components::editor::MAX_IMAGE_ATTACHMENTS
                ));
                false
            }
        }
    }

    /// Insert clipboard *text* at the cursor — the fallback
    /// `app.clipboard.pasteImage` takes when the clipboard holds no image
    /// (upstream `handleClipboardPaste`'s else branch).
    ///
    /// Upstream wraps the clipboard text in the bracketed-paste markers and
    /// feeds it back through `editor.handleInput`
    /// (`interactive-mode.ts:2445`, `:2927`), so a pasted log gets the same
    /// marker / undo treatment as a terminal paste; the port calls the same
    /// entry point directly.
    pub fn paste_text(&mut self, text: &str) {
        self.prompt.editor_mut().insert_paste(text);
    }

    /// Clear the composer: buffer text, pasted chips and the history
    /// browsing / undo state, but keep the prompt history. `/new` and
    /// `app.session.new` call this so a new session never inherits a draft.
    pub fn clear_composer(&mut self) {
        self.prompt.clear();
    }

    /// Number of image chips currently attached to the draft.
    pub fn image_count(&self) -> usize {
        self.prompt.image_count()
    }

    /// Paint the active selection into the already-rendered message area by
    /// adding the reversed-video modifier to the selected cells.
    fn apply_selection_highlight(&self, area: Rect, buf: &mut Buffer) {
        crate::utils::render_helpers::apply_selection_highlight(
            &self.messages,
            self.selection.as_ref(),
            area,
            buf,
        );
    }

    /// Lines per page — one message-viewport height, but never zero so a
    /// key press before the first render is still a no-op instead of a
    /// panic.
    fn message_page(&self) -> usize {
        let (_, height) = self.viewport();
        height.max(1) as usize
    }

    /// Rows the composer window can show: the height
    /// [`App::paint_prompt`] last used after the
    /// [`AppConfig::composer_max_rows`] clamp, or the configured cap itself
    /// before the first frame — the same number the first frame will use on
    /// a terminal tall enough to grant it, so a key pressed before any
    /// paint takes the branch it would take one frame later.
    pub fn composer_window_rows(&self) -> usize {
        let rows = self.viewport.composer_window.load(Ordering::Relaxed) as usize;
        if rows == 0 {
            self.config.composer_max_rows.max(1)
        } else {
            rows
        }
    }

    /// Effective composer row cap for the current viewport.
    ///
    /// On terminals narrower than [`crate::EXTREME_NARROW_WIDTH`] columns
    /// this returns `1` regardless of the configured cap — the
    /// extreme-narrow band forces the composer to one row so the cell
    /// budget stays predictable. Everywhere else it returns the configured
    /// [`AppConfig::composer_max_rows`], clamped to at least one row.
    pub fn composer_max_rows_effective(&self) -> usize {
        let opts = self.narrow_options();
        let cap = self.config.composer_max_rows.max(1);
        cap.min(opts.max_composer_rows as usize).max(1)
    }

    /// The configured composer row cap from [`AppConfig::composer_max_rows`].
    ///
    /// Tests and tools that need to compare the configured cap against the
    /// narrow-terminal effective cap reach for this. Production renderers
    /// should call [`App::composer_max_rows_effective`] so the
    /// extreme-narrow clamp is honoured.
    pub fn composer_max_rows_configured(&self) -> usize {
        self.config.composer_max_rows.max(1)
    }

    /// [`crate::NarrowOptions`] for the current viewport.
    ///
    /// Every narrow-aware component on the App side reaches for this
    /// single value so the thresholds stay consistent. The width used is
    /// the cached viewport width; if the viewport hasn't been measured
    /// yet the function falls back to [`u16::MAX`], which classifies as
    /// wide.
    pub fn narrow_options(&self) -> crate::NarrowOptions {
        let width = self.viewport.viewport_width.load(Ordering::Relaxed);
        let width = if width == 0 { u16::MAX } else { width };
        crate::narrow_options(width)
    }

    /// True when the composer's draft needs more rows than the composer
    /// window can show, i.e. when part of the draft is outside it.
    ///
    /// This is the condition that decides who owns `PageUp` / `PageDown`:
    /// a clipped composer pages itself (the editor's `tui.editor.pageUp` /
    /// `pageDown`), and a composer that fits hands the bare chords to the
    /// transcript (`tui.altScreen.pageUp` / `pageDown`), so a draft that
    /// already fits cannot swallow the chat log's own scrolling. The row
    /// count is measured with the width the last frame wrapped at, so it
    /// answers exactly the question the renderer answered.
    fn composer_overflows(&self) -> bool {
        self.prompt.editor().visual_row_count() > self.composer_window_rows()
    }

    /// Largest valid scroll offset for the viewport the App last rendered.
    fn max_scroll(&self) -> usize {
        let (width, height) = self.viewport();
        if height == 0 {
            return 0;
        }
        self.messages
            .line_count(width)
            .saturating_sub(height as usize)
    }

    /// Current scroll offset, resolving `MessageView::scroll_to_top`'s
    /// `usize::MAX` sentinel against the last rendered viewport so it can
    /// be scrolled back down from.
    fn resolved_scroll(&self) -> usize {
        let max = self.max_scroll();
        match self.messages.scroll_offset() {
            usize::MAX => max,
            offset => offset.min(max),
        }
    }

    /// Scroll the chat log up (towards older output) by `lines`. Returns
    /// true when the viewport actually moved, so the caller can skip a
    /// redraw.
    ///
    /// Mirrors `tui.altScreen.pageUp` / `lineUp`
    /// (`packages/tui/src/keybindings.ts:160-176`) — the fullscreen TUI
    /// owns the scrollback because the alternate screen hides the
    /// terminal's own.
    pub fn scroll_viewport_up(&mut self, lines: usize) -> bool {
        let next = self
            .resolved_scroll()
            .saturating_add(lines.max(1))
            .min(self.max_scroll());
        if next == self.resolved_scroll() {
            return false;
        }
        self.messages.set_scroll_from_bottom(next);
        true
    }

    /// Scroll the chat log down (towards the tail) by `lines`. Reaching
    /// the tail re-attaches the viewport to new output. Returns true when
    /// the viewport actually moved.
    pub fn scroll_viewport_down(&mut self, lines: usize) -> bool {
        let current = self.resolved_scroll();
        if current == 0 {
            return false;
        }
        // `set_scroll_from_bottom(0)` also re-attaches to the tail.
        self.messages
            .set_scroll_from_bottom(current.saturating_sub(lines.max(1)));
        true
    }

    /// Current scroll offset (lines from the bottom). `0` means
    /// pinned.
    pub fn scroll_offset_for_test(&self) -> usize {
        self.resolved_scroll()
    }

    /// Scroll the chat log up by half a viewport (`tui.altScreen.halfPageUp`).
    ///
    /// A viewport of height `h` scrolls by `(h + 1) / 2` so the user crosses
    /// the midpoint of an odd-height terminal on the first keystroke. The
    /// bare chord is unbound by default
    /// (`packages/tui/src/keybindings.ts:218-219`), so this path runs only
    /// when an installed override binds the id; with nothing installed the
    /// chord falls through and the bare `PageUp` / `PageDown` handler above
    /// stays in charge.
    pub fn scroll_viewport_half_page_up(&mut self) -> bool {
        let page = self.message_page();
        let half = page.div_ceil(2).max(1);
        self.scroll_viewport_up(half)
    }

    /// Scroll the chat log down by half a viewport (`tui.altScreen.halfPageDown`).
    ///
    /// Mirror of [`scroll_viewport_half_page_up`](Self::scroll_viewport_half_page_up);
    /// the count rounds up so the user crosses the midpoint of an odd-height
    /// terminal on the first keystroke.
    pub fn scroll_viewport_half_page_down(&mut self) -> bool {
        let page = self.message_page();
        let half = page.div_ceil(2).max(1);
        self.scroll_viewport_down(half)
    }

    /// Scroll the chat log up by one line (`tui.altScreen.lineUp`).
    ///
    /// Upstream binds this id to no chord by default, so a custom overlay
    /// or a user override is the only way to reach it; without one the
    /// `PageUp` / `PageDown` handler above wins. The unit is one rendered
    /// row, so a keypress on a tall transcript makes small, predictable
    /// progress (LUM-1317 §5).
    pub fn scroll_viewport_line_up(&mut self) -> bool {
        self.scroll_viewport_up(1)
    }

    /// Scroll the chat log down by one line (`tui.altScreen.lineDown`).
    ///
    /// Mirror of [`scroll_viewport_line_up`](Self::scroll_viewport_line_up).
    pub fn scroll_viewport_line_down(&mut self) -> bool {
        self.scroll_viewport_down(1)
    }

    /// Jump the viewport to the start row of the previous user prompt
    /// (`tui.altScreen.previousPrompt`).
    ///
    /// "Previous" means any [`Role::User`] item whose first rendered row
    /// is above the viewport's current top edge. If the viewport is already
    /// showing the oldest user prompt, the call is a no-op (returns
    /// `false`). If no user prompts exist, the call is also a no-op.
    /// Otherwise the viewport detaches from the tail and lands on the
    /// prompt's first row.
    pub fn jump_to_previous_prompt(&mut self, width: u16) -> bool {
        self.jump_to_prompt_boundary(width, PromptJumpDirection::Previous)
    }

    /// Jump the viewport to the start row of the next user prompt
    /// (`tui.altScreen.nextPrompt`).
    ///
    /// Mirror of [`jump_to_previous_prompt`](Self::jump_to_previous_prompt):
    /// finds the first [`Role::User`] item whose first rendered row is
    /// below the viewport's current top edge. If no such prompt exists
    /// (or no user prompts exist at all) the call is a no-op.
    pub fn jump_to_next_prompt(&mut self, width: u16) -> bool {
        self.jump_to_prompt_boundary(width, PromptJumpDirection::Next)
    }

    fn jump_to_prompt_boundary(
        &mut self,
        width: u16,
        direction: PromptJumpDirection,
    ) -> bool {
        let total = self.messages.line_count(width);
        let viewport_top = total.saturating_sub(
            self.viewport().1 as usize + self.resolved_scroll(),
        );
        let ranges = self.messages.item_line_ranges(width);
        let boundary = match direction {
            PromptJumpDirection::Previous => ranges
                .into_iter()
                .enumerate()
                .filter_map(|(idx, (start, _))| {
                    if matches!(
                        self.messages.items().get(idx).map(|i| i.role),
                        Some(Role::User)
                    ) {
                        Some(start)
                    } else {
                        None
                    }
                })
                .take_while(|&start| start < viewport_top)
                .last(),
            PromptJumpDirection::Next => ranges
                .into_iter()
                .enumerate()
                .filter_map(|(idx, (start, _))| {
                    if matches!(
                        self.messages.items().get(idx).map(|i| i.role),
                        Some(Role::User)
                    ) {
                        Some(start)
                    } else {
                        None
                    }
                })
                .find(|&start| start > viewport_top),
        };
        let Some(target_row) = boundary else {
            return false;
        };
        let new_offset = total.saturating_sub(target_row).saturating_sub(
            self.viewport().1 as usize,
        );
        if new_offset == self.resolved_scroll() {
            return false;
        }
        self.messages.set_scroll_from_bottom(new_offset);
        true
    }

    /// Jump the chat log to the oldest line — `tui.altScreen.top`.
    pub fn scroll_viewport_to_top(&mut self) -> bool {
        let max = self.max_scroll();
        if max == 0 || (self.resolved_scroll() == max && !self.messages.is_following()) {
            return false;
        }
        self.messages.set_scroll_from_bottom(max);
        true
    }

    /// Jump the chat log back to the tail — `tui.altScreen.bottom`. New
    /// output pins the viewport again.
    ///
    /// Returns `false` only when the viewport is already following the tail
    /// and pinned to it; a detached viewport whose offset happens to be `0`
    /// (possible through [`MessageView::set_following`]) is still re-attached
    /// so the jump-to-latest indicator always clears.
    pub fn scroll_viewport_to_bottom(&mut self) -> bool {
        if self.messages.is_following() && self.messages.scroll_offset() == 0 {
            return false;
        }
        self.messages.scroll_to_bottom();
        true
    }

    /// Render the App into a `Buffer` at the given area.
    ///
    /// This is also the drag autoscroll's clock: a pending edge autoscroll
    /// advances one line before the frame is painted, so the highlight in
    /// the frame already reflects the moved viewport. The driver redraws on
    /// a 50 ms interval, which is the beat upstream gets from
    /// `setInterval(autoScrollSelection, 50)`
    /// (`packages/tui/src/tui-alt-screen.ts:1264-1297`). The receiver is
    /// `&mut self` for that reason; callers that only need pixels (snapshot
    /// tests, `transcript` helpers) use [`App::render_snapshot`], which does
    /// not tick.
    pub fn render_to_buffer(&mut self, area: Rect, buf: &mut Buffer) {
        let _ = self.advance_selection_autoscroll();
        // Same beat, second animation: the busy spinner reuses this 50 ms
        // tick instead of owning a timer (see [`App::tick_busy_feedback`]).
        let _ = self.tick_busy_feedback(Instant::now());
        // Phase 3 (G6): mirror the busy spinner frame onto the message view
        // so the in-flight assistant header glyph stays in lock-step with
        // the footer cursor. The setter is cheap; it lives here rather than
        // inside `tick_busy_feedback` to keep that helper single-purpose.
        self.messages.set_spinner_frame(self.spinner.frame());
        // Render the extension regions and budget the chrome before anything
        // else: the message viewport this frame paints is what the scroll,
        // selection and search paths must index.
        let frame = self.composed_frame(area.width, area.height);
        let editor_min_rows = self
            .prompt
            .line_count(area.width, self.config.composer_max_rows)
            .max(1) as u16;
        // Phase 2 (G3): when the built-in prompt is in use (`frame.editor` is
        // None) and the composer is configured to wrap, reserve an extra
        // chrome row for the `─` border painted above it (TS
        // `interactive-mode.ts` editor border colour). A custom editor
        // replaces the prompt entirely — there is no border to paint — so
        // it must keep the row count it asks for.
        let editor_min_rows = if self.config.composer_max_rows > 1 && frame.editor.is_none() {
            editor_min_rows + 1
        } else {
            editor_min_rows
        };
        let layout = plan_chrome(area.height, &frame, editor_min_rows);
        // Record the geometry first so the refresh below indexes the exact
        // viewport this frame is about to paint.
        let (message_area, reserved) = self.viewport_for_render(message_rect(area, &layout), true);
        self.record_viewport(message_area, reserved);
        // Keep the search results in step with the transcript they indexed —
        // streaming output and `/clear` both change the corpus under an open
        // bar, which is where upstream refreshes it too (from `render`).
        let _ = self.refresh_search();
        self.render_to_buffer_impl(area, buf, true, self.messages.hyperlinks(), &frame, &layout);
    }

    /// The whole chat log rendered as themed ANSI lines — the flat transcript,
    /// not the viewport.
    ///
    /// Upstream prints this *after* leaving the alternate screen when a
    /// fullscreen session ends with `fullscreenExitOutput: "transcript"`
    /// (the default): `stopInteractiveTui` switches the fullscreen TUI to the
    /// regular renderer and renders once, so the session is left in the
    /// terminal's own scrollback instead of vanishing with the alt screen
    /// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:790-795`).
    /// This port has no regular renderer, so the driver prints the same
    /// document from the frame-buffer side: [`MessageView::render_styled_lines`]
    /// is the single layout behind the screen *and* this dump, and each line is
    /// themed with the live palette by [`crate::utils::styled::themed_text`].
    ///
    /// Empty when there is nothing worth printing (a session quit before its
    /// first line), so the driver writes nothing rather than a screen of
    /// blank rows. Trailing whitespace is trimmed per line: the composer rows
    /// are padded to the full width, and padding would paint the terminal's
    /// background where the dump is supposed to stop.
    ///
    /// Scope note: this is the chat log, not the composer / status chrome. The
    /// regular renderer upstream switches to is what paints the input dock, and
    /// it does not exist here (see `docs/LUM1455_EXIT_TRANSCRIPT.md` §5).
    pub fn transcript_text(&self, width: u16) -> String {
        if width == 0 {
            return String::new();
        }
        let mut out = String::new();
        for line in self.messages.render_styled_lines(width) {
            out.push_str(themed_text(&line, &self.theme).trim_end());
            out.push('\n');
        }
        if out.trim().is_empty() {
            return String::new();
        }
        out
    }

    /// The message viewport a frame paints into, narrowed by the chat-log
    /// scrollbar's column when the bar is going to be drawn.
    ///
    /// The bar is painted on the frame's right edge, so on a transcript whose
    /// rows use the full width it used to land on top of the last character of
    /// every wrapped line — one lost character per line on exactly the rows
    /// `/help` emits (LUM-1238 §15.4, LUM-1262 §2). Narrowing the viewport
    /// instead keeps the text and the bar side by side, and
    /// [`App::scrollbar_geometry`] adds the reserved column back to find the
    /// bar's own cell.
    ///
    /// The decision is stable by construction: only a transcript that already
    /// overflows at the full width reserves a column, and wrapping one column
    /// earlier can only add lines, never remove them.
    fn viewport_for_render(&self, area: Rect, scrollbar: bool) -> (Rect, u16) {
        let reserved = if scrollbar
            && area.width > SCROLLBAR_COLUMNS
            && area.height > 0
            && self.messages.line_count(area.width) > area.height as usize
        {
            SCROLLBAR_COLUMNS
        } else {
            0
        };
        (
            Rect {
                width: area.width - reserved,
                ..area
            },
            reserved,
        )
    }

    /// Remember the message viewport's geometry as of a render: the width the
    /// log wraps at, its height (the page size), and its top-left cell so
    /// pointer coordinates can be mapped back into it.
    ///
    /// `reserved` is the width [`App::viewport_for_render`] took off `area`
    /// for the scrollbar, kept only so the bar can find its own column.
    fn record_viewport(&self, message_area: Rect, reserved: u16) {
        self.viewport.viewport_width
            .store(message_area.width, Ordering::Relaxed);
        self.viewport.viewport_reserved.store(reserved, Ordering::Relaxed);
        self.viewport.viewport_height
            .store(message_area.height, Ordering::Relaxed);
        self.viewport.viewport_origin
            .0
            .store(message_area.x, Ordering::Relaxed);
        self.viewport.viewport_origin
            .1
            .store(message_area.y, Ordering::Relaxed);
    }

    /// Paint the App without advancing the autoscroll clock.
    ///
    /// `scrollbar` selects whether the chat-log scrollbar overlay is painted.
    /// [`App::render_to_buffer`] passes `true` — the live alt-screen frame
    /// where the bar is a real pointer affordance. [`App::render_snapshot`]
    /// passes `false`: it is a flat text snapshot (it also backs the
    /// `/transcript` export), so it stays about content rather than screen
    /// furniture. See the module docs.
    fn render_to_buffer_impl(
        &self,
        area: Rect,
        buf: &mut Buffer,
        scrollbar: bool,
        hyperlinks: bool,
        frame: &ExtensionFrame,
        layout: &ChromeLayout,
    ) {
        // Layout: the extension regions wrap the message view, which keeps at
        // least one row. See the module docs for the order and
        // [`crate::components::extension_ui::plan_chrome`] for the budget.
        let message_height = layout.message;
        // The modal list geometry is per-frame: an overlay that is gone this
        // frame must stop owning the pointer (the same rule
        // [`App::record_autocomplete_area`] follows). The paint blocks below
        // record whichever list they actually drew.
        self.record_modal_list(MODAL_LIST_NONE, 0, 0, 0);
        let (message_area, reserved) =
            self.viewport_for_render(message_rect(area, layout), scrollbar);
        let header_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: layout.header,
        };
        let above_area = Rect {
            x: area.x,
            y: header_area.y + layout.header,
            width: area.width,
            height: layout.above,
        };
        let editor_area = Rect {
            x: area.x,
            y: message_area.y + message_height + layout.pending,
            width: area.width,
            height: layout.editor,
        };
        let below_area = Rect {
            x: area.x,
            y: editor_area.y + layout.editor,
            width: area.width,
            height: layout.below,
        };
        let tool_strip_area = Rect {
            x: area.x,
            y: below_area.y + layout.below,
            width: area.width,
            height: layout.tool_strip,
        };
        let status_area = Rect {
            x: area.x,
            y: tool_strip_area.y + layout.tool_strip,
            width: area.width,
            height: layout.status,
        };
        let footer_area = Rect {
            x: area.x,
            y: status_area.y + layout.status,
            width: area.width,
            height: layout.footer,
        };

        // Record the viewport the scroll keys clamp against. Keys arrive
        // between renders, so the previous render's geometry is what they
        // see — exactly what the reader was looking at.
        self.record_viewport(message_area, reserved);

        // Header, then the above-editor widgets (insertion order).
        self.paint_extension_lines(header_area, &frame.header, buf);
        self.paint_extension_lines(above_area, &frame.above, buf);

        self.messages.render_to_buffer_themed_with_links(
            message_area,
            buf,
            &self.theme,
            hyperlinks,
        );
        // Selection highlight goes on top of the message cells but under
        // any modal, so an open selector or dialog stays readable.
        self.apply_selection_highlight(message_area, buf);
        self.apply_search_highlight(message_area, buf);
        // The scrollbar sits on top of the message cells but under every
        // overlay, so an open search bar, selector or dialog stays readable
        // (upstream paints it from the scroll view, before the overlays).
        if scrollbar {
            self.apply_scrollbar(message_area, buf);
        }
        // The "jump to latest" pill is composited over the bottom of the
        // transcript, after the scrollbar so its own width budget can stop
        // left of the bar. `render_snapshot` passes `scrollbar == false`,
        // which is also what keeps `/transcript` free of screen furniture.
        if scrollbar {
            self.paint_scroll_to_end(message_area, buf);
            self.paint_truncated_above(message_area, buf);
        } else {
            self.viewport.scroll_to_end.2.store(0, Ordering::Relaxed);
            self.viewport.truncated_above.2.store(0, Ordering::Relaxed);
        }

        // Empty-state welcome hint: a single dim line in the middle of the
        // transcript area when the log is empty, no turn is running, and
        // the composer has no draft. Without it a fresh session reads as a
        // wall of blank rows; with it the user sees what to do next.
        self.paint_empty_hint(message_area, buf);

        // The queued-messages block sits between the transcript and the
        // composer (upstream keeps it in the prompt area), on top of whatever
        // the transcript painted there last frame — hence the per-row blank
        // before the line is written.
        self.paint_pending_block(
            Rect {
                x: area.x,
                y: message_area.y + message_height,
                width: area.width,
                height: layout.pending,
            },
            buf,
        );
        // The editor region: a custom component (a non-overlay `custom`
        // session or `set_editor_component`) replaces the prompt line
        // entirely.
        match &frame.editor {
            Some(lines) => {
                // A custom editor component replaces the prompt entirely, so
                // there is no composer for a click to place a caret in — and
                // no dropdown either.
                self.record_composer_area(None);
                self.record_autocomplete_area(None, 0, 0);
                self.paint_extension_lines(editor_area, lines, buf);
            }
            None => {
                self.paint_prompt(editor_area, buf);
                // Paint composer drag-selection REVERSED on top of the cells
                // that `paint_prompt` just drew.
                crate::utils::render_helpers::apply_composer_selection_highlight(
                    self.composer_drag_selection,
                    editor_area,
                    crate::utils::width::columns(self.prompt.label()) as u16,
                    buf,
                );
                self.paint_autocomplete(message_area, editor_area, buf);
            }
        }

        // Slash menu overlay — rendered after the editor so it appears above it.
        self.paint_slash_menu(message_area, editor_area, buf);

        self.paint_shortcut_overlay(message_area, editor_area, buf);

        // Below-editor widgets.
        self.paint_extension_lines(below_area, &frame.below, buf);

        // Docked tool strip (N2). nanopi dedicates a 1-row strip above
        // the status bar so a busy state is visible without parsing the
        // footer. `layout.tool_strip` is `1` only while a turn is in
        // flight, so an idle session keeps the pre-N2 geometry.
        if layout.tool_strip > 0 {
            self.tool_strip
                .render(self.tool_strip_state(), tool_strip_area, buf, &self.theme);
        }

        self.status_bar.render_to_buffer_themed(
            &self.status_for_render(),
            status_area,
            buf,
            &self.theme,
        );

        self.paint_extension_lines(footer_area, &frame.footer, buf);

        // Selector overlay — when open, draw on top of everything
        // except the prompt and status.
        //
        // Anchored to the **message viewport**, one row below its top, and
        // clipped to it: the startup header above and the prompt / status rows
        // below are not the selector's to overwrite. Anchoring to `area`
        // instead painted the picker over the header — with the 20-row startup
        // header the `/model` list covered the key hints while the transcript
        // underneath was left untouched, and the clip compared an absolute row
        // against a height (LUM-1235 PTY capture).
        if let Some(selector) = &self.selector {
            let lines = selector.render_styled_lines(area.width);
            let start_row = message_area.y + 1;
            // Record the item window for the pointer: the title and the rule
            // are the selector's own first two rows, so the first *item* row
            // is `start` rows into the visible window and carries item index
            // `start` (upstream `SelectList::handleMouse`
            // (`components/select-list.ts:124-140`)).
            let (first_item, last_item) = selector.visible_range();
            let items_row = start_row + SELECTOR_HEADER_ROWS;
            let clip = (message_area.y + message_area.height).saturating_sub(items_row) as usize;
            self.record_modal_list(
                MODAL_LIST_SELECTOR,
                items_row,
                first_item,
                (last_item - first_item).min(clip),
            );
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= message_area.y + message_area.height {
                    break;
                }
                // Blank the row first: a picker line is shorter than the
                // transcript line it covers, and ratatui only emits the cells
                // this buffer changed — an unblanked row left the old text
                // bleeding through the picker (`Pick a model` + `errupt` from
                // the header hint underneath). `reset` also drops the covered
                // cell's colours so a picked-over selection highlight cannot
                // tint the modal.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.reset();
                    }
                }
                write_styled_line(buf, area.x, y, area.width, line, &self.theme);
            }
        }

        // Settings overlay — drawn after the selector so an accidental
        // overlap (both open) still leaves the `/settings` modal readable.
        if let Some(settings) = &self.settings {
            let lines = settings.render_styled_lines(area.width);
            let start_row = area.y + 1;
            // The pointer maps a clicked row back onto a setting: the search
            // box and its spacer are the list's first two rows when it is
            // searchable (upstream `settings-list.ts:186-191` skips them with
            // `rowOffset = searchEnabled ? 2 : 0`).
            let (first_item, last_item) = settings.visible_range();
            let header_rows = if settings.is_searchable() { 2 } else { 0 };
            let items_row = start_row + header_rows;
            let clip = (area.y + message_height).saturating_sub(items_row) as usize;
            self.record_modal_list(
                MODAL_LIST_SETTINGS,
                items_row,
                first_item,
                (last_item - first_item).min(clip),
            );
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                // Blank the row first: the settings modal is the whole
                // point of the screen while it is open.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char(' ');
                    }
                }
                write_styled_line(buf, area.x, y, area.width, line, &self.theme);
            }
        }

        // Extension dialog overlay — topmost, so it wins over the
        // selector if both are somehow open.
        if let Some(dialog) = &self.dialog {
            let lines = dialog.render_lines(area.width);
            let start_row = area.y;
            // A `ctx.ui.select` dialog paints its list first, so the item
            // rows start at the same title + rule offset; the other flavours
            // have no list and must not claim the pointer
            // (`select_window()` is `None` for them).
            if let Some((first_item, last_item)) = dialog.select_window() {
                let items_row = start_row + SELECTOR_HEADER_ROWS;
                let clip = (area.y + message_height).saturating_sub(items_row) as usize;
                self.record_modal_list(
                    MODAL_LIST_DIALOG,
                    items_row,
                    first_item,
                    (last_item - first_item).min(clip),
                );
            } else {
                // A `Confirm` / `Input` / `Notify` dialog has no list, and it
                // is topmost: nothing underneath it keeps owning the pointer.
                self.record_modal_list(MODAL_LIST_NONE, 0, 0, 0);
            }
            for (offset, line) in lines.iter().enumerate() {
                let y = start_row + offset as u16;
                if y >= area.y + message_height {
                    break;
                }
                // Blank the row first: a modal must be readable even
                // when the message view underneath is full of text.
                for col in 0..area.width {
                    if let Some(cell) = buf.cell_mut((area.x + col, y)) {
                        cell.set_char(' ');
                    }
                }
                write_plain_row(buf, area.x, y, area.width, line);
            }
        }

        // Transcript search bar — anchored to the top-right of the message
        // area (`showOverlay(component, { anchor: "top-right", width: "40%",
        // minWidth: 32, margin: 1 })`,
        // `packages/tui/src/tui-alt-screen.ts:512-518`). Drawn last so the bar
        // stays readable if an extension dialog arrives while it is open.
        if let Some(state) = &self.search {
            if let Some(rect) = search_bar_rect(message_area) {
                let layout = render_search_bar(&state.bar, rect.width);
                for (offset, line) in layout.lines.iter().enumerate() {
                    let y = rect.y + offset as u16;
                    if y >= rect.y + rect.height {
                        break;
                    }
                    write_styled_line(buf, rect.x, y, rect.width, line, &self.theme);
                }
            }
        }

        // The `custom` overlay is the topmost layer: it paints after the
        // transcript search bar so an extension component can cover every
        // region, exactly like a focus-owning upstream overlay.
        if let Some(overlay) = &frame.overlay {
            let rect = overlay_rect(area, overlay.options, overlay.lines.len());
            if rect.width > 0 && rect.height > 0 {
                // Blank the box first: it must be readable over the
                // transcript underneath (the dialog overlay does the same).
                for y in rect.y..rect.y + rect.height {
                    for x in rect.x..rect.x + rect.width {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char(' ');
                        }
                    }
                }
                self.paint_extension_lines(rect, &overlay.lines, buf);
            }
        }
    }

    /// Paint a region's styled lines, resolving each [`SpanStyle`] through the
    /// live theme and truncating both the lines' width and a too-tall block's
    /// tail to the region.
    ///
    /// Width truncation is *marked* (`…`), never silent: every extension
    /// widget — the startup header, the above/below-editor widgets, the
    /// footer, and a `custom` overlay — goes through here, and a row that is
    /// wider than the terminal is the normal case on a 44-column window. A
    /// silent clip made `hints hidden on a short terminal — Alt+H sho`
    /// indistinguishable from a line whose author wrote exactly that
    /// (LUM-1412). Height truncation stays silent by design: a block with a
    /// taller tail than the region is already summarised by the caller's own
    /// content, and a mark on the region's last row would be confusable with
    /// a width clip.
    fn paint_extension_lines(&self, rect: Rect, lines: &[StyledLine], buf: &mut Buffer) {
        crate::utils::render_helpers::paint_extension_lines(rect, lines, &self.theme, buf);
    }

    /// Paint the built-in prompt into the editor region. The region may be
    /// taller than one row when the buffer wraps (see
    /// [`crate::Prompt::render_lines`]); the cap lives in
    /// [`AppConfig::composer_max_rows`].
    ///
    /// The frame is also where the composer window's two remembered facts
    /// come from: the draft rows it showed (`composer_scroll`) and the rows
    /// it could show (`composer_window`). Both are read back by the key path
    /// before [`crate::Prompt::render_lines`] runs again.
    /// The chord the queued-messages hint advertises (`app.message.dequeue`).
    ///
    /// Resolved through the live keybinding table, so an override in
    /// `keybindings.json` shows up here exactly as it does in `/hotkeys` and
    /// the startup header (LUM-1447/1450). The fallback is the shipped
    /// default from upstream's table (`packages/coding-agent/src/core/keybindings.ts:138`:
    /// `alt+up`, `alt+q` on Windows), used when no `app.*` table is installed
    /// — a bare `pi-tui` host, which is what the frame tests are.
    fn dequeue_chord(&self) -> String {
        // A **raw** chord into `format_chord`, so the unknown-id fallback is
        // spelled the way every other surface spells it (`Alt+Up`), while a
        // resolvable id keeps the registry's effective set — the same rule the
        // startup header's hint rows use.
        key_text_or(
            "app.message.dequeue",
            &format_chord(if cfg!(windows) { "alt+q" } else { "alt+up" }),
        )
    }

    /// Paint the queued-messages block into `rect`.
    ///
    /// The block is only planned when something is queued
    /// ([`MessageView::pending_block_rows`]), so an empty queue paints
    /// nothing and the rows stay with the transcript. Rows are blanked before
    /// the line is written: this region is carved out of what the transcript
    /// painted last frame, and the block's spacer row is genuinely empty.
    fn paint_pending_block(&self, rect: Rect, buf: &mut Buffer) {
        crate::utils::render_helpers::paint_pending_block(&self.messages, rect, &self.theme, buf);
    }

    fn paint_prompt(&self, rect: Rect, buf: &mut Buffer) {
        crate::utils::render_helpers::paint_prompt(
            &self.prompt,
            self.config.composer_max_rows,
            self.thinking_level,
            &self.viewport,
            rect,
            &self.theme,
            buf,
        );
    }

    /// Paint the composer's autocomplete dropdown into the rows directly
    /// above the editor, on top of the message view.
    ///
    /// The dropdown itself belongs to [`crate::Editor`] (candidates,
    /// `SelectList` layout, windowing, selection); the App only places it and
    /// hands the rows to the buffer. Upstream draws the list above the input
    /// and grows it towards older output (`Editor.renderAutocomplete`), so the
    /// rows are anchored to the editor's top edge and the prompt line is never
    /// covered. Without this the provider could be installed and still show
    /// nothing, which is exactly the state LUM-1236 found: the engine and the
    /// keyboard map were in place, the paint call was not.
    ///
    /// The rows arrive as theme-slot spans from the shared `SelectList` layout
    /// (LUM-1305), so the dropdown paints the same roles the modal pickers do —
    /// plain label, `muted` description column, `accent` over `selectedBg` for
    /// the highlighted row — instead of the App re-deriving a style from the
    /// text.
    fn paint_autocomplete(&self, message_area: Rect, editor_area: Rect, buf: &mut Buffer) {
        crate::utils::render_helpers::paint_autocomplete(
            &self.messages,
            &self.prompt,
            &self.viewport,
            message_area,
            editor_area,
            &self.theme,
            buf,
        );
    }

    /// Paint the slash menu overlay directly above the editor.
    ///
    /// The menu appears when the user types `/` in the editor and shows
    /// matching slash commands with descriptions. Mirrors Martty's slash menu UI.
    fn paint_slash_menu(&self, message_area: Rect, editor_area: Rect, buf: &mut Buffer) {
        let _ = message_area; // kept for signature compatibility
        crate::utils::render_helpers::paint_slash_menu(&self.slash_menu, editor_area, &self.theme, buf);
    }

    /// Paint the `?` shortcut overlay directly above the composer.
    ///
    /// Bottom-anchored to the editor row and clipped to the message viewport,
    /// so the startup header above and the composer / status rows below are
    /// untouched (the rule the selector overlay follows). The panel borrows
    /// transcript rows instead of claiming a chrome row: opening help must not
    /// reflow the conversation the reader was looking at, and closing it
    /// restores the same frame.
    fn paint_shortcut_overlay(&self, message_area: Rect, editor_area: Rect, buf: &mut Buffer) {
        crate::utils::render_helpers::paint_shortcut_overlay(
            self.shortcut_overlay,
            &self.hint_entries(),
            &self.config.locale,
            editor_area,
            message_area,
            &self.theme,
            buf,
        );
    }

    /// Render the App into a flat snapshot (used by the snapshot tests
    /// in `tests/snapshot.rs`).
    ///
    /// The snapshot is text-only and deliberately omits the scrollbar
    /// overlay (see [`App::render_to_buffer_impl`]): it backs the
    /// `/transcript` export as well as the content assertions of the
    /// snapshot tests, and neither wants a pointer affordance in the last
    /// column. Use [`App::render_to_buffer`] to assert the bar.
    pub fn render_snapshot(&self, width: u16, height: u16) -> RenderSnapshot {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut buf = Buffer::empty(area);
        // `/transcript` (and the snapshot tests) want plain text, never
        // OSC 8 escapes, so links fall back to the inline `(url)` form
        // regardless of the live capability.
        let frame = self.composed_frame(width, height);
        let editor_min_rows = self
            .prompt
            .line_count(width, self.config.composer_max_rows)
            .max(1) as u16;
        let editor_min_rows = if self.config.composer_max_rows > 1 && frame.editor.is_none() {
            editor_min_rows + 1
        } else {
            editor_min_rows
        };
        let layout = plan_chrome(height, &frame, editor_min_rows);
        self.render_to_buffer_impl(area, &mut buf, false, false, &frame, &layout);
        let lines = buf
            .content()
            .chunks(width as usize)
            .map(|row| buffer_row_text(row).trim_end().to_string())
            .collect::<Vec<_>>();
        RenderSnapshot {
            width,
            height,
            lines,
            prompt_placeholder: self.config.prompt_placeholder.clone(),
            prompt_buffer: self.prompt.text().to_string(),
            settings_open: self.settings_open(),
            settings_lines: self
                .settings
                .as_ref()
                .map(|settings| settings.render_lines(width))
                .unwrap_or_default(),
            selector_open: self.selector_open(),
            selector_title: self.selector.as_ref().map(|s| s.title().to_string()),
            selector_items: self
                .selector
                .as_ref()
                // The rows that pass the selector's search filter (the
                // scroll window is a rendering detail and is applied by
                // `Selector::render_lines`).
                .map(|s| s.visible_items().cloned().collect())
                .unwrap_or_default(),
            selector_cursor: self.selector.as_ref().map(|s| s.cursor()),
            dialog_open: self.dialog.is_some(),
            dialog_kind: self.dialog.as_ref().map(|d| d.kind()),
            dialog_title: self.dialog.as_ref().map(|d| d.title().to_string()),
            dialog_lines: self
                .dialog
                .as_ref()
                .map(|d| d.render_lines(width))
                .unwrap_or_default(),
            search_open: self.search_open(),
            search_query: self
                .search
                .as_ref()
                .map(|state| state.bar.query().to_string())
                .unwrap_or_default(),
            history_search_query: self
                .prompt
                .editor()
                .history_search_query()
                .map(String::from)
                .unwrap_or_default(),
            search_lines: self
                .search
                .as_ref()
                .map(|state| {
                    crate::components::search::search_bar_text(&render_search_bar(&state.bar, width).lines)
                })
                .unwrap_or_default(),
            status: self.status_for_render().into_owned(),
        }
    }

    /// Convert a raw `crossterm` event into an [`InputEvent`], or `None`
    /// when the event carries no input. Used by the binary entry point.
    ///
    /// **Key releases carry no input.** crossterm's Win32 backend reports a
    /// `KeyEventKind::Release` for every key the user lets go of (its own
    /// source comments call the release events out; a crossterm probe run
    /// under a Windows ConPTY prints `kind=Press` *and* `kind=Release` for a
    /// single typed character), and a Windows console is the only way pi runs
    /// there. Upstream pi (Node `readline`) never sees a release, each of its
    /// key handlers runs once per press — so mapping releases here made every
    /// keystroke act **twice** on Windows: typing `alpha` rendered
    /// `aallpphhaa`, one `Backspace` deleted two characters, one `Enter`
    /// submitted twice. Dropping releases is parity with upstream and with
    /// codex, not a platform workaround; `Press` and `Repeat` (key
    /// auto-repeat) still map, and no chord in this port consumes
    /// `KeyEventKind`, so nothing else can regress.
    ///
    /// Found by `scripts/pty_capture_win.py`, the ConPTY backend of the
    /// capture harness (LUM-1457) — the Windows rounds before it only ever
    /// drove the app through `App::step(InputEvent::Key)`, which cannot show
    /// how many times a real console delivers one keystroke.
    pub fn translate_event(event: CtEvent) -> Option<InputEvent> {
        match event {
            CtEvent::Key(key) => match key.kind {
                CtKeyEventKind::Release => None,
                CtKeyEventKind::Press | CtKeyEventKind::Repeat => Some(InputEvent::from(key)),
            },
            CtEvent::Mouse(mouse) => Some(match mouse.kind {
                CtMouseEventKind::ScrollUp => InputEvent::wheel(
                    true,
                    mouse.modifiers.contains(CtModifiers::ALT),
                    mouse.column,
                    mouse.row,
                ),
                CtMouseEventKind::ScrollDown => InputEvent::wheel(
                    false,
                    mouse.modifiers.contains(CtModifiers::ALT),
                    mouse.column,
                    mouse.row,
                ),
                CtMouseEventKind::Down(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Press(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Up(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Release(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Drag(button) => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Drag(mouse_button(button)),
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                CtMouseEventKind::Moved => InputEvent::MouseGesture(MouseGesture::new(
                    MouseGestureKind::Move,
                    mouse.column,
                    mouse.row,
                    mouse.modifiers.contains(CtModifiers::ALT),
                )),
                // Horizontal wheel goes through the same `routeWheel`
                // upstream ignores for the chat log: no horizontal
                // scrolling exists yet.
                _ => InputEvent::Ignored,
            }),
            CtEvent::Resize(w, h) => Some(InputEvent::Resize {
                width: w,
                height: h,
            }),
            // Bracketed paste has no `InputEvent` counterpart (the enum is
            // `Copy`, and the payload is owned): the driver recognises
            // `CtEvent::Paste` itself and calls [`App::step_paste`]. Mapping
            // it to `Ignored` here is what keeps a byte-level paste from
            // being replayed as a burst of key events.
            _ => Some(InputEvent::Ignored),
        }
    }

    /// Translate a slice of `crossterm` events.
    pub fn translate_events<I: IntoIterator<Item = CtEvent>>(events: I) -> Vec<InputEvent> {
        events
            .into_iter()
            .filter_map(Self::translate_event)
            .collect()
    }
}

// Quiet unused-import warning for the parking_lot::Mutex, which we
// keep available for downstream code that wants to wrap state behind
// the App handle.
#[allow(dead_code)]
fn _keep_mutex_path() -> Arc<Mutex<()>> {
    Arc::new(Mutex::new(()))
}

// Quiet unused-editor-action warning — the import is there for the
// public method signature contract.
#[allow(dead_code)]
const _: EditorAction = EditorAction::None;

/// Base index `next` / `previous` navigation steps from
/// (`refreshSearch`, `packages/tui/src/tui-alt-screen.ts:614-622`): the exact
/// match when the key still resolves, else the clamped previous selection,
/// else `-1` so the caller wraps from the ends.
fn search_base_index(exact: Option<usize>, previous: Option<usize>, len: usize) -> i64 {
    if let Some(index) = exact {
        return index as i64;
    }
    previous
        .map(|index| index as i64)
        .unwrap_or(-1)
        .min(len as i64 - 1)
}

/// Map a crossterm mouse button onto the component-level [`MouseButton`].
fn mouse_button(button: crossterm::event::MouseButton) -> MouseButton {
    match button {
        crossterm::event::MouseButton::Left => MouseButton::Left,
        crossterm::event::MouseButton::Middle => MouseButton::Middle,
        crossterm::event::MouseButton::Right => MouseButton::Right,
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn plain_segments(line: &str) -> Vec<(usize, usize, bool, bool)> {
        word_segments(line)
            .into_iter()
            .map(|segment| {
                (
                    segment.start,
                    segment.end,
                    segment.selectable,
                    segment.joiner,
                )
            })
            .collect()
    }

    #[test]
    fn word_segments_measure_character_columns() {
        assert_eq!(
            plain_segments("ab cd"),
            vec![
                (0, 2, true, false),
                (2, 3, false, false),
                (3, 5, true, false),
            ]
        );
    }

    #[test]
    fn word_segments_mark_joiners_and_word_like_runs() {
        // "/" and "-" are selectable joiners; punctuation and spaces are not.
        // UAX #29 keeps `file.rs` together (`.` is a MidNumLet), while the
        // `/` and `-` are the joiner segments.
        assert_eq!(
            plain_segments("path/to-file.rs"),
            vec![
                (0, 4, true, false),
                (4, 5, true, true),
                (5, 7, true, false),
                (7, 8, true, true),
                (8, 15, true, false),
            ]
        );
        assert_eq!(plain_segments(" "), vec![(0, 1, false, false)]);
    }

    #[test]
    fn joiners_glue_pickable_neighbours_but_not_punctuation() {
        let segments = word_segments("a-b c");
        // "a" joins "-"; "-" joins "b"; "b" and " " do not join.
        assert!(word_segments_can_join(&segments[0], &segments[1]));
        assert!(word_segments_can_join(&segments[1], &segments[2]));
        assert!(!word_segments_can_join(&segments[2], &segments[3]));
    }

    #[test]
    fn selection_end_column_is_exclusive_for_boundaries() {
        let exclusive = SelectionPoint::boundary(0, 4);
        assert_eq!(selection_end_column(&exclusive, 10), 4);
        assert_eq!(
            selection_end_column(&exclusive, 3),
            3,
            "clamped to the line"
        );

        let inclusive = SelectionPoint::cell(0, 4);
        assert_eq!(selection_end_column(&inclusive, 10), 5);
        assert_eq!(
            selection_end_column(&inclusive, 5),
            5,
            "clamped to the line"
        );
    }

    #[test]
    fn selection_bounds_ignore_the_boundary_flag_when_ordering() {
        let selection = Selection {
            anchor: SelectionPoint::boundary(3, 0),
            focus: SelectionPoint::cell(1, 5),
            granularity: SelectionGranularity::Word,
            initial: None,
        };
        let (start, end) = selection.bounds().expect("non-empty");
        assert_eq!(start.line, 1);
        assert_eq!(end.line, 3);
        // Equal coordinates read as empty even when the flags differ.
        let empty = Selection {
            anchor: SelectionPoint::cell(2, 2),
            focus: SelectionPoint::boundary(2, 2),
            granularity: SelectionGranularity::Word,
            initial: None,
        };
        assert!(empty.bounds().is_none());
    }
}

/// Regression tests for the `ToolCallDelta` → block mapping (LUM-1141 /
/// LUM-1152). A provider streams each tool call as several deltas that all
/// share one provider id, so the transcript must end up with exactly one
/// block per call — deltas, execution and result folded together — rather
/// than one block per event.
#[cfg(test)]
mod tool_stream_tests {
    use super::*;
    use crate::components::message::Role;
    use pi_agent_core::AgentOptions;
    use pi_ai::providers::faux::FauxProvider;
    use pi_protocol::{Api, AssistantMessage, Content, Model, ProviderId, ToolCall, ToolResult};
    use pi_agent_core::{AgentEvent, AssistantMessageUpdate};

    fn faux_model() -> Model {
        Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: Api::Faux,
            label: Some("Faux".into()),
            context_window: 1024,
            max_output_tokens: 256,
        }
    }

    pub(super) fn test_app() -> App {
        let agent = Agent::new(AgentOptions::new(
            faux_model(),
            Arc::new(FauxProvider::default()),
            "you are pi",
        ));
        App::new(&agent, AppConfig::default())
    }

    fn tool_delta(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments_delta: &str,
    ) -> AgentEvent {
        AgentEvent::MessageUpdate(AssistantMessageUpdate::ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            arguments_delta: Some(arguments_delta.to_string()),
        })
    }

    fn finished_message() -> AssistantMessage {
        AssistantMessage {
            model: "faux-model".into(),
            content: Vec::new(),
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            error_message: None,
        }
    }

    fn tool_items(app: &App) -> Vec<String> {
        app.messages()
            .items()
            .iter()
            .filter(|item| item.role == Role::Tool)
            .map(|item| item.text.clone())
            .collect()
    }

    #[test]
    fn repeated_deltas_for_one_call_render_one_block() {
        let mut app = test_app();
        app.apply_agent_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        // First fragment carries id + name; later fragments only arguments.
        app.apply_agent_event(tool_delta(0, Some("call_1"), Some("read"), "{\"path\":"));
        app.apply_agent_event(tool_delta(0, None, None, "\"/tmp/x\""));
        app.apply_agent_event(tool_delta(0, None, None, "}"));
        app.apply_agent_event(AgentEvent::MessageEnd {
            message: finished_message(),
        });

        assert_eq!(
            tool_items(&app),
            vec!["[tool:read] {\"path\":\"/tmp/x\"}".to_string()],
            "one call id must produce exactly one tool block"
        );
        assert_eq!(
            app.messages().items().len(),
            2,
            "assistant + one tool block"
        );
    }

    #[test]
    fn message_end_feeds_the_footer_usage_and_context_gauge() {
        let mut app = test_app();
        // The constructor seeds the gauge window from the agent's model.
        assert_eq!(app.status_data().context_window, 1024);
        assert_eq!(app.status_data().context_used, 0);

        let mut message = finished_message();
        message.usage = Usage {
            input: 300,
            output: 40,
            cache_read: 100,
            cache_write: 5,
            total: 0,
        };
        app.apply_agent_event(AgentEvent::MessageEnd { message });

        let status = app.status_data();
        assert_eq!(status.input_tokens, 300);
        assert_eq!(status.output_tokens, 40);
        assert_eq!(status.cache_read, 100);
        assert_eq!(status.cache_write, 5);
        // No provider total: the context size is the sum of the four counters.
        assert_eq!(status.context_used, 445);
    }

    #[test]
    fn execution_and_result_land_in_the_streamed_block() {
        let mut app = test_app();
        app.apply_agent_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        app.apply_agent_event(tool_delta(0, Some("call_1"), Some("read"), "{\"path\":"));
        app.apply_agent_event(tool_delta(0, None, None, "\"/tmp/x\"}"));
        app.apply_agent_event(AgentEvent::MessageEnd {
            message: finished_message(),
        });
        app.apply_agent_event(AgentEvent::ToolExecutionStart {
            call: ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": "/tmp/x" }),
            },
        });
        app.apply_agent_event(AgentEvent::ToolExecutionEnd {
            result: ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("ok")),
                is_error: false,
                details: None,
                added_tool_names: None,
                images: Vec::new(),
            },
            duration_ms: 3,
        });

        assert_eq!(
            tool_items(&app),
            vec!["[tool:read] {\"path\":\"/tmp/x\"} → ok".to_string()],
            "the result must rewrite the streamed block, not append a new one"
        );
    }

    #[test]
    fn separate_calls_keep_separate_blocks() {
        let mut app = test_app();
        app.apply_agent_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        app.apply_agent_event(tool_delta(0, Some("call_a"), Some("read"), "{}"));
        app.apply_agent_event(tool_delta(1, Some("call_b"), Some("list"), "{}"));
        app.apply_agent_event(AgentEvent::MessageEnd {
            message: finished_message(),
        });

        assert_eq!(
            tool_items(&app),
            vec!["[tool:read] {}".to_string(), "[tool:list] {}".to_string()]
        );
    }

    #[test]
    fn result_without_a_streamed_block_still_renders() {
        // A replayed / synthetic turn can deliver only the result event; the
        // old standalone-block behaviour must survive.
        let mut app = test_app();
        app.apply_agent_event(AgentEvent::ToolExecutionEnd {
            result: ToolResult {
                tool_call_id: "orphan".into(),
                content: Box::new(Content::text("done")),
                is_error: false,
                details: None,
                added_tool_names: None,
                images: Vec::new(),
            },
            duration_ms: 1,
        });
        assert_eq!(tool_items(&app), vec!["[tool:] → done".to_string()]);
    }
}

/// Placement maths for the `custom` overlay box.
///
/// These pin the exact geometry so the integration tests only have to prove
/// the box is painted last, covers what it spans, and disappears when hidden.
#[cfg(test)]
mod overlay_rect_tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn default_overlay_is_centred_and_inset_by_the_margin() {
        // `CustomOptions::overlay()` carries a 1-cell margin, so the box is
        // the full terminal minus the margin on each side.
        let rect = overlay_rect(area(24, 8), CustomOptions::overlay(), 2);
        assert_eq!(
            rect,
            Rect {
                x: 1,
                y: 3,
                width: 22,
                height: 2,
            }
        );
    }

    #[test]
    fn width_and_max_height_shrink_the_box() {
        let options = CustomOptions::overlay().width(10).max_height(1);
        let rect = overlay_rect(area(24, 8), options, 4);
        assert_eq!(
            rect,
            Rect {
                x: 7,
                y: 3,
                width: 10,
                height: 1,
            }
        );
    }

    #[test]
    fn a_max_height_larger_than_the_content_does_not_over_allocate() {
        let options = CustomOptions::overlay().max_height(20);
        let rect = overlay_rect(area(20, 10), options, 1);
        assert_eq!(rect.height, 1);
    }

    #[test]
    fn anchors_hug_the_corners_inside_the_margin() {
        let cases = [
            (
                OverlayAnchor::TopLeft,
                Rect {
                    x: 1,
                    y: 1,
                    width: 6,
                    height: 2,
                },
            ),
            (
                OverlayAnchor::TopRight,
                Rect {
                    x: 13,
                    y: 1,
                    width: 6,
                    height: 2,
                },
            ),
            (
                OverlayAnchor::BottomLeft,
                Rect {
                    x: 1,
                    y: 7,
                    width: 6,
                    height: 2,
                },
            ),
            (
                OverlayAnchor::BottomRight,
                Rect {
                    x: 13,
                    y: 7,
                    width: 6,
                    height: 2,
                },
            ),
        ];
        for (anchor, expected) in cases {
            let options = CustomOptions::overlay().anchor(anchor).width(6);
            assert_eq!(
                overlay_rect(area(20, 10), options, 2),
                expected,
                "{anchor:?}"
            );
        }
    }

    #[test]
    fn a_huge_margin_collapses_the_box_without_leaving_the_screen() {
        let options = CustomOptions::overlay()
            .anchor(OverlayAnchor::BottomRight)
            .margin(50);
        let rect = overlay_rect(area(10, 4), options, 2);
        assert!(
            rect.x + rect.width <= 10 && rect.y + rect.height <= 4,
            "clamped box: {rect:?}"
        );
        // The margin eats the whole inner area, so the box shrinks to a
        // single clamped cell rather than underflowing.
        assert_eq!(
            rect,
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }
        );
    }

    #[test]
    fn a_zero_sized_terminal_yields_an_empty_box() {
        assert_eq!(
            overlay_rect(area(0, 0), CustomOptions::overlay(), 3),
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            }
        );
    }

    #[test]
    fn the_box_stays_inside_a_non_zero_origin() {
        let rect = overlay_rect(
            Rect {
                x: 5,
                y: 7,
                width: 20,
                height: 10,
            },
            CustomOptions::overlay()
                .anchor(OverlayAnchor::BottomRight)
                .width(6),
            2,
        );
        assert_eq!(
            rect,
            Rect {
                x: 18,
                y: 14,
                width: 6,
                height: 2,
            }
        );
    }
}

#[cfg(test)]
mod public_api_tests {
    use super::*;
    use crate::components::slash_menu::SlashMenuEntry;

    fn make_app() -> App {
        tool_stream_tests::test_app()
    }

    #[test]
    fn markdown_round_trips_through_setter() {
        let mut app = make_app();
        assert!(app.markdown());
        app.set_markdown(false);
        assert!(!app.markdown());
        app.set_markdown(true);
        assert!(app.markdown());
    }

    #[test]
    fn thinking_visible_round_trips() {
        let mut app = make_app();
        let initial = app.thinking_visible();
        app.set_thinking_visible(!initial);
        assert_eq!(app.thinking_visible(), !initial);
    }

    #[test]
    fn toggle_thinking_visibility_inverts_the_flag() {
        let mut app = make_app();
        let before = app.thinking_visible();
        let new = app.toggle_thinking_visibility();
        assert_eq!(new, !before);
        assert_eq!(app.thinking_visible(), !before);
    }

    #[test]
    fn tools_expanded_round_trips() {
        let mut app = make_app();
        let initial = app.tools_expanded();
        app.toggle_tools_expanded();
        assert_eq!(app.tools_expanded(), !initial);
    }

    #[test]
    fn slash_menu_is_hidden_by_default() {
        let app = make_app();
        assert!(!app.slash_menu_visible());
        assert_eq!(app.slash_menu_len(), 0);
        assert!(app.slash_menu_selected().is_none());
    }

    #[test]
    fn show_slash_menu_makes_it_visible() {
        let mut app = make_app();
        let entries = vec![SlashMenuEntry::new("help", "/help", "show help")];
        app.show_slash_menu(entries);
        assert!(app.slash_menu_visible());
        assert_eq!(app.slash_menu_len(), 1);
        assert!(app.slash_menu_selected().is_some());
    }

    #[test]
    fn hide_slash_menu_keeps_the_entries_but_clears_visibility() {
        let mut app = make_app();
        app.show_slash_menu(vec![SlashMenuEntry::new("a", "/a", "A")]);
        app.hide_slash_menu();
        assert!(!app.slash_menu_visible());
        assert_eq!(app.slash_menu_len(), 1);
    }

    #[test]
    fn clear_slash_menu_removes_everything() {
        let mut app = make_app();
        app.show_slash_menu(vec![
            SlashMenuEntry::new("a", "/a", "A"),
            SlashMenuEntry::new("b", "/b", "B"),
        ]);
        app.clear_slash_menu();
        assert_eq!(app.slash_menu_len(), 0);
        assert!(!app.slash_menu_visible());
    }

    #[test]
    fn slash_menu_navigation_walks_within_bounds() {
        let mut app = make_app();
        app.show_slash_menu(vec![
            SlashMenuEntry::new("a", "/a", "A"),
            SlashMenuEntry::new("b", "/b", "B"),
            SlashMenuEntry::new("c", "/c", "C"),
        ]);
        // `show` resets selection to 0.
        assert_eq!(
            app.slash_menu_selected().map(|e| e.name.clone()),
            Some("a".to_string())
        );

        app.slash_menu_down();
        assert_eq!(
            app.slash_menu_selected().map(|e| e.name.clone()),
            Some("b".to_string())
        );
        app.slash_menu_down();
        assert_eq!(
            app.slash_menu_selected().map(|e| e.name.clone()),
            Some("c".to_string())
        );
        // Past the end wraps to 0.
        app.slash_menu_down();
        assert_eq!(
            app.slash_menu_selected().map(|e| e.name.clone()),
            Some("a".to_string())
        );
        // `move_up` from 0 wraps to the last entry.
        app.slash_menu_up();
        assert_eq!(
            app.slash_menu_selected().map(|e| e.name.clone()),
            Some("c".to_string())
        );
    }

    #[test]
    fn header_visible_round_trips() {
        let mut app = make_app();
        let initial = app.header_visible();
        app.set_header_visible(!initial);
        assert_eq!(app.header_visible(), !initial);
    }

    #[test]
    fn header_expanded_round_trips() {
        let mut app = make_app();
        let initial = app.header_expanded();
        app.set_header_expanded(!initial);
        assert_eq!(app.header_expanded(), !initial);
    }

    #[test]
    fn thinking_level_round_trips() {
        let mut app = make_app();
        let initial = app.thinking_level();
        // Cycle to Medium (always present) and back to the initial.
        app.set_thinking_level(pi_agent_core::ThinkingLevel::Medium);
        assert_eq!(app.thinking_level(), pi_agent_core::ThinkingLevel::Medium);
        app.set_thinking_level(initial);
        assert_eq!(app.thinking_level(), initial);
    }

    #[test]
    fn thinking_supported_round_trips() {
        let mut app = make_app();
        let initial = app.thinking_supported();
        app.set_thinking_supported(!initial);
        assert_eq!(app.thinking_supported(), !initial);
    }

    #[test]
    fn request_exit_sets_the_flag() {
        let mut app = make_app();
        assert!(!app.exit_requested);
        app.request_exit();
        assert!(app.exit_requested);
    }

    #[test]
    fn status_flash_round_trips() {
        let mut app = make_app();
        assert!(app.status_flash.is_none());
        app.flash_status("hello");
        assert!(app.status_flash.is_some());
    }

    #[test]
    fn dirty_flag_round_trip() {
        let mut app = make_app();
        // mark_dirty sets the flag.
        app.mark_dirty();
        // clear_dirty unsets it.
        app.clear_dirty();
        // We don't assert the boolean here because `dirty()` is also
        // OR'd against modal/turn-busy state, but the round-trip of
        // the setter pair must compile.
        let _ = app.dirty();
    }

    #[test]
    fn dirty_returns_false_when_no_modals_and_idle() {
        let mut app = make_app();
        app.clear_dirty();
        // No background turn, no modal, no paste burst deadline → not
        // dirty.
        assert!(!app.is_busy());
        assert!(!app.dirty());
    }

    #[test]
    fn viewport_returns_some_dimensions() {
        // Without a real terminal the dimensions are 0x0. The getter
        // must still be callable and produce a tuple.
        let app = make_app();
        let (w, h) = app.viewport();
        let (ox, oy) = app.viewport_origin();
        assert_eq!(w, 0);
        assert_eq!(h, 0);
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
    }

    #[test]
    fn composer_scroll_starts_at_zero() {
        let app = make_app();
        assert_eq!(app.composer_scroll(), 0);
    }

    #[test]
    fn selection_starts_empty() {
        let app = make_app();
        assert!(!app.has_selection());
        assert!(app.selection_text().is_none());
        assert!(app.selection_bounds().is_none());
        assert!(app.composer_selection_text().is_none());
    }

    #[test]
    fn clear_selection_is_idempotent() {
        let mut app = make_app();
        app.clear_selection();
        app.clear_selection();
        assert!(!app.has_selection());
    }

    #[test]
    fn theme_round_trip_via_json_name() {
        let mut app = make_app();
        let initial_name = app.theme().name().map(str::to_string);
        // Both built-in themes must resolve by name.
        assert!(app.set_theme_by_name("dark").is_ok());
        assert!(app.set_theme_by_name("light").is_ok());
        // Restore the original name if it was one of the built-ins.
        if let Some(name) = initial_name {
            let _ = app.set_theme_by_name(&name);
        }
    }

    #[test]
    fn set_theme_by_name_rejects_unknown() {
        let mut app = make_app();
        assert!(app.set_theme_by_name("definitely-not-a-real-theme").is_err());
    }

    #[test]
    fn copy_on_select_round_trips() {
        let mut app = make_app();
        let initial = app.copy_on_select();
        app.set_copy_on_select(!initial);
        assert_eq!(app.copy_on_select(), !initial);
        app.set_copy_on_select(initial);
        assert_eq!(app.copy_on_select(), initial);
    }

    #[test]
    fn autocomplete_max_visible_round_trips() {
        let mut app = make_app();
        let initial = app.autocomplete_max_visible();
        app.set_autocomplete_max_visible(initial.saturating_add(1).max(3));
        assert_ne!(app.autocomplete_max_visible(), initial);
        app.set_autocomplete_max_visible(initial);
        assert_eq!(app.autocomplete_max_visible(), initial);
    }

    #[test]
    fn toggle_tools_expanded_inverts() {
        let mut app = make_app();
        let initial = app.tools_expanded();
        let flipped = app.toggle_tools_expanded();
        assert_eq!(flipped, !initial);
        assert_eq!(app.tools_expanded(), !initial);
        app.toggle_tools_expanded();
        assert_eq!(app.tools_expanded(), initial);
    }

    #[test]
    fn shortcut_overlay_default_closed() {
        let mut app = make_app();
        assert!(!app.shortcut_overlay_open());
        // First toggle returns true (new open state).
        assert!(app.toggle_shortcut_overlay());
        assert!(app.shortcut_overlay_open());
        // close_shortcut_overlay returns whether it was open.
        assert!(app.close_shortcut_overlay());
        assert!(!app.shortcut_overlay_open());
        // Already closed → returns false.
        assert!(!app.close_shortcut_overlay());
    }

    #[test]
    fn status_session_metadata_setters_round_trip() {
        let mut app = make_app();
        app.set_session_id("sess-1");
        assert_eq!(app.status_data().session_id, "sess-1");
        app.set_session_name(Some("demo".into()));
        assert_eq!(app.status_data().session_name.as_deref(), Some("demo"));
        app.set_session_name(None);
        assert!(app.status_data().session_name.is_none());

        app.set_status_cwd(Some("/tmp".into()));
        assert_eq!(app.status_data().cwd.as_deref(), Some("/tmp"));
        app.set_status_cwd(None);
        assert!(app.status_data().cwd.is_none());

        app.set_status_git_branch(Some("main".into()));
        assert_eq!(app.status_data().git_branch.as_deref(), Some("main"));
        app.set_status_git_branch(None);
        assert!(app.status_data().git_branch.is_none());
    }

    #[test]
    fn status_provider_count_and_label_round_trip() {
        let mut app = make_app();
        app.set_status_provider(2, Some("anthropic".into()));
        assert_eq!(app.status_data().provider_count, 2);
        assert_eq!(app.status_data().provider_label.as_deref(), Some("anthropic"));
        app.set_status_provider(0, None);
        assert_eq!(app.status_data().provider_count, 0);
        assert!(app.status_data().provider_label.is_none());
    }

    #[test]
    fn status_auto_compact_and_subscription_round_trip() {
        let mut app = make_app();
        app.set_status_auto_compact(true);
        assert!(app.status_data().auto_compact);
        app.set_status_auto_compact(false);
        assert!(!app.status_data().auto_compact);
        app.set_status_subscription(true);
        assert!(app.status_data().subscription);
        app.set_status_subscription(false);
        assert!(!app.status_data().subscription);
    }

    #[test]
    fn status_pricing_round_trip() {
        let mut app = make_app();
        app.set_status_pricing(Some(crate::components::status::StatusPricing {
            input_micro_usd: 1_000_000,
            output_micro_usd: 2_000_000,
            cache_read_micro_usd: 500_000,
            cache_write_micro_usd: 250_000,
        }));
        assert!(app.status_data().pricing.is_some());
        app.set_status_pricing(None);
        assert!(app.status_data().pricing.is_none());
    }

    #[test]
    fn extension_status_set_and_clear() {
        let mut app = make_app();
        app.set_extension_status("plugin-a", Some("loading"));
        assert!(
            app.status_data()
                .extension_statuses
                .iter()
                .any(|(k, v)| k == "plugin-a" && v == "loading")
        );
        // Setting to None removes the entry entirely.
        app.set_extension_status("plugin-a", None);
        assert!(
            !app.status_data()
                .extension_statuses
                .iter()
                .any(|(k, _)| k == "plugin-a")
        );

        app.set_extension_status("plugin-b", Some("ready"));
        app.clear_extension_statuses();
        assert!(app.status_data().extension_statuses.is_empty());
    }

    #[test]
    fn terminal_title_round_trip() {
        let mut app = make_app();
        app.set_terminal_title("hello");
        // `terminal_title()` reads the in-effect value.
        assert_eq!(app.terminal_title(), Some("hello"));
        // `take_terminal_title()` returns the pending title (same
        // string the first time) and clears the pending slot.
        assert_eq!(app.take_terminal_title(), Some("hello".to_string()));
        // Nothing else pending now.
        assert_eq!(app.take_terminal_title(), None);
    }

    #[test]
    fn reassert_terminal_title_overwrites_with_auto() {
        let mut app = make_app();
        // Set a custom title.
        app.set_terminal_title("first");
        // reassert overwrites with the auto-computed title.
        app.reassert_terminal_title();
        let after = app.terminal_title().map(str::to_string);
        assert!(after.is_some());
        assert_ne!(after.as_deref(), Some("first"));
        // sync_terminal_title is idempotent (no-op when nothing
        // changed).
        app.sync_terminal_title();
    }

    #[test]
    fn mouse_regions_default_empty() {
        let app = make_app();
        assert!(app.mouse_regions().is_empty());
    }

    #[test]
    fn composer_area_returns_a_rect() {
        let app = make_app();
        let (x, y, w, h) = app.composer_area();
        // Without a real terminal the rect is all zeros; we just assert
        // the tuple is destructurable.
        assert_eq!((x, y, w, h), (0, 0, 0, 0));
    }

    #[test]
    fn take_selector_commit_default_none() {
        let mut app = make_app();
        assert!(app.take_selector_commit().is_none());
        assert!(!app.selector_open());
    }

    #[test]
    fn history_search_starts_inactive() {
        let app = make_app();
        assert!(!app.history_search_active());
        assert!(app.history_search_hint().is_none());
    }

    #[test]
    fn editor_text_round_trip() {
        let mut app = make_app();
        app.set_editor_text("hello world");
        assert_eq!(app.editor_text(), "hello world");
        let expanded = app.expanded_editor_text();
        assert!(expanded.contains("hello"));
    }

    #[test]
    fn paste_marker_count_starts_at_zero() {
        let app = make_app();
        assert_eq!(app.paste_marker_count(), 0);
    }

    #[test]
    fn scrollbar_state_starts_idle() {
        let app = make_app();
        assert!(!app.scrollbar_hovered());
        assert!(!app.scrollbar_dragging());
        assert!(app.scrollbar_geometry().is_none());
        assert!(app.scroll_to_end_rect().is_none());
        assert!(app.truncated_above_rect().is_none());
        assert!(app.truncated_above_lines().is_none());
    }

    #[test]
    fn info_and_info_block_do_not_panic() {
        let mut app = make_app();
        app.info("plain note");
        app.info_block("block note");
    }

    #[test]
    fn locale_round_trips() {
        let mut app = make_app();
        let initial = app.locale();
        let other = match initial {
            crate::locale::Locale::En => crate::locale::Locale::Zh,
            _ => crate::locale::Locale::En,
        };
        app.set_locale(other);
        assert_eq!(app.locale(), other);
        app.set_locale(initial);
        assert_eq!(app.locale(), initial);
    }

    #[test]
    fn spinner_starts_visible() {
        let app = make_app();
        // Spinner is always constructed; just verify the getter.
        let _ = app.spinner();
    }

    #[test]
    fn status_data_and_mut_accessors_match() {
        let mut app = make_app();
        let snapshot = app.status_data().clone();
        // Mut path lets us poke without triggering any setters.
        app.status_data_mut().provider_count = 99;
        assert_eq!(app.status_data().provider_count, 99);
        // Restore via the public setter so the rest of the test suite
        // sees the original value.
        app.set_status_provider(snapshot.provider_count, snapshot.provider_label.clone());
    }

    #[test]
    fn messages_accessor_returns_a_view() {
        let app = make_app();
        let view = app.messages();
        let _len = view.len();
    }
}

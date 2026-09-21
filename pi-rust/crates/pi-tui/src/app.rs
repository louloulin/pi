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
//! * **Columns are characters, not display cells.** The whole port tracks
//!   selection columns as character offsets (see [`App::selection_text`]),
//!   so word / line ranges are measured with `chars().count()` rather than
//!   upstream's `visibleWidth`. Wide glyphs keep the existing behaviour of
//!   the character-granularity path.
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
//! process-wide registry ([`crate::keybindings::get_keybindings`]), so an
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
//! ([`crate::keybindings::CONSUMED_APP_ACTIONS`]). Without the second filter a
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
//!
//! `setStatus` / `setWorkingMessage` / `setTitle` and the remaining
//! `ctx.ui` methods are *not* region-shaped and stay with the coding-agent
//! layer; they are out of scope here, as is the JS factory → Rust component
//! bridge (`pi-extensions` still answers `ERR_PI_UI_UNSUPPORTED` for
//! `ctx.ui.custom` and the setters).
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
//! All region rects are computed once per frame by
//! `crate::extension_ui::plan_chrome`; the message viewport's geometry (and
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
//! [`crate::styled::SpanStyle`] slots through the live [`Theme`] on every
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as CtEvent, KeyModifiers as CtModifiers, MouseEventKind as CtMouseEventKind,
};
use parking_lot::Mutex;
use pi_agent_core::{Agent, AgentEvent, AssistantMessageUpdate, ThinkingLevel};
use pi_protocol::{Content, Message, StopReason, Usage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

use crate::component::{Component, CustomHandle, CustomOptions, OverlayAnchor, WidgetPlacement};
use crate::dialog::{Dialog, DialogAction, DialogKind};
use crate::editor::EditorAction;
use crate::extension_ui::{plan_chrome, ChromeLayout, ExtensionFrame, ExtensionUi};
use crate::input::{
    InputEvent, Key, KeyCode, KeyModifiers, MouseButton, MouseGesture, MouseGestureKind,
};
use crate::keybindings::{get_keybindings, matches_with_fallback, KeybindingsManager};
use crate::loader::{format_elapsed, Spinner, SPINNER_INTERVAL_MS};
use crate::locale::{
    format_chord, HeaderKey, Locale, EXTENSIONS_DISABLED_EN, EXTENSIONS_DISABLED_ZH,
    HEADER_ONBOARDING_EN, HEADER_ONBOARDING_ZH, HEADER_TITLE, STARTUP_HINTS,
};
use crate::message::{MessageItem, MessageView, PendingMessageKind, Role, ToolBlockRenderer};
use crate::mouse_region::{MouseRegion, MouseRegionPoint};
use crate::prompt::{Prompt, PromptAction};
use crate::search::{
    apply_query_key, render_search_bar, search_bar_rect, SearchBar, SearchIndex, SearchMatch,
    SearchSelectionMode,
};
use crate::selector::{Selector, SelectorAction, SelectorItem};
use crate::settings::{SettingsAction, SettingsList};
use crate::status::{StatusBar, StatusData};
use crate::styled::{plain_text, write_styled_line, SpanStyle, StyledLine, StyledSpan};
use crate::theme::{
    builtin_theme, load_theme, thinking_border_color, ColorMode, Theme, ThemeBg, ThemeColor,
    ThemeError,
};

/// Lines scrolled per wheel notch. Mirrors the upstream `wheelScrollLines`
/// option's default (`packages/tui/src/tui-alt-screen.ts:166,264`).
const WHEEL_SCROLL_LINES: usize = 1;

/// Alt+wheel multiplies the per-notch step by this factor, matching
/// upstream's `ALT_WHEEL_SCROLL_MULTIPLIER`
/// (`packages/tui/src/tui-alt-screen.ts:75,968-971`).
const ALT_WHEEL_SCROLL_MULTIPLIER: usize = 5;

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
/// the composer and the status bar. Mirrors the reservation in
/// [`crate::extension_ui::plan_chrome`].
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
    /// [`crate::hyperlink::supports_hyperlinks`]; `Some(false)` forces the
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
    /// Defaults to [`crate::message::TOOL_PREVIEW_LINES`] (4). This is the
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
            tool_preview_lines: crate::message::TOOL_PREVIEW_LINES,
            startup_header: false,
            startup_header_expanded: true,
            locale: Locale::default(),
            extension_header: ExtensionHeader::Hidden,
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
struct SelectionPoint {
    /// Index of a line in [`MessageView::render_styled_lines`].
    line: usize,
    /// Character column within that line.
    col: usize,
    /// True when `col` is an exclusive end (a word / line range edge).
    boundary: bool,
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
struct Selection {
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
    fn bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
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
/// Columns are character offsets, not terminal cells — see the module docs.
fn word_segments(line: &str) -> Vec<WordSegment> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for segment in line.split_word_bounds() {
        let end = start + segment.chars().count();
        let joiner = TERMINAL_WORD_SELECTION_JOINERS.contains(&segment);
        let selectable = crate::word_navigation::is_word_like(segment) || joiner;
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
struct SearchState {
    /// Cached corpus + matches for the rendered transcript.
    index: SearchIndex,
    /// The query bar and its result counter.
    bar: SearchBar,
    /// Matches from the last refresh.
    matches: Vec<SearchMatch>,
    /// Index into [`SearchState::matches`] of the selected match.
    selected_index: Option<usize>,
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
enum SearchKeyOutcome {
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
    /// The draft's visible text (chip labels expanded).
    pub text: String,
    /// Pasted images attached to the draft, in buffer order.
    pub images: Vec<pi_protocol::ImageContent>,
}

impl Submission {
    /// A text-only submission.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
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

/// Geometry of the chat-log scrollbar, in absolute terminal cells.
///
/// The port of upstream's `ScrollbarGeometry`
/// (`packages/tui/src/layout.ts:44-51`), computed by
/// [`App::scrollbar_geometry`]. Upstream resolves it per `ScrollView` from
/// the layout box under the pointer; this port has a single scrollable
/// region (the message log), so one geometry covers it — see the module
/// docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    /// Absolute column the bar is painted in — the viewport's right edge.
    pub column: u16,
    /// Absolute row of the track's first cell.
    pub track_top: u16,
    /// Rows the track spans; the viewport height.
    pub track_height: u16,
    /// Absolute row of the thumb's first cell.
    pub thumb_top: u16,
    /// Rows the thumb spans — at least two, at most the whole track.
    pub thumb_height: u16,
    /// Largest valid top-relative scroll offset (`content - viewport`).
    pub max_scroll: usize,
}

/// In-flight scrollbar drag — upstream's `ScrollbarDrag`
/// (`packages/tui/src/tui-alt-screen.ts:129-138`). The App has a single
/// scroll view, so the convergence drops the view handle and keeps only the
/// grab offset; the geometry is re-read from the current viewport on every
/// drag event, exactly like upstream's `getScrollViewBox` lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollbarDrag {
    /// Rows between the pointer and the thumb's top when the press landed.
    /// A press on the track uses half the thumb height, so the thumb centres
    /// on the pointer.
    grab_offset: u16,
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
    /// When the last idle `app.clear` (`Ctrl+C`) press landed. A second
    /// press within [`CLEAR_EXIT_WINDOW`] exits; see [`App::step_key_at`].
    /// `None` before the first press of the session.
    last_clear_at: Option<Instant>,
    /// Geometry of the jump-to-latest indicator painted on the viewport's
    /// bottom edge, in absolute terminal cells. `width == 0` means the
    /// last frame did not paint one (the viewport was following the tail),
    /// which is also what the mouse hit test reads.
    /// Width of the message viewport as of the last render. Scroll keys use
    /// it to wrap the log exactly like the renderer does, so a "page" is a
    /// real screenful.
    viewport_width: AtomicU16,
    /// Columns of the frame's right edge the scrollbar took out of
    /// [`App::viewport_width`] on the last render.
    ///
    /// The transcript is laid out one column narrower while the bar is
    /// visible, so the bar never lands on the last character of a wrapped
    /// line; pointer and scroll math that needs the bar itself adds this back.
    viewport_reserved: AtomicU16,
    /// Height of the message viewport as of the last render; the page size
    /// for `PageUp` / `PageDown`.
    viewport_height: AtomicU16,
    /// Top-left cell of the message viewport as of the last render. Pointer
    /// coordinates are absolute, so selection has to map them back into the
    /// viewport the reader was actually looking at.
    viewport_origin: (AtomicU16, AtomicU16),
    /// Rectangle of the "jump to latest" pill as of the last render:
    /// `(row, column, width)`. `width == 0` means it was not painted (the
    /// viewport is following the tail), which is also how the pointer
    /// hit-test knows there is nothing to hit. Upstream keeps the same
    /// record in `scrollToEndIndicatorRect`
    /// (`packages/tui/src/tui-alt-screen.ts:222,1618-1634`).
    scroll_to_end: (AtomicU16, AtomicU16, AtomicU16),
    /// Rectangle of the "cut above" hint as of the last render:
    /// `(row, column, width)`. `width == 0` means it was not painted —
    /// either the top edge is a block boundary or the reader has scrolled
    /// away from the tail. Same shape as [`App::scroll_to_end`] for the
    /// same reason: the pointer reads it between renders.
    truncated_above: (AtomicU16, AtomicU16, AtomicU16),
    /// Active chat-log text selection, if any.
    selection: Option<Selection>,
    /// Region-local cell of a left press that landed inside a modal overlay,
    /// kept until its release so only a click that starts and ends on the
    /// same cell commits (upstream's `isClick`,
    /// `packages/tui/src/tui-alt-screen.ts:1312-1315`). `None` whenever no
    /// modal is on screen.
    modal_mouse_press: Option<MouseRegionPoint>,
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
    /// Driver-supplied rich renderer for tool blocks.
    ///
    /// `pi-tui` cannot depend on the crate that owns the tool renderers, so
    /// the driver installs one with [`App::set_tool_block_renderer`] and the
    /// App hands it each finished [`pi_protocol::ToolResult`] for styling.
    /// See [`ToolBlockRenderer`](crate::message::ToolBlockRenderer).
    tool_block_renderer: Option<Box<dyn ToolBlockRenderer>>,
    /// Cell a left press landed on when it hit a tool block, kept until its
    /// release: a click that starts and ends on the same tool block toggles
    /// that block's expansion instead of starting a text selection.
    tool_press: Option<(u16, u16, usize)>,
    /// Cursor into [`crate::loader::SPINNER_FRAMES`] for the footer's busy
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
    /// Whether the built-in startup header is currently expanded. Distinct
    /// from [`AppConfig::startup_header`] (visible at all) and from an
    /// extension's `ctx.ui.setHeader`, which replaces the built-in lines.
    header_expanded: bool,
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
        let mut prompt = Prompt::new("> ");
        prompt.set_placeholder(config.prompt_placeholder.clone());
        let event_rx = agent.subscribe();
        let markdown = config.markdown;
        let tool_preview_lines = config.tool_preview_lines;
        let header_expanded = config.startup_header_expanded;
        let hyperlinks = config
            .hyperlinks
            .unwrap_or_else(crate::hyperlink::supports_hyperlinks);
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
            last_clear_at: None,
            viewport_width: AtomicU16::new(0),
            viewport_reserved: AtomicU16::new(0),
            viewport_height: AtomicU16::new(0),
            viewport_origin: (AtomicU16::new(0), AtomicU16::new(0)),
            scroll_to_end: (AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0)),
            truncated_above: (AtomicU16::new(0), AtomicU16::new(0), AtomicU16::new(0)),
            selection: None,
            search: None,
            modal_mouse_press: None,
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
            tool_block_renderer: None,
            tool_press: None,
            spinner: Spinner::new(),
            turn_started: None,
            spinner_advanced_at: Instant::now(),
            header_expanded,
            thinking_level: ThinkingLevel::Medium,
            thinking_supported: false,
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
        frame
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
        if !self.config.startup_header || !self.header_expanded {
            return Vec::new();
        }
        let mut lines = self.header_title_lines();
        let hints = self.header_hint_lines();
        if hints.is_empty() {
            return lines;
        }
        let expanded_rows = lines.len() as u16 + u16::try_from(hints.len()).unwrap_or(u16::MAX);
        if expanded_rows.saturating_add(HEADER_RESERVED_ROWS) <= total_height {
            lines.extend(hints);
            return lines;
        }
        // Short terminal: the title survives, the hints fold away, and the
        // row that says so is dropped too if even that does not fit.
        let kb = get_keybindings();
        let folded = HeaderKey::Chord("app.header")
            .label(|id| kb.get_keys(id))
            .map(|keys| crate::locale::header_folded_line(self.config.locale, &keys));
        let folded_rows = lines.len() as u16 + u16::from(folded.is_some());
        if folded_rows.saturating_add(HEADER_RESERVED_ROWS) <= total_height {
            if let Some(text) = folded {
                lines.push(vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Dim))]);
            }
        }
        lines
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
                format!("v{}", crate::VERSION),
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
        let kb = get_keybindings();
        let locale = self.config.locale;
        let mut lines: Vec<StyledLine> = Vec::with_capacity(STARTUP_HINTS.len() + 2);
        for hint in STARTUP_HINTS {
            // Two filters: the id must resolve to a chord in the live table
            // *and* name an action this port consumes. The second is what
            // keeps a bound-but-unimplemented `app.*` id out of the header
            // (`CONSUMED_APP_ACTIONS`, LUM-1240/LUM-1245).
            if !hint.is_wired() {
                continue;
            }
            let Some(keys) = hint.key.label(|id| kb.get_keys(id)) else {
                // Unbound in this table: an unbound action is not a hint.
                continue;
            };
            lines.push(vec![
                StyledSpan::new(format!("  {keys} "), SpanStyle::fg(ThemeColor::Accent)),
                StyledSpan::new(
                    hint.description(locale).to_string(),
                    SpanStyle::fg(ThemeColor::Muted),
                ),
            ]);
        }
        if lines.is_empty() {
            return lines;
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

    /// The status data as it should be painted: [`App::status_data`] with the
    /// transient hint layered on top when a flash is pending. Borrowed in the
    /// common case so the render path does not clone on every frame.
    fn status_for_render(&self) -> Cow<'_, StatusData> {
        let flash = self.status_flash.as_deref();
        if !self.thinking_supported && flash.is_none() {
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
        if let Some(flash) = flash {
            data.hint = Some(flash.to_string());
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

    /// Whether a background agent turn is currently in flight.
    pub fn is_busy(&self) -> bool {
        self.turn_busy.load(Ordering::SeqCst)
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

    /// Drain any pending agent events into the message view. The
    /// caller calls this on every render tick — the App drains
    /// synchronously, so the TUI never blocks on the agent.
    pub fn drain_agent_events(&mut self) -> bool {
        let mut changed = false;
        loop {
            let event = match self.event_rx.as_mut() {
                None => break,
                Some(rx) => match rx.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.cancel_token = None;
                        self.event_rx = None;
                        changed = true;
                        break;
                    }
                },
            };
            changed = true;
            self.apply_event(event);
        }
        changed
    }

    /// Apply one [`AgentEvent`] to the message view and status bar.
    ///
    /// [`App::drain_agent_events`] funnels every queued event through this;
    /// it is public so a driver (or a test) can inject a synthetic event —
    /// the faux provider only streams text, so a thinking or tool event has
    /// no other way in (the render loop itself goes through
    /// [`App::drain_agent_events`]).
    pub fn apply_agent_event(&mut self, event: AgentEvent) {
        self.apply_event(event);
    }

    /// Apply a single [`AgentEvent`] to the message view + status bar.
    fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {}
            AgentEvent::MessageStart { model } => {
                self.tool_call_ids.clear();
                self.messages.begin_assistant_stream(&model);
            }
            AgentEvent::MessageUpdate(update) => match update {
                AssistantMessageUpdate::TextDelta { delta } => {
                    self.messages.append_assistant_delta(&delta);
                }
                AssistantMessageUpdate::ThinkingDelta { delta } => {
                    self.messages.append_thinking_delta(&delta);
                }
                AssistantMessageUpdate::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => {
                    // The first delta for a call carries the provider id;
                    // the rest only carry argument fragments. Remember the
                    // id by index so every fragment lands in one block.
                    let call_id = match id {
                        Some(id) => {
                            self.tool_call_ids.insert(index, id.clone());
                            id
                        }
                        None => self
                            .tool_call_ids
                            .get(&index)
                            .cloned()
                            .unwrap_or_else(|| format!("tool-{index}")),
                    };
                    self.messages.begin_tool_stream(&call_id, name.as_deref());
                    if let Some(delta) = arguments_delta {
                        self.messages.append_tool_stream_args(&call_id, &delta);
                    }
                }
            },
            AgentEvent::MessageEnd { message } => {
                self.messages.end_assistant_stream();
                // Tool execution events that follow carry their own call id,
                // so the index map has done its job and can be dropped.
                self.tool_call_ids.clear();
                // Cumulative session totals plus the latest turn's context
                // size, which drives the footer's context gauge.
                let usage = message.usage;
                self.status_data.add_usage(&usage);
                let context_used = if usage.total > 0 {
                    usage.total
                } else {
                    usage.input + usage.output + usage.cache_read + usage.cache_write
                };
                self.status_data.set_context_used(context_used);
            }
            AgentEvent::ToolExecutionStart { call } => {
                if let Some(renderer) = self.tool_block_renderer.as_mut() {
                    renderer.begin_tool(&call);
                }
                let args = if call.arguments.is_null() {
                    String::new()
                } else {
                    call.arguments.to_string()
                };
                self.messages
                    .start_tool_execution(&call.id, &call.name, &args);
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: _,
                delta,
            } => {
                self.messages.append_assistant_delta(&delta);
            }
            AgentEvent::ToolExecutionEnd {
                result,
                duration_ms,
            } => {
                let is_error = result.is_error;
                let body = match result.content.as_ref() {
                    Content::Text(t) => t.text.clone(),
                    _ => "(binary result)".to_string(),
                };
                // Let the driver's renderer style the block against the
                // viewport we last painted at. Before the first render there
                // is no width, so fall back to a sane 80 columns.
                let width = {
                    let w = self.viewport_width.load(Ordering::Relaxed);
                    if w == 0 {
                        80
                    } else {
                        w
                    }
                };
                let styled = self
                    .tool_block_renderer
                    .as_mut()
                    .and_then(|renderer| renderer.finish_tool(&result, width));
                self.messages.finish_tool_execution_with_lines(
                    &result.tool_call_id,
                    duration_ms,
                    &body,
                    is_error,
                    styled,
                );
            }
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => {
                if message.stop_reason == pi_protocol::StopReason::Error {
                    self.pending_error = Some("provider returned an error".into());
                }
                self.last_turn_usage = Some(TurnUsage {
                    usage: message.usage,
                    stop_reason: message.stop_reason,
                    trailing: tool_results,
                });
            }
            AgentEvent::UserMessage(_) => {}
            // Run brackets. The TUI renders turn/message state, not the run
            // boundary itself, so these need no UI work — but the extension
            // fan-out hook (if installed) still sees them.
            AgentEvent::AgentStart | AgentEvent::AgentEnd { .. } => {}
            AgentEvent::Error(message) => {
                self.pending_error = Some(message);
            }
        }
    }

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
            self.prompt.push_history(&text);
            return;
        }
        if self.event_rx.is_none() {
            // App constructed without a subscription — re-establish one.
            if let Ok(guard) = agent.try_lock() {
                self.event_rx = Some(guard.subscribe());
            }
        }
        self.messages.push(MessageItem::user(&text));
        self.prompt.push_history(&text);
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
            text: self.prompt.text(),
            images: self.prompt.images().to_vec(),
        };
        self.prompt.clear();
        if busy {
            self.messages
                .push_pending(PendingMessageKind::FollowUp, submission.text.clone());
            self.prompt.push_history(&submission.text);
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

    /// Route one key while the settings modal is open.
    fn step_settings(&mut self, key: Key) -> StepOutcome {
        let Some(list) = self.settings.as_mut() else {
            return StepOutcome::Idle;
        };
        match list.handle_key(key) {
            SettingsAction::None => StepOutcome::Idle,
            SettingsAction::Changed => StepOutcome::Redraw,
            SettingsAction::ValueChanged { id, value } => {
                self.pending_setting_change = Some((id, value));
                StepOutcome::Redraw
            }
            SettingsAction::Activated(id) => {
                self.pending_setting_activation = Some(id);
                StepOutcome::Redraw
            }
            SettingsAction::Cancelled => {
                self.settings = None;
                StepOutcome::Redraw
            }
        }
    }

    /// Route a wheel event to the open settings list. Returns `None` when
    /// no settings modal is open, so the caller can fall through to the
    /// transcript.
    fn step_settings_wheel(&mut self, up: bool, alt: bool) -> Option<StepOutcome> {
        let list = self.settings.as_mut()?;
        let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
        let delta = if up { -(lines as i32) } else { lines as i32 };
        Some(match list.scroll_by(delta) {
            SettingsAction::Changed => StepOutcome::Redraw,
            _ => StepOutcome::Idle,
        })
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
            if let InputEvent::Mouse { up, alt } = event {
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
            if let InputEvent::Mouse { up, alt } = event {
                let lines = WHEEL_SCROLL_LINES * if alt { ALT_WHEEL_SCROLL_MULTIPLIER } else { 1 };
                let changed = if up {
                    self.scroll_viewport_up(lines)
                } else {
                    self.scroll_viewport_down(lines)
                };
                return if changed {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                };
            }
            return StepOutcome::Idle;
        };
        self.step_key(key)
    }

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

    /// Process a single [`Key`]. Public so tests can step the App
    /// with explicit keys. Equivalent to [`App::step_key_at`] with the
    /// current wall clock; the production render loop goes through here.
    pub fn step_key(&mut self, key: Key) -> StepOutcome {
        self.step_key_at(key, Instant::now())
    }

    /// Process a single [`Key`] at the caller-supplied instant.
    ///
    /// The instant is what the `app.clear` (`Ctrl+C`) double-press window is
    /// measured against, so tests can drive the window without sleeping;
    /// nothing else reads it. `Instant::now()` must not appear in the
    /// decision itself (LUM-1238 acceptance 2).
    pub fn step_key_at(&mut self, key: Key, now: Instant) -> StepOutcome {
        // A transient status message lives for exactly one key press
        // (upstream's `showStatus` clears on a timer; this port has no timer
        // in the App, and a key press is the next thing the reader does).
        self.status_flash = None;
        // A visible custom overlay is the outermost layer; see [`App::step`].
        if self.extension.handle_overlay_input(key) {
            return StepOutcome::Redraw;
        }
        // A modal dialog swallows every key — including Ctrl+C / Esc,
        // which cancel the dialog instead of the turn or the App.
        if self.dialog.is_some() {
            return self.step_dialog(key);
        }
        // The settings modal is the next-outermost layer.
        if self.settings.is_some() {
            return self.step_settings(key);
        }
        // Global keys. Resolved through the keybinding registry so an
        // installed override reaches the App; with nothing installed the
        // registry serves the defaults, so the behaviour below is the
        // pre-keybinding behaviour (see the module docs).
        let kb = get_keybindings();
        let event = InputEvent::Key(key);
        // The transcript search overlay owns the keyboard while it is open,
        // except for the chords the viewport keeps for itself
        // (`shouldDeferViewportInputToOverlay`,
        // `packages/tui/src/tui-alt-screen.ts:644-645`).
        if self.search.is_some() {
            match self.step_search_key(key) {
                SearchKeyOutcome::Handled(outcome) => return outcome,
                SearchKeyOutcome::PassThrough => {}
            }
        }
        // `tui.altScreen.search` opens the overlay; while it is open the
        // overlay itself consumes the chord above (upstream checks the chord
        // before it checks whether the overlay holds focus,
        // `packages/tui/src/tui-alt-screen.ts:705-708`).
        if kb.matches(&event, "tui.altScreen.search") {
            return if self.open_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        // `app.interrupt` (`Escape`): cancel the in-flight turn. When the
        // App is idle the chord falls through to the prompt, which is the
        // pre-keybinding behaviour (selectors/settings got the key above).
        if Self::matches_app_key(&kb, &event, "app.interrupt", &["escape"]) && self.is_busy() {
            self.cancel();
            return StepOutcome::Redraw;
        }
        // `app.clear` (`Ctrl+C`): cancel while a turn is in flight; when
        // idle, clear the composer on the first press and exit on a second
        // press inside [`CLEAR_EXIT_WINDOW`] — upstream `handleCtrlC`
        // (`interactive-mode.ts:3931-3939`). Only the composer is touched by
        // the clearing press (`Prompt::clear` drops the draft text, chips,
        // history browsing and undo stack), so nothing else about the
        // session changes.
        if Self::matches_app_key(&kb, &event, "app.clear", &["ctrl+c"]) {
            if self.is_busy() {
                self.cancel();
                return StepOutcome::Redraw;
            }
            let double_press = self
                .last_clear_at
                .is_some_and(|last| now.saturating_duration_since(last) < CLEAR_EXIT_WINDOW);
            if double_press {
                self.exit_requested = true;
                return StepOutcome::Exit;
            }
            self.last_clear_at = Some(now);
            self.prompt.clear();
            return StepOutcome::Redraw;
        }
        // `app.model.select` (`Ctrl+L`) is **not** claimed here. Upstream's
        // `app.model.select` means "open the model selector"
        // (`packages/coding-agent/src/core/keybindings.ts:116`), and this port's
        // selector lives in the coding-agent driver, which claims the chord
        // before the App sees the key. Clearing the transcript is `/clear`'s
        // job; a hardcoded `Ctrl+L` here would shadow the driver's chord (see
        // the module docs).
        // `app.thinking.toggle` (`Ctrl+T`): collapse / expand every assistant
        // reasoning block (upstream's `toggleThinkingBlockVisibility`,
        // `interactive-mode.ts:4239`, which also reports the new state through
        // `showStatus`).
        if Self::matches_app_key(&kb, &event, "app.thinking.toggle", &["ctrl+t"]) {
            let visible = self.toggle_thinking_visibility();
            self.flash_status(format!(
                "Thinking blocks: {}",
                if visible { "visible" } else { "hidden" }
            ));
            return StepOutcome::Redraw;
        }
        // `app.tools.expand` (`Ctrl+O`): expand / collapse every tool block
        // (upstream's `setToolsExpanded` / `toggleToolOutputExpansion`,
        // `interactive-mode.ts:4231-4246`, which also reports the new state
        // through `showStatus`). The rich bodies were rendered by the driver
        // at execution end; this only flips how much of them the App paints.
        if Self::matches_app_key(&kb, &event, "app.tools.expand", &["ctrl+o"]) {
            let expanded = self.toggle_tools_expanded();
            self.flash_status(format!(
                "Tool output: {}",
                if expanded { "expanded" } else { "collapsed" }
            ));
            return StepOutcome::Redraw;
        }
        // `app.clipboard.pasteImage` (`Alt+V`): attach a clipboard image to
        // the draft. The App cannot read the system clipboard, so it records
        // the request and the driver answers it with [`App::paste_image`]
        // (or the text fallback, [`App::paste_text`]) — the same seam as
        // copy-on-select. A key press always redraws so the "reading…" frame
        // and the restored status hint stay honest.
        if Self::matches_app_key(&kb, &event, "app.clipboard.pasteImage", &["alt+v"]) {
            self.pending_image_paste = true;
            return StepOutcome::Redraw;
        }
        // `app.header` (`Alt+H`): fold / unfold the built-in startup header.
        // A Rust-port addition — upstream ties the header's expansion to
        // `app.tools.expand` — so it is resolved with the same
        // registry-first / builtin-fallback rule as every other `app.*` chord
        // and reported through `showStatus` (`flash_status`).
        if Self::matches_app_key(&kb, &event, "app.header", &["alt+h"]) {
            let expanded = self.toggle_header();
            let chord = kb
                .get_keys("app.header")
                .first()
                .map(|chord| format_chord(chord))
                .unwrap_or_else(|| format_chord("alt+h"));
            self.flash_status(format!(
                "Startup header: {}{}",
                if expanded { "expanded" } else { "collapsed" },
                if expanded {
                    String::new()
                } else {
                    format!(" ({chord} to show)")
                }
            ));
            return StepOutcome::Redraw;
        }
        // Fullscreen chat-log scrolling. Upstream deliberately shadows
        // the bare editor bindings for these chords in fullscreen mode
        // (`packages/tui/src/keybindings.ts:159-165,208-209`: "These
        // intentionally shadow the unmodified editor bindings in
        // fullscreen mode"); `Ctrl+A` / `Ctrl+E` still reach the editor
        // for start / end of line.
        if kb.matches(&event, "tui.altScreen.pageUp") {
            let page = self.message_page();
            return if self.scroll_viewport_up(page) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        if kb.matches(&event, "tui.altScreen.pageDown") {
            let page = self.message_page();
            return if self.scroll_viewport_down(page) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        // `tui.altScreen.top` / `tui.altScreen.bottom`.
        if kb.matches(&event, "tui.altScreen.top") {
            return if self.scroll_viewport_to_top() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }
        if kb.matches(&event, "tui.altScreen.bottom") {
            return if self.scroll_viewport_to_bottom() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            };
        }

        // The editor region's component (a non-overlay `custom` session or a
        // custom editor component) gets the key before the prompt. The
        // app-level chords above already had their chance, so an extension
        // editor cannot shadow interrupt / clear / scrolling.
        if self.extension.handle_editor_input(key) {
            return StepOutcome::Redraw;
        }

        match self.prompt.handle_key(key) {
            PromptAction::None => StepOutcome::Idle,
            PromptAction::Changed => StepOutcome::Redraw,
            PromptAction::Submit(text) => {
                // Caller is responsible for invoking `submit` with an
                // `Arc<AsyncMutex<Agent>>` — we just announce the submitted
                // draft (text plus any pasted image chips) and clear the
                // buffer. The images are captured before `clear()` wipes
                // them.
                let images = self.prompt.images().to_vec();
                if self.turn_busy.load(Ordering::SeqCst) && !images.is_empty() {
                    // Refuse *before* clearing: the Stage 61 pending queue is
                    // text-only, so accepting would silently drop the chips.
                    // Nothing is consumed — the draft (text and chips) stays
                    // in the editor for the next attempt.
                    self.flash_status("Cannot attach images while a turn is running");
                    return StepOutcome::Redraw;
                }
                let submitted = Submission { text, images };
                self.prompt.clear();
                StepOutcome::Submitted(submitted)
            }
            PromptAction::Interrupt => {
                self.cancel();
                StepOutcome::Redraw
            }
            PromptAction::Eof => {
                self.exit_requested = true;
                StepOutcome::Exit
            }
        }
    }

    /// Route a key to the open dialog.
    fn step_dialog(&mut self, key: Key) -> StepOutcome {
        let Some(dialog) = self.dialog.as_mut() else {
            return StepOutcome::Idle;
        };
        match dialog.handle_key(key) {
            DialogAction::None => StepOutcome::Idle,
            DialogAction::Changed => StepOutcome::Redraw,
            // The answer already travelled to the host over the
            // dialog's reply channel; the modal just closes.
            DialogAction::Resolved(_) => {
                self.dialog = None;
                StepOutcome::Redraw
            }
        }
    }

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

    /// Route a key to the open search overlay.
    fn step_search_key(&mut self, key: Key) -> SearchKeyOutcome {
        let bindings = get_keybindings();
        let event = InputEvent::Key(key);
        if bindings.matches(&event, "tui.altScreen.searchClose")
            || bindings.matches(&event, "tui.altScreen.search")
        {
            return SearchKeyOutcome::Handled(if self.close_search() {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchNext") {
            return SearchKeyOutcome::Handled(if self.navigate_search(1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }
        if bindings.matches(&event, "tui.altScreen.searchPrevious") {
            return SearchKeyOutcome::Handled(if self.navigate_search(-1) {
                StepOutcome::Redraw
            } else {
                StepOutcome::Idle
            });
        }

        // The viewport scroll chords and the process-global keys keep
        // working while the bar has focus.
        match key {
            Key {
                code: KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End,
                modifiers,
            } if modifiers.is_empty() => return SearchKeyOutcome::PassThrough,
            Key {
                code: KeyCode::Char('c' | 'l'),
                modifiers,
            } if modifiers == KeyModifiers::CONTROL => return SearchKeyOutcome::PassThrough,
            _ => {}
        }

        let Some(state) = self.search.as_mut() else {
            return SearchKeyOutcome::Handled(StepOutcome::Idle);
        };
        if apply_query_key(&mut state.bar, key) {
            self.search_query_changed();
            SearchKeyOutcome::Handled(StepOutcome::Redraw)
        } else {
            SearchKeyOutcome::Handled(StepOutcome::Idle)
        }
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

    /// Paint the search matches into the already-rendered message area.
    ///
    /// Non-current matches get an underline, the current one bold + reverse —
    /// upstream's `searchMatchStyle` / `searchCurrentMatchStyle`
    /// (`packages/tui/src/tui-alt-screen.ts:118-121`). The port keeps the
    /// themed foreground already in the cell instead of replacing it, so a
    /// highlighted token stays readable in any theme.
    fn apply_search_highlight(&self, area: Rect, buf: &mut Buffer) {
        let Some(state) = self.search.as_ref() else {
            return;
        };
        if state.matches.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }
        let (visible_start, lines) = self.messages.visible_lines(area.width, area.height);
        let visible_end = visible_start + lines.len();
        for (index, search_match) in state.matches.iter().enumerate() {
            let modifier = if Some(index) == state.selected_index {
                Modifier::BOLD | Modifier::REVERSED
            } else {
                Modifier::UNDERLINED
            };
            for segment in &search_match.segments {
                if segment.row < visible_start || segment.row >= visible_end {
                    continue;
                }
                let row = segment.row - visible_start;
                let text = plain_text(&lines[row]);
                let len = text.chars().count();
                let to = segment.end_col.min(len);
                let from = segment.start_col.min(to);
                let y = area.y + row as u16;
                for col in from..to {
                    let x = area.x + col as u16;
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.modifier |= modifier;
                    }
                }
            }
        }
    }

    /// Paint the chat-log scrollbar into the message viewport's last column.
    ///
    /// Upstream's `paintScrollbar` / `renderScrollView`
    /// (`packages/tui/src/layout.ts:280-326`): the track is a dim vertical
    /// rule, the thumb a heavier block. While the bar is hovered or dragged
    /// the thumb switches to a solid block and both parts go bold — see the
    /// module docs on why the port adds the modifier. The bar is a no-op when
    /// the transcript fits the viewport ([`App::scrollbar_geometry`] is
    /// `None`).
    fn apply_scrollbar(&self, area: Rect, buf: &mut Buffer) {
        let Some(geometry) = self.scrollbar_geometry() else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let active = self.scrollbar_hover || self.scrollbar_drag.is_some();
        let thumb_end = geometry.thumb_top.saturating_add(geometry.thumb_height);
        for row in 0..geometry.track_height {
            let y = geometry.track_top + row;
            let in_thumb = y >= geometry.thumb_top && y < thumb_end;
            let (glyph, slot) = if in_thumb {
                (if active { '█' } else { '┃' }, ThemeColor::ScrollbarThumb)
            } else {
                ('│', ThemeColor::ScrollbarTrack)
            };
            let style = if active {
                SpanStyle::fg(slot).bold()
            } else {
                SpanStyle::fg(slot)
            };
            if let Some(cell) = buf.cell_mut((geometry.column, y)) {
                cell.set_char(glyph);
                cell.set_style(style.to_style(&self.theme));
            }
        }
    }

    /// Give an open search overlay the mouse first, exactly like the modal
    /// overlays: a gesture inside the bar's rectangle is consumed by the bar
    /// (hover clears, a press on a navigation button navigates) and never
    /// falls through to the chat-log selection underneath
    /// (`getSearchNavigationDirectionAt` / `handleSearchMouseEvent`,
    /// `packages/tui/src/tui-alt-screen.ts:541-568`).
    ///
    /// Returns `None` when the overlay is closed, has no rectangle yet, or the
    /// gesture landed outside it, so the caller keeps routing normally.
    fn step_search_mouse_gesture(&mut self, gesture: &MouseGesture) -> Option<StepOutcome> {
        self.search.as_ref()?;
        let (width, height) = self.viewport();
        let (origin_x, origin_y) = self.viewport_origin();
        if width == 0 || height == 0 {
            return None;
        }
        let rect = search_bar_rect(Rect::new(origin_x, origin_y, width, height))?;
        let inside = gesture.x >= rect.x
            && gesture.x < rect.x + rect.width
            && gesture.y >= rect.y
            && gesture.y < rect.y + rect.height;
        let (row, col) = (
            gesture.y as i64 - rect.y as i64,
            gesture.x as i64 - rect.x as i64,
        );
        let direction = if inside {
            self.search
                .as_ref()
                .and_then(|state| state.bar.navigation_direction_at(rect.width, row, col))
        } else {
            None
        };
        let changed = self
            .search
            .as_mut()
            .map(|state| state.bar.set_hovered(direction))
            .unwrap_or(false);
        if !inside {
            // The pointer left the bar: clear any stale hover and let the
            // gesture reach the chat log.
            return if changed {
                Some(StepOutcome::Redraw)
            } else {
                None
            };
        }
        if let (Some(direction), true) = (
            direction,
            matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)),
        ) {
            if self.navigate_search(direction) {
                return Some(StepOutcome::Redraw);
            }
        }
        Some(if changed {
            StepOutcome::Redraw
        } else {
            StepOutcome::Idle
        })
    }

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
            self.viewport_width.load(Ordering::Relaxed),
            self.viewport_height.load(Ordering::Relaxed),
        )
    }

    /// Top-left cell of the message viewport as of the last render. Pointer
    /// coordinates are absolute, so this is what maps them back in.
    pub fn viewport_origin(&self) -> (u16, u16) {
        (
            self.viewport_origin.0.load(Ordering::Relaxed),
            self.viewport_origin.1.load(Ordering::Relaxed),
        )
    }

    /// Rectangle of the "jump to latest" pill as of the last render, or
    /// `None` when the viewport was following the tail and no pill was
    /// painted.
    pub fn scroll_to_end_rect(&self) -> Option<Rect> {
        let width = self.scroll_to_end.2.load(Ordering::Relaxed);
        (width > 0).then(|| Rect {
            x: self.scroll_to_end.1.load(Ordering::Relaxed),
            y: self.scroll_to_end.0.load(Ordering::Relaxed),
            width,
            height: 1,
        })
    }

    /// The pill's label: ` ↓ Jump to latest message · <shortcut> `, exactly
    /// upstream's string (down to the leading space) with the shortcut
    /// resolved from `tui.altScreen.bottom`
    /// (`packages/coding-agent/src/modes/interactive/tui-renderer.ts:29-33`).
    ///
    /// An unbound action drops the ` · <shortcut>` half rather than
    /// rendering an empty shortcut, which is the same rule `/hotkeys`
    /// follows.
    fn scroll_to_end_label(&self) -> StyledSpan {
        let shortcut = get_keybindings()
            .get_keys("tui.altScreen.bottom")
            .iter()
            .map(|chord| format_chord(chord))
            .collect::<Vec<_>>()
            .join("/");
        let text = if shortcut.is_empty() {
            SCROLL_TO_END_LABEL.to_string()
        } else {
            format!("{SCROLL_TO_END_LABEL}· {shortcut} ")
        };
        StyledSpan {
            text,
            style: SpanStyle::fg_bg(ThemeColor::Text, ThemeBg::SelectedBg),
            link: None,
        }
    }

    /// Composite the pill onto the bottom row of the message viewport when
    /// the reader has scrolled away from the tail, and record where it
    /// landed for the pointer.
    ///
    /// Upstream's `compositeScrollToEndIndicator`
    /// (`packages/tui/src/tui-alt-screen.ts:1617-1637`): only when the scroll
    /// view follows the end but is not at it, drawn on the viewport's last
    /// row, horizontally centred, truncated at — and never wider than — the
    /// space left of the scrollbar column.
    fn paint_scroll_to_end(&self, message_area: Rect, buf: &mut Buffer) {
        // Every path out of here clears the record, so a stale rectangle can
        // never keep swallowing clicks after the pill is gone.
        self.scroll_to_end.2.store(0, Ordering::Relaxed);
        if message_area.width == 0 || message_area.height == 0 {
            return;
        }
        // Nothing to jump to when the transcript already fits, and no pill
        // while the viewport is pinned to the tail.
        if self.messages.is_following() || self.max_scroll() == 0 {
            return;
        }
        let available = match self.scrollbar_geometry() {
            Some(geometry) if geometry.column > message_area.x => {
                (geometry.column - message_area.x).min(message_area.width)
            }
            _ => message_area.width,
        };
        let label = self.scroll_to_end_label();
        let label_width = crate::hyperlink::visible_width(&label.text) as u16;
        if label_width == 0 || available == 0 {
            return;
        }
        let width = label_width.min(available);
        let column = message_area.x + (available - width) / 2;
        let row = message_area.y + message_area.height - 1;
        // Blank the covered cells first: the pill is shorter than the
        // transcript line underneath it, and ratatui only emits the cells
        // this buffer changed, so an unblanked row left the old text bleeding
        // through (`reset` also drops the covered cells' colours).
        for offset in 0..width {
            if let Some(cell) = buf.cell_mut((column + offset, row)) {
                cell.reset();
            }
        }
        let line = [label];
        write_styled_line(buf, column, row, width, &line, &self.theme);
        self.scroll_to_end.0.store(row, Ordering::Relaxed);
        self.scroll_to_end.1.store(column, Ordering::Relaxed);
        self.scroll_to_end.2.store(width, Ordering::Relaxed);
    }

    /// Rectangle of the "cut above" hint as of the last render, or `None`
    /// when the top edge of the viewport is a block boundary (or the reader
    /// has scrolled away from the tail) and nothing was painted.
    pub fn truncated_above_rect(&self) -> Option<Rect> {
        let width = self.truncated_above.2.load(Ordering::Relaxed);
        (width > 0).then(|| Rect {
            x: self.truncated_above.1.load(Ordering::Relaxed),
            y: self.truncated_above.0.load(Ordering::Relaxed),
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

    /// The hint's label: ` ⋯ <n> line(s) above · <shortcut> ` — the prompt
    /// half of the affordance, resolved from `tui.altScreen.top` (the key it
    /// actually points at) with the same unbound-action rule
    /// [`App::scroll_to_end_label`] follows.
    fn truncated_above_label(&self, hidden: usize) -> StyledSpan {
        let shortcut = get_keybindings()
            .get_keys("tui.altScreen.top")
            .iter()
            .map(|chord| format_chord(chord))
            .collect::<Vec<_>>()
            .join("/");
        let noun = if hidden == 1 { "line" } else { "lines" };
        let base = format!("{TRUNCATED_ABOVE_LEAD}{hidden} {noun} above ");
        let text = if shortcut.is_empty() {
            base
        } else {
            format!("{base}· {shortcut} ")
        };
        StyledSpan {
            text,
            style: SpanStyle::fg(ThemeColor::Muted),
            link: None,
        }
    }

    /// Disclose that the block at the top edge of the viewport continues
    /// above it, on the viewport's first row, left-aligned and never wider
    /// than the space left of the scrollbar column.
    ///
    /// The covered row is a continuation row of a block the reader cannot see
    /// the head of, so the trade is one unreadable old line for knowing that
    /// there is something to look for — the alternative (paint nothing, as
    /// upstream does) leaves a cut block looking like a block that starts
    /// there.
    fn paint_truncated_above(&self, message_area: Rect, buf: &mut Buffer) {
        // Every path out clears the record, so a stale rectangle can never
        // keep swallowing clicks after the hint is gone.
        self.truncated_above.2.store(0, Ordering::Relaxed);
        if message_area.width == 0 || message_area.height < 2 {
            return;
        }
        let Some(hidden) = self.truncated_above_lines() else {
            return;
        };
        let available = match self.scrollbar_geometry() {
            Some(geometry) if geometry.column > message_area.x => {
                (geometry.column - message_area.x).min(message_area.width)
            }
            _ => message_area.width,
        };
        let label = self.truncated_above_label(hidden);
        let label_width = crate::hyperlink::visible_width(&label.text) as u16;
        if label_width == 0 || available == 0 {
            return;
        }
        let width = label_width.min(available);
        let column = message_area.x;
        let row = message_area.y;
        // Blank the covered cells first, for the same reason the pill does:
        // ratatui only emits the cells this buffer changed, so an unblanked
        // row would let the covered text bleed through the hint's tail.
        for offset in 0..width {
            if let Some(cell) = buf.cell_mut((column + offset, row)) {
                cell.reset();
            }
        }
        let line = [label];
        write_styled_line(buf, column, row, width, &line, &self.theme);
        self.truncated_above.0.store(row, Ordering::Relaxed);
        self.truncated_above.1.store(column, Ordering::Relaxed);
        self.truncated_above.2.store(width, Ordering::Relaxed);
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
            column: origin_x + width + self.viewport_reserved.load(Ordering::Relaxed) - 1,
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
    fn step_scrollbar_mouse_gesture(&mut self, gesture: &MouseGesture) -> Option<StepOutcome> {
        // A drag owns every non-wheel gesture until the button comes back up
        // (upstream's `if (this.scrollbarDrag) { … return true; }`), so a
        // stray press cannot start a selection mid-drag.
        if let Some(drag) = self.scrollbar_drag {
            return Some(match gesture.kind {
                MouseGestureKind::Release(_) => {
                    self.scrollbar_drag = None;
                    StepOutcome::Idle
                }
                MouseGestureKind::Drag(_) => {
                    // The geometry is recomputed from the live viewport, so a
                    // log that grows mid-drag still maps correctly.
                    let scrolled = self.scrollbar_geometry().is_some_and(|geometry| {
                        self.scroll_scrollbar_to_pointer(&geometry, gesture.y, drag.grab_offset)
                    });
                    if scrolled {
                        StepOutcome::Redraw
                    } else {
                        StepOutcome::Idle
                    }
                }
                _ => StepOutcome::Idle,
            });
        }

        let left = MouseButton::Left;
        match gesture.kind {
            MouseGestureKind::Press(button) if button == left => {
                let geometry = self.scrollbar_geometry_at(gesture.x, gesture.y)?;
                let on_thumb = gesture.y >= geometry.thumb_top
                    && gesture.y < geometry.thumb_top.saturating_add(geometry.thumb_height);
                let grab_offset = if on_thumb {
                    gesture.y - geometry.thumb_top
                } else {
                    geometry.thumb_height / 2
                };
                // The bar takes the pointer: drop the text selection and the
                // pending double-click, and cancel any drag autoscroll
                // (upstream's `clearTextSelection()` / `stopSelectionAutoScroll()`).
                self.selection = None;
                self.selection_dragging = false;
                self.stop_selection_autoscroll();
                self.last_click = None;
                self.scrollbar_hover = true;
                self.scrollbar_drag = Some(ScrollbarDrag { grab_offset });
                let scrolled = if on_thumb {
                    false
                } else {
                    self.scroll_scrollbar_to_pointer(&geometry, gesture.y, grab_offset)
                };
                Some(if scrolled {
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                })
            }
            _ => None,
        }
    }

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
    pub fn step_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        // A modal owns the mouse exactly as it owns the keyboard
        // (`step_key`): gestures are hit-tested against the open overlays'
        // rectangles and never reach the chat log underneath.
        if self.dialog.is_some() || self.settings.is_some() || self.selector.is_some() {
            // A modal covers the chat log, so the bar behind it is neither
            // hovered nor draggable while one is up (upstream's `hasOverlay()`
            // guard in `getScrollbarTargetAt`).
            self.scrollbar_hover = false;
            self.scrollbar_drag = None;
            return self.step_modal_mouse_gesture(gesture);
        }
        // The search bar is an overlay too, but it lives alongside the chat
        // log instead of over a modal, so it gets the same first pass.
        if let Some(outcome) = self.step_search_mouse_gesture(&gesture) {
            // The bar was consumed by the search bar's own rows.
            self.scrollbar_hover = false;
            self.scrollbar_drag = None;
            return outcome;
        }
        // No modal is up, so a modal click cannot still be pending.
        self.modal_mouse_press = None;
        // The pill is the first thing on the transcript to get the pointer
        // (upstream `handleScrollToEndIndicatorMouseEvent`, tested before the
        // scrollbar and the selection, `packages/tui/src/tui-alt-screen.ts:1017-1024`):
        // a left press on it jumps to the tail instead of starting a
        // selection of the pill's own text.
        if let Some(rect) = self.scroll_to_end_rect() {
            let on_pill = gesture.y == rect.y
                && gesture.x >= rect.x
                && gesture.x < rect.x.saturating_add(rect.width);
            if on_pill && matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)) {
                self.stop_selection_autoscroll();
                self.selection = None;
                self.selection_dragging = false;
                self.scrollbar_hover = false;
                self.scrollbar_drag = None;
                self.messages.set_following(true);
                return StepOutcome::Redraw;
            }
        }
        // The "cut above" hint is the second piece of transcript furniture to
        // get the pointer: it advertises `tui.altScreen.top`, so a left press
        // on it does exactly what that key does — never a selection of cells
        // the hint is covering.
        if let Some(rect) = self.truncated_above_rect() {
            let on_hint = gesture.y == rect.y
                && gesture.x >= rect.x
                && gesture.x < rect.x.saturating_add(rect.width);
            if on_hint && matches!(gesture.kind, MouseGestureKind::Press(MouseButton::Left)) {
                self.stop_selection_autoscroll();
                self.selection = None;
                self.selection_dragging = false;
                self.scrollbar_hover = false;
                self.scrollbar_drag = None;
                self.scroll_viewport_to_top();
                return StepOutcome::Redraw;
            }
        }
        // The scrollbar is hit-tested before the selection path, exactly
        // like upstream (`handleScrollbarMouseEvent` runs before
        // `handleSelectionMouseEvent`). While a drag owns the pointer the
        // hover flag stays set; otherwise it follows the pointer.
        let handled = self.step_scrollbar_mouse_gesture(&gesture);
        let hover_changed = if self.scrollbar_drag.is_some() {
            false
        } else {
            self.update_scrollbar_hover(gesture.x, gesture.y)
        };
        let outcome = match handled {
            Some(outcome) => outcome,
            None => self.step_selection_mouse_gesture(&gesture),
        };
        if matches!(outcome, StepOutcome::Idle) && hover_changed {
            StepOutcome::Redraw
        } else {
            outcome
        }
    }

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

    /// Route a gesture to the open modal overlays.
    ///
    /// Upstream dispatches to the topmost overlay under the pointer and only
    /// falls back to the layout underneath when no overlay rectangle contains
    /// the event (`packages/tui/src/tui.ts:824-847`,
    /// `tui-alt-screen.ts:912-922`). This port swallows the gesture either
    /// way: LUM-1124 already made an open modal own the mouse
    /// (`tests/mouse_selection.rs::an_open_modal_swallows_gestures`) and the
    /// chat log is not a layout component here, but a hit on a modal's
    /// rectangle is a click *on* that modal.
    ///
    /// A click only counts when the left press and the left release land on
    /// the same cell — upstream's `isClick`
    /// (`packages/tui/src/tui-alt-screen.ts:1312-1315`) — and committing one
    /// drops the chat-log selection, like upstream's click path
    /// (`packages/tui/src/tui-alt-screen.ts:1330-1339`). A press on its own
    /// does *not* clear: upstream clears once a handled press starts a
    /// component gesture (`tui-alt-screen.ts:925-930`), but this port has no
    /// press-capture target yet, and clearing on press would throw the
    /// selection away even when the pointer is dragged off the modal before
    /// releasing. A hit click never queues a clipboard request — copy-on-select
    /// belongs to the chat log, not to a modal.
    fn step_modal_mouse_gesture(&mut self, gesture: MouseGesture) -> StepOutcome {
        let hit = self
            .mouse_regions()
            .into_iter()
            .find_map(|region| region.capture(gesture));
        let Some(point) = hit else {
            // Outside every overlay rectangle the modal still swallows the
            // gesture; a press that missed cannot become a click.
            if matches!(gesture.kind, MouseGestureKind::Release(MouseButton::Left)) {
                self.modal_mouse_press = None;
            }
            return StepOutcome::Idle;
        };
        match gesture.kind {
            MouseGestureKind::Press(MouseButton::Left) => {
                self.modal_mouse_press = Some(point);
                StepOutcome::Idle
            }
            MouseGestureKind::Release(MouseButton::Left) => {
                let clicked = self.modal_mouse_press.take() == Some(point);
                if clicked && self.has_selection() {
                    self.clear_selection();
                    StepOutcome::Redraw
                } else {
                    StepOutcome::Idle
                }
            }
            // Drags inside a modal, bare moves and the other buttons are
            // consumed without changing anything.
            _ => StepOutcome::Idle,
        }
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
        lines.get(line).map(|line| crate::styled::plain_text(line))
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
        let col = x.saturating_sub(origin_x) as usize;
        Some((start + row, col))
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
            let text = crate::styled::plain_text(line);
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
    /// Returns `false` when [`MAX_IMAGE_ATTACHMENTS`](crate::editor::MAX_IMAGE_ATTACHMENTS)
    /// chips are already attached; the caller surfaces the refusal. The
    /// draft text is left untouched either way.
    pub fn paste_image(&mut self, image: pi_protocol::ImageContent) -> bool {
        match self.prompt.editor_mut().insert_image(image) {
            crate::editor::ImageInsertOutcome::Inserted => true,
            crate::editor::ImageInsertOutcome::AtCapacity => {
                self.flash_status(format!(
                    "At most {} images can be attached to a prompt",
                    crate::editor::MAX_IMAGE_ATTACHMENTS
                ));
                false
            }
        }
    }

    /// Insert clipboard *text* at the cursor — the fallback
    /// `app.clipboard.pasteImage` takes when the clipboard holds no image
    /// (upstream `handleClipboardPaste`'s else branch).
    pub fn paste_text(&mut self, text: &str) {
        self.prompt.editor_mut().insert_str(text);
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
        let Some((start, end)) = self.selection.and_then(|selection| selection.bounds()) else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (visible_start, lines) = self.messages.visible_lines(area.width, area.height);
        let visible_end = visible_start + lines.len();
        let first = start.line.max(visible_start);
        let last = end.line.min(visible_end.saturating_sub(1));
        if first > last {
            return;
        }
        for line_idx in first..=last {
            let row = (line_idx - visible_start) as u16;
            let y = area.y + row;
            let text = crate::styled::plain_text(&lines[row as usize]);
            let len = text.chars().count();
            let from = if line_idx == start.line {
                start.col.min(len)
            } else {
                0
            };
            let to = if line_idx == end.line {
                selection_end_column(&end, len)
            } else {
                len
            };
            for col in from..to {
                let x = area.x + col as u16;
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.modifier |= Modifier::REVERSED;
                }
            }
        }
    }

    /// Lines per page — one message-viewport height, but never zero so a
    /// key press before the first render is still a no-op instead of a
    /// panic.
    fn message_page(&self) -> usize {
        let (_, height) = self.viewport();
        height.max(1) as usize
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
        // Render the extension regions and budget the chrome before anything
        // else: the message viewport this frame paints is what the scroll,
        // selection and search paths must index.
        let frame = self.composed_frame(area.width, area.height);
        let layout = plan_chrome(area.height, &frame);
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
        self.viewport_width
            .store(message_area.width, Ordering::Relaxed);
        self.viewport_reserved.store(reserved, Ordering::Relaxed);
        self.viewport_height
            .store(message_area.height, Ordering::Relaxed);
        self.viewport_origin
            .0
            .store(message_area.x, Ordering::Relaxed);
        self.viewport_origin
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
        // [`crate::extension_ui::plan_chrome`] for the budget.
        let message_height = layout.message;
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
            y: message_area.y + message_height,
            width: area.width,
            height: layout.editor,
        };
        let below_area = Rect {
            x: area.x,
            y: editor_area.y + layout.editor,
            width: area.width,
            height: layout.below,
        };
        let status_area = Rect {
            x: area.x,
            y: below_area.y + layout.below,
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
            self.scroll_to_end.2.store(0, Ordering::Relaxed);
            self.truncated_above.2.store(0, Ordering::Relaxed);
        }

        // The editor region: a custom component (a non-overlay `custom`
        // session or `set_editor_component`) replaces the prompt line
        // entirely.
        match &frame.editor {
            Some(lines) => self.paint_extension_lines(editor_area, lines, buf),
            None => {
                self.paint_prompt(editor_area, buf);
                self.paint_autocomplete(message_area, editor_area, buf);
            }
        }

        // Below-editor widgets.
        self.paint_extension_lines(below_area, &frame.below, buf);

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
                for (col, ch) in line.chars().enumerate() {
                    let x = area.x + col as u16;
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                    }
                }
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
    fn paint_extension_lines(&self, rect: Rect, lines: &[StyledLine], buf: &mut Buffer) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        for (row, line) in lines.iter().enumerate() {
            if row as u16 >= rect.height {
                break;
            }
            write_styled_line(
                buf,
                rect.x,
                rect.y + row as u16,
                rect.width,
                line,
                &self.theme,
            );
        }
    }

    /// Paint the built-in prompt into the first row of the editor region.
    fn paint_prompt(&self, rect: Rect, buf: &mut Buffer) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let line = self.prompt.render_line(rect.width);
        // Upstream paints the editor chrome in `bashMode` while the buffer is
        // a `!` submission, otherwise in the thinking level's border colour
        // (`updateEditorBorderColor`,
        // `interactive-mode.ts:4166-4174`). The Rust prompt has no border, so
        // the label carries the colour instead: bash mode wins, the thinking
        // level colours everything else.
        let label_slot = if crate::editor::is_bash_mode(&self.prompt.text()) {
            ThemeColor::BashMode
        } else {
            thinking_border_color(self.thinking_level)
        };
        let label_style = Some(SpanStyle::fg(label_slot).to_style(&self.theme));
        let label_width = self.prompt.label().chars().count() as u16;
        for (col, ch) in line.chars().enumerate() {
            let x = rect.x + col as u16;
            if x >= rect.x + rect.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, rect.y)) {
                cell.set_char(ch);
                if let Some(style) = label_style {
                    if (col as u16) < label_width {
                        cell.set_style(style);
                    }
                }
            }
        }
    }

    /// Paint the composer's autocomplete dropdown into the rows directly
    /// above the editor, on top of the message view.
    ///
    /// The dropdown itself belongs to [`crate::Editor`] (candidates,
    /// windowing, selection); the App only places it. Upstream draws the
    /// list above the input and grows it towards older output
    /// (`Editor.renderAutocomplete`), so the rows are anchored to the
    /// editor's top edge and the prompt line is never covered. Without this
    /// the provider could be installed and still show nothing, which is
    /// exactly the state LUM-1236 found: the engine and the keyboard map
    /// were in place, the paint call was not.
    fn paint_autocomplete(&self, message_area: Rect, editor_area: Rect, buf: &mut Buffer) {
        let editor = self.prompt.editor();
        if !editor.is_showing_autocomplete() || editor_area.width == 0 {
            return;
        }
        // The dropdown borrows rows from the transcript viewport: never
        // paint over the header / above-editor regions, and never over the
        // editor row itself.
        let available = editor_area.y.saturating_sub(message_area.y) as usize;
        if available == 0 {
            return;
        }
        let width = editor_area.width as usize;
        let mut rows = editor.autocomplete_render_lines(width);
        if rows.is_empty() {
            return;
        }
        // On a short terminal keep the candidates nearest the prompt. The
        // list is already windowed around the selection, so the tail is the
        // part the user is actually steering.
        if rows.len() > available {
            rows.drain(..rows.len() - available);
        }
        let selected_style = SpanStyle::fg(ThemeColor::Accent);
        let plain_style = SpanStyle::fg(ThemeColor::Muted);
        let first_row = editor_area.y - rows.len() as u16;
        for (offset, row) in rows.iter().enumerate() {
            let style = if row.starts_with('❯') {
                selected_style
            } else {
                plain_style
            };
            // Pad to the full width so a short candidate never leaves the
            // transcript's cells showing through on its right.
            let mut text = row.clone();
            text.push_str(&" ".repeat(width.saturating_sub(text.chars().count())));
            let line = [StyledSpan::new(text, style)];
            let y = first_row + offset as u16;
            write_styled_line(buf, editor_area.x, y, editor_area.width, &line, &self.theme);
        }
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
        let layout = plan_chrome(height, &frame);
        self.render_to_buffer_impl(area, &mut buf, false, false, &frame, &layout);
        let lines = buf
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
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
            search_lines: self
                .search
                .as_ref()
                .map(|state| {
                    crate::search::search_bar_text(&render_search_bar(&state.bar, width).lines)
                })
                .unwrap_or_default(),
            status: self.status_for_render().into_owned(),
        }
    }

    /// Convert a raw `crossterm` event into an [`InputEvent`]. Used by
    /// the binary entry point.
    pub fn translate_event(event: CtEvent) -> InputEvent {
        match event {
            CtEvent::Key(key) => InputEvent::from(key),
            CtEvent::Mouse(mouse) => match mouse.kind {
                CtMouseEventKind::ScrollUp => InputEvent::Mouse {
                    up: true,
                    alt: mouse.modifiers.contains(CtModifiers::ALT),
                },
                CtMouseEventKind::ScrollDown => InputEvent::Mouse {
                    up: false,
                    alt: mouse.modifiers.contains(CtModifiers::ALT),
                },
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
            },
            CtEvent::Resize(w, h) => InputEvent::Resize {
                width: w,
                height: h,
            },
            _ => InputEvent::Ignored,
        }
    }

    /// Translate a slice of `crossterm` events.
    pub fn translate_events<I: IntoIterator<Item = CtEvent>>(events: I) -> Vec<InputEvent> {
        events.into_iter().map(Self::translate_event).collect()
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
    use crate::message::Role;
    use pi_agent_core::AgentOptions;
    use pi_ai::providers::faux::FauxProvider;
    use pi_protocol::{Api, AssistantMessage, Model, ProviderId, ToolCall, ToolResult};

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

    fn test_app() -> App {
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
        app.apply_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        // First fragment carries id + name; later fragments only arguments.
        app.apply_event(tool_delta(0, Some("call_1"), Some("read"), "{\"path\":"));
        app.apply_event(tool_delta(0, None, None, "\"/tmp/x\""));
        app.apply_event(tool_delta(0, None, None, "}"));
        app.apply_event(AgentEvent::MessageEnd {
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
        app.apply_event(AgentEvent::MessageEnd { message });

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
        app.apply_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        app.apply_event(tool_delta(0, Some("call_1"), Some("read"), "{\"path\":"));
        app.apply_event(tool_delta(0, None, None, "\"/tmp/x\"}"));
        app.apply_event(AgentEvent::MessageEnd {
            message: finished_message(),
        });
        app.apply_event(AgentEvent::ToolExecutionStart {
            call: ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": "/tmp/x" }),
            },
        });
        app.apply_event(AgentEvent::ToolExecutionEnd {
            result: ToolResult {
                tool_call_id: "call_1".into(),
                content: Box::new(Content::text("ok")),
                is_error: false,
                details: None,
                added_tool_names: None,
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
        app.apply_event(AgentEvent::MessageStart {
            model: "faux-model".into(),
        });
        app.apply_event(tool_delta(0, Some("call_a"), Some("read"), "{}"));
        app.apply_event(tool_delta(1, Some("call_b"), Some("list"), "{}"));
        app.apply_event(AgentEvent::MessageEnd {
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
        app.apply_event(AgentEvent::ToolExecutionEnd {
            result: ToolResult {
                tool_call_id: "orphan".into(),
                content: Box::new(Content::text("done")),
                is_error: false,
                details: None,
                added_tool_names: None,
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

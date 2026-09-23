//! Bottom-of-screen status bar — surfaces the active model, the
//! session identifier, and the rolling token usage.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`.
//!
//! LUM-1467 aligned the stats row's *fields* with upstream: `↑/↓` token
//! totals, `R/W` cache totals, `CH<n>%` latest cache-hit rate, `$<cost>`
//! (with ` (sub)` on subscription providers), the context gauge with its
//! ` (auto)` suffix, and the `(provider) model` prefix when more than one
//! provider is routable (`footer.ts:130-200`). The row's arrangement is
//! upstream's too — stats on the left, model on the right — while the middle
//! session segment stays a deliberate deviation (see [`session_segment`]).

use std::time::Duration;

use pi_protocol::Usage;

use crate::loader::format_elapsed;
use crate::styled::{
    plain_text, themed_text, write_plain_row, write_styled_line, SpanStyle, StyledLine, StyledSpan,
};
use crate::styles::SelectListStyles;
use crate::theme::{Theme, ThemeColor};
use crate::width::{columns, prefix_columns};

/// The spinner frame plus the wall-clock time the current turn has been
/// running, drawn at the head of the status bar while a turn is in flight.
///
/// The App owns the clock (one `Instant` per turn) and the frame cursor
/// ([`crate::loader::Spinner`]); this struct is the immutable snapshot the
/// render path consumes, so the status bar itself stays stateless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyIndicator {
    /// The spinner glyph to draw (see [`crate::loader::SPINNER_FRAMES`]).
    pub frame: char,
    /// Time since the turn was submitted.
    pub elapsed: Duration,
}

/// Per-1M-token model pricing, in micro-USD, used to accumulate
/// [`StatusData::cost_micros`] as turns report usage.
///
/// This is the `pi-tui`-local copy of the catalog's `Pricing`
/// (`pi-ai/src/providers/registry.rs`): the TUI cannot depend on `pi-ai`, so
/// the driver installs the rates and the status bar does the arithmetic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusPricing {
    /// Price per 1M input tokens, in micro-USD (USD × 1e6).
    pub input_micro_usd: u64,
    /// Price per 1M output tokens, in micro-USD.
    pub output_micro_usd: u64,
    /// Price per 1M cached-input (prompt-cache read) tokens, in micro-USD.
    pub cache_read_micro_usd: u64,
    /// Price per 1M cache-write tokens, in micro-USD.
    pub cache_write_micro_usd: u64,
}

impl StatusPricing {
    /// The cost of one turn's usage in micro-USD.
    ///
    /// Each term is truncated toward zero after the division, matching the
    /// integer-micro-USD resolution [`StatusData::cost_micros`] keeps; the
    /// rendered `$x.xxx` is rounded from the accumulated total, which is where
    /// upstream's float arithmetic ends up too.
    pub fn cost_micros(&self, usage: &Usage) -> u64 {
        let term = |tokens: u32, rate: u64| -> u64 {
            ((tokens as u128) * (rate as u128) / 1_000_000) as u64
        };
        term(usage.input, self.input_micro_usd)
            .saturating_add(term(usage.output, self.output_micro_usd))
            .saturating_add(term(usage.cache_read, self.cache_read_micro_usd))
            .saturating_add(term(usage.cache_write, self.cache_write_micro_usd))
    }
}

/// Snapshot of the data the status bar renders.
///
/// `Eq` is deliberately absent (unlike the pre-LUM-1467 type):
/// [`StatusData::latest_cache_hit_rate`] is an `f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusData {
    /// Active model display label (e.g. `gpt-4o`).
    pub model: String,
    /// Session identifier shown in the middle of the bar.
    pub session_id: String,
    /// Working directory the session runs in, absolute and host-supplied.
    ///
    /// When present the footer grows a row above the stats line and prints it
    /// the way upstream's `FooterComponent` does — home shortened to `~`,
    /// joined with the git branch and the session name
    /// (`packages/coding-agent/src/modes/interactive/components/footer.ts:119-127`).
    /// `None` renders no location row at all (a host with no working
    /// directory has nothing to say there), which is also why the default
    /// frame stays single-row.
    pub cwd: Option<String>,
    /// Current git branch for [`StatusData::cwd`], or `None` outside a repo
    /// or on a detached HEAD — exactly upstream's `getGitBranch()` contract
    /// (`core/footer-data-provider.ts:126-132`).
    pub git_branch: Option<String>,
    /// Display name set with `/name`, shown in place of the identifier
    /// when present — upstream's footer shows `pwd • name` instead of the
    /// session id (`footer.ts:122-126`).
    pub session_name: Option<String>,
    /// Cumulative input tokens.
    pub input_tokens: u32,
    /// Cumulative output tokens.
    pub output_tokens: u32,
    /// Cumulative cached input tokens (`R` in the footer).
    pub cache_read: u32,
    /// Cumulative cache-write tokens (`W` in the footer).
    pub cache_write: u32,
    /// Cumulative session cost in micro-USD (USD × 1e6), so `$0.123` is
    /// `123_000`. `None` until a model with known pricing reports usage —
    /// upstream hides the `$` part while `usageTotals.cost` is falsy
    /// (`footer.ts:139-143`).
    pub cost_micros: Option<u64>,
    /// Cache-hit rate of the most recent prompt, as a percentage
    /// (`cache_read / (input + cache_read + cache_write) × 100`). `None`
    /// before any turn, or when that prompt reported zero tokens — upstream's
    /// `latestPromptTokens > 0` guard (`footer.ts:88-98`).
    pub latest_cache_hit_rate: Option<f32>,
    /// Auto-compaction switch — upstream's `(auto)` suffix on the context
    /// gauge (`footer.ts:150`). `false` keeps the pre-LUM-1467 frame.
    pub auto_compact: bool,
    /// How many providers the host can route to. Upstream prepends
    /// `(provider) ` to the model only when this is `> 1`
    /// (`footer.ts:191-197`). `0`/`1` hide it.
    pub provider_count: usize,
    /// The active model's provider name, used for that prefix.
    pub provider_label: Option<String>,
    /// True when the active provider bills through a subscription (upstream's
    /// `kimi-coding` special case plus `isUsingSubscription`): the cost part
    /// then renders ` (sub)` (`footer.ts:139-143`).
    pub subscription: bool,
    /// Rates that turn [`StatusData::add_usage`]'s token counts into
    /// [`StatusData::cost_micros`]. `None` leaves the cost unknown.
    pub pricing: Option<StatusPricing>,
    /// Context tokens consumed by the most recent turn (`0` = the window is
    /// known but no turn has reported usage yet, rendered as `?`).
    pub context_used: u32,
    /// Model context window in tokens (`0` = unknown, the gauge is hidden).
    pub context_window: u32,
    /// Free-form trailing hint (e.g. `?` for help).
    pub hint: Option<String>,
    /// When true, [`StatusData::hint`] is not the usual transient
    /// acknowledgement (`? for help`) but **live state the user is typing
    /// into** — the composer's reverse history search. The narrow layout then
    /// sacrifices it *last*: at 44 columns the query has to stay on screen,
    /// while the token counters can wait for a wider terminal.
    pub hint_pinned: bool,
    /// Busy feedback — `Some` exactly while a turn is in flight, so a frozen
    /// screen is distinguishable from a slow model at a glance. `None` hides
    /// the segment entirely (no placeholder, no stray space).
    pub busy: Option<BusyIndicator>,
    /// Status texts extensions installed with
    /// `ctx.ui.setStatus(key, text)`, already sanitized by the caller.
    ///
    /// Upstream draws them as a **third** footer row — one line, sorted by
    /// key, joined with a single space (`footer.ts:243-251`) — and the host
    /// keeps the canonical map (`footer-data-provider.ts:140-147`); the
    /// driver mirrors it here. Empty by default, which keeps the pre-LUM-1481
    /// one/two-row frames byte-identical.
    pub extension_statuses: Vec<(String, String)>,
}

impl StatusData {
    /// Convenience constructor with the most commonly used fields.
    pub fn new(model: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            session_id: session_id.into(),
            session_name: None,
            cwd: None,
            git_branch: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            cost_micros: None,
            latest_cache_hit_rate: None,
            auto_compact: false,
            provider_count: 0,
            provider_label: None,
            subscription: false,
            pricing: None,
            context_used: 0,
            context_window: 0,
            hint: None,
            hint_pinned: false,
            busy: None,
            extension_statuses: Vec::new(),
        }
    }

    /// Install or clear one extension status — upstream
    /// `setExtensionStatus(key, text)` (`footer-data-provider.ts:140-147`):
    /// `Some(text)` sets the key, `None` deletes it.
    ///
    /// The vector is kept sorted by key so the stored order is already the
    /// rendered order; [`StatusBar::render_lines`] re-sorts anyway, so a test
    /// that builds [`StatusData::extension_statuses`] by hand gets upstream's
    /// order too.
    pub fn set_extension_status(&mut self, key: &str, text: Option<&str>) {
        self.extension_statuses
            .retain(|(existing, _)| existing != key);
        if let Some(text) = text {
            self.extension_statuses
                .push((key.to_string(), text.to_string()));
            self.extension_statuses.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    /// Builder form of [`StatusData::set_extension_status`].
    pub fn with_extension_status(
        mut self,
        key: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let key = key.into();
        let text = text.into();
        self.set_extension_status(&key, Some(&text));
        self
    }

    /// Drop every extension status (a fresh session / host teardown).
    pub fn clear_extension_statuses(&mut self) {
        self.extension_statuses.clear();
    }

    /// Start (or refresh) the busy segment.
    pub fn set_busy(&mut self, frame: char, elapsed: Duration) {
        self.busy = Some(BusyIndicator { frame, elapsed });
    }

    /// Clear the busy segment — called when the turn ends so the footer is
    /// idle again.
    pub fn clear_busy(&mut self) {
        self.busy = None;
    }

    /// Set the session display name (`/name`).
    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
    }

    /// Set the working directory shown on the footer's location row.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set the git branch joined onto the location row.
    pub fn with_git_branch(mut self, branch: impl Into<String>) -> Self {
        self.git_branch = Some(branch.into());
        self
    }

    /// The footer's first row (`~/repo (main) • session name`), or `None`
    /// when no working directory is known.
    ///
    /// Upstream builds the identical string in `FooterComponent::render`
    /// (`footer.ts:119-127`): the cwd with the home directory folded to `~`,
    /// then ` (branch)` when git reports one, then ` • name` when `/name` set
    /// one.
    pub fn location_line(&self) -> Option<String> {
        let cwd = self.cwd.as_deref().filter(|cwd| !cwd.is_empty())?;
        let mut line = format_cwd_for_footer(
            cwd,
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok()
                .as_deref(),
        );
        if let Some(branch) = self.git_branch.as_deref().filter(|b| !b.is_empty()) {
            line.push_str(&format!(" ({branch})"));
        }
        if let Some(name) = self.session_name.as_deref().filter(|n| !n.is_empty()) {
            line.push_str(&format!(" • {name}"));
        }
        Some(line)
    }

    /// Set the trailing hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Accumulate token totals.
    pub fn add_tokens(&mut self, input: u32, output: u32) {
        self.input_tokens = self.input_tokens.saturating_add(input);
        self.output_tokens = self.output_tokens.saturating_add(output);
    }

    /// Accumulate one turn's full usage — input, output and cache totals —
    /// into the running session totals.
    pub fn add_usage(&mut self, usage: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input);
        self.output_tokens = self.output_tokens.saturating_add(usage.output);
        self.cache_read = self.cache_read.saturating_add(usage.cache_read);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write);
        if let Some(pricing) = self.pricing {
            let turn = pricing.cost_micros(usage);
            self.cost_micros = Some(self.cost_micros.unwrap_or(0).saturating_add(turn));
        }
        // Upstream recomputes the rate from the latest assistant message that
        // reported a prompt (`footer.ts:88-98`); a prompt with no tokens at
        // all clears it rather than leaving a stale ratio on screen.
        let prompt_tokens = usage.input as u64 + usage.cache_read as u64 + usage.cache_write as u64;
        self.latest_cache_hit_rate = if prompt_tokens > 0 {
            Some((usage.cache_read as f64 / prompt_tokens as f64 * 100.0) as f32)
        } else {
            None
        };
    }

    /// Install the active model's per-1M-token rates so
    /// [`StatusData::add_usage`] can accumulate [`StatusData::cost_micros`].
    /// `None` leaves the cost unknown (no `$` part) without discarding what
    /// earlier turns already accumulated — a session that switched from a
    /// priced model to an unpriced one still spent that money.
    pub fn set_pricing(&mut self, pricing: Option<StatusPricing>) {
        self.pricing = pricing;
    }

    /// Forget the accumulated cost (a fresh session).
    pub fn reset_cost(&mut self) {
        self.cost_micros = None;
    }

    /// Builder form of [`StatusData::set_pricing`].
    pub fn with_pricing(mut self, pricing: StatusPricing) -> Self {
        self.set_pricing(Some(pricing));
        self
    }

    /// Turn the context gauge's `(auto)` suffix on or off
    /// (`footer.ts:150`).
    pub fn with_auto_compact(mut self, enabled: bool) -> Self {
        self.auto_compact = enabled;
        self
    }

    /// Tell the bar how many providers are routable and which one is active,
    /// so the model can carry upstream's `(provider) ` prefix
    /// (`footer.ts:191-197`).
    pub fn with_provider(mut self, count: usize, label: Option<String>) -> Self {
        self.provider_count = count;
        self.provider_label = label;
        self
    }

    /// Set the model context window shown by the context gauge. `0` hides it.
    pub fn with_context_window(mut self, window: u32) -> Self {
        self.context_window = window;
        self
    }

    /// Record how many context tokens the most recent turn consumed. The
    /// window is left untouched so a driver that only receives usage can
    /// update it without clobbering the model's window.
    pub fn set_context_used(&mut self, used: u32) {
        self.context_used = used;
    }
}

/// Stateless status bar — render-only.
#[derive(Debug, Clone, Default)]
pub struct StatusBar;

impl StatusBar {
    /// Construct an empty status bar.
    pub fn new() -> Self {
        Self
    }

    /// How many rows [`StatusBar::render_lines`] draws for this snapshot.
    ///
    /// One stats row, plus a location row when the host supplied a working
    /// directory (upstream's `[pwdLine, statsLine]`), plus a third row when
    /// extensions installed statuses (`footer.ts:243-251`). The App budgets
    /// exactly this many rows for the status region, so the transcript gives
    /// up a row only when there is a row to draw.
    pub fn line_count(&self, data: &StatusData) -> u16 {
        let mut rows = 1;
        if data.location_line().is_some() {
            rows += 1;
        }
        if !data.extension_statuses.is_empty() {
            rows += 1;
        }
        rows
    }

    /// Every footer row as themed spans: the location row (when the host
    /// supplied a cwd) above [`StatusBar::render_styled_line`]'s stats row,
    /// and the extension-status row below it when any extension installed
    /// one.
    pub fn render_lines(&self, data: &StatusData, width: u16) -> Vec<StyledLine> {
        let mut rows = if data.location_line().is_some() { 2 } else { 1 };
        if !data.extension_statuses.is_empty() {
            rows += 1;
        }
        let mut lines: Vec<StyledLine> = Vec::with_capacity(rows);
        if let Some(location) = data.location_line() {
            lines.push(location_span(&location, width));
        }
        lines.push(self.render_styled_line(data, width));
        if !data.extension_statuses.is_empty() {
            lines.push(extension_status_line(data, width));
        }
        lines
    }

    /// [`StatusBar::render_lines`] as plain, space-padded text rows.
    pub fn render_lines_plain(&self, data: &StatusData, width: u16) -> Vec<String> {
        self.render_lines(data, width)
            .iter()
            .map(|line| plain_text(line))
            .collect()
    }

    /// Render the status bar as a single string for a given width.
    ///
    /// Multi-row output (a location row plus the stats row) is joined with
    /// `\n`; a snapshot without a working directory is exactly the one row
    /// this has always returned.
    pub fn render(&self, data: &StatusData, width: u16) -> String {
        self.render_lines_plain(data, width).join("\n")
    }

    /// Themed variant of [`StatusBar::render`].
    ///
    /// The visible text is identical to the plain render; the model is
    /// `accent`, the session id `muted`, and the token/usage segment `dim`.
    /// Upstream's two-line footer dims the whole stats line
    /// (`footer.ts:236-240`); this bar keeps the model readable by putting it
    /// in `accent` instead — a deliberate simplification. The location row is
    /// dim, like upstream's `theme.fg("dim", pwd)`. Multi-row output is
    /// joined with `\n`.
    pub fn render_themed(
        &self,
        data: &StatusData,
        width: u16,
        styles: &SelectListStyles<'_>,
    ) -> String {
        self.render_lines(data, width)
            .iter()
            .map(|line| themed_text(line, styles.theme()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Lay the bar out as theme-slot spans within `width` columns.
    ///
    /// This is the single layout implementation behind [`render`] (plain
    /// text), [`render_themed`] (ANSI strings) and the App's themed buffer
    /// path.
    ///
    /// The row is upstream's: the stats cluster on the left and the model on
    /// the right, right-aligned (`footer.ts:230-240`). The model carries a
    /// `(provider) ` prefix when more than one provider is routable, and the
    /// prefix is dropped first when that is what pushed the line over
    /// (`footer.ts:191-197`). The middle session segment is this port's own
    /// addition (upstream's stats row has none) and keeps the LUM-1466 rule:
    /// suppressed while the location row already carries the name.
    ///
    /// Two layouts share this entry point, and the switch between them is the
    /// one measured difference between them:
    ///
    /// * **Fitted** — the full `<left><session><padding><model>` line is at
    ///   most `width` columns wide, so the model sits flush right.
    /// * **Narrow** — it is not. The line is then laid out from
    ///   [`NARROW_SACRIFICE_ORDER`]: whole parts are dropped, lowest value
    ///   first, until the rest fits, and the drop is marked with a trailing
    ///   `…`. Nothing is ever cut in the middle of a part.
    ///
    /// That second layout is what the bar used to lack. The old code clipped
    /// the parts in visual order against a shrinking budget, so a real 44×14
    /// PTY (measured, `docs/LUM1367_FOOTER_BUDGET.md`) drew
    /// `…session-18d79c9c08d4dceb  i` — the *first character* of a right-hand
    /// part stranded at the right edge with no way to tell that the rest had
    /// been dropped. Whole-part dropping removes the orphans; the `…` is the
    /// honest signal that the bar is showing a subset.
    ///
    /// The busy segment is empty unless [`StatusData::busy`] is `Some`, so an
    /// idle footer is byte-identical to the pre-spinner layout.
    ///
    /// [`render`]: StatusBar::render
    /// [`render_themed`]: StatusBar::render_themed
    pub fn render_styled_line(&self, data: &StatusData, width: u16) -> StyledLine {
        let width = width as usize;
        if width == 0 {
            return StyledLine::new();
        }

        // Left cluster (outermost first): the busy spinner + elapsed while a
        // turn is in flight, then the stats parts in upstream order, then the
        // transient hint. Spans (rather than one dim string) let the context
        // gauge carry its own severity colour while the rest stays dim
        // (`footer.ts:145-176`).
        let left = left_cluster(data);

        // Right cluster: upstream's `rightSide`, the model name — with the
        // `(provider) ` prefix only when more than one provider is routable.
        let prefixed = data.provider_count > 1;
        let mut right = model_right(data, prefixed);
        let session = session_segment(data, data.location_line().is_some());

        let left_len = line_width(&left);
        let session_len = columns(&session);
        if prefixed && left_len + session_len + line_width(&right) > width {
            // Upstream's fallback (`footer.ts:194-196`): the prefix goes
            // before anything else does.
            right = model_right(data, false);
        }
        let right_len = line_width(&right);
        if left_len + session_len + right_len > width {
            // The whole line does not fit. Hand the bar to the budgeted
            // layout, which drops parts instead of cutting them (see the
            // method docs).
            return narrow_layout(data, width);
        }

        // Fitted layout: `<left><session><padding><right>` padded to exactly
        // `width`. The clips below are no-ops for a fitting line and stay as
        // the defensive tail they have always been.
        let pad_count = width - right_len - left_len - session_len;
        let padding = " ".repeat(pad_count);

        let mut spans: StyledLine = Vec::new();
        let mut remaining = width;
        spans.extend(clip_line(&left, &mut remaining));
        let session = clip(&session, &mut remaining);
        if !session.is_empty() {
            spans.push(StyledSpan::new(session, SpanStyle::fg(ThemeColor::Muted)));
        }
        let padding = clip(&padding, &mut remaining);
        if !padding.is_empty() {
            spans.push(StyledSpan::new(padding, SpanStyle::PLAIN));
        }
        spans.extend(clip_line(&right, &mut remaining));
        spans
    }

    /// Render into a `ratatui::buffer::Buffer`. Used by the
    /// [`App`](crate::App) and by the snapshot tests.
    pub fn render_to_buffer(
        &self,
        data: &StatusData,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        for (offset, line) in self.render_lines_plain(data, area.width).iter().enumerate() {
            let y = area.y + offset as u16;
            if y >= area.y + area.height {
                break;
            }
            write_plain_row(buf, area.x, y, area.width, line);
        }
    }

    /// Themed variant of [`StatusBar::render_to_buffer`]: each written cell
    /// carries the [`Style`](ratatui::style::Style) for its segment's theme
    /// slot.
    pub fn render_to_buffer_themed(
        &self,
        data: &StatusData,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        theme: &Theme,
    ) {
        for (offset, line) in self.render_lines(data, area.width).iter().enumerate() {
            let y = area.y + offset as u16;
            if y >= area.y + area.height {
                break;
            }
            write_styled_line(buf, area.x, y, area.width, line, theme);
        }
    }
}

/// The one glyph the narrow layout appends when it had to drop a part.
///
/// Upstream's `truncateToWidth` marks a cut with `"..."`
/// (`packages/tui/src/utils.ts:1060-1067`). The footer is a single row where
/// every column is budgeted, and the crate already ships `…` as its
/// truncation affordance in the transcript (`* … (+N lines, Ctrl+O to
/// expand)`), so the one-column form is used here — three columns of a 44-column
/// bar are worth more as content.
const ELLIPSIS: &str = "…";

/// One part of the status bar, and what it is worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Busy,
    /// `↑12k ↓3k` — upstream's two cumulative token arrows.
    Usage,
    /// `R12k W300` — cumulative cache-read / cache-write tokens.
    Cache,
    /// `CH65.0%` — the latest prompt's cache-hit rate.
    CacheHit,
    /// `$0.123` (optionally ` (sub)`) — cumulative spend.
    Cost,
    /// `42.0%/128k (auto)` — context pressure.
    Gauge,
    Session,
    Hint,
    Model,
}

/// The order the narrow layout gives parts up in — least valuable first.
///
/// The reasoning behind the order, for a bar that cannot show everything:
///
/// * `Hint` is transient by construction (`? for help` and friends): it is
///   the part a returning user needs least, and the startup header already
///   lists the same chords.
/// * `CacheHit` is a ratio derived from the cache totals next to it — the
///   least actionable number in the cluster.
/// * `Cache` is a detail of the usage totals, not a total of its own.
/// * `Session` is an identity the caller usually already knows (it is the
///   session they launched), and it is the widest single part — dropping it
///   buys back a whole line's worth of real numbers.
/// * `Usage` is cumulative session tokens, kept over the identity.
/// * `Cost` is cumulative money — a decision number, but one that moves
///   slowly, so it yields to `Usage` (which explains the spend) and to the
///   gauge (which is live).
/// * `Gauge` is the one number that changes a decision *right now* (context
///   pressure), so it outlives the cumulative totals.
/// * `Model` outlives everything except the busy spinner: it is the shortest
///   part and the one that says what an answer will come from.
/// * `Busy` is never dropped while it is present — it is the only sign a turn
///   is still running, and codex / Martty both keep a live activity indicator
///   when space runs short.
const NARROW_SACRIFICE_ORDER: [Zone; 8] = [
    Zone::Hint,
    Zone::CacheHit,
    Zone::Cache,
    Zone::Session,
    Zone::Usage,
    Zone::Cost,
    Zone::Gauge,
    Zone::Model,
];

/// The sacrifice order for this snapshot.
///
/// A normal hint is the first thing to go — it is transient acknowledgement
/// and the startup header already lists the same chords. A
/// [`StatusData::hint_pinned`] hint is the opposite: it is the query the user
/// is typing into, so it outlives every counter (only the model name, which
/// says what an answer will come from, is kept past it).
fn narrow_sacrifice_order(data: &StatusData) -> [Zone; 8] {
    if data.hint_pinned {
        [
            Zone::CacheHit,
            Zone::Cache,
            Zone::Session,
            Zone::Usage,
            Zone::Cost,
            Zone::Gauge,
            Zone::Model,
            Zone::Hint,
        ]
    } else {
        NARROW_SACRIFICE_ORDER
    }
}

/// The separator drawn in front of a part when it is not the first part left.
///
/// The values mirror the fitted layout's own separators, so a bar that has
/// just lost a part does not reshuffle the parts it keeps. Every lead is
/// either empty (the busy spinner, which is only ever the first part) or at
/// least one space, so two neighbouring parts can never run together — which
/// is also why the budgeted layout only ever needs the lead, never a fallback
/// gap. Upstream joins its `statsParts` with a single space and the busy
/// segment is followed by two (`footer.ts:147`), so the stats parts carry ` `
/// and the first one after the spinner carries `  `.
fn zone_lead(zone: Zone) -> &'static str {
    match zone {
        Zone::Busy => "",
        Zone::Usage => "  ",
        Zone::Cache => " ",
        Zone::CacheHit => " ",
        Zone::Cost => " ",
        Zone::Gauge => " ",
        Zone::Session => "  ",
        Zone::Hint => "  ",
        Zone::Model => "  ",
    }
}

/// The stats cluster plus the transient hint, in upstream order.
///
/// This is the fitted layout's left cluster; the budgeted layout
/// ([`emit_zones`]) rebuilds the same parts from [`zone_body`].
fn left_cluster(data: &StatusData) -> StyledLine {
    let mut spans = zone_body(data, Zone::Busy);
    let mut first = spans.is_empty();
    for zone in [
        Zone::Usage,
        Zone::Cache,
        Zone::CacheHit,
        Zone::Cost,
        Zone::Gauge,
        Zone::Hint,
    ] {
        let body = zone_body(data, zone);
        if body.is_empty() {
            continue;
        }
        if !first {
            spans.push(StyledSpan::new(
                zone_lead(zone),
                SpanStyle::fg(ThemeColor::Dim),
            ));
        }
        first = false;
        spans.extend(body);
    }
    spans
}

/// Upstream's `rightSide` (`footer.ts:178-197`): the model name, optionally
/// preceded by `(provider) ` when more than one provider is routable.
fn model_right(data: &StatusData, with_provider: bool) -> StyledLine {
    let text = match (
        with_provider,
        data.provider_label.as_deref().filter(|p| !p.is_empty()),
    ) {
        (true, Some(provider)) => format!("({provider}) {}", data.model),
        _ => data.model.clone(),
    };
    vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Accent))]
}

/// The parts present for this snapshot, in visual order.
fn narrow_zones(data: &StatusData) -> Vec<Zone> {
    let mut zones: Vec<Zone> = Vec::with_capacity(9);
    if data.busy.is_some() {
        zones.push(Zone::Busy);
    }
    for zone in [
        Zone::Usage,
        Zone::Cache,
        Zone::CacheHit,
        Zone::Cost,
        Zone::Gauge,
        Zone::Hint,
    ] {
        if !zone_body(data, zone).is_empty() {
            zones.push(zone);
        }
    }
    if !session_segment(data, data.location_line().is_some()).is_empty() {
        zones.push(Zone::Session);
    }
    zones.push(Zone::Model);
    zones
}

/// One part's spans, without [`zone_lead`]'s separator.
fn zone_body(data: &StatusData, zone: Zone) -> StyledLine {
    match zone {
        Zone::Busy => match &data.busy {
            Some(busy) => vec![
                StyledSpan::new(
                    format!("{} ", busy.frame),
                    SpanStyle::fg(ThemeColor::Accent),
                ),
                StyledSpan::new(
                    format_elapsed(busy.elapsed),
                    SpanStyle::fg(ThemeColor::Muted),
                ),
            ],
            None => StyledLine::new(),
        },
        Zone::Model => vec![StyledSpan::new(
            data.model.clone(),
            SpanStyle::fg(ThemeColor::Accent),
        )],
        Zone::Session => {
            let text = match data.session_name.as_deref().filter(|name| !name.is_empty()) {
                Some(name) => name.to_string(),
                None if data.session_id.is_empty() => String::new(),
                None => data.session_id.clone(),
            };
            vec![StyledSpan::new(text, SpanStyle::fg(ThemeColor::Muted))]
        }
        // `if (usageTotals.input) statsParts.push(\`↑…\`)` — each arrow is
        // independently conditional upstream (`footer.ts:130-131`), so a
        // fresh session carries no token part at all.
        Zone::Usage => {
            let mut spans = StyledLine::new();
            if data.input_tokens > 0 {
                spans.push(StyledSpan::new(
                    format!("↑{}", format_tokens(data.input_tokens)),
                    SpanStyle::fg(ThemeColor::Dim),
                ));
            }
            if data.output_tokens > 0 {
                if !spans.is_empty() {
                    spans.push(StyledSpan::new(" ", SpanStyle::fg(ThemeColor::Dim)));
                }
                spans.push(StyledSpan::new(
                    format!("↓{}", format_tokens(data.output_tokens)),
                    SpanStyle::fg(ThemeColor::Dim),
                ));
            }
            spans
        }
        // Same rule for `R` / `W` (`footer.ts:132-133`).
        Zone::Cache => {
            let mut spans = StyledLine::new();
            if data.cache_read > 0 {
                spans.push(StyledSpan::new(
                    format!("R{}", format_tokens(data.cache_read)),
                    SpanStyle::fg(ThemeColor::Dim),
                ));
            }
            if data.cache_write > 0 {
                if !spans.is_empty() {
                    spans.push(StyledSpan::new(" ", SpanStyle::fg(ThemeColor::Dim)));
                }
                spans.push(StyledSpan::new(
                    format!("W{}", format_tokens(data.cache_write)),
                    SpanStyle::fg(ThemeColor::Dim),
                ));
            }
            spans
        }
        // `CH<rate>%`, but only while there is cache traffic to explain it
        // (`footer.ts:134-136`).
        Zone::CacheHit => {
            let cache_traffic = data.cache_read > 0 || data.cache_write > 0;
            match (cache_traffic, data.latest_cache_hit_rate) {
                (true, Some(rate)) => vec![StyledSpan::new(
                    format!("CH{rate:.1}%"),
                    SpanStyle::fg(ThemeColor::Dim),
                )],
                _ => StyledLine::new(),
            }
        }
        // `$cost`, shown when a cost is known *or* the provider bills by
        // subscription (`footer.ts:139-143`).
        Zone::Cost => {
            let cost = data.cost_micros.unwrap_or(0);
            if cost > 0 || data.subscription {
                vec![StyledSpan::new(
                    format_cost(cost, data.subscription),
                    SpanStyle::fg(ThemeColor::Dim),
                )]
            } else {
                StyledLine::new()
            }
        }
        Zone::Gauge => {
            // `0` = unknown window, so the gauge has nothing to measure
            // against and is hidden (`StatusData::context_window`).
            if data.context_window == 0 {
                return StyledLine::new();
            }
            let (gauge, color) = context_gauge(data.context_used, data.context_window);
            let gauge = if data.auto_compact {
                format!("{gauge} (auto)")
            } else {
                gauge
            };
            vec![StyledSpan::new(gauge, SpanStyle::fg(color))]
        }
        Zone::Hint => vec![StyledSpan::new(
            data.hint.clone().unwrap_or_default(),
            SpanStyle::fg(ThemeColor::Dim),
        )],
    }
}

/// `$0.123`, or `$0.123 (sub)` on a subscription provider — upstream's
/// `$${cost.toFixed(3)}${usingSubscription ? " (sub)" : ""}`
/// (`footer.ts:141-143`).
pub fn format_cost(cost_micros: u64, subscription: bool) -> String {
    let usd = cost_micros as f64 / 1_000_000.0;
    if subscription {
        format!("${usd:.3} (sub)")
    } else {
        format!("${usd:.3}")
    }
}

/// The stats row's middle identity segment.
///
/// Upstream's `FooterComponent` has no such segment: the session name lives on
/// the `pwd` row (`footer.ts:122-126`) and the stats row is stats + model. This
/// port keeps the segment for the unnamed case — a single-row footer with no
/// location row still has to say which session it is — but suppresses it when
/// the location row is drawn *and* carries the name, so `/name` never appears
/// twice on adjacent rows.
fn session_segment(data: &StatusData, location_present: bool) -> String {
    let name = data.session_name.as_deref().filter(|name| !name.is_empty());
    if location_present && name.is_some() {
        return String::new();
    }
    match name {
        Some(name) => format!("  {name}  "),
        None if data.session_id.is_empty() => String::new(),
        None => format!("  {}  ", data.session_id),
    }
}

/// The footer's third row: every extension status, sorted by key and joined
/// with a single space, truncated to `width` columns with the crate's `…`
/// marker (`footer.ts:243-251`).
///
/// Upstream's marker for this row is the three-dot string `"..."`; the port
/// uses [`ELLIPSIS`] instead, the same deliberate deviation [`location_span`]
/// makes for the pwd row, because every truncated chrome row in this crate is
/// marked the same way (LUM-1412).
fn extension_status_line(data: &StatusData, width: u16) -> StyledLine {
    let width = width as usize;
    if width == 0 {
        return StyledLine::new();
    }
    // Upstream sorts by `localeCompare`; the port sorts by byte order, which
    // is the same order for the ASCII keys extensions use as names.
    let mut entries: Vec<&(String, String)> = data.extension_statuses.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let text = entries
        .iter()
        .map(|(_, text)| sanitize_status_text(text))
        .collect::<Vec<_>>()
        .join(" ");
    if columns(&text) <= width {
        return vec![StyledSpan::new(text, SpanStyle::PLAIN)];
    }
    let (prefix, _) = prefix_columns(&text, width.saturating_sub(1));
    vec![
        StyledSpan::new(prefix.to_string(), SpanStyle::PLAIN),
        StyledSpan::new(ELLIPSIS, SpanStyle::fg(ThemeColor::Dim)),
    ]
}

/// Upstream `sanitizeStatusText` (`footer.ts:13-20`): newlines, tabs and
/// carriage returns become spaces, runs of spaces collapse to one, and the
/// edges are trimmed — so an extension that embeds a multi-line string can
/// never break the footer's one-row-per-status invariant.
fn sanitize_status_text(text: &str) -> String {
    let flattened: String = text
        .chars()
        .map(|ch| {
            if matches!(ch, '\r' | '\n' | '\t') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    flattened
        .split(' ')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The footer's location row as one dim span, truncated to `width` columns
/// with the crate's `…` marker when the path does not fit.
fn location_span(location: &str, width: u16) -> StyledLine {
    let width = width as usize;
    if width == 0 {
        return StyledLine::new();
    }
    if columns(location) <= width {
        return vec![StyledSpan::new(
            location.to_string(),
            SpanStyle::fg(ThemeColor::Dim),
        )];
    }
    let (prefix, _) = prefix_columns(location, width.saturating_sub(1));
    vec![StyledSpan::new(
        format!("{prefix}{ELLIPSIS}"),
        SpanStyle::fg(ThemeColor::Dim),
    )]
}

/// Fold a working directory's home prefix to `~` — upstream
/// `formatCwdForFooter` (`footer.ts:38-51`).
///
/// Upstream resolves both paths and then asks `path.relative` whether the cwd
/// is inside the home directory. This port compares the two strings after
/// normalizing separators to `/` and dropping a trailing one, which is the
/// same predicate for the absolute paths every host supplies. A path that
/// merely *starts with* the home string (`/home/ada2` vs `/home/ada`) is not
/// folded, just like upstream.
///
/// Deliberate deviation: the folded form uses `/` on every platform rather
/// than the platform separator. The rest of the footer already speaks in
/// terminal columns, and a Windows path reads the same to a human either way.
pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return cwd.to_string();
    };
    let normalize = |path: &str| path.replace('\\', "/");
    let trimmed = |path: String| path.trim_end_matches('/').to_string();
    let cwd_norm = trimmed(normalize(cwd));
    let home_norm = trimmed(normalize(home));
    if home_norm.is_empty() {
        return cwd.to_string();
    }
    if cwd_norm == home_norm {
        return "~".to_string();
    }
    match cwd_norm.strip_prefix(&format!("{home_norm}/")) {
        Some(rest) if !rest.is_empty() => format!("~/{rest}"),
        _ => cwd.to_string(),
    }
}

/// Number of columns `zones` would occupy if drawn whole.
///
/// The first zone contributes no lead — [`emit_zones`] suppresses it — so the
/// two functions agree on the width of any subset.
fn narrow_width(data: &StatusData, zones: &[Zone]) -> usize {
    zones
        .iter()
        .enumerate()
        .map(|(index, zone)| {
            let lead = if index == 0 {
                0
            } else {
                columns(zone_lead(*zone))
            };
            lead + line_width(&zone_body(data, *zone))
        })
        .sum()
}

/// Draw `zones` whole, exactly [`narrow_width`] columns wide.
///
/// Every part after the first carries its own [`zone_lead`]; the first part
/// carries none, which is what lets the busy spinner be dropped without
/// leaving the model indented by a separator for a part that is gone.
fn emit_zones(data: &StatusData, zones: &[Zone]) -> StyledLine {
    let mut out: StyledLine = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        if index > 0 {
            out.push(StyledSpan::new(zone_lead(*zone), SpanStyle::PLAIN));
        }
        out.extend(zone_body(data, *zone));
    }
    out
}

/// Visible columns of a styled line, by the same terminal-column budget the
/// fitted layout measures with ([`crate::width`]).
fn line_width(line: &[StyledSpan]) -> usize {
    columns(&plain_text(line))
}

/// The budgeted layout: give up whole parts until the rest fits `width`.
///
/// Dropping is minimal — the loop stops as soon as the retained parts fit —
/// and it never empties the bar: the last remaining part is kept and clipped
/// instead. A bar that had to drop something ends in [`ELLIPSIS`] whenever a
/// column is left for it; a bar whose single remaining part is exactly
/// `width` wide does not, because there is no room to say so.
fn narrow_layout(data: &StatusData, width: usize) -> StyledLine {
    let mut zones = narrow_zones(data);
    let mut dropped = false;
    for zone in narrow_sacrifice_order(data) {
        if zones.len() <= 1 {
            break;
        }
        if !zones.contains(&zone) {
            continue;
        }
        if narrow_width(data, &zones) <= width {
            break;
        }
        zones.retain(|kept| *kept != zone);
        dropped = true;
    }

    let mut spans = emit_zones(data, &zones);
    let mut visible = line_width(&spans);
    if visible > width {
        // Only the last remaining part can still be too wide on its own: a
        // model name longer than the terminal. Cut it and say so, rather than
        // letting the bar run past its own column budget.
        let mut budget = width - 1;
        spans = clip_line(&spans, &mut budget);
        spans.push(StyledSpan::new(ELLIPSIS, SpanStyle::fg(ThemeColor::Dim)));
        visible = line_width(&spans);
    } else if dropped && visible < width {
        spans.push(StyledSpan::new(ELLIPSIS, SpanStyle::fg(ThemeColor::Dim)));
        visible += 1;
    }
    if visible < width {
        spans.push(StyledSpan::new(
            " ".repeat(width - visible),
            SpanStyle::PLAIN,
        ));
    }
    spans
}

/// The context gauge text plus the severity colour it renders with
/// (`footer.ts:151-176`): `?/128k` once the window is known but no turn has
/// reported usage yet, otherwise `42.0%/128k`, escalating from dim to warning
/// above 70% and to error above 90%.
fn context_gauge(used: u32, window: u32) -> (String, ThemeColor) {
    if used == 0 {
        return (format!("?/{}", format_tokens(window)), ThemeColor::Dim);
    }
    let percent = used as f64 / window as f64 * 100.0;
    let color = if percent > 90.0 {
        ThemeColor::Error
    } else if percent > 70.0 {
        ThemeColor::Warning
    } else {
        ThemeColor::Dim
    };
    let gauge = format!("{percent:.1}%/{}", format_tokens(window));
    (gauge, color)
}

/// Compact token-count formatting — upstream `formatTokens`
/// (`footer.ts:23-31`). Keeps the footer from overflowing on long sessions:
/// `<1000` raw, `<10k` one decimal `k`, `<1M` whole `k`, `<10M` one decimal
/// `M`, then whole `M`.
pub fn format_tokens(count: u32) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=9_999 => format!("{:.1}k", count as f64 / 1000.0),
        10_000..=999_999 => format!("{}k", (count as f64 / 1000.0).round() as u64),
        1_000_000..=9_999_999 => format!("{:.1}M", count as f64 / 1_000_000.0),
        _ => format!("{}M", (count as f64 / 1_000_000.0).round() as u64),
    }
}

/// Take at most `*remaining` columns of a styled line and decrement
/// `*remaining` by the columns taken. Like [`clip`], but preserves each span's
/// style, so the multi-span right-hand segment keeps its colours while cut.
fn clip_line(line: &[StyledSpan], remaining: &mut usize) -> StyledLine {
    let mut out = StyledLine::new();
    for span in line {
        if *remaining == 0 {
            break;
        }
        let (prefix, used) = prefix_columns(&span.text, *remaining);
        if prefix.len() == span.text.len() {
            *remaining -= used;
            out.push(span.clone());
        } else {
            out.push(StyledSpan {
                text: prefix.to_string(),
                ..span.clone()
            });
            *remaining = 0;
        }
    }
    out
}

/// Take at most `*remaining` columns of `text` and decrement `*remaining` by
/// the columns taken. Used by the themed status-bar layout so
/// the visible column budget matches the plain render. A wide glyph that
/// would straddle the budget is dropped whole rather than half-drawn.
fn clip<'t>(text: &'t str, remaining: &mut usize) -> &'t str {
    let (prefix, used) = prefix_columns(text, *remaining);
    if prefix.len() == text.len() {
        *remaining -= used;
        text
    } else {
        *remaining = 0;
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::SPINNER_FRAMES;

    #[test]
    fn renders_model_and_session() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");
        let line = bar.render(&data, 60);
        // LUM-1467 — the model is upstream's `rightSide`, flush against the
        // right edge (`footer.ts:230-240`).
        assert!(line.ends_with("gpt-4o"), "{line}");
        assert!(line.contains("abc-123"));
        assert!(line.contains("? for help"));
        // A fresh session reports no usage yet, and upstream hides the arrows
        // entirely (`footer.ts:130-131`).
        assert!(!line.contains('↑'), "{line}");
        assert!(!line.contains('↓'), "{line}");
    }

    #[test]
    fn renders_the_session_name_in_place_of_the_id() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc-123").with_session_name("my session");
        let line = bar.render(&data, 60);
        assert!(line.contains("my session"), "{line}");
        assert!(!line.contains("abc-123"), "{line}");
    }

    // -----------------------------------------------------------------
    // LUM-1466 — upstream's two-row footer: `pwd (branch) • name` above the
    // stats row (`footer.ts:119-127,230-231`).
    // -----------------------------------------------------------------

    #[test]
    fn folds_the_home_prefix_like_upstream() {
        // `footer.ts:38-51`.
        assert_eq!(
            format_cwd_for_footer("/home/ada/repo", Some("/home/ada")),
            "~/repo"
        );
        assert_eq!(format_cwd_for_footer("/home/ada", Some("/home/ada")), "~");
        assert_eq!(
            format_cwd_for_footer("/home/ada/repo/", Some("/home/ada/")),
            "~/repo"
        );
        // No home, or a path outside it, stays absolute.
        assert_eq!(format_cwd_for_footer("/srv/repo", None), "/srv/repo");
        assert_eq!(format_cwd_for_footer("/srv/repo", Some("")), "/srv/repo");
        // A sibling that merely starts with the home string is not folded.
        assert_eq!(
            format_cwd_for_footer("/home/ada2/repo", Some("/home/ada")),
            "/home/ada2/repo"
        );
        // Windows separators fold too, and the folded form reads in `/`.
        assert_eq!(
            format_cwd_for_footer("C:\\Users\\ada\\repo", Some("C:\\Users\\ada")),
            "~/repo"
        );
    }

    #[test]
    fn a_cwd_gives_the_footer_a_location_row() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc-123")
            .with_cwd("/srv/repo")
            .with_git_branch("main")
            .with_session_name("demo");
        assert_eq!(bar.line_count(&data), 2);
        assert_eq!(
            data.location_line().as_deref(),
            Some("/srv/repo (main) • demo")
        );
        let lines = bar
            .render(&data, 80)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], "/srv/repo (main) • demo");
        // The name moved up, so the stats row does not repeat it…
        assert!(!lines[1].contains("demo"), "{}", lines[1]);
        // …and with no usage and no window the stats cluster is empty, so the
        // row is upstream's: right-aligned model, nothing else.
        assert!(lines[1].ends_with("gpt-4o"), "{}", lines[1]);
    }

    #[test]
    fn without_a_cwd_the_footer_stays_one_row() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc-123")
            .with_git_branch("main")
            .with_session_name("demo");
        // A branch with no directory to hang it on renders nothing extra.
        assert_eq!(bar.line_count(&data), 1);
        assert_eq!(data.location_line(), None);
        assert_eq!(bar.render(&data, 80).lines().count(), 1);
        // An unnamed session keeps its id on the stats row even with a cwd.
        let data = StatusData::new("gpt-4o", "abc-123").with_cwd("/srv/repo");
        let lines = bar.render(&data, 80);
        assert!(lines.contains("abc-123"), "{lines}");
        assert!(!lines.contains('('), "no branch, no suffix: {lines}");
    }

    #[test]
    fn a_long_location_row_is_marked_when_it_is_cut() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "s")
            .with_cwd("/home/ada/very/deep/inside/a/long/repository")
            .with_git_branch("feature/a-long-branch");
        let lines = bar
            .render(&data, 20)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(lines[0].chars().count(), 20, "{}", lines[0]);
        assert!(lines[0].ends_with('…'), "{}", lines[0]);
        assert!(lines[1].ends_with("gpt-4o"), "{}", lines[1]);
    }

    #[test]
    fn an_extension_status_adds_a_third_row_sorted_by_key() {
        let bar = StatusBar::new();
        // Installed out of order on purpose: upstream sorts at render
        // (`footer.ts:243-247`), so insertion order must not leak into the row.
        let data = StatusData::new("gpt-4o", "abc")
            .with_extension_status("zz", "last")
            .with_extension_status("aa", "first")
            .with_extension_status("mm", "middle");
        assert_eq!(bar.line_count(&data), 2);
        let lines = bar
            .render(&data, 60)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[1].trim_end(), "first middle last");
    }

    #[test]
    fn the_extension_row_sits_below_the_stats_row_and_above_nothing() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc")
            .with_cwd("/srv/repo")
            .with_extension_status("build", "compiling");
        assert_eq!(bar.line_count(&data), 3);
        let lines = bar
            .render(&data, 60)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0], "/srv/repo");
        assert!(lines[1].ends_with("gpt-4o"), "{}", lines[1]);
        assert_eq!(lines[2].trim_end(), "compiling");
    }

    #[test]
    fn clearing_the_last_status_removes_the_row() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "abc")
            .with_extension_status("aa", "one")
            .with_extension_status("bb", "two");
        assert_eq!(bar.line_count(&data), 2);
        data.set_extension_status("aa", None);
        assert_eq!(bar.line_count(&data), 2);
        assert_eq!(bar.render(&data, 60).lines().nth(1), Some("two"));
        data.set_extension_status("bb", None);
        // The row is gone, not blank: the footer is back to one row.
        assert_eq!(bar.line_count(&data), 1);
        assert_eq!(bar.render(&data, 60).lines().count(), 1);
        assert!(data.extension_statuses.is_empty());
    }

    #[test]
    fn a_status_text_is_sanitised_to_one_line() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc")
            .with_extension_status("aa", "line one\n\tline   two \r");
        let rendered = bar.render(&data, 60);
        let row = rendered.lines().nth(1).expect("the status row");
        assert_eq!(row.trim_end(), "line one line two");
        // A status text can never smuggle a second row into the footer.
        assert_eq!(bar.line_count(&data), 2);
    }

    #[test]
    fn a_long_status_line_is_marked_when_it_is_cut() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "s")
            .with_extension_status("aa", "a-status-text-that-is-far-too-long-for-the-terminal");
        let row = bar
            .render(&data, 20)
            .lines()
            .nth(1)
            .map(str::to_string)
            .expect("the status row");
        assert_eq!(columns(&row), 20, "{row}");
        assert!(row.ends_with('…'), "{row}");
    }

    #[test]
    fn pads_to_width() {
        let bar = StatusBar::new();
        let data = StatusData::new("m", "s");
        let line = bar.render(&data, 20);
        assert!(line.chars().count() <= 20);
    }

    #[test]
    fn truncates_when_narrow() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "session-id");
        let line = bar.render(&data, 4);
        assert_eq!(line.chars().count(), 4);
    }

    #[test]
    fn formats_token_counts_compactly() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(12_345), "12k");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(12_345_678), "12M");
    }

    #[test]
    fn renders_cache_totals_and_the_context_gauge() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "abc-123").with_context_window(128_000);
        data.input_tokens = 1_500;
        data.output_tokens = 250;
        data.cache_read = 12_000;
        data.cache_write = 300;
        data.context_used = 64_000;
        let line = bar.render(&data, 90);
        // Upstream's `↑<in> ↓<out> R<cacheRead> W<cacheWrite> <gauge>` order
        // (`footer.ts:130-146`).
        assert!(line.contains("↑1.5k ↓250 R12k W300 50.0%/128k"), "{line}");
        assert!(line.ends_with("gpt-4o"), "{line}");
    }

    #[test]
    fn hides_the_cache_segment_until_a_turn_reports_it() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc-123").with_hint("? for help");
        let line = bar.render(&data, 80);
        assert!(!line.contains('R'), "{line}");
        assert!(!line.contains("%/"), "{line}");
    }

    #[test]
    fn shows_an_unknown_context_until_a_turn_reports_usage() {
        let bar = StatusBar::new();
        let data = StatusData::new("gpt-4o", "abc").with_context_window(200_000);
        let line = bar.render(&data, 80);
        assert!(line.contains("?/200k"), "{line}");
    }

    #[test]
    fn context_gauge_escalates_colour_past_the_thresholds() {
        assert_eq!(context_gauge(50_000, 100_000).1, ThemeColor::Dim);
        assert_eq!(context_gauge(75_000, 100_000).1, ThemeColor::Warning);
        assert_eq!(context_gauge(95_000, 100_000).1, ThemeColor::Error);
        // Exactly at a threshold stays in the lower band (upstream uses `>`).
        assert_eq!(context_gauge(70_000, 100_000).1, ThemeColor::Dim);
        assert_eq!(context_gauge(90_000, 100_000).1, ThemeColor::Warning);
    }

    #[test]
    fn add_usage_accumulates_every_counter() {
        let mut data = StatusData::new("m", "s");
        data.add_usage(&Usage {
            input: 10,
            output: 2,
            cache_read: 100,
            cache_write: 3,
            total: 0,
        });
        data.add_usage(&Usage {
            input: 5,
            output: 1,
            cache_read: 7,
            cache_write: 0,
            total: 0,
        });
        assert_eq!(data.input_tokens, 15);
        assert_eq!(data.output_tokens, 3);
        assert_eq!(data.cache_read, 107);
        assert_eq!(data.cache_write, 3);
    }

    #[test]
    fn a_narrow_bar_clips_the_right_segment_without_overrunning() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "session-id").with_context_window(128_000);
        data.input_tokens = 12_000;
        let line = bar.render(&data, 12);
        assert_eq!(line.chars().count(), 12);
    }

    #[test]
    fn the_busy_segment_leads_the_bar_with_a_frame_and_elapsed_time() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "abc-123").with_context_window(128_000);
        data.input_tokens = 1_500;
        data.output_tokens = 250;
        data.set_busy('⠹', Duration::from_secs(12));
        let line = bar.render(&data, 90);
        // Spinner first, then the same row's stats cluster (upstream order),
        // with the model flush right.
        assert!(line.starts_with("⠹ 12s  ↑1.5k ↓250 ?/128k"), "{line}");
        assert!(line.ends_with("gpt-4o"), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
    }

    #[test]
    fn the_busy_segment_is_absent_when_idle() {
        let bar = StatusBar::new();
        let idle = StatusData::new("gpt-4o", "abc-123");
        let line = bar.render(&idle, 60);
        // No spinner glyph, and the row is padded from the left so the model
        // keeps the right edge.
        assert!(line.ends_with("gpt-4o"), "{line}");
        assert!(!SPINNER_FRAMES.iter().any(|frame| line.contains(*frame)));
        assert_eq!(line, bar.render(&StatusData::new("gpt-4o", "abc-123"), 60));
    }

    #[test]
    fn clearing_the_busy_segment_restores_the_idle_bar() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "abc-123");
        let idle = bar.render(&data, 60);
        data.set_busy('⠋', Duration::from_secs(3));
        assert_ne!(bar.render(&data, 60), idle);
        data.clear_busy();
        assert_eq!(bar.render(&data, 60), idle);
        assert_eq!(data.busy, None);
    }

    #[test]
    fn the_busy_segment_advances_through_the_frame_table() {
        let bar = StatusBar::new();
        let mut spinner = crate::loader::Spinner::new();
        let mut data = StatusData::new("m", "s");
        for _ in 0..SPINNER_FRAMES.len() {
            data.set_busy(spinner.frame(), Duration::ZERO);
            let line = bar.render(&data, 20);
            assert!(line.starts_with(spinner.frame()), "{line}");
            spinner.advance();
        }
        // A full cycle returns to the first frame.
        assert_eq!(spinner.frame(), SPINNER_FRAMES[0]);
    }

    #[test]
    fn a_narrow_bar_clips_the_busy_segment_like_any_other() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "session-id");
        data.set_busy('⠋', Duration::from_secs(125));
        let line = bar.render(&data, 7);
        assert_eq!(line.chars().count(), 7);
        assert_eq!(line, "⠋ 2m05s");
    }

    // -----------------------------------------------------------------
    // LUM-1367/LUM-1467 — the narrow-footer budget
    // (`docs/LUM1367_FOOTER_BUDGET.md`).
    //
    // Every expectation below is the real 44-column PTY frame's data,
    // replayed through `StatusBar::render`: model `Faux test model`, a
    // 24-character session id, a `?/8.2k` context gauge, zero usage and the
    // `? for help` hint. LUM-1467 moved the stats cluster to the left and the
    // model to the right edge, and made zero-valued stats parts disappear the
    // way upstream's `if (usageTotals.input)` does, so the fitted line is 61
    // columns and every width below that exercises the budgeted layout.
    // -----------------------------------------------------------------

    /// The snapshot a real 44×14 PTY frame carries.
    fn narrow_frame_data() -> StatusData {
        StatusData::new("Faux test model", "session-18d79c9c08d4dceb")
            .with_context_window(8_200)
            .with_hint("? for help")
    }

    const NARROW_FRAME_FULL: &str = "?/8.2k  ? for help  session-18d79c9c08d4dceb  Faux test model";

    #[test]
    fn a_fitting_bar_is_exactly_width_wide_and_right_aligns_the_model() {
        let bar = StatusBar::new();
        let data = narrow_frame_data();
        // The fitted layout is `<left><session><padding><model>`: extra
        // columns land between the session id and the model, right-aligning
        // the model the way upstream's `statsLeft + padding + rightSide`
        // does (`footer.ts:205-215`).
        let fitted = |pad: usize| {
            format!(
                "?/8.2k  ? for help  session-18d79c9c08d4dceb  {}{}",
                " ".repeat(pad),
                "Faux test model"
            )
        };
        // 61 = the exact fit: no padding, and every part present.
        assert_eq!(NARROW_FRAME_FULL.chars().count(), 61);
        assert_eq!(fitted(0), NARROW_FRAME_FULL);
        assert_eq!(bar.render(&data, 61), NARROW_FRAME_FULL);
        assert_eq!(bar.render(&data, 80), fitted(19));
        assert_eq!(bar.render(&data, 120), fitted(59));
    }

    #[test]
    fn a_narrow_bar_drops_whole_parts_instead_of_cutting_them() {
        let bar = StatusBar::new();
        let data = narrow_frame_data();

        // 49..=60: the hint is the first part to go; the marker takes the
        // freed column once there is one.
        assert_eq!(
            bar.render(&data, 49),
            "?/8.2k  session-18d79c9c08d4dceb  Faux test model"
        );
        assert_eq!(
            bar.render(&data, 60),
            format!(
                "?/8.2k  session-18d79c9c08d4dceb  Faux test model…{}",
                " ".repeat(10)
            )
        );

        // 23..=48: the session id goes next, and the model stays on the
        // right edge.
        assert_eq!(bar.render(&data, 23), "?/8.2k  Faux test model");
        assert_eq!(
            bar.render(&data, 48),
            format!("?/8.2k  Faux test model…{}", " ".repeat(24))
        );

        // 15..=22: the gauge is the last number the bar gives up, because it
        // is the one that changes a decision.
        assert_eq!(bar.render(&data, 15), "Faux test model");
        assert_eq!(
            bar.render(&data, 22),
            format!("Faux test model…{}", " ".repeat(6))
        );

        // Below that only the model is left, and it is the one part the
        // budgeted layout cuts — with the crate's marker.
        assert_eq!(bar.render(&data, 14), "Faux test mod…");
    }

    /// The regression the fix exists for: the pre-fix layout clipped parts
    /// positionally, so a narrow bar could end in the *first letters* of a
    /// right-hand part. LUM-1467 changed which parts exist (zero usage is
    /// hidden) but not the invariant — no width may end in a partial stats
    /// token, and every width must fill its budget exactly.
    #[test]
    fn a_narrow_bar_never_ends_with_a_partial_token_at_any_width() {
        const ORPHANS: [&str; 8] = ["↑", "↓", "R", "W", "CH", "$", "?/", "?/8.2"];
        let bar = StatusBar::new();
        let data = narrow_frame_data();
        for width in 1u16..=120 {
            let line = bar.render(&data, width);
            assert_eq!(
                line.chars().count(),
                width as usize,
                "width {width} did not fill its budget: {line:?}"
            );
            let visible = line.trim_end();
            let visible = visible.strip_suffix('…').unwrap_or(visible).trim_end();
            for orphan in ORPHANS {
                assert!(
                    !visible.ends_with(orphan),
                    "width {width} ends in the orphan {orphan:?}: {visible:?}"
                );
            }
        }
    }

    #[test]
    fn the_busy_spinner_outlives_the_model_when_columns_run_out() {
        let bar = StatusBar::new();
        let mut data = StatusData::new("gpt-4o", "session-id");
        data.set_busy('⠋', Duration::from_secs(125));

        // 15 is the exact fit for the spinner + elapsed + model.
        assert_eq!(bar.render(&data, 15), "⠋ 2m05s  gpt-4o");
        // One column less and the model goes, not the elapsed time: a live
        // turn must stay visible, and the marker takes the freed column.
        assert_eq!(bar.render(&data, 14), format!("⠋ 2m05s…{}", " ".repeat(6)));
        assert_eq!(bar.render(&data, 16), "⠋ 2m05s  gpt-4o…");
    }

    #[test]
    fn a_model_wider_than_the_bar_is_clipped_and_marked() {
        let bar = StatusBar::new();
        let data = StatusData::new("a-very-long-model-name", "");
        // The only part left is wider than the bar, so it is the one case the
        // budgeted layout cuts — and it says so.
        assert_eq!(bar.render(&data, 10), "a-very-lo…");
        assert_eq!(bar.render(&data, 1), "…");
        assert_eq!(bar.render(&data, 0), "");
    }

    #[test]
    fn a_budgeted_bar_is_laid_out_by_the_same_span_rules_as_a_fitting_one() {
        let theme = crate::theme::builtin_theme("dark", crate::theme::ColorMode::TrueColor)
            .expect("dark theme");
        let styles = SelectListStyles::new(&theme);
        let bar = StatusBar::new();
        let data = narrow_frame_data();
        for width in 1u16..=120 {
            let plain = bar.render(&data, width);
            assert_eq!(
                crate::hyperlink::strip_ansi(&bar.render_themed(&data, width, &styles)),
                plain,
                "themed and plain renders disagree at width {width}"
            );
        }
    }

    // -----------------------------------------------------------------
    // LUM-1467 — the stats row's upstream fields (`footer.ts:130-200`).
    // -----------------------------------------------------------------

    /// A snapshot carrying every new field, so each part's text and order can
    /// be read off one row.
    fn all_fields_data() -> StatusData {
        let mut data = StatusData::new("claude-sonnet-4", "abc-123")
            .with_context_window(128_000)
            .with_auto_compact(true)
            .with_hint("? for help");
        data.input_tokens = 12_000;
        data.output_tokens = 3_000;
        data.cache_read = 12_000;
        data.cache_write = 300;
        data.context_used = 64_000;
        // The rate the last turn reported (`add_usage` derives it in
        // production; this pins the rendering).
        data.latest_cache_hit_rate = Some(65.0);
        data.cost_micros = Some(123_000);
        data
    }

    #[test]
    fn the_stats_cluster_matches_upstreams_field_order() {
        let bar = StatusBar::new();
        let line = bar.render(&all_fields_data(), 100);
        assert!(
            line.starts_with("↑12k ↓3.0k R12k W300 CH65.0% $0.123 50.0%/128k (auto)"),
            "{line}"
        );
        assert!(line.ends_with("claude-sonnet-4"), "{line}");
    }

    #[test]
    fn each_token_arrow_hides_independently_at_zero() {
        let bar = StatusBar::new();
        // `if (usageTotals.input)` / `if (usageTotals.output)`
        // (`footer.ts:130-131`).
        let mut input_only = StatusData::new("m", "s");
        input_only.input_tokens = 5;
        assert!(bar.render(&input_only, 40).contains("↑5"));
        assert!(!bar.render(&input_only, 40).contains('↓'));

        let mut output_only = StatusData::new("m", "s");
        output_only.output_tokens = 7;
        assert!(bar.render(&output_only, 40).contains("↓7"));
        assert!(!bar.render(&output_only, 40).contains('↑'));

        let fresh = StatusData::new("m", "s");
        let line = bar.render(&fresh, 40);
        assert!(!line.contains('↑') && !line.contains('↓'), "{line}");
    }

    #[test]
    fn the_cache_hit_rate_needs_traffic_and_a_reported_prompt() {
        let bar = StatusBar::new();

        // A rate with no cache traffic renders nothing (upstream's
        // `cacheRead > 0 || cacheWrite > 0` guard).
        let mut no_traffic = StatusData::new("m", "s");
        no_traffic.latest_cache_hit_rate = Some(65.0);
        assert!(!bar.render(&no_traffic, 60).contains("CH"));

        // Traffic with no reported prompt renders nothing either.
        let mut no_rate = StatusData::new("m", "s");
        no_rate.cache_read = 100;
        assert!(!bar.render(&no_rate, 60).contains("CH"));

        // Both together render `CH<rate>%`.
        no_rate.latest_cache_hit_rate = Some(65.0);
        assert!(bar.render(&no_rate, 60).contains("CH65.0%"));
    }

    #[test]
    fn the_cost_part_needs_a_cost_or_a_subscription() {
        let bar = StatusBar::new();

        // No cost, no subscription: hidden.
        let plain = StatusData::new("m", "s");
        assert!(!bar.render(&plain, 60).contains('$'));

        // A known cost renders `$x.xxx`.
        let mut paid = StatusData::new("m", "s");
        paid.cost_micros = Some(123_000);
        assert!(bar.render(&paid, 60).contains("$0.123"));

        // A subscription provider renders `$0.000 (sub)` even at zero spend
        // (`footer.ts:139-143`).
        let mut sub = StatusData::new("m", "s");
        sub.subscription = true;
        assert!(bar.render(&sub, 60).contains("$0.000 (sub)"));
    }

    #[test]
    fn the_auto_suffix_follows_the_auto_compact_switch() {
        let bar = StatusBar::new();
        let off = StatusData::new("m", "s")
            .with_context_window(128_000)
            .with_auto_compact(false);
        assert!(!bar.render(&off, 60).contains("(auto)"));
        let on = off.clone().with_auto_compact(true);
        assert!(bar.render(&on, 60).contains("/128k (auto)"));
    }

    #[test]
    fn the_provider_prefix_needs_more_than_one_provider() {
        let bar = StatusBar::new();
        let single =
            StatusData::new("claude-sonnet-4", "").with_provider(1, Some("anthropic".into()));
        assert!(!bar.render(&single, 60).contains("anthropic"));

        let multi =
            StatusData::new("claude-sonnet-4", "").with_provider(2, Some("anthropic".into()));
        assert!(bar
            .render(&multi, 60)
            .contains("(anthropic) claude-sonnet-4"));

        // No label to print: nothing to prefix with.
        let anonymous = StatusData::new("claude-sonnet-4", "").with_provider(3, None);
        assert!(!bar.render(&anonymous, 60).contains('('));
    }

    #[test]
    fn the_provider_prefix_is_dropped_before_the_bar_goes_narrow() {
        let bar = StatusBar::new();
        let data =
            StatusData::new("claude-sonnet-4", "").with_provider(2, Some("anthropic".into()));
        // `(anthropic) claude-sonnet-4` is 27 columns; the bare model is 15.
        // At 20 the prefix is what does not fit, so upstream's fallback drops
        // it (`footer.ts:194-196`) and the bar stays fitted.
        let narrow = bar.render(&data, 20);
        assert!(!narrow.contains("anthropic"), "{narrow}");
        assert_eq!(narrow, format!("{}claude-sonnet-4", " ".repeat(5)));
        // With room for both, the prefix stays.
        assert!(bar
            .render(&data, 30)
            .contains("(anthropic) claude-sonnet-4"));
    }

    #[test]
    fn add_usage_records_the_latest_prompts_cache_hit_rate() {
        let mut data = StatusData::new("m", "s");
        data.add_usage(&Usage {
            input: 300,
            output: 0,
            cache_read: 650,
            cache_write: 50,
            total: 0,
        });
        // 650 / (300 + 650 + 50) = 65%.
        assert_eq!(data.latest_cache_hit_rate, Some(65.0));

        // A prompt with no tokens at all clears it rather than keeping a
        // stale ratio (upstream's `latestPromptTokens > 0` guard).
        data.add_usage(&Usage::default());
        assert_eq!(data.latest_cache_hit_rate, None);
    }

    #[test]
    fn add_usage_accumulates_cost_from_the_installed_rates() {
        let mut data = StatusData::new("m", "s").with_pricing(StatusPricing {
            input_micro_usd: 3_000_000,
            output_micro_usd: 15_000_000,
            cache_read_micro_usd: 300_000,
            cache_write_micro_usd: 3_750_000,
        });
        // No turn yet: the cost is unknown, not zero.
        assert_eq!(data.cost_micros, None);

        data.add_usage(&Usage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
            total: 0,
        });
        // $3 + $15 + $0.30 + $3.75 = $22.05.
        assert_eq!(data.cost_micros, Some(22_050_000));
        assert_eq!(format_cost(data.cost_micros.unwrap(), false), "$22.050");

        // Installing no rates stops further accumulation but keeps what the
        // session already spent.
        data.set_pricing(None);
        assert_eq!(data.cost_micros, Some(22_050_000));
        data.reset_cost();
        assert_eq!(data.cost_micros, None);
    }

    #[test]
    fn format_cost_renders_micro_usd_with_three_decimals() {
        assert_eq!(format_cost(0, false), "$0.000");
        assert_eq!(format_cost(123_000, false), "$0.123");
        assert_eq!(format_cost(1_234_567, false), "$1.235");
        assert_eq!(format_cost(0, true), "$0.000 (sub)");
        assert_eq!(format_cost(123_000, true), "$0.123 (sub)");
    }

    #[test]
    fn a_narrow_all_fields_bar_sheds_the_cache_hit_rate_before_the_cache_totals() {
        let bar = StatusBar::new();
        let data = all_fields_data();

        // 79 = dropping the hint alone is enough, and the rate survives.
        let wide = bar.render(&data, 79);
        assert!(!wide.contains("? for help"), "{wide}");
        assert!(wide.contains("CH65.0%"), "{wide}");
        assert!(wide.contains("$0.123"), "{wide}");

        // 71..=78: the derived ratio goes next — it is worth less than the
        // cache totals it is derived from — while `$cost` and the gauge stay.
        let tighter = bar.render(&data, 71);
        assert!(!tighter.contains("CH65.0%"), "{tighter}");
        assert!(tighter.contains("R12k W300"), "{tighter}");
        assert!(tighter.contains("$0.123"), "{tighter}");

        // 61..=70: now the cache totals themselves go.
        let without_cache = bar.render(&data, 61);
        assert!(!without_cache.contains("R12k"), "{without_cache}");
        assert!(without_cache.contains("↑12k"), "{without_cache}");
        assert!(without_cache.contains("50.0%/128k"), "{without_cache}");
        assert!(without_cache.contains("claude-sonnet-4"), "{without_cache}");
    }
}

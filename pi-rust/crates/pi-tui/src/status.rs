//! Bottom-of-screen status bar — surfaces the active model, the
//! session identifier, and the rolling token usage.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`.

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

/// Snapshot of the data the status bar renders.
#[derive(Debug, Clone, PartialEq, Eq)]
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
            context_used: 0,
            context_window: 0,
            hint: None,
            hint_pinned: false,
            busy: None,
        }
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
    /// Two when the host supplied a working directory (the location row plus
    /// the stats row, upstream's `[pwdLine, statsLine]`), one otherwise. The
    /// App budgets exactly this many rows for the status region, so the
    /// transcript gives up a row only when there is a row to draw.
    pub fn line_count(&self, data: &StatusData) -> u16 {
        if data.location_line().is_some() {
            2
        } else {
            1
        }
    }

    /// Every footer row as themed spans: the location row (when the host
    /// supplied a cwd) above [`StatusBar::render_styled_line`]'s stats row.
    pub fn render_lines(&self, data: &StatusData, width: u16) -> Vec<StyledLine> {
        let mut lines: Vec<StyledLine> = Vec::with_capacity(2);
        if let Some(location) = data.location_line() {
            lines.push(location_span(&location, width));
        }
        lines.push(self.render_styled_line(data, width));
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
    /// Two layouts share this entry point, and the switch between them is the
    /// one measured difference between them:
    ///
    /// * **Fitted** — the full `<busy><model><session><padding><stats>` line
    ///   is at most `width` columns wide. It is drawn exactly as it always has
    ///   been, right-aligned, so every wide terminal is byte-identical to the
    ///   pre-LUM-1367 footer.
    /// * **Narrow** — it is not. The line is then laid out from
    ///   [`NARROW_SACRIFICE_ORDER`]: whole parts are dropped, lowest value
    ///   first, until the rest fits, and the drop is marked with a trailing
    ///   `…`. Nothing is ever cut in the middle of a part.
    ///
    /// That second layout is what the bar used to lack. The old code clipped
    /// the parts in visual order against a shrinking budget, so a real 44×14
    /// PTY (measured, `docs/LUM1367_FOOTER_BUDGET.md`) drew
    /// `Faux test model  session-18d79c9c08d4dceb  i` — the *first character*
    /// of `in 0 out 0` stranded at the right edge with no way to tell that the
    /// rest had been dropped. Whole-part dropping removes the orphans; the
    /// `…` is the honest signal that the bar is showing a subset.
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

        // Left cluster, built as spans (outermost first): the busy spinner +
        // elapsed while a turn is in flight, then the model. The spinner is
        // `accent` and the elapsed time `muted`, so the animated glyph stands
        // out from the model name next to it.
        let mut left: StyledLine = Vec::new();
        if let Some(busy) = &data.busy {
            left.push(StyledSpan::new(
                format!("{} ", busy.frame),
                SpanStyle::fg(ThemeColor::Accent),
            ));
            left.push(StyledSpan::new(
                format!("{}  ", format_elapsed(busy.elapsed)),
                SpanStyle::fg(ThemeColor::Muted),
            ));
        }
        left.push(StyledSpan::new(
            data.model.clone(),
            SpanStyle::fg(ThemeColor::Accent),
        ));

        // Right-hand side, built as spans (outermost first): cumulative
        // usage, cache totals, the context gauge, then the transient hint.
        // Spans (rather than one dim string) let the context gauge carry its
        // own severity colour while the rest stays dim (`footer.ts:145-176`).
        let mut right: StyledLine = Vec::new();
        right.push(StyledSpan::new(
            format!(
                "in {} out {}",
                format_tokens(data.input_tokens),
                format_tokens(data.output_tokens)
            ),
            SpanStyle::fg(ThemeColor::Dim),
        ));
        if data.cache_read > 0 || data.cache_write > 0 {
            right.push(StyledSpan::new(
                format!(
                    " R{} W{}",
                    format_tokens(data.cache_read),
                    format_tokens(data.cache_write)
                ),
                SpanStyle::fg(ThemeColor::Dim),
            ));
        }
        if data.context_window > 0 {
            let (gauge, color) = context_gauge(data.context_used, data.context_window);
            // The separating space stays dim; only the gauge itself escalates.
            right.push(StyledSpan::new(" ", SpanStyle::fg(ThemeColor::Dim)));
            right.push(StyledSpan::new(gauge, SpanStyle::fg(color)));
        }
        if let Some(hint) = &data.hint {
            right.push(StyledSpan::new(
                format!("  {hint}"),
                SpanStyle::fg(ThemeColor::Dim),
            ));
        }
        let right_len = line_width(&right);
        let session = session_segment(data, data.location_line().is_some());

        let left_len = line_width(&left);
        let session_len = columns(&session);
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
    Model,
    Session,
    Usage,
    Cache,
    Gauge,
    Hint,
}

/// The order the narrow layout gives parts up in — least valuable first.
///
/// The reasoning behind the order, for a bar that cannot show everything:
///
/// * `Hint` is transient by construction (`? for help` and friends): it is
///   the part a returning user needs least, and the startup header already
///   lists the same chords.
/// * `Cache` is a detail of the usage totals next to it, not a total of its
///   own.
/// * `Session` is an identity the caller usually already knows (it is the
///   session they launched), and it is the widest single part — dropping it
///   buys back a whole line's worth of real numbers.
/// * `Usage` is cumulative session cost, worth keeping over the identity.
/// * `Gauge` is the one number that changes a decision (context pressure), so
///   it outlives usage.
/// * `Model` outlives everything except the busy spinner: it is the shortest
///   part and the one that says what an answer will come from.
/// * `Busy` is never dropped while it is present — it is the only sign a turn
///   is still running, and codex / Martty both keep a live activity indicator
///   when space runs short.
const NARROW_SACRIFICE_ORDER: [Zone; 6] = [
    Zone::Hint,
    Zone::Cache,
    Zone::Session,
    Zone::Usage,
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
fn narrow_sacrifice_order(data: &StatusData) -> [Zone; 6] {
    if data.hint_pinned {
        [
            Zone::Cache,
            Zone::Session,
            Zone::Usage,
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
/// The values add up to the fitted layout's own separators, so a bar that has
/// just lost a part does not reshuffle the parts it keeps. Every lead is
/// either empty (the busy spinner, which is only ever the first part) or at
/// least one space, so two neighbouring parts can never run together — which
/// is also why the budgeted layout only ever needs the lead, never a fallback
/// gap.
fn zone_lead(zone: Zone) -> &'static str {
    match zone {
        Zone::Busy => "",
        Zone::Model => "  ",
        Zone::Session => "  ",
        Zone::Usage => "  ",
        Zone::Cache => " ",
        Zone::Gauge => " ",
        Zone::Hint => "  ",
    }
}

/// The parts present for this snapshot, in visual order.
fn narrow_zones(data: &StatusData) -> Vec<Zone> {
    let mut zones: Vec<Zone> = Vec::with_capacity(7);
    if data.busy.is_some() {
        zones.push(Zone::Busy);
    }
    zones.push(Zone::Model);
    let has_name = data
        .session_name
        .as_deref()
        .is_some_and(|name| !name.is_empty());
    if (has_name || !data.session_id.is_empty())
        && !session_segment(data, data.location_line().is_some()).is_empty()
    {
        zones.push(Zone::Session);
    }
    zones.push(Zone::Usage);
    if data.cache_read > 0 || data.cache_write > 0 {
        zones.push(Zone::Cache);
    }
    if data.context_window > 0 {
        zones.push(Zone::Gauge);
    }
    if data.hint.is_some() {
        zones.push(Zone::Hint);
    }
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
        Zone::Usage => vec![StyledSpan::new(
            format!(
                "in {} out {}",
                format_tokens(data.input_tokens),
                format_tokens(data.output_tokens)
            ),
            SpanStyle::fg(ThemeColor::Dim),
        )],
        Zone::Cache => vec![StyledSpan::new(
            format!(
                "R{} W{}",
                format_tokens(data.cache_read),
                format_tokens(data.cache_write)
            ),
            SpanStyle::fg(ThemeColor::Dim),
        )],
        Zone::Gauge => {
            let (gauge, color) = context_gauge(data.context_used, data.context_window);
            vec![StyledSpan::new(gauge, SpanStyle::fg(color))]
        }
        Zone::Hint => vec![StyledSpan::new(
            data.hint.clone().unwrap_or_default(),
            SpanStyle::fg(ThemeColor::Dim),
        )],
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
        assert!(line.starts_with("gpt-4o"));
        assert!(line.contains("abc-123"));
        assert!(line.contains("in 0 out 0"));
        assert!(line.contains("? for help"));
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
        // …but it still carries the model and the counters.
        assert!(lines[1].starts_with("gpt-4o"), "{}", lines[1]);
        assert!(lines[1].contains("in 0 out 0"), "{}", lines[1]);
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
        assert!(lines[1].starts_with("gpt-4o"), "{}", lines[1]);
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
        assert!(line.contains("in 1.5k out 250"), "{line}");
        assert!(line.contains("R12k W300"), "{line}");
        assert!(line.contains("50.0%/128k"), "{line}");
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
        // Spinner first, on the same row as the Stage 64 usage numbers.
        assert!(line.starts_with("⠹ 12s  gpt-4o"), "{line}");
        assert!(line.contains("in 1.5k out 250"), "{line}");
        assert_eq!(line.lines().count(), 1, "{line}");
    }

    #[test]
    fn the_busy_segment_is_absent_when_idle() {
        let bar = StatusBar::new();
        let idle = StatusData::new("gpt-4o", "abc-123");
        let line = bar.render(&idle, 60);
        assert!(line.starts_with("gpt-4o"), "{line}");
        // No placeholder glyph and no extra leading space.
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
    // LUM-1367 — the narrow-footer budget (`docs/LUM1367_FOOTER_BUDGET.md`).
    //
    // Every expectation below is the real 44/46/52/60/70-column PTY frame's
    // shape, replayed through `StatusBar::render` with the frame's own data:
    // model `Faux test model`, a 24-character session id, a `?/8.2k` context
    // gauge, zero usage and the `? for help` hint. The full line is 72
    // columns, so widths below that exercise the budgeted layout.
    // -----------------------------------------------------------------

    /// The snapshot a real 44×14 PTY frame carries.
    fn narrow_frame_data() -> StatusData {
        StatusData::new("Faux test model", "session-18d79c9c08d4dceb")
            .with_context_window(8_200)
            .with_hint("? for help")
    }

    const NARROW_FRAME_FULL: &str =
        "Faux test model  session-18d79c9c08d4dceb  in 0 out 0 ?/8.2k  ? for help";

    #[test]
    fn a_fitting_bar_is_unchanged_and_exactly_width_wide() {
        let bar = StatusBar::new();
        let data = narrow_frame_data();
        // The fitted layout is `<left><session><padding><right>`: extra
        // columns land between the session id and the stats, right-aligning
        // the stats exactly as the pre-LUM-1367 bar did.
        let fitted = |pad: usize| {
            format!(
                "Faux test model  session-18d79c9c08d4dceb  {}in 0 out 0 ?/8.2k  ? for help",
                " ".repeat(pad)
            )
        };
        // 72 = the exact fit: no padding, and every part present.
        assert_eq!(NARROW_FRAME_FULL.chars().count(), 72);
        assert_eq!(fitted(0), NARROW_FRAME_FULL);
        assert_eq!(bar.render(&data, 72), NARROW_FRAME_FULL);
        assert_eq!(bar.render(&data, 80), fitted(8));
        assert_eq!(bar.render(&data, 120), fitted(48));
    }

    #[test]
    fn a_narrow_bar_drops_whole_parts_instead_of_cutting_them() {
        let bar = StatusBar::new();
        let data = narrow_frame_data();

        // 60: dropping the hint alone is enough, and there is no column left
        // for the marker — the pre-fix bar drew exactly this line here.
        assert_eq!(
            bar.render(&data, 60),
            "Faux test model  session-18d79c9c08d4dceb  in 0 out 0 ?/8.2k"
        );
        // 61..=72: the same parts, now honestly marked as a subset.
        assert_eq!(
            bar.render(&data, 61),
            "Faux test model  session-18d79c9c08d4dceb  in 0 out 0 ?/8.2k…"
        );
        assert_eq!(
            bar.render(&data, 70),
            format!(
                "{}…{}",
                "Faux test model  session-18d79c9c08d4dceb  in 0 out 0 ?/8.2k",
                " ".repeat(9)
            )
        );

        // 34..=59: the session id is the next part to go, and dropping it buys
        // back every number that fits. The old bar stranding `in` / `out` at
        // the right edge is what this replaces.
        assert_eq!(
            bar.render(&data, 52),
            format!("Faux test model  in 0 out 0 ?/8.2k…{}", " ".repeat(17))
        );
        assert_eq!(
            bar.render(&data, 46),
            format!("Faux test model  in 0 out 0 ?/8.2k…{}", " ".repeat(11))
        );
        assert_eq!(
            bar.render(&data, 40),
            format!("Faux test model  in 0 out 0 ?/8.2k…{}", " ".repeat(5))
        );
        assert_eq!(bar.render(&data, 34), "Faux test model  in 0 out 0 ?/8.2k");

        // 23..=33: the usage total goes next; the context gauge is the last
        // number the bar gives up.
        assert_eq!(
            bar.render(&data, 30),
            format!("Faux test model ?/8.2k…{}", " ".repeat(7))
        );
        assert_eq!(bar.render(&data, 22), "Faux test model ?/8.2k");

        // 16..=21: only the model is left, marked as a subset.
        assert_eq!(
            bar.render(&data, 21),
            format!("Faux test model…{}", " ".repeat(5))
        );
        assert_eq!(bar.render(&data, 15), "Faux test model");
    }

    /// The regression the fix exists for: the pre-fix layout clipped parts
    /// positionally, so a narrow bar could end in the *first letters* of a
    /// right-hand part. These are the exact endings measured in a real PTY at
    /// 44/46/52/70 columns (`docs/LUM1367_FOOTER_BUDGET.md` §2) — `i`, `in`,
    /// `in 0 out` and `? for he`. No width may produce any of them again.
    #[test]
    fn a_narrow_bar_never_ends_with_a_partial_token_at_any_width() {
        const ORPHANS: [&str; 6] = ["i", "in", "in 0 out", "? for he", "? for h", "? for"];
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
}

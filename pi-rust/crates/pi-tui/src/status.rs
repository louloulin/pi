//! Bottom-of-screen status bar — surfaces the active model, the
//! session identifier, and the rolling token usage.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`.

use std::fmt::Write as _;

use pi_protocol::Usage;

use crate::styled::{
    plain_text, themed_text, write_styled_line, SpanStyle, StyledLine, StyledSpan,
};
use crate::styles::SelectListStyles;
use crate::theme::{Theme, ThemeColor};

/// Snapshot of the data the status bar renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusData {
    /// Active model display label (e.g. `gpt-4o`).
    pub model: String,
    /// Session identifier shown in the middle of the bar.
    pub session_id: String,
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
}

impl StatusData {
    /// Convenience constructor with the most commonly used fields.
    pub fn new(model: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            session_id: session_id.into(),
            session_name: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
            context_used: 0,
            context_window: 0,
            hint: None,
        }
    }

    /// Set the session display name (`/name`).
    pub fn with_session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
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

    /// Render the status bar as a single string for a given width.
    pub fn render(&self, data: &StatusData, width: u16) -> String {
        plain_text(&self.render_styled_line(data, width))
    }

    /// Themed variant of [`StatusBar::render`].
    ///
    /// The visible text is identical to the plain render; the model is
    /// `accent`, the session id `muted`, and the token/usage segment `dim`.
    /// Upstream's two-line footer dims the whole stats line
    /// (`footer.ts:236-240`); this single-line bar keeps the model readable by
    /// putting it in `accent` instead — a deliberate simplification.
    pub fn render_themed(
        &self,
        data: &StatusData,
        width: u16,
        styles: &SelectListStyles<'_>,
    ) -> String {
        themed_text(&self.render_styled_line(data, width), styles.theme())
    }

    /// Lay the bar out as theme-slot spans clipped to `width`.
    ///
    /// This is the single layout implementation behind [`render`] (plain
    /// text), [`render_themed`] (ANSI strings) and the App's themed buffer
    /// path. Each segment is clipped against the remaining budget in visual
    /// order, so the concatenated text is exactly the leading `width`
    /// characters of the full `<model><session><padding><stats>` line.
    ///
    /// [`render`]: StatusBar::render
    /// [`render_themed`]: StatusBar::render_themed
    pub fn render_styled_line(&self, data: &StatusData, width: u16) -> StyledLine {
        let width = width as usize;
        let mut left = String::new();
        let _ = write!(&mut left, "{}", data.model);

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
        let right_len = plain_text(&right).chars().count();
        let session = match data.session_name.as_deref().filter(|name| !name.is_empty()) {
            Some(name) => format!("  {name}  "),
            None if data.session_id.is_empty() => String::new(),
            None => format!("  {}  ", data.session_id),
        };

        // Layout: `<left><session><padding><right>` padded to width.
        let left_len = left.chars().count();
        let session_len = session.chars().count();
        let pad_count = if left_len + session_len + right_len < width {
            width - right_len - left_len - session_len
        } else {
            0
        };
        let padding = " ".repeat(pad_count);

        let mut spans: StyledLine = Vec::new();
        let mut remaining = width;
        let model = clip(&left, &mut remaining);
        if !model.is_empty() {
            spans.push(StyledSpan::new(model, SpanStyle::fg(ThemeColor::Accent)));
        }
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
        let line = self.render(data, area.width);
        for (col, ch) in line.chars().enumerate() {
            let x = area.x + col as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(ch);
            }
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
        let line = self.render_styled_line(data, area.width);
        write_styled_line(buf, area.x, area.y, area.width, &line, theme);
    }
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

/// Take at most `*remaining` leading `char`s of a styled line and decrement
/// `*remaining` by the number taken. Like [`clip`], but preserves each span's
/// style, so the multi-span right-hand segment keeps its colours while cut.
fn clip_line(line: &[StyledSpan], remaining: &mut usize) -> StyledLine {
    let mut out = StyledLine::new();
    for span in line {
        if *remaining == 0 {
            break;
        }
        let count = span.text.chars().count();
        if count <= *remaining {
            *remaining -= count;
            out.push(span.clone());
        } else {
            let end = span
                .text
                .char_indices()
                .nth(*remaining)
                .map(|(idx, _)| idx)
                .unwrap_or(span.text.len());
            out.push(StyledSpan {
                text: span.text[..end].to_string(),
                ..span.clone()
            });
            *remaining = 0;
        }
    }
    out
}

/// Take at most `*remaining` leading `char`s of `text` and decrement
/// `*remaining` by the number taken. Used by the themed status-bar layout so
/// the visible character budget matches the plain render.
fn clip<'t>(text: &'t str, remaining: &mut usize) -> &'t str {
    let count = text.chars().count();
    if count <= *remaining {
        *remaining -= count;
        return text;
    }
    let end = text
        .char_indices()
        .nth(*remaining)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    *remaining = 0;
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

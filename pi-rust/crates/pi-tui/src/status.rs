//! Bottom-of-screen status bar — surfaces the active model, the
//! session identifier, and the rolling token usage.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`.

use std::fmt::Write as _;

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
    /// Cumulative input tokens.
    pub input_tokens: u32,
    /// Cumulative output tokens.
    pub output_tokens: u32,
    /// Free-form trailing hint (e.g. `?` for help).
    pub hint: Option<String>,
}

impl StatusData {
    /// Convenience constructor with the most commonly used fields.
    pub fn new(model: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            session_id: session_id.into(),
            input_tokens: 0,
            output_tokens: 0,
            hint: None,
        }
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
        let mut right = String::new();
        let _ = write!(
            &mut right,
            "in {} out {}",
            data.input_tokens, data.output_tokens
        );
        if let Some(hint) = &data.hint {
            if !right.is_empty() {
                right.push_str("  ");
            }
            right.push_str(hint);
        }
        let session = if data.session_id.is_empty() {
            String::new()
        } else {
            format!("  {}  ", data.session_id)
        };

        // Layout: `<left><session><right>` padded to width.
        let left_len = left.chars().count();
        let session_len = session.chars().count();
        let right_len = right.chars().count();
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
        let stats = clip(&right, &mut remaining);
        if !stats.is_empty() {
            spans.push(StyledSpan::new(stats, SpanStyle::fg(ThemeColor::Dim)));
        }
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
}

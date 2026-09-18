//! Bottom-of-screen status bar — surfaces the active model, the
//! session identifier, and the rolling token usage.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`.

use std::fmt::Write as _;

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

        let mut out = String::with_capacity(width.max(left_len + session_len + right_len));
        out.push_str(&left);
        out.push_str(&session);
        if left_len + session_len + right_len < width {
            for _ in 0..(width - right_len - left_len - session_len) {
                out.push(' ');
            }
        }
        out.push_str(&right);
        if out.chars().count() > width {
            out = out.chars().take(width).collect();
        }
        out
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

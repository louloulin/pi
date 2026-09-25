//! `Spacer` — a fixed-height blank component.
//!
//! Mirrors upstream `Spacer` (`packages/tui/src/components/spacer.ts`).

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `Spacer` — emits `rows` blank lines of width `width`.
#[derive(Debug, Clone)]
pub struct Spacer {
    rows: u16,
}

impl Spacer {
    /// Build a spacer with `rows` blank lines.
    pub fn new(rows: u16) -> Self {
        Self { rows }
    }

    /// Build a single-row spacer.
    pub fn one() -> Self {
        Self::new(1)
    }

    /// Height in rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Set the height.
    pub fn set_rows(&mut self, rows: u16) {
        self.rows = rows;
    }
}

impl Component for Spacer {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let blank = " ".repeat(width as usize);
        (0..self.rows)
            .map(|_| vec![StyledSpan::new(blank.clone(), SpanStyle::PLAIN)])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacer_renders_n_rows() {
        let s = Spacer::new(3);
        let lines = s.render(10);
        assert_eq!(lines.len(), 3);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), " ".repeat(10));
    }

    #[test]
    fn spacer_set_rows() {
        let mut s = Spacer::new(1);
        s.set_rows(5);
        assert_eq!(s.rows(), 5);
    }
}
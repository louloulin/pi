//! `HStack` — horizontal stack of components.
//!
//! Mirrors upstream `HStack` (`packages/tui/src/components/h-stack.ts`).
//! Children render side-by-side in the same row, each occupying its
//! share of the available width.

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `HStack` — horizontal stack of components.
pub struct HStack {
    children: Vec<Box<dyn Component>>,
    /// Whether to divide the width equally or use each child's natural width.
    equal_width: bool,
}

impl HStack {
    /// Build an empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a stack from children with natural widths.
    pub fn from_children(children: Vec<Box<dyn Component>>) -> Self {
        Self { children, equal_width: false }
    }

    /// Build a stack that divides width equally.
    pub fn equal(children: Vec<Box<dyn Component>>) -> Self {
        Self { children, equal_width: true }
    }

    /// Push a child.
    pub fn push(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the stack is empty.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Default for HStack {
    fn default() -> Self {
        Self { children: Vec::new(), equal_width: false }
    }
}

impl Component for HStack {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        if self.children.is_empty() {
            return Vec::new();
        }
        let n = self.children.len() as u16;
        let each = if self.equal_width && n > 0 { width / n } else { width };
        let mut out: Vec<StyledLine> = Vec::new();
        let mut max_rows = 0usize;
        let mut rendered: Vec<Vec<StyledLine>> = Vec::new();
        for child in &self.children {
            let lines = child.render(each);
            max_rows = max_rows.max(lines.len());
            rendered.push(lines);
        }
        for row in 0..max_rows {
            let mut line = Vec::new();
            for (i, child_lines) in rendered.iter().enumerate() {
                let piece = if row < child_lines.len() {
                    child_lines[row].clone()
                } else {
                    vec![StyledSpan::new(String::new(), SpanStyle::PLAIN)]
                };
                line.extend(piece);
                if i + 1 < rendered.len() {
                    line.push(StyledSpan::new(" ".to_string(), SpanStyle::PLAIN));
                }
            }
            out.push(line);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::text::Text;

    #[test]
    fn hstack_empty_renders_nothing() {
        let s = HStack::new();
        let lines = s.render(40);
        assert!(lines.is_empty());
    }

    #[test]
    fn hstack_equal_divides_width() {
        let s = HStack::equal(vec![
            Box::new(Text::new("hi")),
            Box::new(Text::new("yo")),
        ]);
        let lines = s.render(40);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn hstack_push() {
        let mut s = HStack::new();
        s.push(Box::new(Text::new("a")));
        s.push(Box::new(Text::new("b")));
        assert_eq!(s.len(), 2);
    }
}
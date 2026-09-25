//! `VStack` — vertical stack of components.
//!
//! Mirrors upstream `VStack` (`packages/tui/src/components/v-stack.ts`).
//! Identical to `Box` in shape but without padding or header; the
//! upstream API splits them so plugin authors can read
//! `<VStack>...</VStack>` as a one-liner without `.with_padding(0)`.

use crate::component::Component;
use crate::utils::styled::StyledLine;

/// Upstream `VStack` — vertical stack of components.
pub struct VStack {
    children: Vec<Box<dyn Component>>,
}

impl VStack {
    /// Build an empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a stack from children.
    pub fn from_children(children: Vec<Box<dyn Component>>) -> Self {
        Self { children }
    }

    /// Push a child to the stack.
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

impl Default for VStack {
    fn default() -> Self {
        Self { children: Vec::new() }
    }
}

impl Component for VStack {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let mut out = Vec::new();
        for child in &self.children {
            out.extend(child.render(width));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::text::Text;

    #[test]
    fn vstack_concatenates_children() {
        let s = VStack::from_children(vec![
            Box::new(Text::new("a")),
            Box::new(Text::from_lines(["b", "c"])),
        ]);
        let lines = s.render(40);
        assert_eq!(lines.len(), 3);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "a");
        assert_eq!(crate::utils::styled::plain_text(&lines[2]), "c");
    }

    #[test]
    fn vstack_push_appends() {
        let mut s = VStack::new();
        assert!(s.is_empty());
        s.push(Box::new(Text::new("hi")));
        assert_eq!(s.len(), 1);
    }
}
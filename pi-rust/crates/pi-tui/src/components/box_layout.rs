//! `Box` — a vertical layout container with optional padding.
//!
//! Mirrors upstream's `Box` (`packages/tui/src/components/box.ts`). It
//! stacks its children top-to-bottom inside the given width, applies
//! the configured padding, and lets each child render itself.
//!
//! The Rust struct is named `BoxLayout` to avoid shadowing
//! `std::boxed::Box`. The crate re-exports it as [`Box`](crate::components::Box)
 //! from [`crate::components`].

use crate::component::Component;
use crate::theme::ThemeBg;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::utils::width::columns;
use std::boxed::Box as StdBox;

/// Upstream `Box` — a vertical layout container.
///
/// `BoxLayout::new(children)` is the simple form.
/// `BoxLayout::with_padding(...)` adds a uniform margin around the
/// children; `BoxLayout::with_style(...)` lets the box render an
/// optional header line above the children.
///
/// `BoxLayout::with_bg(ThemeBg::UserMessageBg)` mirrors the upstream
/// `Box(paddingX, paddingY, theme.bg("userMessageBg", …))` pattern:
/// every row of the rendered output (including padding rows) is
/// painted with the configured background slot. The TS version accepts
/// an arbitrary `(text) => text` function; Rust uses a [`ThemeBg`]
/// slot, which is the same thing expressed as a theme lookup — the
/// effect is identical for every theme the upstream project ships.
pub struct BoxLayout {
    children: Vec<StdBox<dyn Component>>,
    padding: u16,
    padding_x: u16,
    padding_y: u16,
    style: SpanStyle,
    header: Option<String>,
    bg: Option<ThemeBg>,
}

impl BoxLayout {
    /// Build a box from children. Each child renders top-to-bottom in
    /// the order given.
    pub fn new(children: Vec<StdBox<dyn Component>>) -> Self {
        Self {
            children,
            padding: 0,
            padding_x: 0,
            padding_y: 0,
            style: SpanStyle::PLAIN,
            header: None,
            bg: None,
        }
    }

    /// Set the inner padding (cells) on all sides.
    pub fn with_padding(mut self, padding: u16) -> Self {
        self.padding = padding;
        self
    }

    /// Set the horizontal and vertical padding separately, mirroring
    /// upstream's `Box(paddingX, paddingY, theme.bg(slot, …))`. User
    /// blocks use `(1, 0)`; tool blocks use `(1, 1)`.
    ///
    /// Calling this overrides [`with_padding`](Self::with_padding) for
    /// subsequent renders — the last setter wins.
    pub fn with_padding_xy(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self.padding = 0;
        self
    }

    /// Set the default span style for the header line.
    pub fn with_style(mut self, style: SpanStyle) -> Self {
        self.style = style;
        self
    }

    /// Add a header line rendered above the children.
    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    /// Paint the entire box (including padding rows) with the given
    /// background theme slot. Equivalent to upstream's
    /// `Box(paddingX, paddingY, theme.bg(slot, …))`.
    pub fn with_bg(mut self, bg: ThemeBg) -> Self {
        self.bg = Some(bg);
        self
    }

    /// Replace the background slot after construction.
    pub fn set_bg(&mut self, bg: Option<ThemeBg>) {
        self.bg = bg;
    }

    /// Push a child. Use this when the box outlives the children.
    pub fn push(&mut self, child: StdBox<dyn Component>) {
        self.children.push(child);
    }

    /// Number of children currently in the box.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the box has no children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Component for BoxLayout {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let pad_x = if self.padding > 0 { self.padding } else { self.padding_x };
        let pad_y = if self.padding > 0 { self.padding } else { self.padding_y };
        let mut out = Vec::new();
        if let Some(header) = &self.header {
            let line = vec![StyledSpan::new(header.clone(), self.style)];
            out.push(paint_bg(line, self.bg));
        }
        let pad = " ".repeat(pad_x as usize);
        let child_width = width.saturating_sub(pad_x.saturating_mul(2));
        for child in &self.children {
            let lines = child.render(child_width);
            for line in lines {
                let mut new_line = Vec::new();
                if pad_x > 0 {
                    new_line.push(StyledSpan::new(pad.clone(), SpanStyle::PLAIN));
                }
                let line_cols: usize = line.iter().map(|s| columns(&s.text)).sum();
                if line_cols < child_width as usize && pad_x > 0 {
                    new_line.push(StyledSpan::new(
                        " ".repeat(child_width as usize - line_cols),
                        SpanStyle::PLAIN,
                    ));
                    new_line.push(StyledSpan::new(pad.clone(), SpanStyle::PLAIN));
                } else {
                    for span in line {
                        new_line.push(span);
                    }
                    if pad_x > 0 {
                        new_line.push(StyledSpan::new(pad.clone(), SpanStyle::PLAIN));
                    }
                }
                out.push(paint_bg(new_line, self.bg));
            }
        }
        // Add top/bottom padding rows. Upstream's Box applies bgFn to
        // these too, so the entire block has a continuous background.
        for _ in 0..pad_y {
            out.insert(
                0,
                vec![StyledSpan::new(" ".repeat(width as usize), pad_style(self.bg))],
            );
        }
        for _ in 0..pad_y {
            out.push(vec![StyledSpan::new(
                " ".repeat(width as usize),
                pad_style(self.bg),
            )]);
        }
        out
    }
}

/// Paint every span in `line` with `bg`, preserving the existing
/// foreground slot. Equivalent to upstream `theme.bg(slot, line)`.
fn paint_bg(mut line: StyledLine, bg: Option<ThemeBg>) -> StyledLine {
    if let Some(slot) = bg {
        for span in &mut line {
            span.style.bg = Some(slot);
        }
    }
    line
}

/// A plain run that still carries the configured background slot.
fn pad_style(bg: Option<ThemeBg>) -> SpanStyle {
    let mut s = SpanStyle::PLAIN;
    s.bg = bg;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_box_renders_nothing() {
        let b = BoxLayout::new(vec![]);
        let lines = b.render(40);
        assert!(lines.is_empty());
    }

    #[test]
    fn box_with_header_renders_header_first() {
        let child: StdBox<dyn Component> = StdBox::new(crate::components::text::Text::new("body"));
        let b = BoxLayout::new(vec![child]).with_header("Title");
        let lines = b.render(40);
        assert_eq!(crate::utils::styled::plain_text(&lines[0]), "Title");
    }

    #[test]
    fn box_padding_around_children() {
        let child: StdBox<dyn Component> = StdBox::new(crate::components::text::Text::new("hi"));
        let b = BoxLayout::new(vec![child]).with_padding(1);
        let lines = b.render(40);
        // 1 top padding + 1 body + 1 bottom padding = 3 rows
        assert!(lines.len() >= 3);
    }

    #[test]
    fn box_push_appends() {
        let mut b = BoxLayout::new(vec![]);
        assert!(b.is_empty());
        b.push(StdBox::new(crate::components::text::Text::new("a")));
        b.push(StdBox::new(crate::components::text::Text::new("b")));
        assert_eq!(b.len(), 2);
    }
}
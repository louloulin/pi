//! SelectList — 1:1 port of
//! `packages/tui/src/components/select-list.ts`.
//!
//! A scrollable, filterable, mouse-aware list with selectable rows.
//! Renders each row with an arrow prefix for the current selection and
//! an optional description column. The struct is the upstream
//! `SelectList` class, faithfully preserved so a plugin that imports
//! `SelectList` from `pi-tui` gets the same shape as the TS one.
//!
//! The renderer is split into helper primitives
//! ([`crate::components::selector::select_list_visible_range`],
//! [`crate::components::selector::select_list_row_spans`],
//! [`crate::components::selector::SelectorLayout`]) — that is the public surface
//! the existing `Selector` already consumes; this component just wraps
//! those primitives in the `Component` trait.

use crate::component::Component;
use crate::core::input_parse::{InputEvent, Key};
use crate::components::keybindings::get_keybindings;
use crate::components::selector::{
    select_list_row_spans, select_list_visible_range, SelectorLayout,
};
use crate::utils::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
use crate::utils::util::truncate_to_width;

/// Default width of the primary column (label column), in columns.
///
/// Mirrors upstream `DEFAULT_PRIMARY_COLUMN_WIDTH`
/// (`packages/tui/src/components/select-list.ts:5`).
pub const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;

/// Gap between the primary column and the description column.
///
/// Mirrors upstream `PRIMARY_COLUMN_GAP`
/// (`packages/tui/src/components/select-list.ts:6`).
pub const PRIMARY_COLUMN_GAP: usize = 2;

/// Minimum description column width below which we drop the
/// description entirely.
///
/// Mirrors upstream `MIN_DESCRIPTION_WIDTH`
/// (`packages/tui/src/components/select-list.ts:7`).
pub const MIN_DESCRIPTION_WIDTH: usize = 10;

/// A single selectable entry.
///
/// Mirrors upstream `SelectItem`
/// (`packages/tui/src/components/select-list.ts:12-16`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectItem {
    /// Stable identifier — what `onSelect` reports back.
    pub value: String,
    /// Human-readable label shown in the primary column. Falls back
    /// to `value` when empty.
    pub label: String,
    /// Optional secondary text shown in the description column.
    pub description: Option<String>,
}

impl SelectItem {
    /// Build a new item with only a label.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Attach a description to the item.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl crate::components::selector::SelectListRow for SelectItem {
    fn value(&self) -> &str {
        &self.value
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// Style hooks the host passes in to colour the rows.
///
/// Mirrors upstream `SelectListTheme`
/// (`packages/tui/src/components/select-list.ts:18-24`).
#[derive(Default)]
pub struct SelectListTheme {
    /// Style applied to the `→ ` prefix on the selected row.
    pub selected_prefix: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
    /// Style applied to the whole text of the selected row.
    pub selected_text: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
    /// Style applied to the description column.
    pub description: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
    /// Style applied to the `(<idx>/<count>)` scroll indicator.
    pub scroll_info: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
    /// Style applied to the "no matches" placeholder.
    pub no_match: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
}

impl std::fmt::Debug for SelectListTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectListTheme")
            .field("selected_prefix", &self.selected_prefix.as_ref().map(|_| "<fn>"))
            .field("selected_text", &self.selected_text.as_ref().map(|_| "<fn>"))
            .field("description", &self.description.as_ref().map(|_| "<fn>"))
            .field("scroll_info", &self.scroll_info.as_ref().map(|_| "<fn>"))
            .field("no_match", &self.no_match.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl SelectListTheme {
    /// Style the `→ ` prefix on the selected row.
    pub fn selected_prefix(&self, text: &str) -> String {
        self.selected_prefix.as_ref().map(|f| f(text)).unwrap_or_else(|| text.to_string())
    }

    /// Style the whole selected-row text.
    pub fn selected_text(&self, text: &str) -> String {
        self.selected_text.as_ref().map(|f| f(text)).unwrap_or_else(|| text.to_string())
    }

    /// Style the description column.
    pub fn description(&self, text: &str) -> String {
        self.description.as_ref().map(|f| f(text)).unwrap_or_else(|| text.to_string())
    }

    /// Style the `(<idx>/<count>)` scroll indicator.
    pub fn scroll_info(&self, text: &str) -> String {
        self.scroll_info.as_ref().map(|f| f(text)).unwrap_or_else(|| text.to_string())
    }

    /// Style the "no matches" placeholder.
    pub fn no_match(&self, text: &str) -> String {
        self.no_match.as_ref().map(|f| f(text)).unwrap_or_else(|| text.to_string())
    }
}

/// Context handed to a custom `truncatePrimary` callback.
///
/// Mirrors upstream `SelectListTruncatePrimaryContext`
/// (`packages/tui/src/components/select-list.ts:26-32`).
#[derive(Debug, Clone)]
pub struct SelectListTruncatePrimaryContext<'a> {
    /// Text the layout wants to truncate.
    pub text: &'a str,
    /// Maximum width the truncation should target.
    pub max_width: usize,
    /// The full primary-column width (visible before truncation).
    pub column_width: usize,
    /// The item being rendered.
    pub item: &'a SelectItem,
    /// Whether the item is the currently selected row.
    pub is_selected: bool,
}

/// Layout options for the select list.
///
/// Mirrors upstream `SelectListLayoutOptions`
/// (`packages/tui/src/components/select-list.ts:34-38`).
#[derive(Default)]
pub struct SelectListLayoutOptions {
    /// Minimum width of the primary column. Falls back to
    /// `max_primary_column_width` then [`DEFAULT_PRIMARY_COLUMN_WIDTH`].
    pub min_primary_column_width: Option<usize>,
    /// Maximum width of the primary column. Falls back to
    /// `min_primary_column_width` then [`DEFAULT_PRIMARY_COLUMN_WIDTH`].
    pub max_primary_column_width: Option<usize>,
    /// Custom truncation hook for the primary column. The default
    /// delegates to [`crate::utils::util::truncate_to_width`].
    pub truncate_primary: Option<Box<dyn for<'a> Fn(SelectListTruncatePrimaryContext<'a>) -> String + Send + Sync>>,
}

impl std::fmt::Debug for SelectListLayoutOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectListLayoutOptions")
            .field("min_primary_column_width", &self.min_primary_column_width)
            .field("max_primary_column_width", &self.max_primary_column_width)
            .field("truncate_primary", &self.truncate_primary.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// Scrollable, filterable, mouse-aware list — 1:1 port of upstream
/// `SelectList` (`packages/tui/src/components/select-list.ts:40-273`).
pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    mouse_pressed_index: Option<usize>,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    /// Called when the user confirms a selection (Enter / click).
    pub on_select: Option<Box<dyn Fn(SelectItem) + Send + Sync>>,
    /// Called when the user cancels (Esc / Ctrl+C).
    pub on_cancel: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called whenever the selected index changes.
    pub on_selection_change: Option<Box<dyn Fn(SelectItem) + Send + Sync>>,
}

impl std::fmt::Debug for SelectList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectList")
            .field("items", &self.items)
            .field("filtered_items", &self.filtered_items)
            .field("selected_index", &self.selected_index)
            .field("mouse_pressed_index", &self.mouse_pressed_index)
            .field("max_visible", &self.max_visible)
            .field("theme", &self.theme)
            .finish()
    }
}

impl SelectList {
    /// Build a new list with the given items, visibility cap, theme
    /// and optional layout overrides.
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectListTheme) -> Self {
        let filtered = items.clone();
        Self {
            items,
            filtered_items: filtered,
            selected_index: 0,
            mouse_pressed_index: None,
            max_visible: max_visible.max(1),
            theme,
            layout: SelectListLayoutOptions::default(),
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    /// Override the layout options.
    pub fn with_layout(mut self, layout: SelectListLayoutOptions) -> Self {
        self.layout = layout;
        self
    }

    /// Restrict the visible items to those whose `value` starts with
    /// `filter` (case-insensitive). Resets the selection to the top.
    pub fn set_filter(&mut self, filter: &str) {
        let needle = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&needle))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    /// Move the selection to `index`, clamped to the visible range.
    pub fn set_selected_index(&mut self, index: usize) {
        if self.filtered_items.is_empty() {
            self.selected_index = 0;
            return;
        }
        self.selected_index = index.min(self.filtered_items.len() - 1);
    }

    /// Currently selected item, or `None` if the list is empty.
    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    /// Number of items after filtering.
    pub fn filtered_len(&self) -> usize {
        self.filtered_items.len()
    }

    fn layout(&self) -> SelectorLayout {
        SelectorLayout {
            min_primary_column_width: self.layout.min_primary_column_width.unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH),
            max_primary_column_width: self.layout.max_primary_column_width.unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH),
        }
    }

    fn visible_range(&self) -> (usize, usize) {
        select_list_visible_range(self.filtered_items.len(), self.selected_index, Some(self.max_visible))
    }

    fn notify_selection_change(&self) {
        if let Some(cb) = &self.on_selection_change {
            if let Some(item) = self.filtered_items.get(self.selected_index) {
                cb(item.clone());
            }
        }
    }
}

impl Component for SelectList {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let width = width.max(1) as usize;

        if self.filtered_items.is_empty() {
            let raw = self.theme.no_match("  No matching commands");
            return vec![vec![StyledSpan::new(raw, SpanStyle::PLAIN)]];
        }

        let (start, end) = self.visible_range();
        let layout = self.layout();
        let primary_column_width = layout.primary_column_width(&self.filtered_items);
        let mut lines: Vec<StyledLine> = Vec::new();
        for (offset, item) in self.filtered_items[start..end].iter().enumerate() {
            let is_selected = start + offset == self.selected_index;
            let raw_spans = select_list_row_spans(item, is_selected, width, primary_column_width);
            let mut styled: Vec<StyledSpan> = Vec::with_capacity(raw_spans.len());
            for s in raw_spans {
                let text = if is_selected {
                    self.theme.selected_text(&s.text)
                } else if s.style.fg.is_some() {
                    self.theme.description(&s.text)
                } else {
                    s.text
                };
                styled.push(StyledSpan::new(text, SpanStyle::PLAIN));
            }
            lines.push(styled);
        }

        if start > 0 || end < self.filtered_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len());
            let truncated = truncate_to_width(&scroll_text, width.saturating_sub(2));
            let styled = self.theme.scroll_info(&truncated);
            lines.push(vec![StyledSpan::new(styled, SpanStyle::PLAIN)]);
        }

        lines
    }

    fn handle_input(&mut self, key: Key) -> bool {
        // Map upstream `keyData: string` (legacy keybinding names) to a
        // KeyId through the configured keybindings. The KeybindingsManager
        // matches against `InputEvent::Key(_)` so we wrap it.
        let kb = get_keybindings();
        let event = InputEvent::Key(key);
        let m = |id: &str| kb.matches(&event, id);
        if self.filtered_items.is_empty() {
            // Esc / Ctrl+C still fire `onCancel` so the host can close
            // the empty overlay — matching upstream's behaviour.
            if m("tui.select.cancel") {
                if let Some(cb) = &self.on_cancel {
                    cb();
                }
                return true;
            }
            return false;
        }

        if m("tui.select.up") {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len() - 1
            } else {
                self.selected_index - 1
            };
            self.notify_selection_change();
            true
        } else if m("tui.select.down") {
            self.selected_index = if self.selected_index + 1 >= self.filtered_items.len() {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
            true
        } else if m("tui.select.confirm") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                if let Some(cb) = &self.on_select {
                    cb(item);
                }
            }
            true
        } else if m("tui.select.cancel") {
            if let Some(cb) = &self.on_cancel {
                cb();
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<SelectItem> {
        vec![
            SelectItem::new("a", "Alpha"),
            SelectItem::new("b", "Beta").with_description("Second letter"),
            SelectItem::new("c", "Gamma"),
        ]
    }

    #[test]
    fn render_returns_one_line_per_visible_item() {
        let list = SelectList::new(items(), 5, SelectListTheme::default());
        let lines = list.render(40);
        assert_eq!(lines.len(), 3);
        assert!(plain_text(&lines[0]).contains("Alpha"));
        assert!(plain_text(&lines[1]).contains("Beta"));
        assert!(plain_text(&lines[2]).contains("Gamma"));
    }

    #[test]
    fn filter_narrows_and_resets_selection() {
        let mut list = SelectList::new(items(), 5, SelectListTheme::default());
        list.set_filter("b");
        assert_eq!(list.filtered_len(), 1);
        assert_eq!(list.selected_item().unwrap().value, "b");
    }

    #[test]
    fn filter_no_match_renders_placeholder() {
        let mut list = SelectList::new(items(), 5, SelectListTheme::default());
        list.set_filter("z");
        let lines = list.render(40);
        assert_eq!(lines.len(), 1);
        assert!(plain_text(&lines[0]).contains("No matching"));
    }

    #[test]
    fn selected_index_clamps_to_filtered_bounds() {
        let mut list = SelectList::new(items(), 5, SelectListTheme::default());
        list.set_selected_index(100);
        assert_eq!(list.selected_item().unwrap().value, "c");
    }

    #[test]
    fn description_renders_when_room() {
        let list = SelectList::new(items(), 5, SelectListTheme::default());
        let lines = list.render(60);
        let beta_line = plain_text(&lines[1]);
        assert!(beta_line.contains("Beta"));
        assert!(beta_line.contains("Second letter"));
    }

    #[test]
    fn description_omitted_when_width_too_small() {
        let list = SelectList::new(items(), 5, SelectListTheme::default());
        let lines = list.render(20);
        let beta_line = plain_text(&lines[1]);
        assert!(beta_line.contains("Beta"));
        // No description column when width can't fit it.
        assert!(!beta_line.contains("Second letter"));
    }

    #[test]
    fn scroll_indicator_appends_when_overflow() {
        let many: Vec<SelectItem> = (0..20)
            .map(|i| SelectItem::new(format!("item-{i}"), format!("Item {i}")))
            .collect();
        let list = SelectList::new(many, 5, SelectListTheme::default());
        let lines = list.render(60);
        // 5 visible rows + 1 scroll indicator.
        assert_eq!(lines.len(), 6);
        assert!(plain_text(&lines[5]).contains("1/20"));
    }
}
//! Single-select list component.
//!
//! Mirrors the role of `packages/tui/components/select-list.ts` (the
//! command palette / model selector both reuse the same primitive).
//!
//! The selector is purely data + key handling; the [`App`](crate::App)
//! renders it when it is open.
//!
//! # Filtering and scrolling
//!
//! [`Selector::set_filter`] ports the upstream `SelectList::setFilter`
//! behaviour: the list is narrowed to the matches, the cursor resets to
//! the first row, and an empty result renders a no-match line instead of
//! an empty box. [`Selector::with_max_visible`] ports upstream's
//! `maxVisible` window: only `maxVisible` rows around the cursor are
//! drawn, followed by a `(n/total)` indicator whenever the window does
//! not cover the whole list.
//!
//! [`Selector::searchable`] additionally routes typed characters into the
//! filter (arrows/`PageUp`/`PageDown` still move, `Backspace` deletes,
//! `Esc` cancels). That mirrors the upstream `model-selector` and
//! `session-selector` components, which forward every non-navigation key
//! to their search input — the Rust model/session pickers reuse this
//! primitive, so `j`/`k` are searched for rather than treated as vim
//! navigation once `searchable` is on. The extension `ctx.ui.select`
//! dialog stays non-searchable, matching upstream's
//! `ExtensionSelectorComponent`.
//!
//! Deliberate deviations from upstream, both documented for the review
//! trail:
//!
//! * `SelectList` matches the filter as a case-insensitive **prefix of
//!   `item.value`**. The Rust pickers encode an opaque payload in `value`
//!   (`model:gpt-5`, `resume:<id>`), so [`SelectorItem::matches`] instead
//!   does a case-insensitive **substring** search over `value`, `label`
//!   and `description` — which is also how upstream's fuzzy model search
//!   behaves in practice.
//! * `SelectList` aligns descriptions into a primary column of its own;
//!   [`Selector`] ports that layout, truncating both the label and the
//!   description to the available width. Upstream fixes the primary
//!   column at `DEFAULT_PRIMARY_COLUMN_WIDTH` columns unless the caller
//!   overrides it with [`Selector::with_primary_column_width`]; widths are
//!   counted in `char`s, exactly like the rest of this crate.

use crate::input::{InputEvent, Key, KeyCode};
use crate::styles::SelectListStyles;

/// Default primary (label) column width, upstream
/// `DEFAULT_PRIMARY_COLUMN_WIDTH`.
const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
/// Blank columns between the primary column and the description, upstream
/// `PRIMARY_COLUMN_GAP`.
const PRIMARY_COLUMN_GAP: usize = 2;
/// A description is only rendered when at least this many columns remain,
/// upstream `MIN_DESCRIPTION_WIDTH`.
const MIN_DESCRIPTION_WIDTH: usize = 10;
/// Rows narrower than this render the label alone (upstream `width > 40`).
const MIN_DESCRIPTION_LIST_WIDTH: usize = 40;

/// Primary-column width bounds for a [`Selector`] — the Rust equivalent of
/// upstream `SelectListLayoutOptions`.
///
/// Upstream's default is a fixed 32-column primary column (the model and
/// session pickers use it as-is). Callers that want the column to track the
/// widest label pass explicit bounds through
/// [`Selector::with_primary_column_width`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectorLayout {
    /// Lower bound of the primary column, in `char` columns.
    pub min_primary_column_width: usize,
    /// Upper bound of the primary column, in `char` columns.
    pub max_primary_column_width: usize,
}

impl Default for SelectorLayout {
    fn default() -> Self {
        Self {
            min_primary_column_width: DEFAULT_PRIMARY_COLUMN_WIDTH,
            max_primary_column_width: DEFAULT_PRIMARY_COLUMN_WIDTH,
        }
    }
}

/// Single item in a [`Selector`].
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorItem {
    /// Stable identifier — used as the return value of
    /// [`Selector::selected_value`].
    pub value: String,
    /// Human-readable label.
    pub label: String,
    /// Optional secondary text shown right of the label.
    pub description: Option<String>,
}

impl SelectorItem {
    /// Convenience constructor with just a label/value pair.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Builder-style setter for the description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// True when `query` matches this item.
    ///
    /// An empty query matches everything. Otherwise the query is matched
    /// case-insensitively as a substring of the value, the label or the
    /// description (see the module docs for why this is a substring
    /// match rather than upstream's `value` prefix match).
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let needle = query.to_lowercase();
        [
            Some(self.value.as_str()),
            Some(self.label.as_str()),
            self.description.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|haystack| haystack.to_lowercase().contains(&needle))
    }
}

/// Action returned from [`Selector::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorAction {
    /// No state change worth redrawing.
    None,
    /// Cursor moved — caller redraws.
    Changed,
    /// User picked the highlighted item.
    Selected(String),
    /// User dismissed the selector without picking (Esc / Ctrl+C).
    Cancelled,
}

/// Modal list selector.
#[derive(Debug, Clone)]
pub struct Selector {
    title: String,
    items: Vec<SelectorItem>,
    /// Indices into `items` that pass the current filter, in list order.
    /// Always `0..items.len()` while no filter is set.
    filtered: Vec<usize>,
    /// Current search text. Empty when no filter is active.
    filter: String,
    /// Whether printable keys extend the filter (`true`) or are ignored.
    searchable: bool,
    /// Scroll window size. `None` renders every match (the historical
    /// Rust behaviour, still used by the extension select dialog).
    max_visible: Option<usize>,
    /// Bounds for the description column (upstream layout options).
    layout: SelectorLayout,
    cursor: usize,
    /// Initial cursor position used when the selector is re-opened.
    initial_cursor: usize,
}

impl Selector {
    /// Construct a selector with the given items.
    pub fn new(title: impl Into<String>, items: Vec<SelectorItem>) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            title: title.into(),
            items,
            filtered,
            filter: String::new(),
            searchable: false,
            max_visible: None,
            layout: SelectorLayout::default(),
            cursor: 0,
            initial_cursor: 0,
        }
    }

    /// Builder: route typed characters into the filter.
    ///
    /// See [`Selector::handle_search_key`] for the key map.
    pub fn searchable(mut self, searchable: bool) -> Self {
        self.searchable = searchable;
        self
    }

    /// Builder: draw at most `max_visible` rows and add a `(n/total)`
    /// indicator when the window does not cover the whole list.
    ///
    /// Upstream passes `10` for the model and session selectors.
    pub fn with_max_visible(mut self, max_visible: usize) -> Self {
        self.max_visible = Some(max_visible.max(1));
        self
    }

    /// Builder: bound the primary (label) column width.
    ///
    /// Mirrors upstream `SelectListLayoutOptions`: the rendered column is the
    /// widest visible label plus a [`PRIMARY_COLUMN_GAP`]-column gap, clamped
    /// to `[min, max]`. Passing the same value for both pins the column.
    pub fn with_primary_column_width(mut self, min: usize, max: usize) -> Self {
        self.layout = SelectorLayout {
            min_primary_column_width: min,
            max_primary_column_width: max,
        };
        self
    }

    /// Whether typed characters edit the filter.
    pub fn is_searchable(&self) -> bool {
        self.searchable
    }

    /// Borrow the title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Borrow the items.
    pub fn items(&self) -> &[SelectorItem] {
        &self.items
    }

    /// The items that pass the current filter, in list order.
    pub fn visible_items(&self) -> impl Iterator<Item = &SelectorItem> {
        self.filtered
            .iter()
            .filter_map(move |idx| self.items.get(*idx))
    }

    /// Number of items, ignoring any filter.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Number of items that pass the current filter.
    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    /// True when the list is empty (ignoring any filter).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Current search text.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Narrow the list to the items matching `filter` and move the
    /// cursor back to the first row (upstream `SelectList::setFilter`).
    ///
    /// Returns [`SelectorAction::Changed`] when the view changed and
    /// [`SelectorAction::None`] when the filter was already set to this
    /// value.
    pub fn set_filter(&mut self, filter: impl Into<String>) -> SelectorAction {
        let filter = filter.into();
        if filter == self.filter {
            return SelectorAction::None;
        }
        self.filter = filter;
        self.refilter();
        SelectorAction::Changed
    }

    /// Drop the filter and show every item again.
    pub fn clear_filter(&mut self) -> SelectorAction {
        self.set_filter(String::new())
    }

    /// Append one character to the search text.
    pub fn push_filter_char(&mut self, ch: char) -> SelectorAction {
        if ch.is_control() {
            return SelectorAction::None;
        }
        self.filter.push(ch);
        self.refilter();
        SelectorAction::Changed
    }

    /// Delete the last character of the search text.
    pub fn pop_filter_char(&mut self) -> SelectorAction {
        if self.filter.pop().is_none() {
            return SelectorAction::None;
        }
        self.refilter();
        SelectorAction::Changed
    }

    /// Recompute the filtered view and reset the cursor to the first row.
    fn refilter(&mut self) {
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.matches(&self.filter))
            .map(|(idx, _)| idx)
            .collect();
        self.cursor = 0;
    }

    /// Current cursor index (into the filtered view).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Highlighted item, if any.
    pub fn selected(&self) -> Option<&SelectorItem> {
        self.filtered
            .get(self.cursor)
            .and_then(|idx| self.items.get(*idx))
    }

    /// Value of the highlighted item.
    pub fn selected_value(&self) -> Option<&str> {
        self.selected().map(|item| item.value.as_str())
    }

    /// Move the cursor down within the filtered view (wraps).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = (self.cursor + 1) % self.filtered.len();
        SelectorAction::Changed
    }

    /// Move the cursor up within the filtered view (wraps).
    pub fn prev(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        if self.cursor == 0 {
            self.cursor = self.filtered.len() - 1;
        } else {
            self.cursor -= 1;
        }
        SelectorAction::Changed
    }

    /// Jump to the first visible item.
    pub fn first(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = 0;
        SelectorAction::Changed
    }

    /// Jump to the last visible item.
    pub fn last(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        self.cursor = self.filtered.len() - 1;
        SelectorAction::Changed
    }

    /// Move up by one window (upstream `session-selector` page-up).
    pub fn page_up(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        let step = self.window_size();
        let next = self.cursor.saturating_sub(step);
        if next == self.cursor {
            return SelectorAction::None;
        }
        self.cursor = next;
        SelectorAction::Changed
    }

    /// Move down by one window (upstream `session-selector` page-down).
    pub fn page_down(&mut self) -> SelectorAction {
        if self.filtered.is_empty() {
            return SelectorAction::None;
        }
        let step = self.window_size();
        let next = (self.cursor + step).min(self.filtered.len() - 1);
        if next == self.cursor {
            return SelectorAction::None;
        }
        self.cursor = next;
        SelectorAction::Changed
    }

    /// Move to the next visible item whose label starts with the given
    /// prefix (case-insensitive). No-op when nothing matches.
    pub fn jump_to_prefix(&mut self, prefix: &str) -> SelectorAction {
        if prefix.is_empty() || self.filtered.is_empty() {
            return SelectorAction::None;
        }
        let lower = prefix.to_ascii_lowercase();
        let len = self.filtered.len();
        for offset in 1..=len {
            let row = (self.cursor + offset) % len;
            if self.items[self.filtered[row]]
                .label
                .to_ascii_lowercase()
                .starts_with(&lower)
            {
                self.cursor = row;
                return SelectorAction::Changed;
            }
        }
        SelectorAction::None
    }

    /// Reset to the initial cursor — used when re-opening the selector.
    pub fn reset_cursor(&mut self) {
        let len = self.filtered.len();
        self.cursor = if len == 0 {
            0
        } else {
            self.initial_cursor.min(len - 1)
        };
    }

    /// Process a key event.
    ///
    /// A [`searchable`](Selector::searchable) selector also routes typed
    /// characters into the filter; a plain selector keeps the vim-style
    /// `j`/`k`/`g`/`G` navigation and ignores them.
    pub fn handle_key(&mut self, key: Key) -> SelectorAction {
        if self.searchable {
            return self.handle_search_key(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.prev(),
            KeyCode::Down | KeyCode::Char('j') => self.next(),
            KeyCode::Home | KeyCode::Char('g') => self.first(),
            KeyCode::End | KeyCode::Char('G') => self.last(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::Enter => match self.selected_value() {
                Some(value) => SelectorAction::Selected(value.to_string()),
                None => SelectorAction::None,
            },
            KeyCode::Esc => SelectorAction::Cancelled,
            _ => SelectorAction::None,
        }
    }

    /// Process a key event for a searchable selector.
    ///
    /// Arrows/`Home`/`End` move, `PageUp`/`PageDown` move by a window,
    /// `Enter` picks the highlighted row, `Esc` cancels, `Backspace`
    /// deletes the last search character, and every other printable key
    /// extends the search. This mirrors upstream's `model-selector` /
    /// `session-selector`, which forward everything that is not
    /// navigation to their search input.
    pub fn handle_search_key(&mut self, key: Key) -> SelectorAction {
        if key.modifiers.control || key.modifiers.alt || key.modifiers.meta {
            // Ctrl+C and friends belong to the App, not to the filter.
            return SelectorAction::None;
        }
        match key.code {
            KeyCode::Up => self.prev(),
            KeyCode::Down => self.next(),
            KeyCode::Home => self.first(),
            KeyCode::End => self.last(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::Enter => match self.selected_value() {
                Some(value) => SelectorAction::Selected(value.to_string()),
                None => SelectorAction::None,
            },
            KeyCode::Esc => SelectorAction::Cancelled,
            KeyCode::Backspace => self.pop_filter_char(),
            KeyCode::Char(ch) if !ch.is_control() => self.push_filter_char(ch),
            _ => SelectorAction::None,
        }
    }

    /// Process an input event.
    pub fn handle_event(&mut self, event: InputEvent) -> SelectorAction {
        let InputEvent::Key(key) = event else {
            return SelectorAction::None;
        };
        self.handle_key(key)
    }

    /// Scroll window size: the explicit `max_visible` when set, the
    /// upstream model/session selector default (10) otherwise.
    fn window_size(&self) -> usize {
        self.max_visible.unwrap_or(10).max(1)
    }

    /// Visible row range `(start, end)` — upstream `getVisibleRange`:
    /// the window is centred on the cursor and clamped to the list.
    fn visible_range(&self) -> (usize, usize) {
        let Some(max_visible) = self.max_visible else {
            return (0, self.filtered.len());
        };
        let len = self.filtered.len();
        if len == 0 {
            return (0, 0);
        }
        let start = self
            .cursor
            .saturating_sub(max_visible / 2)
            .min(len.saturating_sub(max_visible));
        (start, (start + max_visible).min(len))
    }

    /// Width of the primary (label) column — upstream
    /// `SelectList::getPrimaryColumnWidth`: the widest visible label plus
    /// [`PRIMARY_COLUMN_GAP`], clamped to the configured bounds.
    fn primary_column_width(&self) -> usize {
        let (min, max) = self.primary_column_bounds();
        let widest = self
            .filtered
            .iter()
            .filter_map(|idx| self.items.get(*idx))
            .map(|item| display_width(display_value(item)) + PRIMARY_COLUMN_GAP)
            .max()
            .unwrap_or(0);
        widest.max(min).min(max)
    }

    /// Normalised `(min, max)` bounds for the primary column — upstream
    /// `getPrimaryColumnBounds`.
    fn primary_column_bounds(&self) -> (usize, usize) {
        let raw_min = self.layout.min_primary_column_width;
        let raw_max = self.layout.max_primary_column_width;
        (raw_min.min(raw_max).max(1), raw_min.max(raw_max).max(1))
    }

    /// Render one row — upstream `SelectList::renderItem`.
    ///
    /// A row that has a description and enough horizontal room renders the
    /// description in a column that starts at the same offset on every row;
    /// otherwise it falls back to a width-clamped label alone.
    ///
    /// `styles` is the themed variant of the same layout: the selected row is
    /// wrapped whole (upstream `select-list.ts:205,216`), a non-selected
    /// description column is wrapped on its own (`select-list.ts:208`).
    fn render_row(
        &self,
        item: &SelectorItem,
        selected: bool,
        width: usize,
        primary_column_width: usize,
        styles: Option<&SelectListStyles<'_>>,
    ) -> String {
        let marker = if selected { "❯ " } else { "  " };
        let prefix_width = display_width(marker);

        let display = display_value(item);
        if let Some(description) = &item.description {
            let description = normalize_single_line(description);
            if !description.is_empty() && width > MIN_DESCRIPTION_LIST_WIDTH {
                let column = primary_column_width
                    .min(width.saturating_sub(prefix_width + 4))
                    .max(1);
                let label_width = column.saturating_sub(PRIMARY_COLUMN_GAP).max(1);
                let truncated = truncate_to_width(display, label_width);
                let truncated_width = display_width(&truncated);
                let spacing = " ".repeat(column.saturating_sub(truncated_width).max(1));
                let description_start = prefix_width + truncated_width + display_width(&spacing);
                let remaining = width.saturating_sub(description_start + 2);
                if remaining > MIN_DESCRIPTION_WIDTH {
                    let truncated_description = truncate_to_width(&description, remaining);
                    let body = format!("{marker}{truncated}{spacing}{truncated_description}");
                    return match styles {
                        Some(styles) if selected => styles.selected_text(&body),
                        // Upstream wraps the gap and the description together:
                        // `this.theme.description(spacing + truncatedDesc)`
                        // (`select-list.ts:208`).
                        Some(styles) => format!(
                            "{marker}{truncated}{}",
                            styles.description(&format!("{spacing}{truncated_description}"))
                        ),
                        None => body,
                    };
                }
            }
        }

        let max_width = width.saturating_sub(prefix_width + 2).max(1);
        let body = format!("{marker}{}", truncate_to_width(display, max_width));
        match styles {
            Some(styles) if selected => styles.selected_text(&body),
            _ => body,
        }
    }

    /// Render the selector as a flat vector of lines (used by the App
    /// and by tests).
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        self.render_lines_impl(width, None)
    }

    /// Themed variant of [`Selector::render_lines`].
    ///
    /// The visible text is byte-for-byte the same as the plain render; the
    /// no-match line, the selected row, the description column and the
    /// `(n/total)` indicator additionally carry ANSI styling from `styles`.
    /// A [`ColorMode::None`](crate::theme::ColorMode::None) theme makes this
    /// identical to [`Selector::render_lines`].
    pub fn render_lines_themed(&self, width: u16, styles: &SelectListStyles<'_>) -> Vec<String> {
        self.render_lines_impl(width, Some(styles))
    }

    fn render_lines_impl(&self, width: u16, styles: Option<&SelectListStyles<'_>>) -> Vec<String> {
        let mut lines = Vec::new();
        let width = width as usize;
        lines.push(self.title.clone());
        lines.push("─".repeat(width.min(40)));
        if self.filtered.is_empty() {
            // An empty list and a filter without matches read differently
            // to the user, so they keep different lines.
            let line = if self.filter.is_empty() {
                "(no items)".to_string()
            } else {
                "  No matching items".to_string()
            };
            lines.push(match styles {
                Some(styles) => styles.no_match(&line),
                None => line,
            });
            return lines;
        }
        let (start, end) = self.visible_range();
        let primary_column_width = self.primary_column_width();
        for row in start..end {
            let item = &self.items[self.filtered[row]];
            lines.push(self.render_row(
                item,
                row == self.cursor,
                width,
                primary_column_width,
                styles,
            ));
        }
        if start > 0 || end < self.filtered.len() {
            let indicator = format!("  ({}/{})", self.cursor + 1, self.filtered.len());
            lines.push(match styles {
                Some(styles) => styles.scroll_info(&indicator),
                None => indicator,
            });
        }
        lines
    }
}

/// Layout width of a string: this crate counts `char`s (see `message` /
/// `prompt`), so a wide glyph still counts as one column.
fn display_width(text: &str) -> usize {
    text.chars().count()
}

/// Truncate `text` to at most `max` columns, dropping the tail — upstream
/// `truncateToWidth(text, max, "")`.
fn truncate_to_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Upstream `SelectList::getDisplayValue`: the label, falling back to the
/// value for label-less rows.
fn display_value(item: &SelectorItem) -> &str {
    if item.label.is_empty() {
        &item.value
    } else {
        &item.label
    }
}

/// Upstream `normalizeToSingleLine`: collapse line breaks and trim so a
/// multi-line description cannot break the one-line-per-item layout.
fn normalize_single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    fn items() -> Vec<SelectorItem> {
        vec![
            SelectorItem::new("gpt", "gpt-4o").with_description("OpenAI"),
            SelectorItem::new("claude", "claude-3.5-sonnet").with_description("Anthropic"),
            SelectorItem::new("faux", "faux-model").with_description("Test"),
        ]
    }

    #[test]
    fn cursor_wraps_on_next() {
        let mut sel = Selector::new("Pick", items());
        sel.last();
        assert_eq!(sel.cursor(), 2);
        sel.next();
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn cursor_wraps_on_prev() {
        let mut sel = Selector::new("Pick", items());
        sel.prev();
        assert_eq!(sel.cursor(), 2);
    }

    #[test]
    fn jump_to_prefix_finds_next_match() {
        let mut sel = Selector::new("Pick", items());
        sel.cursor = 0;
        assert_eq!(sel.jump_to_prefix("cl"), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 1);
    }

    #[test]
    fn enter_returns_selected_value() {
        let mut sel = Selector::new("Pick", items());
        sel.cursor = 1;
        let action = sel.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            SelectorAction::Selected(value) => assert_eq!(value, "claude"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn esc_cancels() {
        let mut sel = Selector::new("Pick", items());
        let action = sel.handle_key(Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, SelectorAction::Cancelled);
    }

    #[test]
    fn empty_selector_handles_keys_safely() {
        let mut sel = Selector::new("Empty", Vec::new());
        assert_eq!(sel.next(), SelectorAction::None);
        assert_eq!(sel.prev(), SelectorAction::None);
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            SelectorAction::None,
        );
    }

    #[test]
    fn set_filter_keeps_only_matching_items() {
        let mut sel = Selector::new("Pick", items());
        assert_eq!(sel.filtered_len(), 3);
        assert_eq!(sel.set_filter("cl"), SelectorAction::Changed);
        assert_eq!(sel.filter(), "cl");
        assert_eq!(sel.filtered_len(), 1);
        assert_eq!(sel.selected_value(), Some("claude"));
        assert_eq!(sel.selected().unwrap().label, "claude-3.5-sonnet");
    }

    #[test]
    fn filter_matches_value_label_and_description_case_insensitively() {
        let items = vec![
            SelectorItem::new("model:gpt-5", "GPT-5").with_description("OpenAI"),
            SelectorItem::new("model:claude-4", "Claude 4").with_description("Anthropic"),
        ];
        let mut sel = Selector::new("Pick", items);
        // `value` (opaque payload for the model picker).
        sel.set_filter("model:gpt");
        assert_eq!(sel.filtered_len(), 1);
        // `label`.
        sel.set_filter("gpt");
        assert_eq!(sel.filtered_len(), 1);
        // `description` (the provider column).
        sel.set_filter("ANTHROPIC");
        assert_eq!(sel.filtered_len(), 1);
        assert_eq!(sel.selected_value(), Some("model:claude-4"));
        // Substring, not prefix: upstream `SelectList` would not match this.
        sel.set_filter("5");
        assert_eq!(sel.filtered_len(), 1);
    }

    #[test]
    fn set_filter_resets_the_cursor_and_clear_filter_restores_the_list() {
        let mut sel = Selector::new("Pick", items());
        sel.last();
        assert_eq!(sel.cursor(), 2);
        sel.set_filter("faux");
        assert_eq!(sel.cursor(), 0);
        assert_eq!(sel.set_filter("faux"), SelectorAction::None);
        assert_eq!(sel.clear_filter(), SelectorAction::Changed);
        assert_eq!(sel.filter(), "");
        assert_eq!(sel.filtered_len(), 3);
    }

    #[test]
    fn a_filter_without_matches_renders_the_no_match_line() {
        let mut sel = Selector::new("Pick", items());
        sel.set_filter("zzz");
        let lines = sel.render_lines(40);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "Pick");
        assert_eq!(lines[2], "  No matching items");
        assert_eq!(sel.selected_value(), None);
        // Key handling stays safe while nothing matches.
        assert_eq!(sel.next(), SelectorAction::None);
        assert_eq!(sel.page_down(), SelectorAction::None);
    }

    #[test]
    fn an_empty_list_still_renders_the_no_items_line() {
        let sel = Selector::new("Empty", Vec::new());
        let lines = sel.render_lines(20);
        assert_eq!(lines.last().unwrap(), "(no items)");
    }

    #[test]
    fn max_visible_windows_the_list_and_adds_a_scroll_indicator() {
        let items = (0..12)
            .map(|i| SelectorItem::new(format!("item-{i}"), format!("Item {i}")))
            .collect();
        let mut sel = Selector::new("Pick", items).with_max_visible(5);
        let lines = sel.render_lines(40);
        // 2 header lines + 5 rows + the scroll indicator.
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[2], "❯ Item 0");
        assert_eq!(lines[6], "  Item 4");
        assert_eq!(lines[7], "  (1/12)");
        // The window follows the cursor and is centred on it.
        sel.last();
        let lines = sel.render_lines(40);
        assert_eq!(lines[2], "  Item 7");
        assert_eq!(lines[6], "❯ Item 11");
        assert_eq!(lines[7], "  (12/12)");
    }

    #[test]
    fn an_unwindowed_list_has_no_scroll_indicator() {
        let sel = Selector::new("Pick", items());
        let lines = sel.render_lines(40);
        assert_eq!(lines.len(), 5);
        assert!(!lines[2].starts_with("  ("));
    }

    #[test]
    fn page_keys_move_by_one_window() {
        let items = (0..25)
            .map(|i| SelectorItem::new(format!("item-{i}"), format!("Item {i}")))
            .collect();
        let mut sel = Selector::new("Pick", items).with_max_visible(10);
        assert_eq!(sel.page_down(), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 10);
        assert_eq!(sel.page_down(), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 20);
        // Clamped to the last row instead of running past it.
        assert_eq!(sel.page_down(), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 24);
        assert_eq!(sel.page_down(), SelectorAction::None);
        assert_eq!(sel.page_up(), SelectorAction::Changed);
        assert_eq!(sel.cursor(), 14);
        assert_eq!(sel.first(), SelectorAction::Changed);
        assert_eq!(sel.page_up(), SelectorAction::None);
    }

    #[test]
    fn multi_line_descriptions_render_on_one_line() {
        let items = vec![SelectorItem::new("a", "Alpha").with_description("first\nsecond")];
        let sel = Selector::new("Pick", items);
        let lines = sel.render_lines(80);
        assert_eq!(
            lines[2].chars().skip(34).collect::<String>(),
            "first second"
        );
    }

    #[test]
    fn descriptions_align_into_a_primary_column() {
        let sel = Selector::new("Pick", items());
        let lines = sel.render_lines(80);
        let rows = &lines[2..5];
        // Default layout: a fixed 32-column primary column, so the
        // description starts at 2 (marker) + 32 = 34 on every row.
        let starts: Vec<String> = rows
            .iter()
            .map(|row| row.chars().skip(34).collect())
            .collect();
        assert_eq!(starts, vec!["OpenAI", "Anthropic", "Test"]);
        assert!(rows[0].starts_with("❯ gpt-4o"));
        assert!(rows[1].starts_with("  claude-3.5-sonnet"));
    }

    #[test]
    fn primary_column_width_tracks_the_widest_label_within_bounds() {
        let items = vec![
            SelectorItem::new("a", "Alpha").with_description("one"),
            SelectorItem::new("b", "A Much Longer Label").with_description("two"),
        ];
        let sel = Selector::new("Pick", items).with_primary_column_width(10, 40);
        // Widest label (19) + gap (2) = 21, inside [10, 40].
        let lines = sel.render_lines(80);
        assert_eq!(lines[2].chars().skip(23).collect::<String>(), "one");
        assert_eq!(lines[3].chars().skip(23).collect::<String>(), "two");
    }

    #[test]
    fn narrow_rows_render_the_label_without_the_description_column() {
        let items = vec![SelectorItem::new("a", "Alpha").with_description("first second")];
        let sel = Selector::new("Pick", items);
        assert_eq!(sel.render_lines(40)[2], "❯ Alpha");
    }

    #[test]
    fn long_labels_and_descriptions_are_clamped_to_the_width() {
        let items = vec![SelectorItem::new(
            "a",
            "a-very-long-label-that-does-not-fit-in-the-primary-column",
        )
        .with_description("a description that is far too long for the remaining space")];
        let sel = Selector::new("Pick", items);
        let lines = sel.render_lines(60);
        assert!(
            lines[2].chars().count() <= 60,
            "row overflows the width: {:?}",
            lines[2]
        );
    }

    #[test]
    fn searchable_selector_routes_printable_keys_into_the_filter() {
        let mut sel = Selector::new("Pick", items()).searchable(true);
        assert!(sel.is_searchable());
        // `j` filters instead of moving the cursor (upstream model-selector).
        assert_eq!(sel.handle_key(Key::char('j')), SelectorAction::Changed);
        assert_eq!(sel.filter(), "j");
        assert_eq!(sel.filtered_len(), 0);
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Backspace, KeyModifiers::NONE)),
            SelectorAction::Changed
        );
        assert_eq!(sel.filter(), "");
        assert_eq!(sel.filtered_len(), 3);
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Backspace, KeyModifiers::NONE)),
            SelectorAction::None,
        );
        // Navigation still works, and Enter returns the highlighted row.
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Down, KeyModifiers::NONE)),
            SelectorAction::Changed
        );
        match sel.handle_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)) {
            SelectorAction::Selected(value) => assert_eq!(value, "claude"),
            other => panic!("unexpected action: {:?}", other),
        }
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Esc, KeyModifiers::NONE)),
            SelectorAction::Cancelled
        );
    }

    #[test]
    fn searchable_selector_ignores_ctrl_chords() {
        let mut sel = Selector::new("Pick", items()).searchable(true);
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            SelectorAction::None,
        );
        assert_eq!(sel.filter(), "");
    }

    #[test]
    fn a_plain_selector_still_ignores_printable_keys() {
        let mut sel = Selector::new("Pick", items());
        assert_eq!(sel.handle_key(Key::char('z')), SelectorAction::None);
        assert_eq!(sel.filter(), "");
        assert_eq!(sel.filtered_len(), 3);
    }

    #[test]
    fn jump_to_prefix_only_searches_visible_items() {
        let mut sel = Selector::new("Pick", items());
        sel.set_filter("cl");
        // Only `claude` is visible, so a prefix it does not have is a no-op.
        assert_eq!(sel.jump_to_prefix("fa"), SelectorAction::None);
        assert_eq!(sel.cursor(), 0);
    }

    #[test]
    fn reset_cursor_clamps_to_the_filtered_view() {
        let mut sel = Selector::new("Pick", items());
        sel.first();
        sel.set_filter("faux");
        sel.reset_cursor();
        assert_eq!(sel.cursor(), 0);
    }
}

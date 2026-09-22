//! `SettingsList` — the Rust port of
//! `packages/tui/src/components/settings-list.ts`.
//!
//! A settings list is a small, searchable, two-column list: a label on the
//! left and the current value on the right. `Enter` / `Space` cycles an
//! item through its `values`; items without `values` report
//! [`SettingsAction::Activated`] so the driver can open a submenu of its own.
//!
//! The component owns no terminal and no persistence: it renders lines and
//! reports what the user did, exactly like [`Selector`](crate::selector).
//! The `pi-coding-agent` `/settings` command builds the items from
//! `settings.json`, applies the [`SettingsAction`] and writes the file back.
//!
//! # Deliberate differences from upstream
//!
//! * **No `submenu` callback field.** Upstream stores a component factory on
//!   the item. Rust items are plain data; an item without values yields
//!   [`SettingsAction::Activated`] and the driver decides what to open.
//! * **No mouse hit-testing.** Upstream maps a click row back to an item and
//!   supports press/click/hover. The Rust [`App`](crate::App) has no
//!   component-level mouse regions yet, so only the wheel is wired
//!   ([`SettingsList::scroll_by`]); clicks stay an explicit follow-up.
//! * **Search line is plain text.** Upstream embeds a full `Input` component
//!   (cursor, editing keys). Here the filter is edited with the printable
//!   keys and `Backspace`, and rendered as `> <filter>`.

use crate::fuzzy::fuzzy_rank;
use crate::input::{Key, KeyCode};
use crate::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::theme::{ColorMode, Theme, ThemeColor};
use crate::width::{columns, truncate_columns};

/// Maximum label column width, upstream
/// `Math.min(36, ...)` (`settings-list.ts:137`).
const MAX_LABEL_WIDTH: usize = 36;

/// One row in a [`SettingsList`].
///
/// Mirrors upstream's `SettingItem` minus the `submenu` factory (see the
/// module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingItem {
    /// Unique identifier — the key the driver uses to persist a change.
    pub id: String,
    /// Left-hand display label.
    pub label: String,
    /// Help text shown under the list while this item is selected.
    pub description: Option<String>,
    /// Right-hand value shown for the item.
    pub current_value: String,
    /// Values `Enter` / `Space` cycles through, in order. Empty means the
    /// item is not editable in place.
    pub values: Vec<String>,
}

impl SettingItem {
    /// A value-less item (the driver must handle its activation).
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            current_value: String::new(),
            values: Vec::new(),
        }
    }

    /// Attach the help text shown while the item is selected.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Make the item cycle through `values`, starting at `current`.
    ///
    /// When `current` is not one of `values`, activation starts at the
    /// first value (upstream `values.indexOf(current) === -1` → `next = 0`
    /// after the modulo), so a value written by hand still cycles.
    pub fn with_values<V, S>(mut self, values: V, current: impl Into<String>) -> Self
    where
        V: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.values = values.into_iter().map(Into::into).collect();
        self.current_value = current.into();
        self
    }
}

/// What a key did to the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    /// Nothing changed (an unhandled key, or a no-op move).
    None,
    /// Only the view changed — the cursor moved or the filter was edited.
    /// Nothing needs persisting.
    Changed,
    /// A value was committed. Upstream fires its `onChange(id, newValue)`
    /// callback here, and only here; the driver persists it.
    ValueChanged {
        /// The item that changed.
        id: String,
        /// Its new value.
        value: String,
    },
    /// An item without `values` was confirmed; the driver owns the next
    /// screen (upstream opens the item's `submenu`). Carries the item id.
    Activated(String),
    /// `Esc` — the caller should close the list.
    Cancelled,
}

/// A searchable two-column settings list.
#[derive(Debug, Clone)]
pub struct SettingsList {
    items: Vec<SettingItem>,
    /// Indices into `items`, fuzzy-ranked against `filter`. With an empty
    /// filter this is `0..items.len()` in input order.
    filtered: Vec<usize>,
    /// Cursor position into `filtered`.
    selected: usize,
    max_visible: usize,
    searchable: bool,
    filter: String,
}

impl SettingsList {
    /// Build a list over `items` that windows at `max_visible` rows.
    pub fn new(items: Vec<SettingItem>, max_visible: usize) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            items,
            filtered,
            selected: 0,
            max_visible: max_visible.max(1),
            searchable: false,
            filter: String::new(),
        }
    }

    /// Turn the fuzzy filter on (upstream `options.enableSearch`).
    pub fn searchable(mut self, searchable: bool) -> Self {
        self.searchable = searchable;
        self
    }

    /// Whether typed characters feed the filter.
    pub fn is_searchable(&self) -> bool {
        self.searchable
    }

    /// Every item, in declaration order (unaffected by the filter).
    pub fn items(&self) -> &[SettingItem] {
        &self.items
    }

    /// Mutable access to the items — used by the driver to refresh a value
    /// it persisted outside the list.
    pub fn items_mut(&mut self) -> &mut [SettingItem] {
        &mut self.items
    }

    /// Number of items, filtered or not.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when there are no items at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of rows matching the current filter.
    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    /// The current fuzzy filter.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Cursor position into the filtered rows.
    pub fn cursor(&self) -> usize {
        self.selected
    }

    /// The highlighted item, if any.
    pub fn selected(&self) -> Option<&SettingItem> {
        self.filtered
            .get(self.selected)
            .map(|idx| &self.items[*idx])
    }

    /// Look an item up by id (filter-independent).
    pub fn item(&self, id: &str) -> Option<&SettingItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Overwrite an item's value without emitting an action — upstream
    /// `updateValue`.
    ///
    /// Returns `false` when no item carries `id`.
    pub fn set_value(&mut self, id: &str, value: impl Into<String>) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.current_value = value.into();
                true
            }
            None => false,
        }
    }

    /// Move the cursor to `id` (upstream `selectItem`).
    ///
    /// Returns `false` when the item is missing or filtered out.
    pub fn select_item(&mut self, id: &str) -> bool {
        let Some(index) = self
            .filtered
            .iter()
            .position(|idx| self.items[*idx].id == id)
        else {
            return false;
        };
        self.selected = index;
        true
    }

    /// Visible row range `(start, end)` — upstream `getVisibleRange`: the
    /// window is centred on the cursor and clamped to the list.
    pub fn visible_range(&self) -> (usize, usize) {
        let len = self.filtered.len();
        if len == 0 {
            return (0, 0);
        }
        let half = self.max_visible / 2;
        let start = self
            .selected
            .saturating_sub(half)
            .min(len.saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(len);
        (start, end)
    }

    /// Replace the filter, re-rank, and reset the cursor (upstream
    /// `applyFilter`).
    pub fn set_filter(&mut self, filter: impl Into<String>) -> SettingsAction {
        let filter = filter.into();
        if filter == self.filter {
            return SettingsAction::None;
        }
        self.filter = filter;
        self.filtered = fuzzy_rank(&self.items, &self.filter, |item| item.label.clone());
        self.selected = 0;
        SettingsAction::Changed
    }

    /// Move the cursor up one row, wrapping at the top.
    pub fn prev(&mut self) -> SettingsAction {
        if self.filtered.is_empty() {
            return SettingsAction::None;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
        SettingsAction::Changed
    }

    /// Move the cursor down one row, wrapping at the bottom.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> SettingsAction {
        if self.filtered.is_empty() {
            return SettingsAction::None;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
        SettingsAction::Changed
    }

    /// Move the cursor by `delta` rows, clamped to the ends — the wheel
    /// path (upstream `handleMouse` wheel branch).
    pub fn scroll_by(&mut self, delta: i32) -> SettingsAction {
        if self.filtered.is_empty() || delta == 0 {
            return SettingsAction::None;
        }
        let len = self.filtered.len();
        let target = (self.selected as i64 + delta as i64).clamp(0, len as i64 - 1);
        if target as usize == self.selected {
            return SettingsAction::None;
        }
        self.selected = target as usize;
        SettingsAction::Changed
    }

    /// Confirm the highlighted row.
    ///
    /// An item with `values` advances to the next value (wrapping) and
    /// reports [`SettingsAction::ValueChanged`]; an item without values
    /// reports [`SettingsAction::Activated`] and leaves the list untouched.
    pub fn activate(&mut self) -> SettingsAction {
        let Some(item) = self.selected() else {
            return SettingsAction::None;
        };
        if item.values.is_empty() {
            return SettingsAction::Activated(item.id.clone());
        }
        let current = item.current_value.clone();
        let next = match item.values.iter().position(|value| *value == current) {
            Some(index) => (index + 1) % item.values.len(),
            None => 0,
        };
        let value = item.values[next].clone();
        let id = item.id.clone();
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = value.clone();
        }
        SettingsAction::ValueChanged { id, value }
    }

    /// Process one key event.
    pub fn handle_key(&mut self, key: Key) -> SettingsAction {
        let kb = crate::keybindings::get_keybindings();
        self.handle_key_with(&kb, key)
    }

    /// [`SettingsList::handle_key`] against an explicit table.
    ///
    /// Upstream `SettingsList::handleInput` reads all four list chords off the
    /// registry (`components/settings-list.ts:231-246`); the Rust list used to
    /// hardcode `Enter` / `Esc` / arrows, so a `keybindings.json` override was
    /// advertised by `/hotkeys` but never reached the list.
    pub fn handle_key_with(
        &mut self,
        kb: &crate::keybindings::KeybindingsManager,
        key: Key,
    ) -> SettingsAction {
        let event = crate::input::InputEvent::Key(key);
        // Upstream's order: up, down, confirm, cancel — before the modifier
        // guard, because `tui.select.cancel` ships as `escape` + `ctrl+c`.
        if kb.matches(&event, "tui.select.up") {
            return self.prev();
        }
        if kb.matches(&event, "tui.select.down") {
            return self.next();
        }
        if kb.matches(&event, "tui.select.confirm") {
            return self.activate();
        }
        if kb.matches(&event, "tui.select.cancel") {
            return SettingsAction::Cancelled;
        }
        if key.modifiers.control || key.modifiers.alt || key.modifiers.meta {
            // Every other modified chord belongs to the App, not to the list.
            return SettingsAction::None;
        }
        match key.code {
            KeyCode::Char('k') if !self.searchable => self.prev(),
            KeyCode::Char('j') if !self.searchable => self.next(),
            KeyCode::Home => self.set_cursor(0),
            KeyCode::End => self.set_cursor(self.filtered.len().saturating_sub(1)),
            // Space only activates while the search box is empty: once the
            // user is typing a query, a space is a query character
            // (upstream `data === " " && searchInput.getValue().length === 0`).
            KeyCode::Char(' ') if !self.searchable || self.filter.is_empty() => self.activate(),
            KeyCode::Backspace if self.searchable => {
                let mut filter = self.filter.clone();
                filter.pop();
                self.set_filter(filter)
            }
            KeyCode::Char(ch) if self.searchable && !ch.is_control() => {
                let mut filter = self.filter.clone();
                filter.push(ch);
                self.set_filter(filter)
            }
            _ => SettingsAction::None,
        }
    }

    /// Move the selection to the filtered row `index`, clamped to the last
    /// row. Public because the pointer press path maps a clicked row back
    /// onto a list row (upstream `settings-list.ts:199-205` sets
    /// `selectedIndex` from `event.y` before the click activates it).
    pub fn set_cursor(&mut self, index: usize) -> SettingsAction {
        if self.filtered.is_empty() || index == self.selected {
            return SettingsAction::None;
        }
        self.selected = index.min(self.filtered.len() - 1);
        SettingsAction::Changed
    }

    /// Render the list as plain lines (no ANSI escapes).
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        self.render_styled_lines(width)
            .iter()
            .map(|line| crate::styled::plain_text(line))
            .collect()
    }

    /// Render the list as plain lines wrapped in the `SettingsListTheme`
    /// slots upstream defines:
    ///
    /// | slot | theme colour | upstream |
    /// |------|--------------|----------|
    /// | cursor `→ ` | `accent` | `theme.ts:1231` |
    /// | selected label / value | `accent` | `theme.ts:1228-1229` |
    /// | unselected value | `muted` | `theme.ts:1229` |
    /// | description / hint | `dim` | `theme.ts:1230,1232` |
    pub fn render_lines_themed(&self, width: u16, theme: &Theme) -> Vec<String> {
        self.render_styled_lines(width)
            .iter()
            .map(|line| crate::styled::themed_text(line, theme))
            .collect()
    }

    /// Render the list as theme-slot spans, one line per row. The single
    /// layout implementation behind every other render method.
    pub fn render_styled_lines(&self, width: u16) -> Vec<StyledLine> {
        let width = width as usize;
        let mut lines: Vec<StyledLine> = Vec::new();

        if self.searchable {
            lines.push(vec![StyledSpan::new(
                format!("> {}", self.filter),
                SpanStyle::PLAIN,
            )]);
            lines.push(Vec::new());
        }

        if self.items.is_empty() {
            lines.push(vec![StyledSpan::new(
                "  No settings available",
                SpanStyle::fg(ThemeColor::Dim),
            )]);
            lines.extend(self.hint_lines(width));
            return lines;
        }

        if self.filtered.is_empty() {
            lines.push(vec![StyledSpan::new(
                "  No matching settings",
                SpanStyle::fg(ThemeColor::Dim),
            )]);
            lines.extend(self.hint_lines(width));
            return lines;
        }

        let label_width = self
            .items
            .iter()
            .map(|item| display_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_WIDTH);

        let (start, end) = self.visible_range();
        for row in start..end {
            let item = &self.items[self.filtered[row]];
            let selected = row == self.selected;
            let cursor = if selected { "→ " } else { "  " };
            let label = format!("{}{}  ", cursor, pad_right(&item.label, label_width));
            let used = display_width(&label);
            let value_max = width.saturating_sub(used + 2);
            let value = truncate_to_width(&item.current_value, value_max);
            let label_style = if selected {
                SpanStyle::fg(ThemeColor::Accent)
            } else {
                SpanStyle::PLAIN
            };
            let value_style = if selected {
                SpanStyle::fg(ThemeColor::Accent)
            } else {
                SpanStyle::fg(ThemeColor::Muted)
            };
            lines.push(vec![
                StyledSpan::new(label, label_style),
                StyledSpan::new(value, value_style),
            ]);
        }

        if start > 0 || end < self.filtered.len() {
            lines.push(vec![StyledSpan::new(
                format!("  ({}/{})", self.selected + 1, self.filtered.len()),
                SpanStyle::fg(ThemeColor::Muted),
            )]);
        }

        if let Some(description) = self.selected().and_then(|item| item.description.as_deref()) {
            lines.push(Vec::new());
            for wrapped in wrap_words(description, width.saturating_sub(4)) {
                lines.push(vec![StyledSpan::new(
                    format!("  {wrapped}"),
                    SpanStyle::fg(ThemeColor::Dim),
                )]);
            }
        }

        lines.extend(self.hint_lines(width));
        lines
    }

    fn hint_lines(&self, _width: usize) -> Vec<StyledLine> {
        let hint = if self.searchable {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        vec![
            Vec::new(),
            vec![StyledSpan::new(hint, SpanStyle::fg(ThemeColor::Dim))],
        ]
    }
}

/// Layout width of a string in terminal columns ([`crate::width`]): a CJK
/// ideograph is two columns, an emoji two, a combining mark none.
fn display_width(text: &str) -> usize {
    columns(text)
}

/// Pad `text` with trailing spaces to `width` columns, never truncating it.
fn pad_right(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    let len = display_width(text);
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

/// Truncate `text` to at most `max` columns, dropping the tail — upstream
/// `truncateToWidth(text, max, "")`.
fn truncate_to_width(text: &str, max: usize) -> String {
    truncate_columns(text, max).to_string()
}

/// Greedy word wrap at `width` columns, falling back to a hard break for a
/// word longer than the line.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if display_width(&current) + 1 + display_width(word) <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        }
        while display_width(&current) > width {
            let head = truncate_columns(&current, width).to_string();
            let consumed = head.chars().count();
            lines.push(head);
            current = current.chars().skip(consumed).collect();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Upstream `SettingsListTheme` resolution: a `ColorMode::None` theme
/// renders the exact plain text because every span style collapses to
/// nothing.
///
/// Kept private — callers assert on output, not on the mode.
#[allow(dead_code)]
fn is_plain_theme(theme: &Theme) -> bool {
    matches!(theme.color_mode(), ColorMode::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    fn items() -> Vec<SettingItem> {
        vec![
            SettingItem::new("autocompact", "Auto-compact")
                .with_description("Automatically compact context when it gets too large")
                .with_values(["true", "false"], "true"),
            SettingItem::new("theme", "Theme").with_values(["dark", "light"], "dark"),
            SettingItem::new("warnings", "Warnings")
                .with_description("Enable or disable individual warnings"),
        ]
    }

    fn key(code: KeyCode) -> Key {
        Key::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn activation_cycles_values_and_wraps() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(list.selected().unwrap().id, "autocompact");
        assert_eq!(
            list.activate(),
            SettingsAction::ValueChanged {
                id: "autocompact".to_string(),
                value: "false".to_string(),
            }
        );
        assert_eq!(list.item("autocompact").unwrap().current_value, "false");
        assert_eq!(
            list.activate(),
            SettingsAction::ValueChanged {
                id: "autocompact".to_string(),
                value: "true".to_string(),
            }
        );
        assert_eq!(list.item("autocompact").unwrap().current_value, "true");
    }

    #[test]
    fn moving_the_cursor_is_not_a_value_change() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(list.next(), SettingsAction::Changed);
        assert_eq!(list.prev(), SettingsAction::Changed);
        assert_eq!(list.scroll_by(1), SettingsAction::Changed);
        assert_eq!(list.set_filter("t"), SettingsAction::Changed);
    }

    #[test]
    fn activation_without_values_reports_the_id() {
        let mut list = SettingsList::new(items(), 10);
        list.select_item("warnings");
        assert_eq!(
            list.activate(),
            SettingsAction::Activated("warnings".to_string())
        );
        // The item is left untouched.
        assert_eq!(list.item("warnings").unwrap().current_value, "");
    }

    #[test]
    fn unknown_current_value_starts_at_the_first_entry() {
        let mut list = SettingsList::new(
            vec![SettingItem::new("x", "X").with_values(["a", "b"], "hand-written")],
            10,
        );
        assert_eq!(
            list.activate(),
            SettingsAction::ValueChanged {
                id: "x".to_string(),
                value: "a".to_string(),
            }
        );
        assert_eq!(list.item("x").unwrap().current_value, "a");
    }

    #[test]
    fn navigation_wraps_and_ignores_an_empty_list() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(list.prev(), SettingsAction::Changed);
        assert_eq!(list.selected().unwrap().id, "warnings");
        assert_eq!(list.next(), SettingsAction::Changed);
        assert_eq!(list.selected().unwrap().id, "autocompact");

        let mut empty = SettingsList::new(Vec::new(), 10);
        assert_eq!(empty.prev(), SettingsAction::None);
        assert_eq!(empty.next(), SettingsAction::None);
    }

    #[test]
    fn search_filters_by_label_and_resets_the_cursor() {
        let mut list = SettingsList::new(items(), 10).searchable(true);
        list.scroll_by(2);
        assert_eq!(list.cursor(), 2);
        assert_eq!(list.set_filter("the"), SettingsAction::Changed);
        assert_eq!(list.filtered_len(), 1);
        assert_eq!(list.selected().unwrap().id, "theme");
        assert_eq!(list.cursor(), 0);
        assert_eq!(list.set_filter("the"), SettingsAction::None);
    }

    #[test]
    fn searchable_list_routes_printable_keys_and_backspace() {
        let mut list = SettingsList::new(items(), 10).searchable(true);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('t'))),
            SettingsAction::Changed
        );
        assert_eq!(list.filter(), "t");
        assert_eq!(
            list.handle_key(key(KeyCode::Backspace)),
            SettingsAction::Changed
        );
        assert_eq!(list.filter(), "");
        // A space is a query character once the filter is non-empty.
        list.set_filter("the");
        assert_eq!(
            list.handle_key(key(KeyCode::Char('m'))),
            SettingsAction::Changed
        );
        assert_eq!(list.filter(), "them");
    }

    #[test]
    fn space_activates_only_on_an_empty_filter() {
        let mut list = SettingsList::new(items(), 10).searchable(true);
        assert_eq!(
            list.handle_key(key(KeyCode::Char(' '))),
            SettingsAction::ValueChanged {
                id: "autocompact".to_string(),
                value: "false".to_string(),
            }
        );
        assert_eq!(list.item("autocompact").unwrap().current_value, "false");
        list.set_filter("theme");
        list.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(list.filter(), "theme ");
    }

    #[test]
    fn a_plain_list_keeps_vim_keys_out_of_the_filter() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(
            list.handle_key(key(KeyCode::Char('j'))),
            SettingsAction::Changed
        );
        assert_eq!(list.selected().unwrap().id, "theme");
        assert_eq!(list.filter(), "");
    }

    #[test]
    fn non_list_ctrl_chords_are_left_to_the_app() {
        let mut list = SettingsList::new(items(), 10).searchable(true);
        // `tui.select.cancel` ships as `escape` + `ctrl+c` and upstream
        // `SettingsList` answers it itself (`settings-list.ts:244-246`); every
        // other modified chord still belongs to the app.
        assert_eq!(
            list.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            SettingsAction::Cancelled
        );
        assert_eq!(
            list.handle_key(Key::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            SettingsAction::None
        );
        assert_eq!(list.filter(), "");
    }

    #[test]
    fn esc_cancels() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(
            list.handle_key(key(KeyCode::Esc)),
            SettingsAction::Cancelled
        );
    }

    #[test]
    fn scroll_clamps_instead_of_wrapping() {
        let mut list = SettingsList::new(items(), 10);
        assert_eq!(list.scroll_by(1), SettingsAction::Changed);
        assert_eq!(list.cursor(), 1);
        assert_eq!(list.scroll_by(5), SettingsAction::Changed);
        assert_eq!(list.cursor(), 2);
        // Already at the end — no change, so the App does not redraw.
        assert_eq!(list.scroll_by(1), SettingsAction::None);
        assert_eq!(list.scroll_by(-9), SettingsAction::Changed);
        assert_eq!(list.cursor(), 0);
    }

    #[test]
    fn visible_range_centres_on_the_cursor() {
        let list = SettingsList::new(
            (0..20)
                .map(|i| SettingItem::new(format!("i{i}"), format!("Item {i}")))
                .collect(),
            5,
        );
        assert_eq!(list.visible_range(), (0, 5));
        let mut list = list;
        list.scroll_by(10);
        assert_eq!(list.visible_range(), (8, 13));
        list.scroll_by(10);
        assert_eq!(list.visible_range(), (15, 20));
    }

    #[test]
    fn render_aligns_labels_and_shows_the_value() {
        let list = SettingsList::new(items(), 10);
        let lines = list.render_lines(60);
        assert_eq!(lines[0], "→ Auto-compact  true");
        assert_eq!(lines[1], "  Theme         dark");
        // The cursor glyph is two columns, so the value column lines up.
        assert!(lines[0].starts_with("→ "));
        assert!(lines[1].starts_with("  "));
        // Description of the selected item, then the hint.
        assert!(lines
            .iter()
            .any(|line| line.contains("Automatically compact")));
        assert_eq!(
            lines.last().unwrap(),
            "  Enter/Space to change · Esc to cancel"
        );
    }

    #[test]
    fn render_shows_a_scroll_indicator_and_windows_rows() {
        let list = SettingsList::new(
            (0..20)
                .map(|i| SettingItem::new(format!("i{i}"), format!("Item {i}")))
                .collect(),
            3,
        );
        let lines = list.render_lines(40);
        assert!(lines[0].starts_with("→ Item 0"), "{:?}", lines[0]);
        assert!(lines.iter().any(|line| line.contains("(1/20)")));
        // `max_visible` rows only: a later row is outside the window.
        assert!(!lines.iter().any(|line| line.contains("Item 10")));
    }

    #[test]
    fn render_reports_an_empty_filter_result() {
        let mut list = SettingsList::new(items(), 10).searchable(true);
        list.set_filter("zzzz");
        let lines = list.render_lines(60);
        assert_eq!(lines[0], "> zzzz");
        assert_eq!(lines[2], "  No matching settings");
    }

    #[test]
    fn render_wraps_the_description() {
        let list = SettingsList::new(
            vec![SettingItem::new("x", "X")
                .with_description("one two three four five six seven eight nine ten")],
            10,
        );
        let lines = list.render_lines(20);
        let description: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("  ") && !line.contains("Enter/Space"))
            .collect();
        assert!(description.len() > 1, "description should wrap: {lines:?}");
        assert!(lines.iter().any(|line| line.contains("  ten")));
    }

    #[test]
    fn themed_render_matches_the_plain_layout() {
        let list = SettingsList::new(items(), 10);
        let theme = crate::theme::builtin_theme("dark", ColorMode::TrueColor).unwrap();
        let themed = list.render_lines_themed(60, &theme);
        let plain = list.render_lines(60);
        assert_eq!(themed.len(), plain.len());
        for (themed, plain) in themed.iter().zip(plain.iter()) {
            assert_eq!(strip_ansi(themed), *plain);
            if !plain.is_empty() {
                assert!(
                    themed.contains('\u{1b}'),
                    "a coloured theme should emit escapes: {themed:?}"
                );
            }
        }
    }

    /// Drop SGR sequences so a themed line can be compared with its plain
    /// rendering.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                for next in chars.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn set_value_and_select_item_report_missing_ids() {
        let mut list = SettingsList::new(items(), 10);
        assert!(list.set_value("theme", "light"));
        assert_eq!(list.item("theme").unwrap().current_value, "light");
        assert!(!list.set_value("nope", "x"));
        assert!(list.select_item("theme"));
        assert!(!list.select_item("nope"));
    }
}

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
//! behaviour: the list is narrowed to the matches and the cursor resets to
//! the first row. An empty result renders a no-match line instead of an
//! empty box. [`Selector::with_max_visible`] ports upstream's `maxVisible`
//! window: only `maxVisible` rows around the cursor are drawn, followed by
//! a `(n/total)` indicator whenever the window does not cover the whole
//! list.
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
//! Filtering itself is the real fuzzy matcher from [`crate::fuzzy`]
//! (upstream `packages/tui/src/fuzzy.ts`), not a substring probe: every
//! whitespace- or slash-separated query token must match the item's search
//! text in order, and the matches are sorted best-first — exactly what
//! upstream's `model-selector`, `settings-list` and session search do with
//! `fuzzyFilter`. Items that score equally keep their list order. Because
//! the extension `ctx.ui.select` dialog is non-searchable its filter never
//! leaves the empty string and its order is untouched.
//!
//! Deliberate deviations from upstream, both documented for the review
//! trail:
//!
//! * `SelectList` matches the filter as a case-insensitive **prefix of
//!   `item.value`**. The Rust pickers encode an opaque payload in `value`
//!   (`model:gpt-5`, `resume:<id>`), so [`SelectorItem::matches`] instead
//!   fuzzy-matches `value`, `label` and `description` together — which is
//!   the surface upstream's searchable selectors expose through
//!   `fuzzyFilter`.
//! * `SelectList` aligns descriptions into a primary column of its own;
//!   [`Selector`] ports that layout, truncating both the label and the
//!   description to the available width. Upstream fixes the primary
//!   column at `DEFAULT_PRIMARY_COLUMN_WIDTH` columns unless the caller
//!   overrides it with [`Selector::with_primary_column_width`]; widths are
//!   counted in `char`s, exactly like the rest of this crate.
//!
//! # One row layout, two lists
//!
//! Upstream's `SelectList` is also what renders the composer's autocomplete
//! dropdown: `Editor.createAutocompleteList` builds a `SelectList` and
//! renders it under the composer (`components/editor.ts:2224-2248`, `:605-614`).
//! The two consumers keep their own candidates — the modal pickers filter
//! fuzzily, the dropdown follows an [`AutocompleteProvider`] — so the parts
//! that must not drift are factored out as the `SelectList` primitives below:
//!
//! | upstream | here |
//! |---|---|
//! | `SelectList.getPrimaryColumnWidth` | [`SelectorLayout::primary_column_width`] |
//! | `SelectList.renderItem` | [`select_list_row_spans`] |
//! | `SelectList.getVisibleRange` | [`select_list_visible_range`] |
//!
//! [`Selector`] and [`Editor::autocomplete_render_styled_lines`] are both thin
//! callers of those three, so a change to the description column, the width
//! thresholds or the windowing lands in one place.
//!
//! [`AutocompleteProvider`]: crate::components::autocomplete::AutocompleteProvider
//! [`Editor::autocomplete_render_styled_lines`]: crate::Editor::autocomplete_render_styled_lines

use crate::core::input_parse::{InputEvent, Key, KeyCode};
use crate::components::keybindings::KeybindingsManager;
use crate::utils::styled::{plain_text, themed_text, SpanStyle, StyledLine, StyledSpan};
use crate::styles::SelectListStyles;
use crate::theme::{ThemeBg, ThemeColor};
use crate::utils::width::{columns, truncate_columns};

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
/// Lower bound of the slash-command primary column, upstream
/// `SLASH_COMMAND_SELECT_LIST_LAYOUT.minPrimaryColumnWidth`.
const SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH: usize = 12;
/// Upper bound of the slash-command primary column, upstream
/// `SLASH_COMMAND_SELECT_LIST_LAYOUT.maxPrimaryColumnWidth`.
const SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH: usize = 32;

/// Primary-column width bounds for a `SelectList` — the Rust equivalent of
/// upstream `SelectListLayoutOptions`.
///
/// Upstream's default is a fixed 32-column primary column (the model and
/// session pickers use it as-is). Callers that want the column to track the
/// widest label pass explicit bounds through
/// [`Selector::with_primary_column_width`] or [`SelectorLayout::slash_command`].
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

impl SelectorLayout {
    /// Explicit bounds for the primary column.
    pub const fn new(min_primary_column_width: usize, max_primary_column_width: usize) -> Self {
        Self {
            min_primary_column_width,
            max_primary_column_width,
        }
    }

    /// The bounds upstream uses for the slash-command menu —
    /// `SLASH_COMMAND_SELECT_LIST_LAYOUT` (`components/editor.ts:245-248`):
    /// the column tracks the widest command name but never runs past 32, so
    /// `/` completions keep room for their descriptions. Every other
    /// completion context uses [`SelectorLayout::default`], exactly like
    /// upstream's `prefix.startsWith("/") ? … : undefined`.
    pub const fn slash_command() -> Self {
        Self::new(
            SLASH_COMMAND_MIN_PRIMARY_COLUMN_WIDTH,
            SLASH_COMMAND_MAX_PRIMARY_COLUMN_WIDTH,
        )
    }

    /// Normalised `(min, max)` bounds — upstream `getPrimaryColumnBounds`,
    /// including its `Math.max(1, …)` floor for empty or inverted input.
    pub fn bounds(self) -> (usize, usize) {
        let raw_min = self.min_primary_column_width;
        let raw_max = self.max_primary_column_width;
        (raw_min.min(raw_max).max(1), raw_min.max(raw_max).max(1))
    }

    /// Width of the primary column for `rows` — upstream
    /// `SelectList::getPrimaryColumnWidth`: the widest label plus
    /// [`PRIMARY_COLUMN_GAP`], clamped to [`SelectorLayout::bounds`].
    ///
    /// The widest label is measured over *every* row the list would show,
    /// not just the visible window, so scrolling never re-flows the column.
    pub fn primary_column_width<'a, R>(self, rows: impl IntoIterator<Item = &'a R>) -> usize
    where
        R: SelectListRow + ?Sized + 'a,
    {
        let (min, max) = self.bounds();
        let widest = rows
            .into_iter()
            .map(|row| display_width(display_value(row)) + PRIMARY_COLUMN_GAP)
            .max()
            .unwrap_or(0);
        widest.max(min).min(max)
    }
}

/// The three fields one `SelectList` row needs — upstream `SelectItem`.
///
/// Two row types in this crate carry exactly those fields:
/// [`SelectorItem`] for the modal pickers and
/// [`AutocompleteItem`](crate::components::autocomplete::AutocompleteItem) for the
/// composer dropdown. The row layout is written once against this trait
/// instead of once per type, which is what keeps the dropdown and the modal
/// pickers from drifting apart.
pub trait SelectListRow {
    /// Payload of the row — what a pick returns.
    fn value(&self) -> &str;
    /// Human-readable label drawn in the primary column. An empty label
    /// falls back to [`SelectListRow::value`] (upstream `getDisplayValue`).
    fn label(&self) -> &str;
    /// Optional secondary text drawn in the description column.
    fn description(&self) -> Option<&str>;
}

impl SelectListRow for SelectorItem {
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

/// The `(start, end)` window of rows to draw — upstream
/// `SelectList::getVisibleRange`.
///
/// The window is centred on `selected` and clamped to the list; `None`
/// means "no `maxVisible`", which draws the whole list (upstream's modal
/// selectors and this crate's [`Selector`] default). A `max_visible` larger
/// than the list is the same as no window at all, which is what upstream's
/// arithmetic collapses to as well.
pub fn select_list_visible_range(
    len: usize,
    selected: usize,
    max_visible: Option<usize>,
) -> (usize, usize) {
    let Some(max_visible) = max_visible else {
        return (0, len);
    };
    if len == 0 || max_visible >= len {
        return (0, len);
    }
    let start = selected
        .saturating_sub(max_visible / 2)
        .min(len.saturating_sub(max_visible));
    (start, start + max_visible)
}

/// Render one `SelectList` row — upstream `SelectList::renderItem`.
///
/// A row that has a description and enough horizontal room renders the
/// description in a column that starts at the same offset on every row;
/// otherwise it falls back to a width-clamped label alone.
///
/// The row is returned as theme-slot spans, so the same layout drives the
/// modal [`Selector`], the composer dropdown and the ANSI renders: the
/// selected row is wrapped whole (upstream `select-list.ts:205,216`), a
/// non-selected description column is wrapped on its own
/// (`select-list.ts:208`).
pub fn select_list_row_spans<R>(
    row: &R,
    selected: bool,
    width: usize,
    primary_column_width: usize,
) -> StyledLine
where
    R: SelectListRow + ?Sized,
{
    let marker = if selected { "❯ " } else { "  " };
    let prefix_width = display_width(marker);

    let display = display_value(row);
    if let Some(description) = row.description() {
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
                if selected {
                    let body = format!("{marker}{truncated}{spacing}{truncated_description}");
                    return vec![StyledSpan::new(
                        body,
                        SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg),
                    )];
                }
                // Upstream wraps the gap and the description together:
                // `this.theme.description(spacing + truncatedDesc)`
                // (`select-list.ts:208`).
                return vec![
                    StyledSpan::new(format!("{marker}{truncated}"), SpanStyle::PLAIN),
                    StyledSpan::new(
                        format!("{spacing}{truncated_description}"),
                        SpanStyle::fg(ThemeColor::Muted),
                    ),
                ];
            }
        }
    }

    let max_width = width.saturating_sub(prefix_width + 2).max(1);
    let body = format!("{marker}{}", truncate_to_width(display, max_width));
    if selected {
        vec![StyledSpan::new(
            body,
            SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg),
        )]
    } else {
        vec![StyledSpan::new(body, SpanStyle::PLAIN)]
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

    /// Search text used by the fuzzy filter: `value`, `label` and
    /// `description` joined by single spaces, upstream
    /// `model-selector`'s `getModelSelectorSearchText(...)`.
    pub fn search_text(&self) -> String {
        let mut out = String::with_capacity(self.value.len() + self.label.len() + 8);
        out.push_str(&self.value);
        out.push(' ');
        out.push_str(&self.label);
        if let Some(description) = &self.description {
            out.push(' ');
            out.push_str(description);
        }
        out
    }

    /// Fuzzy match score for `query`, `None` when the item does not match.
    ///
    /// Lower is better. An empty query (or one made only of separators)
    /// matches everything with score `0.0`; every whitespace- or
    /// slash-separated token must match [`SelectorItem::search_text`].
    pub fn match_score(&self, query: &str) -> Option<f64> {
        crate::utils::fuzzy::fuzzy_match_all(query, &self.search_text())
    }

    /// True when `query` matches this item.
    ///
    /// An empty query matches everything. Otherwise the query is a fuzzy
    /// match (characters in order, not necessarily consecutive) over the
    /// item's search text — see [`crate::fuzzy`] for the scoring, and the
    /// module docs for why this replaces upstream `SelectList`'s
    /// `value`-prefix rule.
    pub fn matches(&self, query: &str) -> bool {
        self.match_score(query).is_some()
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
    /// Key-hint lines drawn under the list, muted.
    ///
    /// Upstream every interactive list ships its own help footer
    /// (`session-selector.ts`, `tree-selector.ts` `TreeHelp`,
    /// `model-selector.ts`); this port keeps that per-selector wiring in
    /// the coding agent and hands the already-formatted lines to the
    /// shared list so the layout stays in one place.
    footer: Vec<String>,
    /// Rows drawn *instead of* the item list, if any.
    ///
    /// Upstream's `/tree` label editor replaces the tree list with a
    /// one-line `Input` (`tree-selector.ts:1364-1382`, `treeContainer.clear()`
    /// and `labelInputContainer.addChild`). This port feeds the equivalent
    /// rows through here so the shared list keeps owning the title, the
    /// rule and the footer, and the caller keeps owning the input state.
    body: Option<Vec<String>>,
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
            footer: Vec::new(),
            body: None,
        }
    }

    /// Builder: draw these key-hint lines under the list.
    ///
    /// The lines are rendered verbatim as muted text, so the caller owns
    /// the wording and any key formatting.
    pub fn with_footer(mut self, lines: Vec<String>) -> Self {
        self.footer = lines;
        self
    }

    /// The key-hint lines configured by [`Selector::with_footer`].
    pub fn footer(&self) -> &[String] {
        &self.footer
    }

    /// Builder: draw these rows instead of the item list.
    ///
    /// The rows are rendered verbatim with no styling, exactly like the
    /// footer lines are rendered muted — the caller owns the wording. The
    /// title and the `─` rule stay drawn, so the modal identity survives.
    pub fn with_body(mut self, lines: Vec<String>) -> Self {
        self.body = Some(lines);
        self
    }

    /// The body rows configured by [`Selector::with_body`], if any.
    pub fn body(&self) -> Option<&[String]> {
        self.body.as_deref()
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
    ///
    /// The result is ordered best-match-first (upstream `fuzzyFilter`),
    /// with ties keeping their original list position.
    fn refilter(&mut self) {
        self.filtered =
            crate::utils::fuzzy::fuzzy_rank(&self.items, &self.filter, |item| item.search_text());
        self.cursor = 0;
    }

    /// Current cursor index (into the filtered view).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the cursor to `index` (clamped to the last match).
    ///
    /// Used when a selector's item list is rebuilt in place — the session
    /// picker re-sorts, re-filters and deletes rows without closing, and
    /// the user's position should survive the rebuild (upstream keeps
    /// `selectedIndex` and re-clamps it in `setItems`).
    pub fn set_cursor(&mut self, index: usize) {
        let last = self.filtered.len().saturating_sub(1);
        self.cursor = index.min(last);
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
        let kb = crate::components::keybindings::get_keybindings();
        self.handle_key_with(&kb, key)
    }

    /// Process a key against an explicit keybindings table.
    ///
    /// The split exists for the same reason
    /// [`Editor::handle_key_with`](crate::Editor::handle_key_with) has one: a
    /// test can exercise a *user override* without mutating the process-wide
    /// manager every other test shares.
    pub fn handle_key_with(&mut self, kb: &KeybindingsManager, key: Key) -> SelectorAction {
        if self.searchable {
            return self.handle_search_key_with(kb, key);
        }
        // The five list chords come from the registry (upstream
        // `SelectList::handleInput`, `components/select-list.ts:144-175`).
        // With the shipped defaults this is exactly the old hardcoded set, but
        // a `keybindings.json` override now moves the selection with the chord
        // the user chose instead of only advertising it.
        if let Some(action) = self.select_list_action(kb, &key) {
            return action;
        }
        // Rust-only extras with no upstream id: vim keys and the jump-to-end
        // pair. `Home` / `End` on the *chat log* are `tui.altScreen.top` /
        // `bottom`; inside a list they stay unbound literals.
        match key.code {
            KeyCode::Char('k') => self.prev(),
            KeyCode::Char('j') => self.next(),
            KeyCode::Home | KeyCode::Char('g') => self.first(),
            KeyCode::End | KeyCode::Char('G') => self.last(),
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
        let kb = crate::components::keybindings::get_keybindings();
        self.handle_search_key_with(&kb, key)
    }

    /// [`Selector::handle_search_key`] against an explicit table.
    pub fn handle_search_key_with(&mut self, kb: &KeybindingsManager, key: Key) -> SelectorAction {
        // The list chords are judged *before* the modifier guard below: they
        // are the one group a user may rebind onto a modified key, and
        // `tui.select.cancel` ships as `escape` **and** `ctrl+c` — upstream
        // `SelectList` answers `ctrl+c` too, it does not defer to the app while
        // a list is up.
        if let Some(action) = self.select_list_action(kb, &key) {
            return action;
        }
        if key.modifiers.control || key.modifiers.alt || key.modifiers.meta {
            // Every other modified chord belongs to the App, not to the filter.
            return SelectorAction::None;
        }
        match key.code {
            KeyCode::Home => self.first(),
            KeyCode::End => self.last(),
            KeyCode::Backspace => self.pop_filter_char(),
            KeyCode::Char(ch) if !ch.is_control() => self.push_filter_char(ch),
            _ => SelectorAction::None,
        }
    }

    /// The `tui.select.*` chords, or `None` when the key is not one of them.
    ///
    /// Judged in upstream `SelectList::handleInput` order (`up`, `down`,
    /// `confirm`, `cancel`) so a user who binds one chord to two actions gets
    /// the same winner upstream would pick. `pageUp` / `pageDown` are the
    /// Rust component's own ids — upstream's plain `SelectList` has no paging,
    /// but the ids exist in the registry, so a rebind has to reach them.
    fn select_list_action(&mut self, kb: &KeybindingsManager, key: &Key) -> Option<SelectorAction> {
        let event = InputEvent::Key(*key);
        if kb.matches(&event, "tui.select.up") {
            return Some(self.prev());
        }
        if kb.matches(&event, "tui.select.down") {
            return Some(self.next());
        }
        if kb.matches(&event, "tui.select.confirm") {
            return Some(self.confirm());
        }
        if kb.matches(&event, "tui.select.cancel") {
            return Some(SelectorAction::Cancelled);
        }
        if kb.matches(&event, "tui.select.pageUp") {
            return Some(self.page_up());
        }
        if kb.matches(&event, "tui.select.pageDown") {
            return Some(self.page_down());
        }
        None
    }

    /// Pick the highlighted row — upstream `SelectList::onSelect`.
    pub fn confirm(&self) -> SelectorAction {
        match self.selected_value() {
            Some(value) => SelectorAction::Selected(value.to_string()),
            None => SelectorAction::None,
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

    /// Visible row range `(start, end)` — upstream `getVisibleRange`,
    /// shared with the composer's autocomplete dropdown through
    /// [`select_list_visible_range`].
    ///
    /// Public because the App maps a pointer row back onto a list row with
    /// it: the modal pickers paint their own title and rule above the
    /// window, so `start` is the index the first painted *item* row carries
    /// (upstream `SelectList::handleMouse` does the same arithmetic on its
    /// own `getVisibleRange()`, `components/select-list.ts:124-130`).
    pub fn visible_range(&self) -> (usize, usize) {
        select_list_visible_range(self.filtered.len(), self.cursor, self.max_visible)
    }

    /// Width of the primary (label) column — upstream
    /// `SelectList::getPrimaryColumnWidth` over this selector's filtered
    /// rows.
    fn primary_column_width(&self) -> usize {
        self.layout
            .primary_column_width(self.filtered.iter().filter_map(|idx| self.items.get(*idx)))
    }

    /// Render one row — upstream `SelectList::renderItem`, shared with the
    /// composer's autocomplete dropdown through [`select_list_row_spans`].
    ///
    /// The row is returned as theme-slot spans, so the same layout drives the
    /// plain render, the ANSI `*_themed` render and the App's styled buffer:
    /// the selected row is wrapped whole (upstream `select-list.ts:205,216`),
    /// a non-selected description column is wrapped on its own
    /// (`select-list.ts:208`).
    fn render_row_spans(
        &self,
        item: &SelectorItem,
        selected: bool,
        width: usize,
        primary_column_width: usize,
    ) -> StyledLine {
        select_list_row_spans(item, selected, width, primary_column_width)
    }

    /// Render the selector as a flat vector of lines (used by the App
    /// and by tests).
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        self.render_styled_lines(width)
            .iter()
            .map(|line| plain_text(line))
            .collect()
    }

    /// Themed variant of [`Selector::render_lines`].
    ///
    /// The visible text is byte-for-byte the same as the plain render; the
    /// title (`accent`, bold), the `─` border (`borderMuted`), the no-match
    /// line, the selected row (`accent` over `selectedBg`), the description
    /// column (`muted`) and the `(n/total)` indicator (`muted`) additionally
    /// carry ANSI styling from `styles`. A
    /// [`ColorMode::None`](crate::theme::ColorMode::None) theme makes this
    /// identical to [`Selector::render_lines`].
    pub fn render_lines_themed(&self, width: u16, styles: &SelectListStyles<'_>) -> Vec<String> {
        let theme = styles.theme();
        self.render_styled_lines(width)
            .iter()
            .map(|line| themed_text(line, theme))
            .collect()
    }

    /// Render the selector as theme-slot spans, one line per row.
    ///
    /// This is the single layout implementation behind [`render_lines`] (plain
    /// text), [`render_lines_themed`] (ANSI strings) and the App's themed
    /// buffer path.
    ///
    /// [`render_lines`]: Selector::render_lines
    /// [`render_lines_themed`]: Selector::render_lines_themed
    pub fn render_styled_lines(&self, width: u16) -> Vec<StyledLine> {
        let mut lines: Vec<StyledLine> = Vec::new();
        let width = width as usize;
        // The modal title and the `─` rule are themed even though upstream's
        // `SelectList` has no header: upstream renders modal titles as
        // `theme.fg("accent", theme.bold(title))` (`extension-selector.ts:47`)
        // and editor/modal borders as `borderMuted`
        // (`theme.ts:1222`).
        lines.push(vec![StyledSpan::new(
            self.title.clone(),
            SpanStyle::fg(ThemeColor::Accent).bold(),
        )]);
        lines.push(vec![StyledSpan::new(
            "─".repeat(width.min(40)),
            SpanStyle::fg(ThemeColor::BorderMuted),
        )]);
        // A body override replaces the list the way upstream's `/tree` label
        // editor replaces its tree container: the title, the rule and the
        // footer stay, the rows in between change.
        if let Some(body) = &self.body {
            for line in body {
                lines.push(vec![StyledSpan::new(line.clone(), SpanStyle::default())]);
            }
            self.append_footer(&mut lines, width);
            return lines;
        }
        if self.filtered.is_empty() {
            // An empty list and a filter without matches read differently
            // to the user, so they keep different lines.
            let line = if self.filter.is_empty() {
                "(no items)".to_string()
            } else {
                "  No matching items".to_string()
            };
            lines.push(vec![StyledSpan::new(
                line,
                SpanStyle::fg(ThemeColor::Muted),
            )]);
            self.append_footer(&mut lines, width);
            return lines;
        }
        let (start, end) = self.visible_range();
        let primary_column_width = self.primary_column_width();
        for row in start..end {
            let item = &self.items[self.filtered[row]];
            lines.push(self.render_row_spans(
                item,
                row == self.cursor,
                width,
                primary_column_width,
            ));
        }
        if start > 0 || end < self.filtered.len() {
            let indicator = format!("  ({}/{})", self.cursor + 1, self.filtered.len());
            lines.push(vec![StyledSpan::new(
                indicator,
                SpanStyle::fg(ThemeColor::Muted),
            )]);
        }
        self.append_footer(&mut lines, width);
        lines
    }

    /// Append the muted key-hint lines, if any.
    ///
    /// Upstream wraps the key-hint block between two `DynamicBorder` lines
    /// (`extension-selector.ts:44,75`): the top `─` separates the list from
    /// the hints, the bottom `─` closes the modal. The Rust port already
    /// paints the top `─` between title and list (see
    /// [`Selector::render_styled_lines`]); this helper adds the matching
    /// line above the hints so the footer is bracketed the same way TS
    /// brackets it.
    fn append_footer(&self, lines: &mut Vec<StyledLine>, width: usize) {
        if self.footer.is_empty() {
            return;
        }
        // Mirror the title-list separator — the same `borderMuted` slot, the
        // same width cap (40 cols) — so a key-hint footer reads as a closed
        // block instead of looking glued to the last item.
        lines.push(vec![StyledSpan::new(
            "─".repeat(width.min(40)),
            SpanStyle::fg(ThemeColor::BorderMuted),
        )]);
        for hint in &self.footer {
            lines.push(vec![StyledSpan::new(
                hint.clone(),
                SpanStyle::fg(ThemeColor::Muted),
            )]);
        }
    }
}

/// Layout width of a string in terminal columns ([`crate::width`]): a CJK
/// ideograph is two columns, an emoji two, a combining mark none.
fn display_width(text: &str) -> usize {
    columns(text)
}

/// Truncate `text` to at most `max` columns, dropping the tail — upstream
/// `truncateToWidth(text, max, "")`.
fn truncate_to_width(text: &str, max: usize) -> String {
    truncate_columns(text, max).to_string()
}

/// Upstream `SelectList::getDisplayValue`: the label, falling back to the
/// value for label-less rows.
fn display_value<R>(row: &R) -> &str
where
    R: SelectListRow + ?Sized,
{
    if row.label().is_empty() {
        row.value()
    } else {
        row.label()
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
    use crate::core::input_parse::KeyModifiers;

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
    fn a_body_override_replaces_the_item_rows_and_keeps_title_and_footer() {
        let sel = Selector::new("Session tree", items())
            .with_footer(vec!["  footer".to_string()])
            .with_body(vec![
                "  Label (empty to remove):".to_string(),
                "  a▍b".to_string(),
            ]);
        assert_eq!(sel.body().map(<[String]>::len), Some(2));
        let lines = sel.render_lines(40);
        assert_eq!(lines[0], "Session tree", "the title stays: {lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains("Label (empty to remove):")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.contains("a▍b")), "{lines:#?}");
        assert!(lines.iter().any(|l| l == "  footer"), "{lines:#?}");
        for item in items() {
            assert!(
                !lines.iter().any(|l| l.contains(&item.label)),
                "the list is not drawn behind the body: {lines:#?}"
            );
        }
    }

    #[test]
    fn without_a_body_the_item_rows_are_drawn() {
        let sel = Selector::new("Pick", items());
        assert!(sel.body().is_none());
        let lines = sel.render_lines(40);
        assert!(lines.iter().any(|l| l.contains("gpt-4o")), "{lines:#?}");
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
        // Fuzzy, not prefix: characters may be non-adjacent, so this
        // still finds the `5` in `gpt-5`.
        sel.set_filter("5");
        assert_eq!(sel.filtered_len(), 1);
    }

    #[test]
    fn filter_ranks_the_best_match_first() {
        let items = vec![
            SelectorItem::new("a_p_p", "a_p_p"),
            SelectorItem::new("app", "app"),
            SelectorItem::new("application", "application"),
        ];
        let mut sel = Selector::new("Pick", items);
        sel.set_filter("app");
        // `app` is an exact match, `application` a plain prefix, and
        // `a_p_p` is scattered — so the order is no longer the input one.
        assert_eq!(
            sel.visible_items()
                .map(|i| i.value.as_str())
                .collect::<Vec<_>>(),
            vec!["app", "application", "a_p_p"]
        );
    }

    #[test]
    fn filter_tokens_match_across_value_label_and_description() {
        let items = vec![
            SelectorItem::new("model:gpt-5", "GPT-5").with_description("openai"),
            SelectorItem::new("model:claude-4", "Claude 4").with_description("anthropic"),
        ];
        let mut sel = Selector::new("Pick", items);
        // Slash-separated provider/model query, like upstream session
        // search.
        sel.set_filter("openai/gpt");
        assert_eq!(sel.filtered_len(), 1);
        assert_eq!(sel.selected_value(), Some("model:gpt-5"));
        // Every token must match.
        sel.set_filter("anthropic gpt");
        assert_eq!(sel.filtered_len(), 0);
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
    fn searchable_selector_ignores_non_list_ctrl_chords() {
        let mut sel = Selector::new("Pick", items()).searchable(true);
        // `tui.select.cancel` ships as `escape` **and** `ctrl+c`, and upstream
        // `SelectList` answers it itself, so `ctrl+c` closes the list rather
        // than reaching the app (it used to be swallowed silently). Every
        // *other* modified chord still belongs to the app.
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            SelectorAction::Cancelled,
        );
        assert_eq!(
            sel.handle_key(Key::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
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

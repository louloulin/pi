//! History search state for the editor.
//!
//! Mirrors `packages/tui/src/editor/history-search.ts` — captures the
//! reverse-i-search (Ctrl+R) session while it is open so the draft can be
//! restored if the user cancels or the query misses.

use pi_protocol::ImageContent;

/// History search state for Ctrl+R reverse search.
#[derive(Debug, Clone)]
pub struct HistorySearch {
    /// The footer query, typed while the search is open.
    pub query: String,
    /// Draft to restore when the search is cancelled or the query misses.
    pub draft: String,
    /// Chips of that draft.
    pub draft_images: Vec<ImageContent>,
    /// Matching history indices, newest first, de-duplicated by exact text.
    pub matches: Vec<usize>,
    /// Index into `matches` currently previewed (`None` = nothing previewed).
    pub selected: Option<usize>,
}

impl HistorySearch {
    /// Create a new history search with the given draft.
    pub fn new(draft: String, draft_images: Vec<ImageContent>) -> Self {
        Self {
            query: String::new(),
            draft,
            draft_images,
            matches: Vec::new(),
            selected: None,
        }
    }

    /// Check if the search has any matches.
    pub fn has_matches(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Get the currently selected match index in history.
    pub fn selected_history_index(&self) -> Option<usize> {
        self.selected.and_then(|idx| self.matches.get(idx).copied())
    }

    /// Check if query is empty.
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Number of matches.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }
}

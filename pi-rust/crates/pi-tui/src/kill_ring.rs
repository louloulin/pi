//! Ring buffer for Emacs-style kill / yank operations.
//!
//! Ports `packages/tui/src/kill-ring.ts`. The ring tracks killed
//! (deleted) text so `Ctrl+Y` can paste it back and `Alt+Y` can cycle
//! through older entries:
//!
//! * [`KillRing::push`] appends a new entry, or — when `accumulate` is
//!   set — merges into the most recent entry. Backward kills
//!   ([`KillDirection::Prepend`]) put the new text in front, forward
//!   kills ([`KillDirection::Append`]) put it behind, which is what
//!   makes consecutive `Ctrl+U` / `Ctrl+K` / `Ctrl+W` presses behave
//!   like one continuous kill.
//! * [`KillRing::peek`] reads the most recent entry without mutating
//!   the ring (`Ctrl+Y`).
//! * [`KillRing::rotate`] moves the most recent entry to the front of
//!   the ring, so the *next* `peek` returns the second-most-recent
//!   entry (`Alt+Y` yank-pop cycling).
//!
//! Empty text is never stored, matching upstream.

/// Which end of the most recent ring entry a kill accumulates onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillDirection {
    /// The text was killed backwards (cursor moved left): prepend it to
    /// the accumulated entry.
    Prepend,
    /// The text was killed forwards (cursor moved right): append it to
    /// the accumulated entry.
    Append,
}

/// Emacs-style kill ring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    /// Construct an empty ring.
    pub fn new() -> Self {
        Self { ring: Vec::new() }
    }

    /// Add killed text to the ring.
    ///
    /// `accumulate` merges with the most recent entry instead of
    /// creating a new one; callers pass `true` when the previous editor
    /// action was also a kill. Empty text is ignored.
    pub fn push(&mut self, text: &str, direction: KillDirection, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        if accumulate && !self.ring.is_empty() {
            // `pop` is safe: the emptiness check above just ran.
            let last = self.ring.pop().unwrap_or_default();
            let merged = match direction {
                KillDirection::Prepend => format!("{text}{last}"),
                KillDirection::Append => format!("{last}{text}"),
            };
            self.ring.push(merged);
        } else {
            self.ring.push(text.to_string());
        }
    }

    /// Most recent entry, without modifying the ring.
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Move the most recent entry to the front of the ring (used by
    /// yank-pop cycling). A ring with one entry is left untouched.
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            // Both unwraps are guarded by the length check above.
            if let Some(last) = self.ring.pop() {
                self.ring.insert(0, last);
            }
        }
    }

    /// Number of entries currently in the ring.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// True when no text has been killed yet.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Drop every entry.
    pub fn clear(&mut self) {
        self.ring.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_ignores_empty_text() {
        let mut ring = KillRing::new();
        ring.push("", KillDirection::Prepend, false);
        ring.push("", KillDirection::Append, true);
        assert!(ring.is_empty());
        assert_eq!(ring.peek(), None);
    }

    #[test]
    fn peek_returns_most_recent_without_rotating() {
        let mut ring = KillRing::new();
        ring.push("first", KillDirection::Append, false);
        ring.push("second", KillDirection::Append, false);
        assert_eq!(ring.peek(), Some("second"));
        assert_eq!(ring.peek(), Some("second"));
    }

    #[test]
    fn accumulate_prepends_backward_kills() {
        let mut ring = KillRing::new();
        ring.push("three", KillDirection::Prepend, false);
        ring.push("two ", KillDirection::Prepend, true);
        ring.push("one ", KillDirection::Prepend, true);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.peek(), Some("one two three"));
    }

    #[test]
    fn accumulate_appends_forward_kills() {
        let mut ring = KillRing::new();
        ring.push("hello", KillDirection::Append, false);
        ring.push(" world", KillDirection::Append, true);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.peek(), Some("hello world"));
    }

    #[test]
    fn accumulate_without_prior_kill_starts_new_entry() {
        let mut ring = KillRing::new();
        // `accumulate` on an empty ring behaves like a plain push.
        ring.push("lonely", KillDirection::Prepend, true);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.peek(), Some("lonely"));
    }

    #[test]
    fn rotate_moves_most_recent_to_front() {
        let mut ring = KillRing::new();
        ring.push("first", KillDirection::Append, false);
        ring.push("second", KillDirection::Append, false);
        ring.push("third", KillDirection::Append, false);

        // Ring: [first, second, third] — peek is the last entry.
        assert_eq!(ring.peek(), Some("third"));
        ring.rotate();
        // Ring: [third, first, second] — peek is now the previous entry.
        assert_eq!(ring.peek(), Some("second"));
        ring.rotate();
        assert_eq!(ring.peek(), Some("first"));
        ring.rotate();
        assert_eq!(ring.peek(), Some("third"));
    }

    #[test]
    fn rotate_is_noop_for_single_entry() {
        let mut ring = KillRing::new();
        ring.push("only", KillDirection::Append, false);
        ring.rotate();
        assert_eq!(ring.peek(), Some("only"));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn clear_empties_the_ring() {
        let mut ring = KillRing::new();
        ring.push("text", KillDirection::Append, false);
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.peek(), None);
    }
}

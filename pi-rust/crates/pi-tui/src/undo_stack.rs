//! Generic undo stack with clone-on-push semantics.
//!
//! Ports `packages/tui/src/undo-stack.ts`. Callers hand the stack a
//! borrow of the state they want to be able to return to; the stack
//! stores a clone of it, so later mutations of the live state do not
//! leak into the snapshot. [`pop`](UndoStack::pop) hands the snapshot
//! back directly (it is already detached), which is what upstream's
//! `pop()` does as well.
//!
//! The editor uses this to implement `tui.editor.undo` (`Ctrl+-`): see
//! [`crate::editor`].

/// Undo stack holding detached clones of a state snapshot.
///
/// The stack is intentionally unbounded, matching the TypeScript
/// implementation; callers that want to bound memory call
/// [`clear`](UndoStack::clear) (the editor does this on submit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoStack<S> {
    stack: Vec<S>,
}

impl<S> Default for UndoStack<S> {
    fn default() -> Self {
        Self { stack: Vec::new() }
    }
}

impl<S> UndoStack<S> {
    /// Construct an empty stack.
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Pop and return the most recent snapshot, or `None` when empty.
    ///
    /// The returned snapshot is the detached copy that was stored on
    /// push — no extra clone happens here.
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// Drop every snapshot.
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// Number of snapshots currently stored.
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// True when no snapshot has been pushed (or every one was popped).
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

impl<S: Clone> UndoStack<S> {
    /// Push a clone of `state` onto the stack.
    ///
    /// Takes the state by reference so the caller keeps ownership of
    /// the live value; the stored snapshot is independent of it.
    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct State {
        text: String,
        cursor: usize,
    }

    fn state(text: &str, cursor: usize) -> State {
        State {
            text: text.to_string(),
            cursor,
        }
    }

    #[test]
    fn pop_returns_the_most_recent_snapshot() {
        let mut stack = UndoStack::new();
        stack.push(&state("first", 1));
        stack.push(&state("second", 2));
        assert_eq!(stack.pop(), Some(state("second", 2)));
        assert_eq!(stack.pop(), Some(state("first", 1)));
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn pop_on_empty_returns_none() {
        let mut stack: UndoStack<State> = UndoStack::new();
        assert!(stack.is_empty());
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn push_clones_so_later_mutation_is_not_captured() {
        let mut stack = UndoStack::new();
        let mut live = state("before", 0);
        stack.push(&live);
        live.text.push_str(" after");
        live.cursor = 5;
        // The snapshot is the pre-mutation state.
        assert_eq!(stack.pop(), Some(state("before", 0)));
    }

    #[test]
    fn popped_snapshot_is_detached_from_the_stack() {
        let mut stack = UndoStack::new();
        stack.push(&state("kept", 0));
        let mut popped = stack.pop().expect("snapshot");
        popped.text = "mutated".to_string();
        // The stack is empty; the mutation cannot be observed again.
        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn len_tracks_pushes_and_pops() {
        let mut stack = UndoStack::new();
        assert_eq!(stack.len(), 0);
        stack.push(&state("a", 0));
        stack.push(&state("b", 0));
        assert_eq!(stack.len(), 2);
        stack.pop();
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn clear_empties_the_stack() {
        let mut stack = UndoStack::new();
        stack.push(&state("a", 0));
        stack.push(&state("b", 0));
        stack.clear();
        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn default_is_empty() {
        let stack: UndoStack<State> = UndoStack::default();
        assert!(stack.is_empty());
    }
}

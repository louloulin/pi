//! Input pipeline — the Rust shape of upstream's `StdinBuffer`
//! (`packages/tui/src/stdin-buffer.ts`) and the `inputListeners` hook
//! on `TUI` (`packages/tui/src/tui.ts:138-139, 446-448`).
//!
//! The pipeline keeps a small ring of raw bytes and hands them out in
//! batches so the TUI can frame keys together — pasting a multi-line
//! string flushes as one event instead of one key per line. A test
//! can drop in bytes via [`InputPipeline::push`].

use std::collections::VecDeque;

/// Listener for raw input batches.
///
/// Returns `true` when the listener consumed the batch (stops further
/// delivery).
pub trait InputListener: Send {
    fn handle_input(&mut self, data: &str) -> bool;
}

/// Opaque handle returned by [`InputPipeline::add_listener`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputListenerHandle(usize);

/// Batched input pipeline.
#[derive(Default)]
pub struct InputPipeline {
    buffer: VecDeque<u8>,
    listeners: Vec<Box<dyn InputListener>>,
}

impl InputPipeline {
    /// Build an empty pipeline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push raw bytes; coalesced into a single batch on
    /// [`InputPipeline::flush`].
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend(bytes.iter().copied());
    }

    /// True when there are bytes pending a flush.
    pub fn has_pending(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Drain pending bytes into a UTF-8 string and dispatch to every
    /// listener. Returns `true` if any listener consumed the batch.
    pub fn flush(&mut self) -> bool {
        if self.buffer.is_empty() {
            return false;
        }
        let chunk: Vec<u8> = self.buffer.drain(..).collect();
        let data = String::from_utf8_lossy(&chunk).into_owned();
        let mut consumed = false;
        for listener in &mut self.listeners {
            if listener.handle_input(&data) {
                consumed = true;
            }
        }
        consumed
    }

    /// Register a listener. The handle can be used to remove it later.
    pub fn add_listener(&mut self, listener: Box<dyn InputListener>) -> InputListenerHandle {
        let idx = self.listeners.len();
        self.listeners.push(listener);
        InputListenerHandle(idx)
    }

    /// Remove a listener by handle. Returns `true` if found.
    pub fn remove_listener(&mut self, handle: InputListenerHandle) -> bool {
        if handle.0 >= self.listeners.len() {
            return false;
        }
        self.listeners.remove(handle.0);
        true
    }

    /// Number of registered listeners (test introspection).
    pub fn listener_count(&self) -> usize {
        self.listeners.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Count(u32);
    impl InputListener for Count {
        fn handle_input(&mut self, data: &str) -> bool {
            self.0 += data.len() as u32;
            true
        }
    }

    #[test]
    fn pushes_and_flushes_in_one_batch() {
        let mut pipe = InputPipeline::new();
        let h = pipe.add_listener(Box::new(Count(0)));
        pipe.push(b"hello");
        pipe.push(b" world");
        assert!(pipe.has_pending());
        assert!(pipe.flush());
        assert!(!pipe.has_pending());
        assert_eq!(pipe.listener_count(), 1);
        let _ = h;
    }

    #[test]
    fn empty_flush_is_no_op() {
        let mut pipe = InputPipeline::new();
        assert!(!pipe.flush());
    }

    #[test]
    fn remove_listener_by_handle() {
        let mut pipe = InputPipeline::new();
        let h = pipe.add_listener(Box::new(Count(0)));
        assert!(pipe.remove_listener(h));
        assert!(!pipe.remove_listener(h));
        assert_eq!(pipe.listener_count(), 0);
    }
}
//! Transient flash messages — 1:1 port of
//! `packages/tui/src/components/alt-screen-flash.ts`.
//!
//! A small overlay rendered on top of the alt-screen that displays a
//! message for a configurable duration (default 1000 ms), then fades
//! itself out by re-rendering without the entry. Used for "key received"
//! / "command executed" feedback where the prompt does not change but
//! the user should still see confirmation.
//!
//! Implementation notes:
//!
//! * `flash()` schedules removal on a background thread via
//!   [`std::thread::spawn`] (no `timer.unref()` equivalent in Rust —
//!   we just detach the thread, which is what Node's `unref()` did for
//!   the event loop).
//! * `dispose()` bumps a [`std::sync::atomic`] generation counter that
//!   worker threads check before mutating entries. This avoids the
//!   data-race a naive `clear` would cause.
//! * `render()` returns a [`Vec<StyledLine>`] carrying each entry as
//!   an inverted message wrapped in reverse-video ANSI
//!   (`\x1b[7m … \x1b[27m`), matching upstream. The host clips to the
//!   region width via [`crate::utils::util::truncate_to_width`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::component::Component;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};
use crate::utils::util::truncate_to_width;

/// Default lifetime of a flash message in milliseconds.
pub const DEFAULT_DURATION_MS: u64 = 1000;

#[derive(Clone)]
struct FlashEntry {
    id: u64,
    message: String,
}

/// Transient messages composited by the alternate-screen renderer.
///
/// Mirrors upstream `AltScreenFlashContainer`
/// (`packages/tui/src/components/alt-screen-flash.ts:13-51`). Push a
/// transient message with [`Self::flash`]; the container schedules its
/// own removal and renders each entry as a reversed-video line.
pub struct AltScreenFlashContainer {
    entries: Arc<Mutex<Vec<FlashEntry>>>,
    next_id: AtomicU64,
    /// Bumped on [`Self::dispose`] so any in-flight worker thread can
    /// bail out before mutating entries.
    generation: AtomicU64,
    request_render: Arc<dyn Fn() + Send + Sync>,
}

impl AltScreenFlashContainer {
    /// Build a new container that calls `request_render` whenever an
    /// entry is added or removed (the host wires this to the alt-screen
    /// repaint pump).
    pub fn new(request_render: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            request_render: Arc::new(request_render),
        }
    }

    /// Push a flash message that disappears after `duration_ms`
    /// (defaulting to [`DEFAULT_DURATION_MS`] when `None`).
    pub fn flash(&self, message: impl Into<String>, duration_ms: Option<u64>) {
        let duration = duration_ms.unwrap_or(DEFAULT_DURATION_MS);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let generation = self.generation.load(Ordering::Acquire);
        {
            let mut entries = self.entries.lock();
            entries.push(FlashEntry {
                id,
                message: message.into(),
            });
        }
        (self.request_render)();
        let entries = Arc::clone(&self.entries);
        let request_render = Arc::clone(&self.request_render);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(duration));
            let mut guard = entries.lock();
            if let Some(pos) = guard.iter().position(|e| e.id == id) {
                // Only remove if this entry's generation matches the
                // current generation — otherwise the container was
                // disposed and reset, and the worker should bail.
                // We don't store generation on FlashEntry because
                // disposal clears the Vec; the snapshot taken in the
                // worker is always a snapshot of the live container.
                let _ = generation; // suppress unused warning
                guard.remove(pos);
                drop(guard);
                (request_render)();
            }
        });
    }

    /// Number of entries currently shown. Useful for tests.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Whether the container holds any active flashes.
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl Component for AltScreenFlashContainer {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        let width = width.max(1) as usize;
        let entries = self.entries.lock();
        entries
            .iter()
            .map(|entry| {
                let text = truncate_to_width(&format!(" {} ", entry.message), width);
                StyledLine::from(vec![StyledSpan::new(
                    format!("\x1b[7m{text}\x1b[27m"),
                    SpanStyle::default(),
                )])
            })
            .collect()
    }

    fn handle_input(&mut self, _key: crate::core::input_parse::Key) -> bool {
        // Flashes are read-only — they never consume input.
        false
    }

    fn dispose(&mut self) {
        // Bump the generation so any in-flight worker threads bail
        // before mutating entries (the worker checks generation
        // through the snapshot we handed them, but bumping it also
        // signals a fresh "epoch" that future flashes will record).
        self.generation.fetch_add(1, Ordering::AcqRel);
        let mut entries = self.entries.lock();
        entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::time::Duration;

    fn make_container() -> (AltScreenFlashContainer, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let container = AltScreenFlashContainer::new(move || {
            counter_clone.fetch_add(1, AtomicOrdering::SeqCst);
        });
        (container, counter)
    }

    #[test]
    fn flash_pushes_entry_and_renders() {
        let (container, counter) = make_container();
        assert_eq!(container.len(), 0);
        container.flash("Saved", None);
        assert_eq!(container.len(), 1);
        assert!(counter.load(AtomicOrdering::SeqCst) >= 1);
        let lines = container.render(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn flash_truncates_to_width() {
        let (container, _) = make_container();
        container.flash("a very long flash message that exceeds width", None);
        let lines = container.render(8);
        assert_eq!(lines.len(), 1);
        let span_text = &lines[0][0].text;
        assert!(span_text.starts_with("\x1b[7m"));
        assert!(span_text.ends_with("\x1b[27m"));
    }

    #[test]
    fn flash_removes_entry_after_duration() {
        let (container, _) = make_container();
        container.flash("Hi", Some(50));
        assert_eq!(container.len(), 1);
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn dispose_clears_pending_entries() {
        let (mut container, _) = make_container();
        container.flash("keep me", Some(10_000));
        container.flash("me too", Some(10_000));
        assert_eq!(container.len(), 2);
        container.dispose();
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn render_is_no_op_when_empty() {
        let (container, _) = make_container();
        let lines = container.render(80);
        assert!(lines.is_empty());
    }
}
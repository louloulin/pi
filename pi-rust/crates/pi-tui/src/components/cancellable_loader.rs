//! `CancellableLoader` — a loader that can be cancelled.
//!
//! Mirrors upstream `CancellableLoader`
//! (`packages/tui/src/components/cancellable-loader.ts`).

use crate::component::Component;
use crate::components::loader::Spinner;
use crate::utils::styled::{SpanStyle, StyledLine, StyledSpan};

/// Upstream `CancellableLoader` — a [`Loader`] with a `cancel()` method.
#[derive(Debug, Clone)]
pub struct CancellableLoader {
    loader: Spinner,
    cancelled: bool,
}

impl CancellableLoader {
    /// Build a cancellable loader wrapping the given loader.
    pub fn new(loader: Spinner) -> Self {
        Self { loader, cancelled: false }
    }

    /// Mark the loader cancelled; subsequent renders emit nothing.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Whether the loader has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Mutable access to the inner loader.
    pub fn loader_mut(&mut self) -> &mut Spinner {
        &mut self.loader
    }
}

impl Component for CancellableLoader {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        if self.cancelled {
            Vec::new()
        } else {
            let _ = width;
            let glyph = self.loader.frame().to_string();
            vec![vec![StyledSpan::new(glyph, SpanStyle::PLAIN)]]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellable_loader_renders_when_active() {
        let cl = CancellableLoader::new(Spinner::new());
        let lines = cl.render(40);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn cancellable_loader_clears_after_cancel() {
        let mut cl = CancellableLoader::new(Spinner::new());
        cl.cancel();
        assert!(cl.is_cancelled());
        let lines = cl.render(40);
        assert!(lines.is_empty());
    }
}
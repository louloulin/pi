//! `pi-tui` — Stage 0 stub. Stage 4 ports `packages/tui` (differential
//! renderer, editor component, autocomplete, keybindings) onto
//! `ratatui` + `crossterm`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Re-export of the underlying terminal backend so binaries can pin a
/// single version of `crossterm`.
pub use crossterm as backend;
/// Re-export of the underlying widget library.
pub use ratatui as widgets;

/// Marker type used until Stage 4 fills the crate in.
#[derive(Debug, Clone, Copy, Default)]
pub struct TuiVersion {
    /// Crate version (mirrors `Cargo.toml`).
    pub version: &'static str,
}

/// Build info that the binary prints at startup.
pub const VERSION: TuiVersion = TuiVersion {
    version: env!("CARGO_PKG_VERSION"),
};

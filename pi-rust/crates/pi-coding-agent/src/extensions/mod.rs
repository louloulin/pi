//! Extension loaders for `pi-coding-agent`.
//!
//! Stage 0 wired a JSON-descriptor path (kept for compatibility). Stage
//! 3 adds the [`js_loader`] path which drives the embedded QuickJS
//! host from [`pi_extensions`]. Both paths coexist; the file extension
//! on disk decides which one runs.

pub mod js_loader;
pub mod ui_bridge;
pub mod wiring;

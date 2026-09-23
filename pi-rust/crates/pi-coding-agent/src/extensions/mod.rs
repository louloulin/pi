//! Extension loaders for `pi-coding-agent`.
//!
//! Stage 0 wired a JSON-descriptor path (kept for compatibility). Stage
//! 3 adds the [`js_loader`] path which drives the embedded QuickJS
//! host from [`pi_extensions`]. Both paths coexist; the file extension
//! on disk decides which one runs.

pub mod autocomplete;
pub mod events;
pub mod hook;
pub mod js_loader;
pub mod lifecycle;
pub mod pi_ai_runner;
pub mod ui_bridge;
pub mod wiring;

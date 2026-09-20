//! Slash + CLI command handlers.
//!
//! - [`slash`] parses `/help`, `/clear`, `/model`, `/session`, `/resume`,
//!   etc. into a [`SlashCommand`] enum dispatched by
//!   `interactive.rs`.
//! - [`resume`] implements the `/resume` selector logic on top of the
//!   `pi-session` SQLite reader.
//! - [`session`] implements the `pi session list / show / export /
//!   migrate` subcommands.

pub mod resume;
pub mod session;
pub mod slash;

pub use slash::{handle_command, help_text, SlashCommand};

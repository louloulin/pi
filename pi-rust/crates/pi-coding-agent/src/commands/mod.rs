//! Slash + CLI command handlers.
//!
//! - [`slash`] parses `/help`, `/clear`, `/model`, `/session`, `/resume`,
//!   etc. into a [`SlashCommand`] enum dispatched by
//!   `interactive.rs`.
//! - [`resume`] implements the `/resume` selector logic on top of the
//!   `pi-session` SQLite reader.
//! - [`export`] implements the `/export` command and the `--export`
//!   CLI entry point over [`crate::export`].
//! - [`session`] implements the `pi session list / show / export /
//!   migrate` subcommands.

pub mod export;
pub mod resume;
pub mod session;
pub mod slash;

pub use export::{run_cli_export, run_slash_export, ActiveSession};
pub use slash::{handle_command, help_text, SlashCommand};

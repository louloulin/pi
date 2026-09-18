//! `pi-coding-agent` — interactive CLI binary.
//!
//! Stage 0 declares the CLI entry point and the public surface. Stage 4
//! wires the TUI + interactive mode; Stage 5 wires the session backend.
//!
//! Stage 1 (this commit) also lands the built-in tool bundle
//! ([`tools`]): `read`, `write`, `edit`, `bash`. These are the tools the
//! model sees by default; extension tools are layered on top of them via
//! [`pi_extensions`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod config;
pub mod tools;

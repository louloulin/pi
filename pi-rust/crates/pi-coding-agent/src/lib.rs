//! `pi-coding-agent` — interactive CLI binary.
//!
//! Stage 0 only declares the CLI entry point. Stage 4 wires the TUI +
//! interactive mode; Stage 5 wires the session backend.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod config;

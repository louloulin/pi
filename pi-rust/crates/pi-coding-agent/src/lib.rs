//! `pi-coding-agent` — interactive CLI binary.
//!
//! Stage 4 wires the TUI + interactive mode; Stage 5 wires the
//! session backend.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod interactive;
pub mod session_log;
pub mod text_fallback;

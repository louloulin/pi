//! CLI flag parser — mirrors the `pi` CLI surface from
//! `packages/coding-agent/src/cli.ts`.

use clap::{Parser, Subcommand};

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(name = "pi", about = "Rust port of the Pi coding agent")]
pub struct Cli {
    /// Subcommand to run. Defaults to interactive mode when omitted.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to a single extension file to load for this run only
    /// (matches `pi -e <path>` in the TS CLI).
    #[arg(short = 'e', long = "extension")]
    pub extension: Vec<String>,

    /// Print events to stdout as JSON instead of rendering the TUI.
    #[arg(long)]
    pub print: bool,

    /// RPC mode — stream JSON events on stdout and read JSON-RPC requests
    /// on stdin (matches `pi --rpc` in the TS CLI).
    #[arg(long)]
    pub rpc: bool,
}

/// Subcommands — mirrors `pi <subcommand>` in the TS CLI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the resolved version of pi + active providers.
    Version,
    /// Install a pi package (`npm:`, `git:`, local path, or HTTPS URL).
    Install {
        /// Package spec (see packages.md in the docs).
        spec: String,
    },
    /// Remove a pi package.
    Remove {
        /// Package spec.
        spec: String,
    },
    /// List installed pi packages.
    List,
    /// Refresh the model catalog without changing installed packages.
    UpdateModels,
}

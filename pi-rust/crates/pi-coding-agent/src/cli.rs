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

    /// Default model — accepts `<provider>/<model-id>` (e.g.
    /// `faux/faux-model`). Mirrors `pi --model` in the TS CLI.
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,

    /// Append text to the system prompt. Mirrors `pi --append-system-prompt`.
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Vec<String>,

    /// Path to a session directory. Defaults to `~/.pi/sessions/`.
    #[arg(long, value_name = "PATH")]
    pub session_dir: Option<std::path::PathBuf>,

    /// Resume a previous session by id or path.
    #[arg(long, value_name = "SESSION")]
    pub resume: Option<String>,
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
    /// Start an interactive session (the default when no subcommand
    /// is given).
    Interactive,
    /// Print mode — send a prompt, output result, exit (alias for
    /// `--print`).
    Print {
        /// Initial prompt.
        prompt: Vec<String>,
    },
    /// RPC mode — stream JSON events on stdout, accept JSON-RPC
    /// requests on stdin (alias for `--rpc`).
    Rpc,
    /// List built-in models — used by the `/model` slash command and
    /// the `--list-models` flag.
    ListModels,
}

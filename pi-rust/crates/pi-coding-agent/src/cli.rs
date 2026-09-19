//! CLI flag parser — mirrors the `pi` CLI surface from
//! `packages/coding-agent/src/cli.ts`.

use clap::{Parser, Subcommand};

use crate::print_mode::OutputFormat;

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
    ///
    /// Accepts an optional inline prompt: `pi --print "hello"` runs a
    /// single turn with the given prompt; `pi --print` (no argument)
    /// reads the prompt from stdin (when piped) or fails with
    /// `EX_USAGE` (64).
    #[arg(long, value_name = "PROMPT", num_args = 0..=1, default_missing_value = "")]
    pub print: Option<String>,

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

    /// Continue the most recent session (print mode only). Shorthand
    /// for `--session` with the most recently modified database.
    #[arg(long, value_name = "SESSION", conflicts_with_all = ["resume"])]
    pub r#continue_: Option<Option<String>>,

    /// Attach a specific session by id or path (print mode only).
    /// When omitted in `--print` mode the agent starts a fresh
    /// session.
    #[arg(long, value_name = "SESSION")]
    pub session: Option<String>,

    /// Output format for print mode. `text` streams the final reply on
    /// stdout (default); `json` emits a single JSON object at the end
    /// of the turn; `json-events` streams NDJSON event objects as they
    /// arrive (matches the RPC-mode wire shape).
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub output_format: OutputFormat,

    /// Hard cap on the number of agent turns the print mode will run.
    /// Exceeding the cap forces the turn to end and exits with code 1.
    /// `0` disables the cap.
    #[arg(long, value_name = "N", default_value_t = 0u32)]
    pub max_turns: u32,
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
    /// Session management subcommands (`list`, `show`, `export`,
    /// `migrate`). See [`SessionCommand`].
    Session {
        /// Session sub-action.
        #[command(subcommand)]
        action: SessionCommand,
    },
}

/// `pi session <action>` subcommands.
#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List every session in the session directory.
    List {
        /// Optional path to a specific SQLite database; defaults to
        /// every `*.sqlite` in the session directory.
        #[arg(long, value_name = "PATH")]
        database: Option<std::path::PathBuf>,
    },
    /// Show the entries of a single session as JSON Lines on stdout.
    Show {
        /// Session id to show (matches the `id` field of the header).
        session_id: String,
        /// Path to the SQLite database containing the session.
        #[arg(long, value_name = "PATH")]
        database: std::path::PathBuf,
    },
    /// Export a session to stdout as JSON Lines (alias for `show`,
    /// kept for parity with the TS CLI).
    Export {
        /// Session id to export.
        session_id: String,
        /// Path to the SQLite database containing the session.
        #[arg(long, value_name = "PATH")]
        database: std::path::PathBuf,
    },
    /// Migrate a Stage 4 JSONL session file into the SQLite backend.
    /// The original JSONL is preserved on disk; the migration is
    /// non-blocking on failure.
    Migrate {
        /// Path to the JSONL session file.
        jsonl_path: std::path::PathBuf,
        /// Optional destination path; defaults to `<jsonl-stem>.sqlite`
        /// alongside the source.
        #[arg(long, value_name = "PATH")]
        to: Option<std::path::PathBuf>,
    },
}
